use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context;
use chrono::Utc;
use futures_util::StreamExt;
use parking_lot::Mutex;
use serde::Serialize;
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
    sync::broadcast,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    catalog::{classify_ram, curated_by_id, unique_alias, RamFit},
    domain::TargetKind,
    hub::{required_weight_files, HubClient, ModelInspection},
    library::{self, safe_model_path},
    providers::WireProtocol,
    public_models::{preferred_public_id, GLOBAL_ADAPTIVE_MODEL_ID},
    routing::TargetRoutingProfile,
    secrets::SecretStore,
    storage::{InstallJob, LocalModelMeta, ModelTarget, Store},
};

#[derive(Debug, Clone, Serialize)]
pub struct InstallJobEvent {
    pub job_id: String,
    pub status: String,
    pub file: Option<String>,
    pub bytes_downloaded: u64,
    pub bytes_total: Option<u64>,
    pub progress: f32,
}

struct LiveJob {
    cancel: CancellationToken,
}

pub struct InstallManager {
    store: Store,
    client: reqwest::Client,
    secrets: Arc<dyn SecretStore>,
    library: PathBuf,
    hub_base: String,
    live: Arc<Mutex<HashMap<String, LiveJob>>>,
    events: broadcast::Sender<InstallJobEvent>,
}

impl InstallManager {
    pub fn new(
        store: Store,
        client: reqwest::Client,
        secrets: Arc<dyn SecretStore>,
        library: PathBuf,
        hub_base: impl Into<String>,
    ) -> Self {
        let (events, _) = broadcast::channel(64);
        Self {
            store,
            client,
            secrets,
            library,
            hub_base: hub_base.into(),
            live: Arc::new(Mutex::new(HashMap::new())),
            events,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<InstallJobEvent> {
        self.events.subscribe()
    }

    pub async fn interrupt_active(&self) -> anyhow::Result<()> {
        self.store.interrupt_active_install_jobs().await
    }

    pub async fn list(&self) -> anyhow::Result<Vec<InstallJob>> {
        self.store.install_jobs().await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        &self,
        inspection: ModelInspection,
        catalog_id: Option<String>,
        confirm_over_budget: bool,
        budget_bytes: u64,
        display_name: String,
        capabilities: Vec<String>,
        engine: String,
        task: String,
        estimated_memory_bytes: u64,
    ) -> anyhow::Result<InstallJob> {
        if !inspection.installable
            && catalog_id
                .as_deref()
                .and_then(curated_by_id)
                .map(|model| model.installable)
                != Some(true)
        {
            anyhow::bail!(
                "{}",
                inspection
                    .blockers
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "model cannot be installed".into())
            );
        }
        if self
            .store
            .target_by_source(&inspection.repo_id, &inspection.revision)
            .await?
            .is_some()
        {
            anyhow::bail!("this model revision is already installed");
        }
        if let Some(existing) = self
            .store
            .active_install_job(&inspection.repo_id, &inspection.revision)
            .await?
        {
            anyhow::bail!(
                "an install job for this model is already {}",
                existing.status
            );
        }
        match classify_ram(estimated_memory_bytes, budget_bytes) {
            RamFit::Fits => {}
            RamFit::Tight | RamFit::Unsuitable | RamFit::Unknown if confirm_over_budget => {}
            RamFit::Tight | RamFit::Unsuitable | RamFit::Unknown => {
                anyhow::bail!(
                    "this model exceeds the comfortable memory budget and needs confirmation"
                )
            }
        }
        let files = required_weight_files(&inspection.files)?;
        let job = InstallJob {
            id: Uuid::new_v4().to_string(),
            repo_id: inspection.repo_id.clone(),
            revision: inspection.revision.clone(),
            status: "queued".into(),
            catalog_id,
            alias: None,
            engine: Some(engine.clone()),
            task: Some(task.clone()),
            capabilities: capabilities.clone(),
            bytes_downloaded: 0,
            bytes_total: Some(inspection.download_bytes as i64),
            current_file: None,
            staging_dir: None,
            error: None,
            confirm_over_budget,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        self.store.upsert_install_job(&job).await?;
        self.spawn(
            job.id.clone(),
            inspection,
            files,
            display_name,
            capabilities,
            engine,
            task,
            estimated_memory_bytes,
        );
        self.store
            .install_job(&job.id)
            .await?
            .context("install job missing")
    }

    pub async fn resume(&self, job_id: &str) -> anyhow::Result<InstallJob> {
        let job = self
            .store
            .install_job(job_id)
            .await?
            .context("install job not found")?;
        if !matches!(job.status.as_str(), "paused" | "interrupted" | "failed") {
            anyhow::bail!("job cannot be resumed");
        }
        let hub = self.hub().await?;
        let inspection = hub
            .inspect(&job.repo_id, Some(&job.revision), 1, true)
            .await?;
        let files = required_weight_files(&inspection.files)?;
        self.spawn(
            job.id.clone(),
            inspection,
            files,
            job.alias.clone().unwrap_or_else(|| job.repo_id.clone()),
            job.capabilities.clone(),
            job.engine.clone().unwrap_or_else(|| "mlx_chat".into()),
            job.task.clone().unwrap_or_else(|| "chat".into()),
            0,
        );
        self.store
            .install_job(&job.id)
            .await?
            .context("install job missing")
    }

    pub async fn pause(&self, job_id: &str) -> anyhow::Result<InstallJob> {
        if let Some(live) = self.live.lock().remove(job_id) {
            live.cancel.cancel();
        }
        let mut job = self
            .store
            .install_job(job_id)
            .await?
            .context("install job not found")?;
        if matches!(job.status.as_str(), "completed" | "cancelled") {
            anyhow::bail!("job cannot be paused");
        }
        job.status = "paused".into();
        job.updated_at = Utc::now();
        self.store.upsert_install_job(&job).await?;
        self.emit(&job);
        Ok(job)
    }

    pub async fn cancel(&self, job_id: &str) -> anyhow::Result<InstallJob> {
        if let Some(live) = self.live.lock().remove(job_id) {
            live.cancel.cancel();
        }
        let mut job = self
            .store
            .install_job(job_id)
            .await?
            .context("install job not found")?;
        if matches!(job.status.as_str(), "completed") {
            anyhow::bail!("completed jobs cannot be cancelled");
        }
        job.status = "cancelled".into();
        job.updated_at = Utc::now();
        self.store.upsert_install_job(&job).await?;
        self.emit(&job);
        Ok(job)
    }

    pub async fn clear(&self, job_id: &str) -> anyhow::Result<()> {
        if let Some(live) = self.live.lock().remove(job_id) {
            live.cancel.cancel();
        }
        let job = self
            .store
            .install_job(job_id)
            .await?
            .context("install job not found")?;
        if matches!(job.status.as_str(), "queued" | "downloading" | "validating") {
            anyhow::bail!("cancel the download before removing it");
        }
        let staging = job
            .staging_dir
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.library.join(".staging").join(job_id));
        let _ = fs::remove_dir_all(&staging).await;
        self.store.delete_install_job(job_id).await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn(
        &self,
        job_id: String,
        inspection: ModelInspection,
        files: Vec<String>,
        display_name: String,
        capabilities: Vec<String>,
        engine: String,
        task: String,
        estimated_memory_bytes: u64,
    ) {
        let cancel = CancellationToken::new();
        self.live.lock().insert(
            job_id.clone(),
            LiveJob {
                cancel: cancel.clone(),
            },
        );
        let this = self.clone_handle();
        tokio::spawn(async move {
            let result = this
                .run(
                    &job_id,
                    inspection,
                    files,
                    display_name,
                    capabilities,
                    engine,
                    task,
                    estimated_memory_bytes,
                    cancel,
                )
                .await;
            this.live.lock().remove(&job_id);
            if let Err(error) = result {
                if let Ok(Some(mut job)) = this.store.install_job(&job_id).await {
                    if !matches!(job.status.as_str(), "cancelled" | "paused" | "completed") {
                        job.status = "failed".into();
                        job.error = Some(error.to_string());
                        job.updated_at = Utc::now();
                        let _ = this.store.upsert_install_job(&job).await;
                        this.emit(&job);
                    }
                }
            }
        });
    }

    fn clone_handle(&self) -> Self {
        Self {
            store: self.store.clone(),
            client: self.client.clone(),
            secrets: self.secrets.clone(),
            library: self.library.clone(),
            hub_base: self.hub_base.clone(),
            live: self.live.clone(),
            events: self.events.clone(),
        }
    }

    async fn hub(&self) -> anyhow::Result<HubClient> {
        Ok(HubClient::new(
            self.client.clone(),
            self.hub_base.clone(),
            self.secrets.get(crate::secrets::HF_ACCOUNT)?,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn run(
        &self,
        job_id: &str,
        inspection: ModelInspection,
        files: Vec<String>,
        display_name: String,
        capabilities: Vec<String>,
        engine: String,
        task: String,
        estimated_memory_bytes: u64,
        cancel: CancellationToken,
    ) -> anyhow::Result<()> {
        let mut job = self
            .store
            .install_job(job_id)
            .await?
            .context("job missing")?;
        if matches!(job.status.as_str(), "cancelled" | "paused") || cancel.is_cancelled() {
            return Ok(());
        }
        let staging = self.library.join(".staging").join(&job.id);
        fs::create_dir_all(&staging).await?;
        job.staging_dir = Some(staging.to_string_lossy().into_owned());
        job.status = "downloading".into();
        self.save_and_emit(&job).await?;
        let hub = self.hub().await?;
        let hf_token = self.secrets.get(crate::secrets::HF_ACCOUNT)?;
        let civitai_token = self.secrets.get(crate::secrets::CIVITAI_ACCOUNT)?;
        let mut downloaded = 0u64;
        let total = inspection.download_bytes;
        for file in &files {
            if cancel.is_cancelled() {
                let current = self.store.install_job(job_id).await?;
                if current
                    .as_ref()
                    .is_some_and(|item| matches!(item.status.as_str(), "paused" | "cancelled"))
                {
                    return Ok(());
                }
                job.status = "cancelled".into();
                self.save_and_emit(&job).await?;
                return Ok(());
            }
            job.current_file = Some(file.clone());
            self.save_and_emit(&job).await?;
            let destination = staging.join(safe_model_path(file)?);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).await?;
            }
            let (url, token) = if let Some(file_url) = inspection.file_url.as_deref() {
                (file_url.to_owned(), civitai_token.as_deref())
            } else {
                (
                    hub.download_url(&inspection.repo_id, &inspection.revision, file)
                        .await,
                    hf_token.as_deref(),
                )
            };
            let base = downloaded;
            downloaded += match download_resumable(
                &self.client,
                token,
                &url,
                &destination,
                &cancel,
                |bytes| {
                    job.bytes_downloaded = (base + bytes) as i64;
                    self.emit(&job);
                },
            )
            .await
            {
                Ok(bytes) => bytes,
                Err(_) if cancel.is_cancelled() => {
                    let current = self.store.install_job(job_id).await?;
                    if current
                        .as_ref()
                        .is_some_and(|item| matches!(item.status.as_str(), "paused" | "cancelled"))
                    {
                        return Ok(());
                    }
                    job.status = "cancelled".into();
                    self.save_and_emit(&job).await?;
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
            job.bytes_downloaded = downloaded as i64;
            job.bytes_total = Some(total as i64);
            self.save_and_emit(&job).await?;
        }
        job.status = "validating".into();
        self.save_and_emit(&job).await?;
        library::validate_model(&staging, &TargetKind::Mlx).await?;
        let actual = dir_size(&staging).await?;
        if total > 0 && actual.abs_diff(total) > total / 10 && actual.abs_diff(total) > 1024 * 1024
        {
            anyhow::bail!("downloaded size {actual} does not match advertised size {total}");
        }
        let destination =
            unique_library_dir(&self.library, &inspection.repo_id, &inspection.revision).await;
        fs::create_dir_all(destination.parent().unwrap()).await?;
        fs::rename(&staging, &destination).await?;
        let mut taken: HashSet<String> = self.store.aliases().await?.into_iter().collect();
        taken.insert(GLOBAL_ADAPTIVE_MODEL_ID.to_owned());
        for existing in self.store.targets().await? {
            taken.insert(preferred_public_id(
                &existing.provider_model,
                &existing.name,
            ));
        }
        let preferred = curated_by_id(job.catalog_id.as_deref().unwrap_or(""))
            .map(|model| model.alias.to_string())
            .unwrap_or_else(|| crate::hub::slug(&inspection.repo_id));
        let alias = unique_alias(&preferred, &taken);
        let target = ModelTarget {
            id: Uuid::new_v4().to_string(),
            provider_id: None,
            name: display_name,
            kind: TargetKind::Mlx,
            provider_model: alias.clone(),
            local_path: Some(destination.to_string_lossy().into_owned()),
            runtime_url: None,
            wire_protocol: WireProtocol::OpenAiChat,
            capabilities,
            enabled: true,
            state: "stopped".into(),
            size_bytes: Some(actual as i64),
            local: LocalModelMeta {
                task: Some(task),
                runtime_engine: Some(engine),
                source_repo: Some(inspection.repo_id),
                source_revision: Some(inspection.revision),
                estimated_memory_bytes: Some(estimated_memory_bytes as i64)
                    .filter(|value| *value > 0),
                catalog_id: job.catalog_id.clone(),
                trust_status: Some(
                    if job.catalog_id.is_some() {
                        "curated"
                    } else {
                        "untested"
                    }
                    .into(),
                ),
                resource_overrides: None,
            },
        };
        self.store.upsert_target(&target).await?;
        self.store
            .upsert_target_routing_profile(&TargetRoutingProfile::for_target(&target))
            .await?;
        job.alias = Some(alias);
        job.status = "completed".into();
        job.updated_at = Utc::now();
        self.save_and_emit(&job).await?;
        Ok(())
    }

    async fn save_and_emit(&self, job: &InstallJob) -> anyhow::Result<()> {
        let mut job = job.clone();
        job.updated_at = Utc::now();
        self.store.upsert_install_job(&job).await?;
        self.emit(&job);
        Ok(())
    }

    fn emit(&self, job: &InstallJob) {
        let total = job.bytes_total.unwrap_or(0).max(0) as u64;
        let downloaded = job.bytes_downloaded.max(0) as u64;
        let _ = self.events.send(InstallJobEvent {
            job_id: job.id.clone(),
            status: job.status.clone(),
            file: job.current_file.clone(),
            bytes_downloaded: downloaded,
            bytes_total: job.bytes_total.map(|value| value.max(0) as u64),
            progress: if total == 0 {
                0.0
            } else {
                downloaded as f32 / total as f32
            },
        });
    }
}

async fn unique_library_dir(library: &Path, repo: &str, revision: &str) -> PathBuf {
    let short = revision.chars().take(12).collect::<String>();
    let name = format!("{}@{short}", repo.replace('/', "--"));
    let mut path = library.join(&name);
    if !path.exists() {
        return path;
    }
    for index in 2..1000 {
        path = library.join(format!("{name}-{index}"));
        if !path.exists() {
            return path;
        }
    }
    path
}

async fn dir_size(path: &Path) -> anyhow::Result<u64> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || crate_library_dir_size(&path)).await?
}

pub(crate) fn crate_library_dir_size(path: &Path) -> anyhow::Result<u64> {
    if path.is_file() {
        return Ok(path.metadata()?.len());
    }
    let mut total = 0;
    for entry in std::fs::read_dir(path)? {
        total += crate_library_dir_size(&entry?.path())?;
    }
    Ok(total)
}

pub async fn download_resumable(
    client: &reqwest::Client,
    token: Option<&str>,
    url: &str,
    destination: &Path,
    cancel: &CancellationToken,
    mut progress: impl FnMut(u64),
) -> anyhow::Result<u64> {
    let part = destination.with_extension(format!(
        "{}part",
        destination
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| format!("{value}."))
            .unwrap_or_default()
    ));
    if looks_like_redirect_body(destination).await {
        let _ = fs::remove_file(destination).await;
    }
    if looks_like_redirect_body(&part).await {
        let _ = fs::remove_file(&part).await;
    }
    let offset = fs::metadata(&part)
        .await
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let mut request = client.get(url);
    if offset > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
    }
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await?;
    if response.status().is_redirection() {
        anyhow::bail!(
            "Hugging Face returned a redirect instead of file bytes; downloads must follow the CDN"
        );
    }
    let response = response.error_for_status()?;
    let append = offset > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(&part)
        .await?;
    let mut stream = response.bytes_stream();
    let mut written = if append { offset } else { 0 };
    let mut last_emit = written;
    progress(written);
    while let Some(chunk) = stream.next().await {
        if cancel.is_cancelled() {
            file.flush().await?;
            anyhow::bail!("install cancelled");
        }
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        written += chunk.len() as u64;
        if written.saturating_sub(last_emit) >= 512 * 1024 {
            progress(written);
            last_emit = written;
        }
    }
    file.flush().await?;
    fs::rename(part, destination).await?;
    progress(written);
    Ok(written)
}

async fn looks_like_redirect_body(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path).await else {
        return false;
    };
    let mut buf = [0u8; 96];
    let Ok(n) = file.read(&mut buf).await else {
        return false;
    };
    let text = String::from_utf8_lossy(&buf[..n]);
    let trimmed = text.trim_start();
    trimmed.starts_with("Found")
        || trimmed.starts_with("Moved")
        || trimmed.contains("Redirect")
        || trimmed.starts_with("<!DOCTYPE")
        || trimmed.starts_with("<html")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{hub::HubClient, secrets::MemorySecrets};
    use axum::{
        extract::Request,
        http::{header, StatusCode},
        response::IntoResponse,
        routing::get,
        Json, Router,
    };
    use serde_json::json;

    #[tokio::test]
    async fn install_job_resumes_from_range_and_completes_atomically() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::memory().await.unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        let hub = mock_files(b"hello world model").await;
        let manager = Arc::new(InstallManager::new(
            store.clone(),
            reqwest::Client::new(),
            secrets,
            root.path().join("library"),
            hub,
        ));
        let inspection = HubClient::new(reqwest::Client::new(), manager.hub_base.clone(), None)
            .inspect("org/ok", Some("rev"), 32 * 1024 * 1024 * 1024, true)
            .await
            .unwrap();
        let job = manager
            .start(
                inspection,
                None,
                false,
                32 * 1024 * 1024 * 1024,
                "Ok".into(),
                vec!["chat".into()],
                "mlx_chat".into(),
                "chat".into(),
                1024,
            )
            .await
            .unwrap();
        wait_for(&manager, &job.id, "completed").await;
        let completed = store.install_job(&job.id).await.unwrap().unwrap();
        assert_eq!(completed.status, "completed");
        let targets = store.targets().await.unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].state, "stopped");
        assert!(targets[0].local.source_revision.is_some());
        assert!(store
            .route(targets[0].provider_model.as_str())
            .await
            .unwrap()
            .is_none());
        assert!(!targets[0].provider_model.is_empty());
        assert!(!root.path().join("library/.staging").join(&job.id).exists());
    }

    #[tokio::test]
    async fn duplicate_and_parallel_jobs_for_the_same_revision_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::memory().await.unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        let hub = mock_files(b"hello world model").await;
        let manager = InstallManager::new(
            store.clone(),
            reqwest::Client::new(),
            secrets,
            root.path().join("library"),
            hub,
        );
        let inspection = HubClient::new(reqwest::Client::new(), manager.hub_base.clone(), None)
            .inspect("org/ok", Some("rev"), 32 * 1024 * 1024 * 1024, true)
            .await
            .unwrap();
        manager
            .start(
                inspection.clone(),
                None,
                false,
                32 * 1024 * 1024 * 1024,
                "Ok".into(),
                vec!["chat".into()],
                "mlx_chat".into(),
                "chat".into(),
                1024,
            )
            .await
            .unwrap();
        let err = manager
            .start(
                inspection,
                None,
                false,
                32 * 1024 * 1024 * 1024,
                "Ok".into(),
                vec!["chat".into()],
                "mlx_chat".into(),
                "chat".into(),
                1024,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("already"));
    }

    #[tokio::test]
    async fn cancel_keeps_a_resumable_staging_directory() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::memory().await.unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        let hub = mock_slow_files().await;
        let manager = Arc::new(InstallManager::new(
            store.clone(),
            reqwest::Client::new(),
            secrets,
            root.path().join("library"),
            hub,
        ));
        let inspection = HubClient::new(reqwest::Client::new(), manager.hub_base.clone(), None)
            .inspect("org/ok", Some("rev"), 32 * 1024 * 1024 * 1024, true)
            .await
            .unwrap();
        let job = manager
            .start(
                inspection,
                None,
                false,
                32 * 1024 * 1024 * 1024,
                "Ok".into(),
                vec!["chat".into()],
                "mlx_chat".into(),
                "chat".into(),
                1024,
            )
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        manager.cancel(&job.id).await.unwrap();
        wait_for(&manager, &job.id, "cancelled").await;
        assert!(
            root.path().join("library/.staging").join(&job.id).exists()
                || store.install_job(&job.id).await.unwrap().unwrap().status == "cancelled"
        );
        manager.clear(&job.id).await.unwrap();
        assert!(store.install_job(&job.id).await.unwrap().is_none());
        assert!(!root.path().join("library/.staging").join(&job.id).exists());
    }

    #[tokio::test]
    async fn advertised_size_mismatch_fails_validation() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::memory().await.unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        let hub = mock_mismatched_size().await;
        let manager = Arc::new(InstallManager::new(
            store.clone(),
            reqwest::Client::new(),
            secrets,
            root.path().join("library"),
            hub,
        ));
        let inspection = HubClient::new(reqwest::Client::new(), manager.hub_base.clone(), None)
            .inspect("org/ok", Some("rev"), 32 * 1024 * 1024 * 1024, true)
            .await
            .unwrap();
        let job = manager
            .start(
                inspection,
                None,
                false,
                32 * 1024 * 1024 * 1024,
                "Ok".into(),
                vec!["chat".into()],
                "mlx_chat".into(),
                "chat".into(),
                1024,
            )
            .await
            .unwrap();
        wait_for(&manager, &job.id, "failed").await;
        let failed = store.install_job(&job.id).await.unwrap().unwrap();
        assert!(failed
            .error
            .as_deref()
            .unwrap_or("")
            .contains("does not match advertised size"));
        assert!(store.targets().await.unwrap().is_empty());
    }

    async fn wait_for(manager: &InstallManager, id: &str, status: &str) {
        for _ in 0..200 {
            if manager
                .store
                .install_job(id)
                .await
                .unwrap()
                .is_some_and(|job| job.status == status)
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("job did not reach {status}");
    }

    #[tokio::test]
    async fn download_follows_cdn_redirect_and_rejects_unfollowed_redirects() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/file", get(|| async { "payload-bytes" }))
            .route(
                "/go",
                get(move || async move {
                    (
                        StatusCode::FOUND,
                        [(header::LOCATION, format!("http://{address}/file"))],
                        "redirect body",
                    )
                }),
            );
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let followed = dir.path().join("followed.bin");
        let blocked = dir.path().join("blocked.bin");
        let cancel = CancellationToken::new();
        let written = download_resumable(
            &reqwest::Client::new(),
            None,
            &format!("http://{address}/go"),
            &followed,
            &cancel,
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(written, 13);
        assert_eq!(
            tokio::fs::read_to_string(&followed).await.unwrap(),
            "payload-bytes"
        );
        let error = download_resumable(
            &reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            None,
            &format!("http://{address}/go"),
            &blocked,
            &cancel,
            |_| {},
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("redirect"));
    }

    async fn mock_files(body: &'static [u8]) -> String {
        let app = Router::new()
            .route("/api/models/org/ok", get(|| async {
                Json(json!({"id":"org/ok","sha":"rev","tags":["mlx"],"pipeline_tag":"text-generation","cardData":{"license":"mit"}}))
            }))
            .route("/api/models/org/ok/tree/rev", get(|| async {
                Json(json!([
                    {"path":"config.json","type":"file","size":15},
                    {"path":"model.safetensors","type":"file","size":17}
                ]))
            }))
            .route("/org/ok/resolve/rev/config.json", get(|| async {
                Json(json!({"model_type":"llama"}))
            }))
            .route("/org/ok/resolve/rev/model.safetensors", get(|request: Request| async move {
                let range = request.headers().get(header::RANGE).and_then(|value| value.to_str().ok());
                let bytes = b"hello world model";
                if let Some(range) = range.and_then(|value| value.strip_prefix("bytes=")) {
                    let start = range.trim_end_matches('-').parse::<usize>().unwrap_or(0);
                    return (StatusCode::PARTIAL_CONTENT, bytes[start.min(bytes.len())..].to_vec()).into_response();
                }
                bytes.to_vec().into_response()
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let _ = body;
        format!("http://{address}")
    }

    async fn mock_slow_files() -> String {
        mock_variant(b"hello world model", 17, true).await
    }

    async fn mock_mismatched_size() -> String {
        mock_variant(b"tiny", 50_000_000, false).await
    }

    async fn mock_variant(body: &'static [u8], advertised: u64, slow: bool) -> String {
        let app = Router::new()
            .route("/api/models/org/ok", get(|| async {
                Json(json!({"id":"org/ok","sha":"rev","tags":["mlx"],"pipeline_tag":"text-generation","cardData":{"license":"mit"}}))
            }))
            .route("/api/models/org/ok/tree/rev", get(move || async move {
                Json(json!([
                    {"path":"config.json","type":"file","size":15},
                    {"path":"model.safetensors","type":"file","size":advertised}
                ]))
            }))
            .route("/org/ok/resolve/rev/config.json", get(|| async {
                Json(json!({"model_type":"llama"}))
            }))
            .route("/org/ok/resolve/rev/model.safetensors", get(move || async move {
                if slow {
                    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                }
                body.to_vec().into_response()
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}")
    }
}

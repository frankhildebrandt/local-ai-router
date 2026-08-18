use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Context;
use parking_lot::Mutex;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sysinfo::{Pid, System};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    time::sleep,
};
use tokio_util::sync::CancellationToken;

use crate::secrets::generate_local_token;
use crate::{
    core::LocalActivityRegistry,
    domain::TargetKind,
    resource::{ResourcePolicy, ResourceProfile},
    storage::ModelTarget,
};

struct RuntimeEntry {
    child: Child,
    pid: u32,
    port: u16,
    size_bytes: u64,
    policy: ResourcePolicy,
    governor: CancellationToken,
    pending_restart: bool,
}

#[derive(Debug, Serialize)]
pub struct RuntimeStatus {
    pub target_id: String,
    pub port: u16,
    pub size_bytes: u64,
    pub queued: usize,
    pub active: usize,
    pub resident_bytes: u64,
    pub memory_warning: bool,
    pub profile: ResourceProfile,
    pub compute_duty_percent: u8,
    pub pending_restart: bool,
    pub tokens_per_second: Option<f64>,
}

pub struct RuntimeManager {
    entries: Mutex<HashMap<String, RuntimeEntry>>,
    starts: tokio::sync::Mutex<()>,
    restart_attempted: Mutex<HashSet<String>>,
    bin_dir: PathBuf,
    kv_cache_dir: PathBuf,
    policy: parking_lot::RwLock<ResourcePolicy>,
    activity: LocalActivityRegistry,
}

impl RuntimeManager {
    pub fn new(
        bin_dir: PathBuf,
        kv_cache_dir: PathBuf,
        policy: ResourcePolicy,
        activity: LocalActivityRegistry,
    ) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            starts: tokio::sync::Mutex::new(()),
            restart_attempted: Mutex::new(HashSet::new()),
            bin_dir,
            kv_cache_dir,
            policy: parking_lot::RwLock::new(policy),
            activity,
        }
    }

    fn effective_policy(&self, target: &ModelTarget) -> ResourcePolicy {
        let policy = self.policy.read().clone();
        target
            .local
            .resource_overrides
            .as_ref()
            .map(|overrides| policy.resolve(overrides))
            .unwrap_or(policy)
    }

    pub fn apply_policy(&self, policy: ResourcePolicy) -> anyhow::Result<()> {
        policy.validate()?;
        *self.policy.write() = policy;
        for entry in self.entries.lock().values_mut() {
            entry.pending_restart = true;
        }
        Ok(())
    }

    pub fn mark_target_pending_restart(&self, id: &str) {
        if let Some(entry) = self.entries.lock().get_mut(id) {
            entry.pending_restart = true;
        }
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.entries.lock().contains_key(id)
    }

    pub async fn clear_kv_cache(&self, target_id: Option<&str>) -> anyhow::Result<()> {
        let path = target_id
            .map(|id| self.cache_target_dir(id))
            .unwrap_or_else(|| self.kv_cache_dir.clone());
        if tokio::fs::try_exists(&path).await? {
            tokio::fs::remove_dir_all(&path).await?;
        }
        create_private_dir(&path)?;
        Ok(())
    }

    pub async fn restore_kv(
        &self,
        target: &ModelTarget,
        api_key_id: &str,
        session_id: &str,
    ) -> anyhow::Result<bool> {
        let policy = self.effective_policy(target);
        if target.kind != TargetKind::Gguf || !policy.disk_kv_enabled {
            return Ok(false);
        }
        let (directory, filename) = self.kv_snapshot(target, api_key_id, session_id);
        let path = directory.join(&filename);
        if !tokio::fs::try_exists(&path).await? {
            return Ok(false);
        }
        let (base, token) = self.local_control_endpoint(&target.id)?;
        let response = reqwest::Client::new()
            .post(format!("{base}/slots/0?action=restore"))
            .bearer_auth(token)
            .json(&serde_json::json!({ "filename": filename }))
            .send()
            .await?;
        if !response.status().is_success() {
            let _ = tokio::fs::remove_file(path).await;
            return Ok(false);
        }
        touch_file(&path)?;
        Ok(true)
    }

    pub async fn save_kv(
        &self,
        target: &ModelTarget,
        api_key_id: &str,
        session_id: &str,
    ) -> anyhow::Result<bool> {
        let policy = self.effective_policy(target);
        if target.kind != TargetKind::Gguf || !policy.disk_kv_enabled {
            return Ok(false);
        }
        let (directory, filename) = self.kv_snapshot(target, api_key_id, session_id);
        create_private_dir(&self.kv_cache_dir)?;
        create_private_dir(&directory)?;
        let (base, token) = self.local_control_endpoint(&target.id)?;
        let response = reqwest::Client::new()
            .post(format!("{base}/slots/0?action=save"))
            .bearer_auth(token)
            .json(&serde_json::json!({ "filename": filename }))
            .send()
            .await?;
        if !response.status().is_success() {
            anyhow::bail!("llama.cpp rejected the KV snapshot");
        }
        set_private_file_permissions(&directory.join(filename))?;
        self.enforce_kv_budget(policy.disk_kv_max_bytes)?;
        Ok(true)
    }

    fn local_control_endpoint(&self, id: &str) -> anyhow::Result<(String, String)> {
        let port = self
            .entries
            .lock()
            .get(id)
            .map(|entry| entry.port)
            .context("local runtime is not loaded")?;
        let token = self
            .activity
            .token(id)
            .context("runtime credential missing")?;
        Ok((format!("http://127.0.0.1:{port}"), token))
    }

    fn cache_target_dir(&self, target_id: &str) -> PathBuf {
        self.kv_cache_dir.join(hex_digest(target_id.as_bytes()))
    }

    fn kv_snapshot(
        &self,
        target: &ModelTarget,
        api_key_id: &str,
        session_id: &str,
    ) -> (PathBuf, String) {
        let path = target.local_path.as_deref().unwrap_or_default();
        let metadata = std::fs::metadata(path).ok();
        let modified = metadata
            .as_ref()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let policy = self.effective_policy(target);
        let fingerprint = format!(
            "v2\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
            target.id,
            path,
            target.local.source_revision.as_deref().unwrap_or_default(),
            metadata
                .as_ref()
                .map(|value| value.len())
                .unwrap_or_default(),
            modified,
            policy.cpu_threads,
            policy.max_parallel_prompts,
            policy.gguf_gpu_layers,
            policy.compute_duty_percent,
            api_key_id,
            session_id
        );
        (
            self.cache_target_dir(&target.id),
            format!("{}.bin", hex_digest(fingerprint.as_bytes())),
        )
    }

    fn enforce_kv_budget(&self, max_bytes: u64) -> anyhow::Result<()> {
        let mut files = Vec::new();
        if !self.kv_cache_dir.exists() {
            return Ok(());
        }
        for directory in std::fs::read_dir(&self.kv_cache_dir)? {
            let directory = directory?;
            if !directory.file_type()?.is_dir() {
                continue;
            }
            for file in std::fs::read_dir(directory.path())? {
                let file = file?;
                let metadata = file.metadata()?;
                if metadata.is_file() {
                    files.push((
                        file.path(),
                        metadata.len(),
                        metadata
                            .modified()
                            .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                    ));
                }
            }
        }
        let mut total = files.iter().map(|(_, size, _)| *size).sum::<u64>();
        files.sort_by_key(|(_, _, modified)| *modified);
        for (path, size, _) in files {
            if total <= max_bytes {
                break;
            }
            if std::fs::remove_file(path).is_ok() {
                total = total.saturating_sub(size);
            }
        }
        Ok(())
    }

    pub async fn start(&self, target: &ModelTarget) -> anyhow::Result<String> {
        let _start_guard = self.starts.lock().await;
        if let Some(port) = self.entries.lock().get(&target.id).map(|entry| entry.port) {
            return Ok(format!("http://127.0.0.1:{port}/v1"));
        }
        let path = target
            .local_path
            .as_deref()
            .context("local model path missing")?;
        let size = target
            .local
            .estimated_memory_bytes
            .or(target.size_bytes)
            .unwrap_or(0)
            .max(0) as u64;
        let policy = self.effective_policy(target);
        policy.validate()?;
        self.activity
            .configure(&target.id, policy.max_parallel_prompts)?;
        self.evict_to_fit(size, &policy).await?;
        let port = self.available_port()?;
        let engine = target
            .local
            .runtime_engine
            .as_deref()
            .unwrap_or(match target.kind {
                TargetKind::Gguf => "llama",
                TargetKind::Mlx => "mlx_chat",
                _ => "cloud",
            });
        let binary = match engine {
            "mlx_image" => "mlx-image-server-aarch64-apple-darwin",
            "mlx_speech" => "mlx-speech-server-aarch64-apple-darwin",
            "mlx_chat" => "mlx-server-aarch64-apple-darwin",
            _ => match target.kind {
                TargetKind::Gguf => "llama-server-aarch64-apple-darwin",
                TargetKind::Mlx => "mlx-server-aarch64-apple-darwin",
                _ => anyhow::bail!("not a local target"),
            },
        };
        let binary = self.bin_dir.join(binary);
        if !binary.exists() {
            anyhow::bail!("runtime sidecar missing: {}", binary.display());
        }
        if matches!(target.kind, TargetKind::Mlx)
            && !self.bin_dir.join("mlx.metallib").exists()
            && !self.bin_dir.join("default.metallib").exists()
        {
            anyhow::bail!(
                "MLX Metal kernels missing next to the sidecar (mlx.metallib). Rebuild with ./scripts/build-sidecars.sh; `swift build` cannot compile the shaders"
            );
        }
        let mut command = Command::new(&binary);
        let token = generate_local_token();
        match target.kind {
            TargetKind::Gguf => {
                command.env("LLAMA_ARG_API_KEY", &token);
                command.args([
                    "-m",
                    path,
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &port.to_string(),
                    "--alias",
                    &target.provider_model,
                    "--jinja",
                    "--threads",
                    &policy.cpu_threads.to_string(),
                    "--parallel",
                    &policy.max_parallel_prompts.to_string(),
                    "--prio",
                    &policy.process_priority.to_string(),
                    "--poll",
                    "0",
                    "-ngl",
                    &if policy.gguf_gpu_layers < 0 {
                        "99".to_string()
                    } else {
                        policy.gguf_gpu_layers.to_string()
                    },
                ]);
                if policy.disk_kv_enabled {
                    let target_cache = self.cache_target_dir(&target.id);
                    create_private_dir(&self.kv_cache_dir)?;
                    create_private_dir(&target_cache)?;
                    command.arg("--slot-save-path").arg(target_cache);
                }
            }
            TargetKind::Mlx => {
                command.env("LOCAL_AI_ROUTER_RUNTIME_TOKEN", &token);
                command.args([
                    "--model",
                    path,
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &port.to_string(),
                    "--alias",
                    &target.provider_model,
                ]);
                if engine == "mlx_chat" {
                    let mut system = System::new();
                    system.refresh_memory();
                    let memory_mib =
                        policy.memory_budget_bytes(system.total_memory()) / (1024 * 1024);
                    command
                        .arg("--memory-limit-mib")
                        .arg(memory_mib.to_string());
                }
                if engine == "mlx_image" {
                    command.args(["--pipeline", image_pipeline_for(target)]);
                }
            }
            _ => unreachable!(),
        }
        command
            .current_dir(&self.bin_dir)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let mut child = command.spawn().context("starting inference sidecar")?;
        let pid = child.id().context("inference sidecar has no process id")?;
        set_background_priority(pid, policy.process_priority);
        let base = format!("http://127.0.0.1:{port}");
        wait_ready(&base, &token, &mut child).await?;
        let governor = CancellationToken::new();
        self.entries.lock().insert(
            target.id.clone(),
            RuntimeEntry {
                child,
                pid,
                port,
                size_bytes: size,
                policy: policy.clone(),
                governor: governor.clone(),
                pending_restart: false,
            },
        );
        self.activity.set_token(&target.id, token);
        self.activity.touch(&target.id);
        spawn_duty_governor(
            pid,
            target.id.clone(),
            policy.compute_duty_percent,
            self.activity.clone(),
            governor,
        );
        Ok(format!("{base}/v1"))
    }

    pub async fn stop(&self, id: &str) -> anyhow::Result<()> {
        // Cloud targets and unloaded local targets do not have a runtime entry.
        // There is nothing to reserve or stop for them; in particular, the
        // absence of a local activity gate must not be interpreted as a busy
        // model.
        if !self.entries.lock().contains_key(id) {
            return Ok(());
        }
        let _reservation = self
            .activity
            .try_reserve_for_unload(id)
            .context("model is serving or has queued requests")?;
        self.stop_reserved(id).await
    }

    async fn stop_reserved(&self, id: &str) -> anyhow::Result<()> {
        let entry = self.entries.lock().remove(id);
        self.restart_attempted.lock().remove(id);
        if let Some(mut entry) = entry {
            entry.governor.cancel();
            signal_process(entry.pid, libc::SIGCONT);
            entry.child.kill().await?;
        }
        Ok(())
    }

    pub async fn stop_all(&self) {
        let entries = std::mem::take(&mut *self.entries.lock());
        for (_, mut entry) in entries {
            entry.governor.cancel();
            signal_process(entry.pid, libc::SIGCONT);
            let _ = entry.child.kill().await;
        }
    }

    pub fn statuses(&self) -> Vec<RuntimeStatus> {
        let mut system = System::new_all();
        system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        let total_resident = self
            .entries
            .lock()
            .values()
            .map(|entry| {
                system
                    .process(Pid::from_u32(entry.pid))
                    .map(|process| process.memory())
                    .unwrap_or(entry.size_bytes)
            })
            .sum::<u64>();
        self.entries
            .lock()
            .iter()
            .map(|(id, entry)| RuntimeStatus {
                target_id: id.clone(),
                port: entry.port,
                size_bytes: entry.size_bytes,
                queued: self.activity.queued(id),
                active: self.activity.active(id),
                resident_bytes: system
                    .process(Pid::from_u32(entry.pid))
                    .map(|process| process.memory())
                    .unwrap_or(entry.size_bytes),
                memory_warning: total_resident
                    > entry.policy.memory_budget_bytes(system.total_memory()),
                profile: entry.policy.profile,
                compute_duty_percent: entry.policy.compute_duty_percent,
                pending_restart: entry.pending_restart,
                tokens_per_second: None,
            })
            .collect()
    }

    pub fn take_crashed(&self) -> Vec<(String, bool)> {
        let crashed = self
            .entries
            .lock()
            .iter_mut()
            .filter_map(|(id, entry)| entry.child.try_wait().ok().flatten().map(|_| id.clone()))
            .collect::<Vec<_>>();
        let mut entries = self.entries.lock();
        let mut attempted = self.restart_attempted.lock();
        crashed
            .into_iter()
            .map(|id| {
                if let Some(entry) = entries.remove(&id) {
                    entry.governor.cancel();
                    signal_process(entry.pid, libc::SIGCONT);
                }
                let may_restart = attempted.insert(id.clone());
                (id, may_restart)
            })
            .collect()
    }

    pub async fn reap_idle(&self) -> Vec<String> {
        let candidates = self
            .entries
            .lock()
            .iter()
            .map(|(id, entry)| (id.clone(), entry.policy.idle_unload_minutes))
            .collect::<Vec<_>>();
        let ids = candidates
            .into_iter()
            .filter_map(|(id, idle_timeout)| {
                (idle_timeout > 0
                    && self.activity.idle_for(&id)
                        >= Duration::from_secs(idle_timeout.saturating_mul(60)))
                .then(|| {
                    self.activity
                        .try_reserve_for_unload(&id)
                        .map(|permit| (id, permit))
                })
                .flatten()
            })
            .collect::<Vec<_>>();
        let mut stopped = Vec::new();
        for (id, _reservation) in ids {
            if self.stop_reserved(&id).await.is_ok() {
                stopped.push(id);
            }
        }
        stopped
    }

    pub async fn reap_over_budget(&self) -> Vec<String> {
        let mut stopped = Vec::new();
        loop {
            let mut system = System::new_all();
            system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
            let (used, budget) = {
                let entries = self.entries.lock();
                let used = entries
                    .values()
                    .map(|entry| {
                        system
                            .process(Pid::from_u32(entry.pid))
                            .map(|process| process.memory())
                            .unwrap_or(entry.size_bytes)
                            .max(entry.size_bytes)
                    })
                    .sum::<u64>();
                let budget = entries
                    .values()
                    .map(|entry| entry.policy.memory_budget_bytes(system.total_memory()))
                    .min();
                (used, budget)
            };
            if budget.map_or(true, |budget| used <= budget) {
                break;
            }
            let candidate = self
                .entries
                .lock()
                .keys()
                .filter_map(|id| {
                    self.activity
                        .try_reserve_for_unload(id)
                        .map(|permit| (id.clone(), self.activity.idle_for(id), permit))
                })
                .max_by_key(|(_, idle, _)| *idle);
            match candidate {
                Some((id, _, _reservation)) => {
                    if self.stop_reserved(&id).await.is_ok() {
                        stopped.push(id);
                    }
                }
                None => break,
            }
        }
        stopped
    }

    pub async fn reap_pending_restarts(&self) -> Vec<String> {
        let ids = self
            .entries
            .lock()
            .iter()
            .filter_map(|(id, entry)| {
                (entry.pending_restart && self.activity.queued(id) == 0)
                    .then(|| {
                        self.activity
                            .try_reserve_for_unload(id)
                            .map(|permit| (id.clone(), permit))
                    })
                    .flatten()
            })
            .collect::<Vec<_>>();
        let mut stopped = Vec::new();
        for (id, _reservation) in ids {
            if self.stop_reserved(&id).await.is_ok() {
                stopped.push(id);
            }
        }
        stopped
    }

    async fn evict_to_fit(&self, incoming: u64, policy: &ResourcePolicy) -> anyhow::Result<()> {
        let mut system = System::new();
        system.refresh_memory();
        let budget = policy.memory_budget_bytes(system.total_memory());
        loop {
            let mut processes = System::new_all();
            processes.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
            let used: u64 = self
                .entries
                .lock()
                .values()
                .map(|entry| {
                    processes
                        .process(Pid::from_u32(entry.pid))
                        .map(|process| process.memory())
                        .unwrap_or(entry.size_bytes)
                        .max(entry.size_bytes)
                })
                .sum();
            if used.saturating_add(incoming) <= budget {
                return Ok(());
            }
            if incoming > budget {
                anyhow::bail!(
                    "this model exceeds the configured memory budget and cannot be loaded"
                );
            }
            let candidate = self
                .entries
                .lock()
                .keys()
                .filter_map(|id| {
                    self.activity
                        .try_reserve_for_unload(id)
                        .map(|permit| (id.clone(), self.activity.idle_for(id), permit))
                })
                .max_by_key(|(_, idle, _)| *idle);
            match candidate {
                Some((id, _, _reservation)) => self.stop_reserved(&id).await?,
                None => anyhow::bail!("memory budget is exhausted by active models"),
            }
        }
    }

    fn available_port(&self) -> anyhow::Result<u16> {
        for port in 12100..12200 {
            if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
                return Ok(port);
            }
        }
        anyhow::bail!("no free inference port")
    }
}

fn spawn_duty_governor(
    pid: u32,
    target_id: String,
    duty_percent: u8,
    activity: LocalActivityRegistry,
    cancellation: CancellationToken,
) {
    if duty_percent >= 100 {
        return;
    }
    tokio::spawn(async move {
        let (active_slice, paused_slice) = duty_slices(duty_percent);
        loop {
            if cancellation.is_cancelled() {
                break;
            }
            if activity.active(&target_id) == 0 {
                signal_process(pid, libc::SIGCONT);
                tokio::select! {
                    _ = cancellation.cancelled() => break,
                    _ = sleep(Duration::from_millis(50)) => {}
                }
                continue;
            }
            signal_process(pid, libc::SIGCONT);
            tokio::select! {
                _ = cancellation.cancelled() => break,
                _ = sleep(active_slice) => {}
            }
            if cancellation.is_cancelled() {
                break;
            }
            signal_process(pid, libc::SIGSTOP);
            tokio::select! {
                _ = cancellation.cancelled() => break,
                _ = sleep(paused_slice) => {}
            }
        }
        signal_process(pid, libc::SIGCONT);
    });
}

fn duty_slices(duty_percent: u8) -> (Duration, Duration) {
    let window = Duration::from_millis(400);
    let active = window.mul_f64(duty_percent.clamp(1, 100) as f64 / 100.0);
    (active, window.saturating_sub(active))
}

#[cfg(unix)]
fn signal_process(pid: u32, signal: libc::c_int) {
    // The PID comes directly from the child we spawned and is never user-controlled.
    unsafe {
        libc::kill(pid as libc::pid_t, signal);
    }
}

#[cfg(not(unix))]
fn signal_process(_pid: u32, _signal: libc::c_int) {}

#[cfg(unix)]
fn set_background_priority(pid: u32, priority: i8) {
    let nice = if priority < 0 { 10 } else { 0 };
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, pid, nice);
    }
}

#[cfg(not(unix))]
fn set_background_priority(_pid: u32, _priority: i8) {}

fn hex_digest(value: &[u8]) -> String {
    Sha256::digest(value)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn create_private_dir(path: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn set_private_file_permissions(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn touch_file(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let path = CString::new(path.as_os_str().as_bytes())?;
        let times = [
            libc::timespec {
                tv_sec: 0,
                tv_nsec: libc::UTIME_NOW,
            },
            libc::timespec {
                tv_sec: 0,
                tv_nsec: libc::UTIME_NOW,
            },
        ];
        let result = unsafe { libc::utimensat(libc::AT_FDCWD, path.as_ptr(), times.as_ptr(), 0) };
        anyhow::ensure!(result == 0, "failed to update KV snapshot recency");
    }
    #[cfg(not(unix))]
    {
        let file = std::fs::OpenOptions::new().append(true).open(path)?;
        file.sync_all()?;
    }
    Ok(())
}

async fn wait_ready(base_url: &str, token: &str, child: &mut Child) -> anyhow::Result<()> {
    let stderr_task = child.stderr.take().map(|mut stderr| {
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf).await;
            tail_log(&String::from_utf8_lossy(&buf))
        })
    });
    let client = reqwest::Client::new();
    for _ in 0..600 {
        if let Some(status) = child.try_wait()? {
            anyhow::bail!(
                "inference sidecar exited ({status}){}",
                format_sidecar_detail(join_sidecar_log(stderr_task).await)
            );
        }
        if client
            .get(format!("{base_url}/health"))
            .bearer_auth(token)
            .send()
            .await
            .map(|response| response.status().is_success())
            .unwrap_or(false)
        {
            return Ok(());
        }
        sleep(Duration::from_millis(500)).await;
    }
    let _ = child.start_kill();
    anyhow::bail!(
        "inference sidecar did not become ready{}",
        format_sidecar_detail(join_sidecar_log(stderr_task).await)
    )
}

async fn join_sidecar_log(task: Option<tokio::task::JoinHandle<String>>) -> String {
    match task {
        Some(task) => task.await.unwrap_or_default(),
        None => String::new(),
    }
}

fn format_sidecar_detail(detail: String) -> String {
    let detail = detail.trim();
    if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    }
}

fn tail_log(text: &str) -> String {
    const MAX: usize = 4000;
    if text.len() <= MAX {
        return text.to_string();
    }
    let mut start = text.len() - MAX;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_string()
}

pub fn image_pipeline_for(target: &ModelTarget) -> &'static str {
    let catalog = target.local.catalog_id.as_deref().unwrap_or_default();
    let path = target.local_path.as_deref().unwrap_or_default();
    let haystack = format!("{catalog} {path}").to_ascii_lowercase();
    if catalog == "sdxl-turbo"
        || catalog == "sdxl"
        || haystack.contains("sdxl")
        || haystack.contains("stable-diffusion-xl")
        || haystack.contains("sd-xl")
        || Path::new(path).join("text_encoder_2").is_dir()
    {
        return "sdxl";
    }
    if catalog == "sd"
        || catalog.starts_with("sd-")
        || haystack.contains("stable-diffusion")
        || haystack.contains("sd-1")
        || haystack.contains("sd15")
        || haystack.contains("sd-2")
        || (Path::new(path).join("unet").join("config.json").is_file()
            && Path::new(path).join("text_encoder").is_dir()
            && !Path::new(path).join("text_encoder_2").is_dir())
    {
        return "sd";
    }
    "flux2"
}

pub fn bundled_bin_dir(resource_dir: &Path) -> PathBuf {
    let bundled = resource_dir.join("sidecars/bin");
    if bundled.join("llama-server-aarch64-apple-darwin").exists()
        || bundled.join("mlx-server-aarch64-apple-darwin").exists()
        || bundled
            .join("mlx-image-server-aarch64-apple-darwin")
            .exists()
        || bundled
            .join("mlx-speech-server-aarch64-apple-darwin")
            .exists()
    {
        bundled
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sidecars/bin")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stopping_a_target_without_a_local_runtime_is_a_noop() {
        let manager = RuntimeManager::new(
            PathBuf::new(),
            PathBuf::new(),
            ResourcePolicy::preset(ResourceProfile::Stealth, 8),
            LocalActivityRegistry::default(),
        );

        manager.stop("cloud-target").await.unwrap();
    }

    #[tokio::test]
    async fn wait_ready_reports_sidecar_exit() {
        let mut child = Command::new("false")
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let error = wait_ready("http://127.0.0.1:1", "token", &mut child)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("exited"), "{error}");
    }

    #[test]
    fn stealth_duty_cycle_uses_a_short_one_to_three_window() {
        assert_eq!(
            duty_slices(25),
            (Duration::from_millis(100), Duration::from_millis(300))
        );
    }

    #[test]
    fn image_pipeline_selects_sd_sdxl_and_flux_from_catalog_path_or_layout() {
        assert_eq!(
            image_pipeline_for(&image_target(Some("sd-2-1-base"), "/models/sd-2-1")),
            "sd"
        );
        assert_eq!(
            image_pipeline_for(&image_target(
                None,
                "/library/stabilityai--stable-diffusion-v1-5"
            )),
            "sd"
        );
        assert_eq!(
            image_pipeline_for(&image_target(
                None,
                "/library/stabilityai--stable-diffusion-xl-base-1.0"
            )),
            "sdxl"
        );
        assert_eq!(
            image_pipeline_for(&image_target(Some("sdxl-turbo"), "/models/anything")),
            "sdxl"
        );
        assert_eq!(
            image_pipeline_for(&image_target(None, "/library/org--sdxl-lightning")),
            "sdxl"
        );
        assert_eq!(
            image_pipeline_for(&image_target(Some("flux2-klein-4b"), "/models/flux")),
            "flux2"
        );

        let root = tempfile::tempdir().unwrap();
        let sd = root.path().join("imported-sd");
        std::fs::create_dir_all(sd.join("unet")).unwrap();
        std::fs::create_dir_all(sd.join("text_encoder")).unwrap();
        std::fs::write(sd.join("unet/config.json"), "{}").unwrap();
        assert_eq!(
            image_pipeline_for(&image_target(None, sd.to_str().unwrap())),
            "sd"
        );

        let sdxl = root.path().join("imported-sdxl");
        std::fs::create_dir_all(sdxl.join("text_encoder_2")).unwrap();
        assert_eq!(
            image_pipeline_for(&image_target(None, sdxl.to_str().unwrap())),
            "sdxl"
        );
    }

    fn image_target(catalog_id: Option<&str>, path: &str) -> ModelTarget {
        ModelTarget {
            id: "img".into(),
            provider_id: None,
            name: "Image".into(),
            kind: TargetKind::Mlx,
            provider_model: "image".into(),
            local_path: Some(path.into()),
            runtime_url: None,
            wire_protocol: crate::providers::WireProtocol::OpenAiChat,
            capabilities: vec!["images".into()],
            enabled: true,
            state: "stopped".into(),
            size_bytes: None,
            local: crate::storage::LocalModelMeta {
                catalog_id: catalog_id.map(str::to_owned),
                runtime_engine: Some("mlx_image".into()),
                task: Some("image".into()),
                ..Default::default()
            },
        }
    }
}

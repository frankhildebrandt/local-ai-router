use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::Context;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{
    oauth::OAuthManager,
    providers::{provider_preset, validate_cloud_base_url, AuthMode, AuthScheme},
    secrets::{
        generate_local_token, local_api_key_account, provider_account, SecretStore, LOCAL_API_KEY,
    },
    storage::{LocalApiKey, ModelTarget, Provider, Store},
};

#[derive(Clone)]
pub struct AppCore {
    pub store: Store,
    pub secrets: Arc<dyn SecretStore>,
    pub client: Client,
    pub oauth: OAuthManager,
    local_gates: Arc<parking_lot::Mutex<HashMap<String, Arc<InferenceGate>>>>,
}

struct InferenceGate {
    active: Arc<Semaphore>,
    limit: usize,
    active_count: AtomicUsize,
    queued: AtomicUsize,
    last_used: parking_lot::Mutex<std::time::Instant>,
    token: parking_lot::Mutex<Option<String>>,
}

pub struct LocalInferencePermit {
    _permit: OwnedSemaphorePermit,
    gate: Arc<InferenceGate>,
}

impl Drop for LocalInferencePermit {
    fn drop(&mut self) {
        self.gate.active_count.fetch_sub(1, Ordering::AcqRel);
        *self.gate.last_used.lock() = std::time::Instant::now();
    }
}

struct WaitingGuard(Arc<InferenceGate>);

impl Drop for WaitingGuard {
    fn drop(&mut self) {
        self.0.queued.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Default)]
pub struct LocalActivityRegistry {
    gates: Arc<parking_lot::Mutex<HashMap<String, Arc<InferenceGate>>>>,
}

impl LocalActivityRegistry {
    fn gate(&self, id: &str, limit: usize) -> Arc<InferenceGate> {
        let mut gates = self.gates.lock();
        let previous = gates.get(id).cloned();
        if let Some(gate) = previous.as_ref() {
            return gate.clone();
        }
        let gate = Arc::new(InferenceGate {
            active: Arc::new(Semaphore::new(limit)),
            limit,
            active_count: AtomicUsize::new(0),
            queued: AtomicUsize::new(0),
            last_used: parking_lot::Mutex::new(std::time::Instant::now()),
            token: parking_lot::Mutex::new(previous.and_then(|gate| gate.token.lock().clone())),
        });
        gates.insert(id.to_owned(), gate.clone());
        gate
    }

    pub fn configure(&self, id: &str, limit: usize) -> anyhow::Result<()> {
        let mut gates = self.gates.lock();
        let previous = gates.get(id).cloned();
        if previous.as_ref().is_some_and(|gate| {
            gate.active_count.load(Ordering::Acquire) > 0 || gate.queued.load(Ordering::Acquire) > 0
        }) {
            anyhow::bail!("model still has active or queued requests");
        }
        let gate = Arc::new(InferenceGate {
            active: Arc::new(Semaphore::new(limit)),
            limit,
            active_count: AtomicUsize::new(0),
            queued: AtomicUsize::new(0),
            last_used: parking_lot::Mutex::new(std::time::Instant::now()),
            token: parking_lot::Mutex::new(previous.and_then(|gate| gate.token.lock().clone())),
        });
        gates.insert(id.to_owned(), gate);
        Ok(())
    }

    pub fn touch(&self, id: &str) {
        let gate = self.gates.lock().get(id).cloned();
        if let Some(gate) = gate {
            *gate.last_used.lock() = std::time::Instant::now();
        }
    }

    pub fn set_token(&self, id: &str, token: String) {
        let gate = self.gate(id, 1);
        *gate.token.lock() = Some(token);
    }

    pub fn token(&self, id: &str) -> Option<String> {
        self.gates
            .lock()
            .get(id)
            .and_then(|gate| gate.token.lock().clone())
    }

    pub fn try_reserve_for_unload(&self, id: &str) -> Option<OwnedSemaphorePermit> {
        let gate = self.gates.lock().get(id)?.clone();
        if gate.queued.load(Ordering::Acquire) > 0 {
            return None;
        }
        gate.active
            .clone()
            .try_acquire_many_owned(gate.limit as u32)
            .ok()
    }

    pub fn idle_for(&self, id: &str) -> Duration {
        self.gates
            .lock()
            .get(id)
            .map(|gate| gate.last_used.lock().elapsed())
            .unwrap_or_default()
    }

    pub fn queued(&self, id: &str) -> usize {
        self.gates
            .lock()
            .get(id)
            .map(|gate| gate.queued.load(Ordering::Acquire))
            .unwrap_or(0)
    }

    pub fn active(&self, id: &str) -> usize {
        self.gates
            .lock()
            .get(id)
            .map(|gate| gate.active_count.load(Ordering::Acquire))
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerStatus {
    pub running: bool,
    pub base_url: String,
    pub port: u16,
}

impl AppCore {
    pub async fn open(path: &Path, secrets: Arc<dyn SecretStore>) -> anyhow::Result<Self> {
        let store = Store::open(path).await?;
        let core = Self::new(store, secrets)?;
        core.migrate_legacy_local_api_key().await?;
        if core.store.local_api_keys().await?.is_empty() {
            core.create_local_api_key("Default").await?;
        }
        core.cleanup_revoked_local_api_key_secrets().await;
        Ok(core)
    }

    pub fn new(store: Store, secrets: Arc<dyn SecretStore>) -> anyhow::Result<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .user_agent("LocalAI-Router/0.1")
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let oauth = OAuthManager::new(client.clone(), secrets.clone());
        Ok(Self {
            store,
            secrets,
            client,
            oauth,
            local_gates: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        })
    }

    pub fn local_activity(&self) -> LocalActivityRegistry {
        LocalActivityRegistry {
            gates: self.local_gates.clone(),
        }
    }

    pub async fn migrate_legacy_local_api_key(&self) -> anyhow::Result<()> {
        if self.store.local_api_key("default").await?.is_some() {
            return Ok(());
        }
        let Some(token) = self.secrets.get(LOCAL_API_KEY)? else {
            return Ok(());
        };
        let key = LocalApiKey {
            id: "default".into(),
            name: "Default".into(),
            created_at: chrono::Utc::now(),
            last_used_at: None,
            revoked_at: None,
        };
        let account = local_api_key_account(&key.id);
        self.secrets.set(&account, &token)?;
        if let Err(error) = self
            .store
            .insert_local_api_key(&key, &token_hash(&token))
            .await
        {
            let _ = self.secrets.delete(&account);
            return Err(error);
        }
        self.secrets.delete(LOCAL_API_KEY)?;
        Ok(())
    }

    pub async fn create_local_api_key(&self, name: &str) -> anyhow::Result<(LocalApiKey, String)> {
        let name = validated_key_name(name)?;
        let key = LocalApiKey {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            created_at: chrono::Utc::now(),
            last_used_at: None,
            revoked_at: None,
        };
        let token = generate_local_token();
        let account = local_api_key_account(&key.id);
        self.secrets.set(&account, &token)?;
        if let Err(error) = self
            .store
            .insert_local_api_key(&key, &token_hash(&token))
            .await
        {
            let _ = self.secrets.delete(&account);
            return Err(error);
        }
        Ok((key, token))
    }

    pub fn reveal_local_api_key(&self, id: &str) -> anyhow::Result<String> {
        self.secrets
            .get(&local_api_key_account(id))?
            .context("local API key is revoked or missing")
    }

    pub async fn rename_local_api_key(&self, id: &str, name: &str) -> anyhow::Result<LocalApiKey> {
        let name = validated_key_name(name)?;
        if !self.store.rename_local_api_key(id, &name).await? {
            anyhow::bail!("local API key not found");
        }
        self.store
            .local_api_key(id)
            .await?
            .context("local API key not found")
    }

    pub async fn rotate_local_api_key(&self, id: &str) -> anyhow::Result<String> {
        let key = self
            .store
            .local_api_key(id)
            .await?
            .context("local API key not found")?;
        if key.revoked_at.is_some() {
            anyhow::bail!("revoked local API keys cannot be rotated");
        }
        let account = local_api_key_account(id);
        let previous = self.secrets.get(&account)?;
        let token = generate_local_token();
        self.secrets.set(&account, &token)?;
        if let Err(error) = self
            .store
            .rotate_local_api_key(id, &token_hash(&token))
            .await
        {
            if let Some(previous) = previous {
                let _ = self.secrets.set(&account, &previous);
            }
            return Err(error);
        }
        Ok(token)
    }

    pub async fn revoke_local_api_key(&self, id: &str) -> anyhow::Result<()> {
        let key = self
            .store
            .local_api_key(id)
            .await?
            .context("local API key not found")?;
        if key.revoked_at.is_none() && !self.store.revoke_local_api_key(id).await? {
            anyhow::bail!("active local API key not found");
        }
        let account = local_api_key_account(id);
        self.secrets.delete(&account)
    }

    async fn cleanup_revoked_local_api_key_secrets(&self) {
        if let Ok(keys) = self.store.local_api_keys().await {
            for key in keys.into_iter().filter(|key| key.revoked_at.is_some()) {
                let _ = self.secrets.delete(&local_api_key_account(&key.id));
            }
        }
    }

    pub async fn authorized(&self, authorization: Option<&str>) -> Option<String> {
        let candidate = authorization.and_then(|value| value.strip_prefix("Bearer "))?;
        self.authorized_token(Some(candidate)).await
    }

    pub async fn authorized_token(&self, candidate: Option<&str>) -> Option<String> {
        let candidate = candidate?;
        let Ok(keys) = self.store.active_local_api_key_hashes().await else {
            return None;
        };
        let candidate_hash = token_hash(candidate);
        let mut matched = None;
        for (id, expected_hash) in keys {
            if candidate_hash
                .as_slice()
                .ct_eq(expected_hash.as_slice())
                .into()
            {
                matched = Some(id);
            }
        }
        if let Some(id) = matched.as_deref() {
            let _ = self.store.touch_local_api_key(id).await;
        }
        matched
    }

    pub async fn target_endpoint(
        &self,
        target: &ModelTarget,
    ) -> anyhow::Result<(String, Option<String>, Option<String>)> {
        match target.kind {
            crate::domain::TargetKind::Cloud => {
                let provider_id = target
                    .provider_id
                    .as_deref()
                    .context("cloud target has no provider")?;
                let provider = self
                    .store
                    .provider(provider_id)
                    .await?
                    .context("provider not found")?;
                if !provider.enabled {
                    anyhow::bail!("provider is disabled");
                }
                let base_url = validate_cloud_base_url(
                    &provider.base_url,
                    provider.preset_id == "custom_openai" || cfg!(test),
                )?;
                let (credential, account_id) = if provider.auth_mode == AuthMode::OpenAiSubscription
                {
                    let credential = self.oauth.access_token(provider_id).await?;
                    (credential.access_token, credential.account_id)
                } else {
                    (self.provider_api_key(provider_id)?, None)
                };
                Ok((base_url, Some(credential), account_id))
            }
            crate::domain::TargetKind::Gguf | crate::domain::TargetKind::Mlx => {
                let token = self
                    .local_activity()
                    .token(&target.id)
                    .context("local runtime credential missing")?;
                Ok((
                    target
                        .runtime_url
                        .clone()
                        .context("local model is not loaded")?
                        .trim_end_matches('/')
                        .to_owned(),
                    Some(token),
                    None,
                ))
            }
        }
    }

    pub async fn providers_with_credentials(&self) -> anyhow::Result<Vec<Provider>> {
        let mut providers = self.store.providers().await?;
        for provider in &mut providers {
            provider.has_credential = self.secrets.get(&provider_account(&provider.id))?.is_some();
        }
        Ok(providers)
    }

    pub fn save_provider_api_key(&self, provider_id: &str, key: &str) -> anyhow::Result<()> {
        let value = serde_json::json!({ "version": 1, "type": "api_key", "key": key });
        self.secrets
            .set(&provider_account(provider_id), &value.to_string())
    }

    pub fn provider_api_key(&self, provider_id: &str) -> anyhow::Result<String> {
        let stored = self
            .secrets
            .get(&provider_account(provider_id))?
            .context("provider credential missing")?;
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&stored) else {
            return Ok(stored);
        };
        value
            .get("key")
            .and_then(|key| key.as_str())
            .map(str::to_owned)
            .context("provider API key missing")
    }

    pub async fn acquire_local_slot(
        &self,
        target: &ModelTarget,
    ) -> anyhow::Result<Option<LocalInferencePermit>> {
        if !matches!(
            target.kind,
            crate::domain::TargetKind::Gguf | crate::domain::TargetKind::Mlx
        ) {
            return Ok(None);
        }
        let policy = self.effective_resource_policy(target).await?;
        let activity = self.local_activity();
        let gate = activity.gate(&target.id, policy.max_parallel_prompts);
        gate.queued.fetch_add(1, Ordering::AcqRel);
        let waiting = WaitingGuard(gate.clone());
        let permit = gate
            .active
            .clone()
            .acquire_owned()
            .await
            .context("local inference gate closed")?;
        drop(waiting);
        gate.active_count.fetch_add(1, Ordering::AcqRel);
        *gate.last_used.lock() = std::time::Instant::now();
        Ok(Some(LocalInferencePermit {
            _permit: permit,
            gate,
        }))
    }

    pub async fn effective_resource_policy(
        &self,
        target: &ModelTarget,
    ) -> anyhow::Result<crate::resource::ResourcePolicy> {
        let logical_cpus = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1);
        let policy = self.store.resource_policy(logical_cpus).await?;
        let resolved = target
            .local
            .resource_overrides
            .as_ref()
            .map(|overrides| policy.resolve(overrides))
            .unwrap_or(policy);
        resolved.validate()?;
        Ok(resolved)
    }

    pub async fn validate_provider(
        &self,
        provider: &Provider,
        credential: &str,
    ) -> anyhow::Result<Vec<String>> {
        let preset = provider_preset(&provider.preset_id).context("provider preset missing")?;
        let mut request = self.client.get(format!(
            "{}/models",
            provider.base_url.trim_end_matches('/')
        ));
        request = match preset.auth_scheme {
            AuthScheme::Bearer => request.bearer_auth(credential),
            AuthScheme::XApiKey => request
                .header("x-api-key", credential)
                .header("anthropic-version", "2023-06-01"),
            AuthScheme::XGoogApiKey => request.header("x-goog-api-key", credential),
            AuthScheme::OpenAiSubscription => {
                anyhow::bail!("subscription providers use their curated model catalog")
            }
        };
        let response = request.send().await?;
        let status = response.status();
        let body: serde_json::Value = response
            .json()
            .await
            .context("provider returned invalid JSON")?;
        if !status.is_success() {
            anyhow::bail!("provider returned {status}");
        }
        Ok(body
            .get("data")
            .or_else(|| body.get("models"))
            .or_else(|| body.as_array().map(|_| &body))
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
            .filter_map(|value| {
                value
                    .get("id")
                    .or_else(|| value.get("name"))
                    .and_then(|id| id.as_str())
                    .map(|id| id.trim_start_matches("models/").to_owned())
            })
            .collect())
    }
}

fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn validated_key_name(name: &str) -> anyhow::Result<String> {
    let name = name.trim();
    if name.is_empty() {
        anyhow::bail!("local API key name is required");
    }
    if name.chars().count() > 80 {
        anyhow::bail!("local API key name must be at most 80 characters");
    }
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::TargetKind,
        secrets::{local_api_key_account, MemorySecrets, SecretStore, LOCAL_API_KEY},
    };

    fn local_target() -> ModelTarget {
        ModelTarget {
            id: "local".into(),
            provider_id: None,
            name: "Local".into(),
            kind: TargetKind::Gguf,
            provider_model: "local".into(),
            local_path: None,
            runtime_url: Some("http://127.0.0.1:1/v1".into()),
            wire_protocol: crate::providers::WireProtocol::OpenAiChat,
            capabilities: vec!["chat".into()],
            enabled: true,
            state: "ready".into(),
            size_bytes: None,
            local: crate::storage::LocalModelMeta::default(),
        }
    }

    #[tokio::test]
    async fn legacy_token_is_migrated_and_multiple_keys_authenticate_independently() {
        let store = Store::memory().await.unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        secrets.set(LOCAL_API_KEY, "legacy-token").unwrap();
        let core = AppCore::new(store, secrets.clone()).unwrap();

        core.migrate_legacy_local_api_key().await.unwrap();
        core.migrate_legacy_local_api_key().await.unwrap();
        assert_eq!(core.store.local_api_keys().await.unwrap().len(), 1);
        let (second, second_token) = core.create_local_api_key("Automation").await.unwrap();

        assert_eq!(
            core.authorized(Some("Bearer legacy-token"))
                .await
                .as_deref(),
            Some("default")
        );
        assert_eq!(
            core.authorized(Some(&format!("Bearer {second_token}")))
                .await
                .as_deref(),
            Some(second.id.as_str())
        );
        assert!(core.authorized(Some("Bearer invalid")).await.is_none());

        let rotated_token = core.rotate_local_api_key(&second.id).await.unwrap();
        assert!(core
            .authorized(Some(&format!("Bearer {second_token}")))
            .await
            .is_none());
        assert_eq!(
            core.authorized(Some(&format!("Bearer {rotated_token}")))
                .await
                .as_deref(),
            Some(second.id.as_str())
        );
        assert_eq!(
            core.reveal_local_api_key(&second.id).unwrap(),
            rotated_token
        );
        assert_eq!(
            core.rename_local_api_key(&second.id, "Renamed automation")
                .await
                .unwrap()
                .name,
            "Renamed automation"
        );

        core.revoke_local_api_key(&second.id).await.unwrap();
        assert!(core
            .authorized(Some(&format!("Bearer {rotated_token}")))
            .await
            .is_none());
        assert!(secrets
            .get(&local_api_key_account(&second.id))
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn local_inference_queue_is_unbounded_and_cancellation_safe() {
        let core = AppCore::new(
            Store::memory().await.unwrap(),
            Arc::new(MemorySecrets::default()),
        )
        .unwrap();
        let target = local_target();
        let active = core.acquire_local_slot(&target).await.unwrap().unwrap();
        let mut waiting = Vec::new();
        for _ in 0..32 {
            let core = core.clone();
            let target = target.clone();
            waiting.push(tokio::spawn(async move {
                core.acquire_local_slot(&target).await
            }));
        }
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let queued = core
                    .local_gates
                    .lock()
                    .get("local")
                    .unwrap()
                    .queued
                    .load(Ordering::Acquire);
                if queued == 32 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        for task in waiting.drain(..) {
            task.abort();
        }
        tokio::time::timeout(Duration::from_secs(1), async {
            while core.local_activity().queued(&target.id) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(active);
        let permit = tokio::time::timeout(Duration::from_secs(1), core.acquire_local_slot(&target))
            .await
            .expect("a new request should be admitted after cancelled waiters")
            .unwrap()
            .unwrap();
        drop(permit);
    }

    #[tokio::test]
    async fn unload_reservation_cannot_race_an_active_request() {
        let core = AppCore::new(
            Store::memory().await.unwrap(),
            Arc::new(MemorySecrets::default()),
        )
        .unwrap();
        let target = local_target();
        let activity = core.local_activity();
        let request = core.acquire_local_slot(&target).await.unwrap().unwrap();
        assert!(activity.try_reserve_for_unload(&target.id).is_none());
        drop(request);
        assert!(activity.try_reserve_for_unload(&target.id).is_some());
    }

    #[tokio::test]
    async fn legacy_raw_provider_keys_are_read_and_rewritten_as_versioned_records() {
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        secrets
            .set(&provider_account("provider"), "legacy-key")
            .unwrap();
        let core = AppCore::new(Store::memory().await.unwrap(), secrets.clone()).unwrap();
        assert_eq!(core.provider_api_key("provider").unwrap(), "legacy-key");
        core.save_provider_api_key("provider", "legacy-key")
            .unwrap();
        let stored = secrets.get(&provider_account("provider")).unwrap().unwrap();
        let value: serde_json::Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(value["version"], 1);
        assert_eq!(value["key"], "legacy-key");
    }
}

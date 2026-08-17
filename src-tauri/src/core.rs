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
    secrets::{generate_local_token, provider_account, SecretStore, LOCAL_API_KEY},
    storage::{ModelTarget, Provider, Store},
};

#[derive(Clone)]
pub struct AppCore {
    pub store: Store,
    pub secrets: Arc<dyn SecretStore>,
    pub client: Client,
    local_gates: Arc<parking_lot::Mutex<HashMap<String, Arc<InferenceGate>>>>,
}

struct InferenceGate {
    active: Arc<Semaphore>,
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
        *self.gate.last_used.lock() = std::time::Instant::now();
    }
}

#[derive(Clone, Default)]
pub struct LocalActivityRegistry {
    gates: Arc<parking_lot::Mutex<HashMap<String, Arc<InferenceGate>>>>,
}

impl LocalActivityRegistry {
    fn gate(&self, id: &str) -> Arc<InferenceGate> {
        self.gates
            .lock()
            .entry(id.to_owned())
            .or_insert_with(|| {
                Arc::new(InferenceGate {
                    active: Arc::new(Semaphore::new(1)),
                    queued: AtomicUsize::new(0),
                    last_used: parking_lot::Mutex::new(std::time::Instant::now()),
                    token: parking_lot::Mutex::new(None),
                })
            })
            .clone()
    }

    pub fn touch(&self, id: &str) {
        *self.gate(id).last_used.lock() = std::time::Instant::now();
    }

    pub fn set_token(&self, id: &str, token: String) {
        *self.gate(id).token.lock() = Some(token);
    }

    pub fn token(&self, id: &str) -> Option<String> {
        self.gates
            .lock()
            .get(id)
            .and_then(|gate| gate.token.lock().clone())
    }

    pub fn try_reserve_for_unload(&self, id: &str) -> Option<OwnedSemaphorePermit> {
        let gate = self.gates.lock().get(id)?.clone();
        gate.active.clone().try_acquire_owned().ok()
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
        Self::new(store, secrets)
    }

    pub fn new(store: Store, secrets: Arc<dyn SecretStore>) -> anyhow::Result<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .user_agent("LocalAI-Router/0.1")
            .build()?;
        Ok(Self {
            store,
            secrets,
            client,
            local_gates: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        })
    }

    pub fn local_activity(&self) -> LocalActivityRegistry {
        LocalActivityRegistry {
            gates: self.local_gates.clone(),
        }
    }

    pub fn ensure_local_token(&self) -> anyhow::Result<String> {
        if let Some(token) = self.secrets.get(LOCAL_API_KEY)? {
            return Ok(token);
        }
        let token = generate_local_token();
        self.secrets.set(LOCAL_API_KEY, &token)?;
        Ok(token)
    }

    pub fn rotate_local_token(&self) -> anyhow::Result<String> {
        let token = generate_local_token();
        self.secrets.set(LOCAL_API_KEY, &token)?;
        Ok(token)
    }

    pub fn authorized(&self, authorization: Option<&str>) -> bool {
        let Some(candidate) = authorization.and_then(|value| value.strip_prefix("Bearer ")) else {
            return false;
        };
        let Ok(Some(expected)) = self.secrets.get(LOCAL_API_KEY) else {
            return false;
        };
        let left = Sha256::digest(candidate.as_bytes());
        let right = Sha256::digest(expected.as_bytes());
        left.as_slice().ct_eq(right.as_slice()).into()
    }

    pub async fn target_endpoint(
        &self,
        target: &ModelTarget,
    ) -> anyhow::Result<(String, Option<String>)> {
        match target.kind {
            crate::domain::TargetKind::OpenAi | crate::domain::TargetKind::OpenRouter => {
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
                let credential = self
                    .secrets
                    .get(&provider_account(provider_id))?
                    .context("provider credential missing")?;
                Ok((
                    provider.base_url.trim_end_matches('/').to_owned(),
                    Some(credential),
                ))
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
        let activity = self.local_activity();
        let gate = activity.gate(&target.id);
        let waiting = gate.queued.fetch_add(1, Ordering::AcqRel);
        if waiting >= 8 {
            gate.queued.fetch_sub(1, Ordering::AcqRel);
            anyhow::bail!("local inference queue is full");
        }
        let permit = gate
            .active
            .clone()
            .acquire_owned()
            .await
            .context("local inference gate closed")?;
        gate.queued.fetch_sub(1, Ordering::AcqRel);
        *gate.last_used.lock() = std::time::Instant::now();
        Ok(Some(LocalInferencePermit {
            _permit: permit,
            gate,
        }))
    }

    pub async fn validate_provider(
        &self,
        provider: &Provider,
        credential: &str,
    ) -> anyhow::Result<Vec<String>> {
        let response = self
            .client
            .get(format!(
                "{}/models",
                provider.base_url.trim_end_matches('/')
            ))
            .bearer_auth(credential)
            .send()
            .await?;
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
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
            .filter_map(|value| {
                value
                    .get("id")
                    .and_then(|id| id.as_str())
                    .map(str::to_owned)
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{domain::TargetKind, secrets::MemorySecrets};

    fn local_target() -> ModelTarget {
        ModelTarget {
            id: "local".into(),
            provider_id: None,
            name: "Local".into(),
            kind: TargetKind::Gguf,
            provider_model: "local".into(),
            local_path: None,
            runtime_url: Some("http://127.0.0.1:1/v1".into()),
            capabilities: vec!["chat".into()],
            enabled: true,
            state: "ready".into(),
            size_bytes: None,
        }
    }

    #[tokio::test]
    async fn local_inference_allows_one_active_and_eight_queued_requests() {
        let core = AppCore::new(
            Store::memory().await.unwrap(),
            Arc::new(MemorySecrets::default()),
        )
        .unwrap();
        let target = local_target();
        let active = core.acquire_local_slot(&target).await.unwrap().unwrap();
        let mut waiting = Vec::new();
        for _ in 0..8 {
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
                if queued == 8 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(core.acquire_local_slot(&target).await.is_err());
        drop(active);
        for task in waiting {
            drop(task.await.unwrap().unwrap());
        }
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
}

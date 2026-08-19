use std::{collections::HashSet, path::PathBuf, sync::Arc};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tauri::State;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    core::{AppCore, InFlightRequest},
    domain::{ModelRoute, RouteRole, TargetKind},
    library,
    providers::{
        provider_preset, provider_presets, validate_cloud_base_url, AuthMode, ProviderPreset,
        WireProtocol,
    },
    resource::{ResourceOverrides, ResourcePolicy, ResourceProfile},
    routing::{
        builtin_tasks, evaluate_route, RoutingAttemptRecord, RoutingConfigExport, RoutingPolicy,
        RoutingTaskDefinition, TargetRoutingProfile,
    },
    runtime::{RuntimeManager, RuntimeStatus},
    secrets::{
        local_api_key_account, provider_account, CIVITAI_ACCOUNT, HF_ACCOUNT, LOCAL_API_KEY,
    },
    speculative::{self, SpeculativeConfig},
    storage::{
        KeyUsageData, LocalApiKey, LogFacets, LogQuery, LogResult, ModelTarget, Provider,
        ProviderModel, Store, UsageData,
    },
};

pub struct AppServices {
    pub core: Arc<AppCore>,
    pub runtimes: Arc<RuntimeManager>,
    pub model_library: PathBuf,
    pub port: u16,
    pub install: Arc<crate::install::InstallManager>,
    pub shutdown: CancellationToken,
}

#[derive(Debug, Serialize)]
pub struct Dashboard {
    pub running: bool,
    pub base_url: String,
    pub provider_count: usize,
    pub target_count: usize,
    pub route_count: usize,
    pub recent_requests: usize,
    pub runtimes: Vec<RuntimeStatus>,
    pub inflight: Vec<InFlightRequest>,
}

#[derive(Debug, Serialize)]
pub struct LocalApiKeyWithToken {
    #[serde(flatten)]
    pub key: LocalApiKey,
    pub token: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientChatInput {
    pub model: String,
    pub messages: Vec<ClientChatMessage>,
    pub session_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ClientChatResponse {
    pub content: String,
    pub model: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveProviderInput {
    pub id: Option<String>,
    pub name: String,
    pub preset_id: String,
    #[serde(default)]
    pub auth_mode: AuthMode,
    pub base_url: String,
    pub enabled: bool,
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportModelInput {
    pub source: String,
    pub name: String,
    pub kind: TargetKind,
    pub alias_model: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadModelInput {
    pub repo_id: String,
    pub filename: Option<String>,
    pub name: String,
    pub kind: TargetKind,
    pub alias_model: String,
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub source: Option<String>,
}

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn provider_model_from_discovery(
    preset_id: &str,
    model_id: &str,
    api: Option<&serde_json::Value>,
) -> ProviderModel {
    let meta = crate::model_catalog::resolve_model_metadata(model_id, api);
    ProviderModel {
        id: model_id.to_owned(),
        wire_protocol: crate::providers::inferred_protocol(preset_id, model_id),
        capabilities: meta.capabilities,
        context_window: Some(meta.context_window).filter(|value| *value > 0),
        input_price_per_million: meta.input_price_per_million,
        output_price_per_million: meta.output_price_per_million,
        cache_read_price_per_million: meta.cache_read_price_per_million,
        cache_write_price_per_million: meta.cache_write_price_per_million,
    }
}

#[tauri::command]
pub async fn dashboard(state: State<'_, AppServices>) -> Result<Dashboard, String> {
    let providers = state.core.providers_with_credentials().await.map_err(err)?;
    let targets = state.core.store.targets().await.map_err(err)?;
    let routes = state.core.store.routes().await.map_err(err)?;
    let recent_requests = state.core.store.logs(1000).await.map_err(err)?.len();
    let throughput = state
        .core
        .store
        .tokens_per_second_by_target(chrono::Utc::now())
        .await
        .map_err(err)?;
    let mut runtimes = state.runtimes.statuses();
    for runtime in &mut runtimes {
        let name = targets
            .iter()
            .find(|target| target.id == runtime.target_id)
            .map(|target| target.name.as_str());
        runtime.tokens_per_second = name
            .and_then(|name| throughput.get(name).copied())
            .or_else(|| throughput.get(&runtime.target_id).copied());
    }
    Ok(Dashboard {
        running: true,
        base_url: format!("http://127.0.0.1:{}/v1", state.port),
        provider_count: providers.len(),
        target_count: targets.len(),
        route_count: routes.len(),
        recent_requests,
        runtimes,
        inflight: state.core.traffic.snapshot(),
    })
}

#[tauri::command]
pub async fn cancel_inflight_request(
    state: State<'_, AppServices>,
    id: String,
) -> Result<(), String> {
    state.core.traffic.cancel(&id);
    Ok(())
}

#[tauri::command]
pub async fn cancel_all_inflight_requests(state: State<'_, AppServices>) -> Result<(), String> {
    state.core.traffic.cancel_all();
    Ok(())
}

#[tauri::command]
pub async fn list_local_api_keys(
    state: State<'_, AppServices>,
) -> Result<Vec<LocalApiKey>, String> {
    state.core.store.local_api_keys().await.map_err(err)
}

#[tauri::command]
pub async fn create_local_api_key(
    state: State<'_, AppServices>,
    name: String,
) -> Result<LocalApiKeyWithToken, String> {
    let (key, token) = state.core.create_local_api_key(&name).await.map_err(err)?;
    Ok(LocalApiKeyWithToken { key, token })
}

#[tauri::command]
pub async fn reveal_local_api_key(
    state: State<'_, AppServices>,
    id: String,
) -> Result<String, String> {
    let key = state
        .core
        .store
        .local_api_key(&id)
        .await
        .map_err(err)?
        .ok_or("local API key not found")?;
    if key.revoked_at.is_some() {
        return Err("local API key is revoked".into());
    }
    state.core.reveal_local_api_key(&id).map_err(err)
}

#[tauri::command]
pub async fn rename_local_api_key(
    state: State<'_, AppServices>,
    id: String,
    name: String,
) -> Result<LocalApiKey, String> {
    state
        .core
        .rename_local_api_key(&id, &name)
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn rotate_local_api_key(
    state: State<'_, AppServices>,
    id: String,
) -> Result<String, String> {
    state.core.rotate_local_api_key(&id).await.map_err(err)
}

#[tauri::command]
pub async fn revoke_local_api_key(state: State<'_, AppServices>, id: String) -> Result<(), String> {
    state.core.revoke_local_api_key(&id).await.map_err(err)
}

#[tauri::command]
pub async fn client_chat(
    state: State<'_, AppServices>,
    input: ClientChatInput,
) -> Result<ClientChatResponse, String> {
    let model = input.model.trim();
    if model.is_empty() {
        return Err("a model is required".into());
    }
    if input.messages.is_empty() || input.messages.len() > 200 {
        return Err("a chat must contain between 1 and 200 messages".into());
    }
    let total_chars = input.messages.iter().try_fold(0usize, |total, message| {
        if !matches!(message.role.as_str(), "system" | "user" | "assistant") {
            return Err("chat messages must use the system, user, or assistant role");
        }
        total
            .checked_add(message.content.len())
            .ok_or("chat request is too large")
    })?;
    if total_chars > 512_000 {
        return Err("chat request is too large".into());
    }

    let key = state
        .core
        .store
        .local_api_keys()
        .await
        .map_err(err)?
        .into_iter()
        .filter(|key| key.revoked_at.is_none())
        .find_map(|key| state.core.reveal_local_api_key(&key.id).ok())
        .ok_or("no active local API key is available")?;
    let mut request = state
        .core
        .client
        .post(format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            state.port
        ))
        .bearer_auth(key);
    if let Some(session_id) = input.session_id.as_deref() {
        request = request.header("X-Local-AI-Session", session_id);
    }
    let response = request
        .json(&serde_json::json!({
            "model": model,
            "messages": input.messages,
            "stream": false
        }))
        .send()
        .await
        .map_err(err)?;
    let status = response.status();
    let bytes = response.bytes().await.map_err(err)?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("chat response is too large".into());
    }
    let payload: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| format!("gateway returned an invalid response ({status})"))?;
    if !status.is_success() {
        let message = payload
            .pointer("/error/message")
            .and_then(|value| value.as_str())
            .unwrap_or("the gateway rejected the chat request");
        return Err(message.into());
    }
    let content = payload
        .pointer("/choices/0/message/content")
        .and_then(|value| value.as_str())
        .ok_or("gateway response did not contain assistant text")?;
    Ok(ClientChatResponse {
        content: content.into(),
        model: payload
            .get("model")
            .and_then(|value| value.as_str())
            .unwrap_or(model)
            .into(),
    })
}

#[tauri::command]
pub async fn list_providers(state: State<'_, AppServices>) -> Result<Vec<Provider>, String> {
    state.core.providers_with_credentials().await.map_err(err)
}

#[tauri::command]
pub fn list_provider_presets() -> Vec<ProviderPreset> {
    provider_presets()
}

#[tauri::command]
pub async fn save_provider(
    state: State<'_, AppServices>,
    input: SaveProviderInput,
) -> Result<Provider, String> {
    let preset = provider_preset(&input.preset_id).ok_or("unknown provider preset")?;
    if input.auth_mode != preset.auth_mode {
        return Err("authentication mode does not match provider preset".into());
    }
    let id = input.id.unwrap_or_else(|| Uuid::new_v4().to_string());
    if let Some(previous) = state.core.store.provider(&id).await.map_err(err)? {
        if previous.auth_mode != input.auth_mode {
            state
                .core
                .secrets
                .delete(&provider_account(&id))
                .map_err(err)?;
        }
    }
    let base_url = if preset.editable_base_url {
        validate_cloud_base_url(&input.base_url, preset.id == "custom_openai").map_err(err)?
    } else {
        preset
            .base_url
            .context("provider preset has no base URL")
            .map_err(err)?
            .to_owned()
    };
    let provider = Provider {
        id: id.clone(),
        name: input.name,
        preset_id: input.preset_id,
        auth_mode: input.auth_mode,
        base_url,
        enabled: input.enabled,
        has_credential: input.api_key.as_ref().is_some_and(|key| !key.is_empty()),
    };
    if let Some(key) = input.api_key.filter(|key| !key.trim().is_empty()) {
        state
            .core
            .save_provider_api_key(&id, key.trim())
            .map_err(err)?;
    } else if provider.auth_mode == AuthMode::ApiKey {
        if let Ok(existing) = state.core.provider_api_key(&id) {
            state
                .core
                .save_provider_api_key(&id, &existing)
                .map_err(err)?;
        }
    }
    state
        .core
        .store
        .upsert_provider(&provider)
        .await
        .map_err(err)?;
    let mut provider = provider;
    provider.has_credential = state
        .core
        .secrets
        .get(&provider_account(&id))
        .map_err(err)?
        .is_some();
    Ok(provider)
}

#[tauri::command]
pub async fn test_provider_connection(
    state: State<'_, AppServices>,
    id: String,
) -> Result<Vec<String>, String> {
    let provider = state
        .core
        .store
        .provider(&id)
        .await
        .map_err(err)?
        .ok_or("provider not found")?;
    if provider.auth_mode == AuthMode::OpenAiSubscription {
        state.core.oauth.access_token(&id).await.map_err(err)?;
        return Ok(vec!["subscription".into()]);
    }
    let credential = state.core.provider_api_key(&id).map_err(err)?;
    state
        .core
        .validate_provider(&provider, &credential)
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn begin_openai_subscription(
    state: State<'_, AppServices>,
    id: String,
) -> Result<crate::oauth::OAuthStart, String> {
    let provider = state
        .core
        .store
        .provider(&id)
        .await
        .map_err(err)?
        .ok_or("provider not found")?;
    if provider.auth_mode != AuthMode::OpenAiSubscription {
        return Err("provider is not configured for subscription OAuth".into());
    }
    state.core.oauth.begin(&id).await.map_err(err)
}

#[tauri::command]
pub async fn openai_subscription_status(
    state: State<'_, AppServices>,
    id: String,
) -> Result<crate::oauth::OAuthStatus, String> {
    state.core.oauth.status(&id).await.map_err(err)
}

#[tauri::command]
pub async fn logout_openai_subscription(
    state: State<'_, AppServices>,
    id: String,
) -> Result<(), String> {
    state.core.oauth.logout(&id).await.map_err(err)
}

#[tauri::command]
pub async fn delete_provider(state: State<'_, AppServices>, id: String) -> Result<(), String> {
    state.core.store.delete_provider(&id).await.map_err(err)?;
    state
        .core
        .secrets
        .delete(&provider_account(&id))
        .map_err(err)
}

#[tauri::command]
pub async fn sync_provider_models(
    state: State<'_, AppServices>,
    id: String,
) -> Result<Vec<ProviderModel>, String> {
    let provider = state
        .core
        .store
        .provider(&id)
        .await
        .map_err(err)?
        .ok_or("provider not found")?;
    let models = if provider.preset_id == "opencode_zen" {
        Vec::new()
    } else if provider.auth_mode == AuthMode::OpenAiSubscription {
        ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"]
            .into_iter()
            .map(|model_id| provider_model_from_discovery(&provider.preset_id, model_id, None))
            .collect()
    } else {
        let credential = state.core.provider_api_key(&id).map_err(err)?;
        state
            .core
            .discover_provider_models(&provider, &credential)
            .await
            .map_err(err)?
            .iter()
            .filter_map(|raw| {
                let model_id = crate::model_catalog::extract_model_id(raw)?;
                Some(provider_model_from_discovery(
                    &provider.preset_id,
                    &model_id,
                    Some(raw),
                ))
            })
            .collect()
    };
    state
        .core
        .store
        .replace_provider_models(&id, &models)
        .await
        .map_err(err)?;
    Ok(models)
}

#[tauri::command]
pub async fn cached_provider_models(
    state: State<'_, AppServices>,
    id: String,
) -> Result<Vec<ProviderModel>, String> {
    state.core.store.provider_models(&id).await.map_err(err)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchCatalogInput {
    pub query: Option<String>,
    pub cursor: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectModelInput {
    pub repo_id: String,
    pub revision: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallCatalogInput {
    pub repo_id: String,
    pub revision: Option<String>,
    pub catalog_id: Option<String>,
    #[serde(default)]
    pub confirm_over_budget: bool,
    pub name: Option<String>,
}

fn memory_budget_bytes(percent: u8) -> u64 {
    let mut system = sysinfo::System::new();
    system.refresh_memory();
    crate::catalog::memory_budget(system.total_memory(), percent)
}

async fn budget_from_store(store: &Store) -> anyhow::Result<(u8, u64)> {
    let percent = store
        .setting("memory_budget_percent")
        .await?
        .and_then(|value| value.parse().ok())
        .unwrap_or(70);
    Ok((percent, memory_budget_bytes(percent)))
}

async fn hub_client(state: &AppServices) -> Result<crate::hub::HubClient, String> {
    Ok(crate::hub::HubClient::new(
        crate::hub::hub_http_client().map_err(err)?,
        "https://huggingface.co",
        state.core.secrets.get(HF_ACCOUNT).map_err(err)?,
    ))
}

fn civitai_http(state: &AppServices) -> Result<(reqwest::Client, Option<String>), String> {
    Ok((
        crate::hub::hub_http_client().map_err(err)?,
        state.core.secrets.get(CIVITAI_ACCOUNT).map_err(err)?,
    ))
}

#[tauri::command]
pub async fn list_local_catalog(
    state: State<'_, AppServices>,
) -> Result<crate::catalog::LocalCatalog, String> {
    let (percent, budget) = budget_from_store(&state.core.store).await.map_err(err)?;
    Ok(crate::catalog::LocalCatalog {
        platform: crate::catalog::mac_compatibility(),
        memory_budget_bytes: budget,
        memory_budget_percent: percent,
        entries: crate::catalog::catalog_views(budget),
    })
}

#[tauri::command]
pub async fn search_mlx_catalog(
    state: State<'_, AppServices>,
    input: SearchCatalogInput,
) -> Result<crate::hub::SearchPage, String> {
    let (_, budget) = budget_from_store(&state.core.store).await.map_err(err)?;
    if crate::civitai::CivitaiHost::parse(input.source.as_deref().unwrap_or("")).is_some() {
        let (http, token) = civitai_http(&state)?;
        return crate::civitai::search(
            http,
            token,
            input.query.as_deref().unwrap_or(""),
            input.cursor.as_deref(),
            budget,
        )
        .await
        .map_err(err);
    }
    hub_client(&state)
        .await?
        .search(
            input.query.as_deref().unwrap_or(""),
            input.cursor.as_deref(),
            budget,
        )
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn inspect_mlx_model(
    state: State<'_, AppServices>,
    input: InspectModelInput,
) -> Result<crate::hub::ModelInspection, String> {
    let (_, budget) = budget_from_store(&state.core.store).await.map_err(err)?;
    if crate::civitai::is_civitai_repo(&input.repo_id) {
        let (http, token) = civitai_http(&state)?;
        return crate::civitai::inspect(http, token, &input.repo_id, budget)
            .await
            .map_err(err);
    }
    let has_token = state.core.secrets.get(HF_ACCOUNT).map_err(err)?.is_some();
    hub_client(&state)
        .await?
        .inspect(&input.repo_id, input.revision.as_deref(), budget, has_token)
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn install_catalog_model(
    state: State<'_, AppServices>,
    input: InstallCatalogInput,
) -> Result<crate::storage::InstallJob, String> {
    let (_, budget) = budget_from_store(&state.core.store).await.map_err(err)?;
    let curated = input
        .catalog_id
        .as_deref()
        .and_then(crate::catalog::curated_by_id);
    if let Some(model) = &curated {
        if !model.installable {
            return Err(model
                .lock_reason
                .unwrap_or("this catalog entry is locked")
                .into());
        }
    }
    let inspection = if crate::civitai::is_civitai_repo(&input.repo_id) {
        let (http, token) = civitai_http(&state)?;
        crate::civitai::inspect(http, token, &input.repo_id, budget)
            .await
            .map_err(err)?
    } else {
        let has_token = state.core.secrets.get(HF_ACCOUNT).map_err(err)?.is_some();
        hub_client(&state)
            .await?
            .inspect(&input.repo_id, input.revision.as_deref(), budget, has_token)
            .await
            .map_err(err)?
    };
    let catalog_id = input.catalog_id.clone().or_else(|| {
        crate::civitai::is_civitai_repo(&input.repo_id)
            .then(|| crate::civitai::catalog_id_from_inspection(&inspection).to_string())
    });
    let (engine, task, capabilities, estimated, name) = if let Some(model) = curated {
        (
            model.runtime_engine.to_string(),
            model.task.to_string(),
            model
                .capabilities
                .iter()
                .map(|item| (*item).to_string())
                .collect(),
            model.measured_peak_bytes,
            input.name.unwrap_or_else(|| model.name.to_string()),
        )
    } else {
        (
            inspection
                .runtime_engine
                .clone()
                .unwrap_or_else(|| "mlx_chat".into()),
            inspection.task.clone().unwrap_or_else(|| "chat".into()),
            inspection.capabilities.clone(),
            inspection.estimated_memory_bytes,
            input.name.unwrap_or_else(|| {
                input
                    .repo_id
                    .split('/')
                    .next_back()
                    .unwrap_or("model")
                    .into()
            }),
        )
    };
    state
        .install
        .start(
            inspection,
            catalog_id,
            input.confirm_over_budget,
            budget,
            name,
            capabilities,
            engine,
            task,
            estimated,
        )
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn list_install_jobs(
    state: State<'_, AppServices>,
) -> Result<Vec<crate::storage::InstallJob>, String> {
    state.install.list().await.map_err(err)
}

#[tauri::command]
pub async fn pause_install_job(
    state: State<'_, AppServices>,
    id: String,
) -> Result<crate::storage::InstallJob, String> {
    state.install.pause(&id).await.map_err(err)
}

#[tauri::command]
pub async fn resume_install_job(
    state: State<'_, AppServices>,
    id: String,
) -> Result<crate::storage::InstallJob, String> {
    state.install.resume(&id).await.map_err(err)
}

#[tauri::command]
pub async fn cancel_install_job(
    state: State<'_, AppServices>,
    id: String,
) -> Result<crate::storage::InstallJob, String> {
    state.install.cancel(&id).await.map_err(err)
}

#[tauri::command]
pub async fn clear_install_job(state: State<'_, AppServices>, id: String) -> Result<(), String> {
    state.install.clear(&id).await.map_err(err)
}

#[tauri::command]
pub async fn list_targets(state: State<'_, AppServices>) -> Result<Vec<ModelTarget>, String> {
    state.core.store.targets().await.map_err(err)
}

fn apply_known_model_defaults(target: &mut ModelTarget) {
    let meta = crate::model_catalog::resolve_model_metadata(&target.provider_model, None);
    if crate::model_catalog::capabilities_are_placeholder(&target.capabilities)
        && meta.source != crate::model_catalog::MetadataSource::Fallback
    {
        target.capabilities = meta.capabilities;
    }
}

async fn seed_target_routing_profile(store: &Store, target: &ModelTarget) -> anyhow::Result<()> {
    let mut profile = crate::routing::TargetRoutingProfile::for_target(target);
    if let Some(provider_id) = &target.provider_id {
        if let Some(cached) = store
            .provider_models(provider_id)
            .await?
            .into_iter()
            .find(|model| model.id == target.provider_model)
        {
            if let Some(value) = cached.input_price_per_million {
                profile.input_price_per_million = Some(value);
            }
            if let Some(value) = cached.output_price_per_million {
                profile.output_price_per_million = Some(value);
            }
            if let Some(value) = cached.context_window.filter(|window| *window > 0) {
                profile.context_window = value;
            }
        }
    }
    store.upsert_target_routing_profile(&profile).await
}

async fn persist_target(store: &Store, mut target: ModelTarget) -> Result<ModelTarget, String> {
    if let Some(provider_id) = &target.provider_id {
        if let Some(cached) = store
            .provider_models(provider_id)
            .await
            .map_err(err)?
            .into_iter()
            .find(|model| model.id == target.provider_model)
        {
            if crate::model_catalog::capabilities_are_placeholder(&target.capabilities) {
                target.capabilities = cached.capabilities;
            }
        }
    }
    apply_known_model_defaults(&mut target);
    let seed_profile = store
        .target_routing_profile(&target.id)
        .await
        .map_err(err)?
        .is_none();
    store.upsert_target(&target).await.map_err(err)?;
    if seed_profile {
        seed_target_routing_profile(store, &target)
            .await
            .map_err(err)?;
    }
    Ok(target)
}

#[tauri::command]
pub async fn lookup_model_metadata(
    model: String,
) -> Result<crate::model_catalog::ModelMetadata, String> {
    Ok(crate::model_catalog::resolve_model_metadata(&model, None))
}

#[tauri::command]
pub async fn save_target(
    state: State<'_, AppServices>,
    target: ModelTarget,
) -> Result<ModelTarget, String> {
    persist_target(&state.core.store, target).await
}

#[tauri::command]
pub async fn delete_target(state: State<'_, AppServices>, id: String) -> Result<(), String> {
    let dependents: Vec<String> = state
        .core
        .store
        .targets()
        .await
        .map_err(err)?
        .into_iter()
        .filter(|target| {
            target
                .local
                .speculative_config
                .as_ref()
                .and_then(|config| config.draft_target_id.as_deref())
                == Some(id.as_str())
        })
        .map(|target| target.id)
        .collect();
    state.runtimes.stop(&id).await.map_err(err)?;
    state.core.store.delete_target(&id).await.map_err(err)?;
    for dependent in dependents {
        state.runtimes.mark_target_pending_restart(&dependent);
    }
    Ok(())
}

#[tauri::command]
pub async fn import_local_model(
    state: State<'_, AppServices>,
    input: ImportModelInput,
) -> Result<ModelTarget, String> {
    let imported = library::import_model(
        PathBuf::from(&input.source).as_path(),
        &state.model_library,
        input.kind.clone(),
    )
    .await
    .map_err(err)?;
    let target = ModelTarget {
        id: Uuid::new_v4().to_string(),
        provider_id: None,
        name: input.name,
        kind: input.kind,
        provider_model: input.alias_model,
        local_path: Some(imported.path),
        runtime_url: None,
        wire_protocol: WireProtocol::OpenAiChat,
        capabilities: input.capabilities,
        enabled: true,
        state: "stopped".into(),
        size_bytes: Some(imported.size_bytes as i64),
        local: crate::storage::LocalModelMeta::default(),
    };
    persist_target(&state.core.store, target).await
}

#[tauri::command]
pub async fn download_local_model(
    state: State<'_, AppServices>,
    input: DownloadModelInput,
) -> Result<ModelTarget, String> {
    let imported = library::download_hugging_face(
        &crate::hub::hub_http_client().map_err(err)?,
        state.core.secrets.clone(),
        &input.repo_id,
        input.filename.as_deref(),
        &state.model_library,
        input.kind.clone(),
    )
    .await
    .map_err(err)?;
    let target = ModelTarget {
        id: Uuid::new_v4().to_string(),
        provider_id: None,
        name: input.name,
        kind: input.kind,
        provider_model: input.alias_model,
        local_path: Some(imported.path),
        runtime_url: None,
        wire_protocol: WireProtocol::OpenAiChat,
        capabilities: input.capabilities,
        enabled: true,
        state: "stopped".into(),
        size_bytes: Some(imported.size_bytes as i64),
        local: crate::storage::LocalModelMeta::default(),
    };
    persist_target(&state.core.store, target).await
}

#[tauri::command]
pub async fn start_local_model(
    state: State<'_, AppServices>,
    id: String,
) -> Result<ModelTarget, String> {
    let mut target = state
        .core
        .store
        .target(&id)
        .await
        .map_err(err)?
        .ok_or("model not found")?;
    let runtime_url = state
        .runtimes
        .start_resolved(&state.core.store, &target)
        .await
        .map_err(err)?;
    let active = state
        .runtimes
        .statuses()
        .into_iter()
        .map(|runtime| runtime.target_id)
        .collect::<std::collections::HashSet<_>>();
    for mut other in state.core.store.targets().await.map_err(err)? {
        if matches!(other.kind, TargetKind::Gguf | TargetKind::Mlx)
            && other.runtime_url.is_some()
            && !active.contains(&other.id)
        {
            other.runtime_url = None;
            other.state = "stopped".into();
            state.core.store.upsert_target(&other).await.map_err(err)?;
        }
    }
    target.runtime_url = Some(runtime_url);
    target.state = "ready".into();
    state.core.store.upsert_target(&target).await.map_err(err)?;
    Ok(target)
}

#[tauri::command]
pub async fn stop_local_model(
    state: State<'_, AppServices>,
    id: String,
) -> Result<ModelTarget, String> {
    state.runtimes.stop(&id).await.map_err(err)?;
    let mut target = state
        .core
        .store
        .target(&id)
        .await
        .map_err(err)?
        .ok_or("model not found")?;
    target.runtime_url = None;
    target.state = "stopped".into();
    state.core.store.upsert_target(&target).await.map_err(err)?;
    Ok(target)
}

#[tauri::command]
pub async fn list_routes(state: State<'_, AppServices>) -> Result<Vec<ModelRoute>, String> {
    state.core.store.routes().await.map_err(err)
}

#[tauri::command]
pub async fn list_public_models(
    state: State<'_, AppServices>,
) -> Result<Vec<crate::public_models::PublicModel>, String> {
    crate::public_models::list_public_models(&state.core.store)
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn save_route(
    state: State<'_, AppServices>,
    mut route: ModelRoute,
) -> Result<ModelRoute, String> {
    if route.alias.trim().is_empty() || route.alias.contains(char::is_whitespace) {
        return Err("alias must be non-empty and contain no whitespace".into());
    }
    if crate::public_models::is_reserved_public_model_id(&route.alias) {
        return Err(
            "adaptive-routing is a built-in model and cannot be used as a custom alias".into(),
        );
    }
    if !route
        .targets
        .iter()
        .any(|target| target.enabled && target.role == RouteRole::Primary)
    {
        return Err("route must have at least one enabled primary".into());
    }
    let mut advertised = Vec::new();
    for route_target in route.targets.iter().filter(|target| target.enabled) {
        if route_target.id == route.alias {
            return Err("a route cannot fall back to itself".into());
        }
        let capabilities = if route_target.kind.is_alias()
            || state
                .core
                .store
                .target(&route_target.id)
                .await
                .map_err(err)?
                .is_none()
        {
            crate::public_models::resolve_public_model(&state.core.store, &route_target.id)
                .await
                .map_err(err)?
                .ok_or("route fallback alias not found")?
                .route
                .capabilities
        } else {
            let target = state
                .core
                .store
                .target(&route_target.id)
                .await
                .map_err(err)?
                .ok_or("route target not found")?;
            let mut capabilities = target.capabilities.clone();
            if target.wire_protocol == WireProtocol::AnthropicMessages {
                capabilities.retain(|item| item != "structured_output");
            }
            if target.wire_protocol == WireProtocol::GeminiGenerateContent {
                capabilities.retain(|item| item != "reasoning");
            }
            capabilities
        };
        for capability in capabilities {
            if !advertised.contains(&capability) {
                advertised.push(capability);
            }
        }
    }
    advertised.sort();
    route.capabilities = advertised;
    state.core.store.upsert_route(&route).await.map_err(err)?;
    prune_route_policy(&state.core.store, &route).await?;
    Ok(route)
}

#[tauri::command]
pub async fn delete_route(state: State<'_, AppServices>, alias: String) -> Result<(), String> {
    state.core.store.delete_route(&alias).await.map_err(err)
}

#[tauri::command]
pub async fn list_routing_policies(
    state: State<'_, AppServices>,
) -> Result<Vec<RoutingPolicy>, String> {
    state.core.store.routing_policies().await.map_err(err)
}

#[tauri::command]
pub async fn save_routing_policy(
    state: State<'_, AppServices>,
    policy: RoutingPolicy,
) -> Result<RoutingPolicy, String> {
    let route = state
        .core
        .store
        .route(&policy.alias)
        .await
        .map_err(err)?
        .ok_or("route not found")?;
    let mut tasks = builtin_tasks();
    tasks.extend(state.core.store.custom_routing_tasks().await.map_err(err)?);
    let known = tasks.into_iter().map(|task| task.id).collect::<Vec<_>>();
    let policy = prepare_routing_policy(policy, &route, &known)?;
    state
        .core
        .store
        .upsert_routing_policy(&policy)
        .await
        .map_err(err)?;
    Ok(policy)
}

fn prepare_routing_policy(
    mut policy: RoutingPolicy,
    route: &ModelRoute,
    known_tasks: &[String],
) -> Result<RoutingPolicy, String> {
    let hop_ids = route.primary_ids();
    policy.retain_route_candidates(&hop_ids);
    policy.validate(known_tasks)?;
    Ok(policy)
}

async fn prune_route_policy(store: &Store, route: &ModelRoute) -> Result<(), String> {
    let Some(mut policy) = store.routing_policy(&route.alias).await.map_err(err)? else {
        return Ok(());
    };
    let hop_ids = route.primary_ids();
    policy.retain_route_candidates(&hop_ids);
    if policy.candidate_target_ids.is_empty() {
        policy.candidate_target_ids = hop_ids;
    }
    store.upsert_routing_policy(&policy).await.map_err(err)?;
    Ok(())
}

#[tauri::command]
pub async fn list_target_routing_profiles(
    state: State<'_, AppServices>,
) -> Result<Vec<TargetRoutingProfile>, String> {
    state
        .core
        .store
        .target_routing_profiles()
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn save_target_routing_profile(
    state: State<'_, AppServices>,
    profile: TargetRoutingProfile,
) -> Result<TargetRoutingProfile, String> {
    if state
        .core
        .store
        .target(&profile.target_id)
        .await
        .map_err(err)?
        .is_none()
    {
        return Err("model target not found".into());
    }
    profile.validate()?;
    let mut known = builtin_tasks()
        .into_iter()
        .map(|task| task.id)
        .collect::<Vec<_>>();
    known.extend(
        state
            .core
            .store
            .custom_routing_tasks()
            .await
            .map_err(err)?
            .into_iter()
            .map(|task| task.id),
    );
    if let Some(task) = profile
        .task_quality
        .keys()
        .find(|task| !known.contains(task))
    {
        return Err(format!("unknown task in routing profile: {task}"));
    }
    state
        .core
        .store
        .upsert_target_routing_profile(&profile)
        .await
        .map_err(err)?;
    Ok(profile)
}

#[tauri::command]
pub async fn list_routing_tasks(
    state: State<'_, AppServices>,
) -> Result<Vec<RoutingTaskDefinition>, String> {
    let mut tasks = builtin_tasks();
    tasks.extend(state.core.store.custom_routing_tasks().await.map_err(err)?);
    Ok(tasks)
}

#[tauri::command]
pub async fn save_routing_task(
    state: State<'_, AppServices>,
    mut task: RoutingTaskDefinition,
) -> Result<RoutingTaskDefinition, String> {
    task.id = task
        .id
        .trim()
        .to_lowercase()
        .replace(char::is_whitespace, "_");
    task.label = task.label.trim().to_owned();
    task.builtin = false;
    if task.id.is_empty()
        || !task
            .id
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '_' | '-'))
        || task.label.is_empty()
        || builtin_tasks().iter().any(|builtin| builtin.id == task.id)
    {
        return Err(
            "custom task id and label must be non-empty and must not replace a built-in task"
                .into(),
        );
    }
    state
        .core
        .store
        .upsert_routing_task(&task)
        .await
        .map_err(err)?;
    Ok(task)
}

#[tauri::command]
pub async fn delete_routing_task(state: State<'_, AppServices>, id: String) -> Result<(), String> {
    if builtin_tasks().iter().any(|task| task.id == id) {
        return Err("built-in routing tasks cannot be deleted".into());
    }
    if state
        .core
        .store
        .routing_policies()
        .await
        .map_err(err)?
        .iter()
        .any(|policy| policy.default_task == id || policy.rules.iter().any(|rule| rule.task == id))
        || state
            .core
            .store
            .target_routing_profiles()
            .await
            .map_err(err)?
            .iter()
            .any(|profile| profile.task_quality.contains_key(&id))
    {
        return Err("routing task is still referenced by a policy or target profile".into());
    }
    state.core.store.delete_routing_task(&id).await.map_err(err)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingSimulationInput {
    pub alias: String,
    pub policy: Option<RoutingPolicy>,
    pub task: Option<String>,
    pub endpoint: Option<String>,
    pub text: Option<String>,
    #[serde(default)]
    pub has_tools: bool,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub modalities: Vec<String>,
    pub max_output_tokens: Option<u64>,
}

#[tauri::command]
pub async fn simulate_routing(
    state: State<'_, AppServices>,
    input: RoutingSimulationInput,
) -> Result<crate::routing::RoutingEvaluation, String> {
    let resolved = crate::public_models::resolve_public_model(&state.core.store, &input.alias)
        .await
        .map_err(err)?
        .ok_or("route not found")?;
    let route = resolved.route;
    let policy = match input.policy {
        Some(policy) => policy,
        None => resolved
            .policy
            .unwrap_or_else(|| RoutingPolicy::new(&input.alias)),
    };
    let mut canonical = crate::protocol::CanonicalRequest {
        system: input
            .text
            .map(|text| vec![crate::protocol::ContentBlock::Text { text }])
            .unwrap_or_default(),
        messages: vec![],
        tools: vec![],
        tool_choice: None,
        parallel_tool_calls: None,
        max_tokens: input.max_output_tokens,
        temperature: None,
        top_p: None,
        stop: None,
        reasoning: input
            .reasoning
            .then(|| serde_json::json!({"effort":"medium"})),
        response_format: None,
        stream: false,
    };
    if input.has_tools {
        canonical.tools.push(crate::protocol::CanonicalTool {
            name: "simulated_tool".into(),
            description: None,
            input_schema: serde_json::json!({"type":"object"}),
        });
    }
    for modality in input.modalities {
        let block = match modality.as_str() {
            "vision" => crate::protocol::ContentBlock::Image {
                url: "simulated".into(),
                media_type: None,
            },
            "audio" => crate::protocol::ContentBlock::Audio {
                url: "simulated".into(),
                media_type: None,
            },
            "video" => crate::protocol::ContentBlock::Video {
                url: "simulated".into(),
                media_type: None,
            },
            _ => continue,
        };
        canonical.system.push(block);
    }
    let mut required = vec!["chat".into()];
    if input.has_tools {
        required.push("tools".into());
    }
    if input.reasoning {
        required.push("reasoning".into());
    }
    evaluate_route(
        &state.core.store,
        &route,
        crate::routing::RouteEvaluationInput {
            policy: Some(&policy),
            explicit_task: input.task.as_deref(),
            endpoint: input.endpoint.as_deref().unwrap_or("/v1/chat/completions"),
            canonical: Some(&canonical),
            required_capabilities: required,
            streaming: false,
        },
    )
    .await
    .map_err(err)
}

#[tauri::command]
pub async fn list_routing_attempts(
    state: State<'_, AppServices>,
    request_id: Option<String>,
    limit: Option<i64>,
) -> Result<Vec<RoutingAttemptRecord>, String> {
    state
        .core
        .store
        .routing_attempts(request_id.as_deref(), limit.unwrap_or(100))
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn export_routing_config(
    state: State<'_, AppServices>,
) -> Result<RoutingConfigExport, String> {
    Ok(RoutingConfigExport {
        schema: "local-ai-router/routing-policy/v1".into(),
        tasks: state.core.store.custom_routing_tasks().await.map_err(err)?,
        profiles: state
            .core
            .store
            .target_routing_profiles()
            .await
            .map_err(err)?,
        policies: state.core.store.routing_policies().await.map_err(err)?,
    })
}

#[derive(Debug, Serialize)]
pub struct RoutingImportPreview {
    pub valid: bool,
    pub task_count: usize,
    pub profile_count: usize,
    pub policy_count: usize,
    pub warnings: Vec<String>,
}

#[tauri::command]
pub async fn import_routing_config(
    state: State<'_, AppServices>,
    config: RoutingConfigExport,
    apply: bool,
) -> Result<RoutingImportPreview, String> {
    if config.schema != "local-ai-router/routing-policy/v1" {
        return Err("unsupported routing configuration schema".into());
    }
    let builtin = builtin_tasks()
        .into_iter()
        .map(|task| task.id)
        .collect::<Vec<_>>();
    let mut known = builtin.clone();
    let mut task_ids = HashSet::new();
    for task in &config.tasks {
        if task.id.trim().is_empty()
            || !task
                .id
                .chars()
                .all(|value| value.is_ascii_alphanumeric() || matches!(value, '_' | '-'))
            || task.label.trim().is_empty()
            || task.builtin
            || builtin.contains(&task.id)
            || !task_ids.insert(task.id.clone())
        {
            return Err("invalid or duplicate custom routing task".into());
        }
        known.push(task.id.clone());
    }
    let targets = state.core.store.targets().await.map_err(err)?;
    let routes = state.core.store.routes().await.map_err(err)?;
    let mut profile_ids = HashSet::new();
    for profile in &config.profiles {
        profile.validate()?;
        if !profile_ids.insert(profile.target_id.clone()) {
            return Err(format!("duplicate routing profile: {}", profile.target_id));
        }
        if let Some(task) = profile
            .task_quality
            .keys()
            .find(|task| !known.contains(task))
        {
            return Err(format!("unknown task in routing profile: {task}"));
        }
        if !targets.iter().any(|target| target.id == profile.target_id) {
            return Err(format!(
                "unknown target in routing profile: {}",
                profile.target_id
            ));
        }
    }
    let mut policy_aliases = HashSet::new();
    for policy in &config.policies {
        policy.validate(&known)?;
        if !policy_aliases.insert(policy.alias.clone()) {
            return Err(format!("duplicate routing policy: {}", policy.alias));
        }
        if !routes.iter().any(|route| route.alias == policy.alias) {
            return Err(format!("unknown alias in routing policy: {}", policy.alias));
        }
        let route = routes
            .iter()
            .find(|route| route.alias == policy.alias)
            .expect("route existence checked above");
        if let Some(target_id) = policy
            .candidate_target_ids
            .iter()
            .find(|id| !route.targets.iter().any(|target| target.id == id.as_str()))
        {
            return Err(format!(
                "candidate target is not part of alias: {target_id}"
            ));
        }
    }
    let warnings = config
        .profiles
        .iter()
        .filter(|profile| {
            profile.input_price_per_million.is_none() || profile.output_price_per_million.is_none()
        })
        .map(|profile| format!("{} has unknown pricing", profile.target_id))
        .collect();
    if apply {
        state
            .core
            .store
            .import_routing_config(&config)
            .await
            .map_err(err)?;
    }
    Ok(RoutingImportPreview {
        valid: true,
        task_count: config.tasks.len(),
        profile_count: config.profiles.len(),
        policy_count: config.policies.len(),
        warnings,
    })
}

#[tauri::command]
pub async fn list_logs(
    state: State<'_, AppServices>,
    query: Option<LogQuery>,
) -> Result<LogResult, String> {
    state
        .core
        .store
        .query_logs(&query.unwrap_or_default())
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn get_usage(
    state: State<'_, AppServices>,
    period: String,
    target: Option<String>,
) -> Result<UsageData, String> {
    state
        .core
        .store
        .usage_for_target(&period, target.as_deref())
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn get_key_usage(
    state: State<'_, AppServices>,
    id: String,
    period: String,
) -> Result<KeyUsageData, String> {
    state
        .core
        .store
        .usage_for_key(&id, &period)
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn get_log_facets(state: State<'_, AppServices>) -> Result<LogFacets, String> {
    state.core.store.log_facets().await.map_err(err)
}

#[tauri::command]
pub async fn clear_logs(state: State<'_, AppServices>) -> Result<(), String> {
    state.core.store.clear_logs().await.map_err(err)
}

#[tauri::command]
pub async fn export_logs_csv(
    state: State<'_, AppServices>,
    path: String,
    query: Option<LogQuery>,
) -> Result<(), String> {
    let csv = logs_csv(&state.core.store, query.unwrap_or_default())
        .await
        .map_err(err)?;
    tokio::fs::write(path, csv).await.map_err(err)
}

async fn logs_csv(store: &Store, mut query: LogQuery) -> anyhow::Result<String> {
    query.limit = Some(500);
    query.offset = Some(0);
    let mut logs = Vec::new();
    loop {
        let page = store.query_logs(&query).await?;
        let page_len = page.items.len();
        logs.extend(page.items);
        if logs.len() as i64 >= page.total || page_len == 0 {
            break;
        }
        query.offset = Some(logs.len() as i64);
    }
    let mut csv = String::from("id,created_at,api_key_id,api_key_name,endpoint,alias,target,attempts,status,latency_ms,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,error_code,error_message\n");
    for log in logs {
        let values = [
            log.id,
            log.created_at.to_rfc3339(),
            log.api_key_id.unwrap_or_default(),
            log.api_key_name
                .unwrap_or_else(|| "Unknown / Legacy".into()),
            log.endpoint,
            log.alias.unwrap_or_default(),
            log.target.unwrap_or_default(),
            log.attempts.to_string(),
            log.status.to_string(),
            log.latency_ms.to_string(),
            log.input_tokens.map(|v| v.to_string()).unwrap_or_default(),
            log.output_tokens.map(|v| v.to_string()).unwrap_or_default(),
            log.cache_read_tokens
                .map(|v| v.to_string())
                .unwrap_or_default(),
            log.cache_write_tokens
                .map(|v| v.to_string())
                .unwrap_or_default(),
            log.error_code.unwrap_or_default(),
            log.error_message.unwrap_or_default(),
        ];
        csv.push_str(
            &values
                .into_iter()
                .map(csv_cell)
                .collect::<Vec<_>>()
                .join(","),
        );
        csv.push('\n');
    }
    Ok(csv)
}

#[tauri::command]
pub async fn get_settings(
    state: State<'_, AppServices>,
) -> Result<std::collections::HashMap<String, String>, String> {
    let mut result = std::collections::HashMap::new();
    for key in [
        "port",
        "memory_budget_percent",
        "idle_unload_minutes",
        "log_retention_days",
    ] {
        if let Some(value) = state.core.store.setting(key).await.map_err(err)? {
            result.insert(key.into(), value);
        }
    }
    result.insert(
        "has_hf_token".into(),
        state
            .core
            .secrets
            .get(HF_ACCOUNT)
            .map_err(err)?
            .is_some()
            .to_string(),
    );
    result.insert(
        "has_civitai_token".into(),
        state
            .core
            .secrets
            .get(CIVITAI_ACCOUNT)
            .map_err(err)?
            .is_some()
            .to_string(),
    );
    Ok(result)
}

#[tauri::command]
pub async fn save_setting(
    state: State<'_, AppServices>,
    key: String,
    value: String,
) -> Result<(), String> {
    if ![
        "memory_budget_percent",
        "idle_unload_minutes",
        "log_retention_days",
    ]
    .contains(&key.as_str())
    {
        return Err("setting is not editable at runtime".into());
    }
    let parsed = value
        .parse::<u64>()
        .map_err(|_| "setting must be a positive integer")?;
    if parsed == 0 {
        return Err("setting must be greater than zero".into());
    }
    state
        .core
        .store
        .set_setting(&key, &value)
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn get_resource_policy(state: State<'_, AppServices>) -> Result<ResourcePolicy, String> {
    let logical_cpus = crate::resource::host_performance_cpu_count();
    state
        .core
        .store
        .resource_policy(logical_cpus)
        .await
        .map_err(err)
}

#[tauri::command]
pub fn get_resource_profile_preset(profile: ResourceProfile) -> Result<ResourcePolicy, String> {
    if profile == ResourceProfile::Custom {
        return Err("custom is not a preset".into());
    }
    Ok(ResourcePolicy::preset(
        profile,
        crate::resource::host_performance_cpu_count(),
    ))
}

#[tauri::command]
pub async fn save_resource_policy(
    state: State<'_, AppServices>,
    policy: ResourcePolicy,
) -> Result<(), String> {
    policy.validate().map_err(err)?;
    state
        .core
        .store
        .set_resource_policy(&policy)
        .await
        .map_err(err)?;
    state
        .core
        .store
        .set_setting(
            "memory_budget_percent",
            &policy.memory_budget_percent.to_string(),
        )
        .await
        .map_err(err)?;
    state
        .core
        .store
        .set_setting(
            "idle_unload_minutes",
            &policy.idle_unload_minutes.to_string(),
        )
        .await
        .map_err(err)?;
    state.runtimes.apply_policy(policy).map_err(err)
}

#[tauri::command]
pub async fn save_model_resource_overrides(
    state: State<'_, AppServices>,
    id: String,
    overrides: Option<ResourceOverrides>,
    force_tool_support: Option<bool>,
) -> Result<ModelTarget, String> {
    let mut target = state
        .core
        .store
        .target(&id)
        .await
        .map_err(err)?
        .ok_or("model not found")?;
    if !matches!(target.kind, TargetKind::Gguf | TargetKind::Mlx) {
        return Err("resource overrides are only available for local models".into());
    }
    target.local.resource_overrides = overrides;
    if let Some(force_tool_support) = force_tool_support {
        target.local.force_tool_support = Some(force_tool_support);
    }
    state
        .core
        .effective_resource_policy(&target)
        .await
        .map_err(err)?;
    state.core.store.upsert_target(&target).await.map_err(err)?;
    state.runtimes.mark_target_pending_restart(&id);
    Ok(target)
}

#[tauri::command]
pub async fn save_model_speculative_config(
    state: State<'_, AppServices>,
    id: String,
    config: Option<SpeculativeConfig>,
) -> Result<ModelTarget, String> {
    let mut target = state
        .core
        .store
        .target(&id)
        .await
        .map_err(err)?
        .ok_or("model not found")?;
    let config = config.map(|value| value.normalized(target.kind));
    if let Some(config) = config.as_ref() {
        let draft = match config.draft_target_id.as_deref() {
            Some(draft_id) => Some(
                state
                    .core
                    .store
                    .target(draft_id)
                    .await
                    .map_err(err)?
                    .ok_or("draft model is no longer in the library")?,
            ),
            None => None,
        };
        speculative::validate(&target, config, draft.as_ref()).map_err(err)?;
    }
    target.local.speculative_config = config;
    state.core.store.upsert_target(&target).await.map_err(err)?;
    state.runtimes.mark_target_pending_restart(&id);
    Ok(target)
}

#[tauri::command]
pub async fn clear_kv_cache(
    state: State<'_, AppServices>,
    target_id: Option<String>,
) -> Result<(), String> {
    if let Some(id) = target_id.as_deref() {
        let target = state
            .core
            .store
            .target(id)
            .await
            .map_err(err)?
            .ok_or("model not found")?;
        if !matches!(target.kind, TargetKind::Gguf | TargetKind::Mlx) {
            return Err("disk KV is only available for local chat models".into());
        }
    }
    state
        .runtimes
        .clear_kv_cache(target_id.as_deref())
        .await
        .map_err(err)
}

fn csv_cell(mut value: String) -> String {
    if value.starts_with(['=', '+', '-', '@']) {
        value.insert(0, '\'');
    }
    format!("\"{}\"", value.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::{csv_cell, logs_csv, prepare_routing_policy, prune_route_policy};
    use crate::{
        domain::{ModelRoute, RouteRole, RouteTarget, TargetKind},
        routing::{builtin_tasks, PolicyStatus, RoutingMode, RoutingPolicy},
        storage::{LogQuery, RequestLog, Store},
    };
    use chrono::Utc;

    fn sample_route() -> ModelRoute {
        ModelRoute {
            alias: "assistant".into(),
            enabled: true,
            capabilities: vec!["chat".into()],
            targets: vec![RouteTarget {
                id: "cloud".into(),
                kind: TargetKind::Cloud,
                model: "coding".into(),
                priority: 10,
                enabled: true,
                ..Default::default()
            }],
        }
    }

    fn known_tasks() -> Vec<String> {
        builtin_tasks().into_iter().map(|task| task.id).collect()
    }

    #[test]
    fn csv_export_neutralizes_spreadsheet_formulas() {
        assert_eq!(
            csv_cell("=HYPERLINK(\"bad\")".into()),
            "\"'=HYPERLINK(\"\"bad\"\")\""
        );
    }

    #[tokio::test]
    async fn csv_export_uses_filters_and_is_not_limited_to_one_page() {
        let store = Store::memory().await.unwrap();
        for index in 0..501 {
            store
                .insert_log(&RequestLog {
                    id: format!("kept-{index}"),
                    created_at: Utc::now(),
                    endpoint: "/v1/chat/completions".into(),
                    alias: Some("keep".into()),
                    target: Some("cloud".into()),
                    attempts: 1,
                    status: 200,
                    latency_ms: 5,
                    input_tokens: Some(1),
                    output_tokens: Some(1),
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    error_code: None,
                    error_message: None,
                    api_key_id: None,
                    api_key_name: None,
                })
                .await
                .unwrap();
        }
        store
            .insert_log(&RequestLog {
                id: "excluded".into(),
                created_at: Utc::now(),
                endpoint: "/v1/embeddings".into(),
                alias: Some("drop".into()),
                target: Some("cloud".into()),
                attempts: 1,
                status: 500,
                latency_ms: 5,
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                error_code: None,
                error_message: None,
                api_key_id: None,
                api_key_name: None,
            })
            .await
            .unwrap();

        let csv = logs_csv(
            &store,
            LogQuery {
                alias: Some("keep".into()),
                ..LogQuery::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(csv.lines().count(), 502);
        assert!(!csv.contains("excluded"));
    }

    #[test]
    fn save_routing_policy_drops_fallback_candidate_ids() {
        let mut route = sample_route();
        route.targets.push(RouteTarget {
            id: "reserve".into(),
            kind: TargetKind::Cloud,
            model: "vision".into(),
            priority: 20,
            enabled: true,
            role: RouteRole::Fallback,
        });
        let mut policy = RoutingPolicy::new("assistant");
        policy.mode = RoutingMode::Adaptive;
        policy.status = PolicyStatus::Active;
        policy.candidate_target_ids = vec!["cloud".into(), "reserve".into()];

        let saved = prepare_routing_policy(policy, &route, &known_tasks()).unwrap();

        assert_eq!(saved.candidate_target_ids, vec!["cloud".to_string()]);
    }

    #[test]
    fn save_routing_policy_drops_candidates_that_are_not_alias_hops() {
        let mut policy = RoutingPolicy::new("assistant");
        policy.mode = RoutingMode::Adaptive;
        policy.status = PolicyStatus::Active;
        policy.candidate_target_ids = vec![
            "cloud".into(),
            "dae9cea9-c842-4a88-9d23-e0562d2d7646".into(),
        ];

        let saved = prepare_routing_policy(policy, &sample_route(), &known_tasks()).unwrap();

        assert_eq!(saved.candidate_target_ids, vec!["cloud".to_string()]);
    }

    #[test]
    fn adaptive_policy_without_remaining_hops_is_rejected() {
        let mut policy = RoutingPolicy::new("assistant");
        policy.mode = RoutingMode::Adaptive;
        policy.status = PolicyStatus::Active;
        policy.candidate_target_ids = vec!["dae9cea9-c842-4a88-9d23-e0562d2d7646".into()];

        let error = prepare_routing_policy(policy, &sample_route(), &known_tasks()).unwrap_err();

        assert!(error.contains("at least one candidate target"));
    }

    #[tokio::test]
    async fn save_route_prunes_policy_candidates_to_remaining_hops() {
        let store = Store::memory().await.unwrap();
        let mut route = sample_route();
        route.targets.push(RouteTarget {
            id: "dae9cea9-c842-4a88-9d23-e0562d2d7646".into(),
            kind: TargetKind::Cloud,
            model: "stale".into(),
            priority: 20,
            enabled: true,
            ..Default::default()
        });
        store.upsert_route(&route).await.unwrap();
        let mut policy = RoutingPolicy::new("assistant");
        policy.mode = RoutingMode::Adaptive;
        policy.status = PolicyStatus::Active;
        policy.candidate_target_ids = vec![
            "cloud".into(),
            "dae9cea9-c842-4a88-9d23-e0562d2d7646".into(),
        ];
        store.upsert_routing_policy(&policy).await.unwrap();

        route.targets.pop();
        prune_route_policy(&store, &route).await.unwrap();

        let stored = store.routing_policy("assistant").await.unwrap().unwrap();
        assert_eq!(stored.candidate_target_ids, vec!["cloud".to_string()]);
    }
}

#[tauri::command]
pub fn save_hugging_face_token(state: State<'_, AppServices>, token: String) -> Result<(), String> {
    if token.trim().is_empty() {
        state.core.secrets.delete(HF_ACCOUNT).map_err(err)
    } else {
        state
            .core
            .secrets
            .set(HF_ACCOUNT, token.trim())
            .map_err(err)
    }
}

#[tauri::command]
pub fn save_civitai_token(state: State<'_, AppServices>, token: String) -> Result<(), String> {
    if token.trim().is_empty() {
        state.core.secrets.delete(CIVITAI_ACCOUNT).map_err(err)
    } else {
        state
            .core
            .secrets
            .set(CIVITAI_ACCOUNT, token.trim())
            .map_err(err)
    }
}

#[tauri::command]
pub async fn forget_all_credentials(state: State<'_, AppServices>) -> Result<(), String> {
    for provider in state.core.store.providers().await.map_err(err)? {
        state
            .core
            .secrets
            .delete(&provider_account(&provider.id))
            .map_err(err)?;
    }
    for key in state.core.store.local_api_keys().await.map_err(err)? {
        state
            .core
            .secrets
            .delete(&local_api_key_account(&key.id))
            .map_err(err)?;
        if key.revoked_at.is_none() {
            state
                .core
                .store
                .revoke_local_api_key(&key.id)
                .await
                .map_err(err)?;
        }
    }
    state.core.secrets.delete(HF_ACCOUNT).map_err(err)?;
    state.core.secrets.delete(CIVITAI_ACCOUNT).map_err(err)?;
    state.core.secrets.delete(LOCAL_API_KEY).map_err(err)
}

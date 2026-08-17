use std::{path::PathBuf, sync::Arc};

use serde::{Deserialize, Serialize};
use tauri::State;
use uuid::Uuid;

use crate::{
    core::AppCore,
    domain::{ModelRoute, TargetKind},
    library,
    runtime::{RuntimeManager, RuntimeStatus},
    secrets::{provider_account, HF_ACCOUNT, LOCAL_API_KEY},
    storage::{ModelTarget, Provider, RequestLog},
};

pub struct AppServices {
    pub core: Arc<AppCore>,
    pub runtimes: Arc<RuntimeManager>,
    pub model_library: PathBuf,
    pub port: u16,
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
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveProviderInput {
    pub id: Option<String>,
    pub name: String,
    pub kind: TargetKind,
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
}

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[tauri::command]
pub async fn dashboard(state: State<'_, AppServices>) -> Result<Dashboard, String> {
    let providers = state.core.providers_with_credentials().await.map_err(err)?;
    let targets = state.core.store.targets().await.map_err(err)?;
    let routes = state.core.store.routes().await.map_err(err)?;
    let recent_requests = state.core.store.logs(1000).await.map_err(err)?.len();
    Ok(Dashboard {
        running: true,
        base_url: format!("http://127.0.0.1:{}/v1", state.port),
        provider_count: providers.len(),
        target_count: targets.len(),
        route_count: routes.len(),
        recent_requests,
        runtimes: state.runtimes.statuses(),
    })
}

#[tauri::command]
pub fn get_local_api_key(state: State<'_, AppServices>) -> Result<String, String> {
    state.core.ensure_local_token().map_err(err)
}

#[tauri::command]
pub fn rotate_local_api_key(state: State<'_, AppServices>) -> Result<String, String> {
    state.core.rotate_local_token().map_err(err)
}

#[tauri::command]
pub async fn list_providers(state: State<'_, AppServices>) -> Result<Vec<Provider>, String> {
    state.core.providers_with_credentials().await.map_err(err)
}

#[tauri::command]
pub async fn save_provider(
    state: State<'_, AppServices>,
    input: SaveProviderInput,
) -> Result<Provider, String> {
    if !matches!(input.kind, TargetKind::OpenAi | TargetKind::OpenRouter) {
        return Err("provider must be OpenAI or OpenRouter".into());
    }
    let id = input.id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let provider = Provider {
        id: id.clone(),
        name: input.name,
        kind: input.kind,
        base_url: input.base_url.trim_end_matches('/').to_owned(),
        enabled: input.enabled,
        has_credential: input.api_key.as_ref().is_some_and(|key| !key.is_empty()),
    };
    if let Some(key) = input.api_key.filter(|key| !key.trim().is_empty()) {
        state
            .core
            .validate_provider(&provider, &key)
            .await
            .map_err(err)?;
        state
            .core
            .secrets
            .set(&provider_account(&id), key.trim())
            .map_err(err)?;
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
) -> Result<Vec<String>, String> {
    let provider = state
        .core
        .store
        .provider(&id)
        .await
        .map_err(err)?
        .ok_or("provider not found")?;
    let credential = state
        .core
        .secrets
        .get(&provider_account(&id))
        .map_err(err)?
        .ok_or("provider credential missing")?;
    let models = state
        .core
        .validate_provider(&provider, &credential)
        .await
        .map_err(err)?;
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
) -> Result<Vec<String>, String> {
    state.core.store.provider_models(&id).await.map_err(err)
}

#[tauri::command]
pub async fn list_targets(state: State<'_, AppServices>) -> Result<Vec<ModelTarget>, String> {
    state.core.store.targets().await.map_err(err)
}

#[tauri::command]
pub async fn save_target(
    state: State<'_, AppServices>,
    target: ModelTarget,
) -> Result<ModelTarget, String> {
    state.core.store.upsert_target(&target).await.map_err(err)?;
    Ok(target)
}

#[tauri::command]
pub async fn delete_target(state: State<'_, AppServices>, id: String) -> Result<(), String> {
    state.runtimes.stop(&id).await.map_err(err)?;
    state.core.store.delete_target(&id).await.map_err(err)
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
        capabilities: input.capabilities,
        enabled: true,
        state: "stopped".into(),
        size_bytes: Some(imported.size_bytes as i64),
    };
    state.core.store.upsert_target(&target).await.map_err(err)?;
    Ok(target)
}

#[tauri::command]
pub async fn download_local_model(
    state: State<'_, AppServices>,
    input: DownloadModelInput,
) -> Result<ModelTarget, String> {
    let imported = library::download_hugging_face(
        &state.core.client,
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
        capabilities: input.capabilities,
        enabled: true,
        state: "stopped".into(),
        size_bytes: Some(imported.size_bytes as i64),
    };
    state.core.store.upsert_target(&target).await.map_err(err)?;
    Ok(target)
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
    let runtime_url = state.runtimes.start(&target).await.map_err(err)?;
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
pub async fn save_route(
    state: State<'_, AppServices>,
    route: ModelRoute,
) -> Result<ModelRoute, String> {
    if route.alias.trim().is_empty() || route.alias.contains(char::is_whitespace) {
        return Err("alias must be non-empty and contain no whitespace".into());
    }
    state.core.store.upsert_route(&route).await.map_err(err)?;
    Ok(route)
}

#[tauri::command]
pub async fn delete_route(state: State<'_, AppServices>, alias: String) -> Result<(), String> {
    state.core.store.delete_route(&alias).await.map_err(err)
}

#[tauri::command]
pub async fn list_logs(
    state: State<'_, AppServices>,
    limit: Option<i64>,
) -> Result<Vec<RequestLog>, String> {
    state
        .core
        .store
        .logs(limit.unwrap_or(250))
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn clear_logs(state: State<'_, AppServices>) -> Result<(), String> {
    state.core.store.clear_logs().await.map_err(err)
}

#[tauri::command]
pub async fn export_logs_csv(state: State<'_, AppServices>, path: String) -> Result<(), String> {
    let logs = state.core.store.logs(1000).await.map_err(err)?;
    let mut csv = String::from("id,created_at,endpoint,alias,target,attempts,status,latency_ms,input_tokens,output_tokens,error_code\n");
    for log in logs {
        let values = [
            log.id,
            log.created_at.to_rfc3339(),
            log.endpoint,
            log.alias.unwrap_or_default(),
            log.target.unwrap_or_default(),
            log.attempts.to_string(),
            log.status.to_string(),
            log.latency_ms.to_string(),
            log.input_tokens.map(|v| v.to_string()).unwrap_or_default(),
            log.output_tokens.map(|v| v.to_string()).unwrap_or_default(),
            log.error_code.unwrap_or_default(),
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
    tokio::fs::write(path, csv).await.map_err(err)
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

fn csv_cell(mut value: String) -> String {
    if value.starts_with(['=', '+', '-', '@']) {
        value.insert(0, '\'');
    }
    format!("\"{}\"", value.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::csv_cell;

    #[test]
    fn csv_export_neutralizes_spreadsheet_formulas() {
        assert_eq!(
            csv_cell("=HYPERLINK(\"bad\")".into()),
            "\"'=HYPERLINK(\"\"bad\"\")\""
        );
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
pub async fn forget_all_credentials(state: State<'_, AppServices>) -> Result<(), String> {
    for provider in state.core.store.providers().await.map_err(err)? {
        state
            .core
            .secrets
            .delete(&provider_account(&provider.id))
            .map_err(err)?;
    }
    state.core.secrets.delete(HF_ACCOUNT).map_err(err)?;
    state.core.secrets.delete(LOCAL_API_KEY).map_err(err)
}

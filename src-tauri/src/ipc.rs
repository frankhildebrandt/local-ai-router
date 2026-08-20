use tauri::State;

use crate::commands::{
    self, AppServices, ClientChatInput, ClientChatResponse, Dashboard, DownloadModelInput,
    ImportModelInput, InspectModelInput, InstallCatalogInput, LocalApiKeyWithToken,
    RoutingImportPreview, RoutingSimulationInput, SaveProviderInput, SearchCatalogInput,
};
use crate::domain::ModelRoute;
use crate::model_catalog::ModelMetadata;
use crate::providers::ProviderPreset;
use crate::public_models::PublicModel;
use crate::resource::{ResourceOverrides, ResourcePolicy, ResourceProfile};
use crate::routing::{
    RoutingAttemptRecord, RoutingConfigExport, RoutingEvaluation, RoutingPolicy,
    RoutingTaskDefinition, TargetRoutingProfile,
};
use crate::speculative::SpeculativeConfig;
use crate::storage::{
    InstallJob, KeyUsageData, LocalApiKey, LogFacets, LogQuery, LogResult, ModelTarget, Provider,
    ProviderModel, UsageData,
};

macro_rules! wrap_async {
    ($name:ident ( $($arg:ident : $ty:ty),* ) -> $ret:ty) => {
        #[tauri::command]
        pub async fn $name(state: State<'_, AppServices>, $($arg: $ty),*) -> Result<$ret, String> {
            crate::commands::$name(&state, $($arg),*).await
        }
    };
}

macro_rules! wrap_sync {
    ($name:ident ( $($arg:ident : $ty:ty),* ) -> $ret:ty) => {
        #[tauri::command]
        pub fn $name(state: State<'_, AppServices>, $($arg: $ty),*) -> Result<$ret, String> {
            crate::commands::$name(&state, $($arg),*)
        }
    };
}

wrap_async!(dashboard() -> Dashboard);
wrap_async!(cancel_inflight_request(id: String) -> ());
wrap_async!(cancel_all_inflight_requests() -> ());
wrap_async!(list_local_api_keys() -> Vec<LocalApiKey>);
wrap_async!(create_local_api_key(name: String) -> LocalApiKeyWithToken);
wrap_async!(reveal_local_api_key(id: String) -> String);
wrap_async!(rename_local_api_key(id: String, name: String) -> LocalApiKey);
wrap_async!(rotate_local_api_key(id: String) -> String);
wrap_async!(revoke_local_api_key(id: String) -> ());
wrap_async!(client_chat(input: ClientChatInput) -> ClientChatResponse);
wrap_async!(list_providers() -> Vec<Provider>);
wrap_async!(save_provider(input: SaveProviderInput) -> Provider);
wrap_async!(test_provider_connection(id: String) -> Vec<String>);
wrap_async!(begin_openai_subscription(id: String) -> crate::oauth::OAuthStart);
wrap_async!(openai_subscription_status(id: String) -> crate::oauth::OAuthStatus);
wrap_async!(logout_openai_subscription(id: String) -> ());
wrap_async!(delete_provider(id: String) -> ());
wrap_async!(sync_provider_models(id: String) -> Vec<ProviderModel>);
wrap_async!(cached_provider_models(id: String) -> Vec<ProviderModel>);
wrap_async!(list_local_catalog() -> crate::catalog::LocalCatalog);
wrap_async!(search_mlx_catalog(input: SearchCatalogInput) -> crate::hub::SearchPage);
wrap_async!(inspect_mlx_model(input: InspectModelInput) -> crate::hub::ModelInspection);
wrap_async!(install_catalog_model(input: InstallCatalogInput) -> InstallJob);
wrap_async!(list_install_jobs() -> Vec<InstallJob>);
wrap_async!(pause_install_job(id: String) -> InstallJob);
wrap_async!(resume_install_job(id: String) -> InstallJob);
wrap_async!(cancel_install_job(id: String) -> InstallJob);
wrap_async!(clear_install_job(id: String) -> ());
wrap_async!(list_targets() -> Vec<ModelTarget>);
wrap_async!(save_target(target: ModelTarget) -> ModelTarget);
wrap_async!(delete_target(id: String) -> ());
wrap_async!(import_local_model(input: ImportModelInput) -> ModelTarget);
wrap_async!(download_local_model(input: DownloadModelInput) -> ModelTarget);
wrap_async!(start_local_model(id: String) -> ModelTarget);
wrap_async!(stop_local_model(id: String) -> ModelTarget);
wrap_async!(list_routes() -> Vec<ModelRoute>);
wrap_async!(list_public_models() -> Vec<PublicModel>);
wrap_async!(save_route(route: ModelRoute) -> ModelRoute);
wrap_async!(delete_route(alias: String) -> ());
wrap_async!(list_routing_policies() -> Vec<RoutingPolicy>);
wrap_async!(save_routing_policy(policy: RoutingPolicy) -> RoutingPolicy);
wrap_async!(list_target_routing_profiles() -> Vec<TargetRoutingProfile>);
wrap_async!(save_target_routing_profile(profile: TargetRoutingProfile) -> TargetRoutingProfile);
wrap_async!(list_routing_tasks() -> Vec<RoutingTaskDefinition>);
wrap_async!(save_routing_task(task: RoutingTaskDefinition) -> RoutingTaskDefinition);
wrap_async!(delete_routing_task(id: String) -> ());
wrap_async!(simulate_routing(input: RoutingSimulationInput) -> RoutingEvaluation);
wrap_async!(list_routing_attempts(request_id: Option<String>, limit: Option<i64>) -> Vec<RoutingAttemptRecord>);
wrap_async!(export_routing_config() -> RoutingConfigExport);
wrap_async!(import_routing_config(config: RoutingConfigExport, apply: bool) -> RoutingImportPreview);
wrap_async!(list_logs(query: Option<LogQuery>) -> LogResult);
wrap_async!(get_usage(period: String, target: Option<String>) -> UsageData);
wrap_async!(get_key_usage(id: String, period: String) -> KeyUsageData);
wrap_async!(get_log_facets() -> LogFacets);
wrap_async!(clear_logs() -> ());
wrap_async!(export_logs_csv(path: Option<String>, query: Option<LogQuery>) -> String);
wrap_async!(get_settings() -> std::collections::HashMap<String, String>);
wrap_async!(save_setting(key: String, value: String) -> ());
wrap_async!(get_resource_policy() -> ResourcePolicy);
wrap_async!(save_resource_policy(policy: ResourcePolicy) -> ());
wrap_async!(save_model_resource_overrides(id: String, overrides: Option<ResourceOverrides>, force_tool_support: Option<bool>) -> ModelTarget);
wrap_async!(save_model_speculative_config(id: String, config: Option<SpeculativeConfig>) -> ModelTarget);
wrap_async!(clear_kv_cache(target_id: Option<String>) -> ());
wrap_async!(forget_all_credentials() -> ());
wrap_async!(auth_status() -> crate::commands::AuthStatus);
wrap_async!(login(username: String, password: String) -> crate::identity::DirectoryUser);
wrap_async!(list_directory_users() -> Vec<crate::identity::DirectoryUser>);
wrap_async!(create_directory_user(input: crate::identity::CreateUserInput) -> crate::identity::DirectoryUser);
wrap_async!(update_directory_user(id: String, input: crate::identity::UpdateUserInput) -> crate::identity::DirectoryUser);
wrap_async!(list_directory_groups() -> Vec<crate::identity::DirectoryGroup>);
wrap_async!(save_directory_group(id: Option<String>, input: crate::identity::UpsertGroupInput) -> crate::identity::DirectoryGroup);
wrap_async!(delete_directory_group(id: String) -> ());
wrap_async!(user_permissions(id: String) -> crate::identity::EffectivePermissions);
wrap_async!(join_uplink(input: crate::uplink::JoinUplinkInput) -> crate::uplink::UplinkParent);
wrap_async!(uplink_status() -> Option<crate::uplink::UplinkParent>);
wrap_async!(disconnect_uplink() -> ());
wrap_async!(reveal_operator_bootstrap() -> Option<String>);
wrap_async!(list_oidc_allowlist() -> Vec<crate::identity::OidcAllowlistEntry>);
wrap_async!(invite_oidc_identity(provider: String, identifier: String, user_id: Option<String>) -> crate::identity::OidcAllowlistEntry);
wrap_async!(delete_oidc_allowlist(id: String) -> ());
wrap_async!(save_oidc_client(provider: String, client_id: String, client_secret: String) -> ());

#[tauri::command]
pub async fn begin_oidc_login(
    state: State<'_, AppServices>,
    provider: String,
) -> Result<crate::oidc::OidcStart, String> {
    let scheme = if state.tls_required { "https" } else { "http" };
    let redirect_uri = format!("{scheme}://127.0.0.1:{}/auth/oidc/callback", state.port);
    crate::commands::begin_oidc_login(&state, provider, redirect_uri).await
}
wrap_sync!(save_hugging_face_token(token: String) -> ());
wrap_sync!(save_civitai_token(token: String) -> ());

#[tauri::command]
pub fn list_provider_presets() -> Vec<ProviderPreset> {
    commands::list_provider_presets()
}

#[tauri::command]
pub fn get_resource_profile_preset(profile: ResourceProfile) -> Result<ResourcePolicy, String> {
    commands::get_resource_profile_preset(profile)
}

#[tauri::command]
pub async fn lookup_model_metadata(model: String) -> Result<ModelMetadata, String> {
    commands::lookup_model_metadata(model).await
}

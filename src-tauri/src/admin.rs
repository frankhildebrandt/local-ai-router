use std::{path::PathBuf, sync::Arc};

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode, Uri},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Redirect, Response,
    },
    routing::{get, post},
    Json, Router,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::{
    commands::{self, AppServices},
    gateway,
    identity::{self, DirectoryUser},
};

#[derive(Clone)]
struct AdminState {
    services: Arc<AppServices>,
}

pub fn router(services: Arc<AppServices>) -> Router {
    Router::new()
        .route("/admin/events", get(events))
        .route("/admin/{name}", post(invoke))
        .route("/auth/oidc/callback", get(oidc_callback))
        .with_state(AdminState { services })
}

pub async fn fallback(uri: Uri, ui_dir: Option<PathBuf>) -> Response {
    let path = uri.path();
    if path.starts_with("/v1") || path.starts_with("/v1beta") {
        return gateway::not_found().await.into_response();
    }
    let Some(root) = ui_dir else {
        return (
            StatusCode::NOT_FOUND,
            "Admin UI is not available. Pass --ui-dir or build the frontend into dist/.",
        )
            .into_response();
    };
    let relative = path.trim_start_matches('/');
    let candidate = if relative.is_empty() {
        root.join("index.html")
    } else {
        root.join(relative)
    };
    let file = if is_safe_file(&root, &candidate) {
        candidate
    } else {
        root.join("index.html")
    };
    match tokio::fs::read(&file).await {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, content_type(&file))
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Err(_) => match tokio::fs::read(root.join("index.html")).await {
            Ok(bytes) => Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                .body(Body::from(bytes))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
            Err(_) => StatusCode::NOT_FOUND.into_response(),
        },
    }
}

fn is_safe_file(root: &std::path::Path, candidate: &std::path::Path) -> bool {
    let Ok(root) = root.canonicalize() else {
        return false;
    };
    let Ok(path) = candidate.canonicalize() else {
        return false;
    };
    path.starts_with(root) && path.is_file()
}

fn content_type(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("js") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("woff2") => "font/woff2",
        Some("json") => "application/json",
        Some("html") => "text/html; charset=utf-8",
        _ => "application/octet-stream",
    }
}

async fn events(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    let request = match load_admin_request(&state, &headers).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    if let Some(response) = deny_if_needed(&state, &request, "events").await {
        return response;
    }
    let mut install = state.services.install.subscribe();
    let traffic = state.services.core.traffic.clone();
    let mut traffic_events = traffic.subscribe();
    let stream = async_stream::stream! {
        loop {
            tokio::select! {
                result = install.recv() => {
                    match result {
                        Ok(event) => {
                            if let Ok(item) = Event::default().event("install-job").json_data(event) {
                                yield Ok::<Event, std::convert::Infallible>(item);
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                result = traffic_events.recv() => {
                    match result {
                        Ok(event) => {
                            if let Ok(item) = Event::default().event("gateway-traffic").json_data(event) {
                                yield Ok::<Event, std::convert::Infallible>(item);
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            if let Ok(item) = Event::default()
                                .event("gateway-traffic")
                                .json_data(traffic.snapshot())
                            {
                                yield Ok::<Event, std::convert::Infallible>(item);
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    };
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

async fn invoke(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    body: bytes::Bytes,
) -> Response {
    let args = if body.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice::<Value>(&body) {
            Ok(value) => snake_case_top_level(value),
            Err(error) => return (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
        }
    };
    let request = match load_admin_request(&state, &headers).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    if let Some(response) = deny_if_needed(&state, &request, &name).await {
        return response;
    }
    if name == "login" {
        return login_response(&state, &request, &args).await;
    }
    if name == "logout" {
        if let Some(token) = &request.token {
            let _ = commands::logout(&state.services, Some(token)).await;
        }
        return cookie_json(Value::Null, identity::clear_cookie_header(request.secure));
    }
    if name == "auth_status" {
        return match commands::auth_status_for(&state.services, request.login_required, request.user)
            .await
        {
            Ok(value) => ok_response(value),
            Err(error) => (StatusCode::BAD_REQUEST, error).into_response(),
        };
    }
    if name == "begin_oidc_login" {
        let provider = match field::<String>(&args, "provider") {
            Ok(value) => value,
            Err(error) => return (StatusCode::BAD_REQUEST, error).into_response(),
        };
        let origin = request_origin(&headers, request.secure, state.services.port);
        let redirect = format!("{origin}/auth/oidc/callback");
        return match commands::begin_oidc_login(&state.services, provider, redirect).await {
            Ok(value) => ok_response(value),
            Err(error) => (StatusCode::BAD_REQUEST, error).into_response(),
        };
    }
    match dispatch(&state.services, &name, args).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => {
            let status = if error == "unknown admin command" {
                StatusCode::NOT_FOUND
            } else if error == "login required" {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::BAD_REQUEST
            };
            (status, error).into_response()
        }
    }
}

struct AdminRequest {
    login_required: bool,
    secure: bool,
    user: Option<DirectoryUser>,
    token: Option<String>,
}

async fn load_admin_request(
    state: &AdminState,
    headers: &HeaderMap,
) -> Result<AdminRequest, Response> {
    let login_required = state.services.tls_required;
    let token = identity::parse_session_cookie(
        headers
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok()),
    );
    let user = if let Some(token) = token.as_deref() {
        identity::user_for_session(&state.services.core.store, token)
            .await
            .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()).into_response())?
            .filter(|user| user.disabled_at.is_none())
    } else {
        None
    };
    Ok(AdminRequest {
        login_required,
        secure: state.services.tls_required,
        user,
        token,
    })
}

async fn deny_if_needed(state: &AdminState, request: &AdminRequest, name: &str) -> Option<Response> {
    if is_public_command(name) {
        return None;
    }
    if !request.login_required {
        return None;
    }
    let Some(user) = &request.user else {
        return Some((StatusCode::UNAUTHORIZED, "login required").into_response());
    };
    if is_admin_command(name) {
        let Ok(permissions) = state.services.core.store.permissions_for(user).await else {
            return Some((StatusCode::FORBIDDEN, "admin permission required").into_response());
        };
        if !permissions.may_admin {
            return Some((StatusCode::FORBIDDEN, "admin permission required").into_response());
        }
    }
    None
}

fn is_public_command(name: &str) -> bool {
    matches!(
        name,
        "auth_status" | "login" | "logout" | "begin_oidc_login"
    )
}

fn is_admin_command(name: &str) -> bool {
    matches!(
        name,
        "create_directory_user"
            | "update_directory_user"
            | "save_directory_group"
            | "delete_directory_group"
            | "invite_oidc_identity"
            | "delete_oidc_allowlist"
            | "save_oidc_client"
            | "reveal_operator_bootstrap"
            | "save_setting"
            | "forget_all_credentials"
    )
}

async fn login_response(state: &AdminState, request: &AdminRequest, args: &Value) -> Response {
    let username = match field::<String>(args, "username") {
        Ok(value) => value,
        Err(error) => return (StatusCode::BAD_REQUEST, error).into_response(),
    };
    let password = match field::<String>(args, "password") {
        Ok(value) => value,
        Err(error) => return (StatusCode::BAD_REQUEST, error).into_response(),
    };
    match commands::login_with_session(&state.services, username, password).await {
        Ok((user, token)) => cookie_json(
            serde_json::to_value(&user).unwrap_or(Value::Null),
            identity::set_cookie_header(&token, request.secure),
        ),
        Err(error) => (StatusCode::UNAUTHORIZED, error).into_response(),
    }
}

fn cookie_json(value: Value, cookie: String) -> Response {
    (
        [(header::SET_COOKIE, cookie)],
        Json(value),
    )
        .into_response()
}

fn ok_response<T: serde::Serialize>(value: T) -> Response {
    match serde_json::to_value(value) {
        Ok(value) => Json(value).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

fn request_origin(headers: &HeaderMap, secure: bool, port: u16) -> String {
    if let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
    {
        let scheme = if secure { "https" } else { "http" };
        return format!("{scheme}://{host}");
    }
    if let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
    {
        return origin.trim_end_matches('/').to_owned();
    }
    format!(
        "{}://127.0.0.1:{port}",
        if secure { "https" } else { "http" }
    )
}

#[derive(Debug, serde::Deserialize)]
struct OidcCallback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn oidc_callback(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Query(query): Query<OidcCallback>,
) -> Response {
    let request = match load_admin_request(&state, &headers).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    let origin = request_origin(&headers, request.secure, state.services.port);
    if let Some(error) = query.error {
        return Redirect::to(&format!("{origin}/?oidc_error={error}")).into_response();
    }
    let (Some(code), Some(oidc_state)) = (query.code, query.state) else {
        return Redirect::to(&format!("{origin}/?oidc_error=missing_code")).into_response();
    };
    match commands::finish_oidc_login(&state.services, code, oidc_state).await {
        Ok((_, token)) => {
            let cookie = identity::set_cookie_header(&token, request.secure);
            Response::builder()
                .status(StatusCode::SEE_OTHER)
                .header(header::LOCATION, format!("{origin}/"))
                .header(header::SET_COOKIE, cookie)
                .body(Body::empty())
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        Err(error) => {
            let encoded = urlencoding_lite(&error);
            Redirect::to(&format!("{origin}/?oidc_error={encoded}")).into_response()
        }
    }
}

fn urlencoding_lite(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

async fn dispatch(services: &AppServices, name: &str, args: Value) -> Result<Value, String> {
    match name {
        "dashboard" => ok(commands::dashboard(services).await?),
        "cancel_inflight_request" => {
            commands::cancel_inflight_request(services, field(&args, "id")?).await?;
            Ok(Value::Null)
        }
        "cancel_all_inflight_requests" => {
            commands::cancel_all_inflight_requests(services).await?;
            Ok(Value::Null)
        }
        "list_local_api_keys" => ok(commands::list_local_api_keys(services).await?),
        "create_local_api_key" => {
            ok(commands::create_local_api_key(services, field(&args, "name")?).await?)
        }
        "reveal_local_api_key" => {
            ok(commands::reveal_local_api_key(services, field(&args, "id")?).await?)
        }
        "rename_local_api_key" => ok(commands::rename_local_api_key(
            services,
            field(&args, "id")?,
            field(&args, "name")?,
        )
        .await?),
        "rotate_local_api_key" => {
            ok(commands::rotate_local_api_key(services, field(&args, "id")?).await?)
        }
        "revoke_local_api_key" => {
            commands::revoke_local_api_key(services, field(&args, "id")?).await?;
            Ok(Value::Null)
        }
        "client_chat" => ok(commands::client_chat(services, field(&args, "input")?).await?),
        "list_providers" => ok(commands::list_providers(services).await?),
        "list_provider_presets" => ok(commands::list_provider_presets()),
        "save_provider" => ok(commands::save_provider(services, field(&args, "input")?).await?),
        "delete_provider" => {
            commands::delete_provider(services, field(&args, "id")?).await?;
            Ok(Value::Null)
        }
        "sync_provider_models" => {
            ok(commands::sync_provider_models(services, field(&args, "id")?).await?)
        }
        "cached_provider_models" => {
            ok(commands::cached_provider_models(services, field(&args, "id")?).await?)
        }
        "begin_openai_subscription" => {
            ok(commands::begin_openai_subscription(services, field(&args, "id")?).await?)
        }
        "openai_subscription_status" => {
            ok(commands::openai_subscription_status(services, field(&args, "id")?).await?)
        }
        "logout_openai_subscription" => {
            commands::logout_openai_subscription(services, field(&args, "id")?).await?;
            Ok(Value::Null)
        }
        "test_provider_connection" => {
            ok(commands::test_provider_connection(services, field(&args, "id")?).await?)
        }
        "list_targets" => ok(commands::list_targets(services).await?),
        "save_target" => ok(commands::save_target(services, field(&args, "target")?).await?),
        "lookup_model_metadata" => {
            ok(commands::lookup_model_metadata(field(&args, "model")?).await?)
        }
        "delete_target" => {
            commands::delete_target(services, field(&args, "id")?).await?;
            Ok(Value::Null)
        }
        "import_local_model" => {
            ok(commands::import_local_model(services, field(&args, "input")?).await?)
        }
        "download_local_model" => {
            ok(commands::download_local_model(services, field(&args, "input")?).await?)
        }
        "list_local_catalog" => ok(commands::list_local_catalog(services).await?),
        "search_mlx_catalog" => {
            ok(commands::search_mlx_catalog(services, field(&args, "input")?).await?)
        }
        "inspect_mlx_model" => {
            ok(commands::inspect_mlx_model(services, field(&args, "input")?).await?)
        }
        "install_catalog_model" => {
            ok(commands::install_catalog_model(services, field(&args, "input")?).await?)
        }
        "list_install_jobs" => ok(commands::list_install_jobs(services).await?),
        "pause_install_job" => {
            ok(commands::pause_install_job(services, field(&args, "id")?).await?)
        }
        "resume_install_job" => {
            ok(commands::resume_install_job(services, field(&args, "id")?).await?)
        }
        "cancel_install_job" => {
            ok(commands::cancel_install_job(services, field(&args, "id")?).await?)
        }
        "clear_install_job" => {
            commands::clear_install_job(services, field(&args, "id")?).await?;
            Ok(Value::Null)
        }
        "start_local_model" => {
            ok(commands::start_local_model(services, field(&args, "id")?).await?)
        }
        "stop_local_model" => ok(commands::stop_local_model(services, field(&args, "id")?).await?),
        "list_routes" => ok(commands::list_routes(services).await?),
        "list_public_models" => ok(commands::list_public_models(services).await?),
        "save_route" => ok(commands::save_route(services, field(&args, "route")?).await?),
        "delete_route" => {
            commands::delete_route(services, field(&args, "alias")?).await?;
            Ok(Value::Null)
        }
        "list_routing_policies" => ok(commands::list_routing_policies(services).await?),
        "save_routing_policy" => {
            ok(commands::save_routing_policy(services, field(&args, "policy")?).await?)
        }
        "list_target_routing_profiles" => {
            ok(commands::list_target_routing_profiles(services).await?)
        }
        "save_target_routing_profile" => {
            ok(commands::save_target_routing_profile(services, field(&args, "profile")?).await?)
        }
        "list_routing_tasks" => ok(commands::list_routing_tasks(services).await?),
        "save_routing_task" => {
            ok(commands::save_routing_task(services, field(&args, "task")?).await?)
        }
        "delete_routing_task" => {
            commands::delete_routing_task(services, field(&args, "id")?).await?;
            Ok(Value::Null)
        }
        "simulate_routing" => {
            ok(commands::simulate_routing(services, field(&args, "input")?).await?)
        }
        "list_routing_attempts" => ok(commands::list_routing_attempts(
            services,
            optional(&args, "request_id")?,
            optional(&args, "limit")?,
        )
        .await?),
        "export_routing_config" => ok(commands::export_routing_config(services).await?),
        "import_routing_config" => ok(commands::import_routing_config(
            services,
            field(&args, "config")?,
            optional(&args, "apply")?.unwrap_or(false),
        )
        .await?),
        "list_logs" => ok(commands::list_logs(services, optional(&args, "query")?).await?),
        "get_usage" => ok(commands::get_usage(
            services,
            field(&args, "period")?,
            optional(&args, "target")?,
        )
        .await?),
        "get_key_usage" => {
            ok(
                commands::get_key_usage(services, field(&args, "id")?, field(&args, "period")?)
                    .await?,
            )
        }
        "get_log_facets" => ok(commands::get_log_facets(services).await?),
        "clear_logs" => {
            commands::clear_logs(services).await?;
            Ok(Value::Null)
        }
        "export_logs_csv" => ok(commands::export_logs_csv(
            services,
            optional(&args, "path")?,
            optional(&args, "query")?,
        )
        .await?),
        "get_settings" => ok(commands::get_settings(services).await?),
        "save_setting" => {
            commands::save_setting(services, field(&args, "key")?, field(&args, "value")?).await?;
            Ok(Value::Null)
        }
        "get_resource_policy" => ok(commands::get_resource_policy(services).await?),
        "get_resource_profile_preset" => ok(commands::get_resource_profile_preset(field(
            &args, "profile",
        )?)?),
        "save_resource_policy" => {
            commands::save_resource_policy(services, field(&args, "policy")?).await?;
            Ok(Value::Null)
        }
        "save_model_resource_overrides" => ok(commands::save_model_resource_overrides(
            services,
            field(&args, "id")?,
            optional(&args, "overrides")?,
            optional(&args, "force_tool_support")?,
        )
        .await?),
        "save_model_speculative_config" => ok(commands::save_model_speculative_config(
            services,
            field(&args, "id")?,
            optional(&args, "config")?,
        )
        .await?),
        "clear_kv_cache" => {
            commands::clear_kv_cache(services, optional(&args, "target_id")?).await?;
            Ok(Value::Null)
        }
        "save_hugging_face_token" => {
            commands::save_hugging_face_token(services, field(&args, "token")?)?;
            Ok(Value::Null)
        }
        "save_civitai_token" => {
            commands::save_civitai_token(services, field(&args, "token")?)?;
            Ok(Value::Null)
        }
        "forget_all_credentials" => {
            commands::forget_all_credentials(services).await?;
            Ok(Value::Null)
        }
        "list_directory_users" => ok(commands::list_directory_users(services).await?),
        "create_directory_user" => ok(commands::create_directory_user(
            services,
            field(&args, "input")?,
        )
        .await?),
        "update_directory_user" => ok(commands::update_directory_user(
            services,
            field(&args, "id")?,
            field(&args, "input")?,
        )
        .await?),
        "list_directory_groups" => ok(commands::list_directory_groups(services).await?),
        "save_directory_group" => ok(commands::save_directory_group(
            services,
            optional(&args, "id")?,
            field(&args, "input")?,
        )
        .await?),
        "delete_directory_group" => {
            commands::delete_directory_group(services, field(&args, "id")?).await?;
            Ok(Value::Null)
        }
        "user_permissions" => ok(commands::user_permissions(services, field(&args, "id")?).await?),
        "reveal_operator_bootstrap" => {
            ok(commands::reveal_operator_bootstrap(services).await?)
        }
        "list_oidc_allowlist" => ok(commands::list_oidc_allowlist(services).await?),
        "invite_oidc_identity" => ok(commands::invite_oidc_identity(
            services,
            field(&args, "provider")?,
            field(&args, "identifier")?,
            optional(&args, "user_id")?,
        )
        .await?),
        "delete_oidc_allowlist" => {
            commands::delete_oidc_allowlist(services, field(&args, "id")?).await?;
            Ok(Value::Null)
        }
        "save_oidc_client" => {
            commands::save_oidc_client(
                services,
                field(&args, "provider")?,
                field(&args, "client_id")?,
                field(&args, "client_secret")?,
            )
            .await?;
            Ok(Value::Null)
        }
        _ => Err("unknown admin command".into()),
    }
}

fn ok<T: serde::Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| error.to_string())
}

fn field<T: DeserializeOwned>(args: &Value, name: &str) -> Result<T, String> {
    let value = args
        .get(name)
        .cloned()
        .ok_or_else(|| format!("missing argument {name}"))?;
    serde_json::from_value(value).map_err(|error| format!("{name}: {error}"))
}

fn optional<T: DeserializeOwned>(args: &Value, name: &str) -> Result<Option<T>, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|error| format!("{name}: {error}")),
    }
}

fn snake_case_top_level(value: Value) -> Value {
    let Value::Object(map) = value else {
        return value;
    };
    Value::Object(
        map.into_iter()
            .map(|(key, value)| (camel_to_snake(&key), value))
            .collect(),
    )
}

fn camel_to_snake(input: &str) -> String {
    let mut out = String::new();
    for (index, ch) in input.chars().enumerate() {
        if ch.is_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

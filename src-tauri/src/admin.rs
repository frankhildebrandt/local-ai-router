use std::{convert::Infallible, path::PathBuf, sync::Arc};

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode, Uri},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use futures_util::stream::Stream;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::{
    commands::{self, AppServices},
    gateway,
};

#[derive(Clone)]
struct AdminState {
    services: Arc<AppServices>,
}

pub fn router(services: Arc<AppServices>) -> Router {
    Router::new()
        .route("/admin/events", get(events))
        .route("/admin/{name}", post(invoke))
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

async fn events(
    State(state): State<AdminState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
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
                                yield Ok(item);
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
                                yield Ok(item);
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            if let Ok(item) = Event::default()
                                .event("gateway-traffic")
                                .json_data(traffic.snapshot())
                            {
                                yield Ok(item);
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn invoke(
    State(state): State<AdminState>,
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
    match dispatch(&state.services, &name, args).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => {
            let status = if error == "unknown admin command" {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::BAD_REQUEST
            };
            (status, error).into_response()
        }
    }
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

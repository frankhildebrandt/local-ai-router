use std::{
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    body::{Body, Bytes},
    extract::{OriginalUri, Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::Response,
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    core::{AppCore, InFlightGuard, InFlightProgress, InFlightRequest},
    domain::{
        can_retry_same_target, first_byte_timeout_ms, is_fallback_status, is_transient_status,
        supports_capability, RATE_LIMIT_DEFAULT_SECS, SAME_TARGET_RETRY_DELAY_MS,
        SAME_TARGET_RETRY_LIMIT, SAME_TARGET_RETRY_MAX_WAIT_MS, SLOW_WINDOW_SECS,
    },
    protocol::{
        decode_request, decode_response, encode_request, encode_response, validate_cross_protocol,
        PublicProtocol, StreamTranslator,
    },
    providers::{provider_preset, AuthScheme, WireProtocol},
    public_models::{advertised_public_models, resolve_public_model},
    routing::{
        evaluate_route, PolicyStatus, RouteEvaluationInput, RoutingAttemptRecord,
        RoutingEvaluation, RoutingMode,
    },
    runtime::RuntimeManager,
    storage::{RequestLog, TokenUsage},
};

#[derive(Clone)]
struct GatewayState {
    core: Arc<AppCore>,
    runtimes: Option<Arc<RuntimeManager>>,
}

pub fn router(core: Arc<AppCore>) -> Router {
    inference_router(core, None).fallback(not_found)
}

pub fn managed_router(core: Arc<AppCore>, runtimes: Arc<RuntimeManager>) -> Router {
    inference_router(core, Some(runtimes)).fallback(not_found)
}

pub fn inference_router(core: Arc<AppCore>, runtimes: Option<Arc<RuntimeManager>>) -> Router {
    router_with_state(GatewayState { core, runtimes })
}

async fn sync_runtime_states(core: &AppCore, runtimes: &RuntimeManager) {
    let Ok(targets) = core.store.targets().await else {
        return;
    };
    for mut target in targets {
        if matches!(
            target.kind,
            crate::domain::TargetKind::Gguf | crate::domain::TargetKind::Mlx
        ) && target.runtime_url.is_some()
            && !runtimes.is_running(&target.id)
        {
            target.runtime_url = None;
            target.state = "stopped".into();
            if let Err(error) = core.store.upsert_target(&target).await {
                tracing::warn!(target = %target.id, %error, "failed to synchronize local runtime state");
            }
        }
    }
}

fn router_with_state(state: GatewayState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1beta/models", get(gemini_models))
        .route("/v1/chat/completions", post(proxy))
        .route("/v1/responses", post(proxy))
        .route("/v1/messages", post(proxy))
        .route("/v1beta/models/{*operation}", post(proxy))
        .route("/v1/completions", post(proxy))
        .route("/v1/embeddings", post(proxy))
        .route("/v1/images/generations", post(proxy))
        .route("/v1/images/edits", post(proxy))
        .route("/v1/audio/speech", post(proxy))
        .route("/v1/audio/transcriptions", post(proxy))
        .route("/v1/audio/translations", post(proxy))
        .route("/v1/moderations", post(proxy))
        .route("/uplink/join", post(uplink_join))
        .route("/uplink/models", get(uplink_models))
        .route("/uplink/leave", post(uplink_leave))
        .route("/uplink/publish", post(uplink_publish))
        .route("/uplink/unpublish", post(uplink_unpublish))
        .route("/uplink/replicas/heartbeat", post(uplink_replica_heartbeat))
        .route("/uplink/images", get(uplink_images).post(uplink_register_image))
        .route("/uplink/images/{id}/blob", get(uplink_image_blob).post(uplink_image_blob_upload))
        .route("/uplink/images/{id}/installed", post(uplink_image_installed))
        .with_state(state)
}

async fn gemini_models(
    State(state): State<GatewayState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response<Body> {
    let core = state.core;
    if query_has_api_key(&uri) {
        return protocol_error(
            PublicProtocol::Gemini,
            StatusCode::BAD_REQUEST,
            "query_key_rejected",
            "API keys in the query string are not accepted",
        );
    }
    let Some(caller) = authenticated_caller(&core, &headers).await else {
        return protocol_error(
            PublicProtocol::Gemini,
            StatusCode::UNAUTHORIZED,
            "invalid_api_key",
            "Invalid local API key",
        );
    };
    match advertised_routes(&core).await {
        Ok(routes) => {
            let routes = filter_routes_for_caller(&core, &caller, routes).await;
            json_response(
                StatusCode::OK,
                json!({"models": routes.into_iter().filter(|route| route.enabled).map(|route| json!({"name":format!("models/{}",route.alias),"displayName":route.alias,"supportedGenerationMethods":["generateContent","streamGenerateContent"]})).collect::<Vec<_>>() }),
            )
        }
        Err(_) => protocol_error(
            PublicProtocol::Gemini,
            StatusCode::INTERNAL_SERVER_ERROR,
            "storage_error",
            "Unable to read model routes",
        ),
    }
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

pub(crate) async fn not_found() -> Response<Body> {
    openai_error(
        StatusCode::NOT_FOUND,
        "route_not_found",
        "This OpenAI-compatible endpoint is not implemented",
    )
}

async fn models(
    State(state): State<GatewayState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response<Body> {
    let core = state.core;
    if query_has_api_key(&uri) {
        return openai_error(
            StatusCode::BAD_REQUEST,
            "query_key_rejected",
            "API keys in the query string are not accepted",
        );
    }
    let Some(caller) = authenticated_caller(&core, &headers).await else {
        return unauthorized();
    };
    match advertised_routes(&core).await {
        Ok(routes) => {
            let routes = filter_routes_for_caller(&core, &caller, routes).await;
            json_response(
                StatusCode::OK,
                json!({
                    "object": "list",
                    "data": routes.into_iter().filter(|route| route.enabled).map(|route| json!({
                        "id": route.alias, "object": "model", "created": 0, "owned_by": "local-ai-router", "capabilities": route.capabilities
                    })).collect::<Vec<_>>()
                }),
            )
        }
        Err(_) => openai_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "storage_error",
            "Unable to read model routes",
        ),
    }
}

async fn proxy(
    State(state): State<GatewayState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    let core = state.core;
    let runtimes = state.runtimes;
    let public_protocol = protocol_for_path(uri.path());
    if query_has_api_key(&uri) {
        return request_error(
            public_protocol,
            StatusCode::BAD_REQUEST,
            "query_key_rejected",
            "API keys in the query string are not accepted",
        );
    }
    let Some(caller) = authenticated_caller(&core, &headers).await else {
        return request_error(
            public_protocol,
            StatusCode::UNAUTHORIZED,
            "invalid_api_key",
            "Invalid local API key",
        );
    };
    let api_key_id = caller.kv_id();
    let session_id = match validated_session_id(headers.get("x-local-ai-session")) {
        Ok(value) => value.map(str::to_owned),
        Err(message) => {
            return request_error(
                public_protocol,
                StatusCode::BAD_REQUEST,
                "invalid_local_session",
                message,
            )
        }
    };
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json");
    let mut json_payload = if content_type.starts_with("application/json") {
        match serde_json::from_slice::<Value>(&body) {
            Ok(value) => Some(value),
            Err(_) => {
                return request_error(
                    public_protocol,
                    StatusCode::BAD_REQUEST,
                    "invalid_json",
                    "Request body must be valid JSON",
                )
            }
        }
    } else if content_type.starts_with("multipart/form-data") {
        None
    } else {
        return request_error(
            public_protocol,
            StatusCode::BAD_REQUEST,
            "unsupported_content_type",
            "Request body must be JSON or multipart/form-data",
        );
    };
    let path_model = gemini_path_model(uri.path());
    let alias = path_model.map(str::to_owned).or_else(|| {
        json_payload
            .as_ref()
            .and_then(|payload| payload.get("model"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| extract_multipart_model(&body).map(str::to_owned))
    });
    let Some(alias) = alias else {
        return request_error(
            public_protocol,
            StatusCode::BAD_REQUEST,
            "model_required",
            "The model field is required",
        );
    };
    let resolved = match resolve_public_model(&core.store, &alias).await {
        Ok(Some(resolved)) if resolved.route.enabled => resolved,
        Ok(Some(_)) | Ok(None) => {
            return request_error(
                public_protocol,
                StatusCode::NOT_FOUND,
                "model_not_found",
                "Unknown or unavailable model",
            )
        }
        Err(_) => {
            return request_error(
                public_protocol,
                StatusCode::INTERNAL_SERVER_ERROR,
                "storage_error",
                "Unable to resolve the requested model",
            )
        }
    };
    let route = resolved.route;
    if let Err(response) = enforce_uplink_access(&core, &caller, public_protocol, &alias).await {
        return response;
    }
    if let Err(response) = enforce_replica_access(&core, &caller, public_protocol, &alias).await {
        return response;
    }
    let capability = endpoint_capability(uri.path());
    let routing_policy = resolved.policy;
    let adaptive_active = routing_policy.as_ref().is_some_and(|policy| {
        policy.mode == RoutingMode::Adaptive && policy.status == PolicyStatus::Active
    });
    if !adaptive_active && !route_supports_capability(&route, capability) {
        return request_error(
            public_protocol,
            StatusCode::BAD_REQUEST,
            "unsupported_capability",
            "This alias does not support the requested capability",
        );
    }
    let is_stream = uri.path().contains(":streamGenerateContent")
        || json_payload
            .as_ref()
            .and_then(|payload| payload.get("stream"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
    if let Some(payload) = json_payload.as_mut() {
        if uri.path().contains(":streamGenerateContent") {
            payload["stream"] = Value::Bool(true);
        }
    }
    let decoded = match (public_protocol, json_payload.as_ref()) {
        (Some(protocol), Some(payload)) => match decode_request(protocol, payload, path_model) {
            Ok(request) => Some(request),
            Err(error) => {
                return protocol_error(
                    protocol,
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    &error.to_string(),
                )
            }
        },
        _ => None,
    };
    let mut required_capabilities = vec![capability.to_owned()];
    if is_stream {
        required_capabilities.push("streaming".into());
    }
    if let Some(canonical) = decoded.as_ref() {
        if !canonical.tools.is_empty() {
            required_capabilities.push("tools".into());
        }
        for (needed, name) in [
            (
                canonical
                    .messages
                    .iter()
                    .flat_map(|message| &message.content)
                    .any(|block| matches!(block, crate::protocol::ContentBlock::Image { .. })),
                "vision",
            ),
            (
                canonical
                    .messages
                    .iter()
                    .flat_map(|message| &message.content)
                    .any(|block| matches!(block, crate::protocol::ContentBlock::Audio { .. })),
                "audio_input",
            ),
            (
                canonical
                    .messages
                    .iter()
                    .flat_map(|message| &message.content)
                    .any(|block| matches!(block, crate::protocol::ContentBlock::Video { .. })),
                "video_input",
            ),
        ] {
            if needed {
                required_capabilities.push(name.into());
            }
        }
    }
    required_capabilities.sort();
    required_capabilities.dedup();
    let explicit_task = headers
        .get("x-local-ai-task")
        .and_then(|value| value.to_str().ok());
    let evaluation = match evaluate_route(
        &core.store,
        &route,
        RouteEvaluationInput {
            policy: routing_policy.as_ref(),
            explicit_task,
            endpoint: uri.path(),
            canonical: decoded.as_ref(),
            required_capabilities,
            streaming: is_stream,
        },
    )
    .await
    {
        Ok(evaluation) => evaluation,
        Err(error) if error.to_string().starts_with("unknown routing task:") => {
            return request_error(
                public_protocol,
                StatusCode::BAD_REQUEST,
                "invalid_task",
                &error.to_string(),
            )
        }
        Err(error) => {
            return request_error(
                public_protocol,
                StatusCode::INTERNAL_SERVER_ERROR,
                "routing_error",
                &error.to_string(),
            )
        }
    };
    let mut inflight = Some(InFlightGuard::new(
        core.traffic.clone(),
        InFlightRequest {
            id: request_id.clone(),
            started_at: Utc::now(),
            endpoint: uri.path().into(),
            alias: alias.clone(),
            target_id: None,
            target_name: None,
            phase: "trying".into(),
            attempt: 0,
            last_error_code: None,
            last_error_message: None,
        },
    ));
    let cancel = inflight.as_ref().expect("in-flight guard").cancellation();
    let target_ids = evaluation.ordered_target_ids.clone();
    if target_ids.is_empty() {
        if evaluation.mode == "adaptive" {
            let mut outcome =
                RoutingAttemptOutcome::from_previous(503, Duration::ZERO, is_stream, None);
            outcome.transient_failure = false;
            record_routing_attempt(&core, &evaluation, &request_id, "none", outcome).await;
        }
        return request_error(
            public_protocol,
            StatusCode::SERVICE_UNAVAILABLE,
            if evaluation.mode == "adaptive" {
                "no_available_target"
            } else {
                "no_targets"
            },
            if evaluation.mode == "adaptive" {
                "No target satisfies this adaptive routing policy"
            } else {
                "This alias has no enabled targets"
            },
        );
    }

    let mut attempts = 0i64;
    let mut last_error: Option<Response<Body>> = None;
    let mut last_error_target_id: Option<String> = None;
    let mut last_error_target_name: Option<String> = None;
    let mut last_error_detail: Option<(u16, Option<String>, Option<String>)> = None;
    let mut last_translation_error: Option<String> = None;
    let mut last_capability_error: Option<String> = None;
    let mut previous_failure: Option<(u16, Option<String>)> = None;
    let mut request_logged = false;
    'hops: for target_id in &target_ids {
        let has_later_hop = evaluation.has_later_hop(target_id);
        let is_fallback_hop = evaluation.is_fallback_hop(target_id);
        let tighten_first_byte = !is_fallback_hop && evaluation.has_later_primary(target_id);
        if evaluation.mode == "adaptive"
            && evaluation.half_open_target_ids.contains(target_id)
            && !core
                .store
                .claim_half_open(target_id, &evaluation.task)
                .await
                .unwrap_or(false)
        {
            attempts += 1;
            continue;
        }
        let mut same_target_attempt = 0u32;
        'retry: loop {
            same_target_attempt += 1;
            attempts += 1;
            let attempt_started = Instant::now();
            let Ok(Some(mut target)) = core.store.target(target_id).await else {
                continue 'hops;
            };
            let phase = if same_target_attempt > 1 {
                "retrying"
            } else if previous_failure.is_some() {
                "rerouting"
            } else {
                "trying"
            };
            let live_error_code = last_error_detail
                .as_ref()
                .and_then(|(_, code, _)| code.clone());
            let live_error_message = last_error_detail
                .as_ref()
                .and_then(|(_, _, message)| message.clone());
            inflight.as_ref().expect("in-flight guard").apply(
                InFlightProgress::new(&target.id, &target.name, phase)
                    .with_attempt(same_target_attempt)
                    .with_error(live_error_code.as_deref(), live_error_message.as_deref()),
            );
            if !target_supports_capability(&target, capability) {
                last_capability_error =
                    Some("No target in this alias supports the requested capability".into());
                continue 'hops;
            }
            if decoded.is_none()
                && !matches!(
                    target.wire_protocol,
                    WireProtocol::OpenAiChat | WireProtocol::OpenAiResponses
                )
            {
                continue 'hops;
            }
            let mut canonical = decoded.clone();
            if let Some(canonical) = canonical.as_mut() {
                if canonical.reasoning.is_some()
                    && !target_supports_capability(&target, "reasoning")
                {
                    canonical.reasoning = None;
                }
                if canonical.response_format.is_some()
                    && !target_supports_capability(&target, "structured_output")
                {
                    canonical.response_format = None;
                }
                let required = [
                    (is_stream, "streaming"),
                    (!canonical.tools.is_empty(), "tools"),
                    (
                        canonical
                            .messages
                            .iter()
                            .flat_map(|message| &message.content)
                            .any(|block| {
                                matches!(block, crate::protocol::ContentBlock::Image { .. })
                            }),
                        "vision",
                    ),
                    (
                        canonical
                            .messages
                            .iter()
                            .flat_map(|message| &message.content)
                            .any(|block| {
                                matches!(block, crate::protocol::ContentBlock::Audio { .. })
                            }),
                        "audio_input",
                    ),
                    (
                        canonical
                            .messages
                            .iter()
                            .flat_map(|message| &message.content)
                            .any(|block| {
                                matches!(block, crate::protocol::ContentBlock::Video { .. })
                            }),
                        "video_input",
                    ),
                ];
                if let Some((_, missing)) = required.into_iter().find(|(needed, capability)| {
                    *needed && !target_supports_capability(&target, capability)
                }) {
                    last_capability_error =
                        Some(format!("No target in this alias supports `{missing}`"));
                    continue 'hops;
                }
            }
            let tools_for_salvage = canonical
                .as_ref()
                .map(|request| request.tools.clone())
                .unwrap_or_default();
            let emulation = canonical
                .as_ref()
                .map(|request| {
                    crate::tool_emulation::tool_emulation_for(&target, !request.tools.is_empty())
                })
                .unwrap_or(crate::tool_emulation::ToolEmulation::None);
            if emulation == crate::tool_emulation::ToolEmulation::MlxInject {
                if let Some(request) = canonical.as_mut() {
                    crate::tool_emulation::prepare_mlx_request(request);
                }
            }
            let buffer_emulation = emulation == crate::tool_emulation::ToolEmulation::MlxInject;
            if matches!(
                target.kind,
                crate::domain::TargetKind::Gguf | crate::domain::TargetKind::Mlx
            ) {
                if uri.path() == "/v1/images/generations" {
                    if let Some(payload) = json_payload.as_ref() {
                        if let Err(error) = validate_local_image_request(payload) {
                            return request_error(
                                public_protocol,
                                StatusCode::BAD_REQUEST,
                                "unsupported_parameter",
                                &error.to_string(),
                            );
                        }
                    }
                }
                if uri.path() == "/v1/audio/speech" {
                    if let Some(payload) = json_payload.as_ref() {
                        if let Err(error) = validate_local_speech_request(payload) {
                            return request_error(
                                public_protocol,
                                StatusCode::BAD_REQUEST,
                                "unsupported_parameter",
                                &error.to_string(),
                            );
                        }
                    }
                }
                if let Some(canonical) = canonical.as_mut() {
                    if let Err(error) =
                        crate::media::resolve_request_media(&core.client, canonical).await
                    {
                        return request_error(
                            public_protocol,
                            StatusCode::BAD_REQUEST,
                            "invalid_media",
                            &error.to_string(),
                        );
                    }
                }
            }
            if matches!(
                target.kind,
                crate::domain::TargetKind::Gguf | crate::domain::TargetKind::Mlx
            ) && runtimes
                .as_ref()
                .is_some_and(|runtimes| !runtimes.is_running(&target.id))
            {
                target.runtime_url = None;
                target.state = "stopped".into();
                let _ = core.store.upsert_target(&target).await;
            }
            if matches!(
                target.kind,
                crate::domain::TargetKind::Gguf | crate::domain::TargetKind::Mlx
            ) && target.runtime_url.is_none()
            {
                let auto_load = core
                    .effective_resource_policy(&target)
                    .await
                    .map(|policy| policy.auto_load)
                    .unwrap_or(false);
                let load = if auto_load {
                    match runtimes.as_ref() {
                        Some(runtimes) => runtimes.start_resolved(&core.store, &target).await,
                        None => Err(anyhow::anyhow!("runtime manager unavailable")),
                    }
                } else {
                    Err(anyhow::anyhow!("automatic loading is disabled"))
                };
                match load {
                    Ok(url) => {
                        target.runtime_url = Some(url);
                        target.state = "ready".into();
                        let _ = core.store.upsert_target(&target).await;
                        if let Some(runtimes) = runtimes.as_ref() {
                            sync_runtime_states(&core, runtimes).await;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(target = %target.id, %error, "local model load failed");
                        let message = format!("The local model could not be loaded: {error}");
                        record_skipped_hop(
                            &core,
                            &evaluation,
                            &request_id,
                            &target.id,
                            attempt_started,
                            is_stream,
                            &mut previous_failure,
                            503,
                            "local_load_failed",
                            message.clone(),
                            same_target_attempt,
                        )
                        .await;
                        last_error = Some(request_error(
                            public_protocol,
                            StatusCode::SERVICE_UNAVAILABLE,
                            "local_load_failed",
                            &message,
                        ));
                        last_error_target_id = Some(target.id.clone());
                        last_error_target_name = Some(target.name.clone());
                        last_error_detail =
                            Some((503, Some("local_load_failed".into()), Some(message.clone())));
                        match recover_failed_hop(
                            &cancel,
                            FailedHop {
                                inflight: inflight.as_ref(),
                                target_id: &target.id,
                                target_name: &target.name,
                                status: 503,
                                error_code: Some("local_load_failed"),
                                error_message: Some(&message),
                                same_target_attempt,
                                has_later_hop,
                                retry_after_until: None,
                            },
                        )
                        .await
                        {
                            HopRecovery::RetrySame => continue 'retry,
                            HopRecovery::NextHop => continue 'hops,
                            HopRecovery::Cancelled => {
                                return cancelled_proxy_response(
                                    &core,
                                    public_protocol,
                                    &request_id,
                                    &api_key_id,
                                    uri.path(),
                                    &alias,
                                    Some(&target.name),
                                    attempts,
                                    started,
                                )
                                .await;
                            }
                            HopRecovery::Exhausted => {
                                return request_error(
                                    public_protocol,
                                    StatusCode::SERVICE_UNAVAILABLE,
                                    "local_load_failed",
                                    &message,
                                );
                            }
                        }
                    }
                }
            }
            let local_permit = tokio::select! {
                result = core.acquire_local_slot(&target) => match result {
                    Ok(permit) => permit,
                    Err(error) => {
                        tracing::warn!(target = %target.id, %error, "local admission failed");
                        let message = format!("The local model could not admit the request: {error}");
                        record_skipped_hop(
                            &core,
                            &evaluation,
                            &request_id,
                            &target.id,
                            attempt_started,
                            is_stream,
                            &mut previous_failure,
                            503,
                            "local_busy",
                            message.clone(),
                            same_target_attempt,
                        )
                        .await;
                        last_error = Some(request_error(
                            public_protocol,
                            StatusCode::SERVICE_UNAVAILABLE,
                            "local_busy",
                            &message,
                        ));
                        last_error_target_id = Some(target.id.clone());
                        last_error_target_name = Some(target.name.clone());
                        last_error_detail =
                            Some((503, Some("local_busy".into()), Some(message.clone())));
                        match recover_failed_hop(
                            &cancel,
                            FailedHop {
                                inflight: inflight.as_ref(),
                                target_id: &target.id,
                                target_name: &target.name,
                                status: 503,
                                error_code: Some("local_busy"),
                                error_message: Some(&message),
                                same_target_attempt,
                                has_later_hop,
                                retry_after_until: None,
                            },
                        )
                        .await
                        {
                            HopRecovery::RetrySame => continue 'retry,
                            HopRecovery::NextHop => continue 'hops,
                            HopRecovery::Cancelled => {
                                return cancelled_proxy_response(
                                    &core,
                                    public_protocol,
                                    &request_id,
                                    &api_key_id,
                                    uri.path(),
                                    &alias,
                                    Some(&target.name),
                                    attempts,
                                    started,
                                )
                                .await;
                            }
                            HopRecovery::Exhausted => {
                                return request_error(
                                    public_protocol,
                                    StatusCode::SERVICE_UNAVAILABLE,
                                    "local_busy",
                                    &message,
                                )
                            }
                        }
                    }
                },
                _ = cancel.cancelled() => {
                    return cancelled_proxy_response(
                        &core,
                        public_protocol,
                        &request_id,
                        &api_key_id,
                        uri.path(),
                        &alias,
                        Some(&target.name),
                        attempts,
                        started,
                    )
                    .await;
                }
            };
            let Ok((base_url, credential, account_id)) = core.target_endpoint(&target).await else {
                if target.kind.is_replica() {
                    let _ = crate::publish::mark_replica_unhealthy(&core.store, &target.id).await;
                }
                record_skipped_hop(
                    &core,
                    &evaluation,
                    &request_id,
                    &target.id,
                    attempt_started,
                    is_stream,
                    &mut previous_failure,
                    502,
                    "upstream_unavailable",
                    "The selected backend could not be reached".into(),
                    same_target_attempt,
                )
                .await;
                last_error = Some(request_error(
                    public_protocol,
                    StatusCode::BAD_GATEWAY,
                    "upstream_unavailable",
                    "The selected backend could not be reached",
                ));
                last_error_target_id = Some(target.id.clone());
                last_error_target_name = Some(target.name.clone());
                last_error_detail = Some((
                    502,
                    Some("upstream_unavailable".into()),
                    Some("The selected backend could not be reached".into()),
                ));
                match recover_failed_hop(
                    &cancel,
                    FailedHop {
                        inflight: inflight.as_ref(),
                        target_id: &target.id,
                        target_name: &target.name,
                        status: 502,
                        error_code: Some("upstream_unavailable"),
                        error_message: Some("The selected backend could not be reached"),
                        same_target_attempt,
                        has_later_hop,
                        retry_after_until: None,
                    },
                )
                .await
                {
                    HopRecovery::RetrySame => continue 'retry,
                    HopRecovery::NextHop => continue 'hops,
                    HopRecovery::Cancelled => {
                        return cancelled_proxy_response(
                            &core,
                            public_protocol,
                            &request_id,
                            &api_key_id,
                            uri.path(),
                            &alias,
                            Some(&target.name),
                            attempts,
                            started,
                        )
                        .await;
                    }
                    HopRecovery::Exhausted => {
                        request_logged = true;
                        log_request(
                            &core,
                            LogMetadata {
                                id: &request_id,
                                api_key_id: caller.api_key_id(),
                                directory_user_id: caller.directory_user_id(),
                                endpoint: uri.path(),
                                alias: Some(&alias),
                                target: Some(&target.name),
                                attempts,
                                status: 502,
                                latency_ms: started.elapsed().as_millis() as i64,
                                usage: TokenUsage::default(),
                                error_code: Some("upstream_unavailable"),
                                error_message: Some("The selected backend could not be reached"),
                            },
                        )
                        .await;
                        continue 'hops;
                    }
                }
            };
            let kv_context = if matches!(
                target.kind,
                crate::domain::TargetKind::Gguf | crate::domain::TargetKind::Mlx
            ) {
                match (runtimes.as_ref(), session_id.as_deref()) {
                    (Some(runtimes), Some(session_id)) => {
                        if let Err(error) =
                            runtimes.restore_kv(&target, &api_key_id, session_id).await
                        {
                            tracing::warn!(target = %target.id, %error, "discarding unusable KV snapshot");
                        }
                        Some((
                            runtimes.clone(),
                            target.clone(),
                            api_key_id.clone(),
                            session_id.to_owned(),
                        ))
                    }
                    _ => None,
                }
            } else {
                None
            };
            let provider = if let Some(provider_id) = target.provider_id.as_deref() {
                core.store.provider(provider_id).await.ok().flatten()
            } else {
                None
            };
            let upstream_path = if target.kind.is_uplink() || target.kind.is_replica() {
                crate::uplink::uplink_upstream_path(uri.path(), &target.provider_model)
            } else if provider
                .as_ref()
                .is_some_and(|provider| provider.preset_id == "openai_subscription")
                && canonical.is_some()
            {
                "/responses".into()
            } else if canonical.is_some() {
                text_upstream_path(target.wire_protocol, &target.provider_model, is_stream)
            } else {
                uri.path().to_owned()
            };
            let upstream_url = if target.wire_protocol == WireProtocol::GeminiGenerateContent
                && canonical.is_some()
            {
                format!(
                    "{}/{}",
                    base_url.trim_end_matches('/'),
                    upstream_path.trim_start_matches('/')
                )
            } else {
                join_api_url(&base_url, &upstream_path)
            };
            let client = hop_http_client(&core, &target)
                .await
                .unwrap_or_else(|_| core.client.clone());
            let mut request = client.post(upstream_url);
            if let Some(payload) = json_payload.as_mut() {
                let mut outbound = if target.kind.is_uplink() || target.kind.is_replica() {
                    let mut native = payload.clone();
                    if native.get("model").is_some() {
                        native["model"] = Value::String(target.provider_model.clone());
                    }
                    native
                } else if let Some(canonical) = canonical.as_ref() {
                    let protocol = public_protocol.unwrap();
                    if !matches!(
                        target.kind,
                        crate::domain::TargetKind::Gguf | crate::domain::TargetKind::Mlx
                    ) && protocol_matches(protocol, target.wire_protocol)
                    {
                        let mut native = payload.clone();
                        if protocol != PublicProtocol::Gemini {
                            native["model"] = Value::String(target.provider_model.clone());
                        }
                        native
                    } else {
                        if !protocol_matches(protocol, target.wire_protocol) {
                            if let Err(error) = validate_cross_protocol(protocol, payload) {
                                last_translation_error = Some(error.to_string());
                                continue 'hops;
                            }
                        }
                        match encode_request(
                            target.wire_protocol,
                            canonical,
                            &target.provider_model,
                        ) {
                            Ok(value) => value,
                            Err(error) => {
                                last_translation_error = Some(error.to_string());
                                continue 'hops;
                            }
                        }
                    }
                } else {
                    payload["model"] = Value::String(target.provider_model.clone());
                    payload.clone()
                };
                if buffer_emulation {
                    crate::tool_emulation::strip_unsupported_mlx_fields(&mut outbound);
                }
                request = request.json(&outbound);
            } else {
                request = request
                    .header(header::CONTENT_TYPE, content_type)
                    .body(rewrite_multipart_model(&body, &target.provider_model));
            }
            if let Some(credential) = credential {
                request = match provider
                    .as_ref()
                    .and_then(|provider| provider_preset(&provider.preset_id))
                    .map(|preset| preset.auth_scheme)
                    .unwrap_or(AuthScheme::Bearer)
                {
                    AuthScheme::Bearer => request.bearer_auth(credential),
                    AuthScheme::XApiKey => request
                        .header("x-api-key", credential)
                        .header("anthropic-version", "2023-06-01"),
                    AuthScheme::XGoogApiKey => request.header("x-goog-api-key", credential),
                    AuthScheme::OpenAiSubscription => request
                        .bearer_auth(credential)
                        .header("originator", "local_ai_router")
                        .header("OpenAI-Beta", "responses=experimental"),
                };
            }
            if let Some(account_id) = account_id {
                request = request.header("ChatGPT-Account-Id", account_id);
            }
            if target.wire_protocol == WireProtocol::AnthropicMessages {
                request = request.header("anthropic-version", "2023-06-01");
            }
            let is_openrouter = provider
                .as_ref()
                .is_some_and(|provider| provider.preset_id == "openrouter");
            if target.kind == crate::domain::TargetKind::Mlx {
                request = request.header("x-local-ai-cache-namespace", &api_key_id);
                if let Some(session_id) = session_id.as_deref() {
                    request = request.header("x-local-ai-session", session_id);
                }
            }
            if is_openrouter {
                request = request
                    .header("HTTP-Referer", "https://local-ai-router.app")
                    .header("X-Title", "Local AI Router");
            }
            let attempt_timeout = Duration::from_millis(first_byte_timeout_ms(
                evaluation.peer_latency_ms,
                tighten_first_byte,
            ));
            match wait_with_timeout_or_cancel(attempt_timeout, &cancel, request.send()).await {
                WaitOutcome::Cancelled => {
                    return cancelled_proxy_response(
                        &core,
                        public_protocol,
                        &request_id,
                        &api_key_id,
                        uri.path(),
                        &alias,
                        Some(&target.name),
                        attempts,
                        started,
                    )
                    .await;
                }
                WaitOutcome::Ready(Ok(upstream)) => {
                    let status = upstream.status();
                    let retry_after_until = rate_limit_until(upstream.headers(), status.as_u16());
                    if status.is_redirection() {
                        log_request(
                            &core,
                            LogMetadata {
                                id: &request_id,
                                api_key_id: caller.api_key_id(),
                                directory_user_id: caller.directory_user_id(),
                                endpoint: uri.path(),
                                alias: Some(&alias),
                                target: Some(&target.name),
                                attempts,
                                status: 502,
                                latency_ms: started.elapsed().as_millis() as i64,
                                usage: TokenUsage::default(),
                                error_code: Some("credential_redirect_rejected"),
                                error_message: Some(
                                    "The upstream attempted an unexpected redirect",
                                ),
                            },
                        )
                        .await;
                        return protocol_error(
                            public_protocol.unwrap_or(PublicProtocol::OpenAiChat),
                            StatusCode::BAD_GATEWAY,
                            "credential_redirect_rejected",
                            "The upstream attempted an unexpected redirect",
                        );
                    }
                    let mut usage = TokenUsage::default();
                    let mut error_code = None;
                    let mut error_message = None;
                    let mut attempt_ttft = None;
                    let response = if is_stream && !buffer_emulation && status.is_success() {
                        let translated_stream = public_protocol.filter(|protocol| {
                            !target.kind.is_uplink()
                                && !protocol_matches(*protocol, target.wire_protocol)
                        });
                        let content_type = if translated_stream.is_some() {
                            Some(HeaderValue::from_static("text/event-stream"))
                        } else {
                            upstream.headers().get(header::CONTENT_TYPE).cloned()
                        };
                        let mut upstream_stream = upstream.bytes_stream();
                        let first_chunk = match wait_with_timeout_or_cancel(
                            attempt_timeout,
                            &cancel,
                            upstream_stream.next(),
                        )
                        .await
                        {
                            WaitOutcome::Cancelled => {
                                return cancelled_proxy_response(
                                    &core,
                                    public_protocol,
                                    &request_id,
                                    &api_key_id,
                                    uri.path(),
                                    &alias,
                                    Some(&target.name),
                                    attempts,
                                    started,
                                )
                                .await;
                            }
                            WaitOutcome::Ready(Some(Ok(chunk))) => chunk,
                            WaitOutcome::Ready(Some(Err(_)))
                            | WaitOutcome::Ready(None)
                            | WaitOutcome::Timeout => {
                                let (status, error_code, client_code) = if has_later_hop {
                                    (504, "timeout", "timeout")
                                } else {
                                    (502, "upstream_stream_error", "upstream_stream_error")
                                };
                                let message = "The upstream stream ended before producing data";
                                let mut outcome = RoutingAttemptOutcome::from_previous(
                                    status,
                                    attempt_started.elapsed(),
                                    true,
                                    previous_failure.as_ref(),
                                )
                                .with_same_target_attempt(same_target_attempt);
                                if !can_retry_same_target(status, same_target_attempt) {
                                    outcome.retry_after_until =
                                        slow_skip_until(is_fallback_hop, has_later_hop);
                                }
                                outcome.error_code = Some(error_code.into());
                                outcome.error_message = Some(message.into());
                                record_routing_attempt(
                                    &core,
                                    &evaluation,
                                    &request_id,
                                    &target.id,
                                    outcome,
                                )
                                .await;
                                last_error = Some(request_error(
                                    public_protocol,
                                    StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
                                    client_code,
                                    message,
                                ));
                                last_error_target_id = Some(target.id.clone());
                                last_error_target_name = Some(target.name.clone());
                                last_error_detail =
                                    Some((status, Some(error_code.into()), Some(message.into())));
                                previous_failure = Some((status, Some(error_code.into())));
                                match recover_failed_hop(
                                    &cancel,
                                    FailedHop {
                                        inflight: inflight.as_ref(),
                                        target_id: &target.id,
                                        target_name: &target.name,
                                        status,
                                        error_code: Some(error_code),
                                        error_message: Some(message),
                                        same_target_attempt,
                                        has_later_hop,
                                        retry_after_until: None,
                                    },
                                )
                                .await
                                {
                                    HopRecovery::RetrySame => continue 'retry,
                                    HopRecovery::NextHop | HopRecovery::Exhausted => continue 'hops,
                                    HopRecovery::Cancelled => {
                                        return cancelled_proxy_response(
                                            &core,
                                            public_protocol,
                                            &request_id,
                                            &api_key_id,
                                            uri.path(),
                                            &alias,
                                            Some(&target.name),
                                            attempts,
                                            started,
                                        )
                                        .await;
                                    }
                                }
                            }
                        };
                        attempt_ttft = Some(attempt_started.elapsed());
                        inflight.as_ref().expect("in-flight guard").apply(
                            InFlightProgress::new(&target.id, &target.name, "streaming")
                                .with_attempt(same_target_attempt)
                                .with_error(
                                    live_error_code.as_deref(),
                                    live_error_message.as_deref(),
                                ),
                        );
                        let stream_guard = inflight.take().expect("in-flight guard");
                        let stream_core = core.clone();
                        let stream_request_id = request_id.clone();
                        let stream_model = alias.clone();
                        let stream_wire_protocol = target.wire_protocol;
                        let stream_kv = kv_context.clone();
                        let stream_runtimes = runtimes.clone();
                        let stream_evaluation = evaluation.clone();
                        let stream_target_id = target.id.clone();
                        let stream_attempt_started = attempt_started;
                        let stream_ttft = attempt_ttft;
                        let stream_previous = previous_failure.clone();
                        let stream_cancel = cancel.clone();
                        let stream = async_stream::stream! {
                            let _inflight = stream_guard;
                            let _permit = local_permit;
                            let mut stream_ok = true;
                            let mut usage_buffer = Vec::new();
                            let mut translator = translated_stream.map(|protocol| StreamTranslator::new(stream_wire_protocol, protocol, &stream_model));
                            if let Some(usage) = extract_sse_usage(&mut usage_buffer, &first_chunk) {
                                let _ = stream_core.store.update_log_usage(&stream_request_id, usage).await;
                            }
                            let first_output = translator.as_mut().map(|translator| Bytes::from(translator.push(&first_chunk))).unwrap_or(first_chunk);
                            if !first_output.is_empty() { yield Ok::<_, std::io::Error>(first_output); }
                            loop {
                                let chunk = tokio::select! {
                                    chunk = upstream_stream.next() => chunk,
                                    _ = stream_cancel.cancelled() => {
                                        stream_ok = false;
                                        break;
                                    }
                                };
                                match chunk {
                                    Some(Ok(chunk)) => {
                                        if let Some(usage) = extract_sse_usage(&mut usage_buffer, &chunk) {
                                            let _ = stream_core.store.update_log_usage(&stream_request_id, usage).await;
                                        }
                                        let output = translator.as_mut().map(|translator| Bytes::from(translator.push(&chunk))).unwrap_or(chunk);
                                        if !output.is_empty() { yield Ok(output); }
                                    }
                                    Some(Err(error)) => {
                                        stream_ok = false;
                                        let mut outcome = RoutingAttemptOutcome::from_previous(
                                            502,
                                            stream_attempt_started.elapsed(),
                                            true,
                                            stream_previous.as_ref(),
                                        );
                                        outcome.ttft = stream_ttft;
                                        outcome.error_code = Some("upstream_stream_error".into());
                                        outcome.error_message = Some("The upstream stream failed after the first chunk".into());
                                        record_routing_attempt(
                                            &stream_core,
                                            &stream_evaluation,
                                            &stream_request_id,
                                            &stream_target_id,
                                            outcome,
                                        ).await;
                                        yield Err(std::io::Error::other(error));
                                        break;
                                    }
                                    None => break,
                                }
                            }
                            if stream_ok {
                                if let Some((runtimes, target, api_key_id, session_id)) = stream_kv {
                                    if let Err(error) = runtimes.save_kv(&target, &api_key_id, &session_id).await {
                                        tracing::warn!(target = %target.id, %error, "KV snapshot save failed");
                                    }
                                }
                                let mut outcome = RoutingAttemptOutcome::from_previous(
                                    status.as_u16(),
                                    stream_attempt_started.elapsed(),
                                    true,
                                    stream_previous.as_ref(),
                                );
                                outcome.ttft = stream_ttft;
                                record_routing_attempt(
                                    &stream_core,
                                    &stream_evaluation,
                                    &stream_request_id,
                                    &stream_target_id,
                                    outcome,
                                ).await;
                            }
                            drop(_permit);
                            if let Some(runtimes) = stream_runtimes.as_ref() {
                                runtimes.reap_over_budget().await;
                                sync_runtime_states(&stream_core, runtimes).await;
                            }
                        };
                        response_from_body(
                            status,
                            content_type,
                            Body::from_stream(stream),
                            &request_id,
                        )
                    } else {
                        let mut content_type =
                            upstream.headers().get(header::CONTENT_TYPE).cloned();
                        let bytes = match wait_with_timeout_or_cancel(
                            attempt_timeout,
                            &cancel,
                            upstream.bytes(),
                        )
                        .await
                        {
                            WaitOutcome::Cancelled => {
                                return cancelled_proxy_response(
                                    &core,
                                    public_protocol,
                                    &request_id,
                                    &api_key_id,
                                    uri.path(),
                                    &alias,
                                    Some(&target.name),
                                    attempts,
                                    started,
                                )
                                .await;
                            }
                            WaitOutcome::Ready(Ok(bytes)) => bytes,
                            WaitOutcome::Ready(Err(_)) | WaitOutcome::Timeout => {
                                let message = "The upstream response body did not complete";
                                let mut outcome = RoutingAttemptOutcome::from_previous(
                                    504,
                                    attempt_started.elapsed(),
                                    false,
                                    previous_failure.as_ref(),
                                )
                                .with_same_target_attempt(same_target_attempt);
                                if !can_retry_same_target(504, same_target_attempt) {
                                    outcome.retry_after_until =
                                        slow_skip_until(is_fallback_hop, has_later_hop);
                                }
                                outcome.error_code = Some("timeout".into());
                                outcome.error_message = Some(message.into());
                                record_routing_attempt(
                                    &core,
                                    &evaluation,
                                    &request_id,
                                    &target.id,
                                    outcome,
                                )
                                .await;
                                last_error = Some(request_error(
                                    public_protocol,
                                    StatusCode::GATEWAY_TIMEOUT,
                                    "upstream_body_timeout",
                                    message,
                                ));
                                last_error_target_id = Some(target.id.clone());
                                last_error_target_name = Some(target.name.clone());
                                last_error_detail =
                                    Some((504, Some("timeout".into()), Some(message.into())));
                                previous_failure = Some((504, Some("timeout".into())));
                                match recover_failed_hop(
                                    &cancel,
                                    FailedHop {
                                        inflight: inflight.as_ref(),
                                        target_id: &target.id,
                                        target_name: &target.name,
                                        status: 504,
                                        error_code: Some("timeout"),
                                        error_message: Some(message),
                                        same_target_attempt,
                                        has_later_hop,
                                        retry_after_until: None,
                                    },
                                )
                                .await
                                {
                                    HopRecovery::RetrySame => continue 'retry,
                                    HopRecovery::NextHop | HopRecovery::Exhausted => continue 'hops,
                                    HopRecovery::Cancelled => {
                                        return cancelled_proxy_response(
                                            &core,
                                            public_protocol,
                                            &request_id,
                                            &api_key_id,
                                            uri.path(),
                                            &alias,
                                            Some(&target.name),
                                            attempts,
                                            started,
                                        )
                                        .await;
                                    }
                                }
                            }
                        };
                        let mut response_bytes = bytes;
                        let extracted = extract_upstream_error(&response_bytes);
                        if !status.is_success() {
                            error_code = extracted.0;
                            error_message = extracted.1;
                        }
                        if let Ok(value) = serde_json::from_slice::<Value>(&response_bytes) {
                            usage = usage_from_value(&value);
                            if status.is_success() {
                                if let Some(protocol) = public_protocol {
                                    let translate = !target.kind.is_uplink()
                                        && (!protocol_matches(protocol, target.wire_protocol)
                                            || !matches!(
                                                emulation,
                                                crate::tool_emulation::ToolEmulation::None
                                            ));
                                    if translate {
                                        match decode_response(target.wire_protocol, &value) {
                                            Ok(mut canonical_response) => {
                                                if !matches!(
                                                    emulation,
                                                    crate::tool_emulation::ToolEmulation::None
                                                ) {
                                                    crate::tool_emulation::salvage_tool_calls(
                                                        &mut canonical_response,
                                                        &tools_for_salvage,
                                                    );
                                                }
                                                if is_stream {
                                                    content_type = Some(HeaderValue::from_static(
                                                        "text/event-stream",
                                                    ));
                                                    let mut translator = StreamTranslator::new(
                                                        target.wire_protocol,
                                                        protocol,
                                                        &alias,
                                                    );
                                                    response_bytes = translator
                                                        .encode_canonical(&canonical_response)
                                                        .into();
                                                } else {
                                                    response_bytes = encode_response(
                                                        protocol,
                                                        &canonical_response,
                                                        &alias,
                                                    )
                                                    .to_string()
                                                    .into();
                                                }
                                            }
                                            Err(error) => {
                                                return protocol_error(
                                                    protocol,
                                                    StatusCode::BAD_GATEWAY,
                                                    "invalid_upstream_response",
                                                    &error.to_string(),
                                                )
                                            }
                                        }
                                    }
                                }
                            } else if let Some(protocol) = public_protocol {
                                if !target.kind.is_uplink()
                                    && !protocol_matches(protocol, target.wire_protocol)
                                {
                                    response_bytes = protocol_error_value(
                                        protocol,
                                        status,
                                        error_code.as_deref().unwrap_or("upstream_error"),
                                        error_message
                                            .as_deref()
                                            .unwrap_or("The upstream rejected the request"),
                                    )
                                    .to_string()
                                    .into();
                                }
                            }
                        } else if status.is_success()
                            && public_protocol.is_some_and(|protocol| {
                                !target.kind.is_uplink()
                                    && !protocol_matches(protocol, target.wire_protocol)
                            })
                        {
                            log_request(
                            &core,
                            LogMetadata {
                                id: &request_id,
                                api_key_id: caller.api_key_id(),
                    directory_user_id: caller.directory_user_id(),
                                endpoint: uri.path(),
                                alias: Some(&alias),
                                target: Some(&target.name),
                                attempts,
                                status: 502,
                                latency_ms: started.elapsed().as_millis() as i64,
                                usage: TokenUsage::default(),
                                error_code: Some("invalid_upstream_response"),
                                error_message: Some(
                                    "The upstream returned a non-JSON response that cannot be translated",
                                ),
                            },
                        )
                        .await;
                            return protocol_error(
                            public_protocol.unwrap(),
                            StatusCode::BAD_GATEWAY,
                            "invalid_upstream_response",
                            "The upstream returned a non-JSON response that cannot be translated",
                        );
                        }
                        if status.is_success() {
                            if let Some((runtimes, target, api_key_id, session_id)) =
                                kv_context.as_ref()
                            {
                                if let Err(error) =
                                    runtimes.save_kv(target, api_key_id, session_id).await
                                {
                                    tracing::warn!(target = %target.id, %error, "KV snapshot save failed");
                                }
                            }
                        }
                        drop(local_permit);
                        if let Some(runtimes) = runtimes.as_ref() {
                            runtimes.reap_over_budget().await;
                            sync_runtime_states(&core, runtimes).await;
                        }
                        response_from_body(
                            status,
                            content_type,
                            Body::from(response_bytes),
                            &request_id,
                        )
                    };
                    let hop_outcome = RoutingAttemptOutcome {
                        status: status.as_u16(),
                        transient_failure: is_transient_status(status.as_u16()),
                        retry_after_until,
                        latency: attempt_started.elapsed(),
                        ttft: attempt_ttft,
                        streaming: is_stream,
                        error_code: error_code.clone(),
                        error_message: error_message.clone(),
                        previous_status: previous_failure.as_ref().map(|(status, _)| *status),
                        previous_error_code: previous_failure
                            .as_ref()
                            .and_then(|(_, code)| code.clone()),
                        same_target_attempt,
                    };
                    if !status.is_success() && is_fallback_status(status.as_u16()) {
                        last_error_detail =
                            Some((status.as_u16(), error_code.clone(), error_message.clone()));
                        previous_failure = Some((status.as_u16(), error_code.clone()));
                        match recover_failed_hop(
                            &cancel,
                            FailedHop {
                                inflight: inflight.as_ref(),
                                target_id: &target.id,
                                target_name: &target.name,
                                status: status.as_u16(),
                                error_code: error_code.as_deref(),
                                error_message: error_message.as_deref(),
                                same_target_attempt,
                                has_later_hop,
                                retry_after_until,
                            },
                        )
                        .await
                        {
                            HopRecovery::RetrySame => {
                                record_routing_attempt(
                                    &core,
                                    &evaluation,
                                    &request_id,
                                    &target.id,
                                    hop_outcome,
                                )
                                .await;
                                last_error = Some(response);
                                last_error_target_id = Some(target.id.clone());
                                last_error_target_name = Some(target.name.clone());
                                continue 'retry;
                            }
                            HopRecovery::NextHop => {
                                record_routing_attempt(
                                    &core,
                                    &evaluation,
                                    &request_id,
                                    &target.id,
                                    hop_outcome,
                                )
                                .await;
                                last_error = Some(response);
                                last_error_target_id = Some(target.id.clone());
                                last_error_target_name = Some(target.name.clone());
                                continue 'hops;
                            }
                            HopRecovery::Cancelled => {
                                return cancelled_proxy_response(
                                    &core,
                                    public_protocol,
                                    &request_id,
                                    &api_key_id,
                                    uri.path(),
                                    &alias,
                                    Some(&target.name),
                                    attempts,
                                    started,
                                )
                                .await;
                            }
                            HopRecovery::Exhausted => {}
                        }
                    }
                    log_request(
                        &core,
                        LogMetadata {
                            id: &request_id,
                            api_key_id: caller.api_key_id(),
                            directory_user_id: caller.directory_user_id(),
                            endpoint: uri.path(),
                            alias: Some(&alias),
                            target: Some(&target.name),
                            attempts,
                            status: status.as_u16(),
                            latency_ms: started.elapsed().as_millis() as i64,
                            usage,
                            error_code: error_code.as_deref(),
                            error_message: error_message.as_deref(),
                        },
                    )
                    .await;
                    if !is_stream {
                        record_routing_attempt(
                            &core,
                            &evaluation,
                            &request_id,
                            &target.id,
                            hop_outcome.clone(),
                        )
                        .await;
                    }
                    return with_routing_headers(response, &evaluation, &target.id, &hop_outcome);
                }
                WaitOutcome::Ready(Err(error)) => {
                    last_error = Some(request_error(
                        public_protocol,
                        StatusCode::BAD_GATEWAY,
                        "upstream_unavailable",
                        "The selected backend could not be reached",
                    ));
                    last_error_target_id = Some(target.id.clone());
                    last_error_target_name = Some(target.name.clone());
                    let network_code = if error.is_timeout() {
                        "timeout"
                    } else {
                        "network_error"
                    };
                    last_error_detail = Some((
                        502,
                        Some(network_code.into()),
                        Some("The selected backend could not be reached".into()),
                    ));
                    record_routing_attempt(
                        &core,
                        &evaluation,
                        &request_id,
                        &target.id,
                        RoutingAttemptOutcome {
                            status: 502,
                            transient_failure: true,
                            retry_after_until: None,
                            latency: attempt_started.elapsed(),
                            ttft: None,
                            streaming: is_stream,
                            error_code: Some(network_code.into()),
                            error_message: Some("The selected backend could not be reached".into()),
                            previous_status: previous_failure.as_ref().map(|(status, _)| *status),
                            previous_error_code: previous_failure
                                .as_ref()
                                .and_then(|(_, code)| code.clone()),
                            same_target_attempt,
                        },
                    )
                    .await;
                    previous_failure = Some((502, Some(network_code.into())));
                    match recover_failed_hop(
                        &cancel,
                        FailedHop {
                            inflight: inflight.as_ref(),
                            target_id: &target.id,
                            target_name: &target.name,
                            status: 502,
                            error_code: Some(network_code),
                            error_message: Some("The selected backend could not be reached"),
                            same_target_attempt,
                            has_later_hop,
                            retry_after_until: None,
                        },
                    )
                    .await
                    {
                        HopRecovery::RetrySame => continue 'retry,
                        HopRecovery::NextHop => continue 'hops,
                        HopRecovery::Cancelled => {
                            return cancelled_proxy_response(
                                &core,
                                public_protocol,
                                &request_id,
                                &api_key_id,
                                uri.path(),
                                &alias,
                                Some(&target.name),
                                attempts,
                                started,
                            )
                            .await;
                        }
                        HopRecovery::Exhausted => {
                            request_logged = true;
                            log_request(
                                &core,
                                LogMetadata {
                                    id: &request_id,
                                    api_key_id: caller.api_key_id(),
                                    directory_user_id: caller.directory_user_id(),
                                    endpoint: uri.path(),
                                    alias: Some(&alias),
                                    target: Some(&target.name),
                                    attempts,
                                    status: 502,
                                    latency_ms: started.elapsed().as_millis() as i64,
                                    usage: TokenUsage::default(),
                                    error_code: Some(network_code),
                                    error_message: Some(
                                        "The selected backend could not be reached",
                                    ),
                                },
                            )
                            .await;
                            continue 'hops;
                        }
                    }
                }
                WaitOutcome::Timeout => {
                    last_error = Some(request_error(
                        public_protocol,
                        StatusCode::GATEWAY_TIMEOUT,
                        "timeout",
                        "The selected backend timed out",
                    ));
                    last_error_target_id = Some(target.id.clone());
                    last_error_target_name = Some(target.name.clone());
                    last_error_detail = Some((
                        504,
                        Some("timeout".into()),
                        Some("The selected backend timed out".into()),
                    ));
                    let retrying = can_retry_same_target(504, same_target_attempt);
                    record_routing_attempt(
                        &core,
                        &evaluation,
                        &request_id,
                        &target.id,
                        RoutingAttemptOutcome {
                            status: 504,
                            transient_failure: true,
                            retry_after_until: if retrying {
                                None
                            } else {
                                slow_skip_until(is_fallback_hop, has_later_hop)
                            },
                            latency: attempt_started.elapsed(),
                            ttft: None,
                            streaming: is_stream,
                            error_code: Some("timeout".into()),
                            error_message: Some("The selected backend timed out".into()),
                            previous_status: previous_failure.as_ref().map(|(status, _)| *status),
                            previous_error_code: previous_failure
                                .as_ref()
                                .and_then(|(_, code)| code.clone()),
                            same_target_attempt,
                        },
                    )
                    .await;
                    previous_failure = Some((504, Some("timeout".into())));
                    match recover_failed_hop(
                        &cancel,
                        FailedHop {
                            inflight: inflight.as_ref(),
                            target_id: &target.id,
                            target_name: &target.name,
                            status: 504,
                            error_code: Some("timeout"),
                            error_message: Some("The selected backend timed out"),
                            same_target_attempt,
                            has_later_hop,
                            retry_after_until: None,
                        },
                    )
                    .await
                    {
                        HopRecovery::RetrySame => continue 'retry,
                        HopRecovery::NextHop => continue 'hops,
                        HopRecovery::Cancelled => {
                            return cancelled_proxy_response(
                                &core,
                                public_protocol,
                                &request_id,
                                &api_key_id,
                                uri.path(),
                                &alias,
                                Some(&target.name),
                                attempts,
                                started,
                            )
                            .await;
                        }
                        HopRecovery::Exhausted => {
                            request_logged = true;
                            log_request(
                                &core,
                                LogMetadata {
                                    id: &request_id,
                                    api_key_id: caller.api_key_id(),
                                    directory_user_id: caller.directory_user_id(),
                                    endpoint: uri.path(),
                                    alias: Some(&alias),
                                    target: Some(&target.name),
                                    attempts,
                                    status: 504,
                                    latency_ms: started.elapsed().as_millis() as i64,
                                    usage: TokenUsage::default(),
                                    error_code: Some("timeout"),
                                    error_message: Some("The selected backend timed out"),
                                },
                            )
                            .await;
                            continue 'hops;
                        }
                    }
                }
            }
        }
    }
    if let Some(error) = last_error {
        if !request_logged {
            let (status, error_code, error_message) = last_error_detail
                .as_ref()
                .cloned()
                .unwrap_or((502, None, None));
            log_request(
                &core,
                LogMetadata {
                    id: &request_id,
                    api_key_id: caller.api_key_id(),
                    directory_user_id: caller.directory_user_id(),
                    endpoint: uri.path(),
                    alias: Some(&alias),
                    target: last_error_target_name.as_deref(),
                    attempts,
                    status,
                    latency_ms: started.elapsed().as_millis() as i64,
                    usage: TokenUsage::default(),
                    error_code: error_code.as_deref(),
                    error_message: error_message.as_deref(),
                },
            )
            .await;
        }
        let outcome = last_error_detail
            .as_ref()
            .map(|(status, code, message)| RoutingAttemptOutcome {
                error_code: code.clone(),
                error_message: message.clone(),
                ..RoutingAttemptOutcome::from_previous(*status, Duration::ZERO, false, None)
            })
            .unwrap_or_else(|| {
                RoutingAttemptOutcome::from_previous(502, Duration::ZERO, false, None)
            });
        return with_routing_headers(
            error,
            &evaluation,
            last_error_target_id.as_deref().unwrap_or("none"),
            &outcome,
        );
    }
    if let (Some(protocol), Some(error)) = (public_protocol, last_translation_error) {
        return protocol_error(
            protocol,
            StatusCode::BAD_REQUEST,
            "unsupported_translation",
            &error,
        );
    }
    if let Some(error) = last_capability_error {
        return request_error(
            public_protocol,
            StatusCode::BAD_REQUEST,
            "unsupported_capability",
            &error,
        );
    }
    request_error(
        public_protocol,
        StatusCode::SERVICE_UNAVAILABLE,
        "no_available_target",
        "No configured target is currently available",
    )
}

async fn advertised_routes(core: &AppCore) -> anyhow::Result<Vec<crate::domain::ModelRoute>> {
    advertised_public_models(&core.store).await
}

#[derive(Clone)]
struct RoutingAttemptOutcome {
    status: u16,
    transient_failure: bool,
    retry_after_until: Option<chrono::DateTime<Utc>>,
    latency: Duration,
    ttft: Option<Duration>,
    streaming: bool,
    error_code: Option<String>,
    error_message: Option<String>,
    previous_status: Option<u16>,
    previous_error_code: Option<String>,
    same_target_attempt: u32,
}

impl RoutingAttemptOutcome {
    fn from_previous(
        status: u16,
        latency: Duration,
        streaming: bool,
        previous: Option<&(u16, Option<String>)>,
    ) -> Self {
        Self {
            status,
            transient_failure: is_transient_status(status),
            retry_after_until: None,
            latency,
            ttft: None,
            streaming,
            error_code: None,
            error_message: None,
            previous_status: previous.map(|(status, _)| *status),
            previous_error_code: previous.and_then(|(_, code)| code.clone()),
            same_target_attempt: 1,
        }
    }

    fn with_same_target_attempt(mut self, attempt: u32) -> Self {
        self.same_target_attempt = attempt;
        self
    }
}

enum HopRecovery {
    RetrySame,
    NextHop,
    Exhausted,
    Cancelled,
}

fn same_target_retry_delay(retry_after_until: Option<chrono::DateTime<Utc>>) -> Duration {
    let default = Duration::from_millis(SAME_TARGET_RETRY_DELAY_MS);
    let max = Duration::from_millis(SAME_TARGET_RETRY_MAX_WAIT_MS);
    let Some(until) = retry_after_until else {
        return default;
    };
    let Ok(wait) = (until - Utc::now()).to_std() else {
        return default;
    };
    if wait.is_zero() || wait > max {
        default
    } else {
        wait
    }
}

async fn wait_for_same_target_retry(
    cancel: &CancellationToken,
    retry_after_until: Option<chrono::DateTime<Utc>>,
) -> bool {
    let delay = same_target_retry_delay(retry_after_until);
    tokio::select! {
        _ = cancel.cancelled() => false,
        _ = tokio::time::sleep(delay) => true,
    }
}

struct FailedHop<'a> {
    inflight: Option<&'a InFlightGuard>,
    target_id: &'a str,
    target_name: &'a str,
    status: u16,
    error_code: Option<&'a str>,
    error_message: Option<&'a str>,
    same_target_attempt: u32,
    has_later_hop: bool,
    retry_after_until: Option<chrono::DateTime<Utc>>,
}

async fn recover_failed_hop(cancel: &CancellationToken, hop: FailedHop<'_>) -> HopRecovery {
    if can_retry_same_target(hop.status, hop.same_target_attempt) {
        tracing::warn!(
            target = %hop.target_id,
            status = hop.status,
            code = hop.error_code.unwrap_or("-"),
            message = hop.error_message.unwrap_or("-"),
            attempt = hop.same_target_attempt,
            "upstream error; retrying same target"
        );
        if let Some(inflight) = hop.inflight {
            inflight.apply(
                InFlightProgress::new(hop.target_id, hop.target_name, "retrying")
                    .with_attempt(hop.same_target_attempt + 1)
                    .with_error(hop.error_code, hop.error_message),
            );
        }
        if !wait_for_same_target_retry(cancel, hop.retry_after_until).await {
            return HopRecovery::Cancelled;
        }
        return HopRecovery::RetrySame;
    }
    if hop.has_later_hop {
        tracing::warn!(
            target = %hop.target_id,
            status = hop.status,
            code = hop.error_code.unwrap_or("-"),
            message = hop.error_message.unwrap_or("-"),
            "upstream error; trying fallback"
        );
        if let Some(inflight) = hop.inflight {
            inflight.apply(
                InFlightProgress::new(hop.target_id, hop.target_name, "rerouting")
                    .with_attempt(hop.same_target_attempt)
                    .with_error(hop.error_code, hop.error_message),
            );
        }
        return HopRecovery::NextHop;
    }
    HopRecovery::Exhausted
}

fn slow_skip_until(is_fallback_hop: bool, has_later_hop: bool) -> Option<chrono::DateTime<Utc>> {
    (!is_fallback_hop && has_later_hop)
        .then(|| Utc::now() + chrono::Duration::seconds(SLOW_WINDOW_SECS))
}

#[allow(clippy::too_many_arguments)]
async fn record_skipped_hop(
    core: &AppCore,
    evaluation: &RoutingEvaluation,
    request_id: &str,
    target_id: &str,
    attempt_started: Instant,
    streaming: bool,
    previous_failure: &mut Option<(u16, Option<String>)>,
    status: u16,
    error_code: &str,
    error_message: String,
    same_target_attempt: u32,
) {
    let mut outcome = RoutingAttemptOutcome::from_previous(
        status,
        attempt_started.elapsed(),
        streaming,
        previous_failure.as_ref(),
    )
    .with_same_target_attempt(same_target_attempt);
    outcome.error_code = Some(error_code.into());
    outcome.error_message = Some(truncate_error_message(&error_message));
    record_routing_attempt(core, evaluation, request_id, target_id, outcome).await;
    *previous_failure = Some((status, Some(error_code.into())));
}

async fn record_routing_attempt(
    core: &AppCore,
    evaluation: &RoutingEvaluation,
    request_id: &str,
    target_id: &str,
    outcome: RoutingAttemptOutcome,
) {
    let ranked = evaluation
        .decision
        .ranked
        .iter()
        .find(|candidate| candidate.target_id == target_id);
    let reason = routing_attempt_reason(evaluation, target_id, &outcome);
    let _ = core
        .store
        .insert_routing_attempt(&RoutingAttemptRecord {
            id: Uuid::new_v4().to_string(),
            request_id: request_id.into(),
            created_at: Utc::now(),
            alias: evaluation.alias.clone(),
            task: evaluation.task.clone(),
            task_source: evaluation.task_source.clone(),
            target_id: target_id.into(),
            routing_mode: evaluation.mode.clone(),
            status: outcome.status,
            transient_failure: outcome.transient_failure,
            retry_after_until: outcome.retry_after_until,
            latency_ms: outcome.latency.as_millis() as u64,
            ttft_ms: outcome.ttft.map(|value| value.as_millis() as u64),
            streaming: outcome.streaming,
            input_tokens: Some(evaluation.estimated_input_tokens),
            output_tokens: None,
            estimated_cost_usd: ranked.and_then(|candidate| candidate.estimated_cost_usd),
            cost_verified: ranked.is_some_and(|candidate| candidate.cost_verified),
            score: ranked.map(|candidate| candidate.score.clone()),
            reason,
        })
        .await;
}

fn routing_attempt_reason(
    evaluation: &RoutingEvaluation,
    target_id: &str,
    outcome: &RoutingAttemptOutcome,
) -> String {
    let mut parts = Vec::new();
    if outcome.same_target_attempt > 1 {
        let total = SAME_TARGET_RETRY_LIMIT.saturating_add(1);
        let mut retry = format!("retry {}/{total} same target", outcome.same_target_attempt);
        if let Some(status) = outcome.previous_status {
            retry.push_str(&format!(" after {status}"));
            if let Some(code) = outcome.previous_error_code.as_deref() {
                retry.push(' ');
                retry.push_str(code);
            }
        }
        parts.push(retry);
    } else if let Some(status) = outcome.previous_status {
        let mut fallback = format!("fallback after {status}");
        if let Some(code) = outcome.previous_error_code.as_deref() {
            fallback.push(' ');
            fallback.push_str(code);
        }
        parts.push(fallback);
    }
    parts.push(selection_reason(evaluation, target_id));
    parts.push(task_reason(evaluation));
    if let Some(shadow) = evaluation.shadow_target_id.as_deref() {
        parts.push(format!("shadow={shadow}"));
    }
    if let Some(ranked) = evaluation
        .decision
        .ranked
        .iter()
        .find(|candidate| candidate.target_id == target_id)
    {
        let score = &ranked.score;
        parts.push(format!(
            "score={:.3} q={:.2} c={:.2} l={:.2} r={:.2} loc={:.2}",
            score.total,
            score.quality,
            score.cost,
            score.latency,
            score.reliability,
            score.locality
        ));
    }
    if outcome.status >= 400 {
        let mut error = format!("error {}", outcome.status);
        if let Some(code) = outcome.error_code.as_deref() {
            error.push(' ');
            error.push_str(code);
        }
        if let Some(message) = outcome.error_message.as_deref() {
            error.push_str(": ");
            error.push_str(message);
        }
        parts.push(error);
    }
    if !evaluation.decision.excluded.is_empty() {
        parts.push(format!(
            "skipped {}",
            evaluation
                .decision
                .excluded
                .iter()
                .map(|candidate| format!("{}:{}", candidate.target_id, candidate.reason))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    parts.join("; ")
}

fn selection_reason(evaluation: &RoutingEvaluation, target_id: &str) -> String {
    let total = evaluation.ordered_target_ids.len().max(1);
    let hop = evaluation
        .ordered_target_ids
        .iter()
        .position(|id| id == target_id)
        .map(|index| index + 1)
        .unwrap_or(total);
    if evaluation.mode == "adaptive" {
        if let Some(rank) = evaluation
            .decision
            .ranked
            .iter()
            .position(|candidate| candidate.target_id == target_id)
        {
            return format!(
                "adaptive rank {}/{}",
                rank + 1,
                evaluation.decision.ranked.len()
            );
        }
        return format!("adaptive fallback hop {hop}/{total}");
    }
    if evaluation.is_fallback_hop(target_id) {
        return format!("failover hop {hop}/{total}");
    }
    format!("performance hop {hop}/{total}")
}

fn task_reason(evaluation: &RoutingEvaluation) -> String {
    match evaluation.task_source.as_str() {
        "header" => format!("task={} via header", evaluation.task),
        "rule" => format!(
            "task={} via rule {}",
            evaluation.task,
            evaluation.task_rule_id.as_deref().unwrap_or("unknown")
        ),
        _ => format!("task={} via default", evaluation.task),
    }
}

const ERROR_MESSAGE_LIMIT: usize = 500;

fn truncate_error_message(text: &str) -> String {
    let mut chars = text.chars();
    let mut truncated: String = chars.by_ref().take(ERROR_MESSAGE_LIMIT).collect();
    if chars.next().is_some() {
        truncated.push('…');
    }
    truncated
}

fn extract_upstream_error(bytes: &[u8]) -> (Option<String>, Option<String>) {
    if let Ok(value) = serde_json::from_slice::<Value>(bytes) {
        let code = json_error_code(&value);
        let message = value
            .pointer("/error/message")
            .or_else(|| value.get("message"))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(truncate_error_message);
        if code.is_some() || message.is_some() {
            return (code, message);
        }
    }
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        (None, None)
    } else {
        (None, Some(truncate_error_message(trimmed)))
    }
}

fn json_error_code(value: &Value) -> Option<String> {
    value
        .pointer("/error/code")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            value
                .pointer("/error/type")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        })
        .or_else(|| {
            value
                .pointer("/error/status")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        })
        .or_else(|| {
            value
                .get("type")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        })
        .or_else(|| {
            value.pointer("/error/code").and_then(|node| match node {
                Value::Number(number) => Some(number.to_string()),
                _ => None,
            })
        })
}

fn header_safe_reason(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii() && !ch.is_control() {
                ch
            } else {
                ' '
            }
        })
        .collect();
    sanitized
        .chars()
        .take(1024)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn retry_after_deadline(headers: &HeaderMap) -> Option<chrono::DateTime<Utc>> {
    let value = headers.get(header::RETRY_AFTER)?.to_str().ok()?.trim();
    parse_reset_deadline(value)
}

fn rate_limit_until(headers: &HeaderMap, status: u16) -> Option<chrono::DateTime<Utc>> {
    let from_retry_after = retry_after_deadline(headers);
    let remaining_zero = rate_limit_remaining_is_zero(headers);
    let from_reset = [
        "x-ratelimit-reset-requests",
        "x-ratelimit-reset",
        "anthropic-ratelimit-requests-reset",
    ]
    .into_iter()
    .find_map(|name| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_reset_deadline)
    });
    if status == 429 {
        return Some(
            from_retry_after
                .or(from_reset)
                .unwrap_or_else(|| Utc::now() + chrono::Duration::seconds(RATE_LIMIT_DEFAULT_SECS)),
        );
    }
    if remaining_zero {
        return from_reset.or(from_retry_after);
    }
    from_retry_after
}

fn rate_limit_remaining_is_zero(headers: &HeaderMap) -> bool {
    [
        "x-ratelimit-remaining-requests",
        "x-ratelimit-remaining",
        "anthropic-ratelimit-requests-remaining",
    ]
    .into_iter()
    .filter_map(|name| headers.get(name).and_then(|value| value.to_str().ok()))
    .any(|value| {
        value
            .trim()
            .parse::<f64>()
            .is_ok_and(|remaining| remaining <= 0.0)
    })
}

fn parse_reset_deadline(value: &str) -> Option<chrono::DateTime<Utc>> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<i64>() {
        if seconds >= 1_000_000_000 {
            return chrono::DateTime::from_timestamp(seconds, 0)
                .map(|value| value.min(Utc::now() + chrono::Duration::minutes(5)));
        }
        return Some(Utc::now() + chrono::Duration::seconds(seconds.clamp(0, 300)));
    }
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some(
            parsed
                .with_timezone(&Utc)
                .min(Utc::now() + chrono::Duration::minutes(5)),
        );
    }
    chrono::DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
        .map(|value| value.min(Utc::now() + chrono::Duration::minutes(5)))
}

fn with_routing_headers(
    mut response: Response<Body>,
    evaluation: &RoutingEvaluation,
    target_id: &str,
    outcome: &RoutingAttemptOutcome,
) -> Response<Body> {
    let reason = header_safe_reason(&routing_attempt_reason(evaluation, target_id, outcome));
    for (name, value) in [
        ("x-local-ai-task", evaluation.task.as_str()),
        ("x-local-ai-target", target_id),
        ("x-local-ai-routing-mode", evaluation.mode.as_str()),
        ("x-local-ai-routing-reason", reason.as_str()),
    ] {
        if let Ok(value) = HeaderValue::from_str(value) {
            response
                .headers_mut()
                .insert(axum::http::HeaderName::from_static(name), value);
        }
    }
    if crate::uplink::is_uplink_target_id(target_id) {
        response.headers_mut().insert(
            axum::http::HeaderName::from_static("x-local-ai-hop"),
            HeaderValue::from_static("uplink"),
        );
    }
    if crate::publish::is_replica_target_id(target_id) {
        response.headers_mut().insert(
            axum::http::HeaderName::from_static("x-local-ai-hop"),
            HeaderValue::from_static("replica"),
        );
    }
    response
}

fn extract_multipart_model(body: &[u8]) -> Option<&str> {
    let marker = b"name=\"model\"";
    let marker_start = find_bytes(body, marker)? + marker.len();
    let content_start = find_bytes(&body[marker_start..], b"\r\n\r\n")? + marker_start + 4;
    let content_end = find_bytes(&body[content_start..], b"\r\n")? + content_start;
    std::str::from_utf8(&body[content_start..content_end]).ok()
}

fn rewrite_multipart_model(body: &[u8], model: &str) -> Vec<u8> {
    let Some(current) = extract_multipart_model(body) else {
        return body.to_vec();
    };
    let offset = current.as_ptr() as usize - body.as_ptr() as usize;
    let mut rewritten = Vec::with_capacity(body.len() + model.len().saturating_sub(current.len()));
    rewritten.extend_from_slice(&body[..offset]);
    rewritten.extend_from_slice(model.as_bytes());
    rewritten.extend_from_slice(&body[offset + current.len()..]);
    rewritten
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn join_api_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with("/v1") {
        format!("{base}/{}", path.trim_start_matches("/v1/"))
    } else {
        format!("{base}/{}", path.trim_start_matches('/'))
    }
}

fn usage_from_value(value: &Value) -> TokenUsage {
    let usage = value
        .get("usage")
        .or_else(|| {
            value
                .get("response")
                .and_then(|response| response.get("usage"))
        })
        .or_else(|| value.get("usageMetadata"));
    let Some(usage) = usage else {
        return TokenUsage::default();
    };
    let input_tokens = usage_i64(
        usage,
        &["prompt_tokens", "input_tokens", "promptTokenCount"],
    );
    let output_tokens = usage_i64(
        usage,
        &["completion_tokens", "output_tokens", "candidatesTokenCount"],
    );
    TokenUsage {
        input_tokens,
        output_tokens,
        cache_read_tokens: cache_read_tokens(usage),
        cache_write_tokens: cache_write_tokens(usage),
    }
}

fn usage_i64(usage: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|key| usage.get(*key).and_then(Value::as_i64))
}

fn cache_read_tokens(usage: &Value) -> Option<i64> {
    usage
        .pointer("/prompt_tokens_details/cached_tokens")
        .or_else(|| usage.pointer("/input_tokens_details/cached_tokens"))
        .and_then(Value::as_i64)
        .or_else(|| {
            usage_i64(
                usage,
                &[
                    "cache_read_input_tokens",
                    "cachedContentTokenCount",
                    "cached_tokens",
                ],
            )
        })
}

fn cache_write_tokens(usage: &Value) -> Option<i64> {
    if let Some(total) = usage_i64(
        usage,
        &["cache_creation_input_tokens", "cache_creation_tokens"],
    ) {
        return Some(total);
    }
    let creation = usage.get("cache_creation")?;
    if let Some(total) = creation.as_i64() {
        return Some(total);
    }
    let five = creation
        .get("ephemeral_5m_input_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let hour = creation
        .get("ephemeral_1h_input_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    Some(five + hour).filter(|value| *value > 0)
}

fn extract_sse_usage(buffer: &mut Vec<u8>, chunk: &[u8]) -> Option<TokenUsage> {
    buffer.extend_from_slice(chunk);
    let mut found = None;
    while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
        let mut line = buffer.drain(..=newline).collect::<Vec<_>>();
        while matches!(line.last(), Some(b'\n' | b'\r')) {
            line.pop();
        }
        let Ok(line) = std::str::from_utf8(&line) else {
            continue;
        };
        let Some(payload) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if payload == "[DONE]" {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(payload) {
            let usage = usage_from_value(&value);
            if usage.is_present() {
                found = Some(usage);
            }
        }
    }
    if buffer.len() > 1024 * 1024 {
        buffer.clear();
    }
    found
}

fn endpoint_capability(path: &str) -> &'static str {
    match path {
        "/v1/chat/completions" | "/v1/responses" | "/v1/messages" | "/v1/completions" => "chat",
        path if path.starts_with("/v1beta/models/") => "chat",
        "/v1/embeddings" => "embeddings",
        path if path.starts_with("/v1/images/") => "images",
        "/v1/audio/speech" => "speech",
        path if path.starts_with("/v1/audio/") => "audio",
        "/v1/moderations" => "moderation",
        _ => "unknown",
    }
}

fn route_supports_capability(route: &crate::domain::ModelRoute, capability: &str) -> bool {
    route.capabilities.iter().any(|item| item == capability)
        || (capability == "speech" && route.capabilities.iter().any(|item| item == "audio"))
}

fn target_supports_capability(target: &crate::storage::ModelTarget, capability: &str) -> bool {
    if capability == "tools"
        && target.kind == crate::domain::TargetKind::Mlx
        && !crate::tool_emulation::force_tool_support(target)
    {
        return false;
    }
    supports_capability(target.kind, &target.capabilities, capability)
}

fn validate_local_image_request(payload: &Value) -> anyhow::Result<()> {
    if payload.get("n").and_then(Value::as_u64).unwrap_or(1) != 1 {
        anyhow::bail!("local image generation only supports n=1");
    }
    if let Some(format) = payload.get("response_format").and_then(Value::as_str) {
        if format != "b64_json" {
            anyhow::bail!("local image generation only supports response_format=b64_json");
        }
    }
    for field in [
        "quality",
        "style",
        "background",
        "moderation",
        "output_format",
        "output_compression",
        "user",
        "partial_images",
        "stream",
        "mask",
        "image",
    ] {
        if payload.get(field).is_some() {
            anyhow::bail!("local image generation does not support `{field}`");
        }
    }
    Ok(())
}

fn validate_local_speech_request(payload: &Value) -> anyhow::Result<()> {
    if let Some(format) = payload.get("response_format").and_then(Value::as_str) {
        if !matches!(format, "wav" | "pcm") {
            anyhow::bail!("local speech only supports wav and pcm");
        }
    }
    Ok(())
}

async fn authenticated_caller(core: &AppCore, headers: &HeaderMap) -> Option<GatewayCaller> {
    let candidate = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok())
        })
        .or_else(|| {
            headers
                .get("x-goog-api-key")
                .and_then(|value| value.to_str().ok())
        });
    if let Some(id) = core.authorized_token(candidate).await {
        return Some(GatewayCaller::LocalApiKey { id });
    }
    if crate::publish::authenticate_replica_inbound(core, candidate).await {
        return Some(GatewayCaller::ReplicaInbound);
    }
    crate::uplink::authenticate_token(core, candidate)
        .await
        .map(GatewayCaller::Uplink)
}

#[derive(Clone)]
enum GatewayCaller {
    LocalApiKey { id: String },
    Uplink(crate::uplink::UplinkCaller),
    ReplicaInbound,
}

impl GatewayCaller {
    fn kv_id(&self) -> String {
        match self {
            Self::LocalApiKey { id } => id.clone(),
            Self::Uplink(caller) => format!("uplink:{}", caller.user_id),
            Self::ReplicaInbound => "replica-inbound".into(),
        }
    }

    fn api_key_id(&self) -> Option<&str> {
        match self {
            Self::LocalApiKey { id } => Some(id),
            Self::Uplink(_) | Self::ReplicaInbound => None,
        }
    }

    fn directory_user_id(&self) -> Option<&str> {
        match self {
            Self::Uplink(caller) => Some(caller.user_id.as_str()),
            Self::LocalApiKey { .. } | Self::ReplicaInbound => None,
        }
    }
}

async fn hop_http_client(
    core: &AppCore,
    target: &crate::storage::ModelTarget,
) -> anyhow::Result<reqwest::Client> {
    if target.kind.is_uplink() {
        crate::uplink::parent_http_client(&core.store).await
    } else if target.kind.is_replica() {
        crate::publish::replica_http_client(&core.store, &target.id).await
    } else {
        Ok(core.client.clone())
    }
}

async fn filter_routes_for_caller(
    core: &AppCore,
    caller: &GatewayCaller,
    routes: Vec<crate::domain::ModelRoute>,
) -> Vec<crate::domain::ModelRoute> {
    match caller {
        GatewayCaller::LocalApiKey { .. } => routes,
        GatewayCaller::ReplicaInbound => {
            let offered = crate::publish::offered_local_model_ids(&core.store)
                .await
                .unwrap_or_default();
            routes
                .into_iter()
                .filter(|route| offered.iter().any(|id| id == &route.alias))
                .collect()
        }
        GatewayCaller::Uplink(uplink) => {
            let Ok(Some(user)) = core.store.directory_user(&uplink.user_id).await else {
                return Vec::new();
            };
            let Ok(permissions) = core.store.permissions_for(&user).await else {
                return Vec::new();
            };
            routes
                .into_iter()
                .filter(|route| permissions.allows_model(&route.alias))
                .collect()
        }
    }
}

async fn enforce_uplink_access(
    core: &AppCore,
    caller: &GatewayCaller,
    public_protocol: Option<crate::protocol::PublicProtocol>,
    alias: &str,
) -> Result<(), Response<Body>> {
    let GatewayCaller::Uplink(uplink) = caller else {
        return Ok(());
    };
    let user = match core.store.directory_user(&uplink.user_id).await {
        Ok(Some(user)) => user,
        _ => {
            return Err(request_error(
                public_protocol,
                StatusCode::UNAUTHORIZED,
                "invalid_api_key",
                "Invalid uplink session",
            ))
        }
    };
    let Ok(permissions) = core.store.permissions_for(&user).await else {
        return Err(request_error(
            public_protocol,
            StatusCode::INTERNAL_SERVER_ERROR,
            "storage_error",
            "Unable to read permissions",
        ));
    };
    if !permissions.allows_model(alias) {
        return Err(request_error(
            public_protocol,
            StatusCode::FORBIDDEN,
            "model_not_allowed",
            "This uplink user is not granted that model",
        ));
    }
    let groups = core.store.directory_groups().await.unwrap_or_default();
    let quota = crate::identity::effective_quota(&user, &groups);
    let status = match crate::uplink::quota_status(&core.store, &user).await {
        Ok(status) => status,
        Err(_) => {
            return Err(request_error(
                public_protocol,
                StatusCode::INTERNAL_SERVER_ERROR,
                "storage_error",
                "Unable to read uplink quota",
            ))
        }
    };
    if let Some(message) = crate::uplink::quota_rejection(&quota, &status) {
        return Err(request_error(
            public_protocol,
            StatusCode::TOO_MANY_REQUESTS,
            "uplink_quota_exceeded",
            &message,
        ));
    }
    Ok(())
}

async fn uplink_join(
    State(state): State<GatewayState>,
    Json(request): Json<crate::uplink::JoinRequest>,
) -> Response<Body> {
    match crate::uplink::accept_join(&state.core, request).await {
        Ok(joined) => json_response(
            StatusCode::OK,
            serde_json::to_value(joined).unwrap_or(json!({})),
        ),
        Err(error) => {
            let message = error.to_string();
            let status = if message.contains("cycle") || message.contains("cannot join") {
                StatusCode::CONFLICT
            } else if message.contains("invalid") || message.contains("disabled") {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::BAD_REQUEST
            };
            json_response(
                status,
                json!({"error": {"code": "uplink_join_failed", "message": message}}),
            )
        }
    }
}

async fn uplink_models(State(state): State<GatewayState>, headers: HeaderMap) -> Response<Body> {
    let Some(GatewayCaller::Uplink(caller)) = authenticated_caller(&state.core, &headers).await
    else {
        return openai_error(
            StatusCode::UNAUTHORIZED,
            "invalid_api_key",
            "Invalid uplink session",
        );
    };
    let Ok(Some(user)) = state.core.store.directory_user(&caller.user_id).await else {
        return openai_error(
            StatusCode::UNAUTHORIZED,
            "invalid_api_key",
            "Invalid uplink session",
        );
    };
    match crate::uplink::granted_models(&state.core.store, &user).await {
        Ok(models) => {
            let quota = crate::uplink::quota_status(&state.core.store, &user)
                .await
                .ok();
            let parent_node_id = crate::uplink::node_id(&state.core.store)
                .await
                .unwrap_or_default();
            let may_publish = state
                .core
                .store
                .permissions_for(&user)
                .await
                .map(|permissions| permissions.may_publish)
                .unwrap_or(false);
            json_response(
                StatusCode::OK,
                serde_json::to_value(crate::uplink::JoinResponse {
                    token: String::new(),
                    parent_node_id: parent_node_id.clone(),
                    ancestor_node_ids: crate::uplink::load_parent(&state.core.store)
                        .await
                        .ok()
                        .flatten()
                        .map(|parent| parent.ancestor_node_ids)
                        .unwrap_or_else(|| vec![parent_node_id]),
                    user_id: user.id,
                    username: user.username,
                    models,
                    quota: quota.unwrap_or(crate::uplink::QuotaStatus {
                        rpm: None,
                        rpm_used: 0,
                        daily_token_budget: None,
                        daily_tokens_used: 0,
                        daily_usd_budget: None,
                        daily_usd_used: 0.0,
                    }),
                    may_publish,
                })
                .unwrap_or(json!({})),
            )
        }
        Err(error) => openai_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "storage_error",
            &error.to_string(),
        ),
    }
}

async fn uplink_leave(State(state): State<GatewayState>, headers: HeaderMap) -> Response<Body> {
    let candidate = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if let Some(token) = candidate {
        let _ = crate::uplink::revoke_session(&state.core, token).await;
    }
    json_response(StatusCode::OK, json!({"ok": true}))
}

async fn require_uplink(
    core: &AppCore,
    headers: &HeaderMap,
) -> Result<crate::uplink::UplinkCaller, Response<Body>> {
    match authenticated_caller(core, headers).await {
        Some(GatewayCaller::Uplink(caller)) => Ok(caller),
        _ => Err(openai_error(
            StatusCode::UNAUTHORIZED,
            "invalid_api_key",
            "Invalid uplink session",
        )),
    }
}

fn publish_error_response(error: anyhow::Error) -> Response<Body> {
    let message = error.to_string();
    let status = if message.contains("not allowed") {
        StatusCode::FORBIDDEN
    } else if message.contains("not found") || message.contains("unknown") {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::BAD_REQUEST
    };
    json_response(
        status,
        json!({"error": {"code": "publish_failed", "message": message}}),
    )
}

async fn uplink_publish(
    State(state): State<GatewayState>,
    headers: HeaderMap,
    Json(request): Json<crate::publish::AdvertiseRequest>,
) -> Response<Body> {
    let caller = match require_uplink(&state.core, &headers).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    match crate::publish::accept_publish(&state.core, &caller, request).await {
        Ok(model) => json_response(StatusCode::OK, serde_json::to_value(model).unwrap_or(json!({}))),
        Err(error) => publish_error_response(error),
    }
}

async fn uplink_unpublish(
    State(state): State<GatewayState>,
    headers: HeaderMap,
    Json(request): Json<serde_json::Value>,
) -> Response<Body> {
    let caller = match require_uplink(&state.core, &headers).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let Some(network_model_id) = request
        .get("network_model_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return publish_error_response(anyhow::anyhow!("network_model_id is required"));
    };
    match crate::publish::accept_unpublish(&state.core, &caller, &network_model_id).await {
        Ok(()) => json_response(StatusCode::OK, json!({"ok": true})),
        Err(error) => publish_error_response(error),
    }
}

async fn uplink_replica_heartbeat(
    State(state): State<GatewayState>,
    headers: HeaderMap,
) -> Response<Body> {
    let caller = match require_uplink(&state.core, &headers).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    match crate::publish::accept_heartbeat(&state.core, &caller).await {
        Ok(replicas) => {
            json_response(StatusCode::OK, serde_json::to_value(replicas).unwrap_or(json!([])))
        }
        Err(error) => publish_error_response(error),
    }
}

async fn uplink_images(State(state): State<GatewayState>, headers: HeaderMap) -> Response<Body> {
    if require_uplink(&state.core, &headers).await.is_err() {
        return openai_error(
            StatusCode::UNAUTHORIZED,
            "invalid_api_key",
            "Invalid uplink session",
        );
    }
    match crate::publish::list_shared_images(&state.core.store).await {
        Ok(images) => json_response(StatusCode::OK, serde_json::to_value(images).unwrap_or(json!([]))),
        Err(error) => publish_error_response(error),
    }
}

async fn uplink_register_image(
    State(state): State<GatewayState>,
    headers: HeaderMap,
    Json(input): Json<crate::publish::RegisterSharedImageInput>,
) -> Response<Body> {
    let caller = match require_uplink(&state.core, &headers).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    match crate::publish::accept_register_image(&state.core, &caller, input).await {
        Ok(image) => json_response(StatusCode::OK, serde_json::to_value(image).unwrap_or(json!({}))),
        Err(error) => publish_error_response(error),
    }
}

async fn uplink_image_blob(
    State(state): State<GatewayState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response<Body> {
    if require_uplink(&state.core, &headers).await.is_err() {
        return openai_error(
            StatusCode::UNAUTHORIZED,
            "invalid_api_key",
            "Invalid uplink session",
        );
    }
    match crate::publish::shared_image_blob(&state.core.store, &id).await {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .body(Body::from(bytes))
            .unwrap_or_else(|_| openai_error(StatusCode::INTERNAL_SERVER_ERROR, "storage_error", "Unable to return catalog blob")),
        Err(error) => publish_error_response(error),
    }
}

async fn uplink_image_blob_upload(
    State(state): State<GatewayState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    let caller = match require_uplink(&state.core, &headers).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    match crate::publish::accept_image_blob(&state.core, &caller, &id, &body).await {
        Ok(image) => json_response(StatusCode::OK, serde_json::to_value(image).unwrap_or(json!({}))),
        Err(error) => publish_error_response(error),
    }
}

async fn uplink_image_installed(
    State(state): State<GatewayState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response<Body> {
    let caller = match require_uplink(&state.core, &headers).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    match crate::publish::mark_image_installed(&state.core.store, &id, &caller.child_node_id).await {
        Ok(()) => json_response(StatusCode::OK, json!({"ok": true})),
        Err(error) => publish_error_response(error),
    }
}

async fn enforce_replica_access(
    core: &AppCore,
    caller: &GatewayCaller,
    public_protocol: Option<crate::protocol::PublicProtocol>,
    alias: &str,
) -> Result<(), Response<Body>> {
    if !matches!(caller, GatewayCaller::ReplicaInbound) {
        return Ok(());
    }
    let offered = match crate::publish::offered_local_model_ids(&core.store).await {
        Ok(ids) => ids,
        Err(_) => {
            return Err(request_error(
                public_protocol,
                StatusCode::INTERNAL_SERVER_ERROR,
                "storage_error",
                "Unable to read published offers",
            ))
        }
    };
    if offered.iter().any(|id| id == alias) {
        return Ok(());
    }
    Err(request_error(
        public_protocol,
        StatusCode::FORBIDDEN,
        "model_not_allowed",
        "This replica session is not allowed to call that model",
    ))
}

fn protocol_for_path(path: &str) -> Option<PublicProtocol> {
    match path {
        "/v1/chat/completions" => Some(PublicProtocol::OpenAiChat),
        "/v1/responses" => Some(PublicProtocol::OpenAiResponses),
        "/v1/messages" => Some(PublicProtocol::Anthropic),
        path if path.starts_with("/v1beta/models/") => Some(PublicProtocol::Gemini),
        _ => None,
    }
}

fn query_has_api_key(uri: &axum::http::Uri) -> bool {
    uri.query().is_some_and(|query| {
        url::form_urlencoded::parse(query.as_bytes()).any(|(name, _)| {
            matches!(
                name.as_ref(),
                "key" | "api_key" | "x-api-key" | "x-goog-api-key"
            )
        })
    })
}

fn validated_session_id(value: Option<&HeaderValue>) -> Result<Option<&str>, &'static str> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .map_err(|_| "X-Local-AI-Session must be valid ASCII")?
        .trim();
    if value.is_empty() || value.len() > 128 {
        return Err("X-Local-AI-Session must contain between 1 and 128 characters");
    }
    Ok(Some(value))
}

fn gemini_path_model(path: &str) -> Option<&str> {
    path.strip_prefix("/v1beta/models/")?
        .split_once(':')
        .map(|(model, _)| model)
}

fn protocol_matches(public: PublicProtocol, wire: WireProtocol) -> bool {
    matches!(
        (public, wire),
        (PublicProtocol::OpenAiChat, WireProtocol::OpenAiChat)
            | (
                PublicProtocol::OpenAiResponses,
                WireProtocol::OpenAiResponses
            )
            | (PublicProtocol::Anthropic, WireProtocol::AnthropicMessages)
            | (PublicProtocol::Gemini, WireProtocol::GeminiGenerateContent)
    )
}

fn text_upstream_path(wire: WireProtocol, model: &str, stream: bool) -> String {
    match wire {
        WireProtocol::OpenAiChat => "/v1/chat/completions".into(),
        WireProtocol::OpenAiResponses => "/v1/responses".into(),
        WireProtocol::AnthropicMessages => "/v1/messages".into(),
        WireProtocol::GeminiGenerateContent => format!(
            "models/{model}:{}",
            if stream {
                "streamGenerateContent?alt=sse"
            } else {
                "generateContent"
            }
        ),
    }
}

enum WaitOutcome<T> {
    Ready(T),
    Timeout,
    Cancelled,
}

async fn wait_with_timeout_or_cancel<T>(
    timeout: Duration,
    cancel: &CancellationToken,
    fut: impl Future<Output = T>,
) -> WaitOutcome<T> {
    tokio::select! {
        _ = cancel.cancelled() => WaitOutcome::Cancelled,
        result = tokio::time::timeout(timeout, fut) => match result {
            Ok(value) => WaitOutcome::Ready(value),
            Err(_) => WaitOutcome::Timeout,
        },
    }
}

fn cancelled_status() -> StatusCode {
    StatusCode::from_u16(499).unwrap_or(StatusCode::REQUEST_TIMEOUT)
}

#[allow(clippy::too_many_arguments)]
async fn cancelled_proxy_response(
    core: &AppCore,
    public_protocol: Option<PublicProtocol>,
    request_id: &str,
    api_key_id: &str,
    endpoint: &str,
    alias: &str,
    target_name: Option<&str>,
    attempts: i64,
    started: Instant,
) -> Response<Body> {
    log_request(
        core,
        LogMetadata {
            id: request_id,
            api_key_id: (!api_key_id.starts_with("uplink:")).then_some(api_key_id),
            directory_user_id: api_key_id.strip_prefix("uplink:"),
            endpoint,
            alias: Some(alias),
            target: target_name,
            attempts,
            status: cancelled_status().as_u16(),
            latency_ms: started.elapsed().as_millis() as i64,
            usage: TokenUsage::default(),
            error_code: Some("cancelled"),
            error_message: Some("The request was cancelled"),
        },
    )
    .await;
    request_error(
        public_protocol,
        cancelled_status(),
        "cancelled",
        "The request was cancelled",
    )
}

fn protocol_error(
    protocol: PublicProtocol,
    status: StatusCode,
    code: &str,
    message: &str,
) -> Response<Body> {
    json_response(
        status,
        protocol_error_value(protocol, status, code, message),
    )
}

fn request_error(
    protocol: Option<PublicProtocol>,
    status: StatusCode,
    code: &str,
    message: &str,
) -> Response<Body> {
    protocol
        .map(|protocol| protocol_error(protocol, status, code, message))
        .unwrap_or_else(|| openai_error(status, code, message))
}

fn protocol_error_value(
    protocol: PublicProtocol,
    status: StatusCode,
    code: &str,
    message: &str,
) -> Value {
    match protocol {
        PublicProtocol::Anthropic => {
            json!({"type":"error","error":{"type":code,"message":message}})
        }
        PublicProtocol::Gemini => {
            json!({"error":{"code":status.as_u16(),"status":code.to_uppercase(),"message":message}})
        }
        _ => {
            json!({"error":{"message":message,"type":"invalid_request_error","param":null,"code":code}})
        }
    }
}

fn unauthorized() -> Response<Body> {
    openai_error(
        StatusCode::UNAUTHORIZED,
        "invalid_api_key",
        "Invalid local API key",
    )
}

fn openai_error(status: StatusCode, code: &str, message: &str) -> Response<Body> {
    json_response(
        status,
        json!({ "error": { "message": message, "type": "invalid_request_error", "param": null, "code": code } }),
    )
}

fn json_response(status: StatusCode, value: Value) -> Response<Body> {
    response_from_body(
        status,
        Some(HeaderValue::from_static("application/json")),
        Body::from(value.to_string()),
        "",
    )
}

fn response_from_body(
    status: StatusCode,
    content_type: Option<HeaderValue>,
    body: Body,
    request_id: &str,
) -> Response<Body> {
    let mut builder = Response::builder()
        .status(status)
        .header("x-request-id", request_id);
    if let Some(content_type) = content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    builder.body(body).expect("valid response")
}

struct LogMetadata<'a> {
    id: &'a str,
    api_key_id: Option<&'a str>,
    directory_user_id: Option<&'a str>,
    endpoint: &'a str,
    alias: Option<&'a str>,
    target: Option<&'a str>,
    attempts: i64,
    status: u16,
    latency_ms: i64,
    usage: TokenUsage,
    error_code: Option<&'a str>,
    error_message: Option<&'a str>,
}

async fn log_request(core: &AppCore, metadata: LogMetadata<'_>) {
    let _ = core
        .store
        .insert_log(&RequestLog {
            id: metadata.id.into(),
            created_at: Utc::now(),
            endpoint: metadata.endpoint.into(),
            alias: metadata.alias.map(str::to_owned),
            target: metadata.target.map(str::to_owned),
            attempts: metadata.attempts,
            status: metadata.status as i64,
            latency_ms: metadata.latency_ms,
            input_tokens: metadata.usage.input_tokens,
            output_tokens: metadata.usage.output_tokens,
            cache_read_tokens: metadata.usage.cache_read_tokens,
            cache_write_tokens: metadata.usage.cache_write_tokens,
            error_code: metadata.error_code.map(str::to_owned),
            error_message: metadata.error_message.map(str::to_owned),
            api_key_id: metadata.api_key_id.map(str::to_owned),
            api_key_name: None,
            directory_user_id: metadata.directory_user_id.map(str::to_owned),
            directory_user_name: None,
            estimated_cost_usd: estimated_logged_cost(core, metadata.target, metadata.usage).await,
        })
        .await;
}

async fn estimated_logged_cost(
    core: &AppCore,
    target_name: Option<&str>,
    usage: TokenUsage,
) -> Option<f64> {
    if !usage.is_present() {
        return None;
    }
    let target_name = target_name?;
    let targets = core.store.targets().await.ok()?;
    let target = targets
        .iter()
        .find(|target| target.name == target_name || target.id == target_name)?;
    let profile = core
        .store
        .target_routing_profile(&target.id)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| crate::routing::TargetRoutingProfile::for_target(target));
    let input = profile.input_price_per_million?;
    let output = profile.output_price_per_million?;
    Some(
        (usage.input_tokens.unwrap_or(0) as f64 * input
            + usage.output_tokens.unwrap_or(0) as f64 * output)
            / 1_000_000.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::AppCore,
        domain::{ModelRoute, RouteRole, RouteTarget, TargetKind},
        providers::AuthMode,
        secrets::{MemorySecrets, SecretStore, LOCAL_API_KEY},
        storage::{ModelTarget, Provider, Store},
    };
    use axum::{
        body::Body,
        http::{HeaderMap, HeaderValue, Request},
        Json,
    };
    use http_body_util::BodyExt;
    use std::{path::PathBuf, sync::Arc};
    use tower::ServiceExt;

    async fn test_app() -> Router {
        let store = Store::memory().await.unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        secrets.set(LOCAL_API_KEY, "test-token").unwrap();
        let core = AppCore::new(store, secrets).unwrap();
        core.migrate_legacy_local_api_key().await.unwrap();
        router(Arc::new(core))
    }

    async fn app_from_store(store: Store) -> Router {
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        secrets.set(LOCAL_API_KEY, "test-token").unwrap();
        let core = AppCore::new(store.clone(), secrets).unwrap();
        core.migrate_legacy_local_api_key().await.unwrap();
        for target in store.targets().await.unwrap() {
            core.local_activity()
                .set_token(&target.id, "runtime-token".into());
        }
        router(Arc::new(core))
    }

    fn sample_target(id: &str, provider_model: &str, runtime_url: Option<String>) -> ModelTarget {
        ModelTarget {
            id: id.into(),
            provider_id: None,
            name: id.into(),
            kind: TargetKind::Gguf,
            provider_model: provider_model.into(),
            local_path: None,
            runtime_url,
            wire_protocol: WireProtocol::OpenAiChat,
            capabilities: vec!["chat".into()],
            enabled: true,
            state: "ready".into(),
            size_bytes: None,
            local: crate::storage::LocalModelMeta::default(),
        }
    }

    #[tokio::test]
    async fn models_rejects_missing_local_token() {
        let response = test_app()
            .await
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&body).contains("invalid_api_key"));
    }

    #[tokio::test]
    async fn models_lists_enabled_targets_and_global_adaptive_without_an_alias() {
        let store = Store::memory().await.unwrap();
        store
            .upsert_target(&sample_target("local-chat", "qwen-3-5-4b", None))
            .await
            .unwrap();
        let app = app_from_store(store).await;
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .header("authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        let ids: Vec<_> = payload["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["id"].as_str())
            .collect();
        assert_eq!(ids, vec!["adaptive-routing", "qwen-3-5-4b"]);
    }

    #[tokio::test]
    async fn chat_completions_can_call_a_target_public_id_without_an_alias() {
        let url = upstream(
            StatusCode::OK,
            json!({"id":"ok","choices":[{"message":{"role":"assistant","content":"direct"},"finish_reason":"stop"}]}),
        )
        .await;
        let store = Store::memory().await.unwrap();
        store
            .upsert_target(&sample_target("local-chat", "qwen-3-5-4b", Some(url)))
            .await
            .unwrap();
        let app = app_from_store(store).await;
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"qwen-3-5-4b","messages":[{"role":"user","content":"hello"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("x-local-ai-target").unwrap(),
            "local-chat"
        );
    }

    #[tokio::test]
    async fn hermes_responses_tools_and_reasoning_hints_reach_local_chat_models() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let captured_for_server = captured.clone();
        let app = Router::new().fallback(move |Json(body): Json<Value>| {
            let captured = captured_for_server.clone();
            async move {
                captured.lock().unwrap().push(body);
                (
                    StatusCode::OK,
                    Json(json!({"id":"ok","choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]})),
                )
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let url = format!("http://{address}/v1");
        let store = Store::memory().await.unwrap();
        let mut target = sample_target("local-chat", "qwen-3-5-4b", Some(url));
        target.kind = TargetKind::Mlx;
        target.capabilities = vec!["chat".into(), "streaming".into()];
        store.upsert_target(&target).await.unwrap();
        let app = app_from_store(store).await;
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"qwen-3-5-4b","input":"hello","tools":[{"type":"function","name":"terminal","parameters":{"type":"object"}}],"reasoning":{"effort":"medium","summary":"auto"},"include":["reasoning.encrypted_content"],"store":false}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let forwarded = captured.lock().unwrap();
        assert_eq!(forwarded.len(), 1);
        assert!(forwarded[0].get("tools").is_none());
        let system = forwarded[0]["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|message| message["role"] == "system")
            .and_then(|message| message["content"].as_str())
            .unwrap_or_default();
        assert!(system.contains("terminal"), "{system}");
        assert!(forwarded[0].get("reasoning_effort").is_none());
        assert!(forwarded[0].get("reasoning").is_none());
    }

    #[tokio::test]
    async fn mlx_force_tool_support_salvages_text_tool_calls() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let captured_for_server = captured.clone();
        let app = Router::new().fallback(move |Json(body): Json<Value>| {
            let captured = captured_for_server.clone();
            async move {
                captured.lock().unwrap().push(body);
                (
                    StatusCode::OK,
                    Json(json!({"id":"ok","choices":[{"message":{"role":"assistant","content":"<tool_call>{\"name\":\"terminal\",\"arguments\":{\"cmd\":\"ls\"}}</tool_call>"},"finish_reason":"stop"}]})),
                )
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let url = format!("http://{address}/v1");
        let store = Store::memory().await.unwrap();
        let mut target = sample_target("local-chat", "qwen-3-5-4b", Some(url));
        target.kind = TargetKind::Mlx;
        target.capabilities = vec!["chat".into(), "streaming".into()];
        store.upsert_target(&target).await.unwrap();
        let app = app_from_store(store).await;
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"qwen-3-5-4b","messages":[{"role":"user","content":"list files"}],"tools":[{"type":"function","function":{"name":"terminal","parameters":{"type":"object"}}}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(captured.lock().unwrap()[0].get("tools").is_none());
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            payload["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "terminal"
        );
        assert_eq!(payload["choices"][0]["finish_reason"], "tool_calls");
    }

    #[tokio::test]
    async fn global_adaptive_routing_picks_by_task_quality_and_price() {
        let cheap = upstream(
            StatusCode::OK,
            json!({"id":"cheap","choices":[{"message":{"role":"assistant","content":"cheap"},"finish_reason":"stop"}]}),
        )
        .await;
        let expensive = upstream(
            StatusCode::OK,
            json!({"id":"expensive","choices":[{"message":{"role":"assistant","content":"expensive"},"finish_reason":"stop"}]}),
        )
        .await;
        let store = Store::memory().await.unwrap();
        store
            .upsert_target(&sample_target("cheap", "cheap-coder", Some(cheap)))
            .await
            .unwrap();
        store
            .upsert_target(&sample_target(
                "expensive",
                "premium-coder",
                Some(expensive),
            ))
            .await
            .unwrap();
        let mut cheap_profile =
            crate::routing::TargetRoutingProfile::neutral("cheap", TargetKind::Gguf);
        cheap_profile.task_quality.insert("coding".into(), 80.0);
        cheap_profile.input_price_per_million = Some(1.0);
        cheap_profile.output_price_per_million = Some(1.0);
        store
            .upsert_target_routing_profile(&cheap_profile)
            .await
            .unwrap();
        let mut expensive_profile =
            crate::routing::TargetRoutingProfile::neutral("expensive", TargetKind::Gguf);
        expensive_profile.task_quality.insert("coding".into(), 90.0);
        expensive_profile.input_price_per_million = Some(100.0);
        expensive_profile.output_price_per_million = Some(100.0);
        store
            .upsert_target_routing_profile(&expensive_profile)
            .await
            .unwrap();
        let app = app_from_store(store).await;
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .header("x-local-ai-task", "coding")
                    .body(Body::from(
                        r#"{"model":"adaptive-routing","messages":[{"role":"user","content":"write code"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("x-local-ai-target").unwrap(),
            "cheap"
        );
        assert_eq!(
            response.headers().get("x-local-ai-routing-mode").unwrap(),
            "adaptive"
        );
        assert_eq!(response.headers().get("x-local-ai-task").unwrap(), "coding");
    }

    #[tokio::test]
    async fn unsupported_api_surface_is_explicit() {
        let response = test_app()
            .await
            .oneshot(
                Request::builder()
                    .uri("/v1/files")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn multipart_model_rewrite_preserves_binary_file_content() {
        let body = b"--boundary\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nlocal-name\r\n--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n\r\n\x00\xff\x10\r\n--boundary--\r\n";
        assert_eq!(extract_multipart_model(body), Some("local-name"));
        let rewritten = rewrite_multipart_model(body, "whisper-1");
        assert_eq!(extract_multipart_model(&rewritten), Some("whisper-1"));
        assert!(rewritten.windows(3).any(|window| window == [0, 255, 16]));
    }

    #[test]
    fn streaming_usage_parser_handles_split_sse_events() {
        let mut buffer = Vec::new();
        assert_eq!(
            extract_sse_usage(
                &mut buffer,
                br#"data: {"choices":[],"usage":{"prompt_tokens":3,"completion"#,
            ),
            None
        );
        assert_eq!(
            extract_sse_usage(&mut buffer, b"_tokens\":5}}\n\ndata: [DONE]\n\n"),
            Some(TokenUsage {
                input_tokens: Some(3),
                output_tokens: Some(5),
                ..TokenUsage::default()
            })
        );

        let mut responses_buffer = Vec::new();
        assert_eq!(
            extract_sse_usage(
                &mut responses_buffer,
                b"data: {\"response\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":11}}}\n\n",
            ),
            Some(TokenUsage {
                input_tokens: Some(7),
                output_tokens: Some(11),
                ..TokenUsage::default()
            })
        );
    }

    #[test]
    fn usage_parser_reads_openai_anthropic_and_gemini_cache_tokens() {
        assert_eq!(
            usage_from_value(&json!({
                "usage": {
                    "prompt_tokens": 100,
                    "completion_tokens": 20,
                    "prompt_tokens_details": { "cached_tokens": 80 }
                }
            })),
            TokenUsage {
                input_tokens: Some(100),
                output_tokens: Some(20),
                cache_read_tokens: Some(80),
                cache_write_tokens: None,
            }
        );
        assert_eq!(
            usage_from_value(&json!({
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 8,
                    "cache_read_input_tokens": 50,
                    "cache_creation_input_tokens": 40
                }
            })),
            TokenUsage {
                input_tokens: Some(10),
                output_tokens: Some(8),
                cache_read_tokens: Some(50),
                cache_write_tokens: Some(40),
            }
        );
        assert_eq!(
            usage_from_value(&json!({
                "usageMetadata": {
                    "promptTokenCount": 30,
                    "candidatesTokenCount": 4,
                    "cachedContentTokenCount": 12
                }
            })),
            TokenUsage {
                input_tokens: Some(30),
                output_tokens: Some(4),
                cache_read_tokens: Some(12),
                cache_write_tokens: None,
            }
        );
    }

    #[test]
    fn upstream_url_does_not_duplicate_v1() {
        assert_eq!(
            join_api_url("https://api.openai.com/v1", "/v1/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            join_api_url("http://127.0.0.1:12100", "/v1/responses"),
            "http://127.0.0.1:12100/v1/responses"
        );
    }

    async fn upstream(status: StatusCode, payload: Value) -> String {
        sequence_upstream(vec![(status, payload)]).await.0
    }

    async fn sequence_upstream(
        responses: Vec<(StatusCode, Value)>,
    ) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let responses = Arc::new(responses);
        let app = Router::new().fallback({
            let calls = calls.clone();
            let responses = responses.clone();
            move || {
                let calls = calls.clone();
                let responses = responses.clone();
                async move {
                    let index = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let (status, payload) = responses
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| responses.last().cloned().expect("responses"));
                    (status, Json(payload))
                }
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}/v1"), calls)
    }

    async fn authenticated_upstream(expected: Vec<(&'static str, &'static str)>) -> String {
        let app=Router::new().fallback(move |headers:HeaderMap| { let expected=expected.clone(); async move { if headers.get("x-local-ai-session").is_none() && expected.iter().all(|(name,value)|headers.get(*name).and_then(|header|header.to_str().ok())==Some(*value)) { (StatusCode::OK,Json(json!({"id":"ok","choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}))) } else { (StatusCode::UNAUTHORIZED,Json(json!({"error":{"code":"bad_auth"}}))) } } });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}/v1")
    }

    #[tokio::test]
    async fn provider_auth_profiles_emit_expected_headers() {
        for (preset_id, auth_mode, expected) in [
            (
                "anthropic",
                AuthMode::ApiKey,
                vec![
                    ("x-api-key", "provider-key"),
                    ("anthropic-version", "2023-06-01"),
                ],
            ),
            (
                "gemini",
                AuthMode::ApiKey,
                vec![("x-goog-api-key", "provider-key")],
            ),
            (
                "openrouter",
                AuthMode::ApiKey,
                vec![
                    ("authorization", "Bearer provider-key"),
                    ("http-referer", "https://local-ai-router.app"),
                    ("x-title", "Local AI Router"),
                ],
            ),
        ] {
            let url = authenticated_upstream(expected).await;
            let store = Store::memory().await.unwrap();
            store
                .upsert_provider(&Provider {
                    id: "provider".into(),
                    name: preset_id.into(),
                    preset_id: preset_id.into(),
                    auth_mode,
                    base_url: url,
                    enabled: true,
                    has_credential: false,
                })
                .await
                .unwrap();
            store
                .upsert_target(&ModelTarget {
                    id: "target".into(),
                    provider_id: Some("provider".into()),
                    name: "Target".into(),
                    kind: TargetKind::Cloud,
                    provider_model: "real".into(),
                    local_path: None,
                    runtime_url: None,
                    wire_protocol: WireProtocol::OpenAiChat,
                    capabilities: vec!["chat".into()],
                    enabled: true,
                    state: "ready".into(),
                    size_bytes: None,
                    local: crate::storage::LocalModelMeta::default(),
                })
                .await
                .unwrap();
            store
                .upsert_route(&ModelRoute {
                    alias: "assistant".into(),
                    enabled: true,
                    capabilities: vec!["chat".into()],
                    targets: vec![RouteTarget {
                        id: "target".into(),
                        kind: TargetKind::Cloud,
                        model: "real".into(),
                        priority: 10,
                        enabled: true,
                        ..Default::default()
                    }],
                })
                .await
                .unwrap();
            let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
            secrets.set(LOCAL_API_KEY, "local-key").unwrap();
            let core = AppCore::new(store, secrets).unwrap();
            core.migrate_legacy_local_api_key().await.unwrap();
            core.save_provider_api_key("provider", "provider-key")
                .unwrap();
            let response=router(Arc::new(core)).oneshot(Request::builder().method("POST").uri("/v1/chat/completions").header("authorization","Bearer local-key").header("x-local-ai-session", "private-session").header("content-type","application/json").body(Body::from(r#"{"model":"assistant","messages":[{"role":"user","content":"hello"}]}"#)).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "preset {preset_id}");
        }

        let url = authenticated_upstream(vec![
            ("authorization", "Bearer subscription-token"),
            ("chatgpt-account-id", "acct_1"),
            ("originator", "local_ai_router"),
        ])
        .await;
        let store = Store::memory().await.unwrap();
        store
            .upsert_provider(&Provider {
                id: "provider".into(),
                name: "Subscription".into(),
                preset_id: "openai_subscription".into(),
                auth_mode: AuthMode::OpenAiSubscription,
                base_url: url,
                enabled: true,
                has_credential: false,
            })
            .await
            .unwrap();
        store
            .upsert_target(&ModelTarget {
                id: "target".into(),
                provider_id: Some("provider".into()),
                name: "Target".into(),
                kind: TargetKind::Cloud,
                provider_model: "real".into(),
                local_path: None,
                runtime_url: None,
                wire_protocol: WireProtocol::OpenAiChat,
                capabilities: vec!["chat".into()],
                enabled: true,
                state: "ready".into(),
                size_bytes: None,
                local: crate::storage::LocalModelMeta::default(),
            })
            .await
            .unwrap();
        store
            .upsert_route(&ModelRoute {
                alias: "assistant".into(),
                enabled: true,
                capabilities: vec!["chat".into()],
                targets: vec![RouteTarget {
                    id: "target".into(),
                    kind: TargetKind::Cloud,
                    model: "real".into(),
                    priority: 10,
                    enabled: true,
                    ..Default::default()
                }],
            })
            .await
            .unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        secrets.set(LOCAL_API_KEY, "local-key").unwrap();
        let credential = crate::oauth::SubscriptionCredential {
            version: 1,
            credential_type: "openai_subscription".into(),
            access_token: "subscription-token".into(),
            refresh_token: "refresh".into(),
            expires_at: Utc::now() + chrono::TimeDelta::hours(1),
            account_id: Some("acct_1".into()),
        };
        secrets
            .set(
                &crate::secrets::provider_account("provider"),
                &serde_json::to_string(&credential).unwrap(),
            )
            .unwrap();
        let core = AppCore::new(store, secrets).unwrap();
        core.migrate_legacy_local_api_key().await.unwrap();
        let response = router(Arc::new(core))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer local-key")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"assistant","messages":[{"role":"user","content":"hello"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn upstream_redirects_are_rejected_before_credentials_can_be_forwarded_again() {
        let url = upstream(StatusCode::FOUND, json!({})).await;
        let store = Store::memory().await.unwrap();
        store
            .upsert_provider(&Provider {
                id: "provider".into(),
                name: "Custom".into(),
                preset_id: "custom_openai".into(),
                auth_mode: AuthMode::ApiKey,
                base_url: url,
                enabled: true,
                has_credential: false,
            })
            .await
            .unwrap();
        store
            .upsert_target(&ModelTarget {
                id: "target".into(),
                provider_id: Some("provider".into()),
                name: "Target".into(),
                kind: TargetKind::Cloud,
                provider_model: "real".into(),
                local_path: None,
                runtime_url: None,
                wire_protocol: WireProtocol::OpenAiChat,
                capabilities: vec!["chat".into()],
                enabled: true,
                state: "ready".into(),
                size_bytes: None,
                local: crate::storage::LocalModelMeta::default(),
            })
            .await
            .unwrap();
        store
            .upsert_route(&ModelRoute {
                alias: "assistant".into(),
                enabled: true,
                capabilities: vec!["chat".into()],
                targets: vec![RouteTarget {
                    id: "target".into(),
                    kind: TargetKind::Cloud,
                    model: "real".into(),
                    priority: 10,
                    enabled: true,
                    ..Default::default()
                }],
            })
            .await
            .unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        secrets.set(LOCAL_API_KEY, "local").unwrap();
        let core = AppCore::new(store, secrets).unwrap();
        core.migrate_legacy_local_api_key().await.unwrap();
        core.save_provider_api_key("provider", "secret").unwrap();
        let response = router(Arc::new(core))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer local")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"assistant","messages":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn transient_status_falls_back_and_logs_only_metadata() {
        let first = upstream(
            StatusCode::TOO_MANY_REQUESTS,
            json!({ "error": { "code": "rate_limit" } }),
        )
        .await;
        let second = upstream(
            StatusCode::OK,
            json!({ "id": "ok", "usage": { "prompt_tokens": 3, "completion_tokens": 5 } }),
        )
        .await;
        let store = Store::memory().await.unwrap();
        for (id, name, url, priority) in [
            ("first", "primary", first, 10),
            ("second", "fallback", second, 20),
        ] {
            store
                .upsert_target(&ModelTarget {
                    id: id.into(),
                    provider_id: None,
                    name: name.into(),
                    kind: TargetKind::Gguf,
                    provider_model: name.into(),
                    local_path: None,
                    runtime_url: Some(url),
                    wire_protocol: crate::providers::WireProtocol::OpenAiChat,
                    capabilities: vec!["chat".into()],
                    enabled: true,
                    state: "ready".into(),
                    size_bytes: None,
                    local: crate::storage::LocalModelMeta::default(),
                })
                .await
                .unwrap();
            if priority == 20 { /* keeps tuple explicit for readability */ }
        }
        store
            .upsert_route(&ModelRoute {
                alias: "assistant".into(),
                enabled: true,
                capabilities: vec!["chat".into()],
                targets: vec![
                    RouteTarget {
                        id: "first".into(),
                        kind: TargetKind::Gguf,
                        model: "primary".into(),
                        priority: 10,
                        enabled: true,
                        ..Default::default()
                    },
                    RouteTarget {
                        id: "second".into(),
                        kind: TargetKind::Gguf,
                        model: "fallback".into(),
                        priority: 20,
                        enabled: true,
                        ..Default::default()
                    },
                ],
            })
            .await
            .unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        secrets.set(LOCAL_API_KEY, "test-token").unwrap();
        let core = AppCore::new(store.clone(), secrets).unwrap();
        core.migrate_legacy_local_api_key().await.unwrap();
        core.local_activity()
            .set_token("first", "runtime-token".into());
        core.local_activity()
            .set_token("second", "runtime-token".into());
        let app = router(Arc::new(core));
        let response = app.oneshot(Request::builder().method("POST").uri("/v1/chat/completions").header("authorization", "Bearer test-token").header("content-type", "application/json").body(Body::from(r#"{"model":"assistant","messages":[{"role":"user","content":"private marker"}]}"#)).unwrap()).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let logs = store.logs(10).await.unwrap();
        assert_eq!(logs[0].api_key_id.as_deref(), Some("default"));
        assert_eq!(logs[0].api_key_name.as_deref(), Some("Default"));
        assert_eq!(logs[0].attempts, 3);
        assert_eq!(logs[0].target.as_deref(), Some("fallback"));
        assert_eq!(
            (logs[0].input_tokens, logs[0].output_tokens),
            (Some(3), Some(5))
        );
        assert!(!serde_json::to_string(&logs)
            .unwrap()
            .contains("private marker"));
    }

    #[tokio::test]
    async fn transient_overloaded_retries_the_same_singleton_target() {
        let (url, calls) = sequence_upstream(vec![
            (
                StatusCode::SERVICE_UNAVAILABLE,
                json!({ "error": { "code": "overloaded", "message": "Service temporarily overloaded" } }),
            ),
            (
                StatusCode::OK,
                json!({ "id": "ok", "choices": [{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}] }),
            ),
        ])
        .await;
        let store = assistant_store(vec![("nvidia", url)]).await;
        let app = app_from_store(store.clone()).await;
        let response = app.oneshot(chat_request(false)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        let logs = store.logs(10).await.unwrap();
        assert_eq!(logs[0].status, 200);
        assert_eq!(logs[0].attempts, 2);
        assert_eq!(logs[0].target.as_deref(), Some("nvidia"));
        let attempts = store.routing_attempts(None, 20).await.unwrap();
        assert_eq!(attempts.len(), 2);
        assert!(attempts.iter().any(|attempt| {
            attempt.reason.contains("retry 2/2 same target after 503")
                && attempt.reason.contains("overloaded")
        }));
    }

    #[tokio::test]
    async fn singleton_stays_on_the_pinned_target_after_retry_exhausts() {
        let (url, calls) = sequence_upstream(vec![(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({ "error": { "code": "overloaded", "message": "Service temporarily overloaded" } }),
        )])
        .await;
        let store = assistant_store(vec![("nvidia", url)]).await;
        let app = app_from_store(store.clone()).await;
        let response = app.oneshot(chat_request(false)).await.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        let logs = store.logs(10).await.unwrap();
        assert_eq!(logs[0].attempts, 2);
        assert_eq!(logs[0].target.as_deref(), Some("nvidia"));
        assert_eq!(store.routing_attempts(None, 20).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn transient_retry_then_failsover_to_the_next_hop() {
        let (first, first_calls) = sequence_upstream(vec![(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({ "error": { "code": "overloaded", "message": "Service temporarily overloaded" } }),
        )])
        .await;
        let second = upstream(
            StatusCode::OK,
            json!({ "id": "ok", "choices": [{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}] }),
        )
        .await;
        let store = assistant_store(vec![("nvidia", first), ("backup", second)]).await;
        let app = app_from_store(store.clone()).await;
        let response = app.oneshot(chat_request(false)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(first_calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(
            response.headers().get("x-local-ai-target").unwrap(),
            "backup"
        );
        let logs = store.logs(10).await.unwrap();
        assert_eq!(logs[0].attempts, 3);
        assert_eq!(logs[0].target.as_deref(), Some("backup"));
    }

    #[tokio::test]
    async fn client_errors_do_not_retry_the_same_target() {
        let started = Instant::now();
        let (first, first_calls) = sequence_upstream(vec![(
            StatusCode::BAD_REQUEST,
            json!({ "error": { "code": "bad_request", "message": "nope" } }),
        )])
        .await;
        let second = upstream(
            StatusCode::OK,
            json!({ "id": "ok", "choices": [{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}] }),
        )
        .await;
        let store = assistant_store(vec![("primary", first), ("backup", second)]).await;
        let app = app_from_store(store.clone()).await;
        let response = app.oneshot(chat_request(false)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(first_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(started.elapsed() < Duration::from_millis(300));
        assert_eq!(store.logs(10).await.unwrap()[0].attempts, 2);
    }

    #[tokio::test]
    async fn not_found_falls_back_to_the_next_target() {
        let first = upstream(
            StatusCode::NOT_FOUND,
            json!({ "error": { "code": "model_not_found" } }),
        )
        .await;
        let second = upstream(
            StatusCode::OK,
            json!({ "id": "ok", "choices": [{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}] }),
        )
        .await;
        let store = Store::memory().await.unwrap();
        store
            .upsert_target(&sample_target("first", "primary", Some(first)))
            .await
            .unwrap();
        store
            .upsert_target(&sample_target("second", "fallback", Some(second)))
            .await
            .unwrap();
        store
            .upsert_route(&ModelRoute {
                alias: "assistant".into(),
                enabled: true,
                capabilities: vec!["chat".into()],
                targets: vec![
                    RouteTarget {
                        id: "first".into(),
                        kind: TargetKind::Gguf,
                        model: "primary".into(),
                        priority: 10,
                        enabled: true,
                        ..Default::default()
                    },
                    RouteTarget {
                        id: "second".into(),
                        kind: TargetKind::Gguf,
                        model: "fallback".into(),
                        priority: 20,
                        enabled: true,
                        ..Default::default()
                    },
                ],
            })
            .await
            .unwrap();
        let app = app_from_store(store.clone()).await;
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"assistant","messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            store.logs(10).await.unwrap()[0].target.as_deref(),
            Some("second")
        );
    }

    #[tokio::test]
    async fn alias_fallback_expands_to_another_route() {
        let missing = upstream(
            StatusCode::NOT_FOUND,
            json!({ "error": { "code": "missing" } }),
        )
        .await;
        let backup = upstream(
            StatusCode::OK,
            json!({ "id": "ok", "choices": [{"message":{"role":"assistant","content":"alias"},"finish_reason":"stop"}] }),
        )
        .await;
        let store = Store::memory().await.unwrap();
        store
            .upsert_target(&sample_target("primary", "primary", Some(missing)))
            .await
            .unwrap();
        store
            .upsert_target(&sample_target("backup-target", "backup", Some(backup)))
            .await
            .unwrap();
        store
            .upsert_route(&ModelRoute {
                alias: "safer".into(),
                enabled: true,
                capabilities: vec!["chat".into()],
                targets: vec![RouteTarget {
                    id: "backup-target".into(),
                    kind: TargetKind::Gguf,
                    model: "backup".into(),
                    priority: 10,
                    enabled: true,
                    ..Default::default()
                }],
            })
            .await
            .unwrap();
        store
            .upsert_route(&ModelRoute {
                alias: "cheap".into(),
                enabled: true,
                capabilities: vec!["chat".into()],
                targets: vec![
                    RouteTarget {
                        id: "primary".into(),
                        kind: TargetKind::Gguf,
                        model: "primary".into(),
                        priority: 10,
                        enabled: true,
                        ..Default::default()
                    },
                    RouteTarget {
                        id: "safer".into(),
                        kind: TargetKind::Alias,
                        model: "safer".into(),
                        priority: 20,
                        enabled: true,
                        ..Default::default()
                    },
                ],
            })
            .await
            .unwrap();
        let app = app_from_store(store.clone()).await;
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"cheap","messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            store.logs(10).await.unwrap()[0].target.as_deref(),
            Some("backup-target")
        );
    }

    #[tokio::test]
    async fn slow_first_byte_falls_back_before_the_global_timeout() {
        let slow = hanging_upstream(Duration::from_secs(20)).await;
        let fast = upstream(
            StatusCode::OK,
            json!({ "id": "ok", "choices": [{"message":{"role":"assistant","content":"fast"},"finish_reason":"stop"}] }),
        )
        .await;
        let other = upstream(
            StatusCode::OK,
            json!({ "id": "ok", "choices": [{"message":{"role":"assistant","content":"other"},"finish_reason":"stop"}] }),
        )
        .await;
        let store = Store::memory().await.unwrap();
        store
            .upsert_target(&sample_target("slow", "slow", Some(slow)))
            .await
            .unwrap();
        store
            .upsert_target(&sample_target("fast", "fast", Some(fast)))
            .await
            .unwrap();
        store
            .upsert_target(&sample_target("other", "other", Some(other)))
            .await
            .unwrap();
        for (id, latency) in [("fast", 1_000), ("other", 1_000)] {
            store
                .insert_routing_attempt(&crate::routing::RoutingAttemptRecord {
                    id: format!("hist-{id}"),
                    request_id: "seed".into(),
                    created_at: Utc::now(),
                    alias: "assistant".into(),
                    task: "general".into(),
                    task_source: "default".into(),
                    target_id: id.into(),
                    routing_mode: "fixed".into(),
                    status: 200,
                    transient_failure: false,
                    retry_after_until: None,
                    latency_ms: latency,
                    ttft_ms: Some(latency),
                    streaming: false,
                    input_tokens: Some(1),
                    output_tokens: None,
                    estimated_cost_usd: None,
                    cost_verified: false,
                    score: None,
                    reason: "default".into(),
                })
                .await
                .unwrap();
        }
        store
            .upsert_route(&ModelRoute {
                alias: "assistant".into(),
                enabled: true,
                capabilities: vec!["chat".into()],
                targets: vec![
                    RouteTarget {
                        id: "slow".into(),
                        kind: TargetKind::Gguf,
                        model: "slow".into(),
                        priority: 10,
                        enabled: true,
                        ..Default::default()
                    },
                    RouteTarget {
                        id: "fast".into(),
                        kind: TargetKind::Gguf,
                        model: "fast".into(),
                        priority: 20,
                        enabled: true,
                        ..Default::default()
                    },
                    RouteTarget {
                        id: "other".into(),
                        kind: TargetKind::Gguf,
                        model: "other".into(),
                        priority: 30,
                        enabled: true,
                        ..Default::default()
                    },
                ],
            })
            .await
            .unwrap();
        let app = app_from_store(store.clone()).await;
        let started = Instant::now();
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"assistant","messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(started.elapsed() < Duration::from_secs(25));
        assert_eq!(
            store.logs(10).await.unwrap()[0].target.as_deref(),
            Some("fast")
        );
    }

    fn hop(id: &str, priority: i64, role: RouteRole) -> RouteTarget {
        RouteTarget {
            id: id.into(),
            kind: TargetKind::Gguf,
            model: id.into(),
            priority,
            enabled: true,
            role,
        }
    }

    async fn assistant_store(targets: Vec<(&str, String)>) -> Store {
        let store = Store::memory().await.unwrap();
        let mut hops = Vec::new();
        for (index, (id, url)) in targets.into_iter().enumerate() {
            store
                .upsert_target(&sample_target(id, id, Some(url)))
                .await
                .unwrap();
            hops.push(hop(id, ((index as i64) + 1) * 10, RouteRole::Primary));
        }
        store
            .upsert_route(&ModelRoute {
                alias: "assistant".into(),
                enabled: true,
                capabilities: vec!["chat".into()],
                targets: hops,
            })
            .await
            .unwrap();
        store
    }

    async fn seed_fast_latency(store: &Store, target_id: &str) {
        store
            .insert_routing_attempt(&crate::routing::RoutingAttemptRecord {
                id: format!("hist-{target_id}"),
                request_id: "seed".into(),
                created_at: Utc::now(),
                alias: "assistant".into(),
                task: "general".into(),
                task_source: "default".into(),
                target_id: target_id.into(),
                routing_mode: "fixed".into(),
                status: 200,
                transient_failure: false,
                retry_after_until: None,
                latency_ms: 1_000,
                ttft_ms: Some(1_000),
                streaming: false,
                input_tokens: Some(1),
                output_tokens: None,
                estimated_cost_usd: None,
                cost_verified: false,
                score: None,
                reason: "default".into(),
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn failed_primary_walks_the_rest_of_the_pool_before_fallbacks() {
        let first = upstream(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": { "code": "unavailable" } }),
        )
        .await;
        let second = upstream(
            StatusCode::OK,
            json!({ "id": "ok", "choices": [{"message":{"role":"assistant","content":"pool"},"finish_reason":"stop"}] }),
        )
        .await;
        let local = upstream(
            StatusCode::OK,
            json!({ "id": "ok", "choices": [{"message":{"role":"assistant","content":"local"},"finish_reason":"stop"}] }),
        )
        .await;
        let deepseek = upstream(
            StatusCode::OK,
            json!({ "id": "ok", "choices": [{"message":{"role":"assistant","content":"deepseek"},"finish_reason":"stop"}] }),
        )
        .await;
        let store = Store::memory().await.unwrap();
        for (id, url) in [
            ("first", first),
            ("second", second),
            ("local", local),
            ("deepseek", deepseek),
        ] {
            store
                .upsert_target(&sample_target(id, id, Some(url)))
                .await
                .unwrap();
        }
        store
            .upsert_route(&ModelRoute {
                alias: "assistant".into(),
                enabled: true,
                capabilities: vec!["chat".into()],
                targets: vec![
                    hop("first", 10, RouteRole::Primary),
                    hop("second", 20, RouteRole::Primary),
                    hop("local", 10, RouteRole::Fallback),
                    hop("deepseek", 20, RouteRole::Fallback),
                ],
            })
            .await
            .unwrap();
        let app = app_from_store(store.clone()).await;
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"assistant","messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("x-local-ai-target").unwrap(),
            "second"
        );
        let attempted: Vec<_> = store
            .routing_attempts(None, 20)
            .await
            .unwrap()
            .into_iter()
            .map(|attempt| attempt.target_id)
            .collect();
        assert!(attempted.contains(&"first".to_string()));
        assert!(attempted.contains(&"second".to_string()));
        assert!(!attempted.contains(&"local".to_string()));
        assert!(!attempted.contains(&"deepseek".to_string()));
    }

    #[tokio::test]
    async fn fallback_failover_does_not_use_primary_first_byte_timeout() {
        let first = upstream(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": { "code": "unavailable" } }),
        )
        .await;
        let second = upstream(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": { "code": "unavailable" } }),
        )
        .await;
        let local = hanging_upstream(Duration::from_millis(9_500)).await;
        let deepseek = upstream(
            StatusCode::OK,
            json!({ "id": "ok", "choices": [{"message":{"role":"assistant","content":"deepseek"},"finish_reason":"stop"}] }),
        )
        .await;
        let store = Store::memory().await.unwrap();
        for (id, url) in [
            ("first", first),
            ("second", second),
            ("local", local),
            ("deepseek", deepseek),
        ] {
            store
                .upsert_target(&sample_target(id, id, Some(url)))
                .await
                .unwrap();
        }
        for id in ["first", "second", "deepseek"] {
            seed_fast_latency(&store, id).await;
        }
        store
            .upsert_route(&ModelRoute {
                alias: "assistant".into(),
                enabled: true,
                capabilities: vec!["chat".into()],
                targets: vec![
                    hop("first", 10, RouteRole::Primary),
                    hop("second", 20, RouteRole::Primary),
                    hop("local", 10, RouteRole::Fallback),
                    hop("deepseek", 20, RouteRole::Fallback),
                ],
            })
            .await
            .unwrap();
        let app = app_from_store(store.clone()).await;
        let started = Instant::now();
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"assistant","messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(started.elapsed() >= Duration::from_millis(9_000));
        assert!(started.elapsed() < Duration::from_secs(20));
        assert_eq!(
            response.headers().get("x-local-ai-target").unwrap(),
            "local"
        );
        let attempts = store.routing_attempts(None, 20).await.unwrap();
        assert!(attempts.iter().any(|attempt| attempt.target_id == "local"));
        assert!(!attempts
            .iter()
            .any(|attempt| { attempt.target_id == "deepseek" && attempt.request_id != "seed" }));
    }

    #[tokio::test]
    async fn local_load_failure_is_recorded_then_failsover() {
        let deepseek = upstream(
            StatusCode::OK,
            json!({ "id": "ok", "choices": [{"message":{"role":"assistant","content":"deepseek"},"finish_reason":"stop"}] }),
        )
        .await;
        let store = Store::memory().await.unwrap();
        store
            .upsert_target(&sample_target("local", "local", None))
            .await
            .unwrap();
        store
            .upsert_target(&sample_target("deepseek", "deepseek", Some(deepseek)))
            .await
            .unwrap();
        store
            .upsert_route(&ModelRoute {
                alias: "assistant".into(),
                enabled: true,
                capabilities: vec!["chat".into()],
                targets: vec![
                    hop("local", 10, RouteRole::Fallback),
                    hop("deepseek", 20, RouteRole::Fallback),
                ],
            })
            .await
            .unwrap();
        let app = app_from_store(store.clone()).await;
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"assistant","messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("x-local-ai-target").unwrap(),
            "deepseek"
        );
        let attempts = store.routing_attempts(None, 20).await.unwrap();
        assert!(attempts.iter().any(|attempt| {
            attempt.target_id == "local" && attempt.reason.contains("local_load_failed")
        }));
        assert!(attempts
            .iter()
            .any(|attempt| { attempt.target_id == "deepseek" && attempt.status == 200 }));
    }

    #[test]
    fn rate_limit_without_retry_after_defaults_to_thirty_seconds() {
        let until = rate_limit_until(&HeaderMap::new(), 429).unwrap();
        let delta = (until - Utc::now()).num_seconds();
        assert!((29..=31).contains(&delta));
    }

    #[test]
    fn rate_limit_reset_header_is_honored() {
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-reset-requests", HeaderValue::from_static("12"));
        let until = rate_limit_until(&headers, 429).unwrap();
        let delta = (until - Utc::now()).num_seconds();
        assert!((11..=13).contains(&delta));
    }

    #[test]
    fn same_target_retry_waits_short_retry_after_only() {
        assert_eq!(
            same_target_retry_delay(None),
            Duration::from_millis(SAME_TARGET_RETRY_DELAY_MS)
        );
        let long = Utc::now() + chrono::Duration::seconds(30);
        assert_eq!(
            same_target_retry_delay(Some(long)),
            Duration::from_millis(SAME_TARGET_RETRY_DELAY_MS)
        );
        let short = Utc::now() + chrono::Duration::milliseconds(800);
        let wait = same_target_retry_delay(Some(short));
        assert!(wait >= Duration::from_millis(500) && wait <= Duration::from_millis(900));
    }

    async fn hanging_upstream(delay: Duration) -> String {
        let app = Router::new().fallback(move || async move {
            tokio::time::sleep(delay).await;
            (
                StatusCode::OK,
                Json(json!({"id":"slow","choices":[{"message":{"role":"assistant","content":"slow"},"finish_reason":"stop"}]})),
            )
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}/v1")
    }

    async fn hanging_sse_upstream() -> String {
        let app = Router::new().fallback(|| async {
            let stream = async_stream::stream! {
                yield Ok::<_, std::io::Error>(Bytes::from(
                    "data: {\"id\":\"s\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n",
                ));
                std::future::pending::<()>().await;
                yield Ok(Bytes::from("data: [DONE]\n\n"));
            };
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from_stream(stream))
                .unwrap()
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}/v1")
    }

    async fn wait_for_inflight(core: &AppCore) -> crate::core::InFlightRequest {
        wait_for_inflight_matching(core, |_| true).await
    }

    async fn wait_for_inflight_phase(core: &AppCore, phase: &str) -> crate::core::InFlightRequest {
        wait_for_inflight_matching(core, |request| request.phase == phase).await
    }

    async fn wait_for_inflight_matching(
        core: &AppCore,
        predicate: impl Fn(&crate::core::InFlightRequest) -> bool,
    ) -> crate::core::InFlightRequest {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(request) = core
                .traffic
                .snapshot()
                .into_iter()
                .find(|request| predicate(request))
            {
                return request;
            }
            if Instant::now() > deadline {
                panic!("in-flight request never appeared");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn wait_until_idle(core: &AppCore) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if core.traffic.snapshot().is_empty() {
                return;
            }
            if Instant::now() > deadline {
                panic!("in-flight request did not clear");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn inflight_test_core(runtime_url: String) -> (Arc<AppCore>, Router) {
        let store = Store::memory().await.unwrap();
        let mut target = sample_target("primary", "primary", Some(runtime_url));
        target.capabilities = vec!["chat".into(), "streaming".into()];
        store.upsert_target(&target).await.unwrap();
        store
            .upsert_route(&ModelRoute {
                alias: "assistant".into(),
                enabled: true,
                capabilities: vec!["chat".into(), "streaming".into()],
                targets: vec![RouteTarget {
                    id: "primary".into(),
                    kind: TargetKind::Gguf,
                    model: "primary".into(),
                    priority: 10,
                    enabled: true,
                    ..Default::default()
                }],
            })
            .await
            .unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        secrets.set(LOCAL_API_KEY, "test-token").unwrap();
        let core = AppCore::new(store.clone(), secrets).unwrap();
        core.migrate_legacy_local_api_key().await.unwrap();
        core.local_activity()
            .set_token("primary", "runtime-token".into());
        let core = Arc::new(core);
        let app = router(core.clone());
        (core, app)
    }

    fn chat_request(stream: bool) -> Request<Body> {
        let body = if stream {
            r#"{"model":"assistant","stream":true,"messages":[{"role":"user","content":"hi"}]}"#
        } else {
            r#"{"model":"assistant","messages":[{"role":"user","content":"hi"}]}"#
        };
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("authorization", "Bearer test-token")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap()
    }

    #[tokio::test]
    async fn inflight_appears_during_proxy_and_clears_after_response() {
        let hang = hanging_upstream(Duration::from_millis(300)).await;
        let (core, app) = inflight_test_core(hang).await;
        let pending = tokio::spawn(async move { app.oneshot(chat_request(false)).await });
        let request = wait_for_inflight(&core).await;
        assert_eq!(request.alias, "assistant");
        assert_eq!(request.endpoint, "/v1/chat/completions");
        let response = pending.await.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(core.traffic.snapshot().is_empty());
    }

    #[tokio::test]
    async fn inflight_clears_when_streaming_client_disconnects() {
        let hang = hanging_sse_upstream().await;
        let (core, app) = inflight_test_core(hang).await;
        {
            let request_fut = app.oneshot(chat_request(true));
            tokio::pin!(request_fut);
            tokio::select! {
                biased;
                request = wait_for_inflight(&core) => {
                    assert_eq!(request.alias, "assistant");
                }
                result = &mut request_fut => {
                    drop(result.unwrap());
                }
            }
        }
        wait_until_idle(&core).await;
    }

    #[tokio::test]
    async fn inflight_cancel_aborts_a_hanging_upstream() {
        let hang = hanging_upstream(Duration::from_secs(30)).await;
        let (core, app) = inflight_test_core(hang).await;
        let pending = tokio::spawn(async move { app.oneshot(chat_request(false)).await });
        let request = wait_for_inflight(&core).await;
        assert!(core.traffic.cancel(&request.id));
        wait_until_idle(&core).await;
        let response = pending.await.unwrap().unwrap();
        assert_eq!(response.status().as_u16(), 499);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&body).contains("cancelled"));
    }

    #[tokio::test]
    async fn cancel_during_same_target_retry_pause_returns_499() {
        let (url, _) = sequence_upstream(vec![(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({ "error": { "code": "overloaded", "message": "Service temporarily overloaded" } }),
        )])
        .await;
        let (core, app) = inflight_test_core(url).await;
        let pending = tokio::spawn(async move { app.oneshot(chat_request(false)).await });
        let request = wait_for_inflight_phase(&core, "retrying").await;
        assert_eq!(request.last_error_code.as_deref(), Some("overloaded"));
        assert!(request
            .last_error_message
            .as_deref()
            .unwrap_or("")
            .contains("overloaded"));
        assert!(core.traffic.cancel(&request.id));
        wait_until_idle(&core).await;
        let response = pending.await.unwrap().unwrap();
        assert_eq!(response.status().as_u16(), 499);
    }

    #[tokio::test]
    async fn inflight_cancel_aborts_a_hanging_stream() {
        let hang = hanging_sse_upstream().await;
        let (core, app) = inflight_test_core(hang).await;
        let pending = tokio::spawn(async move { app.oneshot(chat_request(true)).await });
        let request = wait_for_inflight(&core).await;
        assert_eq!(request.phase, "streaming");
        assert!(core.traffic.cancel(&request.id));
        wait_until_idle(&core).await;
        let response = pending.await.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn active_adaptive_policy_routes_by_explicit_task_and_explains_the_choice() {
        let low = upstream(StatusCode::OK, json!({ "id": "low", "choices": [{"message":{"role":"assistant","content":"low"},"finish_reason":"stop"}] })).await;
        let high = upstream(StatusCode::OK, json!({ "id": "high", "choices": [{"message":{"role":"assistant","content":"high"},"finish_reason":"stop"}] })).await;
        let store = Store::memory().await.unwrap();
        for (id, url, priority) in [("low", low, 10), ("high", high, 20)] {
            store
                .upsert_target(&ModelTarget {
                    id: id.into(),
                    provider_id: None,
                    name: id.into(),
                    kind: TargetKind::Gguf,
                    provider_model: id.into(),
                    local_path: None,
                    runtime_url: Some(url),
                    wire_protocol: WireProtocol::OpenAiChat,
                    capabilities: vec!["chat".into()],
                    enabled: true,
                    state: "ready".into(),
                    size_bytes: None,
                    local: crate::storage::LocalModelMeta::default(),
                })
                .await
                .unwrap();
            let mut profile = crate::routing::TargetRoutingProfile::neutral(id, TargetKind::Gguf);
            profile
                .task_quality
                .insert("coding".into(), if id == "high" { 95.0 } else { 20.0 });
            store.upsert_target_routing_profile(&profile).await.unwrap();
            if priority == 20 { /* documents the fixed-order inversion */ }
        }
        store
            .upsert_route(&ModelRoute {
                alias: "assistant".into(),
                enabled: true,
                capabilities: vec!["chat".into()],
                targets: vec![
                    RouteTarget {
                        id: "low".into(),
                        kind: TargetKind::Gguf,
                        model: "low".into(),
                        priority: 10,
                        enabled: true,
                        ..Default::default()
                    },
                    RouteTarget {
                        id: "high".into(),
                        kind: TargetKind::Gguf,
                        model: "high".into(),
                        priority: 20,
                        enabled: true,
                        ..Default::default()
                    },
                ],
            })
            .await
            .unwrap();
        let mut policy = crate::routing::RoutingPolicy::new("assistant");
        policy.mode = crate::routing::RoutingMode::Adaptive;
        policy.status = crate::routing::PolicyStatus::Active;
        policy.candidate_target_ids = vec!["low".into(), "high".into()];
        policy.privacy = crate::routing::PrivacyMode::CloudAllowed;
        store.upsert_routing_policy(&policy).await.unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        secrets.set(LOCAL_API_KEY, "test-token").unwrap();
        let core = AppCore::new(store.clone(), secrets).unwrap();
        core.migrate_legacy_local_api_key().await.unwrap();
        core.local_activity()
            .set_token("low", "runtime-token".into());
        core.local_activity()
            .set_token("high", "runtime-token".into());

        let response = router(Arc::new(core)).oneshot(Request::builder().method("POST").uri("/v1/chat/completions")
            .header("authorization", "Bearer test-token").header("content-type", "application/json").header("x-local-ai-task", "coding")
            .body(Body::from(r#"{"model":"assistant","messages":[{"role":"user","content":"write code"}]}"#)).unwrap()).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get("x-local-ai-target").unwrap(), "high");
        assert_eq!(response.headers().get("x-local-ai-task").unwrap(), "coding");
        assert_eq!(
            response.headers().get("x-local-ai-routing-mode").unwrap(),
            "adaptive"
        );
    }

    #[tokio::test]
    async fn anthropic_and_gemini_facades_translate_openai_upstream() {
        let url = upstream(StatusCode::OK, json!({
            "id":"upstream","choices":[{"message":{"role":"assistant","content":"translated"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":2,"completion_tokens":1}
        })).await;
        let store = Store::memory().await.unwrap();
        store
            .upsert_target(&ModelTarget {
                id: "cloud".into(),
                provider_id: None,
                name: "Cloud".into(),
                kind: TargetKind::Gguf,
                provider_model: "provider-model".into(),
                local_path: None,
                runtime_url: Some(url),
                wire_protocol: crate::providers::WireProtocol::OpenAiChat,
                capabilities: vec![
                    "chat".into(),
                    "streaming".into(),
                    "tools".into(),
                    "vision".into(),
                    "structured_output".into(),
                ],
                enabled: true,
                state: "ready".into(),
                size_bytes: None,
                local: crate::storage::LocalModelMeta::default(),
            })
            .await
            .unwrap();
        store
            .upsert_route(&ModelRoute {
                alias: "assistant".into(),
                enabled: true,
                capabilities: vec!["chat".into()],
                targets: vec![RouteTarget {
                    id: "cloud".into(),
                    kind: TargetKind::Gguf,
                    model: "provider-model".into(),
                    priority: 10,
                    enabled: true,
                    ..Default::default()
                }],
            })
            .await
            .unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        secrets.set(LOCAL_API_KEY, "test-token").unwrap();
        let core = AppCore::new(store, secrets).unwrap();
        core.migrate_legacy_local_api_key().await.unwrap();
        core.local_activity()
            .set_token("cloud", "runtime-token".into());
        let app = router(Arc::new(core));

        let anthropic=app.clone().oneshot(Request::builder().method("POST").uri("/v1/messages").header("x-api-key","test-token").header("content-type","application/json").body(Body::from(r#"{"model":"assistant","max_tokens":20,"messages":[{"role":"user","content":"hello"}]}"#)).unwrap()).await.unwrap();
        assert_eq!(anthropic.status(), StatusCode::OK);
        let anthropic_body: Value =
            serde_json::from_slice(&anthropic.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(anthropic_body["content"][0]["text"], "translated");

        let gemini = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/assistant:generateContent")
                    .header("x-goog-api-key", "test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"contents":[{"role":"user","parts":[{"text":"hello"}]}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(gemini.status(), StatusCode::OK);
        let gemini_body: Value =
            serde_json::from_slice(&gemini.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(
            gemini_body["candidates"][0]["content"]["parts"][0]["text"],
            "translated"
        );
    }

    #[tokio::test]
    async fn query_string_api_keys_are_rejected() {
        let response = test_app()
            .await
            .oneshot(
                Request::builder()
                    .uri("/v1beta/models?key=test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    async fn local_app(store: Store, token: &str, runtime_token: &str, target_id: &str) -> Router {
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        secrets.set(LOCAL_API_KEY, token).unwrap();
        let core = AppCore::new(store, secrets).unwrap();
        core.migrate_legacy_local_api_key().await.unwrap();
        core.local_activity()
            .set_token(target_id, runtime_token.into());
        router(Arc::new(core))
    }

    #[tokio::test]
    async fn local_speech_requires_speech_capability_not_legacy_audio() {
        let url = upstream(StatusCode::OK, json!({"id":"ok"})).await;
        let store = Store::memory().await.unwrap();
        store
            .upsert_target(&ModelTarget {
                id: "tts".into(),
                provider_id: None,
                name: "Legacy audio".into(),
                kind: TargetKind::Mlx,
                provider_model: "kokoro".into(),
                local_path: Some("/tmp/model".into()),
                runtime_url: Some(url),
                wire_protocol: crate::providers::WireProtocol::OpenAiChat,
                capabilities: vec!["audio".into()],
                enabled: true,
                state: "ready".into(),
                size_bytes: None,
                local: crate::storage::LocalModelMeta::default(),
            })
            .await
            .unwrap();
        store
            .upsert_route(&ModelRoute {
                alias: "speaker".into(),
                enabled: true,
                capabilities: vec!["speech".into(), "audio".into()],
                targets: vec![RouteTarget {
                    id: "tts".into(),
                    kind: TargetKind::Mlx,
                    model: "kokoro".into(),
                    priority: 10,
                    enabled: true,
                    ..Default::default()
                }],
            })
            .await
            .unwrap();
        let response = local_app(store, "test-token", "runtime-token", "tts")
            .await
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/audio/speech")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"speaker","input":"hello","voice":"af_heart"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8_lossy(&bytes);
        assert!(body.contains("unsupported_capability"));
    }

    #[tokio::test]
    async fn local_image_generation_rejects_unsupported_openai_options() {
        let url = upstream(
            StatusCode::OK,
            json!({"created":1,"data":[{"b64_json":"aa"}]}),
        )
        .await;
        let store = Store::memory().await.unwrap();
        store
            .upsert_target(&ModelTarget {
                id: "image".into(),
                provider_id: None,
                name: "FLUX".into(),
                kind: TargetKind::Mlx,
                provider_model: "flux".into(),
                local_path: Some("/tmp/model".into()),
                runtime_url: Some(url),
                wire_protocol: crate::providers::WireProtocol::OpenAiChat,
                capabilities: vec!["images".into()],
                enabled: true,
                state: "ready".into(),
                size_bytes: None,
                local: crate::storage::LocalModelMeta::default(),
            })
            .await
            .unwrap();
        store
            .upsert_route(&ModelRoute {
                alias: "painter".into(),
                enabled: true,
                capabilities: vec!["images".into()],
                targets: vec![RouteTarget {
                    id: "image".into(),
                    kind: TargetKind::Mlx,
                    model: "flux".into(),
                    priority: 10,
                    enabled: true,
                    ..Default::default()
                }],
            })
            .await
            .unwrap();
        let response = local_app(store, "test-token", "runtime-token", "image")
            .await
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/images/generations")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"painter","prompt":"a cat","n":2}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8_lossy(&bytes);
        assert!(body.contains("unsupported_parameter"));
    }

    #[tokio::test]
    async fn audio_input_falls_back_to_a_capable_target() {
        let first = upstream(StatusCode::OK, json!({"id":"primary","choices":[{"message":{"role":"assistant","content":"primary"},"finish_reason":"stop"}]})).await;
        let second = upstream(StatusCode::OK, json!({"id":"fallback","choices":[{"message":{"role":"assistant","content":"audio-ok"},"finish_reason":"stop"}]})).await;
        let store = Store::memory().await.unwrap();
        store
            .upsert_target(&ModelTarget {
                id: "text".into(),
                provider_id: None,
                name: "Text".into(),
                kind: TargetKind::Mlx,
                provider_model: "qwen".into(),
                local_path: Some("/tmp/a".into()),
                runtime_url: Some(first),
                wire_protocol: crate::providers::WireProtocol::OpenAiChat,
                capabilities: vec!["chat".into()],
                enabled: true,
                state: "ready".into(),
                size_bytes: None,
                local: crate::storage::LocalModelMeta::default(),
            })
            .await
            .unwrap();
        store
            .upsert_target(&ModelTarget {
                id: "audio".into(),
                provider_id: None,
                name: "Audio".into(),
                kind: TargetKind::Mlx,
                provider_model: "vlm".into(),
                local_path: Some("/tmp/b".into()),
                runtime_url: Some(second),
                wire_protocol: crate::providers::WireProtocol::OpenAiChat,
                capabilities: vec!["chat".into(), "audio_input".into()],
                enabled: true,
                state: "ready".into(),
                size_bytes: None,
                local: crate::storage::LocalModelMeta::default(),
            })
            .await
            .unwrap();
        store
            .upsert_route(&ModelRoute {
                alias: "assistant".into(),
                enabled: true,
                capabilities: vec!["chat".into()],
                targets: vec![
                    RouteTarget {
                        id: "text".into(),
                        kind: TargetKind::Mlx,
                        model: "qwen".into(),
                        priority: 10,
                        enabled: true,
                        ..Default::default()
                    },
                    RouteTarget {
                        id: "audio".into(),
                        kind: TargetKind::Mlx,
                        model: "vlm".into(),
                        priority: 20,
                        enabled: true,
                        ..Default::default()
                    },
                ],
            })
            .await
            .unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        secrets.set(LOCAL_API_KEY, "test-token").unwrap();
        let core = AppCore::new(store, secrets).unwrap();
        core.migrate_legacy_local_api_key().await.unwrap();
        core.local_activity()
            .set_token("text", "runtime-token".into());
        core.local_activity()
            .set_token("audio", "runtime-token".into());
        let response = router(Arc::new(core))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"assistant","messages":[{"role":"user","content":[{"type":"input_audio","input_audio":{"data":"AAAA","format":"wav"}}]}]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["choices"][0]["message"]["content"], "audio-ok");
    }

    #[tokio::test]
    async fn missing_video_input_capability_is_a_client_error() {
        let url = upstream(StatusCode::OK, json!({"id":"ok","choices":[{"message":{"role":"assistant","content":"no"},"finish_reason":"stop"}]})).await;
        let store = Store::memory().await.unwrap();
        store
            .upsert_target(&ModelTarget {
                id: "chat".into(),
                provider_id: None,
                name: "Chat".into(),
                kind: TargetKind::Mlx,
                provider_model: "qwen".into(),
                local_path: Some("/tmp/a".into()),
                runtime_url: Some(url),
                wire_protocol: crate::providers::WireProtocol::OpenAiChat,
                capabilities: vec!["chat".into(), "vision".into()],
                enabled: true,
                state: "ready".into(),
                size_bytes: None,
                local: crate::storage::LocalModelMeta::default(),
            })
            .await
            .unwrap();
        store
            .upsert_route(&ModelRoute {
                alias: "assistant".into(),
                enabled: true,
                capabilities: vec!["chat".into()],
                targets: vec![RouteTarget {
                    id: "chat".into(),
                    kind: TargetKind::Mlx,
                    model: "qwen".into(),
                    priority: 10,
                    enabled: true,
                    ..Default::default()
                }],
            })
            .await
            .unwrap();
        let response = local_app(store, "test-token", "runtime-token", "chat")
            .await
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"assistant","messages":[{"role":"user","content":[{"type":"input_video","input_video":{"url":"data:video/mp4;base64,AA=="}}]}]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8_lossy(&bytes);
        assert!(body.contains("unsupported_capability"));
    }

    #[tokio::test]
    async fn vision_request_uses_capability_fallback_instead_of_rejecting() {
        let chat = upstream(StatusCode::OK, json!({"id":"chat","choices":[{"message":{"role":"assistant","content":"text-only"},"finish_reason":"stop"}]})).await;
        let vision = upstream(StatusCode::OK, json!({"id":"vision","choices":[{"message":{"role":"assistant","content":"saw-it"},"finish_reason":"stop"}]})).await;
        let store = Store::memory().await.unwrap();
        let mut chat_target = sample_target("chat", "chat-model", Some(chat));
        chat_target.kind = TargetKind::Mlx;
        chat_target.capabilities = vec!["chat".into()];
        store.upsert_target(&chat_target).await.unwrap();
        let mut vision_target = sample_target("vision", "vision-model", Some(vision));
        vision_target.kind = TargetKind::Mlx;
        vision_target.capabilities = vec!["chat".into(), "vision".into()];
        store.upsert_target(&vision_target).await.unwrap();
        store
            .upsert_route(&ModelRoute {
                alias: "assistant".into(),
                enabled: true,
                capabilities: vec!["chat".into(), "vision".into()],
                targets: vec![
                    RouteTarget {
                        id: "chat".into(),
                        kind: TargetKind::Mlx,
                        model: "chat-model".into(),
                        priority: 10,
                        enabled: true,
                        role: RouteRole::Primary,
                    },
                    RouteTarget {
                        id: "vision".into(),
                        kind: TargetKind::Mlx,
                        model: "vision-model".into(),
                        priority: 10,
                        enabled: true,
                        role: RouteRole::Fallback,
                    },
                ],
            })
            .await
            .unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        secrets.set(LOCAL_API_KEY, "test-token").unwrap();
        let core = AppCore::new(store, secrets).unwrap();
        core.migrate_legacy_local_api_key().await.unwrap();
        core.local_activity()
            .set_token("chat", "runtime-token".into());
        core.local_activity()
            .set_token("vision", "runtime-token".into());
        let response = router(Arc::new(core))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"assistant","messages":[{"role":"user","content":[{"type":"text","text":"what is this"},{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}]}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("x-local-ai-target").unwrap(),
            "vision"
        );
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["choices"][0]["message"]["content"], "saw-it");
    }

    #[tokio::test]
    async fn adaptive_ranks_primaries_and_does_not_select_a_capable_fallback() {
        let low = upstream(StatusCode::OK, json!({ "id": "low", "choices": [{"message":{"role":"assistant","content":"low"},"finish_reason":"stop"}] })).await;
        let high = upstream(StatusCode::OK, json!({ "id": "high", "choices": [{"message":{"role":"assistant","content":"high"},"finish_reason":"stop"}] })).await;
        let reserve = upstream(StatusCode::OK, json!({ "id": "reserve", "choices": [{"message":{"role":"assistant","content":"reserve"},"finish_reason":"stop"}] })).await;
        let store = Store::memory().await.unwrap();
        for (id, url, quality) in [
            ("low", low, 20.0),
            ("high", high, 95.0),
            ("reserve", reserve, 100.0),
        ] {
            store
                .upsert_target(&sample_target(id, id, Some(url)))
                .await
                .unwrap();
            let mut profile = crate::routing::TargetRoutingProfile::neutral(id, TargetKind::Gguf);
            profile.task_quality.insert("coding".into(), quality);
            store.upsert_target_routing_profile(&profile).await.unwrap();
        }
        store
            .upsert_route(&ModelRoute {
                alias: "assistant".into(),
                enabled: true,
                capabilities: vec!["chat".into()],
                targets: vec![
                    RouteTarget {
                        id: "low".into(),
                        kind: TargetKind::Gguf,
                        model: "low".into(),
                        priority: 10,
                        enabled: true,
                        role: RouteRole::Primary,
                    },
                    RouteTarget {
                        id: "high".into(),
                        kind: TargetKind::Gguf,
                        model: "high".into(),
                        priority: 20,
                        enabled: true,
                        role: RouteRole::Primary,
                    },
                    RouteTarget {
                        id: "reserve".into(),
                        kind: TargetKind::Gguf,
                        model: "reserve".into(),
                        priority: 10,
                        enabled: true,
                        role: RouteRole::Fallback,
                    },
                ],
            })
            .await
            .unwrap();
        let mut policy = crate::routing::RoutingPolicy::new("assistant");
        policy.mode = crate::routing::RoutingMode::Adaptive;
        policy.status = crate::routing::PolicyStatus::Active;
        policy.candidate_target_ids = vec!["low".into(), "high".into()];
        policy.privacy = crate::routing::PrivacyMode::CloudAllowed;
        store.upsert_routing_policy(&policy).await.unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        secrets.set(LOCAL_API_KEY, "test-token").unwrap();
        let core = AppCore::new(store, secrets).unwrap();
        core.migrate_legacy_local_api_key().await.unwrap();
        for id in ["low", "high", "reserve"] {
            core.local_activity().set_token(id, "runtime-token".into());
        }
        let response = router(Arc::new(core))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .header("x-local-ai-task", "coding")
                    .body(Body::from(
                        r#"{"model":"assistant","messages":[{"role":"user","content":"write code"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get("x-local-ai-target").unwrap(), "high");
    }

    #[tokio::test]
    async fn performance_uses_fallback_after_primary_not_found() {
        let first = upstream(
            StatusCode::NOT_FOUND,
            json!({ "error": { "code": "model_not_found" } }),
        )
        .await;
        let second = upstream(
            StatusCode::OK,
            json!({ "id": "ok", "choices": [{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}] }),
        )
        .await;
        let store = Store::memory().await.unwrap();
        store
            .upsert_target(&sample_target("first", "primary", Some(first)))
            .await
            .unwrap();
        store
            .upsert_target(&sample_target("second", "fallback", Some(second)))
            .await
            .unwrap();
        store
            .upsert_route(&ModelRoute {
                alias: "assistant".into(),
                enabled: true,
                capabilities: vec!["chat".into()],
                targets: vec![
                    RouteTarget {
                        id: "first".into(),
                        kind: TargetKind::Gguf,
                        model: "primary".into(),
                        priority: 10,
                        enabled: true,
                        role: RouteRole::Primary,
                    },
                    RouteTarget {
                        id: "second".into(),
                        kind: TargetKind::Gguf,
                        model: "fallback".into(),
                        priority: 10,
                        enabled: true,
                        role: RouteRole::Fallback,
                    },
                ],
            })
            .await
            .unwrap();
        let app = app_from_store(store.clone()).await;
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"assistant","messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("x-local-ai-target").unwrap(),
            "second"
        );
    }

    #[tokio::test]
    async fn performance_uses_fallback_after_primary_bad_request() {
        let first = upstream(
            StatusCode::BAD_REQUEST,
            json!({
                "error": {
                    "code": "context_length_exceeded",
                    "message": "This model's maximum context length is 8192 tokens"
                }
            }),
        )
        .await;
        let second = upstream(
            StatusCode::OK,
            json!({ "id": "ok", "choices": [{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}] }),
        )
        .await;
        let store = Store::memory().await.unwrap();
        store
            .upsert_target(&sample_target("first", "primary", Some(first)))
            .await
            .unwrap();
        store
            .upsert_target(&sample_target("second", "fallback", Some(second)))
            .await
            .unwrap();
        store
            .upsert_route(&ModelRoute {
                alias: "assistant".into(),
                enabled: true,
                capabilities: vec!["chat".into()],
                targets: vec![
                    RouteTarget {
                        id: "first".into(),
                        kind: TargetKind::Gguf,
                        model: "primary".into(),
                        priority: 10,
                        enabled: true,
                        role: RouteRole::Primary,
                    },
                    RouteTarget {
                        id: "second".into(),
                        kind: TargetKind::Gguf,
                        model: "fallback".into(),
                        priority: 10,
                        enabled: true,
                        role: RouteRole::Fallback,
                    },
                ],
            })
            .await
            .unwrap();
        let app = app_from_store(store.clone()).await;
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"assistant","messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("x-local-ai-target").unwrap(),
            "second"
        );
        let logs = store.logs(10).await.unwrap();
        assert_eq!(logs[0].status, 200);
        assert_eq!(logs[0].attempts, 2);
        let attempts = store.routing_attempts(None, 20).await.unwrap();
        let failed = attempts
            .iter()
            .find(|attempt| attempt.status == 400)
            .expect("failed hop");
        assert!(failed
            .reason
            .contains("error 400 context_length_exceeded: This model's maximum context length"));
        assert!(failed.reason.contains("performance hop 1/2"));
        let served = attempts
            .iter()
            .find(|attempt| attempt.status == 200)
            .expect("served hop");
        assert!(served
            .reason
            .contains("fallback after 400 context_length_exceeded"));
        assert!(served.reason.contains("failover hop 2/2"));
        assert!(served.reason.contains("task=general via default"));
    }

    #[test]
    fn extract_upstream_error_reads_openai_anthropic_and_plain_text() {
        let (code, message) = extract_upstream_error(
            br#"{"error":{"code":"context_length_exceeded","message":"This model's maximum context length is 8192 tokens"}}"#,
        );
        assert_eq!(code.as_deref(), Some("context_length_exceeded"));
        assert!(message
            .as_deref()
            .unwrap()
            .contains("maximum context length"));

        let (code, message) = extract_upstream_error(
            br#"{"error":{"type":"invalid_request_error","message":"bad tools"}}"#,
        );
        assert_eq!(code.as_deref(), Some("invalid_request_error"));
        assert_eq!(message.as_deref(), Some("bad tools"));

        let (code, message) = extract_upstream_error(
            br#"{"error":{"code":400,"status":"INVALID_ARGUMENT","message":"blocked"}}"#,
        );
        assert_eq!(code.as_deref(), Some("INVALID_ARGUMENT"));
        assert_eq!(message.as_deref(), Some("blocked"));

        let (code, message) = extract_upstream_error(b"plain failure");
        assert_eq!(code, None);
        assert_eq!(message.as_deref(), Some("plain failure"));
    }

    #[test]
    fn routing_reason_explains_selection_task_and_upstream_error() {
        let evaluation = RoutingEvaluation {
            alias: "assistant".into(),
            mode: "fixed".into(),
            task: "coding".into(),
            task_source: "rule".into(),
            task_rule_id: Some("builtin-coding".into()),
            decision: crate::routing::RoutingDecision {
                task: "coding".into(),
                ranked: vec![],
                excluded: vec![crate::routing::ExcludedCandidate {
                    target_id: "small".into(),
                    reason: "context_window".into(),
                }],
            },
            ordered_target_ids: vec!["first".into(), "second".into()],
            primary_target_ids: vec!["first".into(), "second".into()],
            fallback_target_ids: vec![],
            shadow_target_id: None,
            half_open_target_ids: vec![],
            estimated_input_tokens: 10,
            peer_latency_ms: None,
        };
        let outcome = RoutingAttemptOutcome {
            error_code: Some("context_length_exceeded".into()),
            error_message: Some("too long".into()),
            ..RoutingAttemptOutcome::from_previous(400, Duration::from_millis(5), false, None)
        };
        let reason = routing_attempt_reason(&evaluation, "first", &outcome);
        assert!(reason.contains("performance hop 1/2"));
        assert!(reason.contains("task=coding via rule builtin-coding"));
        assert!(reason.contains("error 400 context_length_exceeded: too long"));
        assert!(reason.contains("skipped small:context_window"));

        let follow = RoutingAttemptOutcome::from_previous(
            200,
            Duration::from_millis(5),
            false,
            Some(&(400, Some("context_length_exceeded".into()))),
        );
        let follow_reason = routing_attempt_reason(&evaluation, "second", &follow);
        assert!(follow_reason.contains("fallback after 400 context_length_exceeded"));
        assert!(follow_reason.contains("performance hop 2/2"));

        let retry = RoutingAttemptOutcome {
            same_target_attempt: 2,
            error_code: Some("overloaded".into()),
            error_message: Some("Service temporarily overloaded".into()),
            ..RoutingAttemptOutcome::from_previous(
                503,
                Duration::from_millis(5),
                false,
                Some(&(503, Some("overloaded".into()))),
            )
        };
        let retry_reason = routing_attempt_reason(&evaluation, "first", &retry);
        assert!(retry_reason.contains("retry 2/2 same target after 503 overloaded"));
    }

    #[test]
    fn local_session_header_is_optional_bounded_and_trimmed() {
        assert_eq!(validated_session_id(None).unwrap(), None);
        let value = HeaderValue::from_static(" session-1 ");
        assert_eq!(
            validated_session_id(Some(&value)).unwrap(),
            Some("session-1")
        );
        let long = HeaderValue::from_str(&"x".repeat(129)).unwrap();
        assert!(validated_session_id(Some(&long)).is_err());
    }

    #[tokio::test]
    async fn mlx_named_kv_hits_slots_only_with_a_session_header() {
        let slot_calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let chat_headers = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(
            Option<String>,
            Option<String>,
        )>::new()));
        let slot_for_server = slot_calls.clone();
        let headers_for_server = chat_headers.clone();
        let app = Router::new()
            .route(
                "/v1/chat/completions",
                axum::routing::post({
                    let headers_for_server = headers_for_server.clone();
                    move |headers: HeaderMap| {
                        let headers_for_server = headers_for_server.clone();
                        async move {
                            headers_for_server.lock().unwrap().push((
                                headers
                                    .get("x-local-ai-cache-namespace")
                                    .and_then(|value| value.to_str().ok())
                                    .map(str::to_owned),
                                headers
                                    .get("x-local-ai-session")
                                    .and_then(|value| value.to_str().ok())
                                    .map(str::to_owned),
                            ));
                            (
                                StatusCode::OK,
                                Json(json!({"id":"ok","choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]})),
                            )
                        }
                    }
                }),
            )
            .route(
                "/slots/0",
                axum::routing::post({
                    let slot_for_server = slot_for_server.clone();
                    move |axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>| {
                        let slot_for_server = slot_for_server.clone();
                        async move {
                            slot_for_server
                                .lock()
                                .unwrap()
                                .push(query.get("action").cloned().unwrap_or_default());
                            (StatusCode::OK, Json(json!({"ok": true})))
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let store = Store::memory().await.unwrap();
        let mut target = sample_target(
            "mlx-chat",
            "qwen-local",
            Some(format!("http://{address}/v1")),
        );
        target.kind = TargetKind::Mlx;
        target.local.runtime_engine = Some("mlx_chat".into());
        store.upsert_target(&target).await.unwrap();

        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        secrets.set(LOCAL_API_KEY, "test-token").unwrap();
        let core = AppCore::new(store, secrets).unwrap();
        core.migrate_legacy_local_api_key().await.unwrap();
        let cache = tempfile::tempdir().unwrap();
        let runtimes = Arc::new(RuntimeManager::new(
            PathBuf::new(),
            cache.path().to_path_buf(),
            crate::resource::ResourcePolicy::preset(crate::resource::ResourceProfile::Stealth, 8),
            core.local_activity(),
        ));
        let child = tokio::process::Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        runtimes.insert_test_runtime(&target.id, address.port(), child);

        let app = managed_router(Arc::new(core), runtimes);
        let with_session = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .header("x-local-ai-session", "chat-1")
                    .body(Body::from(
                        r#"{"model":"qwen-local","messages":[{"role":"user","content":"hello"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(with_session.status(), StatusCode::OK);
        assert_eq!(slot_calls.lock().unwrap().as_slice(), ["save"]);
        assert_eq!(
            chat_headers.lock().unwrap()[0],
            (Some("default".into()), Some("chat-1".into()))
        );

        slot_calls.lock().unwrap().clear();
        let without_session = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"qwen-local","messages":[{"role":"user","content":"hello again"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(without_session.status(), StatusCode::OK);
        assert!(slot_calls.lock().unwrap().is_empty());
        assert_eq!(
            chat_headers.lock().unwrap()[1],
            (Some("default".into()), None)
        );
    }
}

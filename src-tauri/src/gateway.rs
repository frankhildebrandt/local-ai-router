use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    body::{Body, Bytes},
    extract::{OriginalUri, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::Response,
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use futures_util::StreamExt;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    core::{AppCore, InFlightGuard, InFlightRequest},
    domain::{
        first_byte_timeout_ms, is_fallback_status, is_transient_status, RATE_LIMIT_DEFAULT_SECS,
        SLOW_WINDOW_SECS,
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
    storage::RequestLog,
};

#[derive(Clone)]
struct GatewayState {
    core: Arc<AppCore>,
    runtimes: Option<Arc<RuntimeManager>>,
}

pub fn router(core: Arc<AppCore>) -> Router {
    router_with_state(GatewayState {
        core,
        runtimes: None,
    })
}

pub fn managed_router(core: Arc<AppCore>, runtimes: Arc<RuntimeManager>) -> Router {
    router_with_state(GatewayState {
        core,
        runtimes: Some(runtimes),
    })
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
        .fallback(not_found)
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
    if authenticated_key_id(&core, &headers).await.is_none() {
        return protocol_error(
            PublicProtocol::Gemini,
            StatusCode::UNAUTHORIZED,
            "invalid_api_key",
            "Invalid local API key",
        );
    }
    match advertised_routes(&core).await {
        Ok(routes) => json_response(
            StatusCode::OK,
            json!({"models": routes.into_iter().filter(|route| route.enabled).map(|route| json!({"name":format!("models/{}",route.alias),"displayName":route.alias,"supportedGenerationMethods":["generateContent","streamGenerateContent"]})).collect::<Vec<_>>() }),
        ),
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

async fn not_found() -> Response<Body> {
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
    if authenticated_key_id(&core, &headers).await.is_none() {
        return unauthorized();
    }
    match advertised_routes(&core).await {
        Ok(routes) => json_response(
            StatusCode::OK,
            json!({
                "object": "list",
                "data": routes.into_iter().filter(|route| route.enabled).map(|route| json!({
                    "id": route.alias, "object": "model", "created": 0, "owned_by": "local-ai-router", "capabilities": route.capabilities
                })).collect::<Vec<_>>()
            }),
        ),
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
    let Some(api_key_id) = authenticated_key_id(&core, &headers).await else {
        return request_error(
            public_protocol,
            StatusCode::UNAUTHORIZED,
            "invalid_api_key",
            "Invalid local API key",
        );
    };
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
        if canonical.reasoning.is_some() {
            required_capabilities.push("reasoning".into());
        }
        if canonical.response_format.is_some() {
            required_capabilities.push("structured_output".into());
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
    let mut inflight = InFlightGuard::new(
        core.traffic.clone(),
        InFlightRequest {
            id: request_id.clone(),
            started_at: Utc::now(),
            endpoint: uri.path().into(),
            alias: alias.clone(),
            target_id: None,
            target_name: None,
            phase: "trying".into(),
        },
    );
    let target_ids = evaluation.ordered_target_ids.clone();
    if target_ids.is_empty() {
        if evaluation.mode == "adaptive" {
            record_routing_attempt(
                &core,
                &evaluation,
                &request_id,
                "none",
                RoutingAttemptOutcome {
                    status: 503,
                    transient_failure: false,
                    retry_after_until: None,
                    latency: Duration::ZERO,
                    ttft: None,
                    streaming: is_stream,
                },
            )
            .await;
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
    let total_targets = target_ids.len() as i64;

    let mut attempts = 0i64;
    let mut last_error: Option<Response<Body>> = None;
    let mut last_translation_error: Option<String> = None;
    let mut last_capability_error: Option<&str> = None;
    for target_id in &target_ids {
        attempts += 1;
        if evaluation.mode == "adaptive"
            && evaluation.half_open_target_ids.contains(target_id)
            && !core
                .store
                .claim_half_open(target_id, &evaluation.task)
                .await
                .unwrap_or(false)
        {
            continue;
        }
        let attempt_started = Instant::now();
        let Ok(Some(mut target)) = core.store.target(target_id).await else {
            continue;
        };
        inflight.update(&target.id, &target.name, "trying");
        if !target_supports_capability(&target, capability) {
            last_capability_error =
                Some("No target in this alias supports the requested capability");
            continue;
        }
        if decoded.is_none()
            && !matches!(
                target.wire_protocol,
                WireProtocol::OpenAiChat | WireProtocol::OpenAiResponses
            )
        {
            continue;
        }
        let mut canonical = decoded.clone();
        if let Some(canonical) = canonical.as_ref() {
            let required = [
                (is_stream, "streaming"),
                (!canonical.tools.is_empty(), "tools"),
                (canonical.reasoning.is_some(), "reasoning"),
                (canonical.response_format.is_some(), "structured_output"),
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
            ];
            if required.into_iter().any(|(needed, capability)| {
                needed && !target.capabilities.iter().any(|item| item == capability)
            }) {
                last_capability_error =
                    Some("No target in this alias supports the requested media capability");
                continue;
            }
        }
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
                    Some(runtimes) => runtimes.start(&target).await,
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
                Err(error) if attempts < total_targets => {
                    tracing::warn!(target = %target.id, %error, "local model load failed; trying fallback");
                    continue;
                }
                Err(error) => {
                    return request_error(
                        public_protocol,
                        StatusCode::SERVICE_UNAVAILABLE,
                        "local_load_failed",
                        &format!("The local model could not be loaded: {error}"),
                    );
                }
            }
        }
        let local_permit = match core.acquire_local_slot(&target).await {
            Ok(permit) => permit,
            Err(error) if attempts < total_targets => {
                tracing::warn!(target = %target.id, %error, "local admission failed; trying fallback");
                continue;
            }
            Err(error) => {
                return request_error(
                    public_protocol,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "local_busy",
                    &format!("The local model could not admit the request: {error}"),
                )
            }
        };
        let Ok((base_url, credential, account_id)) = core.target_endpoint(&target).await else {
            continue;
        };
        let kv_context = if target.kind == crate::domain::TargetKind::Gguf {
            match (runtimes.as_ref(), session_id.as_deref()) {
                (Some(runtimes), Some(session_id)) => {
                    if let Err(error) = runtimes.restore_kv(&target, &api_key_id, session_id).await
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
        let upstream_path = if provider
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
        let upstream_url =
            if target.wire_protocol == WireProtocol::GeminiGenerateContent && canonical.is_some() {
                format!(
                    "{}/{}",
                    base_url.trim_end_matches('/'),
                    upstream_path.trim_start_matches('/')
                )
            } else {
                join_api_url(&base_url, &upstream_path)
            };
        let mut request = core.client.post(upstream_url);
        if let Some(payload) = json_payload.as_mut() {
            let outbound = if let Some(canonical) = canonical.as_ref() {
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
                            continue;
                        }
                    }
                    match encode_request(target.wire_protocol, canonical, &target.provider_model) {
                        Ok(value) => value,
                        Err(error) => {
                            last_translation_error = Some(error.to_string());
                            continue;
                        }
                    }
                }
            } else {
                payload["model"] = Value::String(target.provider_model.clone());
                payload.clone()
            };
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
        if is_openrouter {
            request = request
                .header("HTTP-Referer", "https://local-ai-router.app")
                .header("X-Title", "Local AI Router");
        }
        let has_fallback = attempts < total_targets;
        let attempt_timeout = Duration::from_millis(first_byte_timeout_ms(
            evaluation.peer_latency_ms,
            has_fallback,
        ));
        match tokio::time::timeout(attempt_timeout, request.send()).await {
            Ok(Ok(upstream)) => {
                let status = upstream.status();
                let retry_after_until = rate_limit_until(upstream.headers(), status.as_u16());
                if status.is_redirection() {
                    log_request(
                        &core,
                        LogMetadata {
                            id: &request_id,
                            api_key_id: &api_key_id,
                            endpoint: uri.path(),
                            alias: Some(&alias),
                            target: Some(&target.name),
                            attempts,
                            status: 502,
                            latency_ms: started.elapsed().as_millis() as i64,
                            usage: (None, None),
                            error_code: Some("credential_redirect_rejected"),
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
                if is_fallback_status(status.as_u16()) && has_fallback {
                    record_routing_attempt(
                        &core,
                        &evaluation,
                        &request_id,
                        &target.id,
                        RoutingAttemptOutcome {
                            status: status.as_u16(),
                            transient_failure: is_transient_status(status.as_u16()),
                            retry_after_until,
                            latency: attempt_started.elapsed(),
                            ttft: None,
                            streaming: is_stream,
                        },
                    )
                    .await;
                    continue;
                }
                let mut usage = (None, None);
                let mut error_code = None;
                let mut attempt_ttft = None;
                let response = if is_stream && status.is_success() {
                    let translated_stream = public_protocol
                        .filter(|protocol| !protocol_matches(*protocol, target.wire_protocol));
                    let content_type = if translated_stream.is_some() {
                        Some(HeaderValue::from_static("text/event-stream"))
                    } else {
                        upstream.headers().get(header::CONTENT_TYPE).cloned()
                    };
                    let mut upstream_stream = upstream.bytes_stream();
                    let first_chunk =
                        match tokio::time::timeout(attempt_timeout, upstream_stream.next()).await {
                            Ok(Some(Ok(chunk))) => chunk,
                            Ok(Some(Err(_))) | Ok(None) | Err(_) if has_fallback => {
                                record_routing_attempt(
                                    &core,
                                    &evaluation,
                                    &request_id,
                                    &target.id,
                                    RoutingAttemptOutcome {
                                        status: 504,
                                        transient_failure: true,
                                        retry_after_until: Some(
                                            Utc::now()
                                                + chrono::Duration::seconds(SLOW_WINDOW_SECS),
                                        ),
                                        latency: attempt_started.elapsed(),
                                        ttft: None,
                                        streaming: true,
                                    },
                                )
                                .await;
                                continue;
                            }
                            Ok(Some(Err(_))) | Ok(None) | Err(_) => {
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
                                        streaming: true,
                                    },
                                )
                                .await;
                                last_error = Some(request_error(
                                    public_protocol,
                                    StatusCode::BAD_GATEWAY,
                                    "upstream_stream_error",
                                    "The upstream stream ended before producing data",
                                ));
                                continue;
                            }
                        };
                    attempt_ttft = Some(attempt_started.elapsed());
                    inflight.update(&target.id, &target.name, "streaming");
                    let stream_traffic = inflight.hub();
                    let stream_traffic_id = inflight.id().to_owned();
                    inflight.hand_off();
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
                    let stream = async_stream::stream! {
                        let _permit = local_permit;
                        let mut stream_ok = true;
                        let mut usage_buffer = Vec::new();
                        let mut translator = translated_stream.map(|protocol| StreamTranslator::new(stream_wire_protocol, protocol, &stream_model));
                        if let Some((input_tokens, output_tokens)) = extract_sse_usage(&mut usage_buffer, &first_chunk) {
                            let _ = stream_core.store.update_log_usage(&stream_request_id, input_tokens, output_tokens).await;
                        }
                        let first_output = translator.as_mut().map(|translator| Bytes::from(translator.push(&first_chunk))).unwrap_or(first_chunk);
                        if !first_output.is_empty() { yield Ok::<_, std::io::Error>(first_output); }
                        while let Some(chunk) = upstream_stream.next().await {
                            match chunk {
                                Ok(chunk) => {
                                    if let Some((input_tokens, output_tokens)) = extract_sse_usage(&mut usage_buffer, &chunk) {
                                        let _ = stream_core.store.update_log_usage(&stream_request_id, input_tokens, output_tokens).await;
                                    }
                                    let output = translator.as_mut().map(|translator| Bytes::from(translator.push(&chunk))).unwrap_or(chunk);
                                    if !output.is_empty() { yield Ok(output); }
                                }
                                Err(error) => {
                                    stream_ok = false;
                                    record_routing_attempt(
                                        &stream_core,
                                        &stream_evaluation,
                                        &stream_request_id,
                                        &stream_target_id,
                                        RoutingAttemptOutcome {
                                            status: 502,
                                            transient_failure: true,
                                            retry_after_until: None,
                                            latency: stream_attempt_started.elapsed(),
                                            ttft: stream_ttft,
                                            streaming: true,
                                        },
                                    ).await;
                                    yield Err(std::io::Error::other(error));
                                    break;
                                }
                            }
                        }
                        if stream_ok {
                            if let Some((runtimes, target, api_key_id, session_id)) = stream_kv {
                                if let Err(error) = runtimes.save_kv(&target, &api_key_id, &session_id).await {
                                    tracing::warn!(target = %target.id, %error, "KV snapshot save failed");
                                }
                            }
                            record_routing_attempt(
                                &stream_core,
                                &stream_evaluation,
                                &stream_request_id,
                                &stream_target_id,
                                RoutingAttemptOutcome {
                                    status: status.as_u16(),
                                    transient_failure: false,
                                    retry_after_until: None,
                                    latency: stream_attempt_started.elapsed(),
                                    ttft: stream_ttft,
                                    streaming: true,
                                },
                            ).await;
                        }
                        drop(_permit);
                        stream_traffic.finish(&stream_traffic_id);
                        if let Some(runtimes) = stream_runtimes.as_ref() {
                            runtimes.reap_over_budget().await;
                            sync_runtime_states(&stream_core, runtimes).await;
                        }
                    };
                    response_from_body(status, content_type, Body::from_stream(stream), &request_id)
                } else {
                    let content_type = upstream.headers().get(header::CONTENT_TYPE).cloned();
                    let bytes = match tokio::time::timeout(attempt_timeout, upstream.bytes()).await
                    {
                        Ok(Ok(bytes)) => bytes,
                        Ok(Err(_)) | Err(_) if has_fallback => {
                            record_routing_attempt(
                                &core,
                                &evaluation,
                                &request_id,
                                &target.id,
                                RoutingAttemptOutcome {
                                    status: 504,
                                    transient_failure: true,
                                    retry_after_until: Some(
                                        Utc::now() + chrono::Duration::seconds(SLOW_WINDOW_SECS),
                                    ),
                                    latency: attempt_started.elapsed(),
                                    ttft: None,
                                    streaming: false,
                                },
                            )
                            .await;
                            continue;
                        }
                        Ok(Err(_)) | Err(_) => {
                            record_routing_attempt(
                                &core,
                                &evaluation,
                                &request_id,
                                &target.id,
                                RoutingAttemptOutcome {
                                    status: 504,
                                    transient_failure: true,
                                    retry_after_until: None,
                                    latency: attempt_started.elapsed(),
                                    ttft: None,
                                    streaming: false,
                                },
                            )
                            .await;
                            last_error = Some(request_error(
                                public_protocol,
                                StatusCode::GATEWAY_TIMEOUT,
                                "upstream_body_timeout",
                                "The upstream response body did not complete",
                            ));
                            continue;
                        }
                    };
                    let mut response_bytes = bytes;
                    if let Ok(value) = serde_json::from_slice::<Value>(&response_bytes) {
                        usage = usage_from_value(&value);
                        error_code = value
                            .get("error")
                            .and_then(|error| error.get("code"))
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                        if status.is_success() {
                            if let Some(protocol) = public_protocol {
                                if !protocol_matches(protocol, target.wire_protocol) {
                                    match decode_response(target.wire_protocol, &value) {
                                        Ok(canonical_response) => {
                                            response_bytes = encode_response(
                                                protocol,
                                                &canonical_response,
                                                &alias,
                                            )
                                            .to_string()
                                            .into()
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
                            if !protocol_matches(protocol, target.wire_protocol) {
                                let message = value
                                    .pointer("/error/message")
                                    .or_else(|| value.get("message"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("The upstream rejected the request");
                                let code = value
                                    .pointer("/error/code")
                                    .or_else(|| value.pointer("/error/type"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("upstream_error");
                                response_bytes =
                                    protocol_error_value(protocol, status, code, message)
                                        .to_string()
                                        .into();
                            }
                        }
                    } else if status.is_success()
                        && public_protocol.is_some_and(|protocol| {
                            !protocol_matches(protocol, target.wire_protocol)
                        })
                    {
                        log_request(
                            &core,
                            LogMetadata {
                                id: &request_id,
                                api_key_id: &api_key_id,
                                endpoint: uri.path(),
                                alias: Some(&alias),
                                target: Some(&target.name),
                                attempts,
                                status: 502,
                                latency_ms: started.elapsed().as_millis() as i64,
                                usage: (None, None),
                                error_code: Some("invalid_upstream_response"),
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
                log_request(
                    &core,
                    LogMetadata {
                        id: &request_id,
                        api_key_id: &api_key_id,
                        endpoint: uri.path(),
                        alias: Some(&alias),
                        target: Some(&target.name),
                        attempts,
                        status: status.as_u16(),
                        latency_ms: started.elapsed().as_millis() as i64,
                        usage,
                        error_code: error_code.as_deref(),
                    },
                )
                .await;
                if !is_stream {
                    record_routing_attempt(
                        &core,
                        &evaluation,
                        &request_id,
                        &target.id,
                        RoutingAttemptOutcome {
                            status: status.as_u16(),
                            transient_failure: is_transient_status(status.as_u16()),
                            retry_after_until,
                            latency: attempt_started.elapsed(),
                            ttft: attempt_ttft,
                            streaming: false,
                        },
                    )
                    .await;
                }
                return with_routing_headers(response, &evaluation, &target.id);
            }
            Ok(Err(error)) => {
                last_error = Some(request_error(
                    public_protocol,
                    StatusCode::BAD_GATEWAY,
                    "upstream_unavailable",
                    "The selected backend could not be reached",
                ));
                if attempts >= total_targets {
                    log_request(
                        &core,
                        LogMetadata {
                            id: &request_id,
                            api_key_id: &api_key_id,
                            endpoint: uri.path(),
                            alias: Some(&alias),
                            target: Some(&target.name),
                            attempts,
                            status: 502,
                            latency_ms: started.elapsed().as_millis() as i64,
                            usage: (None, None),
                            error_code: Some(if error.is_timeout() {
                                "timeout"
                            } else {
                                "network_error"
                            }),
                        },
                    )
                    .await;
                }
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
                    },
                )
                .await;
            }
            Err(_) => {
                last_error = Some(request_error(
                    public_protocol,
                    StatusCode::GATEWAY_TIMEOUT,
                    "timeout",
                    "The selected backend timed out",
                ));
                if attempts >= total_targets {
                    log_request(
                        &core,
                        LogMetadata {
                            id: &request_id,
                            api_key_id: &api_key_id,
                            endpoint: uri.path(),
                            alias: Some(&alias),
                            target: Some(&target.name),
                            attempts,
                            status: 504,
                            latency_ms: started.elapsed().as_millis() as i64,
                            usage: (None, None),
                            error_code: Some("timeout"),
                        },
                    )
                    .await;
                }
                record_routing_attempt(
                    &core,
                    &evaluation,
                    &request_id,
                    &target.id,
                    RoutingAttemptOutcome {
                        status: 504,
                        transient_failure: true,
                        retry_after_until: has_fallback
                            .then(|| Utc::now() + chrono::Duration::seconds(SLOW_WINDOW_SECS)),
                        latency: attempt_started.elapsed(),
                        ttft: None,
                        streaming: is_stream,
                    },
                )
                .await;
            }
        }
    }
    if let Some(error) = last_error {
        return error;
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
            error,
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

struct RoutingAttemptOutcome {
    status: u16,
    transient_failure: bool,
    retry_after_until: Option<chrono::DateTime<Utc>>,
    latency: Duration,
    ttft: Option<Duration>,
    streaming: bool,
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
    let mut reason = if let Some(shadow) = evaluation.shadow_target_id.as_deref() {
        format!("{};shadow={shadow}", evaluation.task_source)
    } else {
        evaluation.task_source.clone()
    };
    if !evaluation.decision.excluded.is_empty() {
        reason.push_str(";excluded=");
        reason.push_str(
            &evaluation
                .decision
                .excluded
                .iter()
                .map(|candidate| format!("{}:{}", candidate.target_id, candidate.reason))
                .collect::<Vec<_>>()
                .join("|"),
        );
    }
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
) -> Response<Body> {
    let ranked = evaluation
        .decision
        .ranked
        .iter()
        .find(|candidate| candidate.target_id == target_id);
    let reason = ranked
        .map(|candidate| {
            format!(
                "{};score={:.4};cost={}",
                evaluation.task_source,
                candidate.score.total,
                if candidate.cost_verified {
                    "verified"
                } else {
                    "unknown"
                }
            )
        })
        .unwrap_or_else(|| evaluation.task_source.clone());
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

fn usage_from_value(value: &Value) -> (Option<i64>, Option<i64>) {
    let usage = value
        .get("usage")
        .or_else(|| {
            value
                .get("response")
                .and_then(|response| response.get("usage"))
        })
        .or_else(|| value.get("usageMetadata"));
    let input = usage
        .and_then(|usage| {
            usage
                .get("prompt_tokens")
                .or_else(|| usage.get("input_tokens"))
                .or_else(|| usage.get("promptTokenCount"))
        })
        .and_then(Value::as_i64);
    let output = usage
        .and_then(|usage| {
            usage
                .get("completion_tokens")
                .or_else(|| usage.get("output_tokens"))
                .or_else(|| usage.get("candidatesTokenCount"))
        })
        .and_then(Value::as_i64);
    (input, output)
}

fn extract_sse_usage(buffer: &mut Vec<u8>, chunk: &[u8]) -> Option<(Option<i64>, Option<i64>)> {
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
            if usage.0.is_some() || usage.1.is_some() {
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
    if capability == "speech"
        && matches!(
            target.kind,
            crate::domain::TargetKind::Gguf | crate::domain::TargetKind::Mlx
        )
    {
        return target.capabilities.iter().any(|item| item == "speech");
    }
    target.capabilities.iter().any(|item| item == capability)
        || (capability == "speech" && target.capabilities.iter().any(|item| item == "audio"))
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

async fn authenticated_key_id(core: &AppCore, headers: &HeaderMap) -> Option<String> {
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
    core.authorized_token(candidate).await
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
    api_key_id: &'a str,
    endpoint: &'a str,
    alias: Option<&'a str>,
    target: Option<&'a str>,
    attempts: i64,
    status: u16,
    latency_ms: i64,
    usage: (Option<i64>, Option<i64>),
    error_code: Option<&'a str>,
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
            input_tokens: metadata.usage.0,
            output_tokens: metadata.usage.1,
            error_code: metadata.error_code.map(str::to_owned),
            api_key_id: Some(metadata.api_key_id.into()),
            api_key_name: None,
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::AppCore,
        domain::{ModelRoute, RouteTarget, TargetKind},
        providers::AuthMode,
        secrets::{MemorySecrets, SecretStore, LOCAL_API_KEY},
        storage::{ModelTarget, Provider, Store},
    };
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use std::sync::Arc;
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
            Some((Some(3), Some(5)))
        );

        let mut responses_buffer = Vec::new();
        assert_eq!(
            extract_sse_usage(
                &mut responses_buffer,
                b"data: {\"response\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":11}}}\n\n",
            ),
            Some((Some(7), Some(11)))
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
        let app = Router::new().fallback(move || {
            let payload = payload.clone();
            async move { (status, Json(payload)) }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}/v1")
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
                    },
                    RouteTarget {
                        id: "second".into(),
                        kind: TargetKind::Gguf,
                        model: "fallback".into(),
                        priority: 20,
                        enabled: true,
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
        assert_eq!(logs[0].attempts, 2);
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
                    },
                    RouteTarget {
                        id: "second".into(),
                        kind: TargetKind::Gguf,
                        model: "fallback".into(),
                        priority: 20,
                        enabled: true,
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
                    },
                    RouteTarget {
                        id: "safer".into(),
                        kind: TargetKind::Alias,
                        model: "safer".into(),
                        priority: 20,
                        enabled: true,
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
                    },
                    RouteTarget {
                        id: "fast".into(),
                        kind: TargetKind::Gguf,
                        model: "fast".into(),
                        priority: 20,
                        enabled: true,
                    },
                    RouteTarget {
                        id: "other".into(),
                        kind: TargetKind::Gguf,
                        model: "other".into(),
                        priority: 30,
                        enabled: true,
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
        assert!(started.elapsed() < Duration::from_secs(15));
        assert_eq!(
            store.logs(10).await.unwrap()[0].target.as_deref(),
            Some("fast")
        );
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

    #[tokio::test]
    async fn inflight_appears_during_proxy_and_clears_after_response() {
        let hang = hanging_upstream(Duration::from_millis(300)).await;
        let store = Store::memory().await.unwrap();
        store
            .upsert_target(&sample_target("primary", "primary", Some(hang)))
            .await
            .unwrap();
        store
            .upsert_route(&ModelRoute {
                alias: "assistant".into(),
                enabled: true,
                capabilities: vec!["chat".into()],
                targets: vec![RouteTarget {
                    id: "primary".into(),
                    kind: TargetKind::Gguf,
                    model: "primary".into(),
                    priority: 10,
                    enabled: true,
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
        let pending = tokio::spawn(async move {
            app.oneshot(
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
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let inflight = core.traffic.snapshot();
            if let Some(request) = inflight.first() {
                assert_eq!(request.alias, "assistant");
                assert_eq!(request.endpoint, "/v1/chat/completions");
                break;
            }
            if Instant::now() > deadline {
                panic!("in-flight request never appeared");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let response = pending.await.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(core.traffic.snapshot().is_empty());
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
                    },
                    RouteTarget {
                        id: "high".into(),
                        kind: TargetKind::Gguf,
                        model: "high".into(),
                        priority: 20,
                        enabled: true,
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
                    },
                    RouteTarget {
                        id: "audio".into(),
                        kind: TargetKind::Mlx,
                        model: "vlm".into(),
                        priority: 20,
                        enabled: true,
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
}

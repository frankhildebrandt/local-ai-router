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
    core::AppCore,
    domain::is_transient_status,
    protocol::{
        decode_request, decode_response, encode_request, encode_response, validate_cross_protocol,
        PublicProtocol, StreamTranslator,
    },
    providers::{provider_preset, AuthScheme, WireProtocol},
    storage::RequestLog,
};

pub fn router(core: Arc<AppCore>) -> Router {
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
        .with_state(core)
}

async fn gemini_models(
    State(core): State<Arc<AppCore>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response<Body> {
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
    match core.store.routes().await {
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
    State(core): State<Arc<AppCore>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response<Body> {
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
    match core.store.routes().await {
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
    State(core): State<Arc<AppCore>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
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
    let Ok(Some(route)) = core.store.route(&alias).await else {
        return request_error(
            public_protocol,
            StatusCode::NOT_FOUND,
            "model_not_found",
            "Unknown or unavailable model alias",
        );
    };
    if !route.enabled {
        return request_error(
            public_protocol,
            StatusCode::NOT_FOUND,
            "model_not_found",
            "Unknown or unavailable model alias",
        );
    }
    let capability = endpoint_capability(uri.path());
    if !route.capabilities.iter().any(|item| item == capability) {
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
    let canonical = match (public_protocol, json_payload.as_ref()) {
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
    let targets = route.ordered_targets();
    if targets.is_empty() {
        return request_error(
            public_protocol,
            StatusCode::SERVICE_UNAVAILABLE,
            "no_targets",
            "This alias has no enabled targets",
        );
    }
    let total_targets = targets.len() as i64;

    let mut attempts = 0i64;
    let mut last_error: Option<Response<Body>> = None;
    let mut last_translation_error: Option<String> = None;
    for route_target in targets {
        attempts += 1;
        let Ok(Some(target)) = core.store.target(&route_target.id).await else {
            continue;
        };
        if !target.capabilities.iter().any(|item| item == capability) {
            continue;
        }
        if canonical.is_none()
            && !matches!(
                target.wire_protocol,
                WireProtocol::OpenAiChat | WireProtocol::OpenAiResponses
            )
        {
            continue;
        }
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
            ];
            if required.into_iter().any(|(needed, capability)| {
                needed && !target.capabilities.iter().any(|item| item == capability)
            }) {
                continue;
            }
        }
        let local_permit = match core.acquire_local_slot(&target).await {
            Ok(permit) => permit,
            Err(_) if attempts < total_targets => continue,
            Err(_) => {
                return request_error(
                    public_protocol,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "local_busy",
                    "The local model queue is full",
                )
            }
        };
        let Ok((base_url, credential, account_id)) = core.target_endpoint(&target).await else {
            continue;
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
            let outbound = if let (Some(protocol), Some(canonical)) =
                (public_protocol, canonical.as_ref())
            {
                if protocol_matches(protocol, target.wire_protocol) {
                    let mut native = payload.clone();
                    if protocol != PublicProtocol::Gemini {
                        native["model"] = Value::String(target.provider_model.clone());
                    }
                    native
                } else {
                    if let Err(error) = validate_cross_protocol(protocol, payload) {
                        last_translation_error = Some(error.to_string());
                        continue;
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
        match tokio::time::timeout(Duration::from_secs(120), request.send()).await {
            Ok(Ok(upstream)) => {
                let status = upstream.status();
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
                if is_transient_status(status.as_u16()) && attempts < total_targets {
                    continue;
                }
                let mut usage = (None, None);
                let mut error_code = None;
                let response = if is_stream && status.is_success() {
                    let translated_stream = public_protocol
                        .filter(|protocol| !protocol_matches(*protocol, target.wire_protocol));
                    let content_type = if translated_stream.is_some() {
                        Some(HeaderValue::from_static("text/event-stream"))
                    } else {
                        upstream.headers().get(header::CONTENT_TYPE).cloned()
                    };
                    let mut upstream_stream = upstream.bytes_stream();
                    let first_chunk = match tokio::time::timeout(
                        Duration::from_secs(120),
                        upstream_stream.next(),
                    )
                    .await
                    {
                        Ok(Some(Ok(chunk))) => chunk,
                        Ok(Some(Err(_))) | Ok(None) | Err(_) if attempts < total_targets => {
                            continue
                        }
                        Ok(Some(Err(_))) | Ok(None) | Err(_) => {
                            last_error = Some(request_error(
                                public_protocol,
                                StatusCode::BAD_GATEWAY,
                                "upstream_stream_error",
                                "The upstream stream ended before producing data",
                            ));
                            continue;
                        }
                    };
                    let stream_core = core.clone();
                    let stream_request_id = request_id.clone();
                    let stream_model = alias.clone();
                    let stream_wire_protocol = target.wire_protocol;
                    let stream = async_stream::stream! {
                        let _permit = local_permit;
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
                                Err(error) => yield Err(std::io::Error::other(error)),
                            }
                        }
                    };
                    response_from_body(status, content_type, Body::from_stream(stream), &request_id)
                } else {
                    let content_type = upstream.headers().get(header::CONTENT_TYPE).cloned();
                    let bytes = match tokio::time::timeout(
                        Duration::from_secs(120),
                        upstream.bytes(),
                    )
                    .await
                    {
                        Ok(Ok(bytes)) => bytes,
                        Ok(Err(_)) | Err(_) if attempts < total_targets => continue,
                        Ok(Err(_)) | Err(_) => {
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
                return response;
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
    request_error(
        public_protocol,
        StatusCode::SERVICE_UNAVAILABLE,
        "no_available_target",
        "No configured target is currently available",
    )
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
        path if path.starts_with("/v1/audio/") => "audio",
        "/v1/moderations" => "moderation",
        _ => "unknown",
    }
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
        let app=Router::new().fallback(move |headers:HeaderMap| { let expected=expected.clone(); async move { if expected.iter().all(|(name,value)|headers.get(*name).and_then(|header|header.to_str().ok())==Some(*value)) { (StatusCode::OK,Json(json!({"id":"ok","choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}))) } else { (StatusCode::UNAUTHORIZED,Json(json!({"error":{"code":"bad_auth"}}))) } } });
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
            let response=router(Arc::new(core)).oneshot(Request::builder().method("POST").uri("/v1/chat/completions").header("authorization","Bearer local-key").header("content-type","application/json").body(Body::from(r#"{"model":"assistant","messages":[{"role":"user","content":"hello"}]}"#)).unwrap()).await.unwrap();
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
}

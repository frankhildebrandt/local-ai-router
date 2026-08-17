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

use crate::{core::AppCore, domain::is_transient_status, storage::RequestLog};

pub fn router(core: Arc<AppCore>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(proxy))
        .route("/v1/responses", post(proxy))
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

async fn models(State(core): State<Arc<AppCore>>, headers: HeaderMap) -> Response<Body> {
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
    let Some(api_key_id) = authenticated_key_id(&core, &headers).await else {
        return unauthorized();
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
                return openai_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_json",
                    "Request body must be valid JSON",
                )
            }
        }
    } else if content_type.starts_with("multipart/form-data") {
        None
    } else {
        return openai_error(
            StatusCode::BAD_REQUEST,
            "unsupported_content_type",
            "Request body must be JSON or multipart/form-data",
        );
    };
    let alias = json_payload
        .as_ref()
        .and_then(|payload| payload.get("model"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| extract_multipart_model(&body).map(str::to_owned));
    let Some(alias) = alias else {
        return openai_error(
            StatusCode::BAD_REQUEST,
            "model_required",
            "The model field is required",
        );
    };
    let Ok(Some(route)) = core.store.route(&alias).await else {
        return openai_error(
            StatusCode::NOT_FOUND,
            "model_not_found",
            "Unknown or unavailable model alias",
        );
    };
    if !route.enabled {
        return openai_error(
            StatusCode::NOT_FOUND,
            "model_not_found",
            "Unknown or unavailable model alias",
        );
    }
    let capability = endpoint_capability(uri.path());
    if !route.capabilities.iter().any(|item| item == capability) {
        return openai_error(
            StatusCode::BAD_REQUEST,
            "unsupported_capability",
            "This alias does not support the requested capability",
        );
    }
    let is_stream = json_payload
        .as_ref()
        .and_then(|payload| payload.get("stream"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let targets = route.ordered_targets();
    if targets.is_empty() {
        return openai_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "no_targets",
            "This alias has no enabled targets",
        );
    }
    let total_targets = targets.len() as i64;

    let mut attempts = 0i64;
    let mut last_error: Option<Response<Body>> = None;
    for route_target in targets {
        attempts += 1;
        let Ok(Some(target)) = core.store.target(&route_target.id).await else {
            continue;
        };
        let local_permit = match core.acquire_local_slot(&target).await {
            Ok(permit) => permit,
            Err(_) if attempts < total_targets => continue,
            Err(_) => {
                return openai_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "local_busy",
                    "The local model queue is full",
                )
            }
        };
        let Ok((base_url, credential)) = core.target_endpoint(&target).await else {
            continue;
        };
        let mut request = core.client.post(join_api_url(&base_url, uri.path()));
        if let Some(payload) = json_payload.as_mut() {
            payload["model"] = Value::String(target.provider_model.clone());
            request = request.json(payload);
        } else {
            request = request
                .header(header::CONTENT_TYPE, content_type)
                .body(rewrite_multipart_model(&body, &target.provider_model));
        }
        if let Some(credential) = credential {
            request = request.bearer_auth(credential);
        }
        if matches!(target.kind, crate::domain::TargetKind::OpenRouter) {
            request = request
                .header("HTTP-Referer", "https://local-ai-router.app")
                .header("X-Title", "Local AI Router");
        }
        match tokio::time::timeout(Duration::from_secs(120), request.send()).await {
            Ok(Ok(upstream)) => {
                let status = upstream.status();
                if is_transient_status(status.as_u16()) && attempts < total_targets {
                    continue;
                }
                let mut usage = (None, None);
                let mut error_code = None;
                let response = if is_stream && status.is_success() {
                    let content_type = upstream.headers().get(header::CONTENT_TYPE).cloned();
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
                            last_error = Some(openai_error(
                                StatusCode::BAD_GATEWAY,
                                "upstream_stream_error",
                                "The upstream stream ended before producing data",
                            ));
                            continue;
                        }
                    };
                    let stream_core = core.clone();
                    let stream_request_id = request_id.clone();
                    let stream = async_stream::stream! {
                        let _permit = local_permit;
                        let mut usage_buffer = Vec::new();
                        if let Some((input_tokens, output_tokens)) = extract_sse_usage(&mut usage_buffer, &first_chunk) {
                            let _ = stream_core.store.update_log_usage(&stream_request_id, input_tokens, output_tokens).await;
                        }
                        yield Ok::<_, std::io::Error>(first_chunk);
                        while let Some(chunk) = upstream_stream.next().await {
                            match chunk {
                                Ok(chunk) => {
                                    if let Some((input_tokens, output_tokens)) = extract_sse_usage(&mut usage_buffer, &chunk) {
                                        let _ = stream_core.store.update_log_usage(&stream_request_id, input_tokens, output_tokens).await;
                                    }
                                    yield Ok(chunk);
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
                            last_error = Some(openai_error(
                                StatusCode::GATEWAY_TIMEOUT,
                                "upstream_body_timeout",
                                "The upstream response body did not complete",
                            ));
                            continue;
                        }
                    };
                    if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                        usage = usage_from_value(&value);
                        error_code = value
                            .get("error")
                            .and_then(|error| error.get("code"))
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                    }
                    response_from_body(status, content_type, Body::from(bytes), &request_id)
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
                last_error = Some(openai_error(
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
                last_error = Some(openai_error(
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
    last_error.unwrap_or_else(|| {
        openai_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "no_available_target",
            "No configured target is currently available",
        )
    })
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
    let usage = value.get("usage").or_else(|| {
        value
            .get("response")
            .and_then(|response| response.get("usage"))
    });
    let input = usage
        .and_then(|usage| {
            usage
                .get("prompt_tokens")
                .or_else(|| usage.get("input_tokens"))
        })
        .and_then(Value::as_i64);
    let output = usage
        .and_then(|usage| {
            usage
                .get("completion_tokens")
                .or_else(|| usage.get("output_tokens"))
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
        "/v1/chat/completions" | "/v1/responses" | "/v1/completions" => "chat",
        "/v1/embeddings" => "embeddings",
        path if path.starts_with("/v1/images/") => "images",
        path if path.starts_with("/v1/audio/") => "audio",
        "/v1/moderations" => "moderation",
        _ => "unknown",
    }
}

async fn authenticated_key_id(core: &AppCore, headers: &HeaderMap) -> Option<String> {
    core.authorized(
        headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok()),
    )
    .await
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
        secrets::{MemorySecrets, SecretStore, LOCAL_API_KEY},
        storage::{ModelTarget, Store},
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
}

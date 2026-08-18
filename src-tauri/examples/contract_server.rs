use std::sync::Arc;

use axum::{
    body::Body,
    extract::OriginalUri,
    http::{header, StatusCode},
    response::Response,
    routing::post,
    Json, Router,
};
use local_ai_router_lib::{
    core::AppCore,
    domain::{ModelRoute, RouteTarget, TargetKind},
    gateway,
    secrets::{MemorySecrets, SecretStore, LOCAL_API_KEY},
    storage::{ModelTarget, Store},
};
use serde_json::{json, Value};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let upstream_url = format!("http://{}/v1", upstream_listener.local_addr()?);
    tokio::spawn(async move {
        axum::serve(upstream_listener, Router::new().fallback(post(upstream)))
            .await
            .unwrap();
    });

    let store = Store::memory().await?;
    let target = ModelTarget {
        id: "contract-target".into(),
        provider_id: None,
        name: "Contract backend".into(),
        kind: TargetKind::Gguf,
        wire_protocol: local_ai_router_lib::providers::WireProtocol::OpenAiChat,
        provider_model: "real-model".into(),
        local_path: None,
        runtime_url: Some(upstream_url),
        capabilities: vec![
            "chat".into(),
            "streaming".into(),
            "tools".into(),
            "embeddings".into(),
        ],
        enabled: true,
        state: "ready".into(),
        size_bytes: None,
        local: local_ai_router_lib::storage::LocalModelMeta::default(),
    };
    store.upsert_target(&target).await?;
    store
        .upsert_route(&ModelRoute {
            alias: "sdk-model".into(),
            enabled: true,
            capabilities: target.capabilities.clone(),
            targets: vec![RouteTarget {
                id: target.id.clone(),
                kind: target.kind.clone(),
                model: target.provider_model.clone(),
                priority: 10,
                enabled: true,
                ..Default::default()
            }],
        })
        .await?;
    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
    secrets.set(LOCAL_API_KEY, "contract-token")?;
    let core = AppCore::new(store, secrets)?;
    core.migrate_legacy_local_api_key().await?;
    core.local_activity()
        .set_token("contract-target", "runtime-token".into());
    let core = Arc::new(core);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:11436").await?;
    println!("READY");
    axum::serve(listener, gateway::router(core)).await?;
    Ok(())
}

async fn upstream(OriginalUri(uri): OriginalUri, Json(payload): Json<Value>) -> Response<Body> {
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if model != "real-model" {
        return response(
            StatusCode::BAD_REQUEST,
            json!({"error":{"code":"model_not_rewritten","message":"alias was not rewritten"}}),
        );
    }
    match uri.path() {
        "/v1/chat/completions" if payload.get("stream").and_then(Value::as_bool) == Some(true) => {
            let body = concat!(
                "data: {\"id\":\"chatcmpl-test\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"real-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hello\"},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"chatcmpl-test\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"real-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n"
            );
            Response::builder()
                .status(200)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from(body))
                .unwrap()
        }
        "/v1/chat/completions"
            if payload
                .get("tools")
                .and_then(Value::as_array)
                .is_some_and(|tools| !tools.is_empty()) =>
        {
            response(
                StatusCode::OK,
                json!({"id":"chatcmpl-tool","object":"chat.completion","created":1,"model":"real-model","choices":[{"index":0,"message":{"role":"assistant","content":"","tool_calls":[{"id":"call_lookup","type":"function","function":{"name":"lookup","arguments":"{\"query\":\"hello\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":2,"completion_tokens":1,"total_tokens":3}}),
            )
        }
        "/v1/chat/completions" => response(
            StatusCode::OK,
            json!({"id":"chatcmpl-test","object":"chat.completion","created":1,"model":"real-model","choices":[{"index":0,"message":{"role":"assistant","content":"hello"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":1,"total_tokens":3}}),
        ),
        "/v1/responses" => response(
            StatusCode::OK,
            json!({"id":"resp_test","object":"response","created_at":1,"status":"completed","model":"real-model","output":[{"id":"msg_test","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"hello","annotations":[]}]}],"usage":{"input_tokens":2,"output_tokens":1,"total_tokens":3}}),
        ),
        "/v1/completions" => response(
            StatusCode::OK,
            json!({"id":"cmpl-test","object":"text_completion","created":1,"model":"real-model","choices":[{"index":0,"text":"hello","finish_reason":"stop"}]}),
        ),
        "/v1/embeddings" => response(
            StatusCode::OK,
            json!({"object":"list","model":"real-model","data":[{"object":"embedding","index":0,"embedding":[0.1,0.2]}],"usage":{"prompt_tokens":1,"total_tokens":1}}),
        ),
        _ => response(
            StatusCode::NOT_FOUND,
            json!({"error":{"code":"not_found","message":"not found"}}),
        ),
    }
}

fn response(status: StatusCode, payload: Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap()
}

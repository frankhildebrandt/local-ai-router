use std::collections::HashSet;

use anyhow::Context;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::{
    commands::AppServices,
    core::AppCore,
    domain::TargetKind,
    identity::{self, DirectoryUser, EffectiveQuota},
    providers::validate_cloud_base_url,
    secrets::generate_local_token,
    storage::{ModelTarget, Store},
};

pub const TOKEN_ACCOUNT: &str = "uplink-parent-token";
pub const NODE_ID_SETTING: &str = "node_id";
const SESSION_TTL_DAYS: i64 = 30;
const TARGET_PREFIX: &str = "uplink:";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UplinkModel {
    pub id: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuotaStatus {
    pub rpm: Option<i64>,
    pub rpm_used: i64,
    pub daily_token_budget: Option<i64>,
    pub daily_tokens_used: i64,
    pub daily_usd_budget: Option<f64>,
    pub daily_usd_used: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UplinkParent {
    pub base_url: String,
    pub parent_node_id: String,
    pub username: String,
    pub user_id: String,
    pub ancestor_node_ids: Vec<String>,
    pub models: Vec<UplinkModel>,
    pub quota: Option<QuotaStatus>,
    pub joined_at: chrono::DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_fingerprint: Option<String>,
    #[serde(default)]
    pub may_publish: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinUplinkInput {
    pub base_url: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub session_token: Option<String>,
    pub tls_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinRequest {
    pub username: Option<String>,
    pub password: Option<String>,
    pub session_token: Option<String>,
    pub child_node_id: String,
    pub descendant_node_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinResponse {
    pub token: String,
    pub parent_node_id: String,
    pub ancestor_node_ids: Vec<String>,
    pub user_id: String,
    pub username: String,
    pub models: Vec<UplinkModel>,
    pub quota: QuotaStatus,
    #[serde(default)]
    pub may_publish: bool,
}

#[derive(Debug, Clone)]
pub struct UplinkCaller {
    pub user_id: String,
    pub username: String,
    pub child_node_id: String,
}

pub async fn migrate(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS uplink_parent (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            base_url TEXT NOT NULL,
            parent_node_id TEXT NOT NULL,
            username TEXT NOT NULL,
            user_id TEXT NOT NULL,
            ancestor_node_ids TEXT NOT NULL DEFAULT '[]',
            models TEXT NOT NULL DEFAULT '[]',
            joined_at TEXT NOT NULL,
            tls_fingerprint TEXT
        )",
    )
    .execute(pool)
    .await?;
    let parent_columns = sqlx::query("PRAGMA table_info(uplink_parent)")
        .fetch_all(pool)
        .await?;
    if !parent_columns
        .iter()
        .any(|column| column.get::<String, _>("name") == "tls_fingerprint")
    {
        sqlx::query("ALTER TABLE uplink_parent ADD COLUMN tls_fingerprint TEXT")
            .execute(pool)
            .await?;
    }
    let parent_columns = sqlx::query("PRAGMA table_info(uplink_parent)")
        .fetch_all(pool)
        .await?;
    if !parent_columns
        .iter()
        .any(|column| column.get::<String, _>("name") == "may_publish")
    {
        sqlx::query("ALTER TABLE uplink_parent ADD COLUMN may_publish INTEGER NOT NULL DEFAULT 0")
            .execute(pool)
            .await?;
    }
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS uplink_sessions (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            child_node_id TEXT NOT NULL,
            token_hash BLOB NOT NULL,
            created_at TEXT NOT NULL,
            expires_at TEXT NOT NULL,
            FOREIGN KEY (user_id) REFERENCES directory_users(id) ON DELETE CASCADE
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS uplink_sessions_token_idx ON uplink_sessions(token_hash)",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS uplink_sessions_child_idx ON uplink_sessions(child_node_id)",
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn node_id(store: &Store) -> anyhow::Result<String> {
    if let Some(id) = store.setting(NODE_ID_SETTING).await? {
        if !id.trim().is_empty() {
            return Ok(id);
        }
    }
    let id = Uuid::new_v4().to_string();
    store.set_setting(NODE_ID_SETTING, &id).await?;
    Ok(id)
}

pub fn target_id_for(model_id: &str) -> String {
    format!("{TARGET_PREFIX}{model_id}")
}

pub fn is_uplink_target_id(id: &str) -> bool {
    id.starts_with(TARGET_PREFIX)
}

fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn generate_uplink_token() -> String {
    generate_local_token().replacen("lar_", "lar_uplink_", 1)
}

pub async fn descendant_node_ids(store: &Store) -> anyhow::Result<Vec<String>> {
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT child_node_id FROM uplink_sessions WHERE expires_at > ?",
    )
    .bind(Utc::now().to_rfc3339())
    .fetch_all(store.pool())
    .await?;
    Ok(rows)
}

pub fn cycle_reason(
    parent_node_id: &str,
    parent_ancestors: &[String],
    child_node_id: &str,
    child_descendants: &[String],
) -> Option<&'static str> {
    if child_node_id == parent_node_id {
        return Some("a router cannot join itself");
    }
    if parent_ancestors.iter().any(|id| id == child_node_id) {
        return Some("join would create a cycle");
    }
    if child_descendants.iter().any(|id| id == parent_node_id) {
        return Some("join would create a cycle");
    }
    None
}

pub async fn granted_models(
    store: &Store,
    user: &DirectoryUser,
) -> anyhow::Result<Vec<UplinkModel>> {
    let permissions = store.permissions_for(user).await?;
    let routes = crate::public_models::advertised_public_models(store).await?;
    Ok(routes
        .into_iter()
        .filter(|route| route.enabled && permissions.allows_model(&route.alias))
        .map(|route| UplinkModel {
            id: route.alias,
            capabilities: route.capabilities,
        })
        .collect())
}

pub async fn quota_status(store: &Store, user: &DirectoryUser) -> anyhow::Result<QuotaStatus> {
    let groups = store.directory_groups().await?;
    let quota = identity::effective_quota(user, &groups);
    let now = Utc::now();
    let minute_ago = (now - Duration::minutes(1)).to_rfc3339();
    let day_start = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .to_rfc3339();
    let rpm_used: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM request_logs WHERE directory_user_id=? AND created_at>=?",
    )
    .bind(&user.id)
    .bind(&minute_ago)
    .fetch_one(store.pool())
    .await?;
    let row = sqlx::query(
        "SELECT COALESCE(SUM(COALESCE(input_tokens,0)+COALESCE(output_tokens,0)),0) AS tokens,
                CAST(COALESCE(SUM(estimated_cost_usd),0) AS REAL) AS usd
         FROM request_logs WHERE directory_user_id=? AND created_at>=?",
    )
    .bind(&user.id)
    .bind(&day_start)
    .fetch_one(store.pool())
    .await?;
    Ok(QuotaStatus {
        rpm: quota.rpm,
        rpm_used,
        daily_token_budget: quota.daily_token_budget,
        daily_tokens_used: row.get::<i64, _>("tokens"),
        daily_usd_budget: quota.daily_usd_budget,
        daily_usd_used: row.get::<f64, _>("usd"),
    })
}

pub fn quota_rejection(quota: &EffectiveQuota, status: &QuotaStatus) -> Option<String> {
    if let Some(limit) = quota.rpm {
        if status.rpm_used >= limit {
            return Some(format!(
                "uplink RPM quota exceeded ({limit} requests/minute)"
            ));
        }
    }
    if let Some(limit) = quota.daily_token_budget {
        if status.daily_tokens_used >= limit {
            return Some(format!(
                "uplink daily token quota exceeded ({limit} tokens)"
            ));
        }
    }
    if let Some(limit) = quota.daily_usd_budget {
        if status.daily_usd_used + f64::EPSILON >= limit {
            return Some(format!("uplink daily USD quota exceeded ({limit})"));
        }
    }
    None
}

pub async fn accept_join(core: &AppCore, request: JoinRequest) -> anyhow::Result<JoinResponse> {
    let parent_node_id = node_id(&core.store).await?;
    let ancestors = current_ancestors(&core.store, &parent_node_id).await?;
    if let Some(reason) = cycle_reason(
        &parent_node_id,
        &ancestors,
        &request.child_node_id,
        &request.descendant_node_ids,
    ) {
        anyhow::bail!("{reason}");
    }
    let user = authenticate_join(&core.store, &request).await?;
    if user.disabled_at.is_some() {
        anyhow::bail!("this account is disabled");
    }
    sqlx::query("DELETE FROM uplink_sessions WHERE child_node_id=?")
        .bind(&request.child_node_id)
        .execute(core.store.pool())
        .await?;
    let token = generate_uplink_token();
    sqlx::query(
        "INSERT INTO uplink_sessions(id,user_id,child_node_id,token_hash,created_at,expires_at)
         VALUES(?,?,?,?,?,?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(&user.id)
    .bind(&request.child_node_id)
    .bind(token_hash(&token))
    .bind(Utc::now().to_rfc3339())
    .bind((Utc::now() + Duration::days(SESSION_TTL_DAYS)).to_rfc3339())
    .execute(core.store.pool())
    .await?;
    let models = granted_models(&core.store, &user).await?;
    let quota = quota_status(&core.store, &user).await?;
    let permissions = core.store.permissions_for(&user).await?;
    Ok(JoinResponse {
        token,
        parent_node_id: parent_node_id.clone(),
        ancestor_node_ids: ancestors,
        user_id: user.id,
        username: user.username,
        models,
        quota,
        may_publish: permissions.may_publish,
    })
}

async fn authenticate_join(store: &Store, request: &JoinRequest) -> anyhow::Result<DirectoryUser> {
    if let Some(token) = request
        .session_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return identity::user_for_session(store, token)
            .await?
            .context("invalid session");
    }
    let username = request
        .username
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("username is required")?;
    let password = request
        .password
        .as_deref()
        .context("password is required")?;
    let (user, _) = identity::login_with_password(store, username, password).await?;
    Ok(user)
}

async fn current_ancestors(store: &Store, node_id: &str) -> anyhow::Result<Vec<String>> {
    let mut ancestors = vec![node_id.to_owned()];
    if let Some(parent) = load_parent(store).await? {
        ancestors.extend(parent.ancestor_node_ids);
        if !ancestors.iter().any(|id| id == &parent.parent_node_id) {
            ancestors.push(parent.parent_node_id);
        }
    }
    Ok(ancestors)
}

pub async fn authenticate_token(core: &AppCore, token: Option<&str>) -> Option<UplinkCaller> {
    let token = token?;
    let row = sqlx::query(
        "SELECT s.user_id, s.child_node_id, u.username, u.disabled_at
         FROM uplink_sessions s JOIN directory_users u ON u.id=s.user_id
         WHERE s.token_hash=? AND s.expires_at > ? LIMIT 1",
    )
    .bind(token_hash(token))
    .bind(Utc::now().to_rfc3339())
    .fetch_optional(core.store.pool())
    .await
    .ok()??;
    let disabled: Option<String> = row.get("disabled_at");
    if disabled.is_some() {
        return None;
    }
    Some(UplinkCaller {
        user_id: row.get("user_id"),
        username: row.get("username"),
        child_node_id: row.get("child_node_id"),
    })
}

pub async fn load_parent(store: &Store) -> anyhow::Result<Option<UplinkParent>> {
    let row = sqlx::query(
        "SELECT base_url,parent_node_id,username,user_id,ancestor_node_ids,models,joined_at,tls_fingerprint,may_publish FROM uplink_parent WHERE id=1",
    )
        .fetch_optional(store.pool())
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(UplinkParent {
        base_url: row.get("base_url"),
        parent_node_id: row.get("parent_node_id"),
        username: row.get("username"),
        user_id: row.get("user_id"),
        ancestor_node_ids: serde_json::from_str(&row.get::<String, _>("ancestor_node_ids"))
            .unwrap_or_default(),
        models: serde_json::from_str(&row.get::<String, _>("models")).unwrap_or_default(),
        quota: None,
        joined_at: row.get::<String, _>("joined_at").parse()?,
        tls_fingerprint: row.get("tls_fingerprint"),
        may_publish: row
            .try_get::<i64, _>("may_publish")
            .ok()
            .is_some_and(|flag| flag != 0),
    }))
}

pub async fn join_uplink(
    services: &AppServices,
    input: JoinUplinkInput,
) -> anyhow::Result<UplinkParent> {
    if load_parent(&services.core.store).await?.is_some() {
        anyhow::bail!("disconnect the current uplink before joining another parent");
    }
    let base_url = normalize_base_url(&input.base_url)?;
    let child_node_id = node_id(&services.core.store).await?;
    let descendants = descendant_node_ids(&services.core.store).await?;
    let body = JoinRequest {
        username: input.username,
        password: input.password,
        session_token: input.session_token,
        child_node_id,
        descendant_node_ids: descendants,
    };
    let client = http_client(input.tls_fingerprint.as_deref())?;
    let response = client
        .post(format!("{base_url}/uplink/join"))
        .json(&body)
        .send()
        .await
        .context("unable to reach uplink parent")?;
    let status = response.status();
    let payload = response.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("{}", join_error_message(status.as_u16(), &payload));
    }
    let joined: JoinResponse = serde_json::from_str(&payload).context("invalid join response")?;
    let local_id = node_id(&services.core.store).await?;
    if let Some(reason) = cycle_reason(
        &joined.parent_node_id,
        &joined.ancestor_node_ids,
        &local_id,
        &[],
    ) {
        anyhow::bail!("{reason}");
    }
    persist_parent(
        services,
        &base_url,
        &joined,
        input.tls_fingerprint.as_deref(),
    )
    .await?;
    mount_models(&services.core.store, &joined.models).await?;
    load_parent(&services.core.store)
        .await?
        .map(|mut parent| {
            parent.quota = Some(joined.quota.clone());
            parent.may_publish = joined.may_publish;
            parent
        })
        .context("uplink parent missing after join")
}

pub async fn refresh_uplink(services: &AppServices) -> anyhow::Result<UplinkParent> {
    let parent = load_parent(&services.core.store)
        .await?
        .context("not joined to an uplink")?;
    let token = services
        .core
        .secrets
        .get(TOKEN_ACCOUNT)?
        .context("uplink token missing")?;
    let client = http_client(parent.tls_fingerprint.as_deref())?;
    let response = client
        .get(format!("{}/uplink/models", parent.base_url))
        .bearer_auth(&token)
        .send()
        .await
        .context("unable to reach uplink parent")?;
    if !response.status().is_success() {
        anyhow::bail!("uplink refresh failed: {}", response.status());
    }
    let joined: JoinResponse = response.json().await.context("invalid uplink models")?;
    persist_parent(
        services,
        &parent.base_url,
        &joined,
        parent.tls_fingerprint.as_deref(),
    )
    .await?;
    mount_models(&services.core.store, &joined.models).await?;
    load_parent(&services.core.store)
        .await?
        .map(|mut item| {
            item.quota = Some(joined.quota.clone());
            item.may_publish = joined.may_publish;
            item
        })
        .context("uplink parent missing after refresh")
}

pub async fn disconnect_uplink(services: &AppServices) -> anyhow::Result<()> {
    if let (Some(parent), Some(token)) = (
        load_parent(&services.core.store).await?,
        services.core.secrets.get(TOKEN_ACCOUNT)?,
    ) {
        let client = http_client(parent.tls_fingerprint.as_deref())?;
        let _ = client
            .post(format!("{}/uplink/leave", parent.base_url))
            .bearer_auth(token)
            .send()
            .await;
    }
    let _ = crate::publish::clear_local_offers(services).await;
    sqlx::query("DELETE FROM uplink_parent")
        .execute(services.core.store.pool())
        .await?;
    let _ = services.core.secrets.delete(TOKEN_ACCOUNT);
    unmount_models(&services.core.store).await
}

pub async fn revoke_session(core: &crate::core::AppCore, token: &str) -> anyhow::Result<()> {
    let child_node_id = sqlx::query_scalar::<_, String>(
        "SELECT child_node_id FROM uplink_sessions WHERE token_hash=? LIMIT 1",
    )
    .bind(token_hash(token))
    .fetch_optional(core.store.pool())
    .await?;
    sqlx::query("DELETE FROM uplink_sessions WHERE token_hash=?")
        .bind(token_hash(token))
        .execute(core.store.pool())
        .await?;
    if let Some(child_node_id) = child_node_id {
        crate::publish::drop_child_replicas(core, &child_node_id).await?;
    }
    Ok(())
}

async fn persist_parent(
    services: &AppServices,
    base_url: &str,
    joined: &JoinResponse,
    tls_fingerprint: Option<&str>,
) -> anyhow::Result<()> {
    let tls_fingerprint = tls_fingerprint
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            let digest = crate::tls::parse_fingerprint(value)?;
            Ok::<_, anyhow::Error>(crate::tls::format_fingerprint(&digest))
        })
        .transpose()?;
    sqlx::query(
        "INSERT INTO uplink_parent(id,base_url,parent_node_id,username,user_id,ancestor_node_ids,models,joined_at,tls_fingerprint,may_publish)
         VALUES(1,?,?,?,?,?,?,?,?,?)
         ON CONFLICT(id) DO UPDATE SET
            base_url=excluded.base_url,
            parent_node_id=excluded.parent_node_id,
            username=excluded.username,
            user_id=excluded.user_id,
            ancestor_node_ids=excluded.ancestor_node_ids,
            models=excluded.models,
            tls_fingerprint=excluded.tls_fingerprint,
            may_publish=excluded.may_publish",
    )
    .bind(base_url)
    .bind(&joined.parent_node_id)
    .bind(&joined.username)
    .bind(&joined.user_id)
    .bind(serde_json::to_string(&joined.ancestor_node_ids)?)
    .bind(serde_json::to_string(&joined.models)?)
    .bind(Utc::now().to_rfc3339())
    .bind(tls_fingerprint)
    .bind(joined.may_publish as i64)
    .execute(services.core.store.pool())
    .await?;
    if !joined.token.is_empty() {
        services.core.secrets.set(TOKEN_ACCOUNT, &joined.token)?;
    }
    Ok(())
}

async fn mount_models(store: &Store, models: &[UplinkModel]) -> anyhow::Result<()> {
    let existing: HashSet<String> = store
        .targets()
        .await?
        .into_iter()
        .filter(|target| target.kind.is_uplink())
        .map(|target| target.id)
        .collect();
    let mut keep = HashSet::new();
    for model in models {
        let id = target_id_for(&model.id);
        keep.insert(id.clone());
        store
            .upsert_target(&ModelTarget {
                id,
                provider_id: None,
                name: model.id.clone(),
                kind: TargetKind::Uplink,
                provider_model: model.id.clone(),
                local_path: None,
                runtime_url: None,
                wire_protocol: crate::providers::WireProtocol::OpenAiChat,
                capabilities: model.capabilities.clone(),
                enabled: true,
                state: "ready".into(),
                size_bytes: None,
                local: crate::storage::LocalModelMeta::default(),
            })
            .await?;
    }
    for id in existing.difference(&keep) {
        let _ = store.delete_target(id).await;
    }
    Ok(())
}

async fn unmount_models(store: &Store) -> anyhow::Result<()> {
    for target in store.targets().await? {
        if target.kind.is_uplink() {
            let _ = store.delete_target(&target.id).await;
        }
    }
    Ok(())
}

fn normalize_base_url(value: &str) -> anyhow::Result<String> {
    let value = validate_cloud_base_url(value, true)?;
    Ok(value
        .trim_end_matches("/v1")
        .trim_end_matches('/')
        .to_owned())
}

fn join_error_message(status: u16, payload: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) {
        if let Some(message) = value
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(|message| message.as_str())
            .or_else(|| value.get("message").and_then(|message| message.as_str()))
        {
            return message.to_owned();
        }
    }
    let trimmed = payload.trim();
    if !trimmed.is_empty() && trimmed.len() < 300 {
        return trimmed.to_owned();
    }
    format!("uplink join failed ({status})")
}

pub fn http_client(fingerprint: Option<&str>) -> anyhow::Result<reqwest::Client> {
    let fingerprint = fingerprint.map(str::trim).filter(|value| !value.is_empty());
    let mut builder = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .user_agent("LocalAI-Router-Uplink/0.1")
        .redirect(reqwest::redirect::Policy::none());
    if let Some(fingerprint) = fingerprint {
        builder = builder.use_preconfigured_tls(crate::tls::pinned_client_config(fingerprint)?);
    }
    Ok(builder.build()?)
}

pub async fn parent_http_client(store: &Store) -> anyhow::Result<reqwest::Client> {
    let parent = load_parent(store)
        .await?
        .context("not joined to an uplink")?;
    http_client(parent.tls_fingerprint.as_deref())
}

pub fn uplink_upstream_path(path: &str, parent_model: &str) -> String {
    if let Some(rest) = path.strip_prefix("/v1beta/models/") {
        if let Some((_, operation)) = rest.split_once(':') {
            return format!("/v1beta/models/{parent_model}:{operation}");
        }
    }
    path.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{ModelRoute, RouteRole, RouteTarget},
        engine::{serve_gateway, test_engine},
        identity::{
            login_with_password, update_user, upsert_group, CreateUserInput, UpdateUserInput,
            UpsertGroupInput,
        },
        providers::{AuthMode, WireProtocol},
        routing::TargetRoutingProfile,
        storage::Provider,
    };
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
        response::IntoResponse,
        Json, Router,
    };
    use http_body_util::BodyExt;
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;
    use tower::ServiceExt;

    struct UpstreamCapture {
        auths: Mutex<Vec<String>>,
        bodies: Mutex<Vec<Value>>,
        paths: Mutex<Vec<String>>,
        calls: Mutex<usize>,
    }

    impl UpstreamCapture {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                auths: Mutex::new(Vec::new()),
                bodies: Mutex::new(Vec::new()),
                paths: Mutex::new(Vec::new()),
                calls: Mutex::new(0),
            })
        }
    }

    async fn listen(engine: &crate::engine::Engine) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = engine.router();
        let shutdown = engine.services.shutdown.clone();
        tokio::spawn(async move {
            let _ = serve_gateway(listener, None, router, shutdown).await;
        });
        format!("http://{address}")
    }

    async fn listen_tls(engine: &crate::engine::Engine) -> (String, String) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let material = crate::tls::generate_self_signed().unwrap();
        let config = std::sync::Arc::new(material.server_config().unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = engine.router();
        let shutdown = engine.services.shutdown.clone();
        tokio::spawn(async move {
            let _ = serve_gateway(listener, Some(config), router, shutdown).await;
        });
        (format!("https://{address}"), material.fingerprint)
    }

    async fn json_body(response: axum::response::Response) -> Value {
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
    }

    async fn mock_openai(capture: Arc<UpstreamCapture>) -> String {
        let app = Router::new().fallback(move |request: Request<Body>| {
            let capture = capture.clone();
            async move {
                *capture.calls.lock().unwrap() += 1;
                capture
                    .paths
                    .lock()
                    .unwrap()
                    .push(request.uri().path().to_owned());
                if let Some(auth) = request
                    .headers()
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                {
                    capture.auths.lock().unwrap().push(auth.to_owned());
                }
                let streaming = request
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.contains("event-stream"));
                let path = request.uri().path().to_owned();
                let bytes = request.into_body().collect().await.unwrap().to_bytes();
                let body: Value = serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({}));
                let stream = streaming
                    || body
                        .get("stream")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                capture.bodies.lock().unwrap().push(body);
                if path.contains("/images/") {
                    return Json(json!({"data":[{"b64_json":"AAAA"}]})).into_response();
                }
                if path.contains("/moderations") {
                    return Json(json!({"id":"mod","results":[{"flagged":false}]})).into_response();
                }
                if path.contains("/audio/") {
                    return (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "audio/mpeg")],
                        "ID3",
                    )
                        .into_response();
                }
                if stream {
                    return (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        "data: {\"id\":\"s\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"from-parent\"}}]}\n\ndata: [DONE]\n\n",
                    )
                        .into_response();
                }
                Json(json!({
                    "id": "ok",
                    "choices": [{"message":{"role":"assistant","content":"from-parent"},"finish_reason":"stop"}],
                    "usage": {"prompt_tokens": 3, "completion_tokens": 2}
                }))
                .into_response()
            }
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}/v1")
    }

    async fn down_upstream() -> String {
        let app = Router::new().fallback(|| async {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":{"message":"down"}})),
            )
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}/v1")
    }

    fn alice_join(parent_url: String) -> JoinUplinkInput {
        JoinUplinkInput {
            base_url: parent_url,
            username: Some("alice".into()),
            password: Some("alice-pass".into()),
            session_token: None,
            tls_fingerprint: None,
        }
    }

    async fn child_request(
        engine: &crate::engine::Engine,
        token: &str,
        method: &str,
        uri: &str,
        body: &str,
    ) -> axum::response::Response {
        engine
            .router()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn text_body(response: axum::response::Response) -> String {
        String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap()
    }

    fn blank_user_update() -> UpdateUserInput {
        UpdateUserInput {
            display_name: None,
            password: None,
            group_ids: None,
            allowed_model_ids: None,
            inherit_models: None,
            may_publish: None,
            inherit_publish: None,
            may_admin: None,
            inherit_admin: None,
            disabled: None,
            rpm: None,
            daily_token_budget: None,
            daily_usd_budget: None,
        }
    }

    async fn parent_with_alice(
        data: &std::path::Path,
        upstream: String,
        model_id: &str,
        capabilities: Vec<String>,
    ) -> crate::engine::Engine {
        let engine = test_engine(data, None).await;
        engine
            .services
            .core
            .store
            .upsert_provider(&Provider {
                id: "openai".into(),
                name: "OpenAI".into(),
                preset_id: "openai".into(),
                auth_mode: AuthMode::ApiKey,
                base_url: upstream,
                enabled: true,
                has_credential: false,
            })
            .await
            .unwrap();
        engine
            .services
            .core
            .save_provider_api_key("openai", "parent-provider-key")
            .unwrap();
        engine
            .services
            .core
            .store
            .upsert_target(&ModelTarget {
                id: "cloud".into(),
                provider_id: Some("openai".into()),
                name: model_id.into(),
                kind: TargetKind::Cloud,
                provider_model: model_id.into(),
                local_path: None,
                runtime_url: None,
                wire_protocol: WireProtocol::OpenAiChat,
                capabilities,
                enabled: true,
                state: "ready".into(),
                size_bytes: None,
                local: crate::storage::LocalModelMeta::default(),
            })
            .await
            .unwrap();
        identity::create_user(
            &engine.services.core.store,
            CreateUserInput {
                username: "alice".into(),
                display_name: "Alice".into(),
                password: Some("alice-pass".into()),
                group_ids: Vec::new(),
                allowed_model_ids: Some(vec![model_id.into()]),
                may_publish: None,
                may_admin: None,
                rpm: None,
                daily_token_budget: None,
                daily_usd_budget: None,
            },
        )
        .await
        .unwrap();
        engine
    }

    async fn child_token(engine: &crate::engine::Engine) -> (String, String) {
        let created = engine
            .router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/create_local_api_key")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"name":"Laptop"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let payload = json_body(created).await;
        (
            payload["token"].as_str().unwrap().to_owned(),
            payload["id"].as_str().unwrap().to_owned(),
        )
    }

    #[tokio::test]
    async fn child_joins_parent_and_streams_granted_chat_without_leaking_keys() {
        let capture = UpstreamCapture::new();
        let upstream = mock_openai(capture.clone()).await;
        let root = tempfile::tempdir().unwrap();
        let parent = parent_with_alice(
            &root.path().join("parent"),
            upstream,
            "alice-gpt",
            vec!["chat".into(), "streaming".into()],
        )
        .await;
        let parent_url = listen(&parent).await;
        let child = test_engine(&root.path().join("child"), None).await;
        let joined = join_uplink(&child.services, alice_join(parent_url))
            .await
            .unwrap();
        assert_eq!(
            joined
                .models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["alice-gpt"]
        );
        assert!(child
            .services
            .core
            .secrets
            .get("provider:openai")
            .unwrap()
            .is_none());
        let (token, key_id) = child_token(&child).await;
        let models = child
            .router()
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(models.status(), StatusCode::OK);
        let listed = json_body(models).await;
        let ids: Vec<_> = listed["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["id"].as_str())
            .collect();
        assert!(ids.contains(&"alice-gpt"), "{ids:?}");

        let chat = child_request(
            &child,
            &token,
            "POST",
            "/v1/chat/completions",
            r#"{"model":"alice-gpt","messages":[{"role":"user","content":"hello"}]}"#,
        )
        .await;
        assert_eq!(chat.status(), StatusCode::OK, "{}", chat.status());
        let hop = chat
            .headers()
            .get("x-local-ai-hop")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = json_body(chat).await;
        assert_eq!(body["choices"][0]["message"]["content"], "from-parent");
        assert_eq!(hop.as_deref(), Some("uplink"));

        let stream = child_request(
            &child,
            &token,
            "POST",
            "/v1/chat/completions",
            r#"{"model":"alice-gpt","stream":true,"messages":[{"role":"user","content":"hello"}]}"#,
        )
        .await;
        assert_eq!(stream.status(), StatusCode::OK, "{}", stream.status());
        assert_eq!(
            stream
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(|value| value.contains("event-stream")),
            Some(true)
        );
        let streamed = text_body(stream).await;
        assert!(streamed.contains("from-parent"), "{streamed}");

        let auths = capture.auths.lock().unwrap().clone();
        assert!(auths
            .iter()
            .all(|value| value == "Bearer parent-provider-key"));
        assert!(!auths.iter().any(|value| value.contains(&token)));

        let parent_logs = parent.services.core.store.logs(10).await.unwrap();
        assert_eq!(parent_logs[0].directory_user_name.as_deref(), Some("alice"));
        assert!(parent_logs[0].api_key_id.is_none());
        let child_logs = child.services.core.store.logs(10).await.unwrap();
        assert_eq!(child_logs[0].api_key_id.as_deref(), Some(key_id.as_str()));
        assert!(child_logs[0].directory_user_id.is_none());
    }

    #[tokio::test]
    async fn second_parent_is_rejected_until_disconnect() {
        let capture = UpstreamCapture::new();
        let root = tempfile::tempdir().unwrap();
        let parent = parent_with_alice(
            &root.path().join("parent"),
            mock_openai(capture).await,
            "alice-gpt",
            vec!["chat".into()],
        )
        .await;
        let parent_url = listen(&parent).await;
        let child = test_engine(&root.path().join("child"), None).await;
        join_uplink(&child.services, alice_join(parent_url.clone()))
            .await
            .unwrap();
        let error = join_uplink(&child.services, alice_join(parent_url.clone()))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("disconnect"), "{error}");
        disconnect_uplink(&child.services).await.unwrap();
        join_uplink(&child.services, alice_join(parent_url))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn join_rejects_a_self_cycle_and_session_tokens_work() {
        let capture = UpstreamCapture::new();
        let root = tempfile::tempdir().unwrap();
        let parent = parent_with_alice(
            &root.path().join("parent"),
            mock_openai(capture).await,
            "alice-gpt",
            vec!["chat".into()],
        )
        .await;
        let parent_url = listen(&parent).await;
        let error = join_uplink(&parent.services, alice_join(parent_url.clone()))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("cannot join itself") || error.contains("cycle"),
            "{error}"
        );

        let (_, session) = login_with_password(&parent.services.core.store, "alice", "alice-pass")
            .await
            .unwrap();
        let child = test_engine(&root.path().join("child"), None).await;
        let joined = join_uplink(
            &child.services,
            JoinUplinkInput {
                base_url: parent_url,
                username: None,
                password: None,
                session_token: Some(session),
                tls_fingerprint: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(joined.username, "alice");
        assert_eq!(joined.models[0].id, "alice-gpt");
    }

    #[tokio::test]
    async fn local_models_stay_local_and_revoked_grants_are_rejected() {
        let capture = UpstreamCapture::new();
        let root = tempfile::tempdir().unwrap();
        let parent = parent_with_alice(
            &root.path().join("parent"),
            mock_openai(capture.clone()).await,
            "alice-gpt",
            vec!["chat".into()],
        )
        .await;
        let parent_url = listen(&parent).await;
        let child = test_engine(&root.path().join("child"), None).await;
        child
            .services
            .core
            .store
            .upsert_provider(&Provider {
                id: "local-cloud".into(),
                name: "Local cloud".into(),
                preset_id: "openai".into(),
                auth_mode: AuthMode::ApiKey,
                base_url: down_upstream().await,
                enabled: true,
                has_credential: false,
            })
            .await
            .unwrap();
        child
            .services
            .core
            .save_provider_api_key("local-cloud", "child-provider-key")
            .unwrap();
        child
            .services
            .core
            .store
            .upsert_target(&ModelTarget {
                id: "local-cloud".into(),
                provider_id: Some("local-cloud".into()),
                name: "laptop-gpt".into(),
                kind: TargetKind::Cloud,
                provider_model: "laptop-gpt".into(),
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
        join_uplink(&child.services, alice_join(parent_url))
            .await
            .unwrap();
        let (token, _) = child_token(&child).await;
        let local = child_request(
            &child,
            &token,
            "POST",
            "/v1/chat/completions",
            r#"{"model":"laptop-gpt","messages":[{"role":"user","content":"stay"}]}"#,
        )
        .await;
        assert_ne!(local.status(), StatusCode::OK);
        assert_eq!(*capture.calls.lock().unwrap(), 0);

        let alice = parent
            .services
            .core
            .store
            .directory_users()
            .await
            .unwrap()
            .into_iter()
            .find(|user| user.username == "alice")
            .unwrap();
        let mut update = blank_user_update();
        update.allowed_model_ids = Some(vec!["other".into()]);
        update_user(
            &parent.services.core.store,
            parent.services.core.secrets.as_ref(),
            &alice.id,
            update,
        )
        .await
        .unwrap();
        let revoked = child_request(
            &child,
            &token,
            "POST",
            "/v1/chat/completions",
            r#"{"model":"alice-gpt","messages":[{"role":"user","content":"hello"}]}"#,
        )
        .await;
        assert_eq!(revoked.status(), StatusCode::FORBIDDEN);
        assert_eq!(*capture.calls.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn aliases_and_adaptive_routing_can_hop_to_uplink() {
        let capture = UpstreamCapture::new();
        let root = tempfile::tempdir().unwrap();
        let parent = parent_with_alice(
            &root.path().join("parent"),
            mock_openai(capture.clone()).await,
            "alice-gpt",
            vec!["chat".into(), "streaming".into()],
        )
        .await;
        let parent_url = listen(&parent).await;
        let child = test_engine(&root.path().join("child"), None).await;
        child
            .services
            .core
            .store
            .upsert_provider(&Provider {
                id: "broken".into(),
                name: "Broken".into(),
                preset_id: "openai".into(),
                auth_mode: AuthMode::ApiKey,
                base_url: down_upstream().await,
                enabled: true,
                has_credential: false,
            })
            .await
            .unwrap();
        child
            .services
            .core
            .save_provider_api_key("broken", "broken-key")
            .unwrap();
        child
            .services
            .core
            .store
            .upsert_target(&ModelTarget {
                id: "broken".into(),
                provider_id: Some("broken".into()),
                name: "broken-gpt".into(),
                kind: TargetKind::Cloud,
                provider_model: "broken-gpt".into(),
                local_path: None,
                runtime_url: None,
                wire_protocol: WireProtocol::OpenAiChat,
                capabilities: vec!["chat".into(), "streaming".into()],
                enabled: true,
                state: "ready".into(),
                size_bytes: None,
                local: crate::storage::LocalModelMeta::default(),
            })
            .await
            .unwrap();
        join_uplink(&child.services, alice_join(parent_url))
            .await
            .unwrap();
        child
            .services
            .core
            .store
            .upsert_route(&ModelRoute {
                alias: "team-code".into(),
                enabled: true,
                capabilities: vec!["chat".into(), "streaming".into()],
                targets: vec![
                    RouteTarget {
                        id: "broken".into(),
                        kind: TargetKind::Cloud,
                        model: "broken-gpt".into(),
                        priority: 10,
                        enabled: true,
                        role: RouteRole::Primary,
                    },
                    RouteTarget {
                        id: target_id_for("alice-gpt"),
                        kind: TargetKind::Uplink,
                        model: "alice-gpt".into(),
                        priority: 20,
                        enabled: true,
                        role: RouteRole::Fallback,
                    },
                ],
            })
            .await
            .unwrap();
        let (token, _) = child_token(&child).await;
        let fallback = child_request(
            &child,
            &token,
            "POST",
            "/v1/chat/completions",
            r#"{"model":"team-code","messages":[{"role":"user","content":"hello"}]}"#,
        )
        .await;
        assert_eq!(fallback.status(), StatusCode::OK, "{}", fallback.status());
        assert_eq!(
            fallback
                .headers()
                .get("x-local-ai-hop")
                .and_then(|value| value.to_str().ok()),
            Some("uplink")
        );
        let adaptive = child_request(
            &child,
            &token,
            "POST",
            "/v1/chat/completions",
            r#"{"model":"adaptive-routing","messages":[{"role":"user","content":"hello"}]}"#,
        )
        .await;
        assert_eq!(adaptive.status(), StatusCode::OK, "{}", adaptive.status());
        child
            .services
            .core
            .store
            .upsert_route(&ModelRoute {
                alias: "uplink-primary".into(),
                enabled: true,
                capabilities: vec!["chat".into(), "streaming".into()],
                targets: vec![RouteTarget {
                    id: target_id_for("alice-gpt"),
                    kind: TargetKind::Uplink,
                    model: "alice-gpt".into(),
                    priority: 10,
                    enabled: true,
                    role: RouteRole::Primary,
                }],
            })
            .await
            .unwrap();
        let primary = child_request(
            &child,
            &token,
            "POST",
            "/v1/chat/completions",
            r#"{"model":"uplink-primary","messages":[{"role":"user","content":"hello"}]}"#,
        )
        .await;
        assert_eq!(primary.status(), StatusCode::OK, "{}", primary.status());
        assert_eq!(
            primary
                .headers()
                .get("x-local-ai-hop")
                .and_then(|value| value.to_str().ok()),
            Some("uplink")
        );
        assert!(*capture.calls.lock().unwrap() >= 3);
    }

    #[tokio::test]
    async fn parent_enforces_user_and_group_uplink_quotas() {
        let capture = UpstreamCapture::new();
        let root = tempfile::tempdir().unwrap();
        let parent = parent_with_alice(
            &root.path().join("parent"),
            mock_openai(capture.clone()).await,
            "alice-gpt",
            vec!["chat".into()],
        )
        .await;
        let mut profile = TargetRoutingProfile::neutral("cloud", TargetKind::Cloud);
        profile.input_price_per_million = Some(1_000_000.0);
        profile.output_price_per_million = Some(1_000_000.0);
        parent
            .services
            .core
            .store
            .upsert_target_routing_profile(&profile)
            .await
            .unwrap();
        let alice = parent
            .services
            .core
            .store
            .directory_users()
            .await
            .unwrap()
            .into_iter()
            .find(|user| user.username == "alice")
            .unwrap();
        let mut update = blank_user_update();
        update.rpm = Some(Some(1));
        update_user(
            &parent.services.core.store,
            parent.services.core.secrets.as_ref(),
            &alice.id,
            update,
        )
        .await
        .unwrap();
        let parent_url = listen(&parent).await;
        let child = test_engine(&root.path().join("child"), None).await;
        let joined = join_uplink(&child.services, alice_join(parent_url))
            .await
            .unwrap();
        assert_eq!(joined.quota.as_ref().and_then(|quota| quota.rpm), Some(1));
        let uplink_token = child
            .services
            .core
            .secrets
            .get(TOKEN_ACCOUNT)
            .unwrap()
            .unwrap();
        let first = parent
            .router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, format!("Bearer {uplink_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"model":"alice-gpt","messages":[{"role":"user","content":"hello"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK, "{}", first.status());
        let second = parent
            .router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, format!("Bearer {uplink_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"model":"alice-gpt","messages":[{"role":"user","content":"hello"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            second.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "{}",
            text_body(second).await
        );

        let mut tokens = blank_user_update();
        tokens.rpm = Some(Some(100));
        tokens.daily_token_budget = Some(Some(5));
        update_user(
            &parent.services.core.store,
            parent.services.core.secrets.as_ref(),
            &alice.id,
            tokens,
        )
        .await
        .unwrap();
        let token_cap = parent
            .router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, format!("Bearer {uplink_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"model":"alice-gpt","messages":[{"role":"user","content":"hello"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(token_cap.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(text_body(token_cap).await.contains("token"));

        let mut usd = blank_user_update();
        usd.daily_token_budget = Some(Some(1_000_000));
        usd.daily_usd_budget = Some(Some(4.0));
        update_user(
            &parent.services.core.store,
            parent.services.core.secrets.as_ref(),
            &alice.id,
            usd,
        )
        .await
        .unwrap();
        let usd_cap = parent
            .router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, format!("Bearer {uplink_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"model":"alice-gpt","messages":[{"role":"user","content":"hello"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(usd_cap.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(text_body(usd_cap).await.contains("USD"));
        assert_eq!(*capture.calls.lock().unwrap(), 1);

        let group = upsert_group(
            &parent.services.core.store,
            None,
            UpsertGroupInput {
                name: "limited".into(),
                allowed_model_ids: vec!["alice-gpt".into()],
                may_publish: false,
                may_admin: false,
                rpm: Some(1),
                daily_token_budget: None,
                daily_usd_budget: None,
            },
        )
        .await
        .unwrap();
        let mut membership = blank_user_update();
        membership.group_ids = Some(vec![group.id]);
        membership.rpm = Some(Some(50));
        update_user(
            &parent.services.core.store,
            parent.services.core.secrets.as_ref(),
            &alice.id,
            membership,
        )
        .await
        .unwrap();
        let tightened = crate::identity::effective_quota(
            &parent
                .services
                .core
                .store
                .directory_user(&alice.id)
                .await
                .unwrap()
                .unwrap(),
            &parent.services.core.store.directory_groups().await.unwrap(),
        );
        assert_eq!(tightened.rpm, Some(1));
    }

    #[tokio::test]
    async fn remaining_protocols_proxy_through_uplink_and_missing_capabilities_stay_local() {
        let capture = UpstreamCapture::new();
        let root = tempfile::tempdir().unwrap();
        let parent = parent_with_alice(
            &root.path().join("parent"),
            mock_openai(capture.clone()).await,
            "alice-gpt",
            vec![
                "chat".into(),
                "streaming".into(),
                "images".into(),
                "speech".into(),
                "audio".into(),
                "moderation".into(),
            ],
        )
        .await;
        let parent_url = listen(&parent).await;
        let child = test_engine(&root.path().join("child"), None).await;
        join_uplink(&child.services, alice_join(parent_url))
            .await
            .unwrap();
        let (token, _) = child_token(&child).await;

        let anthropic = child_request(
            &child,
            &token,
            "POST",
            "/v1/messages",
            r#"{"model":"alice-gpt","max_tokens":20,"messages":[{"role":"user","content":"hello"}]}"#,
        )
        .await;
        assert_eq!(anthropic.status(), StatusCode::OK, "{}", anthropic.status());
        let anthropic_body = json_body(anthropic).await;
        assert_eq!(anthropic_body["content"][0]["text"], "from-parent");

        let anthropic_stream = child_request(
            &child,
            &token,
            "POST",
            "/v1/messages",
            r#"{"model":"alice-gpt","max_tokens":20,"stream":true,"messages":[{"role":"user","content":"hello"}]}"#,
        )
        .await;
        assert_eq!(anthropic_stream.status(), StatusCode::OK);
        assert!(text_body(anthropic_stream).await.contains("from-parent"));

        let gemini = child_request(
            &child,
            &token,
            "POST",
            "/v1beta/models/alice-gpt:generateContent",
            r#"{"contents":[{"role":"user","parts":[{"text":"hello"}]}]}"#,
        )
        .await;
        assert_eq!(gemini.status(), StatusCode::OK, "{}", gemini.status());
        let gemini_body = json_body(gemini).await;
        assert_eq!(
            gemini_body["candidates"][0]["content"]["parts"][0]["text"],
            "from-parent"
        );

        let gemini_stream = child_request(
            &child,
            &token,
            "POST",
            "/v1beta/models/alice-gpt:streamGenerateContent",
            r#"{"contents":[{"role":"user","parts":[{"text":"hello"}]}]}"#,
        )
        .await;
        assert_eq!(
            gemini_stream.status(),
            StatusCode::OK,
            "{}",
            gemini_stream.status()
        );
        assert!(text_body(gemini_stream).await.contains("from-parent"));

        let images = child_request(
            &child,
            &token,
            "POST",
            "/v1/images/generations",
            r#"{"model":"alice-gpt","prompt":"a cat"}"#,
        )
        .await;
        assert_eq!(images.status(), StatusCode::OK, "{}", images.status());
        assert_eq!(json_body(images).await["data"][0]["b64_json"], "AAAA");

        let speech = child_request(
            &child,
            &token,
            "POST",
            "/v1/audio/speech",
            r#"{"model":"alice-gpt","input":"hello","voice":"alloy"}"#,
        )
        .await;
        assert_eq!(speech.status(), StatusCode::OK, "{}", speech.status());

        let moderation = child_request(
            &child,
            &token,
            "POST",
            "/v1/moderations",
            r#"{"model":"alice-gpt","input":"hello"}"#,
        )
        .await;
        assert_eq!(
            moderation.status(),
            StatusCode::OK,
            "{}",
            moderation.status()
        );
        assert_eq!(json_body(moderation).await["results"][0]["flagged"], false);

        let auths = capture.auths.lock().unwrap().clone();
        assert!(auths
            .iter()
            .all(|value| value == "Bearer parent-provider-key"));
        assert!(!auths.iter().any(|value| value.contains(&token)));
    }

    #[tokio::test]
    async fn missing_uplink_capability_is_rejected_without_calling_the_parent() {
        let capture = UpstreamCapture::new();
        let root = tempfile::tempdir().unwrap();
        let parent = parent_with_alice(
            &root.path().join("parent"),
            mock_openai(capture.clone()).await,
            "alice-gpt",
            vec!["chat".into()],
        )
        .await;
        let parent_url = listen(&parent).await;
        let child = test_engine(&root.path().join("child"), None).await;
        join_uplink(&child.services, alice_join(parent_url))
            .await
            .unwrap();
        let (token, _) = child_token(&child).await;
        let images = child_request(
            &child,
            &token,
            "POST",
            "/v1/images/generations",
            r#"{"model":"alice-gpt","prompt":"a cat"}"#,
        )
        .await;
        assert_eq!(images.status(), StatusCode::BAD_REQUEST);
        assert!(text_body(images).await.contains("unsupported_capability"));
        assert_eq!(*capture.calls.lock().unwrap(), 0);
    }

    #[test]
    fn cycle_detection_rejects_ancestor_and_descendant_loops() {
        assert!(cycle_reason("a", &["a".into()], "a", &[]).is_some());
        assert!(cycle_reason("c", &["c".into(), "b".into(), "a".into()], "a", &[]).is_some());
        assert!(cycle_reason("a", &["a".into()], "b", &["c".into(), "a".into()]).is_some());
        assert!(cycle_reason("a", &["a".into()], "b", &["c".into()]).is_none());
    }

    #[test]
    fn uplink_urls_follow_cloud_https_rules() {
        assert!(normalize_base_url("http://127.0.0.1:11435").is_ok());
        assert!(normalize_base_url("http://localhost:11435/v1").is_ok());
        assert!(normalize_base_url("https://router.example:11435").is_ok());
        assert!(normalize_base_url("http://192.168.1.10:11435").is_err());
        assert!(normalize_base_url("https://user:pass@example.com").is_err());
        assert!(normalize_base_url("https://example.com?key=secret").is_err());
    }

    #[tokio::test]
    async fn https_join_pins_the_parent_certificate_fingerprint() {
        let capture = UpstreamCapture::new();
        let root = tempfile::tempdir().unwrap();
        let parent = parent_with_alice(
            &root.path().join("parent"),
            mock_openai(capture.clone()).await,
            "alice-gpt",
            vec!["chat".into(), "streaming".into()],
        )
        .await;
        let (parent_url, fingerprint) = listen_tls(&parent).await;
        let child = test_engine(&root.path().join("child"), None).await;
        let mut unpinned = alice_join(parent_url.clone());
        unpinned.tls_fingerprint = None;
        assert!(join_uplink(&child.services, unpinned).await.is_err());
        let mut wrong = alice_join(parent_url.clone());
        wrong.tls_fingerprint = Some("00".repeat(32));
        assert!(join_uplink(&child.services, wrong).await.is_err());
        let mut pinned = alice_join(parent_url);
        pinned.tls_fingerprint = Some(fingerprint);
        join_uplink(&child.services, pinned).await.unwrap();
        let (token, _) = child_token(&child).await;
        let response = child_request(
            &child,
            &token,
            "POST",
            "/v1/chat/completions",
            r#"{"model":"alice-gpt","messages":[{"role":"user","content":"hello"}]}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK, "{}", response.status());
        assert_eq!(*capture.calls.lock().unwrap(), 1);
    }
}

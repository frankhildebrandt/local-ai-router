use std::{
    net::{IpAddr, UdpSocket},
    path::PathBuf,
};

use anyhow::Context;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use subtle::ConstantTimeEq;
use tokio::fs;

use crate::{
    commands::AppServices,
    core::AppCore,
    domain::{ModelRoute, RouteRole, RouteTarget, TargetKind},
    identity::DirectoryUser,
    providers::validate_cloud_base_url,
    public_models,
    routing::TargetRoutingProfile,
    secrets::generate_local_token,
    storage::{LocalModelMeta, ModelTarget, Store},
    uplink,
};

pub const REPLICA_INBOUND_ACCOUNT: &str = "replica-inbound-token";
pub const HEALTH_TTL_SECS: i64 = 45;
const TARGET_PREFIX: &str = "replica:";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Replica {
    pub target_id: String,
    pub network_model_id: String,
    pub child_node_id: String,
    pub user_id: String,
    pub local_model_id: String,
    pub callback_url: String,
    pub tls_fingerprint: Option<String>,
    pub last_seen: chrono::DateTime<Utc>,
    pub healthy: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NetworkModel {
    pub id: String,
    pub capabilities: Vec<String>,
    pub replicas: Vec<ReplicaView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReplicaView {
    pub child_node_id: String,
    pub local_model_id: String,
    pub callback_url: String,
    pub healthy: bool,
    pub last_seen: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishLocalModelInput {
    pub local_model_id: String,
    pub network_model_id: String,
    pub callback_url: Option<String>,
    pub tls_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdvertiseRequest {
    pub local_model_id: String,
    pub network_model_id: String,
    pub callback_url: String,
    pub callback_token: String,
    pub capabilities: Vec<String>,
    pub tls_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnpublishInput {
    pub network_model_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SharedImage {
    pub id: String,
    pub name: String,
    pub source_kind: String,
    pub source_ref: String,
    pub revision: Option<String>,
    pub filename: Option<String>,
    pub kind: String,
    pub capabilities: Vec<String>,
    pub size_bytes: Option<i64>,
    pub nodes: Vec<SharedImageNode>,
    pub created_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SharedImageNode {
    pub node_id: String,
    pub installed_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterSharedImageInput {
    pub id: String,
    pub name: String,
    pub source_kind: String,
    pub source_ref: String,
    pub revision: Option<String>,
    pub filename: Option<String>,
    pub kind: String,
    pub capabilities: Vec<String>,
    pub local_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullSharedImageInput {
    pub id: String,
}

pub fn callback_secret_account(child_node_id: &str) -> String {
    format!("replica-callback:{child_node_id}")
}

pub fn is_replica_target_id(id: &str) -> bool {
    id.starts_with(TARGET_PREFIX)
}

pub async fn migrate(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS replicas (
            target_id TEXT PRIMARY KEY,
            network_model_id TEXT NOT NULL,
            child_node_id TEXT NOT NULL,
            user_id TEXT NOT NULL,
            local_model_id TEXT NOT NULL,
            callback_url TEXT NOT NULL,
            tls_fingerprint TEXT,
            last_seen TEXT NOT NULL,
            healthy INTEGER NOT NULL DEFAULT 1,
            UNIQUE(network_model_id, child_node_id)
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS published_offers (
            network_model_id TEXT NOT NULL,
            local_model_id TEXT NOT NULL,
            advertised_at TEXT NOT NULL,
            PRIMARY KEY (network_model_id, local_model_id)
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS shared_images (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            source_kind TEXT NOT NULL,
            source_ref TEXT NOT NULL,
            revision TEXT,
            filename TEXT,
            kind TEXT NOT NULL,
            capabilities TEXT NOT NULL DEFAULT '[]',
            size_bytes INTEGER,
            blob_path TEXT,
            blob_bytes BLOB,
            created_by_user_id TEXT,
            created_at TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS shared_image_nodes (
            image_id TEXT NOT NULL,
            node_id TEXT NOT NULL,
            installed_at TEXT NOT NULL,
            PRIMARY KEY (image_id, node_id),
            FOREIGN KEY (image_id) REFERENCES shared_images(id) ON DELETE CASCADE
        )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn replica_by_target(store: &Store, target_id: &str) -> anyhow::Result<Option<Replica>> {
    let row = sqlx::query(
        "SELECT target_id,network_model_id,child_node_id,user_id,local_model_id,callback_url,tls_fingerprint,last_seen,healthy
         FROM replicas WHERE target_id=?",
    )
    .bind(target_id)
    .fetch_optional(store.pool())
    .await?;
    Ok(row.map(|row| replica_from_row(&row)))
}

pub async fn authenticate_replica_inbound(core: &AppCore, token: Option<&str>) -> bool {
    let Some(token) = token else {
        return false;
    };
    let Ok(Some(expected)) = core.secrets.get(REPLICA_INBOUND_ACCOUNT) else {
        return false;
    };
    expected.as_bytes().ct_eq(token.as_bytes()).into()
}

pub async fn offered_local_model_ids(store: &Store) -> anyhow::Result<Vec<String>> {
    sqlx::query_scalar::<_, String>("SELECT DISTINCT local_model_id FROM published_offers")
        .fetch_all(store.pool())
        .await
        .map_err(Into::into)
}

pub async fn list_network_models(store: &Store) -> anyhow::Result<Vec<NetworkModel>> {
    mark_stale(store).await?;
    let rows = sqlx::query(
        "SELECT target_id,network_model_id,child_node_id,user_id,local_model_id,callback_url,tls_fingerprint,last_seen,healthy
         FROM replicas ORDER BY network_model_id, child_node_id",
    )
    .fetch_all(store.pool())
    .await?;
    let mut models = Vec::<NetworkModel>::new();
    for row in rows {
        let replica = replica_from_row(&row);
        let view = ReplicaView {
            child_node_id: replica.child_node_id,
            local_model_id: replica.local_model_id,
            callback_url: replica.callback_url,
            healthy: replica.healthy,
            last_seen: replica.last_seen,
        };
        if let Some(model) = models
            .iter_mut()
            .find(|model| model.id == replica.network_model_id)
        {
            model.replicas.push(view);
        } else {
            let capabilities = store
                .route(&replica.network_model_id)
                .await?
                .map(|route| route.capabilities)
                .unwrap_or_default();
            models.push(NetworkModel {
                id: replica.network_model_id,
                capabilities,
                replicas: vec![view],
            });
        }
    }
    for route in store.routes().await? {
        if route.targets.iter().any(|target| target.kind.is_replica())
            && !models.iter().any(|model| model.id == route.alias)
        {
            models.push(NetworkModel {
                id: route.alias,
                capabilities: route.capabilities,
                replicas: Vec::new(),
            });
        }
    }
    models.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(models)
}

pub async fn publish_local_model(
    services: &AppServices,
    input: PublishLocalModelInput,
) -> anyhow::Result<NetworkModel> {
    let parent = uplink::load_parent(&services.core.store)
        .await?
        .context("not joined to an uplink")?;
    if !parent.may_publish {
        anyhow::bail!("this uplink user is not allowed to publish local models");
    }
    let local_id = input.local_model_id.trim();
    let network_id = normalize_network_id(&input.network_model_id)?;
    let (target, capabilities) = local_offer(&services.core.store, local_id).await?;
    let callback = match input.callback_url.clone() {
        Some(url) => url,
        None => dashboard_callback_url(services).await,
    };
    let callback_url = normalize_callback_url(&callback)?;
    let public_id = public_models::preferred_public_id(&target.provider_model, &target.name);
    let token = replica_inbound_token(services)?;
    let client = uplink::http_client(parent.tls_fingerprint.as_deref())?;
    let parent_token = services
        .core
        .secrets
        .get(uplink::TOKEN_ACCOUNT)?
        .context("uplink token missing")?;
    let response = client
        .post(format!("{}/uplink/publish", parent.base_url))
        .bearer_auth(&parent_token)
        .json(&AdvertiseRequest {
            local_model_id: public_id.clone(),
            network_model_id: network_id.clone(),
            callback_url,
            callback_token: token,
            capabilities,
            tls_fingerprint: input.tls_fingerprint.or(services.tls_fingerprint.clone()),
        })
        .send()
        .await
        .context("unable to reach uplink parent")?;
    let status = response.status();
    let payload = response.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("{}", publish_error(&payload, status.as_u16()));
    }
    sqlx::query(
        "INSERT INTO published_offers(network_model_id,local_model_id,advertised_at)
         VALUES(?,?,?)
         ON CONFLICT(network_model_id, local_model_id) DO UPDATE SET advertised_at=excluded.advertised_at",
    )
    .bind(&network_id)
    .bind(&public_id)
    .bind(Utc::now().to_rfc3339())
    .execute(services.core.store.pool())
    .await?;
    serde_json::from_str(&payload).context("invalid publish response")
}

pub async fn unpublish_local_model(
    services: &AppServices,
    input: UnpublishInput,
) -> anyhow::Result<()> {
    let parent = uplink::load_parent(&services.core.store)
        .await?
        .context("not joined to an uplink")?;
    let token = services
        .core
        .secrets
        .get(uplink::TOKEN_ACCOUNT)?
        .context("uplink token missing")?;
    let client = uplink::http_client(parent.tls_fingerprint.as_deref())?;
    let _ = client
        .post(format!("{}/uplink/unpublish", parent.base_url))
        .bearer_auth(token)
        .json(&serde_json::json!({"network_model_id": input.network_model_id}))
        .send()
        .await;
    sqlx::query("DELETE FROM published_offers WHERE network_model_id=?")
        .bind(&input.network_model_id)
        .execute(services.core.store.pool())
        .await?;
    Ok(())
}

pub async fn accept_publish(
    core: &AppCore,
    caller: &uplink::UplinkCaller,
    request: AdvertiseRequest,
) -> anyhow::Result<NetworkModel> {
    let user = core
        .store
        .directory_user(&caller.user_id)
        .await?
        .context("uplink user missing")?;
    let permissions = core.store.permissions_for(&user).await?;
    if !permissions.may_publish {
        anyhow::bail!("this uplink user is not allowed to publish local models");
    }
    let network_id = normalize_network_id(&request.network_model_id)?;
    let callback_url = normalize_callback_url(&request.callback_url)?;
    let local_model_id = request.local_model_id.trim();
    if local_model_id.is_empty() {
        anyhow::bail!("local model ID is required");
    }
    if request.callback_token.trim().is_empty() {
        anyhow::bail!("replica session token is required");
    }
    let target_id = format!("{TARGET_PREFIX}{network_id}:{}", caller.child_node_id);
    let now = Utc::now();
    core.secrets.set(
        &callback_secret_account(&caller.child_node_id),
        request.callback_token.trim(),
    )?;
    sqlx::query(
        "INSERT INTO replicas(target_id,network_model_id,child_node_id,user_id,local_model_id,callback_url,tls_fingerprint,last_seen,healthy)
         VALUES(?,?,?,?,?,?,?,?,1)
         ON CONFLICT(network_model_id, child_node_id) DO UPDATE SET
            target_id=excluded.target_id,
            user_id=excluded.user_id,
            local_model_id=excluded.local_model_id,
            callback_url=excluded.callback_url,
            tls_fingerprint=excluded.tls_fingerprint,
            last_seen=excluded.last_seen,
            healthy=1",
    )
    .bind(&target_id)
    .bind(&network_id)
    .bind(&caller.child_node_id)
    .bind(&caller.user_id)
    .bind(local_model_id)
    .bind(&callback_url)
    .bind(request.tls_fingerprint.as_deref())
    .bind(now.to_rfc3339())
    .execute(core.store.pool())
    .await?;
    core.store
        .upsert_target(&ModelTarget {
            id: target_id.clone(),
            provider_id: None,
            name: network_id.clone(),
            kind: TargetKind::Replica,
            provider_model: local_model_id.to_owned(),
            local_path: None,
            runtime_url: Some(callback_url),
            wire_protocol: crate::providers::WireProtocol::OpenAiChat,
            capabilities: request.capabilities.clone(),
            enabled: true,
            state: "ready".into(),
            size_bytes: None,
            local: LocalModelMeta::default(),
        })
        .await?;
    let mut profile = TargetRoutingProfile::neutral(&target_id, TargetKind::Replica);
    profile.context_window = 128_000;
    core.store.upsert_target_routing_profile(&profile).await?;
    sync_network_route(&core.store, &network_id, &request.capabilities).await?;
    grant_model(&core.store, &user, &network_id).await?;
    list_network_models(&core.store)
        .await?
        .into_iter()
        .find(|model| model.id == network_id)
        .context("network model missing after publish")
}

pub async fn accept_unpublish(
    core: &AppCore,
    caller: &uplink::UplinkCaller,
    network_model_id: &str,
) -> anyhow::Result<()> {
    drop_replica(core, network_model_id, &caller.child_node_id).await
}

pub async fn accept_heartbeat(
    core: &AppCore,
    caller: &uplink::UplinkCaller,
) -> anyhow::Result<Vec<ReplicaView>> {
    let user = core
        .store
        .directory_user(&caller.user_id)
        .await?
        .context("uplink user missing")?;
    let permissions = core.store.permissions_for(&user).await?;
    if !permissions.may_publish {
        drop_child_replicas(core, &caller.child_node_id).await?;
        anyhow::bail!("this uplink user is not allowed to publish local models");
    }
    let now = Utc::now().to_rfc3339();
    sqlx::query("UPDATE replicas SET last_seen=?, healthy=1 WHERE child_node_id=?")
        .bind(&now)
        .bind(&caller.child_node_id)
        .execute(core.store.pool())
        .await?;
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT target_id FROM replicas WHERE child_node_id=?",
    )
    .bind(&caller.child_node_id)
    .fetch_all(core.store.pool())
    .await?;
    for id in ids {
        if let Some(mut target) = core.store.target(&id).await? {
            target.enabled = true;
            target.state = "ready".into();
            core.store.upsert_target(&target).await?;
        }
    }
    mark_stale(&core.store).await?;
    let models = list_network_models(&core.store).await?;
    Ok(models
        .into_iter()
        .flat_map(|model| model.replicas)
        .filter(|replica| replica.child_node_id == caller.child_node_id)
        .collect())
}

pub async fn drop_child_replicas(core: &AppCore, child_node_id: &str) -> anyhow::Result<()> {
    let network_ids: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT network_model_id FROM replicas WHERE child_node_id=?",
    )
    .bind(child_node_id)
    .fetch_all(core.store.pool())
    .await?;
    let target_ids: Vec<String> =
        sqlx::query_scalar("SELECT target_id FROM replicas WHERE child_node_id=?")
            .bind(child_node_id)
            .fetch_all(core.store.pool())
            .await?;
    sqlx::query("DELETE FROM replicas WHERE child_node_id=?")
        .bind(child_node_id)
        .execute(core.store.pool())
        .await?;
    sqlx::query("DELETE FROM shared_image_nodes WHERE node_id=?")
        .bind(child_node_id)
        .execute(core.store.pool())
        .await?;
    let _ = core.secrets.delete(&callback_secret_account(child_node_id));
    for target_id in target_ids {
        let _ = core.store.delete_target(&target_id).await;
    }
    for network_id in network_ids {
        sync_network_route(&core.store, &network_id, &[]).await?;
    }
    Ok(())
}

pub async fn drop_replicas_for_user(core: &AppCore, user_id: &str) -> anyhow::Result<()> {
    let children: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT child_node_id FROM replicas WHERE user_id=?")
            .bind(user_id)
            .fetch_all(core.store.pool())
            .await?;
    for child in children {
        drop_child_replicas(core, &child).await?;
    }
    Ok(())
}

pub async fn mark_replica_unhealthy(store: &Store, target_id: &str) -> anyhow::Result<()> {
    sqlx::query("UPDATE replicas SET healthy=0 WHERE target_id=?")
        .bind(target_id)
        .execute(store.pool())
        .await?;
    if let Some(mut target) = store.target(target_id).await? {
        target.enabled = false;
        target.state = "unhealthy".into();
        store.upsert_target(&target).await?;
        sync_network_route(store, &target.name, &target.capabilities).await?;
    }
    Ok(())
}

pub async fn clear_local_offers(services: &AppServices) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM published_offers")
        .execute(services.core.store.pool())
        .await?;
    let _ = services.core.secrets.delete(REPLICA_INBOUND_ACCOUNT);
    Ok(())
}

pub async fn heartbeat_to_parent(services: &AppServices) -> anyhow::Result<()> {
    let offers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM published_offers")
        .fetch_one(services.core.store.pool())
        .await?;
    if offers == 0 {
        return Ok(());
    }
    let Some(parent) = uplink::load_parent(&services.core.store).await? else {
        return Ok(());
    };
    let Some(token) = services.core.secrets.get(uplink::TOKEN_ACCOUNT)? else {
        return Ok(());
    };
    let client = uplink::http_client(parent.tls_fingerprint.as_deref())?;
    let _ = client
        .post(format!("{}/uplink/replicas/heartbeat", parent.base_url))
        .bearer_auth(token)
        .send()
        .await;
    Ok(())
}

pub async fn maintain(services: &AppServices) -> anyhow::Result<()> {
    mark_stale(&services.core.store).await?;
    heartbeat_to_parent(services).await
}

pub async fn replica_http_client(
    store: &Store,
    target_id: &str,
) -> anyhow::Result<reqwest::Client> {
    let replica = replica_by_target(store, target_id)
        .await?
        .context("replica missing")?;
    uplink::http_client(replica.tls_fingerprint.as_deref())
}

pub async fn list_shared_images(store: &Store) -> anyhow::Result<Vec<SharedImage>> {
    load_shared_images(store).await
}

pub async fn list_parent_shared_images(services: &AppServices) -> anyhow::Result<Vec<SharedImage>> {
    let Some(parent) = uplink::load_parent(&services.core.store).await? else {
        return Ok(Vec::new());
    };
    let Some(token) = services.core.secrets.get(uplink::TOKEN_ACCOUNT)? else {
        return Ok(Vec::new());
    };
    let client = uplink::http_client(parent.tls_fingerprint.as_deref())?;
    let response = client
        .get(format!("{}/uplink/images", parent.base_url))
        .bearer_auth(token)
        .send()
        .await
        .context("unable to reach uplink parent")?;
    if !response.status().is_success() {
        anyhow::bail!("parent image catalog failed: {}", response.status());
    }
    Ok(response.json().await.unwrap_or_default())
}

pub async fn register_shared_image(
    services: &AppServices,
    input: RegisterSharedImageInput,
) -> anyhow::Result<SharedImage> {
    if let Some(parent) = uplink::load_parent(&services.core.store).await? {
        if !parent.may_publish {
            anyhow::bail!("this uplink user is not allowed to register shared images");
        }
        return register_image_on_parent(services, &parent, input).await;
    }
    if input.source_kind.trim() == "local_blob"
        && input
            .local_path
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
    {
        anyhow::bail!("local imports need a file path so another node can pull a copy");
    }
    register_image(&services.core.store, None, &input).await
}

async fn register_image_on_parent(
    services: &AppServices,
    parent: &uplink::UplinkParent,
    input: RegisterSharedImageInput,
) -> anyhow::Result<SharedImage> {
    let token = services
        .core
        .secrets
        .get(uplink::TOKEN_ACCOUNT)?
        .context("uplink token missing")?;
    let client = uplink::http_client(parent.tls_fingerprint.as_deref())?;
    let mut remote = input.clone();
    let local_path = remote.local_path.take();
    let response = client
        .post(format!("{}/uplink/images", parent.base_url))
        .bearer_auth(&token)
        .json(&remote)
        .send()
        .await
        .context("unable to reach uplink parent")?;
    let status = response.status().as_u16();
    if !response.status().is_success() {
        anyhow::bail!("{}", publish_error(&response.text().await.unwrap_or_default(), status));
    }
    let image: SharedImage = response.json().await.context("invalid catalog response")?;
    if remote.source_kind.trim() == "local_blob" {
        let path = local_path
            .as_deref()
            .context("local imports need a file path so another node can pull a copy")?;
        let bytes = fs::read(path).await?;
        let uploaded = client
            .post(format!("{}/uplink/images/{}/blob", parent.base_url, image.id))
            .bearer_auth(&token)
            .body(bytes)
            .send()
            .await
            .context("unable to upload catalog blob")?;
        let uploaded_status = uploaded.status().as_u16();
        if !uploaded.status().is_success() {
            anyhow::bail!(
                "{}",
                publish_error(&uploaded.text().await.unwrap_or_default(), uploaded_status)
            );
        }
        return Ok(uploaded.json().await.unwrap_or(image));
    }
    Ok(image)
}

pub async fn accept_register_image(
    core: &AppCore,
    caller: &uplink::UplinkCaller,
    input: RegisterSharedImageInput,
) -> anyhow::Result<SharedImage> {
    let user = core
        .store
        .directory_user(&caller.user_id)
        .await?
        .context("uplink user missing")?;
    let permissions = core.store.permissions_for(&user).await?;
    if !permissions.may_publish {
        anyhow::bail!("this uplink user is not allowed to register shared images");
    }
    register_image(&core.store, Some(&user.id), &input).await
}

pub async fn accept_image_blob(
    core: &AppCore,
    caller: &uplink::UplinkCaller,
    image_id: &str,
    bytes: &[u8],
) -> anyhow::Result<SharedImage> {
    let user = core
        .store
        .directory_user(&caller.user_id)
        .await?
        .context("uplink user missing")?;
    let permissions = core.store.permissions_for(&user).await?;
    if !permissions.may_publish {
        anyhow::bail!("this uplink user is not allowed to publish local models");
    }
    let image = get_shared_image(&core.store, image_id)
        .await?
        .context("shared image not found")?;
    if image.source_kind != "local_blob" {
        anyhow::bail!("this catalog entry is a hub reference; pull it from Hugging Face or CivitAI");
    }
    sqlx::query("UPDATE shared_images SET blob_bytes=?, size_bytes=? WHERE id=?")
        .bind(bytes)
        .bind(bytes.len() as i64)
        .bind(image_id)
        .execute(core.store.pool())
        .await?;
    record_image_node(&core.store, image_id, &caller.child_node_id).await?;
    get_shared_image(&core.store, image_id)
        .await?
        .context("shared image missing after upload")
}

pub async fn shared_image_blob(store: &Store, image_id: &str) -> anyhow::Result<Vec<u8>> {
    let row = sqlx::query("SELECT blob_bytes FROM shared_images WHERE id=?")
        .bind(image_id)
        .fetch_optional(store.pool())
        .await?
        .context("shared image not found")?;
    row.get::<Option<Vec<u8>>, _>("blob_bytes")
        .context("this catalog entry has no stored bytes; pull it from the hub")
}

pub async fn pull_shared_image(
    services: &AppServices,
    input: PullSharedImageInput,
) -> anyhow::Result<ModelTarget> {
    let images = list_parent_shared_images(services).await?;
    let image = images
        .into_iter()
        .find(|item| item.id == input.id)
        .context("shared image not found on parent")?;
    let parent = uplink::load_parent(&services.core.store)
        .await?
        .context("not joined to an uplink")?;
    let token = services
        .core
        .secrets
        .get(uplink::TOKEN_ACCOUNT)?
        .context("uplink token missing")?;
    let kind = match image.kind.as_str() {
        "mlx" => TargetKind::Mlx,
        _ => TargetKind::Gguf,
    };
    let imported = if image.source_kind == "local_blob" {
        let client = uplink::http_client(parent.tls_fingerprint.as_deref())?;
        let response = client
            .get(format!("{}/uplink/images/{}/blob", parent.base_url, image.id))
            .bearer_auth(&token)
            .send()
            .await
            .context("unable to download catalog blob")?;
        if !response.status().is_success() {
            anyhow::bail!("catalog blob download failed: {}", response.status());
        }
        let bytes = response.bytes().await?;
        fs::create_dir_all(&services.model_library).await?;
        let filename = image
            .filename
            .clone()
            .unwrap_or_else(|| format!("{}.gguf", image.id));
        let dest = services.model_library.join(filename);
        fs::write(&dest, &bytes).await?;
        crate::library::validate_model(&dest, &kind).await?;
        crate::library::ImportedModel {
            path: dest.to_string_lossy().into_owned(),
            size_bytes: bytes.len() as u64,
            kind,
        }
    } else {
        anyhow::bail!(
            "install this image from Hugging Face, CivitAI, or the local catalog using source {}",
            image.source_ref
        );
    };
    let target = ModelTarget {
        id: format!("shared:{}", image.id),
        provider_id: None,
        name: image.name.clone(),
        kind: imported.kind,
        provider_model: image.id.clone(),
        local_path: Some(imported.path),
        runtime_url: None,
        wire_protocol: crate::providers::WireProtocol::OpenAiChat,
        capabilities: image.capabilities.clone(),
        enabled: true,
        state: "stopped".into(),
        size_bytes: Some(imported.size_bytes as i64),
        local: LocalModelMeta {
            source_repo: Some(image.source_ref.clone()),
            source_revision: image.revision.clone(),
            catalog_id: Some(image.id.clone()),
            trust_status: Some("network".into()),
            ..LocalModelMeta::default()
        },
    };
    services.core.store.upsert_target(&target).await?;
    let _ = notify_parent_installed(services, &image.id).await;
    Ok(target)
}

pub async fn report_shared_image_installed(
    services: &AppServices,
    input: PullSharedImageInput,
) -> anyhow::Result<()> {
    notify_parent_installed(services, &input.id).await
}

async fn notify_parent_installed(services: &AppServices, image_id: &str) -> anyhow::Result<()> {
    let parent = uplink::load_parent(&services.core.store)
        .await?
        .context("not joined to an uplink")?;
    let token = services
        .core
        .secrets
        .get(uplink::TOKEN_ACCOUNT)?
        .context("uplink token missing")?;
    let client = uplink::http_client(parent.tls_fingerprint.as_deref())?;
    let response = client
        .post(format!(
            "{}/uplink/images/{}/installed",
            parent.base_url, image_id
        ))
        .bearer_auth(token)
        .send()
        .await
        .context("unable to reach uplink parent")?;
    if !response.status().is_success() {
        anyhow::bail!("catalog install report failed: {}", response.status());
    }
    Ok(())
}

pub async fn mark_image_installed(
    store: &Store,
    image_id: &str,
    node_id: &str,
) -> anyhow::Result<()> {
    get_shared_image(store, image_id)
        .await?
        .context("shared image not found")?;
    record_image_node(store, image_id, node_id).await
}

async fn dashboard_callback_url(services: &AppServices) -> String {
    let scheme = if services.tls_required { "https" } else { "http" };
    let parent_url = uplink::load_parent(&services.core.store)
        .await
        .ok()
        .flatten()
        .map(|parent| parent.base_url);
    let host = callback_host(services.bind_ip, parent_url.as_deref());
    format!("{scheme}://{host}:{}", services.port)
}

fn callback_host(bind_ip: IpAddr, parent_base_url: Option<&str>) -> String {
    if bind_ip.is_loopback() {
        return "127.0.0.1".into();
    }
    if !bind_ip.is_unspecified() {
        return bind_ip.to_string();
    }
    parent_base_url
        .and_then(local_ip_toward)
        .filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
        .map(|ip| ip.to_string())
        .or_else(first_non_loopback_ip)
        .unwrap_or_else(|| "127.0.0.1".into())
}

fn local_ip_toward(base_url: &str) -> Option<IpAddr> {
    let parsed = reqwest::Url::parse(base_url).ok()?;
    let host = parsed.host_str()?;
    let port = parsed.port_or_known_default()?;
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect((host, port)).ok()?;
    Some(socket.local_addr().ok()?.ip())
}

fn first_non_loopback_ip() -> Option<String> {
    let networks = sysinfo::Networks::new_with_refreshed_list();
    let mut v6 = None;
    for (_, data) in networks.iter() {
        for network in data.ip_networks() {
            let ip = network.addr;
            if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
                continue;
            }
            if let IpAddr::V4(v4) = ip {
                if v4.is_link_local() {
                    continue;
                }
                return Some(ip.to_string());
            }
            if v6.is_none() {
                v6 = Some(ip.to_string());
            }
        }
    }
    v6
}

fn replica_from_row(row: &sqlx::sqlite::SqliteRow) -> Replica {
    Replica {
        target_id: row.get("target_id"),
        network_model_id: row.get("network_model_id"),
        child_node_id: row.get("child_node_id"),
        user_id: row.get("user_id"),
        local_model_id: row.get("local_model_id"),
        callback_url: row.get("callback_url"),
        tls_fingerprint: row.get("tls_fingerprint"),
        last_seen: row
            .get::<String, _>("last_seen")
            .parse()
            .unwrap_or_else(|_| Utc::now()),
        healthy: row.get::<i64, _>("healthy") != 0,
    }
}

fn normalize_network_id(value: &str) -> anyhow::Result<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        anyhow::bail!("network model ID cannot contain whitespace");
    }
    if public_models::is_reserved_public_model_id(value)
        || value.starts_with("uplink:")
        || value.starts_with(TARGET_PREFIX)
    {
        anyhow::bail!("network model ID is reserved");
    }
    Ok(value.to_owned())
}

fn normalize_callback_url(value: &str) -> anyhow::Result<String> {
    let value = validate_cloud_base_url(value, true)?;
    Ok(value
        .trim_end_matches("/v1")
        .trim_end_matches('/')
        .to_owned())
}

async fn local_offer(store: &Store, public_id: &str) -> anyhow::Result<(ModelTarget, Vec<String>)> {
    let resolved = public_models::resolve_public_model(store, public_id)
        .await?
        .context("unknown local model")?;
    if !resolved.route.enabled {
        anyhow::bail!("local model is disabled");
    }
    let hop = resolved
        .route
        .targets
        .iter()
        .find(|hop| hop.enabled)
        .context("local model has no enabled target")?;
    let target = store
        .target(&hop.id)
        .await?
        .context("local model target missing")?;
    if !target.kind.is_local() {
        anyhow::bail!("only local models can be offered to a parent");
    }
    Ok((target, resolved.route.capabilities))
}

fn replica_inbound_token(services: &AppServices) -> anyhow::Result<String> {
    if let Some(token) = services.core.secrets.get(REPLICA_INBOUND_ACCOUNT)? {
        return Ok(token);
    }
    let token = generate_local_token().replacen("lar_", "lar_replica_", 1);
    services
        .core
        .secrets
        .set(REPLICA_INBOUND_ACCOUNT, &token)?;
    Ok(token)
}

async fn grant_model(store: &Store, user: &DirectoryUser, model_id: &str) -> anyhow::Result<()> {
    let Some(mut ids) = user.allowed_model_ids.clone() else {
        return Ok(());
    };
    if ids.iter().any(|id| id == model_id) {
        return Ok(());
    }
    ids.push(model_id.to_owned());
    sqlx::query("UPDATE directory_users SET allowed_model_ids=? WHERE id=?")
        .bind(serde_json::to_string(&ids)?)
        .bind(&user.id)
        .execute(store.pool())
        .await?;
    Ok(())
}

async fn sync_network_route(
    store: &Store,
    network_id: &str,
    fallback_capabilities: &[String],
) -> anyhow::Result<()> {
    let replicas = sqlx::query(
        "SELECT target_id,local_model_id,healthy FROM replicas WHERE network_model_id=? ORDER BY child_node_id",
    )
    .bind(network_id)
    .fetch_all(store.pool())
    .await?;
    let mut capabilities = fallback_capabilities.to_vec();
    if let Some(existing) = store.route(network_id).await? {
        if capabilities.is_empty() {
            capabilities = existing.capabilities;
        }
    }
    let mut targets = Vec::new();
    for (index, row) in replicas.iter().enumerate() {
        let target_id: String = row.get("target_id");
        let model: String = row.get("local_model_id");
        let healthy = row.get::<i64, _>("healthy") != 0;
        targets.push(RouteTarget {
            id: target_id,
            kind: TargetKind::Replica,
            model,
            priority: ((index + 1) * 10) as i64,
            enabled: healthy,
            role: RouteRole::Primary,
        });
    }
    store
        .upsert_route(&ModelRoute {
            alias: network_id.to_owned(),
            enabled: true,
            capabilities,
            targets,
        })
        .await
}

async fn drop_replica(core: &AppCore, network_id: &str, child_node_id: &str) -> anyhow::Result<()> {
    let target_id: Option<String> = sqlx::query_scalar(
        "SELECT target_id FROM replicas WHERE network_model_id=? AND child_node_id=?",
    )
    .bind(network_id)
    .bind(child_node_id)
    .fetch_optional(core.store.pool())
    .await?;
    sqlx::query("DELETE FROM replicas WHERE network_model_id=? AND child_node_id=?")
        .bind(network_id)
        .bind(child_node_id)
        .execute(core.store.pool())
        .await?;
    if let Some(target_id) = target_id {
        let _ = core.store.delete_target(&target_id).await;
    }
    let remaining: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM replicas WHERE child_node_id=?")
            .bind(child_node_id)
            .fetch_one(core.store.pool())
            .await?;
    if remaining == 0 {
        let _ = core.secrets.delete(&callback_secret_account(child_node_id));
    }
    sync_network_route(&core.store, network_id, &[]).await
}

async fn mark_stale(store: &Store) -> anyhow::Result<()> {
    let cutoff = (Utc::now() - Duration::seconds(HEALTH_TTL_SECS)).to_rfc3339();
    let stale: Vec<String> = sqlx::query_scalar(
        "SELECT target_id FROM replicas WHERE healthy=1 AND last_seen < ?",
    )
    .bind(&cutoff)
    .fetch_all(store.pool())
    .await?;
    for target_id in stale {
        mark_replica_unhealthy(store, &target_id).await?;
    }
    Ok(())
}

fn publish_error(payload: &str, status: u16) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) {
        if let Some(message) = value
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(serde_json::Value::as_str)
        {
            return message.to_owned();
        }
    }
    format!("publish failed ({status})")
}

async fn register_image(
    store: &Store,
    created_by: Option<&str>,
    input: &RegisterSharedImageInput,
) -> anyhow::Result<SharedImage> {
    let id = normalize_network_id(&input.id)?;
    let source_kind = match input.source_kind.trim() {
        "huggingface" | "civitai" | "catalog" | "local_blob" => input.source_kind.trim(),
        other => anyhow::bail!("unknown image source {other}"),
    };
    let kind = match input.kind.trim() {
        "mlx" | "gguf" | "image" | "speech" => input.kind.trim(),
        other => anyhow::bail!("unknown artifact kind {other}"),
    };
    if input.source_ref.trim().is_empty() {
        anyhow::bail!("source_ref is required");
    }
    let mut blob_bytes: Option<Vec<u8>> = None;
    let mut size_bytes = None;
    if source_kind == "local_blob" {
        if let Some(path) = input
            .local_path
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
        {
            let source = PathBuf::from(path);
            if source.is_dir() {
                anyhow::bail!("directory imports must be archived before they are published");
            }
            let bytes = fs::read(&source).await?;
            size_bytes = Some(bytes.len() as i64);
            blob_bytes = Some(bytes);
        }
    }
    sqlx::query(
        "INSERT INTO shared_images(id,name,source_kind,source_ref,revision,filename,kind,capabilities,size_bytes,blob_bytes,created_by_user_id,created_at)
         VALUES(?,?,?,?,?,?,?,?,?,?,?,?)
         ON CONFLICT(id) DO UPDATE SET
            name=excluded.name,
            source_kind=excluded.source_kind,
            source_ref=excluded.source_ref,
            revision=excluded.revision,
            filename=excluded.filename,
            kind=excluded.kind,
            capabilities=excluded.capabilities,
            size_bytes=COALESCE(excluded.size_bytes, shared_images.size_bytes),
            blob_bytes=COALESCE(excluded.blob_bytes, shared_images.blob_bytes)",
    )
    .bind(&id)
    .bind(input.name.trim())
    .bind(source_kind)
    .bind(input.source_ref.trim())
    .bind(input.revision.as_deref())
    .bind(input.filename.as_deref())
    .bind(kind)
    .bind(serde_json::to_string(&input.capabilities)?)
    .bind(size_bytes)
    .bind(blob_bytes.as_deref())
    .bind(created_by)
    .bind(Utc::now().to_rfc3339())
    .execute(store.pool())
    .await?;
    get_shared_image(store, &id)
        .await?
        .context("shared image missing after register")
}

async fn load_shared_images(store: &Store) -> anyhow::Result<Vec<SharedImage>> {
    let rows = sqlx::query(
        "SELECT id,name,source_kind,source_ref,revision,filename,kind,capabilities,size_bytes,created_at
         FROM shared_images ORDER BY name",
    )
    .fetch_all(store.pool())
    .await?;
    let mut images = Vec::new();
    for row in rows {
        images.push(shared_image_from_row(store, &row).await?);
    }
    Ok(images)
}

async fn get_shared_image(store: &Store, id: &str) -> anyhow::Result<Option<SharedImage>> {
    let row = sqlx::query(
        "SELECT id,name,source_kind,source_ref,revision,filename,kind,capabilities,size_bytes,created_at
         FROM shared_images WHERE id=?",
    )
    .bind(id)
    .fetch_optional(store.pool())
    .await?;
    match row {
        Some(row) => Ok(Some(shared_image_from_row(store, &row).await?)),
        None => Ok(None),
    }
}

async fn shared_image_from_row(
    store: &Store,
    row: &sqlx::sqlite::SqliteRow,
) -> anyhow::Result<SharedImage> {
    let id: String = row.get("id");
    let nodes = sqlx::query("SELECT node_id, installed_at FROM shared_image_nodes WHERE image_id=?")
        .bind(&id)
        .fetch_all(store.pool())
        .await?;
    Ok(SharedImage {
        id,
        name: row.get("name"),
        source_kind: row.get("source_kind"),
        source_ref: row.get("source_ref"),
        revision: row.get("revision"),
        filename: row.get("filename"),
        kind: row.get("kind"),
        capabilities: serde_json::from_str(&row.get::<String, _>("capabilities"))
            .unwrap_or_default(),
        size_bytes: row.get("size_bytes"),
        created_at: row.get::<String, _>("created_at").parse()?,
        nodes: nodes
            .into_iter()
            .map(|node| SharedImageNode {
                node_id: node.get("node_id"),
                installed_at: node
                    .get::<String, _>("installed_at")
                    .parse()
                    .unwrap_or_else(|_| Utc::now()),
            })
            .collect(),
    })
}

async fn record_image_node(store: &Store, image_id: &str, node_id: &str) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO shared_image_nodes(image_id,node_id,installed_at) VALUES(?,?,?)
         ON CONFLICT(image_id, node_id) DO UPDATE SET installed_at=excluded.installed_at",
    )
    .bind(image_id)
    .bind(node_id)
    .bind(Utc::now().to_rfc3339())
    .execute(store.pool())
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::{serve_gateway, test_engine},
        identity::{create_user, CreateUserInput},
        providers::WireProtocol,
        uplink::{join_uplink, JoinUplinkInput},
    };
    use std::path::Path;
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
        bodies: Mutex<Vec<Value>>,
        calls: Mutex<usize>,
        reply: String,
    }

    impl UpstreamCapture {
        fn new(reply: &str) -> Arc<Self> {
            Arc::new(Self {
                bodies: Mutex::new(Vec::new()),
                calls: Mutex::new(0),
                reply: reply.into(),
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

    async fn json_body(response: axum::response::Response) -> Value {
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
    }

    async fn mock_runtime(capture: Arc<UpstreamCapture>) -> String {
        let app = Router::new().fallback(move |request: Request<Body>| {
            let capture = capture.clone();
            async move {
                *capture.calls.lock().unwrap() += 1;
                let bytes = request.into_body().collect().await.unwrap().to_bytes();
                let body: Value = serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({}));
                capture.bodies.lock().unwrap().push(body.clone());
                let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
                if stream {
                    let chunk = format!(
                        "data: {{\"id\":\"s\",\"object\":\"chat.completion.chunk\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{}\"}}}}]}}\n\ndata: [DONE]\n\n",
                        capture.reply
                    );
                    return (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        chunk,
                    )
                        .into_response();
                }
                Json(json!({
                    "id": "ok",
                    "choices": [{"message":{"role":"assistant","content": capture.reply},"finish_reason":"stop"}],
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

    fn alice_join(parent_url: String) -> JoinUplinkInput {
        JoinUplinkInput {
            base_url: parent_url,
            username: Some("alice".into()),
            password: Some("alice-pass".into()),
            session_token: None,
            tls_fingerprint: None,
        }
    }

    async fn parent_with_alice(data: &Path, may_publish: bool) -> crate::engine::Engine {
        let engine = test_engine(data, None).await;
        create_user(
            &engine.services.core.store,
            CreateUserInput {
                username: "alice".into(),
                display_name: "Alice".into(),
                password: Some("alice-pass".into()),
                group_ids: Vec::new(),
                allowed_model_ids: Some(Vec::new()),
                may_publish: Some(may_publish),
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

    async fn add_local_runtime(
        engine: &crate::engine::Engine,
        id: &str,
        public_id: &str,
        runtime_url: String,
    ) {
        engine
            .services
            .core
            .store
            .upsert_target(&ModelTarget {
                id: id.into(),
                provider_id: None,
                name: public_id.into(),
                kind: TargetKind::Gguf,
                provider_model: public_id.into(),
                local_path: Some("/tmp/model.gguf".into()),
                runtime_url: Some(runtime_url),
                wire_protocol: WireProtocol::OpenAiChat,
                capabilities: vec!["chat".into(), "streaming".into()],
                enabled: true,
                state: "ready".into(),
                size_bytes: Some(12),
                local: LocalModelMeta::default(),
            })
            .await
            .unwrap();
        engine.services.runtimes.mark_test_running(id);
        engine
            .services
            .core
            .local_activity()
            .set_token(id, "runtime-token".into());
    }

    async fn child_token(engine: &crate::engine::Engine) -> String {
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
        json_body(created).await["token"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn chat(
        engine: &crate::engine::Engine,
        token: &str,
        model: &str,
        stream: bool,
    ) -> axum::response::Response {
        engine
            .router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({"model": model, "stream": stream, "messages":[{"role":"user","content":"hello"}]}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn unauthorized_publish_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let parent = parent_with_alice(&root.path().join("parent"), false).await;
        let parent_url = listen(&parent).await;
        let child = test_engine(&root.path().join("child"), None).await;
        join_uplink(&child.services, alice_join(parent_url.clone()))
            .await
            .unwrap();
        add_local_runtime(&child, "gpu", "gpu-llama", mock_runtime(UpstreamCapture::new("x")).await)
            .await;
        let error = publish_local_model(
            &child.services,
            PublishLocalModelInput {
                local_model_id: "gpu-llama".into(),
                network_model_id: "team-llama-70b".into(),
                callback_url: Some(listen(&child).await),
                tls_fingerprint: None,
            },
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("not allowed to publish"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn child_advertises_local_model_and_parent_proxies_with_replica_failover() {
        let root = tempfile::tempdir().unwrap();
        let parent = parent_with_alice(&root.path().join("parent"), true).await;
        let parent_url = listen(&parent).await;

        let first_runtime = UpstreamCapture::new("from-first");
        let second_runtime = UpstreamCapture::new("from-second");
        let child_a = test_engine(&root.path().join("a"), None).await;
        let child_b = test_engine(&root.path().join("b"), None).await;
        add_local_runtime(
            &child_a,
            "gpu",
            "gpu-llama",
            mock_runtime(first_runtime.clone()).await,
        )
        .await;
        add_local_runtime(
            &child_b,
            "gpu",
            "gpu-llama",
            mock_runtime(second_runtime.clone()).await,
        )
        .await;
        join_uplink(&child_a.services, alice_join(parent_url.clone()))
            .await
            .unwrap();
        join_uplink(&child_b.services, alice_join(parent_url.clone()))
            .await
            .unwrap();
        let url_a = listen(&child_a).await;
        let url_b = listen(&child_b).await;
        let published = publish_local_model(
            &child_a.services,
            PublishLocalModelInput {
                local_model_id: "gpu-llama".into(),
                network_model_id: "team-llama-70b".into(),
                callback_url: Some(url_a),
                tls_fingerprint: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(published.id, "team-llama-70b");
        publish_local_model(
            &child_b.services,
            PublishLocalModelInput {
                local_model_id: "gpu-llama".into(),
                network_model_id: "team-llama-70b".into(),
                callback_url: Some(url_b),
                tls_fingerprint: None,
            },
        )
        .await
        .unwrap();

        create_user(
            &parent.services.core.store,
            CreateUserInput {
                username: "bob".into(),
                display_name: "Bob".into(),
                password: Some("bob-pass".into()),
                group_ids: Vec::new(),
                allowed_model_ids: Some(vec!["team-llama-70b".into()]),
                may_publish: Some(false),
                may_admin: None,
                rpm: None,
                daily_token_budget: None,
                daily_usd_budget: None,
            },
        )
        .await
        .unwrap();
        let consumer = test_engine(&root.path().join("bob"), None).await;
        join_uplink(
            &consumer.services,
            JoinUplinkInput {
                base_url: parent_url,
                username: Some("bob".into()),
                password: Some("bob-pass".into()),
                session_token: None,
                tls_fingerprint: None,
            },
        )
        .await
        .unwrap();
        let token = child_token(&consumer).await;
        let response = chat(&consumer, &token, "team-llama-70b", false).await;
        let status = response.status();
        let body = json_body(response).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let content = body["choices"][0]["message"]["content"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            content == "from-first" || content == "from-second",
            "{content}"
        );

        let streamed = chat(&consumer, &token, "team-llama-70b", true).await;
        assert_eq!(streamed.status(), StatusCode::OK);
        let text = String::from_utf8(
            streamed
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        assert!(text.contains("from-first") || text.contains("from-second"), "{text}");

        child_a.services.shutdown.cancel();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let failover = chat(&consumer, &token, "team-llama-70b", false).await;
        assert_eq!(failover.status(), StatusCode::OK);
        let failed_over = json_body(failover).await["choices"][0]["message"]["content"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(failed_over, "from-second");
        let replicas = list_network_models(&parent.services.core.store)
            .await
            .unwrap();
        for replica in replicas.iter().flat_map(|model| &model.replicas) {
            let token = parent
                .services
                .core
                .secrets
                .get(&callback_secret_account(&replica.child_node_id))
                .unwrap()
                .expect("replica callback token");
            assert!(
                token.starts_with("lar_replica_"),
                "parent must call children with a replica session, not a local API key"
            );
        }

        crate::uplink::disconnect_uplink(&child_b.services)
            .await
            .unwrap();
        let gone = chat(&consumer, &token, "team-llama-70b", false).await;
        assert!(
            gone.status() == StatusCode::BAD_GATEWAY
                || gone.status() == StatusCode::SERVICE_UNAVAILABLE
                || gone.status() == StatusCode::NOT_FOUND,
            "{}",
            gone.status()
        );
    }

    #[tokio::test]
    async fn shared_catalog_registers_hub_source_and_survives_replica_disconnect() {
        let root = tempfile::tempdir().unwrap();
        let parent = parent_with_alice(&root.path().join("parent"), true).await;
        let parent_url = listen(&parent).await;
        register_shared_image(
            &parent.services,
            RegisterSharedImageInput {
                id: "llama-70b".into(),
                name: "Llama 70B".into(),
                source_kind: "huggingface".into(),
                source_ref: "org/llama-70b".into(),
                revision: Some("main".into()),
                filename: Some("model.gguf".into()),
                kind: "gguf".into(),
                capabilities: vec!["chat".into()],
                local_path: None,
            },
        )
        .await
        .unwrap();

        let blob = root.path().join("weights.gguf");
        std::fs::write(&blob, b"GGUFtiny-weights").unwrap();
        register_shared_image(
            &parent.services,
            RegisterSharedImageInput {
                id: "unique-import".into(),
                name: "Unique import".into(),
                source_kind: "local_blob".into(),
                source_ref: "local-import".into(),
                revision: None,
                filename: Some("unique-import.gguf".into()),
                kind: "gguf".into(),
                capabilities: vec!["chat".into()],
                local_path: Some(blob.to_string_lossy().into_owned()),
            },
        )
        .await
        .unwrap();

        let child = test_engine(&root.path().join("child"), None).await;
        join_uplink(&child.services, alice_join(parent_url)).await.unwrap();
        let catalog = list_parent_shared_images(&child.services).await.unwrap();
        assert!(catalog.iter().any(|item| item.id == "llama-70b" && item.source_kind == "huggingface"));
        report_shared_image_installed(
            &child.services,
            PullSharedImageInput {
                id: "llama-70b".into(),
            },
        )
        .await
        .unwrap();
        let pulled = pull_shared_image(
            &child.services,
            PullSharedImageInput {
                id: "unique-import".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(pulled.provider_model, "unique-import");
        assert!(PathBuf::from(pulled.local_path.unwrap()).is_file());

        let listed = list_shared_images(&parent.services.core.store)
            .await
            .unwrap();
        let unique = listed
            .iter()
            .find(|item| item.id == "unique-import")
            .unwrap();
        assert!(!unique.nodes.is_empty());
        let hub = listed.iter().find(|item| item.id == "llama-70b").unwrap();
        assert!(!hub.nodes.is_empty());

        crate::uplink::disconnect_uplink(&child.services)
            .await
            .unwrap();
        let after = list_shared_images(&parent.services.core.store)
            .await
            .unwrap();
        assert!(after.iter().any(|item| item.id == "llama-70b"));
        let hub = after.iter().find(|item| item.id == "llama-70b").unwrap();
        assert!(hub.nodes.is_empty());
        let unique = after
            .iter()
            .find(|item| item.id == "unique-import")
            .unwrap();
        assert!(unique.nodes.is_empty());
    }

    #[test]
    fn callback_host_keeps_loopback_and_specific_binds() {
        assert_eq!(
            callback_host("192.168.1.10".parse().unwrap(), None),
            "192.168.1.10"
        );
        assert_eq!(
            callback_host(
                "127.0.0.1".parse().unwrap(),
                Some("https://192.168.1.1:11435")
            ),
            "127.0.0.1"
        );
    }
}

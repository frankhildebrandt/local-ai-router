use std::{path::Path, str::FromStr};

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqliteConnectOptions, QueryBuilder, Row, Sqlite, SqlitePool};

use crate::{
    domain::{ModelRoute, RouteTarget, TargetKind},
    providers::{AuthMode, WireProtocol},
    resource::{ResourceOverrides, ResourcePolicy, ResourceProfile},
    routing::{
        RoutingAttemptRecord, RoutingConfigExport, RoutingPolicy, RoutingStats,
        RoutingTaskDefinition, TargetRoutingProfile,
    },
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub preset_id: String,
    pub auth_mode: AuthMode,
    pub base_url: String,
    pub enabled: bool,
    pub has_credential: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalModelMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_engine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_memory_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_overrides: Option<ResourceOverrides>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelTarget {
    pub id: String,
    pub provider_id: Option<String>,
    pub name: String,
    pub kind: TargetKind,
    pub provider_model: String,
    pub local_path: Option<String>,
    pub runtime_url: Option<String>,
    #[serde(default)]
    pub wire_protocol: WireProtocol,
    pub capabilities: Vec<String>,
    pub enabled: bool,
    pub state: String,
    pub size_bytes: Option<i64>,
    #[serde(flatten, default)]
    pub local: LocalModelMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallJob {
    pub id: String,
    pub repo_id: String,
    pub revision: String,
    pub status: String,
    pub catalog_id: Option<String>,
    pub alias: Option<String>,
    pub engine: Option<String>,
    pub task: Option<String>,
    pub capabilities: Vec<String>,
    pub bytes_downloaded: i64,
    pub bytes_total: Option<i64>,
    pub current_file: Option<String>,
    pub staging_dir: Option<String>,
    pub error: Option<String>,
    pub confirm_over_budget: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderModel {
    pub id: String,
    pub wire_protocol: WireProtocol,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalApiKey {
    pub id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLog {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub endpoint: String,
    pub alias: Option<String>,
    pub target: Option<String>,
    pub attempts: i64,
    pub status: i64,
    pub latency_ms: i64,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub error_code: Option<String>,
    pub api_key_id: Option<String>,
    pub api_key_name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogQuery {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub api_key_id: Option<String>,
    #[serde(default)]
    pub legacy_only: bool,
    pub alias: Option<String>,
    pub target: Option<String>,
    pub endpoint: Option<String>,
    pub status_class: Option<String>,
    pub query: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogResult {
    pub items: Vec<RequestLog>,
    pub total: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogFacets {
    pub aliases: Vec<String>,
    pub targets: Vec<String>,
    pub endpoints: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageSummary {
    pub request_count: i64,
    pub success_count: i64,
    pub average_latency_ms: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub unknown_usage_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageBucket {
    pub start: DateTime<Utc>,
    pub request_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyUsage {
    pub api_key_id: Option<String>,
    pub api_key_name: String,
    #[serde(flatten)]
    pub summary: UsageSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageData {
    #[serde(flatten)]
    pub summary: UsageSummary,
    pub buckets: Vec<UsageBucket>,
    pub by_key: Vec<KeyUsage>,
}

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
    was_existing: bool,
}

impl Store {
    pub async fn open(path: &Path) -> anyhow::Result<Self> {
        let was_existing = path.exists();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePool::connect_with(options).await?;
        let store = Self { pool, was_existing };
        store.migrate().await?;
        Ok(store)
    }

    pub async fn memory() -> anyhow::Result<Self> {
        let pool = SqlitePool::connect("sqlite::memory:").await?;
        let store = Self {
            pool,
            was_existing: false,
        };
        store.migrate().await?;
        Ok(store)
    }

    async fn migrate(&self) -> anyhow::Result<()> {
        for statement in [
            "CREATE TABLE IF NOT EXISTS providers (id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL DEFAULT 'cloud', base_url TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1, preset_id TEXT NOT NULL DEFAULT 'openai', auth_mode TEXT NOT NULL DEFAULT 'api_key')",
            "CREATE TABLE IF NOT EXISTS model_targets (id TEXT PRIMARY KEY, provider_id TEXT, name TEXT NOT NULL, kind TEXT NOT NULL, provider_model TEXT NOT NULL, local_path TEXT, runtime_url TEXT, capabilities TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1, state TEXT NOT NULL DEFAULT 'ready', size_bytes INTEGER, wire_protocol TEXT NOT NULL DEFAULT 'open_ai_chat', FOREIGN KEY(provider_id) REFERENCES providers(id) ON DELETE CASCADE)",
            "CREATE TABLE IF NOT EXISTS routes (alias TEXT PRIMARY KEY, enabled INTEGER NOT NULL DEFAULT 1, capabilities TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS route_targets (alias TEXT NOT NULL, target_id TEXT NOT NULL, priority INTEGER NOT NULL, PRIMARY KEY(alias, target_id), FOREIGN KEY(alias) REFERENCES routes(alias) ON DELETE CASCADE, FOREIGN KEY(target_id) REFERENCES model_targets(id) ON DELETE CASCADE)",
            "CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS provider_models (provider_id TEXT NOT NULL, model_id TEXT NOT NULL, synced_at TEXT NOT NULL, wire_protocol TEXT NOT NULL DEFAULT 'open_ai_chat', capabilities TEXT NOT NULL DEFAULT '[\"chat\",\"streaming\"]', PRIMARY KEY(provider_id, model_id), FOREIGN KEY(provider_id) REFERENCES providers(id) ON DELETE CASCADE)",
            "CREATE TABLE IF NOT EXISTS local_api_keys (id TEXT PRIMARY KEY, name TEXT NOT NULL, token_hash BLOB NOT NULL, created_at TEXT NOT NULL, last_used_at TEXT, revoked_at TEXT)",
            "CREATE TABLE IF NOT EXISTS request_logs (id TEXT PRIMARY KEY, created_at TEXT NOT NULL, endpoint TEXT NOT NULL, alias TEXT, target TEXT, attempts INTEGER NOT NULL, status INTEGER NOT NULL, latency_ms INTEGER NOT NULL, input_tokens INTEGER, output_tokens INTEGER, error_code TEXT)",
            "CREATE INDEX IF NOT EXISTS request_logs_created_idx ON request_logs(created_at DESC)",
            "CREATE INDEX IF NOT EXISTS local_api_keys_active_idx ON local_api_keys(revoked_at)",
            "CREATE TABLE IF NOT EXISTS routing_policies (alias TEXT PRIMARY KEY, policy TEXT NOT NULL, FOREIGN KEY(alias) REFERENCES routes(alias) ON DELETE CASCADE)",
            "CREATE TABLE IF NOT EXISTS target_routing_profiles (target_id TEXT PRIMARY KEY, profile TEXT NOT NULL, FOREIGN KEY(target_id) REFERENCES model_targets(id) ON DELETE CASCADE)",
            "CREATE TABLE IF NOT EXISTS routing_tasks (id TEXT PRIMARY KEY, definition TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS routing_attempts (id TEXT PRIMARY KEY, request_id TEXT NOT NULL, created_at TEXT NOT NULL, alias TEXT NOT NULL, task TEXT NOT NULL, task_source TEXT NOT NULL, target_id TEXT NOT NULL, routing_mode TEXT NOT NULL, status INTEGER NOT NULL, transient_failure INTEGER NOT NULL, retry_after_until TEXT, latency_ms INTEGER NOT NULL, ttft_ms INTEGER, streaming INTEGER NOT NULL, input_tokens INTEGER, output_tokens INTEGER, estimated_cost_usd REAL, cost_verified INTEGER NOT NULL, score TEXT, reason TEXT NOT NULL)",
            "CREATE INDEX IF NOT EXISTS routing_attempts_stats_idx ON routing_attempts(target_id,task,created_at DESC)",
            "CREATE INDEX IF NOT EXISTS routing_attempts_request_idx ON routing_attempts(request_id,created_at)",
            "CREATE TABLE IF NOT EXISTS routing_half_open_leases (target_id TEXT NOT NULL, task TEXT NOT NULL, lease_until TEXT NOT NULL, PRIMARY KEY(target_id,task))",
        ] {
            sqlx::query(statement).execute(&self.pool).await?;
        }
        let attempt_columns = sqlx::query("PRAGMA table_info(routing_attempts)")
            .fetch_all(&self.pool)
            .await?;
        if !attempt_columns
            .iter()
            .any(|column| column.get::<String, _>("name") == "retry_after_until")
        {
            sqlx::query("ALTER TABLE routing_attempts ADD COLUMN retry_after_until TEXT")
                .execute(&self.pool)
                .await?;
        }
        for column in ["input_tokens", "output_tokens"] {
            if !attempt_columns
                .iter()
                .any(|entry| entry.get::<String, _>("name") == column)
            {
                sqlx::query(&format!(
                    "ALTER TABLE routing_attempts ADD COLUMN {column} INTEGER"
                ))
                .execute(&self.pool)
                .await?;
            }
        }
        let log_columns = sqlx::query("PRAGMA table_info(request_logs)")
            .fetch_all(&self.pool)
            .await?;
        if !log_columns
            .iter()
            .any(|column| column.get::<String, _>("name") == "api_key_id")
        {
            sqlx::query("ALTER TABLE request_logs ADD COLUMN api_key_id TEXT")
                .execute(&self.pool)
                .await?;
        }
        let provider_columns = sqlx::query("PRAGMA table_info(providers)")
            .fetch_all(&self.pool)
            .await?;
        if !provider_columns
            .iter()
            .any(|column| column.get::<String, _>("name") == "preset_id")
        {
            sqlx::query(
                "ALTER TABLE providers ADD COLUMN preset_id TEXT NOT NULL DEFAULT 'openai'",
            )
            .execute(&self.pool)
            .await?;
            sqlx::query("UPDATE providers SET preset_id=CASE WHEN kind='open_router' THEN 'openrouter' ELSE 'openai' END").execute(&self.pool).await?;
        }
        sqlx::query("UPDATE providers SET preset_id='openrouter' WHERE kind='open_router' AND preset_id='openai'").execute(&self.pool).await?;
        if !provider_columns
            .iter()
            .any(|column| column.get::<String, _>("name") == "auth_mode")
        {
            sqlx::query(
                "ALTER TABLE providers ADD COLUMN auth_mode TEXT NOT NULL DEFAULT 'api_key'",
            )
            .execute(&self.pool)
            .await?;
        }
        let target_columns = sqlx::query("PRAGMA table_info(model_targets)")
            .fetch_all(&self.pool)
            .await?;
        if !target_columns
            .iter()
            .any(|column| column.get::<String, _>("name") == "wire_protocol")
        {
            sqlx::query("ALTER TABLE model_targets ADD COLUMN wire_protocol TEXT NOT NULL DEFAULT 'open_ai_chat'").execute(&self.pool).await?;
        }
        let model_columns = sqlx::query("PRAGMA table_info(provider_models)")
            .fetch_all(&self.pool)
            .await?;
        if !model_columns
            .iter()
            .any(|column| column.get::<String, _>("name") == "wire_protocol")
        {
            sqlx::query("ALTER TABLE provider_models ADD COLUMN wire_protocol TEXT NOT NULL DEFAULT 'open_ai_chat'").execute(&self.pool).await?;
        }
        if !model_columns
            .iter()
            .any(|column| column.get::<String, _>("name") == "capabilities")
        {
            sqlx::query("ALTER TABLE provider_models ADD COLUMN capabilities TEXT NOT NULL DEFAULT '[\"chat\",\"streaming\"]'").execute(&self.pool).await?;
        }
        for (name, definition) in [
            ("task", "TEXT"),
            ("runtime_engine", "TEXT"),
            ("source_repo", "TEXT"),
            ("source_revision", "TEXT"),
            ("estimated_memory_bytes", "INTEGER"),
            ("catalog_id", "TEXT"),
            ("trust_status", "TEXT"),
            ("resource_overrides", "TEXT"),
        ] {
            if !target_columns
                .iter()
                .any(|column| column.get::<String, _>("name") == name)
            {
                sqlx::query(&format!(
                    "ALTER TABLE model_targets ADD COLUMN {name} {definition}"
                ))
                .execute(&self.pool)
                .await?;
            }
        }
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS install_jobs (
                id TEXT PRIMARY KEY,
                repo_id TEXT NOT NULL,
                revision TEXT NOT NULL,
                status TEXT NOT NULL,
                catalog_id TEXT,
                alias TEXT,
                engine TEXT,
                task TEXT,
                capabilities TEXT NOT NULL DEFAULT '[]',
                bytes_downloaded INTEGER NOT NULL DEFAULT 0,
                bytes_total INTEGER,
                current_file TEXT,
                staging_dir TEXT,
                error TEXT,
                confirm_over_budget INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS request_logs_api_key_idx ON request_logs(api_key_id, created_at DESC)")
            .execute(&self.pool)
            .await?;
        self.set_default("port", "11435").await?;
        self.set_default("memory_budget_percent", "70").await?;
        self.set_default("idle_unload_minutes", "15").await?;
        self.set_default("log_retention_days", "30").await?;
        if self.setting("resource_policy_v1").await?.is_none() {
            let logical_cpus = crate::resource::host_performance_cpu_count();
            let policy = if self.was_existing {
                let memory = self
                    .setting("memory_budget_percent")
                    .await?
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(70);
                let idle = self
                    .setting("idle_unload_minutes")
                    .await?
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(15);
                ResourcePolicy::migrated(memory, idle, logical_cpus)
            } else {
                ResourcePolicy::preset(ResourceProfile::Stealth, logical_cpus)
            };
            self.set_resource_policy(&policy).await?;
        }
        Ok(())
    }

    async fn set_default(&self, key: &str, value: &str) -> anyhow::Result<()> {
        sqlx::query("INSERT OR IGNORE INTO settings(key, value) VALUES(?, ?)")
            .bind(key)
            .bind(value)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn setting(&self, key: &str) -> anyhow::Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
                .bind(key)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    pub async fn set_setting(&self, key: &str, value: &str) -> anyhow::Result<()> {
        sqlx::query("INSERT INTO settings(key, value) VALUES(?, ?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .bind(key).bind(value).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn resource_policy(&self, logical_cpus: usize) -> anyhow::Result<ResourcePolicy> {
        if let Some(value) = self.setting("resource_policy_v1").await? {
            let policy: ResourcePolicy =
                serde_json::from_str(&value).context("invalid resource policy")?;
            policy.validate()?;
            return Ok(policy);
        }
        Ok(ResourcePolicy::preset(
            ResourceProfile::Stealth,
            logical_cpus,
        ))
    }

    pub async fn set_resource_policy(&self, policy: &ResourcePolicy) -> anyhow::Result<()> {
        policy.validate()?;
        self.set_setting("resource_policy_v1", &serde_json::to_string(policy)?)
            .await
    }

    pub async fn providers(&self) -> anyhow::Result<Vec<Provider>> {
        let rows = sqlx::query(
            "SELECT id, name, preset_id, auth_mode, base_url, enabled FROM providers ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(Provider {
                    id: row.get("id"),
                    name: row.get("name"),
                    preset_id: row.get("preset_id"),
                    auth_mode: decode_auth_mode(row.get::<String, _>("auth_mode").as_str())?,
                    base_url: row.get("base_url"),
                    enabled: row.get::<i64, _>("enabled") != 0,
                    has_credential: false,
                })
            })
            .collect()
    }

    pub async fn provider(&self, id: &str) -> anyhow::Result<Option<Provider>> {
        let row = sqlx::query(
            "SELECT id, name, preset_id, auth_mode, base_url, enabled FROM providers WHERE id=?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(Provider {
                id: row.get("id"),
                name: row.get("name"),
                preset_id: row.get("preset_id"),
                auth_mode: decode_auth_mode(row.get::<String, _>("auth_mode").as_str())?,
                base_url: row.get("base_url"),
                enabled: row.get::<i64, _>("enabled") != 0,
                has_credential: false,
            })
        })
        .transpose()
    }

    pub async fn upsert_provider(&self, provider: &Provider) -> anyhow::Result<()> {
        sqlx::query("INSERT INTO providers(id,name,kind,base_url,enabled,preset_id,auth_mode) VALUES(?,?,'cloud',?,?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,base_url=excluded.base_url,enabled=excluded.enabled,preset_id=excluded.preset_id,auth_mode=excluded.auth_mode")
            .bind(&provider.id).bind(&provider.name).bind(&provider.base_url).bind(provider.enabled as i64).bind(&provider.preset_id).bind(encode_auth_mode(provider.auth_mode))
            .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn delete_provider(&self, id: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM providers WHERE id=?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn replace_provider_models(
        &self,
        provider_id: &str,
        models: &[ProviderModel],
    ) -> anyhow::Result<()> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("DELETE FROM provider_models WHERE provider_id=?")
            .bind(provider_id)
            .execute(&mut *transaction)
            .await?;
        let synced_at = Utc::now().to_rfc3339();
        for model in models {
            sqlx::query(
                "INSERT INTO provider_models(provider_id, model_id, synced_at,wire_protocol,capabilities) VALUES(?,?,?,?,?)",
            )
            .bind(provider_id)
            .bind(&model.id)
            .bind(&synced_at)
            .bind(encode_wire_protocol(model.wire_protocol))
            .bind(serde_json::to_string(&model.capabilities)?)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn provider_models(&self, provider_id: &str) -> anyhow::Result<Vec<ProviderModel>> {
        let rows = sqlx::query(
            "SELECT model_id,wire_protocol,capabilities FROM provider_models WHERE provider_id=? ORDER BY model_id",
        )
        .bind(provider_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(ProviderModel {
                    id: row.get("model_id"),
                    wire_protocol: decode_wire_protocol(
                        row.get::<String, _>("wire_protocol").as_str(),
                    )?,
                    capabilities: serde_json::from_str(
                        row.get::<String, _>("capabilities").as_str(),
                    )?,
                })
            })
            .collect()
    }

    pub async fn targets(&self) -> anyhow::Result<Vec<ModelTarget>> {
        let rows = sqlx::query("SELECT * FROM model_targets ORDER BY name")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(row_to_target).collect()
    }

    pub async fn target(&self, id: &str) -> anyhow::Result<Option<ModelTarget>> {
        sqlx::query("SELECT * FROM model_targets WHERE id=?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .map(row_to_target)
            .transpose()
    }

    pub async fn upsert_target(&self, target: &ModelTarget) -> anyhow::Result<()> {
        let capabilities = serde_json::to_string(&target.capabilities)?;
        let resource_overrides = target
            .local
            .resource_overrides
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        sqlx::query("INSERT INTO model_targets(id,provider_id,name,kind,provider_model,local_path,runtime_url,capabilities,enabled,state,size_bytes,wire_protocol,task,runtime_engine,source_repo,source_revision,estimated_memory_bytes,catalog_id,trust_status,resource_overrides) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET provider_id=excluded.provider_id,name=excluded.name,kind=excluded.kind,provider_model=excluded.provider_model,local_path=excluded.local_path,runtime_url=excluded.runtime_url,capabilities=excluded.capabilities,enabled=excluded.enabled,state=excluded.state,size_bytes=excluded.size_bytes,wire_protocol=excluded.wire_protocol,task=excluded.task,runtime_engine=excluded.runtime_engine,source_repo=excluded.source_repo,source_revision=excluded.source_revision,estimated_memory_bytes=excluded.estimated_memory_bytes,catalog_id=excluded.catalog_id,trust_status=excluded.trust_status,resource_overrides=excluded.resource_overrides")
            .bind(&target.id).bind(&target.provider_id).bind(&target.name).bind(encode_kind(&target.kind)).bind(&target.provider_model)
            .bind(&target.local_path).bind(&target.runtime_url).bind(capabilities).bind(target.enabled as i64).bind(&target.state).bind(target.size_bytes).bind(encode_wire_protocol(target.wire_protocol))
            .bind(&target.local.task).bind(&target.local.runtime_engine).bind(&target.local.source_repo).bind(&target.local.source_revision).bind(target.local.estimated_memory_bytes).bind(&target.local.catalog_id).bind(&target.local.trust_status).bind(resource_overrides)
            .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn target_by_source(
        &self,
        repo: &str,
        revision: &str,
    ) -> anyhow::Result<Option<ModelTarget>> {
        sqlx::query("SELECT * FROM model_targets WHERE source_repo=? AND source_revision=?")
            .bind(repo)
            .bind(revision)
            .fetch_optional(&self.pool)
            .await?
            .map(row_to_target)
            .transpose()
    }

    pub async fn aliases(&self) -> anyhow::Result<Vec<String>> {
        Ok(
            sqlx::query_scalar("SELECT alias FROM routes ORDER BY alias")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    pub async fn delete_target(&self, id: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM model_targets WHERE id=?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn reset_local_runtime_states(&self) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE model_targets SET runtime_url=NULL,state='stopped' WHERE kind IN ('gguf','mlx')",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn routes(&self) -> anyhow::Result<Vec<ModelRoute>> {
        let rows = sqlx::query("SELECT alias,enabled,capabilities FROM routes ORDER BY alias")
            .fetch_all(&self.pool)
            .await?;
        let mut routes = Vec::new();
        for row in rows {
            let alias: String = row.get("alias");
            let target_rows = sqlx::query("SELECT mt.id,mt.kind,mt.provider_model,mt.enabled,rt.priority FROM route_targets rt JOIN model_targets mt ON mt.id=rt.target_id WHERE rt.alias=? ORDER BY rt.priority")
                .bind(&alias).fetch_all(&self.pool).await?;
            let targets = target_rows
                .into_iter()
                .map(|target| {
                    Ok(RouteTarget {
                        id: target.get("id"),
                        kind: decode_kind(target.get::<String, _>("kind").as_str())?,
                        model: target.get("provider_model"),
                        priority: target.get("priority"),
                        enabled: target.get::<i64, _>("enabled") != 0,
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            routes.push(ModelRoute {
                alias,
                enabled: row.get::<i64, _>("enabled") != 0,
                capabilities: serde_json::from_str(row.get::<String, _>("capabilities").as_str())?,
                targets,
            });
        }
        Ok(routes)
    }

    pub async fn route(&self, alias: &str) -> anyhow::Result<Option<ModelRoute>> {
        Ok(self
            .routes()
            .await?
            .into_iter()
            .find(|route| route.alias == alias))
    }

    pub async fn upsert_route(&self, route: &ModelRoute) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("INSERT INTO routes(alias,enabled,capabilities) VALUES(?,?,?) ON CONFLICT(alias) DO UPDATE SET enabled=excluded.enabled,capabilities=excluded.capabilities")
            .bind(&route.alias).bind(route.enabled as i64).bind(serde_json::to_string(&route.capabilities)?).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM route_targets WHERE alias=?")
            .bind(&route.alias)
            .execute(&mut *tx)
            .await?;
        for target in &route.targets {
            sqlx::query("INSERT INTO route_targets(alias,target_id,priority) VALUES(?,?,?)")
                .bind(&route.alias)
                .bind(&target.id)
                .bind(target.priority)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn delete_route(&self, alias: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM routes WHERE alias=?")
            .bind(alias)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn routing_policies(&self) -> anyhow::Result<Vec<RoutingPolicy>> {
        let rows = sqlx::query("SELECT policy FROM routing_policies ORDER BY alias")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                serde_json::from_str::<RoutingPolicy>(&row.get::<String, _>("policy"))
                    .context("invalid routing policy")
            })
            .collect()
    }

    pub async fn routing_policy(&self, alias: &str) -> anyhow::Result<Option<RoutingPolicy>> {
        let row = sqlx::query("SELECT policy FROM routing_policies WHERE alias=?")
            .bind(alias)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| {
            serde_json::from_str::<RoutingPolicy>(&row.get::<String, _>("policy"))
                .context("invalid routing policy")
        })
        .transpose()
    }

    pub async fn upsert_routing_policy(&self, policy: &RoutingPolicy) -> anyhow::Result<()> {
        sqlx::query("INSERT INTO routing_policies(alias,policy) VALUES(?,?) ON CONFLICT(alias) DO UPDATE SET policy=excluded.policy")
            .bind(&policy.alias).bind(serde_json::to_string(policy)?)
            .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn target_routing_profiles(&self) -> anyhow::Result<Vec<TargetRoutingProfile>> {
        let rows = sqlx::query("SELECT profile FROM target_routing_profiles ORDER BY target_id")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                serde_json::from_str::<TargetRoutingProfile>(&row.get::<String, _>("profile"))
                    .context("invalid target routing profile")
            })
            .collect()
    }

    pub async fn target_routing_profile(
        &self,
        target_id: &str,
    ) -> anyhow::Result<Option<TargetRoutingProfile>> {
        let row = sqlx::query("SELECT profile FROM target_routing_profiles WHERE target_id=?")
            .bind(target_id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| {
            serde_json::from_str::<TargetRoutingProfile>(&row.get::<String, _>("profile"))
                .context("invalid target routing profile")
        })
        .transpose()
    }

    pub async fn upsert_target_routing_profile(
        &self,
        profile: &TargetRoutingProfile,
    ) -> anyhow::Result<()> {
        sqlx::query("INSERT INTO target_routing_profiles(target_id,profile) VALUES(?,?) ON CONFLICT(target_id) DO UPDATE SET profile=excluded.profile")
            .bind(&profile.target_id).bind(serde_json::to_string(profile)?)
            .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn custom_routing_tasks(&self) -> anyhow::Result<Vec<RoutingTaskDefinition>> {
        let rows = sqlx::query("SELECT definition FROM routing_tasks ORDER BY id")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                serde_json::from_str::<RoutingTaskDefinition>(&row.get::<String, _>("definition"))
                    .context("invalid routing task")
            })
            .collect()
    }

    pub async fn upsert_routing_task(&self, task: &RoutingTaskDefinition) -> anyhow::Result<()> {
        sqlx::query("INSERT INTO routing_tasks(id,definition) VALUES(?,?) ON CONFLICT(id) DO UPDATE SET definition=excluded.definition")
            .bind(&task.id).bind(serde_json::to_string(task)?)
            .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn delete_routing_task(&self, id: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM routing_tasks WHERE id=?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn insert_routing_attempt(
        &self,
        attempt: &RoutingAttemptRecord,
    ) -> anyhow::Result<()> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("INSERT INTO routing_attempts(id,request_id,created_at,alias,task,task_source,target_id,routing_mode,status,transient_failure,retry_after_until,latency_ms,ttft_ms,streaming,input_tokens,output_tokens,estimated_cost_usd,cost_verified,score,reason) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
            .bind(&attempt.id).bind(&attempt.request_id).bind(attempt.created_at.to_rfc3339()).bind(&attempt.alias).bind(&attempt.task)
            .bind(&attempt.task_source).bind(&attempt.target_id).bind(&attempt.routing_mode).bind(attempt.status as i64)
            .bind(attempt.transient_failure as i64).bind(attempt.retry_after_until.map(|value| value.to_rfc3339())).bind(attempt.latency_ms as i64).bind(attempt.ttft_ms.map(|value| value as i64))
            .bind(attempt.streaming as i64).bind(attempt.input_tokens.map(|value| value as i64)).bind(attempt.output_tokens.map(|value| value as i64)).bind(attempt.estimated_cost_usd).bind(attempt.cost_verified as i64)
            .bind(attempt.score.as_ref().map(serde_json::to_string).transpose()?).bind(&attempt.reason)
            .execute(&mut *transaction).await?;
        sqlx::query("DELETE FROM routing_half_open_leases WHERE target_id=? AND task=?")
            .bind(&attempt.target_id)
            .bind(&attempt.task)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn claim_half_open(&self, target_id: &str, task: &str) -> anyhow::Result<bool> {
        let now = Utc::now().to_rfc3339();
        let lease_until = (Utc::now() + chrono::Duration::seconds(120)).to_rfc3339();
        let result = sqlx::query("INSERT INTO routing_half_open_leases(target_id,task,lease_until) VALUES(?,?,?) ON CONFLICT(target_id,task) DO UPDATE SET lease_until=excluded.lease_until WHERE routing_half_open_leases.lease_until<=?")
            .bind(target_id).bind(task).bind(lease_until).bind(now).execute(&self.pool).await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn routing_attempts(
        &self,
        request_id: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Vec<RoutingAttemptRecord>> {
        let rows = if let Some(request_id) = request_id {
            sqlx::query("SELECT * FROM routing_attempts WHERE request_id=? ORDER BY created_at,target_id LIMIT ?")
                .bind(request_id).bind(limit.clamp(1, 500)).fetch_all(&self.pool).await?
        } else {
            sqlx::query("SELECT * FROM routing_attempts ORDER BY created_at DESC LIMIT ?")
                .bind(limit.clamp(1, 500))
                .fetch_all(&self.pool)
                .await?
        };
        rows.into_iter().map(row_to_routing_attempt).collect()
    }

    pub async fn routing_stats(
        &self,
        target_id: &str,
        task: &str,
        streaming: bool,
    ) -> anyhow::Result<RoutingStats> {
        let since = (Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        let rows = sqlx::query("SELECT created_at,status,transient_failure,retry_after_until,latency_ms,ttft_ms FROM routing_attempts WHERE target_id=? AND task=? AND streaming=? AND created_at>=? ORDER BY created_at DESC LIMIT 100")
            .bind(target_id).bind(task).bind(streaming as i64).bind(since).fetch_all(&self.pool).await?;
        if rows.is_empty() {
            return Ok(RoutingStats::default());
        }
        let mut latencies = rows
            .iter()
            .filter(|row| row.get::<i64, _>("status") < 400)
            .map(|row| {
                if streaming {
                    row.get::<Option<i64>, _>("ttft_ms")
                        .unwrap_or_else(|| row.get("latency_ms"))
                } else {
                    row.get("latency_ms")
                }
            })
            .collect::<Vec<i64>>();
        latencies.sort_unstable();
        let p90 = if latencies.is_empty() {
            0
        } else {
            latencies[((latencies.len() - 1) * 9) / 10].max(0) as u64
        };
        let success = rows
            .iter()
            .filter(|row| row.get::<i64, _>("status") < 400)
            .count() as f64;
        let eligible_samples = rows
            .iter()
            .filter(|row| {
                row.get::<i64, _>("status") < 400 || row.get::<i64, _>("transient_failure") != 0
            })
            .count();
        let reliability = (success + 19.0) / (eligible_samples as f64 + 20.0);
        // A successful probe resets the breaker. Other 4xx responses are neutral:
        // they neither open nor reset it. Keep the failure epoch across cooldowns so
        // repeated half-open failures can increase the backoff beyond 60 seconds.
        let failure_epoch = rows
            .iter()
            .take_while(|row| row.get::<i64, _>("status") >= 400)
            .filter(|row| row.get::<i64, _>("transient_failure") != 0)
            .filter_map(|row| {
                DateTime::parse_from_rfc3339(&row.get::<String, _>("created_at"))
                    .ok()
                    .map(|value| value.with_timezone(&Utc))
            })
            .collect::<Vec<_>>();
        let breaker_tripped = failure_epoch.windows(3).any(|window| {
            window[0].signed_duration_since(window[2]) <= chrono::Duration::seconds(60)
        });
        let retry_after_open = rows
            .iter()
            .filter(|row| row.get::<i64, _>("transient_failure") != 0)
            .filter_map(|row| row.get::<Option<String>, _>("retry_after_until"))
            .filter_map(|value| DateTime::parse_from_rfc3339(&value).ok())
            .any(|value| value.with_timezone(&Utc) > Utc::now());
        let circuit_open = retry_after_open
            || if breaker_tripped {
                let exponent = failure_epoch.len().saturating_sub(3).min(4) as u32;
                let cooldown = 30_i64.saturating_mul(2_i64.pow(exponent)).min(300);
                failure_epoch
                    .first()
                    .is_some_and(|value| Utc::now() < *value + chrono::Duration::seconds(cooldown))
            } else {
                false
            };
        Ok(RoutingStats {
            samples: eligible_samples,
            p90_latency_ms: p90,
            reliability,
            circuit_open,
            half_open_required: breaker_tripped && !circuit_open,
        })
    }

    pub async fn import_routing_config(&self, config: &RoutingConfigExport) -> anyhow::Result<()> {
        let mut transaction = self.pool.begin().await?;
        for task in &config.tasks {
            sqlx::query("INSERT INTO routing_tasks(id,definition) VALUES(?,?) ON CONFLICT(id) DO UPDATE SET definition=excluded.definition")
                .bind(&task.id).bind(serde_json::to_string(task)?).execute(&mut *transaction).await?;
        }
        for profile in &config.profiles {
            sqlx::query("INSERT INTO target_routing_profiles(target_id,profile) VALUES(?,?) ON CONFLICT(target_id) DO UPDATE SET profile=excluded.profile")
                .bind(&profile.target_id).bind(serde_json::to_string(profile)?).execute(&mut *transaction).await?;
        }
        for policy in &config.policies {
            sqlx::query("INSERT INTO routing_policies(alias,policy) VALUES(?,?) ON CONFLICT(alias) DO UPDATE SET policy=excluded.policy")
                .bind(&policy.alias).bind(serde_json::to_string(policy)?).execute(&mut *transaction).await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn local_api_keys(&self) -> anyhow::Result<Vec<LocalApiKey>> {
        let rows = sqlx::query("SELECT id,name,created_at,last_used_at,revoked_at FROM local_api_keys ORDER BY created_at, name")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(row_to_local_api_key).collect()
    }

    pub async fn local_api_key(&self, id: &str) -> anyhow::Result<Option<LocalApiKey>> {
        sqlx::query(
            "SELECT id,name,created_at,last_used_at,revoked_at FROM local_api_keys WHERE id=?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .map(row_to_local_api_key)
        .transpose()
    }

    pub async fn insert_local_api_key(
        &self,
        key: &LocalApiKey,
        token_hash: &[u8],
    ) -> anyhow::Result<()> {
        sqlx::query("INSERT INTO local_api_keys(id,name,token_hash,created_at,last_used_at,revoked_at) VALUES(?,?,?,?,?,?)")
            .bind(&key.id)
            .bind(&key.name)
            .bind(token_hash)
            .bind(key.created_at.to_rfc3339())
            .bind(key.last_used_at.map(|value| value.to_rfc3339()))
            .bind(key.revoked_at.map(|value| value.to_rfc3339()))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn active_local_api_key_hashes(&self) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
        let rows = sqlx::query(
            "SELECT id,token_hash FROM local_api_keys WHERE revoked_at IS NULL ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| (row.get("id"), row.get("token_hash")))
            .collect())
    }

    pub async fn rename_local_api_key(&self, id: &str, name: &str) -> anyhow::Result<bool> {
        Ok(sqlx::query("UPDATE local_api_keys SET name=? WHERE id=?")
            .bind(name)
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected()
            == 1)
    }

    pub async fn rotate_local_api_key(&self, id: &str, token_hash: &[u8]) -> anyhow::Result<bool> {
        Ok(
            sqlx::query("UPDATE local_api_keys SET token_hash=?, revoked_at=NULL WHERE id=?")
                .bind(token_hash)
                .bind(id)
                .execute(&self.pool)
                .await?
                .rows_affected()
                == 1,
        )
    }

    pub async fn revoke_local_api_key(&self, id: &str) -> anyhow::Result<bool> {
        Ok(
            sqlx::query("UPDATE local_api_keys SET revoked_at=? WHERE id=? AND revoked_at IS NULL")
                .bind(Utc::now().to_rfc3339())
                .bind(id)
                .execute(&self.pool)
                .await?
                .rows_affected()
                == 1,
        )
    }

    pub async fn touch_local_api_key(&self, id: &str) -> anyhow::Result<()> {
        sqlx::query("UPDATE local_api_keys SET last_used_at=? WHERE id=?")
            .bind(Utc::now().to_rfc3339())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn insert_log(&self, log: &RequestLog) -> anyhow::Result<()> {
        sqlx::query("INSERT INTO request_logs(id,created_at,endpoint,alias,target,attempts,status,latency_ms,input_tokens,output_tokens,error_code,api_key_id) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)")
            .bind(&log.id).bind(log.created_at.to_rfc3339()).bind(&log.endpoint).bind(&log.alias).bind(&log.target)
            .bind(log.attempts).bind(log.status).bind(log.latency_ms).bind(log.input_tokens).bind(log.output_tokens).bind(&log.error_code).bind(&log.api_key_id)
            .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn update_log_usage(
        &self,
        id: &str,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
    ) -> anyhow::Result<()> {
        sqlx::query("UPDATE request_logs SET input_tokens=?,output_tokens=? WHERE id=?")
            .bind(input_tokens)
            .bind(output_tokens)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn logs(&self, limit: i64) -> anyhow::Result<Vec<RequestLog>> {
        let rows = sqlx::query("SELECT l.*,k.name AS api_key_name FROM request_logs l LEFT JOIN local_api_keys k ON k.id=l.api_key_id ORDER BY l.created_at DESC LIMIT ?")
            .bind(limit.clamp(1, 1000))
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(row_to_request_log).collect()
    }

    pub async fn query_logs(&self, query: &LogQuery) -> anyhow::Result<LogResult> {
        let mut count = QueryBuilder::<Sqlite>::new(
            "SELECT COUNT(*) FROM request_logs l LEFT JOIN local_api_keys k ON k.id=l.api_key_id",
        );
        push_log_filters(&mut count, query);
        let total: i64 = count.build_query_scalar().fetch_one(&self.pool).await?;

        let mut select = QueryBuilder::<Sqlite>::new(
            "SELECT l.*,k.name AS api_key_name FROM request_logs l LEFT JOIN local_api_keys k ON k.id=l.api_key_id",
        );
        push_log_filters(&mut select, query);
        select
            .push(" ORDER BY l.created_at DESC LIMIT ")
            .push_bind(query.limit.unwrap_or(100).clamp(1, 500))
            .push(" OFFSET ")
            .push_bind(query.offset.unwrap_or(0).max(0));
        let rows = select.build().fetch_all(&self.pool).await?;
        Ok(LogResult {
            items: rows
                .into_iter()
                .map(row_to_request_log)
                .collect::<anyhow::Result<_>>()?,
            total,
        })
    }

    pub async fn log_facets(&self) -> anyhow::Result<LogFacets> {
        Ok(LogFacets {
            aliases: sqlx::query_scalar(
                "SELECT DISTINCT alias FROM request_logs WHERE alias IS NOT NULL ORDER BY alias",
            )
            .fetch_all(&self.pool)
            .await?,
            targets: sqlx::query_scalar(
                "SELECT DISTINCT target FROM request_logs WHERE target IS NOT NULL ORDER BY target",
            )
            .fetch_all(&self.pool)
            .await?,
            endpoints: sqlx::query_scalar(
                "SELECT DISTINCT endpoint FROM request_logs ORDER BY endpoint",
            )
            .fetch_all(&self.pool)
            .await?,
        })
    }

    pub async fn usage(&self, period: &str) -> anyhow::Result<UsageData> {
        self.usage_at(period, Utc::now()).await
    }

    async fn usage_at(&self, period: &str, now: DateTime<Utc>) -> anyhow::Result<UsageData> {
        let (cutoff, hourly) = match period {
            "24h" => (Some(now - chrono::Duration::hours(24)), true),
            "7d" => (Some(now - chrono::Duration::days(7)), false),
            "30d" => (Some(now - chrono::Duration::days(30)), false),
            "all" => (None, false),
            _ => anyhow::bail!("usage period must be 24h, 7d, 30d or all"),
        };

        let metrics = "COUNT(*) AS request_count, COALESCE(SUM(CASE WHEN l.status >= 200 AND l.status < 300 THEN 1 ELSE 0 END),0) AS success_count, COALESCE(AVG(l.latency_ms),0.0) AS average_latency_ms, COALESCE(SUM(l.input_tokens),0) AS input_tokens, COALESCE(SUM(l.output_tokens),0) AS output_tokens, COALESCE(SUM(CASE WHEN l.input_tokens IS NULL OR l.output_tokens IS NULL THEN 1 ELSE 0 END),0) AS unknown_usage_count";
        let mut summary_query =
            QueryBuilder::<Sqlite>::new(format!("SELECT {metrics} FROM request_logs l"));
        push_usage_cutoff(&mut summary_query, cutoff);
        let summary = usage_summary_from_row(summary_query.build().fetch_one(&self.pool).await?);

        let mut by_key_query = QueryBuilder::<Sqlite>::new(format!(
            "SELECT l.api_key_id,COALESCE(k.name,'Unknown / Legacy') AS api_key_name,{metrics} FROM request_logs l LEFT JOIN local_api_keys k ON k.id=l.api_key_id"
        ));
        push_usage_cutoff(&mut by_key_query, cutoff);
        by_key_query.push(" GROUP BY l.api_key_id,k.name ORDER BY request_count DESC,api_key_name");
        let by_key = by_key_query
            .build()
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(|row| KeyUsage {
                api_key_id: row.get("api_key_id"),
                api_key_name: row.get("api_key_name"),
                summary: usage_summary_from_row(row),
            })
            .collect();

        let bucket_expression = if hourly {
            "strftime('%Y-%m-%dT%H:00:00Z',l.created_at)"
        } else {
            "strftime('%Y-%m-%dT00:00:00Z',l.created_at)"
        };
        let mut bucket_query = QueryBuilder::<Sqlite>::new(format!(
            "SELECT {bucket_expression} AS bucket_start,COUNT(*) AS request_count,COALESCE(SUM(l.input_tokens),0) AS input_tokens,COALESCE(SUM(l.output_tokens),0) AS output_tokens FROM request_logs l"
        ));
        push_usage_cutoff(&mut bucket_query, cutoff);
        bucket_query.push(" GROUP BY bucket_start ORDER BY bucket_start");
        let buckets = bucket_query
            .build()
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(|row| {
                Ok(UsageBucket {
                    start: parse_timestamp(row.get("bucket_start"))?,
                    request_count: row.get("request_count"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                })
            })
            .collect::<anyhow::Result<_>>()?;

        Ok(UsageData {
            summary,
            buckets,
            by_key,
        })
    }

    pub async fn clear_logs(&self) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM request_logs")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn purge_old_logs(&self, retention_days: i64) -> anyhow::Result<u64> {
        let cutoff = Utc::now() - chrono::Duration::days(retention_days.max(1));
        Ok(sqlx::query("DELETE FROM request_logs WHERE created_at < ?")
            .bind(cutoff.to_rfc3339())
            .execute(&self.pool)
            .await?
            .rows_affected())
    }

    pub async fn install_jobs(&self) -> anyhow::Result<Vec<InstallJob>> {
        let rows = sqlx::query("SELECT * FROM install_jobs ORDER BY created_at DESC")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(row_to_install_job).collect()
    }

    pub async fn install_job(&self, id: &str) -> anyhow::Result<Option<InstallJob>> {
        sqlx::query("SELECT * FROM install_jobs WHERE id=?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .map(row_to_install_job)
            .transpose()
    }

    pub async fn active_install_job(
        &self,
        repo_id: &str,
        revision: &str,
    ) -> anyhow::Result<Option<InstallJob>> {
        sqlx::query("SELECT * FROM install_jobs WHERE repo_id=? AND revision=? AND status IN ('queued','downloading','paused','validating','interrupted') ORDER BY created_at DESC LIMIT 1")
            .bind(repo_id)
            .bind(revision)
            .fetch_optional(&self.pool)
            .await?
            .map(row_to_install_job)
            .transpose()
    }

    pub async fn upsert_install_job(&self, job: &InstallJob) -> anyhow::Result<()> {
        sqlx::query("INSERT INTO install_jobs(id,repo_id,revision,status,catalog_id,alias,engine,task,capabilities,bytes_downloaded,bytes_total,current_file,staging_dir,error,confirm_over_budget,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET status=excluded.status,alias=excluded.alias,bytes_downloaded=excluded.bytes_downloaded,bytes_total=excluded.bytes_total,current_file=excluded.current_file,staging_dir=excluded.staging_dir,error=excluded.error,updated_at=excluded.updated_at")
            .bind(&job.id)
            .bind(&job.repo_id)
            .bind(&job.revision)
            .bind(&job.status)
            .bind(&job.catalog_id)
            .bind(&job.alias)
            .bind(&job.engine)
            .bind(&job.task)
            .bind(serde_json::to_string(&job.capabilities)?)
            .bind(job.bytes_downloaded)
            .bind(job.bytes_total)
            .bind(&job.current_file)
            .bind(&job.staging_dir)
            .bind(&job.error)
            .bind(job.confirm_over_budget as i64)
            .bind(job.created_at.to_rfc3339())
            .bind(job.updated_at.to_rfc3339())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_install_job(&self, id: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM install_jobs WHERE id=?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn interrupt_active_install_jobs(&self) -> anyhow::Result<()> {
        sqlx::query("UPDATE install_jobs SET status='interrupted', updated_at=? WHERE status IN ('queued','downloading','validating')")
            .bind(Utc::now().to_rfc3339())
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

fn push_log_filters(builder: &mut QueryBuilder<'_, Sqlite>, query: &LogQuery) {
    builder.push(" WHERE 1=1");
    if let Some(from) = query.from {
        builder
            .push(" AND l.created_at >= ")
            .push_bind(from.to_rfc3339());
    }
    if let Some(to) = query.to {
        builder
            .push(" AND l.created_at <= ")
            .push_bind(to.to_rfc3339());
    }
    if query.legacy_only {
        builder.push(" AND l.api_key_id IS NULL");
    } else if let Some(api_key_id) = query
        .api_key_id
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        builder
            .push(" AND l.api_key_id = ")
            .push_bind(api_key_id.to_owned());
    }
    if let Some(alias) = query.alias.as_deref().filter(|value| !value.is_empty()) {
        builder.push(" AND l.alias = ").push_bind(alias.to_owned());
    }
    if let Some(target) = query.target.as_deref().filter(|value| !value.is_empty()) {
        builder
            .push(" AND l.target = ")
            .push_bind(target.to_owned());
    }
    if let Some(endpoint) = query.endpoint.as_deref().filter(|value| !value.is_empty()) {
        builder
            .push(" AND l.endpoint = ")
            .push_bind(endpoint.to_owned());
    }
    match query.status_class.as_deref() {
        Some("success") => builder.push(" AND l.status >= 200 AND l.status < 300"),
        Some("4xx") => builder.push(" AND l.status >= 400 AND l.status < 500"),
        Some("5xx") => builder.push(" AND l.status >= 500 AND l.status < 600"),
        _ => builder,
    };
    if let Some(search) = query
        .query
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        builder
            .push(" AND lower(l.endpoint || ' ' || coalesce(l.alias,'') || ' ' || coalesce(l.target,'') || ' ' || l.status || ' ' || coalesce(l.error_code,'') || ' ' || coalesce(k.name,'')) LIKE ")
            .push_bind(format!("%{}%", search.to_lowercase()));
    }
}

fn push_usage_cutoff(builder: &mut QueryBuilder<'_, Sqlite>, cutoff: Option<DateTime<Utc>>) {
    if let Some(cutoff) = cutoff {
        builder
            .push(" WHERE l.created_at >= ")
            .push_bind(cutoff.to_rfc3339());
    }
}

fn usage_summary_from_row(row: sqlx::sqlite::SqliteRow) -> UsageSummary {
    UsageSummary {
        request_count: row.get("request_count"),
        success_count: row.get("success_count"),
        average_latency_ms: row.get("average_latency_ms"),
        input_tokens: row.get("input_tokens"),
        output_tokens: row.get("output_tokens"),
        unknown_usage_count: row.get("unknown_usage_count"),
    }
}

fn row_to_local_api_key(row: sqlx::sqlite::SqliteRow) -> anyhow::Result<LocalApiKey> {
    Ok(LocalApiKey {
        id: row.get("id"),
        name: row.get("name"),
        created_at: parse_timestamp(row.get::<String, _>("created_at"))?,
        last_used_at: row
            .get::<Option<String>, _>("last_used_at")
            .map(parse_timestamp)
            .transpose()?,
        revoked_at: row
            .get::<Option<String>, _>("revoked_at")
            .map(parse_timestamp)
            .transpose()?,
    })
}

fn row_to_request_log(row: sqlx::sqlite::SqliteRow) -> anyhow::Result<RequestLog> {
    let api_key_id: Option<String> = row.get("api_key_id");
    let stored_name: Option<String> = row.get("api_key_name");
    Ok(RequestLog {
        id: row.get("id"),
        created_at: parse_timestamp(row.get::<String, _>("created_at"))?,
        endpoint: row.get("endpoint"),
        alias: row.get("alias"),
        target: row.get("target"),
        attempts: row.get("attempts"),
        status: row.get("status"),
        latency_ms: row.get("latency_ms"),
        input_tokens: row.get("input_tokens"),
        output_tokens: row.get("output_tokens"),
        error_code: row.get("error_code"),
        api_key_name: Some(stored_name.unwrap_or_else(|| {
            if api_key_id.is_none() {
                "Unknown / Legacy".into()
            } else {
                "Archived key".into()
            }
        })),
        api_key_id,
    })
}

fn row_to_routing_attempt(row: sqlx::sqlite::SqliteRow) -> anyhow::Result<RoutingAttemptRecord> {
    Ok(RoutingAttemptRecord {
        id: row.get("id"),
        request_id: row.get("request_id"),
        created_at: parse_timestamp(row.get::<String, _>("created_at"))?,
        alias: row.get("alias"),
        task: row.get("task"),
        task_source: row.get("task_source"),
        target_id: row.get("target_id"),
        routing_mode: row.get("routing_mode"),
        status: row.get::<i64, _>("status") as u16,
        transient_failure: row.get::<i64, _>("transient_failure") != 0,
        retry_after_until: row
            .get::<Option<String>, _>("retry_after_until")
            .map(parse_timestamp)
            .transpose()?,
        latency_ms: row.get::<i64, _>("latency_ms").max(0) as u64,
        ttft_ms: row
            .get::<Option<i64>, _>("ttft_ms")
            .map(|value| value.max(0) as u64),
        streaming: row.get::<i64, _>("streaming") != 0,
        input_tokens: row
            .get::<Option<i64>, _>("input_tokens")
            .map(|value| value.max(0) as u64),
        output_tokens: row
            .get::<Option<i64>, _>("output_tokens")
            .map(|value| value.max(0) as u64),
        estimated_cost_usd: row.get("estimated_cost_usd"),
        cost_verified: row.get::<i64, _>("cost_verified") != 0,
        score: row
            .get::<Option<String>, _>("score")
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .context("invalid routing attempt score")?,
        reason: row.get("reason"),
    })
}

fn parse_timestamp(value: String) -> anyhow::Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(&value)?.with_timezone(&Utc))
}

fn row_to_target(row: sqlx::sqlite::SqliteRow) -> anyhow::Result<ModelTarget> {
    Ok(ModelTarget {
        id: row.get("id"),
        provider_id: row.get("provider_id"),
        name: row.get("name"),
        kind: decode_kind(row.get::<String, _>("kind").as_str())?,
        provider_model: row.get("provider_model"),
        local_path: row.get("local_path"),
        runtime_url: row.get("runtime_url"),
        wire_protocol: decode_wire_protocol(row.get::<String, _>("wire_protocol").as_str())?,
        capabilities: serde_json::from_str(row.get::<String, _>("capabilities").as_str())
            .context("invalid capabilities")?,
        enabled: row.get::<i64, _>("enabled") != 0,
        state: row.get("state"),
        size_bytes: row.get("size_bytes"),
        local: LocalModelMeta {
            task: optional_column(&row, "task"),
            runtime_engine: optional_column(&row, "runtime_engine"),
            source_repo: optional_column(&row, "source_repo"),
            source_revision: optional_column(&row, "source_revision"),
            estimated_memory_bytes: row.try_get("estimated_memory_bytes").ok().flatten(),
            catalog_id: optional_column(&row, "catalog_id"),
            trust_status: optional_column(&row, "trust_status"),
            resource_overrides: optional_column(&row, "resource_overrides")
                .map(|value| serde_json::from_str(&value))
                .transpose()
                .context("invalid resource overrides")?,
        },
    })
}

fn row_to_install_job(row: sqlx::sqlite::SqliteRow) -> anyhow::Result<InstallJob> {
    let capabilities: String = row.get("capabilities");
    Ok(InstallJob {
        id: row.get("id"),
        repo_id: row.get("repo_id"),
        revision: row.get("revision"),
        status: row.get("status"),
        catalog_id: row.get("catalog_id"),
        alias: row.get("alias"),
        engine: row.get("engine"),
        task: row.get("task"),
        capabilities: serde_json::from_str(if capabilities.is_empty() {
            "[]"
        } else {
            &capabilities
        })?,
        bytes_downloaded: row.get("bytes_downloaded"),
        bytes_total: row.get("bytes_total"),
        current_file: row.get("current_file"),
        staging_dir: row.get("staging_dir"),
        error: row.get("error"),
        confirm_over_budget: row.get::<i64, _>("confirm_over_budget") != 0,
        created_at: parse_timestamp(row.get::<String, _>("created_at"))?,
        updated_at: parse_timestamp(row.get::<String, _>("updated_at"))?,
    })
}

fn optional_column(row: &sqlx::sqlite::SqliteRow, name: &str) -> Option<String> {
    row.try_get::<Option<String>, _>(name).ok().flatten()
}

pub fn encode_kind(kind: &TargetKind) -> &'static str {
    match kind {
        TargetKind::Cloud => "cloud",
        TargetKind::Gguf => "gguf",
        TargetKind::Mlx => "mlx",
    }
}

pub fn decode_kind(value: &str) -> anyhow::Result<TargetKind> {
    match value {
        "cloud" | "open_ai" | "open_router" => Ok(TargetKind::Cloud),
        "gguf" => Ok(TargetKind::Gguf),
        "mlx" => Ok(TargetKind::Mlx),
        _ => anyhow::bail!("unknown target kind: {value}"),
    }
}

fn encode_auth_mode(value: AuthMode) -> &'static str {
    match value {
        AuthMode::ApiKey => "api_key",
        AuthMode::OpenAiSubscription => "open_ai_subscription",
    }
}

fn decode_auth_mode(value: &str) -> anyhow::Result<AuthMode> {
    match value {
        "api_key" => Ok(AuthMode::ApiKey),
        "open_ai_subscription" => Ok(AuthMode::OpenAiSubscription),
        _ => anyhow::bail!("unknown auth mode {value}"),
    }
}

fn encode_wire_protocol(value: WireProtocol) -> &'static str {
    match value {
        WireProtocol::OpenAiChat => "open_ai_chat",
        WireProtocol::OpenAiResponses => "open_ai_responses",
        WireProtocol::AnthropicMessages => "anthropic_messages",
        WireProtocol::GeminiGenerateContent => "gemini_generate_content",
    }
}

fn decode_wire_protocol(value: &str) -> anyhow::Result<WireProtocol> {
    match value {
        "open_ai_chat" => Ok(WireProtocol::OpenAiChat),
        "open_ai_responses" => Ok(WireProtocol::OpenAiResponses),
        "anthropic_messages" => Ok(WireProtocol::AnthropicMessages),
        "gemini_generate_content" => Ok(WireProtocol::GeminiGenerateContent),
        _ => anyhow::bail!("unknown wire protocol {value}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::{ResourcePolicy, ResourceProfile};
    use chrono::TimeZone;

    #[test]
    fn log_query_defaults_legacy_filter_when_omitted() {
        let query: LogQuery = serde_json::from_value(serde_json::json!({ "limit": 100 })).unwrap();

        assert!(!query.legacy_only);
        assert_eq!(query.limit, Some(100));
    }

    #[tokio::test]
    async fn resource_policy_defaults_to_stealth_and_round_trips() {
        let store = Store::memory().await.unwrap();
        let initial = store.resource_policy(8).await.unwrap();
        assert_eq!(initial.profile, ResourceProfile::Stealth);

        let balanced = ResourcePolicy::preset(ResourceProfile::Balanced, 8);
        store.set_resource_policy(&balanced).await.unwrap();
        assert_eq!(store.resource_policy(2).await.unwrap(), balanced);
    }

    #[tokio::test]
    async fn provider_metadata_and_wire_protocol_round_trip() {
        let store = Store::memory().await.unwrap();
        let provider = Provider {
            id: "groq".into(),
            name: "Groq".into(),
            preset_id: "groq".into(),
            auth_mode: crate::providers::AuthMode::ApiKey,
            base_url: "https://api.groq.com/openai/v1".into(),
            enabled: true,
            has_credential: false,
        };
        store.upsert_provider(&provider).await.unwrap();
        assert_eq!(
            store.provider("groq").await.unwrap().unwrap().preset_id,
            "groq"
        );

        let target = ModelTarget {
            id: "model".into(),
            provider_id: Some("groq".into()),
            name: "Model".into(),
            kind: TargetKind::Cloud,
            provider_model: "model-id".into(),
            local_path: None,
            runtime_url: None,
            wire_protocol: crate::providers::WireProtocol::OpenAiChat,
            capabilities: vec!["chat".into(), "tools".into()],
            enabled: true,
            state: "ready".into(),
            size_bytes: None,
            local: LocalModelMeta::default(),
        };
        store.upsert_target(&target).await.unwrap();
        assert_eq!(
            store.target("model").await.unwrap().unwrap().wire_protocol,
            crate::providers::WireProtocol::OpenAiChat
        );
    }

    #[tokio::test]
    async fn legacy_openrouter_database_is_migrated_idempotently() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy.sqlite3");
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .unwrap()
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePool::connect_with(options).await.unwrap();
        for statement in [
            "CREATE TABLE providers (id TEXT PRIMARY KEY,name TEXT NOT NULL,kind TEXT NOT NULL,base_url TEXT NOT NULL,enabled INTEGER NOT NULL)",
            "CREATE TABLE model_targets (id TEXT PRIMARY KEY,provider_id TEXT,name TEXT NOT NULL,kind TEXT NOT NULL,provider_model TEXT NOT NULL,local_path TEXT,runtime_url TEXT,capabilities TEXT NOT NULL,enabled INTEGER NOT NULL,state TEXT NOT NULL,size_bytes INTEGER)",
            "CREATE TABLE routes (alias TEXT PRIMARY KEY,enabled INTEGER NOT NULL,capabilities TEXT NOT NULL)",
            "CREATE TABLE route_targets (alias TEXT NOT NULL,target_id TEXT NOT NULL,priority INTEGER NOT NULL,PRIMARY KEY(alias,target_id))",
            "CREATE TABLE settings (key TEXT PRIMARY KEY,value TEXT NOT NULL)",
            "CREATE TABLE provider_models (provider_id TEXT NOT NULL,model_id TEXT NOT NULL,synced_at TEXT NOT NULL,PRIMARY KEY(provider_id,model_id))",
            "CREATE TABLE request_logs (id TEXT PRIMARY KEY,created_at TEXT NOT NULL,endpoint TEXT NOT NULL,alias TEXT,target TEXT,attempts INTEGER NOT NULL,status INTEGER NOT NULL,latency_ms INTEGER NOT NULL,input_tokens INTEGER,output_tokens INTEGER,error_code TEXT)",
            "INSERT INTO providers VALUES ('provider','OpenRouter','open_router','https://openrouter.ai/api/v1',1)",
            "INSERT INTO model_targets VALUES ('target','provider','Old target','open_router','old-model',NULL,NULL,'[\"chat\"]',1,'ready',NULL)",
            "INSERT INTO routes VALUES ('assistant',1,'[\"chat\"]')",
            "INSERT INTO route_targets VALUES ('assistant','target',10)",
            "INSERT INTO provider_models VALUES ('provider','old-model','2025-01-01T00:00:00Z')",
            "INSERT INTO request_logs VALUES ('log','2025-01-01T00:00:00Z','/v1/chat/completions','assistant','Old target',1,200,5,1,2,NULL)",
        ] { sqlx::query(statement).execute(&pool).await.unwrap(); }
        pool.close().await;

        let store = Store::open(&path).await.unwrap();
        assert_eq!(
            store.provider("provider").await.unwrap().unwrap().preset_id,
            "openrouter"
        );
        assert_eq!(
            store.target("target").await.unwrap().unwrap().kind,
            TargetKind::Cloud
        );
        assert_eq!(
            store.provider_models("provider").await.unwrap()[0].capabilities,
            vec!["chat", "streaming"]
        );
        assert!(store.route("assistant").await.unwrap().is_some());
        assert_eq!(store.logs(10).await.unwrap().len(), 1);
        assert_eq!(
            store.resource_policy(8).await.unwrap().profile,
            ResourceProfile::Custom
        );
        assert!(!store.resource_policy(8).await.unwrap().auto_load);
        drop(store);
        Store::open(&path).await.unwrap();
    }

    #[tokio::test]
    async fn route_round_trip_preserves_order_and_capabilities() {
        let store = Store::memory().await.unwrap();
        for (id, model) in [("one", "gpt-one"), ("two", "gpt-two")] {
            store
                .upsert_target(&ModelTarget {
                    id: id.into(),
                    provider_id: None,
                    name: id.into(),
                    kind: TargetKind::Cloud,
                    provider_model: model.into(),
                    local_path: None,
                    runtime_url: Some("http://example.test/v1".into()),
                    wire_protocol: WireProtocol::OpenAiChat,
                    capabilities: vec!["chat".into()],
                    enabled: true,
                    state: "ready".into(),
                    size_bytes: None,
                    local: LocalModelMeta::default(),
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
                        id: "two".into(),
                        kind: TargetKind::Cloud,
                        model: "gpt-two".into(),
                        priority: 20,
                        enabled: true,
                    },
                    RouteTarget {
                        id: "one".into(),
                        kind: TargetKind::Cloud,
                        model: "gpt-one".into(),
                        priority: 10,
                        enabled: true,
                    },
                ],
            })
            .await
            .unwrap();

        let route = store.route("assistant").await.unwrap().unwrap();
        assert_eq!(route.capabilities, vec!["chat"]);
        assert_eq!(route.ordered_targets()[0].id, "one");
    }

    #[tokio::test]
    async fn log_storage_never_has_a_body_field() {
        let store = Store::memory().await.unwrap();
        store
            .insert_log(&RequestLog {
                id: "request".into(),
                created_at: Utc::now(),
                endpoint: "/v1/chat/completions".into(),
                alias: Some("safe".into()),
                target: Some("cloud".into()),
                attempts: 1,
                status: 200,
                latency_ms: 5,
                input_tokens: Some(2),
                output_tokens: Some(3),
                error_code: None,
                api_key_id: None,
                api_key_name: None,
            })
            .await
            .unwrap();
        let encoded = serde_json::to_string(&store.logs(10).await.unwrap()).unwrap();
        assert!(!encoded.contains("prompt"));
        assert!(!encoded.contains("response"));
    }

    #[tokio::test]
    async fn local_api_keys_and_legacy_logs_are_exposed_through_public_store_interfaces() {
        let store = Store::memory().await.unwrap();
        let key = LocalApiKey {
            id: "client-one".into(),
            name: "Client one".into(),
            created_at: Utc::now(),
            last_used_at: None,
            revoked_at: None,
        };
        store.insert_local_api_key(&key, &[7; 32]).await.unwrap();

        assert_eq!(store.local_api_keys().await.unwrap(), vec![key]);
        assert_eq!(
            store.active_local_api_key_hashes().await.unwrap(),
            vec![("client-one".into(), vec![7; 32])]
        );

        store
            .insert_log(&RequestLog {
                id: "legacy".into(),
                created_at: Utc::now(),
                endpoint: "/v1/chat/completions".into(),
                alias: Some("assistant".into()),
                target: Some("cloud".into()),
                attempts: 1,
                status: 200,
                latency_ms: 10,
                input_tokens: None,
                output_tokens: None,
                error_code: None,
                api_key_id: None,
                api_key_name: None,
            })
            .await
            .unwrap();

        let result = store
            .query_logs(&LogQuery {
                legacy_only: true,
                ..LogQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(result.total, 1);
        assert_eq!(
            result.items[0].api_key_name.as_deref(),
            Some("Unknown / Legacy")
        );

        store
            .insert_log(&RequestLog {
                id: "attributed".into(),
                created_at: Utc::now(),
                endpoint: "/v1/embeddings".into(),
                alias: Some("embed".into()),
                target: Some("cloud".into()),
                attempts: 1,
                status: 500,
                latency_ms: 30,
                input_tokens: Some(12),
                output_tokens: Some(4),
                error_code: Some("upstream".into()),
                api_key_id: Some("client-one".into()),
                api_key_name: None,
            })
            .await
            .unwrap();
        let usage = store.usage("all").await.unwrap();
        assert_eq!(usage.summary.request_count, 2);
        assert_eq!(usage.summary.success_count, 1);
        assert_eq!(usage.summary.input_tokens, 12);
        assert_eq!(usage.summary.unknown_usage_count, 1);
        assert_eq!(usage.by_key.len(), 2);

        let filtered = store
            .query_logs(&LogQuery {
                api_key_id: Some("client-one".into()),
                alias: Some("embed".into()),
                target: Some("cloud".into()),
                endpoint: Some("/v1/embeddings".into()),
                status_class: Some("5xx".into()),
                query: Some("upstream".into()),
                from: Some(Utc::now() - chrono::Duration::minutes(1)),
                to: Some(Utc::now() + chrono::Duration::minutes(1)),
                limit: Some(1),
                ..LogQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(filtered.total, 1);
        assert_eq!(filtered.items[0].id, "attributed");

        let second_page = store
            .query_logs(&LogQuery {
                limit: Some(1),
                offset: Some(1),
                ..LogQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(second_page.total, 2);
        assert_eq!(second_page.items.len(), 1);
        assert_ne!(second_page.items[0].id, filtered.items[0].id);
    }

    #[tokio::test]
    async fn usage_periods_apply_cutoffs_and_hourly_or_daily_buckets() {
        let store = Store::memory().await.unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 17, 12, 0, 0).unwrap();
        for (id, created_at) in [
            ("hour-one", now - chrono::Duration::minutes(30)),
            ("hour-two", now - chrono::Duration::minutes(90)),
            ("previous-day", now - chrono::Duration::hours(25)),
        ] {
            store
                .insert_log(&RequestLog {
                    id: id.into(),
                    created_at,
                    endpoint: "/v1/chat/completions".into(),
                    alias: Some("assistant".into()),
                    target: Some("cloud".into()),
                    attempts: 1,
                    status: 200,
                    latency_ms: 5,
                    input_tokens: Some(2),
                    output_tokens: Some(3),
                    error_code: None,
                    api_key_id: None,
                    api_key_name: None,
                })
                .await
                .unwrap();
        }

        let daily = store.usage_at("7d", now).await.unwrap();
        assert_eq!(daily.summary.request_count, 3);
        assert_eq!(daily.buckets.len(), 2);
        let hourly = store.usage_at("24h", now).await.unwrap();
        assert_eq!(hourly.summary.request_count, 2);
        assert_eq!(hourly.buckets.len(), 2);
    }

    #[tokio::test]
    async fn existing_cloud_gguf_and_mlx_targets_load_without_catalog_metadata() {
        let store = Store::memory().await.unwrap();
        for (id, kind) in [
            ("cloud", TargetKind::Cloud),
            ("gguf", TargetKind::Gguf),
            ("mlx", TargetKind::Mlx),
        ] {
            store
                .upsert_target(&ModelTarget {
                    id: id.into(),
                    provider_id: None,
                    name: id.into(),
                    kind,
                    provider_model: id.into(),
                    local_path: None,
                    runtime_url: None,
                    wire_protocol: WireProtocol::OpenAiChat,
                    capabilities: vec!["chat".into()],
                    enabled: true,
                    state: "stopped".into(),
                    size_bytes: None,
                    local: LocalModelMeta::default(),
                })
                .await
                .unwrap();
        }
        let loaded = store.targets().await.unwrap();
        assert_eq!(loaded.len(), 3);
        for target in loaded {
            assert!(target.local.task.is_none());
            assert!(target.local.source_repo.is_none());
            assert_eq!(target.capabilities, vec!["chat"]);
        }
    }

    #[tokio::test]
    async fn adaptive_routing_configuration_and_attempt_history_round_trip() {
        let store = Store::memory().await.unwrap();
        store
            .upsert_target(&ModelTarget {
                id: "adaptive-target".into(),
                provider_id: None,
                name: "Adaptive".into(),
                kind: TargetKind::Gguf,
                provider_model: "adaptive".into(),
                local_path: None,
                runtime_url: None,
                wire_protocol: WireProtocol::OpenAiChat,
                capabilities: vec!["chat".into()],
                enabled: true,
                state: "stopped".into(),
                size_bytes: None,
                local: LocalModelMeta::default(),
            })
            .await
            .unwrap();
        store
            .upsert_route(&ModelRoute {
                alias: "adaptive".into(),
                enabled: true,
                capabilities: vec!["chat".into()],
                targets: vec![RouteTarget {
                    id: "adaptive-target".into(),
                    kind: TargetKind::Gguf,
                    model: "adaptive".into(),
                    priority: 10,
                    enabled: true,
                }],
            })
            .await
            .unwrap();
        let mut policy = crate::routing::RoutingPolicy::new("adaptive");
        policy.status = crate::routing::PolicyStatus::Shadow;
        store.upsert_routing_policy(&policy).await.unwrap();
        let mut profile =
            crate::routing::TargetRoutingProfile::neutral("adaptive-target", TargetKind::Gguf);
        profile.task_quality.insert("coding".into(), 90.0);
        store.upsert_target_routing_profile(&profile).await.unwrap();
        for index in 0..3 {
            store
                .insert_routing_attempt(&crate::routing::RoutingAttemptRecord {
                    id: format!("attempt-{index}"),
                    request_id: "request".into(),
                    created_at: Utc::now(),
                    alias: "adaptive".into(),
                    task: "coding".into(),
                    task_source: "header".into(),
                    target_id: "adaptive-target".into(),
                    routing_mode: "adaptive".into(),
                    status: 503,
                    transient_failure: true,
                    retry_after_until: None,
                    latency_ms: 20,
                    ttft_ms: None,
                    streaming: false,
                    input_tokens: Some(10),
                    output_tokens: None,
                    estimated_cost_usd: Some(0.0),
                    cost_verified: true,
                    score: None,
                    reason: "header".into(),
                })
                .await
                .unwrap();
        }

        assert_eq!(
            store
                .routing_policy("adaptive")
                .await
                .unwrap()
                .unwrap()
                .status,
            crate::routing::PolicyStatus::Shadow
        );
        assert_eq!(
            store
                .target_routing_profile("adaptive-target")
                .await
                .unwrap()
                .unwrap()
                .task_quality["coding"],
            90.0
        );
        assert_eq!(
            store
                .routing_attempts(Some("request"), 10)
                .await
                .unwrap()
                .len(),
            3
        );
        assert!(
            store
                .routing_stats("adaptive-target", "coding", false)
                .await
                .unwrap()
                .circuit_open
        );
    }
}

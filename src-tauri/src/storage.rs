use std::{path::Path, str::FromStr};

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqliteConnectOptions, QueryBuilder, Row, Sqlite, SqlitePool};

use crate::domain::{ModelRoute, RouteTarget, TargetKind};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub kind: TargetKind,
    pub base_url: String,
    pub enabled: bool,
    pub has_credential: bool,
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
    pub capabilities: Vec<String>,
    pub enabled: bool,
    pub state: String,
    pub size_bytes: Option<i64>,
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
}

impl Store {
    pub async fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePool::connect_with(options).await?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    pub async fn memory() -> anyhow::Result<Self> {
        let pool = SqlitePool::connect("sqlite::memory:").await?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    async fn migrate(&self) -> anyhow::Result<()> {
        for statement in [
            "CREATE TABLE IF NOT EXISTS providers (id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL, base_url TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1)",
            "CREATE TABLE IF NOT EXISTS model_targets (id TEXT PRIMARY KEY, provider_id TEXT, name TEXT NOT NULL, kind TEXT NOT NULL, provider_model TEXT NOT NULL, local_path TEXT, runtime_url TEXT, capabilities TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1, state TEXT NOT NULL DEFAULT 'ready', size_bytes INTEGER, FOREIGN KEY(provider_id) REFERENCES providers(id) ON DELETE CASCADE)",
            "CREATE TABLE IF NOT EXISTS routes (alias TEXT PRIMARY KEY, enabled INTEGER NOT NULL DEFAULT 1, capabilities TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS route_targets (alias TEXT NOT NULL, target_id TEXT NOT NULL, priority INTEGER NOT NULL, PRIMARY KEY(alias, target_id), FOREIGN KEY(alias) REFERENCES routes(alias) ON DELETE CASCADE, FOREIGN KEY(target_id) REFERENCES model_targets(id) ON DELETE CASCADE)",
            "CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS provider_models (provider_id TEXT NOT NULL, model_id TEXT NOT NULL, synced_at TEXT NOT NULL, PRIMARY KEY(provider_id, model_id), FOREIGN KEY(provider_id) REFERENCES providers(id) ON DELETE CASCADE)",
            "CREATE TABLE IF NOT EXISTS local_api_keys (id TEXT PRIMARY KEY, name TEXT NOT NULL, token_hash BLOB NOT NULL, created_at TEXT NOT NULL, last_used_at TEXT, revoked_at TEXT)",
            "CREATE TABLE IF NOT EXISTS request_logs (id TEXT PRIMARY KEY, created_at TEXT NOT NULL, endpoint TEXT NOT NULL, alias TEXT, target TEXT, attempts INTEGER NOT NULL, status INTEGER NOT NULL, latency_ms INTEGER NOT NULL, input_tokens INTEGER, output_tokens INTEGER, error_code TEXT)",
            "CREATE INDEX IF NOT EXISTS request_logs_created_idx ON request_logs(created_at DESC)",
            "CREATE INDEX IF NOT EXISTS local_api_keys_active_idx ON local_api_keys(revoked_at)",
        ] {
            sqlx::query(statement).execute(&self.pool).await?;
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
        sqlx::query("CREATE INDEX IF NOT EXISTS request_logs_api_key_idx ON request_logs(api_key_id, created_at DESC)")
            .execute(&self.pool)
            .await?;
        self.set_default("port", "11435").await?;
        self.set_default("memory_budget_percent", "70").await?;
        self.set_default("idle_unload_minutes", "15").await?;
        self.set_default("log_retention_days", "30").await?;
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

    pub async fn providers(&self) -> anyhow::Result<Vec<Provider>> {
        let rows =
            sqlx::query("SELECT id, name, kind, base_url, enabled FROM providers ORDER BY name")
                .fetch_all(&self.pool)
                .await?;
        rows.into_iter()
            .map(|row| {
                Ok(Provider {
                    id: row.get("id"),
                    name: row.get("name"),
                    kind: decode_kind(row.get::<String, _>("kind").as_str())?,
                    base_url: row.get("base_url"),
                    enabled: row.get::<i64, _>("enabled") != 0,
                    has_credential: false,
                })
            })
            .collect()
    }

    pub async fn provider(&self, id: &str) -> anyhow::Result<Option<Provider>> {
        let row = sqlx::query("SELECT id, name, kind, base_url, enabled FROM providers WHERE id=?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| {
            Ok(Provider {
                id: row.get("id"),
                name: row.get("name"),
                kind: decode_kind(row.get::<String, _>("kind").as_str())?,
                base_url: row.get("base_url"),
                enabled: row.get::<i64, _>("enabled") != 0,
                has_credential: false,
            })
        })
        .transpose()
    }

    pub async fn upsert_provider(&self, provider: &Provider) -> anyhow::Result<()> {
        sqlx::query("INSERT INTO providers(id,name,kind,base_url,enabled) VALUES(?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,kind=excluded.kind,base_url=excluded.base_url,enabled=excluded.enabled")
            .bind(&provider.id).bind(&provider.name).bind(encode_kind(&provider.kind)).bind(&provider.base_url).bind(provider.enabled as i64)
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
        models: &[String],
    ) -> anyhow::Result<()> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("DELETE FROM provider_models WHERE provider_id=?")
            .bind(provider_id)
            .execute(&mut *transaction)
            .await?;
        let synced_at = Utc::now().to_rfc3339();
        for model in models {
            sqlx::query(
                "INSERT INTO provider_models(provider_id, model_id, synced_at) VALUES(?,?,?)",
            )
            .bind(provider_id)
            .bind(model)
            .bind(&synced_at)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn provider_models(&self, provider_id: &str) -> anyhow::Result<Vec<String>> {
        Ok(sqlx::query_scalar(
            "SELECT model_id FROM provider_models WHERE provider_id=? ORDER BY model_id",
        )
        .bind(provider_id)
        .fetch_all(&self.pool)
        .await?)
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
        sqlx::query("INSERT INTO model_targets(id,provider_id,name,kind,provider_model,local_path,runtime_url,capabilities,enabled,state,size_bytes) VALUES(?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET provider_id=excluded.provider_id,name=excluded.name,kind=excluded.kind,provider_model=excluded.provider_model,local_path=excluded.local_path,runtime_url=excluded.runtime_url,capabilities=excluded.capabilities,enabled=excluded.enabled,state=excluded.state,size_bytes=excluded.size_bytes")
            .bind(&target.id).bind(&target.provider_id).bind(&target.name).bind(encode_kind(&target.kind)).bind(&target.provider_model)
            .bind(&target.local_path).bind(&target.runtime_url).bind(capabilities).bind(target.enabled as i64).bind(&target.state).bind(target.size_bytes)
            .execute(&self.pool).await?;
        Ok(())
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
        capabilities: serde_json::from_str(row.get::<String, _>("capabilities").as_str())
            .context("invalid capabilities")?,
        enabled: row.get::<i64, _>("enabled") != 0,
        state: row.get("state"),
        size_bytes: row.get("size_bytes"),
    })
}

pub fn encode_kind(kind: &TargetKind) -> &'static str {
    match kind {
        TargetKind::OpenAi => "open_ai",
        TargetKind::OpenRouter => "open_router",
        TargetKind::Gguf => "gguf",
        TargetKind::Mlx => "mlx",
    }
}

pub fn decode_kind(value: &str) -> anyhow::Result<TargetKind> {
    match value {
        "open_ai" => Ok(TargetKind::OpenAi),
        "open_router" => Ok(TargetKind::OpenRouter),
        "gguf" => Ok(TargetKind::Gguf),
        "mlx" => Ok(TargetKind::Mlx),
        _ => anyhow::bail!("unknown target kind: {value}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[tokio::test]
    async fn route_round_trip_preserves_order_and_capabilities() {
        let store = Store::memory().await.unwrap();
        for (id, model) in [("one", "gpt-one"), ("two", "gpt-two")] {
            store
                .upsert_target(&ModelTarget {
                    id: id.into(),
                    provider_id: None,
                    name: id.into(),
                    kind: TargetKind::OpenAi,
                    provider_model: model.into(),
                    local_path: None,
                    runtime_url: Some("http://example.test/v1".into()),
                    capabilities: vec!["chat".into()],
                    enabled: true,
                    state: "ready".into(),
                    size_bytes: None,
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
                        kind: TargetKind::OpenAi,
                        model: "gpt-two".into(),
                        priority: 20,
                        enabled: true,
                    },
                    RouteTarget {
                        id: "one".into(),
                        kind: TargetKind::OpenAi,
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
}

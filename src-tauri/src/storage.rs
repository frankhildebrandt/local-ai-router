use std::{path::Path, str::FromStr};

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqliteConnectOptions, Row, SqlitePool};

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
            "CREATE TABLE IF NOT EXISTS request_logs (id TEXT PRIMARY KEY, created_at TEXT NOT NULL, endpoint TEXT NOT NULL, alias TEXT, target TEXT, attempts INTEGER NOT NULL, status INTEGER NOT NULL, latency_ms INTEGER NOT NULL, input_tokens INTEGER, output_tokens INTEGER, error_code TEXT)",
            "CREATE INDEX IF NOT EXISTS request_logs_created_idx ON request_logs(created_at DESC)",
        ] {
            sqlx::query(statement).execute(&self.pool).await?;
        }
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

    pub async fn insert_log(&self, log: &RequestLog) -> anyhow::Result<()> {
        sqlx::query("INSERT INTO request_logs(id,created_at,endpoint,alias,target,attempts,status,latency_ms,input_tokens,output_tokens,error_code) VALUES(?,?,?,?,?,?,?,?,?,?,?)")
            .bind(&log.id).bind(log.created_at.to_rfc3339()).bind(&log.endpoint).bind(&log.alias).bind(&log.target)
            .bind(log.attempts).bind(log.status).bind(log.latency_ms).bind(log.input_tokens).bind(log.output_tokens).bind(&log.error_code)
            .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn logs(&self, limit: i64) -> anyhow::Result<Vec<RequestLog>> {
        let rows = sqlx::query("SELECT * FROM request_logs ORDER BY created_at DESC LIMIT ?")
            .bind(limit.clamp(1, 1000))
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                Ok(RequestLog {
                    id: row.get("id"),
                    created_at: DateTime::parse_from_rfc3339(
                        row.get::<String, _>("created_at").as_str(),
                    )?
                    .with_timezone(&Utc),
                    endpoint: row.get("endpoint"),
                    alias: row.get("alias"),
                    target: row.get("target"),
                    attempts: row.get("attempts"),
                    status: row.get("status"),
                    latency_ms: row.get("latency_ms"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    error_code: row.get("error_code"),
                })
            })
            .collect()
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
            })
            .await
            .unwrap();
        let encoded = serde_json::to_string(&store.logs(10).await.unwrap()).unwrap();
        assert!(!encoded.contains("prompt"));
        assert!(!encoded.contains("response"));
    }
}

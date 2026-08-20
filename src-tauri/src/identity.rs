use std::collections::BTreeSet;

use anyhow::Context;
use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::{
    secrets::{generate_local_token, SecretStore},
    storage::Store,
};

pub const OPERATOR_BOOTSTRAP_ACCOUNT: &str = "directory-operator-bootstrap";
pub const SESSION_COOKIE: &str = "lar_session";
const SESSION_TTL_DAYS: i64 = 14;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectoryUser {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub is_operator: bool,
    pub disabled_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub group_ids: Vec<String>,
    pub allowed_model_ids: Option<Vec<String>>,
    pub may_publish: Option<bool>,
    pub may_admin: Option<bool>,
    pub has_password: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectoryGroup {
    pub id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub allowed_model_ids: Vec<String>,
    pub may_publish: bool,
    pub may_admin: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EffectivePermissions {
    pub allowed_model_ids: Option<Vec<String>>,
    pub may_publish: bool,
    pub may_admin: bool,
}

impl EffectivePermissions {
    pub fn all() -> Self {
        Self {
            allowed_model_ids: None,
            may_publish: true,
            may_admin: true,
        }
    }

    pub fn none() -> Self {
        Self {
            allowed_model_ids: Some(Vec::new()),
            may_publish: false,
            may_admin: false,
        }
    }

    pub fn allows_model(&self, model_id: &str) -> bool {
        match &self.allowed_model_ids {
            None => true,
            Some(ids) => ids.iter().any(|id| id == model_id),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OidcIdentity {
    pub id: String,
    pub user_id: String,
    pub provider: String,
    pub subject: String,
    pub email: Option<String>,
    pub login: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OidcAllowlistEntry {
    pub id: String,
    pub provider: String,
    pub identifier: String,
    pub user_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateUserInput {
    pub username: String,
    pub display_name: String,
    pub password: Option<String>,
    #[serde(default)]
    pub group_ids: Vec<String>,
    pub allowed_model_ids: Option<Vec<String>>,
    pub may_publish: Option<bool>,
    pub may_admin: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateUserInput {
    pub display_name: Option<String>,
    pub password: Option<String>,
    pub group_ids: Option<Vec<String>>,
    pub allowed_model_ids: Option<Vec<String>>,
    pub inherit_models: Option<bool>,
    pub may_publish: Option<bool>,
    pub inherit_publish: Option<bool>,
    pub may_admin: Option<bool>,
    pub inherit_admin: Option<bool>,
    pub disabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertGroupInput {
    pub name: String,
    #[serde(default)]
    pub allowed_model_ids: Vec<String>,
    #[serde(default)]
    pub may_publish: bool,
    #[serde(default)]
    pub may_admin: bool,
}

pub async fn migrate(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS directory_users (
            id TEXT PRIMARY KEY,
            username TEXT NOT NULL UNIQUE,
            display_name TEXT NOT NULL,
            password_hash TEXT,
            is_operator INTEGER NOT NULL DEFAULT 0,
            disabled_at TEXT,
            created_at TEXT NOT NULL,
            allowed_model_ids TEXT,
            may_publish INTEGER,
            may_admin INTEGER
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS directory_groups (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL,
            allowed_model_ids TEXT NOT NULL DEFAULT '[]',
            may_publish INTEGER NOT NULL DEFAULT 0,
            may_admin INTEGER NOT NULL DEFAULT 0
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS directory_user_groups (
            user_id TEXT NOT NULL,
            group_id TEXT NOT NULL,
            PRIMARY KEY (user_id, group_id),
            FOREIGN KEY (user_id) REFERENCES directory_users(id) ON DELETE CASCADE,
            FOREIGN KEY (group_id) REFERENCES directory_groups(id) ON DELETE CASCADE
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS directory_sessions (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            token_hash BLOB NOT NULL,
            created_at TEXT NOT NULL,
            expires_at TEXT NOT NULL,
            FOREIGN KEY (user_id) REFERENCES directory_users(id) ON DELETE CASCADE
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS directory_oidc_identities (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            provider TEXT NOT NULL,
            subject TEXT NOT NULL,
            email TEXT,
            login TEXT,
            created_at TEXT NOT NULL,
            UNIQUE(provider, subject),
            FOREIGN KEY (user_id) REFERENCES directory_users(id) ON DELETE CASCADE
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS directory_oidc_allowlist (
            id TEXT PRIMARY KEY,
            provider TEXT NOT NULL,
            identifier TEXT NOT NULL,
            user_id TEXT,
            created_at TEXT NOT NULL,
            UNIQUE(provider, identifier),
            FOREIGN KEY (user_id) REFERENCES directory_users(id) ON DELETE SET NULL
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS directory_sessions_token_idx ON directory_sessions(token_hash)")
        .execute(pool)
        .await?;
    Ok(())
}

fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut argon2::password_hash::rand_core::OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| anyhow::anyhow!("{error}"))
}

fn verify_password(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash)
        .ok()
        .and_then(|parsed| Argon2::default().verify_password(password.as_bytes(), &parsed).ok())
        .is_some()
}

fn session_token_hash(token: &str) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    Sha256::digest(token.as_bytes()).to_vec()
}

fn normalize_username(username: &str) -> anyhow::Result<String> {
    let username = username.trim().to_lowercase();
    if username.is_empty() || username.chars().count() > 40 {
        anyhow::bail!("username must be 1–40 characters");
    }
    if !username
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_' | '-'))
    {
        anyhow::bail!("username may only contain letters, digits, '.', '_' and '-'");
    }
    Ok(username)
}

fn normalize_group_name(name: &str) -> anyhow::Result<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        anyhow::bail!("group name must be 1–80 characters");
    }
    Ok(name.to_owned())
}

fn encode_models(ids: &[String]) -> String {
    serde_json::to_string(ids).unwrap_or_else(|_| "[]".into())
}

fn decode_models(raw: Option<String>) -> Option<Vec<String>> {
    raw.and_then(|value| serde_json::from_str(&value).ok())
}

fn optional_bool(value: Option<i64>) -> Option<bool> {
    value.map(|flag| flag != 0)
}

impl Store {
    pub async fn directory_users(&self) -> anyhow::Result<Vec<DirectoryUser>> {
        let rows = sqlx::query(
            "SELECT id,username,display_name,password_hash,is_operator,disabled_at,created_at,allowed_model_ids,may_publish,may_admin
             FROM directory_users ORDER BY is_operator DESC, username",
        )
        .fetch_all(self.pool())
        .await?;
        let mut users = Vec::new();
        for row in rows {
            users.push(self.hydrate_user(row).await?);
        }
        Ok(users)
    }

    pub async fn directory_user(&self, id: &str) -> anyhow::Result<Option<DirectoryUser>> {
        let row = sqlx::query(
            "SELECT id,username,display_name,password_hash,is_operator,disabled_at,created_at,allowed_model_ids,may_publish,may_admin
             FROM directory_users WHERE id=?",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        match row {
            Some(row) => Ok(Some(self.hydrate_user(row).await?)),
            None => Ok(None),
        }
    }

    pub async fn directory_user_by_username(
        &self,
        username: &str,
    ) -> anyhow::Result<Option<DirectoryUser>> {
        let row = sqlx::query(
            "SELECT id,username,display_name,password_hash,is_operator,disabled_at,created_at,allowed_model_ids,may_publish,may_admin
             FROM directory_users WHERE username=?",
        )
        .bind(username)
        .fetch_optional(self.pool())
        .await?;
        match row {
            Some(row) => Ok(Some(self.hydrate_user(row).await?)),
            None => Ok(None),
        }
    }

    async fn hydrate_user(&self, row: sqlx::sqlite::SqliteRow) -> anyhow::Result<DirectoryUser> {
        let id: String = row.get("id");
        let groups = sqlx::query_scalar::<_, String>(
            "SELECT group_id FROM directory_user_groups WHERE user_id=? ORDER BY group_id",
        )
        .bind(&id)
        .fetch_all(self.pool())
        .await?;
        let password_hash: Option<String> = row.get("password_hash");
        Ok(DirectoryUser {
            id,
            username: row.get("username"),
            display_name: row.get("display_name"),
            is_operator: row.get::<i64, _>("is_operator") != 0,
            disabled_at: row
                .get::<Option<String>, _>("disabled_at")
                .and_then(|value| value.parse().ok()),
            created_at: row.get::<String, _>("created_at").parse()?,
            group_ids: groups,
            allowed_model_ids: decode_models(row.get("allowed_model_ids")),
            may_publish: optional_bool(row.get("may_publish")),
            may_admin: optional_bool(row.get("may_admin")),
            has_password: password_hash.is_some(),
        })
    }

    async fn user_password_hash(&self, id: &str) -> anyhow::Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT password_hash FROM directory_users WHERE id=?")
                .bind(id)
                .fetch_optional(self.pool())
                .await?,
        )
    }

    pub async fn insert_directory_user(
        &self,
        user: &DirectoryUser,
        password_hash: Option<&str>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO directory_users(id,username,display_name,password_hash,is_operator,disabled_at,created_at,allowed_model_ids,may_publish,may_admin)
             VALUES(?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(&user.id)
        .bind(&user.username)
        .bind(&user.display_name)
        .bind(password_hash)
        .bind(user.is_operator as i64)
        .bind(user.disabled_at.map(|value| value.to_rfc3339()))
        .bind(user.created_at.to_rfc3339())
        .bind(user.allowed_model_ids.as_ref().map(|ids| encode_models(ids)))
        .bind(user.may_publish.map(|flag| flag as i64))
        .bind(user.may_admin.map(|flag| flag as i64))
        .execute(self.pool())
        .await?;
        self.replace_user_groups(&user.id, &user.group_ids).await
    }

    pub async fn update_directory_user(
        &self,
        user: &DirectoryUser,
        password_hash: Option<&str>,
        replace_password: bool,
    ) -> anyhow::Result<()> {
        if replace_password {
            sqlx::query(
                "UPDATE directory_users SET display_name=?, password_hash=?, disabled_at=?, allowed_model_ids=?, may_publish=?, may_admin=? WHERE id=?",
            )
            .bind(&user.display_name)
            .bind(password_hash)
            .bind(user.disabled_at.map(|value| value.to_rfc3339()))
            .bind(user.allowed_model_ids.as_ref().map(|ids| encode_models(ids)))
            .bind(user.may_publish.map(|flag| flag as i64))
            .bind(user.may_admin.map(|flag| flag as i64))
            .bind(&user.id)
            .execute(self.pool())
            .await?;
        } else {
            sqlx::query(
                "UPDATE directory_users SET display_name=?, disabled_at=?, allowed_model_ids=?, may_publish=?, may_admin=? WHERE id=?",
            )
            .bind(&user.display_name)
            .bind(user.disabled_at.map(|value| value.to_rfc3339()))
            .bind(user.allowed_model_ids.as_ref().map(|ids| encode_models(ids)))
            .bind(user.may_publish.map(|flag| flag as i64))
            .bind(user.may_admin.map(|flag| flag as i64))
            .bind(&user.id)
            .execute(self.pool())
            .await?;
        }
        self.replace_user_groups(&user.id, &user.group_ids).await
    }

    async fn replace_user_groups(&self, user_id: &str, group_ids: &[String]) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM directory_user_groups WHERE user_id=?")
            .bind(user_id)
            .execute(self.pool())
            .await?;
        for group_id in group_ids {
            sqlx::query("INSERT INTO directory_user_groups(user_id, group_id) VALUES(?,?)")
                .bind(user_id)
                .bind(group_id)
                .execute(self.pool())
                .await?;
        }
        Ok(())
    }

    pub async fn directory_groups(&self) -> anyhow::Result<Vec<DirectoryGroup>> {
        let rows = sqlx::query(
            "SELECT id,name,created_at,allowed_model_ids,may_publish,may_admin FROM directory_groups ORDER BY name",
        )
        .fetch_all(self.pool())
        .await?;
        rows.into_iter().map(row_to_group).collect()
    }

    pub async fn directory_group(&self, id: &str) -> anyhow::Result<Option<DirectoryGroup>> {
        sqlx::query(
            "SELECT id,name,created_at,allowed_model_ids,may_publish,may_admin FROM directory_groups WHERE id=?",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?
        .map(row_to_group)
        .transpose()
    }

    pub async fn upsert_directory_group(&self, group: &DirectoryGroup) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO directory_groups(id,name,created_at,allowed_model_ids,may_publish,may_admin)
             VALUES(?,?,?,?,?,?)
             ON CONFLICT(id) DO UPDATE SET name=excluded.name, allowed_model_ids=excluded.allowed_model_ids, may_publish=excluded.may_publish, may_admin=excluded.may_admin",
        )
        .bind(&group.id)
        .bind(&group.name)
        .bind(group.created_at.to_rfc3339())
        .bind(encode_models(&group.allowed_model_ids))
        .bind(group.may_publish as i64)
        .bind(group.may_admin as i64)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn delete_directory_group(&self, id: &str) -> anyhow::Result<bool> {
        let result = sqlx::query("DELETE FROM directory_groups WHERE id=?")
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn insert_session(
        &self,
        user_id: &str,
        token_hash: &[u8],
        expires_at: DateTime<Utc>,
    ) -> anyhow::Result<String> {
        let id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO directory_sessions(id,user_id,token_hash,created_at,expires_at) VALUES(?,?,?,?,?)",
        )
        .bind(&id)
        .bind(user_id)
        .bind(token_hash)
        .bind(Utc::now().to_rfc3339())
        .bind(expires_at.to_rfc3339())
        .execute(self.pool())
        .await?;
        Ok(id)
    }

    pub async fn session_user_id(&self, token_hash: &[u8]) -> anyhow::Result<Option<String>> {
        let now = Utc::now().to_rfc3339();
        Ok(sqlx::query_scalar(
            "SELECT user_id FROM directory_sessions WHERE token_hash=? AND expires_at > ? LIMIT 1",
        )
        .bind(token_hash)
        .bind(now)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn delete_sessions_for_token(&self, token_hash: &[u8]) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM directory_sessions WHERE token_hash=?")
            .bind(token_hash)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn oidc_allowlist(&self) -> anyhow::Result<Vec<OidcAllowlistEntry>> {
        let rows = sqlx::query(
            "SELECT id,provider,identifier,user_id,created_at FROM directory_oidc_allowlist ORDER BY provider, identifier",
        )
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(OidcAllowlistEntry {
                    id: row.get("id"),
                    provider: row.get("provider"),
                    identifier: row.get("identifier"),
                    user_id: row.get("user_id"),
                    created_at: row.get::<String, _>("created_at").parse()?,
                })
            })
            .collect()
    }

    pub async fn upsert_oidc_allowlist(
        &self,
        entry: &OidcAllowlistEntry,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO directory_oidc_allowlist(id,provider,identifier,user_id,created_at)
             VALUES(?,?,?,?,?)
             ON CONFLICT(provider, identifier) DO UPDATE SET user_id=excluded.user_id",
        )
        .bind(&entry.id)
        .bind(&entry.provider)
        .bind(&entry.identifier)
        .bind(&entry.user_id)
        .bind(entry.created_at.to_rfc3339())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn delete_oidc_allowlist(&self, id: &str) -> anyhow::Result<bool> {
        let result = sqlx::query("DELETE FROM directory_oidc_allowlist WHERE id=?")
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn oidc_allowlist_match(
        &self,
        provider: &str,
        identifiers: &[String],
    ) -> anyhow::Result<Option<OidcAllowlistEntry>> {
        let entries = self.oidc_allowlist().await?;
        let lowered: Vec<String> = identifiers.iter().map(|item| item.to_lowercase()).collect();
        Ok(entries.into_iter().find(|entry| {
            entry.provider == provider && lowered.iter().any(|item| item == &entry.identifier)
        }))
    }

    pub async fn oidc_identities_for_user(
        &self,
        user_id: &str,
    ) -> anyhow::Result<Vec<OidcIdentity>> {
        let rows = sqlx::query(
            "SELECT id,user_id,provider,subject,email,login FROM directory_oidc_identities WHERE user_id=? ORDER BY provider",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| OidcIdentity {
                id: row.get("id"),
                user_id: row.get("user_id"),
                provider: row.get("provider"),
                subject: row.get("subject"),
                email: row.get("email"),
                login: row.get("login"),
            })
            .collect())
    }

    pub async fn oidc_identity(
        &self,
        provider: &str,
        subject: &str,
    ) -> anyhow::Result<Option<OidcIdentity>> {
        Ok(sqlx::query(
            "SELECT id,user_id,provider,subject,email,login FROM directory_oidc_identities WHERE provider=? AND subject=?",
        )
        .bind(provider)
        .bind(subject)
        .fetch_optional(self.pool())
        .await?
        .map(|row| OidcIdentity {
            id: row.get("id"),
            user_id: row.get("user_id"),
            provider: row.get("provider"),
            subject: row.get("subject"),
            email: row.get("email"),
            login: row.get("login"),
        }))
    }

    pub async fn insert_oidc_identity(&self, identity: &OidcIdentity) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO directory_oidc_identities(id,user_id,provider,subject,email,login,created_at)
             VALUES(?,?,?,?,?,?,?)",
        )
        .bind(&identity.id)
        .bind(&identity.user_id)
        .bind(&identity.provider)
        .bind(&identity.subject)
        .bind(&identity.email)
        .bind(&identity.login)
        .bind(Utc::now().to_rfc3339())
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

fn row_to_group(row: sqlx::sqlite::SqliteRow) -> anyhow::Result<DirectoryGroup> {
    Ok(DirectoryGroup {
        id: row.get("id"),
        name: row.get("name"),
        created_at: row.get::<String, _>("created_at").parse()?,
        allowed_model_ids: decode_models(row.get("allowed_model_ids")).unwrap_or_default(),
        may_publish: row.get::<i64, _>("may_publish") != 0,
        may_admin: row.get::<i64, _>("may_admin") != 0,
    })
}

pub fn effective_permissions(user: &DirectoryUser, groups: &[DirectoryGroup]) -> EffectivePermissions {
    if user.disabled_at.is_some() {
        return EffectivePermissions::none();
    }
    if user.is_operator {
        return EffectivePermissions::all();
    }
    let membership: Vec<&DirectoryGroup> = groups
        .iter()
        .filter(|group| user.group_ids.iter().any(|id| id == &group.id))
        .collect();
    let mut inherited_models = BTreeSet::new();
    let mut inherited_publish = false;
    let mut inherited_admin = false;
    for group in membership {
        inherited_models.extend(group.allowed_model_ids.iter().cloned());
        inherited_publish |= group.may_publish;
        inherited_admin |= group.may_admin;
    }
    EffectivePermissions {
        allowed_model_ids: Some(
            user.allowed_model_ids
                .clone()
                .unwrap_or_else(|| inherited_models.into_iter().collect()),
        ),
        may_publish: user.may_publish.unwrap_or(inherited_publish),
        may_admin: user.may_admin.unwrap_or(inherited_admin),
    }
}

impl Store {
    pub async fn permissions_for(&self, user: &DirectoryUser) -> anyhow::Result<EffectivePermissions> {
        let groups = self.directory_groups().await?;
        Ok(effective_permissions(user, &groups))
    }
}

pub async fn bootstrap_operator(
    store: &Store,
    secrets: &dyn SecretStore,
) -> anyhow::Result<Option<String>> {
    if !store.directory_users().await?.is_empty() {
        return Ok(None);
    }
    let password = generate_local_token().replacen("lar_", "op_", 1);
    let user = DirectoryUser {
        id: Uuid::new_v4().to_string(),
        username: "operator".into(),
        display_name: "Operator".into(),
        is_operator: true,
        disabled_at: None,
        created_at: Utc::now(),
        group_ids: Vec::new(),
        allowed_model_ids: None,
        may_publish: None,
        may_admin: None,
        has_password: true,
    };
    store
        .insert_directory_user(&user, Some(&hash_password(&password)?))
        .await?;
    secrets.set(OPERATOR_BOOTSTRAP_ACCOUNT, &password)?;
    Ok(Some(password))
}

pub async fn create_user(store: &Store, input: CreateUserInput) -> anyhow::Result<DirectoryUser> {
    let username = normalize_username(&input.username)?;
    if store.directory_user_by_username(&username).await?.is_some() {
        anyhow::bail!("username already exists");
    }
    let display_name = input.display_name.trim();
    if display_name.is_empty() {
        anyhow::bail!("display name is required");
    }
    let password_hash = match input.password.as_deref().map(str::trim).filter(|value| !value.is_empty())
    {
        Some(password) => Some(hash_password(password)?),
        None => None,
    };
    let user = DirectoryUser {
        id: Uuid::new_v4().to_string(),
        username,
        display_name: display_name.to_owned(),
        is_operator: false,
        disabled_at: None,
        created_at: Utc::now(),
        group_ids: input.group_ids,
        allowed_model_ids: input.allowed_model_ids,
        may_publish: input.may_publish,
        may_admin: input.may_admin,
        has_password: password_hash.is_some(),
    };
    store
        .insert_directory_user(&user, password_hash.as_deref())
        .await?;
    store
        .directory_user(&user.id)
        .await?
        .context("created user missing")
}

pub async fn update_user(
    store: &Store,
    secrets: &dyn SecretStore,
    id: &str,
    input: UpdateUserInput,
) -> anyhow::Result<DirectoryUser> {
    let mut user = store
        .directory_user(id)
        .await?
        .context("user not found")?;
    if let Some(display_name) = input.display_name {
        let display_name = display_name.trim();
        if display_name.is_empty() {
            anyhow::bail!("display name is required");
        }
        user.display_name = display_name.to_owned();
    }
    if let Some(group_ids) = input.group_ids {
        user.group_ids = group_ids;
    }
    if input.inherit_models == Some(true) {
        user.allowed_model_ids = None;
    } else if let Some(allowed_model_ids) = input.allowed_model_ids {
        user.allowed_model_ids = Some(allowed_model_ids);
    }
    if input.inherit_publish == Some(true) {
        user.may_publish = None;
    } else if let Some(may_publish) = input.may_publish {
        user.may_publish = Some(may_publish);
    }
    if input.inherit_admin == Some(true) {
        user.may_admin = None;
    } else if let Some(may_admin) = input.may_admin {
        user.may_admin = Some(may_admin);
    }
    if let Some(disabled) = input.disabled {
        if disabled && user.is_operator {
            let enabled_operators = store
                .directory_users()
                .await?
                .into_iter()
                .filter(|item| item.is_operator && item.disabled_at.is_none() && item.id != user.id)
                .count();
            if enabled_operators == 0 {
                anyhow::bail!("the last operator cannot be disabled");
            }
        }
        user.disabled_at = disabled.then(Utc::now);
        if disabled {
            // drop sessions by deleting all for this user via token table scan isn't needed;
            sqlx::query("DELETE FROM directory_sessions WHERE user_id=?")
                .bind(&user.id)
                .execute(store.pool())
                .await?;
        }
    }
    let mut replace_password = false;
    let mut password_hash = None;
    if let Some(password) = input.password {
        let password = password.trim();
        if password.is_empty() {
            anyhow::bail!("password is required");
        }
        password_hash = Some(hash_password(password)?);
        replace_password = true;
        if user.is_operator {
            let _ = secrets.delete(OPERATOR_BOOTSTRAP_ACCOUNT);
        }
    }
    store
        .update_directory_user(&user, password_hash.as_deref(), replace_password)
        .await?;
    store
        .directory_user(&user.id)
        .await?
        .context("updated user missing")
}

pub async fn upsert_group(store: &Store, id: Option<String>, input: UpsertGroupInput) -> anyhow::Result<DirectoryGroup> {
    let name = normalize_group_name(&input.name)?;
    let existing = if let Some(id) = id.as_deref() {
        store.directory_group(id).await?
    } else {
        None
    };
    let group = DirectoryGroup {
        id: existing
            .as_ref()
            .map(|item| item.id.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        name,
        created_at: existing
            .map(|item| item.created_at)
            .unwrap_or_else(Utc::now),
        allowed_model_ids: input.allowed_model_ids,
        may_publish: input.may_publish,
        may_admin: input.may_admin,
    };
    store.upsert_directory_group(&group).await?;
    Ok(group)
}

pub async fn login_with_password(
    store: &Store,
    username: &str,
    password: &str,
) -> anyhow::Result<(DirectoryUser, String)> {
    let username = normalize_username(username)?;
    let user = store
        .directory_user_by_username(&username)
        .await?
        .context("invalid username or password")?;
    if user.disabled_at.is_some() {
        anyhow::bail!("this account is disabled");
    }
    let hash = store
        .user_password_hash(&user.id)
        .await?
        .context("invalid username or password")?;
    if !verify_password(&hash, password) {
        anyhow::bail!("invalid username or password");
    }
    let token = create_session(store, &user.id).await?;
    Ok((user, token))
}

pub async fn create_session(store: &Store, user_id: &str) -> anyhow::Result<String> {
    let token = generate_local_token();
    store
        .insert_session(
            user_id,
            &session_token_hash(&token),
            Utc::now() + Duration::days(SESSION_TTL_DAYS),
        )
        .await?;
    Ok(token)
}

pub async fn user_for_session(store: &Store, token: &str) -> anyhow::Result<Option<DirectoryUser>> {
    let user_id = store.session_user_id(&session_token_hash(token)).await?;
    match user_id {
        Some(id) => store.directory_user(&id).await,
        None => Ok(None),
    }
}

pub async fn revoke_session(store: &Store, token: &str) -> anyhow::Result<()> {
    store
        .delete_sessions_for_token(&session_token_hash(token))
        .await
}

pub fn parse_session_cookie(header: Option<&str>) -> Option<String> {
    header.and_then(|cookie| {
        cookie.split(';').find_map(|part| {
            let part = part.trim();
            part.strip_prefix(&format!("{SESSION_COOKIE}="))
                .map(|value| value.to_owned())
        })
    })
}

pub fn set_cookie_header(token: &str, secure: bool) -> String {
    let mut cookie = format!(
        "{SESSION_COOKIE}={token}; HttpOnly; Path=/; SameSite=Lax; Max-Age={}",
        SESSION_TTL_DAYS * 24 * 60 * 60
    );
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

pub fn clear_cookie_header(secure: bool) -> String {
    let mut cookie = format!("{SESSION_COOKIE}=; HttpOnly; Path=/; SameSite=Lax; Max-Age=0");
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

pub async fn invite_oidc(
    store: &Store,
    provider: &str,
    identifier: &str,
    user_id: Option<String>,
) -> anyhow::Result<OidcAllowlistEntry> {
    let provider = normalize_oidc_provider(provider)?;
    let identifier = identifier.trim().to_lowercase();
    if identifier.is_empty() {
        anyhow::bail!("allowlist identifier is required");
    }
    if let Some(user_id) = user_id.as_deref() {
        if store.directory_user(user_id).await?.is_none() {
            anyhow::bail!("user not found");
        }
    }
    let entry = OidcAllowlistEntry {
        id: Uuid::new_v4().to_string(),
        provider,
        identifier,
        user_id,
        created_at: Utc::now(),
    };
    store.upsert_oidc_allowlist(&entry).await?;
    Ok(entry)
}

async fn ensure_oidc_still_invited(
    store: &Store,
    provider: &str,
    user_id: &str,
    email: Option<&str>,
    login: Option<&str>,
    stored_email: Option<&str>,
    stored_login: Option<&str>,
) -> anyhow::Result<()> {
    let mut identifiers = Vec::new();
    for value in [email, login, stored_email, stored_login]
        .into_iter()
        .flatten()
    {
        identifiers.push(value.to_lowercase());
    }
    if store
        .oidc_allowlist_match(provider, &identifiers)
        .await?
        .is_some()
    {
        return Ok(());
    }
    let linked = store.oidc_allowlist().await?.into_iter().any(|entry| {
        entry.provider == provider && entry.user_id.as_deref() == Some(user_id)
    });
    if linked {
        return Ok(());
    }
    anyhow::bail!("this identity is not invited")
}

pub async fn complete_oidc_login(
    store: &Store,
    provider: &str,
    subject: &str,
    email: Option<&str>,
    login: Option<&str>,
) -> anyhow::Result<(DirectoryUser, String)> {
    let provider = normalize_oidc_provider(provider)?;
    if let Some(existing) = store.oidc_identity(&provider, subject).await? {
        let user = store
            .directory_user(&existing.user_id)
            .await?
            .context("linked user missing")?;
        if user.disabled_at.is_some() {
            anyhow::bail!("this account is disabled");
        }
        ensure_oidc_still_invited(store, &provider, &user.id, email, login, existing.email.as_deref(), existing.login.as_deref()).await?;
        let token = create_session(store, &user.id).await?;
        return Ok((user, token));
    }
    let mut identifiers = Vec::new();
    if let Some(email) = email {
        identifiers.push(email.to_lowercase());
    }
    if let Some(login) = login {
        identifiers.push(login.to_lowercase());
    }
    let invite = store
        .oidc_allowlist_match(&provider, &identifiers)
        .await?
        .context("this identity is not invited")?;
    let user = if let Some(user_id) = invite.user_id.as_deref() {
        store
            .directory_user(user_id)
            .await?
            .context("invited user missing")?
    } else {
        let username = unique_oidc_username(
            store,
            login.or(email).unwrap_or(subject),
        )
        .await?;
        create_user(
            store,
            CreateUserInput {
                username,
                display_name: login.or(email).unwrap_or(subject).to_owned(),
                password: None,
                group_ids: Vec::new(),
                allowed_model_ids: None,
                may_publish: None,
                may_admin: None,
            },
        )
        .await?
    };
    if user.disabled_at.is_some() {
        anyhow::bail!("this account is disabled");
    }
    store
        .insert_oidc_identity(&OidcIdentity {
            id: Uuid::new_v4().to_string(),
            user_id: user.id.clone(),
            provider,
            subject: subject.to_owned(),
            email: email.map(ToOwned::to_owned),
            login: login.map(ToOwned::to_owned),
        })
        .await?;
    let token = create_session(store, &user.id).await?;
    Ok((user, token))
}

fn normalize_oidc_provider(provider: &str) -> anyhow::Result<String> {
    match provider.trim().to_lowercase().as_str() {
        "github" | "google" => Ok(provider.trim().to_lowercase()),
        _ => anyhow::bail!("unsupported OpenID provider"),
    }
}

async fn unique_oidc_username(store: &Store, seed: &str) -> anyhow::Result<String> {
    let mut base: String = seed
        .split('@')
        .next()
        .unwrap_or("user")
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    base = base.trim_matches('-').to_owned();
    if base.is_empty() {
        base = "user".into();
    }
    base.truncate(32);
    if store.directory_user_by_username(&base).await?.is_none() {
        return Ok(base);
    }
    for index in 2..1000 {
        let candidate = format!("{base}-{index}");
        if store.directory_user_by_username(&candidate).await?.is_none() {
            return Ok(candidate);
        }
    }
    anyhow::bail!("unable to allocate a username")
}

pub fn github_authorization_url(
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
) -> anyhow::Result<String> {
    let mut url = url::Url::parse("https://github.com/login/oauth/authorize")?;
    url.query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", "read:user user:email")
        .append_pair("state", state)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(url.to_string())
}

pub fn google_authorization_url(
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
) -> anyhow::Result<String> {
    let mut url = url::Url::parse("https://accounts.google.com/o/oauth2/v2/auth")?;
    url.query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", "openid email profile")
        .append_pair("state", state)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(url.to_string())
}

pub fn oidc_secret_account(provider: &str) -> String {
    format!("oidc:{provider}")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OidcClientConfig {
    pub client_id: String,
    pub client_secret: String,
}

pub fn parse_oidc_config(raw: &str) -> anyhow::Result<OidcClientConfig> {
    serde_json::from_str(raw).context("invalid OpenID client configuration")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemorySecrets;
    use std::sync::Arc;

    async fn memory() -> Store {
        Store::memory().await.unwrap()
    }

    #[tokio::test]
    async fn first_run_creates_a_local_operator_account() {
        let store = memory().await;
        let secrets = MemorySecrets::default();
        let password = bootstrap_operator(&store, &secrets).await.unwrap().unwrap();
        let again = bootstrap_operator(&store, &secrets).await.unwrap();
        assert!(again.is_none());
        let users = store.directory_users().await.unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].username, "operator");
        assert!(users[0].is_operator);
        let (user, token) = login_with_password(&store, "operator", &password)
            .await
            .unwrap();
        assert!(user.is_operator);
        assert!(user_for_session(&store, &token).await.unwrap().is_some());
        assert_eq!(
            secrets.get(OPERATOR_BOOTSTRAP_ACCOUNT).unwrap().as_deref(),
            Some(password.as_str())
        );
    }

    #[tokio::test]
    async fn directory_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("router.sqlite3");
        let secrets = MemorySecrets::default();
        {
            let store = Store::open(&path).await.unwrap();
            bootstrap_operator(&store, &secrets).await.unwrap();
            create_user(
                &store,
                CreateUserInput {
                    username: "alice".into(),
                    display_name: "Alice".into(),
                    password: Some("secret-pass".into()),
                    group_ids: Vec::new(),
                    allowed_model_ids: None,
                    may_publish: None,
                    may_admin: None,
                },
            )
            .await
            .unwrap();
        }
        let store = Store::open(&path).await.unwrap();
        let users = store.directory_users().await.unwrap();
        assert_eq!(users.len(), 2);
        assert!(users.iter().any(|user| user.username == "alice"));
        login_with_password(&store, "alice", "secret-pass")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn groups_grant_models_and_user_overrides_win() {
        let store = memory().await;
        let group = upsert_group(
            &store,
            None,
            UpsertGroupInput {
                name: "readers".into(),
                allowed_model_ids: vec!["qwen".into(), "llama".into()],
                may_publish: true,
                may_admin: false,
            },
        )
        .await
        .unwrap();
        let alice = create_user(
            &store,
            CreateUserInput {
                username: "alice".into(),
                display_name: "Alice".into(),
                password: Some("pw".into()),
                group_ids: vec![group.id.clone()],
                allowed_model_ids: None,
                may_publish: None,
                may_admin: None,
            },
        )
        .await
        .unwrap();
        let inherited = store.permissions_for(&alice).await.unwrap();
        assert!(inherited.allows_model("qwen"));
        assert!(inherited.allows_model("llama"));
        assert!(!inherited.allows_model("gpt"));
        assert!(inherited.may_publish);
        assert!(!inherited.may_admin);

        let alice = update_user(
            &store,
            &MemorySecrets::default(),
            &alice.id,
            UpdateUserInput {
                display_name: None,
                password: None,
                group_ids: None,
                allowed_model_ids: Some(vec!["gpt".into()]),
                inherit_models: None,
                may_publish: Some(false),
                inherit_publish: None,
                may_admin: Some(true),
                inherit_admin: None,
                disabled: None,
            },
        )
        .await
        .unwrap();
        let overridden = store.permissions_for(&alice).await.unwrap();
        assert!(!overridden.allows_model("qwen"));
        assert!(overridden.allows_model("gpt"));
        assert!(!overridden.may_publish);
        assert!(overridden.may_admin);
    }

    #[tokio::test]
    async fn disabled_users_and_ungrouped_users_are_denied() {
        let store = memory().await;
        let secrets = MemorySecrets::default();
        let bob = create_user(
            &store,
            CreateUserInput {
                username: "bob".into(),
                display_name: "Bob".into(),
                password: Some("pw".into()),
                group_ids: Vec::new(),
                allowed_model_ids: None,
                may_publish: None,
                may_admin: None,
            },
        )
        .await
        .unwrap();
        let denied = store.permissions_for(&bob).await.unwrap();
        assert!(!denied.allows_model("qwen"));
        assert!(!denied.may_admin);
        assert!(!denied.may_publish);

        update_user(
            &store,
            &secrets,
            &bob.id,
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
                disabled: Some(true),
            },
        )
        .await
        .unwrap();
        assert!(login_with_password(&store, "bob", "pw").await.is_err());
        let bob = store.directory_user(&bob.id).await.unwrap().unwrap();
        let disabled = store.permissions_for(&bob).await.unwrap();
        assert!(!disabled.allows_model("qwen"));
        assert!(!disabled.may_admin);
    }

    #[tokio::test]
    async fn operator_keeps_full_access_and_cannot_be_the_last_disabled() {
        let store = memory().await;
        let secrets = MemorySecrets::default();
        bootstrap_operator(&store, &secrets).await.unwrap();
        let operator = store.directory_user_by_username("operator").await.unwrap().unwrap();
        let perms = store.permissions_for(&operator).await.unwrap();
        assert!(perms.allows_model("anything"));
        assert!(perms.may_admin);
        let error = update_user(
            &store,
            &secrets,
            &operator.id,
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
                disabled: Some(true),
            },
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("last operator"));
    }

    #[tokio::test]
    async fn unknown_oidc_identity_is_rejected_until_allowlisted() {
        let store = memory().await;
        let error = complete_oidc_login(
            &store,
            "google",
            "sub-1",
            Some("alice@example.com"),
            None,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("not invited"));
        invite_oidc(&store, "google", "alice@example.com", None)
            .await
            .unwrap();
        let (user, token) = complete_oidc_login(
            &store,
            "google",
            "sub-1",
            Some("alice@example.com"),
            None,
        )
        .await
        .unwrap();
        assert_eq!(user.username, "alice");
        assert!(user_for_session(&store, &token).await.unwrap().is_some());
        let again = complete_oidc_login(
            &store,
            "google",
            "sub-1",
            Some("alice@example.com"),
            None,
        )
        .await
        .unwrap();
        assert_eq!(again.0.id, user.id);
        let invite = store
            .oidc_allowlist()
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        store.delete_oidc_allowlist(&invite.id).await.unwrap();
        let denied = complete_oidc_login(
            &store,
            "google",
            "sub-1",
            Some("alice@example.com"),
            None,
        )
        .await
        .unwrap_err();
        assert!(denied.to_string().contains("not invited"));
    }

    #[tokio::test]
    async fn oidc_login_maps_onto_an_existing_grouped_user() {
        let store = memory().await;
        let group = upsert_group(
            &store,
            None,
            UpsertGroupInput {
                name: "readers".into(),
                allowed_model_ids: vec!["qwen".into()],
                may_publish: false,
                may_admin: false,
            },
        )
        .await
        .unwrap();
        let alice = create_user(
            &store,
            CreateUserInput {
                username: "alice".into(),
                display_name: "Alice".into(),
                password: None,
                group_ids: vec![group.id],
                allowed_model_ids: None,
                may_publish: None,
                may_admin: None,
            },
        )
        .await
        .unwrap();
        invite_oidc(
            &store,
            "google",
            "alice@example.com",
            Some(alice.id.clone()),
        )
        .await
        .unwrap();
        let (user, _) = complete_oidc_login(
            &store,
            "google",
            "sub-9",
            Some("alice@example.com"),
            None,
        )
        .await
        .unwrap();
        assert_eq!(user.id, alice.id);
        let perms = store.permissions_for(&user).await.unwrap();
        assert!(perms.allows_model("qwen"));
        assert!(!perms.may_admin);
    }

    #[tokio::test]
    async fn local_api_keys_are_not_stored_on_directory_users() {
        let store = memory().await;
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        bootstrap_operator(&store, secrets.as_ref()).await.unwrap();
        assert!(store.local_api_keys().await.unwrap().is_empty());
        let users = store.directory_users().await.unwrap();
        assert!(users.iter().all(|user| user.username != "lar_"));
    }
}

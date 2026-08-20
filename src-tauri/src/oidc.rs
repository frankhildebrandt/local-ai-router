use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use reqwest::Client;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::{
    identity::{
        complete_oidc_login, github_authorization_url, google_authorization_url,
        oidc_secret_account, parse_oidc_config, DirectoryUser,
    },
    secrets::SecretStore,
    storage::Store,
};

#[derive(Debug, Clone)]
pub struct OidcEndpoints {
    pub github_token_url: String,
    pub github_user_url: String,
    pub github_emails_url: String,
    pub google_token_url: String,
    pub google_userinfo_url: String,
}

impl Default for OidcEndpoints {
    fn default() -> Self {
        Self {
            github_token_url: "https://github.com/login/oauth/access_token".into(),
            github_user_url: "https://api.github.com/user".into(),
            github_emails_url: "https://api.github.com/user/emails".into(),
            google_token_url: "https://oauth2.googleapis.com/token".into(),
            google_userinfo_url: "https://openidconnect.googleapis.com/userinfo".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct OidcStart {
    pub authorization_url: String,
}

struct PendingFlow {
    provider: String,
    verifier: String,
    redirect_uri: String,
}

#[derive(Clone)]
pub struct OidcManager {
    client: Client,
    secrets: Arc<dyn SecretStore>,
    endpoints: OidcEndpoints,
    pending: Arc<Mutex<HashMap<String, PendingFlow>>>,
}

impl OidcManager {
    pub fn new(client: Client, secrets: Arc<dyn SecretStore>) -> Self {
        Self::with_endpoints(client, secrets, OidcEndpoints::default())
    }

    pub fn with_endpoints(
        client: Client,
        secrets: Arc<dyn SecretStore>,
        endpoints: OidcEndpoints,
    ) -> Self {
        Self {
            client,
            secrets,
            endpoints,
            pending: Default::default(),
        }
    }

    pub fn configured_providers(&self) -> Vec<String> {
        ["github", "google"]
            .into_iter()
            .filter(|provider| {
                self.secrets
                    .get(&oidc_secret_account(provider))
                    .ok()
                    .flatten()
                    .is_some()
            })
            .map(str::to_owned)
            .collect()
    }

    pub async fn begin(&self, provider: &str, redirect_uri: &str) -> anyhow::Result<OidcStart> {
        let config_raw = self
            .secrets
            .get(&oidc_secret_account(provider))?
            .context("OpenID client is not configured")?;
        let config = parse_oidc_config(&config_raw)?;
        let verifier = random_urlsafe(48);
        let state = random_urlsafe(32);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let authorization_url = match provider {
            "github" => {
                github_authorization_url(&config.client_id, redirect_uri, &state, &challenge)?
            }
            "google" => {
                google_authorization_url(&config.client_id, redirect_uri, &state, &challenge)?
            }
            _ => anyhow::bail!("unsupported OpenID provider"),
        };
        self.pending.lock().await.insert(
            state,
            PendingFlow {
                provider: provider.to_owned(),
                verifier,
                redirect_uri: redirect_uri.to_owned(),
            },
        );
        Ok(OidcStart { authorization_url })
    }

    pub async fn finish(
        &self,
        store: &Store,
        code: &str,
        state: &str,
    ) -> anyhow::Result<(DirectoryUser, String)> {
        let flow = self
            .pending
            .lock()
            .await
            .remove(state)
            .context("OpenID sign-in expired or is invalid")?;
        let config_raw = self
            .secrets
            .get(&oidc_secret_account(&flow.provider))?
            .context("OpenID client is not configured")?;
        let config = parse_oidc_config(&config_raw)?;
        match flow.provider.as_str() {
            "github" => {
                let token = self
                    .exchange_github(&config.client_id, &config.client_secret, code, &flow)
                    .await?;
                let (subject, email, login) = self.github_profile(&token).await?;
                complete_oidc_login(store, "github", &subject, email.as_deref(), Some(&login)).await
            }
            "google" => {
                let token = self
                    .exchange_google(&config.client_id, &config.client_secret, code, &flow)
                    .await?;
                let (subject, email) = self.google_profile(&token).await?;
                complete_oidc_login(store, "google", &subject, email.as_deref(), None).await
            }
            _ => anyhow::bail!("unsupported OpenID provider"),
        }
    }

    async fn exchange_github(
        &self,
        client_id: &str,
        client_secret: &str,
        code: &str,
        flow: &PendingFlow,
    ) -> anyhow::Result<String> {
        let response = self
            .client
            .post(&self.endpoints.github_token_url)
            .header("Accept", "application/json")
            .json(&serde_json::json!({
                "client_id": client_id,
                "client_secret": client_secret,
                "code": code,
                "redirect_uri": flow.redirect_uri,
                "code_verifier": flow.verifier,
            }))
            .send()
            .await?
            .error_for_status()?;
        let body: Value = response.json().await?;
        body["access_token"]
            .as_str()
            .map(ToOwned::to_owned)
            .context("GitHub token response missing access_token")
    }

    async fn github_profile(
        &self,
        token: &str,
    ) -> anyhow::Result<(String, Option<String>, String)> {
        let user: Value = self
            .client
            .get(&self.endpoints.github_user_url)
            .header("Authorization", format!("Bearer {token}"))
            .header("User-Agent", "local-ai-router")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let login = user["login"]
            .as_str()
            .context("GitHub user missing login")?
            .to_owned();
        let subject = user["id"]
            .as_i64()
            .map(|id| id.to_string())
            .or_else(|| user["id"].as_str().map(ToOwned::to_owned))
            .context("GitHub user missing id")?;
        let mut email = user["email"].as_str().map(ToOwned::to_owned);
        if email.is_none() {
            if let Ok(emails) = self
                .client
                .get(&self.endpoints.github_emails_url)
                .header("Authorization", format!("Bearer {token}"))
                .header("User-Agent", "local-ai-router")
                .send()
                .await
            {
                if let Ok(list) = emails.json::<Vec<Value>>().await {
                    email = list
                        .iter()
                        .find(|item| item["primary"].as_bool() == Some(true))
                        .or_else(|| list.first())
                        .and_then(|item| item["email"].as_str().map(ToOwned::to_owned));
                }
            }
        }
        Ok((subject, email, login))
    }

    async fn exchange_google(
        &self,
        client_id: &str,
        client_secret: &str,
        code: &str,
        flow: &PendingFlow,
    ) -> anyhow::Result<String> {
        let response = self
            .client
            .post(&self.endpoints.google_token_url)
            .form(&[
                ("client_id", client_id),
                ("client_secret", client_secret),
                ("code", code),
                ("redirect_uri", flow.redirect_uri.as_str()),
                ("grant_type", "authorization_code"),
                ("code_verifier", flow.verifier.as_str()),
            ])
            .send()
            .await?
            .error_for_status()?;
        let body: Value = response.json().await?;
        body["access_token"]
            .as_str()
            .map(ToOwned::to_owned)
            .context("Google token response missing access_token")
    }

    async fn google_profile(&self, token: &str) -> anyhow::Result<(String, Option<String>)> {
        let user: Value = self
            .client
            .get(&self.endpoints.google_userinfo_url)
            .bearer_auth(token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let subject = user["sub"]
            .as_str()
            .context("Google userinfo missing sub")?
            .to_owned();
        let email = user["email"].as_str().map(ToOwned::to_owned);
        Ok((subject, email))
    }
}

fn random_urlsafe(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn save_oidc_client(
    secrets: &dyn SecretStore,
    provider: &str,
    client_id: &str,
    client_secret: &str,
) -> anyhow::Result<()> {
    if !matches!(provider, "github" | "google") {
        anyhow::bail!("unsupported OpenID provider");
    }
    let client_id = client_id.trim();
    let client_secret = client_secret.trim();
    if client_id.is_empty() {
        secrets.delete(&oidc_secret_account(provider))?;
        return Ok(());
    }
    if client_secret.is_empty() {
        anyhow::bail!("client secret is required");
    }
    secrets.set(
        &oidc_secret_account(provider),
        &serde_json::to_string(&serde_json::json!({
            "client_id": client_id,
            "client_secret": client_secret,
        }))?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        identity::{bootstrap_operator, invite_oidc},
        secrets::MemorySecrets,
        storage::Store,
    };
    use axum::{
        extract::Json,
        http::StatusCode,
        response::IntoResponse,
        routing::{get, post},
        Router,
    };

    async fn github_token() -> impl IntoResponse {
        Json(serde_json::json!({ "access_token": "gh-token" }))
    }

    async fn github_user() -> impl IntoResponse {
        Json(serde_json::json!({ "id": 42, "login": "alice", "email": "alice@example.com" }))
    }

    async fn google_token() -> impl IntoResponse {
        Json(serde_json::json!({ "access_token": "g-token" }))
    }

    async fn google_user() -> impl IntoResponse {
        Json(serde_json::json!({ "sub": "sub-9", "email": "bob@example.com" }))
    }

    async fn start_idp() -> (String, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/github/token", post(github_token))
            .route("/github/user", get(github_user))
            .route("/google/token", post(google_token))
            .route("/google/user", get(google_user))
            .route("/deny", post(|| async { StatusCode::UNAUTHORIZED }));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base = format!("http://{addr}");
        (base.clone(), base)
    }

    #[tokio::test]
    async fn oidc_maps_an_allowlisted_github_account_to_a_session() {
        let (base, _) = start_idp().await;
        let store = Store::memory().await.unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        bootstrap_operator(&store, secrets.as_ref()).await.unwrap();
        invite_oidc(&store, "github", "alice", None).await.unwrap();
        save_oidc_client(secrets.as_ref(), "github", "id", "secret").unwrap();
        let manager = OidcManager::with_endpoints(
            Client::new(),
            secrets,
            OidcEndpoints {
                github_token_url: format!("{base}/github/token"),
                github_user_url: format!("{base}/github/user"),
                github_emails_url: format!("{base}/github/emails"),
                google_token_url: format!("{base}/google/token"),
                google_userinfo_url: format!("{base}/google/user"),
            },
        );
        let start = manager
            .begin("github", "http://127.0.0.1/auth/oidc/callback")
            .await
            .unwrap();
        let state = url::Url::parse(&start.authorization_url)
            .unwrap()
            .query_pairs()
            .find(|(key, _)| key == "state")
            .unwrap()
            .1
            .into_owned();
        let (user, token) = manager.finish(&store, "code", &state).await.unwrap();
        assert_eq!(user.username, "alice");
        assert!(!token.is_empty());
    }

    #[tokio::test]
    async fn local_operator_login_still_works_when_oidc_is_configured() {
        let store = Store::memory().await.unwrap();
        let secrets = MemorySecrets::default();
        let password = bootstrap_operator(&store, &secrets).await.unwrap().unwrap();
        save_oidc_client(&secrets, "google", "id", "secret").unwrap();
        crate::identity::login_with_password(&store, "operator", &password)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn oidc_without_allowlist_never_creates_a_user() {
        let (base, _) = start_idp().await;
        let store = Store::memory().await.unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        save_oidc_client(secrets.as_ref(), "google", "id", "secret").unwrap();
        let manager = OidcManager::with_endpoints(
            Client::new(),
            secrets,
            OidcEndpoints {
                github_token_url: format!("{base}/github/token"),
                github_user_url: format!("{base}/github/user"),
                github_emails_url: format!("{base}/github/emails"),
                google_token_url: format!("{base}/google/token"),
                google_userinfo_url: format!("{base}/google/user"),
            },
        );
        let start = manager
            .begin("google", "http://127.0.0.1/auth/oidc/callback")
            .await
            .unwrap();
        let state = url::Url::parse(&start.authorization_url)
            .unwrap()
            .query_pairs()
            .find(|(key, _)| key == "state")
            .unwrap()
            .1
            .into_owned();
        let error = manager.finish(&store, "code", &state).await.unwrap_err();
        assert!(error.to_string().contains("not invited"));
        assert!(store.directory_users().await.unwrap().is_empty());
    }
}

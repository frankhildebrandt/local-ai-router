use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::Context;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, TimeDelta, Utc};
use rand::RngCore;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Mutex,
};
use url::Url;

use crate::secrets::{provider_account, SecretStore};

const OPENAI_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

#[derive(Debug, Clone)]
pub struct OAuthConfig {
    pub authorization_url: String,
    pub token_url: String,
    pub client_id: String,
    pub timeout: Duration,
}

impl Default for OAuthConfig {
    fn default() -> Self {
        Self {
            authorization_url: "https://auth.openai.com/oauth/authorize".into(),
            token_url: "https://auth.openai.com/oauth/token".into(),
            client_id: OPENAI_CLIENT_ID.into(),
            timeout: Duration::from_secs(180),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionCredential {
    pub version: u8,
    #[serde(rename = "type")]
    pub credential_type: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: DateTime<Utc>,
    pub account_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OAuthStart {
    pub authorization_url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OAuthState {
    Disconnected,
    Waiting,
    Connected,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct OAuthStatus {
    pub state: OAuthState,
    pub account_id: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct OAuthManager {
    client: Client,
    secrets: Arc<dyn SecretStore>,
    config: OAuthConfig,
    pending: Arc<Mutex<HashMap<String, OAuthStatus>>>,
    flows: Arc<Mutex<HashMap<String, String>>>,
    refresh_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

impl OAuthManager {
    pub fn new(client: Client, secrets: Arc<dyn SecretStore>) -> Self {
        Self::with_config(client, secrets, OAuthConfig::default())
    }

    pub fn with_config(client: Client, secrets: Arc<dyn SecretStore>, config: OAuthConfig) -> Self {
        Self {
            client,
            secrets,
            config,
            pending: Default::default(),
            flows: Default::default(),
            refresh_locks: Default::default(),
        }
    }

    pub async fn begin(&self, provider_id: &str) -> anyhow::Result<OAuthStart> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let redirect_uri = format!("http://127.0.0.1:{port}/auth/callback");
        let verifier = random_urlsafe(48);
        let state = random_urlsafe(32);
        let flow_id = random_urlsafe(24);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut authorization = Url::parse(&self.config.authorization_url)?;
        authorization
            .query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.config.client_id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("scope", "openid profile email offline_access")
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state);
        self.pending.lock().await.insert(
            provider_id.into(),
            OAuthStatus {
                state: OAuthState::Waiting,
                account_id: None,
                error: None,
            },
        );
        self.flows
            .lock()
            .await
            .insert(provider_id.into(), flow_id.clone());

        let manager = self.clone();
        let provider_id = provider_id.to_owned();
        tokio::spawn(async move {
            let result = tokio::time::timeout(
                manager.config.timeout,
                manager.handle_callback(
                    listener,
                    &provider_id,
                    &flow_id,
                    &state,
                    &verifier,
                    &redirect_uri,
                ),
            )
            .await;
            let status = match result {
                Ok(Ok(credential)) => OAuthStatus {
                    state: OAuthState::Connected,
                    account_id: credential.account_id,
                    error: None,
                },
                Ok(Err(error)) => OAuthStatus {
                    state: OAuthState::Error,
                    account_id: None,
                    error: Some(error.to_string()),
                },
                Err(_) => OAuthStatus {
                    state: OAuthState::Error,
                    account_id: None,
                    error: Some("OAuth callback timed out".into()),
                },
            };
            if manager.flows.lock().await.get(&provider_id) == Some(&flow_id) {
                manager.pending.lock().await.insert(provider_id, status);
            }
        });
        Ok(OAuthStart {
            authorization_url: authorization.to_string(),
        })
    }

    async fn handle_callback(
        &self,
        listener: TcpListener,
        provider_id: &str,
        flow_id: &str,
        expected_state: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> anyhow::Result<SubscriptionCredential> {
        let (mut socket, _) = listener.accept().await?;
        let mut bytes = Vec::with_capacity(2048);
        loop {
            let mut chunk = [0u8; 1024];
            let read = socket.read(&mut chunk).await?;
            if read == 0 {
                anyhow::bail!("OAuth callback ended before HTTP headers completed");
            }
            bytes.extend_from_slice(&chunk[..read]);
            if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
            if bytes.len() > 16 * 1024 {
                anyhow::bail!("OAuth callback headers are too large");
            }
        }
        let first_line = std::str::from_utf8(&bytes)?
            .lines()
            .next()
            .context("invalid OAuth callback")?;
        let path = first_line
            .split_whitespace()
            .nth(1)
            .context("invalid OAuth callback path")?;
        let callback = Url::parse(&format!("http://127.0.0.1{path}"))?;
        let params = callback.query_pairs().collect::<HashMap<_, _>>();
        let result = if params.get("state").map(|value| value.as_ref()) != Some(expected_state) {
            Err(anyhow::anyhow!("OAuth state did not match"))
        } else if let Some(error) = params.get("error") {
            Err(anyhow::anyhow!("OAuth authorization failed: {error}"))
        } else {
            let code = params
                .get("code")
                .context("OAuth callback did not include a code")?;
            let credential = self.exchange(code, verifier, redirect_uri).await?;
            let flows = self.flows.lock().await;
            if flows.get(provider_id).map(String::as_str) != Some(flow_id) {
                anyhow::bail!("OAuth flow was superseded by a newer login");
            }
            self.store_credential(provider_id, &credential)?;
            Ok(credential)
        };
        let (status, message) = if result.is_ok() {
            ("200 OK", "Authentication complete. You can close this tab.")
        } else {
            (
                "400 Bad Request",
                "Authentication failed. Return to Local AI Router for details.",
            )
        };
        let response = format!("HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{message}", message.len());
        let _ = socket.write_all(response.as_bytes()).await;
        result
    }

    async fn exchange(
        &self,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> anyhow::Result<SubscriptionCredential> {
        let response = self
            .client
            .post(&self.config.token_url)
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", self.config.client_id.as_str()),
                ("code", code),
                ("code_verifier", verifier),
                ("redirect_uri", redirect_uri),
            ])
            .send()
            .await?;
        let status = response.status();
        let token: TokenResponse = response
            .json()
            .await
            .context("OAuth issuer returned invalid JSON")?;
        if !status.is_success() {
            anyhow::bail!("OAuth token exchange returned {status}");
        }
        token.into_credential(None)
    }

    pub async fn status(&self, provider_id: &str) -> anyhow::Result<OAuthStatus> {
        if let Some(status) = self.pending.lock().await.get(provider_id).cloned() {
            if matches!(status.state, OAuthState::Waiting | OAuthState::Error) {
                return Ok(status);
            }
        }
        if let Some(credential) = self.load_credential(provider_id)? {
            return Ok(OAuthStatus {
                state: OAuthState::Connected,
                account_id: credential.account_id,
                error: None,
            });
        }
        Ok(self
            .pending
            .lock()
            .await
            .get(provider_id)
            .cloned()
            .unwrap_or(OAuthStatus {
                state: OAuthState::Disconnected,
                account_id: None,
                error: None,
            }))
    }

    pub async fn access_token(&self, provider_id: &str) -> anyhow::Result<SubscriptionCredential> {
        let lock = {
            let mut locks = self.refresh_locks.lock().await;
            locks
                .entry(provider_id.into())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = lock.lock().await;
        let current = self
            .load_credential(provider_id)?
            .context("subscription credential missing")?;
        if current.expires_at > Utc::now() + TimeDelta::seconds(60) {
            return Ok(current);
        }
        let response = self
            .client
            .post(&self.config.token_url)
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", self.config.client_id.as_str()),
                ("refresh_token", current.refresh_token.as_str()),
            ])
            .send()
            .await?;
        let status = response.status();
        let token: TokenResponse = response
            .json()
            .await
            .context("OAuth refresh returned invalid JSON")?;
        if !status.is_success() {
            anyhow::bail!("OAuth refresh returned {status}");
        }
        let refreshed = token.into_credential(Some(&current))?;
        self.store_credential(provider_id, &refreshed)?;
        Ok(refreshed)
    }

    pub async fn logout(&self, provider_id: &str) -> anyhow::Result<()> {
        self.secrets.delete(&provider_account(provider_id))?;
        self.pending.lock().await.remove(provider_id);
        self.flows.lock().await.remove(provider_id);
        Ok(())
    }

    fn load_credential(&self, provider_id: &str) -> anyhow::Result<Option<SubscriptionCredential>> {
        self.secrets
            .get(&provider_account(provider_id))?
            .map(|stored| {
                serde_json::from_str(&stored).context("invalid subscription credential in Keychain")
            })
            .transpose()
    }

    fn store_credential(
        &self,
        provider_id: &str,
        credential: &SubscriptionCredential,
    ) -> anyhow::Result<()> {
        self.secrets.set(
            &provider_account(provider_id),
            &serde_json::to_string(credential)?,
        )
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    id_token: Option<String>,
}

impl TokenResponse {
    fn into_credential(
        self,
        previous: Option<&SubscriptionCredential>,
    ) -> anyhow::Result<SubscriptionCredential> {
        let access_token = self
            .access_token
            .context("OAuth response omitted access_token")?;
        let account_id = jwt_account_id(&access_token)
            .or_else(|| self.id_token.as_deref().and_then(jwt_account_id))
            .or_else(|| previous.and_then(|value| value.account_id.clone()));
        Ok(SubscriptionCredential {
            version: 1,
            credential_type: "openai_subscription".into(),
            access_token,
            refresh_token: self
                .refresh_token
                .or_else(|| previous.map(|value| value.refresh_token.clone()))
                .context("OAuth response omitted refresh_token")?,
            expires_at: Utc::now() + TimeDelta::seconds(self.expires_in.unwrap_or(3600)),
            account_id,
        })
    }
}

fn jwt_account_id(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .or_else(|| value.get("chatgpt_account_id"))
        .and_then(|id| id.as_str())
        .map(str::to_owned)
}

fn random_urlsafe(len: usize) -> String {
    let mut bytes = vec![0; len];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemorySecrets;
    use axum::{extract::Form, routing::post, Json, Router};
    use tokio::net::TcpStream;

    struct FailingWriteSecrets(String);
    impl SecretStore for FailingWriteSecrets {
        fn get(&self, _: &str) -> anyhow::Result<Option<String>> {
            Ok(Some(self.0.clone()))
        }
        fn set(&self, _: &str, _: &str) -> anyhow::Result<()> {
            anyhow::bail!("simulated Keychain write failure")
        }
        fn delete(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn extracts_chatgpt_account_id_from_access_token() {
        let payload = URL_SAFE_NO_PAD
            .encode(br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct_1"}}"#);
        assert_eq!(
            jwt_account_id(&format!("a.{payload}.c")).as_deref(),
            Some("acct_1")
        );
    }

    async fn mock_issuer() -> String {
        async fn token(Form(form): Form<HashMap<String, String>>) -> Json<serde_json::Value> {
            assert!(!form.get("client_id").unwrap().is_empty());
            if form.get("grant_type").map(String::as_str) == Some("authorization_code") {
                assert!(!form.get("code_verifier").unwrap().is_empty());
            }
            Json(
                serde_json::json!({"access_token":"access.new.token","refresh_token":"refresh-rotated","expires_in":3600}),
            )
        }
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/token", post(token)))
                .await
                .unwrap();
        });
        format!("http://{address}/token")
    }

    #[tokio::test]
    async fn wrong_state_is_rejected_by_the_loopback_callback() {
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        let manager = OAuthManager::with_config(
            Client::new(),
            secrets.clone(),
            OAuthConfig {
                authorization_url: "https://auth.example/authorize".into(),
                token_url: mock_issuer().await,
                client_id: "client".into(),
                timeout: Duration::from_secs(2),
            },
        );
        let start = manager.begin("provider").await.unwrap();
        let authorization = Url::parse(&start.authorization_url).unwrap();
        let params = authorization.query_pairs().collect::<HashMap<_, _>>();
        let redirect = Url::parse(params.get("redirect_uri").unwrap()).unwrap();
        let mut stream = TcpStream::connect(("127.0.0.1", redirect.port().unwrap()))
            .await
            .unwrap();
        stream
            .write_all(
                b"GET /auth/callback?code=approved&state=wrong HTTP/1.1\r\nHost: localhost\r\n\r\n",
            )
            .await
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        assert!(String::from_utf8(response)
            .unwrap()
            .contains("400 Bad Request"));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(matches!(
            manager.status("provider").await.unwrap().state,
            OAuthState::Error
        ));
        assert!(secrets
            .get(&provider_account("provider"))
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn callback_checks_state_and_exchanges_pkce_code() {
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        let manager = OAuthManager::with_config(
            Client::new(),
            secrets,
            OAuthConfig {
                authorization_url: "https://auth.example/authorize".into(),
                token_url: mock_issuer().await,
                client_id: "client".into(),
                timeout: Duration::from_secs(2),
            },
        );
        let start = manager.begin("provider").await.unwrap();
        let authorization = Url::parse(&start.authorization_url).unwrap();
        let params = authorization.query_pairs().collect::<HashMap<_, _>>();
        assert_eq!(
            params
                .get("code_challenge_method")
                .map(|value| value.as_ref()),
            Some("S256")
        );
        let redirect = Url::parse(params.get("redirect_uri").unwrap()).unwrap();
        let state = params.get("state").unwrap();
        let mut stream = TcpStream::connect(("127.0.0.1", redirect.port().unwrap()))
            .await
            .unwrap();
        stream.write_all(format!("GET /auth/callback?code=approved&state={state} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        assert!(String::from_utf8(response).unwrap().contains("200 OK"));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(matches!(
            manager.status("provider").await.unwrap().state,
            OAuthState::Connected
        ));
    }

    #[tokio::test]
    async fn expired_access_token_refreshes_with_rotated_refresh_token() {
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        let manager = OAuthManager::with_config(
            Client::new(),
            secrets,
            OAuthConfig {
                authorization_url: "https://auth.example/authorize".into(),
                token_url: mock_issuer().await,
                client_id: "client".into(),
                timeout: Duration::from_secs(2),
            },
        );
        manager
            .store_credential(
                "provider",
                &SubscriptionCredential {
                    version: 1,
                    credential_type: "openai_subscription".into(),
                    access_token: "expired".into(),
                    refresh_token: "refresh-old".into(),
                    expires_at: Utc::now() - TimeDelta::seconds(1),
                    account_id: Some("acct".into()),
                },
            )
            .unwrap();
        let refreshed = manager.access_token("provider").await.unwrap();
        assert_eq!(refreshed.refresh_token, "refresh-rotated");
        assert_eq!(refreshed.account_id.as_deref(), Some("acct"));
    }

    #[tokio::test]
    async fn callback_timeout_is_reported_without_storing_tokens() {
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        let manager = OAuthManager::with_config(
            Client::new(),
            secrets,
            OAuthConfig {
                authorization_url: "https://auth.example/authorize".into(),
                token_url: "https://auth.example/token".into(),
                client_id: "client".into(),
                timeout: Duration::from_millis(10),
            },
        );
        manager.begin("provider").await.unwrap();
        tokio::time::sleep(Duration::from_millis(25)).await;
        let status = manager.status("provider").await.unwrap();
        assert!(matches!(status.state, OAuthState::Error));
        assert!(status.error.unwrap().contains("timed out"));
    }

    #[tokio::test]
    async fn refresh_reports_keychain_write_failures() {
        let expired = SubscriptionCredential {
            version: 1,
            credential_type: "openai_subscription".into(),
            access_token: "expired".into(),
            refresh_token: "old".into(),
            expires_at: Utc::now() - TimeDelta::seconds(1),
            account_id: None,
        };
        let secrets: Arc<dyn SecretStore> = Arc::new(FailingWriteSecrets(
            serde_json::to_string(&expired).unwrap(),
        ));
        let manager = OAuthManager::with_config(
            Client::new(),
            secrets,
            OAuthConfig {
                authorization_url: "https://auth.example/authorize".into(),
                token_url: mock_issuer().await,
                client_id: "client".into(),
                timeout: Duration::from_secs(1),
            },
        );
        assert!(manager
            .access_token("provider")
            .await
            .unwrap_err()
            .to_string()
            .contains("Keychain"));
    }

    #[tokio::test]
    async fn logout_removes_the_entire_subscription_record() {
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        let manager = OAuthManager::new(Client::new(), secrets.clone());
        manager
            .store_credential(
                "provider",
                &SubscriptionCredential {
                    version: 1,
                    credential_type: "openai_subscription".into(),
                    access_token: "access".into(),
                    refresh_token: "refresh".into(),
                    expires_at: Utc::now() + TimeDelta::hours(1),
                    account_id: Some("acct".into()),
                },
            )
            .unwrap();
        manager.logout("provider").await.unwrap();
        assert!(secrets
            .get(&provider_account("provider"))
            .unwrap()
            .is_none());
        assert!(matches!(
            manager.status("provider").await.unwrap().state,
            OAuthState::Disconnected
        ));
    }
}

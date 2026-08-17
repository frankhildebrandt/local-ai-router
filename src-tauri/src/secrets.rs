use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use parking_lot::RwLock;
use rand::RngCore;

pub trait SecretStore: Send + Sync {
    fn get(&self, account: &str) -> anyhow::Result<Option<String>>;
    fn set(&self, account: &str, value: &str) -> anyhow::Result<()>;
    fn delete(&self, account: &str) -> anyhow::Result<()>;
}

pub struct KeychainSecrets {
    service: String,
}

impl KeychainSecrets {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }
}

impl SecretStore for KeychainSecrets {
    fn get(&self, account: &str) -> anyhow::Result<Option<String>> {
        let entry = keyring::Entry::new(&self.service, account)?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error).context("reading macOS Keychain"),
        }
    }

    fn set(&self, account: &str, value: &str) -> anyhow::Result<()> {
        keyring::Entry::new(&self.service, account)?
            .set_password(value)
            .context("writing macOS Keychain")
    }

    fn delete(&self, account: &str) -> anyhow::Result<()> {
        let entry = keyring::Entry::new(&self.service, account)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[derive(Default)]
pub struct MemorySecrets(RwLock<HashMap<String, String>>);

impl SecretStore for MemorySecrets {
    fn get(&self, account: &str) -> anyhow::Result<Option<String>> {
        Ok(self.0.read().get(account).cloned())
    }
    fn set(&self, account: &str, value: &str) -> anyhow::Result<()> {
        self.0.write().insert(account.into(), value.into());
        Ok(())
    }
    fn delete(&self, account: &str) -> anyhow::Result<()> {
        self.0.write().remove(account);
        Ok(())
    }
}

pub fn shared_keychain() -> Arc<dyn SecretStore> {
    Arc::new(KeychainSecrets::new("app.local-ai-router.desktop"))
}

pub fn generate_local_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    format!("lar_{}", URL_SAFE_NO_PAD.encode(bytes))
}

pub const LOCAL_API_KEY: &str = "local-api-key";
pub fn provider_account(id: &str) -> String {
    format!("provider:{id}")
}
pub const HF_ACCOUNT: &str = "hugging-face-token";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_are_unique_and_not_short() {
        let first = generate_local_token();
        let second = generate_local_token();
        assert!(first.starts_with("lar_"));
        assert!(first.len() >= 40);
        assert_ne!(first, second);
    }
}

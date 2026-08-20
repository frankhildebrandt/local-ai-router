use std::{collections::HashMap, io::Write, path::PathBuf, sync::Arc};

use anyhow::Context;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use parking_lot::RwLock;
use rand::RngCore;
use serde::{Deserialize, Serialize};

pub trait SecretStore: Send + Sync {
    fn get(&self, account: &str) -> anyhow::Result<Option<String>>;
    fn set(&self, account: &str, value: &str) -> anyhow::Result<()>;
    fn delete(&self, account: &str) -> anyhow::Result<()>;
    fn migrate_legacy_accounts(&self, _accounts: &[String]) -> anyhow::Result<()> {
        Ok(())
    }
}

const VAULT_ACCOUNT: &str = "credentials";

#[derive(Debug, Serialize, Deserialize)]
struct CredentialVault {
    version: u32,
    secrets: HashMap<String, String>,
}

struct KeychainItems {
    service: String,
}

impl KeychainItems {
    fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }
}

impl SecretStore for KeychainItems {
    fn get(&self, account: &str) -> anyhow::Result<Option<String>> {
        let entry = keyring::Entry::new(&self.service, account)?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error).context("reading the platform keyring"),
        }
    }

    fn set(&self, account: &str, value: &str) -> anyhow::Result<()> {
        keyring::Entry::new(&self.service, account)?
            .set_password(value)
            .context("writing the platform keyring")
    }

    fn delete(&self, account: &str) -> anyhow::Result<()> {
        let entry = keyring::Entry::new(&self.service, account)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

struct BundledStore<S> {
    inner: S,
    cache: RwLock<Option<HashMap<String, String>>>,
}

impl<S: SecretStore> BundledStore<S> {
    fn new(inner: S) -> Self {
        Self {
            inner,
            cache: RwLock::new(None),
        }
    }

    fn read_vault(&self) -> anyhow::Result<HashMap<String, String>> {
        match self.inner.get(VAULT_ACCOUNT)? {
            None => Ok(HashMap::new()),
            Some(raw) => parse_vault(&raw),
        }
    }

    fn persist(&self, map: &HashMap<String, String>) -> anyhow::Result<()> {
        if map.is_empty() {
            self.inner.delete(VAULT_ACCOUNT)
        } else {
            self.inner.set(VAULT_ACCOUNT, &encode_vault(map)?)
        }
    }

    fn loaded_map(
        &self,
    ) -> anyhow::Result<parking_lot::RwLockWriteGuard<'_, Option<HashMap<String, String>>>> {
        let mut cache = self.cache.write();
        if cache.is_none() {
            *cache = Some(self.read_vault()?);
        }
        Ok(cache)
    }
}

fn parse_vault(raw: &str) -> anyhow::Result<HashMap<String, String>> {
    let vault: CredentialVault =
        serde_json::from_str(raw).context("invalid credential vault in the platform keyring")?;
    Ok(vault.secrets)
}

fn encode_vault(secrets: &HashMap<String, String>) -> anyhow::Result<String> {
    Ok(serde_json::to_string(&CredentialVault {
        version: 1,
        secrets: secrets.clone(),
    })?)
}

impl<S: SecretStore> SecretStore for BundledStore<S> {
    fn get(&self, account: &str) -> anyhow::Result<Option<String>> {
        let cache = self.loaded_map()?;
        Ok(cache.as_ref().and_then(|map| map.get(account).cloned()))
    }

    fn set(&self, account: &str, value: &str) -> anyhow::Result<()> {
        let mut cache = self.loaded_map()?;
        let map = cache.as_mut().context("credential vault cache missing")?;
        let previous = map.insert(account.into(), value.into());
        if let Err(error) = self.persist(map) {
            match previous {
                Some(old) => {
                    map.insert(account.into(), old);
                }
                None => {
                    map.remove(account);
                }
            }
            return Err(error);
        }
        Ok(())
    }

    fn delete(&self, account: &str) -> anyhow::Result<()> {
        let mut cache = self.loaded_map()?;
        let map = cache.as_mut().context("credential vault cache missing")?;
        let Some(previous) = map.remove(account) else {
            return Ok(());
        };
        if let Err(error) = self.persist(map) {
            map.insert(account.into(), previous);
            return Err(error);
        }
        Ok(())
    }

    fn migrate_legacy_accounts(&self, accounts: &[String]) -> anyhow::Result<()> {
        let mut cache = self.loaded_map()?;
        let map = cache.as_mut().context("credential vault cache missing")?;
        let mut copied = Vec::new();
        for account in accounts {
            if account == VAULT_ACCOUNT || map.contains_key(account) {
                continue;
            }
            if let Some(value) = self.inner.get(account)? {
                map.insert(account.clone(), value);
                copied.push(account.clone());
            }
        }
        if !copied.is_empty() {
            if let Err(error) = self.persist(map) {
                for account in &copied {
                    map.remove(account);
                }
                return Err(error);
            }
        }
        drop(cache);
        for account in copied {
            let _ = self.inner.delete(&account);
        }
        Ok(())
    }
}

pub struct KeychainSecrets {
    store: BundledStore<KeychainItems>,
}

impl KeychainSecrets {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            store: BundledStore::new(KeychainItems::new(service)),
        }
    }
}

impl SecretStore for KeychainSecrets {
    fn get(&self, account: &str) -> anyhow::Result<Option<String>> {
        self.store.get(account)
    }
    fn set(&self, account: &str, value: &str) -> anyhow::Result<()> {
        self.store.set(account, value)
    }
    fn delete(&self, account: &str) -> anyhow::Result<()> {
        self.store.delete(account)
    }
    fn migrate_legacy_accounts(&self, accounts: &[String]) -> anyhow::Result<()> {
        self.store.migrate_legacy_accounts(accounts)
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

struct FileItems {
    path: PathBuf,
}

impl SecretStore for FileItems {
    fn get(&self, account: &str) -> anyhow::Result<Option<String>> {
        if account != VAULT_ACCOUNT {
            return Ok(None);
        }
        match std::fs::read_to_string(&self.path) {
            Ok(raw) if raw.trim().is_empty() => Ok(None),
            Ok(raw) => Ok(Some(raw)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).context("reading secrets file"),
        }
    }

    fn set(&self, _account: &str, value: &str) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(&self.path)?
            .write_all(value.as_bytes())
            .context("writing secrets file")?;
        Ok(())
    }

    fn delete(&self, _account: &str) -> anyhow::Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

pub struct FileSecrets {
    store: BundledStore<FileItems>,
}

impl FileSecrets {
    pub fn new(path: PathBuf) -> Self {
        Self {
            store: BundledStore::new(FileItems { path }),
        }
    }
}

impl SecretStore for FileSecrets {
    fn get(&self, account: &str) -> anyhow::Result<Option<String>> {
        self.store.get(account)
    }
    fn set(&self, account: &str, value: &str) -> anyhow::Result<()> {
        self.store.set(account, value)
    }
    fn delete(&self, account: &str) -> anyhow::Result<()> {
        self.store.delete(account)
    }
    fn migrate_legacy_accounts(&self, accounts: &[String]) -> anyhow::Result<()> {
        self.store.migrate_legacy_accounts(accounts)
    }
}

pub fn file_secrets(path: PathBuf) -> Arc<dyn SecretStore> {
    Arc::new(FileSecrets::new(path))
}

pub fn default_secrets_file(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("secrets.json")
}

pub fn generate_local_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    format!("lar_{}", URL_SAFE_NO_PAD.encode(bytes))
}

pub const LOCAL_API_KEY: &str = "local-api-key";
pub fn local_api_key_account(id: &str) -> String {
    format!("local-api-key:{id}")
}
pub fn provider_account(id: &str) -> String {
    format!("provider:{id}")
}
pub const HF_ACCOUNT: &str = "hugging-face-token";
pub const CIVITAI_ACCOUNT: &str = "civitai-token";

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

    #[test]
    fn get_does_not_create_a_vault_item() {
        let store = BundledStore::new(MemorySecrets::default());
        assert!(store.get("provider:openai").unwrap().is_none());
        assert!(store.inner.get(VAULT_ACCOUNT).unwrap().is_none());
    }

    #[test]
    fn vault_roundtrips_through_backend_and_cache() {
        let store = BundledStore::new(MemorySecrets::default());
        store.set("provider:openai", "sk-1").unwrap();
        store.set(HF_ACCOUNT, "hf_1").unwrap();
        assert_eq!(
            store.get("provider:openai").unwrap().as_deref(),
            Some("sk-1")
        );

        let persisted = store.inner.get(VAULT_ACCOUNT).unwrap().unwrap();
        let vault: CredentialVault = serde_json::from_str(&persisted).unwrap();
        assert_eq!(vault.version, 1);
        assert_eq!(vault.secrets["provider:openai"], "sk-1");
        assert!(store.inner.get("provider:openai").unwrap().is_none());

        store.cache.write().take();
        assert_eq!(store.get(HF_ACCOUNT).unwrap().as_deref(), Some("hf_1"));
        assert!(store.get("missing").unwrap().is_none());

        store.delete("provider:openai").unwrap();
        store.cache.write().take();
        assert!(store.get("provider:openai").unwrap().is_none());
        assert_eq!(store.get(HF_ACCOUNT).unwrap().as_deref(), Some("hf_1"));

        store.delete(HF_ACCOUNT).unwrap();
        assert!(store.inner.get(VAULT_ACCOUNT).unwrap().is_none());
    }

    #[test]
    fn migrate_legacy_accounts_copies_into_vault_and_deletes_old_items() {
        let inner = MemorySecrets::default();
        inner.set("provider:openai", "sk-legacy").unwrap();
        inner.set(HF_ACCOUNT, "hf-legacy").unwrap();
        inner.set(LOCAL_API_KEY, "lar_legacy").unwrap();
        let store = BundledStore::new(inner);
        store
            .migrate_legacy_accounts(&[
                "provider:openai".into(),
                HF_ACCOUNT.into(),
                CIVITAI_ACCOUNT.into(),
                LOCAL_API_KEY.into(),
            ])
            .unwrap();

        assert_eq!(
            store.get("provider:openai").unwrap().as_deref(),
            Some("sk-legacy")
        );
        assert_eq!(store.get(HF_ACCOUNT).unwrap().as_deref(), Some("hf-legacy"));
        assert_eq!(
            store.get(LOCAL_API_KEY).unwrap().as_deref(),
            Some("lar_legacy")
        );
        assert!(store.get(CIVITAI_ACCOUNT).unwrap().is_none());

        assert!(store.inner.get("provider:openai").unwrap().is_none());
        assert!(store.inner.get(HF_ACCOUNT).unwrap().is_none());
        assert!(store.inner.get(LOCAL_API_KEY).unwrap().is_none());
        assert!(store.inner.get(VAULT_ACCOUNT).unwrap().is_some());
    }

    #[test]
    fn migrate_legacy_accounts_does_not_overwrite_vault_values() {
        let inner = MemorySecrets::default();
        inner.set("provider:openai", "old").unwrap();
        let store = BundledStore::new(inner);
        store.set("provider:openai", "new").unwrap();
        store
            .migrate_legacy_accounts(&["provider:openai".into()])
            .unwrap();
        assert_eq!(
            store.get("provider:openai").unwrap().as_deref(),
            Some("new")
        );
        assert_eq!(
            store.inner.get("provider:openai").unwrap().as_deref(),
            Some("old")
        );
    }

    #[test]
    fn file_secrets_roundtrip_in_the_data_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let store = FileSecrets::new(path.clone());
        store.set("local-api-key:default", "lar_file").unwrap();
        drop(store);
        let reopened = FileSecrets::new(path.clone());
        assert_eq!(
            reopened.get("local-api-key:default").unwrap().as_deref(),
            Some("lar_file")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn systemd_unit_uses_the_default_secrets_file_in_the_service_data_dir() {
        let unit = include_str!("../../packaging/linux/local-ai-router.service");
        let dir = PathBuf::from("/var/lib/local-ai-router");
        assert!(unit.contains("ExecStart=/usr/bin/local-ai-router serve"));
        assert!(unit.contains(&format!(
            "--secrets-file {}",
            default_secrets_file(&dir).display()
        )));
        assert!(unit.contains(&format!("--data-dir {}", dir.display())));
    }
}

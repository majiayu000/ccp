//! Secret storage for token-shaped env values.
//!
//! Production: macOS Keychain via the `keyring` crate. The profile TOML only
//! holds an `@keychain` marker, so tokens never sit in a plaintext file.
//! Tests: in-memory store (no keychain prompts, no pollution).

use std::collections::BTreeMap;
use std::io;
use std::sync::Mutex;

/// Written into profile TOML in place of the real token.
pub const MARKER: &str = "@keychain";

/// Keychain service name; entries are keyed `<profile>/<ENV_KEY>`.
const SERVICE: &str = "com.ccp.profiles";

pub fn is_marker(value: &str) -> bool {
    value == MARKER
}

/// Env keys whose values must live in the keychain, not the TOML file.
pub fn is_secret_key(key: &str) -> bool {
    matches!(key, "ANTHROPIC_AUTH_TOKEN" | "ANTHROPIC_API_KEY")
}

pub trait SecretStore: Send + Sync {
    fn get(&self, profile: &str, key: &str) -> io::Result<Option<String>>;
    fn set(&self, profile: &str, key: &str, value: &str) -> io::Result<()>;
    fn delete(&self, profile: &str, key: &str) -> io::Result<()>;
}

// ---------- macOS Keychain ----------

pub struct KeychainStore;

impl KeychainStore {
    fn entry(profile: &str, key: &str) -> io::Result<keyring::Entry> {
        keyring::Entry::new(SERVICE, &format!("{profile}/{key}"))
            .map_err(|e| io::Error::other(format!("keychain entry init: {e}")))
    }
}

impl SecretStore for KeychainStore {
    fn get(&self, profile: &str, key: &str) -> io::Result<Option<String>> {
        match Self::entry(profile, key)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(io::Error::other(format!(
                "keychain read {profile}/{key}: {e}"
            ))),
        }
    }

    fn set(&self, profile: &str, key: &str, value: &str) -> io::Result<()> {
        Self::entry(profile, key)?
            .set_password(value)
            .map_err(|e| io::Error::other(format!("keychain write {profile}/{key}: {e}")))
    }

    fn delete(&self, profile: &str, key: &str) -> io::Result<()> {
        match Self::entry(profile, key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(io::Error::other(format!(
                "keychain delete {profile}/{key}: {e}"
            ))),
        }
    }
}

// ---------- in-memory (tests, and a reference for non-mac fallbacks) ----------

#[derive(Default)]
pub struct MemoryStore {
    map: Mutex<BTreeMap<(String, String), String>>,
}

impl SecretStore for MemoryStore {
    fn get(&self, profile: &str, key: &str) -> io::Result<Option<String>> {
        Ok(self
            .map
            .lock()
            .expect("memory store poisoned")
            .get(&(profile.into(), key.into()))
            .cloned())
    }

    fn set(&self, profile: &str, key: &str, value: &str) -> io::Result<()> {
        self.map
            .lock()
            .expect("memory store poisoned")
            .insert((profile.into(), key.into()), value.into());
        Ok(())
    }

    fn delete(&self, profile: &str, key: &str) -> io::Result<()> {
        self.map
            .lock()
            .expect("memory store poisoned")
            .remove(&(profile.into(), key.into()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_key_classification() {
        assert!(is_secret_key("ANTHROPIC_AUTH_TOKEN"));
        assert!(is_secret_key("ANTHROPIC_API_KEY"));
        assert!(!is_secret_key("ANTHROPIC_BASE_URL"));
        assert!(!is_secret_key("ANTHROPIC_MODEL"));
    }

    #[test]
    fn memory_store_roundtrip() {
        let s = MemoryStore::default();
        assert_eq!(s.get("kimi", "K").unwrap(), None);
        s.set("kimi", "K", "v1").unwrap();
        assert_eq!(s.get("kimi", "K").unwrap().as_deref(), Some("v1"));
        s.set("kimi", "K", "v2").unwrap();
        assert_eq!(s.get("kimi", "K").unwrap().as_deref(), Some("v2"));
        s.delete("kimi", "K").unwrap();
        assert_eq!(s.get("kimi", "K").unwrap(), None);
        // Profiles are isolated.
        s.set("a", "K", "va").unwrap();
        assert_eq!(s.get("b", "K").unwrap(), None);
    }
}

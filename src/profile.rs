//! Profile store: CRUD over `~/.ccp/profiles/*.toml`, discovery of unmanaged
//! `~/.claude-*` dirs, template symlinks, and the shared env overlay.

use crate::paths::{Paths, DEFAULT_PROFILE};
use crate::secret::{self, SecretStore};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

/// Env files hold tokens; they must never be world-readable.
const ENV_FILE_MODE: u32 = 0o600;

/// Symlinked from `~/.claude` into newly created profile homes.
const TEMPLATE_ENTRIES: [&str; 3] = ["CLAUDE.md", "agents", "skills"];

/// Keychain slot for shared-overlay secrets (not a real profile).
const SHARED_SLOT: &str = "_shared";

#[derive(Debug)]
pub enum StoreError {
    InvalidName(String),
    InvalidEnvKey(String),
    Reserved(String),
    Exists(String),
    HomeExists(PathBuf),
    NotFound(String),
    Io(io::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName(n) => {
                write!(f, "invalid profile name: {n:?} (want [a-z0-9][a-z0-9-]*)")
            }
            Self::InvalidEnvKey(k) => {
                write!(f, "invalid env key: {k:?} (want [A-Za-z_][A-Za-z0-9_]*)")
            }
            Self::Reserved(n) => write!(f, "operation not allowed on reserved profile {n:?}"),
            Self::Exists(n) => write!(f, "profile {n:?} already exists"),
            Self::HomeExists(p) => write!(
                f,
                "directory {} already exists; import it instead",
                p.display()
            ),
            Self::NotFound(n) => write!(f, "profile {n:?} not found"),
            Self::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// On-disk profile file (`~/.ccp/profiles/<name>.toml`).
#[derive(Debug, Default, Serialize, Deserialize)]
struct ProfileFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    home: Option<PathBuf>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Profile {
    pub name: String,
    pub home: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    pub env: BTreeMap<String, String>,
    /// False only for the lazily-materialized `default` profile.
    pub managed: bool,
}

/// A `~/.claude-*` directory not yet managed by ccp.
#[derive(Clone, Debug, Serialize)]
pub struct Unmanaged {
    pub name: String,
    pub home: PathBuf,
}

pub struct ProfileStore {
    paths: Paths,
    secrets: Arc<dyn SecretStore>,
}

impl ProfileStore {
    /// Production store: tokens go to the OS keychain.
    pub fn new(paths: Paths) -> Self {
        Self::with_secrets(paths, Arc::new(secret::KeychainStore))
    }

    /// Store with an injectable secret backend (tests use in-memory).
    pub fn with_secrets(paths: Paths, secrets: Arc<dyn SecretStore>) -> Self {
        Self { paths, secrets }
    }

    /// One managed profile by name (`default` resolves even without a file).
    pub fn get(&self, name: &str) -> Result<Profile, StoreError> {
        if name == DEFAULT_PROFILE && !self.paths.profile_file(name).exists() {
            return Ok(self.default_profile());
        }
        self.read_profile(name)
    }

    /// Managed profiles (default always included) + unmanaged `~/.claude-*` dirs.
    pub fn list(&self) -> Result<(Vec<Profile>, Vec<Unmanaged>), StoreError> {
        let mut profiles = Vec::new();
        let mut seen = std::collections::HashSet::new();

        let dir = self.paths.profiles_dir();
        if dir.exists() {
            for entry in fs::read_dir(&dir)? {
                let path = entry?.path();
                if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                    continue;
                }
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string();
                if !is_valid_name(&name) && name != DEFAULT_PROFILE {
                    continue;
                }
                seen.insert(name.clone());
                profiles.push(self.read_profile(&name)?);
            }
        }
        if !seen.contains(DEFAULT_PROFILE) {
            profiles.push(self.default_profile());
        }
        profiles.sort_by(|a, b| {
            (a.name != DEFAULT_PROFILE, a.name.clone())
                .cmp(&(b.name != DEFAULT_PROFILE, b.name.clone()))
        });

        let mut unmanaged = Vec::new();
        for entry in fs::read_dir(&self.paths.user_home)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let Some(dir_name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Some(suffix) = dir_name.strip_prefix(".claude-") else {
                continue;
            };
            if !is_valid_name(suffix) || seen.contains(suffix) {
                continue;
            }
            unmanaged.push(Unmanaged {
                name: suffix.to_string(),
                home: entry.path(),
            });
        }
        unmanaged.sort_by(|a, b| a.name.cmp(&b.name));
        Ok((profiles, unmanaged))
    }

    /// Create a new managed profile: fresh home dir + template symlinks.
    pub fn create(
        &self,
        name: &str,
        preset: Option<String>,
        env: BTreeMap<String, String>,
    ) -> Result<Profile, StoreError> {
        validate_name(name)?;
        if name == DEFAULT_PROFILE {
            return Err(StoreError::Reserved(name.into()));
        }
        if self.paths.profile_file(name).exists() {
            return Err(StoreError::Exists(name.into()));
        }
        let home = self.paths.profile_home(name);
        if home.exists() {
            return Err(StoreError::HomeExists(home));
        }
        validate_env_keys(env.keys())?;
        let env = self.protect_secrets(name, env)?;
        fs::create_dir_all(&home)?;
        self.apply_templates(&home);
        self.copy_mcp_servers(&home);
        self.write_profile_file(
            name,
            &ProfileFile {
                preset,
                home: None,
                env,
            },
        )?;
        self.read_profile(name)
    }

    /// Adopt an existing unmanaged `~/.claude-<name>` dir without touching it.
    pub fn import(&self, name: &str) -> Result<Profile, StoreError> {
        validate_name(name)?;
        if name == DEFAULT_PROFILE {
            return Err(StoreError::Reserved(name.into()));
        }
        if self.paths.profile_file(name).exists() {
            return Err(StoreError::Exists(name.into()));
        }
        let home = self.paths.profile_home(name);
        if !home.is_dir() {
            return Err(StoreError::NotFound(format!(
                "{name} (no dir {})",
                home.display()
            )));
        }
        self.write_profile_file(name, &ProfileFile::default())?;
        self.read_profile(name)
    }

    /// Merge env updates. An empty value deletes the key.
    /// Secret values are diverted to the keychain; the file keeps a marker.
    pub fn update_env(
        &self,
        name: &str,
        patch: BTreeMap<String, String>,
    ) -> Result<Profile, StoreError> {
        validate_name(name)?;
        validate_env_keys(patch.keys())?;
        let mut file = self.read_profile_file(name).unwrap_or_default();
        for (k, v) in patch {
            if v.is_empty() {
                file.env.remove(&k);
                if secret::is_secret_key(&k) {
                    let _ = self.secrets.delete(name, &k);
                }
            } else if secret::is_secret_key(&k) && !secret::is_marker(&v) {
                self.secrets.set(name, &k, &v).map_err(StoreError::Io)?;
                file.env.insert(k, secret::MARKER.into());
            } else {
                file.env.insert(k, v);
            }
        }
        self.write_profile_file(name, &file)?;
        self.read_profile(name)
    }

    /// Env with keychain markers resolved to real values — for launching,
    /// connectivity tests, and plaintext export. Fails closed if a marker
    /// has no keychain entry.
    pub fn resolve_env(&self, name: &str) -> Result<BTreeMap<String, String>, StoreError> {
        let profile = self.get(name)?;
        self.resolve_map(name, profile.env)
    }

    /// Shared overlay with markers resolved (stored under the `_shared` slot).
    pub fn resolve_shared(&self) -> Result<BTreeMap<String, String>, StoreError> {
        self.resolve_map(SHARED_SLOT, self.read_shared()?)
    }

    fn resolve_map(
        &self,
        owner: &str,
        env: BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, String>, StoreError> {
        let mut out = BTreeMap::new();
        for (k, v) in env {
            if secret::is_marker(&v) {
                let real = self
                    .secrets
                    .get(owner, &k)
                    .map_err(StoreError::Io)?
                    .ok_or_else(|| {
                        StoreError::Io(io::Error::new(
                            io::ErrorKind::NotFound,
                            format!("no keychain entry for {owner}/{k}; re-enter the token"),
                        ))
                    })?;
                out.insert(k, real);
            } else {
                out.insert(k, v);
            }
        }
        Ok(out)
    }

    /// Divert secret-keyed values into the keychain, leaving markers behind.
    fn protect_secrets(
        &self,
        name: &str,
        mut env: BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, String>, StoreError> {
        for (k, v) in env.iter_mut() {
            if secret::is_secret_key(k) && !v.is_empty() && !secret::is_marker(v) {
                self.secrets.set(name, k, v).map_err(StoreError::Io)?;
                *v = secret::MARKER.into();
            }
        }
        Ok(env)
    }

    /// Unmanage a profile. `purge` also deletes the home dir (sessions included).
    /// Keychain entries for the profile are removed either way — orphaned
    /// secrets serve nothing.
    pub fn delete(&self, name: &str, purge: bool) -> Result<(), StoreError> {
        validate_name(name)?;
        if name == DEFAULT_PROFILE {
            return Err(StoreError::Reserved(name.into()));
        }
        let file = self.paths.profile_file(name);
        if !file.exists() {
            return Err(StoreError::NotFound(name.into()));
        }
        if let Ok(profile) = self.read_profile(name) {
            for (k, v) in &profile.env {
                if secret::is_marker(v) {
                    let _ = self.secrets.delete(name, k);
                }
            }
        }
        fs::remove_file(file)?;
        if purge {
            let home = self.paths.profile_home(name);
            if home.exists() {
                fs::remove_dir_all(home)?;
            }
        }
        Ok(())
    }

    pub fn read_shared(&self) -> Result<BTreeMap<String, String>, StoreError> {
        let path = self.paths.shared_file();
        if !path.exists() {
            return Ok(BTreeMap::new());
        }
        let raw = fs::read_to_string(path)?;
        let file: SharedFile = toml::from_str(&raw)
            .map_err(|e| StoreError::Io(io::Error::new(io::ErrorKind::InvalidData, e)))?;
        Ok(file.env)
    }

    pub fn write_shared(&self, patch: BTreeMap<String, String>) -> Result<(), StoreError> {
        validate_env_keys(patch.keys())?;
        let patch = self.protect_secrets(SHARED_SLOT, patch)?;
        let mut env = self.read_shared()?;
        for (k, v) in patch {
            if v.is_empty() {
                env.remove(&k);
                if secret::is_secret_key(&k) {
                    let _ = self.secrets.delete(SHARED_SLOT, &k);
                }
            } else {
                env.insert(k, v);
            }
        }
        let body = toml::to_string(&SharedFile { env })
            .map_err(|e| StoreError::Io(io::Error::new(io::ErrorKind::InvalidData, e)))?;
        write_private(&self.paths.shared_file(), body.as_bytes())
    }

    fn default_profile(&self) -> Profile {
        Profile {
            name: DEFAULT_PROFILE.into(),
            home: self.paths.default_claude_home(),
            preset: None,
            env: BTreeMap::new(),
            managed: false,
        }
    }

    fn read_profile(&self, name: &str) -> Result<Profile, StoreError> {
        let file = self
            .read_profile_file(name)
            .ok_or(StoreError::NotFound(name.into()))?;
        Ok(Profile {
            name: name.into(),
            home: file.home.unwrap_or_else(|| self.paths.profile_home(name)),
            preset: file.preset,
            env: file.env,
            managed: true,
        })
    }

    fn read_profile_file(&self, name: &str) -> Option<ProfileFile> {
        let raw = fs::read_to_string(self.paths.profile_file(name)).ok()?;
        toml::from_str(&raw).ok()
    }

    fn write_profile_file(&self, name: &str, file: &ProfileFile) -> Result<(), StoreError> {
        fs::create_dir_all(self.paths.profiles_dir())?;
        let body = toml::to_string(file)
            .map_err(|e| StoreError::Io(io::Error::new(io::ErrorKind::InvalidData, e)))?;
        write_private(&self.paths.profile_file(name), body.as_bytes())
    }

    /// Symlink CLAUDE.md / agents / skills from `~/.claude` into a fresh home.
    /// Best-effort: missing sources or existing targets are skipped, and a
    /// symlink failure does not fail profile creation.
    fn apply_templates(&self, home: &std::path::Path) {
        let default_home = self.paths.default_claude_home();
        for entry in TEMPLATE_ENTRIES {
            let src = default_home.join(entry);
            let dst = home.join(entry);
            if !src.exists() || dst.exists() {
                continue;
            }
            #[cfg(unix)]
            let _ = std::os::unix::fs::symlink(&src, &dst);
        }
    }

    /// Copy `mcpServers` from the default profile's `.claude.json` so the new
    /// profile starts with the same MCP servers. Best-effort: any parse or
    /// write trouble skips the copy rather than failing creation.
    fn copy_mcp_servers(&self, home: &std::path::Path) {
        let src = self.paths.default_claude_home().join(".claude.json");
        let Ok(raw) = fs::read_to_string(&src) else {
            return;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return;
        };
        let Some(servers) = json.get("mcpServers").and_then(|m| m.as_object()) else {
            return;
        };
        if servers.is_empty() {
            return;
        }
        let out = serde_json::json!({ "mcpServers": servers });
        let Ok(body) = serde_json::to_string_pretty(&out) else {
            return;
        };
        let _ = write_private(&home.join(".claude.json"), body.as_bytes());
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SharedFile {
    #[serde(default)]
    env: BTreeMap<String, String>,
}

pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
}

/// POSIX portable env name: `[A-Za-z_][A-Za-z0-9_]*`.
///
/// Keys that fail this check must never be concatenated into the shell line
/// built by `launch::build_command`.
pub fn is_valid_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn validate_name(name: &str) -> Result<(), StoreError> {
    if is_valid_name(name) {
        Ok(())
    } else {
        Err(StoreError::InvalidName(name.into()))
    }
}

fn validate_env_keys<'a>(keys: impl IntoIterator<Item = &'a String>) -> Result<(), StoreError> {
    for k in keys {
        if !is_valid_env_key(k) {
            return Err(StoreError::InvalidEnvKey(k.clone()));
        }
    }
    Ok(())
}

/// Write a file readable only by the owner (tokens live in these).
fn write_private(path: &std::path::Path, body: &[u8]) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(ENV_FILE_MODE))?;
    }
    Ok(())
}

/// Mask a secret for display: keep tiny affordances, never the substance.
pub fn mask(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 8 {
        return "•••".into();
    }
    let head: String = chars.iter().take(4).collect();
    let tail: String = chars.iter().skip(chars.len() - 4).collect();
    format!("{head}…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_validation() {
        assert!(is_valid_name("kimi"));
        assert!(is_valid_name("a1-b2"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name("-kimi"));
        assert!(!is_valid_name("Kimi"));
        assert!(!is_valid_name("ki mi"));
        assert!(!is_valid_name("ki/mi"));
    }

    #[test]
    fn env_key_validation() {
        assert!(is_valid_env_key("ANTHROPIC_BASE_URL"));
        assert!(is_valid_env_key("_private"));
        assert!(is_valid_env_key("a1"));
        assert!(!is_valid_env_key(""));
        assert!(!is_valid_env_key("1ABC"));
        assert!(!is_valid_env_key("FOO; touch /tmp/pwned; BAR"));
        assert!(!is_valid_env_key("FOO BAR"));
        assert!(!is_valid_env_key("FOO=BAR"));
        assert!(!is_valid_env_key("FOO$USER"));
    }

    #[test]
    fn masking_hides_middle() {
        assert_eq!(mask("sk-abcdefgh12345678"), "sk-a…5678");
        assert_eq!(mask("short"), "•••");
    }
}

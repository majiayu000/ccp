//! Filesystem layout resolution. Kept injectable so tests can point at tempdirs.

use std::path::PathBuf;

pub const DEFAULT_PROFILE: &str = "default";

#[derive(Clone, Debug)]
pub struct Paths {
    pub user_home: PathBuf,
    pub ccp_home: PathBuf,
}

impl Paths {
    /// Resolve from the environment: `$CCP_HOME` (else `~/.ccp`), `$HOME`.
    pub fn from_env() -> std::io::Result<Self> {
        let user_home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME is not set"))?;
        let ccp_home = std::env::var_os("CCP_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| user_home.join(".ccp"));
        Ok(Self::new(user_home, ccp_home))
    }

    pub fn new(user_home: impl Into<PathBuf>, ccp_home: impl Into<PathBuf>) -> Self {
        Self {
            user_home: user_home.into(),
            ccp_home: ccp_home.into(),
        }
    }

    /// The default profile home: the user's real `~/.claude`.
    pub fn default_claude_home(&self) -> PathBuf {
        self.user_home.join(".claude")
    }

    /// A profile's CLAUDE_CONFIG_DIR. `default` maps to `~/.claude`,
    /// anything else to `~/.claude-<name>`.
    pub fn profile_home(&self, name: &str) -> PathBuf {
        if name == DEFAULT_PROFILE {
            self.default_claude_home()
        } else {
            self.user_home.join(format!(".claude-{name}"))
        }
    }

    pub fn profiles_dir(&self) -> PathBuf {
        self.ccp_home.join("profiles")
    }

    pub fn profile_file(&self, name: &str) -> PathBuf {
        self.profiles_dir().join(format!("{name}.toml"))
    }

    pub fn shared_file(&self) -> PathBuf {
        self.ccp_home.join("shared.toml")
    }

    pub fn config_file(&self) -> PathBuf {
        self.ccp_home.join("config.toml")
    }
}

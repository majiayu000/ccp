//! Launching claude sessions in a new Terminal window.
//!
//! The command is a single POSIX sh line: every value is single-quote
//! escaped, so profile env values can never break out of their assignment.
//! Env *keys* must also be POSIX identifiers; they are concatenated raw.

use crate::profile::is_valid_env_key;
use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::Path;

/// Shell-quote a value: wrap in single quotes, escape embedded ones (`'\''`).
fn sh_quote(v: &str) -> String {
    format!("'{}'", v.replace('\'', "'\\''"))
}

#[derive(Debug)]
pub struct BuildError {
    pub key: String,
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "refusing to emit unsafe env key {:?}: want [A-Za-z_][A-Za-z0-9_]*",
            self.key
        )
    }
}

/// Build `cd <cwd> && env CLAUDE_CONFIG_DIR=... K=V ... claude [--resume id]`.
///
/// Returns an error if any env key is not a POSIX portable identifier, so a
/// metacharacter key can never break out of `env KEY='value' claude`.
pub fn build_command(
    home: &Path,
    env: &BTreeMap<String, String>,
    resume: Option<&str>,
    cwd: Option<&str>,
) -> Result<String, BuildError> {
    for k in env.keys() {
        if !is_valid_env_key(k) {
            return Err(BuildError { key: k.clone() });
        }
    }
    let mut cmd = String::new();
    if let Some(dir) = cwd {
        cmd.push_str("cd ");
        cmd.push_str(&sh_quote(dir));
        cmd.push_str(" && ");
    }
    cmd.push_str("env CLAUDE_CONFIG_DIR=");
    cmd.push_str(&sh_quote(&home.display().to_string()));
    for (k, v) in env {
        cmd.push(' ');
        cmd.push_str(k);
        cmd.push('=');
        cmd.push_str(&sh_quote(v));
    }
    cmd.push_str(" claude");
    if let Some(id) = resume {
        cmd.push_str(" --resume ");
        cmd.push_str(&sh_quote(id));
    }
    Ok(cmd)
}

/// Escape for embedding in an AppleScript double-quoted string literal.
fn as_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Open a new Terminal.app window running `command`.
pub fn spawn_terminal(command: &str) -> io::Result<()> {
    let script = format!(
        "tell application \"Terminal\" to do script \"{}\"",
        as_escape(command)
    );
    run_osascript(&script)
}

/// Open a new iTerm2 window running `command`.
pub fn spawn_iterm(command: &str) -> io::Result<()> {
    let script = format!(
        "tell application \"iTerm\" to create window with default profile command \"{}\"",
        as_escape(command)
    );
    run_osascript(&script)
}

fn run_osascript(script: &str) -> io::Result<()> {
    let status = std::process::Command::new("osascript")
        .args(["-e", script])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("osascript exited with {status}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_pairs() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("ANTHROPIC_BASE_URL".into(), "https://example.com".into()),
            ("ANTHROPIC_AUTH_TOKEN".into(), "sk-test".into()),
        ])
    }

    #[test]
    fn command_contains_home_and_env() {
        let cmd =
            build_command(Path::new("/Users/x/.claude-kimi"), &env_pairs(), None, None).unwrap();
        assert!(cmd.starts_with("env CLAUDE_CONFIG_DIR='/Users/x/.claude-kimi'"));
        assert!(cmd.contains(" ANTHROPIC_BASE_URL='https://example.com'"));
        assert!(cmd.ends_with(" claude"));
    }

    #[test]
    fn command_quotes_nasty_values() {
        let mut env = BTreeMap::new();
        env.insert("WEIRD".to_string(), "it's a $trap `here`".to_string());
        let cmd = build_command(Path::new("/h"), &env, None, Some("/tmp/a b")).unwrap();
        assert!(cmd.starts_with("cd '/tmp/a b' && "));
        assert!(cmd.contains("WEIRD='it'\\''s a $trap `here`'"));
    }

    #[test]
    fn command_resume_appended() {
        let cmd = build_command(Path::new("/h"), &BTreeMap::new(), Some("abc-123"), None).unwrap();
        assert!(cmd.ends_with("claude --resume 'abc-123'"));
    }

    #[test]
    fn command_rejects_metacharacter_key() {
        let mut env = BTreeMap::new();
        env.insert("FOO; touch /tmp/pwned; BAR".into(), "x".into());
        let err = build_command(Path::new("/h"), &env, None, None).unwrap_err();
        assert_eq!(err.key, "FOO; touch /tmp/pwned; BAR");
        assert!(err.to_string().contains("refusing to emit unsafe env key"));
    }

    #[test]
    fn applescript_escaping() {
        assert_eq!(as_escape("a\"b\\c"), "a\\\"b\\\\c");
    }
}

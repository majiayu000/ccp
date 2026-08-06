//! `ccp doctor` — sanity checks across the ccp data dir and profile homes.

use crate::paths::Paths;
use crate::profile::ProfileStore;
use std::fmt;
use std::fs;
use std::path::Path;

pub struct Check {
    pub ok: bool,
    pub label: String,
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", if self.ok { "✓" } else { "✗" }, self.label)
    }
}

pub fn run(paths: &Paths) -> Vec<Check> {
    let mut checks = Vec::new();

    checks.push(claude_cli_check());

    let store = ProfileStore::new(paths.clone());
    match store.list() {
        Ok((profiles, unmanaged)) => {
            checks.push(Check {
                ok: true,
                label: format!("{} managed profile(s)", profiles.len()),
            });
            for p in &profiles {
                if p.name == "default" && !p.managed {
                    continue;
                }
                checks.push(file_perm_check(paths, &p.name));
                checks.extend(broken_symlink_checks(&p.home));
                checks.extend(plaintext_secret_checks(p));
            }
            if unmanaged.is_empty() {
                checks.push(Check {
                    ok: true,
                    label: "no unmanaged ~/.claude-* dirs".into(),
                });
            } else {
                let names: Vec<_> = unmanaged.iter().map(|u| u.name.as_str()).collect();
                checks.push(Check {
                    ok: false,
                    label: format!("unmanaged dirs importable: {}", names.join(", ")),
                });
            }
        }
        Err(e) => checks.push(Check {
            ok: false,
            label: format!("profile store unreadable: {e}"),
        }),
    }

    checks
}

fn claude_cli_check() -> Check {
    match std::process::Command::new("claude")
        .arg("--version")
        .output()
    {
        Ok(out) if out.status.success() => {
            let v = String::from_utf8_lossy(&out.stdout);
            Check {
                ok: true,
                label: format!("claude CLI: {}", v.trim()),
            }
        }
        Ok(out) => Check {
            ok: false,
            label: format!("claude --version failed (status {})", out.status),
        },
        Err(e) => Check {
            ok: false,
            label: format!("claude CLI not found on PATH: {e}"),
        },
    }
}

fn file_perm_check(paths: &Paths, name: &str) -> Check {
    let file = paths.profile_file(name);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match fs::metadata(&file) {
            Ok(m) => {
                let mode = m.permissions().mode() & 0o777;
                Check {
                    ok: mode == 0o600,
                    label: format!("{} perms {:o} (want 600)", file.display(), mode),
                }
            }
            Err(e) => Check {
                ok: false,
                label: format!("{} unreadable: {e}", file.display()),
            },
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (paths, name);
        Check {
            ok: true,
            label: "perm check skipped (non-unix)".into(),
        }
    }
}

/// Template symlinks that no longer resolve (e.g. source file deleted).
fn broken_symlink_checks(home: &Path) -> Vec<Check> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(home) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() && !path.exists() {
            out.push(Check {
                ok: false,
                label: format!("broken symlink: {}", path.display()),
            });
        }
    }
    out
}

/// Tokens sitting in the TOML as plaintext instead of the keychain.
fn plaintext_secret_checks(p: &crate::profile::Profile) -> Vec<Check> {
    p.env
        .iter()
        .filter(|(k, v)| crate::secret::is_secret_key(k) && !crate::secret::is_marker(v))
        .map(|(k, _)| Check {
            ok: false,
            label: format!(
                "profile {:?} stores {k} in plaintext — re-save it to move to keychain",
                p.name
            ),
        })
        .collect()
}

pub fn print_report(checks: &[Check]) -> i32 {
    let mut failures = 0;
    for c in checks {
        println!("{c}");
        if !c.ok {
            failures += 1;
        }
    }
    println!("\n{} check(s), {} problem(s)", checks.len(), failures);
    if failures > 0 {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Paths;

    #[test]
    fn doctor_flags_bad_perms_and_broken_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let user_home = tmp.path().join("home");
        let ccp_home = tmp.path().join("ccp");
        std::fs::create_dir_all(user_home.join(".claude")).unwrap();

        let paths = Paths::new(&user_home, &ccp_home);
        let store = ProfileStore::new(paths.clone());
        store
            .create("kimi", None, Default::default())
            .expect("create");

        // Loosen perms on purpose.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file = paths.profile_file("kimi");
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
            // Broken symlink inside the profile home.
            std::os::unix::fs::symlink(
                user_home.join(".claude/does-not-exist"),
                user_home.join(".claude-kimi/AGENTS.md"),
            )
            .unwrap();
        }

        let checks = run(&paths);
        let labels: Vec<_> = checks.iter().map(|c| &c.label).collect();
        assert!(labels.iter().any(|l| l.contains("perms 644")), "{labels:?}");
        assert!(
            labels.iter().any(|l| l.contains("broken symlink")),
            "{labels:?}"
        );
    }
}

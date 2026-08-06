//! Scan a profile home's `projects/**/*.jsonl` into session summaries.

use serde::Serialize;
use std::fs;
use std::io;
use std::path::Path;

/// Only read this much of each transcript — the first user message and cwd
/// live at the top, and sessions can be hundreds of MB.
const HEAD_BYTES: usize = 64 * 1024;
const PREVIEW_CHARS: usize = 100;

#[derive(Clone, Debug, Serialize)]
pub struct SessionSummary {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub preview: String,
    /// Seconds since Unix epoch.
    pub modified: u64,
}

/// Session transcript files are `<uuid>.jsonl`; sub-agent sidecars are not resumable.
fn looks_like_session_id(stem: &str) -> bool {
    stem.len() >= 32 && stem.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// Public check used by the launch endpoint to validate `resume` ids.
pub fn is_resumable_id(stem: &str) -> bool {
    looks_like_session_id(stem)
}

/// First user text + cwd, extracted from the head of a transcript.
/// Returns None if there is no real user message in the head.
fn summarize_head(bytes: &[u8]) -> (Option<String>, Option<String>) {
    let text = String::from_utf8_lossy(bytes);
    let mut cwd = None;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if cwd.is_none() {
            cwd = v.get("cwd").and_then(|c| c.as_str()).map(str::to_string);
        }
        if v.get("type").and_then(|t| t.as_str()) != Some("user") {
            continue;
        }
        let content = v.pointer("/message/content");
        let preview = match content {
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(serde_json::Value::Array(items)) => items
                .iter()
                .find(|i| i.get("type").and_then(|t| t.as_str()) == Some("text"))
                .and_then(|i| i.get("text"))
                .and_then(|t| t.as_str())
                .map(str::to_string),
            _ => None,
        };
        if let Some(p) = preview {
            let flat = p.split_whitespace().collect::<Vec<_>>().join(" ");
            let preview: String = flat.chars().take(PREVIEW_CHARS).collect();
            if !preview.is_empty() {
                return (cwd, Some(preview));
            }
        }
    }
    (cwd, None)
}

/// Newest-first session summaries for a profile home. A missing projects
/// dir is an empty list, not an error.
pub fn scan(home: &Path, limit: usize) -> io::Result<Vec<SessionSummary>> {
    let projects = home.join("projects");
    if !projects.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for project in fs::read_dir(projects)? {
        let project = project?;
        if !project.file_type()?.is_dir() {
            continue;
        }
        for file in fs::read_dir(project.path())? {
            let file = file?;
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if !looks_like_session_id(stem) {
                continue;
            }
            let meta = file.metadata()?;
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let head = read_head(&path)?;
            let (cwd, preview) = summarize_head(&head);
            let Some(preview) = preview else { continue };
            out.push(SessionSummary {
                id: stem.to_string(),
                cwd,
                preview,
                modified,
            });
        }
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.modified));
    out.truncate(limit);
    Ok(out)
}

fn read_head(path: &Path) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = fs::File::open(path)?;
    let mut buf = vec![0u8; HEAD_BYTES];
    let n = f.read(&mut buf)?;
    buf.truncate(n);
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_session(dir: &Path, name: &str, lines: &[&str]) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(name), lines.join("\n")).unwrap();
    }

    #[test]
    fn session_id_shape() {
        assert!(looks_like_session_id(
            "1e4f5b3c-9a2d-4c8e-bf01-23456789abcd"
        ));
        assert!(!looks_like_session_id("agent-abc"));
        assert!(!looks_like_session_id("short"));
    }

    #[test]
    fn head_extracts_cwd_and_first_user_text() {
        let (cwd, preview) = summarize_head(
            br#"{"type":"system","cwd":"/work/proj"}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"x"}]},"cwd":"/work/proj"}
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"fix   the\nbug"}]},"cwd":"/work/proj"}"#,
        );
        assert_eq!(cwd.as_deref(), Some("/work/proj"));
        assert_eq!(preview.as_deref(), Some("fix the bug"));
    }

    #[test]
    fn scan_orders_newest_first_and_skips_sidecars() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("projects/-work-proj");
        write_session(
            &proj,
            "1e4f5b3c-9a2d-4c8e-bf01-23456789abcd.jsonl",
            &[r#"{"type":"user","message":{"content":"old"},"cwd":"/work/proj"}"#],
        );
        write_session(
            &proj,
            "2e4f5b3c-9a2d-4c8e-bf01-23456789abce.jsonl",
            &[r#"{"type":"user","message":{"content":"new"},"cwd":"/work/proj"}"#],
        );
        write_session(&proj, "agent-1e4f.jsonl", &[r#"{"type":"user"}"#]);

        // Ensure deterministic mtimes: bump the second file's mtime.
        let newer = proj.join("2e4f5b3c-9a2d-4c8e-bf01-23456789abce.jsonl");
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
        let f = fs::File::options().write(true).open(&newer).unwrap();
        f.set_modified(later).unwrap();

        let sessions = scan(tmp.path(), 50).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].preview, "new");
        assert_eq!(sessions[1].preview, "old");
        assert_eq!(sessions[0].cwd.as_deref(), Some("/work/proj"));
    }
}

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
fn summarize_head(bytes: &[u8]) -> io::Result<(Option<String>, Option<String>)> {
    let text = String::from_utf8_lossy(bytes);
    let mut cwd = None;
    let records = agent_sessions::read_raw_from(
        io::Cursor::new(text.as_bytes()),
        &agent_sessions::RawReadOptions {
            max_read_bytes: None,
            max_line_bytes: None,
            ..Default::default()
        },
    )
    .map_err(io::Error::other)?;
    for record in records {
        let record = record.map_err(io::Error::other)?;
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&record.bytes) else {
            continue;
        };
        let projected =
            agent_sessions::project_transcript(agent_sessions::Agent::ClaudeCode, &value);
        if cwd.is_none() {
            cwd = projected.meta.cwd;
        }
        let Some(message) = projected
            .message
            .filter(|m| m.role == agent_sessions::Role::User)
        else {
            continue;
        };
        // A malformed first text block must not promote a later block into the preview.
        if value
            .pointer("/message/content")
            .and_then(serde_json::Value::as_array)
            .and_then(|items| {
                items.iter().find(|item| {
                    item.get("type").and_then(serde_json::Value::as_str) == Some("text")
                })
            })
            .is_some_and(|item| !item.get("text").is_some_and(serde_json::Value::is_string))
        {
            continue;
        }
        if let Some(text) = message.first_text() {
            let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let preview: String = flat.chars().take(PREVIEW_CHARS).collect();
            if !preview.is_empty() {
                return Ok((cwd, Some(preview)));
            }
        }
    }
    Ok((cwd, None))
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
            let (cwd, preview) = summarize_head(&head)?;
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
    let file = fs::File::open(path)?;
    let mut buf = Vec::new();
    file.take(HEAD_BYTES as u64).read_to_end(&mut buf)?;
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
        ).unwrap();
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
    #[test]
    fn preview_keeps_first_block_and_first_cwd() {
        let bytes = br#"{"type":"system","cwd":"/first"}
{"type":"user","cwd":"/later","message":{"content":[{"type":"text","text":"first block"},{"type":"text","text":"second block"}]}}
"#;
        let (cwd, preview) = summarize_head(bytes).unwrap();
        assert_eq!(cwd.as_deref(), Some("/first"));
        assert_eq!(preview.as_deref(), Some("first block"));
    }

    #[test]
    fn empty_or_invalid_first_block_does_not_promote_later_blocks() {
        for first in [
            r#"{"type":"text","text":""}"#,
            r#"{"type":"text","text":7}"#,
        ] {
            let input = format!(
                r#"{{"type":"user","message":{{"content":[{first},{{"type":"text","text":"ignore"}}]}}}}
{{"type":"user","message":{{"content":"next message"}}}}"#
            );
            assert_eq!(
                summarize_head(input.as_bytes()).unwrap().1.as_deref(),
                Some("next message")
            );
        }
    }

    #[test]
    fn preview_limit_counts_unicode_characters_and_ignores_partial_tail() {
        let input = format!(
            r#"{{"type":"user","timestamp":"invalid","message":{{"content":"{}"}}}}
{{"type":"user","message":{{"content":"unfinished"#,
            "界".repeat(120)
        );
        assert_eq!(
            summarize_head(input.as_bytes()).unwrap().1,
            Some("界".repeat(100))
        );
        assert_eq!(summarize_head(b"{bad}\n{\"type\":").unwrap().1, None);
    }

    #[test]
    fn read_head_bounds_large_files_and_accepts_short_files() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("head.jsonl");
        fs::write(&path, b"short").unwrap();
        assert_eq!(read_head(&path).unwrap(), b"short");
        fs::write(&path, vec![b'x'; HEAD_BYTES + 100]).unwrap();
        assert_eq!(read_head(&path).unwrap().len(), HEAD_BYTES);
    }
}

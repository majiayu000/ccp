//! Token-usage stats parsed from session transcripts (`projects/**/*.jsonl`).
//!
//! Assistant lines carry `message.usage` + `message.model` + `timestamp`.
//! Files are filtered by mtime first, so "last 30 days" never reads old data.

use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::path::Path;

#[derive(Clone, Debug, Default, Serialize)]
pub struct DayUsage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct UsageReport {
    /// Date-keyed rows, ascending (`YYYY-MM-DD`).
    pub days: BTreeMap<String, DayUsage>,
    pub totals: DayUsage,
    /// Model -> total output tokens.
    pub models: BTreeMap<String, u64>,
    /// Transcript files parsed (after the mtime filter).
    pub files: usize,
}

impl UsageReport {
    fn add(&mut self, date: String, u: &Usage, model: Option<&str>) {
        let day = self.days.entry(date).or_default();
        day.input += u.input;
        day.output += u.output;
        day.cache_read += u.cache_read;
        day.cache_write += u.cache_write;
        self.totals.input += u.input;
        self.totals.output += u.output;
        self.totals.cache_read += u.cache_read;
        self.totals.cache_write += u.cache_write;
        if let Some(m) = model {
            *self.models.entry(m.into()).or_default() += u.output;
        }
    }
}

#[derive(Default)]
struct Usage {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
}

/// Aggregate usage from transcripts modified at or after `since_secs` (unix).
pub fn scan(home: &Path, since_secs: u64) -> io::Result<UsageReport> {
    let mut report = UsageReport::default();
    let projects = home.join("projects");
    if !projects.is_dir() {
        return Ok(report);
    }
    let mut seen_messages: HashSet<String> = HashSet::new();
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
            let modified = file
                .metadata()?
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if modified < since_secs {
                continue;
            }
            report.files += 1;
            parse_file(&path, &mut seen_messages, &mut report)?;
        }
    }
    Ok(report)
}

fn parse_file(path: &Path, seen: &mut HashSet<String>, report: &mut UsageReport) -> io::Result<()> {
    use std::io::BufRead;
    let file = fs::File::open(path)?;
    for line in io::BufReader::new(file).lines() {
        let line = line?;
        // Cheap gate: usage objects only appear on assistant usage lines.
        if !line.contains("\"usage\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(usage) = v.pointer("/message/usage") else {
            continue;
        };
        // Retries can replay the same message; count it once.
        if let Some(id) = v.pointer("/message/id").and_then(|i| i.as_str()) {
            if !seen.insert(id.to_string()) {
                continue;
            }
        }
        let u = Usage {
            input: num(usage.get("input_tokens")),
            output: num(usage.get("output_tokens")),
            cache_read: num(usage.get("cache_read_input_tokens")),
            cache_write: num(usage.get("cache_creation_input_tokens")),
        };
        if u.input + u.output + u.cache_read + u.cache_write == 0 {
            continue;
        }
        let date = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .map(|t| t.chars().take(10).collect::<String>())
            .unwrap_or_else(|| "unknown".into());
        let model = v.pointer("/message/model").and_then(|m| m.as_str());
        report.add(date, &u, model);
    }
    Ok(())
}

fn num(v: Option<&serde_json::Value>) -> u64 {
    v.and_then(|v| v.as_u64()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_per_day_and_dedups_messages() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("projects/-x");
        fs::create_dir_all(&proj).unwrap();
        fs::write(
            proj.join("1e4f5b3c-9a2d-4c8e-bf01-23456789abcd.jsonl"),
            concat!(
                r#"{"type":"assistant","timestamp":"2026-08-01T10:00:00Z","message":{"id":"m1","model":"claude-x","usage":{"input_tokens":10,"output_tokens":5}}}"#,
                "\n",
                r#"{"type":"assistant","timestamp":"2026-08-01T11:00:00Z","message":{"id":"m1","model":"claude-x","usage":{"input_tokens":10,"output_tokens":5}}}"#,
                "\n",
                r#"{"type":"assistant","timestamp":"2026-08-02T09:00:00Z","message":{"id":"m2","model":"claude-y","usage":{"input_tokens":3,"output_tokens":7,"cache_read_input_tokens":100}}}"#,
                "\n",
                r#"{"type":"user","message":{"content":"hi"}}"#,
                "\n"
            ),
        )
        .unwrap();

        let report = scan(tmp.path(), 0).unwrap();
        assert_eq!(report.files, 1);
        let d1 = &report.days["2026-08-01"];
        assert_eq!((d1.input, d1.output), (10, 5), "duplicate m1 counted once");
        let d2 = &report.days["2026-08-02"];
        assert_eq!((d2.input, d2.output, d2.cache_read), (3, 7, 100));
        assert_eq!(report.totals.output, 12);
        assert_eq!(report.models["claude-x"], 5);
        assert_eq!(report.models["claude-y"], 7);
    }

    #[test]
    fn mtime_filter_skips_old_files() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("projects/-x");
        fs::create_dir_all(&proj).unwrap();
        let f = proj.join("1e4f5b3c-9a2d-4c8e-bf01-23456789abcd.jsonl");
        fs::write(
            &f,
            r#"{"type":"assistant","timestamp":"2020-01-01T00:00:00Z","message":{"usage":{"input_tokens":1,"output_tokens":1}}}"#,
        )
        .unwrap();
        let file = fs::File::options().write(true).open(&f).unwrap();
        file.set_modified(
            std::time::SystemTime::now() - std::time::Duration::from_secs(90 * 86400),
        )
        .unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let report = scan(tmp.path(), now - 30 * 86400).unwrap();
        assert_eq!(report.files, 0);
        assert_eq!(report.totals.output, 0);
    }
}

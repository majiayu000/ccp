//! Built-in provider presets, embedded from `presets.toml` at compile time.
//! Field shape mirrors cc-switch's preset table so it can be synced upstream.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::OnceLock;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preset {
    pub key: String,
    pub label: String,
    pub category: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub website_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_url: Option<String>,
    #[serde(default = "default_api_format")]
    pub api_format: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

fn default_api_format() -> String {
    "anthropic".to_string()
}

#[derive(Deserialize)]
struct PresetFile {
    preset: Vec<Preset>,
}

static PRESETS: OnceLock<Vec<Preset>> = OnceLock::new();

/// All built-in presets. The embedded file is validated by unit tests, so a
/// parse failure here is a build-time bug, not a runtime condition.
pub fn load() -> &'static [Preset] {
    PRESETS.get_or_init(|| {
        toml::from_str::<PresetFile>(include_str!("../presets.toml"))
            .expect("embedded presets.toml must be valid")
            .preset
    })
}

pub fn find(key: &str) -> Option<&'static Preset> {
    load().iter().find(|p| p.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_presets_parse_and_are_unique() {
        let presets = load();
        assert!(!presets.is_empty());
        let mut keys = std::collections::HashSet::new();
        for p in presets {
            assert!(
                keys.insert(p.key.as_str()),
                "duplicate preset key {}",
                p.key
            );
            assert!(!p.label.is_empty());
        }
    }

    #[test]
    fn moonshot_preset_matches_cc_switch() {
        let p = find("moonshot").expect("moonshot preset exists");
        assert_eq!(
            p.env.get("ANTHROPIC_BASE_URL").map(String::as_str),
            Some("https://api.moonshot.cn/anthropic")
        );
    }
}

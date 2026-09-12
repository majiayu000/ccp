//! The embedded single-page UI.
//!
//! Security note: untrusted session fields (especially `cwd`) must not be
//! interpolated into `onclick` / other JS-handler source. HTML entity decoding
//! undoes `esc()` before the script engine runs. Prefer `data-*` + listeners.

pub const INDEX_HTML: &str = include_str!("../static/index.html");

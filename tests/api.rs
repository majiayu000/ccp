//! In-memory API tests: router + tempdir-backed store, no live server.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ccp::paths::Paths;
use ccp::web::{router, AppState};
use serde_json::{json, Value};
use tower::ServiceExt;

struct Fixture {
    app: Router,
    _tmp: tempfile::TempDir,
    user_home: std::path::PathBuf,
    ccp_home: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let user_home = tmp.path().join("home");
    let ccp_home = tmp.path().join("ccp");
    std::fs::create_dir_all(user_home.join(".claude")).expect("default home");
    let paths = Paths::new(&user_home, &ccp_home);
    let app = router(AppState::new(paths));
    Fixture {
        app,
        _tmp: tmp,
        user_home,
        ccp_home,
    }
}

async fn call(app: Router, req: Request<Body>) -> (StatusCode, Value) {
    let res = app.oneshot(req).await.expect("response");
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .expect("body");
    let json = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, json)
}

fn get0(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).unwrap()
}

fn json_req(method: &str, path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn default_profile_always_listed() {
    let f = fixture();
    let (status, body) = call(f.app, get0("/api/profiles")).await;
    assert_eq!(status, StatusCode::OK);
    let profiles = body["profiles"].as_array().unwrap();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0]["name"], "default");
    assert_eq!(profiles[0]["managed"], false);
    assert!(body["unmanaged"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn presets_include_moonshot() {
    let f = fixture();
    let (status, body) = call(f.app, get0("/api/presets")).await;
    assert_eq!(status, StatusCode::OK);
    let list = body.as_array().unwrap();
    let moonshot = list.iter().find(|p| p["key"] == "moonshot").unwrap();
    assert_eq!(
        moonshot["env"]["ANTHROPIC_BASE_URL"],
        "https://api.moonshot.cn/anthropic"
    );
}

#[tokio::test]
async fn create_profile_happy_path() {
    let f = fixture();
    // Template source exists in the fake ~/.claude.
    std::fs::write(f.user_home.join(".claude/CLAUDE.md"), "# rules").unwrap();

    let (status, body) = call(
        f.app,
        json_req(
            "POST",
            "/api/profiles",
            json!({
                "name": "kimi",
                "preset": "moonshot",
                "env": {"ANTHROPIC_BASE_URL": "https://api.moonshot.cn/anthropic",
                        "ANTHROPIC_AUTH_TOKEN": "sk-test-1234567890"}
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // Secrets are masked in responses.
    let token = body["env"]["ANTHROPIC_AUTH_TOKEN"].as_str().unwrap();
    assert!(token.contains('…'), "masked: {token}");
    assert!(!token.contains("cdef"));

    // Home dir created at top level, template symlinked, file is 0600.
    let home = f.user_home.join(".claude-kimi");
    assert!(home.is_dir());
    assert_eq!(
        std::fs::read_link(home.join("CLAUDE.md")).unwrap(),
        f.user_home.join(".claude/CLAUDE.md")
    );
    let meta = std::fs::metadata(f.ccp_home.join("profiles/kimi.toml")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }
    // Raw file holds the real token.
    let raw = std::fs::read_to_string(f.ccp_home.join("profiles/kimi.toml")).unwrap();
    assert!(raw.contains("sk-test-1234567890"));
}

#[tokio::test]
async fn create_rejects_bad_names_and_duplicates() {
    let f = fixture();
    for bad in ["Kimi", "-x", "a b", "a/b", ""] {
        let (status, _) = call(
            f.app.clone(),
            json_req("POST", "/api/profiles", json!({"name": bad, "env": {}})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "name {bad:?}");
    }
    let (s1, _) = call(
        f.app.clone(),
        json_req("POST", "/api/profiles", json!({"name": "kimi", "env": {}})),
    )
    .await;
    assert_eq!(s1, StatusCode::CREATED);
    let (s2, _) = call(
        f.app.clone(),
        json_req("POST", "/api/profiles", json!({"name": "kimi", "env": {}})),
    )
    .await;
    assert_eq!(s2, StatusCode::CONFLICT);
    let (s3, _) = call(
        f.app.clone(),
        json_req(
            "POST",
            "/api/profiles",
            json!({"name": "x", "preset": "nope", "env": {}}),
        ),
    )
    .await;
    assert_eq!(s3, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn existing_dir_requires_import_not_create() {
    let f = fixture();
    std::fs::create_dir_all(f.user_home.join(".claude-zhipu")).unwrap();

    let (status, body) = call(
        f.app.clone(),
        json_req("POST", "/api/profiles", json!({"name": "zhipu", "env": {}})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // Discovery surfaces it, import adopts it without touching contents.
    let (_, list) = call(f.app.clone(), get0("/api/profiles")).await;
    assert_eq!(list["unmanaged"][0]["name"], "zhipu");

    let (status, _) = call(
        f.app.clone(),
        json_req("POST", "/api/profiles/import", json!({"name": "zhipu"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    // No template symlinks on import.
    assert!(!f.user_home.join(".claude-zhipu/CLAUDE.md").exists());
}

#[tokio::test]
async fn update_merges_and_empty_deletes() {
    let f = fixture();
    call(
        f.app.clone(),
        json_req(
            "POST",
            "/api/profiles",
            json!({"name": "kimi", "env": {"A": "1", "B": "2"}}),
        ),
    )
    .await;
    let (status, body) = call(
        f.app.clone(),
        json_req(
            "PUT",
            "/api/profiles/kimi",
            json!({"env": {"B": "", "C": "3"}}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let raw = std::fs::read_to_string(f.ccp_home.join("profiles/kimi.toml")).unwrap();
    assert!(raw.contains("A = \"1\""));
    assert!(!raw.contains("\"2\""));
    assert!(raw.contains("C = \"3\""));
    assert!(body["env"]["A"].is_string());
}

#[tokio::test]
async fn delete_requires_confirm_and_keeps_home() {
    let f = fixture();
    call(
        f.app.clone(),
        json_req("POST", "/api/profiles", json!({"name": "kimi", "env": {}})),
    )
    .await;

    let (status, _) = call(
        f.app.clone(),
        Request::delete("/api/profiles/kimi")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = call(
        f.app.clone(),
        Request::delete("/api/profiles/kimi?confirm=true")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!f.ccp_home.join("profiles/kimi.toml").exists());
    assert!(f.user_home.join(".claude-kimi").is_dir(), "home kept");

    // Reserved profile cannot be deleted.
    let (status, _) = call(
        f.app.clone(),
        Request::delete("/api/profiles/default?confirm=true")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn shared_overlay_roundtrip_masked() {
    let f = fixture();
    let (status, _) = call(
        f.app.clone(),
        json_req(
            "PUT",
            "/api/shared",
            json!({"env": {"CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1", "SECRET": "abcdefgh12345678"}}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, body) = call(f.app.clone(), get0("/api/shared")).await;
    let secret = body["SECRET"].as_str().unwrap();
    assert!(secret.contains('…'));
    assert!(!secret.contains("cdef"));

    // Empty value deletes the key.
    call(
        f.app.clone(),
        json_req("PUT", "/api/shared", json!({"env": {"SECRET": ""}})),
    )
    .await;
    let (_, body) = call(f.app.clone(), get0("/api/shared")).await;
    assert!(body["SECRET"].is_null());
}

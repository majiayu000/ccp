//! In-memory API tests: router + tempdir-backed store, no live server.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ccp::paths::Paths;
use ccp::secret::SecretStore;
use ccp::web::{router, AppState};
use serde_json::{json, Value};
use tower::ServiceExt;

struct Fixture {
    app: Router,
    _tmp: tempfile::TempDir,
    user_home: std::path::PathBuf,
    ccp_home: std::path::PathBuf,
    secrets: std::sync::Arc<ccp::secret::MemoryStore>,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let user_home = tmp.path().join("home");
    let ccp_home = tmp.path().join("ccp");
    std::fs::create_dir_all(user_home.join(".claude")).expect("default home");
    let paths = Paths::new(&user_home, &ccp_home);
    let secrets = std::sync::Arc::new(ccp::secret::MemoryStore::default());
    let app = router(AppState::with_parts(paths, secrets.clone(), |_cmd| Ok(())));
    Fixture {
        app,
        _tmp: tmp,
        user_home,
        ccp_home,
        secrets,
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
    // Token never appears in responses or files — keychain marker instead.
    assert_eq!(
        body["env"]["ANTHROPIC_AUTH_TOKEN"].as_str().unwrap(),
        "🔑 keychain"
    );

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
    // File holds only the marker; the secret backend holds the real token.
    let raw = std::fs::read_to_string(f.ccp_home.join("profiles/kimi.toml")).unwrap();
    assert!(raw.contains("@keychain"));
    assert!(!raw.contains("sk-test"));
    assert_eq!(
        f.secrets
            .get("kimi", "ANTHROPIC_AUTH_TOKEN")
            .unwrap()
            .as_deref(),
        Some("sk-test-1234567890")
    );
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
async fn sessions_list_and_launch_with_injected_launcher() {
    use std::sync::{Arc, Mutex};
    let tmp = tempfile::tempdir().unwrap();
    let user_home = tmp.path().join("home");
    let ccp_home = tmp.path().join("ccp");
    let session_dir = user_home.join(".claude/projects/-work-proj");
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(
        session_dir.join("1e4f5b3c-9a2d-4c8e-bf01-23456789abcd.jsonl"),
        "{\"type\":\"user\",\"message\":{\"content\":\"hello world\"},\"cwd\":\"/work/proj\"}\n",
    )
    .unwrap();

    let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let cap = captured.clone();
    let paths = Paths::new(&user_home, &ccp_home);
    let secrets = Arc::new(ccp::secret::MemoryStore::default());
    let app = router(AppState::with_parts(paths, secrets, move |cmd| {
        cap.lock().unwrap().push(cmd.to_string());
        Ok(())
    }));

    // Sessions for the default profile come from its real projects/ dir.
    let (status, body) = call(app.clone(), get0("/api/profiles/default/sessions")).await;
    assert_eq!(status, StatusCode::OK);
    let sessions = body.as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["preview"], "hello world");
    assert_eq!(sessions[0]["cwd"], "/work/proj");

    // Unknown profile → 404.
    let (status, _) = call(app.clone(), get0("/api/profiles/ghost/sessions")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Launch: shared overlay under profile env, resume id validated.
    call(
        app.clone(),
        json_req("PUT", "/api/shared", json!({"env": {"SHARED": "yes"}})),
    )
    .await;
    call(
        app.clone(),
        json_req(
            "POST",
            "/api/profiles",
            json!({"name": "kimi", "env": {"ANTHROPIC_AUTH_TOKEN": "sk-x"}}),
        ),
    )
    .await;
    let (status, _) = call(
        app.clone(),
        json_req(
            "POST",
            "/api/profiles/kimi/launch",
            json!({"resume": "not-a-uuid"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = call(
        app.clone(),
        json_req(
            "POST",
            "/api/profiles/kimi/launch",
            json!({"resume": "1e4f5b3c-9a2d-4c8e-bf01-23456789abcd", "cwd": "/work/proj"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let cmds = captured.lock().unwrap();
    assert_eq!(cmds.len(), 1);
    let cmd = &cmds[0];
    assert!(cmd.starts_with("cd '/work/proj' && env CLAUDE_CONFIG_DIR="));
    assert!(cmd.contains(".claude-kimi"));
    assert!(cmd.contains("SHARED='yes'"));
    assert!(cmd.contains("ANTHROPIC_AUTH_TOKEN='sk-x'"));
    assert!(cmd.ends_with("claude --resume '1e4f5b3c-9a2d-4c8e-bf01-23456789abcd'"));
}

#[tokio::test]
async fn test_endpoint_probes_profile_base_url() {
    async fn models(headers: axum::http::HeaderMap) -> (StatusCode, &'static str) {
        if headers.get("x-api-key").is_some() {
            (StatusCode::OK, "[]")
        } else {
            (StatusCode::UNAUTHORIZED, "nope")
        }
    }
    let stub = Router::new().route("/v1/models", axum::routing::get(models));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });

    let f = fixture();
    call(
        f.app.clone(),
        json_req(
            "POST",
            "/api/profiles",
            json!({"name": "kimi", "env": {
                "ANTHROPIC_BASE_URL": format!("http://{addr}"),
                "ANTHROPIC_AUTH_TOKEN": "sk-x",
            }}),
        ),
    )
    .await;
    let (status, body) = call(
        f.app.clone(),
        json_req("POST", "/api/profiles/kimi/test", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["auth"], "ok");
    assert_eq!(body["http_status"], 200);
    assert!(body["latency_ms"].is_number());

    // Unreachable upstream → 502 with a readable message.
    call(
        f.app.clone(),
        json_req(
            "PUT",
            "/api/profiles/kimi",
            json!({"env": {"ANTHROPIC_BASE_URL": "http://127.0.0.1:1"}}),
        ),
    )
    .await;
    let (status, body) = call(
        f.app.clone(),
        json_req("POST", "/api/profiles/kimi/test", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("connection failed"));
}

#[tokio::test]
async fn test_endpoint_rejects_ssrf_and_untrusted_credential_hosts() {
    let f = fixture();

    // Attacker-controlled HTTPS host must not receive Keychain tokens.
    call(
        f.app.clone(),
        json_req(
            "POST",
            "/api/profiles",
            json!({"name": "evil", "env": {
                "ANTHROPIC_BASE_URL": "https://evil.example.com",
                "ANTHROPIC_AUTH_TOKEN": "sk-exfiltrate",
            }}),
        ),
    )
    .await;
    let (status, body) = call(
        f.app.clone(),
        json_req("POST", "/api/profiles/evil/test", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    let err = body["error"].as_str().unwrap();
    assert!(err.contains("untrusted host"), "{err}");
    assert!(err.contains("evil.example.com"), "{err}");

    // Link-local metadata SSRF target is rejected before any request.
    call(
        f.app.clone(),
        json_req(
            "POST",
            "/api/profiles",
            json!({"name": "meta", "env": {
                "ANTHROPIC_BASE_URL": "http://169.254.169.254",
                "ANTHROPIC_AUTH_TOKEN": "sk-x",
            }}),
        ),
    )
    .await;
    let (status, body) = call(
        f.app.clone(),
        json_req("POST", "/api/profiles/meta/test", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    let err = body["error"].as_str().unwrap();
    assert!(
        err.contains("refusing") || err.contains("non-HTTPS"),
        "{err}"
    );
}

#[tokio::test]
async fn export_masks_and_import_respects_policy() {
    let f = fixture();
    call(
        f.app.clone(),
        json_req(
            "POST",
            "/api/profiles",
            json!({"name": "kimi", "preset": "moonshot",
                   "env": {"ANTHROPIC_AUTH_TOKEN": "sk-secret-1234567890"}}),
        ),
    )
    .await;

    // Default export shows the keychain marker, never the token.
    let (status, body) = call(f.app.clone(), get0("/api/export")).await;
    assert_eq!(status, StatusCode::OK);
    let kimi = body["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "kimi")
        .unwrap()
        .clone();
    assert_eq!(
        kimi["env"]["ANTHROPIC_AUTH_TOKEN"].as_str().unwrap(),
        "🔑 keychain"
    );

    // include_secrets round-trips the real value.
    let (_, body) = call(f.app.clone(), get0("/api/export?include_secrets=true")).await;
    assert_eq!(
        body["profiles"][1]["env"]["ANTHROPIC_AUTH_TOKEN"],
        "sk-secret-1234567890"
    );

    // Omitted policy defaults to skip and leaves existing untouched.
    let (status, body) = call(
        f.app.clone(),
        json_req(
            "POST",
            "/api/import",
            json!({"profiles": [{"name": "kimi", "env": {"A": "1"}},
                                {"name": "zhipu", "env": {"B": "2"}}]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["skipped"], json!(["kimi"]));
    assert_eq!(body["created"], json!(["zhipu"]));
    let raw = std::fs::read_to_string(f.ccp_home.join("profiles/kimi.toml")).unwrap();
    assert!(!raw.contains("A = "));

    // Explicit policy "skip" also leaves existing untouched (GUI path).
    let (status, body) = call(
        f.app.clone(),
        json_req(
            "POST",
            "/api/import",
            json!({"policy": "skip",
                   "profiles": [{"name": "kimi", "env": {"A": "should-not-apply"}}]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["skipped"], json!(["kimi"]));
    let raw = std::fs::read_to_string(f.ccp_home.join("profiles/kimi.toml")).unwrap();
    assert!(!raw.contains("A = "));

    // Overwrite merges into existing.
    let (_, body) = call(
        f.app.clone(),
        json_req(
            "POST",
            "/api/import",
            json!({"policy": "overwrite",
                   "profiles": [{"name": "kimi", "env": {"A": "1"}}]}),
        ),
    )
    .await;
    assert_eq!(body["updated"], json!(["kimi"]));
    let raw = std::fs::read_to_string(f.ccp_home.join("profiles/kimi.toml")).unwrap();
    assert!(raw.contains("A = \"1\""));

    // Bad policy is a 400, not a silent skip.
    let (status, _) = call(
        f.app.clone(),
        json_req(
            "POST",
            "/api/import",
            json!({"policy": "merge", "profiles": []}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[test]
fn index_html_resume_avoids_onclick_esc_cwd() {
    // SEC-XSS (#9): esc() inside onclick is HTML-decoded before JS parses the
    // handler, so a cwd containing ' can break out. Resume must use data-* +
    // addEventListener instead of embedding esc(s.cwd) in onclick source.
    let html = ccp::page::INDEX_HTML;
    assert!(
        !html.contains("onclick=\"doResume('${esc(name)}','${esc(s.id)}','${esc(s.cwd"),
        "INDEX_HTML must not embed esc()'d cwd inside doResume onclick handlers"
    );
    assert!(
        html.contains("data-cwd=") && html.contains("addEventListener"),
        "resume buttons should wire doResume via data-cwd + addEventListener"
    );
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

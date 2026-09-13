//! axum server: routes, handlers, error mapping.

use crate::connect;
use crate::launch;
use crate::page::INDEX_HTML;
use crate::paths::Paths;
use crate::presets;
use crate::profile::{mask, Profile, ProfileStore, StoreError, Unmanaged};
use crate::secret;
use crate::sessions;
use crate::usage;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::{get, post, put};
use axum::Router;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};

/// How many sessions to return per profile.
const SESSION_LIST_LIMIT: usize = 50;

/// Confirmation header value required for plaintext secret export.
const EXPORT_SECRETS_CONFIRM: &str = "export-secrets";

/// Launch hook: production spawns Terminal via osascript; tests capture.
type Launcher = std::sync::Arc<dyn Fn(&str) -> std::io::Result<()> + Send + Sync>;

#[derive(Clone)]
pub struct AppState {
    store: std::sync::Arc<ProfileStore>,
    launcher: Launcher,
    /// Bearer token required on every `/api/*` request.
    api_token: String,
    /// Listen port — used to allowlist `Origin` / `Host` for loopback only.
    port: u16,
}

impl AppState {
    pub fn new(paths: Paths, port: u16) -> std::io::Result<Self> {
        Self::with_launcher(paths, port, launch::spawn_terminal)
    }

    pub fn with_launcher(
        paths: Paths,
        port: u16,
        launcher: impl Fn(&str) -> std::io::Result<()> + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let token = load_or_create_api_token(&paths)?;
        Ok(Self::with_parts(
            paths,
            std::sync::Arc::new(secret::KeychainStore),
            launcher,
            token,
            port,
        ))
    }

    pub fn with_parts(
        paths: Paths,
        secrets: std::sync::Arc<dyn secret::SecretStore>,
        launcher: impl Fn(&str) -> std::io::Result<()> + Send + Sync + 'static,
        api_token: impl Into<String>,
        port: u16,
    ) -> Self {
        Self {
            store: std::sync::Arc::new(ProfileStore::with_secrets(paths, secrets)),
            launcher: std::sync::Arc::new(launcher),
            api_token: api_token.into(),
            port,
        }
    }
}

/// Load `~/.ccp/api_token` or create a fresh random one (mode 0600).
pub fn load_or_create_api_token(paths: &Paths) -> std::io::Result<String> {
    let path = paths.api_token_file();
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim().to_string();
        if !trimmed.is_empty() {
            return Ok(trimmed);
        }
    }
    let token = generate_api_token()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::File::create(&path)?;
    file.write_all(token.as_bytes())?;
    file.write_all(b"\n")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(token)
}

fn generate_api_token() -> std::io::Result<String> {
    let mut bytes = [0u8; 32];
    #[cfg(unix)]
    {
        std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    }
    #[cfg(not(unix))]
    {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        std::time::SystemTime::now().hash(&mut h);
        std::process::id().hash(&mut h);
        bytes[..8].copy_from_slice(&h.finish().to_le_bytes());
    }
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/api/presets", get(list_presets))
        .route("/api/profiles", get(list_profiles).post(create_profile))
        .route("/api/profiles/import", post(import_profile))
        .route(
            "/api/profiles/{name}",
            put(update_profile).delete(delete_profile),
        )
        .route("/api/profiles/{name}/sessions", get(list_sessions))
        .route("/api/profiles/{name}/launch", post(launch_profile))
        .route("/api/profiles/{name}/test", post(test_profile))
        .route("/api/profiles/{name}/usage", get(profile_usage))
        .route(
            "/api/export",
            get(export_profiles_get).post(export_profiles_post),
        )
        .route("/api/import", post(import_profiles))
        .route("/api/shared", get(get_shared).put(put_shared))
        .layer(middleware::from_fn_with_state(state.clone(), protect_api))
        .with_state(state.clone());

    Router::new()
        .route("/", get(index))
        .merge(api)
        .with_state(state)
}

pub async fn serve(paths: Paths, port: u16, iterm: bool) -> Result<(), Box<dyn std::error::Error>> {
    let state = if iterm {
        AppState::with_launcher(paths, port, launch::spawn_iterm)?
    } else {
        AppState::new(paths, port)?
    };
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    eprintln!("ccp GUI: http://127.0.0.1:{port}");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Require loopback `Host`/`Origin` and a bearer token on `/api/*`.
/// Intentionally does **not** emit `Access-Control-Allow-Private-Network`.
async fn protect_api(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if let Some(host) = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
    {
        if !host_allowed(host, state.port) {
            return Err(ApiError {
                status: StatusCode::FORBIDDEN,
                message: "refusing non-loopback Host".into(),
            });
        }
    }
    if let Some(origin) = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
    {
        if !origin_allowed(origin, state.port) {
            return Err(ApiError {
                status: StatusCode::FORBIDDEN,
                message: "cross-origin request rejected".into(),
            });
        }
    }
    if !request_has_valid_token(&req, &state.api_token) {
        return Err(ApiError {
            status: StatusCode::UNAUTHORIZED,
            message: "missing or invalid API token".into(),
        });
    }
    Ok(next.run(req).await)
}

fn host_allowed(host: &str, port: u16) -> bool {
    let host = host.trim();
    host.eq_ignore_ascii_case(&format!("127.0.0.1:{port}"))
        || host.eq_ignore_ascii_case(&format!("localhost:{port}"))
        || host.eq_ignore_ascii_case("127.0.0.1")
        || host.eq_ignore_ascii_case("localhost")
        || host.eq_ignore_ascii_case(&format!("[::1]:{port}"))
        || host.eq_ignore_ascii_case("[::1]")
}

fn origin_allowed(origin: &str, port: u16) -> bool {
    let origin = origin.trim();
    origin.eq_ignore_ascii_case(&format!("http://127.0.0.1:{port}"))
        || origin.eq_ignore_ascii_case(&format!("http://localhost:{port}"))
        || origin.eq_ignore_ascii_case(&format!("http://[::1]:{port}"))
}

fn request_has_valid_token(req: &Request, expected: &str) -> bool {
    if let Some(auth) = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(token) = auth.strip_prefix("Bearer ") {
            return token == expected;
        }
    }
    req.headers()
        .get("x-ccp-token")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t == expected)
}

// ---------- DTOs (secrets always masked on the way out) ----------

#[derive(Serialize)]
struct ProfileView {
    name: String,
    home: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    preset: Option<String>,
    managed: bool,
    env: BTreeMap<String, String>,
}

impl From<Profile> for ProfileView {
    fn from(p: Profile) -> Self {
        Self {
            name: p.name,
            home: p.home.display().to_string(),
            preset: p.preset,
            managed: p.managed,
            env: p
                .env
                .into_iter()
                .map(|(k, v)| {
                    let shown = if secret::is_marker(&v) {
                        "🔑 keychain".to_string()
                    } else {
                        mask(&v)
                    };
                    (k, shown)
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct ProfilesResponse {
    profiles: Vec<ProfileView>,
    unmanaged: Vec<Unmanaged>,
}

#[derive(Deserialize)]
pub struct CreateProfile {
    name: String,
    #[serde(default)]
    preset: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Deserialize)]
pub struct EnvPatch {
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Deserialize)]
pub struct NameOnly {
    name: String,
}

#[derive(Deserialize)]
pub struct DeleteQuery {
    #[serde(default)]
    confirm: bool,
    #[serde(default)]
    purge: bool,
}

#[derive(Deserialize)]
pub struct LaunchRequest {
    #[serde(default)]
    resume: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Deserialize)]
pub struct ExportQuery {
    #[serde(default)]
    include_secrets: bool,
}

#[derive(Deserialize)]
pub struct ExportBody {
    #[serde(default)]
    include_secrets: bool,
}

#[derive(Deserialize)]
pub struct UsageQuery {
    #[serde(default = "default_usage_days")]
    days: u64,
}

fn default_usage_days() -> u64 {
    30
}

#[derive(Deserialize)]
pub struct ImportRequest {
    profiles: Vec<ImportProfile>,
    #[serde(default)]
    shared: Option<BTreeMap<String, String>>,
    /// "skip" (default) or "overwrite" for existing profiles.
    #[serde(default)]
    policy: Option<String>,
}

#[derive(Deserialize)]
pub struct ImportProfile {
    name: String,
    #[serde(default)]
    preset: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Default, Serialize)]
pub struct ImportSummary {
    created: Vec<String>,
    updated: Vec<String>,
    skipped: Vec<String>,
    errors: Vec<ImportError>,
}

#[derive(Serialize)]
pub struct ImportError {
    name: String,
    error: String,
}

// ---------- handlers ----------

async fn index(State(state): State<AppState>) -> Html<String> {
    // Token is hex from /dev/urandom; still escape for a JS string literal.
    let escaped = state
        .api_token
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\'', "\\'")
        .replace('<', "\\u003c");
    Html(INDEX_HTML.replace("%%CCP_API_TOKEN%%", &escaped))
}

async fn list_presets() -> Json<&'static [presets::Preset]> {
    Json(presets::load())
}

async fn list_profiles(State(state): State<AppState>) -> Result<Json<ProfilesResponse>, ApiError> {
    let (profiles, unmanaged) = state.store.list()?;
    Ok(Json(ProfilesResponse {
        profiles: profiles.into_iter().map(ProfileView::from).collect(),
        unmanaged,
    }))
}

async fn create_profile(
    State(state): State<AppState>,
    Json(body): Json<CreateProfile>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(key) = &body.preset {
        if presets::find(key).is_none() {
            return Err(ApiError::bad_request(format!("unknown preset {key:?}")));
        }
    }
    let profile = state.store.create(&body.name, body.preset, body.env)?;
    Ok((StatusCode::CREATED, Json(ProfileView::from(profile))))
}

async fn import_profile(
    State(state): State<AppState>,
    Json(body): Json<NameOnly>,
) -> Result<impl IntoResponse, ApiError> {
    let profile = state.store.import(&body.name)?;
    Ok((StatusCode::CREATED, Json(ProfileView::from(profile))))
}

async fn update_profile(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<EnvPatch>,
) -> Result<Json<ProfileView>, ApiError> {
    let profile = state.store.update_env(&name, body.env)?;
    Ok(Json(ProfileView::from(profile)))
}

async fn delete_profile(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<DeleteQuery>,
) -> Result<StatusCode, ApiError> {
    if !q.confirm {
        return Err(ApiError::bad_request("pass ?confirm=true to delete".into()));
    }
    state.store.delete(&name, q.purge)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_sessions(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Vec<sessions::SessionSummary>>, ApiError> {
    let profile = state.store.get(&name)?;
    let list = sessions::scan(&profile.home, SESSION_LIST_LIMIT)?;
    Ok(Json(list))
}

async fn launch_profile(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<LaunchRequest>,
) -> Result<StatusCode, ApiError> {
    let profile = state.store.get(&name)?;
    // Shared overlay under the profile's own env; profile wins on conflict.
    // Both are keychain-resolved so the child process gets real values.
    let mut env = state.store.resolve_shared()?;
    env.extend(state.store.resolve_env(&name)?);
    if let Some(id) = &body.resume {
        if !sessions::is_resumable_id(id) {
            return Err(ApiError::bad_request(format!("invalid session id {id:?}")));
        }
    }
    let cmd = launch::build_command(
        &profile.home,
        &env,
        body.resume.as_deref(),
        body.cwd.as_deref(),
    )
    .map_err(|e| ApiError::bad_request(e.to_string()))?;
    (state.launcher)(&cmd).map_err(|e| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("failed to open Terminal: {e}"),
    })?;
    Ok(StatusCode::NO_CONTENT)
}

async fn test_profile(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<connect::Connectivity>, ApiError> {
    state.store.get(&name)?;
    let mut env = state.store.resolve_shared()?;
    env.extend(state.store.resolve_env(&name)?);
    let base = env.get("ANTHROPIC_BASE_URL").map(String::as_str);
    let token = env
        .get("ANTHROPIC_AUTH_TOKEN")
        .or_else(|| env.get("ANTHROPIC_API_KEY"))
        .map(String::as_str);
    let result = connect::check(base, token).await.map_err(|e| ApiError {
        status: StatusCode::BAD_GATEWAY,
        message: e,
    })?;
    Ok(Json(result))
}

/// Masked export only. `?include_secrets=true` on GET is rejected — plaintext
/// must use POST with an explicit confirmation header (not cacheable).
async fn export_profiles_get(
    State(state): State<AppState>,
    Query(q): Query<ExportQuery>,
) -> Result<impl IntoResponse, ApiError> {
    if q.include_secrets {
        return Err(ApiError::bad_request(
            "plaintext export requires POST /api/export with header X-Ccp-Confirm: export-secrets"
                .into(),
        ));
    }
    let body = build_export(&state, false)?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(body)))
}

async fn export_profiles_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ExportBody>,
) -> Result<impl IntoResponse, ApiError> {
    if body.include_secrets {
        let confirm = headers.get("x-ccp-confirm").and_then(|v| v.to_str().ok());
        if confirm != Some(EXPORT_SECRETS_CONFIRM) {
            return Err(ApiError::bad_request(
                "plaintext export requires header X-Ccp-Confirm: export-secrets".into(),
            ));
        }
    }
    let payload = build_export(&state, body.include_secrets)?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(payload)))
}

fn build_export(state: &AppState, include_secrets: bool) -> Result<serde_json::Value, ApiError> {
    let (profiles, _) = state.store.list()?;
    let mut out_profiles = Vec::new();
    for p in profiles {
        let env = if include_secrets {
            state.store.resolve_env(&p.name)?
        } else {
            p.env
                .into_iter()
                .map(|(k, v)| {
                    let shown = if secret::is_marker(&v) {
                        "🔑 keychain".to_string()
                    } else {
                        mask(&v)
                    };
                    (k, shown)
                })
                .collect()
        };
        out_profiles.push(serde_json::json!({
            "name": p.name,
            "preset": p.preset,
            "env": env,
        }));
    }
    let shared = if include_secrets {
        state.store.resolve_shared()?
    } else {
        state
            .store
            .read_shared()?
            .into_iter()
            .map(|(k, v)| {
                let shown = if secret::is_marker(&v) {
                    "🔑 keychain".to_string()
                } else {
                    mask(&v)
                };
                (k, shown)
            })
            .collect()
    };
    Ok(serde_json::json!({
        "version": 1,
        "profiles": out_profiles,
        "shared": shared,
        "warning": if include_secrets {
            "this file contains plaintext tokens — store it accordingly"
        } else {
            "tokens are masked or keychain references; re-enter them after import"
        },
    }))
}

async fn profile_usage(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<UsageQuery>,
) -> Result<Json<usage::UsageReport>, ApiError> {
    let profile = state.store.get(&name)?;
    let days = q.days.clamp(1, 365);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let since = now.saturating_sub(days * 86400);
    let report = tokio::task::spawn_blocking(move || usage::scan(&profile.home, since))
        .await
        .map_err(|e| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("usage scan failed: {e}"),
        })??;
    Ok(Json(report))
}

async fn import_profiles(
    State(state): State<AppState>,
    Json(body): Json<ImportRequest>,
) -> Result<Json<ImportSummary>, ApiError> {
    let mut summary = ImportSummary::default();
    let overwrite = match body.policy.as_deref() {
        None | Some("skip") => false,
        Some("overwrite") => true,
        Some(_) => {
            return Err(ApiError::bad_request(
                "policy must be \"skip\" or \"overwrite\"".into(),
            ));
        }
    };
    for p in body.profiles {
        let name = p.name.as_str();
        let exists = state.store.get(name).map(|pr| pr.managed).unwrap_or(false)
            || name == crate::paths::DEFAULT_PROFILE;
        let result = if exists && !overwrite {
            summary.skipped.push(name.into());
            continue;
        } else if exists || state.store.import(name).is_ok() {
            // Existing profile, or an unmanaged dir we can adopt on the fly.
            state.store.update_env(name, p.env)
        } else {
            state.store.create(name, p.preset.clone(), p.env)
        };
        match result {
            Ok(_) if exists => summary.updated.push(name.into()),
            Ok(_) => summary.created.push(name.into()),
            Err(e) => summary.errors.push(ImportError {
                name: name.into(),
                error: e.to_string(),
            }),
        }
    }
    if let Some(shared) = body.shared {
        state.store.write_shared(shared)?;
    }
    Ok(Json(summary))
}

async fn get_shared(
    State(state): State<AppState>,
) -> Result<Json<BTreeMap<String, String>>, ApiError> {
    let env = state.store.read_shared()?;
    Ok(Json(
        env.into_iter()
            .map(|(k, v)| {
                let shown = if secret::is_marker(&v) {
                    "🔑 keychain".to_string()
                } else {
                    mask(&v)
                };
                (k, shown)
            })
            .collect(),
    ))
}

async fn put_shared(
    State(state): State<AppState>,
    Json(body): Json<EnvPatch>,
) -> Result<StatusCode, ApiError> {
    state.store.write_shared(body.env)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- errors ----------

pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: String) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message,
        }
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        let status = match &e {
            StoreError::InvalidName(_) | StoreError::InvalidEnvKey(_) | StoreError::Reserved(_) => {
                StatusCode::BAD_REQUEST
            }
            StoreError::Exists(_) | StoreError::HomeExists(_) => StatusCode::CONFLICT,
            StoreError::NotFound(_) => StatusCode::NOT_FOUND,
            StoreError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self {
            status,
            message: e.to_string(),
        }
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("io error: {e}"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

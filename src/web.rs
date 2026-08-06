//! axum server: routes, handlers, error mapping.

use crate::connect;
use crate::launch;
use crate::page::INDEX_HTML;
use crate::paths::Paths;
use crate::presets;
use crate::profile::{mask, Profile, ProfileStore, StoreError, Unmanaged};
use crate::sessions;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json};
use axum::routing::{get, post, put};
use axum::Router;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// How many sessions to return per profile.
const SESSION_LIST_LIMIT: usize = 50;

/// Launch hook: production spawns Terminal via osascript; tests capture.
type Launcher = std::sync::Arc<dyn Fn(&str) -> std::io::Result<()> + Send + Sync>;

#[derive(Clone)]
pub struct AppState {
    store: std::sync::Arc<ProfileStore>,
    launcher: Launcher,
}

impl AppState {
    pub fn new(paths: Paths) -> Self {
        Self::with_launcher(paths, launch::spawn_terminal)
    }

    pub fn with_launcher(
        paths: Paths,
        launcher: impl Fn(&str) -> std::io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            store: std::sync::Arc::new(ProfileStore::new(paths)),
            launcher: std::sync::Arc::new(launcher),
        }
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
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
        .route("/api/export", get(export_profiles))
        .route("/api/import", post(import_profiles))
        .route("/api/shared", get(get_shared).put(put_shared))
        .with_state(state)
}

pub async fn serve(paths: Paths, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let app = router(AppState::new(paths));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    eprintln!("ccp GUI: http://127.0.0.1:{port}");
    axum::serve(listener, app).await?;
    Ok(())
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
            env: p.env.into_iter().map(|(k, v)| (k, mask(&v))).collect(),
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

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
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
    // Shared overlay goes under the profile's own env; profile wins on conflict.
    let mut env = state.store.read_shared()?;
    env.extend(profile.env);
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
    );
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
    let profile = state.store.get(&name)?;
    let mut env = state.store.read_shared()?;
    env.extend(profile.env);
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

async fn export_profiles(
    State(state): State<AppState>,
    Query(q): Query<ExportQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (profiles, _) = state.store.list()?;
    let mask_map = |env: BTreeMap<String, String>| -> BTreeMap<String, String> {
        if q.include_secrets {
            env
        } else {
            env.into_iter().map(|(k, v)| (k, mask(&v))).collect()
        }
    };
    let shared = state.store.read_shared()?;
    Ok(Json(serde_json::json!({
        "version": 1,
        "profiles": profiles
            .into_iter()
            .map(|p| serde_json::json!({
                "name": p.name,
                "preset": p.preset,
                "env": mask_map(p.env),
            }))
            .collect::<Vec<_>>(),
        "shared": mask_map(shared),
        "warning": if q.include_secrets {
            "this file contains plaintext tokens — store it accordingly"
        } else {
            "tokens are masked; re-enter them after import"
        },
    })))
}

async fn import_profiles(
    State(state): State<AppState>,
    Json(body): Json<ImportRequest>,
) -> Result<Json<ImportSummary>, ApiError> {
    let mut summary = ImportSummary::default();
    let overwrite = body.policy.as_deref() == Some("overwrite");
    if body.policy.is_some() && !overwrite {
        return Err(ApiError::bad_request(
            "policy must be \"skip\" or \"overwrite\"".into(),
        ));
    }
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
    Ok(Json(env.into_iter().map(|(k, v)| (k, mask(&v))).collect()))
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
            StoreError::InvalidName(_) | StoreError::Reserved(_) => StatusCode::BAD_REQUEST,
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

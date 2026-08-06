//! axum server: routes, handlers, error mapping.

use crate::page::INDEX_HTML;
use crate::paths::Paths;
use crate::presets;
use crate::profile::{mask, Profile, ProfileStore, StoreError, Unmanaged};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json};
use axum::routing::{get, post, put};
use axum::Router;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone)]
pub struct AppState {
    store: std::sync::Arc<ProfileStore>,
}

impl AppState {
    pub fn new(paths: Paths) -> Self {
        Self {
            store: std::sync::Arc::new(ProfileStore::new(paths)),
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

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

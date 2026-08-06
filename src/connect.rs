//! Connectivity check for a profile's upstream endpoint.
//!
//! Probe: `GET {base}/v1/models` with the standard Anthropic auth headers.
//! Almost every Anthropic-compatible relay implements this route; the result
//! distinguishes "network unreachable" from "endpoint alive but auth wrong".

use serde::Serialize;
use std::time::Instant;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Auth {
    Ok,
    Failed,
    /// Endpoint responded, but not with a clear 200/401 — or no token to test with.
    Unknown,
}

#[derive(Clone, Debug, Serialize)]
pub struct Connectivity {
    pub url: String,
    pub latency_ms: u128,
    pub http_status: u16,
    pub auth: Auth,
}

pub async fn check(base_url: Option<&str>, token: Option<&str>) -> Result<Connectivity, String> {
    let base = base_url
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(DEFAULT_BASE_URL);
    let url = format!("{}/v1/models", base.trim_end_matches('/'));

    let client = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .build()
        .map_err(|e| format!("http client init failed: {e}"))?;

    let mut req = client.get(&url).header("anthropic-version", "2023-06-01");
    if let Some(t) = token.filter(|s| !s.is_empty()) {
        req = req
            .header("x-api-key", t)
            .header("authorization", format!("Bearer {t}"));
    }

    let start = Instant::now();
    let resp = req.send().await.map_err(|e| {
        if e.is_timeout() {
            format!("timeout after {}s", PROBE_TIMEOUT.as_secs())
        } else if e.is_connect() {
            format!("connection failed: {e}")
        } else {
            format!("request failed: {e}")
        }
    })?;

    let status = resp.status().as_u16();
    let auth = match status {
        200 => Auth::Ok,
        401 | 403 => Auth::Failed,
        _ => Auth::Unknown,
    };
    Ok(Connectivity {
        url,
        latency_ms: start.elapsed().as_millis(),
        http_status: status,
        auth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;
    use axum::routing::get;
    use axum::Router;

    async fn stub_server() -> String {
        async fn models_status(headers: HeaderMap) -> (axum::http::StatusCode, &'static str) {
            if headers.get("x-api-key").is_some() {
                (axum::http::StatusCode::OK, "[]")
            } else {
                (axum::http::StatusCode::UNAUTHORIZED, "unauthorized")
            }
        }
        let app = Router::new().route("/v1/models", get(models_status));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn check_reports_auth_ok_and_failed() {
        let base = stub_server().await;
        let ok = check(Some(&base), Some("sk-test")).await.unwrap();
        assert_eq!(ok.auth, Auth::Ok);
        assert_eq!(ok.http_status, 200);
        assert!(ok.url.ends_with("/v1/models"));

        let bad = check(Some(&base), None).await.unwrap();
        assert_eq!(bad.auth, Auth::Failed);
        assert_eq!(bad.http_status, 401);
    }

    #[tokio::test]
    async fn check_reports_unreachable() {
        // Port 1 is reserved and refuses connections on loopback.
        let err = check(Some("http://127.0.0.1:1"), None).await.unwrap_err();
        assert!(err.contains("connection failed"), "{err}");
    }
}

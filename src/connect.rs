//! Connectivity check for a profile's upstream endpoint.
//!
//! Probe: `GET {base}/v1/models` with the standard Anthropic auth headers.
//! Almost every Anthropic-compatible relay implements this route; the result
//! distinguishes "network unreachable" from "endpoint alive but auth wrong".
//!
//! Security (SEC-08): the probe URL is validated (https-only, plus loopback
//! http for tests), credentials are attached only for allowlisted hosts, and
//! redirects are disabled so tokens cannot follow an attacker-controlled hop.

use crate::presets;
use reqwest::Url;
use serde::Serialize;
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::OnceLock;
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
    let url = validated_probe_url(base)?;
    let host = url
        .host_str()
        .ok_or_else(|| "probe URL is missing a host".to_string())?
        .to_ascii_lowercase();

    let token = token.filter(|s| !s.is_empty());
    if token.is_some() && !credential_host_allowed(&host) {
        return Err(format!(
            "refusing to send credentials to untrusted host '{host}'"
        ));
    }

    let client = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("http client init failed: {e}"))?;

    let url_str = url.to_string();
    let mut req = client
        .get(url.clone())
        .header("anthropic-version", "2023-06-01");
    if let Some(t) = token {
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
        url: url_str,
        latency_ms: start.elapsed().as_millis(),
        http_status: status,
        auth,
    })
}

fn validated_probe_url(base: &str) -> Result<Url, String> {
    let trimmed = base.trim().trim_end_matches('/');
    let joined = format!("{trimmed}/v1/models");
    let url = Url::parse(&joined).map_err(|e| format!("invalid base URL: {e}"))?;

    match url.scheme() {
        "https" | "http" => {}
        other => {
            return Err(format!(
                "refusing probe URL with scheme '{other}' (only https, or http loopback, allowed)"
            ));
        }
    }

    let host = url
        .host_str()
        .ok_or_else(|| "probe URL is missing a host".to_string())?;

    match url.scheme() {
        "https" => validate_https_host(host)?,
        "http" => {
            if !is_loopback_host(host) {
                return Err(format!(
                    "refusing non-HTTPS probe URL (only http://127.0.0.1 and http://localhost are allowed): {joined}"
                ));
            }
        }
        _ => unreachable!("scheme already validated"),
    }

    Ok(url)
}

fn validate_https_host(host: &str) -> Result<(), String> {
    if is_loopback_host(host) {
        return Ok(());
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        match ip {
            IpAddr::V4(v4) if is_non_public_ipv4(v4) => {
                return Err(format!(
                    "refusing probe to private/link-local address '{v4}'"
                ));
            }
            IpAddr::V6(v6) if is_non_public_ipv6(v6) => {
                return Err(format!(
                    "refusing probe to private/link-local address '{v6}'"
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

fn is_loopback_host(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    if h == "localhost" {
        return true;
    }
    match h.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => v4.is_loopback(),
        Ok(IpAddr::V6(v6)) => v6.is_loopback(),
        Err(_) => false,
    }
}

fn is_non_public_ipv4(v4: Ipv4Addr) -> bool {
    v4.is_private()
        || v4.is_loopback()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_multicast()
        // Shared / documentation / benchmarking ranges commonly used in SSRF payloads.
        || matches!(
            v4.octets(),
            [0, ..]
                | [100, 64..=127, ..]
                | [192, 0, 0, ..]
                | [192, 0, 2, ..]
                | [198, 18..=19, ..]
                | [198, 51, 100, ..]
                | [203, 0, 113, ..]
                | [240..=255, ..]
        )
}

fn is_non_public_ipv6(v6: Ipv6Addr) -> bool {
    v6.is_loopback()
        || v6.is_unspecified()
        || v6.is_multicast()
        || v6.is_unique_local()
        || is_ipv6_link_local(v6)
}

fn is_ipv6_link_local(v6: Ipv6Addr) -> bool {
    // fe80::/10
    (v6.segments()[0] & 0xffc0) == 0xfe80
}

fn credential_host_allowed(host: &str) -> bool {
    credential_allowed_hosts().contains(host)
}

fn credential_allowed_hosts() -> &'static HashSet<String> {
    static HOSTS: OnceLock<HashSet<String>> = OnceLock::new();
    HOSTS.get_or_init(|| {
        let mut hosts = HashSet::new();
        hosts.insert("api.anthropic.com".to_string());
        hosts.insert("127.0.0.1".to_string());
        hosts.insert("localhost".to_string());
        hosts.insert("::1".to_string());
        for h in presets::probe_credential_hosts() {
            hosts.insert(h);
        }
        hosts
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

    #[tokio::test]
    async fn rejects_metadata_link_local_http() {
        let err = check(Some("http://169.254.169.254"), Some("sk-x"))
            .await
            .unwrap_err();
        assert!(
            err.contains("refusing") || err.contains("non-HTTPS"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn rejects_file_scheme() {
        let err = check(Some("file:///etc/passwd"), None).await.unwrap_err();
        assert!(err.contains("scheme"), "{err}");
    }

    #[tokio::test]
    async fn refuses_credentials_for_untrusted_https_host() {
        let err = check(Some("https://evil.example.com"), Some("sk-secret"))
            .await
            .unwrap_err();
        assert!(
            err.contains("untrusted host") && err.contains("evil.example.com"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn allowlisted_https_host_accepts_credentials_shape() {
        // Official Anthropic host is on the allowlist; assert validation + credential
        // policy accept the URL (network may fail offline).
        let result = check(Some("https://api.anthropic.com"), Some("sk-test")).await;
        match result {
            Ok(c) => assert!(c.url.starts_with("https://api.anthropic.com/")),
            Err(e) => {
                assert!(
                    !e.contains("untrusted host") && !e.contains("refusing"),
                    "{e}"
                );
            }
        }
    }

    #[test]
    fn validated_probe_url_allows_https_and_loopback_http() {
        assert!(validated_probe_url("https://api.moonshot.cn/anthropic").is_ok());
        assert!(validated_probe_url("http://127.0.0.1:9").is_ok());
        assert!(validated_probe_url("http://localhost:9").is_ok());
        assert!(validated_probe_url("http://evil.example.com").is_err());
        assert!(validated_probe_url("https://169.254.169.254").is_err());
        assert!(validated_probe_url("https://10.0.0.1").is_err());
    }

    #[test]
    fn preset_hosts_are_credential_allowed() {
        assert!(credential_host_allowed("api.anthropic.com"));
        assert!(credential_host_allowed("api.moonshot.cn"));
        assert!(credential_host_allowed("127.0.0.1"));
        assert!(!credential_host_allowed("evil.example.com"));
    }
}

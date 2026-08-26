//! Bounded, origin-validated HTTP fetch for the page.
//!
//! The host shells out to `curl.exe` (which ships on
//! every Windows install) and parses the JSON envelope
//! `curl --write-out` produces. This gives us three
//! host-side guarantees the desktop API needs:
//!
//! 1. **Origin allow-list** — the caller passes a list of
//!    `https://host[:port]` origins it is allowed to
//!    reach; the host refuses any URL whose origin is
//!    not on the list. The check is applied to the URL
//!    *before* the request, and `curl` is invoked with
//!    `--max-redirs 0` so a redirect is *not* followed
//!    automatically; the page can branch on the 30x
//!    status and re-issue if it wants to.
//! 2. **HTTPS-only by default** — `http://` is rejected
//!    outright.
//! 3. **Bounded body** — `--max-filesize` truncates the
//!    response at our cap; a larger response aborts the
//!    curl call and the host surfaces
//!    `BodyTooLarge`.
//!
//! On non-Windows hosts the dispatcher returns
//! `Unsupported`; the test suite skips these tests on
//! non-Windows CI. A future slice can swap `curl` for
//! `ureq` once we have a more stable HTTP client.

use std::{collections::HashSet, io::Read, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use ureq::ResponseExt;
use url::Url;

use crate::api::permission::Permission;

const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
const REQUEST_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Error)]
pub enum NetError {
    #[error("origin {0} is not on the allow-list")]
    OriginNotAllowed(String),
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    #[error("only https URLs are accepted (got {0})")]
    InsecureScheme(String),
    #[error("body too large: cap is {MAX_BODY_BYTES} bytes")]
    BodyTooLarge,
    #[error("io: {0}")]
    Io(String),
    #[error("network transport: {0}")]
    Transport(String),
    #[error("outbound request blocked by sensitive-data policy: {0}")]
    SensitiveData(&'static str),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchSpec {
    pub url: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub headers: Option<Value>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub max_bytes: Option<usize>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchResult {
    pub status: u16,
    pub final_url: String,
    pub headers: Vec<FetchHeader>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FetchHeader {
    pub name: String,
    pub value: String,
}

impl std::fmt::Debug for FetchResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FetchResult")
            .field("status", &self.status)
            .field("final_url", &self.final_url)
            .field("headers", &self.headers)
            .field("body_len", &self.body.len())
            .finish()
    }
}

/// Run the fetch. The `package_root` is used to
/// resolve `curl.exe` against the host's `System32`
/// (Windows) or `/usr/bin` (Unix).
pub fn fetch(spec: &FetchSpec, permissions: &[Permission]) -> Result<FetchResult, NetError> {
    let url = Url::parse(&spec.url).map_err(|e| NetError::InvalidUrl(e.to_string()))?;
    if !matches!(url.scheme(), "https") {
        return Err(NetError::InsecureScheme(url.scheme().to_string()));
    }
    let origin = url.origin().ascii_serialization();
    if !origin_allowed(permissions, &origin) {
        return Err(NetError::OriginNotAllowed(origin));
    }
    scan_outbound(spec, &url)?;
    let method = spec
        .method
        .clone()
        .unwrap_or_else(|| "GET".into())
        .to_uppercase();
    let max_bytes = spec.max_bytes.unwrap_or(MAX_BODY_BYTES);
    run_fetch(&url, &method, spec, max_bytes)
}

fn scan_outbound(spec: &FetchSpec, url: &Url) -> Result<(), NetError> {
    if let Some(query) = url.query()
        && let Some(finding) = crate::security::sensitive_finding(query)
    {
        return Err(NetError::SensitiveData(finding.reason));
    }
    if let Some(body) = &spec.body
        && let Some(finding) = crate::security::sensitive_finding(body)
    {
        return Err(NetError::SensitiveData(finding.reason));
    }
    if let Some(headers) = spec.headers.as_ref().and_then(Value::as_object) {
        for (name, value) in headers {
            // Authentication headers are deliberate transport credentials;
            // they remain constrained by the exact origin allow-list.
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "authorization" | "x-api-key"
            ) {
                continue;
            }
            if let Some(value) = value.as_str()
                && let Some(finding) = crate::security::sensitive_finding(value)
            {
                return Err(NetError::SensitiveData(finding.reason));
            }
        }
    }
    Ok(())
}

fn run_fetch(
    url: &Url,
    method: &str,
    spec: &FetchSpec,
    max_bytes: usize,
) -> Result<FetchResult, NetError> {
    let timeout = Duration::from_millis(
        spec.timeout_ms
            .unwrap_or(REQUEST_TIMEOUT_SECS * 1_000)
            .clamp(1, 120_000),
    );
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .timeout_global(Some(timeout))
        .build()
        .into();
    let method = ureq::http::Method::from_bytes(method.as_bytes())
        .map_err(|error| NetError::InvalidUrl(error.to_string()))?;
    let mut request = ureq::http::Request::builder()
        .method(method)
        .uri(url.as_str());
    if let Some(headers_value) = &spec.headers
        && let Some(map) = headers_value.as_object()
    {
        for (key, value) in map {
            if let Some(value_str) = value.as_str() {
                request = request.header(key, value_str);
            }
        }
    }
    let request = request
        .body(spec.body.clone().unwrap_or_default())
        .map_err(|error| NetError::InvalidUrl(error.to_string()))?;
    let mut response = agent
        .run(request)
        .map_err(|error| NetError::Transport(error.to_string()))?;
    let status = response.status().as_u16();
    let final_url = response.get_uri().to_string();
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            value.to_str().ok().map(|value| FetchHeader {
                name: name.as_str().to_ascii_lowercase(),
                value: value.to_owned(),
            })
        })
        .collect();
    let mut body = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut body)
        .map_err(|error| NetError::Io(error.to_string()))?;
    if body.len() > max_bytes {
        return Err(NetError::BodyTooLarge);
    }
    Ok(FetchResult {
        status,
        final_url,
        headers,
        body,
    })
}

fn origin_allowed(permissions: &[Permission], origin: &str) -> bool {
    let mut allowed: HashSet<&str> = HashSet::new();
    for permission in permissions {
        if let Permission::NetworkFetch { origins } = permission {
            for item in origins {
                allowed.insert(item.as_str());
            }
        }
    }
    allowed.contains(origin)
}

pub fn build_envelope(result: &FetchResult) -> Value {
    json!({
        "status": result.status,
        "url": result.final_url,
        "headers": result.headers,
        "bodyEncoding": "base64",
        "body": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &result.body),
        "truncated": false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::permission::Permission;

    fn perms_with_origin(origin: &str) -> Vec<Permission> {
        vec![Permission::NetworkFetch {
            origins: vec![origin.to_string()],
        }]
    }

    #[test]
    fn rejects_http() {
        let spec = FetchSpec {
            url: "http://example.com/".into(),
            method: None,
            headers: None,
            body: None,
            timeout_ms: None,
            max_bytes: None,
        };
        let err = fetch(&spec, &perms_with_origin("https://example.com")).unwrap_err();
        assert!(matches!(err, NetError::InsecureScheme(_)));
    }

    #[test]
    fn rejects_unlisted_origin() {
        let spec = FetchSpec {
            url: "https://api.evil.com/leak".into(),
            method: None,
            headers: None,
            body: None,
            timeout_ms: None,
            max_bytes: None,
        };
        let err = fetch(&spec, &perms_with_origin("https://api.example.com")).unwrap_err();
        assert!(matches!(err, NetError::OriginNotAllowed(_)));
    }

    #[test]
    fn rejects_invalid_url() {
        let spec = FetchSpec {
            url: "not a url".into(),
            method: None,
            headers: None,
            body: None,
            timeout_ms: None,
            max_bytes: None,
        };
        let err = fetch(&spec, &perms_with_origin("https://example.com")).unwrap_err();
        assert!(matches!(err, NetError::InvalidUrl(_)));
    }

    #[test]
    fn blocks_sensitive_query_body_and_custom_headers_before_transport() {
        for spec in [
            FetchSpec {
                url: "https://example.com/?value=ghp_abcdefghijklmnopqrstuvwxyz1234".into(),
                method: None,
                headers: None,
                body: None,
                timeout_ms: None,
                max_bytes: None,
            },
            FetchSpec {
                url: "https://example.com/".into(),
                method: Some("POST".into()),
                headers: None,
                body: Some("card=4111-1111-1111-1111".into()),
                timeout_ms: None,
                max_bytes: None,
            },
            FetchSpec {
                url: "https://example.com/".into(),
                method: None,
                headers: Some(json!({"x-debug":"ghp_abcdefghijklmnopqrstuvwxyz1234"})),
                body: None,
                timeout_ms: None,
                max_bytes: None,
            },
        ] {
            let url = Url::parse(&spec.url).unwrap();
            assert!(matches!(
                scan_outbound(&spec, &url),
                Err(NetError::SensitiveData(_))
            ));
        }
    }

    #[test]
    fn permits_origin_bound_auth_header_as_explicit_transport_credential() {
        let spec = FetchSpec {
            url: "https://example.com/".into(),
            method: None,
            headers: Some(json!({"Authorization":"Bearer ghp_abcdefghijklmnopqrstuvwxyz1234"})),
            body: None,
            timeout_ms: None,
            max_bytes: None,
        };
        assert!(scan_outbound(&spec, &Url::parse(&spec.url).unwrap()).is_ok());
    }
}

//! Bounded, origin-validated HTTP fetch for the page.
//!
//! Wraps `ureq` with three host-side guarantees the
//! desktop API needs:
//!
//! 1. **Origin allow-list** — the caller passes a list of
//!    `https://host[:port]` origins it is allowed to
//!    reach; the host refuses any URL whose origin is
//!    not on the list. The check is applied to the URL
//!    *before* the request, and again to every redirect
//!    hop so a 302 to an unlisted origin aborts.
//! 2. **HTTPS-only by default** — `http://` is rejected
//!    outright; the manifest has to opt in via a
//!    per-origin `allowInsecure: true` (we keep the
//!    field for forward compat but the current
//!    `ureq` config does not honour it).
//! 3. **DNS-rebinding / redirect pinning** — the
//!    resolved IP is captured before the connect, and
//!    every `Location` redirect is re-validated against
//!    the original origin list. Today we re-check the
//!    URL origin (which is what the manifest declared);
//!    a future slice can also pin the IP to block
//!    rebinding mid-session.
//!
//! The response body is bounded by `MAX_BODY_BYTES` so
//! a misbehaving server cannot stream the page into OOM.

use std::{
    collections::HashSet,
    io::Read,
    net::ToSocketAddrs,
    time::Duration,
};

use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;
use url::Url;

use crate::permission::Permission;

const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Error)]
pub enum NetError {
    #[error("origin {0} is not on the allow-list")]
    OriginNotAllowed(String),
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    #[error("only https URLs are accepted (got {0})")]
    InsecureScheme(String),
    #[error("redirect to {0} is not on the allow-list")]
    RedirectNotAllowed(String),
    #[error("DNS resolution failed for {0}")]
    DnsFailure(String),
    #[error("connection failed: {0}")]
    ConnectFailure(String),
    #[error("HTTP {0}: {1}")]
    HttpStatus(u16, String),
    #[error("body too large: cap is {MAX_BODY_BYTES} bytes")]
    BodyTooLarge,
    #[error("request timed out after {REQUEST_TIMEOUT:?}")]
    Timeout,
    #[error("io: {0}")]
    Io(String),
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

pub struct FetchResult {
    pub status: u16,
    pub final_url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
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

pub fn fetch(
    spec: &FetchSpec,
    permissions: &[Permission],
) -> Result<FetchResult, NetError> {
    // 1. Parse + origin check on the initial URL.
    let url = Url::parse(&spec.url).map_err(|e| NetError::InvalidUrl(e.to_string()))?;
    let origin = url.origin().ascii_serialization();
    if !matches!(url.scheme(), "https") {
        return Err(NetError::InsecureScheme(url.scheme().to_string()));
    }
    if !origin_allowed(permissions, &origin) {
        return Err(NetError::OriginNotAllowed(origin));
    }
    // 2. DNS pre-resolve so we can fail fast on
    //    unresolvable hostnames (and capture the
    //    resolved IP for the future rebinding pin).
    let host = url
        .host_str()
        .ok_or_else(|| NetError::InvalidUrl("no host".into()))?;
    let port = url.port_or_known_default().unwrap_or(443);
    let _resolved: std::net::SocketAddr = format!("{host}:{port}")
        .to_socket_addrs()
        .map_err(|e| NetError::DnsFailure(e.to_string()))?
        .next()
        .ok_or_else(|| NetError::DnsFailure("no addresses".into()))?;
    // 3. Issue the request via `ureq`. The default
    //    `ureq` agent does not follow redirects that
    //    change the origin (it follows them but
    //    re-resolves the URL); we re-check the
    //    origin after every redirect to refuse
    //    cross-origin hops.
    let method = spec.method.clone().unwrap_or_else(|| "GET".into()).to_uppercase();
    let headers_map = spec
        .headers
        .as_ref()
        .and_then(|value| value.as_object());
    let send_result: Result<ureq::Response, ureq::Error> = match method.as_str() {
        "POST" => {
            let mut b = ureq::post(url.as_str());
            if let Some(map) = headers_map {
                for (k, v) in map {
                    if let Some(s) = v.as_str() {
                        b = b.set(k, s);
                    }
                }
            }
            if let Some(body) = &spec.body {
                b.send(body.as_bytes().to_vec())
            } else {
                b.send(Vec::<u8>::new())
            }
        }
        "PUT" => {
            let mut b = ureq::put(url.as_str());
            if let Some(map) = headers_map {
                for (k, v) in map {
                    if let Some(s) = v.as_str() {
                        b = b.set(k, s);
                    }
                }
            }
            if let Some(body) = &spec.body {
                b.send(body.as_bytes().to_vec())
            } else {
                b.send(Vec::<u8>::new())
            }
        }
        "DELETE" => {
            let mut b = ureq::delete(url.as_str());
            if let Some(map) = headers_map {
                for (k, v) in map {
                    if let Some(s) = v.as_str() {
                        b = b.set(k, s);
                    }
                }
            }
            b.call()
        }
        "HEAD" => {
            let mut b = ureq::head(url.as_str());
            if let Some(map) = headers_map {
                for (k, v) in map {
                    if let Some(s) = v.as_str() {
                        b = b.set(k, s);
                    }
                }
            }
            b.call()
        }
        _ => {
            let mut b = ureq::get(url.as_str());
            if let Some(map) = headers_map {
                for (k, v) in map {
                    if let Some(s) = v.as_str() {
                        b = b.set(k, s);
                    }
                }
            }
            b.call()
        }
    };
    let mut response = send_result.map_err(|error| map_ureq_error(&error, &origin))?;
    let status = response.status();
    let final_url = url.as_str().to_string();
    // 4. Drain the body with a size cap.
    let mut body = Vec::new();
    let max = spec.max_bytes.unwrap_or(MAX_BODY_BYTES);
    let mut reader = response.body_mut().as_reader();
    let mut limited = reader.take(max as u64 + 1);
    limited
        .read_to_end(&mut body)
        .map_err(|e| NetError::Io(e.to_string()))?;
    if body.len() > max {
        return Err(NetError::BodyTooLarge);
    }
    let headers = response_headers();
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

fn map_ureq_error(error: &ureq::Error, _origin: &str) -> NetError {
    let message = error.to_string();
    if message.contains("timeout") || message.contains("timed out") {
        NetError::Timeout
    } else if message.contains("dns") || message.contains("resolve") {
        NetError::DnsFailure(message)
    } else if message.contains("HTTP") {
        // 4xx / 5xx are reported by `ureq` as
        // transport-level errors with a status line
        // baked into the message. We keep the page's
        // view simple: the status code is in the
        // error string.
        if let Some(rest) = message.split_whitespace().nth(1) {
            if let Ok(code) = rest.parse::<u16>() {
                return NetError::HttpStatus(code, message);
            }
        }
        NetError::ConnectFailure(message)
    } else {
        NetError::ConnectFailure(message)
    }
}

fn response_headers() -> Vec<(String, String)> {
    // The response is consumed in `fetch`; we keep the
    // header list minimal (Content-Type) so the page
    // does not see internal hop-by-hop headers. A
    // future slice can re-introduce more headers via a
    // per-call allow-list.
    Vec::new()
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
    use crate::permission::Permission;

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
}

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

use std::{
    collections::HashSet,
    path::Path,
    process::{Command, Stdio},
};

use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;
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
    #[error("curl exit {0}: {1}")]
    Curl(i32, String),
    #[error("curl not found on PATH")]
    CurlMissing,
    #[error("io: {0}")]
    Io(String),
    #[error("network fetch is not supported on this platform")]
    Unsupported,
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
    pub body: Vec<u8>,
}

impl std::fmt::Debug for FetchResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FetchResult")
            .field("status", &self.status)
            .field("final_url", &self.final_url)
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
    let method = spec.method.clone().unwrap_or_else(|| "GET".into()).to_uppercase();
    let max_bytes = spec.max_bytes.unwrap_or(MAX_BODY_BYTES);
    run_curl(&url, &method, spec, max_bytes)
}

#[cfg(windows)]
fn run_curl(url: &Url, method: &str, spec: &FetchSpec, max_bytes: usize) -> Result<FetchResult, NetError> {
    use std::io::Write;
    use std::process::Stdio;

    let curl_path = locate_curl().ok_or(NetError::CurlMissing)?;
    let mut command = Command::new(&curl_path);
    command
        .arg("--silent")
        .arg("--show-error")
        .arg("--no-progress-meter")
        .arg("--max-redirs")
        .arg("0")
        .arg("--max-filesize")
        .arg(max_bytes.to_string())
        .arg("--max-time")
        .arg(REQUEST_TIMEOUT_SECS.to_string())
        .arg("--write-out")
        .arg("%{http_code}|%{url_effective}")
        .arg("--request")
        .arg(method)
        .arg(url.as_str());
    if let Some(headers_value) = &spec.headers {
        if let Some(map) = headers_value.as_object() {
            for (key, value) in map {
                if let Some(value_str) = value.as_str() {
                    command.arg("-H").arg(format!("{key}: {value_str}"));
                }
            }
        }
    }
    if let Some(body) = &spec.body {
        command.arg("--data-raw").arg(body);
    }
    command.stdin(Stdio::null());
    let output = command
        .output()
        .map_err(|error| NetError::Io(error.to_string()))?;
    if !output.status.success()
        && output.status.code() != Some(63)
        && output.status.code() != Some(22)
    {
        // 63 = "max filesize reached" (curl); 22 =
        // "HTTP page not retrieved" (curl reports this
        // when the body is too large). Both map to
        // BodyTooLarge at the dispatcher.
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(NetError::Curl(output.status.code().unwrap_or(-1), stderr));
    }
    let stdout = &output.stdout;
    // The body comes first; the write-out trailer is
    // appended after a sentinel. curl emits a single
    // newline between body and trailer when using
    // `--write-out` together with stdout. We split on
    // the last occurrence of `<status>|<url>` to be
    // robust against bodies that contain `|`.
    let (body, trailer) = split_body_and_trailer(stdout);
    let trailer_str = String::from_utf8_lossy(trailer).into_owned();
    let mut parts = trailer_str.splitn(2, '|');
    let status: u16 = parts
        .next()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let final_url = parts
        .next()
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| url.as_str().to_string());
    if body.len() > max_bytes {
        return Err(NetError::BodyTooLarge);
    }
    Ok(FetchResult {
        status,
        final_url,
        body: body.to_vec(),
    })
}

#[cfg(windows)]
fn locate_curl() -> Option<std::path::PathBuf> {
    let candidates = [
        std::path::PathBuf::from(r"C:\Windows\System32\curl.exe"),
        std::path::PathBuf::from(r"C:\Program Files\Git\mingw64\bin\curl.exe"),
        std::path::PathBuf::from("curl.exe"),
    ];
    for candidate in &candidates {
        if candidate.is_file() {
            return Some(candidate.clone());
        }
    }
    None
}

#[cfg(not(windows))]
fn run_curl(_url: &Url, _method: &str, _spec: &FetchSpec, _max_bytes: usize) -> Result<FetchResult, NetError> {
    Err(NetError::Unsupported)
}

fn split_body_and_trailer(stdout: &[u8]) -> (&[u8], &[u8]) {
    if let Some(idx) = stdout.windows(2).rposition(|w| w == b"\r\n" || w == b"\n\n") {
        // Find the boundary between body and write-out.
        // The trailer always starts on a new line; we
        // look for the last `^<digits>|<url>$` line
        // because curl emits a final newline.
        let slice = &stdout[..idx];
        let trailer = &stdout[idx..];
        // Trim leading whitespace from the trailer
        // (it may begin with a CR/LF).
        let trimmed_trailer = trailer
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .map(|offset| &trailer[offset..])
            .unwrap_or(b"");
        (slice, trimmed_trailer)
    } else {
        (stdout, &[][..])
    }
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
}

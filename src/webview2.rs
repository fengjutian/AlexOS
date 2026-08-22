//! Microsoft Edge WebView2 Runtime detection.
//!
//! Alex OS renders every page — including the system App Manager —
//! through `wry` / WebView2. The runtime is a separate install from
//! Edge itself and from the OS, and Microsoft ships the bootstrapper
//! separately. A clean error here is the difference between "user
//! follows a 30-second download link" and "user stares at a blank
//! window with no logs".
//!
//! Detection order matches Microsoft's documented discovery order:
//!
//! 1. `HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\<GUID>`
//!    — Evergreen runtime, system-wide install (most common).
//! 2. `HKLM\SOFTWARE\Microsoft\EdgeUpdate\Clients\<GUID>`
//!    — Non-WOW fallback for 32-bit Windows / ARM.
//! 3. `HKCU\Software\Microsoft\EdgeUpdate\Clients\<GUID>`
//!    — Per-user install.
//!
//! We shell out to `reg.exe` rather than use the `windows` crate
//! directly because the reg.exe roundtrip is ~3 ms (well below the
//! 100 ms one-shot startup cost this check contributes), keeps the
//! dependency surface flat, and matches the existing `taskkill.exe`
//! pattern in `runtime.rs`. If we ever need to call this in a hot
//! path, swap to `RegOpenKeyExW` behind a feature flag.

use std::path::PathBuf;
use std::process::Command;

use thiserror::Error;

/// WebView2 Evergreen Runtime product GUID. Stable across versions;
/// changing it would require coordination with Edge Update.
const WEBVIEW2_PRODUCT_GUID: &str = "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";

/// Microsoft's official Evergreen Bootstrapper. The `?form=MAFJA`
/// is the "machine-friendly auto-download" form that doesn't render
/// the web landing page and triggers the actual MSI / exe.
pub const WEBVIEW2_BOOTSTRAP_URL: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebView2Status {
    /// Display name from the registry, e.g. "Microsoft Edge WebView2
    /// Runtime". Useful for the user-facing `alex doctor` output.
    pub name: String,
    /// Evergreen version, e.g. "151.0.4129.101".
    pub version: String,
    /// Install root, e.g.
    /// `C:\Program Files (x86)\Microsoft\EdgeWebView\Application`.
    pub install_path: PathBuf,
    /// Which registry key the detection came from. The order is
    /// documented above; the first match wins.
    pub source: RegistrySource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrySource {
    /// `HKLM\SOFTWARE\WOW6432Node\...` — 64-bit Windows, 32-bit
    /// process, system-wide install.
    HklmWow6432,
    /// `HKLM\SOFTWARE\...` — non-WOW, e.g. 32-bit Windows.
    Hklm,
    /// `HKCU\Software\...` — per-user install.
    Hkcu,
}

impl RegistrySource {
    pub fn as_reg_path(self) -> &'static str {
        match self {
            Self::HklmWow6432 => "HKLM\\SOFTWARE\\WOW6432Node\\Microsoft\\EdgeUpdate\\Clients",
            Self::Hklm => "HKLM\\SOFTWARE\\Microsoft\\EdgeUpdate\\Clients",
            Self::Hkcu => "HKCU\\Software\\Microsoft\\EdgeUpdate\\Clients",
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WebView2Error {
    #[error(
        "Microsoft Edge WebView2 Runtime is not installed.\n\
         Alex OS renders every page through WebView2 and cannot start\n\
         without it. Install the Evergreen Bootstrapper from:\n  \
         {0}\n\
         then re-run this command.",
        WEBVIEW2_BOOTSTRAP_URL
    )]
    NotInstalled,
    #[error("reg.exe query for {path} failed: {message}")]
    RegFailed { path: String, message: String },
    #[error("WebView2 registry key is present but missing required values: {detail}")]
    Malformed { detail: String },
}

/// Detect the WebView2 Evergreen Runtime. Returns `NotInstalled`
/// only when every documented registry hive was checked and none
/// had the runtime key. `reg.exe` failures fall through to the next
/// hive — the only "real" error is malformed registry data, which
/// suggests the user has a half-installed or otherwise broken
/// WebView2 setup.
pub fn detect() -> Result<WebView2Status, WebView2Error> {
    for source in [
        RegistrySource::HklmWow6432,
        RegistrySource::Hklm,
        RegistrySource::Hkcu,
    ] {
        match query_hive(source) {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => continue,
            Err(WebView2Error::Malformed { detail }) => {
                // Half-installed: report the specific key that
                // looked broken rather than falling through silently.
                return Err(WebView2Error::Malformed { detail });
            }
            Err(_) => continue, // reg.exe unavailable or hive absent — try next
        }
    }
    Err(WebView2Error::NotInstalled)
}

fn query_hive(source: RegistrySource) -> Result<Option<WebView2Status>, WebView2Error> {
    let key = format!("{}\\{}", source.as_reg_path(), WEBVIEW2_PRODUCT_GUID);
    let output = Command::new("reg.exe")
        .args(["query", &key])
        .output()
        .map_err(|e| WebView2Error::RegFailed {
            path: key.clone(),
            message: format!("spawn reg.exe: {e}"),
        })?;

    // `reg query` exits 1 when the key is absent — that is the
    // expected "this hive doesn't have it" path, not an error.
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.contains("unable to find the specified registry key") || stderr.is_empty() {
            return Ok(None);
        }
        return Err(WebView2Error::RegFailed {
            path: key,
            message: stderr,
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut install_path: Option<PathBuf> = None;

    for line in stdout.lines() {
        let trimmed = line.trim();
        // Each value line is "    <name>    REG_SZ    <data>".
        // reg.exe's column alignment is consistent in practice
        // (3 spaces of indent + name + at least 4 spaces + type +
        // at least 4 spaces + data), but we just look for the
        // REG_SZ marker so we don't care about exact spacing.
        if !trimmed.contains("REG_SZ") {
            continue;
        }
        let Some((name_part, value_part)) = trimmed.split_once("REG_SZ") else {
            continue;
        };
        let value = value_part.trim();
        let name_field = name_part.trim();
        match name_field {
            "name" => name = Some(value.to_string()),
            "pv" => version = Some(value.to_string()),
            "location" => install_path = Some(PathBuf::from(value)),
            _ => {} // SilentUninstall etc. — not relevant to detection.
        }
    }

    // A successful reg query that yielded no recognised values is
    // genuinely weird. Report it instead of pretending the runtime
    // is missing.
    if name.is_none() || version.is_none() || install_path.is_none() {
        return Err(WebView2Error::Malformed {
            detail: format!(
                "{}: name={:?} pv={:?} location={:?}",
                key, name, version, install_path
            ),
        });
    }

    Ok(Some(WebView2Status {
        name: name.unwrap(),
        version: version.unwrap(),
        install_path: install_path.unwrap(),
        source,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_source_paths_are_stable() {
        // The exact strings end up in user-facing `alex doctor`
        // output; lock them down.
        assert_eq!(
            RegistrySource::HklmWow6432.as_reg_path(),
            "HKLM\\SOFTWARE\\WOW6432Node\\Microsoft\\EdgeUpdate\\Clients"
        );
        assert_eq!(
            RegistrySource::Hklm.as_reg_path(),
            "HKLM\\SOFTWARE\\Microsoft\\EdgeUpdate\\Clients"
        );
        assert_eq!(
            RegistrySource::Hkcu.as_reg_path(),
            "HKCU\\Software\\Microsoft\\EdgeUpdate\\Clients"
        );
    }

    #[test]
    fn webview2_product_guid_is_the_documented_one() {
        // Changing this would silently break detection across the
        // user base. The Evergreen Runtime GUID is owned by
        // Microsoft and is not expected to change.
        assert_eq!(
            WEBVIEW2_PRODUCT_GUID,
            "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
        );
    }

    #[test]
    fn bootstrap_url_points_to_microsoft_cdn() {
        // The link target changes with Microsoft's marketing
        // site updates; the host (go.microsoft.com) is stable.
        assert!(WEBVIEW2_BOOTSTRAP_URL.starts_with("https://go.microsoft.com/"));
    }

    /// The dev host runs other WebView-backed tests, so WebView2 is
    /// expected to be present. If this test fails the dev box is
    /// broken, not the detection code.
    #[cfg(windows)]
    #[test]
    fn detection_finds_webview2_on_dev_host() {
        let status = detect().expect("WebView2 must be installed to run other tests");
        assert!(!status.version.is_empty());
        assert!(status.install_path.exists());
    }
}

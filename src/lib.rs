// Top-level modules are grouped by responsibility. The subdirs own the
// concrete file layout; we re-export the moved modules at the crate
// root so `use crate::shell`, `use crate::api::ApiRouter`, etc. keep
// working unchanged after the reorganization.
pub mod core;
pub mod webview;
pub mod api;
pub mod runtime;
pub mod data;
pub mod container;

pub use core::{manager, manifest, package, plugin, trust, update};
pub use webview::{dev, manager_webview, native, shell, webview2};
pub use api::{authorization, ipc, permission, permission_shim};
pub use runtime::{
    event_bus, menu_tray, net, process, proxy, watcher, window_manager, windows,
};
pub use data::{file_token, storage};

use std::path::{Path, PathBuf};

use manifest::AppManifest;
use thiserror::Error;

/// Wry rewrites a custom-protocol URL on Windows from
/// `alex://<authority>/...` to `http://alex.<authority>/...` before the
/// navigation callback sees it. Accept both representations, but only for
/// the exact internal authority.
pub(crate) fn is_internal_webview_url(value: &str, authority: &str) -> bool {
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    if !url.username().is_empty() || url.password().is_some() || url.port().is_some() {
        return false;
    }
    matches!(
        (url.scheme(), url.host_str()),
        ("alex", Some(host)) if host == authority
    ) || matches!(
        (url.scheme(), url.host_str()),
        ("http", Some(host)) if host == format!("alex.{authority}")
    )
}

#[cfg(test)]
mod webview_url_tests {
    use super::is_internal_webview_url;

    #[test]
    fn accepts_native_and_windows_mapped_internal_urls() {
        assert!(is_internal_webview_url("alex://app/", "app"));
        assert!(is_internal_webview_url("alex://app/frontend/app.js", "app"));
        assert!(is_internal_webview_url("http://alex.app/", "app"));
        assert!(is_internal_webview_url(
            "http://alex.system/app-manager/",
            "system"
        ));
    }

    #[test]
    fn rejects_lookalike_and_external_urls() {
        assert!(!is_internal_webview_url("https://example.com/", "app"));
        assert!(!is_internal_webview_url("http://alex.app.evil/", "app"));
        assert!(!is_internal_webview_url("http://alex.system/", "app"));
        assert!(!is_internal_webview_url("http://alex.app:8080/", "app"));
        assert!(!is_internal_webview_url("alex://user@app/", "app"));
    }
}

#[derive(Debug, Error)]
pub enum AlexError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid manifest {path}: {source}")]
    Manifest {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("invalid application: {0}")]
    Validation(String),
    #[error(transparent)]
    Runtime(#[from] runtime::RuntimeError),
}

pub fn load_app(app_dir: &Path) -> Result<AppManifest, AlexError> {
    let path = app_dir.join("manifest.json");
    let input = std::fs::read_to_string(&path).map_err(|source| AlexError::Read {
        path: path.clone(),
        source,
    })?;
    let manifest =
        serde_json::from_str::<AppManifest>(&input).map_err(|source| AlexError::Manifest {
            path: path.clone(),
            source,
        })?;
    manifest.validate(app_dir)?;
    Ok(manifest)
}

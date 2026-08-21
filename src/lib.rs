pub mod api;
pub mod authorization;
pub mod dev;
pub mod ipc;
pub mod manager;
pub mod manager_webview;
pub mod manifest;
pub mod native;
pub mod package;
pub mod permission;
pub mod plugin;
pub mod runtime;
pub mod shell;
pub mod trust;
pub mod update;

use std::path::{Path, PathBuf};

use manifest::AppManifest;
use thiserror::Error;

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

//! Thin wrapper over `crate::runtime::RuntimeProcess` that the
//! `ContainerService` uses as the backend launcher.
//!
//! Phase A delegates to the existing `RuntimeProcess` so the 0.1
//! `alex run` / `alex shell` paths keep working unchanged. Phase B
//! will route the spawn through the chosen `IsolationProvider` so
//! the same launch path can land in a Windows Job Object.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::core::manifest::Backend;

#[derive(Debug, Clone)]
pub struct ServiceEndpoint {
    pub port: u16,
    pub token: String,
}

pub struct LaunchRequest<'a> {
    pub app_id: &'a str,
    pub package_root: &'a Path,
    pub backend: &'a Backend,
    pub data_dir: Option<&'a Path>,
    pub cache_dir: Option<&'a Path>,
    pub log_dir: Option<&'a Path>,
    pub port: Option<u16>,
    pub token: Option<&'a str>,
}

pub struct Launched {
    pub pid: u32,
    pub endpoint: Option<ServiceEndpoint>,
    pub ready: bool,
}

pub fn launch_backend(request: LaunchRequest<'_>) -> Result<Launched, ContainerLauncherError> {
    let LaunchRequest {
        app_id,
        package_root,
        backend,
        data_dir,
        cache_dir,
        log_dir: _,
        port: _,
        token: _,
    } = request;
    let spec = crate::runtime::RuntimeSpec {
        app_id: app_id.to_owned(),
        package_root: package_root.to_path_buf(),
        backend: backend.clone(),
        data_dir: data_dir.map(Path::to_path_buf),
        cache_dir: cache_dir.map(Path::to_path_buf),
    };
    let handle = crate::runtime::RuntimeHandle::start_with_spec(spec)
        .map_err(|e| ContainerLauncherError::Spawn(e.to_string()))?;
    let status = handle
        .status(Duration::from_millis(500))
        .map_err(|e| ContainerLauncherError::Status(e.to_string()))?;
    let endpoint = status.port.map(|runtime_port| ServiceEndpoint {
        port: runtime_port,
        // The runtime mints its own token; the container layer
        // does not see it. Phase B will pass through a host-minted
        // token so the supervisor holds the only copy.
        token: String::new(),
    });
    let pid = status.pid.unwrap_or(0);
    // The supervisor thread inside `RuntimeHandle` keeps the child
    // alive. We must not let the `RuntimeHandle` drop, or the
    // supervisor thread exits and the child is shut down. Leaking
    // is the 0.1 path's answer too.
    Box::leak(Box::new(handle));
    Ok(Launched {
        pid,
        endpoint,
        ready: status.ready,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum ContainerLauncherError {
    #[error("failed to spawn backend: {0}")]
    Spawn(String),
    #[error("failed to read backend status: {0}")]
    Status(String),
}

pub fn installed_package_root(install_root: &Path, app_id: &str) -> PathBuf {
    install_root.join(app_id)
}

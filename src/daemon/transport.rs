use std::path::Path;

use std::sync::Arc;

use super::{ControlRequest, ControlResponse, DaemonService, DaemonStateStore};

pub const DEFAULT_PIPE_NAME: &str = r"\\.\pipe\alex-runtime-v1";

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub fn run_server(
    state_path: &Path,
    pipe_name: &str,
    manager: Arc<dyn crate::manager::AppManager>,
) -> std::io::Result<()> {
    let ai_root = state_path
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| state_path.parent().unwrap_or(Path::new(".")));
    let service = DaemonService::new(DaemonStateStore::new(state_path))
        .with_manager(manager)
        .with_ai_root(ai_root)
        .map_err(std::io::Error::other)?;
    let recovery = service.recover_startup();
    for app_id in &recovery.recovered {
        eprintln!("alexd: recovered {app_id}");
    }
    for failure in &recovery.failed {
        eprintln!(
            "alexd: failed to recover {}: {}",
            failure.app_id, failure.error
        );
    }
    windows::run_server(service, pipe_name)
}

#[cfg(windows)]
pub fn send_request(pipe_name: &str, request: &ControlRequest) -> std::io::Result<ControlResponse> {
    windows::send_request(pipe_name, request)
}

#[cfg(not(windows))]
pub fn run_server(
    _: &Path,
    _: &str,
    _: Arc<dyn crate::manager::AppManager>,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "alexd local transport is currently implemented for Windows only",
    ))
}

#[cfg(not(windows))]
pub fn send_request(_: &str, _: &ControlRequest) -> std::io::Result<ControlResponse> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "alexd local transport is currently implemented for Windows only",
    ))
}

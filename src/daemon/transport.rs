use std::path::Path;

use std::sync::Arc;

use super::{DaemonService, DaemonStateStore};

pub const DEFAULT_PIPE_NAME: &str = r"\\.\pipe\alex-runtime-v1";

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub fn run_server(
    state_path: &Path,
    pipe_name: &str,
    manager: Arc<dyn crate::manager::AppManager>,
) -> std::io::Result<()> {
    windows::run_server(
        DaemonService::new(DaemonStateStore::new(state_path)).with_manager(manager),
        pipe_name,
    )
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

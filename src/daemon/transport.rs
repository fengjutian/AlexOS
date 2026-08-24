use std::path::Path;

use super::{DaemonService, DaemonStateStore};

pub const DEFAULT_PIPE_NAME: &str = r"\\.\pipe\alex-runtime-v1";

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub fn run_server(state_path: &Path, pipe_name: &str) -> std::io::Result<()> {
    windows::run_server(
        DaemonService::new(DaemonStateStore::new(state_path)),
        pipe_name,
    )
}

#[cfg(not(windows))]
pub fn run_server(_: &Path, _: &str) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "alexd local transport is currently implemented for Windows only",
    ))
}

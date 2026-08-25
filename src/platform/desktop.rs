//! Desktop interaction boundary used by the API layer.

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NativeError {
    #[error("native capability failed: {0}")]
    Failed(String),
    #[error("native capability is unavailable on this platform")]
    Unsupported,
}

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub temp_dir: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct DialogFilter {
    pub name: String,
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct OpenDialogSpec {
    pub title: Option<String>,
    pub default_path: Option<PathBuf>,
    pub filters: Vec<DialogFilter>,
    pub multiple: bool,
    pub directory: bool,
}

#[derive(Debug, Clone, Default)]
pub struct SaveDialogSpec {
    pub title: Option<String>,
    pub default_path: Option<PathBuf>,
    pub filters: Vec<DialogFilter>,
    pub suggested_name: Option<String>,
}

pub trait DesktopServices: Send + Sync {
    fn confirm_permission(&self, app_name: &str, permission: &str) -> Result<bool, NativeError>;
    fn clipboard_read_text(&self) -> Result<String, NativeError>;
    fn clipboard_write_text(&self, text: String) -> Result<(), NativeError>;
    fn app_paths(&self, app_id: &str) -> Result<AppPaths, NativeError>;
    fn pick_paths(&self, spec: OpenDialogSpec) -> Result<Vec<PathBuf>, NativeError>;
    fn pick_save_path(&self, spec: SaveDialogSpec) -> Result<Option<PathBuf>, NativeError>;
    fn open_external(&self, url: &str) -> Result<(), NativeError>;
    fn show_notification(&self, title: &str, body: &str) -> Result<(), NativeError>;
}

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as implementation;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as implementation;
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod portable;
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
use portable as implementation;

#[derive(Debug, Clone, Copy)]
pub struct NativeDesktopPlatform;

impl DesktopServices for NativeDesktopPlatform {
    fn confirm_permission(&self, app_name: &str, permission: &str) -> Result<bool, NativeError> {
        implementation::confirm_permission(app_name, permission)
    }
    fn clipboard_read_text(&self) -> Result<String, NativeError> {
        implementation::clipboard_read_text()
    }
    fn clipboard_write_text(&self, text: String) -> Result<(), NativeError> {
        implementation::clipboard_write_text(text)
    }
    fn app_paths(&self, app_id: &str) -> Result<AppPaths, NativeError> {
        implementation::app_paths(app_id)
    }
    fn pick_paths(&self, spec: OpenDialogSpec) -> Result<Vec<PathBuf>, NativeError> {
        implementation::pick_paths(spec)
    }
    fn pick_save_path(&self, spec: SaveDialogSpec) -> Result<Option<PathBuf>, NativeError> {
        implementation::pick_save_path(spec)
    }
    fn open_external(&self, url: &str) -> Result<(), NativeError> {
        implementation::open_external(url)
    }
    fn show_notification(&self, title: &str, body: &str) -> Result<(), NativeError> {
        implementation::show_notification(title, body)
    }
}

pub fn native() -> NativeDesktopPlatform {
    NativeDesktopPlatform
}

/// UI-thread entry points used by WebView hosts. Keeping these here avoids
/// duplicating platform dialog construction in each event loop.
pub fn pick_paths_on_ui_thread(spec: OpenDialogSpec) -> Result<Vec<PathBuf>, NativeError> {
    implementation::pick_paths(spec)
}

pub fn pick_save_path_on_ui_thread(spec: SaveDialogSpec) -> Result<Option<PathBuf>, NativeError> {
    implementation::pick_save_path(spec)
}

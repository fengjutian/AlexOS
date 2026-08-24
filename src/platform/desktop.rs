//! Desktop interaction boundary used by the API layer.
//!
//! `LegacyDesktopPlatform` currently adapts the established Wry/Windows
//! implementation. macOS can implement this contract without changing the
//! router or Desktop API handlers.

use std::path::PathBuf;

use crate::webview::native::NativeError;
pub use crate::webview::native::{AppPaths, DialogFilter, OpenDialogSpec, SaveDialogSpec};

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

#[derive(Debug, Clone, Copy)]
pub struct NativeDesktopPlatform;

impl DesktopServices for NativeDesktopPlatform {
    fn confirm_permission(&self, app_name: &str, permission: &str) -> Result<bool, NativeError> {
        crate::webview::native::confirm_permission(app_name, permission)
    }
    fn clipboard_read_text(&self) -> Result<String, NativeError> {
        crate::webview::native::clipboard_read_text()
    }
    fn clipboard_write_text(&self, text: String) -> Result<(), NativeError> {
        crate::webview::native::clipboard_write_text(text)
    }
    fn app_paths(&self, app_id: &str) -> Result<AppPaths, NativeError> {
        crate::webview::native::app_paths(app_id)
    }
    fn pick_paths(&self, spec: OpenDialogSpec) -> Result<Vec<PathBuf>, NativeError> {
        crate::webview::native::pick_paths(spec)
    }
    fn pick_save_path(&self, spec: SaveDialogSpec) -> Result<Option<PathBuf>, NativeError> {
        crate::webview::native::pick_save_path(spec)
    }
    fn open_external(&self, url: &str) -> Result<(), NativeError> {
        crate::webview::native::open_external(url)
    }
    fn show_notification(&self, title: &str, body: &str) -> Result<(), NativeError> {
        crate::webview::native::show_notification(title, body)
    }
}

pub fn native() -> NativeDesktopPlatform {
    NativeDesktopPlatform
}

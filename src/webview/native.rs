use std::path::PathBuf;

use crate::menu_tray::{MenuTemplate, TraySpec};
use crate::platform::desktop::{OpenDialogSpec, SaveDialogSpec};
use crate::windows::{WindowBounds, WindowInfo};

#[derive(Debug, Clone)]
pub enum HostCommand {
    SetWindowTitle(String),
    MinimizeWindow,
    MaximizeWindow,
    CloseWindow,
    CreateWindow(WindowInfo),
    SetWindowBounds(u64, WindowBounds),
    SetWindowFullscreen(u64, bool),
    DestroyWindow(u64),
    SetApplicationMenu(MenuTemplate),
    SetContextMenu(MenuTemplate),
    CreateTray(String, TraySpec, PathBuf),
    DestroyTray(String),
    RegisterShortcut(String),
    UnregisterShortcut(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NativeHostCapabilities {
    pub secondary_windows: bool,
    pub menus: bool,
    pub tray: bool,
    pub shortcuts: bool,
    pub dialogs: bool,
    pub media: bool,
    pub geolocation: bool,
}

pub trait NativeHost: Send + Sync {
    fn execute(&self, command: HostCommand) -> Result<(), NativeError>;

    /// Ask for a first-use permission decision on the host UI thread.
    fn confirm_permission(&self, _app_name: &str, _permission: &str) -> Result<bool, NativeError> {
        Err(NativeError::Unsupported)
    }

    fn pick_paths(&self, _spec: OpenDialogSpec) -> Result<Vec<PathBuf>, NativeError> {
        Err(NativeError::Unsupported)
    }

    fn pick_save_path(&self, _spec: SaveDialogSpec) -> Result<Option<PathBuf>, NativeError> {
        Err(NativeError::Unsupported)
    }

    fn confirm_mrtr(&self, _title: &str, _message: &str) -> Result<bool, NativeError> {
        Err(NativeError::Unsupported)
    }

    fn capabilities(&self) -> NativeHostCapabilities {
        NativeHostCapabilities::default()
    }
}

pub use crate::platform::desktop::NativeError;

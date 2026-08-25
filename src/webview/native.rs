use std::path::PathBuf;

use crate::menu_tray::{MenuTemplate, TraySpec};
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

pub trait NativeHost: Send + Sync {
    fn execute(&self, command: HostCommand) -> Result<(), NativeError>;

    fn confirm_mrtr(&self, _title: &str, _message: &str) -> Result<bool, NativeError> {
        Err(NativeError::Unsupported)
    }

    fn supports_secondary_windows(&self) -> bool {
        false
    }
}

pub use crate::platform::desktop::NativeError;

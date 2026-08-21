use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Clone)]
pub enum HostCommand {
    SetWindowTitle(String),
    MinimizeWindow,
    MaximizeWindow,
    CloseWindow,
}

pub trait NativeHost: Send + Sync {
    fn execute(&self, command: HostCommand) -> Result<(), NativeError>;
}

#[derive(Debug, Error)]
pub enum NativeError {
    #[error("native capability failed: {0}")]
    Failed(String),
    #[error("native capability is unavailable on this platform")]
    Unsupported,
}

#[cfg(windows)]
pub fn confirm_permission(app_name: &str, permission: &str) -> Result<bool, NativeError> {
    use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
    let result = MessageDialog::new()
        .set_level(MessageLevel::Warning)
        .set_title("Alex OS Permission Request")
        .set_description(format!(
            "{app_name} requests permission:\n\n{permission}\n\nAllow this application to use it?"
        ))
        .set_buttons(MessageButtons::YesNo)
        .show();
    Ok(matches!(result, MessageDialogResult::Yes))
}

#[cfg(not(windows))]
pub fn confirm_permission(_app_name: &str, _permission: &str) -> Result<bool, NativeError> {
    Err(NativeError::Unsupported)
}

#[cfg(windows)]
pub fn clipboard_read_text() -> Result<String, NativeError> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|error| NativeError::Failed(error.to_string()))?;
    clipboard
        .get_text()
        .map_err(|error| NativeError::Failed(error.to_string()))
}

#[cfg(not(windows))]
pub fn clipboard_read_text() -> Result<String, NativeError> {
    Err(NativeError::Unsupported)
}

#[cfg(windows)]
pub fn clipboard_write_text(text: String) -> Result<(), NativeError> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|error| NativeError::Failed(error.to_string()))?;
    clipboard
        .set_text(text)
        .map_err(|error| NativeError::Failed(error.to_string()))
}

#[cfg(not(windows))]
pub fn clipboard_write_text(_text: String) -> Result<(), NativeError> {
    Err(NativeError::Unsupported)
}

#[cfg(windows)]
pub fn pick_file(title: Option<&str>) -> Result<Option<PathBuf>, NativeError> {
    let mut dialog = rfd::FileDialog::new();
    if let Some(title) = title {
        dialog = dialog.set_title(title);
    }
    Ok(dialog.pick_file())
}

#[cfg(not(windows))]
pub fn pick_file(_title: Option<&str>) -> Result<Option<PathBuf>, NativeError> {
    Err(NativeError::Unsupported)
}

#[cfg(windows)]
pub fn open_external(url: &str) -> Result<(), NativeError> {
    open::that_detached(url).map_err(|error| NativeError::Failed(error.to_string()))
}

#[cfg(not(windows))]
pub fn open_external(_url: &str) -> Result<(), NativeError> {
    Err(NativeError::Unsupported)
}

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum NativeError {
    #[error("native capability failed: {0}")]
    Failed(String),
    #[error("native capability is unavailable on this platform")]
    Unsupported,
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

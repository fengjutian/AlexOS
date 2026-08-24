use std::path::PathBuf;

use super::{AppPaths, NativeError, OpenDialogSpec, SaveDialogSpec};

pub(super) fn confirm_permission(_: &str, _: &str) -> Result<bool, NativeError> {
    Err(NativeError::Unsupported)
}
pub(super) fn clipboard_read_text() -> Result<String, NativeError> {
    Err(NativeError::Unsupported)
}
pub(super) fn clipboard_write_text(_: String) -> Result<(), NativeError> {
    Err(NativeError::Unsupported)
}

pub(super) fn app_paths(app_id: &str) -> Result<AppPaths, NativeError> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| NativeError::Failed("HOME is not set".into()))?;
    let (data, cache) = if cfg!(target_os = "macos") {
        (
            home.join("Library/Application Support/AlexOS/apps"),
            home.join("Library/Caches/AlexOS/apps"),
        )
    } else {
        (
            std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/share"))
                .join("AlexOS/apps"),
            std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".cache"))
                .join("AlexOS/apps"),
        )
    };
    Ok(AppPaths {
        data_dir: data.join(app_id).join("data"),
        cache_dir: cache.join(app_id),
        temp_dir: std::env::temp_dir().join("AlexOS").join(app_id),
    })
}

pub(super) fn pick_paths(_: OpenDialogSpec) -> Result<Vec<PathBuf>, NativeError> {
    Err(NativeError::Unsupported)
}
pub(super) fn pick_save_path(_: SaveDialogSpec) -> Result<Option<PathBuf>, NativeError> {
    Err(NativeError::Unsupported)
}
pub(super) fn open_external(_: &str) -> Result<(), NativeError> {
    Err(NativeError::Unsupported)
}
pub(super) fn show_notification(_: &str, _: &str) -> Result<(), NativeError> {
    Err(NativeError::Unsupported)
}

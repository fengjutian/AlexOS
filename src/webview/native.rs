use std::path::PathBuf;

use crate::windows::{WindowBounds, WindowInfo};
use crate::menu_tray::{MenuTemplate, TraySpec};

use thiserror::Error;

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

    fn supports_secondary_windows(&self) -> bool {
        false
    }
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

/// Per-app storage layout: the host exposes the canonical
/// `data`, `cache`, and `temp` paths so the app can ask the user
/// for them or use them as defaults. The paths are created
/// lazily on first access and live under the host's local
/// data root.
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub temp_dir: PathBuf,
}

#[cfg(windows)]
pub fn app_paths(app_id: &str) -> Result<AppPaths, NativeError> {
    use std::env;
    let local = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| NativeError::Failed("LOCALAPPDATA is not set".into()))?;
    let base = local.join("AlexOS").join("apps").join(app_id);
    let temp_root = env::var_os("TEMP")
        .or_else(|| env::var_os("TMP"))
        .map(PathBuf::from)
        .ok_or_else(|| NativeError::Failed("TEMP is not set".into()))?;
    Ok(AppPaths {
        data_dir: base.join("data"),
        cache_dir: base.join("cache"),
        temp_dir: temp_root.join("AlexOS").join(app_id),
    })
}

#[cfg(not(windows))]
pub fn app_paths(_app_id: &str) -> Result<AppPaths, NativeError> {
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

/// Filter set passed to the native dialog. Extension entries are
/// matched case-insensitively; the host does not validate them
/// further.
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

#[cfg(windows)]
pub fn pick_paths(spec: OpenDialogSpec) -> Result<Vec<PathBuf>, NativeError> {
    let mut dialog = rfd::FileDialog::new();
    if let Some(title) = spec.title.as_deref() {
        dialog = dialog.set_title(title);
    }
    if let Some(default) = spec.default_path.as_deref() {
        dialog = dialog.set_directory(default);
    }
    for filter in &spec.filters {
        dialog = dialog.add_filter(&filter.name, &filter.extensions);
    }
    if spec.directory {
        let chosen = dialog.pick_folder();
        return Ok(chosen.into_iter().collect());
    }
    if spec.multiple {
        let chosen = dialog.pick_files();
        return Ok(chosen.unwrap_or_default());
    }
    let chosen = dialog.pick_file();
    Ok(chosen.into_iter().collect())
}

#[cfg(not(windows))]
pub fn pick_paths(_spec: OpenDialogSpec) -> Result<Vec<PathBuf>, NativeError> {
    Err(NativeError::Unsupported)
}

#[derive(Debug, Clone, Default)]
pub struct SaveDialogSpec {
    pub title: Option<String>,
    pub default_path: Option<PathBuf>,
    pub filters: Vec<DialogFilter>,
    pub suggested_name: Option<String>,
}

#[cfg(windows)]
pub fn pick_save_path(spec: SaveDialogSpec) -> Result<Option<PathBuf>, NativeError> {
    let mut dialog = rfd::FileDialog::new();
    if let Some(title) = spec.title.as_deref() {
        dialog = dialog.set_title(title);
    }
    if let Some(default) = spec.default_path.as_deref() {
        dialog = dialog.set_directory(default);
    }
    for filter in &spec.filters {
        dialog = dialog.add_filter(&filter.name, &filter.extensions);
    }
    if let Some(name) = spec.suggested_name.as_deref() {
        dialog = dialog.set_file_name(name);
    }
    Ok(dialog.save_file())
}

#[cfg(not(windows))]
pub fn pick_save_path(_spec: SaveDialogSpec) -> Result<Option<PathBuf>, NativeError> {
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

#[cfg(windows)]
pub fn show_notification(title: &str, body: &str) -> Result<(), NativeError> {
    use windows::{
        Data::Xml::Dom::XmlDocument,
        UI::Notifications::{ToastNotification, ToastNotificationManager},
        core::HSTRING,
    };
    let xml = format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        xml_escape(title),
        xml_escape(body)
    );
    let document = XmlDocument::new().map_err(|error| NativeError::Failed(error.to_string()))?;
    document
        .LoadXml(&HSTRING::from(xml))
        .map_err(|error| NativeError::Failed(error.to_string()))?;
    let toast = ToastNotification::CreateToastNotification(&document)
        .map_err(|error| NativeError::Failed(error.to_string()))?;
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from("Alex OS"))
        .map_err(|error| NativeError::Failed(error.to_string()))?;
    notifier
        .Show(&toast)
        .map_err(|error| NativeError::Failed(error.to_string()))
}

#[cfg(not(windows))]
pub fn show_notification(_title: &str, _body: &str) -> Result<(), NativeError> {
    Err(NativeError::Unsupported)
}

#[cfg(windows)]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

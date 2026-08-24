use std::path::PathBuf;

use super::{AppPaths, NativeError, OpenDialogSpec, SaveDialogSpec};

pub(super) fn confirm_permission(app_name: &str, permission: &str) -> Result<bool, NativeError> {
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

pub(super) fn clipboard_read_text() -> Result<String, NativeError> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| NativeError::Failed(e.to_string()))?;
    clipboard
        .get_text()
        .map_err(|e| NativeError::Failed(e.to_string()))
}

pub(super) fn clipboard_write_text(text: String) -> Result<(), NativeError> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| NativeError::Failed(e.to_string()))?;
    clipboard
        .set_text(text)
        .map_err(|e| NativeError::Failed(e.to_string()))
}

pub(super) fn app_paths(app_id: &str) -> Result<AppPaths, NativeError> {
    let local = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| NativeError::Failed("LOCALAPPDATA is not set".into()))?;
    let temp = std::env::var_os("TEMP")
        .or_else(|| std::env::var_os("TMP"))
        .map(PathBuf::from)
        .ok_or_else(|| NativeError::Failed("TEMP is not set".into()))?;
    let base = local.join("AlexOS").join("apps").join(app_id);
    Ok(AppPaths {
        data_dir: base.join("data"),
        cache_dir: base.join("cache"),
        temp_dir: temp.join("AlexOS").join(app_id),
    })
}

fn configure_dialog(
    mut dialog: rfd::FileDialog,
    title: Option<&str>,
    default: Option<&std::path::Path>,
    filters: &[super::DialogFilter],
) -> rfd::FileDialog {
    if let Some(title) = title {
        dialog = dialog.set_title(title);
    }
    if let Some(default) = default {
        dialog = dialog.set_directory(default);
    }
    for filter in filters {
        dialog = dialog.add_filter(&filter.name, &filter.extensions);
    }
    dialog
}

pub(super) fn pick_paths(spec: OpenDialogSpec) -> Result<Vec<PathBuf>, NativeError> {
    let dialog = configure_dialog(
        rfd::FileDialog::new(),
        spec.title.as_deref(),
        spec.default_path.as_deref(),
        &spec.filters,
    );
    if spec.directory {
        return Ok(dialog.pick_folder().into_iter().collect());
    }
    if spec.multiple {
        return Ok(dialog.pick_files().unwrap_or_default());
    }
    Ok(dialog.pick_file().into_iter().collect())
}

pub(super) fn pick_save_path(spec: SaveDialogSpec) -> Result<Option<PathBuf>, NativeError> {
    let mut dialog = configure_dialog(
        rfd::FileDialog::new(),
        spec.title.as_deref(),
        spec.default_path.as_deref(),
        &spec.filters,
    );
    if let Some(name) = spec.suggested_name.as_deref() {
        dialog = dialog.set_file_name(name);
    }
    Ok(dialog.save_file())
}

pub(super) fn open_external(url: &str) -> Result<(), NativeError> {
    open::that_detached(url).map_err(|e| NativeError::Failed(e.to_string()))
}

pub(super) fn show_notification(title: &str, body: &str) -> Result<(), NativeError> {
    use windows::{
        Data::Xml::Dom::XmlDocument,
        UI::Notifications::{ToastNotification, ToastNotificationManager},
        core::HSTRING,
    };
    let escape = |value: &str| {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    };
    let xml = format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        escape(title),
        escape(body)
    );
    let document = XmlDocument::new().map_err(|e| NativeError::Failed(e.to_string()))?;
    document
        .LoadXml(&HSTRING::from(xml))
        .map_err(|e| NativeError::Failed(e.to_string()))?;
    let toast = ToastNotification::CreateToastNotification(&document)
        .map_err(|e| NativeError::Failed(e.to_string()))?;
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from("Alex OS"))
        .map_err(|e| NativeError::Failed(e.to_string()))?;
    notifier
        .Show(&toast)
        .map_err(|e| NativeError::Failed(e.to_string()))
}

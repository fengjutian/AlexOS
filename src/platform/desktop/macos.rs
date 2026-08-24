use std::path::PathBuf;

use super::{AppPaths, NativeError, OpenDialogSpec, SaveDialogSpec};

pub(super) fn confirm_permission(app_name: &str, permission: &str) -> Result<bool, NativeError> {
    use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
    let answer = MessageDialog::new()
        .set_level(MessageLevel::Warning)
        .set_title("AlexOS Permission Request")
        .set_description(format!(
            "{app_name} requests permission:\n\n{permission}\n\nAllow this application to use it?"
        ))
        .set_buttons(MessageButtons::YesNo)
        .show();
    Ok(matches!(answer, MessageDialogResult::Yes))
}

pub(super) fn clipboard_read_text() -> Result<String, NativeError> {
    let mut clipboard = arboard::Clipboard::new().map_err(failed)?;
    clipboard.get_text().map_err(failed)
}

pub(super) fn clipboard_write_text(text: String) -> Result<(), NativeError> {
    let mut clipboard = arboard::Clipboard::new().map_err(failed)?;
    clipboard.set_text(text).map_err(failed)
}

pub(super) fn app_paths(app_id: &str) -> Result<AppPaths, NativeError> {
    validate_app_id(app_id)?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| NativeError::Failed("HOME is not set".into()))?;
    Ok(AppPaths {
        data_dir: home
            .join("Library/Application Support/AlexOS/apps")
            .join(app_id)
            .join("data"),
        cache_dir: home.join("Library/Caches/AlexOS/apps").join(app_id),
        temp_dir: std::env::temp_dir().join("AlexOS").join(app_id),
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
    open::that_detached(url).map_err(failed)
}

pub(super) fn show_notification(title: &str, body: &str) -> Result<(), NativeError> {
    let script = format!(
        "display notification {} with title {}",
        apple_script_string(body),
        apple_script_string(title)
    );
    let output = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .output()
        .map_err(failed)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(NativeError::Failed(format!(
            "osascript notification failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

fn apple_script_string(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\r', " ")
            .replace('\n', " ")
    )
}

fn validate_app_id(value: &str) -> Result<(), NativeError> {
    if value.is_empty() || value.contains(['/', '\\']) || value == "." || value == ".." {
        Err(NativeError::Failed("invalid application id".into()))
    } else {
        Ok(())
    }
}

fn failed(error: impl std::fmt::Display) -> NativeError {
    NativeError::Failed(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applescript_values_are_quoted_and_single_line() {
        assert_eq!(apple_script_string("a\"b\\c\nnext"), "\"a\\\"b\\\\c next\"");
    }

    #[test]
    fn app_ids_cannot_escape_the_platform_directory() {
        assert!(validate_app_id("com.example.notes").is_ok());
        assert!(validate_app_id("../escape").is_err());
    }
}

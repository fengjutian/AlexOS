use std::path::PathBuf;

use super::{AppPaths, NativeError, OpenDialogSpec, SaveDialogSpec};

pub const ALEX_APP_USER_MODEL_ID: &str = "AlexOS.Runtime";

fn native_failed(error: impl std::fmt::Display) -> NativeError {
    NativeError::Failed(error.to_string())
}

pub(super) fn initialize_notification_identity() -> Result<(), NativeError> {
    use windows::{
        Win32::{
            Foundation::PROPERTYKEY,
            System::Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                IPersistFile, StructuredStorage::PROPVARIANT,
            },
            UI::Shell::{
                IShellLinkW, PropertiesSystem::IPropertyStore,
                SetCurrentProcessExplicitAppUserModelID, ShellLink,
            },
        },
        core::{GUID, HSTRING, Interface},
    };

    unsafe { SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(ALEX_APP_USER_MODEL_ID)) }
        .map_err(native_failed)?;

    let appdata = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| NativeError::Failed("APPDATA is not set".into()))?;
    let shortcut = appdata
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("Alex OS.lnk");
    if shortcut.is_file() {
        return Ok(());
    }
    if let Some(parent) = shortcut.parent() {
        std::fs::create_dir_all(parent).map_err(native_failed)?;
    }

    let init = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if init.is_err() && init.0 != windows::Win32::Foundation::RPC_E_CHANGED_MODE.0 {
        return Err(native_failed(windows::core::Error::from_hresult(init)));
    }

    let executable = std::env::current_exe().map_err(native_failed)?;
    let executable = HSTRING::from(executable.as_os_str());
    let shortcut_path = HSTRING::from(shortcut.as_os_str());
    let link: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }
        .map_err(native_failed)?;
    unsafe {
        link.SetPath(&executable).map_err(native_failed)?;
        link.SetDescription(&HSTRING::from("Alex OS Runtime"))
            .map_err(native_failed)?;
    }

    // PKEY_AppUserModel_ID = {9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3}, 5.
    const APP_USER_MODEL_ID_KEY: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
        pid: 5,
    };
    let store: IPropertyStore = link.cast().map_err(native_failed)?;
    let value = PROPVARIANT::from(ALEX_APP_USER_MODEL_ID);
    unsafe {
        store
            .SetValue(&APP_USER_MODEL_ID_KEY, &value)
            .map_err(native_failed)?;
        store.Commit().map_err(native_failed)?;
    }
    let persist: IPersistFile = link.cast().map_err(native_failed)?;
    unsafe { persist.Save(&shortcut_path, true) }.map_err(native_failed)?;
    Ok(())
}

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
        Foundation::TypedEventHandler,
        UI::Notifications::{ToastFailedEventArgs, ToastNotification, ToastNotificationManager},
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
    let (failed_tx, failed_rx) = std::sync::mpsc::sync_channel(1);
    let failure_handler =
        TypedEventHandler::<ToastNotification, ToastFailedEventArgs>::new(move |_, args| {
            let code = args
                .as_ref()
                .and_then(|args| args.ErrorCode().ok())
                .map(|code| code.0)
                .unwrap_or_default();
            let _ = failed_tx.try_send(code);
            Ok(())
        });
    let failed_token = toast.Failed(&failure_handler).map_err(native_failed)?;
    let notifier =
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(ALEX_APP_USER_MODEL_ID))
            .map_err(native_failed)?;
    notifier.Show(&toast).map_err(native_failed)?;
    if let Ok(code) = failed_rx.recv_timeout(std::time::Duration::from_millis(500)) {
        let _ = toast.RemoveFailed(failed_token);
        return Err(NativeError::Failed(format!(
            "Windows rejected the toast notification (HRESULT {code:#010x})"
        )));
    }
    let _ = toast.RemoveFailed(failed_token);
    Ok(())
}

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "name", rename_all = "camelCase", deny_unknown_fields)]
pub enum Permission {
    #[serde(rename = "filesystem.read")]
    FilesystemRead { paths: Vec<PathBuf> },
    #[serde(rename = "filesystem.write")]
    FilesystemWrite { paths: Vec<PathBuf> },
    #[serde(rename = "dialog.open")]
    DialogOpen,
    #[serde(rename = "clipboard.read")]
    ClipboardRead,
    #[serde(rename = "clipboard.write")]
    ClipboardWrite,
    #[serde(rename = "system.openExternal")]
    OpenExternal { origins: Vec<String> },
    #[serde(rename = "window.manage")]
    WindowManage,
    #[serde(rename = "notification.show")]
    NotificationShow,
    #[serde(rename = "runtime.invoke")]
    RuntimeInvoke,
    #[serde(rename = "runtime.manage")]
    RuntimeManage,
    #[serde(rename = "media.camera")]
    MediaCamera,
    #[serde(rename = "media.microphone")]
    MediaMicrophone,
    #[serde(rename = "geolocation")]
    Geolocation,
    #[serde(rename = "system.install")]
    SystemInstall,
    #[serde(rename = "system.uninstall")]
    SystemUninstall,
    #[serde(rename = "system.manageApps")]
    SystemManageApps,
    #[serde(rename = "system.manageExtensions")]
    SystemManageExtensions,
}

impl Permission {
    /// Return the canonical permission name as written in `manifest.json`
    /// (matches the serde `rename` on each variant, e.g. `"system.manageApps"`).
    /// Used by `plugin::run` to pre-grant `system.*` permissions without
    /// parsing the manifest twice.
    pub fn name(&self) -> &'static str {
        match self {
            Permission::FilesystemRead { .. } => "filesystem.read",
            Permission::FilesystemWrite { .. } => "filesystem.write",
            Permission::DialogOpen => "dialog.open",
            Permission::ClipboardRead => "clipboard.read",
            Permission::ClipboardWrite => "clipboard.write",
            Permission::OpenExternal { .. } => "system.openExternal",
            Permission::WindowManage => "window.manage",
            Permission::NotificationShow => "notification.show",
            Permission::RuntimeInvoke => "runtime.invoke",
            Permission::RuntimeManage => "runtime.manage",
            Permission::MediaCamera => "media.camera",
            Permission::MediaMicrophone => "media.microphone",
            Permission::Geolocation => "geolocation",
            Permission::SystemInstall => "system.install",
            Permission::SystemUninstall => "system.uninstall",
            Permission::SystemManageApps => "system.manageApps",
            Permission::SystemManageExtensions => "system.manageExtensions",
        }
    }

    pub fn allows_path(&self, operation: &str, package_root: &Path, requested: &Path) -> bool {
        let roots = match (self, operation) {
            (Permission::FilesystemRead { paths }, "filesystem.read") => paths,
            (Permission::FilesystemWrite { paths }, "filesystem.write") => paths,
            _ => return false,
        };
        let Some(requested) = normalize(requested, package_root) else {
            return false;
        };
        roots.iter().any(|allowed| {
            normalize(allowed, package_root).is_some_and(|allowed| requested.starts_with(allowed))
        })
    }
}

fn normalize(path: &Path, package_root: &Path) -> Option<PathBuf> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        package_root.join(path)
    };
    let mut clean = PathBuf::new();
    for component in joined.components() {
        match component {
            std::path::Component::ParentDir => {
                if !clean.pop() {
                    return None;
                }
            }
            std::path::Component::CurDir => {}
            other => clean.push(other.as_os_str()),
        }
    }
    Some(clean)
}

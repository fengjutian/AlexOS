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
    #[serde(rename = "runtime.invoke")]
    RuntimeInvoke,
    #[serde(rename = "runtime.manage")]
    RuntimeManage,
}

impl Permission {
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

//! Per-instance directory layout.
//!
//! ```text
//! %LOCALAPPDATA%/AlexOS/
//!   packages/<app_id>/<version>/   # verified, read-only application layer
//!   containers/<instance_id>/
//!     state.json
//!     data/                         # persistent; preserved on `rm` by default
//!     cache/                        # regenerable
//!     logs/                         # host + backend log mirror; rotated
//!     runtime/                      # pid, port, token; cleared on start
//!     events/                       # JSONL audit trail
//! ```

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::runtime::AppDirs;

#[derive(Debug, Clone)]
pub struct ContainerDirs {
    pub instance_root: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
    pub logs: PathBuf,
    pub runtime: PathBuf,
    pub events: PathBuf,
    pub application_root: PathBuf,
}

impl ContainerDirs {
    pub fn resolve(data_root: &Path, instance_id: &str, app_id: &str, app_version: &str) -> Self {
        let instance_root = data_root.join("containers").join(instance_id);
        let events = instance_root.join("events");
        let app_dirs: AppDirs = AppDirs {
            data: instance_root.join("data"),
            cache: instance_root.join("cache"),
            logs: instance_root.join("logs"),
            runtime: instance_root.join("runtime"),
        };
        let application_root = data_root.join("packages").join(app_id).join(app_version);
        Self {
            instance_root,
            data: app_dirs.data,
            cache: app_dirs.cache,
            logs: app_dirs.logs,
            runtime: app_dirs.runtime,
            events,
            application_root,
        }
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        for dir in [
            &self.instance_root,
            &self.data,
            &self.cache,
            &self.logs,
            &self.runtime,
            &self.events,
        ] {
            fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    pub fn reset_runtime_slot(&self) -> std::io::Result<()> {
        if self.runtime.exists() {
            let entries = fs::read_dir(&self.runtime)?;
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let _ = fs::remove_dir_all(&path);
                } else {
                    let _ = fs::remove_file(&path);
                }
            }
        }
        fs::create_dir_all(&self.runtime)
    }
}

pub fn data_local_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share"))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_uses_the_canonical_layout() {
        let root = PathBuf::from("/alex");
        let dirs = ContainerDirs::resolve(&root, "com.example.notes", "com.example.notes", "1.0.0");
        assert_eq!(
            dirs.instance_root,
            PathBuf::from("/alex/containers/com.example.notes")
        );
        assert_eq!(
            dirs.data,
            PathBuf::from("/alex/containers/com.example.notes/data")
        );
        assert_eq!(
            dirs.application_root,
            PathBuf::from("/alex/packages/com.example.notes/1.0.0")
        );
    }

    #[test]
    fn ensure_creates_the_full_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = ContainerDirs::resolve(
            tmp.path(),
            "com.example.notes",
            "com.example.notes",
            "1.0.0",
        );
        dirs.ensure().unwrap();
        for d in [
            &dirs.instance_root,
            &dirs.data,
            &dirs.cache,
            &dirs.logs,
            &dirs.runtime,
            &dirs.events,
        ] {
            assert!(d.is_dir(), "missing {}", d.display());
        }
    }

    #[test]
    fn reset_runtime_slot_wipes_pid_and_token_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = ContainerDirs::resolve(
            tmp.path(),
            "com.example.notes",
            "com.example.notes",
            "1.0.0",
        );
        dirs.ensure().unwrap();
        std::fs::write(dirs.runtime.join("pid"), b"1234").unwrap();
        std::fs::write(dirs.runtime.join("token"), b"secret").unwrap();
        dirs.reset_runtime_slot().unwrap();
        assert!(!dirs.runtime.join("pid").exists());
        assert!(!dirs.runtime.join("token").exists());
        assert!(dirs.runtime.is_dir());
    }
}

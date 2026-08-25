//! Host-level executable allowlist for `Native` services (roadmap 0.4).
//!
//! A v2 manifest may declare a `runtime: native` service whose entry is
//! a package-relative executable. Running arbitrary executables is the
//! most dangerous backend class the host supports, so the host refuses
//! any Native launch unless the executable is explicitly allowlisted by
//! **both** package-relative path and SHA-256 digest.
//!
//! The allowlist lives at `<ALEX_DATA_DIR>/AlexOS/exec-allowlist.json`
//! (falling back to `%LOCALAPPDATA%/AlexOS/exec-allowlist.json`). An
//! absent file is an empty allowlist: every Native service is refused,
//! which is the secure default. There is deliberately no "allow all"
//! entry.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const ALLOWLIST_FILE: &str = "exec-allowlist.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecAllowlistEntry {
    /// Package-relative path of the executable (e.g. `backend/app.exe`).
    pub path: String,
    /// Lowercase-hex SHA-256 of the executable file.
    pub sha256: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecAllowlist {
    pub entries: Vec<ExecAllowlistEntry>,
}

#[derive(Debug, Error)]
pub enum ExecAllowlistError {
    #[error("exec allowlist I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid exec allowlist: {0}")]
    Invalid(String),
    #[error("native executable {path:?} is not allowlisted (path + sha256 must match)")]
    NotAllowlisted { path: String },
    #[error("native executable does not exist: {path}")]
    ExecutableMissing { path: String },
}

impl ExecAllowlist {
    /// Load the allowlist from `root`. A missing file is an empty
    /// allowlist (deny all), not an error.
    pub fn load(root: &Path) -> Result<Self, ExecAllowlistError> {
        let file = root.join(ALLOWLIST_FILE);
        if !file.is_file() {
            return Ok(Self::default());
        }
        let data = std::fs::read(&file)?;
        serde_json::from_slice(&data).map_err(|error| ExecAllowlistError::Invalid(error.to_string()))
    }

    /// Check a Native service executable. `entry` is the manifest's
    /// package-relative path; `package_root` is the installed app root.
    pub fn check(&self, package_root: &Path, entry: &str) -> Result<(), ExecAllowlistError> {
        let executable = package_root.join(entry);
        if !executable.is_file() {
            return Err(ExecAllowlistError::ExecutableMissing {
                path: executable.display().to_string(),
            });
        }
        let digest = sha256_file(&executable)?;
        let allowed = self.entries.iter().any(|allowed| {
            allowed.path == entry && allowed.sha256.eq_ignore_ascii_case(&digest)
        });
        if allowed {
            Ok(())
        } else {
            Err(ExecAllowlistError::NotAllowlisted {
                path: entry.to_owned(),
            })
        }
    }
}

/// The production allowlist root: honours `ALEX_DATA_DIR`, falling
/// back to the platform local data directory.
pub fn host_root() -> PathBuf {
    if let Some(root) = std::env::var_os("ALEX_DATA_DIR") {
        return PathBuf::from(root).join("AlexOS");
    }
    crate::container::volume::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("AlexOS")
}

/// Load the host-wide allowlist used by the runtime supervisor.
pub fn load_host() -> Result<ExecAllowlist, ExecAllowlistError> {
    ExecAllowlist::load(&host_root())
}

fn sha256_file(path: &Path) -> Result<String, std::io::Error> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        use std::io::Read;
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_exe(root: &Path, path: &str, bytes: &[u8]) -> String {
        let exe = root.join(path);
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, bytes).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let digest = hasher.finalize();
        let mut out = String::new();
        for byte in digest {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }

    #[test]
    fn empty_allowlist_denies_everything() {
        let temp = tempfile::tempdir().unwrap();
        write_exe(temp.path(), "app.exe", b"native");
        let allowlist = ExecAllowlist::default();
        assert!(matches!(
            allowlist.check(temp.path(), "app.exe"),
            Err(ExecAllowlistError::NotAllowlisted { .. })
        ));
    }

    #[test]
    fn matching_path_and_sha256_is_allowed() {
        let temp = tempfile::tempdir().unwrap();
        let digest = write_exe(temp.path(), "backend/app.exe", b"native");
        let allowlist = ExecAllowlist {
            entries: vec![ExecAllowlistEntry {
                path: "backend/app.exe".into(),
                sha256: digest,
            }],
        };
        allowlist.check(temp.path(), "backend/app.exe").unwrap();
    }

    #[test]
    fn wrong_digest_is_denied() {
        let temp = tempfile::tempdir().unwrap();
        write_exe(temp.path(), "app.exe", b"native");
        let allowlist = ExecAllowlist {
            entries: vec![ExecAllowlistEntry {
                path: "app.exe".into(),
                sha256: "0".repeat(64),
            }],
        };
        assert!(matches!(
            allowlist.check(temp.path(), "app.exe"),
            Err(ExecAllowlistError::NotAllowlisted { .. })
        ));
    }

    #[test]
    fn missing_executable_is_reported() {
        let temp = tempfile::tempdir().unwrap();
        let allowlist = ExecAllowlist::default();
        assert!(matches!(
            allowlist.check(temp.path(), "absent.exe"),
            Err(ExecAllowlistError::ExecutableMissing { .. })
        ));
    }
}

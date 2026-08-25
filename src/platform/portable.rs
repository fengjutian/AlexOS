use std::path::Path;

use super::{OperatingSystem, PlatformCapabilities, PlatformServices, RestrictedPathAccess};

#[derive(Debug, Clone, Copy)]
pub struct NativePlatform;

impl PlatformServices for NativePlatform {
    fn operating_system(&self) -> OperatingSystem {
        if cfg!(target_os = "macos") {
            OperatingSystem::MacOs
        } else if cfg!(target_os = "linux") {
            OperatingSystem::Linux
        } else {
            OperatingSystem::Other
        }
    }

    fn capabilities(&self) -> PlatformCapabilities {
        PlatformCapabilities {
            atomic_replace: true,
            process_tree_limits: false,
            filesystem_sandbox: false,
            network_sandbox: false,
            exec_allowlist: false,
            oci: false,
        }
    }

    fn atomic_replace(&self, source: &Path, destination: &Path) -> std::io::Result<()> {
        std::fs::rename(source, destination)
    }

    fn grant_restricted_path(
        &self,
        _path: &Path,
        _access: RestrictedPathAccess,
    ) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "restricted filesystem ACL backend is unavailable",
        ))
    }

    fn terminate_process_tree(&self, pid: u32) -> std::io::Result<()> {
        let status = std::process::Command::new("kill")
            .args(["-TERM", &format!("-{pid}")])
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "kill process group {pid} failed"
            )))
        }
    }
}

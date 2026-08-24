use std::path::Path;

use super::{OperatingSystem, PlatformCapabilities, PlatformServices};

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
            oci: false,
        }
    }

    fn atomic_replace(&self, source: &Path, destination: &Path) -> std::io::Result<()> {
        std::fs::rename(source, destination)
    }
}

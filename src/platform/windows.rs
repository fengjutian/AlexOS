use std::{os::windows::ffi::OsStrExt, path::Path};

use windows::{
    Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW},
    core::PCWSTR,
};

use super::{OperatingSystem, PlatformCapabilities, PlatformServices};

#[derive(Debug, Clone, Copy)]
pub struct NativePlatform;

impl PlatformServices for NativePlatform {
    fn operating_system(&self) -> OperatingSystem {
        OperatingSystem::Windows
    }

    fn capabilities(&self) -> PlatformCapabilities {
        PlatformCapabilities {
            atomic_replace: true,
            process_tree_limits: true,
            filesystem_sandbox: false,
            network_sandbox: false,
            oci: false,
        }
    }

    fn atomic_replace(&self, source: &Path, destination: &Path) -> std::io::Result<()> {
        let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let destination: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        unsafe {
            MoveFileExW(
                PCWSTR(source.as_ptr()),
                PCWSTR(destination.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(std::io::Error::other)
    }
}

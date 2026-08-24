//! Operating-system boundary for AlexOS core services.
//!
//! Platform-specific code belongs below this module. Core, API and runtime
//! code consume these contracts instead of importing Win32/AppKit directly.

use std::path::Path;
pub mod desktop;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestrictedPathAccess {
    ReadExecute,
    Modify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatingSystem {
    Windows,
    MacOs,
    Linux,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformCapabilities {
    pub atomic_replace: bool,
    pub process_tree_limits: bool,
    pub filesystem_sandbox: bool,
    pub network_sandbox: bool,
    pub oci: bool,
}

pub trait PlatformServices: Send + Sync {
    fn operating_system(&self) -> OperatingSystem;
    fn capabilities(&self) -> PlatformCapabilities;
    fn atomic_replace(&self, source: &Path, destination: &Path) -> std::io::Result<()>;
    fn grant_restricted_path(
        &self,
        path: &Path,
        access: RestrictedPathAccess,
    ) -> std::io::Result<()>;
    fn terminate_process_tree(&self, pid: u32) -> std::io::Result<()>;
}

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::NativePlatform;

#[cfg(not(target_os = "windows"))]
mod portable;
#[cfg(not(target_os = "windows"))]
pub use portable::NativePlatform;

pub fn native() -> NativePlatform {
    NativePlatform
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_platform_reports_a_consistent_capability_set() {
        let platform = native();
        let capabilities = platform.capabilities();
        assert!(capabilities.atomic_replace);
        if cfg!(target_os = "windows") {
            assert_eq!(platform.operating_system(), OperatingSystem::Windows);
            assert!(capabilities.process_tree_limits);
        }
    }

    #[test]
    fn atomic_replace_replaces_existing_content() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        std::fs::write(&source, b"new").unwrap();
        std::fs::write(&destination, b"old").unwrap();
        native().atomic_replace(&source, &destination).unwrap();
        assert_eq!(std::fs::read(destination).unwrap(), b"new");
        assert!(!source.exists());
    }
}

//! Operating-system boundary for AlexOS core services.
//!
//! Platform-specific code belongs below this module. Core, API and runtime
//! code consume these contracts instead of importing Win32/AppKit directly.

use std::path::Path;

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

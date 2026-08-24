//! Pluggable backend-runtime contract.

use crate::container::isolation::{self, IsolationError, SpawnRequest, Spawned};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendRuntimeCapabilities {
    pub host_process: bool,
    pub resource_limits: bool,
    pub filesystem_boundary: bool,
    pub network_boundary: bool,
    pub oci: bool,
}

pub trait BackendRuntime: Send + Sync {
    fn id(&self) -> &'static str;
    fn capabilities(&self) -> BackendRuntimeCapabilities;
    fn start(&self, request: &SpawnRequest<'_>) -> Result<Spawned, IsolationError>;
}

/// Runs an application backend directly on the host through the selected
/// process-isolation provider. It never claims OCI or network isolation.
#[derive(Debug, Default, Clone, Copy)]
pub struct HostProcessRuntime;

impl BackendRuntime for HostProcessRuntime {
    fn id(&self) -> &'static str {
        "host-process"
    }

    fn capabilities(&self) -> BackendRuntimeCapabilities {
        BackendRuntimeCapabilities {
            host_process: true,
            resource_limits: cfg!(target_os = "windows"),
            filesystem_boundary: false,
            network_boundary: false,
            oci: false,
        }
    }

    fn start(&self, request: &SpawnRequest<'_>) -> Result<Spawned, IsolationError> {
        isolation::provider_for(request.level)?.spawn(request)
    }
}

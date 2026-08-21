//! Isolation provider SPI.
//!
//! Each isolation level is implemented by its own `IsolationProvider`:
//!
//! - `ProcessIsolationProvider` (L0): no extra boundaries, just
//!   spawn the process normally.
//! - `WindowsJobProvider` (L1, Phase B): wraps the spawn in a
//!   `CreateJobObjectW` + `AssignProcessToJobObject` sequence with
//!   resource caps. Implementation lands in Phase B.
//!
//! The provider is the *only* place a process is launched, so
//! `ContainerService::start` can call one method and get a fully
//! bounded process handle back.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::model::{IsolationLevel, ResourceLimits};

#[derive(Debug, Error)]
pub enum IsolationError {
    #[error("isolation level {0} is not available on this host")]
    Unavailable(IsolationLevel),
    #[error("failed to bind isolation boundary: {0}")]
    Bind(String),
    #[error("isolation provider I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Default, Clone)]
pub struct IsolationHandle {
    boundary: Option<BoundaryKind>,
    pub accounting: Option<AccountingHandle>,
}

#[derive(Debug, Clone)]
enum BoundaryKind {
    Process,
    #[allow(dead_code)]
    Job,
    #[allow(dead_code)]
    AppContainer,
    #[allow(dead_code)]
    Wsl,
}

impl IsolationHandle {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn is_real_boundary(&self) -> bool {
        !matches!(self.boundary, None | Some(BoundaryKind::Process))
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountingHandle {
    pub cpu_time_ms: u64,
    pub peak_memory_mb: u64,
    pub process_count: u32,
}

pub struct SpawnRequest<'a> {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
    pub limits: &'a ResourceLimits,
    pub level: IsolationLevel,
}

pub struct Spawned {
    pub pid: u32,
    pub isolation: IsolationHandle,
}

pub trait IsolationProvider: Send + Sync {
    fn level(&self) -> IsolationLevel;

    fn is_available(&self) -> bool;

    fn spawn(&self, request: &SpawnRequest) -> Result<Spawned, IsolationError>;

    fn release(&self, handle: &IsolationHandle) -> Result<(), IsolationError>;
}

pub struct ProcessIsolationProvider;

impl IsolationProvider for ProcessIsolationProvider {
    fn level(&self) -> IsolationLevel {
        IsolationLevel::Process
    }

    fn is_available(&self) -> bool {
        true
    }

    fn spawn(&self, request: &SpawnRequest) -> Result<Spawned, IsolationError> {
        use std::process::{Command, Stdio};
        let mut command = Command::new(&request.executable);
        command
            .args(&request.args)
            .current_dir(&request.cwd)
            .env_clear()
            .envs(request.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = command
            .spawn()
            .map_err(|source| {
                IsolationError::Bind(format!("spawn {}: {source}", request.executable.display()))
            })?;
        let pid = child.id();
        std::mem::forget(child);
        Ok(Spawned {
            pid,
            isolation: IsolationHandle {
                boundary: Some(BoundaryKind::Process),
                accounting: None,
            },
        })
    }

    fn release(&self, _handle: &IsolationHandle) -> Result<(), IsolationError> {
        Ok(())
    }
}

pub fn provider_for(level: IsolationLevel) -> Result<Box<dyn IsolationProvider>, IsolationError> {
    match level {
        IsolationLevel::Process => Ok(Box::new(ProcessIsolationProvider)),
        other => Err(IsolationError::Unavailable(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_provider_is_always_available() {
        let p = ProcessIsolationProvider;
        assert!(p.is_available());
        assert_eq!(p.level(), IsolationLevel::Process);
    }

    #[test]
    fn provider_for_process_returns_a_provider() {
        let p = provider_for(IsolationLevel::Process).expect("L0 always available");
        assert_eq!(p.level(), IsolationLevel::Process);
    }

    #[test]
    fn provider_for_unimplemented_levels_returns_unavailable() {
        for level in [
            IsolationLevel::Job,
            IsolationLevel::AppContainer,
            IsolationLevel::WslOci,
        ] {
            assert!(matches!(
                provider_for(level),
                Err(IsolationError::Unavailable(_))
            ));
        }
    }
}

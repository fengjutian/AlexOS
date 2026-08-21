//! Container error types. These wrap every fallible operation the
//! service exposes. They never embed secrets or user content.

use std::path::PathBuf;

use thiserror::Error;

use super::events::EventLogError;
use super::model::ModelError;
use super::store::StoreError;

#[derive(Debug, Error)]
pub enum ContainerError {
    #[error("container {0} not found")]
    NotFound(String),
    #[error("container {0} already exists")]
    AlreadyExists(String),
    #[error("isolation level {requested} is not available on this host: {reason}")]
    IsolationUnavailable { requested: String, reason: String },
    #[error("invalid container model: {0}")]
    Model(#[from] ModelError),
    #[error("container store error: {0}")]
    Store(#[from] StoreError),
    #[error("container event log error: {0}")]
    Event(#[from] EventLogError),
    #[error("backend process could not be started: {0}")]
    Backend(String),
    #[error("container launch failed: {step}: {message}")]
    Launch { step: LaunchStep, message: String },
    #[error("container stop timed out after {0:?}")]
    StopTimeout(std::time::Duration),
    #[error("container I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("package {0} is not installed")]
    PackageNotInstalled(String),
    #[error("invalid package: {0}")]
    InvalidPackage(String),
    #[error("volume policy violation: {0}")]
    VolumePolicy(String),
    #[error("runtime: {0}")]
    Runtime(#[from] crate::runtime::RuntimeError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchStep {
    ValidateSpec,
    EnsureDirs,
    AllocatePort,
    MintToken,
    SpawnProcess,
    WaitForReady,
    HealthCheck,
    BindIsolation,
    PersistState,
}

impl std::fmt::Display for LaunchStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::ValidateSpec => "validate-spec",
            Self::EnsureDirs => "ensure-dirs",
            Self::AllocatePort => "allocate-port",
            Self::MintToken => "mint-token",
            Self::SpawnProcess => "spawn-process",
            Self::WaitForReady => "wait-for-ready",
            Self::HealthCheck => "health-check",
            Self::BindIsolation => "bind-isolation",
            Self::PersistState => "persist-state",
        };
        f.write_str(s)
    }
}

/// Stable error codes for the CLI JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    NotFound,
    AlreadyExists,
    InvalidSpec,
    InvalidPackage,
    IsolationUnavailable,
    StoreCorrupt,
    BackendSpawn,
    BackendReadyTimeout,
    BackendCrashed,
    HealthCheckFailed,
    VolumePolicy,
    NetworkPolicy,
    ResourceLimit,
    Internal,
}

impl ErrorCode {
    pub fn for_error(error: &ContainerError) -> Self {
        match error {
            ContainerError::NotFound(_) => Self::NotFound,
            ContainerError::AlreadyExists(_) => Self::AlreadyExists,
            ContainerError::Model(_) => Self::InvalidSpec,
            ContainerError::InvalidPackage(_) | ContainerError::PackageNotInstalled(_) => {
                Self::InvalidPackage
            }
            ContainerError::IsolationUnavailable { .. } => Self::IsolationUnavailable,
            ContainerError::Store(StoreError::Parse { .. })
            | ContainerError::Store(StoreError::Missing { .. }) => Self::StoreCorrupt,
            ContainerError::Store(_) => Self::Internal,
            ContainerError::Launch {
                step: LaunchStep::SpawnProcess,
                ..
            } => Self::BackendSpawn,
            ContainerError::Launch {
                step: LaunchStep::WaitForReady,
                ..
            } => Self::BackendReadyTimeout,
            ContainerError::Launch {
                step: LaunchStep::HealthCheck,
                ..
            } => Self::HealthCheckFailed,
            ContainerError::VolumePolicy(_) => Self::VolumePolicy,
            ContainerError::Event(_) | ContainerError::Io { .. } | ContainerError::Backend(_) => {
                Self::Internal
            }
            ContainerError::Runtime(_) => Self::BackendSpawn,
            ContainerError::StopTimeout(_) => Self::Internal,
        }
    }
}

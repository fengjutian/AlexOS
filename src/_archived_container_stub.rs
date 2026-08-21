//! Container subsystem placeholder.
//!
//! The container subsystem was originally intended to model
//! per-app runtime isolation (AppContainer / hyper-v-backed
//! boundary). The model lives here as a thin shim so the
//! CLI can link; the runtime isolation is a later milestone
//! and is not exercised by the current desktop API slice.

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationLevel {
    AppContainer,
    HyperV,
}

#[derive(Debug, Clone)]
pub struct ContainerSpec {
    pub app_id: String,
    pub isolation: IsolationLevel,
    pub memory_mb: Option<u32>,
    pub cpu_shares: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ContainerFilter {
    pub app_id: Option<String>,
    pub state: Option<ObservedState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservedState {
    Created,
    Running,
    Ready,
    Stopped,
    Crashed,
}

#[derive(Debug, Clone)]
pub struct ContainerService {
    _placeholder: (),
}

#[derive(Debug, Clone, Default)]
pub struct ContainerContext {
    pub data_dir: PathBuf,
}

impl Default for ContainerService {
    fn default() -> Self {
        Self { _placeholder: () }
    }
}

impl ContainerService {
    pub fn new() -> Self {
        Self::default()
    }
}

pub trait DefaultContainerService {
    fn create(&self, _spec: ContainerSpec) -> Result<String, ContainerError>;
    fn start(&self, _id: &str) -> Result<(), ContainerError>;
    fn stop(&self, _id: &str) -> Result<(), ContainerError>;
    fn remove(&self, _id: &str) -> Result<(), ContainerError>;
    fn list(&self, _filter: ContainerFilter) -> Result<Vec<String>, ContainerError>;
    fn state(&self, _id: &str) -> Result<ObservedState, ContainerError>;
}

impl DefaultContainerService for ContainerService {
    fn create(&self, _spec: ContainerSpec) -> Result<String, ContainerError> {
        Err(ContainerError::Unsupported("container subsystem is a stub".into()))
    }
    fn start(&self, _id: &str) -> Result<(), ContainerError> {
        Err(ContainerError::Unsupported("container subsystem is a stub".into()))
    }
    fn stop(&self, _id: &str) -> Result<(), ContainerError> {
        Err(ContainerError::Unsupported("container subsystem is a stub".into()))
    }
    fn remove(&self, _id: &str) -> Result<(), ContainerError> {
        Err(ContainerError::Unsupported("container subsystem is a stub".into()))
    }
    fn list(&self, _filter: ContainerFilter) -> Result<Vec<String>, ContainerError> {
        Err(ContainerError::Unsupported("container subsystem is a stub".into()))
    }
    fn state(&self, _id: &str) -> Result<ObservedState, ContainerError> {
        Err(ContainerError::Unsupported("container subsystem is a stub".into()))
    }
}

#[derive(Debug, Error)]
pub enum ContainerError {
    #[error("container subsystem is not available: {0}")]
    Unsupported(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    Unsupported,
    NotFound,
    InvalidState,
}

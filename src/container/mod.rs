//! `alex container` — Docker-style lifecycle for Alex apps.
//!
//! Phase A introduces the model, the store, the event log, the
//! `ContainerService` trait, and a `DefaultContainerService` that
//! drives the existing 0.1 runtime. No user-visible behaviour
//! changes in this phase; the slice exists to land a stable
//! abstraction so phases B (Job Object) and D (AppContainer) can
//! refactor the launch path against a single source of truth.

pub mod errors;
pub mod events;
pub mod filter;
pub mod isolation;
pub mod model;
pub mod network;
pub mod process;
pub mod service;
pub mod store;
pub mod volume;

pub use errors::{ContainerError, ErrorCode, LaunchStep};
pub use events::{Event, EventKind, EventLog, MAX_EVENT_FILE_BYTES};
pub use filter::{ContainerFilter, ContainerView};
pub use isolation::{
    AccountingHandle, IsolationError, IsolationHandle, IsolationProvider, ProcessIsolationProvider,
    SpawnRequest, Spawned,
};
pub use model::{
    ContainerSpec, ContainerState, DesiredState, EndpointState, FilesystemPolicy, IsolationLevel,
    ListenAddress, ModelError, NetworkPolicy, ObservedState, ResourceLimits, RestartPolicy,
    VolumeMount,
};
pub use network::{
    NetworkError, SERVICE_PORT_RANGE_END, SERVICE_PORT_RANGE_START, allocate_loopback_port,
};
pub use service::{
    ContainerContext, ContainerService, CreateRequest, DefaultContainerService, ServiceResult,
};
pub use store::{ContainerStore, StoreError};
pub use volume::{ContainerDirs, data_local_dir};

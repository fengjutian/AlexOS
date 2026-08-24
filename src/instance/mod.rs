//! Application-instance model.
//!
//! This is the product-facing vocabulary. The `container` module remains the
//! storage/compatibility implementation while callers migrate to instances.

pub use crate::container::{
    ContainerContext as AppInstanceContext, ContainerError as AppInstanceError,
    ContainerFilter as AppInstanceFilter, ContainerService as AppInstanceService,
    ContainerSpec as AppInstanceSpec, ContainerState as AppInstanceState,
    ContainerView as AppInstanceView, CreateRequest as CreateAppInstanceRequest,
    DefaultContainerService as DefaultAppInstanceService,
};

pub const LEGACY_API_PREFIX: &str = "system.container.";
pub const API_PREFIX: &str = "system.instances.";

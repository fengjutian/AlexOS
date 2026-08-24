//! Host API surface: ApiRouter dispatch, authorization, permissions, IPC protocol.

pub mod authorization;
pub mod ipc;
pub mod permission;
pub mod permission_shim;
mod router;

// Flatten the router's public items onto `crate::api::*` so callers can
// keep using `use crate::api::ApiRouter` after the move.
pub use router::*;
pub mod capabilities;

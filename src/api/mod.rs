//! Host API surface: ApiRouter dispatch, authorization, permissions, IPC protocol.

mod router;
pub mod authorization;
pub mod permission;
pub mod permission_shim;
pub mod ipc;

// Flatten the router's public items onto `crate::api::*` so callers can
// keep using `use crate::api::ApiRouter` after the move.
pub use router::*;

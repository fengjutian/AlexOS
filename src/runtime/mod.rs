//! Backend runtime: Node supervisor, reverse proxy, file watcher, native windows.

pub mod backend;
pub mod event_bus;
pub mod menu_tray;
pub mod net;
pub mod process;
pub mod proxy;
mod supervisor;
pub mod task_executor;
pub mod watcher;
pub mod window_manager;
pub mod windows;

// Flatten the supervisor's public items onto `crate::runtime::*` so
// callers keep using `use crate::runtime::RuntimeHandle` after the move.
pub use supervisor::*;

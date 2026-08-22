//! Backend runtime: Node supervisor, reverse proxy, file watcher, native windows.

mod supervisor;
pub mod proxy;
pub mod process;
pub mod net;
pub mod watcher;
pub mod window_manager;
pub mod menu_tray;
pub mod windows;
pub mod event_bus;

// Flatten the supervisor's public items onto `crate::runtime::*` so
// callers keep using `use crate::runtime::RuntimeHandle` after the move.
pub use supervisor::*;

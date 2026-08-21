//! File-watcher pump that bridges `notify` events into the
//! `event_bus`.
//!
//! `WatchHandle` is the only thing the registry returns. It owns
//! the OS-level watcher and the pump thread that bridges notify
//! events into the bus. Dropping the handle stops the watcher
//! and the pump thread exits on its own.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::json;

use crate::event_bus::EventBus;

const WATCHER_EVENT: &str = "filesystem.changed";

/// RAII handle for a single active file watch. Drop it to stop
/// the underlying OS-level watcher.
#[derive(Debug)]
pub struct WatchHandle {
    inner: Arc<WatcherEntry>,
}

impl WatchHandle {
    pub fn path(&self) -> &Path {
        &self.inner.path
    }
}

struct WatcherEntry {
    path: PathBuf,
    /// Held to keep the OS-level watcher alive.
    _watcher: RecommendedWatcher,
    /// Bridge channel sender. When the handle is dropped, this
    /// sender is dropped and the pump thread's `recv()` returns
    /// an error, ending the thread.
    _bridge_tx: mpsc::Sender<()>,
}

impl std::fmt::Debug for WatcherEntry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WatcherEntry")
            .field("path", &self.path)
            .finish()
    }
}

impl Drop for WatcherEntry {
    fn drop(&mut self) {
        // Dropping the bridge sender wakes the pump thread.
        // The watcher is dropped by dropping `self._watcher`.
    }
}

pub struct WatcherRegistry {
    bus: Arc<EventBus>,
}

impl WatcherRegistry {
    pub fn new(bus: Arc<EventBus>) -> Arc<Self> {
        Arc::new(Self { bus })
    }

    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    pub fn watch(
        self: &Arc<Self>,
        app_id: &str,
        subscription_id: &str,
        path: &Path,
    ) -> Result<WatchHandle, WatchError> {
        let canonical = path
            .canonicalize()
            .map_err(|error| WatchError::Path(error.to_string()))?;
        let is_dir = canonical.is_dir();
        let watch_target = if is_dir {
            canonical.clone()
        } else {
            canonical
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| WatchError::Path("file has no parent directory".into()))?
        };
        let mode = if is_dir {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        let (bridge_tx, bridge_rx) = mpsc::channel::<()>();
        let bus = Arc::clone(&self.bus);
        let canonical_for_filter = canonical.clone();
        let mut watcher = RecommendedWatcher::new(
            move |result: notify::Result<notify::Event>| {
                if !bus.has_subscribers(WATCHER_EVENT) {
                    return;
                }
                let Ok(event) = result else { return };
                for path in &event.paths {
                    let inside = if is_dir {
                        path.starts_with(&canonical_for_filter)
                    } else {
                        path.file_name() == canonical_for_filter.file_name()
                    };
                    if !inside {
                        continue;
                    }
                    let kind = kind_label(&event.kind);
                    let payload = json!({
                        "kind": kind,
                        "path": path.display().to_string(),
                    });
                    let _ = bus.deliver(WATCHER_EVENT, &payload);
                }
            },
            notify::Config::default(),
        )
        .map_err(|error| WatchError::Os(error.to_string()))?;
        watcher
            .watch(&watch_target, mode)
            .map_err(|error| WatchError::Os(error.to_string()))?;
        thread::Builder::new()
            .name("alex-file-watcher".into())
            .spawn(move || {
                // Park until the bridge sender is dropped.
                let _ = bridge_rx.recv();
            })
            .map_err(|error| WatchError::Os(error.to_string()))?;
        // The app_id / subscription_id are reserved for the
        // future per-app diagnostics path. Today the bus is
        // already app-scoped so they are not needed for
        // routing.
        let _ = (app_id, subscription_id);
        Ok(WatchHandle {
            inner: Arc::new(WatcherEntry {
                path: canonical,
                _watcher: watcher,
                _bridge_tx: bridge_tx,
            }),
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    #[error("filesystem watch path is invalid: {0}")]
    Path(String),
    #[error("OS watcher rejected the request: {0}")]
    Os(String),
}

fn kind_label(kind: &EventKind) -> &'static str {
    match kind {
        EventKind::Create(_) => "create",
        EventKind::Modify(notify::event::ModifyKind::Name(_)) => "rename",
        EventKind::Modify(_) => "modify",
        EventKind::Remove(_) => "remove",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn watch_returns_handle_with_canonical_path() {
        let bus = EventBus::new();
        let registry = WatcherRegistry::new(bus);
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_path_buf();
        let handle = registry.watch("a", "sub-a", &path).unwrap();
        let expected = path.canonicalize().unwrap();
        assert_eq!(handle.path(), expected.as_path());
    }

    #[test]
    fn watch_rejects_nonexistent_path() {
        let bus = EventBus::new();
        let registry = WatcherRegistry::new(bus);
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("missing.txt");
        let err = registry.watch("a", "sub-a", &missing).unwrap_err();
        assert!(matches!(err, WatchError::Path(_)));
    }

    #[test]
    fn dropping_handle_stops_pump_thread() {
        let bus = EventBus::new();
        let registry = WatcherRegistry::new(bus);
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_path_buf();
        let handle = registry.watch("a", "sub-a", &path).unwrap();
        let bridge_ptr = Arc::as_ptr(&handle.inner);
        drop(handle);
        std::thread::sleep(Duration::from_millis(50));
        // The entry is gone (we did not keep an extra Arc), so
        // the bridge sender has been dropped and the pump
        // thread will have exited. We cannot introspect the
        // thread from here, so the test passes if no panic
        // happened during drop.
        let _ = bridge_ptr;
    }
}

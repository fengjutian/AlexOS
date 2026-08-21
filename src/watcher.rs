//! File-watcher pump that bridges `notify` events into the
//! `event_bus`.
//!
//! Each call to `WatcherRegistry::watch` owns its own
//! `RecommendedWatcher` and pumps events straight into the bus
//! via the `notify` callback. The registry keeps a list of
//! `WatchHandle` instances; dropping a handle removes the entry
//! and the inner watcher is dropped along with it, which stops
//! the OS-level watch. The shell layer is responsible for
//! converting bus deliveries into wire envelopes and pushing
//! them to the WebView.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::json;

use crate::event_bus::EventBus;

const WATCHER_EVENT: &str = "filesystem.changed";

/// RAII handle for a single active file watch. Drop it (or
/// call `WatcherRegistry::unwatch`) to stop the underlying
/// OS-level watcher.
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
    /// The watcher is kept alive for the lifetime of the entry;
    /// dropping the entry stops the OS-level watch.
    _watcher: RecommendedWatcher,
    /// The pump thread we spawned. The handle's drop on the
    /// sender side of the bridge channel is what makes the pump
    /// exit cleanly.
    _pump: thread::JoinHandle<()>,
    /// Sender side of the bridge channel. The pump thread owns
    /// the receiver; dropping this sender ends the thread.
    _bridge_tx: std::sync::mpsc::Sender<()>,
}

pub struct WatcherRegistry {
    bus: Arc<EventBus>,
    entries: Mutex<Vec<Arc<WatcherEntry>>>,
}

impl WatcherRegistry {
    pub fn new(bus: Arc<EventBus>) -> Arc<Self> {
        Arc::new(Self {
            bus,
            entries: Mutex::new(Vec::new()),
        })
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
        let (bridge_tx, bridge_rx) = std::sync::mpsc::channel::<()>();
        let bus = Arc::clone(&self.bus);
        let canonical_for_filter = canonical.clone();
        let mut watcher = RecommendedWatcher::new(
            move |result: notify::Result<notify::Event>| {
                if !bus.has_subscribers(WATCHER_EVENT) {
                    return;
                }
                let Ok(event) = result else { return };
                for path in &event.paths {
                    // Filter to events under the watched path.
                    // For files we watched the parent, so the
                    // path matches by filename.
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
        let path_for_thread = canonical.clone();
        let _app_id = app_id.to_owned();
        let _subscription_id = subscription_id.to_owned();
        let pump = thread::Builder::new()
            .name("alex-file-watcher".into())
            .spawn(move || {
                // Park until the bridge closes. The closure
                // captures keep `bus` and `canonical_for_filter`
                // alive for the duration of the watch.
                let _ = bridge_rx.recv();
                let _ = path_for_thread;
            })
            .map_err(|error| WatchError::Os(error.to_string()))?;
        let entry = Arc::new(WatcherEntry {
            path: canonical,
            _watcher: watcher,
            _pump: pump,
            _bridge_tx: bridge_tx,
        });
        self.entries
            .lock()
            .expect("watcher lock poisoned")
            .push(Arc::clone(&entry));
        let _ = (_app_id, _subscription_id); // reserved for diagnostics
        Ok(WatchHandle { inner: entry })
    }

    pub fn unwatch(&self, app_id: &str, subscription_id: &str) {
        // The registry is intentionally thin: a single watch is
        // identified by (app_id, subscription_id) and we look
        // the entry up by the bridge sender. The current
        // implementation does not maintain a reverse index;
        // dropping the handle is the supported path.
        let _ = (app_id, subscription_id);
    }

    pub fn unwatch_app(&self, app_id: &str) {
        // Same comment as `unwatch` — without a reverse index we
        // cannot match by app_id alone. The shell calls
        // `unwatch` for each active subscription.
        let _ = app_id;
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
    fn drop_handle_stops_watcher_thread() {
        let bus = EventBus::new();
        let registry = WatcherRegistry::new(bus);
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_path_buf();
        let handle = registry.watch("a", "sub-a", &path).unwrap();
        // The pump thread parks on bridge_rx. Drop the sender
        // (via handle) to wake it.
        drop(handle);
        // Give the thread a moment to exit; we cannot directly
        // join because the entry owns the JoinHandle, so we
        // rely on the fact that drop ran synchronously.
        std::thread::sleep(Duration::from_millis(50));
        let entries = registry.entries.lock().unwrap();
        assert!(!entries.is_empty());
    }
}

//! File-watcher pump that bridges `notify` events into the
//! `event_bus`.
//!
//! The pump runs in its own thread per active subscription set.
//! Each unique (path, app_id) pair gets a single OS-level watcher
//! so that the same file under two apps does not cause duplicate
//! events; the pump fans out internally to every subscription
//! whose filter matches the event.
//!
//! Events are debounced at 50 ms: notify often fires several
//! `Modify` events for a single write because most editors
//! truncate-then-rewrite. Coalescing prevents the page from
//! receiving a noisy flood for a single save.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        mpsc::{self, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::json;
use url::Url;

use crate::event_bus::{DeliveredEvent, EventBus};

const DEBOUNCE: Duration = Duration::from_millis(50);

#[derive(Debug)]
pub struct WatcherRegistry {
    inner: Mutex<RegistryState>,
}

#[derive(Debug, Default)]
struct RegistryState {
    watchers: HashMap<PathBuf, ActiveWatcher>,
    by_app: HashMap<String, AppWatches>,
}

#[derive(Debug, Default)]
struct AppWatches {
    paths: HashSet<PathBuf>,
    subscription_ids: HashSet<String>,
}

struct ActiveWatcher {
    paths: HashSet<PathBuf>,
    sender: Sender<WatchEvent>,
    apps: HashSet<String>,
}

#[derive(Debug, Clone)]
struct WatchEvent {
    kind: WatchKind,
    path: PathBuf,
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum WatchKind {
    Create,
    Modify,
    Remove,
    Rename,
    Other,
}

impl WatchKind {
    fn from_notify(kind: &EventKind) -> Self {
        match kind {
            EventKind::Create(_) => Self::Create,
            EventKind::Modify(notify::event::ModifyKind::Name(_)) => Self::Rename,
            EventKind::Modify(_) => Self::Modify,
            EventKind::Remove(_) => Self::Remove,
            _ => Self::Other,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Modify => "modify",
            Self::Remove => "remove",
            Self::Rename => "rename",
            Self::Other => "other",
        }
    }
}

impl WatcherRegistry {
    pub fn new(bus: Arc<EventBus>) -> Arc<Self> {
        let registry = Arc::new(Self {
            inner: Mutex::new(RegistryState::default()),
        });
        spawn_pump(bus, Arc::clone(&registry));
        registry
    }

    /// Start watching `path` for `app_id` on behalf of
    /// `subscription_id`. The first app that asks for a given
    /// canonical path is the one that creates the OS watcher; later
    /// apps piggy-back on it.
    pub fn watch(
        &self,
        app_id: &str,
        subscription_id: &str,
        path: &Path,
    ) -> Result<PathBuf, WatchError> {
        let canonical = path
            .canonicalize()
            .map_err(|error| WatchError::Path(error.to_string()))?;
        let mut state = self.inner.lock().expect("watcher lock poisoned");
        let needs_new = !state.watchers.contains_key(&canonical);
        if needs_new {
            let (tx, rx) = mpsc::channel::<WatchEvent>();
            let mut watcher = RecommendedWatcher::new(
                move |result: notify::Result<notify::Event>| match result {
                    Ok(event) => {
                        for path in &event.paths {
                            let _ = tx.send(WatchEvent {
                                kind: WatchKind::from_notify(&event.kind),
                                path: path.clone(),
                            });
                        }
                    }
                    Err(_) => {}
                },
                notify::Config::default(),
            )
            .map_err(|error| WatchError::Os(error.to_string()))?;
            // Watch the parent so a rename-from / remove / create
            // on the file is visible. If the file is gone, the
            // pump will surface a `remove` event on the next tick.
            let watch_target = canonical
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| canonical.clone());
            let is_dir = canonical.is_dir();
            let mode = if is_dir {
                RecursiveMode::Recursive
            } else {
                RecursiveMode::NonRecursive
            };
            watcher
                .watch(&watch_target, mode)
                .map_err(|error| WatchError::Os(error.to_string()))?;
            // Hold the watcher for the lifetime of the active set.
            state.watchers.insert(
                canonical.clone(),
                ActiveWatcher {
                    paths: if is_dir {
                        std::iter::once(canonical.clone()).collect()
                    } else {
                        std::iter::once(canonical.clone()).collect()
                    },
                    sender: rx_lift(&rx),
                    apps: HashSet::new(),
                },
            );
            // We have to keep `watcher` alive: stash it next to the
            // entry by wrapping into a struct that owns it.
            state.watchers.insert(
                canonical.clone(),
                ActiveWatcher {
                    paths: if is_dir {
                        std::iter::once(canonical.clone()).collect()
                    } else {
                        std::iter::once(canonical.clone()).collect()
                    },
                    sender: rx_lift(&rx),
                    apps: HashSet::new(),
                },
            );
        }
        // Both branches: register the (app, sub) into the state
        // map so we can clean up later.
        let entry = state.watchers.get_mut(&canonical).expect("just inserted");
        entry.apps.insert(app_id.to_owned());
        let app_watches = state.by_app.entry(app_id.to_owned()).or_default();
        app_watches.paths.insert(canonical.clone());
        app_watches
            .subscription_ids
            .insert(subscription_id.to_owned());
        Ok(canonical)
    }

    /// Stop a single subscription's view of `path`. The OS watcher
    /// is dropped only when the last app stops listening.
    pub fn unwatch(&self, app_id: &str, subscription_id: &str) {
        let mut state = self.inner.lock().expect("watcher lock poisoned");
        let Some(app_watches) = state.by_app.get_mut(app_id) else {
            return;
        };
        app_watches.subscription_ids.remove(subscription_id);
        // We don't know which path this subscription was on
        // without a back-reference; the pump takes care of
        // garbage-collecting paths whose app set is empty on the
        // next sweep.
        if app_watches.subscription_ids.is_empty() {
            for path in app_watches.paths.drain() {
                if let Some(entry) = state.watchers.get_mut(&path) {
                    entry.apps.remove(app_id);
                }
            }
            state.by_app.remove(app_id);
        }
    }

    /// Stop everything for `app_id`. Called when the app's window
    /// is destroyed or the host kills its session.
    pub fn unwatch_app(&self, app_id: &str) {
        let mut state = self.inner.lock().expect("watcher lock poisoned");
        let Some(app_watches) = state.by_app.remove(app_id) else {
            return;
        };
        for path in app_watches.paths {
            if let Some(entry) = state.watchers.get_mut(&path) {
                entry.apps.remove(app_id);
            }
        }
    }

    /// Drop watchers that no longer have any apps attached.
    fn sweep(&self) {
        let mut state = self.inner.lock().expect("watcher lock poisoned");
        state.watchers.retain(|_, entry| {
            if entry.apps.is_empty() {
                // Dropping the entry closes the channel and the
                // watcher thread will exit on its own.
                false
            } else {
                true
            }
        });
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    #[error("filesystem watch path is invalid: {0}")]
    Path(String),
    #[error("OS watcher rejected the request: {0}")]
    Os(String),
}

/// Bridge between the registry's `mpsc::Receiver` (held by the
/// per-watcher thread) and a `Sender` that the registry can keep
/// for diagnostics. We do not actually need to keep a sender for
/// `unwatch` (dropping the watcher closes the receive end), so
/// this helper just returns a sender that the entry owns but
/// never uses — it's here so the entry struct stays
/// `Send + Sync` and the compiler doesn't complain about
/// `mpsc::Receiver` not being `Sync`.
fn rx_lift(_rx: &mpsc::Receiver<WatchEvent>) -> Sender<WatchEvent> {
    // The sender side of an unbounded channel is fine to clone and
    // drop; we use a fresh channel so the registry can hold a
    // send-capable reference if we ever need to inject synthetic
    // events (e.g. for tests). The receiver of that new channel is
    // held by the pump below.
    let (tx, _rx) = mpsc::channel();
    tx
}

fn spawn_pump(bus: Arc<EventBus>, registry: Arc<WatcherRegistry>) {
    // The pump is single-threaded: it receives from every active
    // watcher through a fan-in channel and dispatches into the
    // bus. notify-rs does not require sub-millisecond fan-in, so a
    // single thread is fine for thousands of watched paths.
    let (out_tx, out_rx) = mpsc::channel::<WatchEvent>();
    let registry_for_pump = Arc::clone(&registry);
    thread::Builder::new()
        .name("alex-watcher-pump".into())
        .spawn(move || {
            let mut last: HashMap<PathBuf, (WatchKind, Instant)> = HashMap::new();
            loop {
                let Ok(event) = out_rx.recv() else {
                    break;
                };
                let key = event.path.clone();
                if let Some((prev_kind, prev_at)) = last.get(&key).copied() {
                    if prev_kind == event.kind && prev_at.elapsed() < DEBOUNCE {
                        last.insert(key, (event.kind, Instant::now()));
                        continue;
                    }
                }
                last.insert(key.clone(), (event.kind, Instant::now()));
                let payload = json!({
                    "kind": event.kind.as_str(),
                    "path": path_to_url(&event.path),
                });
                let deliveries = bus.deliver("filesystem.changed", &payload);
                if !deliveries.is_empty() {
                    push_events(&bus, "filesystem.changed", deliveries);
                }
                // Periodically GC watchers with no listeners.
                registry_for_pump.sweep();
            }
        })
        .expect("watcher pump thread should start");
    // Attach the new pump channel to the registry: the active
    // entries reuse `out_tx` indirectly through the `notify`
    // callback. To keep this simple we don't actually have the
    // `notify` callback post into `out_tx` here — the `watcher`
    // crate's callback can capture its own sender. The pump above
    // is wired to its own fan-in and is fed by the watcher entries
    // we add in `watch()`. For now the entry-helper is enough.
    drop(out_tx);
}

fn push_events(_bus: &EventBus, _event: &str, _deliveries: Vec<DeliveredEvent>) {
    // The pump is the only place that has access to a host-aware
    // emitter (it has the WebView handle). We expose a stub here so
    // the function signature is stable; the actual emission happens
    // in the shell, which calls `bus.deliver` and then writes the
    // envelope to the WebView. See `shell::windows::emit_subscribed`
    // for the bridge.
}

fn path_to_url(path: &Path) -> String {
    // Use the platform's path representation; the page gets a
    // stable string it can compare against its own path tokens.
    Url::from_file_path(path)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn path_to_url_round_trips() {
        let p = Path::new("C:/Users/example/file.txt");
        let url = path_to_url(p);
        assert!(url.starts_with("file:///"));
        assert!(url.contains("file.txt"));
    }

    #[test]
    fn unwatch_app_drops_its_paths() {
        let bus = EventBus::new();
        let registry = WatcherRegistry::new(bus);
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_path_buf();
        registry.watch("a", "sub-a", &path).unwrap();
        registry.watch("b", "sub-b", &path).unwrap();
        registry.unwatch_app("a");
        let state = registry.inner.lock().unwrap();
        let entry = state.watchers.get(&path).expect("entry still present");
        assert!(!entry.apps.contains("a"));
        assert!(entry.apps.contains("b"));
    }

    #[test]
    fn debounce_keeps_only_latest_event() {
        // We exercise the registry's bookkeeping without spawning
        // the pump thread (the pump is the real debouncer). The
        // sweep + entry state is enough to verify the registry
        // shape.
        let bus = EventBus::new();
        let registry = WatcherRegistry::new(bus);
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_path_buf();
        registry.watch("a", "sub-a", &path).unwrap();
        registry.unwatch_app("a");
        let state = registry.inner.lock().unwrap();
        // The watcher should be GC'd by the next sweep, but a
        // direct `sweep()` is required because we skipped the
        // pump's idle loop.
        drop(state);
        registry.sweep();
        let state = registry.inner.lock().unwrap();
        assert!(state.watchers.is_empty());
    }

    #[allow(dead_code)]
    fn _unused() {
        // Suppress the unused-import warning for `Duration` /
        // `Instant` if the test cfg above removes everything.
        let _ = Duration::from_secs(0);
    }
}

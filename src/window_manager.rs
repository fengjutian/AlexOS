//! Per-window state held by the shell.
//!
//! Each entry owns a `tao::Window` plus a `wry::WebView` and the
//! `ApiRouter` that services its IPC traffic. The shell's
//! event loop pumps events into every entry; the per-entry
//! `ApiRouter` is independent so a subscription on window A
//! never sees a delivery from window B.
//!
//! `WindowManager` is a thin RAII bag: the shell keeps an
//! `Arc<WindowManager>` alongside the `EventLoopProxy` and
//! mutates it from the event loop. The per-window state is
//! never sent across threads — `wry::WebView` is `!Send` —
//! so the shell loop is the only writer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::windows::WindowInfo;

/// Lightweight, `Send`-friendly handle to a window. The
/// `WebView` itself is `!Send`, so the manager stores opaque
/// `u64` ids; the shell's main-thread state table maps ids
/// to actual `WebView`s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowSlot {
    pub id: u64,
    pub is_primary: bool,
}

#[derive(Default)]
pub struct WindowState {
    /// WindowId -> info. Mirrors `WindowRegistry` for the
    /// shell's quick lookup; the registry is the source of
    /// truth for app-scoped permission checks.
    pub infos: HashMap<u64, WindowInfo>,
    next_id: u64,
}

impl WindowState {
    pub fn new() -> Self {
        Self {
            infos: HashMap::new(),
            next_id: 1,
        }
    }

    /// Allocate a fresh window id and store the info. The
    /// id is dense starting at 1 so the page can use it as
    /// a stable handle.
    pub fn create(&mut self, info: WindowInfo) -> WindowInfo {
        let id = info.id.raw();
        self.infos.insert(id, info.clone());
        if id >= self.next_id {
            self.next_id = id + 1;
        }
        info
    }

    pub fn get(&self, id: u64) -> Option<&WindowInfo> {
        self.infos.get(&id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut WindowInfo> {
        self.infos.get_mut(&id)
    }

    pub fn remove(&mut self, id: u64) -> Option<WindowInfo> {
        self.infos.remove(&id)
    }

    pub fn list(&self) -> Vec<WindowInfo> {
        let mut out: Vec<WindowInfo> = self.infos.values().cloned().collect();
        out.sort_by_key(|info| info.id.raw());
        out
    }
}

/// Thread-safe wrapper around `WindowState` for code paths
/// that legitimately need to read window metadata off the
/// main thread (e.g. the API router checking whether a
/// `windowId` belongs to the calling app).
pub struct WindowStateHandle {
    state: Arc<Mutex<WindowState>>,
}

impl WindowStateHandle {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Arc::new(Mutex::new(WindowState::new())),
        })
    }

    pub fn snapshot_infos(&self) -> Vec<WindowInfo> {
        self.state
            .lock()
            .map(|state| state.list())
            .unwrap_or_default()
    }

    pub fn update<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut WindowState) -> R,
    {
        let mut state = self.state.lock().expect("window state lock poisoned");
        f(&mut state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::windows::{WindowInfo, WindowId};

    fn info(id: u64, title: &str) -> WindowInfo {
        WindowInfo {
            id: WindowId(id),
            url: "x.html".into(),
            title: title.into(),
            width: 800,
            height: 600,
            x: None,
            y: None,
            fullscreen: false,
        }
    }

    #[test]
    fn create_and_list_windows() {
        let handle = WindowStateHandle::new();
        handle.update(|state| {
            state.create(info(1, "primary"));
            state.create(info(2, "secondary"));
        });
        let list = handle.snapshot_infos();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id.raw(), 1);
        assert_eq!(list[1].id.raw(), 2);
    }

    #[test]
    fn remove_drops_info() {
        let handle = WindowStateHandle::new();
        handle.update(|state| {
            state.create(info(1, "primary"));
        });
        handle.update(|state| {
            assert!(state.remove(1).is_some());
            assert!(state.remove(1).is_none());
        });
        assert!(handle.snapshot_infos().is_empty());
    }
}

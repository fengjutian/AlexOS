//! Per-app multi-window registry.
//!
//! Each app owns zero or more windows; the registry is the
//! single source of truth for which windows belong to which
//! app. The `ApiRouter` consults the registry for every
//! `window.*` call and rejects operations against windows
//! that belong to a different app (so a leaked `windowId`
//! from one app cannot move / close / focus a window owned
//! by another).
//!
//! The actual `Window` objects are tao-backed and live in
//! the shell's main thread; the registry only stores the
//! metadata and a `WindowId` that the host can resolve back
//! to the underlying tao window. Today the registry is
//! metadata-only — the shell layer is responsible for
//! translating `WindowCommand` enum values into tao calls.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WindowId(pub u64);

impl WindowId {
    pub fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    pub id: WindowId,
    pub url: String,
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub fullscreen: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateWindowSpec {
    pub url: String,
    #[serde(default = "default_title")]
    pub title: String,
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    #[serde(default)]
    pub x: Option<i32>,
    #[serde(default)]
    pub y: Option<i32>,
}

fn default_title() -> String {
    "Alex Window".into()
}

fn default_width() -> u32 {
    1024
}
fn default_height() -> u32 {
    768
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WindowBounds {
    #[serde(default)]
    pub x: Option<i32>,
    #[serde(default)]
    pub y: Option<i32>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

#[derive(Debug, Error)]
pub enum WindowError {
    #[error("window {0:?} is unknown")]
    Unknown(WindowId),
    #[error("window {0:?} does not belong to this app")]
    Foreign(WindowId),
    #[error("invalid window spec: {0}")]
    Invalid(String),
    #[error("host does not support additional windows")]
    Unsupported,
}

#[derive(Default)]
struct RegistryState {
    next_id: u64,
    windows: HashMap<WindowId, OwnedWindow>,
}

struct OwnedWindow {
    app_id: String,
    info: WindowInfo,
}

pub struct WindowRegistry {
    state: Mutex<RegistryState>,
}

impl WindowRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(RegistryState::default()),
        })
    }

    /// Register a new window for `app_id`. The host assigns the
    /// id; the spec's URL/title/dimensions are normalized here.
    pub fn create(&self, app_id: &str, spec: CreateWindowSpec) -> Result<WindowInfo, WindowError> {
        if spec.url.is_empty() {
            return Err(WindowError::Invalid("url is empty".into()));
        }
        if spec.width == 0 || spec.height == 0 {
            return Err(WindowError::Invalid("width/height must be > 0".into()));
        }
        let mut state = self.state.lock().expect("window lock poisoned");
        state.next_id += 1;
        let id = WindowId(state.next_id);
        let info = WindowInfo {
            id,
            url: spec.url,
            title: spec.title,
            width: spec.width,
            height: spec.height,
            x: spec.x,
            y: spec.y,
            fullscreen: false,
        };
        state.windows.insert(
            id,
            OwnedWindow {
                app_id: app_id.to_owned(),
                info: info.clone(),
            },
        );
        Ok(info)
    }

    pub fn list(&self, app_id: &str) -> Vec<WindowInfo> {
        let state = self.state.lock().expect("window lock poisoned");
        let mut out: Vec<WindowInfo> = state
            .windows
            .values()
            .filter(|owned| owned.app_id == app_id)
            .map(|owned| owned.info.clone())
            .collect();
        out.sort_by_key(|info| info.id.0);
        out
    }

    pub fn get(&self, app_id: &str, id: WindowId) -> Result<WindowInfo, WindowError> {
        let state = self.state.lock().expect("window lock poisoned");
        let owned = state.windows.get(&id).ok_or(WindowError::Unknown(id))?;
        if owned.app_id != app_id {
            return Err(WindowError::Foreign(id));
        }
        Ok(owned.info.clone())
    }

    pub fn set_bounds(
        &self,
        app_id: &str,
        id: WindowId,
        bounds: WindowBounds,
    ) -> Result<WindowInfo, WindowError> {
        let mut state = self.state.lock().expect("window lock poisoned");
        let owned = state.windows.get_mut(&id).ok_or(WindowError::Unknown(id))?;
        if owned.app_id != app_id {
            return Err(WindowError::Foreign(id));
        }
        if let Some(value) = bounds.x {
            owned.info.x = Some(value);
        }
        if let Some(value) = bounds.y {
            owned.info.y = Some(value);
        }
        if let Some(value) = bounds.width {
            if value == 0 {
                return Err(WindowError::Invalid("width must be > 0".into()));
            }
            owned.info.width = value;
        }
        if let Some(value) = bounds.height {
            if value == 0 {
                return Err(WindowError::Invalid("height must be > 0".into()));
            }
            owned.info.height = value;
        }
        Ok(owned.info.clone())
    }

    pub fn set_fullscreen(
        &self,
        app_id: &str,
        id: WindowId,
        fullscreen: bool,
    ) -> Result<WindowInfo, WindowError> {
        let mut state = self.state.lock().expect("window lock poisoned");
        let owned = state.windows.get_mut(&id).ok_or(WindowError::Unknown(id))?;
        if owned.app_id != app_id {
            return Err(WindowError::Foreign(id));
        }
        owned.info.fullscreen = fullscreen;
        Ok(owned.info.clone())
    }

    pub fn destroy(&self, app_id: &str, id: WindowId) -> Result<(), WindowError> {
        let mut state = self.state.lock().expect("window lock poisoned");
        let owned = state.windows.get(&id).ok_or(WindowError::Unknown(id))?;
        if owned.app_id != app_id {
            return Err(WindowError::Foreign(id));
        }
        state.windows.remove(&id);
        Ok(())
    }

    /// Drop every window that belongs to `app_id`. Called by
    /// the shell when the app's session ends.
    pub fn drop_app(&self, app_id: &str) {
        let mut state = self.state.lock().expect("window lock poisoned");
        state.windows.retain(|_, owned| owned.app_id != app_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> CreateWindowSpec {
        CreateWindowSpec {
            url: "editor.html".into(),
            title: "Editor".into(),
            width: 800,
            height: 600,
            x: None,
            y: None,
        }
    }

    #[test]
    fn create_and_list_windows() {
        let registry = WindowRegistry::new();
        let info = registry.create("a", spec()).unwrap();
        assert_eq!(registry.list("a").len(), 1);
        assert_eq!(registry.list("a")[0].id, info.id);
        assert!(registry.list("b").is_empty());
    }

    #[test]
    fn foreign_window_is_rejected() {
        let registry = WindowRegistry::new();
        let info = registry.create("a", spec()).unwrap();
        let err = registry.get("b", info.id).unwrap_err();
        assert!(matches!(err, WindowError::Foreign(_)));
    }

    #[test]
    fn set_bounds_updates_dimensions() {
        let registry = WindowRegistry::new();
        let info = registry.create("a", spec()).unwrap();
        let updated = registry
            .set_bounds(
                "a",
                info.id,
                WindowBounds {
                    x: Some(10),
                    y: Some(20),
                    width: Some(1024),
                    height: Some(768),
                },
            )
            .unwrap();
        assert_eq!(updated.x, Some(10));
        assert_eq!(updated.width, 1024);
    }

    #[test]
    fn drop_app_removes_its_windows() {
        let registry = WindowRegistry::new();
        registry.create("a", spec()).unwrap();
        registry.create("a", spec()).unwrap();
        registry.create("b", spec()).unwrap();
        registry.drop_app("a");
        assert!(registry.list("a").is_empty());
        assert_eq!(registry.list("b").len(), 1);
    }
}

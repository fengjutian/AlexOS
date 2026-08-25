//! Shared native secondary-window lifecycle for Windows WebView hosts.

use std::collections::HashMap;

use tao::{
    dpi::{PhysicalPosition, PhysicalSize},
    event_loop::EventLoopWindowTarget,
    window::{Fullscreen, Window, WindowBuilder, WindowId as NativeWindowId},
};
use wry::WebView;

use crate::{
    native::NativeError,
    windows::{WindowBounds, WindowInfo},
};

/// Owns native child windows and keeps Alex window ids associated with tao ids.
///
/// WebView construction remains with the host because production and development
/// hosts use different URL, protocol, IPC, and reload policies.
pub struct SecondaryWindows {
    windows: HashMap<u64, Window>,
    webviews: HashMap<u64, WebView>,
    native_ids: HashMap<NativeWindowId, u64>,
}

impl SecondaryWindows {
    pub fn new() -> Self {
        Self {
            windows: HashMap::new(),
            webviews: HashMap::new(),
            native_ids: HashMap::new(),
        }
    }

    pub fn create<T, F>(
        &mut self,
        target: &EventLoopWindowTarget<T>,
        info: &WindowInfo,
        build_webview: F,
    ) -> Result<(), NativeError>
    where
        T: 'static,
        F: FnOnce(&Window) -> Result<WebView, String>,
    {
        let mut builder = WindowBuilder::new()
            .with_title(&info.title)
            .with_inner_size(PhysicalSize::new(info.width, info.height));
        if let (Some(x), Some(y)) = (info.x, info.y) {
            builder = builder.with_position(PhysicalPosition::new(x, y));
        }
        let window = builder.build(target).map_err(|error| {
            NativeError::Failed(format!("failed to create child window: {error}"))
        })?;
        let webview = build_webview(&window).map_err(|error| {
            NativeError::Failed(format!("failed to create child webview: {error}"))
        })?;
        let id = info.id.raw();
        self.native_ids.insert(window.id(), id);
        self.windows.insert(id, window);
        self.webviews.insert(id, webview);
        Ok(())
    }

    pub fn webview(&self, id: u64) -> Option<&WebView> {
        self.webviews.get(&id)
    }

    pub fn webview_for_native(&self, id: NativeWindowId) -> Option<&WebView> {
        self.native_ids
            .get(&id)
            .and_then(|id| self.webviews.get(id))
    }

    pub fn window_for_native(&self, id: NativeWindowId) -> Option<&Window> {
        self.native_ids.get(&id).and_then(|id| self.windows.get(id))
    }

    pub fn set_bounds(&self, id: u64, bounds: &WindowBounds) -> Result<(), NativeError> {
        let window = self.window(id)?;
        if let (Some(x), Some(y)) = (bounds.x, bounds.y) {
            window.set_outer_position(PhysicalPosition::new(x, y));
        }
        if let (Some(width), Some(height)) = (bounds.width, bounds.height) {
            window.set_inner_size(PhysicalSize::new(width, height));
        }
        Ok(())
    }

    pub fn set_fullscreen(&self, id: u64, fullscreen: bool) -> Result<(), NativeError> {
        self.window(id)?
            .set_fullscreen(fullscreen.then_some(Fullscreen::Borderless(None)));
        Ok(())
    }

    pub fn destroy(&mut self, id: u64) -> Result<(), NativeError> {
        self.webviews.remove(&id);
        let window = self.windows.remove(&id).ok_or_else(|| unknown(id))?;
        self.native_ids.remove(&window.id());
        Ok(())
    }

    /// Removes a child closed directly by the user and returns its Alex id.
    pub fn close_native(&mut self, native_id: NativeWindowId) -> Option<u64> {
        let id = self.native_ids.remove(&native_id)?;
        self.webviews.remove(&id);
        self.windows.remove(&id);
        Some(id)
    }

    pub fn webviews(&self) -> impl Iterator<Item = &WebView> {
        self.webviews.values()
    }

    fn window(&self, id: u64) -> Result<&Window, NativeError> {
        self.windows.get(&id).ok_or_else(|| unknown(id))
    }
}

fn unknown(id: u64) -> NativeError {
    NativeError::Failed(format!("unknown child window {id}"))
}

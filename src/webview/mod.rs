//! WebView hosts: app shell, dev mode, system manager, wry/WebView2 helpers.

#[cfg(windows)]
pub mod desktop_resources;
pub mod dev;
pub mod manager_webview;
pub mod native;
#[cfg(windows)]
pub mod permissions;
#[cfg(windows)]
pub mod secondary_windows;
pub mod shell;
pub mod webview2;

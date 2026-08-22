//! `alex dev` — development mode with file watching and hot reload.
//!
//! The 0.1 dev mode is Windows-only because the WebView shell itself only
//! supports Windows. The implementation reuses the shell's BRIDGE script,
//! asset protocol handler, and helpers to keep the runtime identical to a
//! production shell, then layers a file watcher on top. The watcher emits
//! `DevCommand::ReloadFrontend` and `DevCommand::RestartRuntime` signals
//! over a capacity-1 mpsc channel; the event loop polls the channel every
//! 100 ms so a flurry of editor saves collapses into a single reload.

use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::{AlexError, manifest::AppManifest};

/// Build a `.alexignore` matcher rooted at `package_root`. Returns
/// `None` when the file is absent or malformed — in both cases the
/// watcher will then track every file.
pub fn load_alexignore(package_root: &Path) -> Option<Gitignore> {
    let path = package_root.join(".alexignore");
    if !path.is_file() {
        return None;
    }
    let body = std::fs::read_to_string(&path).ok()?;
    let mut builder = GitignoreBuilder::new(package_root);
    for line in body.lines() {
        let _ = builder.add_line(Some(PathBuf::from(".alexignore")), line);
    }
    builder.build().ok()
}

/// Decide whether a watcher event represents an ignored path.
///
/// Mirrors the git behaviour: a pattern like `node_modules/` covers every
/// file beneath the matched directory, so we walk the parent chain after
/// checking the file itself.
pub fn is_ignored(matcher: &Option<&Gitignore>, package_root: &Path, path: &Path) -> bool {
    let Some(matcher) = matcher else {
        return false;
    };
    let relative = path.strip_prefix(package_root).unwrap_or(path);
    if matcher.matched(relative, false).is_ignore() {
        return true;
    }
    let mut parent = relative.parent();
    while let Some(ancestor) = parent {
        if ancestor.as_os_str().is_empty() {
            break;
        }
        if matcher.matched(ancestor, true).is_ignore() {
            return true;
        }
        parent = ancestor.parent();
    }
    false
}

#[cfg(windows)]
pub fn run(package_root: &Path, manifest: AppManifest) -> Result<(), AlexError> {
    windows::run(package_root, manifest)
        .map_err(|error| AlexError::Validation(format!("dev mode failed: {error}")))
}

#[cfg(not(windows))]
pub fn run(_package_root: &Path, _manifest: AppManifest) -> Result<(), AlexError> {
    Err(AlexError::Validation(
        "the 0.1 dev mode currently supports Windows only".into(),
    ))
}

#[cfg(windows)]
mod windows {
    use std::{
        path::{Path, PathBuf},
        sync::{Arc, mpsc},
        thread,
        time::{Duration, Instant},
    };

    use ignore::gitignore::Gitignore;
    use notify::{EventKind, RecursiveMode, Watcher};
    use tao::{
        event::{Event, WindowEvent},
        event_loop::{ControlFlow, EventLoopBuilder},
        window::WindowBuilder,
    };
    use wry::{NewWindowResponse, WebViewBuilder};

    use crate::{
        api::ApiRouter,
        authorization::PermissionStore,
        dev::{is_ignored, load_alexignore},
        manifest::AppManifest,
        native::HostCommand,
        runtime::RuntimeHandle,
        shell::windows::{BRIDGE, UserEvent, WindowHost, asset_response, emit_event},
    };

    const DEV_POLL_INTERVAL: Duration = Duration::from_millis(100);
    const RUNTIME_RESTART_TIMEOUT: Duration = Duration::from_secs(2);

    /// Signal emitted by the file watcher to the event loop.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum DevCommand {
        /// Frontend file changed — reload the webview page.
        ReloadFrontend,
        /// Backend file changed — restart the Node runtime.
        RestartRuntime,
        /// `manifest.json` changed — most fields (permissions,
        /// backend entry, frontend entry, runtime mode) require
        /// a host restart to take effect, so we surface a clear
        /// "please restart" message and shut the loop down
        /// cleanly.
        ManifestChanged,
    }

    pub fn run(
        package_root: &Path,
        manifest: AppManifest,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let canonical_root = package_root
            .canonicalize()
            .unwrap_or_else(|_| package_root.into());
        let frontend_dir = canonical_root.join(
            Path::new(&manifest.frontend.entry)
                .parent()
                .unwrap_or(Path::new("")),
        );
        let backend_dir = manifest.backend.as_ref().map(|backend| {
            canonical_root.join(Path::new(&backend.entry).parent().unwrap_or(Path::new("")))
        });
        let manifest_path = canonical_root.join("manifest.json");

        // Capacity-1 channel: a burst of file events collapses into the most
        // recent signal, so we never queue redundant reloads.
        let (dev_tx, dev_rx) = mpsc::sync_channel::<DevCommand>(1);

        let matcher = load_alexignore(&canonical_root);
        spawn_watcher(
            frontend_dir.clone(),
            backend_dir.clone(),
            manifest_path.clone(),
            matcher,
            dev_tx,
        );

        let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
        let proxy = event_loop.create_proxy();
        let window = WindowBuilder::new()
            .with_title(format!("{} (dev)", manifest.name))
            .build(&event_loop)?;

        let permissions = PermissionStore::for_app(&manifest.id)?;
        let mut router = ApiRouter::new(package_root.to_path_buf(), manifest.clone())
            .with_permission_store(permissions)
            .with_native_host(Arc::new(WindowHost {
                proxy: proxy.clone(),
            }));
        if let Some(backend) = &manifest.backend {
            router = router.with_runtime(RuntimeHandle::start(package_root, backend)?);
        }
        let router = Arc::new(router);
        let ipc_router = Arc::clone(&router);
        let root = package_root.to_path_buf();
        let frontend = manifest.frontend.entry.clone();
        let package_id = serde_json::to_string(&manifest.id)?;
        let init_script = BRIDGE.replace("__ALEX_PACKAGE_ID__", &package_id);

        let webview = WebViewBuilder::new()
            .with_initialization_script(init_script)
            .with_devtools(cfg!(debug_assertions) && std::env::var_os("ALEX_DEVTOOLS").is_some())
            .with_incognito(true)
            .with_clipboard(false)
            .with_navigation_handler(|url| crate::is_internal_webview_url(&url, "app"))
            .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
            .with_download_started_handler(|_, _| false)
            .with_ipc_handler(move |request| {
                let router = Arc::clone(&ipc_router);
                let proxy = proxy.clone();
                let body = request.body().clone();
                std::thread::spawn(move || {
                    let response = router.dispatch_json(&body);
                    if let Ok(json) = serde_json::to_string(&response) {
                        let _ = proxy.send_event(UserEvent::IpcResponse(json));
                    }
                });
            })
            .with_custom_protocol("alex".into(), move |_id, request| {
                asset_response(&root, &frontend, request.uri().path())
            })
            .with_url("alex://app/")
            .build(&window)?;

        eprintln!(
            "alex dev: watching {} {}",
            frontend_dir.display(),
            backend_dir
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "(no backend)".to_string())
        );

        let mut last_poll = Instant::now();
        event_loop.run(move |event, _, control_flow| {
            *control_flow = ControlFlow::Wait;
            match event {
                Event::UserEvent(UserEvent::IpcResponse(json)) => {
                    let script = format!("window.__alexResolve({json})");
                    let _ = webview.evaluate_script(&script);
                }
                Event::UserEvent(UserEvent::Host(command)) => match command {
                    HostCommand::SetWindowTitle(title) => window.set_title(&title),
                    HostCommand::MinimizeWindow => window.set_minimized(true),
                    HostCommand::MaximizeWindow => window.set_maximized(true),
                    HostCommand::CloseWindow => *control_flow = ControlFlow::Exit,
                },
                Event::WindowEvent { event, .. } => match event {
                    WindowEvent::CloseRequested => *control_flow = ControlFlow::Exit,
                    WindowEvent::Focused(focused) => emit_event(
                        &webview,
                        "window.focusChanged",
                        serde_json::json!({ "focused": focused }),
                    ),
                    WindowEvent::Resized(size) => emit_event(
                        &webview,
                        "window.resized",
                        serde_json::json!({ "width": size.width, "height": size.height }),
                    ),
                    WindowEvent::Moved(position) => emit_event(
                        &webview,
                        "window.moved",
                        serde_json::json!({ "x": position.x, "y": position.y }),
                    ),
                    _ => {}
                },
                _ => {}
            }

            // 100 ms tick: drain dev commands while the event loop is otherwise idle.
            if last_poll.elapsed() >= DEV_POLL_INTERVAL {
                last_poll = Instant::now();
                while let Ok(command) = dev_rx.try_recv() {
                    if handle_dev_command(&webview, &router, command) {
                        *control_flow = ControlFlow::Exit;
                    }
                }
            }
        })
    }

    /// Returns `true` when the event loop should exit. The only
    /// command that does this today is `ManifestChanged` — the
    /// rest mutate the running shell in place.
    fn handle_dev_command(webview: &wry::WebView, router: &ApiRouter, command: DevCommand) -> bool {
        match command {
            DevCommand::ReloadFrontend => {
                let _ = webview.evaluate_script("location.reload()");
                eprintln!("alex dev: reloaded frontend");
                false
            }
            DevCommand::RestartRuntime => match router.restart_runtime(RUNTIME_RESTART_TIMEOUT) {
                Some(Ok(())) => {
                    eprintln!("alex dev: restarted backend runtime");
                    false
                }
                Some(Err(error)) => {
                    eprintln!("alex dev: backend restart failed: {error}");
                    false
                }
                None => {
                    eprintln!("alex dev: no runtime to restart");
                    false
                }
            },
            DevCommand::ManifestChanged => {
                eprintln!(
                    "alex dev: manifest.json changed — restart `alex dev` to pick up \
                     new permissions, backend, or runtime mode"
                );
                true
            }
        }
    }

    fn spawn_watcher(
        frontend_dir: PathBuf,
        backend_dir: Option<PathBuf>,
        manifest_path: PathBuf,
        matcher: Option<Gitignore>,
        dev_tx: mpsc::SyncSender<DevCommand>,
    ) {
        thread::Builder::new()
            .name("alex-dev-watcher".into())
            .spawn(move || run_watcher(frontend_dir, backend_dir, manifest_path, matcher, dev_tx))
            .expect("dev watcher thread should start");
    }

    fn run_watcher(
        frontend_dir: PathBuf,
        backend_dir: Option<PathBuf>,
        manifest_path: PathBuf,
        matcher: Option<Gitignore>,
        dev_tx: mpsc::SyncSender<DevCommand>,
    ) {
        let (notify_tx, notify_rx) = mpsc::channel();
        let mut watcher = match notify::recommended_watcher(notify_tx) {
            Ok(watcher) => watcher,
            Err(error) => {
                eprintln!("alex dev: failed to create file watcher: {error}");
                return;
            }
        };
        if let Err(error) = watcher.watch(&frontend_dir, RecursiveMode::Recursive) {
            eprintln!("alex dev: cannot watch {}: {error}", frontend_dir.display());
            return;
        }
        if let Some(backend) = &backend_dir
            && let Err(error) = watcher.watch(backend, RecursiveMode::Recursive)
        {
            eprintln!("alex dev: cannot watch {}: {error}", backend.display());
            return;
        }
        // Watch the manifest itself. We use NonRecursive because
        // manifest.json is a single file at a known path; the
        // notify crate accepts that for any path.
        if let Err(error) = watcher.watch(&manifest_path, RecursiveMode::NonRecursive) {
            eprintln!(
                "alex dev: cannot watch {}: {error}",
                manifest_path.display()
            );
        }
        // Hold the watcher until the channel closes (i.e. until the
        // process exits and the receiver is dropped). Dropping the watcher
        // releases the OS file handles.
        let _watcher = watcher;
        let package_root = frontend_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or(frontend_dir.clone());
        for event in notify_rx {
            let Ok(event) = event else { continue };
            // Ignore read/access notifications — editors often poll metadata.
            if matches!(event.kind, EventKind::Access(_) | EventKind::Other) {
                continue;
            }
            for path in &event.paths {
                let matcher_ref = matcher.as_ref();
                if is_ignored(&matcher_ref, &package_root, path) {
                    continue;
                }
                if let Some(signal) =
                    classify_change(&frontend_dir, backend_dir.as_deref(), &manifest_path, path)
                {
                    // Capacity-1 channel: older signals are dropped, only
                    // the most recent command reaches the event loop.
                    let _ = dev_tx.try_send(signal);
                }
            }
        }
    }

    fn classify_change(
        frontend_dir: &Path,
        backend_dir: Option<&Path>,
        manifest_path: &Path,
        path: &Path,
    ) -> Option<DevCommand> {
        if path == manifest_path {
            return Some(DevCommand::ManifestChanged);
        }
        if path.starts_with(frontend_dir) {
            return Some(DevCommand::ReloadFrontend);
        }
        if let Some(backend) = backend_dir
            && path.starts_with(backend)
        {
            return Some(DevCommand::RestartRuntime);
        }
        None
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::path::PathBuf;

        fn p(s: &str) -> PathBuf {
            PathBuf::from(s)
        }

        #[test]
        fn classify_change_flags_manifest_path() {
            let manifest = p("/app/manifest.json");
            let frontend = p("/app/frontend");
            let backend = Some(p("/app/backend"));
            assert_eq!(
                classify_change(&frontend, backend.as_deref(), &manifest, &manifest),
                Some(DevCommand::ManifestChanged)
            );
        }

        #[test]
        fn classify_change_flags_frontend_paths() {
            let manifest = p("/app/manifest.json");
            let frontend = p("/app/frontend");
            let backend = Some(p("/app/backend"));
            assert_eq!(
                classify_change(
                    &frontend,
                    backend.as_deref(),
                    &manifest,
                    &p("/app/frontend/index.html"),
                ),
                Some(DevCommand::ReloadFrontend)
            );
            assert_eq!(
                classify_change(
                    &frontend,
                    backend.as_deref(),
                    &manifest,
                    &p("/app/frontend/app/main.js"),
                ),
                Some(DevCommand::ReloadFrontend)
            );
        }

        #[test]
        fn classify_change_flags_backend_paths() {
            let manifest = p("/app/manifest.json");
            let frontend = p("/app/frontend");
            let backend = Some(p("/app/backend"));
            assert_eq!(
                classify_change(
                    &frontend,
                    backend.as_deref(),
                    &manifest,
                    &p("/app/backend/index.js"),
                ),
                Some(DevCommand::RestartRuntime)
            );
        }

        #[test]
        fn classify_change_handles_no_backend() {
            // Frontend-only app (no backend declared) — the
            // backend slot is None and any change outside the
            // frontend tree is a no-op.
            let manifest = p("/app/manifest.json");
            let frontend = p("/app/frontend");
            assert_eq!(
                classify_change(&frontend, None, &manifest, &p("/app/anything/else")),
                None
            );
        }

        #[test]
        fn classify_change_ignores_unrelated_paths() {
            let manifest = p("/app/manifest.json");
            let frontend = p("/app/frontend");
            let backend = Some(p("/app/backend"));
            assert_eq!(
                classify_change(
                    &frontend,
                    backend.as_deref(),
                    &manifest,
                    &p("/elsewhere/something.txt"),
                ),
                None
            );
        }

        #[test]
        fn manifest_path_takes_precedence_over_overlapping_dirs() {
            // Defensive: if the user happens to also place
            // manifest.json under their frontend dir (a mistake
            // we still want to handle), the manifest signal must
            // win so the user gets the "restart required"
            // message instead of a silent reload that drops the
            // new permission set.
            let manifest = p("/app/frontend/manifest.json");
            let frontend = p("/app/frontend");
            let backend = None;
            assert_eq!(
                classify_change(&frontend, backend, &manifest, &manifest),
                Some(DevCommand::ManifestChanged)
            );
        }
    }
}

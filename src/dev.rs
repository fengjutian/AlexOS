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
    #[derive(Debug, Clone, Copy)]
    enum DevCommand {
        /// Frontend file changed — reload the webview page.
        ReloadFrontend,
        /// Backend file changed — restart the Node runtime.
        RestartRuntime,
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

        // Capacity-1 channel: a burst of file events collapses into the most
        // recent signal, so we never queue redundant reloads.
        let (dev_tx, dev_rx) = mpsc::sync_channel::<DevCommand>(1);

        let matcher = load_alexignore(&canonical_root);
        spawn_watcher(frontend_dir.clone(), backend_dir.clone(), matcher, dev_tx);

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
            .with_navigation_handler(|url| url.starts_with("alex://app/"))
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
                    handle_dev_command(&webview, &router, command);
                }
            }
        })
    }

    fn handle_dev_command(webview: &wry::WebView, router: &ApiRouter, command: DevCommand) {
        match command {
            DevCommand::ReloadFrontend => {
                let _ = webview.evaluate_script("location.reload()");
                eprintln!("alex dev: reloaded frontend");
            }
            DevCommand::RestartRuntime => match router.restart_runtime(RUNTIME_RESTART_TIMEOUT) {
                Some(Ok(())) => eprintln!("alex dev: restarted backend runtime"),
                Some(Err(error)) => eprintln!("alex dev: backend restart failed: {error}"),
                None => eprintln!("alex dev: no runtime to restart"),
            },
        }
    }

    fn spawn_watcher(
        frontend_dir: PathBuf,
        backend_dir: Option<PathBuf>,
        matcher: Option<Gitignore>,
        dev_tx: mpsc::SyncSender<DevCommand>,
    ) {
        thread::Builder::new()
            .name("alex-dev-watcher".into())
            .spawn(move || run_watcher(frontend_dir, backend_dir, matcher, dev_tx))
            .expect("dev watcher thread should start");
    }

    fn run_watcher(
        frontend_dir: PathBuf,
        backend_dir: Option<PathBuf>,
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
                if let Some(signal) = classify_change(&frontend_dir, backend_dir.as_deref(), path) {
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
        path: &Path,
    ) -> Option<DevCommand> {
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
}

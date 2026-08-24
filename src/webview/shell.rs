use std::path::Path;

use crate::{AlexError, manifest::AppManifest};

#[cfg(windows)]
pub fn run(
    package_root: &Path,
    manifest: AppManifest,
    system_install_root: Option<&Path>,
    system_trust_root: Option<&Path>,
) -> Result<(), AlexError> {
    windows::run(
        package_root,
        manifest,
        system_install_root,
        system_trust_root,
    )
    .map_err(|error| AlexError::Validation(format!("shell failed: {error}")))
}

#[cfg(not(windows))]
pub fn run(
    _package_root: &Path,
    _manifest: AppManifest,
    _system_install_root: Option<&Path>,
    _system_trust_root: Option<&Path>,
) -> Result<(), AlexError> {
    Err(AlexError::Validation(
        "the 0.1 shell currently supports Windows only".into(),
    ))
}

#[cfg(windows)]
pub mod windows {
    use std::{
        collections::HashMap,
        path::{Path, PathBuf},
        sync::Arc,
        time::Duration,
    };

    use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
    use muda::{
        CheckMenuItem, ContextMenu, Menu, MenuItem as NativeMenuItem, PredefinedMenuItem, Submenu,
    };
    use tao::platform::windows::WindowExtWindows;
    use tao::{
        dpi::{PhysicalPosition, PhysicalSize},
        event::{Event, WindowEvent},
        event_loop::{ControlFlow, EventLoopBuilder},
        window::WindowBuilder,
    };
    use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};
    use wry::{
        NewWindowResponse, WebViewBuilder,
        http::{Response as HttpResponse, header::CONTENT_TYPE},
    };

    use crate::{
        api::ApiRouter,
        authorization::PermissionStore,
        manifest::AppManifest,
        menu_tray::{MenuItem, MenuTemplate},
        native::{HostCommand, NativeError, NativeHost},
        runtime::RuntimeHandle,
    };

    pub const BRIDGE: &str = r#"
      (() => {
        const pending = new Map();
        const listeners = new Map();
        let sequence = 0;
        window.__alexResolve = (response) => {
          const item = pending.get(response.id);
          if (!item) return;
          pending.delete(response.id);
          response.error ? item.reject(response.error) : item.resolve(response.result);
        };
        window.__alexEmit = (event, data) => {
          for (const listener of listeners.get(event) ?? []) {
            try { listener(data); } catch (error) { queueMicrotask(() => { throw error; }); }
          }
        };
        window.__alexDeliver = (envelope) => {
          window.__alexEmit(envelope.event, envelope);
        };
        window.alex = Object.freeze({
          invoke(method, params = {}, options = {}) {
            const id = `web-${Date.now()}-${++sequence}`;
            const timeoutMs = options.timeoutMs ?? 30000;
            const deadlineMs = Date.now() + timeoutMs;
            const request = { protocol: 1, id, source: __ALEX_PACKAGE_ID__, method, params, deadlineMs };
            return new Promise((resolve, reject) => {
              const cancelRuntime = () => {
                if (method !== "runtime.invoke") return;
                window.ipc.postMessage(JSON.stringify({
                  protocol: 1,
                  id: `cancel-${id}`,
                  source: __ALEX_PACKAGE_ID__,
                  method: "runtime.cancel",
                  params: { requestId: id }
                }));
              };
              const timer = setTimeout(() => {
                pending.delete(id);
                cancelRuntime();
                reject({ code: "DEADLINE_EXCEEDED", message: "Alex API request timed out" });
              }, timeoutMs);
              const abortHandler = () => {
                clearTimeout(timer);
                pending.delete(id);
                cancelRuntime();
                reject({ code: "ABORTED", message: "Alex API request was aborted" });
              };
              options.signal?.addEventListener("abort", abortHandler, { once: true });
              pending.set(id, {
                resolve: (value) => { clearTimeout(timer); options.signal?.removeEventListener("abort", abortHandler); resolve(value); },
                reject: (error) => { clearTimeout(timer); options.signal?.removeEventListener("abort", abortHandler); reject(error); }
              });
              window.ipc.postMessage(JSON.stringify(request));
            });
          },
          on(event, listener) {
            if (typeof event !== "string" || typeof listener !== "function") {
              throw new TypeError("alex.on requires an event name and listener");
            }
            const eventListeners = listeners.get(event) ?? new Set();
            eventListeners.add(listener);
            listeners.set(event, eventListeners);
            return () => {
              eventListeners.delete(listener);
              if (eventListeners.size === 0) listeners.delete(event);
            };
          }
        });
      })();
    "#;

    #[derive(Debug)]
    pub enum UserEvent {
        IpcResponse(Option<u64>, String),
        Host(HostCommand),
    }

    #[derive(Clone)]
    pub struct WindowHost {
        pub proxy: tao::event_loop::EventLoopProxy<UserEvent>,
        pub secondary_windows: bool,
    }

    impl NativeHost for WindowHost {
        fn execute(&self, command: HostCommand) -> Result<(), NativeError> {
            if !self.secondary_windows
                && matches!(
                    &command,
                    HostCommand::CreateWindow(_)
                        | HostCommand::SetWindowBounds(_, _)
                        | HostCommand::SetWindowFullscreen(_, _)
                        | HostCommand::DestroyWindow(_)
                        | HostCommand::SetApplicationMenu(_)
                        | HostCommand::SetContextMenu(_)
                        | HostCommand::CreateTray(_, _, _)
                        | HostCommand::DestroyTray(_)
                        | HostCommand::RegisterShortcut(_)
                        | HostCommand::UnregisterShortcut(_)
                )
            {
                return Err(NativeError::Unsupported);
            }
            self.proxy
                .send_event(UserEvent::Host(command))
                .map_err(|_| NativeError::Failed("window event loop is closed".into()))
        }

        fn supports_secondary_windows(&self) -> bool {
            self.secondary_windows
        }
    }

    pub fn run(
        package_root: &Path,
        manifest: AppManifest,
        system_install_root: Option<&Path>,
        system_trust_root: Option<&Path>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
        let proxy = event_loop.create_proxy();
        let window = WindowBuilder::new()
            .with_title(&manifest.name)
            .build(&event_loop)?;

        let permissions = PermissionStore::for_app(&manifest.id)?;
        // Plugin manifests declare their `system.*` permissions up
        // front, and the user opted in by installing the plugin. The
        // IPC handler runs in a non-STA thread (rfd's modal dialogs
        // need STA + a message pump to render), so a fresh
        // `PermissionDecision::Prompt` would block forever without
        // ever showing a dialog. Pre-grant `system.*` for plugins
        // and let non-system permissions keep their normal
        // prompt-on-first-use flow.
        if matches!(manifest.kind, crate::core::manifest::PackageKind::Plugin) {
            for permission in &manifest.permissions {
                let name = permission.name();
                if name.starts_with("system.")
                    && matches!(
                        permissions.decision(name),
                        crate::api::authorization::PermissionDecision::Prompt
                    )
                {
                    let _ = permissions
                        .set(name, crate::api::authorization::PermissionDecision::Granted);
                }
            }
        }
        let mut router = ApiRouter::new(package_root.to_path_buf(), manifest.clone())
            .with_permission_store(permissions)
            .with_native_host(Arc::new(WindowHost {
                proxy: proxy.clone(),
                secondary_windows: true,
            }));
        if let Some(install_root) = system_install_root {
            router = router.with_system_install_root(install_root.to_path_buf());
        }
        if let Some(trust_root) = system_trust_root {
            router = router.with_system_trust_root(trust_root.to_path_buf());
        }
        // Service-mode backends expose an `alex://app/api/*` reverse
        // proxy. We need the host-allocated endpoint in scope before
        // building the WebView, so we resolve it from the runtime
        // status right after start.
        let service_endpoint: Option<crate::runtime::ServiceEndpoint> =
            if let Some(backend) = &manifest.backend {
                if matches!(backend.mode, crate::manifest::BackendMode::Service) {
                    let spec = crate::runtime::RuntimeSpec {
                        app_id: manifest.id.clone(),
                        package_root: package_root.to_path_buf(),
                        backend: backend.clone(),
                        data_dir: None,
                        cache_dir: None,
                    };
                    let handle = RuntimeHandle::start_with_spec(spec)?;
                    let status = handle.status(Duration::from_secs(20))?;
                    if !matches!(status.state, crate::runtime::RuntimeState::Ready) {
                        return Err(format!(
                        "service backend {} failed to reach Ready within handshake window: {:?}",
                        manifest.id, status.last_error
                    )
                        .into());
                    }
                    router = router.with_runtime(handle);
                    match (status.port, status.token) {
                        (Some(port), Some(token)) => {
                            Some(crate::runtime::ServiceEndpoint { port, token })
                        }
                        _ => None,
                    }
                } else {
                    router = router.with_runtime(RuntimeHandle::start(package_root, backend)?);
                    None
                }
            } else {
                None
            };
        let router = Arc::new(router);
        let ipc_router = Arc::clone(&router);
        let root = package_root.to_path_buf();
        let frontend = manifest.frontend.entry.clone();
        let package_id = serde_json::to_string(&manifest.id)?;
        let init_script = BRIDGE.replace("__ALEX_PACKAGE_ID__", &package_id)
            + &crate::permission_shim::shim_source(&manifest.permissions);
        let endpoint_for_handler = service_endpoint.clone();
        let app_id_for_handler = manifest.id.clone();
        let drop_router = Arc::clone(&router);

        let webview = WebViewBuilder::new()
            .with_initialization_script(&init_script)
            .with_devtools(cfg!(debug_assertions) && std::env::var_os("ALEX_DEVTOOLS").is_some())
            .with_incognito(true)
            .with_clipboard(false)
            .with_navigation_handler(|url| crate::is_internal_webview_url(&url, "app"))
            .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
            .with_download_started_handler(|_, _| false)
            .with_drag_drop_handler(move |event| match event {
                wry::DragDropEvent::Drop { paths, position } => {
                    drop_router.deliver_file_drop(paths, position.0, position.1)
                }
                _ => false,
            })
            .with_ipc_handler(move |request| {
                let router = Arc::clone(&ipc_router);
                let proxy = proxy.clone();
                let body = request.body().clone();
                std::thread::spawn(move || {
                    let response = router.dispatch_json(&body);
                    if let Ok(json) = serde_json::to_string(&response) {
                        let _ = proxy.send_event(UserEvent::IpcResponse(None, json));
                    }
                });
            })
            .with_custom_protocol("alex".into(), move |_id, request| {
                let path = request.uri().path();
                if path.starts_with("/api/") {
                    if let Some(endpoint) = &endpoint_for_handler {
                        return crate::proxy::proxy_to_service(
                            endpoint,
                            &app_id_for_handler,
                            path,
                            &request,
                        );
                    }
                    return crate::proxy::service_unavailable_response();
                }
                asset_response(&root, &frontend, path)
            })
            .with_url("alex://app/")
            .build(&window)?;

        let child_proxy = event_loop.create_proxy();
        let child_root = package_root.to_path_buf();
        let child_frontend = manifest.frontend.entry.clone();
        let child_init_script = init_script.clone();
        let child_router = Arc::clone(&router);
        let child_service_endpoint = service_endpoint.clone();
        let child_app_id = manifest.id.clone();
        let mut child_windows: HashMap<u64, tao::window::Window> = HashMap::new();
        let mut child_webviews: HashMap<u64, wry::WebView> = HashMap::new();
        let mut native_child_ids: HashMap<tao::window::WindowId, u64> = HashMap::new();
        let mut application_menu: Option<Menu> = None;
        let mut context_menu: Option<Menu> = None;
        let mut tray_icons: HashMap<String, TrayIcon> = HashMap::new();
        let hotkey_manager = GlobalHotKeyManager::new()?;
        let mut hotkeys: HashMap<u32, (HotKey, String)> = HashMap::new();

        event_loop.run(move |event, event_loop_target, control_flow| {
            *control_flow =
                ControlFlow::WaitUntil(std::time::Instant::now() + Duration::from_millis(50));
            match event {
                Event::UserEvent(UserEvent::IpcResponse(target, json)) => {
                    let script = format!("window.__alexResolve({json})");
                    if let Some(id) = target {
                        if let Some(child) = child_webviews.get(&id) {
                            let _ = child.evaluate_script(&script);
                        }
                    } else {
                        let _ = webview.evaluate_script(&script);
                    }
                }
                Event::UserEvent(UserEvent::Host(command)) => match command {
                    HostCommand::SetWindowTitle(title) => window.set_title(&title),
                    HostCommand::MinimizeWindow => window.set_minimized(true),
                    HostCommand::MaximizeWindow => window.set_maximized(true),
                    HostCommand::CloseWindow => *control_flow = ControlFlow::Exit,
                    HostCommand::CreateWindow(info) => {
                        let mut builder = WindowBuilder::new()
                            .with_title(&info.title)
                            .with_inner_size(PhysicalSize::new(info.width, info.height));
                        if let (Some(x), Some(y)) = (info.x, info.y) {
                            builder = builder.with_position(PhysicalPosition::new(x, y));
                        }
                        match builder.build(event_loop_target) {
                            Ok(child_window) => {
                                let native_id = child_window.id();
                                let ipc_router = Arc::clone(&child_router);
                                let ipc_proxy = child_proxy.clone();
                                let child_id = info.id.raw();
                                let asset_root = child_root.clone();
                                let frontend = child_frontend.clone();
                                let service_endpoint = child_service_endpoint.clone();
                                let service_app_id = child_app_id.clone();
                                let drop_router = Arc::clone(&child_router);
                                let url = if info.url.starts_with("alex://app/") {
                                    info.url.clone()
                                } else {
                                    format!("alex://app/{}", info.url.trim_start_matches('/'))
                                };
                                let built = WebViewBuilder::new()
                                    .with_initialization_script(child_init_script.clone())
                                    .with_incognito(true)
                                    .with_clipboard(false)
                                    .with_navigation_handler(|url| {
                                        crate::is_internal_webview_url(&url, "app")
                                    })
                                    .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
                                    .with_download_started_handler(|_, _| false)
                                    .with_drag_drop_handler(move |event| match event {
                                        wry::DragDropEvent::Drop { paths, position } => drop_router
                                            .deliver_file_drop(paths, position.0, position.1),
                                        _ => false,
                                    })
                                    .with_ipc_handler(move |request| {
                                        let router = Arc::clone(&ipc_router);
                                        let proxy = ipc_proxy.clone();
                                        let body = request.body().clone();
                                        std::thread::spawn(move || {
                                            let response = router.dispatch_json(&body);
                                            if let Ok(json) = serde_json::to_string(&response) {
                                                let _ = proxy.send_event(UserEvent::IpcResponse(
                                                    Some(child_id),
                                                    json,
                                                ));
                                            }
                                        });
                                    })
                                    .with_custom_protocol("alex".into(), move |_id, request| {
                                        let path = request.uri().path();
                                        if path.starts_with("/api/") {
                                            if let Some(endpoint) = &service_endpoint {
                                                return crate::proxy::proxy_to_service(
                                                    endpoint,
                                                    &service_app_id,
                                                    path,
                                                    &request,
                                                );
                                            }
                                            return crate::proxy::service_unavailable_response();
                                        }
                                        asset_response(&asset_root, &frontend, path)
                                    })
                                    .with_url(&url)
                                    .build(&child_window);
                                match built {
                                    Ok(child_webview) => {
                                        native_child_ids.insert(native_id, info.id.raw());
                                        child_windows.insert(info.id.raw(), child_window);
                                        child_webviews.insert(info.id.raw(), child_webview);
                                    }
                                    Err(error) => {
                                        eprintln!("alex window: failed to create webview: {error}")
                                    }
                                }
                            }
                            Err(error) => {
                                eprintln!("alex window: failed to create window: {error}")
                            }
                        }
                    }
                    HostCommand::SetWindowBounds(id, bounds) => {
                        if let Some(child) = child_windows.get(&id) {
                            if let (Some(x), Some(y)) = (bounds.x, bounds.y) {
                                child.set_outer_position(PhysicalPosition::new(x, y));
                            }
                            if let (Some(width), Some(height)) = (bounds.width, bounds.height) {
                                child.set_inner_size(PhysicalSize::new(width, height));
                            }
                        }
                    }
                    HostCommand::SetWindowFullscreen(id, fullscreen) => {
                        if let Some(child) = child_windows.get(&id) {
                            child.set_fullscreen(
                                fullscreen.then(|| tao::window::Fullscreen::Borderless(None)),
                            );
                        }
                    }
                    HostCommand::DestroyWindow(id) => {
                        child_webviews.remove(&id);
                        if let Some(child) = child_windows.remove(&id) {
                            native_child_ids.remove(&child.id());
                        }
                    }
                    HostCommand::SetApplicationMenu(template) => match build_menu(&template) {
                        Ok(menu) => {
                            if let Some(previous) = application_menu.take() {
                                let _ = unsafe { previous.remove_for_hwnd(window.hwnd() as isize) };
                            }
                            match unsafe { menu.init_for_hwnd(window.hwnd() as isize) } {
                                Ok(()) => application_menu = Some(menu),
                                Err(error) => {
                                    eprintln!("alex menu: failed to attach menu: {error}")
                                }
                            }
                        }
                        Err(error) => eprintln!("alex menu: invalid native menu: {error}"),
                    },
                    HostCommand::SetContextMenu(template) => match build_menu(&template) {
                        Ok(menu) => context_menu = Some(menu),
                        Err(error) => eprintln!("alex menu: invalid context menu: {error}"),
                    },
                    HostCommand::CreateTray(id, spec, root) => {
                        let icon_path = root.join(&spec.icon);
                        match tray_icon::Icon::from_path(&icon_path, None) {
                            Ok(icon) => {
                                let mut builder =
                                    TrayIconBuilder::new().with_id(id.clone()).with_icon(icon);
                                if let Some(tooltip) = spec.tooltip {
                                    builder = builder.with_tooltip(tooltip);
                                }
                                if let Some(template) = spec.menu {
                                    if let Ok(menu) = build_menu(&template) {
                                        builder = builder.with_menu(Box::new(menu));
                                    }
                                }
                                match builder.build() {
                                    Ok(tray) => {
                                        tray_icons.insert(id, tray);
                                    }
                                    Err(error) => {
                                        eprintln!("alex tray: failed to create icon: {error}")
                                    }
                                }
                            }
                            Err(error) => eprintln!(
                                "alex tray: failed to load {}: {error}",
                                icon_path.display()
                            ),
                        }
                    }
                    HostCommand::DestroyTray(id) => {
                        tray_icons.remove(&id);
                    }
                    HostCommand::RegisterShortcut(accelerator) => match accelerator
                        .parse::<HotKey>()
                    {
                        Ok(hotkey) => match hotkey_manager.register(hotkey) {
                            Ok(()) => {
                                hotkeys.insert(hotkey.id(), (hotkey, accelerator));
                            }
                            Err(error) => eprintln!("alex shortcut: registration failed: {error}"),
                        },
                        Err(error) => eprintln!("alex shortcut: invalid accelerator: {error}"),
                    },
                    HostCommand::UnregisterShortcut(accelerator) => {
                        if let Some((id, (hotkey, _))) = hotkeys
                            .iter()
                            .find(|(_, (_, value))| value == &accelerator)
                            .map(|(id, value)| (*id, value.clone()))
                        {
                            let _ = hotkey_manager.unregister(hotkey);
                            hotkeys.remove(&id);
                        }
                    }
                },
                Event::WindowEvent {
                    window_id, event, ..
                } => match event {
                    WindowEvent::CloseRequested => {
                        if window_id == window.id() {
                            *control_flow = ControlFlow::Exit;
                        } else if let Some(id) = native_child_ids.remove(&window_id) {
                            child_webviews.remove(&id);
                            child_windows.remove(&id);
                            router.native_window_closed(id);
                        }
                    }
                    WindowEvent::Focused(focused) => {
                        let target = native_child_ids
                            .get(&window_id)
                            .and_then(|id| child_webviews.get(id));
                        emit_event(
                            target.unwrap_or(&webview),
                            "window.focusChanged",
                            serde_json::json!({ "focused": focused }),
                        )
                    }
                    WindowEvent::Resized(size) => {
                        let target = native_child_ids
                            .get(&window_id)
                            .and_then(|id| child_webviews.get(id));
                        emit_event(
                            target.unwrap_or(&webview),
                            "window.resized",
                            serde_json::json!({ "width": size.width, "height": size.height }),
                        )
                    }
                    WindowEvent::Moved(position) => {
                        let target = native_child_ids
                            .get(&window_id)
                            .and_then(|id| child_webviews.get(id));
                        emit_event(
                            target.unwrap_or(&webview),
                            "window.moved",
                            serde_json::json!({ "x": position.x, "y": position.y }),
                        )
                    }
                    WindowEvent::MouseInput {
                        state: tao::event::ElementState::Pressed,
                        button: tao::event::MouseButton::Right,
                        ..
                    } if window_id == window.id() => {
                        if let Some(menu) = &context_menu {
                            unsafe {
                                menu.show_context_menu_for_hwnd(window.hwnd() as isize, None);
                            }
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
            while let Ok(event) = muda::MenuEvent::receiver().try_recv() {
                router
                    .event_bus()
                    .deliver("menu.clicked", &serde_json::json!({ "id": event.id().0 }));
            }
            while let Ok(event) = TrayIconEvent::receiver().try_recv() {
                router
                    .event_bus()
                    .deliver("tray.clicked", &serde_json::json!({ "id": event.id().0 }));
            }
            while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
                if event.state == HotKeyState::Pressed {
                    if let Some((_, accelerator)) = hotkeys.get(&event.id) {
                        router.event_bus().deliver(
                            "shortcut.triggered",
                            &serde_json::json!({ "accelerator": accelerator }),
                        );
                    }
                }
            }
            for (event, delivered) in router.event_bus().drain_pending() {
                emit_subscribed(
                    &webview,
                    &event,
                    &delivered.subscription_id,
                    delivered.sequence,
                    &delivered.payload,
                );
                for child in child_webviews.values() {
                    emit_subscribed(
                        child,
                        &event,
                        &delivered.subscription_id,
                        delivered.sequence,
                        &delivered.payload,
                    );
                }
            }
        })
    }

    fn build_menu(template: &MenuTemplate) -> Result<Menu, Box<dyn std::error::Error>> {
        let menu = Menu::new();
        append_menu_items(&menu, &template.items)?;
        Ok(menu)
    }

    fn append_menu_items(
        parent: &dyn MenuAppender,
        items: &[MenuItem],
    ) -> Result<(), Box<dyn std::error::Error>> {
        for item in items {
            match item {
                MenuItem::Normal {
                    id,
                    label,
                    accelerator,
                    enabled,
                } => {
                    let accelerator = accelerator.as_deref().map(str::parse).transpose()?;
                    parent.append_item(&NativeMenuItem::with_id(
                        id,
                        label,
                        enabled.unwrap_or(true),
                        accelerator,
                    ))?;
                }
                MenuItem::Checkbox {
                    id,
                    label,
                    checked,
                    accelerator,
                } => {
                    let accelerator = accelerator.as_deref().map(str::parse).transpose()?;
                    parent.append_item(&CheckMenuItem::with_id(
                        id,
                        label,
                        true,
                        checked.unwrap_or(false),
                        accelerator,
                    ))?;
                }
                MenuItem::Separator => parent.append_item(&PredefinedMenuItem::separator())?,
                MenuItem::Submenu { id, label, items } => {
                    let submenu = Submenu::with_id(id, label, true);
                    append_menu_items(&submenu, items)?;
                    parent.append_item(&submenu)?;
                }
            }
        }
        Ok(())
    }

    trait MenuAppender {
        fn append_item(&self, item: &dyn muda::IsMenuItem) -> muda::Result<()>;
    }
    impl MenuAppender for Menu {
        fn append_item(&self, item: &dyn muda::IsMenuItem) -> muda::Result<()> {
            self.append(item)
        }
    }
    impl MenuAppender for Submenu {
        fn append_item(&self, item: &dyn muda::IsMenuItem) -> muda::Result<()> {
            self.append(item)
        }
    }

    pub fn asset_response(
        root: &Path,
        frontend: &str,
        uri_path: &str,
    ) -> HttpResponse<std::borrow::Cow<'static, [u8]>> {
        // Browsers (and WebView2) auto-request /favicon.ico in
        // parallel with the page, before the <link rel="icon">
        // link is parsed. Silently answer 204 so the resource
        // panel stays clean for apps that ship no favicon.
        if uri_path == "/favicon.ico" {
            return response(204, "text/plain", Vec::new());
        }
        // The frontend directory declared by the manifest entry is
        // the document root for the WebView. Stripping the
        // frontend-prefix from the manifest entry and using its
        // parent as the asset root means a URL like
        // `/src/main.tsx` resolves to `<root>/frontend/src/main.tsx`
        // — the same shape Vite's `base: "./"` build emits, and the
        // shape the React scaffold uses.
        let entry = Path::new(frontend);
        let asset_root = root.join(entry.parent().unwrap_or(Path::new("")));
        let entry_basename = entry
            .file_name()
            .map(Path::new)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("index.html"));
        let relative = if uri_path == "/" {
            entry_basename
        } else {
            PathBuf::from(uri_path.trim_start_matches('/'))
        };
        let candidate = asset_root.join(relative);
        // Distinguish "file does not exist" (404) from "path escapes
        // the package root" (403). A non-existent file fails
        // `canonicalize`, so it must not be reported as Forbidden.
        if !candidate.exists() {
            return response(404, "text/plain", b"Not found".to_vec());
        }
        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let Some(path) = candidate.canonicalize().ok() else {
            return response(404, "text/plain", b"Not found".to_vec());
        };
        if !path.starts_with(&canonical_root) {
            return response(403, "text/plain", b"Forbidden".to_vec());
        }
        match std::fs::read(&path) {
            Ok(body) => response(200, content_type(&path), body),
            Err(_) => response(404, "text/plain", b"Not found".to_vec()),
        }
    }

    pub fn response(
        status: u16,
        content_type: &str,
        body: Vec<u8>,
    ) -> HttpResponse<std::borrow::Cow<'static, [u8]>> {
        HttpResponse::builder()
            .status(status)
            .header(CONTENT_TYPE, content_type)
            .header("X-Content-Type-Options", "nosniff")
            // WebView2 rewrites the custom protocol to http://alex.<authority>
            // before the navigation callback sees it (see lib.rs), so the
            // CSP must allow both the native scheme and the rewritten
            // http://alex.app/ origin. `connect-src` needs them both for
            // `fetch('alex://app/api/...')` to clear the policy check.
            .header("Content-Security-Policy", "default-src 'self' alex: http://alex.app; script-src 'self' alex: http://alex.app; style-src 'self' alex: http://alex.app; img-src 'self' data:; connect-src 'self' alex: http://alex.app; object-src 'none'; base-uri 'none'; frame-src 'none'; form-action 'none'")
            .body(body.into())
            .expect("static response is valid")
    }

    pub fn content_type(path: &Path) -> &'static str {
        match path.extension().and_then(|value| value.to_str()) {
            Some("html") => "text/html; charset=utf-8",
            Some("js" | "mjs" | "ts" | "tsx" | "jsx") => "text/javascript; charset=utf-8",
            Some("css") => "text/css; charset=utf-8",
            Some("json" | "map") => "application/json; charset=utf-8",
            Some("svg") => "image/svg+xml",
            Some("png") => "image/png",
            Some("jpg" | "jpeg") => "image/jpeg",
            Some("gif") => "image/gif",
            Some("webp") => "image/webp",
            Some("ico") => "image/x-icon",
            Some("woff") => "font/woff",
            Some("woff2") => "font/woff2",
            Some("wasm") => "application/wasm",
            Some("txt") => "text/plain; charset=utf-8",
            _ => "application/octet-stream",
        }
    }

    pub fn emit_event(webview: &wry::WebView, event: &str, data: serde_json::Value) {
        let event = serde_json::to_string(event).expect("event name is valid JSON");
        let script = format!("window.__alexEmit?.({event},{data})");
        let _ = webview.evaluate_script(&script);
    }

    /// Forward a delivered bus event to the WebView. The
    /// page's `__alexDeliver` shim dispatches it to whichever
    /// subscriber is listening for the matching event name.
    /// The script swallows any host-side error so a dead
    /// WebView does not bubble back into the runtime manager.
    pub fn emit_subscribed(
        webview: &wry::WebView,
        event: &str,
        subscription_id: &str,
        sequence: u64,
        payload: &serde_json::Value,
    ) {
        let envelope = serde_json::json!({
            "kind": "event",
            "event": event,
            "subscriptionId": subscription_id,
            "sequence": sequence,
            "payload": payload,
        });
        let envelope_str = serde_json::to_string(&envelope).expect("envelope is valid JSON");
        let script = format!("window.__alexDeliver?.({envelope_str})");
        let _ = webview.evaluate_script(&script);
    }
}

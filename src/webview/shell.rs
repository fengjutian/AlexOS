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
        path::{Path, PathBuf},
        sync::Arc,
        time::Duration,
    };

    use tao::{
        event::{Event, WindowEvent},
        event_loop::{ControlFlow, EventLoopBuilder},
        window::WindowBuilder,
    };
    use wry::{
        NewWindowResponse, WebViewBuilder,
        http::{Response as HttpResponse, header::CONTENT_TYPE},
    };

    use crate::{
        api::ApiRouter,
        authorization::PermissionStore,
        manifest::AppManifest,
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
        // Subscriptions have their own listener bucket so
        // the page can subscribe to a streamed event without
        // re-registering a listener every time the host
        // delivers one.
        const subscriptionListeners = new Map();
        window.__alexDeliver = (envelope) => {
          const set = subscriptionListeners.get(envelope.event) ?? new Set();
          for (const listener of set) {
            try { listener(envelope); } catch (error) { queueMicrotask(() => { throw error; }); }
          }
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
        IpcResponse(String),
        Host(HostCommand),
    }

    #[derive(Clone)]
    pub struct WindowHost {
        pub proxy: tao::event_loop::EventLoopProxy<UserEvent>,
    }

    impl NativeHost for WindowHost {
        fn execute(&self, command: HostCommand) -> Result<(), NativeError> {
            self.proxy
                .send_event(UserEvent::Host(command))
                .map_err(|_| NativeError::Failed("window event loop is closed".into()))
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
        })
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
        let canonical_root = root
            .canonicalize()
            .unwrap_or_else(|_| root.to_path_buf());
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
            .header(
                "Content-Security-Policy",
                "default-src 'self' alex: http://alex.app; script-src 'self' alex: http://alex.app; style-src 'self' alex: http://alex.app; img-src 'self' data:; connect-src 'self' alex: http://alex.app; object-src 'none'; base-uri 'none'; frame-src 'none'; form-action 'none'",
            )
            .body(body.into())
            .expect("static response is valid")
    }

    pub fn content_type(path: &Path) -> &'static str {
        match path.extension().and_then(|value| value.to_str()) {
            Some("html") => "text/html; charset=utf-8",
            Some("js" | "mjs" | "ts" | "tsx" | "jsx") => {
                "text/javascript; charset=utf-8"
            }
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

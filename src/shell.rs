use std::path::Path;

use crate::{AlexError, manifest::AppManifest};

#[cfg(windows)]
pub fn run(
    package_root: &Path,
    manifest: AppManifest,
    system_install_root: Option<&Path>,
) -> Result<(), AlexError> {
    windows::run(package_root, manifest, system_install_root)
        .map_err(|error| AlexError::Validation(format!("shell failed: {error}")))
}

#[cfg(not(windows))]
pub fn run(
    _package_root: &Path,
    _manifest: AppManifest,
    _system_install_root: Option<&Path>,
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
    ) -> Result<(), Box<dyn std::error::Error>> {
        let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
        let proxy = event_loop.create_proxy();
        let window = WindowBuilder::new()
            .with_title(&manifest.name)
            .build(&event_loop)?;

        let permissions = PermissionStore::for_app(&manifest.id)?;
        let mut router = ApiRouter::new(package_root.to_path_buf(), manifest.clone())
            .with_permission_store(permissions)
            .with_native_host(Arc::new(WindowHost {
                proxy: proxy.clone(),
            }));
        if let Some(install_root) = system_install_root {
            router = router.with_system_install_root(install_root.to_path_buf());
        }
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
        let relative = if uri_path == "/" {
            PathBuf::from(frontend)
        } else {
            PathBuf::from(uri_path.trim_start_matches('/'))
        };
        let candidate = root.join(relative);
        let safe = candidate
            .canonicalize()
            .ok()
            .filter(|path| path.starts_with(root.canonicalize().unwrap_or_else(|_| root.into())));
        let Some(path) = safe else {
            return response(403, "text/plain", b"Forbidden".to_vec());
        };
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
            .header("Content-Security-Policy", "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'none'; object-src 'none'; base-uri 'none'; frame-src 'none'; form-action 'none'")
            .body(body.into())
            .expect("static response is valid")
    }

    pub fn content_type(path: &Path) -> &'static str {
        match path.extension().and_then(|value| value.to_str()) {
            Some("html") => "text/html; charset=utf-8",
            Some("js") => "text/javascript; charset=utf-8",
            Some("css") => "text/css; charset=utf-8",
            Some("json") => "application/json; charset=utf-8",
            Some("svg") => "image/svg+xml",
            Some("png") => "image/png",
            Some("jpg" | "jpeg") => "image/jpeg",
            _ => "application/octet-stream",
        }
    }

    pub fn emit_event(webview: &wry::WebView, event: &str, data: serde_json::Value) {
        let event = serde_json::to_string(event).expect("event name is valid JSON");
        let script = format!("window.__alexEmit?.({event},{data})");
        let _ = webview.evaluate_script(&script);
    }
}

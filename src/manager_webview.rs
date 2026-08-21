//! System WebView hosting the App Manager UI.
//!
//! This module is intentionally separate from `shell::windows::run`:
//! - The shell is for ordinary applications and uses `ApiRouter`
//! - The system manager uses `ManagerRouter` and a different identity
//!
//! The two paths share no runtime state. Even if a malicious app somehow
//! triggered a path that loaded the manager URL, the request would fail
//! the `source == SYSTEM_IDENTITY` check inside `ManagerRouter`.

use std::{path::Path, sync::Arc};

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
    AlexError,
    manager::{ManagerRouter, SYSTEM_IDENTITY},
    manifest::AppManifest,
};

const BRIDGE: &str = r#"
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
        const id = `mgr-${Date.now()}-${++sequence}`;
        const timeoutMs = options.timeoutMs ?? 30000;
        const request = { protocol: 1, id, source: __ALEX_PACKAGE_ID__, method, params };
        return new Promise((resolve, reject) => {
          const timer = setTimeout(() => {
            pending.delete(id);
            reject({ code: "DEADLINE_EXCEEDED", message: "manager request timed out" });
          }, timeoutMs);
          options.signal?.addEventListener("abort", () => {
            clearTimeout(timer);
            pending.delete(id);
            reject({ code: "ABORTED", message: "manager request was aborted" });
          }, { once: true });
          pending.set(id, {
            resolve: (value) => { clearTimeout(timer); resolve(value); },
            reject: (error) => { clearTimeout(timer); reject(error); }
          });
          window.ipc.postMessage(JSON.stringify(request));
        });
      }
    });
  })();
"#;

const PLACEHOLDER_HTML: &str = include_str!("manager_placeholder.html");
const PLACEHOLDER_CSS: &str = include_str!("manager_placeholder.css");

pub fn run(manager: Arc<ManagerRouter>) -> Result<(), AlexError> {
    let event_loop = EventLoopBuilder::<ManagerEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = WindowBuilder::new()
        .with_title("Alex OS App Manager")
        .build(&event_loop)
        .map_err(|e| AlexError::Validation(format!("manager window failed: {e}")))?;

    let router = manager;
    let ipc_router = Arc::clone(&router);
    let package_id = serde_json::to_string(SYSTEM_IDENTITY)
        .map_err(|e| AlexError::Validation(format!("encode system identity: {e}")))?;
    let init_script = BRIDGE.replace("__ALEX_PACKAGE_ID__", &package_id);

    let webview = WebViewBuilder::new()
        .with_initialization_script(init_script)
        .with_devtools(cfg!(debug_assertions) && std::env::var_os("ALEX_DEVTOOLS").is_some())
        .with_incognito(true)
        .with_clipboard(false)
        .with_navigation_handler(|url| crate::is_internal_webview_url(&url, "system"))
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
        .with_download_started_handler(|_, _| false)
        .with_ipc_handler(move |request| {
            let router = Arc::clone(&ipc_router);
            let proxy = proxy.clone();
            let body = request.body().clone();
            std::thread::spawn(move || {
                let response = router.dispatch_json(&body);
                if let Ok(json) = serde_json::to_string(&response) {
                    let _ = proxy.send_event(ManagerEvent::IpcResponse(json));
                }
            });
        })
        .with_custom_protocol("alex".into(), move |_id, request| {
            serve_system_asset(request.uri().path())
        })
        .with_url("alex://system/app-manager/")
        .build(&window)
        .map_err(|e| AlexError::Validation(format!("manager webview failed: {e}")))?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            *control_flow = ControlFlow::Exit;
            return;
        }
        if let Event::UserEvent(ManagerEvent::IpcResponse(json)) = event {
            let script = format!("window.__alexResolve({json})");
            let _ = webview.evaluate_script(&script);
        }
    });
}

#[derive(Debug)]
enum ManagerEvent {
    IpcResponse(String),
}

fn serve_system_asset(uri_path: &str) -> HttpResponse<std::borrow::Cow<'static, [u8]>> {
    if uri_path == "/" || uri_path.is_empty() {
        return response(
            200,
            "text/html; charset=utf-8",
            PLACEHOLDER_HTML.as_bytes().to_vec(),
        );
    }
    if uri_path == "/placeholder" {
        return response(
            200,
            "text/html; charset=utf-8",
            PLACEHOLDER_HTML.as_bytes().to_vec(),
        );
    }
    if uri_path == "/manager_placeholder.css" {
        return response(
            200,
            "text/css; charset=utf-8",
            PLACEHOLDER_CSS.as_bytes().to_vec(),
        );
    }
    response(404, "text/plain", b"Not found".to_vec())
}

fn response(
    status: u16,
    content_type_value: &str,
    body: Vec<u8>,
) -> HttpResponse<std::borrow::Cow<'static, [u8]>> {
    HttpResponse::builder()
        .status(status)
        .header(CONTENT_TYPE, content_type_value)
        .header("X-Content-Type-Options", "nosniff")
        .header(
            "Content-Security-Policy",
            // connect-src 'self' matches the main shell's policy
            // (`src/shell.rs`). The fallback system WebView does
            // not host a service backend, so the rule has no
            // practical effect here; keeping it consistent with the
            // main shell avoids a future surprise if a system
            // page ever starts calling out to a service.
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-src 'none'; form-action 'none'",
        )
        .body(body.into())
        .expect("static response is valid")
}

// Anchor AppManifest so the path stays used even before Phase 1.5.
#[allow(dead_code)]
fn _manifest_anchor(_: &Path, _: AppManifest) {}

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

// Built-in App Manager assets. The real UI is `manager_app.html` (a
// single-page list + detail view that drives the per-service
// `manager.*` IPC surface); the `manager_placeholder.html` page is
// kept as a backwards-compatible route at `/placeholder` for any
// saved bookmark from the Phase 1.4 placeholder era.
const APP_HTML: &str = include_str!("manager_app.html");
const APP_CSS: &str = include_str!("manager_app.css");
const APP_JS: &str = include_str!("manager_app.js");
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
            let fallback_proxy = proxy.clone();
            if crate::runtime::task_executor::ipc_executor()
                .submit(move || {
                    let response = router.dispatch_json(&body);
                    if let Ok(json) = serde_json::to_string(&response) {
                        let _ = proxy.send_event(ManagerEvent::IpcResponse(json));
                    }
                })
                .is_err()
            {
                let response =
                    crate::ipc::Response::error("unknown", "HOST_BUSY", "host IPC queue is full");
                if let Ok(json) = serde_json::to_string(&response) {
                    let _ = fallback_proxy.send_event(ManagerEvent::IpcResponse(json));
                }
            }
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
    // The manager WebView is loaded at `alex://system/app-manager/`,
    // so every asset path arrives prefixed with `/app-manager/`.
    // Strip that prefix so the rest of the routing uses the bare
    // asset paths declared in the manager HTML.
    let without_prefix = uri_path
        .strip_prefix("/app-manager/")
        .or_else(|| uri_path.strip_prefix("/app-manager"))
        .unwrap_or(uri_path);
    // Re-add a leading `/` so the bare routes below can use the
    // same shape whether the caller passed the prefix or not.
    // (`/app-manager/manager_app.js` and `/manager_app.js` both
    // arrive here as `manager_app.js` after stripping, so we
    // can't distinguish them at this point — but neither can
    // the rest of the routing, and the asset paths are the
    // same either way.)
    let stripped = if without_prefix.is_empty() || without_prefix.starts_with('/') {
        if without_prefix.is_empty() {
            "/".to_owned()
        } else {
            without_prefix.to_owned()
        }
    } else {
        format!("/{without_prefix}")
    };
    // Real App Manager UI (Phase 1.5) — the index, its stylesheet,
    // and its app script. Served with the same restricted CSP as the
    // main shell so a future system page that does call out to a
    // service still passes the policy.
    if stripped == "/" {
        return response(
            200,
            "text/html; charset=utf-8",
            APP_HTML.as_bytes().to_vec(),
        );
    }
    if stripped == "/manager_app.css" {
        return response(200, "text/css; charset=utf-8", APP_CSS.as_bytes().to_vec());
    }
    if stripped == "/manager_app.js" {
        return response(
            200,
            "application/javascript; charset=utf-8",
            APP_JS.as_bytes().to_vec(),
        );
    }
    // Backwards-compatible route for any saved bookmark from the
    // Phase 1.4 placeholder era. New code should not depend on this.
    if stripped == "/placeholder" {
        return response(
            200,
            "text/html; charset=utf-8",
            PLACEHOLDER_HTML.as_bytes().to_vec(),
        );
    }
    if stripped == "/manager_placeholder.css" {
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
            // Matches the main shell's policy in `src/shell.rs`.
            // The system WebView does not host a service backend,
            // so the `connect-src` rule is mostly belt-and-suspenders;
            // we keep it consistent so a future system page that
            // does call out to a service still passes.
            "default-src 'self' alex: http://alex.app; script-src 'self' alex: http://alex.app; style-src 'self' alex: http://alex.app; img-src 'self' data:; connect-src 'self' alex: http://alex.app; object-src 'none'; base-uri 'none'; frame-src 'none'; form-action 'none'",
        )
        .body(body.into())
        .expect("static response is valid")
}

// Anchor AppManifest so the path stays used even before Phase 1.5.
#[allow(dead_code)]
fn _manifest_anchor(_: &Path, _: AppManifest) {}

#[cfg(test)]
mod route_tests {
    //! Route-level coverage for `serve_system_asset`. We can't easily
    //! exercise the WebView itself without a window, but the helper is
    //! a pure string-to-response function so a unit test is enough to
    //! catch "renamed a file, forgot to update the route" regressions.
    use super::{APP_CSS, APP_HTML, APP_JS, PLACEHOLDER_CSS, PLACEHOLDER_HTML};
    use wry::http::header::CONTENT_TYPE;

    fn body_and_type(path: &str) -> (u16, String, Vec<u8>) {
        let response = super::serve_system_asset(path);
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .map(|value| value.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        let body = response.body().to_vec();
        (status, content_type, body)
    }

    #[test]
    fn index_serves_real_manager_app_html() {
        let (status, content_type, body) = body_and_type("/");
        assert_eq!(status, 200);
        assert!(content_type.starts_with("text/html"));
        // The real UI must be the served page, not the placeholder.
        assert!(body.starts_with(b"<!doctype html>"));
        assert!(
            body.windows(APP_HTML.len())
                .any(|window| window == APP_HTML.as_bytes())
        );
    }

    #[test]
    fn manager_app_css_route() {
        let (status, content_type, body) = body_and_type("/manager_app.css");
        assert_eq!(status, 200);
        assert!(content_type.starts_with("text/css"));
        assert!(!body.is_empty());
        assert_eq!(body, APP_CSS.as_bytes());
    }

    #[test]
    fn manager_app_js_route() {
        let (status, content_type, body) = body_and_type("/manager_app.js");
        assert_eq!(status, 200);
        assert!(content_type.starts_with("application/javascript"));
        assert!(!body.is_empty());
        assert_eq!(body, APP_JS.as_bytes());
    }

    #[test]
    fn placeholder_route_still_serves_legacy_html() {
        let (status, content_type, body) = body_and_type("/placeholder");
        assert_eq!(status, 200);
        assert!(content_type.starts_with("text/html"));
        assert_eq!(body, PLACEHOLDER_HTML.as_bytes());
    }

    #[test]
    fn placeholder_css_route_still_serves_legacy_stylesheet() {
        let (status, content_type, body) = body_and_type("/manager_placeholder.css");
        assert_eq!(status, 200);
        assert!(content_type.starts_with("text/css"));
        assert_eq!(body, PLACEHOLDER_CSS.as_bytes());
    }

    #[test]
    fn unknown_route_returns_404() {
        let (status, content_type, _body) = body_and_type("/nope.txt");
        assert_eq!(status, 404);
        assert!(content_type.starts_with("text/plain"));
    }

    #[test]
    fn app_manager_url_prefix_is_stripped() {
        // Wry sometimes reports the request URI with the manager
        // prefix still attached (`/app-manager/...`). Stripping
        // should be transparent — same status, same body.
        let (status, _, body) = body_and_type("/app-manager/manager_app.js");
        assert_eq!(status, 200);
        assert_eq!(body, APP_JS.as_bytes());
    }

    #[test]
    fn assets_contain_expected_landing_marks() {
        // Regression guard against a future edit that strips the
        // bridge or the per-service controls out of the bundled
        // HTML/JS. The UI is the user-facing surface; the test
        // only needs to know the high-level shape.
        let html = APP_HTML;
        assert!(
            html.contains("App Manager"),
            "asset html missing app manager title"
        );
        assert!(html.contains("manager_app.js"));
        assert!(html.contains("manager_app.css"));
        assert!(
            html.contains("audit-heading"),
            "asset html missing audit panel"
        );
        let js = APP_JS;
        assert!(js.contains("manager.list_apps"));
        assert!(js.contains("manager.list_services"));
        assert!(js.contains("manager.set_permission"));
        assert!(js.contains("manager.ai_overview"));
        assert!(js.contains("manager.ai_action"));
        assert!(html.contains("AI Runtime"));
        assert!(
            js.contains("manager.read_audit_log"),
            "asset js missing audit IPC"
        );
        assert!(js.contains("file.path")); // WebView2 path accessor
    }
}

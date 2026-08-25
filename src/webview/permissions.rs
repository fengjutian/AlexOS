//! WebView2 permission enforcement for camera, microphone, and geolocation.

use std::sync::Arc;

use webview2_com::{
    Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_PERMISSION_KIND, COREWEBVIEW2_PERMISSION_KIND_CAMERA,
        COREWEBVIEW2_PERMISSION_KIND_GEOLOCATION, COREWEBVIEW2_PERMISSION_KIND_MICROPHONE,
        COREWEBVIEW2_PERMISSION_STATE_ALLOW, COREWEBVIEW2_PERMISSION_STATE_DENY,
    },
    PermissionRequestedEventHandler,
};
use wry::{WebView, WebViewExtWindows};

use crate::api::ApiRouter;

/// Attach a fail-closed WebView2 permission handler. The initialization shim
/// performs the first-use prompt through Alex IPC; this native handler makes
/// sure a page cannot bypass that shim and ask WebView2 directly.
pub fn attach(webview: &WebView, router: Arc<ApiRouter>) -> Result<(), String> {
    let core = webview.webview();
    let handler = PermissionRequestedEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else { return Ok(()) };
        let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
        unsafe { args.PermissionKind(&mut kind)? };
        let permission = if kind == COREWEBVIEW2_PERMISSION_KIND_CAMERA {
            Some("media.camera")
        } else if kind == COREWEBVIEW2_PERMISSION_KIND_MICROPHONE {
            Some("media.microphone")
        } else if kind == COREWEBVIEW2_PERMISSION_KIND_GEOLOCATION {
            Some("geolocation")
        } else {
            None
        };
        if let Some(permission) = permission {
            let state = if router.webview_permission_granted(permission) {
                COREWEBVIEW2_PERMISSION_STATE_ALLOW
            } else {
                COREWEBVIEW2_PERMISSION_STATE_DENY
            };
            unsafe { args.SetState(state)? };
        }
        Ok(())
    }));
    let mut token = 0i64;
    unsafe { core.add_PermissionRequested(&handler, &mut token) }
        .map_err(|error| format!("failed to attach WebView2 permission handler: {error}"))?;
    Ok(())
}

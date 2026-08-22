//! Page-side shim for the WebView-level permission APIs that
//! the host cannot intercept via `wry` 0.55. WebView2's
//! `PermissionRequested` event is exposed by `IWebView2WebView5`
//! but `wry` does not surface it; the page would otherwise
//! receive Edge's default prompt and bypass the host's
//! `PermissionStore` entirely.
//!
//! The shim is a small JavaScript snippet that, when injected
//! alongside the existing [`BRIDGE`], wraps three APIs:
//!
//! - `navigator.mediaDevices.getUserMedia`
//! - `navigator.geolocation.getCurrentPosition`
//! - `navigator.geolocation.watchPosition`
//!
//! Before calling the original API, the shim asks the host for
//! the matching manifest permission via
//! `window.alex.invoke('system.requestPermission', { kind })`.
//! The host's own `permission_granted` flow already
//! (a) checks the persisted store, (b) shows the first-use
//! dialog on `Prompt`, and (c) records the result back to disk.
//! Caching the result for the rest of the page lifetime keeps
//! the shim non-intrusive for the common case where the user
//! has already accepted.
//!
//! The shim is only injected when the app's manifest actually
//! declares one of `media.camera`, `media.microphone`, or
//! `geolocation`. Apps that do not need WebView-level
//! permissions get nothing appended to the BRIDGE.

use crate::permission::Permission;

/// Returns `true` when the manifest needs the page-side
/// permission shim. Cheap check; used by host entry points to
/// decide whether to append the shim to the bridge.
pub fn needs_shim(manifest_permissions: &[Permission]) -> bool {
    manifest_permissions.iter().any(|p| {
        matches!(
            p,
            Permission::MediaCamera | Permission::MediaMicrophone | Permission::Geolocation
        )
    })
}

/// The JavaScript source to append to the BRIDGE
/// `with_initialization_script` payload. Returns an empty
/// string when the manifest does not request any
/// WebView-level permission — the host can then simply
/// concatenate, no conditional needed at the call site.
pub fn shim_source(manifest_permissions: &[Permission]) -> String {
    if !needs_shim(manifest_permissions) {
        return String::new();
    }
    SHIM_JS.to_string()
}

/// The shim itself. Kept as a constant so the same string is
/// reused across `shell::run`, `dev::run`, and
/// `manager_webview::run` (the manager never needs it because
/// the system WebView has no media/geolocation manifest
/// entries, so it short-circuits via [`needs_shim`]).
const SHIM_JS: &str = r#"
(() => {
  if (!window.alex || !window.alex.invoke) return;
  // Per-kind decision cache. The host already caches the
  // persisted result, but the JS side also caches so a page
  // that calls getUserMedia every animation frame does not
  // flood the IPC channel.
  const decided = new Map();
  async function ensure(kind) {
    if (decided.has(kind)) return decided.get(kind);
    try {
      const result = await window.alex.invoke('system.requestPermission', { kind });
      const granted = !!(result && result.granted);
      decided.set(kind, granted);
      return granted;
    } catch (error) {
      decided.set(kind, false);
      return false;
    }
  }
  // ----- media (camera + microphone) -----
  const mediaDevices = navigator.mediaDevices;
  if (mediaDevices && typeof mediaDevices.getUserMedia === 'function') {
    const original = mediaDevices.getUserMedia.bind(mediaDevices);
    mediaDevices.getUserMedia = function (constraints) {
      const wants = [];
      if (constraints) {
        if (constraints.audio) wants.push('media.microphone');
        if (constraints.video) wants.push('media.camera');
      }
      return Promise.all(wants.map(ensure)).then((results) => {
        for (let i = 0; i < results.length; i += 1) {
          if (!results[i]) {
            const kind = wants[i];
            const name = kind === 'media.microphone' ? 'microphone' : 'camera';
            throw new DOMException(
              `Permission for ${name} was denied by the host`,
              'NotAllowedError'
            );
          }
        }
        return original(constraints);
      });
    };
  }
  // ----- geolocation -----
  if (navigator.geolocation) {
    const wrap = (callOriginal) => function (success, error, options) {
      ensure('geolocation').then((granted) => {
        if (granted) {
          callOriginal(success, error, options);
        } else if (typeof error === 'function') {
          error({
            code: 1, // PERMISSION_DENIED
            message: 'Geolocation was denied by the host',
            PERMISSION_DENIED: 1,
          });
        }
      });
    };
    const geo = navigator.geolocation;
    if (typeof geo.getCurrentPosition === 'function') {
      const origGet = geo.getCurrentPosition.bind(geo);
      geo.getCurrentPosition = wrap(origGet);
    }
    if (typeof geo.watchPosition === 'function') {
      const origWatch = geo.watchPosition.bind(geo);
      geo.watchPosition = wrap(origWatch);
    }
  }
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_with(perms: &[Permission]) -> Vec<Permission> {
        perms.to_vec()
    }

    #[test]
    fn empty_manifest_gets_no_shim() {
        assert!(!needs_shim(&manifest_with(&[])));
        assert_eq!(shim_source(&manifest_with(&[])), "");
    }

    #[test]
    fn manifest_without_webview_perms_gets_no_shim() {
        // App that only needs filesystem / clipboard / runtime
        // permissions — the WebView-level APIs are irrelevant
        // and the shim should not be injected (smaller init
        // script, fewer false-positive conflicts).
        let perms = manifest_with(&[
            Permission::FilesystemRead { paths: vec![] },
            Permission::ClipboardRead,
        ]);
        assert!(!needs_shim(&perms));
        assert_eq!(shim_source(&perms), "");
    }

    #[test]
    fn manifest_with_camera_requests_shim() {
        let perms = manifest_with(&[Permission::MediaCamera]);
        assert!(needs_shim(&perms));
        let src = shim_source(&perms);
        assert!(!src.is_empty());
        // Sanity: must wrap the three APIs we care about.
        assert!(src.contains("getUserMedia"));
        assert!(src.contains("getCurrentPosition"));
        assert!(src.contains("watchPosition"));
        // And must talk to the host via the BRIDGE-exposed API.
        assert!(src.contains("system.requestPermission"));
    }

    #[test]
    fn manifest_with_microphone_requests_shim() {
        let perms = manifest_with(&[Permission::MediaMicrophone]);
        assert!(needs_shim(&perms));
        assert!(shim_source(&perms).contains("getUserMedia"));
    }

    #[test]
    fn manifest_with_geolocation_requests_shim() {
        let perms = manifest_with(&[Permission::Geolocation]);
        assert!(needs_shim(&perms));
        let src = shim_source(&perms);
        assert!(src.contains("geolocation"));
        assert!(src.contains("getCurrentPosition"));
    }

    #[test]
    fn mixed_manifest_requests_shim() {
        // Media + geo at the same time: the shim must wrap
        // both paths. We only need a smoke test — the
        // presence of the marker is enough to catch
        // regressions where one branch gets dropped.
        let perms = manifest_with(&[Permission::MediaCamera, Permission::Geolocation]);
        let src = shim_source(&perms);
        assert!(src.contains("getUserMedia"));
        assert!(src.contains("getCurrentPosition"));
    }

    #[test]
    fn shim_source_is_idempotent_across_calls() {
        // The host may build the bridge string more than
        // once (e.g. when a window is rebuilt). The shim
        // should be byte-identical each time so the WebView
        // init script is stable.
        let perms = manifest_with(&[Permission::MediaCamera]);
        let a = shim_source(&perms);
        let b = shim_source(&perms);
        assert_eq!(a, b);
    }
}

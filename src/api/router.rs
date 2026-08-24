use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::container::{ContainerContext, DefaultContainerService};
use crate::{
    authorization::{AuditEntry as AuthorizationAuditEntry, PermissionDecision, PermissionStore},
    event_bus::{EventBus, SubscriptionFilter},
    file_token::{FileOp, FileTokenStore},
    ipc::{PROTOCOL_VERSION, Request, Response, SubscribeRequest, UnsubscribeRequest},
    manifest::AppManifest,
    menu_tray::{MenuStore, MenuTemplate, TraySpec},
    native::{self, DialogFilter, HostCommand, NativeHost, OpenDialogSpec, SaveDialogSpec},
    permission::Permission,
    process::{ProcessRegistry, ProcessSpec},
    runtime::{RuntimeError, RuntimeHandle},
    storage::AppStorage,
    watcher::{WatchHandle, WatcherRegistry},
    windows::{CreateWindowSpec, WindowBounds, WindowId, WindowRegistry},
};

const MAX_IPC_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_BINARY_VALUE_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_RUNTIME_TIMEOUT: Duration = Duration::from_secs(30);
const FILE_TOKEN_TTL: Duration = Duration::from_secs(60 * 5);

/// Cap on concurrent in-flight IPC requests per app. Picked
/// conservatively so a single page cannot starve the host by
/// opening hundreds of calls at once.
const MAX_INFLIGHT_PER_APP: usize = 32;

pub struct ApiRouter {
    package_root: PathBuf,
    manifest: AppManifest,
    runtime: Option<RuntimeHandle>,
    permission_store: Option<PermissionStore>,
    native_host: Option<Arc<dyn NativeHost>>,
    system_install_root: Option<PathBuf>,
    system_trust_root: Option<PathBuf>,
    container_service: Option<Arc<DefaultContainerService>>,
    event_bus: Arc<EventBus>,
    file_tokens: Arc<FileTokenStore>,
    storage: Option<AppStorage>,
    watcher_registry: Option<Arc<WatcherRegistry>>,
    watch_handles: Mutex<HashMap<String, WatchHandle>>,
    windows: Arc<WindowRegistry>,
    menu_store: Arc<MenuStore>,
    process_registry: Arc<ProcessRegistry>,
    inflight: Arc<Mutex<InflightTracker>>,
    /// When true, every call to `permission_granted` is echoed
    /// to stderr with the resolved decision. `alex dev` flips
    /// this on; the production shell does not. Off by default
    /// because a polling page would otherwise log every frame.
    permission_log: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Default)]
struct InflightTracker {
    pending: HashMap<String, CancellationToken>,
    count: usize,
}

use std::collections::HashMap;

/// Lightweight cancellation token used to flag a single request
/// as cancelled without killing the underlying runtime. Each
/// `runtime.invoke` call owns one; the host flips it to `true`
/// when the page issues `runtime.cancel` with the matching
/// request id. The backend then sees an `abort` hint on its
/// `AbortController`-style wrapper.
#[derive(Debug, Clone)]
struct CancellationToken {
    flag: Arc<std::sync::atomic::AtomicBool>,
}

impl CancellationToken {
    fn new() -> Self {
        Self {
            flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
    fn cancel(&self) {
        self.flag.store(true, std::sync::atomic::Ordering::Release);
    }
}

use std::sync::Mutex;
mod dispatch;
mod handlers;

impl ApiRouter {
    pub fn new(package_root: PathBuf, manifest: AppManifest) -> Self {
        let package_root = package_root.canonicalize().unwrap_or(package_root);
        let bus = EventBus::new();
        let tokens = FileTokenStore::new(manifest.id.clone(), FILE_TOKEN_TTL);
        let storage = crate::runtime::compute_app_dirs(&manifest.id)
            .ok()
            .and_then(|dirs| AppStorage::open(&dirs.data).ok());
        let watcher_registry = WatcherRegistry::new(Arc::clone(&bus));
        let windows = WindowRegistry::new();
        let menu_store = MenuStore::new();
        let process_registry = ProcessRegistry::new();
        Self {
            package_root,
            manifest,
            runtime: None,
            permission_store: None,
            native_host: None,
            system_install_root: None,
            system_trust_root: None,
            container_service: None,
            event_bus: bus,
            file_tokens: tokens,
            storage,
            watcher_registry: Some(watcher_registry),
            watch_handles: Mutex::new(HashMap::new()),
            windows,
            menu_store,
            process_registry,
            inflight: Arc::new(Mutex::new(InflightTracker::default())),
            permission_log: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    pub fn with_runtime(mut self, runtime: RuntimeHandle) -> Self {
        self.runtime = Some(runtime);
        self
    }

    pub fn with_permission_store(mut self, store: PermissionStore) -> Self {
        self.permission_store = Some(store);
        self
    }

    /// Enable verbose permission-decision logging on stderr.
    /// Used by `alex dev` to surface the "permission call panel"
    /// in the dev terminal without making it the production
    /// default. See `permission_granted` for the log format.
    pub fn with_permission_logging(self, enabled: bool) -> Self {
        self.permission_log
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
        self
    }

    pub fn with_native_host(mut self, host: Arc<dyn NativeHost>) -> Self {
        self.native_host = Some(host);
        self
    }

    /// Override the persistent data directory. Primarily useful for isolated
    /// hosts and tests that must not touch the current user's profile.
    pub fn with_storage_root(mut self, data_dir: PathBuf) -> Self {
        self.storage = AppStorage::open(&data_dir).ok();
        self
    }

    /// System-wide install root. Only consulted by `system.*` methods
    /// (which require `kind: "plugin"`). Apps that don't call into
    /// `system.*` never see this.
    pub fn with_system_install_root(mut self, root: PathBuf) -> Self {
        self.container_service = ContainerContext::with_default_data_root(root.clone())
            .and_then(DefaultContainerService::new)
            .map(Arc::new)
            .ok();
        self.system_install_root = Some(root);
        self
    }

    /// Trust store root. Same gating rules as the install root: only
    /// consulted by `system.*` methods (which require `kind: "plugin"`).
    /// Apps that don't call into `system.*` never see this.
    pub fn with_system_trust_root(mut self, root: PathBuf) -> Self {
        self.system_trust_root = Some(root);
        self
    }

    /// Return the bus so the shell layer can forward delivered
    /// events to the WebView. Kept as a getter rather than a
    /// constructor argument so `ApiRouter` does not grow
    /// every new dependency into a builder method.
    pub fn event_bus(&self) -> Arc<EventBus> {
        Arc::clone(&self.event_bus)
    }

    /// Convert an OS file-drop into app-scoped, short-lived grants and
    /// enqueue it for WebView delivery. Returns false when the package did
    /// not declare (or the user revoked) `filesystem.drop`.
    pub fn deliver_file_drop(&self, paths: Vec<PathBuf>, x: i32, y: i32) -> bool {
        let declared = self
            .manifest
            .permissions
            .iter()
            .any(|permission| matches!(permission, Permission::FilesystemDrop));
        if !declared || !self.permission_granted("filesystem.drop") {
            return false;
        }
        let files: Vec<Value> = paths
            .iter()
            .filter_map(|path| {
                self.file_tokens
                    .issue(&self.manifest.id, path, &[FileOp::Read])
                    .ok()
                    .and_then(|grant| serde_json::to_value(grant).ok())
            })
            .collect();
        if files.is_empty() {
            return false;
        }
        self.event_bus.deliver(
            "fileDrop",
            &json!({ "files": files, "position": { "x": x, "y": y } }),
        );
        true
    }

    /// Reconcile the logical registry when the user closes a native child
    /// window instead of calling `window.destroy` through IPC.
    pub fn native_window_closed(&self, window_id: u64) {
        let _ = self.windows.destroy(&self.manifest.id, WindowId(window_id));
    }

    fn require_secondary_window_host(&self) -> Result<(), (&'static str, String)> {
        if self
            .native_host
            .as_ref()
            .is_some_and(|host| host.supports_secondary_windows())
        {
            Ok(())
        } else {
            Err((
                "NATIVE_UNAVAILABLE",
                "secondary windows are unavailable in this host (alex dev does not emulate them)"
                    .into(),
            ))
        }
    }

    /// Drop every resource this router owns. The shell calls
    /// this when the window is destroyed or the host kills
    /// the app session.
    pub fn shutdown(&self) {
        self.watch_handles
            .lock()
            .expect("watch handles lock poisoned")
            .clear();
        self.event_bus.clear();
        self.file_tokens.revoke_all(&self.manifest.id);
        self.windows.drop_app(&self.manifest.id);
        self.menu_store.drop_app(&self.manifest.id);
        self.process_registry.clear();
        let mut tracker = self.inflight.lock().expect("inflight lock poisoned");
        for token in tracker.pending.values() {
            token.cancel();
        }
        tracker.pending.clear();
        tracker.count = 0;
    }

    pub fn restart_runtime(&self, timeout: Duration) -> Option<Result<(), RuntimeError>> {
        self.runtime
            .as_ref()
            .map(|handle| handle.restart(timeout).map(|_| ()))
    }

    pub fn dispatch_json(&self, input: &str) -> Response {
        self.dispatch_json_for_window(input, None)
    }

    pub fn dispatch_json_for_window(&self, input: &str, window_id: Option<u64>) -> Response {
        if input.len() > MAX_IPC_MESSAGE_BYTES {
            return Response::error(
                "unknown",
                "MESSAGE_TOO_LARGE",
                "IPC messages are limited to 1 MiB",
            );
        }
        let request = match serde_json::from_str::<Request>(input) {
            Ok(request) => request,
            Err(error) => {
                return Response::error("unknown", "INVALID_REQUEST", error.to_string());
            }
        };
        self.dispatch_for_window(request, window_id)
    }

    pub fn dispatch(&self, request: Request) -> Response {
        self.dispatch_for_window(request, None)
    }

    pub fn dispatch_for_window(&self, request: Request, window_id: Option<u64>) -> Response {
        if request.protocol != PROTOCOL_VERSION {
            return Response::error(
                request.id,
                "UNSUPPORTED_PROTOCOL",
                format!("expected protocol {PROTOCOL_VERSION}"),
            );
        }
        if request.source != self.manifest.id {
            return Response::error(request.id, "SOURCE_MISMATCH", "invalid package identity");
        }
        if request
            .deadline_ms
            .is_some_and(|deadline| now_ms() > deadline)
        {
            return Response::error(request.id, "DEADLINE_EXCEEDED", "request expired");
        }
        // Reject duplicate in-flight request ids to keep the
        // page from double-submitting. The page can simply
        // re-issue with a fresh id.
        if !self.register_inflight(&request.id) {
            return Response::error(
                request.id,
                "DUPLICATE_REQUEST_ID",
                "an in-flight request with this id is already running",
            );
        }
        let result = self.dispatch_inner(&request, window_id);
        self.unregister_inflight(&request.id);
        match result {
            Ok(value) => Response::success(request.id, value),
            Err((code, message)) => Response::error(request.id, code, message),
        }
    }

    fn dispatch_inner(&self, request: &Request, window_id: Option<u64>) -> ApiResult {
        dispatch::route(self, request, window_id)
    }

    // ------------------------------------------------------------------
    // Inflight tracking / cancellation
    // ------------------------------------------------------------------

    fn register_inflight(&self, id: &str) -> bool {
        let mut tracker = self.inflight.lock().expect("inflight lock poisoned");
        if tracker.pending.contains_key(id) {
            return false;
        }
        if tracker.count >= MAX_INFLIGHT_PER_APP {
            return false;
        }
        tracker
            .pending
            .insert(id.to_owned(), CancellationToken::new());
        tracker.count += 1;
        true
    }

    fn unregister_inflight(&self, id: &str) {
        let mut tracker = self.inflight.lock().expect("inflight lock poisoned");
        if tracker.pending.remove(id).is_some() {
            tracker.count = tracker.count.saturating_sub(1);
        }
    }

    fn cancel_inflight(&self, id: &str) -> bool {
        let tracker = self.inflight.lock().expect("inflight lock poisoned");
        if let Some(token) = tracker.pending.get(id) {
            token.cancel();
            return true;
        }
        false
    }

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    fn require_plugin(&self) -> ApiResult {
        if self.manifest.kind != crate::manifest::PackageKind::Plugin {
            return Err((
                "PERMISSION_DENIED",
                "system methods are reserved for plugins".into(),
            ));
        }
        Ok(json!({}))
    }

    fn require_permission(
        &self,
        predicate: impl Fn(&Permission) -> bool,
        name: &'static str,
    ) -> Result<(), (&'static str, String)> {
        let declared =
            self.manifest.permissions.iter().any(predicate) && self.permission_granted(name);
        declared.then_some(()).ok_or((
            "PERMISSION_DENIED",
            format!("{name} is not allowed or was revoked"),
        ))
    }

    fn has_permission_for(&self, operation: &str, _path: &Path) -> bool {
        // A coarse-grained check; the path is already
        // scope-validated by `resolve_scoped_path` before
        // reaching here, so this just confirms the app
        // declared the permission at all.
        self.manifest
            .permissions
            .iter()
            .any(|permission| permission.paths_for(operation).is_some())
            && self.permission_granted(operation)
    }

    fn require_runtime_manage(&self) -> Result<(), (&'static str, String)> {
        let allowed = self
            .manifest
            .permissions
            .iter()
            .any(|permission| matches!(permission, Permission::RuntimeManage))
            && self.permission_granted("runtime.manage");
        allowed.then_some(()).ok_or((
            "PERMISSION_DENIED",
            "runtime.manage is not allowed or was revoked".into(),
        ))
    }

    fn permission_granted(&self, name: &str) -> bool {
        let Some(store) = &self.permission_store else {
            // No store attached (e.g. an internal helper path
            // during startup): silently allow. This branch is
            // intentionally not logged because the dev does not
            // see internal calls; the public path is
            // `with_permission_store` which is set by every
            // host entry point.
            return true;
        };
        let perm_log = self
            .permission_log
            .load(std::sync::atomic::Ordering::Relaxed);
        match store.decision(name) {
            PermissionDecision::Granted => {
                if perm_log {
                    eprintln!(
                        "{}",
                        format_permission_decision("cached-grant", name, &self.manifest.name,)
                    );
                }
                true
            }
            PermissionDecision::Denied => {
                if perm_log {
                    eprintln!(
                        "{}",
                        format_permission_decision("cached-deny", name, &self.manifest.name,)
                    );
                }
                false
            }
            PermissionDecision::Prompt => {
                if perm_log {
                    eprintln!(
                        "{}",
                        format_permission_decision("prompt", name, &self.manifest.name)
                    );
                }
                let granted =
                    native::confirm_permission(&self.manifest.name, name).unwrap_or(false);
                let decision = if granted {
                    PermissionDecision::Granted
                } else {
                    PermissionDecision::Denied
                };
                let _ = store.set(name, decision);
                if perm_log {
                    eprintln!(
                        "{}",
                        format_permission_decision(
                            if granted {
                                "answered-yes"
                            } else {
                                "answered-no"
                            },
                            name,
                            &self.manifest.name,
                        )
                    );
                }
                granted
            }
        }
    }
}

fn directory_size(root: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(root) else {
        return 0;
    };
    if metadata.file_type().is_symlink() {
        return 0;
    }
    if metadata.is_file() {
        return metadata.len();
    }
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| directory_size(&entry.path()))
        .sum()
}

/// One line of the dev-mode permission call panel. Kept as a
/// free function so the format is unit-testable without
/// capturing stderr (which is fragile in `cargo test`'s
/// parallel runner).
fn format_permission_decision(stage: &str, permission: &str, app_name: &str) -> String {
    format!("alex: permission {permission} on {app_name} -> {stage}")
}

type ApiResult = Result<Value, (&'static str, String)>;

// ---- parameter structs ---------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathParams {
    path: String,
    #[serde(default)]
    access_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteParams {
    path: String,
    content: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteBinaryParams {
    path: String,
    data: String,
    #[serde(default)]
    access_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateDirParams {
    path: String,
    #[serde(default)]
    recursive: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveParams {
    path: String,
    #[serde(default)]
    recursive: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FromToParams {
    from: String,
    to: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyParams {
    key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyValueParams {
    key: String,
    value: Value,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct OpenDialogParams {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    default_path: Option<String>,
    #[serde(default)]
    filters: Option<Vec<DialogFilterParam>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct SaveDialogParams {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    default_path: Option<String>,
    #[serde(default)]
    filters: Option<Vec<DialogFilterParam>>,
    #[serde(default)]
    suggested_name: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DialogFilterParam {
    name: String,
    #[serde(default)]
    extensions: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeInvokeParams {
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeCancelParams {
    request_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClipboardWriteParams {
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenExternalParams {
    url: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WindowTitleParams {
    title: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NotificationParams {
    title: String,
    body: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct ProcessSpawnParams {
    executable: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct NetFetchParams {
    url: String,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    headers: Option<Value>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    max_bytes: Option<usize>,
}

// ---- helpers -------------------------------------------------------------

fn parse_params<T: for<'de> Deserialize<'de>>(value: &Value) -> Result<T, (&'static str, String)> {
    serde_json::from_value(value.clone()).map_err(|error| ("INVALID_PARAMS", error.to_string()))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn filters_from_params(filters: Option<&Vec<DialogFilterParam>>) -> Vec<DialogFilter> {
    filters
        .map(|list| {
            list.iter()
                .map(|f| DialogFilter {
                    name: f.name.clone(),
                    extensions: f.extensions.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn mint_token_entry(store: &FileTokenStore, app_id: &str, path: &Path, ops: &[FileOp]) -> Value {
    match store.issue(app_id, path, ops) {
        Ok(token) => json!({
            "path": path.to_string_lossy(),
            "token": token.token,
            "ops": token.ops,
            "expiresAt": token.expires_at_ms,
        }),
        Err(_) => json!({
            "path": path.to_string_lossy(),
            "token": Value::Null,
        }),
    }
}

fn copy_dir_recursive(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = to.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else if file_type.is_symlink() {
            // Skip symlinks to avoid escape.
            continue;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::format_permission_decision;

    #[test]
    fn permission_log_cached_grant_includes_permission_and_app() {
        let line = format_permission_decision("cached-grant", "fs.readText", "com.alex.demo");
        assert_eq!(
            line,
            "alex: permission fs.readText on com.alex.demo -> cached-grant"
        );
    }

    #[test]
    fn permission_log_cached_deny_uses_distinct_stage_token() {
        let grant = format_permission_decision("cached-grant", "net.fetch", "a");
        let deny = format_permission_decision("cached-deny", "net.fetch", "a");
        let answered_yes = format_permission_decision("answered-yes", "net.fetch", "a");
        let answered_no = format_permission_decision("answered-no", "net.fetch", "a");
        let prompt = format_permission_decision("prompt", "net.fetch", "a");
        assert_ne!(grant, deny);
        assert_ne!(answered_yes, answered_no);
        assert_ne!(prompt, answered_yes);
        assert!(grant.contains("cached-grant"));
        assert!(answered_no.ends_with(" -> answered-no"));
    }

    #[test]
    fn permission_log_preserves_long_permission_names_verbatim() {
        let name = "system.manageApps.withQuotaOverride";
        let line = format_permission_decision("cached-grant", name, "com.alex.demo");
        assert!(line.contains(name));
    }
}

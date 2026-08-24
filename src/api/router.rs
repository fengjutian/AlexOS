use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::container::{
    ContainerContext, ContainerFilter, ContainerService, CreateRequest, DefaultContainerService,
};
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
        match request.method.as_str() {
            // ---- filesystem ------------------------------------------------
            "filesystem.readText" => self.read_text(&request.params),
            "filesystem.writeText" => self.write_text(&request.params),
            "filesystem.readBinary" => self.read_binary(&request.params),
            "filesystem.writeBinary" => self.write_binary(&request.params),
            "filesystem.exists" => self.fs_exists(&request.params),
            "filesystem.stat" => self.fs_stat(&request.params),
            "filesystem.readDir" => self.fs_read_dir(&request.params),
            "filesystem.createDir" => self.fs_create_dir(&request.params),
            "filesystem.remove" => self.fs_remove(&request.params),
            "filesystem.rename" => self.fs_rename(&request.params),
            "filesystem.copy" => self.fs_copy(&request.params),
            "filesystem.watch" => self.fs_watch(&request.params, window_id),
            "filesystem.unwatch" => self.fs_unwatch(&request.params),
            // ---- storage ---------------------------------------------------
            "storage.get" => self.storage_get(&request.params),
            "storage.set" => self.storage_set(&request.params),
            "storage.delete" => self.storage_delete(&request.params),
            "storage.clear" => self.storage_clear(),
            "storage.keys" => self.storage_keys(),
            "paths.dataDir" => self.paths_data_dir(),
            "paths.cacheDir" => self.paths_cache_dir(),
            "paths.tempDir" => self.paths_temp_dir(),
            // ---- dialog ----------------------------------------------------
            "dialog.openFile" => self.dialog_open_file(&request.params, false, false),
            "dialog.openFiles" => self.dialog_open_files(&request.params),
            "dialog.openDirectory" => self.dialog_open_directory(&request.params),
            "dialog.saveFile" => self.dialog_save_file(&request.params),
            // ---- clipboard -------------------------------------------------
            "clipboard.readText" => self.clipboard_read_text(),
            "clipboard.writeText" => self.clipboard_write_text(&request.params),
            // ---- system ----------------------------------------------------
            "system.info" => self.system_info(),
            "system.capabilities" => self.system_capabilities(),
            "system.openExternal" => self.open_external(&request.params),
            "system.requestPermission" => self.request_permission(&request.params),
            "system.install" => self.system_install(&request.params),
            "system.uninstall" => self.system_uninstall(&request.params),
            "system.updateStart" => self.system_update_start(&request.params),
            "system.updateTasks" => self.system_update_tasks(),
            "system.updateCancel" => self.system_update_cancel(&request.params),
            "system.updateRetry" => self.system_update_retry(&request.params),
            "system.listApps" => self.system_list_apps(),
            "system.listExtensions" => self.system_list_extensions(),
            "system.listPermissions" => self.system_list_permissions(&request.params),
            "system.setPermission" => self.system_set_permission(&request.params),
            "system.listTrustedPublishers" => self.system_list_trusted_publishers(),
            "system.readAuditLog" => self.system_read_audit_log(&request.params),
            "system.container.create" => self.system_container_create(&request.params),
            "system.container.start" => self.system_container_start(&request.params),
            "system.container.stop" => self.system_container_stop(&request.params),
            "system.container.restart" => self.system_container_restart(&request.params),
            "system.container.remove" => self.system_container_remove(&request.params),
            "system.container.inspect" => self.system_container_inspect(&request.params),
            "system.container.list" => self.system_container_list(&request.params),
            "system.container.logs" => self.system_container_logs(&request.params),
            // ---- window ----------------------------------------------------
            "window.setTitle" => self.window_set_title(&request.params),
            "window.minimize" => self.window_command(HostCommand::MinimizeWindow),
            "window.maximize" => self.window_command(HostCommand::MaximizeWindow),
            "window.close" => self.window_command(HostCommand::CloseWindow),
            "window.create" => self.window_create(&request.params),
            "window.list" => self.window_list(),
            "window.getBounds" => self.window_get_bounds(&request.params),
            "window.setBounds" => self.window_set_bounds(&request.params),
            "window.setFullscreen" => self.window_set_fullscreen(&request.params),
            "window.isFullscreen" => self.window_is_fullscreen(&request.params),
            "window.destroy" => self.window_destroy(&request.params),
            // ---- menu / tray / shortcut ------------------------------------
            "menu.setApplicationMenu" => self.menu_set_application_menu(&request.params),
            "menu.setContextMenu" => self.menu_set_context_menu(&request.params),
            "tray.create" => self.tray_create(&request.params),
            "tray.destroy" => self.tray_destroy(&request.params),
            "shortcuts.register" => self.shortcuts_register(&request.params),
            "shortcuts.unregister" => self.shortcuts_unregister(&request.params),
            "shortcuts.list" => self.shortcuts_list(),
            // ---- notification ---------------------------------------------
            "notification.show" => self.notification_show(&request.params),
            // ---- runtime ---------------------------------------------------
            "runtime.invoke" => {
                self.runtime_invoke(&request.id, &request.params, request.deadline_ms)
            }
            "runtime.status" => self.runtime_status(),
            "runtime.restart" => self.runtime_restart(),
            "runtime.cancel" => self.runtime_cancel(&request.params),
            // ---- events ----------------------------------------------------
            "events.subscribe" => self.events_subscribe(&request.id, &request.params, window_id),
            "events.unsubscribe" => self.events_unsubscribe(&request.params),
            // ---- process ---------------------------------------------------
            "process.spawn" => self.process_spawn(&request.params),
            "process.kill" => self.process_kill(&request.params),
            // ---- network ---------------------------------------------------
            "net.fetch" => self.net_fetch(&request.params),
            // ---- fallback -------------------------------------------------
            _ => Err(("METHOD_NOT_FOUND", "unknown Alex API method".to_owned())),
        }
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
    // Filesystem
    // ------------------------------------------------------------------

    fn resolve_scoped(
        &self,
        path: &str,
        operation: &str,
    ) -> Result<PathBuf, (&'static str, String)> {
        let permission = self
            .manifest
            .permissions
            .iter()
            .find(|permission| permission.paths_for(operation).is_some())
            .ok_or((
                "PERMISSION_DENIED",
                format!("{operation} is not declared by this package"),
            ))?;
        if !self.permission_granted(operation) {
            return Err(("PERMISSION_DENIED", format!("{operation} was revoked")));
        }
        let requested = PathBuf::from(path);
        crate::permission::resolve_scoped_path(
            &self.package_root,
            &requested,
            permission,
            operation,
        )
        .map_err(|error| match error {
            crate::permission::PathError::NotAllowed => (
                "PERMISSION_DENIED",
                format!("{operation} is not declared by this package"),
            ),
            crate::permission::PathError::NotFound(_) => {
                ("PATH_NOT_FOUND", format!("path not found: {path}"))
            }
            crate::permission::PathError::Escape => {
                ("PATH_ERROR", "path escapes the package root".into())
            }
            crate::permission::PathError::OutsideScope => (
                "PERMISSION_DENIED",
                format!("{path} is outside the granted scope"),
            ),
        })
    }

    fn resolve_with_token(
        &self,
        path: &str,
        token: Option<&str>,
        op: FileOp,
    ) -> Result<PathBuf, (&'static str, String)> {
        let path_buf = PathBuf::from(path);
        if let Some(token) = token {
            self.file_tokens
                .verify(token, &self.manifest.id, &path_buf, op)
                .map_err(|error| ("TOKEN_ERROR", error.to_string()))
        } else {
            let operation = match op {
                FileOp::Read => "filesystem.read",
                FileOp::Write => "filesystem.write",
            };
            self.resolve_scoped(path, operation)
        }
    }

    fn read_text(&self, params: &Value) -> ApiResult {
        let params: PathParams = parse_params(params)?;
        let resolved = self.resolve_scoped(&params.path, "filesystem.read")?;
        let contents = fs::read_to_string(&resolved)
            .map_err(|error| ("IO_ERROR", format!("cannot read text: {error}")))?;
        Ok(json!({ "content": contents }))
    }

    fn write_text(&self, params: &Value) -> ApiResult {
        let params: WriteParams = parse_params(params)?;
        let resolved = self.resolve_scoped(&params.path, "filesystem.write")?;
        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| ("IO_ERROR", format!("cannot create parent: {error}")))?;
        }
        let temp = resolved.with_extension("alex.tmp");
        fs::write(&temp, params.content.as_bytes())
            .map_err(|error| ("IO_ERROR", format!("cannot write temp: {error}")))?;
        fs::rename(&temp, &resolved)
            .map_err(|error| ("IO_ERROR", format!("cannot rename: {error}")))?;
        Ok(json!({ "written": true }))
    }

    fn read_binary(&self, params: &Value) -> ApiResult {
        let params: PathParams = parse_params(params)?;
        let resolved =
            self.resolve_with_token(&params.path, params.access_token.as_deref(), FileOp::Read)?;
        let bytes = fs::read(&resolved)
            .map_err(|error| ("IO_ERROR", format!("cannot read binary: {error}")))?;
        if bytes.len() > MAX_BINARY_VALUE_BYTES {
            return Err((
                "VALUE_TOO_LARGE",
                format!(
                    "binary file is {} bytes; cap is {MAX_BINARY_VALUE_BYTES}",
                    bytes.len()
                ),
            ));
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok(json!({ "encoding": "base64", "data": encoded }))
    }

    fn write_binary(&self, params: &Value) -> ApiResult {
        let params: WriteBinaryParams = parse_params(params)?;
        if params.data.len() > MAX_BINARY_VALUE_BYTES {
            return Err((
                "VALUE_TOO_LARGE",
                format!(
                    "binary payload is {} bytes; cap is {MAX_BINARY_VALUE_BYTES}",
                    params.data.len()
                ),
            ));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(params.data.as_bytes())
            .map_err(|error| ("INVALID_PARAMS", format!("invalid base64: {error}")))?;
        let resolved =
            self.resolve_with_token(&params.path, params.access_token.as_deref(), FileOp::Write)?;
        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| ("IO_ERROR", format!("cannot create parent: {error}")))?;
        }
        let temp = resolved.with_extension("alex.tmp");
        fs::write(&temp, &bytes)
            .map_err(|error| ("IO_ERROR", format!("cannot write temp: {error}")))?;
        fs::rename(&temp, &resolved)
            .map_err(|error| ("IO_ERROR", format!("cannot rename: {error}")))?;
        Ok(json!({ "written": true }))
    }

    fn fs_exists(&self, params: &Value) -> ApiResult {
        let params: PathParams = parse_params(params)?;
        let resolved = self.resolve_scoped(&params.path, "filesystem.read")?;
        Ok(json!({ "exists": resolved.exists() }))
    }

    fn fs_stat(&self, params: &Value) -> ApiResult {
        let params: PathParams = parse_params(params)?;
        let resolved = self.resolve_scoped(&params.path, "filesystem.read")?;
        let metadata = fs::metadata(&resolved)
            .map_err(|error| ("IO_ERROR", format!("cannot stat: {error}")))?;
        let file_type = if metadata.is_dir() {
            "directory"
        } else if metadata.is_file() {
            "file"
        } else if metadata.is_symlink() {
            "symlink"
        } else {
            "other"
        };
        let modified_ms = metadata
            .modified()
            .ok()
            .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64);
        Ok(json!({
            "path": resolved.to_string_lossy(),
            "type": file_type,
            "size": metadata.len(),
            "readOnly": metadata.permissions().readonly(),
            "modifiedMs": modified_ms,
        }))
    }

    fn fs_read_dir(&self, params: &Value) -> ApiResult {
        let params: PathParams = parse_params(params)?;
        let resolved = self.resolve_scoped(&params.path, "filesystem.read")?;
        let entries = fs::read_dir(&resolved)
            .map_err(|error| ("IO_ERROR", format!("cannot read dir: {error}")))?;
        let mut out = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(value) => value,
                Err(_) => continue,
            };
            let metadata = entry.metadata().ok();
            let file_type = metadata
                .as_ref()
                .map(|m| {
                    if m.is_dir() {
                        "directory"
                    } else if m.is_symlink() {
                        "symlink"
                    } else {
                        "file"
                    }
                })
                .unwrap_or("other");
            out.push(json!({
                "name": entry.file_name().to_string_lossy(),
                "type": file_type,
                "size": metadata.as_ref().map(|m| m.len()).unwrap_or(0),
            }));
        }
        out.sort_by(|a, b| {
            a["name"]
                .as_str()
                .unwrap_or("")
                .cmp(b["name"].as_str().unwrap_or(""))
        });
        Ok(json!({ "entries": out }))
    }

    fn fs_create_dir(&self, params: &Value) -> ApiResult {
        let params: CreateDirParams = parse_params(params)?;
        let resolved = self.resolve_scoped(&params.path, "filesystem.write")?;
        if resolved.exists() {
            if params.recursive.unwrap_or(false) {
                return Ok(json!({ "created": false, "exists": true }));
            }
            return Err(("ALREADY_EXISTS", "path already exists".into()));
        }
        if params.recursive.unwrap_or(false) {
            fs::create_dir_all(&resolved)
                .map_err(|error| ("IO_ERROR", format!("cannot create dir: {error}")))?;
        } else {
            fs::create_dir(&resolved)
                .map_err(|error| ("IO_ERROR", format!("cannot create dir: {error}")))?;
        }
        Ok(json!({ "created": true }))
    }

    fn fs_remove(&self, params: &Value) -> ApiResult {
        let params: RemoveParams = parse_params(params)?;
        if params.recursive.unwrap_or(false) {
            // Recursive removal must never be allowed for the
            // package root. Defence in depth: even if the app
            // has write access, we refuse to delete its own
            // root.
            let package_canonical = self
                .package_root
                .canonicalize()
                .unwrap_or_else(|_| self.package_root.clone());
            let resolved = self.resolve_scoped(&params.path, "filesystem.delete")?;
            if resolved == package_canonical {
                return Err((
                    "OPERATION_FORBIDDEN",
                    "refusing to delete the package root".into(),
                ));
            }
            if resolved.starts_with(&package_canonical)
                && resolved.parent() == Some(package_canonical.as_path())
            {
                return Err((
                    "OPERATION_FORBIDDEN",
                    "refusing to remove a top-level package directory recursively".into(),
                ));
            }
            if !self.has_permission_for("filesystem.delete", &resolved) {
                return Err((
                    "PERMISSION_DENIED",
                    "filesystem.delete is not allowed".into(),
                ));
            }
            fs::remove_dir_all(&resolved)
                .map_err(|error| ("IO_ERROR", format!("cannot remove dir: {error}")))?;
        } else {
            let resolved = self.resolve_scoped(&params.path, "filesystem.delete")?;
            let metadata = fs::metadata(&resolved)
                .map_err(|error| ("IO_ERROR", format!("cannot stat: {error}")))?;
            if metadata.is_dir() {
                fs::remove_dir(&resolved)
                    .map_err(|error| ("IO_ERROR", format!("cannot remove dir: {error}")))?;
            } else {
                fs::remove_file(&resolved)
                    .map_err(|error| ("IO_ERROR", format!("cannot remove file: {error}")))?;
            }
        }
        Ok(json!({ "removed": true }))
    }

    fn fs_rename(&self, params: &Value) -> ApiResult {
        let params: FromToParams = parse_params(params)?;
        let from = self.resolve_scoped(&params.from, "filesystem.write")?;
        let to = self.resolve_scoped(&params.to, "filesystem.write")?;
        // The rename is also validated against `filesystem.delete`
        // on the source path: moving out of a granted root is
        // semantically a delete.
        if !self.has_permission_for("filesystem.delete", &from) {
            return Err((
                "PERMISSION_DENIED",
                "rename source requires filesystem.delete".into(),
            ));
        }
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| ("IO_ERROR", format!("cannot create parent: {error}")))?;
        }
        fs::rename(&from, &to).map_err(|error| ("IO_ERROR", format!("cannot rename: {error}")))?;
        Ok(json!({ "renamed": true }))
    }

    fn fs_copy(&self, params: &Value) -> ApiResult {
        let params: FromToParams = parse_params(params)?;
        let from = self.resolve_scoped(&params.from, "filesystem.read")?;
        let to = self.resolve_scoped(&params.to, "filesystem.write")?;
        if from == to {
            return Err(("INVALID_PARAMS", "from and to must differ".into()));
        }
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| ("IO_ERROR", format!("cannot create parent: {error}")))?;
        }
        let metadata = fs::metadata(&from)
            .map_err(|error| ("IO_ERROR", format!("cannot stat source: {error}")))?;
        if metadata.is_dir() {
            copy_dir_recursive(&from, &to)
                .map_err(|error| ("IO_ERROR", format!("cannot copy dir: {error}")))?;
        } else {
            fs::copy(&from, &to)
                .map_err(|error| ("IO_ERROR", format!("cannot copy file: {error}")))?;
        }
        Ok(json!({ "copied": true }))
    }

    fn fs_watch(&self, params: &Value, window_id: Option<u64>) -> ApiResult {
        let params: PathParams = parse_params(params)?;
        let resolved = self.resolve_scoped(&params.path, "filesystem.watch")?;
        let subscription_id = self
            .event_bus
            .subscribe_for_window(
                "filesystem.changed",
                Some(SubscriptionFilter::Path {
                    value: resolved.to_string_lossy().into_owned(),
                }),
                window_id,
            )
            .map_err(|error| ("SUBSCRIBE_FAILED", error.to_string()))?;
        if let Some(registry) = &self.watcher_registry {
            let handle = match registry.watch(&self.manifest.id, &subscription_id, &resolved) {
                Ok(handle) => handle,
                Err(error) => {
                    let _ = self.event_bus.unsubscribe(&subscription_id);
                    return Err(("WATCH_ERROR", error.to_string()));
                }
            };
            self.watch_handles
                .lock()
                .expect("watch handles lock poisoned")
                .insert(subscription_id.clone(), handle);
        }
        Ok(json!({ "subscriptionId": subscription_id, "path": resolved }))
    }

    fn fs_unwatch(&self, params: &Value) -> ApiResult {
        let params: UnsubscribeRequest = parse_params(params)?;
        self.watch_handles
            .lock()
            .expect("watch handles lock poisoned")
            .remove(&params.subscription_id);
        let removed = self
            .event_bus
            .unsubscribe(&params.subscription_id)
            .map_err(|error| ("SUBSCRIBE_FAILED", error.to_string()))?;
        Ok(json!({ "removed": removed }))
    }

    // ------------------------------------------------------------------
    // Storage
    // ------------------------------------------------------------------

    fn storage_get(&self, params: &Value) -> ApiResult {
        self.require_storage()?;
        let params: KeyParams = parse_params(params)?;
        let store = self
            .storage
            .as_ref()
            .ok_or(("STORAGE_UNAVAILABLE", "storage is not available".into()))?;
        Ok(json!({ "value": store.get(&params.key) }))
    }

    fn storage_set(&self, params: &Value) -> ApiResult {
        self.require_storage()?;
        let params: KeyValueParams = parse_params(params)?;
        if params.key.len() > 128 {
            return Err(("INVALID_PARAMS", "key length must be <= 128 bytes".into()));
        }
        let store = self
            .storage
            .as_ref()
            .ok_or(("STORAGE_UNAVAILABLE", "storage is not available".into()))?;
        store
            .set(&params.key, params.value)
            .map_err(|error| ("STORAGE_ERROR", error.to_string()))?;
        Ok(json!({ "written": true }))
    }

    fn storage_delete(&self, params: &Value) -> ApiResult {
        self.require_storage()?;
        let params: KeyParams = parse_params(params)?;
        let store = self
            .storage
            .as_ref()
            .ok_or(("STORAGE_UNAVAILABLE", "storage is not available".into()))?;
        let removed = store
            .delete(&params.key)
            .map_err(|error| ("STORAGE_ERROR", error.to_string()))?;
        Ok(json!({ "removed": removed }))
    }

    fn storage_clear(&self) -> ApiResult {
        self.require_storage()?;
        let store = self
            .storage
            .as_ref()
            .ok_or(("STORAGE_UNAVAILABLE", "storage is not available".into()))?;
        store
            .clear()
            .map_err(|error| ("STORAGE_ERROR", error.to_string()))?;
        Ok(json!({ "cleared": true }))
    }

    fn storage_keys(&self) -> ApiResult {
        self.require_storage()?;
        let store = self
            .storage
            .as_ref()
            .ok_or(("STORAGE_UNAVAILABLE", "storage is not available".into()))?;
        Ok(json!({ "keys": store.keys() }))
    }

    fn require_storage(&self) -> ApiResult {
        let declared = self
            .manifest
            .permissions
            .iter()
            .any(|p| matches!(p, Permission::Storage));
        if !declared {
            return Err((
                "PERMISSION_DENIED",
                "storage is not declared by this package".into(),
            ));
        }
        if !self.permission_granted("storage") {
            return Err(("PERMISSION_DENIED", "storage was revoked".into()));
        }
        Ok(json!({}))
    }

    fn paths_data_dir(&self) -> ApiResult {
        self.require_paths()?;
        let dirs = native::app_paths(&self.manifest.id)
            .map_err(|error| ("NATIVE_ERROR", error.to_string()))?;
        std::fs::create_dir_all(&dirs.data_dir).ok();
        Ok(json!({ "path": dirs.data_dir.to_string_lossy() }))
    }

    fn paths_cache_dir(&self) -> ApiResult {
        self.require_paths()?;
        let dirs = native::app_paths(&self.manifest.id)
            .map_err(|error| ("NATIVE_ERROR", error.to_string()))?;
        std::fs::create_dir_all(&dirs.cache_dir).ok();
        Ok(json!({ "path": dirs.cache_dir.to_string_lossy() }))
    }

    fn paths_temp_dir(&self) -> ApiResult {
        self.require_paths()?;
        let dirs = native::app_paths(&self.manifest.id)
            .map_err(|error| ("NATIVE_ERROR", error.to_string()))?;
        std::fs::create_dir_all(&dirs.temp_dir).ok();
        Ok(json!({ "path": dirs.temp_dir.to_string_lossy() }))
    }

    fn require_paths(&self) -> ApiResult {
        let declared = self
            .manifest
            .permissions
            .iter()
            .any(|p| matches!(p, Permission::Paths));
        if !declared {
            return Err((
                "PERMISSION_DENIED",
                "paths is not declared by this package".into(),
            ));
        }
        if !self.permission_granted("paths") {
            return Err(("PERMISSION_DENIED", "paths was revoked".into()));
        }
        Ok(json!({}))
    }

    // ------------------------------------------------------------------
    // Dialogs
    // ------------------------------------------------------------------

    fn dialog_open_file(&self, params: &Value, multiple: bool, directory: bool) -> ApiResult {
        if directory {
            self.require_permission(
                |permission| matches!(permission, Permission::DialogOpen),
                "dialog.open",
            )?;
        } else {
            self.require_permission(
                |permission| matches!(permission, Permission::DialogOpen),
                "dialog.open",
            )?;
        }
        let params: OpenDialogParams = parse_params(params)?;
        if let Some(title) = params.title.as_ref()
            && title.len() > 200
        {
            return Err(("INVALID_PARAMS", "dialog title is too long".into()));
        }
        let filters = filters_from_params(params.filters.as_ref());
        let spec = OpenDialogSpec {
            title: params.title.clone(),
            default_path: params.default_path.as_deref().map(PathBuf::from),
            filters,
            multiple,
            directory,
        };
        let paths =
            native::pick_paths(spec).map_err(|error| ("NATIVE_ERROR", error.to_string()))?;
        if directory {
            // Directory pick returns paths with full access
            // (read + write). The page can use these to call
            // readBinary / writeText without an extra dialog.
            let minted: Vec<Value> = paths
                .into_iter()
                .map(|p| {
                    mint_token_entry(
                        &self.file_tokens,
                        &self.manifest.id,
                        &p,
                        &[FileOp::Read, FileOp::Write],
                    )
                })
                .collect();
            return Ok(json!({ "paths": minted }));
        }
        if multiple {
            let minted: Vec<Value> = paths
                .into_iter()
                .map(|p| {
                    mint_token_entry(&self.file_tokens, &self.manifest.id, &p, &[FileOp::Read])
                })
                .collect();
            return Ok(json!({ "paths": minted }));
        }
        let Some(first) = paths.into_iter().next() else {
            return Ok(json!({ "path": Value::Null, "token": Value::Null }));
        };
        let minted = mint_token_entry(
            &self.file_tokens,
            &self.manifest.id,
            &first,
            &[FileOp::Read],
        );
        Ok(minted)
    }

    fn dialog_open_files(&self, params: &Value) -> ApiResult {
        self.dialog_open_file(params, true, false)
    }

    fn dialog_open_directory(&self, params: &Value) -> ApiResult {
        self.dialog_open_file(params, false, true)
    }

    fn dialog_save_file(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::DialogSave),
            "dialog.save",
        )?;
        let params: SaveDialogParams = parse_params(params)?;
        if let Some(name) = params.suggested_name.as_ref()
            && name.len() > 200
        {
            return Err(("INVALID_PARAMS", "suggestedName is too long".into()));
        }
        let filters = filters_from_params(params.filters.as_ref());
        let spec = SaveDialogSpec {
            title: params.title.clone(),
            default_path: params.default_path.as_deref().map(PathBuf::from),
            filters,
            suggested_name: params.suggested_name.clone(),
        };
        let chosen =
            native::pick_save_path(spec).map_err(|error| ("NATIVE_ERROR", error.to_string()))?;
        let Some(path) = chosen else {
            return Ok(json!({ "path": Value::Null, "token": Value::Null }));
        };
        let minted = mint_token_entry(
            &self.file_tokens,
            &self.manifest.id,
            &path,
            &[FileOp::Read, FileOp::Write],
        );
        Ok(minted)
    }

    // ------------------------------------------------------------------
    // Runtime
    // ------------------------------------------------------------------

    fn runtime_invoke(
        &self,
        request_id: &str,
        params: &Value,
        deadline_ms: Option<u64>,
    ) -> ApiResult {
        if !self.permission_granted("runtime.invoke")
            || !self
                .manifest
                .permissions
                .iter()
                .any(|permission| matches!(permission, Permission::RuntimeInvoke))
        {
            return Err(("PERMISSION_DENIED", "runtime.invoke is not allowed".into()));
        }
        let params: RuntimeInvokeParams = parse_params(params)?;
        let runtime = self
            .runtime
            .as_ref()
            .ok_or(("RUNTIME_UNAVAILABLE", "application has no backend".into()))?;
        let timeout = deadline_ms
            .map(|deadline| Duration::from_millis(deadline.saturating_sub(now_ms())))
            .map(|timeout| timeout.min(DEFAULT_RUNTIME_TIMEOUT))
            .unwrap_or(DEFAULT_RUNTIME_TIMEOUT);
        // The cancellation token is bound to the IPC request
        // id; the page sends `runtime.cancel { requestId }`
        // and we flip the token. The runtime is unaffected —
        // each call is independent.
        let _ = self.cancel_inflight(request_id);
        runtime
            .invoke(request_id, &params.method, &params.params, timeout)
            .map_err(|error| match error {
                RuntimeError::Timeout(_) => ("DEADLINE_EXCEEDED", error.to_string()),
                _ => ("RUNTIME_FAILURE", error.to_string()),
            })
    }

    fn runtime_status(&self) -> ApiResult {
        self.require_runtime_manage()?;
        let runtime = self
            .runtime
            .as_ref()
            .ok_or(("RUNTIME_UNAVAILABLE", "application has no backend".into()))?;
        runtime
            .status(Duration::from_secs(2))
            .and_then(|status| {
                serde_json::to_value(status)
                    .map_err(|error| RuntimeError::Protocol(error.to_string()))
            })
            .map_err(|error| ("RUNTIME_FAILURE", error.to_string()))
    }

    fn runtime_restart(&self) -> ApiResult {
        self.require_runtime_manage()?;
        let runtime = self
            .runtime
            .as_ref()
            .ok_or(("RUNTIME_UNAVAILABLE", "application has no backend".into()))?;
        runtime
            .restart(Duration::from_secs(5))
            .and_then(|status| {
                serde_json::to_value(status)
                    .map_err(|error| RuntimeError::Protocol(error.to_string()))
            })
            .map_err(|error| ("RUNTIME_FAILURE", error.to_string()))
    }

    fn runtime_cancel(&self, params: &Value) -> ApiResult {
        if !self.permission_granted("runtime.invoke") {
            return Err(("PERMISSION_DENIED", "runtime.invoke was revoked".into()));
        }
        let params: RuntimeCancelParams = parse_params(params)?;
        let cancelled = self.cancel_inflight(&params.request_id);
        Ok(json!({ "cancelled": cancelled }))
    }

    // ------------------------------------------------------------------
    // Clipboard
    // ------------------------------------------------------------------

    fn clipboard_read_text(&self) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::ClipboardRead),
            "clipboard.read",
        )?;
        native::clipboard_read_text()
            .map(|text| json!({ "text": text }))
            .map_err(|error| ("NATIVE_ERROR", error.to_string()))
    }

    fn clipboard_write_text(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::ClipboardWrite),
            "clipboard.write",
        )?;
        let params: ClipboardWriteParams = parse_params(params)?;
        if params.text.len() > MAX_IPC_MESSAGE_BYTES {
            return Err(("INVALID_PARAMS", "clipboard text exceeds 1 MiB".into()));
        }
        native::clipboard_write_text(params.text)
            .map(|_| json!({ "written": true }))
            .map_err(|error| ("NATIVE_ERROR", error.to_string()))
    }

    // ------------------------------------------------------------------
    // System
    // ------------------------------------------------------------------

    fn system_info(&self) -> ApiResult {
        eprintln!("[alex] system_info: enter");
        // The `paths` block is the only host-side state we expose to
        // every app. Other apps never see `system_install_root` etc.
        // because those fields are gated by `system.manageApps`, but
        // `system.info` is callable by any app, so we hand back the
        // resolved host paths only if the caller is a plugin (the
        // same gate as the rest of the `system.*` surface).
        let paths = if self.require_plugin().is_ok() {
            json!({
                "installRoot": self.system_install_root
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(not configured)".to_owned()),
                "trustRoot": self.system_trust_root
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(not configured)".to_owned()),
                "permissionsDir": self.permission_store
                    .as_ref()
                    .map(|s| s.audit_dir().display().to_string())
                    .unwrap_or_else(|| "(not configured)".to_owned()),
                "dataDir": self.permission_store
                    .as_ref()
                    .and_then(|s| s.audit_dir().parent())
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(not configured)".to_owned()),
            })
        } else {
            json!(null)
        };
        let result = json!({
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "alexVersion": env!("CARGO_PKG_VERSION"),
            "protocol": PROTOCOL_VERSION,
            "paths": paths,
        });
        eprintln!("[alex] system_info: returning");
        Ok(result)
    }

    fn system_capabilities(&self) -> ApiResult {
        // Capability negotiation: pages call this once at
        // boot to learn which APIs the current build supports.
        // The split between `available` and `experimental`
        // is honest: `available` is wired end-to-end through
        // the shell, `experimental` is parsed/permission-gated
        // but the host-side native side is still a stub. Pages
        // should branch on `available` for production paths and
        // treat `experimental` as a "the method will accept
        // your call but no side effect happens yet" signal.
        /* let mut available = vec![
            "filesystem.readText",
            "filesystem.readBinary",
            "filesystem.writeText",
            "filesystem.writeBinary",
            "filesystem.exists",
            "filesystem.stat",
            "filesystem.readDir",
            "filesystem.createDir",
            "filesystem.remove",
            "filesystem.rename",
            "filesystem.copy",
            "filesystem.watch",
            "filesystem.unwatch",
            "storage.get",
            "storage.set",
            "storage.delete",
            "storage.clear",
            "storage.keys",
            "paths.dataDir",
            "paths.cacheDir",
            "paths.tempDir",
            "dialog.openFile",
            "dialog.openFiles",
            "dialog.openDirectory",
            "dialog.saveFile",
            "clipboard.readText",
            "clipboard.writeText",
            "system.info",
            "system.capabilities",
            "system.requestPermission",
            "system.openExternal",
            "system.listApps",
            "system.listExtensions",
            "system.install",
            "system.uninstall",
            "system.listPermissions",
            "system.setPermission",
            "system.listTrustedPublishers",
            "system.readAuditLog",
            "window.setTitle",
            "window.minimize",
            "window.maximize",
            "window.close",
            "notification.show",
            "runtime.invoke",
            "runtime.status",
            "runtime.restart",
            "runtime.cancel",
            "events.subscribe",
            "events.unsubscribe",
            "system.container.create",
            "system.container.start",
            "system.container.stop",
            "system.container.restart",
            "system.container.remove",
            "system.container.inspect",
            "system.container.list",
            "system.container.logs",
            "process.spawn",
            "process.kill",
        ];
        if self
            .native_host
            .as_ref()
            .is_some_and(|host| host.supports_secondary_windows())
        {
            available.extend([
                "window.create",
                "window.list",
                "window.getBounds",
                "window.setBounds",
                "window.setFullscreen",
                "window.isFullscreen",
                "window.destroy",
                "menu.setApplicationMenu",
                "menu.setContextMenu",
                "tray.create",
                "tray.destroy",
                "shortcuts.register",
                "shortcuts.unregister",
                "shortcuts.list",
            ]);
        }
        available.push("net.fetch"); */
        let native_desktop = self.native_host.as_ref().is_some_and(|host| host.supports_secondary_windows());
        let available = super::capabilities::available(native_desktop);
        let experimental = super::capabilities::experimental();
        Ok(json!({
            "capabilities": available,
            "experimental": experimental,
        }))
    }

    fn open_external(&self, params: &Value) -> ApiResult {
        let params: OpenExternalParams = parse_params(params)?;
        let parsed = url::Url::parse(&params.url)
            .map_err(|error| ("INVALID_PARAMS", format!("invalid URL: {error}")))?;
        if !matches!(parsed.scheme(), "https" | "http") {
            return Err((
                "INVALID_PARAMS",
                "only http and https URLs are allowed".into(),
            ));
        }
        let origin = parsed.origin().ascii_serialization();
        let allowed = self.manifest.permissions.iter().any(|permission| {
            matches!(permission, Permission::OpenExternal { origins } if origins.iter().any(|item| item == &origin))
        });
        if !allowed {
            return Err((
                "PERMISSION_DENIED",
                format!("system.openExternal is not allowed for {origin}"),
            ));
        }
        if !self.permission_granted("system.openExternal") {
            return Err((
                "PERMISSION_DENIED",
                "system.openExternal was revoked".into(),
            ));
        }
        native::open_external(parsed.as_str())
            .map(|_| json!({ "opened": true }))
            .map_err(|error| ("NATIVE_ERROR", error.to_string()))
    }

    fn request_permission(&self, params: &Value) -> ApiResult {
        let kind = params
            .get("kind")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `kind`".to_owned()))?;
        let method_name = match kind {
            "media.camera" => "media.camera",
            "media.microphone" => "media.microphone",
            "geolocation" => "geolocation",
            other => {
                return Err((
                    "INVALID_PARAMS",
                    format!("unknown permission kind: {other}"),
                ));
            }
        };
        // The manifest must declare the matching WebView-level
        // permission. The page-side shim is a thin wrapper around
        // the host's normal permission flow, so a manifest that
        // does not opt in cannot get a dialog through this path.
        let declared = self.manifest.permissions.iter().any(|permission| {
            matches!(
                (permission, kind),
                (Permission::MediaCamera, "media.camera")
                    | (Permission::MediaMicrophone, "media.microphone")
                    | (Permission::Geolocation, "geolocation")
            )
        });
        if !declared {
            return Err((
                "PERMISSION_DENIED",
                format!("{method_name} is not declared in manifest"),
            ));
        }
        // `permission_granted` already handles the persisted
        // store, the first-use dialog, and the audit log entry
        // — calling `native::confirm_permission` again would
        // show a second dialog and double-write the decision.
        let granted = self.permission_granted(method_name);
        Ok(json!({ "granted": granted }))
    }

    fn system_install(&self, params: &Value) -> ApiResult {
        self.require_plugin()?;
        self.require_permission(
            |permission| matches!(permission, Permission::SystemInstall),
            "system.install",
        )?;
        let install_root = self.system_install_root.as_ref().ok_or_else(|| {
            (
                "OPERATION_FAILED",
                "system install root is not configured for this app".into(),
            )
        })?;
        let package_path = params
            .get("packagePath")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `packagePath`".to_owned()))?;
        let require_signature = params
            .get("requireSignature")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let trusted_key = params.get("trustedKey").and_then(|v| v.as_str());
        crate::package::install_verified(
            std::path::Path::new(package_path),
            install_root,
            require_signature,
            trusted_key,
        )
        .map(|installed| json!({ "installed": installed.display().to_string() }))
        .map_err(|error| ("OPERATION_FAILED", error.to_string()))
    }

    fn system_uninstall(&self, params: &Value) -> ApiResult {
        self.require_plugin()?;
        self.require_permission(
            |permission| matches!(permission, Permission::SystemUninstall),
            "system.uninstall",
        )?;
        let install_root = self.system_install_root.as_ref().ok_or_else(|| {
            (
                "OPERATION_FAILED",
                "system install root is not configured for this app".into(),
            )
        })?;
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `id`".to_owned()))?;
        if id == crate::manager::MANAGER_PLUGIN_ID {
            return Err((
                "OPERATION_FAILED",
                format!("refusing to uninstall the running App Manager ({id})"),
            ));
        }
        crate::package::uninstall(id, install_root)
            .map(|removed| json!({ "removed": removed.display().to_string() }))
            .map_err(|error| ("OPERATION_FAILED", error.to_string()))
    }

    fn system_list_apps(&self) -> ApiResult {
        self.require_plugin()?;
        self.require_permission(
            |permission| matches!(permission, Permission::SystemManageApps),
            "system.manageApps",
        )?;
        let install_root = self.system_install_root.as_ref().ok_or_else(|| {
            (
                "OPERATION_FAILED",
                "system install root is not configured for this app".into(),
            )
        })?;
        let apps = crate::package::list_installed(install_root)
            .map_err(|error| ("OPERATION_FAILED", error.to_string()))?;
        let summary: Vec<_> = apps
            .into_iter()
            .map(|a| {
                json!({
                    "id": a.id,
                    "name": a.name,
                    "version": a.version,
                    "path": a.path.display().to_string(),
                })
            })
            .collect();
        Ok(json!({ "apps": summary }))
    }

    fn require_update_roots(&self) -> Result<(PathBuf, PathBuf), (&'static str, String)> {
        self.require_plugin()?;
        self.require_permission(|permission| matches!(permission, Permission::SystemManageApps), "system.manageApps")?;
        Ok((
            self.system_install_root.clone().ok_or(("OPERATION_FAILED", "system install root is not configured".into()))?,
            self.system_trust_root.clone().ok_or(("OPERATION_FAILED", "system trust root is not configured".into()))?,
        ))
    }

    fn system_update_start(&self, params: &Value) -> ApiResult {
        let (install, trust) = self.require_update_roots()?;
        let id = params.get("id").and_then(Value::as_str).filter(|v| !v.is_empty()).ok_or(("INVALID_PARAMS", "missing `id`".into()))?;
        let url = params.get("manifestUrl").and_then(Value::as_str).filter(|v| !v.is_empty()).ok_or(("INVALID_PARAMS", "missing `manifestUrl`".into()))?;
        let channel = match params.get("channel").and_then(Value::as_str).unwrap_or("stable") {
            "stable" => crate::update::UpdateChannel::Stable,
            "beta" => crate::update::UpdateChannel::Beta,
            "dev" => crate::update::UpdateChannel::Dev,
            _ => return Err(("INVALID_PARAMS", "channel must be stable, beta, or dev".into())),
        };
        serde_json::to_value(crate::core::update_tasks::start(id.into(), url.into(), channel, install, trust)).map_err(|e| ("OPERATION_FAILED", e.to_string()))
    }

    fn system_update_tasks(&self) -> ApiResult {
        let _ = self.require_update_roots()?;
        Ok(json!({ "tasks": crate::core::update_tasks::list() }))
    }

    fn system_update_cancel(&self, params: &Value) -> ApiResult {
        let _ = self.require_update_roots()?;
        let task_id = params.get("taskId").and_then(Value::as_str).ok_or(("INVALID_PARAMS", "missing `taskId`".into()))?;
        Ok(json!({ "cancelled": crate::core::update_tasks::cancel(task_id) }))
    }

    fn system_update_retry(&self, params: &Value) -> ApiResult {
        let _ = self.require_update_roots()?;
        let task_id = params.get("taskId").and_then(Value::as_str).ok_or(("INVALID_PARAMS", "missing `taskId`".into()))?;
        let task = crate::core::update_tasks::retry(task_id).ok_or(("INVALID_STATE", "task is not failed or cancelled".into()))?;
        serde_json::to_value(task).map_err(|e| ("OPERATION_FAILED", e.to_string()))
    }

    fn system_list_extensions(&self) -> ApiResult {
        self.require_plugin()?;
        self.require_permission(
            |permission| matches!(permission, Permission::SystemManageExtensions),
            "system.manageExtensions",
        )?;
        let install_root = self.system_install_root.as_ref().ok_or_else(|| {
            (
                "OPERATION_FAILED",
                "system install root is not configured for this app".into(),
            )
        })?;
        let extensions = crate::plugin::discover_extensions(install_root)
            .map_err(|error| ("OPERATION_FAILED", error.to_string()))?;
        let entries: Vec<_> = extensions
            .into_iter()
            .map(|b| {
                json!({
                    "pluginId": b.plugin_id,
                    "kind": b.extension.kind,
                    "id": b.extension.id,
                    "label": b.extension.label,
                    "entry": b.extension.entry,
                })
            })
            .collect();
        Ok(json!({ "extensions": entries }))
    }

    // ------------------------------------------------------------------
    // system.listPermissions / system.setPermission
    //
    // Read and write the persisted `PermissionStore` decisions for any
    // installed app. The calling plugin is itself trusted (it has
    // `system.managePermissions` and the source identity is bound by
    // `require_plugin()`), so this is an operator-level action — we
    // do not prompt the user again for the right to manage other
    // apps' grants.
    //
    // Both methods deliberately open a fresh `PermissionStore` for
    // the *target* app id rather than reusing the host's own store,
    // because the host's `PermissionStore` is keyed by the host's
    // own id. Transient ("Allow Once") grants live in memory only
    // and are not visible across `PermissionStore::for_app` calls —
    // `listPermissions` reports the persisted decisions; the host's
    // own transient grants are not exposed.
    // ------------------------------------------------------------------

    fn system_list_permissions(&self, params: &Value) -> ApiResult {
        self.require_plugin()?;
        self.require_permission(
            |permission| matches!(permission, Permission::SystemManagePermissions),
            "system.managePermissions",
        )?;
        let app_id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `id`".to_owned()))?;
        if app_id.is_empty() {
            return Err(("INVALID_PARAMS", "`id` must not be empty".into()));
        }
        let store = PermissionStore::for_app(app_id).map_err(|error| {
            (
                "OPERATION_FAILED",
                format!("failed to open permission store: {error}"),
            )
        })?;
        let decisions: Vec<_> = store
            .list()
            .into_iter()
            .map(|(name, decision)| {
                json!({
                    "name": name,
                    "decision": match decision {
                        PermissionDecision::Granted => "granted",
                        PermissionDecision::Denied => "denied",
                        PermissionDecision::Prompt => "prompt",
                    },
                })
            })
            .collect();
        Ok(json!({
            "id": app_id,
            "permissions": decisions,
        }))
    }

    fn system_set_permission(&self, params: &Value) -> ApiResult {
        self.require_plugin()?;
        self.require_permission(
            |permission| matches!(permission, Permission::SystemManagePermissions),
            "system.managePermissions",
        )?;
        let app_id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `id`".to_owned()))?;
        if app_id.is_empty() {
            return Err(("INVALID_PARAMS", "`id` must not be empty".into()));
        }
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `name`".to_owned()))?;
        if name.is_empty() {
            return Err(("INVALID_PARAMS", "`name` must not be empty".into()));
        }
        let decision_str = params
            .get("decision")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `decision`".to_owned()))?;
        let decision = match decision_str {
            "granted" => PermissionDecision::Granted,
            "denied" => PermissionDecision::Denied,
            "prompt" => PermissionDecision::Prompt,
            other => {
                return Err((
                    "INVALID_PARAMS",
                    format!("`decision` must be granted/denied/prompt, got {other}"),
                ));
            }
        };
        let store = PermissionStore::for_app(app_id).map_err(|error| {
            (
                "OPERATION_FAILED",
                format!("failed to open permission store: {error}"),
            )
        })?;
        store
            .set(name, decision)
            .map_err(|error| ("OPERATION_FAILED", error.to_string()))?;
        Ok(json!({ "ok": true }))
    }

    // ------------------------------------------------------------------
    // system.listTrustedPublishers
    //
    // Read-only view of every entry in the local Trust Store. The
    // Trust Store lives at `<system_trust_root>/publishers.json`; if
    // the host did not configure a trust root (older CLI invocations,
    // plain `alex run` without `--root`), the method returns an empty
    // list rather than failing — the absence of a trust store is a
    // valid state ("no publishers trusted yet"), distinct from
    // "trust store exists but is empty".
    //
    // Reuses `system.manageApps` rather than introducing a new
    // permission: the trust store is part of the same app-management
    // surface as install/list/uninstall, and `com.alex.manager` is
    // already pre-granted that.
    // ------------------------------------------------------------------

    fn system_list_trusted_publishers(&self) -> ApiResult {
        eprintln!("[alex] system_list_trusted_publishers: enter");
        self.require_plugin()?;
        self.require_permission(
            |permission| matches!(permission, Permission::SystemManageApps),
            "system.manageApps",
        )?;
        let trust_root = match self.system_trust_root.as_ref() {
            Some(root) => root,
            None => return Ok(json!({ "publishers": [] })),
        };
        let store = match crate::core::trust::TrustStore::open(trust_root) {
            Ok(store) => store,
            Err(crate::core::trust::TrustError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                return Ok(json!({ "publishers": [] }));
            }
            Err(error) => {
                return Err((
                    "OPERATION_FAILED",
                    format!("failed to open trust store: {error}"),
                ));
            }
        };
        let entries: Vec<_> = store
            .list()
            .map(|(fingerprint, publisher)| {
                json!({
                    "fingerprint": fingerprint,
                    "label": publisher.label,
                    "publicKey": publisher.public_key,
                })
            })
            .collect();
        eprintln!(
            "[alex] system_list_trusted_publishers: returning {} entries",
            entries.len()
        );
        Ok(json!({ "publishers": entries }))
    }

    // ------------------------------------------------------------------
    // system.readAuditLog
    //
    // Returns the most recent permission decisions across every
    // installed app. Each app's PermissionStore writes to
    // `<permissions_root>/<app_id>.audit.jsonl`; we walk that
    // directory, parse each line, tag the entry with the owning
    // `appId`, and return the most recent N entries (newest first).
    //
    // Reuses `system.managePermissions`: the same caller that can
    // mutate decisions is the one that needs to *see* them.
    //
    // The result is intentionally capped (`limit`, default 200) so a
    // long-running system with thousands of decisions does not freeze
    // the UI on first paint. Older entries remain on disk and can be
    // re-read by re-issuing the call with a higher limit.
    // ------------------------------------------------------------------

    fn system_read_audit_log(&self, params: &Value) -> ApiResult {
        eprintln!("[alex] system_read_audit_log: enter");
        self.require_plugin()?;
        self.require_permission(
            |permission| matches!(permission, Permission::SystemManagePermissions),
            "system.managePermissions",
        )?;
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(200)
            .clamp(1, 5000) as usize;
        // The directory holding the audit logs is the parent of the
        // permission store's own audit file. We always have a
        // permission store (shell::run wires one in for every app),
        // so the directory is always derivable. As a defensive
        // fallback, try the env-var path the store would have used.
        let directory = match self.permission_store.as_ref() {
            Some(store) => store.audit_dir().to_path_buf(),
            None => match std::env::var_os("ALEX_DATA_DIR")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("LOCALAPPDATA").map(|path| PathBuf::from(path).join("AlexOS"))
                }) {
                Some(root) => root.join("permissions"),
                None => {
                    return Err((
                        "OPERATION_FAILED",
                        "no permission store or data dir is configured".into(),
                    ));
                }
            },
        };
        let read_dir = match fs::read_dir(&directory) {
            Ok(dir) => dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(json!({ "entries": [] }));
            }
            Err(error) => {
                return Err((
                    "OPERATION_FAILED",
                    format!("failed to read audit directory: {error}"),
                ));
            }
        };
        let mut entries: Vec<AuthorizationAuditEntry> = Vec::new();
        for entry in read_dir.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            // PermissionsStore writes to `<app_id>.audit.jsonl`. Skip
            // the non-audit sibling (`<app_id>.json`) and any `.tmp`
            // lockfile left behind by an interrupted write.
            if name.strip_suffix(".audit.jsonl").is_none() {
                continue;
            }
            let Ok(contents) = fs::read_to_string(&path) else {
                continue;
            };
            for line in contents.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(record) = serde_json::from_str::<AuthorizationAuditEntry>(trimmed) {
                    entries.push(record);
                }
            }
        }
        // Newest first; ties broken by appId+permission for stable order.
        entries.sort_by(|a, b| {
            b.timestamp_ms
                .cmp(&a.timestamp_ms)
                .then_with(|| a.app_id.cmp(&b.app_id))
                .then_with(|| a.permission.cmp(&b.permission))
        });
        entries.truncate(limit);
        let values: Vec<Value> = entries
            .into_iter()
            .map(|e| {
                json!({
                    "appId": e.app_id,
                    "permission": e.permission,
                    "decision": match e.decision {
                        PermissionDecision::Granted => "granted",
                        PermissionDecision::Denied => "denied",
                        PermissionDecision::Prompt => "prompt",
                    },
                    "timestampMs": e.timestamp_ms,
                })
            })
            .collect();
        eprintln!(
            "[alex] system_read_audit_log: returning {} entries",
            values.len()
        );
        Ok(json!({ "entries": values }))
    }

    fn container_service(&self) -> Result<&DefaultContainerService, (&'static str, String)> {
        self.require_plugin()?;
        self.require_permission(
            |permission| matches!(permission, Permission::SystemManageApps),
            "system.manageApps",
        )?;
        self.container_service.as_deref().ok_or((
            "CONTAINER_UNAVAILABLE",
            "container service requires a configured system install root".into(),
        ))
    }

    fn container_instance_id<'a>(
        &self,
        params: &'a Value,
    ) -> Result<&'a str, (&'static str, String)> {
        params
            .get("instanceId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or(("INVALID_PARAMS", "missing `instanceId`".into()))
    }

    fn container_result<T: serde::Serialize>(
        result: Result<T, crate::container::ContainerError>,
    ) -> ApiResult {
        result
            .and_then(|value| {
                serde_json::to_value(value)
                    .map_err(|error| crate::container::ContainerError::Backend(error.to_string()))
            })
            .map_err(|error| ("CONTAINER_ERROR", error.to_string()))
    }

    fn system_container_create(&self, params: &Value) -> ApiResult {
        let service = self.container_service()?;
        let request: CreateRequest = parse_params(params)?;
        Self::container_result(service.create(request.into_spec()))
    }

    fn system_container_start(&self, params: &Value) -> ApiResult {
        let service = self.container_service()?;
        Self::container_result(service.start(self.container_instance_id(params)?))
    }

    fn system_container_stop(&self, params: &Value) -> ApiResult {
        let service = self.container_service()?;
        let timeout_ms = params
            .get("timeoutMs")
            .and_then(Value::as_u64)
            .unwrap_or(5_000)
            .clamp(100, 60_000);
        Self::container_result(service.stop(
            self.container_instance_id(params)?,
            Duration::from_millis(timeout_ms),
        ))
    }

    fn system_container_restart(&self, params: &Value) -> ApiResult {
        let service = self.container_service()?;
        Self::container_result(service.restart(self.container_instance_id(params)?))
    }

    fn system_container_remove(&self, params: &Value) -> ApiResult {
        let service = self.container_service()?;
        service
            .remove(
                self.container_instance_id(params)?,
                params
                    .get("deleteData")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            )
            .map(|_| json!({ "removed": true }))
            .map_err(|error| ("CONTAINER_ERROR", error.to_string()))
    }

    fn system_container_inspect(&self, params: &Value) -> ApiResult {
        let service = self.container_service()?;
        Self::container_result(service.inspect(self.container_instance_id(params)?))
    }

    fn system_container_list(&self, params: &Value) -> ApiResult {
        let service = self.container_service()?;
        let filter: ContainerFilter = parse_params(params)?;
        service
            .list(&filter)
            .map(|containers| json!({ "containers": containers }))
            .map_err(|error| ("CONTAINER_ERROR", error.to_string()))
    }

    fn system_container_logs(&self, params: &Value) -> ApiResult {
        let service = self.container_service()?;
        let tail = params
            .get("tail")
            .and_then(Value::as_u64)
            .unwrap_or(200)
            .clamp(1, 5_000) as usize;
        service
            .logs(self.container_instance_id(params)?, tail)
            .map(|entries| json!({ "entries": entries }))
            .map_err(|error| ("CONTAINER_ERROR", error.to_string()))
    }

    // ------------------------------------------------------------------
    // Window
    // ------------------------------------------------------------------

    fn window_set_title(&self, params: &Value) -> ApiResult {
        let params: WindowTitleParams = parse_params(params)?;
        if params.title.is_empty() || params.title.len() > 200 {
            return Err((
                "INVALID_PARAMS",
                "window title must contain 1 to 200 bytes".into(),
            ));
        }
        self.window_command(HostCommand::SetWindowTitle(params.title))
    }

    fn window_command(&self, command: HostCommand) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::WindowManage),
            "window.manage",
        )?;
        self.execute_host(command)
    }

    fn execute_host(&self, command: HostCommand) -> ApiResult {
        self.native_host
            .as_ref()
            .ok_or(("NATIVE_UNAVAILABLE", "window host is unavailable".into()))?
            .execute(command)
            .map(|_| json!({ "accepted": true }))
            .map_err(|error| ("NATIVE_ERROR", error.to_string()))
    }

    fn window_create(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::WindowOpen),
            "window.open",
        )?;
        self.require_secondary_window_host()?;
        let spec: CreateWindowSpec = parse_params(params)?;
        let info = self
            .windows
            .create(&self.manifest.id, spec)
            .map_err(|error| ("WINDOW_ERROR", error.to_string()))?;
        if let Err(error) = self.execute_host(HostCommand::CreateWindow(info.clone())) {
            let _ = self.windows.destroy(&self.manifest.id, info.id);
            return Err(error);
        }
        Ok(serde_json::to_value(info).unwrap_or(Value::Null))
    }

    fn window_list(&self) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::WindowOpen),
            "window.open",
        )?;
        let list = self.windows.list(&self.manifest.id);
        Ok(json!({ "windows": list }))
    }

    fn parse_window_id(&self, params: &Value) -> Result<WindowId, (&'static str, String)> {
        let raw = params
            .get("windowId")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `windowId`".to_owned()))?;
        Ok(WindowId(raw))
    }

    fn window_get_bounds(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::WindowManage),
            "window.manage",
        )?;
        let id = self.parse_window_id(params)?;
        self.windows
            .get(&self.manifest.id, id)
            .map(|info| {
                json!({
                    "windowId": info.id.raw(),
                    "x": info.x,
                    "y": info.y,
                    "width": info.width,
                    "height": info.height,
                })
            })
            .map_err(|error| ("WINDOW_ERROR", error.to_string()))
    }

    fn window_set_bounds(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::WindowManage),
            "window.manage",
        )?;
        self.require_secondary_window_host()?;
        let id = self.parse_window_id(params)?;
        // WindowBounds is `deny_unknown_fields`; strip the
        // `windowId` key before deserializing.
        let mut filtered = params.clone();
        if let Some(object) = filtered.as_object_mut() {
            object.remove("windowId");
        }
        let bounds: WindowBounds = parse_params(&filtered)?;
        let previous = self
            .windows
            .get(&self.manifest.id, id)
            .map_err(|error| ("WINDOW_ERROR", error.to_string()))?;
        let info = self
            .windows
            .set_bounds(&self.manifest.id, id, bounds)
            .map_err(|error| ("WINDOW_ERROR", error.to_string()))?;
        if let Err(error) = self.execute_host(HostCommand::SetWindowBounds(
            id.raw(),
            WindowBounds {
                x: info.x,
                y: info.y,
                width: Some(info.width),
                height: Some(info.height),
            },
        )) {
            let _ = self.windows.set_bounds(
                &self.manifest.id,
                id,
                WindowBounds {
                    x: previous.x,
                    y: previous.y,
                    width: Some(previous.width),
                    height: Some(previous.height),
                },
            );
            return Err(error);
        }
        Ok(serde_json::to_value(info).unwrap_or(Value::Null))
    }

    fn window_set_fullscreen(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::WindowManage),
            "window.manage",
        )?;
        self.require_secondary_window_host()?;
        let id = self.parse_window_id(params)?;
        let value = params
            .get("fullscreen")
            .and_then(|v| v.as_bool())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `fullscreen`".to_owned()))?;
        let previous = self
            .windows
            .get(&self.manifest.id, id)
            .map_err(|error| ("WINDOW_ERROR", error.to_string()))?;
        let info = self
            .windows
            .set_fullscreen(&self.manifest.id, id, value)
            .map_err(|error| ("WINDOW_ERROR", error.to_string()))?;
        if let Err(error) = self.execute_host(HostCommand::SetWindowFullscreen(id.raw(), value)) {
            let _ = self
                .windows
                .set_fullscreen(&self.manifest.id, id, previous.fullscreen);
            return Err(error);
        }
        Ok(serde_json::to_value(info).unwrap_or(Value::Null))
    }

    fn window_is_fullscreen(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::WindowManage),
            "window.manage",
        )?;
        let id = self.parse_window_id(params)?;
        self.windows
            .get(&self.manifest.id, id)
            .map(|info| json!({ "fullscreen": info.fullscreen }))
            .map_err(|error| ("WINDOW_ERROR", error.to_string()))
    }

    fn window_destroy(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::WindowOpen),
            "window.open",
        )?;
        self.require_secondary_window_host()?;
        let id = self.parse_window_id(params)?;
        self.execute_host(HostCommand::DestroyWindow(id.raw()))?;
        self.windows
            .destroy(&self.manifest.id, id)
            .map_err(|error| ("WINDOW_ERROR", error.to_string()))?;
        Ok(json!({ "destroyed": true }))
    }

    fn menu_set_application_menu(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::MenuManage),
            "menu.manage",
        )?;
        let template: MenuTemplate = parse_params(params)?;
        crate::menu_tray::validate_menu_template(&template)
            .map_err(|error| ("MENU_ERROR", error.to_string()))?;
        self.execute_host(HostCommand::SetApplicationMenu(template.clone()))?;
        self.menu_store
            .set_application_menu(&self.manifest.id, template)
            .map_err(|error| ("MENU_ERROR", error.to_string()))?;
        Ok(json!({ "applied": true }))
    }

    fn menu_set_context_menu(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::MenuManage),
            "menu.manage",
        )?;
        let template: MenuTemplate = parse_params(params)?;
        crate::menu_tray::validate_menu_template(&template)
            .map_err(|error| ("MENU_ERROR", error.to_string()))?;
        self.execute_host(HostCommand::SetContextMenu(template.clone()))?;
        self.menu_store
            .set_context_menu(&self.manifest.id, template)
            .map_err(|error| ("MENU_ERROR", error.to_string()))?;
        Ok(json!({ "applied": true }))
    }

    fn tray_create(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::TrayManage),
            "tray.manage",
        )?;
        let spec: TraySpec = parse_params(params)?;
        let info = self
            .menu_store
            .create_tray(&self.manifest.id, spec.clone(), &self.package_root)
            .map_err(|error| ("TRAY_ERROR", error.to_string()))?;
        if let Err(error) = self.execute_host(HostCommand::CreateTray(
            info.id.clone(),
            spec,
            self.package_root.clone(),
        )) {
            let _ = self.menu_store.destroy_tray(&self.manifest.id, &info.id);
            return Err(error);
        }
        Ok(serde_json::to_value(info).unwrap_or(Value::Null))
    }

    fn tray_destroy(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::TrayManage),
            "tray.manage",
        )?;
        let tray_id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `id`".to_owned()))?;
        self.execute_host(HostCommand::DestroyTray(tray_id.to_owned()))?;
        self.menu_store
            .destroy_tray(&self.manifest.id, tray_id)
            .map_err(|error| ("TRAY_ERROR", error.to_string()))?;
        Ok(json!({ "destroyed": true }))
    }

    fn shortcuts_register(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::ShortcutRegister),
            "shortcut.register",
        )?;
        let accelerator = params
            .get("accelerator")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `accelerator`".to_owned()))?;
        self.menu_store
            .register_shortcut(&self.manifest.id, accelerator)
            .map_err(|error| ("SHORTCUT_ERROR", error.to_string()))?;
        let normalized = crate::menu_tray::normalize_accelerator_public(accelerator)
            .map_err(|error| ("SHORTCUT_ERROR", error.to_string()))?;
        if let Err(error) = self.execute_host(HostCommand::RegisterShortcut(normalized)) {
            let _ = self
                .menu_store
                .unregister_shortcut(&self.manifest.id, accelerator);
            return Err(error);
        }
        Ok(json!({ "registered": true }))
    }

    fn shortcuts_unregister(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::ShortcutRegister),
            "shortcut.register",
        )?;
        let accelerator = params
            .get("accelerator")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `accelerator`".to_owned()))?;
        let normalized = crate::menu_tray::normalize_accelerator_public(accelerator)
            .map_err(|error| ("SHORTCUT_ERROR", error.to_string()))?;
        self.execute_host(HostCommand::UnregisterShortcut(normalized))?;
        self.menu_store
            .unregister_shortcut(&self.manifest.id, accelerator)
            .map_err(|error| ("SHORTCUT_ERROR", error.to_string()))?;
        Ok(json!({ "unregistered": true }))
    }

    fn shortcuts_list(&self) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::ShortcutRegister),
            "shortcut.register",
        )?;
        Ok(json!({ "shortcuts": self.menu_store.app_shortcuts(&self.manifest.id) }))
    }

    // ------------------------------------------------------------------
    // Process
    // ------------------------------------------------------------------

    fn process_spawn(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::ProcessSpawn { .. }),
            "process.spawn",
        )?;
        let params: ProcessSpawnParams = parse_params(params)?;
        if params.executable.is_empty() {
            return Err(("INVALID_PARAMS", "executable is empty".into()));
        }
        let executable_path = PathBuf::from(&params.executable);
        // Refuse paths that contain `..` components. The
        // allow-list is enforced against the resolved
        // (package-root-joined) form below.
        if executable_path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err((
                "OPERATION_FORBIDDEN",
                "executable path may not contain '..'".into(),
            ));
        }
        let resolved = if executable_path.is_absolute() {
            executable_path.clone()
        } else {
            self.package_root.join(&executable_path)
        };
        let allowed = self.manifest.permissions.iter().any(|permission| {
            matches!(permission, Permission::ProcessSpawn { executables } if executables.iter().any(|allowed| {
                let allowed_path = PathBuf::from(allowed);
                let resolved_allowed = if allowed_path.is_absolute() {
                    allowed_path
                } else {
                    self.package_root.join(&allowed_path)
                };
                resolved_allowed == resolved
            }))
        });
        if !allowed {
            return Err((
                "PERMISSION_DENIED",
                "executable is not on the process.spawn allow-list".into(),
            ));
        }
        // Build the spec and hand it to the real
        // registry. The registry spawns a `Command` child
        // and starts a reaper thread that drops the
        // entry when the child exits.
        let spec = ProcessSpec {
            executable: params.executable.clone(),
            args: params.args.clone(),
            cwd: params.cwd.clone(),
            timeout_ms: params.timeout_ms,
        };
        self.process_registry
            .spawn(&self.package_root, &spec)
            .map(|info| {
                json!({
                    "pid": info.pid,
                    "executable": params.executable,
                    "args": params.args,
                    "started": true,
                })
            })
            .map_err(|error| ("PROCESS_ERROR", error.to_string()))
    }

    fn process_kill(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::ProcessSpawn { .. }),
            "process.spawn",
        )?;
        let pid = params
            .get("pid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `pid`".to_owned()))?;
        self.process_registry
            .kill(pid)
            .map(|_| json!({ "killed": true }))
            .map_err(|error| ("PROCESS_ERROR", error.to_string()))
    }

    // ------------------------------------------------------------------
    // Network
    // ------------------------------------------------------------------

    fn net_fetch(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::NetworkFetch { .. }),
            "network.fetch",
        )?;
        let params: NetFetchParams = parse_params(params)?;
        if !self.permission_granted("network.fetch") {
            return Err(("PERMISSION_DENIED", "network.fetch was revoked".into()));
        }
        let spec = crate::net::FetchSpec {
            url: params.url,
            method: params.method,
            headers: params.headers,
            body: params.body,
            timeout_ms: params.timeout_ms,
            max_bytes: params.max_bytes,
        };
        let response = crate::net::fetch(&spec, &self.manifest.permissions)
            .map_err(|error| ("NETWORK_ERROR", error.to_string()))?;
        Ok(json!({
            "status": response.status,
            "finalUrl": response.final_url,
            "body": base64::engine::general_purpose::STANDARD.encode(response.body),
        }))
    }

    // ------------------------------------------------------------------
    // Notification
    // ------------------------------------------------------------------

    fn notification_show(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::NotificationShow),
            "notification.show",
        )?;
        let params: NotificationParams = parse_params(params)?;
        if params.title.is_empty() || params.title.len() > 200 || params.body.len() > 1_000 {
            return Err((
                "INVALID_PARAMS",
                "notification title or body exceeds its limit".into(),
            ));
        }
        native::show_notification(&params.title, &params.body)
            .map(|_| json!({ "shown": true }))
            .map_err(|error| ("NATIVE_ERROR", error.to_string()))
    }

    // ------------------------------------------------------------------
    // Events
    // ------------------------------------------------------------------

    fn events_subscribe(
        &self,
        request_id: &str,
        params: &Value,
        window_id: Option<u64>,
    ) -> ApiResult {
        let parsed: SubscribeRequest = serde_json::from_value(params.clone())
            .map_err(|error| ("INVALID_PARAMS", error.to_string()))?;
        let filter = match parsed.filter {
            Some(value) => match serde_json::from_value::<SubscriptionFilter>(value) {
                Ok(filter) => Some(filter),
                Err(error) => {
                    return Err(("INVALID_PARAMS", format!("invalid filter: {error}")));
                }
            },
            None => None,
        };
        let id = self
            .event_bus
            .subscribe_for_window(&parsed.event, filter, window_id)
            .map_err(|error| ("SUBSCRIBE_FAILED", error.to_string()))?;
        let _ = request_id;
        Ok(json!({ "subscriptionId": id, "event": parsed.event }))
    }

    fn events_unsubscribe(&self, params: &Value) -> ApiResult {
        let parsed: UnsubscribeRequest = serde_json::from_value(params.clone())
            .map_err(|error| ("INVALID_PARAMS", error.to_string()))?;
        let removed = self
            .event_bus
            .unsubscribe(&parsed.subscription_id)
            .map_err(|error| ("SUBSCRIBE_FAILED", error.to_string()))?;
        Ok(json!({ "removed": removed }))
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

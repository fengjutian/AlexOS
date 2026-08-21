use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    authorization::{PermissionDecision, PermissionStore},
    ipc::{PROTOCOL_VERSION, Request, Response},
    manifest::AppManifest,
    native::{self, HostCommand, NativeHost},
    permission::Permission,
    runtime::{RuntimeError, RuntimeHandle},
};

const MAX_IPC_MESSAGE_BYTES: usize = 1024 * 1024;
const DEFAULT_RUNTIME_TIMEOUT: Duration = Duration::from_secs(30);

pub struct ApiRouter {
    package_root: PathBuf,
    manifest: AppManifest,
    runtime: Option<RuntimeHandle>,
    permission_store: Option<PermissionStore>,
    native_host: Option<Arc<dyn NativeHost>>,
    system_install_root: Option<PathBuf>,
}

impl ApiRouter {
    pub fn new(package_root: PathBuf, manifest: AppManifest) -> Self {
        let package_root = package_root.canonicalize().unwrap_or(package_root);
        Self {
            package_root,
            manifest,
            runtime: None,
            permission_store: None,
            native_host: None,
            system_install_root: None,
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

    pub fn with_native_host(mut self, host: Arc<dyn NativeHost>) -> Self {
        self.native_host = Some(host);
        self
    }

    /// System-wide install root. Only consulted by `system.*` methods
    /// (which require `kind: "plugin"`). Apps that don't call into
    /// `system.*` never see this.
    pub fn with_system_install_root(mut self, root: PathBuf) -> Self {
        self.system_install_root = Some(root);
        self
    }

    /// Restart the attached backend runtime, if any. Returns `None` when no
    /// runtime was attached. Used by `alex dev` to pick up backend code
    /// changes without restarting the shell. Additive — does not change
    /// existing dispatch behavior.
    pub fn restart_runtime(&self, timeout: Duration) -> Option<Result<(), RuntimeError>> {
        self.runtime
            .as_ref()
            .map(|handle| handle.restart(timeout).map(|_| ()))
    }

    pub fn dispatch_json(&self, input: &str) -> Response {
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
        self.dispatch(request)
    }

    pub fn dispatch(&self, request: Request) -> Response {
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

        let result = match request.method.as_str() {
            "filesystem.readText" => self.read_text(&request.params),
            "filesystem.writeText" => self.write_text(&request.params),
            "system.info" => Ok(json!({
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "alexVersion": env!("CARGO_PKG_VERSION")
            })),
            "clipboard.readText" => self.clipboard_read_text(),
            "clipboard.writeText" => self.clipboard_write_text(&request.params),
            "dialog.openFile" => self.dialog_open_file(&request.params),
            "system.openExternal" => self.open_external(&request.params),
            "window.setTitle" => self.window_set_title(&request.params),
            "window.minimize" => self.window_command(HostCommand::MinimizeWindow),
            "window.maximize" => self.window_command(HostCommand::MaximizeWindow),
            "window.close" => self.window_command(HostCommand::CloseWindow),
            "notification.show" => self.notification_show(&request.params),
            "system.requestPermission" => self.request_permission(&request.params),
            "system.install" => self.system_install(&request.params),
            "system.uninstall" => self.system_uninstall(&request.params),
            "system.listApps" => self.system_list_apps(),
            "system.listExtensions" => self.system_list_extensions(),
            "runtime.invoke" => {
                self.runtime_invoke(&request.id, &request.params, request.deadline_ms)
            }
            "runtime.status" => self.runtime_status(),
            "runtime.restart" => self.runtime_restart(),
            "runtime.cancel" => self.runtime_cancel(),
            _ => Err(("METHOD_NOT_FOUND", "unknown Alex API method".to_owned())),
        };

        match result {
            Ok(value) => Response::success(request.id, value),
            Err((code, message)) => Response::error(request.id, code, message),
        }
    }

    fn read_text(&self, params: &Value) -> ApiResult {
        let params: PathParams = parse_params(params)?;
        let requested = self.resolve_requested(&params.path);
        if !self.permission_granted("filesystem.read")
            || !self.manifest.permissions.iter().any(|permission| {
                permission.allows_path("filesystem.read", &self.package_root, &requested)
            })
        {
            return Err(("PERMISSION_DENIED", "filesystem.read is not allowed".into()));
        }
        fs::read_to_string(&requested)
            .map(|content| json!({ "content": content }))
            .map_err(|error| ("IO_ERROR", error.to_string()))
    }

    fn write_text(&self, params: &Value) -> ApiResult {
        let params: WriteParams = parse_params(params)?;
        let requested = self.resolve_requested(&params.path);
        if !self.permission_granted("filesystem.write")
            || !self.manifest.permissions.iter().any(|permission| {
                permission.allows_path("filesystem.write", &self.package_root, &requested)
            })
        {
            return Err((
                "PERMISSION_DENIED",
                "filesystem.write is not allowed".into(),
            ));
        }
        fs::write(&requested, params.content)
            .map(|_| json!({ "written": true }))
            .map_err(|error| ("IO_ERROR", error.to_string()))
    }

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
        runtime
            .invoke(request_id, &params.method, &params.params, timeout)
            .map_err(|error| match error {
                RuntimeError::Timeout(_) => ("DEADLINE_EXCEEDED", error.to_string()),
                _ => ("RUNTIME_FAILURE", error.to_string()),
            })
    }

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

    fn dialog_open_file(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::DialogOpen),
            "dialog.open",
        )?;
        let params: DialogOpenParams = parse_params(params)?;
        if params.title.as_ref().is_some_and(|title| title.len() > 200) {
            return Err(("INVALID_PARAMS", "dialog title is too long".into()));
        }
        native::pick_file(params.title.as_deref())
            .map(|path| json!({ "path": path.map(|value| value.to_string_lossy().into_owned()) }))
            .map_err(|error| ("NATIVE_ERROR", error.to_string()))
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
        self.native_host
            .as_ref()
            .ok_or(("NATIVE_UNAVAILABLE", "window host is unavailable".into()))?
            .execute(command)
            .map(|_| json!({ "accepted": true }))
            .map_err(|error| ("NATIVE_ERROR", error.to_string()))
    }

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

    fn require_plugin(&self) -> ApiResult {
        if self.manifest.kind != crate::manifest::PackageKind::Plugin {
            return Err((
                "PERMISSION_DENIED",
                "system methods are reserved for plugins".into(),
            ));
        }
        Ok(json!({}))
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
            .unwrap_or(false);
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

    /// Front-end calls this before invoking `getUserMedia` /
    /// `getCurrentPosition`. WebView2 does not surface a permission prompt
    /// for these; we model the gate as an explicit IPC so the user
    /// always sees a native confirmation tied to the app's manifest.
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
        self.require_permission(
            |permission| {
                matches!(
                    (permission, kind),
                    (Permission::MediaCamera, "media.camera")
                        | (Permission::MediaMicrophone, "media.microphone")
                        | (Permission::Geolocation, "geolocation")
                )
            },
            method_name,
        )?;
        let granted = native::confirm_permission(&self.manifest.name, method_name)
            .map_err(|error| ("NATIVE_ERROR", error.to_string()))?;
        // Persist the user's choice so subsequent calls don't re-prompt
        // until they explicitly revoke via `alex permissions revoke`.
        if let Some(store) = &self.permission_store {
            let decision = if granted {
                crate::authorization::PermissionDecision::Granted
            } else {
                crate::authorization::PermissionDecision::Denied
            };
            store
                .set(method_name, decision)
                .map_err(|error| ("AUTHORIZATION_ERROR", error.to_string()))?;
        }
        Ok(json!({ "granted": granted }))
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

    fn runtime_cancel(&self) -> ApiResult {
        if !self.permission_granted("runtime.invoke") {
            return Err(("PERMISSION_DENIED", "runtime.invoke was revoked".into()));
        }
        let runtime = self
            .runtime
            .as_ref()
            .ok_or(("RUNTIME_UNAVAILABLE", "application has no backend".into()))?;
        Ok(json!({ "cancelled": runtime.cancel() }))
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
            return true;
        };
        match store.decision(name) {
            PermissionDecision::Granted => true,
            PermissionDecision::Denied => false,
            PermissionDecision::Prompt => {
                let granted =
                    native::confirm_permission(&self.manifest.name, name).unwrap_or(false);
                let decision = if granted {
                    PermissionDecision::Granted
                } else {
                    PermissionDecision::Denied
                };
                let _ = store.set(name, decision);
                granted
            }
        }
    }

    fn resolve_requested(&self, path: &str) -> PathBuf {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            path
        } else {
            self.package_root.join(path)
        }
    }
}

type ApiResult = Result<Value, (&'static str, String)>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathParams {
    path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteParams {
    path: String,
    content: String,
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
struct ClipboardWriteParams {
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DialogOpenParams {
    #[serde(default)]
    title: Option<String>,
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

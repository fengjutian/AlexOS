use std::{
    fs,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    authorization::{PermissionDecision, PermissionStore},
    ipc::{PROTOCOL_VERSION, Request, Response},
    manifest::AppManifest,
    native,
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
}

impl ApiRouter {
    pub fn new(package_root: PathBuf, manifest: AppManifest) -> Self {
        let package_root = package_root.canonicalize().unwrap_or(package_root);
        Self {
            package_root,
            manifest,
            runtime: None,
            permission_store: None,
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
            "runtime.invoke" => {
                self.runtime_invoke(&request.id, &request.params, request.deadline_ms)
            }
            "runtime.status" => self.runtime_status(),
            "runtime.restart" => self.runtime_restart(),
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

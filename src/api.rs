use std::{
    fs,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    ipc::{PROTOCOL_VERSION, Request, Response},
    manifest::AppManifest,
    permission::Permission,
    runtime::{RuntimeError, RuntimeHandle, RuntimeProcess},
};

const MAX_IPC_MESSAGE_BYTES: usize = 1024 * 1024;
const DEFAULT_RUNTIME_TIMEOUT: Duration = Duration::from_secs(30);

pub struct ApiRouter {
    package_root: PathBuf,
    manifest: AppManifest,
    runtime: Option<RuntimeHandle>,
}

impl ApiRouter {
    pub fn new(package_root: PathBuf, manifest: AppManifest) -> Self {
        let package_root = package_root.canonicalize().unwrap_or(package_root);
        Self {
            package_root,
            manifest,
            runtime: None,
        }
    }

    pub fn with_runtime(mut self, runtime: RuntimeProcess) -> Self {
        self.runtime = Some(RuntimeHandle::spawn(runtime));
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
            "runtime.invoke" => {
                self.runtime_invoke(&request.id, &request.params, request.deadline_ms)
            }
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
        if !self.manifest.permissions.iter().any(|permission| {
            permission.allows_path("filesystem.read", &self.package_root, &requested)
        }) {
            return Err(("PERMISSION_DENIED", "filesystem.read is not allowed".into()));
        }
        fs::read_to_string(&requested)
            .map(|content| json!({ "content": content }))
            .map_err(|error| ("IO_ERROR", error.to_string()))
    }

    fn write_text(&self, params: &Value) -> ApiResult {
        let params: WriteParams = parse_params(params)?;
        let requested = self.resolve_requested(&params.path);
        if !self.manifest.permissions.iter().any(|permission| {
            permission.allows_path("filesystem.write", &self.package_root, &requested)
        }) {
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
        if !self
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

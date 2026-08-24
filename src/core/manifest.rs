use std::{collections::BTreeMap, path::Path};

use serde::{Deserialize, Serialize};

use crate::{AlexError, permission::Permission};


#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppManifest {
    pub schema_version: u32,
    #[serde(default, rename = "kind")]
    pub kind: PackageKind,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<Author>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icons: Option<Icons>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<UpdateSource>,
    pub frontend: Frontend,
    #[serde(default)]
    pub backend: Option<Backend>,
    #[serde(default)]
    pub permissions: Vec<Permission>,
    /// Plugin 静态声明的扩展点(命令 / 面板 / 菜单)。
    /// 0.1 切片 3:只解析和聚合,host 不主动调用(那是 0.2 的事)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_points: Option<Vec<ExtensionPoint>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateSource {
    pub manifest_url: String,
    #[serde(default = "default_update_channel")]
    pub channel: String,
}

fn default_update_channel() -> String {
    "stable".into()
}

/// 0.1 引入的字段。`App` 是默认(向后兼容),`Plugin` 启用扩展点挂载。
/// schemaVersion 不 bump — 老 manifest 仍然能跑。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PackageKind {
    #[default]
    App,
    Plugin,
}

/// Plugin 静态声明的扩展点。`entry` 是 plugin backend 暴露的方法名,
/// host 通过 system permission 调用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionPoint {
    pub kind: ExtensionKind,
    pub id: String,
    pub label: String,
    pub entry: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ExtensionKind {
    Command,
    Panel,
    Menu,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, transparent)]
pub struct Icons {
    pub entries: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Author {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frontend {
    pub entry: String,
    /// Optional build descriptor. When present, `alex build`
    /// shells out to `command` with `args` from the
    /// `frontend/` directory so frameworks like Vite can
    /// bundle source files into the single `entry` the host
    /// serves at runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<FrontendBuild>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrontendBuild {
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
}

/// Backend execution mode.
///
/// `Rpc` is the default: Node receives a single JSON Lines request on
/// stdin and writes a single response on stdout (current 0.1 behaviour,
/// see `src/runtime.rs`).
///
/// `Service` is a long-running backend (Express, WebSocket, SQLite,
/// background jobs). The host allocates a private `127.0.0.1` port and
/// injects it via `ALEX_SERVICE_PORT`, gives the backend a per-launch
/// token via `ALEX_RUNTIME_TOKEN`, and waits for the backend to report
/// readiness via a stderr JSON line:
///
/// ```text
/// {"type":"alex.ready","port":<bound_port>}
/// ```
///
/// All `service` backends must expose a health-check endpoint (default
/// `GET /health`, configurable via `healthCheck.path`) and listen on
/// `127.0.0.1` only.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BackendMode {
    #[default]
    Rpc,
    Service,
}

/// Optional HTTP health check for a `service` backend. The host polls
/// `path` (default `/health`) for `200 OK` after receiving the
/// `alex.ready` signal. If `timeout_ms` elapses without a 200, the
/// backend is marked `unhealthy` and the restart policy decides what
/// happens next.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HealthCheck {
    #[serde(default = "HealthCheck::default_path")]
    pub path: String,
    #[serde(default = "HealthCheck::default_timeout_ms")]
    pub timeout_ms: u64,
}

impl Default for HealthCheck {
    fn default() -> Self {
        Self {
            path: Self::default_path(),
            timeout_ms: Self::default_timeout_ms(),
        }
    }
}

impl HealthCheck {
    fn default_path() -> String {
        "/health".into()
    }
    fn default_timeout_ms() -> u64 {
        10_000
    }
}

/// Restart policy for a backend. `policy` is one of:
///
/// - `never`: no auto-restart;
/// - `on-failure` (default): restart only on non-zero exit;
/// - `always`: restart on any exit.
///
/// `max_retries` caps the count inside a host-defined sliding window.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestartPolicy {
    #[serde(default = "RestartPolicy::default_policy")]
    pub policy: String,
    #[serde(default = "RestartPolicy::default_max_retries")]
    pub max_retries: u32,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            policy: Self::default_policy(),
            max_retries: Self::default_max_retries(),
        }
    }
}

impl RestartPolicy {
    fn default_policy() -> String {
        "on-failure".into()
    }
    fn default_max_retries() -> u32 {
        5
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Backend {
    pub runtime: RuntimeKind,
    pub entry: String,
    /// Execution mode. Defaults to `rpc` for backward compatibility.
    #[serde(default)]
    pub mode: BackendMode,
    /// Optional HTTP health check for `service` mode. Defaults are
    /// applied per-field when the struct is present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_check: Option<HealthCheck>,
    /// Optional restart policy. Defaults are applied per-field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<RestartPolicy>,
    /// Optional fixed service port. If absent, the host allocates one
    /// from the private 28000–28999 range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Extra command-line arguments appended after `entry`. The
    /// host spawns `<runtime> <entry> <args...>`. v1 manifests
    /// leave this empty; the Phase 2 multi-service supervisor
    /// uses it to project v2 `ServiceSpec.args` onto a
    /// `Backend` so the lower-level `RuntimeHandle` can launch
    /// a service that takes its own CLI flags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Per-launch environment variables. The host injects these
    /// alongside the framework-managed `ALEX_*` set. v1 manifests
    /// leave this empty; v2 services may declare arbitrary
    /// `env:` entries that the supervisor forwards.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeKind {
    /// Node.js backend. The host locates a `node` binary on
    /// `PATH` (or honours `ALEX_NODE`) and runs
    /// `node <entry> <args...>`.
    Node,
    /// Python backend. Phase 7's managed Python runtime lands
    /// the actual implementation; today the supervisor surfaces
    /// a clear "not yet supported" error if a v2 service
    /// declares this runtime.
    Python,
    /// Native executable. The host runs `<entry> <args...>`
    /// directly, without an interpreter.
    Native,
}

impl AppManifest {
    pub fn validate(&self, root: &Path) -> Result<(), AlexError> {
        if self.schema_version != 1 {
            return Err(AlexError::Validation(format!(
                "unsupported schemaVersion {}; expected 1",
                self.schema_version
            )));
        }
        if !valid_id(&self.id) {
            return Err(AlexError::Validation(format!(
                "invalid package id {:?}; use reverse-domain components",
                self.id
            )));
        }
        validate_relative_entry(root, &self.frontend.entry, "frontend")?;
        if let Some(backend) = &self.backend {
            validate_relative_entry(root, &backend.entry, "backend")?;
        }
        Ok(())
    }
}

fn valid_id(id: &str) -> bool {
    id.contains('.')
        && id.split('.').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
}

fn validate_relative_entry(root: &Path, entry: &str, kind: &str) -> Result<(), AlexError> {
    let entry_path = Path::new(entry);
    if entry_path.is_absolute()
        || entry_path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(AlexError::Validation(format!(
            "{kind} entry must stay inside the package"
        )));
    }
    if !root.join(entry_path).is_file() {
        return Err(AlexError::Validation(format!(
            "{kind} entry does not exist: {entry}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod backend_mode_tests {
    use super::*;
    use serde_json::json;

    /// Pre-0.1 backend blocks (no `mode` / `healthCheck` / `restart`
    /// / `port` fields) must still parse — the new fields all carry
    /// per-field `#[serde(default)]`.
    #[test]
    fn legacy_backend_block_still_parses_as_rpc() {
        let value = json!({
            "runtime": "node",
            "entry": "backend/index.js",
        });
        let backend: Backend = serde_json::from_value(value).expect("legacy backend parses");
        assert_eq!(backend.mode, BackendMode::Rpc);
        assert!(backend.health_check.is_none());
        assert!(backend.restart.is_none());
        assert!(backend.port.is_none());
    }

    #[test]
    fn service_backend_parses_with_health_check_and_restart() {
        let value = json!({
            "runtime": "node",
            "entry": "backend/index.js",
            "mode": "service",
            "healthCheck": { "path": "/livez", "timeoutMs": 3000 },
            "restart": { "policy": "always", "maxRetries": 9 },
            "port": 28100,
        });
        let backend: Backend = serde_json::from_value(value).expect("service backend parses");
        assert_eq!(backend.mode, BackendMode::Service);
        let health = backend.health_check.expect("health_check present");
        assert_eq!(health.path, "/livez");
        assert_eq!(health.timeout_ms, 3000);
        let restart = backend.restart.expect("restart present");
        assert_eq!(restart.policy, "always");
        assert_eq!(restart.max_retries, 9);
        assert_eq!(backend.port, Some(28100));
    }

    #[test]
    fn health_check_defaults_apply_when_struct_is_partial() {
        let value = json!({ "path": "/readyz" });
        let health: HealthCheck = serde_json::from_value(value).expect("partial health parses");
        assert_eq!(health.path, "/readyz");
        assert_eq!(health.timeout_ms, 10_000);
    }

    #[test]
    fn health_check_rejects_unknown_fields() {
        let value = json!({ "path": "/health", "method": "POST" });
        let result = serde_json::from_value::<HealthCheck>(value);
        assert!(result.is_err(), "unknown fields must be rejected");
    }

    #[test]
    fn backend_rejects_unknown_fields() {
        let value = json!({
            "runtime": "node",
            "entry": "backend/index.js",
            "mystery": true,
        });
        let result = serde_json::from_value::<Backend>(value);
        assert!(result.is_err(), "unknown Backend fields must be rejected");
    }

    #[test]
    fn mode_serializes_lowercase() {
        assert_eq!(
            serde_json::to_value(BackendMode::Rpc).unwrap(),
            json!("rpc")
        );
        assert_eq!(
            serde_json::to_value(BackendMode::Service).unwrap(),
            json!("service")
        );
    }
}

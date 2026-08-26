//! Unified application manifest view for Alex OS.
//!
//! The codebase historically carried two manifest formats side by side:
//!
//! * **v1** (`manifest.json`, parsed by [`crate::manifest`]) is the
//!   desktop-oriented format. A package has at most one Node.js
//!   backend and one frontend entry. Permissions are declared as
//!   flat IPC-method keys.
//! * **v2** (`app.yaml`, parsed by [`crate::manifest_v2`]) is the
//!   runtime-oriented format. A package may declare multiple
//!   services with explicit dependency edges, runtime version
//!   requirements, and policy-style permission sections.
//!
//! Both shapes stay loadable. The rest of the host — App Manager,
//! Daemon, Supervisor, the App Manager UI — wants a single, stable
//! description that hides the schema split. [`ApplicationManifest`]
//! is that description.
//!
//! Phase 1 of the multi-service roadmap introduces this type without
//! yet moving the call sites; downstream layers (Phase 1.4 in
//! `docs/roadmap/manifest-unification.md`) will switch from
//! `crate::load_app` to [`load_application`] in subsequent PRs.
//!
//! ## v1 → service mapping
//!
//! A v1 manifest is projected into the service list as if it had a
//! single service named [`ServiceDescriptor::V1_MAIN_SERVICE`]
//! (the literal string `"main"`), with no dependencies, default
//! `OnFailure`/5-retry restart policy, and a single HTTP health
//! check when the legacy `mode: service` block is present. The
//! legacy `mode: rpc` backend has no health check in the unified
//! view; the supervisor continues to look at it as a request/
//! response runtime for now.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde::Serialize;
use thiserror::Error;

use crate::{
    manifest::{
        AppManifest as AppManifestV1, Backend, BackendMode, HealthCheck, RestartPolicy,
        RuntimeKind as RuntimeKindV1, UpdateSource,
    },
    manifest_v2::{
        ApplicationManifestV2, HealthKind, ManifestV2Error, RestartPolicyV2, RuntimeRequirements,
        ServiceHealth, ServicePort, ServiceResources, ServiceRuntime, ServiceSpec,
    },
};

/// Hard cap on the size of any single manifest file. 1 MiB is
/// roughly four orders of magnitude larger than the largest
/// realistic v1/v2 manifest Alex OS has ever seen (the actual
/// upper bound is dominated by the permission allow-list), so this
/// is purely a defence against accidental blow-ups (e.g. a binary
/// file dropped into a manifest).
pub const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("package I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("package declares both manifest.json and app.yaml; expected exactly one")]
    BothManifests,
    #[error("package declares neither manifest.json nor app.yaml in {0}")]
    MissingManifest(PathBuf),
    #[error("manifest exceeds {MAX_MANIFEST_BYTES} bytes")]
    ManifestTooLarge,
    #[error("invalid application manifest: {0}")]
    Invalid(String),
}

impl From<ManifestV2Error> for ManifestError {
    fn from(error: ManifestV2Error) -> Self {
        match error {
            ManifestV2Error::Io(error) => Self::Io(error),
            other => Self::Invalid(other.to_string()),
        }
    }
}

impl From<crate::AlexError> for ManifestError {
    fn from(error: crate::AlexError) -> Self {
        match error {
            crate::AlexError::Read { source, .. } => Self::Io(source),
            crate::AlexError::Manifest { source, .. } => Self::Invalid(source.to_string()),
            other => Self::Invalid(other.to_string()),
        }
    }
}

/// Unified application manifest. The discriminant encodes the
/// underlying schema version so callers that genuinely need the
/// raw fields (e.g. the App Manager detail view) can drop down via
/// [`Self::as_v1`] / [`Self::as_v2`].
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "version", rename_all = "lowercase")]
pub enum ApplicationManifest {
    V1(AppManifestV1),
    V2(ApplicationManifestV2),
}

impl ApplicationManifest {
    /// Reverse-domain application id (`com.example.foo`).
    pub fn id(&self) -> &str {
        match self {
            Self::V1(m) => &m.id,
            Self::V2(m) => &m.id,
        }
    }

    /// Human-readable display name. v1's `name` and v2's `name`
    /// are both required strings.
    pub fn name(&self) -> &str {
        match self {
            Self::V1(m) => &m.name,
            Self::V2(m) => &m.name,
        }
    }

    /// `semver` version string. v2 mandates a parseable value at
    /// load time; v1 stores it verbatim.
    pub fn version(&self) -> &str {
        match self {
            Self::V1(m) => &m.version,
            Self::V2(m) => &m.version,
        }
    }

    /// Human-readable description. Only v1 has a `description`
    /// field today; v2 returns `None` until the schema adds one.
    pub fn description(&self) -> Option<&str> {
        match self {
            Self::V1(m) => m.description.as_deref(),
            Self::V2(_) => None,
        }
    }

    /// Web entry the host serves inside the WebView. `None` for
    /// headless v2 applications.
    pub fn frontend(&self) -> Option<FrontendDescriptor> {
        match self {
            Self::V1(m) => Some(FrontendDescriptor {
                entry: m.frontend.entry.clone(),
            }),
            Self::V2(m) => m.frontend.as_ref().map(|f| FrontendDescriptor {
                entry: f.entry.clone(),
            }),
        }
    }

    /// All declared services. For v1 this is either a single
    /// `main` service (when a `backend` block is present) or an
    /// empty list (frontend-only desktop apps). For v2 this is the
    /// declared service map, in declaration order.
    pub fn services(&self) -> Vec<ServiceDescriptor> {
        match self {
            Self::V1(m) => match &m.backend {
                Some(backend) => vec![v1_backend_to_service(backend)],
                None => Vec::new(),
            },
            Self::V2(m) => m
                .services
                .iter()
                .map(|(name, spec)| v2_service_to_descriptor(name, spec))
                .collect(),
        }
    }

    /// Declared permission identifiers. v1 emits one descriptor
    /// per declared IPC method (`"filesystem.read"`,
    /// `"runtime.invoke"`, ...). v2 emits one descriptor per
    /// declared policy entry, with a stable synthetic name
    /// (`fs:read:<path>`, `net:allow:<origin>`, `shell:allow:<cmd>`)
    /// so downstream code can keep matching on a single string
    /// without caring about the source schema.
    pub fn permissions(&self) -> Vec<PermissionDescriptor> {
        match self {
            Self::V1(m) => m
                .permissions
                .iter()
                .map(|p| PermissionDescriptor {
                    name: p.name().to_string(),
                })
                .collect(),
            Self::V2(m) => {
                let mut out = Vec::new();
                for path in &m.permissions.filesystem.read {
                    out.push(PermissionDescriptor {
                        name: format!("fs:read:{path}"),
                    });
                }
                for path in &m.permissions.filesystem.write {
                    out.push(PermissionDescriptor {
                        name: format!("fs:write:{path}"),
                    });
                }
                for origin in &m.permissions.network.allow {
                    out.push(PermissionDescriptor {
                        name: format!("net:allow:{origin}"),
                    });
                }
                for command in &m.permissions.shell.allow {
                    out.push(PermissionDescriptor {
                        name: format!("shell:allow:{command}"),
                    });
                }
                out
            }
        }
    }

    /// Update source URL and channel, when declared. v2 does not
    /// currently model this; v1 packages opt in via
    /// `manifest.update`. Returns `None` for v2.
    pub fn update_source(&self) -> Option<UpdateSource> {
        match self {
            Self::V1(m) => m.update.clone(),
            Self::V2(_) => None,
        }
    }

    /// Schema version as a string. `"1"` or `"2"`.
    pub fn schema_version(&self) -> &'static str {
        match self {
            Self::V1(_) => "1",
            Self::V2(_) => "2",
        }
    }

    /// True when the manifest declares at least one service. A
    /// v1 manifest without a `backend` block is considered a
    /// frontend-only desktop app and answers `false`.
    pub fn has_services(&self) -> bool {
        match self {
            Self::V1(m) => m.backend.is_some(),
            Self::V2(m) => !m.services.is_empty(),
        }
    }

    pub fn as_v1(&self) -> Option<&AppManifestV1> {
        match self {
            Self::V1(m) => Some(m),
            Self::V2(_) => None,
        }
    }

    pub fn as_v2(&self) -> Option<&ApplicationManifestV2> {
        match self {
            Self::V1(_) => None,
            Self::V2(m) => Some(m),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FrontendDescriptor {
    pub entry: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceDescriptor {
    /// Service identifier, unique within an application. For v1
    /// the only legal value is `"main"` (see
    /// [`ServiceDescriptor::V1_MAIN_SERVICE`]). For v2 the value
    /// is the map key in `app.yaml`'s `services` block.
    pub name: String,
    pub runtime: ServiceRuntime,
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Concrete TCP port, when fixed. `None` means "host picks a
    /// free port from the private service range" — this collapses
    /// the v2 `ServicePort::Auto` variant for downstream code that
    /// does not need to distinguish.
    pub port: Option<u16>,
    /// Execution mode. v2 manifests with an HTTP health check
    /// imply `Service`; v1 single-backend apps may set
    /// `Service` without declaring a health check, in which
    /// case the supervisor still waits for the
    /// `alex.ready` handshake and uses the default `/health`
    /// polling endpoint. The default `Rpc` covers the
    /// request/response backends that do not run an HTTP
    /// server.
    pub mode: ServiceMode,
    pub health: Option<ServiceHealthDescriptor>,
    pub restart: ServiceRestartDescriptor,
    /// Per-service resource quotas. `None` means the host applies
    /// no quota. Only v2 manifests can declare these today; v1
    /// backends project `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ServiceResourcesDescriptor>,
}

impl ServiceDescriptor {
    /// The single service name a v1 `backend` block is projected
    /// onto.
    pub const V1_MAIN_SERVICE: &'static str = "main";
}

/// Execution mode for a service. Mirrors the v1 `BackendMode`
/// split so a v1 `Backend` projected into a `ServiceDescriptor`
/// (or vice versa) does not lose the rpc/service distinction.
/// v2 manifests can leave the default `Rpc` and the supervisor
/// will switch to `Service` automatically when an HTTP health
/// check is declared.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServiceMode {
    /// Request/response backend. The host sends a single
    /// JSON Lines request on stdin and reads a single
    /// response on stdout.
    #[default]
    Rpc,
    /// Long-running backend (Express, WebSocket, etc.). The
    /// host waits for the `alex.ready` handshake and then
    /// polls an HTTP health endpoint.
    Service,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceHealthDescriptor {
    pub kind: ServiceHealthKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub interval_ms: u64,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServiceHealthKind {
    /// Backend is healthy as long as the process is alive. The
    /// host does not speak to it.
    Process,
    /// Host polls `path` on the loopback port allocated for the
    /// service; `2xx` is healthy.
    Http,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceRestartDescriptor {
    pub policy: ServiceRestartPolicy,
    pub max_retries: u32,
}

#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceRestartPolicy {
    Never,
    #[default]
    OnFailure,
    Always,
}

impl Default for ServiceRestartDescriptor {
    fn default() -> Self {
        Self {
            policy: ServiceRestartPolicy::OnFailure,
            max_retries: 5,
        }
    }
}

/// Per-service resource quotas in the unified service view. Mirrors
/// [`crate::manifest_v2::ServiceResources`] so a resolved service
/// carries the same four quota fields regardless of schema version.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceResourcesDescriptor {
    /// Hard memory cap in MiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u32>,
    /// CPU share (0-100), a percentage of one CPU.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_percent: Option<u32>,
    /// Maximum number of processes in the service process tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processes: Option<u32>,
    /// Soft per-instance data directory quota in MiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_quota_mb: Option<u32>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PermissionDescriptor {
    pub name: String,
}

/// The flattened, schema-agnostic service view produced by
/// [`ApplicationManifest::resolve`]. This type plays the role the
/// AI Runtime plan names `ResolvedService`: a validated, runnable
/// description with no reference back to the source schema. v1
/// backends are projected onto the single `main` service; v2
/// services are copied through with their declared DAG edges.
pub type ResolvedService = ServiceDescriptor;

/// Resolved frontend entry point.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedFrontend {
    pub entry: String,
}

/// Effective permission request the resolved application asks for.
///
/// M1 populates this from the manifest's declared permissions (v1
/// flat IPC-method names, or v2 synthesised `fs:` / `net:` /
/// `shell:` policy names). Policy *evaluation* (grant/deny, user
/// decisions, parameter checks) is introduced with the later
/// permission/MCP milestones; M1 only needs the declared set to be
/// carried in the resolved model so execution never re-reads the
/// source schema.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EffectivePermissionRequest {
    pub descriptors: Vec<PermissionDescriptor>,
}

/// The single, stable execution model handed to the supervisor.
///
/// This is the AI Runtime plan's `ResolvedApplication`: parsing
/// keeps the v1/v2 split ([`ApplicationManifest`]), execution only
/// ever sees this flattened shape. `models`, `mcp_servers` and
/// `agent` are intentionally absent — they are introduced with the
/// Model (M6), MCP (M7) and Agent (M10) milestones and each needs
/// its own manifest schema + types. M1 covers frontend, services
/// and the declared permission set.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedApplication {
    pub id: String,
    pub name: String,
    pub version: semver::Version,
    pub frontend: Option<ResolvedFrontend>,
    pub services: BTreeMap<String, ResolvedService>,
    pub native_workers: BTreeMap<String, crate::manifest_v2::NativeWorkerSpec>,
    pub mcp_servers: BTreeMap<String, crate::manifest_v2::McpServerSpec>,
    pub agent: Option<crate::agent::AgentSpec>,
    /// App-level runtime version requirements (`runtime.node` /
    /// `runtime.python` in v2). v1 projects the default (no pin).
    pub runtime: RuntimeRequirements,
    pub permissions: EffectivePermissionRequest,
}

impl ApplicationManifest {
    /// Resolve the loaded manifest into the flattened execution
    /// model. Fails only when the declared version is not a valid
    /// semantic version — v2 validates this at load time, v1 stores
    /// the version verbatim so a non-semver v1 version is caught
    /// here instead of leaking into the execution layer.
    pub fn resolve(&self) -> Result<ResolvedApplication, ManifestError> {
        let version = semver::Version::parse(self.version())
            .map_err(|error| ManifestError::Invalid(format!("invalid version: {error}")))?;
        let frontend = self.frontend().map(|frontend| ResolvedFrontend {
            entry: frontend.entry,
        });
        let services = self
            .services()
            .into_iter()
            .map(|descriptor| (descriptor.name.clone(), descriptor))
            .collect();
        let permissions = EffectivePermissionRequest {
            descriptors: self.permissions(),
        };
        Ok(ResolvedApplication {
            id: self.id().to_owned(),
            name: self.name().to_owned(),
            version,
            frontend,
            services,
            native_workers: self
                .as_v2()
                .map(|manifest| manifest.native_workers.clone())
                .unwrap_or_default(),
            mcp_servers: self
                .as_v2()
                .map(|manifest| manifest.mcp_servers.clone())
                .unwrap_or_default(),
            agent: self.as_v2().and_then(|manifest| manifest.agent.clone()),
            runtime: self
                .as_v2()
                .map(|manifest| manifest.runtime.clone())
                .unwrap_or_default(),
            permissions,
        })
    }
}

/// Load the unified application manifest at `root`. Exactly one
/// of `manifest.json` or `app.yaml` must be present; see
/// [`ManifestError`] for the failure modes. The path arguments
/// inside the loaded manifest are also re-validated against
/// `root`, so a malicious `frontend.entry = "../secrets.json"` is
/// rejected by both the v1 and v2 validators.
pub fn load_application(root: &Path) -> Result<ApplicationManifest, ManifestError> {
    let v1_path = root.join("manifest.json");
    let v2_path = root.join("app.yaml");
    let has_v1 = v1_path.is_file();
    let has_v2 = v2_path.is_file();
    match (has_v1, has_v2) {
        (true, true) => Err(ManifestError::BothManifests),
        (true, false) => load_v1(&v1_path, root).map(ApplicationManifest::V1),
        (false, true) => load_v2(&v2_path, root).map(ApplicationManifest::V2),
        (false, false) => Err(ManifestError::MissingManifest(root.to_path_buf())),
    }
}

fn load_v1(path: &Path, root: &Path) -> Result<AppManifestV1, ManifestError> {
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(ManifestError::ManifestTooLarge);
    }
    let input = std::fs::read_to_string(path)?;
    let manifest: AppManifestV1 =
        serde_json::from_str(&input).map_err(|error| ManifestError::Invalid(error.to_string()))?;
    manifest
        .validate(root)
        .map_err(|error| ManifestError::Invalid(error.to_string()))?;
    Ok(manifest)
}

fn load_v2(_path: &Path, root: &Path) -> Result<ApplicationManifestV2, ManifestError> {
    // `manifest_v2::load` already enforces the same 1 MiB cap
    // and runs the full v2 validator (path safety, runtime
    // requirements, DAG cycle detection), so we delegate entirely
    // to it. The path is still passed in so the caller can reuse
    // it later if a richer error needs to be added.
    crate::manifest_v2::load(root).map_err(Into::into)
}

fn v1_backend_to_service(backend: &Backend) -> ServiceDescriptor {
    let health = match (backend.mode, backend.health_check.as_ref()) {
        (BackendMode::Service, Some(check)) => Some(map_v1_health_check(check)),
        _ => None,
    };
    let restart = backend
        .restart
        .as_ref()
        .map(map_v1_restart)
        .unwrap_or_default();
    ServiceDescriptor {
        name: ServiceDescriptor::V1_MAIN_SERVICE.to_owned(),
        runtime: map_v1_runtime_kind(backend.runtime),
        command: backend.entry.clone(),
        args: Vec::new(),
        depends_on: Vec::new(),
        env: BTreeMap::new(),
        port: backend.port,
        mode: match backend.mode {
            BackendMode::Rpc => ServiceMode::Rpc,
            BackendMode::Service => ServiceMode::Service,
        },
        health,
        restart,
        resources: None,
    }
}

fn map_v1_health_check(check: &HealthCheck) -> ServiceHealthDescriptor {
    ServiceHealthDescriptor {
        kind: ServiceHealthKind::Http,
        path: Some(check.path.clone()),
        // v1 did not model the polling interval; the supervisor
        // polls on a fixed 5s cadence. Preserve that here so a
        // migrated v1 service behaves identically.
        interval_ms: 5_000,
        timeout_ms: check.timeout_ms,
    }
}

fn map_v1_restart(policy: &RestartPolicy) -> ServiceRestartDescriptor {
    let parsed = match policy.policy.as_str() {
        "never" => ServiceRestartPolicy::Never,
        "always" => ServiceRestartPolicy::Always,
        // "on-failure" and any future / unknown name: fall back
        // to on-failure so a typo in the manifest does not silently
        // disable restarts.
        _ => ServiceRestartPolicy::OnFailure,
    };
    ServiceRestartDescriptor {
        policy: parsed,
        max_retries: policy.max_retries,
    }
}

fn map_v1_runtime_kind(kind: RuntimeKindV1) -> ServiceRuntime {
    match kind {
        RuntimeKindV1::Node => ServiceRuntime::Node,
        // v1's `RuntimeKind` enum only declared `Node`; the
        // v1 enum cannot be extended without bumping the
        // schema, so a future v1 manifest that asked for
        // Python / Native is rejected upstream by serde. This
        // catch-all therefore only fires if someone hand-wrote
        // an unsupported runtime in a programmatic
        // `AppManifest` value, in which case falling back to
        // `Node` is no worse than the v1 default.
        _ => ServiceRuntime::Node,
    }
}

fn v2_service_to_descriptor(name: &str, spec: &ServiceSpec) -> ServiceDescriptor {
    let health = spec.health.as_ref().map(map_v2_health);
    let restart = ServiceRestartDescriptor {
        policy: match spec.restart.policy {
            RestartPolicyV2::Never => ServiceRestartPolicy::Never,
            RestartPolicyV2::OnFailure => ServiceRestartPolicy::OnFailure,
            RestartPolicyV2::Always => ServiceRestartPolicy::Always,
        },
        max_retries: spec.restart.max_retries,
    };
    let port = match spec.port {
        Some(ServicePort::Fixed(port)) => Some(port),
        // Auto-port is collapsed to `None` in the unified view;
        // the supervisor allocates one from the private range.
        Some(ServicePort::Auto(_)) | None => None,
    };
    ServiceDescriptor {
        name: name.to_owned(),
        runtime: spec.runtime,
        command: spec.command.clone(),
        args: spec.args.clone(),
        depends_on: spec.depends_on.clone(),
        env: spec.env.clone(),
        port,
        // v2 services with an HTTP health check are implicitly
        // `Service` mode. Rpc mode is the fallback for v2
        // services that are request/response only.
        mode: if health.is_some() {
            ServiceMode::Service
        } else {
            ServiceMode::Rpc
        },
        health,
        restart,
        resources: spec.resources.as_ref().map(map_v2_resources),
    }
}

fn map_v2_resources(resources: &ServiceResources) -> ServiceResourcesDescriptor {
    ServiceResourcesDescriptor {
        memory_mb: resources.memory_mb,
        cpu_percent: resources.cpu_percent,
        processes: resources.processes,
        data_quota_mb: resources.data_quota_mb,
    }
}

fn map_v2_health(health: &ServiceHealth) -> ServiceHealthDescriptor {
    ServiceHealthDescriptor {
        kind: match health.kind {
            HealthKind::Http => ServiceHealthKind::Http,
            HealthKind::Process => ServiceHealthKind::Process,
        },
        path: health.path.clone(),
        interval_ms: health.interval_ms,
        timeout_ms: health.timeout_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_v1(root: &Path, manifest: &str) {
        std::fs::write(root.join("manifest.json"), manifest).unwrap();
    }

    fn write_v2(root: &Path, yaml: &str) {
        std::fs::write(root.join("app.yaml"), yaml).unwrap();
    }

    fn write_main_js(root: &Path) {
        std::fs::write(root.join("main.js"), "console.log('ok')").unwrap();
    }

    fn write_index_html(root: &Path) {
        std::fs::write(root.join("index.html"), "<!doctype html>").unwrap();
    }

    fn v1_with_backend() -> &'static str {
        r#"{
          "schemaVersion": 1,
          "id": "com.alex.hello",
          "name": "Hello",
          "version": "0.1.0",
          "frontend": { "entry": "index.html" },
          "backend": {
            "runtime": "node",
            "entry": "main.js"
          },
          "permissions": [
            { "name": "runtime.invoke" },
            { "name": "filesystem.read", "paths": ["data"] }
          ]
        }"#
    }

    fn v1_service_mode() -> &'static str {
        r#"{
          "schemaVersion": 1,
          "id": "com.alex.svc",
          "name": "Svc",
          "version": "0.1.0",
          "frontend": { "entry": "index.html" },
          "backend": {
            "runtime": "node",
            "entry": "main.js",
            "mode": "service",
            "healthCheck": { "path": "/livez", "timeoutMs": 2500 },
            "restart": { "policy": "always", "maxRetries": 9 },
            "port": 28100
          }
        }"#
    }

    fn v2_two_services() -> &'static str {
        r#"
schemaVersion: 2
id: com.alex.agent
name: agent
version: 1.0.0
runtime:
  node: "22"
  python: "3.12"
services:
  api:
    runtime: node
    command: main.js
    args: ["--serve"]
    dependsOn: [worker]
    port: 29010
    env:
      LOG_LEVEL: info
    health: { type: http, path: /health, intervalMs: 3000, timeoutMs: 1500 }
    restart: { policy: on-failure, maxRetries: 7 }
    resources:
      memoryMb: 512
      cpuPercent: 50
      processes: 4
      dataQuotaMb: 1024
  worker:
    runtime: python
    command: worker.py
    health: { type: process }
"#
    }

    #[test]
    fn v1_with_backend_loads_and_projects_a_single_main_service() {
        let dir = tempfile::tempdir().unwrap();
        write_index_html(&dir.path());
        write_main_js(&dir.path());
        write_v1(&dir.path(), v1_with_backend());

        let manifest = load_application(dir.path()).expect("load v1");
        assert_eq!(manifest.schema_version(), "1");
        assert_eq!(manifest.id(), "com.alex.hello");
        assert_eq!(manifest.name(), "Hello");
        assert_eq!(manifest.version(), "0.1.0");
        let frontend = manifest.frontend().expect("frontend present");
        assert_eq!(frontend.entry, "index.html");

        let services = manifest.services();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "main");
        assert_eq!(services[0].command, "main.js");
        assert!(services[0].depends_on.is_empty());
        assert!(services[0].args.is_empty());
        assert!(
            services[0].health.is_none(),
            "rpc backend has no health check"
        );
        assert!(services[0].port.is_none());
        assert!(manifest.has_services());
    }

    #[test]
    fn v1_service_mode_projects_health_and_restart() {
        let dir = tempfile::tempdir().unwrap();
        write_index_html(&dir.path());
        write_main_js(&dir.path());
        write_v1(&dir.path(), v1_service_mode());

        let manifest = load_application(dir.path()).expect("load v1 service");
        let services = manifest.services();
        let service = &services[0];
        let health = service.health.as_ref().expect("health projected");
        assert_eq!(health.kind, ServiceHealthKind::Http);
        assert_eq!(health.path.as_deref(), Some("/livez"));
        assert_eq!(health.timeout_ms, 2500);
        assert_eq!(health.interval_ms, 5_000);
        assert_eq!(service.port, Some(28100));
        assert_eq!(service.restart.policy, ServiceRestartPolicy::Always);
        assert_eq!(service.restart.max_retries, 9);
    }

    #[test]
    fn v1_without_backend_has_no_services_but_keeps_frontend() {
        let dir = tempfile::tempdir().unwrap();
        write_index_html(&dir.path());
        write_v1(
            &dir.path(),
            r#"{
              "schemaVersion": 1,
              "id": "com.alex.ui",
              "name": "UI",
              "version": "0.1.0",
              "frontend": { "entry": "index.html" }
            }"#,
        );
        let manifest = load_application(dir.path()).expect("load frontend-only v1");
        assert!(manifest.services().is_empty());
        assert!(!manifest.has_services());
        assert!(manifest.frontend().is_some());
    }

    #[test]
    fn v1_permission_names_match_legacy_ipc_methods() {
        let dir = tempfile::tempdir().unwrap();
        write_index_html(&dir.path());
        write_main_js(&dir.path());
        write_v1(&dir.path(), v1_with_backend());
        let manifest = load_application(dir.path()).unwrap();
        let names: Vec<_> = manifest.permissions().into_iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["runtime.invoke", "filesystem.read"]);
    }

    #[test]
    fn v1_update_source_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        write_index_html(&dir.path());
        write_v1(
            &dir.path(),
            r#"{
              "schemaVersion": 1,
              "id": "com.alex.up",
              "name": "Up",
              "version": "0.1.0",
              "frontend": { "entry": "index.html" },
              "update": { "manifestUrl": "https://example.com/manifest.json" }
            }"#,
        );
        let manifest = load_application(dir.path()).unwrap();
        let source = manifest.update_source().expect("update source");
        assert_eq!(source.manifest_url, "https://example.com/manifest.json");
        assert_eq!(source.channel, "stable");
    }

    #[test]
    fn v2_with_frontend_loads_and_lists_services() {
        let dir = tempfile::tempdir().unwrap();
        write_index_html(&dir.path());
        write_main_js(&dir.path());
        std::fs::write(dir.path().join("worker.py"), "").unwrap();
        write_v2(&dir.path(), v2_two_services());

        let manifest = load_application(dir.path()).expect("load v2");
        assert_eq!(manifest.schema_version(), "2");
        assert_eq!(manifest.id(), "com.alex.agent");
        let services = manifest.services();
        assert_eq!(services.len(), 2);

        let api = services.iter().find(|s| s.name == "api").expect("api");
        assert_eq!(api.runtime, ServiceRuntime::Node);
        assert_eq!(api.command, "main.js");
        assert_eq!(api.args, vec!["--serve"]);
        assert_eq!(api.depends_on, vec!["worker"]);
        assert_eq!(api.env.get("LOG_LEVEL").map(String::as_str), Some("info"));
        assert_eq!(api.port, Some(29010));
        let health = api.health.as_ref().expect("http health");
        assert_eq!(health.kind, ServiceHealthKind::Http);
        assert_eq!(health.path.as_deref(), Some("/health"));
        assert_eq!(health.interval_ms, 3000);
        assert_eq!(health.timeout_ms, 1500);
        assert_eq!(api.restart.policy, ServiceRestartPolicy::OnFailure);
        assert_eq!(api.restart.max_retries, 7);

        let resources = api.resources.as_ref().expect("api resources");
        assert_eq!(resources.memory_mb, Some(512));
        assert_eq!(resources.cpu_percent, Some(50));
        assert_eq!(resources.processes, Some(4));
        assert_eq!(resources.data_quota_mb, Some(1024));

        let worker = services
            .iter()
            .find(|s| s.name == "worker")
            .expect("worker");
        assert_eq!(worker.runtime, ServiceRuntime::Python);
        assert_eq!(worker.command, "worker.py");
        let worker_health = worker.health.as_ref().expect("process health");
        assert_eq!(worker_health.kind, ServiceHealthKind::Process);
        assert!(worker.depends_on.is_empty());
        assert!(worker.resources.is_none(), "worker declares no quota");
    }

    #[test]
    fn v2_without_frontend_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        write_main_js(&dir.path());
        write_v2(
            &dir.path(),
            r#"
schemaVersion: 2
id: com.alex.headless
name: headless
version: 1.0.0
runtime: { node: "22" }
services:
  worker:
    runtime: node
    command: main.js
"#,
        );
        let manifest = load_application(dir.path()).expect("headless v2 loads");
        assert!(manifest.frontend().is_none());
        assert_eq!(manifest.services().len(), 1);
    }

    #[test]
    fn v2_permission_descriptors_carry_paths() {
        let dir = tempfile::tempdir().unwrap();
        write_main_js(&dir.path());
        write_v2(
            &dir.path(),
            r#"
schemaVersion: 2
id: com.alex.policy
name: policy
version: 1.0.0
runtime: { node: "22" }
services:
  worker:
    runtime: node
    command: main.js
permissions:
  filesystem:
    read: ["docs", "data"]
    write: ["logs"]
  network:
    allow: ["https://example.com"]
  shell:
    allow: ["git"]
"#,
        );
        let manifest = load_application(dir.path()).expect("load v2 with policy");
        let names: Vec<_> = manifest.permissions().into_iter().map(|p| p.name).collect();
        assert!(names.contains(&"fs:read:docs".to_string()));
        assert!(names.contains(&"fs:read:data".to_string()));
        assert!(names.contains(&"fs:write:logs".to_string()));
        assert!(names.contains(&"net:allow:https://example.com".to_string()));
        assert!(names.contains(&"shell:allow:git".to_string()));
    }

    #[test]
    fn v2_update_source_is_always_none() {
        let dir = tempfile::tempdir().unwrap();
        write_main_js(&dir.path());
        write_v2(
            &dir.path(),
            r#"
schemaVersion: 2
id: com.alex.noupdate
name: noupdate
version: 1.0.0
runtime: { node: "22" }
services:
  worker:
    runtime: node
    command: main.js
"#,
        );
        let manifest = load_application(dir.path()).unwrap();
        assert!(manifest.update_source().is_none());
    }

    #[test]
    fn both_manifests_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_index_html(&dir.path());
        write_main_js(&dir.path());
        write_v1(&dir.path(), v1_with_backend());
        write_v2(
            &dir.path(),
            r#"
schemaVersion: 2
id: com.alex.dupe
name: dupe
version: 1.0.0
runtime: { node: "22" }
services:
  worker:
    runtime: node
    command: main.js
"#,
        );
        let error = load_application(dir.path()).unwrap_err();
        assert!(matches!(error, ManifestError::BothManifests));
    }

    #[test]
    fn missing_manifest_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "no manifest here").unwrap();
        let error = load_application(dir.path()).unwrap_err();
        assert!(matches!(error, ManifestError::MissingManifest(_)));
    }

    #[test]
    fn oversized_v1_manifest_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        // 1 MiB + 1 byte of valid JSON. The padding keeps the
        // bytes outside the JSON tokenizer's hot path while
        // still tripping the size guard.
        let padding = "x".repeat(MAX_MANIFEST_BYTES as usize);
        let blob = format!(
            r#"{{ "schemaVersion": 1, "id": "com.alex.big", "name": "Big", "version": "0.1.0", "frontend": {{ "entry": "index.html" }}, "_pad": "{}" }}"#,
            padding
        );
        write_index_html(&dir.path());
        write_v1(&dir.path(), &blob);
        let error = load_application(dir.path()).unwrap_err();
        assert!(matches!(error, ManifestError::ManifestTooLarge));
    }

    #[test]
    fn invalid_v1_manifest_surfaces_a_validation_error() {
        let dir = tempfile::tempdir().unwrap();
        // Manifest references a frontend entry that does not
        // exist on disk. The existing v1 validator rejects this
        // with a "does not exist" message; the unified loader
        // must propagate it as `Invalid`.
        write_v1(
            &dir.path(),
            r#"{
              "schemaVersion": 1,
              "id": "com.alex.missing",
              "name": "Missing",
              "version": "0.1.0",
              "frontend": { "entry": "absent.html" }
            }"#,
        );
        let error = load_application(dir.path()).unwrap_err();
        assert!(matches!(error, ManifestError::Invalid(_)));
    }

    #[test]
    fn invalid_v2_manifest_surfaces_a_validation_error() {
        let dir = tempfile::tempdir().unwrap();
        write_v2(
            &dir.path(),
            r#"
schemaVersion: 2
id: com.alex.bad
name: bad
version: 1.0.0
runtime: { node: "22" }
services:
  worker:
    runtime: node
    command: ../escape.js
"#,
        );
        let error = load_application(dir.path()).unwrap_err();
        assert!(matches!(error, ManifestError::Invalid(_)));
    }

    #[test]
    fn as_v1_and_as_v2_are_mutually_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        write_index_html(&dir.path());
        write_main_js(&dir.path());
        write_v1(&dir.path(), v1_with_backend());
        let manifest = load_application(dir.path()).unwrap();
        assert!(manifest.as_v1().is_some());
        assert!(manifest.as_v2().is_none());

        let dir2 = tempfile::tempdir().unwrap();
        write_main_js(&dir2.path());
        write_v2(
            &dir2.path(),
            r#"
schemaVersion: 2
id: com.alex.x
name: x
version: 1.0.0
runtime: { node: "22" }
services:
  worker:
    runtime: node
    command: main.js
"#,
        );
        let manifest = load_application(dir2.path()).unwrap();
        assert!(manifest.as_v1().is_none());
        assert!(manifest.as_v2().is_some());
    }

    #[test]
    fn resolve_projects_v1_backend_into_a_resolved_application() {
        let dir = tempfile::tempdir().unwrap();
        write_index_html(&dir.path());
        write_main_js(&dir.path());
        write_v1(&dir.path(), v1_with_backend());

        let manifest = load_application(dir.path()).expect("load v1");
        let resolved = manifest.resolve().expect("resolve v1");

        assert_eq!(resolved.id, "com.alex.hello");
        assert_eq!(resolved.name, "Hello");
        assert_eq!(resolved.version, semver::Version::parse("0.1.0").unwrap());
        assert_eq!(
            resolved.frontend.as_ref().map(|f| f.entry.as_str()),
            Some("index.html")
        );
        assert_eq!(resolved.services.len(), 1);
        let main = resolved.services.get("main").expect("main service");
        assert_eq!(main.command, "main.js");
        assert!(main.depends_on.is_empty());
        let names: Vec<_> = resolved
            .permissions
            .descriptors
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(names, vec!["runtime.invoke", "filesystem.read"]);
    }

    #[test]
    fn resolve_projects_v2_services_and_headless_frontend() {
        let dir = tempfile::tempdir().unwrap();
        write_main_js(&dir.path());
        std::fs::write(dir.path().join("worker.py"), "").unwrap();
        write_v2(&dir.path(), v2_two_services());

        let manifest = load_application(dir.path()).expect("load v2");
        let resolved = manifest.resolve().expect("resolve v2");

        assert_eq!(resolved.id, "com.alex.agent");
        assert!(resolved.frontend.is_none());
        assert_eq!(resolved.runtime.node.as_deref(), Some("22"));
        assert_eq!(resolved.runtime.python.as_deref(), Some("3.12"));
        assert_eq!(resolved.services.len(), 2);
        let api = resolved.services.get("api").expect("api");
        assert_eq!(api.depends_on, vec!["worker"]);
        assert_eq!(api.port, Some(29010));
        let resources = api.resources.as_ref().expect("api resources");
        assert_eq!(resources.memory_mb, Some(512));
        assert_eq!(resources.cpu_percent, Some(50));
        let worker = resolved.services.get("worker").expect("worker");
        assert_eq!(worker.runtime, ServiceRuntime::Python);
    }

    #[test]
    fn resolve_rejects_a_non_semver_v1_version() {
        let dir = tempfile::tempdir().unwrap();
        write_index_html(&dir.path());
        write_v1(
            &dir.path(),
            r#"{
              "schemaVersion": 1,
              "id": "com.alex.loose",
              "name": "Loose",
              "version": "latest",
              "frontend": { "entry": "index.html" }
            }"#,
        );
        // v1 stores the version verbatim, so loading succeeds; the
        // execution model is stricter and must reject the value here.
        let manifest = load_application(dir.path()).expect("load loose v1");
        let error = manifest.resolve().unwrap_err();
        assert!(matches!(error, ManifestError::Invalid(_)));
    }
}

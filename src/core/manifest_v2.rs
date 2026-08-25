//! Alex Runtime application manifest v2 (`app.yaml`).
//!
//! This model is intentionally separate from the desktop-oriented v1
//! `manifest.json`. It introduces optional frontend, multiple services,
//! runtime version requirements and deterministic dependency ordering.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Error)]
pub enum ManifestV2Error {
    #[error("app.yaml I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid app.yaml: {0}")]
    Yaml(#[from] serde_yaml_ng::Error),
    #[error("invalid application manifest: {0}")]
    Validation(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplicationManifestV2 {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontend: Option<FrontendV2>,
    #[serde(default)]
    pub runtime: RuntimeRequirements,
    pub services: BTreeMap<String, ServiceSpec>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mcp_servers: BTreeMap<String, McpServerSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<crate::agent::AgentSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub storage: Vec<StorageSpec>,
    #[serde(default)]
    pub permissions: PermissionPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "transport", rename_all = "kebab-case", deny_unknown_fields)]
pub enum McpServerSpec {
    Stdio {
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        #[serde(default)]
        legacy: bool,
    },
    StreamableHttp {
        endpoint: String,
        #[serde(
            default,
            rename = "tokenAccount",
            skip_serializing_if = "Option::is_none"
        )]
        token_account: Option<String>,
        #[serde(default)]
        legacy: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrontendV2 {
    pub entry: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeRequirements {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServiceRuntime {
    Node,
    Python,
    Native,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceSpec {
    pub runtime: ServiceRuntime,
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<ServicePort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<ServiceHealth>,
    #[serde(default)]
    pub restart: ServiceRestart,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ServicePort {
    Fixed(u16),
    Auto(AutoPort),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AutoPort {
    Auto,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceHealth {
    #[serde(rename = "type")]
    pub kind: HealthKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default = "default_health_interval_ms")]
    pub interval_ms: u64,
    #[serde(default = "default_health_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_health_interval_ms() -> u64 {
    5_000
}
fn default_health_timeout_ms() -> u64 {
    10_000
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HealthKind {
    Http,
    Process,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceRestart {
    #[serde(default)]
    pub policy: RestartPolicyV2,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

impl Default for ServiceRestart {
    fn default() -> Self {
        Self {
            policy: RestartPolicyV2::OnFailure,
            max_retries: default_max_retries(),
        }
    }
}

fn default_max_retries() -> u32 {
    5
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicyV2 {
    Never,
    #[default]
    OnFailure,
    Always,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StorageSpec {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PermissionPolicy {
    #[serde(default)]
    pub filesystem: FilesystemPermissions,
    #[serde(default)]
    pub network: NetworkPermissions,
    #[serde(default)]
    pub shell: ShellPermissions,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FilesystemPermissions {
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NetworkPermissions {
    #[serde(default)]
    pub allow: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShellPermissions {
    #[serde(default)]
    pub allow: Vec<String>,
}

pub fn load(root: &Path) -> Result<ApplicationManifestV2, ManifestV2Error> {
    let path = root.join("app.yaml");
    let metadata = std::fs::metadata(&path)?;
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(ManifestV2Error::Validation("app.yaml exceeds 1 MiB".into()));
    }
    let manifest: ApplicationManifestV2 = serde_yaml_ng::from_slice(&std::fs::read(path)?)?;
    manifest.validate(root)?;
    Ok(manifest)
}

impl ApplicationManifestV2 {
    pub fn validate(&self, root: &Path) -> Result<(), ManifestV2Error> {
        if self.schema_version != 2 {
            return Err(validation(format!(
                "unsupported schemaVersion {}; expected 2",
                self.schema_version
            )));
        }
        if !valid_id(&self.id) {
            return Err(validation(format!("invalid application id {:?}", self.id)));
        }
        for (binding, server) in &self.mcp_servers {
            if !valid_component(binding) {
                return Err(validation(format!("invalid MCP binding {binding:?}")));
            }
            match server {
                McpServerSpec::Stdio { command, .. } => {
                    validate_package_path(root, command, "MCP stdio command")?;
                }
                McpServerSpec::StreamableHttp {
                    endpoint,
                    token_account,
                    ..
                } => {
                    let url = url::Url::parse(endpoint)
                        .map_err(|error| validation(format!("invalid MCP endpoint: {error}")))?;
                    let loopback = url.host_str().is_some_and(|host| {
                        host == "localhost"
                            || host
                                .parse::<std::net::IpAddr>()
                                .is_ok_and(|ip| ip.is_loopback())
                    });
                    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
                        return Err(validation(
                            "MCP endpoint must use HTTPS (HTTP is loopback-only)",
                        ));
                    }
                    if token_account.as_deref().is_some_and(|value| {
                        value.is_empty() || value.len() > 255 || value.contains(['\r', '\n', '\0'])
                    }) {
                        return Err(validation("invalid MCP token account"));
                    }
                }
            }
        }
        if let Some(agent) = &self.agent {
            crate::agent::validate_spec(agent).map_err(|error| validation(error.to_string()))?;
        }
        semver::Version::parse(&self.version)
            .map_err(|error| validation(format!("invalid version: {error}")))?;
        if self.name.trim().is_empty() {
            return Err(validation("name cannot be empty"));
        }
        if self.services.is_empty() {
            return Err(validation("at least one service is required"));
        }
        if let Some(frontend) = &self.frontend {
            validate_package_path(root, &frontend.entry, "frontend entry")?;
        }
        for (name, service) in &self.services {
            if !valid_component(name) {
                return Err(validation(format!("invalid service name {name:?}")));
            }
            validate_package_path(root, &service.command, &format!("service {name} command"))?;
            match service.runtime {
                ServiceRuntime::Node if self.runtime.node.is_none() => {
                    return Err(validation(format!("service {name} requires runtime.node")));
                }
                ServiceRuntime::Python if self.runtime.python.is_none() => {
                    return Err(validation(format!(
                        "service {name} requires runtime.python"
                    )));
                }
                _ => {}
            }
            if service.health.as_ref().is_some_and(|health| {
                health.kind == HealthKind::Http
                    && health
                        .path
                        .as_deref()
                        .is_none_or(|path| !path.starts_with('/'))
            }) {
                return Err(validation(format!(
                    "service {name} HTTP health path must start with '/'"
                )));
            }
            for dependency in &service.depends_on {
                if !self.services.contains_key(dependency) {
                    return Err(validation(format!(
                        "service {name} depends on unknown service {dependency}"
                    )));
                }
            }
        }
        for storage in &self.storage {
            if !valid_component(&storage.name) {
                return Err(validation(format!(
                    "invalid storage name {:?}",
                    storage.name
                )));
            }
            validate_relative_path(&storage.path, &format!("storage {} path", storage.name))?;
        }
        for path in self
            .permissions
            .filesystem
            .read
            .iter()
            .chain(&self.permissions.filesystem.write)
        {
            validate_relative_path(path, "filesystem permission path")?;
        }
        self.start_order()?;
        Ok(())
    }

    pub fn start_order(&self) -> Result<Vec<String>, ManifestV2Error> {
        fn visit(
            name: &str,
            manifest: &ApplicationManifestV2,
            visiting: &mut BTreeSet<String>,
            visited: &mut BTreeSet<String>,
            out: &mut Vec<String>,
        ) -> Result<(), ManifestV2Error> {
            if visited.contains(name) {
                return Ok(());
            }
            if !visiting.insert(name.to_owned()) {
                return Err(validation(format!(
                    "service dependency cycle includes {name}"
                )));
            }
            let service = manifest
                .services
                .get(name)
                .ok_or_else(|| validation(format!("unknown service {name}")))?;
            for dependency in &service.depends_on {
                visit(dependency, manifest, visiting, visited, out)?;
            }
            visiting.remove(name);
            visited.insert(name.to_owned());
            out.push(name.to_owned());
            Ok(())
        }
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        let mut out = Vec::with_capacity(self.services.len());
        for name in self.services.keys() {
            visit(name, self, &mut visiting, &mut visited, &mut out)?;
        }
        Ok(out)
    }

    pub fn stop_order(&self) -> Result<Vec<String>, ManifestV2Error> {
        let mut order = self.start_order()?;
        order.reverse();
        Ok(order)
    }
}

fn validate_package_path(root: &Path, value: &str, label: &str) -> Result<(), ManifestV2Error> {
    validate_relative_path(value, label)?;
    if !root.join(value).is_file() {
        return Err(validation(format!("{label} does not exist: {value}")));
    }
    Ok(())
}

fn validate_relative_path(value: &str, label: &str) -> Result<(), ManifestV2Error> {
    let path = Path::new(value);
    if value.trim().is_empty()
        || path.is_absolute()
        || path.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(validation(format!(
            "{label} must be a package-relative path"
        )));
    }
    Ok(())
}

fn valid_id(id: &str) -> bool {
    id.contains('.') && id.split('.').all(valid_component)
}
fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}
fn validation(message: impl Into<String>) -> ManifestV2Error {
    ManifestV2Error::Validation(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(yaml: &str) -> (tempfile::TempDir, ApplicationManifestV2) {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("server")).unwrap();
        std::fs::create_dir_all(temp.path().join("python")).unwrap();
        std::fs::write(temp.path().join("server/index.js"), "").unwrap();
        std::fs::write(temp.path().join("python/main.py"), "").unwrap();
        std::fs::write(temp.path().join("app.yaml"), yaml).unwrap();
        let manifest = load(temp.path()).unwrap();
        (temp, manifest)
    }

    #[test]
    fn loads_multi_service_yaml_and_orders_dependencies() {
        let (_temp, manifest) = fixture(
            r#"
schemaVersion: 2
id: com.example.agent
name: agent
version: 1.0.0
runtime:
  node: "22"
  python: "3.12"
services:
  api:
    runtime: node
    command: server/index.js
    dependsOn: [worker]
    port: auto
    health: { type: http, path: /health }
  worker:
    runtime: python
    command: python/main.py
"#,
        );
        assert_eq!(manifest.start_order().unwrap(), ["worker", "api"]);
        assert_eq!(manifest.stop_order().unwrap(), ["api", "worker"]);
    }

    #[test]
    fn loads_and_validates_mcp_bindings() {
        let (temp, manifest) = fixture(
            r#"
schemaVersion: 2
id: com.example.mcp
name: mcp
version: 1.0.0
runtime: { node: "22" }
services:
  app: { runtime: node, command: server/index.js }
mcpServers:
  local:
    transport: stdio
    command: server/index.js
  remote:
    transport: streamable-http
    endpoint: https://mcp.example.test/v1
    tokenAccount: com.example.mcp/remote
"#,
        );
        assert_eq!(manifest.mcp_servers.len(), 2);
        manifest.validate(temp.path()).unwrap();
    }

    #[test]
    fn dependency_cycle_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("a.js"), "").unwrap();
        std::fs::write(
            temp.path().join("app.yaml"),
            r#"
schemaVersion: 2
id: com.example.cycle
name: cycle
version: 1.0.0
runtime: { node: "22" }
services:
  a: { runtime: node, command: a.js, dependsOn: [b] }
  b: { runtime: node, command: a.js, dependsOn: [a] }
"#,
        )
        .unwrap();
        assert!(load(temp.path()).unwrap_err().to_string().contains("cycle"));
    }

    #[test]
    fn runtime_requirement_and_parent_escape_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("app.yaml"),
            r#"
schemaVersion: 2
id: com.example.bad
name: bad
version: 1.0.0
services:
  worker: { runtime: python, command: ../worker.py }
"#,
        )
        .unwrap();
        let error = load(temp.path()).unwrap_err().to_string();
        assert!(error.contains("package-relative") || error.contains("runtime.python"));
    }
}

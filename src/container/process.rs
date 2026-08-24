//! Container backend launcher routed through the selected isolation provider.

use crate::{
    container::{
        isolation::{self, IsolationHandle, SpawnRequest},
        model::{ContainerSpec, ListenAddress},
    },
    core::manifest::{Backend, BackendMode},
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub struct ServiceEndpoint {
    pub port: u16,
    pub token: String,
}
pub struct LaunchRequest<'a> {
    pub app_id: &'a str,
    pub package_root: &'a Path,
    pub backend: &'a Backend,
    pub data_dir: Option<&'a Path>,
    pub cache_dir: Option<&'a Path>,
    pub log_dir: Option<&'a Path>,
    pub port: Option<u16>,
    pub token: Option<&'a str>,
    pub container: &'a ContainerSpec,
}
pub struct Launched {
    pub pid: u32,
    pub endpoint: Option<ServiceEndpoint>,
    pub ready: bool,
    pub isolation: IsolationHandle,
}

pub fn launch_backend(request: LaunchRequest<'_>) -> Result<Launched, ContainerLauncherError> {
    enforce_policy(request.container, request.package_root)?;
    let executable = crate::runtime::discover_node()
        .ok_or_else(|| ContainerLauncherError::Spawn("node runtime not found".into()))?;
    let mut env = vec![
        (
            "ALEX_PACKAGE_ROOT".into(),
            request.package_root.display().to_string(),
        ),
        ("ALEX_APP_ID".into(), request.app_id.into()),
    ];
    if let Some(path) = request.data_dir {
        env.push(("ALEX_APP_DATA_DIR".into(), path.display().to_string()));
    }
    if let Some(path) = request.cache_dir {
        env.push(("ALEX_APP_CACHE_DIR".into(), path.display().to_string()));
    }
    if let Some(path) = request.log_dir {
        env.push(("ALEX_APP_LOG_DIR".into(), path.display().to_string()));
    }
    if let Some(path) = std::env::var_os("PATH") {
        env.push(("PATH".into(), path.to_string_lossy().into_owned()));
    }
    let endpoint = if matches!(request.backend.mode, BackendMode::Service) {
        if matches!(request.container.network.listen, ListenAddress::None) {
            return Err(ContainerLauncherError::Policy(
                "service backend conflicts with network.listen=none".into(),
            ));
        }
        let port = request.port.or(request.backend.port).map_or_else(
            || {
                crate::container::network::allocate_loopback_port()
                    .map_err(|e| ContainerLauncherError::Spawn(e.to_string()))
            },
            Ok,
        )?;
        let token = request
            .token
            .map(str::to_owned)
            .unwrap_or_else(runtime_token);
        env.push(("ALEX_SERVICE_PORT".into(), port.to_string()));
        env.push(("ALEX_RUNTIME_TOKEN".into(), token.clone()));
        Some(ServiceEndpoint { port, token })
    } else {
        None
    };
    let provider = isolation::provider_for(request.container.isolation)
        .map_err(|e| ContainerLauncherError::Spawn(e.to_string()))?;
    let spawned = provider
        .spawn(&SpawnRequest {
            executable,
            args: vec![request.backend.entry.clone()],
            env,
            cwd: request.package_root.to_path_buf(),
            limits: &request.container.resources,
            level: request.container.isolation,
        })
        .map_err(|e| ContainerLauncherError::Spawn(e.to_string()))?;
    let ready = endpoint
        .as_ref()
        .is_none_or(|endpoint| wait_for_loopback(endpoint.port, Duration::from_secs(5)));
    Ok(Launched {
        pid: spawned.pid,
        endpoint,
        ready,
        isolation: spawned.isolation,
    })
}

fn enforce_policy(spec: &ContainerSpec, package_root: &Path) -> Result<(), ContainerLauncherError> {
    if spec.isolation != crate::container::model::IsolationLevel::Process
        && !spec.filesystem.application_read_only
    {
        return Err(ContainerLauncherError::Policy(
            "L1+ requires a read-only application layer".into(),
        ));
    }
    if !package_root.is_dir() {
        return Err(ContainerLauncherError::Policy(
            "application root is missing".into(),
        ));
    }
    if spec.isolation == crate::container::model::IsolationLevel::AppContainer
        && (!spec.network.outbound_allow.is_empty() || !spec.network.outbound_deny.is_empty())
    {
        return Err(ContainerLauncherError::Policy("per-app outbound ACL enforcement requires the WSL provider; refusing an audit-only downgrade".into()));
    }
    Ok(())
}
fn wait_for_loopback(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}
fn runtime_token() -> String {
    let mut bytes = [0u8; 32];
    let _ = getrandom::fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, thiserror::Error)]
pub enum ContainerLauncherError {
    #[error("failed to spawn backend: {0}")]
    Spawn(String),
    #[error("container policy cannot be enforced: {0}")]
    Policy(String),
}
pub fn installed_package_root(install_root: &Path, app_id: &str) -> PathBuf {
    install_root.join(app_id)
}

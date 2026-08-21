//! `alex` 应用管理服务 — CLI 和管理 UI 共用的后端 facade。
//!
//! 设计目标:
//! - CLI 和管理 UI 调用同一套服务,避免双实现漂移
//! - UI 不得通过启动 CLI 进程并解析 stdout 操作应用(进程外调用是
//!   显式禁止的反模式)
//! - App Registry 原子写 + 损坏时从 install_root 重建,保证启动总能恢复
//!
//! MVP 阶段 1(对应 P0 设计):
//! - 列表 / 详情 / 安装 / 卸载 / 权限读写
//! - launch / stop 在 Phase 1.4(系统 WebView)一起做,因为需要 Runtime
//!   进程宿主
//!
//! 不在 MVP 阶段 1 范围:更新检查、任务队列、远程拉取、UI 系统身份

use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    authorization::{AuthorizationError, PermissionDecision, PermissionStore},
    load_app,
    manifest::AppManifest,
    package,
    package::PackageError,
    permission::Permission,
    runtime::{RuntimeHandle, RuntimeState, RuntimeStatus},
};

const REGISTRY_FILENAME: &str = "registry.json";
const REGISTRY_VERSION: u32 = 1;

/// 应用安装来源 — 用于 App Registry 区分 .alex 包 / 远程更新 / dev 模式
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum InstallSource {
    LocalPackage,
    RemoteUpdate,
    DevMode,
}

/// 列表视图精简信息
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSummary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub path: PathBuf,
    pub install_source: InstallSource,
    pub last_launched_at: Option<String>,
    pub publisher_fingerprint: Option<String>,
    pub signed: bool,
}

/// 详情视图
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppDetails {
    #[serde(flatten)]
    pub summary: AppSummary,
    pub manifest: AppManifest,
    pub permissions: Vec<PermissionState>,
    pub install_path: PathBuf,
}

/// 单一权限状态 — 用于权限面板
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionState {
    pub name: String,
    pub decision: PermissionDecision,
    pub manifest_declared: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallOptions {
    pub require_signature: bool,
    pub trusted_key: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UninstallOptions {
    pub remove_data: bool,
}

#[derive(Debug, Error)]
pub enum ManagerError {
    #[error(transparent)]
    Package(#[from] PackageError),
    #[error(transparent)]
    Authorization(#[from] AuthorizationError),
    #[error(transparent)]
    Alex(#[from] crate::AlexError),
    #[error("runtime: {0}")]
    Runtime(String),
    #[error("application not found: {0}")]
    NotFound(String),
    #[error("permission is not declared in the application manifest: {0}")]
    UndeclaredPermission(String),
    #[error("manager I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("registry is invalid: {0}")]
    Registry(String),
}

impl From<crate::runtime::RuntimeError> for ManagerError {
    fn from(error: crate::runtime::RuntimeError) -> Self {
        ManagerError::Runtime(error.to_string())
    }
}

impl From<SupervisorError> for ManagerError {
    fn from(error: SupervisorError) -> Self {
        ManagerError::Runtime(error.to_string())
    }
}

pub trait AppManager: Send + Sync {
    fn list_apps(&self) -> Result<Vec<AppSummary>, ManagerError>;
    fn get_app(&self, id: &str) -> Result<AppDetails, ManagerError>;
    fn install(
        &self,
        package_path: &Path,
        options: InstallOptions,
    ) -> Result<AppSummary, ManagerError>;
    fn uninstall(&self, id: &str, options: UninstallOptions) -> Result<(), ManagerError>;
    fn launch(&self, id: &str) -> Result<RuntimeStatus, ManagerError>;
    fn stop(&self, id: &str) -> Result<RuntimeStatus, ManagerError>;
    fn runtime_status(&self, id: &str) -> Result<RuntimeStatus, ManagerError>;
    fn permissions(&self, id: &str) -> Result<Vec<PermissionState>, ManagerError>;
    fn set_permission(
        &self,
        id: &str,
        permission: &str,
        decision: PermissionDecision,
    ) -> Result<(), ManagerError>;
    fn registry_path(&self) -> &Path;
    fn install_root(&self) -> &Path;
}

/// App Registry — 本地状态,记录已安装应用的元数据(安装时间、来源等)
/// Manifest 本身只读,Registry 是 Alex OS 维护的状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RegistryFile {
    version: u32,
    apps: BTreeMap<String, AppRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppRecord {
    pub install_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub last_launched_at: Option<String>,
    #[serde(default)]
    pub publisher_fingerprint: Option<String>,
    pub source: InstallSource,
    #[serde(default)]
    pub package_sha256: Option<String>,
    pub signed: bool,
}

#[derive(Debug, Clone)]
pub struct AppRegistry {
    path: PathBuf,
    state: std::sync::Arc<std::sync::Mutex<RegistryFile>>,
}

impl AppRegistry {
    /// Open an existing registry, or rebuild it by scanning the install root.
    /// Either way, the returned registry is non-empty and ready to read.
    pub fn open_or_rebuild(install_root: &Path) -> Result<Self, ManagerError> {
        let path = registry_path_for(install_root);
        let (file, needs_save) = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<RegistryFile>(&bytes) {
                Ok(file) if file.version == REGISTRY_VERSION => (file, false),
                Ok(_) => (rebuild_from_install_root(install_root)?, true),
                Err(error) => {
                    eprintln!("alex manager: registry invalid, rebuilding ({error})");
                    (rebuild_from_install_root(install_root)?, true)
                }
            },
            Err(_) => (rebuild_from_install_root(install_root)?, true),
        };
        let registry = Self {
            path,
            state: std::sync::Arc::new(std::sync::Mutex::new(file)),
        };
        if needs_save {
            registry.flush()?;
        }
        Ok(registry)
    }

    /// Persist whatever the in-memory state currently holds, regardless of
    /// whether it has been mutated through this handle. Used after a rebuild
    /// so the freshly scanned file lands on disk.
    pub fn flush(&self) -> Result<(), ManagerError> {
        let snapshot = self.state.lock().expect("registry lock poisoned").clone();
        Self::save(&self.path, &snapshot)
    }

    pub fn records(&self) -> Vec<(String, AppRecord)> {
        let state = self.state.lock().expect("registry lock poisoned");
        state
            .apps
            .iter()
            .map(|(id, record)| (id.clone(), record.clone()))
            .collect()
    }

    pub fn upsert(&self, id: String, record: AppRecord) -> Result<(), ManagerError> {
        let snapshot = {
            let mut state = self.state.lock().expect("registry lock poisoned");
            state.apps.insert(id, record);
            state.clone()
        };
        Self::save(&self.path, &snapshot)
    }

    pub fn remove(&self, id: &str) -> Result<Option<AppRecord>, ManagerError> {
        let (removed, snapshot) = {
            let mut state = self.state.lock().expect("registry lock poisoned");
            let removed = state.apps.remove(id);
            (removed, state.clone())
        };
        Self::save(&self.path, &snapshot)?;
        Ok(removed)
    }

    pub fn touch_last_launched(&self, id: &str) -> Result<(), ManagerError> {
        let snapshot = {
            let mut state = self.state.lock().expect("registry lock poisoned");
            if let Some(record) = state.apps.get_mut(id) {
                record.last_launched_at = Some(iso8601_now());
                Some(state.clone())
            } else {
                None
            }
        };
        if let Some(snapshot) = snapshot {
            Self::save(&self.path, &snapshot)?;
        }
        Ok(())
    }

    fn save(path: &Path, file: &RegistryFile) -> Result<(), ManagerError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(file)
            .map_err(|error| ManagerError::Registry(error.to_string()))?;
        let tmp = path.with_extension("json.tmp");
        {
            let mut output = fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&tmp)?;
            output.write_all(&bytes)?;
            output.flush()?;
        }
        fs::rename(&tmp, path)?;
        Ok(())
    }
}

fn registry_path_for(install_root: &Path) -> PathBuf {
    install_root.join(".alex").join(REGISTRY_FILENAME)
}

fn rebuild_from_install_root(install_root: &Path) -> Result<RegistryFile, ManagerError> {
    let installed = package::list_installed(install_root)?;
    let mut apps = BTreeMap::new();
    let now = iso8601_now();
    for app in installed {
        apps.insert(
            app.id.clone(),
            AppRecord {
                install_at: now.clone(),
                updated_at: now.clone(),
                last_launched_at: None,
                publisher_fingerprint: None,
                source: InstallSource::LocalPackage,
                package_sha256: None,
                signed: false,
            },
        );
    }
    Ok(RegistryFile {
        version: REGISTRY_VERSION,
        apps,
    })
}

fn iso8601_now() -> String {
    // Avoid pulling chrono; seconds since epoch is enough for Phase 1
    // and avoids a new dependency. UI can format it as ISO 8601 UTC.
    let seconds = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{seconds}")
}

/// 本地实现:把现有 package / permission / trust 模块 facade 起来
pub struct LocalAppManager {
    install_root: PathBuf,
    registry: AppRegistry,
    permissions_root: PathBuf,
    runtimes: Arc<RuntimeSupervisor>,
}

impl LocalAppManager {
    pub fn open(install_root: &Path) -> Result<Self, ManagerError> {
        let permissions_root = std::env::var_os("ALEX_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("AlexOS")))
            .unwrap_or_else(|| install_root.to_path_buf());
        Self::open_with(install_root, permissions_root)
    }

    /// Construct a manager with an explicit permissions root. Tests use this
    /// to avoid sharing the global `ALEX_DATA_DIR` between parallel runs.
    pub fn open_with(install_root: &Path, permissions_root: PathBuf) -> Result<Self, ManagerError> {
        let registry = AppRegistry::open_or_rebuild(install_root)?;
        Ok(Self {
            install_root: install_root.to_path_buf(),
            registry,
            permissions_root,
            runtimes: Arc::new(RuntimeSupervisor::default()),
        })
    }
}

impl AppManager for LocalAppManager {
    fn list_apps(&self) -> Result<Vec<AppSummary>, ManagerError> {
        let mut out = Vec::new();
        for (id, record) in self.registry.records() {
            let app_path = self.install_root.join(&id);
            if !app_path.is_dir() {
                continue;
            }
            let manifest = match load_app(&app_path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            out.push(summary_from(&manifest, &record, &app_path));
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    fn get_app(&self, id: &str) -> Result<AppDetails, ManagerError> {
        let record = self
            .registry
            .records()
            .into_iter()
            .find(|(rid, _)| rid == id)
            .map(|(_, r)| r)
            .ok_or_else(|| ManagerError::NotFound(id.to_owned()))?;
        let install_path = self.install_root.join(id);
        if !install_path.is_dir() {
            return Err(ManagerError::NotFound(id.to_owned()));
        }
        let manifest = load_app(&install_path)?;
        let summary = summary_from(&manifest, &record, &install_path);
        let permissions = self.permissions(id)?;
        Ok(AppDetails {
            summary,
            manifest,
            permissions,
            install_path,
        })
    }

    fn install(
        &self,
        package_path: &Path,
        options: InstallOptions,
    ) -> Result<AppSummary, ManagerError> {
        let installed = package::install_verified(
            package_path,
            &self.install_root,
            options.require_signature,
            options.trusted_key.as_deref(),
        )?;
        let manifest = load_app(&installed)?;
        let now = iso8601_now();
        let publisher_fingerprint = package::signer_public_key(package_path)?;
        let signed = publisher_fingerprint.is_some();
        let record = AppRecord {
            install_at: now.clone(),
            updated_at: now,
            last_launched_at: None,
            publisher_fingerprint,
            source: InstallSource::LocalPackage,
            package_sha256: None,
            signed,
        };
        self.registry.upsert(manifest.id.clone(), record)?;
        let record_ref = self
            .registry
            .records()
            .into_iter()
            .find(|(rid, _)| rid == &manifest.id)
            .map(|(_, r)| r)
            .ok_or_else(|| ManagerError::NotFound(manifest.id.clone()))?;
        Ok(summary_from(&manifest, &record_ref, &installed))
    }

    fn uninstall(&self, id: &str, options: UninstallOptions) -> Result<(), ManagerError> {
        let _removed = package::uninstall(id, &self.install_root)?;
        self.registry.remove(id)?;
        if options.remove_data {
            let data_dir = self.permissions_root.join("data").join(id);
            if data_dir.exists() {
                let _ = fs::remove_dir_all(&data_dir);
            }
        }
        Ok(())
    }

    fn permissions(&self, id: &str) -> Result<Vec<PermissionState>, ManagerError> {
        let install_path = self.install_root.join(id);
        let manifest = load_app(&install_path)?;
        let store = PermissionStore::open_at(&self.permissions_root, id)?;
        let decisions = store.list();
        let mut out = Vec::new();
        for permission in &manifest.permissions {
            let name = permission_method_name(permission);
            let decision = decisions
                .get(&name)
                .copied()
                .unwrap_or(PermissionDecision::Prompt);
            out.push(PermissionState {
                name,
                decision,
                manifest_declared: true,
            });
        }
        Ok(out)
    }

    fn set_permission(
        &self,
        id: &str,
        permission: &str,
        decision: PermissionDecision,
    ) -> Result<(), ManagerError> {
        let install_path = self.install_root.join(id);
        let manifest = load_app(&install_path)?;
        if !manifest
            .permissions
            .iter()
            .any(|p| permission_method_name(p) == permission)
        {
            return Err(ManagerError::UndeclaredPermission(permission.to_owned()));
        }
        let store = PermissionStore::open_at(&self.permissions_root, id)?;
        store.set(permission, decision)?;
        Ok(())
    }

    fn registry_path(&self) -> &Path {
        &self.registry.path
    }

    fn install_root(&self) -> &Path {
        &self.install_root
    }

    fn launch(&self, id: &str) -> Result<RuntimeStatus, ManagerError> {
        let install_path = self.install_root.join(id);
        let manifest = load_app(&install_path)?;
        let backend = manifest
            .backend
            .as_ref()
            .ok_or_else(|| ManagerError::Runtime("application has no backend runtime".into()))?;
        Ok(self.runtimes.launch(id, &install_path, backend)?)
    }

    fn stop(&self, id: &str) -> Result<RuntimeStatus, ManagerError> {
        Ok(self.runtimes.stop(id)?)
    }

    fn runtime_status(&self, id: &str) -> Result<RuntimeStatus, ManagerError> {
        Ok(self.runtimes.status(id)?)
    }
}

/// In-memory map of running app backends. Keyed by app id. Phase 1.4
/// only needs an in-process supervisor; the long-term plan is to move
/// state to the App Registry and accept that the manager process is the
/// single supervisor.
#[derive(Default)]
pub struct RuntimeSupervisor {
    runtimes: Mutex<HashMap<String, RuntimeHandle>>,
}

#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error("application {0} is already running")]
    AlreadyRunning(String),
    #[error("runtime error: {0}")]
    Runtime(#[from] crate::runtime::RuntimeError),
}

impl RuntimeSupervisor {
    pub fn launch(
        &self,
        id: &str,
        install_root: &Path,
        backend: &crate::manifest::Backend,
    ) -> Result<RuntimeStatus, SupervisorError> {
        let mut runtimes = self.runtimes.lock().expect("runtime lock poisoned");
        if runtimes.contains_key(id) {
            return Err(SupervisorError::AlreadyRunning(id.to_owned()));
        }
        let handle = RuntimeHandle::start(install_root, backend)?;
        let status = handle.status(Duration::from_secs(2))?;
        runtimes.insert(id.to_owned(), handle);
        Ok(status)
    }

    pub fn stop(&self, id: &str) -> Result<RuntimeStatus, SupervisorError> {
        let mut runtimes = self.runtimes.lock().expect("runtime lock poisoned");
        let Some(handle) = runtimes.remove(id) else {
            // Idempotent: stopping a non-running app is a no-op, not an error.
            return Ok(RuntimeStatus {
                state: RuntimeState::Stopped,
                pid: None,
                restart_count: 0,
                last_error: None,
                logs: Vec::new(),
            });
        };
        handle.cancel();
        let _ = handle.status(Duration::from_millis(200));
        Ok(RuntimeStatus {
            state: RuntimeState::Stopped,
            pid: None,
            restart_count: 0,
            last_error: None,
            logs: Vec::new(),
        })
    }

    pub fn status(&self, id: &str) -> Result<RuntimeStatus, SupervisorError> {
        let runtimes = self.runtimes.lock().expect("runtime lock poisoned");
        let Some(handle) = runtimes.get(id) else {
            return Ok(RuntimeStatus {
                state: RuntimeState::Stopped,
                pid: None,
                restart_count: 0,
                last_error: None,
                logs: Vec::new(),
            });
        };
        Ok(handle.status(Duration::from_millis(200))?)
    }
}

/// 唯一允许调用 `manager.*` IPC 的来源身份。
/// 系统 WebView 启动时,BRIDGE 用这个 ID 替换 `__ALEX_PACKAGE_ID__`,
/// 这样任何来自 UI 的请求都会带上这个 source。普通 app 改不了 —
/// 它们启动时从 `manifest.id` 注入,没法伪装成系统身份。
pub const SYSTEM_IDENTITY: &str = "alex://system/app-manager";

const MAX_IPC_MESSAGE_BYTES: usize = 1024 * 1024;

/// 独立于普通 ApiRouter 的系统级 IPC 路由。校验:
/// 1. `request.source == SYSTEM_IDENTITY`
/// 2. `request.method` 以 `manager.` 为前缀
///
/// 普通 app 的 dispatch 不会触发这条路径;系统 WebView 直接使用。
pub struct ManagerRouter {
    manager: Arc<dyn AppManager>,
}

impl ManagerRouter {
    pub fn new(manager: Arc<dyn AppManager>) -> Self {
        Self { manager }
    }

    /// Parse a JSON request body and dispatch it. Mirrors `ApiRouter::dispatch_json`.
    pub fn dispatch_json(&self, body: &str) -> crate::ipc::Response {
        if body.len() > MAX_IPC_MESSAGE_BYTES {
            return crate::ipc::Response::error(
                "unknown",
                "MESSAGE_TOO_LARGE",
                "IPC messages are limited to 1 MiB",
            );
        }
        match serde_json::from_str::<crate::ipc::Request>(body) {
            Ok(request) => self.dispatch(request),
            Err(error) => {
                crate::ipc::Response::error("unknown", "INVALID_REQUEST", error.to_string())
            }
        }
    }

    pub fn dispatch(&self, request: crate::ipc::Request) -> crate::ipc::Response {
        if request.source != SYSTEM_IDENTITY {
            return crate::ipc::Response::error(
                &request.id,
                "SOURCE_MISMATCH",
                "manager API requires system identity",
            );
        }
        self.dispatch_authorized(request)
    }

    fn dispatch_authorized(&self, request: crate::ipc::Request) -> crate::ipc::Response {
        let method = request.method.as_str();
        if !method.starts_with("manager.") {
            return crate::ipc::Response::error(
                &request.id,
                "UNKNOWN_METHOD",
                format!("manager router received non-manager method: {method}"),
            );
        }
        match method {
            "manager.list_apps" => match self.manager.list_apps() {
                Ok(apps) => json_response(&request.id, &serde_json::json!({ "apps": apps })),
                Err(error) => manager_error_response(&request.id, error),
            },
            "manager.get_app" => match parse_id(&request.params) {
                Ok(id) => match self.manager.get_app(&id) {
                    Ok(details) => json_response(
                        &request.id,
                        &serde_json::to_value(details).unwrap_or_default(),
                    ),
                    Err(error) => manager_error_response(&request.id, error),
                },
                Err(msg) => crate::ipc::Response::error(&request.id, "INVALID_PARAMS", msg),
            },
            "manager.install" => match parse_install(&request.params) {
                Ok((path, options)) => match self.manager.install(&path, options) {
                    Ok(summary) => json_response(
                        &request.id,
                        &serde_json::to_value(summary).unwrap_or_default(),
                    ),
                    Err(error) => manager_error_response(&request.id, error),
                },
                Err(msg) => crate::ipc::Response::error(&request.id, "INVALID_PARAMS", msg),
            },
            "manager.uninstall" => match parse_uninstall(&request.params) {
                Ok((id, options)) => match self.manager.uninstall(&id, options) {
                    Ok(()) => json_response(&request.id, &serde_json::json!({ "ok": true })),
                    Err(error) => manager_error_response(&request.id, error),
                },
                Err(msg) => crate::ipc::Response::error(&request.id, "INVALID_PARAMS", msg),
            },
            "manager.launch" => match parse_id(&request.params) {
                Ok(id) => match self.manager.launch(&id) {
                    Ok(status) => json_response(
                        &request.id,
                        &serde_json::to_value(status).unwrap_or_default(),
                    ),
                    Err(error) => manager_error_response(&request.id, error),
                },
                Err(msg) => crate::ipc::Response::error(&request.id, "INVALID_PARAMS", msg),
            },
            "manager.stop" => match parse_id(&request.params) {
                Ok(id) => match self.manager.stop(&id) {
                    Ok(status) => json_response(
                        &request.id,
                        &serde_json::to_value(status).unwrap_or_default(),
                    ),
                    Err(error) => manager_error_response(&request.id, error),
                },
                Err(msg) => crate::ipc::Response::error(&request.id, "INVALID_PARAMS", msg),
            },
            "manager.runtime_status" => match parse_id(&request.params) {
                Ok(id) => match self.manager.runtime_status(&id) {
                    Ok(status) => json_response(
                        &request.id,
                        &serde_json::to_value(status).unwrap_or_default(),
                    ),
                    Err(error) => manager_error_response(&request.id, error),
                },
                Err(msg) => crate::ipc::Response::error(&request.id, "INVALID_PARAMS", msg),
            },
            "manager.permissions" => match parse_id(&request.params) {
                Ok(id) => match self.manager.permissions(&id) {
                    Ok(perms) => {
                        json_response(&request.id, &serde_json::json!({ "permissions": perms }))
                    }
                    Err(error) => manager_error_response(&request.id, error),
                },
                Err(msg) => crate::ipc::Response::error(&request.id, "INVALID_PARAMS", msg),
            },
            "manager.set_permission" => match parse_set_permission(&request.params) {
                Ok((id, permission, decision)) => {
                    match self.manager.set_permission(&id, &permission, decision) {
                        Ok(()) => json_response(&request.id, &serde_json::json!({ "ok": true })),
                        Err(error) => manager_error_response(&request.id, error),
                    }
                }
                Err(msg) => crate::ipc::Response::error(&request.id, "INVALID_PARAMS", msg),
            },
            _ => crate::ipc::Response::error(
                &request.id,
                "UNKNOWN_METHOD",
                format!("no such manager method: {method}"),
            ),
        }
    }
}

fn json_response(id: &str, value: &serde_json::Value) -> crate::ipc::Response {
    crate::ipc::Response::success(id, value.clone())
}

fn parse_id(params: &serde_json::Value) -> Result<String, String> {
    params
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
        .ok_or_else(|| "missing `id` parameter".to_owned())
}

fn parse_install(params: &serde_json::Value) -> Result<(PathBuf, InstallOptions), String> {
    let path = params
        .get("packagePath")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing `packagePath`".to_owned())?;
    let require_signature = params
        .get("requireSignature")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let trusted_key = params
        .get("trustedKey")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned());
    Ok((
        PathBuf::from(path),
        InstallOptions {
            require_signature,
            trusted_key,
        },
    ))
}

fn parse_uninstall(params: &serde_json::Value) -> Result<(String, UninstallOptions), String> {
    let id = params
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing `id`".to_owned())?;
    let remove_data = params
        .get("removeData")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Ok((id.to_owned(), UninstallOptions { remove_data }))
}

fn parse_set_permission(
    params: &serde_json::Value,
) -> Result<(String, String, PermissionDecision), String> {
    let id = params
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing `id`".to_owned())?
        .to_owned();
    let permission = params
        .get("permission")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing `permission`".to_owned())?
        .to_owned();
    let decision_str = params
        .get("decision")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing `decision`".to_owned())?;
    let decision = match decision_str {
        "granted" => PermissionDecision::Granted,
        "denied" => PermissionDecision::Denied,
        "prompt" => PermissionDecision::Prompt,
        other => return Err(format!("invalid decision: {other}")),
    };
    Ok((id, permission, decision))
}

fn manager_error_response(id: &str, error: ManagerError) -> crate::ipc::Response {
    let code = match &error {
        ManagerError::NotFound(_) => "NOT_FOUND",
        ManagerError::UndeclaredPermission(_) => "UNDECLARED_PERMISSION",
        ManagerError::Io(_) => "IO_ERROR",
        ManagerError::Registry(_) => "REGISTRY_ERROR",
        ManagerError::Package(_)
        | ManagerError::Authorization(_)
        | ManagerError::Alex(_)
        | ManagerError::Runtime(_) => "OPERATION_FAILED",
    };
    crate::ipc::Response::error(id, code, error.to_string())
}

fn summary_from(manifest: &AppManifest, record: &AppRecord, path: &Path) -> AppSummary {
    AppSummary {
        id: manifest.id.clone(),
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        description: manifest.description.clone(),
        path: path.to_path_buf(),
        install_source: record.source,
        last_launched_at: record.last_launched_at.clone(),
        publisher_fingerprint: record.publisher_fingerprint.clone(),
        signed: record.signed,
    }
}

fn permission_method_name(permission: &Permission) -> String {
    // Mirror the IPC method name the runtime uses. Keep in sync with api.rs.
    match permission {
        Permission::FilesystemRead { .. } => "filesystem.readText".into(),
        Permission::FilesystemWrite { .. } => "filesystem.writeText".into(),
        Permission::DialogOpen => "dialog.openFile".into(),
        Permission::ClipboardRead => "clipboard.readText".into(),
        Permission::ClipboardWrite => "clipboard.writeText".into(),
        Permission::OpenExternal { .. } => "system.openExternal".into(),
        Permission::WindowManage => "window.setTitle".into(),
        Permission::NotificationShow => "notification.show".into(),
        Permission::RuntimeInvoke => "runtime.invoke".into(),
        Permission::RuntimeManage => "runtime.restart".into(),
        Permission::MediaCamera => "media.camera".into(),
        Permission::MediaMicrophone => "media.microphone".into(),
        Permission::Geolocation => "geolocation".into(),
        Permission::SystemInstall => "system.install".into(),
        Permission::SystemUninstall => "system.uninstall".into(),
        Permission::SystemManageApps => "system.manageApps".into(),
        Permission::SystemManageExtensions => "system.manageExtensions".into(),
    }
}

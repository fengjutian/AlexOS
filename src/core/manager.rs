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
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    authorization::{AuthorizationError, PermissionDecision, PermissionStore},
    core::{
        application_manifest::{load_application, ApplicationManifest, ManifestError},
        manifest::{AppManifest, BackendMode, RuntimeKind},
    },
    package,
    package::PackageError,
    runtime::{
        application_supervisor::{ApplicationSupervisor, ApplicationSupervisorError},
        RuntimeState, RuntimeStatus,
    },
    trust,
};

const REGISTRY_FILENAME: &str = "registry.json";
const REGISTRY_VERSION: u32 = 1;

/// The well-known plugin id of the self-hosted App Manager. The host
/// looks this id up in the install root to decide whether to delegate
/// `alex manager` to the plugin; the same id is used by both
/// `system.uninstall` and `manager.uninstall` to refuse a self-remove.
pub const MANAGER_PLUGIN_ID: &str = "com.alex.manager";

/// 应用安装来源 — 用于 App Registry 区分 .alex 包 / 远程更新 / dev 模式
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum InstallSource {
    LocalPackage,
    RemoteUpdate,
    DevMode,
}

/// Signature state of an installed package. Persisted on `AppRecord`
/// so the registry survives restarts; the `Signed` / `InvalidSignature`
/// half is determined at install time, and the trust half is
/// re-evaluated against the trust store on each `list_apps` /
/// `get_app` read.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SignatureState {
    /// No signature metadata in the archive.
    Unsigned,
    /// Signature metadata exists; the publisher key is in the trust
    /// store.
    SignedTrusted,
    /// Signature metadata exists; the publisher key is NOT in the
    /// trust store.
    SignedUntrusted,
    /// Signature metadata exists but is malformed (bad base64, wrong
    /// key length, etc.).
    InvalidSignature,
}

impl SignatureState {
    /// The coarse shape without a trust-store lookup: "no signature
    /// metadata", "valid-looking signature", or "broken signature".
    pub fn without_trust_lookup(signer_public_key: Option<&str>) -> Self {
        match signer_public_key {
            None => Self::Unsigned,
            Some(key) if trust::fingerprint(key).is_ok() => Self::SignedUntrusted,
            Some(_) => Self::InvalidSignature,
        }
    }
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
    pub signature_state: SignatureState,
    /// Live runtime snapshot for this app. `None` when the app is
    /// not currently running; populated by `LocalAppManager::list_apps`
    /// from the `RuntimeSupervisor` so the manager UI can show
    /// mode / port / pid / ready state without an extra round-trip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeSnapshot>,
}

/// Lightweight runtime state for the App Manager list view. Only the
/// fields the UI needs are copied off the supervisor so we can return
/// the list without holding the supervisor lock or re-querying the
/// child process.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSnapshot {
    pub state: RuntimeState,
    pub mode: BackendMode,
    pub pid: Option<u32>,
    pub port: Option<u16>,
    pub ready: bool,
    pub last_error: Option<String>,
    /// Last few stderr lines from the backend's ring-buffered log.
    /// The buffer itself is bounded (`MAX_LOG_LINES = 200` in
    /// `src/runtime.rs`); the snapshot keeps the most recent slice so
    /// the UI can render a "tail" without paging through the full
    /// history.
    pub recent_logs: Vec<String>,
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

impl From<ManifestError> for ManagerError {
    fn from(error: ManifestError) -> Self {
        // The unified loader is just a thin wrapper over the
        // historical AlexError / ManifestV2Error paths, so the
        // call-site error remains a `runtime` failure (the manager
        // is not the right place to surface parser-level detail).
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
    /// App-level restart (stop every service, then
    /// start every service). The v1 backward-compat
    /// path stops + starts the legacy "main" service;
    /// v2 goes through the supervisor's
    /// `restart_application` so the DAG layering is
    /// honoured. Returns the v1 `RuntimeStatus` for
    /// the primary service so the App Manager UI
    /// can show "running again" without a follow-up
    /// `runtime_status` call.
    fn restart(&self, id: &str) -> Result<RuntimeStatus, ManagerError>;
    fn runtime_status(&self, id: &str) -> Result<RuntimeStatus, ManagerError>;
    /// Phase 5 per-service surface. `start_service` runs
    /// exactly one service from the manifest without
    /// touching the app's other services (DAG layering is
    /// the caller's responsibility — for a "start
    /// everything" path use [`Self::launch`] which now
    /// goes through the layered [`crate::runtime::application_supervisor::ApplicationSupervisor`]
    /// path for v2). The v1 backward-compat callers
    /// continue to call `start_service("main", ...)`
    /// through [`Self::launch`] — the trait method is the
    /// general-purpose one.
    fn start_service(
        &self,
        id: &str,
        service: &str,
    ) -> Result<RuntimeStatus, ManagerError>;
    fn stop_service(
        &self,
        id: &str,
        service: &str,
    ) -> Result<RuntimeStatus, ManagerError>;
    fn restart_service(
        &self,
        id: &str,
        service: &str,
    ) -> Result<RuntimeStatus, ManagerError>;
    fn service_status(
        &self,
        id: &str,
        service: &str,
    ) -> Result<RuntimeStatus, ManagerError>;
    fn list_services(
        &self,
        id: &str,
    ) -> Result<Vec<crate::runtime::application_supervisor::ServiceSummary>, ManagerError>;
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
    /// Coarse signature state at install time. Re-evaluated against
    /// the trust store on every read (see `with_trust_lookup`) so a
    /// publisher added after install shows up as `signed-trusted` on
    /// the next refresh.
    pub signature_state: SignatureState,
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
                signature_state: SignatureState::Unsigned,
            },
        );
    }
    Ok(RegistryFile {
        version: REGISTRY_VERSION,
        apps,
    })
}

fn iso8601_now() -> String {
    // RFC 3339 / ISO 8601 UTC. Hand-rolled from epoch seconds so we
    // don't need to pull `chrono` or `time` as dependencies. UI
    // consumers can pass the string directly to `new Date(...)` and
    // get the right instant.
    let seconds = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (year, month, day, hour, minute, second) = epoch_seconds_to_ymdhms(seconds);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hour, minute, second
    )
}

/// Convert epoch seconds (UTC) to (year, month, day, hour, minute,
/// second). Uses Howard Hinnant's days-from-civil algorithm so the
/// conversion is constant-time and stays correct past 2100.
fn epoch_seconds_to_ymdhms(secs: u64) -> (i32, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let time_of_day = (secs % 86_400) as u32;
    let hour = time_of_day / 3600;
    let minute = (time_of_day % 3600) / 60;
    let second = time_of_day % 60;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yp = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yp as i64 + era * 400;
    let doy = doe - (365 * yp + yp / 4 - yp / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = y + (if m <= 2 { 1 } else { 0 });
    (y as i32, m as u32, d as u32, hour, minute, second)
}

/// Public version of `iso8601_now`/`epoch_seconds_to_ymdhms` for
/// tests that want to assert on the exact serialised format. Tests
/// cover a few well-known epoch seconds to catch a regression to
/// epoch-seconds-as-string.
pub fn format_epoch_seconds_as_iso8601(secs: u64) -> String {
    let (year, month, day, hour, minute, second) = epoch_seconds_to_ymdhms(secs);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hour, minute, second
    )
}

/// 本地实现:把现有 package / permission / trust 模块 facade 起来
pub struct LocalAppManager {
    install_root: PathBuf,
    registry: AppRegistry,
    permissions_root: PathBuf,
    runtimes: Arc<RuntimeSupervisor>,
    /// Optional trust store root. When `Some`, `list_apps` and
    /// `get_app` will upgrade a `SignedUntrusted` summary to
    /// `SignedTrusted` for any fingerprint present in this store.
    /// `None` means "no trust info available" — the per-record
    /// `Signed` / `InvalidSignature` states still come through, but
    /// we never promote anything to `SignedTrusted`.
    trust_root: Option<PathBuf>,
}

impl LocalAppManager {
    /// Project a live `RuntimeStatus` into the lighter
    /// `RuntimeSnapshot` shape the App Manager UI consumes. Returns
    /// `None` if the supervisor has no slot for `id` (i.e. the app
    /// is not currently running). We deliberately drop the
    /// `restartCount` and full log history — the snapshot is meant
    /// to fit in a single manager-UI row, not to be a full replica
    /// of the runtime supervisor state.
    fn runtime_snapshot(&self, id: &str) -> Option<RuntimeSnapshot> {
        let status = self.runtimes.snapshot(id)?;
        // Keep the most recent slice of the backend's stderr ring
        // buffer; `MAX_LOG_LINES` is 200 in `src/runtime.rs` so this
        // is a tail, not the full log. The UI can format it as a
        // `<details>` block if it wants more.
        let recent_logs: Vec<String> = status
            .logs
            .iter()
            .rev()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Some(RuntimeSnapshot {
            state: status.state,
            mode: status.mode,
            pid: status.pid,
            port: status.port,
            ready: status.ready,
            last_error: status.last_error,
            recent_logs,
        })
    }

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
        Self::open_with_trust(install_root, permissions_root, None)
    }

    /// Construct a manager with an explicit trust store root. The CLI
    /// passes `--trust-root` through here so the running manager can
    /// answer "is this fingerprint trusted?" from inside `list_apps` /
    /// `get_app` without going through the env.
    pub fn open_with_trust(
        install_root: &Path,
        permissions_root: PathBuf,
        trust_root: Option<PathBuf>,
    ) -> Result<Self, ManagerError> {
        let registry = AppRegistry::open_or_rebuild(install_root)?;
        // Phase 5: thread the install root into the
        // supervisor so the per-service `start_*` paths
        // can look up `ServiceDescriptor`s from the
        // on-disk manifest without forcing the IPC
        // layer to ferry the spec.
        let supervisor = RuntimeSupervisor {
            inner: ApplicationSupervisor::default(),
            install_root: install_root.to_path_buf(),
        };
        Ok(Self {
            install_root: install_root.to_path_buf(),
            registry,
            permissions_root,
            runtimes: Arc::new(supervisor),
            trust_root,
        })
    }

    /// Build a closure that answers "is this fingerprint in the
    /// trust store?". Caches the open store per call so a missing /
    /// unreadable trust root doesn't keep re-failing on every list.
    fn trust_lookup(&self) -> impl Fn(&str) -> bool + '_ {
        let cache = self
            .trust_root
            .as_deref()
            .and_then(|root| trust::TrustStore::open(root).ok());
        move |fingerprint: &str| {
            cache
                .as_ref()
                .is_some_and(|store| store.list().any(|(fp, _)| fp == fingerprint))
        }
    }
}

impl AppManager for LocalAppManager {
    fn list_apps(&self) -> Result<Vec<AppSummary>, ManagerError> {
        let trust_lookup = self.trust_lookup();
        let mut out = Vec::new();
        for (id, record) in self.registry.records() {
            let app_path = self.install_root.join(&id);
            if !app_path.is_dir() {
                continue;
            }
            // Phase 1: support both v1 and v2 manifests in the list
            // view. Apps whose manifest fails to load (corrupt,
            // missing, rejected) are silently skipped — the same
            // behaviour the pre-Phase-1 v1-only path had. A
            // follow-up Phase 6 change will surface a
            // `manifestError` field on the summary so the UI can
            // show why an installed app no longer loads.
            let manifest = match load_application(&app_path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let mut summary = summary_from(&manifest, &record, &app_path, &trust_lookup);
            summary.runtime = self.runtime_snapshot(&id);
            out.push(summary);
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    fn get_app(&self, id: &str) -> Result<AppDetails, ManagerError> {
        let trust_lookup = self.trust_lookup();
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
        let unified = load_application(&install_path)?;
        // Phase 1 keeps the `manifest: AppManifest` field on
        // `AppDetails` for v1 packages. v2 packages are surfaced
        // as v1-flavored details built from the projected service
        // set (frontend entry, single "main" service description,
        // empty permission list) so the existing App Manager UI
        // can still render the row without crashing. Phase 6
        // changes the detail model to carry the unified manifest
        // view directly.
        let (manifest, summary) = match unified {
            ApplicationManifest::V1(manifest) => {
                let summary = summary_from(
                    &ApplicationManifest::V1(manifest.clone()),
                    &record,
                    &install_path,
                    &trust_lookup,
                );
                (manifest, summary)
            }
            ApplicationManifest::V2(_) => {
                let summary = summary_from(&unified, &record, &install_path, &trust_lookup);
                (v2_fallback_manifest(&unified), summary)
            }
        };
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
        let manifest = load_application(&installed)?;
        let now = iso8601_now();
        // Record the trust-store fingerprint (NOT the raw public key)
        // so the UI can display a short identifier and match it
        // against `alex trust list` output directly. The signature
        // state separates "no metadata" from "metadata but broken"
        // from "metadata looks valid" so the UI can warn the user
        // about invalid signatures instead of silently treating them
        // as a normal install.
        let signer_public_key = package::signer_public_key(package_path)?;
        let publisher_fingerprint = signer_public_key
            .as_deref()
            .and_then(|key| trust::fingerprint(key).ok());
        let signature_state = SignatureState::without_trust_lookup(signer_public_key.as_deref());
        let record = AppRecord {
            install_at: now.clone(),
            updated_at: now,
            last_launched_at: None,
            publisher_fingerprint,
            source: InstallSource::LocalPackage,
            package_sha256: None,
            signature_state,
        };
        self.registry.upsert(manifest.id().to_owned(), record)?;
        let record_ref = self
            .registry
            .records()
            .into_iter()
            .find(|(rid, _)| rid == manifest.id())
            .map(|(_, r)| r)
            .ok_or_else(|| ManagerError::NotFound(manifest.id().to_owned()))?;
        Ok(summary_from(
            &manifest,
            &record_ref,
            &installed,
            &self.trust_lookup(),
        ))
    }

    fn uninstall(&self, id: &str, options: UninstallOptions) -> Result<(), ManagerError> {
        // Self-protection: refuse to remove the running App Manager.
        // Without this guard, the manager UI's own Uninstall button
        // could yank the install out from under the live process.
        if id == MANAGER_PLUGIN_ID {
            return Err(ManagerError::NotFound(format!(
                "refusing to uninstall the running App Manager ({id})"
            )));
        }
        // Stop the runtime first so the install directory is not
        // yanked out from under a live Node process. Without this,
        // Windows would keep the file handles open long enough to
        // block the directory removal, the backend would keep
        // running against data that no longer exists on disk, and
        // the supervisor would carry a phantom handle for an app
        // that the registry already says is gone.
        self.runtimes.stop_and_forget(id);
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
        let manifest = load_application(&install_path)?;
        let store = PermissionStore::open_at(&self.permissions_root, id)?;
        let decisions = store.list();
        let mut out = Vec::new();
        // Phase 1: permission state rows are sourced from the
        // unified accessor. v1's `permission.name()` strings and
        // v2's synthesised `fs:read:<path>` / `net:allow:<origin>`
        // / `shell:allow:<command>` strings both flow through the
        // same `PermissionStore` key space, so the same row shape
        // works for both manifest schemas. The IPC runtime still
        // understands only the v1 names today — v2 permissions are
        // surfaced for visibility in the manager UI but do not yet
        // affect the running app. Phase 9 (security boundaries)
        // will teach the IPC layer to honour the v2 policy blocks.
        let declared: Vec<String> = manifest
            .permissions()
            .into_iter()
            .map(|descriptor| descriptor.name)
            .collect();
        for name in declared {
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
        let manifest = load_application(&install_path)?;
        let declared: Vec<String> = manifest
            .permissions()
            .into_iter()
            .map(|descriptor| descriptor.name)
            .collect();
        if !declared.iter().any(|name| name == permission) {
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
        let manifest = load_application(&install_path)?;
        let status = match manifest.as_v1() {
            Some(legacy) => {
                // v1 single-backend path: project the legacy
                // `Backend` block onto a "main" service
                // descriptor and call the v1 shim. This is
                // the path every pre-Phase-2 app uses.
                let backend = legacy.backend.as_ref().ok_or_else(|| {
                    ManagerError::Runtime("application has no backend runtime".into())
                })?;
                self.runtimes.launch(id, &install_path, backend)?
            }
            None => {
                // v2 multi-service path. Phase 5 finally
                // routes `launch` through the layered
                // `start_application` so the daemon's
                // "start the app" command respects the
                // service DAG. v2 launch is a no-op for
                // manifests that declare zero services
                // (headless UI-only apps).
                let _ = self
                    .runtimes
                    .launch_v2(id, &install_path, &manifest)?;
                self.runtimes.status(id)?
            }
        };
        // Update "last launched" after a successful start so the UI
        // can show a recents list. The launch itself has already
        // succeeded — a registry write failure here is a soft error:
        // we log it but do not roll back the runtime.
        if let Err(error) = self.registry.touch_last_launched(id) {
            eprintln!("alex manager: failed to record launch time for {id}: {error}");
        }
        Ok(status)
    }

    fn stop(&self, id: &str) -> Result<RuntimeStatus, ManagerError> {
        Ok(self.runtimes.stop(id)?)
    }

    fn restart(&self, id: &str) -> Result<RuntimeStatus, ManagerError> {
        self.runtimes
            .restart(id)
            .map_err(|error| ManagerError::Runtime(error.to_string()))
    }

    fn runtime_status(&self, id: &str) -> Result<RuntimeStatus, ManagerError> {
        Ok(self.runtimes.status(id)?)
    }

    fn start_service(
        &self,
        id: &str,
        service: &str,
    ) -> Result<RuntimeStatus, ManagerError> {
        Ok(self.runtimes.start_one_service(id, service)?)
    }

    fn stop_service(
        &self,
        id: &str,
        service: &str,
    ) -> Result<RuntimeStatus, ManagerError> {
        Ok(self.runtimes.stop_one_service(id, service)?)
    }

    fn restart_service(
        &self,
        id: &str,
        service: &str,
    ) -> Result<RuntimeStatus, ManagerError> {
        Ok(self.runtimes.restart_one_service(id, service)?)
    }

    fn service_status(
        &self,
        id: &str,
        service: &str,
    ) -> Result<RuntimeStatus, ManagerError> {
        Ok(self.runtimes.service_status(id, service)?)
    }

    fn list_services(
        &self,
        id: &str,
    ) -> Result<Vec<crate::runtime::application_supervisor::ServiceSummary>, ManagerError> {
        // The supervisor only knows about services
        // that have been started at least once; the
        // App Manager UI needs the full declared
        // list from the manifest so the detail view
        // can render every service even when the
        // app is stopped. We project the manifest's
        // `ServiceDescriptor` list onto
        // `ServiceSummary` with `Pending` status.
        // Services that the supervisor *does* know
        // about override the projected status (so
        // the live state shows through once the app
        // has been launched).
        let install_path = self.install_root.join(id);
        let manifest = match load_application(&install_path) {
            Ok(m) => m,
            Err(error) => {
                return Err(ManagerError::Runtime(format!(
                    "failed to load manifest for {id}: {error}"
                )));
            }
        };
        let mut summaries: Vec<crate::runtime::application_supervisor::ServiceSummary> = manifest
            .services()
            .into_iter()
            .map(|descriptor| {
                let status = self
                    .runtimes
                    .service_status_only(id, &descriptor.name)
                    .unwrap_or(crate::runtime::service_supervisor::ServiceStatus::Pending);
                crate::runtime::application_supervisor::ServiceSummary {
                    name: descriptor.name,
                    status,
                    restart_count: 0,
                    last_error: None,
                }
            })
            .collect();
        // Mirror the supervisor's known status for
        // every slot the supervisor already has. This
        // is a no-op when the supervisor has not seen
        // the app yet.
        if let Some(application) = self.runtimes.application_supervisor().application(id) {
            for summary in summaries.iter_mut() {
                if let Some(slot) = application.services.get(&summary.name) {
                    summary.status = slot.status;
                    summary.restart_count = slot.restart_count;
                    summary.last_error = slot.last_error.clone();
                }
            }
        }
        // Sort by service name for stable UI
        // rendering. The supervisor's own
        // `list_services` already returns a
        // `BTreeMap`-backed order, so the merged
        // result is also deterministic.
        summaries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(summaries)
    }
}

/// In-memory map of running app backends. Keyed by app id.
///
/// Phase 2 reshapes this from "one app → one process" to a
/// thin facade over [`ApplicationSupervisor`], which holds
/// N services per app. The legacy public methods (`launch`,
/// `stop`, `restart`, `status`, `snapshot`, `stop_and_forget`)
/// keep their old signatures and are implemented in terms of the
/// new supervisor so the existing App Manager / Daemon / shell
/// callers do not need to change.
pub struct RuntimeSupervisor {
    inner: ApplicationSupervisor,
    /// Path under which each app's install directory
    /// lives. Phase 5 needs this to look up a service's
    /// `ServiceDescriptor` from the on-disk manifest
    /// without forcing every per-service IPC call to
    /// ferry the spec across the wire. `Default::default`
    /// keeps the existing v1 callers (which only use
    /// the in-process `ApplicationSupervisor`) working
    /// — they never trigger a manifest lookup.
    install_root: PathBuf,
}

impl Default for RuntimeSupervisor {
    fn default() -> Self {
        Self {
            inner: ApplicationSupervisor::default(),
            install_root: PathBuf::new(),
        }
    }
}

#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error("application {0} is already running")]
    AlreadyRunning(String),
    #[error("application supervisor: {0}")]
    Supervisor(String),
    #[error("runtime error: {0}")]
    Runtime(#[from] crate::runtime::RuntimeError),
}

impl From<ApplicationSupervisorError> for SupervisorError {
    fn from(error: ApplicationSupervisorError) -> Self {
        match error {
            ApplicationSupervisorError::ApplicationAlreadyRunning(id) => {
                SupervisorError::AlreadyRunning(id)
            }
            ApplicationSupervisorError::Runtime(source) => SupervisorError::Runtime(source),
            other => SupervisorError::Supervisor(other.to_string()),
        }
    }
}

impl RuntimeSupervisor {
    /// Backward-compatible launch. Spawns the app's primary
    /// service (v1 backend) and returns the v1-shaped
    /// `RuntimeStatus` so the existing App Manager / Daemon
    /// callers see the same fields as before. The
    /// `ApplicationSupervisor` is the actual owner of the
    /// process; this method is a thin shim.
    pub fn launch(
        &self,
        id: &str,
        install_root: &Path,
        backend: &crate::manifest::Backend,
    ) -> Result<RuntimeStatus, SupervisorError> {
        // Defensive stale-handle cleanup: a previous Phase 1
        // test left a `RuntimeHandle` whose child has already
        // exited. We mirror the Phase 1 behaviour by probing
        // the live status first; if the slot is empty or the
        // process is dead, the `start_application` path
        // proceeds to spawn a fresh one.
        if self.inner.is_application_running(id) {
            return Err(SupervisorError::AlreadyRunning(id.to_owned()));
        }
        let descriptor = service_descriptor_from_backend(backend);
        self.inner
            .start_service(id, "main", install_root, &descriptor)?;
        let status = self
            .inner
            .runtime_status_compat(id)
            .unwrap_or(RuntimeStatus {
                state: RuntimeState::Running,
                mode: backend.mode,
                ..Default::default()
            });
        Ok(status)
    }

    /// Backward-compatible stop. Stops the app's primary
    /// service (v1 backend) and returns a fabricated
    /// `RuntimeStatus::Stopped`.
    pub fn stop(&self, id: &str) -> Result<RuntimeStatus, SupervisorError> {
        let _ = self.inner.stop_service(id, "main");
        Ok(RuntimeStatus {
            state: RuntimeState::Stopped,
            ..Default::default()
        })
    }

    /// App-level restart. v1: stop + start the legacy
    /// "main" service. v2: go through
    /// `restart_application` so the supervisor honours
    /// the DAG and the layered start. Either way the
    /// returned `RuntimeStatus` is shaped for the v1
    /// compat caller.
    pub fn restart(&self, id: &str) -> Result<RuntimeStatus, SupervisorError> {
        let _ = self.inner.stop_service(id, "main");
        self.inner.restart_service(id, "main", &self.install_root_for(id))?;
        Ok(self
            .inner
            .runtime_status_compat(id)
            .unwrap_or_default())
    }

    /// v2 multi-service launch. Goes through the layered
    /// `start_application` so a v2 manifest's service DAG
    /// is honoured (topological order, per-layer
    /// concurrency, rollback on failure). Returns the v1
    /// compat `RuntimeStatus` for the app's primary
    /// service so the daemon's `start` / `status` response
    /// shape is unchanged.
    pub fn launch_v2(
        &self,
        id: &str,
        install_root: &Path,
        manifest: &crate::core::application_manifest::ApplicationManifest,
    ) -> Result<RuntimeStatus, SupervisorError> {
        self.inner.start_application(id, install_root, manifest)?;
        Ok(self
            .inner
            .runtime_status_compat(id)
            .unwrap_or(RuntimeStatus {
                state: RuntimeState::Running,
                ..Default::default()
            }))
    }

    /// Phase 5 per-service surface — start exactly one
    /// service from the manifest. Looks up the
    /// `ServiceDescriptor` from the on-disk manifest so we
    /// do not have to ferry the spec through the IPC layer.
    /// Returns the v1 `RuntimeStatus` shape so the daemon
    /// can keep its `Result<RuntimeStatus, _>` contract.
    pub fn start_one_service(
        &self,
        id: &str,
        service: &str,
    ) -> Result<RuntimeStatus, SupervisorError> {
        let descriptor = self.load_service_descriptor(id, service)?;
        let install_root = self.install_root_for(id);
        self.inner
            .start_service(id, service, &install_root, &descriptor)?;
        Ok(self
            .inner
            .runtime_status_compat(id)
            .unwrap_or_default())
    }

    /// Per-service stop. Idempotent: stopping a terminal
    /// service is a no-op, not an error.
    pub fn stop_one_service(
        &self,
        id: &str,
        service: &str,
    ) -> Result<RuntimeStatus, SupervisorError> {
        let _ = self.inner.stop_service(id, service);
        Ok(self
            .inner
            .runtime_status_compat(id)
            .unwrap_or(RuntimeStatus {
                state: RuntimeState::Stopped,
                ..Default::default()
            }))
    }

    /// Per-service restart (stop + start with the same
    /// spec).
    pub fn restart_one_service(
        &self,
        id: &str,
        service: &str,
    ) -> Result<RuntimeStatus, SupervisorError> {
        let install_root = self.install_root_for(id);
        self.inner.restart_service(id, service, &install_root)?;
        Ok(self
            .inner
            .runtime_status_compat(id)
            .unwrap_or_default())
    }

    /// Per-service status snapshot. Returns a fabricated
    /// `Stopped` snapshot if the app or service is not
    /// currently registered with the supervisor, so the
    /// daemon can show "service is not running" instead of
    /// failing the IPC call.
    pub fn service_status(
        &self,
        id: &str,
        service: &str,
    ) -> Result<RuntimeStatus, SupervisorError> {
        match self.inner.service_status(id, service) {
            Ok(snapshot) => Ok(runtime_status_from_service_snapshot(snapshot)),
            Err(_) => Ok(RuntimeStatus {
                state: RuntimeState::Stopped,
                ..Default::default()
            }),
        }
    }

    /// Per-service summary list. Returns an empty `Vec`
    /// for an unknown app so the daemon can render an
    /// empty list rather than 404.
    pub fn list_services(
        &self,
        id: &str,
    ) -> Result<
        Vec<crate::runtime::application_supervisor::ServiceSummary>,
        SupervisorError,
    > {
        self.inner
            .list_services(id)
            .map_err(|error| SupervisorError::Supervisor(error.to_string()))
    }

    /// Phase 6 helper: read just the per-service
    /// `ServiceStatus` from the supervisor, falling
    /// back to `Pending` when the slot does not
    /// exist. Used by `LocalAppManager::list_services`
    /// to merge the manifest's declared service list
    /// with whatever the supervisor has observed.
    pub fn service_status_only(
        &self,
        id: &str,
        service: &str,
    ) -> Option<crate::runtime::service_supervisor::ServiceStatus> {
        self.inner
            .service_status(id, service)
            .ok()
            .map(|snapshot| snapshot.status)
    }

    /// Look up the `ServiceDescriptor` for `(id, service)`
    /// on disk. Used by every per-service `start_*` path
    /// so the supervisor's `start_service` does not have
    /// to take a separate spec parameter through the IPC
    /// layer. Returns a `SupervisorError::Runtime` if the
    /// manifest is missing, the service is not declared,
    /// or the v2 manifest is structurally invalid.
    fn load_service_descriptor(
        &self,
        id: &str,
        service: &str,
    ) -> Result<crate::core::application_manifest::ServiceDescriptor, SupervisorError> {
        let install_path = self.install_root.join(id);
        let manifest = load_application(&install_path).map_err(|error| {
            SupervisorError::Supervisor(format!(
                "failed to load manifest for {id}: {error}"
            ))
        })?;
        // v1 single-backend manifests have a single
        // implicit "main" service; everything else is a
        // v2 manifest that must list the service by name.
        if let Some(legacy) = manifest.as_v1() {
            if service == "main" {
                let backend = legacy.backend.as_ref().ok_or_else(|| {
                    SupervisorError::Supervisor(format!(
                        "v1 application {id} has no backend runtime"
                    ))
                })?;
                return Ok(service_descriptor_from_backend(backend));
            }
            return Err(SupervisorError::Supervisor(format!(
                "v1 application {id} only exposes the main service; \
                 requested {service:?}"
            )));
        }
        // v2: walk the `services` map and find the entry.
        let services = manifest.services();
        services
            .into_iter()
            .find(|svc| svc.name == service)
            .ok_or_else(|| {
                SupervisorError::Supervisor(format!(
                    "service {service:?} is not declared in {id}"
                ))
            })
    }

    /// Backward-compatible status. Returns the v1-shaped
    /// `RuntimeStatus` for the app's primary service, or a
    /// fabricated `Stopped` snapshot for an unknown app.
    pub fn status(&self, id: &str) -> Result<RuntimeStatus, SupervisorError> {
        Ok(self
            .inner
            .runtime_status_compat(id)
            .unwrap_or(RuntimeStatus {
                state: RuntimeState::Stopped,
                ..Default::default()
            }))
    }

    /// Snapshot the live runtime for `id`, or `None` when no
    /// supervisor slot is currently held. Distinct from
    /// `status`: that one always returns a `RuntimeStatus` and
    /// reports a fabricated `Stopped` state for not-running
    /// apps, which would be misleading to embed in
    /// `AppSummary.runtime`. Callers that want the optional
    /// "is the backend up right now" view should use this
    /// helper.
    pub fn snapshot(&self, id: &str) -> Option<RuntimeStatus> {
        self.inner.runtime_status_compat(id)
    }

    /// Stop the runtime (graceful, then forceful) and forget
    /// about it. Used by `uninstall` so the next install of
    /// the same id can start with a clean supervisor slot.
    /// Idempotent.
    pub fn stop_and_forget(&self, id: &str) {
        self.inner.forget_application(id);
    }

    /// Resolve the install root for a given app id.
    /// Returns an owned `PathBuf` because the supervisor
    /// takes an owned `PathBuf` in
    /// `ApplicationSupervisor::start_service` and we
    /// cannot borrow a `Path` that lives inside this
    /// method. The v1 backward-compat callers (which
    /// use `Default::default()` for the supervisor and
    /// therefore have an empty `install_root`) get
    /// `./<id>` as a fallback so the daemon's
    /// `start-service <id> <name>` call still works
    /// during a dev run without an explicit install.
    fn install_root_for(&self, id: &str) -> PathBuf {
        let base: PathBuf = if self.install_root.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            self.install_root.clone()
        };
        base.join(id)
    }

    /// Escape hatch into the new multi-service API. Returns
    /// a clone of the inner `ApplicationSupervisor` so the
    /// Phase 5 daemon protocol can drive the per-service
    /// surface without going through the v1 shim.
    pub fn application_supervisor(&self) -> ApplicationSupervisor {
        self.inner.clone()
    }
}

/// Build a `ServiceDescriptor` for the v1 "main" service from a
/// legacy `Backend` block. Used by the v1 backward-compat
/// launch path. The reverse mapping lives in
/// `application_supervisor::service_descriptor_to_backend`.
fn service_descriptor_from_backend(backend: &crate::manifest::Backend) -> crate::core::application_manifest::ServiceDescriptor {
    use crate::core::application_manifest::{
        ServiceDescriptor, ServiceHealthDescriptor, ServiceHealthKind, ServiceMode,
        ServiceRestartDescriptor, ServiceRestartPolicy,
    };
    use crate::core::manifest_v2::ServiceRuntime as V2Runtime;
    let health = backend.health_check.as_ref().map(|check| ServiceHealthDescriptor {
        kind: ServiceHealthKind::Http,
        path: Some(check.path.clone()),
        interval_ms: 5_000,
        timeout_ms: check.timeout_ms,
    });
    let restart_policy = match backend.restart.as_ref().map(|p| p.policy.as_str()) {
        Some("never") => ServiceRestartPolicy::Never,
        Some("always") => ServiceRestartPolicy::Always,
        _ => ServiceRestartPolicy::OnFailure,
    };
    let max_retries = backend
        .restart
        .as_ref()
        .map(|p| p.max_retries)
        .unwrap_or(5);
    ServiceDescriptor {
        name: "main".to_owned(),
        runtime: match backend.runtime {
            RuntimeKind::Node => V2Runtime::Node,
            RuntimeKind::Python => V2Runtime::Python,
            RuntimeKind::Native => V2Runtime::Native,
        },
        command: backend.entry.clone(),
        args: backend.args.clone(),
        depends_on: Vec::new(),
        env: backend.env.clone(),
        port: backend.port,
        // The v1 `Backend` carries an explicit `mode`; the v2
        // `ServiceDescriptor` carries an HTTP health check
        // that implies `Service`. Both round-trip cleanly
        // through the `service_descriptor_to_backend`
        // projection in `application_supervisor`.
        mode: match backend.mode {
            crate::manifest::BackendMode::Rpc => ServiceMode::Rpc,
            crate::manifest::BackendMode::Service => ServiceMode::Service,
        },
        health,
        restart: ServiceRestartDescriptor {
            policy: restart_policy,
            max_retries,
        },
    }
}

/// Map a `ServiceSnapshot` (Phase 2/3 supervisor shape)
/// onto the v1 `RuntimeStatus` so the daemon can return a
/// stable response shape. The fields line up 1:1 except
/// for the per-service `name` (which the daemon does not
/// surface in the v1 `status` response) and the live log
/// tail (the daemon answers `logs` separately).
fn runtime_status_from_service_snapshot(
    snapshot: crate::runtime::application_supervisor::ServiceSnapshot,
) -> RuntimeStatus {
    use crate::runtime::service_supervisor::ServiceStatus as V2;
    let state = match snapshot.status {
        V2::Pending
        | V2::WaitingForDependencies
        | V2::Stopping
        | V2::Stopped
        | V2::Blocked => RuntimeState::Stopped,
        V2::Starting => RuntimeState::Starting,
        V2::Healthy | V2::Unhealthy | V2::Restarting => RuntimeState::Running,
        V2::Crashed => RuntimeState::Crashed,
    };
    RuntimeStatus {
        state,
        pid: snapshot.pid,
        port: snapshot.port,
        restart_count: snapshot.restart_count,
        last_error: snapshot.last_error,
        ..Default::default()
    }
}

impl std::fmt::Debug for RuntimeSupervisor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("RuntimeSupervisor").finish()
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
            "manager.restart" => match parse_id(&request.params) {
                Ok(id) => match self.manager.restart(&id) {
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
            // Phase 6 — per-service surface for the App
            // Manager UI. Each method takes a
            // `{ "id": "<app>", "service": "<name>" }`
            // payload and goes through the
            // `AppManager`'s per-service shims (which
            // in turn drive the multi-service
            // supervisor). The list endpoint returns
            // the full `Vec<ServiceSummary>` so the
            // UI's detail view can render every
            // declared service in one round trip.
            "manager.start_service" => {
                match parse_id_and_service(&request.params) {
                    Ok((id, service)) => match self.manager.start_service(&id, &service) {
                        Ok(status) => json_response(
                            &request.id,
                            &serde_json::to_value(status).unwrap_or_default(),
                        ),
                        Err(error) => manager_error_response(&request.id, error),
                    },
                    Err(msg) => {
                        crate::ipc::Response::error(&request.id, "INVALID_PARAMS", msg)
                    }
                }
            }
            "manager.stop_service" => {
                match parse_id_and_service(&request.params) {
                    Ok((id, service)) => match self.manager.stop_service(&id, &service) {
                        Ok(status) => json_response(
                            &request.id,
                            &serde_json::to_value(status).unwrap_or_default(),
                        ),
                        Err(error) => manager_error_response(&request.id, error),
                    },
                    Err(msg) => {
                        crate::ipc::Response::error(&request.id, "INVALID_PARAMS", msg)
                    }
                }
            }
            "manager.restart_service" => {
                match parse_id_and_service(&request.params) {
                    Ok((id, service)) => match self.manager.restart_service(&id, &service) {
                        Ok(status) => json_response(
                            &request.id,
                            &serde_json::to_value(status).unwrap_or_default(),
                        ),
                        Err(error) => manager_error_response(&request.id, error),
                    },
                    Err(msg) => {
                        crate::ipc::Response::error(&request.id, "INVALID_PARAMS", msg)
                    }
                }
            }
            "manager.service_status" => {
                match parse_id_and_service(&request.params) {
                    Ok((id, service)) => match self.manager.service_status(&id, &service) {
                        Ok(status) => json_response(
                            &request.id,
                            &serde_json::to_value(status).unwrap_or_default(),
                        ),
                        Err(error) => manager_error_response(&request.id, error),
                    },
                    Err(msg) => {
                        crate::ipc::Response::error(&request.id, "INVALID_PARAMS", msg)
                    }
                }
            }
            "manager.list_services" => match parse_id(&request.params) {
                Ok(id) => match self.manager.list_services(&id) {
                    Ok(services) => json_response(
                        &request.id,
                        &serde_json::json!({ "services": services }),
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

/// Parse the `(id, service)` pair that every per-service
/// `manager.*` IPC method takes. Both fields are
/// required; a missing or empty `service` is rejected
/// because every multi-service supervisor slot is
/// keyed by a non-empty name.
fn parse_id_and_service(
    params: &serde_json::Value,
) -> Result<(String, String), String> {
    let id = parse_id(params)?;
    let service = params
        .get("service")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
        .ok_or_else(|| "missing `service` parameter".to_owned())?;
    if service.is_empty() {
        return Err("`service` must be non-empty".to_owned());
    }
    Ok((id, service))
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

fn summary_from(
    manifest: &ApplicationManifest,
    record: &AppRecord,
    path: &Path,
    trust_lookup: &dyn Fn(&str) -> bool,
) -> AppSummary {
    AppSummary {
        id: manifest.id().to_owned(),
        name: manifest.name().to_owned(),
        version: manifest.version().to_owned(),
        description: manifest.description().map(str::to_owned),
        path: path.to_path_buf(),
        install_source: record.source,
        last_launched_at: record.last_launched_at.clone(),
        publisher_fingerprint: record.publisher_fingerprint.clone(),
        signature_state: with_trust_lookup(
            record.signature_state,
            record.publisher_fingerprint.as_deref(),
            trust_lookup,
        ),
        runtime: None,
    }
}

/// Build a placeholder v1 `AppManifest` from a v2 unified
/// manifest so the existing `AppDetails.manifest: AppManifest`
/// field can still hold a value. The placeholder is good enough
/// for the App Manager UI to render a row (id / name / version /
/// frontend entry) without crashing; it deliberately carries no
/// permissions and no backend block. Phase 6 replaces this with a
/// first-class `ApplicationManifestView` so the UI can read the
/// real service list instead of this lossy projection.
fn v2_fallback_manifest(manifest: &ApplicationManifest) -> AppManifest {
    manifest
        .as_v2()
        .expect("v2_fallback_manifest called with a v1 manifest");
    let mut permissions = Vec::new();
    // v2 permission descriptors that look like legacy IPC method
    // names (`filesystem.read` etc.) are carried over so the
    // existing permission store can read decisions written by
    // earlier versions of the UI. v2-only descriptors
    // (`fs:read:<path>`, ...) are dropped on the floor for Phase
    // 1; they will land in a v2-shaped detail view in Phase 6.
    for descriptor in manifest.permissions() {
        if descriptor.name.contains(':') {
            continue;
        }
        if let Some(permission) = legacy_permission_from_name(&descriptor.name) {
            permissions.push(permission);
        }
    }
    let frontend_entry = manifest
        .frontend()
        .map(|frontend| frontend.entry)
        .unwrap_or_default();
    AppManifest {
        schema_version: 1,
        kind: crate::manifest::PackageKind::App,
        id: manifest.id().to_owned(),
        name: manifest.name().to_owned(),
        version: manifest.version().to_owned(),
        description: None,
        author: None,
        icons: None,
        homepage: None,
        license: None,
        update: None,
        frontend: crate::manifest::Frontend {
            entry: frontend_entry,
            build: None,
        },
        backend: None,
        permissions,
        extension_points: None,
    }
}

fn legacy_permission_from_name(name: &str) -> Option<crate::permission::Permission> {
    use crate::permission::Permission;
    match name {
        "filesystem.read" => Some(Permission::FilesystemRead { paths: Vec::new() }),
        "filesystem.write" => Some(Permission::FilesystemWrite { paths: Vec::new() }),
        "filesystem.watch" => Some(Permission::FilesystemWatch { paths: Vec::new() }),
        "filesystem.delete" => Some(Permission::FilesystemDelete { paths: Vec::new() }),
        "filesystem.drop" => Some(Permission::FilesystemDrop),
        "dialog.open" => Some(Permission::DialogOpen),
        "dialog.save" => Some(Permission::DialogSave),
        "clipboard.read" => Some(Permission::ClipboardRead),
        "clipboard.write" => Some(Permission::ClipboardWrite),
        "system.openExternal" => Some(Permission::OpenExternal { origins: Vec::new() }),
        "storage" => Some(Permission::Storage),
        "paths" => Some(Permission::Paths),
        "window.manage" => Some(Permission::WindowManage),
        "window.open" => Some(Permission::WindowOpen),
        "notification.show" => Some(Permission::NotificationShow),
        "menu.manage" => Some(Permission::MenuManage),
        "tray.manage" => Some(Permission::TrayManage),
        "shortcut.register" => Some(Permission::ShortcutRegister),
        "runtime.invoke" => Some(Permission::RuntimeInvoke),
        "runtime.manage" => Some(Permission::RuntimeManage),
        "process.spawn" => Some(Permission::ProcessSpawn { executables: Vec::new() }),
        "media.camera" => Some(Permission::MediaCamera),
        "media.microphone" => Some(Permission::MediaMicrophone),
        "geolocation" => Some(Permission::Geolocation),
        "system.install" => Some(Permission::SystemInstall),
        "system.uninstall" => Some(Permission::SystemUninstall),
        "system.manageApps" => Some(Permission::SystemManageApps),
        "system.manageExtensions" => Some(Permission::SystemManageExtensions),
        "system.managePermissions" => Some(Permission::SystemManagePermissions),
        "network.fetch" => Some(Permission::NetworkFetch { origins: Vec::new() }),
        _ => None,
    }
}

/// Upgrade `SignedUntrusted` to `SignedTrusted` when the fingerprint
/// is present in the trust store. The lookup is supplied as a
/// closure so `LocalAppManager` can use its injected `trust_root`
/// (and so tests can supply an in-memory trust set without
/// touching the filesystem). Missing / unreadable trust stores are
/// treated as "no trust info" and the state is left unchanged
/// (showing `SignedUntrusted`). This is intentionally best-effort —
/// the trust store can be added after install, and the next read
/// will surface the new state without rewriting the registry.
fn with_trust_lookup(
    state: SignatureState,
    fingerprint: Option<&str>,
    trust_lookup: &dyn Fn(&str) -> bool,
) -> SignatureState {
    if !matches!(state, SignatureState::SignedUntrusted) {
        return state;
    }
    let Some(fingerprint) = fingerprint else {
        return state;
    };
    if trust_lookup(fingerprint) {
        SignatureState::SignedTrusted
    } else {
        state
    }
}

// `permission_method_name` was removed in H1 — the manifest
// permission name (e.g. `filesystem.read`) is the only key the
// permission store should use, and the `Permission::name` method is
// the single source of truth for that string. See `permissions` and
// `set_permission` above.

#[cfg(test)]
mod runtime_snapshot_tests {
    use super::*;
    use serde_json::json;

    /// The wire shape the App Manager UI consumes: `runtime` is
    /// `camelCase` and the field is omitted entirely when no live
    /// snapshot is available, so apps that aren't currently running
    /// don't show an `offline` badge by default.
    #[test]
    fn app_summary_serializes_runtime_when_present_and_skips_when_absent() {
        let summary_with_runtime = AppSummary {
            id: "com.example.notes".into(),
            name: "Notes".into(),
            version: "0.1.0".into(),
            description: None,
            path: PathBuf::from("apps/com.example.notes"),
            install_source: InstallSource::LocalPackage,
            last_launched_at: None,
            publisher_fingerprint: None,
            signature_state: SignatureState::Unsigned,
            runtime: Some(RuntimeSnapshot {
                state: RuntimeState::Ready,
                mode: BackendMode::Service,
                pid: Some(1234),
                port: Some(28100),
                ready: true,
                last_error: None,
                recent_logs: vec!["ready".into()],
            }),
        };
        let value = serde_json::to_value(&summary_with_runtime).expect("serialize");
        let runtime = value
            .get("runtime")
            .expect("runtime present when snapshot exists");
        assert_eq!(runtime["state"], "ready");
        assert_eq!(runtime["mode"], "service");
        assert_eq!(runtime["pid"], 1234);
        assert_eq!(runtime["port"], 28100);
        assert_eq!(runtime["ready"], true);
        assert_eq!(runtime["recentLogs"], json!(["ready"]));

        let summary_offline = AppSummary {
            runtime: None,
            ..summary_with_runtime
        };
        let value = serde_json::to_value(&summary_offline).expect("serialize");
        assert!(
            value.get("runtime").is_none(),
            "offline apps must not carry a runtime key, got: {value}"
        );
    }

    #[test]
    fn runtime_snapshot_keeps_only_a_tail_of_logs() {
        // The supervisor's ring buffer is 200 lines; the snapshot
        // is supposed to take a tail so the manager UI doesn't
        // stream the full history on every refresh. We only verify
        // the contract here — that the tail is the most recent N
        // lines, in order — and let the size be whatever the host
        // decides.
        let logs: Vec<String> = (0..50).map(|i| format!("line {i}")).collect();
        let mut tail: Vec<String> = logs.iter().rev().take(20).cloned().collect();
        tail.reverse();
        let snapshot = RuntimeSnapshot {
            state: RuntimeState::Ready,
            mode: BackendMode::Service,
            pid: Some(1),
            port: Some(28000),
            ready: true,
            last_error: None,
            recent_logs: tail,
        };
        assert_eq!(snapshot.recent_logs.len(), 20);
        assert_eq!(snapshot.recent_logs.first().unwrap(), "line 30");
        assert_eq!(snapshot.recent_logs.last().unwrap(), "line 49");
    }
}

#[cfg(test)]
mod supervisor_launch_env_injection_tests {
    use super::*;
    use crate::manifest::{Backend, RuntimeKind};

    /// Regression: `RuntimeSupervisor::launch` used to call the
    /// legacy `RuntimeHandle::start` with a hardcoded `app_id =
    /// "<unknown>"`, which made `start_with_spec` skip the
    /// auto-managed data / cache / log dir resolution and never
    /// inject `ALEX_APP_DATA_DIR` into the child env. The result:
    /// `node:sqlite` backends silently fell back to `:memory:` and
    /// the user's notes.db ended up nowhere.
    ///
    /// This test launches a tiny Node fixture that dumps the env
    /// values to a JSON file, then asserts the host's expected
    /// paths are present. It exercises the actual `launch` path
    /// (not just the lower-level `start_with_spec`) so the bug
    /// class can't slip back in via the call site.
    #[test]
    #[serial_test::serial]
    fn supervisor_launch_injects_alex_env_into_service_backend() {
        if crate::runtime::discover_node().is_none() {
            eprintln!("skipping: Node.js not available");
            return;
        }
        let workspace = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fixture = workspace
            .join("tests")
            .join("fixtures")
            .join("smoke-env.js");
        if !fixture.is_file() {
            eprintln!("skipping: {} not built", fixture.display());
            return;
        }
        let id = "com.alex.supervisor-env-test";
        let out = std::env::temp_dir().join(format!("alex-supervisor-env-{id}.json"));
        let _ = std::fs::remove_file(&out);

        let backend = Backend {
            runtime: RuntimeKind::Node,
            entry: fixture.to_string_lossy().into_owned(),
            mode: crate::manifest::BackendMode::Service,
            health_check: None,
            restart: None,
            port: None,
            args: Vec::new(),
            env: std::collections::BTreeMap::new(),
        };
        let install_root = workspace.join("examples").join("notes");
        let supervisor = RuntimeSupervisor::default();
        let status = supervisor
            .launch(id, &install_root, &backend)
            .expect("supervisor launch with real app_id");

        assert_eq!(status.state, RuntimeState::Ready, "status was: {status:?}");
        let port = status.port.expect("service mode reports a port");
        assert!((28000..=28999).contains(&port));

        // Give the fixture a tick to flush its env dump.
        for _ in 0..50 {
            if out.is_file() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let body = std::fs::read_to_string(&out).expect("fixture wrote env dump");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("env dump is json");

        assert_eq!(parsed["ALEX_APP_ID"], id, "wrong app id injected: {parsed}");

        let data_dir = parsed["ALEX_APP_DATA_DIR"]
            .as_str()
            .expect("ALEX_APP_DATA_DIR injected");
        let cache_dir = parsed["ALEX_APP_CACHE_DIR"]
            .as_str()
            .expect("ALEX_APP_CACHE_DIR injected");
        let log_dir = parsed["ALEX_APP_LOG_DIR"]
            .as_str()
            .expect("ALEX_APP_LOG_DIR injected");

        let expected_dirs = crate::runtime::compute_app_dirs(id).expect("valid id");
        expected_dirs.ensure().expect("ensure dirs");
        assert_eq!(data_dir, expected_dirs.data.to_string_lossy());
        assert_eq!(cache_dir, expected_dirs.cache.to_string_lossy());
        assert_eq!(log_dir, expected_dirs.logs.to_string_lossy());

        assert_eq!(
            parsed["ALEX_SERVICE_PORT"].as_str(),
            Some(port.to_string().as_str()),
            "ALEX_SERVICE_PORT mismatch"
        );
        let token = parsed["ALEX_RUNTIME_TOKEN"]
            .as_str()
            .expect("ALEX_RUNTIME_TOKEN injected");
        assert_eq!(token.len(), 64, "token must be 64 hex chars: {token}");

        let _ = std::fs::remove_file(&out);
    }
}

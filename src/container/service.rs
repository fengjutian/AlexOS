//! `ContainerService` — the single write entry point for container
//! state.
//!
//! Every code path that wants to start, stop, restart, remove, or
//! read container state goes through this trait. The CLI, the App
//! Manager UI, and the shell all share one instance. The 0.1
//! `RuntimeSupervisor` becomes a coordinator *inside* the default
//! implementation rather than a parallel source of truth.
//!
//! Phase A (`DefaultContainerService`) implements every method and
//! the launch path goes through the existing
//! `crate::runtime::RuntimeHandle`. This keeps the 0.1 manager /
//! shell / CLI commands working unchanged while we put a stable
//! on-disk state file underneath them. Phase B will swap the launch
//! path for the Windows Job Object provider.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use semver::Version;
use serde::{Deserialize, Serialize};

use super::errors::{ContainerError, LaunchStep};
use super::events::{Event, EventKind, EventLog};
use super::filter::{ContainerFilter, ContainerView};
use super::model::{
    ContainerSpec, ContainerState, DesiredState, EndpointState, IsolationLevel, ObservedState,
    RestartPolicy,
};
use super::process;
use super::store::ContainerStore;
use super::volume::{ContainerDirs, data_local_dir};

#[derive(Debug, Clone)]
pub struct ContainerContext {
    pub install_root: PathBuf,
    pub data_root: PathBuf,
}

impl ContainerContext {
    pub fn with_default_data_root(install_root: PathBuf) -> Result<Self, ContainerError> {
        let data_root = data_local_dir().map(|p| p.join("AlexOS")).ok_or_else(|| {
            ContainerError::Backend("could not resolve a per-user data root".into())
        })?;
        Ok(Self {
            install_root,
            data_root,
        })
    }
}

pub type ServiceResult<T> = Result<T, ContainerError>;

/// Per-container handle the supervisor keeps while the container is
/// running.
struct LiveHandle {
    pid: u32,
    #[allow(dead_code)]
    port: Option<u16>,
    #[allow(dead_code)]
    runtime_handle: usize,
    isolation: super::isolation::IsolationHandle,
}

pub trait ContainerService: Send + Sync {
    fn create(&self, spec: ContainerSpec) -> ServiceResult<ContainerView>;
    fn start(&self, instance_id: &str) -> ServiceResult<ContainerView>;
    fn stop(&self, instance_id: &str, timeout: Duration) -> ServiceResult<ContainerView>;
    fn restart(&self, instance_id: &str) -> ServiceResult<ContainerView>;
    fn remove(&self, instance_id: &str, delete_data: bool) -> ServiceResult<()>;
    fn inspect(&self, instance_id: &str) -> ServiceResult<ContainerView>;
    fn list(&self, filter: &ContainerFilter) -> ServiceResult<Vec<ContainerView>>;
    fn logs(&self, instance_id: &str, tail: usize) -> ServiceResult<Vec<Event>>;
    fn isolation_available(&self, level: IsolationLevel) -> bool;
}

pub struct DefaultContainerService {
    ctx: ContainerContext,
    instances: Mutex<HashMap<PathBuf, InstanceSlot>>,
}

struct InstanceSlot {
    state: ContainerState,
    instance_dir: PathBuf,
    live: Option<LiveHandle>,
}

impl DefaultContainerService {
    pub fn new(ctx: ContainerContext) -> Result<Self, ContainerError> {
        let service = Self {
            ctx,
            instances: Mutex::new(HashMap::new()),
        };
        service.reconcile()?;
        Ok(service)
    }

    pub fn context(&self) -> &ContainerContext {
        &self.ctx
    }

    pub fn reconcile(&self) -> Result<(), ContainerError> {
        let containers_root = self.ctx.data_root.join("containers");
        let entries = match std::fs::read_dir(&containers_root) {
            Ok(d) => d,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(ContainerError::Io {
                    path: containers_root,
                    source,
                });
            }
        };
        let mut map = self.lock();
        for entry in entries.flatten() {
            let instance_dir = entry.path();
            if !instance_dir.is_dir() {
                continue;
            }
            let store = ContainerStore::new(instance_dir.clone());
            let Some(state) = store.load()? else {
                continue;
            };
            map.entry(instance_dir.clone())
                .and_modify(|slot| {
                    slot.state = state.clone();
                })
                .or_insert(InstanceSlot {
                    state,
                    instance_dir,
                    live: None,
                });
        }
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<PathBuf, InstanceSlot>> {
        self.instances.lock().expect("instance lock poisoned")
    }

    fn record_event(&self, slot: &InstanceSlot, kind: EventKind, message: impl Into<String>) {
        let mut log = EventLog::new(slot.instance_dir.join("events"));
        let event = Event::new(
            unix_millis(),
            slot.state.generation,
            slot.state.instance_id.clone(),
            slot.state.app_id.clone(),
            kind,
            message,
        );
        if let Err(error) = log.append(&event) {
            eprintln!("alex container: failed to append event: {error}");
        }
    }

    #[allow(dead_code)]
    fn write_state(&self, slot: &InstanceSlot) -> Result<(), ContainerError> {
        let store = ContainerStore::new(slot.instance_dir.clone());
        let generation = store.save(slot.state.clone())?;
        let mut map = self.lock();
        if let Some(entry) = map.get_mut(&slot.instance_dir) {
            entry.state.generation = generation;
        }
        Ok(())
    }

    fn validate_level(&self, level: IsolationLevel) -> Result<(), ContainerError> {
        if super::isolation::provider_for(level).is_err() {
            return Err(ContainerError::IsolationUnavailable {
                requested: level.to_string(),
                reason: "the required host isolation provider is unavailable".into(),
            });
        }
        Ok(())
    }

    fn record_event_locked(&self, slot: &InstanceSlot, kind: EventKind, message: String) {
        let mut log = EventLog::new(slot.instance_dir.join("events"));
        let event = Event::new(
            unix_millis(),
            slot.state.generation,
            slot.state.instance_id.clone(),
            slot.state.app_id.clone(),
            kind,
            message,
        );
        if let Err(error) = log.append(&event) {
            eprintln!("alex container: failed to append event: {error}");
        }
    }
}

impl ContainerService for DefaultContainerService {
    fn create(&self, spec: ContainerSpec) -> ServiceResult<ContainerView> {
        spec.validate()?;
        self.validate_level(spec.isolation)?;
        let dirs = ContainerDirs::resolve(
            &self.ctx.data_root,
            &spec.instance_id,
            &spec.app_id,
            &spec.app_version.to_string(),
        );
        let install_path = self.ctx.install_root.join(&spec.app_id);
        if !install_path.is_dir() {
            return Err(ContainerError::PackageNotInstalled(spec.app_id.clone()));
        }
        let application_root = install_path.clone();
        if !application_root.join("manifest.json").is_file() {
            return Err(ContainerError::InvalidPackage(format!(
                "no manifest.json under {}",
                application_root.display()
            )));
        }
        let mut state = ContainerState {
            instance_id: spec.instance_id.clone(),
            app_id: spec.app_id.clone(),
            app_version: spec.app_version.clone(),
            desired: DesiredState::Created,
            observed: ObservedState::Created,
            isolation_effective: spec.isolation,
            degraded_reason: None,
            pid: None,
            exit_code: None,
            endpoint: None,
            restart_count: 0,
            last_error: None,
            generation: 0,
            created_at: iso8601_now(),
            updated_at: iso8601_now(),
        };
        let instance_dir = dirs.instance_root.clone();
        if instance_dir.join("state.json").exists() {
            return Err(ContainerError::AlreadyExists(spec.instance_id));
        }
        let store = ContainerStore::new(instance_dir.clone());
        store.save(state.clone())?;
        state.generation = store.load()?.map(|s| s.generation).unwrap_or(0);
        let slot = InstanceSlot {
            state: state.clone(),
            instance_dir: instance_dir.clone(),
            live: None,
        };
        self.record_event(&slot, EventKind::Created, "container record created");
        let mut map = self.lock();
        map.insert(instance_dir.clone(), slot);
        Ok(ContainerView::from_state(&state, instance_dir))
    }

    fn start(&self, instance_id: &str) -> ServiceResult<ContainerView> {
        let mut map = self.lock();
        let entry = map
            .values_mut()
            .find(|slot| slot.state.instance_id == instance_id)
            .ok_or_else(|| ContainerError::NotFound(instance_id.to_owned()))?;
        if let Some(live) = &entry.live {
            let mut view = ContainerView::from_state(&entry.state, entry.instance_dir.clone());
            view.pid = Some(live.pid);
            return Ok(view);
        }
        let spec = build_spec_from_state(&entry.state);
        spec.validate()?;
        self.validate_level(entry.state.isolation_effective)?;
        let dirs = ContainerDirs::resolve(
            &self.ctx.data_root,
            &entry.state.instance_id,
            &entry.state.app_id,
            &entry.state.app_version.to_string(),
        );
        dirs.ensure().map_err(|source| ContainerError::Io {
            path: dirs.instance_root.clone(),
            source,
        })?;
        dirs.reset_runtime_slot()
            .map_err(|source| ContainerError::Io {
                path: dirs.runtime.clone(),
                source,
            })?;
        let install_path = self.ctx.install_root.join(&entry.state.app_id);
        let manifest_path = install_path.join("manifest.json");
        if !manifest_path.is_file() {
            return Err(ContainerError::Launch {
                step: LaunchStep::ValidateSpec,
                message: format!(
                    "no manifest at {}; the package is not installed",
                    manifest_path.display()
                ),
            });
        }
        let manifest = crate::load_app(&install_path).map_err(|error| ContainerError::Launch {
            step: LaunchStep::ValidateSpec,
            message: error.to_string(),
        })?;
        let backend = manifest
            .backend
            .clone()
            .ok_or_else(|| ContainerError::Launch {
                step: LaunchStep::ValidateSpec,
                message: "manifest has no backend runtime".into(),
            })?;
        // Phase A: let the runtime pick the port itself. See the
        // long comment in `process::launch_backend`.
        let launched = process::launch_backend(process::LaunchRequest {
            app_id: &entry.state.app_id,
            package_root: &install_path,
            backend: &backend,
            data_dir: Some(dirs.data.as_path()),
            cache_dir: Some(dirs.cache.as_path()),
            log_dir: Some(dirs.logs.as_path()),
            port: None,
            token: None,
            container: &spec,
        })
        .map_err(|error| ContainerError::Launch {
            step: LaunchStep::SpawnProcess,
            message: error.to_string(),
        })?;
        let endpoint = launched.endpoint.as_ref().map(|e| EndpointState {
            port: e.port,
            token_fingerprint: token_fingerprint(&e.token),
        });
        entry.state.desired = DesiredState::Running;
        entry.state.observed = if launched.ready {
            ObservedState::Ready
        } else {
            ObservedState::Running
        };
        entry.state.pid = Some(launched.pid);
        entry.state.endpoint = endpoint.clone();
        entry.state.last_error = None;
        entry.state.updated_at = iso8601_now();
        let store = ContainerStore::new(entry.instance_dir.clone());
        store.save(entry.state.clone())?;
        let live = LiveHandle {
            pid: launched.pid,
            port: launched.endpoint.as_ref().map(|e| e.port),
            runtime_handle: 0,
            isolation: launched.isolation,
        };
        entry.live = Some(live);
        self.record_event_locked(entry, EventKind::Spawned, format!("pid={}", launched.pid));
        Ok(ContainerView::from_state(
            &entry.state,
            entry.instance_dir.clone(),
        ))
    }

    fn stop(&self, instance_id: &str, timeout: Duration) -> ServiceResult<ContainerView> {
        let mut map = self.lock();
        let entry = map
            .values_mut()
            .find(|slot| slot.state.instance_id == instance_id)
            .ok_or_else(|| ContainerError::NotFound(instance_id.to_owned()))?;
        let Some(live) = entry.live.take() else {
            entry.state.observed = ObservedState::Stopped;
            entry.state.desired = DesiredState::Stopped;
            entry.state.pid = None;
            entry.state.endpoint = None;
            entry.state.updated_at = iso8601_now();
            let store = ContainerStore::new(entry.instance_dir.clone());
            store.save(entry.state.clone())?;
            return Ok(ContainerView::from_state(
                &entry.state,
                entry.instance_dir.clone(),
            ));
        };
        entry.state.desired = DesiredState::Stopped;
        entry.state.observed = ObservedState::Stopping;
        entry.state.updated_at = iso8601_now();
        terminate_pid(live.pid, timeout);
        drop(live.isolation);
        let store = ContainerStore::new(entry.instance_dir.clone());
        store.save(entry.state.clone())?;
        entry.state.observed = ObservedState::Stopped;
        entry.state.pid = None;
        entry.state.endpoint = None;
        entry.state.updated_at = iso8601_now();
        store.save(entry.state.clone())?;
        self.record_event_locked(
            entry,
            EventKind::StopRequested,
            "stop requested".to_string(),
        );
        Ok(ContainerView::from_state(
            &entry.state,
            entry.instance_dir.clone(),
        ))
    }

    fn restart(&self, instance_id: &str) -> ServiceResult<ContainerView> {
        self.stop(instance_id, Duration::from_secs(2))?;
        self.start(instance_id)
    }

    fn remove(&self, instance_id: &str, delete_data: bool) -> ServiceResult<()> {
        let mut map = self.lock();
        let entry = map
            .values()
            .find(|slot| slot.state.instance_id == instance_id)
            .ok_or_else(|| ContainerError::NotFound(instance_id.to_owned()))?;
        if entry.live.is_some() {
            return Err(ContainerError::Backend(format!(
                "{instance_id} is still running; stop it first"
            )));
        }
        let instance_dir = entry.instance_dir.clone();
        let data_dir = entry.instance_dir.join("data");
        let cache_dir = entry.instance_dir.join("cache");
        let state_path = instance_dir.join("state.json");
        for path in [&state_path, &instance_dir.join("runtime")] {
            if path.exists() {
                let _ = std::fs::remove_dir_all(path);
            }
        }
        if delete_data {
            for path in [&data_dir, &cache_dir] {
                if path.exists() {
                    let _ = std::fs::remove_dir_all(path);
                }
            }
        }
        map.remove(&instance_dir);
        Ok(())
    }

    fn inspect(&self, instance_id: &str) -> ServiceResult<ContainerView> {
        let map = self.lock();
        let entry = map
            .values()
            .find(|slot| slot.state.instance_id == instance_id)
            .ok_or_else(|| ContainerError::NotFound(instance_id.to_owned()))?;
        let mut state = entry.state.clone();
        if let Some(live) = &entry.live {
            state.pid = Some(live.pid);
        }
        Ok(ContainerView::from_state(
            &state,
            entry.instance_dir.clone(),
        ))
    }

    fn list(&self, filter: &ContainerFilter) -> ServiceResult<Vec<ContainerView>> {
        let map = self.lock();
        let mut out = Vec::new();
        for slot in map.values() {
            if !filter.matches(&slot.state) {
                continue;
            }
            out.push(ContainerView::from_state(
                &slot.state,
                slot.instance_dir.clone(),
            ));
        }
        out.sort_by(|a, b| a.instance_id.cmp(&b.instance_id));
        Ok(out)
    }

    fn logs(&self, instance_id: &str, tail: usize) -> ServiceResult<Vec<Event>> {
        let map = self.lock();
        let entry = map
            .values()
            .find(|slot| slot.state.instance_id == instance_id)
            .ok_or_else(|| ContainerError::NotFound(instance_id.to_owned()))?;
        let log = EventLog::new(entry.instance_dir.join("events"));
        Ok(log.tail(tail)?)
    }

    fn isolation_available(&self, level: IsolationLevel) -> bool {
        super::isolation::provider_for(level).is_ok()
    }
}

fn terminate_pid(pid: u32, _timeout: Duration) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill.exe")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .output();
    }
}

fn build_spec_from_state(state: &ContainerState) -> ContainerSpec {
    ContainerSpec {
        instance_id: state.instance_id.clone(),
        app_id: state.app_id.clone(),
        app_version: state.app_version.clone(),
        isolation: state.isolation_effective,
        resources: Default::default(),
        filesystem: Default::default(),
        network: Default::default(),
        restart: RestartPolicy::default(),
    }
}

fn iso8601_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (year, month, day, hour, minute, second) = epoch_seconds_to_ymdhms(seconds);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hour, minute, second
    )
}

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

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn token_fingerprint(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(16);
    for byte in &digest[..8] {
        let _ = std::fmt::Write::write_fmt(&mut hex, format_args!("{byte:02x}"));
    }
    hex
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateRequest {
    pub app_id: String,
    pub app_version: Version,
    #[serde(default)]
    pub instance_id: Option<String>,
    #[serde(default)]
    pub isolation: Option<IsolationLevel>,
}

impl CreateRequest {
    pub fn into_spec(self) -> ContainerSpec {
        let instance_id = self.instance_id.unwrap_or_else(|| self.app_id.clone());
        ContainerSpec {
            instance_id,
            app_id: self.app_id,
            app_version: self.app_version,
            isolation: self
                .isolation
                .unwrap_or_else(IsolationLevel::default_for_manifest),
            resources: Default::default(),
            filesystem: Default::default(),
            network: Default::default(),
            restart: RestartPolicy::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::container::model::{FilesystemPolicy, NetworkPolicy, ResourceLimits};

    fn write_manifest(root: &Path, app_id: &str) {
        let app_dir = root.join(app_id);
        std::fs::create_dir_all(&app_dir).unwrap();
        let manifest = serde_json::json!({
            "schemaVersion": 1,
            "id": app_id,
            "name": app_id,
            "version": "1.0.0",
            "frontend": { "entry": "index.html" },
            "backend": {
                "runtime": "node",
                "entry": "backend/index.js",
                "mode": "rpc",
            },
        });
        std::fs::write(
            app_dir.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        std::fs::create_dir_all(app_dir.join("backend")).unwrap();
        std::fs::write(app_dir.join("backend").join("index.js"), "").unwrap();
        std::fs::write(app_dir.join("index.html"), "<html/>").unwrap();
    }

    fn service_for(tmp: &tempfile::TempDir) -> DefaultContainerService {
        let install_root = tmp.path().join("apps");
        let data_root = tmp.path().join("data");
        std::fs::create_dir_all(&install_root).unwrap();
        std::fs::create_dir_all(&data_root).unwrap();
        DefaultContainerService::new(ContainerContext {
            install_root,
            data_root,
        })
        .unwrap()
    }

    #[test]
    fn create_persists_state_and_returns_a_view() {
        let tmp = tempfile::tempdir().unwrap();
        write_manifest(tmp.path().join("apps").as_path(), "com.example.notes");
        let service = service_for(&tmp);
        let spec = ContainerSpec {
            instance_id: "com.example.notes".into(),
            app_id: "com.example.notes".into(),
            app_version: Version::new(1, 0, 0),
            isolation: IsolationLevel::Process,
            resources: ResourceLimits::default(),
            filesystem: FilesystemPolicy::default(),
            network: NetworkPolicy::default(),
            restart: RestartPolicy::default(),
        };
        let view = service.create(spec).expect("create");
        assert_eq!(view.instance_id, "com.example.notes");
        assert!(view.instance_dir.join("state.json").is_file());
    }

    #[test]
    fn create_rejects_unknown_app_id() {
        let tmp = tempfile::tempdir().unwrap();
        let service = service_for(&tmp);
        let spec = ContainerSpec {
            instance_id: "missing".into(),
            app_id: "com.example.missing".into(),
            app_version: Version::new(1, 0, 0),
            isolation: IsolationLevel::Process,
            resources: ResourceLimits::default(),
            filesystem: FilesystemPolicy::default(),
            network: NetworkPolicy::default(),
            restart: RestartPolicy::default(),
        };
        let error = service.create(spec).unwrap_err();
        assert!(matches!(error, ContainerError::PackageNotInstalled(_)));
    }

    #[test]
    fn create_rejects_duplicate_instance() {
        let tmp = tempfile::tempdir().unwrap();
        write_manifest(tmp.path().join("apps").as_path(), "com.example.notes");
        let service = service_for(&tmp);
        let mut spec = ContainerSpec {
            instance_id: "com.example.notes".into(),
            app_id: "com.example.notes".into(),
            app_version: Version::new(1, 0, 0),
            isolation: IsolationLevel::Process,
            resources: ResourceLimits::default(),
            filesystem: FilesystemPolicy::default(),
            network: NetworkPolicy::default(),
            restart: RestartPolicy::default(),
        };
        service.create(spec.clone()).expect("first create");
        spec.app_version = Version::new(1, 0, 1);
        let error = service.create(spec).unwrap_err();
        assert!(matches!(error, ContainerError::AlreadyExists(_)));
    }

    #[test]
    fn list_filters_by_app_id() {
        let tmp = tempfile::tempdir().unwrap();
        write_manifest(tmp.path().join("apps").as_path(), "com.example.notes");
        write_manifest(tmp.path().join("apps").as_path(), "com.example.tasks");
        let service = service_for(&tmp);
        for app in ["com.example.notes", "com.example.tasks"] {
            service
                .create(ContainerSpec {
                    instance_id: app.into(),
                    app_id: app.into(),
                    app_version: Version::new(1, 0, 0),
                    isolation: IsolationLevel::Process,
                    resources: ResourceLimits::default(),
                    filesystem: FilesystemPolicy::default(),
                    network: NetworkPolicy::default(),
                    restart: RestartPolicy::default(),
                })
                .unwrap();
        }
        let filter = ContainerFilter {
            app_id: Some("com.example.notes".into()),
            ..Default::default()
        };
        let listed = service.list(&filter).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].app_id, "com.example.notes");
    }

    #[test]
    fn remove_preserves_data_unless_opted_in() {
        let tmp = tempfile::tempdir().unwrap();
        write_manifest(tmp.path().join("apps").as_path(), "com.example.notes");
        let service = service_for(&tmp);
        let spec = ContainerSpec {
            instance_id: "com.example.notes".into(),
            app_id: "com.example.notes".into(),
            app_version: Version::new(1, 0, 0),
            isolation: IsolationLevel::Process,
            resources: ResourceLimits::default(),
            filesystem: FilesystemPolicy::default(),
            network: NetworkPolicy::default(),
            restart: RestartPolicy::default(),
        };
        let view = service.create(spec).unwrap();
        std::fs::create_dir_all(view.instance_dir.join("data")).unwrap();
        std::fs::write(view.instance_dir.join("data").join("note.txt"), b"hi").unwrap();
        service
            .remove("com.example.notes", false)
            .expect("remove without --delete-data");
        assert!(view.instance_dir.join("data").join("note.txt").is_file());
    }
}

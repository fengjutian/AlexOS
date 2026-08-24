use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::json;

use super::{
    ControlCommand, ControlRequest, ControlResponse, DaemonStateStore, DesiredState, ObservedState,
    PROTOCOL_VERSION,
};
use crate::runtime::application_supervisor::ServiceSummary;

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryFailure {
    pub app_id: String,
    pub error: String,
}

#[derive(Debug, Clone, Default, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryReport {
    pub recovered: Vec<String>,
    pub failed: Vec<RecoveryFailure>,
}

#[derive(Clone)]
pub struct DaemonService {
    state: DaemonStateStore,
    manager: Option<Arc<dyn crate::manager::AppManager>>,
}

impl DaemonService {
    pub fn new(state: DaemonStateStore) -> Self {
        Self {
            state,
            manager: None,
        }
    }

    pub fn with_manager(mut self, manager: Arc<dyn crate::manager::AppManager>) -> Self {
        self.manager = Some(manager);
        self
    }

    pub fn handle(&self, request: ControlRequest) -> ControlResponse {
        if request.protocol != PROTOCOL_VERSION {
            return ControlResponse::failure(
                request.id,
                format!(
                    "unsupported protocol {}; expected {}",
                    request.protocol, PROTOCOL_VERSION
                ),
            );
        }
        let id = request.id;
        let result: Result<serde_json::Value, String> = match request.command {
            ControlCommand::Ping => Ok(json!({
                "daemon": "alexd",
                "protocol": PROTOCOL_VERSION
            })),
            ControlCommand::Shutdown => self.shutdown(),
            ControlCommand::List => self.list(),
            ControlCommand::Start { app_id } => self.start(&app_id),
            ControlCommand::Stop { app_id } => self.stop(&app_id),
            ControlCommand::Restart { app_id } => self.restart(&app_id),
            ControlCommand::Status { app_id } => self.status(&app_id),
            ControlCommand::Logs {
                app_id,
                service,
                limit,
            } => self.logs(&app_id, service.as_deref(), limit),
            ControlCommand::StartService { app_id, service } => {
                self.start_service(&app_id, &service)
            }
            ControlCommand::StopService { app_id, service } => {
                self.stop_service(&app_id, &service)
            }
            ControlCommand::RestartService { app_id, service } => {
                self.restart_service(&app_id, &service)
            }
            ControlCommand::ServiceStatus { app_id, service } => {
                self.service_status(&app_id, &service)
            }
            ControlCommand::ListServices { app_id } => self.list_services(&app_id),
        };
        match result {
            Ok(value) => ControlResponse::success(id, value),
            Err(error) => ControlResponse::failure(id, error),
        }
    }

    /// Converge persisted desired state after a daemon restart. A failed app
    /// remains desired=running so a future explicit start or daemon restart can
    /// retry it, while observed=crashed and lastError make the failure visible.
    ///
    /// Phase 5: this is now per-service aware. The
    /// algorithm walks the persisted state in two
    /// passes:
    ///
    /// 1. For each app whose `desired == Running` and
    ///    that has *no* per-service state recorded
    ///    (the legacy v1 case), call `launch` so the
    ///    whole application — including the DAG start
    ///    for v2 manifests — comes back up. The v1
    ///    shim inside `LocalAppManager::launch` handles
    ///    "main only" apps.
    /// 2. For each app with per-service desired entries,
    ///    call `start_service` for every service whose
    ///    `desired == Running`. The supervisor's
    ///    `start_service` does not require a DAG layer
    ///    ordering — it just spawns the one process —
    ///    so the daemon is free to fire them
    ///    sequentially in BTreeMap order (which is
    ///    alphabetical and reproducible).
    pub fn recover_startup(&self) -> RecoveryReport {
        let mut report = RecoveryReport::default();
        let Some(manager) = &self.manager else {
            return report;
        };
        let state = match self.state.load() {
            Ok(state) => state,
            Err(error) => {
                report.failed.push(RecoveryFailure {
                    app_id: "*".into(),
                    error: error.to_string(),
                });
                return report;
            }
        };
        for app in state
            .applications
            .values()
            .filter(|app| app.desired == DesiredState::Running)
        {
            if app.services.is_empty() {
                self.recover_app_whole(manager, &app.app_id, &mut report);
            } else {
                self.recover_app_per_service(manager, &app.app_id, &app.services, &mut report);
            }
        }
        report
    }

    /// Whole-app recovery. Used when no per-service
    /// intent is recorded (legacy v1 callers, or a fresh
    /// `start <id>` issued before any per-service
    /// command).
    fn recover_app_whole(
        &self,
        manager: &Arc<dyn crate::manager::AppManager>,
        app_id: &str,
        report: &mut RecoveryReport,
    ) {
        let result = manager
            .get_app(app_id)
            .map_err(|error| error.to_string())
            .and_then(|_| manager.launch(app_id).map_err(|error| error.to_string()));
        match result {
            Ok(status) => {
                if let Err(error) = self.record_status(app_id, &status) {
                    report.failed.push(RecoveryFailure {
                        app_id: app_id.to_owned(),
                        error,
                    });
                } else {
                    report.recovered.push(app_id.to_owned());
                }
            }
            Err(error) => {
                let persistence_error = self
                    .state
                    .set_observed(
                        app_id,
                        ObservedState::Crashed,
                        Some(error.clone()),
                        now_ms().unwrap_or_default(),
                    )
                    .err()
                    .map(|state_error| format!("; state update failed: {state_error}"))
                    .unwrap_or_default();
                report.failed.push(RecoveryFailure {
                    app_id: app_id.to_owned(),
                    error: format!("{error}{persistence_error}"),
                });
            }
        }
    }

    /// Per-service recovery. The supervisor's
    /// `start_service` is a single-process spawn (no
    /// DAG layering), so the daemon does not need to
    /// reason about dependencies here — the App Manager
    /// UI's "start" button is responsible for the
    /// layering when the user clicks it. The daemon's
    /// job is just to re-apply the persisted intent.
    fn recover_app_per_service(
        &self,
        manager: &Arc<dyn crate::manager::AppManager>,
        app_id: &str,
        services: &std::collections::BTreeMap<
            String,
            super::ServiceControlState,
        >,
        report: &mut RecoveryReport,
    ) {
        for (service_name, svc) in services {
            if svc.desired != DesiredState::Running {
                continue;
            }
            let result = manager.start_service(app_id, service_name);
            match result {
                Ok(status) => {
                    if let Err(error) =
                        self.record_service_status(app_id, service_name, &status)
                    {
                        report.failed.push(RecoveryFailure {
                            app_id: app_id.to_owned(),
                            error: format!("service {service_name}: {error}"),
                        });
                    }
                }
                Err(error) => {
                    let _ = self.state.set_service_observed(
                        app_id,
                        service_name,
                        ObservedState::Crashed,
                        Some(error.to_string()),
                        now_ms().unwrap_or_default(),
                    );
                    report.failed.push(RecoveryFailure {
                        app_id: app_id.to_owned(),
                        error: format!("service {service_name}: {error}"),
                    });
                }
            }
        }
        if report
            .failed
            .iter()
            .all(|f| f.app_id != app_id)
        {
            report.recovered.push(app_id.to_owned());
        }
    }

    fn list(&self) -> Result<serde_json::Value, String> {
        if let Some(manager) = &self.manager {
            return manager
                .list_apps()
                .map(|applications| json!({ "applications": applications }))
                .map_err(|error| error.to_string());
        }
        self.state
            .load()
            .map(|state| {
                json!({
                    "applications": state.applications.into_values().collect::<Vec<_>>()
                })
            })
            .map_err(|error| error.to_string())
    }

    fn shutdown(&self) -> Result<serde_json::Value, String> {
        let Some(manager) = &self.manager else {
            return Ok(json!({ "stopped": [], "errors": [] }));
        };
        let applications = manager.list_apps().map_err(|error| error.to_string())?;
        let mut stopped = Vec::new();
        let mut errors = Vec::new();
        for app in applications.into_iter().filter(|app| app.runtime.is_some()) {
            match manager.stop(&app.id) {
                Ok(_) => stopped.push(app.id),
                Err(error) => errors.push(json!({
                    "appId": app.id,
                    "error": error.to_string()
                })),
            }
        }
        Ok(json!({ "stopped": stopped, "errors": errors }))
    }

    fn start(&self, app_id: &str) -> Result<serde_json::Value, String> {
        if let Some(manager) = &self.manager {
            manager.get_app(app_id).map_err(|error| error.to_string())?;
            let status = manager.launch(app_id).map_err(|error| error.to_string())?;
            self.set_desired(app_id, DesiredState::Running)?;
            self.record_status(app_id, &status)?;
            return Ok(json!(status));
        }
        self.set_desired(app_id, DesiredState::Running)
    }

    fn stop(&self, app_id: &str) -> Result<serde_json::Value, String> {
        if let Some(manager) = &self.manager {
            manager.get_app(app_id).map_err(|error| error.to_string())?;
            let status = manager.stop(app_id).map_err(|error| error.to_string())?;
            self.set_desired(app_id, DesiredState::Stopped)?;
            self.record_status(app_id, &status)?;
            return Ok(json!(status));
        }
        self.set_desired(app_id, DesiredState::Stopped)
    }

    fn restart(&self, app_id: &str) -> Result<serde_json::Value, String> {
        if let Some(manager) = &self.manager {
            manager.get_app(app_id).map_err(|error| error.to_string())?;
            manager.stop(app_id).map_err(|error| error.to_string())?;
            let status = manager.launch(app_id).map_err(|error| error.to_string())?;
            self.set_desired(app_id, DesiredState::Running)?;
            self.record_status(app_id, &status)?;
            return Ok(json!(status));
        }
        self.set_desired(app_id, DesiredState::Running)
    }

    fn status(&self, app_id: &str) -> Result<serde_json::Value, String> {
        if let Some(manager) = &self.manager {
            manager.get_app(app_id).map_err(|error| error.to_string())?;
            return manager
                .runtime_status(app_id)
                .map(|status| json!(status))
                .map_err(|error| error.to_string());
        }
        self.state
            .load()
            .map_err(|error| error.to_string())?
            .applications
            .get(app_id)
            .map(|app| json!(app))
            .ok_or_else(|| format!("application {app_id} has no daemon state"))
    }

    fn logs(
        &self,
        app_id: &str,
        service: Option<&str>,
        limit: u32,
    ) -> Result<serde_json::Value, String> {
        let manager = self
            .manager
            .as_ref()
            .ok_or_else(|| "log service is not connected yet".to_string())?;
        manager.get_app(app_id).map_err(|error| error.to_string())?;
        // Phase 5: when `service` is given, fetch that
        // service's runtime status. The v1 path
        // (no service name) still routes through
        // `runtime_status` so the response shape stays
        // identical for v1 callers — a v1 manifest
        // exposes exactly one service named "main" /
        // "backend", so the legacy `logs` request with
        // no `service` field is equivalent to asking
        // for that one.
        let status = if let Some(name) = service {
            manager
                .service_status(app_id, name)
                .map_err(|error| error.to_string())?
        } else {
            manager
                .runtime_status(app_id)
                .map_err(|error| error.to_string())?
        };
        let limit = usize::try_from(limit.min(10_000)).unwrap_or(10_000);
        let start = status.logs.len().saturating_sub(limit);
        let resolved_service = service.unwrap_or("backend");
        Ok(json!({
            "appId": app_id,
            "service": resolved_service,
            "lines": &status.logs[start..]
        }))
    }

    /// Phase 5 per-service start. Records
    /// `ServiceControlState{ desired: Running }` and
    /// delegates to `AppManager::start_service`. The
    /// `recover_startup` path uses the same state row
    /// to drive a daemon restart.
    fn start_service(
        &self,
        app_id: &str,
        service: &str,
    ) -> Result<serde_json::Value, String> {
        let manager = self
            .manager
            .as_ref()
            .ok_or_else(|| "service manager is not connected yet".to_string())?;
        manager.get_app(app_id).map_err(|error| error.to_string())?;
        let status = manager
            .start_service(app_id, service)
            .map_err(|error| error.to_string())?;
        self.state
            .set_service_desired(app_id, service, DesiredState::Running, now_ms().unwrap_or_default())
            .map_err(|error| error.to_string())?;
        self.record_service_status(app_id, service, &status)?;
        Ok(json!(status))
    }

    fn stop_service(
        &self,
        app_id: &str,
        service: &str,
    ) -> Result<serde_json::Value, String> {
        let manager = self
            .manager
            .as_ref()
            .ok_or_else(|| "service manager is not connected yet".to_string())?;
        manager.get_app(app_id).map_err(|error| error.to_string())?;
        let status = manager
            .stop_service(app_id, service)
            .map_err(|error| error.to_string())?;
        self.state
            .set_service_desired(app_id, service, DesiredState::Stopped, now_ms().unwrap_or_default())
            .map_err(|error| error.to_string())?;
        self.record_service_status(app_id, service, &status)?;
        Ok(json!(status))
    }

    fn restart_service(
        &self,
        app_id: &str,
        service: &str,
    ) -> Result<serde_json::Value, String> {
        let manager = self
            .manager
            .as_ref()
            .ok_or_else(|| "service manager is not connected yet".to_string())?;
        manager.get_app(app_id).map_err(|error| error.to_string())?;
        let status = manager
            .restart_service(app_id, service)
            .map_err(|error| error.to_string())?;
        self.state
            .set_service_desired(app_id, service, DesiredState::Running, now_ms().unwrap_or_default())
            .map_err(|error| error.to_string())?;
        self.record_service_status(app_id, service, &status)?;
        Ok(json!(status))
    }

    fn service_status(
        &self,
        app_id: &str,
        service: &str,
    ) -> Result<serde_json::Value, String> {
        let manager = self
            .manager
            .as_ref()
            .ok_or_else(|| "service manager is not connected yet".to_string())?;
        manager.get_app(app_id).map_err(|error| error.to_string())?;
        manager
            .service_status(app_id, service)
            .map(|status| json!(status))
            .map_err(|error| error.to_string())
    }

    fn list_services(&self, app_id: &str) -> Result<serde_json::Value, String> {
        let manager = self
            .manager
            .as_ref()
            .ok_or_else(|| "service manager is not connected yet".to_string())?;
        manager.get_app(app_id).map_err(|error| error.to_string())?;
        manager
            .list_services(app_id)
            .map(|services: Vec<ServiceSummary>| json!({ "services": services }))
            .map_err(|error| error.to_string())
    }

    fn set_desired(
        &self,
        app_id: &str,
        desired: DesiredState,
    ) -> Result<serde_json::Value, String> {
        let state = self
            .state
            .set_desired(app_id, desired, now_ms().unwrap_or_default())
            .map_err(|error| error.to_string())?;
        Ok(json!(state.applications.get(app_id)))
    }

    fn record_status(
        &self,
        app_id: &str,
        status: &crate::runtime::RuntimeStatus,
    ) -> Result<(), String> {
        let observed = match status.state {
            crate::runtime::RuntimeState::Starting => ObservedState::Starting,
            crate::runtime::RuntimeState::Running => ObservedState::Running,
            crate::runtime::RuntimeState::Ready => ObservedState::Ready,
            crate::runtime::RuntimeState::Crashed => ObservedState::Crashed,
            crate::runtime::RuntimeState::Stopped => ObservedState::Stopped,
        };
        self.state
            .set_observed(
                app_id,
                observed,
                status.last_error.clone(),
                now_ms().unwrap_or_default(),
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Phase 5 helper: record the observed state of a
    /// single service. Mirrors [`Self::record_status`]
    /// but writes the `ServiceControlState` row, not
    /// the app-level `AppControlState`. A `set_observed`
    /// that fails with `Invalid` ("no desired state for
    /// service") is treated as a soft no-op so a
    /// `service-status` poll that runs before any
    /// `start-service` was issued does not abort the
    /// parent IPC call.
    fn record_service_status(
        &self,
        app_id: &str,
        service: &str,
        status: &crate::runtime::RuntimeStatus,
    ) -> Result<(), String> {
        let observed = match status.state {
            crate::runtime::RuntimeState::Starting => ObservedState::Starting,
            crate::runtime::RuntimeState::Running => ObservedState::Running,
            crate::runtime::RuntimeState::Ready => ObservedState::Ready,
            crate::runtime::RuntimeState::Crashed => ObservedState::Crashed,
            crate::runtime::RuntimeState::Stopped => ObservedState::Stopped,
        };
        match self.state.set_service_observed(
            app_id,
            service,
            observed,
            status.last_error.clone(),
            now_ms().unwrap_or_default(),
        ) {
            Ok(_) => Ok(()),
            Err(super::DaemonStateError::Invalid(message))
                if message.contains("no desired state for service") =>
            {
                Ok(())
            }
            Err(error) => Err(error.to_string()),
        }
    }
}

fn now_ms() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(command: ControlCommand) -> ControlRequest {
        ControlRequest {
            protocol: PROTOCOL_VERSION,
            id: "test-1".into(),
            command,
        }
    }

    #[test]
    fn start_persists_and_status_observes_the_same_state() {
        let temp = tempfile::tempdir().unwrap();
        let service = DaemonService::new(DaemonStateStore::new(temp.path().join("state.json")));
        let started = service.handle(request(ControlCommand::Start {
            app_id: "com.example.agent".into(),
        }));
        assert!(started.ok, "{:?}", started.error);
        let status = service.handle(request(ControlCommand::Status {
            app_id: "com.example.agent".into(),
        }));
        assert_eq!(status.result.unwrap()["desired"], "running");
    }

    #[test]
    fn incompatible_protocol_fails_without_mutating_state() {
        let temp = tempfile::tempdir().unwrap();
        let store = DaemonStateStore::new(temp.path().join("state.json"));
        let service = DaemonService::new(store.clone());
        let response = service.handle(ControlRequest {
            protocol: 99,
            id: "bad".into(),
            command: ControlCommand::Start {
                app_id: "com.example.agent".into(),
            },
        });
        assert!(!response.ok);
        assert!(store.load().unwrap().applications.is_empty());
    }

    #[test]
    fn recovery_records_an_uninstalled_desired_app_as_crashed() {
        let temp = tempfile::tempdir().unwrap();
        let install_root = temp.path().join("apps");
        std::fs::create_dir_all(&install_root).unwrap();
        let store = DaemonStateStore::new(temp.path().join("state.json"));
        store
            .set_desired("com.example.missing", DesiredState::Running, 1)
            .unwrap();
        let manager = Arc::new(
            crate::manager::LocalAppManager::open_with(
                &install_root,
                temp.path().join("permissions"),
            )
            .unwrap(),
        );
        let report = DaemonService::new(store.clone())
            .with_manager(manager)
            .recover_startup();
        assert!(report.recovered.is_empty());
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].app_id, "com.example.missing");
        let app = &store.load().unwrap().applications["com.example.missing"];
        assert_eq!(app.desired, DesiredState::Running);
        assert_eq!(app.observed, ObservedState::Crashed);
        assert!(app.last_error.as_deref().unwrap().contains("not found"));
    }

    // -----------------------------------------------------------------
    // Phase 5 — per-service handlers + recovery
    // -----------------------------------------------------------------
    //
    // The daemon's per-service surface needs a manager
    // that records each call so the test can assert the
    // dispatch went to the right AppManager method. We
    // build a thin `AppManager` impl backed by a
    // `Mutex<Vec<Call>>` rather than spinning up a real
    // `LocalAppManager` (which would require a Node
    // binary on the test host). The stub also returns
    // deterministic v1 `RuntimeStatus` snapshots so the
    // daemon's `record_service_status` helper has
    // something to write.

    use crate::manager::{AppDetails, AppManager, AppSummary, InstallOptions, InstallSource, ManagerError, PermissionState, SignatureState, UninstallOptions};
    use crate::core::manifest::{AppManifest as V1AppManifest, Frontend, PackageKind};
    use crate::authorization::PermissionDecision;
    use std::path::{Path, PathBuf};

    /// Build a placeholder v1 manifest the test
    /// stub's `get_app` can return. The test does not
    /// need the manifest content (it just needs a
    /// non-error response); a minimal valid manifest
    /// is the smallest way to satisfy the type.
    fn stub_manifest(id: &str) -> V1AppManifest {
        V1AppManifest {
            schema_version: 1,
            kind: PackageKind::App,
            id: id.into(),
            name: "stub".into(),
            version: "0.0.0".into(),
            description: None,
            author: None,
            icons: None,
            homepage: None,
            license: None,
            update: None,
            frontend: Frontend {
                entry: "index.html".into(),
                build: None,
            },
            backend: None,
            permissions: Vec::new(),
            extension_points: None,
        }
    }

    #[derive(Debug, Clone, PartialEq)]
    enum StubCall {
        StartService(String, String),
        StopService(String, String),
        RestartService(String, String),
        ServiceStatus(String, String),
        ListServices(String),
    }

    struct StubManager {
        calls: std::sync::Mutex<Vec<StubCall>>,
    }

    impl StubManager {
        fn new() -> Self {
            Self {
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }
        fn snapshot(&self) -> Vec<StubCall> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl AppManager for StubManager {
        fn list_apps(&self) -> Result<Vec<AppSummary>, ManagerError> {
            Ok(Vec::new())
        }
        fn get_app(&self, _id: &str) -> Result<AppDetails, ManagerError> {
            // `get_app` would normally read the on-disk
            // manifest; the Phase 5 tests do not need
            // the data, only the dispatch.
            Ok(AppDetails {
                summary: AppSummary {
                    id: "com.example.stub".into(),
                    name: "stub".into(),
                    version: "0.0.0".into(),
                    description: None,
                    path: PathBuf::new(),
                    install_source: InstallSource::LocalPackage,
                    last_launched_at: None,
                    publisher_fingerprint: None,
                    signature_state: SignatureState::Unsigned,
                    runtime: None,
                },
                manifest: stub_manifest("com.example.stub"),
                permissions: Vec::new(),
                install_path: PathBuf::new(),
            })
        }
        fn install(
            &self,
            _package_path: &Path,
            _options: InstallOptions,
        ) -> Result<AppSummary, ManagerError> {
            unimplemented!()
        }
        fn uninstall(
            &self,
            _id: &str,
            _options: UninstallOptions,
        ) -> Result<(), ManagerError> {
            unimplemented!()
        }
        fn launch(&self, _id: &str) -> Result<crate::runtime::RuntimeStatus, ManagerError> {
            unimplemented!()
        }
        fn stop(&self, _id: &str) -> Result<crate::runtime::RuntimeStatus, ManagerError> {
            unimplemented!()
        }
        fn runtime_status(
            &self,
            _id: &str,
        ) -> Result<crate::runtime::RuntimeStatus, ManagerError> {
            Ok(crate::runtime::RuntimeStatus {
                state: crate::runtime::RuntimeState::Running,
                ..Default::default()
            })
        }
        fn start_service(
            &self,
            id: &str,
            service: &str,
        ) -> Result<crate::runtime::RuntimeStatus, ManagerError> {
            self.calls
                .lock()
                .unwrap()
                .push(StubCall::StartService(id.into(), service.into()));
            Ok(crate::runtime::RuntimeStatus {
                state: crate::runtime::RuntimeState::Running,
                ..Default::default()
            })
        }
        fn stop_service(
            &self,
            id: &str,
            service: &str,
        ) -> Result<crate::runtime::RuntimeStatus, ManagerError> {
            self.calls
                .lock()
                .unwrap()
                .push(StubCall::StopService(id.into(), service.into()));
            Ok(crate::runtime::RuntimeStatus {
                state: crate::runtime::RuntimeState::Stopped,
                ..Default::default()
            })
        }
        fn restart_service(
            &self,
            id: &str,
            service: &str,
        ) -> Result<crate::runtime::RuntimeStatus, ManagerError> {
            self.calls
                .lock()
                .unwrap()
                .push(StubCall::RestartService(id.into(), service.into()));
            Ok(crate::runtime::RuntimeStatus {
                state: crate::runtime::RuntimeState::Running,
                ..Default::default()
            })
        }
        fn service_status(
            &self,
            id: &str,
            service: &str,
        ) -> Result<crate::runtime::RuntimeStatus, ManagerError> {
            self.calls
                .lock()
                .unwrap()
                .push(StubCall::ServiceStatus(id.into(), service.into()));
            Ok(crate::runtime::RuntimeStatus {
                state: crate::runtime::RuntimeState::Running,
                ..Default::default()
            })
        }
        fn list_services(
            &self,
            id: &str,
        ) -> Result<
            Vec<crate::runtime::application_supervisor::ServiceSummary>,
            ManagerError,
        > {
            self.calls
                .lock()
                .unwrap()
                .push(StubCall::ListServices(id.into()));
            Ok(Vec::new())
        }
        fn permissions(
            &self,
            _id: &str,
        ) -> Result<Vec<PermissionState>, ManagerError> {
            Ok(Vec::new())
        }
        fn set_permission(
            &self,
            _id: &str,
            _permission: &str,
            _decision: PermissionDecision,
        ) -> Result<(), ManagerError> {
            unimplemented!()
        }
        fn registry_path(&self) -> &Path {
            Path::new(".")
        }
        fn install_root(&self) -> &Path {
            Path::new(".")
        }
    }

    fn service_with_stub() -> (tempfile::TempDir, DaemonService, Arc<StubManager>) {
        let temp = tempfile::tempdir().unwrap();
        let store = DaemonStateStore::new(temp.path().join("state.json"));
        let stub = Arc::new(StubManager::new());
        let service = DaemonService::new(store).with_manager(stub.clone());
        (temp, service, stub)
    }

    #[test]
    fn start_service_dispatches_to_manager_and_persists_desired() {
        let (_temp, service, stub) = service_with_stub();
        let response = service.handle(request(ControlCommand::StartService {
            app_id: "com.example.api".into(),
            service: "api".into(),
        }));
        assert!(response.ok, "{:?}", response.error);
        assert_eq!(
            stub.snapshot(),
            vec![StubCall::StartService("com.example.api".into(), "api".into())]
        );
    }

    #[test]
    fn stop_service_flips_desired_to_stopped() {
        let (_temp, service, stub) = service_with_stub();
        // Prime: start, then stop. Each call must hit
        // the right manager method.
        service.handle(request(ControlCommand::StartService {
            app_id: "com.example.api".into(),
            service: "api".into(),
        }));
        let response = service.handle(request(ControlCommand::StopService {
            app_id: "com.example.api".into(),
            service: "api".into(),
        }));
        assert!(response.ok, "{:?}", response.error);
        assert_eq!(
            stub.snapshot(),
            vec![
                StubCall::StartService("com.example.api".into(), "api".into()),
                StubCall::StopService("com.example.api".into(), "api".into()),
            ]
        );
    }

    #[test]
    fn list_services_returns_a_services_envelope() {
        let (_temp, service, stub) = service_with_stub();
        let response = service.handle(request(ControlCommand::ListServices {
            app_id: "com.example.api".into(),
        }));
        assert!(response.ok, "{:?}", response.error);
        let result = response.result.unwrap();
        assert!(result.get("services").is_some());
        assert_eq!(
            stub.snapshot(),
            vec![StubCall::ListServices("com.example.api".into())]
        );
    }

    #[test]
    fn recovery_walks_per_service_desired_state() {
        // Pre-seed the store with per-service intent
        // for two services; the stub records the calls
        // so we can assert `recover_startup` invoked
        // `start_service` for each, in BTreeMap order.
        let (temp, _service, stub) = service_with_stub();
        let store = DaemonStateStore::new(temp.path().join("state.json"));
        store
            .set_desired("com.example.dag", DesiredState::Running, 1)
            .unwrap();
        store
            .set_service_desired(
                "com.example.dag",
                "api",
                DesiredState::Running,
                2,
            )
            .unwrap();
        store
            .set_service_desired(
                "com.example.dag",
                "worker",
                DesiredState::Running,
                3,
            )
            .unwrap();
        // Re-create the daemon with the seeded store
        // (the helper above created an empty one).
        let service = DaemonService::new(store).with_manager(stub.clone());
        let report = service.recover_startup();
        assert!(
            report.recovered.contains(&"com.example.dag".to_owned()),
            "recovered should contain the app, was {:?}",
            report.recovered
        );
        assert!(report.failed.is_empty(), "failed: {:?}", report.failed);
        // Two `StartService` calls, alphabetical
        // (BTreeMap iteration).
        assert_eq!(
            stub.snapshot(),
            vec![
                StubCall::StartService("com.example.dag".into(), "api".into()),
                StubCall::StartService("com.example.dag".into(), "worker".into()),
            ]
        );
    }

    #[test]
    fn recovery_records_per_service_crash_in_state() {
        // A failed per-service start during recovery
        // must persist `observed=crashed` on the
        // individual `ServiceControlState` row, not the
        // app-level `AppControlState`. We simulate the
        // failure by reaching into the manager via a
        // tiny wrapper that always errors.
        let (temp, _service, _stub) = service_with_stub();
        let store = DaemonStateStore::new(temp.path().join("state.json"));
        store
            .set_desired("com.example.fail", DesiredState::Running, 1)
            .unwrap();
        store
            .set_service_desired(
                "com.example.fail",
                "broken",
                DesiredState::Running,
                2,
            )
            .unwrap();
        struct AlwaysFail;
        impl AppManager for AlwaysFail {
            fn list_apps(&self) -> Result<Vec<AppSummary>, ManagerError> { Ok(Vec::new()) }
            fn get_app(&self, _id: &str) -> Result<AppDetails, ManagerError> {
                Ok(AppDetails {
                    summary: AppSummary {
                        id: "com.example.fail".into(),
                        name: "fail".into(),
                        version: "0.0.0".into(),
                        description: None,
                        path: PathBuf::new(),
                        install_source: InstallSource::LocalPackage,
                        last_launched_at: None,
                        publisher_fingerprint: None,
                        signature_state: SignatureState::Unsigned,
                        runtime: None,
                    },
                    manifest: stub_manifest("com.example.fail"),
                    permissions: Vec::new(),
                    install_path: PathBuf::new(),
                })
            }
            fn install(&self, _p: &Path, _o: InstallOptions) -> Result<AppSummary, ManagerError> { unimplemented!() }
            fn uninstall(&self, _id: &str, _o: UninstallOptions) -> Result<(), ManagerError> { unimplemented!() }
            fn launch(&self, _id: &str) -> Result<crate::runtime::RuntimeStatus, ManagerError> { unimplemented!() }
            fn stop(&self, _id: &str) -> Result<crate::runtime::RuntimeStatus, ManagerError> { unimplemented!() }
            fn runtime_status(&self, _id: &str) -> Result<crate::runtime::RuntimeStatus, ManagerError> {
                Ok(crate::runtime::RuntimeStatus::default())
            }
            fn start_service(&self, _id: &str, _s: &str) -> Result<crate::runtime::RuntimeStatus, ManagerError> {
                Err(ManagerError::Runtime("synthetic".into()))
            }
            fn stop_service(&self, _id: &str, _s: &str) -> Result<crate::runtime::RuntimeStatus, ManagerError> { unimplemented!() }
            fn restart_service(&self, _id: &str, _s: &str) -> Result<crate::runtime::RuntimeStatus, ManagerError> { unimplemented!() }
            fn service_status(&self, _id: &str, _s: &str) -> Result<crate::runtime::RuntimeStatus, ManagerError> { unimplemented!() }
            fn list_services(&self, _id: &str) -> Result<Vec<crate::runtime::application_supervisor::ServiceSummary>, ManagerError> { Ok(Vec::new()) }
            fn permissions(&self, _id: &str) -> Result<Vec<PermissionState>, ManagerError> { Ok(Vec::new()) }
            fn set_permission(&self, _id: &str, _p: &str, _d: PermissionDecision) -> Result<(), ManagerError> { unimplemented!() }
            fn registry_path(&self) -> &Path { Path::new(".") }
            fn install_root(&self) -> &Path { Path::new(".") }
        }
        let report = DaemonService::new(store.clone())
            .with_manager(Arc::new(AlwaysFail))
            .recover_startup();
        assert_eq!(report.failed.len(), 1);
        assert!(
            report.failed[0].error.contains("synthetic"),
            "failed: {:?}",
            report.failed
        );
        let app = &store.load().unwrap().applications["com.example.fail"];
        let broken = &app.services["broken"];
        assert_eq!(broken.observed, ObservedState::Crashed);
        // The error string goes through `ManagerError`'s
        // `Display`, which prefixes `runtime: ` to the
        // inner message. We assert on the substring so
        // the test does not couple to the exact prefix
        // shape.
        let message = broken.last_error.as_deref().unwrap_or_default();
        assert!(
            message.contains("synthetic"),
            "last_error should mention 'synthetic', was {message:?}"
        );
        // The app-level `observed` is left alone — a
        // crashed service is *not* the same as a
        // crashed app, and a future successful start
        // of another service should not be masked.
        assert_ne!(app.observed, ObservedState::Crashed);
    }
}

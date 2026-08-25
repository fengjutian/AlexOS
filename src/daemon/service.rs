use std::{
    collections::BTreeMap,
    sync::Arc,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use base64::Engine as _;
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
    websocket_tunnels: Arc<Mutex<BTreeMap<String, crate::proxy::WebSocketTunnel>>>,
    streams: Arc<crate::runtime::stream::StreamManager>,
}

impl DaemonService {
    pub fn new(state: DaemonStateStore) -> Self {
        Self {
            state,
            manager: None,
            websocket_tunnels: Arc::new(Mutex::new(BTreeMap::new())),
            streams: Arc::new(crate::runtime::stream::StreamManager::new(
                crate::runtime::stream::StreamLimits::default(),
            )),
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
            ControlCommand::StopService { app_id, service } => self.stop_service(&app_id, &service),
            ControlCommand::RestartService { app_id, service } => {
                self.restart_service(&app_id, &service)
            }
            ControlCommand::ServiceStatus { app_id, service } => {
                self.service_status(&app_id, &service)
            }
            ControlCommand::ListServices { app_id } => self.list_services(&app_id),
            ControlCommand::InvokeService {
                app_id,
                service,
                method,
                arguments,
                timeout_ms,
            } => self.invoke_service(&id, &app_id, &service, &method, &arguments, timeout_ms),
            ControlCommand::OpenServiceWebSocket { app_id, service } => {
                self.open_service_websocket(&app_id, &service)
            }
            ControlCommand::ProxyServiceHttp {
                app_id,
                service,
                method,
                path,
                headers,
                body_base64,
            } => self.proxy_service_http(&app_id, &service, &method, &path, &headers, &body_base64),
            ControlCommand::StreamOpen {
                app_id,
                request_id,
                stream_id,
                metadata,
            } => self.stream_open(&app_id, &request_id, &stream_id, metadata),
            ControlCommand::StreamCredit { stream_id, bytes } => {
                self.stream_credit(&stream_id, bytes)
            }
            ControlCommand::StreamPush {
                stream_id,
                data_base64,
            } => self.stream_push(&stream_id, &data_base64),
            ControlCommand::StreamRead { stream_id, wait_ms } => {
                self.stream_read(&stream_id, wait_ms)
            }
            ControlCommand::StreamEnd { stream_id, error } => self.stream_end(&stream_id, error),
            ControlCommand::StreamCancel { stream_id, reason } => {
                self.stream_cancel(&stream_id, &reason)
            }
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
        services: &std::collections::BTreeMap<String, super::ServiceControlState>,
        report: &mut RecoveryReport,
    ) {
        let declared = match manager.list_services(app_id) {
            Ok(declared) => declared,
            Err(error) => {
                report.failed.push(RecoveryFailure {
                    app_id: app_id.to_owned(),
                    error: format!("cannot load service graph: {error}"),
                });
                return;
            }
        };
        let order = match recovery_service_order(&declared, services) {
            Ok(order) => order,
            Err(error) => {
                report.failed.push(RecoveryFailure {
                    app_id: app_id.to_owned(),
                    error,
                });
                return;
            }
        };
        for service_name in order {
            let result = manager.start_service(app_id, &service_name);
            match result {
                Ok(status) => {
                    if let Err(error) = self.record_service_status(app_id, &service_name, &status) {
                        report.failed.push(RecoveryFailure {
                            app_id: app_id.to_owned(),
                            error: format!("service {service_name}: {error}"),
                        });
                    }
                }
                Err(error) => {
                    let _ = self.state.set_service_observed(
                        app_id,
                        &service_name,
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
        if report.failed.iter().all(|f| f.app_id != app_id) {
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
    fn start_service(&self, app_id: &str, service: &str) -> Result<serde_json::Value, String> {
        let manager = self
            .manager
            .as_ref()
            .ok_or_else(|| "service manager is not connected yet".to_string())?;
        manager.get_app(app_id).map_err(|error| error.to_string())?;
        let status = manager
            .start_service(app_id, service)
            .map_err(|error| error.to_string())?;
        self.state
            .set_service_desired(
                app_id,
                service,
                DesiredState::Running,
                now_ms().unwrap_or_default(),
            )
            .map_err(|error| error.to_string())?;
        self.record_service_status(app_id, service, &status)?;
        Ok(json!(status))
    }

    fn stop_service(&self, app_id: &str, service: &str) -> Result<serde_json::Value, String> {
        let manager = self
            .manager
            .as_ref()
            .ok_or_else(|| "service manager is not connected yet".to_string())?;
        manager.get_app(app_id).map_err(|error| error.to_string())?;
        let status = manager
            .stop_service(app_id, service)
            .map_err(|error| error.to_string())?;
        self.state
            .set_service_desired(
                app_id,
                service,
                DesiredState::Stopped,
                now_ms().unwrap_or_default(),
            )
            .map_err(|error| error.to_string())?;
        self.record_service_status(app_id, service, &status)?;
        Ok(json!(status))
    }

    fn restart_service(&self, app_id: &str, service: &str) -> Result<serde_json::Value, String> {
        let manager = self
            .manager
            .as_ref()
            .ok_or_else(|| "service manager is not connected yet".to_string())?;
        manager.get_app(app_id).map_err(|error| error.to_string())?;
        let status = manager
            .restart_service(app_id, service)
            .map_err(|error| error.to_string())?;
        self.state
            .set_service_desired(
                app_id,
                service,
                DesiredState::Running,
                now_ms().unwrap_or_default(),
            )
            .map_err(|error| error.to_string())?;
        self.record_service_status(app_id, service, &status)?;
        Ok(json!(status))
    }

    fn service_status(&self, app_id: &str, service: &str) -> Result<serde_json::Value, String> {
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

    fn invoke_service(
        &self,
        request_id: &str,
        app_id: &str,
        service: &str,
        method: &str,
        arguments: &serde_json::Value,
        timeout_ms: u64,
    ) -> Result<serde_json::Value, String> {
        if app_id.trim().is_empty() {
            return Err("appId must not be empty".into());
        }
        if service.trim().is_empty() {
            return Err("service must not be empty".into());
        }
        if method.trim().is_empty() {
            return Err("method must not be empty".into());
        }
        if !(1..=30_000).contains(&timeout_ms) {
            return Err("timeoutMs must be between 1 and 30000".into());
        }
        let manager = self
            .manager
            .as_ref()
            .ok_or_else(|| "runtime manager is not configured".to_owned())?;
        manager
            .invoke_service(app_id, service, request_id, method, arguments, timeout_ms)
            .map_err(|error| error.to_string())
    }

    fn open_service_websocket(
        &self,
        app_id: &str,
        service: &str,
    ) -> Result<serde_json::Value, String> {
        let manager = self
            .manager
            .as_ref()
            .ok_or_else(|| "runtime manager is not configured".to_owned())?;
        let endpoint = manager
            .service_endpoint(app_id, service)
            .map_err(|error| error.to_string())?;
        let tunnel = crate::proxy::WebSocketTunnel::start(endpoint, app_id.to_owned())
            .map_err(|error| error.to_string())?;
        let base_url = tunnel.base_url.clone();
        let key = format!("{app_id}\0{service}");
        self.websocket_tunnels
            .lock()
            .map_err(|_| "websocket tunnel registry lock poisoned".to_owned())?
            .insert(key, tunnel);
        Ok(json!({ "baseUrl": base_url }))
    }

    fn proxy_service_http(
        &self,
        app_id: &str,
        service: &str,
        method: &str,
        path: &str,
        headers: &BTreeMap<String, String>,
        body_base64: &str,
    ) -> Result<serde_json::Value, String> {
        if !matches!(
            method,
            "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS"
        ) {
            return Err(format!(
                "HTTP method {method:?} is not allowed by the service proxy"
            ));
        }
        if !path.starts_with("/api/") || path.contains(['\r', '\n']) {
            return Err("proxy path must start with /api/ and contain no control lines".into());
        }
        let body = base64::engine::general_purpose::STANDARD
            .decode(body_base64)
            .map_err(|_| "bodyBase64 is invalid".to_owned())?;
        if body.len() > super::MAX_PROXY_BODY_BYTES {
            return Err(format!(
                "proxy request body exceeds {} byte control-plane cap",
                super::MAX_PROXY_BODY_BYTES
            ));
        }
        let manager = self
            .manager
            .as_ref()
            .ok_or_else(|| "runtime manager is not configured".to_owned())?;
        let endpoint = manager
            .service_endpoint(app_id, service)
            .map_err(|error| error.to_string())?;
        let mut builder = wry::http::Request::builder().method(method).uri(path);
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        let request = builder
            .body(body)
            .map_err(|error| format!("invalid proxy request: {error}"))?;
        let response = crate::proxy::proxy_to_service(&endpoint, app_id, path, &request);
        let status = response.status().as_u16();
        let response_headers: BTreeMap<String, String> = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_owned(), value.to_owned()))
            })
            .collect();
        if response.body().len() > super::MAX_PROXY_BODY_BYTES {
            return Err(format!(
                "proxy response body exceeds {} byte control-plane cap",
                super::MAX_PROXY_BODY_BYTES
            ));
        }
        let body_base64 =
            base64::engine::general_purpose::STANDARD.encode(response.body().as_ref());
        Ok(json!({
            "status": status,
            "headers": response_headers,
            "bodyBase64": body_base64,
        }))
    }

    fn stream_open(
        &self,
        app_id: &str,
        request_id: &str,
        stream_id: &str,
        metadata: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.streams
            .open(app_id, stream_id)
            .map_err(|error| error.to_string())?;
        Ok(json!({ "requestId": request_id, "streamId": stream_id, "metadata": metadata }))
    }

    fn stream_credit(&self, stream_id: &str, bytes: usize) -> Result<serde_json::Value, String> {
        self.streams
            .grant_credit(stream_id, bytes)
            .map(|available| json!({ "streamId": stream_id, "available": available }))
            .map_err(|error| error.to_string())
    }

    fn stream_push(&self, stream_id: &str, data_base64: &str) -> Result<serde_json::Value, String> {
        let data = base64::engine::general_purpose::STANDARD
            .decode(data_base64)
            .map_err(|_| "dataBase64 is invalid".to_owned())?;
        self.streams
            .push(stream_id, data)
            .map(|sequence| json!({ "streamId": stream_id, "sequence": sequence }))
            .map_err(|error| error.to_string())
    }

    fn stream_read(&self, stream_id: &str, wait_ms: u32) -> Result<serde_json::Value, String> {
        const MAX_STREAM_READ_WAIT_MS: u32 = 30_000;
        let chunk = self
            .streams
            .pop_wait(
                stream_id,
                std::time::Duration::from_millis(wait_ms.min(MAX_STREAM_READ_WAIT_MS).into()),
            )
            .map_err(|error| error.to_string())?;
        let terminal = self
            .streams
            .terminal(stream_id)
            .map_err(|error| error.to_string())?;
        Ok(match chunk {
            Some(chunk) => json!({
                "streamId": stream_id,
                "sequence": chunk.sequence,
                "dataBase64": base64::engine::general_purpose::STANDARD.encode(chunk.data),
            }),
            None => json!({
                "streamId": stream_id,
                "pending": terminal.is_none(),
                "terminal": terminal.map(stream_terminal_json),
            }),
        })
    }

    fn stream_end(
        &self,
        stream_id: &str,
        error: Option<super::StreamControlError>,
    ) -> Result<serde_json::Value, String> {
        let terminal = error.map_or(crate::runtime::stream::StreamTerminal::Completed, |error| {
            crate::runtime::stream::StreamTerminal::Failed {
                code: error.code,
                message: error.message,
            }
        });
        self.streams
            .finish(stream_id, terminal)
            .map(|_| json!({ "streamId": stream_id, "ended": true }))
            .map_err(|error| error.to_string())
    }

    fn stream_cancel(&self, stream_id: &str, reason: &str) -> Result<serde_json::Value, String> {
        self.streams
            .cancel(stream_id, reason)
            .map(|_| json!({ "streamId": stream_id, "cancelled": true }))
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

fn stream_terminal_json(terminal: crate::runtime::stream::StreamTerminal) -> serde_json::Value {
    match terminal {
        crate::runtime::stream::StreamTerminal::Completed => json!({ "kind": "completed" }),
        crate::runtime::stream::StreamTerminal::Failed { code, message } => {
            json!({ "kind": "failed", "error": { "code": code, "message": message } })
        }
        crate::runtime::stream::StreamTerminal::Cancelled { reason } => {
            json!({ "kind": "cancelled", "reason": reason })
        }
    }
}

fn recovery_service_order(
    declared: &[ServiceSummary],
    persisted: &std::collections::BTreeMap<String, super::ServiceControlState>,
) -> Result<Vec<String>, String> {
    use std::collections::{BTreeMap, BTreeSet};

    let specs: BTreeMap<&str, &ServiceSummary> = declared
        .iter()
        .map(|service| (service.name.as_str(), service))
        .collect();
    let desired: BTreeSet<String> = persisted
        .iter()
        .filter_map(|(name, state)| {
            (state.desired == DesiredState::Running).then_some(name.clone())
        })
        .collect();
    // Some alternate AppManager implementations (including remote/test
    // facades) cannot return descriptors. Preserve their legacy behaviour:
    // the manager remains responsible for rejecting unknown services.
    if declared.is_empty() {
        return Ok(desired.into_iter().collect());
    }
    for name in &desired {
        let spec = specs
            .get(name.as_str())
            .ok_or_else(|| format!("persisted service {name:?} is no longer declared"))?;
        for dependency in &spec.depends_on {
            if !desired.contains(dependency.as_str()) {
                return Err(format!(
                    "service {name:?} cannot recover because dependency {dependency:?} is not desired=running"
                ));
            }
        }
    }

    fn visit<'a>(
        name: &'a str,
        specs: &BTreeMap<&'a str, &'a ServiceSummary>,
        desired: &BTreeSet<String>,
        visiting: &mut BTreeSet<&'a str>,
        visited: &mut BTreeSet<&'a str>,
        output: &mut Vec<&'a str>,
    ) -> Result<(), String> {
        if visited.contains(name) {
            return Ok(());
        }
        if !visiting.insert(name) {
            return Err(format!("service dependency cycle includes {name:?}"));
        }
        let spec = specs
            .get(name)
            .ok_or_else(|| format!("service {name:?} is not declared"))?;
        for dependency in &spec.depends_on {
            if desired.contains(dependency.as_str()) {
                visit(dependency, specs, desired, visiting, visited, output)?;
            }
        }
        visiting.remove(name);
        visited.insert(name);
        output.push(name);
        Ok(())
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut output = Vec::with_capacity(desired.len());
    for name in &desired {
        visit(
            specs
                .get_key_value(name.as_str())
                .map(|(declared_name, _)| *declared_name)
                .ok_or_else(|| format!("service {name:?} is not declared"))?,
            &specs,
            &desired,
            &mut visiting,
            &mut visited,
            &mut output,
        )?;
    }
    Ok(output.into_iter().map(str::to_owned).collect())
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

    fn persisted_service(name: &str, desired: DesiredState) -> crate::daemon::ServiceControlState {
        crate::daemon::ServiceControlState {
            service: name.into(),
            desired,
            updated_at_ms: 0,
            observed: ObservedState::Stopped,
            last_error: None,
        }
    }

    fn declared_service(name: &str, depends_on: &[&str]) -> ServiceSummary {
        ServiceSummary {
            name: name.into(),
            depends_on: depends_on.iter().map(|value| (*value).into()).collect(),
            status: crate::runtime::service_supervisor::ServiceStatus::Pending,
            restart_count: 0,
            last_error: None,
        }
    }

    fn request(command: ControlCommand) -> ControlRequest {
        ControlRequest {
            protocol: PROTOCOL_VERSION,
            id: "test-1".into(),
            command,
        }
    }

    #[test]
    fn daemon_stream_flow_enforces_credit_and_reports_terminal_once() {
        let temp = tempfile::tempdir().unwrap();
        let service = DaemonService::new(DaemonStateStore::new(temp.path().join("state.json")));
        assert!(
            service
                .handle(request(ControlCommand::StreamOpen {
                    app_id: "com.example.stream".into(),
                    request_id: "model-1".into(),
                    stream_id: "stream-1".into(),
                    metadata: json!({ "contentType": "text/plain" }),
                }))
                .ok
        );
        let blocked = service.handle(request(ControlCommand::StreamPush {
            stream_id: "stream-1".into(),
            data_base64: "aGVsbG8=".into(),
        }));
        assert!(!blocked.ok);
        assert!(blocked.error.unwrap().contains("insufficient credit"));
        assert!(
            service
                .handle(request(ControlCommand::StreamCredit {
                    stream_id: "stream-1".into(),
                    bytes: 5,
                }))
                .ok
        );
        assert!(
            service
                .handle(request(ControlCommand::StreamPush {
                    stream_id: "stream-1".into(),
                    data_base64: "aGVsbG8=".into(),
                }))
                .ok
        );
        let chunk = service.handle(request(ControlCommand::StreamRead {
            stream_id: "stream-1".into(),
        }));
        assert_eq!(chunk.result.unwrap()["dataBase64"], "aGVsbG8=");
        assert!(
            service
                .handle(request(ControlCommand::StreamEnd {
                    stream_id: "stream-1".into(),
                    error: None,
                }))
                .ok
        );
        let ended = service.handle(request(ControlCommand::StreamRead {
            stream_id: "stream-1".into(),
        }));
        assert_eq!(ended.result.unwrap()["terminal"]["kind"], "completed");
        assert!(
            !service
                .handle(request(ControlCommand::StreamEnd {
                    stream_id: "stream-1".into(),
                    error: None,
                }))
                .ok
        );
    }

    #[test]
    fn service_recovery_order_respects_dependencies() {
        let declared = vec![
            declared_service("api", &["worker"]),
            declared_service("worker", &["database"]),
            declared_service("database", &[]),
        ];
        let persisted = std::collections::BTreeMap::from([
            (
                "api".into(),
                persisted_service("api", DesiredState::Running),
            ),
            (
                "worker".into(),
                persisted_service("worker", DesiredState::Running),
            ),
            (
                "database".into(),
                persisted_service("database", DesiredState::Running),
            ),
        ]);
        assert_eq!(
            recovery_service_order(&declared, &persisted).unwrap(),
            ["database", "worker", "api"]
        );
    }

    #[test]
    fn service_recovery_rejects_a_stopped_dependency() {
        let declared = vec![
            declared_service("api", &["database"]),
            declared_service("database", &[]),
        ];
        let persisted = std::collections::BTreeMap::from([
            (
                "api".into(),
                persisted_service("api", DesiredState::Running),
            ),
            (
                "database".into(),
                persisted_service("database", DesiredState::Stopped),
            ),
        ]);
        let error = recovery_service_order(&declared, &persisted).unwrap_err();
        assert!(error.contains("dependency \"database\" is not desired=running"));
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

    use crate::authorization::{AuditEntry, PermissionDecision};
    use crate::core::manifest::{AppManifest as V1AppManifest, Frontend, PackageKind};
    use crate::manager::{
        AppDetails, AppManager, AppSummary, InstallOptions, InstallSource, ManagerError,
        PermissionState, SignatureState, UninstallOptions,
    };
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
        InvokeService(String, String, String, String, serde_json::Value, u64),
        ServiceEndpoint(String, String),
    }

    struct StubManager {
        calls: std::sync::Mutex<Vec<StubCall>>,
        endpoint: crate::runtime::ServiceEndpoint,
    }

    impl StubManager {
        fn new() -> Self {
            Self {
                calls: std::sync::Mutex::new(Vec::new()),
                endpoint: crate::runtime::ServiceEndpoint {
                    port: 1,
                    token: "private-runtime-token".into(),
                },
            }
        }
        fn with_endpoint(endpoint: crate::runtime::ServiceEndpoint) -> Self {
            Self {
                calls: std::sync::Mutex::new(Vec::new()),
                endpoint,
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
        fn uninstall(&self, _id: &str, _options: UninstallOptions) -> Result<(), ManagerError> {
            unimplemented!()
        }
        fn launch(&self, _id: &str) -> Result<crate::runtime::RuntimeStatus, ManagerError> {
            unimplemented!()
        }
        fn stop(&self, _id: &str) -> Result<crate::runtime::RuntimeStatus, ManagerError> {
            unimplemented!()
        }
        fn restart(&self, _id: &str) -> Result<crate::runtime::RuntimeStatus, ManagerError> {
            unimplemented!()
        }
        fn runtime_status(&self, _id: &str) -> Result<crate::runtime::RuntimeStatus, ManagerError> {
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
        ) -> Result<Vec<crate::runtime::application_supervisor::ServiceSummary>, ManagerError>
        {
            self.calls
                .lock()
                .unwrap()
                .push(StubCall::ListServices(id.into()));
            Ok(Vec::new())
        }
        fn invoke_service(
            &self,
            id: &str,
            service: &str,
            request_id: &str,
            method: &str,
            params: &serde_json::Value,
            timeout_ms: u64,
        ) -> Result<serde_json::Value, ManagerError> {
            self.calls.lock().unwrap().push(StubCall::InvokeService(
                id.into(),
                service.into(),
                request_id.into(),
                method.into(),
                params.clone(),
                timeout_ms,
            ));
            Ok(json!({ "echo": params }))
        }
        fn service_endpoint(
            &self,
            id: &str,
            service: &str,
        ) -> Result<crate::runtime::ServiceEndpoint, ManagerError> {
            self.calls
                .lock()
                .unwrap()
                .push(StubCall::ServiceEndpoint(id.into(), service.into()));
            Ok(self.endpoint.clone())
        }
        fn permissions(&self, _id: &str) -> Result<Vec<PermissionState>, ManagerError> {
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
        fn recent_audit_log(
            &self,
            _id: &str,
            _limit: usize,
        ) -> Result<Vec<AuditEntry>, ManagerError> {
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
            vec![StubCall::StartService(
                "com.example.api".into(),
                "api".into()
            )]
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
    fn invoke_service_dispatches_to_manager_with_request_identity() {
        let (_temp, service, stub) = service_with_stub();
        let response = service.handle(request(ControlCommand::InvokeService {
            app_id: "com.example.api".into(),
            service: "worker".into(),
            method: "generate".into(),
            arguments: json!({ "prompt": "hello" }),
            timeout_ms: 5_000,
        }));
        assert!(response.ok, "{:?}", response.error);
        assert_eq!(
            response.result,
            Some(json!({ "echo": { "prompt": "hello" } }))
        );
        assert_eq!(
            stub.snapshot(),
            vec![StubCall::InvokeService(
                "com.example.api".into(),
                "worker".into(),
                "test-1".into(),
                "generate".into(),
                json!({ "prompt": "hello" }),
                5_000,
            )]
        );
    }

    #[test]
    fn invoke_service_rejects_invalid_timeout_before_dispatch() {
        let (_temp, service, stub) = service_with_stub();
        let response = service.handle(request(ControlCommand::InvokeService {
            app_id: "com.example.api".into(),
            service: "main".into(),
            method: "ping".into(),
            arguments: serde_json::Value::Null,
            timeout_ms: 0,
        }));
        assert!(!response.ok);
        assert!(response.error.unwrap().contains("timeoutMs"));
        assert!(stub.snapshot().is_empty());
    }

    #[test]
    fn websocket_tunnel_returns_capability_url_without_runtime_token() {
        let (_temp, service, stub) = service_with_stub();
        let response = service.handle(request(ControlCommand::OpenServiceWebSocket {
            app_id: "com.example.api".into(),
            service: "events".into(),
        }));
        assert!(response.ok, "{:?}", response.error);
        let result = response.result.unwrap();
        let base_url = result["baseUrl"].as_str().unwrap();
        assert!(base_url.starts_with("ws://127.0.0.1:"));
        assert!(!result.to_string().contains("private-runtime-token"));
        assert_eq!(
            stub.snapshot(),
            vec![StubCall::ServiceEndpoint(
                "com.example.api".into(),
                "events".into()
            )]
        );
    }

    #[test]
    fn http_proxy_rejects_non_api_paths_before_endpoint_lookup() {
        let (_temp, service, stub) = service_with_stub();
        let response = service.handle(request(ControlCommand::ProxyServiceHttp {
            app_id: "com.example.api".into(),
            service: "main".into(),
            method: "GET".into(),
            path: "/admin/secrets".into(),
            headers: BTreeMap::new(),
            body_base64: String::new(),
        }));
        assert!(!response.ok);
        assert!(response.error.unwrap().contains("/api/"));
        assert!(stub.snapshot().is_empty());
    }

    #[test]
    fn http_proxy_rejects_connect_before_endpoint_lookup() {
        let (_temp, service, stub) = service_with_stub();
        let response = service.handle(request(ControlCommand::ProxyServiceHttp {
            app_id: "com.example.api".into(),
            service: "main".into(),
            method: "CONNECT".into(),
            path: "/api/tunnel".into(),
            headers: BTreeMap::new(),
            body_base64: String::new(),
        }));
        assert!(!response.ok);
        assert!(response.error.unwrap().contains("not allowed"));
        assert!(stub.snapshot().is_empty());
    }

    #[test]
    fn http_proxy_returns_a_transport_safe_response_envelope() {
        let (_temp, service, stub) = service_with_stub();
        let response = service.handle(request(ControlCommand::ProxyServiceHttp {
            app_id: "com.example.api".into(),
            service: "main".into(),
            method: "GET".into(),
            path: "/api/health".into(),
            headers: BTreeMap::new(),
            body_base64: String::new(),
        }));
        assert!(response.ok, "{:?}", response.error);
        let result = response.result.unwrap();
        assert_eq!(result["status"], 502);
        assert!(result["bodyBase64"].as_str().is_some());
        assert!(!result.to_string().contains("private-runtime-token"));
        assert_eq!(
            stub.snapshot(),
            vec![StubCall::ServiceEndpoint(
                "com.example.api".into(),
                "main".into()
            )]
        );
    }

    #[test]
    fn http_proxy_injects_app_identity_and_private_token_into_real_backend() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let backend = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut chunk).unwrap();
                assert!(read > 0);
                request.extend_from_slice(&chunk[..read]);
            }
            stream
                .write_all(
                    b"HTTP/1.0 201 Created\r\nContent-Type: application/octet-stream\r\nContent-Length: 3\r\n\r\n\x00\x01\xff",
                )
                .unwrap();
            String::from_utf8(request).unwrap()
        });

        let temp = tempfile::tempdir().unwrap();
        let store = DaemonStateStore::new(temp.path().join("state.json"));
        let stub = Arc::new(StubManager::with_endpoint(
            crate::runtime::ServiceEndpoint {
                port,
                token: "integration-private-token".into(),
            },
        ));
        let service = DaemonService::new(store).with_manager(stub);
        let response = service.handle(request(ControlCommand::ProxyServiceHttp {
            app_id: "com.example.integration".into(),
            service: "main".into(),
            method: "GET".into(),
            path: "/api/binary?version=1".into(),
            headers: BTreeMap::from([("accept".into(), "application/octet-stream".into())]),
            body_base64: String::new(),
        }));
        assert!(response.ok, "{:?}", response.error);
        let result = response.result.unwrap();
        assert_eq!(result["status"], 201);
        assert_eq!(result["bodyBase64"], "AAH/");

        let upstream_request = backend.join().unwrap();
        assert!(upstream_request.starts_with("GET /api/binary?version=1 HTTP/1.0"));
        assert!(upstream_request.contains("X-Alx-App-Id: com.example.integration"));
        assert!(upstream_request.contains("X-Alx-Token: integration-private-token"));
        assert!(upstream_request.contains("accept: application/octet-stream"));
        assert!(!result.to_string().contains("integration-private-token"));
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
            .set_service_desired("com.example.dag", "api", DesiredState::Running, 2)
            .unwrap();
        store
            .set_service_desired("com.example.dag", "worker", DesiredState::Running, 3)
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
        // Recovery first asks the manager for the dependency graph, then
        // starts the desired services. This stub returns no descriptors, so
        // the compatibility fallback remains alphabetical.
        assert_eq!(
            stub.snapshot(),
            vec![
                StubCall::ListServices("com.example.dag".into()),
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
            .set_service_desired("com.example.fail", "broken", DesiredState::Running, 2)
            .unwrap();
        struct AlwaysFail;
        impl AppManager for AlwaysFail {
            fn list_apps(&self) -> Result<Vec<AppSummary>, ManagerError> {
                Ok(Vec::new())
            }
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
            fn install(&self, _p: &Path, _o: InstallOptions) -> Result<AppSummary, ManagerError> {
                unimplemented!()
            }
            fn uninstall(&self, _id: &str, _o: UninstallOptions) -> Result<(), ManagerError> {
                unimplemented!()
            }
            fn launch(&self, _id: &str) -> Result<crate::runtime::RuntimeStatus, ManagerError> {
                unimplemented!()
            }
            fn stop(&self, _id: &str) -> Result<crate::runtime::RuntimeStatus, ManagerError> {
                unimplemented!()
            }
            fn restart(&self, _id: &str) -> Result<crate::runtime::RuntimeStatus, ManagerError> {
                unimplemented!()
            }
            fn runtime_status(
                &self,
                _id: &str,
            ) -> Result<crate::runtime::RuntimeStatus, ManagerError> {
                Ok(crate::runtime::RuntimeStatus::default())
            }
            fn start_service(
                &self,
                _id: &str,
                _s: &str,
            ) -> Result<crate::runtime::RuntimeStatus, ManagerError> {
                Err(ManagerError::Runtime("synthetic".into()))
            }
            fn stop_service(
                &self,
                _id: &str,
                _s: &str,
            ) -> Result<crate::runtime::RuntimeStatus, ManagerError> {
                unimplemented!()
            }
            fn restart_service(
                &self,
                _id: &str,
                _s: &str,
            ) -> Result<crate::runtime::RuntimeStatus, ManagerError> {
                unimplemented!()
            }
            fn service_status(
                &self,
                _id: &str,
                _s: &str,
            ) -> Result<crate::runtime::RuntimeStatus, ManagerError> {
                unimplemented!()
            }
            fn list_services(
                &self,
                _id: &str,
            ) -> Result<Vec<crate::runtime::application_supervisor::ServiceSummary>, ManagerError>
            {
                Ok(Vec::new())
            }
            fn permissions(&self, _id: &str) -> Result<Vec<PermissionState>, ManagerError> {
                Ok(Vec::new())
            }
            fn set_permission(
                &self,
                _id: &str,
                _p: &str,
                _d: PermissionDecision,
            ) -> Result<(), ManagerError> {
                unimplemented!()
            }
            fn recent_audit_log(
                &self,
                _id: &str,
                _limit: usize,
            ) -> Result<Vec<AuditEntry>, ManagerError> {
                Ok(Vec::new())
            }
            fn registry_path(&self) -> &Path {
                Path::new(".")
            }
            fn install_root(&self) -> &Path {
                Path::new(".")
            }
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

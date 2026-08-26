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
struct PendingMcpOAuth {
    application: String,
    binding: String,
    token_account: String,
    request: crate::mcp::oauth::AuthorizationRequest,
    created_at: std::time::Instant,
}

struct PendingMcpInput {
    application: String,
    response: Arc<(Mutex<Option<serde_json::Value>>, std::sync::Condvar)>,
}

struct StreamingMcpInputHandler {
    application: String,
    stream_id: String,
    streams: Arc<crate::runtime::stream::StreamManager>,
    cancellation: crate::runtime::stream::CancellationToken,
    pending: Arc<Mutex<BTreeMap<String, PendingMcpInput>>>,
    allowed: std::collections::BTreeSet<String>,
    next_id: std::sync::atomic::AtomicU64,
}

struct DaemonAgentNativeTools {
    state: DaemonStateStore,
}

impl crate::agent::AgentNativeTools for DaemonAgentNativeTools {
    fn call(
        &self,
        application: &str,
        name: &str,
        arguments: &serde_json::Value,
        _idempotency_key: &str,
    ) -> Result<serde_json::Value, crate::agent::AgentError> {
        if !arguments.is_null()
            && arguments
                .as_object()
                .is_none_or(|arguments| !arguments.is_empty())
        {
            return Err(crate::agent::AgentError::Tool(format!(
                "Alex native tool {name} does not accept arguments"
            )));
        }
        match name {
            "system.info" => Ok(json!({
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "family": std::env::consts::FAMILY,
                "timestampMs": now_ms().unwrap_or_default()
            })),
            "runtime.status" => {
                let state = self
                    .state
                    .load()
                    .map_err(|error| crate::agent::AgentError::Tool(error.to_string()))?;
                Ok(serde_json::to_value(state.applications.get(application))
                    .map_err(|error| crate::agent::AgentError::Tool(error.to_string()))?)
            }
            _ => Err(crate::agent::AgentError::Tool(format!(
                "unknown Alex native tool {name:?}"
            ))),
        }
    }
}

impl crate::mcp::InputRequiredHandler for StreamingMcpInputHandler {
    fn handle(
        &self,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, crate::mcp::McpError> {
        use std::sync::atomic::Ordering;
        if !self.allowed.contains(method) {
            return Err(crate::mcp::McpError::Authorization(format!(
                "MRTR method {method:?} is not permitted"
            )));
        }
        let input_id = format!(
            "{}:{}",
            self.stream_id,
            self.next_id.fetch_add(1, Ordering::Relaxed)
        );
        let response = Arc::new((Mutex::new(None), std::sync::Condvar::new()));
        self.pending
            .lock()
            .map_err(|_| crate::mcp::McpError::Transport("MCP input broker lock poisoned".into()))?
            .insert(
                input_id.clone(),
                PendingMcpInput {
                    application: self.application.clone(),
                    response: Arc::clone(&response),
                },
            );
        let payload = serde_json::to_vec(
            &json!({"type":"inputRequired","inputId":input_id,"method":method,"params":params}),
        )
        .map_err(|error| crate::mcp::McpError::Protocol(error.to_string()))?;
        push_stream_with_cancel(&self.streams, &self.stream_id, &self.cancellation, payload)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        let (gate, changed) = &*response;
        let mut value = gate.lock().map_err(|_| {
            crate::mcp::McpError::Transport("MCP input response lock poisoned".into())
        })?;
        loop {
            if let Some(value) = value.take() {
                self.pending
                    .lock()
                    .ok()
                    .and_then(|mut pending| pending.remove(&input_id));
                validate_mrtr_response(method, &value)?;
                return Ok(value);
            }
            if self.cancellation.is_cancelled() {
                self.pending
                    .lock()
                    .ok()
                    .and_then(|mut pending| pending.remove(&input_id));
                return Err(crate::mcp::McpError::InputRequired(
                    "MRTR interaction cancelled".into(),
                ));
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                self.pending
                    .lock()
                    .ok()
                    .and_then(|mut pending| pending.remove(&input_id));
                return Err(crate::mcp::McpError::InputRequired(
                    "MRTR interaction timed out".into(),
                ));
            }
            let waited = changed
                .wait_timeout(value, std::time::Duration::from_millis(250))
                .map_err(|_| {
                    crate::mcp::McpError::Transport("MCP input response lock poisoned".into())
                })?;
            value = waited.0;
        }
    }
}

#[derive(Clone)]
pub struct DaemonService {
    state: DaemonStateStore,
    manager: Option<Arc<dyn crate::manager::AppManager>>,
    websocket_tunnels: Arc<Mutex<BTreeMap<String, crate::proxy::WebSocketTunnel>>>,
    streams: Arc<crate::runtime::stream::StreamManager>,
    mcp: crate::mcp::ConnectionManager,
    mcp_health: Option<crate::mcp::ConnectionHealthMonitor>,
    mcp_audit: Option<crate::mcp::AuditLog>,
    mcp_configs: Option<crate::mcp::ConnectionConfigStore>,
    mcp_tokens: Option<crate::mcp::oauth::TokenVault>,
    mcp_oauth_pending: Arc<Mutex<BTreeMap<String, PendingMcpOAuth>>>,
    mcp_input_pending: Arc<Mutex<BTreeMap<String, PendingMcpInput>>>,
    mcp_approvals: crate::mcp::ApprovalStore,
    models: Option<crate::model::ModelManager>,
    model_downloads: Option<crate::model::download_tasks::ModelDownloadManager>,
    worker_packages: Option<crate::model::worker_packages::WorkerPackageStore>,
    native_workers: crate::native_worker::NativeWorkerManager,
    agents: Option<crate::agent::AgentManager>,
    agent_scheduler: Option<Arc<crate::agent::AgentScheduler>>,
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
            mcp: crate::mcp::ConnectionManager::default(),
            mcp_health: None,
            mcp_audit: None,
            mcp_configs: None,
            mcp_tokens: None,
            mcp_oauth_pending: Arc::new(Mutex::new(BTreeMap::new())),
            mcp_input_pending: Arc::new(Mutex::new(BTreeMap::new())),
            mcp_approvals: crate::mcp::ApprovalStore::default(),
            models: None,
            model_downloads: None,
            worker_packages: None,
            native_workers: crate::native_worker::NativeWorkerManager::default(),
            agents: None,
            agent_scheduler: None,
        }
    }

    pub fn native_worker_start(
        &self,
        application: &crate::core::application_manifest::ResolvedApplication,
        package_root: &std::path::Path,
        binding: &str,
    ) -> Result<crate::native_worker::NativeWorkerStatus, String> {
        let spec = application
            .native_workers
            .get(binding)
            .ok_or_else(|| format!("unknown native worker binding {binding:?}"))?;
        self.native_workers
            .start(&application.id, binding, package_root, spec)
            .map_err(|error| error.to_string())
    }

    pub fn native_worker_invoke(
        &self,
        application: &str,
        binding: &str,
        method: &str,
        params: serde_json::Value,
        timeout: std::time::Duration,
    ) -> Result<serde_json::Value, String> {
        self.native_workers
            .invoke(application, binding, method, params, timeout)
            .map_err(|error| error.to_string())
    }

    pub fn native_worker_status(
        &self,
        application: &str,
    ) -> Result<Vec<crate::native_worker::NativeWorkerStatus>, String> {
        self.native_workers
            .list(application)
            .map_err(|error| error.to_string())
    }

    pub fn native_worker_stop(&self, application: &str, binding: &str) -> Result<(), String> {
        self.native_workers
            .stop(application, binding)
            .map_err(|error| error.to_string())
    }

    fn start_installed_native_worker(
        &self,
        application: &str,
        binding: &str,
    ) -> Result<serde_json::Value, String> {
        let manager = self.manager.as_ref().ok_or("app manager is unavailable")?;
        let details = manager
            .get_app(application)
            .map_err(|error| error.to_string())?;
        let manifest = crate::core::application_manifest::load_application(&details.install_path)
            .map_err(|error| error.to_string())?;
        let resolved = manifest.resolve().map_err(|error| error.to_string())?;
        if resolved.id != application {
            return Err("installed manifest identity does not match the requested app".into());
        }
        let status = self.native_worker_start(&resolved, &details.install_path, binding)?;
        serde_json::to_value(status).map_err(|error| error.to_string())
    }

    fn restart_installed_native_worker(
        &self,
        application: &str,
        binding: &str,
    ) -> Result<serde_json::Value, String> {
        match self.native_workers.stop(application, binding) {
            Ok(()) | Err(crate::native_worker::NativeWorkerError::NotRunning { .. }) => {}
            Err(error) => return Err(error.to_string()),
        }
        self.start_installed_native_worker(application, binding)
    }

    fn invoke_native_worker_command(
        &self,
        application: &str,
        binding: &str,
        method: &str,
        arguments: serde_json::Value,
        timeout_ms: u64,
    ) -> Result<serde_json::Value, String> {
        if !(1..=120_000).contains(&timeout_ms) {
            return Err("native worker timeoutMs must be in 1..=120000".into());
        }
        self.native_worker_invoke(
            application,
            binding,
            method,
            arguments,
            std::time::Duration::from_millis(timeout_ms),
        )
    }

    fn invoke_native_worker_stream(
        &self,
        application: &str,
        binding: &str,
        method: &str,
        stream_id: &str,
        arguments: serde_json::Value,
        timeout_ms: u64,
    ) -> Result<serde_json::Value, String> {
        if !(1..=120_000).contains(&timeout_ms) {
            return Err("native worker timeoutMs must be in 1..=120000".into());
        }
        let cancellation = self
            .streams
            .open(application, stream_id)
            .map_err(|error| error.to_string())?;
        let manager = self.native_workers.clone();
        let streams = Arc::clone(&self.streams);
        let application = application.to_owned();
        let binding = binding.to_owned();
        let method = method.to_owned();
        let stream_id = stream_id.to_owned();
        let response_stream_id = stream_id.clone();
        let response_binding = binding.clone();
        let response_method = method.clone();
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let monitor_done = Arc::clone(&done);
        let monitor_cancellation = cancellation.clone();
        let monitor_manager = manager.clone();
        let monitor_application = application.clone();
        let monitor_binding = binding.clone();
        let worker_done = Arc::clone(&done);
        std::thread::Builder::new()
            .name("alex-native-worker-stream-cancel".into())
            .spawn(move || {
                use std::sync::atomic::Ordering;
                while !monitor_done.load(Ordering::Acquire) {
                    if monitor_cancellation.is_cancelled() {
                        let _ = monitor_manager.cancel(&monitor_application, &monitor_binding);
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            })
            .map_err(|error| error.to_string())?;
        let worker = std::thread::Builder::new()
            .name("alex-native-worker-stream".into())
            .spawn(move || {
                let stream_for_event = stream_id.clone();
                let mut emit = |event: serde_json::Value| {
                    let payload = serde_json::to_vec(&event).map_err(|error| {
                        crate::native_worker::NativeWorkerError::Protocol(error.to_string())
                    })?;
                    loop {
                        if cancellation.is_cancelled() {
                            let _ = manager.cancel(&application, &binding);
                            return Err(crate::native_worker::NativeWorkerError::Cancelled);
                        }
                        match streams.push(&stream_for_event, payload.clone()) {
                            Ok(_) => return Ok(()),
                            Err(crate::runtime::stream::StreamError::Backpressured { .. })
                            | Err(crate::runtime::stream::StreamError::BufferFull { .. }) => {
                                std::thread::sleep(std::time::Duration::from_millis(5));
                            }
                            Err(error) => {
                                return Err(crate::native_worker::NativeWorkerError::Protocol(
                                    error.to_string(),
                                ));
                            }
                        }
                    }
                };
                let result = manager.invoke_streaming(
                    &application,
                    &binding,
                    &method,
                    arguments,
                    std::time::Duration::from_millis(timeout_ms),
                    &mut emit,
                );
                if cancellation.is_cancelled() {
                    worker_done.store(true, std::sync::atomic::Ordering::Release);
                    return;
                }
                let terminal = match result {
                    Ok(result) => match emit(json!({"type":"result","result":result})) {
                        Ok(()) => crate::runtime::stream::StreamTerminal::Completed,
                        Err(error) => crate::runtime::stream::StreamTerminal::Failed {
                            code: "NATIVE_WORKER_STREAM_RESULT_FAILED".into(),
                            message: error.to_string(),
                        },
                    },
                    Err(error) => crate::runtime::stream::StreamTerminal::Failed {
                        code: "NATIVE_WORKER_STREAM_FAILED".into(),
                        message: error.to_string(),
                    },
                };
                let _ = streams.finish(&stream_id, terminal);
                worker_done.store(true, std::sync::atomic::Ordering::Release);
            });
        if let Err(error) = worker {
            done.store(true, std::sync::atomic::Ordering::Release);
            return Err(error.to_string());
        }
        Ok(
            json!({"streamId": response_stream_id, "binding": response_binding, "method": response_method}),
        )
    }

    pub fn with_ai_root(mut self, root: &std::path::Path) -> Result<Self, String> {
        let store = crate::model::ModelStore::open(root.join("models"))
            .map_err(|error| error.to_string())?;
        let model_downloads = crate::model::download_tasks::ModelDownloadManager::open(
            store.clone(),
            root.join("models").join("download-tasks.json"),
        )?;
        let models = crate::model::ModelManager::new(store);
        let worker_packages =
            crate::model::worker_packages::WorkerPackageStore::open(&root.join("runtimes"))
                .map_err(|error| error.to_string())?;
        models
            .register_process_workers(&root.join("runtimes"))
            .map_err(|error| error.to_string())?;
        let secret_resolver =
            crate::model::remote::SecretResolver::new(Arc::new(crate::platform::secret::native()));
        let remote_router = crate::model::remote::RemoteProviderRouter::open(root, secret_resolver)
            .map_err(|error| error.to_string())?;
        models.set_remote(remote_router);
        let agents =
            crate::agent::AgentManager::open(root.join("agents"), models.clone(), self.mcp.clone())
                .map_err(|error| error.to_string())?
                .with_native_tools(Arc::new(DaemonAgentNativeTools {
                    state: self.state.clone(),
                }));
        self.agent_scheduler = Some(Arc::new(
            crate::agent::AgentScheduler::start(agents.clone())
                .map_err(|error| error.to_string())?,
        ));
        self.agents = Some(agents);
        self.models = Some(models);
        self.model_downloads = Some(model_downloads);
        self.worker_packages = Some(worker_packages);
        self.mcp_audit = Some(
            crate::mcp::AuditLog::open(root.join("audit").join("mcp.jsonl"))
                .map_err(|error| error.to_string())?,
        );
        self.mcp_configs = Some(
            crate::mcp::ConnectionConfigStore::open(root.join("mcp").join("connections.json"))
                .map_err(|error| error.to_string())?,
        );
        self.mcp_tokens = Some(crate::mcp::oauth::TokenVault::new(Arc::new(
            crate::platform::secret::native(),
        )));
        let recovery_service = self.clone();
        self.mcp_health = Some(
            crate::mcp::ConnectionHealthMonitor::start_with_recovery(
                self.mcp.clone(),
                std::time::Duration::from_secs(15),
                Arc::new(move |connection| {
                    let service = recovery_service.clone();
                    let identity = format!("{}/{}", connection.application, connection.binding);
                    if let Err(error) =
                        crate::runtime::task_executor::mcp_executor().submit(move || {
                            if let Err(error) = service.reconnect_persisted_mcp(
                                &connection.application,
                                &connection.binding,
                            ) {
                                eprintln!("alexd: MCP recovery failed for {identity}: {error}");
                            }
                        })
                    {
                        eprintln!("alexd: MCP recovery queue rejected: {error}");
                    }
                }),
            )
            .map_err(|error| error.to_string())?,
        );
        self.restore_mcp_connections();
        Ok(self)
    }

    pub fn with_manager(mut self, manager: Arc<dyn crate::manager::AppManager>) -> Self {
        self.manager = Some(manager);
        self.restore_mcp_connections();
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
            ControlCommand::NativeWorkerStart { app_id, binding } => {
                self.start_installed_native_worker(&app_id, &binding)
            }
            ControlCommand::NativeWorkerInvoke {
                app_id,
                binding,
                method,
                arguments,
                timeout_ms,
            } => self.invoke_native_worker_command(
                &app_id,
                &binding,
                &method,
                arguments,
                timeout_ms,
            ),
            ControlCommand::NativeWorkerStatus { app_id } => self
                .native_worker_status(&app_id)
                .and_then(|status| serde_json::to_value(status).map_err(|e| e.to_string())),
            ControlCommand::NativeWorkerStop { app_id, binding } => self
                .native_worker_stop(&app_id, &binding)
                .map(|()| json!({ "stopped": true })),
            ControlCommand::NativeWorkerRestart { app_id, binding } => {
                self.restart_installed_native_worker(&app_id, &binding)
            }
            ControlCommand::NativeWorkerCancel { app_id, binding } => self
                .native_workers
                .cancel(&app_id, &binding)
                .map(|()| json!({ "cancelRequested": true }))
                .map_err(|error| error.to_string()),
            ControlCommand::NativeWorkerInvokeStream {
                app_id,
                binding,
                method,
                stream_id,
                arguments,
                timeout_ms,
            } => self.invoke_native_worker_stream(
                &app_id,
                &binding,
                &method,
                &stream_id,
                arguments,
                timeout_ms,
            ),
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
            ControlCommand::McpConnections { app_id } => {
                let reconciled = if self.manager.is_some() {
                    self.reconcile_manifest_mcp(&app_id)
                } else {
                    Ok(())
                };
                reconciled.and_then(|_| {
                    serde_json::to_value(
                        self.mcp
                            .list()
                            .into_iter()
                            .filter(|connection| connection.application == app_id)
                            .collect::<Vec<_>>(),
                    )
                    .map_err(|error| error.to_string())
                })
            }
            ControlCommand::McpHealth { app_id } => Ok(json!({
                "connections": self
                    .mcp_health
                    .as_ref()
                    .map(|monitor| monitor.list(&app_id))
                    .unwrap_or_default()
            })),
            ControlCommand::McpConnectStdio {
                app_id,
                binding,
                command,
                args,
                era,
            } => self.mcp_connect_stdio(&app_id, &binding, &command, &args, era),
            ControlCommand::McpConnectHttp {
                app_id,
                binding,
                endpoint,
                era,
                token_account,
            } => self.mcp_connect_http(&app_id, &binding, &endpoint, era, token_account.as_deref()),
            ControlCommand::McpDisconnect { app_id, binding } => {
                self.mcp_disconnect(&app_id, &binding)
            }
            ControlCommand::McpDiscover { app_id, binding } => self
                .mcp
                .get(&app_id, &binding)
                .and_then(|client| client.discover())
                .and_then(|result| {
                    serde_json::to_value(result)
                        .map_err(|error| crate::mcp::McpError::Protocol(error.to_string()))
                })
                .map_err(|error| error.to_string()),
            ControlCommand::McpListTools {
                app_id,
                binding,
                cursor,
            } => self.mcp_list_tools(&app_id, &binding, cursor.as_deref()),
            ControlCommand::McpListResources {
                app_id,
                binding,
                cursor,
            } => self
                .mcp
                .get(&app_id, &binding)
                .and_then(|client| client.list_resources(cursor.as_deref()))
                .map_err(|error| error.to_string()),
            ControlCommand::McpReadResource {
                app_id,
                binding,
                uri,
            } => self
                .mcp
                .get(&app_id, &binding)
                .and_then(|client| client.read_resource(&uri))
                .map_err(|error| error.to_string()),
            ControlCommand::McpListPrompts {
                app_id,
                binding,
                cursor,
            } => self
                .mcp
                .get(&app_id, &binding)
                .and_then(|client| client.list_prompts(cursor.as_deref()))
                .map_err(|error| error.to_string()),
            ControlCommand::McpGetPrompt {
                app_id,
                binding,
                name,
                arguments,
            } => self
                .mcp
                .get(&app_id, &binding)
                .and_then(|client| client.get_prompt(&name, arguments))
                .map_err(|error| error.to_string()),
            ControlCommand::McpComplete {
                app_id,
                binding,
                reference,
                argument,
            } => self
                .mcp
                .get(&app_id, &binding)
                .and_then(|client| client.complete(reference, argument))
                .map_err(|error| error.to_string()),
            ControlCommand::McpPing { app_id, binding } => self
                .mcp
                .get(&app_id, &binding)
                .and_then(|client| client.ping())
                .map(|_| json!({ "ok": true }))
                .map_err(|error| error.to_string()),
            ControlCommand::McpListen {
                app_id,
                binding,
                stream_id,
                filter,
            } => self.mcp_listen(&app_id, &binding, &stream_id, filter),
            ControlCommand::McpCallTool {
                app_id,
                binding,
                name,
                arguments,
                approval_token,
            } => self.mcp_call_tool(&id, &app_id, &binding, &name, arguments, approval_token.as_deref()),
            ControlCommand::McpApprovalIssue { app_id, binding, name, argument_hash } => {
                self.require_mcp_tool_policy(&app_id, &binding, &name, true).and_then(|_| self.mcp_approvals.issue(
                    crate::mcp::ApprovalBinding {
                        application: app_id,
                        connection: binding,
                        tool: name,
                        argument_hash,
                    },
                    crate::mcp::ApprovalStore::DEFAULT_TTL,
                ).map(|token| json!({"approvalToken": token, "expiresInMs": crate::mcp::ApprovalStore::DEFAULT_TTL.as_millis()}))
                 .map_err(|error| error.to_string()))
            }
            ControlCommand::McpRevokeApplication { app_id, reason } => {
                let approvals = self.mcp_approvals.revoke_application(&app_id);
                let streams = self.streams.close_app(&app_id);
                Ok(json!({"revokedApprovals": approvals, "cancelledStreams": streams, "reason": reason}))
            }
            ControlCommand::McpCallToolInteractive {
                app_id,
                binding,
                stream_id,
                name,
                arguments,
                approval_token,
                allowed_input_methods,
            } => self.mcp_call_tool_interactive(
                &app_id,
                &binding,
                &stream_id,
                &name,
                arguments,
                approval_token.as_deref(),
                allowed_input_methods,
            ),
            ControlCommand::McpInputRespond {
                app_id,
                input_id,
                response,
            } => self.mcp_input_respond(&app_id, &input_id, response),
            ControlCommand::McpAudit { app_id, limit } => self.mcp_audit(&app_id, limit),
            ControlCommand::McpOAuthBegin {
                app_id,
                binding,
                client_id,
                redirect_uri,
                scopes,
            } => self.mcp_oauth_begin(&app_id, &binding, &client_id, &redirect_uri, &scopes),
            ControlCommand::McpOAuthLoopback {
                app_id,
                binding,
                client_id,
                scopes,
            } => self.mcp_oauth_loopback(&app_id, &binding, &client_id, &scopes),
            ControlCommand::McpOAuthComplete {
                app_id,
                state,
                code,
                issuer,
            } => self.mcp_oauth_complete(&app_id, &state, &code, &issuer),
            ControlCommand::ModelList => self.model_list(),
            ControlCommand::ModelImport { source, manifest } => {
                self.model_import(&source, manifest)
            }
            ControlCommand::ModelDownloadStart { request } => self.model_download_start(request),
            ControlCommand::ModelDownloadList => self.model_download_list(),
            ControlCommand::ModelDownloadStatus { task_id } => self.model_download_status(&task_id),
            ControlCommand::ModelDownloadPause { task_id } => self.model_download_pause(&task_id),
            ControlCommand::ModelDownloadResume { task_id } => self.model_download_resume(&task_id),
            ControlCommand::ModelRemove { model_id } => self.model_remove(&model_id),
            ControlCommand::ModelLoad { model_id, worker } => self.model_load(&model_id, &worker),
            ControlCommand::ModelUnload { model_id } => self.model_unload(&model_id),
            ControlCommand::ModelCancel {
                model_id,
                request_id,
            } => self.model_cancel(&model_id, &request_id),
            ControlCommand::ModelGenerate {
                app_id,
                stream_id,
                request,
            } => self.model_generate(&app_id, &stream_id, request),
            ControlCommand::ModelEmbed { request } => self.model_embed(request),
            ControlCommand::ModelProviders => self.model_providers(),
            ControlCommand::ModelHardware => Ok(json!(crate::model::hardware::discover())),
            ControlCommand::ModelRuntimeStatus => self.model_runtime_status(),
            ControlCommand::ModelWorkerPackages => self.model_worker_packages(),
            ControlCommand::ModelWorkerInstall { request } => self.model_worker_install(request),
            ControlCommand::ModelWorkerActivate { kind, version, triple } => self.model_worker_activate(&kind, &version, &triple),
            ControlCommand::ModelProviderUpsert { config } => self.model_provider_upsert(config),
            ControlCommand::ModelProviderRemove { provider_id } => {
                self.model_provider_remove(&provider_id)
            }
            ControlCommand::ModelProviderHealth { provider_id } => {
                self.model_provider_health(provider_id.as_deref())
            }
            ControlCommand::ModelSecretSet {
                service,
                account,
                secret,
            } => self.model_secret_set(&service, &account, &secret),
            ControlCommand::ModelSecretDelete { service, account } => {
                self.model_secret_delete(&service, &account)
            }
            ControlCommand::ModelSecretExists { service, account } => {
                self.model_secret_exists(&service, &account)
            }
            ControlCommand::AgentCreate {
                app_id,
                spec,
                messages,
            } => self.agent_create(&app_id, spec, messages),
            ControlCommand::AgentSpawnChild { app_id, parent_run_id, spec, messages } => {
                self.agent_spawn_child(&app_id, &parent_run_id, spec, messages)
            }
            ControlCommand::AgentChildren { app_id, parent_run_id } => {
                self.agent_children(&app_id, &parent_run_id)
            }
            ControlCommand::AgentWaitChildren {
                app_id,
                parent_run_id,
                wait_ms,
                cancel_on_timeout,
            } => self.agent_wait_children(
                &app_id,
                &parent_run_id,
                wait_ms,
                cancel_on_timeout,
            ),
            ControlCommand::AgentSchedule { app_id, run_id, scheduled_at_ms } => {
                self.agent_schedule(&app_id, &run_id, scheduled_at_ms)
            }
            ControlCommand::AgentScheduled { app_id } => self.agent_scheduled(&app_id),
            ControlCommand::AgentStart {
                app_id,
                run_id,
                stream_id,
            } => self.agent_start(&app_id, &run_id, &stream_id),
            ControlCommand::AgentPause { app_id, run_id } => {
                self.agent_action(&app_id, &run_id, "pause")
            }
            ControlCommand::AgentResume { app_id, run_id } => {
                self.agent_action(&app_id, &run_id, "resume")
            }
            ControlCommand::AgentCancel { app_id, run_id } => {
                self.agent_action(&app_id, &run_id, "cancel")
            }
            ControlCommand::AgentApprove { app_id, run_id } => {
                self.agent_action(&app_id, &run_id, "approve")
            }
            ControlCommand::AgentDeny { app_id, run_id } => {
                self.agent_action(&app_id, &run_id, "deny")
            }
            ControlCommand::AgentStatus { app_id, run_id } => self.agent_status(&app_id, &run_id),
            ControlCommand::AgentList { app_id } => self.agent_list(&app_id),
            ControlCommand::AgentHistory {
                app_id,
                run_id,
                limit,
            } => self.agent_history(&app_id, &run_id, limit),
            ControlCommand::AgentTimeline {
                app_id,
                run_id,
                limit,
            } => self.agent_timeline(&app_id, &run_id, limit),
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
        let native_workers = self
            .native_workers
            .stop_all()
            .map_err(|error| error.to_string())?;
        let Some(manager) = &self.manager else {
            return Ok(json!({ "stopped": [], "nativeWorkers": native_workers, "errors": [] }));
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
        Ok(json!({ "stopped": stopped, "nativeWorkers": native_workers, "errors": errors }))
    }

    fn start(&self, app_id: &str) -> Result<serde_json::Value, String> {
        if let Some(manager) = &self.manager {
            manager.get_app(app_id).map_err(|error| error.to_string())?;
            self.reconcile_manifest_mcp(app_id)?;
            let status = manager.launch(app_id).map_err(|error| error.to_string())?;
            self.set_desired(app_id, DesiredState::Running)?;
            self.record_status(app_id, &status)?;
            return Ok(json!(status));
        }
        self.set_desired(app_id, DesiredState::Running)
    }

    fn stop(&self, app_id: &str) -> Result<serde_json::Value, String> {
        self.native_workers
            .stop_application(app_id)
            .map_err(|error| error.to_string())?;
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
            self.reconcile_manifest_mcp(app_id)?;
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

    fn mcp_connect_stdio(
        &self,
        app_id: &str,
        binding: &str,
        command: &str,
        args: &[String],
        era: crate::mcp::ProtocolEra,
    ) -> Result<serde_json::Value, String> {
        let package_root = self
            .manager
            .as_ref()
            .ok_or_else(|| "application manager unavailable".to_owned())?
            .get_app(app_id)
            .map_err(|error| error.to_string())?
            .install_path;
        let transport =
            crate::mcp::StdioTransport::spawn(&package_root, std::path::Path::new(command), args)
                .map_err(|error| error.to_string())?;
        let client = crate::mcp::McpClient::new(Arc::new(transport), era);
        let server = if era == crate::mcp::ProtocolEra::Legacy {
            Some(
                client
                    .initialize_legacy()
                    .map_err(|error| error.to_string())?,
            )
        } else {
            None
        };
        self.mcp
            .connect(app_id, binding, client)
            .map_err(|error| error.to_string())?;
        if let Some(configs) = &self.mcp_configs
            && let Err(error) = configs.upsert(crate::mcp::PersistedConnection {
                application: app_id.into(),
                binding: binding.into(),
                era,
                transport: crate::mcp::PersistedTransport::Stdio {
                    command: command.into(),
                    args: args.to_vec(),
                },
                enabled: true,
                managed_by_manifest: false,
            })
        {
            self.mcp.disconnect(app_id, binding);
            return Err(error.to_string());
        }
        Ok(json!({
            "application": app_id,
            "binding": binding,
            "era": era,
            "server": server,
        }))
    }

    fn reconcile_manifest_mcp(&self, app_id: &str) -> Result<(), String> {
        let manager = self.manager.as_ref().ok_or("app manager is unavailable")?;
        let details = manager.get_app(app_id).map_err(|error| error.to_string())?;
        let manifest = crate::core::application_manifest::load_application(&details.install_path)
            .map_err(|error| error.to_string())?;
        let resolved = manifest.resolve().map_err(|error| error.to_string())?;
        let connected = self
            .mcp
            .list()
            .into_iter()
            .filter(|connection| connection.application == app_id)
            .map(|connection| connection.binding)
            .collect::<std::collections::BTreeSet<_>>();
        let desired = resolved
            .mcp_servers
            .into_iter()
            .map(|(binding, spec)| {
                let (era, transport) = match spec {
                    crate::core::manifest_v2::McpServerSpec::Stdio {
                        command,
                        args,
                        legacy,
                    } => (
                        if legacy {
                            crate::mcp::ProtocolEra::Legacy
                        } else {
                            crate::mcp::ProtocolEra::Modern
                        },
                        crate::mcp::PersistedTransport::Stdio { command, args },
                    ),
                    crate::core::manifest_v2::McpServerSpec::StreamableHttp {
                        endpoint,
                        token_account,
                        legacy,
                    } => (
                        if legacy {
                            crate::mcp::ProtocolEra::Legacy
                        } else {
                            crate::mcp::ProtocolEra::Modern
                        },
                        crate::mcp::PersistedTransport::StreamableHttp {
                            endpoint,
                            token_account,
                        },
                    ),
                };
                (
                    binding.clone(),
                    crate::mcp::PersistedConnection {
                        application: app_id.into(),
                        binding,
                        era,
                        transport,
                        enabled: true,
                        managed_by_manifest: true,
                    },
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        if let Some(configs) = &self.mcp_configs {
            for stale in configs
                .list()
                .map_err(|error| error.to_string())?
                .into_iter()
                .filter(|value| {
                    value.application == app_id
                        && value.managed_by_manifest
                        && !desired.contains_key(&value.binding)
                })
            {
                self.mcp_disconnect(app_id, &stale.binding)?;
            }
        }
        for (binding, expected) in desired {
            let stored = self
                .mcp_configs
                .as_ref()
                .map(|configs| configs.get(app_id, &binding))
                .transpose()
                .map_err(|error| error.to_string())?
                .flatten();
            let healthy = connected.contains(&binding)
                && self
                    .mcp
                    .get(app_id, &binding)
                    .and_then(|client| client.ping())
                    .is_ok();
            if stored.as_ref() == Some(&expected) && healthy {
                continue;
            }
            if connected.contains(&binding) {
                self.mcp_disconnect(app_id, &binding)?;
            }
            match &expected.transport {
                crate::mcp::PersistedTransport::Stdio { command, args } => {
                    self.mcp_connect_stdio(app_id, &binding, command, args, expected.era)?;
                }
                crate::mcp::PersistedTransport::StreamableHttp {
                    endpoint,
                    token_account,
                } => {
                    self.mcp_connect_http(
                        app_id,
                        &binding,
                        endpoint,
                        expected.era,
                        token_account.as_deref(),
                    )?;
                }
            }
            if let Some(configs) = &self.mcp_configs {
                configs
                    .upsert(expected)
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }

    fn mcp_connect_http(
        &self,
        app_id: &str,
        binding: &str,
        endpoint: &str,
        era: crate::mcp::ProtocolEra,
        token_account: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let mut transport = crate::mcp::StreamableHttpTransport::new(endpoint, era)
            .map_err(|error| error.to_string())?;
        if let Some(account) = token_account {
            let vault = self
                .mcp_tokens
                .clone()
                .ok_or_else(|| "MCP OAuth token vault is unavailable".to_owned())?;
            let provider = crate::mcp::oauth::VaultAccessTokenProvider::new(vault, account)
                .map_err(|error| error.to_string())?;
            transport = transport.with_access_tokens(Arc::new(provider));
        }
        let client = crate::mcp::McpClient::new(Arc::new(transport), era);
        let server = if era == crate::mcp::ProtocolEra::Legacy {
            Some(
                client
                    .initialize_legacy()
                    .map_err(|error| error.to_string())?,
            )
        } else {
            None
        };
        self.mcp
            .connect(app_id, binding, client)
            .map_err(|error| error.to_string())?;
        if let Some(configs) = &self.mcp_configs
            && let Err(error) = configs.upsert(crate::mcp::PersistedConnection {
                application: app_id.into(),
                binding: binding.into(),
                era,
                transport: crate::mcp::PersistedTransport::StreamableHttp {
                    endpoint: endpoint.into(),
                    token_account: token_account.map(str::to_owned),
                },
                enabled: true,
                managed_by_manifest: false,
            })
        {
            self.mcp.disconnect(app_id, binding);
            return Err(error.to_string());
        }
        Ok(json!({
            "application": app_id,
            "binding": binding,
            "era": era,
            "transport": "streamable-http",
            "server": server,
        }))
    }

    fn mcp_disconnect(&self, app_id: &str, binding: &str) -> Result<serde_json::Value, String> {
        if let Some(configs) = &self.mcp_configs {
            configs
                .remove(app_id, binding)
                .map_err(|error| error.to_string())?;
        }
        Ok(json!({
            "disconnected": self.mcp.disconnect(app_id, binding)
        }))
    }

    fn restore_mcp_connections(&self) {
        let Some(configs) = &self.mcp_configs else {
            return;
        };
        let values = match configs.list() {
            Ok(values) => values,
            Err(error) => {
                eprintln!("alexd: failed to load MCP connections: {error}");
                return;
            }
        };
        for value in values.into_iter().filter(|value| value.enabled) {
            let service = self.clone();
            let persisted = value.clone();
            let identity = format!("{}/{}", value.application, value.binding);
            let restore_identity = identity.clone();
            if let Err(error) = crate::runtime::task_executor::mcp_executor().submit(move || {
                if service.mcp.get(&value.application, &value.binding).is_ok() {
                    return;
                }
                let result = match &value.transport {
                    crate::mcp::PersistedTransport::Stdio { command, args } => service
                        .mcp_connect_stdio(
                            &value.application,
                            &value.binding,
                            command,
                            args,
                            value.era,
                        ),
                    crate::mcp::PersistedTransport::StreamableHttp {
                        endpoint,
                        token_account,
                    } => service.mcp_connect_http(
                        &value.application,
                        &value.binding,
                        endpoint,
                        value.era,
                        token_account.as_deref(),
                    ),
                };
                if let Err(error) = result {
                    eprintln!(
                        "alexd: failed to restore MCP connection {restore_identity}: {error}"
                    );
                } else if let Some(configs) = &service.mcp_configs
                    && let Err(error) = configs.upsert(persisted)
                {
                    eprintln!("alexd: failed to preserve MCP connection ownership: {error}");
                }
            }) {
                eprintln!("alexd: MCP restore queue rejected {identity}: {error}");
            }
        }
    }

    fn reconnect_persisted_mcp(&self, app_id: &str, binding: &str) -> Result<(), String> {
        let config = self
            .mcp_configs
            .as_ref()
            .ok_or_else(|| "MCP connection store is unavailable".to_owned())?
            .get(app_id, binding)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("MCP connection {app_id}/{binding} is no longer configured"))?;
        if !config.enabled {
            return Ok(());
        }
        self.mcp.disconnect(app_id, binding);
        match &config.transport {
            crate::mcp::PersistedTransport::Stdio { command, args } => {
                self.mcp_connect_stdio(app_id, binding, command, args, config.era)?;
            }
            crate::mcp::PersistedTransport::StreamableHttp {
                endpoint,
                token_account,
            } => {
                self.mcp_connect_http(
                    app_id,
                    binding,
                    endpoint,
                    config.era,
                    token_account.as_deref(),
                )?;
            }
        }
        self.mcp_configs
            .as_ref()
            .expect("connection store checked above")
            .upsert(config)
            .map_err(|error| error.to_string())
    }

    fn mcp_list_tools(
        &self,
        app_id: &str,
        binding: &str,
        cursor: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let (tools, next_cursor) = self
            .mcp
            .get(app_id, binding)
            .and_then(|client| client.list_tools(cursor))
            .map_err(|error| error.to_string())?;
        Ok(json!({ "tools": tools, "nextCursor": next_cursor }))
    }

    /// Resolve MCP authorization from the daemon-owned installed manifest.
    /// When no manager is attached (isolated protocol/unit tests), policy is
    /// enforced by the embedding caller as before. Production alexd always
    /// has a manager and therefore never trusts a client-provided flag.
    fn require_mcp_tool_policy(
        &self,
        app_id: &str,
        binding: &str,
        tool: &str,
        issuing_approval: bool,
    ) -> Result<bool, String> {
        let Some(manager) = &self.manager else {
            return Ok(issuing_approval);
        };
        let details = manager.get_app(app_id).map_err(|error| error.to_string())?;
        let granted = details.permissions.iter().any(|permission| {
            permission.name == "mcp.use"
                && permission.decision == crate::authorization::PermissionDecision::Granted
        });
        if !granted {
            self.mcp_approvals.revoke_application(app_id);
            return Err("mcp.use is not granted or was revoked".into());
        }
        let policy = details
            .manifest
            .permissions
            .iter()
            .find_map(|permission| {
                if let crate::permission::Permission::McpUse {
                    servers,
                    tools,
                    always_ask,
                    ..
                } = permission
                    && servers.iter().any(|server| server == binding)
                    && tools
                        .get(binding)
                        .is_some_and(|allowed| allowed.iter().any(|name| name == tool))
                {
                    return Some(
                        always_ask
                            .get(binding)
                            .is_some_and(|names| names.iter().any(|name| name == tool)),
                    );
                }
                None
            })
            .ok_or_else(|| {
                "MCP binding or tool is not declared by the installed manifest".to_owned()
            })?;
        if issuing_approval && !policy {
            return Err("MCP tool does not use the always-ask policy".into());
        }
        Ok(policy)
    }

    fn mcp_call_tool(
        &self,
        call_id: &str,
        app_id: &str,
        binding: &str,
        name: &str,
        arguments: serde_json::Value,
        approval_token: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let always_ask = self.require_mcp_tool_policy(app_id, binding, name, false)?;
        let mut started = crate::mcp::AuditLog::entry(call_id, app_id, binding, name, "started");
        started.argument_hash =
            Some(crate::mcp::audit_argument_hash(&arguments).map_err(|error| error.to_string())?);
        if always_ask && approval_token.is_none() {
            return Err("MCP approval token is required by the installed manifest".into());
        }
        if let Some(token) = approval_token {
            self.mcp_approvals
                .consume(
                    token,
                    &crate::mcp::ApprovalBinding {
                        application: app_id.into(),
                        connection: binding.into(),
                        tool: name.into(),
                        argument_hash: started
                            .argument_hash
                            .clone()
                            .expect("argument hash assigned"),
                    },
                )
                .map_err(|error| error.to_string())?;
        }
        if let Some(audit) = &self.mcp_audit {
            audit
                .append(&started)
                .map_err(|error| format!("MCP audit unavailable; tool was not invoked: {error}"))?;
        }
        let start = std::time::Instant::now();
        let result = self
            .mcp
            .get(app_id, binding)
            .and_then(|client| client.call_tool(name, arguments))
            .and_then(crate::mcp::filter_tool_result)
            .and_then(|result| {
                serde_json::to_value(result)
                    .map_err(|error| crate::mcp::McpError::Protocol(error.to_string()))
            });
        started.timestamp_ms = now_ms().unwrap_or_default();
        started.phase = "finished".into();
        started.duration_ms = Some(start.elapsed().as_millis().try_into().unwrap_or(u64::MAX));
        match result {
            Ok(value) => {
                started.outcome = Some("success".into());
                if let Some(audit) = &self.mcp_audit {
                    audit.append(&started).map_err(|error| {
                        format!(
                            "MCP tool completed, but its audit outcome could not be persisted; do not retry automatically: {error}"
                        )
                    })?;
                }
                Ok(value)
            }
            Err(error) => {
                started.outcome = Some("failure".into());
                started.error_kind = Some(
                    match &error {
                        crate::mcp::McpError::NotFound(_) => "not-found",
                        crate::mcp::McpError::Duplicate(_) => "duplicate",
                        crate::mcp::McpError::InvalidConfig(_) => "invalid-config",
                        crate::mcp::McpError::Transport(_) => "transport",
                        crate::mcp::McpError::Protocol(_) => "protocol",
                        crate::mcp::McpError::Server { .. } => "server",
                        crate::mcp::McpError::Authorization(_) => "authorization",
                        crate::mcp::McpError::InputRequired(_) => "input_required",
                    }
                    .into(),
                );
                if let Some(audit) = &self.mcp_audit
                    && let Err(audit_error) = audit.append(&started)
                {
                    return Err(format!(
                        "{error}; additionally failed to persist MCP audit outcome: {audit_error}"
                    ));
                }
                Err(error.to_string())
            }
        }
    }

    fn mcp_audit(&self, app_id: &str, limit: usize) -> Result<serde_json::Value, String> {
        let audit = self
            .mcp_audit
            .as_ref()
            .ok_or_else(|| "MCP audit is unavailable".to_owned())?;
        let integrity = audit.verify().map_err(|error| error.to_string())?;
        let entries = audit
            .recent(app_id, limit)
            .map_err(|error| error.to_string())?;
        Ok(json!({ "entries": entries, "integrity": integrity }))
    }

    fn mcp_call_tool_interactive(
        &self,
        app_id: &str,
        binding: &str,
        stream_id: &str,
        name: &str,
        arguments: serde_json::Value,
        approval_token: Option<&str>,
        allowed_input_methods: Vec<String>,
    ) -> Result<serde_json::Value, String> {
        let always_ask = self.require_mcp_tool_policy(app_id, binding, name, false)?;
        let base = self
            .mcp
            .get(app_id, binding)
            .map_err(|error| error.to_string())?;
        let cancellation = self
            .streams
            .open(app_id, stream_id)
            .map_err(|error| error.to_string())?;
        let handler = StreamingMcpInputHandler {
            application: app_id.into(),
            stream_id: stream_id.into(),
            streams: Arc::clone(&self.streams),
            cancellation: cancellation.clone(),
            pending: Arc::clone(&self.mcp_input_pending),
            allowed: allowed_input_methods.into_iter().collect(),
            next_id: std::sync::atomic::AtomicU64::new(1),
        };
        let client = base
            .with_input_handler(Arc::new(handler), 10)
            .map_err(|error| error.to_string())?;
        let mut audit_entry =
            crate::mcp::AuditLog::entry(stream_id, app_id, binding, name, "started");
        audit_entry.argument_hash =
            Some(crate::mcp::audit_argument_hash(&arguments).map_err(|error| error.to_string())?);
        if always_ask && approval_token.is_none() {
            return Err("MCP approval token is required by the installed manifest".into());
        }
        if let Some(token) = approval_token {
            self.mcp_approvals
                .consume(
                    token,
                    &crate::mcp::ApprovalBinding {
                        application: app_id.into(),
                        connection: binding.into(),
                        tool: name.into(),
                        argument_hash: audit_entry
                            .argument_hash
                            .clone()
                            .expect("argument hash assigned"),
                    },
                )
                .map_err(|error| error.to_string())?;
        }
        if let Some(audit) = &self.mcp_audit {
            audit.append(&audit_entry).map_err(|error| {
                format!("MCP audit unavailable; interactive tool was not invoked: {error}")
            })?;
        }
        let audit = self.mcp_audit.clone();
        let streams = Arc::clone(&self.streams);
        let worker_stream_id = stream_id.to_owned();
        let tool = name.to_owned();
        std::thread::Builder::new()
            .name("alex-mcp-mrtr".into())
            .spawn(move || {
                let started_at = std::time::Instant::now();
                let result = client
                    .call_tool(&tool, arguments)
                    .and_then(crate::mcp::filter_tool_result);
                audit_entry.timestamp_ms = now_ms().unwrap_or_default();
                audit_entry.phase = "finished".into();
                audit_entry.duration_ms = Some(
                    started_at
                        .elapsed()
                        .as_millis()
                        .try_into()
                        .unwrap_or(u64::MAX),
                );
                audit_entry.outcome =
                    Some(if result.is_ok() { "success" } else { "failure" }.into());
                if let Err(error) = &result {
                    audit_entry.error_kind = Some(mcp_error_kind(error).into());
                }
                if let Some(audit) = audit {
                    let _ = audit.append(&audit_entry);
                }
                if cancellation.is_cancelled() {
                    return;
                }
                let terminal = match result {
                    Ok(result) => {
                        match serde_json::to_vec(&json!({"type":"result","result":result})) {
                            Ok(payload) => match push_stream_with_cancel(
                                &streams,
                                &worker_stream_id,
                                &cancellation,
                                payload,
                            ) {
                                Ok(()) => crate::runtime::stream::StreamTerminal::Completed,
                                Err(error) => crate::runtime::stream::StreamTerminal::Failed {
                                    code: "MCP_MRTR_STREAM_FAILED".into(),
                                    message: error.to_string(),
                                },
                            },
                            Err(error) => crate::runtime::stream::StreamTerminal::Failed {
                                code: "MCP_MRTR_ENCODE_FAILED".into(),
                                message: error.to_string(),
                            },
                        }
                    }
                    Err(error) => crate::runtime::stream::StreamTerminal::Failed {
                        code: "MCP_MRTR_FAILED".into(),
                        message: error.to_string(),
                    },
                };
                let _ = streams.finish(&worker_stream_id, terminal);
            })
            .map_err(|error| error.to_string())?;
        Ok(json!({"streamId":stream_id,"binding":binding,"tool":name}))
    }

    fn mcp_input_respond(
        &self,
        app_id: &str,
        input_id: &str,
        response: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let pending = self
            .mcp_input_pending
            .lock()
            .map_err(|_| "MCP input broker lock poisoned".to_owned())?;
        let input = pending
            .get(input_id)
            .ok_or_else(|| "MCP input request was not found or already completed".to_owned())?;
        if input.application != app_id {
            return Err("MCP input request belongs to another application".into());
        }
        let (gate, changed) = &*input.response;
        let mut slot = gate
            .lock()
            .map_err(|_| "MCP input response lock poisoned".to_owned())?;
        if slot.is_some() {
            return Err("MCP input request was already answered".into());
        }
        *slot = Some(response);
        changed.notify_all();
        Ok(json!({"inputId":input_id,"accepted":true}))
    }

    fn mcp_listen(
        &self,
        app_id: &str,
        binding: &str,
        stream_id: &str,
        filter: crate::mcp::SubscriptionFilter,
    ) -> Result<serde_json::Value, String> {
        self.mcp
            .get(app_id, binding)
            .map_err(|error| error.to_string())?;
        let cancellation = self
            .streams
            .open(app_id, stream_id)
            .map_err(|error| error.to_string())?;
        let streams = Arc::clone(&self.streams);
        let connections = self.mcp.clone();
        let application = app_id.to_owned();
        let subscription_binding = binding.to_owned();
        let worker_stream_id = stream_id.to_owned();
        std::thread::Builder::new()
            .name("alex-mcp-subscription".into())
            .spawn(move || {
                let mut emit = |notification: serde_json::Value| {
                    let payload = serde_json::to_vec(&notification)
                        .map_err(|error| crate::mcp::McpError::Protocol(error.to_string()))?;
                    loop {
                        if cancellation.is_cancelled() {
                            return Err(crate::mcp::McpError::Transport(
                                "subscription cancelled".into(),
                            ));
                        }
                        match streams.push(&worker_stream_id, payload.clone()) {
                            Ok(_) => return Ok(()),
                            Err(crate::runtime::stream::StreamError::Backpressured { .. })
                            | Err(crate::runtime::stream::StreamError::BufferFull { .. }) => {
                                std::thread::sleep(std::time::Duration::from_millis(5));
                            }
                            Err(error) => {
                                return Err(crate::mcp::McpError::Transport(error.to_string()));
                            }
                        }
                    }
                };
                let mut retry = std::time::Duration::from_millis(250);
                loop {
                    if cancellation.is_cancelled() {
                        return;
                    }
                    let result = connections
                        .get(&application, &subscription_binding)
                        .and_then(|client| client.listen(filter.clone(), &mut emit));
                    if cancellation.is_cancelled() {
                        return;
                    }
                    if result.is_ok() {
                        let _ = streams.finish(
                            &worker_stream_id,
                            crate::runtime::stream::StreamTerminal::Completed,
                        );
                        return;
                    }
                    let deadline = std::time::Instant::now() + retry;
                    while std::time::Instant::now() < deadline {
                        if cancellation.is_cancelled() {
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    retry = retry
                        .saturating_mul(2)
                        .min(std::time::Duration::from_secs(30));
                }
            })
            .map_err(|error| {
                let _ = self.streams.finish(
                    stream_id,
                    crate::runtime::stream::StreamTerminal::Failed {
                        code: "MCP_SUBSCRIPTION_START_FAILED".into(),
                        message: error.to_string(),
                    },
                );
                error.to_string()
            })?;
        Ok(json!({ "streamId": stream_id, "binding": binding }))
    }

    fn mcp_oauth_begin(
        &self,
        app_id: &str,
        binding: &str,
        client_id: &str,
        redirect_uri: &str,
        scopes: &[String],
    ) -> Result<serde_json::Value, String> {
        let config = self
            .mcp_configs
            .as_ref()
            .ok_or_else(|| "MCP connection store is unavailable".to_owned())?
            .get(app_id, binding)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("MCP connection {app_id}/{binding} is not configured"))?;
        let crate::mcp::PersistedTransport::StreamableHttp { endpoint, .. } = config.transport
        else {
            return Err("OAuth is only valid for Streamable HTTP connections".into());
        };
        let oauth = crate::mcp::oauth::OAuthClient::default();
        let resource = oauth
            .discover_resource(&endpoint)
            .map_err(|error| error.to_string())?;
        let issuer = resource
            .authorization_servers
            .first()
            .ok_or_else(|| "OAuth resource has no authorization server".to_owned())?;
        let metadata = oauth
            .discover_authorization_server(issuer)
            .map_err(|error| error.to_string())?;
        let request = oauth
            .begin(&endpoint, &metadata, client_id, redirect_uri, scopes)
            .map_err(|error| error.to_string())?;
        let token_account = format!("{app_id}:{binding}");
        let pending = PendingMcpOAuth {
            application: app_id.into(),
            binding: binding.into(),
            token_account,
            request: request.clone(),
            created_at: std::time::Instant::now(),
        };
        let mut values = self
            .mcp_oauth_pending
            .lock()
            .map_err(|_| "MCP OAuth pending-state lock poisoned".to_owned())?;
        values.retain(|_, value| value.created_at.elapsed() < std::time::Duration::from_secs(600));
        if values.len() >= 32 {
            return Err("MCP OAuth pending-state limit reached".into());
        }
        values.insert(request.state.clone(), pending);
        Ok(json!({
            "authorizationUrl": request.authorization_url,
            "state": request.state,
            "expiresInMs": 600_000,
        }))
    }

    fn mcp_oauth_complete(
        &self,
        app_id: &str,
        state: &str,
        code: &str,
        issuer: &str,
    ) -> Result<serde_json::Value, String> {
        let pending = self
            .mcp_oauth_pending
            .lock()
            .map_err(|_| "MCP OAuth pending-state lock poisoned".to_owned())?
            .remove(state)
            .ok_or_else(|| "MCP OAuth state is unknown or already consumed".to_owned())?;
        if pending.application != app_id {
            self.mcp_oauth_pending
                .lock()
                .map_err(|_| "MCP OAuth pending-state lock poisoned".to_owned())?
                .insert(state.to_owned(), pending);
            return Err("MCP OAuth state belongs to another application".into());
        }
        if pending.created_at.elapsed() >= std::time::Duration::from_secs(600) {
            return Err("MCP OAuth state expired".into());
        }
        let tokens = crate::mcp::oauth::OAuthClient::default()
            .exchange_code(&pending.request, code, state, issuer)
            .map_err(|error| error.to_string())?;
        self.mcp_tokens
            .as_ref()
            .ok_or_else(|| "MCP OAuth token vault is unavailable".to_owned())?
            .save(&pending.token_account, &tokens)
            .map_err(|error| error.to_string())?;
        let config = self
            .mcp_configs
            .as_ref()
            .ok_or_else(|| "MCP connection store is unavailable".to_owned())?
            .get(&pending.application, &pending.binding)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "MCP connection was removed during authorization".to_owned())?;
        let crate::mcp::PersistedTransport::StreamableHttp { endpoint, .. } = config.transport
        else {
            return Err("MCP connection transport changed during authorization".into());
        };
        self.mcp.disconnect(&pending.application, &pending.binding);
        self.mcp_connect_http(
            &pending.application,
            &pending.binding,
            &endpoint,
            config.era,
            Some(&pending.token_account),
        )?;
        Ok(json!({
            "application": pending.application,
            "binding": pending.binding,
            "authorized": true,
        }))
    }

    fn mcp_oauth_loopback(
        &self,
        app_id: &str,
        binding: &str,
        client_id: &str,
        scopes: &[String],
    ) -> Result<serde_json::Value, String> {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .map_err(|error| format!("failed to bind OAuth loopback callback: {error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("failed to configure OAuth loopback callback: {error}"))?;
        let port = listener
            .local_addr()
            .map_err(|error| error.to_string())?
            .port();
        let redirect_uri = format!("http://127.0.0.1:{port}/oauth/callback");
        let result = self.mcp_oauth_begin(app_id, binding, client_id, &redirect_uri, scopes)?;
        let state = result["state"]
            .as_str()
            .ok_or_else(|| "OAuth begin omitted state".to_owned())?
            .to_owned();
        let service = self.clone();
        let application = app_id.to_owned();
        let worker_state = state.clone();
        std::thread::Builder::new()
            .name("alex-mcp-oauth-loopback".into())
            .spawn(move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
                loop {
                    if std::time::Instant::now() >= deadline {
                        return;
                    }
                    match listener.accept() {
                        Ok((mut stream, address)) => {
                            if !address.ip().is_loopback() {
                                continue;
                            }
                            let response = service.handle_oauth_loopback_stream(
                                &application,
                                &worker_state,
                                &mut stream,
                            );
                            let (status, body) = match response {
                                Ok(()) => ("200 OK", "Authorization completed. You can close this window."),
                                Err(error) => {
                                    eprintln!("alexd: OAuth loopback callback failed: {error}");
                                    ("400 Bad Request", "Authorization failed. Return to Alex OS and try again.")
                                }
                            };
                            let reply = format!(
                                "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}",
                                body.len()
                            );
                            use std::io::Write as _;
                            let _ = stream.write_all(reply.as_bytes());
                            return;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(25));
                        }
                        Err(error) => {
                            eprintln!("alexd: OAuth loopback listener failed: {error}");
                            return;
                        }
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(json!({
            "authorizationUrl": result["authorizationUrl"],
            "state": state,
            "redirectUri": redirect_uri,
            "expiresInMs": 600_000
        }))
    }

    fn handle_oauth_loopback_stream(
        &self,
        app_id: &str,
        expected_state: &str,
        stream: &mut std::net::TcpStream,
    ) -> Result<(), String> {
        use std::io::Read as _;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .map_err(|error| error.to_string())?;
        let mut buffer = [0_u8; 16 * 1024];
        let size = stream
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        let request = std::str::from_utf8(&buffer[..size])
            .map_err(|_| "OAuth callback request was not UTF-8".to_owned())?;
        let line = request
            .lines()
            .next()
            .ok_or_else(|| "OAuth callback request was empty".to_owned())?;
        let mut parts = line.split_whitespace();
        if parts.next() != Some("GET") {
            return Err("OAuth callback must use GET".into());
        }
        let target = parts
            .next()
            .ok_or_else(|| "OAuth callback target is missing".to_owned())?;
        let url = url::Url::parse(&format!("http://127.0.0.1{target}"))
            .map_err(|error| error.to_string())?;
        if url.path() != "/oauth/callback" {
            return Err("OAuth callback path is invalid".into());
        }
        let query = url
            .query_pairs()
            .collect::<std::collections::BTreeMap<_, _>>();
        let state = query
            .get("state")
            .ok_or_else(|| "OAuth callback state is missing".to_owned())?;
        if state.as_ref() != expected_state {
            return Err("OAuth callback state mismatch".into());
        }
        let code = query
            .get("code")
            .ok_or_else(|| "OAuth callback code is missing".to_owned())?;
        let issuer = self
            .mcp_oauth_pending
            .lock()
            .map_err(|_| "MCP OAuth pending-state lock poisoned".to_owned())?
            .get(expected_state)
            .ok_or_else(|| "OAuth state is no longer pending".to_owned())?
            .request
            .issuer
            .clone();
        if let Some(returned) = query.get("iss")
            && returned.as_ref() != issuer
        {
            return Err("OAuth callback issuer mismatch".into());
        }
        self.mcp_oauth_complete(app_id, expected_state, code, &issuer)?;
        Ok(())
    }

    fn model_manager(&self) -> Result<&crate::model::ModelManager, String> {
        self.models
            .as_ref()
            .ok_or_else(|| "model runtime is not configured".into())
    }

    fn model_list(&self) -> Result<serde_json::Value, String> {
        let models = self
            .model_manager()?
            .list()
            .map_err(|error| error.to_string())?;
        Ok(json!({ "models": models }))
    }

    fn model_runtime_status(&self) -> Result<serde_json::Value, String> {
        Ok(json!({
            "hardware": crate::model::hardware::discover(),
            "resources": self.model_manager()?.resource_status(),
            "workers": self.model_manager()?.worker_health()
        }))
    }
    fn worker_package_store(
        &self,
    ) -> Result<&crate::model::worker_packages::WorkerPackageStore, String> {
        self.worker_packages
            .as_ref()
            .ok_or_else(|| "worker package store is not configured".into())
    }
    fn model_worker_packages(&self) -> Result<serde_json::Value, String> {
        self.worker_package_store()?
            .list()
            .map(|packages| json!({"packages":packages}))
            .map_err(|error| error.to_string())
    }
    fn model_worker_install(
        &self,
        request: crate::model::worker_packages::WorkerPackageRequest,
    ) -> Result<serde_json::Value, String> {
        let store = self.worker_package_store()?;
        let previous = store
            .list()
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|package| package.worker_kind == request.manifest.worker_kind && package.active);
        let installed = store
            .download_install(&request)
            .map_err(|error| error.to_string())?;
        if let Err(error) = self
            .model_manager()?
            .register_process_workers(&store.runtimes_root())
        {
            match previous {
                Some(previous) => {
                    store.activate(&previous.worker_kind, &previous.version, &previous.triple)
                }
                None => store.deactivate(&request.manifest.worker_kind),
            }
            .map_err(|rollback| {
                format!("worker reload failed: {error}; rollback failed: {rollback}")
            })?;
            self.model_manager()?
                .register_process_workers(&store.runtimes_root())
                .map_err(|rollback| {
                    format!("worker reload failed: {error}; in-memory rollback failed: {rollback}")
                })?;
            return Err(format!(
                "worker reload failed; previous version restored: {error}"
            ));
        }
        serde_json::to_value(installed).map_err(|error| error.to_string())
    }
    fn model_worker_activate(
        &self,
        kind: &str,
        version: &str,
        triple: &str,
    ) -> Result<serde_json::Value, String> {
        let store = self.worker_package_store()?;
        let previous = store
            .list()
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|package| package.worker_kind == kind && package.active);
        store
            .activate(kind, version, triple)
            .map_err(|error| error.to_string())?;
        if let Err(error) = self
            .model_manager()?
            .register_process_workers(&store.runtimes_root())
        {
            match previous {
                Some(previous) => {
                    store.activate(&previous.worker_kind, &previous.version, &previous.triple)
                }
                None => store.deactivate(kind),
            }
            .map_err(|rollback| {
                format!("worker activation failed: {error}; rollback failed: {rollback}")
            })?;
            self.model_manager()?
                .register_process_workers(&store.runtimes_root())
                .map_err(|rollback| {
                    format!(
                        "worker activation failed: {error}; in-memory rollback failed: {rollback}"
                    )
                })?;
            return Err(format!(
                "worker activation failed; previous version restored: {error}"
            ));
        }
        Ok(json!({"kind":kind,"version":version,"triple":triple,"active":true}))
    }

    fn model_import(
        &self,
        source: &str,
        manifest: crate::model::ModelManifest,
    ) -> Result<serde_json::Value, String> {
        let model = self
            .model_manager()?
            .import(std::path::Path::new(source), manifest)
            .map_err(|error| error.to_string())?;
        serde_json::to_value(model).map_err(|error| error.to_string())
    }

    fn model_download_manager(
        &self,
    ) -> Result<&crate::model::download_tasks::ModelDownloadManager, String> {
        self.model_downloads
            .as_ref()
            .ok_or_else(|| "model download runtime is not configured".into())
    }

    fn model_download_start(
        &self,
        request: crate::model::ModelDownloadRequest,
    ) -> Result<serde_json::Value, String> {
        serde_json::to_value(self.model_download_manager()?.start(request)?)
            .map_err(|error| error.to_string())
    }

    fn model_download_list(&self) -> Result<serde_json::Value, String> {
        Ok(json!({ "tasks": self.model_download_manager()?.list() }))
    }

    fn model_download_status(&self, task_id: &str) -> Result<serde_json::Value, String> {
        let task = self
            .model_download_manager()?
            .get(task_id)
            .ok_or_else(|| format!("model download task {task_id:?} was not found"))?;
        serde_json::to_value(task).map_err(|error| error.to_string())
    }

    fn model_download_pause(&self, task_id: &str) -> Result<serde_json::Value, String> {
        let paused = self.model_download_manager()?.pause(task_id)?;
        Ok(json!({ "taskId": task_id, "paused": paused }))
    }

    fn model_download_resume(&self, task_id: &str) -> Result<serde_json::Value, String> {
        let task = self
            .model_download_manager()?
            .resume(task_id)?
            .ok_or_else(|| format!("model download task {task_id:?} cannot be resumed"))?;
        serde_json::to_value(task).map_err(|error| error.to_string())
    }

    fn model_remove(&self, model_id: &str) -> Result<serde_json::Value, String> {
        self.model_manager()?
            .remove(model_id)
            .map(|removed| json!({ "modelId": model_id, "removed": removed }))
            .map_err(|error| error.to_string())
    }

    fn model_load(&self, model_id: &str, worker: &str) -> Result<serde_json::Value, String> {
        self.model_manager()?
            .load(model_id, worker)
            .map(|_| json!({ "modelId": model_id, "worker": worker, "loaded": true }))
            .map_err(|error| error.to_string())
    }

    fn model_unload(&self, model_id: &str) -> Result<serde_json::Value, String> {
        self.model_manager()?
            .unload(model_id)
            .map(|unloaded| json!({ "modelId": model_id, "unloaded": unloaded }))
            .map_err(|error| error.to_string())
    }

    fn model_cancel(&self, model_id: &str, request_id: &str) -> Result<serde_json::Value, String> {
        self.model_manager()?
            .cancel(model_id, request_id)
            .map(|_| json!({ "modelId": model_id, "requestId": request_id, "cancelled": true }))
            .map_err(|error| error.to_string())
    }

    fn model_generate(
        &self,
        app_id: &str,
        stream_id: &str,
        request: crate::model::GenerateRequest,
    ) -> Result<serde_json::Value, String> {
        let manager = self.model_manager()?.clone();
        let cancellation = self
            .streams
            .open(app_id, stream_id)
            .map_err(|error| error.to_string())?;
        let streams = Arc::clone(&self.streams);
        let stream_id_for_worker = stream_id.to_owned();
        let model_id = request.model.clone();
        let request_id = request.request_id.clone();
        let response_request_id = request.request_id.clone();
        std::thread::Builder::new()
            .name("alex-model-generate".into())
            .spawn(move || {
                let mut emit = |event: crate::model::GenerateEvent| {
                    let payload = serde_json::to_vec(&event)
                        .map_err(|error| crate::model::ModelError::Worker(error.to_string()))?;
                    loop {
                        if cancellation.is_cancelled() {
                            let _ = manager.cancel(&model_id, &request_id);
                            return Err(crate::model::ModelError::Worker(
                                "generation cancelled".into(),
                            ));
                        }
                        match streams.push(&stream_id_for_worker, payload.clone()) {
                            Ok(_) => return Ok(()),
                            Err(crate::runtime::stream::StreamError::Backpressured { .. })
                            | Err(crate::runtime::stream::StreamError::BufferFull { .. }) => {
                                std::thread::sleep(std::time::Duration::from_millis(5));
                            }
                            Err(error) => {
                                return Err(crate::model::ModelError::Worker(error.to_string()));
                            }
                        }
                    }
                };
                let result = manager.generate(&request, &mut emit);
                if cancellation.is_cancelled() {
                    return;
                }
                let terminal = match result {
                    Ok(()) => crate::runtime::stream::StreamTerminal::Completed,
                    Err(error) => crate::runtime::stream::StreamTerminal::Failed {
                        code: "MODEL_GENERATION_FAILED".into(),
                        message: error.to_string(),
                    },
                };
                let _ = streams.finish(&stream_id_for_worker, terminal);
            })
            .map_err(|error| error.to_string())?;
        Ok(json!({ "streamId": stream_id, "requestId": response_request_id }))
    }

    fn model_embed(
        &self,
        request: crate::model::EmbedRequest,
    ) -> Result<serde_json::Value, String> {
        self.model_manager()?
            .embed(&request)
            .and_then(|response| {
                serde_json::to_value(response)
                    .map_err(|error| crate::model::ModelError::Worker(error.to_string()))
            })
            .map_err(|error| error.to_string())
    }

    fn model_providers(&self) -> Result<serde_json::Value, String> {
        let providers = self
            .model_manager()?
            .list_providers()
            .map_err(|error| error.to_string())?;
        Ok(json!({ "providers": providers }))
    }

    fn model_provider_upsert(
        &self,
        config: crate::model::remote::RemoteProviderConfig,
    ) -> Result<serde_json::Value, String> {
        self.model_manager()?
            .upsert_provider(config.clone())
            .map_err(|error| error.to_string())?;
        serde_json::to_value(config).map_err(|error| error.to_string())
    }

    fn model_provider_remove(&self, provider_id: &str) -> Result<serde_json::Value, String> {
        let removed = self
            .model_manager()?
            .remove_provider(provider_id)
            .map_err(|error| error.to_string())?;
        Ok(json!({ "providerId": provider_id, "removed": removed }))
    }

    fn model_provider_health(
        &self,
        provider_id: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let health = self
            .model_manager()?
            .provider_health(provider_id)
            .map_err(|error| error.to_string())?;
        Ok(json!({ "providers": health }))
    }

    fn model_secret_set(
        &self,
        service: &str,
        account: &str,
        secret: &crate::model::remote::SecretValue,
    ) -> Result<serde_json::Value, String> {
        let reference = crate::model::remote::SecretRef {
            service: service.to_string(),
            account: account.to_string(),
        };
        self.model_manager()?
            .secret_set(&reference, secret.as_bytes())
            .map_err(|error| error.to_string())?;
        Ok(json!({ "configured": true }))
    }

    fn model_secret_delete(
        &self,
        service: &str,
        account: &str,
    ) -> Result<serde_json::Value, String> {
        let reference = crate::model::remote::SecretRef {
            service: service.to_string(),
            account: account.to_string(),
        };
        let deleted = self
            .model_manager()?
            .secret_delete(&reference)
            .map_err(|error| error.to_string())?;
        Ok(json!({ "deleted": deleted }))
    }

    fn model_secret_exists(
        &self,
        service: &str,
        account: &str,
    ) -> Result<serde_json::Value, String> {
        let reference = crate::model::remote::SecretRef {
            service: service.to_string(),
            account: account.to_string(),
        };
        let exists = self
            .model_manager()?
            .secret_exists(&reference)
            .map_err(|error| error.to_string())?;
        Ok(json!({ "exists": exists }))
    }

    fn agent_manager(&self) -> Result<&crate::agent::AgentManager, String> {
        self.agents
            .as_ref()
            .ok_or_else(|| "Agent Runtime is not configured".into())
    }

    fn agent_create(
        &self,
        app_id: &str,
        spec: crate::agent::AgentSpec,
        messages: Vec<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        self.agent_manager()?
            .create(app_id, spec, messages)
            .and_then(|run| {
                serde_json::to_value(run)
                    .map_err(|error| crate::agent::AgentError::Invalid(error.to_string()))
            })
            .map_err(|error| error.to_string())
    }

    fn agent_spawn_child(
        &self,
        app_id: &str,
        parent_run_id: &str,
        spec: crate::agent::AgentSpec,
        messages: Vec<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        self.agent_manager()?
            .spawn_child(app_id, parent_run_id, spec, messages)
            .and_then(|run| {
                serde_json::to_value(run)
                    .map_err(|error| crate::agent::AgentError::Invalid(error.to_string()))
            })
            .map_err(|error| error.to_string())
    }

    fn agent_children(
        &self,
        app_id: &str,
        parent_run_id: &str,
    ) -> Result<serde_json::Value, String> {
        self.agent_manager()?
            .children(app_id, parent_run_id)
            .map(|runs| json!({"runs": runs}))
            .map_err(|error| error.to_string())
    }

    fn agent_wait_children(
        &self,
        app_id: &str,
        parent_run_id: &str,
        wait_ms: u32,
        cancel_on_timeout: bool,
    ) -> Result<serde_json::Value, String> {
        self.agent_manager()?
            .wait_children(app_id, parent_run_id, wait_ms, cancel_on_timeout)
            .and_then(|result| {
                serde_json::to_value(result)
                    .map_err(|error| crate::agent::AgentError::Invalid(error.to_string()))
            })
            .map_err(|error| error.to_string())
    }

    fn agent_schedule(
        &self,
        app_id: &str,
        run_id: &str,
        scheduled_at_ms: u64,
    ) -> Result<serde_json::Value, String> {
        self.agent_manager()?
            .schedule(app_id, run_id, scheduled_at_ms)
            .and_then(|run| {
                serde_json::to_value(run)
                    .map_err(|error| crate::agent::AgentError::Invalid(error.to_string()))
            })
            .map_err(|error| error.to_string())
    }

    fn agent_scheduled(&self, app_id: &str) -> Result<serde_json::Value, String> {
        self.agent_manager()?
            .scheduled(app_id)
            .map(|runs| json!({"runs":runs}))
            .map_err(|error| error.to_string())
    }

    fn agent_start(
        &self,
        app_id: &str,
        run_id: &str,
        stream_id: &str,
    ) -> Result<serde_json::Value, String> {
        let manager = self.agent_manager()?.clone();
        manager
            .status(app_id, run_id)
            .map_err(|error| error.to_string())?;
        let cancellation = self
            .streams
            .open(app_id, stream_id)
            .map_err(|error| error.to_string())?;
        let streams = Arc::clone(&self.streams);
        let application = app_id.to_owned();
        let run = run_id.to_owned();
        let worker_stream = stream_id.to_owned();
        std::thread::Builder::new()
            .name("alex-agent-run".into())
            .spawn(move || {
                let mut emit = |event: crate::agent::AgentEvent| {
                    let payload = serde_json::to_vec(&event)
                        .map_err(|error| crate::agent::AgentError::Invalid(error.to_string()))?;
                    push_agent_stream(&streams, &worker_stream, &cancellation, payload)
                };
                let result = manager.execute(&application, &run, &mut emit);
                if cancellation.is_cancelled() {
                    let _ = manager.cancel(&application, &run);
                    return;
                }
                let terminal = match result {
                    Ok(_) => crate::runtime::stream::StreamTerminal::Completed,
                    Err(error) => crate::runtime::stream::StreamTerminal::Failed {
                        code: "AGENT_RUN_FAILED".into(),
                        message: error.to_string(),
                    },
                };
                let _ = streams.finish(&worker_stream, terminal);
            })
            .map_err(|error| error.to_string())?;
        Ok(json!({"runId":run_id,"streamId":stream_id}))
    }

    fn agent_action(
        &self,
        app_id: &str,
        run_id: &str,
        action: &str,
    ) -> Result<serde_json::Value, String> {
        let manager = self.agent_manager()?;
        let result = match action {
            "pause" => manager.pause(app_id, run_id),
            "resume" => manager.resume(app_id, run_id),
            "cancel" => manager.cancel(app_id, run_id),
            "approve" => manager.approve(app_id, run_id),
            "deny" => manager.deny(app_id, run_id),
            _ => return Err("unknown Agent action".into()),
        }
        .map_err(|error| error.to_string())?;
        serde_json::to_value(result).map_err(|error| error.to_string())
    }
    fn agent_status(&self, app_id: &str, run_id: &str) -> Result<serde_json::Value, String> {
        self.agent_manager()?
            .status(app_id, run_id)
            .and_then(|run| {
                serde_json::to_value(run)
                    .map_err(|error| crate::agent::AgentError::Invalid(error.to_string()))
            })
            .map_err(|error| error.to_string())
    }
    fn agent_list(&self, app_id: &str) -> Result<serde_json::Value, String> {
        self.agent_manager()?
            .list(app_id)
            .map(|runs| json!({"runs":runs}))
            .map_err(|error| error.to_string())
    }
    fn agent_history(
        &self,
        app_id: &str,
        run_id: &str,
        limit: usize,
    ) -> Result<serde_json::Value, String> {
        self.agent_manager()?
            .history(app_id, run_id, limit)
            .map(|events| json!({"events":events}))
            .map_err(|error| error.to_string())
    }

    fn agent_timeline(
        &self,
        app_id: &str,
        run_id: &str,
        limit: usize,
    ) -> Result<serde_json::Value, String> {
        self.agent_manager()?
            .timeline(app_id, run_id, limit)
            .map(|entries| json!({ "entries": entries }))
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

fn push_stream_with_cancel(
    streams: &crate::runtime::stream::StreamManager,
    stream_id: &str,
    cancellation: &crate::runtime::stream::CancellationToken,
    payload: Vec<u8>,
) -> Result<(), crate::mcp::McpError> {
    loop {
        if cancellation.is_cancelled() {
            return Err(crate::mcp::McpError::InputRequired(
                "stream cancelled".into(),
            ));
        }
        match streams.push(stream_id, payload.clone()) {
            Ok(_) => return Ok(()),
            Err(crate::runtime::stream::StreamError::Backpressured { .. })
            | Err(crate::runtime::stream::StreamError::BufferFull { .. }) => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(error) => return Err(crate::mcp::McpError::Transport(error.to_string())),
        }
    }
}

fn push_agent_stream(
    streams: &crate::runtime::stream::StreamManager,
    stream_id: &str,
    cancellation: &crate::runtime::stream::CancellationToken,
    payload: Vec<u8>,
) -> Result<(), crate::agent::AgentError> {
    loop {
        if cancellation.is_cancelled() {
            return Err(crate::agent::AgentError::Conflict(
                "agent stream cancelled".into(),
            ));
        }
        match streams.push(stream_id, payload.clone()) {
            Ok(_) => return Ok(()),
            Err(crate::runtime::stream::StreamError::Backpressured { .. })
            | Err(crate::runtime::stream::StreamError::BufferFull { .. }) => {
                std::thread::sleep(std::time::Duration::from_millis(5))
            }
            Err(error) => return Err(crate::agent::AgentError::Conflict(error.to_string())),
        }
    }
}

fn validate_mrtr_response(
    method: &str,
    value: &serde_json::Value,
) -> Result<(), crate::mcp::McpError> {
    let object = value
        .as_object()
        .ok_or_else(|| crate::mcp::McpError::Protocol("MRTR response must be an object".into()))?;
    match method {
        "elicitation/create" => {
            if !object
                .get("action")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|action| matches!(action, "accept" | "decline" | "cancel"))
            {
                return Err(crate::mcp::McpError::Protocol(
                    "elicitation response has an invalid action".into(),
                ));
            }
        }
        "sampling/createMessage" => {
            if !object
                .get("role")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|role| matches!(role, "assistant" | "user"))
                || object.get("content").is_none()
            {
                return Err(crate::mcp::McpError::Protocol(
                    "sampling response omitted role or content".into(),
                ));
            }
        }
        "roots/list" => {
            if !object.get("roots").is_some_and(serde_json::Value::is_array) {
                return Err(crate::mcp::McpError::Protocol(
                    "roots response omitted roots".into(),
                ));
            }
        }
        _ => {
            return Err(crate::mcp::McpError::Protocol(
                "unsupported MRTR response method".into(),
            ));
        }
    }
    Ok(())
}

fn mcp_error_kind(error: &crate::mcp::McpError) -> &'static str {
    match error {
        crate::mcp::McpError::NotFound(_) => "not-found",
        crate::mcp::McpError::Duplicate(_) => "duplicate",
        crate::mcp::McpError::InvalidConfig(_) => "invalid-config",
        crate::mcp::McpError::Transport(_) => "transport",
        crate::mcp::McpError::Protocol(_) => "protocol",
        crate::mcp::McpError::Server { .. } => "server",
        crate::mcp::McpError::Authorization(_) => "authorization",
        crate::mcp::McpError::InputRequired(_) => "input_required",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockMcpTransport;
    impl crate::mcp::RpcTransport for MockMcpTransport {
        fn request(
            &self,
            _id: u64,
            method: &str,
            _params: serde_json::Value,
        ) -> Result<serde_json::Value, crate::mcp::McpError> {
            Ok(match method {
                "tools/list" => json!({"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}),
                "tools/call" => json!({"content":[{"type":"text","text":"ok"}]}),
                _ => json!({}),
            })
        }
        fn notify(
            &self,
            _method: &str,
            _params: serde_json::Value,
        ) -> Result<(), crate::mcp::McpError> {
            Ok(())
        }
    }

    struct MockInferenceWorker;

    struct MockMrtrTransport;
    impl crate::mcp::RpcTransport for MockMrtrTransport {
        fn request(
            &self,
            _id: u64,
            method: &str,
            params: serde_json::Value,
        ) -> Result<serde_json::Value, crate::mcp::McpError> {
            if method != "tools/call" {
                return Ok(json!({}));
            }
            if params.get("inputResponses").is_none() {
                Ok(
                    json!({"resultType":"input_required","inputRequests":{"confirm":{"method":"elicitation/create","params":{"message":"Continue?"}}},"requestState":"state-1"}),
                )
            } else {
                Ok(json!({"resultType":"complete","content":[{"type":"text","text":"done"}]}))
            }
        }
        fn notify(&self, _: &str, _: serde_json::Value) -> Result<(), crate::mcp::McpError> {
            Ok(())
        }
    }
    impl crate::model::InferenceWorker for MockInferenceWorker {
        fn kind(&self) -> &str {
            "mock"
        }
        fn load(
            &self,
            _model: &crate::model::ModelManifest,
            _blob: &std::path::Path,
        ) -> Result<(), crate::model::ModelError> {
            Ok(())
        }
        fn generate(
            &self,
            _request: &crate::model::GenerateRequest,
            emit: &mut dyn FnMut(
                crate::model::GenerateEvent,
            ) -> Result<(), crate::model::ModelError>,
        ) -> Result<(), crate::model::ModelError> {
            emit(crate::model::GenerateEvent::Delta {
                text: "hello".into(),
            })?;
            emit(crate::model::GenerateEvent::Finish {
                reason: "stop".into(),
            })
        }
        fn embed(
            &self,
            request: &crate::model::EmbedRequest,
        ) -> Result<crate::model::EmbeddingResponse, crate::model::ModelError> {
            Ok(crate::model::EmbeddingResponse {
                request_id: request.request_id.clone(),
                model: request.model.clone(),
                embeddings: request
                    .input
                    .iter()
                    .enumerate()
                    .map(|(index, _)| crate::model::Embedding {
                        index,
                        values: vec![1.0],
                    })
                    .collect(),
                usage: crate::model::EmbedUsage {
                    input_tokens: request.input.len() as u64,
                },
            })
        }
        fn cancel(&self, _request_id: &str) -> Result<(), crate::model::ModelError> {
            Ok(())
        }
        fn unload(&self, _model_id: &str) -> Result<(), crate::model::ModelError> {
            Ok(())
        }
    }

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
    fn daemon_owns_mcp_connections_and_tool_calls() {
        let temp = tempfile::tempdir().unwrap();
        let service = DaemonService::new(DaemonStateStore::new(temp.path().join("state.json")))
            .with_ai_root(temp.path())
            .unwrap();
        service
            .mcp
            .connect(
                "com.example.app",
                "tools",
                crate::mcp::McpClient::new(
                    Arc::new(MockMcpTransport),
                    crate::mcp::ProtocolEra::Modern,
                ),
            )
            .unwrap();
        let tools = service.handle(request(ControlCommand::McpListTools {
            app_id: "com.example.app".into(),
            binding: "tools".into(),
            cursor: None,
        }));
        assert!(tools.ok);
        assert_eq!(tools.result.unwrap()["tools"][0]["name"], "echo");
        let arguments = json!({"text":"hello"});
        let issued = service.handle(request(ControlCommand::McpApprovalIssue {
            app_id: "com.example.app".into(),
            binding: "tools".into(),
            name: "echo".into(),
            argument_hash: crate::mcp::audit_argument_hash(&arguments).unwrap(),
        }));
        assert!(issued.ok, "{:?}", issued.error);
        let approval_token = issued.result.unwrap()["approvalToken"]
            .as_str()
            .unwrap()
            .to_owned();
        let called = service.handle(request(ControlCommand::McpCallTool {
            app_id: "com.example.app".into(),
            binding: "tools".into(),
            name: "echo".into(),
            arguments: arguments.clone(),
            approval_token: Some(approval_token.clone()),
        }));
        assert!(called.ok);
        let replay = service.handle(request(ControlCommand::McpCallTool {
            app_id: "com.example.app".into(),
            binding: "tools".into(),
            name: "echo".into(),
            arguments,
            approval_token: Some(approval_token),
        }));
        assert!(
            !replay.ok,
            "an approval token must be consumed exactly once"
        );
        let audit = std::fs::read_to_string(temp.path().join("audit").join("mcp.jsonl")).unwrap();
        let entries = audit
            .lines()
            .map(|line| serde_json::from_str::<crate::mcp::AuditEntry>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].phase, "started");
        assert_eq!(entries[1].outcome.as_deref(), Some("success"));
        assert_eq!(entries[1].tool, "echo");
        assert!(
            !audit.contains("hello"),
            "tool arguments must not be audited"
        );
        let own = service.handle(request(ControlCommand::McpAudit {
            app_id: "com.example.app".into(),
            limit: 20,
        }));
        assert_eq!(own.result.unwrap()["entries"].as_array().unwrap().len(), 2);
        let foreign = service.handle(request(ControlCommand::McpAudit {
            app_id: "com.example.other".into(),
            limit: 20,
        }));
        assert!(
            foreign.result.unwrap()["entries"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn daemon_bridges_mrtr_input_over_credit_stream() {
        let temp = tempfile::tempdir().unwrap();
        let service = DaemonService::new(DaemonStateStore::new(temp.path().join("state.json")));
        service
            .mcp
            .connect(
                "com.example.app",
                "tools",
                crate::mcp::McpClient::new(
                    Arc::new(MockMrtrTransport),
                    crate::mcp::ProtocolEra::Modern,
                ),
            )
            .unwrap();
        let opened = service.handle(request(ControlCommand::McpCallToolInteractive {
            app_id: "com.example.app".into(),
            binding: "tools".into(),
            stream_id: "mrtr-1".into(),
            name: "confirm".into(),
            arguments: json!({}),
            approval_token: None,
            allowed_input_methods: vec!["elicitation/create".into()],
        }));
        assert!(opened.ok, "{:?}", opened.error);
        service.stream_credit("mrtr-1", 64 * 1024).unwrap();
        let first = service
            .streams
            .pop_wait("mrtr-1", std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap();
        let event: serde_json::Value = serde_json::from_slice(&first.data).unwrap();
        assert_eq!(event["type"], "inputRequired");
        let input_id = event["inputId"].as_str().unwrap();
        service
            .mcp_input_respond(
                "com.example.app",
                input_id,
                json!({"action":"accept","content":{"confirmed":true}}),
            )
            .unwrap();
        let second = service
            .streams
            .pop_wait("mrtr-1", std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap();
        let result: serde_json::Value = serde_json::from_slice(&second.data).unwrap();
        assert_eq!(result["type"], "result");
        assert_eq!(result["result"]["content"][0]["text"], "done");
    }

    #[test]
    fn daemon_restores_and_removes_persisted_http_mcp_connection() {
        let temp = tempfile::tempdir().unwrap();
        let configs = crate::mcp::ConnectionConfigStore::open(
            temp.path().join("mcp").join("connections.json"),
        )
        .unwrap();
        configs
            .upsert(crate::mcp::PersistedConnection {
                application: "com.example.app".into(),
                binding: "remote".into(),
                era: crate::mcp::ProtocolEra::Modern,
                transport: crate::mcp::PersistedTransport::StreamableHttp {
                    endpoint: "https://mcp.example.test/v1".into(),
                    token_account: None,
                },
                enabled: true,
                managed_by_manifest: false,
            })
            .unwrap();
        let service = DaemonService::new(DaemonStateStore::new(temp.path().join("state.json")))
            .with_ai_root(temp.path())
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while service.mcp.get("com.example.app", "remote").is_err()
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(service.mcp.get("com.example.app", "remote").is_ok());
        let disconnected = service.handle(request(ControlCommand::McpDisconnect {
            app_id: "com.example.app".into(),
            binding: "remote".into(),
        }));
        assert!(disconnected.ok, "{:?}", disconnected.error);
        assert!(configs.list().unwrap().is_empty());
        assert!(service.mcp.get("com.example.app", "remote").is_err());
    }

    #[test]
    fn daemon_persists_imported_models_under_ai_root() {
        use sha2::{Digest, Sha256};
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("tiny.gguf");
        std::fs::write(&source, b"weights").unwrap();
        let digest: String = Sha256::digest(b"weights")
            .iter()
            .map(|value| format!("{value:02x}"))
            .collect();
        let service = DaemonService::new(DaemonStateStore::new(temp.path().join("state.json")))
            .with_ai_root(temp.path())
            .unwrap();
        let imported = service.handle(request(ControlCommand::ModelImport {
            source: source.to_string_lossy().into_owned(),
            manifest: crate::model::ModelManifest {
                id: "local/tiny@1".into(),
                digest: format!("sha256:{digest}"),
                size_bytes: 0,
                format: "gguf".into(),
                architecture: "llama".into(),
                quantization: None,
                license: None,
                source: None,
                compatible_workers: vec![],
            },
        }));
        assert!(imported.ok, "{:?}", imported.error);
        let listed = service.handle(request(ControlCommand::ModelList));
        assert_eq!(listed.result.unwrap()["models"][0]["id"], "local/tiny@1");
    }

    #[test]
    fn daemon_model_generation_uses_credit_stream() {
        use sha2::{Digest, Sha256};
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("tiny.gguf");
        std::fs::write(&source, b"weights").unwrap();
        let digest: String = Sha256::digest(b"weights")
            .iter()
            .map(|v| format!("{v:02x}"))
            .collect();
        let service = DaemonService::new(DaemonStateStore::new(temp.path().join("state.json")))
            .with_ai_root(temp.path())
            .unwrap();
        let models = service.models.as_ref().unwrap();
        models
            .register_worker(Arc::new(MockInferenceWorker))
            .unwrap();
        models
            .import(
                &source,
                crate::model::ModelManifest {
                    id: "local/tiny@1".into(),
                    digest: format!("sha256:{digest}"),
                    size_bytes: 0,
                    format: "gguf".into(),
                    architecture: "llama".into(),
                    quantization: None,
                    license: None,
                    source: None,
                    compatible_workers: vec!["mock".into()],
                },
            )
            .unwrap();
        models.load("local/tiny@1", "mock").unwrap();
        let opened = service.handle(request(ControlCommand::ModelGenerate {
            app_id: "com.example.app".into(),
            stream_id: "model-stream".into(),
            request: crate::model::GenerateRequest {
                request_id: "generate-1".into(),
                model: "local/tiny@1".into(),
                messages: vec![],
                options: json!({}),
            },
        }));
        assert!(opened.ok);
        assert!(
            service
                .handle(request(ControlCommand::StreamCredit {
                    stream_id: "model-stream".into(),
                    bytes: 4096
                }))
                .ok
        );
        let first = service.handle(request(ControlCommand::StreamRead {
            stream_id: "model-stream".into(),
            wait_ms: 1000,
        }));
        assert!(first.ok);
        let encoded = first.result.unwrap()["dataBase64"]
            .as_str()
            .unwrap()
            .to_owned();
        let event: serde_json::Value = serde_json::from_slice(
            &base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(event, json!({"type":"delta","text":"hello"}));
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
            wait_ms: 0,
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
            wait_ms: 0,
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
                dev: None,
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

    #[test]
    fn native_worker_control_surface_lists_and_bounds_timeout() {
        let temp = tempfile::tempdir().unwrap();
        let service = DaemonService::new(DaemonStateStore::new(temp.path().join("state.json")));
        let status = service.handle(ControlRequest {
            protocol: PROTOCOL_VERSION,
            id: "native-status".into(),
            command: ControlCommand::NativeWorkerStatus {
                app_id: "com.example.app".into(),
            },
        });
        assert!(status.ok);
        assert_eq!(status.result, Some(json!([])));

        let invoke = service.handle(ControlRequest {
            protocol: PROTOCOL_VERSION,
            id: "native-invoke".into(),
            command: ControlCommand::NativeWorkerInvoke {
                app_id: "com.example.app".into(),
                binding: "image".into(),
                method: "image.resize".into(),
                arguments: json!({}),
                timeout_ms: 0,
            },
        });
        assert!(!invoke.ok);
        assert!(invoke.error.unwrap().contains("timeoutMs"));

        let stream = service.handle(ControlRequest {
            protocol: PROTOCOL_VERSION,
            id: "native-stream".into(),
            command: ControlCommand::NativeWorkerInvokeStream {
                app_id: "com.example.app".into(),
                binding: "image".into(),
                method: "image.resize".into(),
                stream_id: "stream-native-1".into(),
                arguments: json!({}),
                timeout_ms: 0,
            },
        });
        assert!(!stream.ok);
        assert!(stream.error.unwrap().contains("timeoutMs"));

        let restart = service.handle(ControlRequest {
            protocol: PROTOCOL_VERSION,
            id: "native-restart".into(),
            command: ControlCommand::NativeWorkerRestart {
                app_id: "com.example.app".into(),
                binding: "image".into(),
            },
        });
        assert!(!restart.ok);
        assert!(
            restart
                .error
                .unwrap()
                .contains("app manager is unavailable")
        );
    }
}

use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::json;

use super::{
    ControlCommand, ControlRequest, ControlResponse, DaemonStateStore, DesiredState,
    PROTOCOL_VERSION,
};

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
        };
        match result {
            Ok(value) => ControlResponse::success(id, value),
            Err(error) => ControlResponse::failure(id, error),
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

    fn start(&self, app_id: &str) -> Result<serde_json::Value, String> {
        if let Some(manager) = &self.manager {
            manager.get_app(app_id).map_err(|error| error.to_string())?;
            let status = manager.launch(app_id).map_err(|error| error.to_string())?;
            self.set_desired(app_id, DesiredState::Running)?;
            return Ok(json!(status));
        }
        self.set_desired(app_id, DesiredState::Running)
    }

    fn stop(&self, app_id: &str) -> Result<serde_json::Value, String> {
        if let Some(manager) = &self.manager {
            manager.get_app(app_id).map_err(|error| error.to_string())?;
            let status = manager.stop(app_id).map_err(|error| error.to_string())?;
            self.set_desired(app_id, DesiredState::Stopped)?;
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
        if service.is_some_and(|name| name != "backend") {
            return Err("manifest v1 only has the backend service".into());
        }
        let manager = self
            .manager
            .as_ref()
            .ok_or_else(|| "log service is not connected yet".to_string())?;
        manager.get_app(app_id).map_err(|error| error.to_string())?;
        let status = manager
            .runtime_status(app_id)
            .map_err(|error| error.to_string())?;
        let limit = usize::try_from(limit.min(10_000)).unwrap_or(10_000);
        let start = status.logs.len().saturating_sub(limit);
        Ok(json!({
            "appId": app_id,
            "service": "backend",
            "lines": &status.logs[start..]
        }))
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
}

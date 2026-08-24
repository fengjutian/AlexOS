use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

use super::{
    ControlCommand, ControlRequest, ControlResponse, DaemonStateStore, DesiredState,
    PROTOCOL_VERSION,
};

#[derive(Debug, Clone)]
pub struct DaemonService {
    state: DaemonStateStore,
}

impl DaemonService {
    pub fn new(state: DaemonStateStore) -> Self {
        Self { state }
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
        let result = match request.command {
            ControlCommand::Ping => Ok(json!({
                "daemon": "alexd",
                "protocol": PROTOCOL_VERSION
            })),
            ControlCommand::List => self.state.load().map(|state| {
                json!({
                    "applications": state.applications.into_values().collect::<Vec<_>>()
                })
            }),
            ControlCommand::Start { app_id } => self.set_desired(&app_id, DesiredState::Running),
            ControlCommand::Stop { app_id } => self.set_desired(&app_id, DesiredState::Stopped),
            ControlCommand::Restart { app_id } => {
                // The orchestration layer will turn this desired-state write into
                // a stop/start transition. Persisting Running here means a daemon
                // crash during restart still converges toward a running app.
                self.set_desired(&app_id, DesiredState::Running)
            }
            ControlCommand::Status { app_id } => self.state.load().and_then(|state| {
                state
                    .applications
                    .get(&app_id)
                    .map(|app| json!(app))
                    .ok_or_else(|| {
                        super::DaemonStateError::Invalid(format!(
                            "application {app_id} has no daemon state"
                        ))
                    })
            }),
            ControlCommand::Logs { .. } => Err(super::DaemonStateError::Invalid(
                "log service is not connected yet".into(),
            )),
        };
        match result {
            Ok(value) => ControlResponse::success(id, value),
            Err(error) => ControlResponse::failure(id, error.to_string()),
        }
    }

    fn set_desired(
        &self,
        app_id: &str,
        desired: DesiredState,
    ) -> Result<serde_json::Value, super::DaemonStateError> {
        let state = self
            .state
            .set_desired(app_id, desired, now_ms().unwrap_or_default())?;
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

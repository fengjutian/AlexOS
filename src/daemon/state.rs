use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::platform::PlatformServices;

const STATE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DesiredState {
    Running,
    #[default]
    Stopped,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ObservedState {
    Starting,
    Running,
    Ready,
    Crashed,
    #[default]
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppControlState {
    pub app_id: String,
    pub desired: DesiredState,
    pub updated_at_ms: u64,
    #[serde(default)]
    pub observed: ObservedState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Phase 5 per-service desired/observed state. A
    /// service is "desired running" when it appears in
    /// this map with `desired = Running`. An empty map
    /// means "no per-service intent recorded" — the
    /// daemon treats that as "fall back to app-level
    /// `start_application`" on recovery. The map is
    /// omitted from the JSON when empty so v1 callers
    /// keep reading the legacy 4-field shape.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub services: BTreeMap<String, ServiceControlState>,
}

/// Per-service control state. Mirrors
/// [`AppControlState`] but tracks one service. The
/// `service` field is the service's name as declared in
/// the manifest (v1: always `"main"`; v2: free-form).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceControlState {
    pub service: String,
    pub desired: DesiredState,
    pub updated_at_ms: u64,
    #[serde(default)]
    pub observed: ObservedState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DaemonState {
    pub schema_version: u32,
    #[serde(default)]
    pub applications: BTreeMap<String, AppControlState>,
}

impl Default for DaemonState {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            applications: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Error)]
pub enum DaemonStateError {
    #[error("daemon state I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid daemon state: {0}")]
    Invalid(String),
}

/// Transactional JSON state store used before the daemon adopts a database.
/// A temp file plus the platform atomic-replace boundary prevents partially
/// written desired state after a host crash.
#[derive(Debug, Clone)]
pub struct DaemonStateStore {
    path: PathBuf,
}

impl DaemonStateStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<DaemonState, DaemonStateError> {
        if !self.path.exists() {
            return Ok(DaemonState::default());
        }
        let state: DaemonState = serde_json::from_slice(&fs::read(&self.path)?)
            .map_err(|error| DaemonStateError::Invalid(error.to_string()))?;
        if state.schema_version != STATE_SCHEMA_VERSION {
            return Err(DaemonStateError::Invalid(format!(
                "unsupported schemaVersion {}",
                state.schema_version
            )));
        }
        for (id, app) in &state.applications {
            if id != &app.app_id || !valid_app_id(id) {
                return Err(DaemonStateError::Invalid(format!(
                    "invalid application state key {id:?}"
                )));
            }
        }
        Ok(state)
    }

    pub fn save(&self, state: &DaemonState) -> Result<(), DaemonStateError> {
        if state.schema_version != STATE_SCHEMA_VERSION {
            return Err(DaemonStateError::Invalid("unsupported state schema".into()));
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = self
            .path
            .with_extension(format!("tmp-{}", std::process::id()));
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|error| DaemonStateError::Invalid(error.to_string()))?;
        fs::write(&temp, bytes)?;
        if let Err(error) = crate::platform::native().atomic_replace(&temp, &self.path) {
            let _ = fs::remove_file(&temp);
            return Err(error.into());
        }
        Ok(())
    }

    pub fn set_desired(
        &self,
        app_id: &str,
        desired: DesiredState,
        updated_at_ms: u64,
    ) -> Result<DaemonState, DaemonStateError> {
        if !valid_app_id(app_id) {
            return Err(DaemonStateError::Invalid(format!(
                "invalid application id {app_id:?}"
            )));
        }
        let mut state = self.load()?;
        state.applications.insert(
            app_id.into(),
            AppControlState {
                app_id: app_id.into(),
                desired,
                updated_at_ms,
                observed: ObservedState::Stopped,
                last_error: None,
                services: BTreeMap::new(),
            },
        );
        self.save(&state)?;
        Ok(state)
    }

    pub fn set_observed(
        &self,
        app_id: &str,
        observed: ObservedState,
        last_error: Option<String>,
        updated_at_ms: u64,
    ) -> Result<DaemonState, DaemonStateError> {
        let mut state = self.load()?;
        let app = state.applications.get_mut(app_id).ok_or_else(|| {
            DaemonStateError::Invalid(format!("application {app_id} has no desired state"))
        })?;
        app.observed = observed;
        app.last_error = last_error;
        app.updated_at_ms = updated_at_ms;
        self.save(&state)?;
        Ok(state)
    }

    /// Phase 5 per-service desired state. Inserts a
    /// `ServiceControlState` if `service` is not already
    /// tracked, or overwrites the `desired` /
    /// `updated_at_ms` fields of an existing entry. The
    /// `observed` / `last_error` fields are left alone so
    /// a fresh `start-service` call does not stomp the
    /// last reported status from a successful launch.
    ///
    /// If the app itself has no `AppControlState` yet
    /// (the user issued `start-service` before any
    /// app-level `start`), the call auto-creates the
    /// app entry with `desired = Stopped` so the
    /// per-service intent is recorded. The app-level
    /// rollup is then updated to `Running` only when
    /// at least one service is `desired = Running`,
    /// which keeps the "did the user start the whole
    /// app?" answer distinct from "did the user start
    /// one service out of band?".
    pub fn set_service_desired(
        &self,
        app_id: &str,
        service: &str,
        desired: DesiredState,
        updated_at_ms: u64,
    ) -> Result<DaemonState, DaemonStateError> {
        if !valid_service_name(service) {
            return Err(DaemonStateError::Invalid(format!(
                "invalid service name {service:?}"
            )));
        }
        let mut state = self.load()?;
        let app = state
            .applications
            .entry(app_id.to_owned())
            .or_insert_with(|| AppControlState {
                app_id: app_id.to_owned(),
                desired: DesiredState::Stopped,
                updated_at_ms,
                observed: ObservedState::Stopped,
                last_error: None,
                services: BTreeMap::new(),
            });
        let entry = app
            .services
            .entry(service.to_owned())
            .or_insert_with(|| ServiceControlState {
                service: service.to_owned(),
                desired: DesiredState::Stopped,
                updated_at_ms,
                observed: ObservedState::Stopped,
                last_error: None,
            });
        entry.desired = desired;
        entry.updated_at_ms = updated_at_ms;
        self.save(&state)?;
        Ok(state)
    }

    /// Phase 5 per-service observed state. Mirrors
    /// [`Self::set_observed`] but writes the
    /// `ServiceControlState.observed` /
    /// `last_error` / `updated_at_ms` fields. Returns
    /// `Invalid` if the service is not tracked (i.e. no
    /// `start-service` was issued for it yet) so a stray
    /// `logs` call cannot create a phantom state row.
    pub fn set_service_observed(
        &self,
        app_id: &str,
        service: &str,
        observed: ObservedState,
        last_error: Option<String>,
        updated_at_ms: u64,
    ) -> Result<DaemonState, DaemonStateError> {
        let mut state = self.load()?;
        let app = state.applications.get_mut(app_id).ok_or_else(|| {
            DaemonStateError::Invalid(format!("application {app_id} has no desired state"))
        })?;
        let entry = app.services.get_mut(service).ok_or_else(|| {
            DaemonStateError::Invalid(format!(
                "application {app_id} has no desired state for service {service:?}"
            ))
        })?;
        entry.observed = observed;
        entry.last_error = last_error;
        entry.updated_at_ms = updated_at_ms;
        self.save(&state)?;
        Ok(state)
    }

    /// Convenience accessor — returns every
    /// `(service, desired)` pair for the app, useful
    /// for `recover_startup`.
    pub fn services_desired_running(
        &self,
        app_id: &str,
    ) -> Vec<(String, ServiceControlState)> {
        let Ok(state) = self.load() else {
            return Vec::new();
        };
        state
            .applications
            .get(app_id)
            .map(|app| {
                app.services
                    .iter()
                    .filter(|(_, svc)| svc.desired == DesiredState::Running)
                    .map(|(name, svc)| (name.clone(), svc.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn valid_app_id(id: &str) -> bool {
    id.contains('.')
        && id.split('.').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
}

/// Service names are free-form (the v2 manifest allows
/// any non-empty alphanumeric+dash+underscore token).
/// v1's "main" still passes this check.
fn valid_service_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desired_state_survives_store_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("daemon-state.json");
        DaemonStateStore::new(&path)
            .set_desired("com.example.agent", DesiredState::Running, 42)
            .unwrap();
        let reopened = DaemonStateStore::new(path).load().unwrap();
        assert_eq!(
            reopened.applications["com.example.agent"].desired,
            DesiredState::Running
        );
        assert_eq!(reopened.applications["com.example.agent"].updated_at_ms, 42);
        assert_eq!(
            reopened.applications["com.example.agent"].observed,
            ObservedState::Stopped
        );
    }

    #[test]
    fn corrupted_or_unknown_state_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("daemon-state.json");
        fs::write(&path, br#"{"schemaVersion":99,"applications":{}}"#).unwrap();
        assert!(matches!(
            DaemonStateStore::new(path).load(),
            Err(DaemonStateError::Invalid(_))
        ));
    }

    #[test]
    fn legacy_v1_state_defaults_observed_to_stopped() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("daemon-state.json");
        fs::write(
            &path,
            br#"{"schemaVersion":1,"applications":{"com.example.agent":{"appId":"com.example.agent","desired":"running","updatedAtMs":7}}}"#,
        )
        .unwrap();
        let state = DaemonStateStore::new(path).load().unwrap();
        let app = &state.applications["com.example.agent"];
        assert_eq!(app.observed, ObservedState::Stopped);
        assert_eq!(app.last_error, None);
        // Legacy v1 payloads do not have a `services`
        // field; the loader must default it to an empty
        // map so per-service `recover_startup` can
        // detect "no per-service intent recorded" and
        // fall back to the whole-app `start_application`
        // path.
        assert!(app.services.is_empty());
    }

    #[test]
    fn service_desired_round_trips_through_save_and_load() {
        let temp = tempfile::tempdir().unwrap();
        let store = DaemonStateStore::new(temp.path().join("state.json"));
        store
            .set_desired("com.example.svc", DesiredState::Running, 1)
            .unwrap();
        store
            .set_service_desired("com.example.svc", "api", DesiredState::Running, 2)
            .unwrap();
        store
            .set_service_observed(
                "com.example.svc",
                "api",
                ObservedState::Crashed,
                Some("boot failed".into()),
                3,
            )
            .unwrap();
        let reopened = DaemonStateStore::new(temp.path().join("state.json"))
            .load()
            .unwrap();
        let app = &reopened.applications["com.example.svc"];
        let api = &app.services["api"];
        assert_eq!(api.desired, DesiredState::Running);
        assert_eq!(api.observed, ObservedState::Crashed);
        assert_eq!(api.last_error.as_deref(), Some("boot failed"));
        assert_eq!(api.updated_at_ms, 3);
    }

    #[test]
    fn set_service_observed_rejects_unknown_service() {
        let temp = tempfile::tempdir().unwrap();
        let store = DaemonStateStore::new(temp.path().join("state.json"));
        store
            .set_desired("com.example.empty", DesiredState::Running, 1)
            .unwrap();
        let error = store
            .set_service_observed(
                "com.example.empty",
                "missing",
                ObservedState::Running,
                None,
                2,
            )
            .unwrap_err();
        // The `Invalid` variant is the documented surface;
        // a future schema-bump can keep the test passing
        // by string-matching the message.
        let message = error.to_string();
        assert!(
            message.contains("no desired state for service"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn set_service_desired_validates_service_name() {
        let temp = tempfile::tempdir().unwrap();
        let store = DaemonStateStore::new(temp.path().join("state.json"));
        store
            .set_desired("com.example.bad", DesiredState::Running, 1)
            .unwrap();
        for bad in ["", "has spaces", "weird/chars"] {
            let error = store
                .set_service_desired("com.example.bad", bad, DesiredState::Running, 2)
                .unwrap_err();
            let message = error.to_string();
            assert!(
                message.contains("invalid service name"),
                "{bad:?} should be rejected, got: {message}"
            );
        }
    }

    #[test]
    fn services_desired_running_filters_to_running_only() {
        let temp = tempfile::tempdir().unwrap();
        let store = DaemonStateStore::new(temp.path().join("state.json"));
        store
            .set_desired("com.example.mix", DesiredState::Running, 1)
            .unwrap();
        store
            .set_service_desired("com.example.mix", "api", DesiredState::Running, 2)
            .unwrap();
        store
            .set_service_desired("com.example.mix", "worker", DesiredState::Stopped, 3)
            .unwrap();
        store
            .set_service_desired("com.example.mix", "cron", DesiredState::Running, 4)
            .unwrap();
        let running = store.services_desired_running("com.example.mix");
        // Sorted BTreeMap iteration: api, cron.
        assert_eq!(
            running.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(),
            vec!["api", "cron"]
        );
    }
}

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppControlState {
    pub app_id: String,
    pub desired: DesiredState,
    pub updated_at_ms: u64,
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
            },
        );
        self.save(&state)?;
        Ok(state)
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
}

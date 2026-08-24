//! Container list / inspect views.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::model::{ContainerState, DesiredState, IsolationLevel, ObservedState};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<ObservedState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desired: Option<DesiredState>,
    #[serde(default = "default_true")]
    pub include_terminal: bool,
}

fn default_true() -> bool {
    true
}

impl ContainerFilter {
    pub fn matches(&self, state: &ContainerState) -> bool {
        if let Some(app_id) = &self.app_id
            && &state.app_id != app_id
        {
            return false;
        }
        if let Some(observed) = self.observed
            && state.observed != observed
        {
            return false;
        }
        if let Some(desired) = self.desired
            && state.desired != desired
        {
            return false;
        }
        if !self.include_terminal && state.observed.is_terminal() {
            return false;
        }
        true
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerView {
    pub instance_id: String,
    pub app_id: String,
    pub app_version: String,
    pub desired: DesiredState,
    pub observed: ObservedState,
    pub isolation_requested: IsolationLevel,
    pub isolation_effective: IsolationLevel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    pub restart_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub generation: u64,
    pub created_at: String,
    pub updated_at: String,
    pub instance_dir: PathBuf,
}

impl ContainerView {
    pub fn from_state(state: &ContainerState, instance_dir: PathBuf) -> Self {
        Self {
            instance_id: state.instance_id.clone(),
            app_id: state.app_id.clone(),
            app_version: state.app_version.to_string(),
            desired: state.desired,
            observed: state.observed,
            isolation_requested: state.isolation_effective,
            isolation_effective: state.isolation_effective,
            degraded_reason: state.degraded_reason.clone(),
            pid: state.pid,
            exit_code: state.exit_code,
            port: state.endpoint.as_ref().map(|e| e.port),
            restart_count: state.restart_count,
            last_error: state.last_error.clone(),
            generation: state.generation,
            created_at: state.created_at.clone(),
            updated_at: state.updated_at.clone(),
            instance_dir,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::model::IsolationLevel;
    use semver::Version;

    fn state(observed: ObservedState) -> ContainerState {
        ContainerState {
            instance_id: "com.example.notes".into(),
            app_id: "com.example.notes".into(),
            app_version: Version::new(1, 0, 0),
            desired: DesiredState::Created,
            observed,
            isolation_effective: IsolationLevel::Job,
            spec: None,
            degraded_reason: None,
            pid: None,
            exit_code: None,
            endpoint: None,
            restart_count: 0,
            last_error: None,
            generation: 1,
            created_at: "2026-08-21T00:00:00Z".into(),
            updated_at: "2026-08-21T00:00:00Z".into(),
        }
    }

    #[test]
    fn filter_matches_by_app_id() {
        let mut f = ContainerFilter::default();
        assert!(f.matches(&state(ObservedState::Ready)));
        f.app_id = Some("com.example.other".into());
        assert!(!f.matches(&state(ObservedState::Ready)));
    }

    #[test]
    fn filter_can_hide_terminal_states() {
        let mut f = ContainerFilter {
            include_terminal: false,
            ..Default::default()
        };
        assert!(!f.matches(&state(ObservedState::Stopped)));
        f.include_terminal = true;
        assert!(f.matches(&state(ObservedState::Stopped)));
    }

    #[test]
    fn view_carries_the_instance_dir_for_log_lookup() {
        let s = state(ObservedState::Ready);
        let view = ContainerView::from_state(&s, PathBuf::from("/alex/c1"));
        assert_eq!(view.instance_dir, PathBuf::from("/alex/c1"));
        assert_eq!(view.app_id, "com.example.notes");
    }
}

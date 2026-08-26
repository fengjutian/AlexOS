//! Headless application entry point (roadmap P0 §0.2).
//!
//! A headless v2 application has no `frontend` block: it declares one
//! or more services and (for agent apps) an `agent` block. The 0.1
//! `alex shell` / `alex dev` entries all assume a WebView; this module
//! is the product entry that starts the same `ApplicationSupervisor`
//! lifecycle without one, so a background / agent app can run exactly
//! like a desktop app minus the UI container.
//!
//! The long-running loop (block until Ctrl+C) stays in the CLI so the
//! library surface is testable without signals.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::{
    agent::AgentSpec,
    core::application_manifest::{ApplicationManifest, ManifestError, load_application},
    runtime::application_supervisor::{
        ApplicationObservedState, ApplicationSupervisor, ApplicationSupervisorError,
    },
};

#[derive(Debug, Error)]
pub enum HeadlessError {
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("headless agent app must not declare a frontend")]
    FrontendDeclared,
    #[error("headless agent app must declare an `agent` block")]
    AgentMissing,
    #[error("headless agent app must declare at least one service")]
    NoServices,
    #[error(transparent)]
    Supervisor(#[from] ApplicationSupervisorError),
}

/// A running headless application. Dropping this value does **not**
/// stop the app; call [`HeadlessRun::stop`] (or run it under the
/// daemon for the long-lived path).
pub struct HeadlessRun {
    pub app_id: String,
    pub agent: AgentSpec,
    pub observed: ApplicationObservedState,
    supervisor: ApplicationSupervisor,
    package_root: PathBuf,
}

impl std::fmt::Debug for HeadlessRun {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HeadlessRun")
            .field("app_id", &self.app_id)
            .field("agent", &self.agent)
            .field("observed", &self.observed)
            .finish_non_exhaustive()
    }
}

impl HeadlessRun {
    /// Stop every service and return the final observed state.
    pub fn stop(&self) -> Result<ApplicationObservedState, HeadlessError> {
        Ok(self.supervisor.stop_application(&self.app_id)?)
    }

    /// The manifest this run was resolved from (callers may want to
    /// print the agent model / tools for observability).
    pub fn manifest(&self) -> Result<ApplicationManifest, HeadlessError> {
        Ok(load_application(&self.package_root)?)
    }
}

/// Validate and start a headless application from `package_root`.
/// Returns once every service has converged to `Healthy` (same
/// layering / rollback semantics as the desktop supervisor).
pub fn start(package_root: &Path) -> Result<HeadlessRun, HeadlessError> {
    let manifest = load_application(package_root)?;
    let resolved = manifest.resolve()?;
    if resolved.frontend.is_some() {
        return Err(HeadlessError::FrontendDeclared);
    }
    let agent = resolved.agent.clone().ok_or(HeadlessError::AgentMissing)?;
    if resolved.services.is_empty() {
        return Err(HeadlessError::NoServices);
    }
    let supervisor = ApplicationSupervisor::new();
    let observed = supervisor.start_application(&resolved.id, package_root, &resolved)?;
    Ok(HeadlessRun {
        app_id: resolved.id.clone(),
        agent,
        observed,
        supervisor,
        package_root: package_root.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_rejects_apps_with_a_frontend() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("app.yaml"),
            r#"
schemaVersion: 2
id: com.example.frontend
name: frontend
version: 1.0.0
runtime: { node: "22" }
frontend: { entry: index.html }
services:
  app: { runtime: node, command: main.js }
agent:
  model: local/test@1
  tools: []
"#,
        )
        .unwrap();
        std::fs::write(temp.path().join("main.js"), "").unwrap();
        std::fs::write(temp.path().join("index.html"), "").unwrap();
        let error = start(temp.path()).unwrap_err();
        assert!(matches!(error, HeadlessError::FrontendDeclared), "{error}");
    }

    #[test]
    fn start_requires_an_agent_block() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("app.yaml"),
            r#"
schemaVersion: 2
id: com.example.noagent
name: noagent
version: 1.0.0
runtime: { node: "22" }
services:
  app: { runtime: node, command: main.js }
"#,
        )
        .unwrap();
        std::fs::write(temp.path().join("main.js"), "").unwrap();
        let error = start(temp.path()).unwrap_err();
        assert!(matches!(error, HeadlessError::AgentMissing), "{error}");
    }
}

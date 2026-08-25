//! E2E for the headless (no-frontend) agent app entry point.
//!
//! The start path exercises the same `ApplicationSupervisor` layering
//! as the desktop shell, minus the WebView. The test spawns a real
//! Node child, so it skips when no Node runtime is discoverable.

use std::path::Path;

fn write_app(root: &Path) {
    std::fs::write(
        root.join("app.yaml"),
        r#"
schemaVersion: 2
id: com.alex.headless-e2e
name: headless-e2e
version: 1.0.0
runtime: { node: "22" }
services:
  worker:
    runtime: node
    command: main.js
agent:
  model: local/test@1
  tools: []
"#,
    )
    .unwrap();
    std::fs::write(root.join("main.js"), "console.log('headless e2e ok');\n").unwrap();
}

#[test]
fn headless_agent_app_starts_and_stops() {
    if alex::runtime::discover_node().is_none() {
        eprintln!("SKIP: node runtime not available on this host");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    write_app(temp.path());

    let run = alex::headless::start(temp.path()).expect("headless app starts");
    assert_eq!(run.app_id, "com.alex.headless-e2e");
    assert_eq!(run.agent.model, "local/test@1");
    assert_eq!(
        run.observed,
        alex::runtime::application_supervisor::ApplicationObservedState::Running,
    );

    let stopped = run.stop().expect("headless app stops");
    assert_eq!(
        stopped,
        alex::runtime::application_supervisor::ApplicationObservedState::Stopped,
    );
}

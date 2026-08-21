use std::fs;

use alex::{
    api::ApiRouter,
    ipc::{self, Request},
    load_app,
    permission::Permission,
};
use serde_json::json;

#[test]
fn loads_the_example_application() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello");
    let app = load_app(&root).expect("example manifest should be valid");
    assert_eq!(app.id, "com.alex.hello");
}

#[test]
fn rejects_entries_that_escape_the_package() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("index.html"), "hello").unwrap();
    fs::write(
        dir.path().join("manifest.json"),
        r#"{
          "schemaVersion": 1,
          "id": "com.alex.escape",
          "name": "Escape",
          "version": "0.1.0",
          "frontend": { "entry": "../index.html" }
        }"#,
    )
    .unwrap();
    let error = load_app(dir.path()).unwrap_err().to_string();
    assert!(error.contains("must stay inside"));
}

#[test]
fn path_permissions_are_scoped() {
    let root = std::path::Path::new("C:/apps/demo");
    let permission = Permission::FilesystemRead {
        paths: vec!["data".into()],
    };
    assert!(permission.allows_path("filesystem.read", root, &root.join("data/note.txt")));
    assert!(!permission.allows_path("filesystem.read", root, &root.join("secrets.txt")));
    assert!(!permission.allows_path("filesystem.write", root, &root.join("data/note.txt")));
}

#[test]
fn ipc_response_has_a_stable_shape() {
    let response = ipc::Response::error("req-1", "PERMISSION_DENIED", "not allowed");
    let value = serde_json::to_value(response).unwrap();
    assert_eq!(value["protocol"], 1);
    assert_eq!(value["error"]["code"], "PERMISSION_DENIED");
    assert!(value.get("result").is_none());
}

#[test]
fn router_reads_only_from_an_allowed_scope() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello");
    let app = load_app(&root).unwrap();
    let router = ApiRouter::new(root, app);
    let allowed = router.dispatch(Request {
        protocol: 1,
        id: "allowed".into(),
        source: "com.alex.hello".into(),
        method: "filesystem.readText".into(),
        params: json!({ "path": "data/message.txt" }),
        deadline_ms: None,
    });
    assert_eq!(
        allowed.result.unwrap()["content"],
        "Hello from the permission-checked Alex API.\n"
    );

    let denied = router.dispatch(Request {
        protocol: 1,
        id: "denied".into(),
        source: "com.alex.hello".into(),
        method: "filesystem.readText".into(),
        params: json!({ "path": "manifest.json" }),
        deadline_ms: None,
    });
    assert_eq!(denied.error.unwrap().code, "PERMISSION_DENIED");
}

#[test]
fn router_rejects_spoofed_package_identity() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello");
    let app = load_app(&root).unwrap();
    let response = ApiRouter::new(root, app).dispatch(Request {
        protocol: 1,
        id: "spoofed".into(),
        source: "com.attacker.app".into(),
        method: "system.info".into(),
        params: json!({}),
        deadline_ms: None,
    });
    assert_eq!(response.error.unwrap().code, "SOURCE_MISMATCH");
}

#[test]
fn router_rejects_oversized_ipc_messages() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello");
    let app = load_app(&root).unwrap();
    let response = ApiRouter::new(root, app).dispatch_json(&"x".repeat(1024 * 1024 + 1));
    assert_eq!(response.error.unwrap().code, "MESSAGE_TOO_LARGE");
}

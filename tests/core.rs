use std::{fs, io::Write};

use alex::{
    api::ApiRouter,
    authorization::{PermissionDecision, PermissionStore},
    ipc::{self, Request},
    load_app, package,
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

#[test]
fn package_round_trip_preserves_a_valid_application() {
    let workspace = tempfile::tempdir().unwrap();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello");
    let archive = workspace.path().join("hello.alex");
    let apps = workspace.path().join("apps");
    package::pack(&source, &archive).unwrap();
    let installed = package::install(&archive, &apps).unwrap();
    let app = load_app(&installed).unwrap();
    assert_eq!(app.id, "com.alex.hello");
    assert!(installed.join("backend/index.js").is_file());
}

#[test]
fn project_scaffolding_creates_a_valid_application() {
    let workspace = tempfile::tempdir().unwrap();
    let project = workspace.path().join("new_app");
    package::create_project(&project, "com.alex.new_app").unwrap();
    assert_eq!(load_app(&project).unwrap().id, "com.alex.new_app");
}

#[test]
fn installed_applications_can_be_listed_and_safely_uninstalled() {
    let workspace = tempfile::tempdir().unwrap();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello");
    let archive = workspace.path().join("hello.alex");
    let apps = workspace.path().join("apps");
    package::pack(&source, &archive).unwrap();
    package::install(&archive, &apps).unwrap();

    let installed = package::list_installed(&apps).unwrap();
    assert_eq!(installed.len(), 1);
    assert_eq!(installed[0].id, "com.alex.hello");

    let removed = package::uninstall("com.alex.hello", &apps).unwrap();
    assert!(!removed.exists());
    assert!(package::list_installed(&apps).unwrap().is_empty());
}

#[test]
fn uninstall_rejects_a_path_like_package_id() {
    let workspace = tempfile::tempdir().unwrap();
    let error = package::uninstall("../outside", workspace.path())
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid package id"));
}

#[test]
fn install_rejects_a_tampered_package() {
    let workspace = tempfile::tempdir().unwrap();
    let archive_path = workspace.path().join("tampered.alex");
    let file = fs::File::create(&archive_path).unwrap();
    let mut archive = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    let manifest = r#"{"schemaVersion":1,"id":"com.alex.tampered","name":"Tampered","version":"0.1.0","frontend":{"entry":"frontend/index.html"}}"#;
    archive.start_file("manifest.json", options).unwrap();
    archive.write_all(manifest.as_bytes()).unwrap();
    archive.start_file("frontend/index.html", options).unwrap();
    archive.write_all(b"<h1>changed</h1>").unwrap();
    archive.start_file(".alex/integrity.json", options).unwrap();
    archive
        .write_all(
            br#"{"algorithm":"sha256","files":{"frontend/index.html":"0000","manifest.json":"0000"}}"#,
        )
        .unwrap();
    archive.finish().unwrap();

    let error = package::install(&archive_path, &workspace.path().join("apps"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("hash mismatch"));
}

#[test]
fn external_urls_require_https_or_http_and_an_allowed_origin() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello");
    let app = load_app(&root).unwrap();
    let router = ApiRouter::new(root, app);
    let invalid = router.dispatch(Request {
        protocol: 1,
        id: "invalid-url".into(),
        source: "com.alex.hello".into(),
        method: "system.openExternal".into(),
        params: json!({ "url": "file:///C:/Windows/System32/cmd.exe" }),
        deadline_ms: None,
    });
    assert_eq!(invalid.error.unwrap().code, "INVALID_PARAMS");

    let denied = router.dispatch(Request {
        protocol: 1,
        id: "denied-origin".into(),
        source: "com.alex.hello".into(),
        method: "system.openExternal".into(),
        params: json!({ "url": "https://example.com/path" }),
        deadline_ms: None,
    });
    assert_eq!(denied.error.unwrap().code, "PERMISSION_DENIED");
}

#[test]
fn signed_packages_verify_against_the_trusted_publisher() {
    let workspace = tempfile::tempdir().unwrap();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello");
    let key = workspace.path().join("publisher.json");
    let archive = workspace.path().join("signed.alex");
    let public_key = package::generate_signing_key(&key).unwrap();
    package::pack_signed(&source, &archive, &key).unwrap();
    let installed = package::install_verified(
        &archive,
        &workspace.path().join("trusted-apps"),
        true,
        Some(&public_key),
    )
    .unwrap();
    assert!(installed.join("manifest.json").is_file());

    let error = package::install_verified(
        &archive,
        &workspace.path().join("untrusted-apps"),
        true,
        Some("not-the-publisher"),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("not trusted"));
}

#[test]
fn signature_required_rejects_unsigned_packages() {
    let workspace = tempfile::tempdir().unwrap();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello");
    let archive = workspace.path().join("unsigned.alex");
    package::pack(&source, &archive).unwrap();
    let error = package::install_verified(&archive, &workspace.path().join("apps"), true, None)
        .unwrap_err()
        .to_string();
    assert!(error.contains("unsigned"));
}

#[test]
fn persisted_permission_revocation_is_enforced_and_audited() {
    let workspace = tempfile::tempdir().unwrap();
    let store = PermissionStore::open_at(workspace.path(), "com.alex.hello").unwrap();
    store
        .set("filesystem.read", PermissionDecision::Denied)
        .unwrap();
    let reopened = PermissionStore::open_at(workspace.path(), "com.alex.hello").unwrap();
    assert_eq!(
        reopened.decision("filesystem.read"),
        PermissionDecision::Denied
    );

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello");
    let app = load_app(&root).unwrap();
    let response = ApiRouter::new(root, app)
        .with_permission_store(reopened)
        .dispatch(Request {
            protocol: 1,
            id: "revoked".into(),
            source: "com.alex.hello".into(),
            method: "filesystem.readText".into(),
            params: json!({ "path": "data/message.txt" }),
            deadline_ms: None,
        });
    assert_eq!(response.error.unwrap().code, "PERMISSION_DENIED");
    assert!(
        workspace
            .path()
            .join("permissions/com.alex.hello.audit.jsonl")
            .is_file()
    );
}

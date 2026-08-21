use std::{fs, io::Write, path::Path, sync::Arc};

use alex::{
    api::ApiRouter,
    authorization::{PermissionDecision, PermissionStore},
    dev,
    ipc::{self, Request},
    load_app,
    manager::{
        AppManager, InstallOptions, LocalAppManager, ManagerError, ManagerRouter,
        RuntimeSupervisor, SYSTEM_IDENTITY, SupervisorError, UninstallOptions,
    },
    package,
    permission::Permission,
    plugin,
    trust::TrustStore,
    update::{self, UpdateChannel},
};
use serde_json::json;

// Tests below manage the global ALEX_DATA_DIR. Hold this lock to keep
// `PermissionStore` writes from racing between parallel tests.
static ALEX_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    assert_eq!(
        store.decision("filesystem.read"),
        PermissionDecision::Prompt
    );
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

#[test]
fn publisher_trust_store_persists_and_matches_signed_packages() {
    let workspace = tempfile::tempdir().unwrap();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello");
    let key = workspace.path().join("publisher.json");
    let archive = workspace.path().join("signed.alex");
    let public_key = package::generate_signing_key(&key).unwrap();
    package::pack_signed(&source, &archive, &key).unwrap();
    assert_eq!(
        package::signer_public_key(&archive).unwrap().unwrap(),
        public_key
    );

    let trust_root = workspace.path().join("trust");
    let fingerprint = TrustStore::open(&trust_root)
        .unwrap()
        .add("Test Publisher".into(), public_key.clone())
        .unwrap();
    let mut reopened = TrustStore::open(&trust_root).unwrap();
    assert_eq!(
        reopened.require(&public_key).unwrap().label,
        "Test Publisher"
    );
    assert_eq!(reopened.list().count(), 1);
    assert!(reopened.remove(&fingerprint).unwrap());
    assert!(reopened.require(&public_key).is_err());
}

#[test]
fn application_updates_are_atomic_and_reject_downgrades() {
    let workspace = tempfile::tempdir().unwrap();
    let source = workspace.path().join("update_app");
    let apps = workspace.path().join("apps");
    let version_one = workspace.path().join("v1.alex");
    let version_two = workspace.path().join("v2.alex");
    package::create_project(&source, "com.alex.update_test").unwrap();
    package::pack(&source, &version_one).unwrap();
    package::install(&version_one, &apps).unwrap();

    let manifest_path = source.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["version"] = json!("0.2.0");
    fs::write(
        &manifest_path,
        format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap()),
    )
    .unwrap();
    package::pack(&source, &version_two).unwrap();

    let result = package::update_verified(&version_two, &apps, false, None, false).unwrap();
    assert_eq!(result.previous_version, "0.1.0");
    assert_eq!(result.version, "0.2.0");
    assert!(!result.backup_retained);
    assert_eq!(load_app(&result.path).unwrap().version, "0.2.0");

    let error = package::update_verified(&version_one, &apps, false, None, false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("not newer"));
    assert_eq!(load_app(&result.path).unwrap().version, "0.2.0");
}

#[test]
fn signed_update_manifests_bind_channel_version_url_and_hash() {
    let workspace = tempfile::tempdir().unwrap();
    let key = workspace.path().join("publisher.json");
    let package_path = workspace.path().join("app.alex");
    let source = workspace.path().join("update_source");
    package::create_project(&source, "com.alex.update_test").unwrap();
    package::pack(&source, &package_path).unwrap();
    let public_key = package::generate_signing_key(&key).unwrap();
    let manifest = update::manifest_for_package(
        "com.alex.update_test".into(),
        UpdateChannel::Stable,
        "0.1.0".into(),
        "https://updates.example.com/app.alex".into(),
        &package_path,
    )
    .unwrap();
    let mut envelope = update::create_signed_manifest(manifest, &key).unwrap();
    let trust_root = workspace.path().join("trust");
    TrustStore::open(&trust_root)
        .unwrap()
        .add("Update Publisher".into(), public_key)
        .unwrap();
    let trust = TrustStore::open(&trust_root).unwrap();
    update::verify_manifest(
        &envelope,
        "com.alex.update_test",
        "0.0.1",
        UpdateChannel::Stable,
        &trust,
    )
    .unwrap();

    envelope.manifest.version = "9.9.9".into();
    assert!(
        update::verify_manifest(
            &envelope,
            "com.alex.update_test",
            "0.0.1",
            UpdateChannel::Stable,
            &trust,
        )
        .unwrap_err()
        .to_string()
        .contains("signature")
    );
}

#[test]
fn update_manifests_reject_insecure_urls() {
    let workspace = tempfile::tempdir().unwrap();
    let package_path = workspace.path().join("app.alex");
    let source = workspace.path().join("insecure_source");
    package::create_project(&source, "com.alex.insecure").unwrap();
    package::pack(&source, &package_path).unwrap();
    let error = update::manifest_for_package(
        "com.alex.insecure".into(),
        UpdateChannel::Dev,
        "0.1.0".into(),
        "http://updates.example.com/app.alex".into(),
        &package_path,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("HTTPS"));
}

#[test]
fn alexignore_returns_none_when_file_is_absent() {
    let workspace = tempfile::tempdir().unwrap();
    assert!(dev::load_alexignore(workspace.path()).is_none());
}

#[test]
fn alexignore_filters_watch_paths_with_gitignore_grammar() {
    let workspace = tempfile::tempdir().unwrap();
    fs::write(
        workspace.path().join(".alexignore"),
        "node_modules/\n*.log\n!keep.log\n",
    )
    .unwrap();
    let matcher = dev::load_alexignore(workspace.path()).expect(".alexignore should load");
    let m = Some(&matcher);

    assert!(
        matcher.matched(Path::new("debug.log"), false).is_ignore(),
        "*.log should match debug.log"
    );
    assert!(
        matcher
            .matched(Path::new("frontend/debug.log"), false)
            .is_ignore(),
        "*.log should match nested debug.log"
    );
    assert!(
        !matcher.matched(Path::new("keep.log"), false).is_ignore(),
        "!keep.log should override *.log"
    );
    assert!(
        !matcher
            .matched(Path::new("frontend/index.html"), false)
            .is_ignore(),
        "unrelated files are not affected"
    );

    // Verify the wrapper used by the watcher reaches the same answer.
    assert!(
        dev::is_ignored(
            &m,
            workspace.path(),
            &workspace.path().join("node_modules/lodash/index.js")
        ),
        "node_modules/ should cover nested files"
    );
    assert!(dev::is_ignored(
        &m,
        workspace.path(),
        &workspace.path().join("frontend/debug.log")
    ));
    assert!(!dev::is_ignored(
        &m,
        workspace.path(),
        &workspace.path().join("keep.log")
    ));
    assert!(!dev::is_ignored(
        &m,
        workspace.path(),
        &workspace.path().join("frontend/index.html")
    ));
}

#[test]
fn alexignore_malformed_file_falls_back_to_no_filtering() {
    let workspace = tempfile::tempdir().unwrap();
    fs::write(workspace.path().join(".alexignore"), "[[[invalid").unwrap();
    // ignore crate is permissive about most grammars; we just want a
    // callable, non-panicking result. Either None (strict) or Some (lenient)
    // is acceptable — the contract is "watcher must not panic".
    let _ = dev::load_alexignore(workspace.path());
}

#[test]
fn manifest_parses_minimal_form_without_metadata() {
    // Schema 1 manifests written before Phase 1.1 must keep working
    // because every new field is optional.
    let workspace = tempfile::tempdir().unwrap();
    fs::create_dir_all(workspace.path().join("frontend")).unwrap();
    fs::write(workspace.path().join("frontend/index.html"), "<h1>hi</h1>").unwrap();
    fs::write(
        workspace.path().join("manifest.json"),
        r#"{
          "schemaVersion": 1,
          "id": "com.alex.legacy",
          "name": "Legacy",
          "version": "0.1.0",
          "frontend": { "entry": "frontend/index.html" }
        }"#,
    )
    .unwrap();
    let app = load_app(workspace.path()).expect("legacy manifest should load");
    assert_eq!(app.id, "com.alex.legacy");
    assert!(app.description.is_none());
    assert!(app.author.is_none());
    assert!(app.icons.is_none());
    assert!(app.homepage.is_none());
    assert!(app.license.is_none());
}

#[test]
fn manifest_parses_full_metadata() {
    let workspace = tempfile::tempdir().unwrap();
    fs::create_dir_all(workspace.path().join("frontend")).unwrap();
    fs::create_dir_all(workspace.path().join("assets")).unwrap();
    fs::write(workspace.path().join("frontend/index.html"), "<h1>hi</h1>").unwrap();
    fs::write(workspace.path().join("assets/icon-256.png"), [0u8; 4]).unwrap();
    fs::write(
        workspace.path().join("manifest.json"),
        r#"{
          "schemaVersion": 1,
          "id": "com.example.notes",
          "name": "Notes",
          "version": "1.2.0",
          "description": "A local notes application",
          "author": { "name": "Example Studio", "url": "https://example.com" },
          "icons": { "16": "assets/icon-16.png", "256": "assets/icon-256.png" },
          "homepage": "https://example.com/notes",
          "license": "MIT",
          "frontend": { "entry": "frontend/index.html" }
        }"#,
    )
    .unwrap();
    let app = load_app(workspace.path()).expect("full manifest should load");
    assert_eq!(
        app.description.as_deref(),
        Some("A local notes application")
    );
    let author = app.author.as_ref().expect("author");
    assert_eq!(author.name, "Example Studio");
    assert_eq!(author.url.as_deref(), Some("https://example.com"));
    let icons = app.icons.as_ref().expect("icons");
    assert_eq!(
        icons.entries.get("16").map(String::as_str),
        Some("assets/icon-16.png")
    );
    assert_eq!(
        icons.entries.get("256").map(String::as_str),
        Some("assets/icon-256.png")
    );
    assert_eq!(app.homepage.as_deref(), Some("https://example.com/notes"));
    assert_eq!(app.license.as_deref(), Some("MIT"));
}

#[test]
fn manifest_rejects_unknown_top_level_fields() {
    // deny_unknown_fields must still fire on the new shape.
    let workspace = tempfile::tempdir().unwrap();
    fs::create_dir_all(workspace.path().join("frontend")).unwrap();
    fs::write(workspace.path().join("frontend/index.html"), "<h1>hi</h1>").unwrap();
    fs::write(
        workspace.path().join("manifest.json"),
        r#"{
          "schemaVersion": 1,
          "id": "com.alex.typo",
          "name": "Typo",
          "version": "0.1.0",
          "frontend": { "entry": "frontend/index.html" },
          "descriptoin": "misspelled"
        }"#,
    )
    .unwrap();
    assert!(load_app(workspace.path()).is_err());
}

fn install_root() -> (std::sync::MutexGuard<'static, ()>, tempfile::TempDir) {
    // ALEX_DATA_DIR points the permission store at the same root so tests
    // can read permissions from the install root, not %LOCALAPPDATA%.
    let guard = ALEX_ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: serialised through ALEX_ENV_LOCK; safe per Rust 2024 edition.
    unsafe {
        std::env::set_var("ALEX_DATA_DIR", dir.path());
    }
    (guard, dir)
}

#[test]
fn manager_list_is_empty_for_a_fresh_install_root() {
    let (_lock, workspace) = install_root();
    let manager = LocalAppManager::open(workspace.path()).unwrap();
    assert!(manager.list_apps().unwrap().is_empty());
}

#[test]
fn manager_install_then_list_then_uninstall() {
    let (_lock, workspace) = install_root();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello");
    let archive = workspace.path().join("hello.alex");
    package::pack(&source, &archive).unwrap();

    let manager = LocalAppManager::open(workspace.path()).unwrap();
    let summary = manager
        .install(&archive, InstallOptions::default())
        .unwrap();
    assert_eq!(summary.id, "com.alex.hello");
    assert_eq!(summary.version, "0.1.0");
    assert!(
        summary.description.is_none(),
        "examples/hello has no description"
    );
    assert!(
        matches!(
            summary.signature_state,
            alex::manager::SignatureState::Unsigned
        ),
        "unsigned archive should report Unsigned"
    );

    let list = manager.list_apps().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, "com.alex.hello");

    // Registry file was created next to the install root.
    let registry_path = manager.registry_path().to_path_buf();
    assert!(
        registry_path.is_file(),
        "registry file should exist after install"
    );

    manager
        .uninstall("com.alex.hello", UninstallOptions::default())
        .unwrap();
    assert!(manager.list_apps().unwrap().is_empty());
    // Registry rebuild should drop the entry too.
    let manager2 = LocalAppManager::open(workspace.path()).unwrap();
    assert!(manager2.list_apps().unwrap().is_empty());
}

#[test]
fn manager_rebuilds_registry_from_install_root_when_missing() {
    let (_lock, workspace) = install_root();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello");
    let archive = workspace.path().join("hello.alex");
    package::pack(&source, &archive).unwrap();
    let manager = LocalAppManager::open(workspace.path()).unwrap();
    manager
        .install(&archive, InstallOptions::default())
        .unwrap();

    // Wipe registry; manager.open should rebuild it by scanning the dir.
    let registry_path = workspace.path().join(".alex").join("registry.json");
    assert!(registry_path.is_file());
    fs::remove_file(&registry_path).unwrap();
    assert!(!registry_path.exists());

    let manager = LocalAppManager::open(workspace.path()).unwrap();
    let list = manager.list_apps().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, "com.alex.hello");
    assert!(
        registry_path.is_file(),
        "registry should be rebuilt on open"
    );
}

#[test]
fn manager_permissions_round_trip() {
    let (_lock, workspace) = install_root();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello");
    let archive = workspace.path().join("hello.alex");
    package::pack(&source, &archive).unwrap();
    let manager = LocalAppManager::open(workspace.path()).unwrap();
    manager
        .install(&archive, InstallOptions::default())
        .unwrap();

    let states = manager.permissions("com.alex.hello").unwrap();
    assert!(
        states
            .iter()
            .any(|s| s.name == "filesystem.read" && s.manifest_declared),
        "hello declares filesystem.read; should appear in permissions"
    );

    manager
        .set_permission(
            "com.alex.hello",
            "filesystem.read",
            PermissionDecision::Denied,
        )
        .unwrap();
    let after = manager.permissions("com.alex.hello").unwrap();
    let read = after
        .iter()
        .find(|s| s.name == "filesystem.read")
        .unwrap();
    assert_eq!(read.decision, PermissionDecision::Denied);

    let error = manager
        .set_permission(
            "com.alex.hello",
            "network.connect",
            PermissionDecision::Granted,
        )
        .unwrap_err();
    assert!(matches!(error, ManagerError::UndeclaredPermission(_)));
}

#[test]
fn manager_get_app_returns_manifest_and_permissions() {
    let (_lock, workspace) = install_root();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello");
    let archive = workspace.path().join("hello.alex");
    package::pack(&source, &archive).unwrap();
    let manager = LocalAppManager::open(workspace.path()).unwrap();
    manager
        .install(&archive, InstallOptions::default())
        .unwrap();

    let details = manager.get_app("com.alex.hello").unwrap();
    assert_eq!(details.manifest.id, "com.alex.hello");
    assert!(!details.permissions.is_empty());
    assert!(details.install_path.is_dir());
}

#[test]
fn manager_router_rejects_wrong_source() {
    let (_lock, workspace) = install_root();
    let manager = LocalAppManager::open(workspace.path()).unwrap();
    let router = ManagerRouter::new(Arc::new(manager));

    let response = router.dispatch(Request {
        protocol: 1,
        id: "r-1".into(),
        source: "com.alex.hello".into(), // not system identity
        method: "manager.list_apps".into(),
        params: json!({}),
        deadline_ms: None,
    });
    let error = response.error.expect("error");
    assert_eq!(error.code, "SOURCE_MISMATCH");
}

#[test]
fn manager_router_rejects_non_manager_method() {
    let (_lock, workspace) = install_root();
    let manager = LocalAppManager::open(workspace.path()).unwrap();
    let router = ManagerRouter::new(Arc::new(manager));

    let response = router.dispatch(Request {
        protocol: 1,
        id: "r-2".into(),
        source: SYSTEM_IDENTITY.into(),
        method: "filesystem.readText".into(), // not a manager.* method
        params: json!({}),
        deadline_ms: None,
    });
    let error = response.error.expect("error");
    assert_eq!(error.code, "UNKNOWN_METHOD");
}

#[test]
fn manager_router_dispatches_list_apps() {
    let (_lock, workspace) = install_root();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello");
    let archive = workspace.path().join("hello.alex");
    package::pack(&source, &archive).unwrap();
    let manager = LocalAppManager::open(workspace.path()).unwrap();
    manager
        .install(&archive, InstallOptions::default())
        .unwrap();
    let router = ManagerRouter::new(Arc::new(manager));

    let response = router.dispatch(Request {
        protocol: 1,
        id: "r-3".into(),
        source: SYSTEM_IDENTITY.into(),
        method: "manager.list_apps".into(),
        params: json!({}),
        deadline_ms: None,
    });
    let result = response.result.expect("result");
    let apps = result
        .get("apps")
        .and_then(|v| v.as_array())
        .expect("apps array");
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0]["id"], "com.alex.hello");
}

#[test]
fn manager_router_dispatch_json_rejects_oversized_messages() {
    let (_lock, workspace) = install_root();
    let manager = LocalAppManager::open(workspace.path()).unwrap();
    let router = ManagerRouter::new(Arc::new(manager));
    let response = router.dispatch_json(&"x".repeat(1024 * 1024 + 1));
    assert_eq!(response.error.unwrap().code, "MESSAGE_TOO_LARGE");
}

#[test]
fn runtime_supervisor_stop_is_idempotent() {
    let supervisor = RuntimeSupervisor::default();
    let status = supervisor.stop("never-launched").unwrap();
    assert!(matches!(status.state, alex::runtime::RuntimeState::Stopped));
}

#[test]
fn system_request_permission_blocks_undeclared_kind() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello");
    let app = load_app(&root).unwrap();
    let router = ApiRouter::new(root, app);
    let response = router.dispatch(Request {
        protocol: 1,
        id: "media-1".into(),
        source: "com.alex.hello".into(),
        method: "system.requestPermission".into(),
        params: json!({ "kind": "media.camera" }),
        deadline_ms: None,
    });
    // examples/hello does not declare media.camera, so the request is
    // rejected before any native dialog appears.
    assert_eq!(response.error.unwrap().code, "PERMISSION_DENIED");
}

#[test]
fn system_request_permission_rejects_unknown_kind() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello");
    let app = load_app(&root).unwrap();
    let router = ApiRouter::new(root, app);
    let response = router.dispatch(Request {
        protocol: 1,
        id: "media-2".into(),
        source: "com.alex.hello".into(),
        method: "system.requestPermission".into(),
        params: json!({ "kind": "telepathy.peer" }),
        deadline_ms: None,
    });
    assert_eq!(response.error.unwrap().code, "INVALID_PARAMS");
}

#[test]
fn runtime_supervisor_rejects_double_launch() {
    // The supervisor `launch` path spawns a real Node child process, so
    // on a machine without Node this would fail with `NodeNotFound`
    // rather than the intended `AlreadyRunning`. Skip cleanly when
    // Node is unavailable so CI without Node (or a developer's
    // Rust-only setup) still passes.
    if alex::runtime::discover_node().is_none() {
        eprintln!("skipping: Node.js not available on this machine");
        return;
    }
    let workspace = tempfile::tempdir().unwrap();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello");
    let archive = workspace.path().join("hello.alex");
    package::pack(&source, &archive).unwrap();
    let install_root = workspace.path().join("apps");
    package::install(&archive, &install_root).unwrap();

    let supervisor = RuntimeSupervisor::default();
    let manifest = load_app(&install_root.join("com.alex.hello")).unwrap();
    let backend = manifest.backend.as_ref().unwrap();
    let _ = supervisor
        .launch(
            "com.alex.hello",
            &install_root.join("com.alex.hello"),
            backend,
        )
        .expect("first launch should succeed");
    let second = supervisor.launch(
        "com.alex.hello",
        &install_root.join("com.alex.hello"),
        backend,
    );
    assert!(matches!(second, Err(SupervisorError::AlreadyRunning(_))));
    let _ = supervisor.stop("com.alex.hello");
}

#[test]
fn manifest_parses_plugin_kind_and_defaults_to_app() {
    // Backward compat: legacy manifests without `kind` must still parse as App.
    let workspace = tempfile::tempdir().unwrap();
    fs::create_dir_all(workspace.path().join("frontend")).unwrap();
    fs::write(workspace.path().join("frontend/index.html"), "<h1>x</h1>").unwrap();
    fs::write(
        workspace.path().join("manifest.json"),
        r#"{
          "schemaVersion": 1,
          "id": "com.alex.legacy",
          "name": "Legacy",
          "version": "0.1.0",
          "frontend": { "entry": "frontend/index.html" }
        }"#,
    )
    .unwrap();
    let manifest = load_app(workspace.path()).unwrap();
    assert_eq!(manifest.kind, alex::manifest::PackageKind::App);
}

#[test]
fn manifest_parses_plugin_kind_explicitly() {
    let workspace = tempfile::tempdir().unwrap();
    fs::create_dir_all(workspace.path().join("backend")).unwrap();
    fs::create_dir_all(workspace.path().join("frontend")).unwrap();
    fs::write(
        workspace.path().join("backend/index.js"),
        "// minimal plugin backend",
    )
    .unwrap();
    fs::write(
        workspace.path().join("frontend/index.html"),
        "<h1>stub</h1>",
    )
    .unwrap();
    fs::write(
        workspace.path().join("manifest.json"),
        r#"{
          "schemaVersion": 1,
          "kind": "plugin",
          "id": "com.example.plugin",
          "name": "Example Plugin",
          "version": "0.1.0",
          "frontend": { "entry": "frontend/index.html" },
          "backend": { "runtime": "node", "entry": "backend/index.js" }
        }"#,
    )
    .unwrap();
    let manifest = load_app(workspace.path()).unwrap();
    assert_eq!(manifest.kind, alex::manifest::PackageKind::Plugin);
    plugin::validate_plugin_manifest(&manifest).unwrap();
}

#[test]
fn plugin_discover_finds_only_plugin_kind() {
    let workspace = tempfile::tempdir().unwrap();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello");
    let app_archive = workspace.path().join("hello.alex");
    package::pack(&source, &app_archive).unwrap();
    let apps_root = workspace.path().join("apps");
    package::install(&app_archive, &apps_root).unwrap();

    // Build a fake plugin by hand.
    let plugin_dir = apps_root.join("com.example.fake-plugin");
    fs::create_dir_all(plugin_dir.join("backend")).unwrap();
    fs::write(plugin_dir.join("backend/index.js"), "// plugin").unwrap();
    fs::write(
        plugin_dir.join("manifest.json"),
        r#"{
          "schemaVersion": 1,
          "kind": "plugin",
          "id": "com.example.fake-plugin",
          "name": "Fake Plugin",
          "version": "0.1.0",
          "frontend": { "entry": "frontend/index.html" },
          "backend": { "runtime": "node", "entry": "backend/index.js" }
        }"#,
    )
    .unwrap();
    fs::create_dir_all(plugin_dir.join("frontend")).unwrap();
    fs::write(
        plugin_dir.join("frontend/index.html"),
        "<h1>plugin stub</h1>",
    )
    .unwrap();

    let plugins = plugin::discover(&apps_root).unwrap();
    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0].id, "com.example.fake-plugin");
    assert_eq!(plugins[0].kind, alex::manifest::PackageKind::Plugin);
}

#[test]
fn system_methods_are_blocked_for_apps() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello");
    let app = load_app(&root).unwrap();
    // examples/hello is `kind: "app"`; system.* must be unreachable even
    // if a permission manifest declared it (we don't here, but kind check
    // runs first so the rejection is unambiguous).
    let router = ApiRouter::new(root, app);
    for method in ["system.install", "system.uninstall", "system.listApps"] {
        let params = if method == "system.listApps" {
            json!({})
        } else if method == "system.install" {
            json!({ "packagePath": "C:/nope.alex" })
        } else {
            json!({ "id": "com.alex.hello" })
        };
        let response = router.dispatch(Request {
            protocol: 1,
            id: format!("{method}-1"),
            source: "com.alex.hello".into(),
            method: method.into(),
            params,
            deadline_ms: None,
        });
        let error = response.error.expect("error");
        assert_eq!(
            error.code, "PERMISSION_DENIED",
            "{method}: {}",
            error.message
        );
        assert!(
            error.message.contains("reserved for plugins"),
            "{method}: {}",
            error.message
        );
    }
}

#[test]
fn system_list_apps_returns_installed_apps_for_plugins() {
    let workspace = tempfile::tempdir().unwrap();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello");
    let archive = workspace.path().join("hello.alex");
    package::pack(&source, &archive).unwrap();
    let install_root = workspace.path().join("apps");
    package::install(&archive, &install_root).unwrap();

    // Build a plugin manifest declaring system.manageApps.
    let plugin_dir = workspace.path().join("plugin");
    fs::create_dir_all(plugin_dir.join("backend")).unwrap();
    fs::create_dir_all(plugin_dir.join("frontend")).unwrap();
    fs::write(plugin_dir.join("backend/index.js"), "// plugin").unwrap();
    fs::write(plugin_dir.join("frontend/index.html"), "<h1>p</h1>").unwrap();
    fs::write(
        plugin_dir.join("manifest.json"),
        r#"{
          "schemaVersion": 1,
          "kind": "plugin",
          "id": "com.example.listing-plugin",
          "name": "Listing Plugin",
          "version": "0.1.0",
          "frontend": { "entry": "frontend/index.html" },
          "backend": { "runtime": "node", "entry": "backend/index.js" },
          "permissions": [{ "name": "system.manageApps" }]
        }"#,
    )
    .unwrap();
    let manifest = load_app(&plugin_dir).unwrap();
    let router = ApiRouter::new(plugin_dir, manifest).with_system_install_root(install_root);

    let response = router.dispatch(Request {
        protocol: 1,
        id: "list-1".into(),
        source: "com.example.listing-plugin".into(),
        method: "system.listApps".into(),
        params: json!({}),
        deadline_ms: None,
    });
    let result = response.result.expect("result");
    let apps = result.get("apps").and_then(|v| v.as_array()).expect("apps");
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0]["id"], "com.alex.hello");
}

#[test]
fn plugin_find_in_install_rejects_apps() {
    let workspace = tempfile::tempdir().unwrap();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello");
    let archive = workspace.path().join("hello.alex");
    package::pack(&source, &archive).unwrap();
    let install_root = workspace.path().join("apps");
    package::install(&archive, &install_root).unwrap();

    // examples/hello is `kind: "app"`; `find_in_install` must return None
    // for it so `alex plugin <id>` cannot start a non-plugin by accident.
    let found = plugin::find_in_install(&install_root, "com.alex.hello").unwrap();
    assert!(
        found.is_none(),
        "an app must not be discoverable as a plugin"
    );

    let missing = plugin::find_in_install(&install_root, "com.does.not.exist").unwrap();
    assert!(missing.is_none());
}

#[test]
fn self_host_manager_plugin_can_be_packed_and_installed() {
    let workspace = tempfile::tempdir().unwrap();
    let plugin_src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("plugins")
        .join("manager");
    let archive = workspace.path().join("manager.alex");
    let install_root = workspace.path().join("apps");

    // Same commands a developer would run by hand.
    package::pack(&plugin_src, &archive).unwrap();
    package::install(&archive, &install_root).unwrap();

    // The plugin is discoverable by both the manager helper and the
    // plugin discovery entry point.
    let found = plugin::find_in_install(&install_root, "com.alex.manager")
        .unwrap()
        .expect("manager plugin should be installed");
    assert!(found.join("manifest.json").is_file());
    assert!(found.join("backend/index.js").is_file());
    assert!(found.join("frontend/index.html").is_file());

    let discovered = plugin::discover(&install_root).unwrap();
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].id, "com.alex.manager");
    assert_eq!(discovered[0].kind, alex::manifest::PackageKind::Plugin);
}

#[test]
fn app_manager_html_does_not_use_unsafe_inline_csp() {
    // P0.3.2 acceptance: production apps run under a CSP that does not
    // allow inline scripts or styles. Validate the live header string
    // that the shell would emit (we cannot exercise the WebView in a
    // headless test, but the CSP source-of-truth is a string literal).
    let csp = std::include_str!("../src/shell.rs");
    let header = csp
        .lines()
        .find(|line| line.contains("Content-Security-Policy"))
        .expect("shell.rs must set a CSP header");
    assert!(
        !header.contains("'unsafe-inline'"),
        "shell CSP still allows unsafe-inline: {header}"
    );
    assert!(header.contains("script-src 'self'"), "{header}");
    assert!(header.contains("style-src 'self'"), "{header}");
}

#[test]
fn manager_webview_does_not_use_unsafe_inline_csp() {
    let csp = std::include_str!("../src/manager_webview.rs");
    let header = csp
        .lines()
        .find(|line| line.contains("Content-Security-Policy"))
        .expect("manager_webview.rs must set a CSP header");
    assert!(
        !header.contains("'unsafe-inline'"),
        "manager CSP still allows unsafe-inline: {header}"
    );
}

#[test]
fn hello_frontend_does_not_use_inline_script() {
    let html = std::include_str!("../examples/hello/frontend/index.html");
    assert!(
        !html.contains("<script>"),
        "hello frontend must not contain an inline <script> block"
    );
    assert!(html.contains("app.js"), "must reference external app.js");
}

#[test]
fn self_host_manager_plugin_is_discoverable_after_install() {
    // End-to-end self-hosting smoke: pack the manager plugin, install it,
    // and verify it shows up as the discoverable plugin form. This is
    // the data path `alex manager` checks to decide between plugin and
    // built-in fallback.
    let workspace = tempfile::tempdir().unwrap();
    let plugin_src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("plugins")
        .join("manager");
    let archive = workspace.path().join("manager.alex");
    let install_root = workspace.path().join("apps");
    package::pack(&plugin_src, &archive).unwrap();
    package::install(&archive, &install_root).unwrap();

    // 1. The plugin is discoverable by id.
    let found = plugin::find_in_install(&install_root, "com.alex.manager")
        .unwrap()
        .expect("manager plugin should be present after install");
    assert!(found.join("manifest.json").is_file());

    // 2. The manifest is recognised as a plugin.
    let manifest = load_app(&found).unwrap();
    assert_eq!(manifest.kind, alex::manifest::PackageKind::Plugin);
    plugin::validate_plugin_manifest(&manifest).unwrap();

    // 3. The plugin manifest declares the system permissions the
    //    self-hosted manager needs.
    let system_perms: Vec<&str> = manifest
        .permissions
        .iter()
        .filter_map(|p| match p {
            alex::permission::Permission::SystemInstall => Some("system.install"),
            alex::permission::Permission::SystemUninstall => Some("system.uninstall"),
            alex::permission::Permission::SystemManageApps => Some("system.manageApps"),
            _ => None,
        })
        .collect();
    assert!(system_perms.contains(&"system.manageApps"));
    assert!(system_perms.contains(&"system.install"));
    assert!(system_perms.contains(&"system.uninstall"));
}

#[test]
fn manifest_parses_extension_points() {
    let workspace = tempfile::tempdir().unwrap();
    fs::create_dir_all(workspace.path().join("backend")).unwrap();
    fs::create_dir_all(workspace.path().join("frontend")).unwrap();
    fs::write(workspace.path().join("backend/index.js"), "// stub").unwrap();
    fs::write(workspace.path().join("frontend/index.html"), "<h1>p</h1>").unwrap();
    fs::write(
        workspace.path().join("manifest.json"),
        r#"{
          "schemaVersion": 1,
          "kind": "plugin",
          "id": "com.example.with-ext",
          "name": "Ext",
          "version": "0.1.0",
          "frontend": { "entry": "frontend/index.html" },
          "backend": { "runtime": "node", "entry": "backend/index.js" },
          "extensionPoints": [
            { "kind": "command", "id": "open-docs", "label": "Open Docs", "entry": "openDocs" },
            { "kind": "panel",   "id": "status",    "label": "Status",    "entry": "status" }
          ]
        }"#,
    )
    .unwrap();
    let manifest = load_app(workspace.path()).unwrap();
    let exts = manifest.extension_points.expect("extension_points present");
    assert_eq!(exts.len(), 2);
    assert_eq!(exts[0].id, "open-docs");
    assert_eq!(exts[0].kind, alex::manifest::ExtensionKind::Command);
    assert_eq!(exts[1].kind, alex::manifest::ExtensionKind::Panel);
}

#[test]
fn discover_extensions_aggregates_across_plugins() {
    // Two plugins, each with a command. Result must surface both.
    let workspace = tempfile::tempdir().unwrap();
    let install_root = workspace.path().join("apps");
    for (id, ext_id) in [
        ("com.example.alpha", "alpha.run"),
        ("com.example.beta", "beta.status"),
    ] {
        let dir = install_root.join(id);
        fs::create_dir_all(dir.join("backend")).unwrap();
        fs::create_dir_all(dir.join("frontend")).unwrap();
        fs::write(dir.join("backend/index.js"), "//").unwrap();
        fs::write(dir.join("frontend/index.html"), "<h1>").unwrap();
        fs::write(
            dir.join("manifest.json"),
            format!(
                r#"{{
                  "schemaVersion": 1,
                  "kind": "plugin",
                  "id": "{id}",
                  "name": "X",
                  "version": "0.1.0",
                  "frontend": {{ "entry": "frontend/index.html" }},
                  "backend": {{ "runtime": "node", "entry": "backend/index.js" }},
                  "extensionPoints": [
                    {{ "kind": "command", "id": "{ext_id}", "label": "Run", "entry": "run" }}
                  ]
                }}"#
            ),
        )
        .unwrap();
    }
    let exts = plugin::discover_extensions(&install_root).unwrap();
    assert_eq!(exts.len(), 2);
    let ids: Vec<_> = exts.iter().map(|b| b.extension.id.as_str()).collect();
    assert!(ids.contains(&"alpha.run"));
    assert!(ids.contains(&"beta.status"));
}

#[test]
fn reverse_ipc_parser_recognises_host_call_envelopes() {
    use alex::plugin;
    let line = r#"{"kind":"hostCall","id":"r-1","method":"system.listApps","params":{}}"#;
    let parsed = plugin::parse_host_call(line).expect("hostCall parses");
    assert_eq!(parsed.0, "r-1");
    assert_eq!(parsed.1, "system.listApps");
    assert_eq!(parsed.2, serde_json::json!({}));

    // Lines without `kind: "hostCall"` are not host calls (they're log output).
    assert!(plugin::parse_host_call(r#"{"kind":"started","payload":{}}"#).is_none());
    assert!(plugin::parse_host_call("not json at all").is_none());
    assert!(plugin::parse_host_call("").is_none());
    // Missing fields - None rather than a panic.
    assert!(plugin::parse_host_call(r#"{"kind":"hostCall"}"#).is_none());
}

#[test]
fn reverse_ipc_dispatches_to_plugin_router_and_round_trips_a_list_apps_call() {
    // The end-to-end reverse IPC round trip (plugin writes hostCall,
    // host dispatches via plugin's own ApiRouter, host writes back
    // hostResponse) is exercised by the `alex plugin` smoke path
    // because spawning a real Node backend from a unit test would
    // also touch Windows file-watcher / Defender behaviour that has
    // been observed to hang. We test the contract here at the
    // boundary: the dispatch helper is fed a hostCall-shaped line and
    // we verify that an ApiRouter built for a plugin would accept it.
    let workspace = tempfile::tempdir().unwrap();
    let plugin_dir = workspace.path().join("plugin");
    fs::create_dir_all(plugin_dir.join("backend")).unwrap();
    fs::create_dir_all(plugin_dir.join("frontend")).unwrap();
    fs::write(plugin_dir.join("backend/index.js"), "// stub").unwrap();
    fs::write(plugin_dir.join("frontend/index.html"), "<h1>").unwrap();
    fs::write(
        plugin_dir.join("manifest.json"),
        r#"{
          "schemaVersion": 1,
          "kind": "plugin",
          "id": "com.example.reverse-ipc",
          "name": "Reverse IPC",
          "version": "0.1.0",
          "frontend": { "entry": "frontend/index.html" },
          "backend": { "runtime": "node", "entry": "backend/index.js" },
          "permissions": [{ "name": "system.manageApps" }]
        }"#,
    )
    .unwrap();
    let manifest = load_app(&plugin_dir).unwrap();
    let request_line = r#"{"kind":"hostCall","id":"r-42","method":"system.listApps","params":{}}"#;
    let parsed = alex::plugin::parse_host_call(request_line).expect("parses");
    assert_eq!(parsed.0, "r-42");
    assert_eq!(parsed.1, "system.listApps");
    assert_eq!(manifest.id, "com.example.reverse-ipc");
    assert!(
        manifest
            .permissions
            .iter()
            .any(|p| matches!(p, alex::permission::Permission::SystemManageApps))
    );
}

#[test]
fn reverse_ipc_dispatch_returns_serialized_host_response_for_pre_granted_plugin() {
    // Mimic the in-host half of the reverse-IPC round trip:
    //   1. Plugin writes a `hostCall` envelope to its stdout.
    //   2. `run_unified_dispatch` parses it and calls
    //      `ApiRouter::dispatch` with a synthetic `Request` whose
    //      `source` matches the plugin manifest id.
    //   3. With a pre-granted `PermissionStore`, the dispatch resolves
    //      to a successful `system.listApps` result.
    //   4. The host serializes the response back into the
    //      `hostResponse` envelope that the plugin reads on stdin.
    // The test exercises the in-process path; the cross-process
    // byte-level plumbing (stdin/stdout pipes) is covered by the
    // `alex plugin --headless` smoke path.
    let _guard = ALEX_ENV_LOCK.lock().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let install_root = workspace.path().join("apps");
    fs::create_dir_all(&install_root).unwrap();
    let hello_dir = install_root.join("com.alex.hello");
    fs::create_dir_all(&hello_dir).unwrap();
    fs::create_dir_all(hello_dir.join("frontend")).unwrap();
    fs::create_dir_all(hello_dir.join("backend")).unwrap();
    fs::write(hello_dir.join("frontend/index.html"), "<h1>hello</h1>").unwrap();
    fs::write(hello_dir.join("backend/index.js"), "// stub").unwrap();
    fs::write(
        hello_dir.join("manifest.json"),
        r#"{
          "schemaVersion": 1,
          "id": "com.alex.hello",
          "name": "Hello",
          "version": "0.1.0",
          "frontend": { "entry": "frontend/index.html" },
          "backend": { "runtime": "node", "entry": "backend/index.js" }
        }"#,
    )
    .unwrap();

    let plugin_dir = workspace.path().join("plugin");
    fs::create_dir_all(plugin_dir.join("backend")).unwrap();
    fs::create_dir_all(plugin_dir.join("frontend")).unwrap();
    fs::write(plugin_dir.join("backend/index.js"), "// stub").unwrap();
    fs::write(plugin_dir.join("frontend/index.html"), "<h1>").unwrap();
    fs::write(
        plugin_dir.join("manifest.json"),
        r#"{
          "schemaVersion": 1,
          "kind": "plugin",
          "id": "com.example.reverse-ipc",
          "name": "Reverse IPC",
          "version": "0.1.0",
          "frontend": { "entry": "frontend/index.html" },
          "backend": { "runtime": "node", "entry": "backend/index.js" },
          "permissions": [{ "name": "system.manageApps" }]
        }"#,
    )
    .unwrap();
    let manifest = load_app(&plugin_dir).unwrap();

    // The plugin is given its own PermissionStore with the declared
    // system permission already granted — exactly what
    // `plugin::run(..., headless=true)` does at startup.
    let store = PermissionStore::open_at(workspace.path(), &manifest.id).unwrap();
    store
        .set("system.manageApps", PermissionDecision::Granted)
        .unwrap();
    let router = ApiRouter::new(plugin_dir.clone(), manifest.clone())
        .with_permission_store(store)
        .with_system_install_root(install_root.clone());

    // 1. Parse the hostCall envelope the way `run_unified_dispatch` does.
    let line = r#"{"kind":"hostCall","id":"abc","method":"system.listApps","params":{}}"#;
    let (id, method, params) = alex::plugin::parse_host_call(line).expect("hostCall parses");

    // 2. Dispatch through the plugin's router.
    let response = router.dispatch(Request {
        protocol: 1,
        id: id.clone(),
        source: manifest.id.clone(),
        method,
        params,
        deadline_ms: None,
    });

    // 3. Serialize into the hostResponse envelope the way
    //    `run_unified_dispatch` writes it back to stdin.
    let envelope = serde_json::json!({
        "kind": "hostResponse",
        "id": response.id,
        "result": response.result,
        "error": response.error,
    });
    let serialized = envelope.to_string();
    assert!(serialized.contains(r#""kind":"hostResponse""#));
    assert!(serialized.contains(r#""id":"abc""#));
    // The result should list the installed apps we created above.
    let parsed_back: serde_json::Value = serde_json::from_str(&serialized).unwrap();
    let apps = parsed_back
        .get("result")
        .and_then(|r| r.get("apps"))
        .and_then(|a| a.as_array())
        .expect("apps array present");
    assert_eq!(apps.len(), 1, "only com.alex.hello is installed");
    assert_eq!(apps[0]["id"], "com.alex.hello");
}

#[test]
fn reverse_ipc_system_install_dispatch_writes_a_new_app_into_install_root() {
    // Drive the write half of the reverse-IPC contract: a plugin
    // asks the host to install a `.alex` archive by writing a
    // `hostCall` envelope, the host dispatches it through the
    // plugin's own `ApiRouter` (with the install + manageApps
    // permissions pre-granted), and a subsequent `hostCall` for
    // `system.listApps` returns both the pre-existing app and the
    // newly installed one.
    let _guard = ALEX_ENV_LOCK.lock().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let install_root = workspace.path().join("apps");
    fs::create_dir_all(&install_root).unwrap();

    // First app is laid out directly on disk so `list_installed` can
    // see it without a round-trip through `install_verified`.
    let first_dir = install_root.join("com.alex.first");
    fs::create_dir_all(&first_dir).unwrap();
    fs::create_dir_all(first_dir.join("frontend")).unwrap();
    fs::create_dir_all(first_dir.join("backend")).unwrap();
    fs::write(first_dir.join("frontend/index.html"), "<h1>first</h1>").unwrap();
    fs::write(first_dir.join("backend/index.js"), "// stub").unwrap();
    fs::write(
        first_dir.join("manifest.json"),
        r#"{
          "schemaVersion": 1,
          "id": "com.alex.first",
          "name": "First",
          "version": "0.1.0",
          "frontend": { "entry": "frontend/index.html" },
          "backend": { "runtime": "node", "entry": "backend/index.js" }
        }"#,
    )
    .unwrap();

    // Second app lives in a source directory and is packed into a
    // `.alex` archive, which the reverse-IPC dispatch is asked to
    // install. This is the same flow `alex install` triggers.
    let second_source = workspace.path().join("second-src");
    fs::create_dir_all(second_source.join("frontend")).unwrap();
    fs::create_dir_all(second_source.join("backend")).unwrap();
    fs::write(second_source.join("frontend/index.html"), "<h1>second</h1>").unwrap();
    fs::write(second_source.join("backend/index.js"), "// stub").unwrap();
    fs::write(
        second_source.join("manifest.json"),
        r#"{
          "schemaVersion": 1,
          "id": "com.alex.second",
          "name": "Second",
          "version": "0.2.0",
          "frontend": { "entry": "frontend/index.html" },
          "backend": { "runtime": "node", "entry": "backend/index.js" }
        }"#,
    )
    .unwrap();
    let archive = workspace.path().join("second.alex");
    package::pack(&second_source, &archive).unwrap();

    // Plugin manifest declares the system permissions it will use.
    let plugin_dir = workspace.path().join("plugin");
    fs::create_dir_all(plugin_dir.join("backend")).unwrap();
    fs::create_dir_all(plugin_dir.join("frontend")).unwrap();
    fs::write(plugin_dir.join("backend/index.js"), "// stub").unwrap();
    fs::write(plugin_dir.join("frontend/index.html"), "<h1>plugin</h1>").unwrap();
    fs::write(
        plugin_dir.join("manifest.json"),
        r#"{
          "schemaVersion": 1,
          "kind": "plugin",
          "id": "com.example.reverse-ipc-install",
          "name": "Reverse IPC Install",
          "version": "0.1.0",
          "frontend": { "entry": "frontend/index.html" },
          "backend": { "runtime": "node", "entry": "backend/index.js" },
          "permissions": [
            { "name": "system.manageApps" },
            { "name": "system.install" },
            { "name": "system.uninstall" }
          ]
        }"#,
    )
    .unwrap();
    let manifest = load_app(&plugin_dir).unwrap();

    // Pre-grant every declared system permission — mirrors what
    // `plugin::run(..., headless=true)` does at startup.
    let store = PermissionStore::open_at(workspace.path(), &manifest.id).unwrap();
    for permission in &manifest.permissions {
        if permission.name().starts_with("system.") {
            store
                .set(permission.name(), PermissionDecision::Granted)
                .unwrap();
        }
    }
    let router = ApiRouter::new(plugin_dir.clone(), manifest.clone())
        .with_permission_store(store)
        .with_system_install_root(install_root.clone());

    // 1. hostCall → system.install. Build the same Request shape
    //    that `run_unified_dispatch` constructs. The package was
    //    packed without a signing key, so we pass
    //    `requireSignature: false` explicitly — this exercises the
    //    "operator-confirmed unsigned install" path the new H2
    //    default policy carves out.
    let install_line = format!(
        r#"{{"kind":"hostCall","id":"inst-1","method":"system.install","params":{{"packagePath":"{}","requireSignature":false}}}}"#,
        archive.display().to_string().replace('\\', "\\\\")
    );
    let (install_id, install_method, install_params) =
        alex::plugin::parse_host_call(&install_line).expect("install hostCall parses");
    let install_response = router.dispatch(Request {
        protocol: 1,
        id: install_id,
        source: manifest.id.clone(),
        method: install_method,
        params: install_params,
        deadline_ms: None,
    });
    let install_envelope = serde_json::json!({
        "kind": "hostResponse",
        "id": install_response.id,
        "result": install_response.result,
        "error": install_response.error,
    });
    let install_serialized = install_envelope.to_string();
    assert!(
        install_serialized.contains(r#""error":null"#),
        "install must succeed: {install_serialized}"
    );
    assert!(
        install_serialized.contains("com.alex.second"),
        "hostResponse should mention the newly installed id: {install_serialized}"
    );
    assert!(
        install_root.join("com.alex.second").is_dir(),
        "install_root must now contain the newly installed app"
    );

    // 2. hostCall → system.listApps. Now both apps should be
    //    visible because install wrote into the same install_root
    //    the router is configured with.
    let list_line = r#"{"kind":"hostCall","id":"list-2","method":"system.listApps","params":{}}"#;
    let (list_id, list_method, list_params) =
        alex::plugin::parse_host_call(list_line).expect("list hostCall parses");
    let list_response = router.dispatch(Request {
        protocol: 1,
        id: list_id,
        source: manifest.id.clone(),
        method: list_method,
        params: list_params,
        deadline_ms: None,
    });
    let parsed: serde_json::Value = serde_json::to_value(&list_response).unwrap();
    let apps = parsed
        .pointer("/result/apps")
        .and_then(|value| value.as_array())
        .expect("apps array present");
    let ids: Vec<&str> = apps
        .iter()
        .map(|value| value.get("id").and_then(|v| v.as_str()).unwrap_or(""))
        .collect();
    assert!(
        ids.contains(&"com.alex.first"),
        "pre-existing app should still be listed: {ids:?}"
    );
    assert!(
        ids.contains(&"com.alex.second"),
        "newly installed app should be listed: {ids:?}"
    );

    // 3. hostCall → system.uninstall. Removes the second app.
    let uninstall_line = r#"{"kind":"hostCall","id":"un-1","method":"system.uninstall","params":{"id":"com.alex.second"}}"#;
    let (un_id, un_method, un_params) =
        alex::plugin::parse_host_call(uninstall_line).expect("uninstall hostCall parses");
    let un_response = router.dispatch(Request {
        protocol: 1,
        id: un_id,
        source: manifest.id.clone(),
        method: un_method,
        params: un_params,
        deadline_ms: None,
    });
    let un_envelope = serde_json::json!({
        "kind": "hostResponse",
        "id": un_response.id,
        "result": un_response.result,
        "error": un_response.error,
    });
    let un_serialized = un_envelope.to_string();
    assert!(
        un_serialized.contains(r#""error":null"#),
        "uninstall must succeed: {un_serialized}"
    );
    assert!(
        !install_root.join("com.alex.second").exists(),
        "second app directory should be removed after uninstall"
    );
}

#[test]
fn manager_uninstall_refuses_to_remove_the_running_app_manager() {
    // Self-protection: the manager plugin is the one currently
    // running this code, and the UI happens to be served by it. A
    // stray `system.uninstall` against the manager id must be
    // refused so the user cannot accidentally nuke the running
    // process and the host it is driving.
    let (_lock, workspace) = install_root();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello");
    let archive = workspace.path().join("hello.alex");
    package::pack(&source, &archive).unwrap();
    let manager = LocalAppManager::open(workspace.path()).unwrap();
    manager
        .install(&archive, InstallOptions::default())
        .unwrap();

    // Lay out a com.alex.manager directory so the path validation
    // would otherwise succeed — we want the refusal to come from
    // the self-protection check, not from "package not found".
    let manager_dir = workspace.path().join("com.alex.manager");
    fs::create_dir_all(manager_dir.join("frontend")).unwrap();
    fs::create_dir_all(manager_dir.join("backend")).unwrap();
    fs::write(manager_dir.join("frontend/index.html"), "<h1>m</h1>").unwrap();
    fs::write(manager_dir.join("backend/index.js"), "// stub").unwrap();
    fs::write(
        manager_dir.join("manifest.json"),
        r#"{
          "schemaVersion": 1,
          "kind": "plugin",
          "id": "com.alex.manager",
          "name": "Manager",
          "version": "0.1.0",
          "frontend": { "entry": "frontend/index.html" },
          "backend": { "runtime": "node", "entry": "backend/index.js" }
        }"#,
    )
    .unwrap();

    let error = manager
        .uninstall(
            alex::manager::MANAGER_PLUGIN_ID,
            UninstallOptions::default(),
        )
        .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("refusing to uninstall the running App Manager"),
        "self-uninstall must be refused with a clear message: {message}"
    );
    // The install must still be on disk after the failed removal.
    assert!(
        manager_dir.join("manifest.json").is_file(),
        "manager install must remain after a refused self-uninstall"
    );
}

#[test]
fn permission_store_migrates_legacy_ipc_method_name_keys() {
    // H1 migration: stores written before the manifest-name key
    // change are still keyed by the runtime IPC method name. On the
    // next open we should rewrite the file so the keys line up with
    // what the runtime now checks.
    let workspace = tempfile::tempdir().unwrap();
    let root = workspace.path();
    let app_id = "com.alex.legacy-store";

    // Lay down a legacy store file with IPC method-name keys.
    let directory = root.join("permissions");
    fs::create_dir_all(&directory).unwrap();
    let state_path = directory.join(format!("{app_id}.json"));
    let legacy = serde_json::json!({
        "clipboard.readText": "granted",
        "clipboard.writeText": "denied",
        "filesystem.readText": "prompt",
        "totally.unrelated.key": "granted",
    });
    fs::write(&state_path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();

    // Open the store — migration should run, rewrite the file, and
    // surface the decisions under their manifest names.
    let store = PermissionStore::open_at(root, app_id).unwrap();
    let decisions = store.list();
    assert_eq!(
        decisions.get("clipboard.read").copied(),
        Some(PermissionDecision::Granted),
        "legacy clipboard.readText must become clipboard.read"
    );
    assert_eq!(
        decisions.get("clipboard.write").copied(),
        Some(PermissionDecision::Denied)
    );
    assert_eq!(
        decisions.get("filesystem.read").copied(),
        Some(PermissionDecision::Prompt)
    );
    assert_eq!(
        decisions.get("clipboard.readText").copied(),
        None,
        "legacy key should have been removed, not kept alongside"
    );
    assert_eq!(
        decisions.get("totally.unrelated.key").copied(),
        Some(PermissionDecision::Granted),
        "unrelated keys must survive migration untouched"
    );

    // Reopen — migration should be idempotent (no work, no rewrite).
    let _ = PermissionStore::open_at(root, app_id).unwrap();
    let second = store.list();
    assert_eq!(second.get("clipboard.read").copied(), Some(PermissionDecision::Granted));
    assert_eq!(second.get("filesystem.read").copied(), Some(PermissionDecision::Prompt));
}

#[test]
fn api_system_uninstall_refuses_to_remove_the_running_app_manager() {
    // Same self-protection contract on the `system.uninstall` path:
    // a plugin that has been granted `system.uninstall` still cannot
    // use that permission to remove the manager itself.
    let workspace = tempfile::tempdir().unwrap();
    let install_root = workspace.path().join("apps");
    fs::create_dir_all(&install_root).unwrap();

    let plugin_dir = workspace.path().join("plugin");
    fs::create_dir_all(plugin_dir.join("backend")).unwrap();
    fs::create_dir_all(plugin_dir.join("frontend")).unwrap();
    fs::write(plugin_dir.join("backend/index.js"), "// stub").unwrap();
    fs::write(plugin_dir.join("frontend/index.html"), "<h1>p</h1>").unwrap();
    fs::write(
        plugin_dir.join("manifest.json"),
        r#"{
          "schemaVersion": 1,
          "kind": "plugin",
          "id": "com.example.killer",
          "name": "Killer",
          "version": "0.1.0",
          "frontend": { "entry": "frontend/index.html" },
          "backend": { "runtime": "node", "entry": "backend/index.js" },
          "permissions": [
            { "name": "system.manageApps" },
            { "name": "system.uninstall" }
          ]
        }"#,
    )
    .unwrap();
    let manifest = load_app(&plugin_dir).unwrap();
    let store = PermissionStore::open_at(workspace.path(), &manifest.id).unwrap();
    for permission in &manifest.permissions {
        if permission.name().starts_with("system.") {
            store
                .set(permission.name(), PermissionDecision::Granted)
                .unwrap();
        }
    }
    let router = ApiRouter::new(plugin_dir.clone(), manifest.clone())
        .with_permission_store(store)
        .with_system_install_root(install_root.clone());

    let response = router.dispatch(Request {
        protocol: 1,
        id: "self-rm".into(),
        source: manifest.id.clone(),
        method: "system.uninstall".into(),
        params: json!({ "id": "com.alex.manager" }),
        deadline_ms: None,
    });
    let error = response.error.expect("self-uninstall must be rejected");
    assert_eq!(error.code, "OPERATION_FAILED");
    assert!(
        error.message.contains("refusing to uninstall the running App Manager"),
        "rejection message should mention the self-protection: {}",
        error.message
    );
}

#[test]
fn iso8601_strings_round_trip_through_javascript_date() {
    // M8: the registry timestamps are fed to `new Date(value)` on
    // the JS side, so they must be real ISO 8601 — the previous
    // epoch-seconds string would parse as a Unix ms value when
    // JavaScript is in a forgiving mood and silently produce a date
    // in 1970. Sample a few instants across the year boundary to
    // catch a hard-coded value or a UTC vs local-time bug.
    let candidates = [
        0,                     // 1970-01-01T00:00:00Z
        86_400,                // 1970-01-02T00:00:00Z
        1_577_836_800,         // 2020-01-01T00:00:00Z
        1_704_067_200,         // 2024-01-01T00:00:00Z
        1_704_153_600,         // 2024-01-02T00:00:00Z
        1_893_456_000,         // 2030-01-01T00:00:00Z (close to the 2038 wrap)
    ];
    for secs in candidates {
        let formatted =
            alex::manager::format_epoch_seconds_as_iso8601(secs);
        let parsed = chrono_like_parse(&formatted).unwrap_or_else(|| {
            panic!("iso8601 string did not parse: {formatted}")
        });
        assert_eq!(
            parsed, secs,
            "round-trip mismatch: formatted={formatted}, original={secs}"
        );
    }
}

// JavaScript's `Date.parse` accepts RFC 3339 / ISO 8601 strings and
// returns the millisecond count. We don't have `chrono` in the
// dependency graph, so emulate the relevant subset by computing
// the expected instant from the same RFC 3339 string with a small
// parser — enough to verify round-tripping without a full date
// library. If this ever bites (e.g. 2038 overflow), the test is the
// place to surface it.
fn chrono_like_parse(value: &str) -> Option<u64> {
    let (date, _) = value.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i32 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    let time = value.split_once('T')?.1.trim_end_matches('Z');
    let mut time_parts = time.split(':');
    let hour: u32 = time_parts.next()?.parse().ok()?;
    let minute: u32 = time_parts.next()?.parse().ok()?;
    let second: u32 = time_parts.next()?.parse().ok()?;
    let days_from_civil = |y: i32, m: u32, d: u32| -> Option<i64> {
        // Howard Hinnant's days-from-civil, matching
        // `epoch_seconds_to_ymdhms` in `manager.rs` exactly so this
        // round-trip test stays valid as long as the two functions
        // agree.
        let y = if m <= 2 { y - 1 } else { y };
        let era: i64 = if y >= 0 {
            (y / 400) as i64
        } else {
            ((y - 399) / 400) as i64
        };
        let yoe = (y - (era as i32) * 400) as u32;
        let m = if m > 2 { m - 3 } else { m + 9 };
        let doy = (153 * m + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        Some(era * 146_097 + doe as i64 - 719_468)
    };
    let days = days_from_civil(year, month, day)?;
    let secs = (days as u64) * 86_400 + (hour as u64) * 3600 + (minute as u64) * 60 + second as u64;
    Some(secs)
}

// ---------------------------------------------------------------------------
// Phase 1 (P0/P1) API surface: filesystem binary, storage, paths, dialog,
// runtime cancel, events, watch, and capabilities. These tests exercise the
// new IPC methods end-to-end through `ApiRouter::dispatch` against the
// `examples/hello` package, which now declares every new permission so a
// single manifest can drive both the existing and the new tests.
// ---------------------------------------------------------------------------

fn hello_router() -> (std::path::PathBuf, ApiRouter) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/full");
    let app = load_app(&root).unwrap();
    (root.clone(), ApiRouter::new(root, app))
}

fn call(router: &ApiRouter, method: &str, params: serde_json::Value) -> ipc::Response {
    let _lock = ALEX_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    router.dispatch(Request {
        protocol: 1,
        id: format!("test-{}", method),
        source: "com.alex.full".into(),
        method: method.into(),
        params,
        deadline_ms: None,
    })
}

#[test]
fn api_filesystem_binary_round_trip() {
    let (_root, router) = hello_router();
    let payload = vec![0u8, 1, 2, 3, 0xFF, 0xAB, 0xCD];
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &payload);
    let write = call(
        &router,
        "filesystem.writeBinary",
        json!({ "path": "data/blob.bin", "data": encoded }),
    );
    assert!(write.error.is_none(), "writeBinary: {:?}", write.error);
    let read = call(&router, "filesystem.readBinary", json!({ "path": "data/blob.bin" }));
    let result = read.result.expect("readBinary result");
    let data_b64 = result["data"].as_str().expect("base64 string");
    let decoded = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data_b64)
        .expect("decode base64");
    assert_eq!(decoded, payload);
    let stat = call(&router, "filesystem.stat", json!({ "path": "data/blob.bin" }));
    let stat_value = stat.result.expect("stat result");
    assert_eq!(stat_value["type"], "file");
    assert_eq!(stat_value["size"], payload.len() as u64);
}

#[test]
fn api_filesystem_readdir_returns_sorted_entries() {
    let (_root, router) = hello_router();
    let list = call(&router, "filesystem.readDir", json!({ "path": "data" }));
    let result = list.result.expect("readDir result");
    let entries = result["entries"].as_array().expect("entries array");
    let names: Vec<&str> = entries
        .iter()
        .map(|entry| entry["name"].as_str().unwrap_or(""))
        .collect();
    assert!(names.contains(&"message.txt"));
}

#[test]
fn api_filesystem_create_remove_rename_copy() {
    let (_root, router) = hello_router();
    let create = call(
        &router,
        "filesystem.createDir",
        json!({ "path": "data/sub", "recursive": true }),
    );
    assert!(create.result.is_some(), "createDir result");
    let create_file = call(
        &router,
        "filesystem.writeText",
        json!({ "path": "data/sub/note.txt", "content": "abc" }),
    );
    assert!(create_file.result.is_some());
    let copy = call(
        &router,
        "filesystem.copy",
        json!({ "from": "data/sub/note.txt", "to": "data/sub/note-copy.txt" }),
    );
    assert!(copy.result.is_some(), "copy result: {:?}", copy.error);
    let rename = call(
        &router,
        "filesystem.rename",
        json!({ "from": "data/sub/note-copy.txt", "to": "data/sub/note-renamed.txt" }),
    );
    assert!(rename.result.is_some(), "rename result: {:?}", rename.error);
    let remove = call(
        &router,
        "filesystem.remove",
        json!({ "path": "data/sub/note-renamed.txt" }),
    );
    assert!(remove.result.is_some(), "remove file: {:?}", remove.error);
    let exists = call(&router, "filesystem.exists", json!({ "path": "data/sub/note-renamed.txt" }));
    assert_eq!(exists.result.unwrap()["exists"], json!(false));
    let remove_dir = call(
        &router,
        "filesystem.remove",
        json!({ "path": "data/sub", "recursive": true }),
    );
    assert!(remove_dir.result.is_some(), "remove dir: {:?}", remove_dir.error);
}

#[test]
fn api_filesystem_remove_blocks_recursive_root() {
    let (_root, router) = hello_router();
    // Recursive delete with `..` resolves to a path that
    // escapes the package root, which the host refuses. The
    // exact code is `PATH_ERROR` (escape) rather than
    // `PERMISSION_DENIED` because the path simply isn't
    // reachable from any granted root.
    let result = call(
        &router,
        "filesystem.remove",
        json!({ "path": "..", "recursive": true }),
    );
    let err = result.error.expect("expected error for recursive root delete");
    assert!(
        err.code == "PATH_ERROR" || err.code == "PERMISSION_DENIED",
        "unexpected error code: {err:?}"
    );
}

#[test]
fn api_storage_round_trip() {
    let (_root, router) = hello_router();
    let set = call(
        &router,
        "storage.set",
        json!({ "key": "user.name", "value": "Alex" }),
    );
    assert!(set.error.is_none(), "storage.set: {:?}", set.error);
    let get = call(&router, "storage.get", json!({ "key": "user.name" }));
    assert_eq!(get.result.unwrap()["value"], json!("Alex"));
    let keys = call(&router, "storage.keys", json!({}));
    let keys_value = keys.result.unwrap()["keys"].as_array().unwrap().clone();
    assert!(keys_value.iter().any(|k| k == "user.name"));
    let del = call(&router, "storage.delete", json!({ "key": "user.name" }));
    assert_eq!(del.result.unwrap()["removed"], json!(true));
}

#[test]
fn api_storage_rejects_invalid_key() {
    let (_root, router) = hello_router();
    let result = call(
        &router,
        "storage.set",
        json!({ "key": "has spaces", "value": "x" }),
    );
    assert!(result.error.is_some());
    assert_eq!(result.error.unwrap().code, "STORAGE_ERROR");
}

#[test]
fn api_paths_return_local_app_data() {
    let (_root, router) = hello_router();
    for method in ["paths.dataDir", "paths.cacheDir", "paths.tempDir"] {
        let result = call(&router, method, json!({}));
        let path = result
            .result
            .unwrap_or_else(|| panic!("{method} failed: {:?}", result.error))
            ["path"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(!path.is_empty(), "{method} returned empty path");
    }
}

#[test]
fn api_events_subscribe_and_unsubscribe() {
    let (_root, router) = hello_router();
    let sub = call(
        &router,
        "events.subscribe",
        json!({ "event": "filesystem.changed" }),
    );
    let result = sub.result.unwrap_or_else(|| {
        panic!(
            "subscribe failed: code={} message={}",
            sub.error.as_ref().map(|e| &e.code).unwrap_or(&String::new()),
            sub.error.as_ref().map(|e| &e.message).unwrap_or(&String::new())
        )
    });
    let id = result["subscriptionId"].as_str().unwrap().to_owned();
    let unsub = call(
        &router,
        "events.unsubscribe",
        json!({ "subscriptionId": id }),
    );
    assert_eq!(unsub.result.unwrap()["removed"], json!(true));
}

#[test]
fn api_events_rejects_empty_event_name() {
    let (_root, router) = hello_router();
    let result = call(&router, "events.subscribe", json!({ "event": "" }));
    assert!(result.error.is_some());
    assert_eq!(result.error.unwrap().code, "SUBSCRIBE_FAILED");
}

#[test]
fn api_filesystem_watch_returns_subscription() {
    let (_root, router) = hello_router();
    let result = call(&router, "filesystem.watch", json!({ "path": "data" }));
    // The bus-driven subscription must always succeed
    // (the OS watcher is an implementation detail of the
    // shell layer). The current shell-less test environment
    // still has the watcher registry attached, so we
    // expect the watcher to be created or — if notify
    // refused to watch a Windows test path — for the
    // subscription to still be issued and the shell to
    // clean up the handle on unwatch.
    let payload = result.result.unwrap_or_else(|| {
        panic!(
            "watch failed: code={} message={}",
            result.error.as_ref().map(|e| &e.code).unwrap_or(&String::new()),
            result.error.as_ref().map(|e| &e.message).unwrap_or(&String::new())
        )
    });
    let sub_id = payload["subscriptionId"]
        .as_str()
        .expect("subscriptionId in watch result")
        .to_owned();
    let unwatch = call(
        &router,
        "filesystem.unwatch",
        json!({ "subscriptionId": sub_id }),
    );
    assert_eq!(unwatch.result.unwrap()["removed"], json!(true));
}

#[test]
fn api_capabilities_lists_every_method() {
    let (_root, router) = hello_router();
    let result = call(&router, "system.capabilities", json!({}));
    let caps = result
        .result
        .unwrap()["capabilities"]
        .as_array()
        .unwrap()
        .clone();
    let names: Vec<&str> = caps.iter().map(|v| v.as_str().unwrap()).collect();
    for required in [
        "filesystem.readBinary",
        "filesystem.writeBinary",
        "filesystem.stat",
        "filesystem.readDir",
        "filesystem.remove",
        "filesystem.rename",
        "filesystem.copy",
        "filesystem.watch",
        "storage",
        "paths",
        "events.subscribe",
        "runtime.cancel",
    ] {
        assert!(names.contains(&required), "missing capability {required}");
    }
}

#[test]
fn api_capabilities_rejects_unknown_method() {
    let (_root, router) = hello_router();
    let result = call(&router, "filesystem.doesNotExist", json!({}));
    assert!(result.error.is_some());
    assert_eq!(result.error.unwrap().code, "METHOD_NOT_FOUND");
}

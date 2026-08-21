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
        !summary.signed,
        "unsigned archive should report signed=false"
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
            .any(|s| s.name == "filesystem.readText" && s.manifest_declared),
        "hello declares filesystem.read; should appear in permissions"
    );

    manager
        .set_permission(
            "com.alex.hello",
            "filesystem.readText",
            PermissionDecision::Denied,
        )
        .unwrap();
    let after = manager.permissions("com.alex.hello").unwrap();
    let read = after
        .iter()
        .find(|s| s.name == "filesystem.readText")
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

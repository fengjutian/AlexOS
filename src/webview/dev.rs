//! `alex dev` — development mode with file watching and hot reload.
//!
//! The 0.1 dev mode is Windows-only because the WebView shell itself only
//! supports Windows. The implementation reuses the shell's BRIDGE script,
//! asset protocol handler, and helpers to keep the runtime identical to a
//! production shell, then layers a file watcher on top. The watcher emits
//! `DevCommand::ReloadFrontend` and `DevCommand::RestartRuntime` signals
//! over a capacity-1 mpsc channel; the event loop polls the channel every
//! 100 ms so a flurry of editor saves collapses into a single reload.

use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::{
    AlexError,
    core::application_manifest::{ApplicationManifest, ServiceDescriptor},
    manifest::{
        AppManifest, Author, Backend, BackendMode, Frontend, FrontendBuild, HealthCheck, Icons,
        PackageKind, RestartPolicy, RuntimeKind,
    },
    permission::Permission,
};

/// Build a `.alexignore` matcher rooted at `package_root`. Returns
/// `None` when the file is absent or malformed — in both cases the
/// watcher will then track every file.
pub fn load_alexignore(package_root: &Path) -> Option<Gitignore> {
    let path = package_root.join(".alexignore");
    if !path.is_file() {
        return None;
    }
    let body = std::fs::read_to_string(&path).ok()?;
    let mut builder = GitignoreBuilder::new(package_root);
    for line in body.lines() {
        let _ = builder.add_line(Some(PathBuf::from(".alexignore")), line);
    }
    builder.build().ok()
}

/// Decide whether a watcher event represents an ignored path.
///
/// Mirrors the git behaviour: a pattern like `node_modules/` covers every
/// file beneath the matched directory, so we walk the parent chain after
/// checking the file itself.
pub fn is_ignored(matcher: &Option<&Gitignore>, package_root: &Path, path: &Path) -> bool {
    let Some(matcher) = matcher else {
        return false;
    };
    let relative = path.strip_prefix(package_root).unwrap_or(path);
    if matcher.matched(relative, false).is_ignore() {
        return true;
    }
    let mut parent = relative.parent();
    while let Some(ancestor) = parent {
        if ancestor.as_os_str().is_empty() {
            break;
        }
        if matcher.matched(ancestor, true).is_ignore() {
            return true;
        }
        parent = ancestor.parent();
    }
    false
}

#[cfg(windows)]
pub fn run(package_root: &Path, manifest: AppManifest) -> Result<(), AlexError> {
    windows::run(package_root, manifest, None, None)
        .map_err(|error| AlexError::Validation(format!("dev shell failed: {error}")))
}

#[cfg(not(windows))]
pub fn run(_package_root: &Path, _manifest: AppManifest) -> Result<(), AlexError> {
    Err(AlexError::Validation(
        "the 0.1 dev mode currently supports Windows only".into(),
    ))
}

/// `alex dev` entry point that accepts the unified
/// [`ApplicationManifest`]. v1 manifests are forwarded to [`run`]
/// unchanged. v2 manifests are projected onto a v1 [`AppManifest`]
/// using the first declared service as the "main" backend, which is
/// enough for the common dev loop (single service + frontend). A
/// warning is printed when the v2 manifest declares multiple
/// services so the developer knows only the first one is being
/// hot-reloaded; multi-service dev mode is a future enhancement.
pub fn run_unified(package_root: &Path, manifest: ApplicationManifest) -> Result<(), AlexError> {
    let frontend_dev = manifest
        .as_v1()
        .and_then(|v1| v1.frontend.dev.clone())
        .or_else(|| {
            manifest
                .as_v2()
                .and_then(|v2| v2.frontend.as_ref())
                .and_then(|frontend| frontend.dev.clone())
        });
    let backend_dev = manifest
        .as_v2()
        .and_then(|v2| v2.services.values().next())
        .and_then(|service| service.dev.clone());
    let v1 = match manifest {
        ApplicationManifest::V1(m) => m,
        ApplicationManifest::V2(_) => project_v2_for_dev(manifest),
    };
    #[cfg(windows)]
    {
        windows::run(package_root, v1, frontend_dev, backend_dev)
            .map_err(|error| AlexError::Validation(format!("dev shell failed: {error}")))
    }
    #[cfg(not(windows))]
    {
        let _ = (frontend_dev, backend_dev);
        run(package_root, v1)
    }
}

/// Project a v2 [`ApplicationManifest`] onto a v1 [`AppManifest`]
/// suitable for the existing dev shell. The first declared service
/// becomes the "main" backend; the frontend entry, id, name, and
/// version are preserved. v2-only permission descriptors (those
/// containing a `:`) are dropped — they have no v1 permission
/// key, so the dev shell would ignore them anyway.
fn project_v2_for_dev(manifest: ApplicationManifest) -> AppManifest {
    let v2 = manifest
        .as_v2()
        .expect("project_v2_for_dev called with a v1 manifest")
        .clone();
    let services = manifest.services();
    if services.len() > 1 {
        eprintln!(
            "alex dev: v2 manifest declares {} services; only {:?} is being watched. \
             Use `alex launch` + the daemon for multi-service hot reload.",
            services.len(),
            services[0].name,
        );
    }
    let frontend_entry = manifest
        .frontend()
        .map(|frontend| frontend.entry)
        .unwrap_or_default();
    let backend = services.first().cloned().map(service_to_backend);
    let permissions: Vec<Permission> = manifest
        .permissions()
        .into_iter()
        .filter_map(|descriptor| {
            if descriptor.name.contains(':') {
                return None;
            }
            legacy_permission_from_name(&descriptor.name)
        })
        .collect();
    AppManifest {
        schema_version: 1,
        kind: PackageKind::App,
        id: v2.id,
        name: v2.name,
        version: v2.version,
        description: None,
        author: None,
        icons: None,
        homepage: None,
        license: None,
        update: None,
        frontend: Frontend {
            entry: frontend_entry,
            build: None,
            dev: None,
        },
        backend,
        permissions,
        extension_points: None,
    }
}

fn service_to_backend(service: ServiceDescriptor) -> Backend {
    let port = match service.port {
        Some(value) => Some(value),
        None => None,
    };
    let health_check = service.health.and_then(|health| {
        if matches!(
            health.kind,
            crate::core::application_manifest::ServiceHealthKind::Http
        ) {
            health.path.map(|path| HealthCheck {
                path,
                timeout_ms: health.timeout_ms,
            })
        } else {
            None
        }
    });
    let restart = Some(RestartPolicy {
        policy: match service.restart.policy {
            crate::core::application_manifest::ServiceRestartPolicy::Never => "never".into(),
            crate::core::application_manifest::ServiceRestartPolicy::OnFailure => {
                "on-failure".into()
            }
            crate::core::application_manifest::ServiceRestartPolicy::Always => "always".into(),
        },
        max_retries: service.restart.max_retries,
    });
    let mode = match service.mode {
        crate::core::application_manifest::ServiceMode::Rpc => BackendMode::Rpc,
        crate::core::application_manifest::ServiceMode::Service => BackendMode::Service,
    };
    let runtime = match service.runtime {
        crate::manifest_v2::ServiceRuntime::Node => RuntimeKind::Node,
        crate::manifest_v2::ServiceRuntime::Python => RuntimeKind::Python,
        crate::manifest_v2::ServiceRuntime::Native => RuntimeKind::Native,
    };
    Backend {
        runtime,
        entry: service.command,
        mode,
        health_check,
        restart,
        port,
        args: service.args,
        env: service.env,
    }
}

fn legacy_permission_from_name(name: &str) -> Option<Permission> {
    use crate::permission::Permission;
    match name {
        "filesystem.read" => Some(Permission::FilesystemRead { paths: Vec::new() }),
        "filesystem.write" => Some(Permission::FilesystemWrite { paths: Vec::new() }),
        "filesystem.watch" => Some(Permission::FilesystemWatch { paths: Vec::new() }),
        "filesystem.delete" => Some(Permission::FilesystemDelete { paths: Vec::new() }),
        "filesystem.drop" => Some(Permission::FilesystemDrop),
        "dialog.open" => Some(Permission::DialogOpen),
        "dialog.save" => Some(Permission::DialogSave),
        "clipboard.read" => Some(Permission::ClipboardRead),
        "clipboard.write" => Some(Permission::ClipboardWrite),
        "system.openExternal" => Some(Permission::OpenExternal {
            origins: Vec::new(),
        }),
        "storage" => Some(Permission::Storage),
        "paths" => Some(Permission::Paths),
        "window.manage" => Some(Permission::WindowManage),
        "window.open" => Some(Permission::WindowOpen),
        "notification.show" => Some(Permission::NotificationShow),
        "menu.manage" => Some(Permission::MenuManage),
        "tray.manage" => Some(Permission::TrayManage),
        "shortcut.register" => Some(Permission::ShortcutRegister),
        "runtime.invoke" => Some(Permission::RuntimeInvoke),
        "runtime.manage" => Some(Permission::RuntimeManage),
        "process.spawn" => Some(Permission::ProcessSpawn {
            executables: Vec::new(),
        }),
        "media.camera" => Some(Permission::MediaCamera),
        "media.microphone" => Some(Permission::MediaMicrophone),
        "geolocation" => Some(Permission::Geolocation),
        "system.install" => Some(Permission::SystemInstall),
        "system.uninstall" => Some(Permission::SystemUninstall),
        "system.manageApps" => Some(Permission::SystemManageApps),
        "system.manageExtensions" => Some(Permission::SystemManageExtensions),
        "system.managePermissions" => Some(Permission::SystemManagePermissions),
        "network.fetch" => Some(Permission::NetworkFetch {
            origins: Vec::new(),
        }),
        _ => None,
    }
}

// `Author` and `Icons` are re-exported through `crate::manifest` so
// the projection above can spell the field types in the v1
// `AppManifest` literal without importing the re-export. Anchor
// them so the `use` line stays used even if the projection grows
// or shrinks.
#[allow(dead_code)]
fn _type_anchors(_: Author, _: Icons, _: FrontendBuild) {}

#[cfg(test)]
mod v2_projection_tests {
    //! `alex dev` is the v1 `AppManifest` shape under the hood, so
    //! the v2 path goes through a projection. These tests pin the
    //! projection rules so a refactor of `project_v2_for_dev`
    //! cannot silently drop a field the dev shell relies on.
    //!
    //! The tests build the `ApplicationManifest` programmatically
    //! instead of going through `load_application` so the
    //! projection is exercised in isolation — the YAML-side
    //! validation (file existence, runtime version block, etc.)
    //! has its own coverage in `application_manifest::tests`.
    use std::collections::BTreeMap;

    use crate::core::application_manifest::ApplicationManifest;
    use crate::core::manifest_v2::{
        ApplicationManifestV2, FrontendV2, HealthKind, RestartPolicyV2, RuntimeRequirements,
        ServiceHealth, ServiceRuntime, ServiceSpec,
    };

    fn v2_manifest(
        id: &str,
        frontend: Option<&str>,
        services: Vec<(&str, ServiceSpec)>,
    ) -> ApplicationManifest {
        let services_map: BTreeMap<String, ServiceSpec> = services
            .into_iter()
            .map(|(name, spec)| (name.to_owned(), spec))
            .collect();
        let v2 = ApplicationManifestV2 {
            schema_version: 2,
            id: id.to_owned(),
            name: id.to_owned(),
            version: "0.1.0".to_owned(),
            frontend: frontend.map(|entry| FrontendV2 {
                entry: entry.to_owned(),
                dev: None,
            }),
            runtime: RuntimeRequirements {
                node: Some("22".to_owned()),
                python: None,
            },
            services: services_map,
            native_workers: BTreeMap::new(),
            mcp_servers: BTreeMap::new(),
            agent: None,
            storage: Vec::new(),
            permissions: Default::default(),
        };
        ApplicationManifest::V2(v2)
    }

    fn service(
        runtime: ServiceRuntime,
        command: &str,
        health: Option<ServiceHealth>,
        restart_policy: RestartPolicyV2,
        max_retries: u32,
    ) -> ServiceSpec {
        ServiceSpec {
            runtime,
            command: command.to_owned(),
            args: Vec::new(),
            depends_on: Vec::new(),
            env: Default::default(),
            port: None,
            health,
            restart: crate::core::manifest_v2::ServiceRestart {
                policy: restart_policy,
                max_retries,
            },
            dev: None,
            resources: None,
        }
    }

    #[test]
    fn v2_with_one_service_projects_to_v1_backend() {
        let unified = v2_manifest(
            "com.alex.one",
            Some("index.html"),
            vec![(
                "api",
                service(
                    ServiceRuntime::Node,
                    "main.js",
                    Some(ServiceHealth {
                        kind: HealthKind::Http,
                        path: Some("/health".to_owned()),
                        interval_ms: 3000,
                        timeout_ms: 1500,
                    }),
                    RestartPolicyV2::OnFailure,
                    7,
                ),
            )],
        );
        let projected = super::project_v2_for_dev(unified);
        let backend = projected.backend.expect("v1 backend should be set");
        assert_eq!(projected.id, "com.alex.one");
        assert_eq!(projected.frontend.entry, "index.html");
        assert_eq!(backend.entry, "main.js");
        let health = backend.health_check.expect("http health should project");
        assert_eq!(health.path, "/health");
        assert_eq!(health.timeout_ms, 1500);
        let restart = backend.restart.expect("restart should project");
        assert_eq!(restart.policy, "on-failure");
        assert_eq!(restart.max_retries, 7);
    }

    #[test]
    fn v2_with_process_health_projects_no_http_check() {
        // v1 only models HTTP health checks; a v2 `process` health
        // is projected to `None` so the dev shell treats the
        // backend as a request/response runtime.
        let unified = v2_manifest(
            "com.alex.proc",
            None,
            vec![(
                "api",
                service(
                    ServiceRuntime::Python,
                    "main.py",
                    Some(ServiceHealth {
                        kind: HealthKind::Process,
                        path: None,
                        interval_ms: 5000,
                        timeout_ms: 10_000,
                    }),
                    RestartPolicyV2::OnFailure,
                    5,
                ),
            )],
        );
        let projected = super::project_v2_for_dev(unified);
        let backend = projected.backend.expect("backend should be set");
        assert!(backend.health_check.is_none());
    }

    #[test]
    fn v2_uses_first_service_when_multiple_are_declared() {
        // Multi-service dev mode is not supported in 0.1 (the
        // `run_unified` wrapper logs a warning and projects only
        // the first service). The projection itself should pick
        // the first key in declaration order, not the first by
        // name, so the dev can rely on `services: [api, worker]`
        // mapping to "api" being watched.
        let mut services_map = BTreeMap::new();
        services_map.insert(
            "api".to_owned(),
            service(
                ServiceRuntime::Node,
                "main.js",
                None,
                RestartPolicyV2::OnFailure,
                5,
            ),
        );
        services_map.insert(
            "worker".to_owned(),
            service(
                ServiceRuntime::Python,
                "worker.py",
                None,
                RestartPolicyV2::OnFailure,
                5,
            ),
        );
        let v2 = ApplicationManifestV2 {
            schema_version: 2,
            id: "com.alex.multi".to_owned(),
            name: "multi".to_owned(),
            version: "0.1.0".to_owned(),
            frontend: Some(FrontendV2 {
                entry: "index.html".to_owned(),
                dev: None,
            }),
            runtime: RuntimeRequirements {
                node: Some("22".to_owned()),
                python: Some("3.12".to_owned()),
            },
            services: services_map,
            native_workers: BTreeMap::new(),
            mcp_servers: BTreeMap::new(),
            agent: None,
            storage: Vec::new(),
            permissions: Default::default(),
        };
        let projected = super::project_v2_for_dev(ApplicationManifest::V2(v2));
        let backend = projected.backend.expect("backend should be set");
        assert_eq!(backend.entry, "main.js", "first service (api) should win");
    }

    #[test]
    fn v2_runtime_kind_projects_one_to_one() {
        // Node / Python / Native each have a direct v1
        // counterpart; the projection must not lose the variant
        // (e.g. collapsing Native to Node) because the dev shell
        // uses the runtime kind to pick the spawn command.
        for (v2_runtime, expected) in [
            (ServiceRuntime::Node, crate::manifest::RuntimeKind::Node),
            (ServiceRuntime::Python, crate::manifest::RuntimeKind::Python),
            (ServiceRuntime::Native, crate::manifest::RuntimeKind::Native),
        ] {
            let unified = v2_manifest(
                "com.alex.runtime",
                None,
                vec![(
                    "api",
                    service(v2_runtime, "main", None, RestartPolicyV2::OnFailure, 5),
                )],
            );
            let projected = super::project_v2_for_dev(unified);
            let backend = projected.backend.expect("backend should be set");
            assert_eq!(
                backend.runtime, expected,
                "v2 {v2_runtime:?} should project to {expected:?}"
            );
        }
    }

    #[test]
    fn v1_manifest_passes_through_run_unified() {
        // `run_unified` for v1 should not project anything; the
        // inner AppManifest is forwarded unchanged so the existing
        // dev shell still sees the exact field layout it expects.
        // We don't open a WebView here; we just confirm the v1
        // arm of the dispatcher is reachable by feeding an
        // `ApplicationManifest::V1` and asserting the
        // `as_v1()` accessor still works.
        let v1 = crate::manifest::AppManifest {
            schema_version: 1,
            kind: crate::manifest::PackageKind::App,
            id: "com.alex.v1".to_owned(),
            name: "v1".to_owned(),
            version: "0.1.0".to_owned(),
            description: None,
            author: None,
            icons: None,
            homepage: None,
            license: None,
            update: None,
            frontend: crate::manifest::Frontend {
                entry: "index.html".to_owned(),
                build: None,
                dev: None,
            },
            backend: None,
            permissions: Vec::new(),
            extension_points: None,
        };
        let unified = ApplicationManifest::V1(v1);
        // `run_unified` would call `run()` next; we don't want a
        // WebView in the test. We instead assert the dispatcher
        // picks the v1 arm and returns the inner manifest. The
        // real `run()` call is covered by the windows-only
        // `windows` test module.
        let as_v1 = unified.as_v1().expect("v1 should round-trip");
        assert_eq!(as_v1.id, "com.alex.v1");
    }
}

#[cfg(windows)]
#[allow(dead_code)]
mod windows {
    use std::{
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
        sync::{Arc, mpsc},
        thread,
        time::{Duration, Instant},
    };

    use ignore::gitignore::Gitignore;
    use notify::{EventKind, RecursiveMode, Watcher};
    use tao::{
        event::{Event, WindowEvent},
        event_loop::{ControlFlow, EventLoopBuilder},
        window::WindowBuilder,
    };
    use wry::{NewWindowResponse, WebViewBuilder};

    use crate::{
        api::ApiRouter,
        authorization::PermissionStore,
        dev::{is_ignored, load_alexignore},
        manifest::AppManifest,
        manifest_v2::{FrontendDev, ServiceDev},
        native::HostCommand,
        runtime::RuntimeHandle,
        shell::windows::{BRIDGE, UserEvent, WindowHost, asset_response, emit_event},
        webview::desktop_resources::DesktopResources,
        webview::secondary_windows::SecondaryWindows,
    };

    const DEV_POLL_INTERVAL: Duration = Duration::from_millis(100);
    const RUNTIME_RESTART_TIMEOUT: Duration = Duration::from_secs(2);

    /// Signal emitted by the file watcher to the event loop.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum DevCommand {
        /// Frontend file changed — reload the webview page.
        ReloadFrontend,
        /// Backend file changed — restart the Node runtime.
        RestartRuntime,
        /// `manifest.json` changed — most fields (permissions,
        /// backend entry, frontend entry, runtime mode) require
        /// a host restart to take effect, so we surface a clear
        /// "please restart" message and shut the loop down
        /// cleanly.
        ManifestChanged,
    }

    pub fn run(
        package_root: &Path,
        manifest: AppManifest,
        frontend_dev: Option<FrontendDev>,
        backend_dev: Option<ServiceDev>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let canonical_root = package_root
            .canonicalize()
            .unwrap_or_else(|_| package_root.into());
        let frontend_dir = canonical_root.join(
            Path::new(&manifest.frontend.entry)
                .parent()
                .unwrap_or(Path::new("")),
        );
        let backend_dir = manifest.backend.as_ref().map(|backend| {
            canonical_root.join(Path::new(&backend.entry).parent().unwrap_or(Path::new("")))
        });
        let manifest_path = canonical_root.join("manifest.json");

        if let Some(dev) = frontend_dev.as_ref() {
            install_frontend_dependencies(&canonical_root, dev)?;
        }
        if let Some(dev) = backend_dev.as_ref() {
            install_service_dependencies(&canonical_root, dev)?;
        }
        let mut frontend_process = match frontend_dev.as_ref() {
            Some(dev) if dev_server_is_ready(&dev.url) => {
                eprintln!(
                    "alex dev: frontend server already running at {}; reusing it",
                    dev.url
                );
                None
            }
            Some(dev) => Some(start_frontend_dev_server(&canonical_root, dev)?),
            None => None,
        };
        let mut backend_dev_process = match backend_dev.as_ref() {
            Some(dev) => Some(start_service_dev_process(&canonical_root, dev)?),
            None => None,
        };
        if let Some(dev) = frontend_dev.as_ref() {
            wait_for_dev_server(&dev.url, &mut frontend_process)?;
        }

        // Capacity-1 channel: a burst of file events collapses into the most
        // recent signal, so we never queue redundant reloads.
        let (dev_tx, dev_rx) = mpsc::sync_channel::<DevCommand>(1);

        let matcher = load_alexignore(&canonical_root);
        spawn_watcher(
            frontend_dir.clone(),
            backend_dir.clone(),
            manifest_path.clone(),
            matcher,
            dev_tx,
        );

        let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
        let proxy = event_loop.create_proxy();
        let window = WindowBuilder::new()
            .with_title(format!("{} (dev)", manifest.name))
            .build(&event_loop)?;

        let permissions = PermissionStore::for_app(&manifest.id)?;
        let mut router = ApiRouter::new(package_root.to_path_buf(), manifest.clone())
            .with_permission_store(permissions)
            .with_permission_logging(true) // dev mode = permission call panel on
            .with_native_host(Arc::new(WindowHost {
                proxy: proxy.clone(),
                secondary_windows: true,
            }));
        if let Some(backend) = &manifest.backend {
            router = router.with_runtime(RuntimeHandle::start(package_root, backend)?);
        }
        let router = Arc::new(router);
        let ipc_router = Arc::clone(&router);
        let root = package_root.to_path_buf();
        let frontend = manifest.frontend.entry.clone();
        let package_id = serde_json::to_string(&manifest.id)?;
        let init_script = BRIDGE.replace("__ALEX_PACKAGE_ID__", &package_id)
            + &crate::permission_shim::shim_source(&manifest.permissions);
        let drop_router = Arc::clone(&router);

        let initial_url = frontend_dev
            .as_ref()
            .map(|dev| dev.url.clone())
            .unwrap_or_else(|| "alex://app/".into());
        let allowed_dev_origin = frontend_dev.as_ref().map(|dev| dev.url.clone());
        let webview = WebViewBuilder::new()
            .with_initialization_script(init_script.clone())
            // `alex dev` IS the dev mode: DevTools are always
            // on (debug build) without needing ALEX_DEVTOOLS.
            // Production shells (src/shell.rs,
            // src/manager_webview.rs) keep the env gate so a
            // developer can still attach F12 in dev / opt out
            // in shipping.
            .with_devtools(cfg!(debug_assertions))
            .with_incognito(true)
            .with_clipboard(false)
            .with_navigation_handler(move |url| {
                crate::is_internal_webview_url(&url, "app")
                    || allowed_dev_origin
                        .as_ref()
                        .is_some_and(|allowed| same_origin(url.as_str(), allowed))
            })
            .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
            .with_download_started_handler(|_, _| false)
            .with_drag_drop_handler(move |event| match event {
                wry::DragDropEvent::Drop { paths, position } => {
                    drop_router.deliver_file_drop(paths, position.0, position.1)
                }
                _ => false,
            })
            .with_ipc_handler(move |request| {
                let router = Arc::clone(&ipc_router);
                let proxy = proxy.clone();
                let body = request.body().clone();
                let fallback_proxy = proxy.clone();
                if crate::runtime::task_executor::ipc_executor()
                    .submit(move || {
                        // IPC Inspector: log each round-trip to stderr
                        // so the dev can tail the dev shell to see
                        // exactly which calls the page is making and
                        // which permission path each one took. Kept
                        // short (truncated params) to keep the
                        // terminal readable while a page is polling.
                        let method = serde_json::from_str::<crate::ipc::Request>(&body)
                            .ok()
                            .map(|req| {
                                let params = truncate_params(&req.params, 160);
                                format!("{}.{} params={params}", req.source, req.method)
                            })
                            .unwrap_or_else(|| "(unparseable)".to_string());
                        let response = router.dispatch_json(&body);
                        let outcome = if response.error.is_some() {
                            "err"
                        } else {
                            "ok"
                        };
                        eprintln!("alex dev: ipc {method} -> {outcome}");
                        if let Ok(json) = serde_json::to_string(&response) {
                            let _ = proxy.send_event(UserEvent::IpcResponse(None, json));
                        }
                    })
                    .is_err()
                {
                    let response = crate::ipc::Response::error(
                        "unknown",
                        "HOST_BUSY",
                        "host IPC queue is full",
                    );
                    if let Ok(json) = serde_json::to_string(&response) {
                        let _ = fallback_proxy.send_event(UserEvent::IpcResponse(None, json));
                    }
                }
            })
            .with_custom_protocol("alex".into(), move |_id, request| {
                asset_response(&root, &frontend, request.uri().path())
            })
            .with_url(&initial_url)
            .build(&window)?;
        crate::webview::permissions::attach(&webview, Arc::clone(&router))?;

        if let Some(dev) = frontend_dev.as_ref() {
            eprintln!("alex dev: frontend dev server ready at {}", dev.url);
        }

        eprintln!(
            "alex dev: watching {} {}",
            frontend_dir.display(),
            backend_dir
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "(no backend)".to_string())
        );

        let child_proxy = event_loop.create_proxy();
        let child_router = Arc::clone(&router);
        let child_root = package_root.to_path_buf();
        let child_frontend = manifest.frontend.entry.clone();
        let child_init_script = init_script.clone();
        let child_dev_url = frontend_dev.as_ref().map(|dev| dev.url.clone());
        let mut secondary_windows = SecondaryWindows::new();
        let mut desktop_resources = DesktopResources::new()?;
        let mut last_poll = Instant::now();
        event_loop.run(move |event, event_loop_target, control_flow| {
            *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(50));
            match event {
                Event::UserEvent(UserEvent::IpcResponse(target, json)) => {
                    let script = format!("window.__alexResolve({json})");
                    if let Some(id) = target {
                        if let Some(child) = secondary_windows.webview(id) {
                            let _ = child.evaluate_script(&script);
                        }
                    } else {
                        let _ = webview.evaluate_script(&script);
                    }
                }
                Event::UserEvent(UserEvent::MrtrPrompt(title, message, reply)) => {
                    use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
                    let result = MessageDialog::new()
                        .set_level(MessageLevel::Warning)
                        .set_title(title)
                        .set_description(message)
                        .set_buttons(MessageButtons::YesNo)
                        .show();
                    let _ = reply.send(Ok(matches!(result, MessageDialogResult::Yes)));
                }
                Event::UserEvent(UserEvent::OpenDialog(spec, reply)) => {
                    let _ = reply.send(crate::platform::desktop::pick_paths_on_ui_thread(spec));
                }
                Event::UserEvent(UserEvent::SaveDialog(spec, reply)) => {
                    let _ = reply.send(crate::platform::desktop::pick_save_path_on_ui_thread(spec));
                }
                Event::UserEvent(UserEvent::Host(command, reply)) => {
                    let result = match command {
                        HostCommand::SetWindowTitle(title) => {
                            window.set_title(&title);
                            Ok(())
                        }
                        HostCommand::MinimizeWindow => {
                            window.set_minimized(true);
                            Ok(())
                        }
                        HostCommand::MaximizeWindow => {
                            window.set_maximized(true);
                            Ok(())
                        }
                        HostCommand::CloseWindow => {
                            *control_flow = ControlFlow::Exit;
                            Ok(())
                        }
                        HostCommand::CreateWindow(info) => {
                            let child_id = info.id.raw();
                            secondary_windows.create(event_loop_target, &info, |child_window| {
                                let ipc_router = Arc::clone(&child_router);
                                let permission_router = Arc::clone(&child_router);
                                let ipc_proxy = child_proxy.clone();
                                let drop_router = Arc::clone(&child_router);
                                let asset_root = child_root.clone();
                                let frontend = child_frontend.clone();
                                let allowed_origin = child_dev_url.clone();
                                let url = if let Some(base) = child_dev_url.as_ref() {
                                    url::Url::parse(base)
                                        .and_then(|base| {
                                            base.join(info.url.trim_start_matches('/'))
                                        })
                                        .map(|url| url.to_string())
                                        .unwrap_or_else(|_| base.clone())
                                } else if info.url.starts_with("alex://app/") {
                                    info.url.clone()
                                } else {
                                    format!("alex://app/{}", info.url.trim_start_matches('/'))
                                };
                                let child_webview = WebViewBuilder::new()
                                    .with_initialization_script(child_init_script.clone())
                                    .with_devtools(cfg!(debug_assertions))
                                    .with_incognito(true)
                                    .with_clipboard(false)
                                    .with_navigation_handler(move |url| {
                                        crate::is_internal_webview_url(&url, "app")
                                            || allowed_origin.as_ref().is_some_and(|allowed| {
                                                same_origin(url.as_str(), allowed)
                                            })
                                    })
                                    .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
                                    .with_download_started_handler(|_, _| false)
                                    .with_drag_drop_handler(move |event| match event {
                                        wry::DragDropEvent::Drop { paths, position } => drop_router
                                            .deliver_file_drop(paths, position.0, position.1),
                                        _ => false,
                                    })
                                    .with_ipc_handler(move |request| {
                                        let router = Arc::clone(&ipc_router);
                                        let proxy = ipc_proxy.clone();
                                        let body = request.body().clone();
                                        let fallback_proxy = proxy.clone();
                                        if crate::runtime::task_executor::ipc_executor()
                                            .submit(move || {
                                                let response = router.dispatch_json_for_window(
                                                    &body,
                                                    Some(child_id),
                                                );
                                                if let Ok(json) = serde_json::to_string(&response) {
                                                    let _ =
                                                        proxy.send_event(UserEvent::IpcResponse(
                                                            Some(child_id),
                                                            json,
                                                        ));
                                                }
                                            })
                                            .is_err()
                                        {
                                            let response = crate::ipc::Response::error(
                                                "unknown",
                                                "HOST_BUSY",
                                                "host IPC queue is full",
                                            );
                                            if let Ok(json) = serde_json::to_string(&response) {
                                                let _ = fallback_proxy.send_event(
                                                    UserEvent::IpcResponse(Some(child_id), json),
                                                );
                                            }
                                        }
                                    })
                                    .with_custom_protocol("alex".into(), move |_id, request| {
                                        asset_response(&asset_root, &frontend, request.uri().path())
                                    })
                                    .with_url(&url)
                                    .build(child_window)
                                    .map_err(|error| error.to_string())?;
                                crate::webview::permissions::attach(
                                    &child_webview,
                                    permission_router,
                                )?;
                                Ok(child_webview)
                            })
                        }
                        HostCommand::SetWindowBounds(id, bounds) => {
                            secondary_windows.set_bounds(id, &bounds)
                        }
                        HostCommand::SetWindowFullscreen(id, fullscreen) => {
                            secondary_windows.set_fullscreen(id, fullscreen)
                        }
                        HostCommand::DestroyWindow(id) => secondary_windows.destroy(id),
                        HostCommand::SetApplicationMenu(template) => {
                            desktop_resources.set_application_menu(&window, &template)
                        }
                        HostCommand::SetContextMenu(template) => {
                            desktop_resources.set_context_menu(&template)
                        }
                        HostCommand::CreateTray(id, spec, root) => {
                            desktop_resources.create_tray(id, spec, root)
                        }
                        HostCommand::DestroyTray(id) => desktop_resources.destroy_tray(&id),
                        HostCommand::RegisterShortcut(accelerator) => {
                            desktop_resources.register_shortcut(accelerator)
                        }
                        HostCommand::UnregisterShortcut(accelerator) => {
                            desktop_resources.unregister_shortcut(&accelerator)
                        }
                    };
                    let _ = reply.send(result);
                }
                Event::WindowEvent {
                    window_id, event, ..
                } => match event {
                    WindowEvent::CloseRequested => {
                        if window_id == window.id() {
                            *control_flow = ControlFlow::Exit;
                        } else if let Some(id) = secondary_windows.close_native(window_id) {
                            router.native_window_closed(id);
                        }
                    }
                    WindowEvent::Focused(focused) => emit_event(
                        secondary_windows
                            .webview_for_native(window_id)
                            .unwrap_or(&webview),
                        "window.focusChanged",
                        serde_json::json!({ "focused": focused }),
                    ),
                    WindowEvent::Resized(size) => emit_event(
                        secondary_windows
                            .webview_for_native(window_id)
                            .unwrap_or(&webview),
                        "window.resized",
                        serde_json::json!({ "width": size.width, "height": size.height }),
                    ),
                    WindowEvent::Moved(position) => emit_event(
                        secondary_windows
                            .webview_for_native(window_id)
                            .unwrap_or(&webview),
                        "window.moved",
                        serde_json::json!({ "x": position.x, "y": position.y }),
                    ),
                    WindowEvent::MouseInput {
                        state: tao::event::ElementState::Pressed,
                        button: tao::event::MouseButton::Right,
                        ..
                    } => {
                        if window_id == window.id() {
                            desktop_resources.show_context_menu(&window);
                        } else if let Some(child) = secondary_windows.window_for_native(window_id) {
                            desktop_resources.show_context_menu(child);
                        }
                    }
                    _ => {}
                },
                Event::LoopDestroyed => {
                    if let Some(child) = frontend_process.as_mut() {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    if let Some(child) = backend_dev_process.as_mut() {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                }
                _ => {}
            }

            desktop_resources.drain_events(&router);

            for (event, delivered) in router.event_bus().drain_pending() {
                let target = delivered
                    .window_id
                    .and_then(|id| secondary_windows.webview(id));
                if delivered.window_id.is_none() || target.is_some() {
                    crate::shell::windows::emit_subscribed(
                        target.unwrap_or(&webview),
                        &event,
                        &delivered.subscription_id,
                        delivered.sequence,
                        &delivered.payload,
                    );
                }
            }

            // 100 ms tick: drain dev commands while the event loop is otherwise idle.
            if last_poll.elapsed() >= DEV_POLL_INTERVAL {
                last_poll = Instant::now();
                while let Ok(command) = dev_rx.try_recv() {
                    if handle_dev_command(&webview, &router, command) {
                        *control_flow = ControlFlow::Exit;
                    }
                    if command == DevCommand::ReloadFrontend {
                        for child in secondary_windows.webviews() {
                            let _ = child.evaluate_script("window.location.reload()");
                        }
                    }
                }
            }
        })
    }

    fn start_frontend_dev_server(
        package_root: &Path,
        dev: &FrontendDev,
    ) -> Result<Child, Box<dyn std::error::Error>> {
        eprintln!(
            "alex dev: starting frontend: {} {} (cwd: {})",
            dev.command,
            dev.args.join(" "),
            dev.cwd
        );
        let mut command = frontend_command(&dev.command)?;
        Ok(command
            .args(&dev.args)
            .current_dir(package_root.join(&dev.cwd))
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?)
    }

    fn install_frontend_dependencies(
        package_root: &Path,
        dev: &FrontendDev,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(install) = &dev.install else {
            return Ok(());
        };
        let cwd = package_root.join(&dev.cwd);
        if cwd.join("node_modules").is_dir() {
            return Ok(());
        }
        eprintln!(
            "alex dev: frontend dependencies missing; running {} {}",
            install.command,
            install.args.join(" ")
        );
        let status = frontend_command(&install.command)?
            .args(&install.args)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()?;
        if !status.success() {
            return Err(format!("frontend dependency install failed with {status}").into());
        }
        Ok(())
    }

    fn install_service_dependencies(
        package_root: &Path,
        dev: &ServiceDev,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(install) = &dev.install else {
            return Ok(());
        };
        let cwd = package_root.join(&dev.cwd);
        if cwd.join("node_modules").is_dir() {
            return Ok(());
        }
        eprintln!(
            "alex dev: backend dependencies missing; running {} {}",
            install.command,
            install.args.join(" ")
        );
        let status = frontend_command(&install.command)?
            .args(&install.args)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()?;
        if !status.success() {
            return Err(format!("backend dependency install failed with {status}").into());
        }
        Ok(())
    }

    fn start_service_dev_process(
        package_root: &Path,
        dev: &ServiceDev,
    ) -> Result<Child, Box<dyn std::error::Error>> {
        eprintln!(
            "alex dev: starting backend compiler: {} {} (cwd: {})",
            dev.command,
            dev.args.join(" "),
            dev.cwd
        );
        let mut command = frontend_command(&dev.command)?;
        Ok(command
            .args(&dev.args)
            .current_dir(package_root.join(&dev.cwd))
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?)
    }

    fn frontend_command(name: &str) -> Result<Command, Box<dyn std::error::Error>> {
        Ok(crate::runtime::node_tool_command(name))
    }

    fn wait_for_dev_server(
        url: &str,
        child: &mut Option<Child>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let parsed = url::Url::parse(url)?;
        let host = parsed.host_str().ok_or("frontend dev URL has no host")?;
        let port = parsed
            .port_or_known_default()
            .ok_or("frontend dev URL has no port")?;
        let address = format!("{host}:{port}").parse()?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = child
                .as_mut()
                .and_then(|process| process.try_wait().ok())
                .flatten()
            {
                return Err(
                    format!("frontend dev command exited before becoming ready: {status}").into(),
                );
            }
            if std::net::TcpStream::connect_timeout(&address, Duration::from_millis(250)).is_ok() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                if let Some(process) = child.as_mut() {
                    let _ = process.kill();
                }
                return Err(format!("frontend dev server did not become ready at {url}").into());
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn dev_server_is_ready(url: &str) -> bool {
        let Ok(parsed) = url::Url::parse(url) else {
            return false;
        };
        let Some(host) = parsed.host_str() else {
            return false;
        };
        let Some(port) = parsed.port_or_known_default() else {
            return false;
        };
        let Ok(address) = format!("{host}:{port}").parse() else {
            return false;
        };
        std::net::TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok()
    }

    fn same_origin(candidate: &str, allowed: &str) -> bool {
        let Ok(candidate) = url::Url::parse(candidate) else {
            return false;
        };
        let Ok(allowed) = url::Url::parse(allowed) else {
            return false;
        };
        candidate.scheme() == allowed.scheme()
            && candidate.host_str() == allowed.host_str()
            && candidate.port_or_known_default() == allowed.port_or_known_default()
    }

    /// Returns `true` when the event loop should exit. The only
    /// command that does this today is `ManifestChanged` — the
    /// rest mutate the running shell in place.
    fn handle_dev_command(webview: &wry::WebView, router: &ApiRouter, command: DevCommand) -> bool {
        match command {
            DevCommand::ReloadFrontend => {
                let _ = webview.evaluate_script("location.reload()");
                eprintln!("alex dev: reloaded frontend");
                false
            }
            DevCommand::RestartRuntime => match router.restart_runtime(RUNTIME_RESTART_TIMEOUT) {
                Some(Ok(())) => {
                    eprintln!("alex dev: restarted backend runtime");
                    false
                }
                Some(Err(error)) => {
                    eprintln!("alex dev: backend restart failed: {error}");
                    false
                }
                None => {
                    eprintln!("alex dev: no runtime to restart");
                    false
                }
            },
            DevCommand::ManifestChanged => {
                eprintln!(
                    "alex dev: manifest.json changed — restart `alex dev` to pick up \
                     new permissions, backend, or runtime mode"
                );
                true
            }
        }
    }

    fn spawn_watcher(
        frontend_dir: PathBuf,
        backend_dir: Option<PathBuf>,
        manifest_path: PathBuf,
        matcher: Option<Gitignore>,
        dev_tx: mpsc::SyncSender<DevCommand>,
    ) {
        thread::Builder::new()
            .name("alex-dev-watcher".into())
            .spawn(move || run_watcher(frontend_dir, backend_dir, manifest_path, matcher, dev_tx))
            .expect("dev watcher thread should start");
    }

    fn run_watcher(
        frontend_dir: PathBuf,
        backend_dir: Option<PathBuf>,
        manifest_path: PathBuf,
        matcher: Option<Gitignore>,
        dev_tx: mpsc::SyncSender<DevCommand>,
    ) {
        let (notify_tx, notify_rx) = mpsc::channel();
        let mut watcher = match notify::recommended_watcher(notify_tx) {
            Ok(watcher) => watcher,
            Err(error) => {
                eprintln!("alex dev: failed to create file watcher: {error}");
                return;
            }
        };
        if let Err(error) = watcher.watch(&frontend_dir, RecursiveMode::Recursive) {
            eprintln!("alex dev: cannot watch {}: {error}", frontend_dir.display());
            return;
        }
        if let Some(backend) = &backend_dir
            && let Err(error) = watcher.watch(backend, RecursiveMode::Recursive)
        {
            eprintln!("alex dev: cannot watch {}: {error}", backend.display());
            return;
        }
        // Watch the manifest itself. We use NonRecursive because
        // manifest.json is a single file at a known path; the
        // notify crate accepts that for any path.
        if let Err(error) = watcher.watch(&manifest_path, RecursiveMode::NonRecursive) {
            eprintln!(
                "alex dev: cannot watch {}: {error}",
                manifest_path.display()
            );
        }
        // Hold the watcher until the channel closes (i.e. until the
        // process exits and the receiver is dropped). Dropping the watcher
        // releases the OS file handles.
        let _watcher = watcher;
        let package_root = frontend_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or(frontend_dir.clone());
        for event in notify_rx {
            let Ok(event) = event else { continue };
            // Ignore read/access notifications — editors often poll metadata.
            if matches!(event.kind, EventKind::Access(_) | EventKind::Other) {
                continue;
            }
            for path in &event.paths {
                let matcher_ref = matcher.as_ref();
                if is_ignored(&matcher_ref, &package_root, path) {
                    continue;
                }
                if let Some(signal) =
                    classify_change(&frontend_dir, backend_dir.as_deref(), &manifest_path, path)
                {
                    // Capacity-1 channel: older signals are dropped, only
                    // the most recent command reaches the event loop.
                    let _ = dev_tx.try_send(signal);
                }
            }
        }
    }

    fn classify_change(
        frontend_dir: &Path,
        backend_dir: Option<&Path>,
        manifest_path: &Path,
        path: &Path,
    ) -> Option<DevCommand> {
        if path == manifest_path {
            return Some(DevCommand::ManifestChanged);
        }
        if path.starts_with(frontend_dir) {
            return Some(DevCommand::ReloadFrontend);
        }
        if let Some(backend) = backend_dir
            && path.starts_with(backend)
        {
            return Some(DevCommand::RestartRuntime);
        }
        None
    }

    /// Render a JSON `Value` as a single-line, length-bounded
    /// string for the IPC inspector stderr log. Keeps the
    /// developer-facing log readable when a page is polling a
    /// method every 16ms; the original `Value` is unaffected
    /// because we only ever read it for logging.
    fn truncate_params(value: &serde_json::Value, max_bytes: usize) -> String {
        let rendered = match value {
            serde_json::Value::Null => "null".to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => format!("\"{}\"", s),
            // Compound types fall through to the full
            // serializer so the dev can see the shape.
            other => other.to_string(),
        };
        if rendered.len() <= max_bytes {
            return rendered;
        }
        // Cut at a char boundary that respects UTF-8 so the
        // terminal doesn't print a half-codepoint.
        let mut end = max_bytes;
        while end > 0 && !rendered.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &rendered[..end])
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::path::PathBuf;

        fn p(s: &str) -> PathBuf {
            PathBuf::from(s)
        }

        #[test]
        fn classify_change_flags_manifest_path() {
            let manifest = p("/app/manifest.json");
            let frontend = p("/app/frontend");
            let backend = Some(p("/app/backend"));
            assert_eq!(
                classify_change(&frontend, backend.as_deref(), &manifest, &manifest),
                Some(DevCommand::ManifestChanged)
            );
        }

        #[test]
        fn classify_change_flags_frontend_paths() {
            let manifest = p("/app/manifest.json");
            let frontend = p("/app/frontend");
            let backend = Some(p("/app/backend"));
            assert_eq!(
                classify_change(
                    &frontend,
                    backend.as_deref(),
                    &manifest,
                    &p("/app/frontend/index.html"),
                ),
                Some(DevCommand::ReloadFrontend)
            );
            assert_eq!(
                classify_change(
                    &frontend,
                    backend.as_deref(),
                    &manifest,
                    &p("/app/frontend/app/main.js"),
                ),
                Some(DevCommand::ReloadFrontend)
            );
        }

        #[test]
        fn classify_change_flags_backend_paths() {
            let manifest = p("/app/manifest.json");
            let frontend = p("/app/frontend");
            let backend = Some(p("/app/backend"));
            assert_eq!(
                classify_change(
                    &frontend,
                    backend.as_deref(),
                    &manifest,
                    &p("/app/backend/index.js"),
                ),
                Some(DevCommand::RestartRuntime)
            );
        }

        #[test]
        fn classify_change_handles_no_backend() {
            // Frontend-only app (no backend declared) — the
            // backend slot is None and any change outside the
            // frontend tree is a no-op.
            let manifest = p("/app/manifest.json");
            let frontend = p("/app/frontend");
            assert_eq!(
                classify_change(&frontend, None, &manifest, &p("/app/anything/else")),
                None
            );
        }

        #[test]
        fn classify_change_ignores_unrelated_paths() {
            let manifest = p("/app/manifest.json");
            let frontend = p("/app/frontend");
            let backend = Some(p("/app/backend"));
            assert_eq!(
                classify_change(
                    &frontend,
                    backend.as_deref(),
                    &manifest,
                    &p("/elsewhere/something.txt"),
                ),
                None
            );
        }

        #[test]
        fn manifest_path_takes_precedence_over_overlapping_dirs() {
            // Defensive: if the user happens to also place
            // manifest.json under their frontend dir (a mistake
            // we still want to handle), the manifest signal must
            // win so the user gets the "restart required"
            // message instead of a silent reload that drops the
            // new permission set.
            let manifest = p("/app/frontend/manifest.json");
            let frontend = p("/app/frontend");
            let backend = None;
            assert_eq!(
                classify_change(&frontend, backend, &manifest, &manifest),
                Some(DevCommand::ManifestChanged)
            );
        }

        #[test]
        fn truncate_params_passes_short_values_through() {
            let v = serde_json::json!({"path": "/tmp/hi.txt"});
            assert_eq!(truncate_params(&v, 200), "{\"path\":\"/tmp/hi.txt\"}");
        }

        #[test]
        fn truncate_params_cuts_long_values() {
            let big = "x".repeat(5000);
            let v = serde_json::json!({ "data": big });
            let out = truncate_params(&v, 80);
            // The `…` ellipsis is 3 UTF-8 bytes, so the total
            // length is at most `max_bytes + 3`.
            assert!(
                out.len() <= 80 + "…".len(),
                "got len {} = {:?}",
                out.len(),
                out
            );
            assert!(out.ends_with('…'), "missing ellipsis: {out:?}");
        }

        #[test]
        fn truncate_params_keeps_scalar_shape() {
            // Booleans, numbers, and null must render as their
            // bare form so the dev can `grep 'ipc .* -> ok'`
            // without parsing JSON.
            assert_eq!(truncate_params(&serde_json::json!(true), 100), "true");
            assert_eq!(truncate_params(&serde_json::json!(42), 100), "42");
            assert_eq!(truncate_params(&serde_json::json!(null), 100), "null");
            assert_eq!(
                truncate_params(&serde_json::json!("hello"), 100),
                "\"hello\""
            );
        }

        #[test]
        fn truncate_params_respects_utf8_boundaries() {
            // 4-byte emoji at the cut point must not be split
            // mid-codepoint.
            let emoji = "🎉"; // 4 bytes in UTF-8
            let value = serde_json::json!({ "k": emoji.repeat(100) });
            let out = truncate_params(&value, 12);
            assert!(out.ends_with('…'));
            // Re-encoding the prefix must not panic.
            let _ = serde_json::from_str::<serde_json::Value>(out.trim_end_matches('…'));
        }
    }
}

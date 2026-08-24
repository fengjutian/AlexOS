//! Application-level supervisor: one application, many services.
//!
//! The 0.1 `RuntimeSupervisor` (in `core/manager.rs`) was shaped
//! "one app → one process". Phase 2 of the multi-service roadmap
//! reshapes it to "one app → many services → many processes". This
//! module owns the higher-level orchestration:
//!
//! * `ApplicationSupervisor` holds N applications, each keyed by
//!   `app_id`. For every app it tracks a `desired` state, an
//!   `observed` aggregate state, and a `BTreeMap<String,
//!   ServiceRuntime>` of per-service slots.
//! * `ServiceRuntime` (from [`super::service_supervisor`]) holds the
//!   per-service state: spec, live handle, status, restart count,
//!   generation counter.
//! * The new service-level API (`start_service`, `stop_service`,
//!   `restart_service`, `service_status`, `list_services`) lives
//!   here; the legacy per-app API (`launch`, `stop`, `restart`,
//!   `status`, `snapshot`, `stop_and_forget`) is preserved as a
//!   thin wrapper that operates on the app's primary service.
//!
//! Phase 3 (DAG orchestration) will replace the "iterate services
//! in declaration order" loop with a layered start that respects
//! `depends_on`. Phase 2 only needs the data shape so the
//! supervisor can hold multiple services per app, plus the
//! per-service start/stop semantics the acceptance tests require.

use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::Serialize;
use thiserror::Error;

use crate::{
    core::{
        application_manifest::{ApplicationManifest, ServiceDescriptor, ServiceRestartPolicy},
        manifest::{Backend, BackendMode, HealthCheck, RestartPolicy, RuntimeKind},
    },
    runtime::{
        service_supervisor::{ServiceRuntime, ServiceStatus},
        supervisor::{RuntimeHandle, RuntimeSpec, RuntimeState, RuntimeStatus},
    },
};

/// Per-app `desired` state. The Daemon protocol and the App Manager
/// UI set this; the supervisor's `start_application` /
///
/// `stop_application` honour it. `Restart` would be a third state
/// in a long-lived deployment, but for Phase 2 callers always
/// issue an explicit `restart_application` so the supervisor does
/// not need a dedicated state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApplicationDesiredState {
    #[default]
    Stopped,
    Running,
}

/// Per-app `observed` state. The supervisor rolls up the
/// per-service states into a single value for the App Manager
/// list / detail views. The mapping is:
///
/// * all services `Healthy` → `Running`
/// * some services `Healthy` and the rest `Starting` / `Restarting`
///   → `Starting`
/// * some services `Crashed` and the rest not all `Stopped` →
///   `Degraded`
/// * all services terminal (`Stopped` / `Crashed` / `Blocked`) →
///   `Stopped` (or `Crashed` if any service is `Crashed` and not
///   `Blocked`)
/// * a stop is in progress → `Stopping`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApplicationObservedState {
    Starting,
    Running,
    Degraded,
    Stopping,
    #[default]
    Stopped,
    Crashed,
}

/// Snapshot of one service as the supervisor sees it. Used by the
/// Phase 5 daemon protocol and the Phase 6 manager UI. Phase 2
/// only fills the `status` / `pid` / `port` / `restart_count` /
/// `last_error` fields — `runtime` (mode / logs) is filled by the
/// compat layer that wraps this struct in a `RuntimeStatus`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceSnapshot {
    pub name: String,
    pub status: ServiceStatus,
    pub pid: Option<u32>,
    pub port: Option<u16>,
    pub restart_count: u32,
    pub last_exit_code: Option<i32>,
    pub last_error: Option<String>,
}

/// One application's worth of state: the set of services plus the
/// desired / observed rollup. The supervisor never mutates
/// `services` outside the lock; every accessor that returns
/// borrowed data holds the supervisor lock for the duration of
/// the borrow.
#[derive(Debug, Clone)]
pub struct ApplicationRuntime {
    pub app_id: String,
    pub services: BTreeMap<String, ServiceRuntime>,
    pub desired: ApplicationDesiredState,
    pub observed: ApplicationObservedState,
    /// Bumped on every `start_application` / `stop_application` /
    /// `restart_application` call. Late state from a previous
    /// generation is dropped by background status probes.
    pub generation: u64,
    /// Most recent supervisor-level error (start failure, missing
    /// service name, etc.). Service-level errors live on the
    /// `ServiceRuntime.last_error` field.
    pub last_error: Option<String>,
}

impl ApplicationRuntime {
    /// Public constructor used by the supervisor and by tests
    /// that want to pre-seed an `ApplicationRuntime` in a
    /// known state without going through `start_service`. The
    /// production path always uses `start_service` (which
    /// auto-creates the application on demand).
    pub fn new(app_id: String) -> Self {
        Self {
            app_id,
            services: BTreeMap::new(),
            desired: ApplicationDesiredState::Stopped,
            observed: ApplicationObservedState::Stopped,
            generation: 0,
            last_error: None,
        }
    }
}

/// The Phase 2 multi-service supervisor. Holds N applications,
/// each with N services. Cheap to clone (`Arc<Mutex<...>>` inner
/// state) so the Daemon can keep one handle and call into it
/// from any thread.
#[derive(Clone, Default)]
pub struct ApplicationSupervisor {
    applications: Arc<Mutex<BTreeMap<String, ApplicationRuntime>>>,
}

#[derive(Debug, Error)]
pub enum ApplicationSupervisorError {
    #[error("application {0} not found")]
    NotFound(String),
    #[error("application {app} service {service} is already running")]
    ServiceAlreadyRunning { app: String, service: String },
    #[error("application {app} service {service} not found")]
    ServiceNotFound { app: String, service: String },
    #[error("application {0} already running; stop it first")]
    ApplicationAlreadyRunning(String),
    #[error("v2 application launch is not supported in Phase 2: {0}")]
    V2LaunchNotSupported(String),
    #[error("service {service} runtime {runtime:?} is not supported in Phase 2: {message}")]
    RuntimeUnsupported {
        service: String,
        runtime: RuntimeKind,
        message: String,
    },
    #[error("runtime error: {0}")]
    Runtime(#[from] crate::runtime::RuntimeError),
}

impl ApplicationSupervisor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read-only access to the per-app state, for snapshot / list
    /// callers. Returns `None` for unknown apps.
    pub fn application(&self, app_id: &str) -> Option<ApplicationRuntime> {
        self.applications
            .lock()
            .expect("application supervisor lock poisoned")
            .get(app_id)
            .cloned()
    }

    /// `true` if the supervisor is currently holding a live
    /// handle for the given app's primary service (or any service,
    /// for v2). Used by the App Manager to decide whether the
    /// "start" button is enabled.
    pub fn is_application_running(&self, app_id: &str) -> bool {
        let guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        guard
            .get(app_id)
            .map(|app| app.services.values().any(|svc| svc.status.is_running()))
            .unwrap_or(false)
    }

    /// Pre-register an `ApplicationRuntime` with the supervisor.
    /// Each declared service gets a `Pending` slot ready for
    /// `start_service`. Returns the freshly created
    /// `ApplicationRuntime` clone so callers can hand it to
    /// the higher-level API without an extra lookup.
    ///
    /// Tests use this to seed known states (e.g. a "Healthy"
    /// primary service) before exercising the launch / stop
    /// paths. The production code path goes through
    /// `start_service` (which calls this internally on
    /// demand) — the public visibility exists for
    /// integration tests and the Phase 6 App Manager
    /// detail view.
    pub fn register_application(
        &self,
        app_id: &str,
        services: Vec<ServiceDescriptor>,
    ) -> ApplicationRuntime {
        let mut guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        let application = guard
            .entry(app_id.to_owned())
            .or_insert_with(|| ApplicationRuntime::new(app_id.to_owned()));
        for descriptor in services {
            application
                .services
                .entry(descriptor.name.clone())
                .or_insert_with(|| ServiceRuntime::new(descriptor));
        }
        application.clone()
    }

    /// Start every service declared in `manifest`. Phase 2 starts
    /// them in declaration order; Phase 3 will replace this with
    /// the layered DAG start.
    ///
    /// Returns the final observed state when the synchronous
    /// part of the start completes (i.e. every service has been
    /// spawned). For service-mode backends this blocks until the
    /// `alex.ready` handshake; for rpc mode it returns as soon
    /// as the process is up.
    pub fn start_application(
        &self,
        app_id: &str,
        install_root: &Path,
        manifest: &ApplicationManifest,
    ) -> Result<ApplicationObservedState, ApplicationSupervisorError> {
        let services = manifest.services();
        if services.is_empty() {
            return Err(ApplicationSupervisorError::V2LaunchNotSupported(
                "manifest declares no services; headless UI-only apps are not runnable".into(),
            ));
        }
        // Phase 2 only implements Node launch end-to-end. v2 apps
        // that declare Python or Native services fail with a
        // clear error; this lifts to a managed runtime in
        // Phase 7. v1 single-backend apps always project to a
        // Node service, so they keep working unchanged.
        for descriptor in &services {
            if !matches!(descriptor.runtime, crate::manifest_v2::ServiceRuntime::Node) {
                return Err(ApplicationSupervisorError::V2LaunchNotSupported(format!(
                    "service {} declares runtime {:?}; Phase 2 only supports Node",
                    descriptor.name, descriptor.runtime
                )));
            }
        }
        let mut guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        let application = guard.entry(app_id.to_owned()).or_insert_with(|| {
            ApplicationRuntime::new(app_id.to_owned())
        });
        if application
            .services
            .values()
            .any(|svc| svc.status.is_running())
        {
            return Err(ApplicationSupervisorError::ApplicationAlreadyRunning(
                app_id.to_owned(),
            ));
        }
        application.generation = application.generation.wrapping_add(1);
        application.observed = ApplicationObservedState::Starting;
        application.last_error = None;
        // Insert empty slots for every declared service first so
        // the supervisor can answer `list_services` mid-start.
        for descriptor in &services {
            application
                .services
                .entry(descriptor.name.clone())
                .or_insert_with(|| ServiceRuntime::new(descriptor.clone()));
        }
        drop(guard);
        // Start each service. A failure mid-way leaves earlier
        // services running (Phase 3 adds reverse-order rollback);
        // callers can `stop_application` to clean up. We still
        // return the error so the App Manager can surface it.
        let mut last_error: Option<String> = None;
        for descriptor in &services {
            if let Err(error) = self.start_service(app_id, &descriptor.name, install_root, descriptor)
            {
                last_error = Some(error.to_string());
                break;
            }
        }
        let mut guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        let application = guard
            .get_mut(app_id)
            .expect("application inserted above");
        application.observed = if let Some(error) = last_error.clone() {
            application.last_error = Some(error);
            ApplicationObservedState::Crashed
        } else {
            rollup_observed_state(&application.services)
        };
        application.desired = ApplicationDesiredState::Running;
        Ok(application.observed)
    }

    /// Start one service within an app. The supervisor inserts
    /// (or re-uses) a `ServiceRuntime` slot for `service_name` and
    /// spawns the child process. Returns the new
    /// `ServiceStatus`. The app itself is created on demand if
    /// it does not already exist; this lets the v1
    /// backward-compat launch path skip a separate
    /// `register_application` call.
    ///
    /// Fails with `ServiceAlreadyRunning` if the slot is already
    /// `Starting` / `Healthy` / `Unhealthy` / `Restarting`. Idempotent
    /// in the sense that re-starting a `Stopped` / `Crashed` /
    /// `Blocked` service is allowed.
    pub fn start_service(
        &self,
        app_id: &str,
        service_name: &str,
        install_root: &Path,
        spec: &ServiceDescriptor,
    ) -> Result<ServiceStatus, ApplicationSupervisorError> {
        let mut guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        let application = guard
            .entry(app_id.to_owned())
            .or_insert_with(|| ApplicationRuntime::new(app_id.to_owned()));
        let status = application
            .services
            .entry(service_name.to_owned())
            .or_insert_with(|| ServiceRuntime::new(spec.clone()))
            .status;
        if status.is_running() {
            return Err(ApplicationSupervisorError::ServiceAlreadyRunning {
                app: app_id.to_owned(),
                service: service_name.to_owned(),
            });
        }
        application.generation = application.generation.wrapping_add(1);
        let service = application
            .services
            .get_mut(service_name)
            .expect("service slot inserted above");
        service.spec = spec.clone();
        service.generation = service.generation.wrapping_add(1);
        service.restart_count = service.restart_count.wrapping_add(1);
        service.status = ServiceStatus::Starting;
        service.last_error = None;
        let backend = service_descriptor_to_backend(service_name, spec);
        let spec_for_launch = RuntimeSpec {
            app_id: app_id.to_owned(),
            package_root: install_root.to_path_buf(),
            backend,
            data_dir: None,
            cache_dir: None,
        };
        drop(guard);
        let handle = match RuntimeHandle::start_with_spec(spec_for_launch) {
            Ok(handle) => handle,
            Err(error) => {
                let mut guard = self
                    .applications
                    .lock()
                    .expect("application supervisor lock poisoned");
                if let Some(application) = guard.get_mut(app_id)
                    && let Some(service) = application.services.get_mut(service_name)
                {
                    service.status = ServiceStatus::Crashed;
                    service.last_error = Some(error.to_string());
                    service.consecutive_failures =
                        service.consecutive_failures.wrapping_add(1);
                }
                return Err(ApplicationSupervisorError::Runtime(error));
            }
        };
        let mut guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        let application = guard
            .get_mut(app_id)
            .ok_or_else(|| ApplicationSupervisorError::NotFound(app_id.to_owned()))?;
        let service = application
            .services
            .get_mut(service_name)
            .ok_or_else(|| ApplicationSupervisorError::ServiceNotFound {
                app: app_id.to_owned(),
                service: service_name.to_owned(),
            })?;
        service.handle = Some(handle);
        service.status = ServiceStatus::Healthy;
        Ok(service.status)
    }

    /// Stop a single service. Idempotent: stopping a
    /// `Stopped` / `Crashed` / `Blocked` service is a no-op
    /// (returns `Ok(ServiceStatus::Stopped)`) and does not
    /// error. The supervisor's existing graceful-then-forceful
    /// shutdown path on `RuntimeHandle::cancel` does the actual
    /// teardown.
    pub fn stop_service(
        &self,
        app_id: &str,
        service_name: &str,
    ) -> Result<ServiceStatus, ApplicationSupervisorError> {
        let mut guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        let application = guard
            .get_mut(app_id)
            .ok_or_else(|| ApplicationSupervisorError::NotFound(app_id.to_owned()))?;
        let service = application
            .services
            .get_mut(service_name)
            .ok_or_else(|| ApplicationSupervisorError::ServiceNotFound {
                app: app_id.to_owned(),
                service: service_name.to_owned(),
            })?;
        if service.status.is_terminal() {
            return Ok(service.status);
        }
        service.status = ServiceStatus::Stopping;
        let mut handle = service.handle.take();
        service.generation = service.generation.wrapping_add(1);
        drop(guard);
        if let Some(handle) = handle.as_mut() {
            handle.cancel();
            // Best-effort drain: the supervisor thread already
            // observes the cancel and tears the process down
            // through the same path `RuntimeSupervisor::stop`
            // uses in 0.1. A short status poll gives the watch
            // thread time to update `pid` / `state` before we
            // return.
            let _ = handle.status(Duration::from_millis(50));
        }
        let mut guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        let application = guard
            .get_mut(app_id)
            .ok_or_else(|| ApplicationSupervisorError::NotFound(app_id.to_owned()))?;
        let service = application
            .services
            .get_mut(service_name)
            .ok_or_else(|| ApplicationSupervisorError::ServiceNotFound {
                app: app_id.to_owned(),
                service: service_name.to_owned(),
            })?;
        service.status = ServiceStatus::Stopped;
        service.handle = None;
        service.last_exit_code = None;
        Ok(service.status)
    }

    /// Restart one service: stop, then start with the same spec.
    /// Returns the new `ServiceStatus` (always `Healthy` on
    /// success).
    pub fn restart_service(
        &self,
        app_id: &str,
        service_name: &str,
        install_root: &Path,
    ) -> Result<ServiceStatus, ApplicationSupervisorError> {
        let spec = {
            let guard = self
                .applications
                .lock()
                .expect("application supervisor lock poisoned");
            let application = guard
                .get(app_id)
                .ok_or_else(|| ApplicationSupervisorError::NotFound(app_id.to_owned()))?;
            application
                .services
                .get(service_name)
                .ok_or_else(|| ApplicationSupervisorError::ServiceNotFound {
                    app: app_id.to_owned(),
                    service: service_name.to_owned(),
                })?
                .spec
                .clone()
        };
        self.stop_service(app_id, service_name)?;
        self.start_service(app_id, service_name, install_root, &spec)
    }

    /// Snapshot one service.
    pub fn service_status(
        &self,
        app_id: &str,
        service_name: &str,
    ) -> Result<ServiceSnapshot, ApplicationSupervisorError> {
        let guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        let application = guard
            .get(app_id)
            .ok_or_else(|| ApplicationSupervisorError::NotFound(app_id.to_owned()))?;
        let service = application
            .services
            .get(service_name)
            .ok_or_else(|| ApplicationSupervisorError::ServiceNotFound {
                app: app_id.to_owned(),
                service: service_name.to_owned(),
            })?;
        let (pid, port) = match service.handle.as_ref() {
            Some(handle) => match handle.status(Duration::from_millis(200)) {
                Ok(status) => (
                    status.pid,
                    if status.mode == BackendMode::Service {
                        status.port
                    } else {
                        None
                    },
                ),
                Err(_) => (None, None),
            },
            None => (None, None),
        };
        Ok(ServiceSnapshot {
            name: service.name.clone(),
            status: service.status,
            pid,
            port,
            restart_count: service.restart_count,
            last_exit_code: service.last_exit_code,
            last_error: service.last_error.clone(),
        })
    }

    /// List all service slots for an app, regardless of state.
    /// The order matches the `BTreeMap` key order (alphabetical),
    /// so callers do not have to sort.
    pub fn list_services(
        &self,
        app_id: &str,
    ) -> Result<Vec<ServiceSummary>, ApplicationSupervisorError> {
        let guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        let application = guard
            .get(app_id)
            .ok_or_else(|| ApplicationSupervisorError::NotFound(app_id.to_owned()))?;
        Ok(application
            .services
            .values()
            .map(|service| ServiceSummary {
                name: service.name.clone(),
                status: service.status,
                restart_count: service.restart_count,
                last_error: service.last_error.clone(),
            })
            .collect())
    }

    /// Stop every service in the app. Idempotent (each
    /// `stop_service` is itself idempotent).
    pub fn stop_application(
        &self,
        app_id: &str,
    ) -> Result<ApplicationObservedState, ApplicationSupervisorError> {
        let service_names: Vec<String> = {
            let guard = self
                .applications
                .lock()
                .expect("application supervisor lock poisoned");
            let application = guard
                .get(app_id)
                .ok_or_else(|| ApplicationSupervisorError::NotFound(app_id.to_owned()))?;
            application.services.keys().cloned().collect()
        };
        {
            let mut guard = self
                .applications
                .lock()
                .expect("application supervisor lock poisoned");
            if let Some(application) = guard.get_mut(app_id) {
                application.observed = ApplicationObservedState::Stopping;
            }
        }
        for name in &service_names {
            // `stop_service` returns `Ok` even when the service
            // is already terminal; we deliberately swallow that
            // success so a single non-running service does not
            // abort the whole `stop_application` loop.
            let _ = self.stop_service(app_id, name);
        }
        let mut guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        let application = guard
            .get_mut(app_id)
            .ok_or_else(|| ApplicationSupervisorError::NotFound(app_id.to_owned()))?;
        application.desired = ApplicationDesiredState::Stopped;
        application.observed = rollup_observed_state(&application.services);
        Ok(application.observed)
    }

    /// Restart every service of the app. Phase 2 stops all then
    /// starts all in declaration order; Phase 3 will preserve
    /// dependency order during both phases.
    pub fn restart_application(
        &self,
        app_id: &str,
        install_root: &Path,
    ) -> Result<ApplicationObservedState, ApplicationSupervisorError> {
        let specs: Vec<ServiceDescriptor> = {
            let guard = self
                .applications
                .lock()
                .expect("application supervisor lock poisoned");
            let application = guard
                .get(app_id)
                .ok_or_else(|| ApplicationSupervisorError::NotFound(app_id.to_owned()))?;
            application.services.values().map(|svc| svc.spec.clone()).collect()
        };
        for spec in &specs {
            let _ = self.stop_service(app_id, &spec.name);
        }
        for spec in &specs {
            self.start_service(app_id, &spec.name, install_root, spec)?;
        }
        let guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        let application = guard
            .get(app_id)
            .ok_or_else(|| ApplicationSupervisorError::NotFound(app_id.to_owned()))?;
        Ok(application.observed)
    }

    /// Roll up the per-service state into an `ApplicationObservedState`.
    pub fn application_status(
        &self,
        app_id: &str,
    ) -> Result<ApplicationObservedState, ApplicationSupervisorError> {
        let guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        let application = guard
            .get(app_id)
            .ok_or_else(|| ApplicationSupervisorError::NotFound(app_id.to_owned()))?;
        Ok(application.observed)
    }

    /// Backward-compatible single-process view. Returns the
    /// `RuntimeStatus` of the app's primary service (the service
    /// named `main` for v1 apps, the first declared service for
    /// v2). Returns a `Stopped` `RuntimeStatus` if the app is
    /// not currently running. This is the only public surface
    /// `manager::RuntimeSupervisor` needs to preserve its 0.1
    /// callers (Daemon, App Manager, shell).
    pub fn runtime_status_compat(&self, app_id: &str) -> Option<RuntimeStatus> {
        let guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        let application = guard.get(app_id)?;
        // v1 convention: the primary service is named "main".
        // v2 apps that have not been launched yet still return
        // a snapshot for the App Manager list view, so we fall
        // back to whichever service is currently live, then to
        // the first declared service.
        let primary_name = if application.services.contains_key("main") {
            "main".to_owned()
        } else {
            application
                .services
                .values()
                .find(|svc| svc.status.is_running())
                .map(|svc| svc.name.clone())
                .or_else(|| application.services.keys().next().cloned())?
        };
        let service = application.services.get(&primary_name)?;
        let mut status = match service.handle.as_ref() {
            Some(handle) => handle.status(Duration::from_millis(200)).unwrap_or_default(),
            None => RuntimeStatus::default(),
        };
        status.state = match service.status {
            ServiceStatus::Pending | ServiceStatus::WaitingForDependencies => {
                RuntimeState::Stopped
            }
            ServiceStatus::Starting => RuntimeState::Starting,
            ServiceStatus::Healthy | ServiceStatus::Unhealthy | ServiceStatus::Restarting => {
                if status.state == RuntimeState::Stopped {
                    RuntimeState::Running
                } else {
                    status.state
                }
            }
            ServiceStatus::Stopping => RuntimeState::Stopped,
            ServiceStatus::Stopped => RuntimeState::Stopped,
            ServiceStatus::Crashed => RuntimeState::Crashed,
            ServiceStatus::Blocked => RuntimeState::Stopped,
        };
        status.restart_count = service.restart_count;
        status.last_error = service.last_error.clone();
        Some(status)
    }

    /// Forget every running service for `app_id` without
    /// attempting a graceful shutdown. Used by `uninstall` so the
    /// next install of the same id can start with a clean
    /// supervisor slot.
    pub fn forget_application(&self, app_id: &str) {
        let mut guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        if let Some(mut application) = guard.remove(app_id) {
            for (_, service) in application.services.iter_mut() {
                if let Some(handle) = service.handle.as_mut() {
                    handle.cancel();
                }
            }
        }
    }

    /// Test-only hook: force a service slot into a specific
    /// status without going through the spawn path. The
    /// production code path uses `start_service` to transition
    /// slots; this method exists so integration tests can
    /// pre-seed the supervisor in a known state (e.g. a
    /// "Healthy" service for the duplicate-start test).
    pub fn set_service_status(
        &self,
        app_id: &str,
        service_name: &str,
        status: ServiceStatus,
    ) -> bool {
        let mut guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        if let Some(application) = guard.get_mut(app_id)
            && let Some(service) = application.services.get_mut(service_name)
        {
            service.status = status;
            return true;
        }
        false
    }
}

/// Summary view used by `list_services` and the App Manager UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceSummary {
    pub name: String,
    pub status: ServiceStatus,
    pub restart_count: u32,
    pub last_error: Option<String>,
}

fn rollup_observed_state(
    services: &BTreeMap<String, ServiceRuntime>,
) -> ApplicationObservedState {
    if services.is_empty() {
        return ApplicationObservedState::Stopped;
    }
    let mut any_healthy = false;
    let mut any_starting = false;
    let mut any_stopping = false;
    let mut any_crashed = false;
    let mut any_alive = false;
    for service in services.values() {
        match service.status {
            ServiceStatus::Healthy | ServiceStatus::Unhealthy | ServiceStatus::Restarting => {
                any_healthy = true;
                any_alive = true;
            }
            ServiceStatus::Starting | ServiceStatus::WaitingForDependencies => {
                any_starting = true;
                any_alive = true;
            }
            ServiceStatus::Stopping => {
                any_stopping = true;
                any_alive = true;
            }
            ServiceStatus::Crashed => {
                any_crashed = true;
            }
            ServiceStatus::Stopped | ServiceStatus::Blocked | ServiceStatus::Pending => {}
        }
    }
    if any_stopping {
        return ApplicationObservedState::Stopping;
    }
    if !any_alive {
        // Every service is in a terminal state. If any of them
        // is `Crashed`, the app is `Crashed`; otherwise it is
        // simply `Stopped`.
        return if any_crashed {
            ApplicationObservedState::Crashed
        } else {
            ApplicationObservedState::Stopped
        };
    }
    if any_starting && !any_healthy {
        return ApplicationObservedState::Starting;
    }
    if any_crashed && any_healthy {
        return ApplicationObservedState::Degraded;
    }
    if any_healthy {
        return ApplicationObservedState::Running;
    }
    ApplicationObservedState::Degraded
}

/// Project a unified `ServiceDescriptor` onto the v1-shaped
/// `Backend` that the lower-level `RuntimeHandle` consumes. This
/// is the bridge between Phase 1's unified type and Phase 2's
/// multi-service shape.
///
/// Phase 2 only knows how to start `Node` services. Python /
/// Native descriptors still project to a `Backend` so the caller
/// can introspect them, but the supervisor refuses to start them
/// (`V2LaunchNotSupported`); Phase 7's managed runtime providers
/// replace this projection with native launch paths.
pub(crate) fn service_descriptor_to_backend(
    name: &str,
    spec: &ServiceDescriptor,
) -> Backend {
    let runtime = match spec.runtime {
        crate::manifest_v2::ServiceRuntime::Node => RuntimeKind::Node,
        crate::manifest_v2::ServiceRuntime::Python => RuntimeKind::Python,
        crate::manifest_v2::ServiceRuntime::Native => RuntimeKind::Native,
    };
    // Use the explicit `mode` from the descriptor, falling back
    // to `Service` when an HTTP health check is present (v2
    // manifests without an explicit mode field still get the
    // right behaviour). Rpc is the default for everything else.
    let mode = match spec.mode {
        crate::core::application_manifest::ServiceMode::Rpc => BackendMode::Rpc,
        crate::core::application_manifest::ServiceMode::Service => BackendMode::Service,
    };
    let health_check = spec.health.as_ref().and_then(|health| {
        if health.path.is_some() {
            Some(HealthCheck {
                path: health.path.clone().unwrap_or_else(|| "/health".into()),
                timeout_ms: health.timeout_ms,
            })
        } else {
            None
        }
    });
    let restart = RestartPolicy {
        policy: match spec.restart.policy {
            ServiceRestartPolicy::Never => "never".into(),
            ServiceRestartPolicy::OnFailure => "on-failure".into(),
            ServiceRestartPolicy::Always => "always".into(),
        },
        max_retries: spec.restart.max_retries,
    };
    let _ = name; // service name is consumed by the supervisor, not the Backend
    Backend {
        runtime,
        entry: spec.command.clone(),
        mode,
        health_check,
        restart: Some(restart),
        port: spec.port,
        args: spec.args.clone(),
        env: spec.env.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::application_manifest::{
        ServiceDescriptor, ServiceHealthDescriptor, ServiceHealthKind, ServiceRestartDescriptor,
        ServiceRestartPolicy,
    };
    use crate::core::manifest_v2::ServiceRuntime as V2Runtime;
    use std::path::PathBuf;

    fn node_service(name: &str, command: &str) -> ServiceDescriptor {
        use crate::core::application_manifest::ServiceMode;
        ServiceDescriptor {
            name: name.to_owned(),
            runtime: V2Runtime::Node,
            command: command.to_owned(),
            args: Vec::new(),
            depends_on: Vec::new(),
            env: std::collections::BTreeMap::new(),
            port: None,
            mode: ServiceMode::Rpc,
            health: None,
            restart: ServiceRestartDescriptor::default(),
        }
    }

    #[test]
    fn service_descriptor_to_backend_maps_fields() {
        use crate::core::application_manifest::ServiceMode;
        let mut env = std::collections::BTreeMap::new();
        env.insert("LOG".to_owned(), "info".to_owned());
        let mut spec = node_service("api", "server.js");
        spec.args = vec!["--port".into(), "8080".into()];
        spec.env = env.clone();
        spec.mode = ServiceMode::Service;
        spec.health = Some(ServiceHealthDescriptor {
            kind: ServiceHealthKind::Http,
            path: Some("/livez".into()),
            interval_ms: 1000,
            timeout_ms: 2000,
        });
        spec.restart = ServiceRestartDescriptor {
            policy: ServiceRestartPolicy::Always,
            max_retries: 9,
        };
        let backend = service_descriptor_to_backend("api", &spec);
        assert_eq!(backend.runtime, RuntimeKind::Node);
        assert_eq!(backend.entry, "server.js");
        assert_eq!(backend.args, vec!["--port", "8080"]);
        assert_eq!(backend.env.get("LOG").map(String::as_str), Some("info"));
        assert_eq!(backend.mode, BackendMode::Service);
        let health = backend.health_check.expect("health projected");
        assert_eq!(health.path, "/livez");
        assert_eq!(health.timeout_ms, 2000);
        let restart = backend.restart.expect("restart projected");
        assert_eq!(restart.policy, "always");
        assert_eq!(restart.max_retries, 9);
    }

    #[test]
    fn rollup_state_running_when_all_healthy() {
        let mut services = BTreeMap::new();
        for name in ["a", "b", "c"] {
            let mut svc = ServiceRuntime::new(node_service(name, "x.js"));
            svc.status = ServiceStatus::Healthy;
            services.insert(name.to_owned(), svc);
        }
        assert_eq!(rollup_observed_state(&services), ApplicationObservedState::Running);
    }

    #[test]
    fn rollup_state_degraded_when_some_crashed() {
        let mut services = BTreeMap::new();
        for (name, status) in [("a", ServiceStatus::Healthy), ("b", ServiceStatus::Crashed)] {
            let mut svc = ServiceRuntime::new(node_service(name, "x.js"));
            svc.status = status;
            services.insert(name.to_owned(), svc);
        }
        assert_eq!(
            rollup_observed_state(&services),
            ApplicationObservedState::Degraded
        );
    }

    #[test]
    fn rollup_state_starting_when_all_starting() {
        let mut services = BTreeMap::new();
        for name in ["a", "b"] {
            let mut svc = ServiceRuntime::new(node_service(name, "x.js"));
            svc.status = ServiceStatus::Starting;
            services.insert(name.to_owned(), svc);
        }
        assert_eq!(
            rollup_observed_state(&services),
            ApplicationObservedState::Starting
        );
    }

    #[test]
    fn rollup_state_stopped_when_all_terminal() {
        let mut services = BTreeMap::new();
        for (name, status) in [
            ("a", ServiceStatus::Stopped),
            ("b", ServiceStatus::Crashed),
        ] {
            let mut svc = ServiceRuntime::new(node_service(name, "x.js"));
            svc.status = status;
            services.insert(name.to_owned(), svc);
        }
        assert_eq!(
            rollup_observed_state(&services),
            ApplicationObservedState::Crashed
        );
    }

    #[test]
    fn stop_service_on_terminal_service_is_idempotent() {
        let supervisor = ApplicationSupervisor::new();
        {
            let mut guard = supervisor
                .applications
                .lock()
                .expect("lock poisoned");
            let mut app = ApplicationRuntime::new("com.example.idempotent".into());
            let mut svc = ServiceRuntime::new(node_service("main", "x.js"));
            svc.status = ServiceStatus::Stopped;
            app.services.insert("main".to_owned(), svc);
            guard.insert("com.example.idempotent".to_owned(), app);
        }
        let result = supervisor.stop_service("com.example.idempotent", "main");
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(result.unwrap(), ServiceStatus::Stopped);
    }

    #[test]
    fn duplicate_start_service_errors() {
        let supervisor = ApplicationSupervisor::new();
        {
            let mut guard = supervisor
                .applications
                .lock()
                .expect("lock poisoned");
            let mut app = ApplicationRuntime::new("com.example.dup".into());
            let mut svc = ServiceRuntime::new(node_service("main", "x.js"));
            svc.status = ServiceStatus::Healthy;
            app.services.insert("main".to_owned(), svc);
            guard.insert("com.example.dup".to_owned(), app);
        }
        let err = supervisor
            .start_service(
                "com.example.dup",
                "main",
                Path::new("."),
                &node_service("main", "x.js"),
            )
            .unwrap_err();
        assert!(matches!(
            err,
            ApplicationSupervisorError::ServiceAlreadyRunning { .. }
        ));
    }

    #[test]
    fn same_service_name_in_different_apps_does_not_collide() {
        let supervisor = ApplicationSupervisor::new();
        // App A has a "main" service in slot Pending; App B has
        // a "main" service in slot Healthy. They share the
        // service name but live in separate `ApplicationRuntime`
        // records, so the supervisor answers them independently.
        for (id, status) in [
            ("com.example.alpha", ServiceStatus::Pending),
            ("com.example.beta", ServiceStatus::Healthy),
        ] {
            let mut guard = supervisor
                .applications
                .lock()
                .expect("lock poisoned");
            let mut app = ApplicationRuntime::new(id.to_owned());
            let mut svc = ServiceRuntime::new(node_service("main", "x.js"));
            svc.status = status;
            app.services.insert("main".to_owned(), svc);
            guard.insert(id.to_owned(), app);
        }
        assert!(!supervisor.is_application_running("com.example.alpha"));
        assert!(supervisor.is_application_running("com.example.beta"));
        let alpha = supervisor
            .service_status("com.example.alpha", "main")
            .expect("alpha main");
        let beta = supervisor
            .service_status("com.example.beta", "main")
            .expect("beta main");
        assert_eq!(alpha.status, ServiceStatus::Pending);
        assert_eq!(beta.status, ServiceStatus::Healthy);
    }

    #[test]
    fn forget_application_drops_all_service_slots() {
        let supervisor = ApplicationSupervisor::new();
        {
            let mut guard = supervisor
                .applications
                .lock()
                .expect("lock poisoned");
            let mut app = ApplicationRuntime::new("com.example.forget".into());
            for name in ["a", "b", "c"] {
                app.services
                    .insert(name.to_owned(), ServiceRuntime::new(node_service(name, "x.js")));
            }
            guard.insert("com.example.forget".to_owned(), app);
        }
        assert!(supervisor.application("com.example.forget").is_some());
        supervisor.forget_application("com.example.forget");
        assert!(supervisor.application("com.example.forget").is_none());
    }

    #[test]
    fn application_supervisor_clone_shares_state() {
        let supervisor = ApplicationSupervisor::new();
        let clone = supervisor.clone();
        {
            let mut guard = supervisor
                .applications
                .lock()
                .expect("lock poisoned");
            let mut app = ApplicationRuntime::new("com.example.shared".into());
            app.services.insert(
                "main".to_owned(),
                ServiceRuntime::new(node_service("main", "x.js")),
            );
            guard.insert("com.example.shared".to_owned(), app);
        }
        assert!(clone.application("com.example.shared").is_some());
    }

    #[test]
    fn start_application_rejects_non_node_runtimes_for_now() {
        let supervisor = ApplicationSupervisor::new();
        // Build a v2 manifest with a Python service to drive
        // the runtime check. `start_application` must refuse
        // it before the service slot is even created.
        use crate::core::manifest_v2::{
            ApplicationManifestV2, RuntimeRequirements, ServiceSpec,
        };
        let mut services = BTreeMap::new();
        services.insert(
            "worker".to_owned(),
            ServiceSpec {
                runtime: V2Runtime::Python,
                command: "worker.py".into(),
                args: Vec::new(),
                depends_on: Vec::new(),
                env: BTreeMap::new(),
                port: None,
                health: None,
                restart: Default::default(),
            },
        );
        let manifest = ApplicationManifestV2 {
            schema_version: 2,
            id: "com.example.python".into(),
            name: "python".into(),
            version: "0.1.0".into(),
            frontend: None,
            runtime: RuntimeRequirements {
                node: None,
                python: Some("3.12".into()),
            },
            services,
            storage: Vec::new(),
            permissions: Default::default(),
        };
        let unified = ApplicationManifest::V2(manifest);
        let result = supervisor.start_application("com.example.python", Path::new("."), &unified);
        match result {
            Err(ApplicationSupervisorError::V2LaunchNotSupported(message)) => {
                assert!(message.contains("Python"), "unexpected: {message}");
            }
            other => panic!("expected V2LaunchNotSupported, got {other:?}"),
        }
    }

    #[test]
    fn list_services_returns_all_slots_in_alphabetical_order() {
        let supervisor = ApplicationSupervisor::new();
        {
            let mut guard = supervisor
                .applications
                .lock()
                .expect("lock poisoned");
            let mut app = ApplicationRuntime::new("com.example.list".into());
            for name in ["zeta", "alpha", "mu"] {
                let mut svc = ServiceRuntime::new(node_service(name, "x.js"));
                svc.status = ServiceStatus::Pending;
                app.services.insert(name.to_owned(), svc);
            }
            guard.insert("com.example.list".to_owned(), app);
        }
        let list = supervisor
            .list_services("com.example.list")
            .expect("list services");
        let names: Vec<_> = list.iter().map(|svc| svc.name.clone()).collect();
        assert_eq!(names, vec!["alpha", "mu", "zeta"]);
    }

    #[test]
    fn runtime_status_compat_returns_stopped_for_unknown_app() {
        let supervisor = ApplicationSupervisor::new();
        assert!(supervisor.runtime_status_compat("nope").is_none());
    }

    #[test]
    fn application_status_unknown_app_errors() {
        let supervisor = ApplicationSupervisor::new();
        let err = supervisor
            .application_status("nope")
            .expect_err("not found");
        assert!(matches!(err, ApplicationSupervisorError::NotFound(_)));
    }

    #[test]
    fn start_application_without_node_manifest_succeeds_through_stub() {
        // Phase 2 only supports `Node` services end-to-end. This
        // test wires a service slot directly so we do not need a
        // Node binary on the test host; the `start_service` path
        // is exercised by the integration test in `tests/core.rs`.
        let supervisor = ApplicationSupervisor::new();
        let path = PathBuf::from(".");
        // Pre-register the app + service slot so the supervisor
        // sees an `ApplicationRuntime` for it.
        {
            let mut guard = supervisor
                .applications
                .lock()
                .expect("lock poisoned");
            guard.insert(
                "com.example.stub".into(),
                ApplicationRuntime::new("com.example.stub".into()),
            );
        }
        let snapshot = supervisor
            .service_status("com.example.stub", "main")
            .unwrap_err();
        assert!(matches!(
            snapshot,
            ApplicationSupervisorError::ServiceNotFound { .. }
        ));
        let _ = path; // silence unused
    }
}

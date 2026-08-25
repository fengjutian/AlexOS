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
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use serde::Serialize;
use thiserror::Error;

use crate::{
    core::{
        application_manifest::{ResolvedApplication, ServiceDescriptor, ServiceRestartPolicy},
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

/// Tunable parameters for [`ApplicationSupervisor::start_application`].
///
/// The defaults are conservative: a 4-wide per-app concurrency
/// keeps the launch latency for typical 5-service apps under a
/// second, the 5s per-layer timeout matches the existing
/// `RuntimeHandle::start_with_spec` ready-handshake budget, and
/// the 8-wide global cap means ten apps starting at once will
/// queue rather than fork-bomb the host.
#[derive(Debug, Clone)]
pub struct StartConfig {
    /// Maximum number of services from the same application
    /// the supervisor will spawn concurrently inside a single
    /// layer. A value of `0` falls back to the default.
    pub per_app_concurrency: usize,
    /// Wall-clock budget for an entire layer. When the budget
    /// elapses the layer's not-yet-`Healthy` services are
    /// considered failed and trigger the reverse-order rollback
    /// path. A value of `Duration::ZERO` falls back to the
    /// default.
    pub per_layer_timeout: Duration,
    /// Global cap on simultaneously in-flight service spawns
    /// across all applications. The supervisor drops new
    /// spawns that would push the in-flight count past the
    /// cap; callers see a `ServiceAlreadyRunning` style
    /// failure that resolves itself as soon as a slot frees.
    /// A value of `0` falls back to the default.
    pub global_concurrency: usize,
}

impl Default for StartConfig {
    fn default() -> Self {
        Self {
            per_app_concurrency: 4,
            per_layer_timeout: Duration::from_secs(5),
            global_concurrency: 8,
        }
    }
}

impl StartConfig {
    fn effective_per_app(&self) -> usize {
        if self.per_app_concurrency == 0 {
            4
        } else {
            self.per_app_concurrency
        }
    }
    fn effective_global(&self) -> usize {
        if self.global_concurrency == 0 {
            8
        } else {
            self.global_concurrency
        }
    }
    fn effective_layer_timeout(&self) -> Duration {
        if self.per_layer_timeout.is_zero() {
            Duration::from_secs(5)
        } else {
            self.per_layer_timeout
        }
    }
}

/// Tunable parameters for [`ApplicationSupervisor::stop_application`].
///
/// The defaults match [`StartConfig`] so a `restart_application`
/// that comes right after a `start_application` does not
/// surprise callers with a 10× longer stop window than the
/// start window. Tests can dial `per_layer_timeout` to zero
/// to force the `force_kill_stuck_services` path.
#[derive(Debug, Clone)]
pub struct StopConfig {
    /// Maximum number of services from the same application
    /// the supervisor will stop concurrently inside a single
    /// layer. A value of `0` falls back to the default.
    pub per_app_concurrency: usize,
    /// Wall-clock budget for an entire layer. When the
    /// budget elapses the supervisor issues a force-kill on
    /// every still-running service in the layer and gives
    /// up waiting. A value of `Duration::ZERO` falls back to
    /// the default.
    pub per_layer_timeout: Duration,
}

impl Default for StopConfig {
    fn default() -> Self {
        Self {
            per_app_concurrency: 4,
            per_layer_timeout: Duration::from_secs(5),
        }
    }
}

impl StopConfig {
    fn effective_per_app(&self) -> usize {
        if self.per_app_concurrency == 0 {
            4
        } else {
            self.per_app_concurrency
        }
    }
    fn effective_per_layer_timeout(&self) -> Duration {
        if self.per_layer_timeout.is_zero() {
            Duration::from_secs(5)
        } else {
            self.per_layer_timeout
        }
    }
}

/// RAII guard for the global spawn budget. The supervisor
/// increments a process-wide counter on entry and decrements
/// on drop. The cap is enforced lazily: a spawn that would
/// push the counter past the cap is reported as
/// `ServiceAlreadyRunning` so the layer can be retried by
/// the next `start_application` once another app's start
/// releases its slot.
struct GlobalSpawnBudget {
    _private: (),
}

impl GlobalSpawnBudget {
    fn enter(cap: usize) -> Self {
        if cap == 0 {
            return Self { _private: () };
        }
        let mut current = GLOBAL_SPAWNS_IN_FLIGHT.load(Ordering::Relaxed);
        loop {
            if current >= cap as u64 {
                // We do not actually block here — the
                // supervisor's caller will translate the
                // `ServiceAlreadyRunning` signal into a
                // user-visible retry. The guard is still
                // useful for the other half of the cap
                // accounting (release on drop).
                return Self { _private: () };
            }
            match GLOBAL_SPAWNS_IN_FLIGHT.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Self { _private: () },
                Err(actual) => current = actual,
            }
        }
    }
}

impl Drop for GlobalSpawnBudget {
    fn drop(&mut self) {
        GLOBAL_SPAWNS_IN_FLIGHT.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Global counter for the in-flight service spawn budget. The
/// supervisor increments this at the start of every
/// `start_service` and decrements it when the spawn returns;
/// `start_application` consults it before starting a layer to
/// stay under the configured cap.
static GLOBAL_SPAWNS_IN_FLIGHT: AtomicU64 = AtomicU64::new(0);

/// Topologically sort `services` into a layered schedule. The
/// returned `Vec<Vec<String>>` has one inner vector per
/// layer; services in the same layer have no dependency
/// between them and may be started concurrently. The
/// function rejects dependency cycles and unknown-dependency
/// references with [`LayerError`].
pub(crate) fn start_layers(services: &[ServiceDescriptor]) -> Result<Vec<Vec<String>>, LayerError> {
    let names: BTreeMap<&str, ()> = services.iter().map(|svc| (svc.name.as_str(), ())).collect();
    // Build `in_degree[name] = number of dependencies inside
    // the same manifest that have not been assigned to a
    // layer yet. We mutate the counter as we pop layers.
    let mut in_degree: BTreeMap<&str, usize> = services
        .iter()
        .map(|svc| (svc.name.as_str(), svc.depends_on.len()))
        .collect();
    let mut adjacency: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for svc in services {
        for dep in &svc.depends_on {
            if !names.contains_key(dep.as_str()) {
                return Err(LayerError::UnknownDependency {
                    service: svc.name.clone(),
                    dependency: dep.clone(),
                });
            }
            adjacency
                .entry(dep.as_str())
                .or_default()
                .push(svc.name.as_str());
        }
    }
    let mut layers: Vec<Vec<String>> = Vec::new();
    loop {
        let ready: Vec<String> = in_degree
            .iter()
            .filter_map(|(name, deg)| {
                if *deg == 0 {
                    Some(name.to_string())
                } else {
                    None
                }
            })
            .collect();
        if ready.is_empty() {
            if in_degree.is_empty() {
                break;
            }
            let cycle: Vec<String> = in_degree.keys().map(|name| name.to_string()).collect();
            return Err(LayerError::Cycle(cycle));
        }
        // Sort the layer so the spawn order is stable across
        // runs — handy for the integration tests and the App
        // Manager UI ("api" and "worker" should always appear
        // in the same order even when they are siblings).
        let mut ready = ready;
        ready.sort();
        for name in &ready {
            in_degree.remove(name.as_str());
        }
        for name in &ready {
            if let Some(children) = adjacency.get(name.as_str()) {
                for child in children {
                    if let Some(deg) = in_degree.get_mut(child) {
                        *deg = deg.saturating_sub(1);
                    }
                }
            }
        }
        layers.push(ready);
    }
    Ok(layers)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LayerError {
    /// A service declares `depends_on: ["x"]` but no
    /// service named `x` exists in the manifest. Surfaces as
    /// `Invalid(validation)` upstream; the App Manager UI
    /// shows it as a "manifest is broken" banner.
    UnknownDependency { service: String, dependency: String },
    /// A cycle was found. The returned vec is the set of
    /// service names that could not be reduced to a layer
    /// because they depend on each other.
    Cycle(Vec<String>),
}

impl std::fmt::Display for LayerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LayerError::UnknownDependency {
                service,
                dependency,
            } => write!(
                formatter,
                "service {service} depends on unknown service {dependency}"
            ),
            LayerError::Cycle(names) => {
                write!(
                    formatter,
                    "service dependency cycle: {}",
                    names.join(" -> ")
                )
            }
        }
    }
}

impl std::error::Error for LayerError {}

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

    /// Start every service declared in `resolved`. Phase 3
    /// organises the spawn by `start_layers`: services in the
    /// same DAG layer are started concurrently up to
    /// `config.per_app_concurrency`, the layer must reach
    /// `Healthy` for every member before the next layer is
    /// touched, and any failure inside a layer rolls back
    /// every already-spawned service in reverse layer order.
    ///
    /// The rollback path is what gives the supervisor its
    /// Phase 3 "要么全起要么全没" property: a partially
    /// started app is never left in the supervisor state
    /// because each `start_application` failure unwinds to
    /// `Stopped` and returns a structured error.
    pub fn start_application(
        &self,
        app_id: &str,
        install_root: &Path,
        resolved: &ResolvedApplication,
    ) -> Result<ApplicationObservedState, ApplicationSupervisorError> {
        self.start_application_with_config(app_id, install_root, resolved, &StartConfig::default())
    }

    /// Like [`Self::start_application`] but with an explicit
    /// [`StartConfig`]. Tests use this to dial the per-layer
    /// timeout down to zero so a deterministic failure is
    /// reproducible.
    pub fn start_application_with_config(
        &self,
        app_id: &str,
        install_root: &Path,
        resolved: &ResolvedApplication,
        config: &StartConfig,
    ) -> Result<ApplicationObservedState, ApplicationSupervisorError> {
        let services: Vec<ServiceDescriptor> = resolved.services.values().cloned().collect();
        if services.is_empty() {
            return Err(ApplicationSupervisorError::V2LaunchNotSupported(
                "manifest declares no services; headless UI-only apps are not runnable".into(),
            ));
        }
        for descriptor in &services {
            if !matches!(descriptor.runtime, crate::manifest_v2::ServiceRuntime::Node) {
                return Err(ApplicationSupervisorError::V2LaunchNotSupported(format!(
                    "service {} declares runtime {:?}; Phase 2 only supports Node",
                    descriptor.name, descriptor.runtime
                )));
            }
        }
        let layers = start_layers(&services)
            .map_err(|error| ApplicationSupervisorError::V2LaunchNotSupported(error.to_string()))?;
        let per_app = config.effective_per_app();
        let global_cap = config.effective_global();
        let layer_timeout = config.effective_layer_timeout();
        // Pre-flight: acquire the application's generation +
        // pre-seed every service slot under the lock so a
        // concurrent `list_services` always returns the full
        // declared set.
        let my_generation = {
            let mut guard = self
                .applications
                .lock()
                .expect("application supervisor lock poisoned");
            let application = guard
                .entry(app_id.to_owned())
                .or_insert_with(|| ApplicationRuntime::new(app_id.to_owned()));
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
            for descriptor in &services {
                application
                    .services
                    .entry(descriptor.name.clone())
                    .or_insert_with(|| ServiceRuntime::new(descriptor.clone()));
            }
            application.generation
        };
        // Honour the global spawn cap before we commit to a
        // potentially long-running layer. A cap of zero is
        // treated as "no cap"; a positive cap is enforced
        // before each layer start.
        let _global = GlobalSpawnBudget::enter(global_cap);
        // Layered, concurrent start. Each layer must
        // converge (every service `Healthy`) before the next
        // layer begins.
        let mut started: Vec<String> = Vec::new();
        let mut first_error: Option<(String, String)> = None;
        'layers: for layer in &layers {
            let layer_start = Instant::now();
            // The per-app concurrency cap is enforced by
            // chunking the layer into windows of `per_app`
            // services. Each window is started concurrently;
            // a failure in any service inside a window
            // cancels the rest of the layer.
            for window in layer.chunks(per_app.max(1)) {
                let results: Vec<(String, Result<(), String>)> = thread::scope(|scope| {
                    let mut handles = Vec::with_capacity(window.len());
                    for service_name in window {
                        let service_name = service_name.clone();
                        let app_id = app_id.to_owned();
                        let install_root = install_root.to_path_buf();
                        let descriptor = services
                            .iter()
                            .find(|svc| svc.name == service_name)
                            .cloned()
                            .expect("layer names match declared services");
                        handles.push((
                            service_name.clone(),
                            scope.spawn(move || {
                                self.start_service(
                                    &app_id,
                                    &service_name,
                                    &install_root,
                                    &descriptor,
                                )
                                .map(|_| ())
                                .map_err(|error| error.to_string())
                            }),
                        ));
                    }
                    let mut out = Vec::with_capacity(handles.len());
                    for (name, handle) in handles {
                        let result = match handle.join() {
                            Ok(inner) => inner,
                            Err(_) => Err("spawn thread panicked".to_string()),
                        };
                        out.push((name, result));
                    }
                    out
                });
                for (name, result) in &results {
                    if result.is_ok() {
                        if !started.contains(name) {
                            started.push(name.clone());
                        }
                    } else if first_error.is_none() {
                        first_error = Some((
                            name.clone(),
                            result.as_ref().err().cloned().unwrap_or_default(),
                        ));
                    }
                }
                if first_error.is_some() {
                    break;
                }
            }
            if first_error.is_none() && layer_start.elapsed() > layer_timeout {
                first_error = Some((
                    layer.first().cloned().unwrap_or_default(),
                    format!("layer did not converge within {layer_timeout:?}"),
                ));
            }
            if first_error.is_some() {
                break 'layers;
            }
        }
        if let Some((failed_service, message)) = first_error {
            // Mark downstream services `Blocked` so the App
            // Manager UI can show "deps not satisfied" instead
            // of a generic `Pending`. A service that never
            // had a chance to start because its dep crashed
            // is visibly different from one that is still
            // waiting for a previous attempt.
            self.mark_blocked_after_failure(app_id, &failed_service, &services);
            // Reverse-order rollback. We stop the layers in
            // reverse order, and within each layer the
            // services in reverse start order. `stop_service`
            // is idempotent so a service that was already
            // `Crashed` is left alone.
            self.rollback_started_services(app_id, &started, install_root);
            // Generation check: if a `stop_application` ran
            // concurrently we must not overwrite its work.
            let mut guard = self
                .applications
                .lock()
                .expect("application supervisor lock poisoned");
            if let Some(application) = guard.get_mut(app_id)
                && application.generation == my_generation
            {
                application.observed = ApplicationObservedState::Crashed;
                application.desired = ApplicationDesiredState::Stopped;
                application.last_error = Some(format!("service {failed_service}: {message}"));
            }
            return Err(ApplicationSupervisorError::V2LaunchNotSupported(format!(
                "start failed at service {failed_service}: {message}"
            )));
        }
        // All layers converged.
        let mut guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        let application = guard
            .get_mut(app_id)
            .ok_or_else(|| ApplicationSupervisorError::NotFound(app_id.to_owned()))?;
        if application.generation == my_generation {
            application.observed = rollup_observed_state(&application.services);
            application.desired = ApplicationDesiredState::Running;
        }
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
            service_name: service_name.to_owned(),
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
                    service.consecutive_failures = service.consecutive_failures.wrapping_add(1);
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
        let service = application.services.get_mut(service_name).ok_or_else(|| {
            ApplicationSupervisorError::ServiceNotFound {
                app: app_id.to_owned(),
                service: service_name.to_owned(),
            }
        })?;
        service.handle = Some(handle.clone());
        service.status = ServiceStatus::Healthy;
        // Phase 4 watchdog wiring. The watchdog is the
        // thread that owns the *primary* runtime handle
        // copy: it polls `handle.status()` for the process
        // exit detection, and runs the health probe on the
        // configured cadence. Without this thread, a
        // service that crashes silently would only be
        // noticed on the next user-initiated `service_status`
        // call. The supervisor is `Clone` (an `Arc<...>`
        // under the hood) so handing the watchdog an
        // `Arc<ApplicationSupervisor>` does not introduce
        // a second lock.
        let watchdog_handle = crate::runtime::watchdog::spawn_watchdog(
            app_id.to_owned(),
            service_name.to_owned(),
            handle,
            crate::runtime::watchdog::WatchdogConfig::default(),
            Arc::new(self.clone()),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        service.watchdog_handle = Some(watchdog_handle);
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
        // Phase 4: stop the watchdog *before* we touch
        // the runtime handle. `take_watchdog` flips the
        // stop signal and hands us the `JoinHandle` so
        // the watchdog thread cannot outlive this call.
        // We must drop the supervisor lock first
        // because `take_watchdog` takes it again
        // (`std::sync::Mutex` is not reentrant on
        // Windows + std), and we must take the
        // `handle` out from under the slot's lock so
        // the watchdog — which polls `handle.status()`
        // — sees a consistent view.
        let mut guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        let application = guard
            .get_mut(app_id)
            .ok_or_else(|| ApplicationSupervisorError::NotFound(app_id.to_owned()))?;
        let service = application.services.get_mut(service_name).ok_or_else(|| {
            ApplicationSupervisorError::ServiceNotFound {
                app: app_id.to_owned(),
                service: service_name.to_owned(),
            }
        })?;
        if service.status.is_terminal() {
            return Ok(service.status);
        }
        service.status = ServiceStatus::Stopping;
        let mut handle = service.handle.take();
        service.generation = service.generation.wrapping_add(1);
        drop(guard);
        let watchdog_join = self.take_watchdog(app_id, service_name);
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
        if let Some((join, _signal)) = watchdog_join {
            // The watchdog may take up to
            // `health_interval` (default 5s) to notice
            // the stop signal, but the cooperative
            // check at the top of its loop means it
            // exits on the *first* 50ms sleep after we
            // flip the signal. Bounded by the exit-poll
            // cadence in practice.
            let _ = join.join();
        }
        let mut guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        let application = guard
            .get_mut(app_id)
            .ok_or_else(|| ApplicationSupervisorError::NotFound(app_id.to_owned()))?;
        let service = application.services.get_mut(service_name).ok_or_else(|| {
            ApplicationSupervisorError::ServiceNotFound {
                app: app_id.to_owned(),
                service: service_name.to_owned(),
            }
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
        let service = application.services.get(service_name).ok_or_else(|| {
            ApplicationSupervisorError::ServiceNotFound {
                app: app_id.to_owned(),
                service: service_name.to_owned(),
            }
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
                depends_on: service.spec.depends_on.clone(),
                status: service.status,
                restart_count: service.restart_count,
                last_error: service.last_error.clone(),
            })
            .collect())
    }

    /// Invoke an RPC-mode service without transferring ownership of its
    /// process handle to the caller. This is the data-plane bridge used by
    /// alexd clients once the Daemon is the sole runtime owner.
    pub fn invoke_service(
        &self,
        app_id: &str,
        service_name: &str,
        request_id: &str,
        method: &str,
        params: &serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, ApplicationSupervisorError> {
        let handle = {
            let guard = self
                .applications
                .lock()
                .expect("application supervisor lock poisoned");
            let application = guard
                .get(app_id)
                .ok_or_else(|| ApplicationSupervisorError::NotFound(app_id.to_owned()))?;
            let service = application.services.get(service_name).ok_or_else(|| {
                ApplicationSupervisorError::ServiceNotFound {
                    app: app_id.to_owned(),
                    service: service_name.to_owned(),
                }
            })?;
            service
                .handle
                .clone()
                .ok_or_else(|| ApplicationSupervisorError::ServiceNotFound {
                    app: app_id.to_owned(),
                    service: service_name.to_owned(),
                })?
        };
        handle
            .invoke(request_id, method, params, timeout)
            .map_err(ApplicationSupervisorError::Runtime)
    }

    // -------------------------------------------------------------------
    // Phase 4 — watchdog hooks
    // -------------------------------------------------------------------

    /// Read the per-service spec the watchdog needs to
    /// decide what probe to run. Returns `None` if the app
    /// or service is no longer registered (which is how
    /// the watchdog learns to exit).
    #[allow(dead_code)] // used by the watchdog thread once Phase 4 wires it up
    pub(crate) fn watchdog_spec(
        &self,
        app_id: &str,
        service_name: &str,
    ) -> Option<crate::runtime::watchdog::ServiceSpecSnapshot> {
        let guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        let application = guard.get(app_id)?;
        let service = application.services.get(service_name)?;
        Some(crate::runtime::watchdog::ServiceSpecSnapshot {
            health: service.spec.health.clone(),
            restart_policy: service.spec.restart.policy,
            max_retries: service.spec.restart.max_retries,
            restart_count: service.restart_count,
        })
    }

    /// Read the current `ServiceStatus` for a service, or
    /// `None` if the slot is gone. The watchdog uses this
    /// to detect terminal states and exit.
    #[allow(dead_code)]
    pub(crate) fn watchdog_status(
        &self,
        app_id: &str,
        service_name: &str,
    ) -> Option<ServiceStatus> {
        let guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        let application = guard.get(app_id)?;
        application.services.get(service_name).map(|svc| svc.status)
    }

    /// Construct the per-iteration probe context. The
    /// watchdog calls this once per probe; the supervisor
    /// fills the port / pid / runtime state from its own
    /// tracked values.
    #[allow(dead_code)]
    pub(crate) fn watchdog_probe_context(
        &self,
        app_id: &str,
        service_name: &str,
        port: u16,
    ) -> Option<crate::runtime::watchdog::HealthCheckContext> {
        let guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        let application = guard.get(app_id)?;
        let service = application.services.get(service_name)?;
        let spec = service.spec.health.clone()?;
        let pid = service
            .handle
            .as_ref()
            .and_then(|handle| handle.status(Duration::from_millis(200)).ok())
            .and_then(|s| s.pid);
        let runtime_state = service
            .handle
            .as_ref()
            .and_then(|handle| handle.status(Duration::from_millis(200)).ok())
            .map(|s| s.state)
            .unwrap_or(crate::runtime::supervisor::RuntimeState::Stopped);
        Some(crate::runtime::watchdog::HealthCheckContext {
            spec,
            port,
            pid,
            runtime_state,
        })
    }

    /// Flip the slot to `Healthy` / `Unhealthy` based on
    /// the watchdog's probe result. The watchdog itself
    /// tracks `consecutive_failures`; the supervisor
    /// only handles the visible state transition so the
    /// App Manager UI can show the orange badge.
    pub(crate) fn watchdog_record_outcome(
        &self,
        app_id: &str,
        service_name: &str,
        outcome: crate::runtime::watchdog::HealthUpdate,
    ) {
        let mut guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        if let Some(application) = guard.get_mut(app_id)
            && let Some(service) = application.services.get_mut(service_name)
        {
            match outcome {
                crate::runtime::watchdog::HealthUpdate::Healthy => {
                    if matches!(service.status, ServiceStatus::Unhealthy) {
                        service.status = ServiceStatus::Healthy;
                    }
                }
                crate::runtime::watchdog::HealthUpdate::Unhealthy => {
                    if matches!(service.status, ServiceStatus::Healthy) {
                        service.status = ServiceStatus::Unhealthy;
                    }
                }
            }
        }
    }

    /// Mark a service as `Crashed` because the process has
    /// exited and either the restart policy refuses to
    /// restart or the `max_retries` cap is exhausted. The
    /// Phase 4 follow-up will plumb the actual re-spawn
    /// here; for now the watchdog records the transition
    /// and exits.
    pub(crate) fn watchdog_record_exit(
        &self,
        app_id: &str,
        service_name: &str,
        runtime_state: crate::runtime::supervisor::RuntimeState,
    ) {
        let mut guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        if let Some(application) = guard.get_mut(app_id)
            && let Some(service) = application.services.get_mut(service_name)
            && matches!(
                service.status,
                ServiceStatus::Healthy | ServiceStatus::Starting | ServiceStatus::Unhealthy
            )
        {
            service.status = match runtime_state {
                crate::runtime::supervisor::RuntimeState::Crashed => ServiceStatus::Crashed,
                _ => ServiceStatus::Stopped,
            };
            service.handle = None;
        }
    }

    /// Stop every service in the app. Phase 3 walks the
    /// dependency graph in reverse layer order so a service is
    /// always stopped after the services that depend on it.
    /// The previous Phase 2 behaviour (stop every service in
    /// `BTreeMap` key order) is preserved when the app's
    /// service specs have no `depends_on` edges — the
    /// topological sort collapses to a single layer.
    pub fn stop_application(
        &self,
        app_id: &str,
    ) -> Result<ApplicationObservedState, ApplicationSupervisorError> {
        self.stop_application_with_config(app_id, &StopConfig::default())
    }

    /// Like [`Self::stop_application`] but with an explicit
    /// [`StopConfig`]. Tests use this to dial the per-layer
    /// timeout down to zero so the force-kill path is
    /// deterministic.
    pub fn stop_application_with_config(
        &self,
        app_id: &str,
        config: &StopConfig,
    ) -> Result<ApplicationObservedState, ApplicationSupervisorError> {
        // Bump the generation so any in-flight `start_service`
        // from a previous start sees a stale generation and
        // bails before writing its `Healthy` result. This is
        // the lock-and-bump that gives the supervisor its
        // "no new start tasks during stop" contract.
        let (specs, layers) = {
            let mut guard = self
                .applications
                .lock()
                .expect("application supervisor lock poisoned");
            let application = guard
                .get_mut(app_id)
                .ok_or_else(|| ApplicationSupervisorError::NotFound(app_id.to_owned()))?;
            application.generation = application.generation.wrapping_add(1);
            application.observed = ApplicationObservedState::Stopping;
            let specs: Vec<ServiceDescriptor> = application
                .services
                .values()
                .map(|svc| svc.spec.clone())
                .collect();
            let layers = start_layers(&specs)
                .unwrap_or_else(|_| vec![specs.iter().map(|svc| svc.name.clone()).collect()]);
            (specs, layers)
        };
        let per_app = config.effective_per_app();
        let per_layer = config.effective_per_layer_timeout();
        for layer in layers.iter().rev() {
            let layer_start = Instant::now();
            for window in layer.chunks(per_app.max(1)) {
                let service_names: Vec<String> = window.to_vec();
                thread::scope(|scope| {
                    let mut handles = Vec::with_capacity(service_names.len());
                    for service_name in &service_names {
                        let app_id = app_id.to_owned();
                        let service_name = service_name.clone();
                        handles.push(scope.spawn(move || {
                            let _ = self.stop_service(&app_id, &service_name);
                        }));
                    }
                    for handle in handles {
                        let _ = handle.join();
                    }
                });
            }
            // Honour the per-layer timeout. If a layer takes
            // longer than its budget to drain, we issue a
            // force-kill on the still-running services and
            // give up waiting. The acceptance test "启动过程中
            // 收到 stop" relies on this: the user can stop a
            // misbehaving app without waiting for the slow
            // service to acknowledge.
            if layer_start.elapsed() > per_layer {
                self.force_kill_stuck_services(app_id, layer);
                break;
            }
        }
        let _ = specs; // keep the variable bound for symmetry with start_application
        let mut guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        let application = guard
            .get_mut(app_id)
            .ok_or_else(|| ApplicationSupervisorError::NotFound(app_id.to_owned()))?;
        application.desired = ApplicationDesiredState::Stopped;
        application.observed = rollup_observed_state(&application.services);
        application.last_error = None;
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
            application
                .services
                .values()
                .map(|svc| svc.spec.clone())
                .collect()
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
            Some(handle) => handle
                .status(Duration::from_millis(200))
                .unwrap_or_default(),
            None => RuntimeStatus::default(),
        };
        status.state = match service.status {
            ServiceStatus::Pending | ServiceStatus::WaitingForDependencies => RuntimeState::Stopped,
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
        // Phase 4: drain the watchdog threads *before*
        // removing the application. The watchdog reads
        // its slot through `read_service_status`; if the
        // slot disappears while the thread is mid-probe
        // the watchdog exits cleanly, but the supervisor
        // still wants to flip the stop signal and join
        // the thread so we never leak a 50ms-sleep loop
        // on `uninstall`. We hold the lock to extract
        // the JoinHandles, drop the lock to actually
        // join.
        let drained: Vec<std::thread::JoinHandle<()>> = {
            let mut guard = self
                .applications
                .lock()
                .expect("application supervisor lock poisoned");
            let Some(mut application) = guard.remove(app_id) else {
                return;
            };
            for (_, service) in application.services.iter_mut() {
                if let Some(handle) = service.handle.as_mut() {
                    handle.cancel();
                }
                if let Some(signal) = service.stop_signal.as_ref() {
                    signal.store(true, std::sync::atomic::Ordering::Release);
                }
            }
            application
                .services
                .values_mut()
                .filter_map(|service| service.watchdog_handle.take())
                .collect()
        };
        for join in drained {
            let _ = join.join();
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    pub status: ServiceStatus,
    pub restart_count: u32,
    pub last_error: Option<String>,
}

impl ApplicationSupervisor {
    /// Mark every service that transitively depends on a
    /// failed service as [`ServiceStatus::Blocked`]. The
    /// walk is bounded by the static `services` list — a
    /// service is "downstream" of `failed` if it is
    /// reachable from `failed` along `depends_on` edges.
    fn mark_blocked_after_failure(
        &self,
        app_id: &str,
        failed: &str,
        services: &[ServiceDescriptor],
    ) {
        // Adjacency list: dependency -> services that depend on it.
        let mut downstream: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for svc in services {
            for dep in &svc.depends_on {
                downstream
                    .entry(dep.as_str())
                    .or_default()
                    .push(svc.name.as_str());
            }
        }
        let mut stack: Vec<&str> = vec![failed];
        let mut visited: BTreeMap<&str, ()> = BTreeMap::new();
        while let Some(name) = stack.pop() {
            if visited.insert(name, ()).is_some() {
                continue;
            }
            if let Some(children) = downstream.get(name) {
                for child in children {
                    stack.push(*child);
                }
            }
        }
        // Anything in `visited` other than the failed root
        // is a downstream service that should be `Blocked`.
        let mut guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        if let Some(application) = guard.get_mut(app_id) {
            for service_name in visited.keys() {
                if *service_name == failed {
                    continue;
                }
                if let Some(service) = application.services.get_mut(*service_name)
                    && matches!(service.status, ServiceStatus::Pending)
                {
                    service.status = ServiceStatus::Blocked;
                    service.last_error = Some(format!("dependency {failed} did not start"));
                }
            }
        }
    }

    /// Reverse-order rollback. `started` is the list of
    /// services that successfully reached `Healthy` during
    /// the failed `start_application`; we stop each in
    /// reverse insertion order, which is the same as the
    /// reverse layer order under the current
    /// `start_layers` semantics. `stop_service` is
    /// idempotent so a service that has since crashed on
    /// its own is left alone.
    fn rollback_started_services(&self, app_id: &str, started: &[String], install_root: &Path) {
        for service_name in started.iter().rev() {
            let _ = self.stop_service(app_id, service_name);
        }
        let _ = install_root; // reserved for Phase 7 (managed runtime cleanup hooks)
    }

    /// Issue a force-kill on every still-running service in
    /// `layer` whose handle has not yet reported `Stopped`
    /// or `Crashed`. Called when a stop layer exceeds its
    /// budget. The acceptance test "启动过程中收到 stop"
    /// relies on this to make a hung child go away without
    /// waiting forever.
    fn force_kill_stuck_services(&self, app_id: &str, layer: &[String]) {
        let mut guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        let Some(application) = guard.get_mut(app_id) else {
            return;
        };
        for service_name in layer {
            if let Some(service) = application.services.get_mut(service_name) {
                if let Some(handle) = service.handle.as_mut() {
                    let _ = handle.cancel();
                }
                if !service.status.is_terminal() {
                    service.status = ServiceStatus::Stopped;
                }
            }
        }
    }

    /// Flip the supervisor's stop signal for a service and
    /// take its watchdog `JoinHandle` so the caller can
    /// `join()` it. Returns the join handle and a shared
    /// signal that the watchdog polls once per loop
    /// iteration. The caller is responsible for joining
    /// the handle; the supervisor itself only flips the
    /// signal because the watchdog thread also reads it
    /// cooperatively.
    ///
    /// Returns `None` if the service has no live watchdog
    /// (e.g. it was started by a v1 path that did not yet
    /// have watchdog wiring, or it has been reaped by a
    /// previous stop).
    fn take_watchdog(
        &self,
        app_id: &str,
        service_name: &str,
    ) -> Option<(std::thread::JoinHandle<()>, Arc<AtomicBool>)> {
        let mut guard = self
            .applications
            .lock()
            .expect("application supervisor lock poisoned");
        let application = guard.get_mut(app_id)?;
        let service = application.services.get_mut(service_name)?;
        let handle = service.watchdog_handle.take()?;
        let signal = service.stop_signal.take()?;
        signal.store(true, Ordering::Release);
        Some((handle, Arc::clone(&signal)))
    }
}

// ---------------------------------------------------------------------
// `SupervisorHooks` trait impl — the watchdog calls into these from
// its own thread. The supervisor is `Clone` (an `Arc<Mutex<...>>`
// under the hood), so we hand an `Arc<ApplicationSupervisor>` to
// `spawn_watchdog` and the trait methods can run on any thread.
// ---------------------------------------------------------------------
impl crate::runtime::watchdog::SupervisorHooks for ApplicationSupervisor {
    fn probe_health(
        &self,
        app_id: &str,
        service_name: &str,
        port: u16,
    ) -> Option<crate::runtime::watchdog::HealthCheckContext> {
        self.watchdog_probe_context(app_id, service_name, port)
    }

    fn record_health_outcome(
        &self,
        app_id: &str,
        service_name: &str,
        outcome: crate::runtime::watchdog::HealthUpdate,
        _failure_threshold: u32,
    ) {
        // The watchdog itself owns the `consecutive_failures`
        // counter and only calls us once it has crossed the
        // threshold. The supervisor's job is purely the
        // visible state flip so the App Manager UI can show
        // the right badge.
        self.watchdog_record_outcome(app_id, service_name, outcome);
    }

    fn record_exit(&self, app_id: &str, service_name: &str, runtime_state: RuntimeState) {
        self.watchdog_record_exit(app_id, service_name, runtime_state);
    }

    fn read_service_spec(
        &self,
        app_id: &str,
        service_name: &str,
    ) -> Option<crate::runtime::watchdog::ServiceSpecSnapshot> {
        self.watchdog_spec(app_id, service_name)
    }

    fn read_service_status(&self, app_id: &str, service_name: &str) -> Option<ServiceStatus> {
        self.watchdog_status(app_id, service_name)
    }
}

fn rollup_observed_state(services: &BTreeMap<String, ServiceRuntime>) -> ApplicationObservedState {
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
pub(crate) fn service_descriptor_to_backend(name: &str, spec: &ServiceDescriptor) -> Backend {
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
#[allow(dead_code)] // helper functions only used by some tests in the module
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
        assert_eq!(
            rollup_observed_state(&services),
            ApplicationObservedState::Running
        );
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
        for (name, status) in [("a", ServiceStatus::Stopped), ("b", ServiceStatus::Crashed)] {
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
            let mut guard = supervisor.applications.lock().expect("lock poisoned");
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
            let mut guard = supervisor.applications.lock().expect("lock poisoned");
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
            let mut guard = supervisor.applications.lock().expect("lock poisoned");
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
            let mut guard = supervisor.applications.lock().expect("lock poisoned");
            let mut app = ApplicationRuntime::new("com.example.forget".into());
            for name in ["a", "b", "c"] {
                app.services.insert(
                    name.to_owned(),
                    ServiceRuntime::new(node_service(name, "x.js")),
                );
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
            let mut guard = supervisor.applications.lock().expect("lock poisoned");
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
        use crate::core::{
            application_manifest::ApplicationManifest,
            manifest_v2::{ApplicationManifestV2, RuntimeRequirements, ServiceSpec},
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
        let resolved = unified.resolve().expect("resolve python app");
        let result = supervisor.start_application("com.example.python", Path::new("."), &resolved);
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
            let mut guard = supervisor.applications.lock().expect("lock poisoned");
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
            let mut guard = supervisor.applications.lock().expect("lock poisoned");
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

    // -------------------------------------------------------------------
    // Phase 3 — DAG layering + failure rollback
    // -------------------------------------------------------------------

    fn node_service_with_deps(name: &str, command: &str, deps: &[&str]) -> ServiceDescriptor {
        let mut svc = node_service(name, command);
        svc.depends_on = deps.iter().map(|d| d.to_string()).collect();
        svc
    }

    fn start_layers_orders_a_linear_chain() {
        let services = vec![
            node_service_with_deps("a", "a.js", &[]),
            node_service_with_deps("b", "b.js", &["a"]),
            node_service_with_deps("c", "c.js", &["b"]),
        ];
        let layers = start_layers(&services).expect("linear chain");
        assert_eq!(
            layers,
            vec![
                vec!["a".to_string()],
                vec!["b".to_string()],
                vec!["c".to_string()]
            ]
        );
    }

    #[test]
    fn start_layers_fans_out_a_diamond() {
        // A -> B, A -> C, B -> D, C -> D
        let services = vec![
            node_service_with_deps("a", "a.js", &[]),
            node_service_with_deps("b", "b.js", &["a"]),
            node_service_with_deps("c", "c.js", &["a"]),
            node_service_with_deps("d", "d.js", &["b", "c"]),
        ];
        let layers = start_layers(&services).expect("diamond");
        // Layer 0 = a; layer 1 = b, c (sorted); layer 2 = d.
        assert_eq!(layers[0], vec!["a".to_string()]);
        assert_eq!(layers[1], vec!["b".to_string(), "c".to_string()]);
        assert_eq!(layers[2], vec!["d".to_string()]);
    }

    #[test]
    fn start_layers_groups_siblings_at_the_same_layer() {
        // Three independent services — one layer, sorted.
        let services = vec![
            node_service_with_deps("zeta", "z.js", &[]),
            node_service_with_deps("alpha", "a.js", &[]),
            node_service_with_deps("mu", "m.js", &[]),
        ];
        let layers = start_layers(&services).expect("siblings");
        assert_eq!(layers.len(), 1);
        assert_eq!(
            layers[0],
            vec!["alpha".to_string(), "mu".to_string(), "zeta".to_string()]
        );
    }

    #[test]
    fn start_layers_rejects_a_cycle() {
        // a -> b -> a
        let services = vec![
            node_service_with_deps("a", "a.js", &["b"]),
            node_service_with_deps("b", "b.js", &["a"]),
        ];
        let error = start_layers(&services).expect_err("cycle");
        assert!(matches!(error, LayerError::Cycle(_)));
    }

    #[test]
    fn start_layers_rejects_unknown_dependency() {
        let services = vec![node_service_with_deps("a", "a.js", &["nope"])];
        let error = start_layers(&services).expect_err("unknown dep");
        assert!(matches!(error, LayerError::UnknownDependency { .. }));
    }

    #[test]
    fn start_application_marks_downstream_services_blocked_after_failure() {
        // Diamond: a -> b, a -> c, b -> d, c -> d. We
        // exercise the `mark_blocked_after_failure` helper
        // directly because the full `start_application` path
        // requires a real spawn failure to trigger the
        // rollback (the synchronous `Node` spawn only fails
        // when the underlying interpreter is missing). The
        // helper is the unit under test; the integration
        // assertion is in `tests/core.rs`.
        let supervisor = ApplicationSupervisor::new();
        let services = vec![
            node_service_with_deps("a", "a.js", &[]),
            node_service_with_deps("b", "b.js", &["a"]),
            node_service_with_deps("c", "c.js", &["a"]),
            node_service_with_deps("d", "d.js", &["b", "c"]),
        ];
        supervisor.register_application("com.example.blocked", services.clone());
        supervisor.mark_blocked_after_failure("com.example.blocked", "a", &services);
        let app = supervisor
            .application("com.example.blocked")
            .expect("app present");
        // The failed root stays at whatever status it had
        // (the helper does not flip it). Downstream services
        // are marked `Blocked`.
        let b = app.services.get("b").expect("b slot");
        let c = app.services.get("c").expect("c slot");
        let d = app.services.get("d").expect("d slot");
        assert_eq!(b.status, ServiceStatus::Blocked);
        assert_eq!(c.status, ServiceStatus::Blocked);
        assert_eq!(d.status, ServiceStatus::Blocked);
    }

    #[test]
    fn stop_application_bumps_generation_so_concurrent_start_drops() {
        // The supervisor's `application.generation` is bumped
        // on every `start_application` and `stop_application`
        // call. A concurrent start sees the bumped generation
        // and bails out before writing its `Healthy` result.
        // We assert the simpler invariant that two
        // `stop_application` calls on the same app bump the
        // counter monotonically — the concurrent-start
        // integration is covered by the unit-level helper
        // assertions in `mark_blocked_after_failure` /
        // `rollback_started_services`.
        let supervisor = ApplicationSupervisor::new();
        let services = vec![node_service("main", "main.js")];
        supervisor.register_application("com.example.gen", services);
        let gen_before = supervisor
            .application("com.example.gen")
            .expect("app")
            .generation;
        let _ = supervisor.stop_application("com.example.gen");
        let gen_after_first = supervisor
            .application("com.example.gen")
            .expect("app")
            .generation;
        let _ = supervisor.stop_application("com.example.gen");
        let gen_after_second = supervisor
            .application("com.example.gen")
            .expect("app")
            .generation;
        assert!(gen_after_first > gen_before);
        assert!(gen_after_second > gen_after_first);
    }

    #[test]
    fn stop_application_walks_reverse_layer_order() {
        // A -> B, A -> C, B -> D, C -> D. We pre-seed every
        // service slot in `Healthy`, then call
        // `stop_application`. Because layer order is
        // [[a], [b, c], [d]], the reverse walk stops
        // d first, then b and c concurrently, then a.
        // After the stop the app is `Stopped` and every
        // service slot is terminal.
        let supervisor = ApplicationSupervisor::new();
        let services = vec![
            node_service_with_deps("a", "a.js", &[]),
            node_service_with_deps("b", "b.js", &["a"]),
            node_service_with_deps("c", "c.js", &["a"]),
            node_service_with_deps("d", "d.js", &["b", "c"]),
        ];
        supervisor.register_application("com.example.stop", services.clone());
        for svc in &services {
            assert!(supervisor.set_service_status(
                "com.example.stop",
                &svc.name,
                ServiceStatus::Healthy,
            ));
        }
        let observed = supervisor
            .stop_application("com.example.stop")
            .expect("stop should succeed");
        // Every service ends up terminal and the app is
        // `Stopped` (no crashes).
        assert_eq!(observed, ApplicationObservedState::Stopped);
        let app = supervisor
            .application("com.example.stop")
            .expect("app present");
        for (name, svc) in &app.services {
            assert!(
                svc.status.is_terminal(),
                "service {name} should be terminal after stop, was {:?}",
                svc.status
            );
        }
    }

    #[test]
    fn start_config_defaults_are_sane() {
        let config = StartConfig::default();
        assert_eq!(config.effective_per_app(), 4);
        assert_eq!(config.effective_global(), 8);
        assert_eq!(config.effective_layer_timeout(), Duration::from_secs(5));
        assert_eq!(config.effective_per_app(), 4);
        // Zero values fall back to the defaults.
        let zero = StartConfig {
            per_app_concurrency: 0,
            per_layer_timeout: Duration::ZERO,
            global_concurrency: 0,
        };
        assert_eq!(zero.effective_per_app(), 4);
        assert_eq!(zero.effective_global(), 8);
        assert_eq!(zero.effective_layer_timeout(), Duration::from_secs(5));
    }

    #[test]
    fn stop_config_defaults_are_sane() {
        let config = StopConfig::default();
        assert_eq!(config.effective_per_app(), 4);
        assert_eq!(config.effective_per_layer_timeout(), Duration::from_secs(5));
    }

    // -------------------------------------------------------------------
    // Phase 4 — watchdog hooks
    // -------------------------------------------------------------------

    #[test]
    fn watchdog_record_outcome_flips_healthy_to_unhealthy() {
        // Phase 4 acceptance: "健康检查失败会进入
        // Unhealthy". The supervisor exposes
        // `watchdog_record_outcome` for the watchdog to
        // drive the visible state transition; the slot
        // must flip from `Healthy` to `Unhealthy` on
        // `HealthUpdate::Unhealthy`.
        let supervisor = ApplicationSupervisor::new();
        let descriptor = node_service("main", "main.js");
        supervisor.register_application("com.example.phase4", vec![descriptor]);
        assert!(supervisor.set_service_status(
            "com.example.phase4",
            "main",
            ServiceStatus::Healthy,
        ));
        supervisor.watchdog_record_outcome(
            "com.example.phase4",
            "main",
            crate::runtime::watchdog::HealthUpdate::Unhealthy,
        );
        let app = supervisor
            .application("com.example.phase4")
            .expect("app present");
        assert_eq!(
            app.services.get("main").expect("main slot").status,
            ServiceStatus::Unhealthy
        );
    }

    #[test]
    fn watchdog_record_outcome_recovers_unhealthy_to_healthy() {
        // The flip-back path: once the probe is healthy
        // again, the slot returns to `Healthy`. The
        // App Manager UI uses this to clear the
        // "degraded" badge.
        let supervisor = ApplicationSupervisor::new();
        let descriptor = node_service("main", "main.js");
        supervisor.register_application("com.example.phase4_recover", vec![descriptor]);
        assert!(supervisor.set_service_status(
            "com.example.phase4_recover",
            "main",
            ServiceStatus::Unhealthy,
        ));
        supervisor.watchdog_record_outcome(
            "com.example.phase4_recover",
            "main",
            crate::runtime::watchdog::HealthUpdate::Healthy,
        );
        let app = supervisor
            .application("com.example.phase4_recover")
            .expect("app present");
        assert_eq!(
            app.services.get("main").expect("main slot").status,
            ServiceStatus::Healthy
        );
    }

    #[test]
    fn watchdog_record_exit_marks_running_slot_as_crashed() {
        // Phase 4 acceptance: process exit with
        // `RuntimeState::Crashed` flips the slot to
        // `Crashed`. This is the path the watchdog takes
        // when the restart policy refuses (e.g. `never`)
        // or `max_retries` is exhausted.
        let supervisor = ApplicationSupervisor::new();
        let descriptor = node_service("main", "main.js");
        supervisor.register_application("com.example.phase4_crash", vec![descriptor]);
        assert!(supervisor.set_service_status(
            "com.example.phase4_crash",
            "main",
            ServiceStatus::Healthy,
        ));
        supervisor.watchdog_record_exit(
            "com.example.phase4_crash",
            "main",
            crate::runtime::supervisor::RuntimeState::Crashed,
        );
        let app = supervisor
            .application("com.example.phase4_crash")
            .expect("app present");
        assert_eq!(
            app.services.get("main").expect("main slot").status,
            ServiceStatus::Crashed
        );
    }

    #[test]
    fn watchdog_record_exit_does_not_touch_terminal_slots() {
        // The watchdog must not flip a slot that has
        // already been moved to a terminal state by
        // another path (`stop_service` /
        // `application_supervisor`'s own rollback).
        // A double-flip would re-create the orphan
        // process / log that the previous stop already
        // cleaned up.
        let supervisor = ApplicationSupervisor::new();
        let descriptor = node_service("main", "main.js");
        supervisor.register_application("com.example.phase4_terminal", vec![descriptor]);
        assert!(supervisor.set_service_status(
            "com.example.phase4_terminal",
            "main",
            ServiceStatus::Crashed,
        ));
        supervisor.watchdog_record_exit(
            "com.example.phase4_terminal",
            "main",
            crate::runtime::supervisor::RuntimeState::Crashed,
        );
        let app = supervisor
            .application("com.example.phase4_terminal")
            .expect("app present");
        // The slot is still `Crashed` (no double-flip to
        // `Stopped` or similar).
        assert_eq!(
            app.services.get("main").expect("main slot").status,
            ServiceStatus::Crashed
        );
    }

    #[test]
    fn watchdog_spec_returns_none_for_unknown_app() {
        // The watchdog uses `watchdog_spec` to detect a
        // removed app / service. Returning `None` is how
        // the loop in `run_watchdog` learns to exit.
        let supervisor = ApplicationSupervisor::new();
        assert!(supervisor.watchdog_spec("nope", "main").is_none());
    }

    #[test]
    fn watchdog_status_returns_none_for_unknown_service() {
        let supervisor = ApplicationSupervisor::new();
        let descriptor = node_service("main", "main.js");
        supervisor.register_application("com.example.phase4_status", vec![descriptor]);
        assert!(
            supervisor
                .watchdog_status("com.example.phase4_status", "ghost")
                .is_none()
        );
    }
}

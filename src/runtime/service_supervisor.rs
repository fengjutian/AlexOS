//! Per-service supervisor state.
//!
//! The 0.1 `RuntimeSupervisor` was shaped "one app → one process";
//! Phase 2 of the multi-service roadmap reshapes it to "one app →
//! many services → many processes". This module owns the per-service
//! state slice: the spec the service was launched with, the live
//! [`RuntimeHandle`] once it is up, and the bookkeeping the
//! higher-level [`super::application_supervisor::ApplicationSupervisor`]
//! needs to answer status / restart / stop calls.
//!
//! Phase 3 (DAG orchestration) will read the `depends_on` field on
//! the spec to drive the start order; Phase 2 only needs the data
//! shape so the supervisor can hold multiple services per app.

use serde::Serialize;

use crate::core::application_manifest::ServiceDescriptor;
use crate::runtime::RuntimeHandle;

/// Lifecycle states a single service moves through while the
/// supervisor owns it. These mirror the states listed in
/// `docs/roadmap/multi-service.md` §2.1, with a couple of internal
/// additions (`Pending` is the "never started yet" initial state;
/// `WaitingForDependencies` only fires under Phase 3's DAG start).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceStatus {
    /// Service has been registered with the supervisor but has
    /// never been started. Initial state for every service slot.
    Pending,
    /// Phase 3 only. The service has unmet dependency edges and
    /// is waiting for an upstream service to reach `Healthy`.
    WaitingForDependencies,
    /// The host has spawned the child process; the `alex.ready`
    /// handshake (for service mode) or first `invoke` response
    /// (for rpc mode) is still pending.
    Starting,
    /// The service is up and passing its health check (or, for
    /// `process` health, still alive). Phase 2 only flips into
    /// this state when the host observes a healthy process.
    Healthy,
    /// The service is running but its most recent health probe
    /// failed. The supervisor applies the restart policy before
    /// giving up.
    Unhealthy,
    /// The service is being deliberately restarted (e.g. after a
    /// config change or via the `restart_service` API). The
    /// previous process is being torn down and a fresh one has
    /// not yet been spawned.
    Restarting,
    /// A stop request has been accepted; the host is draining the
    /// process (SIGTERM → grace → SIGKILL) and waiting for it to
    /// exit.
    Stopping,
    /// The service was stopped cleanly (or never started and a
    /// stop call was made; idempotent). The handle has been
    /// dropped.
    Stopped,
    /// The service has failed past the configured `maxRetries`
    /// and the supervisor will not restart it without an explicit
    /// `start_service` / `restart_service` call.
    Crashed,
    /// Phase 3 only. The service is structurally unable to start
    /// because an upstream dependency has been marked `Crashed`.
    Blocked,
}

impl ServiceStatus {
    /// `true` once the service has produced a live `RuntimeHandle`.
    /// Phase 2 uses this to decide whether a `start_service` call
    /// should error (`ServiceAlreadyRunning`) or proceed.
    pub fn is_running(&self) -> bool {
        matches!(
            self,
            ServiceStatus::Starting
                | ServiceStatus::Healthy
                | ServiceStatus::Unhealthy
                | ServiceStatus::Restarting
        )
    }

    /// `true` once the service has either finished a stop or was
    /// never started. The supervisor uses this for the idempotent
    /// `stop_service` semantics: stopping a `Stopped` service is a
    /// no-op, not an error.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            ServiceStatus::Stopped | ServiceStatus::Crashed | ServiceStatus::Blocked
        )
    }
}

/// Per-service state tracked by [`super::application_supervisor::ApplicationSupervisor`].
///
/// One instance per declared service. The supervisor owns the
/// `ServiceRuntime` for the entire lifetime of the service slot,
/// including across stop / start cycles: a restart bumps
/// [`Self::restart_count`] and [`Self::generation`] but does not
/// drop the slot.
#[derive(Debug, Clone)]
pub struct ServiceRuntime {
    /// Service identifier, unique within the application. For v1
    /// single-backend apps this is always
    /// [`crate::core::application_manifest::ServiceDescriptor::V1_MAIN_SERVICE`].
    pub name: String,
    /// The descriptor the service was registered with. Carried
    /// alongside the live state so the supervisor can re-issue a
    /// `start_service` after a crash without the caller having
    /// to re-supply the spec.
    pub spec: ServiceDescriptor,
    /// Live process handle, present when the service is up. The
    /// supervisor drops the handle on `stop_service`; Phase 3 will
    /// keep it around for `Restarting` so the previous PID is
    /// observable until the new process is healthy.
    pub handle: Option<RuntimeHandle>,
    pub status: ServiceStatus,
    /// Total number of times the supervisor has started this
    /// service since the slot was created. Reset only when the
    /// slot itself is removed (e.g. on `uninstall`).
    pub restart_count: u32,
    /// Number of consecutive failed health probes / crashed
    /// exits without an intervening successful start. The
    /// restart policy consults this counter.
    pub consecutive_failures: u32,
    pub last_exit_code: Option<i32>,
    pub last_error: Option<String>,
    /// Per-service generation counter. Bumped on every
    /// start / stop / restart request. The supervisor thread
    /// uses this to discard late results from a previous
    /// generation (e.g. a status reply that arrives after the
    /// user already issued a stop).
    pub generation: u64,
}

impl ServiceRuntime {
    /// Build a fresh slot for `spec` in the `Pending` state. The
    /// supervisor calls this for every service declared in the
    /// application's manifest during `start_application`.
    pub fn new(spec: ServiceDescriptor) -> Self {
        let name = spec.name.clone();
        Self {
            name,
            spec,
            handle: None,
            status: ServiceStatus::Pending,
            restart_count: 0,
            consecutive_failures: 0,
            last_exit_code: None,
            last_error: None,
            generation: 0,
        }
    }
}

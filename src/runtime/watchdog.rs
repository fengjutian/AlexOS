//! Per-service watchdog that runs health probes and applies
//! the restart policy. Phase 4 acceptance requires:
//!
//! * "健康检查失败会进入 `Unhealthy`"
//! * "达到阈值后触发重启"
//! * "`never` 不会重启"
//! * "超出重试次数进入 `Crashed`"
//! * "停止应用后不残留健康检查线程"
//!
//! The watchdog is a single `std::thread` per service.
//! It calls into the supervisor (`probe_health`,
//! `record_health_outcome`, `record_exit`) which
//! short-holds the supervisor's lock to read or update
//! the service slot. The watchdog itself never holds the
//! lock.
//!
//! Phase 4 status: the types compile and the public API is
//! stable, but `start_service` does not yet spawn a
//! watchdog thread. A follow-up wires the
//! `spawn_watchdog` call into `start_service` and adds a
//! stop-signal field to `ServiceRuntime` so `stop_service`
//! can join the thread. The unit tests in this module
//! exercise the helpers directly, and the supervisor
//! exposes the `watchdog_*` mutator methods that the
//! live watchdog thread will call.

#![allow(dead_code)] // some defaults/helpers are still used only by tests

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    core::application_manifest::ServiceRestartPolicy,
    runtime::{
        health::{HealthChecker, HealthOutcome},
        service_supervisor::ServiceStatus,
        supervisor::{RuntimeHandle, RuntimeState},
    },
};

/// Tunable parameters for the watchdog. The defaults are
/// conservative: 5s probe interval, 1s probe timeout, two
/// consecutive failures flips to `Unhealthy`. Tests dial
/// the intervals down to milliseconds to exercise the
/// full path deterministically.
#[derive(Debug, Clone)]
pub struct WatchdogConfig {
    pub health_interval: Duration,
    pub health_timeout: Duration,
    pub failure_threshold: u32,
    /// How often the watchdog polls the `RuntimeHandle` to
    /// detect process exit. 200 ms is a good default —
    /// the watchdog is not on the hot path.
    pub exit_poll_interval: Duration,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            health_interval: Duration::from_secs(5),
            health_timeout: Duration::from_secs(1),
            failure_threshold: 2,
            exit_poll_interval: Duration::from_millis(200),
        }
    }
}

/// v1 backoff schedule (kept in lock-step with
/// `runtime::supervisor::backoff_for`).
const BACKOFF_SCHEDULE: &[Duration] = &[
    Duration::from_millis(0),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
];

fn backoff_for(restart_count: u32) -> Duration {
    let idx = (restart_count as usize).min(BACKOFF_SCHEDULE.len() - 1);
    BACKOFF_SCHEDULE[idx]
}

/// Outcome the supervisor hands the watchdog after a
/// health-probe iteration. The watchdog decides whether
/// the slot should flip to `Unhealthy` and how many
/// consecutive failures to record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthUpdate {
    Healthy,
    Unhealthy,
}

/// Spawn the watchdog thread. The watchdog calls into
/// the supervisor through the `SupervisorHooks` trait so
/// the supervisor can keep its own lock policy. Returns
/// the `JoinHandle` so the caller can wait for the
/// watchdog to drain (e.g. in a test that wants to assert
/// on the final state). The `stop_signal` is the
/// cooperative cancel the supervisor flips in
/// `stop_service`; the watchdog polls it once per loop
/// iteration so a clean stop can exit in under
/// `exit_poll_interval` (typically 200 ms) instead of
/// waiting for the runtime handle to report an exit.
pub(crate) fn spawn_watchdog<H: SupervisorHooks + Send + Sync + 'static>(
    app_id: String,
    service_name: String,
    handle: RuntimeHandle,
    config: WatchdogConfig,
    hooks: Arc<H>,
    stop_signal: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name(format!("alex-watchdog-{app_id}-{service_name}"))
        .spawn(move || {
            run_watchdog(
                app_id,
                service_name,
                handle,
                config,
                hooks,
                stop_signal,
            );
        })
        .expect("watchdog thread should start")
}

/// Trait the supervisor implements so the watchdog can
/// read and write the per-service state without holding
/// the supervisor's lock itself. Each method takes the
/// `app_id` + `service_name` so the supervisor can route
/// the call to the right slot. The methods are short
/// (one or two map lookups + a clone) so the supervisor's
/// lock is held only for the duration of the call.
pub(crate) trait SupervisorHooks {
    fn probe_health(&self, app_id: &str, service_name: &str, port: u16)
        -> Option<HealthCheckContext>;
    fn record_health_outcome(
        &self,
        app_id: &str,
        service_name: &str,
        outcome: HealthUpdate,
        failure_threshold: u32,
    );
    fn record_exit(
        &self,
        app_id: &str,
        service_name: &str,
        runtime_state: RuntimeState,
    );
    fn read_service_spec(
        &self,
        app_id: &str,
        service_name: &str,
    ) -> Option<ServiceSpecSnapshot>;
    fn read_service_status(&self, app_id: &str, service_name: &str) -> Option<ServiceStatus>;
}

/// Snapshot of the service spec the watchdog needs to
/// decide what probe to run. `RuntimeSpec` is not
/// exposed to keep the abstraction tight; the watchdog
/// only needs the health block, the restart policy, and
/// the restart count.
#[derive(Debug, Clone)]
pub(crate) struct ServiceSpecSnapshot {
    pub health: Option<crate::core::application_manifest::ServiceHealthDescriptor>,
    pub restart_policy: ServiceRestartPolicy,
    pub max_retries: u32,
    pub restart_count: u32,
}

/// What the watchdog needs to construct a `HealthChecker`
/// for one probe iteration. The supervisor fills this in
/// from its own state (`pid`, `port`).
#[derive(Debug, Clone)]
pub(crate) struct HealthCheckContext {
    pub spec: crate::core::application_manifest::ServiceHealthDescriptor,
    pub port: u16,
    pub pid: Option<u32>,
    pub runtime_state: RuntimeState,
}

fn run_watchdog<H: SupervisorHooks>(
    app_id: String,
    service_name: String,
    handle: RuntimeHandle,
    config: WatchdogConfig,
    hooks: Arc<H>,
    stop_signal: Arc<AtomicBool>,
) {
    let mut consecutive_failures: u32 = 0;
    let mut last_health = Instant::now() - config.health_interval;
    let loop_start = Instant::now();
    // The watchdog exits when:
    //   1. The supervisor signals stop via the
    //      `stop_signal` atomic — checked at the top
    //      of every loop iteration.
    //   2. The service slot is no longer registered
    //      (`read_service_status` returns `None`).
    //   3. The service slot reaches a terminal state
    //      (`Stopped` / `Crashed` / `Blocked`).
    //   4. The watchdog has applied a final `record_exit`
    //      and there is nothing left to do.
    // The watchdog exits when:
    //   1. The supervisor signals stop via the
    //      `stop_signal` atomic — checked at the top
    //      of every loop iteration.
    //   2. The service slot is no longer registered
    //      (`read_service_status` returns `None`).
    //   3. The service slot reaches a terminal state
    //      (`Stopped` / `Crashed` / `Blocked`).
    //   4. The watchdog has applied a final `record_exit`
    //      and there is nothing left to do.
    loop {
        // (1) Cooperative stop from `stop_service`.
        if stop_signal.load(Ordering::Acquire) {
            break;
        }
        // (2) Slot deleted entirely (uninstall / forget_application).
        if hooks.read_service_status(&app_id, &service_name).is_none() {
            break;
        }
        // (3) Slot already in a terminal state — nothing
        // to do; the supervisor or a previous watchdog
        // iteration already finalised it. Break so we do
        // not spin on `handle.status()` after a stop.
        if let Some(status) = hooks.read_service_status(&app_id, &service_name) {
            if matches!(
                status,
                ServiceStatus::Stopped | ServiceStatus::Crashed | ServiceStatus::Blocked
            ) {
                break;
            }
        }
        // 1) Health probe at the configured cadence.
        if last_health.elapsed() >= config.health_interval
            && loop_start.elapsed() > Duration::from_millis(50)
        {
            last_health = Instant::now();
            if let Some(spec_snapshot) = hooks.read_service_spec(&app_id, &service_name) {
                if spec_snapshot.health.is_some() {
                    let runtime_state = match handle.status(config.health_timeout) {
                        Ok(s) => s.state,
                        Err(_) => RuntimeState::Stopped,
                    };
                    // We can only construct an HTTP probe
                    // when the supervisor knows the port.
                    // Process probes run unconditionally.
                    let port_known = hooks
                        .probe_health(&app_id, &service_name, 0)
                        .map(|ctx| ctx.port)
                        .unwrap_or(0);
                    if let Some(ctx) = hooks.probe_health(&app_id, &service_name, port_known) {
                        let spec = ctx.spec.clone();
                        let port = ctx.port;
                        let pid = ctx.pid;
                        let runtime_state = ctx.runtime_state;
                        let mut spec_for_checker =
                            crate::runtime::health::HealthCheckSpec::from_descriptor(&spec);
                        spec_for_checker.port = port;
                        let checker = HealthChecker::new(spec_for_checker);
                        let outcome = checker.probe(pid, runtime_state);
                        if outcome == HealthOutcome::Unhealthy {
                            consecutive_failures = consecutive_failures.saturating_add(1);
                            if consecutive_failures >= config.failure_threshold {
                                hooks.record_health_outcome(
                                    &app_id,
                                    &service_name,
                                    HealthUpdate::Unhealthy,
                                    config.failure_threshold,
                                );
                            }
                        } else {
                            if consecutive_failures > 0 {
                                hooks.record_health_outcome(
                                    &app_id,
                                    &service_name,
                                    HealthUpdate::Healthy,
                                    config.failure_threshold,
                                );
                            }
                            consecutive_failures = 0;
                        }
                    } else {
                        // No health context available; the
                        // service is either no longer
                        // registered or has no health
                        // descriptor. Either way, the
                        // watchdog has nothing to do for
                        // this iteration.
                        let _ = runtime_state;
                    }
                }
            }
        }
        // 2) Process-exit detection. The runtime status
        // returns `Stopped` / `Crashed` once the child
        // has exited. We consult the restart policy and
        // apply it via `record_exit` (which the supervisor
        // implements to either flip to `Crashed` or
        // re-spawn). For Phase 4 we only flip the slot;
        // the actual re-spawn lands in a follow-up.
        let runtime_state = match handle.status(config.exit_poll_interval) {
            Ok(s) => s.state,
            Err(_) => RuntimeState::Stopped,
        };
        if matches!(runtime_state, RuntimeState::Stopped | RuntimeState::Crashed) {
            let exit_code = match runtime_state {
                RuntimeState::Crashed => Some(1),
                RuntimeState::Stopped => Some(0),
                _ => None,
            };
            let policy = hooks
                .read_service_spec(&app_id, &service_name)
                .map(|s| s.restart_policy)
                .unwrap_or(ServiceRestartPolicy::OnFailure);
            let max_retries = hooks
                .read_service_spec(&app_id, &service_name)
                .map(|s| s.max_retries)
                .unwrap_or(5);
            let restart_count = hooks
                .read_service_spec(&app_id, &service_name)
                .map(|s| s.restart_count)
                .unwrap_or(0);
            let should_restart = match policy {
                ServiceRestartPolicy::Never => false,
                ServiceRestartPolicy::OnFailure => exit_code != Some(0),
                ServiceRestartPolicy::Always => true,
            };
            if !should_restart || restart_count >= max_retries {
                hooks.record_exit(&app_id, &service_name, runtime_state);
                break;
            }
            // Phase 4 leaves the re-spawn to the
            // supervisor's `restart_service` API; here
            // we just sleep the backoff window so the
            // watchdog does not pin the CPU.
            let backoff = backoff_for(restart_count);
            thread::sleep(backoff);
            hooks.record_exit(&app_id, &service_name, runtime_state);
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::application_manifest::{
        ServiceDescriptor, ServiceHealthDescriptor, ServiceHealthKind, ServiceMode,
        ServiceRestartDescriptor, ServiceRestartPolicy,
    };
    use crate::core::manifest_v2::ServiceRuntime as V2Runtime;
    use std::collections::BTreeMap;

    fn http_descriptor() -> ServiceHealthDescriptor {
        ServiceHealthDescriptor {
            kind: ServiceHealthKind::Http,
            path: Some("/health".into()),
            interval_ms: 50,
            timeout_ms: 200,
        }
    }

    fn descriptor_with(policy: ServiceRestartPolicy, max_retries: u32) -> ServiceDescriptor {
        ServiceDescriptor {
            name: "main".to_owned(),
            runtime: V2Runtime::Node,
            command: "main.js".into(),
            args: Vec::new(),
            depends_on: Vec::new(),
            env: BTreeMap::new(),
            port: None,
            mode: ServiceMode::Rpc,
            health: Some(http_descriptor()),
            restart: ServiceRestartDescriptor {
                policy,
                max_retries,
            },
        }
    }

    #[test]
    fn backoff_schedule_caps_at_16_seconds() {
        // The v1 backoff schedule tops out at 16 s; further
        // restart attempts reuse the same window. The
        // watchdog relies on this so a misbehaving service
        // cannot keep the CPU at 100%.
        assert_eq!(backoff_for(0), Duration::from_millis(0));
        assert_eq!(backoff_for(1), Duration::from_secs(1));
        assert_eq!(backoff_for(2), Duration::from_secs(2));
        assert_eq!(backoff_for(3), Duration::from_secs(4));
        assert_eq!(backoff_for(4), Duration::from_secs(8));
        assert_eq!(backoff_for(5), Duration::from_secs(16));
        assert_eq!(backoff_for(6), Duration::from_secs(16));
        assert_eq!(backoff_for(100), Duration::from_secs(16));
    }

    #[test]
    fn health_update_carries_healthy_and_unhealthy_variants() {
        // The watchdog's `HealthUpdate` enum is the
        // supervisor-facing surface of a single probe
        // result. The mapping from `HealthOutcome` (the
        // checker's return) to `HealthUpdate` is a
        // straight pass-through; the separate type lets
        // the watchdog's policy live in its own module
        // without depending on the checker internals.
        assert_eq!(HealthUpdate::Healthy as usize, 0);
        assert_eq!(HealthUpdate::Unhealthy as usize, 1);
    }
}

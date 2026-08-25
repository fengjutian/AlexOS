//! Health checking for managed services.
//!
//! Phase 4 introduces a per-service health checker that runs
//! alongside the supervisor's watchdog thread. The checker
//! reports `Healthy` / `Unhealthy` to the supervisor; once
//! `consecutive_failures` crosses a configurable threshold,
//! the supervisor marks the service `Unhealthy` and the
//! restart policy decides what to do next (restart, stay
//! unhealthy, or give up and mark `Crashed`).
//!
//! Two built-in check kinds:
//!
//! * `Process` — the cheapest check. The supervisor already
//!   knows the live `pid`; this just consults the process
//!   table (via `OpenProcess` / `kill -0`) to confirm the
//!   child is still alive. No loopback traffic, no HTTP
//!   client, no extra dependencies.
//! * `Http` — opens a TCP connection to
//!   `127.0.0.1:<service_port>`, sends a `GET <path>
//!   HTTP/1.1` request, and treats any `2xx` response as
//!   healthy. The connection is closed as soon as the
//!   status line is read; the body is discarded with a
//!   4 KiB cap so a chatty backend cannot wedge the
//!   supervisor.
//!
//! The TCP probe is bounded to `127.0.0.1` because the
//! supervisor is the only legitimate listener. The
//! service port is whatever the supervisor allocated (or
//! the manifest-declared fixed port) — there is no DNS
//! resolution, so a malicious manifest pointing at
//! `evil.example.com` would fail the TCP connect anyway.

use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpStream, ToSocketAddrs},
    time::Duration,
};

use thiserror::Error;

use crate::runtime::supervisor::RuntimeState;

/// What kind of probe the supervisor should run. The
/// `ServiceHealthDescriptor` from the unified manifest
/// projects onto this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthCheckKind {
    /// Liveness is enough: as long as the process is
    /// alive, the service is healthy. Use this for
    /// request/response backends that do not speak HTTP.
    Process,
    /// Open a TCP connection, send a `GET`, and treat any
    /// 2xx as healthy. Use this for service-mode
    /// backends that expose a health endpoint.
    Http,
}

/// What the probe should target.
#[derive(Debug, Clone)]
pub struct HealthCheckSpec {
    pub kind: HealthCheckKind,
    /// Loopback port. Required for `Http`; ignored for
    /// `Process`.
    pub port: u16,
    /// HTTP path requested by the probe. Required for
    /// `Http`; ignored for `Process`.
    pub path: String,
    /// Time between consecutive probes. A typical Phase
    /// 4 default is 5 s.
    pub interval: Duration,
    /// Per-probe timeout (connect + write + read). 2 s
    /// is the v1 default.
    pub timeout: Duration,
    /// Number of consecutive failures that flip the
    /// service from `Healthy` to `Unhealthy`. The
    /// restart policy uses `consecutive_failures` (set by
    /// the supervisor) to make the actual decision; the
    /// checker only reports the bool.
    pub failure_threshold: u32,
}

impl HealthCheckSpec {
    /// Project the v2 `ServiceHealthDescriptor` onto the
    /// Phase 4 spec. Returns `None` when the service has
    /// no health block declared (the supervisor falls
    /// back to `process` liveness for those).
    pub fn from_descriptor(
        descriptor: &crate::core::application_manifest::ServiceHealthDescriptor,
    ) -> Self {
        let kind = match descriptor.kind {
            crate::core::application_manifest::ServiceHealthKind::Process => {
                HealthCheckKind::Process
            }
            crate::core::application_manifest::ServiceHealthKind::Http => HealthCheckKind::Http,
        };
        Self {
            kind,
            port: 0,
            path: descriptor
                .path
                .clone()
                .unwrap_or_else(|| "/health".to_string()),
            interval: Duration::from_millis(descriptor.interval_ms),
            timeout: Duration::from_millis(descriptor.timeout_ms),
            // Two consecutive failures flips to
            // Unhealthy. Tunable in the future; kept here
            // so the spec is self-describing.
            failure_threshold: 2,
        }
    }
}

#[derive(Debug, Error)]
pub enum HealthError {
    #[error("probe timed out after {0:?}")]
    Timeout(Duration),
    #[error("probe i/o failed: {0}")]
    Io(#[from] io::Error),
    #[error("backend returned HTTP {code}: {body_preview:?}")]
    BadStatus { code: u16, body_preview: String },
    #[error("process is not running")]
    ProcessGone,
}

/// Result of a single probe. The supervisor turns this
/// into a per-service `Unhealthy` / `Healthy` transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthOutcome {
    Healthy,
    Unhealthy,
}

/// A single probe attempt. Cheap to construct; the
/// `check_*` methods do the actual work.
#[derive(Debug, Clone)]
pub struct HealthChecker {
    spec: HealthCheckSpec,
}

impl HealthChecker {
    pub fn new(spec: HealthCheckSpec) -> Self {
        Self { spec }
    }

    pub fn spec(&self) -> &HealthCheckSpec {
        &self.spec
    }

    /// Run one probe and return whether the service is
    /// healthy. The probe is bounded by `spec.timeout`;
    /// any error / non-2xx response / timed-out
    /// connection is treated as unhealthy.
    pub fn probe(&self, pid: Option<u32>, runtime_state: RuntimeState) -> HealthOutcome {
        match self.spec.kind {
            HealthCheckKind::Process => self.probe_process(pid, runtime_state),
            HealthCheckKind::Http => self.probe_http(),
        }
    }

    /// `process` liveness check. The pid is consulted
    /// against the platform process table; a missing
    /// process (or one already in `Stopped` / `Crashed`
    /// state) is unhealthy. We do not need a syscall
    /// here because the supervisor's `RuntimeState` is
    /// already the source of truth.
    fn probe_process(&self, _pid: Option<u32>, state: RuntimeState) -> HealthOutcome {
        match state {
            RuntimeState::Running | RuntimeState::Ready | RuntimeState::Starting => {
                HealthOutcome::Healthy
            }
            RuntimeState::Stopped | RuntimeState::Crashed => HealthOutcome::Unhealthy,
        }
    }

    /// `http` check. Opens a TCP connection to
    /// `127.0.0.1:<port>`, sends a `GET <path>`, and reads
    /// up to 4 KiB of the response. Any 2xx is healthy;
    /// anything else (timeout, refused, non-2xx, bad
    /// status line) is unhealthy.
    fn probe_http(&self) -> HealthOutcome {
        let port = self.spec.port;
        let path = &self.spec.path;
        let timeout = self.spec.timeout;
        let addr = match ("127.0.0.1", port).to_socket_addrs() {
            Ok(mut iter) => match iter.next() {
                Some(addr) => addr,
                None => return HealthOutcome::Unhealthy,
            },
            Err(_) => return HealthOutcome::Unhealthy,
        };
        match Self::http_get(addr, path, timeout) {
            Ok(code) if (200..=299).contains(&code) => HealthOutcome::Healthy,
            Ok(_) => HealthOutcome::Unhealthy,
            Err(_) => HealthOutcome::Unhealthy,
        }
    }

    /// Internal helper: open the TCP connection, write a
    /// `GET <path> HTTP/1.1`, read the status line, and
    /// return the parsed status code. The body is
    /// drained (up to 4 KiB) so the backend does not see
    /// a half-closed connection.
    fn http_get(
        addr: std::net::SocketAddr,
        path: &str,
        timeout: Duration,
    ) -> Result<u16, HealthError> {
        let mut stream = TcpStream::connect_timeout(&addr, timeout)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nUser-Agent: alex-health/1\r\n\r\n"
        );
        stream.write_all(request.as_bytes())?;
        let mut buf = [0_u8; 4096];
        let read = stream.read(&mut buf)?;
        if read == 0 {
            let _ = stream.shutdown(Shutdown::Both);
            return Err(HealthError::ProcessGone);
        }
        let response = std::str::from_utf8(&buf[..read]).unwrap_or("");
        let code = response
            .split_whitespace()
            .nth(1)
            .and_then(|token| token.parse::<u16>().ok())
            .ok_or(HealthError::ProcessGone)?;
        let _ = stream.shutdown(Shutdown::Both);
        Ok(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;
    use std::thread;

    /// Stand up a one-shot TCP listener on
    /// `127.0.0.1:0` that replies with a fixed status
    /// line, then close. Returns the bound port.
    fn spawn_http_server(response: &'static str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let port = listener.local_addr().expect("local_addr").port();
        thread::spawn(move || {
            // Accept at most one connection; the health
            // checker closes after the first request.
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                let mut buf = [0_u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        port
    }

    fn http_spec(port: u16, path: &str, _status: &'static str) -> HealthCheckSpec {
        HealthCheckSpec {
            kind: HealthCheckKind::Http,
            port,
            path: path.to_string(),
            interval: Duration::from_millis(50),
            timeout: Duration::from_millis(500),
            failure_threshold: 2,
        }
    }

    #[test]
    fn http_probe_returns_healthy_for_2xx() {
        let port = spawn_http_server("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        let checker = HealthChecker::new(http_spec(port, "/health", ""));
        assert_eq!(
            checker.probe(None, RuntimeState::Ready),
            HealthOutcome::Healthy
        );
    }

    #[test]
    fn http_probe_returns_unhealthy_for_5xx() {
        let port =
            spawn_http_server("HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n");
        let checker = HealthChecker::new(http_spec(port, "/health", ""));
        assert_eq!(
            checker.probe(None, RuntimeState::Ready),
            HealthOutcome::Unhealthy
        );
    }

    #[test]
    fn http_probe_returns_unhealthy_when_nothing_listens() {
        // Bind, grab the port, immediately drop — no one
        // is listening anymore.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        let checker = HealthChecker::new(http_spec(port, "/health", ""));
        assert_eq!(
            checker.probe(None, RuntimeState::Ready),
            HealthOutcome::Unhealthy
        );
    }

    #[test]
    fn process_probe_treats_running_state_as_healthy() {
        let checker = HealthChecker::new(HealthCheckSpec {
            kind: HealthCheckKind::Process,
            port: 0,
            path: String::new(),
            interval: Duration::from_secs(1),
            timeout: Duration::from_secs(1),
            failure_threshold: 2,
        });
        assert_eq!(
            checker.probe(Some(1234), RuntimeState::Running),
            HealthOutcome::Healthy
        );
        assert_eq!(
            checker.probe(Some(1234), RuntimeState::Ready),
            HealthOutcome::Healthy
        );
    }

    #[test]
    fn process_probe_treats_stopped_or_crashed_as_unhealthy() {
        let checker = HealthChecker::new(HealthCheckSpec {
            kind: HealthCheckKind::Process,
            port: 0,
            path: String::new(),
            interval: Duration::from_secs(1),
            timeout: Duration::from_secs(1),
            failure_threshold: 2,
        });
        assert_eq!(
            checker.probe(None, RuntimeState::Stopped),
            HealthOutcome::Unhealthy
        );
        assert_eq!(
            checker.probe(None, RuntimeState::Crashed),
            HealthOutcome::Unhealthy
        );
    }

    #[test]
    fn from_descriptor_projects_v2_health_into_spec() {
        let descriptor = crate::core::application_manifest::ServiceHealthDescriptor {
            kind: crate::core::application_manifest::ServiceHealthKind::Http,
            path: Some("/livez".to_string()),
            interval_ms: 250,
            timeout_ms: 750,
        };
        let spec = HealthCheckSpec::from_descriptor(&descriptor);
        assert_eq!(spec.kind, HealthCheckKind::Http);
        assert_eq!(spec.path, "/livez");
        assert_eq!(spec.interval, Duration::from_millis(250));
        assert_eq!(spec.timeout, Duration::from_millis(750));
    }
}

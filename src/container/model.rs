//! Container model: spec, state, and the policies that bind them.
//!
//! Phase A introduces these types *behind* the existing
//! `RuntimeSupervisor`. Nothing in `manager.rs` or `runtime.rs`
//! changes its public behaviour in this phase — the goal is to put a
//! stable model on disk so subsequent phases (Job Object, AppContainer,
//! OCI) can refactor launch paths against a single source of truth.
//!
//! Naming: a *container* in Alex OS is one instance of a packaged
//! app. A package can be launched multiple times as distinct
//! containers (e.g. `com.example.notes@work`, `com.example.notes@home`).
//! `instance_id` is the runtime key; `app_id` is the manifest identity
//! shared across all instances of the same app.

use std::path::PathBuf;

use semver::Version;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Isolation grade the host is willing to enforce. L0/L1 are usable
/// today; L2/L3 are described here so the manifest can declare the
/// requirement and the host can reject configurations it cannot
/// honour. The string form is the wire/manifest form; the
/// `FromStr` impl accepts kebab-case variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IsolationLevel {
    /// Independent process, directory, port, lifecycle. No resource
    /// limits. Suitable for local trusted code only.
    Process,
    /// L1: Windows Job Object — resource caps and process-tree
    /// cleanup, but no file or network isolation.
    Job,
    /// L2: Windows AppContainer / restricted token. Capable of
    /// hosting audited third-party code. Not implemented in 0.2.
    AppContainer,
    /// L3: external VM isolation (WSL2 / Hyper-V / OCI runtime).
    /// Not implemented in 0.2.
    WslOci,
}

impl IsolationLevel {
    /// True for levels the host can currently enforce on Windows.
    /// L2/L3 are declared by the manifest and the host either meets
    /// the requirement or refuses to start the container — silent
    /// downgrade is forbidden by the design contract.
    pub fn available_on_windows(self) -> bool {
        matches!(self, Self::Process | Self::Job)
    }

    /// Manifest defaults per the design: production deployments must
    /// be at least `job`. We don't fail validation on `process` here
    /// — the host policy layer (later) decides whether the install is
    /// acceptable.
    pub const fn default_for_manifest() -> Self {
        Self::Job
    }
}

impl std::str::FromStr for IsolationLevel {
    type Err = ModelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "process" => Ok(Self::Process),
            "job" => Ok(Self::Job),
            "appcontainer" | "app-container" => Ok(Self::AppContainer),
            "wsl-oci" | "wsl_oci" | "wsl" => Ok(Self::WslOci),
            other => Err(ModelError::InvalidIsolation(other.to_owned())),
        }
    }
}

impl std::fmt::Display for IsolationLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Process => "process",
            Self::Job => "job",
            Self::AppContainer => "appcontainer",
            Self::WslOci => "wsl-oci",
        };
        f.write_str(s)
    }
}

/// Resource limits the host is willing to enforce. `None` means
/// "no host-imposed limit"; the manifest may still set a soft cap
/// the backend self-enforces. The host policy may further tighten
/// the manifest's request but never loosen it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceLimits {
    /// Hard memory cap in MiB. Enforced by Windows Job Object in L1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u32>,
    /// CPU share (0-100). L1's `JOB_OBJECT_CPU_RATE_CONTROL` takes a
    /// percentage of one CPU, so the host maps this directly. `None`
    /// means no CPU cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_percent: Option<u32>,
    /// Maximum number of processes in the container's process tree.
    /// Enforced by `JOB_OBJECT_LIMIT_ACTIVE_PROCESS` in L1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processes: Option<u32>,
    /// Soft data directory quota in MiB. 0.2 only reports usage; the
    /// hard quota is enforced starting in 0.3 once the volume layer
    /// knows how to set per-instance ACLs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_quota_mb: Option<u32>,
}

/// Mounted volume. `source` must be one of the directories
/// pre-authorised at install time — the host rejects arbitrary host
/// paths to defend against junction/symlink escapes. `name` is the
/// alias the backend sees through `ALEX_VOLUME_<name>` env.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VolumeMount {
    pub name: String,
    pub source: PathBuf,
    #[serde(default)]
    pub read_only: bool,
}

impl VolumeMount {
    /// Volume alias names are used as env-var suffixes, so the same
    /// rules as POSIX env names apply: ASCII alnum / underscore, not
    /// starting with a digit.
    pub fn validate_name(&self) -> Result<(), ModelError> {
        let bytes = self.name.as_bytes();
        if bytes.is_empty() {
            return Err(ModelError::InvalidVolumeName("<empty>".into()));
        }
        if !(bytes[0] as char).is_ascii_alphabetic() && bytes[0] != b'_' {
            return Err(ModelError::InvalidVolumeName(self.name.clone()));
        }
        for byte in bytes {
            let c = *byte as char;
            if !(c.is_ascii_alphanumeric() || c == '_') {
                return Err(ModelError::InvalidVolumeName(self.name.clone()));
            }
        }
        Ok(())
    }
}

/// Filesystem policy. The application layer is the verified, read-only
/// package layout under `%LOCALAPPDATA%/AlexOS/packages/<app_id>/<ver>/`.
/// The instance layer is per-container `data` / `cache` / `logs` /
/// `runtime` under `%LOCALAPPDATA%/AlexOS/containers/<instance_id>/`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FilesystemPolicy {
    /// Treat the application layer as read-only. The host enforces
    /// this on the install tree; a `false` value here is allowed for
    /// dev packages but the host still refuses it for L1+ isolation
    /// (because a writable install layer would defeat update
    /// atomicity).
    #[serde(default = "default_true")]
    pub application_read_only: bool,
    /// Soft quota for the per-instance `data/` directory in MiB.
    /// Reporting-only in 0.2; see `ResourceLimits::data_quota_mb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_quota_mb: Option<u32>,
    /// External volumes the manifest declares. The host filters this
    /// against the install-time volume authorisations; arbitrary host
    /// paths are rejected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<VolumeMount>,
}

fn default_true() -> bool {
    true
}

/// Where the backend may listen. 0.2 only supports loopback; the
/// `Host` variant is reserved for future OCI / WSL backends and is
/// rejected by the host policy today.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ListenAddress {
    #[default]
    Loopback,
    None,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NetworkPolicy {
    /// Backend listens on the loopback allocator-allocated port. The
    /// host never exposes the port to the page directly; the proxy
    /// injects the auth token. `None` means the backend does not
    /// accept inbound connections (RPC mode in 0.1).
    #[serde(default)]
    pub listen: ListenAddress,
    /// Hostnames the backend is *allowed* to reach. L1 only audits;
    /// L2 (AppContainer) enforces via Windows capability / firewall
    /// rules. Wildcard rules (`*`) are forbidden.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outbound_allow: Vec<String>,
    /// Hostnames the backend is *denied*. Takes precedence over
    /// `outbound_allow`. Empty in well-behaved manifests; populated
    /// by the host policy as a backstop.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outbound_deny: Vec<String>,
}

impl NetworkPolicy {
    /// `true` for policies the host can honour on Windows in 0.2.
    /// L1/L2 both accept loopback listening; L1 only audits outbound
    /// rules and that is what "audit-only" below means.
    pub fn is_audit_only(&self) -> bool {
        // In 0.2 the host records outbound policy in the event log
        // but does not block the connection. L2 will flip this to
        // false once AppContainer enforcement lands.
        true
    }
}

/// Restart policy for the host supervisor. `policy` mirrors the
/// manifest's `Backend.restart` so we can keep a single declaration
/// of intent; the supervisor consults the policy at every exit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestartPolicy {
    #[serde(default = "RestartPolicy::default_policy")]
    pub policy: String,
    #[serde(default = "RestartPolicy::default_max_retries")]
    pub max_retries: u32,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            policy: Self::default_policy(),
            max_retries: Self::default_max_retries(),
        }
    }
}

impl RestartPolicy {
    fn default_policy() -> String {
        "on-failure".into()
    }
    fn default_max_retries() -> u32 {
        5
    }
}

/// The full set of inputs the host needs to launch a container. The
/// `app_version` and `isolation` fields are required (we never
/// silently pick a default for them); resource / filesystem / network
/// fields fall back to host defaults if absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerSpec {
    /// Stable instance identifier. Defaults to `<app_id>` for the
    /// "single instance per app" 0.2 case; users can pass `--name`
    /// to override.
    pub instance_id: String,
    /// Reverse-domain app id from the manifest.
    pub app_id: String,
    /// Verified package version, pinned at install time.
    pub app_version: Version,
    /// Isolation level the host must enforce. The host rejects a
    /// spec that asks for a level it cannot provide.
    pub isolation: IsolationLevel,
    /// Resource limits; missing fields mean "no host-imposed limit".
    #[serde(default)]
    pub resources: ResourceLimits,
    /// Filesystem policy.
    #[serde(default)]
    pub filesystem: FilesystemPolicy,
    /// Network policy.
    #[serde(default)]
    pub network: NetworkPolicy,
    /// Restart policy.
    #[serde(default)]
    pub restart: RestartPolicy,
}

impl ContainerSpec {
    /// Validate cross-field invariants. Volume mount names must be
    /// env-safe; outbound rules must be non-empty strings without
    /// wildcards; `instance_id` follows the same reverse-domain rules
    /// as `app_id` but is allowed to be a freeform slug for named
    /// instances (`--name`).
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.instance_id.is_empty() {
            return Err(ModelError::InvalidInstanceId("<empty>".into()));
        }
        if self.instance_id.contains('/') || self.instance_id.contains('\\') {
            return Err(ModelError::InvalidInstanceId(self.instance_id.clone()));
        }
        for mount in &self.filesystem.mounts {
            mount.validate_name()?;
        }
        for rule in self
            .network
            .outbound_allow
            .iter()
            .chain(self.network.outbound_deny.iter())
        {
            if rule.is_empty() || rule == "*" {
                return Err(ModelError::InvalidOutboundRule(rule.clone()));
            }
        }
        if let Some(mem) = self.resources.memory_mb
            && mem == 0
        {
            return Err(ModelError::InvalidResource("memoryMb must be > 0".into()));
        }
        if let Some(cpu) = self.resources.cpu_percent
            && cpu > 100
        {
            return Err(ModelError::InvalidResource(format!(
                "cpuPercent must be in 0..=100, got {cpu}"
            )));
        }
        if let Some(proc) = self.resources.processes
            && proc == 0
        {
            return Err(ModelError::InvalidResource("processes must be > 0".into()));
        }
        Ok(())
    }
}

/// Desired state, written by `ContainerService` callers. The host
/// supervises `observed` toward `desired`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DesiredState {
    Created,
    Running,
    Stopped,
    Removed,
}

impl std::fmt::Display for DesiredState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Created => "created",
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Removed => "removed",
        };
        f.write_str(s)
    }
}

/// Observed state. Lives in the supervisor and is the source of
/// truth for "what is the container doing right now".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObservedState {
    Created,
    Starting,
    Running,
    Ready,
    Stopping,
    Stopped,
    Failed,
    Backoff,
}

impl ObservedState {
    /// Terminal states are `Stopped` and `Failed`. The supervisor
    /// does not auto-transition out of a terminal state without an
    /// explicit `start` call.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Stopped | Self::Failed)
    }
}

impl std::fmt::Display for ObservedState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Created => "created",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Ready => "ready",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
            Self::Backoff => "backoff",
        };
        f.write_str(s)
    }
}

/// State recorded for a single container instance. `generation`
/// increments on every persisted state change and is what readers use
/// to detect torn writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerState {
    pub instance_id: String,
    pub app_id: String,
    pub app_version: Version,
    pub desired: DesiredState,
    pub observed: ObservedState,
    pub isolation_effective: IsolationLevel,
    /// `Some(reason)` when the host could not honour the requested
    /// isolation. By contract the host must refuse the launch in that
    /// case rather than silently downgrade; this field is for
    /// diagnostics if a future slice decides to *display* the
    /// unavailability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
    /// PID of the supervisor's child process, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Exit code of the last child to terminate. `None` if the
    /// container is still running or has never started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Service-mode endpoint: `127.0.0.1:<port>` plus the per-launch
    /// token. `None` for RPC backends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<EndpointState>,
    /// Number of supervisor-initiated restarts since the last
    /// user-initiated stop. Used to enforce `RestartPolicy::max_retries`.
    #[serde(default)]
    pub restart_count: u32,
    /// Last error message, if any. Stays `None` on a clean exit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Monotonic counter incremented on every persisted state
    /// change. Readers compare `generation` to detect concurrent
    /// writers and torn writes.
    #[serde(default)]
    pub generation: u64,
    /// ISO 8601 UTC of the first time this instance was created.
    pub created_at: String,
    /// ISO 8601 UTC of the most recent state transition.
    pub updated_at: String,
}

/// Service endpoint as persisted in `state.json`. We avoid
/// persisting the raw token to plaintext on disk; instead we store a
/// short fingerprint and a pointer to the runtime-managed secret
/// file. The host reconstructs the live token from
/// `containers/<id>/runtime/token` at startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EndpointState {
    pub port: u16,
    /// SHA-256 fingerprint of the per-launch token. The token itself
    /// is read from the instance's `runtime/token` file at supervisor
    /// startup and never written to `state.json`.
    pub token_fingerprint: String,
}

/// Validation errors for the model layer. These never include
/// secrets or user content; they are safe to surface in CLI output.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ModelError {
    #[error("invalid isolation level: {0:?}")]
    InvalidIsolation(String),
    #[error("invalid volume name {0:?}; expected ASCII alnum/underscore, not starting with a digit")]
    InvalidVolumeName(String),
    #[error("invalid instance id {0:?}; must not be empty or contain path separators")]
    InvalidInstanceId(String),
    #[error("invalid outbound rule {0:?}; must be non-empty and not a wildcard")]
    InvalidOutboundRule(String),
    #[error("invalid resource limit: {0}")]
    InvalidResource(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolation_round_trips_through_kebab_case_strings() {
        for level in [
            IsolationLevel::Process,
            IsolationLevel::Job,
            IsolationLevel::AppContainer,
            IsolationLevel::WslOci,
        ] {
            let json = serde_json::to_string(&level).unwrap();
            let back: IsolationLevel = serde_json::from_str(&json).unwrap();
            assert_eq!(level, back);
        }
    }

    #[test]
    fn from_str_accepts_legacy_aliases() {
        assert_eq!("job".parse::<IsolationLevel>().unwrap(), IsolationLevel::Job);
        assert_eq!(
            "app-container".parse::<IsolationLevel>().unwrap(),
            IsolationLevel::AppContainer
        );
        assert_eq!(
            "wsl_oci".parse::<IsolationLevel>().unwrap(),
            IsolationLevel::WslOci
        );
        assert!("not-a-level".parse::<IsolationLevel>().is_err());
    }

    #[test]
    fn volume_names_follow_env_var_rules() {
        let ok = VolumeMount {
            name: "USER_DATA".into(),
            source: PathBuf::from("/data"),
            read_only: true,
        };
        assert!(ok.validate_name().is_ok());

        for bad in ["1data", "data-x", "data space", "", "data/path"] {
            let m = VolumeMount {
                name: bad.into(),
                source: PathBuf::from("/data"),
                read_only: true,
            };
            assert!(m.validate_name().is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn spec_validation_rejects_zero_memory_and_wildcard_rules() {
        let mut spec = ContainerSpec {
            instance_id: "x".into(),
            app_id: "com.example.x".into(),
            app_version: Version::new(1, 0, 0),
            isolation: IsolationLevel::Job,
            resources: ResourceLimits {
                memory_mb: Some(0),
                ..Default::default()
            },
            filesystem: Default::default(),
            network: NetworkPolicy {
                outbound_allow: vec!["*".into()],
                ..Default::default()
            },
            restart: Default::default(),
        };
        assert!(spec.validate().is_err());
        spec.resources.memory_mb = Some(512);
        spec.network.outbound_allow.clear();
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn state_machine_marks_only_stopped_and_failed_terminal() {
        assert!(ObservedState::Stopped.is_terminal());
        assert!(ObservedState::Failed.is_terminal());
        for s in [
            ObservedState::Created,
            ObservedState::Starting,
            ObservedState::Running,
            ObservedState::Ready,
            ObservedState::Stopping,
            ObservedState::Backoff,
        ] {
            assert!(!s.is_terminal(), "{s:?} should not be terminal");
        }
    }
}

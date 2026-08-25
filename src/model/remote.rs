//! Remote model providers.
//!
//! This module implements the "remote model" half of the model catalog. It is
//! deliberately split from the local inference worker (`super`) so the two
//! transports stay independent:
//!
//! * applications and agents reference a [`SecretRef`], never a key;
//! * provider configuration and secrets are stored separately;
//! * every remote request is issued by the daemon through a [`RemoteModelProvider`];
//! * endpoint, model, capability, timeout and retry are all managed here;
//! * API keys never appear in logs, errors, events or checkpoints.

use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::platform::PlatformServices;
use crate::platform::secret::{SecretStore, SecretStoreError};

use super::{EmbedRequest, EmbeddingResponse, GenerateEvent, GenerateRequest};

const PROVIDERS_SCHEMA_VERSION: u32 = 1;
const MAX_PROVIDER_ID_LEN: usize = 64;
const MAX_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;
const CIRCUIT_BREAKER_THRESHOLD: u32 = 5;
const CIRCUIT_BREAKER_COOLDOWN: Duration = Duration::from_secs(30);
/// Exponential backoff with jitter. The schedule mirrors the spec
/// (250ms → 500ms → 1s → 2s → 5s); every delay is jittered.
const BACKOFF_SCHEDULE_MS: [u64; 5] = [250, 500, 1000, 2000, 5000];

/// A secret value in transit (e.g. `secret.set`). It redacts itself in `Debug`
/// and `Display` so a key can never leak through a `{:?}` of a request or a
/// `format!` of an error path. The only legitimate consumer is the daemon-side
/// [`SecretResolver`], which writes it straight into the OS secret store.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretValue(pub String);

impl SecretValue {
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretValue([REDACTED])")
    }
}

impl std::fmt::Display for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// Stable identity of a credential in the OS secret store. It is a *reference*,
/// not the credential itself; only the daemon-side [`SecretResolver`] can turn
/// it into bytes, and it does so only to issue an outbound request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecretRef {
    pub service: String,
    pub account: String,
}

impl SecretRef {
    /// Validate length and character set. Credential Manager target names must
    /// not contain NUL and must be bounded; we additionally forbid whitespace
    /// and control characters so a reference can never smuggle a key.
    pub fn validate(&self) -> Result<(), String> {
        for (label, value) in [("service", &self.service), ("account", &self.account)] {
            if value.is_empty() {
                return Err(format!("secret {label} must not be empty"));
            }
            if value.len() > 255 {
                return Err(format!("secret {label} exceeds 255 characters"));
            }
            if value.chars().any(|c| c.is_control() || c.is_whitespace()) {
                return Err(format!(
                    "secret {label} contains a control or whitespace character"
                ));
            }
        }
        Ok(())
    }
}

/// Supported remote provider families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    OpenAiCompatible,
    Anthropic,
    Gemini,
}

/// Provider configuration. This *must not* contain any credential material:
/// the key lives behind [`SecretRef`] in the OS secret store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteProviderConfig {
    pub id: String,
    pub kind: ProviderKind,
    pub endpoint: String,
    pub secret_ref: SecretRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_timeout_ms() -> u64 {
    60_000
}
fn default_max_retries() -> u32 {
    2
}
fn default_enabled() -> bool {
    true
}

impl RemoteProviderConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty() || self.id.len() > MAX_PROVIDER_ID_LEN {
            return Err("provider id must be 1..64 characters".into());
        }
        if !self
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        {
            return Err("provider id contains an invalid character".into());
        }
        self.secret_ref.validate()?;
        validate_endpoint(&self.endpoint)?;
        if self.timeout_ms == 0 || self.timeout_ms > 10 * 60_000 {
            return Err("timeout_ms must be within 1..600000".into());
        }
        if self.max_retries > 10 {
            return Err("max_retries must not exceed 10".into());
        }
        if self
            .default_model
            .as_ref()
            .is_some_and(|m| m.is_empty() || m.len() > 255)
        {
            return Err("default_model must be 1..255 characters".into());
        }
        Ok(())
    }
}

/// Validate an endpoint. HTTPS is required for anything non-loopback; loopback
/// HTTP is allowed so a local OpenAI-compatible server (and the mock test
/// server) can be used without a certificate. URLs must not embed credentials.
fn validate_endpoint(endpoint: &str) -> Result<(), String> {
    let url = url::Url::parse(endpoint).map_err(|e| format!("endpoint is not a valid URL: {e}"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err("endpoint must not embed credentials".into());
    }
    if url.fragment().is_some() {
        return Err("endpoint must not contain a fragment".into());
    }
    let loopback = url
        .host_str()
        .is_some_and(|h| h == "localhost" || h == "127.0.0.1" || h == "::1" || h == "[::1]");
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        _ => return Err("endpoint must use HTTPS (or HTTP on loopback)".into()),
    }
    if url.host_str().is_none() {
        return Err("endpoint must include a host".into());
    }
    Ok(())
}

/// The operations a provider can perform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapabilities {
    pub streaming: bool,
    pub embeddings: bool,
    pub tool_calls: bool,
    pub json_mode: bool,
}

/// A provider's coarse health classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderStatus {
    Healthy,
    Degraded,
    CredentialsMissing,
    Disabled,
    Unreachable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

/// Health snapshot. `last_error` is always redacted and never contains a key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderHealth {
    pub id: String,
    pub kind: ProviderKind,
    pub status: ProviderStatus,
    pub circuit: CircuitState,
    pub consecutive_failures: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub secret_configured: bool,
}

/// Error classification. The kind drives the retry/fallback strategy and never
/// carries credential material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorKind {
    /// 400 / malformed request. Never retried.
    InvalidRequest,
    /// 401 / 403. Credentials are missing or rejected; no unbounded retry.
    Authentication,
    /// 408 / 429. Honor Retry-After.
    RateLimited,
    /// 500 / 502 / 503 / 504. Exponential backoff.
    ServerError,
    TlsError,
    Timeout,
    Connection,
    Cancelled,
    /// The requested model/provider does not exist or is disabled.
    Unavailable,
    /// Anything else that cannot be classified.
    Transport,
}

#[derive(Debug, Error)]
#[error("{message}")]
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    pub status: Option<u16>,
    pub retry_after: Option<Duration>,
    pub message: String,
}

impl ProviderError {
    pub fn new(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            status: None,
            retry_after: None,
            message: message.into(),
        }
    }
    pub fn with_status(kind: ProviderErrorKind, status: u16, message: impl Into<String>) -> Self {
        Self {
            kind,
            status: Some(status),
            retry_after: None,
            message: message.into(),
        }
    }
    /// Whether the error is worth retrying (idempotently) before surfacing.
    pub fn retryable(&self) -> bool {
        matches!(
            self.kind,
            ProviderErrorKind::RateLimited
                | ProviderErrorKind::ServerError
                | ProviderErrorKind::Connection
                | ProviderErrorKind::Timeout
        )
    }
}

/// Daemon-owned resolver: the only path from a [`SecretRef`] to key bytes.
/// Applications and agents never obtain an instance of this type.
#[derive(Clone)]
pub struct SecretResolver {
    store: Arc<dyn SecretStore>,
}

impl SecretResolver {
    pub fn new(store: Arc<dyn SecretStore>) -> Self {
        Self { store }
    }
    pub fn resolve(&self, reference: &SecretRef) -> Result<Option<Vec<u8>>, SecretStoreError> {
        self.store.get(&reference.service, &reference.account)
    }
    pub fn exists(&self, reference: &SecretRef) -> Result<bool, SecretStoreError> {
        self.store
            .get(&reference.service, &reference.account)
            .map(|value| value.is_some())
    }
    pub fn set(&self, reference: &SecretRef, secret: &[u8]) -> Result<(), SecretStoreError> {
        self.store
            .set(&reference.service, &reference.account, secret)
    }
    pub fn delete(&self, reference: &SecretRef) -> Result<bool, SecretStoreError> {
        self.store.delete(&reference.service, &reference.account)
    }
}

/// The provider SPI. All transports (OpenAI-compatible, Anthropic, Gemini)
/// normalize into the shared [`GenerateEvent`] stream and the shared embedding
/// types, so callers never branch on the wire format.
pub trait RemoteModelProvider: Send + Sync {
    fn id(&self) -> &str;
    fn kind(&self) -> ProviderKind;
    fn capabilities(&self) -> ProviderCapabilities;
    fn generate(
        &self,
        request: &GenerateRequest,
        cancel: &Arc<AtomicBool>,
        emit: &mut dyn FnMut(GenerateEvent) -> Result<(), ProviderError>,
    ) -> Result<(), ProviderError>;
    fn embed(&self, request: &EmbedRequest) -> Result<EmbeddingResponse, ProviderError>;
    fn health(&self) -> ProviderHealth;
}

/// Atomic JSON persistence for provider configurations. Secret material lives
/// in the OS store and is never written here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderIndex {
    schema_version: u32,
    providers: BTreeMap<String, RemoteProviderConfig>,
}

#[derive(Clone)]
pub struct ProviderConfigStore {
    path: PathBuf,
}

impl ProviderConfigStore {
    pub fn open(root: &Path) -> Result<Self, ProviderError> {
        std::fs::create_dir_all(root)
            .map_err(|e| ProviderError::new(ProviderErrorKind::Transport, e.to_string()))?;
        let path = root.join("providers.json");
        if !path.exists() {
            atomic_json(
                &path,
                &ProviderIndex {
                    schema_version: PROVIDERS_SCHEMA_VERSION,
                    providers: BTreeMap::new(),
                },
            )?;
        }
        // Validate on open so a corrupt or hand-edited file fails loudly.
        let store = Self { path };
        store.load()?;
        Ok(store)
    }

    fn load(&self) -> Result<BTreeMap<String, RemoteProviderConfig>, ProviderError> {
        let bytes = std::fs::read(&self.path)
            .map_err(|e| ProviderError::new(ProviderErrorKind::Transport, e.to_string()))?;
        let index: ProviderIndex = serde_json::from_slice(&bytes).map_err(|e| {
            ProviderError::new(
                ProviderErrorKind::Transport,
                format!("invalid providers index: {e}"),
            )
        })?;
        if index.schema_version != PROVIDERS_SCHEMA_VERSION {
            return Err(ProviderError::new(
                ProviderErrorKind::Transport,
                format!("unsupported providers schema {}", index.schema_version),
            ));
        }
        for config in index.providers.values() {
            if let Err(error) = config.validate() {
                return Err(ProviderError::new(
                    ProviderErrorKind::Transport,
                    format!("provider {} is invalid: {error}", config.id),
                ));
            }
        }
        Ok(index.providers)
    }

    fn save(
        &self,
        providers: &BTreeMap<String, RemoteProviderConfig>,
    ) -> Result<(), ProviderError> {
        atomic_json(
            &self.path,
            &ProviderIndex {
                schema_version: PROVIDERS_SCHEMA_VERSION,
                providers: providers.clone(),
            },
        )
    }

    pub fn list(&self) -> Result<Vec<RemoteProviderConfig>, ProviderError> {
        Ok(self.load()?.into_values().collect())
    }

    pub fn upsert(&self, config: &RemoteProviderConfig) -> Result<(), ProviderError> {
        config
            .validate()
            .map_err(|e| ProviderError::new(ProviderErrorKind::InvalidRequest, e))?;
        let mut providers = self.load()?;
        providers.insert(config.id.clone(), config.clone());
        self.save(&providers)
    }

    pub fn remove(&self, id: &str) -> Result<bool, ProviderError> {
        let mut providers = self.load()?;
        let removed = providers.remove(id).is_some();
        if removed {
            self.save(&providers)?;
        }
        Ok(removed)
    }
}

/// A tiny circuit breaker that opens after `CIRCUIT_BREAKER_THRESHOLD`
/// consecutive failures and allows a single half-open probe after a cooldown.
#[derive(Debug)]
struct CircuitBreaker {
    state: CircuitState,
    consecutive_failures: u32,
    opened_at: Option<Instant>,
    half_open_inflight: bool,
}

impl CircuitBreaker {
    fn new() -> Self {
        Self {
            state: CircuitState::Closed,
            consecutive_failures: 0,
            opened_at: None,
            half_open_inflight: false,
        }
    }
    fn allow(&mut self) -> bool {
        match self.state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                if self
                    .opened_at
                    .is_some_and(|t| t.elapsed() >= CIRCUIT_BREAKER_COOLDOWN)
                {
                    self.state = CircuitState::HalfOpen;
                    self.half_open_inflight = true;
                    true
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => !self.half_open_inflight,
        }
    }
    fn record_success(&mut self) {
        self.state = CircuitState::Closed;
        self.consecutive_failures = 0;
        self.opened_at = None;
        self.half_open_inflight = false;
    }
    fn record_failure(&mut self) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        match self.state {
            CircuitState::HalfOpen => {
                self.state = CircuitState::Open;
                self.opened_at = Some(Instant::now());
                self.half_open_inflight = false;
            }
            _ if self.consecutive_failures >= CIRCUIT_BREAKER_THRESHOLD => {
                self.state = CircuitState::Open;
                self.opened_at = Some(Instant::now());
                self.half_open_inflight = false;
            }
            _ => {
                self.half_open_inflight = false;
            }
        }
    }
    fn snapshot(&self) -> (CircuitState, u32) {
        (self.state, self.consecutive_failures)
    }
}

/// The daemon-owned router: resolves providers from `remote/<provider>/<model>`
/// model IDs, applies circuit breaking and retries, and resolves secrets.
#[derive(Clone)]
pub struct RemoteProviderRouter {
    store: ProviderConfigStore,
    resolver: Arc<SecretResolver>,
    providers: Arc<Mutex<BTreeMap<String, Arc<dyn RemoteModelProvider>>>>,
    breakers: Arc<Mutex<BTreeMap<String, CircuitBreaker>>>,
    cancellations: Arc<Mutex<BTreeMap<String, Arc<AtomicBool>>>>,
    last_errors: Arc<Mutex<BTreeMap<String, String>>>,
    latencies: Arc<Mutex<BTreeMap<String, Duration>>>,
}

impl RemoteProviderRouter {
    pub fn open(root: &Path, resolver: SecretResolver) -> Result<Self, ProviderError> {
        let store = ProviderConfigStore::open(root)?;
        let resolver = Arc::new(resolver);
        let router = Self {
            store,
            resolver,
            providers: Arc::new(Mutex::new(BTreeMap::new())),
            breakers: Arc::new(Mutex::new(BTreeMap::new())),
            cancellations: Arc::new(Mutex::new(BTreeMap::new())),
            last_errors: Arc::new(Mutex::new(BTreeMap::new())),
            latencies: Arc::new(Mutex::new(BTreeMap::new())),
        };
        router.rebuild()?;
        Ok(router)
    }

    /// Rebuild the provider table from persisted config. Disabled providers are
    /// retained in config but not registered.
    fn rebuild(&self) -> Result<(), ProviderError> {
        let configs = self.store.list()?;
        let mut providers = self.providers.lock().map_err(|_| {
            ProviderError::new(ProviderErrorKind::Transport, "provider registry poisoned")
        })?;
        providers.clear();
        for config in configs {
            if !config.enabled {
                continue;
            }
            if let Some(provider) = build_provider(&config, Arc::clone(&self.resolver)) {
                providers.insert(config.id.clone(), provider);
            }
        }
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<RemoteProviderConfig>, ProviderError> {
        self.store.list()
    }

    pub fn upsert(&self, config: RemoteProviderConfig) -> Result<(), ProviderError> {
        config
            .validate()
            .map_err(|e| ProviderError::new(ProviderErrorKind::InvalidRequest, e))?;
        self.store.upsert(&config)?;
        self.rebuild()?;
        // Clear stale circuit/error state for a provider whose config changed.
        if let Ok(mut breakers) = self.breakers.lock() {
            breakers.remove(&config.id);
        }
        self.last_errors
            .lock()
            .ok()
            .map(|mut m| m.remove(&config.id));
        Ok(())
    }

    pub fn remove(&self, id: &str) -> Result<bool, ProviderError> {
        let removed = self.store.remove(id)?;
        if removed {
            self.providers.lock().ok().map(|mut m| m.remove(id));
            self.breakers.lock().ok().map(|mut m| m.remove(id));
            self.last_errors.lock().ok().map(|mut m| m.remove(id));
        }
        Ok(removed)
    }

    pub fn secret_set(&self, reference: &SecretRef, secret: &[u8]) -> Result<(), ProviderError> {
        reference
            .validate()
            .map_err(|e| ProviderError::new(ProviderErrorKind::InvalidRequest, e))?;
        self.resolver
            .set(reference, secret)
            .map_err(|e| ProviderError::new(ProviderErrorKind::Transport, e.to_string()))
    }

    pub fn secret_delete(&self, reference: &SecretRef) -> Result<bool, ProviderError> {
        reference
            .validate()
            .map_err(|e| ProviderError::new(ProviderErrorKind::InvalidRequest, e))?;
        self.resolver
            .delete(reference)
            .map_err(|e| ProviderError::new(ProviderErrorKind::Transport, e.to_string()))
    }

    pub fn secret_exists(&self, reference: &SecretRef) -> Result<bool, ProviderError> {
        reference
            .validate()
            .map_err(|e| ProviderError::new(ProviderErrorKind::InvalidRequest, e))?;
        self.resolver
            .exists(reference)
            .map_err(|e| ProviderError::new(ProviderErrorKind::Transport, e.to_string()))
    }

    fn provider_for_model(
        &self,
        model: &str,
    ) -> Result<(String, Arc<dyn RemoteModelProvider>), ProviderError> {
        let (provider_id, _) = parse_remote_model(model).ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Unavailable,
                "model is not a remote model",
            )
        })?;
        let providers = self.providers.lock().map_err(|_| {
            ProviderError::new(ProviderErrorKind::Transport, "provider registry poisoned")
        })?;
        let provider = providers.get(provider_id).cloned().ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Unavailable,
                format!("remote provider {provider_id:?} is not configured or is disabled"),
            )
        })?;
        Ok((provider_id.to_string(), provider))
    }

    pub fn health(&self, id: &str) -> ProviderHealth {
        let config = self
            .store
            .list()
            .ok()
            .and_then(|c| c.into_iter().find(|c| c.id == id));
        let secret_configured = config
            .as_ref()
            .and_then(|c| self.resolver.exists(&c.secret_ref).ok())
            .unwrap_or(false);
        let (circuit, consecutive_failures) = self
            .breakers
            .lock()
            .ok()
            .and_then(|m| m.get(id).map(|b| b.snapshot()))
            .unwrap_or((CircuitState::Closed, 0));
        let latency_ms = self
            .latencies
            .lock()
            .ok()
            .and_then(|m| m.get(id).map(|d| d.as_millis() as u64));
        let last_error = self
            .last_errors
            .lock()
            .ok()
            .and_then(|m| m.get(id).cloned());
        let status = match &config {
            None => ProviderStatus::Unreachable,
            Some(c) if !c.enabled => ProviderStatus::Disabled,
            Some(c) if !secret_configured => ProviderStatus::CredentialsMissing,
            Some(_) if circuit == CircuitState::Open => ProviderStatus::Degraded,
            Some(_) if consecutive_failures > 0 => ProviderStatus::Degraded,
            Some(_) => ProviderStatus::Healthy,
        };
        ProviderHealth {
            id: id.to_string(),
            kind: config
                .as_ref()
                .map(|c| c.kind)
                .unwrap_or(ProviderKind::OpenAiCompatible),
            status,
            circuit,
            consecutive_failures,
            latency_ms,
            last_error,
            secret_configured,
        }
    }

    pub fn health_all(&self) -> Vec<ProviderHealth> {
        let mut ids: Vec<String> = self
            .store
            .list()
            .map(|c| c.into_iter().map(|c| c.id).collect())
            .unwrap_or_default();
        ids.sort();
        ids.into_iter().map(|id| self.health(&id)).collect()
    }

    pub fn generate(
        &self,
        request: &GenerateRequest,
        emit: &mut dyn FnMut(GenerateEvent) -> Result<(), ProviderError>,
    ) -> Result<(), ProviderError> {
        let (provider_id, provider) = self.provider_for_model(&request.model)?;
        let cancel = self.register_cancel(&request.request_id);
        let started = Instant::now();
        let result = self.run_with_breaker(&provider_id, || {
            let mut attempt = 0u32;
            loop {
                if cancel.load(Ordering::Acquire) {
                    return Err(ProviderError::new(
                        ProviderErrorKind::Cancelled,
                        "generation cancelled",
                    ));
                }
                let mut first_token_seen = false;
                let mut emitted = |event: GenerateEvent| -> Result<(), ProviderError> {
                    if matches!(
                        event,
                        GenerateEvent::Delta { .. } | GenerateEvent::ToolCall { .. }
                    ) {
                        first_token_seen = true;
                    }
                    emit(event)
                };
                match provider.generate(request, &cancel, &mut emitted) {
                    Ok(()) => return Ok(()),
                    Err(error) => {
                        // Once the first token has been produced, never replay.
                        if first_token_seen || !error.retryable() {
                            return Err(error);
                        }
                        let max_retries = self
                            .store
                            .list()
                            .ok()
                            .and_then(|c| c.into_iter().find(|c| c.id == provider_id))
                            .map(|c| c.max_retries)
                            .unwrap_or(0);
                        if attempt >= max_retries {
                            return Err(error);
                        }
                        attempt += 1;
                        let delay = retry_delay(&error, attempt);
                        if cancel.load(Ordering::Acquire) {
                            return Err(ProviderError::new(
                                ProviderErrorKind::Cancelled,
                                "generation cancelled",
                            ));
                        }
                        std::thread::sleep(delay);
                    }
                }
            }
        });
        self.record_outcome(&provider_id, &result, started);
        if cancel.load(Ordering::Acquire) {
            self.unregister_cancel(&request.request_id);
        }
        result
    }

    pub fn embed(&self, request: &EmbedRequest) -> Result<EmbeddingResponse, ProviderError> {
        let (provider_id, provider) = self.provider_for_model(&request.model)?;
        let started = Instant::now();
        let result = self.run_with_breaker(&provider_id, || {
            let mut attempt = 0u32;
            loop {
                match provider.embed(request) {
                    Ok(response) => return Ok(response),
                    Err(error) => {
                        if !error.retryable() {
                            return Err(error);
                        }
                        let max_retries = self
                            .store
                            .list()
                            .ok()
                            .and_then(|c| c.into_iter().find(|c| c.id == provider_id))
                            .map(|c| c.max_retries)
                            .unwrap_or(0);
                        if attempt >= max_retries {
                            return Err(error);
                        }
                        attempt += 1;
                        std::thread::sleep(retry_delay(&error, attempt));
                    }
                }
            }
        });
        self.record_outcome(&provider_id, &result, started);
        result
    }

    pub fn cancel(&self, request_id: &str) {
        if let Some(cancel) = self
            .cancellations
            .lock()
            .ok()
            .and_then(|m| m.get(request_id).cloned())
        {
            cancel.store(true, Ordering::Release);
        }
    }

    fn register_cancel(&self, request_id: &str) -> Arc<AtomicBool> {
        let mut map = self
            .cancellations
            .lock()
            .expect("cancellation registry poisoned");
        map.entry(request_id.to_string())
            .or_insert_with(|| Arc::new(AtomicBool::new(false)))
            .clone()
    }

    fn unregister_cancel(&self, request_id: &str) {
        if let Ok(mut map) = self.cancellations.lock() {
            map.remove(request_id);
        }
    }

    fn run_with_breaker<T>(
        &self,
        provider_id: &str,
        operation: impl FnOnce() -> Result<T, ProviderError>,
    ) -> Result<T, ProviderError> {
        {
            let mut breakers = self
                .breakers
                .lock()
                .expect("circuit breaker registry poisoned");
            let breaker = breakers
                .entry(provider_id.to_string())
                .or_insert_with(CircuitBreaker::new);
            if !breaker.allow() {
                return Err(ProviderError::new(
                    ProviderErrorKind::Unavailable,
                    format!("provider {provider_id:?} circuit is open"),
                ));
            }
        }
        match operation() {
            Ok(value) => {
                if let Ok(mut breakers) = self.breakers.lock() {
                    if let Some(breaker) = breakers.get_mut(provider_id) {
                        breaker.record_success();
                    }
                }
                Ok(value)
            }
            Err(error) => {
                if let Ok(mut breakers) = self.breakers.lock() {
                    if let Some(breaker) = breakers.get_mut(provider_id) {
                        breaker.record_failure();
                    }
                }
                Err(error)
            }
        }
    }

    fn record_outcome<T>(
        &self,
        provider_id: &str,
        result: &Result<T, ProviderError>,
        started: Instant,
    ) {
        let elapsed = started.elapsed();
        if let Ok(mut latencies) = self.latencies.lock() {
            latencies.insert(provider_id.to_string(), elapsed);
        }
        if let Err(error) = result {
            if let Ok(mut errors) = self.last_errors.lock() {
                errors.insert(provider_id.to_string(), error.message.clone());
            }
        }
    }
}

/// Parse `remote/<provider-id>/<model-name>` into its two components.
fn parse_remote_model(model: &str) -> Option<(&str, &str)> {
    let rest = model.strip_prefix("remote/")?;
    let (provider, name) = rest.split_once('/')?;
    if provider.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some((provider, name))
}

/// Compute a retry delay from the error and attempt count, honoring
/// `Retry-After` and applying jitter to the exponential schedule.
fn retry_delay(error: &ProviderError, attempt: u32) -> Duration {
    if let Some(retry_after) = error.retry_after {
        return retry_after;
    }
    let base_ms = BACKOFF_SCHEDULE_MS
        .get(attempt.saturating_sub(1) as usize)
        .copied()
        .unwrap_or(5000);
    // Deterministic jitter derived from attempt so tests stay reproducible.
    let jitter = (attempt as u64 * 37) % 100;
    Duration::from_millis(base_ms + jitter)
}

fn build_provider(
    config: &RemoteProviderConfig,
    resolver: Arc<SecretResolver>,
) -> Option<Arc<dyn RemoteModelProvider>> {
    match config.kind {
        ProviderKind::OpenAiCompatible => Some(Arc::new(OpenAiCompatibleProvider::new(
            config.clone(),
            resolver,
        ))),
        // Anthropic and Gemini are declared but not yet wired; an enabled
        // provider of an unsupported kind reports Unreachable rather than
        // silently falling back to another wire format.
        ProviderKind::Anthropic | ProviderKind::Gemini => None,
    }
}

/// OpenAI-compatible provider over `ureq` (rustls). Supports
/// `/chat/completions` (non-streaming and SSE) and `/embeddings`.
pub struct OpenAiCompatibleProvider {
    config: RemoteProviderConfig,
    resolver: Arc<SecretResolver>,
    agent: ureq::Agent,
}

impl OpenAiCompatibleProvider {
    pub fn new(config: RemoteProviderConfig, resolver: Arc<SecretResolver>) -> Self {
        let agent = ureq::Agent::config_builder()
            .https_only(false) // loopback HTTP is allowed for local/test servers
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_millis(config.timeout_ms)))
            .build()
            .into();
        Self {
            config,
            resolver,
            agent,
        }
    }

    fn resolve_secret(&self) -> Result<Vec<u8>, ProviderError> {
        self.resolver
            .resolve(&self.config.secret_ref)
            .map_err(|e| ProviderError::new(ProviderErrorKind::Transport, e.to_string()))?
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::Authentication,
                    "provider credentials are missing",
                )
            })
    }

    fn upstream_model<'a>(&self, model_id: &'a str) -> Result<&'a str, ProviderError> {
        match parse_remote_model(model_id) {
            Some((provider, name)) if provider == self.config.id => Ok(name),
            Some((provider, _)) => Err(ProviderError::new(
                ProviderErrorKind::Unavailable,
                format!(
                    "model {model_id:?} belongs to provider {provider:?}, not {}",
                    self.config.id
                ),
            )),
            None => Err(ProviderError::new(
                ProviderErrorKind::InvalidRequest,
                format!("model {model_id:?} is not a remote model"),
            )),
        }
    }

    fn redact(&self, secret: &[u8], text: &str) -> String {
        if secret.is_empty() {
            return text.to_string();
        }
        let key = String::from_utf8_lossy(secret);
        text.replace(key.as_ref(), "[REDACTED]")
    }

    fn chat_url(&self) -> String {
        format!(
            "{}/chat/completions",
            self.config.endpoint.trim_end_matches('/')
        )
    }
    fn embed_url(&self) -> String {
        format!("{}/embeddings", self.config.endpoint.trim_end_matches('/'))
    }

    fn request(&self, url: &str, secret: &[u8]) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
        let mut request = self
            .agent
            .post(url)
            .header(
                "Authorization",
                format!("Bearer {}", String::from_utf8_lossy(secret)),
            )
            .header("Content-Type", "application/json");
        if let Some(org) = &self.config.organization {
            request = request.header("OpenAI-Organization", org);
        }
        request
    }

    fn build_chat_body(&self, model: &str, request: &GenerateRequest, stream: bool) -> Value {
        let mut body = serde_json::Map::new();
        body.insert("model".into(), json!(model));
        body.insert("messages".into(), Value::Array(request.messages.clone()));
        body.insert("stream".into(), json!(stream));
        if let Value::Object(options) = &request.options {
            for (key, value) in options {
                if !matches!(key.as_str(), "model" | "messages" | "stream") {
                    body.insert(key.clone(), value.clone());
                }
            }
        }
        Value::Object(body)
    }
}

impl RemoteModelProvider for OpenAiCompatibleProvider {
    fn id(&self) -> &str {
        &self.config.id
    }
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAiCompatible
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            embeddings: true,
            tool_calls: true,
            json_mode: true,
        }
    }
    fn generate(
        &self,
        request: &GenerateRequest,
        cancel: &Arc<AtomicBool>,
        emit: &mut dyn FnMut(GenerateEvent) -> Result<(), ProviderError>,
    ) -> Result<(), ProviderError> {
        let secret = self.resolve_secret()?;
        let model = self.upstream_model(&request.model)?;
        let stream = request
            .options
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let body = self.build_chat_body(model, request, stream);
        let body_bytes = serde_json::to_vec(&body).map_err(|e| {
            ProviderError::new(
                ProviderErrorKind::InvalidRequest,
                format!("failed to serialize chat request: {e}"),
            )
        })?;
        let mut response = self
            .request(&self.chat_url(), &secret)
            .send(body_bytes)
            .map_err(|e| self.classify_transport(&secret, e))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let retry_after = retry_after_header(response.headers());
            let kind = classify_status(status);
            let raw = response
                .body_mut()
                .with_config()
                .limit(64 * 1024)
                .read_to_string()
                .unwrap_or_default();
            let mut message = self.redact(&secret, &raw);
            if message.is_empty() {
                message = format!("upstream returned HTTP {status}");
            }
            let mut error = ProviderError::with_status(kind, status, message);
            error.retry_after = retry_after;
            return Err(error);
        }
        if stream {
            self.consume_sse(response, cancel, emit)
        } else {
            self.consume_json(response, emit)
        }
    }
    fn embed(&self, request: &EmbedRequest) -> Result<EmbeddingResponse, ProviderError> {
        let secret = self.resolve_secret()?;
        let model = self.upstream_model(&request.model)?;
        let body = json!({ "model": model, "input": request.input });
        let body_bytes = serde_json::to_vec(&body).map_err(|e| {
            ProviderError::new(
                ProviderErrorKind::InvalidRequest,
                format!("failed to serialize embedding request: {e}"),
            )
        })?;
        let mut response = self
            .request(&self.embed_url(), &secret)
            .send(body_bytes)
            .map_err(|e| self.classify_transport(&secret, e))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let raw = response
                .body_mut()
                .with_config()
                .limit(64 * 1024)
                .read_to_string()
                .unwrap_or_default();
            let message = self.redact(&secret, &raw);
            return Err(ProviderError::with_status(
                classify_status(status),
                status,
                if message.is_empty() {
                    format!("upstream returned HTTP {status}")
                } else {
                    message
                },
            ));
        }
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_to_string()
            .map_err(|e| ProviderError::new(ProviderErrorKind::Transport, e.to_string()))?;
        let value: Value = serde_json::from_str(&body).map_err(|e| {
            ProviderError::new(
                ProviderErrorKind::Transport,
                format!("invalid embedding response: {e}"),
            )
        })?;
        parse_embedding_response(request, &value)
    }
    fn health(&self) -> ProviderHealth {
        let secret_configured = self
            .resolver
            .exists(&self.config.secret_ref)
            .unwrap_or(false);
        let status = if !self.config.enabled {
            ProviderStatus::Disabled
        } else if !secret_configured {
            ProviderStatus::CredentialsMissing
        } else {
            ProviderStatus::Healthy
        };
        ProviderHealth {
            id: self.config.id.clone(),
            kind: self.kind(),
            status,
            circuit: CircuitState::Closed,
            consecutive_failures: 0,
            latency_ms: None,
            last_error: None,
            secret_configured,
        }
    }
}

impl OpenAiCompatibleProvider {
    fn classify_transport(&self, secret: &[u8], error: ureq::Error) -> ProviderError {
        use ureq::Error;
        match error {
            Error::StatusCode(status) => ProviderError::with_status(
                classify_status(status),
                status,
                format!("upstream returned HTTP {status}"),
            ),
            Error::Io(io) => {
                let message = self.redact(secret, &io.to_string());
                if io.kind() == std::io::ErrorKind::TimedOut {
                    ProviderError::new(ProviderErrorKind::Timeout, message)
                } else if io.to_string().to_ascii_lowercase().contains("certificate")
                    || io.to_string().to_ascii_lowercase().contains("tls")
                {
                    ProviderError::new(ProviderErrorKind::TlsError, message)
                } else {
                    ProviderError::new(ProviderErrorKind::Connection, message)
                }
            }
            Error::Http(_) => ProviderError::new(
                ProviderErrorKind::Transport,
                self.redact(secret, &error.to_string()),
            ),
            _ => ProviderError::new(
                ProviderErrorKind::Transport,
                self.redact(secret, &error.to_string()),
            ),
        }
    }

    /// Parse a non-streaming `chat/completions` response and emit a single
    /// delta, tool calls, usage and finish.
    fn consume_json(
        &self,
        mut response: ureq::http::Response<ureq::Body>,
        emit: &mut dyn FnMut(GenerateEvent) -> Result<(), ProviderError>,
    ) -> Result<(), ProviderError> {
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_to_string()
            .map_err(|e| ProviderError::new(ProviderErrorKind::Transport, e.to_string()))?;
        let value: Value = serde_json::from_str(&body).map_err(|e| {
            ProviderError::new(
                ProviderErrorKind::Transport,
                format!("invalid chat response: {e}"),
            )
        })?;
        let choice = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::Transport,
                    "chat response omitted choices",
                )
            })?;
        let message = choice.get("message").unwrap_or(&Value::Null);
        if let Some(content) = message.get("content").and_then(Value::as_str) {
            if !content.is_empty() {
                emit(GenerateEvent::Delta {
                    text: content.to_string(),
                })?;
            }
        }
        if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
            for tool_call in tool_calls {
                let function = tool_call.get("function");
                let name = function
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let arguments = function
                    .and_then(|f| f.get("arguments"))
                    .and_then(|a| a.as_str())
                    .map(parse_arguments)
                    .unwrap_or_else(|| Value::Null);
                if !name.is_empty() {
                    emit(GenerateEvent::ToolCall {
                        name: name.to_string(),
                        arguments,
                    })?;
                }
            }
        }
        if let Some(usage) = value.get("usage") {
            let input_tokens = usage
                .get("prompt_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let output_tokens = usage
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            emit(GenerateEvent::Usage {
                input_tokens,
                output_tokens,
            })?;
        }
        let reason = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .unwrap_or("stop");
        emit(GenerateEvent::Finish {
            reason: reason.to_string(),
        })?;
        Ok(())
    }

    /// Parse an SSE stream, accumulating tool-call deltas across chunks and
    /// emitting usage/finish when the upstream signals them.
    fn consume_sse(
        &self,
        response: ureq::http::Response<ureq::Body>,
        cancel: &Arc<AtomicBool>,
        emit: &mut dyn FnMut(GenerateEvent) -> Result<(), ProviderError>,
    ) -> Result<(), ProviderError> {
        let mut reader = BufReader::new(response.into_body().into_reader());
        let mut data_lines: Vec<String> = Vec::new();
        let mut tool_calls: BTreeMap<usize, ToolCallAccumulator> = BTreeMap::new();
        let mut finished = false;
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(ProviderError::new(
                    ProviderErrorKind::Cancelled,
                    "generation cancelled",
                ));
            }
            let mut line = String::new();
            let read = reader
                .read_line(&mut line)
                .map_err(|e| ProviderError::new(ProviderErrorKind::Transport, e.to_string()))?;
            if read == 0 {
                break;
            }
            let trimmed = line.trim_end_matches(['\n', '\r']);
            if trimmed.is_empty() {
                if !data_lines.is_empty() {
                    let data = data_lines.join("\n");
                    data_lines.clear();
                    if data == "[DONE]" {
                        finished = true;
                        break;
                    }
                    self.handle_sse_event(&data, &mut tool_calls, emit)?;
                }
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("data:") {
                data_lines.push(rest.trim_start().to_string());
            }
            // Ignore `event:`, `id:`, `retry:` and comments.
        }
        if !finished {
            // Stream ended without [DONE]; surface as a transport error unless
            // the caller already emitted a finish event.
            return Err(ProviderError::new(
                ProviderErrorKind::Transport,
                "upstream stream ended without [DONE]",
            ));
        }
        Ok(())
    }

    fn handle_sse_event(
        &self,
        data: &str,
        tool_calls: &mut BTreeMap<usize, ToolCallAccumulator>,
        emit: &mut dyn FnMut(GenerateEvent) -> Result<(), ProviderError>,
    ) -> Result<(), ProviderError> {
        let value: Value = serde_json::from_str(data).map_err(|e| {
            ProviderError::new(
                ProviderErrorKind::Transport,
                format!("invalid SSE chunk: {e}"),
            )
        })?;
        if let Some(usage) = value.get("usage") {
            if usage.get("prompt_tokens").is_some() || usage.get("completion_tokens").is_some() {
                let input_tokens = usage
                    .get("prompt_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let output_tokens = usage
                    .get("completion_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                emit(GenerateEvent::Usage {
                    input_tokens,
                    output_tokens,
                })?;
            }
        }
        let Some(choice) = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
        else {
            return Ok(());
        };
        if let Some(delta) = choice.get("delta") {
            if let Some(content) = delta.get("content").and_then(Value::as_str) {
                if !content.is_empty() {
                    emit(GenerateEvent::Delta {
                        text: content.to_string(),
                    })?;
                }
            }
            if let Some(chunks) = delta.get("tool_calls").and_then(Value::as_array) {
                for tool_call in chunks {
                    let index =
                        tool_call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                    let entry = tool_calls.entry(index).or_default();
                    let function = tool_call.get("function");
                    if let Some(name) = function.and_then(|f| f.get("name")).and_then(Value::as_str)
                    {
                        entry.name.push_str(name);
                    }
                    if let Some(arguments) = function
                        .and_then(|f| f.get("arguments"))
                        .and_then(Value::as_str)
                    {
                        entry.arguments.push_str(arguments);
                    }
                }
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            // Flush any accumulated tool calls before the finish event.
            let indices: Vec<usize> = tool_calls.keys().copied().collect();
            for index in indices {
                let accumulator = tool_calls.remove(&index).unwrap_or_default();
                if !accumulator.name.is_empty() {
                    emit(GenerateEvent::ToolCall {
                        name: accumulator.name,
                        arguments: parse_arguments(&accumulator.arguments),
                    })?;
                }
            }
            emit(GenerateEvent::Finish {
                reason: reason.to_string(),
            })?;
        }
        Ok(())
    }
}

#[derive(Default)]
struct ToolCallAccumulator {
    name: String,
    arguments: String,
}

/// Best-effort argument parsing: JSON arguments become the parsed value,
/// otherwise the raw string is returned as a JSON string.
fn parse_arguments(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

fn classify_status(status: u16) -> ProviderErrorKind {
    match status {
        400..=400 => ProviderErrorKind::InvalidRequest,
        401 | 403 => ProviderErrorKind::Authentication,
        408 | 429 => ProviderErrorKind::RateLimited,
        500 | 502 | 503 | 504 => ProviderErrorKind::ServerError,
        _ => ProviderErrorKind::Transport,
    }
}

fn retry_after_header(headers: &ureq::http::HeaderMap) -> Option<Duration> {
    let value = headers.get("retry-after")?.to_str().ok()?;
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Some(Duration::from_secs(seconds.min(300)));
    }
    // HTTP-date form is uncommon from API providers; ignore rather than misparse.
    None
}

fn parse_embedding_response(
    request: &EmbedRequest,
    value: &Value,
) -> Result<EmbeddingResponse, ProviderError> {
    let data = value.get("data").and_then(Value::as_array).ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::Transport,
            "embedding response omitted data",
        )
    })?;
    let mut embeddings = Vec::with_capacity(data.len());
    for entry in data {
        let index = entry
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(embeddings.len() as u64) as usize;
        let values = entry
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::Transport,
                    "embedding entry omitted vector",
                )
            })?
            .iter()
            .filter_map(Value::as_f64)
            .map(|v| v as f32)
            .collect();
        embeddings.push(super::Embedding { index, values });
    }
    let usage = value.get("usage");
    let input_tokens = usage
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Ok(EmbeddingResponse {
        request_id: request.request_id.clone(),
        model: request.model.clone(),
        embeddings,
        usage: super::EmbedUsage { input_tokens },
    })
}

fn atomic_json(path: &Path, value: &impl Serialize) -> Result<(), ProviderError> {
    let parent = path.parent().ok_or_else(|| {
        ProviderError::new(ProviderErrorKind::Transport, "config path has no parent")
    })?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| ProviderError::new(ProviderErrorKind::Transport, e.to_string()))?;
    use std::io::Write as _;
    temp.write_all(
        &serde_json::to_vec_pretty(value)
            .map_err(|e| ProviderError::new(ProviderErrorKind::Transport, e.to_string()))?,
    )
    .map_err(|e| ProviderError::new(ProviderErrorKind::Transport, e.to_string()))?;
    temp.as_file()
        .sync_all()
        .map_err(|e| ProviderError::new(ProviderErrorKind::Transport, e.to_string()))?;
    crate::platform::native()
        .atomic_replace(temp.path(), path)
        .map_err(|e| ProviderError::new(ProviderErrorKind::Transport, e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(id: &str, endpoint: &str) -> RemoteProviderConfig {
        RemoteProviderConfig {
            id: id.into(),
            kind: ProviderKind::OpenAiCompatible,
            endpoint: endpoint.into(),
            secret_ref: SecretRef {
                service: "com.alex.model-provider".into(),
                account: id.into(),
            },
            default_model: None,
            organization: None,
            timeout_ms: 60_000,
            max_retries: 2,
            enabled: true,
        }
    }

    #[test]
    fn secret_ref_validation_rejects_empty_and_whitespace() {
        assert!(
            SecretRef {
                service: "s".into(),
                account: "a".into()
            }
            .validate()
            .is_ok()
        );
        assert!(
            SecretRef {
                service: "".into(),
                account: "a".into()
            }
            .validate()
            .is_err()
        );
        assert!(
            SecretRef {
                service: "s".into(),
                account: "a b".into()
            }
            .validate()
            .is_err()
        );
        assert!(
            SecretRef {
                service: "s".into(),
                account: "a\0b".into()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn endpoint_validation_requires_https_or_loopback() {
        assert!(validate_endpoint("https://api.openai.com/v1").is_ok());
        assert!(validate_endpoint("http://127.0.0.1:8080/v1").is_ok());
        assert!(validate_endpoint("http://localhost:8080/v1").is_ok());
        assert!(validate_endpoint("http://example.com/v1").is_err());
        assert!(validate_endpoint("https://user:pass@example.com").is_err());
        assert!(validate_endpoint("ftp://example.com").is_err());
    }

    #[test]
    fn provider_config_validation_bounds_fields() {
        let mut c = config("openai-main", "https://api.openai.com/v1");
        assert!(c.validate().is_ok());
        c.timeout_ms = 0;
        assert!(c.validate().is_err());
        c.timeout_ms = 60_000;
        c.max_retries = 11;
        assert!(c.validate().is_err());
    }

    #[test]
    fn remote_model_id_parses_provider_and_name() {
        assert_eq!(
            parse_remote_model("remote/openai-main/gpt-4.1"),
            Some(("openai-main", "gpt-4.1"))
        );
        assert_eq!(parse_remote_model("local/qwen@1"), None);
        assert_eq!(parse_remote_model("remote/nomodel"), None);
    }

    #[test]
    fn config_store_persists_and_round_trips() {
        let temp = tempfile::tempdir().unwrap();
        let store = ProviderConfigStore::open(temp.path()).unwrap();
        let c = config("openai-main", "https://api.openai.com/v1");
        store.upsert(&c).unwrap();
        assert_eq!(store.list().unwrap(), vec![c.clone()]);
        // Reopen and confirm persistence.
        let reopened = ProviderConfigStore::open(temp.path()).unwrap();
        assert_eq!(reopened.list().unwrap(), vec![c.clone()]);
        assert!(reopened.remove("openai-main").unwrap());
        assert!(reopened.list().unwrap().is_empty());
    }

    #[test]
    fn config_never_serializes_a_key() {
        // The config type has no apiKey field at all; this guards the shape.
        let c = config("openai-main", "https://api.openai.com/v1");
        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.to_ascii_lowercase().contains("apikey"));
        assert!(json.contains("secretRef"));
    }

    #[test]
    fn retry_delay_honors_retry_after() {
        let error = ProviderError {
            kind: ProviderErrorKind::RateLimited,
            status: Some(429),
            retry_after: Some(Duration::from_secs(3)),
            message: "limited".into(),
        };
        assert_eq!(retry_delay(&error, 1), Duration::from_secs(3));
    }

    #[test]
    fn retryability_classification() {
        assert!(ProviderError::with_status(ProviderErrorKind::ServerError, 500, "x").retryable());
        assert!(ProviderError::with_status(ProviderErrorKind::RateLimited, 429, "x").retryable());
        assert!(
            !ProviderError::with_status(ProviderErrorKind::InvalidRequest, 400, "x").retryable()
        );
        assert!(
            !ProviderError::with_status(ProviderErrorKind::Authentication, 401, "x").retryable()
        );
    }

    #[test]
    fn circuit_breaker_opens_and_recovers() {
        let mut breaker = CircuitBreaker::new();
        assert!(breaker.allow());
        for _ in 0..CIRCUIT_BREAKER_THRESHOLD {
            breaker.record_failure();
        }
        assert_eq!(breaker.snapshot().0, CircuitState::Open);
        assert!(!breaker.allow());
        breaker.opened_at =
            Some(Instant::now() - CIRCUIT_BREAKER_COOLDOWN - Duration::from_secs(1));
        assert!(breaker.allow()); // half-open probe
        breaker.record_success();
        assert_eq!(breaker.snapshot(), (CircuitState::Closed, 0));
    }
}

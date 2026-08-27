//! Stable principals, authenticated session identities, and delegation chains.
//!
//! These types are deliberately policy-engine agnostic. They establish the
//! identity facts that authorization can consume without trusting arbitrary
//! strings supplied by an application or an AI tool.

use std::{collections::BTreeMap, fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MAX_ACTOR_HOPS: usize = 16;
const MAX_PRINCIPAL_ID_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PrincipalKind {
    User,
    Application,
    Plugin,
    AgentRun,
    Service,
    McpServer,
    ModelProvider,
    NativeWorker,
    Publisher,
    Administrator,
    System,
}

impl PrincipalKind {
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Application => "app",
            Self::Plugin => "plugin",
            Self::AgentRun => "agent",
            Self::Service => "service",
            Self::McpServer => "mcp",
            Self::ModelProvider => "model",
            Self::NativeWorker => "worker",
            Self::Publisher => "publisher",
            Self::Administrator => "admin",
            Self::System => "system",
        }
    }

    fn from_prefix(value: &str) -> Option<Self> {
        Some(match value {
            "user" => Self::User,
            "app" => Self::Application,
            "plugin" => Self::Plugin,
            "agent" => Self::AgentRun,
            "service" => Self::Service,
            "mcp" => Self::McpServer,
            "model" => Self::ModelProvider,
            "worker" => Self::NativeWorker,
            "publisher" => Self::Publisher,
            "admin" => Self::Administrator,
            "system" => Self::System,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PrincipalId(String);

impl PrincipalId {
    pub fn new(kind: PrincipalKind, subject: impl AsRef<str>) -> Result<Self, IdentityError> {
        Self::from_str(&format!("{}:{}", kind.prefix(), subject.as_ref()))
    }

    pub fn application(app_id: &str) -> Result<Self, IdentityError> {
        Self::new(PrincipalKind::Application, app_id)
    }

    pub fn service(app_id: &str, service: &str) -> Result<Self, IdentityError> {
        Self::new(PrincipalKind::Service, format!("{app_id}/{service}"))
    }

    pub fn kind(&self) -> PrincipalKind {
        PrincipalKind::from_prefix(self.0.split_once(':').expect("validated principal id").0)
            .expect("validated principal kind")
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PrincipalId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for PrincipalId {
    type Err = IdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() || value.len() > MAX_PRINCIPAL_ID_BYTES {
            return Err(IdentityError::InvalidPrincipalId);
        }
        let (prefix, subject) = value
            .split_once(':')
            .ok_or(IdentityError::InvalidPrincipalId)?;
        PrincipalKind::from_prefix(prefix).ok_or(IdentityError::UnknownPrincipalKind)?;
        if subject.is_empty()
            || subject.contains("..")
            || subject.starts_with('/')
            || subject.ends_with('/')
            || subject.chars().any(|character| {
                character.is_control()
                    || character.is_whitespace()
                    || character == '\\'
                    || !(character.is_ascii_alphanumeric()
                        || matches!(character, '.' | '-' | '_' | '/' | ':'))
            })
        {
            return Err(IdentityError::InvalidPrincipalId);
        }
        Ok(Self(value.to_owned()))
    }
}

impl TryFrom<String> for PrincipalId {
    type Error = IdentityError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::from_str(&value)
    }
}

impl From<PrincipalId> for String {
    fn from(value: PrincipalId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PrincipalStatus {
    Active,
    Disabled,
    Revoked,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Principal {
    pub id: PrincipalId,
    pub kind: PrincipalKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<PrincipalId>,
    pub status: PrincipalStatus,
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}

impl Principal {
    pub fn validate(&self) -> Result<(), IdentityError> {
        if self.id.kind() != self.kind {
            return Err(IdentityError::PrincipalKindMismatch);
        }
        if self.owner.as_ref() == Some(&self.id) {
            return Err(IdentityError::SelfOwnership);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthenticationMethod {
    WindowsToken,
    NamedPipePeer,
    AppLaunchToken,
    McpOAuth,
    PackageSignature,
    WorkerHandshake,
    InternalDaemon,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AssuranceLevel {
    Unverified,
    ProcessBound,
    UserBound,
    Cryptographic,
    AdministratorVerified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Identity {
    pub principal_id: PrincipalId,
    pub authentication: AuthenticationMethod,
    pub session_id: String,
    pub issued_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,
    pub assurance: AssuranceLevel,
    #[serde(default)]
    pub claims: BTreeMap<String, String>,
}

impl Identity {
    pub fn validate_at(&self, now_ms: u64) -> Result<(), IdentityError> {
        validate_token(&self.session_id)?;
        if self
            .expires_at_ms
            .is_some_and(|expiry| expiry <= self.issued_at_ms)
        {
            return Err(IdentityError::InvalidLifetime);
        }
        if self.expires_at_ms.is_some_and(|expiry| expiry <= now_ms) {
            return Err(IdentityError::ExpiredIdentity);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActorHop {
    pub principal: PrincipalId,
    pub acting_for: PrincipalId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActorChain {
    pub initiator: PrincipalId,
    #[serde(default)]
    pub actors: Vec<ActorHop>,
}

impl ActorChain {
    pub fn new(initiator: PrincipalId) -> Self {
        Self {
            initiator,
            actors: Vec::new(),
        }
    }

    pub fn for_application(app_id: &str) -> Result<Self, IdentityError> {
        Ok(Self::new(PrincipalId::application(app_id)?))
    }

    pub fn effective_actor(&self) -> &PrincipalId {
        self.actors
            .last()
            .map(|hop| &hop.principal)
            .unwrap_or(&self.initiator)
    }

    pub fn delegate(
        mut self,
        principal: PrincipalId,
        delegation_id: Option<String>,
    ) -> Result<Self, IdentityError> {
        if let Some(value) = &delegation_id {
            validate_token(value)?;
        }
        let acting_for = self.effective_actor().clone();
        self.actors.push(ActorHop {
            principal,
            acting_for,
            delegation_id,
        });
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), IdentityError> {
        if self.actors.len() > MAX_ACTOR_HOPS {
            return Err(IdentityError::ActorChainTooLong);
        }
        let mut expected = &self.initiator;
        let mut seen = std::collections::BTreeSet::from([self.initiator.clone()]);
        for hop in &self.actors {
            if &hop.acting_for != expected {
                return Err(IdentityError::BrokenActorChain);
            }
            if !seen.insert(hop.principal.clone()) {
                return Err(IdentityError::ActorCycle);
            }
            if let Some(value) = &hop.delegation_id {
                validate_token(value)?;
            }
            expected = &hop.principal;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestIdentity {
    pub identity: Identity,
    pub actor_chain: ActorChain,
}

impl RequestIdentity {
    pub fn validate_at(&self, now_ms: u64) -> Result<(), IdentityError> {
        self.identity.validate_at(now_ms)?;
        self.actor_chain.validate()?;
        if self.identity.principal_id != self.actor_chain.initiator {
            return Err(IdentityError::InitiatorMismatch);
        }
        Ok(())
    }
}

fn validate_token(value: &str) -> Result<(), IdentityError> {
    if value.is_empty()
        || value.len() > 256
        || value
            .chars()
            .any(|character| !(character.is_ascii_alphanumeric() || matches!(character, '-' | '_')))
    {
        return Err(IdentityError::InvalidSessionOrGrantId);
    }
    Ok(())
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum IdentityError {
    #[error("principal id is invalid")]
    InvalidPrincipalId,
    #[error("principal kind is unknown")]
    UnknownPrincipalKind,
    #[error("principal id prefix does not match its kind")]
    PrincipalKindMismatch,
    #[error("a principal cannot own itself")]
    SelfOwnership,
    #[error("identity lifetime is invalid")]
    InvalidLifetime,
    #[error("identity has expired")]
    ExpiredIdentity,
    #[error("session or delegation id is invalid")]
    InvalidSessionOrGrantId,
    #[error("actor chain exceeds {MAX_ACTOR_HOPS} hops")]
    ActorChainTooLong,
    #[error("actor chain delegation is not contiguous")]
    BrokenActorChain,
    #[error("actor chain contains a principal cycle")]
    ActorCycle,
    #[error("authenticated principal is not the actor-chain initiator")]
    InitiatorMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_principal_ids_reject_escapes_and_kind_mismatches() {
        let app = PrincipalId::application("com.example.assistant").unwrap();
        assert_eq!(app.kind(), PrincipalKind::Application);
        assert!(PrincipalId::from_str("app:../escape").is_err());
        assert!(PrincipalId::from_str("unknown:value").is_err());
        let principal = Principal {
            id: app,
            kind: PrincipalKind::Service,
            tenant: None,
            owner: None,
            status: PrincipalStatus::Active,
            attributes: BTreeMap::new(),
        };
        assert_eq!(
            principal.validate(),
            Err(IdentityError::PrincipalKindMismatch)
        );
    }

    #[test]
    fn actor_chain_is_contiguous_acyclic_and_bounded() {
        let app = PrincipalId::application("com.example.assistant").unwrap();
        let agent =
            PrincipalId::new(PrincipalKind::AgentRun, "com.example.assistant/run_1").unwrap();
        let mcp =
            PrincipalId::new(PrincipalKind::McpServer, "com.example.assistant/files").unwrap();
        let chain = ActorChain::new(app.clone())
            .delegate(agent, Some("grant_1".into()))
            .unwrap()
            .delegate(mcp, Some("grant_2".into()))
            .unwrap();
        assert_eq!(chain.actors.len(), 2);
        assert_eq!(chain.actors[1].acting_for, chain.actors[0].principal);
        assert_eq!(
            chain.clone().delegate(app, None),
            Err(IdentityError::ActorCycle)
        );
    }

    #[test]
    fn request_identity_round_trips_and_checks_the_initiator() {
        let app = PrincipalId::application("com.example.assistant").unwrap();
        let request = RequestIdentity {
            identity: Identity {
                principal_id: app.clone(),
                authentication: AuthenticationMethod::AppLaunchToken,
                session_id: "session_123".into(),
                issued_at_ms: 10,
                expires_at_ms: Some(100),
                assurance: AssuranceLevel::ProcessBound,
                claims: BTreeMap::new(),
            },
            actor_chain: ActorChain::new(app),
        };
        request.validate_at(20).unwrap();
        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(
            serde_json::from_value::<RequestIdentity>(encoded).unwrap(),
            request
        );
        assert_eq!(
            request.validate_at(100),
            Err(IdentityError::ExpiredIdentity)
        );
    }
}

//! Structured authorization requests and the first fail-closed shadow evaluator.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::identity::{IdentityError, PrincipalId, RequestIdentity};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResourceKind {
    Capability,
    File,
    NetworkOrigin,
    McpTool,
    Model,
    AgentRun,
    Service,
    Process,
    Window,
    KnowledgeBase,
    KnowledgeDocument,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Resource {
    pub id: String,
    pub kind: ResourceKind,
    pub owner: PrincipalId,
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}

impl Resource {
    pub fn capability(owner: PrincipalId, permission: &str) -> Result<Self, PolicyError> {
        validate_name(permission)?;
        Ok(Self {
            id: format!("capability://{owner}/{permission}"),
            kind: ResourceKind::Capability,
            owner,
            attributes: BTreeMap::new(),
        })
    }

    pub fn validate(&self) -> Result<(), PolicyError> {
        if self.id.is_empty() || self.id.len() > 2_048 || !self.id.contains("://") {
            return Err(PolicyError::InvalidResource);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorizationRequest {
    pub request_id: String,
    pub caller: RequestIdentity,
    pub action: String,
    pub resource: Resource,
    #[serde(default)]
    pub context: BTreeMap<String, String>,
}

impl AuthorizationRequest {
    pub fn validate_at(&self, now_ms: u64) -> Result<(), PolicyError> {
        validate_token(&self.request_id)?;
        validate_name(&self.action)?;
        self.resource.validate()?;
        self.caller.validate_at(now_ms)?;
        if self.resource.owner != self.caller.actor_chain.initiator {
            return Err(PolicyError::CrossPrincipalResource);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Effect {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DecisionReason {
    AuthIdentityInvalid,
    AuthActionUnknown,
    AuthPlatformUnavailable,
    AuthNotDeclared,
    AuthUserDenied,
    AuthResourceDenied,
    AuthDelegationMissing,
    AuthAllowed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorizationDecision {
    pub effect: Effect,
    pub reason_code: DecisionReason,
    pub decision_id: String,
    #[serde(default)]
    pub obligations: Vec<String>,
}

impl AuthorizationDecision {
    pub fn allowed(&self) -> bool {
        self.effect == Effect::Allow
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompatibilityFacts {
    pub action_registered: bool,
    pub platform_supported: bool,
    pub manifest_declared: bool,
    pub user_granted: bool,
    pub resource_allowed: bool,
    pub delegation_valid: bool,
}

pub struct ShadowPolicyEngine;

impl ShadowPolicyEngine {
    pub fn evaluate(
        request: &AuthorizationRequest,
        facts: CompatibilityFacts,
        now_ms: u64,
    ) -> AuthorizationDecision {
        let validation_reason = match request.validate_at(now_ms) {
            Ok(()) => None,
            Err(PolicyError::InvalidAction) => Some(DecisionReason::AuthActionUnknown),
            Err(PolicyError::InvalidResource | PolicyError::CrossPrincipalResource) => {
                Some(DecisionReason::AuthResourceDenied)
            }
            Err(PolicyError::InvalidRequestId | PolicyError::Identity(_)) => {
                Some(DecisionReason::AuthIdentityInvalid)
            }
        };
        let reason = if let Some(reason) = validation_reason {
            reason
        } else if !facts.action_registered {
            DecisionReason::AuthActionUnknown
        } else if !facts.platform_supported {
            DecisionReason::AuthPlatformUnavailable
        } else if !facts.manifest_declared {
            DecisionReason::AuthNotDeclared
        } else if !facts.user_granted {
            DecisionReason::AuthUserDenied
        } else if !facts.resource_allowed {
            DecisionReason::AuthResourceDenied
        } else if !facts.delegation_valid {
            DecisionReason::AuthDelegationMissing
        } else {
            DecisionReason::AuthAllowed
        };
        AuthorizationDecision {
            effect: if reason == DecisionReason::AuthAllowed {
                Effect::Allow
            } else {
                Effect::Deny
            },
            reason_code: reason,
            decision_id: format!("shadow_{}", request.request_id),
            obligations: vec!["audit".into()],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShadowDifference {
    pub request_id: String,
    pub action: String,
    pub resource_id: String,
    pub legacy_allowed: bool,
    pub shadow_allowed: bool,
    pub shadow_reason: DecisionReason,
}

impl ShadowDifference {
    pub fn between(
        request: &AuthorizationRequest,
        legacy_allowed: bool,
        decision: &AuthorizationDecision,
    ) -> Option<Self> {
        (legacy_allowed != decision.allowed()).then(|| Self {
            request_id: request.request_id.clone(),
            action: request.action.clone(),
            resource_id: request.resource.id.clone(),
            legacy_allowed,
            shadow_allowed: decision.allowed(),
            shadow_reason: decision.reason_code,
        })
    }
}

fn validate_name(value: &str) -> Result<(), PolicyError> {
    if value.is_empty()
        || value.len() > 256
        || value.starts_with('.')
        || value.ends_with('.')
        || value.chars().any(|character| {
            !(character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_'))
        })
    {
        return Err(PolicyError::InvalidAction);
    }
    Ok(())
}

fn validate_token(value: &str) -> Result<(), PolicyError> {
    if value.is_empty()
        || value.len() > 256
        || value
            .chars()
            .any(|character| !(character.is_ascii_alphanumeric() || matches!(character, '-' | '_')))
    {
        return Err(PolicyError::InvalidRequestId);
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("authorization action is invalid")]
    InvalidAction,
    #[error("authorization request id is invalid")]
    InvalidRequestId,
    #[error("authorization resource is invalid")]
    InvalidResource,
    #[error("authorization resource belongs to another principal")]
    CrossPrincipalResource,
    #[error(transparent)]
    Identity(#[from] IdentityError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{
        ActorChain, AssuranceLevel, AuthenticationMethod, Identity, RequestIdentity,
    };

    fn request(action: &str) -> AuthorizationRequest {
        let app = PrincipalId::application("com.example.app").unwrap();
        AuthorizationRequest {
            request_id: "request_1".into(),
            caller: RequestIdentity {
                identity: Identity {
                    principal_id: app.clone(),
                    authentication: AuthenticationMethod::AppLaunchToken,
                    session_id: "session_1".into(),
                    issued_at_ms: 1,
                    expires_at_ms: None,
                    assurance: AssuranceLevel::ProcessBound,
                    claims: BTreeMap::new(),
                },
                actor_chain: ActorChain::new(app.clone()),
            },
            action: action.into(),
            resource: Resource::capability(app, action).unwrap(),
            context: BTreeMap::new(),
        }
    }

    #[test]
    fn shadow_engine_allows_only_the_full_compatibility_intersection() {
        let request = request("filesystem.read");
        let all = CompatibilityFacts {
            action_registered: true,
            platform_supported: true,
            manifest_declared: true,
            user_granted: true,
            resource_allowed: true,
            delegation_valid: true,
        };
        assert_eq!(
            ShadowPolicyEngine::evaluate(&request, all, 2).reason_code,
            DecisionReason::AuthAllowed
        );
        assert_eq!(
            ShadowPolicyEngine::evaluate(
                &request,
                CompatibilityFacts {
                    user_granted: false,
                    ..all
                },
                2,
            )
            .reason_code,
            DecisionReason::AuthUserDenied
        );
    }

    #[test]
    fn unknown_actions_and_cross_principal_resources_fail_closed() {
        let mut request = request("filesystem.read");
        let facts = CompatibilityFacts {
            action_registered: false,
            platform_supported: true,
            manifest_declared: true,
            user_granted: true,
            resource_allowed: true,
            delegation_valid: true,
        };
        assert_eq!(
            ShadowPolicyEngine::evaluate(&request, facts, 2).reason_code,
            DecisionReason::AuthActionUnknown
        );
        request.resource.owner = PrincipalId::application("com.other.app").unwrap();
        assert_eq!(
            ShadowPolicyEngine::evaluate(
                &request,
                CompatibilityFacts {
                    action_registered: true,
                    ..facts
                },
                2,
            )
            .reason_code,
            DecisionReason::AuthResourceDenied
        );
    }
}

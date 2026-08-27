//! In-memory delegated capabilities with attenuation, expiry and atomic use.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{identity::PrincipalId, policy::ResourceKind};

const MAX_PARENT_DEPTH: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScopeMatch {
    Exact,
    Prefix,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceScope {
    pub kind: ResourceKind,
    pub id: String,
    pub match_mode: ScopeMatch,
}

impl ResourceScope {
    pub fn exact(kind: ResourceKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
            match_mode: ScopeMatch::Exact,
        }
    }

    pub fn prefix(kind: ResourceKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
            match_mode: ScopeMatch::Prefix,
        }
    }

    fn matches(&self, kind: ResourceKind, id: &str) -> bool {
        self.kind == kind
            && match self.match_mode {
                ScopeMatch::Exact => self.id == id,
                ScopeMatch::Prefix => boundary_prefix(&self.id, id),
            }
    }

    fn contains(&self, child: &Self) -> bool {
        if self.kind != child.kind {
            return false;
        }
        match (self.match_mode, child.match_mode) {
            (ScopeMatch::Exact, ScopeMatch::Exact) => self.id == child.id,
            (ScopeMatch::Exact, ScopeMatch::Prefix) => false,
            (ScopeMatch::Prefix, _) => boundary_prefix(&self.id, &child.id),
        }
    }
}

fn boundary_prefix(prefix: &str, value: &str) -> bool {
    value == prefix
        || value.strip_prefix(prefix).is_some_and(|tail| {
            prefix.ends_with(['/', ':', '?', '#']) || tail.starts_with(['/', ':', '?', '#'])
        })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrantSpec {
    pub issuer: PrincipalId,
    pub grantee: PrincipalId,
    pub parent_id: Option<String>,
    pub actions: BTreeSet<String>,
    pub resources: Vec<ResourceScope>,
    pub expires_at_ms: u64,
    pub max_uses: Option<u32>,
    pub session_id: Option<String>,
    pub generation: u64,
    /// Burn a limited-use grant even when a claim mismatches its binding.
    pub consume_on_attempt: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GrantStatus {
    Active,
    Consumed,
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Grant {
    pub id: String,
    pub spec: GrantSpec,
    pub issued_at_ms: u64,
    pub remaining_uses: Option<u32>,
    pub status: GrantStatus,
    pub revoked_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantClaim<'a> {
    pub grantee: &'a PrincipalId,
    pub action: &'a str,
    pub resource_kind: ResourceKind,
    pub resource_id: &'a str,
    pub session_id: Option<&'a str>,
    pub generation: u64,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum GrantError {
    #[error("grant store lock poisoned")]
    StorePoisoned,
    #[error("grant is missing or already used")]
    Missing,
    #[error("grant expired")]
    Expired,
    #[error("grant was revoked")]
    Revoked,
    #[error("grant claim does not match its binding")]
    ClaimMismatch,
    #[error("invalid grant specification: {0}")]
    InvalidSpec(&'static str),
    #[error("child grant exceeds its parent scope")]
    NotAttenuated,
    #[error("grant parent chain is too deep")]
    ParentDepth,
    #[error("random grant identifier could not be generated: {0}")]
    Random(String),
}

#[derive(Debug, Clone, Default)]
pub struct GrantStore {
    grants: Arc<Mutex<BTreeMap<String, Grant>>>,
}

impl GrantStore {
    pub fn issue(&self, spec: GrantSpec) -> Result<String, GrantError> {
        validate_spec(&spec)?;
        let now = unix_time_ms();
        let mut grants = self.grants.lock().map_err(|_| GrantError::StorePoisoned)?;
        if let Some(parent_id) = &spec.parent_id {
            validate_parent(&grants, parent_id, &spec, now)?;
        }
        let id = random_id()?;
        grants.insert(
            id.clone(),
            Grant {
                remaining_uses: spec.max_uses,
                id: id.clone(),
                spec,
                issued_at_ms: now,
                status: GrantStatus::Active,
                revoked_reason: None,
            },
        );
        Ok(id)
    }

    /// Validation and use-count mutation happen under one lock, so concurrent
    /// callers cannot replay a one-shot capability.
    pub fn claim(&self, id: &str, claim: &GrantClaim<'_>) -> Result<(), GrantError> {
        let now = unix_time_ms();
        let mut grants = self.grants.lock().map_err(|_| GrantError::StorePoisoned)?;
        let validation = validate_claim(&grants, id, claim, now);
        let burn = grants
            .get(id)
            .is_some_and(|grant| grant.spec.consume_on_attempt);
        if validation.is_ok() || burn {
            consume(grants.get_mut(id).ok_or(GrantError::Missing)?);
        }
        validation
    }

    pub fn revoke(&self, id: &str, reason: impl Into<String>) -> Result<usize, GrantError> {
        let mut grants = self.grants.lock().map_err(|_| GrantError::StorePoisoned)?;
        if !grants.contains_key(id) {
            return Err(GrantError::Missing);
        }
        let reason = reason.into();
        let mut targets = vec![id.to_owned()];
        let mut index = 0;
        while index < targets.len() {
            let parent = &targets[index];
            let children: Vec<_> = grants
                .iter()
                .filter(|(_, grant)| grant.spec.parent_id.as_deref() == Some(parent))
                .map(|(id, _)| id.clone())
                .collect();
            targets.extend(children);
            index += 1;
        }
        for target in &targets {
            if let Some(grant) = grants.get_mut(target) {
                grant.status = GrantStatus::Revoked;
                grant.revoked_reason = Some(reason.clone());
            }
        }
        Ok(targets.len())
    }

    pub fn revoke_grantee(&self, grantee: &PrincipalId, reason: &str) -> usize {
        let mut grants = self.grants.lock().expect("grant store lock poisoned");
        let mut targets = grants
            .iter()
            .filter(|(_, grant)| {
                &grant.spec.grantee == grantee && grant.status == GrantStatus::Active
            })
            .map(|(id, _)| id.clone())
            .collect::<BTreeSet<_>>();
        loop {
            let children = grants
                .iter()
                .filter(|(_, grant)| {
                    grant
                        .spec
                        .parent_id
                        .as_ref()
                        .is_some_and(|id| targets.contains(id))
                })
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            let before = targets.len();
            targets.extend(children);
            if targets.len() == before {
                break;
            }
        }
        for id in &targets {
            if let Some(grant) = grants.get_mut(id) {
                grant.status = GrantStatus::Revoked;
                grant.revoked_reason = Some(reason.to_owned());
            }
        }
        targets.len()
    }

    pub fn get(&self, id: &str) -> Option<Grant> {
        self.grants.lock().ok()?.get(id).cloned()
    }

    /// Permanently forget one capability. This is intended for compatibility
    /// stores whose public contract treats explicit revocation as "unknown".
    pub fn remove(&self, id: &str) -> bool {
        self.grants
            .lock()
            .expect("grant store lock poisoned")
            .remove(id)
            .is_some()
    }

    /// Permanently remove grants for a principal and their descendants.
    pub fn remove_grantee(&self, grantee: &PrincipalId) -> usize {
        let mut grants = self.grants.lock().expect("grant store lock poisoned");
        let mut targets = grants
            .iter()
            .filter(|(_, grant)| &grant.spec.grantee == grantee)
            .map(|(id, _)| id.clone())
            .collect::<BTreeSet<_>>();
        loop {
            let children = grants
                .iter()
                .filter(|(_, grant)| {
                    grant
                        .spec
                        .parent_id
                        .as_ref()
                        .is_some_and(|id| targets.contains(id))
                })
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            let before = targets.len();
            targets.extend(children);
            if targets.len() == before {
                break;
            }
        }
        for id in &targets {
            grants.remove(id);
        }
        targets.len()
    }

    /// Remove expired capabilities and return the number reclaimed.
    pub fn sweep_expired(&self) -> usize {
        let now = unix_time_ms();
        let mut grants = self.grants.lock().expect("grant store lock poisoned");
        let before = grants.len();
        grants.retain(|_, grant| grant.spec.expires_at_ms > now);
        before.saturating_sub(grants.len())
    }
}

fn validate_spec(spec: &GrantSpec) -> Result<(), GrantError> {
    if spec.actions.is_empty() || spec.actions.iter().any(|a| a.is_empty() || a.len() > 256) {
        return Err(GrantError::InvalidSpec("actions are required"));
    }
    if spec.resources.is_empty()
        || spec
            .resources
            .iter()
            .any(|r| r.id.is_empty() || r.id.len() > 2_048)
    {
        return Err(GrantError::InvalidSpec("resource scopes are required"));
    }
    if spec.max_uses == Some(0) {
        return Err(GrantError::InvalidSpec("maxUses must be positive"));
    }
    Ok(())
}

fn validate_parent(
    grants: &BTreeMap<String, Grant>,
    parent_id: &str,
    child: &GrantSpec,
    now: u64,
) -> Result<(), GrantError> {
    let parent = grants.get(parent_id).ok_or(GrantError::Missing)?;
    validate_active(parent, now)?;
    if parent.spec.grantee != child.issuer
        || child.expires_at_ms > parent.spec.expires_at_ms
        || !child.actions.is_subset(&parent.spec.actions)
        || !child
            .resources
            .iter()
            .all(|c| parent.spec.resources.iter().any(|p| p.contains(c)))
        || !uses_attenuated(parent.remaining_uses, child.max_uses)
        || (parent.spec.session_id.is_some() && parent.spec.session_id != child.session_id)
        || child.generation != parent.spec.generation
    {
        return Err(GrantError::NotAttenuated);
    }
    let mut cursor = parent.spec.parent_id.as_deref();
    for _ in 0..MAX_PARENT_DEPTH {
        let Some(id) = cursor else {
            return Ok(());
        };
        cursor = grants
            .get(id)
            .ok_or(GrantError::Missing)?
            .spec
            .parent_id
            .as_deref();
    }
    Err(GrantError::ParentDepth)
}

fn uses_attenuated(parent: Option<u32>, child: Option<u32>) -> bool {
    match (parent, child) {
        (Some(p), Some(c)) => c <= p,
        (Some(_), None) => false,
        _ => true,
    }
}

fn validate_claim(
    grants: &BTreeMap<String, Grant>,
    id: &str,
    claim: &GrantClaim<'_>,
    now: u64,
) -> Result<(), GrantError> {
    let grant = grants.get(id).ok_or(GrantError::Missing)?;
    validate_active(grant, now)?;
    let mut cursor = grant.spec.parent_id.as_deref();
    for _ in 0..MAX_PARENT_DEPTH {
        let Some(parent_id) = cursor else {
            break;
        };
        let parent = grants.get(parent_id).ok_or(GrantError::Missing)?;
        validate_active(parent, now)?;
        cursor = parent.spec.parent_id.as_deref();
    }
    if cursor.is_some() {
        return Err(GrantError::ParentDepth);
    }
    if &grant.spec.grantee != claim.grantee
        || !grant.spec.actions.contains(claim.action)
        || !grant
            .spec
            .resources
            .iter()
            .any(|scope| scope.matches(claim.resource_kind, claim.resource_id))
        || grant.spec.session_id.as_deref() != claim.session_id
        || grant.spec.generation != claim.generation
    {
        return Err(GrantError::ClaimMismatch);
    }
    Ok(())
}

fn validate_active(grant: &Grant, now: u64) -> Result<(), GrantError> {
    match grant.status {
        GrantStatus::Revoked => return Err(GrantError::Revoked),
        GrantStatus::Consumed => return Err(GrantError::Missing),
        GrantStatus::Active => {}
    }
    if grant.spec.expires_at_ms <= now {
        return Err(GrantError::Expired);
    }
    Ok(())
}

fn consume(grant: &mut Grant) {
    if let Some(remaining) = &mut grant.remaining_uses {
        *remaining = remaining.saturating_sub(1);
        if *remaining == 0 {
            grant.status = GrantStatus::Consumed;
        }
    }
}

pub fn expires_after(ttl: Duration) -> u64 {
    unix_time_ms().saturating_add(ttl.as_millis().min(u64::MAX as u128) as u64)
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn random_id() -> Result<String, GrantError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| GrantError::Random(e.to_string()))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Barrier, thread};

    fn principal(kind: crate::identity::PrincipalKind, id: &str) -> PrincipalId {
        PrincipalId::new(kind, id).unwrap()
    }
    fn spec(issuer: PrincipalId, grantee: PrincipalId) -> GrantSpec {
        GrantSpec {
            issuer,
            grantee,
            parent_id: None,
            actions: BTreeSet::from(["mcp.invoke".into()]),
            resources: vec![ResourceScope::exact(
                ResourceKind::McpTool,
                "mcp://server/tool",
            )],
            expires_at_ms: expires_after(Duration::from_secs(60)),
            max_uses: Some(1),
            session_id: None,
            generation: 1,
            consume_on_attempt: true,
        }
    }

    #[test]
    fn one_shot_claim_is_atomic_under_concurrency() {
        let store = GrantStore::default();
        let app = principal(crate::identity::PrincipalKind::Application, "demo");
        let id = store
            .issue(spec(
                principal(crate::identity::PrincipalKind::System, "alexd"),
                app.clone(),
            ))
            .unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let store = store.clone();
                let id = id.clone();
                let app = app.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    store.claim(
                        &id,
                        &GrantClaim {
                            grantee: &app,
                            action: "mcp.invoke",
                            resource_kind: ResourceKind::McpTool,
                            resource_id: "mcp://server/tool",
                            session_id: None,
                            generation: 1,
                        },
                    )
                })
            })
            .collect();
        barrier.wait();
        assert_eq!(
            handles
                .into_iter()
                .map(|h| h.join().unwrap().is_ok())
                .filter(|ok| *ok)
                .count(),
            1
        );
    }

    #[test]
    fn child_must_attenuate_and_parent_revocation_cascades() {
        let store = GrantStore::default();
        let system = principal(crate::identity::PrincipalKind::System, "alexd");
        let app = principal(crate::identity::PrincipalKind::Application, "demo");
        let agent = principal(crate::identity::PrincipalKind::AgentRun, "run-1");
        let mut parent_spec = spec(system, app.clone());
        parent_spec.max_uses = Some(3);
        parent_spec.resources = vec![ResourceScope::prefix(ResourceKind::McpTool, "mcp://server")];
        let parent = store.issue(parent_spec.clone()).unwrap();
        let mut child_spec = spec(app, agent.clone());
        child_spec.parent_id = Some(parent.clone());
        child_spec.expires_at_ms = parent_spec.expires_at_ms;
        let child = store.issue(child_spec).unwrap();
        let mut invalid = spec(parent_spec.grantee, agent);
        invalid.parent_id = Some(parent.clone());
        invalid.expires_at_ms = parent_spec.expires_at_ms;
        invalid.actions.insert("process.spawn".into());
        assert_eq!(store.issue(invalid), Err(GrantError::NotAttenuated));
        assert_eq!(store.revoke(&parent, "logout").unwrap(), 2);
        assert_eq!(store.get(&child).unwrap().status, GrantStatus::Revoked);
    }
}

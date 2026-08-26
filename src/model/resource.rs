//! Memory budgeting, request concurrency and LRU eviction for local models.

use super::ModelError;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceBudget {
    pub memory_bytes: u64,
    pub max_loaded_models: usize,
    pub max_concurrent_requests_per_model: u32,
}

impl Default for ResourceBudget {
    fn default() -> Self {
        let detected = crate::model::hardware::discover()
            .devices
            .into_iter()
            .find(|device| device.kind == "cpu")
            .and_then(|device| device.memory_mb)
            .map(|mb| mb.saturating_mul(1024 * 1024).saturating_mul(3) / 4)
            .unwrap_or(8 * 1024 * 1024 * 1024);
        Self {
            memory_bytes: detected.max(512 * 1024 * 1024),
            max_loaded_models: 3,
            max_concurrent_requests_per_model: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelAllocation {
    pub model_id: String,
    pub worker: String,
    pub memory_bytes: u64,
    pub active_requests: u32,
    pub last_used_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceStatus {
    pub budget: ResourceBudget,
    pub allocated_bytes: u64,
    pub models: Vec<ModelAllocation>,
}

#[derive(Clone)]
pub struct ResourceGovernor {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    budget: ResourceBudget,
    models: BTreeMap<String, ModelAllocation>,
}

impl ResourceGovernor {
    pub fn new(budget: ResourceBudget) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                budget,
                models: BTreeMap::new(),
            })),
        }
    }

    pub fn reserve(
        &self,
        model_id: &str,
        worker: &str,
        memory_bytes: u64,
    ) -> Result<Vec<ModelAllocation>, ModelError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| ModelError::Worker("model resource lock poisoned".into()))?;
        if memory_bytes > inner.budget.memory_bytes {
            return Err(ModelError::Worker(format!(
                "model needs {memory_bytes} bytes but budget is {}",
                inner.budget.memory_bytes
            )));
        }
        let mut prospective = inner.models.clone();
        prospective.remove(model_id);
        let mut evicted = Vec::new();
        loop {
            let allocated = prospective
                .values()
                .map(|item| item.memory_bytes)
                .sum::<u64>();
            if allocated.saturating_add(memory_bytes) <= inner.budget.memory_bytes
                && prospective.len() < inner.budget.max_loaded_models
            {
                break;
            }
            let candidate = prospective
                .values()
                .filter(|item| item.active_requests == 0)
                .min_by_key(|item| item.last_used_ms)
                .map(|item| item.model_id.clone())
                .ok_or_else(|| {
                    ModelError::Worker(
                        "model resource budget is exhausted by active requests".into(),
                    )
                })?;
            if let Some(item) = prospective.remove(&candidate) {
                evicted.push(item);
            }
        }
        inner.models = prospective;
        inner.models.insert(
            model_id.into(),
            ModelAllocation {
                model_id: model_id.into(),
                worker: worker.into(),
                memory_bytes,
                active_requests: 0,
                last_used_ms: now_ms(),
            },
        );
        Ok(evicted)
    }

    pub fn acquire(&self, model_id: &str) -> Result<RequestPermit, ModelError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| ModelError::Worker("model resource lock poisoned".into()))?;
        let limit = inner.budget.max_concurrent_requests_per_model;
        let item = inner
            .models
            .get_mut(model_id)
            .ok_or_else(|| ModelError::Worker("model has no resource allocation".into()))?;
        if item.active_requests >= limit {
            return Err(ModelError::Worker(format!(
                "model concurrency limit ({limit}) reached"
            )));
        }
        item.active_requests += 1;
        item.last_used_ms = now_ms();
        Ok(RequestPermit {
            governor: self.clone(),
            model_id: model_id.into(),
        })
    }

    pub fn release(&self, model_id: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.models.remove(model_id);
        }
    }

    pub fn status(&self) -> ResourceStatus {
        let inner = self.inner.lock().expect("model resource lock poisoned");
        ResourceStatus {
            budget: inner.budget,
            allocated_bytes: inner.models.values().map(|item| item.memory_bytes).sum(),
            models: inner.models.values().cloned().collect(),
        }
    }
}

pub struct RequestPermit {
    governor: ResourceGovernor,
    model_id: String,
}
impl Drop for RequestPermit {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.governor.inner.lock()
            && let Some(item) = inner.models.get_mut(&self.model_id)
        {
            item.active_requests = item.active_requests.saturating_sub(1);
            item.last_used_ms = now_ms();
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn evicts_idle_lru_and_never_active_model() {
        let governor = ResourceGovernor::new(ResourceBudget {
            memory_bytes: 10,
            max_loaded_models: 2,
            max_concurrent_requests_per_model: 1,
        });
        governor.reserve("a", "worker", 5).unwrap();
        let permit = governor.acquire("a").unwrap();
        governor.reserve("b", "worker", 5).unwrap();
        let error = governor.reserve("c", "worker", 6).unwrap_err();
        assert!(error.to_string().contains("active"));
        drop(permit);
        let evicted = governor.reserve("c", "worker", 6).unwrap();
        assert_eq!(evicted.len(), 2);
        assert_eq!(governor.status().allocated_bytes, 6);
    }

    #[test]
    fn request_limit_is_released_by_raii() {
        let governor = ResourceGovernor::new(ResourceBudget {
            memory_bytes: 10,
            max_loaded_models: 1,
            max_concurrent_requests_per_model: 1,
        });
        governor.reserve("a", "worker", 5).unwrap();
        let permit = governor.acquire("a").unwrap();
        assert!(governor.acquire("a").is_err());
        drop(permit);
        assert!(governor.acquire("a").is_ok());
    }
}

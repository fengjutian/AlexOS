use std::time::Duration;

use serde_json::{Value, json};

use crate::{
    container::{ContainerFilter, ContainerService, CreateRequest, DefaultContainerService},
    permission::Permission,
};

use super::super::{ApiResult, ApiRouter, parse_params};

impl ApiRouter {
    fn container_service(&self) -> Result<&DefaultContainerService, (&'static str, String)> {
        self.require_plugin()?;
        self.require_permission(
            |permission| matches!(permission, Permission::SystemManageApps),
            "system.manageApps",
        )?;
        self.container_service.as_deref().ok_or((
            "CONTAINER_UNAVAILABLE",
            "container service requires a configured system install root".into(),
        ))
    }

    fn container_instance_id<'a>(&self, params: &'a Value) -> Result<&'a str, (&'static str, String)> {
        params.get("instanceId").and_then(Value::as_str).filter(|v| !v.is_empty())
            .ok_or(("INVALID_PARAMS", "missing `instanceId`".into()))
    }

    fn container_result<T: serde::Serialize>(result: Result<T, crate::container::ContainerError>) -> ApiResult {
        result.and_then(|value| serde_json::to_value(value)
            .map_err(|error| crate::container::ContainerError::Backend(error.to_string())))
            .map_err(|error| ("CONTAINER_ERROR", error.to_string()))
    }

    pub(crate) fn system_container_create(&self, params: &Value) -> ApiResult {
        let request: CreateRequest = parse_params(params)?;
        Self::container_result(self.container_service()?.create(request.into_spec()))
    }
    pub(crate) fn system_container_start(&self, params: &Value) -> ApiResult {
        Self::container_result(self.container_service()?.start(self.container_instance_id(params)?))
    }
    pub(crate) fn system_container_stop(&self, params: &Value) -> ApiResult {
        let timeout = params.get("timeoutMs").and_then(Value::as_u64).unwrap_or(5_000).clamp(100, 60_000);
        Self::container_result(self.container_service()?.stop(self.container_instance_id(params)?, Duration::from_millis(timeout)))
    }
    pub(crate) fn system_container_restart(&self, params: &Value) -> ApiResult {
        Self::container_result(self.container_service()?.restart(self.container_instance_id(params)?))
    }
    pub(crate) fn system_container_remove(&self, params: &Value) -> ApiResult {
        self.container_service()?.remove(self.container_instance_id(params)?, params.get("deleteData").and_then(Value::as_bool).unwrap_or(false))
            .map(|_| json!({ "removed": true })).map_err(|e| ("CONTAINER_ERROR", e.to_string()))
    }
    pub(crate) fn system_container_inspect(&self, params: &Value) -> ApiResult {
        Self::container_result(self.container_service()?.inspect(self.container_instance_id(params)?))
    }
    pub(crate) fn system_container_list(&self, params: &Value) -> ApiResult {
        let filter: ContainerFilter = parse_params(params)?;
        self.container_service()?.list(&filter).map(|containers| json!({ "containers": containers }))
            .map_err(|e| ("CONTAINER_ERROR", e.to_string()))
    }
    pub(crate) fn system_container_logs(&self, params: &Value) -> ApiResult {
        let tail = params.get("tail").and_then(Value::as_u64).unwrap_or(200).clamp(1, 5_000) as usize;
        self.container_service()?.logs(self.container_instance_id(params)?, tail)
            .map(|entries| json!({ "entries": entries })).map_err(|e| ("CONTAINER_ERROR", e.to_string()))
    }
}

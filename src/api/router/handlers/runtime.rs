use super::super::*;

impl ApiRouter {
    // ------------------------------------------------------------------
    // Runtime
    // ------------------------------------------------------------------

    pub(crate) fn runtime_invoke(
        &self,
        request_id: &str,
        params: &Value,
        deadline_ms: Option<u64>,
    ) -> ApiResult {
        if !self.permission_granted("runtime.invoke")
            || !self
                .manifest
                .permissions
                .iter()
                .any(|permission| matches!(permission, Permission::RuntimeInvoke))
        {
            return Err(("PERMISSION_DENIED", "runtime.invoke is not allowed".into()));
        }
        let params: RuntimeInvokeParams = parse_params(params)?;
        let runtime = self
            .runtime
            .as_ref()
            .ok_or(("RUNTIME_UNAVAILABLE", "application has no backend".into()))?;
        let timeout = deadline_ms
            .map(|deadline| Duration::from_millis(deadline.saturating_sub(now_ms())))
            .map(|timeout| timeout.min(DEFAULT_RUNTIME_TIMEOUT))
            .unwrap_or(DEFAULT_RUNTIME_TIMEOUT);
        // The cancellation token is bound to the IPC request
        // id; the page sends `runtime.cancel { requestId }`
        // and we flip the token. The runtime is unaffected —
        // each call is independent.
        let _ = self.cancel_inflight(request_id);
        runtime
            .invoke(request_id, &params.method, &params.params, timeout)
            .map_err(|error| match error {
                RuntimeError::Timeout(_) => ("DEADLINE_EXCEEDED", error.to_string()),
                _ => ("RUNTIME_FAILURE", error.to_string()),
            })
    }

    pub(crate) fn runtime_status(&self) -> ApiResult {
        self.require_runtime_manage()?;
        let runtime = self
            .runtime
            .as_ref()
            .ok_or(("RUNTIME_UNAVAILABLE", "application has no backend".into()))?;
        runtime
            .status(Duration::from_secs(2))
            .and_then(|status| {
                serde_json::to_value(status)
                    .map_err(|error| RuntimeError::Protocol(error.to_string()))
            })
            .map_err(|error| ("RUNTIME_FAILURE", error.to_string()))
    }

    pub(crate) fn runtime_restart(&self) -> ApiResult {
        self.require_runtime_manage()?;
        let runtime = self
            .runtime
            .as_ref()
            .ok_or(("RUNTIME_UNAVAILABLE", "application has no backend".into()))?;
        runtime
            .restart(Duration::from_secs(5))
            .and_then(|status| {
                serde_json::to_value(status)
                    .map_err(|error| RuntimeError::Protocol(error.to_string()))
            })
            .map_err(|error| ("RUNTIME_FAILURE", error.to_string()))
    }

    pub(crate) fn runtime_cancel(&self, params: &Value) -> ApiResult {
        if !self.permission_granted("runtime.invoke") {
            return Err(("PERMISSION_DENIED", "runtime.invoke was revoked".into()));
        }
        let params: RuntimeCancelParams = parse_params(params)?;
        let cancelled = self.cancel_inflight(&params.request_id);
        Ok(json!({ "cancelled": cancelled }))
    }
}

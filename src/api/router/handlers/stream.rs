use super::super::*;
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StreamIdParams {
    stream_id: String,
    #[serde(default)]
    wait_ms: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StreamCreditParams {
    stream_id: String,
    bytes: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StreamCancelParams {
    stream_id: String,
    #[serde(default)]
    reason: String,
}

impl ApiRouter {
    pub(crate) fn stream_credit(&self, params: &Value) -> ApiResult {
        let params: StreamCreditParams = parse_params(params)?;
        if let Some(result) = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.stream_credit(&params.stream_id, params.bytes))
        {
            return result.map_err(runtime_error);
        }
        self.stream_manager
            .grant_credit(&params.stream_id, params.bytes)
            .map(|available| json!({ "streamId": params.stream_id, "available": available }))
            .map_err(stream_error)
    }

    pub(crate) fn stream_read(&self, params: &Value) -> ApiResult {
        let params: StreamIdParams = parse_params(params)?;
        if let Some(result) = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.stream_read(&params.stream_id, params.wait_ms))
        {
            return result.map_err(runtime_error);
        }
        let chunk = self
            .stream_manager
            .pop_wait(
                &params.stream_id,
                std::time::Duration::from_millis(params.wait_ms.min(30_000).into()),
            )
            .map_err(stream_error)?;
        let terminal = self
            .stream_manager
            .terminal(&params.stream_id)
            .map_err(stream_error)?;
        Ok(match chunk {
            Some(chunk) => json!({
                "streamId": params.stream_id,
                "sequence": chunk.sequence,
                "bytes": chunk.data.len(),
                "dataBase64": base64::engine::general_purpose::STANDARD.encode(chunk.data),
            }),
            None => json!({
                "streamId": params.stream_id,
                "pending": terminal.is_none(),
                "terminal": terminal.map(terminal_json),
            }),
        })
    }

    pub(crate) fn stream_cancel(&self, params: &Value) -> ApiResult {
        let params: StreamCancelParams = parse_params(params)?;
        if let Some(result) = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.stream_cancel(&params.stream_id, &params.reason))
        {
            return result.map_err(runtime_error);
        }
        self.stream_manager
            .cancel(&params.stream_id, params.reason)
            .map(|_| json!({ "streamId": params.stream_id, "cancelled": true }))
            .map_err(stream_error)
    }
}

fn runtime_error(error: crate::runtime::RuntimeError) -> (&'static str, String) {
    ("STREAM_RUNTIME_FAILURE", error.to_string())
}

fn stream_error(error: crate::runtime::stream::StreamError) -> (&'static str, String) {
    let code = match error {
        crate::runtime::stream::StreamError::Backpressured { .. } => "STREAM_BACKPRESSURED",
        crate::runtime::stream::StreamError::NotFound(_) => "STREAM_NOT_FOUND",
        crate::runtime::stream::StreamError::Terminal => "STREAM_TERMINAL",
        _ => "STREAM_INVALID",
    };
    (code, error.to_string())
}

fn terminal_json(terminal: crate::runtime::stream::StreamTerminal) -> Value {
    match terminal {
        crate::runtime::stream::StreamTerminal::Completed => json!({ "kind": "completed" }),
        crate::runtime::stream::StreamTerminal::Failed { code, message } => {
            json!({ "kind": "failed", "error": { "code": code, "message": message } })
        }
        crate::runtime::stream::StreamTerminal::Cancelled { reason } => {
            json!({ "kind": "cancelled", "reason": reason })
        }
    }
}

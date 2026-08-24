use super::super::*;

impl ApiRouter {
    // ------------------------------------------------------------------
    // Clipboard
    // ------------------------------------------------------------------

    pub(crate) fn clipboard_read_text(&self) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::ClipboardRead),
            "clipboard.read",
        )?;
        self.desktop_services
            .clipboard_read_text()
            .map(|text| json!({ "text": text }))
            .map_err(|error| ("NATIVE_ERROR", error.to_string()))
    }

    pub(crate) fn clipboard_write_text(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::ClipboardWrite),
            "clipboard.write",
        )?;
        let params: ClipboardWriteParams = parse_params(params)?;
        if params.text.len() > MAX_IPC_MESSAGE_BYTES {
            return Err(("INVALID_PARAMS", "clipboard text exceeds 1 MiB".into()));
        }
        self.desktop_services
            .clipboard_write_text(params.text)
            .map(|_| json!({ "written": true }))
            .map_err(|error| ("NATIVE_ERROR", error.to_string()))
    }
}

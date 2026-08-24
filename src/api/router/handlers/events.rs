use super::super::*;

impl ApiRouter {
    // ------------------------------------------------------------------
    // Notification
    // ------------------------------------------------------------------

    pub(crate) fn notification_show(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::NotificationShow),
            "notification.show",
        )?;
        let params: NotificationParams = parse_params(params)?;
        if params.title.is_empty() || params.title.len() > 200 || params.body.len() > 1_000 {
            return Err((
                "INVALID_PARAMS",
                "notification title or body exceeds its limit".into(),
            ));
        }
        native::show_notification(&params.title, &params.body)
            .map(|_| json!({ "shown": true }))
            .map_err(|error| ("NATIVE_ERROR", error.to_string()))
    }

    // ------------------------------------------------------------------
    // Events
    // ------------------------------------------------------------------

    pub(crate) fn events_subscribe(
        &self,
        request_id: &str,
        params: &Value,
        window_id: Option<u64>,
    ) -> ApiResult {
        let parsed: SubscribeRequest = serde_json::from_value(params.clone())
            .map_err(|error| ("INVALID_PARAMS", error.to_string()))?;
        let filter = match parsed.filter {
            Some(value) => match serde_json::from_value::<SubscriptionFilter>(value) {
                Ok(filter) => Some(filter),
                Err(error) => {
                    return Err(("INVALID_PARAMS", format!("invalid filter: {error}")));
                }
            },
            None => None,
        };
        let id = self
            .event_bus
            .subscribe_for_window(&parsed.event, filter, window_id)
            .map_err(|error| ("SUBSCRIBE_FAILED", error.to_string()))?;
        let _ = request_id;
        Ok(json!({ "subscriptionId": id, "event": parsed.event }))
    }

    pub(crate) fn events_unsubscribe(&self, params: &Value) -> ApiResult {
        let parsed: UnsubscribeRequest = serde_json::from_value(params.clone())
            .map_err(|error| ("INVALID_PARAMS", error.to_string()))?;
        let removed = self
            .event_bus
            .unsubscribe(&parsed.subscription_id)
            .map_err(|error| ("SUBSCRIBE_FAILED", error.to_string()))?;
        Ok(json!({ "removed": removed }))
    }
}

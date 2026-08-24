//! Generic event / subscription protocol.
//!
//! Page code subscribes with `{ kind: "subscribe", event: "..." }` and
//! the host later delivers `{ kind: "event", subscriptionId, payload }`
//! messages back. Subscriptions are owned by the issuing `ApiRouter`,
//! so when the WebView reloads, the window is destroyed, or the
//! runtime exits, every subscription for that app is dropped in one
//! pass — no leftover events leak into a different app's session.
//!
//! `events::emit` deduplicates by `subscriptionId` and assigns a
//! monotonically increasing `sequence` per (router, subscriptionId)
//! pair so the page can detect dropped events even if the host
//! already flushed them. The 0.1 WebView IPC is fire-and-forget
//! (the page cannot NACK); a future binary channel can add
//! back-pressure.

use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::api::ipc::{PROTOCOL_VERSION, Request, Response};

/// Per-app subscription table. The router holds one of these and
/// passes the handle to whichever subsystem wants to push events
/// (file watcher, notification listener, runtime state, etc.).
#[derive(Debug, Default)]
pub struct EventBus {
    state: Mutex<BusState>,
    next_id: AtomicU64,
}

#[derive(Debug, Default)]
struct BusState {
    subscriptions: HashMap<String, Subscription>,
    /// Reverse index: which subscriptions are interested in a given
    /// event name. The page subscribes to one event at a time, so a
    /// simple list is enough — a `HashMap` would only matter if we
    /// later add filter multiplexing.
    by_event: HashMap<String, Vec<String>>,
    /// Pending deliveries drained by the shell layer on each
    /// tao event-loop tick. We keep this inside the bus (rather
    /// than scatter writes to a side channel) so the host can
    /// `drain` it without holding the per-event lock for long,
    /// and so a subscription that arrives after a delivery can
    /// still see it on the next tick.
    pending: Vec<PendingDelivery>,
}

/// One entry in the delivery queue. The shell hands the bus
/// the event name + the delivered event; the bus pushes the
/// (event, delivery) pair so the wire envelope can be built
/// without re-locking.
#[derive(Debug, Clone)]
struct PendingDelivery {
    event: String,
    delivered: DeliveredEvent,
}

#[derive(Debug, Clone)]
struct Subscription {
    id: String,
    event: String,
    sequence: u64,
    /// Optional payload filter the host applies before forwarding.
    /// Filesystem path filters match the selected file or any path
    /// below a selected directory.
    filter: Option<SubscriptionFilter>,
    window_id: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SubscriptionFilter {
    #[serde(rename = "path")]
    Path { value: String },
}

#[derive(Debug, Error)]
pub enum EventBusError {
    #[error("subscription id is empty")]
    EmptyId,
    #[error("subscription id is already registered")]
    Duplicate,
    #[error("unknown subscription id")]
    Unknown,
    #[error("invalid event payload: {0}")]
    InvalidPayload(String),
}

impl EventBus {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Subscribe `event` for this app. Returns the subscription id
    /// the page will use to unsubscribe and to correlate deliveries.
    pub fn subscribe(
        &self,
        event: &str,
        filter: Option<SubscriptionFilter>,
    ) -> Result<String, EventBusError> {
        self.subscribe_for_window(event, filter, None)
    }

    pub fn subscribe_for_window(
        &self,
        event: &str,
        filter: Option<SubscriptionFilter>,
        window_id: Option<u64>,
    ) -> Result<String, EventBusError> {
        if event.is_empty() {
            return Err(EventBusError::InvalidPayload("event name is empty".into()));
        }
        let mut state = self.state.lock().expect("event bus lock poisoned");
        let id = format!("sub-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        if state.subscriptions.contains_key(&id) {
            return Err(EventBusError::Duplicate);
        }
        state.subscriptions.insert(
            id.clone(),
            Subscription {
                id: id.clone(),
                event: event.to_owned(),
                sequence: 0,
                filter,
                window_id,
            },
        );
        state
            .by_event
            .entry(event.to_owned())
            .or_default()
            .push(id.clone());
        Ok(id)
    }

    /// Drop a subscription. Returns `Ok(false)` when the id is
    /// unknown — the page never has to track whether it asked
    /// first; the host treats both cases as success so a stray
    /// unsubscribe never breaks the page.
    pub fn unsubscribe(&self, id: &str) -> Result<bool, EventBusError> {
        if id.is_empty() {
            return Err(EventBusError::EmptyId);
        }
        let mut state = self.state.lock().expect("event bus lock poisoned");
        let Some(subscription) = state.subscriptions.remove(id) else {
            return Ok(false);
        };
        if let Some(list) = state.by_event.get_mut(&subscription.event) {
            list.retain(|existing| existing != id);
            if list.is_empty() {
                state.by_event.remove(&subscription.event);
            }
        }
        Ok(true)
    }

    /// Drop every subscription this app owns. Called when the
    /// WebView reloads, the window is destroyed, or the runtime
    /// exits. No-op for an empty bus.
    pub fn clear(&self) {
        let mut state = self.state.lock().expect("event bus lock poisoned");
        state.subscriptions.clear();
        state.by_event.clear();
        state.pending.clear();
    }

    /// Returns true if the app currently has at least one active
    /// subscription. Used to short-circuit file-watcher pumps and
    /// the like when nothing is listening.
    pub fn has_subscribers(&self, event: &str) -> bool {
        let state = self.state.lock().expect("event bus lock poisoned");
        state
            .by_event
            .get(event)
            .is_some_and(|list| !list.is_empty())
    }

    /// Construct a delivery for a single subscription that matched
    /// the event. The host drops subscriptions whose filter does
    /// not match the payload (so a per-path watch only fires on
    /// events inside the watched root). Returns `None` if no
    /// subscription matches or the filter rejects the payload.
    ///
    /// Side effect: each successful match is appended to the
    /// pending delivery queue so the shell layer can drain it
    /// on the next event-loop tick.
    pub fn deliver(&self, event: &str, payload: &Value) -> Vec<DeliveredEvent> {
        let mut state = self.state.lock().expect("event bus lock poisoned");
        let Some(list) = state.by_event.get(event).cloned() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for id in list {
            let Some(subscription) = state.subscriptions.get_mut(&id) else {
                continue;
            };
            if !filter_matches(subscription.filter.as_ref(), payload) {
                continue;
            }
            subscription.sequence = subscription.sequence.saturating_add(1);
            let delivered = DeliveredEvent {
                subscription_id: subscription.id.clone(),
                sequence: subscription.sequence,
                payload: payload.clone(),
                window_id: subscription.window_id,
            };
            state.pending.push(PendingDelivery {
                event: event.to_owned(),
                delivered: delivered.clone(),
            });
            out.push(delivered);
        }
        out
    }

    /// Drain the pending delivery queue. The shell calls this on
    /// every event-loop tick (or after a known side effect) to
    /// pull events that the bus accumulated since the last
    /// drain. Returns `(event, delivery)` pairs in the order
    /// they were produced.
    pub fn drain_pending(&self) -> Vec<(String, DeliveredEvent)> {
        let mut state = self.state.lock().expect("event bus lock poisoned");
        let pending = std::mem::take(&mut state.pending);
        pending
            .into_iter()
            .map(|entry| (entry.event, entry.delivered))
            .collect()
    }
}

fn filter_matches(filter: Option<&SubscriptionFilter>, payload: &Value) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    match filter {
        SubscriptionFilter::Path { value } => payload
            .get("path")
            .and_then(|v| v.as_str())
            .is_some_and(|p| std::path::Path::new(p).starts_with(std::path::Path::new(value))),
    }
}

/// A single delivery as it leaves the bus. The host wraps it in
/// the wire envelope (see `deliver_to_router`) and forwards it to
/// the page.
#[derive(Debug, Clone, Serialize)]
pub struct DeliveredEvent {
    pub subscription_id: String,
    pub sequence: u64,
    pub payload: Value,
    pub window_id: Option<u64>,
}

/// Envelope sent to the page. The protocol is JSON Lines over the
/// same IPC channel that carries normal API responses; the page
/// bridges this through the SDK's `events.on` helper.
#[derive(Debug, Clone, Serialize)]
pub struct EventEnvelope {
    pub protocol: u32,
    pub kind: &'static str,
    pub event: String,
    pub subscription_id: String,
    pub sequence: u64,
    pub payload: Value,
}

/// Subscribe / unsubscribe request envelopes (call side).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubscribeRequest {
    pub id: String,
    pub event: String,
    #[serde(default)]
    pub filter: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnsubscribeRequest {
    pub id: String,
    pub subscription_id: String,
}

impl EventEnvelope {
    pub fn new(event: &str, delivered: &DeliveredEvent) -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            kind: "event",
            event: event.to_owned(),
            subscription_id: delivered.subscription_id.clone(),
            sequence: delivered.sequence,
            payload: delivered.payload.clone(),
        }
    }
}

/// Translate a `subscribe` IPC request into a `Response` that hands
/// the subscription id back to the page. Used by `ApiRouter`.
pub fn handle_subscribe(bus: &EventBus, request: &Request, event_name: &str) -> Response {
    let parsed: Result<SubscribeRequest, _> = serde_json::from_value(request.params.clone());
    let parsed = match parsed {
        Ok(value) => value,
        Err(error) => {
            return Response::error(request.id.clone(), "INVALID_PARAMS", error.to_string());
        }
    };
    let filter = match parsed.filter {
        Some(value) => match serde_json::from_value::<SubscriptionFilter>(value) {
            Ok(filter) => Some(filter),
            Err(error) => {
                return Response::error(
                    request.id.clone(),
                    "INVALID_PARAMS",
                    format!("invalid filter: {error}"),
                );
            }
        },
        None => None,
    };
    match bus.subscribe(event_name, filter) {
        Ok(subscription_id) => Response::success(
            request.id.clone(),
            serde_json::json!({ "subscriptionId": subscription_id, "event": event_name }),
        ),
        Err(error) => Response::error(request.id.clone(), "SUBSCRIBE_FAILED", error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_returns_stable_id_and_unsubscribe_removes() {
        let bus = EventBus::new();
        let id = bus.subscribe("filesystem.changed", None).unwrap();
        assert!(id.starts_with("sub-"));
        assert!(bus.has_subscribers("filesystem.changed"));
        assert!(bus.unsubscribe(&id).unwrap());
        assert!(!bus.has_subscribers("filesystem.changed"));
        // Double unsubscribe is allowed and reports false.
        assert!(!bus.unsubscribe(&id).unwrap());
    }

    #[test]
    fn clear_drops_every_subscription() {
        let bus = EventBus::new();
        let a = bus.subscribe("filesystem.changed", None).unwrap();
        let b = bus.subscribe("window.boundsChanged", None).unwrap();
        bus.clear();
        assert!(!bus.has_subscribers("filesystem.changed"));
        assert!(!bus.has_subscribers("window.boundsChanged"));
        assert!(!bus.unsubscribe(&a).unwrap());
        assert!(!bus.unsubscribe(&b).unwrap());
    }

    #[test]
    fn deliver_returns_sequences_in_order() {
        let bus = EventBus::new();
        let id = bus.subscribe("filesystem.changed", None).unwrap();
        let first = bus.deliver("filesystem.changed", &serde_json::json!({"path": "a"}));
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].subscription_id, id);
        assert_eq!(first[0].sequence, 1);
        let second = bus.deliver("filesystem.changed", &serde_json::json!({"path": "b"}));
        assert_eq!(second[0].sequence, 2);
    }

    #[test]
    fn deliver_filters_by_path() {
        let bus = EventBus::new();
        bus.subscribe(
            "filesystem.changed",
            Some(SubscriptionFilter::Path {
                value: "data/".into(),
            }),
        )
        .unwrap();
        // Path does not start with `data/` — no delivery.
        let outside = bus.deliver("filesystem.changed", &serde_json::json!({"path": "other/"}));
        assert!(outside.is_empty());
        let inside = bus.deliver("filesystem.changed", &serde_json::json!({"path": "data/"}));
        assert_eq!(inside.len(), 1);
        let child = bus.deliver(
            "filesystem.changed",
            &serde_json::json!({"path": "data/child.txt"}),
        );
        assert_eq!(child.len(), 1);
    }

    #[test]
    fn empty_event_name_is_rejected() {
        let bus = EventBus::new();
        let err = bus.subscribe("", None).unwrap_err();
        assert!(matches!(err, EventBusError::InvalidPayload(_)));
    }

    #[test]
    fn drain_pending_returns_event_name_with_each_delivery() {
        let bus = EventBus::new();
        bus.subscribe("filesystem.changed", None).unwrap();
        bus.deliver("filesystem.changed", &serde_json::json!({ "path": "a" }));
        let drained = bus.drain_pending();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].0, "filesystem.changed");
        assert_eq!(drained[0].1.sequence, 1);
        // A second drain returns nothing — the queue is empty.
        assert!(bus.drain_pending().is_empty());
    }

    #[test]
    fn delivery_preserves_the_subscribing_window() {
        let bus = EventBus::new();
        bus.subscribe_for_window("menu.clicked", None, Some(7))
            .unwrap();
        bus.deliver("menu.clicked", &serde_json::json!({ "id": "open" }));
        let drained = bus.drain_pending();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].1.window_id, Some(7));
    }

    #[test]
    fn clear_drops_pending_too() {
        let bus = EventBus::new();
        bus.subscribe("filesystem.changed", None).unwrap();
        bus.deliver("filesystem.changed", &serde_json::json!({}));
        bus.clear();
        assert!(bus.drain_pending().is_empty());
    }
}

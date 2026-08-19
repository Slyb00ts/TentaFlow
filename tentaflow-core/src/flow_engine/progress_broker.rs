// =============================================================================
// Plik: flow_engine/progress_broker.rs
// Opis: ProgressBroker (§3.11 C) — per-scope tokio broadcast fan-out for
//       ephemeral run progress events. Lives in AppState; the production
//       ProgressSink publishes here and phase-3 wire handlers
//       (AgentsPayload::RunEventsSubscribe) subscribe per scope. Events are
//       NOT persisted — durable record is `run_log`.
// =============================================================================

use std::sync::Arc;
use std::sync::OnceLock;

use dashmap::DashMap;
use tokio::sync::broadcast;

use crate::flow_engine::dispatchers::{ProgressEvent, ProgressSink};

/// Process-global broker. The dashboard server builds one `AppState` per WS
/// connection but all connections must share a single broker so a run started
/// on one socket is visible to a subscriber on another. Mirrors the
/// `ui_session::global_registry` pattern.
static GLOBAL_BROKER: OnceLock<Arc<ProgressBroker>> = OnceLock::new();

/// Returns the shared broker, initialising it on first call.
pub fn global_broker() -> Arc<ProgressBroker> {
    GLOBAL_BROKER
        .get_or_init(|| Arc::new(ProgressBroker::new()))
        .clone()
}

/// Capacity of each per-scope broadcast ring. A slow subscriber that lags past
/// this many events gets `RecvError::Lagged` (the UI reconciles from RunDetail
/// on reconnect — §3.11 C), so a dropped tail never blocks the executor.
const SCOPE_CHANNEL_CAPACITY: usize = 256;

/// Per-scope broadcast registry. A scope is a session id or a run id. Senders
/// are created lazily on first publish/subscribe and dropped when the last
/// subscriber goes away AND no fresh event arrives — see `prune_if_idle`.
pub struct ProgressBroker {
    scopes: DashMap<String, broadcast::Sender<ProgressEvent>>,
    /// §3.3 ACL — server-side binding of a session-scope key to the principal
    /// that started the flow under it. A `Session` event scope is the
    /// client-minted conversation id (low entropy, time-correlated); it MUST
    /// NOT double as an authorization token. The dispatch layer records the
    /// owner here when a foreground flow starts, and `authorize_scope` rejects a
    /// `Session` subscription whose key is unbound or owned by a different
    /// principal (admin bypass). Without this, any authenticated user could
    /// guess/observe another user's session id and read their harness activity
    /// (UserQuestion / PermissionRequest / RouterDecision) — cross-principal
    /// leak (OWASP A01).
    session_owners: DashMap<String, String>,
}

impl ProgressBroker {
    pub fn new() -> Self {
        Self {
            scopes: DashMap::new(),
            session_owners: DashMap::new(),
        }
    }

    /// Binds a session-scope key to the principal that started a flow under it.
    /// Called from the foreground dispatch path with the authenticated
    /// `user_id`. Idempotent re-binding to the same owner is a no-op; a session
    /// id is owned by the first principal to claim it, so a second principal
    /// cannot hijack an in-flight session scope by re-dispatching under the same
    /// id (the binding is only cleared by `release_session_owner`).
    pub fn bind_session_owner(&self, session_id: &str, user_id: &str) {
        if session_id.is_empty() || user_id.is_empty() {
            return;
        }
        self.session_owners
            .entry(session_id.to_string())
            .or_insert_with(|| user_id.to_string());
    }

    /// Returns the principal that owns a session-scope key, if bound.
    pub fn session_owner(&self, session_id: &str) -> Option<String> {
        self.session_owners
            .get(session_id)
            .map(|o| o.value().clone())
    }

    /// Subscribe to a scope's progress stream. Creates the channel if absent so
    /// a subscriber that arrives before the first event still receives later
    /// ones. The returned receiver only sees events published AFTER it
    /// subscribed (broadcast semantics) — the dashboard reconciles backlog from
    /// `RunDetail`.
    pub fn subscribe(&self, scope: &str) -> broadcast::Receiver<ProgressEvent> {
        if let Some(tx) = self.scopes.get(scope) {
            return tx.subscribe();
        }
        let (tx, rx) = broadcast::channel(SCOPE_CHANNEL_CAPACITY);
        self.scopes.insert(scope.to_string(), tx);
        rx
    }

    /// Publish one event to a scope. No-op when no channel exists yet AND there
    /// is no subscriber — we do NOT create a channel just to publish into the
    /// void, so idle runs leave no entries. When a channel exists but has zero
    /// live receivers, `send` returns `Err` (no receivers); we drop the entry
    /// so the map does not grow unbounded across runs.
    pub fn publish(&self, scope: &str, event: ProgressEvent) {
        let Some(tx) = self.scopes.get(scope) else {
            return;
        };
        if tx.send(event).is_err() {
            // No live receivers — drop the sender lock before removing to avoid
            // a deadlock on the DashMap shard.
            drop(tx);
            self.scopes.remove(scope);
        }
    }

    /// Number of live subscribers for a scope (0 when the scope is unknown).
    /// Used by the wire layer to decide whether a run is being watched.
    pub fn subscriber_count(&self, scope: &str) -> usize {
        self.scopes
            .get(scope)
            .map(|tx| tx.receiver_count())
            .unwrap_or(0)
    }
}

impl Default for ProgressBroker {
    fn default() -> Self {
        Self::new()
    }
}

/// Production `ProgressSink` backed by a `ProgressBroker`. The executor holds it
/// as `Arc<dyn ProgressSink>`; `emit` forwards to `broker.publish`. A single
/// sink serves every run — the scope arrives per `emit` call, so the sink is
/// stateless apart from the shared broker handle.
pub struct BrokerProgressSink {
    broker: Arc<ProgressBroker>,
}

impl BrokerProgressSink {
    pub fn new(broker: Arc<ProgressBroker>) -> Self {
        Self { broker }
    }
}

impl ProgressSink for BrokerProgressSink {
    fn emit(&self, scope: &str, event: ProgressEvent) {
        self.broker.publish(scope, event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscriber_receives_published_events() {
        let broker = Arc::new(ProgressBroker::new());
        let mut rx = broker.subscribe("session-1");
        let sink = BrokerProgressSink::new(broker.clone());
        sink.emit(
            "session-1",
            ProgressEvent::NodeStarted {
                node_id: "n1".into(),
                node_type: "llm".into(),
            },
        );
        let got = rx.recv().await.expect("event delivered");
        assert_eq!(
            got,
            ProgressEvent::NodeStarted {
                node_id: "n1".into(),
                node_type: "llm".into(),
            }
        );
    }

    #[tokio::test]
    async fn publish_without_subscriber_is_noop() {
        let broker = Arc::new(ProgressBroker::new());
        let sink = BrokerProgressSink::new(broker.clone());
        // No subscriber for this scope — publish must not create an entry.
        sink.emit(
            "orphan",
            ProgressEvent::Compaction {
                node_id: "n".into(),
            },
        );
        assert_eq!(broker.subscriber_count("orphan"), 0);
    }

    #[tokio::test]
    async fn dropped_subscriber_prunes_scope_on_next_publish() {
        let broker = Arc::new(ProgressBroker::new());
        let rx = broker.subscribe("session-2");
        drop(rx);
        // First publish after the last receiver dropped removes the entry.
        broker.publish(
            "session-2",
            ProgressEvent::NodeFinished {
                node_id: "n1".into(),
                status: "ok".into(),
            },
        );
        assert_eq!(broker.subscriber_count("session-2"), 0);
    }

    #[test]
    fn session_owner_binds_first_principal_and_rejects_hijack() {
        let broker = ProgressBroker::new();
        assert_eq!(broker.session_owner("s1"), None);
        broker.bind_session_owner("s1", "user-a");
        assert_eq!(broker.session_owner("s1").as_deref(), Some("user-a"));
        // A second principal cannot steal an in-flight session scope.
        broker.bind_session_owner("s1", "user-b");
        assert_eq!(broker.session_owner("s1").as_deref(), Some("user-a"));
        // Empty inputs never create a binding.
        broker.bind_session_owner("", "user-c");
        broker.bind_session_owner("s2", "");
        assert_eq!(broker.session_owner("s2"), None);
    }

    #[tokio::test]
    async fn isolates_scopes() {
        let broker = Arc::new(ProgressBroker::new());
        let mut rx_a = broker.subscribe("a");
        let mut rx_b = broker.subscribe("b");
        broker.publish(
            "a",
            ProgressEvent::ToolCallStarted {
                call_id: "test-call".into(),
                name: "search".into(),
            },
        );
        let got = rx_a.recv().await.expect("scope a delivered");
        assert_eq!(
            got,
            ProgressEvent::ToolCallStarted {
                call_id: "test-call".into(),
                name: "search".into()
            }
        );
        // Scope b saw nothing.
        assert!(rx_b.try_recv().is_err());
    }
}

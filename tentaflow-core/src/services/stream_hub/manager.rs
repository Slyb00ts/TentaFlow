// =============================================================================
// File: services/stream_hub/manager.rs — StreamHub singleton + lifecycle logic
// =============================================================================
//
// One process-wide hub. Producers register a factory once at startup (camera
// ingest, audio capture, addon services). The hub instantiates the source the
// first time a peer subscribes, caches it together with its init segment, and
// hands the peer a fresh `broadcast::Receiver`. Subsequent subscribers reuse
// the same source — the factory is *not* called again. When the last
// subscriber drops its handle the source is removed and the producing
// pipeline can shut down.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use parking_lot::RwLock;

use super::error::StreamHubError;
use super::source::{BinaryStreamSource, StreamSourceFactory};
use super::subscription::{SubscriberToken, SubscriptionHandle};

/// Active source entry. `subscribers` is the refcount that gates teardown.
struct ActiveSource {
    source: Arc<dyn BinaryStreamSource>,
    init_segment: Option<Bytes>,
    subscribers: AtomicU64,
}

/// Process-wide stream hub.
pub struct StreamHub {
    factories: RwLock<HashMap<String, StreamSourceFactory>>,
    active: RwLock<HashMap<String, Arc<ActiveSource>>>,
    /// Per-stream async lock serializing cold-path source creation. Without it,
    /// two concurrent cold subscribes each build a source; the loser is
    /// discarded and (for camera fMP4 sources) its Drop emits DetachMp4Branch,
    /// tearing down the WINNER's mux branch — leaving the stream producing
    /// nothing and the client stuck on "connecting".
    creation_locks: parking_lot::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

static GLOBAL: OnceLock<Arc<StreamHub>> = OnceLock::new();

impl StreamHub {
    fn new() -> Self {
        Self {
            factories: RwLock::new(HashMap::new()),
            active: RwLock::new(HashMap::new()),
            creation_locks: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    /// Returns the per-stream creation lock, allocating it on first use.
    fn creation_lock(&self, stream_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.creation_locks
            .lock()
            .entry(stream_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// Returns the process-wide hub, constructing it on first access.
    pub fn global() -> &'static Arc<StreamHub> {
        GLOBAL.get_or_init(|| Arc::new(StreamHub::new()))
    }

    /// Registers a factory under `stream_id`. Re-registering the same id
    /// replaces the previous factory (idempotent; latest wins). Already-active
    /// sources are not torn down — the new factory takes effect on the next
    /// cold subscribe.
    pub fn register_factory(
        &self,
        stream_id: String,
        factory: StreamSourceFactory,
    ) -> Result<(), StreamHubError> {
        self.factories.write().insert(stream_id, factory);
        Ok(())
    }

    /// Removes a factory. Active sources remain alive for current subscribers
    /// but no new cold subscribes will succeed for this id.
    pub fn unregister_factory(&self, stream_id: &str) {
        self.factories.write().remove(stream_id);
    }

    /// Subscribes to a stream. The first subscriber pays for factory + init
    /// segment fetch; subsequent ones reuse the cached entry.
    pub async fn subscribe(
        self: &Arc<Self>,
        stream_id: &str,
    ) -> Result<SubscriptionHandle, StreamHubError> {
        // Fast path: source already active — just bump the counter.
        if let Some(entry) = self.active.read().get(stream_id).cloned() {
            // A terminally-failed source (no broadcaster) must not hand out a
            // hung receiver: drop it so the next cold subscribe rebuilds it.
            if let Some(broadcaster) = entry.source.chunk_broadcaster() {
                entry.subscribers.fetch_add(1, Ordering::AcqRel);
                let receiver = broadcaster.subscribe();
                let mime = entry.source.mime_type().to_string();
                let init = entry.init_segment.clone();
                let token = SubscriberToken::new(Arc::clone(self), stream_id.to_string());
                return Ok(SubscriptionHandle::new(
                    stream_id.to_string(),
                    mime,
                    init,
                    receiver,
                    token,
                ));
            }
            self.active.write().remove(stream_id);
        }

        // Cold path: serialize per stream so exactly ONE source is created.
        // Holding this async lock across factory()+init_segment().await means a
        // racing cold subscribe waits here and then takes the fast path below —
        // no discarded loser source, no spurious DetachMp4Branch.
        let create_lock = self.creation_lock(stream_id);
        let _create_guard = create_lock.lock().await;

        // Re-check under the creation lock — a racer may have just published it.
        if let Some(entry) = self.active.read().get(stream_id).cloned() {
            if let Some(broadcaster) = entry.source.chunk_broadcaster() {
                entry.subscribers.fetch_add(1, Ordering::AcqRel);
                let receiver = broadcaster.subscribe();
                let mime = entry.source.mime_type().to_string();
                let init = entry.init_segment.clone();
                let token = SubscriberToken::new(Arc::clone(self), stream_id.to_string());
                return Ok(SubscriptionHandle::new(
                    stream_id.to_string(),
                    mime,
                    init,
                    receiver,
                    token,
                ));
            }
            self.active.write().remove(stream_id);
        }

        let factory_result = {
            let factories = self.factories.read();
            let factory = factories
                .get(stream_id)
                .ok_or_else(|| StreamHubError::NotRegistered(stream_id.to_string()))?;
            factory()
        };
        let source = factory_result?;
        let init_segment = source.init_segment().await;

        // A source that terminally failed during creation (relay open refused,
        // owner closed before init, init timed out) reports no broadcaster. Do
        // not cache it — surface a clean failure so the subscriber resubscribes
        // instead of hanging on an empty registered stream.
        let Some(broadcaster) = source.chunk_broadcaster() else {
            return Err(StreamHubError::NotRegistered(stream_id.to_string()));
        };

        let entry = {
            let mut active = self.active.write();
            if let Some(existing) = active.get(stream_id).cloned() {
                // Lost the race; discard our freshly created source and reuse.
                existing
            } else {
                let entry = Arc::new(ActiveSource {
                    source: Arc::clone(&source),
                    init_segment: init_segment.clone(),
                    subscribers: AtomicU64::new(0),
                });
                active.insert(stream_id.to_string(), Arc::clone(&entry));
                entry
            }
        };

        // Re-fetch the broadcaster from the cached entry: if we lost the race
        // the winner's source is the one we subscribe to. A racing winner that
        // is itself terminal collapses to the same clean failure.
        let Some(broadcaster) = entry.source.chunk_broadcaster() else {
            // The (possibly race-winning) cached source is terminal — evict it so
            // the next subscribe rebuilds a fresh source instead of reusing this
            // dead zero-subscriber entry, then surface a clean failure. Guard on
            // identity so we never evict a different entry that already replaced it.
            {
                let mut active = self.active.write();
                if let Some(cur) = active.get(stream_id) {
                    if Arc::ptr_eq(cur, &entry) {
                        active.remove(stream_id);
                    }
                }
            }
            return Err(StreamHubError::NotRegistered(stream_id.to_string()));
        };
        entry.subscribers.fetch_add(1, Ordering::AcqRel);
        let receiver = broadcaster.subscribe();
        let mime = entry.source.mime_type().to_string();
        let init = entry.init_segment.clone();
        let token = SubscriberToken::new(Arc::clone(self), stream_id.to_string());
        Ok(SubscriptionHandle::new(
            stream_id.to_string(),
            mime,
            init,
            receiver,
            token,
        ))
    }

    /// Called by `SubscriberToken::drop` to release one reference. Removes the
    /// source when the counter hits zero so the producing pipeline can detach.
    pub(super) fn decrement_subscriber(&self, stream_id: &str) {
        let mut active = self.active.write();
        if let Some(entry) = active.get(stream_id) {
            let prev = entry.subscribers.fetch_sub(1, Ordering::AcqRel);
            if prev == 1 {
                active.remove(stream_id);
            }
        }
    }

    /// Test helper — current subscriber count for `stream_id`, or 0 if the
    /// source is not active.
    #[cfg(test)]
    pub(super) fn subscriber_count(&self, stream_id: &str) -> u64 {
        self.active
            .read()
            .get(stream_id)
            .map(|e| e.subscribers.load(Ordering::Acquire))
            .unwrap_or(0)
    }

    /// Test helper — whether the source is currently instantiated.
    #[cfg(test)]
    pub(super) fn is_active(&self, stream_id: &str) -> bool {
        self.active.read().contains_key(stream_id)
    }
}

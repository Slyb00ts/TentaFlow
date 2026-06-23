// =============================================================================
// File: services/stream_hub/tests.rs — round-trip + lifecycle coverage
// =============================================================================

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::broadcast;

use super::source::{BinaryStreamSource, BROADCAST_CAPACITY};
use super::{StreamHub, StreamHubError};

/// Minimal in-memory source used across all tests. The internal sender is
/// exposed via `tx_clone` so tests can drive the broadcast channel directly.
struct FakeSource {
    id: String,
    mime: String,
    init: Option<Bytes>,
    tx: broadcast::Sender<Bytes>,
}

impl FakeSource {
    fn new(id: &str, init: Option<Bytes>) -> (Arc<Self>, broadcast::Sender<Bytes>) {
        let (tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
        let producer = tx.clone();
        let src = Arc::new(Self {
            id: id.to_string(),
            mime: "application/octet-stream".to_string(),
            init,
            tx,
        });
        (src, producer)
    }
}

#[async_trait::async_trait]
impl BinaryStreamSource for FakeSource {
    fn id(&self) -> &str {
        &self.id
    }
    fn mime_type(&self) -> &str {
        &self.mime
    }
    async fn init_segment(&self) -> Option<Bytes> {
        self.init.clone()
    }
    fn chunk_broadcaster(&self) -> Option<broadcast::Sender<Bytes>> {
        Some(self.tx.clone())
    }
}

/// Each test gets a reference to the process-wide hub; isolation between
/// tests is via unique stream ids.
fn hub() -> Arc<StreamHub> {
    Arc::clone(StreamHub::global())
}

#[tokio::test]
async fn register_factory_idempotent() {
    let hub = hub();
    let id = "test:register_idempotent";

    hub.register_factory(
        id.to_string(),
        Box::new(|| Ok(FakeSource::new("a", None).0)),
    )
    .unwrap();
    hub.register_factory(
        id.to_string(),
        Box::new(|| Ok(FakeSource::new("b", None).0)),
    )
    .unwrap();

    let handle = hub.subscribe(id).await.unwrap();
    assert_eq!(handle.stream_id, id);

    drop(handle);
    hub.unregister_factory(id);
}

#[tokio::test]
async fn subscribe_calls_factory_once_then_reuses_active() {
    let hub = hub();
    let id = "test:factory_once";

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_clone = Arc::clone(&calls);
    hub.register_factory(
        id.to_string(),
        Box::new(move || {
            calls_clone.fetch_add(1, Ordering::SeqCst);
            Ok(FakeSource::new("once", Some(Bytes::from_static(b"INIT"))).0)
        }),
    )
    .unwrap();

    let h1 = hub.subscribe(id).await.unwrap();
    let h2 = hub.subscribe(id).await.unwrap();
    let h3 = hub.subscribe(id).await.unwrap();

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "factory must run only once"
    );
    assert_eq!(h1.init_segment.as_deref(), Some(&b"INIT"[..]));
    assert_eq!(h2.init_segment.as_deref(), Some(&b"INIT"[..]));
    assert_eq!(h3.init_segment.as_deref(), Some(&b"INIT"[..]));
    assert_eq!(hub.subscriber_count(id), 3);

    drop(h1);
    drop(h2);
    drop(h3);

    hub.unregister_factory(id);
}

#[tokio::test]
async fn last_unsubscribe_drops_source() {
    let hub = hub();
    let id = "test:last_drop";

    hub.register_factory(
        id.to_string(),
        Box::new(|| Ok(FakeSource::new("drop", None).0)),
    )
    .unwrap();

    let h1 = hub.subscribe(id).await.unwrap();
    let h2 = hub.subscribe(id).await.unwrap();
    assert!(hub.is_active(id));
    assert_eq!(hub.subscriber_count(id), 2);

    drop(h1);
    assert!(hub.is_active(id));
    assert_eq!(hub.subscriber_count(id), 1);

    drop(h2);
    assert!(
        !hub.is_active(id),
        "source must be removed when count hits 0"
    );
    assert_eq!(hub.subscriber_count(id), 0);

    hub.unregister_factory(id);
}

#[tokio::test]
async fn unknown_stream_returns_not_registered() {
    let hub = hub();
    let err = hub.subscribe("test:never_registered").await.unwrap_err();
    match err {
        StreamHubError::NotRegistered(ref id) => assert_eq!(id, "test:never_registered"),
        other => panic!("expected NotRegistered, got {other:?}"),
    }
}

#[tokio::test]
async fn lagged_subscriber_handles_gracefully() {
    let hub = hub();
    let id = "test:lagged";

    // The factory captures a producer-side sender clone so the test can push
    // chunks into the channel without going through the source trait.
    let (producer_tx, _producer_rx) = broadcast::channel::<Bytes>(BROADCAST_CAPACITY);

    struct HeldSource {
        id: String,
        mime: String,
        tx: broadcast::Sender<Bytes>,
    }
    #[async_trait::async_trait]
    impl BinaryStreamSource for HeldSource {
        fn id(&self) -> &str {
            &self.id
        }
        fn mime_type(&self) -> &str {
            &self.mime
        }
        async fn init_segment(&self) -> Option<Bytes> {
            None
        }
        fn chunk_broadcaster(&self) -> Option<broadcast::Sender<Bytes>> {
            Some(self.tx.clone())
        }
    }

    let tx_for_factory = producer_tx.clone();
    hub.register_factory(
        id.to_string(),
        Box::new(move || {
            Ok(Arc::new(HeldSource {
                id: "lag".to_string(),
                mime: "application/octet-stream".to_string(),
                tx: tx_for_factory.clone(),
            }))
        }),
    )
    .unwrap();

    let mut handle = hub.subscribe(id).await.unwrap();

    // Push more than the capacity without consuming so the receiver lags.
    for i in 0..(BROADCAST_CAPACITY as u32 + 8) {
        let _ = producer_tx.send(Bytes::from(vec![i as u8]));
    }

    match handle.receiver.recv().await {
        Err(broadcast::error::RecvError::Lagged(n)) => {
            assert!(n > 0, "lag count must be positive");
        }
        other => panic!("expected Lagged, got {other:?}"),
    }

    drop(handle);
    hub.unregister_factory(id);
}

/// A source that reports `None` from `chunk_broadcaster` models a relay that
/// terminally failed during creation (no init, no media). The hub must NOT
/// cache it and must surface a clean `NotRegistered` failure so the subscriber
/// resubscribes instead of hanging on an empty registered stream.
#[tokio::test]
async fn terminal_source_fails_subscribe_cleanly() {
    struct TerminalSource {
        id: String,
        mime: String,
    }
    #[async_trait::async_trait]
    impl BinaryStreamSource for TerminalSource {
        fn id(&self) -> &str {
            &self.id
        }
        fn mime_type(&self) -> &str {
            &self.mime
        }
        async fn init_segment(&self) -> Option<Bytes> {
            None
        }
        fn chunk_broadcaster(&self) -> Option<broadcast::Sender<Bytes>> {
            None
        }
    }

    let hub = hub();
    let id = "camera:terminal-relay-test";
    hub.register_factory(
        id.to_string(),
        Box::new(|| {
            Ok(Arc::new(TerminalSource {
                id: "camera:terminal-relay-test".to_string(),
                mime: "video/mp4".to_string(),
            }) as Arc<dyn BinaryStreamSource>)
        }),
    )
    .unwrap();

    match hub.subscribe(id).await {
        Err(StreamHubError::NotRegistered(got)) => assert_eq!(got, id),
        other => panic!("expected NotRegistered, got {other:?}"),
    }
    // The terminal source must not have been cached.
    assert!(!hub.is_active(id), "terminal source must not be cached");

    hub.unregister_factory(id);
}

/// A `dynamic_init()==true` source must serve each subscriber a FRESH init,
/// while a default (`false`) source reuses the value cached at creation. This
/// proves the late-joiner fix for latest-wins self-describing streams (LiDAR):
/// the first subscriber caches one init, the current init then changes, and a
/// SECOND subscriber receives the NEW init — not the stale cached one. The
/// control source with default `dynamic_init` keeps the cache-once behavior so
/// camera fMP4 is unaffected.
#[tokio::test]
async fn dynamic_init_serves_fresh_init_to_late_joiner() {
    use std::sync::Mutex;

    struct MutableInitSource {
        id: String,
        mime: String,
        tx: broadcast::Sender<Bytes>,
        current_init: Arc<Mutex<Option<Bytes>>>,
        dynamic: bool,
    }
    #[async_trait::async_trait]
    impl BinaryStreamSource for MutableInitSource {
        fn id(&self) -> &str {
            &self.id
        }
        fn mime_type(&self) -> &str {
            &self.mime
        }
        async fn init_segment(&self) -> Option<Bytes> {
            self.current_init.lock().unwrap().clone()
        }
        fn chunk_broadcaster(&self) -> Option<broadcast::Sender<Bytes>> {
            Some(self.tx.clone())
        }
        fn dynamic_init(&self) -> bool {
            self.dynamic
        }
    }

    let hub = hub();

    // Dynamic-init source: starts with NO init (cache-once would cache None).
    let dyn_init = Arc::new(Mutex::new(None::<Bytes>));
    let dyn_init_factory = Arc::clone(&dyn_init);
    let dyn_id = "lidar:dynamic-init-test";
    hub.register_factory(
        dyn_id.to_string(),
        Box::new(move || {
            let (tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
            Ok(Arc::new(MutableInitSource {
                id: "lidar:dynamic-init-test".to_string(),
                mime: "application/octet-stream".to_string(),
                tx,
                current_init: Arc::clone(&dyn_init_factory),
                dynamic: true,
            }) as Arc<dyn BinaryStreamSource>)
        }),
    )
    .unwrap();

    // First subscriber: init is None (no frame yet), source now cached.
    let h1 = hub.subscribe(dyn_id).await.unwrap();
    assert!(h1.init_segment.is_none(), "first joiner sees no frame yet");

    // The current frame changes (a publish), but publishing then PAUSES.
    *dyn_init.lock().unwrap() = Some(Bytes::from_static(b"FRAME1"));

    // Late joiner: must get the CURRENT frame via a fresh init, not the stale
    // cached None — proving dynamic_init re-fetches per subscriber.
    let h2 = hub.subscribe(dyn_id).await.unwrap();
    assert_eq!(
        h2.init_segment.as_deref(),
        Some(&b"FRAME1"[..]),
        "late joiner on a dynamic-init source gets the current frame"
    );

    drop(h1);
    drop(h2);
    hub.unregister_factory(dyn_id);

    // Control: a default (cache-once) source ignores later init changes.
    let static_init = Arc::new(Mutex::new(Some(Bytes::from_static(b"CACHED"))));
    let static_init_factory = Arc::clone(&static_init);
    let static_id = "camera:cache-once-test";
    hub.register_factory(
        static_id.to_string(),
        Box::new(move || {
            let (tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
            Ok(Arc::new(MutableInitSource {
                id: "camera:cache-once-test".to_string(),
                mime: "video/mp4".to_string(),
                tx,
                current_init: Arc::clone(&static_init_factory),
                dynamic: false,
            }) as Arc<dyn BinaryStreamSource>)
        }),
    )
    .unwrap();

    let c1 = hub.subscribe(static_id).await.unwrap();
    assert_eq!(c1.init_segment.as_deref(), Some(&b"CACHED"[..]));
    // Even if the underlying init changes, a cache-once source serves the value
    // captured at creation (camera fMP4 ftyp+moov never changes).
    *static_init.lock().unwrap() = Some(Bytes::from_static(b"CHANGED"));
    let c2 = hub.subscribe(static_id).await.unwrap();
    assert_eq!(
        c2.init_segment.as_deref(),
        Some(&b"CACHED"[..]),
        "cache-once source reuses the cached init (camera unaffected)"
    );

    drop(c1);
    drop(c2);
    hub.unregister_factory(static_id);
}

/// RAII-covers-await invariant for the fast (already-active) path: on a
/// `dynamic_init()==true` source the subscriber counter is incremented BEFORE
/// the per-subscriber `init_segment().await`, and the `SubscriberToken` is
/// constructed in the same step (before the await). If the subscribe future is
/// dropped mid-await (client disconnect / caller timeout), the token's Drop
/// still runs and returns the counter to 0 — no leaked refcount keeping the
/// source alive forever. The first call (creation init) is fast so the source
/// becomes active; every later per-subscriber fetch is slow, suspending the
/// future exactly inside the increment→await window.
#[tokio::test]
async fn dropped_subscribe_during_slow_dynamic_init_leaves_no_leak() {
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    struct SlowInitSource {
        id: String,
        mime: String,
        tx: broadcast::Sender<Bytes>,
        creation_done: AtomicBool,
    }
    #[async_trait::async_trait]
    impl BinaryStreamSource for SlowInitSource {
        fn id(&self) -> &str {
            &self.id
        }
        fn mime_type(&self) -> &str {
            &self.mime
        }
        async fn init_segment(&self) -> Option<Bytes> {
            // First call = creation init: return immediately so the source goes
            // active. Every subsequent per-subscriber fetch is slow so the test
            // can drop the future while suspended inside the await window.
            if self.creation_done.swap(true, Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
            Some(Bytes::from_static(b"INIT"))
        }
        fn chunk_broadcaster(&self) -> Option<broadcast::Sender<Bytes>> {
            Some(self.tx.clone())
        }
        fn dynamic_init(&self) -> bool {
            true
        }
    }

    let hub = hub();
    let id = "lidar:slow-dynamic-init-cancel-test";
    hub.register_factory(
        id.to_string(),
        Box::new(move || {
            let (tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
            Ok(Arc::new(SlowInitSource {
                id: "lidar:slow-dynamic-init-cancel-test".to_string(),
                mime: "application/octet-stream".to_string(),
                tx,
                creation_done: AtomicBool::new(false),
            }) as Arc<dyn BinaryStreamSource>)
        }),
    )
    .unwrap();

    // Prime: creation init is fast, so this completes and the source is active.
    let primer = hub.subscribe(id).await.unwrap();
    assert!(hub.is_active(id));
    assert_eq!(hub.subscriber_count(id), 1);

    // Second subscribe takes the fast active path: it does fetch_add (count→2),
    // builds the token, then suspends in the slow per-subscriber init. The
    // timeout drops that future mid-await; the token's Drop must decrement back.
    let timed = tokio::time::timeout(Duration::from_millis(50), hub.subscribe(id)).await;
    assert!(timed.is_err(), "slow dynamic init must not finish in time");
    assert_eq!(
        hub.subscriber_count(id),
        1,
        "cancelled subscribe mid-init must not leak a refcount"
    );

    drop(primer);
    assert_eq!(hub.subscriber_count(id), 0);
    assert!(!hub.is_active(id), "source freed once all handles drop");
    hub.unregister_factory(id);
}

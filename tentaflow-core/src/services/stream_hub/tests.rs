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

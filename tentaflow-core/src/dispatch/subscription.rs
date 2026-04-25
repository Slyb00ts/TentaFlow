// =============================================================================
// Plik: dispatch/subscription.rs
// Opis: Subscription/streaming framework dla MessageBody wariantow ze streamem
//       (R-STREAM archetyp). Per-subscription bounded mpsc, server-issued
//       resume_token (HMAC) dla reconnect drain, three-tier bucket aggregation
//       (1s/10s/60s) dla wysokoczestotliwych eventow (np. dashboard metrics).
//
//       Architektura (bootstrap):
//         - SubscriptionRegistry: globalny, per-correlation_id mpsc handle
//         - SubscriptionHandle: tx side dla handlerow do wysylania chunkow
//         - Receiver side trzymany przez ws_binary writer task — drain do WSS
//
//       Resume token (NIE bootstrap): generowany przez serwer przy stream end
//       jesli klient straci polaczenie; nowa sesja moze zlozyc subscribe-resume
//       z token + last_seq, serwer waliduje HMAC i wysyla brakujace chunki z
//       SQLite buffer (recorder reuse). To jest #36 phase 2 + #34.
// =============================================================================

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use tentaflow_protocol::MessageBody;
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Domyslna pojemnosc bounded mpsc per subscription. 256 chunki — odpowiada
/// ~18s przy 14 tok/s (small LLM) i ~280ms przy 900 tok/s (hi-end GPU).
/// Wystarczajaco aby WS batch writer (recv_many, 16 max) nigdy nie blokowal
/// upstream stream handler na push_chunk_async. Backpressure przez `send().await`.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 256;

// =============================================================================
// Globalny registry
// =============================================================================

static REGISTRY: OnceLock<Arc<SubscriptionRegistry>> = OnceLock::new();

/// Zwraca globalny SubscriptionRegistry. Lazy init przy pierwszym wywolaniu.
pub fn global() -> &'static Arc<SubscriptionRegistry> {
    REGISTRY.get_or_init(|| Arc::new(SubscriptionRegistry::new()))
}

// =============================================================================
// Bucket tier (3 stale poziomy agregacji)
// =============================================================================

/// Stale poziomy agregacji dla wysokoczestotliwych eventow. Klient subskrybujacy
/// metryki wybiera bucket tier (np. dashboard 1s, retention 10s, archive 60s).
/// Server agreguje chunki w pamieci i flushuje na rytmie tickera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BucketTier {
    /// 1 sekunda — real-time dashboard.
    OneSecond,
    /// 10 sekund — retention view (wykres ostatnich 10 min).
    TenSeconds,
    /// 60 sekund — long-term archive (wykres ostatnich 24h).
    SixtySeconds,
}

impl BucketTier {
    pub fn duration_ms(self) -> u64 {
        match self {
            BucketTier::OneSecond => 1_000,
            BucketTier::TenSeconds => 10_000,
            BucketTier::SixtySeconds => 60_000,
        }
    }
}

// =============================================================================
// Subscription
// =============================================================================

/// Pojedyncza aktywna subscription. Tworzona gdy handler zwraca
/// IS_STREAM_CHUNK frames; usuwana po IS_STREAM_END lub MetaCancelStream.
pub struct Subscription {
    /// Correlation_id z klient request — ten sam ID dla wszystkich chunkow.
    pub correlation_id: u64,
    /// Bucket aggregation tier (None = passthrough, kazdy chunk od razu).
    pub bucket: Option<BucketTier>,
    /// Sender — handler uzywa do push chunkow.
    pub tx: mpsc::Sender<SubscriptionEvent>,
    /// Liczba chunkow wyslanych (do resume_token).
    pub chunks_sent: u64,
}

/// Zdarzenie wysylane przez handler do mpsc kanalu.
#[derive(Debug)]
pub enum SubscriptionEvent {
    /// Kolejny chunk streama.
    Chunk(MessageBody),
    /// Koniec streama z opcjonalnym final body (np. ChatStreamEnd usage).
    End(Option<MessageBody>),
    /// Blad — wyemitowane jako MessageBody::Error + IS_STREAM_END.
    Error(tentaflow_protocol::ProtocolError),
}

// =============================================================================
// SubscriptionRegistry
// =============================================================================

pub struct SubscriptionRegistry {
    /// correlation_id → Subscription handle.
    subs: RwLock<HashMap<u64, Arc<Subscription>>>,
}

impl SubscriptionRegistry {
    pub fn new() -> Self {
        Self {
            subs: RwLock::new(HashMap::new()),
        }
    }

    /// Tworzy nowa subscription. Zwraca (handle dla handlera, receiver dla writer task'a).
    pub fn create(
        &self,
        correlation_id: u64,
        bucket: Option<BucketTier>,
    ) -> (Arc<Subscription>, mpsc::Receiver<SubscriptionEvent>) {
        let (tx, rx) = mpsc::channel(DEFAULT_CHANNEL_CAPACITY);
        let sub = Arc::new(Subscription {
            correlation_id,
            bucket,
            tx,
            chunks_sent: 0,
        });
        let mut guard = self.subs.write().unwrap();
        if let Some(_old) = guard.insert(correlation_id, Arc::clone(&sub)) {
            warn!(
                correlation_id,
                "subscription_registry: nadpisano istniejaca subscription"
            );
        }
        debug!(correlation_id, "subscription created");
        (sub, rx)
    }

    /// Pobiera subscription handle (dla MetaCancelStream / status query).
    pub fn get(&self, correlation_id: u64) -> Option<Arc<Subscription>> {
        self.subs.read().unwrap().get(&correlation_id).cloned()
    }

    /// Anuluje subscription (MetaCancelStream lub disconnect).
    /// Zwraca true jesli usunieto, false jesli nie istnialo.
    pub fn cancel(&self, correlation_id: u64) -> bool {
        let removed = self.subs.write().unwrap().remove(&correlation_id);
        if let Some(sub) = removed {
            // Try-send error — jesli kanal zapelniony to writer juz odpina sie.
            let _ = sub.tx.try_send(SubscriptionEvent::Error(
                tentaflow_protocol::ProtocolError {
                    code: tentaflow_protocol::ProtocolErrorCode::StreamCancelled,
                    message: "subscription cancelled".to_string(),
                    trace_id: None,
                },
            ));
            debug!(correlation_id, "subscription cancelled");
            true
        } else {
            false
        }
    }

    /// Liczba aktywnych subscriptions (observability).
    pub fn count(&self) -> usize {
        self.subs.read().unwrap().len()
    }

    /// Wszystkie aktywne correlation_ids (dla admin UI).
    pub fn active_correlation_ids(&self) -> Vec<u64> {
        self.subs.read().unwrap().keys().copied().collect()
    }
}

impl Default for SubscriptionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Helpery dla handlera
// =============================================================================

/// Wysyla chunk do subscription. Zwraca Err gdy kanal zapelniony (backpressure).
pub fn push_chunk(sub: &Subscription, body: MessageBody) -> Result<(), String> {
    sub.tx
        .try_send(SubscriptionEvent::Chunk(body))
        .map_err(|e| format!("backpressure: {}", e))
}

/// Async wersja `push_chunk` z prawdziwym backpressure. Czeka na slot w kanale
/// gdy pelno (nie gubi chunkow). Zwraca Err tylko gdy receiver odpadl — wtedy
/// caller powinien zakonczyc task. Uzywane przez handlery ktore emituja duzo
/// chunkow (np. chat LLM streaming z maly modelu szybko generujaca tokeny).
pub async fn push_chunk_async(sub: &Subscription, body: MessageBody) -> Result<(), String> {
    sub.tx
        .send(SubscriptionEvent::Chunk(body))
        .await
        .map_err(|e| format!("receiver closed: {}", e))
}

/// Wysyla koncowy frame. Po tym subscription powinno byc cancel'owane przez writer.
pub fn push_end(sub: &Subscription, final_body: Option<MessageBody>) -> Result<(), String> {
    sub.tx
        .try_send(SubscriptionEvent::End(final_body))
        .map_err(|e| format!("end send: {}", e))
}

/// Async wersja `push_end` — czeka na slot gdy kanal pelny. Gwarantuje ze frame
/// End dotrze do writera nawet przy wysokiej intensywnosci chunkow tuz przed.
pub async fn push_end_async(
    sub: &Subscription,
    final_body: Option<MessageBody>,
) -> Result<(), String> {
    sub.tx
        .send(SubscriptionEvent::End(final_body))
        .await
        .map_err(|e| format!("end send: {}", e))
}

// =============================================================================
// Streaming handler registry (parallel do HandlerMeta dla sync handlerow)
// =============================================================================

/// Wskaznik do funkcji streaming handlera. Spawnowany jako task — handler
/// pisze chunki przez `Subscription::tx` (mpsc), writer task drainuje rx i
/// emituje IS_STREAM_CHUNK / IS_STREAM_END frames przez WSS.
pub type StreamHandlerFn = fn(MessageBody, super::HandlerContext, Arc<Subscription>);

/// Metadata streaming handlera. Rejestrowane oddzielnie od HandlerMeta zeby
/// ws_binary mogl rozroznic sync (zwraca jedna odpowiedz) vs streaming
/// (spawnuje writer task z mpsc).
pub struct StreamHandlerMeta {
    pub variant_name: &'static str,
    pub required_auth: super::SessionAuthKind,
    pub handler_fn: StreamHandlerFn,
}

inventory::collect!(StreamHandlerMeta);

static STREAM_REGISTRY: OnceLock<HashMap<&'static str, &'static StreamHandlerMeta>> =
    OnceLock::new();

fn stream_registry() -> &'static HashMap<&'static str, &'static StreamHandlerMeta> {
    STREAM_REGISTRY.get_or_init(|| {
        inventory::iter::<StreamHandlerMeta>()
            .map(|h| (h.variant_name, h))
            .collect()
    })
}

/// Wyszukuje streaming handler po nazwie wariantu.
pub fn find_stream_handler(variant_name: &str) -> Option<&'static StreamHandlerMeta> {
    stream_registry().get(variant_name).copied()
}

/// Liczba zarejestrowanych streaming handlerow (debug/observability).
pub fn stream_handler_count() -> usize {
    stream_registry().len()
}

// =============================================================================
// Testy
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_and_cancel_subscription() {
        let reg = SubscriptionRegistry::new();
        let (sub, _rx) = reg.create(42, None);
        assert_eq!(sub.correlation_id, 42);
        assert_eq!(reg.count(), 1);
        assert!(reg.cancel(42));
        assert_eq!(reg.count(), 0);
        assert!(!reg.cancel(42)); // juz nie istnieje
    }

    #[tokio::test]
    async fn push_chunk_and_end_flow() {
        let reg = SubscriptionRegistry::new();
        let (sub, mut rx) = reg.create(1, None);

        push_chunk(&sub, MessageBody::MetaHeartbeat { sent_at_epoch: 1 }).unwrap();
        push_chunk(&sub, MessageBody::MetaHeartbeat { sent_at_epoch: 2 }).unwrap();
        push_end(&sub, None).unwrap();

        let event1 = rx.recv().await.unwrap();
        match event1 {
            SubscriptionEvent::Chunk(MessageBody::MetaHeartbeat { sent_at_epoch }) => {
                assert_eq!(sent_at_epoch, 1)
            }
            other => panic!("expected Chunk, got {:?}", other),
        }
        let event2 = rx.recv().await.unwrap();
        assert!(matches!(event2, SubscriptionEvent::Chunk(_)));
        let event3 = rx.recv().await.unwrap();
        assert!(matches!(event3, SubscriptionEvent::End(None)));
    }

    #[tokio::test]
    async fn multiple_subscriptions_independent() {
        let reg = SubscriptionRegistry::new();
        let (sub1, mut rx1) = reg.create(100, Some(BucketTier::OneSecond));
        let (sub2, mut rx2) = reg.create(200, Some(BucketTier::TenSeconds));
        assert_eq!(reg.count(), 2);

        push_chunk(&sub1, MessageBody::ModelListRequest).unwrap();
        push_chunk(&sub2, MessageBody::ModelListRequest).unwrap();

        let e1 = rx1.recv().await.unwrap();
        let e2 = rx2.recv().await.unwrap();
        assert!(matches!(
            e1,
            SubscriptionEvent::Chunk(MessageBody::ModelListRequest)
        ));
        assert!(matches!(
            e2,
            SubscriptionEvent::Chunk(MessageBody::ModelListRequest)
        ));
    }

    #[tokio::test]
    async fn cancel_emits_error_event() {
        let reg = SubscriptionRegistry::new();
        let (_sub, mut rx) = reg.create(7, None);
        assert!(reg.cancel(7));

        let event = rx.recv().await.unwrap();
        match event {
            SubscriptionEvent::Error(e) => assert_eq!(
                e.code,
                tentaflow_protocol::ProtocolErrorCode::StreamCancelled
            ),
            other => panic!("expected Error, got {:?}", other),
        }
    }

    #[test]
    fn bucket_tier_durations() {
        assert_eq!(BucketTier::OneSecond.duration_ms(), 1_000);
        assert_eq!(BucketTier::TenSeconds.duration_ms(), 10_000);
        assert_eq!(BucketTier::SixtySeconds.duration_ms(), 60_000);
    }

    #[tokio::test]
    async fn backpressure_full_channel_returns_err() {
        let reg = SubscriptionRegistry::new();
        let (sub, _rx) = reg.create(1, None);
        // Wypelnij kanal (capacity = 64) — _rx nie czyta wiec try_send blokuje.
        let mut accepted = 0;
        for i in 0..(DEFAULT_CHANNEL_CAPACITY + 5) {
            if push_chunk(
                &sub,
                MessageBody::MetaHeartbeat {
                    sent_at_epoch: i as u64,
                },
            )
            .is_ok()
            {
                accepted += 1;
            } else {
                break;
            }
        }
        assert_eq!(accepted, DEFAULT_CHANNEL_CAPACITY);
    }

    #[tokio::test]
    async fn active_correlation_ids_returns_all() {
        let reg = SubscriptionRegistry::new();
        let (_s1, _r1) = reg.create(11, None);
        let (_s2, _r2) = reg.create(22, None);
        let (_s3, _r3) = reg.create(33, None);
        let mut ids = reg.active_correlation_ids();
        ids.sort();
        assert_eq!(ids, vec![11, 22, 33]);
    }
}

// =============================================================================
// Plik: addon/event_bus.rs
// Opis: EventBus — system pub/sub eventow dla addonow. Obsluguje subskrypcje
//       per typ eventu, dostarczanie do zasubskrybowanych addonow z kontrola
//       uprawnien, ring buffer 4096 eventow w pamieci.
// =============================================================================

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, warn};

// =============================================================================
// Event — typ eventu w systemie
// =============================================================================

/// Event systemowy lub addonowy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Typ eventu (np. "message_received", "addon.started", "model.loaded")
    pub event_type: String,
    /// Addon zrodlowy (None = event systemowy)
    pub source_addon: Option<String>,
    /// Uzytkownik zrodlowy (None = system)
    pub source_user: Option<i64>,
    /// Dane eventu
    pub payload: serde_json::Value,
    /// Znacznik czasu
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

// =============================================================================
// EventSubscriber — subskrybent eventu
// =============================================================================

/// Subskrybent eventu — addon + instancja + callback
#[derive(Debug, Clone)]
pub struct EventSubscriber {
    /// ID addonu subskrybujacego
    pub addon_id: String,
    /// ID instancji addonu
    pub instance_id: String,
    /// Nazwa guest export do wywolania (domyslnie "on_event")
    pub callback_name: String,
}

// =============================================================================
// EventBus — centralny bus eventow
// =============================================================================

/// Rozmiar ring buffer eventow w pamieci
const EVENT_RING_BUFFER_SIZE: usize = 4096;

/// EventBus — publish/subscribe in-process z ring buffer
pub struct EventBus {
    /// Mapa: event_type -> lista subskrybentow. ArcSwap — readers lock-free,
    /// subscribe/unsubscribe kopiuja mape (COW) i atomowo podmieniaja.
    subscribers: ArcSwap<HashMap<String, Vec<EventSubscriber>>>,
    /// Ring buffer ostatnich eventow (do debugowania i replay)
    event_history: RwLock<RingBuffer<Event>>,
    /// Globalny licznik subskrypcji (do generowania ID)
    subscription_counter: AtomicU64,
    /// Licznik opublikowanych eventow
    published_count: AtomicU64,
    /// Licznik dostarczonych eventow
    delivered_count: AtomicU64,
    /// Sender kanalu dispatchera — kazdy `publish()` wpada na ten kanal,
    /// `AddonManager` drenuje go w dedykowanym blocking watku i woluje
    /// `handle_event`. RwLock<Option> (nie OnceLock!) bo shutdown musi
    /// mochic dropowac sender — inaczej blocking_recv wisi wiecznie i
    /// proces nie konczy sie po SIGINT (cykl referencyjny przez
    /// Arc<AddonManager> trzymany w spawn_blocking task).
    dispatch_tx: RwLock<Option<UnboundedSender<Event>>>,
}

impl EventBus {
    /// Tworzy nowy EventBus
    pub fn new() -> Self {
        Self {
            subscribers: ArcSwap::from_pointee(HashMap::with_capacity(64)),
            event_history: RwLock::new(RingBuffer::new(EVENT_RING_BUFFER_SIZE)),
            subscription_counter: AtomicU64::new(1),
            published_count: AtomicU64::new(0),
            delivered_count: AtomicU64::new(0),
            dispatch_tx: RwLock::new(None),
        }
    }

    /// Podpina sender kanalu dispatchera — moze byc wywolany tylko raz przez
    /// `AddonManager` przy inicjalizacji. Kolejne wywolania sa ignorowane.
    pub fn set_dispatch_sender(&self, tx: UnboundedSender<Event>) {
        let mut slot = self.dispatch_tx.write();
        if slot.is_some() {
            warn!("EventBus: dispatcher sender juz ustawiony, ignoruje");
            return;
        }
        *slot = Some(tx);
    }

    /// Zamyka kanal dispatchera — dropuje sender, dispatcher loop dostaje
    /// `None` z `blocking_recv` i wychodzi. Wolane z `AddonManager::shutdown`
    /// zeby graceful shutdown faktycznie sie zakonczyl (bez tego blocking
    /// thread wisi wiecznie przez cykl referencyjny Arc<AddonManager>).
    pub fn close_dispatcher(&self) {
        *self.dispatch_tx.write() = None;
    }

    /// Bumpuje licznik dostarczonych eventow (wolane przez dispatcher po handle_event).
    pub fn record_delivery(&self, count: u64) {
        self.delivered_count.fetch_add(count, Ordering::Relaxed);
    }

    /// Subskrybuje typ eventu. Zwraca subscription_id.
    pub fn subscribe(&self, event_type: &str, subscriber: EventSubscriber) -> u64 {
        let subscription_id = self.subscription_counter.fetch_add(1, Ordering::Relaxed);

        let mut new_subs = (**self.subscribers.load()).clone();
        new_subs
            .entry(event_type.to_string())
            .or_insert_with(|| Vec::with_capacity(4))
            .push(subscriber.clone());
        self.subscribers.store(Arc::new(new_subs));

        debug!(
            "EventBus: addon '{}' subskrybuje '{}' (subscription_id={})",
            subscriber.addon_id, event_type, subscription_id
        );

        subscription_id
    }

    /// Publikuje event — dodaje do ring buffer i wysyla na kanal dispatchera.
    /// Dispatcher (w `AddonManager::start_event_dispatcher`) drenuje kanal i
    /// dostarcza event do subskrybentow przez `handle_event`. Jesli dispatcher
    /// nie jest jeszcze podpiety (np. testy event_bus w izolacji), event ladu-
    /// je tylko w ring bufferze.
    pub fn publish(&self, event: Event) {
        self.published_count.fetch_add(1, Ordering::Relaxed);

        debug!(
            "EventBus: event '{}' od {:?}",
            event.event_type, event.source_addon
        );

        // Wyslij na kanal dispatchera (jesli skonfigurowany).
        // Klon eventu zostaje pozniej dolozony do ring buffera dla historii.
        // Drop guard `tx` szybko — nie trzymamy locka podczas send.
        let tx = self.dispatch_tx.read().clone();
        if let Some(tx) = tx {
            if let Err(e) = tx.send(event.clone()) {
                warn!(
                    "EventBus: dispatcher kanal zamkniety, event '{}' upuszczony: {}",
                    event.event_type, e
                );
            }
        }

        self.event_history.write().push(event);
    }

    /// Pobiera liste subskrybentow dla danego typu eventu
    pub fn get_subscribers(&self, event_type: &str) -> Vec<EventSubscriber> {
        let subs = self.subscribers.load();

        let mut result = Vec::new();

        // Subskrybenci dokladnego typu
        if let Some(exact) = subs.get(event_type) {
            result.extend(exact.iter().cloned());
        }

        // Subskrybenci wildcard ("*")
        if let Some(wildcard) = subs.get("*") {
            result.extend(wildcard.iter().cloned());
        }

        // Subskrybenci z prefix pattern (np. "addon.*" matchuje "addon.started")
        for (pattern, subscribers) in subs.iter() {
            if pattern.ends_with('*') && pattern != "*" {
                let prefix = &pattern[..pattern.len() - 1];
                if event_type.starts_with(prefix) {
                    result.extend(subscribers.iter().cloned());
                }
            }
        }

        result
    }

    /// Odsubskrybowuje addon z danego typu eventu
    pub fn unsubscribe(&self, addon_id: &str, event_type: &str) {
        let mut new_subs = (**self.subscribers.load()).clone();
        if let Some(subscribers) = new_subs.get_mut(event_type) {
            subscribers.retain(|s| s.addon_id != addon_id);
            if subscribers.is_empty() {
                new_subs.remove(event_type);
            }
        }
        self.subscribers.store(Arc::new(new_subs));
    }

    /// Odsubskrybowuje addon ze wszystkich typow eventow
    pub fn unsubscribe_all(&self, addon_id: &str) {
        let mut new_subs = (**self.subscribers.load()).clone();
        new_subs.values_mut().for_each(|subscribers| {
            subscribers.retain(|s| s.addon_id != addon_id);
        });
        new_subs.retain(|_, v| !v.is_empty());
        self.subscribers.store(Arc::new(new_subs));

        debug!(
            "EventBus: addon '{}' odsubskrybowany ze wszystkich eventow",
            addon_id
        );
    }

    /// Odsubskrybowuje konkretna instancje
    pub fn unsubscribe_instance(&self, instance_id: &str) {
        let mut new_subs = (**self.subscribers.load()).clone();
        new_subs.values_mut().for_each(|subscribers| {
            subscribers.retain(|s| s.instance_id != instance_id);
        });
        new_subs.retain(|_, v| !v.is_empty());
        self.subscribers.store(Arc::new(new_subs));
    }

    /// Pobiera ostatnie N eventow z ring buffer
    pub fn recent_events(&self, count: usize) -> Vec<Event> {
        self.event_history.read().recent(count)
    }

    /// Zwraca statystyki
    pub fn stats(&self) -> EventBusStats {
        let subs = self.subscribers.load();
        let total_subscribers: usize = subs.values().map(|v| v.len()).sum();
        let event_types = subs.len();

        EventBusStats {
            total_subscribers,
            event_types,
            published_count: self.published_count.load(Ordering::Relaxed),
            delivered_count: self.delivered_count.load(Ordering::Relaxed),
            history_size: self.event_history.read().len(),
        }
    }
}

/// Statystyki EventBus
#[derive(Debug, Clone)]
pub struct EventBusStats {
    pub total_subscribers: usize,
    pub event_types: usize,
    pub published_count: u64,
    pub delivered_count: u64,
    pub history_size: usize,
}

// =============================================================================
// RingBuffer — cykliczny bufor eventow
// =============================================================================

/// Cykliczny bufor o stalym rozmiarze — starsze elementy wypadaja
struct RingBuffer<T> {
    buffer: Vec<Option<T>>,
    head: usize,
    count: usize,
    capacity: usize,
}

impl<T: Clone> RingBuffer<T> {
    /// Tworzy nowy ring buffer o podanej pojemnosci
    fn new(capacity: usize) -> Self {
        let mut buffer = Vec::with_capacity(capacity);
        buffer.resize_with(capacity, || None);
        Self {
            buffer,
            head: 0,
            count: 0,
            capacity,
        }
    }

    /// Dodaje element do bufora (nadpisuje najstarszy jesli pelny)
    fn push(&mut self, item: T) {
        self.buffer[self.head] = Some(item);
        self.head = (self.head + 1) % self.capacity;
        if self.count < self.capacity {
            self.count += 1;
        }
    }

    /// Pobiera ostatnie N elementow (od najnowszego)
    fn recent(&self, count: usize) -> Vec<T> {
        let count = count.min(self.count);
        let mut result = Vec::with_capacity(count);

        for i in 0..count {
            let idx = if self.head >= i + 1 {
                self.head - i - 1
            } else {
                self.capacity - (i + 1 - self.head)
            };

            if let Some(ref item) = self.buffer[idx] {
                result.push(item.clone());
            }
        }

        result
    }

    /// Zwraca ilosc elementow w buforze
    fn len(&self) -> usize {
        self.count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ring_buffer_push_and_recent() {
        let mut buf = RingBuffer::new(4);
        buf.push(1);
        buf.push(2);
        buf.push(3);

        let recent = buf.recent(2);
        assert_eq!(recent, vec![3, 2]);
    }

    #[test]
    fn test_ring_buffer_overflow() {
        let mut buf = RingBuffer::new(3);
        buf.push(1);
        buf.push(2);
        buf.push(3);
        buf.push(4); // Nadpisuje 1

        assert_eq!(buf.len(), 3);
        let recent = buf.recent(3);
        assert_eq!(recent, vec![4, 3, 2]);
    }

    #[test]
    fn test_event_bus_subscribe_and_get() {
        let bus = EventBus::new();

        let sub = EventSubscriber {
            addon_id: "test.addon".to_string(),
            instance_id: "inst-1".to_string(),
            callback_name: "on_event".to_string(),
        };

        bus.subscribe("message_received", sub.clone());

        let subs = bus.get_subscribers("message_received");
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].addon_id, "test.addon");

        // Brak subskrybentow dla innego typu
        let subs2 = bus.get_subscribers("model_loaded");
        assert_eq!(subs2.len(), 0);
    }

    #[test]
    fn test_event_bus_wildcard() {
        let bus = EventBus::new();

        let sub = EventSubscriber {
            addon_id: "test.addon".to_string(),
            instance_id: "inst-1".to_string(),
            callback_name: "on_event".to_string(),
        };

        bus.subscribe("*", sub);

        let subs = bus.get_subscribers("anything");
        assert_eq!(subs.len(), 1);
    }

    #[test]
    fn test_event_bus_unsubscribe() {
        let bus = EventBus::new();

        let sub = EventSubscriber {
            addon_id: "test.addon".to_string(),
            instance_id: "inst-1".to_string(),
            callback_name: "on_event".to_string(),
        };

        bus.subscribe("test_event", sub);
        assert_eq!(bus.get_subscribers("test_event").len(), 1);

        bus.unsubscribe("test.addon", "test_event");
        assert_eq!(bus.get_subscribers("test_event").len(), 0);
    }

    #[test]
    fn test_event_bus_meeting_transcript_flow() {
        // Pelny przepyw: subskrypcja meeting.transcript, publikacja, odbiór

        // Arrange
        let bus = EventBus::new();

        let sub = EventSubscriber {
            addon_id: "meeting-recorder".to_string(),
            instance_id: "recorder-inst-1".to_string(),
            callback_name: "on_transcript".to_string(),
        };

        bus.subscribe("meeting.transcript", sub);

        // Act — publikacja eventu transkrypcji
        let now = chrono::Utc::now();
        bus.publish(Event {
            event_type: "meeting.transcript".to_string(),
            source_addon: None,
            source_user: None,
            payload: serde_json::json!({
                "speaker": "Jan Kowalski",
                "text": "Dzien dobry wszystkim",
                "timestamp_ms": 1_710_000_000_000u64,
            }),
            timestamp: now,
        });

        // Assert — subskrybent jest na liscie
        let subs = bus.get_subscribers("meeting.transcript");
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].addon_id, "meeting-recorder");
        assert_eq!(subs[0].callback_name, "on_transcript");

        // Assert — event w historii z poprawnymi danymi
        let events = bus.recent_events(1);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "meeting.transcript");
        assert_eq!(events[0].payload["speaker"], "Jan Kowalski");
        assert_eq!(events[0].payload["text"], "Dzien dobry wszystkim");
        assert_eq!(events[0].payload["timestamp_ms"], 1_710_000_000_000u64);

        // Assert — statystyki
        let stats = bus.stats();
        assert_eq!(stats.total_subscribers, 1);
        assert_eq!(stats.published_count, 1);
        assert_eq!(stats.history_size, 1);
    }

    #[test]
    fn test_event_bus_meeting_transcript_multiple_speakers() {
        // Wielu mowcow — kazdy event trafia do historii w kolejnosci

        // Arrange
        let bus = EventBus::new();

        let sub = EventSubscriber {
            addon_id: "summary-addon".to_string(),
            instance_id: "summary-1".to_string(),
            callback_name: "on_event".to_string(),
        };

        bus.subscribe("meeting.transcript", sub);

        // Act — 3 rozne wypowiedzi
        let speakers = [
            ("Jan", "Zaczynamy spotkanie"),
            ("Anna", "Mam aktualizacje projektu"),
            ("Piotr", "Jakie sa priorytety?"),
        ];

        let now = chrono::Utc::now();
        for (speaker, text) in &speakers {
            bus.publish(Event {
                event_type: "meeting.transcript".to_string(),
                source_addon: None,
                source_user: None,
                payload: serde_json::json!({
                    "speaker": speaker,
                    "text": text,
                }),
                timestamp: now,
            });
        }

        // Assert — 3 eventy w historii, najnowszy pierwszy
        let events = bus.recent_events(3);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].payload["speaker"], "Piotr");
        assert_eq!(events[1].payload["speaker"], "Anna");
        assert_eq!(events[2].payload["speaker"], "Jan");

        assert_eq!(bus.stats().published_count, 3);
    }

    #[test]
    fn test_event_bus_prefix_pattern_meeting_events() {
        // Subskrypcja z prefix pattern "meeting.*" matchuje "meeting.transcript"

        // Arrange
        let bus = EventBus::new();

        let sub = EventSubscriber {
            addon_id: "meeting-monitor".to_string(),
            instance_id: "monitor-1".to_string(),
            callback_name: "on_event".to_string(),
        };

        bus.subscribe("meeting.*", sub);

        // Act & Assert
        let subs = bus.get_subscribers("meeting.transcript");
        assert_eq!(subs.len(), 1);

        let subs = bus.get_subscribers("meeting.control");
        assert_eq!(subs.len(), 1);

        // Nie matchuje innego prefixu
        let subs = bus.get_subscribers("addon.started");
        assert_eq!(subs.len(), 0);
    }

    #[tokio::test]
    async fn publish_pushes_event_to_dispatcher_channel() {
        // Po `set_dispatch_sender` kazdy publish musi pojawic sie na kanale —
        // to gwarancja ze AddonManager dispatcher faktycznie dostaje eventy.
        let bus = EventBus::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        bus.set_dispatch_sender(tx);

        bus.publish(Event {
            event_type: "test.evt".to_string(),
            source_addon: Some("addon-a".to_string()),
            source_user: None,
            payload: serde_json::json!({"v": 42}),
            timestamp: chrono::Utc::now(),
        });

        let delivered = rx.recv().await.expect("event powinien dotrzec na kanal");
        assert_eq!(delivered.event_type, "test.evt");
        assert_eq!(delivered.payload["v"], 42);
        assert_eq!(delivered.source_addon.as_deref(), Some("addon-a"));

        // Event powinien byc tez w ring bufferze.
        let recent = bus.recent_events(1);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].event_type, "test.evt");
    }

    #[test]
    fn publish_without_dispatcher_only_lands_in_ring_buffer() {
        // Brak podpiętego sendera nie moze wywalic publish — event powinien
        // wyladowac w ring bufferze, a licznik published byc zwiekszony.
        let bus = EventBus::new();
        bus.publish(Event {
            event_type: "test.evt".to_string(),
            source_addon: None,
            source_user: None,
            payload: serde_json::Value::Null,
            timestamp: chrono::Utc::now(),
        });

        assert_eq!(bus.stats().published_count, 1);
        assert_eq!(bus.recent_events(1).len(), 1);
    }

    #[test]
    fn second_set_dispatch_sender_is_ignored() {
        let bus = EventBus::new();
        let (tx1, _rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
        bus.set_dispatch_sender(tx1);
        bus.set_dispatch_sender(tx2);

        bus.publish(Event {
            event_type: "test.evt".to_string(),
            source_addon: None,
            source_user: None,
            payload: serde_json::Value::Null,
            timestamp: chrono::Utc::now(),
        });

        // Drugi sender pozostal odpiety — recv musi zwrocic Empty,
        // bo event poszedl do pierwszego (porzuconego) kanalu.
        assert!(rx2.try_recv().is_err());
    }

    #[test]
    fn test_event_bus_unsubscribe_all_removes_meeting_subscriptions() {
        // Odsubskrybowanie addonu usuwa go ze wszystkich eventow meeting.*

        // Arrange
        let bus = EventBus::new();

        let sub = EventSubscriber {
            addon_id: "teams-bot".to_string(),
            instance_id: "teams-1".to_string(),
            callback_name: "on_event".to_string(),
        };

        bus.subscribe("meeting.transcript", sub.clone());
        bus.subscribe("meeting.control", sub);
        assert_eq!(bus.get_subscribers("meeting.transcript").len(), 1);
        assert_eq!(bus.get_subscribers("meeting.control").len(), 1);

        // Act
        bus.unsubscribe_all("teams-bot");

        // Assert
        assert_eq!(bus.get_subscribers("meeting.transcript").len(), 0);
        assert_eq!(bus.get_subscribers("meeting.control").len(), 0);
    }
}

// =============================================================================
// Plik: addon/event_bus.rs
// Opis: EventBus — system pub/sub eventow dla addonow. Obsluguje subskrypcje
//       per typ eventu, dostarczanie do zasubskrybowanych addonow z kontrola
//       uprawnien, ring buffer 4096 eventow w pamieci.
// =============================================================================

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::RwLock;
use serde::{Serialize, Deserialize};
use tracing::debug;

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
    /// Mapa: event_type -> lista subskrybentow
    subscribers: RwLock<HashMap<String, Vec<EventSubscriber>>>,
    /// Ring buffer ostatnich eventow (do debugowania i replay)
    event_history: RwLock<RingBuffer<Event>>,
    /// Globalny licznik subskrypcji (do generowania ID)
    subscription_counter: AtomicU64,
    /// Licznik opublikowanych eventow
    published_count: AtomicU64,
    /// Licznik dostarczonych eventow
    delivered_count: AtomicU64,
}

impl EventBus {
    /// Tworzy nowy EventBus
    pub fn new() -> Self {
        Self {
            subscribers: RwLock::new(HashMap::with_capacity(64)),
            event_history: RwLock::new(RingBuffer::new(EVENT_RING_BUFFER_SIZE)),
            subscription_counter: AtomicU64::new(1),
            published_count: AtomicU64::new(0),
            delivered_count: AtomicU64::new(0),
        }
    }

    /// Subskrybuje typ eventu. Zwraca subscription_id.
    pub fn subscribe(&self, event_type: &str, subscriber: EventSubscriber) -> u64 {
        let subscription_id = self.subscription_counter.fetch_add(1, Ordering::Relaxed);

        let mut subs = self.subscribers.write();
        subs.entry(event_type.to_string())
            .or_insert_with(|| Vec::with_capacity(4))
            .push(subscriber.clone());

        debug!(
            "EventBus: addon '{}' subskrybuje '{}' (subscription_id={})",
            subscriber.addon_id, event_type, subscription_id
        );

        subscription_id
    }

    /// Publikuje event — dodaje do ring buffer.
    /// Dostarczenie do addonow WASM odbywa sie przez AddonManager::handle_event().
    pub fn publish(&self, event: Event) {
        self.published_count.fetch_add(1, Ordering::Relaxed);

        debug!(
            "EventBus: event '{}' od {:?}",
            event.event_type, event.source_addon
        );

        // Dodaj do ring buffer
        self.event_history.write().push(event);
    }

    /// Pobiera liste subskrybentow dla danego typu eventu
    pub fn get_subscribers(&self, event_type: &str) -> Vec<EventSubscriber> {
        let subs = self.subscribers.read();

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
        let mut subs = self.subscribers.write();
        if let Some(subscribers) = subs.get_mut(event_type) {
            subscribers.retain(|s| s.addon_id != addon_id);
            if subscribers.is_empty() {
                subs.remove(event_type);
            }
        }
    }

    /// Odsubskrybowuje addon ze wszystkich typow eventow
    pub fn unsubscribe_all(&self, addon_id: &str) {
        let mut subs = self.subscribers.write();
        subs.values_mut().for_each(|subscribers| {
            subscribers.retain(|s| s.addon_id != addon_id);
        });
        subs.retain(|_, v| !v.is_empty());

        debug!("EventBus: addon '{}' odsubskrybowany ze wszystkich eventow", addon_id);
    }

    /// Odsubskrybowuje konkretna instancje
    pub fn unsubscribe_instance(&self, instance_id: &str) {
        let mut subs = self.subscribers.write();
        subs.values_mut().for_each(|subscribers| {
            subscribers.retain(|s| s.instance_id != instance_id);
        });
        subs.retain(|_, v| !v.is_empty());
    }

    /// Pobiera ostatnie N eventow z ring buffer
    pub fn recent_events(&self, count: usize) -> Vec<Event> {
        self.event_history.read().recent(count)
    }

    /// Zwraca statystyki
    pub fn stats(&self) -> EventBusStats {
        let subs = self.subscribers.read();
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
}

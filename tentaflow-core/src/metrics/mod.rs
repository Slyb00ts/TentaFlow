// =============================================================================
// Plik: metrics/mod.rs
// Opis: Glowna struktura metryk — thread-safe, lock-free countery atomowe.
//       Zbiera statystyki requestow, tokenow i serwisow.
// =============================================================================

pub mod collector;
pub mod service_stats;
pub mod token_counter;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;
use serde::Serialize;

use service_stats::ServiceStats;

/// Glowna struktura metryk routera.
///
/// Wszystkie countery sa atomowe (lock-free) - bezpieczne do wspoldzielenia
/// miedzy watkami bez mutexow. Jedynie `service_stats` uzywa RwLock bo
/// wymaga dynamicznej kolekcji.
pub struct RouterMetrics {
    /// Laczna liczba obsluzonych requestow
    pub total_requests: AtomicU64,
    /// Laczna liczba bledow
    pub total_errors: AtomicU64,
    /// Laczna liczba tokenow wejsciowych (estymacja)
    pub total_input_tokens: AtomicU64,
    /// Laczna liczba tokenow wyjsciowych (estymacja)
    pub total_output_tokens: AtomicU64,
    /// Aktualnie przetwarzane requesty
    pub active_requests: AtomicU64,
    /// Tokeny wygenerowane w ostatniej sekundzie (obliczane przez collector)
    pub tokens_last_second: AtomicU64,
    /// Tokeny wejsciowe w ostatniej sekundzie (obliczane przez collector)
    pub input_tokens_last_second: AtomicU64,
    /// Liczba aktywnych serwisow (ustawiana przez collector z DB)
    pub active_services: AtomicU64,
    /// Statystyki per-serwis
    pub service_stats: RwLock<Vec<ServiceStats>>,
}

impl RouterMetrics {
    /// Tworzy nowa instancje metryk opakowana w Arc.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            total_requests: AtomicU64::new(0),
            total_errors: AtomicU64::new(0),
            total_input_tokens: AtomicU64::new(0),
            total_output_tokens: AtomicU64::new(0),
            active_requests: AtomicU64::new(0),
            tokens_last_second: AtomicU64::new(0),
            input_tokens_last_second: AtomicU64::new(0),
            active_services: AtomicU64::new(0),
            service_stats: RwLock::new(Vec::new()),
        })
    }

    /// Rejestruje nowy request (inkrementuje total i active).
    pub fn record_request(&self) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        self.active_requests.fetch_add(1, Ordering::Relaxed);
    }

    /// Rejestruje zakonczenie requestu (dekrementuje active).
    pub fn record_request_done(&self) {
        self.active_requests.fetch_sub(1, Ordering::Relaxed);
    }

    /// Rejestruje blad.
    pub fn record_error(&self) {
        self.total_errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Rejestruje zuzycie tokenow (wejsciowe i wyjsciowe).
    pub fn record_tokens(&self, input: u64, output: u64) {
        self.total_input_tokens.fetch_add(input, Ordering::Relaxed);
        self.total_output_tokens
            .fetch_add(output, Ordering::Relaxed);
    }

    /// Ustawia liczbe aktywnych serwisow
    pub fn set_active_services(&self, count: u64) {
        self.active_services.store(count, Ordering::Relaxed);
    }

    /// Zwraca migawke wszystkich metryk (do JSON / dashboard / WebSocket).
    pub fn snapshot(&self) -> MetricsSnapshot {
        let service_stats = self.service_stats.read().clone();

        MetricsSnapshot {
            total_requests: self.total_requests.load(Ordering::Relaxed),
            total_errors: self.total_errors.load(Ordering::Relaxed),
            total_input_tokens: self.total_input_tokens.load(Ordering::Relaxed),
            total_output_tokens: self.total_output_tokens.load(Ordering::Relaxed),
            active_requests: self.active_requests.load(Ordering::Relaxed),
            tokens_per_second: self.tokens_last_second.load(Ordering::Relaxed),
            input_tokens_per_second: self.input_tokens_last_second.load(Ordering::Relaxed),
            active_services: self.active_services.load(Ordering::Relaxed),
            service_stats,
        }
    }
}

/// Migawka metryk do serializacji (JSON).
#[derive(Debug, Clone, Serialize)]
pub struct MetricsSnapshot {
    pub total_requests: u64,
    pub total_errors: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub active_requests: u64,
    pub tokens_per_second: u64,
    pub input_tokens_per_second: u64,
    pub active_services: u64,
    pub service_stats: Vec<ServiceStats>,
}

// =============================================================================
// Plik: routing/loadbalancer/circuit_breaker.rs
// Opis: Implementacja wzorca Circuit Breaker dla ochrony przed kaskadowymi
//       awariami backendow. Kazdy backend ma wlasny circuit breaker monitorujacy
//       bledy i automatycznie przelaczajacy sie miedzy stanami:
//       CLOSED (zdrowy) -> OPEN (padl) -> HALF_OPEN (test recovery).
// =============================================================================

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, warn, info};

/// Stan circuit breakera
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Zamkniety - normalny ruch, monitorujemy bledy
    Closed,

    /// Otwarty - backend padl, odrzucamy requesty (fail-fast)
    Open,

    /// Polotwarty - testujemy czy backend sie naprawil
    HalfOpen,
}

/// Konfiguracja circuit breakera
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Ile bledow zanim circuit sie otworzy (CLOSED -> OPEN)
    pub threshold: u32,

    /// Czas w stanie OPEN przed przejsciem do HALF_OPEN (milisekundy)
    pub timeout_ms: u64,

    /// Ile requestow przepuscic w HALF_OPEN dla testu (zazwyczaj 1)
    pub half_open_max_calls: u32,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            threshold: 5,
            timeout_ms: 60_000, // 60 sekund
            half_open_max_calls: 1,
        }
    }
}

/// Wewnetrzny stan circuit breakera (chroniony Mutex)
#[derive(Debug)]
struct CircuitBreakerState {
    /// Aktualny stan
    state: CircuitState,

    /// Licznik bledow (CLOSED state)
    error_count: u32,

    /// Licznik requestow w HALF_OPEN state
    half_open_calls: u32,

    /// Timestamp ostatniego przejscia do OPEN
    opened_at: Option<Instant>,
}

/// Circuit Breaker - wzorzec odpornosci dla backendow
///
/// Thread-safe implementacja (Arc<Mutex<...>>) dla concurrent access.
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    state: Arc<Mutex<CircuitBreakerState>>,
    backend_name: Arc<str>,
    config: CircuitBreakerConfig,
    /// Obliczony raz z config.timeout_ms
    timeout: Duration,
}

impl CircuitBreaker {
    /// Tworzy nowy circuit breaker z konfiguracja
    ///
    /// Parametry:
    /// - backend_name: Nazwa backendu (dla logowania)
    /// - config: Konfiguracja circuit breakera
    ///
    /// Zwraca: Nowy circuit breaker w stanie CLOSED
    pub fn new(backend_name: impl Into<Arc<str>>, config: CircuitBreakerConfig) -> Self {
        let backend_name: Arc<str> = backend_name.into();
        let timeout = Duration::from_millis(config.timeout_ms);
        debug!(
            "Circuit breaker utworzony dla '{}' (threshold: {}, timeout: {}ms)",
            backend_name, config.threshold, config.timeout_ms
        );

        Self {
            state: Arc::new(Mutex::new(CircuitBreakerState {
                state: CircuitState::Closed,
                error_count: 0,
                half_open_calls: 0,
                opened_at: None,
            })),
            backend_name,
            config,
            timeout,
        }
    }

    /// Sprawdza czy mozna wykonac request (czy circuit pozwala)
    ///
    /// Algorytm:
    /// 1. CLOSED -> zawsze pozwol
    /// 2. OPEN -> sprawdz timeout, jesli uplynal -> HALF_OPEN, w przeciwnym razie odrzuc
    /// 3. HALF_OPEN -> pozwol jesli nie przekroczono limitu testowych requestow
    ///
    /// Zwraca: true jesli mozna wykonac request, false jesli circuit jest OPEN
    pub fn can_execute(&self) -> bool {
        // Zbieramy dane i decyzje wewnatrz locka, logujemy po zwolnieniu
        enum LogAction {
            None,
            OpenToHalfOpen,
            OpenRemaining(u128),
            OpenNoTimestamp,
            HalfOpenTest(u32, u32),
            HalfOpenLimit,
        }

        let (result, log_action) = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());

            match state.state {
                CircuitState::Closed => (true, LogAction::None),

                CircuitState::Open => {
                    if let Some(opened_at) = state.opened_at {
                        let elapsed = opened_at.elapsed();

                        if elapsed >= self.timeout {
                            state.state = CircuitState::HalfOpen;
                            state.half_open_calls = 0;
                            (true, LogAction::OpenToHalfOpen)
                        } else {
                            let remaining = (self.timeout - elapsed).as_millis();
                            (false, LogAction::OpenRemaining(remaining))
                        }
                    } else {
                        state.state = CircuitState::Closed;
                        state.error_count = 0;
                        (true, LogAction::OpenNoTimestamp)
                    }
                }

                CircuitState::HalfOpen => {
                    if state.half_open_calls < self.config.half_open_max_calls {
                        state.half_open_calls += 1;
                        let calls = state.half_open_calls;
                        (true, LogAction::HalfOpenTest(calls, self.config.half_open_max_calls))
                    } else {
                        (false, LogAction::HalfOpenLimit)
                    }
                }
            }
        };

        match log_action {
            LogAction::None => {}
            LogAction::OpenToHalfOpen => {
                info!("Circuit breaker '{}': OPEN -> HALF_OPEN (timeout uplynal)", self.backend_name);
            }
            LogAction::OpenRemaining(ms) => {
                debug!("Circuit breaker '{}': OPEN - odrzucam request ({}ms pozostalo)", self.backend_name, ms);
            }
            LogAction::OpenNoTimestamp => {
                warn!("Circuit breaker '{}': OPEN ale brak opened_at - resetuje do CLOSED", self.backend_name);
            }
            LogAction::HalfOpenTest(calls, max) => {
                debug!("Circuit breaker '{}': HALF_OPEN - testowy request {}/{}", self.backend_name, calls, max);
            }
            LogAction::HalfOpenLimit => {
                debug!("Circuit breaker '{}': HALF_OPEN - limit testowych requestow przekroczony", self.backend_name);
            }
        }

        result
    }

    /// Rejestruje sukces requestu
    ///
    /// Algorytm:
    /// - CLOSED -> reset error_count do 0
    /// - HALF_OPEN -> przejdz do CLOSED (backend sie naprawil)
    /// - OPEN -> nie powinno sie zdarzyc (request nie powinien byc wykonany)
    pub fn record_success(&self) {
        let log_action = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());

            match state.state {
                CircuitState::Closed => {
                    if state.error_count > 0 {
                        let prev = state.error_count;
                        state.error_count = 0;
                        Some(("debug_reset", prev))
                    } else {
                        None
                    }
                }

                CircuitState::HalfOpen => {
                    state.state = CircuitState::Closed;
                    state.error_count = 0;
                    state.half_open_calls = 0;
                    state.opened_at = None;
                    Some(("half_open_closed", 0))
                }

                CircuitState::Open => {
                    Some(("open_unexpected", 0))
                }
            }
        };

        match log_action {
            Some(("debug_reset", prev)) => {
                debug!("Circuit breaker '{}': sukces - resetuje error count ({} -> 0)", self.backend_name, prev);
            }
            Some(("half_open_closed", _)) => {
                info!("Circuit breaker '{}': HALF_OPEN -> CLOSED (backend naprawiony)", self.backend_name);
            }
            Some(("open_unexpected", _)) => {
                warn!("Circuit breaker '{}': sukces w stanie OPEN (nie powinno sie zdarzyc)", self.backend_name);
            }
            _ => {}
        }
    }

    /// Rejestruje blad requestu
    ///
    /// Algorytm:
    /// - CLOSED -> error_count++, jesli >= threshold -> OPEN
    /// - HALF_OPEN -> przejdz do OPEN (backend nadal nie dziala)
    /// - OPEN -> nie powinno sie zdarzyc
    pub fn record_failure(&self) {
        enum FailLog {
            ClosedError(u32, u32),
            ClosedToOpen(u32),
            HalfOpenToOpen,
            OpenUnexpected,
        }

        let log_action = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());

            match state.state {
                CircuitState::Closed => {
                    state.error_count += 1;
                    let count = state.error_count;
                    let threshold = self.config.threshold;

                    if count >= threshold {
                        state.state = CircuitState::Open;
                        state.opened_at = Some(Instant::now());
                        FailLog::ClosedToOpen(count)
                    } else {
                        FailLog::ClosedError(count, threshold)
                    }
                }

                CircuitState::HalfOpen => {
                    state.state = CircuitState::Open;
                    state.half_open_calls = 0;
                    state.opened_at = Some(Instant::now());
                    FailLog::HalfOpenToOpen
                }

                CircuitState::Open => {
                    FailLog::OpenUnexpected
                }
            }
        };

        match log_action {
            FailLog::ClosedError(count, threshold) => {
                debug!("Circuit breaker '{}': blad {}/{}", self.backend_name, count, threshold);
            }
            FailLog::ClosedToOpen(count) => {
                warn!("Circuit breaker '{}': CLOSED -> OPEN (threshold przekroczony: {} bledow)", self.backend_name, count);
            }
            FailLog::HalfOpenToOpen => {
                warn!("Circuit breaker '{}': HALF_OPEN -> OPEN (testowy request failed)", self.backend_name);
            }
            FailLog::OpenUnexpected => {
                warn!("Circuit breaker '{}': blad w stanie OPEN (nie powinno sie zdarzyc)", self.backend_name);
            }
        }
    }

    /// Zwraca aktualny stan circuit breakera
    pub fn state(&self) -> CircuitState {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).state
    }

    /// Zwraca aktualny error count (tylko dla CLOSED)
    pub fn error_count(&self) -> u32 {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).error_count
    }

    /// Resetuje circuit breaker do stanu CLOSED (force reset)
    ///
    /// Uzywane do manualnego resetu (np. po restarcie backendu)
    pub fn reset(&self) {
        {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            state.state = CircuitState::Closed;
            state.error_count = 0;
            state.half_open_calls = 0;
            state.opened_at = None;
        };
        info!("Circuit breaker '{}': force reset do CLOSED", self.backend_name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_circuit_breaker_closed_to_open() {
        let config = CircuitBreakerConfig {
            threshold: 3,
            timeout_ms: 1000,
            half_open_max_calls: 1,
        };
        let cb = CircuitBreaker::new("test-backend".to_string(), config);

        // Poczatkowy stan: CLOSED
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.can_execute());

        // Rejestruj bledy
        cb.record_failure();
        assert_eq!(cb.error_count(), 1);
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure();
        assert_eq!(cb.error_count(), 2);
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure();
        assert_eq!(cb.error_count(), 3);

        // Powinien przejsc do OPEN
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.can_execute()); // Odrzuca requesty
    }

    #[test]
    fn test_circuit_breaker_open_to_half_open() {
        let config = CircuitBreakerConfig {
            threshold: 2,
            timeout_ms: 100, // Krotki timeout dla testu
            half_open_max_calls: 1,
        };
        let cb = CircuitBreaker::new("test-backend".to_string(), config);

        // Otworz circuit
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);

        // Czekaj na timeout
        thread::sleep(Duration::from_millis(150));

        // Powinien przejsc do HALF_OPEN
        assert!(cb.can_execute());
        assert_eq!(cb.state(), CircuitState::HalfOpen);
    }

    #[test]
    fn test_circuit_breaker_half_open_to_closed() {
        let config = CircuitBreakerConfig {
            threshold: 2,
            timeout_ms: 100,
            half_open_max_calls: 1,
        };
        let cb = CircuitBreaker::new("test-backend".to_string(), config);

        // Otworz circuit i przejdz do HALF_OPEN
        cb.record_failure();
        cb.record_failure();
        thread::sleep(Duration::from_millis(150));
        assert!(cb.can_execute());

        // Sukces w HALF_OPEN -> CLOSED
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_half_open_to_open() {
        let config = CircuitBreakerConfig {
            threshold: 2,
            timeout_ms: 100,
            half_open_max_calls: 1,
        };
        let cb = CircuitBreaker::new("test-backend".to_string(), config);

        // Otworz circuit i przejdz do HALF_OPEN
        cb.record_failure();
        cb.record_failure();
        thread::sleep(Duration::from_millis(150));
        assert!(cb.can_execute());

        // Blad w HALF_OPEN -> OPEN
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn test_circuit_breaker_reset() {
        let config = CircuitBreakerConfig::default();
        let cb = CircuitBreaker::new("test-backend".to_string(), config);

        // Otworz circuit
        for _ in 0..5 {
            cb.record_failure();
        }
        assert_eq!(cb.state(), CircuitState::Open);

        // Reset
        cb.reset();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert_eq!(cb.error_count(), 0);
    }
}

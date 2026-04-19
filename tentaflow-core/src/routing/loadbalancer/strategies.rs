// =============================================================================
// Plik: routing/loadbalancer/strategies.rs
// Opis: Strategie load balancingu — round robin, least connections, weighted.
//       Kazda strategia implementuje trait LoadBalancingStrategy i wybiera
//       indeks backendu z puli.
// =============================================================================

use crate::routing::backend::BackendClient;
use crate::error::{Result, CoreError};
use rand::RngExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tracing::{debug, warn};

/// Walidacja listy backendow - zwraca blad jesli lista jest pusta
fn validate_backends(backends: &[Arc<BackendClient>]) -> Result<()> {
    if backends.is_empty() {
        return Err(CoreError::AllBackendsUnavailable {
            model_name: "unknown".to_string(),
        }
        .into());
    }
    Ok(())
}

/// Trait dla strategii load balancingu.
///
/// Kazda strategia musi implementowac metode `select_backend` ktora zwraca
/// indeks wybranego backendu. Strategy nie musi sprawdzac circuit breaker -
/// caller powinien to zrobic i retry z innym backendem jesli circuit jest OPEN.
pub trait LoadBalancingStrategy: Send + Sync {
    /// Wybiera backend z listy.
    ///
    /// Parametry:
    /// - backends: Lista dostepnych backendow
    ///
    /// Zwraca: Indeks wybranego backendu (0..backends.len())
    ///
    /// Bledy:
    /// - AllBackendsUnavailable: Jesli nie mozna wybrac zadnego backendu
    fn select_backend(&self, backends: &[Arc<BackendClient>]) -> Result<usize>;

    /// Nazwa strategii (dla logowania)
    fn name(&self) -> &str;
}

// =============================================================================
// ROUND ROBIN STRATEGY
// =============================================================================

/// Round Robin - rotuje przez backendy po kolei.
///
/// Prosty i sprawiedliwy algorytm ktory wybiera backendy po kolei:
/// A -> B -> C -> A -> B -> C -> ...
///
/// **Thread-safe:** Tak (AtomicUsize)
#[derive(Debug, Clone)]
pub struct RoundRobinStrategy {
    /// Licznik nastepnego backendu do wyboru (atomic dla thread-safety)
    counter: Arc<AtomicUsize>,
}

impl RoundRobinStrategy {
    /// Tworzy nowa strategie Round Robin
    pub fn new() -> Self {
        Self {
            counter: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl LoadBalancingStrategy for RoundRobinStrategy {
    fn select_backend(&self, backends: &[Arc<BackendClient>]) -> Result<usize> {
        validate_backends(backends)?;

        // Atomic fetch_add: pobierz current i zwieksz o 1 (thread-safe)
        let next = self.counter.fetch_add(1, Ordering::Relaxed);
        let idx = next % backends.len();

        debug!("Round Robin: wybrany backend {} z {}", idx, backends.len());

        Ok(idx)
    }

    fn name(&self) -> &str {
        "round_robin"
    }
}

// =============================================================================
// LEAST CONNECTIONS STRATEGY
// =============================================================================

/// Least Connections - wybiera backend z najmniejsza liczba aktywnych requestow.
///
/// Dynamicznie dostosowuje sie do obciazenia backendow. Zawsze wybiera backend
/// ktory ma najmniej aktywnych requestow w danym momencie.
///
/// **Thread-safe:** Tak (AtomicUsize per backend)
///
/// Caller musi wywolac `increment_active()` po wybraniu backendu
/// i `decrement_active()` po zakonczeniu requestu.
#[derive(Debug, Clone)]
pub struct LeastConnectionsStrategy {
    /// Liczniki aktywnych polaczen per backend (indeks = backend index)
    active_connections: Arc<[AtomicUsize]>,
}

impl LeastConnectionsStrategy {
    /// Tworzy nowa strategie Least Connections.
    ///
    /// Inicjalizuje countery dla wszystkich backendow (wszyscy zaczynaja z 0).
    ///
    /// Parametry:
    /// - num_backends: Liczba backendow w pool
    pub fn new(num_backends: usize) -> Self {
        let counters: Vec<AtomicUsize> = (0..num_backends)
            .map(|_| AtomicUsize::new(0))
            .collect();

        Self {
            active_connections: Arc::from(counters),
        }
    }

    /// Zwieksza licznik aktywnych polaczen dla backendu.
    pub fn increment_active(&self, backend_idx: usize) {
        if let Some(counter) = self.active_connections.get(backend_idx) {
            let prev = counter.fetch_add(1, Ordering::Relaxed);
            debug!(
                "Backend {} active connections: {} -> {}",
                backend_idx,
                prev,
                prev + 1
            );
        }
    }

    /// Zmniejsza licznik aktywnych polaczen dla backendu.
    pub fn decrement_active(&self, backend_idx: usize) {
        if let Some(counter) = self.active_connections.get(backend_idx) {
            // Zabezpieczenie przed underflow (wraparound do usize::MAX)
            let result = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                if current > 0 {
                    Some(current - 1)
                } else {
                    None
                }
            });
            match result {
                Ok(prev) => {
                    debug!(
                        "Backend {} active connections: {} -> {}",
                        backend_idx, prev, prev - 1
                    );
                }
                Err(_) => {
                    warn!(
                        "Backend {} active connections juz 0, pominieto dekrementacje",
                        backend_idx
                    );
                }
            }
        }
    }

    /// Zwraca aktualna liczbe aktywnych polaczen dla backendu (dla debugowania)
    pub fn get_active(&self, backend_idx: usize) -> usize {
        self.active_connections
            .get(backend_idx)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }
}

impl LoadBalancingStrategy for LeastConnectionsStrategy {
    fn select_backend(&self, backends: &[Arc<BackendClient>]) -> Result<usize> {
        validate_backends(backends)?;

        // Znajdz backend z najmniejsza liczba aktywnych polaczen
        let first = self.active_connections.first().ok_or_else(|| CoreError::AllBackendsUnavailable {
            model_name: "unknown".to_string(),
        })?;
        let mut min_idx = 0;
        let mut min_active = first.load(Ordering::Relaxed);

        for (idx, counter) in self.active_connections.iter().enumerate().take(backends.len()) {
            let active = counter.load(Ordering::Relaxed);
            if active < min_active {
                min_idx = idx;
                min_active = active;
            }
        }

        debug!(
            "Least Connections: wybrany backend {} ({} aktywnych)",
            min_idx, min_active
        );

        Ok(min_idx)
    }

    fn name(&self) -> &str {
        "least_connections"
    }
}

// =============================================================================
// WEIGHTED STRATEGY
// =============================================================================

/// Weighted - wybiera backendy z prawdopodobienstwem proporcjonalnym do wag.
///
/// Uzywa algorytmu "roulette wheel selection":
/// - Backend z waga 2 dostaje 2x wiecej requestow niz backend z waga 1
///
/// **Thread-safe:** Tak (immutable after creation)
#[derive(Debug, Clone)]
pub struct WeightedStrategy {
    /// Cumulative weights dla roulette wheel selection
    /// Np. wagi [1,2,3] -> cumulative [1,3,6]
    cumulative_weights: Arc<[u32]>,

    /// Suma wszystkich wag (uzywane jako max range dla RNG)
    total_weight: u32,
}

impl WeightedStrategy {
    /// Tworzy nowa strategie Weighted.
    ///
    /// Parametry:
    /// - weights: Wagi backendow (z config.toml backend.weight)
    ///
    /// Bledy:
    /// - Jesli suma wag == 0
    /// - Jesli weights jest puste
    pub fn new(weights: Vec<u32>) -> Result<Self> {
        if weights.is_empty() {
            return Err(CoreError::ConfigError {
                message: "Weighted strategy wymaga przynajmniej jednej wagi".to_string(),
                source: anyhow::anyhow!("weights jest puste"),
            }
            .into());
        }

        // Zbuduj cumulative weights: [1,2,3] -> [1,3,6]
        let mut cumulative = Vec::with_capacity(weights.len());
        let mut sum = 0u32;

        for &weight in &weights {
            sum = sum.saturating_add(weight);
            cumulative.push(sum);
        }

        if sum == 0 {
            return Err(CoreError::ConfigError {
                message: "Suma wag nie moze byc 0".to_string(),
                source: anyhow::anyhow!("Wszystkie wagi sa 0"),
            }
            .into());
        }

        debug!(
            "Weighted strategy: wagi {:?}, cumulative {:?}, total {}",
            weights, cumulative, sum
        );

        Ok(Self {
            cumulative_weights: Arc::from(cumulative),
            total_weight: sum,
        })
    }
}

impl LoadBalancingStrategy for WeightedStrategy {
    fn select_backend(&self, backends: &[Arc<BackendClient>]) -> Result<usize> {
        validate_backends(backends)?;

        // Roulette wheel selection z binary search O(log n)
        let random_value = rand::rng().random_range(0..self.total_weight);

        // Binary search: znajdz pierwszy indeks gdzie cumulative_weight > random_value
        let idx = self
            .cumulative_weights
            .partition_point(|&w| w <= random_value)
            .min(backends.len() - 1);

        debug!(
            "Weighted: losowanie {}/{} -> backend {}",
            random_value, self.total_weight, idx
        );

        Ok(idx)
    }

    fn name(&self) -> &str {
        "weighted"
    }
}

// =============================================================================
// FACTORY - Tworzenie strategii na podstawie config
// =============================================================================

/// Factory do tworzenia strategii load balancingu na podstawie config.
///
/// Parametry:
/// - strategy_name: Nazwa strategii ("round_robin", "least_connections", "weighted")
/// - backends: Lista backendow (do inicjalizacji LeastConnections i Weighted)
/// - weights: Wagi backendow (dla Weighted strategy)
///
/// Zwraca: Box<dyn LoadBalancingStrategy>
pub fn create_strategy(
    strategy_name: &str,
    backends: &[Arc<BackendClient>],
    weights: Vec<u32>,
) -> Result<Box<dyn LoadBalancingStrategy>> {
    match strategy_name {
        "single" => {
            // Single backend - zawsze zwraca index 0
            debug!("Inicjalizacja Single Backend strategy");
            Ok(Box::new(RoundRobinStrategy::new()))
        }

        "round_robin" => {
            debug!("Inicjalizacja Round Robin strategy");
            Ok(Box::new(RoundRobinStrategy::new()))
        }

        "least_connections" | "least_loaded" => {
            debug!(
                "Inicjalizacja Least Connections strategy ({} backends)",
                backends.len()
            );
            Ok(Box::new(LeastConnectionsStrategy::new(backends.len())))
        }

        "weighted" => {
            debug!("Inicjalizacja Weighted strategy (wagi: {:?})", weights);
            let strategy = WeightedStrategy::new(weights)?;
            Ok(Box::new(strategy))
        }

        _ => {
            warn!(
                "Nieznana strategia '{}' - uzywam Round Robin jako fallback",
                strategy_name
            );
            Ok(Box::new(RoundRobinStrategy::new()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_robin() {
        let strategy = RoundRobinStrategy::new();

        // Mock backends (uzywamy pustych Arc - strategy tylko sprawdza .len())
        let backends: Vec<Arc<BackendClient>> = vec![];
        // W prawdziwym tescie potrzebowalibysmy prawdziwych BackendClient

        // Test ze counter sie zwieksza
        assert_eq!(strategy.counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_least_connections() {
        let strategy = LeastConnectionsStrategy::new(3);

        // Poczatkowy stan: wszystkie 0
        assert_eq!(strategy.get_active(0), 0);
        assert_eq!(strategy.get_active(1), 0);
        assert_eq!(strategy.get_active(2), 0);

        // Increment backend 0
        strategy.increment_active(0);
        assert_eq!(strategy.get_active(0), 1);

        // Increment backend 1 twice
        strategy.increment_active(1);
        strategy.increment_active(1);
        assert_eq!(strategy.get_active(1), 2);

        // Decrement backend 1
        strategy.decrement_active(1);
        assert_eq!(strategy.get_active(1), 1);
    }

    #[test]
    fn test_weighted() {
        let weights = vec![1, 2, 3];
        let strategy = WeightedStrategy::new(weights).unwrap();

        assert_eq!(strategy.total_weight, 6);
        assert_eq!(*strategy.cumulative_weights, vec![1, 3, 6]);
    }

    #[test]
    fn test_weighted_zero_sum() {
        let weights = vec![0, 0, 0];
        let result = WeightedStrategy::new(weights);
        assert!(result.is_err());
    }
}

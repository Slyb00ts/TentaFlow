// =============================================================================
// Plik: routing/loadbalancer/mod.rs
// Opis: Load balancing i circuit breaker dla backendow.
//       Circuit breaker chroni przed kaskadowymi awariami i zapewnia fail-fast
//       dla niedzialajacych backendow.
// =============================================================================

pub mod circuit_breaker;
pub mod strategies;

pub use circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState};
pub use strategies::{
    create_strategy, LeastConnectionsStrategy, LoadBalancingStrategy, RoundRobinStrategy,
    WeightedStrategy,
};

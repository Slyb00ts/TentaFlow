// =============================================================================
// File: services/runtime/mod.rs
// Unified runtime: takes a model name, walks the catalog through the
// alias resolver, applies strategy ranking, and dispatches to the right
// transport (local handle / mesh forward / flow_engine).
// =============================================================================

pub mod circuit_breaker;
pub mod context;
pub mod executor;
pub mod quic_handle;
pub mod resolver;
pub mod strategy;
pub mod target;
pub mod transport_client;

pub use circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState};
pub use context::{ContextLimitError, ExecutionContext, RouteMetadata};
pub use executor::{ExecutorError, ModelRuntimeExecutor};
pub use resolver::{AliasResolver, ResolveError, ResolveOutcome, ResolveRequest};
pub use strategy::{rank, StrategyState};
pub use target::ResolvedExecutionTarget;

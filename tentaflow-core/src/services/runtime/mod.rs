// =============================================================================
// File: services/runtime/mod.rs
// Unified runtime: takes a model name, walks the catalog through the
// alias resolver, applies strategy ranking + middleware, and dispatches
// to the right transport (local handle / mesh forward / flow).
// =============================================================================

pub mod context;
pub mod executor;
pub mod middleware;
pub mod resolver;
pub mod strategy;
pub mod target;

pub use context::{ContextLimitError, ExecutionContext, RouteMetadata};
pub use executor::{ExecutorError, ModelRuntimeExecutor};
pub use middleware::{
    apply_stack, flush_stack, open_session_stack, PiiFilterFactory, StreamMiddlewareFactory,
    StreamMiddlewareSession, TtsBufferFactory,
};
pub use resolver::{AliasResolver, ResolveError, ResolveOutcome, ResolveRequest};
pub use strategy::{rank, StrategyState};
pub use target::ResolvedExecutionTarget;

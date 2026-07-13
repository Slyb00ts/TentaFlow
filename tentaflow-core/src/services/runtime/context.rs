// =============================================================================
// File: services/runtime/context.rs
// Per-request bookkeeping shared across the runtime layer. Carries identity
// (user → ACL), recursion guards (mesh hop count, alias chain depth, flow
// nesting), and a metadata bag for telemetry. Cheap to clone — adapters and
// strategies pass it by value into nested dispatches.
// =============================================================================

use crate::auth::acl::UserContext;

/// Hard ceilings — anything deeper trips the guard rather than risk an
/// infinite chain. `MAX_HOP_COUNT` mirrors the legacy `MAX_HOPS` in
/// `routing/middleware.rs` so a request bouncing through both old and
/// new dispatch layers cannot exceed a single shared budget.
pub const MAX_HOP_COUNT: u8 = 3;
pub const MAX_ALIAS_DEPTH: usize = 8;
pub const MAX_FLOW_DEPTH: usize = 3;

/// Metadata key used to carry the forward-side hop count across mesh
/// boundaries. Reverse handlers read this and refuse to dispatch when
/// the value is already at or above `MAX_HOP_COUNT` — that cuts off
/// A→B→A cycles that would otherwise reset hop tracking on each node
/// (Codex R3b.7 H2).
pub const MESH_HOP_HEADER: &str = "x-tentaflow-mesh-hop";

/// Tracing breadcrumbs captured during dispatch — surfaced on the
/// response metadata so the GUI can show "served by node X via alias Y".
/// A near-twin lives in `routing/middleware::RouteMetadata` for the
/// legacy dispatch path; the two will collapse into one once handlers
/// stop calling `Router::route_chat_completion` directly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteMetadata {
    /// Node that ran the request end-to-end (`local_node_id` for in-process,
    /// peer node id for forwarded calls).
    pub served_by_node: Option<String>,
    /// REAL model name of the winning `ResolvedExecutionTarget` (the model
    /// that actually served the request), NOT the requested alias. Filled
    /// from `ResolvedExecutionTarget::requested_model()` of the candidate the
    /// failover loop landed on — for MeshForward the wire response echoes the
    /// alias as `model`, so audit telemetry (alias_calls.target_used) must
    /// read this field instead of the response body.
    pub served_model: Option<String>,
    /// `embedded` / `http` / `quic` / `mesh_forward` / `flow_engine` —
    /// matches existing `routing::RouteMetadata` strings so the OpenAI
    /// response shape stays stable.
    pub backend_type: Option<String>,
    /// `direct` / `first_available` / `round_robin`.
    pub strategy_used: Option<String>,
    /// How many fallback targets we tried before this hit landed.
    pub fallbacks_tried: u32,
}

/// Per-request runtime context. Construct via `ExecutionContext::new`
/// (top-level dispatch) and `enter_alias` / `enter_flow` / `enter_hop`
/// when the runtime recurses; each enter helper bumps the matching guard
/// and rejects with `ContextLimitError` when the limit would be exceeded.
#[derive(Debug, Clone, Default)]
pub struct ExecutionContext {
    /// User identity for ACL gating. `None` is reserved for internal
    /// callers (addons, reverse mesh, translate) that bypass user-level
    /// ACL by design.
    pub user: Option<UserContext>,
    /// RAG E1.0 — tożsamość addona-callera (== instance_id) zasiana z
    /// `CallerContext.addon_id` w `service_call.rs`. Przeprowadzana do
    /// `FlowRequestMeta.addon_id` w gałęziach `ResolvedExecutionTarget::Flow`
    /// (chat/embeddings/rerank/tts), żeby węzeł retrievalu w query-flow wiedział
    /// w którą przestrzeń instancji uderzać. `None` dla wywołań nie-addonowych
    /// (routing /v1 user, kamera, agent).
    pub addon_id: Option<String>,
    /// RAG E1.0 — organizacja-właściciel (`CallerContext.org_id`). `None` =>
    /// domyślny tenant. Przeprowadzana razem z `addon_id`.
    pub org_id: Option<String>,
    /// Total mesh hops this request has crossed. Each forward step must
    /// `enter_hop` to bump it; loops trip `MaxHopCount`.
    pub hop_count: u8,
    /// Stack of flow ids currently executing. A flow that resolves into
    /// another flow pushes onto this stack; a 4th entry would mean a
    /// deeply nested user flow, which we reject as a config mistake.
    pub flow_stack: Vec<String>,
    /// Stack of alias names currently being resolved. Each `enter_alias`
    /// pushes; cycle detection compares against existing entries.
    pub alias_stack: Vec<String>,
    /// Telemetry bag — populated as dispatch proceeds, returned on the
    /// final response.
    pub route_metadata: RouteMetadata,
}

/// Reasons an `enter_*` call refused to descend further. Always actionable
/// — the caller maps to a user-facing error (loop detected, too deep).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContextLimitError {
    #[error("alias chain exceeded depth {limit} at '{name}'")]
    MaxAliasDepth { name: String, limit: usize },
    #[error("alias cycle detected at '{name}' (chain: {chain})")]
    AliasCycle { name: String, chain: String },
    #[error("flow recursion exceeded depth {limit} at flow_id={flow_id}")]
    MaxFlowDepth { flow_id: String, limit: usize },
    #[error("mesh forward exceeded {limit} hops")]
    MaxHopCount { limit: u8 },
}

impl ExecutionContext {
    /// Top-level entry — used by HTTP / binary RPC / WebSocket handlers.
    pub fn new(user: Option<UserContext>) -> Self {
        Self {
            user,
            addon_id: None,
            org_id: None,
            hop_count: 0,
            flow_stack: Vec::new(),
            alias_stack: Vec::new(),
            route_metadata: RouteMetadata::default(),
        }
    }

    /// RAG E1.0 — zasiewa tożsamość addona-callera (instance_id) i org. Wołane w
    /// `service_call.rs` z `req.caller`, żeby executor mógł ją przepisać do
    /// `FlowRequestMeta` dla flow-targetu. Builder (zwraca `self`) — zwięzłe
    /// wpięcie tuż po `ExecutionContext::new(None)`.
    pub fn with_addon_identity(mut self, addon_id: Option<String>, org_id: Option<String>) -> Self {
        self.addon_id = addon_id;
        self.org_id = org_id;
        self
    }

    /// Top-level entry seeded with an inherited flow nesting depth (RAG C2).
    /// Capability dispatchers (reranker/embeddings) build a fresh runtime
    /// context per call; when the calling flow node already sits `depth`
    /// flows deep, seeding the `flow_stack` with that many placeholders makes
    /// the runtime `MAX_FLOW_DEPTH` guard account for the inbound depth, so a
    /// self-referential rerank/embeddings flow trips the limit instead of
    /// recursing forever (each re-entry would otherwise reset to 0).
    pub fn new_with_flow_depth(user: Option<UserContext>, depth: u8) -> Self {
        let mut ctx = Self::new(user);
        // Cap przy zasiewie — głębokość ponad limit i tak natychmiast trafi
        // w guard przy pierwszym `enter_flow`, nie potrzeba dłuższego wektora.
        let seed = (depth as usize).min(MAX_FLOW_DEPTH);
        ctx.flow_stack = (0..seed).map(|i| format!("__inherited_{i}")).collect();
        ctx
    }

    /// Push an alias onto the resolution chain. Returns `Err` when the
    /// limit is exceeded or the same alias appears twice (cycle).
    pub fn enter_alias(&mut self, alias: &str) -> Result<(), ContextLimitError> {
        if self.alias_stack.iter().any(|a| a == alias) {
            return Err(ContextLimitError::AliasCycle {
                name: alias.to_string(),
                chain: self.alias_stack.join(" → "),
            });
        }
        if self.alias_stack.len() >= MAX_ALIAS_DEPTH {
            return Err(ContextLimitError::MaxAliasDepth {
                name: alias.to_string(),
                limit: MAX_ALIAS_DEPTH,
            });
        }
        self.alias_stack.push(alias.to_string());
        Ok(())
    }

    /// Pop the most recent alias off the stack — call this on the way out
    /// of a resolution branch so a parallel branch can reuse the slot.
    pub fn leave_alias(&mut self) {
        self.alias_stack.pop();
    }

    /// Push a flow id onto the recursion stack.
    pub fn enter_flow(&mut self, flow_id: &str) -> Result<(), ContextLimitError> {
        if self.flow_stack.len() >= MAX_FLOW_DEPTH {
            return Err(ContextLimitError::MaxFlowDepth {
                flow_id: flow_id.to_string(),
                limit: MAX_FLOW_DEPTH,
            });
        }
        self.flow_stack.push(flow_id.to_string());
        Ok(())
    }

    pub fn leave_flow(&mut self) {
        self.flow_stack.pop();
    }

    /// Increment the mesh hop counter. Forward steps call this before they
    /// dispatch the request to the next node.
    pub fn enter_hop(&mut self) -> Result<(), ContextLimitError> {
        if self.hop_count >= MAX_HOP_COUNT {
            return Err(ContextLimitError::MaxHopCount {
                limit: MAX_HOP_COUNT,
            });
        }
        self.hop_count += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_cycle_is_caught_before_depth_limit() {
        let mut ctx = ExecutionContext::new(None);
        ctx.enter_alias("a").unwrap();
        ctx.enter_alias("b").unwrap();
        let err = ctx.enter_alias("a").unwrap_err();
        match err {
            ContextLimitError::AliasCycle { name, chain } => {
                assert_eq!(name, "a");
                assert_eq!(chain, "a → b");
            }
            other => panic!("expected AliasCycle, got {:?}", other),
        }
    }

    #[test]
    fn alias_depth_8_passes_9_fails() {
        let mut ctx = ExecutionContext::new(None);
        for i in 0..MAX_ALIAS_DEPTH {
            ctx.enter_alias(&format!("a{}", i)).unwrap();
        }
        let err = ctx.enter_alias("overflow").unwrap_err();
        assert!(matches!(err, ContextLimitError::MaxAliasDepth { .. }));
    }

    /// RAG C2 (bug 1, recursion guard): kontekst zasiany odziedziczoną
    /// głębokością NIE startuje od zera. Gdy węzeł flow (`subflow_depth`)
    /// dispatchuje reranker/embeddings, świeży runtime-context dziedziczy tę
    /// głębokość, więc kolejne `enter_flow` narasta od niej zamiast resetować
    /// do 0 — self-referencyjny rerank-flow trafia w `MAX_FLOW_DEPTH`.
    #[test]
    fn seeded_flow_depth_does_not_reset_the_guard() {
        // Re-wejście na głębokości 1: jeden `enter_flow` jeszcze przejdzie
        // (1 → 2 → 3), czwarty trafia w limit.
        let mut ctx = ExecutionContext::new_with_flow_depth(None, 1);
        assert_eq!(ctx.flow_stack.len(), 1);
        ctx.enter_flow("a").unwrap();
        ctx.enter_flow("b").unwrap();
        let err = ctx.enter_flow("c").unwrap_err();
        assert!(matches!(err, ContextLimitError::MaxFlowDepth { .. }));
    }

    /// Re-wejście już NA limicie (głębokość == `MAX_FLOW_DEPTH`) ubija
    /// rekurencję od razu: pierwszy `enter_flow` w zasianym kontekście trafia
    /// w guard. Dowód, że nieograniczona rekurencja jest przerwana po N.
    #[test]
    fn seeded_at_max_flow_depth_trips_immediately() {
        let mut ctx = ExecutionContext::new_with_flow_depth(None, MAX_FLOW_DEPTH as u8);
        assert_eq!(ctx.flow_stack.len(), MAX_FLOW_DEPTH);
        let err = ctx.enter_flow("self").unwrap_err();
        assert!(matches!(err, ContextLimitError::MaxFlowDepth { .. }));
    }

    /// Zasiew ponad limit jest capowany do `MAX_FLOW_DEPTH` (brak alokacji
    /// gigantycznego wektora przy zepsutej/zapętlonej głębokości), a guard
    /// i tak trafia od razu.
    #[test]
    fn seeded_depth_is_capped_at_max() {
        let ctx = ExecutionContext::new_with_flow_depth(None, 250);
        assert_eq!(ctx.flow_stack.len(), MAX_FLOW_DEPTH);
    }

    #[test]
    fn flow_depth_3_passes_4_fails() {
        let mut ctx = ExecutionContext::new(None);
        for i in 0..MAX_FLOW_DEPTH {
            ctx.enter_flow(&i.to_string()).unwrap();
        }
        let err = ctx.enter_flow("99").unwrap_err();
        assert!(matches!(err, ContextLimitError::MaxFlowDepth { .. }));
    }

    #[test]
    fn hop_count_caps_at_max() {
        let mut ctx = ExecutionContext::new(None);
        for _ in 0..MAX_HOP_COUNT {
            ctx.enter_hop().unwrap();
        }
        let err = ctx.enter_hop().unwrap_err();
        assert!(matches!(err, ContextLimitError::MaxHopCount { .. }));
    }

    /// RAG E1.0 (enabler): `with_addon_identity` zasiewa tożsamość addona-callera
    /// (instance_id) i org. To ono niesie tożsamość z `service_call.rs` (z
    /// `req.caller`) przez executor do `FlowRequestMeta` dla flow-targetu. Bez
    /// niego flow widziałby `addon_id=None` i węzeł retrievalu nie wiedziałby w
    /// którą przestrzeń instancji uderzać.
    #[test]
    fn with_addon_identity_seeds_instance_and_org() {
        let ctx = ExecutionContext::new(None)
            .with_addon_identity(Some("inst-rag-1".to_string()), Some("org-7".to_string()));
        assert_eq!(ctx.addon_id.as_deref(), Some("inst-rag-1"));
        assert_eq!(ctx.org_id.as_deref(), Some("org-7"));
    }

    /// Wywołanie nie-addonowe (routing /v1 user, kamera, agent) nie zasiewa
    /// tożsamości — `ExecutionContext::new` zostawia `addon_id`/`org_id` None,
    /// więc węzeł vector odmawia zapisu zamiast trafić w cudzą przestrzeń.
    #[test]
    fn new_without_identity_leaves_addon_and_org_none() {
        let ctx = ExecutionContext::new(None);
        assert!(ctx.addon_id.is_none());
        assert!(ctx.org_id.is_none());
    }

    #[test]
    fn leave_alias_lets_a_sibling_branch_reuse_the_name() {
        let mut ctx = ExecutionContext::new(None);
        ctx.enter_alias("primary").unwrap();
        ctx.enter_alias("fallback-a").unwrap();
        ctx.leave_alias(); // back out of fallback-a
                           // Sibling branch can now visit fallback-b and even revisit a
                           // previously-popped alias by name.
        ctx.enter_alias("fallback-b").unwrap();
        ctx.leave_alias();
        ctx.enter_alias("fallback-a").unwrap();
    }
}

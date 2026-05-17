// =============================================================================
// File: flow_runtime/registry.rs — process-wide store of compiled flows
// =============================================================================
//
// Owned by the addon lifecycle: `install` adds entries for each compiled
// flow template, `uninstall` drops every entry whose `addon_id` matches.
// Lookup is `(addon_id, flow_id) -> Arc<CompiledFlow>` so a long-running
// invocation can hold the flow definition without blocking subsequent
// re-installs of the same addon (the new install puts a new Arc behind the
// key; in-flight tasks keep the old Arc until they complete).

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use super::types::CompiledFlow;

pub struct FlowRegistry {
    inner: RwLock<HashMap<(String, String), Arc<CompiledFlow>>>,
}

impl Default for FlowRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl FlowRegistry {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    /// Inserts (or replaces) the compiled flow for `(addon_id, flow.def.id)`.
    pub fn register(&self, addon_id: &str, flow: Arc<CompiledFlow>) {
        let flow_id = flow.def.id.clone();
        let mut guard = self.inner.write().expect("flow registry write lock");
        guard.insert((addon_id.to_string(), flow_id), flow);
    }

    pub fn get(&self, addon_id: &str, flow_id: &str) -> Option<Arc<CompiledFlow>> {
        let guard = self.inner.read().expect("flow registry read lock");
        guard
            .get(&(addon_id.to_string(), flow_id.to_string()))
            .cloned()
    }

    /// Drops every flow owned by `addon_id`. Called from addon uninstall.
    /// Returns the number of entries removed.
    pub fn unregister_addon(&self, addon_id: &str) -> usize {
        let mut guard = self.inner.write().expect("flow registry write lock");
        let before = guard.len();
        guard.retain(|(aid, _), _| aid != addon_id);
        before - guard.len()
    }

    /// Returns flow ids owned by `addon_id`, sorted lexicographically for
    /// stable diagnostics / test assertions.
    pub fn list_for_addon(&self, addon_id: &str) -> Vec<String> {
        let guard = self.inner.read().expect("flow registry read lock");
        let mut out: Vec<String> = guard
            .keys()
            .filter(|(aid, _)| aid == addon_id)
            .map(|(_, fid)| fid.clone())
            .collect();
        out.sort();
        out
    }
}

static FLOW_REGISTRY: OnceLock<FlowRegistry> = OnceLock::new();

/// Process-wide singleton. Initialized lazily on first access.
pub fn global() -> &'static FlowRegistry {
    FLOW_REGISTRY.get_or_init(FlowRegistry::default)
}

// === File: peer_registry/shard.rs — sharded storage map keyed by NodeId ===

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

use crate::mesh::peer_registry::entry::{NodeId, PeerEntry};

pub const NUM_SHARDS: usize = 64;

/// One shard of the peer map. Each shard has its own RwLock so reads on
/// different shards don't block each other.
pub struct Shard {
    pub map: RwLock<HashMap<NodeId, Arc<RwLock<PeerEntry>>>>,
}

impl Shard {
    pub fn new() -> Self {
        Self { map: RwLock::new(HashMap::new()) }
    }
}

impl Default for Shard {
    fn default() -> Self {
        Self::new()
    }
}

/// Deterministic shard selection using FxHash on the first 8 bytes of node_id.
/// Avoids SipHash overhead and stays stable across process restarts (we never
/// rely on the value across versions, but determinism helps debugging).
pub fn shard_for(id: &NodeId, n: usize) -> usize {
    use rustc_hash::FxHasher;
    use std::hash::Hasher;
    let mut h = FxHasher::default();
    h.write(&id[..8]);
    (h.finish() as usize) % n
}

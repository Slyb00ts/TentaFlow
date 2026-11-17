//! Service SLA policies and the per-service gates that enforce them
//! (SUM/tentabus/PLAN-TB03-LIMITY-SLA.md, ML 90 and ML 91).
//!
//! `policy` holds the stored types and their validation, `repository` the
//! org-scoped database layer together with its Sync Ledger capture and apply,
//! `gate` the in-memory enforcement of one service's limits and `registry` the
//! node-global map of gates that is rebuilt when a policy or an assignment
//! changes. `breach`, `evaluator` and `status` are the monitoring side: the
//! stored breaches and minute samples, the per-node evaluator that judges the
//! gates' stats against the policy thresholds, and the current-state reads.
//!
//! Limits are counted per node and live in memory: a restart zeroes the call
//! window, and a cluster of N nodes serving the same flow admits up to N times
//! the limit. The policies themselves replicate; the counters never do.

pub mod breach;
pub mod evaluator;
pub mod gate;
pub mod policy;
pub mod registry;
pub mod repository;
pub mod status;

use std::sync::atomic::{AtomicU64, Ordering};

pub use gate::{
    AdmitOrigin, GateConfig, GateLimits, GateStats, LatencyHistogram, Outcome, RefusalReason,
    ServiceGate, SlaPermit, SlaRefusal,
};
pub use policy::{BusOrdering, SlaAssignment, SlaPolicy, SlaPolicySpec, SLA_TARGET_FLOW};
pub use registry::{global, SlaRegistry};
pub use repository::{Result, SlaError};

/// Bumped on every committed change to a policy or an assignment, local or
/// applied from the mesh. A registry that loaded an older value is stale.
static GENERATION: AtomicU64 = AtomicU64::new(1);

pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

pub fn bump_generation() -> u64 {
    GENERATION.fetch_add(1, Ordering::AcqRel) + 1
}

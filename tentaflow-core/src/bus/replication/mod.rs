// =============================================================================
// File: bus/replication/mod.rs — M2 replication & failover (PLAN-M2)
// =============================================================================
//
// Wave 0 (coordinator): only `frames.rs` (the frozen wire contract, PLAN-M2
// §1b) and `assignment.rs` (the frozen `PartitionAssignment` type, PLAN-M2
// §1c) exist in this build. Every other file this module will eventually
// hold is wave-1 work, one file per agent, all building against `frames.rs`
// and against `tentaflow_bus::Partition`'s contract (PLAN-M2 §1a) read-only:
//
//   leader.rs    (agent RL) — leader-side feeder/ACK bookkeeping/ISR.
//   follower.rs  (agent RF) — follower-side stream handling, Truncate,
//                             leader-lease watchdog.
//   election.rs  (agent EL) — promotion state machine (LeoQuery, majority,
//                             epoch monotonicity).
//   manager.rs   (agent EL) — `ReplicationManager`, the
//                             `bus::ReplicationCoordinator` implementor,
//                             ALPN_BUS dial/accept lifecycle.
//   metrics.rs   (agent RL) — replication gauges/counters.
//
// None of those are declared here yet — adding `pub mod leader;` etc.
// before the corresponding file exists would not compile, and this module
// is deliberately left buildable with just the two wave-0 files so every
// other wave-0 deliverable (which depends on `tentaflow-core` compiling as
// a whole) is unaffected by wave-1's in-progress work landing later.

pub mod assignment;
pub mod election;
pub mod follower;
pub mod frames;
pub mod glue;
pub mod init;
pub mod leader;
pub mod manager;
pub mod metrics;
pub mod router;

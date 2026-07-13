// =============================================================================
// Plik: sync/ledger/mod.rs
// Opis: Publiczne API Sync Ledger: typy operacji, trait storage i implementacja Fjall.
// =============================================================================

mod fjall_store;
mod types;
mod validation;

pub use fjall_store::FjallSyncLedgerStore;
pub(crate) use types::{decode, encode, hash_canonical};
pub use types::{
    partition_materialization_order, ActionType, AppendResult, BaselineEpoch, CompactionPolicy,
    FieldValue, HybridLogicalTimestamp, InboxEntry, LedgerResult, NewSyncOperation, NodeChainEntry,
    NodeFrontierEntry, NodeHead, NodeLogQuery, OperationId, OperationQuery, OutboxEntry,
    PartitionId, PeerId, RedactedRecord, RepairQueueEntry, SnapshotId, SyncLedgerError,
    SyncLedgerStore, SyncMerkleSummary, SyncOperation, SyncOperationBody, SyncOperationSigner,
    SyncOperationVerifier, SyncSnapshot, SyncTarget, CORE_PARTITION_PREFIX,
};
pub use validation::{
    build_merkle_summary, node_frontier_for_operations, operation_body_hash, validate_hash_chain,
    validate_hash_chain_anchored, validate_hash_chain_from, Ed25519OperationSigner,
    HexNodeIdOperationVerifier, TrustedKeyOperationVerifier,
};

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
    ActionType, AppendResult, CompactionPolicy, FieldValue, HybridLogicalTimestamp, InboxEntry,
    LedgerResult, NewSyncOperation, OperationId, OperationQuery, OutboxEntry, PartitionHead,
    PartitionId, PeerCursor, PeerId, RepairQueueEntry, SnapshotId, SyncLedgerError,
    SyncLedgerStore, SyncMerkleSummary, SyncOperation, SyncOperationBody, SyncOperationSigner,
    SyncOperationVerifier, SyncSnapshot, SyncTarget,
};
pub use validation::{
    Ed25519OperationSigner, HexNodeIdOperationVerifier, TrustedKeyOperationVerifier,
    build_merkle_summary, operation_body_hash, validate_hash_chain, validate_hash_chain_from,
};

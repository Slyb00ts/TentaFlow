// =============================================================================
// Plik: sync/ledger/types.rs
// Opis: Typy domenowe Sync Ledger i kontrakt storage używany przez implementacje KV.
// =============================================================================

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

pub type LedgerResult<T> = std::result::Result<T, SyncLedgerError>;

#[derive(Debug, thiserror::Error)]
pub enum SyncLedgerError {
    #[error("pusty identyfikator: {0}")]
    EmptyIdentifier(&'static str),
    #[error("nie znaleziono operacji: {0}")]
    OperationNotFound(OperationId),
    #[error("nie znaleziono wpisu outbox: target={target}, op_id={op_id}")]
    OutboxEntryNotFound { target: String, op_id: OperationId },
    #[error("nie znaleziono wpisu inbox: peer={peer}, op_id={op_id}")]
    InboxEntryNotFound { peer: String, op_id: OperationId },
    #[error("nie znaleziono snapshotu: partition={partition}, snapshot_id={snapshot_id}")]
    SnapshotNotFound {
        partition: String,
        snapshot_id: String,
    },
    #[error("snapshot nie zgadza sie z ledgerem: snapshot_id={snapshot_id}, field={field}")]
    SnapshotIntegrityMismatch {
        snapshot_id: String,
        field: &'static str,
    },
    #[error("hash operacji nie zgadza sie z trescia: {op_id}")]
    InvalidOperationHash { op_id: OperationId },
    #[error(
        "identyfikator operacji nie zgadza sie z hashem: expected={expected}, actual={actual}"
    )]
    InvalidOperationId {
        expected: OperationId,
        actual: OperationId,
    },
    #[error("niepoprawny identyfikator operacji: {value}")]
    InvalidOperationIdHex { value: String },
    #[error(
        "actor_node_id operacji nie zgadza sie z signerem: expected={expected}, actual={actual}"
    )]
    ActorNodeMismatch { expected: String, actual: String },
    #[error("niepoprawna dlugosc podpisu: {len}")]
    InvalidSignatureLength { len: usize },
    #[error("niepoprawny podpis operacji od {actor_node_id}")]
    InvalidSignature { actor_node_id: String },
    #[error("niepoprawny klucz publiczny dla {actor_node_id}")]
    InvalidPublicKey { actor_node_id: String },
    #[error("hash-chain partycji nie zgadza sie: partition={partition}, sequence={sequence}")]
    HashChainMismatch { partition: String, sequence: u64 },
    #[error("merkle summary wymaga przynajmniej jednej operacji")]
    EmptyMerkleSummary,
    #[error("operacja z innej partycji w merkle summary: expected={expected}, actual={actual}")]
    MerklePartitionMismatch { expected: String, actual: String },
    #[error("luka sekwencji w merkle summary: expected={expected}, actual={actual}")]
    MerkleSequenceGap { expected: u64, actual: u64 },
    #[error("błąd serializacji ledgera: {0}")]
    Codec(#[from] rmp_serde::encode::Error),
    #[error("błąd deserializacji ledgera: {0}")]
    Decode(#[from] rmp_serde::decode::Error),
    #[error("błąd storage Fjall: {0}")]
    Fjall(#[from] fjall::Error),
    #[error("błąd runtime sync: {0}")]
    Runtime(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct OperationId([u8; 32]);

impl OperationId {
    pub fn from_hash(hash: [u8; 32]) -> Self {
        Self(hash)
    }

    pub fn from_hex(value: &str) -> LedgerResult<Self> {
        let bytes = hex::decode(value).map_err(|_| SyncLedgerError::InvalidOperationIdHex {
            value: value.to_string(),
        })?;
        let hash: [u8; 32] =
            bytes
                .try_into()
                .map_err(|_| SyncLedgerError::InvalidOperationIdHex {
                    value: value.to_string(),
                })?;
        Ok(Self(hash))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Display for OperationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PartitionId(String);

impl PartitionId {
    pub fn new(value: impl Into<String>) -> LedgerResult<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(SyncLedgerError::EmptyIdentifier("partition_id"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PeerId(String);

impl PeerId {
    pub fn new(value: impl Into<String>) -> LedgerResult<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(SyncLedgerError::EmptyIdentifier("peer_id"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SyncTarget(String);

impl SyncTarget {
    pub fn new(value: impl Into<String>) -> LedgerResult<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(SyncLedgerError::EmptyIdentifier("sync_target"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SnapshotId(String);

impl SnapshotId {
    pub fn new(value: impl Into<String>) -> LedgerResult<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(SyncLedgerError::EmptyIdentifier("snapshot_id"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionType {
    Insert,
    Update,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FieldValue {
    Null,
    Bool(bool),
    I64(i64),
    U64(u64),
    Decimal(String),
    String(String),
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HybridLogicalTimestamp {
    pub wall_time_ms: i64,
    pub logical: u32,
    pub node_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewSyncOperation {
    pub org_id: String,
    pub partition_id: PartitionId,
    pub addon_id: String,
    pub resource_type: String,
    pub resource_id: String,
    pub table_name: String,
    pub primary_key: String,
    pub action: ActionType,
    pub changed_fields: BTreeMap<String, FieldValue>,
    pub before_hash: Option<[u8; 32]>,
    pub after_hash: Option<[u8; 32]>,
    pub actor_user_id: String,
    pub actor_device_id: String,
    pub actor_node_id: String,
    pub hlc_timestamp: HybridLogicalTimestamp,
    pub payload_hash: [u8; 32],
    pub acl_snapshot_hash: [u8; 32],
    pub policy_epoch: u64,
    pub encryption_info: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncOperationBody {
    pub org_id: String,
    pub partition_id: PartitionId,
    pub partition_sequence: u64,
    pub addon_id: String,
    pub resource_type: String,
    pub resource_id: String,
    pub table_name: String,
    pub primary_key: String,
    pub action: ActionType,
    pub changed_fields: BTreeMap<String, FieldValue>,
    pub before_hash: Option<[u8; 32]>,
    pub after_hash: Option<[u8; 32]>,
    pub actor_user_id: String,
    pub actor_device_id: String,
    pub actor_node_id: String,
    pub hlc_timestamp: HybridLogicalTimestamp,
    pub prev_partition_hash: Option<[u8; 32]>,
    pub payload_hash: [u8; 32],
    pub acl_snapshot_hash: [u8; 32],
    pub policy_epoch: u64,
    pub encryption_info: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncOperation {
    pub op_id: OperationId,
    pub operation_hash: [u8; 32],
    pub body: SyncOperationBody,
    pub signature: Vec<u8>,
}

impl SyncOperation {
    pub fn from_new(
        new_operation: NewSyncOperation,
        partition_sequence: u64,
        prev_partition_hash: Option<[u8; 32]>,
        signer: &dyn SyncOperationSigner,
    ) -> LedgerResult<Self> {
        if new_operation.actor_node_id != signer.node_id() {
            return Err(SyncLedgerError::ActorNodeMismatch {
                expected: signer.node_id().to_string(),
                actual: new_operation.actor_node_id,
            });
        }
        let body = SyncOperationBody {
            org_id: new_operation.org_id,
            partition_id: new_operation.partition_id,
            partition_sequence,
            addon_id: new_operation.addon_id,
            resource_type: new_operation.resource_type,
            resource_id: new_operation.resource_id,
            table_name: new_operation.table_name,
            primary_key: new_operation.primary_key,
            action: new_operation.action,
            changed_fields: new_operation.changed_fields,
            before_hash: new_operation.before_hash,
            after_hash: new_operation.after_hash,
            actor_user_id: new_operation.actor_user_id,
            actor_device_id: new_operation.actor_device_id,
            actor_node_id: new_operation.actor_node_id,
            hlc_timestamp: new_operation.hlc_timestamp,
            prev_partition_hash,
            payload_hash: new_operation.payload_hash,
            acl_snapshot_hash: new_operation.acl_snapshot_hash,
            policy_epoch: new_operation.policy_epoch,
            encryption_info: new_operation.encryption_info,
        };
        let operation_hash = hash_canonical(&body)?;
        let op_id = OperationId::from_hash(operation_hash);
        let signature = signer.sign_operation(&signing_bytes_for_hash(operation_hash))?;
        Ok(Self {
            op_id,
            operation_hash,
            body,
            signature,
        })
    }

    pub fn canonical_bytes(&self) -> LedgerResult<Vec<u8>> {
        encode(self)
    }

    pub fn signing_bytes(&self) -> Vec<u8> {
        signing_bytes_for_hash(self.operation_hash)
    }

    pub fn validate_integrity(&self) -> LedgerResult<()> {
        let expected_hash = hash_canonical(&self.body)?;
        if expected_hash != self.operation_hash {
            return Err(SyncLedgerError::InvalidOperationHash { op_id: self.op_id });
        }
        let expected_id = OperationId::from_hash(expected_hash);
        if expected_id != self.op_id {
            return Err(SyncLedgerError::InvalidOperationId {
                expected: expected_id,
                actual: self.op_id,
            });
        }
        Ok(())
    }
}

pub trait SyncOperationSigner: Send + Sync {
    fn node_id(&self) -> &str;
    fn sign_operation(&self, message: &[u8]) -> LedgerResult<Vec<u8>>;
}

pub trait SyncOperationVerifier: Send + Sync {
    fn verify_operation_signature(&self, operation: &SyncOperation) -> LedgerResult<()>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionHead {
    pub partition_id: PartitionId,
    pub last_sequence: u64,
    pub last_hash: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppendResult {
    pub op_id: OperationId,
    pub operation_hash: [u8; 32],
    pub previous_partition_hash: Option<[u8; 32]>,
    pub partition_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationQuery {
    pub partition_id: PartitionId,
    pub from_sequence: Option<u64>,
    pub to_sequence: Option<u64>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEntry {
    pub target: SyncTarget,
    pub op_id: OperationId,
    pub delivered: bool,
    pub acknowledged: bool,
    pub retry_count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboxEntry {
    pub source: PeerId,
    pub operation: SyncOperation,
    pub applied: bool,
    #[serde(default)]
    pub conflicted: bool,
    #[serde(default)]
    pub conflict_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerCursor {
    pub peer: PeerId,
    pub partition_id: PartitionId,
    pub last_sequence: u64,
    pub last_hash: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairQueueEntry {
    pub peer: PeerId,
    pub partition_id: PartitionId,
    pub from_sequence: u64,
    pub next_attempt_ms: i64,
    pub retry_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncSnapshot {
    pub snapshot_id: SnapshotId,
    pub partition_id: PartitionId,
    #[serde(default)]
    pub from_sequence: u64,
    pub up_to_sequence: u64,
    #[serde(default)]
    pub operation_count: u64,
    pub root_hash: [u8; 32],
    #[serde(default)]
    pub state_hash: [u8; 32],
    #[serde(default)]
    pub last_operation_hash: Option<[u8; 32]>,
    #[serde(default)]
    pub policy_epoch: u64,
    #[serde(default)]
    pub blob_kind: Option<String>,
    #[serde(default)]
    pub blob_hash: Option<[u8; 32]>,
    #[serde(default)]
    pub blob_size_bytes: u64,
    pub created_at_ms: i64,
    pub author_node_id: String,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncMerkleSummary {
    pub partition_id: PartitionId,
    pub from_sequence: u64,
    pub to_sequence: u64,
    pub operation_count: u64,
    pub root_hash: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionPolicy {
    pub partition_id: PartitionId,
    pub keep_operations_after_sequence: Option<u64>,
}

pub trait SyncLedgerStore: Send + Sync {
    fn append_operation(
        &self,
        operation: NewSyncOperation,
        signer: &dyn SyncOperationSigner,
    ) -> LedgerResult<AppendResult>;
    fn get_operations(&self, query: OperationQuery) -> LedgerResult<Vec<SyncOperation>>;
    fn get_operation(&self, op_id: OperationId) -> LedgerResult<SyncOperation>;
    fn put_in_outbox(&self, target: SyncTarget, op_id: OperationId) -> LedgerResult<()>;
    fn get_outbox_entry(&self, target: SyncTarget, op_id: OperationId)
        -> LedgerResult<OutboxEntry>;
    fn list_pending_outbox(
        &self,
        target: SyncTarget,
        limit: usize,
    ) -> LedgerResult<Vec<OutboxEntry>>;
    fn put_verified_in_inbox(
        &self,
        source: PeerId,
        operation: SyncOperation,
        verifier: &dyn SyncOperationVerifier,
    ) -> LedgerResult<()>;
    fn get_inbox_entry(&self, source: PeerId, op_id: OperationId) -> LedgerResult<InboxEntry>;
    fn list_unapplied_inbox(&self, limit: usize) -> LedgerResult<Vec<InboxEntry>>;
    fn mark_inbox_applied(&self, source: PeerId, op_id: OperationId) -> LedgerResult<()>;
    fn mark_inbox_conflicted(
        &self,
        source: PeerId,
        op_id: OperationId,
        message: String,
    ) -> LedgerResult<()>;
    fn mark_delivered(&self, target: SyncTarget, op_id: OperationId) -> LedgerResult<()>;
    fn mark_acknowledged(&self, target: SyncTarget, op_id: OperationId) -> LedgerResult<()>;
    fn get_peer_cursor(
        &self,
        peer: PeerId,
        partition: PartitionId,
    ) -> LedgerResult<Option<PeerCursor>>;
    fn save_peer_cursor(&self, cursor: PeerCursor) -> LedgerResult<()>;
    fn upsert_repair_request(&self, entry: RepairQueueEntry) -> LedgerResult<()>;
    fn list_due_repair_requests(
        &self,
        peer: PeerId,
        now_ms: i64,
        limit: usize,
    ) -> LedgerResult<Vec<RepairQueueEntry>>;
    fn mark_repair_attempted(
        &self,
        peer: PeerId,
        partition: PartitionId,
        next_attempt_ms: i64,
        retry_count: u32,
    ) -> LedgerResult<()>;
    fn remove_repair_request(&self, peer: PeerId, partition: PartitionId) -> LedgerResult<()>;
    fn save_snapshot(&self, snapshot: SyncSnapshot) -> LedgerResult<()>;
    fn get_snapshot(
        &self,
        partition: PartitionId,
        up_to_sequence: u64,
        snapshot_id: SnapshotId,
    ) -> LedgerResult<SyncSnapshot>;
    fn latest_snapshot(
        &self,
        partition: PartitionId,
        up_to_sequence: Option<u64>,
    ) -> LedgerResult<Option<SyncSnapshot>>;
    fn get_partition_head(&self, partition: PartitionId) -> LedgerResult<Option<PartitionHead>>;
    fn list_outbox_for_partition(
        &self,
        partition: PartitionId,
        up_to_sequence: u64,
    ) -> LedgerResult<Vec<OutboxEntry>>;
    fn compact(&self, policy: CompactionPolicy) -> LedgerResult<()>;
}

pub(crate) fn encode<T: Serialize>(value: &T) -> LedgerResult<Vec<u8>> {
    Ok(rmp_serde::to_vec_named(value)?)
}

pub(crate) fn decode<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> LedgerResult<T> {
    Ok(rmp_serde::from_slice(bytes)?)
}

pub(crate) fn hash_canonical<T: Serialize>(value: &T) -> LedgerResult<[u8; 32]> {
    Ok(*blake3::hash(&encode(value)?).as_bytes())
}

pub(crate) fn signing_bytes_for_hash(operation_hash: [u8; 32]) -> Vec<u8> {
    let mut bytes = b"tentaflow-sync-operation-v1".to_vec();
    bytes.extend_from_slice(&operation_hash);
    bytes
}

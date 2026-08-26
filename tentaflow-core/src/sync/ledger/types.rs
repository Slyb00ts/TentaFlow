// =============================================================================
// Plik: sync/ledger/types.rs
// Opis: Typy domenowe Sync Ledger i kontrakt storage używany przez implementacje KV.
// =============================================================================

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

pub use tentaflow_protocol::environment::NodeEnvironment;
pub use tentaflow_protocol::mesh::BaselineEpoch;

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
    #[error("hash-chain noda nie zgadza sie: node={node}, node_seq={node_seq}")]
    HashChainMismatch { node: String, node_seq: u64 },
    /// Two distinct operations carry the same `(actor_node_id, node_seq)` but
    /// different `op_hash`. A node is single-writer over its own chain, so this
    /// can only mean the author signed two conflicting histories (Byzantine
    /// equivocation). The operation is rejected outright — never repaired.
    #[error("equivocation noda: node={node}, node_seq={node_seq}, existing={existing}, incoming={incoming}")]
    NodeEquivocation {
        node: String,
        node_seq: u64,
        existing: OperationId,
        incoming: OperationId,
    },
    /// `hlc_timestamp.node_id` must equal `actor_node_id`. A node mints HLC
    /// timestamps only on its own behalf, so a divergence means a forged HLC
    /// origin — an attempt to skew last-writer-wins resolution by pretending an
    /// op came from a different (e.g. higher-priority) node. Rejected outright.
    #[error("hlc.node_id nie zgadza sie z actor_node_id: actor={actor}, hlc={hlc}")]
    HlcNodeMismatch { actor: String, hlc: String },
    /// `node_seq` is 1-based and dense; seq 0 is never a valid chain position.
    #[error("niepoprawny node_seq=0 dla noda {node}")]
    InvalidNodeSeq { node: String },
    /// The on-disk ledger schema does not match the version this build expects.
    /// Triggers a wipe + reseed under a bumped epoch so a layout change (e.g.
    /// per-partition → per-node chains) never silently restarts node_seq.
    #[error("niezgodna wersja schematu ledgera: on_disk={on_disk}, expected={expected}")]
    SchemaVersionMismatch { on_disk: u32, expected: u32 },
    #[error("operacja z innego epoch baseline: expected={expected:?}, actual={actual:?}")]
    EpochMismatch {
        expected: BaselineEpoch,
        actual: BaselineEpoch,
    },
    /// Independent from `EpochMismatch` — a node's environment (Dev/Test/Prod)
    /// and its baseline epoch are two separate fencing dimensions (ROADMAP
    /// Z12). An operation minted under a different declared environment than
    /// the local node's is rejected outright, never repaired, mirroring
    /// `NodeEquivocation`/`EpochMismatch`. Enforced at the SAME admission
    /// point as `EpochMismatch` (`fjall_store.rs::put_verified_in_inbox` /
    /// `admit_verified_operation`).
    #[error("operacja z innego srodowiska: expected={expected}, actual={actual}")]
    EnvironmentMismatch {
        expected: NodeEnvironment,
        actual: NodeEnvironment,
    },
    #[error("merkle summary wymaga przynajmniej jednej operacji")]
    EmptyMerkleSummary,
    #[error("operacja z innej partycji w merkle summary: expected={expected}, actual={actual}")]
    MerklePartitionMismatch { expected: String, actual: String },
    #[error("luka sekwencji w merkle summary: expected={expected}, actual={actual}")]
    MerkleSequenceGap { expected: u64, actual: u64 },
    #[error("błąd serializacji CBOR ledgera: {0}")]
    Codec(String),
    #[error("błąd deserializacji CBOR ledgera: {0}")]
    Decode(String),
    #[error("błąd storage Fjall: {0}")]
    Fjall(#[from] fjall::Error),
    #[error("błąd runtime sync: {0}")]
    Runtime(String),
    /// A purely ordering-related failure: the operation could not be applied yet
    /// because a causally-prior operation (the INSERT that creates the target row
    /// of an UPDATE) has not landed. NOT a data conflict — the inbox entry must
    /// stay retryable so a later drain applies it once the prerequisite arrives.
    #[error("operacja odroczona (brak operacji przyczynowej): {0}")]
    DeferredOrdering(String),
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

// Total order over the lexicographic tuple (wall_time_ms, logical, node_id).
// The node_id tie-break makes the order total even when two nodes mint the
// same (wall, logical) pair, which is what conflict resolution needs to pick a
// single deterministic winner across the whole mesh.
impl Ord for HybridLogicalTimestamp {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.wall_time_ms
            .cmp(&other.wall_time_ms)
            .then_with(|| self.logical.cmp(&other.logical))
            .then_with(|| self.node_id.cmp(&other.node_id))
    }
}

impl PartialOrd for HybridLogicalTimestamp {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
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
    pub epoch: BaselineEpoch,
    // Independent, append-only field alongside `epoch` (ROADMAP Z12) — the
    // node's declared environment (Dev/Test/Prod) at mint time. NOT a
    // replacement for `epoch`: a schema migration inside one environment
    // still bumps `epoch` alone, and switching environment bumps both.
    pub environment: NodeEnvironment,
    pub payload_hash: [u8; 32],
    pub acl_snapshot_hash: [u8; 32],
    pub policy_epoch: u64,
    pub encryption_info: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncOperationBody {
    pub org_id: String,
    pub partition_id: PartitionId,
    // Per-node sequence: a strictly-monotonic counter in the `actor_node_id`
    // space. A node is single-writer over its own chain, so this never forks the
    // way a per-partition counter did once two nodes wrote one partition.
    pub node_seq: u64,
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
    // Pre-epoch operations (read from disk, or received from un-upgraded peers)
    // carry no `epoch` field. Defaulting to genesis lets them deserialize cleanly
    // and then get rejected by epoch-fencing in put_verified_in_inbox
    // (EpochMismatch), instead of a hard CBOR `missing field` failure.
    #[serde(default)]
    pub epoch: BaselineEpoch,
    // Same compatibility pattern as `epoch` above, independent dimension
    // (ROADMAP Z12). A pre-Z12 operation (or one from an un-upgraded peer)
    // decodes as `NodeEnvironment::default()` (Prod) and is then fenced by
    // `EnvironmentMismatch` in admission like any other cross-environment
    // operation — never silently accepted as same-environment.
    #[serde(default)]
    pub environment: NodeEnvironment,
    // Hash of the previous operation in THIS node's chain (None = node genesis).
    pub prev_node_hash: Option<[u8; 32]>,
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
        node_seq: u64,
        prev_node_hash: Option<[u8; 32]>,
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
            node_seq,
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
            epoch: new_operation.epoch,
            environment: new_operation.environment,
            prev_node_hash,
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
        // node_seq is the 1-based dense chain position; 0 is structurally invalid.
        if self.body.node_seq == 0 {
            return Err(SyncLedgerError::InvalidNodeSeq {
                node: self.body.actor_node_id.clone(),
            });
        }
        // The HLC origin must be the authoring node: a forged hlc.node_id would
        // let an attacker steer last-writer-wins resolution without touching the
        // signed chain. The hash above already binds hlc into the op identity, so
        // this rejects the forgery before it can ever influence materialization.
        if self.body.hlc_timestamp.node_id != self.body.actor_node_id {
            return Err(SyncLedgerError::HlcNodeMismatch {
                actor: self.body.actor_node_id.clone(),
                hlc: self.body.hlc_timestamp.node_id.clone(),
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
    /// Verifies a redacted chain placeholder. We cannot recompute `operation_hash`
    /// (the body is absent), so we trust the carried hash ONLY because the
    /// signature is over it: a forged hash would need the author's key to produce
    /// a matching signature. The caller still links `prev_node_hash` to the chain.
    fn verify_redacted_signature(&self, record: &RedactedRecord) -> LedgerResult<()>;
}

/// Head of a single node's hash chain. Keyed by `node_id`; a node is the only
/// writer of its own chain, so this advances monotonically and never forks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeHead {
    pub node_id: String,
    pub last_seq: u64,
    pub last_hash: [u8; 32],
}

/// One position on a node's chain as the local store holds it: either the full
/// operation (we are a sync target for the resource) or a redacted placeholder
/// (we are not). Pull-serving walks the chain in this form so a relay can pass on
/// a position it itself only holds redacted, without forging a body.
#[derive(Debug, Clone)]
pub enum NodeChainEntry {
    Full(SyncOperation),
    Redacted(RedactedRecord),
}

impl NodeChainEntry {
    pub fn node_seq(&self) -> u64 {
        match self {
            NodeChainEntry::Full(op) => op.body.node_seq,
            NodeChainEntry::Redacted(record) => record.node_seq,
        }
    }

    pub fn op_id(&self) -> OperationId {
        match self {
            NodeChainEntry::Full(op) => op.op_id,
            NodeChainEntry::Redacted(record) => record.op_id,
        }
    }
}

/// A chain position the local node knows ONLY as a verified placeholder: the
/// authoring node minted it, the signature checks out over `operation_hash`, and
/// it links the per-node chain via `prev_node_hash` — but the body was withheld
/// because the local node is not a sync target for the underlying resource. It
/// lets the receiver advance its node-frontier and detect equivocation at this
/// `(actor_node_id, node_seq)` without ever materializing the resource. If the
/// receiver later gains permission, the full op (same `op_id`) replaces this
/// record and is materialized — never a fork, always a completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactedRecord {
    pub op_id: OperationId,
    pub operation_hash: [u8; 32],
    pub actor_node_id: String,
    pub node_seq: u64,
    pub prev_node_hash: Option<[u8; 32]>,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppendResult {
    pub op_id: OperationId,
    pub operation_hash: [u8; 32],
    pub previous_node_hash: Option<[u8; 32]>,
    pub node_seq: u64,
}

/// Reads every operation routed to one partition (materialization index). The
/// partition no longer carries a global sequence, so the caller orders the
/// result by HLC. Snapshot/pull use `NodeLogQuery` for per-node chain reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationQuery {
    pub partition_id: PartitionId,
    pub limit: Option<usize>,
}

/// Reads a contiguous slice of one node's chain ordered by `node_seq`. This is
/// the dense, monotonic axis pull/repair/snapshot-tail validate against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeLogQuery {
    pub node_id: String,
    pub from_node_seq: Option<u64>,
    pub to_node_seq: Option<u64>,
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
    /// Number of times this entry was deferred for ordering reasons (target row
    /// of an UPDATE not yet created). Bounded so a genuinely orphaned UPDATE is
    /// eventually surfaced as a conflict instead of being retried forever.
    #[serde(default)]
    pub deferred_count: u32,
}

/// What the local node has observed of one remote node's chain: the highest
/// contiguous `node_seq` accepted and that operation's hash. Replaces the old
/// per-(peer, partition) cursor — the node-frontier is keyed by the authoring
/// node, not by partition, because admission is now per-node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeFrontierEntry {
    pub node_id: String,
    pub last_seq: u64,
    pub last_hash: [u8; 32],
}

/// A pending catch-up pull for a gap in one node's chain. `peer` is the mesh
/// peer we ask; `target_node_id` is the authoring node whose chain we need from
/// `from_node_seq` onward (peer and target may differ — any peer can relay
/// another node's chain).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairQueueEntry {
    pub peer: PeerId,
    pub target_node_id: String,
    pub from_node_seq: u64,
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
    /// Per-node coverage of this snapshot: `node_id -> (last_node_seq, last_hash)`
    /// over every authoring node whose chain the snapshot prefix includes. This is
    /// the AUTHORITATIVE coverage axis in the per-node model (`up_to_sequence` is
    /// only a 1-based count watermark + storage key). A node that adopts this
    /// snapshot sets its node-frontier to exactly this map, then pulls each
    /// writer's chain forward from `last_node_seq` — so the catch-up tail is bounded
    /// per writer with no fork. It is bound into the snapshot signature, so the
    /// frontier a receiver commits to is attested by the author, not merely
    /// reconstructed from the blob.
    #[serde(default)]
    pub node_frontier: std::collections::BTreeMap<String, (u64, [u8; 32])>,
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
    /// Every operation routed to a partition (materialization index), unordered
    /// across node chains. Callers order by HLC.
    fn get_operations(&self, query: OperationQuery) -> LedgerResult<Vec<SyncOperation>>;
    /// Every full operation body the store currently holds (the content keyspace),
    /// independent of partition. Used by the authority-side permission backfill to
    /// re-evaluate outbox targets for ops minted BEFORE a grant: a redacted op
    /// carries no partition/resource, so a freshly-permitted receiver cannot ask
    /// for the right partition — the authority must reverse the gate and re-enqueue.
    fn list_all_operations(&self) -> LedgerResult<Vec<SyncOperation>>;
    /// A contiguous slice of one node's chain ordered by `node_seq`. The dense
    /// axis pull/repair/snapshot-tail validate against.
    fn get_node_operations(&self, query: NodeLogQuery) -> LedgerResult<Vec<SyncOperation>>;
    /// Like `get_node_operations`, but each chain position is returned in the form
    /// the store holds it: a full operation OR a redacted placeholder. Pull-serving
    /// uses this so a relay can forward a position it itself holds only redacted,
    /// preserving chain density instead of leaving a gap the requester loops on.
    fn get_node_chain_entries(&self, query: NodeLogQuery) -> LedgerResult<Vec<NodeChainEntry>>;
    fn get_operation(&self, op_id: OperationId) -> LedgerResult<SyncOperation>;
    /// The `op_id` we actually recorded at `(node_id, node_seq)` on the per-node
    /// chain axis, or `None` if that position was compacted away or never seen.
    /// Equivocation detection compares this against the incoming op's `op_id`:
    /// because ops are content-addressed, an op forged at an already-known seq has
    /// a DIFFERENT op_id, which a by-op_id lookup would miss entirely.
    fn get_node_log_entry(&self, node_id: &str, node_seq: u64)
        -> LedgerResult<Option<OperationId>>;
    /// The lowest `node_seq` for `node_id` whose operation row is still present
    /// (not compacted away) AND carries the current baseline epoch, i.e. the
    /// earliest position this node can RELAY to a peer — an op kept under an
    /// abandoned epoch is rejected by every peer's admission fence, so it is not
    /// servable. `None` if we hold nothing live for that chain. A pull asking
    /// below this floor cannot be served from the log: it escalates to a snapshot
    /// or, when no snapshot covers the prefix, is served from the floor so the
    /// requester can anchor there.
    fn earliest_live_node_seq(&self, node_id: &str) -> LedgerResult<Option<u64>>;
    fn put_in_outbox(&self, target: SyncTarget, op_id: OperationId) -> LedgerResult<()>;
    fn get_outbox_entry(&self, target: SyncTarget, op_id: OperationId)
        -> LedgerResult<OutboxEntry>;
    fn list_pending_outbox(
        &self,
        target: SyncTarget,
        limit: usize,
    ) -> LedgerResult<Vec<OutboxEntry>>;
    fn list_outbox_for_operation(&self, op_id: OperationId) -> LedgerResult<Vec<OutboxEntry>>;
    fn put_verified_in_inbox(
        &self,
        source: PeerId,
        operation: SyncOperation,
        verifier: &dyn SyncOperationVerifier,
    ) -> LedgerResult<()>;
    /// Atomically verifies + stores an accepted op in the inbox AND advances the
    /// author's node-frontier in ONE durable batch. The two MUST be atomic: if a
    /// crash advanced the frontier without persisting the inbox entry, that seq
    /// would be skipped forever (the op is neither pulled again — frontier is past
    /// it — nor applied — it never reached the inbox), silently losing data.
    fn admit_verified_operation(
        &self,
        source: PeerId,
        operation: SyncOperation,
        frontier: NodeFrontierEntry,
        verifier: &dyn SyncOperationVerifier,
    ) -> LedgerResult<()>;
    /// Atomically verifies a redacted chain placeholder, records it on the per-node
    /// axis (`node_log` + a `redacted_log` row) and advances the author's
    /// node-frontier — all in ONE durable batch, for the same crash-safety reason
    /// as `admit_verified_operation`. It deliberately writes NOTHING to the inbox
    /// or the partition view: a redacted op carries no body, so there is nothing to
    /// materialize or relay as full. `get_node_log_entry` still resolves this
    /// position, so equivocation detection works against it.
    fn admit_redacted_operation(
        &self,
        record: RedactedRecord,
        frontier: NodeFrontierEntry,
        verifier: &dyn SyncOperationVerifier,
    ) -> LedgerResult<()>;
    /// The redacted placeholder stored at `op_id`, if the local node holds this
    /// position only in redacted form (no full body). `None` once the full op
    /// lands (it is removed) or if the position was never seen as redacted.
    fn get_redacted_record(&self, op_id: OperationId) -> LedgerResult<Option<RedactedRecord>>;
    fn get_inbox_entry(&self, source: PeerId, op_id: OperationId) -> LedgerResult<InboxEntry>;
    fn list_unapplied_inbox(&self, limit: usize) -> LedgerResult<Vec<InboxEntry>>;
    fn mark_inbox_applied(&self, source: PeerId, op_id: OperationId) -> LedgerResult<()>;
    fn mark_inbox_conflicted(
        &self,
        source: PeerId,
        op_id: OperationId,
        message: String,
    ) -> LedgerResult<()>;
    /// Marks an entry as deferred (retryable) for ordering reasons and returns the
    /// new deferral count, so the caller can give up after a bounded number of
    /// fruitless retries. The entry stays in `list_unapplied_inbox`.
    fn mark_inbox_deferred(
        &self,
        source: PeerId,
        op_id: OperationId,
        message: String,
    ) -> LedgerResult<u32>;
    fn mark_delivered(&self, target: SyncTarget, op_id: OperationId) -> LedgerResult<()>;
    fn mark_acknowledged(&self, target: SyncTarget, op_id: OperationId) -> LedgerResult<()>;
    /// Removes a single outbox entry keyed by `(target, op_id)`. Used to lazily
    /// reap orphaned entries whose backing operation has been compacted away;
    /// removing an absent key is a no-op.
    fn remove_outbox_entry(&self, target: SyncTarget, op_id: OperationId) -> LedgerResult<()>;
    /// What the local node has observed of `node_id`'s chain (last contiguous
    /// `node_seq` + hash). `None` until the first operation from that node lands.
    fn get_node_frontier(&self, node_id: &str) -> LedgerResult<Option<NodeFrontierEntry>>;
    fn save_node_frontier(&self, frontier: NodeFrontierEntry) -> LedgerResult<()>;
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
        target_node_id: &str,
        next_attempt_ms: i64,
        retry_count: u32,
    ) -> LedgerResult<()>;
    fn remove_repair_request(&self, peer: PeerId, target_node_id: &str) -> LedgerResult<()>;
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
    /// Head of `node_id`'s LOCAL chain (only the local node ever appends to its
    /// own head). Returns `None` before the node mints its first operation.
    fn get_node_head(&self, node_id: &str) -> LedgerResult<Option<NodeHead>>;
    fn list_outbox_for_partition(
        &self,
        partition: PartitionId,
        up_to_sequence: u64,
    ) -> LedgerResult<Vec<OutboxEntry>>;
    fn compact(&self, policy: CompactionPolicy) -> LedgerResult<()>;
    /// Returns the locally-active baseline epoch. Every operation minted or
    /// accepted into the inbox must carry exactly this epoch; mismatches are
    /// rejected so a node never mixes pre/post-baseline-reset operations.
    fn current_epoch(&self) -> LedgerResult<BaselineEpoch>;
    /// Persists `epoch` as the locally-active baseline epoch. Called during a
    /// baseline reset (phase B/C cutover) to advance past every prior operation.
    fn set_epoch(&self, epoch: BaselineEpoch) -> LedgerResult<()>;
    /// Returns the locally-declared node environment (Dev/Test/Prod, ROADMAP
    /// Z12), stamped on every operation minted here and checked against every
    /// operation admitted into the inbox — independent of, and enforced
    /// alongside, `current_epoch`. Defaults to `Prod` (the conservative
    /// choice) before the node has ever declared an environment.
    fn current_environment(&self) -> LedgerResult<NodeEnvironment>;
    /// Persists `environment` as the locally-declared node environment.
    /// Called by `SetKind` (after server-side confirmation for a Prod
    /// target) together with a core baseline wipe+reseed
    /// (`reset_core_partitions` + `bump_epoch`) — a node's environment
    /// identity change fences it from every sync partner still on its old
    /// core partitions.
    fn set_environment(&self, environment: NodeEnvironment) -> LedgerResult<()>;
    /// Returns the last persisted HLC state, used to resume the local clock
    /// after a restart so monotonicity survives across process boundaries.
    fn current_hlc(&self) -> LedgerResult<Option<HybridLogicalTimestamp>>;
    /// Persists the latest HLC state emitted/observed by the local clock.
    fn save_hlc(&self, timestamp: &HybridLogicalTimestamp) -> LedgerResult<()>;
    /// Advances the local epoch counter, stamping `origin_node` as the minter,
    /// and returns the new epoch. Used by the local node when it performs a
    /// core baseline reset and re-seeds operations under a fresh epoch.
    fn bump_epoch(&self, origin_node: &str) -> LedgerResult<BaselineEpoch> {
        let current = self.current_epoch()?;
        let next = BaselineEpoch {
            counter: current.counter.saturating_add(1),
            origin_node: origin_node.to_string(),
        };
        self.set_epoch(next.clone())?;
        Ok(next)
    }
    /// Wipes all ledger state for partitions whose `partition_id` starts with
    /// `partition_prefix`. Phase B uses this to rebuild core data from a fresh
    /// baseline without touching addon/kv data.
    fn reset_partitions_with_prefix(&self, partition_prefix: &str) -> LedgerResult<()>;
    /// Convenience wrapper that resets every core partition (`core/...`).
    fn reset_core_partitions(&self) -> LedgerResult<()> {
        self.reset_partitions_with_prefix(CORE_PARTITION_PREFIX)
    }
}

/// Prefix shared by every core-owned partition_id (`core/org/<org>/<suffix>`).
pub const CORE_PARTITION_PREFIX: &str = "core/";

/// Canonical leaf order for operations within a partition. This is the single
/// deterministic total order every structural reader (Merkle leaves, snapshot
/// prefix/tail build, compaction) must agree on so two honest nodes that hold the
/// same operation set compute the SAME root_hash / state_hash.
///
/// It deliberately does NOT use the HLC: HLC carries a wall-clock component, so
/// under clock skew two nodes can disagree on the relative HLC order of ops from
/// different authors, which would make the Merkle root un-reconcilable. Instead
/// the order is `(actor_node_id, node_seq)` — each per-node chain is dense and
/// strictly monotonic in `node_seq`, so this is a stable total order regardless
/// of clock skew. `operation_hash` is a final tie-break that can only matter if
/// the input set is already Byzantine (equivocation), which the chain validators
/// reject upstream. The HLC is used ONLY for last-writer-wins value resolution in
/// the materializer (`incoming_hlc_wins`), never for leaf ordering.
pub fn partition_materialization_order(a: &SyncOperation, b: &SyncOperation) -> std::cmp::Ordering {
    a.body
        .actor_node_id
        .cmp(&b.body.actor_node_id)
        .then_with(|| a.body.node_seq.cmp(&b.body.node_seq))
        .then_with(|| a.operation_hash.cmp(&b.operation_hash))
}

pub(crate) fn encode<T: Serialize>(value: &T) -> LedgerResult<Vec<u8>> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(value, &mut bytes)
        .map_err(|e| SyncLedgerError::Codec(e.to_string()))?;
    Ok(bytes)
}

pub(crate) fn decode<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> LedgerResult<T> {
    ciborium::de::from_reader(std::io::Cursor::new(bytes))
        .map_err(|e| SyncLedgerError::Decode(e.to_string()))
}

pub(crate) fn hash_canonical<T: Serialize>(value: &T) -> LedgerResult<[u8; 32]> {
    Ok(*blake3::hash(&encode(value)?).as_bytes())
}

pub(crate) fn signing_bytes_for_hash(operation_hash: [u8; 32]) -> Vec<u8> {
    let mut bytes = b"tentaflow-sync-operation-v1".to_vec();
    bytes.extend_from_slice(&operation_hash);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    fn hlc(wall: i64, logical: u32, node: &str) -> HybridLogicalTimestamp {
        HybridLogicalTimestamp {
            wall_time_ms: wall,
            logical,
            node_id: node.to_string(),
        }
    }

    #[test]
    fn newer_wall_time_is_greater() {
        let older = hlc(100, 9, "node_z");
        let newer = hlc(200, 0, "node_a");
        assert!(newer > older);
        assert!(older < newer);
    }

    #[test]
    fn equal_wall_breaks_on_logical() {
        let lower = hlc(100, 1, "node_z");
        let higher = hlc(100, 2, "node_a");
        assert!(higher > lower);
    }

    #[test]
    fn equal_wall_and_logical_breaks_on_node_id() {
        let a = hlc(100, 5, "node_a");
        let b = hlc(100, 5, "node_b");
        assert!(b > a);
        assert_eq!(a.cmp(&b), Ordering::Less);
    }

    #[test]
    fn operation_body_with_epoch_round_trips_cbor() {
        let body = SyncOperationBody {
            org_id: "org_1".to_string(),
            partition_id: PartitionId::new("core/org/org_1/flows").unwrap(),
            node_seq: 7,
            addon_id: "core".to_string(),
            resource_type: "core.flow".to_string(),
            resource_id: "flow-uuid".to_string(),
            table_name: "flows".to_string(),
            primary_key: "id".to_string(),
            action: ActionType::Insert,
            changed_fields: BTreeMap::new(),
            before_hash: None,
            after_hash: Some([3; 32]),
            actor_user_id: "user-uuid".to_string(),
            actor_device_id: "node_a".to_string(),
            actor_node_id: "node_a".to_string(),
            hlc_timestamp: hlc(100, 1, "node_a"),
            epoch: BaselineEpoch {
                counter: 5,
                origin_node: "node_a".to_string(),
            },
            environment: NodeEnvironment::Test,
            prev_node_hash: Some([9; 32]),
            payload_hash: [1; 32],
            acl_snapshot_hash: [2; 32],
            policy_epoch: 2,
            encryption_info: None,
        };

        let bytes = encode(&body).expect("encode");
        let decoded: SyncOperationBody = decode(&bytes).expect("decode");

        assert_eq!(decoded.epoch.counter, 5);
        assert_eq!(decoded.epoch.origin_node, "node_a");
        assert_eq!(decoded, body);
    }

    #[test]
    fn pre_epoch_operation_body_decodes_to_genesis_epoch() {
        // Mirrors SyncOperationBody as it was serialized before the epoch field
        // existed (faza B). Encoding it produces a CBOR map with no `epoch` key,
        // exactly like an operation read from an old on-disk ledger or received
        // from an un-upgraded peer. Decoding MUST yield genesis epoch (so epoch
        // fencing can reject it cleanly) instead of a hard "missing field" error.
        #[derive(Serialize)]
        struct LegacyOperationBody {
            org_id: String,
            partition_id: PartitionId,
            node_seq: u64,
            addon_id: String,
            resource_type: String,
            resource_id: String,
            table_name: String,
            primary_key: String,
            action: ActionType,
            changed_fields: BTreeMap<String, FieldValue>,
            before_hash: Option<[u8; 32]>,
            after_hash: Option<[u8; 32]>,
            actor_user_id: String,
            actor_device_id: String,
            actor_node_id: String,
            hlc_timestamp: HybridLogicalTimestamp,
            prev_node_hash: Option<[u8; 32]>,
            payload_hash: [u8; 32],
            acl_snapshot_hash: [u8; 32],
            policy_epoch: u64,
            encryption_info: Option<String>,
        }

        let legacy = LegacyOperationBody {
            org_id: "org_1".to_string(),
            partition_id: PartitionId::new("core/org/org_1/flows").unwrap(),
            node_seq: 7,
            addon_id: "core".to_string(),
            resource_type: "core.flow".to_string(),
            resource_id: "flow-uuid".to_string(),
            table_name: "flows".to_string(),
            primary_key: "id".to_string(),
            action: ActionType::Insert,
            changed_fields: BTreeMap::new(),
            before_hash: None,
            after_hash: Some([3; 32]),
            actor_user_id: "user-uuid".to_string(),
            actor_device_id: "node_a".to_string(),
            actor_node_id: "node_a".to_string(),
            hlc_timestamp: hlc(100, 1, "node_a"),
            prev_node_hash: Some([9; 32]),
            payload_hash: [1; 32],
            acl_snapshot_hash: [2; 32],
            policy_epoch: 2,
            encryption_info: None,
        };

        let bytes = encode(&legacy).expect("encode legacy");
        let decoded: SyncOperationBody = decode(&bytes).expect("decode legacy into current");

        assert_eq!(decoded.epoch, BaselineEpoch::default());
        assert_eq!(decoded.epoch.counter, 0);
        assert!(decoded.epoch.origin_node.is_empty());
        // Same append-only pattern for `environment` (ROADMAP Z12) — a
        // pre-Z12 operation decodes as `Prod`, the conservative default. On
        // an existing all-Prod mesh this MATCHES the local node's own
        // environment, so admission's environment fence does not reject it
        // (P1-2's whole point: upgrading an existing Prod-Prod deployment
        // must not break sync). This is decode-only, not proof that a
        // byte-identical legacy operation replays cleanly through admission
        // end-to-end — the operation hash used to exclude `environment` from
        // its input and now includes it, so a REAL legacy blob would fail
        // signature/hash verification (`InvalidOperationHash`) before the
        // environment/epoch fence is ever reached; that failure mode is
        // unrelated to what this test exercises (`SyncOperationBody`'s serde
        // defaulting alone).
        assert_eq!(decoded.environment, NodeEnvironment::default());
        assert_eq!(decoded.environment, NodeEnvironment::Prod);
    }

    #[test]
    fn ordering_is_antisymmetric_and_reflexive() {
        let a = hlc(100, 5, "node_a");
        let b = hlc(100, 5, "node_b");
        assert_eq!(a.cmp(&b), b.cmp(&a).reverse());
        assert_eq!(a.cmp(&a), Ordering::Equal);
    }
}

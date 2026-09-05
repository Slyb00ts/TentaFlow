// =============================================================================
// Plik: sync/snapshot.rs
// Opis: Snapshot Manager buduje podpisane checkpointy partycji Sync Ledger.
// =============================================================================

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::sync::ledger::{
    build_merkle_summary, hash_canonical, node_frontier_for_operations,
    partition_materialization_order, validate_hash_chain, validate_hash_chain_anchored,
    HexNodeIdOperationVerifier, LedgerResult, OperationQuery, PartitionId, SnapshotId,
    SyncLedgerError, SyncLedgerStore, SyncOperation, SyncOperationSigner, SyncOperationVerifier,
    SyncSnapshot,
};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use std::collections::BTreeMap;

const SNAPSHOT_SIGNATURE_DOMAIN: &[u8] = b"tentaflow-sync-snapshot-v1";
const SNAPSHOT_STATE_DOMAIN: &[u8] = b"tentaflow-sync-snapshot-state-v1";
const SNAPSHOT_SQL_BLOB_DOMAIN: &[u8] = b"tentaflow-sync-snapshot-sql-blob-v1";
const SNAPSHOT_SQL_BLOB_KIND: &str = "sql_replay_v1";

pub struct SnapshotManager<'a> {
    ledger: &'a dyn SyncLedgerStore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotBuildRequest {
    pub partition_id: PartitionId,
    pub up_to_sequence: Option<u64>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotBuildResult {
    pub snapshot: SyncSnapshot,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotSqlPackageBuildResult {
    pub snapshot: SyncSnapshot,
    pub blob: SnapshotSqlBlobPackage,
    pub blob_bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotSqlPackagePersistResult {
    pub snapshot: SyncSnapshot,
    pub blob_path: PathBuf,
    pub blob_size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRestoreRequest {
    pub partition_id: PartitionId,
    pub up_to_sequence: u64,
    pub snapshot_id: SnapshotId,
    pub operation_limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotRestorePlan {
    pub snapshot: SyncSnapshot,
    pub operations_after_snapshot: Vec<SyncOperation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotSqlRestoreRequest {
    pub partition_id: PartitionId,
    pub up_to_sequence: u64,
    pub snapshot_id: SnapshotId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotSqlPackageRestoreRequest {
    pub partition_id: PartitionId,
    pub up_to_sequence: u64,
    pub snapshot_id: SnapshotId,
    pub blob_bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotSqlRestoreResult {
    pub snapshot: SyncSnapshot,
    pub operations_applied: usize,
    pub sqlite_rows_changed: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotSqlBlobPackage {
    pub version: u32,
    pub snapshot_id: SnapshotId,
    pub partition_id: PartitionId,
    pub from_sequence: u64,
    pub up_to_sequence: u64,
    pub operation_count: u64,
    pub root_hash: [u8; 32],
    pub state_hash: [u8; 32],
    #[serde(default)]
    pub node_frontier: BTreeMap<String, (u64, [u8; 32])>,
    pub last_operation_hash: Option<[u8; 32]>,
    pub policy_epoch: u64,
    pub operations: Vec<SyncOperation>,
}

#[derive(Debug, Clone)]
pub struct SnapshotPackageStore {
    root: PathBuf,
}

#[derive(Debug, Serialize)]
struct SnapshotSignaturePayload<'a> {
    snapshot_id: &'a str,
    partition_id: &'a str,
    from_sequence: u64,
    up_to_sequence: u64,
    operation_count: u64,
    root_hash: [u8; 32],
    state_hash: [u8; 32],
    node_frontier: &'a BTreeMap<String, (u64, [u8; 32])>,
    last_operation_hash: Option<[u8; 32]>,
    policy_epoch: u64,
    blob_kind: Option<&'a str>,
    blob_hash: Option<[u8; 32]>,
    blob_size_bytes: u64,
    created_at_ms: i64,
    author_node_id: &'a str,
}

#[derive(Debug, Clone, Copy)]
struct SnapshotBlobMetadata<'a> {
    kind: Option<&'a str>,
    hash: Option<[u8; 32]>,
    size_bytes: u64,
}

#[derive(Debug, Serialize)]
struct SnapshotStatePayload<'a> {
    partition_id: &'a str,
    from_sequence: u64,
    up_to_sequence: u64,
    operation_ids: Vec<[u8; 32]>,
    operation_hashes: Vec<[u8; 32]>,
}

impl<'a> SnapshotManager<'a> {
    pub fn new(ledger: &'a dyn SyncLedgerStore) -> Self {
        Self { ledger }
    }

    /// Returns the partition's operations in HLC order, truncated to the first
    /// `up_to_count` of them (all of them when `None`). The snapshot watermark is
    /// a 1-based count over this HLC-ordered set — there is no global partition
    /// sequence anymore, so HLC is the canonical materialization order.
    fn partition_prefix_operations(
        &self,
        partition_id: &PartitionId,
        up_to_count: Option<u64>,
    ) -> LedgerResult<Vec<SyncOperation>> {
        let mut operations = self.ledger.get_operations(OperationQuery {
            partition_id: partition_id.clone(),
            limit: None,
        })?;
        operations.sort_by(|a, b| partition_materialization_order(a, b));
        if let Some(count) = up_to_count {
            operations.truncate(count as usize);
        }
        Ok(operations)
    }

    pub fn build_and_store(
        &self,
        request: SnapshotBuildRequest,
        signer: &dyn SyncOperationSigner,
    ) -> LedgerResult<Option<SnapshotBuildResult>> {
        let operations =
            self.partition_prefix_operations(&request.partition_id, request.up_to_sequence)?;
        let up_to_sequence = operations.len() as u64;
        if up_to_sequence == 0 {
            return Ok(None);
        }
        validate_hash_chain(&operations)?;

        let summary = build_merkle_summary(&operations)?;
        if summary.from_sequence != 1 || summary.to_sequence != up_to_sequence {
            return Err(SyncLedgerError::MerkleSequenceGap {
                expected: up_to_sequence,
                actual: summary.to_sequence,
            });
        }

        let last_operation_hash = operations.last().map(|operation| operation.operation_hash);
        let policy_epoch = operations
            .last()
            .map(|operation| operation.body.policy_epoch)
            .unwrap_or_default();
        let state_hash = state_hash(&request.partition_id, &operations)?;
        let node_frontier = node_frontier_for_operations(&operations);
        let snapshot_id = snapshot_id(&request.partition_id, up_to_sequence, summary.root_hash)?;
        let signature_payload = SnapshotSignaturePayload {
            snapshot_id: snapshot_id.as_str(),
            partition_id: request.partition_id.as_str(),
            from_sequence: summary.from_sequence,
            up_to_sequence,
            operation_count: summary.operation_count,
            root_hash: summary.root_hash,
            state_hash,
            node_frontier: &node_frontier,
            last_operation_hash,
            policy_epoch,
            blob_kind: None,
            blob_hash: None,
            blob_size_bytes: 0,
            created_at_ms: request.created_at_ms,
            author_node_id: signer.node_id(),
        };
        let signature = signer.sign_operation(&snapshot_signing_bytes(&signature_payload)?)?;
        let snapshot = SyncSnapshot {
            snapshot_id,
            partition_id: request.partition_id,
            from_sequence: summary.from_sequence,
            up_to_sequence,
            operation_count: summary.operation_count,
            root_hash: summary.root_hash,
            state_hash,
            node_frontier,
            last_operation_hash,
            policy_epoch,
            blob_kind: None,
            blob_hash: None,
            blob_size_bytes: 0,
            created_at_ms: request.created_at_ms,
            author_node_id: signer.node_id().to_string(),
            signature,
        };
        self.ledger.save_snapshot(snapshot.clone())?;
        Ok(Some(SnapshotBuildResult { snapshot }))
    }

    pub fn build_sql_package_and_store(
        &self,
        request: SnapshotBuildRequest,
        signer: &dyn SyncOperationSigner,
    ) -> LedgerResult<Option<SnapshotSqlPackageBuildResult>> {
        let operations =
            self.partition_prefix_operations(&request.partition_id, request.up_to_sequence)?;
        let up_to_sequence = operations.len() as u64;
        if up_to_sequence == 0 {
            return Ok(None);
        }
        validate_hash_chain(&operations)?;
        validate_operation_signatures(&operations)?;
        for operation in &operations {
            crate::sync::runtime::capture_from_operation(operation)?;
        }

        let summary = build_merkle_summary(&operations)?;
        if summary.from_sequence != 1 || summary.to_sequence != up_to_sequence {
            return Err(SyncLedgerError::MerkleSequenceGap {
                expected: up_to_sequence,
                actual: summary.to_sequence,
            });
        }

        let last_operation_hash = operations.last().map(|operation| operation.operation_hash);
        let policy_epoch = operations
            .last()
            .map(|operation| operation.body.policy_epoch)
            .unwrap_or_default();
        let state_hash = state_hash(&request.partition_id, &operations)?;
        let node_frontier = node_frontier_for_operations(&operations);
        let snapshot_id = snapshot_id(&request.partition_id, up_to_sequence, summary.root_hash)?;
        let blob = SnapshotSqlBlobPackage {
            version: 1,
            snapshot_id: snapshot_id.clone(),
            partition_id: request.partition_id.clone(),
            from_sequence: summary.from_sequence,
            up_to_sequence,
            operation_count: summary.operation_count,
            root_hash: summary.root_hash,
            state_hash,
            node_frontier: node_frontier.clone(),
            last_operation_hash,
            policy_epoch,
            operations,
        };
        let blob_bytes = encode_snapshot_sql_blob(&blob)?;
        let blob_hash = snapshot_blob_hash(&blob_bytes);
        let snapshot = self.build_snapshot_from_parts(
            request.partition_id,
            request.created_at_ms,
            signer,
            snapshot_id,
            summary.from_sequence,
            up_to_sequence,
            summary.operation_count,
            summary.root_hash,
            state_hash,
            node_frontier,
            last_operation_hash,
            policy_epoch,
            SnapshotBlobMetadata {
                kind: Some(SNAPSHOT_SQL_BLOB_KIND),
                hash: Some(blob_hash),
                size_bytes: blob_bytes.len() as u64,
            },
        )?;
        self.ledger.save_snapshot(snapshot.clone())?;
        Ok(Some(SnapshotSqlPackageBuildResult {
            snapshot,
            blob,
            blob_bytes,
        }))
    }

    pub fn build_sql_package_and_persist(
        &self,
        request: SnapshotBuildRequest,
        signer: &dyn SyncOperationSigner,
        store: &SnapshotPackageStore,
    ) -> LedgerResult<Option<SnapshotSqlPackagePersistResult>> {
        let Some(result) = self.build_sql_package_and_store(request, signer)? else {
            return Ok(None);
        };
        let blob_path = store.put_sql_package(&result.snapshot, &result.blob_bytes)?;
        Ok(Some(SnapshotSqlPackagePersistResult {
            snapshot: result.snapshot,
            blob_path,
            blob_size_bytes: result.blob_bytes.len() as u64,
        }))
    }

    pub fn build_restore_plan(
        &self,
        request: SnapshotRestoreRequest,
    ) -> LedgerResult<SnapshotRestorePlan> {
        let snapshot = self.ledger.get_snapshot(
            request.partition_id.clone(),
            request.up_to_sequence,
            request.snapshot_id,
        )?;
        verify_snapshot_signature(&snapshot)?;
        if snapshot.partition_id != request.partition_id {
            return Err(SyncLedgerError::MerklePartitionMismatch {
                expected: request.partition_id.as_str().to_string(),
                actual: snapshot.partition_id.as_str().to_string(),
            });
        }
        if snapshot.up_to_sequence != request.up_to_sequence {
            return Err(SyncLedgerError::MerkleSequenceGap {
                expected: request.up_to_sequence,
                actual: snapshot.up_to_sequence,
            });
        }

        // The snapshot prefix is the first `up_to_sequence` partition ops in
        // canonical order; the tail is every remaining op. Reconstruct the prefix
        // op_ids from the live ledger and treat everything not in that set as the
        // tail. This stays correct after compaction (prefix ops are gone, so the
        // whole live partition is tail). Each node's chain in the tail is checked.
        let prefix =
            self.partition_prefix_operations(&request.partition_id, Some(snapshot.up_to_sequence))?;
        let covered: std::collections::HashSet<crate::sync::ledger::OperationId> =
            prefix.iter().map(|op| op.op_id).collect();
        let mut operations_after_snapshot =
            self.partition_tail_after(&request.partition_id, &covered)?;
        if let Some(limit) = request.operation_limit {
            operations_after_snapshot.truncate(limit);
        }
        if !operations_after_snapshot.is_empty() {
            validate_hash_chain(&operations_after_snapshot)?;
            validate_operation_signatures(&operations_after_snapshot)?;
        }

        Ok(SnapshotRestorePlan {
            snapshot,
            operations_after_snapshot,
        })
    }

    /// Partition operations not in `covered`, in canonical materialization order.
    /// The complement of a snapshot prefix — the snapshot tail.
    fn partition_tail_after(
        &self,
        partition_id: &PartitionId,
        covered: &std::collections::HashSet<crate::sync::ledger::OperationId>,
    ) -> LedgerResult<Vec<SyncOperation>> {
        let mut tail: Vec<SyncOperation> = self
            .partition_prefix_operations(partition_id, None)?
            .into_iter()
            .filter(|op| !covered.contains(&op.op_id))
            .collect();
        tail.sort_by(partition_materialization_order);
        Ok(tail)
    }

    pub fn restore_sql_from_snapshot(
        &self,
        request: SnapshotSqlRestoreRequest,
    ) -> LedgerResult<SnapshotSqlRestoreResult> {
        let plan = self.build_restore_plan(SnapshotRestoreRequest {
            partition_id: request.partition_id,
            up_to_sequence: request.up_to_sequence,
            snapshot_id: request.snapshot_id,
            operation_limit: None,
        })?;
        let mut operations = self.validated_snapshot_prefix(&plan.snapshot)?;
        operations.extend(plan.operations_after_snapshot.iter().cloned());

        let mut sqlite_rows_changed = 0u64;
        for operation in &operations {
            let capture = crate::sync::runtime::capture_from_operation(operation)?;
            sqlite_rows_changed +=
                crate::addon::storage_sql_exec::apply_replicated_write(&capture, operation.op_id)
                    .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        }

        Ok(SnapshotSqlRestoreResult {
            snapshot: plan.snapshot,
            operations_applied: operations.len(),
            sqlite_rows_changed,
        })
    }

    pub fn restore_sql_from_package(
        &self,
        request: SnapshotSqlPackageRestoreRequest,
    ) -> LedgerResult<SnapshotSqlRestoreResult> {
        let snapshot = self.ledger.get_snapshot(
            request.partition_id.clone(),
            request.up_to_sequence,
            request.snapshot_id.clone(),
        )?;
        verify_snapshot_signature(&snapshot)?;
        validate_snapshot_blob_bytes(&snapshot, &request.blob_bytes)?;
        let blob = decode_snapshot_sql_blob(&request.blob_bytes)?;
        validate_snapshot_sql_blob(&snapshot, &blob, &request.blob_bytes)?;

        // The blob op_ids are the authoritative covered set; the tail is the live
        // partition ops not in it (correct even after the prefix is compacted).
        let covered: std::collections::HashSet<crate::sync::ledger::OperationId> =
            blob.operations.iter().map(|op| op.op_id).collect();
        let operations_after_snapshot =
            self.partition_tail_after(&request.partition_id, &covered)?;
        if !operations_after_snapshot.is_empty() {
            validate_hash_chain(&operations_after_snapshot)?;
            validate_operation_signatures(&operations_after_snapshot)?;
        }

        let mut sqlite_rows_changed = 0u64;
        for operation in blob
            .operations
            .iter()
            .chain(operations_after_snapshot.iter())
        {
            let capture = crate::sync::runtime::capture_from_operation(operation)?;
            sqlite_rows_changed +=
                crate::addon::storage_sql_exec::apply_replicated_write(&capture, operation.op_id)
                    .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        }

        Ok(SnapshotSqlRestoreResult {
            operations_applied: blob.operations.len() + operations_after_snapshot.len(),
            snapshot,
            sqlite_rows_changed,
        })
    }

    pub fn restore_sql_from_package_parts(
        &self,
        snapshot: &SyncSnapshot,
        blob_bytes: &[u8],
        operations_after_snapshot: &[SyncOperation],
    ) -> LedgerResult<SnapshotSqlRestoreResult> {
        verify_snapshot_signature(snapshot)?;
        validate_snapshot_blob_bytes(snapshot, blob_bytes)?;
        let blob = decode_snapshot_sql_blob(blob_bytes)?;
        // `validate_snapshot_sql_blob` re-verifies every blob op's Ed25519
        // signature (and the blob's hash chain / merkle root) against the
        // attested snapshot, so the prefix needs no separate per-op check here.
        validate_snapshot_sql_blob(snapshot, &blob, blob_bytes)?;
        // The tail comes from a possibly hostile/relaying peer: anchor each
        // writer's chain onto the snapshot's author-attested `node_frontier`
        // (bound into the verified snapshot signature). A writer absent from the
        // frontier anchors at genesis. This rejects a stale/forked but validly
        // signed segment whose first op does not continue the donor's chain.
        validate_hash_chain_anchored(operations_after_snapshot, &snapshot.node_frontier)?;
        // Unlike the blob, the tail ops are NOT covered by the snapshot
        // signature, so each must be independently signature-verified before it
        // touches SQLite or advances the frontier.
        validate_operation_signatures(operations_after_snapshot)?;

        for operation in operations_after_snapshot {
            if operation.body.partition_id != snapshot.partition_id {
                return Err(SyncLedgerError::MerklePartitionMismatch {
                    expected: snapshot.partition_id.as_str().to_string(),
                    actual: operation.body.partition_id.as_str().to_string(),
                });
            }
        }

        let mut sqlite_rows_changed = 0u64;
        for operation in blob
            .operations
            .iter()
            .chain(operations_after_snapshot.iter())
        {
            let capture = crate::sync::runtime::capture_from_operation(operation)?;
            sqlite_rows_changed +=
                crate::addon::storage_sql_exec::apply_replicated_write(&capture, operation.op_id)
                    .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        }

        Ok(SnapshotSqlRestoreResult {
            snapshot: snapshot.clone(),
            operations_applied: blob.operations.len() + operations_after_snapshot.len(),
            sqlite_rows_changed,
        })
    }

    pub fn restore_sql_from_persisted_package(
        &self,
        request: SnapshotSqlRestoreRequest,
        store: &SnapshotPackageStore,
    ) -> LedgerResult<SnapshotSqlRestoreResult> {
        let snapshot = self.ledger.get_snapshot(
            request.partition_id.clone(),
            request.up_to_sequence,
            request.snapshot_id.clone(),
        )?;
        let blob_bytes = store.get_sql_package(&snapshot)?;
        self.restore_sql_from_package(SnapshotSqlPackageRestoreRequest {
            partition_id: request.partition_id,
            up_to_sequence: request.up_to_sequence,
            snapshot_id: request.snapshot_id,
            blob_bytes,
        })
    }

    fn validated_snapshot_prefix(
        &self,
        snapshot: &SyncSnapshot,
    ) -> LedgerResult<Vec<SyncOperation>> {
        let operations = self
            .partition_prefix_operations(&snapshot.partition_id, Some(snapshot.up_to_sequence))?;
        if operations.len() as u64 != snapshot.operation_count {
            return Err(snapshot_mismatch(snapshot, "operation_count"));
        }
        validate_hash_chain(&operations)?;
        validate_operation_signatures(&operations)?;
        let summary = build_merkle_summary(&operations)?;
        if summary.from_sequence != snapshot.from_sequence {
            return Err(snapshot_mismatch(snapshot, "from_sequence"));
        }
        if summary.to_sequence != snapshot.up_to_sequence {
            return Err(snapshot_mismatch(snapshot, "up_to_sequence"));
        }
        if summary.operation_count != snapshot.operation_count {
            return Err(snapshot_mismatch(snapshot, "operation_count"));
        }
        if summary.root_hash != snapshot.root_hash {
            return Err(snapshot_mismatch(snapshot, "root_hash"));
        }
        if state_hash(&snapshot.partition_id, &operations)? != snapshot.state_hash {
            return Err(snapshot_mismatch(snapshot, "state_hash"));
        }
        if node_frontier_for_operations(&operations) != snapshot.node_frontier {
            return Err(snapshot_mismatch(snapshot, "node_frontier"));
        }
        let last_operation = operations
            .last()
            .ok_or_else(|| snapshot_mismatch(snapshot, "last_operation_hash"))?;
        if Some(last_operation.operation_hash) != snapshot.last_operation_hash {
            return Err(snapshot_mismatch(snapshot, "last_operation_hash"));
        }
        if last_operation.body.policy_epoch != snapshot.policy_epoch {
            return Err(snapshot_mismatch(snapshot, "policy_epoch"));
        }
        Ok(operations)
    }

    #[allow(clippy::too_many_arguments)]
    fn build_snapshot_from_parts(
        &self,
        partition_id: PartitionId,
        created_at_ms: i64,
        signer: &dyn SyncOperationSigner,
        snapshot_id: SnapshotId,
        from_sequence: u64,
        up_to_sequence: u64,
        operation_count: u64,
        root_hash: [u8; 32],
        state_hash: [u8; 32],
        node_frontier: BTreeMap<String, (u64, [u8; 32])>,
        last_operation_hash: Option<[u8; 32]>,
        policy_epoch: u64,
        blob: SnapshotBlobMetadata<'_>,
    ) -> LedgerResult<SyncSnapshot> {
        let signature_payload = SnapshotSignaturePayload {
            snapshot_id: snapshot_id.as_str(),
            partition_id: partition_id.as_str(),
            from_sequence,
            up_to_sequence,
            operation_count,
            root_hash,
            state_hash,
            node_frontier: &node_frontier,
            last_operation_hash,
            policy_epoch,
            blob_kind: blob.kind,
            blob_hash: blob.hash,
            blob_size_bytes: blob.size_bytes,
            created_at_ms,
            author_node_id: signer.node_id(),
        };
        let signature = signer.sign_operation(&snapshot_signing_bytes(&signature_payload)?)?;
        Ok(SyncSnapshot {
            snapshot_id,
            partition_id,
            from_sequence,
            up_to_sequence,
            operation_count,
            root_hash,
            state_hash,
            node_frontier,
            last_operation_hash,
            policy_epoch,
            blob_kind: blob.kind.map(str::to_string),
            blob_hash: blob.hash,
            blob_size_bytes: blob.size_bytes,
            created_at_ms,
            author_node_id: signer.node_id().to_string(),
            signature,
        })
    }
}

impl SnapshotPackageStore {
    pub fn default_root() -> PathBuf {
        crate::paths::tentaflow_home()
            .join("sync")
            .join("snapshot-blobs")
    }

    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn put_sql_package(
        &self,
        snapshot: &SyncSnapshot,
        blob_bytes: &[u8],
    ) -> LedgerResult<PathBuf> {
        validate_snapshot_blob_bytes(snapshot, blob_bytes)?;
        let path = self.path_for_snapshot(snapshot)?;
        if path.is_file() {
            return Ok(path);
        }
        let parent = path.parent().ok_or_else(|| {
            SyncLedgerError::Runtime("snapshot blob path has no parent".to_string())
        })?;
        std::fs::create_dir_all(parent)
            .map_err(|e| SyncLedgerError::Runtime(format!("snapshot blob dir: {e}")))?;
        let tmp = parent.join(format!(
            ".{}.tmp",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| SyncLedgerError::Runtime(format!("snapshot blob clock: {e}")))?
                .as_nanos()
        ));
        std::fs::write(&tmp, blob_bytes)
            .map_err(|e| SyncLedgerError::Runtime(format!("snapshot blob write: {e}")))?;
        std::fs::rename(&tmp, &path)
            .map_err(|e| SyncLedgerError::Runtime(format!("snapshot blob rename: {e}")))?;
        Ok(path)
    }

    pub fn get_sql_package(&self, snapshot: &SyncSnapshot) -> LedgerResult<Vec<u8>> {
        let path = self.path_for_snapshot(snapshot)?;
        let bytes = std::fs::read(&path)
            .map_err(|e| SyncLedgerError::Runtime(format!("snapshot blob read: {e}")))?;
        validate_snapshot_blob_bytes(snapshot, &bytes)?;
        Ok(bytes)
    }

    pub fn path_for_snapshot(&self, snapshot: &SyncSnapshot) -> LedgerResult<PathBuf> {
        let Some(hash) = snapshot.blob_hash else {
            return Err(snapshot_mismatch(snapshot, "blob_hash"));
        };
        if snapshot.blob_kind.as_deref() != Some(SNAPSHOT_SQL_BLOB_KIND) {
            return Err(snapshot_mismatch(snapshot, "blob_kind"));
        }
        let hex = hex::encode(hash);
        Ok(self
            .root
            .join(SNAPSHOT_SQL_BLOB_KIND)
            .join(&hex[0..2])
            .join(&hex[2..4])
            .join(format!("{hex}.bin")))
    }
}

pub fn verify_snapshot_signature(snapshot: &SyncSnapshot) -> LedgerResult<()> {
    let key_bytes =
        hex::decode(&snapshot.author_node_id).map_err(|_| SyncLedgerError::InvalidPublicKey {
            actor_node_id: snapshot.author_node_id.clone(),
        })?;
    let key_array: [u8; 32] =
        key_bytes
            .try_into()
            .map_err(|_| SyncLedgerError::InvalidPublicKey {
                actor_node_id: snapshot.author_node_id.clone(),
            })?;
    let key =
        VerifyingKey::from_bytes(&key_array).map_err(|_| SyncLedgerError::InvalidPublicKey {
            actor_node_id: snapshot.author_node_id.clone(),
        })?;
    let signature_array: [u8; 64] = snapshot.signature.as_slice().try_into().map_err(|_| {
        SyncLedgerError::InvalidSignatureLength {
            len: snapshot.signature.len(),
        }
    })?;
    let signature = Signature::from_bytes(&signature_array);
    let payload = SnapshotSignaturePayload {
        snapshot_id: snapshot.snapshot_id.as_str(),
        partition_id: snapshot.partition_id.as_str(),
        from_sequence: snapshot.from_sequence,
        up_to_sequence: snapshot.up_to_sequence,
        operation_count: snapshot.operation_count,
        root_hash: snapshot.root_hash,
        state_hash: snapshot.state_hash,
        node_frontier: &snapshot.node_frontier,
        last_operation_hash: snapshot.last_operation_hash,
        policy_epoch: snapshot.policy_epoch,
        blob_kind: snapshot.blob_kind.as_deref(),
        blob_hash: snapshot.blob_hash,
        blob_size_bytes: snapshot.blob_size_bytes,
        created_at_ms: snapshot.created_at_ms,
        author_node_id: &snapshot.author_node_id,
    };
    key.verify(&snapshot_signing_bytes(&payload)?, &signature)
        .map_err(|_| SyncLedgerError::InvalidSignature {
            actor_node_id: snapshot.author_node_id.clone(),
        })
}

fn snapshot_id(
    partition_id: &PartitionId,
    up_to_sequence: u64,
    root_hash: [u8; 32],
) -> LedgerResult<SnapshotId> {
    SnapshotId::new(format!(
        "snap_{}_{}_{}",
        partition_id.as_str().replace(['/', ':'], "_"),
        up_to_sequence,
        hex::encode(&root_hash[..8])
    ))
}

/// Deterministic digest of a snapshot's covered op set. It canonicalizes the
/// input by `partition_materialization_order` (`(actor_node_id, node_seq)`)
/// internally, so the result is independent of the order the caller supplies and
/// of HLC/clock skew — two honest nodes holding the same op set always produce
/// the same `state_hash`.
fn state_hash(
    partition_id: &PartitionId,
    operations: &[crate::sync::ledger::SyncOperation],
) -> LedgerResult<[u8; 32]> {
    let mut ordered: Vec<&crate::sync::ledger::SyncOperation> = operations.iter().collect();
    ordered.sort_by(|a, b| partition_materialization_order(a, b));
    let payload = SnapshotStatePayload {
        partition_id: partition_id.as_str(),
        // 1-based count watermark over the canonical-ordered op set.
        from_sequence: if ordered.is_empty() { 0 } else { 1 },
        up_to_sequence: ordered.len() as u64,
        operation_ids: ordered
            .iter()
            .map(|operation| *operation.op_id.as_bytes())
            .collect(),
        operation_hashes: ordered
            .iter()
            .map(|operation| operation.operation_hash)
            .collect(),
    };
    let mut bytes = SNAPSHOT_STATE_DOMAIN.to_vec();
    bytes.extend_from_slice(&hash_canonical(&payload)?);
    Ok(*blake3::hash(&bytes).as_bytes())
}

fn snapshot_signing_bytes(payload: &SnapshotSignaturePayload<'_>) -> LedgerResult<Vec<u8>> {
    let mut bytes = SNAPSHOT_SIGNATURE_DOMAIN.to_vec();
    bytes.extend_from_slice(&hash_canonical(payload)?);
    Ok(bytes)
}

fn encode_snapshot_sql_blob(blob: &SnapshotSqlBlobPackage) -> LedgerResult<Vec<u8>> {
    let payload = crate::sync::ledger::encode(blob)?;
    let mut bytes = SNAPSHOT_SQL_BLOB_DOMAIN.to_vec();
    bytes.extend_from_slice(&payload);
    Ok(bytes)
}

pub(crate) fn decode_snapshot_sql_blob(bytes: &[u8]) -> LedgerResult<SnapshotSqlBlobPackage> {
    let Some(payload) = bytes.strip_prefix(SNAPSHOT_SQL_BLOB_DOMAIN) else {
        return Err(SyncLedgerError::Runtime(
            "snapshot sql blob has invalid domain".to_string(),
        ));
    };
    crate::sync::ledger::decode(payload)
}

fn snapshot_blob_hash(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

fn validate_snapshot_sql_blob(
    snapshot: &SyncSnapshot,
    blob: &SnapshotSqlBlobPackage,
    bytes: &[u8],
) -> LedgerResult<()> {
    validate_snapshot_blob_bytes(snapshot, bytes)?;
    if blob.version != 1 {
        return Err(snapshot_mismatch(snapshot, "blob_version"));
    }
    if blob.snapshot_id != snapshot.snapshot_id {
        return Err(snapshot_mismatch(snapshot, "snapshot_id"));
    }
    if blob.partition_id != snapshot.partition_id {
        return Err(snapshot_mismatch(snapshot, "partition_id"));
    }
    if blob.from_sequence != snapshot.from_sequence {
        return Err(snapshot_mismatch(snapshot, "from_sequence"));
    }
    if blob.up_to_sequence != snapshot.up_to_sequence {
        return Err(snapshot_mismatch(snapshot, "up_to_sequence"));
    }
    if blob.operation_count != snapshot.operation_count {
        return Err(snapshot_mismatch(snapshot, "operation_count"));
    }
    if blob.root_hash != snapshot.root_hash {
        return Err(snapshot_mismatch(snapshot, "root_hash"));
    }
    if blob.state_hash != snapshot.state_hash {
        return Err(snapshot_mismatch(snapshot, "state_hash"));
    }
    if blob.node_frontier != snapshot.node_frontier {
        return Err(snapshot_mismatch(snapshot, "node_frontier"));
    }
    if blob.last_operation_hash != snapshot.last_operation_hash {
        return Err(snapshot_mismatch(snapshot, "last_operation_hash"));
    }
    if blob.policy_epoch != snapshot.policy_epoch {
        return Err(snapshot_mismatch(snapshot, "policy_epoch"));
    }
    validate_hash_chain(&blob.operations)?;
    validate_operation_signatures(&blob.operations)?;
    let summary = build_merkle_summary(&blob.operations)?;
    if summary.from_sequence != snapshot.from_sequence {
        return Err(snapshot_mismatch(snapshot, "from_sequence"));
    }
    if summary.to_sequence != snapshot.up_to_sequence {
        return Err(snapshot_mismatch(snapshot, "up_to_sequence"));
    }
    if summary.operation_count != snapshot.operation_count {
        return Err(snapshot_mismatch(snapshot, "operation_count"));
    }
    if summary.root_hash != snapshot.root_hash {
        return Err(snapshot_mismatch(snapshot, "root_hash"));
    }
    if state_hash(&snapshot.partition_id, &blob.operations)? != snapshot.state_hash {
        return Err(snapshot_mismatch(snapshot, "state_hash"));
    }
    if node_frontier_for_operations(&blob.operations) != snapshot.node_frontier {
        return Err(snapshot_mismatch(snapshot, "node_frontier"));
    }
    Ok(())
}

fn validate_snapshot_blob_bytes(snapshot: &SyncSnapshot, bytes: &[u8]) -> LedgerResult<()> {
    if snapshot.blob_kind.as_deref() != Some(SNAPSHOT_SQL_BLOB_KIND) {
        return Err(snapshot_mismatch(snapshot, "blob_kind"));
    }
    if snapshot.blob_size_bytes != bytes.len() as u64 {
        return Err(snapshot_mismatch(snapshot, "blob_size_bytes"));
    }
    if snapshot.blob_hash != Some(snapshot_blob_hash(bytes)) {
        return Err(snapshot_mismatch(snapshot, "blob_hash"));
    }
    Ok(())
}

fn validate_operation_signatures(operations: &[SyncOperation]) -> LedgerResult<()> {
    for operation in operations {
        HexNodeIdOperationVerifier.verify_operation_signature(operation)?;
    }
    Ok(())
}

fn snapshot_mismatch(snapshot: &SyncSnapshot, field: &'static str) -> SyncLedgerError {
    SyncLedgerError::SnapshotIntegrityMismatch {
        snapshot_id: snapshot.snapshot_id.as_str().to_string(),
        field,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::ledger::Ed25519OperationSigner;
    use crate::sync::ledger::{
        ActionType, CompactionPolicy, FieldValue, FjallSyncLedgerStore, HybridLogicalTimestamp,
        NewSyncOperation, SyncOperationSigner,
    };
    use ed25519_dalek::SigningKey;
    use rand_core_06::OsRng;
    use serde_json::Value as JsonValue;
    use std::collections::BTreeMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn signer() -> Ed25519OperationSigner {
        let signing_key = SigningKey::generate(&mut OsRng);
        let node_id = hex::encode(signing_key.verifying_key().to_bytes());
        Ed25519OperationSigner::new(node_id, signing_key).unwrap()
    }

    fn operation(signer: &dyn SyncOperationSigner, partition_id: PartitionId) -> NewSyncOperation {
        operation_with_resource(signer, partition_id, "person_1", 1)
    }

    fn operation_with_resource(
        signer: &dyn SyncOperationSigner,
        partition_id: PartitionId,
        resource_id: &str,
        logical: u32,
    ) -> NewSyncOperation {
        let mut changed_fields = BTreeMap::new();
        changed_fields.insert("name".to_string(), FieldValue::String("Jan".to_string()));
        NewSyncOperation {
            org_id: "org_1".to_string(),
            partition_id,
            addon_id: "contacts".to_string(),
            resource_type: "person".to_string(),
            resource_id: resource_id.to_string(),
            table_name: "persons".to_string(),
            primary_key: resource_id.to_string(),
            action: ActionType::Insert,
            changed_fields,
            before_hash: None,
            after_hash: Some([7; 32]),
            actor_user_id: "user_1".to_string(),
            actor_device_id: "device_1".to_string(),
            actor_node_id: signer.node_id().to_string(),
            hlc_timestamp: HybridLogicalTimestamp {
                wall_time_ms: 1_765_000_000_000,
                logical,
                node_id: signer.node_id().to_string(),
            },
            epoch: crate::sync::ledger::BaselineEpoch {
                counter: 0,
                origin_node: String::new(),
            },
            environment: crate::sync::ledger::NodeEnvironment::default(),
            payload_hash: [1; 32],
            acl_snapshot_hash: [2; 32],
            policy_epoch: 3,
            encryption_info: None,
        }
    }

    fn sql_operation(
        signer: &dyn SyncOperationSigner,
        partition_id: PartitionId,
        addon_id: &str,
        action: ActionType,
        query: &str,
        params: Vec<JsonValue>,
        capture_id: &str,
        logical: u32,
    ) -> NewSyncOperation {
        let mut changed_fields = BTreeMap::new();
        changed_fields.insert("sql".to_string(), FieldValue::String(query.to_string()));
        changed_fields.insert(
            "params_json".to_string(),
            FieldValue::String(JsonValue::Array(params).to_string()),
        );
        changed_fields.insert("rows_affected".to_string(), FieldValue::U64(1));
        changed_fields.insert("last_insert_id".to_string(), FieldValue::I64(1));
        changed_fields.insert(
            "capture_id".to_string(),
            FieldValue::String(capture_id.to_string()),
        );
        NewSyncOperation {
            org_id: "org-default".to_string(),
            partition_id,
            addon_id: addon_id.to_string(),
            resource_type: "contacts".to_string(),
            resource_id: "1".to_string(),
            table_name: "contacts".to_string(),
            primary_key: "1".to_string(),
            action,
            changed_fields,
            before_hash: None,
            after_hash: Some([9; 32]),
            actor_user_id: "11".to_string(),
            actor_device_id: "device_1".to_string(),
            actor_node_id: signer.node_id().to_string(),
            hlc_timestamp: HybridLogicalTimestamp {
                wall_time_ms: 1_765_000_000_000 + i64::from(logical),
                logical,
                node_id: signer.node_id().to_string(),
            },
            epoch: crate::sync::ledger::BaselineEpoch {
                counter: 0,
                origin_node: String::new(),
            },
            environment: crate::sync::ledger::NodeEnvironment::default(),
            payload_hash: [3; 32],
            acl_snapshot_hash: [4; 32],
            policy_epoch: 3,
            encryption_info: None,
        }
    }

    fn unique_addon_id(prefix: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        format!("{prefix}-{nanos}")
    }

    /// Pins a stable `HOME` (resolved by `dirs::home_dir()` inside `open_addon_db`)
    /// under the shared `test_home_lock`. Without the lock a concurrently running
    /// test that mutates the process-global `HOME` could yank it out from under
    /// an addon-db open here; holding the guard serialises HOME-dependent tests.
    struct HomeGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        _tmp: tempfile::TempDir,
        old_home: Option<std::ffi::OsString>,
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.old_home {
                Some(value) => unsafe { std::env::set_var("HOME", value) },
                None => unsafe { std::env::remove_var("HOME") },
            }
        }
    }

    fn pin_home() -> HomeGuard {
        let lock = crate::addon::fs_sandbox::test_home_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tmp = tempfile::tempdir().expect("home tempdir");
        let old_home = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }
        HomeGuard {
            _lock: lock,
            _tmp: tmp,
            old_home,
        }
    }

    #[test]
    fn snapshot_manager_builds_and_stores_partition_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        let partition = PartitionId::new("addon/contacts/persons").unwrap();
        store
            .append_operation(operation(&signer, partition.clone()), &signer)
            .unwrap();

        let manager = SnapshotManager::new(&store);
        let result = manager
            .build_and_store(
                SnapshotBuildRequest {
                    partition_id: partition.clone(),
                    up_to_sequence: None,
                    created_at_ms: 1_765_000_000_001,
                },
                &signer,
            )
            .unwrap()
            .expect("snapshot");

        assert_eq!(result.snapshot.partition_id, partition.clone());
        assert_eq!(result.snapshot.from_sequence, 1);
        assert_eq!(result.snapshot.up_to_sequence, 1);
        assert_eq!(result.snapshot.operation_count, 1);
        assert_eq!(result.snapshot.policy_epoch, 3);
        assert_eq!(result.snapshot.author_node_id, signer.node_id());
        assert_eq!(result.snapshot.signature.len(), 64);

        verify_snapshot_signature(&result.snapshot).unwrap();
        let stored = store
            .get_snapshot(partition, 1, result.snapshot.snapshot_id.clone())
            .unwrap();
        assert_eq!(stored, result.snapshot);
    }

    #[test]
    fn snapshot_signature_rejects_tampered_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        let partition = PartitionId::new("addon/contacts/persons").unwrap();
        store
            .append_operation(operation(&signer, partition.clone()), &signer)
            .unwrap();
        let manager = SnapshotManager::new(&store);
        let mut snapshot = manager
            .build_and_store(
                SnapshotBuildRequest {
                    partition_id: partition,
                    up_to_sequence: None,
                    created_at_ms: 1_765_000_000_001,
                },
                &signer,
            )
            .unwrap()
            .expect("snapshot")
            .snapshot;

        snapshot.policy_epoch = snapshot.policy_epoch.saturating_add(1);

        assert!(matches!(
            verify_snapshot_signature(&snapshot),
            Err(SyncLedgerError::InvalidSignature { .. })
        ));
    }

    #[test]
    fn snapshot_manager_returns_none_for_empty_partition() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        let manager = SnapshotManager::new(&store);

        let result = manager
            .build_and_store(
                SnapshotBuildRequest {
                    partition_id: PartitionId::new("addon/contacts/persons").unwrap(),
                    up_to_sequence: None,
                    created_at_ms: 1_765_000_000_001,
                },
                &signer,
            )
            .unwrap();

        assert!(result.is_none());
    }

    #[test]
    fn snapshot_restore_plan_returns_verified_tail_operations() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        let partition = PartitionId::new("addon/contacts/persons").unwrap();
        store
            .append_operation(operation(&signer, partition.clone()), &signer)
            .unwrap();
        let manager = SnapshotManager::new(&store);
        let snapshot = manager
            .build_and_store(
                SnapshotBuildRequest {
                    partition_id: partition.clone(),
                    up_to_sequence: Some(1),
                    created_at_ms: 1_765_000_000_001,
                },
                &signer,
            )
            .unwrap()
            .expect("snapshot")
            .snapshot;
        store
            .append_operation(
                operation_with_resource(&signer, partition.clone(), "person_2", 2),
                &signer,
            )
            .unwrap();

        let plan = manager
            .build_restore_plan(SnapshotRestoreRequest {
                partition_id: partition,
                up_to_sequence: snapshot.up_to_sequence,
                snapshot_id: snapshot.snapshot_id.clone(),
                operation_limit: None,
            })
            .unwrap();

        assert_eq!(plan.snapshot, snapshot);
        assert_eq!(plan.operations_after_snapshot.len(), 1);
        assert_eq!(plan.operations_after_snapshot[0].body.node_seq, 2);
    }

    #[test]
    fn snapshot_restore_plan_rejects_tampered_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        let partition = PartitionId::new("addon/contacts/persons").unwrap();
        store
            .append_operation(operation(&signer, partition.clone()), &signer)
            .unwrap();
        let manager = SnapshotManager::new(&store);
        let mut snapshot = manager
            .build_and_store(
                SnapshotBuildRequest {
                    partition_id: partition.clone(),
                    up_to_sequence: Some(1),
                    created_at_ms: 1_765_000_000_001,
                },
                &signer,
            )
            .unwrap()
            .expect("snapshot")
            .snapshot;
        snapshot.operation_count = snapshot.operation_count.saturating_add(1);
        store.save_snapshot(snapshot.clone()).unwrap();

        assert!(matches!(
            manager.build_restore_plan(SnapshotRestoreRequest {
                partition_id: partition,
                up_to_sequence: snapshot.up_to_sequence,
                snapshot_id: snapshot.snapshot_id,
                operation_limit: None,
            }),
            Err(SyncLedgerError::InvalidSignature { .. })
        ));
    }

    #[test]
    fn snapshot_restore_materializes_sqlite_state_from_ledger_history() {
        let _home = pin_home();
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        let addon_id = unique_addon_id("snapshot-sql-restore");
        let partition = PartitionId::new(format!("addon/{addon_id}/contacts/1")).unwrap();
        let pool = crate::addon::storage_sql::open_addon_db("org-default", &addon_id)
            .expect("open addon db");
        {
            let conn = pool.get().expect("conn");
            conn.execute(
                "CREATE TABLE contacts (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
                [],
            )
            .expect("create table");
        }
        store
            .append_operation(
                sql_operation(
                    &signer,
                    partition.clone(),
                    &addon_id,
                    ActionType::Insert,
                    "INSERT INTO contacts (id, name) VALUES (?1, ?2)",
                    vec![JsonValue::from(1), JsonValue::String("Ewa".to_string())],
                    "capture-insert",
                    1,
                ),
                &signer,
            )
            .unwrap();
        let manager = SnapshotManager::new(&store);
        let snapshot = manager
            .build_and_store(
                SnapshotBuildRequest {
                    partition_id: partition.clone(),
                    up_to_sequence: Some(1),
                    created_at_ms: 1_765_000_000_010,
                },
                &signer,
            )
            .unwrap()
            .expect("snapshot")
            .snapshot;
        store
            .append_operation(
                sql_operation(
                    &signer,
                    partition.clone(),
                    &addon_id,
                    ActionType::Update,
                    "UPDATE contacts SET name = ?1 WHERE id = ?2",
                    vec![
                        JsonValue::String("Ewa Nowak".to_string()),
                        JsonValue::from(1),
                    ],
                    "capture-update",
                    2,
                ),
                &signer,
            )
            .unwrap();

        let result = manager
            .restore_sql_from_snapshot(SnapshotSqlRestoreRequest {
                partition_id: partition,
                up_to_sequence: snapshot.up_to_sequence,
                snapshot_id: snapshot.snapshot_id,
            })
            .unwrap();

        assert_eq!(result.operations_applied, 2);
        assert_eq!(result.sqlite_rows_changed, 2);
        let conn = pool.get().expect("conn");
        let name: String = conn
            .query_row("SELECT name FROM contacts WHERE id = 1", [], |row| {
                row.get(0)
            })
            .expect("contact");
        assert_eq!(name, "Ewa Nowak");
        let applied_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM __tentaflow_sync_applied", [], |row| {
                row.get(0)
            })
            .expect("applied count");
        assert_eq!(applied_count, 2);
    }

    #[test]
    fn snapshot_sql_package_restores_after_prefix_compaction() {
        let _home = pin_home();
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        let addon_id = unique_addon_id("snapshot-sql-package");
        let partition = PartitionId::new(format!("addon/{addon_id}/contacts/1")).unwrap();
        let pool = crate::addon::storage_sql::open_addon_db("org-default", &addon_id)
            .expect("open addon db");
        {
            let conn = pool.get().expect("conn");
            conn.execute(
                "CREATE TABLE contacts (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
                [],
            )
            .expect("create table");
        }
        store
            .append_operation(
                sql_operation(
                    &signer,
                    partition.clone(),
                    &addon_id,
                    ActionType::Insert,
                    "INSERT INTO contacts (id, name) VALUES (?1, ?2)",
                    vec![JsonValue::from(1), JsonValue::String("Ewa".to_string())],
                    "package-capture-insert",
                    1,
                ),
                &signer,
            )
            .unwrap();
        let manager = SnapshotManager::new(&store);
        let package = manager
            .build_sql_package_and_store(
                SnapshotBuildRequest {
                    partition_id: partition.clone(),
                    up_to_sequence: Some(1),
                    created_at_ms: 1_765_000_000_010,
                },
                &signer,
            )
            .unwrap()
            .expect("snapshot package");
        assert_eq!(
            package.snapshot.blob_kind.as_deref(),
            Some(SNAPSHOT_SQL_BLOB_KIND)
        );
        assert_eq!(
            package.snapshot.blob_hash,
            Some(snapshot_blob_hash(&package.blob_bytes))
        );
        assert_eq!(package.blob.operations.len(), 1);
        store
            .append_operation(
                sql_operation(
                    &signer,
                    partition.clone(),
                    &addon_id,
                    ActionType::Update,
                    "UPDATE contacts SET name = ?1 WHERE id = ?2",
                    vec![
                        JsonValue::String("Ewa Nowak".to_string()),
                        JsonValue::from(1),
                    ],
                    "package-capture-update",
                    2,
                ),
                &signer,
            )
            .unwrap();
        store
            .compact(CompactionPolicy {
                partition_id: partition.clone(),
                keep_operations_after_sequence: Some(2),
            })
            .unwrap();
        assert_eq!(
            store
                .get_operations(OperationQuery {
                    partition_id: partition.clone(),
                    limit: None,
                })
                .unwrap()
                .len(),
            1
        );

        let result = manager
            .restore_sql_from_package(SnapshotSqlPackageRestoreRequest {
                partition_id: partition,
                up_to_sequence: package.snapshot.up_to_sequence,
                snapshot_id: package.snapshot.snapshot_id,
                blob_bytes: package.blob_bytes,
            })
            .unwrap();

        assert_eq!(result.operations_applied, 2);
        assert_eq!(result.sqlite_rows_changed, 2);
        let conn = pool.get().expect("conn");
        let name: String = conn
            .query_row("SELECT name FROM contacts WHERE id = 1", [], |row| {
                row.get(0)
            })
            .expect("contact");
        assert_eq!(name, "Ewa Nowak");
    }

    #[test]
    fn snapshot_sql_package_store_persists_and_restores_package() {
        let _home = pin_home();
        let dir = tempfile::tempdir().unwrap();
        let blob_dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let package_store = SnapshotPackageStore::new(blob_dir.path());
        let signer = signer();
        let addon_id = unique_addon_id("snapshot-sql-package-store");
        let partition = PartitionId::new(format!("addon/{addon_id}/contacts/1")).unwrap();
        let pool = crate::addon::storage_sql::open_addon_db("org-default", &addon_id)
            .expect("open addon db");
        {
            let conn = pool.get().expect("conn");
            conn.execute(
                "CREATE TABLE contacts (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
                [],
            )
            .expect("create table");
        }
        store
            .append_operation(
                sql_operation(
                    &signer,
                    partition.clone(),
                    &addon_id,
                    ActionType::Insert,
                    "INSERT INTO contacts (id, name) VALUES (?1, ?2)",
                    vec![JsonValue::from(1), JsonValue::String("Ewa".to_string())],
                    "store-capture-insert",
                    1,
                ),
                &signer,
            )
            .unwrap();
        let manager = SnapshotManager::new(&store);
        let persisted = manager
            .build_sql_package_and_persist(
                SnapshotBuildRequest {
                    partition_id: partition.clone(),
                    up_to_sequence: Some(1),
                    created_at_ms: 1_765_000_000_010,
                },
                &signer,
                &package_store,
            )
            .unwrap()
            .expect("persisted snapshot package");
        assert!(persisted.blob_path.is_file());
        assert_eq!(
            persisted.blob_size_bytes,
            persisted.snapshot.blob_size_bytes
        );
        store
            .append_operation(
                sql_operation(
                    &signer,
                    partition.clone(),
                    &addon_id,
                    ActionType::Update,
                    "UPDATE contacts SET name = ?1 WHERE id = ?2",
                    vec![
                        JsonValue::String("Ewa Nowak".to_string()),
                        JsonValue::from(1),
                    ],
                    "store-capture-update",
                    2,
                ),
                &signer,
            )
            .unwrap();
        store
            .compact(CompactionPolicy {
                partition_id: partition.clone(),
                keep_operations_after_sequence: Some(2),
            })
            .unwrap();

        let result = manager
            .restore_sql_from_persisted_package(
                SnapshotSqlRestoreRequest {
                    partition_id: partition,
                    up_to_sequence: persisted.snapshot.up_to_sequence,
                    snapshot_id: persisted.snapshot.snapshot_id,
                },
                &package_store,
            )
            .unwrap();

        assert_eq!(result.operations_applied, 2);
        let conn = pool.get().expect("conn");
        let name: String = conn
            .query_row("SELECT name FROM contacts WHERE id = 1", [], |row| {
                row.get(0)
            })
            .expect("contact");
        assert_eq!(name, "Ewa Nowak");
    }

    #[test]
    fn snapshot_sql_package_rejects_tampered_blob() {
        let _home = pin_home();
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        let addon_id = unique_addon_id("snapshot-sql-package-tamper");
        let partition = PartitionId::new(format!("addon/{addon_id}/contacts/1")).unwrap();
        crate::addon::storage_sql::open_addon_db("org-default", &addon_id).expect("open addon db");
        store
            .append_operation(
                sql_operation(
                    &signer,
                    partition.clone(),
                    &addon_id,
                    ActionType::Insert,
                    "INSERT INTO contacts (id, name) VALUES (?1, ?2)",
                    vec![JsonValue::from(1), JsonValue::String("Ewa".to_string())],
                    "tamper-capture-insert",
                    1,
                ),
                &signer,
            )
            .unwrap();
        let manager = SnapshotManager::new(&store);
        let mut package = manager
            .build_sql_package_and_store(
                SnapshotBuildRequest {
                    partition_id: partition.clone(),
                    up_to_sequence: Some(1),
                    created_at_ms: 1_765_000_000_010,
                },
                &signer,
            )
            .unwrap()
            .expect("snapshot package");
        let last = package.blob_bytes.len() - 1;
        package.blob_bytes[last] ^= 0x01;

        assert!(matches!(
            manager.restore_sql_from_package(SnapshotSqlPackageRestoreRequest {
                partition_id: partition,
                up_to_sequence: package.snapshot.up_to_sequence,
                snapshot_id: package.snapshot.snapshot_id,
                blob_bytes: package.blob_bytes,
            }),
            Err(SyncLedgerError::SnapshotIntegrityMismatch {
                field: "blob_hash",
                ..
            })
        ));
    }

    /// Two operations from two writers, validly chained per node, built directly
    /// so a test can shuffle them and prove the snapshot hashes are order-free.
    fn two_writer_ops(
        signer_a: &dyn SyncOperationSigner,
        signer_b: &dyn SyncOperationSigner,
        partition: &PartitionId,
    ) -> Vec<SyncOperation> {
        let op_a = SyncOperation::from_new(
            operation_with_resource(signer_a, partition.clone(), "person_a", 1),
            1,
            None,
            signer_a,
        )
        .unwrap();
        let op_b = SyncOperation::from_new(
            operation_with_resource(signer_b, partition.clone(), "person_b", 1),
            1,
            None,
            signer_b,
        )
        .unwrap();
        vec![op_a, op_b]
    }

    #[test]
    fn snapshot_roundtrip_state_hash_canonical() {
        // The snapshot is built off the canonical (actor_node_id, node_seq) order,
        // never insertion order or HLC: the state_hash, node_frontier and root_hash
        // must be identical for any permutation of the same op set, and survive a
        // CBOR round-trip + signature verify unchanged.
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        let partition = PartitionId::new("addon/contacts/persons").unwrap();
        store
            .append_operation(operation(&signer, partition.clone()), &signer)
            .unwrap();
        store
            .append_operation(
                operation_with_resource(&signer, partition.clone(), "person_2", 2),
                &signer,
            )
            .unwrap();
        let manager = SnapshotManager::new(&store);
        let snapshot = manager
            .build_and_store(
                SnapshotBuildRequest {
                    partition_id: partition.clone(),
                    up_to_sequence: None,
                    created_at_ms: 1_765_000_000_001,
                },
                &signer,
            )
            .unwrap()
            .expect("snapshot")
            .snapshot;

        // node_frontier covers the single writer up to its highest node_seq.
        assert_eq!(snapshot.node_frontier.len(), 1);
        let (last_seq, _) = snapshot.node_frontier[signer.node_id()];
        assert_eq!(last_seq, 2);

        let bytes = crate::sync::ledger::encode(&snapshot).unwrap();
        let decoded: SyncSnapshot = crate::sync::ledger::decode(&bytes).unwrap();
        assert_eq!(decoded, snapshot);
        verify_snapshot_signature(&decoded).unwrap();

        // Permutation independence at the structural level: the two pure functions
        // that feed the snapshot signature must ignore slice order for a multi-
        // writer set (different authors interleaved).
        let signer_b = {
            let signing_key = SigningKey::generate(&mut OsRng);
            let node_id = hex::encode(signing_key.verifying_key().to_bytes());
            Ed25519OperationSigner::new(node_id, signing_key).unwrap()
        };
        let ops = two_writer_ops(&signer, &signer_b, &partition);
        let reversed: Vec<SyncOperation> = ops.iter().rev().cloned().collect();
        assert_eq!(
            super::state_hash(&partition, &ops).unwrap(),
            super::state_hash(&partition, &reversed).unwrap()
        );
        assert_eq!(
            crate::sync::ledger::node_frontier_for_operations(&ops),
            crate::sync::ledger::node_frontier_for_operations(&reversed)
        );
        assert_eq!(
            crate::sync::ledger::build_merkle_summary(&ops)
                .unwrap()
                .root_hash,
            crate::sync::ledger::build_merkle_summary(&reversed)
                .unwrap()
                .root_hash
        );
    }

    #[test]
    fn new_node_adopt_converges() {
        // A fresh node adopts a donor snapshot package: it learns the donor's
        // attested node_frontier and recomputes state_hash/root_hash over the exact
        // op set the donor signed. Both must equal the donor's signed values — that
        // equality is precisely what makes the adopted state convergent with the
        // donor, with no fork. `validate_snapshot_sql_blob` is the production gate
        // that enforces this on every import; here we exercise it end-to-end.
        let _home = pin_home();
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        let addon_id = unique_addon_id("adopt-converge");
        let partition = PartitionId::new(format!("addon/{addon_id}/contacts/1")).unwrap();
        let pool = crate::addon::storage_sql::open_addon_db("org-default", &addon_id)
            .expect("open addon db");
        {
            let conn = pool.get().expect("conn");
            conn.execute(
                "CREATE TABLE contacts (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
                [],
            )
            .expect("create table");
        }
        store
            .append_operation(
                sql_operation(
                    &signer,
                    partition.clone(),
                    &addon_id,
                    ActionType::Insert,
                    "INSERT INTO contacts (id, name) VALUES (?1, ?2)",
                    vec![JsonValue::from(1), JsonValue::String("Ola".to_string())],
                    "adopt-capture-insert",
                    1,
                ),
                &signer,
            )
            .unwrap();
        let donor = SnapshotManager::new(&store)
            .build_sql_package_and_store(
                SnapshotBuildRequest {
                    partition_id: partition.clone(),
                    up_to_sequence: Some(1),
                    created_at_ms: 1_765_000_000_010,
                },
                &signer,
            )
            .unwrap()
            .expect("snapshot package");

        // The fresh node decodes the donor blob and recomputes the canonical
        // structural fields over the donor's op set; convergence == byte-equality
        // with the donor's signed snapshot fields.
        let blob = decode_snapshot_sql_blob(&donor.blob_bytes).unwrap();
        let fresh_node = SnapshotManager::new(&store);
        fresh_node
            .restore_sql_from_package(SnapshotSqlPackageRestoreRequest {
                partition_id: partition.clone(),
                up_to_sequence: donor.snapshot.up_to_sequence,
                snapshot_id: donor.snapshot.snapshot_id.clone(),
                blob_bytes: donor.blob_bytes.clone(),
            })
            .unwrap();

        assert_eq!(
            super::state_hash(&partition, &blob.operations).unwrap(),
            donor.snapshot.state_hash
        );
        assert_eq!(
            crate::sync::ledger::node_frontier_for_operations(&blob.operations),
            donor.snapshot.node_frontier
        );
        assert_eq!(
            crate::sync::ledger::build_merkle_summary(&blob.operations)
                .unwrap()
                .root_hash,
            donor.snapshot.root_hash
        );
        // The adopted frontier names the donor writer at its highest covered seq.
        assert_eq!(donor.snapshot.node_frontier[signer.node_id()].0, 1);
    }

    /// Builds a donor SQL package whose snapshot covers a single seq-1 op, plus a
    /// validly signed seq-2 tail op chained onto the snapshot frontier. Returns the
    /// store, partition, snapshot package and the legitimate tail op so individual
    /// tests can swap the tail for a tampered/forked variant.
    fn donor_with_tail(
        addon_prefix: &str,
    ) -> (
        tempfile::TempDir,
        FjallSyncLedgerStore,
        PartitionId,
        SyncSnapshot,
        Vec<u8>,
        SyncOperation,
        Ed25519OperationSigner,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        let addon_id = unique_addon_id(addon_prefix);
        let partition = PartitionId::new(format!("addon/{addon_id}/contacts/1")).unwrap();
        let pool = crate::addon::storage_sql::open_addon_db("org-default", &addon_id)
            .expect("open addon db");
        {
            let conn = pool.get().expect("conn");
            conn.execute(
                "CREATE TABLE contacts (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
                [],
            )
            .expect("create table");
        }
        let first = store
            .append_operation(
                sql_operation(
                    &signer,
                    partition.clone(),
                    &addon_id,
                    ActionType::Insert,
                    "INSERT INTO contacts (id, name) VALUES (?1, ?2)",
                    vec![JsonValue::from(1), JsonValue::String("Ola".to_string())],
                    "donor-capture-insert",
                    1,
                ),
                &signer,
            )
            .unwrap();
        let donor = SnapshotManager::new(&store)
            .build_sql_package_and_store(
                SnapshotBuildRequest {
                    partition_id: partition.clone(),
                    up_to_sequence: Some(1),
                    created_at_ms: 1_765_000_000_010,
                },
                &signer,
            )
            .unwrap()
            .expect("snapshot package");

        // A real seq-2 op for the same writer, chained onto the seq-1 op's hash —
        // exactly what the snapshot frontier attests, so the legitimate tail passes.
        let tail = SyncOperation::from_new(
            sql_operation(
                &signer,
                partition.clone(),
                &addon_id,
                ActionType::Update,
                "UPDATE contacts SET name = ?2 WHERE id = ?1",
                vec![JsonValue::from(1), JsonValue::String("Ala".to_string())],
                "donor-capture-update",
                2,
            ),
            2,
            Some(first.operation_hash),
            &signer,
        )
        .unwrap();

        let snapshot = donor.snapshot.clone();
        let blob_bytes = donor.blob_bytes.clone();
        (dir, store, partition, snapshot, blob_bytes, tail, signer)
    }

    #[test]
    fn remote_tail_with_forged_signature_is_rejected() {
        // A relay swaps the tail op for one signed by an UNRELATED key (so its
        // actor_node_id no longer matches the signing key). The wire adopt path
        // must reject it on per-op signature verification before any SQLite write
        // or frontier advance — the snapshot signature only covers the blob, never
        // the tail (CR-B1).
        let _home = pin_home();
        let (_dir, store, _partition, snapshot, blob_bytes, tail, _signer) =
            donor_with_tail("forged-tail");
        let manager = SnapshotManager::new(&store);

        // Re-sign the same op body with a foreign key while keeping the genuine
        // actor_node_id: the signature no longer verifies against that node's key.
        let foreign = signer();
        let mut forged = tail.clone();
        forged.signature = foreign.sign_operation(&forged.signing_bytes()).unwrap();

        let result = manager.restore_sql_from_package_parts(&snapshot, &blob_bytes, &[forged]);
        assert!(
            matches!(result, Err(SyncLedgerError::InvalidSignature { .. })),
            "forged tail signature must be rejected, got {result:?}"
        );
    }

    #[test]
    fn remote_tail_forked_off_frontier_is_rejected() {
        // The tail op is genuinely signed by the donor writer but chains onto a
        // bogus predecessor instead of the snapshot's attested frontier hash — a
        // stale/forked segment a relay could splice. It must be rejected before
        // touching SQLite or advancing the frontier (CR-W1).
        let _home = pin_home();
        let (_dir, store, partition, snapshot, blob_bytes, _tail, signer) =
            donor_with_tail("forked-tail");
        let manager = SnapshotManager::new(&store);
        let addon_id = partition
            .as_str()
            .trim_start_matches("addon/")
            .split('/')
            .next()
            .unwrap()
            .to_string();

        // Same writer, correct seq-2, validly signed, but prev_node_hash points at
        // a fork rather than the seq-1 hash the snapshot frontier attests.
        let forked = SyncOperation::from_new(
            sql_operation(
                &signer,
                partition.clone(),
                &addon_id,
                ActionType::Update,
                "UPDATE contacts SET name = ?2 WHERE id = ?1",
                vec![JsonValue::from(1), JsonValue::String("Eva".to_string())],
                "forked-capture-update",
                2,
            ),
            2,
            Some([0xAB; 32]),
            &signer,
        )
        .unwrap();

        let result = manager.restore_sql_from_package_parts(&snapshot, &blob_bytes, &[forked]);
        assert!(
            matches!(result, Err(SyncLedgerError::HashChainMismatch { .. })),
            "forked tail must be rejected against attested frontier, got {result:?}"
        );

        // The legitimate tail (chained onto the frontier) is still accepted, so the
        // anchoring check did not over-reject.
        let (_dir2, store2, _p2, snapshot2, blob2, good_tail, _signer2) =
            donor_with_tail("forked-tail-ok");
        SnapshotManager::new(&store2)
            .restore_sql_from_package_parts(&snapshot2, &blob2, &[good_tail])
            .expect("legitimate frontier-anchored tail must be accepted");
    }
}

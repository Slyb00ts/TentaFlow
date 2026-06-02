// =============================================================================
// Plik: sync/snapshot.rs
// Opis: Snapshot Manager buduje podpisane checkpointy partycji Sync Ledger.
// =============================================================================

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::sync::ledger::{
    build_merkle_summary, hash_canonical, validate_hash_chain, validate_hash_chain_from,
    HexNodeIdOperationVerifier, LedgerResult, OperationQuery, PartitionId, SnapshotId,
    SyncLedgerError, SyncLedgerStore, SyncOperation, SyncOperationSigner, SyncOperationVerifier,
    SyncSnapshot,
};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

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

    pub fn build_and_store(
        &self,
        request: SnapshotBuildRequest,
        signer: &dyn SyncOperationSigner,
    ) -> LedgerResult<Option<SnapshotBuildResult>> {
        let Some(head) = self
            .ledger
            .get_partition_head(request.partition_id.clone())?
        else {
            return Ok(None);
        };
        let up_to_sequence = request
            .up_to_sequence
            .unwrap_or(head.last_sequence)
            .min(head.last_sequence);
        if up_to_sequence == 0 {
            return Ok(None);
        }

        let operations = self.ledger.get_operations(OperationQuery {
            partition_id: request.partition_id.clone(),
            from_sequence: Some(1),
            to_sequence: Some(up_to_sequence),
            limit: Some(usize::try_from(up_to_sequence).map_err(|_| {
                SyncLedgerError::Runtime("snapshot sequence does not fit usize".to_string())
            })?),
        })?;
        if operations.is_empty() {
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
        let snapshot_id = snapshot_id(&request.partition_id, up_to_sequence, summary.root_hash)?;
        let signature_payload = SnapshotSignaturePayload {
            snapshot_id: snapshot_id.as_str(),
            partition_id: request.partition_id.as_str(),
            from_sequence: summary.from_sequence,
            up_to_sequence,
            operation_count: summary.operation_count,
            root_hash: summary.root_hash,
            state_hash,
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
        let Some(head) = self
            .ledger
            .get_partition_head(request.partition_id.clone())?
        else {
            return Ok(None);
        };
        let up_to_sequence = request
            .up_to_sequence
            .unwrap_or(head.last_sequence)
            .min(head.last_sequence);
        if up_to_sequence == 0 {
            return Ok(None);
        }

        let operations = self.ledger.get_operations(OperationQuery {
            partition_id: request.partition_id.clone(),
            from_sequence: Some(1),
            to_sequence: Some(up_to_sequence),
            limit: Some(usize::try_from(up_to_sequence).map_err(|_| {
                SyncLedgerError::Runtime("snapshot sequence does not fit usize".to_string())
            })?),
        })?;
        if operations.is_empty() {
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

        let from_sequence = snapshot.up_to_sequence.saturating_add(1);
        let operations_after_snapshot = match self
            .ledger
            .get_partition_head(request.partition_id.clone())?
        {
            Some(head) if head.last_sequence >= from_sequence => {
                self.ledger.get_operations(OperationQuery {
                    partition_id: request.partition_id,
                    from_sequence: Some(from_sequence),
                    to_sequence: None,
                    limit: request.operation_limit,
                })?
            }
            _ => Vec::new(),
        };
        if let Some(first_operation) = operations_after_snapshot.first() {
            if first_operation.body.partition_sequence != from_sequence {
                return Err(SyncLedgerError::MerkleSequenceGap {
                    expected: from_sequence,
                    actual: first_operation.body.partition_sequence,
                });
            }
            validate_hash_chain_from(&operations_after_snapshot, snapshot.last_operation_hash)?;
            validate_operation_signatures(&operations_after_snapshot)?;
        }

        Ok(SnapshotRestorePlan {
            snapshot,
            operations_after_snapshot,
        })
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
        let plan = self.build_restore_plan(SnapshotRestoreRequest {
            partition_id: request.partition_id,
            up_to_sequence: request.up_to_sequence,
            snapshot_id: request.snapshot_id,
            operation_limit: None,
        })?;
        validate_snapshot_blob_bytes(&plan.snapshot, &request.blob_bytes)?;
        let blob = decode_snapshot_sql_blob(&request.blob_bytes)?;
        validate_snapshot_sql_blob(&plan.snapshot, &blob, &request.blob_bytes)?;

        let mut sqlite_rows_changed = 0u64;
        for operation in blob
            .operations
            .iter()
            .chain(plan.operations_after_snapshot.iter())
        {
            let capture = crate::sync::runtime::capture_from_operation(operation)?;
            sqlite_rows_changed +=
                crate::addon::storage_sql_exec::apply_replicated_write(&capture, operation.op_id)
                    .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        }

        Ok(SnapshotSqlRestoreResult {
            snapshot: plan.snapshot,
            operations_applied: blob.operations.len() + plan.operations_after_snapshot.len(),
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
        validate_snapshot_sql_blob(snapshot, &blob, blob_bytes)?;
        validate_hash_chain_from(operations_after_snapshot, snapshot.last_operation_hash)?;

        let mut expected_sequence = snapshot.up_to_sequence.saturating_add(1);
        for operation in operations_after_snapshot {
            if operation.body.partition_id != snapshot.partition_id {
                return Err(SyncLedgerError::MerklePartitionMismatch {
                    expected: snapshot.partition_id.as_str().to_string(),
                    actual: operation.body.partition_id.as_str().to_string(),
                });
            }
            if operation.body.partition_sequence != expected_sequence {
                return Err(SyncLedgerError::MerkleSequenceGap {
                    expected: expected_sequence,
                    actual: operation.body.partition_sequence,
                });
            }
            expected_sequence = expected_sequence.saturating_add(1);
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
        let operations = self.ledger.get_operations(OperationQuery {
            partition_id: snapshot.partition_id.clone(),
            from_sequence: Some(snapshot.from_sequence),
            to_sequence: Some(snapshot.up_to_sequence),
            limit: Some(
                usize::try_from(snapshot.operation_count)
                    .map_err(|_| snapshot_mismatch(snapshot, "operation_count"))?,
            ),
        })?;
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

fn state_hash(
    partition_id: &PartitionId,
    operations: &[crate::sync::ledger::SyncOperation],
) -> LedgerResult<[u8; 32]> {
    let payload = SnapshotStatePayload {
        partition_id: partition_id.as_str(),
        from_sequence: operations
            .first()
            .map(|operation| operation.body.partition_sequence)
            .unwrap_or_default(),
        up_to_sequence: operations
            .last()
            .map(|operation| operation.body.partition_sequence)
            .unwrap_or_default(),
        operation_ids: operations
            .iter()
            .map(|operation| *operation.op_id.as_bytes())
            .collect(),
        operation_hashes: operations
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
        assert_eq!(plan.operations_after_snapshot[0].body.partition_sequence, 2);
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
        assert!(store
            .get_operations(OperationQuery {
                partition_id: partition.clone(),
                from_sequence: Some(1),
                to_sequence: Some(1),
                limit: None,
            })
            .unwrap()
            .is_empty());

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
}

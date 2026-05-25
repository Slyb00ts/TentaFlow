// =============================================================================
// Plik: sync/runtime.rs
// Opis: Procesowy runtime Sync Ledger laczacy zapisy SQL addonow z Fjall i outbox.
// =============================================================================

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::addon::storage_sql_exec::{SyncConflictResolution, SyncConflictResolveResult};
use crate::db::{DbPool, repository};
use crate::mesh::security::MeshSecurity;
use crate::paths;
use crate::sync::ledger::{
    ActionType, FieldValue, FjallSyncLedgerStore, HexNodeIdOperationVerifier,
    HybridLogicalTimestamp, LedgerResult, NewSyncOperation, OperationId, OperationQuery,
    PartitionId, PeerCursor, PeerId, RepairQueueEntry, SnapshotId, SyncLedgerError,
    SyncLedgerStore, SyncOperation, SyncOperationSigner, SyncSnapshot, SyncTarget,
};
use crate::sync::snapshot::{SnapshotManager, SnapshotPackageStore, verify_snapshot_signature};
use tentaflow_protocol::mesh::{
    MeshSyncAckPayload, MeshSyncOperationWire, MeshSyncPullPayload, MeshSyncPullResponsePayload,
    MeshSyncPushPayload, MeshSyncSnapshotPullPayload, MeshSyncSnapshotResponsePayload,
};

static SYNC_RUNTIME: OnceLock<Arc<SyncRuntime>> = OnceLock::new();
const BLOB_SYNC_CHUNK_SIZE: usize = 1024 * 1024;

pub struct SyncRuntime {
    db: DbPool,
    ledger: Arc<FjallSyncLedgerStore>,
    signer: RuntimeSigner,
    local_node_id: String,
}

struct RuntimeSigner {
    node_id: String,
    security: Arc<MeshSecurity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SqlWriteCapture {
    pub capture_id: String,
    pub org_id: String,
    pub addon_id: String,
    pub table_name: String,
    pub action: SqlWriteAction,
    pub resource_type: String,
    pub resource_id: String,
    pub query: String,
    pub params: Vec<JsonValue>,
    pub rows_affected: u64,
    pub last_insert_id: i64,
    pub actor_user_id: Option<i64>,
    pub created_at_ms: i64,
}

pub use crate::sync::kv_capture::KvWriteCapture;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SqlWriteAction {
    Insert,
    Update,
    Delete,
}

impl SqlWriteAction {
    pub fn from_str(value: &str) -> LedgerResult<Self> {
        match value {
            "insert" => Ok(SqlWriteAction::Insert),
            "update" => Ok(SqlWriteAction::Update),
            "delete" => Ok(SqlWriteAction::Delete),
            other => Err(crate::sync::ledger::SyncLedgerError::Runtime(format!(
                "unknown sql write action: {other}"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SqlWriteAction::Insert => "insert",
            SqlWriteAction::Update => "update",
            SqlWriteAction::Delete => "delete",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SqlCaptureRecordResult {
    pub op_id: OperationId,
    pub queued_targets: usize,
}

pub enum MeshSyncPullResult {
    Operations(MeshSyncPullResponsePayload),
    Snapshot(MeshSyncSnapshotResponsePayload),
}

pub fn init(db: DbPool, signer: Arc<MeshSecurity>) -> LedgerResult<Arc<SyncRuntime>> {
    let ledger_path = paths::tentaflow_home().join("sync").join("ledger");
    let ledger = Arc::new(FjallSyncLedgerStore::open(&ledger_path)?);
    let local_node_id = signer.ed25519_public_key_hex();
    let runtime = Arc::new(SyncRuntime {
        db,
        ledger,
        signer: RuntimeSigner {
            node_id: local_node_id.clone(),
            security: signer,
        },
        local_node_id,
    });
    let _ = SYNC_RUNTIME.set(runtime.clone());
    Ok(runtime)
}

pub fn record_sql_capture(
    capture: SqlWriteCapture,
) -> LedgerResult<Option<SqlCaptureRecordResult>> {
    let Some(runtime) = SYNC_RUNTIME.get() else {
        return Ok(None);
    };
    runtime.record_sql_capture(capture).map(Some)
}

pub fn record_core_capture(
    capture: crate::sync::core_capture::CoreWriteCapture,
) -> LedgerResult<Option<SqlCaptureRecordResult>> {
    let Some(runtime) = SYNC_RUNTIME.get() else {
        return Ok(None);
    };
    runtime.record_core_capture(capture).map(Some)
}

pub fn record_blob_capture(
    capture: crate::sync::blob_capture::BlobWriteCapture,
) -> LedgerResult<Option<SqlCaptureRecordResult>> {
    let Some(runtime) = SYNC_RUNTIME.get() else {
        return Ok(None);
    };
    runtime.record_blob_capture(capture).map(Some)
}

pub fn record_kv_capture(capture: KvWriteCapture) -> LedgerResult<Option<SqlCaptureRecordResult>> {
    let Some(runtime) = SYNC_RUNTIME.get() else {
        return Ok(None);
    };
    runtime.record_kv_capture(capture).map(Some)
}

pub fn record_sql_capture_outbox_only(
    capture: SqlWriteCapture,
    op_id: OperationId,
) -> LedgerResult<Option<SqlCaptureRecordResult>> {
    let Some(runtime) = SYNC_RUNTIME.get() else {
        return Ok(None);
    };
    runtime
        .record_sql_capture_outbox_only(capture, op_id)
        .map(Some)
}

pub fn build_push_payload_for_target(
    target_node_id: &str,
    limit: usize,
) -> LedgerResult<Option<MeshSyncPushPayload>> {
    let Some(runtime) = SYNC_RUNTIME.get() else {
        return Ok(None);
    };
    runtime.build_push_payload_for_target(target_node_id, limit)
}

pub fn handle_push_payload(
    source_node_id: &str,
    payload: MeshSyncPushPayload,
) -> LedgerResult<Option<MeshSyncAckPayload>> {
    let Some(runtime) = SYNC_RUNTIME.get() else {
        return Ok(None);
    };
    runtime
        .handle_push_payload(source_node_id, payload)
        .map(Some)
}

pub fn handle_ack_payload(source_node_id: &str, payload: MeshSyncAckPayload) -> LedgerResult<()> {
    let Some(runtime) = SYNC_RUNTIME.get() else {
        return Ok(());
    };
    runtime.handle_ack_payload(source_node_id, payload)
}

pub fn handle_pull_payload(
    source_node_id: &str,
    payload: MeshSyncPullPayload,
) -> LedgerResult<Option<MeshSyncPullResult>> {
    let Some(runtime) = SYNC_RUNTIME.get() else {
        return Ok(None);
    };
    runtime
        .handle_pull_payload(source_node_id, payload)
        .map(Some)
}

pub fn handle_pull_response_payload(
    source_node_id: &str,
    payload: MeshSyncPullResponsePayload,
) -> LedgerResult<Option<MeshSyncAckPayload>> {
    let Some(runtime) = SYNC_RUNTIME.get() else {
        return Ok(None);
    };
    runtime
        .handle_pull_response_payload(source_node_id, payload)
        .map(Some)
}

pub fn handle_snapshot_pull_payload(
    source_node_id: &str,
    payload: MeshSyncSnapshotPullPayload,
) -> LedgerResult<Option<MeshSyncSnapshotResponsePayload>> {
    let Some(runtime) = SYNC_RUNTIME.get() else {
        return Ok(None);
    };
    runtime
        .handle_snapshot_pull_payload(source_node_id, payload)
        .map(Some)
}

pub fn build_snapshot_pull_payload(
    partition_id: &str,
    up_to_sequence: u64,
    snapshot_id: &str,
    include_tail: bool,
    tail_limit: u32,
) -> LedgerResult<Option<MeshSyncSnapshotPullPayload>> {
    let Some(runtime) = SYNC_RUNTIME.get() else {
        return Ok(None);
    };
    runtime
        .build_snapshot_pull_payload(
            partition_id,
            up_to_sequence,
            snapshot_id,
            include_tail,
            tail_limit,
        )
        .map(Some)
}

pub fn build_repair_pull_payloads_for_peer(
    peer_id: &str,
    max_partitions: usize,
    operation_limit: u32,
) -> LedgerResult<Vec<MeshSyncPullPayload>> {
    let Some(runtime) = SYNC_RUNTIME.get() else {
        return Ok(Vec::new());
    };
    runtime.build_repair_pull_payloads_for_peer(peer_id, max_partitions, operation_limit)
}

pub fn handle_snapshot_response_payload(
    source_node_id: &str,
    payload: MeshSyncSnapshotResponsePayload,
) -> LedgerResult<Option<MeshSyncAckPayload>> {
    let Some(runtime) = SYNC_RUNTIME.get() else {
        return Ok(None);
    };
    runtime
        .handle_snapshot_response_payload(source_node_id, payload)
        .map(Some)
}

pub fn apply_unapplied_inbox(limit: usize) -> LedgerResult<Option<usize>> {
    let Some(runtime) = SYNC_RUNTIME.get() else {
        return Ok(None);
    };
    runtime.apply_unapplied_inbox(limit).map(Some)
}

pub fn resolve_addon_sync_conflict(
    org_id: &str,
    addon_id: &str,
    operation_id: OperationId,
    resolution: SyncConflictResolution,
) -> LedgerResult<Option<SyncConflictResolveResult>> {
    let Some(runtime) = SYNC_RUNTIME.get() else {
        return Ok(None);
    };
    runtime
        .resolve_addon_sync_conflict(org_id, addon_id, operation_id, resolution)
        .map(Some)
}

impl SyncRuntime {
    fn record_sql_capture(&self, capture: SqlWriteCapture) -> LedgerResult<SqlCaptureRecordResult> {
        let op = self.build_operation(&capture)?;
        let append = self.ledger.append_operation(op, &self.signer)?;
        let queued_targets = self.queue_targets(&capture, append.op_id)?;
        Ok(SqlCaptureRecordResult {
            op_id: append.op_id,
            queued_targets,
        })
    }

    fn record_core_capture(
        &self,
        capture: crate::sync::core_capture::CoreWriteCapture,
    ) -> LedgerResult<SqlCaptureRecordResult> {
        let op = self.build_core_operation(&capture)?;
        let append = self.ledger.append_operation(op, &self.signer)?;
        let queued_targets = self.queue_core_targets(&capture, append.op_id)?;
        Ok(SqlCaptureRecordResult {
            op_id: append.op_id,
            queued_targets,
        })
    }

    fn record_blob_capture(
        &self,
        capture: crate::sync::blob_capture::BlobWriteCapture,
    ) -> LedgerResult<SqlCaptureRecordResult> {
        self.append_blob_operations(&capture)
    }

    fn record_kv_capture(&self, capture: KvWriteCapture) -> LedgerResult<SqlCaptureRecordResult> {
        let op = self.build_kv_operation(&capture)?;
        let append = self.ledger.append_operation(op, &self.signer)?;
        let queued_targets = self.queue_targets_for_resource(
            &capture.org_id,
            &capture.addon_id,
            "addon.kv",
            &kv_resource_id(&capture.instance_id, &capture.key),
            append.op_id,
        )?;
        Ok(SqlCaptureRecordResult {
            op_id: append.op_id,
            queued_targets,
        })
    }

    fn record_sql_capture_outbox_only(
        &self,
        capture: SqlWriteCapture,
        op_id: OperationId,
    ) -> LedgerResult<SqlCaptureRecordResult> {
        self.ledger.get_operation(op_id)?;
        let queued_targets = self.queue_targets(&capture, op_id)?;
        Ok(SqlCaptureRecordResult {
            op_id,
            queued_targets,
        })
    }

    fn build_repair_pull_payloads_for_peer(
        &self,
        peer_id: &str,
        max_partitions: usize,
        operation_limit: u32,
    ) -> LedgerResult<Vec<MeshSyncPullPayload>> {
        let now = now_ms();
        let peer = PeerId::new(peer_id.to_string())?;
        let requests = self
            .ledger
            .list_due_repair_requests(peer.clone(), now, max_partitions)?;
        let mut payloads = Vec::new();
        for request in requests {
            payloads.push(MeshSyncPullPayload {
                from_node_id: self.local_node_id.clone(),
                partition_id: request.partition_id.as_str().to_string(),
                from_sequence: request.from_sequence,
                limit: operation_limit,
            });
            let retry_count = request.retry_count.saturating_add(1);
            let next_attempt_ms = now.saturating_add(repair_backoff_ms(retry_count));
            self.ledger.mark_repair_attempted(
                peer.clone(),
                request.partition_id,
                next_attempt_ms,
                retry_count,
            )?;
        }
        Ok(payloads)
    }

    fn queue_targets(&self, capture: &SqlWriteCapture, op_id: OperationId) -> LedgerResult<usize> {
        self.queue_targets_for_resource(
            &capture.org_id,
            &capture.addon_id,
            &capture.resource_type,
            &capture.resource_id,
            op_id,
        )
    }

    fn queue_targets_for_resource(
        &self,
        org_id: &str,
        addon_id: &str,
        resource_type: &str,
        resource_id: &str,
        op_id: OperationId,
    ) -> LedgerResult<usize> {
        let targets = repository::list_sync_targets_for_resource(
            &self.db,
            org_id,
            addon_id,
            resource_type,
            resource_id,
        )
        .map_err(|e| crate::sync::ledger::SyncLedgerError::Runtime(e.to_string()))?;
        let mut queued = 0usize;
        for target in targets {
            match SyncTarget::new(target.node_id) {
                Ok(sync_target) => {
                    self.ledger.put_in_outbox(sync_target, op_id)?;
                    queued += 1;
                }
                Err(e) => warn!("sync runtime: pominieto niepoprawny target outbox: {}", e),
            }
        }
        Ok(queued)
    }

    fn queue_core_targets(
        &self,
        capture: &crate::sync::core_capture::CoreWriteCapture,
        op_id: OperationId,
    ) -> LedgerResult<usize> {
        let targets = repository::list_sync_targets_for_resource(
            &self.db,
            &capture.org_id,
            crate::sync::core_registry::CORE_SYNC_ADDON_ID,
            &capture.resource_type,
            &capture.resource_id,
        )
        .map_err(|e| crate::sync::ledger::SyncLedgerError::Runtime(e.to_string()))?;
        let mut queued = 0usize;
        for target in targets {
            match SyncTarget::new(target.node_id) {
                Ok(sync_target) => {
                    self.ledger.put_in_outbox(sync_target, op_id)?;
                    queued += 1;
                }
                Err(e) => warn!("sync runtime: pominieto niepoprawny target outbox: {}", e),
            }
        }
        Ok(queued)
    }

    fn build_push_payload_for_target(
        &self,
        target_node_id: &str,
        limit: usize,
    ) -> LedgerResult<Option<MeshSyncPushPayload>> {
        let target = SyncTarget::new(target_node_id.to_string())?;
        let entries = self.ledger.list_pending_outbox(target.clone(), limit)?;
        if entries.is_empty() {
            return Ok(None);
        }
        let mut pending = Vec::with_capacity(entries.len());
        for entry in entries {
            let operation = self.ledger.get_operation(entry.op_id)?;
            if !self.outbox_target_still_allowed(target.as_str(), &operation)? {
                self.ledger.mark_acknowledged(target.clone(), entry.op_id)?;
                continue;
            }
            pending.push((entry.op_id, operation));
        }
        pending.sort_by(|(_, left), (_, right)| {
            left.body
                .partition_id
                .as_str()
                .cmp(right.body.partition_id.as_str())
                .then_with(|| {
                    left.body
                        .partition_sequence
                        .cmp(&right.body.partition_sequence)
                })
        });
        let mut operations = Vec::with_capacity(pending.len());
        for (op_id, operation) in pending {
            operations.push(operation_to_wire(&operation)?);
            self.ledger.mark_delivered(target.clone(), op_id)?;
        }
        if operations.is_empty() {
            return Ok(None);
        }
        Ok(Some(MeshSyncPushPayload {
            from_node_id: self.local_node_id.clone(),
            operations,
        }))
    }

    fn outbox_target_still_allowed(
        &self,
        target_node_id: &str,
        operation: &SyncOperation,
    ) -> LedgerResult<bool> {
        let targets = repository::list_sync_targets_for_resource(
            &self.db,
            &operation.body.org_id,
            &operation.body.addon_id,
            &operation.body.resource_type,
            &operation.body.resource_id,
        )
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        Ok(targets
            .iter()
            .any(|target| target.node_id == target_node_id))
    }

    fn handle_push_payload(
        &self,
        source_node_id: &str,
        payload: MeshSyncPushPayload,
    ) -> LedgerResult<MeshSyncAckPayload> {
        if payload.from_node_id != source_node_id {
            return Err(SyncLedgerError::Runtime(format!(
                "sync push sender mismatch: frame={source_node_id}, payload={}",
                payload.from_node_id
            )));
        }
        let operation_ids = self.store_incoming_operations(source_node_id, payload.operations)?;
        if let Err(e) = self.apply_unapplied_inbox(128) {
            warn!("sync runtime: apply incoming operations failed: {}", e);
        }
        Ok(MeshSyncAckPayload {
            from_node_id: self.local_node_id.clone(),
            operation_ids,
        })
    }

    fn handle_ack_payload(
        &self,
        source_node_id: &str,
        payload: MeshSyncAckPayload,
    ) -> LedgerResult<()> {
        if payload.from_node_id != source_node_id {
            return Err(SyncLedgerError::Runtime(format!(
                "sync ack sender mismatch: frame={source_node_id}, payload={}",
                payload.from_node_id
            )));
        }
        let target = SyncTarget::new(source_node_id.to_string())?;
        for op_id in payload.operation_ids {
            self.ledger
                .mark_acknowledged(target.clone(), operation_id_from_wire(&op_id)?)?;
        }
        Ok(())
    }

    fn handle_pull_payload(
        &self,
        source_node_id: &str,
        payload: MeshSyncPullPayload,
    ) -> LedgerResult<MeshSyncPullResult> {
        if payload.from_node_id != source_node_id {
            return Err(SyncLedgerError::Runtime(format!(
                "sync pull sender mismatch: frame={source_node_id}, payload={}",
                payload.from_node_id
            )));
        }
        let partition_id = PartitionId::new(payload.partition_id.clone())?;
        let operations = self.ledger.get_operations(OperationQuery {
            partition_id: partition_id.clone(),
            from_sequence: Some(payload.from_sequence),
            to_sequence: None,
            limit: Some(payload.limit as usize),
        })?;
        if self.pull_needs_snapshot(&partition_id, payload.from_sequence, &operations)? {
            let snapshot = self
                .ledger
                .latest_snapshot(partition_id, None)?
                .ok_or_else(|| {
                    SyncLedgerError::Runtime(
                        "sync pull cannot be served contiguously and no snapshot exists"
                            .to_string(),
                    )
                })?;
            return self
                .build_snapshot_response_from_snapshot(
                    payload.partition_id,
                    snapshot,
                    true,
                    payload.limit,
                )
                .map(MeshSyncPullResult::Snapshot);
        }
        let mut wire = Vec::with_capacity(operations.len());
        for operation in operations {
            wire.push(operation_to_wire(&operation)?);
        }
        Ok(MeshSyncPullResult::Operations(
            MeshSyncPullResponsePayload {
                from_node_id: self.local_node_id.clone(),
                partition_id: payload.partition_id,
                from_sequence: payload.from_sequence,
                operations: wire,
            },
        ))
    }

    fn handle_pull_response_payload(
        &self,
        source_node_id: &str,
        payload: MeshSyncPullResponsePayload,
    ) -> LedgerResult<MeshSyncAckPayload> {
        if payload.from_node_id != source_node_id {
            return Err(SyncLedgerError::Runtime(format!(
                "sync pull response sender mismatch: frame={source_node_id}, payload={}",
                payload.from_node_id
            )));
        }
        validate_pull_response_wire(&payload)?;
        let response_partition = payload.partition_id.clone();
        let operation_ids = self.store_incoming_operations(source_node_id, payload.operations)?;
        self.clear_repair_request(source_node_id, &response_partition);
        if let Err(e) = self.apply_unapplied_inbox(128) {
            warn!("sync runtime: apply pulled operations failed: {}", e);
        }
        Ok(MeshSyncAckPayload {
            from_node_id: self.local_node_id.clone(),
            operation_ids,
        })
    }

    fn handle_snapshot_pull_payload(
        &self,
        source_node_id: &str,
        payload: MeshSyncSnapshotPullPayload,
    ) -> LedgerResult<MeshSyncSnapshotResponsePayload> {
        if payload.from_node_id != source_node_id {
            return Err(SyncLedgerError::Runtime(format!(
                "sync snapshot pull sender mismatch: frame={source_node_id}, payload={}",
                payload.from_node_id
            )));
        }
        let partition_id = PartitionId::new(payload.partition_id.clone())?;
        let snapshot_id = SnapshotId::new(payload.snapshot_id.clone())?;
        let snapshot =
            self.ledger
                .get_snapshot(partition_id.clone(), payload.up_to_sequence, snapshot_id)?;
        self.build_snapshot_response_from_snapshot(
            payload.partition_id,
            snapshot,
            payload.include_tail,
            payload.tail_limit,
        )
    }

    fn pull_needs_snapshot(
        &self,
        partition_id: &PartitionId,
        from_sequence: u64,
        operations: &[SyncOperation],
    ) -> LedgerResult<bool> {
        if operations
            .first()
            .is_some_and(|operation| operation.body.partition_sequence != from_sequence)
        {
            return Ok(true);
        }
        if operations.is_empty()
            && self
                .ledger
                .get_partition_head(partition_id.clone())?
                .is_some_and(|head| head.last_sequence >= from_sequence)
        {
            return Ok(self
                .ledger
                .latest_snapshot(partition_id.clone(), None)?
                .is_some_and(|snapshot| snapshot.up_to_sequence >= from_sequence));
        }
        Ok(false)
    }

    fn build_snapshot_response_from_snapshot(
        &self,
        partition_id: String,
        snapshot: SyncSnapshot,
        include_tail: bool,
        tail_limit: u32,
    ) -> LedgerResult<MeshSyncSnapshotResponsePayload> {
        verify_snapshot_signature(&snapshot)?;
        let store = SnapshotPackageStore::new(SnapshotPackageStore::default_root());
        let blob_bytes = store.get_sql_package(&snapshot)?;
        let operations_after_snapshot = if include_tail && tail_limit > 0 {
            self.ledger
                .get_operations(OperationQuery {
                    partition_id: snapshot.partition_id.clone(),
                    from_sequence: Some(snapshot.up_to_sequence.saturating_add(1)),
                    to_sequence: None,
                    limit: Some(tail_limit as usize),
                })?
                .iter()
                .map(operation_to_wire)
                .collect::<LedgerResult<Vec<_>>>()?
        } else {
            Vec::new()
        };
        Ok(MeshSyncSnapshotResponsePayload {
            from_node_id: self.local_node_id.clone(),
            partition_id,
            up_to_sequence: snapshot.up_to_sequence,
            snapshot_id: snapshot.snapshot_id.as_str().to_string(),
            snapshot_bytes: rmp_serde::to_vec_named(&snapshot)?,
            blob_bytes,
            operations_after_snapshot,
        })
    }

    fn build_snapshot_pull_payload(
        &self,
        partition_id: &str,
        up_to_sequence: u64,
        snapshot_id: &str,
        include_tail: bool,
        tail_limit: u32,
    ) -> LedgerResult<MeshSyncSnapshotPullPayload> {
        Ok(MeshSyncSnapshotPullPayload {
            from_node_id: self.local_node_id.clone(),
            partition_id: PartitionId::new(partition_id.to_string())?
                .as_str()
                .to_string(),
            up_to_sequence,
            snapshot_id: SnapshotId::new(snapshot_id.to_string())?
                .as_str()
                .to_string(),
            include_tail,
            tail_limit,
        })
    }

    fn handle_snapshot_response_payload(
        &self,
        source_node_id: &str,
        payload: MeshSyncSnapshotResponsePayload,
    ) -> LedgerResult<MeshSyncAckPayload> {
        if payload.from_node_id != source_node_id {
            return Err(SyncLedgerError::Runtime(format!(
                "sync snapshot response sender mismatch: frame={source_node_id}, payload={}",
                payload.from_node_id
            )));
        }
        let snapshot: SyncSnapshot = rmp_serde::from_slice(&payload.snapshot_bytes)?;
        if snapshot.partition_id.as_str() != payload.partition_id
            || snapshot.up_to_sequence != payload.up_to_sequence
            || snapshot.snapshot_id.as_str() != payload.snapshot_id
        {
            return Err(SyncLedgerError::Runtime(
                "sync snapshot response metadata mismatch".to_string(),
            ));
        }
        verify_snapshot_signature(&snapshot)?;
        validate_snapshot_tail_wire(&payload)?;
        let store = SnapshotPackageStore::new(SnapshotPackageStore::default_root());
        store.put_sql_package(&snapshot, &payload.blob_bytes)?;
        let tail_operations = payload
            .operations_after_snapshot
            .iter()
            .map(operation_from_wire)
            .collect::<LedgerResult<Vec<_>>>()?;
        SnapshotManager::new(self.ledger.as_ref()).restore_sql_from_package_parts(
            &snapshot,
            &payload.blob_bytes,
            &tail_operations,
        )?;
        let source_peer = PeerId::new(source_node_id.to_string())?;
        let snapshot_cursor = snapshot.last_operation_hash.map(|last_hash| PeerCursor {
            peer: source_peer,
            partition_id: snapshot.partition_id.clone(),
            last_sequence: snapshot.up_to_sequence,
            last_hash,
        });
        self.ledger.save_snapshot(snapshot)?;
        if let Some(cursor) = snapshot_cursor {
            self.ledger.save_peer_cursor(cursor)?;
        }
        self.clear_repair_request(source_node_id, &payload.partition_id);
        let operation_ids =
            self.store_incoming_operations(source_node_id, payload.operations_after_snapshot)?;
        if let Err(e) = self.apply_unapplied_inbox(128) {
            warn!("sync runtime: apply snapshot tail operations failed: {}", e);
        }
        Ok(MeshSyncAckPayload {
            from_node_id: self.local_node_id.clone(),
            operation_ids,
        })
    }

    fn store_incoming_operations(
        &self,
        source_node_id: &str,
        operations: Vec<MeshSyncOperationWire>,
    ) -> LedgerResult<Vec<Vec<u8>>> {
        let source = PeerId::new(source_node_id.to_string())?;
        let mut accepted = Vec::with_capacity(operations.len());
        let mut expected_sequences: HashMap<String, u64> = HashMap::new();
        for wire in operations {
            let operation = operation_from_wire(&wire)?;
            self.ensure_local_target_allowed(&operation)?;
            let partition_key = operation.body.partition_id.as_str().to_string();
            let expected_sequence = match expected_sequences.get(&partition_key).copied() {
                Some(sequence) => sequence,
                None => self.initial_expected_sequence(
                    source.clone(),
                    operation.body.partition_id.clone(),
                )?,
            };
            if operation.body.partition_sequence < expected_sequence {
                accepted.push(operation.op_id.as_bytes().to_vec());
                expected_sequences.insert(partition_key, expected_sequence);
                continue;
            }
            self.ensure_operation_follows_known_state(
                &source,
                &operation,
                &mut expected_sequences,
            )?;
            self.ledger.put_verified_in_inbox(
                source.clone(),
                operation.clone(),
                &HexNodeIdOperationVerifier,
            )?;
            self.ledger.save_peer_cursor(PeerCursor {
                peer: source.clone(),
                partition_id: operation.body.partition_id.clone(),
                last_sequence: operation.body.partition_sequence,
                last_hash: operation.operation_hash,
            })?;
            accepted.push(operation.op_id.as_bytes().to_vec());
        }
        Ok(accepted)
    }

    fn ensure_operation_follows_known_state(
        &self,
        source: &PeerId,
        operation: &SyncOperation,
        expected_sequences: &mut HashMap<String, u64>,
    ) -> LedgerResult<()> {
        let partition = operation.body.partition_id.clone();
        let partition_key = partition.as_str().to_string();
        let expected_sequence = match expected_sequences.get(&partition_key).copied() {
            Some(sequence) => sequence,
            None => {
                let sequence = self.initial_expected_sequence(source.clone(), partition.clone())?;
                expected_sequences.insert(partition_key.clone(), sequence);
                sequence
            }
        };
        if operation.body.partition_sequence != expected_sequence {
            if operation.body.partition_sequence > expected_sequence {
                self.queue_repair_request(
                    source.as_str(),
                    operation.body.partition_id.as_str(),
                    expected_sequence,
                );
            }
            return Err(SyncLedgerError::Runtime(format!(
                "incoming operation sequence gap for {}: expected {}, actual {}",
                operation.body.partition_id.as_str(),
                expected_sequence,
                operation.body.partition_sequence
            )));
        }
        if let Some(expected_previous_hash) =
            self.expected_previous_hash(source.clone(), partition, expected_sequence)?
        {
            if operation.body.prev_partition_hash != Some(expected_previous_hash) {
                self.queue_repair_request(
                    source.as_str(),
                    operation.body.partition_id.as_str(),
                    expected_sequence,
                );
                return Err(SyncLedgerError::HashChainMismatch {
                    partition: operation.body.partition_id.as_str().to_string(),
                    sequence: operation.body.partition_sequence,
                });
            }
        } else if operation.body.prev_partition_hash.is_some() {
            self.queue_repair_request(
                source.as_str(),
                operation.body.partition_id.as_str(),
                expected_sequence,
            );
            return Err(SyncLedgerError::HashChainMismatch {
                partition: operation.body.partition_id.as_str().to_string(),
                sequence: operation.body.partition_sequence,
            });
        }
        expected_sequences.insert(partition_key, expected_sequence.saturating_add(1));
        Ok(())
    }

    fn initial_expected_sequence(
        &self,
        source: PeerId,
        partition: PartitionId,
    ) -> LedgerResult<u64> {
        if let Some(cursor) = self.ledger.get_peer_cursor(source, partition.clone())? {
            return Ok(cursor.last_sequence.saturating_add(1));
        }
        Ok(self
            .ledger
            .latest_snapshot(partition, None)?
            .map_or(1, |snapshot| snapshot.up_to_sequence.saturating_add(1)))
    }

    fn expected_previous_hash(
        &self,
        source: PeerId,
        partition: PartitionId,
        expected_sequence: u64,
    ) -> LedgerResult<Option<[u8; 32]>> {
        if expected_sequence == 1 {
            return Ok(None);
        }
        if let Some(cursor) = self.ledger.get_peer_cursor(source, partition.clone())? {
            if cursor.last_sequence.saturating_add(1) == expected_sequence {
                return Ok(Some(cursor.last_hash));
            }
        }
        Ok(self
            .ledger
            .latest_snapshot(partition, Some(expected_sequence.saturating_sub(1)))?
            .and_then(|snapshot| {
                if snapshot.up_to_sequence.saturating_add(1) == expected_sequence {
                    snapshot.last_operation_hash
                } else {
                    None
                }
            }))
    }

    fn queue_repair_request(&self, peer_id: &str, partition_id: &str, from_sequence: u64) {
        let entry = match (
            PeerId::new(peer_id.to_string()),
            PartitionId::new(partition_id.to_string()),
        ) {
            (Ok(peer), Ok(partition_id)) => RepairQueueEntry {
                peer,
                partition_id,
                from_sequence,
                next_attempt_ms: now_ms(),
                retry_count: 0,
            },
            (Err(e), _) | (_, Err(e)) => {
                warn!("sync runtime: cannot queue repair request: {}", e);
                return;
            }
        };
        if let Err(e) = self.ledger.upsert_repair_request(entry) {
            warn!("sync runtime: repair request persist failed: {}", e);
        }
    }

    fn clear_repair_request(&self, peer_id: &str, partition_id: &str) {
        let result = PeerId::new(peer_id.to_string()).and_then(|peer| {
            PartitionId::new(partition_id.to_string())
                .and_then(|partition| self.ledger.remove_repair_request(peer, partition))
        });
        if let Err(e) = result {
            warn!("sync runtime: repair request clear failed: {}", e);
        }
    }

    fn apply_unapplied_inbox(&self, limit: usize) -> LedgerResult<usize> {
        let mut entries = self.ledger.list_unapplied_inbox(limit)?;
        entries.sort_by_key(|entry| blob_apply_priority(&entry.operation));
        let mut applied = 0usize;
        for entry in entries {
            if entry.operation.body.resource_type == "core.blob" {
                match apply_blob_operation(&entry.operation) {
                    Ok(BlobApplyOutcome::Applied) => {
                        self.ledger
                            .mark_inbox_applied(entry.source.clone(), entry.operation.op_id)?;
                        applied += 1;
                    }
                    Ok(BlobApplyOutcome::Pending) => {}
                    Err(e) => {
                        self.ledger.mark_inbox_conflicted(
                            entry.source.clone(),
                            entry.operation.op_id,
                            e.to_string(),
                        )?;
                        warn!(
                            "sync runtime: incoming blob operation {} recorded as conflict: {}",
                            entry.operation.op_id.to_hex(),
                            e
                        );
                    }
                }
                continue;
            }
            if entry.operation.body.addon_id == crate::sync::core_registry::CORE_SYNC_ADDON_ID {
                match crate::sync::core_materializer::apply_core_operation(
                    &self.db,
                    &entry.operation,
                ) {
                    Ok(_) => {
                        self.ledger
                            .mark_inbox_applied(entry.source.clone(), entry.operation.op_id)?;
                        applied += 1;
                    }
                    Err(e) => {
                        self.ledger.mark_inbox_conflicted(
                            entry.source.clone(),
                            entry.operation.op_id,
                            e.to_string(),
                        )?;
                        warn!(
                            "sync runtime: incoming core operation {} recorded as conflict: {}",
                            entry.operation.op_id.to_hex(),
                            e
                        );
                    }
                }
                continue;
            }
            if entry.operation.body.resource_type == "addon.kv" {
                match apply_kv_operation(&self.db, &entry.operation) {
                    Ok(_) => {
                        self.ledger
                            .mark_inbox_applied(entry.source.clone(), entry.operation.op_id)?;
                        applied += 1;
                    }
                    Err(e) => {
                        self.ledger.mark_inbox_conflicted(
                            entry.source.clone(),
                            entry.operation.op_id,
                            e.to_string(),
                        )?;
                        warn!(
                            "sync runtime: incoming kv operation {} recorded as conflict: {}",
                            entry.operation.op_id.to_hex(),
                            e
                        );
                    }
                }
                continue;
            }
            let capture = capture_from_operation(&entry.operation)?;
            match crate::addon::storage_sql_exec::apply_replicated_write(
                &capture,
                entry.operation.op_id,
            ) {
                Ok(_) => {
                    self.ledger
                        .mark_inbox_applied(entry.source.clone(), entry.operation.op_id)?;
                    applied += 1;
                }
                Err(e) => {
                    if let Err(record_error) = crate::addon::storage_sql_exec::record_sync_conflict(
                        &capture,
                        entry.operation.op_id,
                        entry.source.as_str(),
                        &e,
                    ) {
                        return Err(SyncLedgerError::Runtime(record_error.to_string()));
                    }
                    self.ledger.mark_inbox_conflicted(
                        entry.source.clone(),
                        entry.operation.op_id,
                        e.to_string(),
                    )?;
                    warn!(
                        "sync runtime: incoming operation {} recorded as conflict: {}",
                        entry.operation.op_id.to_hex(),
                        e
                    );
                }
            }
        }
        Ok(applied)
    }

    fn resolve_addon_sync_conflict(
        &self,
        org_id: &str,
        addon_id: &str,
        operation_id: OperationId,
        resolution: SyncConflictResolution,
    ) -> LedgerResult<SyncConflictResolveResult> {
        let operation_hex = operation_id.to_hex();
        let conflict = crate::addon::storage_sql_exec::list_sync_conflicts(
            org_id,
            addon_id,
            Some("open"),
            1_000,
        )
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?
        .into_iter()
        .find(|row| row.operation_id == operation_hex)
        .ok_or_else(|| SyncLedgerError::Runtime("open sync conflict not found".to_string()))?;
        let source = PeerId::new(conflict.source_node_id)?;
        let result = crate::addon::storage_sql_exec::resolve_sync_conflict(
            org_id,
            addon_id,
            operation_id,
            resolution,
        )
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        if result.status == "resolved" {
            self.ledger.mark_inbox_applied(source, operation_id)?;
        }
        Ok(result)
    }

    fn ensure_local_target_allowed(&self, operation: &SyncOperation) -> LedgerResult<()> {
        let targets = repository::list_sync_targets_for_resource(
            &self.db,
            &operation.body.org_id,
            &operation.body.addon_id,
            &operation.body.resource_type,
            &operation.body.resource_id,
        )
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        let allowed = targets
            .iter()
            .any(|target| target.node_id == self.local_node_id);
        if allowed {
            Ok(())
        } else {
            Err(SyncLedgerError::Runtime(format!(
                "local node is not a sync target for {}/{}/{}",
                operation.body.addon_id, operation.body.resource_type, operation.body.resource_id
            )))
        }
    }

    fn build_core_operation(
        &self,
        capture: &crate::sync::core_capture::CoreWriteCapture,
    ) -> LedgerResult<NewSyncOperation> {
        let payload = rmp_serde::to_vec_named(capture)?;
        let payload_hash = sha256(&payload);
        let policy_epoch = repository::get_sync_permission_epoch(&self.db, &capture.org_id)
            .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        let descriptor = crate::sync::core_registry::descriptor_for_table(&capture.table_name)
            .ok_or_else(|| {
                SyncLedgerError::Runtime(format!("unknown core sync table: {}", capture.table_name))
            })?;
        let mut changed_fields = capture.changed_fields.clone();
        changed_fields.insert(
            "capture_id".to_string(),
            FieldValue::String(capture.capture_id.clone()),
        );
        Ok(NewSyncOperation {
            org_id: capture.org_id.clone(),
            partition_id: descriptor.partition_id(&capture.org_id, capture.actor_user_id)?,
            addon_id: crate::sync::core_registry::CORE_SYNC_ADDON_ID.to_string(),
            resource_type: capture.resource_type.clone(),
            resource_id: capture.resource_id.clone(),
            table_name: capture.table_name.clone(),
            primary_key: capture.primary_key.clone(),
            action: match capture.action {
                SqlWriteAction::Insert => ActionType::Insert,
                SqlWriteAction::Update => ActionType::Update,
                SqlWriteAction::Delete => ActionType::Delete,
            },
            changed_fields,
            before_hash: None,
            after_hash: Some(payload_hash),
            actor_user_id: capture
                .actor_user_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "system".to_string()),
            actor_device_id: self.local_node_id.clone(),
            actor_node_id: self.local_node_id.clone(),
            hlc_timestamp: HybridLogicalTimestamp {
                wall_time_ms: capture.created_at_ms,
                logical: 0,
                node_id: self.local_node_id.clone(),
            },
            payload_hash,
            acl_snapshot_hash: sha256(
                format!(
                    "{}:{}:{}:{}",
                    capture.org_id,
                    crate::sync::core_registry::CORE_SYNC_ADDON_ID,
                    capture.resource_type,
                    capture.resource_id
                )
                .as_bytes(),
            ),
            policy_epoch,
            encryption_info: None,
        })
    }

    fn build_operation(&self, capture: &SqlWriteCapture) -> LedgerResult<NewSyncOperation> {
        let payload = rmp_serde::to_vec_named(capture)?;
        let payload_hash = sha256(&payload);
        let policy_epoch = repository::get_sync_permission_epoch(&self.db, &capture.org_id)
            .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        let mut changed_fields = BTreeMap::new();
        changed_fields.insert("sql".to_string(), FieldValue::String(capture.query.clone()));
        changed_fields.insert(
            "params_json".to_string(),
            FieldValue::String(JsonValue::Array(capture.params.clone()).to_string()),
        );
        changed_fields.insert(
            "rows_affected".to_string(),
            FieldValue::U64(capture.rows_affected),
        );
        changed_fields.insert(
            "last_insert_id".to_string(),
            FieldValue::I64(capture.last_insert_id),
        );
        changed_fields.insert(
            "capture_id".to_string(),
            FieldValue::String(capture.capture_id.clone()),
        );
        Ok(NewSyncOperation {
            org_id: capture.org_id.clone(),
            partition_id: PartitionId::new(format!(
                "addon/{}/{}/{}",
                capture.addon_id, capture.resource_type, capture.resource_id
            ))?,
            addon_id: capture.addon_id.clone(),
            resource_type: capture.resource_type.clone(),
            resource_id: capture.resource_id.clone(),
            table_name: capture.table_name.clone(),
            primary_key: capture.resource_id.clone(),
            action: match capture.action {
                SqlWriteAction::Insert => ActionType::Insert,
                SqlWriteAction::Update => ActionType::Update,
                SqlWriteAction::Delete => ActionType::Delete,
            },
            changed_fields,
            before_hash: None,
            after_hash: Some(payload_hash),
            actor_user_id: capture
                .actor_user_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "system".to_string()),
            actor_device_id: self.local_node_id.clone(),
            actor_node_id: self.local_node_id.clone(),
            hlc_timestamp: HybridLogicalTimestamp {
                wall_time_ms: capture.created_at_ms,
                logical: 0,
                node_id: self.local_node_id.clone(),
            },
            payload_hash,
            acl_snapshot_hash: sha256(
                format!(
                    "{}:{}:{}:{}",
                    capture.org_id, capture.addon_id, capture.resource_type, capture.resource_id
                )
                .as_bytes(),
            ),
            policy_epoch,
            encryption_info: None,
        })
    }

    fn append_blob_operations(
        &self,
        capture: &crate::sync::blob_capture::BlobWriteCapture,
    ) -> LedgerResult<SqlCaptureRecordResult> {
        crate::sync::storage_monitor::ensure_large_blob_allowed(capture.size_bytes)?;
        validate_blob_sha(&capture.sha256)?;
        let metadata = std::fs::metadata(&capture.file_path)
            .map_err(|e| SyncLedgerError::Runtime(format!("blob metadata: {e}")))?;
        if metadata.len() != capture.size_bytes {
            return Err(SyncLedgerError::Runtime(format!(
                "blob size mismatch for {}",
                capture.sha256
            )));
        }
        let chunk_count = capture
            .size_bytes
            .div_ceil(BLOB_SYNC_CHUNK_SIZE as u64)
            .max(1);
        let policy_epoch = repository::get_sync_permission_epoch(&self.db, &capture.org_id)
            .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        let mut file = std::fs::File::open(&capture.file_path)
            .map_err(|e| SyncLedgerError::Runtime(format!("blob open: {e}")))?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; BLOB_SYNC_CHUNK_SIZE];
        let mut total_read = 0u64;
        loop {
            use std::io::Read;
            let read = file
                .read(&mut buffer)
                .map_err(|e| SyncLedgerError::Runtime(format!("blob read: {e}")))?;
            if read == 0 {
                break;
            }
            let chunk = &buffer[..read];
            hasher.update(chunk);
            total_read = total_read.saturating_add(read as u64);
        }
        if total_read != capture.size_bytes || hex::encode(hasher.finalize()) != capture.sha256 {
            return Err(SyncLedgerError::Runtime(format!(
                "blob sha256 mismatch for {}",
                capture.sha256
            )));
        }
        {
            use std::io::{Seek, SeekFrom};
            file.seek(SeekFrom::Start(0))
                .map_err(|e| SyncLedgerError::Runtime(format!("blob seek: {e}")))?;
        }
        let mut queued_targets = 0usize;
        let mut chunk_index = 0u64;
        loop {
            use std::io::Read;
            let read = file
                .read(&mut buffer)
                .map_err(|e| SyncLedgerError::Runtime(format!("blob read: {e}")))?;
            if read == 0 {
                break;
            }
            let op = self.build_blob_chunk_operation(
                capture,
                policy_epoch,
                chunk_index,
                chunk_count,
                &buffer[..read],
            )?;
            let append = self.ledger.append_operation(op, &self.signer)?;
            queued_targets += self.queue_targets_for_resource(
                &capture.org_id,
                "core",
                "core.blob",
                &capture.sha256,
                append.op_id,
            )?;
            chunk_index = chunk_index.saturating_add(1);
        }
        if capture.size_bytes == 0 {
            let op = self.build_blob_chunk_operation(capture, policy_epoch, 0, chunk_count, &[])?;
            let append = self.ledger.append_operation(op, &self.signer)?;
            queued_targets += self.queue_targets_for_resource(
                &capture.org_id,
                "core",
                "core.blob",
                &capture.sha256,
                append.op_id,
            )?;
        }
        let manifest = self.build_blob_manifest_operation(capture, policy_epoch, chunk_count)?;
        let append = self.ledger.append_operation(manifest, &self.signer)?;
        queued_targets += self.queue_targets_for_resource(
            &capture.org_id,
            "core",
            "core.blob",
            &capture.sha256,
            append.op_id,
        )?;
        Ok(SqlCaptureRecordResult {
            op_id: append.op_id,
            queued_targets,
        })
    }

    fn build_blob_manifest_operation(
        &self,
        capture: &crate::sync::blob_capture::BlobWriteCapture,
        policy_epoch: u64,
        chunk_count: u64,
    ) -> LedgerResult<NewSyncOperation> {
        let payload = rmp_serde::to_vec_named(capture)?;
        let payload_hash = sha256(&payload);
        let mut changed_fields = BTreeMap::new();
        changed_fields.insert(
            "blob_id".to_string(),
            FieldValue::String(capture.blob_id.clone()),
        );
        changed_fields.insert(
            "sha256".to_string(),
            FieldValue::String(capture.sha256.clone()),
        );
        changed_fields.insert("mime".to_string(), FieldValue::String(capture.mime.clone()));
        changed_fields.insert(
            "size_bytes".to_string(),
            FieldValue::U64(capture.size_bytes),
        );
        changed_fields.insert(
            "chunk_size".to_string(),
            FieldValue::U64(BLOB_SYNC_CHUNK_SIZE as u64),
        );
        changed_fields.insert("chunk_count".to_string(), FieldValue::U64(chunk_count));
        Ok(NewSyncOperation {
            org_id: capture.org_id.clone(),
            partition_id: PartitionId::new(format!(
                "core/blob/{}",
                &capture.sha256[..2.min(capture.sha256.len())]
            ))?,
            addon_id: "core".to_string(),
            resource_type: "core.blob".to_string(),
            resource_id: capture.sha256.clone(),
            table_name: "blob_store".to_string(),
            primary_key: capture.sha256.clone(),
            action: ActionType::Insert,
            changed_fields,
            before_hash: None,
            after_hash: Some(hex_sha_to_bytes(&capture.sha256)?),
            actor_user_id: capture
                .actor_user_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "system".to_string()),
            actor_device_id: self.local_node_id.clone(),
            actor_node_id: self.local_node_id.clone(),
            hlc_timestamp: HybridLogicalTimestamp {
                wall_time_ms: capture.created_at_ms,
                logical: 0,
                node_id: self.local_node_id.clone(),
            },
            payload_hash,
            acl_snapshot_hash: sha256(
                format!("{}:{}:{}", capture.org_id, "core.blob", capture.sha256).as_bytes(),
            ),
            policy_epoch,
            encryption_info: None,
        })
    }

    fn build_blob_chunk_operation(
        &self,
        capture: &crate::sync::blob_capture::BlobWriteCapture,
        policy_epoch: u64,
        chunk_index: u64,
        chunk_count: u64,
        chunk: &[u8],
    ) -> LedgerResult<NewSyncOperation> {
        let chunk_hash = sha256(chunk);
        let mut payload = Vec::with_capacity(capture.sha256.len() + chunk.len() + 32);
        payload.extend_from_slice(capture.sha256.as_bytes());
        payload.extend_from_slice(&chunk_index.to_le_bytes());
        payload.extend_from_slice(chunk);
        let payload_hash = sha256(&payload);
        let mut changed_fields = BTreeMap::new();
        changed_fields.insert(
            "sha256".to_string(),
            FieldValue::String(capture.sha256.clone()),
        );
        changed_fields.insert("chunk_index".to_string(), FieldValue::U64(chunk_index));
        changed_fields.insert("chunk_count".to_string(), FieldValue::U64(chunk_count));
        changed_fields.insert(
            "chunk_size".to_string(),
            FieldValue::U64(chunk.len() as u64),
        );
        changed_fields.insert(
            "chunk_sha256".to_string(),
            FieldValue::String(hex::encode(chunk_hash)),
        );
        changed_fields.insert("bytes".to_string(), FieldValue::Bytes(chunk.to_vec()));
        Ok(NewSyncOperation {
            org_id: capture.org_id.clone(),
            partition_id: PartitionId::new(format!(
                "core/blob/{}",
                &capture.sha256[..2.min(capture.sha256.len())]
            ))?,
            addon_id: "core".to_string(),
            resource_type: "core.blob".to_string(),
            resource_id: capture.sha256.clone(),
            table_name: "blob_store_chunks".to_string(),
            primary_key: format!("{}:{chunk_index}", capture.sha256),
            action: ActionType::Insert,
            changed_fields,
            before_hash: None,
            after_hash: Some(chunk_hash),
            actor_user_id: capture
                .actor_user_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "system".to_string()),
            actor_device_id: self.local_node_id.clone(),
            actor_node_id: self.local_node_id.clone(),
            hlc_timestamp: HybridLogicalTimestamp {
                wall_time_ms: capture.created_at_ms,
                logical: chunk_index as u32,
                node_id: self.local_node_id.clone(),
            },
            payload_hash,
            acl_snapshot_hash: sha256(
                format!("{}:{}:{}", capture.org_id, "core.blob", capture.sha256).as_bytes(),
            ),
            policy_epoch,
            encryption_info: None,
        })
    }

    fn build_kv_operation(&self, capture: &KvWriteCapture) -> LedgerResult<NewSyncOperation> {
        let payload = rmp_serde::to_vec_named(capture)?;
        let payload_hash = sha256(&payload);
        let policy_epoch = repository::get_sync_permission_epoch(&self.db, &capture.org_id)
            .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        let resource_id = kv_resource_id(&capture.instance_id, &capture.key);
        let mut changed_fields = BTreeMap::new();
        changed_fields.insert(
            "instance_id".to_string(),
            FieldValue::String(capture.instance_id.clone()),
        );
        changed_fields.insert("key".to_string(), FieldValue::String(capture.key.clone()));
        if let Some(value) = &capture.value {
            changed_fields.insert("value".to_string(), FieldValue::Bytes(value.clone()));
            changed_fields.insert(
                "value_size_bytes".to_string(),
                FieldValue::U64(value.len() as u64),
            );
        }
        Ok(NewSyncOperation {
            org_id: capture.org_id.clone(),
            partition_id: PartitionId::new(format!(
                "addon/{}/kv/{}",
                capture.addon_id, capture.instance_id
            ))?,
            addon_id: capture.addon_id.clone(),
            resource_type: "addon.kv".to_string(),
            resource_id: resource_id.clone(),
            table_name: "addon_storage".to_string(),
            primary_key: resource_id.clone(),
            action: if capture.value.is_some() {
                ActionType::Update
            } else {
                ActionType::Delete
            },
            changed_fields,
            before_hash: None,
            after_hash: capture.value.as_ref().map(|value| sha256(value)),
            actor_user_id: capture
                .actor_user_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "system".to_string()),
            actor_device_id: self.local_node_id.clone(),
            actor_node_id: self.local_node_id.clone(),
            hlc_timestamp: HybridLogicalTimestamp {
                wall_time_ms: capture.created_at_ms,
                logical: 0,
                node_id: self.local_node_id.clone(),
            },
            payload_hash,
            acl_snapshot_hash: sha256(
                format!(
                    "{}:{}:{}:{}",
                    capture.org_id, capture.addon_id, "addon.kv", resource_id
                )
                .as_bytes(),
            ),
            policy_epoch,
            encryption_info: None,
        })
    }
}

fn operation_to_wire(operation: &SyncOperation) -> LedgerResult<MeshSyncOperationWire> {
    Ok(MeshSyncOperationWire {
        op_id: operation.op_id.as_bytes().to_vec(),
        partition_id: operation.body.partition_id.as_str().to_string(),
        partition_sequence: operation.body.partition_sequence,
        operation: rmp_serde::to_vec_named(operation)?,
    })
}

fn operation_from_wire(wire: &MeshSyncOperationWire) -> LedgerResult<SyncOperation> {
    let operation: SyncOperation = rmp_serde::from_slice(&wire.operation)?;
    let op_id = operation_id_from_wire(&wire.op_id)?;
    if operation.op_id != op_id
        || operation.body.partition_id.as_str() != wire.partition_id
        || operation.body.partition_sequence != wire.partition_sequence
    {
        return Err(SyncLedgerError::Runtime(
            "sync operation wire metadata mismatch".to_string(),
        ));
    }
    Ok(operation)
}

fn validate_snapshot_tail_wire(payload: &MeshSyncSnapshotResponsePayload) -> LedgerResult<()> {
    let mut expected_sequence = payload.up_to_sequence.saturating_add(1);
    for wire in &payload.operations_after_snapshot {
        if wire.partition_id != payload.partition_id {
            return Err(SyncLedgerError::Runtime(
                "sync snapshot response tail partition mismatch".to_string(),
            ));
        }
        if wire.partition_sequence != expected_sequence {
            return Err(SyncLedgerError::Runtime(format!(
                "sync snapshot response tail sequence gap: expected {expected_sequence}, actual {}",
                wire.partition_sequence
            )));
        }
        expected_sequence = expected_sequence.saturating_add(1);
    }
    Ok(())
}

fn validate_pull_response_wire(payload: &MeshSyncPullResponsePayload) -> LedgerResult<()> {
    let mut expected_sequence = payload.from_sequence;
    for wire in &payload.operations {
        if wire.partition_id != payload.partition_id {
            return Err(SyncLedgerError::Runtime(
                "sync pull response partition mismatch".to_string(),
            ));
        }
        if wire.partition_sequence != expected_sequence {
            return Err(SyncLedgerError::Runtime(format!(
                "sync pull response sequence gap: expected {expected_sequence}, actual {}",
                wire.partition_sequence
            )));
        }
        expected_sequence = expected_sequence.saturating_add(1);
    }
    Ok(())
}

fn apply_kv_operation(db: &DbPool, operation: &SyncOperation) -> LedgerResult<usize> {
    if operation.body.table_name != "addon_storage" {
        return Err(SyncLedgerError::Runtime(format!(
            "kv operation has invalid table: {}",
            operation.body.table_name
        )));
    }
    let instance_id = field_string(operation, "instance_id")?;
    let key = field_string(operation, "key")?;
    let conn = db
        .lock()
        .map_err(|e| SyncLedgerError::Runtime(format!("Blad blokady bazy: {e}")))?;
    match operation.body.action {
        ActionType::Update | ActionType::Insert => {
            let value = field_bytes(operation, "value")?;
            let value_size = value.len() as i64;
            conn.execute(
                "INSERT INTO addon_storage \
                 (addon_id, instance_id, storage_key, storage_value, value_size_bytes, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, datetime('now')) \
                 ON CONFLICT(addon_id, instance_id, storage_key) DO UPDATE SET \
                    storage_value = excluded.storage_value, \
                    value_size_bytes = excluded.value_size_bytes, \
                    updated_at = excluded.updated_at",
                rusqlite::params![
                    &operation.body.addon_id,
                    instance_id,
                    key,
                    value,
                    value_size
                ],
            )
            .map_err(|e| SyncLedgerError::Runtime(e.to_string()))
        }
        ActionType::Delete => conn
            .execute(
                "DELETE FROM addon_storage \
                 WHERE addon_id = ?1 AND instance_id = ?2 AND storage_key = ?3",
                rusqlite::params![&operation.body.addon_id, instance_id, key],
            )
            .map_err(|e| SyncLedgerError::Runtime(e.to_string())),
    }
}

enum BlobApplyOutcome {
    Applied,
    Pending,
}

fn blob_apply_priority(operation: &SyncOperation) -> u8 {
    if operation.body.resource_type != "core.blob" {
        return 1;
    }
    match operation.body.table_name.as_str() {
        "blob_store_chunks" => 0,
        "blob_store" => 2,
        _ => 1,
    }
}

fn apply_blob_operation(operation: &SyncOperation) -> LedgerResult<BlobApplyOutcome> {
    match operation.body.table_name.as_str() {
        "blob_store_chunks" => apply_blob_chunk_operation(operation),
        "blob_store" => apply_blob_manifest_operation(operation),
        table => Err(SyncLedgerError::Runtime(format!(
            "blob operation has invalid table: {table}"
        ))),
    }
}

fn apply_blob_chunk_operation(operation: &SyncOperation) -> LedgerResult<BlobApplyOutcome> {
    let sha = field_string(operation, "sha256")?;
    validate_blob_sha(&sha)?;
    let chunk_index = field_u64(operation, "chunk_index")?;
    let chunk_size = field_u64(operation, "chunk_size")?;
    let chunk_sha = field_string(operation, "chunk_sha256")?;
    validate_blob_sha(&chunk_sha)?;
    let bytes = field_bytes(operation, "bytes")?;
    if bytes.len() as u64 != chunk_size {
        return Err(SyncLedgerError::Runtime(format!(
            "blob chunk size mismatch for {sha}:{chunk_index}"
        )));
    }
    if hex::encode(sha256(&bytes)) != chunk_sha {
        return Err(SyncLedgerError::Runtime(format!(
            "blob chunk sha mismatch for {sha}:{chunk_index}"
        )));
    }
    let path = blob_chunk_path(&sha, chunk_index)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| SyncLedgerError::Runtime(format!("blob chunk dir: {e}")))?;
    }
    if path.is_file() {
        let existing = std::fs::read(&path)
            .map_err(|e| SyncLedgerError::Runtime(format!("blob chunk read: {e}")))?;
        if hex::encode(sha256(&existing)) == chunk_sha {
            return Ok(BlobApplyOutcome::Applied);
        }
    }
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
    std::fs::write(&tmp, &bytes)
        .map_err(|e| SyncLedgerError::Runtime(format!("blob chunk write: {e}")))?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| SyncLedgerError::Runtime(format!("blob chunk rename: {e}")))?;
    Ok(BlobApplyOutcome::Applied)
}

fn apply_blob_manifest_operation(operation: &SyncOperation) -> LedgerResult<BlobApplyOutcome> {
    let sha = field_string(operation, "sha256")?;
    validate_blob_sha(&sha)?;
    let size_bytes = field_u64(operation, "size_bytes")?;
    let chunk_count = field_u64(operation, "chunk_count")?;
    let path = blob_path_for_sha(&sha)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| SyncLedgerError::Runtime(format!("blob dir: {e}")))?;
    }
    if path.is_file() {
        let chunk_dir = blob_chunk_dir(&sha)?;
        let _ = std::fs::remove_dir_all(chunk_dir);
        return Ok(BlobApplyOutcome::Applied);
    }
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
    let mut file = std::fs::File::create(&tmp)
        .map_err(|e| SyncLedgerError::Runtime(format!("blob create: {e}")))?;
    let mut hasher = Sha256::new();
    let mut written = 0u64;
    for chunk_index in 0..chunk_count {
        let chunk_path = blob_chunk_path(&sha, chunk_index)?;
        if !chunk_path.is_file() {
            let _ = std::fs::remove_file(&tmp);
            return Ok(BlobApplyOutcome::Pending);
        }
        let chunk = std::fs::read(&chunk_path)
            .map_err(|e| SyncLedgerError::Runtime(format!("blob chunk read: {e}")))?;
        use std::io::Write;
        file.write_all(&chunk)
            .map_err(|e| SyncLedgerError::Runtime(format!("blob write: {e}")))?;
        hasher.update(&chunk);
        written = written.saturating_add(chunk.len() as u64);
    }
    file.sync_all()
        .map_err(|e| SyncLedgerError::Runtime(format!("blob fsync: {e}")))?;
    drop(file);
    if written != size_bytes {
        let _ = std::fs::remove_file(&tmp);
        return Err(SyncLedgerError::Runtime(format!(
            "blob operation size mismatch for {sha}"
        )));
    }
    if hex::encode(hasher.finalize()) != sha {
        let _ = std::fs::remove_file(&tmp);
        return Err(SyncLedgerError::Runtime(format!(
            "blob operation sha mismatch for {sha}"
        )));
    }
    std::fs::rename(&tmp, &path)
        .map_err(|e| SyncLedgerError::Runtime(format!("blob rename: {e}")))?;
    let chunk_dir = blob_chunk_dir(&sha)?;
    let _ = std::fs::remove_dir_all(chunk_dir);
    Ok(BlobApplyOutcome::Applied)
}

fn blob_path_for_sha(sha: &str) -> LedgerResult<std::path::PathBuf> {
    validate_blob_sha(sha)?;
    Ok(crate::paths::tentaflow_home()
        .join("blobs")
        .join(&sha[0..2])
        .join(&sha[2..4])
        .join(format!("{sha}.bin")))
}

fn blob_chunk_dir(sha: &str) -> LedgerResult<std::path::PathBuf> {
    validate_blob_sha(sha)?;
    Ok(crate::paths::tentaflow_home()
        .join("sync")
        .join("blob-chunks")
        .join(&sha[0..2])
        .join(&sha[2..4])
        .join(sha))
}

fn blob_chunk_path(sha: &str, chunk_index: u64) -> LedgerResult<std::path::PathBuf> {
    Ok(blob_chunk_dir(sha)?.join(format!("{chunk_index:016}.part")))
}

fn validate_blob_sha(sha: &str) -> LedgerResult<()> {
    if sha.len() != 64
        || !sha
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(SyncLedgerError::Runtime(format!("invalid blob sha: {sha}")));
    }
    Ok(())
}

fn hex_sha_to_bytes(sha: &str) -> LedgerResult<[u8; 32]> {
    validate_blob_sha(sha)?;
    let bytes = hex::decode(sha)
        .map_err(|_| SyncLedgerError::Runtime(format!("invalid blob sha: {sha}")))?;
    bytes
        .try_into()
        .map_err(|_| SyncLedgerError::Runtime(format!("invalid blob sha: {sha}")))
}

pub(crate) fn capture_from_operation(operation: &SyncOperation) -> LedgerResult<SqlWriteCapture> {
    let query = field_string(operation, "sql")?;
    let params_json = field_string(operation, "params_json")?;
    let params = serde_json::from_str::<Vec<JsonValue>>(&params_json)
        .map_err(|e| SyncLedgerError::Runtime(format!("sync operation params_json: {e}")))?;
    let rows_affected = field_u64(operation, "rows_affected")?;
    let last_insert_id = field_i64(operation, "last_insert_id")?;
    let capture_id = field_string(operation, "capture_id")?;
    Ok(SqlWriteCapture {
        capture_id,
        org_id: operation.body.org_id.clone(),
        addon_id: operation.body.addon_id.clone(),
        table_name: operation.body.table_name.clone(),
        action: match operation.body.action {
            ActionType::Insert => SqlWriteAction::Insert,
            ActionType::Update => SqlWriteAction::Update,
            ActionType::Delete => SqlWriteAction::Delete,
        },
        resource_type: operation.body.resource_type.clone(),
        resource_id: operation.body.resource_id.clone(),
        query,
        params,
        rows_affected,
        last_insert_id,
        actor_user_id: operation.body.actor_user_id.parse::<i64>().ok(),
        created_at_ms: operation.body.hlc_timestamp.wall_time_ms,
    })
}

fn field_bytes(operation: &SyncOperation, key: &str) -> LedgerResult<Vec<u8>> {
    match operation.body.changed_fields.get(key) {
        Some(FieldValue::Bytes(value)) => Ok(value.clone()),
        _ => Err(SyncLedgerError::Runtime(format!(
            "sync operation missing bytes field: {key}"
        ))),
    }
}

fn field_string(operation: &SyncOperation, key: &str) -> LedgerResult<String> {
    match operation.body.changed_fields.get(key) {
        Some(FieldValue::String(value)) => Ok(value.clone()),
        _ => Err(SyncLedgerError::Runtime(format!(
            "sync operation missing string field: {key}"
        ))),
    }
}

fn field_u64(operation: &SyncOperation, key: &str) -> LedgerResult<u64> {
    match operation.body.changed_fields.get(key) {
        Some(FieldValue::U64(value)) => Ok(*value),
        _ => Err(SyncLedgerError::Runtime(format!(
            "sync operation missing u64 field: {key}"
        ))),
    }
}

fn field_i64(operation: &SyncOperation, key: &str) -> LedgerResult<i64> {
    match operation.body.changed_fields.get(key) {
        Some(FieldValue::I64(value)) => Ok(*value),
        _ => Err(SyncLedgerError::Runtime(format!(
            "sync operation missing i64 field: {key}"
        ))),
    }
}

fn operation_id_from_wire(bytes: &[u8]) -> LedgerResult<OperationId> {
    let hash: [u8; 32] = bytes
        .try_into()
        .map_err(|_| SyncLedgerError::InvalidOperationIdHex {
            value: hex::encode(bytes),
        })?;
    Ok(OperationId::from_hash(hash))
}

fn repair_backoff_ms(retry_count: u32) -> i64 {
    let shift = retry_count.min(6);
    1_000_i64.saturating_mul(1_i64 << shift)
}

impl SyncOperationSigner for RuntimeSigner {
    fn node_id(&self) -> &str {
        &self.node_id
    }

    fn sign_operation(&self, message: &[u8]) -> LedgerResult<Vec<u8>> {
        Ok(self.security.sign(message))
    }
}

fn kv_resource_id(instance_id: &str, key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(instance_id.as_bytes());
    hasher.update([0]);
    hasher.update(key.as_bytes());
    format!("{}:{}", instance_id, hex::encode(hasher.finalize()))
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;
    use crate::sync::ledger::CompactionPolicy;
    use crate::sync::snapshot::SnapshotBuildRequest;
    use rusqlite::Connection;
    use std::sync::Mutex;

    struct RuntimeHarness {
        runtime: SyncRuntime,
        _ledger_dir: tempfile::TempDir,
    }

    fn make_db() -> DbPool {
        let conn = Connection::open_in_memory().expect("open db");
        migrations::run(&conn).expect("run migrations");
        Arc::new(Mutex::new(conn))
    }

    fn make_security(db: DbPool, key_seed: u8) -> Arc<MeshSecurity> {
        let cipher = Arc::new(crate::crypto::SettingsCipher::new(&[key_seed; 32]));
        Arc::new(MeshSecurity::new(db, cipher).expect("mesh security"))
    }

    fn make_runtime(key_seed: u8) -> RuntimeHarness {
        let ledger_dir = tempfile::tempdir().expect("ledger dir");
        let db = make_db();
        let security = make_security(db.clone(), key_seed);
        let local_node_id = security.ed25519_public_key_hex();
        let ledger = Arc::new(FjallSyncLedgerStore::open(ledger_dir.path()).expect("ledger"));
        RuntimeHarness {
            runtime: SyncRuntime {
                db,
                ledger,
                signer: RuntimeSigner {
                    node_id: local_node_id.clone(),
                    security,
                },
                local_node_id,
            },
            _ledger_dir: ledger_dir,
        }
    }

    fn seed_authority_target(db: &DbPool, addon_id: &str, target_node_id: &str) {
        seed_authority_target_for_resource(db, addon_id, "person", target_node_id);
    }

    fn seed_authority_target_for_resource(
        db: &DbPool,
        addon_id: &str,
        resource_type: &str,
        target_node_id: &str,
    ) {
        repository::upsert_sync_node_identity(
            db,
            target_node_id,
            "pub",
            "ed25519",
            "Authority",
            "authority",
            "trusted",
            None,
            "authority",
        )
        .expect("sync node");
        repository::upsert_sync_policy(
            db,
            &format!("policy-{addon_id}"),
            "org-default",
            addon_id,
            Some(resource_type),
            None,
            "authority_write",
            Some(target_node_id),
            None,
            true,
        )
        .expect("sync policy");
    }

    fn kv_capture(addon_id: &str, instance_id: &str, key: &str, value: &[u8]) -> KvWriteCapture {
        KvWriteCapture::new(
            "org-default",
            addon_id,
            instance_id,
            key,
            Some(value.to_vec()),
            Some(7),
        )
    }

    fn seed_core_authority_target(db: &DbPool, resource_type: &str, target_node_id: &str) {
        repository::upsert_sync_node_identity(
            db,
            target_node_id,
            "pub",
            "ed25519",
            "Authority",
            "authority",
            "trusted",
            None,
            "authority",
        )
        .expect("sync node");
        repository::upsert_sync_policy(
            db,
            &format!("policy-core-{resource_type}"),
            "org-default",
            crate::sync::core_registry::CORE_SYNC_ADDON_ID,
            Some(resource_type),
            None,
            "authority_write",
            Some(target_node_id),
            None,
            true,
        )
        .expect("sync policy");
    }

    fn open_contacts_table(addon_id: &str) {
        let pool = crate::addon::storage_sql::open_addon_db("org-default", addon_id)
            .expect("open addon db");
        let conn = pool.get().expect("addon conn");
        conn.execute(
            "CREATE TABLE IF NOT EXISTS contacts (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
            [],
        )
        .expect("create contacts");
    }

    fn capture(addon_id: &str, resource_id: &str, name: &str) -> SqlWriteCapture {
        SqlWriteCapture {
            capture_id: format!("{addon_id}-{resource_id}-{name}"),
            org_id: "org-default".to_string(),
            addon_id: addon_id.to_string(),
            table_name: "contacts".to_string(),
            action: SqlWriteAction::Insert,
            resource_type: "person".to_string(),
            resource_id: resource_id.to_string(),
            query: "INSERT INTO contacts (id, name) VALUES (?1, ?2)".to_string(),
            params: vec![JsonValue::from(1), JsonValue::String(name.to_string())],
            rows_affected: 1,
            last_insert_id: 1,
            actor_user_id: Some(7),
            created_at_ms: now_ms(),
        }
    }

    fn update_capture(addon_id: &str, resource_id: &str, name: &str) -> SqlWriteCapture {
        SqlWriteCapture {
            capture_id: format!("{addon_id}-{resource_id}-{name}"),
            org_id: "org-default".to_string(),
            addon_id: addon_id.to_string(),
            table_name: "contacts".to_string(),
            action: SqlWriteAction::Update,
            resource_type: "person".to_string(),
            resource_id: resource_id.to_string(),
            query: "UPDATE contacts SET name = ?1 WHERE id = ?2".to_string(),
            params: vec![JsonValue::String(name.to_string()), JsonValue::from(1)],
            rows_affected: 1,
            last_insert_id: 1,
            actor_user_id: Some(7),
            created_at_ms: now_ms(),
        }
    }

    fn core_flow_capture(
        resource_id: &str,
        name: &str,
    ) -> crate::sync::core_capture::CoreWriteCapture {
        let mut fields = BTreeMap::new();
        fields.insert("name".to_string(), FieldValue::String(name.to_string()));
        crate::sync::core_capture::CoreWriteCapture::new(
            crate::sync::core_registry::CoreSyncResourceKind::Flow,
            "org-default",
            resource_id,
            SqlWriteAction::Insert,
            fields,
            Some(7),
        )
    }

    fn complete_core_flow_capture(
        resource_id: &str,
        name: &str,
    ) -> crate::sync::core_capture::CoreWriteCapture {
        let mut capture = core_flow_capture(resource_id, name);
        capture
            .changed_fields
            .insert("is_default".to_string(), FieldValue::Bool(false));
        capture.changed_fields.insert(
            "flow_json".to_string(),
            FieldValue::String(r#"{"nodes":[]}"#.to_string()),
        );
        capture.changed_fields.insert(
            "status".to_string(),
            FieldValue::String("active".to_string()),
        );
        capture
    }

    fn core_flow_update_capture(
        resource_id: &str,
        name: &str,
    ) -> crate::sync::core_capture::CoreWriteCapture {
        let mut fields = BTreeMap::new();
        fields.insert("name".to_string(), FieldValue::String(name.to_string()));
        fields.insert(
            "flow_json".to_string(),
            FieldValue::String(r#"{"nodes":[{"id":"repaired"}]}"#.to_string()),
        );
        fields.insert(
            "status".to_string(),
            FieldValue::String("active".to_string()),
        );
        crate::sync::core_capture::CoreWriteCapture::new(
            crate::sync::core_registry::CoreSyncResourceKind::Flow,
            "org-default",
            resource_id,
            SqlWriteAction::Update,
            fields,
            Some(7),
        )
    }

    fn with_tmp_home<F: FnOnce()>(f: F) {
        let _guard = crate::addon::fs_sandbox::test_home_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());
        f();
        if let Some(p) = prev {
            std::env::set_var("HOME", p);
        } else {
            std::env::remove_var("HOME");
        }
    }

    #[test]
    fn core_capture_records_binary_operation_and_outbox() {
        with_tmp_home(|| {
            let source = make_runtime(21);
            let receiver = make_runtime(22);
            seed_core_authority_target(
                &source.runtime.db,
                "core.flow",
                &receiver.runtime.local_node_id,
            );

            let result = source
                .runtime
                .record_core_capture(core_flow_capture("flow-1", "Flow 1"))
                .expect("record core capture");
            let operation = source
                .runtime
                .ledger
                .get_operation(result.op_id)
                .expect("operation");

            assert_eq!(operation.body.addon_id, "core");
            assert_eq!(operation.body.resource_type, "core.flow");
            assert_eq!(
                operation.body.partition_id.as_str(),
                "core/org/org-default/flows"
            );
            assert_eq!(
                operation.body.changed_fields.get("name"),
                Some(&FieldValue::String("Flow 1".to_string()))
            );
            assert!(!operation.body.changed_fields.contains_key("params_json"));
            assert_eq!(
                operation.body.policy_epoch,
                repository::get_sync_permission_epoch(
                    &source.runtime.db,
                    crate::services::org::DEFAULT_ORG_ID
                )
                .expect("policy epoch")
            );
            assert_eq!(result.queued_targets, 1);
        });
    }

    #[test]
    fn core_inbox_materializer_applies_flow_insert() {
        with_tmp_home(|| {
            let source = make_runtime(23);
            let receiver = make_runtime(24);
            let mut capture = core_flow_capture("41", "Remote Flow");
            capture.changed_fields.insert(
                "description".to_string(),
                FieldValue::String("Opis".to_string()),
            );
            capture
                .changed_fields
                .insert("is_default".to_string(), FieldValue::Bool(false));
            capture.changed_fields.insert(
                "service_type".to_string(),
                FieldValue::String("chat".to_string()),
            );
            capture.changed_fields.insert(
                "flow_json".to_string(),
                FieldValue::String(r#"{"nodes":[]}"#.to_string()),
            );
            capture.changed_fields.insert(
                "status".to_string(),
                FieldValue::String("active".to_string()),
            );
            let result = source
                .runtime
                .record_core_capture(capture)
                .expect("record core capture");
            let operation = source
                .runtime
                .ledger
                .get_operation(result.op_id)
                .expect("operation");

            crate::sync::core_materializer::apply_core_operation(&receiver.runtime.db, &operation)
                .expect("apply core operation");
            let flow = repository::get_flow(&receiver.runtime.db, 41)
                .expect("get flow")
                .expect("flow");

            assert_eq!(flow.name, "Remote Flow");
            assert_eq!(flow.service_type.as_deref(), Some("chat"));
            assert_eq!(flow.status, "active");
            assert_eq!(flow.flow_json, r#"{"nodes":[]}"#);
        });
    }

    #[test]
    fn core_materializer_merges_duplicate_flow_insert_by_field() {
        with_tmp_home(|| {
            let source = make_runtime(27);
            let receiver = make_runtime(28);
            {
                let conn = receiver.runtime.db.lock().expect("db lock");
                conn.execute(
                    "INSERT INTO flows (id, name, flow_json, status) VALUES (43, 'Local Flow', '{\"nodes\":[]}', 'draft')",
                    [],
                )
                .expect("seed flow");
            }
            let mut capture = core_flow_capture("43", "Merged Flow");
            capture
                .changed_fields
                .insert("is_default".to_string(), FieldValue::Bool(false));
            capture.changed_fields.insert(
                "flow_json".to_string(),
                FieldValue::String(r#"{"nodes":[{"id":"remote"}]}"#.to_string()),
            );
            capture.changed_fields.insert(
                "status".to_string(),
                FieldValue::String("active".to_string()),
            );
            let result = source
                .runtime
                .record_core_capture(capture)
                .expect("record core capture");
            let operation = source
                .runtime
                .ledger
                .get_operation(result.op_id)
                .expect("operation");

            crate::sync::core_materializer::apply_core_operation(&receiver.runtime.db, &operation)
                .expect("merge core operation");
            let flow = repository::get_flow(&receiver.runtime.db, 43)
                .expect("get flow")
                .expect("flow");

            assert_eq!(flow.name, "Merged Flow");
            assert_eq!(flow.status, "active");
            assert_eq!(flow.flow_json, r#"{"nodes":[{"id":"remote"}]}"#);
        });
    }

    #[test]
    fn core_push_materializes_flow_on_receiver() {
        with_tmp_home(|| {
            let source = make_runtime(25);
            let receiver = make_runtime(26);
            seed_core_authority_target(
                &source.runtime.db,
                "core.flow",
                &receiver.runtime.local_node_id,
            );
            seed_core_authority_target(
                &receiver.runtime.db,
                "core.flow",
                &receiver.runtime.local_node_id,
            );
            let mut capture = core_flow_capture("42", "Pushed Flow");
            capture
                .changed_fields
                .insert("is_default".to_string(), FieldValue::Bool(false));
            capture.changed_fields.insert(
                "flow_json".to_string(),
                FieldValue::String(r#"{"nodes":[{"id":"n1"}]}"#.to_string()),
            );
            capture.changed_fields.insert(
                "status".to_string(),
                FieldValue::String("active".to_string()),
            );
            let result = source
                .runtime
                .record_core_capture(capture)
                .expect("record core capture");
            let push = source
                .runtime
                .build_push_payload_for_target(&receiver.runtime.local_node_id, 16)
                .expect("push")
                .expect("pending push");
            let ack = receiver
                .runtime
                .handle_push_payload(&source.runtime.local_node_id, push)
                .expect("handle push");
            source
                .runtime
                .handle_ack_payload(&receiver.runtime.local_node_id, ack)
                .expect("ack");

            let flow = repository::get_flow(&receiver.runtime.db, 42)
                .expect("get flow")
                .expect("flow");
            let outbox = source
                .runtime
                .ledger
                .get_outbox_entry(
                    SyncTarget::new(receiver.runtime.local_node_id.clone()).expect("target"),
                    result.op_id,
                )
                .expect("outbox");

            assert_eq!(flow.name, "Pushed Flow");
            assert_eq!(flow.status, "active");
            assert!(outbox.acknowledged);
        });
    }

    #[test]
    fn addon_kv_push_materializes_storage_on_receiver() {
        with_tmp_home(|| {
            let source = make_runtime(51);
            let receiver = make_runtime(52);
            seed_authority_target_for_resource(
                &source.runtime.db,
                "kv-addon",
                "addon.kv",
                &receiver.runtime.local_node_id,
            );
            seed_authority_target_for_resource(
                &receiver.runtime.db,
                "kv-addon",
                "addon.kv",
                &receiver.runtime.local_node_id,
            );

            let result = source
                .runtime
                .record_kv_capture(kv_capture("kv-addon", "inst-1", "settings/theme", b"dark"))
                .expect("record kv capture");
            let operation = source
                .runtime
                .ledger
                .get_operation(result.op_id)
                .expect("operation");
            assert_eq!(operation.body.resource_type, "addon.kv");
            assert_eq!(
                operation.body.changed_fields.get("value"),
                Some(&FieldValue::Bytes(b"dark".to_vec()))
            );

            let push = source
                .runtime
                .build_push_payload_for_target(&receiver.runtime.local_node_id, 16)
                .expect("push")
                .expect("pending push");
            let ack = receiver
                .runtime
                .handle_push_payload(&source.runtime.local_node_id, push)
                .expect("handle push");
            source
                .runtime
                .handle_ack_payload(&receiver.runtime.local_node_id, ack)
                .expect("ack");

            let stored: Vec<u8> = receiver
                .runtime
                .db
                .lock()
                .expect("db lock")
                .query_row(
                    "SELECT storage_value FROM addon_storage \
                     WHERE addon_id = ?1 AND instance_id = ?2 AND storage_key = ?3",
                    rusqlite::params!["kv-addon", "inst-1", "settings/theme"],
                    |row| row.get(0),
                )
                .expect("stored value");
            let outbox = source
                .runtime
                .ledger
                .get_outbox_entry(
                    SyncTarget::new(receiver.runtime.local_node_id.clone()).expect("target"),
                    result.op_id,
                )
                .expect("outbox");

            assert_eq!(stored, b"dark");
            assert!(outbox.acknowledged);
        });
    }

    #[test]
    fn addon_kv_delete_removes_storage_on_receiver() {
        with_tmp_home(|| {
            let source = make_runtime(53);
            let receiver = make_runtime(54);
            seed_authority_target_for_resource(
                &source.runtime.db,
                "kv-addon-delete",
                "addon.kv",
                &receiver.runtime.local_node_id,
            );
            seed_authority_target_for_resource(
                &receiver.runtime.db,
                "kv-addon-delete",
                "addon.kv",
                &receiver.runtime.local_node_id,
            );
            {
                let conn = receiver.runtime.db.lock().expect("db lock");
                conn.execute(
                    "INSERT INTO addon_storage \
                     (addon_id, instance_id, storage_key, storage_value, value_size_bytes) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![
                        "kv-addon-delete",
                        "inst-1",
                        "settings/theme",
                        b"dark".to_vec(),
                        4
                    ],
                )
                .expect("seed kv");
            }

            let mut capture = kv_capture("kv-addon-delete", "inst-1", "settings/theme", b"unused");
            capture.value = None;
            source
                .runtime
                .record_kv_capture(capture)
                .expect("record kv delete");
            let push = source
                .runtime
                .build_push_payload_for_target(&receiver.runtime.local_node_id, 16)
                .expect("push")
                .expect("pending push");
            receiver
                .runtime
                .handle_push_payload(&source.runtime.local_node_id, push)
                .expect("handle push");

            let count: i64 = receiver
                .runtime
                .db
                .lock()
                .expect("db lock")
                .query_row(
                    "SELECT COUNT(*) FROM addon_storage \
                     WHERE addon_id = ?1 AND instance_id = ?2 AND storage_key = ?3",
                    rusqlite::params!["kv-addon-delete", "inst-1", "settings/theme"],
                    |row| row.get(0),
                )
                .expect("count");

            assert_eq!(count, 0);
        });
    }

    #[test]
    fn core_blob_push_materializes_file_on_receiver() {
        with_tmp_home(|| {
            let source = make_runtime(55);
            let receiver = make_runtime(56);
            seed_core_authority_target(
                &source.runtime.db,
                "core.blob",
                &receiver.runtime.local_node_id,
            );
            seed_core_authority_target(
                &receiver.runtime.db,
                "core.blob",
                &receiver.runtime.local_node_id,
            );
            let bytes = b"blob payload".to_vec();
            let sha = hex::encode(sha256(&bytes));
            let blob_source_dir = tempfile::tempdir().expect("blob dir");
            let blob_source_path = blob_source_dir.path().join("payload.bin");
            std::fs::write(&blob_source_path, &bytes).expect("blob write");
            let capture = crate::sync::blob_capture::BlobWriteCapture::new(
                "org-default",
                "blob-1",
                &sha,
                "application/octet-stream",
                bytes.len() as u64,
                blob_source_path.to_string_lossy().to_string(),
                Some(7),
            );

            let result = source
                .runtime
                .record_blob_capture(capture)
                .expect("record blob capture");
            let operation = source
                .runtime
                .ledger
                .get_operation(result.op_id)
                .expect("operation");
            assert_eq!(operation.body.resource_type, "core.blob");
            assert_eq!(operation.body.table_name, "blob_store");
            assert_eq!(
                operation.body.changed_fields.get("chunk_count"),
                Some(&FieldValue::U64(1))
            );
            assert!(!operation.body.changed_fields.contains_key("bytes"));

            let target_path = blob_path_for_sha(&sha).expect("blob path");
            if target_path.exists() {
                std::fs::remove_file(&target_path).expect("remove preexisting blob");
            }
            let push = source
                .runtime
                .build_push_payload_for_target(&receiver.runtime.local_node_id, 16)
                .expect("push")
                .expect("pending push");
            let ack = receiver
                .runtime
                .handle_push_payload(&source.runtime.local_node_id, push)
                .expect("handle push");
            source
                .runtime
                .handle_ack_payload(&receiver.runtime.local_node_id, ack)
                .expect("ack");

            let stored = std::fs::read(target_path).expect("stored blob");
            assert_eq!(stored, bytes);
        });
    }

    #[test]
    fn core_blob_push_materializes_chunked_file_on_receiver() {
        with_tmp_home(|| {
            let source = make_runtime(57);
            let receiver = make_runtime(58);
            seed_core_authority_target(
                &source.runtime.db,
                "core.blob",
                &receiver.runtime.local_node_id,
            );
            seed_core_authority_target(
                &receiver.runtime.db,
                "core.blob",
                &receiver.runtime.local_node_id,
            );
            let mut bytes = Vec::with_capacity(BLOB_SYNC_CHUNK_SIZE * 2 + 17);
            for idx in 0..(BLOB_SYNC_CHUNK_SIZE * 2 + 17) {
                bytes.push((idx % 251) as u8);
            }
            let sha = hex::encode(sha256(&bytes));
            let blob_source_dir = tempfile::tempdir().expect("blob dir");
            let blob_source_path = blob_source_dir.path().join("payload.bin");
            std::fs::write(&blob_source_path, &bytes).expect("blob write");
            let capture = crate::sync::blob_capture::BlobWriteCapture::new(
                "org-default",
                "blob-large",
                &sha,
                "application/octet-stream",
                bytes.len() as u64,
                blob_source_path.to_string_lossy().to_string(),
                Some(7),
            );

            let result = source
                .runtime
                .record_blob_capture(capture)
                .expect("record blob capture");
            let manifest = source
                .runtime
                .ledger
                .get_operation(result.op_id)
                .expect("manifest operation");
            assert_eq!(manifest.body.table_name, "blob_store");
            assert_eq!(
                manifest.body.changed_fields.get("chunk_count"),
                Some(&FieldValue::U64(3))
            );
            assert!(!manifest.body.changed_fields.contains_key("bytes"));

            let push = source
                .runtime
                .build_push_payload_for_target(&receiver.runtime.local_node_id, 16)
                .expect("push")
                .expect("pending push");
            assert_eq!(push.operations.len(), 4);
            let chunk_ops = push
                .operations
                .iter()
                .filter(|wire| {
                    let operation: SyncOperation =
                        rmp_serde::from_slice(&wire.operation).expect("wire operation");
                    operation.body.table_name == "blob_store_chunks"
                })
                .count();
            assert_eq!(chunk_ops, 3);
            let ack = receiver
                .runtime
                .handle_push_payload(&source.runtime.local_node_id, push)
                .expect("handle push");
            source
                .runtime
                .handle_ack_payload(&receiver.runtime.local_node_id, ack)
                .expect("ack");

            let target_path = blob_path_for_sha(&sha).expect("blob path");
            let stored = std::fs::read(target_path).expect("stored blob");
            assert_eq!(stored, bytes);
            assert!(!blob_chunk_dir(&sha).expect("chunk dir").exists());
        });
    }

    #[test]
    fn core_outbox_targets_only_nodes_with_resource_access() {
        with_tmp_home(|| {
            let source = make_runtime(29);
            let receiver_allowed = make_runtime(30);
            let receiver_denied = make_runtime(31);
            let allowed_user_id = repository::create_user_account(
                &source.runtime.db,
                "allowed-user",
                "hash",
                "Allowed User",
                "allowed@example.com",
            )
            .expect("allowed user");
            let denied_user_id = repository::create_user_account(
                &source.runtime.db,
                "denied-user",
                "hash",
                "Denied User",
                "denied@example.com",
            )
            .expect("denied user");
            for (node_id, user_id, display_name) in [
                (
                    receiver_allowed.runtime.local_node_id.as_str(),
                    allowed_user_id,
                    "Allowed Node",
                ),
                (
                    receiver_denied.runtime.local_node_id.as_str(),
                    denied_user_id,
                    "Denied Node",
                ),
            ] {
                repository::upsert_sync_node_identity(
                    &source.runtime.db,
                    node_id,
                    "pub",
                    "ed25519",
                    display_name,
                    "laptop",
                    "trusted",
                    Some(user_id),
                    "standard",
                )
                .expect("sync node");
                repository::assign_node_to_user(
                    &source.runtime.db,
                    node_id,
                    user_id,
                    "primary",
                    None,
                )
                .expect("assign node");
            }
            repository::upsert_sync_policy(
                &source.runtime.db,
                "policy-core-flow-permission",
                "org-default",
                crate::sync::core_registry::CORE_SYNC_ADDON_ID,
                Some("core.flow"),
                None,
                "replicated_by_permission",
                None,
                None,
                true,
            )
            .expect("sync policy");
            repository::upsert_sync_resource_acl(
                &source.runtime.db,
                "org-default",
                crate::sync::core_registry::CORE_SYNC_ADDON_ID,
                "core.flow",
                "44",
                Some(allowed_user_id),
                Some(allowed_user_id),
                None,
                None,
                "assigned",
            )
            .expect("resource acl");

            let result = source
                .runtime
                .record_core_capture(complete_core_flow_capture("44", "Selective Flow"))
                .expect("record core capture");
            let allowed_push = source
                .runtime
                .build_push_payload_for_target(&receiver_allowed.runtime.local_node_id, 16)
                .expect("allowed push");
            let denied_push = source
                .runtime
                .build_push_payload_for_target(&receiver_denied.runtime.local_node_id, 16)
                .expect("denied push");

            assert!(allowed_push.is_some());
            assert!(denied_push.is_none());
            assert_eq!(result.queued_targets, 1);
        });
    }

    #[test]
    fn core_outbox_drops_pending_entry_after_permission_revocation() {
        with_tmp_home(|| {
            let source = make_runtime(34);
            let receiver_allowed = make_runtime(35);
            let receiver_new_owner = make_runtime(36);
            let allowed_user_id = repository::create_user_account(
                &source.runtime.db,
                "revoked-user",
                "hash",
                "Revoked User",
                "revoked@example.com",
            )
            .expect("revoked user");
            let new_owner_id = repository::create_user_account(
                &source.runtime.db,
                "new-owner",
                "hash",
                "New Owner",
                "new-owner@example.com",
            )
            .expect("new owner");
            for (node_id, user_id, display_name) in [
                (
                    receiver_allowed.runtime.local_node_id.as_str(),
                    allowed_user_id,
                    "Revoked Node",
                ),
                (
                    receiver_new_owner.runtime.local_node_id.as_str(),
                    new_owner_id,
                    "New Owner Node",
                ),
            ] {
                repository::upsert_sync_node_identity(
                    &source.runtime.db,
                    node_id,
                    "pub",
                    "ed25519",
                    display_name,
                    "laptop",
                    "trusted",
                    Some(user_id),
                    "standard",
                )
                .expect("sync node");
                repository::assign_node_to_user(
                    &source.runtime.db,
                    node_id,
                    user_id,
                    "primary",
                    None,
                )
                .expect("assign node");
            }
            repository::upsert_sync_policy(
                &source.runtime.db,
                "policy-core-flow-revoke",
                "org-default",
                crate::sync::core_registry::CORE_SYNC_ADDON_ID,
                Some("core.flow"),
                None,
                "replicated_by_permission",
                None,
                None,
                true,
            )
            .expect("sync policy");
            repository::upsert_sync_resource_acl(
                &source.runtime.db,
                "org-default",
                crate::sync::core_registry::CORE_SYNC_ADDON_ID,
                "core.flow",
                "46",
                Some(allowed_user_id),
                Some(allowed_user_id),
                None,
                None,
                "assigned",
            )
            .expect("initial acl");
            let result = source
                .runtime
                .record_core_capture(complete_core_flow_capture("46", "Revoked Flow"))
                .expect("record core capture");
            assert_eq!(result.queued_targets, 1);

            repository::upsert_sync_resource_acl(
                &source.runtime.db,
                "org-default",
                crate::sync::core_registry::CORE_SYNC_ADDON_ID,
                "core.flow",
                "46",
                Some(new_owner_id),
                Some(new_owner_id),
                None,
                None,
                "assigned",
            )
            .expect("revoked acl");
            let old_target_push = source
                .runtime
                .build_push_payload_for_target(&receiver_allowed.runtime.local_node_id, 16)
                .expect("old target push");
            let outbox = source
                .runtime
                .ledger
                .get_outbox_entry(
                    SyncTarget::new(receiver_allowed.runtime.local_node_id.clone())
                        .expect("target"),
                    result.op_id,
                )
                .expect("outbox");

            assert!(old_target_push.is_none());
            assert!(outbox.acknowledged);
        });
    }

    #[test]
    fn core_outbox_targets_org_admin_node_without_resource_acl() {
        with_tmp_home(|| {
            let source = make_runtime(32);
            let receiver = make_runtime(33);
            let admin_user_id = repository::create_user_account(
                &source.runtime.db,
                "admin-user",
                "hash",
                "Admin User",
                "admin@example.com",
            )
            .expect("admin user");
            repository::set_user_role(&source.runtime.db, admin_user_id, "admin")
                .expect("admin role");
            repository::upsert_sync_node_identity(
                &source.runtime.db,
                &receiver.runtime.local_node_id,
                "pub",
                "ed25519",
                "Admin Node",
                "laptop",
                "trusted",
                Some(admin_user_id),
                "standard",
            )
            .expect("sync node");
            repository::assign_node_to_user(
                &source.runtime.db,
                &receiver.runtime.local_node_id,
                admin_user_id,
                "primary",
                None,
            )
            .expect("assign node");
            repository::upsert_sync_policy(
                &source.runtime.db,
                "policy-core-flow-admin",
                "org-default",
                crate::sync::core_registry::CORE_SYNC_ADDON_ID,
                Some("core.flow"),
                None,
                "replicated_by_permission",
                None,
                None,
                true,
            )
            .expect("sync policy");

            let result = source
                .runtime
                .record_core_capture(complete_core_flow_capture("45", "Admin Flow"))
                .expect("record core capture");
            let push = source
                .runtime
                .build_push_payload_for_target(&receiver.runtime.local_node_id, 16)
                .expect("push");

            assert!(push.is_some());
            assert_eq!(result.queued_targets, 1);
        });
    }

    #[test]
    fn offline_outbox_push_is_acknowledged_after_reconnect() {
        with_tmp_home(|| {
            let source = make_runtime(11);
            let receiver = make_runtime(12);
            let addon_id = "sync-runtime-offline";
            seed_authority_target(
                &source.runtime.db,
                addon_id,
                &receiver.runtime.local_node_id,
            );
            seed_authority_target(
                &receiver.runtime.db,
                addon_id,
                &receiver.runtime.local_node_id,
            );
            open_contacts_table(addon_id);

            let result = source
                .runtime
                .record_sql_capture(capture(addon_id, "person-1", "Ewa"))
                .expect("record");
            let target = SyncTarget::new(receiver.runtime.local_node_id.clone()).expect("target");

            let push = source
                .runtime
                .build_push_payload_for_target(&receiver.runtime.local_node_id, 16)
                .expect("push")
                .expect("pending push");
            let ack = receiver
                .runtime
                .handle_push_payload(&source.runtime.local_node_id, push)
                .expect("handle push");
            source
                .runtime
                .handle_ack_payload(&receiver.runtime.local_node_id, ack)
                .expect("ack");

            let entry = source
                .runtime
                .ledger
                .get_outbox_entry(target, result.op_id)
                .expect("outbox entry");
            assert!(entry.acknowledged);
        });
    }

    #[test]
    fn conflict_accept_remote_marks_inbox_applied() {
        with_tmp_home(|| {
            let source = make_runtime(41);
            let receiver = make_runtime(42);
            let addon_id = "sync-runtime-conflict";
            seed_authority_target(
                &source.runtime.db,
                addon_id,
                &receiver.runtime.local_node_id,
            );
            seed_authority_target(
                &receiver.runtime.db,
                addon_id,
                &receiver.runtime.local_node_id,
            );
            open_contacts_table(addon_id);
            {
                let pool = crate::addon::storage_sql::open_addon_db("org-default", addon_id)
                    .expect("open addon db");
                let conn = pool.get().expect("conn");
                conn.execute("INSERT INTO contacts (id, name) VALUES (1, 'Local')", [])
                    .expect("insert local");
            }

            let result = source
                .runtime
                .record_sql_capture(capture(addon_id, "person-1", "Remote"))
                .expect("record");
            let push = source
                .runtime
                .build_push_payload_for_target(&receiver.runtime.local_node_id, 16)
                .expect("push")
                .expect("pending push");
            receiver
                .runtime
                .handle_push_payload(&source.runtime.local_node_id, push)
                .expect("handle push");

            let resolved = receiver
                .runtime
                .resolve_addon_sync_conflict(
                    "org-default",
                    addon_id,
                    result.op_id,
                    SyncConflictResolution::AcceptRemote,
                )
                .expect("resolve");

            assert_eq!(resolved.status, "resolved");
            let entry = receiver
                .runtime
                .ledger
                .get_inbox_entry(
                    PeerId::new(source.runtime.local_node_id.clone()).expect("peer"),
                    result.op_id,
                )
                .expect("inbox");
            assert!(entry.applied);
            assert!(!entry.conflicted);
            let pool = crate::addon::storage_sql::open_addon_db("org-default", addon_id)
                .expect("open addon db");
            let conn = pool.get().expect("conn");
            let name: String = conn
                .query_row("SELECT name FROM contacts WHERE id = 1", [], |row| {
                    row.get(0)
                })
                .expect("name");
            assert_eq!(name, "Remote");
        });
    }

    #[test]
    fn missing_sequence_queues_repair_pull_from_gap() {
        with_tmp_home(|| {
            let source = make_runtime(21);
            let receiver = make_runtime(22);
            let addon_id = "sync-runtime-repair";
            seed_authority_target(
                &source.runtime.db,
                addon_id,
                &receiver.runtime.local_node_id,
            );
            seed_authority_target(
                &receiver.runtime.db,
                addon_id,
                &receiver.runtime.local_node_id,
            );
            open_contacts_table(addon_id);

            source
                .runtime
                .record_sql_capture(capture(addon_id, "person-1", "Anna"))
                .expect("record first");
            let second = source
                .runtime
                .record_sql_capture(update_capture(addon_id, "person-1", "Anna Nowak"))
                .expect("record second");
            let second_operation = source
                .runtime
                .ledger
                .get_operation(second.op_id)
                .expect("second operation");
            let payload = MeshSyncPullResponsePayload {
                from_node_id: source.runtime.local_node_id.clone(),
                partition_id: second_operation.body.partition_id.as_str().to_string(),
                from_sequence: second_operation.body.partition_sequence,
                operations: vec![operation_to_wire(&second_operation).expect("wire")],
            };

            let err = receiver
                .runtime
                .handle_pull_response_payload(&source.runtime.local_node_id, payload)
                .expect_err("gap must fail");
            assert!(matches!(err, SyncLedgerError::Runtime(_)));

            let pulls = receiver
                .runtime
                .build_repair_pull_payloads_for_peer(&source.runtime.local_node_id, 8, 64)
                .expect("repair pulls");
            assert_eq!(pulls.len(), 1);
            assert_eq!(pulls[0].from_sequence, 1);
        });
    }

    #[test]
    fn repair_pull_response_materializes_missing_core_flow_operations() {
        with_tmp_home(|| {
            let source = make_runtime(61);
            let receiver = make_runtime(62);
            seed_core_authority_target(
                &source.runtime.db,
                "core.flow",
                &receiver.runtime.local_node_id,
            );
            seed_core_authority_target(
                &receiver.runtime.db,
                "core.flow",
                &receiver.runtime.local_node_id,
            );

            let insert = source
                .runtime
                .record_core_capture(complete_core_flow_capture("61", "Initial Flow"))
                .expect("record insert");
            let update = source
                .runtime
                .record_core_capture(core_flow_update_capture("61", "Repaired Flow"))
                .expect("record update");
            let update_operation = source
                .runtime
                .ledger
                .get_operation(update.op_id)
                .expect("update operation");
            let partition = update_operation.body.partition_id.as_str().to_string();
            let gap_payload = MeshSyncPullResponsePayload {
                from_node_id: source.runtime.local_node_id.clone(),
                partition_id: partition.clone(),
                from_sequence: update_operation.body.partition_sequence,
                operations: vec![operation_to_wire(&update_operation).expect("wire update")],
            };

            receiver
                .runtime
                .handle_pull_response_payload(&source.runtime.local_node_id, gap_payload)
                .expect_err("missing prefix must queue repair");
            let pulls = receiver
                .runtime
                .build_repair_pull_payloads_for_peer(&source.runtime.local_node_id, 8, 64)
                .expect("repair pulls");
            assert_eq!(pulls.len(), 1);
            assert_eq!(pulls[0].from_sequence, 1);

            let response = source
                .runtime
                .handle_pull_payload(&receiver.runtime.local_node_id, pulls[0].clone())
                .expect("repair response");
            let MeshSyncPullResult::Operations(payload) = response else {
                panic!("expected repair operations response");
            };
            assert_eq!(payload.operations.len(), 2);
            let ack = receiver
                .runtime
                .handle_pull_response_payload(&source.runtime.local_node_id, payload)
                .expect("handle repair response");
            source
                .runtime
                .handle_ack_payload(&receiver.runtime.local_node_id, ack)
                .expect("ack repair");

            let flow = repository::get_flow(&receiver.runtime.db, 61)
                .expect("get flow")
                .expect("flow");
            let target = SyncTarget::new(receiver.runtime.local_node_id.clone()).expect("target");
            let insert_outbox = source
                .runtime
                .ledger
                .get_outbox_entry(target.clone(), insert.op_id)
                .expect("insert outbox");
            let update_outbox = source
                .runtime
                .ledger
                .get_outbox_entry(target, update.op_id)
                .expect("update outbox");

            assert_eq!(flow.name, "Repaired Flow");
            assert_eq!(flow.flow_json, r#"{"nodes":[{"id":"repaired"}]}"#);
            assert!(insert_outbox.acknowledged);
            assert!(update_outbox.acknowledged);
        });
    }

    #[test]
    fn compacted_prefix_is_served_as_snapshot_response() {
        with_tmp_home(|| {
            let source = make_runtime(31);
            let receiver = make_runtime(32);
            let addon_id = "sync-runtime-snapshot";
            seed_authority_target(
                &source.runtime.db,
                addon_id,
                &receiver.runtime.local_node_id,
            );
            seed_authority_target(
                &receiver.runtime.db,
                addon_id,
                &receiver.runtime.local_node_id,
            );
            open_contacts_table(addon_id);

            let first = source
                .runtime
                .record_sql_capture(capture(addon_id, "person-1", "Ola"))
                .expect("record first");
            let second = source
                .runtime
                .record_sql_capture(update_capture(addon_id, "person-1", "Ola Kowalska"))
                .expect("record second");
            let partition = source
                .runtime
                .ledger
                .get_operation(second.op_id)
                .expect("second operation")
                .body
                .partition_id;
            let package_store = SnapshotPackageStore::new(SnapshotPackageStore::default_root());
            let snapshot = SnapshotManager::new(source.runtime.ledger.as_ref())
                .build_sql_package_and_persist(
                    SnapshotBuildRequest {
                        partition_id: partition.clone(),
                        up_to_sequence: Some(1),
                        created_at_ms: now_ms(),
                    },
                    &source.runtime.signer,
                    &package_store,
                )
                .expect("snapshot package")
                .expect("snapshot package result")
                .snapshot;
            source
                .runtime
                .ledger
                .mark_acknowledged(
                    SyncTarget::new(receiver.runtime.local_node_id.clone()).expect("target"),
                    first.op_id,
                )
                .expect("ack first");
            source
                .runtime
                .ledger
                .compact(CompactionPolicy {
                    partition_id: partition.clone(),
                    keep_operations_after_sequence: Some(2),
                })
                .expect("compact");
            receiver.runtime.queue_repair_request(
                &source.runtime.local_node_id,
                partition.as_str(),
                1,
            );
            let queued_repairs = receiver
                .runtime
                .ledger
                .list_due_repair_requests(
                    PeerId::new(source.runtime.local_node_id.clone()).expect("peer"),
                    i64::MAX,
                    8,
                )
                .expect("queued repairs");
            assert_eq!(queued_repairs.len(), 1);

            let pull = MeshSyncPullPayload {
                from_node_id: receiver.runtime.local_node_id.clone(),
                partition_id: partition.as_str().to_string(),
                from_sequence: 1,
                limit: 64,
            };
            let response = source
                .runtime
                .handle_pull_payload(&receiver.runtime.local_node_id, pull)
                .expect("handle pull");

            match response {
                MeshSyncPullResult::Snapshot(payload) => {
                    assert_eq!(payload.snapshot_id, snapshot.snapshot_id.as_str());
                    assert_eq!(payload.operations_after_snapshot.len(), 1);
                    let ack = receiver
                        .runtime
                        .handle_snapshot_response_payload(&source.runtime.local_node_id, payload)
                        .expect("handle snapshot response");
                    source
                        .runtime
                        .handle_ack_payload(&receiver.runtime.local_node_id, ack)
                        .expect("ack snapshot");
                }
                MeshSyncPullResult::Operations(_) => panic!("expected snapshot response"),
            }
            let queued_repairs = receiver
                .runtime
                .ledger
                .list_due_repair_requests(
                    PeerId::new(source.runtime.local_node_id.clone()).expect("peer"),
                    i64::MAX,
                    8,
                )
                .expect("queued repairs");
            let pool = crate::addon::storage_sql::open_addon_db("org-default", addon_id)
                .expect("open addon db");
            let conn = pool.get().expect("conn");
            let name: String = conn
                .query_row("SELECT name FROM contacts WHERE id = 1", [], |row| {
                    row.get(0)
                })
                .expect("name");
            let target = SyncTarget::new(receiver.runtime.local_node_id.clone()).expect("target");
            let first_outbox = source
                .runtime
                .ledger
                .get_outbox_entry(target.clone(), first.op_id)
                .expect("first outbox");
            let second_outbox = source
                .runtime
                .ledger
                .get_outbox_entry(target, second.op_id)
                .expect("second outbox");

            assert_eq!(name, "Ola Kowalska");
            assert!(queued_repairs.is_empty());
            assert!(first_outbox.acknowledged);
            assert!(second_outbox.acknowledged);
        });
    }
}

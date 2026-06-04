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
use crate::db::{repository, DbPool};
use crate::mesh::security::MeshSecurity;
use crate::paths;
use crate::sync::hlc::HlcClock;
use crate::sync::ledger::{
    ActionType, BaselineEpoch, FieldValue, FjallSyncLedgerStore, HexNodeIdOperationVerifier,
    HybridLogicalTimestamp, InboxEntry, LedgerResult, NewSyncOperation, OperationId,
    OperationQuery, PartitionId, PeerCursor, PeerId, RepairQueueEntry, SnapshotId, SyncLedgerError,
    SyncLedgerStore, SyncOperation, SyncOperationSigner, SyncSnapshot, SyncTarget,
};
use crate::sync::snapshot::{
    verify_snapshot_signature, SnapshotBuildRequest, SnapshotManager, SnapshotPackageStore,
};
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
    settings_cipher: Arc<crate::crypto::SettingsCipher>,
    hlc: HlcClock,
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
    pub actor_user_id: Option<String>,
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

pub fn init(
    db: DbPool,
    signer: Arc<MeshSecurity>,
    settings_cipher: Arc<crate::crypto::SettingsCipher>,
) -> LedgerResult<Arc<SyncRuntime>> {
    let ledger_path = paths::tentaflow_home().join("sync").join("ledger");
    let ledger = Arc::new(FjallSyncLedgerStore::open(&ledger_path)?);
    let local_node_id = signer.ed25519_public_key_hex();
    repository::ensure_local_node_in_sync_identity(&db, &local_node_id, &local_node_id)
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
    // Resume the HLC from the persisted ledger state so monotonicity survives a
    // restart: the first post-restart `now()` is strictly later than the last
    // timestamp this node minted or observed before shutting down.
    let initial_hlc = ledger.current_hlc()?;
    let hlc = HlcClock::new(local_node_id.clone(), initial_hlc);
    let runtime = Arc::new(SyncRuntime {
        db,
        ledger,
        signer: RuntimeSigner {
            node_id: local_node_id.clone(),
            security: signer,
        },
        local_node_id,
        settings_cipher,
        hlc,
    });
    let _ = SYNC_RUNTIME.set(runtime.clone());
    Ok(SYNC_RUNTIME
        .get()
        .expect("sync runtime must be initialized")
        .clone())
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

/// Mints the next HLC stamp from the live runtime clock and persists the new
/// state. When the runtime is not yet initialized (early bootstrap or unit
/// tests that write before `init`), it falls back to a process-local clock so
/// captures still receive a monotonic timestamp inside their write transaction.
pub fn core_hlc_now() -> HybridLogicalTimestamp {
    match SYNC_RUNTIME.get() {
        Some(runtime) => runtime.hlc_now(),
        None => fallback_hlc().now(),
    }
}

/// Returns the locally-active baseline epoch used to stamp newly-minted core
/// operations. Defaults to genesis before the runtime exists.
pub fn core_epoch() -> BaselineEpoch {
    match SYNC_RUNTIME.get() {
        Some(runtime) => runtime
            .ledger
            .current_epoch()
            .unwrap_or_else(|_| genesis_epoch()),
        None => genesis_epoch(),
    }
}

/// Folds an incoming (remote) HLC into the local clock so the next locally
/// minted stamp is strictly later than anything observed from the mesh.
pub fn observe_core_hlc(remote: &HybridLogicalTimestamp) {
    if let Some(runtime) = SYNC_RUNTIME.get() {
        runtime.hlc.observe(remote);
        let _ = runtime.ledger.save_hlc(&runtime.hlc.now());
    }
}

/// Consumes the one-shot baseline-reset flag set by the v53 cutover migration.
///
/// WHERE this is called: once at startup, immediately after the sync runtime is
/// initialized and BEFORE the routine core-capture drain (see `main.rs`). The
/// flag (`settings.core_baseline_reset_pending`) is written by `migrations::run`
/// only on the boot that crosses v53. When present, this performs the full
/// baseline reset (bump epoch, wipe stale core ledger state, re-seed the outbox
/// from the post-flip rows) and then clears the flag in the SAME action, so a
/// routine restart finds no flag and never re-bumps the epoch — a second bump
/// would invalidate the post-cutover operations peers have already adopted.
///
/// Returns `Some(reseeded_ops)` when the cutover ran, `None` when there was
/// nothing to do (no flag, or runtime not initialized).
pub fn run_pending_baseline_cutover() -> LedgerResult<Option<usize>> {
    let Some(runtime) = SYNC_RUNTIME.get() else {
        return Ok(None);
    };
    runtime.run_pending_baseline_cutover()
}

/// Adopts the donor's baseline epoch after a baseline-adopt import committed the
/// donor snapshot into SQLite (see `sync::core_baseline::import_baseline`). The
/// local epoch is set to the donor's, then core ledger partitions are wiped and
/// re-seeded from the freshly-imported SQLite state so the joiner's outbox emits
/// every adopted row under the donor's epoch.
///
/// When the runtime is not initialized (in-process unit tests that exercise the
/// pure SQLite import with two bare `DbPool`s), this is a no-op: the SQLite
/// transaction is already committed and fully testable on its own.
pub fn adopt_donor_baseline_epoch(donor_epoch: &BaselineEpoch) -> LedgerResult<()> {
    let Some(runtime) = SYNC_RUNTIME.get() else {
        return Ok(());
    };
    runtime.adopt_donor_baseline_epoch(donor_epoch)
}

/// Parses the baseline-cutover marker. Returns the recorded pre-cutover epoch
/// counter when the marker has been upgraded to `pre_cutover_epoch=<n>`, or
/// `None` for the migration's initial `"1"` sentinel (no counter recorded yet).
fn parse_cutover_marker(marker: &str) -> Option<u64> {
    marker
        .strip_prefix("pre_cutover_epoch=")
        .and_then(|n| n.trim().parse::<u64>().ok())
}

fn genesis_epoch() -> BaselineEpoch {
    BaselineEpoch {
        counter: 0,
        origin_node: String::new(),
    }
}

/// Process-local HLC used only before the runtime is initialized. The node id is
/// stable per process so timestamps minted during bootstrap stay self-consistent
/// until the persisted clock takes over.
fn fallback_hlc() -> &'static HlcClock {
    static FALLBACK: OnceLock<HlcClock> = OnceLock::new();
    FALLBACK.get_or_init(|| HlcClock::new("bootstrap", None))
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

pub fn acknowledged_outbox_count(operation_id: OperationId) -> LedgerResult<Option<usize>> {
    let Some(runtime) = SYNC_RUNTIME.get() else {
        return Ok(None);
    };
    runtime.acknowledged_outbox_count(operation_id).map(Some)
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

pub fn build_sql_snapshot_package(
    partition_id: &str,
    up_to_sequence: Option<u64>,
) -> LedgerResult<Option<SyncSnapshot>> {
    let Some(runtime) = SYNC_RUNTIME.get() else {
        return Ok(None);
    };
    runtime.build_sql_snapshot_package(partition_id, up_to_sequence)
}

pub fn apply_unapplied_inbox(limit: usize) -> LedgerResult<Option<usize>> {
    let Some(runtime) = SYNC_RUNTIME.get() else {
        return Ok(None);
    };
    runtime.apply_unapplied_inbox(limit).map(Some)
}

/// Drains only the pending core write-capture journal into the Sync Ledger
/// outbox using the active runtime's DB pool. The periodic sync-repair scheduler
/// only reads the outbox, so core writes made while the process is running (e.g.
/// a Flow saved in the Flow Builder) would otherwise sit in the journal until the
/// next restart. Running this at the head of each scheduler tick converts those
/// journal entries into outbox operations before the same tick's push reads the
/// outbox.
///
/// Only the core journal is drained here: SQL, KV and blob captures publish
/// immediately after their commit (`record_sql_capture` / `ledger_kv_capture_now`
/// / `ledger_blob_capture_now`), so a periodic drain of those journals would
/// re-emit the same local operation — a row still `pending` when the tick reads
/// it gets published a second time by the immediate path, duplicating it in the
/// ledger/outbox. Core capture (`record_core_capture_tx`) is journal-only with no
/// online publish, so without this it would not sync until the next restart.
///
/// A drain failure is logged and the call returns `Ok(Some(0))`. Returns
/// `Ok(None)` when no runtime is initialized (bootstrap/tests).
pub fn drain_pending_core_captures_online(limit: usize) -> LedgerResult<Option<usize>> {
    let Some(runtime) = SYNC_RUNTIME.get() else {
        return Ok(None);
    };
    match crate::sync::core_capture::drain_pending_core_captures(&runtime.db, limit) {
        Ok(drained) => Ok(Some(drained)),
        Err(e) => {
            warn!("sync runtime: core capture drain failed: {}", e);
            Ok(Some(0))
        }
    }
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

    /// Mints the next HLC stamp and persists the advanced clock state so a
    /// restart resumes strictly after this timestamp.
    fn hlc_now(&self) -> HybridLogicalTimestamp {
        let timestamp = self.hlc.now();
        let _ = self.ledger.save_hlc(&timestamp);
        timestamp
    }

    fn run_pending_baseline_cutover(&self) -> LedgerResult<Option<usize>> {
        let marker = repository::get_setting(
            &self.db,
            crate::db::migrations::CORE_BASELINE_RESET_PENDING_KEY,
        )
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        let Some(marker) = marker else {
            return Ok(None);
        };

        // The marker records the epoch counter observed BEFORE the cutover so a
        // crash between `bump_epoch` and clearing the marker cannot double-bump.
        // The migration arms it with the sentinel "1" (it runs before the ledger
        // exists and cannot read the epoch); on the first cutover pass we replace
        // it with the live pre-cutover counter and persist that BEFORE touching
        // the epoch. On a retry the marker already encodes the counter, so we
        // compare against it and skip a second bump.
        let pre_cutover_counter = match parse_cutover_marker(&marker) {
            Some(counter) => counter,
            None => {
                let counter = self.ledger.current_epoch()?.counter;
                repository::set_setting(
                    &self.db,
                    crate::db::migrations::CORE_BASELINE_RESET_PENDING_KEY,
                    &format!("pre_cutover_epoch={counter}"),
                )
                .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
                counter
            }
        };
        let reseeded = self.perform_core_baseline_reset(Some(pre_cutover_counter))?;
        repository::delete_setting(
            &self.db,
            crate::db::migrations::CORE_BASELINE_RESET_PENDING_KEY,
        )
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        Ok(Some(reseeded))
    }

    /// Rebuilds the local core baseline idempotently. `pre_cutover_counter` is
    /// the epoch counter recorded in the marker before the cutover began; the
    /// epoch is bumped only when the current counter has not already advanced
    /// past it, so a crash-and-retry cannot double-bump. The reseed always re-
    /// emits the CURRENT SQLite snapshot into the freshly-wiped outbox, which is
    /// repeatable.
    fn perform_core_baseline_reset(&self, pre_cutover_counter: Option<u64>) -> LedgerResult<usize> {
        self.ledger.reset_core_partitions()?;

        let current = self.ledger.current_epoch()?;
        let already_bumped = match pre_cutover_counter {
            Some(pre) => current.counter > pre,
            // No recorded baseline (legacy marker): always bump from current.
            None => false,
        };
        if already_bumped {
            warn!(
                "sync runtime: core baseline cutover resuming after crash, epoch already at \
                 counter={} origin={} (recorded pre-cutover={:?}); skipping second bump",
                current.counter, current.origin_node, pre_cutover_counter
            );
        } else {
            let epoch = self.ledger.bump_epoch(&self.local_node_id)?;
            warn!(
                "sync runtime: core baseline reset, new epoch counter={} origin={}",
                epoch.counter, epoch.origin_node
            );
        }

        // Discard the historical core capture journal: the entries that survived
        // the v54 ALTER carry zeroed HLCs and replaying them would resurrect old
        // versions and tie multiple writes of one resource to identical (zero)
        // timestamps, which LWW cannot order. The reseed below re-derives the
        // present state instead.
        crate::sync::core_capture::clear_core_capture_journal(&self.db)
            .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;

        // Re-emit the CURRENT state of every core table as fresh INSERT captures.
        // `reseed_core_state_from_current_rows` writes the snapshot into the
        // (now empty) capture journal with one freshly-minted, monotonic HLC per
        // row; draining records them under the just-bumped epoch. Because the
        // outbox was wiped by `reset_core_partitions`, this is repeatable: a
        // crash-and-retry re-emits the same snapshot into an empty outbox.
        let emitted =
            repository::reseed_core_state_from_current_rows(&self.db, &self.settings_cipher)
                .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        crate::sync::core_capture::drain_pending_core_captures_with(
            &self.db,
            usize::MAX,
            |capture| {
                self.record_core_capture(capture)
                    .map(|record| Some(record.op_id))
            },
        )
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        Ok(emitted)
    }

    /// Sets the local epoch to the donor's and re-seeds core partitions from the
    /// just-imported SQLite snapshot. Called by `adopt_donor_baseline_epoch`
    /// after the baseline-adopt import transaction has committed.
    fn adopt_donor_baseline_epoch(&self, donor_epoch: &BaselineEpoch) -> LedgerResult<()> {
        self.ledger.set_epoch(donor_epoch.clone())?;
        self.ledger.reset_core_partitions()?;
        crate::sync::core_capture::clear_core_capture_journal(&self.db)
            .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        repository::reseed_core_state_from_current_rows(&self.db, &self.settings_cipher)
            .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        crate::sync::core_capture::drain_pending_core_captures_with(
            &self.db,
            usize::MAX,
            |capture| {
                self.record_core_capture(capture)
                    .map(|record| Some(record.op_id))
            },
        )
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        warn!(
            "sync runtime: adopted donor baseline epoch counter={} origin={}",
            donor_epoch.counter, donor_epoch.origin_node
        );
        Ok(())
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

    fn acknowledged_outbox_count(&self, operation_id: OperationId) -> LedgerResult<usize> {
        Ok(self
            .ledger
            .list_outbox_for_operation(operation_id)?
            .into_iter()
            .filter(|entry| entry.acknowledged)
            .count())
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
            if target.node_id == self.local_node_id {
                continue;
            }
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
            if target.node_id == self.local_node_id {
                continue;
            }
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
            let operation = match self.ledger.get_operation(entry.op_id) {
                Ok(operation) => operation,
                // Orphaned outbox row: the backing operation was compacted away
                // (reset_partitions_with_prefix intentionally leaves orphans of
                // unknown partition). Skip it and reap it lazily instead of
                // failing the whole push.
                Err(SyncLedgerError::OperationNotFound(_)) => {
                    warn!(
                        "sync runtime: pomijam osierocony wpis outbox op_id={} (operacja skompaktowana), usuwam",
                        entry.op_id
                    );
                    self.ledger
                        .remove_outbox_entry(target.clone(), entry.op_id)?;
                    continue;
                }
                Err(e) => return Err(e),
            };
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
            match self
                .ledger
                .mark_acknowledged(target.clone(), operation_id_from_wire(&op_id)?)
            {
                Ok(()) => {}
                Err(SyncLedgerError::OutboxEntryNotFound { .. }) => {}
                Err(e) => return Err(e),
            }
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
        for operation in &operations {
            self.ensure_peer_target_allowed(operation, source_node_id)?;
        }
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
                    source_node_id,
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
            source_node_id,
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
        target_node_id: &str,
        partition_id: String,
        snapshot: SyncSnapshot,
        include_tail: bool,
        tail_limit: u32,
    ) -> LedgerResult<MeshSyncSnapshotResponsePayload> {
        verify_snapshot_signature(&snapshot)?;
        let store = SnapshotPackageStore::new(SnapshotPackageStore::default_root());
        let blob_bytes = store.get_sql_package(&snapshot)?;
        let blob = crate::sync::snapshot::decode_snapshot_sql_blob(&blob_bytes)?;
        for operation in &blob.operations {
            self.ensure_peer_target_allowed(operation, target_node_id)?;
        }
        let operations_after_snapshot = if include_tail && tail_limit > 0 {
            let operations = self.ledger.get_operations(OperationQuery {
                partition_id: snapshot.partition_id.clone(),
                from_sequence: Some(snapshot.up_to_sequence.saturating_add(1)),
                to_sequence: None,
                limit: Some(tail_limit as usize),
            })?;
            for operation in &operations {
                self.ensure_peer_target_allowed(operation, target_node_id)?;
            }
            operations
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
            snapshot_bytes: crate::sync::ledger::encode(&snapshot)?,
            blob_bytes,
            operations_after_snapshot,
        })
    }

    fn build_sql_snapshot_package(
        &self,
        partition_id: &str,
        up_to_sequence: Option<u64>,
    ) -> LedgerResult<Option<SyncSnapshot>> {
        let package_store = SnapshotPackageStore::new(SnapshotPackageStore::default_root());
        let partition_id = PartitionId::new(partition_id.to_string())?;
        Ok(SnapshotManager::new(self.ledger.as_ref())
            .build_sql_package_and_persist(
                SnapshotBuildRequest {
                    partition_id,
                    up_to_sequence,
                    created_at_ms: now_ms(),
                },
                &self.signer,
                &package_store,
            )?
            .map(|result| result.snapshot))
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
        let snapshot: SyncSnapshot = crate::sync::ledger::decode(&payload.snapshot_bytes)?;
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
        entries.sort_by(|left, right| inbox_apply_order(left).cmp(&inbox_apply_order(right)));
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
                    &self.settings_cipher,
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
        if result.status == "resolved" || result.status == "ignored" {
            match self.ledger.mark_inbox_applied(source, operation_id) {
                Ok(()) => {}
                Err(SyncLedgerError::InboxEntryNotFound { .. }) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(result)
    }

    fn ensure_local_target_allowed(&self, operation: &SyncOperation) -> LedgerResult<()> {
        self.ensure_peer_target_allowed(operation, &self.local_node_id)
    }

    fn ensure_peer_target_allowed(
        &self,
        operation: &SyncOperation,
        target_node_id: &str,
    ) -> LedgerResult<()> {
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
            .any(|target| target.node_id == target_node_id);
        if allowed {
            Ok(())
        } else {
            Err(SyncLedgerError::Runtime(format!(
                "node {target_node_id} is not a sync target for {}/{}/{}",
                operation.body.addon_id, operation.body.resource_type, operation.body.resource_id
            )))
        }
    }

    fn build_core_operation(
        &self,
        capture: &crate::sync::core_capture::CoreWriteCapture,
    ) -> LedgerResult<NewSyncOperation> {
        let payload = crate::sync::ledger::encode(capture)?;
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
            partition_id: descriptor
                .partition_id(&capture.org_id, capture.actor_user_id.as_deref())?,
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
                .clone()
                .unwrap_or_else(|| "system".to_string()),
            actor_device_id: self.local_node_id.clone(),
            actor_node_id: self.local_node_id.clone(),
            // Reuse the HLC minted inside the write transaction (stored on the
            // capture row) so the operation carries the originating instant.
            hlc_timestamp: capture.hlc.clone(),
            epoch: self.ledger.current_epoch()?,
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
        let payload = crate::sync::ledger::encode(capture)?;
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
                .clone()
                .unwrap_or_else(|| "system".to_string()),
            actor_device_id: self.local_node_id.clone(),
            actor_node_id: self.local_node_id.clone(),
            hlc_timestamp: HybridLogicalTimestamp {
                wall_time_ms: capture.created_at_ms,
                logical: 0,
                node_id: self.local_node_id.clone(),
            },
            epoch: self.ledger.current_epoch()?,
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
        let payload = crate::sync::ledger::encode(capture)?;
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
                .clone()
                .unwrap_or_else(|| "system".to_string()),
            actor_device_id: self.local_node_id.clone(),
            actor_node_id: self.local_node_id.clone(),
            hlc_timestamp: HybridLogicalTimestamp {
                wall_time_ms: capture.created_at_ms,
                logical: 0,
                node_id: self.local_node_id.clone(),
            },
            epoch: self.ledger.current_epoch()?,
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
                .clone()
                .unwrap_or_else(|| "system".to_string()),
            actor_device_id: self.local_node_id.clone(),
            actor_node_id: self.local_node_id.clone(),
            hlc_timestamp: HybridLogicalTimestamp {
                wall_time_ms: capture.created_at_ms,
                logical: chunk_index as u32,
                node_id: self.local_node_id.clone(),
            },
            epoch: self.ledger.current_epoch()?,
            payload_hash,
            acl_snapshot_hash: sha256(
                format!("{}:{}:{}", capture.org_id, "core.blob", capture.sha256).as_bytes(),
            ),
            policy_epoch,
            encryption_info: None,
        })
    }

    fn build_kv_operation(&self, capture: &KvWriteCapture) -> LedgerResult<NewSyncOperation> {
        let payload = crate::sync::ledger::encode(capture)?;
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
                .clone()
                .unwrap_or_else(|| "system".to_string()),
            actor_device_id: self.local_node_id.clone(),
            actor_node_id: self.local_node_id.clone(),
            hlc_timestamp: HybridLogicalTimestamp {
                wall_time_ms: capture.created_at_ms,
                logical: 0,
                node_id: self.local_node_id.clone(),
            },
            epoch: self.ledger.current_epoch()?,
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
        operation: crate::sync::ledger::encode(operation)?,
    })
}

fn operation_from_wire(wire: &MeshSyncOperationWire) -> LedgerResult<SyncOperation> {
    let operation: SyncOperation = crate::sync::ledger::decode(&wire.operation)?;
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

fn inbox_apply_priority(operation: &SyncOperation) -> u8 {
    if operation.body.resource_type != "core.blob" {
        return core_apply_priority(operation);
    }
    match operation.body.table_name.as_str() {
        "blob_store_chunks" => 0,
        "blob_store" => 10,
        _ => 20,
    }
}

fn core_apply_priority(operation: &SyncOperation) -> u8 {
    if operation.body.addon_id != crate::sync::core_registry::CORE_SYNC_ADDON_ID {
        return 20;
    }
    match operation.body.table_name.as_str() {
        "organizations" => 1,
        "roles" => 2,
        "user_accounts" => 3,
        "user_groups" => 4,
        "group_members" => 5,
        "org_memberships" => 6,
        "flows" => 7,
        "flow_model_bindings" => 8,
        _ => 20,
    }
}

fn inbox_apply_order(entry: &InboxEntry) -> (u8, &str, u64) {
    (
        inbox_apply_priority(&entry.operation),
        entry.operation.body.partition_id.as_str(),
        entry.operation.body.partition_sequence,
    )
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
        actor_user_id: match operation.body.actor_user_id.as_str() {
            "" | "system" => None,
            uid => Some(uid.to_string()),
        },
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
    use crate::mesh::iroh_manager::{IrohMeshConfig, IrohMeshEvent, IrohMeshManager};
    use crate::sync::ledger::CompactionPolicy;
    use crate::sync::snapshot::SnapshotBuildRequest;
    use rusqlite::Connection;
    use std::ffi::OsString;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    struct RuntimeHarness {
        runtime: SyncRuntime,
        _ledger_dir: tempfile::TempDir,
    }

    fn make_db() -> DbPool {
        let conn = Connection::open_in_memory().expect("open db");
        migrations::run(&conn).expect("run migrations");
        Arc::new(Mutex::new(conn))
    }

    fn make_db_at(path: &Path) -> DbPool {
        let conn = Connection::open(path).expect("open persistent db");
        migrations::run(&conn).expect("run migrations");
        Arc::new(Mutex::new(conn))
    }

    fn make_security(db: DbPool, key_seed: u8) -> Arc<MeshSecurity> {
        let cipher = Arc::new(crate::crypto::SettingsCipher::new(&[key_seed; 32]));
        Arc::new(MeshSecurity::new(db, cipher).expect("mesh security"))
    }

    fn make_settings_cipher(key_seed: u8) -> Arc<crate::crypto::SettingsCipher> {
        Arc::new(crate::crypto::SettingsCipher::new(&[key_seed; 32]))
    }

    fn make_runtime(key_seed: u8) -> RuntimeHarness {
        let ledger_dir = tempfile::tempdir().expect("ledger dir");
        let db = make_db();
        let security = make_security(db.clone(), key_seed);
        let local_node_id = security.ed25519_public_key_hex();
        let ledger = Arc::new(FjallSyncLedgerStore::open(ledger_dir.path()).expect("ledger"));
        let hlc = HlcClock::new(local_node_id.clone(), None);
        RuntimeHarness {
            runtime: SyncRuntime {
                db,
                ledger,
                signer: RuntimeSigner {
                    node_id: local_node_id.clone(),
                    security,
                },
                local_node_id,
                settings_cipher: make_settings_cipher(key_seed),
                hlc,
            },
            _ledger_dir: ledger_dir,
        }
    }

    fn make_runtime_from_paths(db_path: &Path, ledger_path: &Path, key_seed: u8) -> SyncRuntime {
        let db = make_db_at(db_path);
        let security = make_security(db.clone(), key_seed);
        let local_node_id = security.ed25519_public_key_hex();
        let ledger = Arc::new(FjallSyncLedgerStore::open(ledger_path).expect("ledger"));
        let hlc = HlcClock::new(local_node_id.clone(), None);
        SyncRuntime {
            db,
            ledger,
            signer: RuntimeSigner {
                node_id: local_node_id.clone(),
                security,
            },
            local_node_id,
            settings_cipher: make_settings_cipher(key_seed),
            hlc,
        }
    }

    async fn make_mesh_manager(runtime: &SyncRuntime) -> Arc<IrohMeshManager> {
        let cfg = IrohMeshConfig {
            node_id: String::new(),
            bind_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            relay_url: None,
            enable_lan_discovery: false,
            enable_dht_discovery: false,
        };
        IrohMeshManager::new(cfg, runtime.signer.security.clone())
            .await
            .expect("mesh manager")
    }

    fn loopback_addr_of(manager: &IrohMeshManager) -> std::net::SocketAddr {
        manager
            .endpoint()
            .bound_sockets()
            .into_iter()
            .find(|addr| addr.is_ipv4())
            .expect("bound v4 socket")
    }

    async fn wait_connected(manager: &IrohMeshManager, peer_id: &str) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if manager.is_connected(peer_id).await {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("mesh connected");
    }

    async fn connected_mesh_pair(
        source: &SyncRuntime,
        receiver: &SyncRuntime,
    ) -> (
        Arc<IrohMeshManager>,
        Arc<IrohMeshManager>,
        tokio::sync::broadcast::Receiver<IrohMeshEvent>,
        tokio::sync::broadcast::Receiver<IrohMeshEvent>,
    ) {
        let source_mesh = make_mesh_manager(source).await;
        let receiver_mesh = make_mesh_manager(receiver).await;
        let _source_task = source_mesh.start();
        let _receiver_task = receiver_mesh.start();

        let source_id = source_mesh.node_id();
        let receiver_id = receiver_mesh.node_id();
        let source_addr = loopback_addr_of(&source_mesh);
        let receiver_addr = loopback_addr_of(&receiver_mesh);
        let source_events = source_mesh.subscribe();
        let receiver_events = receiver_mesh.subscribe();

        let dial_source = {
            let source_mesh = source_mesh.clone();
            let receiver_id = receiver_id.clone();
            async move {
                source_mesh
                    .connect_to_peer_direct(&receiver_id, receiver_addr)
                    .await
            }
        };
        let dial_receiver = {
            let receiver_mesh = receiver_mesh.clone();
            let source_id = source_id.clone();
            async move {
                receiver_mesh
                    .connect_to_peer_direct(&source_id, source_addr)
                    .await
            }
        };
        let (source_dial, receiver_dial) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(dial_source, dial_receiver)
        })
        .await
        .expect("mesh dial timeout");
        source_dial.expect("source dial");
        receiver_dial.expect("receiver dial");
        wait_connected(&source_mesh, &receiver_id).await;
        wait_connected(&receiver_mesh, &source_id).await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        (source_mesh, receiver_mesh, source_events, receiver_events)
    }

    async fn connect_mesh_managers(source_mesh: &IrohMeshManager, receiver_mesh: &IrohMeshManager) {
        let source_id = source_mesh.node_id();
        let receiver_id = receiver_mesh.node_id();
        let source_addr = loopback_addr_of(source_mesh);
        let receiver_addr = loopback_addr_of(receiver_mesh);
        let source_dial = source_mesh.connect_to_peer_direct(&receiver_id, receiver_addr);
        let receiver_dial = receiver_mesh.connect_to_peer_direct(&source_id, source_addr);
        let (source_dial, receiver_dial) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(source_dial, receiver_dial)
        })
        .await
        .expect("mesh dial timeout");
        source_dial.expect("source dial");
        receiver_dial.expect("receiver dial");
        wait_connected(source_mesh, &receiver_id).await;
        wait_connected(receiver_mesh, &source_id).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    async fn send_push_and_ack_over_mesh(
        source: &SyncRuntime,
        receiver: &SyncRuntime,
        source_mesh: &IrohMeshManager,
        receiver_mesh: &IrohMeshManager,
        source_events: &mut tokio::sync::broadcast::Receiver<IrohMeshEvent>,
        receiver_events: &mut tokio::sync::broadcast::Receiver<IrohMeshEvent>,
        push: MeshSyncPushPayload,
    ) -> MeshSyncAckPayload {
        let push_bytes = tentaflow_protocol::cbor::encode(&push).expect("encode push");
        source_mesh
            .send_ufp2_to_peer(
                &receiver.local_node_id,
                tentaflow_protocol::mesh::MESH_MSG_SYNC_PUSH,
                &push_bytes,
            )
            .await
            .expect("send push");

        let received_push = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match receiver_events.recv().await {
                    Ok(IrohMeshEvent::SyncPushReceived { from_node_id, data })
                        if from_node_id == source.local_node_id =>
                    {
                        return data;
                    }
                    Ok(_) | Err(_) => continue,
                }
            }
        })
        .await
        .expect("sync push event");
        let received_push = tentaflow_protocol::cbor::decode::<
            tentaflow_protocol::mesh::MeshSyncPushPayload,
        >(&received_push)
        .expect("decode push");
        let ack = receiver
            .handle_push_payload(&source.local_node_id, received_push)
            .expect("handle push");
        let ack_bytes = tentaflow_protocol::cbor::encode(&ack).expect("encode ack");
        receiver_mesh
            .send_ufp2_to_peer(
                &source.local_node_id,
                tentaflow_protocol::mesh::MESH_MSG_SYNC_ACK,
                &ack_bytes,
            )
            .await
            .expect("send ack");

        let received_ack = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match source_events.recv().await {
                    Ok(IrohMeshEvent::SyncAckReceived { from_node_id, data })
                        if from_node_id == receiver.local_node_id =>
                    {
                        return data;
                    }
                    Ok(_) | Err(_) => continue,
                }
            }
        })
        .await
        .expect("sync ack event");
        let received_ack = tentaflow_protocol::cbor::decode::<
            tentaflow_protocol::mesh::MeshSyncAckPayload,
        >(&received_ack)
        .expect("decode ack");
        source
            .handle_ack_payload(&receiver.local_node_id, received_ack.clone())
            .expect("handle ack");
        received_ack
    }

    fn ack_for(receiver: &SyncRuntime, operation_ids: Vec<Vec<u8>>) -> MeshSyncAckPayload {
        MeshSyncAckPayload {
            from_node_id: receiver.local_node_id.clone(),
            operation_ids,
        }
    }

    fn test_hlc() -> HybridLogicalTimestamp {
        HybridLogicalTimestamp {
            wall_time_ms: now_ms(),
            logical: 0,
            node_id: "test-node".to_string(),
        }
    }

    /// HLC strictly later than `test_hlc()` regardless of wall-clock granularity.
    /// A real write transaction mints a monotonic HLC, so an update always stamps
    /// after the insert it follows; LWW materialization relies on that ordering.
    fn test_hlc_later() -> HybridLogicalTimestamp {
        HybridLogicalTimestamp {
            wall_time_ms: now_ms() + 1,
            logical: 0,
            node_id: "test-node".to_string(),
        }
    }

    fn test_epoch() -> BaselineEpoch {
        BaselineEpoch {
            counter: 0,
            origin_node: String::new(),
        }
    }

    fn core_capture_for(
        kind: crate::sync::core_registry::CoreSyncResourceKind,
        resource_id: &str,
        fields: BTreeMap<String, FieldValue>,
    ) -> crate::sync::core_capture::CoreWriteCapture {
        crate::sync::core_capture::CoreWriteCapture::new(
            kind,
            "org-default",
            resource_id,
            SqlWriteAction::Insert,
            fields,
            Some("test-actor".to_string()),
            test_hlc(),
            test_epoch(),
        )
    }

    fn trust_each_other(source: &SyncRuntime, receiver: &SyncRuntime) {
        source
            .signer
            .security
            .add_trusted_key(
                &receiver.local_node_id,
                &receiver.signer.security.public_key_hex(),
                "receiver",
            )
            .expect("source trusts receiver");
        receiver
            .signer
            .security
            .add_trusted_key(
                &source.local_node_id,
                &source.signer.security.public_key_hex(),
                "source",
            )
            .expect("receiver trusts source");
    }

    fn test_home_guard() -> std::sync::MutexGuard<'static, ()> {
        crate::addon::fs_sandbox::test_home_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn unique_addon_id(prefix: &str) -> String {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);

        format!(
            "{prefix}-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        )
    }

    struct EnvVarGuard {
        name: &'static str,
        old_value: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set_path(name: &'static str, value: &std::path::Path) -> Self {
            let guard = Self {
                name,
                old_value: std::env::var_os(name),
            };
            unsafe {
                std::env::set_var(name, value);
            }
            guard
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.old_value {
                Some(value) => unsafe {
                    std::env::set_var(self.name, value);
                },
                None => unsafe {
                    std::env::remove_var(self.name);
                },
            }
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
            Some("00000000-0000-0000-0000-000000000007".to_string()),
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

    /// Inserts the `test-actor` user the capture helpers reference. Required only
    /// when a test persists a capture row into SQLite (`record_core_write_capture`),
    /// because `__tentaflow_core_sync_captures.actor_user_id` is FK-bound to
    /// `user_accounts(id)`. The in-memory ledger path skips this SQL constraint.
    fn seed_actor_user(db: &DbPool, actor_id: &str) {
        let conn = db.lock().expect("db");
        conn.execute(
            "INSERT INTO user_accounts (id, username, password_hash) VALUES (?1, ?1, 'h')",
            rusqlite::params![actor_id],
        )
        .expect("seed actor user");
    }

    /// Inserts a live `flows` row so a baseline reset has current state to
    /// re-seed from. The capture journal is intentionally left untouched.
    fn seed_flow_row(db: &DbPool, id: &str, name: &str) {
        let conn = db.lock().expect("db");
        conn.execute(
            "INSERT INTO flows (id, name, is_default, flow_json, status) \
             VALUES (?1, ?2, 0, '{\"nodes\":[]}', 'active')",
            rusqlite::params![id, name],
        )
        .expect("seed flow row");
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
            actor_user_id: Some("00000000-0000-0000-0000-000000000007".to_string()),
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
            actor_user_id: Some("00000000-0000-0000-0000-000000000007".to_string()),
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
            Some("test-actor".to_string()),
            test_hlc(),
            test_epoch(),
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
            Some("test-actor".to_string()),
            test_hlc_later(),
            test_epoch(),
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
    fn core_flow_capture_queues_for_trusted_mesh_node_with_default_policy() {
        with_tmp_home(|| {
            let source = make_runtime(121);
            let receiver = make_runtime(122);
            source
                .runtime
                .signer
                .security
                .add_trusted_key(
                    &receiver.runtime.local_node_id,
                    &receiver.runtime.signer.security.public_key_hex(),
                    "receiver",
                )
                .expect("trust receiver");

            let result = source
                .runtime
                .record_core_capture(core_flow_capture("flow-default-policy", "Flow"))
                .expect("record core capture");
            let target = SyncTarget::new(receiver.runtime.local_node_id.clone()).expect("target");
            let outbox = source
                .runtime
                .ledger
                .list_pending_outbox(target, 10)
                .expect("outbox");

            assert_eq!(result.queued_targets, 1);
            assert_eq!(outbox.len(), 1);
            assert_eq!(outbox[0].op_id, result.op_id);
        });
    }

    #[test]
    fn push_skips_and_reaps_orphaned_outbox_entry() {
        with_tmp_home(|| {
            let source = make_runtime(221);
            let receiver = make_runtime(222);
            seed_core_authority_target(
                &source.runtime.db,
                "core.flow",
                &receiver.runtime.local_node_id,
            );

            // Two captures land in the same core/flows partition (sequences 1 and
            // 2). Both get queued to the receiver's outbox.
            let first = source
                .runtime
                .record_core_capture(core_flow_capture("flow-orphan", "Orphan"))
                .expect("record first");
            let second = source
                .runtime
                .record_core_capture(core_flow_capture("flow-live", "Live"))
                .expect("record second");

            // Compact away sequence 1: its operation + op_id index disappear, but
            // its outbox row stays — turning it into an orphan.
            source
                .runtime
                .ledger
                .compact(CompactionPolicy {
                    partition_id: PartitionId::new("core/org/org-default/flows").unwrap(),
                    keep_operations_after_sequence: Some(2),
                })
                .expect("compact");
            assert!(source.runtime.ledger.get_operation(first.op_id).is_err());

            // The push path must skip the orphan and still emit the live op,
            // rather than erroring out on the missing operation.
            let payload = source
                .runtime
                .build_push_payload_for_target(&receiver.runtime.local_node_id, 16)
                .expect("build push must not fail on orphan")
                .expect("live operation should produce a payload");
            assert_eq!(payload.operations.len(), 1);

            // The orphaned outbox row was reaped during the push.
            let target = SyncTarget::new(receiver.runtime.local_node_id.clone()).expect("target");
            assert!(
                source
                    .runtime
                    .ledger
                    .get_outbox_entry(target.clone(), first.op_id)
                    .is_err(),
                "orphaned outbox entry must be removed by the push path"
            );
            // The live entry was marked delivered, not removed.
            assert!(source
                .runtime
                .ledger
                .get_outbox_entry(target, second.op_id)
                .is_ok());
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

            crate::sync::core_materializer::apply_core_operation(
                &receiver.runtime.db,
                &receiver.runtime.settings_cipher,
                &operation,
            )
            .expect("apply core operation");
            let flow = repository::get_flow(&receiver.runtime.db, "41")
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

            crate::sync::core_materializer::apply_core_operation(
                &receiver.runtime.db,
                &receiver.runtime.settings_cipher,
                &operation,
            )
            .expect("merge core operation");
            let flow = repository::get_flow(&receiver.runtime.db, "43")
                .expect("get flow")
                .expect("flow");

            assert_eq!(flow.name, "Merged Flow");
            assert_eq!(flow.status, "active");
            assert_eq!(flow.flow_json, r#"{"nodes":[{"id":"remote"}]}"#);
        });
    }

    #[test]
    fn shared_setting_secret_capture_materializes_with_receiver_cipher() {
        with_tmp_home(|| {
            let source = make_runtime(31);
            let receiver = make_runtime(32);
            repository::set_shared_secret_setting_secure(
                &source.runtime.db,
                "hf_token",
                "hf_test_secret",
                &source.runtime.settings_cipher,
                None,
            )
            .expect("set shared secret");
            let capture_id = {
                let conn = source.runtime.db.lock().expect("db lock");
                conn.query_row(
                    "SELECT capture_id FROM __tentaflow_core_sync_captures \
                     WHERE resource_type = 'core.shared_setting_secret' AND resource_id = 'hf_token' \
                     ORDER BY created_at_ms DESC LIMIT 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .expect("capture id")
            };
            let capture = {
                let conn = source.runtime.db.lock().expect("db lock");
                crate::sync::core_capture::load_core_write_capture(&conn, &capture_id)
                    .expect("load capture")
                    .expect("capture")
            };
            let result = source
                .runtime
                .record_core_capture(capture)
                .expect("record core capture");
            let operation = source
                .runtime
                .ledger
                .get_operation(result.op_id)
                .expect("operation");

            crate::sync::core_materializer::apply_core_operation(
                &receiver.runtime.db,
                &receiver.runtime.settings_cipher,
                &operation,
            )
            .expect("apply shared secret");
            let value = repository::get_setting_secure(
                &receiver.runtime.db,
                "hf_token",
                &receiver.runtime.settings_cipher,
            )
            .expect("get secret");

            assert_eq!(value.as_deref(), Some("hf_test_secret"));
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

            let flow = repository::get_flow(&receiver.runtime.db, "42")
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
                Some("00000000-0000-0000-0000-000000000007".to_string()),
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
                Some("00000000-0000-0000-0000-000000000007".to_string()),
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
                        crate::sync::ledger::decode(&wire.operation).expect("wire operation");
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
                    allowed_user_id.as_str(),
                    "Allowed Node",
                ),
                (
                    receiver_denied.runtime.local_node_id.as_str(),
                    denied_user_id.as_str(),
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
                Some(allowed_user_id.as_str()),
                Some(allowed_user_id.as_str()),
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
                    allowed_user_id.as_str(),
                    "Revoked Node",
                ),
                (
                    receiver_new_owner.runtime.local_node_id.as_str(),
                    new_owner_id.as_str(),
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
                Some(allowed_user_id.as_str()),
                Some(allowed_user_id.as_str()),
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
                Some(new_owner_id.as_str()),
                Some(new_owner_id.as_str()),
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
            repository::set_user_role(&source.runtime.db, &admin_user_id, "admin")
                .expect("admin role");
            repository::upsert_sync_node_identity(
                &source.runtime.db,
                &receiver.runtime.local_node_id,
                "pub",
                "ed25519",
                "Admin Node",
                "laptop",
                "trusted",
                Some(&admin_user_id),
                "standard",
            )
            .expect("sync node");
            repository::assign_node_to_user(
                &source.runtime.db,
                &receiver.runtime.local_node_id,
                &admin_user_id,
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
    fn conflict_keep_local_marks_inbox_applied_without_overwrite() {
        with_tmp_home(|| {
            let source = make_runtime(43);
            let receiver = make_runtime(44);
            let addon_id = "sync-runtime-conflict-keep-local";
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
                    SyncConflictResolution::KeepLocal,
                )
                .expect("resolve");

            assert_eq!(resolved.status, "ignored");
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
            assert_eq!(name, "Local");
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

            let flow = repository::get_flow(&receiver.runtime.db, "61")
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn multi_node_mesh_sync_push_materializes_core_flow_and_acks() {
        let _guard = test_home_guard();
        let tmp = tempfile::tempdir().expect("home");
        let _home_guard = EnvVarGuard::set_path("TENTAFLOW_HOME", tmp.path());
        let source = make_runtime(71);
        let receiver = make_runtime(72);
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
        trust_each_other(&source.runtime, &receiver.runtime);

        let (source_mesh, receiver_mesh, mut source_events, mut receiver_events) =
            connected_mesh_pair(&source.runtime, &receiver.runtime).await;

        let capture = source
            .runtime
            .record_core_capture(complete_core_flow_capture("71", "Mesh E2E Flow"))
            .expect("record core flow");
        let push = source
            .runtime
            .build_push_payload_for_target(&receiver.runtime.local_node_id, 16)
            .expect("build push")
            .expect("pending push");
        let bytes = tentaflow_protocol::cbor::encode(&push).expect("encode push");
        source_mesh
            .send_ufp2_to_peer(
                &receiver.runtime.local_node_id,
                tentaflow_protocol::mesh::MESH_MSG_SYNC_PUSH,
                &bytes,
            )
            .await
            .expect("send push");

        let received_push = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match receiver_events.recv().await {
                    Ok(IrohMeshEvent::SyncPushReceived { from_node_id, data })
                        if from_node_id == source.runtime.local_node_id =>
                    {
                        return data;
                    }
                    Ok(_) | Err(_) => continue,
                }
            }
        })
        .await
        .expect("sync push event");
        let received_push = tentaflow_protocol::cbor::decode::<
            tentaflow_protocol::mesh::MeshSyncPushPayload,
        >(&received_push)
        .expect("decode push");
        let ack = receiver
            .runtime
            .handle_push_payload(&source.runtime.local_node_id, received_push)
            .expect("handle push");
        let ack_bytes = tentaflow_protocol::cbor::encode(&ack).expect("encode ack");
        receiver_mesh
            .send_ufp2_to_peer(
                &source.runtime.local_node_id,
                tentaflow_protocol::mesh::MESH_MSG_SYNC_ACK,
                &ack_bytes,
            )
            .await
            .expect("send ack");

        let received_ack = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match source_events.recv().await {
                    Ok(IrohMeshEvent::SyncAckReceived { from_node_id, data })
                        if from_node_id == receiver.runtime.local_node_id =>
                    {
                        return data;
                    }
                    Ok(_) | Err(_) => continue,
                }
            }
        })
        .await
        .expect("sync ack event");
        let received_ack = tentaflow_protocol::cbor::decode::<
            tentaflow_protocol::mesh::MeshSyncAckPayload,
        >(&received_ack)
        .expect("decode ack");
        source
            .runtime
            .handle_ack_payload(&receiver.runtime.local_node_id, received_ack)
            .expect("handle ack");

        let flow = repository::get_flow(&receiver.runtime.db, "71")
            .expect("get flow")
            .expect("flow");
        let outbox = source
            .runtime
            .ledger
            .get_outbox_entry(
                SyncTarget::new(receiver.runtime.local_node_id.clone()).expect("target"),
                capture.op_id,
            )
            .expect("outbox");

        source_mesh.shutdown().await;
        receiver_mesh.shutdown().await;

        assert_eq!(flow.name, "Mesh E2E Flow");
        assert!(outbox.acknowledged);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn multi_node_mesh_four_node_fanout_syncs_core_flow_to_all_targets() {
        let _guard = test_home_guard();
        let tmp = tempfile::tempdir().expect("home");
        let _home_guard = EnvVarGuard::set_path("TENTAFLOW_HOME", tmp.path());
        let source = make_runtime(86);
        let receiver_a = make_runtime(87);
        let receiver_b = make_runtime(88);
        let receiver_c = make_runtime(89);
        let receivers = [&receiver_a, &receiver_b, &receiver_c];
        let mut receiver_user_ids = Vec::new();
        for (idx, receiver) in receivers.iter().enumerate() {
            let user_id = repository::create_user_account(
                &source.runtime.db,
                &format!("fanout-user-{idx}"),
                "hash",
                &format!("Fanout User {idx}"),
                &format!("fanout-{idx}@example.com"),
            )
            .expect("fanout user");
            repository::upsert_sync_node_identity(
                &source.runtime.db,
                &receiver.runtime.local_node_id,
                &receiver.runtime.signer.security.public_key_hex(),
                "ed25519",
                &format!("Fanout Node {idx}"),
                "laptop",
                "trusted",
                Some(user_id.as_str()),
                "standard",
            )
            .expect("fanout node");
            repository::assign_node_to_user(
                &source.runtime.db,
                &receiver.runtime.local_node_id,
                &user_id,
                "primary",
                None,
            )
            .expect("fanout node assignment");
            receiver_user_ids.push(user_id);
            seed_core_authority_target(
                &receiver.runtime.db,
                "core.flow",
                &receiver.runtime.local_node_id,
            );
            trust_each_other(&source.runtime, &receiver.runtime);
        }
        repository::upsert_sync_policy(
            &source.runtime.db,
            "policy-core-flow-fanout",
            "org-default",
            crate::sync::core_registry::CORE_SYNC_ADDON_ID,
            Some("core.flow"),
            None,
            "replicated_by_permission",
            None,
            None,
            true,
        )
        .expect("fanout sync policy");
        repository::upsert_sync_resource_acl(
            &source.runtime.db,
            "org-default",
            crate::sync::core_registry::CORE_SYNC_ADDON_ID,
            "core.flow",
            "86",
            receiver_user_ids.first().map(|s| s.as_str()),
            receiver_user_ids.first().map(|s| s.as_str()),
            None,
            None,
            "all",
        )
        .expect("fanout resource acl");

        let source_mesh = make_mesh_manager(&source.runtime).await;
        let receiver_meshes = vec![
            make_mesh_manager(&receiver_a.runtime).await,
            make_mesh_manager(&receiver_b.runtime).await,
            make_mesh_manager(&receiver_c.runtime).await,
        ];
        let _source_task = source_mesh.start();
        for receiver_mesh in &receiver_meshes {
            let _receiver_task = receiver_mesh.start();
        }
        let mut source_events = source_mesh.subscribe();
        let mut receiver_events = receiver_meshes
            .iter()
            .map(|receiver_mesh| receiver_mesh.subscribe())
            .collect::<Vec<_>>();

        for receiver_mesh in &receiver_meshes {
            connect_mesh_managers(&source_mesh, receiver_mesh).await;
        }

        let capture = source
            .runtime
            .record_core_capture(complete_core_flow_capture("86", "Four Node Fanout Flow"))
            .expect("record fanout flow");
        for idx in 0..receivers.len() {
            let receiver = receivers[idx];
            let push = source
                .runtime
                .build_push_payload_for_target(&receiver.runtime.local_node_id, 16)
                .expect("build fanout push")
                .expect("fanout push");
            send_push_and_ack_over_mesh(
                &source.runtime,
                &receiver.runtime,
                &source_mesh,
                &receiver_meshes[idx],
                &mut source_events,
                &mut receiver_events[idx],
                push,
            )
            .await;
        }

        for receiver in receivers {
            let flow = repository::get_flow(&receiver.runtime.db, "86")
                .expect("get fanout flow")
                .expect("fanout flow");
            let outbox = source
                .runtime
                .ledger
                .get_outbox_entry(
                    SyncTarget::new(receiver.runtime.local_node_id.clone()).expect("target"),
                    capture.op_id,
                )
                .expect("fanout outbox");
            assert_eq!(flow.name, "Four Node Fanout Flow");
            assert!(outbox.acknowledged);
        }

        source_mesh.shutdown().await;
        for receiver_mesh in receiver_meshes {
            receiver_mesh.shutdown().await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn multi_node_mesh_full_restart_persists_fanout_and_acks() {
        let _guard = test_home_guard();
        let tmp = tempfile::tempdir().expect("home");
        let _home_guard = EnvVarGuard::set_path("TENTAFLOW_HOME", tmp.path());
        let source_db_path = tmp.path().join("source.db");
        let source_ledger_path = tmp.path().join("source-ledger");
        let receiver_paths = [
            (
                tmp.path().join("receiver-a.db"),
                tmp.path().join("receiver-a-ledger"),
            ),
            (
                tmp.path().join("receiver-b.db"),
                tmp.path().join("receiver-b-ledger"),
            ),
            (
                tmp.path().join("receiver-c.db"),
                tmp.path().join("receiver-c-ledger"),
            ),
        ];
        let capture_op_id;
        let receiver_node_ids;

        {
            let source = make_runtime_from_paths(&source_db_path, &source_ledger_path, 111);
            let receiver_a =
                make_runtime_from_paths(&receiver_paths[0].0, &receiver_paths[0].1, 112);
            let receiver_b =
                make_runtime_from_paths(&receiver_paths[1].0, &receiver_paths[1].1, 113);
            let receiver_c =
                make_runtime_from_paths(&receiver_paths[2].0, &receiver_paths[2].1, 114);
            let receivers = [&receiver_a, &receiver_b, &receiver_c];
            let mut receiver_user_ids = Vec::new();

            for (idx, receiver) in receivers.iter().enumerate() {
                let user_id = repository::create_user_account(
                    &source.db,
                    &format!("restart-fanout-user-{idx}"),
                    "hash",
                    &format!("Restart Fanout User {idx}"),
                    &format!("restart-fanout-{idx}@example.test"),
                )
                .expect("restart fanout user");
                repository::upsert_sync_node_identity(
                    &source.db,
                    &receiver.local_node_id,
                    &receiver.signer.security.public_key_hex(),
                    "ed25519",
                    &format!("Restart Fanout Node {idx}"),
                    "laptop",
                    "trusted",
                    Some(user_id.as_str()),
                    "standard",
                )
                .expect("restart fanout node");
                repository::assign_node_to_user(
                    &source.db,
                    &receiver.local_node_id,
                    &user_id,
                    "primary",
                    None,
                )
                .expect("restart fanout assignment");
                receiver_user_ids.push(user_id);
                seed_core_authority_target(&receiver.db, "core.flow", &receiver.local_node_id);
                trust_each_other(&source, receiver);
            }
            repository::upsert_sync_policy(
                &source.db,
                "policy-core-flow-full-restart-fanout",
                "org-default",
                crate::sync::core_registry::CORE_SYNC_ADDON_ID,
                Some("core.flow"),
                None,
                "replicated_by_permission",
                None,
                None,
                true,
            )
            .expect("restart fanout policy");
            repository::upsert_sync_resource_acl(
                &source.db,
                "org-default",
                crate::sync::core_registry::CORE_SYNC_ADDON_ID,
                "core.flow",
                "11101",
                receiver_user_ids.first().map(|s| s.as_str()),
                receiver_user_ids.first().map(|s| s.as_str()),
                None,
                None,
                "all",
            )
            .expect("restart fanout acl");

            let capture = source
                .record_core_capture(complete_core_flow_capture(
                    "11101",
                    "Full Restart Fanout Flow",
                ))
                .expect("record restart fanout flow");
            assert_eq!(capture.queued_targets, 3);
            for receiver in receivers {
                let outbox = source
                    .ledger
                    .get_outbox_entry(
                        SyncTarget::new(receiver.local_node_id.clone()).expect("target"),
                        capture.op_id,
                    )
                    .expect("persisted fanout outbox before restart");
                assert!(!outbox.acknowledged);
            }
            capture_op_id = capture.op_id;
            receiver_node_ids = [
                receiver_a.local_node_id.clone(),
                receiver_b.local_node_id.clone(),
                receiver_c.local_node_id.clone(),
            ];
        }

        {
            let source = make_runtime_from_paths(&source_db_path, &source_ledger_path, 111);
            let receiver_a =
                make_runtime_from_paths(&receiver_paths[0].0, &receiver_paths[0].1, 112);
            let receiver_b =
                make_runtime_from_paths(&receiver_paths[1].0, &receiver_paths[1].1, 113);
            let receiver_c =
                make_runtime_from_paths(&receiver_paths[2].0, &receiver_paths[2].1, 114);
            let receivers = [&receiver_a, &receiver_b, &receiver_c];
            for receiver in receivers {
                trust_each_other(&source, receiver);
            }

            let source_mesh = make_mesh_manager(&source).await;
            let receiver_meshes = vec![
                make_mesh_manager(&receiver_a).await,
                make_mesh_manager(&receiver_b).await,
                make_mesh_manager(&receiver_c).await,
            ];
            let _source_task = source_mesh.start();
            for receiver_mesh in &receiver_meshes {
                let _receiver_task = receiver_mesh.start();
            }
            let mut source_events = source_mesh.subscribe();
            let mut receiver_events = receiver_meshes
                .iter()
                .map(|receiver_mesh| receiver_mesh.subscribe())
                .collect::<Vec<_>>();

            for receiver_mesh in &receiver_meshes {
                connect_mesh_managers(&source_mesh, receiver_mesh).await;
            }

            for idx in 0..receivers.len() {
                let receiver = receivers[idx];
                let push = source
                    .build_push_payload_for_target(&receiver.local_node_id, 16)
                    .expect("build restart fanout push")
                    .expect("restart fanout push");
                send_push_and_ack_over_mesh(
                    &source,
                    receiver,
                    &source_mesh,
                    &receiver_meshes[idx],
                    &mut source_events,
                    &mut receiver_events[idx],
                    push,
                )
                .await;
            }

            source_mesh.shutdown().await;
            for receiver_mesh in receiver_meshes {
                receiver_mesh.shutdown().await;
            }
        }

        let source = make_runtime_from_paths(&source_db_path, &source_ledger_path, 111);
        let receiver_a = make_runtime_from_paths(&receiver_paths[0].0, &receiver_paths[0].1, 112);
        let receiver_b = make_runtime_from_paths(&receiver_paths[1].0, &receiver_paths[1].1, 113);
        let receiver_c = make_runtime_from_paths(&receiver_paths[2].0, &receiver_paths[2].1, 114);
        let receivers = [&receiver_a, &receiver_b, &receiver_c];

        for (idx, receiver) in receivers.iter().enumerate() {
            assert_eq!(receiver.local_node_id, receiver_node_ids[idx]);
            let flow = repository::get_flow(&receiver.db, "11101")
                .expect("get persisted restart fanout flow")
                .expect("persisted restart fanout flow");
            let outbox = source
                .ledger
                .get_outbox_entry(
                    SyncTarget::new(receiver.local_node_id.clone()).expect("target"),
                    capture_op_id,
                )
                .expect("persisted fanout outbox after restart");
            assert_eq!(flow.name, "Full Restart Fanout Flow");
            assert!(outbox.acknowledged);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn multi_node_mesh_offline_outbox_survives_source_runtime_restart() {
        let _guard = test_home_guard();
        let tmp = tempfile::tempdir().expect("home");
        let _home_guard = EnvVarGuard::set_path("TENTAFLOW_HOME", tmp.path());
        let source_dir = tempfile::tempdir().expect("source dir");
        let source_db_path = source_dir.path().join("source.db");
        let source_ledger_path = source_dir.path().join("ledger");
        let receiver = make_runtime(90);

        let (source_node_id, capture) = {
            let source = make_runtime_from_paths(&source_db_path, &source_ledger_path, 91);
            seed_core_authority_target(&source.db, "core.flow", &receiver.runtime.local_node_id);
            let capture = source
                .record_core_capture(complete_core_flow_capture("91", "Restart Durable Flow"))
                .expect("record durable flow");
            assert_eq!(capture.queued_targets, 1);
            let queued = source
                .ledger
                .get_outbox_entry(
                    SyncTarget::new(receiver.runtime.local_node_id.clone()).expect("target"),
                    capture.op_id,
                )
                .expect("queued restart outbox");
            assert!(!queued.acknowledged);
            (source.local_node_id.clone(), capture)
        };

        let source = make_runtime_from_paths(&source_db_path, &source_ledger_path, 91);
        assert_eq!(source.local_node_id, source_node_id);
        seed_core_authority_target(
            &receiver.runtime.db,
            "core.flow",
            &receiver.runtime.local_node_id,
        );
        trust_each_other(&source, &receiver.runtime);

        let (source_mesh, receiver_mesh, mut source_events, mut receiver_events) =
            connected_mesh_pair(&source, &receiver.runtime).await;

        crate::mesh::pipeline::run_sync_repair_scheduler_tick_with(
            source_mesh.as_ref(),
            source.signer.security.as_ref(),
            |peer_id| source.build_push_payload_for_target(peer_id, 128),
            |peer_id| source.build_repair_pull_payloads_for_peer(peer_id, 16, 256),
        )
        .await;

        let received_push = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match receiver_events.recv().await {
                    Ok(IrohMeshEvent::SyncPushReceived { from_node_id, data })
                        if from_node_id == source.local_node_id =>
                    {
                        return data;
                    }
                    Ok(_) | Err(_) => continue,
                }
            }
        })
        .await
        .expect("restart push event");
        let received_push = tentaflow_protocol::cbor::decode::<
            tentaflow_protocol::mesh::MeshSyncPushPayload,
        >(&received_push)
        .expect("decode restart push");
        let ack = receiver
            .runtime
            .handle_push_payload(&source.local_node_id, received_push)
            .expect("handle restart push");
        let ack_bytes = tentaflow_protocol::cbor::encode(&ack).expect("encode restart ack");
        receiver_mesh
            .send_ufp2_to_peer(
                &source.local_node_id,
                tentaflow_protocol::mesh::MESH_MSG_SYNC_ACK,
                &ack_bytes,
            )
            .await
            .expect("send restart ack");

        let received_ack = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match source_events.recv().await {
                    Ok(IrohMeshEvent::SyncAckReceived { from_node_id, data })
                        if from_node_id == receiver.runtime.local_node_id =>
                    {
                        return data;
                    }
                    Ok(_) | Err(_) => continue,
                }
            }
        })
        .await
        .expect("restart ack event");
        let received_ack = tentaflow_protocol::cbor::decode::<
            tentaflow_protocol::mesh::MeshSyncAckPayload,
        >(&received_ack)
        .expect("decode restart ack");
        source
            .handle_ack_payload(&receiver.runtime.local_node_id, received_ack)
            .expect("handle restart ack");

        let flow = repository::get_flow(&receiver.runtime.db, "91")
            .expect("get restart flow")
            .expect("restart flow");
        let outbox = source
            .ledger
            .get_outbox_entry(
                SyncTarget::new(receiver.runtime.local_node_id.clone()).expect("target"),
                capture.op_id,
            )
            .expect("restart outbox");

        source_mesh.shutdown().await;
        receiver_mesh.shutdown().await;

        assert_eq!(flow.name, "Restart Durable Flow");
        assert!(outbox.acknowledged);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn multi_node_mesh_repair_pull_materializes_missing_core_flow_operations() {
        let _guard = test_home_guard();
        let tmp = tempfile::tempdir().expect("home");
        let _home_guard = EnvVarGuard::set_path("TENTAFLOW_HOME", tmp.path());
        let source = make_runtime(73);
        let receiver = make_runtime(74);
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
        trust_each_other(&source.runtime, &receiver.runtime);

        let (source_mesh, receiver_mesh, mut source_events, mut receiver_events) =
            connected_mesh_pair(&source.runtime, &receiver.runtime).await;

        let insert = source
            .runtime
            .record_core_capture(complete_core_flow_capture("73", "Initial Mesh Flow"))
            .expect("record insert");
        let update = source
            .runtime
            .record_core_capture(core_flow_update_capture("73", "Repaired Mesh Flow"))
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
        let gap_bytes =
            tentaflow_protocol::cbor::encode(&gap_payload).expect("encode gap response");
        source_mesh
            .send_ufp2_to_peer(
                &receiver.runtime.local_node_id,
                tentaflow_protocol::mesh::MESH_MSG_SYNC_PULL_RESPONSE,
                &gap_bytes,
            )
            .await
            .expect("send gap response");

        let received_gap = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match receiver_events.recv().await {
                    Ok(IrohMeshEvent::SyncPullResponseReceived { from_node_id, data })
                        if from_node_id == source.runtime.local_node_id =>
                    {
                        return data;
                    }
                    Ok(_) | Err(_) => continue,
                }
            }
        })
        .await
        .expect("gap response event");
        let received_gap = tentaflow_protocol::cbor::decode::<
            tentaflow_protocol::mesh::MeshSyncPullResponsePayload,
        >(&received_gap)
        .expect("decode gap response");
        receiver
            .runtime
            .handle_pull_response_payload(&source.runtime.local_node_id, received_gap)
            .expect_err("gap must queue repair");
        let repair_pulls = receiver
            .runtime
            .build_repair_pull_payloads_for_peer(&source.runtime.local_node_id, 8, 64)
            .expect("repair pulls");
        assert_eq!(repair_pulls.len(), 1);
        assert_eq!(repair_pulls[0].from_sequence, 1);
        let pull_bytes =
            tentaflow_protocol::cbor::encode(&repair_pulls[0]).expect("encode repair pull");
        receiver_mesh
            .send_ufp2_to_peer(
                &source.runtime.local_node_id,
                tentaflow_protocol::mesh::MESH_MSG_SYNC_PULL,
                &pull_bytes,
            )
            .await
            .expect("send repair pull");

        let received_pull = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match source_events.recv().await {
                    Ok(IrohMeshEvent::SyncPullReceived { from_node_id, data })
                        if from_node_id == receiver.runtime.local_node_id =>
                    {
                        return data;
                    }
                    Ok(_) | Err(_) => continue,
                }
            }
        })
        .await
        .expect("repair pull event");
        let received_pull = tentaflow_protocol::cbor::decode::<
            tentaflow_protocol::mesh::MeshSyncPullPayload,
        >(&received_pull)
        .expect("decode repair pull");
        let repair_response = source
            .runtime
            .handle_pull_payload(&receiver.runtime.local_node_id, received_pull)
            .expect("handle repair pull");
        let MeshSyncPullResult::Operations(repair_response) = repair_response else {
            panic!("expected operations repair response");
        };
        assert_eq!(repair_response.operations.len(), 2);
        let repair_response_bytes =
            tentaflow_protocol::cbor::encode(&repair_response).expect("encode repair response");
        source_mesh
            .send_ufp2_to_peer(
                &receiver.runtime.local_node_id,
                tentaflow_protocol::mesh::MESH_MSG_SYNC_PULL_RESPONSE,
                &repair_response_bytes,
            )
            .await
            .expect("send repair response");

        let received_repair = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match receiver_events.recv().await {
                    Ok(IrohMeshEvent::SyncPullResponseReceived { from_node_id, data })
                        if from_node_id == source.runtime.local_node_id =>
                    {
                        return data;
                    }
                    Ok(_) | Err(_) => continue,
                }
            }
        })
        .await
        .expect("repair response event");
        let received_repair = tentaflow_protocol::cbor::decode::<
            tentaflow_protocol::mesh::MeshSyncPullResponsePayload,
        >(&received_repair)
        .expect("decode repair response");
        let ack = receiver
            .runtime
            .handle_pull_response_payload(&source.runtime.local_node_id, received_repair)
            .expect("handle repair response");
        let ack_bytes = tentaflow_protocol::cbor::encode(&ack).expect("encode repair ack");
        receiver_mesh
            .send_ufp2_to_peer(
                &source.runtime.local_node_id,
                tentaflow_protocol::mesh::MESH_MSG_SYNC_ACK,
                &ack_bytes,
            )
            .await
            .expect("send repair ack");

        let received_ack = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match source_events.recv().await {
                    Ok(IrohMeshEvent::SyncAckReceived { from_node_id, data })
                        if from_node_id == receiver.runtime.local_node_id =>
                    {
                        return data;
                    }
                    Ok(_) | Err(_) => continue,
                }
            }
        })
        .await
        .expect("repair ack event");
        let received_ack = tentaflow_protocol::cbor::decode::<
            tentaflow_protocol::mesh::MeshSyncAckPayload,
        >(&received_ack)
        .expect("decode repair ack");
        source
            .runtime
            .handle_ack_payload(&receiver.runtime.local_node_id, received_ack)
            .expect("handle repair ack");

        let flow = repository::get_flow(&receiver.runtime.db, "73")
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
        let queued_repairs = receiver
            .runtime
            .ledger
            .list_due_repair_requests(
                PeerId::new(source.runtime.local_node_id.clone()).expect("peer"),
                i64::MAX,
                8,
            )
            .expect("queued repairs");

        source_mesh.shutdown().await;
        receiver_mesh.shutdown().await;

        assert_eq!(flow.name, "Repaired Mesh Flow");
        assert_eq!(flow.flow_json, r#"{"nodes":[{"id":"repaired"}]}"#);
        assert!(queued_repairs.is_empty());
        assert!(insert_outbox.acknowledged);
        assert!(update_outbox.acknowledged);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn multi_node_mesh_repair_scheduler_recovers_gap_after_reconnect() {
        let _guard = test_home_guard();
        let tmp = tempfile::tempdir().expect("home");
        let _home_guard = EnvVarGuard::set_path("TENTAFLOW_HOME", tmp.path());
        let source = make_runtime(84);
        let receiver = make_runtime(85);
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
        trust_each_other(&source.runtime, &receiver.runtime);

        let (source_mesh, receiver_mesh, _source_events, mut receiver_events) =
            connected_mesh_pair(&source.runtime, &receiver.runtime).await;

        let insert = source
            .runtime
            .record_core_capture(complete_core_flow_capture("84", "Scheduler Initial Flow"))
            .expect("record insert");
        let update = source
            .runtime
            .record_core_capture(core_flow_update_capture("84", "Scheduler Repaired Flow"))
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
        let gap_bytes =
            tentaflow_protocol::cbor::encode(&gap_payload).expect("encode gap response");
        source_mesh
            .send_ufp2_to_peer(
                &receiver.runtime.local_node_id,
                tentaflow_protocol::mesh::MESH_MSG_SYNC_PULL_RESPONSE,
                &gap_bytes,
            )
            .await
            .expect("send gap response");

        let received_gap = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match receiver_events.recv().await {
                    Ok(IrohMeshEvent::SyncPullResponseReceived { from_node_id, data })
                        if from_node_id == source.runtime.local_node_id =>
                    {
                        return data;
                    }
                    Ok(_) | Err(_) => continue,
                }
            }
        })
        .await
        .expect("gap response event");
        let received_gap = tentaflow_protocol::cbor::decode::<
            tentaflow_protocol::mesh::MeshSyncPullResponsePayload,
        >(&received_gap)
        .expect("decode gap response");
        receiver
            .runtime
            .handle_pull_response_payload(&source.runtime.local_node_id, received_gap)
            .expect_err("gap must queue repair");

        let source_addr = loopback_addr_of(&source_mesh);
        let receiver_addr = loopback_addr_of(&receiver_mesh);
        source_mesh
            .disconnect_peer(&receiver.runtime.local_node_id)
            .await;
        receiver_mesh
            .disconnect_peer(&source.runtime.local_node_id)
            .await;
        let source_reconnect =
            source_mesh.connect_to_peer_direct(&receiver.runtime.local_node_id, receiver_addr);
        let receiver_reconnect =
            receiver_mesh.connect_to_peer_direct(&source.runtime.local_node_id, source_addr);
        let (source_reconnect, receiver_reconnect) =
            tokio::time::timeout(Duration::from_secs(10), async {
                tokio::join!(source_reconnect, receiver_reconnect)
            })
            .await
            .expect("reconnect timeout");
        source_reconnect.expect("source reconnect");
        receiver_reconnect.expect("receiver reconnect");
        wait_connected(&source_mesh, &receiver.runtime.local_node_id).await;
        wait_connected(&receiver_mesh, &source.runtime.local_node_id).await;
        let mut source_events = source_mesh.subscribe();
        receiver_events = receiver_mesh.subscribe();
        tokio::time::sleep(Duration::from_millis(100)).await;

        crate::mesh::pipeline::run_sync_repair_scheduler_tick_with(
            receiver_mesh.as_ref(),
            receiver.runtime.signer.security.as_ref(),
            |peer_id| receiver.runtime.build_push_payload_for_target(peer_id, 128),
            |peer_id| {
                receiver
                    .runtime
                    .build_repair_pull_payloads_for_peer(peer_id, 16, 256)
            },
        )
        .await;

        let received_pull = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match source_events.recv().await {
                    Ok(IrohMeshEvent::SyncPullReceived { from_node_id, data })
                        if from_node_id == receiver.runtime.local_node_id =>
                    {
                        return data;
                    }
                    Ok(_) | Err(_) => continue,
                }
            }
        })
        .await
        .expect("scheduler repair pull event");
        let received_pull = tentaflow_protocol::cbor::decode::<
            tentaflow_protocol::mesh::MeshSyncPullPayload,
        >(&received_pull)
        .expect("decode scheduler repair pull");
        assert_eq!(received_pull.from_sequence, 1);
        let repair_response = source
            .runtime
            .handle_pull_payload(&receiver.runtime.local_node_id, received_pull)
            .expect("handle scheduler repair pull");
        let MeshSyncPullResult::Operations(repair_response) = repair_response else {
            panic!("expected operations repair response");
        };
        let repair_response_bytes = tentaflow_protocol::cbor::encode(&repair_response)
            .expect("encode scheduler repair response");
        source_mesh
            .send_ufp2_to_peer(
                &receiver.runtime.local_node_id,
                tentaflow_protocol::mesh::MESH_MSG_SYNC_PULL_RESPONSE,
                &repair_response_bytes,
            )
            .await
            .expect("send scheduler repair response");

        let received_repair = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match receiver_events.recv().await {
                    Ok(IrohMeshEvent::SyncPullResponseReceived { from_node_id, data })
                        if from_node_id == source.runtime.local_node_id =>
                    {
                        return data;
                    }
                    Ok(_) | Err(_) => continue,
                }
            }
        })
        .await
        .expect("scheduler repair response event");
        let received_repair = tentaflow_protocol::cbor::decode::<
            tentaflow_protocol::mesh::MeshSyncPullResponsePayload,
        >(&received_repair)
        .expect("decode scheduler repair response");
        let ack = receiver
            .runtime
            .handle_pull_response_payload(&source.runtime.local_node_id, received_repair)
            .expect("handle scheduler repair response");
        let ack_bytes =
            tentaflow_protocol::cbor::encode(&ack).expect("encode scheduler repair ack");
        receiver_mesh
            .send_ufp2_to_peer(
                &source.runtime.local_node_id,
                tentaflow_protocol::mesh::MESH_MSG_SYNC_ACK,
                &ack_bytes,
            )
            .await
            .expect("send scheduler repair ack");

        let received_ack = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match source_events.recv().await {
                    Ok(IrohMeshEvent::SyncAckReceived { from_node_id, data })
                        if from_node_id == receiver.runtime.local_node_id =>
                    {
                        return data;
                    }
                    Ok(_) | Err(_) => continue,
                }
            }
        })
        .await
        .expect("scheduler repair ack event");
        let received_ack = tentaflow_protocol::cbor::decode::<
            tentaflow_protocol::mesh::MeshSyncAckPayload,
        >(&received_ack)
        .expect("decode scheduler repair ack");
        source
            .runtime
            .handle_ack_payload(&receiver.runtime.local_node_id, received_ack)
            .expect("handle scheduler repair ack");

        let flow = repository::get_flow(&receiver.runtime.db, "84")
            .expect("get flow")
            .expect("flow");
        let queued_repairs = receiver
            .runtime
            .ledger
            .list_due_repair_requests(
                PeerId::new(source.runtime.local_node_id.clone()).expect("peer"),
                i64::MAX,
                8,
            )
            .expect("queued repairs");
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

        source_mesh.shutdown().await;
        receiver_mesh.shutdown().await;

        assert_eq!(flow.name, "Scheduler Repaired Flow");
        assert!(queued_repairs.is_empty());
        assert!(insert_outbox.acknowledged);
        assert!(update_outbox.acknowledged);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn multi_node_mesh_permission_revoke_stops_future_core_flow_push() {
        let _guard = test_home_guard();
        let tmp = tempfile::tempdir().expect("home");
        let _home_guard = EnvVarGuard::set_path("TENTAFLOW_HOME", tmp.path());
        let source = make_runtime(77);
        let receiver = make_runtime(78);
        let new_owner_node = make_runtime(79);
        let allowed_user_id = repository::create_user_account(
            &source.runtime.db,
            "mesh-revoked-user",
            "hash",
            "Mesh Revoked User",
            "mesh-revoked@example.com",
        )
        .expect("allowed user");
        let new_owner_id = repository::create_user_account(
            &source.runtime.db,
            "mesh-new-owner",
            "hash",
            "Mesh New Owner",
            "mesh-new-owner@example.com",
        )
        .expect("new owner");
        for (node_id, user_id, display_name) in [
            (
                receiver.runtime.local_node_id.as_str(),
                allowed_user_id.as_str(),
                "Mesh Revoked Node",
            ),
            (
                new_owner_node.runtime.local_node_id.as_str(),
                new_owner_id.as_str(),
                "Mesh New Owner Node",
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
            repository::assign_node_to_user(&source.runtime.db, node_id, user_id, "primary", None)
                .expect("assign node");
        }
        repository::upsert_sync_policy(
            &source.runtime.db,
            "policy-core-flow-mesh-revoke",
            "org-default",
            crate::sync::core_registry::CORE_SYNC_ADDON_ID,
            Some("core.flow"),
            None,
            "replicated_by_permission",
            None,
            None,
            true,
        )
        .expect("source sync policy");
        repository::upsert_sync_resource_acl(
            &source.runtime.db,
            "org-default",
            crate::sync::core_registry::CORE_SYNC_ADDON_ID,
            "core.flow",
            "77",
            Some(allowed_user_id.as_str()),
            Some(allowed_user_id.as_str()),
            None,
            None,
            "assigned",
        )
        .expect("initial acl");
        seed_core_authority_target(
            &receiver.runtime.db,
            "core.flow",
            &receiver.runtime.local_node_id,
        );
        trust_each_other(&source.runtime, &receiver.runtime);

        let (source_mesh, receiver_mesh, mut source_events, mut receiver_events) =
            connected_mesh_pair(&source.runtime, &receiver.runtime).await;

        let insert = source
            .runtime
            .record_core_capture(complete_core_flow_capture("77", "Visible Before Revoke"))
            .expect("record insert");
        let push = source
            .runtime
            .build_push_payload_for_target(&receiver.runtime.local_node_id, 16)
            .expect("build initial push")
            .expect("initial push");
        let push_bytes = tentaflow_protocol::cbor::encode(&push).expect("encode initial push");
        source_mesh
            .send_ufp2_to_peer(
                &receiver.runtime.local_node_id,
                tentaflow_protocol::mesh::MESH_MSG_SYNC_PUSH,
                &push_bytes,
            )
            .await
            .expect("send initial push");

        let received_push = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match receiver_events.recv().await {
                    Ok(IrohMeshEvent::SyncPushReceived { from_node_id, data })
                        if from_node_id == source.runtime.local_node_id =>
                    {
                        return data;
                    }
                    Ok(_) | Err(_) => continue,
                }
            }
        })
        .await
        .expect("initial push event");
        let received_push = tentaflow_protocol::cbor::decode::<
            tentaflow_protocol::mesh::MeshSyncPushPayload,
        >(&received_push)
        .expect("decode initial push");
        let ack = receiver
            .runtime
            .handle_push_payload(&source.runtime.local_node_id, received_push)
            .expect("handle initial push");
        let ack_bytes = tentaflow_protocol::cbor::encode(&ack).expect("encode initial ack");
        receiver_mesh
            .send_ufp2_to_peer(
                &source.runtime.local_node_id,
                tentaflow_protocol::mesh::MESH_MSG_SYNC_ACK,
                &ack_bytes,
            )
            .await
            .expect("send initial ack");

        let received_ack = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match source_events.recv().await {
                    Ok(IrohMeshEvent::SyncAckReceived { from_node_id, data })
                        if from_node_id == receiver.runtime.local_node_id =>
                    {
                        return data;
                    }
                    Ok(_) | Err(_) => continue,
                }
            }
        })
        .await
        .expect("initial ack event");
        let received_ack = tentaflow_protocol::cbor::decode::<
            tentaflow_protocol::mesh::MeshSyncAckPayload,
        >(&received_ack)
        .expect("decode initial ack");
        source
            .runtime
            .handle_ack_payload(&receiver.runtime.local_node_id, received_ack)
            .expect("handle initial ack");

        repository::upsert_sync_resource_acl(
            &source.runtime.db,
            "org-default",
            crate::sync::core_registry::CORE_SYNC_ADDON_ID,
            "core.flow",
            "77",
            Some(new_owner_id.as_str()),
            Some(new_owner_id.as_str()),
            None,
            None,
            "assigned",
        )
        .expect("revoked acl");
        let update = source
            .runtime
            .record_core_capture(core_flow_update_capture("77", "Hidden After Revoke"))
            .expect("record update after revoke");
        let revoked_push = source
            .runtime
            .build_push_payload_for_target(&receiver.runtime.local_node_id, 16)
            .expect("build revoked push");

        let flow = repository::get_flow(&receiver.runtime.db, "77")
            .expect("get flow")
            .expect("flow");
        let target = SyncTarget::new(receiver.runtime.local_node_id.clone()).expect("target");
        let insert_outbox = source
            .runtime
            .ledger
            .get_outbox_entry(target.clone(), insert.op_id)
            .expect("insert outbox");
        let update_outbox = source.runtime.ledger.get_outbox_entry(target, update.op_id);

        source_mesh.shutdown().await;
        receiver_mesh.shutdown().await;

        assert!(revoked_push.is_none());
        assert_eq!(flow.name, "Visible Before Revoke");
        assert!(insert_outbox.acknowledged);
        assert!(update_outbox.is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn multi_node_mesh_kv_push_materializes_storage_on_receiver() {
        let _guard = test_home_guard();
        let tmp = tempfile::tempdir().expect("home");
        let _home_guard = EnvVarGuard::set_path("TENTAFLOW_HOME", tmp.path());
        let source = make_runtime(80);
        let receiver = make_runtime(81);
        let addon_id = unique_addon_id("mesh-kv");
        seed_authority_target_for_resource(
            &source.runtime.db,
            &addon_id,
            "addon.kv",
            &receiver.runtime.local_node_id,
        );
        seed_authority_target_for_resource(
            &receiver.runtime.db,
            &addon_id,
            "addon.kv",
            &receiver.runtime.local_node_id,
        );
        trust_each_other(&source.runtime, &receiver.runtime);

        let (source_mesh, receiver_mesh, mut source_events, mut receiver_events) =
            connected_mesh_pair(&source.runtime, &receiver.runtime).await;

        let value = format!("dark-{}", std::process::id()).into_bytes();
        let result = source
            .runtime
            .record_kv_capture(kv_capture(&addon_id, "inst-1", "settings/theme", &value))
            .expect("record kv capture");
        let push = source
            .runtime
            .build_push_payload_for_target(&receiver.runtime.local_node_id, 16)
            .expect("build kv push")
            .expect("kv push");
        send_push_and_ack_over_mesh(
            &source.runtime,
            &receiver.runtime,
            &source_mesh,
            &receiver_mesh,
            &mut source_events,
            &mut receiver_events,
            push,
        )
        .await;

        let stored: Vec<u8> = receiver
            .runtime
            .db
            .lock()
            .expect("db lock")
            .query_row(
                "SELECT storage_value FROM addon_storage \
                 WHERE addon_id = ?1 AND instance_id = ?2 AND storage_key = ?3",
                rusqlite::params![addon_id, "inst-1", "settings/theme"],
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

        source_mesh.shutdown().await;
        receiver_mesh.shutdown().await;

        assert_eq!(stored, value);
        assert!(outbox.acknowledged);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn multi_node_mesh_chunked_blob_push_materializes_file_on_receiver() {
        let _guard = test_home_guard();
        let tmp = tempfile::tempdir().expect("home");
        let _home_guard = EnvVarGuard::set_path("TENTAFLOW_HOME", tmp.path());
        let source = make_runtime(82);
        let receiver = make_runtime(83);
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
        trust_each_other(&source.runtime, &receiver.runtime);

        let (source_mesh, receiver_mesh, mut source_events, mut receiver_events) =
            connected_mesh_pair(&source.runtime, &receiver.runtime).await;

        let salt = unique_addon_id("mesh-blob");
        let mut bytes = Vec::with_capacity(BLOB_SYNC_CHUNK_SIZE * 2 + 17);
        for idx in 0..(BLOB_SYNC_CHUNK_SIZE * 2 + 17) {
            bytes.push(((idx + salt.len()) % 251) as u8);
        }
        bytes[..salt.len()].copy_from_slice(salt.as_bytes());
        let sha = hex::encode(sha256(&bytes));
        let blob_source_dir = tempfile::tempdir().expect("blob dir");
        let blob_source_path = blob_source_dir.path().join("payload.bin");
        std::fs::write(&blob_source_path, &bytes).expect("blob write");
        let capture = crate::sync::blob_capture::BlobWriteCapture::new(
            "org-default",
            &format!("blob-{salt}"),
            &sha,
            "application/octet-stream",
            bytes.len() as u64,
            blob_source_path.to_string_lossy().to_string(),
            Some("00000000-0000-0000-0000-000000000007".to_string()),
        );

        let result = source
            .runtime
            .record_blob_capture(capture)
            .expect("record blob capture");
        let push = source
            .runtime
            .build_push_payload_for_target(&receiver.runtime.local_node_id, 16)
            .expect("build blob push")
            .expect("blob push");
        assert_eq!(push.operations.len(), 4);
        send_push_and_ack_over_mesh(
            &source.runtime,
            &receiver.runtime,
            &source_mesh,
            &receiver_mesh,
            &mut source_events,
            &mut receiver_events,
            push,
        )
        .await;

        let target_path = blob_path_for_sha(&sha).expect("blob path");
        let stored = std::fs::read(target_path).expect("stored blob");
        let outbox = source
            .runtime
            .ledger
            .get_outbox_entry(
                SyncTarget::new(receiver.runtime.local_node_id.clone()).expect("target"),
                result.op_id,
            )
            .expect("manifest outbox");

        source_mesh.shutdown().await;
        receiver_mesh.shutdown().await;

        assert_eq!(stored, bytes);
        assert!(!blob_chunk_dir(&sha).expect("chunk dir").exists());
        assert!(outbox.acknowledged);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn multi_node_mesh_snapshot_response_restores_compacted_sql_prefix() {
        let _guard = test_home_guard();
        let tmp = tempfile::tempdir().expect("home");
        let _home_guard = EnvVarGuard::set_path("TENTAFLOW_HOME", tmp.path());
        let source = make_runtime(75);
        let receiver = make_runtime(76);
        let addon_id = unique_addon_id("mesh-snap");
        seed_authority_target(
            &source.runtime.db,
            &addon_id,
            &receiver.runtime.local_node_id,
        );
        seed_authority_target(
            &receiver.runtime.db,
            &addon_id,
            &receiver.runtime.local_node_id,
        );
        trust_each_other(&source.runtime, &receiver.runtime);
        open_contacts_table(&addon_id);

        let (source_mesh, receiver_mesh, mut source_events, mut receiver_events) =
            connected_mesh_pair(&source.runtime, &receiver.runtime).await;

        let first = source
            .runtime
            .record_sql_capture(capture(&addon_id, "person-1", "Ala"))
            .expect("record first");
        let second = source
            .runtime
            .record_sql_capture(update_capture(&addon_id, "person-1", "Ala Nowak"))
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
            .expect("ack first before compaction");
        source
            .runtime
            .ledger
            .compact(CompactionPolicy {
                partition_id: partition.clone(),
                keep_operations_after_sequence: Some(2),
            })
            .expect("compact");
        receiver
            .runtime
            .queue_repair_request(&source.runtime.local_node_id, partition.as_str(), 1);

        let pull = MeshSyncPullPayload {
            from_node_id: receiver.runtime.local_node_id.clone(),
            partition_id: partition.as_str().to_string(),
            from_sequence: 1,
            limit: 64,
        };
        let pull_bytes = tentaflow_protocol::cbor::encode(&pull).expect("encode snapshot pull");
        receiver_mesh
            .send_ufp2_to_peer(
                &source.runtime.local_node_id,
                tentaflow_protocol::mesh::MESH_MSG_SYNC_PULL,
                &pull_bytes,
            )
            .await
            .expect("send snapshot pull");

        let received_pull = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match source_events.recv().await {
                    Ok(IrohMeshEvent::SyncPullReceived { from_node_id, data })
                        if from_node_id == receiver.runtime.local_node_id =>
                    {
                        return data;
                    }
                    Ok(_) | Err(_) => continue,
                }
            }
        })
        .await
        .expect("snapshot pull event");
        let received_pull = tentaflow_protocol::cbor::decode::<
            tentaflow_protocol::mesh::MeshSyncPullPayload,
        >(&received_pull)
        .expect("decode snapshot pull");
        let response = source
            .runtime
            .handle_pull_payload(&receiver.runtime.local_node_id, received_pull)
            .expect("handle snapshot pull");
        let MeshSyncPullResult::Snapshot(snapshot_response) = response else {
            panic!("expected snapshot response");
        };
        assert_eq!(snapshot_response.snapshot_id, snapshot.snapshot_id.as_str());
        assert_eq!(snapshot_response.operations_after_snapshot.len(), 1);
        let response_bytes =
            tentaflow_protocol::cbor::encode(&snapshot_response).expect("encode snapshot response");
        source_mesh
            .send_ufp2_to_peer(
                &receiver.runtime.local_node_id,
                tentaflow_protocol::mesh::MESH_MSG_SYNC_SNAPSHOT_RESPONSE,
                &response_bytes,
            )
            .await
            .expect("send snapshot response");

        let received_response = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match receiver_events.recv().await {
                    Ok(IrohMeshEvent::SyncSnapshotResponseReceived { from_node_id, data })
                        if from_node_id == source.runtime.local_node_id =>
                    {
                        return data;
                    }
                    Ok(_) | Err(_) => continue,
                }
            }
        })
        .await
        .expect("snapshot response event");
        let received_response = tentaflow_protocol::cbor::decode::<
            tentaflow_protocol::mesh::MeshSyncSnapshotResponsePayload,
        >(&received_response)
        .expect("decode snapshot response");
        let ack = receiver
            .runtime
            .handle_snapshot_response_payload(&source.runtime.local_node_id, received_response)
            .expect("handle snapshot response");
        let ack_bytes = tentaflow_protocol::cbor::encode(&ack).expect("encode snapshot ack");
        receiver_mesh
            .send_ufp2_to_peer(
                &source.runtime.local_node_id,
                tentaflow_protocol::mesh::MESH_MSG_SYNC_ACK,
                &ack_bytes,
            )
            .await
            .expect("send snapshot ack");

        let received_ack = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match source_events.recv().await {
                    Ok(IrohMeshEvent::SyncAckReceived { from_node_id, data })
                        if from_node_id == receiver.runtime.local_node_id =>
                    {
                        return data;
                    }
                    Ok(_) | Err(_) => continue,
                }
            }
        })
        .await
        .expect("snapshot ack event");
        let received_ack = tentaflow_protocol::cbor::decode::<
            tentaflow_protocol::mesh::MeshSyncAckPayload,
        >(&received_ack)
        .expect("decode snapshot ack");
        source
            .runtime
            .handle_ack_payload(&receiver.runtime.local_node_id, received_ack)
            .expect("handle snapshot ack");

        let queued_repairs = receiver
            .runtime
            .ledger
            .list_due_repair_requests(
                PeerId::new(source.runtime.local_node_id.clone()).expect("peer"),
                i64::MAX,
                8,
            )
            .expect("queued repairs");
        let pool = crate::addon::storage_sql::open_addon_db("org-default", &addon_id)
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

        source_mesh.shutdown().await;
        receiver_mesh.shutdown().await;

        assert_eq!(name, "Ala Nowak");
        assert!(queued_repairs.is_empty());
        assert!(first_outbox.acknowledged);
        assert!(second_outbox.acknowledged);
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

    #[test]
    fn receiver_restart_applies_persisted_inbox_and_acks_source_outbox() {
        let tmp = tempfile::tempdir().expect("tmp");
        let source = make_runtime(91);
        let receiver_db = tmp.path().join("receiver.db");
        let receiver_ledger = tmp.path().join("receiver-ledger");
        let receiver = make_runtime_from_paths(&receiver_db, &receiver_ledger, 92);
        seed_core_authority_target(&source.runtime.db, "core.flow", &receiver.local_node_id);
        seed_core_authority_target(&receiver.db, "core.flow", &receiver.local_node_id);

        let result = source
            .runtime
            .record_core_capture(complete_core_flow_capture("9101", "Restart inbox flow"))
            .expect("record core flow");
        let push = source
            .runtime
            .build_push_payload_for_target(&receiver.local_node_id, 16)
            .expect("build push")
            .expect("push");
        let operation_ids = receiver
            .store_incoming_operations(&source.runtime.local_node_id, push.operations)
            .expect("store inbox");
        drop(receiver);

        let receiver = make_runtime_from_paths(&receiver_db, &receiver_ledger, 92);
        let applied = receiver.apply_unapplied_inbox(16).expect("apply inbox");
        source
            .runtime
            .handle_ack_payload(&receiver.local_node_id, ack_for(&receiver, operation_ids))
            .expect("ack after restart");
        let conn = receiver.db.lock().expect("db");
        let name: String = conn
            .query_row("SELECT name FROM flows WHERE id = '9101'", [], |row| {
                row.get(0)
            })
            .expect("flow");
        drop(conn);
        let outbox = source
            .runtime
            .ledger
            .get_outbox_entry(
                SyncTarget::new(receiver.local_node_id.clone()).expect("target"),
                result.op_id,
            )
            .expect("outbox");

        assert_eq!(applied, 1);
        assert_eq!(name, "Restart inbox flow");
        assert!(outbox.acknowledged);
    }

    #[test]
    fn chunked_blob_restart_completes_from_persisted_partial_inbox() {
        let _guard = test_home_guard();
        let tmp = tempfile::tempdir().expect("home");
        let _home_guard = EnvVarGuard::set_path("TENTAFLOW_HOME", tmp.path());
        let source = make_runtime(93);
        let receiver_db = tmp.path().join("receiver.db");
        let receiver_ledger = tmp.path().join("receiver-ledger");
        let receiver = make_runtime_from_paths(&receiver_db, &receiver_ledger, 94);
        seed_core_authority_target(&source.runtime.db, "core.blob", &receiver.local_node_id);
        seed_core_authority_target(&receiver.db, "core.blob", &receiver.local_node_id);

        let mut bytes = Vec::with_capacity(BLOB_SYNC_CHUNK_SIZE * 2 + 23);
        for idx in 0..(BLOB_SYNC_CHUNK_SIZE * 2 + 23) {
            bytes.push((idx % 251) as u8);
        }
        let sha = hex::encode(sha256(&bytes));
        let source_dir = tempfile::tempdir().expect("source blob dir");
        let source_path = source_dir.path().join("payload.bin");
        std::fs::write(&source_path, &bytes).expect("write source blob");
        let capture = crate::sync::blob_capture::BlobWriteCapture::new(
            "org-default",
            "restart-blob",
            &sha,
            "application/octet-stream",
            bytes.len() as u64,
            source_path.to_string_lossy().to_string(),
            Some("00000000-0000-0000-0000-000000000007".to_string()),
        );
        let result = source
            .runtime
            .record_blob_capture(capture)
            .expect("record blob");
        let push = source
            .runtime
            .build_push_payload_for_target(&receiver.local_node_id, 16)
            .expect("build push")
            .expect("push");
        assert_eq!(push.operations.len(), 4);
        let first_ids = receiver
            .store_incoming_operations(&source.runtime.local_node_id, push.operations[..2].to_vec())
            .expect("store first chunks");
        assert_eq!(receiver.apply_unapplied_inbox(16).expect("apply chunks"), 2);
        assert!(blob_chunk_dir(&sha).expect("chunk dir").exists());
        drop(first_ids);
        drop(receiver);

        let receiver = make_runtime_from_paths(&receiver_db, &receiver_ledger, 94);
        let operation_ids = receiver
            .store_incoming_operations(&source.runtime.local_node_id, push.operations[2..].to_vec())
            .expect("store remaining blob operations");
        let applied = receiver.apply_unapplied_inbox(16).expect("apply manifest");
        source
            .runtime
            .handle_ack_payload(&receiver.local_node_id, ack_for(&receiver, operation_ids))
            .expect("ack blob after restart");
        let stored = std::fs::read(blob_path_for_sha(&sha).expect("blob path")).expect("blob");
        let outbox = source
            .runtime
            .ledger
            .get_outbox_entry(
                SyncTarget::new(receiver.local_node_id.clone()).expect("target"),
                result.op_id,
            )
            .expect("manifest outbox");

        assert_eq!(applied, 2);
        assert_eq!(stored, bytes);
        assert!(!blob_chunk_dir(&sha).expect("chunk dir").exists());
        assert!(outbox.acknowledged);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn multi_node_mesh_conflicting_sql_insert_records_conflict_without_overwrite() {
        let _guard = test_home_guard();
        let tmp = tempfile::tempdir().expect("home");
        let _home_guard = EnvVarGuard::set_path("TENTAFLOW_HOME", tmp.path());
        let source = make_runtime(95);
        let receiver = make_runtime(96);
        let addon_id = unique_addon_id("mesh-conflict");
        seed_authority_target(
            &source.runtime.db,
            &addon_id,
            &receiver.runtime.local_node_id,
        );
        seed_authority_target(
            &receiver.runtime.db,
            &addon_id,
            &receiver.runtime.local_node_id,
        );
        trust_each_other(&source.runtime, &receiver.runtime);
        open_contacts_table(&addon_id);
        let pool = crate::addon::storage_sql::open_addon_db("org-default", &addon_id)
            .expect("open addon db");
        pool.get()
            .expect("conn")
            .execute("INSERT INTO contacts (id, name) VALUES (1, 'Local')", [])
            .expect("seed local");
        let (source_mesh, receiver_mesh, mut source_events, mut receiver_events) =
            connected_mesh_pair(&source.runtime, &receiver.runtime).await;

        let result = source
            .runtime
            .record_sql_capture(capture(&addon_id, "person-conflict", "Remote"))
            .expect("record conflict capture");
        let push = source
            .runtime
            .build_push_payload_for_target(&receiver.runtime.local_node_id, 16)
            .expect("build push")
            .expect("push");
        send_push_and_ack_over_mesh(
            &source.runtime,
            &receiver.runtime,
            &source_mesh,
            &receiver_mesh,
            &mut source_events,
            &mut receiver_events,
            push,
        )
        .await;
        let conflicts = crate::addon::storage_sql_exec::list_sync_conflicts(
            "org-default",
            &addon_id,
            Some("open"),
            10,
        )
        .expect("conflicts");
        let name: String = pool
            .get()
            .expect("conn")
            .query_row("SELECT name FROM contacts WHERE id = 1", [], |row| {
                row.get(0)
            })
            .expect("local row");
        let outbox = source
            .runtime
            .ledger
            .get_outbox_entry(
                SyncTarget::new(receiver.runtime.local_node_id.clone()).expect("target"),
                result.op_id,
            )
            .expect("outbox");

        source_mesh.shutdown().await;
        receiver_mesh.shutdown().await;

        assert_eq!(name, "Local");
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].resource_id, "person-conflict");
        assert!(outbox.acknowledged);
    }

    #[test]
    fn addon_conflict_survives_receiver_restart_and_accepts_remote() {
        let _guard = test_home_guard();
        let tmp = tempfile::tempdir().expect("home");
        let _home_guard = EnvVarGuard::set_path("TENTAFLOW_HOME", tmp.path());
        let source = make_runtime(115);
        let receiver_db = tmp.path().join("receiver.db");
        let receiver_ledger = tmp.path().join("receiver-ledger");
        let addon_id = unique_addon_id("restart-conflict");
        let result = {
            let receiver = make_runtime_from_paths(&receiver_db, &receiver_ledger, 116);
            seed_authority_target(&source.runtime.db, &addon_id, &receiver.local_node_id);
            seed_authority_target(&receiver.db, &addon_id, &receiver.local_node_id);
            open_contacts_table(&addon_id);
            let pool = crate::addon::storage_sql::open_addon_db("org-default", &addon_id)
                .expect("open addon db");
            pool.get()
                .expect("conn")
                .execute("INSERT INTO contacts (id, name) VALUES (1, 'Local')", [])
                .expect("seed local");
            let result = source
                .runtime
                .record_sql_capture(capture(&addon_id, "person-restart-conflict", "Remote"))
                .expect("record conflict capture");
            let push = source
                .runtime
                .build_push_payload_for_target(&receiver.local_node_id, 16)
                .expect("build push")
                .expect("push");
            let ack = receiver
                .handle_push_payload(&source.runtime.local_node_id, push)
                .expect("handle push");
            source
                .runtime
                .handle_ack_payload(&receiver.local_node_id, ack)
                .expect("ack");

            let conflicts = crate::addon::storage_sql_exec::list_sync_conflicts(
                "org-default",
                &addon_id,
                Some("open"),
                10,
            )
            .expect("conflicts before restart");
            let name: String = pool
                .get()
                .expect("conn")
                .query_row("SELECT name FROM contacts WHERE id = 1", [], |row| {
                    row.get(0)
                })
                .expect("local row");
            let entry = receiver
                .ledger
                .get_inbox_entry(
                    PeerId::new(source.runtime.local_node_id.clone()).expect("peer"),
                    result.op_id,
                )
                .expect("inbox before restart");

            assert_eq!(conflicts.len(), 1);
            assert_eq!(conflicts[0].resource_id, "person-restart-conflict");
            assert_eq!(name, "Local");
            assert!(!entry.applied);
            assert!(entry.conflicted);
            result
        };

        let receiver = make_runtime_from_paths(&receiver_db, &receiver_ledger, 116);
        let conflicts = crate::addon::storage_sql_exec::list_sync_conflicts(
            "org-default",
            &addon_id,
            Some("open"),
            10,
        )
        .expect("conflicts after restart");
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].operation_id, result.op_id.to_hex());

        let resolved = receiver
            .resolve_addon_sync_conflict(
                "org-default",
                &addon_id,
                result.op_id,
                SyncConflictResolution::AcceptRemote,
            )
            .expect("resolve after restart");
        let conflicts = crate::addon::storage_sql_exec::list_sync_conflicts(
            "org-default",
            &addon_id,
            Some("open"),
            10,
        )
        .expect("conflicts resolved");
        let pool = crate::addon::storage_sql::open_addon_db("org-default", &addon_id)
            .expect("open addon db");
        let name: String = pool
            .get()
            .expect("conn")
            .query_row("SELECT name FROM contacts WHERE id = 1", [], |row| {
                row.get(0)
            })
            .expect("remote row");
        let entry = receiver
            .ledger
            .get_inbox_entry(
                PeerId::new(source.runtime.local_node_id.clone()).expect("peer"),
                result.op_id,
            )
            .expect("inbox after resolve");
        let outbox = source
            .runtime
            .ledger
            .get_outbox_entry(
                SyncTarget::new(receiver.local_node_id.clone()).expect("target"),
                result.op_id,
            )
            .expect("source outbox");

        assert_eq!(resolved.status, "resolved");
        assert_eq!(resolved.resolution, "accept_remote");
        assert_eq!(conflicts.len(), 0);
        assert_eq!(name, "Remote");
        assert!(entry.applied);
        assert!(!entry.conflicted);
        assert!(outbox.acknowledged);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn multi_node_mesh_partial_fanout_offline_target_catches_up_later() {
        let _guard = test_home_guard();
        let source = make_runtime(97);
        let receiver_a = make_runtime(98);
        let receiver_b = make_runtime(99);
        let receiver_c = make_runtime(100);
        let receivers = [&receiver_a, &receiver_b, &receiver_c];
        let mut receiver_user_ids = Vec::new();
        for (idx, receiver) in receivers.iter().enumerate() {
            let user_id = repository::create_user_account(
                &source.runtime.db,
                &format!("partial-fanout-user-{idx}"),
                "hash",
                &format!("Partial Fanout User {idx}"),
                &format!("partial-fanout-{idx}@example.test"),
            )
            .expect("partial fanout user");
            repository::upsert_sync_node_identity(
                &source.runtime.db,
                &receiver.runtime.local_node_id,
                &receiver.runtime.signer.security.public_key_hex(),
                "ed25519",
                &format!("Partial Fanout Node {idx}"),
                "laptop",
                "trusted",
                Some(user_id.as_str()),
                "standard",
            )
            .expect("partial fanout node");
            repository::assign_node_to_user(
                &source.runtime.db,
                &receiver.runtime.local_node_id,
                &user_id,
                "primary",
                None,
            )
            .expect("partial fanout node assignment");
            receiver_user_ids.push(user_id);
            seed_core_authority_target(
                &receiver.runtime.db,
                "core.flow",
                &receiver.runtime.local_node_id,
            );
            trust_each_other(&source.runtime, &receiver.runtime);
        }
        repository::upsert_sync_policy(
            &source.runtime.db,
            "policy-core-flow-partial-fanout",
            "org-default",
            crate::sync::core_registry::CORE_SYNC_ADDON_ID,
            Some("core.flow"),
            None,
            "replicated_by_permission",
            None,
            None,
            true,
        )
        .expect("partial fanout policy");
        repository::upsert_sync_resource_acl(
            &source.runtime.db,
            "org-default",
            crate::sync::core_registry::CORE_SYNC_ADDON_ID,
            "core.flow",
            "9701",
            receiver_user_ids.first().map(|s| s.as_str()),
            receiver_user_ids.first().map(|s| s.as_str()),
            None,
            None,
            "all",
        )
        .expect("partial fanout acl");

        let source_mesh = make_mesh_manager(&source.runtime).await;
        let receiver_a_mesh = make_mesh_manager(&receiver_a.runtime).await;
        let receiver_b_mesh = make_mesh_manager(&receiver_b.runtime).await;
        let _source_task = source_mesh.start();
        let _receiver_a_task = receiver_a_mesh.start();
        let _receiver_b_task = receiver_b_mesh.start();
        let mut source_events = source_mesh.subscribe();
        let mut receiver_a_events = receiver_a_mesh.subscribe();
        let mut receiver_b_events = receiver_b_mesh.subscribe();
        connect_mesh_managers(&source_mesh, &receiver_a_mesh).await;
        connect_mesh_managers(&source_mesh, &receiver_b_mesh).await;

        let result = source
            .runtime
            .record_core_capture(complete_core_flow_capture("9701", "Partial fanout flow"))
            .expect("record flow");
        for (receiver, receiver_mesh, receiver_events) in [
            (
                &receiver_a.runtime,
                &receiver_a_mesh,
                &mut receiver_a_events,
            ),
            (
                &receiver_b.runtime,
                &receiver_b_mesh,
                &mut receiver_b_events,
            ),
        ] {
            let push = source
                .runtime
                .build_push_payload_for_target(&receiver.local_node_id, 16)
                .expect("build push")
                .expect("push");
            send_push_and_ack_over_mesh(
                &source.runtime,
                receiver,
                &source_mesh,
                receiver_mesh,
                &mut source_events,
                receiver_events,
                push,
            )
            .await;
        }
        let c_target = SyncTarget::new(receiver_c.runtime.local_node_id.clone()).expect("target");
        let c_outbox = source
            .runtime
            .ledger
            .get_outbox_entry(c_target.clone(), result.op_id)
            .expect("c outbox before reconnect");
        assert!(!c_outbox.acknowledged);

        let receiver_c_mesh = make_mesh_manager(&receiver_c.runtime).await;
        let _receiver_c_task = receiver_c_mesh.start();
        let mut receiver_c_events = receiver_c_mesh.subscribe();
        connect_mesh_managers(&source_mesh, &receiver_c_mesh).await;
        let push = source
            .runtime
            .build_push_payload_for_target(&receiver_c.runtime.local_node_id, 16)
            .expect("build c push")
            .expect("c push");
        send_push_and_ack_over_mesh(
            &source.runtime,
            &receiver_c.runtime,
            &source_mesh,
            &receiver_c_mesh,
            &mut source_events,
            &mut receiver_c_events,
            push,
        )
        .await;
        let conn = receiver_c.runtime.db.lock().expect("db");
        let name: String = conn
            .query_row("SELECT name FROM flows WHERE id = '9701'", [], |row| {
                row.get(0)
            })
            .expect("flow on c");
        drop(conn);
        let c_outbox = source
            .runtime
            .ledger
            .get_outbox_entry(c_target, result.op_id)
            .expect("c outbox after reconnect");

        source_mesh.shutdown().await;
        receiver_a_mesh.shutdown().await;
        receiver_b_mesh.shutdown().await;
        receiver_c_mesh.shutdown().await;

        assert_eq!(name, "Partial fanout flow");
        assert!(c_outbox.acknowledged);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn multi_node_mesh_core_scope_materializes_identity_rbac_and_flow_bindings() {
        let source = make_runtime(101);
        let receiver = make_runtime(102);
        let resource_types = [
            "core.user_account",
            "core.user_group",
            "core.group_member",
            "core.role",
            "core.org_membership",
            "core.flow",
            "core.flow_model_binding",
        ];
        for resource_type in resource_types {
            seed_core_authority_target(
                &source.runtime.db,
                resource_type,
                &receiver.runtime.local_node_id,
            );
            seed_core_authority_target(
                &receiver.runtime.db,
                resource_type,
                &receiver.runtime.local_node_id,
            );
        }
        trust_each_other(&source.runtime, &receiver.runtime);
        let (source_mesh, receiver_mesh, mut source_events, mut receiver_events) =
            connected_mesh_pair(&source.runtime, &receiver.runtime).await;
        use crate::sync::core_registry::CoreSyncResourceKind as K;

        let mut role = BTreeMap::new();
        role.insert(
            "name".to_string(),
            FieldValue::String("Sync Role".to_string()),
        );
        role.insert(
            "permissions_json".to_string(),
            FieldValue::String("[]".to_string()),
        );
        source
            .runtime
            .record_core_capture(core_capture_for(K::Role, "sync-role", role))
            .expect("record role");
        let mut user = BTreeMap::new();
        user.insert(
            "username".to_string(),
            FieldValue::String("sync-user".to_string()),
        );
        user.insert(
            "display_name".to_string(),
            FieldValue::String("Sync User".to_string()),
        );
        user.insert(
            "email".to_string(),
            FieldValue::String("sync@example.test".to_string()),
        );
        user.insert("is_active".to_string(), FieldValue::Bool(true));
        user.insert("is_admin".to_string(), FieldValue::Bool(false));
        user.insert("role".to_string(), FieldValue::String("user".to_string()));
        source
            .runtime
            .record_core_capture(core_capture_for(K::UserAccount, "10101", user))
            .expect("record user");
        let mut group = BTreeMap::new();
        group.insert(
            "name".to_string(),
            FieldValue::String("Sync Group".to_string()),
        );
        group.insert(
            "description".to_string(),
            FieldValue::String("Synchronized".to_string()),
        );
        source
            .runtime
            .record_core_capture(core_capture_for(K::UserGroup, "10102", group))
            .expect("record group");
        let mut group_member = BTreeMap::new();
        group_member.insert(
            "group_id".to_string(),
            FieldValue::String("10102".to_string()),
        );
        group_member.insert(
            "user_id".to_string(),
            FieldValue::String("10101".to_string()),
        );
        source
            .runtime
            .record_core_capture(core_capture_for(
                K::GroupMember,
                "10102:10101",
                group_member,
            ))
            .expect("record group member");
        let mut membership = BTreeMap::new();
        membership.insert(
            "org_id".to_string(),
            FieldValue::String("org-default".to_string()),
        );
        membership.insert(
            "user_id".to_string(),
            FieldValue::String("10101".to_string()),
        );
        membership.insert(
            "role_id".to_string(),
            FieldValue::String("sync-role".to_string()),
        );
        membership.insert(
            "granted_by".to_string(),
            FieldValue::String("sync-test".to_string()),
        );
        source
            .runtime
            .record_core_capture(core_capture_for(
                K::OrgMembership,
                "org-default:10101",
                membership,
            ))
            .expect("record membership");
        source
            .runtime
            .record_core_capture(complete_core_flow_capture("10103", "Scoped flow"))
            .expect("record flow");
        let mut binding = BTreeMap::new();
        binding.insert(
            "flow_id".to_string(),
            FieldValue::String("10103".to_string()),
        );
        binding.insert(
            "model_pattern".to_string(),
            FieldValue::String("sync-model".to_string()),
        );
        binding.insert("priority".to_string(), FieldValue::I64(10));
        source
            .runtime
            .record_core_capture(core_capture_for(K::FlowModelBinding, "10104", binding))
            .expect("record binding");

        let push = source
            .runtime
            .build_push_payload_for_target(&receiver.runtime.local_node_id, 32)
            .expect("build push")
            .expect("push");
        send_push_and_ack_over_mesh(
            &source.runtime,
            &receiver.runtime,
            &source_mesh,
            &receiver_mesh,
            &mut source_events,
            &mut receiver_events,
            push,
        )
        .await;
        let mut group_member_retry = BTreeMap::new();
        group_member_retry.insert(
            "group_id".to_string(),
            FieldValue::String("10102".to_string()),
        );
        group_member_retry.insert(
            "user_id".to_string(),
            FieldValue::String("10101".to_string()),
        );
        source
            .runtime
            .record_core_capture(core_capture_for(
                K::GroupMember,
                "10102:10101:retry",
                group_member_retry,
            ))
            .expect("record group member retry");
        let push = source
            .runtime
            .build_push_payload_for_target(&receiver.runtime.local_node_id, 32)
            .expect("build retry push")
            .expect("retry push");
        send_push_and_ack_over_mesh(
            &source.runtime,
            &receiver.runtime,
            &source_mesh,
            &receiver_mesh,
            &mut source_events,
            &mut receiver_events,
            push,
        )
        .await;
        let conn = receiver.runtime.db.lock().expect("db");
        let username: String = conn
            .query_row(
                "SELECT username FROM user_accounts WHERE id = '10101'",
                [],
                |row| row.get(0),
            )
            .expect("user");
        let group_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM group_members WHERE group_id = '10102' AND user_id = '10101'",
                [],
                |row| row.get(0),
            )
            .expect("group member");
        let role_name: String = conn
            .query_row(
                "SELECT name FROM roles WHERE role_id = 'sync-role'",
                [],
                |row| row.get(0),
            )
            .expect("role");
        let membership_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM org_memberships WHERE org_id = 'org-default' AND user_id = '10101'", [], |row| row.get(0))
            .expect("membership");
        let binding_pattern: String = conn
            .query_row(
                "SELECT model_pattern FROM flow_model_bindings WHERE id = '10104'",
                [],
                |row| row.get(0),
            )
            .expect("binding");
        drop(conn);

        source_mesh.shutdown().await;
        receiver_mesh.shutdown().await;

        assert_eq!(username, "sync-user");
        assert_eq!(group_count, 1);
        assert_eq!(role_name, "Sync Role");
        assert_eq!(membership_count, 1);
        assert_eq!(binding_pattern, "sync-model");
    }

    /// After the v53 cutover marker is consumed: the epoch is bumped, the core
    /// outbox is re-seeded from the persisted capture journal under the new
    /// epoch, an operation stamped at the old (genesis) epoch is rejected by the
    /// inbox, and the one-shot marker is cleared so a second pass is a no-op.
    #[test]
    fn baseline_cutover_bumps_epoch_reseeds_outbox_and_rejects_stale_ops() {
        with_tmp_home(|| {
            let node = make_runtime(150);
            seed_core_authority_target(&node.runtime.db, "core.flow", "peer-authority");
            seed_actor_user(&node.runtime.db, "test-actor");

            // Seed a live flow row: the cutover re-seeds from CURRENT SQLite
            // state, not from the historical capture journal.
            seed_flow_row(&node.runtime.db, "cutover-flow", "Cutover Flow");

            // Genesis epoch before the cutover.
            let genesis = node.runtime.ledger.current_epoch().expect("genesis epoch");
            assert_eq!(genesis.counter, 0);

            // Arm the one-shot marker the v53 migration would have written.
            repository::set_setting(
                &node.runtime.db,
                crate::db::migrations::CORE_BASELINE_RESET_PENDING_KEY,
                "1",
            )
            .expect("arm marker");

            let reseeded = node
                .runtime
                .run_pending_baseline_cutover()
                .expect("cutover runs")
                .expect("cutover was pending");
            assert!(
                reseeded >= 1,
                "the persisted capture must re-seed at least one op"
            );

            // Epoch advanced to counter 1 with this node as origin.
            let new_epoch = node.runtime.ledger.current_epoch().expect("new epoch");
            assert_eq!(new_epoch.counter, 1);
            assert_eq!(new_epoch.origin_node, node.runtime.local_node_id);

            // The re-seeded operation carries the NEW epoch and a live outbox row.
            let push = node
                .runtime
                .build_push_payload_for_target("peer-authority", 16)
                .expect("build push")
                .expect("re-seeded op produces a payload");
            assert!(!push.operations.is_empty());
            for wire in &push.operations {
                let op = node
                    .runtime
                    .ledger
                    .get_operation(operation_id_from_wire(&wire.op_id).expect("op id"))
                    .expect("operation");
                assert_eq!(
                    op.body.epoch, new_epoch,
                    "re-seeded op must carry new epoch"
                );
            }

            // An operation minted at the stale genesis epoch (the pre-cutover
            // baseline a peer would still hold) is rejected on the way in.
            let stale_peer = make_runtime(151);
            assert_eq!(
                stale_peer
                    .runtime
                    .ledger
                    .current_epoch()
                    .expect("peer epoch"),
                genesis
            );
            let stale_op_id = stale_peer
                .runtime
                .record_core_capture(complete_core_flow_capture("stale-flow", "Stale Flow"))
                .expect("record stale op")
                .op_id;
            let stale_op = stale_peer
                .runtime
                .ledger
                .get_operation(stale_op_id)
                .expect("stale operation");
            assert_eq!(stale_op.body.epoch, genesis);

            let rejected = node.runtime.ledger.put_verified_in_inbox(
                PeerId::new("peer-authority".to_string()).expect("peer"),
                stale_op,
                &HexNodeIdOperationVerifier,
            );
            assert!(
                matches!(rejected, Err(SyncLedgerError::EpochMismatch { .. })),
                "stale-epoch op must be rejected after the cutover bump, got {rejected:?}"
            );

            // The one-shot marker is cleared, so a second pass is a no-op and the
            // epoch is not bumped again.
            assert!(node
                .runtime
                .run_pending_baseline_cutover()
                .expect("second pass runs")
                .is_none());
            assert_eq!(
                node.runtime
                    .ledger
                    .current_epoch()
                    .expect("epoch unchanged"),
                new_epoch
            );
        });
    }

    /// CRITICAL 1: the cutover re-seeds the CURRENT snapshot with strictly
    /// increasing, distinct, non-zero HLCs (one per live row), and a later
    /// update of the same resource still wins under LWW.
    #[test]
    fn baseline_cutover_reseeds_current_state_with_distinct_increasing_hlc() {
        with_tmp_home(|| {
            let node = make_runtime(160);
            seed_core_authority_target(&node.runtime.db, "core.flow", "peer-authority");
            seed_actor_user(&node.runtime.db, "test-actor");

            // Three live flows. A historical journal full of zeroed-HLC versions
            // must NOT influence the reseed — emulate the post-v54 state.
            seed_flow_row(&node.runtime.db, "flow-a", "Alpha");
            seed_flow_row(&node.runtime.db, "flow-b", "Beta");
            seed_flow_row(&node.runtime.db, "flow-c", "Gamma");
            {
                let conn = node.runtime.db.lock().expect("db");
                let tx = conn.unchecked_transaction().expect("tx");
                // Several historical capture rows for flow-a with zeroed HLCs,
                // exactly what survives the v54 ALTER.
                for name in ["Alpha v1", "Alpha v2", "Alpha v3"] {
                    let mut capture = complete_core_flow_capture("flow-a", name);
                    capture.hlc = HybridLogicalTimestamp {
                        wall_time_ms: 0,
                        logical: 0,
                        node_id: String::new(),
                    };
                    crate::sync::core_capture::record_core_write_capture(&tx, &capture)
                        .expect("persist stale capture");
                }
                tx.commit().expect("commit stale captures");
            }

            repository::set_setting(
                &node.runtime.db,
                crate::db::migrations::CORE_BASELINE_RESET_PENDING_KEY,
                "1",
            )
            .expect("arm marker");

            let reseeded = node
                .runtime
                .run_pending_baseline_cutover()
                .expect("cutover runs")
                .expect("cutover pending");
            // The snapshot reseeds every core table (org, roles, flows, ...); at
            // minimum the three live flows plus the default org/roles seeded by
            // the migrations.
            assert!(reseeded >= 3, "reseed must emit the current snapshot");

            let new_epoch = node.runtime.ledger.current_epoch().expect("epoch");
            assert_eq!(new_epoch.counter, 1);

            // Collect the reseeded FLOW operations' HLCs from the outbox.
            let push = node
                .runtime
                .build_push_payload_for_target("peer-authority", 256)
                .expect("push")
                .expect("payload");
            let mut hlcs: Vec<HybridLogicalTimestamp> = Vec::new();
            for wire in &push.operations {
                let op = node
                    .runtime
                    .ledger
                    .get_operation(operation_id_from_wire(&wire.op_id).expect("op id"))
                    .expect("op");
                assert_eq!(op.body.epoch, new_epoch, "reseed op carries new epoch");
                if op.body.resource_type == "core.flow" {
                    hlcs.push(op.body.hlc_timestamp.clone());
                }
            }
            assert_eq!(
                hlcs.len(),
                3,
                "exactly one reseed op per live flow — no history"
            );
            // No zeroed HLCs survived into the reseed.
            for hlc in &hlcs {
                assert!(
                    hlc.wall_time_ms > 0 || hlc.logical > 0,
                    "reseed must mint a real HLC, got {hlc:?}"
                );
            }
            // All distinct.
            let mut sorted = hlcs.clone();
            sorted.sort_by(|a, b| (a.wall_time_ms, a.logical).cmp(&(b.wall_time_ms, b.logical)));
            sorted.dedup_by(|a, b| a.wall_time_ms == b.wall_time_ms && a.logical == b.logical);
            assert_eq!(sorted.len(), 3, "reseed HLCs must be distinct");

            // A later update of flow-a, recorded post-cutover under the new
            // epoch, carries a strictly greater HLC than its reseed insert — LWW
            // will not drop it.
            let reseed_a_hlc = {
                // The reseed insert for flow-a is the one whose resource_id is
                // flow-a.
                let mut found = None;
                for wire in &push.operations {
                    let op = node
                        .runtime
                        .ledger
                        .get_operation(operation_id_from_wire(&wire.op_id).expect("op"))
                        .expect("op");
                    if op.body.resource_id == "flow-a" {
                        found = Some(op.body.hlc_timestamp.clone());
                    }
                }
                found.expect("flow-a reseed op")
            };
            // Mint the update HLC from the SAME clock the reseed used
            // (`core_hlc_now`) so it is guaranteed strictly later.
            let later_hlc = crate::sync::runtime::core_hlc_now();
            let mut update = core_flow_update_capture("flow-a", "Alpha repaired");
            update.hlc = later_hlc;
            update.epoch = new_epoch.clone();
            let update_op_id = node
                .runtime
                .record_core_capture(update)
                .expect("record update")
                .op_id;
            let update_op = node
                .runtime
                .ledger
                .get_operation(update_op_id)
                .expect("update op");
            let later = update_op.body.hlc_timestamp;
            assert!(
                (later.wall_time_ms, later.logical)
                    > (reseed_a_hlc.wall_time_ms, reseed_a_hlc.logical),
                "post-cutover update HLC {later:?} must exceed reseed HLC {reseed_a_hlc:?}"
            );
        });
    }

    /// Regression for the mesh Flow-sync gap: a core write goes through the
    /// PRODUCTION path, i.e. it lands ONLY in the SQLite capture journal
    /// (`__tentaflow_core_sync_captures`) inside the same transaction as the
    /// `flows` row. It must NOT reach the peer's outbox until the journal is
    /// drained. The bug was that the drain ran only at process startup, so a
    /// Flow saved while the process was running stayed invisible to peers until
    /// a restart. The other mesh-sync tests inject via
    /// `record_core_capture(...)`, the DIRECT path that builds op + outbox
    /// immediately and bypasses the journal — so they never exercised the drain
    /// and could not catch this.
    ///
    /// We drive the drain with `drain_pending_core_captures_with(&db, limit,
    /// |c| self.record_core_capture(c)...)`. That is exactly what the production
    /// `drain_pending_core_captures_online(limit)` wrapper resolves to at
    /// runtime: the wrapper looks up the global `SYNC_RUNTIME` (the live
    /// instance) and records through it. In a per-instance test the global is
    /// not set, so we record through this very runtime instance — the same
    /// load/mark journal machinery, just with the recorder bound explicitly.
    #[test]
    fn core_flow_journal_write_reaches_outbox_only_after_drain() {
        with_tmp_home(|| {
            let node = make_runtime(180);
            seed_core_authority_target(&node.runtime.db, "core.flow", "peer-authority");
            // The journal row's actor_user_id is FK-bound to user_accounts(id).
            seed_actor_user(&node.runtime.db, "test-actor");

            // PRODUCTION path: persist the capture into the journal only — same
            // transaction a real `repository::create_flow` would use when it
            // calls `record_core_capture_tx`. No op, no outbox yet.
            {
                let conn = node.runtime.db.lock().expect("db");
                let tx = conn.unchecked_transaction().expect("tx");
                crate::sync::core_capture::record_core_write_capture(
                    &tx,
                    &complete_core_flow_capture("flow-online", "Online Flow"),
                )
                .expect("persist capture to journal");
                tx.commit().expect("commit journal capture");
            }

            // Our flow's journal row exists and is still pending (undrained).
            // The migrations seed other default core captures (org, roles); we
            // assert only on flow-online, the resource this regression covers.
            let flow_status_before: String = {
                let conn = node.runtime.db.lock().expect("db");
                conn.query_row(
                    "SELECT status FROM __tentaflow_core_sync_captures \
                     WHERE resource_type = 'core.flow' AND resource_id = 'flow-online'",
                    [],
                    |row| row.get(0),
                )
                .expect("flow capture row must exist in the journal")
            };
            assert_eq!(
                flow_status_before, "pending",
                "production write must land in the journal as a pending capture"
            );

            // BEFORE drain: nothing in the peer's outbox — the bug's symptom.
            // Only core.flow has a seeded target here, so the default org/role
            // captures cannot queue to peer-authority and the push stays empty.
            assert!(
                node.runtime
                    .build_push_payload_for_target("peer-authority", 256)
                    .expect("build push before drain")
                    .is_none(),
                "undrained journal capture must NOT appear in the outbox"
            );

            // The drain converts pending journal rows into ledger ops + outbox
            // entries. It also drains the default org/role captures, so the
            // count is >= 1; the flow-specific assertions below are what matter.
            let drained = crate::sync::core_capture::drain_pending_core_captures_with(
                &node.runtime.db,
                1000,
                |capture| {
                    node.runtime
                        .record_core_capture(capture)
                        .map(|record| Some(record.op_id))
                },
            )
            .expect("drain pending captures");
            assert!(drained >= 1, "the drain must process the pending capture(s)");

            // AFTER drain: the core.flow op is now in the peer's outbox.
            let push = node
                .runtime
                .build_push_payload_for_target("peer-authority", 256)
                .expect("build push after drain")
                .expect("drained capture must produce an outbox payload");
            let flow_ops: Vec<_> = push
                .operations
                .iter()
                .filter_map(|wire| {
                    node.runtime
                        .ledger
                        .get_operation(operation_id_from_wire(&wire.op_id).expect("op id"))
                        .ok()
                })
                .filter(|op| {
                    op.body.resource_type == "core.flow" && op.body.resource_id == "flow-online"
                })
                .collect();
            assert_eq!(
                flow_ops.len(),
                1,
                "the drained Flow op must reach the peer outbox"
            );
            assert_eq!(
                flow_ops[0].body.changed_fields.get("name"),
                Some(&FieldValue::String("Online Flow".to_string()))
            );

            // The flow's journal row is no longer pending — it is now ledgered.
            let flow_status_after: String = {
                let conn = node.runtime.db.lock().expect("db");
                conn.query_row(
                    "SELECT status FROM __tentaflow_core_sync_captures \
                     WHERE resource_type = 'core.flow' AND resource_id = 'flow-online'",
                    [],
                    |row| row.get(0),
                )
                .expect("flow capture row must still exist after drain")
            };
            assert_eq!(
                flow_status_after, "ledgered",
                "the drain must mark the flow's journal row as ledgered"
            );
        });
    }

    /// A baseline cutover must re-emit stored shared secrets. The pre-cutover
    /// enqueue creates a `core.shared_setting_secret` capture, the cutover wipes
    /// the whole journal, and the reseed is the only path that restores it — so
    /// after cutover the outbox must hold a fresh secret op under the NEW epoch
    /// with a real (non-zero) HLC, or the secret would silently vanish.
    #[test]
    fn baseline_cutover_reseeds_stored_shared_secret() {
        with_tmp_home(|| {
            let node = make_runtime(170);
            seed_core_authority_target(
                &node.runtime.db,
                "core.shared_setting_secret",
                "peer-authority",
            );

            repository::set_shared_secret_setting_secure(
                &node.runtime.db,
                "hf_token",
                "hf_baseline_secret",
                &node.runtime.settings_cipher,
                None,
            )
            .expect("store shared secret");

            repository::set_setting(
                &node.runtime.db,
                crate::db::migrations::CORE_BASELINE_RESET_PENDING_KEY,
                "1",
            )
            .expect("arm marker");

            node.runtime
                .run_pending_baseline_cutover()
                .expect("cutover runs")
                .expect("cutover pending");

            let new_epoch = node.runtime.ledger.current_epoch().expect("epoch");
            let push = node
                .runtime
                .build_push_payload_for_target("peer-authority", 256)
                .expect("push")
                .expect("payload");

            let mut secret_hlcs: Vec<HybridLogicalTimestamp> = Vec::new();
            for wire in &push.operations {
                let op = node
                    .runtime
                    .ledger
                    .get_operation(operation_id_from_wire(&wire.op_id).expect("op id"))
                    .expect("op");
                if op.body.resource_type == "core.shared_setting_secret"
                    && op.body.resource_id == "hf_token"
                {
                    assert_eq!(op.body.epoch, new_epoch, "secret reseed carries new epoch");
                    secret_hlcs.push(op.body.hlc_timestamp.clone());
                }
            }
            assert_eq!(
                secret_hlcs.len(),
                1,
                "exactly one fresh shared-secret op after cutover"
            );
            let hlc = &secret_hlcs[0];
            assert!(
                hlc.wall_time_ms > 0 || hlc.logical > 0,
                "secret reseed must mint a real HLC, got {hlc:?}"
            );
        });
    }

    /// CRITICAL 2: a crash between `bump_epoch` and clearing the marker must not
    /// double-bump. Re-arming the marker with the counter the runtime persisted
    /// (the crash-resume state) and re-running leaves the epoch bumped exactly
    /// once and re-seeds into the wiped outbox.
    #[test]
    fn baseline_cutover_is_idempotent_across_crash_retry() {
        with_tmp_home(|| {
            let node = make_runtime(161);
            seed_core_authority_target(&node.runtime.db, "core.flow", "peer-authority");
            seed_flow_row(&node.runtime.db, "flow-x", "Xi");

            repository::set_setting(
                &node.runtime.db,
                crate::db::migrations::CORE_BASELINE_RESET_PENDING_KEY,
                "1",
            )
            .expect("arm marker");

            node.runtime
                .run_pending_baseline_cutover()
                .expect("first cutover")
                .expect("pending");
            let after_first = node.runtime.ledger.current_epoch().expect("epoch");
            assert_eq!(after_first.counter, 1, "first cutover bumps to 1");

            // Simulate a crash AFTER bump_epoch but BEFORE the marker clear: the
            // runtime had already rewritten the marker to record the pre-cutover
            // counter (0). Re-arm exactly that state and re-run.
            repository::set_setting(
                &node.runtime.db,
                crate::db::migrations::CORE_BASELINE_RESET_PENDING_KEY,
                "pre_cutover_epoch=0",
            )
            .expect("re-arm crash-resume marker");

            let reseeded = node
                .runtime
                .run_pending_baseline_cutover()
                .expect("retry cutover")
                .expect("still pending");
            assert!(reseeded >= 1, "retry re-seeds the snapshot");

            let after_retry = node.runtime.ledger.current_epoch().expect("epoch");
            assert_eq!(
                after_retry.counter, 1,
                "retry must NOT bump the epoch a second time"
            );
            assert_eq!(after_retry, after_first);

            // Marker cleared; a routine restart is a no-op.
            assert!(node
                .runtime
                .run_pending_baseline_cutover()
                .expect("third pass")
                .is_none());
            assert_eq!(
                node.runtime.ledger.current_epoch().expect("epoch"),
                after_first
            );
        });
    }

    // =========================================================================
    // E2E phase D: cross-node LWW convergence + baseline epoch fencing
    // =========================================================================

    /// HLC with a caller-chosen wall time and origin node — lets a test order two
    /// concurrent edits deterministically and exercise the node_id tie-break.
    fn hlc_at(wall_ms: i64, logical: u32, node_id: &str) -> HybridLogicalTimestamp {
        HybridLogicalTimestamp {
            wall_time_ms: wall_ms,
            logical,
            node_id: node_id.to_string(),
        }
    }

    /// Builds a complete `core.flow` INSERT capture carrying an explicit HLC, so
    /// the materializer's last-writer-wins comparison sees the order the test
    /// intends rather than wall-clock noise.
    fn flow_capture_with_hlc(
        resource_id: &str,
        name: &str,
        hlc: HybridLogicalTimestamp,
    ) -> crate::sync::core_capture::CoreWriteCapture {
        let mut fields = BTreeMap::new();
        fields.insert("name".to_string(), FieldValue::String(name.to_string()));
        fields.insert("is_default".to_string(), FieldValue::Bool(false));
        fields.insert(
            "flow_json".to_string(),
            FieldValue::String(r#"{"nodes":[]}"#.to_string()),
        );
        fields.insert(
            "status".to_string(),
            FieldValue::String("active".to_string()),
        );
        crate::sync::core_capture::CoreWriteCapture::new(
            crate::sync::core_registry::CoreSyncResourceKind::Flow,
            "org-default",
            resource_id,
            SqlWriteAction::Insert,
            fields,
            Some("test-actor".to_string()),
            hlc,
            test_epoch(),
        )
    }

    fn flow_name(db: &DbPool, id: &str) -> Option<String> {
        repository::get_flow(db, id).expect("get flow").map(|f| f.name)
    }

    /// Records a flow capture on `node` (signs + queues it) and returns the
    /// resulting signed `SyncOperation` ready to apply on a peer.
    fn signed_flow_op(
        node: &RuntimeHarness,
        capture: crate::sync::core_capture::CoreWriteCapture,
    ) -> SyncOperation {
        let recorded = node
            .runtime
            .record_core_capture(capture)
            .expect("record core capture");
        node.runtime
            .ledger
            .get_operation(recorded.op_id)
            .expect("operation")
    }

    /// Cross-node LWW: the SAME flow is edited concurrently on two nodes with
    /// different HLCs. Applying both operations in OPPOSITE orders on two separate
    /// databases must converge to the newer-HLC value — proving the winner is the
    /// higher HLC (not whichever arrived last) and that the two nodes do not
    /// diverge. The materializer is the LWW decision point reached by every
    /// inbound operation.
    #[test]
    fn e2e_cross_node_lww_converges_to_newer_hlc_regardless_of_apply_order() {
        with_tmp_home(|| {
            let node_a = make_runtime(91);
            let node_b = make_runtime(92);

            // Two concurrent INSERT edits of flow "1": A older, B newer. Distinct
            // origin node_ids so the comparison never depends on a tie.
            let older = flow_capture_with_hlc("1", "from-A", hlc_at(1_000, 0, "node-a"));
            let newer = flow_capture_with_hlc("1", "from-B", hlc_at(2_000, 0, "node-b"));
            let op_older = signed_flow_op(&node_a, older);
            let op_newer = signed_flow_op(&node_b, newer);

            // DB 1 applies older THEN newer; DB 2 applies newer THEN older.
            let db1 = make_db();
            let db2 = make_db();
            let cipher = make_settings_cipher(91);

            crate::sync::core_materializer::apply_core_operation(&db1, &cipher, &op_older)
                .expect("db1 older");
            crate::sync::core_materializer::apply_core_operation(&db1, &cipher, &op_newer)
                .expect("db1 newer");

            crate::sync::core_materializer::apply_core_operation(&db2, &cipher, &op_newer)
                .expect("db2 newer");
            // The older op arriving last must be DROPPED by the LWW gate (applies
            // 0 rows), not clobber the newer value.
            let stale_rows =
                crate::sync::core_materializer::apply_core_operation(&db2, &cipher, &op_older)
                    .expect("db2 older");
            assert_eq!(stale_rows, 0, "stale older op must be dropped by LWW");

            // Both databases converge to the newer-HLC value.
            assert_eq!(flow_name(&db1, "1").as_deref(), Some("from-B"));
            assert_eq!(flow_name(&db2, "1").as_deref(), Some("from-B"));
        });
    }

    /// LWW tie-break: equal wall time + logical, differing origin node_id. The
    /// total HLC order from phase A breaks the tie by node_id, and it must do so
    /// the SAME way on both databases regardless of apply order.
    #[test]
    fn e2e_cross_node_lww_node_id_tiebreak_is_deterministic() {
        with_tmp_home(|| {
            let node_a = make_runtime(93);
            let node_b = make_runtime(94);

            // Identical wall+logical; tie-break decides. "node-z" > "node-a", so
            // the "node-z" edit wins the total order deterministically.
            let lower = flow_capture_with_hlc("1", "from-a", hlc_at(5_000, 0, "node-a"));
            let higher = flow_capture_with_hlc("1", "from-z", hlc_at(5_000, 0, "node-z"));
            let op_lower = signed_flow_op(&node_a, lower);
            let op_higher = signed_flow_op(&node_b, higher);

            let db1 = make_db();
            let db2 = make_db();
            let cipher = make_settings_cipher(93);

            crate::sync::core_materializer::apply_core_operation(&db1, &cipher, &op_lower)
                .expect("db1 lower");
            crate::sync::core_materializer::apply_core_operation(&db1, &cipher, &op_higher)
                .expect("db1 higher");

            crate::sync::core_materializer::apply_core_operation(&db2, &cipher, &op_higher)
                .expect("db2 higher");
            let stale =
                crate::sync::core_materializer::apply_core_operation(&db2, &cipher, &op_lower)
                    .expect("db2 lower");
            assert_eq!(stale, 0, "lower node_id loses the tie-break and is dropped");

            assert_eq!(flow_name(&db1, "1").as_deref(), Some("from-z"));
            assert_eq!(flow_name(&db2, "1").as_deref(), Some("from-z"));
        });
    }

    /// Cross-node LWW through the FULL outbox -> push -> inbox -> materialize path
    /// (in-process, no iroh). The source records a flow op; the receiver, set up
    /// as a permission target for itself and the source, ingests the push and
    /// materializes the flow row — demonstrating the realistic transport reaches
    /// the same materializer that enforces LWW.
    #[test]
    fn e2e_cross_node_push_inbox_materializes_flow() {
        with_tmp_home(|| {
            let source = make_runtime(95);
            let receiver = make_runtime(96);

            // Authority-write policy on both sides with the receiver as the
            // authority target: the source queues to the receiver, and the
            // receiver accepts inbound ops for itself.
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

            source
                .runtime
                .record_core_capture(complete_core_flow_capture("1", "Pushed Flow"))
                .expect("record core flow");

            let push = source
                .runtime
                .build_push_payload_for_target(&receiver.runtime.local_node_id, 16)
                .expect("build push")
                .expect("pending push");
            let ack = receiver
                .runtime
                .handle_push_payload(&source.runtime.local_node_id, push)
                .expect("handle push");
            assert_eq!(ack.operation_ids.len(), 1, "one op accepted into inbox");

            // The inbound op materialized into the receiver's flows table.
            assert_eq!(flow_name(&receiver.runtime.db, "1").as_deref(), Some("Pushed Flow"));
        });
    }

    /// ACL gating inside the merged org: a `core.flow` resource assigned to one
    /// user replicates ONLY to that user's node. The other node, present in the
    /// same logical organization but lacking permission, is NOT a push target.
    /// This is the phase-A fix verified end-to-end: a denied node receives no push.
    #[test]
    fn e2e_acl_replicates_flow_only_to_permitted_node_in_merged_org() {
        with_tmp_home(|| {
            let source = make_runtime(99);
            let permitted = make_runtime(100);
            let denied = make_runtime(101);

            let permitted_user = repository::create_user_account(
                &source.runtime.db,
                "permitted-user",
                "hash",
                "Permitted User",
                "permitted@example.com",
            )
            .expect("permitted user");
            let denied_user = repository::create_user_account(
                &source.runtime.db,
                "denied-user",
                "hash",
                "Denied User",
                "denied@example.com",
            )
            .expect("denied user");

            for (node_id, user_id, display) in [
                (
                    permitted.runtime.local_node_id.as_str(),
                    permitted_user.as_str(),
                    "Permitted Node",
                ),
                (
                    denied.runtime.local_node_id.as_str(),
                    denied_user.as_str(),
                    "Denied Node",
                ),
            ] {
                repository::upsert_sync_node_identity(
                    &source.runtime.db,
                    node_id,
                    "pub",
                    "ed25519",
                    display,
                    "laptop",
                    "trusted",
                    Some(user_id),
                    "standard",
                )
                .expect("sync node");
                repository::assign_node_to_user(&source.runtime.db, node_id, user_id, "primary", None)
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
            // The flow is assigned to the permitted user only.
            repository::upsert_sync_resource_acl(
                &source.runtime.db,
                "org-default",
                crate::sync::core_registry::CORE_SYNC_ADDON_ID,
                "core.flow",
                "1",
                Some(permitted_user.as_str()),
                Some(permitted_user.as_str()),
                None,
                None,
                "assigned",
            )
            .expect("resource acl");

            let recorded = source
                .runtime
                .record_core_capture(complete_core_flow_capture("1", "ACL Gated Flow"))
                .expect("record core capture");

            let permitted_push = source
                .runtime
                .build_push_payload_for_target(&permitted.runtime.local_node_id, 16)
                .expect("permitted push");
            let denied_push = source
                .runtime
                .build_push_payload_for_target(&denied.runtime.local_node_id, 16)
                .expect("denied push");

            assert!(permitted_push.is_some(), "permitted node must receive the push");
            assert!(denied_push.is_none(), "denied node must NOT receive any push");
            assert_eq!(recorded.queued_targets, 1, "exactly one permitted target queued");
        });
    }

    /// Epoch fencing after baseline adopt: an operation minted under the OLD
    /// (pre-adopt) epoch is rejected by the receiver's inbox once the receiver has
    /// adopted a newer epoch. `put_verified_in_inbox` returns `EpochMismatch` and
    /// nothing materializes — stale-epoch writes from before the merge cannot leak
    /// into the merged organization.
    #[test]
    fn e2e_epoch_fencing_rejects_pre_adopt_operation() {
        with_tmp_home(|| {
            let source = make_runtime(97);
            let receiver = make_runtime(98);
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

            // Source mints an op under the genesis epoch (counter 0).
            let recorded = source
                .runtime
                .record_core_capture(complete_core_flow_capture("1", "Stale Epoch Flow"))
                .expect("record core flow");
            let stale_op = source
                .runtime
                .ledger
                .get_operation(recorded.op_id)
                .expect("operation");
            assert_eq!(stale_op.body.epoch.counter, 0, "op stamped under genesis epoch");

            // Receiver adopts a NEWER baseline epoch (as it would after pairing).
            let new_epoch = BaselineEpoch {
                counter: 7,
                origin_node: receiver.runtime.local_node_id.clone(),
            };
            receiver.runtime.ledger.set_epoch(new_epoch.clone()).expect("set epoch");

            // The push carries the stale-epoch op; the receiver's inbox fences it.
            let push = source
                .runtime
                .build_push_payload_for_target(&receiver.runtime.local_node_id, 16)
                .expect("build push")
                .expect("pending push");
            let err = receiver
                .runtime
                .handle_push_payload(&source.runtime.local_node_id, push)
                .expect_err("stale-epoch push must be fenced");
            match err {
                SyncLedgerError::EpochMismatch { expected, actual } => {
                    assert_eq!(expected, new_epoch, "fence expects the adopted epoch");
                    assert_eq!(actual.counter, 0, "rejected op carried the old epoch");
                }
                other => panic!("expected EpochMismatch, got: {other:?}"),
            }

            // Nothing materialized: the receiver has no such flow row.
            assert!(
                repository::get_flow(&receiver.runtime.db, "1")
                    .expect("get flow")
                    .is_none(),
                "stale-epoch op must not materialize after fencing"
            );
        });
    }
}

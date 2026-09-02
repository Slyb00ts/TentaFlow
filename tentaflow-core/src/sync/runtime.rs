// =============================================================================
// Plik: sync/runtime.rs
// Opis: Procesowy runtime Sync Ledger laczacy zapisy SQL addonow z Fjall i outbox.
// =============================================================================

use std::collections::{BTreeMap, HashMap, HashSet};
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
    partition_materialization_order, ActionType, BaselineEpoch, FieldValue, FjallSyncLedgerStore,
    HexNodeIdOperationVerifier, HybridLogicalTimestamp, InboxEntry, LedgerResult, NewSyncOperation,
    NodeChainEntry, NodeFrontierEntry, NodeLogQuery, OperationId, OperationQuery, PartitionId,
    PeerId, RedactedRecord, RepairQueueEntry, SnapshotId, SyncLedgerError, SyncLedgerStore,
    SyncOperation, SyncOperationSigner, SyncSnapshot, SyncTarget,
};
use crate::sync::snapshot::{
    verify_snapshot_signature, SnapshotBuildRequest, SnapshotManager, SnapshotPackageStore,
};
use tentaflow_protocol::mesh::{
    MeshSyncAckPayload, MeshSyncOperationWire, MeshSyncPullPayload, MeshSyncPullResponsePayload,
    MeshSyncPushPayload, MeshSyncSnapshotPullPayload, MeshSyncSnapshotResponsePayload,
    RedactedOperationWire,
};

static SYNC_RUNTIME: OnceLock<Arc<SyncRuntime>> = OnceLock::new();
const BLOB_SYNC_CHUNK_SIZE: usize = 1024 * 1024;
/// How many times an inbox entry may be deferred for ordering reasons (its
/// target row not yet created) before it is treated as a terminal conflict. High
/// enough that a slow repair pull always wins, low enough that a genuinely
/// orphaned UPDATE is eventually surfaced instead of being retried forever.
const MAX_INBOX_DEFER_ATTEMPTS: u32 = 64;

/// Post-apply hook: when a replicated addon-instance op lands (install / enable
/// toggle / uninstall), the sync runtime asks the addon manager to make the
/// local runtime match the new DB state (load/unload the wasm from the package
/// store). Kept as a trait so the sync layer stays decoupled from AddonManager
/// and so reconcile (wasmtime work) runs OUTSIDE the materializer transaction.
pub trait AddonSyncReconciler: Send + Sync {
    /// Idempotent: make the runtime match the DB row for `addon_id`.
    fn reconcile_addon(&self, addon_id: &str);
    /// Drop the PermissionChecker's cached matrix for `addon_id` after a
    /// replicated matrix row (grant / default / visibility) was materialized,
    /// so a synced admin edit takes effect here without a restart.
    fn refresh_addon_permissions(&self, addon_id: &str);
}

pub struct SyncRuntime {
    db: DbPool,
    ledger: Arc<FjallSyncLedgerStore>,
    signer: RuntimeSigner,
    local_node_id: String,
    settings_cipher: Arc<crate::crypto::SettingsCipher>,
    hlc: HlcClock,
    /// Set once at startup; runs after a synced addon-instance op commits.
    addon_reconciler: parking_lot::RwLock<Option<Arc<dyn AddonSyncReconciler>>>,
    /// Deferral budget before an ordering-gap inbox entry is escalated to a
    /// terminal conflict. Defaults to `MAX_INBOX_DEFER_ATTEMPTS`; only lowered in
    /// tests so escalation can be exercised without thousands of drains.
    max_inbox_defer_attempts: u32,
    /// Epoch-reconciliation debounce: `(donor_node_id, epoch_counter, origin)`
    /// keys for which a baseline adopt has already been REQUESTED and is assumed
    /// in flight. An EpochMismatch surfaces once per op, but the adopt is a single
    /// async handshake — without this guard a batch of N mismatched ops would
    /// request N adopts. The mesh layer clears a key (via `clear_adopt_inflight`)
    /// when its adopt attempt finishes or fails, so a later mismatch can retry.
    adopt_inflight: parking_lot::Mutex<HashSet<String>>,
    /// Epoch-reconcile requests produced while admitting incoming ops (a remote
    /// op carried the canonically-winning epoch, so the local node must adopt that
    /// peer's baseline). Drained by the push/pull-response handlers and handed to
    /// the mesh layer, which owns the transport needed to run the adopt. Kept off
    /// the hot admission path's return type so the many internal callers of
    /// `store_incoming_operations` (tests, snapshot tail) are unaffected.
    pending_epoch_reconcile: parking_lot::Mutex<Vec<EpochReconcileRequest>>,
    /// Target chains for which a below-floor pull with no covering snapshot was
    /// already logged. Serving such a pull is handled gracefully (floor-anchored
    /// slice), but the state still deserves ONE warn per target — repair pulls
    /// retry on a timer, so an unconditional warn would flood the log.
    floor_anchor_warned: parking_lot::Mutex<HashSet<String>>,
    /// In-memory-only scheduling state for the periodic push: per-target retry
    /// backoff and a short-lived sync-target cache. Never persisted — rebuilt on
    /// restart from the durable outbox/SQLite state, so it cannot corrupt the
    /// ledger. Decouples per-tick push cost from the ledger size: a target that
    /// is offline / not ACK-ing is retried on an exponential backoff instead of
    /// having its whole un-acked backlog re-scanned, re-encoded and re-shipped
    /// every 5 s tick.
    push_state: PushState,
}

/// Per-target push retry backoff plus a permission-epoch-scoped sync-target
/// cache. All state is ephemeral: losing it on restart only means the next tick
/// re-evaluates from the durable outbox, never a data loss.
#[derive(Default)]
struct PushState {
    /// `target_node_id -> earliest wall-clock ms at which the periodic push may
    /// re-ship that target's already-delivered-but-unacknowledged backlog`. A
    /// freshly queued op (`put_in_outbox`) clears the target's entry so new data
    /// ships on the very next tick; a push that left un-acked work behind sets an
    /// exponentially growing deadline so an offline / non-ACK peer is no longer
    /// re-processed every tick.
    retry_after_ms: parking_lot::Mutex<std::collections::HashMap<String, RetrySchedule>>,
    /// `(org, addon, resource_type, resource_id) -> resolved target node ids`,
    /// valid only while `epoch` matches the org's current `sync_permission_epoch`.
    /// Every grant/revoke bumps that epoch (`bump_sync_permission_epoch`), so a
    /// stale entry can never authorize a target that lost access. Collapses the
    /// per-op-per-tick `list_sync_targets_for_resource` SQLite fan-out (the
    /// `can_node_receive_sync_resource` / `load_sync_node_identity` perf hotspots)
    /// to one query per distinct resource per epoch.
    target_cache: parking_lot::Mutex<std::collections::HashMap<TargetCacheKey, TargetCacheEntry>>,
}

#[derive(Clone)]
struct RetrySchedule {
    next_attempt_ms: i64,
    retry_count: u32,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct TargetCacheKey {
    org_id: String,
    addon_id: String,
    resource_type: String,
    resource_id: String,
}

struct TargetCacheEntry {
    epoch: u64,
    target_node_ids: Vec<String>,
}

struct RuntimeSigner {
    node_id: String,
    security: Arc<MeshSecurity>,
}

/// What an inbox entry that was deferred for ordering reasons needs in order to
/// be escalated to a terminal conflict once its deferral budget is exhausted.
/// `capture` is `Some` only for the addon-SQL branch, where escalation must also
/// write the conflict to the addon's `__tentaflow_sync_conflicts` table.
struct DeferredInboxContext {
    message: String,
    capture: Option<SqlWriteCapture>,
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

/// A request to converge a diverged baseline epoch: the local node admitted an
/// operation stamped with `donor_epoch`, which is the canonical WINNER over the
/// local epoch (`BaselineEpoch::wins_over`), so the local node must adopt the
/// winning peer's baseline. The mesh layer turns this into a single async
/// `pull_baseline_from_donor(donor_node_id, donor_epoch_counter)` — the same
/// machinery an admin-triggered or pairing-triggered adopt uses. It is emitted at
/// most once per `(donor, epoch)` while an adopt is in flight (see the runtime's
/// adopt-debounce), so a flood of epoch-mismatched ops never starts N adopts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpochReconcileRequest {
    pub donor_node_id: String,
    pub donor_epoch_counter: u64,
}

pub fn init(
    db: DbPool,
    signer: Arc<MeshSecurity>,
    settings_cipher: Arc<crate::crypto::SettingsCipher>,
) -> LedgerResult<Arc<SyncRuntime>> {
    // The runtime is process-global: a second init in the same process (tests,
    // defensive restart paths) must not re-open the Fjall ledger, which is
    // exclusively locked by the first instance, so return the existing runtime.
    if let Some(existing) = SYNC_RUNTIME.get() {
        return Ok(existing.clone());
    }
    // `sync_dir()` respektuje `sync_dir` z storage-paths.conf (aplikowane przy
    // starcie — ledger trzyma otwarty keyspace przez caly czas zycia procesu).
    let ledger_path = paths::sync_dir().join("ledger");
    let ledger = Arc::new(FjallSyncLedgerStore::open(&ledger_path)?);
    let needs_baseline_reset = ledger.needs_baseline_reset();
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
        addon_reconciler: parking_lot::RwLock::new(None),
        max_inbox_defer_attempts: MAX_INBOX_DEFER_ATTEMPTS,
        adopt_inflight: parking_lot::Mutex::new(HashSet::new()),
        pending_epoch_reconcile: parking_lot::Mutex::new(Vec::new()),
        floor_anchor_warned: parking_lot::Mutex::new(HashSet::new()),
        push_state: PushState::default(),
    });
    let _ = SYNC_RUNTIME.set(runtime.clone());
    let runtime = SYNC_RUNTIME
        .get()
        .expect("sync runtime must be initialized")
        .clone();
    // A schema wipe threw away the local chain heads, so the local node must NOT
    // resume minting from a restarted node_seq against peers that still remember
    // the old chain. Bumping the epoch fences the pre-upgrade history, then the
    // reseed re-emits the current SQLite state under that fresh epoch — restarting
    // node_seq at 1 is safe because no live peer accepts ops under the old epoch.
    //
    // The "needs reset" signal is the DURABLE on-disk marker, not the in-memory
    // wipe flag: if a prior boot stamped schema v2 and crashed before this reseed
    // finished, the marker is still set and we repeat the (idempotent) reseed.
    // The marker is cleared ONLY after a successful bump+reseed, so any crash in
    // the window leaves it set for the next boot.
    if needs_baseline_reset {
        match runtime.perform_core_baseline_reset(None) {
            Ok(_) => {
                if let Err(e) = runtime.ledger.clear_baseline_reset_marker() {
                    warn!("sync runtime: clearing baseline reset marker failed: {e}");
                }
            }
            Err(e) => warn!("sync runtime: post-wipe baseline reseed failed: {e}"),
        }
    }
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

/// Wire the addon reconciler onto the global sync runtime (startup). No-op if
/// the runtime is not initialized (e.g. sync disabled) — replicated addon ops
/// simply won't trigger a runtime reconcile in that case.
pub fn set_global_addon_reconciler(reconciler: Arc<dyn AddonSyncReconciler>) {
    if let Some(runtime) = SYNC_RUNTIME.get() {
        runtime.set_addon_reconciler(reconciler);
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

/// Number of operations the local ledger currently holds. Used by the
/// auto-pairing baseline election to tell an EMPTY node (a freshly installed
/// rig, near-zero ops) apart from a DATA-HOLDER (an established node with many
/// content ops): the election must never let an empty node donate over a
/// non-empty peer (it would wipe the peer's content). Returns `0` before the
/// runtime exists, which correctly classifies a not-yet-synced node as empty.
pub fn local_op_count() -> usize {
    match SYNC_RUNTIME.get() {
        Some(runtime) => runtime
            .ledger
            .list_all_operations()
            .map(|ops| ops.len())
            .unwrap_or(0),
        None => 0,
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
/// Settings key holding the permission epoch the authority-side outbox backfill
/// last reconciled for `org_id`. The backfill runs only when the live epoch is
/// higher, so a tick with no permission change is a single marker read.
fn backfill_epoch_marker_key(org_id: &str) -> String {
    format!("sync.backfill_epoch:{org_id}")
}

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

/// Drains baseline-epoch reconcile requests accumulated while admitting incoming
/// operations. The mesh layer calls this right after a push / pull-response was
/// handled and, for each request, runs a single `pull_baseline_from_donor` to
/// adopt the canonically-winning peer's baseline. Empty when no epoch divergence
/// was observed (the common case).
pub fn take_pending_epoch_reconcile() -> Vec<EpochReconcileRequest> {
    match SYNC_RUNTIME.get() {
        Some(runtime) => runtime.take_pending_reconcile(),
        None => Vec::new(),
    }
}

/// Releases the adopt-debounce for a `(donor, epoch_counter)` whose adopt attempt
/// just finished (success or failure), so a later epoch mismatch toward the same
/// target can request a fresh adopt instead of being suppressed forever.
pub fn clear_epoch_adopt_inflight(donor_node_id: &str, epoch_counter: u64) {
    if let Some(runtime) = SYNC_RUNTIME.get() {
        runtime.clear_adopt_inflight(donor_node_id, epoch_counter);
    }
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

/// Local node id of the running sync runtime, if started. Used by callers that
/// must address the local node's own chain (e.g. building a pull for it).
pub fn local_node_id() -> Option<String> {
    SYNC_RUNTIME
        .get()
        .map(|runtime| runtime.local_node_id.clone())
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

/// Re-enqueues already-minted ops to receivers that gained access after the mint.
/// Driven by the sync repair scheduler tick. Returns the number of outbox entries
/// added (`Ok(None)` before the runtime is initialized). Cheap when no permission
/// epoch advanced.
pub fn backfill_outbox_for_permission_grants() -> LedgerResult<Option<usize>> {
    let Some(runtime) = SYNC_RUNTIME.get() else {
        return Ok(None);
    };
    runtime.backfill_outbox_for_permission_grants().map(Some)
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
    /// Wire the addon reconciler (called at startup once the AddonManager exists).
    pub fn set_addon_reconciler(&self, reconciler: Arc<dyn AddonSyncReconciler>) {
        *self.addon_reconciler.write() = Some(reconciler);
    }

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

    /// Re-stamps a capture HLC with the local node id while keeping its wall/
    /// logical instant. The capture instant came from this node's clock, so this
    /// is a no-op in production, but it guarantees the op's HLC origin equals its
    /// `actor_node_id` (the invariant `validate_integrity` enforces).
    fn local_hlc_from(&self, source: &HybridLogicalTimestamp) -> HybridLogicalTimestamp {
        HybridLogicalTimestamp {
            wall_time_ms: source.wall_time_ms,
            logical: source.logical,
            node_id: self.local_node_id.clone(),
        }
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
                target_node_id: request.target_node_id.clone(),
                from_node_seq: request.from_node_seq,
                limit: operation_limit,
            });
            let retry_count = request.retry_count.saturating_add(1);
            let next_attempt_ms = now.saturating_add(repair_backoff_ms(retry_count));
            self.ledger.mark_repair_attempted(
                peer.clone(),
                &request.target_node_id,
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
                    self.reset_push_backoff(sync_target.as_str());
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
                    self.reset_push_backoff(sync_target.as_str());
                    self.ledger.put_in_outbox(sync_target, op_id)?;
                    queued += 1;
                }
                Err(e) => warn!("sync runtime: pominieto niepoprawny target outbox: {}", e),
            }
        }
        // An addon instance is useless on a receiver without the package bytes it
        // names, so route the package blob to the same targets in the same step.
        if capture.resource_type == "core.addon_instance" {
            if let (Some(package_id), Some(version)) = (
                changed_field_string(&capture.changed_fields, "package_id"),
                changed_field_string(&capture.changed_fields, "package_version"),
            ) {
                queued += self.queue_addon_package_blob(&capture.org_id, &package_id, &version)?;
            }
        }
        Ok(queued)
    }

    /// Enqueues a single operation into the outbox of each given node, skipping the
    /// local node and any node that already holds it. Idempotent: re-running for the
    /// same (op, nodes) is a no-op, so the blob-coupling paths can overlap safely.
    fn queue_op_to_nodes(&self, op_id: OperationId, node_ids: &[String]) -> LedgerResult<usize> {
        if node_ids.is_empty() {
            return Ok(0);
        }
        let existing = self.ledger.list_outbox_for_operation(op_id)?;
        let mut queued = 0usize;
        for node_id in node_ids {
            if node_id == &self.local_node_id {
                continue;
            }
            if existing
                .iter()
                .any(|entry| entry.target.as_str() == node_id.as_str())
            {
                continue;
            }
            match SyncTarget::new(node_id.clone()) {
                Ok(sync_target) => {
                    self.reset_push_backoff(sync_target.as_str());
                    self.ledger.put_in_outbox(sync_target, op_id)?;
                    queued += 1;
                }
                Err(e) => warn!("sync runtime: pominieto niepoprawny target outbox: {}", e),
            }
        }
        Ok(queued)
    }

    /// Couples an uploaded addon package's already-ledgered blob (its chunk +
    /// manifest operations) to the nodes that should receive the package, i.e. the
    /// sync targets of the instances referencing it. Called whenever a
    /// `core.addon_instance` op is routed (mint-time fan-out and permission
    /// backfill) so the wasm bytes follow the instance row to every receiver. Bytes
    /// captured before any instance exists are picked up here once the instance is
    /// routed; bytes captured after the instance are routed by the blob-capture path
    /// — together they close both orderings. No-op for bundled packages (which have
    /// no blob) and while the blob is not yet in the ledger.
    fn queue_addon_package_blob(
        &self,
        org_id: &str,
        package_id: &str,
        version: &str,
    ) -> LedgerResult<usize> {
        let node_ids = repository::list_sync_target_nodes_for_addon_package(
            &self.db, org_id, package_id, version,
        )
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        if node_ids.is_empty() {
            return Ok(0);
        }
        let blob_id = format!("addon-package:{package_id}:{version}");
        let Some(sha) = repository::latest_blob_sha_for_blob_id(&self.db, &blob_id)
            .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?
        else {
            return Ok(0);
        };
        let mut queued = 0usize;
        for op in self.blob_operations_for_sha(&sha)? {
            queued += self.queue_op_to_nodes(op.op_id, &node_ids)?;
        }
        Ok(queued)
    }

    /// All `core.blob` chunk + manifest operations for a content hash. Blob ops for
    /// a given sha share the partition `core/blob/<sha[..2]>`, so this is a bounded
    /// prefix scan, not a full ledger walk.
    fn blob_operations_for_sha(&self, sha: &str) -> LedgerResult<Vec<SyncOperation>> {
        let partition = PartitionId::new(format!("core/blob/{}", &sha[..2.min(sha.len())]))?;
        let ops = self.ledger.get_operations(OperationQuery {
            partition_id: partition,
            limit: None,
        })?;
        Ok(ops
            .into_iter()
            .filter(|op| op.body.resource_type == "core.blob" && op.body.resource_id == sha)
            .collect())
    }

    fn build_push_payload_for_target(
        &self,
        target_node_id: &str,
        limit: usize,
    ) -> LedgerResult<Option<MeshSyncPushPayload>> {
        let target = SyncTarget::new(target_node_id.to_string())?;

        // Idle fast-path: a future retry deadline means this target's backlog was
        // already shipped and is only awaiting ACK. A newly queued op always clears
        // the deadline (`reset_push_backoff`), so a deadline still in the future
        // proves there is nothing new to send — skip the whole outbox scan, making
        // the per-tick cost a single in-memory lookup regardless of ledger size.
        // The scan/encode/ship below runs only when the deadline is absent (new
        // data queued) or has elapsed (time to retry a non-ACK peer).
        let now = now_ms();
        let prior_retry = {
            let schedule = self.push_state.retry_after_ms.lock();
            schedule.get(target.as_str()).cloned()
        };
        if let Some(prior) = &prior_retry {
            if prior.next_attempt_ms > now {
                return Ok(None);
            }
        }

        let entries = self.ledger.list_pending_outbox(target.clone(), limit)?;
        if entries.is_empty() {
            // Backlog fully acknowledged/drained: drop any stale retry deadline so
            // the next freshly queued op is not gated behind it.
            self.push_state
                .retry_after_ms
                .lock()
                .remove(target.as_str());
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
                .then_with(|| left.body.actor_node_id.cmp(&right.body.actor_node_id))
                .then_with(|| left.body.node_seq.cmp(&right.body.node_seq))
        });
        let mut operations = Vec::with_capacity(pending.len());
        for (op_id, operation) in pending {
            operations.push(operation_to_wire(&operation)?);
            self.ledger.mark_delivered(target.clone(), op_id)?;
        }
        if operations.is_empty() {
            // Everything in-scope was revoked (acked-as-skip) or reaped; no payload
            // and no obligation left, so clear the deadline.
            self.push_state
                .retry_after_ms
                .lock()
                .remove(target.as_str());
            return Ok(None);
        }

        // We just (re)shipped a batch that is still awaiting ACK. Arm an
        // exponential backoff so an offline / non-ACK peer is retried on a growing
        // interval instead of having its whole backlog re-shipped every tick. A
        // freshly queued op resets this (`reset_push_backoff`), so live data is not
        // delayed. A successful ACK drains the outbox, and the empty-outbox branch
        // above then clears the deadline.
        let retry_count = prior_retry.map_or(0, |prior| prior.retry_count.saturating_add(1));
        let next_attempt_ms = now.saturating_add(repair_backoff_ms(retry_count));
        self.push_state.retry_after_ms.lock().insert(
            target.as_str().to_string(),
            RetrySchedule {
                next_attempt_ms,
                retry_count,
            },
        );

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
        let targets = self.cached_sync_targets_for_resource(
            &operation.body.org_id,
            &operation.body.addon_id,
            &operation.body.resource_type,
            &operation.body.resource_id,
        )?;
        Ok(targets.iter().any(|node_id| node_id == target_node_id))
    }

    /// Permission-epoch-scoped wrapper over `list_sync_targets_for_resource`.
    /// Sync policy + node identity are stable between grant/revoke writes, and
    /// every such write bumps the org's `sync_permission_epoch`; an entry is only
    /// trusted while its recorded epoch matches the live one, so a revoked target
    /// can never be served from a stale entry. Without the cache the periodic push
    /// runs the full `can_node_receive_sync_resource` / `load_sync_node_identity`
    /// SQLite fan-out once PER outbox entry PER tick, the dominant idle cost.
    fn cached_sync_targets_for_resource(
        &self,
        org_id: &str,
        addon_id: &str,
        resource_type: &str,
        resource_id: &str,
    ) -> LedgerResult<Vec<String>> {
        let epoch = repository::get_sync_permission_epoch(&self.db, org_id)
            .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        let key = TargetCacheKey {
            org_id: org_id.to_string(),
            addon_id: addon_id.to_string(),
            resource_type: resource_type.to_string(),
            resource_id: resource_id.to_string(),
        };
        {
            let cache = self.push_state.target_cache.lock();
            if let Some(entry) = cache.get(&key) {
                if entry.epoch == epoch {
                    return Ok(entry.target_node_ids.clone());
                }
            }
        }
        let target_node_ids = repository::list_sync_targets_for_resource(
            &self.db,
            org_id,
            addon_id,
            resource_type,
            resource_id,
        )
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?
        .into_iter()
        .map(|target| target.node_id)
        .collect::<Vec<_>>();
        let mut cache = self.push_state.target_cache.lock();
        cache.insert(
            key,
            TargetCacheEntry {
                epoch,
                target_node_ids: target_node_ids.clone(),
            },
        );
        Ok(target_node_ids)
    }

    /// Clears a target's push backoff so a freshly queued op ships on the next
    /// tick instead of waiting out a previously-set retry deadline. Called from
    /// the outbox enqueue paths.
    fn reset_push_backoff(&self, target_node_id: &str) {
        self.push_state.retry_after_ms.lock().remove(target_node_id);
    }

    /// Re-enqueues already-minted operations to receivers that gained access AFTER
    /// the op was minted. The mint-time outbox fan-out (`queue_targets_for_resource`)
    /// only reaches the targets allowed at mint time; a later grant never revisits
    /// it. The push side has a revoke path (`outbox_target_still_allowed`) but no
    /// inverse, and a receiver that holds the position as a redacted placeholder
    /// cannot self-heal: a redacted op carries no partition, so a frontier pull
    /// never re-fetches it. The authority therefore reverses the permission gate
    /// here and re-enqueues. The full op then flows through `build_push_payload`,
    /// the receiver upgrades redacted→full and materializes.
    ///
    /// Triggered by the per-org permission epoch advancing past a persisted marker;
    /// every grant write bumps that epoch (`bump_sync_permission_epoch`). The pre-gate
    /// enumerates orgs from the cheap `organizations` table and compares each org's
    /// permission epoch against its backfill marker. When NO org advanced (the common
    /// case on every 5s tick) this returns early after `O(orgs)` marker reads, WITHOUT
    /// a full ledger scan or any CBOR decode. The ledger is only scanned after a real
    /// grant moved at least one org's epoch.
    /// Re-enqueue is idempotent: `put_in_outbox` is keyed by `(target, op_id)`, and
    /// we skip targets that already hold the op, so a redelivered grant is a no-op.
    fn backfill_outbox_for_permission_grants(&self) -> LedgerResult<usize> {
        // Pre-gate: enumerate orgs from the cheap table (not the ledger) and find
        // those whose permission epoch advanced past the persisted backfill marker.
        let org_ids = repository::list_all_org_ids(&self.db)
            .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;

        let mut advanced_orgs = std::collections::HashSet::new();
        let mut org_epochs = std::collections::HashMap::new();
        for org_id in &org_ids {
            let current = repository::get_sync_permission_epoch(&self.db, org_id)
                .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
            let marker_key = backfill_epoch_marker_key(org_id);
            let last = repository::get_setting(&self.db, &marker_key)
                .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            if current > last {
                advanced_orgs.insert(org_id.clone());
                org_epochs.insert(org_id.clone(), current);
            }
        }
        if advanced_orgs.is_empty() {
            return Ok(0);
        }

        // At least one org advanced: now the (rarely paid) ledger scan is justified.
        // The `operations` keyspace is keyed by op_id, not org, so per-org filtering
        // still requires decoding each op; we restrict work to advanced orgs below.
        let operations = self.ledger.list_all_operations()?;
        if operations.is_empty() {
            return Ok(0);
        }

        let mut enqueued = 0usize;
        for operation in &operations {
            if !advanced_orgs.contains(&operation.body.org_id) {
                continue;
            }
            // Reverse the SAME gate the mint-time fan-out uses, so a target is only
            // backfilled for ops it is NOW genuinely permitted to receive.
            let targets = repository::list_sync_targets_for_resource(
                &self.db,
                &operation.body.org_id,
                &operation.body.addon_id,
                &operation.body.resource_type,
                &operation.body.resource_id,
            )
            .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
            if targets.is_empty() {
                continue;
            }
            let existing = self.ledger.list_outbox_for_operation(operation.op_id)?;
            for target in targets {
                if target.node_id == self.local_node_id {
                    continue;
                }
                if existing
                    .iter()
                    .any(|entry| entry.target.as_str() == target.node_id)
                {
                    continue;
                }
                match SyncTarget::new(target.node_id) {
                    Ok(sync_target) => {
                        self.reset_push_backoff(sync_target.as_str());
                        self.ledger.put_in_outbox(sync_target, operation.op_id)?;
                        enqueued += 1;
                    }
                    Err(e) => warn!("sync runtime: backfill skipping invalid target: {}", e),
                }
            }
            // A newly-permitted node that now receives an addon instance must also
            // receive its package bytes; the bare blob ops carry no permission of
            // their own and are skipped by the resource gate above, so couple them
            // to the instance's freshly-granted targets here.
            if operation.body.resource_type == "core.addon_instance" {
                if let (Some(package_id), Some(version)) = (
                    changed_field_string(&operation.body.changed_fields, "package_id"),
                    changed_field_string(&operation.body.changed_fields, "package_version"),
                ) {
                    enqueued += self.queue_addon_package_blob(
                        &operation.body.org_id,
                        &package_id,
                        &version,
                    )?;
                }
            }
        }

        // Persist the per-org watermark only after the scan succeeds, so a failure
        // mid-scan re-runs the whole backfill (idempotent) instead of silently
        // skipping ops. Marking the epoch is the LAST step.
        for org_id in advanced_orgs {
            if let Some(epoch) = org_epochs.get(&org_id) {
                repository::set_setting(
                    &self.db,
                    &backfill_epoch_marker_key(&org_id),
                    &epoch.to_string(),
                )
                .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
            }
        }
        Ok(enqueued)
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
        let operation_ids =
            self.store_incoming_operations(source_node_id, payload.operations, None)?;
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
        // The target node's chain is dense and monotonic, so a gap is filled by a
        // contiguous slice from `from_node_seq`. `serving_floor_node_seq` is the
        // lowest seq we can still relay from the log; if the requester asked BELOW
        // it we compacted that prefix away and a log slice cannot anchor the
        // requester's chain. In that case we escalate the same pull to a SNAPSHOT
        // response (the prefix lives in the snapshot that compacted it), which the
        // requester applies and which advances its node-frontier legitimately.
        let serving_floor_node_seq = self
            .ledger
            .earliest_live_node_seq(&payload.target_node_id)?
            .unwrap_or(0);
        // The peer's tip of the target chain: its own head if it IS the target,
        // else what it has observed of that node's chain. Lets the requester know
        // when it has caught up to everything this peer can offer.
        let serving_tip_node_seq = if payload.target_node_id == self.local_node_id {
            self.ledger
                .get_node_head(&payload.target_node_id)?
                .map(|h| h.last_seq)
                .unwrap_or(0)
        } else {
            self.ledger
                .get_node_frontier(&payload.target_node_id)?
                .map(|f| f.last_seq)
                .unwrap_or(0)
        };
        let mut from_node_seq = payload.from_node_seq;
        if serving_floor_node_seq > payload.from_node_seq {
            match self.escalate_pull_to_snapshot(
                source_node_id,
                &payload,
                serving_floor_node_seq,
            )? {
                Some(result) => return Ok(result),
                // CR-W3: the requester sits below our serving floor but we hold no
                // snapshot covering the prefix. This is the normal state after a
                // baseline-epoch adopt: the reset deleted the core prefix (its ops
                // are fenced by every peer's epoch check, so they are unservable
                // anyway) without persisting a snapshot. Serve the chain FROM THE
                // FLOOR instead — the response advertises `serving_floor_node_seq`
                // and the requester anchors the author-signed entry at the floor
                // exactly like the first op of a snapshot tail anchors a writer it
                // has never seen. A hard error here (the old behaviour) deadlocked
                // every below-floor requester forever. Outside the post-adopt case
                // this still indicates a violated compaction invariant (compaction
                // never prunes without a covering snapshot), so warn once per
                // target chain.
                None => {
                    if self
                        .floor_anchor_warned
                        .lock()
                        .insert(payload.target_node_id.clone())
                    {
                        warn!(
                            "sync pull below serving floor {serving_floor_node_seq} for node {} \
                             with no covering snapshot (requested from {}); serving the chain \
                             from the floor so the requester can anchor there",
                            payload.target_node_id, payload.from_node_seq
                        );
                    }
                    from_node_seq = serving_floor_node_seq;
                }
            }
        }
        let mut entries = self.ledger.get_node_chain_entries(NodeLogQuery {
            node_id: payload.target_node_id.clone(),
            from_node_seq: Some(from_node_seq),
            to_node_seq: None,
            limit: Some(payload.limit as usize),
        })?;
        entries.sort_by_key(|entry| entry.node_seq());
        // Per-op permission gating: an op the requester is a sync target for is
        // served in full; one it is NOT is served as a signed redacted placeholder
        // (chain proof only, no body). This keeps the per-node chain DENSE for the
        // requester — it can verify continuity and advance its frontier — while
        // never leaking a resource the requester may not read. A position we
        // ourselves hold only redacted is relayed as redacted unchanged.
        let mut wire = Vec::with_capacity(entries.len());
        for entry in entries {
            wire.push(self.chain_entry_to_wire(entry, source_node_id)?);
        }
        Ok(MeshSyncPullResult::Operations(
            MeshSyncPullResponsePayload {
                from_node_id: self.local_node_id.clone(),
                target_node_id: payload.target_node_id,
                // The EFFECTIVE start of the served slice (the floor when the
                // request sat below it): the requester validates density against
                // this value, and a floor-anchored slice starts at the floor.
                from_node_seq,
                operations: wire,
                serving_floor_node_seq,
                serving_tip_node_seq,
            },
        ))
    }

    /// Serves a SNAPSHOT in answer to a node-log pull whose `from_node_seq` is
    /// below our compaction floor. The compacted prefix lives in the snapshot that
    /// reaped it: we locate the partition of our earliest live op for the target
    /// chain (`serving_floor`) and serve that partition's latest snapshot + tail.
    /// Applying it advances the requester's node-frontier for every node the
    /// snapshot covers; successive pulls then drive the remaining partitions until
    /// the frontier passes the floor, so the catch-up is bounded by partition
    /// count. `None` if we hold no snapshot to serve; the caller then answers with
    /// a floor-anchored log slice instead of failing the pull.
    fn escalate_pull_to_snapshot(
        &self,
        source_node_id: &str,
        payload: &MeshSyncPullPayload,
        serving_floor: u64,
    ) -> LedgerResult<Option<MeshSyncPullResult>> {
        let floor_op_id = match self
            .ledger
            .get_node_log_entry(&payload.target_node_id, serving_floor)?
        {
            Some(op_id) => op_id,
            None => return Ok(None),
        };
        let partition_id = self.ledger.get_operation(floor_op_id)?.body.partition_id;
        let Some(snapshot) = self.ledger.latest_snapshot(partition_id.clone(), None)? else {
            return Ok(None);
        };
        let response = self.build_snapshot_response_from_snapshot(
            source_node_id,
            partition_id.as_str().to_string(),
            snapshot,
            true,
            payload.limit,
        )?;
        Ok(Some(MeshSyncPullResult::Snapshot(response)))
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
        let target_node_id = payload.target_node_id.clone();
        let serving_tip = payload.serving_tip_node_seq;
        // The anchor must compare against the PRE-admission expectation: it only
        // engages when the serving peer's floor sits above the next seq we still
        // need, i.e. the peer declared it cannot relay the prefix and holds no
        // snapshot covering it (otherwise it would have answered with a snapshot).
        let (expected_before, _) = self.initial_node_frontier(&target_node_id)?;
        let floor_anchor = (payload.serving_floor_node_seq > expected_before)
            .then_some(payload.serving_floor_node_seq);
        let operation_ids =
            self.store_incoming_operations(source_node_id, payload.operations, floor_anchor)?;
        if let Err(e) = self.apply_unapplied_inbox(128) {
            warn!("sync runtime: apply pulled operations failed: {}", e);
        }
        // Decide the repair's fate ONLY after admission, from whether the gap is
        // actually closed — clearing it up-front (before admission) was the stall
        // bug: an empty/short response left the gap open with no pending repair, so
        // nothing ever retried.
        let (expected, _) = self.initial_node_frontier(&target_node_id)?;
        self.reconcile_repair_after_pull(source_node_id, &target_node_id, expected, serving_tip);
        Ok(MeshSyncAckPayload {
            from_node_id: self.local_node_id.clone(),
            operation_ids,
        })
    }

    /// Decides what to do with the catch-up repair after a pull RESPONSE (the
    /// node-log variant) landed. `expected` is the next `node_seq` we still need;
    /// `serving_tip` is the highest seq the peer holds for the target chain.
    ///
    /// The compacted-prefix case never reaches here: when the peer cannot serve
    /// our `from_node_seq` from its log it answers a pull with a SNAPSHOT instead
    /// (see `handle_pull_payload`), which `handle_snapshot_response_payload`
    /// applies. So this only handles the log-served path:
    ///  - our frontier reached/passed the peer's tip → there is nothing more this
    ///    peer can offer, the catch-up is satisfied; clear the repair (a later
    ///    push re-opens a gap if the peer advances, so we cannot strand);
    ///  - otherwise the gap is still open (a short/empty response that did NOT
    ///    reach the tip) → re-queue from the new `expected` so the next tick keeps
    ///    pulling. Never leave a still-open gap without a pending repair (the stall
    ///    bug: previously an empty response cleared the repair and froze forever).
    fn reconcile_repair_after_pull(
        &self,
        peer_id: &str,
        target_node_id: &str,
        expected: u64,
        serving_tip: u64,
    ) {
        if expected > serving_tip {
            self.clear_repair_request(peer_id, target_node_id);
            return;
        }
        self.queue_repair_request(peer_id, target_node_id, expected);
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
        // A snapshot blob is a SQL package, not a per-op stream: it cannot be
        // partially redacted, so we serve it ONLY when every op it covers is one
        // the requester may receive. A central-only client never asks for a
        // snapshot of a partition it cannot read (the chain pull serves it redacted
        // ops instead), so this gate stays a hard deny without stalling catch-up.
        for operation in &blob.operations {
            self.ensure_peer_target_allowed(operation, target_node_id)?;
        }
        // The tail is every partition operation not already captured by the
        // snapshot blob, ordered by HLC. The snapshot watermark is an op count,
        // not a partition sequence, so membership (op_id in blob) is the boundary.
        let operations_after_snapshot = if include_tail && tail_limit > 0 {
            let covered: std::collections::HashSet<OperationId> =
                blob.operations.iter().map(|op| op.op_id).collect();
            let mut operations: Vec<SyncOperation> = self
                .ledger
                .get_operations(OperationQuery {
                    partition_id: snapshot.partition_id.clone(),
                    limit: None,
                })?
                .into_iter()
                .filter(|op| !covered.contains(&op.op_id))
                .collect();
            operations.sort_by(partition_materialization_order);
            operations.truncate(tail_limit as usize);
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
        let blob = crate::sync::snapshot::decode_snapshot_sql_blob(&payload.blob_bytes)?;
        // The snapshot prefix + tail are applied straight to SQLite, bypassing the
        // inbox, so advance each authoring node's frontier to the highest node_seq
        // they cover. Without this the very next op of one of those nodes would be
        // seen as a gap (its predecessor lives in the just-applied prefix). The
        // snapshot carries an AUTHOR-ATTESTED `node_frontier` (bound into its
        // signature, already verified above) for the prefix; seed from it, then let
        // the tail (ops beyond the snapshot) extend each writer's frontier further.
        let mut node_tips: HashMap<String, (u64, [u8; 32])> = snapshot
            .node_frontier
            .iter()
            .map(|(node_id, (seq, hash))| (node_id.clone(), (*seq, *hash)))
            .collect();
        self.ledger.save_snapshot(snapshot)?;
        let mut operation_ids = Vec::with_capacity(blob.operations.len() + tail_operations.len());
        for operation in blob.operations.iter().chain(tail_operations.iter()) {
            operation_ids.push(operation.op_id.as_bytes().to_vec());
            let entry = node_tips
                .entry(operation.body.actor_node_id.clone())
                .or_insert((0, [0u8; 32]));
            if operation.body.node_seq >= entry.0 {
                *entry = (operation.body.node_seq, operation.operation_hash);
            }
        }
        for (node_id, (last_seq, last_hash)) in node_tips {
            let advance = self
                .ledger
                .get_node_frontier(&node_id)?
                .is_none_or(|frontier| frontier.last_seq < last_seq);
            if advance {
                self.ledger.save_node_frontier(NodeFrontierEntry {
                    node_id,
                    last_seq,
                    last_hash,
                })?;
            }
        }
        // The snapshot + its tail caught us up on the source node's chain, so the
        // catch-up repair we queued against it is satisfied.
        self.clear_repair_request(source_node_id, source_node_id);
        if let Err(e) = self.apply_unapplied_inbox(128) {
            warn!("sync runtime: apply snapshot tail operations failed: {}", e);
        }
        Ok(MeshSyncAckPayload {
            from_node_id: self.local_node_id.clone(),
            operation_ids,
        })
    }

    /// Stable debounce key for an in-flight adopt toward `(donor, epoch)`.
    fn adopt_inflight_key(donor_node_id: &str, epoch: &BaselineEpoch) -> String {
        format!("{donor_node_id}|{}|{}", epoch.counter, epoch.origin_node)
    }

    /// Decides how the local node reacts to an operation that arrived under a
    /// DIFFERENT baseline epoch than the locally-active one. This is the self-heal
    /// that replaces the old behaviour of rejecting the op forever (the warn-spin):
    ///
    ///  - `remote_epoch` is the canonical WINNER (`wins_over` the local epoch) →
    ///    the local node must adopt that peer's baseline. Returns a single
    ///    `EpochReconcileRequest` the FIRST time per `(donor, epoch)`; later
    ///    mismatches for the same target return `None` (debounced via
    ///    `adopt_inflight`) so a batch of mismatched ops starts exactly one adopt.
    ///  - the local epoch wins (or the epochs are equal) → `None`. We simply drop
    ///    the stale remote op; the peer will itself adopt once it observes one of
    ///    OUR ops under the winning epoch (symmetry), so no node spins and exactly
    ///    one epoch survives mesh-wide.
    fn note_epoch_mismatch(
        &self,
        source_node_id: &str,
        remote_epoch: &BaselineEpoch,
    ) -> LedgerResult<Option<EpochReconcileRequest>> {
        let local_epoch = self.ledger.current_epoch()?;
        if !remote_epoch.wins_over(&local_epoch) {
            return Ok(None);
        }
        let key = Self::adopt_inflight_key(source_node_id, remote_epoch);
        // Debounce: only the first mismatch toward this donor+epoch requests an
        // adopt; the async handshake is single-flight and clears the key when done.
        if !self.adopt_inflight.lock().insert(key) {
            return Ok(None);
        }
        warn!(
            donor = %source_node_id,
            "sync runtime: remote epoch counter={} origin={} wins over local counter={} origin={}; \
             requesting baseline adopt to converge",
            remote_epoch.counter, remote_epoch.origin_node, local_epoch.counter, local_epoch.origin_node
        );
        Ok(Some(EpochReconcileRequest {
            donor_node_id: source_node_id.to_string(),
            donor_epoch_counter: remote_epoch.counter,
        }))
    }

    /// Records a reconcile request for the push/pull handlers to drain and hand to
    /// the mesh layer. De-dupes within one drain window so one batch yields at most
    /// one request per donor.
    fn push_pending_reconcile(&self, request: EpochReconcileRequest) {
        let mut pending = self.pending_epoch_reconcile.lock();
        if !pending.contains(&request) {
            pending.push(request);
        }
    }

    /// Drains the reconcile requests accumulated since the last call.
    fn take_pending_reconcile(&self) -> Vec<EpochReconcileRequest> {
        std::mem::take(&mut *self.pending_epoch_reconcile.lock())
    }

    /// Releases the adopt-debounce for `(donor, epoch_counter)`. The mesh layer
    /// calls this when its adopt attempt finishes (success or failure) so a later
    /// epoch mismatch toward the same target can request a fresh adopt instead of
    /// being silently suppressed forever. We clear EVERY in-flight key whose donor
    /// + counter match (origin is not known at the call site), which is safe: at
    /// most one origin is ever in flight per donor+counter.
    fn clear_adopt_inflight(&self, donor_node_id: &str, epoch_counter: u64) {
        let prefix = format!("{donor_node_id}|{epoch_counter}|");
        self.adopt_inflight
            .lock()
            .retain(|key| !key.starts_with(&prefix));
    }

    /// `floor_anchor` carries a pull-serving peer's compaction/adopt floor when it
    /// sits above our next expected seq for the pulled chain: the prefix below it
    /// is unobtainable from that peer (compacted or abandoned by a baseline-epoch
    /// adopt, with no covering snapshot), so the entry AT the floor anchors the
    /// chain instead of being classified as a gap. Push deliveries pass `None`.
    fn store_incoming_operations(
        &self,
        source_node_id: &str,
        operations: Vec<MeshSyncOperationWire>,
        floor_anchor: Option<u64>,
    ) -> LedgerResult<Vec<Vec<u8>>> {
        let source = PeerId::new(source_node_id.to_string())?;
        // Admit each authoring node's chain in node_seq order. A batch may carry
        // several nodes' ops interleaved across partitions, so decode and sort by
        // (actor_node_id, node_seq) first; otherwise a higher-seq op processed
        // before its predecessor would gap and be dropped within the same batch.
        // A batch mixes full ops and redacted placeholders — both advance the same
        // per-node frontier, so they sort and admit on one axis.
        let mut decoded: Vec<NodeChainEntry> = Vec::with_capacity(operations.len());
        for wire in &operations {
            decoded.push(chain_entry_from_wire(wire)?);
        }
        decoded.sort_by(|a, b| {
            chain_entry_node_id(a)
                .cmp(chain_entry_node_id(b))
                .then_with(|| a.node_seq().cmp(&b.node_seq()))
        });
        let mut accepted = Vec::with_capacity(decoded.len());
        // Per-node admission state for this batch, keyed by authoring node:
        // the next expected `node_seq` and the hash of the last accepted op.
        let mut expected_seqs: HashMap<String, u64> = HashMap::new();
        let mut frontier_hashes: HashMap<String, Option<[u8; 32]>> = HashMap::new();
        for entry in decoded {
            // A FULL op the local node may not materialize is admitted as redacted:
            // we still advance the chain (so we don't loop on it) but never apply
            // its body. A node that hands us a full op we are not a target for is
            // not punished — we simply do not retain the body.
            let entry = match &entry {
                NodeChainEntry::Full(operation) if !self.local_target_allowed(operation)? => {
                    NodeChainEntry::Redacted(RedactedRecord {
                        op_id: operation.op_id,
                        operation_hash: operation.operation_hash,
                        actor_node_id: operation.body.actor_node_id.clone(),
                        node_seq: operation.body.node_seq,
                        prev_node_hash: operation.body.prev_node_hash,
                        signature: operation.signature.clone(),
                    })
                }
                _ => entry,
            };
            let node_id = chain_entry_node_id(&entry).to_string();
            let node_seq = entry.node_seq();
            let op_id = entry.op_id();
            if !expected_seqs.contains_key(&node_id) {
                let (mut seq, mut hash) = self.initial_node_frontier(&node_id)?;
                // Floor anchor: the serving peer cannot relay anything below
                // `floor` and no snapshot covers that prefix. The author-signed
                // entry AT the floor anchors the chain the same way the first op
                // of a snapshot tail anchors a writer the receiver has never seen:
                // its `prev_node_hash` claim seeds the frontier. Entries are
                // sorted, so this runs for the node's lowest-seq entry only, and
                // admission still verifies the signature (which covers node_seq
                // and prev_node_hash) before anything is persisted.
                if let Some(floor) = floor_anchor {
                    if seq < floor && node_seq == floor {
                        seq = floor;
                        hash = chain_entry_prev_hash(&entry);
                    }
                }
                expected_seqs.insert(node_id.clone(), seq);
                frontier_hashes.insert(node_id.clone(), hash);
            }
            let expected = expected_seqs[&node_id];
            let frontier_hash = frontier_hashes[&node_id];

            match self.admit_chain_entry(&source, &entry, expected, frontier_hash) {
                Ok(NodeAdmission::Accepted) => {
                    let advanced_hash = chain_entry_hash(&entry);
                    expected_seqs.insert(node_id.clone(), node_seq.saturating_add(1));
                    frontier_hashes.insert(node_id, Some(advanced_hash));
                    accepted.push(op_id.as_bytes().to_vec());
                }
                // Already known (node_seq below frontier): idempotent, ACK it.
                Ok(NodeAdmission::AlreadyKnown) => {
                    accepted.push(op_id.as_bytes().to_vec());
                }
                // Gap above the frontier: legal catch-up. Queue a per-node pull
                // and stop advancing this node's batch state — later ops of the
                // same node would also gap until the pull lands.
                Ok(NodeAdmission::Gap) => {}
                // The op carries a different baseline epoch. Do NOT reject forever
                // (that was the warn-spin with no convergence). Decide who adopts
                // whom from the canonical epoch order: if the remote epoch wins, we
                // request a baseline adopt from this peer (debounced, one per
                // donor+epoch); if ours wins we just drop the stale op — the peer
                // adopts when it sees our op under the winning epoch. Either way the
                // op is skipped here, never re-admitted under the wrong epoch.
                Err(SyncLedgerError::EpochMismatch { actual, .. }) => {
                    if let Some(request) = self.note_epoch_mismatch(source.as_str(), &actual)? {
                        self.push_pending_reconcile(request);
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(accepted)
    }

    /// Admits one chain entry (full or redacted) for `source`. A full op lands in
    /// the inbox + per-node axis and materializes; a redacted placeholder advances
    /// only the per-node axis (chain proof, equivocation guard, frontier) and never
    /// materializes. Both share the same equivocation/gap logic.
    fn admit_chain_entry(
        &self,
        source: &PeerId,
        entry: &NodeChainEntry,
        expected: u64,
        frontier_hash: Option<[u8; 32]>,
    ) -> LedgerResult<NodeAdmission> {
        let node_id = chain_entry_node_id(entry).to_string();
        let node_seq = entry.node_seq();
        let op_id = entry.op_id();

        // Redacted→full upgrade. A full op the local node is now permitted to
        // receive may arrive for a position we previously advanced past holding
        // ONLY a redacted placeholder (we gained access after the frontier moved
        // on). Materialize it without disturbing the frontier: same op_id, so it is
        // a completion of a known position, never a fork. `admit_verified_operation`
        // removes the redacted row and inserts the body into the inbox + view.
        if let NodeChainEntry::Full(operation) = entry {
            if node_seq < expected && self.ledger.get_redacted_record(op_id)?.is_some() {
                self.ledger.admit_verified_operation(
                    source.clone(),
                    operation.clone(),
                    NodeFrontierEntry {
                        node_id: node_id.clone(),
                        last_seq: node_seq,
                        last_hash: chain_entry_hash(entry),
                    },
                    &HexNodeIdOperationVerifier,
                )?;
                return Ok(NodeAdmission::Accepted);
            }
        }

        let admission = self.classify_chain_admission(
            source,
            &node_id,
            node_seq,
            op_id,
            chain_entry_prev_hash(entry),
            expected,
            frontier_hash,
        )?;
        if admission != NodeAdmission::Accepted {
            return Ok(admission);
        }
        let frontier = NodeFrontierEntry {
            node_id: node_id.clone(),
            last_seq: node_seq,
            last_hash: chain_entry_hash(entry),
        };
        match entry {
            NodeChainEntry::Full(operation) => {
                // Inbox insert + frontier advance must be one atomic batch: a crash
                // that advanced the frontier without the inbox entry would skip this
                // seq forever (never re-pulled, never applied).
                self.ledger.admit_verified_operation(
                    source.clone(),
                    operation.clone(),
                    frontier,
                    &HexNodeIdOperationVerifier,
                )?;
            }
            NodeChainEntry::Redacted(record) => {
                self.ledger.admit_redacted_operation(
                    record.clone(),
                    frontier,
                    &HexNodeIdOperationVerifier,
                )?;
            }
        }
        Ok(NodeAdmission::Accepted)
    }

    /// Decides whether a chain position can join the local view of its author's
    /// chain. A node is single-writer over its own chain, so equivocation — a
    /// different op at the same `(node, node_seq)` — is Byzantine and rejected.
    /// Works identically for full ops and redacted placeholders: both are keyed by
    /// op_id on `node_log`, so the equivocation comparison is the same.
    #[allow(clippy::too_many_arguments)]
    fn classify_chain_admission(
        &self,
        source: &PeerId,
        node_id: &str,
        node_seq: u64,
        op_id: OperationId,
        prev_node_hash: Option<[u8; 32]>,
        expected: u64,
        frontier_hash: Option<[u8; 32]>,
    ) -> LedgerResult<NodeAdmission> {
        if node_seq < expected {
            // We have already advanced past this seq. To catch equivocation we must
            // compare against what we ACTUALLY recorded at this chain position, not
            // look the incoming op up by its own op_id: ops are content-addressed,
            // so an equivocating op (same (node, seq), different body) has a
            // different op_id and a by-op_id lookup would silently miss the fork.
            // `admit_verified_operation`/`admit_redacted_operation` record foreign
            // ops on `node_log` too, so this lookup resolves for foreign authors.
            match self.ledger.get_node_log_entry(node_id, node_seq)? {
                Some(stored) if stored != op_id => {
                    return Err(SyncLedgerError::NodeEquivocation {
                        node: node_id.to_string(),
                        node_seq,
                        existing: stored,
                        incoming: op_id,
                    });
                }
                // stored == incoming: idempotent re-delivery of the op we hold.
                // None: compaction reaps the content row but KEEPS the node_log
                // entry, so a compacted seq still resolves here; None means we
                // genuinely never held this position (e.g. caught up via a snapshot
                // that skipped the prefix) and cannot prove either way — treat as
                // known (the chain head past it stands).
                _ => return Ok(NodeAdmission::AlreadyKnown),
            }
        }
        if node_seq > expected {
            self.queue_repair_request(source.as_str(), node_id, expected);
            return Ok(NodeAdmission::Gap);
        }
        // node_seq == expected: the prev_node_hash must chain onto our frontier.
        // A mismatch at the SAME seq means the author signed two histories.
        if prev_node_hash != frontier_hash {
            warn!(
                "sync runtime: node equivocation detected from {node_id} at node_seq={node_seq}: \
                 prev_node_hash does not chain onto known frontier"
            );
            return Err(SyncLedgerError::HashChainMismatch {
                node: node_id.to_string(),
                node_seq,
            });
        }
        Ok(NodeAdmission::Accepted)
    }

    /// Returns the next expected `node_seq` and the frontier hash for `node_id`,
    /// reading the persisted node-frontier. A node never seen before starts at
    /// seq 1 with no predecessor hash.
    fn initial_node_frontier(&self, node_id: &str) -> LedgerResult<(u64, Option<[u8; 32]>)> {
        match self.ledger.get_node_frontier(node_id)? {
            Some(frontier) => Ok((
                frontier.last_seq.saturating_add(1),
                Some(frontier.last_hash),
            )),
            None => Ok((1, None)),
        }
    }

    fn queue_repair_request(&self, peer_id: &str, target_node_id: &str, from_node_seq: u64) {
        let entry = match PeerId::new(peer_id.to_string()) {
            Ok(peer) => RepairQueueEntry {
                peer,
                target_node_id: target_node_id.to_string(),
                from_node_seq,
                next_attempt_ms: now_ms(),
                retry_count: 0,
            },
            Err(e) => {
                warn!("sync runtime: cannot queue repair request: {}", e);
                return;
            }
        };
        if let Err(e) = self.ledger.upsert_repair_request(entry) {
            warn!("sync runtime: repair request persist failed: {}", e);
        }
    }

    fn clear_repair_request(&self, peer_id: &str, target_node_id: &str) {
        let result = PeerId::new(peer_id.to_string())
            .and_then(|peer| self.ledger.remove_repair_request(peer, target_node_id));
        if let Err(e) = result {
            warn!("sync runtime: repair request clear failed: {}", e);
        }
    }

    fn apply_unapplied_inbox(&self, limit: usize) -> LedgerResult<usize> {
        let mut applied = 0usize;
        // Addon-instance reconciles are deferred until AFTER the drain loop (and
        // deduped) so heavy runtime work (wasmtime load, SQL migrations, flow
        // compile) never interleaves with — and stalls — applying other ops.
        let mut pending_addon_reconciles: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        // Addons whose synced config changed — drop their cached vector backend
        // so a replicated `__vector_config` takes effect without a restart.
        let mut pending_config_invalidations: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        // Addons whose replicated permission-matrix rows (grant / default /
        // visibility) were materialized — their PermissionChecker cache is
        // refreshed after the drain so the synced edit takes effect at once.
        let mut pending_permission_refreshes: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        // Synced addon-package blobs that finished reassembling — (blob_id, sha).
        // Extracted to the package store after the drain loop (untar is heavy).
        let mut pending_package_extractions: Vec<(String, String)> = Vec::new();
        // Entries deferred for ordering reasons during THIS drain, keyed by
        // (source, op_id). The deferral counter is bumped at most once per drain,
        // and only for entries that are still unapplied when the drain ends — an
        // entry whose prerequisite arrives in a later pass of the same drain
        // applies and is removed here, so it never accrues a spurious increment.
        // A long chain of dependent ops (longer than MAX_INBOX_DEFER_ATTEMPTS)
        // therefore converges in a single drain without any false escalation.
        let mut deferred_this_drain: std::collections::HashMap<
            (PeerId, OperationId),
            DeferredInboxContext,
        > = std::collections::HashMap::new();
        // Retry-until-no-progress: applying an INSERT may unblock an UPDATE that
        // was deferred earlier in this drain (or in a prior delivery) because its
        // target row did not exist yet. Each pass re-lists the inbox, so freshly
        // applied prerequisites become visible to the entries that depend on them.
        // The loop stops as soon as a pass applies nothing new, so there is no
        // spin once every applicable entry has either landed or been deferred.
        loop {
            let mut entries = self.ledger.list_unapplied_inbox(limit)?;
            entries.sort_by(|left, right| inbox_apply_order(left).cmp(&inbox_apply_order(right)));
            let mut progressed = false;
            for entry in entries {
                if entry.operation.body.resource_type == "core.blob" {
                    match apply_blob_operation(&entry.operation) {
                        Ok(BlobApplyOutcome::Applied) => {
                            self.ledger
                                .mark_inbox_applied(entry.source.clone(), entry.operation.op_id)?;
                            applied += 1;
                            progressed = true;
                            deferred_this_drain
                                .remove(&(entry.source.clone(), entry.operation.op_id));
                            // A blob MANIFEST (table blob_store) applied → the blob
                            // is fully reassembled. If it's a synced addon package,
                            // defer extraction (untar + catalog) to after the drain.
                            if entry.operation.body.table_name == "blob_store" {
                                if let Ok(blob_id) = field_string(&entry.operation, "blob_id") {
                                    if blob_id.starts_with("addon-package:") {
                                        if let Ok(sha) = field_string(&entry.operation, "sha256") {
                                            pending_package_extractions.push((blob_id, sha));
                                        }
                                    }
                                }
                            }
                        }
                        Ok(BlobApplyOutcome::Pending) => {}
                        Err(e) => {
                            self.classify_inbox_failure(&entry, e, None, &mut deferred_this_drain)?;
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
                            progressed = true;
                            deferred_this_drain
                                .remove(&(entry.source.clone(), entry.operation.op_id));
                            // A replicated addon instance landed — defer the runtime
                            // reconcile to after the drain loop (deduped).
                            if entry.operation.body.resource_type == "core.addon_instance" {
                                pending_addon_reconciles
                                    .insert(entry.operation.body.resource_id.clone());
                            } else if entry.operation.body.resource_type == "core.addon_config" {
                                // resource_id == "addon_id:key" — take the addon_id.
                                if let Some((addon_id, _)) =
                                    entry.operation.body.resource_id.split_once(':')
                                {
                                    pending_config_invalidations.insert(addon_id.to_string());
                                }
                            } else if matches!(
                                entry.operation.body.resource_type.as_str(),
                                "core.addon_permission"
                                    | "core.addon_permission_default"
                                    | "core.addon_visibility"
                            ) {
                                // Every matrix op carries addon_id in its fields
                                // (the composite resource_id is opaque here).
                                if let Ok(addon_id) = field_string(&entry.operation, "addon_id") {
                                    pending_permission_refreshes.insert(addon_id);
                                }
                            }
                        }
                        Err(e) => {
                            self.classify_inbox_failure(&entry, e, None, &mut deferred_this_drain)?;
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
                            progressed = true;
                            deferred_this_drain
                                .remove(&(entry.source.clone(), entry.operation.op_id));
                        }
                        Err(e) => {
                            self.classify_inbox_failure(&entry, e, None, &mut deferred_this_drain)?;
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
                        progressed = true;
                        deferred_this_drain.remove(&(entry.source.clone(), entry.operation.op_id));
                    }
                    Err(e) => {
                        // An ordering gap (missing FK parent, or UPDATE matching no
                        // target row) is deferred and retried; only a genuine data
                        // error (UNIQUE / CHECK / NOTNULL / type / syntax) becomes a
                        // terminal conflict — and only then is it written to the
                        // addon conflict table, so deferrals never pollute it.
                        let ledger_error = match &e {
                            crate::addon::storage_sql_exec::StorageSqlError::OrderingGap(msg) => {
                                SyncLedgerError::DeferredOrdering(msg.clone())
                            }
                            other => SyncLedgerError::Runtime(other.to_string()),
                        };
                        self.classify_inbox_failure(
                            &entry,
                            ledger_error,
                            Some(capture),
                            &mut deferred_this_drain,
                        )?;
                    }
                }
            }
            if !progressed {
                break;
            }
        }
        // After the drain has reached a fixpoint, every entry still left in
        // `deferred_this_drain` truly failed to apply for the whole drain (any
        // entry that applied in a later pass was removed above). Bump its deferral
        // counter exactly once and, only if it has now exhausted its budget,
        // escalate it to a terminal conflict so it stops blocking future drains.
        for ((source, op_id), context) in deferred_this_drain {
            self.escalate_or_defer_inbox_entry(source, op_id, context)?;
        }
        // Run deferred addon reconciles once, after all ops in this batch are
        // applied. Idempotent ("make runtime match DB"); a later op for the same
        // addon will reconcile again on the next drain.
        if !pending_addon_reconciles.is_empty() {
            if let Some(reconciler) = self.addon_reconciler.read().clone() {
                for addon_id in &pending_addon_reconciles {
                    reconciler.reconcile_addon(addon_id);
                }
            }
        }
        // Drop cached vector backends for addons whose config was replicated, so
        // the next access rebuilds against the new `__vector_config`.
        for addon_id in &pending_config_invalidations {
            crate::services::vector_namespace_manager(&self.db).invalidate_addon(addon_id);
        }
        // Replicated matrix edits take effect on the next permission check.
        if !pending_permission_refreshes.is_empty() {
            if let Some(reconciler) = self.addon_reconciler.read().clone() {
                for addon_id in &pending_permission_refreshes {
                    reconciler.refresh_addon_permissions(addon_id);
                }
            }
        }
        // Extract synced addon-package blobs into the local package store +
        // catalog, so the uploaded addon becomes installable on this node.
        for (blob_id, sha) in &pending_package_extractions {
            let blob_path = match blob_path_for_sha(sha) {
                Ok(p) => p,
                Err(e) => {
                    warn!("sync runtime: addon package blob path '{sha}': {e}");
                    continue;
                }
            };
            match crate::addon::lifecycle::materialize_synced_addon_package(
                &self.db, blob_id, &blob_path,
            ) {
                Ok((package_id, version)) => {
                    tracing::info!(
                        "sync runtime: addon package '{package_id}' v{version} zsynchronizowany do katalogu"
                    );
                    // The package just became loadable — (re)reconcile any of its
                    // installed instances whose op arrived before the bytes.
                    if let Some(reconciler) = self.addon_reconciler.read().clone() {
                        match crate::db::repository::installed_addon_ids_for_package(
                            &self.db,
                            &package_id,
                            &version,
                        ) {
                            Ok(ids) => {
                                for addon_id in ids {
                                    reconciler.reconcile_addon(&addon_id);
                                }
                            }
                            Err(e) => warn!(
                                "sync runtime: instancje pakietu '{package_id}' v{version}: {e}"
                            ),
                        }
                    }
                }
                Err(e) => warn!("sync runtime: rozpakowanie pakietu '{blob_id}': {e}"),
            }
        }
        Ok(applied)
    }

    /// Classifies a failed inbox apply within a single drain pass, shared by every
    /// branch (blob / core / kv / addon-SQL). A purely ordering-related failure
    /// (`DeferredOrdering` — the prerequisite INSERT has not landed yet) is NOT
    /// persisted here: the entry stays unapplied (so later passes retry it) and is
    /// merely recorded in `deferred_this_drain`, to be counted once when the drain
    /// settles (see `escalate_or_defer_inbox_entry`). Any other (data) error
    /// — UNIQUE / CHECK / NOTNULL / type / syntax — is a genuine, non-orderable
    /// conflict and is made terminal immediately.
    ///
    /// `capture` is `Some` only for the addon-SQL branch, where a terminal conflict
    /// must also be written to the addon's `__tentaflow_sync_conflicts` table for
    /// the operator. Ordering deferrals never write there, so the conflict table is
    /// not polluted with entries that are merely waiting for their prerequisite.
    fn classify_inbox_failure(
        &self,
        entry: &InboxEntry,
        error: SyncLedgerError,
        capture: Option<SqlWriteCapture>,
        deferred_this_drain: &mut std::collections::HashMap<
            (PeerId, OperationId),
            DeferredInboxContext,
        >,
    ) -> LedgerResult<()> {
        if let SyncLedgerError::DeferredOrdering(message) = error {
            // Defer in memory only; the entry remains unapplied and retryable. The
            // counter is bumped at most once per drain, after the loop settles.
            deferred_this_drain.insert(
                (entry.source.clone(), entry.operation.op_id),
                DeferredInboxContext { message, capture },
            );
            return Ok(());
        }
        if let Some(capture) = &capture {
            let storage_error =
                crate::addon::storage_sql_exec::StorageSqlError::Internal(error.to_string());
            crate::addon::storage_sql_exec::record_sync_conflict(
                capture,
                entry.operation.op_id,
                entry.source.as_str(),
                &storage_error,
            )
            .map_err(|record_error| SyncLedgerError::Runtime(record_error.to_string()))?;
        }
        self.ledger.mark_inbox_conflicted(
            entry.source.clone(),
            entry.operation.op_id,
            error.to_string(),
        )?;
        // For addon-SQL conflicts the error may carry raw rusqlite text
        // (StorageSqlError::Internal); keep it out of the tracing log to avoid
        // leaking schema detail. Full detail still lands in the addon conflict
        // table and the ledger conflict_message above. Core/kv/blob errors are
        // our own SyncLedgerError with safe resource-id messages, so log those.
        if capture.is_some() {
            warn!(
                op_id = %entry.operation.op_id.to_hex(),
                resource_type = %entry.operation.body.resource_type,
                resource_id = %entry.operation.body.resource_id,
                "sync runtime: incoming operation recorded as conflict: addon sql data conflict"
            );
        } else {
            warn!(
                "sync runtime: incoming operation {} recorded as conflict: {}",
                entry.operation.op_id.to_hex(),
                error
            );
        }
        Ok(())
    }

    /// Counts a single drain-long deferral for an entry that stayed unapplied for
    /// the whole drain, and escalates it to a terminal conflict once it has been
    /// deferred more than `MAX_INBOX_DEFER_ATTEMPTS` drains in a row (a genuinely
    /// orphaned op whose prerequisite never arrives). Until then the entry remains
    /// retryable. Only at escalation — and only for the addon-SQL branch — is the
    /// conflict written to the addon conflict table.
    fn escalate_or_defer_inbox_entry(
        &self,
        source: PeerId,
        op_id: OperationId,
        context: DeferredInboxContext,
    ) -> LedgerResult<()> {
        let deferred_count =
            self.ledger
                .mark_inbox_deferred(source.clone(), op_id, context.message.clone())?;
        if deferred_count <= self.max_inbox_defer_attempts {
            return Ok(());
        }
        // Prerequisite never arrived after the bounded number of drains: surface it
        // as a terminal conflict so it stops being retried forever.
        if let Some(capture) = &context.capture {
            let storage_error = crate::addon::storage_sql_exec::StorageSqlError::OrderingGap(
                context.message.clone(),
            );
            crate::addon::storage_sql_exec::record_sync_conflict(
                capture,
                op_id,
                source.as_str(),
                &storage_error,
            )
            .map_err(|record_error| SyncLedgerError::Runtime(record_error.to_string()))?;
        }
        self.ledger
            .mark_inbox_conflicted(source.clone(), op_id, context.message.clone())?;
        // context.message can embed raw rusqlite text (e.g. OrderingGap FK detail);
        // keep it out of tracing. Full detail still reaches the addon conflict
        // table and the ledger conflict_message above.
        warn!(
            op_id = %op_id.to_hex(),
            deferrals = deferred_count,
            "sync runtime: incoming operation recorded as conflict: ordering budget exhausted"
        );
        Ok(())
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

    /// Whether the LOCAL node is a sync target for the op's resource, i.e. whether
    /// a received full op may be materialized here. A full op the local node is not
    /// a target for is retained only as a redacted chain placeholder.
    fn local_target_allowed(&self, operation: &SyncOperation) -> LedgerResult<bool> {
        self.peer_target_allowed(operation, &self.local_node_id)
    }

    /// Encodes one served chain position for `target_node_id`. A FULL op the
    /// requester is permitted to receive becomes a full wire op; one it may not
    /// (or that we ourselves hold only redacted) becomes a signed redacted
    /// placeholder. The placeholder withholds the body AND the partition id, so a
    /// requester learns only that an op exists at this `(node, seq)` — never which
    /// resource it touched (op_id is a one-way hash of the body).
    fn chain_entry_to_wire(
        &self,
        entry: NodeChainEntry,
        target_node_id: &str,
    ) -> LedgerResult<MeshSyncOperationWire> {
        match entry {
            NodeChainEntry::Full(operation) => {
                if self.peer_target_allowed(&operation, target_node_id)? {
                    operation_to_wire(&operation)
                } else {
                    Ok(operation_to_redacted_wire(&RedactedRecord {
                        op_id: operation.op_id,
                        operation_hash: operation.operation_hash,
                        actor_node_id: operation.body.actor_node_id.clone(),
                        node_seq: operation.body.node_seq,
                        prev_node_hash: operation.body.prev_node_hash,
                        signature: operation.signature.clone(),
                    }))
                }
            }
            NodeChainEntry::Redacted(record) => Ok(operation_to_redacted_wire(&record)),
        }
    }

    /// Whether `target_node_id` is a sync target for the op's resource. Same query
    /// as `ensure_peer_target_allowed` but returns a bool instead of erroring, so
    /// the serving path can redact a denied op rather than abort the whole pull.
    fn peer_target_allowed(
        &self,
        operation: &SyncOperation,
        target_node_id: &str,
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

    fn ensure_peer_target_allowed(
        &self,
        operation: &SyncOperation,
        target_node_id: &str,
    ) -> LedgerResult<()> {
        if self.peer_target_allowed(operation, target_node_id)? {
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
            // Reuse the HLC instant minted inside the write transaction (stored on
            // the capture row) but stamp it with the authoring node's id: the op's
            // HLC origin MUST be the actor (enforced by validate_integrity), and
            // the local node is the single writer of this chain.
            hlc_timestamp: self.local_hlc_from(&capture.hlc),
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
        // Resolve the blob's targets once (they are identical for every chunk and
        // the manifest). An addon-package blob has no policy/ACL of its own — its
        // resource id is a content hash — so it routes to the targets of the
        // instances that reference it. Any other blob falls back to its own
        // `core.blob` sync policy.
        let blob_target_nodes = match parse_addon_package_blob_id(&capture.blob_id) {
            Some((package_id, version)) => repository::list_sync_target_nodes_for_addon_package(
                &self.db,
                &capture.org_id,
                &package_id,
                &version,
            )
            .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?,
            None => repository::list_sync_targets_for_resource(
                &self.db,
                &capture.org_id,
                "core",
                "core.blob",
                &capture.sha256,
            )
            .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?
            .into_iter()
            .map(|target| target.node_id)
            .collect(),
        };
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
            queued_targets += self.queue_op_to_nodes(append.op_id, &blob_target_nodes)?;
            chunk_index = chunk_index.saturating_add(1);
        }
        if capture.size_bytes == 0 {
            let op = self.build_blob_chunk_operation(capture, policy_epoch, 0, chunk_count, &[])?;
            let append = self.ledger.append_operation(op, &self.signer)?;
            queued_targets += self.queue_op_to_nodes(append.op_id, &blob_target_nodes)?;
        }
        let manifest = self.build_blob_manifest_operation(capture, policy_epoch, chunk_count)?;
        let append = self.ledger.append_operation(manifest, &self.signer)?;
        queued_targets += self.queue_op_to_nodes(append.op_id, &blob_target_nodes)?;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeAdmission {
    Accepted,
    AlreadyKnown,
    Gap,
}

fn chain_entry_node_id(entry: &NodeChainEntry) -> &str {
    match entry {
        NodeChainEntry::Full(operation) => operation.body.actor_node_id.as_str(),
        NodeChainEntry::Redacted(record) => record.actor_node_id.as_str(),
    }
}

fn chain_entry_hash(entry: &NodeChainEntry) -> [u8; 32] {
    match entry {
        NodeChainEntry::Full(operation) => operation.operation_hash,
        NodeChainEntry::Redacted(record) => record.operation_hash,
    }
}

fn chain_entry_prev_hash(entry: &NodeChainEntry) -> Option<[u8; 32]> {
    match entry {
        NodeChainEntry::Full(operation) => operation.body.prev_node_hash,
        NodeChainEntry::Redacted(record) => record.prev_node_hash,
    }
}

fn operation_to_wire(operation: &SyncOperation) -> LedgerResult<MeshSyncOperationWire> {
    Ok(MeshSyncOperationWire {
        op_id: operation.op_id.as_bytes().to_vec(),
        partition_id: operation.body.partition_id.as_str().to_string(),
        node_seq: operation.body.node_seq,
        operation: crate::sync::ledger::encode(operation)?,
        redacted: None,
    })
}

fn operation_to_redacted_wire(record: &RedactedRecord) -> MeshSyncOperationWire {
    MeshSyncOperationWire {
        op_id: record.op_id.as_bytes().to_vec(),
        // Withhold the partition: the requester is not allowed to learn which
        // resource the op touches, and the chain proof does not need it.
        partition_id: String::new(),
        node_seq: record.node_seq,
        operation: Vec::new(),
        redacted: Some(RedactedOperationWire {
            actor_node_id: record.actor_node_id.clone(),
            prev_node_hash: record.prev_node_hash,
            signature: record.signature.clone(),
        }),
    }
}

fn operation_from_wire(wire: &MeshSyncOperationWire) -> LedgerResult<SyncOperation> {
    let operation: SyncOperation = crate::sync::ledger::decode(&wire.operation)?;
    let op_id = operation_id_from_wire(&wire.op_id)?;
    if operation.op_id != op_id
        || operation.body.partition_id.as_str() != wire.partition_id
        || operation.body.node_seq != wire.node_seq
    {
        return Err(SyncLedgerError::Runtime(
            "sync operation wire metadata mismatch".to_string(),
        ));
    }
    // Reject node_seq==0 centrally for every wire ingress (push / pull-response /
    // snapshot tail): node_seq is 1-based and dense, so 0 is never a valid chain
    // position and must never reach admission as a phantom predecessor.
    if operation.body.node_seq == 0 {
        return Err(SyncLedgerError::InvalidNodeSeq {
            node: operation.body.actor_node_id.clone(),
        });
    }
    Ok(operation)
}

/// Decodes a wire op into the chain form it represents: a redacted placeholder
/// (when `wire.redacted` is set) or a full operation. The redacted variant
/// reconstructs `RedactedRecord` from the wire metadata + carried `op_id`; the
/// signature is verified later at admission, so this only shapes the bytes.
fn chain_entry_from_wire(wire: &MeshSyncOperationWire) -> LedgerResult<NodeChainEntry> {
    match &wire.redacted {
        Some(redacted) => {
            if !wire.operation.is_empty() {
                return Err(SyncLedgerError::Runtime(
                    "redacted sync op carries a body".to_string(),
                ));
            }
            if wire.node_seq == 0 {
                return Err(SyncLedgerError::InvalidNodeSeq {
                    node: redacted.actor_node_id.clone(),
                });
            }
            let op_id = operation_id_from_wire(&wire.op_id)?;
            Ok(NodeChainEntry::Redacted(RedactedRecord {
                op_id,
                operation_hash: *op_id.as_bytes(),
                actor_node_id: redacted.actor_node_id.clone(),
                node_seq: wire.node_seq,
                prev_node_hash: redacted.prev_node_hash,
                signature: redacted.signature.clone(),
            }))
        }
        None => Ok(NodeChainEntry::Full(operation_from_wire(wire)?)),
    }
}

// The snapshot tail spans many authoring nodes, so it is validated per-node:
// each node's chain must be dense and monotonic in `node_seq`. Operations of
// different nodes may interleave on the wire — group and check each chain.
fn validate_snapshot_tail_wire(payload: &MeshSyncSnapshotResponsePayload) -> LedgerResult<()> {
    for wire in &payload.operations_after_snapshot {
        if wire.partition_id != payload.partition_id {
            return Err(SyncLedgerError::Runtime(
                "sync snapshot response tail partition mismatch".to_string(),
            ));
        }
    }
    validate_node_dense_wire(
        payload.operations_after_snapshot.iter(),
        "sync snapshot response tail",
    )
}

fn validate_pull_response_wire(payload: &MeshSyncPullResponsePayload) -> LedgerResult<()> {
    for wire in &payload.operations {
        if wire.node_seq == 0 {
            return Err(SyncLedgerError::Runtime(
                "sync pull response contains zero node_seq".to_string(),
            ));
        }
    }
    // A pull is for one target node's chain, but the wire only carries node_seq
    // per op; the authoring node lives inside the encoded body (full) or the
    // redacted envelope. Decode each entry and check it belongs to
    // `target_node_id` with a dense monotonic chain. Redacted placeholders count
    // toward density exactly like full ops — they ARE chain positions.
    let mut decoded = Vec::with_capacity(payload.operations.len());
    for wire in &payload.operations {
        let entry = chain_entry_from_wire(wire)?;
        if chain_entry_node_id(&entry) != payload.target_node_id {
            return Err(SyncLedgerError::Runtime(
                "sync pull response node mismatch".to_string(),
            ));
        }
        decoded.push(wire.clone());
    }
    let mut expected = payload.from_node_seq;
    let mut sorted: Vec<&MeshSyncOperationWire> = decoded.iter().collect();
    sorted.sort_by_key(|wire| wire.node_seq);
    for wire in sorted {
        if wire.node_seq != expected {
            return Err(SyncLedgerError::Runtime(format!(
                "sync pull response node_seq gap: expected {expected}, actual {}",
                wire.node_seq
            )));
        }
        expected = expected.saturating_add(1);
    }
    Ok(())
}

/// Validates that, grouped by authoring node, every operation forms a dense
/// monotonic `node_seq` run starting at each node's first seen seq. Used for the
/// snapshot tail, where several nodes' chains arrive interleaved.
fn validate_node_dense_wire<'a>(
    operations: impl Iterator<Item = &'a MeshSyncOperationWire>,
    context: &str,
) -> LedgerResult<()> {
    let mut decoded: Vec<SyncOperation> = Vec::new();
    for wire in operations {
        decoded.push(operation_from_wire(wire)?);
    }
    decoded.sort_by(|a, b| {
        a.body
            .actor_node_id
            .cmp(&b.body.actor_node_id)
            .then_with(|| a.body.node_seq.cmp(&b.body.node_seq))
    });
    let mut current: Option<(&str, u64)> = None;
    for operation in &decoded {
        let node = operation.body.actor_node_id.as_str();
        let seq = operation.body.node_seq;
        match current {
            Some((prev_node, prev_seq)) if prev_node == node => {
                if seq != prev_seq.saturating_add(1) {
                    return Err(SyncLedgerError::Runtime(format!(
                        "{context} node_seq gap for {node}: expected {}, actual {seq}",
                        prev_seq.saturating_add(1)
                    )));
                }
            }
            _ => {}
        }
        current = Some((node, seq));
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
    // Fold the incoming HLC into the local clock so a locally-minted KV write that
    // follows is strictly later than anything observed from the mesh.
    observe_core_hlc(&operation.body.hlc_timestamp);

    let mut conn = db
        .write()
        .map_err(|e| SyncLedgerError::Runtime(format!("Blad blokady bazy: {e}")))?;
    let tx = conn
        .transaction()
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;

    // HLC last-writer-wins gate keyed by the per-(instance_id, key) resource_id.
    // Two nodes writing the same addon key concurrently used to materialize in
    // delivery order (blind upsert), so the converged value depended on which push
    // arrived last — the same convergence hole as `external-credentials`. Gating on
    // the recorded HLC makes the outcome the operation with the greatest HLC under
    // the total (wall, logical, node_id) order, regardless of delivery order. The
    // gate, the row write and the version bump share one transaction so the bookmark
    // can never drift from the materialized row. A re-delivered op (same op_hash)
    // carries an HLC equal to the stored one, fails the strict `>` test, and is a
    // no-op — idempotent. Delete is an LWW tombstone: a delete with a higher HLC
    // wins over a later-delivered-but-older insert, and the version row survives the
    // delete so a stale insert that arrives afterwards is still dropped.
    if !crate::sync::core_materializer::incoming_hlc_wins(
        &tx,
        &operation.body.resource_type,
        &operation.body.resource_id,
        &operation.body.hlc_timestamp,
    )? {
        tx.commit()
            .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        return Ok(0);
    }

    let rows = match operation.body.action {
        ActionType::Update | ActionType::Insert => {
            let value = field_bytes(operation, "value")?;
            let value_size = value.len() as i64;
            tx.execute(
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
            .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?
        }
        ActionType::Delete => tx
            .execute(
                "DELETE FROM addon_storage \
                 WHERE addon_id = ?1 AND instance_id = ?2 AND storage_key = ?3",
                rusqlite::params![&operation.body.addon_id, instance_id, key],
            )
            .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?,
    };

    crate::sync::core_materializer::upsert_resource_version(
        &tx,
        &operation.body.resource_type,
        &operation.body.resource_id,
        &operation.body.hlc_timestamp,
    )?;

    tx.commit()
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
    Ok(rows)
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

fn inbox_apply_order(entry: &InboxEntry) -> (u8, &str, &HybridLogicalTimestamp) {
    // Apply within a partition in HLC order: there is no global partition
    // sequence anymore, and HLC is the total order materialization (LWW) relies
    // on, so draining in HLC order keeps causal inserts ahead of their updates.
    (
        inbox_apply_priority(&entry.operation),
        entry.operation.body.partition_id.as_str(),
        &entry.operation.body.hlc_timestamp,
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
    Ok(crate::paths::blobs_dir()
        .join(&sha[0..2])
        .join(&sha[2..4])
        .join(format!("{sha}.bin")))
}

fn blob_chunk_dir(sha: &str) -> LedgerResult<std::path::PathBuf> {
    validate_blob_sha(sha)?;
    Ok(crate::paths::sync_dir()
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

/// Reads a string-valued field from a capture/operation field map, returning None
/// when absent or not a string (e.g. a Delete capture carrying no columns).
fn changed_field_string(fields: &BTreeMap<String, FieldValue>, key: &str) -> Option<String> {
    match fields.get(key) {
        Some(FieldValue::String(value)) => Some(value.clone()),
        _ => None,
    }
}

/// Parses an `addon-package:<package_id>:<version>` blob id. `package_id` and
/// semver `version` never contain `:`, so the first `:` after the prefix splits
/// them unambiguously. Returns None for any other blob id.
fn parse_addon_package_blob_id(blob_id: &str) -> Option<(String, String)> {
    let rest = blob_id.strip_prefix("addon-package:")?;
    let (package_id, version) = rest.split_once(':')?;
    if package_id.is_empty() || version.is_empty() {
        return None;
    }
    Some((package_id.to_string(), version.to_string()))
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
    use std::time::Duration;

    struct RuntimeHarness {
        runtime: SyncRuntime,
        _ledger_dir: tempfile::TempDir,
    }

    fn make_db() -> DbPool {
        let conn = Connection::open_in_memory().expect("open db");
        migrations::run(&conn).expect("run migrations");
        Arc::new(crate::db::Db::from_connection(conn))
    }

    fn make_db_at(path: &Path) -> DbPool {
        let conn = Connection::open(path).expect("open persistent db");
        migrations::run(&conn).expect("run migrations");
        Arc::new(crate::db::Db::from_connection(conn))
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
                addon_reconciler: parking_lot::RwLock::new(None),
                max_inbox_defer_attempts: MAX_INBOX_DEFER_ATTEMPTS,
                adopt_inflight: parking_lot::Mutex::new(HashSet::new()),
                pending_epoch_reconcile: parking_lot::Mutex::new(Vec::new()),
                floor_anchor_warned: parking_lot::Mutex::new(HashSet::new()),
                push_state: PushState::default(),
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
            addon_reconciler: parking_lot::RwLock::new(None),
            max_inbox_defer_attempts: MAX_INBOX_DEFER_ATTEMPTS,
            adopt_inflight: parking_lot::Mutex::new(HashSet::new()),
            pending_epoch_reconcile: parking_lot::Mutex::new(Vec::new()),
            floor_anchor_warned: parking_lot::Mutex::new(HashSet::new()),
            push_state: PushState::default(),
        }
    }

    async fn make_mesh_manager(runtime: &SyncRuntime) -> Arc<IrohMeshManager> {
        let cfg = IrohMeshConfig {
            node_id: String::new(),
            bind_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            relay_url: None,
            enable_lan_discovery: false,
            enable_dht_discovery: false,
            ..Default::default()
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
                None,
            )
            .expect("source trusts receiver");
        receiver
            .signer
            .security
            .add_trusted_key(
                &source.local_node_id,
                &source.signer.security.public_key_hex(),
                "source",
                None,
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

    /// Like `seed_core_authority_target` but scoped to a single `resource_id`, so a
    /// node can be a sync target for ONE flow on the authority chain and not for
    /// another flow authored on the same chain. Used to exercise per-op redaction.
    fn seed_core_authority_target_for_resource(
        db: &DbPool,
        resource_type: &str,
        resource_id: &str,
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
            &format!("policy-core-{resource_type}-{resource_id}"),
            "org-default",
            crate::sync::core_registry::CORE_SYNC_ADDON_ID,
            Some(resource_type),
            Some(resource_id),
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
        let conn = db.write().expect("db");
        conn.execute(
            "INSERT INTO user_accounts (id, username, password_hash) VALUES (?1, ?1, 'h')",
            rusqlite::params![actor_id],
        )
        .expect("seed actor user");
    }

    /// Inserts a live `flows` row so a baseline reset has current state to
    /// re-seed from. The capture journal is intentionally left untouched.
    fn seed_flow_row(db: &DbPool, id: &str, name: &str) {
        let conn = db.write().expect("db");
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
        // Also pin TENTAFLOW_HOME: tentaflow_home() prefers the repo's live
        // .runtime/ over HOME, so HOME alone would not isolate the test.
        let prev_tf = std::env::var_os("TENTAFLOW_HOME");
        std::env::set_var("TENTAFLOW_HOME", tmp.path());
        f();
        if let Some(p) = prev {
            std::env::set_var("HOME", p);
        } else {
            std::env::remove_var("HOME");
        }
        if let Some(p) = prev_tf {
            std::env::set_var("TENTAFLOW_HOME", p);
        } else {
            std::env::remove_var("TENTAFLOW_HOME");
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
                    None,
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
                let conn = receiver.runtime.db.write().expect("db lock");
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

    /// A workspace created on node A must be visible on node B — the product
    /// promise "start on the desktop, finish on the phone" is exactly this.
    /// Node B stores the OWNER node id, which is what lets it render the
    /// workspace while refusing to run it.
    #[test]
    fn core_materializer_replicates_a_code_studio_workspace_to_another_node() {
        with_tmp_home(|| {
            let source = make_runtime(61);
            let receiver = make_runtime(62);

            let new = crate::code_studio::models::NewWorkspace {
                id: "ws-mesh-1".into(),
                org_id: crate::services::org::DEFAULT_ORG_ID.into(),
                owner_user_id: "u-owner".into(),
                name: "TentaFlow Core".into(),
                slug: "tentaflow-core".into(),
                node_id: "node-a".into(),
                exec_mode: crate::code_studio::models::ExecMode::TrustedNative,
                container_image: None,
                egress_enforcement: crate::code_studio::models::EgressEnforcement::Unrestricted,
                repo_kind: "git".into(),
                repo_url: Some("https://example.invalid/repo.git".into()),
                repo_auth_kind: Some("token".into()),
                // A HANDLE, not material: the vault row it points at stays on
                // node A, so node B answers `secret_missing` instead of pretending.
                secret_ref: Some("secret-handle-1".into()),
                ssh_host_fingerprint: None,
                default_branch: None,
                target_branch: None,
                autonomy_ceiling: crate::code_studio::models::AutonomyMode::Normal,
                egress_policy: "org_approved".into(),
                index_enabled: false,
                quota_disk_bytes: Some(1_073_741_824),
                quota_sessions: Some(3),
            };
            crate::code_studio::repository::create_workspace(&source.runtime.db, &new)
                .expect("create workspace");

            let mut ops = Vec::new();
            crate::sync::core_capture::drain_pending_core_captures_with(
                &source.runtime.db,
                64,
                |capture| {
                    let record = source.runtime.record_core_capture(capture)?;
                    ops.push(record.op_id);
                    Ok(Some(record.op_id))
                },
            )
            .expect("drain captures");
            let operations: Vec<_> = ops
                .iter()
                .map(|op_id| source.runtime.ledger.get_operation(*op_id).expect("op"))
                .collect();
            // Creating a workspace captures three things, and all three have to
            // reach the other node: the row, the owner membership, and the
            // read-only `exec` programs it is seeded with — a node that got the
            // workspace without them would ask about `ls`.
            let of_type = |kind: &str| {
                operations
                    .iter()
                    .filter(|op| op.body.resource_type == kind)
                    .count()
            };
            assert_eq!(of_type("core.code_workspace"), 1);
            assert_eq!(of_type("core.code_workspace_member"), 1);
            assert!(
                of_type("core.code_workspace_allowlist") > 0,
                "the seeded standing grants never left the node"
            );

            // The membership carries an FK to the workspace. Arriving first it
            // must stay retryable, not become a terminal conflict.
            let member_first = operations
                .iter()
                .find(|op| op.body.resource_type == "core.code_workspace_member")
                .expect("member op");
            let deferred = crate::sync::core_materializer::apply_core_operation(
                &receiver.runtime.db,
                &receiver.runtime.settings_cipher,
                member_first,
            )
            .expect_err("member before workspace must not apply");
            assert!(
                matches!(deferred, SyncLedgerError::DeferredOrdering(_)),
                "expected a retryable ordering gap, got {deferred:?}"
            );

            // Apply the workspace first, then the rest — which is what the inbox
            // achieves by retrying the deferred entry on the next drain.
            let mut ordered: Vec<_> = operations.iter().collect();
            ordered.sort_by_key(|op| op.body.resource_type != "core.code_workspace");
            for operation in ordered {
                crate::sync::core_materializer::apply_core_operation(
                    &receiver.runtime.db,
                    &receiver.runtime.settings_cipher,
                    operation,
                )
                .expect("apply code studio operation");
            }

            let landed =
                crate::code_studio::repository::get_workspace(&receiver.runtime.db, "ws-mesh-1")
                    .expect("get workspace")
                    .expect("workspace must exist on the receiving node");
            assert_eq!(landed.name, "TentaFlow Core");
            assert_eq!(landed.node_id, "node-a");
            assert_eq!(landed.status, "provisioning");
            assert_eq!(landed.quota_sessions, Some(3));
            assert_eq!(landed.secret_ref.as_deref(), Some("secret-handle-1"));
            // `is_local` is derived from this comparison in the dispatch layer;
            // the receiver's own node id is not node-a, so the workspace is remote.
            assert_ne!(landed.node_id, receiver.runtime.local_node_id);

            let role = crate::code_studio::repository::role_of(
                &receiver.runtime.db,
                "ws-mesh-1",
                "u-owner",
            )
            .expect("role");
            assert_eq!(
                role,
                Some(crate::code_studio::models::WorkspaceRole::Owner),
                "a workspace without its owner membership is unreachable"
            );
        });
    }

    /// CR-001 seed-guard: a flow locally marked `is_system=1` is seed-owned —
    /// EVERY remote write (Insert/Update/Delete) must be rejected, including
    /// an Update forging is_system=true (the seed never captures, so no such
    /// op is legitimate). The wire is_system flag is coerced to 0 on Insert:
    /// a new flow claiming is_system=true lands as a regular, deletable flow.
    #[test]
    fn core_materializer_guards_local_system_flow() {
        with_tmp_home(|| {
            let source = make_runtime(25);
            let receiver = make_runtime(26);
            const SYS_ID: &str = "sys-flow-1";
            {
                let conn = receiver.runtime.db.write().expect("db lock");
                conn.execute(
                    "INSERT INTO flows (id, name, flow_json, status, is_system) \
                     VALUES (?1, 'System Flow', '{\"nodes\":[]}', 'active', 1)",
                    rusqlite::params![SYS_ID],
                )
                .expect("seed system flow");
            }
            let hlc_at = |offset: i64| HybridLogicalTimestamp {
                wall_time_ms: now_ms() + offset,
                logical: 0,
                node_id: "test-node".to_string(),
            };
            let record_and_apply = |resource_id: &str,
                                    action: SqlWriteAction,
                                    fields: BTreeMap<String, FieldValue>,
                                    offset: i64| {
                let capture = crate::sync::core_capture::CoreWriteCapture::new(
                    crate::sync::core_registry::CoreSyncResourceKind::Flow,
                    "org-default",
                    resource_id,
                    action,
                    fields,
                    None,
                    hlc_at(offset),
                    test_epoch(),
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
                .expect("apply core operation")
            };
            let local_flow = || {
                repository::get_flow(&receiver.runtime.db, SYS_ID)
                    .expect("get flow")
                    .expect("flow exists")
            };

            // User-style Update (is_system=false on the wire) → rejected.
            let mut user_update = BTreeMap::new();
            user_update.insert(
                "flow_json".to_string(),
                FieldValue::String(r#"{"nodes":[{"id":"hacked"}]}"#.to_string()),
            );
            user_update.insert("is_system".to_string(), FieldValue::Bool(false));
            let rows = record_and_apply(SYS_ID, SqlWriteAction::Update, user_update, 1);
            assert_eq!(rows, 0, "user update of a system flow must be rejected");
            assert_eq!(local_flow().flow_json, r#"{"nodes":[]}"#);

            // Delete → rejected, the row stays.
            let mut del_fields = BTreeMap::new();
            del_fields.insert("id".to_string(), FieldValue::String(SYS_ID.to_string()));
            let rows = record_and_apply(SYS_ID, SqlWriteAction::Delete, del_fields, 2);
            assert_eq!(rows, 0, "delete of a system flow must be rejected");
            assert!(local_flow().is_system);

            // Forged "seed refresh" (Update carrying is_system=true) → also
            // rejected: the local seed is the only writer of system flows.
            let mut forged = BTreeMap::new();
            forged.insert(
                "flow_json".to_string(),
                FieldValue::String(r#"{"nodes":[{"id":"forged"}]}"#.to_string()),
            );
            forged.insert("is_system".to_string(), FieldValue::Bool(true));
            let rows = record_and_apply(SYS_ID, SqlWriteAction::Update, forged, 3);
            assert_eq!(rows, 0, "forged seed refresh must be rejected");
            let guarded = local_flow();
            assert_eq!(guarded.flow_json, r#"{"nodes":[]}"#);
            assert!(guarded.is_system);

            // Insert of a NEW id claiming is_system=true → applied, but the
            // wire flag is coerced to 0: the row is a regular, deletable flow.
            const NEW_ID: &str = "user-flow-1";
            let mut forged_insert = BTreeMap::new();
            forged_insert.insert(
                "name".to_string(),
                FieldValue::String("Forged System".to_string()),
            );
            forged_insert.insert(
                "flow_json".to_string(),
                FieldValue::String(r#"{"nodes":[]}"#.to_string()),
            );
            forged_insert.insert("is_system".to_string(), FieldValue::Bool(true));
            let rows = record_and_apply(NEW_ID, SqlWriteAction::Insert, forged_insert, 4);
            assert_eq!(rows, 1, "insert of a new flow must apply");
            let inserted = repository::get_flow(&receiver.runtime.db, NEW_ID)
                .expect("get flow")
                .expect("flow exists");
            assert!(!inserted.is_system, "wire is_system must be coerced to 0");
            let mut del_new = BTreeMap::new();
            del_new.insert("id".to_string(), FieldValue::String(NEW_ID.to_string()));
            let rows = record_and_apply(NEW_ID, SqlWriteAction::Delete, del_new, 5);
            assert_eq!(rows, 1, "the coerced flow must stay deletable");
            assert!(repository::get_flow(&receiver.runtime.db, NEW_ID)
                .expect("get flow")
                .is_none());
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
                let conn = source.runtime.db.read().expect("db lock");
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
                let conn = source.runtime.db.read().expect("db lock");
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
    fn pull_redacts_unsubscribed_op_and_advances_chain() {
        // Regression for the per-node chain serving deadlock: one authority chain
        // mixes a flow the receiver subscribes to (flow-allow, seq 1) and one it
        // does not (flow-deny, seq 2). A chain pull must not abort on the denied
        // op — it serves a redacted placeholder for it so the receiver advances its
        // frontier past BOTH and materializes only the allowed flow.
        with_tmp_home(|| {
            let source = make_runtime(140);
            let receiver = make_runtime(141);
            // Receiver is a sync target ONLY for flow-allow, on both sides.
            seed_core_authority_target_for_resource(
                &source.runtime.db,
                "core.flow",
                "flow-allow",
                &receiver.runtime.local_node_id,
            );
            seed_core_authority_target_for_resource(
                &receiver.runtime.db,
                "core.flow",
                "flow-allow",
                &receiver.runtime.local_node_id,
            );

            source
                .runtime
                .record_core_capture(complete_core_flow_capture("flow-allow", "Allowed Flow"))
                .expect("record allow");
            source
                .runtime
                .record_core_capture(complete_core_flow_capture("flow-deny", "Denied Flow"))
                .expect("record deny");

            let pull = MeshSyncPullPayload {
                from_node_id: receiver.runtime.local_node_id.clone(),
                target_node_id: source.runtime.local_node_id.clone(),
                from_node_seq: 1,
                limit: 64,
            };
            let response = source
                .runtime
                .handle_pull_payload(&receiver.runtime.local_node_id, pull)
                .expect("pull serves both, denied one redacted");
            let MeshSyncPullResult::Operations(payload) = response else {
                panic!("expected operations response");
            };
            // Both chain positions are served; the denied one is redacted (no body,
            // no partition leak), the allowed one is full.
            assert_eq!(payload.operations.len(), 2);
            let redacted: Vec<&MeshSyncOperationWire> = payload
                .operations
                .iter()
                .filter(|op| op.redacted.is_some())
                .collect();
            assert_eq!(redacted.len(), 1, "exactly the denied op is redacted");
            assert!(
                redacted[0].operation.is_empty(),
                "redacted op carries no body"
            );
            assert!(
                redacted[0].partition_id.is_empty(),
                "redacted op withholds the partition name"
            );

            receiver
                .runtime
                .handle_pull_response_payload(&source.runtime.local_node_id, payload)
                .expect("apply pulled chain");

            // Frontier advanced past BOTH positions — the chain is not stalled.
            let frontier = receiver
                .runtime
                .ledger
                .get_node_frontier(&source.runtime.local_node_id)
                .expect("frontier")
                .expect("frontier present");
            assert_eq!(frontier.last_seq, 2);

            // Only the allowed flow materialized; the denied one never reached SQL.
            assert!(repository::get_flow(&receiver.runtime.db, "flow-allow")
                .expect("get allow")
                .is_some());
            assert!(
                repository::get_flow(&receiver.runtime.db, "flow-deny")
                    .expect("get deny")
                    .is_none(),
                "redacted op must never materialize"
            );
        });
    }

    #[test]
    fn redacted_op_upgrades_to_full_when_permission_is_granted() {
        // End-to-end proof that the BLOCKER is closed in the production path: the
        // receiver first holds the op only as a redacted placeholder (frontier moved
        // past it). The grant goes through the REAL permission write (upsert_sync_policy,
        // which bumps the permission epoch); the authority's scheduled backfill then
        // re-enqueues the already-minted op on its own, and a normal push upgrades
        // the placeholder to the full body and materializes it. No full op is handed
        // to the receiver by the test.
        with_tmp_home(|| {
            let source = make_runtime(142);
            let receiver = make_runtime(143);

            // Mint the flow with NO target for the receiver: the source serves it
            // redacted, so the receiver advances its frontier past it without a body.
            let op = source
                .runtime
                .record_core_capture(complete_core_flow_capture("flow-x", "Granted Flow"))
                .expect("record");
            let full = source
                .runtime
                .ledger
                .get_operation(op.op_id)
                .expect("full operation");

            // Phase 1: a real pull from the source serves the op redacted (receiver
            // is not yet a target), so the receiver holds only a placeholder.
            let pull = MeshSyncPullPayload {
                from_node_id: receiver.runtime.local_node_id.clone(),
                target_node_id: source.runtime.local_node_id.clone(),
                from_node_seq: 1,
                limit: 64,
            };
            let MeshSyncPullResult::Operations(redacted_payload) = source
                .runtime
                .handle_pull_payload(&receiver.runtime.local_node_id, pull)
                .expect("serve redacted")
            else {
                panic!("expected operations response");
            };
            assert_eq!(redacted_payload.operations.len(), 1);
            assert!(
                redacted_payload.operations[0].redacted.is_some(),
                "served redacted while receiver is not a target"
            );
            receiver
                .runtime
                .handle_pull_response_payload(&source.runtime.local_node_id, redacted_payload)
                .expect("apply redacted");
            assert!(
                receiver
                    .runtime
                    .ledger
                    .get_redacted_record(full.op_id)
                    .expect("redacted lookup")
                    .is_some(),
                "held as redacted placeholder"
            );
            assert!(repository::get_flow(&receiver.runtime.db, "flow-x")
                .expect("get flow")
                .is_none());

            // Phase 2: grant via the real permission write. This bumps the source's
            // permission epoch; the receiver gets the same policy so it may now
            // materialize the body.
            seed_core_authority_target_for_resource(
                &source.runtime.db,
                "core.flow",
                "flow-x",
                &receiver.runtime.local_node_id,
            );
            seed_core_authority_target_for_resource(
                &receiver.runtime.db,
                "core.flow",
                "flow-x",
                &receiver.runtime.local_node_id,
            );

            // The authority's scheduled backfill re-enqueues the already-minted op
            // for the freshly-permitted receiver — with NO manual payload.
            let enqueued = source
                .runtime
                .backfill_outbox_for_permission_grants()
                .expect("backfill");
            assert!(enqueued >= 1, "grant must re-enqueue the minted op");

            // A normal push now ships the full op; the receiver upgrades + materializes.
            let push = source
                .runtime
                .build_push_payload_for_target(&receiver.runtime.local_node_id, 16)
                .expect("push")
                .expect("pending push after backfill");
            assert!(
                push.operations.iter().all(|op| op.redacted.is_none()),
                "authority ships the full body to a permitted target"
            );
            let ack = receiver
                .runtime
                .handle_push_payload(&source.runtime.local_node_id, push)
                .expect("handle push");
            source
                .runtime
                .handle_ack_payload(&receiver.runtime.local_node_id, ack)
                .expect("ack");

            // The redacted placeholder is gone and the flow is materialized.
            assert!(
                receiver
                    .runtime
                    .ledger
                    .get_redacted_record(full.op_id)
                    .expect("redacted lookup")
                    .is_none(),
                "redacted placeholder superseded by full op"
            );
            let flow = repository::get_flow(&receiver.runtime.db, "flow-x")
                .expect("get flow")
                .expect("flow materialized after upgrade");
            assert_eq!(flow.name, "Granted Flow");

            // A second backfill is a no-op: the epoch marker is now current, so the
            // grant is not re-applied and the outbox is not re-enqueued.
            let again = source
                .runtime
                .backfill_outbox_for_permission_grants()
                .expect("idempotent backfill");
            assert_eq!(
                again, 0,
                "backfill is idempotent once the epoch is recorded"
            );
        });
    }

    #[test]
    fn backfill_only_acts_on_orgs_whose_epoch_advanced() {
        // Proves the epoch pre-gate is org-scoped: an op exists under `org-default`
        // with a target it has NOT yet been fanned out to, so it WOULD be enqueued
        // if `org-default` advanced. We instead advance ONLY a different org's epoch.
        // The backfill must scan nothing for `org-default` (its marker is current)
        // and therefore enqueue 0 — no over-share from an unrelated grant.
        with_tmp_home(|| {
            let source = make_runtime(160);
            let receiver = make_runtime(161);

            source
                .runtime
                .record_core_capture(complete_core_flow_capture("flow-y", "Other-Org Flow"))
                .expect("record");

            // A genuine target exists for the op, but no grant bumped `org-default`,
            // so the mint-time fan-out already covered it and the marker is current.
            seed_core_authority_target_for_resource(
                &source.runtime.db,
                "core.flow",
                "flow-y",
                &receiver.runtime.local_node_id,
            );
            // Record `org-default`'s current epoch as the backfill marker so the only
            // way this op gets re-enqueued is a FUTURE `org-default` epoch advance.
            let baseline = repository::get_sync_permission_epoch(
                &source.runtime.db,
                crate::services::org::DEFAULT_ORG_ID,
            )
            .expect("epoch");
            repository::set_setting(
                &source.runtime.db,
                &backfill_epoch_marker_key(crate::services::org::DEFAULT_ORG_ID),
                &baseline.to_string(),
            )
            .expect("seed marker");

            // Advance ONLY an unrelated org's permission epoch.
            let other = crate::services::org::repo::create_organization(
                &source.runtime.db,
                "Other",
                "other-org",
                None,
                None,
                None,
                None,
            )
            .expect("create org");
            repository::bump_sync_permission_epoch(&source.runtime.db, &other.org_id)
                .expect("bump other org epoch");

            // `org-default` did not advance, so its op is never scanned/enqueued.
            let enqueued = source
                .runtime
                .backfill_outbox_for_permission_grants()
                .expect("backfill");
            assert_eq!(
                enqueued, 0,
                "an unrelated org's grant must not enqueue another org's op"
            );
            assert!(
                source
                    .runtime
                    .ledger
                    .list_outbox_for_operation(
                        source.runtime.ledger.list_all_operations().expect("ops")[0].op_id
                    )
                    .expect("outbox")
                    .is_empty(),
                "no outbox entry was created for the non-advanced org's op"
            );
        });
    }

    #[test]
    fn redacted_upgrade_does_not_regress_node_frontier() {
        // CR-002: admitting a full body BELOW the frontier (the redacted→full
        // upgrade) must not rewind the per-node frontier. Build a chain where the
        // receiver advances its frontier to seq 2 holding seq 1 only as a redacted
        // placeholder, then upgrade seq 1 to full and assert the frontier stays at 2.
        with_tmp_home(|| {
            let source = make_runtime(150);
            let receiver = make_runtime(151);
            // Receiver subscribes to flow-b (seq 2) but not flow-a (seq 1).
            seed_core_authority_target_for_resource(
                &source.runtime.db,
                "core.flow",
                "flow-b",
                &receiver.runtime.local_node_id,
            );
            seed_core_authority_target_for_resource(
                &receiver.runtime.db,
                "core.flow",
                "flow-b",
                &receiver.runtime.local_node_id,
            );

            let op_a = source
                .runtime
                .record_core_capture(complete_core_flow_capture("flow-a", "Flow A"))
                .expect("record a");
            source
                .runtime
                .record_core_capture(complete_core_flow_capture("flow-b", "Flow B"))
                .expect("record b");

            // Pull both: seq 1 redacted, seq 2 full. Frontier advances to 2.
            let pull = MeshSyncPullPayload {
                from_node_id: receiver.runtime.local_node_id.clone(),
                target_node_id: source.runtime.local_node_id.clone(),
                from_node_seq: 1,
                limit: 64,
            };
            let MeshSyncPullResult::Operations(payload) = source
                .runtime
                .handle_pull_payload(&receiver.runtime.local_node_id, pull)
                .expect("serve chain")
            else {
                panic!("expected operations response");
            };
            receiver
                .runtime
                .handle_pull_response_payload(&source.runtime.local_node_id, payload)
                .expect("apply chain");
            let frontier_before = receiver
                .runtime
                .ledger
                .get_node_frontier(&source.runtime.local_node_id)
                .expect("frontier")
                .expect("frontier present");
            assert_eq!(frontier_before.last_seq, 2);

            // Grant flow-a and upgrade seq 1 to full via the real backfill + push.
            seed_core_authority_target_for_resource(
                &source.runtime.db,
                "core.flow",
                "flow-a",
                &receiver.runtime.local_node_id,
            );
            seed_core_authority_target_for_resource(
                &receiver.runtime.db,
                "core.flow",
                "flow-a",
                &receiver.runtime.local_node_id,
            );
            source
                .runtime
                .backfill_outbox_for_permission_grants()
                .expect("backfill");
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

            // flow-a materialized (the upgrade applied)…
            assert!(
                repository::get_flow(&receiver.runtime.db, "flow-a")
                    .expect("get a")
                    .is_some(),
                "upgraded op materialized"
            );
            // …and the frontier did NOT regress to seq 1.
            let frontier_after = receiver
                .runtime
                .ledger
                .get_node_frontier(&source.runtime.local_node_id)
                .expect("frontier")
                .expect("frontier present");
            assert_eq!(
                frontier_after.last_seq, 2,
                "upgrade below the frontier must not rewind it"
            );
            assert_eq!(
                frontier_after.last_hash, frontier_before.last_hash,
                "frontier hash stays anchored to the real chain head"
            );
            let _ = op_a;
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
                .read()
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
                let conn = receiver.runtime.db.write().expect("db lock");
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
                .read()
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

    /// Minimal installed-instance row so `installed_addon_ids_for_package` and the
    /// `core.addon_instance` target resolution see the package as installed.
    fn seed_installed_addon(db: &DbPool, package_id: &str, version: &str, addon_id: &str) {
        let conn = db.write().expect("db");
        conn.execute(
            "INSERT INTO addons (addon_id, name, version, package_id, package_version) \
             VALUES (?1, ?1, ?2, ?3, ?2)",
            rusqlite::params![addon_id, version, package_id],
        )
        .expect("seed addon");
    }

    /// Records and ledgers an `addon-package:` blob the way an upload does (capture
    /// row + chunked ledger operations). Returns the content hash.
    fn record_addon_package_blob(
        runtime: &SyncRuntime,
        package_id: &str,
        version: &str,
        bytes: &[u8],
    ) -> String {
        let sha = hex::encode(sha256(bytes));
        let dir = tempfile::tempdir().expect("blob dir");
        let path = dir.path().join("pkg.tar.gz");
        std::fs::write(&path, bytes).expect("write blob");
        let capture = crate::sync::blob_capture::BlobWriteCapture::new(
            "org-default",
            format!("addon-package:{package_id}:{version}"),
            &sha,
            "application/gzip",
            bytes.len() as u64,
            path.to_string_lossy().to_string(),
            None,
        );
        {
            let conn = runtime.db.write().expect("db");
            crate::sync::blob_capture::record_blob_write_capture(&conn, &capture)
                .expect("record capture row");
        }
        runtime
            .record_blob_capture(capture)
            .expect("ledger blob capture");
        sha
    }

    /// Regression: an uploaded addon package's bytes must reach a STANDARD trusted
    /// peer — not only `authority`-profile nodes. The `core.blob` resource type has
    /// no default sync policy and is not a "default core resource", so before the
    /// instance-coupling fix the blob got zero targets and the addon showed up
    /// installed on the peer with its wasm missing. Uses the real production
    /// `ensure_default_core_sync_policies` (NOT a hand-seeded authority target).
    #[test]
    fn addon_package_blob_follows_instance_to_standard_node() {
        with_tmp_home(|| {
            let source = make_runtime(71);
            let db = source.runtime.db.clone();
            repository::ensure_default_core_sync_policies(&db).expect("default core policies");
            repository::upsert_sync_node_identity(
                &db,
                "receiver-node",
                "pub",
                "ed25519",
                "Receiver",
                "server",
                "trusted",
                None,
                "standard",
            )
            .expect("standard trusted node");

            // Bytes captured BEFORE any instance references the package (the upload
            // ordering): no instance → no targets → blob ops sit unrouted.
            let sha = record_addon_package_blob(&source.runtime, "pkg-x", "1.0.0", b"wasm-bytes");
            let blob_ops = source
                .runtime
                .blob_operations_for_sha(&sha)
                .expect("blob ops");
            assert!(!blob_ops.is_empty(), "blob must be chunked into the ledger");
            for op in &blob_ops {
                let outbox = source
                    .runtime
                    .ledger
                    .list_outbox_for_operation(op.op_id)
                    .expect("outbox");
                assert!(
                    outbox.is_empty(),
                    "blob must not route before an instance references the package"
                );
            }

            // Now the instance exists: the package's targets equal the instance's.
            seed_installed_addon(&db, "pkg-x", "1.0.0", "inst-x");
            let nodes = repository::list_sync_target_nodes_for_addon_package(
                &db,
                "org-default",
                "pkg-x",
                "1.0.0",
            )
            .expect("package targets");
            assert_eq!(nodes, vec!["receiver-node".to_string()]);

            // Coupling the blob to the instance routes every blob op to the node.
            let queued = source
                .runtime
                .queue_addon_package_blob("org-default", "pkg-x", "1.0.0")
                .expect("queue addon package blob");
            assert!(queued > 0, "blob ops must be queued to the standard node");
            for op in &blob_ops {
                let outbox = source
                    .runtime
                    .ledger
                    .list_outbox_for_operation(op.op_id)
                    .expect("outbox");
                assert!(
                    outbox
                        .iter()
                        .any(|entry| entry.target.as_str() == "receiver-node"),
                    "blob op missing outbox entry for the standard trusted node"
                );
            }

            // Idempotent: re-running adds nothing.
            let again = source
                .runtime
                .queue_addon_package_blob("org-default", "pkg-x", "1.0.0")
                .expect("requeue addon package blob");
            assert_eq!(again, 0, "blob coupling must be idempotent");
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
    fn push_backoff_skips_unacked_backlog_until_new_op_or_deadline() {
        // A non-ACK peer must NOT have its whole un-acked backlog re-scanned and
        // re-shipped on every scheduler tick (the idle-CPU regression). After one
        // push the per-target backoff suppresses the next push (returns None) while
        // the deadline is in the future, so per-tick cost is a single in-memory
        // lookup independent of ledger size. A freshly queued op resets the backoff
        // so live data still ships immediately, and an ACK drains the backlog.
        with_tmp_home(|| {
            let source = make_runtime(131);
            let receiver = make_runtime(132);
            let addon_id = "sync-runtime-backoff";
            seed_authority_target(
                &source.runtime.db,
                addon_id,
                &receiver.runtime.local_node_id,
            );
            open_contacts_table(addon_id);
            let target = SyncTarget::new(receiver.runtime.local_node_id.clone()).expect("target");

            let first = source
                .runtime
                .record_sql_capture(capture(addon_id, "person-1", "Ewa"))
                .expect("record first");

            // First push ships the op and arms the backoff (peer has not ACKed).
            let push = source
                .runtime
                .build_push_payload_for_target(&receiver.runtime.local_node_id, 16)
                .expect("push")
                .expect("first push ships");
            assert_eq!(push.operations.len(), 1);

            // Second push within the backoff window: nothing new queued, peer still
            // un-acked, so the whole un-acked backlog is NOT re-shipped.
            assert!(
                source
                    .runtime
                    .build_push_payload_for_target(&receiver.runtime.local_node_id, 16)
                    .expect("second push")
                    .is_none(),
                "un-acked backlog must not be re-shipped while backoff holds"
            );

            // A freshly queued op clears the backoff: the next push ships and now
            // carries BOTH the new op and the still-un-acked first op (the peer
            // never confirmed it, so it must keep being delivered).
            source
                .runtime
                .record_sql_capture(capture(addon_id, "person-2", "Ola"))
                .expect("record second");
            let push = source
                .runtime
                .build_push_payload_for_target(&receiver.runtime.local_node_id, 16)
                .expect("push after new op")
                .expect("new op resets backoff and ships");
            assert_eq!(
                push.operations.len(),
                2,
                "a new op resets backoff and re-ships the un-acked tail too"
            );

            // The receiver applies and ACKs; the source marks both delivered ops
            // acknowledged, draining the outbox.
            let ack = receiver
                .runtime
                .handle_push_payload(&source.runtime.local_node_id, push)
                .expect("handle push");
            source
                .runtime
                .handle_ack_payload(&receiver.runtime.local_node_id, ack)
                .expect("ack");
            assert!(
                source
                    .runtime
                    .ledger
                    .get_outbox_entry(target, first.op_id)
                    .expect("outbox entry")
                    .acknowledged,
                "ACK must drain the backlog"
            );

            // With the backlog acked there is nothing pending: the push is a no-op
            // and the stale deadline is cleared by the empty-outbox branch.
            assert!(
                source
                    .runtime
                    .build_push_payload_for_target(&receiver.runtime.local_node_id, 16)
                    .expect("post-ack push")
                    .is_none(),
                "fully acked target yields no payload"
            );
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
                target_node_id: second_operation.body.actor_node_id.clone(),
                from_node_seq: second_operation.body.node_seq,
                operations: vec![operation_to_wire(&second_operation).expect("wire")],
                serving_floor_node_seq: 1,
                serving_tip_node_seq: second_operation.body.node_seq,
            };

            // A node_seq=2 op with no local seq=1 is a legal catch-up gap: it must
            // queue a per-node repair pull from the missing seq, not error.
            receiver
                .runtime
                .handle_pull_response_payload(&source.runtime.local_node_id, payload)
                .expect("gap queues repair, not an error");

            let pulls = receiver
                .runtime
                .build_repair_pull_payloads_for_peer(&source.runtime.local_node_id, 8, 64)
                .expect("repair pulls");
            assert_eq!(pulls.len(), 1);
            assert_eq!(pulls[0].target_node_id, second_operation.body.actor_node_id);
            assert_eq!(pulls[0].from_node_seq, 1);
        });
    }

    #[test]
    fn out_of_order_core_flow_update_defers_then_converges_after_insert() {
        with_tmp_home(|| {
            let source = make_runtime(81);
            let receiver = make_runtime(82);
            seed_core_authority_target(
                &source.runtime.db,
                "core.flow",
                &receiver.runtime.local_node_id,
            );

            // Node A: one INSERT followed by three UPDATEs of the SAME flow, each
            // with a distinct flow_json and a strictly later HLC (mirrors a user
            // editing a flow several times in the Flow Builder).
            let resource_id = "77";
            let flow_update = |json: &str, hlc: HybridLogicalTimestamp| {
                let mut fields = BTreeMap::new();
                fields.insert(
                    "flow_json".to_string(),
                    FieldValue::String(json.to_string()),
                );
                crate::sync::core_capture::CoreWriteCapture::new(
                    crate::sync::core_registry::CoreSyncResourceKind::Flow,
                    "org-default",
                    resource_id,
                    SqlWriteAction::Update,
                    fields,
                    Some("test-actor".to_string()),
                    hlc,
                    test_epoch(),
                )
            };
            let base = now_ms();
            let hlc_at = |offset: i64| HybridLogicalTimestamp {
                wall_time_ms: base + offset,
                logical: 0,
                node_id: "test-node".to_string(),
            };

            let insert = source
                .runtime
                .record_core_capture(complete_core_flow_capture(resource_id, "Flow 77"))
                .expect("record insert");
            let update_one = source
                .runtime
                .record_core_capture(flow_update(r#"{"nodes":[{"id":"u1"}]}"#, hlc_at(1)))
                .expect("record update 1");
            let update_two = source
                .runtime
                .record_core_capture(flow_update(r#"{"nodes":[{"id":"u2"}]}"#, hlc_at(2)))
                .expect("record update 2");
            let update_three = source
                .runtime
                .record_core_capture(flow_update(r#"{"nodes":[{"id":"u3"}]}"#, hlc_at(3)))
                .expect("record update 3");

            let op = |op_id: OperationId| {
                source
                    .runtime
                    .ledger
                    .get_operation(op_id)
                    .expect("operation")
            };
            let insert_op = op(insert.op_id);
            let update_one_op = op(update_one.op_id);
            let update_two_op = op(update_two.op_id);
            let update_three_op = op(update_three.op_id);
            let peer = PeerId::new(source.runtime.local_node_id.clone()).expect("peer");

            // Node B receives the LAST update first (the INSERT has not arrived).
            // It must be DEFERRED (target row missing is an ordering gap, not a
            // data conflict) so a later drain can still apply it.
            receiver
                .runtime
                .ledger
                .put_verified_in_inbox(
                    peer.clone(),
                    update_three_op.clone(),
                    &HexNodeIdOperationVerifier,
                )
                .expect("inbox update three");
            receiver
                .runtime
                .apply_unapplied_inbox(128)
                .expect("apply deferred update");

            assert!(
                repository::get_flow(&receiver.runtime.db, resource_id)
                    .expect("get flow")
                    .is_none(),
                "flow must not exist before its INSERT arrives"
            );
            let deferred_entry = receiver
                .runtime
                .ledger
                .get_inbox_entry(peer.clone(), update_three_op.op_id)
                .expect("deferred inbox entry");
            assert!(
                !deferred_entry.conflicted,
                "ordering gap must not be a terminal conflict"
            );
            assert!(!deferred_entry.applied);
            assert_eq!(deferred_entry.deferred_count, 1);

            // The INSERT and intermediate UPDATEs now arrive. After the drain the
            // flow must exist and carry the LAST update's json — the deferred
            // entry must have been retried and applied, not left stuck.
            for operation in [&insert_op, &update_one_op, &update_two_op] {
                receiver
                    .runtime
                    .ledger
                    .put_verified_in_inbox(
                        peer.clone(),
                        operation.clone(),
                        &HexNodeIdOperationVerifier,
                    )
                    .expect("inbox operation");
            }
            receiver
                .runtime
                .apply_unapplied_inbox(128)
                .expect("apply convergence");

            let flow = repository::get_flow(&receiver.runtime.db, resource_id)
                .expect("get flow")
                .expect("flow exists after convergence");
            assert_eq!(flow.flow_json, r#"{"nodes":[{"id":"u3"}]}"#);

            let applied_entry = receiver
                .runtime
                .ledger
                .get_inbox_entry(peer, update_three_op.op_id)
                .expect("inbox entry after convergence");
            assert!(applied_entry.applied);
            assert!(!applied_entry.conflicted);
        });
    }

    /// Opens an addon SQLite with a parent/child schema whose child rows carry a
    /// FOREIGN KEY to the parent (FK enforcement is ON for addon DBs), plus a
    /// UNIQUE column on the parent — enough to exercise both an ordering gap
    /// (FK / no-target UPDATE) and a genuine data conflict (UNIQUE).
    fn open_orders_schema(addon_id: &str) {
        let pool = crate::addon::storage_sql::open_addon_db("org-default", addon_id)
            .expect("open addon db");
        let conn = pool.get().expect("addon conn");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS customers (\
                 id INTEGER PRIMARY KEY, \
                 email TEXT NOT NULL UNIQUE);\
             CREATE TABLE IF NOT EXISTS orders (\
                 id INTEGER PRIMARY KEY, \
                 customer_id INTEGER NOT NULL REFERENCES customers(id), \
                 total TEXT NOT NULL);",
        )
        .expect("create orders schema");
    }

    fn customer_insert_capture(addon_id: &str, id: i64, email: &str) -> SqlWriteCapture {
        SqlWriteCapture {
            capture_id: format!("{addon_id}-customer-{id}-{email}"),
            org_id: "org-default".to_string(),
            addon_id: addon_id.to_string(),
            table_name: "customers".to_string(),
            action: SqlWriteAction::Insert,
            resource_type: "customer".to_string(),
            resource_id: id.to_string(),
            query: "INSERT INTO customers (id, email) VALUES (?1, ?2)".to_string(),
            params: vec![JsonValue::from(id), JsonValue::String(email.to_string())],
            rows_affected: 1,
            last_insert_id: id,
            actor_user_id: Some("00000000-0000-0000-0000-000000000007".to_string()),
            created_at_ms: now_ms(),
        }
    }

    fn order_insert_capture(addon_id: &str, id: i64, customer_id: i64) -> SqlWriteCapture {
        SqlWriteCapture {
            capture_id: format!("{addon_id}-order-{id}-{customer_id}"),
            org_id: "org-default".to_string(),
            addon_id: addon_id.to_string(),
            table_name: "orders".to_string(),
            action: SqlWriteAction::Insert,
            resource_type: "order".to_string(),
            resource_id: id.to_string(),
            query: "INSERT INTO orders (id, customer_id, total) VALUES (?1, ?2, ?3)".to_string(),
            params: vec![
                JsonValue::from(id),
                JsonValue::from(customer_id),
                JsonValue::String("100".to_string()),
            ],
            rows_affected: 1,
            last_insert_id: id,
            actor_user_id: Some("00000000-0000-0000-0000-000000000007".to_string()),
            created_at_ms: now_ms(),
        }
    }

    /// Builds a `SyncOperation` from an addon-SQL capture by recording it on
    /// `source` and reading it back out of the ledger — the same shape a peer
    /// would push, so it routes through the addon-SQL inbox-apply branch.
    fn addon_sql_operation(source: &RuntimeHarness, capture: SqlWriteCapture) -> SyncOperation {
        let recorded = source
            .runtime
            .record_sql_capture(capture)
            .expect("record sql capture");
        source
            .runtime
            .ledger
            .get_operation(recorded.op_id)
            .expect("operation")
    }

    #[test]
    fn out_of_order_addon_sql_update_defers_then_applies_after_insert() {
        with_tmp_home(|| {
            let source = make_runtime(83);
            let receiver = make_runtime(84);
            let addon_id = "sync-addon-sql-defer-update";
            for db in [&source.runtime.db, &receiver.runtime.db] {
                seed_authority_target(db, addon_id, &receiver.runtime.local_node_id);
            }
            open_contacts_table(addon_id);

            let insert_op = addon_sql_operation(&source, capture(addon_id, "person-1", "Anna"));
            let update_op =
                addon_sql_operation(&source, update_capture(addon_id, "person-1", "Anna Nowak"));
            let peer = PeerId::new(source.runtime.local_node_id.clone()).expect("peer");

            // The UPDATE arrives before the INSERT that creates its target row.
            // It must be DEFERRED (no target row = ordering gap), never written to
            // the addon conflict table.
            receiver
                .runtime
                .ledger
                .put_verified_in_inbox(peer.clone(), update_op.clone(), &HexNodeIdOperationVerifier)
                .expect("inbox update");
            receiver
                .runtime
                .apply_unapplied_inbox(128)
                .expect("apply deferred update");

            let deferred = receiver
                .runtime
                .ledger
                .get_inbox_entry(peer.clone(), update_op.op_id)
                .expect("deferred entry");
            assert!(!deferred.conflicted, "ordering gap must not be a conflict");
            assert!(!deferred.applied);
            assert_eq!(deferred.deferred_count, 1);
            assert!(
                crate::addon::storage_sql_exec::list_sync_conflicts(
                    "org-default",
                    addon_id,
                    Some("open"),
                    10,
                )
                .expect("conflicts")
                .is_empty(),
                "an ordering deferral must not pollute the addon conflict table"
            );
            let pool = crate::addon::storage_sql::open_addon_db("org-default", addon_id)
                .expect("open addon db");
            assert!(
                pool.get()
                    .expect("conn")
                    .query_row("SELECT name FROM contacts WHERE id = 1", [], |row| row
                        .get::<_, String>(
                        0
                    ))
                    .is_err(),
                "row must not exist before its INSERT arrives"
            );

            // The INSERT now arrives; the drain must retry the deferred UPDATE and
            // converge to the UPDATE's value.
            receiver
                .runtime
                .ledger
                .put_verified_in_inbox(peer.clone(), insert_op, &HexNodeIdOperationVerifier)
                .expect("inbox insert");
            receiver
                .runtime
                .apply_unapplied_inbox(128)
                .expect("apply convergence");

            let name: String = pool
                .get()
                .expect("conn")
                .query_row("SELECT name FROM contacts WHERE id = 1", [], |row| {
                    row.get(0)
                })
                .expect("row exists");
            assert_eq!(name, "Anna Nowak");
            let applied = receiver
                .runtime
                .ledger
                .get_inbox_entry(peer, update_op.op_id)
                .expect("entry after convergence");
            assert!(applied.applied);
            assert!(!applied.conflicted);
        });
    }

    #[test]
    fn out_of_order_addon_sql_fk_insert_defers_then_applies_after_parent() {
        with_tmp_home(|| {
            let source = make_runtime(85);
            let receiver = make_runtime(86);
            let addon_id = "sync-addon-sql-defer-fk";
            for db in [&source.runtime.db, &receiver.runtime.db] {
                seed_authority_target(db, addon_id, &receiver.runtime.local_node_id);
            }
            open_orders_schema(addon_id);

            let customer_op =
                addon_sql_operation(&source, customer_insert_capture(addon_id, 1, "a@x.test"));
            let order_op = addon_sql_operation(&source, order_insert_capture(addon_id, 10, 1));
            let peer = PeerId::new(source.runtime.local_node_id.clone()).expect("peer");

            // The child INSERT (orders → customers FK) arrives before its parent.
            // The FOREIGN KEY violation is an ordering gap, not a data conflict.
            receiver
                .runtime
                .ledger
                .put_verified_in_inbox(peer.clone(), order_op.clone(), &HexNodeIdOperationVerifier)
                .expect("inbox order");
            receiver
                .runtime
                .apply_unapplied_inbox(128)
                .expect("apply deferred order");

            let deferred = receiver
                .runtime
                .ledger
                .get_inbox_entry(peer.clone(), order_op.op_id)
                .expect("deferred entry");
            assert!(
                !deferred.conflicted,
                "FK gap must defer, not become a conflict"
            );
            assert!(!deferred.applied);
            assert_eq!(deferred.deferred_count, 1);
            assert!(
                crate::addon::storage_sql_exec::list_sync_conflicts(
                    "org-default",
                    addon_id,
                    Some("open"),
                    10,
                )
                .expect("conflicts")
                .is_empty(),
                "FK ordering gap must not pollute the conflict table"
            );

            // The parent INSERT arrives; the child must now apply.
            receiver
                .runtime
                .ledger
                .put_verified_in_inbox(peer.clone(), customer_op, &HexNodeIdOperationVerifier)
                .expect("inbox customer");
            receiver
                .runtime
                .apply_unapplied_inbox(128)
                .expect("apply convergence");

            let pool = crate::addon::storage_sql::open_addon_db("org-default", addon_id)
                .expect("open addon db");
            let customer_id: i64 = pool
                .get()
                .expect("conn")
                .query_row("SELECT customer_id FROM orders WHERE id = 10", [], |row| {
                    row.get(0)
                })
                .expect("order row exists");
            assert_eq!(customer_id, 1);
            let applied = receiver
                .runtime
                .ledger
                .get_inbox_entry(peer, order_op.op_id)
                .expect("entry after convergence");
            assert!(applied.applied);
            assert!(!applied.conflicted);
        });
    }

    #[test]
    fn addon_sql_unique_violation_is_terminal_conflict_not_deferred() {
        with_tmp_home(|| {
            let source = make_runtime(87);
            let receiver = make_runtime(88);
            let addon_id = "sync-addon-sql-unique-conflict";
            for db in [&source.runtime.db, &receiver.runtime.db] {
                seed_authority_target(db, addon_id, &receiver.runtime.local_node_id);
            }
            open_orders_schema(addon_id);
            // A local row already owns the UNIQUE email; the replicated INSERT for a
            // DIFFERENT primary key collides on it — a genuine data conflict.
            crate::addon::storage_sql::open_addon_db("org-default", addon_id)
                .expect("open addon db")
                .get()
                .expect("conn")
                .execute(
                    "INSERT INTO customers (id, email) VALUES (1, 'dup@x.test')",
                    [],
                )
                .expect("seed local customer");

            let conflicting =
                addon_sql_operation(&source, customer_insert_capture(addon_id, 2, "dup@x.test"));
            let peer = PeerId::new(source.runtime.local_node_id.clone()).expect("peer");
            receiver
                .runtime
                .ledger
                .put_verified_in_inbox(
                    peer.clone(),
                    conflicting.clone(),
                    &HexNodeIdOperationVerifier,
                )
                .expect("inbox conflicting");
            receiver
                .runtime
                .apply_unapplied_inbox(128)
                .expect("apply conflict");

            let entry = receiver
                .runtime
                .ledger
                .get_inbox_entry(peer, conflicting.op_id)
                .expect("conflict entry");
            assert!(
                entry.conflicted,
                "a UNIQUE violation is a data conflict, must be terminal"
            );
            assert!(!entry.applied);
            assert_eq!(
                entry.deferred_count, 0,
                "a data conflict must not be deferred"
            );
            let conflicts = crate::addon::storage_sql_exec::list_sync_conflicts(
                "org-default",
                addon_id,
                Some("open"),
                10,
            )
            .expect("conflicts");
            assert_eq!(
                conflicts.len(),
                1,
                "conflict must be recorded for the operator"
            );
            assert_eq!(conflicts[0].resource_id, "2");
        });
    }

    #[test]
    fn deferred_addon_sql_update_leaves_no_applied_marker_until_it_lands() {
        with_tmp_home(|| {
            let source = make_runtime(95);
            let receiver = make_runtime(96);
            let addon_id = "sync-addon-sql-defer-marker";
            for db in [&source.runtime.db, &receiver.runtime.db] {
                seed_authority_target(db, addon_id, &receiver.runtime.local_node_id);
            }
            open_contacts_table(addon_id);

            let insert_op = addon_sql_operation(&source, capture(addon_id, "person-1", "Anna"));
            let update_op =
                addon_sql_operation(&source, update_capture(addon_id, "person-1", "Anna Nowak"));
            let peer = PeerId::new(source.runtime.local_node_id.clone()).expect("peer");

            // The UPDATE arrives before its INSERT and is deferred. The idempotency
            // marker MUST have been rolled back: if it stuck, the retried apply
            // would short-circuit on `inserted == 0` and silently skip the write.
            receiver
                .runtime
                .ledger
                .put_verified_in_inbox(peer.clone(), update_op.clone(), &HexNodeIdOperationVerifier)
                .expect("inbox update");
            receiver
                .runtime
                .apply_unapplied_inbox(128)
                .expect("apply deferred update");

            let pool = crate::addon::storage_sql::open_addon_db("org-default", addon_id)
                .expect("open addon db");
            let marker_count: i64 = pool
                .get()
                .expect("conn")
                .query_row(
                    "SELECT COUNT(*) FROM __tentaflow_sync_applied WHERE operation_id = ?1",
                    rusqlite::params![update_op.op_id.to_hex()],
                    |row| row.get(0),
                )
                .expect("marker count");
            assert_eq!(
                marker_count, 0,
                "a deferred apply must roll back its idempotency marker"
            );

            // The INSERT arrives; the retried UPDATE must now actually take effect.
            receiver
                .runtime
                .ledger
                .put_verified_in_inbox(peer.clone(), insert_op, &HexNodeIdOperationVerifier)
                .expect("inbox insert");
            receiver
                .runtime
                .apply_unapplied_inbox(128)
                .expect("apply convergence");

            let name: String = pool
                .get()
                .expect("conn")
                .query_row("SELECT name FROM contacts WHERE id = 1", [], |row| {
                    row.get(0)
                })
                .expect("row exists");
            assert_eq!(name, "Anna Nowak");
        });
    }

    /// Opens an addon SQLite with a self-referential parent chain: each row's
    /// `parent_id` FKs the previous row. Inserting N rows in reverse order forces
    /// the inbox drain through N retry passes — one row becomes applicable per pass
    /// — which is exactly the shape that used to over-count deferrals per pass.
    fn open_chain_schema(addon_id: &str) {
        let pool = crate::addon::storage_sql::open_addon_db("org-default", addon_id)
            .expect("open addon db");
        let conn = pool.get().expect("addon conn");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS chain (\
                 id INTEGER PRIMARY KEY, \
                 parent_id INTEGER REFERENCES chain(id), \
                 label TEXT NOT NULL);",
        )
        .expect("create chain schema");
    }

    fn chain_insert_capture(addon_id: &str, id: i64, parent_id: Option<i64>) -> SqlWriteCapture {
        // resource_id is zero-padded so the partition-id string sort used by
        // `inbox_apply_order` matches numeric order. Each row lives in its own
        // partition (distinct resource_id), so the cross-partition tie-break is the
        // partition-id string — padding keeps that deterministic for the chain.
        SqlWriteCapture {
            capture_id: format!("{addon_id}-chain-{id:04}"),
            org_id: "org-default".to_string(),
            addon_id: addon_id.to_string(),
            table_name: "chain".to_string(),
            action: SqlWriteAction::Insert,
            resource_type: "chain".to_string(),
            resource_id: format!("{id:04}"),
            query: "INSERT INTO chain (id, parent_id, label) VALUES (?1, ?2, ?3)".to_string(),
            params: vec![
                JsonValue::from(id),
                match parent_id {
                    Some(p) => JsonValue::from(p),
                    None => JsonValue::Null,
                },
                JsonValue::String(format!("node-{id}")),
            ],
            rows_affected: 1,
            last_insert_id: id,
            actor_user_id: Some("00000000-0000-0000-0000-000000000007".to_string()),
            created_at_ms: now_ms(),
        }
    }

    #[test]
    fn long_dependency_chain_converges_in_one_drain_without_false_escalation() {
        with_tmp_home(|| {
            let source = make_runtime(97);
            let receiver = make_runtime(98);
            let addon_id = "sync-addon-sql-long-chain";
            for db in [&source.runtime.db, &receiver.runtime.db] {
                seed_authority_target(db, addon_id, &receiver.runtime.local_node_id);
            }
            open_chain_schema(addon_id);

            // A chain longer than MAX_INBOX_DEFER_ATTEMPTS where row k's FK parent is
            // row k+1 (the largest id is the parentless root). `inbox_apply_order`
            // sorts by the zero-padded partition id (= numeric id) ascending, so each
            // pass processes children strictly before their parent — only the current
            // root applies per pass. Convergence therefore needs one pass per row, far
            // more passes than the deferral budget. With the OLD per-pass counter the
            // low-id rows (deepest children) would each accrue ~chain_len deferrals
            // mid-drain, exceed the budget, and land as terminal conflicts even though
            // their prerequisite arrives a few passes later in the SAME drain.
            let chain_len: i64 = (MAX_INBOX_DEFER_ATTEMPTS as i64) + 6;
            let peer = PeerId::new(source.runtime.local_node_id.clone()).expect("peer");
            let mut ops: Vec<SyncOperation> = Vec::new();
            for id in 1..=chain_len {
                let parent = if id == chain_len { None } else { Some(id + 1) };
                ops.push(addon_sql_operation(
                    &source,
                    chain_insert_capture(addon_id, id, parent),
                ));
            }

            for op in &ops {
                receiver
                    .runtime
                    .ledger
                    .put_verified_in_inbox(peer.clone(), op.clone(), &HexNodeIdOperationVerifier)
                    .expect("inbox chain op");
            }

            let applied = receiver
                .runtime
                .apply_unapplied_inbox((chain_len as usize) + 16)
                .expect("apply chain");
            assert_eq!(
                applied, chain_len as usize,
                "every chain op must apply in the single drain"
            );

            let pool = crate::addon::storage_sql::open_addon_db("org-default", addon_id)
                .expect("open addon db");
            let row_count: i64 = pool
                .get()
                .expect("conn")
                .query_row("SELECT COUNT(*) FROM chain", [], |row| row.get(0))
                .expect("chain count");
            assert_eq!(row_count, chain_len, "all chain rows must be present");

            assert!(
                crate::addon::storage_sql_exec::list_sync_conflicts(
                    "org-default",
                    addon_id,
                    Some("open"),
                    (chain_len as usize) + 16,
                )
                .expect("conflicts")
                .is_empty(),
                "a converging chain must produce zero terminal conflicts"
            );

            // No entry may carry more than a single deferral: each was deferred at
            // most once (this one drain) before applying, so the per-drain counter
            // never accumulates across the retry passes.
            for op in &ops {
                let entry = receiver
                    .runtime
                    .ledger
                    .get_inbox_entry(peer.clone(), op.op_id)
                    .expect("chain inbox entry");
                assert!(
                    entry.applied,
                    "chain op {} must be applied",
                    op.op_id.to_hex()
                );
                assert!(!entry.conflicted);
                assert!(
                    entry.deferred_count <= 1,
                    "deferred_count must be <= 1 per drain, was {}",
                    entry.deferred_count
                );
            }
        });
    }

    #[test]
    fn genuinely_orphaned_update_escalates_to_conflict_after_budget_exhausted() {
        with_tmp_home(|| {
            let source = make_runtime(99);
            let mut receiver = make_runtime(100);
            // Shrink the deferral budget for this test only; the prerequisite never
            // arrives, so we must escalate after a bounded number of drains rather
            // than run the full production budget.
            receiver.runtime.max_inbox_defer_attempts = 3;
            let addon_id = "sync-addon-sql-orphan-update";
            for db in [&source.runtime.db, &receiver.runtime.db] {
                seed_authority_target(db, addon_id, &receiver.runtime.local_node_id);
            }
            open_contacts_table(addon_id);

            // An UPDATE whose INSERT will NEVER be delivered: a genuine orphan.
            let update_op =
                addon_sql_operation(&source, update_capture(addon_id, "person-1", "Ghost"));
            let peer = PeerId::new(source.runtime.local_node_id.clone()).expect("peer");
            receiver
                .runtime
                .ledger
                .put_verified_in_inbox(peer.clone(), update_op.clone(), &HexNodeIdOperationVerifier)
                .expect("inbox update");

            // Each drain bumps the deferral counter by exactly one. The entry stays
            // retryable for `budget` drains, then escalates on the next.
            let budget = receiver.runtime.max_inbox_defer_attempts;
            for drain in 1..=budget {
                receiver
                    .runtime
                    .apply_unapplied_inbox(128)
                    .expect("deferring drain");
                let entry = receiver
                    .runtime
                    .ledger
                    .get_inbox_entry(peer.clone(), update_op.op_id)
                    .expect("entry mid-defer");
                assert!(!entry.conflicted, "still within budget at drain {drain}");
                assert_eq!(entry.deferred_count, drain);
            }

            // One more drain exceeds the budget → terminal conflict.
            receiver
                .runtime
                .apply_unapplied_inbox(128)
                .expect("escalating drain");
            let entry = receiver
                .runtime
                .ledger
                .get_inbox_entry(peer, update_op.op_id)
                .expect("entry after escalation");
            assert!(
                entry.conflicted,
                "an orphan must become a terminal conflict once the budget is exhausted"
            );
            assert!(!entry.applied);
            let conflicts = crate::addon::storage_sql_exec::list_sync_conflicts(
                "org-default",
                addon_id,
                Some("open"),
                10,
            )
            .expect("conflicts");
            assert_eq!(
                conflicts.len(),
                1,
                "escalation must record an operator conflict"
            );
            assert_eq!(conflicts[0].resource_id, "person-1");
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
            let gap_payload = MeshSyncPullResponsePayload {
                from_node_id: source.runtime.local_node_id.clone(),
                target_node_id: update_operation.body.actor_node_id.clone(),
                from_node_seq: update_operation.body.node_seq,
                operations: vec![operation_to_wire(&update_operation).expect("wire update")],
                serving_floor_node_seq: 1,
                serving_tip_node_seq: update_operation.body.node_seq,
            };

            receiver
                .runtime
                .handle_pull_response_payload(&source.runtime.local_node_id, gap_payload)
                .expect("missing prefix queues repair, not an error");
            let pulls = receiver
                .runtime
                .build_repair_pull_payloads_for_peer(&source.runtime.local_node_id, 8, 64)
                .expect("repair pulls");
            assert_eq!(pulls.len(), 1);
            assert_eq!(pulls[0].from_node_seq, 1);

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
        let gap_payload = MeshSyncPullResponsePayload {
            from_node_id: source.runtime.local_node_id.clone(),
            target_node_id: update_operation.body.actor_node_id.clone(),
            from_node_seq: update_operation.body.node_seq,
            operations: vec![operation_to_wire(&update_operation).expect("wire update")],
            serving_floor_node_seq: 1,
            serving_tip_node_seq: update_operation.body.node_seq,
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
            .expect("gap queues repair, not an error");
        let repair_pulls = receiver
            .runtime
            .build_repair_pull_payloads_for_peer(&source.runtime.local_node_id, 8, 64)
            .expect("repair pulls");
        assert_eq!(repair_pulls.len(), 1);
        assert_eq!(repair_pulls[0].from_node_seq, 1);
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
        let gap_payload = MeshSyncPullResponsePayload {
            from_node_id: source.runtime.local_node_id.clone(),
            target_node_id: update_operation.body.actor_node_id.clone(),
            from_node_seq: update_operation.body.node_seq,
            operations: vec![operation_to_wire(&update_operation).expect("wire update")],
            serving_floor_node_seq: 1,
            serving_tip_node_seq: update_operation.body.node_seq,
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
            .expect("gap queues repair, not an error");

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
        assert_eq!(received_pull.from_node_seq, 1);
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
            .read()
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
        // A partition snapshot is served through the explicit snapshot-pull path
        // (per-node regular pulls only carry one node's dense chain). Request the
        // compacted prefix as a snapshot package with its tail.
        let pull = source
            .runtime
            .build_snapshot_pull_payload(
                partition.as_str(),
                1,
                snapshot.snapshot_id.as_str(),
                true,
                64,
            )
            .expect("build snapshot pull");
        let pull = MeshSyncSnapshotPullPayload {
            from_node_id: receiver.runtime.local_node_id.clone(),
            ..pull
        };
        let pull_bytes = tentaflow_protocol::cbor::encode(&pull).expect("encode snapshot pull");
        receiver_mesh
            .send_ufp2_to_peer(
                &source.runtime.local_node_id,
                tentaflow_protocol::mesh::MESH_MSG_SYNC_SNAPSHOT_PULL,
                &pull_bytes,
            )
            .await
            .expect("send snapshot pull");

        let received_pull = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match source_events.recv().await {
                    Ok(IrohMeshEvent::SyncSnapshotPullReceived { from_node_id, data })
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
            tentaflow_protocol::mesh::MeshSyncSnapshotPullPayload,
        >(&received_pull)
        .expect("decode snapshot pull");
        let snapshot_response = source
            .runtime
            .handle_snapshot_pull_payload(&receiver.runtime.local_node_id, received_pull)
            .expect("handle snapshot pull");
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
                &source.runtime.local_node_id,
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

            let pull = source
                .runtime
                .build_snapshot_pull_payload(
                    partition.as_str(),
                    1,
                    snapshot.snapshot_id.as_str(),
                    true,
                    64,
                )
                .expect("build snapshot pull");
            let pull = MeshSyncSnapshotPullPayload {
                from_node_id: receiver.runtime.local_node_id.clone(),
                ..pull
            };
            let payload = source
                .runtime
                .handle_snapshot_pull_payload(&receiver.runtime.local_node_id, pull)
                .expect("handle snapshot pull");
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
            .store_incoming_operations(&source.runtime.local_node_id, push.operations, None)
            .expect("store inbox");
        drop(receiver);

        let receiver = make_runtime_from_paths(&receiver_db, &receiver_ledger, 92);
        let applied = receiver.apply_unapplied_inbox(16).expect("apply inbox");
        source
            .runtime
            .handle_ack_payload(&receiver.local_node_id, ack_for(&receiver, operation_ids))
            .expect("ack after restart");
        let conn = receiver.db.read().expect("db");
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
            .store_incoming_operations(
                &source.runtime.local_node_id,
                push.operations[..2].to_vec(),
                None,
            )
            .expect("store first chunks");
        assert_eq!(receiver.apply_unapplied_inbox(16).expect("apply chunks"), 2);
        assert!(blob_chunk_dir(&sha).expect("chunk dir").exists());
        drop(first_ids);
        drop(receiver);

        let receiver = make_runtime_from_paths(&receiver_db, &receiver_ledger, 94);
        let operation_ids = receiver
            .store_incoming_operations(
                &source.runtime.local_node_id,
                push.operations[2..].to_vec(),
                None,
            )
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
        let conn = receiver_c.runtime.db.read().expect("db");
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
        let conn = receiver.runtime.db.read().expect("db");
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
                let conn = node.runtime.db.write().expect("db");
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
                let conn = node.runtime.db.write().expect("db");
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
                let conn = node.runtime.db.read().expect("db");
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
            assert!(
                drained >= 1,
                "the drain must process the pending capture(s)"
            );

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
                let conn = node.runtime.db.read().expect("db");
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
        repository::get_flow(db, id)
            .expect("get flow")
            .map(|f| f.name)
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

            // Identical wall+logical; the tie-break decides on the AUTHORING node
            // id. `record_core_capture` stamps each op's HLC origin with its own
            // runtime id (the real invariant: hlc.node_id == actor_node_id), so the
            // winner is whichever runtime id sorts higher — derive it instead of
            // hard-coding, because the ed25519 key hex ordering is not fixed.
            let id_a = node_a.runtime.local_node_id.clone();
            let id_b = node_b.runtime.local_node_id.clone();
            let (lower_node, higher_node, expected_winner) = if id_a < id_b {
                (&node_a, &node_b, "from-z")
            } else {
                (&node_b, &node_a, "from-z")
            };
            let lower = flow_capture_with_hlc("1", "from-a", hlc_at(5_000, 0, "ignored"));
            let higher = flow_capture_with_hlc("1", expected_winner, hlc_at(5_000, 0, "ignored"));
            let op_lower = signed_flow_op(lower_node, lower);
            let op_higher = signed_flow_op(higher_node, higher);

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

    // =========================================================================
    // Slice-4: api_keys + resource_permissions materializer round-trip
    // =========================================================================

    /// Builds a `core.api_key` Insert capture with an explicit HLC so the test
    /// can order concurrent edits deterministically. Mirrors the replicated field
    /// set the repository emits (no `last_used_at`).
    fn api_key_capture(
        uid: &str,
        verifier: &str,
        is_active: bool,
        hlc: HybridLogicalTimestamp,
    ) -> crate::sync::core_capture::CoreWriteCapture {
        let mut fields = BTreeMap::new();
        fields.insert(
            "key_verifier".to_string(),
            FieldValue::String(verifier.to_string()),
        );
        fields.insert(
            "key_prefix".to_string(),
            FieldValue::String("sk-...x".to_string()),
        );
        fields.insert("name".to_string(), FieldValue::String("k".to_string()));
        fields.insert(
            "key_type".to_string(),
            FieldValue::String("user".to_string()),
        );
        fields.insert(
            "subject_id".to_string(),
            FieldValue::String("user-uid-1".to_string()),
        );
        fields.insert("rate_limit_rps".to_string(), FieldValue::I64(60));
        fields.insert("is_active".to_string(), FieldValue::Bool(is_active));
        crate::sync::core_capture::CoreWriteCapture::new(
            crate::sync::core_registry::CoreSyncResourceKind::ApiKey,
            "org-default",
            uid,
            SqlWriteAction::Insert,
            fields,
            None,
            hlc,
            test_epoch(),
        )
    }

    fn perm_capture(
        action: SqlWriteAction,
        access_level: Option<&str>,
        hlc: HybridLogicalTimestamp,
    ) -> crate::sync::core_capture::CoreWriteCapture {
        let rid =
            crate::sync::resource_id::composite_resource_id(&["model", "gpt-4o", "user", "u1"]);
        let mut fields = BTreeMap::new();
        fields.insert(
            "resource_type".to_string(),
            FieldValue::String("model".to_string()),
        );
        fields.insert(
            "resource_id".to_string(),
            FieldValue::String("gpt-4o".to_string()),
        );
        fields.insert(
            "subject_type".to_string(),
            FieldValue::String("user".to_string()),
        );
        fields.insert(
            "subject_id".to_string(),
            FieldValue::String("u1".to_string()),
        );
        if let Some(level) = access_level {
            fields.insert(
                "access_level".to_string(),
                FieldValue::String(level.to_string()),
            );
        }
        crate::sync::core_capture::CoreWriteCapture::new(
            crate::sync::core_registry::CoreSyncResourceKind::ResourcePermission,
            "org-default",
            rid,
            action,
            fields,
            None,
            hlc,
            test_epoch(),
        )
    }

    fn api_key_active(db: &DbPool, uid: &str) -> Option<bool> {
        use rusqlite::OptionalExtension;
        db.read()
            .expect("db lock")
            .query_row(
                "SELECT is_active FROM api_keys WHERE uid = ?1",
                rusqlite::params![uid],
                |row| row.get::<_, bool>(0),
            )
            .optional()
            .expect("query api key")
    }

    fn perm_level(db: &DbPool) -> Option<String> {
        use rusqlite::OptionalExtension;
        db.read()
            .expect("db lock")
            .query_row(
                "SELECT access_level FROM resource_permissions \
                 WHERE resource_type = 'model' AND resource_id = 'gpt-4o' \
                   AND subject_type = 'user' AND subject_id = 'u1'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .expect("query perm")
    }

    /// An api_key issued on node A replicates onto node B and, with the SAME org
    /// pepper, verifies there. last_used_at on B stays node-local across the apply.
    #[test]
    fn e2e_api_key_replicates_and_verifies_on_peer() {
        with_tmp_home(|| {
            let node_a = make_runtime(81);
            let cipher = make_settings_cipher(81);
            let db_b = make_db();

            // Same pepper on both ends: derive a verifier the way auth does.
            let pepper = b"pepper-shared-org-wide-012345678".to_vec();
            let verifier = crate::api::dashboard::auth::api_key_verifier("sk-secret", &pepper);

            let cap = api_key_capture("uid-1", &verifier, true, hlc_at(1_000, 0, "node-a"));
            let op = signed_flow_op(&node_a, cap);
            let rows = crate::sync::core_materializer::apply_core_operation(&db_b, &cipher, &op)
                .expect("apply api key on B");
            assert_eq!(rows, 1);

            // Set the same pepper on B and verify the replicated key.
            crate::db::repository::set_shared_secret_setting_secure(
                &db_b,
                "api_key_pepper",
                &hex::encode(&pepper),
                &cipher,
                None,
            )
            .expect("seed pepper on B");
            let recovered = crate::db::repository::get_or_create_api_key_pepper(&db_b, &cipher)
                .expect("read pepper on B");
            assert_eq!(
                recovered, pepper,
                "B must use the org pepper, not a fresh one"
            );

            let found = crate::db::repository::verify_api_key(&db_b, &verifier)
                .expect("verify")
                .expect("replicated key verifies on B");
            assert_eq!(found.uid, "uid-1");
        });
    }

    /// last_used_at is node-local: a replicated api_key UPSERT must not reset a
    /// value B already stamped locally.
    #[test]
    fn e2e_api_key_upsert_preserves_local_last_used_at() {
        with_tmp_home(|| {
            let node_a = make_runtime(82);
            let cipher = make_settings_cipher(82);
            let db_b = make_db();

            let op1 = signed_flow_op(
                &node_a,
                api_key_capture("uid-2", "v1", true, hlc_at(1_000, 0, "node-a")),
            );
            crate::sync::core_materializer::apply_core_operation(&db_b, &cipher, &op1)
                .expect("first apply");

            // B stamps last_used_at locally (simulating a verify on B).
            db_b.write()
                .unwrap()
                .execute(
                    "UPDATE api_keys SET last_used_at = '2026-01-01T00:00:00Z' WHERE uid = 'uid-2'",
                    [],
                )
                .unwrap();

            // A newer edit (rotation) arrives from A.
            let op2 = signed_flow_op(
                &node_a,
                api_key_capture("uid-2", "v2", true, hlc_at(2_000, 0, "node-a")),
            );
            crate::sync::core_materializer::apply_core_operation(&db_b, &cipher, &op2)
                .expect("second apply");

            use rusqlite::OptionalExtension;
            let last_used: Option<String> = db_b
                .read()
                .unwrap()
                .query_row(
                    "SELECT last_used_at FROM api_keys WHERE uid = 'uid-2'",
                    [],
                    |r| r.get(0),
                )
                .optional()
                .unwrap();
            assert_eq!(
                last_used.as_deref(),
                Some("2026-01-01T00:00:00Z"),
                "synced UPSERT must not reset node-local last_used_at"
            );
        });
    }

    /// A `clear` (Delete tombstone) authored AFTER an `allow` removes the rule on a
    /// peer and a stale older `allow` arriving later must NOT resurrect it.
    #[test]
    fn e2e_resource_permission_clear_tombstone_beats_stale_allow() {
        with_tmp_home(|| {
            let node_a = make_runtime(83);
            let cipher = make_settings_cipher(83);

            let allow_op = signed_flow_op(
                &node_a,
                perm_capture(
                    SqlWriteAction::Insert,
                    Some("allow"),
                    hlc_at(1_000, 0, "node-a"),
                ),
            );
            let clear_op = signed_flow_op(
                &node_a,
                perm_capture(SqlWriteAction::Delete, None, hlc_at(2_000, 0, "node-a")),
            );

            // Order 1: allow then clear → rule gone.
            let db1 = make_db();
            crate::sync::core_materializer::apply_core_operation(&db1, &cipher, &allow_op)
                .expect("db1 allow");
            assert_eq!(perm_level(&db1).as_deref(), Some("allow"));
            crate::sync::core_materializer::apply_core_operation(&db1, &cipher, &clear_op)
                .expect("db1 clear");
            assert_eq!(perm_level(&db1), None, "clear removes the rule");

            // Order 2: clear (newer) then stale allow (older) → allow is dropped by
            // LWW, the tombstone wins, rule stays absent.
            let db2 = make_db();
            crate::sync::core_materializer::apply_core_operation(&db2, &cipher, &clear_op)
                .expect("db2 clear");
            let stale =
                crate::sync::core_materializer::apply_core_operation(&db2, &cipher, &allow_op)
                    .expect("db2 stale allow");
            assert_eq!(stale, 0, "older allow loses LWW against newer clear");
            assert_eq!(
                perm_level(&db2),
                None,
                "stale allow must not resurrect a cleared rule"
            );
        });
    }

    /// Set/active state is concurrently editable: the higher-HLC active flag wins
    /// regardless of apply order (mirrors the flow LWW guarantee).
    #[test]
    fn e2e_api_key_active_flag_is_lww() {
        with_tmp_home(|| {
            let node_a = make_runtime(84);
            let node_b = make_runtime(85);
            let cipher = make_settings_cipher(84);

            let older = signed_flow_op(
                &node_a,
                api_key_capture("uid-3", "v", true, hlc_at(1_000, 0, "node-a")),
            );
            let newer = signed_flow_op(
                &node_b,
                api_key_capture("uid-3", "v", false, hlc_at(2_000, 0, "node-b")),
            );

            let db1 = make_db();
            crate::sync::core_materializer::apply_core_operation(&db1, &cipher, &older).unwrap();
            crate::sync::core_materializer::apply_core_operation(&db1, &cipher, &newer).unwrap();

            let db2 = make_db();
            crate::sync::core_materializer::apply_core_operation(&db2, &cipher, &newer).unwrap();
            let stale = crate::sync::core_materializer::apply_core_operation(&db2, &cipher, &older)
                .unwrap();
            assert_eq!(stale, 0, "older active=true dropped by LWW");

            assert_eq!(api_key_active(&db1, "uid-3"), Some(false));
            assert_eq!(api_key_active(&db2, "uid-3"), Some(false));
        });
    }

    /// Records an `addon.kv` write on `node` with a caller-chosen wall time, so the
    /// resulting signed op carries a deterministic HLC (wall = `created_at_ms`,
    /// node = the runtime's own id) for the LWW comparison.
    fn signed_kv_op(
        node: &RuntimeHarness,
        addon_id: &str,
        instance_id: &str,
        key: &str,
        value: Option<&[u8]>,
        created_at_ms: i64,
    ) -> SyncOperation {
        let mut capture = KvWriteCapture::new(
            "org-default",
            addon_id,
            instance_id,
            key,
            value.map(|v| v.to_vec()),
            Some("00000000-0000-0000-0000-000000000007".to_string()),
        );
        capture.created_at_ms = created_at_ms;
        let recorded = node
            .runtime
            .record_kv_capture(capture)
            .expect("record kv capture");
        node.runtime
            .ledger
            .get_operation(recorded.op_id)
            .expect("operation")
    }

    fn kv_value(db: &DbPool, addon_id: &str, instance_id: &str, key: &str) -> Option<Vec<u8>> {
        use rusqlite::OptionalExtension;
        db.read()
            .expect("db lock")
            .query_row(
                "SELECT storage_value FROM addon_storage \
                 WHERE addon_id = ?1 AND instance_id = ?2 AND storage_key = ?3",
                rusqlite::params![addon_id, instance_id, key],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .expect("query kv")
    }

    /// Cross-node LWW for replicated `addon.kv`: the SAME key is written
    /// concurrently on two nodes with different HLCs. Applying both ops in OPPOSITE
    /// orders on two separate databases must converge to the newer-HLC value —
    /// proving the winner is the higher HLC, not whichever push arrived last. This
    /// is the convergence hole the blind upsert left open. Re-delivering either op
    /// is a no-op, so the merge is idempotent (re-delivery), commutative and
    /// associative (apply order does not change the fixed point): every db reaches
    /// the same `max` over the total HLC order.
    #[test]
    fn e2e_cross_node_kv_lww_converges_to_newer_hlc_regardless_of_apply_order() {
        with_tmp_home(|| {
            let node_a = make_runtime(141);
            let node_b = make_runtime(142);

            // Two concurrent writes of the same addon key: A older, B newer.
            let op_older = signed_kv_op(
                &node_a,
                "kv-addon",
                "inst-1",
                "settings/theme",
                Some(b"light"),
                1_000,
            );
            let op_newer = signed_kv_op(
                &node_b,
                "kv-addon",
                "inst-1",
                "settings/theme",
                Some(b"dark"),
                2_000,
            );

            let db1 = make_db();
            let db2 = make_db();

            // DB 1 applies older THEN newer; DB 2 applies newer THEN older.
            apply_kv_operation(&db1, &op_older).expect("db1 older");
            apply_kv_operation(&db1, &op_newer).expect("db1 newer");

            apply_kv_operation(&db2, &op_newer).expect("db2 newer");
            // The older op arriving last must be DROPPED by the LWW gate (0 rows),
            // not clobber the newer value.
            let stale = apply_kv_operation(&db2, &op_older).expect("db2 older");
            assert_eq!(stale, 0, "stale older kv op must be dropped by LWW");

            // Both databases converge to the newer-HLC value.
            assert_eq!(
                kv_value(&db1, "kv-addon", "inst-1", "settings/theme").as_deref(),
                Some(&b"dark"[..])
            );
            assert_eq!(
                kv_value(&db2, "kv-addon", "inst-1", "settings/theme").as_deref(),
                Some(&b"dark"[..])
            );

            // Re-delivering the winning op is idempotent: same HLC fails the strict
            // `>` gate and applies nothing, value unchanged.
            let redelivered = apply_kv_operation(&db1, &op_newer).expect("db1 re-deliver");
            assert_eq!(redelivered, 0, "re-delivered op must be a no-op");
            assert_eq!(
                kv_value(&db1, "kv-addon", "inst-1", "settings/theme").as_deref(),
                Some(&b"dark"[..])
            );
        });
    }

    /// LWW tombstone for `addon.kv` delete: a delete carrying a HIGHER HLC must win
    /// over a later-DELIVERED-but-older insert of the same key. The version row
    /// survives the delete (tombstone), so the stale insert arriving afterwards is
    /// dropped and the key stays absent on every node regardless of delivery order.
    #[test]
    fn e2e_cross_node_kv_delete_tombstone_beats_late_older_insert() {
        with_tmp_home(|| {
            let node_a = make_runtime(143);
            let node_b = make_runtime(144);

            // Older insert on A, newer delete on B of the same key.
            let op_insert = signed_kv_op(
                &node_a,
                "kv-addon",
                "inst-1",
                "settings/theme",
                Some(b"light"),
                1_000,
            );
            let op_delete =
                signed_kv_op(&node_b, "kv-addon", "inst-1", "settings/theme", None, 2_000);

            let db1 = make_db();
            let db2 = make_db();

            // DB 1: insert THEN delete (delete wins, key removed).
            apply_kv_operation(&db1, &op_insert).expect("db1 insert");
            apply_kv_operation(&db1, &op_delete).expect("db1 delete");
            assert_eq!(kv_value(&db1, "kv-addon", "inst-1", "settings/theme"), None);

            // DB 2: delete THEN late older insert. The tombstone's HLC outranks the
            // insert, so the late insert is dropped and the key stays absent.
            apply_kv_operation(&db2, &op_delete).expect("db2 delete");
            let stale = apply_kv_operation(&db2, &op_insert).expect("db2 late insert");
            assert_eq!(
                stale, 0,
                "older insert after a newer delete must be dropped"
            );
            assert_eq!(kv_value(&db2, "kv-addon", "inst-1", "settings/theme"), None);
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
            assert_eq!(
                flow_name(&receiver.runtime.db, "1").as_deref(),
                Some("Pushed Flow")
            );
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

            assert!(
                permitted_push.is_some(),
                "permitted node must receive the push"
            );
            assert!(
                denied_push.is_none(),
                "denied node must NOT receive any push"
            );
            assert_eq!(
                recorded.queued_targets, 1,
                "exactly one permitted target queued"
            );
        });
    }

    /// Epoch fencing after baseline adopt: an operation minted under the OLD
    /// (pre-adopt) epoch is dropped by the receiver once the receiver has adopted a
    /// newer (canonically-winning) epoch. The push is admitted without error (the
    /// stale op is silently skipped, not a hard failure that spins), nothing
    /// materializes, and — because the LOCAL epoch wins — no baseline adopt is
    /// requested (we never adopt a losing baseline). Stale-epoch writes from before
    /// the merge cannot leak into the merged organization.
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
            assert_eq!(
                stale_op.body.epoch.counter, 0,
                "op stamped under genesis epoch"
            );

            // Receiver adopts a NEWER baseline epoch (as it would after pairing).
            let new_epoch = BaselineEpoch {
                counter: 7,
                origin_node: receiver.runtime.local_node_id.clone(),
            };
            receiver
                .runtime
                .ledger
                .set_epoch(new_epoch.clone())
                .expect("set epoch");

            // The push carries the stale-epoch op; the receiver's higher epoch wins,
            // so the op is dropped during admission. The push itself succeeds (no
            // hard error / spin) and reports no accepted op.
            let push = source
                .runtime
                .build_push_payload_for_target(&receiver.runtime.local_node_id, 16)
                .expect("build push")
                .expect("pending push");
            let ack = receiver
                .runtime
                .handle_push_payload(&source.runtime.local_node_id, push)
                .expect("stale-epoch push is absorbed, not errored");
            assert!(
                ack.operation_ids.is_empty(),
                "stale-epoch op must not be accepted"
            );

            // The local epoch wins, so no baseline adopt is requested (we never
            // adopt a losing baseline; the peer will adopt ours instead).
            assert!(
                receiver.runtime.take_pending_reconcile().is_empty(),
                "winning local epoch must not request an adopt"
            );

            // Nothing materialized: the receiver has no such flow row.
            assert!(
                repository::get_flow(&receiver.runtime.db, "1")
                    .expect("get flow")
                    .is_none(),
                "stale-epoch op must not materialize after fencing"
            );
        });
    }

    // =========================================================================
    // Convergence suite: per-node hash chains + HLC-LWW, conflict-free multi-writer.
    //
    // These tests are the correctness proof of the redesign. They drive several
    // independent `SyncRuntime` instances (each its own ledger tempdir, db, HLC,
    // and ed25519 node identity) through the REAL admission + materialization
    // path and assert that, once every operation has reached every node in some
    // order, all nodes converge: identical materialized SQLite state, identical
    // ledger op-sets, and identical per-node frontiers.
    //
    // Determinism: a seeded SplitMix64 PRNG drives every choice (which node
    // writes, which key, which value, the delivery permutation, the partition
    // heal). HLC stamps are minted from a per-test logical counter so the LWW
    // winner is fully determined by the seed — no wall-clock, no thread RNG.
    // =========================================================================

    /// Seeded SplitMix64 — a tiny, well-distributed deterministic PRNG. Used so
    /// every randomized choice in the convergence suite is reproducible from its
    /// seed alone (no `rand`, no wall-clock, no `thread_rng`).
    struct SplitMix64 {
        state: u64,
    }

    impl SplitMix64 {
        fn new(seed: u64) -> Self {
            Self { state: seed }
        }

        fn next_u64(&mut self) -> u64 {
            self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        fn below(&mut self, bound: usize) -> usize {
            (self.next_u64() % bound as u64) as usize
        }

        /// Fisher–Yates shuffle driven entirely by this seeded PRNG.
        fn shuffle<T>(&mut self, items: &mut [T]) {
            for i in (1..items.len()).rev() {
                let j = self.below(i + 1);
                items.swap(i, j);
            }
        }
    }

    /// A single convergence node: an independent runtime plus the tempdirs that
    /// must outlive it (Fjall ledger on disk).
    struct ConvergenceNode {
        runtime: SyncRuntime,
        _ledger_dir: tempfile::TempDir,
    }

    impl ConvergenceNode {
        fn node_id(&self) -> &str {
            &self.runtime.local_node_id
        }
    }

    /// Builds an isolated node with its own on-disk Fjall ledger and in-memory db.
    /// `key_seed` must be unique per node so each gets a distinct ed25519 identity
    /// (the node_id is the hex public key, which also keys its hash chain).
    fn make_convergence_node(key_seed: u8) -> ConvergenceNode {
        let ledger_dir = tempfile::tempdir().expect("ledger dir");
        let db = make_db();
        let security = make_security(db.clone(), key_seed);
        let local_node_id = security.ed25519_public_key_hex();
        let ledger = Arc::new(FjallSyncLedgerStore::open(ledger_dir.path()).expect("ledger"));
        let hlc = HlcClock::new(local_node_id.clone(), None);
        ConvergenceNode {
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
                addon_reconciler: parking_lot::RwLock::new(None),
                max_inbox_defer_attempts: MAX_INBOX_DEFER_ATTEMPTS,
                adopt_inflight: parking_lot::Mutex::new(HashSet::new()),
                pending_epoch_reconcile: parking_lot::Mutex::new(Vec::new()),
                floor_anchor_warned: parking_lot::Mutex::new(HashSet::new()),
                push_state: PushState::default(),
            },
            _ledger_dir: ledger_dir,
        }
    }

    /// Lets `node` accept incoming operations for `resource_type` by listing
    /// itself as a sync target. Admission gates on `local_target_allowed`, so
    /// without this a delivered full op is retained only as a redacted chain
    /// placeholder and never materializes.
    fn allow_self_target(node: &ConvergenceNode, resource_type: &str) {
        seed_core_authority_target(&node.runtime.db, resource_type, node.node_id());
    }

    /// An Organization INSERT capture carrying `name`. Organization is LWW-tracked
    /// and materializes to `organizations.name`, which is the value the suite reads
    /// back to check cross-node convergence.
    fn org_capture(
        org_id: &str,
        name: &str,
        hlc: HybridLogicalTimestamp,
    ) -> crate::sync::core_capture::CoreWriteCapture {
        let mut fields = BTreeMap::new();
        fields.insert("name".to_string(), FieldValue::String(name.to_string()));
        fields.insert(
            "slug".to_string(),
            FieldValue::String(format!("slug-{org_id}")),
        );
        crate::sync::core_capture::CoreWriteCapture::new(
            crate::sync::core_registry::CoreSyncResourceKind::Organization,
            "org-default",
            org_id,
            SqlWriteAction::Insert,
            fields,
            None,
            hlc,
            test_epoch(),
        )
    }

    /// Reads the materialized organization name on a node (`None` if absent). The
    /// per-node convergence oracle: after every op reaches every node, this value
    /// must be identical everywhere and equal the highest-HLC writer's name.
    fn read_org_name(node: &ConvergenceNode, org_id: &str) -> Option<String> {
        use rusqlite::OptionalExtension;
        let conn = node.runtime.db.read().expect("db lock");
        conn.query_row(
            "SELECT name FROM organizations WHERE org_id = ?1",
            rusqlite::params![org_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .expect("query org name")
    }

    /// Whether `node` holds `op_id` authored by `author`: a locally-authored op
    /// lives in the node-log/operations index; a received op lives in the inbox
    /// keyed by the delivering peer (the author here). Either presence counts —
    /// these are the two authoritative records of an operation on a node.
    fn node_holds_op(node: &ConvergenceNode, author: &str, op_id: OperationId) -> bool {
        if node.runtime.ledger.get_operation(op_id).is_ok() {
            return true;
        }
        let Ok(peer) = PeerId::new(author.to_string()) else {
            return false;
        };
        node.runtime.ledger.get_inbox_entry(peer, op_id).is_ok()
    }

    /// Counts how many ops from `expected` (the full authored set, as
    /// `(author, op_id)`) are present on `node`. The convergence target is that
    /// every node holds the entire set.
    fn present_op_count(node: &ConvergenceNode, expected: &[(String, OperationId)]) -> usize {
        expected
            .iter()
            .filter(|(author, op_id)| node_holds_op(node, author, *op_id))
            .count()
    }

    /// The node-frontier as a sorted `(author, last_seq, last_hash)` vector — must
    /// match across all nodes once they have seen the same chains.
    fn frontier_snapshot(
        node: &ConvergenceNode,
        author_node_ids: &[String],
    ) -> Vec<(String, u64, [u8; 32])> {
        let mut frontier = Vec::new();
        for author in author_node_ids {
            if let Some(entry) = node
                .runtime
                .ledger
                .get_node_frontier(author)
                .expect("node frontier")
            {
                frontier.push((entry.node_id, entry.last_seq, entry.last_hash));
            }
        }
        frontier.sort();
        frontier
    }

    /// Counts every queued repair request across all peers — used to prove the
    /// repair queue does not grow without bound under redelivery / legal
    /// concurrent writes.
    fn total_repair_queue_len(node: &ConvergenceNode, peer_ids: &[String]) -> usize {
        let mut total = 0;
        for peer in peer_ids {
            total += node
                .runtime
                .ledger
                .list_due_repair_requests(PeerId::new(peer.clone()).expect("peer"), i64::MAX, 1024)
                .expect("repair requests")
                .len();
        }
        total
    }

    /// Delivers `operations` to `target` exactly as the mesh push path would:
    /// wraps each op on the wire, admits it (per-node chain admission), then drains
    /// the inbox (materialization + LWW). This is the REAL admission/apply code —
    /// not a shortcut — so the test exercises gap/idempotent/equivocation handling.
    fn deliver(
        target: &ConvergenceNode,
        source_node_id: &str,
        operations: &[SyncOperation],
    ) -> LedgerResult<()> {
        let wire = operations
            .iter()
            .map(operation_to_wire)
            .collect::<LedgerResult<Vec<_>>>()?;
        target
            .runtime
            .store_incoming_operations(source_node_id, wire, None)?;
        target.runtime.apply_unapplied_inbox(128)?;
        Ok(())
    }

    /// One authored operation plus the id of the node that authored it (the
    /// "in-flight" unit shuffled and replayed to every node).
    #[derive(Clone)]
    struct InFlightOp {
        author_node_id: String,
        operation: SyncOperation,
    }

    /// Drives one full convergence scenario for a given seed and returns nothing —
    /// it asserts internally. N nodes each author M Organization writes against a
    /// small key pool with seeded HLCs; the whole op-stream is delivered to every
    /// node in a seeded permutation, with a simulated partition (a slice of
    /// deliveries deferred, then healed). After healing, all nodes must converge.
    fn run_convergence_scenario(seed: u64) {
        let node_count = 3 + (seed as usize % 2); // 3 or 4 nodes
        let writes_per_node = 6;
        let key_pool = ["org-alpha", "org-beta", "org-gamma"];

        let mut nodes: Vec<ConvergenceNode> = (0..node_count)
            .map(|i| make_convergence_node(40u8.wrapping_add(i as u8)))
            .collect();
        let node_ids: Vec<String> = nodes.iter().map(|n| n.node_id().to_string()).collect();
        for node in &nodes {
            allow_self_target(node, "core.organization");
        }

        let mut rng = SplitMix64::new(seed);

        // Each authored write gets a unique, strictly-increasing HLC wall time so
        // the global LWW order is a total order the test can compute exactly. The
        // authoring node mints the op through `record_core_capture`, which appends
        // to its own per-node chain (node_seq) and signs it.
        let mut in_flight: Vec<InFlightOp> = Vec::new();
        // Expected per-key winner: (hlc, name). Highest HLC wins, ties broken by
        // node_id exactly like `HybridLogicalTimestamp::Ord`.
        let mut expected: std::collections::HashMap<String, (HybridLogicalTimestamp, String)> =
            std::collections::HashMap::new();
        let mut wall: i64 = 1_000;

        for round in 0..writes_per_node {
            for node_idx in 0..node_count {
                let author_id = node_ids[node_idx].clone();
                let key = key_pool[rng.below(key_pool.len())].to_string();
                let value = format!("v-{author_id:.6}-{round}-{}", rng.next_u64());
                wall += 1 + rng.below(3) as i64;
                let hlc = hlc_at(wall, rng.below(4) as u32, &author_id);

                let capture = org_capture(&key, &value, hlc.clone());
                let result = nodes[node_idx]
                    .runtime
                    .record_core_capture(capture)
                    .expect("author op");
                let operation = nodes[node_idx]
                    .runtime
                    .ledger
                    .get_operation(result.op_id)
                    .expect("authored operation");

                // The author has already materialized its own write locally.
                let entry = expected.entry(key).or_insert((hlc.clone(), value.clone()));
                if hlc > entry.0 {
                    *entry = (hlc.clone(), value.clone());
                }
                in_flight.push(InFlightOp {
                    author_node_id: author_id,
                    operation,
                });
            }
        }

        // Deliver every op to every node in a seeded permutation. A leading slice
        // of deliveries is held back to model a network partition; the rest are
        // delivered first, then the partition "heals" and the held-back slice is
        // delivered. Convergence must hold regardless of this split.
        for target_idx in 0..node_count {
            let target_id = node_ids[target_idx].clone();
            let mut plan: Vec<InFlightOp> = in_flight
                .iter()
                .filter(|op| op.author_node_id != target_id)
                .cloned()
                .collect();
            rng.shuffle(&mut plan);

            let partition_at = if plan.is_empty() {
                0
            } else {
                rng.below(plan.len())
            };
            let (first_wave, healed_wave) = plan.split_at(partition_at);

            // First wave: many ops arrive out of order; gaps queue catch-up pulls
            // but never corrupt state.
            for op in first_wave {
                let _ = deliver(
                    &nodes[target_idx],
                    &op.author_node_id,
                    std::slice::from_ref(&op.operation),
                );
            }
            // Heal: deliver the rest. Per-node chains are dense, so once a node's
            // gap is filled, every deferred op of that chain applies.
            for op in healed_wave {
                let _ = deliver(
                    &nodes[target_idx],
                    &op.author_node_id,
                    std::slice::from_ref(&op.operation),
                );
            }
        }

        // Snapshot every author's full, ordered chain (authored ops live in the
        // author's local node-log). The author does NOT materialize its own writes
        // by authoring alone — materialization only happens on the inbound apply
        // path — so each chain is delivered to EVERY node, the author included.
        let chains: Vec<(String, Vec<SyncOperation>)> = node_ids
            .iter()
            .map(|author| {
                let chain = nodes
                    .iter()
                    .find(|n| n.node_id() == author)
                    .expect("author node")
                    .runtime
                    .ledger
                    .get_node_operations(NodeLogQuery {
                        node_id: author.clone(),
                        from_node_seq: Some(1),
                        to_node_seq: None,
                        limit: None,
                    })
                    .expect("author chain");
                (author.clone(), chain)
            })
            .collect();

        // Catch-up: deliver every full chain in order to every node. This models
        // the repair-pull the real mesh performs for queued gaps and is what makes
        // every node materialize the entire op-set under HLC-LWW.
        for target_idx in 0..node_count {
            for (author, chain) in &chains {
                deliver(&nodes[target_idx], author, chain).expect("deliver full chain");
            }
        }

        // The full authored set as (author, op_id) — the convergence target.
        let authored: Vec<(String, OperationId)> = in_flight
            .iter()
            .map(|op| (op.author_node_id.clone(), op.operation.op_id))
            .collect();

        // ---- Convergence assertions ----

        // (i) Materialized state identical on every node, and equal to the
        // independently-computed LWW winner per key.
        for key in key_pool {
            let expected_name = expected.get(key).map(|(_, name)| name.clone());
            let names: Vec<Option<String>> = nodes.iter().map(|n| read_org_name(n, key)).collect();
            for (idx, name) in names.iter().enumerate() {
                assert_eq!(
                    name, &expected_name,
                    "seed {seed}: node {idx} materialized {name:?} for {key}, expected LWW winner {expected_name:?}"
                );
            }
        }

        // (ii) Ledger op-set identical on every node: every node holds the FULL
        // authored set (own node-log for self-authored ops, inbox for received).
        let total_ops = node_count * writes_per_node;
        for (idx, node) in nodes.iter().enumerate() {
            assert_eq!(
                present_op_count(node, &authored),
                total_ops,
                "seed {seed}: node {idx} is missing operations from the authored set"
            );
        }

        // (iii) Node-frontier identical on every node.
        let frontiers: Vec<_> = nodes
            .iter()
            .map(|n| frontier_snapshot(n, &node_ids))
            .collect();
        for (idx, frontier) in frontiers.iter().enumerate() {
            assert_eq!(
                frontier, &frontiers[0],
                "seed {seed}: node {idx} frontier diverged"
            );
        }

        // Make tempdirs explicitly live until the end (defensive against early drop).
        drop(nodes);
    }

    #[test]
    fn convergence_property_across_seeds() {
        with_tmp_home(|| {
            // 24 distinct seeds (> the required K>=20). Each is an independent
            // multi-node scenario with its own seeded write-stream, HLCs and
            // delivery permutation. All must converge.
            for seed in 0..24u64 {
                run_convergence_scenario(seed.wrapping_mul(0x1234_5678_9ABC).wrapping_add(7));
            }
        });
    }

    #[test]
    fn convergence_idempotent_under_repeated_delivery() {
        with_tmp_home(|| {
            let author = make_convergence_node(70);
            let target = make_convergence_node(71);
            allow_self_target(&target, "core.organization");

            // Author two writes to two keys; the second key gets a higher HLC.
            let op_a = {
                let cap = org_capture("org-alpha", "alpha-1", hlc_at(2_000, 0, author.node_id()));
                let result = author.runtime.record_core_capture(cap).expect("author a");
                author
                    .runtime
                    .ledger
                    .get_operation(result.op_id)
                    .expect("op a")
            };
            let op_b = {
                let cap = org_capture("org-beta", "beta-1", hlc_at(2_001, 0, author.node_id()));
                let result = author.runtime.record_core_capture(cap).expect("author b");
                author
                    .runtime
                    .ledger
                    .get_operation(result.op_id)
                    .expect("op b")
            };
            let chain = vec![op_a, op_b];
            let author_id = author.node_id().to_string();

            let authored: Vec<(String, OperationId)> = chain
                .iter()
                .map(|op| (author_id.clone(), op.op_id))
                .collect();

            // First delivery: full chain in order.
            deliver(&target, &author_id, &chain).expect("first delivery");
            let frontier_once = frontier_snapshot(&target, &[author_id.clone()]);
            let names_once = (
                read_org_name(&target, "org-alpha"),
                read_org_name(&target, "org-beta"),
            );
            assert_eq!(
                present_op_count(&target, &authored),
                2,
                "both ops admitted on first delivery"
            );

            // Deliver the SAME chain two more times. Every op is now below the
            // frontier → AlreadyKnown → idempotent ACK, no state change, and the
            // repair queue must not grow (no gaps were ever introduced).
            deliver(&target, &author_id, &chain).expect("second delivery");
            deliver(&target, &author_id, &chain).expect("third delivery");

            assert_eq!(
                frontier_snapshot(&target, &[author_id.clone()]),
                frontier_once,
                "frontier changed under redelivery"
            );
            assert_eq!(
                present_op_count(&target, &authored),
                2,
                "ledger op-set changed under redelivery"
            );
            assert_eq!(
                (
                    read_org_name(&target, "org-alpha"),
                    read_org_name(&target, "org-beta"),
                ),
                names_once,
                "materialized state changed under redelivery"
            );
            assert_eq!(
                total_repair_queue_len(&target, &[author_id]),
                0,
                "in-order full-chain redelivery must never queue a repair"
            );
        });
    }

    #[test]
    fn convergence_concurrent_external_credentials_no_equivocation() {
        // Direct regression for the external-credentials seq-7 bug: two nodes each
        // independently write `hf_token` (different values, different HLCs) into the
        // shared-secret partition, then exchange. Because each write lives in its
        // OWN per-node chain, these are LEGAL concurrent writes — NOT a fork of one
        // node's chain — so admission must NOT raise HashChainMismatch/equivocation,
        // the repair queue must stay bounded, and both nodes must converge on the
        // value with the higher HLC.
        with_tmp_home(|| {
            let node_a = make_convergence_node(80);
            let node_b = make_convergence_node(81);
            allow_self_target(&node_a, "core.shared_setting_secret");
            allow_self_target(&node_b, "core.shared_setting_secret");
            let a_id = node_a.node_id().to_string();
            let b_id = node_b.node_id().to_string();

            // Node A writes hf_token first (lower HLC); node B writes a different
            // value with a strictly higher HLC — B must win the LWW everywhere.
            // Wall times are anchored to `now_ms()` so they exceed any baseline a
            // local write transaction would stamp, matching production scale.
            let base = now_ms();
            let op_a =
                author_shared_secret(&node_a, "hf_token", "hf_from_a", hlc_at(base, 0, &a_id));
            let op_b =
                author_shared_secret(&node_b, "hf_token", "hf_from_b", hlc_at(base + 1, 0, &b_id));

            // Exchange both directions. Delivering a legal concurrent write must
            // succeed — these calls would Err with HashChainMismatch if the old
            // single-partition-chain model were still in force.
            deliver(&node_a, &b_id, std::slice::from_ref(&op_b)).expect("B->A must not equivocate");
            deliver(&node_b, &a_id, std::slice::from_ref(&op_a)).expect("A->B must not equivocate");

            // Re-exchange (redelivery) to confirm the repair queue does not grow.
            deliver(&node_a, &b_id, std::slice::from_ref(&op_b)).expect("B->A redeliver");
            deliver(&node_b, &a_id, std::slice::from_ref(&op_a)).expect("A->B redeliver");

            let value_a = repository::get_setting_secure(
                &node_a.runtime.db,
                "hf_token",
                &node_a.runtime.settings_cipher,
            )
            .expect("read secret on A");
            let value_b = repository::get_setting_secure(
                &node_b.runtime.db,
                "hf_token",
                &node_b.runtime.settings_cipher,
            )
            .expect("read secret on B");

            assert_eq!(
                value_a.as_deref(),
                Some("hf_from_b"),
                "node A must converge on the higher-HLC value"
            );
            assert_eq!(
                value_b.as_deref(),
                Some("hf_from_b"),
                "node B must converge on the higher-HLC value"
            );

            // Both ledgers hold both ops (own chain for the local write, inbox for
            // the received concurrent one).
            let both_ops = [(a_id.clone(), op_a.op_id), (b_id.clone(), op_b.op_id)];
            assert_eq!(
                present_op_count(&node_a, &both_ops),
                2,
                "node A must hold both concurrent ops"
            );
            assert_eq!(
                present_op_count(&node_b, &both_ops),
                2,
                "node B must hold both concurrent ops"
            );

            // Repair queue stays bounded (these were never gaps).
            assert_eq!(
                total_repair_queue_len(&node_a, &[b_id.clone()]),
                0,
                "legal concurrent write must not queue a repair on A"
            );
            assert_eq!(
                total_repair_queue_len(&node_b, &[a_id]),
                0,
                "legal concurrent write must not queue a repair on B"
            );
        });
    }

    /// Forges an equivocating operation: a SECOND, distinct op that the same
    /// author signs at an ALREADY-USED `(node, node_seq)`. We start from a real op
    /// the author minted, change one content field (so the canonical hash, and thus
    /// the op_id, differ), recompute hash/op_id, and re-sign with the author's
    /// signer. This is exactly the Byzantine fork admission must reject below the
    /// frontier — and the bug it guards: without foreign ops on the per-node axis,
    /// the lookup at that seq was always None and the fork slipped through as
    /// AlreadyKnown.
    fn forge_equivocation(
        author: &ConvergenceNode,
        original: &SyncOperation,
        new_value: &str,
    ) -> SyncOperation {
        let mut body = original.body.clone();
        body.changed_fields.insert(
            "name".to_string(),
            FieldValue::String(new_value.to_string()),
        );
        let operation_hash = crate::sync::ledger::hash_canonical(&body).expect("hash forged body");
        let op_id = OperationId::from_hash(operation_hash);
        let mut forged = SyncOperation {
            op_id,
            operation_hash,
            body,
            signature: Vec::new(),
        };
        forged.signature = author
            .runtime
            .signer
            .sign_operation(&forged.signing_bytes())
            .expect("sign forged op");
        forged
            .validate_integrity()
            .expect("forged op self-consistent");
        assert_ne!(
            forged.op_id, original.op_id,
            "forged op must differ from the original at the same (node, seq)"
        );
        forged
    }

    #[test]
    fn equivocation_below_frontier_foreign_node_rejected() {
        // Direct regression for the foreign-axis equivocation hole: an equivocating
        // op (same FOREIGN author + node_seq, different body) delivered BELOW the
        // frontier must be rejected as NodeEquivocation, not silently accepted as
        // AlreadyKnown. Pre-fix `get_node_log_entry(foreign, seq)` was always None
        // (foreign ops never reached node_log), so the fork was swallowed.
        with_tmp_home(|| {
            let node_a = make_convergence_node(82); // observer
            let node_b = make_convergence_node(83); // foreign author
            allow_self_target(&node_a, "core.organization");
            allow_self_target(&node_b, "core.organization");
            let b_id = node_b.node_id().to_string();

            // Author B mints a dense chain of 5 ops (node_seq 1..=5) and ships them
            // to A, which advances B's frontier to 5.
            let base = now_ms();
            let mut chain: Vec<SyncOperation> = Vec::new();
            for seq in 1..=5u64 {
                let capture = org_capture(
                    "org-equiv",
                    &format!("b-real-{seq}"),
                    hlc_at(base + seq as i64, 0, &b_id),
                );
                let result = node_b
                    .runtime
                    .record_core_capture(capture)
                    .expect("author chain op");
                chain.push(
                    node_b
                        .runtime
                        .ledger
                        .get_operation(result.op_id)
                        .expect("authored op"),
                );
            }
            for op in &chain {
                deliver(&node_a, &b_id, std::slice::from_ref(op)).expect("deliver real chain");
            }

            let frontier_before = node_a
                .runtime
                .ledger
                .get_node_frontier(&b_id)
                .expect("frontier")
                .expect("frontier present");
            assert_eq!(frontier_before.last_seq, 5, "B frontier must be at seq 5");
            let materialized_before = read_org_name(&node_a, "org-equiv");

            // Forge a DIFFERENT op at the already-recorded seq=3 and deliver it.
            // It is below the frontier, so admission must compare against the op we
            // actually recorded at (B, 3) — which now lives on node_log because
            // foreign admission writes the per-node axis — and reject the fork.
            let original_seq3 = &chain[2];
            assert_eq!(original_seq3.body.node_seq, 3, "third op is seq 3");
            let forged = forge_equivocation(&node_b, original_seq3, "b-FORK-3");

            let err = deliver(&node_a, &b_id, std::slice::from_ref(&forged))
                .expect_err("equivocation below frontier must be rejected");
            assert!(
                matches!(&err, SyncLedgerError::NodeEquivocation { node, node_seq, .. }
                    if *node == b_id && *node_seq == 3),
                "expected NodeEquivocation at (B, 3), got {err:?}"
            );

            // Frontier and materialized state are untouched by the rejected fork.
            let frontier_after = node_a
                .runtime
                .ledger
                .get_node_frontier(&b_id)
                .expect("frontier")
                .expect("frontier present");
            assert_eq!(
                frontier_after, frontier_before,
                "rejected equivocation must not move B's frontier"
            );
            assert_eq!(
                read_org_name(&node_a, "org-equiv"),
                materialized_before,
                "rejected equivocation must not alter materialized state"
            );
            // The forged op_id must never have been recorded on A's view of B.
            assert!(
                node_a.runtime.ledger.get_operation(forged.op_id).is_err(),
                "forged op must not be stored after rejection"
            );
        });
    }

    #[test]
    fn foreign_chain_relayable_after_admission() {
        // Once foreign ops are admitted they must be content-resolvable from the
        // per-node axis (`operations` via `node_log`), so this node can relay /
        // escalate a snapshot of a FOREIGN chain. Pre-fix foreign bodies lived only
        // in the inbox and `get_node_operations` returned nothing for them.
        with_tmp_home(|| {
            let node_a = make_convergence_node(84); // relay node
            let node_b = make_convergence_node(85); // foreign author
            allow_self_target(&node_a, "core.organization");
            allow_self_target(&node_b, "core.organization");
            let b_id = node_b.node_id().to_string();

            let base = now_ms();
            let mut expected_ids: Vec<OperationId> = Vec::new();
            for seq in 1..=4u64 {
                let capture = org_capture(
                    "org-relay",
                    &format!("b-{seq}"),
                    hlc_at(base + seq as i64, 0, &b_id),
                );
                let result = node_b
                    .runtime
                    .record_core_capture(capture)
                    .expect("author op");
                let op = node_b
                    .runtime
                    .ledger
                    .get_operation(result.op_id)
                    .expect("authored op");
                expected_ids.push(op.op_id);
                deliver(&node_a, &b_id, std::slice::from_ref(&op)).expect("deliver to relay");
            }

            // A holds the FULL foreign chain by author + node_seq, body resolvable.
            let relayable = node_a
                .runtime
                .ledger
                .get_node_operations(NodeLogQuery {
                    node_id: b_id.clone(),
                    from_node_seq: Some(1),
                    to_node_seq: None,
                    limit: None,
                })
                .expect("foreign chain query");
            assert_eq!(
                relayable.len(),
                4,
                "relay node must hold the complete foreign chain after admission"
            );
            for (idx, op) in relayable.iter().enumerate() {
                assert_eq!(op.body.actor_node_id, b_id);
                assert_eq!(op.body.node_seq, (idx + 1) as u64);
                assert!(
                    expected_ids.contains(&op.op_id),
                    "relayed op must match an authored op_id"
                );
            }
            // earliest_live_node_seq sees foreign content too (relay floor).
            assert_eq!(
                node_a
                    .runtime
                    .ledger
                    .earliest_live_node_seq(&b_id)
                    .expect("earliest live seq"),
                Some(1),
                "relay floor must reach the foreign chain genesis"
            );
        });
    }

    /// Authors a shared-secret (`external-credentials` partition) write on `node`:
    /// builds the op on the node's own per-node chain with the supplied HLC and
    /// materializes it locally (recording the resource version), exactly as a real
    /// authoring node does. Returns the operation ready to ship to a peer.
    ///
    /// HLCs are passed by the caller from a `now_ms()` base so they sit above any
    /// baseline version a real write transaction would stamp — concurrent writes
    /// in production differ by small deltas at that scale, which is what this
    /// mirrors.
    fn author_shared_secret(
        node: &ConvergenceNode,
        key: &str,
        value: &str,
        hlc: HybridLogicalTimestamp,
    ) -> SyncOperation {
        let mut fields = BTreeMap::new();
        fields.insert("key".to_string(), FieldValue::String(key.to_string()));
        fields.insert("value".to_string(), FieldValue::String(value.to_string()));
        let capture = crate::sync::core_capture::CoreWriteCapture::new(
            crate::sync::core_registry::CoreSyncResourceKind::SharedSettingSecret,
            "org-default",
            key,
            SqlWriteAction::Insert,
            fields,
            None,
            hlc,
            test_epoch(),
        );
        let result = node
            .runtime
            .record_core_capture(capture)
            .expect("author shared secret op");
        let operation = node
            .runtime
            .ledger
            .get_operation(result.op_id)
            .expect("authored shared secret op");
        // A real authoring node also materializes its own write, which records the
        // resource's HLC in `core_resource_versions`. Without this, the local
        // write would carry no version and a stale concurrent op from a peer would
        // wrongly win the LWW gate (which compares against the version table).
        crate::sync::core_materializer::apply_core_operation(
            &node.runtime.db,
            &node.runtime.settings_cipher,
            &operation,
        )
        .expect("materialize own shared secret");
        operation
    }

    #[test]
    fn convergence_equivocation_is_rejected() {
        // Byzantine fork: the SAME author signs two DIFFERENT histories. A node is
        // single-writer over its own chain, so a second op at node_seq=2 that does
        // NOT chain onto the seq=1 the victim already accepted can only mean the
        // author forked its history. Admission must reject it (the per-node chain's
        // hash link is exactly what catches this) and the accepted op must stand.
        with_tmp_home(|| {
            let victim = make_convergence_node(91);
            allow_self_target(&victim, "core.organization");

            // The honest author: real seq1 → seq2 chain.
            let honest = make_convergence_node(90);
            let author_id = honest.node_id().to_string();
            let honest_op1 = {
                let cap = org_capture("org-alpha", "honest-1", hlc_at(7_000, 0, &author_id));
                let r = honest.runtime.record_core_capture(cap).expect("honest1");
                honest.runtime.ledger.get_operation(r.op_id).expect("h1")
            };
            let honest_op2 = {
                let cap = org_capture("org-alpha", "honest-2", hlc_at(7_001, 0, &author_id));
                let r = honest.runtime.record_core_capture(cap).expect("honest2");
                honest.runtime.ledger.get_operation(r.op_id).expect("h2")
            };

            // The forger: the SAME ed25519 identity over a fresh ledger, so it also
            // mints seq1 → seq2, but its seq1 has different content (→ different
            // hash). Its seq2 therefore chains onto a DIFFERENT seq1 than the honest
            // history — a fork. Sharing the signer makes both histories validly
            // signed under the same node_id, which is precisely the Byzantine case.
            let forger = ConvergenceNode {
                runtime: SyncRuntime {
                    db: make_db(),
                    ledger: Arc::new(
                        FjallSyncLedgerStore::open(tempfile::tempdir().expect("forge dir").path())
                            .expect("forge ledger"),
                    ),
                    signer: RuntimeSigner {
                        node_id: author_id.clone(),
                        security: honest.runtime.signer.security.clone(),
                    },
                    local_node_id: author_id.clone(),
                    settings_cipher: make_settings_cipher(90),
                    hlc: HlcClock::new(author_id.clone(), None),
                    addon_reconciler: parking_lot::RwLock::new(None),
                    max_inbox_defer_attempts: MAX_INBOX_DEFER_ATTEMPTS,
                    adopt_inflight: parking_lot::Mutex::new(HashSet::new()),
                    pending_epoch_reconcile: parking_lot::Mutex::new(Vec::new()),
                    floor_anchor_warned: parking_lot::Mutex::new(HashSet::new()),
                    push_state: PushState::default(),
                },
                _ledger_dir: tempfile::tempdir().expect("forge tempdir"),
            };
            // Forged seq1 (different content → different hash than honest seq1).
            let _forged_op1 = {
                let cap = org_capture("org-alpha", "EVIL-1", hlc_at(7_500, 0, &author_id));
                let r = forger.runtime.record_core_capture(cap).expect("evil1");
                forger.runtime.ledger.get_operation(r.op_id).expect("e1")
            };
            let forged_op2 = {
                let cap = org_capture("org-alpha", "EVIL-2", hlc_at(9_999, 0, &author_id));
                let r = forger.runtime.record_core_capture(cap).expect("evil2");
                forger.runtime.ledger.get_operation(r.op_id).expect("e2")
            };
            assert_eq!(honest_op2.body.node_seq, 2);
            assert_eq!(forged_op2.body.node_seq, 2, "fork must reuse seq 2");
            assert_ne!(
                honest_op2.body.prev_node_hash, forged_op2.body.prev_node_hash,
                "the forked seq2 must chain onto a different seq1 — that is the fork"
            );

            // Victim accepts the honest chain: frontier reaches seq2 = honest_op2.
            deliver(&victim, &author_id, std::slice::from_ref(&honest_op1))
                .expect("honest seq1 accepted");
            deliver(&victim, &author_id, std::slice::from_ref(&honest_op2))
                .expect("honest seq2 accepted");
            assert_eq!(
                read_org_name(&victim, "org-alpha").as_deref(),
                Some("honest-2")
            );

            // Now the forged seq2 arrives. Its node_seq (2) is below the frontier
            // (next expected 3), so it is treated as already-known IF its op_id
            // matches; here it does not, and the divergent prev-hash means the
            // author forked. Re-deliver the forged seq2 as a FRESH chain so it lands
            // at the expected slot and the hash-link guard fires.
            let fresh_victim = make_convergence_node(92);
            allow_self_target(&fresh_victim, "core.organization");
            // Accept the honest seq1 so the next expected is seq2.
            deliver(&fresh_victim, &author_id, std::slice::from_ref(&honest_op1))
                .expect("honest seq1 on fresh victim");
            // The forged seq2 at the expected slot does NOT chain onto honest seq1.
            let wire = vec![operation_to_wire(&forged_op2).expect("wire")];
            let err = fresh_victim
                .runtime
                .store_incoming_operations(&author_id, wire, None)
                .expect_err("forked chain must be rejected");
            match err {
                SyncLedgerError::HashChainMismatch { node, node_seq } => {
                    assert_eq!(node, author_id);
                    assert_eq!(node_seq, 2);
                }
                other => panic!("expected HashChainMismatch (fork), got {other:?}"),
            }

            // The honest seq1 still stands; the fork did not advance the frontier.
            let frontier = frontier_snapshot(&fresh_victim, &[author_id.clone()]);
            assert_eq!(frontier.len(), 1);
            assert_eq!(frontier[0].1, 1, "frontier must remain at the honest seq1");
            assert_eq!(
                read_org_name(&fresh_victim, "org-alpha").as_deref(),
                Some("honest-1"),
                "the forged history must not materialize"
            );
        });
    }

    #[test]
    fn convergence_gap_defers_then_catches_up() {
        // A chain hole: op at node_seq=3 arrives before 1 and 2. It must NOT apply
        // prematurely (would gap the chain); it queues a catch-up pull. Once 1 and
        // 2 are delivered, the whole chain applies in order and state is correct.
        with_tmp_home(|| {
            let author = make_convergence_node(100);
            let target = make_convergence_node(101);
            allow_self_target(&target, "core.organization");
            let author_id = author.node_id().to_string();

            // Author a 3-op chain. Use the SAME key so LWW order (by HLC) decides
            // the final value; give seq 3 the highest HLC so the converged value is
            // unambiguous once the chain is complete.
            let op1 = {
                let cap = org_capture("org-alpha", "seq1", hlc_at(8_000, 0, &author_id));
                let r = author.runtime.record_core_capture(cap).expect("op1");
                author
                    .runtime
                    .ledger
                    .get_operation(r.op_id)
                    .expect("get op1")
            };
            let op2 = {
                let cap = org_capture("org-alpha", "seq2", hlc_at(8_001, 0, &author_id));
                let r = author.runtime.record_core_capture(cap).expect("op2");
                author
                    .runtime
                    .ledger
                    .get_operation(r.op_id)
                    .expect("get op2")
            };
            let op3 = {
                let cap = org_capture("org-alpha", "seq3", hlc_at(8_002, 0, &author_id));
                let r = author.runtime.record_core_capture(cap).expect("op3");
                author
                    .runtime
                    .ledger
                    .get_operation(r.op_id)
                    .expect("get op3")
            };
            assert_eq!(
                (op1.body.node_seq, op2.body.node_seq, op3.body.node_seq),
                (1, 2, 3)
            );

            // Deliver seq 3 FIRST: it is a gap above the expected frontier (1).
            deliver(&target, &author_id, std::slice::from_ref(&op3)).expect("deliver seq3 early");

            // Nothing applied, frontier still empty, and a repair pull was queued.
            assert_eq!(
                read_org_name(&target, "org-alpha"),
                None,
                "out-of-order op must not apply prematurely"
            );
            assert!(
                frontier_snapshot(&target, &[author_id.clone()]).is_empty(),
                "frontier must not advance on a gap"
            );
            assert_eq!(
                total_repair_queue_len(&target, &[author_id.clone()]),
                1,
                "a gap must queue exactly one catch-up pull"
            );

            // Catch-up: deliver 1, then 2. After 2 the contiguous chain 1..=3 is
            // complete (seq 3 was buffered? no — it was dropped, so redeliver it).
            deliver(&target, &author_id, std::slice::from_ref(&op1)).expect("deliver seq1");
            deliver(&target, &author_id, std::slice::from_ref(&op2)).expect("deliver seq2");
            deliver(&target, &author_id, std::slice::from_ref(&op3)).expect("redeliver seq3");

            // The full chain applied in order; the highest-HLC write (seq3) wins.
            assert_eq!(
                read_org_name(&target, "org-alpha").as_deref(),
                Some("seq3"),
                "after catch-up the chain applies and LWW picks the latest"
            );
            let frontier = frontier_snapshot(&target, &[author_id.clone()]);
            assert_eq!(frontier.len(), 1);
            assert_eq!(frontier[0].1, 3, "frontier must reach node_seq 3");
            let authored = [
                (author_id.clone(), op1.op_id),
                (author_id.clone(), op2.op_id),
                (author_id.clone(), op3.op_id),
            ];
            assert_eq!(
                present_op_count(&target, &authored),
                3,
                "all three ops present after catch-up"
            );
        });
    }

    /// Forces `node`'s locally-active epoch to `(counter, node_id)`, modelling a
    /// migration where each node independently bumped to its own epoch. Ops the
    /// node mints afterwards carry this epoch.
    fn set_node_epoch(node: &ConvergenceNode, counter: u64) {
        node.runtime
            .ledger
            .set_epoch(BaselineEpoch {
                counter,
                origin_node: node.node_id().to_string(),
            })
            .expect("set epoch");
    }

    /// Authors one Organization write on `node` under its current epoch and
    /// returns the signed operation.
    fn author_org_op(node: &ConvergenceNode, key: &str, value: &str, wall: i64) -> SyncOperation {
        let cap = org_capture(key, value, hlc_at(wall, 0, node.node_id()));
        let result = node.runtime.record_core_capture(cap).expect("author op");
        node.runtime
            .ledger
            .get_operation(result.op_id)
            .expect("authored op")
    }

    /// The migration bug, fixed: every node starts under its OWN epoch (counter:1,
    /// distinct origin) with the same data. Delivering each node's op to the others
    /// must NOT spin on EpochMismatch — instead the lower-epoch nodes request a
    /// baseline adopt from the canonical winner (lowest node_id), and after they
    /// adopt that epoch every op is accepted with no further mismatch.
    #[test]
    fn divergent_epochs_converge_to_canonical() {
        with_tmp_home(|| {
            let nodes: Vec<ConvergenceNode> = (0..4u8)
                .map(|i| make_convergence_node(80u8.wrapping_add(i)))
                .collect();
            for node in &nodes {
                allow_self_target(node, "core.organization");
                set_node_epoch(node, 1);
            }

            // The canonical winner is the lowest node_id at counter:1.
            let node_ids: Vec<String> = nodes.iter().map(|n| n.node_id().to_string()).collect();
            let winner_idx = (0..nodes.len())
                .min_by(|&a, &b| node_ids[a].cmp(&node_ids[b]))
                .expect("non-empty");
            let winner_epoch = nodes[winner_idx].runtime.ledger.current_epoch().unwrap();

            // Each node authors one op under its own epoch.
            let ops: Vec<SyncOperation> = (0..nodes.len())
                .map(|i| author_org_op(&nodes[i], "org-alpha", &format!("v-{i}"), 1_000 + i as i64))
                .collect();

            // Deliver every author's op to every OTHER node. A loser delivering to
            // the winner is dropped as stale (no request). A winner (or higher
            // node) delivering to a loser triggers exactly one adopt request toward
            // that author. No delivery returns an error.
            for target_idx in 0..nodes.len() {
                let _ = nodes[target_idx].runtime.take_pending_reconcile();
                for src_idx in 0..nodes.len() {
                    if src_idx == target_idx {
                        continue;
                    }
                    deliver(
                        &nodes[target_idx],
                        &node_ids[src_idx],
                        std::slice::from_ref(&ops[src_idx]),
                    )
                    .expect("delivery never errors on epoch mismatch");
                }
            }

            // Every node that is NOT the winner must have requested an adopt toward
            // the canonical winner (it saw the winner's op under the winning epoch).
            // The winner requested nothing (its own epoch already wins over all).
            for (idx, node) in nodes.iter().enumerate() {
                let requests = node.runtime.take_pending_reconcile();
                if idx == winner_idx {
                    assert!(
                        requests.is_empty(),
                        "winner must never request an adopt; got {requests:?}"
                    );
                } else {
                    assert!(
                        requests
                            .iter()
                            .any(|r| r.donor_node_id == node_ids[winner_idx]
                                && r.donor_epoch_counter == winner_epoch.counter),
                        "node {idx} must request adopt from canonical winner; got {requests:?}"
                    );
                }
            }

            // Simulate the adopt: every loser adopts the winner's epoch (in
            // production `pull_baseline_from_donor` does this via the donor's
            // snapshot). After that, re-delivering the winner's op is ACCEPTED with
            // no mismatch and no new reconcile request.
            for (idx, node) in nodes.iter().enumerate() {
                if idx == winner_idx {
                    continue;
                }
                node.runtime
                    .ledger
                    .set_epoch(winner_epoch.clone())
                    .expect("adopt epoch");
            }
            for target_idx in 0..nodes.len() {
                if target_idx == winner_idx {
                    continue;
                }
                deliver(
                    &nodes[target_idx],
                    &node_ids[winner_idx],
                    std::slice::from_ref(&ops[winner_idx]),
                )
                .expect("post-adopt delivery");
                let requests = nodes[target_idx].runtime.take_pending_reconcile();
                assert!(
                    requests.is_empty(),
                    "after adopting the winning epoch there must be no further mismatch"
                );
                assert!(
                    node_holds_op(
                        &nodes[target_idx],
                        &node_ids[winner_idx],
                        ops[winner_idx].op_id
                    ),
                    "winner's op must be admitted once epochs agree"
                );
            }

            drop(nodes);
        });
    }

    /// A flood of epoch-mismatched ops from one donor under one winning epoch must
    /// produce exactly ONE adopt request (debounce), not one per op. A different
    /// donor/epoch is a distinct key and is allowed through.
    #[test]
    fn epoch_reconcile_adopt_is_debounced_per_donor_epoch() {
        with_tmp_home(|| {
            let local = make_convergence_node(96);
            allow_self_target(&local, "core.organization");
            set_node_epoch(&local, 1); // local epoch: (1, local_id)

            // A donor whose epoch wins over local: same counter, lower origin. We
            // cannot pick the donor's node_id (it is its ed25519 key), so author the
            // donor's ops on a node and only keep the ops; what matters for the
            // debounce is the (donor_node_id, epoch) the local node observes.
            let donor = make_convergence_node(50);
            set_node_epoch(&donor, 2); // higher counter → always wins regardless of origin
            allow_self_target(&donor, "core.organization");
            let donor_id = donor.node_id().to_string();
            let donor_epoch = donor.runtime.ledger.current_epoch().unwrap();

            // Three ops from the same donor under the same winning epoch.
            let ops: Vec<SyncOperation> = (0..3)
                .map(|i| author_org_op(&donor, "org-alpha", &format!("d-{i}"), 3_000 + i))
                .collect();

            let _ = local.runtime.take_pending_reconcile();
            // The three ops form a contiguous chain seq1..3; the mismatch is hit on
            // the FIRST admitted op (seq1 == expected), which fences on epoch before
            // advancing. Deliver them one batch at a time to exercise repeated
            // mismatches against the same (donor, epoch).
            for op in &ops {
                deliver(&local, &donor_id, std::slice::from_ref(op)).expect("deliver");
            }
            let requests = local.runtime.take_pending_reconcile();
            assert_eq!(
                requests.len(),
                1,
                "debounce: many mismatched ops from one donor+epoch yield ONE adopt request"
            );
            assert_eq!(requests[0].donor_node_id, donor_id);
            assert_eq!(requests[0].donor_epoch_counter, donor_epoch.counter);

            // Until the in-flight adopt is cleared, a further mismatch is suppressed.
            deliver(&local, &donor_id, std::slice::from_ref(&ops[0])).expect("redeliver");
            assert!(
                local.runtime.take_pending_reconcile().is_empty(),
                "still in flight: no duplicate request"
            );

            // Once the adopt settles (cleared), a fresh mismatch may request again.
            local
                .runtime
                .clear_adopt_inflight(&donor_id, donor_epoch.counter);
            deliver(&local, &donor_id, std::slice::from_ref(&ops[0]))
                .expect("redeliver after clear");
            assert_eq!(
                local.runtime.take_pending_reconcile().len(),
                1,
                "after clearing the debounce a new mismatch requests an adopt again"
            );

            drop((local, donor));
        });
    }

    /// The local epoch wins over the remote's: the stale remote op is dropped with
    /// NO adopt request (we do not adopt a losing baseline). The remote peer will
    /// itself adopt when it observes our op under the winning epoch.
    #[test]
    fn winning_local_epoch_drops_stale_remote_without_adopt() {
        with_tmp_home(|| {
            let local = make_convergence_node(60); // will hold the winning epoch
            allow_self_target(&local, "core.organization");
            local
                .runtime
                .ledger
                .set_epoch(BaselineEpoch {
                    counter: 5,
                    origin_node: local.node_id().to_string(),
                })
                .expect("local epoch");

            // Remote authors under a strictly-lower epoch (counter 1).
            let remote = make_convergence_node(61);
            set_node_epoch(&remote, 1);
            allow_self_target(&remote, "core.organization");
            let remote_id = remote.node_id().to_string();
            let stale = author_org_op(&remote, "org-alpha", "stale", 4_000);

            let _ = local.runtime.take_pending_reconcile();
            deliver(&local, &remote_id, std::slice::from_ref(&stale))
                .expect("stale delivery is dropped, not errored");
            assert!(
                local.runtime.take_pending_reconcile().is_empty(),
                "local epoch wins: no adopt requested, op simply dropped"
            );
            assert!(
                !node_holds_op(&local, &remote_id, stale.op_id),
                "the stale op under the losing epoch is not admitted"
            );

            drop((local, remote));
        });
    }

    /// Live-bug regression: after a mesh-wide baseline-epoch adopt, a relay's
    /// serving floor for a foreign chain sits ABOVE the pre-adopt prefix — the
    /// reset deleted those core ops without persisting any snapshot. A pull from
    /// seq 1 used to hard-error ("sync pull below compaction floor ... but no
    /// snapshot covers it") and deadlock the requester forever. Now the relay
    /// serves a floor-anchored log slice and the requester anchors at the floor,
    /// admits the post-adopt chain and ends with no repair left to loop on. The
    /// test also proves the post-adopt PUSH path: chains continue across the
    /// adopt (node_heads survive the reset), so the author's first post-adopt op
    /// chains onto the relay's preserved frontier and MATERIALIZES instead of
    /// being swallowed as already-known or rejected as an equivocation.
    #[test]
    fn post_adopt_pull_below_floor_serves_floor_anchored_slice() {
        with_tmp_home(|| {
            let relay = make_convergence_node(64);
            let author = make_convergence_node(65);
            let joiner = make_convergence_node(66);
            for node in [&author, &joiner] {
                allow_self_target(node, "core.organization");
            }
            let author_id = author.node_id().to_string();
            // The relay must BOTH materialize organization ops itself (admission
            // gates on `local_target_allowed`) AND serve them full to the joiner
            // (`peer_target_allowed`). An authority policy yields a single target,
            // so use the production default for global core resources instead:
            // `replicated_by_permission` blanket-allows every trusted node.
            repository::upsert_sync_policy(
                &relay.runtime.db,
                "policy-core-core.organization",
                "org-default",
                crate::sync::core_registry::CORE_SYNC_ADDON_ID,
                Some("core.organization"),
                None,
                "replicated_by_permission",
                None,
                None,
                true,
            )
            .expect("relay sync policy");
            for node_id in [relay.node_id(), joiner.node_id()] {
                repository::upsert_sync_node_identity(
                    &relay.runtime.db,
                    node_id,
                    "pub",
                    "ed25519",
                    "Trusted Node",
                    "laptop",
                    "trusted",
                    None,
                    "standard",
                )
                .expect("relay sync node");
            }

            // Pre-adopt: the author mints a core chain and the relay admits it.
            let pre_adopt: Vec<SyncOperation> = (0..3)
                .map(|i| author_org_op(&author, "org-alpha", &format!("pre-{i}"), 1_000 + i))
                .collect();
            deliver(&relay, &author_id, &pre_adopt).expect("relay admits pre-adopt chain");
            let pre_adopt_tip = pre_adopt.last().expect("pre-adopt ops").body.node_seq;

            // Mesh-wide convergence on a winning epoch: every node runs the
            // production adopt (set_epoch + core-partition reset + reseed).
            let winning = BaselineEpoch {
                counter: 7,
                origin_node: author_id.clone(),
            };
            for node in [&relay, &author, &joiner] {
                node.runtime
                    .adopt_donor_baseline_epoch(&winning)
                    .expect("baseline adopt");
            }

            // Post-adopt write: the author's chain CONTINUES above the old tip
            // (the adopt reseed already minted its current rows from there on).
            let post = author_org_op(&author, "org-beta", "post-adopt", 2_000);
            assert!(
                post.body.node_seq > pre_adopt_tip,
                "the adopt must not restart the author's chain at seq 1"
            );

            // The relay catches up on the whole post-adopt segment via the normal
            // push path: the preserved frontier (pre-adopt tip) chains straight
            // onto the first post-adopt op.
            let segment = author
                .runtime
                .ledger
                .get_node_operations(NodeLogQuery {
                    node_id: author_id.clone(),
                    from_node_seq: Some(pre_adopt_tip + 1),
                    to_node_seq: None,
                    limit: None,
                })
                .expect("post-adopt segment");
            deliver(&relay, &author_id, &segment).expect("post-adopt push admits");
            assert!(
                node_holds_op(&relay, &author_id, post.op_id),
                "post-adopt op must materialize on the relay, not be swallowed"
            );
            let relay_frontier = relay
                .runtime
                .ledger
                .get_node_frontier(&author_id)
                .expect("frontier")
                .expect("frontier present");
            assert_eq!(
                relay_frontier.last_seq, post.body.node_seq,
                "relay frontier advances to the post-adopt tip"
            );

            // Re-delivering the (now old-epoch) pre-adopt chain stays idempotent:
            // same op_id at a known position acks as already-known — never an
            // equivocation error, never a re-admission under the wrong epoch.
            deliver(&relay, &author_id, &pre_adopt).expect("old-epoch redelivery is idempotent");

            // The relay's serving floor sits above the deleted prefix: only
            // current-epoch FULL content counts (the reseed ops the relay holds
            // redacted do not, the dangling pre-adopt entries do not).
            let floor = relay
                .runtime
                .ledger
                .earliest_live_node_seq(&author_id)
                .expect("serving floor")
                .expect("relay holds live post-adopt content");
            assert!(
                floor > pre_adopt_tip,
                "floor must sit above the deleted prefix"
            );

            // THE LIVE BUG: a pull for the author's chain from seq 1.
            let pull = MeshSyncPullPayload {
                from_node_id: joiner.node_id().to_string(),
                target_node_id: author_id.clone(),
                from_node_seq: 1,
                limit: 4096,
            };
            let result = relay
                .runtime
                .handle_pull_payload(joiner.node_id(), pull)
                .expect("below-floor pull with no covering snapshot must not hard-error");
            let MeshSyncPullResult::Operations(response) = result else {
                panic!("no snapshot exists, so the relay must serve a floor-anchored log slice");
            };
            assert_eq!(
                response.from_node_seq, floor,
                "the served slice starts at the serving floor"
            );
            assert_eq!(response.serving_floor_node_seq, floor);
            assert!(!response.operations.is_empty());

            // The joiner anchors at the floor: the slice admits, its frontier
            // jumps to the post-adopt tip and nothing is left to loop on.
            let relay_id = relay.node_id().to_string();
            joiner
                .runtime
                .handle_pull_response_payload(&relay_id, response)
                .expect("floor-anchored response admits on the joiner");
            assert!(
                node_holds_op(&joiner, &author_id, post.op_id),
                "joiner must admit the post-adopt op pulled from the relay"
            );
            let joiner_frontier = joiner
                .runtime
                .ledger
                .get_node_frontier(&author_id)
                .expect("frontier")
                .expect("frontier present");
            assert_eq!(
                joiner_frontier.last_seq, post.body.node_seq,
                "joiner frontier reaches the post-adopt tip via the floor anchor"
            );
            assert_eq!(
                read_org_name(&joiner, "org-beta").as_deref(),
                Some("post-adopt"),
                "the pulled post-adopt write must materialize on the joiner"
            );
            assert_eq!(
                total_repair_queue_len(&joiner, &[relay_id]),
                0,
                "catch-up satisfied: no repair request left to loop on"
            );
        });
    }
}

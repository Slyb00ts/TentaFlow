// =============================================================================
// Plik: sync/ledger/fjall_store.rs
// Opis: Implementacja SyncLedgerStore oparta o Fjall i partycjonowane keyspace'y.
// =============================================================================

use super::types::{
    decode, encode, partition_materialization_order, AppendResult, BaselineEpoch, CompactionPolicy,
    HybridLogicalTimestamp, InboxEntry, LedgerResult, NewSyncOperation, NodeChainEntry,
    NodeEnvironment, NodeFrontierEntry, NodeHead, NodeLogQuery, OperationId, OperationQuery,
    OutboxEntry, PartitionId, PeerId, RedactedRecord, RepairQueueEntry, SnapshotId,
    SyncLedgerError, SyncLedgerStore, SyncOperation, SyncOperationSigner, SyncOperationVerifier,
    SyncSnapshot, SyncTarget,
};
use fjall::{Database, Keyspace, KeyspaceCreateOptions, PersistMode};
use parking_lot::Mutex;
use std::collections::HashSet;
use std::path::Path;

// Operations are content-addressed by op_id. `node_log` is the per-node chain
// axis (actor_node_id || 0x00 || node_seq.be -> op_id); `partition_index` is the
// materialization routing index (partition_id || 0x00 || op_id -> ()).
const OPERATIONS: &str = "operations";
// Per-node chain positions the local node holds ONLY as verified redacted
// placeholders (op_id -> RedactedRecord): authored + signed, body withheld
// because we are not a sync target for the resource. Keyed by op_id like
// `operations`, but never materialized and never relayed as full.
const REDACTED_LOG: &str = "redacted_log";
const NODE_LOG: &str = "node_log";
const PARTITION_INDEX: &str = "partition_index";
const NODE_HEADS: &str = "node_heads";
const OUTBOX: &str = "outbox";
const INBOX: &str = "inbox";
const NODE_FRONTIER: &str = "node_frontier";
const REPAIR_QUEUE: &str = "repair_queue";
const SNAPSHOTS: &str = "snapshots";
const META: &str = "meta";
const META_EPOCH_KEY: &[u8] = b"baseline_epoch";
const META_ENVIRONMENT_KEY: &[u8] = b"node_environment";
const META_HLC_KEY: &[u8] = b"hlc_state";
const META_SCHEMA_KEY: &[u8] = b"schema_version";
// Durable "needs baseline reseed" marker. `open` sets it in the SAME persist as
// the schema-version stamp + wipe, so a crash after stamping v2 but before the
// runtime bumps the epoch and reseeds is recoverable: the next boot still sees
// the marker and repeats the (idempotent) reseed. The runtime clears it only
// AFTER a successful bump+reseed. Without it the on-disk v2 stamp would suppress
// the wipe on the retry boot, skipping the reseed and letting the local mint
// restart node_seq=1 under genesis epoch=0 — self-equivocation against peers.
const META_NEEDS_BASELINE_RESET_KEY: &[u8] = b"needs_baseline_reset";
const SEP: u8 = 0;

/// On-disk ledger layout version. Bump whenever the physical keyspace contract
/// changes incompatibly (e.g. per-partition chains → per-node chains). A node
/// that opens a ledger written under a different version cannot trust its
/// node_heads/node_log axis, so `open` wipes the directory and the runtime
/// reseeds from SQLite under a bumped epoch instead of silently restarting
/// node_seq from 1 (which would equivocate the local node against live peers
/// that still remember the old chain).
pub const LEDGER_SCHEMA_VERSION: u32 = 2;

pub struct FjallSyncLedgerStore {
    db: Database,
    operations: Keyspace,
    redacted_log: Keyspace,
    node_log: Keyspace,
    partition_index: Keyspace,
    node_heads: Keyspace,
    outbox: Keyspace,
    inbox: Keyspace,
    node_frontier: Keyspace,
    repair_queue: Keyspace,
    snapshots: Keyspace,
    meta: Keyspace,
    // Guards only the LOCAL node's head: a node is single-writer over its own
    // chain, so this serializes node_seq minting without blocking reads.
    append_lock: Mutex<()>,
    // Mirror of the durable `META_NEEDS_BASELINE_RESET_KEY` marker read at `open`.
    // The runtime reads it to force a baseline reset (bump epoch + reseed from
    // SQLite) so the freshly restarted node_seq chain is fenced from the
    // pre-upgrade chain peers remember. The on-disk marker — not this flag — is
    // the source of truth: it survives a crash before the reseed completes, so a
    // retry boot re-runs the (idempotent) reseed. `clear_baseline_reset_marker`
    // erases it only after a successful bump+reseed.
    needs_baseline_reset: bool,
    // In-memory mirror of `META_ENVIRONMENT_KEY` (ROADMAP Z12), read once at
    // `open_at` and kept current by `set_environment`. `current_environment()`
    // is on the hot path (per outbox entry per repair tick, per inbox
    // admission, per resolver request) — without this cache it hit Fjall +
    // CBOR decode on every one of those calls.
    environment_cache: parking_lot::RwLock<NodeEnvironment>,
}

impl FjallSyncLedgerStore {
    /// Picks the frontier entry to persist for `candidate` without ever regressing.
    /// The redacted->full upgrade path admits a body at a position BELOW the current
    /// frontier (we caught up past it holding only a placeholder), so it passes a
    /// frontier carrying that lower `last_seq`. Writing it verbatim would rewind the
    /// node frontier and make us re-pull (and possibly re-admit as a fork) every seq
    /// between the upgraded position and the real head. Keep the higher head and its
    /// hash; only adopt the candidate when it genuinely advances the chain.
    fn frontier_to_persist(&self, candidate: NodeFrontierEntry) -> LedgerResult<NodeFrontierEntry> {
        match self.get_node_frontier(&candidate.node_id)? {
            Some(existing) if existing.last_seq >= candidate.last_seq => Ok(existing),
            _ => Ok(candidate),
        }
    }

    pub fn open(path: impl AsRef<Path>) -> LedgerResult<Self> {
        let path = path.as_ref();
        let mut store = Self::open_at(path)?;

        // Schema fence: an on-disk layout written by a different build cannot be
        // trusted, because the per-node chain axis (node_heads/node_log) may have
        // a different meaning. Detect a missing/mismatched version, wipe the whole
        // directory, and re-open fresh so the local node never reuses stale heads.
        let on_disk = match store.meta.get(META_SCHEMA_KEY)? {
            Some(value) => Some(decode::<u32>(value.as_ref())?),
            None => None,
        };
        match on_disk {
            Some(version) if version == LEDGER_SCHEMA_VERSION => {
                // Current schema, but a prior boot may have wiped + stamped v2 and
                // then crashed before the runtime finished the reseed. The durable
                // marker tells us the reseed still owes us; pick it up so the
                // runtime repeats it instead of minting under genesis epoch=0.
                store.needs_baseline_reset = match store.meta.get(META_NEEDS_BASELINE_RESET_KEY)? {
                    Some(value) => decode::<bool>(value.as_ref())?,
                    None => false,
                };
            }
            Some(version) => {
                // Drop the open handle before removing files so no keyspace holds
                // the directory, then rebuild from scratch.
                drop(store);
                wipe_ledger_dir(path)?;
                store = Self::open_at(path)?;
                // Stamp the version AND the durable reseed marker in one persist:
                // the marker must survive a crash before the runtime reseeds.
                store
                    .meta
                    .insert(META_SCHEMA_KEY, encode(&LEDGER_SCHEMA_VERSION)?)?;
                store
                    .meta
                    .insert(META_NEEDS_BASELINE_RESET_KEY, encode(&true)?)?;
                store.persist()?;
                store.needs_baseline_reset = true;
                tracing::warn!(
                    "sync ledger: on-disk schema v{version} != v{LEDGER_SCHEMA_VERSION}, \
                     wiped ledger and forcing baseline reseed under a bumped epoch"
                );
            }
            None if store_is_empty(&store)? => {
                // Brand-new ledger: stamp the current version, nothing to migrate.
                store
                    .meta
                    .insert(META_SCHEMA_KEY, encode(&LEDGER_SCHEMA_VERSION)?)?;
                store.persist()?;
            }
            None => {
                // Populated but unversioned: predates the schema fence, so its
                // layout is the pre-v2 per-partition chain. Wipe and reseed.
                drop(store);
                wipe_ledger_dir(path)?;
                store = Self::open_at(path)?;
                store
                    .meta
                    .insert(META_SCHEMA_KEY, encode(&LEDGER_SCHEMA_VERSION)?)?;
                store
                    .meta
                    .insert(META_NEEDS_BASELINE_RESET_KEY, encode(&true)?)?;
                store.persist()?;
                store.needs_baseline_reset = true;
                tracing::warn!(
                    "sync ledger: unversioned on-disk schema, wiped ledger and forcing \
                     baseline reseed under a bumped epoch"
                );
            }
        }
        Ok(store)
    }

    fn open_at(path: &Path) -> LedgerResult<Self> {
        let db = Database::builder(path).open()?;
        let meta = db.keyspace(META, KeyspaceCreateOptions::default)?;
        let environment = match meta.get(META_ENVIRONMENT_KEY)? {
            Some(value) => decode(value.as_ref())?,
            None => NodeEnvironment::default(),
        };
        Ok(Self {
            operations: db.keyspace(OPERATIONS, KeyspaceCreateOptions::default)?,
            redacted_log: db.keyspace(REDACTED_LOG, KeyspaceCreateOptions::default)?,
            node_log: db.keyspace(NODE_LOG, KeyspaceCreateOptions::default)?,
            partition_index: db.keyspace(PARTITION_INDEX, KeyspaceCreateOptions::default)?,
            node_heads: db.keyspace(NODE_HEADS, KeyspaceCreateOptions::default)?,
            outbox: db.keyspace(OUTBOX, KeyspaceCreateOptions::default)?,
            inbox: db.keyspace(INBOX, KeyspaceCreateOptions::default)?,
            node_frontier: db.keyspace(NODE_FRONTIER, KeyspaceCreateOptions::default)?,
            repair_queue: db.keyspace(REPAIR_QUEUE, KeyspaceCreateOptions::default)?,
            snapshots: db.keyspace(SNAPSHOTS, KeyspaceCreateOptions::default)?,
            meta,
            db,
            append_lock: Mutex::new(()),
            needs_baseline_reset: false,
            environment_cache: parking_lot::RwLock::new(environment),
        })
    }

    /// True if a durable baseline-reset marker is set (stale-schema wipe, possibly
    /// from a prior boot that crashed mid-reseed); the runtime then forces a
    /// baseline reseed under a bumped epoch and clears the marker on success.
    pub fn needs_baseline_reset(&self) -> bool {
        self.needs_baseline_reset
    }

    /// Erases the durable baseline-reset marker. The runtime calls this ONLY after
    /// a successful bump+reseed, so a crash anywhere before this point leaves the
    /// marker in place and the next boot repeats the idempotent reseed.
    pub fn clear_baseline_reset_marker(&self) -> LedgerResult<()> {
        self.meta.remove(META_NEEDS_BASELINE_RESET_KEY)?;
        self.persist()
    }

    fn persist(&self) -> LedgerResult<()> {
        Ok(self.db.persist(PersistMode::SyncAll)?)
    }

    fn load_node_head(&self, node_id: &str) -> LedgerResult<Option<NodeHead>> {
        match self.node_heads.get(node_id.as_bytes())? {
            Some(value) => Ok(Some(decode(value.as_ref())?)),
            None => Ok(None),
        }
    }
}

impl SyncLedgerStore for FjallSyncLedgerStore {
    fn append_operation(
        &self,
        operation: NewSyncOperation,
        signer: &dyn SyncOperationSigner,
    ) -> LedgerResult<AppendResult> {
        let _guard = self.append_lock.lock();
        let local_node = signer.node_id().to_string();
        let partition_id = operation.partition_id.clone();
        let previous_head = self.load_node_head(&local_node)?;
        let previous_hash = previous_head.as_ref().map(|head| head.last_hash);
        let node_seq = previous_head
            .as_ref()
            .map_or(1, |head| head.last_seq.saturating_add(1));
        let operation = SyncOperation::from_new(operation, node_seq, previous_hash, signer)?;
        operation.validate_integrity()?;
        let head = NodeHead {
            node_id: local_node.clone(),
            last_seq: node_seq,
            last_hash: operation.operation_hash,
        };

        let mut batch = self.db.batch().durability(Some(PersistMode::SyncAll));
        batch.insert(
            &self.operations,
            operation.op_id.as_bytes().to_vec(),
            encode(&operation)?,
        );
        batch.insert(
            &self.node_log,
            node_log_key(&local_node, node_seq),
            operation.op_id.as_bytes().to_vec(),
        );
        batch.insert(
            &self.partition_index,
            partition_index_key(&partition_id, operation.op_id),
            Vec::new(),
        );
        batch.insert(&self.node_heads, local_node.as_bytes(), encode(&head)?);
        batch.commit()?;

        Ok(AppendResult {
            op_id: operation.op_id,
            operation_hash: operation.operation_hash,
            previous_node_hash: previous_hash,
            node_seq,
        })
    }

    fn get_operations(&self, query: OperationQuery) -> LedgerResult<Vec<SyncOperation>> {
        let mut operations = Vec::new();
        let prefix = partition_prefix(&query.partition_id);
        for item in self.partition_index.prefix(&prefix) {
            let (key, _) = item.into_inner()?;
            let Some(op_id) = op_id_from_partition_index_key(key.as_ref()) else {
                continue;
            };
            let Some(value) = self.operations.get(op_id.as_bytes())? else {
                continue;
            };
            operations.push(decode(value.as_ref())?);
            if query.limit.is_some_and(|limit| operations.len() >= limit) {
                break;
            }
        }
        Ok(operations)
    }

    fn list_all_operations(&self) -> LedgerResult<Vec<SyncOperation>> {
        let mut operations = Vec::new();
        for item in self.operations.iter() {
            let (_, value) = item.into_inner()?;
            operations.push(decode(value.as_ref())?);
        }
        Ok(operations)
    }

    fn get_node_operations(&self, query: NodeLogQuery) -> LedgerResult<Vec<SyncOperation>> {
        let mut operations = Vec::new();
        let prefix = node_prefix(&query.node_id);
        for item in self.node_log.prefix(&prefix) {
            let (key, value) = item.into_inner()?;
            let Some(node_seq) = node_seq_from_node_log_key(key.as_ref()) else {
                continue;
            };
            if query.from_node_seq.is_some_and(|from| node_seq < from) {
                continue;
            }
            if query.to_node_seq.is_some_and(|to| node_seq > to) {
                continue;
            }
            let op_id = operation_id_from_bytes(value.as_ref())?;
            let Some(op_value) = self.operations.get(op_id.as_bytes())? else {
                continue;
            };
            operations.push(decode(op_value.as_ref())?);
            if query.limit.is_some_and(|limit| operations.len() >= limit) {
                break;
            }
        }
        Ok(operations)
    }

    fn get_node_chain_entries(&self, query: NodeLogQuery) -> LedgerResult<Vec<NodeChainEntry>> {
        let mut entries = Vec::new();
        let prefix = node_prefix(&query.node_id);
        for item in self.node_log.prefix(&prefix) {
            let (key, value) = item.into_inner()?;
            let Some(node_seq) = node_seq_from_node_log_key(key.as_ref()) else {
                continue;
            };
            if query.from_node_seq.is_some_and(|from| node_seq < from) {
                continue;
            }
            if query.to_node_seq.is_some_and(|to| node_seq > to) {
                continue;
            }
            let op_id = operation_id_from_bytes(value.as_ref())?;
            // Prefer the full op; fall back to the redacted placeholder. A node_log
            // entry whose content row was compacted away AND has no redacted record
            // is simply skipped — the caller's compaction-floor escalation handles
            // a requester sitting below the floor.
            if let Some(op_value) = self.operations.get(op_id.as_bytes())? {
                entries.push(NodeChainEntry::Full(decode(op_value.as_ref())?));
            } else if let Some(red_value) = self.redacted_log.get(op_id.as_bytes())? {
                entries.push(NodeChainEntry::Redacted(decode(red_value.as_ref())?));
            }
            if query.limit.is_some_and(|limit| entries.len() >= limit) {
                break;
            }
        }
        Ok(entries)
    }

    fn get_operation(&self, op_id: OperationId) -> LedgerResult<SyncOperation> {
        let operation = self
            .operations
            .get(op_id.as_bytes())?
            .ok_or(SyncLedgerError::OperationNotFound(op_id))?;
        decode(operation.as_ref())
    }

    fn get_node_log_entry(
        &self,
        node_id: &str,
        node_seq: u64,
    ) -> LedgerResult<Option<OperationId>> {
        match self.node_log.get(node_log_key(node_id, node_seq))? {
            Some(value) => Ok(Some(operation_id_from_bytes(value.as_ref())?)),
            None => Ok(None),
        }
    }

    fn earliest_live_node_seq(&self, node_id: &str) -> LedgerResult<Option<u64>> {
        // node_log is keyed node_id||0x00||node_seq.be, so the prefix scan yields
        // seqs in ascending order; the first entry whose `operations` row still
        // exists AND was minted under the live epoch is the earliest position we
        // can relay. An op kept under an abandoned epoch is not servable: every
        // peer's admission fences on the current epoch, so relaying it can never
        // advance a requester's frontier — treating it as live would also anchor
        // the serving floor below the first admissible position and make the
        // served slice gap across the epoch boundary.
        let current_epoch = self.current_epoch()?;
        let prefix = node_prefix(node_id);
        for item in self.node_log.prefix(&prefix) {
            let (key, value) = item.into_inner()?;
            let Some(node_seq) = node_seq_from_node_log_key(key.as_ref()) else {
                continue;
            };
            let op_id = operation_id_from_bytes(value.as_ref())?;
            if let Some(op_bytes) = self.operations.get(op_id.as_bytes())? {
                let operation: SyncOperation = decode(op_bytes.as_ref())?;
                if operation.body.epoch == current_epoch {
                    return Ok(Some(node_seq));
                }
            }
        }
        Ok(None)
    }

    fn put_in_outbox(&self, target: SyncTarget, op_id: OperationId) -> LedgerResult<()> {
        let entry = OutboxEntry {
            target: target.clone(),
            op_id,
            delivered: false,
            acknowledged: false,
            retry_count: 0,
        };
        self.outbox
            .insert(outbox_key(&target, op_id), encode(&entry)?)?;
        self.persist()
    }

    fn get_outbox_entry(
        &self,
        target: SyncTarget,
        op_id: OperationId,
    ) -> LedgerResult<OutboxEntry> {
        load_outbox_entry(&self.outbox, &target, op_id)
    }

    fn list_pending_outbox(
        &self,
        target: SyncTarget,
        limit: usize,
    ) -> LedgerResult<Vec<OutboxEntry>> {
        let mut entries = Vec::new();
        let prefix = scoped_prefix(target.as_str());
        for item in self.outbox.prefix(&prefix) {
            let (_, value) = item.into_inner()?;
            let entry: OutboxEntry = decode(value.as_ref())?;
            if entry.acknowledged {
                continue;
            }
            entries.push(entry);
            if entries.len() >= limit {
                break;
            }
        }
        Ok(entries)
    }

    fn list_outbox_for_operation(&self, op_id: OperationId) -> LedgerResult<Vec<OutboxEntry>> {
        let mut entries = Vec::new();
        for item in self.outbox.iter() {
            let (_, value) = item.into_inner()?;
            let entry: OutboxEntry = decode(value.as_ref())?;
            if entry.op_id == op_id {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    fn put_verified_in_inbox(
        &self,
        source: PeerId,
        operation: SyncOperation,
        verifier: &dyn SyncOperationVerifier,
    ) -> LedgerResult<()> {
        verifier.verify_operation_signature(&operation)?;
        // Environment fence FIRST (ROADMAP Z12, P1-4): a cross-environment op
        // with a HIGHER epoch must never fall through to the epoch check below,
        // because an `EpochMismatch` there triggers `note_epoch_mismatch` ->
        // `spawn_epoch_reconcile_adopts` -> a full baseline pull from the
        // sender. Checking environment first turns that case into a plain
        // `EnvironmentMismatch` (dropped, never repaired) instead of a
        // cross-environment baseline adopt. Independent from `epoch` — never
        // conflated with it (pitfall #4: a schema migration inside one
        // environment still bumps only `epoch`).
        let local_environment = self.current_environment()?;
        if operation.body.environment != local_environment {
            return Err(SyncLedgerError::EnvironmentMismatch {
                expected: local_environment,
                actual: operation.body.environment,
            });
        }
        let local_epoch = self.current_epoch()?;
        if operation.body.epoch != local_epoch {
            return Err(SyncLedgerError::EpochMismatch {
                expected: local_epoch,
                actual: operation.body.epoch.clone(),
            });
        }
        // A re-delivery of an op already present: if it previously failed (data
        // conflict) or was deferred for ordering, reset it to a fresh retryable
        // state. A repair pull that finally brings the prerequisite INSERT relies
        // on this — otherwise the stuck UPDATE would never be retried again.
        if let Some(existing) = self.inbox.get(inbox_key(&source, operation.op_id))? {
            let mut entry: InboxEntry = decode(existing.as_ref())?;
            if entry.applied || (!entry.conflicted && entry.deferred_count == 0) {
                return Ok(());
            }
            entry.conflicted = false;
            entry.conflict_message = None;
            entry.deferred_count = 0;
            self.inbox
                .insert(inbox_key(&source, entry.operation.op_id), encode(&entry)?)?;
            return self.persist();
        }
        let entry = InboxEntry {
            source: source.clone(),
            operation,
            applied: false,
            conflicted: false,
            conflict_message: None,
            deferred_count: 0,
        };
        self.inbox
            .insert(inbox_key(&source, entry.operation.op_id), encode(&entry)?)?;
        self.persist()
    }

    fn admit_verified_operation(
        &self,
        source: PeerId,
        operation: SyncOperation,
        frontier: NodeFrontierEntry,
        verifier: &dyn SyncOperationVerifier,
    ) -> LedgerResult<()> {
        verifier.verify_operation_signature(&operation)?;
        // Environment fence FIRST — see `put_verified_in_inbox` above (P1-4):
        // must run before the epoch check so a cross-environment op never
        // reaches `EpochMismatch`/the baseline-adopt path.
        let local_environment = self.current_environment()?;
        if operation.body.environment != local_environment {
            return Err(SyncLedgerError::EnvironmentMismatch {
                expected: local_environment,
                actual: operation.body.environment,
            });
        }
        let local_epoch = self.current_epoch()?;
        if operation.body.epoch != local_epoch {
            return Err(SyncLedgerError::EpochMismatch {
                expected: local_epoch,
                actual: operation.body.epoch.clone(),
            });
        }
        // A foreign op must land on the SAME per-node axis a local mint writes to
        // (`operations` + `node_log` + `partition_index`), not only in the inbox.
        // Without it `get_node_log_entry(foreign_node, seq)` is always None, so an
        // equivocation below the frontier (a different op at an already-recorded
        // (node, seq)) cannot be detected and is silently swallowed as AlreadyKnown.
        // It also makes foreign chains content-resolvable for relay/escalation and
        // visible to materialization. We do NOT touch `node_heads`: that axis is the
        // LOCAL mint's chain head only; a foreign node's frontier lives in
        // `node_frontier`. These two write paths never collide: inbox is the
        // not-yet-applied apply queue (drained via `mark_inbox_applied`), while
        // operations/partition_index are the ledger VIEW (snapshot/merkle/relay),
        // never a second apply path. Materialization is idempotent per resource
        // version, so even if a partition scan ever re-observed an applied op it
        // would not double-apply.
        let actor_node_id = operation.body.actor_node_id.clone();
        let node_seq = operation.body.node_seq;
        let partition_id = operation.body.partition_id.clone();
        let op_id = operation.op_id;
        let operation_bytes = encode(&operation)?;

        // Build the inbox entry, preserving the re-delivery reset semantics of
        // `put_verified_in_inbox` (a previously failed/deferred op becomes fresh
        // again), then commit it together with the frontier advance in one batch.
        let inbox_key = inbox_key(&source, op_id);
        let entry = match self.inbox.get(&inbox_key)? {
            Some(existing) => {
                let mut entry: InboxEntry = decode(existing.as_ref())?;
                if entry.applied || (!entry.conflicted && entry.deferred_count == 0) {
                    // Already in a clean/applied state: nothing to rewrite for the
                    // inbox, but the frontier advance and the per-node axis must
                    // still be durable (a re-delivery of an op we hold).
                    let mut batch = self.db.batch().durability(Some(PersistMode::SyncAll));
                    batch.insert(&self.operations, op_id.as_bytes().to_vec(), operation_bytes);
                    // Upgrade redacted→full: if we previously held this position
                    // only as a redacted placeholder, the full body now supersedes
                    // it. Same op_id, so this is a completion, not a fork.
                    batch.remove(&self.redacted_log, op_id.as_bytes().to_vec());
                    batch.insert(
                        &self.node_log,
                        node_log_key(&actor_node_id, node_seq),
                        op_id.as_bytes().to_vec(),
                    );
                    batch.insert(
                        &self.partition_index,
                        partition_index_key(&partition_id, op_id),
                        Vec::new(),
                    );
                    let persisted = self.frontier_to_persist(frontier)?;
                    batch.insert(
                        &self.node_frontier,
                        persisted.node_id.as_bytes().to_vec(),
                        encode(&persisted)?,
                    );
                    batch.commit()?;
                    return Ok(());
                }
                entry.conflicted = false;
                entry.conflict_message = None;
                entry.deferred_count = 0;
                entry
            }
            None => InboxEntry {
                source: source.clone(),
                operation,
                applied: false,
                conflicted: false,
                conflict_message: None,
                deferred_count: 0,
            },
        };

        let mut batch = self.db.batch().durability(Some(PersistMode::SyncAll));
        batch.insert(&self.inbox, inbox_key, encode(&entry)?);
        batch.insert(&self.operations, op_id.as_bytes().to_vec(), operation_bytes);
        // Upgrade redacted→full: a placeholder previously held at this op_id is
        // superseded by the full body. Same op_id, so a completion, not a fork.
        batch.remove(&self.redacted_log, op_id.as_bytes().to_vec());
        batch.insert(
            &self.node_log,
            node_log_key(&actor_node_id, node_seq),
            op_id.as_bytes().to_vec(),
        );
        batch.insert(
            &self.partition_index,
            partition_index_key(&partition_id, op_id),
            Vec::new(),
        );
        let persisted = self.frontier_to_persist(frontier)?;
        batch.insert(
            &self.node_frontier,
            persisted.node_id.as_bytes().to_vec(),
            encode(&persisted)?,
        );
        batch.commit()?;
        Ok(())
    }

    fn admit_redacted_operation(
        &self,
        record: RedactedRecord,
        frontier: NodeFrontierEntry,
        verifier: &dyn SyncOperationVerifier,
    ) -> LedgerResult<()> {
        verifier.verify_redacted_signature(&record)?;
        // No environment/epoch fence here, unlike `put_verified_in_inbox`/
        // `admit_verified_operation` (ROADMAP Z12, P3 — deliberate, low
        // impact): `RedactedRecord` carries no `environment`/`epoch` fields
        // to fence against (it is content-blind by design — op_id/hash/
        // actor/seq/prev_hash/signature only), and admitting one from a
        // cross-environment peer never leaks or materializes any content —
        // it only advances THAT peer's per-node chain-continuity bookkeeping
        // (equivocation guard + frontier), never `inbox`/`operations`. A
        // future fence would need to add `environment` to `RedactedRecord`
        // itself (append-only, mirroring `SyncOperationBody`), which is not
        // worth the wire-format churn for a placeholder that never touches
        // real data.
        //
        // A redacted op only advances the per-node chain axis. It is recorded on
        // `node_log` (so equivocation detection and chain continuity resolve at
        // this seq) and in `redacted_log` (so a later full op can detect it must
        // upgrade), and it advances `node_frontier`. It NEVER touches `inbox`
        // (nothing to apply), `operations`/`partition_index` (no body to relay or
        // materialize), or `node_heads` (the local mint's chain only).
        let mut batch = self.db.batch().durability(Some(PersistMode::SyncAll));
        batch.insert(
            &self.redacted_log,
            record.op_id.as_bytes().to_vec(),
            encode(&record)?,
        );
        batch.insert(
            &self.node_log,
            node_log_key(&record.actor_node_id, record.node_seq),
            record.op_id.as_bytes().to_vec(),
        );
        batch.insert(
            &self.node_frontier,
            frontier.node_id.as_bytes().to_vec(),
            encode(&frontier)?,
        );
        batch.commit()?;
        Ok(())
    }

    fn get_redacted_record(&self, op_id: OperationId) -> LedgerResult<Option<RedactedRecord>> {
        match self.redacted_log.get(op_id.as_bytes())? {
            Some(value) => Ok(Some(decode(value.as_ref())?)),
            None => Ok(None),
        }
    }

    fn get_inbox_entry(&self, source: PeerId, op_id: OperationId) -> LedgerResult<InboxEntry> {
        load_inbox_entry(&self.inbox, &source, op_id)
    }

    fn list_unapplied_inbox(&self, limit: usize) -> LedgerResult<Vec<InboxEntry>> {
        let mut entries = Vec::new();
        for item in self.inbox.iter() {
            let (_, value) = item.into_inner()?;
            let entry: InboxEntry = decode(value.as_ref())?;
            if entry.applied || entry.conflicted {
                continue;
            }
            entries.push(entry);
            if entries.len() >= limit {
                break;
            }
        }
        Ok(entries)
    }

    fn mark_inbox_applied(&self, source: PeerId, op_id: OperationId) -> LedgerResult<()> {
        let mut entry = load_inbox_entry(&self.inbox, &source, op_id)?;
        entry.applied = true;
        entry.conflicted = false;
        entry.conflict_message = None;
        entry.deferred_count = 0;
        self.inbox
            .insert(inbox_key(&source, op_id), encode(&entry)?)?;
        self.persist()
    }

    fn mark_inbox_conflicted(
        &self,
        source: PeerId,
        op_id: OperationId,
        message: String,
    ) -> LedgerResult<()> {
        let mut entry = load_inbox_entry(&self.inbox, &source, op_id)?;
        entry.conflicted = true;
        entry.deferred_count = 0;
        entry.conflict_message = Some(message);
        self.inbox
            .insert(inbox_key(&source, op_id), encode(&entry)?)?;
        self.persist()
    }

    fn mark_inbox_deferred(
        &self,
        source: PeerId,
        op_id: OperationId,
        message: String,
    ) -> LedgerResult<u32> {
        let mut entry = load_inbox_entry(&self.inbox, &source, op_id)?;
        entry.conflicted = false;
        entry.deferred_count = entry.deferred_count.saturating_add(1);
        entry.conflict_message = Some(message);
        let deferred_count = entry.deferred_count;
        self.inbox
            .insert(inbox_key(&source, op_id), encode(&entry)?)?;
        self.persist()?;
        Ok(deferred_count)
    }

    fn mark_delivered(&self, target: SyncTarget, op_id: OperationId) -> LedgerResult<()> {
        let mut entry = load_outbox_entry(&self.outbox, &target, op_id)?;
        entry.delivered = true;
        self.outbox
            .insert(outbox_key(&target, op_id), encode(&entry)?)?;
        self.persist()
    }

    fn mark_acknowledged(&self, target: SyncTarget, op_id: OperationId) -> LedgerResult<()> {
        let mut entry = load_outbox_entry(&self.outbox, &target, op_id)?;
        entry.delivered = true;
        entry.acknowledged = true;
        self.outbox
            .insert(outbox_key(&target, op_id), encode(&entry)?)?;
        self.persist()
    }

    fn remove_outbox_entry(&self, target: SyncTarget, op_id: OperationId) -> LedgerResult<()> {
        self.outbox.remove(outbox_key(&target, op_id))?;
        self.persist()
    }

    fn get_node_frontier(&self, node_id: &str) -> LedgerResult<Option<NodeFrontierEntry>> {
        match self.node_frontier.get(node_id.as_bytes())? {
            Some(value) => Ok(Some(decode(value.as_ref())?)),
            None => Ok(None),
        }
    }

    fn save_node_frontier(&self, frontier: NodeFrontierEntry) -> LedgerResult<()> {
        self.node_frontier
            .insert(frontier.node_id.as_bytes(), encode(&frontier)?)?;
        self.persist()
    }

    fn upsert_repair_request(&self, entry: RepairQueueEntry) -> LedgerResult<()> {
        let key = repair_queue_key(&entry.peer, &entry.target_node_id);
        let entry = match self.repair_queue.get(&key)? {
            Some(value) => {
                let mut existing: RepairQueueEntry = decode(value.as_ref())?;
                if entry.from_node_seq < existing.from_node_seq {
                    existing.from_node_seq = entry.from_node_seq;
                    existing.next_attempt_ms = entry.next_attempt_ms;
                    existing.retry_count = entry.retry_count;
                }
                existing
            }
            None => entry,
        };
        self.repair_queue.insert(key, encode(&entry)?)?;
        self.persist()
    }

    fn list_due_repair_requests(
        &self,
        peer: PeerId,
        now_ms: i64,
        limit: usize,
    ) -> LedgerResult<Vec<RepairQueueEntry>> {
        let mut entries = Vec::new();
        let prefix = scoped_prefix(peer.as_str());
        for item in self.repair_queue.prefix(&prefix) {
            let (_, value) = item.into_inner()?;
            let entry: RepairQueueEntry = decode(value.as_ref())?;
            if entry.next_attempt_ms > now_ms {
                continue;
            }
            entries.push(entry);
            if entries.len() >= limit {
                break;
            }
        }
        Ok(entries)
    }

    fn mark_repair_attempted(
        &self,
        peer: PeerId,
        target_node_id: &str,
        next_attempt_ms: i64,
        retry_count: u32,
    ) -> LedgerResult<()> {
        let key = repair_queue_key(&peer, target_node_id);
        if let Some(value) = self.repair_queue.get(&key)? {
            let mut entry: RepairQueueEntry = decode(value.as_ref())?;
            entry.next_attempt_ms = next_attempt_ms;
            entry.retry_count = retry_count;
            self.repair_queue.insert(key, encode(&entry)?)?;
            self.persist()?;
        }
        Ok(())
    }

    fn remove_repair_request(&self, peer: PeerId, target_node_id: &str) -> LedgerResult<()> {
        self.repair_queue
            .remove(repair_queue_key(&peer, target_node_id))?;
        self.persist()
    }

    fn save_snapshot(&self, snapshot: SyncSnapshot) -> LedgerResult<()> {
        self.snapshots.insert(
            snapshot_key(
                &snapshot.partition_id,
                snapshot.up_to_sequence,
                snapshot.snapshot_id.as_str(),
            ),
            encode(&snapshot)?,
        )?;
        self.persist()
    }

    fn get_snapshot(
        &self,
        partition: PartitionId,
        up_to_sequence: u64,
        snapshot_id: SnapshotId,
    ) -> LedgerResult<SyncSnapshot> {
        self.snapshots
            .get(snapshot_key(
                &partition,
                up_to_sequence,
                snapshot_id.as_str(),
            ))?
            .map(|value| decode(value.as_ref()))
            .transpose()?
            .ok_or_else(|| SyncLedgerError::SnapshotNotFound {
                partition: partition.as_str().to_string(),
                snapshot_id: snapshot_id.as_str().to_string(),
            })
    }

    fn latest_snapshot(
        &self,
        partition: PartitionId,
        up_to_sequence: Option<u64>,
    ) -> LedgerResult<Option<SyncSnapshot>> {
        let prefix = partition_prefix(&partition);
        let mut latest: Option<SyncSnapshot> = None;
        for item in self.snapshots.prefix(&prefix) {
            let (_, value) = item.into_inner()?;
            let snapshot: SyncSnapshot = decode(value.as_ref())?;
            if up_to_sequence.is_some_and(|limit| snapshot.up_to_sequence > limit) {
                continue;
            }
            if latest
                .as_ref()
                .is_none_or(|current| snapshot.up_to_sequence > current.up_to_sequence)
            {
                latest = Some(snapshot);
            }
        }
        Ok(latest)
    }

    fn get_node_head(&self, node_id: &str) -> LedgerResult<Option<NodeHead>> {
        self.load_node_head(node_id)
    }

    fn list_outbox_for_partition(
        &self,
        partition: PartitionId,
        _up_to_sequence: u64,
    ) -> LedgerResult<Vec<OutboxEntry>> {
        let mut entries = Vec::new();
        for item in self.outbox.iter() {
            let (_, value) = item.into_inner()?;
            let entry: OutboxEntry = decode(value.as_ref())?;
            let Ok(operation) = self.get_operation(entry.op_id) else {
                continue;
            };
            // The partition no longer carries a sequence watermark, so an outbox
            // entry is in-scope purely by partition membership.
            if operation.body.partition_id == partition {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    fn compact(&self, policy: CompactionPolicy) -> LedgerResult<()> {
        let Some(keep_after_sequence) = policy.keep_operations_after_sequence else {
            return Ok(());
        };
        // `keep_after_sequence` is a 1-based materialization watermark over the
        // partition's HLC-ordered operations: drop the first `keep_after_sequence
        // - 1` of them (those a snapshot already covers). Node-log entries and the
        // node heads are left intact — only the per-partition materialization
        // index and the content row are reaped, mirroring the old behaviour.
        let keep_after = keep_after_sequence.saturating_sub(1) as usize;
        if keep_after == 0 {
            return Ok(());
        }

        // Drop the oldest operations first, in canonical materialization order, so
        // the prefix removed here matches the snapshot prefix the watermark refers
        // to even when several operations share an HLC.
        let mut ordered: Vec<SyncOperation> = Vec::new();
        let prefix = partition_prefix(&policy.partition_id);
        for item in self.partition_index.prefix(&prefix) {
            let (key, _) = item.into_inner()?;
            let Some(op_id) = op_id_from_partition_index_key(key.as_ref()) else {
                continue;
            };
            let Some(value) = self.operations.get(op_id.as_bytes())? else {
                continue;
            };
            ordered.push(decode(value.as_ref())?);
        }
        ordered.sort_by(partition_materialization_order);

        let mut batch = self.db.batch().durability(Some(PersistMode::SyncAll));
        for operation in ordered.into_iter().take(keep_after) {
            batch.remove(
                &self.partition_index,
                partition_index_key(&policy.partition_id, operation.op_id),
            );
            batch.remove(&self.operations, operation.op_id.as_bytes().to_vec());
        }
        batch.commit()?;
        Ok(())
    }

    fn current_epoch(&self) -> LedgerResult<BaselineEpoch> {
        match self.meta.get(META_EPOCH_KEY)? {
            Some(value) => decode(value.as_ref()),
            // Genesis epoch: a node that has never performed a baseline reset
            // mints and accepts operations under counter 0. `origin_node` is
            // empty so any explicitly-minted epoch (counter >= 1) sorts above it.
            None => Ok(BaselineEpoch {
                counter: 0,
                origin_node: String::new(),
            }),
        }
    }

    fn set_epoch(&self, epoch: BaselineEpoch) -> LedgerResult<()> {
        self.meta.insert(META_EPOCH_KEY, encode(&epoch)?)?;
        self.persist()
    }

    fn current_environment(&self) -> LedgerResult<NodeEnvironment> {
        // In-memory cache (populated at `open_at`, kept current by
        // `set_environment`) — this is a hot path (per-op admission, per
        // outbox entry, per resolver request), never a Fjall read.
        Ok(*self.environment_cache.read())
    }

    fn set_environment(&self, environment: NodeEnvironment) -> LedgerResult<()> {
        self.meta
            .insert(META_ENVIRONMENT_KEY, encode(&environment)?)?;
        self.persist()?;
        *self.environment_cache.write() = environment;
        Ok(())
    }

    fn current_hlc(&self) -> LedgerResult<Option<HybridLogicalTimestamp>> {
        match self.meta.get(META_HLC_KEY)? {
            Some(value) => Ok(Some(decode(value.as_ref())?)),
            None => Ok(None),
        }
    }

    fn save_hlc(&self, timestamp: &HybridLogicalTimestamp) -> LedgerResult<()> {
        self.meta.insert(META_HLC_KEY, encode(timestamp)?)?;
        self.persist()
    }

    fn reset_partitions_with_prefix(&self, partition_prefix: &str) -> LedgerResult<()> {
        let _guard = self.append_lock.lock();
        let prefix_bytes = partition_prefix.as_bytes();

        let mut batch = self.db.batch().durability(Some(PersistMode::SyncAll));

        // Operations are content-addressed, so the partition_index (keyed by
        // partition_id || 0x00 || op_id) is the only way to attribute an op to a
        // partition. Collect the matching op_ids and drop the content rows and
        // their partition-index entries.
        let mut matched_op_ids: HashSet<OperationId> = HashSet::new();
        for item in self.partition_index.prefix(prefix_bytes) {
            let (key, _) = item.into_inner()?;
            let Some(op_id) = op_id_from_partition_index_key(key.as_ref()) else {
                continue;
            };
            batch.remove(&self.partition_index, key.to_vec());
            batch.remove(&self.operations, op_id.as_bytes().to_vec());
            matched_op_ids.insert(op_id);
        }

        // node_log (node_id||seq -> op_id), node_heads and node_frontier are the
        // per-NODE chain axis, not the per-partition materialization axis: a single
        // node writes ops across many partitions on ONE dense, monotonic node_seq
        // chain. A per-partition reset MUST leave that axis fully intact — punching
        // holes in node_log would break chain density and make the local node mint
        // duplicate node_seq positions (self-equivocation). The content rows are
        // gone, so `get_node_operations` simply skips the now-dangling op_ids
        // (it already filters entries whose `operations` row is absent), and epoch
        // fencing rejects any pre-reset operation that arrives late.

        // Snapshots are keyed by partition_prefix too.
        for item in self.snapshots.prefix(prefix_bytes) {
            let (key, _) = item.into_inner()?;
            batch.remove(&self.snapshots, key.to_vec());
        }

        // Outbox is keyed by (target, op_id) and stores no partition, so the only
        // entries we can attribute to this reset are those pointing at a core
        // operation we just removed (`matched_op_ids`). Orphaned entries (op_id
        // outside live operations) must be left untouched: their source
        // partition is unknown, so deleting them here could wipe a non-core
        // (addon/kv) orphan. The push path cleans up orphans lazily instead.
        for item in self.outbox.iter() {
            let (key, value) = item.into_inner()?;
            let entry: OutboxEntry = decode(value.as_ref())?;
            if matched_op_ids.contains(&entry.op_id) {
                batch.remove(&self.outbox, key.to_vec());
            }
        }

        // Inbox carries the full operation, so decode its partition_id.
        for item in self.inbox.iter() {
            let (key, value) = item.into_inner()?;
            let entry: InboxEntry = decode(value.as_ref())?;
            if entry
                .operation
                .body
                .partition_id
                .as_str()
                .starts_with(partition_prefix)
            {
                batch.remove(&self.inbox, key.to_vec());
            }
        }

        batch.commit()?;
        Ok(())
    }
}

/// True only for a freshly-created ledger (no chain state, no journal). Used to
/// tell a brand-new directory (just stamp the schema version) apart from a
/// populated but unversioned one (pre-fence layout — must wipe and reseed).
fn store_is_empty(store: &FjallSyncLedgerStore) -> LedgerResult<bool> {
    let any = store.node_heads.iter().next().is_some()
        || store.operations.iter().next().is_some()
        || store.node_log.iter().next().is_some()
        || store.inbox.iter().next().is_some()
        || store.outbox.iter().next().is_some();
    Ok(!any)
}

/// Removes every entry under the ledger directory so it can be re-created from a
/// clean state, without deleting the directory itself (its handle/permissions
/// may be held by the caller). The schema fence calls this after dropping the DB.
fn wipe_ledger_dir(path: &Path) -> LedgerResult<()> {
    if !path.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(path).map_err(|e| SyncLedgerError::Runtime(e.to_string()))? {
        let entry = entry.map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        let child = entry.path();
        let result = if child.is_dir() {
            std::fs::remove_dir_all(&child)
        } else {
            std::fs::remove_file(&child)
        };
        result.map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
    }
    Ok(())
}

fn load_outbox_entry(
    outbox: &Keyspace,
    target: &SyncTarget,
    op_id: OperationId,
) -> LedgerResult<OutboxEntry> {
    outbox
        .get(outbox_key(target, op_id))?
        .map(|value| decode(value.as_ref()))
        .transpose()?
        .ok_or_else(|| SyncLedgerError::OutboxEntryNotFound {
            target: target.as_str().to_string(),
            op_id,
        })
}

fn load_inbox_entry(
    inbox: &Keyspace,
    source: &PeerId,
    op_id: OperationId,
) -> LedgerResult<InboxEntry> {
    inbox
        .get(inbox_key(source, op_id))?
        .map(|value| decode(value.as_ref()))
        .transpose()?
        .ok_or_else(|| SyncLedgerError::InboxEntryNotFound {
            peer: source.as_str().to_string(),
            op_id,
        })
}

fn partition_prefix(partition: &PartitionId) -> Vec<u8> {
    let mut key = partition.as_str().as_bytes().to_vec();
    key.push(SEP);
    key
}

fn node_prefix(node_id: &str) -> Vec<u8> {
    let mut key = node_id.as_bytes().to_vec();
    key.push(SEP);
    key
}

fn node_log_key(node_id: &str, node_seq: u64) -> Vec<u8> {
    let mut key = node_prefix(node_id);
    key.extend_from_slice(&node_seq.to_be_bytes());
    key
}

fn node_seq_from_node_log_key(key: &[u8]) -> Option<u64> {
    let bytes: [u8; 8] = key.get(key.len().checked_sub(8)?..)?.try_into().ok()?;
    Some(u64::from_be_bytes(bytes))
}

fn partition_index_key(partition: &PartitionId, op_id: OperationId) -> Vec<u8> {
    let mut key = partition_prefix(partition);
    key.extend_from_slice(op_id.as_bytes());
    key
}

fn op_id_from_partition_index_key(key: &[u8]) -> Option<OperationId> {
    let bytes: [u8; 32] = key.get(key.len().checked_sub(32)?..)?.try_into().ok()?;
    Some(OperationId::from_hash(bytes))
}

fn operation_id_from_bytes(bytes: &[u8]) -> LedgerResult<OperationId> {
    let hash: [u8; 32] = bytes
        .try_into()
        .map_err(|_| SyncLedgerError::InvalidOperationIdHex {
            value: hex::encode(bytes),
        })?;
    Ok(OperationId::from_hash(hash))
}

fn outbox_key(target: &SyncTarget, op_id: OperationId) -> Vec<u8> {
    scoped_id_key(target.as_str(), op_id)
}

fn inbox_key(source: &PeerId, op_id: OperationId) -> Vec<u8> {
    scoped_id_key(source.as_str(), op_id)
}

fn scoped_id_key(scope: &str, op_id: OperationId) -> Vec<u8> {
    let mut key = scoped_prefix(scope);
    key.extend_from_slice(op_id.as_bytes());
    key
}

fn scoped_prefix(scope: &str) -> Vec<u8> {
    let mut key = scope.as_bytes().to_vec();
    key.push(SEP);
    key
}

fn repair_queue_key(peer: &PeerId, target_node_id: &str) -> Vec<u8> {
    let mut key = peer.as_str().as_bytes().to_vec();
    key.push(SEP);
    key.extend_from_slice(target_node_id.as_bytes());
    key
}

fn snapshot_key(partition: &PartitionId, sequence: u64, snapshot_id: &str) -> Vec<u8> {
    let mut key = partition_prefix(partition);
    key.extend_from_slice(&sequence.to_be_bytes());
    key.push(SEP);
    key.extend_from_slice(snapshot_id.as_bytes());
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::ledger::{
        ActionType, BaselineEpoch, Ed25519OperationSigner, FieldValue, HexNodeIdOperationVerifier,
        HybridLogicalTimestamp, SnapshotId, SyncOperationSigner,
    };
    use ed25519_dalek::SigningKey;
    use rand_core_06::OsRng;
    use std::collections::BTreeMap;

    fn signer() -> Ed25519OperationSigner {
        let signing_key = SigningKey::generate(&mut OsRng);
        let node_id = hex::encode(signing_key.verifying_key().to_bytes());
        Ed25519OperationSigner::new(node_id, signing_key).unwrap()
    }

    fn operation_in_partition(
        signer: &Ed25519OperationSigner,
        partition: &str,
        resource_id: &str,
    ) -> NewSyncOperation {
        let mut op = sample_operation(signer, resource_id);
        op.partition_id = PartitionId::new(partition).unwrap();
        op
    }

    fn sample_operation(signer: &Ed25519OperationSigner, resource_id: &str) -> NewSyncOperation {
        let mut changed_fields = BTreeMap::new();
        changed_fields.insert(
            "email".to_string(),
            FieldValue::String("jan@example.com".to_string()),
        );
        NewSyncOperation {
            org_id: "org_1".to_string(),
            partition_id: PartitionId::new("addon/contacts/persons").unwrap(),
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
                logical: 0,
                node_id: signer.node_id().to_string(),
            },
            epoch: BaselineEpoch {
                counter: 0,
                origin_node: String::new(),
            },
            environment: NodeEnvironment::default(),
            payload_hash: [1; 32],
            acl_snapshot_hash: [2; 32],
            policy_epoch: 1,
            encryption_info: None,
        }
    }

    #[test]
    fn append_operation_persists_hash_chain() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();

        let first = store
            .append_operation(sample_operation(&signer, "person_1"), &signer)
            .unwrap();
        let second = store
            .append_operation(sample_operation(&signer, "person_2"), &signer)
            .unwrap();

        assert_eq!(first.node_seq, 1);
        assert_eq!(second.node_seq, 2);
        assert_eq!(second.previous_node_hash, Some(first.operation_hash));
    }

    #[test]
    fn admit_redacted_records_node_log_and_advances_frontier() {
        // A redacted placeholder advances the per-node chain axis (node_log +
        // node_frontier) and is content-addressable by op_id, but never lands in
        // the inbox or the partition view — there is nothing to materialize.
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        let op = SyncOperation::from_new(sample_operation(&signer, "person_1"), 1, None, &signer)
            .unwrap();
        let record = RedactedRecord {
            op_id: op.op_id,
            operation_hash: op.operation_hash,
            actor_node_id: op.body.actor_node_id.clone(),
            node_seq: 1,
            prev_node_hash: None,
            signature: op.signature.clone(),
        };

        store
            .admit_redacted_operation(
                record.clone(),
                NodeFrontierEntry {
                    node_id: op.body.actor_node_id.clone(),
                    last_seq: 1,
                    last_hash: op.operation_hash,
                },
                &HexNodeIdOperationVerifier,
            )
            .unwrap();

        // node_log resolves the position (so equivocation detection works) and the
        // redacted record is retrievable, but the inbox stays empty.
        assert_eq!(
            store.get_node_log_entry(signer.node_id(), 1).unwrap(),
            Some(op.op_id)
        );
        assert!(store.get_redacted_record(op.op_id).unwrap().is_some());
        assert!(store.list_unapplied_inbox(16).unwrap().is_empty());
        assert!(store.get_operation(op.op_id).is_err());
        assert_eq!(
            store
                .get_node_frontier(signer.node_id())
                .unwrap()
                .unwrap()
                .last_seq,
            1
        );
    }

    #[test]
    fn full_op_upgrade_removes_redacted_record() {
        // A full op for the same op_id supersedes a redacted placeholder: the
        // redacted row is removed and the body becomes materializable. Same op_id,
        // so this is a completion, not a fork.
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        let op = SyncOperation::from_new(sample_operation(&signer, "person_1"), 1, None, &signer)
            .unwrap();
        let source = PeerId::new("peer-x").unwrap();
        let frontier = NodeFrontierEntry {
            node_id: op.body.actor_node_id.clone(),
            last_seq: 1,
            last_hash: op.operation_hash,
        };
        store
            .admit_redacted_operation(
                RedactedRecord {
                    op_id: op.op_id,
                    operation_hash: op.operation_hash,
                    actor_node_id: op.body.actor_node_id.clone(),
                    node_seq: 1,
                    prev_node_hash: None,
                    signature: op.signature.clone(),
                },
                frontier.clone(),
                &HexNodeIdOperationVerifier,
            )
            .unwrap();
        assert!(store.get_redacted_record(op.op_id).unwrap().is_some());

        store
            .admit_verified_operation(source, op.clone(), frontier, &HexNodeIdOperationVerifier)
            .unwrap();

        assert!(store.get_redacted_record(op.op_id).unwrap().is_none());
        assert!(store.get_operation(op.op_id).is_ok());
        assert_eq!(store.list_unapplied_inbox(16).unwrap().len(), 1);
    }

    #[test]
    fn baseline_reset_marker_survives_crash_before_reseed() {
        // The schema fence stamps schema v2 AND a durable reseed marker in one
        // persist. If the boot crashes after that stamp but before the runtime
        // bumps the epoch + reseeds, the next boot must STILL see the marker and
        // repeat the reseed — otherwise the v2 stamp suppresses the wipe and the
        // local mint restarts node_seq=1 under genesis epoch (self-equivocation).
        let dir = tempfile::tempdir().unwrap();

        // Seed a populated, STALE-schema ledger: write some chain state, then
        // stamp an old schema version so the next open trips the fence.
        {
            let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
            let signer = signer();
            store
                .append_operation(sample_operation(&signer, "person_1"), &signer)
                .unwrap();
            store
                .meta
                .insert(
                    META_SCHEMA_KEY,
                    encode(&(LEDGER_SCHEMA_VERSION - 1)).unwrap(),
                )
                .unwrap();
            store.persist().unwrap();
        }

        // First post-upgrade open: wipes, stamps v2, sets the durable marker.
        {
            let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
            assert!(
                store.needs_baseline_reset(),
                "stale-schema open must flag a baseline reset"
            );
            // Simulate a crash here: the runtime never ran the reseed, so the
            // marker is NOT cleared.
        }

        // Crash-recovery open: schema is already v2, but the marker persisted, so
        // the reset is still owed.
        {
            let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
            assert!(
                store.needs_baseline_reset(),
                "marker must survive a crash before the reseed completed"
            );
            // Now the runtime completes the reseed and clears the marker.
            store.clear_baseline_reset_marker().unwrap();
        }

        // Subsequent opens see a clean, current ledger: no reset owed.
        {
            let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
            assert!(
                !store.needs_baseline_reset(),
                "cleared marker must not re-trigger a reset"
            );
        }
    }

    #[test]
    fn get_node_operations_returns_node_chain_range() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        store
            .append_operation(sample_operation(&signer, "person_1"), &signer)
            .unwrap();
        store
            .append_operation(sample_operation(&signer, "person_2"), &signer)
            .unwrap();

        let operations = store
            .get_node_operations(NodeLogQuery {
                node_id: signer.node_id().to_string(),
                from_node_seq: Some(2),
                to_node_seq: Some(2),
                limit: None,
            })
            .unwrap();

        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].body.resource_id, "person_2");
    }

    #[test]
    fn get_operation_uses_operation_id_index() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        let append = store
            .append_operation(sample_operation(&signer, "person_1"), &signer)
            .unwrap();

        let operation = store.get_operation(append.op_id).unwrap();

        assert_eq!(operation.op_id, append.op_id);
    }

    #[test]
    fn outbox_tracks_delivery_and_ack() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        let append = store
            .append_operation(sample_operation(&signer, "person_1"), &signer)
            .unwrap();
        let target = SyncTarget::new("node_b").unwrap();

        store.put_in_outbox(target.clone(), append.op_id).unwrap();
        let pending = store.list_pending_outbox(target.clone(), 10).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].op_id, append.op_id);
        store.mark_delivered(target.clone(), append.op_id).unwrap();
        store.mark_acknowledged(target, append.op_id).unwrap();

        let entry = store
            .get_outbox_entry(SyncTarget::new("node_b").unwrap(), append.op_id)
            .unwrap();
        assert!(entry.delivered);
        assert!(entry.acknowledged);
        let pending = store
            .list_pending_outbox(SyncTarget::new("node_b").unwrap(), 10)
            .unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn inbox_persists_received_operation() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        let append = store
            .append_operation(sample_operation(&signer, "person_1"), &signer)
            .unwrap();
        let operation = store.get_operation(append.op_id).unwrap();
        let source = PeerId::new("node_b").unwrap();

        store
            .put_verified_in_inbox(source.clone(), operation, &HexNodeIdOperationVerifier)
            .unwrap();
        let pending = store.list_unapplied_inbox(10).unwrap();
        assert_eq!(pending.len(), 1);
        let entry = store.get_inbox_entry(source, append.op_id).unwrap();

        assert_eq!(entry.operation.op_id, append.op_id);
        assert!(!entry.applied);
    }

    #[test]
    fn inbox_marks_entry_applied() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        let append = store
            .append_operation(sample_operation(&signer, "person_1"), &signer)
            .unwrap();
        let operation = store.get_operation(append.op_id).unwrap();
        let source = PeerId::new("node_b").unwrap();

        store
            .put_verified_in_inbox(
                source.clone(),
                operation.clone(),
                &HexNodeIdOperationVerifier,
            )
            .unwrap();
        store
            .mark_inbox_applied(source.clone(), append.op_id)
            .unwrap();
        store
            .put_verified_in_inbox(source.clone(), operation, &HexNodeIdOperationVerifier)
            .unwrap();

        let entry = store.get_inbox_entry(source, append.op_id).unwrap();
        assert!(entry.applied);
        assert!(!entry.conflicted);
        assert!(store.list_unapplied_inbox(10).unwrap().is_empty());
    }

    #[test]
    fn inbox_marks_entry_conflicted() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        let append = store
            .append_operation(sample_operation(&signer, "person_1"), &signer)
            .unwrap();
        let operation = store.get_operation(append.op_id).unwrap();
        let source = PeerId::new("node_b").unwrap();

        store
            .put_verified_in_inbox(source.clone(), operation, &HexNodeIdOperationVerifier)
            .unwrap();
        store
            .mark_inbox_conflicted(
                source.clone(),
                append.op_id,
                "sql constraint violation".to_string(),
            )
            .unwrap();

        let entry = store.get_inbox_entry(source, append.op_id).unwrap();
        assert!(!entry.applied);
        assert!(entry.conflicted);
        assert_eq!(
            entry.conflict_message.as_deref(),
            Some("sql constraint violation")
        );
        assert!(store.list_unapplied_inbox(10).unwrap().is_empty());
    }

    #[test]
    fn inbox_rejects_invalid_signature() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        let append = store
            .append_operation(sample_operation(&signer, "person_1"), &signer)
            .unwrap();
        let mut operation = store.get_operation(append.op_id).unwrap();
        operation.signature[0] ^= 0x01;

        let result = store.put_verified_in_inbox(
            PeerId::new("node_b").unwrap(),
            operation,
            &HexNodeIdOperationVerifier,
        );

        assert!(matches!(
            result,
            Err(SyncLedgerError::InvalidSignature { .. })
        ));
    }

    #[test]
    fn node_frontier_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let frontier = NodeFrontierEntry {
            node_id: "node_b".to_string(),
            last_seq: 7,
            last_hash: [9; 32],
        };

        store.save_node_frontier(frontier.clone()).unwrap();
        let loaded = store.get_node_frontier("node_b").unwrap();

        assert_eq!(loaded, Some(frontier));
    }

    #[test]
    fn repair_queue_persists_due_requests() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let peer = PeerId::new("node_b").unwrap();
        let target_node = "node_author";
        store
            .upsert_repair_request(RepairQueueEntry {
                peer: peer.clone(),
                target_node_id: target_node.to_string(),
                from_node_seq: 4,
                next_attempt_ms: 100,
                retry_count: 0,
            })
            .unwrap();
        store
            .mark_repair_attempted(peer.clone(), target_node, 500, 1)
            .unwrap();
        drop(store);

        let reopened = FjallSyncLedgerStore::open(dir.path()).unwrap();
        assert!(reopened
            .list_due_repair_requests(peer.clone(), 200, 10)
            .unwrap()
            .is_empty());
        let due = reopened
            .list_due_repair_requests(peer.clone(), 500, 10)
            .unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].from_node_seq, 4);
        assert_eq!(due[0].retry_count, 1);

        reopened
            .remove_repair_request(peer.clone(), target_node)
            .unwrap();
        assert!(reopened
            .list_due_repair_requests(peer, 1_000, 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn snapshot_metadata_is_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let snapshot = SyncSnapshot {
            snapshot_id: SnapshotId::new("snap_1").unwrap(),
            partition_id: PartitionId::new("addon/contacts/persons").unwrap(),
            from_sequence: 1,
            up_to_sequence: 10,
            operation_count: 10,
            root_hash: [4; 32],
            state_hash: [6; 32],
            node_frontier: std::collections::BTreeMap::new(),
            last_operation_hash: Some([7; 32]),
            policy_epoch: 1,
            blob_kind: Some("sql_replay_v1".to_string()),
            blob_hash: Some([8; 32]),
            blob_size_bytes: 123,
            created_at_ms: 1_765_000_000_000,
            author_node_id: "node_1".to_string(),
            signature: vec![5; 64],
        };

        store.save_snapshot(snapshot).unwrap();
        let loaded = store
            .get_snapshot(
                PartitionId::new("addon/contacts/persons").unwrap(),
                10,
                SnapshotId::new("snap_1").unwrap(),
            )
            .unwrap();

        assert_eq!(loaded.root_hash, [4; 32]);
    }

    #[test]
    fn latest_snapshot_returns_newest_checkpoint_before_limit() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let partition = PartitionId::new("addon/contacts/persons").unwrap();
        for sequence in [10, 20, 30] {
            store
                .save_snapshot(SyncSnapshot {
                    snapshot_id: SnapshotId::new(format!("snap_{sequence}")).unwrap(),
                    partition_id: partition.clone(),
                    from_sequence: 1,
                    up_to_sequence: sequence,
                    operation_count: sequence,
                    root_hash: [sequence as u8; 32],
                    state_hash: [sequence as u8; 32],
                    node_frontier: std::collections::BTreeMap::new(),
                    last_operation_hash: Some([sequence as u8; 32]),
                    policy_epoch: 1,
                    blob_kind: Some("sql_replay_v1".to_string()),
                    blob_hash: Some([sequence as u8; 32]),
                    blob_size_bytes: 123,
                    created_at_ms: 1_765_000_000_000 + sequence as i64,
                    author_node_id: "node_1".to_string(),
                    signature: vec![5; 64],
                })
                .unwrap();
        }

        let latest = store
            .latest_snapshot(partition.clone(), Some(25))
            .unwrap()
            .unwrap();
        assert_eq!(latest.up_to_sequence, 20);

        let latest = store.latest_snapshot(partition, None).unwrap().unwrap();
        assert_eq!(latest.up_to_sequence, 30);
    }

    #[test]
    fn reset_core_partitions_clears_only_core_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();

        let core_partition = "core/org/org-default/flows";
        let addon_partition = "addon/contacts/persons";
        let kv_partition = "kv/org-default/memory";

        let core_append = store
            .append_operation(
                operation_in_partition(&signer, core_partition, "flow_1"),
                &signer,
            )
            .unwrap();
        store
            .append_operation(
                operation_in_partition(&signer, core_partition, "flow_2"),
                &signer,
            )
            .unwrap();
        let addon_append = store
            .append_operation(
                operation_in_partition(&signer, addon_partition, "person_1"),
                &signer,
            )
            .unwrap();
        let kv_append = store
            .append_operation(
                operation_in_partition(&signer, kv_partition, "key_1"),
                &signer,
            )
            .unwrap();

        // Populate outbox / inbox for both core and non-core.
        let target = SyncTarget::new("node_b").unwrap();
        store
            .put_in_outbox(target.clone(), core_append.op_id)
            .unwrap();
        store.put_in_outbox(target, addon_append.op_id).unwrap();

        let source = PeerId::new("node_c").unwrap();
        let core_op = store.get_operation(core_append.op_id).unwrap();
        let addon_op = store.get_operation(addon_append.op_id).unwrap();
        store
            .put_verified_in_inbox(source.clone(), core_op, &HexNodeIdOperationVerifier)
            .unwrap();
        store
            .put_verified_in_inbox(source.clone(), addon_op, &HexNodeIdOperationVerifier)
            .unwrap();

        let local_node = signer.node_id().to_string();
        let head_before = store.get_node_head(&local_node).unwrap().unwrap();

        store.reset_core_partitions().unwrap();

        // Core operations gone, addon + kv intact.
        assert!(store
            .get_operations(OperationQuery {
                partition_id: PartitionId::new(core_partition).unwrap(),
                limit: None,
            })
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .get_operations(OperationQuery {
                    partition_id: PartitionId::new(addon_partition).unwrap(),
                    limit: None,
                })
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .get_operations(OperationQuery {
                    partition_id: PartitionId::new(kv_partition).unwrap(),
                    limit: None,
                })
                .unwrap()
                .len(),
            1
        );

        // Content store for core is gone; addon/kv still resolvable.
        assert!(store.get_operation(core_append.op_id).is_err());
        assert!(store.get_operation(addon_append.op_id).is_ok());
        assert!(store.get_operation(kv_append.op_id).is_ok());

        // The local node head spans every partition the node ever wrote, so a
        // per-partition reset must NOT touch it: the single writer keeps minting a
        // monotonic node_seq across the reset.
        assert_eq!(store.get_node_head(&local_node).unwrap(), Some(head_before));

        // Outbox: core entry removed, addon entry kept.
        assert!(store
            .get_outbox_entry(SyncTarget::new("node_b").unwrap(), core_append.op_id)
            .is_err());
        assert!(store
            .get_outbox_entry(SyncTarget::new("node_b").unwrap(), addon_append.op_id)
            .is_ok());

        // Inbox: only the addon-partition entry survives.
        let inbox = store.list_unapplied_inbox(10).unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(
            inbox[0].operation.body.partition_id.as_str(),
            addon_partition
        );
    }

    #[test]
    fn reset_core_partitions_preserves_orphaned_outbox() {
        // An orphaned outbox row (operation compacted away) carries no partition,
        // so a core reset cannot know whether it belonged to core or to addon/kv.
        // It must therefore leave every orphan untouched and only remove entries
        // pointing at the live core operations it is actually resetting.
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();

        let core_partition = "core/org/org-default/flows";
        let addon_partition = "addon/contacts/persons";
        let kv_partition = "kv/org-default/memory";

        // core/flows gets two ops (sequences 1 and 2). Compaction below keeps
        // only sequence 2, so the sequence-1 op becomes the core orphan and the
        // sequence-2 op stays live.
        let core_orphan = store
            .append_operation(
                operation_in_partition(&signer, core_partition, "flow_orphan"),
                &signer,
            )
            .unwrap();
        let core_live = store
            .append_operation(
                operation_in_partition(&signer, core_partition, "flow_keep"),
                &signer,
            )
            .unwrap();
        let addon_orphan = store
            .append_operation(
                operation_in_partition(&signer, addon_partition, "person_orphan"),
                &signer,
            )
            .unwrap();
        let kv_append = store
            .append_operation(
                operation_in_partition(&signer, kv_partition, "key_1"),
                &signer,
            )
            .unwrap();

        let target = SyncTarget::new("node_b").unwrap();
        store
            .put_in_outbox(target.clone(), core_live.op_id)
            .unwrap();
        store
            .put_in_outbox(target.clone(), core_orphan.op_id)
            .unwrap();
        store
            .put_in_outbox(target.clone(), addon_orphan.op_id)
            .unwrap();
        store
            .put_in_outbox(target.clone(), kv_append.op_id)
            .unwrap();

        // Compact away the sequence-1 core op and the lone addon op (keeping
        // their outbox rows), turning both into orphans whose source partition is
        // unknowable from the outbox alone.
        store
            .compact(CompactionPolicy {
                partition_id: PartitionId::new(core_partition).unwrap(),
                keep_operations_after_sequence: Some(2),
            })
            .unwrap();
        store
            .compact(CompactionPolicy {
                partition_id: PartitionId::new(addon_partition).unwrap(),
                keep_operations_after_sequence: Some(2),
            })
            .unwrap();
        assert!(store.get_operation(core_orphan.op_id).is_err());
        assert!(store.get_operation(addon_orphan.op_id).is_err());
        assert!(store.get_operation(core_live.op_id).is_ok());

        store.reset_core_partitions().unwrap();

        // (i) Outbox entry for the live core operation being reset is removed.
        assert!(store
            .get_outbox_entry(SyncTarget::new("node_b").unwrap(), core_live.op_id)
            .is_err());
        // (ii) Orphaned outbox entries survive the reset, whether their now-gone
        // operation was core OR addon — reset never deletes orphans.
        assert!(store
            .get_outbox_entry(SyncTarget::new("node_b").unwrap(), core_orphan.op_id)
            .is_ok());
        assert!(store
            .get_outbox_entry(SyncTarget::new("node_b").unwrap(), addon_orphan.op_id)
            .is_ok());
        // The live kv entry is untouched as well.
        assert!(store
            .get_outbox_entry(SyncTarget::new("node_b").unwrap(), kv_append.op_id)
            .is_ok());
    }

    #[test]
    fn epoch_defaults_to_genesis_and_persists_bump() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();

        let genesis = store.current_epoch().unwrap();
        assert_eq!(genesis.counter, 0);
        assert!(genesis.origin_node.is_empty());

        let bumped = store.bump_epoch("node_a").unwrap();
        assert_eq!(bumped.counter, 1);
        assert_eq!(bumped.origin_node, "node_a");
        drop(store);

        let reopened = FjallSyncLedgerStore::open(dir.path()).unwrap();
        assert_eq!(reopened.current_epoch().unwrap(), bumped);
    }

    #[test]
    fn inbox_rejects_operation_from_other_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        // Local node advances past genesis, so a genesis-epoch operation from a
        // peer must be rejected as belonging to a stale baseline.
        store.bump_epoch("node_local").unwrap();
        let append = store
            .append_operation(sample_operation(&signer, "person_1"), &signer)
            .unwrap();
        let operation = store.get_operation(append.op_id).unwrap();

        let result = store.put_verified_in_inbox(
            PeerId::new("node_b").unwrap(),
            operation,
            &HexNodeIdOperationVerifier,
        );

        assert!(matches!(result, Err(SyncLedgerError::EpochMismatch { .. })));
    }

    #[test]
    fn environment_defaults_to_prod_and_persists_change() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();

        assert_eq!(store.current_environment().unwrap(), NodeEnvironment::Prod);

        store.set_environment(NodeEnvironment::Test).unwrap();
        assert_eq!(store.current_environment().unwrap(), NodeEnvironment::Test);
        drop(store);

        let reopened = FjallSyncLedgerStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened.current_environment().unwrap(),
            NodeEnvironment::Test
        );
    }

    /// ROADMAP Z12 hard guarantee: an operation from a peer declaring a
    /// DIFFERENT environment is rejected at admission, independently of
    /// epoch — same epoch, different environment must still be fenced.
    #[test]
    fn inbox_rejects_operation_from_other_environment() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        store.set_environment(NodeEnvironment::Test).unwrap();

        let mut op = sample_operation(&signer, "person_1");
        op.environment = NodeEnvironment::Prod;
        let append = store.append_operation(op, &signer).unwrap();
        let operation = store.get_operation(append.op_id).unwrap();

        let result = store.put_verified_in_inbox(
            PeerId::new("node_b").unwrap(),
            operation,
            &HexNodeIdOperationVerifier,
        );

        assert!(matches!(
            result,
            Err(SyncLedgerError::EnvironmentMismatch { .. })
        ));
    }

    /// ROADMAP Z12, N6(d) delta-review regression: a cross-environment
    /// operation carrying a HIGHER epoch than local must still be rejected as
    /// `EnvironmentMismatch`, never `EpochMismatch` — the environment check
    /// runs FIRST (P1-4), so this never reaches `note_epoch_mismatch` /
    /// `spawn_epoch_reconcile_adopts`, which would otherwise pull a full
    /// cross-environment baseline from the sender to "reconcile" the epoch.
    #[test]
    fn inbox_rejects_cross_environment_operation_with_higher_epoch_as_environment_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        // Local stays on genesis epoch/Prod; the incoming op both mismatches
        // environment AND claims an epoch the local store would otherwise
        // treat as "the remote epoch wins, adopt from this peer".
        assert_eq!(store.current_epoch().unwrap().counter, 0);
        assert_eq!(store.current_environment().unwrap(), NodeEnvironment::Prod);

        let mut op = sample_operation(&signer, "person_1");
        op.environment = NodeEnvironment::Test;
        op.epoch = BaselineEpoch {
            counter: 5,
            origin_node: "node_b".to_string(),
        };
        let append = store.append_operation(op, &signer).unwrap();
        let operation = store.get_operation(append.op_id).unwrap();

        let result = store.put_verified_in_inbox(
            PeerId::new("node_b").unwrap(),
            operation,
            &HexNodeIdOperationVerifier,
        );

        match result {
            Err(SyncLedgerError::EnvironmentMismatch { expected, actual }) => {
                assert_eq!(expected, NodeEnvironment::Prod);
                assert_eq!(actual, NodeEnvironment::Test);
            }
            other => panic!("expected EnvironmentMismatch, got {other:?}"),
        }
    }

    /// Same-environment sync must go through unimpeded — the fence rejects
    /// only a MISMATCH, not every foreign operation.
    #[test]
    fn inbox_accepts_operation_from_same_environment() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        store.set_environment(NodeEnvironment::Test).unwrap();

        let mut op = sample_operation(&signer, "person_1");
        op.environment = NodeEnvironment::Test;
        let append = store.append_operation(op, &signer).unwrap();
        let operation = store.get_operation(append.op_id).unwrap();

        let result = store.put_verified_in_inbox(
            PeerId::new("node_b").unwrap(),
            operation,
            &HexNodeIdOperationVerifier,
        );

        assert!(result.is_ok());
    }

    /// ROADMAP Z12, P1-2 coordination decision: upgrading an EXISTING
    /// all-Prod mesh must not break sync. Neither side here ever calls
    /// `set_environment` — both the local store and the peer's operation
    /// decode to `NodeEnvironment::default()` (Prod), exactly what a
    /// pre-Z12 node and a pre-Z12-minted operation look like after a binary
    /// upgrade with no admin action taken yet. Admission must succeed.
    #[test]
    fn inbox_accepts_pre_z12_default_environment_across_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        assert_eq!(store.current_environment().unwrap(), NodeEnvironment::Prod);
        let signer = signer();

        // No explicit `environment` assignment — relies on `sample_operation`'s
        // own `NodeEnvironment::default()`, the same value a pre-Z12 wire
        // operation decodes to via `SyncOperationBody`'s `#[serde(default)]`.
        let op = sample_operation(&signer, "person_1");
        assert_eq!(op.environment, NodeEnvironment::Prod);
        let append = store.append_operation(op, &signer).unwrap();
        let operation = store.get_operation(append.op_id).unwrap();

        let result = store.put_verified_in_inbox(
            PeerId::new("node_b").unwrap(),
            operation,
            &HexNodeIdOperationVerifier,
        );

        assert!(
            result.is_ok(),
            "an existing Prod-Prod mesh must keep syncing across the v25->v26 upgrade"
        );
    }

    #[test]
    fn hlc_state_round_trips_through_meta() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        assert!(store.current_hlc().unwrap().is_none());

        let ts = HybridLogicalTimestamp {
            wall_time_ms: 1_765_000_000_123,
            logical: 9,
            node_id: "node_a".to_string(),
        };
        store.save_hlc(&ts).unwrap();
        drop(store);

        let reopened = FjallSyncLedgerStore::open(dir.path()).unwrap();
        assert_eq!(reopened.current_hlc().unwrap(), Some(ts));
    }

    #[test]
    fn remove_outbox_entry_drops_single_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        let append = store
            .append_operation(sample_operation(&signer, "person_1"), &signer)
            .unwrap();
        let target = SyncTarget::new("node_b").unwrap();
        store.put_in_outbox(target.clone(), append.op_id).unwrap();

        store
            .remove_outbox_entry(target.clone(), append.op_id)
            .unwrap();

        assert!(store
            .get_outbox_entry(target.clone(), append.op_id)
            .is_err());
        // Removing an absent key is a no-op, not an error.
        store.remove_outbox_entry(target, append.op_id).unwrap();
    }
}

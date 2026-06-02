// =============================================================================
// Plik: sync/ledger/fjall_store.rs
// Opis: Implementacja SyncLedgerStore oparta o Fjall i partycjonowane keyspace'y.
// =============================================================================

use super::types::{
    AppendResult, CompactionPolicy, InboxEntry, LedgerResult, NewSyncOperation, OperationId,
    OperationQuery, OutboxEntry, PartitionHead, PartitionId, PeerCursor, PeerId, RepairQueueEntry,
    SnapshotId, SyncLedgerError, SyncLedgerStore, SyncOperation, SyncOperationSigner,
    SyncOperationVerifier, SyncSnapshot, SyncTarget, decode, encode,
};
use fjall::{Database, Keyspace, KeyspaceCreateOptions, PersistMode};
use parking_lot::Mutex;
use std::collections::HashSet;
use std::path::Path;

const OPERATIONS: &str = "operations";
const OPERATION_INDEX: &str = "operation_index";
const PARTITION_HEADS: &str = "partition_heads";
const OUTBOX: &str = "outbox";
const INBOX: &str = "inbox";
const PEER_CURSORS: &str = "peer_cursors";
const REPAIR_QUEUE: &str = "repair_queue";
const SNAPSHOTS: &str = "snapshots";
const SEP: u8 = 0;

pub struct FjallSyncLedgerStore {
    db: Database,
    operations: Keyspace,
    operation_index: Keyspace,
    partition_heads: Keyspace,
    outbox: Keyspace,
    inbox: Keyspace,
    peer_cursors: Keyspace,
    repair_queue: Keyspace,
    snapshots: Keyspace,
    append_lock: Mutex<()>,
}

impl FjallSyncLedgerStore {
    pub fn open(path: impl AsRef<Path>) -> LedgerResult<Self> {
        let db = Database::builder(path).open()?;
        Ok(Self {
            operations: db.keyspace(OPERATIONS, KeyspaceCreateOptions::default)?,
            operation_index: db.keyspace(OPERATION_INDEX, KeyspaceCreateOptions::default)?,
            partition_heads: db.keyspace(PARTITION_HEADS, KeyspaceCreateOptions::default)?,
            outbox: db.keyspace(OUTBOX, KeyspaceCreateOptions::default)?,
            inbox: db.keyspace(INBOX, KeyspaceCreateOptions::default)?,
            peer_cursors: db.keyspace(PEER_CURSORS, KeyspaceCreateOptions::default)?,
            repair_queue: db.keyspace(REPAIR_QUEUE, KeyspaceCreateOptions::default)?,
            snapshots: db.keyspace(SNAPSHOTS, KeyspaceCreateOptions::default)?,
            db,
            append_lock: Mutex::new(()),
        })
    }

    fn persist(&self) -> LedgerResult<()> {
        Ok(self.db.persist(PersistMode::SyncAll)?)
    }

    fn load_partition_head(&self, partition: &PartitionId) -> LedgerResult<Option<PartitionHead>> {
        match self.partition_heads.get(partition.as_str())? {
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
        let partition_id = operation.partition_id.clone();
        let previous_head = self.load_partition_head(&partition_id)?;
        let previous_hash = previous_head.as_ref().map(|head| head.last_hash);
        let partition_sequence = previous_head
            .as_ref()
            .map_or(1, |head| head.last_sequence.saturating_add(1));
        let operation =
            SyncOperation::from_new(operation, partition_sequence, previous_hash, signer)?;
        operation.validate_integrity()?;
        let head = PartitionHead {
            partition_id: partition_id.clone(),
            last_sequence: partition_sequence,
            last_hash: operation.operation_hash,
        };

        let operation_key = operation_key(&partition_id, partition_sequence);
        let mut batch = self.db.batch().durability(Some(PersistMode::SyncAll));
        batch.insert(&self.operations, operation_key.clone(), encode(&operation)?);
        batch.insert(
            &self.operation_index,
            operation.op_id.as_bytes().to_vec(),
            operation_key,
        );
        batch.insert(&self.partition_heads, partition_id.as_str(), encode(&head)?);
        batch.commit()?;

        Ok(AppendResult {
            op_id: operation.op_id,
            operation_hash: operation.operation_hash,
            previous_partition_hash: previous_hash,
            partition_sequence,
        })
    }

    fn get_operations(&self, query: OperationQuery) -> LedgerResult<Vec<SyncOperation>> {
        let mut operations = Vec::new();
        let prefix = partition_prefix(&query.partition_id);
        for item in self.operations.prefix(&prefix) {
            let (key, value) = item.into_inner()?;
            if let Some(sequence) = sequence_from_operation_key(key.as_ref()) {
                if query.from_sequence.is_some_and(|from| sequence < from) {
                    continue;
                }
                if query.to_sequence.is_some_and(|to| sequence > to) {
                    continue;
                }
                operations.push(decode(value.as_ref())?);
                if query.limit.is_some_and(|limit| operations.len() >= limit) {
                    break;
                }
            }
        }
        Ok(operations)
    }

    fn get_operation(&self, op_id: OperationId) -> LedgerResult<SyncOperation> {
        let key = self
            .operation_index
            .get(op_id.as_bytes().to_vec())?
            .ok_or(SyncLedgerError::OperationNotFound(op_id))?;
        let operation = self
            .operations
            .get(key.as_ref())?
            .ok_or(SyncLedgerError::OperationNotFound(op_id))?;
        decode(operation.as_ref())
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
        if self
            .inbox
            .get(inbox_key(&source, operation.op_id))?
            .is_some()
        {
            return Ok(());
        }
        let entry = InboxEntry {
            source: source.clone(),
            operation,
            applied: false,
            conflicted: false,
            conflict_message: None,
        };
        self.inbox
            .insert(inbox_key(&source, entry.operation.op_id), encode(&entry)?)?;
        self.persist()
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
        entry.conflict_message = Some(message);
        self.inbox
            .insert(inbox_key(&source, op_id), encode(&entry)?)?;
        self.persist()
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

    fn get_peer_cursor(
        &self,
        peer: PeerId,
        partition: PartitionId,
    ) -> LedgerResult<Option<PeerCursor>> {
        match self.peer_cursors.get(peer_cursor_key(&peer, &partition))? {
            Some(value) => Ok(Some(decode(value.as_ref())?)),
            None => Ok(None),
        }
    }

    fn save_peer_cursor(&self, cursor: PeerCursor) -> LedgerResult<()> {
        self.peer_cursors.insert(
            peer_cursor_key(&cursor.peer, &cursor.partition_id),
            encode(&cursor)?,
        )?;
        self.persist()
    }

    fn upsert_repair_request(&self, entry: RepairQueueEntry) -> LedgerResult<()> {
        let key = repair_queue_key(&entry.peer, &entry.partition_id);
        let entry = match self.repair_queue.get(&key)? {
            Some(value) => {
                let mut existing: RepairQueueEntry = decode(value.as_ref())?;
                if entry.from_sequence < existing.from_sequence {
                    existing.from_sequence = entry.from_sequence;
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
        partition: PartitionId,
        next_attempt_ms: i64,
        retry_count: u32,
    ) -> LedgerResult<()> {
        let key = repair_queue_key(&peer, &partition);
        if let Some(value) = self.repair_queue.get(&key)? {
            let mut entry: RepairQueueEntry = decode(value.as_ref())?;
            entry.next_attempt_ms = next_attempt_ms;
            entry.retry_count = retry_count;
            self.repair_queue.insert(key, encode(&entry)?)?;
            self.persist()?;
        }
        Ok(())
    }

    fn remove_repair_request(&self, peer: PeerId, partition: PartitionId) -> LedgerResult<()> {
        self.repair_queue
            .remove(repair_queue_key(&peer, &partition))?;
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

    fn get_partition_head(&self, partition: PartitionId) -> LedgerResult<Option<PartitionHead>> {
        self.load_partition_head(&partition)
    }

    fn list_outbox_for_partition(
        &self,
        partition: PartitionId,
        up_to_sequence: u64,
    ) -> LedgerResult<Vec<OutboxEntry>> {
        let mut entries = Vec::new();
        for item in self.outbox.iter() {
            let (_, value) = item.into_inner()?;
            let entry: OutboxEntry = decode(value.as_ref())?;
            let Ok(operation) = self.get_operation(entry.op_id) else {
                continue;
            };
            if operation.body.partition_id == partition
                && operation.body.partition_sequence <= up_to_sequence
            {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    fn compact(&self, policy: CompactionPolicy) -> LedgerResult<()> {
        let Some(keep_after_sequence) = policy.keep_operations_after_sequence else {
            return Ok(());
        };

        let mut batch = self.db.batch().durability(Some(PersistMode::SyncAll));
        let prefix = partition_prefix(&policy.partition_id);
        for item in self.operations.prefix(&prefix) {
            let (key, value) = item.into_inner()?;
            if sequence_from_operation_key(key.as_ref())
                .is_some_and(|seq| seq < keep_after_sequence)
            {
                let operation: SyncOperation = decode(value.as_ref())?;
                batch.remove(&self.operation_index, operation.op_id.as_bytes().to_vec());
                batch.remove(&self.operations, key.to_vec());
            }
        }
        batch.commit()?;
        Ok(())
    }

    fn reset_partitions_with_prefix(&self, partition_prefix: &str) -> LedgerResult<()> {
        let _guard = self.append_lock.lock();
        let prefix_bytes = partition_prefix.as_bytes();

        let mut batch = self.db.batch().durability(Some(PersistMode::SyncAll));

        // Operations + their op_id index. The operation key starts with the
        // partition_id, so a byte-prefix match selects exactly the partitions
        // under `partition_prefix`. Collect the matching op_ids while we are
        // here so outbox/inbox cleanup below can match without re-reading.
        let mut matched_op_ids: HashSet<OperationId> = HashSet::new();
        for item in self.operations.prefix(prefix_bytes) {
            let (key, value) = item.into_inner()?;
            let operation: SyncOperation = decode(value.as_ref())?;
            batch.remove(&self.operation_index, operation.op_id.as_bytes().to_vec());
            batch.remove(&self.operations, key.to_vec());
            matched_op_ids.insert(operation.op_id);
        }

        // Snapshots are keyed by partition_prefix too.
        for item in self.snapshots.prefix(prefix_bytes) {
            let (key, _) = item.into_inner()?;
            batch.remove(&self.snapshots, key.to_vec());
        }

        // Partition heads are keyed directly by partition_id.
        for item in self.partition_heads.prefix(prefix_bytes) {
            let (key, _) = item.into_inner()?;
            batch.remove(&self.partition_heads, key.to_vec());
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

        // Peer cursors and repair queue are keyed by (peer, partition) but the
        // partition is concatenated raw, so decode the value to be safe.
        for item in self.peer_cursors.iter() {
            let (key, value) = item.into_inner()?;
            let cursor: PeerCursor = decode(value.as_ref())?;
            if cursor.partition_id.as_str().starts_with(partition_prefix) {
                batch.remove(&self.peer_cursors, key.to_vec());
            }
        }

        for item in self.repair_queue.iter() {
            let (key, value) = item.into_inner()?;
            let entry: RepairQueueEntry = decode(value.as_ref())?;
            if entry.partition_id.as_str().starts_with(partition_prefix) {
                batch.remove(&self.repair_queue, key.to_vec());
            }
        }

        batch.commit()?;
        Ok(())
    }
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

fn operation_key(partition: &PartitionId, sequence: u64) -> Vec<u8> {
    let mut key = partition_prefix(partition);
    key.extend_from_slice(&sequence.to_be_bytes());
    key
}

fn sequence_from_operation_key(key: &[u8]) -> Option<u64> {
    let bytes: [u8; 8] = key.get(key.len().checked_sub(8)?..)?.try_into().ok()?;
    Some(u64::from_be_bytes(bytes))
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

fn peer_cursor_key(peer: &PeerId, partition: &PartitionId) -> Vec<u8> {
    let mut key = peer.as_str().as_bytes().to_vec();
    key.push(SEP);
    key.extend_from_slice(partition.as_str().as_bytes());
    key
}

fn repair_queue_key(peer: &PeerId, partition: &PartitionId) -> Vec<u8> {
    peer_cursor_key(peer, partition)
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
        ActionType, Ed25519OperationSigner, FieldValue, HexNodeIdOperationVerifier,
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

        assert_eq!(first.partition_sequence, 1);
        assert_eq!(second.partition_sequence, 2);
        assert_eq!(second.previous_partition_hash, Some(first.operation_hash));
    }

    #[test]
    fn get_operations_returns_partition_range() {
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
            .get_operations(OperationQuery {
                partition_id: PartitionId::new("addon/contacts/persons").unwrap(),
                from_sequence: Some(2),
                to_sequence: Some(2),
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
    fn peer_cursor_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let partition = PartitionId::new("addon/contacts/persons").unwrap();
        let peer = PeerId::new("node_b").unwrap();
        let cursor = PeerCursor {
            peer: peer.clone(),
            partition_id: partition.clone(),
            last_sequence: 7,
            last_hash: [9; 32],
        };

        store.save_peer_cursor(cursor.clone()).unwrap();
        let loaded = store.get_peer_cursor(peer, partition).unwrap();

        assert_eq!(loaded, Some(cursor));
    }

    #[test]
    fn repair_queue_persists_due_requests() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let peer = PeerId::new("node_b").unwrap();
        let partition = PartitionId::new("addon/contacts/persons").unwrap();
        store
            .upsert_repair_request(RepairQueueEntry {
                peer: peer.clone(),
                partition_id: partition.clone(),
                from_sequence: 4,
                next_attempt_ms: 100,
                retry_count: 0,
            })
            .unwrap();
        store
            .mark_repair_attempted(peer.clone(), partition.clone(), 500, 1)
            .unwrap();
        drop(store);

        let reopened = FjallSyncLedgerStore::open(dir.path()).unwrap();
        assert!(
            reopened
                .list_due_repair_requests(peer.clone(), 200, 10)
                .unwrap()
                .is_empty()
        );
        let due = reopened
            .list_due_repair_requests(peer.clone(), 500, 10)
            .unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].from_sequence, 4);
        assert_eq!(due[0].retry_count, 1);

        reopened
            .remove_repair_request(peer.clone(), partition.clone())
            .unwrap();
        assert!(
            reopened
                .list_due_repair_requests(peer, 1_000, 10)
                .unwrap()
                .is_empty()
        );
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

        // Populate outbox / inbox / cursors / repair for both core and non-core.
        let target = SyncTarget::new("node_b").unwrap();
        store.put_in_outbox(target.clone(), core_append.op_id).unwrap();
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

        for partition in [core_partition, addon_partition] {
            store
                .save_peer_cursor(PeerCursor {
                    peer: source.clone(),
                    partition_id: PartitionId::new(partition).unwrap(),
                    last_sequence: 1,
                    last_hash: [3; 32],
                })
                .unwrap();
            store
                .upsert_repair_request(RepairQueueEntry {
                    peer: source.clone(),
                    partition_id: PartitionId::new(partition).unwrap(),
                    from_sequence: 1,
                    next_attempt_ms: 0,
                    retry_count: 0,
                })
                .unwrap();
        }

        store.reset_core_partitions().unwrap();

        // Core operations gone, addon + kv intact.
        assert!(
            store
                .get_operations(OperationQuery {
                    partition_id: PartitionId::new(core_partition).unwrap(),
                    from_sequence: None,
                    to_sequence: None,
                    limit: None,
                })
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .get_operations(OperationQuery {
                    partition_id: PartitionId::new(addon_partition).unwrap(),
                    from_sequence: None,
                    to_sequence: None,
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
                    from_sequence: None,
                    to_sequence: None,
                    limit: None,
                })
                .unwrap()
                .len(),
            1
        );

        // op_id index for core is gone; addon/kv still resolvable.
        assert!(store.get_operation(core_append.op_id).is_err());
        assert!(store.get_operation(addon_append.op_id).is_ok());
        assert!(store.get_operation(kv_append.op_id).is_ok());

        // Partition head only removed for core.
        assert!(
            store
                .get_partition_head(PartitionId::new(core_partition).unwrap())
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get_partition_head(PartitionId::new(addon_partition).unwrap())
                .unwrap()
                .is_some()
        );

        // Outbox: core entry removed, addon entry kept.
        assert!(
            store
                .get_outbox_entry(SyncTarget::new("node_b").unwrap(), core_append.op_id)
                .is_err()
        );
        assert!(
            store
                .get_outbox_entry(SyncTarget::new("node_b").unwrap(), addon_append.op_id)
                .is_ok()
        );

        // Inbox: only the addon-partition entry survives.
        let inbox = store.list_unapplied_inbox(10).unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(
            inbox[0].operation.body.partition_id.as_str(),
            addon_partition
        );

        // Peer cursor: core gone, addon kept.
        assert!(
            store
                .get_peer_cursor(
                    source.clone(),
                    PartitionId::new(core_partition).unwrap()
                )
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get_peer_cursor(source.clone(), PartitionId::new(addon_partition).unwrap())
                .unwrap()
                .is_some()
        );

        // Repair queue: only the addon-partition request remains due.
        let due = store
            .list_due_repair_requests(source, i64::MAX, 10)
            .unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].partition_id.as_str(), addon_partition);
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
        store.put_in_outbox(target.clone(), core_live.op_id).unwrap();
        store.put_in_outbox(target.clone(), core_orphan.op_id).unwrap();
        store.put_in_outbox(target.clone(), addon_orphan.op_id).unwrap();
        store.put_in_outbox(target.clone(), kv_append.op_id).unwrap();

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
        assert!(
            store
                .get_outbox_entry(SyncTarget::new("node_b").unwrap(), core_live.op_id)
                .is_err()
        );
        // (ii) Orphaned outbox entries survive the reset, whether their now-gone
        // operation was core OR addon — reset never deletes orphans.
        assert!(
            store
                .get_outbox_entry(SyncTarget::new("node_b").unwrap(), core_orphan.op_id)
                .is_ok()
        );
        assert!(
            store
                .get_outbox_entry(SyncTarget::new("node_b").unwrap(), addon_orphan.op_id)
                .is_ok()
        );
        // The live kv entry is untouched as well.
        assert!(
            store
                .get_outbox_entry(SyncTarget::new("node_b").unwrap(), kv_append.op_id)
                .is_ok()
        );
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

        assert!(store.get_outbox_entry(target.clone(), append.op_id).is_err());
        // Removing an absent key is a no-op, not an error.
        store.remove_outbox_entry(target, append.op_id).unwrap();
    }
}

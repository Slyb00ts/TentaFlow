// =============================================================================
// Plik: sync/compaction.rs
// Opis: Bezpieczne kompaktowanie Sync Ledger po utrwalonym snapshot package i ACK.
// =============================================================================

use crate::sync::ledger::{
    CompactionPolicy, LedgerResult, OperationQuery, OutboxEntry, PartitionId, SyncLedgerError,
    SyncLedgerStore, SyncSnapshot, SyncTarget,
};
use crate::sync::snapshot::{verify_snapshot_signature, SnapshotPackageStore};
use std::collections::{BTreeSet, HashSet};

pub struct CompactionManager<'a> {
    ledger: &'a dyn SyncLedgerStore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeCompactionRequest {
    pub partition_id: PartitionId,
    pub up_to_sequence: Option<u64>,
    pub finality: CompactionFinalityPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionFinalityPolicy {
    AllOutboxTargets,
    RequiredTargets(Vec<SyncTarget>),
    Quorum {
        eligible_targets: Vec<SyncTarget>,
        min_acks: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeCompactionResult {
    pub snapshot: SyncSnapshot,
    pub compacted_up_to_sequence: u64,
    pub finality: CompactionFinalityReport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionFinalityReport {
    pub required_operations: usize,
    pub acknowledged_targets: Vec<String>,
    pub blocking_targets: Vec<String>,
}

impl CompactionFinalityPolicy {
    pub fn all_outbox_targets() -> Self {
        Self::AllOutboxTargets
    }
}

impl<'a> CompactionManager<'a> {
    pub fn new(ledger: &'a dyn SyncLedgerStore) -> Self {
        Self { ledger }
    }

    pub fn compact_with_snapshot_package(
        &self,
        request: SafeCompactionRequest,
        package_store: &SnapshotPackageStore,
    ) -> LedgerResult<Option<SafeCompactionResult>> {
        let Some(snapshot) = self
            .ledger
            .latest_snapshot(request.partition_id.clone(), request.up_to_sequence)?
        else {
            return Ok(None);
        };
        verify_snapshot_signature(&snapshot)?;
        package_store.get_sql_package(&snapshot)?;
        // The watermark is a 1-based count over the HLC-ordered partition op set:
        // take that prefix for finality evaluation.
        let mut operations = self.ledger.get_operations(OperationQuery {
            partition_id: request.partition_id.clone(),
            limit: None,
        })?;
        operations.sort_by(crate::sync::ledger::partition_materialization_order);
        operations.truncate(snapshot.up_to_sequence as usize);
        let outbox = self
            .ledger
            .list_outbox_for_partition(request.partition_id.clone(), snapshot.up_to_sequence)?;
        let finality = evaluate_finality(&request.finality, &operations, &outbox)?;
        if !finality.blocking_targets.is_empty() {
            return Err(SyncLedgerError::Runtime(format!(
                "cannot compact {} up to {}: finality blocked by {}",
                request.partition_id.as_str(),
                snapshot.up_to_sequence,
                finality.blocking_targets.join(",")
            )));
        }
        self.ledger.compact(CompactionPolicy {
            partition_id: request.partition_id,
            keep_operations_after_sequence: Some(snapshot.up_to_sequence.saturating_add(1)),
        })?;
        Ok(Some(SafeCompactionResult {
            compacted_up_to_sequence: snapshot.up_to_sequence,
            snapshot,
            finality,
        }))
    }
}

fn evaluate_finality(
    policy: &CompactionFinalityPolicy,
    operations: &[crate::sync::ledger::SyncOperation],
    outbox: &[OutboxEntry],
) -> LedgerResult<CompactionFinalityReport> {
    // Only LOCALLY-minted ops carry a delivery obligation: the local node enqueues
    // an outbox entry for each op it mints, never for a foreign op it merely
    // received and re-indexed into this partition. A foreign op is by definition
    // already delivered (it reached us from its author), so it must NOT count
    // toward the ack requirement — otherwise a partition that mixes foreign ops
    // (per-node hash-chains let many nodes write one partition) could never reach
    // finality (`target_entries.len()` would never match a count inflated by
    // un-deliverable foreign ops). Obligation = "has at least one outbox entry".
    let obligated_op_ids = outbox
        .iter()
        .map(|entry| entry.op_id.to_hex())
        .collect::<HashSet<_>>();
    let operation_ids = operations
        .iter()
        .map(|operation| operation.op_id.to_hex())
        .filter(|op_id| obligated_op_ids.contains(op_id))
        .collect::<HashSet<_>>();
    let required_operations = operation_ids.len();
    let required_targets = match policy {
        CompactionFinalityPolicy::AllOutboxTargets => outbox
            .iter()
            .map(|entry| entry.target.as_str().to_string())
            .collect::<BTreeSet<_>>(),
        CompactionFinalityPolicy::RequiredTargets(targets) => targets
            .iter()
            .map(|target| target.as_str().to_string())
            .collect::<BTreeSet<_>>(),
        CompactionFinalityPolicy::Quorum {
            eligible_targets,
            min_acks,
        } => {
            if *min_acks > eligible_targets.len() {
                return Err(SyncLedgerError::Runtime(format!(
                    "compaction quorum requires {min_acks} acknowledgements from {} targets",
                    eligible_targets.len()
                )));
            }
            eligible_targets
                .iter()
                .map(|target| target.as_str().to_string())
                .collect::<BTreeSet<_>>()
        }
    };
    let mut acknowledged_targets = Vec::new();
    let mut blocking_targets = Vec::new();
    for target in required_targets {
        let target_entries = outbox
            .iter()
            .filter(|entry| entry.target.as_str() == target)
            .collect::<Vec<_>>();
        let acknowledged = target_entries.len() == required_operations
            && target_entries
                .iter()
                .all(|entry| entry.acknowledged && operation_ids.contains(&entry.op_id.to_hex()));
        if acknowledged {
            acknowledged_targets.push(target);
        } else {
            blocking_targets.push(target);
        }
    }
    if let CompactionFinalityPolicy::Quorum { min_acks, .. } = policy {
        if acknowledged_targets.len() >= *min_acks {
            blocking_targets.clear();
        }
    }
    Ok(CompactionFinalityReport {
        required_operations,
        acknowledged_targets,
        blocking_targets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::ledger::{
        ActionType, Ed25519OperationSigner, FieldValue, FjallSyncLedgerStore,
        HexNodeIdOperationVerifier, HybridLogicalTimestamp, NewSyncOperation, NodeFrontierEntry,
        OperationQuery, PeerId, SyncOperation, SyncOperationSigner, SyncTarget,
    };
    use crate::sync::snapshot::{SnapshotBuildRequest, SnapshotManager, SnapshotPackageStore};
    use ed25519_dalek::SigningKey;
    use rand_core_06::OsRng;
    use std::collections::BTreeMap;

    fn signer() -> Ed25519OperationSigner {
        let signing_key = SigningKey::generate(&mut OsRng);
        let node_id = hex::encode(signing_key.verifying_key().to_bytes());
        Ed25519OperationSigner::new(node_id, signing_key).unwrap()
    }

    fn operation(
        signer: &Ed25519OperationSigner,
        partition: PartitionId,
        resource_id: &str,
    ) -> NewSyncOperation {
        let mut changed_fields = BTreeMap::new();
        changed_fields.insert(
            "sql".to_string(),
            FieldValue::String("INSERT INTO contacts (id, name) VALUES (?1, ?2)".to_string()),
        );
        changed_fields.insert(
            "params_json".to_string(),
            FieldValue::String("[1,\"Ewa\"]".to_string()),
        );
        changed_fields.insert("rows_affected".to_string(), FieldValue::U64(1));
        changed_fields.insert("last_insert_id".to_string(), FieldValue::I64(1));
        changed_fields.insert(
            "capture_id".to_string(),
            FieldValue::String(format!("capture-{resource_id}")),
        );
        NewSyncOperation {
            org_id: "org-default".to_string(),
            partition_id: partition,
            addon_id: "contacts".to_string(),
            resource_type: "person".to_string(),
            resource_id: resource_id.to_string(),
            table_name: "contacts".to_string(),
            primary_key: resource_id.to_string(),
            action: ActionType::Insert,
            changed_fields,
            before_hash: None,
            after_hash: Some([9; 32]),
            actor_user_id: "user_1".to_string(),
            actor_device_id: "device_1".to_string(),
            actor_node_id: signer.node_id().to_string(),
            hlc_timestamp: HybridLogicalTimestamp {
                wall_time_ms: 1_765_000_000_000,
                logical: 0,
                node_id: signer.node_id().to_string(),
            },
            epoch: crate::sync::ledger::BaselineEpoch {
                counter: 0,
                origin_node: String::new(),
            },
            payload_hash: [1; 32],
            acl_snapshot_hash: [2; 32],
            policy_epoch: 1,
            encryption_info: None,
        }
    }

    #[test]
    fn safe_compaction_requires_acknowledged_outbox() {
        let dir = tempfile::tempdir().unwrap();
        let blob_dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        let partition = PartitionId::new("addon/contacts/persons/1").unwrap();
        let first = store
            .append_operation(operation(&signer, partition.clone(), "1"), &signer)
            .unwrap();
        store
            .append_operation(operation(&signer, partition.clone(), "2"), &signer)
            .unwrap();
        let package_store = SnapshotPackageStore::new(blob_dir.path());
        SnapshotManager::new(&store)
            .build_sql_package_and_persist(
                SnapshotBuildRequest {
                    partition_id: partition.clone(),
                    up_to_sequence: Some(1),
                    created_at_ms: 1_765_000_000_100,
                },
                &signer,
                &package_store,
            )
            .unwrap();
        let target = SyncTarget::new("node-b").unwrap();
        store.put_in_outbox(target.clone(), first.op_id).unwrap();

        let result = CompactionManager::new(&store).compact_with_snapshot_package(
            SafeCompactionRequest {
                partition_id: partition.clone(),
                up_to_sequence: Some(1),
                finality: CompactionFinalityPolicy::all_outbox_targets(),
            },
            &package_store,
        );
        assert!(result.is_err());

        store.mark_acknowledged(target, first.op_id).unwrap();
        let result = CompactionManager::new(&store)
            .compact_with_snapshot_package(
                SafeCompactionRequest {
                    partition_id: partition.clone(),
                    up_to_sequence: Some(1),
                    finality: CompactionFinalityPolicy::all_outbox_targets(),
                },
                &package_store,
            )
            .unwrap()
            .unwrap();
        assert_eq!(result.compacted_up_to_sequence, 1);
        // Compaction dropped the first HLC-ordered op (the snapshot prefix),
        // leaving exactly one live operation in the partition.
        let remaining = store
            .get_operations(OperationQuery {
                partition_id: partition,
                limit: None,
            })
            .unwrap();
        assert_eq!(remaining.len(), 1);
    }

    #[test]
    fn compaction_required_targets_block_missing_ack() {
        let dir = tempfile::tempdir().unwrap();
        let blob_dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        let partition = PartitionId::new("addon/contacts/persons/required").unwrap();
        let first = store
            .append_operation(operation(&signer, partition.clone(), "1"), &signer)
            .unwrap();
        let package_store = SnapshotPackageStore::new(blob_dir.path());
        SnapshotManager::new(&store)
            .build_sql_package_and_persist(
                SnapshotBuildRequest {
                    partition_id: partition.clone(),
                    up_to_sequence: Some(1),
                    created_at_ms: 1_765_000_000_100,
                },
                &signer,
                &package_store,
            )
            .unwrap();
        let node_b = SyncTarget::new("node-b").unwrap();
        let node_c = SyncTarget::new("node-c").unwrap();
        store.put_in_outbox(node_b.clone(), first.op_id).unwrap();
        store
            .mark_acknowledged(node_b.clone(), first.op_id)
            .unwrap();

        let result = CompactionManager::new(&store).compact_with_snapshot_package(
            SafeCompactionRequest {
                partition_id: partition,
                up_to_sequence: Some(1),
                finality: CompactionFinalityPolicy::RequiredTargets(vec![node_b, node_c]),
            },
            &package_store,
        );

        assert!(result.is_err());
    }

    #[test]
    fn compaction_quorum_allows_subset_of_targets() {
        let dir = tempfile::tempdir().unwrap();
        let blob_dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        let partition = PartitionId::new("addon/contacts/persons/quorum").unwrap();
        let first = store
            .append_operation(operation(&signer, partition.clone(), "1"), &signer)
            .unwrap();
        let package_store = SnapshotPackageStore::new(blob_dir.path());
        SnapshotManager::new(&store)
            .build_sql_package_and_persist(
                SnapshotBuildRequest {
                    partition_id: partition.clone(),
                    up_to_sequence: Some(1),
                    created_at_ms: 1_765_000_000_100,
                },
                &signer,
                &package_store,
            )
            .unwrap();
        let node_b = SyncTarget::new("node-b").unwrap();
        let node_c = SyncTarget::new("node-c").unwrap();
        store.put_in_outbox(node_b.clone(), first.op_id).unwrap();
        store.put_in_outbox(node_c.clone(), first.op_id).unwrap();
        store
            .mark_acknowledged(node_b.clone(), first.op_id)
            .unwrap();

        let result = CompactionManager::new(&store)
            .compact_with_snapshot_package(
                SafeCompactionRequest {
                    partition_id: partition,
                    up_to_sequence: Some(1),
                    finality: CompactionFinalityPolicy::Quorum {
                        eligible_targets: vec![node_b, node_c],
                        min_acks: 1,
                    },
                },
                &package_store,
            )
            .unwrap()
            .unwrap();

        assert_eq!(result.finality.acknowledged_targets, vec!["node-b"]);
    }

    #[test]
    fn compaction_reaches_finality_with_foreign_ops_in_partition() {
        // CR-W2: per-node hash-chains let MANY nodes write one partition, so after
        // a foreign op is admitted it sits in `get_operations(partition)` but has
        // NO outbox entry (the outbox only tracks locally-minted ops we owe peers).
        // Finality must not count that foreign op toward the ack requirement —
        // otherwise the partition could never compact. Here a local op (acked) and
        // a foreign op share one partition; compaction must succeed.
        let dir = tempfile::tempdir().unwrap();
        let blob_dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let local = signer();
        let foreign = signer();
        let partition = PartitionId::new("addon/contacts/persons/mixed").unwrap();

        // Local mint (node_seq 1 on the local chain) — gets an outbox obligation.
        let local_op = store
            .append_operation(operation(&local, partition.clone(), "local-1"), &local)
            .unwrap();

        // Foreign op authored by another node, admitted into the SAME partition. It
        // lands in operations/partition_index but carries no outbox entry.
        let foreign_new = operation(&foreign, partition.clone(), "foreign-1");
        let foreign_op = SyncOperation::from_new(foreign_new, 1, None, &foreign).unwrap();
        store
            .admit_verified_operation(
                PeerId::new("peer-foreign").unwrap(),
                foreign_op.clone(),
                NodeFrontierEntry {
                    node_id: foreign.node_id().to_string(),
                    last_seq: 1,
                    last_hash: foreign_op.operation_hash,
                },
                &HexNodeIdOperationVerifier,
            )
            .unwrap();

        // Both ops are in the partition view now.
        assert_eq!(
            store
                .get_operations(OperationQuery {
                    partition_id: partition.clone(),
                    limit: None,
                })
                .unwrap()
                .len(),
            2
        );

        let package_store = SnapshotPackageStore::new(blob_dir.path());
        SnapshotManager::new(&store)
            .build_sql_package_and_persist(
                SnapshotBuildRequest {
                    partition_id: partition.clone(),
                    up_to_sequence: Some(2),
                    created_at_ms: 1_765_000_000_100,
                },
                &local,
                &package_store,
            )
            .unwrap();

        // The local op is delivered + acked; the foreign op is already-delivered by
        // definition (it reached us from its author) and must not block.
        let target = SyncTarget::new("node-b").unwrap();
        store.put_in_outbox(target.clone(), local_op.op_id).unwrap();
        store.mark_acknowledged(target, local_op.op_id).unwrap();

        let result = CompactionManager::new(&store)
            .compact_with_snapshot_package(
                SafeCompactionRequest {
                    partition_id: partition,
                    up_to_sequence: Some(2),
                    finality: CompactionFinalityPolicy::all_outbox_targets(),
                },
                &package_store,
            )
            .unwrap();
        assert!(
            result.is_some(),
            "partition with a foreign op must still reach finality"
        );
    }

    #[test]
    fn compaction_then_catchup_keeps_node_log_for_equivocation_check() {
        // After a snapshot covers a writer's frontier and the prefix is compacted,
        // the per-partition `operations`/`partition_index` rows are gone but the
        // per-node chain axis (`node_log`) MUST survive: it is what equivocation
        // detection and per-node catch-up pulls read. A peer whose frontier sits
        // below the compacted floor is served the snapshot + tail, never a hole.
        let dir = tempfile::tempdir().unwrap();
        let blob_dir = tempfile::tempdir().unwrap();
        let store = FjallSyncLedgerStore::open(dir.path()).unwrap();
        let signer = signer();
        let partition = PartitionId::new("addon/contacts/persons/catchup").unwrap();
        let first = store
            .append_operation(operation(&signer, partition.clone(), "1"), &signer)
            .unwrap();
        store
            .append_operation(operation(&signer, partition.clone(), "2"), &signer)
            .unwrap();
        let package_store = SnapshotPackageStore::new(blob_dir.path());
        let snapshot = SnapshotManager::new(&store)
            .build_sql_package_and_persist(
                SnapshotBuildRequest {
                    partition_id: partition.clone(),
                    up_to_sequence: Some(1),
                    created_at_ms: 1_765_000_000_100,
                },
                &signer,
                &package_store,
            )
            .unwrap()
            .expect("persisted snapshot package")
            .snapshot;
        // The snapshot's attested frontier covers exactly node_seq=1 for the writer.
        assert_eq!(snapshot.node_frontier.len(), 1);
        assert_eq!(snapshot.node_frontier[signer.node_id()].0, 1);

        let target = SyncTarget::new("node-b").unwrap();
        store.put_in_outbox(target.clone(), first.op_id).unwrap();
        store.mark_acknowledged(target, first.op_id).unwrap();
        CompactionManager::new(&store)
            .compact_with_snapshot_package(
                SafeCompactionRequest {
                    partition_id: partition.clone(),
                    up_to_sequence: Some(1),
                    finality: CompactionFinalityPolicy::all_outbox_targets(),
                },
                &package_store,
            )
            .unwrap()
            .unwrap();

        // The per-partition view dropped the compacted prefix op...
        let live = store
            .get_operations(OperationQuery {
                partition_id: partition.clone(),
                limit: None,
            })
            .unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].body.node_seq, 2);
        // ...but the node_log retains seq=1 so equivocation at that seq is still
        // detectable and a peer below the floor can be caught up.
        assert_eq!(
            store.get_node_log_entry(signer.node_id(), 1).unwrap(),
            Some(first.op_id)
        );
        assert!(store
            .get_node_log_entry(signer.node_id(), 2)
            .unwrap()
            .is_some());
    }
}

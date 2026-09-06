// =============================================================================
// File: bus/replication/assignment.rs — PartitionAssignment (PLAN-M2 §1c)
// =============================================================================
//
// Wave 0 (coordinator) froze `PartitionAssignment` itself (fields below are
// NOT touched by wave 1 — every other agent building against this file
// compiled against this exact shape from day one).
//
// Wave 1 (agent L) adds:
//   - `PartitionAssignment ↔ DbBusPartitionAssignment` conversions.
//   - `SqliteLedgerAssignmentStore`: `get`/`list_for_topic`/`list_for_node`
//     are plain reads through `db::repository`'s `bus_assignment_*`
//     functions. `propose` is the ONLY write path (K-M2-4: capture ->
//     ledger -> `core_materializer::apply_bus_partition_assignment`, never
//     a direct SQL write) — it mints a `core.bus_partition_assignment`
//     ledger op via the SAME immediate-publish path `sync::core_capture`
//     itself uses (`sync::runtime::record_core_capture`, `core_capture.rs`
//     :86's pattern), NOT the deferred/journal `record_core_capture_tx`
//     helper in `db/repository.rs` (that one exists for writers holding an
//     open SQLite transaction; `propose` has none — it IS the write, not a
//     side effect of one).
//   - `admitted_by`: majority-observability for the election path (PLAN-M2
//     §1c: `OutboxEntry`/`list_outbox_for_operation` "already exists,
//     nothing to build" at the ledger-store level, `sync/ledger/
//     fjall_store.rs:414-466,765-785`) — reachable from here only through
//     two small additive wrappers added to `sync/runtime.rs`
//     (`get_operation`, `acknowledged_outbox_targets`), mirroring the
//     already-existing `acknowledged_outbox_count` exactly. `sync/
//     runtime.rs` is outside this task's exclusive file list, but
//     `SyncRuntime`'s `ledger`/`db` fields are private with no other public
//     accessor, so reaching the primitive the plan already promises is
//     available requires them; flagged for coordinator review.
//
// `manager.rs` (agent EL)'s `AssignmentStore` trait does not exist in this
// build yet (PLAN-M2 §1b: `bus/replication/manager.rs` is EL's file).
// `SqliteLedgerAssignmentStore` exposes the four methods PLAN-M2 §1c
// specifies (`get`, `list_for_topic`, `list_for_node`, `propose`) plus
// `admitted_by` as INHERENT methods with matching signatures, so `impl
// AssignmentStore for SqliteLedgerAssignmentStore { .. }` is a one-line
// forwarding impl once that trait lands.
//
// K-M2-4 (PLAN-M2 §0): `PartitionAssignment` is the LEDGER resource's domain
// shape — the `bus_partition_assignments` SQLite table (migration v144) is
// a MATERIALIZATION of it, not a second source of truth. Every field here
// mirrors a `bus_partition_assignments` column 1:1 EXCEPT `environment`
// (see `PartitionAssignment::to_db_row`'s doc for why that one is not
// carried on this type).
//
// plan-app-platform §1.4/W3: `instance_id` was added to `PartitionAssignment`
// and threaded through `assignment_resource_id`/`to_db_row`/`From<DbBus
// PartitionAssignment>` (so the materializer's `expected_id` guard and the
// repository row round-trip real per-instance rows). `SqliteLedgerAssignment
// Store`'s own PUBLIC methods (`get`/`list_for_topic`/`list_for_node`/
// `propose`) were deliberately NOT given an instance parameter this wave:
// `AssignmentStore` (`manager.rs`, wave 1 agent EL) is a frozen trait these
// methods satisfy structurally, and `manager.rs`/`election.rs`/`glue.rs`
// (EL's files) are out of W3's file list. Every one of this file's own
// methods stamps `bus::instance::LEGACY_SINGLE_INSTANCE` instead. W4 MUST:
// add an instance parameter to the `AssignmentStore` trait itself, to
// `SqliteLedgerAssignmentStore`'s and `FakeAssignmentStore`'s (`manager.rs`
// test double) implementations, and to every `PartitionAssignment { .. }`
// literal in `manager.rs`/`election.rs`/`glue.rs`/`bus/mod.rs` that still
// stamps the placeholder.

use serde::{Deserialize, Serialize};

use crate::db::repository::DbBusPartitionAssignment;
use crate::db::DbPool;
use crate::sync::ledger::OperationId;

/// One partition's leader/replica/ISR assignment, as recorded in the sync
/// ledger (PLAN-M2 §1c). `isr` is the only field NOT written to the ledger
/// on every change (PLAN-M2 §2's note: it is a fast-changing snapshot,
/// pushed to the ledger only alongside a leader or replica-set change,
/// never on its own) — kept here anyway because the materialized SQLite
/// row (and the M06 UI it feeds) needs the last-known ISR regardless of
/// which event produced the row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionAssignment {
    /// plan-app-platform §1.4/W3: the TentaBus instance this assignment
    /// belongs to — see `db::repository::DbBusPartitionAssignment::
    /// instance_id`'s doc for the type-choice rationale. `SqliteLedgerAssignmentStore`'s
    /// own methods (`get`/`list_for_topic`/`list_for_node`/`propose`) do NOT
    /// take an instance parameter yet (`AssignmentStore` is wave 1 agent
    /// EL's frozen trait, out of this wave's file list) — every one of them
    /// stamps `bus::instance::LEGACY_SINGLE_INSTANCE` here internally. See
    /// that constant's doc for the full W3->W4 bridge, and this module's
    /// header comment for exactly which callers W4 must revisit.
    pub instance_id: String,
    pub org_id: String,
    pub topic: String,
    pub partition: u32,
    pub leader_node_id: String,
    /// The full replica set (RF nodes), stable order — NOT the ISR.
    pub replicas: Vec<String>,
    pub isr: Vec<String>,
    pub leader_epoch: u32,
    pub updated_at_ms: i64,
}

impl From<DbBusPartitionAssignment> for PartitionAssignment {
    fn from(row: DbBusPartitionAssignment) -> Self {
        PartitionAssignment {
            instance_id: row.instance_id,
            org_id: row.org_id,
            topic: row.topic,
            partition: row.partition,
            leader_node_id: row.leader_node_id,
            replicas: row.replicas,
            isr: row.isr,
            leader_epoch: row.leader_epoch,
            updated_at_ms: row.updated_at_ms,
        }
    }
}

impl PartitionAssignment {
    /// `bus_partition_assignments.environment` has no equivalent field on
    /// this frozen ledger type: `environment` is derived transitively from
    /// the topic's own row at materialization time
    /// (`core_materializer::apply_bus_partition_assignment`), not carried
    /// per-assignment — `create_topic` already fences which nodes may be
    /// assigned to a topic's own environment (PLAN-M2 §1b fencing point 1),
    /// so repeating it here would be a second copy of the same fact that
    /// could drift from the topic's own row. Callers building a
    /// `DbBusPartitionAssignment` directly (tests; any future direct-repair
    /// tooling — never the materializer itself, which reads the topic's
    /// row instead) must supply it explicitly.
    pub fn to_db_row(&self, environment: impl Into<String>) -> DbBusPartitionAssignment {
        DbBusPartitionAssignment {
            instance_id: self.instance_id.clone(),
            org_id: self.org_id.clone(),
            topic: self.topic.clone(),
            partition: self.partition,
            leader_node_id: self.leader_node_id.clone(),
            replicas: self.replicas.clone(),
            isr: self.isr.clone(),
            leader_epoch: self.leader_epoch,
            environment: environment.into(),
            updated_at_ms: self.updated_at_ms,
        }
    }
}

/// Resource id for one partition assignment — `org_id`/`topic`/`partition`
/// length-prefixed (`sync::resource_id::composite_resource_id`), the same
/// convention every other multi-column-keyed core resource uses. Shared by
/// `propose` (mints the op under this id) and
/// `core_materializer::apply_bus_partition_assignment` (recomputes it from
/// the payload and rejects a mismatch), so the two can never drift apart.
fn assignment_resource_id(instance_id: &str, org_id: &str, topic: &str, partition: u32) -> String {
    crate::sync::resource_id::composite_resource_id(&[
        instance_id,
        org_id,
        topic,
        &partition.to_string(),
    ])
}

/// Real, ledger-backed implementation of the assignment store PLAN-M2 §1c
/// describes. See this file's header comment for why `propose` goes
/// through `sync::runtime::record_core_capture` rather than any direct SQL
/// write, and why `admitted_by` needs two small `sync/runtime.rs` additions
/// to reach the ledger's outbox.
#[derive(Clone)]
pub struct SqliteLedgerAssignmentStore {
    pool: DbPool,
}

impl SqliteLedgerAssignmentStore {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    /// Reads the current materialized assignment, or `None` if this
    /// partition has never been assigned.
    pub fn get(
        &self,
        org_id: &str,
        topic: &str,
        partition: u32,
    ) -> anyhow::Result<Option<PartitionAssignment>> {
        Ok(crate::db::repository::bus_assignment_get(
            &self.pool,
            crate::bus::instance::LEGACY_SINGLE_INSTANCE,
            org_id,
            topic,
            partition,
        )?
        .map(PartitionAssignment::from))
    }

    /// Every materialized assignment for one topic, ordered by partition.
    pub fn list_for_topic(
        &self,
        org_id: &str,
        topic: &str,
    ) -> anyhow::Result<Vec<PartitionAssignment>> {
        Ok(crate::db::repository::bus_assignment_list_for_topic(
            &self.pool,
            crate::bus::instance::LEGACY_SINGLE_INSTANCE,
            org_id,
            topic,
        )?
        .into_iter()
        .map(PartitionAssignment::from)
        .collect())
    }

    /// Every materialized assignment where `node_id` is the leader OR a
    /// replica-set member (`db::repository::bus_assignment_list_for_node`'s
    /// doc explains the decode-filter choice) — used at startup to rebuild
    /// which partitions this node must feed/follow.
    pub fn list_for_node(&self, node_id: &str) -> anyhow::Result<Vec<PartitionAssignment>> {
        Ok(crate::db::repository::bus_assignment_list_for_node(
            &self.pool,
            crate::bus::instance::LEGACY_SINGLE_INSTANCE,
            node_id,
        )?
        .into_iter()
        .map(PartitionAssignment::from)
        .collect())
    }

    /// Proposes a new assignment (leader/replica-set/epoch change) through
    /// the ledger — NEVER a direct SQL write (K-M2-4). Mints a
    /// `core.bus_partition_assignment` op via the immediate-publish path
    /// (`sync::runtime::record_core_capture`) and returns its `OperationId`,
    /// which the caller (election/reassignment logic, wave 1 agent EL) uses
    /// with `admitted_by` to observe majority acknowledgement before
    /// treating the change as committed.
    ///
    /// Upsert semantics: always captured as `SqlWriteAction::Insert`
    /// regardless of whether a row already exists locally —
    /// `apply_bus_partition_assignment` treats `Insert`/`Update` identically
    /// (both are a full-row upsert gated by the same epoch check), so there
    /// is no local-existence check to make here; the ledger/materializer
    /// pair already handles both "new partition" and "reassigning an
    /// existing one" through the same code path.
    pub fn propose(&self, assignment: &PartitionAssignment) -> anyhow::Result<OperationId> {
        let resource_id = assignment_resource_id(
            &assignment.instance_id,
            &assignment.org_id,
            &assignment.topic,
            assignment.partition,
        );
        let assignment_json = serde_json::to_string(assignment)?;
        let mut changed_fields = std::collections::BTreeMap::new();
        changed_fields.insert(
            "assignment_json".to_string(),
            crate::sync::ledger::FieldValue::String(assignment_json),
        );

        let hlc = crate::sync::runtime::core_hlc_now();
        let epoch = crate::sync::runtime::core_epoch();
        let capture = crate::sync::core_capture::CoreWriteCapture::new(
            crate::sync::core_registry::CoreSyncResourceKind::BusPartitionAssignment,
            assignment.org_id.clone(),
            resource_id,
            // System/election-driven, not a row-level partial update — the
            // whole point of the epoch gate in `apply_bus_partition_
            // assignment` is that `Insert`/`Update` are handled identically.
            crate::sync::runtime::SqlWriteAction::Insert,
            changed_fields,
            None,
            hlc,
            epoch,
        );
        let recorded = crate::sync::runtime::record_core_capture(capture)?.ok_or_else(|| {
            anyhow::anyhow!(
                "sync runtime not initialized: cannot propose partition assignment for {}/{}/{}",
                assignment.org_id,
                assignment.topic,
                assignment.partition
            )
        })?;
        // LOCAL MATERIALIZATION AT MINT TIME (P8 converge-locally, see
        // `sync::runtime::apply_core_operation_locally`'s doc for the
        // measured failure): this resource's ONLY writer is the
        // materializer (K-M2-4), and a minted op would otherwise reach
        // every OTHER node's table before — or, when a peer relays it
        // back, long after — this node's own. Until the author's own row
        // exists, a peer's SAME-EPOCH LOSER proposal is admitted against
        // the stale stored row and the assignment poll regresses this
        // node's freshly-won leadership ("follower of the loser") until
        // the relay catches up — measured as ~40 s of a serving-capable
        // winner answering `not the leader (leader is <loser>)` in the
        // 3-process chaos run. Applying the author's own op through the
        // SAME admission gate here stamps the local HLC watermark first,
        // so the loser op is rejected deterministically on arrival and
        // the author's row is authoritative from this instant.
        // The ledger stays the source of truth and the materializer stays
        // the only writer — this is the local half of the same round trip
        // every peer performs, not a second write path.
        if let Err(e) =
            crate::sync::runtime::apply_core_operation_locally(recorded.op_id, &self.pool)
        {
            // Non-fatal: the op is already minted and in the outbox, so
            // peers still converge and a repair pull can still land the
            // local row. But it must be LOUD — a silent skip here is
            // exactly the stale-registry window this call exists to close.
            tracing::warn!(
                op_id = %recorded.op_id.to_hex(),
                error = %e,
                "replication: local materialization of a proposed assignment failed \
                 (registry may lag the ledger until the next sync round trip)"
            );
        }
        Ok(recorded.op_id)
    }

    /// Node ids that have ACKNOWLEDGED (not merely delivered) `op_id`,
    /// including the local node when it appears in its own outbox — the
    /// majority-observability primitive PLAN-M2 §1c points at
    /// (`OutboxEntry`/`list_outbox_for_operation`). The caller (election,
    /// wave 1 agent EL) compares `admitted_by(op_id).len()` against
    /// `min_isr_required`/a replica-set majority; this method itself makes
    /// no majority decision, it only reports who has acked.
    pub fn admitted_by(&self, op_id: OperationId) -> anyhow::Result<Vec<String>> {
        crate::sync::runtime::acknowledged_outbox_targets(op_id)?
            .ok_or_else(|| anyhow::anyhow!("sync runtime not initialized: cannot query outbox"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repository::{bus_test_support::create_bus_tables, DbBusTopic};

    fn db_row(row: &DbBusPartitionAssignment) -> PartitionAssignment {
        PartitionAssignment::from(row.clone())
    }

    #[test]
    fn to_db_row_and_from_round_trip_every_field_except_environment() {
        let assignment = PartitionAssignment {
            instance_id: "tentabus-00000001".to_string(),
            org_id: "org-1".to_string(),
            topic: "orders.created".to_string(),
            partition: 3,
            leader_node_id: "node-a".to_string(),
            replicas: vec!["node-a".to_string(), "node-b".to_string()],
            isr: vec!["node-a".to_string()],
            leader_epoch: 2,
            updated_at_ms: 5_000,
        };
        let row = assignment.to_db_row("prod");
        assert_eq!(row.instance_id, assignment.instance_id);
        assert_eq!(row.org_id, assignment.org_id);
        assert_eq!(row.topic, assignment.topic);
        assert_eq!(row.partition, assignment.partition);
        assert_eq!(row.leader_node_id, assignment.leader_node_id);
        assert_eq!(row.replicas, assignment.replicas);
        assert_eq!(row.isr, assignment.isr);
        assert_eq!(row.leader_epoch, assignment.leader_epoch);
        assert_eq!(row.environment, "prod");
        assert_eq!(row.updated_at_ms, assignment.updated_at_ms);

        assert_eq!(db_row(&row), assignment);
    }

    /// Mirrors `dispatch/environment.rs`'s `locked_env_fixture` pattern: one
    /// process-global `SYNC_RUNTIME`/Fjall ledger for the whole test binary,
    /// `init` is idempotent (only the first caller across the whole crate's
    /// `--lib` test run actually opens it), serialized against every other
    /// module's own copy of this fixture via the shared
    /// `addon::fs_sandbox::test_home_lock` so environment-sensitive tests
    /// never interleave.
    fn locked_ledger_fixture() -> std::sync::MutexGuard<'static, ()> {
        let guard = crate::addon::fs_sandbox::test_home_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        static INITIALIZED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        if INITIALIZED.get().is_none() {
            let tmp = tempfile::tempdir().expect("tempdir");
            std::env::set_var("HOME", tmp.path());
            std::env::set_var("TENTAFLOW_HOME", tmp.path());

            let conn = rusqlite::Connection::open_in_memory().expect("open db");
            crate::db::migrations::run(&conn).expect("run migrations");
            let db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
            let cipher = std::sync::Arc::new(crate::crypto::SettingsCipher::new(&[11u8; 32]));
            let security = std::sync::Arc::new(
                crate::mesh::security::MeshSecurity::new(db.clone(), cipher.clone())
                    .expect("mesh security"),
            );
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                match crate::sync::runtime::init(db.clone(), security.clone(), cipher.clone()) {
                    Ok(_) => break,
                    Err(crate::sync::ledger::SyncLedgerError::Fjall(fjall::Error::Locked))
                        if std::time::Instant::now() < deadline =>
                    {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    Err(e) => panic!("sync runtime init: {e:?}"),
                }
            }
            // Ledger stays open for the rest of the process; leaking the
            // tempdir handle keeps its path valid for the runtime's
            // lifetime (same tradeoff `dispatch/environment.rs` makes).
            std::mem::forget(tmp);
            let _ = INITIALIZED.set(());
        }
        guard
    }

    fn test_assignment(partition: u32) -> PartitionAssignment {
        PartitionAssignment {
            instance_id: crate::bus::instance::LEGACY_SINGLE_INSTANCE.to_string(),
            org_id: "org-assign".to_string(),
            topic: "orders.created".to_string(),
            partition,
            leader_node_id: "node-a".to_string(),
            replicas: vec!["node-a".to_string(), "node-b".to_string()],
            isr: vec!["node-a".to_string(), "node-b".to_string()],
            leader_epoch: 1,
            updated_at_ms: 1_000,
        }
    }

    /// A fresh SQLite pool (NOT the ledger fixture's own `db`) with the
    /// bus tables plus a parent `bus_topics` row —
    /// `apply_bus_partition_assignment` derives `environment` from that row
    /// and defers otherwise.
    fn fresh_sql_pool_with_topic(org_id: &str, topic: &str, environment: &str) -> DbPool {
        let pool = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        create_bus_tables(&pool).expect("bus fixture tables");
        crate::db::repository::bus_topic_create(
            &pool,
            &DbBusTopic {
                instance_id: crate::bus::instance::LEGACY_SINGLE_INSTANCE.to_string(),
                org_id: org_id.to_string(),
                name: topic.to_string(),
                partitions: 4,
                retention_ms: 1,
                retention_bytes: 1,
                cleanup_policy: "delete".to_string(),
                delivery: "at_least_once".to_string(),
                idempotency_key: None,
                dedup_window_ms: 1,
                max_delivery_attempts: 1,
                retry_backoff_ms: 1,
                schema_id: None,
                validation: "none".to_string(),
                content_type: "application/json".to_string(),
                replication_factor: 2,
                acks: "all".to_string(),
                durability: "fsync_batch".to_string(),
                max_inline_bytes: 1,
                compression: "lz4".to_string(),
                environment: environment.to_string(),
                created_at_ms: 1,
                updated_at_ms: 1,
                durability_class: None,
            },
        )
        .expect("seed parent bus_topic");
        pool
    }

    /// A bus-tables-only pool with no rows in them — one side of the
    /// publish/materialize round trip below.
    fn fresh_bus_pool() -> DbPool {
        let pool = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        create_bus_tables(&pool).expect("bus fixture tables");
        pool
    }

    /// The row shape `bus::topics::create_topic` persists, minus the parts
    /// this test does not care about.
    fn publishable_topic_row(org_id: &str, name: &str) -> DbBusTopic {
        DbBusTopic {
            instance_id: crate::bus::instance::LEGACY_SINGLE_INSTANCE.to_string(),
            org_id: org_id.to_string(),
            name: name.to_string(),
            partitions: 4,
            retention_ms: 60_000,
            retention_bytes: 1_024,
            cleanup_policy: "delete".to_string(),
            delivery: "at_least_once".to_string(),
            idempotency_key: None,
            dedup_window_ms: 1,
            max_delivery_attempts: 3,
            retry_backoff_ms: 100,
            schema_id: None,
            validation: "none".to_string(),
            content_type: "application/json".to_string(),
            replication_factor: 2,
            acks: "all".to_string(),
            durability: "fsync".to_string(),
            max_inline_bytes: 64,
            compression: "lz4".to_string(),
            environment: "prod".to_string(),
            created_at_ms: 1,
            updated_at_ms: 1,
            durability_class: None,
        }
    }

    /// `core_resource_versions` is the writer's own LWW watermark, and the only
    /// local trace a publish leaves behind: reading it says whether
    /// `bus_topics`'s write path actually minted a `core.bus_topic` op.
    fn published_watermark(db: &DbPool, org_id: &str, name: &str) -> Option<i64> {
        let resource_id = crate::sync::resource_id::composite_resource_id(&[
            crate::bus::instance::LEGACY_SINGLE_INSTANCE,
            org_id,
            name,
        ]);
        let conn = crate::db::repository::acquire_for_baseline(db).expect("conn");
        conn.query_row(
            "SELECT hlc_wall FROM core_resource_versions WHERE resource_type = ?1 AND resource_id = ?2",
            rusqlite::params!["core.bus_topic", resource_id],
            |row| row.get::<_, i64>(0),
        )
        .ok()
    }

    /// The other half of K-M2-4, which no earlier wave shipped: a local
    /// `bus_topics` write must MINT a `core.bus_topic` op, and a replica must
    /// materialize it. Before this, `core.bus_topic` had a descriptor, a
    /// materializer and zero producers — so every replica's assignment op
    /// deferred forever on a parent topic row that could never arrive, and
    /// `PartitionProvider` had no config to open a partition with. Asserts the
    /// whole chain on the REAL runtime/ledger/materializer, both directions:
    /// create (Insert) and delete (Delete, which has to carry the row too).
    #[test]
    fn a_topic_write_publishes_a_core_bus_topic_op_a_replica_can_materialize() {
        let _guard = locked_ledger_fixture();
        let org = "org-topic-publish";
        let name = "orders.created";
        let author = fresh_bus_pool();
        let receiver = fresh_bus_pool();
        let row = publishable_topic_row(org, name);

        // 1. The writer itself publishes: `bus_topic_create` must leave the
        //    watermark behind, which only happens on a real mint.
        crate::db::repository::bus_topic_create(&author, &row).expect("create");
        assert!(
            published_watermark(&author, org, name).is_some(),
            "a local bus_topics write must publish a core.bus_topic op"
        );

        // 2. The op is a `core.bus_topic` op on the row's own composite id.
        let op_id = crate::db::repository::publish_bus_topic_capture(
            &author,
            &row,
            crate::sync::runtime::SqlWriteAction::Update,
        )
        .expect("publish")
        .expect("a live sync runtime must publish");
        let op = crate::sync::runtime::get_operation(op_id)
            .expect("get_operation")
            .expect("op must exist right after publish");
        assert_eq!(op.body.resource_type, "core.bus_topic");
        assert_eq!(
            op.body.resource_id,
            crate::sync::resource_id::composite_resource_id(&[
                crate::bus::instance::LEGACY_SINGLE_INSTANCE,
                org,
                name
            ])
        );

        // 3. A replica that never saw the topic materializes it from that op —
        //    with the policy columns intact, which is the point: the
        //    replication layer must not have to invent any of them.
        let cipher = std::sync::Arc::new(crate::crypto::SettingsCipher::new(&[11u8; 32]));
        let rows = crate::sync::core_materializer::apply_core_operation(&receiver, &cipher, &op)
            .expect("materialize");
        assert_eq!(rows, 1);
        let fetched = crate::db::repository::bus_topic_get(
            &receiver,
            crate::bus::instance::LEGACY_SINGLE_INSTANCE,
            org,
            name,
        )
        .expect("get")
        .expect("topic must materialize on the replica");
        assert_eq!(fetched.partitions, 4);
        assert_eq!(fetched.replication_factor, 2);
        assert_eq!(fetched.acks, "all");
        assert_eq!(fetched.durability, "fsync");
        assert_eq!(fetched.environment, "prod");

        // 4. Delete travels the same way: `apply_bus_topic` decodes `row_json`
        //    before it dispatches on the action, so a Delete capture has to
        //    carry the row it is removing.
        let delete_op_id = crate::db::repository::publish_bus_topic_capture(
            &author,
            &row,
            crate::sync::runtime::SqlWriteAction::Delete,
        )
        .expect("publish delete")
        .expect("a live sync runtime must publish");
        let delete_op = crate::sync::runtime::get_operation(delete_op_id)
            .expect("get_operation")
            .expect("delete op must exist");
        assert_eq!(
            delete_op.body.action,
            crate::sync::ledger::ActionType::Delete
        );
        crate::sync::core_materializer::apply_core_operation(&receiver, &cipher, &delete_op)
            .expect("materialize delete");
        assert!(
            crate::db::repository::bus_topic_get(
                &receiver,
                crate::bus::instance::LEGACY_SINGLE_INSTANCE,
                org,
                name
            )
            .expect("get")
            .is_none(),
            "a Delete op must remove the replica's row"
        );
    }

    /// The end-to-end path PLAN-M2 §1c asks for: `propose` mints a real
    /// ledger op (through the SAME `SYNC_RUNTIME` production code uses) and
    /// the row materializes into `bus_partition_assignments` ON THE AUTHOR
    /// immediately — `propose` itself now runs the local half of the
    /// materializer round trip (`sync::runtime::
    /// apply_core_operation_locally`, P8 converge-locally), so the payload
    /// shape it emits is proven against the real
    /// `apply_bus_partition_assignment` and the author's own registry
    /// cannot lag its own election. Re-feeding the SAME op afterwards must
    /// be an idempotent no-op (the outer HLC-LWW gate sees an equal HLC and
    /// rejects) — the property that makes a peer relaying the op back to
    /// its author harmless.
    #[test]
    fn propose_then_materialize_round_trips_through_the_ledger() {
        let _guard = locked_ledger_fixture();
        let assignment = test_assignment(0);
        let pool = fresh_sql_pool_with_topic(&assignment.org_id, &assignment.topic, "test");
        let store = SqliteLedgerAssignmentStore::new(pool.clone());

        assert!(
            store
                .get(&assignment.org_id, &assignment.topic, assignment.partition)
                .unwrap()
                .is_none(),
            "nothing materialized before propose"
        );

        let op_id = store.propose(&assignment).expect("propose");
        let op = crate::sync::runtime::get_operation(op_id)
            .expect("get_operation")
            .expect("operation must exist right after propose");
        assert_eq!(op.body.resource_type, "core.bus_partition_assignment");

        // THE AUTHOR'S OWN ROW EXISTS RIGHT AFTER PROPOSE — no waiting for
        // a peer relay, no manual apply. This is the P8 converge-locally
        // guarantee the election's registry depends on.
        let fetched = store
            .get(&assignment.org_id, &assignment.topic, assignment.partition)
            .expect("get")
            .expect("assignment row must exist right after propose (local materialization)");
        assert_eq!(fetched, assignment);

        // Re-applying the SAME op (the shape of a peer relaying the
        // author's op back) is an idempotent no-op: the outer HLC-LWW gate
        // sees an HLC it already stamped and rejects before the row gate.
        let cipher = std::sync::Arc::new(crate::crypto::SettingsCipher::new(&[11u8; 32]));
        let rows = crate::sync::core_materializer::apply_core_operation(&pool, &cipher, &op)
            .expect("materialize (idempotent re-apply)");
        assert_eq!(
            rows, 0,
            "a re-delivered own op must not re-write the author's row"
        );
        assert_eq!(
            store
                .get(&assignment.org_id, &assignment.topic, assignment.partition)
                .expect("get")
                .expect("row must survive the idempotent re-apply"),
            assignment
        );

        let for_topic = store
            .list_for_topic(&assignment.org_id, &assignment.topic)
            .expect("list_for_topic");
        assert_eq!(for_topic, vec![assignment.clone()]);

        let for_node = store.list_for_node("node-a").expect("list_for_node");
        assert_eq!(for_node, vec![assignment]);
    }

    #[test]
    fn admitted_by_reports_no_targets_for_a_freshly_proposed_op_on_a_single_node() {
        let _guard = locked_ledger_fixture();
        let assignment = test_assignment(1);
        let pool = fresh_sql_pool_with_topic(&assignment.org_id, &assignment.topic, "test");
        let store = SqliteLedgerAssignmentStore::new(pool);

        let op_id = store.propose(&assignment).expect("propose");
        // A single node with no peers has nobody to acknowledge yet — the
        // op is queued in nobody's outbox.
        assert!(store.admitted_by(op_id).expect("admitted_by").is_empty());
    }
}

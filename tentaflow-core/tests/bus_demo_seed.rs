//! Temporary, ignored-by-default seed/wipe harness for TentaBus's
//! consumer-group and DLQ UI screens (M04). Not part of the test suite run
//! by CI — both tests are `#[ignore]`d and must be invoked by name.
//!
//! This file exists purely so a UI critic can click through real
//! consumer-group lag and DLQ screens; delete it once that review is done.
//!
//! ## STOP THE APP FIRST
//! Bus partition directories are `flock`-ed by the running process and the
//! SQLite DB is opened WAL/exclusive by it — this harness must run against
//! a stopped app's `.runtime/`, never a live one.
//!
//! ## Usage
//! ```text
//! TENTABUS_SEED_DB=/path/to/.runtime/data/tentaflow.db \
//!   cargo test --test bus_demo_seed -- --ignored seed_demo_data --nocapture
//!
//! TENTABUS_SEED_DB=/path/to/.runtime/data/tentaflow.db \
//!   cargo test --test bus_demo_seed -- --ignored wipe_demo_data --nocapture
//! ```
//! `TENTABUS_SEED_BUS_DIR` overrides the bus directory; by default it is
//! derived from `TENTABUS_SEED_DB` as `<db's grandparent>/bus` (mirrors
//! `paths::tentaflow_home().join("bus")` next to `.runtime/data/`).
//!
//! Run `seed_demo_data` and `wipe_demo_data` ONE AT A TIME (`cargo test`
//! runs `#[ignore]`d tests selected together in the same process/threads;
//! this harness was only exercised with one test name per invocation).

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;

use tentaflow_core::bus::{
    self, dlq, groups, topics, BusAction, BusCallContext, BusInitConfig, BusService,
    BusServiceError, ConsumerConfig, FetchedRecordMeta, PublishBatch, PublishRecord,
    TopicPartition,
};
use tentaflow_core::db::DbPool;
use tentaflow_core::services::org::DEFAULT_ORG_ID;

const ACTOR: &str = "admin";
const ORIGIN: &str = "bus-demo-seed-harness";

const LAB_TOPIC: &str = "lab.results";
const ORDERS_TOPIC: &str = "orders.created";
const BILLING_GROUP: &str = "billing";
const NOTIFIER_GROUP: &str = "notifier";

const LAB_PARTITIONS: u32 = 8;
const ORDERS_PARTITIONS: u32 = 3;
const LAB_RECORDS_PER_KEY: usize = 20;
const LAB_KEY_COUNT: usize = 100; // -> 2000 records total
const ORDERS_RECORD_COUNT: usize = 500;

const BILLING_COMMIT_FRACTION: f64 = 0.4;
const DLQ_RECORD_COUNT: u64 = 15;
const ATTEMPTS_ONLY_RECORD_COUNT: u64 = 3;
const SCHEMA_ERROR_MESSAGE: &str = "schema validation failed: missing field 'unit'";

/// Allow-all authorizer: the app's real RBAC re-applies once it restarts
/// and reopens the bus against the same on-disk state this harness writes
/// — only the data on disk matters here, not who is allowed to touch it
/// while this harness runs.
struct AllowAllAuthorizer;

impl bus::BusAuthorizer for AllowAllAuthorizer {
    fn authorize(
        &self,
        _ctx: &BusCallContext,
        _action: BusAction,
        _topic: &str,
    ) -> Result<(), BusServiceError> {
        Ok(())
    }

    fn authorize_group(
        &self,
        _ctx: &BusCallContext,
        _action: BusAction,
        _topic: &str,
        _group: &str,
    ) -> Result<(), BusServiceError> {
        Ok(())
    }

    fn generation(&self) -> u64 {
        0
    }
}

fn seed_db_path() -> PathBuf {
    PathBuf::from(env::var("TENTABUS_SEED_DB").expect(
        "TENTABUS_SEED_DB must point at the target tentaflow.db \
         (e.g. <repo>/.runtime/data/tentaflow.db) — see this file's header for usage",
    ))
}

/// Defaults to `<db's grandparent>/bus`, i.e. `<tentaflow_home>/bus`
/// alongside `<tentaflow_home>/data/tentaflow.db` — matches the app's
/// default per-instance bus data layout without this crate needing to
/// depend on `paths` itself.
fn seed_bus_dir(db_path: &Path) -> PathBuf {
    if let Ok(v) = env::var("TENTABUS_SEED_BUS_DIR") {
        return PathBuf::from(v);
    }
    db_path
        .parent()
        .and_then(Path::parent)
        .map(|home| home.join("bus"))
        .expect(
            "TENTABUS_SEED_DB must have at least two parent components \
             (<home>/data/tentaflow.db) or TENTABUS_SEED_BUS_DIR must be set explicitly",
        )
}

fn call_ctx() -> BusCallContext {
    BusCallContext {
        instance_id: tentaflow_core::bus::instance::BusInstanceId::parse("tentabus-00000001")
            .expect("valid instance id"),
        org_id: DEFAULT_ORG_ID.to_string(),
        actor: Some(ACTOR.to_string()),
        correlation_id: Some("bus-demo-seed".to_string()),
        origin: ORIGIN.to_string(),
    }
}

/// Opens the target DB (runs the crate's normal migrations — a no-op on an
/// already-migrated app DB) and the bus service against it, using an
/// allow-all authorizer. Returns a DB handle too so callers can query
/// `bus::topics::get_topic` directly without going through `BusService`
/// (which does not expose its own DB handle).
fn open_service() -> (Arc<BusService>, DbPool) {
    let db_path = seed_db_path();
    let bus_dir = seed_bus_dir(&db_path);
    println!(
        "bus_demo_seed: db={} bus_dir={}",
        db_path.display(),
        bus_dir.display()
    );
    let db = tentaflow_core::db::init(&db_path).expect("open/migrate target db");
    let db_for_checks = db.clone();
    let local_conn = rusqlite::Connection::open_in_memory().expect("open local db");
    tentaflow_core::bus::db::migrate(&local_conn).expect("migrate local db");
    let local_db: DbPool = Arc::new(tentaflow_core::db::Db::from_connection(local_conn));
    let svc = bus::init(BusInitConfig {
        instance_id: tentaflow_core::bus::instance::BusInstanceId::parse("tentabus-00000001")
            .expect("valid instance id"),
        local_db,
        bus_dir,
        db,
        authorizer: Arc::new(AllowAllAuthorizer),
        retention_interval: None,
        dedup_expected_rate_per_sec: 10_000,
        partition_handle_lru: None,
        publish_ack_timeout: bus::DEFAULT_PUBLISH_ACK_TIMEOUT,
    })
    .expect("bus::init");
    (svc, db_for_checks)
}

fn lab_payload(seq: usize, patient_key: &str) -> Bytes {
    // ~300 bytes: a small CBC-result-shaped JSON blob padded with a filler
    // field so the UI has something non-trivial to render in a preview.
    let filler = "x".repeat(150);
    let json = format!(
        "{{\"patient_id\":\"{patient_key}\",\"test\":\"CBC\",\"value\":{value:.2},\
         \"seq\":{seq},\"collected_at\":\"2026-08-{day:02}T09:00:00Z\",\
         \"filler\":\"{filler}\"}}",
        patient_key = patient_key,
        value = 4.0 + (seq % 50) as f64 * 0.1,
        seq = seq,
        day = 1 + (seq % 28),
        filler = filler,
    );
    Bytes::from(json)
}

fn order_payload(seq: usize) -> Bytes {
    let filler = "y".repeat(200);
    let json = format!(
        "{{\"order_id\":\"O-{seq:05}\",\"sku\":\"SKU-{sku:04}\",\"qty\":{qty},\
         \"filler\":\"{filler}\"}}",
        seq = seq,
        sku = seq % 200,
        qty = 1 + (seq % 5),
        filler = filler,
    );
    Bytes::from(json)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_millis() as i64
}

fn seed_headers() -> Vec<(String, Bytes)> {
    vec![("source".to_string(), Bytes::from_static(b"seed"))]
}

/// Creates `name` (with `opts`) unless it already exists — the harness's
/// idempotency knob (re-running `seed_demo_data` without `wipe_demo_data`
/// first is a no-op per topic, not an error). Returns whether the topic was
/// freshly created (i.e. whether the caller should go on to publish/seed
/// consumer state for it).
fn ensure_topic(
    svc: &BusService,
    db: &DbPool,
    ctx: &BusCallContext,
    name: &str,
    opts: topics::TopicOptions,
) -> bool {
    if topics::get_topic(db, svc.instance_id(), &ctx.org_id, name)
        .expect("get_topic")
        .is_some()
    {
        println!("seed: topic '{name}' already exists — skipping creation/seed");
        return false;
    }
    svc.create_topic(ctx, name, opts)
        .unwrap_or_else(|e| panic!("create_topic('{name}') failed: {e}"));
    println!("seed: created topic '{name}'");
    true
}

fn publish_chunked(
    svc: &BusService,
    ctx: &BusCallContext,
    topic: &str,
    records: Vec<PublishRecord>,
) {
    for chunk in records.chunks(200) {
        svc.publish(
            ctx,
            topic,
            PublishBatch {
                partition: None,
                producer: None,
                records: chunk.to_vec(),
            },
        )
        .unwrap_or_else(|e| panic!("publish('{topic}') failed: {e}"));
    }
}

fn by_partition(records: Vec<FetchedRecordMeta>) -> BTreeMap<u32, Vec<FetchedRecordMeta>> {
    let mut map: BTreeMap<u32, Vec<FetchedRecordMeta>> = BTreeMap::new();
    for r in records {
        map.entry(r.partition).or_default().push(r);
    }
    for recs in map.values_mut() {
        recs.sort_by_key(|r| r.offset);
    }
    map
}

/// Read-only record count for one partition — `peek`'s `high_watermark` is
/// computed from the SAME partition snapshot as its `records` field but is
/// never truncated by `PEEK_MAX_RECORDS`/`PEEK_MAX_BYTES`, so a `max_records
/// = 1` call is enough to learn the true total without pulling the whole
/// partition through the (100-record-capped) peek path. Used only for the
/// summary — safe to call on every run, seeded or not.
fn partition_high_watermark(
    svc: &BusService,
    ctx: &BusCallContext,
    topic: &str,
    partition: u32,
) -> u64 {
    svc.peek(ctx, topic, partition, 0, 1, 1)
        .unwrap_or_else(|e| panic!("peek('{topic}', partition {partition}) failed: {e}"))
        .high_watermark
}

/// Read-only per-partition lag for `group` on `topic` — `open_consumer` is
/// idempotent (reconnects to the existing `bus_groups` row rather than
/// resetting it) and `lag()` touches no durable state, so this is safe to
/// call on every run, seeded or not, purely for the summary.
fn group_lag(ctx: &BusCallContext, group: &str, topic: &str) -> Vec<(TopicPartition, u64)> {
    let handle = bus::open_consumer(
        ctx,
        group,
        &[topic.to_string()],
        ConsumerConfig {
            commit_mode: groups::CommitMode::Explicit,
        },
    )
    .unwrap_or_else(|e| panic!("open_consumer('{group}') for summary failed: {e}"));
    handle
        .lag()
        .unwrap_or_else(|e| panic!("lag('{group}') failed: {e}"))
}

#[test]
#[ignore]
fn seed_demo_data() {
    let (svc, db) = open_service();
    let ctx = call_ctx();

    // ---- lab.results (8 partitions, ~2000 keyed records) ----------------
    let lab_created = ensure_topic(
        &svc,
        &db,
        &ctx,
        LAB_TOPIC,
        topics::TopicOptions {
            partitions: Some(LAB_PARTITIONS),
            ..Default::default()
        },
    );
    if lab_created {
        let mut records = Vec::with_capacity(LAB_KEY_COUNT * LAB_RECORDS_PER_KEY);
        let mut seq = 0usize;
        for key_idx in 1..=LAB_KEY_COUNT {
            let key = format!("P-{key_idx:04}");
            for _ in 0..LAB_RECORDS_PER_KEY {
                records.push(PublishRecord {
                    key: Some(Bytes::from(key.clone())),
                    headers: seed_headers(),
                    payload: lab_payload(seq, &key),
                    timestamp_ms: now_ms(),
                    schema_id: 0,
                });
                seq += 1;
            }
        }
        println!(
            "seed: publishing {} records to '{LAB_TOPIC}'",
            records.len()
        );
        publish_chunked(&svc, &ctx, LAB_TOPIC, records);
    }

    // ---- orders.created (3 partitions, ~500 records) ---------------------
    let orders_created = ensure_topic(
        &svc,
        &db,
        &ctx,
        ORDERS_TOPIC,
        topics::TopicOptions {
            partitions: Some(ORDERS_PARTITIONS),
            ..Default::default()
        },
    );
    if orders_created {
        let records: Vec<PublishRecord> = (0..ORDERS_RECORD_COUNT)
            .map(|seq| PublishRecord {
                key: None,
                headers: seed_headers(),
                payload: order_payload(seq),
                timestamp_ms: now_ms(),
                schema_id: 0,
            })
            .collect();
        println!(
            "seed: publishing {} records to '{ORDERS_TOPIC}'",
            records.len()
        );
        publish_chunked(&svc, &ctx, ORDERS_TOPIC, records);
    }

    let lab_cfg = topics::get_topic(&db, svc.instance_id(), &ctx.org_id, LAB_TOPIC)
        .expect("get_topic")
        .expect("lab.results must exist by now");
    let orders_cfg = topics::get_topic(&db, svc.instance_id(), &ctx.org_id, ORDERS_TOPIC)
        .expect("get_topic")
        .expect("orders.created must exist by now");

    // ---- billing group on lab.results: partial commit -> visible lag ----
    // Only runs the FIRST time lab.results is seeded: re-deriving fresh 40%
    // targets and re-injecting DLQ failures on a re-run would try to commit
    // BACKWARDS past what a prior run's DLQ processing already advanced the
    // offset to (`BusServiceError::OffsetRegression`) — `ensure_topic`'s
    // skip is exactly what keeps a second `seed_demo_data` run a no-op here.
    if lab_created {
        let billing_handle = bus::open_consumer(
            &ctx,
            BILLING_GROUP,
            &[LAB_TOPIC.to_string()],
            ConsumerConfig {
                commit_mode: groups::CommitMode::Explicit,
            },
        )
        .expect("open_consumer(billing)");
        // A real fetch (not just peek) so the group genuinely "consumed"
        // the records it is about to partially commit, matching a real
        // client's fetch-then-commit flow.
        let fetched = billing_handle
            .fetch(64 * 1024 * 1024, 500)
            .expect("billing fetch");
        let lab_by_partition = by_partition(fetched.records);

        let mut commit_targets: Vec<(TopicPartition, u64)> = Vec::new();
        let mut committed_by_partition: BTreeMap<u32, u64> = BTreeMap::new();
        for (&p, recs) in &lab_by_partition {
            let target = ((recs.len() as f64) * BILLING_COMMIT_FRACTION).floor() as u64;
            committed_by_partition.insert(p, target);
            commit_targets.push((
                TopicPartition {
                    topic: LAB_TOPIC.to_string(),
                    partition: p,
                },
                target,
            ));
        }
        billing_handle
            .commit(&commit_targets)
            .expect("billing partial commit");
        println!(
            "seed: billing group committed {}% of every lab.results partition",
            (BILLING_COMMIT_FRACTION * 100.0) as u32
        );

        // ---- DLQ injection: ~15 records fully failed, ~3 with attempts>0
        let dlq_partition = *lab_by_partition
            .iter()
            .max_by_key(|entry| entry.1.len() as u64 - committed_by_partition[entry.0])
            .map(|(p, _)| p)
            .expect("lab.results must have at least one non-empty partition");
        let dlq_recs = &lab_by_partition[&dlq_partition];
        let committed = committed_by_partition[&dlq_partition];
        let headroom = dlq_recs.len() as u64 - committed;
        let needed = DLQ_RECORD_COUNT + ATTEMPTS_ONLY_RECORD_COUNT;
        assert!(
            headroom >= needed,
            "partition {dlq_partition} only has {headroom} uncommitted records, need \
             {needed} for the DLQ/attempts scenario — rerun with more lab.results records \
             or fewer keys"
        );

        let mut offset = committed;
        for _ in 0..DLQ_RECORD_COUNT {
            let record = &dlq_recs[offset as usize];
            let mut outcome = None;
            for _ in 0..lab_cfg.max_delivery_attempts {
                outcome = Some(
                    svc.note_delivery_failure(
                        &ctx,
                        BILLING_GROUP,
                        LAB_TOPIC,
                        dlq_partition,
                        offset,
                        record,
                        dlq::DlqReason::SchemaViolation,
                        SCHEMA_ERROR_MESSAGE,
                    )
                    .expect("note_delivery_failure"),
                );
            }
            match outcome.unwrap() {
                dlq::DlqOutcome::SentToDlq { attempts } => {
                    assert_eq!(attempts, lab_cfg.max_delivery_attempts);
                }
                other => panic!(
                    "expected SentToDlq for partition {dlq_partition} offset {offset}, got \
                     {other:?}"
                ),
            }
            offset += 1;
        }
        println!(
            "seed: sent {DLQ_RECORD_COUNT} records from partition {dlq_partition} to \
             '__dlq.{LAB_TOPIC}' (billing group committed offset now {offset})"
        );

        // 3 more records with attempts > 0 but under the DLQ threshold.
        let attempts_only = (lab_cfg.max_delivery_attempts.saturating_sub(1)).max(1);
        for _ in 0..ATTEMPTS_ONLY_RECORD_COUNT {
            let record = &dlq_recs[offset as usize];
            let mut outcome = None;
            for _ in 0..attempts_only {
                outcome = Some(
                    svc.note_delivery_failure(
                        &ctx,
                        BILLING_GROUP,
                        LAB_TOPIC,
                        dlq_partition,
                        offset,
                        record,
                        dlq::DlqReason::SchemaViolation,
                        SCHEMA_ERROR_MESSAGE,
                    )
                    .expect("note_delivery_failure"),
                );
            }
            match outcome.unwrap() {
                dlq::DlqOutcome::Retry { attempts, .. } => assert_eq!(attempts, attempts_only),
                other => panic!(
                    "expected Retry (attempts under threshold) for offset {offset}, got {other:?}"
                ),
            }
            offset += 1;
        }
        println!(
            "seed: left {ATTEMPTS_ONLY_RECORD_COUNT} records in billing/lab.results with \
             attempts={attempts_only} (not yet in DLQ)"
        );
    } else {
        println!(
            "seed: 'lab.results' already existed — skipping billing/DLQ scenario (already seeded)"
        );
    }

    // ---- notifier group on orders.created: fully caught up --------------
    if orders_created {
        let notifier_handle = bus::open_consumer(
            &ctx,
            NOTIFIER_GROUP,
            &[ORDERS_TOPIC.to_string()],
            ConsumerConfig {
                commit_mode: groups::CommitMode::Explicit,
            },
        )
        .expect("open_consumer(notifier)");
        let fetched = notifier_handle
            .fetch(64 * 1024 * 1024, 500)
            .expect("notifier fetch");
        let orders_by_partition = by_partition(fetched.records);
        let notifier_commit: Vec<(TopicPartition, u64)> = orders_by_partition
            .iter()
            .map(|(&p, recs)| {
                (
                    TopicPartition {
                        topic: ORDERS_TOPIC.to_string(),
                        partition: p,
                    },
                    recs.len() as u64,
                )
            })
            .collect();
        notifier_handle
            .commit(&notifier_commit)
            .expect("notifier full commit");
        println!("seed: notifier group fully caught up on orders.created");
    } else {
        println!(
            "seed: 'orders.created' already existed — skipping notifier scenario (already seeded)"
        );
    }

    // ---- summary (read-only: safe whether this run seeded or skipped) ---
    let dlq_topic = dlq::dlq_topic_name(LAB_TOPIC);
    println!("\n=== bus_demo_seed summary ===");
    println!("topic '{LAB_TOPIC}': {} partitions", lab_cfg.partitions);
    let billing_lag = group_lag(&ctx, BILLING_GROUP, LAB_TOPIC);
    let mut lab_total = 0u64;
    for (tp, lag) in &billing_lag {
        let hw = partition_high_watermark(&svc, &ctx, LAB_TOPIC, tp.partition);
        lab_total += hw;
        println!(
            "  partition {}: {hw} records, billing committed={}, lag={lag}",
            tp.partition,
            hw - lag
        );
    }
    println!("  total records published: {lab_total}");

    let notifier_lag = group_lag(&ctx, NOTIFIER_GROUP, ORDERS_TOPIC);
    let orders_total: u64 = (0..orders_cfg.partitions)
        .map(|p| partition_high_watermark(&svc, &ctx, ORDERS_TOPIC, p))
        .sum();
    let notifier_lag_total: u64 = notifier_lag.iter().map(|(_, lag)| lag).sum();
    println!(
        "topic '{ORDERS_TOPIC}': {} partitions, {orders_total} records published, \
         notifier total lag={notifier_lag_total}",
        orders_cfg.partitions
    );

    let dlq_count: u64 = (0..lab_cfg.partitions)
        .map(|p| partition_high_watermark(&svc, &ctx, &dlq_topic, p))
        .sum();
    println!("DLQ topic '{dlq_topic}': {dlq_count} records (expected >= {DLQ_RECORD_COUNT})");
    assert!(
        dlq_count >= DLQ_RECORD_COUNT,
        "expected at least {DLQ_RECORD_COUNT} records in '{dlq_topic}', found {dlq_count}"
    );
    println!("=== end summary ===\n");
}

#[test]
#[ignore]
fn wipe_demo_data() {
    let (svc, _db) = open_service();
    let ctx = call_ctx();

    for topic in [
        dlq::dlq_topic_name(LAB_TOPIC),
        dlq::dlq_topic_name(ORDERS_TOPIC),
        LAB_TOPIC.to_string(),
        ORDERS_TOPIC.to_string(),
    ] {
        match svc.delete_topic(&ctx, &topic) {
            Ok(()) => println!("wipe: deleted topic '{topic}' (and its group state)"),
            Err(BusServiceError::TopicNotFound { .. }) => {
                println!("wipe: topic '{topic}' does not exist — nothing to delete")
            }
            Err(e) => panic!("delete_topic('{topic}') failed: {e}"),
        }
    }
}

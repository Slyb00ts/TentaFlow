//! Temporary, ignored-by-default seed/wipe harness for TentaBus's
//! consumer-group and DLQ UI screens (M04), rewritten for the multi-instance
//! TentaBus platform (SUM/tentabus/PLAN-APP-PLATFORM.md). Not part of the
//! test suite run by CI — both tests are `#[ignore]`d and must be invoked
//! by name.
//!
//! This file exists purely so a UI critic (or a Playwright suite) can click
//! through real consumer-group lag and DLQ screens across TWO real TentaBus
//! instances; delete it once the TentaBus UI review that motivated it is
//! done.
//!
//! ## Why two real instances, on real disk paths
//! Earlier versions of this harness seeded exactly ONE instance under a
//! hardcoded id (`tentabus-00000001`), against an in-memory per-instance
//! `local_db`, and derived its bus log root as `<home>/bus` — none of which
//! matches how a real TentaBus instance is provisioned
//! (`bus::native::native_on_enable`: a real, per-instance on-disk SQLite db
//! opened via `bus::native::open_db`, and a log root at
//! `<data dir>/log/` = `fs_sandbox::addon_data_dir(org, addon_id).join("log")`).
//! This version goes through the SAME lifecycle a real install+enable takes
//! (`addon::lifecycle::install_instance` + `db::repository::
//! set_addon_enabled`) to mint two REAL, distinct instance ids and real
//! on-disk state, then starts each instance's engine (`bus::init_instance`)
//! against exactly those real paths — matching the conventions
//! `tests/tentabus_two_instances.rs` (the W10 acceptance test) established.
//! `AllowAllAuthorizer` is still used for the harness's OWN writes below (it
//! is not driving the dashboard, so RBAC is irrelevant to what it does) —
//! but `grant_full_access` also grants the `admin` actor real `bus.read`/
//! `bus.write`/`bus.admin` permissions on both instances, since every bus
//! permission defaults to deny and a LATER real server process (e.g. one a
//! Playwright suite spawns against this seeded state) enforces that for
//! real, through its own freshly-booted `PermissionChecker`.
//!
//! ## STOP THE APP FIRST
//! Bus partition directories are `flock`-ed by the running process and the
//! per-instance SQLite dbs are opened WAL/exclusive by it — this harness
//! must run against a stopped app's `.runtime/` (or scratch home), never a
//! live one. It stops its own two engines (`bus::stop_instance`) before
//! `seed_demo_data`/`wipe_demo_data` return, releasing every flock it took,
//! so a server started against the same home right after this harness exits
//! sees a clean handoff.
//!
//! ## Usage
//! ```text
//! TENTABUS_SEED_DB=/path/to/tentaflow.db \
//! TENTABUS_SEED_HOME=/path/to/tentaflow_home \
//!   cargo test --test bus_demo_seed -- --ignored seed_demo_data --nocapture
//!
//! TENTABUS_SEED_DB=/path/to/tentaflow.db \
//! TENTABUS_SEED_HOME=/path/to/tentaflow_home \
//!   cargo test --test bus_demo_seed -- --ignored wipe_demo_data --nocapture
//!
//! TENTABUS_SEED_DB=/path/to/tentaflow.db \
//! TENTABUS_SEED_HOME=/path/to/tentaflow_home \
//!   cargo test --test bus_demo_seed -- --ignored seed_clinic_data --nocapture
//! ```
//! `seed_clinic_data` is the small-scale "Przychodnia Zdrowie" world of the
//! TentaBus UI mockups (SUM/mockups/tentabus-20260923): instance
//! "Produkcja" with HL7 v2 / JSON / XML topics, a consumer that falls
//! behind (with 25 minutes of rising lag history), a caught-up one, a paused
//! one with a backlog, unprocessed messages from the last hour and message
//! patterns (one in use, one withdrawn); and an empty instance "Szkolenia".
//! `tests/e2e/tentabus-ui.spec.js` drives the dashboard against it.
//! `TENTABUS_SEED_DB` is the main sqlite database file (may be a fresh,
//! already-migrated db, e.g. one produced by booting the real binary once
//! and stopping it — see `tests/e2e/analytics.spec.js`'s own two-phase
//! "boot once to migrate, seed offline, boot again" pattern, which a
//! TentaBus Playwright fixture follows the same way). `TENTABUS_SEED_HOME`
//! is the `TENTAFLOW_HOME` the harness (and later the real server reading
//! this same state) uses to resolve per-instance data directories
//! (`orgs/<org>/addons/<addon_id>/`); it defaults to `TENTABUS_SEED_DB`'s
//! grandparent directory (matching `<home>/data/tentaflow.db`) when unset.
//!
//! Run `seed_demo_data` and `wipe_demo_data` ONE AT A TIME (`cargo test`
//! runs `#[ignore]`d tests selected together in the same process/threads;
//! this harness was only exercised with one test name per invocation).
//!
//! ## Playwright fixture usage
//! A TentaBus e2e spec follows the exact two-phase shape
//! `tests/e2e/analytics.spec.js` and `tests/e2e/tentaquant.spec.js` already
//! use for their own fixtures:
//!   1. `startBinary({ port, db, home })` once, to create + migrate the
//!      main db and the native package catalog; `waitForServer`; stop it.
//!   2. Run this harness's `seed_demo_data` (via `execFileSync('cargo', […],
//!      { env: { TENTABUS_SEED_DB: db, TENTABUS_SEED_HOME: home, … } })`)
//!      against that SAME `db`/`home` pair.
//!   3. `startBinary({ port, db, home, keepDb: true })` again — the two
//!      seeded instances are `is_enabled = true` in the db this harness
//!      just wrote, so the real server's own boot-time native-instance pass
//!      (`AddonManager::start_installed_native_instances`) starts their
//!      engines against the exact on-disk state this harness produced.
//!   4. Drive the dashboard (`#/tentabus`) with Playwright as usual.

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;

use tentaflow_core::addon::{bundled, fs_sandbox, lifecycle};
use tentaflow_core::bus::instance::BusInstanceId;
use tentaflow_core::bus::schema_registry::{
    registry as schema_registry, Compatibility, SchemaType,
};
use tentaflow_core::bus::{
    self, dlq, groups, lag_history, native as bus_native, topics, BusAction, BusCallContext,
    BusInitConfig, BusService, BusServiceError, ConsumerConfig, FetchedRecordMeta, PublishBatch,
    PublishRecord, TopicPartition,
};
use tentaflow_core::db::{self, repository, DbPool};
use tentaflow_core::services::org::DEFAULT_ORG_ID;

const ACTOR: &str = "admin";
const ORIGIN: &str = "bus-demo-seed-harness";

const LAB_TOPIC: &str = "lab.results";
const ORDERS_TOPIC: &str = "orders.created";
const BILLING_GROUP: &str = "billing";
const NOTIFIER_GROUP: &str = "notifier";

const BILLING_COMMIT_FRACTION: f64 = 0.4;
const DLQ_RECORD_COUNT: u64 = 15;
const ATTEMPTS_ONLY_RECORD_COUNT: u64 = 3;
const SCHEMA_ERROR_MESSAGE: &str = "schema validation failed: missing field 'unit'";

/// The package this harness provisions instances of, and the version its
/// bundled manifest ships (`bus/app-manifest.toml`) — matches the literal
/// `bundled::install_native_packages` reads into the catalog, same as
/// `tests/tentabus_two_instances.rs`'s own `"1.0.0"`.
const PACKAGE_VERSION: &str = "1.0.0";

/// Two real TentaBus instances, each with its own topics/records/consumer
/// groups/DLQ — genuinely separate on-disk state (own per-instance db, own
/// `<data dir>/log/` segments), not one instance's data seeded twice.
/// `lab.results`/`orders.created` are deliberately the SAME topic names in
/// both instances: that is exactly the multi-instance isolation the wider
/// platform promises (`tests/tentabus_two_instances.rs`'s own acceptance
/// scenario), and a UI critic switching between the two in the dashboard's
/// instance gate should see two independently believable environments
/// under identical topic names, not a naming coincidence to explain away.
struct InstanceSpec {
    display_name: &'static str,
    lab_partitions: u32,
    orders_partitions: u32,
    lab_key_count: usize,
    lab_records_per_key: usize,
    orders_record_count: usize,
}

const INSTANCE_SPECS: [InstanceSpec; 2] = [
    InstanceSpec {
        display_name: "seed-primary",
        lab_partitions: 8,
        orders_partitions: 3,
        lab_key_count: 100,
        lab_records_per_key: 20,
        orders_record_count: 500,
    },
    InstanceSpec {
        display_name: "seed-secondary",
        lab_partitions: 4,
        orders_partitions: 2,
        lab_key_count: 40,
        lab_records_per_key: 15,
        orders_record_count: 200,
    },
];

/// Allow-all authorizer for the harness's OWN writes below: the app's real
/// RBAC re-applies once a real server restarts and reopens the bus against
/// the same on-disk state this harness writes — only the data on disk
/// matters here, not who is allowed to touch it while this harness runs.
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
         (e.g. <repo>/.runtime/data/tentaflow.db, or a scratch db a Playwright \
         fixture created) — see this file's header for usage",
    ))
}

/// `TENTAFLOW_HOME` this harness (and, later, the real server reading the
/// same state) resolves every per-instance data directory under. Defaults
/// to `<db's grandparent>`, i.e. `<home>` for `<home>/data/tentaflow.db` —
/// matches the app's default layout (`paths.rs`'s own module doc) without
/// this crate needing to depend on `paths` itself.
fn seed_home(db_path: &Path) -> PathBuf {
    if let Ok(v) = env::var("TENTABUS_SEED_HOME") {
        return PathBuf::from(v);
    }
    db_path
        .parent()
        .and_then(Path::parent)
        .map(|home| home.to_path_buf())
        .expect(
            "TENTABUS_SEED_DB must have at least two parent components \
             (<home>/data/tentaflow.db) or TENTABUS_SEED_HOME must be set explicitly",
        )
}

fn call_ctx(instance_id: &BusInstanceId) -> BusCallContext {
    BusCallContext {
        instance_id: instance_id.clone(),
        org_id: DEFAULT_ORG_ID.to_string(),
        actor: Some(ACTOR.to_string()),
        correlation_id: Some("bus-demo-seed".to_string()),
        origin: ORIGIN.to_string(),
    }
}

/// Opens the target main db (runs the crate's normal migrations — a no-op
/// on an already-migrated db) and points every `fs_sandbox`/`paths` call
/// this process makes at `TENTABUS_SEED_HOME` — same mechanism
/// `tests/tentabus_two_instances.rs` uses (`std::env::set_var`) to make a
/// single process address one specific home instead of the developer's
/// real `.runtime/`. Also reconciles the native package catalog
/// (`bundled::install_native_packages`) so `tentabus` v1.0.0 exists to
/// install instances of — a no-op if a prior run (or a real server boot)
/// already did this against the same db.
fn open_platform() -> DbPool {
    let db_path = seed_db_path();
    let home = seed_home(&db_path);
    println!(
        "bus_demo_seed: db={} home={}",
        db_path.display(),
        home.display()
    );
    std::env::set_var("TENTAFLOW_HOME", &home);
    std::env::set_var("HOME", &home);

    let db = db::init(&db_path).expect("open/migrate target db");
    bundled::install_native_packages(&db).expect("reconcile native package catalog");
    db
}

/// Installs (or reuses an already-installed) instance named `display_name`,
/// enabling it (`is_enabled = true`) if it was not already. Idempotent: a
/// second `seed_demo_data` run against the same db recognizes its own prior
/// instances by display name and reuses their (already real, already
/// on-disk) ids instead of minting duplicates.
fn ensure_instance(db: &DbPool, display_name: &str) -> BusInstanceId {
    let existing =
        repository::list_package_instances(db, BusInstanceId::PACKAGE_ID).unwrap_or_default();
    if let Some((addon_id, enabled, _)) = existing.iter().find(|(_, _, name)| name == display_name)
    {
        if !*enabled {
            repository::set_addon_enabled(db, addon_id, true)
                .expect("re-enable existing seeded instance");
        }
        println!("seed: reusing existing instance '{display_name}' -> {addon_id}");
        return BusInstanceId::parse(addon_id).expect("valid instance id");
    }
    let addon_id = lifecycle::install_instance(
        db,
        BusInstanceId::PACKAGE_ID,
        PACKAGE_VERSION,
        display_name,
        &BTreeMap::new(),
    )
    .unwrap_or_else(|e| panic!("install_instance('{display_name}') failed: {e}"));
    repository::set_addon_enabled(db, &addon_id, true).expect("enable freshly installed instance");
    println!("seed: installed instance '{display_name}' -> {addon_id}");
    BusInstanceId::parse(&addon_id).expect("valid instance id")
}

/// Grants `bus.read`/`bus.write`/`bus.admin` to `ACTOR` on one instance —
/// every bus permission defaults to `deny` (`bus/app-manifest.toml`, owner
/// decision 03.09.2026), so a later real server's dashboard session needs
/// this row to actually see what this harness seeded (the harness's own
/// writes below go through `AllowAllAuthorizer` and never consult this
/// table themselves).
fn grant_full_access(db: &DbPool, addon_id: &str) {
    for perm in ["bus.read", "bus.write", "bus.admin"] {
        repository::upsert_permission(db, addon_id, "user", ACTOR, perm, "allow", None)
            .expect("grant permission");
    }
}

/// Starts (or reattaches to) `instance_id`'s engine against its REAL
/// on-disk state: the per-instance SQLite db `bus::native::open_db` opens
/// (same call `native_on_enable` makes) and the real `<data dir>/log/`
/// bus directory (`fs_sandbox::addon_data_dir(...).join("log")`) — the
/// same two paths a real `native_on_enable` uses, so a real server booting
/// later against this same home finds exactly the state this harness left.
/// Also returns the instance's own local db, where the lag history lives.
fn start_engine(db: &DbPool, instance_id: &BusInstanceId) -> (Arc<BusService>, DbPool) {
    let org_id = DEFAULT_ORG_ID;
    let local_db = bus_native::open_db(db, org_id, instance_id.as_str())
        .expect("open per-instance on-disk local db");
    let bus_dir = fs_sandbox::addon_data_dir(org_id, instance_id.as_str())
        .unwrap_or_else(|e| panic!("instance data dir for '{instance_id}': {e:?}"))
        .join("log");
    let svc = bus::init_instance(BusInitConfig {
        instance_id: instance_id.clone(),
        local_db: local_db.clone(),
        bus_dir,
        db: db.clone(),
        authorizer: Arc::new(AllowAllAuthorizer),
        retention_interval: None,
        dedup_expected_rate_per_sec: 10_000,
        partition_handle_lru: None,
        publish_ack_timeout: bus::DEFAULT_PUBLISH_ACK_TIMEOUT,
    })
    .expect("bus::init_instance");
    (svc, local_db)
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
        println!(
            "seed: topic '{name}' already exists on '{}' — skipping",
            svc.instance_id()
        );
        return false;
    }
    svc.create_topic(ctx, name, opts).unwrap_or_else(|e| {
        panic!(
            "create_topic('{name}') on '{}' failed: {e}",
            svc.instance_id()
        )
    });
    println!("seed: created topic '{name}' on '{}'", svc.instance_id());
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
/// call on every run, seeded or not, purely for the summary. Resolves
/// through the free-function `bus::open_consumer`, which looks its engine
/// up by `ctx.instance_id` in the real multi-instance registry — the same
/// registry `start_engine` above registered this instance's engine into.
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

/// Seeds one instance's full demo scenario (lab.results + DLQ, orders.created
/// + a caught-up notifier group) — the per-instance body of `seed_demo_data`,
/// run once per `InstanceSpec`.
fn seed_instance(svc: &BusService, db: &DbPool, ctx: &BusCallContext, spec: &InstanceSpec) {
    // ---- lab.results (keyed records, ~LAB_KEY_COUNT * LAB_RECORDS_PER_KEY) --
    let lab_created = ensure_topic(
        svc,
        db,
        ctx,
        LAB_TOPIC,
        topics::TopicOptions {
            partitions: Some(spec.lab_partitions),
            ..Default::default()
        },
    );
    if lab_created {
        let mut records = Vec::with_capacity(spec.lab_key_count * spec.lab_records_per_key);
        let mut seq = 0usize;
        for key_idx in 1..=spec.lab_key_count {
            let key = format!("P-{key_idx:04}");
            for _ in 0..spec.lab_records_per_key {
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
            "seed[{}]: publishing {} records to '{LAB_TOPIC}'",
            svc.instance_id(),
            records.len()
        );
        publish_chunked(svc, ctx, LAB_TOPIC, records);
    }

    // ---- orders.created (~orders_record_count records) --------------------
    let orders_created = ensure_topic(
        svc,
        db,
        ctx,
        ORDERS_TOPIC,
        topics::TopicOptions {
            partitions: Some(spec.orders_partitions),
            content_type: Some("application/json".to_string()),
            ..Default::default()
        },
    );
    if orders_created {
        let records: Vec<PublishRecord> = (0..spec.orders_record_count)
            .map(|seq| PublishRecord {
                key: None,
                headers: seed_headers(),
                payload: order_payload(seq),
                timestamp_ms: now_ms(),
                schema_id: 0,
            })
            .collect();
        println!(
            "seed[{}]: publishing {} records to '{ORDERS_TOPIC}'",
            svc.instance_id(),
            records.len()
        );
        publish_chunked(svc, ctx, ORDERS_TOPIC, records);
    }

    let lab_cfg = topics::get_topic(db, svc.instance_id(), &ctx.org_id, LAB_TOPIC)
        .expect("get_topic")
        .expect("lab.results must exist by now");
    let orders_cfg = topics::get_topic(db, svc.instance_id(), &ctx.org_id, ORDERS_TOPIC)
        .expect("get_topic")
        .expect("orders.created must exist by now");

    // ---- billing group on lab.results: partial commit -> visible lag ------
    // Only runs the FIRST time lab.results is seeded on THIS instance: a
    // re-run's `ensure_topic` skip is what keeps a second `seed_demo_data`
    // pass a no-op here (re-deriving fresh commit targets and re-injecting
    // DLQ failures on a re-run would try to commit BACKWARDS past what a
    // prior run's DLQ processing already advanced the offset to).
    if lab_created {
        let billing_handle = bus::open_consumer(
            ctx,
            BILLING_GROUP,
            &[LAB_TOPIC.to_string()],
            ConsumerConfig {
                commit_mode: groups::CommitMode::Explicit,
            },
        )
        .expect("open_consumer(billing)");
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
            "seed[{}]: billing group committed {}% of every lab.results partition",
            svc.instance_id(),
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
             {needed} for the DLQ/attempts scenario — grow the spec's lab record count"
        );

        let mut offset = committed;
        for _ in 0..DLQ_RECORD_COUNT {
            let record = &dlq_recs[offset as usize];
            let mut outcome = None;
            for _ in 0..lab_cfg.max_delivery_attempts {
                outcome = Some(
                    svc.note_delivery_failure(
                        ctx,
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
            "seed[{}]: sent {DLQ_RECORD_COUNT} records from partition {dlq_partition} to \
             '__dlq.{LAB_TOPIC}' (billing group committed offset now {offset})",
            svc.instance_id()
        );

        // 3 more records with attempts > 0 but under the DLQ threshold.
        let attempts_only = (lab_cfg.max_delivery_attempts.saturating_sub(1)).max(1);
        for _ in 0..ATTEMPTS_ONLY_RECORD_COUNT {
            let record = &dlq_recs[offset as usize];
            let mut outcome = None;
            for _ in 0..attempts_only {
                outcome = Some(
                    svc.note_delivery_failure(
                        ctx,
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
            "seed[{}]: left {ATTEMPTS_ONLY_RECORD_COUNT} records in billing/lab.results with \
             attempts={attempts_only} (not yet in DLQ)",
            svc.instance_id()
        );
    } else {
        println!(
            "seed[{}]: 'lab.results' already existed — skipping billing/DLQ scenario",
            svc.instance_id()
        );
    }

    // ---- notifier group on orders.created: fully caught up ----------------
    if orders_created {
        let notifier_handle = bus::open_consumer(
            ctx,
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
        println!(
            "seed[{}]: notifier group fully caught up on orders.created",
            svc.instance_id()
        );
    } else {
        println!(
            "seed[{}]: 'orders.created' already existed — skipping notifier scenario",
            svc.instance_id()
        );
    }

    // ---- summary (read-only: safe whether this run seeded or skipped) -----
    let dlq_topic = dlq::dlq_topic_name(LAB_TOPIC);
    println!(
        "\n=== bus_demo_seed summary: instance '{}' ===",
        svc.instance_id()
    );
    println!("topic '{LAB_TOPIC}': {} partitions", lab_cfg.partitions);
    let billing_lag = group_lag(ctx, BILLING_GROUP, LAB_TOPIC);
    let mut lab_total = 0u64;
    for (tp, lag) in &billing_lag {
        let hw = partition_high_watermark(svc, ctx, LAB_TOPIC, tp.partition);
        lab_total += hw;
        println!(
            "  partition {}: {hw} records, billing committed={}, lag={lag}",
            tp.partition,
            hw - lag
        );
    }
    println!("  total records published: {lab_total}");

    let notifier_lag = group_lag(ctx, NOTIFIER_GROUP, ORDERS_TOPIC);
    let orders_total: u64 = (0..orders_cfg.partitions)
        .map(|p| partition_high_watermark(svc, ctx, ORDERS_TOPIC, p))
        .sum();
    let notifier_lag_total: u64 = notifier_lag.iter().map(|(_, lag)| lag).sum();
    println!(
        "topic '{ORDERS_TOPIC}': {} partitions, {orders_total} records published, \
         notifier total lag={notifier_lag_total}",
        orders_cfg.partitions
    );

    let dlq_count: u64 = (0..lab_cfg.partitions)
        .map(|p| partition_high_watermark(svc, ctx, &dlq_topic, p))
        .sum();
    println!("DLQ topic '{dlq_topic}': {dlq_count} records (expected >= {DLQ_RECORD_COUNT})");
    assert!(
        dlq_count >= DLQ_RECORD_COUNT,
        "expected at least {DLQ_RECORD_COUNT} records in '{dlq_topic}' on instance '{}', \
         found {dlq_count}",
        svc.instance_id()
    );
    println!("=== end summary: instance '{}' ===\n", svc.instance_id());
}

#[test]
#[ignore]
fn seed_demo_data() {
    let db = open_platform();

    for spec in &INSTANCE_SPECS {
        let instance_id = ensure_instance(&db, spec.display_name);
        grant_full_access(&db, instance_id.as_str());
        let (svc, _local_db) = start_engine(&db, &instance_id);
        let ctx = call_ctx(&instance_id);
        seed_instance(&svc, &db, &ctx, spec);
        bus::stop_instance(&instance_id);
    }

    println!(
        "bus_demo_seed: seeded {} instance(s): {}",
        INSTANCE_SPECS.len(),
        INSTANCE_SPECS
            .iter()
            .map(|s| s.display_name)
            .collect::<Vec<_>>()
            .join(", ")
    );
}

#[test]
#[ignore]
fn wipe_demo_data() {
    let db = open_platform();

    for spec in &INSTANCE_SPECS {
        let existing =
            repository::list_package_instances(&db, BusInstanceId::PACKAGE_ID).unwrap_or_default();
        let Some((addon_id, _, _)) = existing
            .into_iter()
            .find(|(_, _, name)| name == spec.display_name)
        else {
            println!(
                "wipe: instance '{}' is not installed — nothing to wipe",
                spec.display_name
            );
            continue;
        };
        // Release this process's own flock/handles first (a no-op if this
        // process never started this instance's engine, e.g. a wipe run
        // right after a separate seed process already exited).
        if let Ok(instance_id) = BusInstanceId::parse(&addon_id) {
            bus::stop_instance(&instance_id);
        }
        lifecycle::uninstall_instance(&addon_id, &db)
            .unwrap_or_else(|e| panic!("uninstall_instance('{addon_id}') failed: {e}"));
        println!(
            "wipe: uninstalled instance '{}' ({addon_id}) — data dir and all rows removed",
            spec.display_name
        );
    }
}

// =============================================================================
// "Przychodnia Zdrowie" — the TentaBus UI mockup world in small scale
// (`seed_clinic_data`). Every figure the Przegląd tab derives is produced by
// the real engine: lag from real commits, unprocessed messages from real
// delivery failures, a paused group through `pause_group`. The only thing
// written directly is the lag HISTORY, because 25 minutes of it cannot be
// lived through in a test. The samples end at the real current lag and at
// the moment of seeding. The server's sampler takes its first sample one
// `lag_history::SAMPLE_INTERVAL` after the instance starts; a flat sample
// (nobody consumes) continues the rising run, but two samples further apart
// than `lag_history::MAX_SAMPLE_GAP_MS` (3 min) do not form one run — the
// node knows nothing about the lag in between. So the server has to be
// booted right after seeding (the e2e rig does), or "rośnie od" rightly
// starts over.
// =============================================================================

const CLINIC_PRODUCTION: &str = "Produkcja";
const CLINIC_TRAINING: &str = "Szkolenia";

const RESULTS_TOPIC: &str = "wyniki-badan";
const VISITS_TOPIC: &str = "wizyty";
const INVOICES_TOPIC: &str = "faktury";

const DOCTOR_APP_GROUP: &str = "aplikacja-lekarza";
const LAB_REPORTS_GROUP: &str = "raporty-laboratorium";
const REGISTRATION_GROUP: &str = "rejestracja-online";
const BILLING_SYSTEM_GROUP: &str = "system-rozliczen";

const VISIT_SCHEMA: &str = "wizyta";
const VISIT_SCHEMA_OLD: &str = "wizyta-2025";

const RESULTS_RECORDS: usize = 3000;
const VISITS_RECORDS: usize = 600;
const INVOICES_RECORDS: usize = 400;
/// Unprocessed messages of `wyniki-badan`, all failed within the last hour.
const RESULTS_UNPROCESSED: u64 = 14;
/// Minutes of rising lag history written for the lagging consumer.
const RISING_MINUTES: i64 = 25;

/// Three backward-compatible versions: each adds an optional field.
const VISIT_SCHEMA_VERSIONS: [&str; 3] = [
    r#"{"type":"object","required":["pacjent","termin"],"properties":{"pacjent":{"type":"string"},"termin":{"type":"string"}}}"#,
    r#"{"type":"object","required":["pacjent","termin"],"properties":{"pacjent":{"type":"string"},"termin":{"type":"string"},"lekarz":{"type":"string"}}}"#,
    r#"{"type":"object","required":["pacjent","termin"],"properties":{"pacjent":{"type":"string"},"termin":{"type":"string"},"lekarz":{"type":"string"},"gabinet":{"type":"string"}}}"#,
];

fn hl7_result(seq: usize) -> Bytes {
    Bytes::from(format!(
        "MSH|^~\\&|LIS|PRZYCHODNIA|HIS|PRZYCHODNIA|20260923140211||ORU^R01|MSG{seq:06}|P|2.5\r\
         PID|1||{pid:011}||Kowalski^Jan||19800101|M\r\
         OBR|1||{seq}|CBC^Morfologia\r\
         OBX|1|NM|HGB^Hemoglobina||{hgb}.{dec}|g/dL|12-16|N",
        pid = 80010112345u64 + (seq % 400) as u64,
        hgb = 12 + seq % 4,
        dec = seq % 10,
    ))
}

fn visit_json(seq: usize) -> Bytes {
    Bytes::from(format!(
        "{{\"pacjent\":\"P-{p:04}\",\"termin\":\"2026-09-{d:02}T{h:02}:00:00\",\"lekarz\":\"L-{l:02}\"}}",
        p = seq % 500,
        d = 1 + seq % 28,
        h = 8 + seq % 9,
        l = seq % 12,
    ))
}

fn invoice_xml(seq: usize) -> Bytes {
    Bytes::from(format!(
        "<faktura><numer>FV/{seq:05}/2026</numer><kwota>{kw}.00</kwota><pacjent>P-{p:04}</pacjent></faktura>",
        kw = 80 + seq % 400,
        p = seq % 500,
    ))
}

fn publish_generated(
    svc: &BusService,
    ctx: &BusCallContext,
    topic: &str,
    count: usize,
    keyed: bool,
    payload: fn(usize) -> Bytes,
) {
    let records = (0..count)
        .map(|seq| PublishRecord {
            key: keyed.then(|| Bytes::from(format!("P-{:04}", seq % 500))),
            headers: seed_headers(),
            payload: payload(seq),
            timestamp_ms: now_ms(),
            schema_id: 0,
        })
        .collect();
    publish_chunked(svc, ctx, topic, records);
}

/// Opens `group` on `topic`, reads everything and commits `fraction` of each
/// partition. Returns the fetched records by partition and the committed
/// offsets.
fn consume_fraction(
    ctx: &BusCallContext,
    group: &str,
    topic: &str,
    fraction: f64,
) -> (BTreeMap<u32, Vec<FetchedRecordMeta>>, BTreeMap<u32, u64>) {
    let handle = bus::open_consumer(
        ctx,
        group,
        &[topic.to_string()],
        ConsumerConfig {
            commit_mode: groups::CommitMode::Explicit,
        },
    )
    .unwrap_or_else(|e| panic!("open_consumer('{group}') failed: {e}"));
    let fetched = handle
        .fetch(64 * 1024 * 1024, 10_000)
        .unwrap_or_else(|e| panic!("fetch('{group}') failed: {e}"));
    let by_part = by_partition(fetched.records);
    let committed: BTreeMap<u32, u64> = by_part
        .iter()
        .map(|(&p, recs)| (p, ((recs.len() as f64) * fraction).floor() as u64))
        .collect();
    let targets: Vec<(TopicPartition, u64)> = committed
        .iter()
        .filter(|(_, &c)| c > 0)
        .map(|(&p, &c)| {
            (
                TopicPartition {
                    topic: topic.to_string(),
                    partition: p,
                },
                c,
            )
        })
        .collect();
    if !targets.is_empty() {
        handle
            .commit(&targets)
            .unwrap_or_else(|e| panic!("commit('{group}') failed: {e}"));
    }
    (by_part, committed)
}

fn total_lag(ctx: &BusCallContext, group: &str, topic: &str) -> (u64, u64) {
    let per_partition = group_lag(ctx, group, topic);
    let lag: u64 = per_partition.iter().map(|(_, l)| l).sum();
    (per_partition.len() as u64, lag)
}

fn seed_clinic_production(svc: &BusService, local_db: &DbPool, db: &DbPool, ctx: &BusCallContext) {
    let instance = svc.instance_id().to_string();

    // ---- message patterns: one bound to `wizyty`, one withdrawn ------------
    let existing = repository::bus_schema_subject_get(db, &instance, &ctx.org_id, VISIT_SCHEMA)
        .expect("read pattern wizyta");
    if existing.is_some() {
        println!("seed[clinic]: pattern '{VISIT_SCHEMA}' already exists — skipping");
    } else {
        // Withdrawing ONE version (not the whole pattern) needs registry
        // support that is not there yet (`registry::delete` refuses
        // `deprecate_only` with a version), so all three stay current.
        for text in VISIT_SCHEMA_VERSIONS {
            schema_registry::register(
                db,
                &instance,
                &ctx.org_id,
                VISIT_SCHEMA,
                SchemaType::JsonSchema,
                text,
                Some(Compatibility::Backward),
                Some(ACTOR),
            )
            .expect("register a version of pattern wizyta");
        }
        schema_registry::register(
            db,
            &instance,
            &ctx.org_id,
            VISIT_SCHEMA_OLD,
            SchemaType::JsonSchema,
            VISIT_SCHEMA_VERSIONS[0],
            Some(Compatibility::Backward),
            Some(ACTOR),
        )
        .expect("register pattern wizyta-2025");
        schema_registry::delete(db, &instance, &ctx.org_id, VISIT_SCHEMA_OLD, None, true)
            .expect("withdraw pattern wizyta-2025");
    }

    // ---- wyniki-badan: HL7 v2, one consumer behind, one caught up -----------
    if ensure_topic(
        svc,
        db,
        ctx,
        RESULTS_TOPIC,
        topics::TopicOptions {
            partitions: Some(3),
            content_type: Some("application/hl7-v2".to_string()),
            ..Default::default()
        },
    ) {
        publish_generated(svc, ctx, RESULTS_TOPIC, RESULTS_RECORDS, true, hl7_result);
        let cfg = topics::get_topic(db, svc.instance_id(), &ctx.org_id, RESULTS_TOPIC)
            .expect("get_topic")
            .expect("wyniki-badan exists");
        let (records, committed) = consume_fraction(ctx, DOCTOR_APP_GROUP, RESULTS_TOPIC, 0.25);
        // The consumer's program reports failures on the records right after
        // what it committed; each exhausts its attempts and becomes an
        // unprocessed message of the last hour.
        let mut sent = 0u64;
        'outer: for (&partition, recs) in &records {
            let mut offset = committed[&partition];
            while (offset as usize) < recs.len() {
                if sent == RESULTS_UNPROCESSED {
                    break 'outer;
                }
                let record = &recs[offset as usize];
                for _ in 0..cfg.max_delivery_attempts {
                    svc.note_delivery_failure(
                        ctx,
                        DOCTOR_APP_GROUP,
                        RESULTS_TOPIC,
                        partition,
                        offset,
                        record,
                        dlq::DlqReason::ConsumerError,
                        "przekroczono czas zapisu wyniku do karty pacjenta",
                    )
                    .expect("note_delivery_failure");
                }
                sent += 1;
                offset += 1;
                if sent % 5 == 0 {
                    continue 'outer;
                }
            }
        }
        consume_fraction(ctx, LAB_REPORTS_GROUP, RESULTS_TOPIC, 1.0);
    }

    // ---- wizyty: JSON bound to the pattern, a consumer slightly behind ------
    if ensure_topic(
        svc,
        db,
        ctx,
        VISITS_TOPIC,
        topics::TopicOptions {
            partitions: Some(3),
            content_type: Some("application/json".to_string()),
            schema_id: Some(VISIT_SCHEMA.to_string()),
            ..Default::default()
        },
    ) {
        publish_generated(svc, ctx, VISITS_TOPIC, VISITS_RECORDS, true, visit_json);
        consume_fraction(ctx, REGISTRATION_GROUP, VISITS_TOPIC, 0.9);
    }

    // ---- faktury: XML, the billing system paused with everything waiting ----
    if ensure_topic(
        svc,
        db,
        ctx,
        INVOICES_TOPIC,
        topics::TopicOptions {
            partitions: Some(2),
            content_type: Some("application/xml".to_string()),
            ..Default::default()
        },
    ) {
        publish_generated(
            svc,
            ctx,
            INVOICES_TOPIC,
            INVOICES_RECORDS,
            false,
            invoice_xml,
        );
        consume_fraction(ctx, BILLING_SYSTEM_GROUP, INVOICES_TOPIC, 0.0);
        svc.pause_group(ctx, BILLING_SYSTEM_GROUP, INVOICES_TOPIC)
            .expect("pause system-rozliczen");
    }

    // ---- 25 minutes of rising lag for the consumer that falls behind -------
    let (_, lag_now) = total_lag(ctx, DOCTOR_APP_GROUP, RESULTS_TOPIC);
    let now = now_ms();
    let start_lag = lag_now / 4;
    for step in 0..=RISING_MINUTES {
        let at_ms = now - (RISING_MINUTES - step) * 60_000;
        let lag = start_lag + (lag_now - start_lag) * step as u64 / RISING_MINUTES as u64;
        lag_history::record(
            local_db,
            at_ms,
            &[lag_history::GroupSample {
                org_id: ctx.org_id.clone(),
                group_id: DOCTOR_APP_GROUP.to_string(),
                topic: RESULTS_TOPIC.to_string(),
                lag_total: lag,
                committed_total: (RESULTS_RECORDS as u64).saturating_sub(lag),
            }],
            &[],
        )
        .expect("record lag history");
    }
    println!(
        "seed[clinic]: '{DOCTOR_APP_GROUP}' lag {lag_now}, rising for {RISING_MINUTES} min in the history"
    );
    for (group, topic) in [
        (LAB_REPORTS_GROUP, RESULTS_TOPIC),
        (REGISTRATION_GROUP, VISITS_TOPIC),
        (BILLING_SYSTEM_GROUP, INVOICES_TOPIC),
    ] {
        let (partitions, lag) = total_lag(ctx, group, topic);
        println!("seed[clinic]: '{group}' on '{topic}': {partitions} partitions, lag {lag}");
    }
}

#[test]
#[ignore]
fn seed_clinic_data() {
    let db = open_platform();

    let production = ensure_instance(&db, CLINIC_PRODUCTION);
    grant_full_access(&db, production.as_str());
    let (svc, local_db) = start_engine(&db, &production);
    seed_clinic_production(&svc, &local_db, &db, &call_ctx(&production));
    bus::stop_instance(&production);

    // "Szkolenia" stays empty on purpose: the T11 empty states.
    let training = ensure_instance(&db, CLINIC_TRAINING);
    grant_full_access(&db, training.as_str());
    let (_svc, _local_db) = start_engine(&db, &training);
    bus::stop_instance(&training);

    println!("bus_demo_seed: seeded '{CLINIC_PRODUCTION}' ({production}) and empty '{CLINIC_TRAINING}' ({training})");
}

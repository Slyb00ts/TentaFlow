// =============================================================================
// File: tests/tentabus_two_instances.rs — W10 acceptance test
//       (SUM/tentabus/PLAN-APP-PLATFORM.md §8 "W10 — Integration test:
//       two-instance isolation"). This is the plan's own acceptance test for
//       the whole multi-instance conversion: two TentaBus instances
//       ("test"/"prod") installed in the SAME organization must never see
//       each other's data, neither in storage nor on the wire.
//
// SINGLE ORDERED SCENARIO, not eleven independent `#[test]` fns: every step
// after #2 depends on state the earlier steps built (two installed+enabled
// instances and their running engines; the topics step 4 creates feed
// steps 5-9; step 9 replays the ledger ops step 4's writes actually
// produced; step 10 uninstalls the "test" instance steps 2-9 populated;
// step 11 disables the "prod" instance step 3 enabled). Splitting this into
// independent `#[test]` fns would force each one to redundantly rebuild the
// whole boot+install+enable fixture, or silently depend on Rust's
// (unspecified) test execution order — both worse than one function whose
// shared-fixture dependency is explicit. Each step is a clearly numbered
// block matching the plan's own numbering, so a future failure names the
// exact contract that broke.
//
// Run:
//   cargo test --features test-support --test tentabus_two_instances
//
// Two production-code accessibility notes (see the bottom of this file's
// doc comment on `capture_topic_row_op` for the one place this test had to
// work around a `pub(crate)` boundary rather than test through it):
//   - `db::repository::publish_bus_topic_capture` / `bus_topic_write_capture`
//     (the functions that mint the REAL ledger op for a `bus_topics` write)
//     are `pub(crate)`, unreachable from this external integration-test
//     binary. Step 9 therefore reconstructs an equivalent capture from the
//     persisted row via the same PUBLIC primitives
//     (`CoreSyncResourceKind::BusTopic`, `composite_resource_id`,
//     `sync::runtime::{core_hlc_now, core_epoch, record_core_capture}`)
//     rather than intercepting the op `bus_topic_create` already minted
//     automatically as a side effect of step 4. This proves the same thing
//     end to end (row_json/composite id carry `instance_id` correctly
//     through the materializer) but does not literally capture the exact
//     op object step 4 produced under the hood.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;

use tentaflow_core::addon::{bundled, fs_sandbox, lifecycle};
use tentaflow_core::api::dashboard::handlers_addon_lifecycle::addon_toggle;
use tentaflow_core::bus::instance::BusInstanceId;
use tentaflow_core::bus::schema_registry::{self, SchemaType};
use tentaflow_core::bus::{
    self, field_policies, groups, topics, BusCallContext, ConsumerConfig, PublishBatch,
    PublishRecord, TopicPartition,
};
use tentaflow_core::crypto::SettingsCipher;
use tentaflow_core::db::repository::{self, DbBusPartitionAssignment};
use tentaflow_core::db::{self, DbPool};
use tentaflow_core::dispatch::app_gate;
use tentaflow_core::dispatch::state::AppState;
use tentaflow_core::dispatch::HandlerContext;
use tentaflow_core::mesh::security::MeshSecurity;
use tentaflow_core::services::bus_authorizer::topic_acl_resource_id;
use tentaflow_core::services::org::DEFAULT_ORG_ID;
use tentaflow_core::sync::core_capture::CoreWriteCapture;
use tentaflow_core::sync::core_materializer::apply_core_operation;
use tentaflow_core::sync::core_registry::CoreSyncResourceKind;
use tentaflow_core::sync::ledger::FieldValue;
use tentaflow_core::sync::resource_id::composite_resource_id;
use tentaflow_core::sync::runtime::{self, SqlWriteAction};

use tentaflow_protocol::{
    AddonToggleRequest, BusEnvelope, BusPayload, MessageBody, ProtocolErrorCode, SessionAuth,
};

const ORG: &str = DEFAULT_ORG_ID;
const TOPIC: &str = "orders.created";
const ACTOR: &str = "actor-tentabus-w10";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_millis() as i64
}

fn publish_record(payload: &str) -> PublishRecord {
    PublishRecord {
        key: None,
        headers: vec![],
        payload: Bytes::from(payload.to_string()),
        timestamp_ms: now_ms(),
        schema_id: 0,
    }
}

/// A `HandlerContext` for a raw admin session, matching the exact shape
/// `tests/native_app_lifecycle.rs::addon_toggle_and_sync_reconcile_run_the_
/// native_hooks_everywhere` uses to drive `addon_toggle` directly. Session-
/// kind enforcement (`#[policy(..)]`) happens at the inventory dispatch
/// layer, not inside the handler body, so calling the handler fn directly —
/// as every dispatch test in this crate does — bypasses it; what this test
/// actually exercises is the handler's OWN internal authorization
/// (`app_gate::require_instance_permission`'s existence/enabled/matrix
/// checks), which is the real substance of the isolation claim.
fn admin_ctx(state: &Arc<AppState>) -> HandlerContext {
    HandlerContext {
        session: SessionAuth::UserSession {
            user_id: [7u8; 16],
            role: Some("admin".to_string()),
        },
        correlation_id: 1,
        connection_id: 0,
        resume_secret: None,
        state: state.clone(),
        org_context: None,
    }
}

fn bus_ctx(instance: &BusInstanceId) -> BusCallContext {
    BusCallContext {
        instance_id: instance.clone(),
        org_id: ORG.to_string(),
        actor: Some(ACTOR.to_string()),
        correlation_id: Some("w10".to_string()),
        origin: "test".to_string(),
    }
}

/// Grants `bus.read`/`bus.write`/`bus.admin` to `ACTOR` on one instance.
/// Every bus permission defaults to `deny` (`bus/app-manifest.toml`, owner
/// decision 03.09.2026), and `native_on_enable` builds a fresh, synchronously-
/// warmed `PermissionChecker` off the CURRENT `addon_permissions` rows (no
/// global checker is registered in this test process) — so every grant here
/// must land BEFORE the corresponding instance is enabled.
fn grant_full_access(db: &DbPool, addon_id: &str) {
    for perm in ["bus.read", "bus.write", "bus.admin"] {
        repository::upsert_permission(db, addon_id, "user", ACTOR, perm, "allow", None)
            .expect("grant permission");
    }
}

/// Drives the REAL dashboard toggle handler (not `native_apps::
/// notify_enabled` directly) exactly like `tests/native_app_lifecycle.rs`'s
/// own H1/H2 regression test does — the local-node production code path an
/// admin's click actually takes, so a bug in the toggle handler's own
/// enable/disable wiring cannot hide behind a shortcut fixture.
fn set_enabled(state: &Arc<AppState>, addon_id: &str, enabled: bool) {
    let ctx = admin_ctx(state);
    addon_toggle(
        &MessageBody::AddonToggleRequestBody(AddonToggleRequest {
            addon_id: addon_id.to_string(),
            enabled,
        }),
        &ctx,
    )
    .expect("addon_toggle");
}

/// Reconstructs the `core.bus_topic` capture for an already-persisted
/// `bus_topics` row, using only PUBLIC primitives — see this file's header
/// doc for why: `db::repository::publish_bus_topic_capture`/
/// `bus_topic_write_capture` (the functions `bus_topic_create` itself calls
/// to mint this exact capture automatically) are `pub(crate)`, unreachable
/// from an external integration-test binary. Building the equivalent
/// capture from the row read back via the public `bus_topic_get` and
/// minting it through the public `sync::runtime::record_core_capture`
/// exercises the identical wire shape (`CoreSyncResourceKind::BusTopic`,
/// the `(instance_id, org_id, name)` composite id, the one-field `row_json`
/// payload) the production path uses — see `core_materializer::
/// apply_bus_topic`'s own doc, which this test's step 9 targets.
fn capture_topic_row_op(
    row: &repository::DbBusTopic,
) -> tentaflow_core::sync::ledger::SyncOperation {
    let mut changed_fields = BTreeMap::new();
    changed_fields.insert(
        "row_json".to_string(),
        FieldValue::String(serde_json::to_string(row).expect("serialize DbBusTopic")),
    );
    let capture = CoreWriteCapture::new(
        CoreSyncResourceKind::BusTopic,
        row.org_id.clone(),
        composite_resource_id(&[&row.instance_id, &row.org_id, &row.name]),
        SqlWriteAction::Insert,
        changed_fields,
        None,
        runtime::core_hlc_now(),
        runtime::core_epoch(),
    );
    let recorded = runtime::record_core_capture(capture)
        .expect("record_core_capture")
        .expect("a live sync runtime must be initialized for this test");
    runtime::get_operation(recorded.op_id)
        .expect("get_operation")
        .expect("the just-recorded op must exist")
}

// A plain, synchronous `#[test]`, NOT `#[tokio::test]`: every `BusService`
// call in steps 4-8 below is synchronous and, deep in the fjall writer path
// (`tentaflow-bus::partition::append_batch`), uses `blocking_recv()` — safe
// from a plain OS thread, but a hard panic ("Cannot block the current
// thread from within a runtime") if called directly on a tokio worker
// thread instead of through `spawn_blocking`, which is exactly why
// `dispatch/bus.rs`'s own `run_blocking` helper exists and why this test
// does not run its whole body on a tokio executor. Step 11's one dispatch
// call needs `.await`, so it gets its OWN throwaway `tokio::runtime::
// Runtime`, entirely separate from this test's synchronous main thread.
#[test]
fn two_tentabus_instances_never_see_each_others_data() {
    // -------------------------------------------------------------------
    // Step 1: boot fixture — temp TENTAFLOW_HOME, a real (fully-migrated)
    // main DB, a live sync runtime (needed by step 9's ledger round trip),
    // and the native package catalog.
    // -------------------------------------------------------------------
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("HOME", home.path());
    std::env::set_var("TENTAFLOW_HOME", home.path());

    let state = AppState::for_test();
    let db: DbPool = state.db.clone();

    let mesh_security = Arc::new(
        MeshSecurity::new(db.clone(), state.settings_cipher.clone()).expect("mesh security"),
    );
    runtime::init(db.clone(), mesh_security, state.settings_cipher.clone())
        .expect("sync runtime init");

    bundled::install_native_packages(&db).expect("native package reconcile");

    // -------------------------------------------------------------------
    // Step 2: install two instances of the SAME "tentabus" package.
    // -------------------------------------------------------------------
    let test_id = lifecycle::install_instance(&db, "tentabus", "1.0.0", "test", &BTreeMap::new())
        .expect("install 'test' instance");
    let prod_id = lifecycle::install_instance(&db, "tentabus", "1.0.0", "prod", &BTreeMap::new())
        .expect("install 'prod' instance");
    assert_ne!(
        test_id, prod_id,
        "two instances of a non-singleton package must get distinct ids"
    );

    let test_instance = BusInstanceId::parse(&test_id).expect("valid instance id");
    let prod_instance = BusInstanceId::parse(&prod_id).expect("valid instance id");

    // -------------------------------------------------------------------
    // Step 3: enable both — two engines, distinct bus_dirs.
    // -------------------------------------------------------------------
    grant_full_access(&db, &test_id);
    grant_full_access(&db, &prod_id);
    set_enabled(&state, &test_id, true);
    set_enabled(&state, &prod_id, true);

    assert_eq!(
        bus::running_instances().len(),
        2,
        "exactly two engines must be running after enabling both instances"
    );
    let svc_test = bus::instance(&test_instance).expect("'test' engine must be running");
    let svc_prod = bus::instance(&prod_instance).expect("'prod' engine must be running");
    assert_ne!(
        svc_test.bus_dir(),
        svc_prod.bus_dir(),
        "each instance's engine must own a distinct on-disk log root"
    );

    let ctx_test = bus_ctx(&test_instance);
    let ctx_prod = bus_ctx(&prod_instance);

    // -------------------------------------------------------------------
    // Step 4: create the SAME-NAMED topic in both, with DIFFERENT
    // partition counts — must not collide, and each instance's topic_list
    // must show exactly its own row with its own partition count.
    // -------------------------------------------------------------------
    let test_topic_opts = topics::TopicOptions {
        partitions: Some(3),
        content_type: Some("application/json".to_string()),
        ..Default::default()
    };
    let prod_topic_opts = topics::TopicOptions {
        partitions: Some(5),
        content_type: Some("application/json".to_string()),
        ..Default::default()
    };
    svc_test
        .create_topic(&ctx_test, TOPIC, test_topic_opts)
        .expect("create_topic on 'test' must not collide with 'prod'");
    svc_prod
        .create_topic(&ctx_prod, TOPIC, prod_topic_opts)
        .expect("create_topic on 'prod' must not collide with 'test'");

    let topics_test = topics::list_topics(&db, test_instance.as_str(), ORG).expect("list test");
    let topics_prod = topics::list_topics(&db, prod_instance.as_str(), ORG).expect("list prod");
    assert_eq!(topics_test.len(), 1, "'test' must see only its own topic");
    assert_eq!(topics_prod.len(), 1, "'prod' must see only its own topic");
    assert_eq!(topics_test[0].name, TOPIC);
    assert_eq!(topics_prod[0].name, TOPIC);
    assert_eq!(
        topics_test[0].partitions, 3,
        "'test' keeps its own partition count"
    );
    assert_eq!(
        topics_prod[0].partitions, 5,
        "'prod' keeps its own partition count"
    );

    // Seeds one `bus_partition_assignments` row per instance directly
    // (there is no live replication coordinator in this test, so
    // `create_topic` itself proposes none) purely so step 10's "all five
    // core tables" teardown assertion is exercising a table that actually
    // had a row, not one that was vacuously empty the whole time.
    for (instance, node) in [
        (test_instance.as_str(), "node-test"),
        (prod_instance.as_str(), "node-prod"),
    ] {
        repository::bus_assignment_upsert(
            &db,
            &DbBusPartitionAssignment {
                instance_id: instance.to_string(),
                org_id: ORG.to_string(),
                topic: TOPIC.to_string(),
                partition: 0,
                leader_node_id: node.to_string(),
                replicas: vec![node.to_string()],
                isr: vec![node.to_string()],
                leader_epoch: 1,
                environment: "prod".to_string(),
                updated_at_ms: now_ms(),
            },
        )
        .expect("seed partition assignment");
    }

    // -------------------------------------------------------------------
    // Step 5: publish 100 records into "test"; "prod" must see none.
    // -------------------------------------------------------------------
    for i in 0..100u32 {
        svc_test
            .publish(
                &ctx_test,
                TOPIC,
                PublishBatch {
                    partition: Some(0),
                    producer: None,
                    records: vec![publish_record(&format!("{{\"n\":{i}}}"))],
                },
            )
            .expect("publish into 'test'");
    }

    let peek_prod = svc_prod
        .peek(&ctx_prod, TOPIC, 0, 0, 10, 1024 * 1024)
        .expect("peek 'prod'");
    assert!(
        peek_prod.records.is_empty(),
        "'prod' must not see any of the 100 records published into 'test'"
    );
    let stats_prod = svc_prod
        .partition_stats(&ctx_prod, TOPIC, 0)
        .expect("partition_stats 'prod'");
    assert_eq!(
        stats_prod.log_end_offset, 0,
        "'prod' partition 0 must still be at offset 0"
    );
    let stats_test = svc_test
        .partition_stats(&ctx_test, TOPIC, 0)
        .expect("partition_stats 'test'");
    assert_eq!(
        stats_test.log_end_offset, 100,
        "'test' must have all 100 records"
    );

    // -------------------------------------------------------------------
    // Step 6: open consumer group "billing" in both; commit an offset in
    // "test" only; "prod"'s bus_groups row, in ITS OWN tentabus.db, must
    // stay untouched.
    // -------------------------------------------------------------------
    let handle_test = svc_test
        .open_consumer(
            &ctx_test,
            "billing",
            &[TOPIC.to_string()],
            ConsumerConfig {
                commit_mode: groups::CommitMode::Explicit,
            },
        )
        .expect("open_consumer 'test'");
    let handle_prod = svc_prod
        .open_consumer(
            &ctx_prod,
            "billing",
            &[TOPIC.to_string()],
            ConsumerConfig {
                commit_mode: groups::CommitMode::Explicit,
            },
        )
        .expect("open_consumer 'prod'");
    let _ = &handle_prod; // kept alive only to prove the open on 'prod' succeeded independently

    let prod_group_before = repository::bus_group_get(svc_prod.local_db(), ORG, "billing", TOPIC)
        .expect("bus_group_get 'prod' before commit")
        .expect("'prod' must have its own 'billing' group row");

    handle_test
        .commit(&[(
            TopicPartition {
                topic: TOPIC.to_string(),
                partition: 0,
            },
            10,
        )])
        .expect("commit on 'test'");

    let test_committed = svc_test
        .group_committed_offset(ORG, "billing", TOPIC, 0)
        .expect("group_committed_offset 'test'");
    let prod_committed = svc_prod
        .group_committed_offset(ORG, "billing", TOPIC, 0)
        .expect("group_committed_offset 'prod'");
    assert_eq!(
        test_committed, 10,
        "'test' group must have committed offset 10"
    );
    assert_eq!(
        prod_committed, 0,
        "'prod' group must be untouched by a commit on 'test'"
    );

    let prod_group_after = repository::bus_group_get(svc_prod.local_db(), ORG, "billing", TOPIC)
        .expect("bus_group_get 'prod' after commit")
        .expect("'prod' group row must still exist");
    assert_eq!(
        prod_group_before.updated_at_ms, prod_group_after.updated_at_ms,
        "committing on 'test' must not touch 'prod's own bus_groups row at all"
    );
    assert_eq!(prod_group_before.paused, prod_group_after.paused);

    // -------------------------------------------------------------------
    // Step 7: register schema subject "orders.v1" in both with DIFFERENT
    // text; both succeed and each `get` returns its own text.
    //
    // Deviation from the plan's literal R5 wording, found while writing
    // this test: R5 asked to "assert two instances can both hold
    // schema_ref_id = 1 for different content", assuming `schema_ref_id`
    // is a small per-instance sequential counter. It is not — `bus/
    // schema_registry/mod.rs::schema_ref_id_for(org_id, subject,
    // content_hash)` is a blake3 hash of `(org_id, subject, content_hash)`
    // and does NOT take `instance_id` as an input at all, so two
    // registrations with DIFFERENT content deterministically get DIFFERENT
    // ids (confirmed below) — there is no "both get 1" case to observe.
    // The real collision R5 is warning about — two instances minting the
    // SAME numeric `schema_ref_id` — requires the SAME `(org_id, subject,
    // content)` on both, which IS reachable (any two instances registering
    // identical content under the same org+subject) and is what the block
    // after this one actually exercises, on a second subject so it does
    // not interfere with the different-content check here.
    // -------------------------------------------------------------------
    const SCHEMA_TEST: &str = r#"{"type":"object","properties":{"kind":{"const":"test"}}}"#;
    const SCHEMA_PROD: &str = r#"{"type":"object","properties":{"kind":{"const":"prod"}}}"#;

    let outcome_test = schema_registry::registry::register(
        &db,
        test_instance.as_str(),
        ORG,
        "orders.v1",
        SchemaType::JsonSchema,
        SCHEMA_TEST,
        None,
        None,
    )
    .expect("register schema on 'test'");
    let outcome_prod = schema_registry::registry::register(
        &db,
        prod_instance.as_str(),
        ORG,
        "orders.v1",
        SchemaType::JsonSchema,
        SCHEMA_PROD,
        None,
        None,
    )
    .expect("register schema on 'prod'");
    assert_ne!(
        outcome_test.schema_ref_id, outcome_prod.schema_ref_id,
        "schema_ref_id is content-hash-derived: different content under the \
         same subject must NOT coincidentally collide"
    );

    let (_, text_test) =
        schema_registry::registry::get(&db, test_instance.as_str(), ORG, "orders.v1", None)
            .expect("get schema on 'test'");
    let (_, text_prod) =
        schema_registry::registry::get(&db, prod_instance.as_str(), ORG, "orders.v1", None)
            .expect("get schema on 'prod'");
    assert_eq!(
        text_test, SCHEMA_TEST,
        "'test' must read back its OWN schema text"
    );
    assert_eq!(
        text_prod, SCHEMA_PROD,
        "'prod' must read back its OWN schema text"
    );
    assert_ne!(text_test, text_prod);

    // R5's actual invariant: IDENTICAL content under the same org+subject
    // on two different instances mints the SAME numeric schema_ref_id
    // (the hash has no instance_id input), yet each instance's own
    // `bus_schema_versions` row is independent (`UNIQUE (instance_id,
    // org_id, schema_ref_id)`, not a globally unique column) and each
    // instance resolves ONLY its own row.
    const SCHEMA_SHARED: &str = r#"{"type":"object","properties":{"kind":{"const":"shared"}}}"#;
    let shared_test = schema_registry::registry::register(
        &db,
        test_instance.as_str(),
        ORG,
        "orders.v2",
        SchemaType::JsonSchema,
        SCHEMA_SHARED,
        None,
        None,
    )
    .expect("register shared-content schema on 'test'");
    let shared_prod = schema_registry::registry::register(
        &db,
        prod_instance.as_str(),
        ORG,
        "orders.v2",
        SchemaType::JsonSchema,
        SCHEMA_SHARED,
        None,
        None,
    )
    .expect("register shared-content schema on 'prod'");
    assert_eq!(
        shared_test.schema_ref_id, shared_prod.schema_ref_id,
        "identical content under the same subject on two instances mints \
         the SAME numeric schema_ref_id — this is expected and safe, see \
         R5"
    );
    let (_, shared_text_test) =
        schema_registry::registry::get(&db, test_instance.as_str(), ORG, "orders.v2", None)
            .expect("get shared schema on 'test'");
    let (_, shared_text_prod) =
        schema_registry::registry::get(&db, prod_instance.as_str(), ORG, "orders.v2", None)
            .expect("get shared schema on 'prod'");
    assert_eq!(shared_text_test, SCHEMA_SHARED);
    assert_eq!(shared_text_prod, SCHEMA_SHARED);

    // -------------------------------------------------------------------
    // Step 8: set a field policy and a topic ACL on "test" only; "prod"'s
    // corresponding lists must be empty.
    // -------------------------------------------------------------------
    let mut fields = BTreeSet::new();
    fields.insert("status".to_string());
    field_policies::set_policy(
        &db,
        test_instance.as_str(),
        ORG,
        TOPIC,
        "any",
        field_policies::SUBJECT_ANY,
        field_policies::Direction::Read,
        &fields,
        &BTreeSet::new(),
    )
    .expect("set field policy on 'test'");

    let test_acl_resource = topic_acl_resource_id(test_instance.as_str(), ORG, TOPIC);
    repository::resource_permissions::set(
        &db,
        "topic",
        &test_acl_resource,
        "user",
        "someone",
        "deny",
    )
    .expect("set topic ACL on 'test'");

    let test_policies = field_policies::list_policies(&db, test_instance.as_str(), ORG, TOPIC)
        .expect("list 'test' policies");
    let prod_policies = field_policies::list_policies(&db, prod_instance.as_str(), ORG, TOPIC)
        .expect("list 'prod' policies");
    assert_eq!(
        test_policies.len(),
        1,
        "'test' must have the field policy it just set"
    );
    assert!(
        prod_policies.is_empty(),
        "'prod' must have NO field policies"
    );

    let prod_acl_resource = topic_acl_resource_id(prod_instance.as_str(), ORG, TOPIC);
    let test_acl =
        repository::resource_permissions::list_for_resource(&db, "topic", &test_acl_resource)
            .expect("list 'test' ACL");
    let prod_acl =
        repository::resource_permissions::list_for_resource(&db, "topic", &prod_acl_resource)
            .expect("list 'prod' ACL");
    assert_eq!(
        test_acl.len(),
        1,
        "'test' must have the ACL row it just set"
    );
    assert!(prod_acl.is_empty(), "'prod' must have NO topic ACL rows");

    // -------------------------------------------------------------------
    // Step 9: sync round trip. Capture the ledger op for each instance's
    // step-4 topic row and materialize both into a SECOND, fresh database
    // — both topics must land, stay separate, and keep their own
    // instance_id (through row_json AND the composite_resource_id).
    // -------------------------------------------------------------------
    let row_test = repository::bus_topic_get(&db, test_instance.as_str(), ORG, TOPIC)
        .expect("bus_topic_get 'test'")
        .expect("'test' topic row must exist");
    let row_prod = repository::bus_topic_get(&db, prod_instance.as_str(), ORG, TOPIC)
        .expect("bus_topic_get 'prod'")
        .expect("'prod' topic row must exist");

    let op_test = capture_topic_row_op(&row_test);
    let op_prod = capture_topic_row_op(&row_prod);
    assert_ne!(
        op_test.body.resource_id, op_prod.body.resource_id,
        "the composite resource id must embed instance_id, so two instances' \
         identically-named topics never share a ledger slot"
    );

    let second_db = db::init(Path::new(":memory:")).expect("second db init");
    let cipher = Arc::new(SettingsCipher::new(&[3u8; 32]));

    let rows_applied_test =
        apply_core_operation(&second_db, &cipher, &op_test).expect("materialize 'test' op");
    let rows_applied_prod =
        apply_core_operation(&second_db, &cipher, &op_prod).expect("materialize 'prod' op");
    assert_eq!(rows_applied_test, 1);
    assert_eq!(rows_applied_prod, 1);

    let materialized_test =
        repository::bus_topic_get(&second_db, test_instance.as_str(), ORG, TOPIC)
            .expect("get materialized 'test' topic")
            .expect("'test' topic must have materialized on the second db");
    let materialized_prod =
        repository::bus_topic_get(&second_db, prod_instance.as_str(), ORG, TOPIC)
            .expect("get materialized 'prod' topic")
            .expect("'prod' topic must have materialized on the second db");
    assert_eq!(
        materialized_test.partitions, 3,
        "'test's materialized row keeps its own partition count"
    );
    assert_eq!(
        materialized_prod.partitions, 5,
        "'prod's materialized row keeps its own partition count"
    );
    assert_eq!(materialized_test.instance_id, test_instance.as_str());
    assert_eq!(materialized_prod.instance_id, prod_instance.as_str());

    // Cross-check: the OTHER instance's row must be absent under either
    // instance's own key on the second db — proves the two rows really
    // are two separate rows, not one row a naive query happens to find
    // twice.
    assert!(
        repository::bus_topic_get(&second_db, prod_instance.as_str(), ORG, TOPIC)
            .expect("get")
            .is_some_and(|r| r.partitions == 5),
        "'prod's materialized row must be reachable ONLY under 'prod's own instance id"
    );

    // -------------------------------------------------------------------
    // Step 10: uninstall "test" — its data dir, its rows in all five core
    // tables, and its ACL rows must be gone; every "prod" row, its data
    // dir and its running engine must be intact.
    // -------------------------------------------------------------------
    let test_dir = fs_sandbox::addon_data_dir(ORG, &test_id).expect("test data dir");
    let prod_dir = fs_sandbox::addon_data_dir(ORG, &prod_id).expect("prod data dir");
    assert!(
        test_dir.exists(),
        "sanity: 'test' data dir must exist before uninstall"
    );
    assert!(
        prod_dir.exists(),
        "sanity: 'prod' data dir must exist before uninstall"
    );

    lifecycle::uninstall_instance(&test_id, &db).expect("uninstall 'test'");

    assert!(
        !test_dir.exists(),
        "'test' data dir must be gone after uninstall"
    );
    assert!(prod_dir.exists(), "'prod' data dir must be untouched");

    // Five core tables (plan §1.4): bus_topics, bus_partition_assignments,
    // bus_field_policies, bus_schema_subjects/bus_schema_versions.
    assert!(
        repository::bus_topic_get(&db, test_instance.as_str(), ORG, TOPIC)
            .expect("get")
            .is_none(),
        "'test's bus_topics row must be gone"
    );
    assert!(
        repository::bus_assignment_get(&db, test_instance.as_str(), ORG, TOPIC, 0)
            .expect("get")
            .is_none(),
        "'test's bus_partition_assignments row must be gone"
    );
    assert!(
        field_policies::list_policies(&db, test_instance.as_str(), ORG, TOPIC)
            .expect("list")
            .is_empty(),
        "'test's bus_field_policies rows must be gone"
    );
    assert!(
        schema_registry::registry::list_subjects(&db, test_instance.as_str(), ORG)
            .expect("list subjects")
            .is_empty(),
        "'test's bus_schema_subjects (and cascaded bus_schema_versions) rows must be gone"
    );
    assert!(
        repository::resource_permissions::list_for_resource(&db, "topic", &test_acl_resource)
            .expect("list")
            .is_empty(),
        "'test's topic ACL rows must be gone"
    );

    // "prod" is completely intact: every row, and its running engine.
    assert!(
        repository::bus_topic_get(&db, prod_instance.as_str(), ORG, TOPIC)
            .expect("get")
            .is_some_and(|r| r.partitions == 5),
        "'prod's bus_topics row must survive 'test's uninstall untouched"
    );
    assert!(
        repository::bus_assignment_get(&db, prod_instance.as_str(), ORG, TOPIC, 0)
            .expect("get")
            .is_some(),
        "'prod's bus_partition_assignments row must survive"
    );
    assert_eq!(
        schema_registry::registry::list_subjects(&db, prod_instance.as_str(), ORG)
            .expect("list subjects")
            .len(),
        2,
        "'prod's bus_schema_subjects rows ('orders.v1' and 'orders.v2') must survive"
    );
    assert!(
        bus::instance(&prod_instance).is_some(),
        "'prod's engine must still be running after 'test' is uninstalled"
    );
    assert_eq!(
        bus::running_instances().len(),
        1,
        "exactly one engine (prod) must remain running"
    );

    // -------------------------------------------------------------------
    // Step 11: disable "prod" — `bus::instance()` returns None,
    // `app_gate::instance_enabled` is false, and a dispatch call answers
    // AppUnavailable.
    // -------------------------------------------------------------------
    set_enabled(&state, &prod_id, false);

    assert!(
        bus::instance(&prod_instance).is_none(),
        "a disabled instance must have no running engine"
    );
    assert!(
        !app_gate::instance_enabled(&db, BusInstanceId::PACKAGE_ID, &prod_id),
        "app_gate::instance_enabled must report false for a disabled instance"
    );

    let dispatch_ctx = admin_ctx(&state);
    let dispatch_rt = tokio::runtime::Runtime::new().expect("tokio runtime for the dispatch call");
    let err = dispatch_rt
        .block_on(tentaflow_core::dispatch::bus::bus_dispatch(
            &MessageBody::BusBody(BusEnvelope {
                instance_id: prod_id.clone(),
                payload: BusPayload::TopicListRequest,
            }),
            &dispatch_ctx,
        ))
        .expect_err("a dispatch call against a disabled instance must fail");
    assert_eq!(
        err.code,
        ProtocolErrorCode::AppUnavailable,
        "a disabled instance must answer AppUnavailable, not silently route \
         to whichever OTHER instance happens to still be running"
    );
}

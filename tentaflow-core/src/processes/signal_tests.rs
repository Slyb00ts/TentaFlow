// ============ File: signal_tests.rs — File-backed signal admission and recipient lifecycle ============

use super::messages::{self, test_support::start_version};
use super::repository;
use super::runtime::{
    self,
    test_support::{actor, edge, manual_input, publish_model, stamp, Fixture},
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;
use tentaflow_protocol::processes::{
    ProcessInstance, ProcessInstancePageRequest, ProcessInstanceStatus, ProcessModel, ProcessNode,
    ProcessNodeKind, ProcessPageSpec, ProcessSignalDeclaration, ProcessSubscriptionKind,
    ProcessSubscriptionStatus, ProcessVersion,
};
use uuid::Uuid;

pub(super) fn signal_catch_model() -> tentaflow_protocol::processes::ProcessModel {
    let mut model = super::model::starter_model();
    model.target_namespace = Some("urn:orders".into());
    model.signals.push(ProcessSignalDeclaration {
        signal_id: "Signal_1".into(),
        namespace_uri: "urn:orders".into(),
        name: "Order changed".into(),
    });
    model
        .variables
        .insert("received".into(), serde_json::Value::Null);
    model.nodes.insert(
        1,
        ProcessNode {
            id: "Catch_1".into(),
            name: "Wait for order change".into(),
            kind: ProcessNodeKind::SignalCatch {
                signal_ref: "Signal_1".into(),
                output_mapping: BTreeMap::from([("received".into(), "outputs".into())]),
            },
            repeat: None,
        },
    );
    model.sequence_flows = vec![
        edge("ToCatch", "Start_1", "Catch_1"),
        edge("CatchEnd", "Catch_1", "End_1"),
    ];
    model
}

pub(super) fn signal_throw_model() -> tentaflow_protocol::processes::ProcessModel {
    let mut model = super::model::starter_model();
    model.target_namespace = Some("urn:orders".into());
    model.signals.push(ProcessSignalDeclaration {
        signal_id: "Signal_1".into(),
        namespace_uri: "urn:orders".into(),
        name: "Order changed".into(),
    });
    model
        .variables
        .insert("payload".into(), json!({"business_key":"PO-7"}));
    model.nodes.insert(
        1,
        ProcessNode {
            id: "Throw_1".into(),
            name: "Announce change".into(),
            kind: ProcessNodeKind::SignalThrow {
                signal_ref: "Signal_1".into(),
                payload_expression: "vars.payload".into(),
                ttl_seconds: 3600,
            },
            repeat: None,
        },
    );
    model.sequence_flows = vec![
        edge("ToThrow", "Start_1", "Throw_1"),
        edge("ThrowEnd", "Throw_1", "End_1"),
    ];
    model
}

pub(super) fn parallel_signal_catches(
    branches: usize,
) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = signal_catch_model();
    model.nodes.retain(|node| node.id != "Catch_1");
    model.nodes.push(ProcessNode {
        id: "Split".into(),
        name: "Open signal waits".into(),
        kind: ProcessNodeKind::ParallelGateway,
        repeat: None,
    });
    model.nodes.push(ProcessNode {
        id: "Join".into(),
        name: "Join received signals".into(),
        kind: ProcessNodeKind::ParallelGateway,
        repeat: None,
    });
    model.sequence_flows = vec![
        edge("StartSplit", "Start_1", "Split"),
        edge("JoinEnd", "Join", "End_1"),
    ];
    for ordinal in 0..branches {
        let id = format!("Catch_{ordinal}");
        model.nodes.push(ProcessNode {
            id: id.clone(),
            name: format!("Signal wait {ordinal}"),
            kind: ProcessNodeKind::SignalCatch {
                signal_ref: "Signal_1".into(),
                output_mapping: BTreeMap::new(),
            },
            repeat: None,
        });
        model
            .sequence_flows
            .push(edge(&format!("Split_{ordinal}"), "Split", &id));
        model
            .sequence_flows
            .push(edge(&format!("Join_{ordinal}"), &id, "Join"));
    }
    model
}

fn signal_namespace(mut model: ProcessModel, namespace_uri: &str) -> ProcessModel {
    model.target_namespace = Some(namespace_uri.into());
    model.signals[0].namespace_uri = namespace_uri.into();
    model
}

fn publish_for_actor(
    fixture: &Fixture,
    actor: &repository::ProcessActor,
    model: &ProcessModel,
) -> ProcessVersion {
    let definition = repository::save_definition(
        &fixture.db,
        actor,
        &stamp("save signal model"),
        None,
        0,
        "Measured signal process",
        "",
        model,
    )
    .unwrap();
    repository::publish_definition(
        &fixture.db,
        actor,
        &stamp("publish signal model"),
        &definition.definition_id,
        definition.draft_revision,
        &[],
        None,
    )
    .unwrap()
    .1
}

fn start_for_actor(
    fixture: &Fixture,
    actor: &repository::ProcessActor,
    version: &ProcessVersion,
    canonical: bool,
) -> anyhow::Result<ProcessInstance> {
    let instance_id = Uuid::new_v4().to_string();
    let variables = serde_json::to_value(&version.model.variables).unwrap();
    let command = stamp("start signal model");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = (!canonical)
        .then(|| {
            runtime::plan_start(
                &version.model,
                &instance_id,
                actor,
                &version.definition_id,
                version.version,
                variables.clone(),
                runtime::StartCause::Manual,
                at_ms,
                manual_input(&command),
                None,
            )
        })
        .transpose()?;
    let input = plan.as_ref().map_or(
        repository::ProcessPlanInput::Canonical,
        repository::ProcessPlanInput::Supplied,
    );
    repository::start_instance(
        &fixture.db,
        actor,
        &command,
        &instance_id,
        &version.definition_id,
        version.version,
        &variables,
        input,
        at_ms,
    )
}

fn assert_signal_subscription_pages(
    pool: &crate::db::DbPool,
    actor: &repository::ProcessActor,
    instance: &ProcessInstance,
    expected: u32,
    status: ProcessSubscriptionStatus,
) {
    let first = &instance.pages.as_ref().unwrap().subscriptions;
    assert_eq!(first.offset, 0);
    assert_eq!(first.total, expected);
    assert_eq!(instance.subscriptions.len(), expected.min(20) as usize);
    let mut request = ProcessInstancePageRequest {
        user_tasks: None,
        incidents: None,
        timers: None,
        subscriptions: Some(ProcessPageSpec {
            offset: 0,
            limit: 20,
        }),
        event_races: None,
        outgoing_messages: None,
        selected_user_task_id: None,
        selected_incident_id: None,
        scopes: None,
        calls: None,
        repetition_groups: None,
        repetition_occurrences: None,
        selected_repetition_group_id: None,
        selected_repetition_occurrence_id: None,
        selected_repetition_value: None,
    };
    let mut seen = BTreeSet::new();
    let mut next = Some(0);
    while let Some(offset) = next {
        request.subscriptions.as_mut().unwrap().offset = offset;
        let page =
            repository::get_instance(pool, actor, &instance.instance_id, Some(&request)).unwrap();
        assert_eq!(page.status, instance.status);
        let info = &page.pages.as_ref().unwrap().subscriptions;
        assert_eq!(info.offset, offset);
        assert_eq!(info.total, expected);
        assert!(!page.subscriptions.is_empty());
        assert!(page.subscriptions.len() <= 20);
        for subscription in &page.subscriptions {
            assert_eq!(subscription.kind, ProcessSubscriptionKind::SignalCatch);
            assert_eq!(subscription.status, status);
            assert!(seen.insert(subscription.subscription_id.clone()));
        }
        let following = offset + u32::try_from(page.subscriptions.len()).unwrap();
        assert_eq!(
            info.next_offset,
            (following < expected).then_some(following)
        );
        assert_eq!(info.has_more, info.next_offset.is_some());
        next = info.next_offset;
    }
    assert_eq!(seen.len(), expected as usize);
}

fn percentile_us(samples: &[u64], percentile: usize) -> u64 {
    assert!(!samples.is_empty());
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    sorted[((sorted.len() * percentile + 99) / 100).max(1) - 1]
}

fn signal_storage(fixture: &Fixture) -> serde_json::Value {
    let conn = fixture.db.read().unwrap();
    let page_count: i64 = conn
        .query_row("PRAGMA page_count", [], |row| row.get(0))
        .unwrap();
    let page_size: i64 = conn
        .query_row("PRAGMA page_size", [], |row| row.get(0))
        .unwrap();
    let freelist_count: i64 = conn
        .query_row("PRAGMA freelist_count", [], |row| row.get(0))
        .unwrap();
    let emissions: i64 = conn
        .query_row("SELECT COUNT(*) FROM bpmn_signal_emissions", [], |row| {
            row.get(0)
        })
        .unwrap();
    let receipts: i64 = conn
        .query_row("SELECT COUNT(*) FROM bpmn_signal_receipts", [], |row| {
            row.get(0)
        })
        .unwrap();
    let retained_payload_bytes: i64 = conn.query_row(
        "SELECT COALESCE(SUM(payload_bytes),0) FROM bpmn_signal_emissions WHERE payload_json IS NOT NULL",
        [], |row| row.get(0)).unwrap();
    drop(conn);
    let path = fixture.directory.path().join("processes.db");
    let db_bytes = std::fs::metadata(&path).unwrap().len();
    let wal_bytes = match std::fs::metadata(path.with_extension("db-wal")) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => panic!("cannot read signal measurement WAL metadata: {error}"),
    };
    json!({"db_bytes":db_bytes,"wal_bytes":wal_bytes,"page_count":page_count,
        "page_size":page_size,"freelist_count":freelist_count,"emissions":emissions,
        "receipts":receipts,"retained_payload_bytes":retained_payload_bytes})
}

#[test]
fn signal_admission_snapshots_an_open_cross_instance_catch_then_delivers_once() {
    let fixture = Fixture::new();
    let recipient_version = publish_model(&fixture, &signal_catch_model());
    let recipient = start_version(&fixture, &recipient_version);
    assert_eq!(recipient.status, ProcessInstanceStatus::Waiting);
    assert_eq!(recipient.subscriptions.len(), 1);
    assert_eq!(
        recipient.subscriptions[0].kind,
        ProcessSubscriptionKind::SignalCatch
    );
    assert_eq!(
        recipient.subscriptions[0].status,
        ProcessSubscriptionStatus::Open
    );
    assert_eq!(
        recipient.subscriptions[0].signal_name.as_deref(),
        Some("Order changed")
    );
    let opened =
        repository::list_events(&fixture.db, &fixture.owner, &recipient.instance_id, 0, 200)
            .unwrap()
            .0;
    let opened: Vec<_> = opened
        .iter()
        .filter(|event| event.kind == "signal_catch_opened")
        .collect();
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].data["signal_namespace_uri"], "urn:orders");
    assert_eq!(opened[0].data["signal_declaration_id"], "Signal_1");
    assert_eq!(
        opened[0].data["subscription_id"],
        recipient.subscriptions[0].subscription_id
    );

    let source_version = publish_model(&fixture, &signal_throw_model());
    let sender = start_version(&fixture, &source_version);
    assert_eq!(sender.status, ProcessInstanceStatus::Completed);
    let events = repository::list_events(&fixture.db, &fixture.owner, &sender.instance_id, 0, 200)
        .unwrap()
        .0;
    let admitted: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "signal_admitted")
        .collect();
    assert_eq!(admitted.len(), 1);
    assert_eq!(admitted[0].data["signal_namespace_uri"], "urn:orders");
    assert_eq!(admitted[0].data["signal_declaration_id"], "Signal_1");
    assert_eq!(admitted[0].data.as_object().unwrap().len(), 4);
    assert_eq!(
        repository::get_instance(&fixture.db, &fixture.owner, &recipient.instance_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Waiting
    );
    let at_ms = chrono::Utc::now().timestamp_millis() + 10;
    let drained = messages::drain_pending(&fixture.db, at_ms);
    drained.completion.unwrap();
    assert_eq!(drained.delivered, 1);
    let delivered =
        repository::get_instance(&fixture.db, &fixture.owner, &recipient.instance_id, None)
            .unwrap();
    assert_eq!(delivered.status, ProcessInstanceStatus::Completed);
    assert_eq!(
        delivered.variables["received"],
        json!({"business_key":"PO-7"})
    );
    assert_eq!(
        delivered.subscriptions[0].status,
        ProcessSubscriptionStatus::Consumed
    );
    let received =
        repository::list_events(&fixture.db, &fixture.owner, &recipient.instance_id, 0, 200)
            .unwrap()
            .0;
    let received: Vec<_> = received
        .iter()
        .filter(|event| event.kind == "signal_received")
        .collect();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].data["signal_id"], admitted[0].data["signal_id"]);
    assert_eq!(
        received[0].data["subscription_id"],
        recipient.subscriptions[0].subscription_id
    );
    assert_eq!(
        received[0].data["attached_token_id"],
        recipient.subscriptions[0].token_id
    );
    assert_eq!(received[0].data["source_event_id"], admitted[0].event_id);
    assert_eq!(received[0].data.as_object().unwrap().len(), 4);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert_eq!(
        repository::get_instance(&reopened, &fixture.owner, &recipient.instance_id, None)
            .unwrap()
            .subscriptions[0]
            .status,
        ProcessSubscriptionStatus::Consumed
    );
    let replay = messages::drain_pending(&reopened, at_ms + 1);
    replay.completion.unwrap();
    assert_eq!(replay.delivered, 0);
    assert_eq!(
        repository::list_events(&reopened, &fixture.owner, &recipient.instance_id, 0, 200)
            .unwrap()
            .0
            .iter()
            .filter(|event| event.kind == "signal_received")
            .count(),
        1
    );
}

#[test]
fn signal_with_zero_eligible_recipients_is_admitted_and_settled_without_a_delivery_fact() {
    let fixture = Fixture::new();
    let version = publish_model(&fixture, &signal_throw_model());
    let sender = start_version(&fixture, &version);
    assert_eq!(sender.status, ProcessInstanceStatus::Completed);
    let events = repository::list_events(&fixture.db, &fixture.owner, &sender.instance_id, 0, 200)
        .unwrap()
        .0;
    let admitted: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "signal_admitted")
        .collect();
    assert_eq!(admitted.len(), 1);
    let signal_id = admitted[0].data["signal_id"].as_str().unwrap();
    let (status, receipts): (String, i64) = fixture.db.read().unwrap().query_row(
        "SELECT e.status,(SELECT COUNT(*) FROM bpmn_signal_receipts r WHERE r.signal_id=e.signal_id) \
         FROM bpmn_signal_emissions e WHERE e.signal_id=?1",
        [signal_id], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(status, "settled");
    assert_eq!(receipts, 0);
    let drained = messages::drain_pending(&fixture.db, chrono::Utc::now().timestamp_millis() + 10);
    drained.completion.unwrap();
    assert_eq!(drained.delivered, 0);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert_eq!(
        repository::get_instance(&reopened, &fixture.owner, &sender.instance_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Completed
    );
    assert_eq!(
        repository::list_events(&reopened, &fixture.owner, &sender.instance_id, 0, 200)
            .unwrap()
            .0
            .iter()
            .filter(|event| event.kind == "signal_admitted")
            .count(),
        1
    );
}

#[test]
fn same_local_signal_id_and_display_name_in_another_namespace_are_not_recipients() {
    let fixture = Fixture::new();
    let mut foreign = signal_catch_model();
    foreign.target_namespace = Some("urn:other-orders".into());
    foreign.signals[0].namespace_uri = "urn:other-orders".into();
    let foreign_version = publish_model(&fixture, &foreign);
    let recipient = start_version(&fixture, &foreign_version);
    let sender_version = publish_model(&fixture, &signal_throw_model());
    let sender = start_version(&fixture, &sender_version);
    assert_eq!(sender.status, ProcessInstanceStatus::Completed);
    let drained = messages::drain_pending(&fixture.db, chrono::Utc::now().timestamp_millis() + 10);
    drained.completion.unwrap();
    assert_eq!(drained.delivered, 0);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let recipient =
        repository::get_instance(&reopened, &fixture.owner, &recipient.instance_id, None).unwrap();
    assert_eq!(recipient.status, ProcessInstanceStatus::Waiting);
    assert_eq!(
        recipient.subscriptions[0].status,
        ProcessSubscriptionStatus::Open
    );
    assert_eq!(
        repository::list_events(&reopened, &fixture.owner, &recipient.instance_id, 0, 200)
            .unwrap()
            .0
            .iter()
            .filter(|event| event.kind == "signal_received")
            .count(),
        0
    );
}

#[test]
fn signal_measurement_sixty_four_real_catches_drain_in_two_thirty_two_receipt_ticks() {
    let fixture = Fixture::new();
    let recipient_version = publish_model(&fixture, &parallel_signal_catches(64));
    let recipient = start_version(&fixture, &recipient_version);
    assert_eq!(recipient.status, ProcessInstanceStatus::Waiting);
    assert_signal_subscription_pages(
        &fixture.db,
        &fixture.owner,
        &recipient,
        64,
        ProcessSubscriptionStatus::Open,
    );
    let source_version = publish_model(&fixture, &signal_throw_model());
    let sender = start_version(&fixture, &source_version);
    assert_eq!(sender.status, ProcessInstanceStatus::Completed);
    let signal_id: String = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT signal_id FROM bpmn_signal_emissions WHERE source_instance_id=?1",
            [&sender.instance_id],
            |row| row.get(0),
        )
        .unwrap();
    let pending: i64 = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM bpmn_signal_receipts WHERE signal_id=?1 AND status='pending'",
            [&signal_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pending, 64);
    let at_ms = chrono::Utc::now().timestamp_millis() + 10;
    let first_started = Instant::now();
    let first = messages::drain_pending(&fixture.db, at_ms);
    let first_us = u64::try_from(first_started.elapsed().as_micros()).unwrap();
    first.completion.unwrap();
    assert_eq!(first.delivered, 32);
    assert_eq!(
        repository::due_signal_receipts(&fixture.db, at_ms)
            .unwrap()
            .len(),
        32
    );
    assert_eq!(
        repository::get_instance(&fixture.db, &fixture.owner, &recipient.instance_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Waiting
    );
    let second_started = Instant::now();
    let second = messages::drain_pending(&fixture.db, at_ms + 1);
    let second_us = u64::try_from(second_started.elapsed().as_micros()).unwrap();
    second.completion.unwrap();
    assert_eq!(second.delivered, 32);
    let completed =
        repository::get_instance(&fixture.db, &fixture.owner, &recipient.instance_id, None)
            .unwrap();
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert_signal_subscription_pages(
        &fixture.db,
        &fixture.owner,
        &completed,
        64,
        ProcessSubscriptionStatus::Consumed,
    );
    let statuses: (i64, i64) = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*),SUM(status='delivered') FROM bpmn_signal_receipts WHERE signal_id=?1",
            [&signal_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(statuses, (64, 64));
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let replay = messages::drain_pending(&reopened, at_ms + 2);
    replay.completion.unwrap();
    assert_eq!(replay.delivered, 0);
    assert_eq!(
        repository::list_events(&reopened, &fixture.owner, &recipient.instance_id, 0, 200)
            .unwrap()
            .0
            .iter()
            .filter(|event| event.kind == "signal_received")
            .count(),
        64
    );
    eprintln!(
        "BPMN_SIGNAL_MEASUREMENT {}",
        json!({"case":"sixty_four_catches_drain32",
        "sample_count":2,"p50_us":percentile_us(&[first_us,second_us],50),
        "p95_us":percentile_us(&[first_us,second_us],95),"delivered":64,
        "replay_delivered":replay.delivered,"storage":signal_storage(&fixture)})
    );
}

#[test]
fn signal_measurement_sixteen_real_emissions_fill_one_thousand_twenty_four_sender_receipts() {
    let fixture = Fixture::new();
    let recipient_version = publish_model(&fixture, &parallel_signal_catches(64));
    let recipient = start_version(&fixture, &recipient_version);
    let source_version = publish_model(&fixture, &signal_throw_model());
    let mut sample_us = Vec::with_capacity(16);
    for _ in 0..16 {
        let started = Instant::now();
        let sender = start_version(&fixture, &source_version);
        sample_us.push(u64::try_from(started.elapsed().as_micros()).unwrap());
        assert_eq!(sender.status, ProcessInstanceStatus::Completed);
    }
    let (emissions, receipts, distinct_arms): (i64, i64, i64) = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(DISTINCT signal_id),COUNT(*),COUNT(DISTINCT recipient_subscription_id) \
            FROM bpmn_signal_receipts WHERE status='pending'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((emissions, receipts, distinct_arms), (16, 1024, 64));
    assert_eq!(
        repository::due_signal_receipts(&fixture.db, chrono::Utc::now().timestamp_millis() + 10)
            .unwrap()
            .len(),
        32
    );
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let reopened_pending: i64 = reopened
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM bpmn_signal_receipts WHERE status='pending'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reopened_pending, 1024);
    assert_eq!(
        repository::get_instance(&reopened, &fixture.owner, &recipient.instance_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Waiting
    );
    let denied_source = start_for_actor(&fixture, &fixture.owner, &source_version, true).unwrap();
    assert_eq!(denied_source.status, ProcessInstanceStatus::Incident);
    assert_eq!(denied_source.incidents.len(), 1);
    assert_eq!(denied_source.incidents[0].code, "SIGNAL_PENDING_LIMIT");
    let denied_rows: (i64, i64) = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT (SELECT COUNT(*) FROM bpmn_signal_emissions WHERE source_instance_id=?1),\
            (SELECT COUNT(*) FROM bpmn_signal_receipts WHERE status='pending')",
            [&denied_source.instance_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(denied_rows, (0, 1024));
    eprintln!(
        "BPMN_SIGNAL_MEASUREMENT {}",
        json!({"case":"sender_receipt_capacity_1024",
        "sample_count":sample_us.len(),"p50_us":percentile_us(&sample_us,50),
        "p95_us":percentile_us(&sample_us,95),"emissions":emissions,
        "pending_receipts":receipts,"distinct_arms":distinct_arms,
        "denied_1025th_receipt":true,
        "storage":signal_storage(&fixture)})
    );
}

#[test]
fn signal_measurement_sender_receipts_cross_1023_1024_1025_with_real_sources() {
    let fixture = Fixture::new();
    let wide_recipient_version = publish_model(&fixture, &parallel_signal_catches(64));
    let wide_recipient = start_version(&fixture, &wide_recipient_version);
    let wide_source_version = publish_model(&fixture, &signal_throw_model());
    let mut sample_us = Vec::with_capacity(79);
    for _ in 0..15 {
        let started = Instant::now();
        assert_eq!(
            start_version(&fixture, &wide_source_version).status,
            ProcessInstanceStatus::Completed
        );
        sample_us.push(u64::try_from(started.elapsed().as_micros()).unwrap());
    }
    let namespace_uri = "urn:orders:one-receipt";
    let single_recipient_version = publish_model(
        &fixture,
        &signal_namespace(signal_catch_model(), namespace_uri),
    );
    let single_recipient = start_version(&fixture, &single_recipient_version);
    let single_source_version = publish_model(
        &fixture,
        &signal_namespace(signal_throw_model(), namespace_uri),
    );
    for _ in 0..63 {
        let started = Instant::now();
        assert_eq!(
            start_version(&fixture, &single_source_version).status,
            ProcessInstanceStatus::Completed
        );
        sample_us.push(u64::try_from(started.elapsed().as_micros()).unwrap());
    }
    let at_1023: i64 = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM bpmn_signal_receipts WHERE status='pending'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(at_1023, 1023);
    let started = Instant::now();
    assert_eq!(
        start_version(&fixture, &single_source_version).status,
        ProcessInstanceStatus::Completed
    );
    sample_us.push(u64::try_from(started.elapsed().as_micros()).unwrap());
    let at_1024: i64 = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM bpmn_signal_receipts WHERE status='pending'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(at_1024, 1024);
    let denied = start_for_actor(&fixture, &fixture.owner, &single_source_version, true).unwrap();
    assert_eq!(denied.status, ProcessInstanceStatus::Incident);
    assert_eq!(denied.incidents.len(), 1);
    assert_eq!(denied.incidents[0].code, "SIGNAL_PENDING_LIMIT");
    let denied_emissions: i64 = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM bpmn_signal_emissions WHERE source_instance_id=?1",
            [&denied.instance_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(denied_emissions, 0);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let after_denial: i64 = reopened
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM bpmn_signal_receipts WHERE status='pending'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after_denial, 1024);
    let reopened_recipient =
        repository::get_instance(&reopened, &fixture.owner, &wide_recipient.instance_id, None)
            .unwrap();
    assert_signal_subscription_pages(
        &reopened,
        &fixture.owner,
        &reopened_recipient,
        64,
        ProcessSubscriptionStatus::Open,
    );
    assert_eq!(
        repository::get_instance(
            &reopened,
            &fixture.owner,
            &single_recipient.instance_id,
            None
        )
        .unwrap()
        .subscriptions[0]
            .status,
        ProcessSubscriptionStatus::Open
    );
    eprintln!(
        "BPMN_SIGNAL_MEASUREMENT {}",
        json!({"case":"sender_receipts_1023_1024_1025",
        "sample_count":sample_us.len(),"p50_us":percentile_us(&sample_us,50),
        "p95_us":percentile_us(&sample_us,95),"before_last":at_1023,
        "at_limit":at_1024,"denied_next_signal":true,"storage":signal_storage(&fixture)})
    );
}

#[test]
fn signal_measurement_four_authorized_senders_fill_four_thousand_ninety_six_org_receipts() {
    let fixture = Fixture::new();
    let actors = [
        fixture.owner.clone(),
        actor(&fixture.db, "signal-cap-1"),
        actor(&fixture.db, "signal-cap-2"),
        actor(&fixture.db, "signal-cap-3"),
    ];
    let mut sample_us = Vec::with_capacity(127);
    let mut recipients = Vec::with_capacity(4);
    for (index, sender) in actors.iter().enumerate() {
        let namespace_uri = format!("urn:orders:capacity:{index}");
        let recipient_model = signal_namespace(parallel_signal_catches(64), &namespace_uri);
        let recipient_version = publish_for_actor(&fixture, sender, &recipient_model);
        let recipient = start_for_actor(&fixture, sender, &recipient_version, false).unwrap();
        assert_eq!(recipient.status, ProcessInstanceStatus::Waiting);
        assert_signal_subscription_pages(
            &fixture.db,
            sender,
            &recipient,
            64,
            ProcessSubscriptionStatus::Open,
        );
        recipients.push((sender.clone(), recipient.instance_id));
        let source_model = signal_namespace(signal_throw_model(), &namespace_uri);
        let source_version = publish_for_actor(&fixture, sender, &source_model);
        for _ in 0..if index == 3 { 15 } else { 16 } {
            let started = Instant::now();
            let source = start_for_actor(&fixture, sender, &source_version, false).unwrap();
            sample_us.push(u64::try_from(started.elapsed().as_micros()).unwrap());
            assert_eq!(source.status, ProcessInstanceStatus::Completed);
        }
        if index == 3 {
            let singleton_namespace = "urn:orders:capacity:one-receipt";
            let singleton_recipient = publish_for_actor(
                &fixture,
                sender,
                &signal_namespace(signal_catch_model(), singleton_namespace),
            );
            let waiting = start_for_actor(&fixture, sender, &singleton_recipient, false).unwrap();
            assert_eq!(waiting.subscriptions.len(), 1);
            let singleton_source = publish_for_actor(
                &fixture,
                sender,
                &signal_namespace(signal_throw_model(), singleton_namespace),
            );
            for _ in 0..63 {
                let started = Instant::now();
                assert_eq!(
                    start_for_actor(&fixture, sender, &singleton_source, false)
                        .unwrap()
                        .status,
                    ProcessInstanceStatus::Completed
                );
                sample_us.push(u64::try_from(started.elapsed().as_micros()).unwrap());
            }
            let before_last: i64 = fixture
                .db
                .read()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM bpmn_signal_receipts WHERE status='pending'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(before_last, 4095);
            let started = Instant::now();
            assert_eq!(
                start_for_actor(&fixture, sender, &singleton_source, false)
                    .unwrap()
                    .status,
                ProcessInstanceStatus::Completed
            );
            sample_us.push(u64::try_from(started.elapsed().as_micros()).unwrap());
        }
    }
    let (pending, emissions, senders): (i64, i64, i64) = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*),COUNT(DISTINCT signal_id),COUNT(DISTINCT sender_user_id) \
            FROM bpmn_signal_receipts r JOIN bpmn_signal_emissions e USING(signal_id) \
            WHERE r.status='pending'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((pending, emissions, senders), (4096, 127, 4));
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    for (actor, instance_id) in &recipients {
        let instance = repository::get_instance(&reopened, actor, instance_id, None).unwrap();
        assert_eq!(instance.status, ProcessInstanceStatus::Waiting);
        assert_signal_subscription_pages(
            &reopened,
            actor,
            &instance,
            64,
            ProcessSubscriptionStatus::Open,
        );
    }
    let fifth = actor(&fixture.db, "signal-cap-fifth");
    let fifth_namespace = "urn:orders:capacity:fifth";
    let fifth_recipient_version = publish_for_actor(
        &fixture,
        &fifth,
        &signal_namespace(signal_catch_model(), fifth_namespace),
    );
    let fifth_recipient =
        start_for_actor(&fixture, &fifth, &fifth_recipient_version, false).unwrap();
    assert_eq!(fifth_recipient.subscriptions.len(), 1);
    let fifth_source_version = publish_for_actor(
        &fixture,
        &fifth,
        &signal_namespace(signal_throw_model(), fifth_namespace),
    );
    let denied_source = start_for_actor(&fixture, &fifth, &fifth_source_version, true).unwrap();
    assert_eq!(denied_source.status, ProcessInstanceStatus::Incident);
    assert_eq!(denied_source.incidents.len(), 1);
    assert_eq!(denied_source.incidents[0].code, "SIGNAL_PENDING_LIMIT");
    assert_eq!(denied_source.active_node_ids, vec!["Throw_1".to_string()]);
    let after_receipts: i64 = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM bpmn_signal_receipts WHERE status='pending'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after_receipts, 4096);
    let denied_emissions: i64 = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM bpmn_signal_emissions WHERE source_instance_id=?1",
            [&denied_source.instance_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(denied_emissions, 0);
    assert_eq!(
        repository::get_instance(&fixture.db, &fifth, &fifth_recipient.instance_id, None)
            .unwrap()
            .subscriptions[0]
            .status,
        ProcessSubscriptionStatus::Open
    );
    eprintln!(
        "BPMN_SIGNAL_MEASUREMENT {}",
        json!({"case":"four_sender_org_receipts_4096",
        "sample_count":sample_us.len(),"p50_us":percentile_us(&sample_us,50),
        "p95_us":percentile_us(&sample_us,95),"pending_receipts":pending,
        "emissions":emissions,"senders":senders,"denied_4097th_receipt":true,
        "storage":signal_storage(&fixture)})
    );
}

#[test]
fn signal_measurement_four_thousand_ninety_six_open_arms_reject_the_next_real_start() {
    let fixture = Fixture::new();
    let version = publish_model(&fixture, &parallel_signal_catches(64));
    let single_version = publish_model(&fixture, &signal_catch_model());
    let mut sample_us = Vec::with_capacity(64);
    let mut instance_ids = Vec::with_capacity(64);
    for _ in 0..64 {
        let started = Instant::now();
        let instance = start_version(&fixture, &version);
        sample_us.push(u64::try_from(started.elapsed().as_micros()).unwrap());
        assert_eq!(instance.status, ProcessInstanceStatus::Waiting);
        assert_eq!(instance.pages.as_ref().unwrap().subscriptions.total, 64);
        instance_ids.push(instance.instance_id);
    }
    let open: i64 = fixture.db.read().unwrap().query_row(
        "SELECT COUNT(*) FROM bpmn_event_subscriptions WHERE kind='signal_catch' AND status='open'",
        [], |row| row.get(0)).unwrap();
    assert_eq!(open, 4096);
    let before_denial = super::signal_proof_tests::all_transition_rows(&fixture);
    let denied_instance_id = Uuid::new_v4().to_string();
    let denied_command = stamp("start one catch beyond the organization arm limit");
    let denied_variables = serde_json::to_value(&single_version.model.variables).unwrap();
    let denied = repository::start_instance(
        &fixture.db,
        &fixture.owner,
        &denied_command,
        &denied_instance_id,
        &single_version.definition_id,
        single_version.version,
        &denied_variables,
        repository::ProcessPlanInput::Canonical,
        chrono::Utc::now().timestamp_millis(),
    )
    .unwrap_err();
    assert!(
        denied
            .to_string()
            .contains("organization signal catch capacity exceeded"),
        "{denied:#}"
    );
    assert_eq!(
        super::signal_proof_tests::all_transition_rows(&fixture),
        before_denial
    );
    let denied_rows: (i64, i64, i64) = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT (SELECT COUNT(*) FROM bpmn_instances WHERE instance_id=?1), \
            (SELECT COUNT(*) FROM bpmn_event_subscriptions WHERE instance_id=?1), \
            (SELECT COUNT(*) FROM bpmn_commands WHERE command_id=?2)",
            rusqlite::params![denied_instance_id, denied_command.command_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(denied_rows, (0, 0, 0));
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted_open: i64 = reopened.read().unwrap().query_row(
        "SELECT COUNT(*) FROM bpmn_event_subscriptions WHERE kind='signal_catch' AND status='open'",
        [], |row| row.get(0)).unwrap();
    assert_eq!(persisted_open, 4096);
    for instance_id in [&instance_ids[0], &instance_ids[63]] {
        let instance =
            repository::get_instance(&reopened, &fixture.owner, instance_id, None).unwrap();
        assert_eq!(instance.status, ProcessInstanceStatus::Waiting);
        assert_signal_subscription_pages(
            &reopened,
            &fixture.owner,
            &instance,
            64,
            ProcessSubscriptionStatus::Open,
        );
    }
    eprintln!(
        "BPMN_SIGNAL_MEASUREMENT {}",
        json!({"case":"four_thousand_ninety_six_open_arms",
        "sample_count":sample_us.len(),"p50_us":percentile_us(&sample_us,50),
        "p95_us":percentile_us(&sample_us,95),"open_arms":open,
        "denied_4097th_open_arm":true,
        "reopened_instances_checked":2,"storage":signal_storage(&fixture)})
    );
}

#[test]
fn signal_sixty_five_real_eligible_catches_park_the_canonical_source_without_an_emission() {
    let fixture = Fixture::new();
    let version = publish_model(&fixture, &parallel_signal_catches(65));
    let recipient = start_version(&fixture, &version);
    assert_eq!(recipient.status, ProcessInstanceStatus::Waiting);
    assert_signal_subscription_pages(
        &fixture.db,
        &fixture.owner,
        &recipient,
        65,
        ProcessSubscriptionStatus::Open,
    );
    let (open, distinct): (i64, i64) = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*),COUNT(DISTINCT subscription_id) FROM bpmn_event_subscriptions \
         WHERE instance_id=?1 AND kind='signal_catch' AND status='open'",
            [&recipient.instance_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((open, distinct), (65, 65));
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted =
        repository::get_instance(&reopened, &fixture.owner, &recipient.instance_id, None).unwrap();
    assert_signal_subscription_pages(
        &reopened,
        &fixture.owner,
        &persisted,
        65,
        ProcessSubscriptionStatus::Open,
    );
    let source_version = publish_model(&fixture, &signal_throw_model());
    let source = start_for_actor(&fixture, &fixture.owner, &source_version, true).unwrap();
    assert_eq!(source.status, ProcessInstanceStatus::Incident);
    assert_eq!(source.incidents.len(), 1);
    assert_eq!(source.incidents[0].code, "SIGNAL_RECIPIENT_LIMIT");
    assert_eq!(source.incidents[0].node_id.as_deref(), Some("Throw_1"));
    assert!(!source.incidents[0].can_retry);
    assert_eq!(source.active_node_ids, vec!["Throw_1".to_string()]);
    let (emissions, receipts): (i64, i64) = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT (SELECT COUNT(*) FROM bpmn_signal_emissions WHERE source_instance_id=?1),\
            (SELECT COUNT(*) FROM bpmn_signal_receipts)",
            [&source.instance_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((emissions, receipts), (0, 0));
    let events = repository::list_events(&fixture.db, &fixture.owner, &source.instance_id, 0, 200)
        .unwrap()
        .0;
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "signal_admitted")
            .count(),
        0
    );
    assert_eq!(
        events
            .iter()
            .filter(
                |event| event.kind == "incident" && event.data["code"] == "SIGNAL_RECIPIENT_LIMIT"
            )
            .count(),
        1
    );
    let source_reopened =
        repository::get_instance(&reopened, &fixture.owner, &source.instance_id, None).unwrap();
    assert_eq!(source_reopened.status, ProcessInstanceStatus::Incident);
    assert_eq!(source_reopened.incidents[0].code, "SIGNAL_RECIPIENT_LIMIT");
}

fn measure_shared_pending_count(signal_first: bool, deny_signal: bool) {
    use super::messages::test_support::{catch_target, envelope, receiving_model};
    use super::runtime::test_support::stamp;
    let fixture = Fixture::new();
    let message_version = publish_model(&fixture, &receiving_model(false, false));
    let recipient_version = publish_model(&fixture, &signal_catch_model());
    let recipient = start_version(&fixture, &recipient_version);
    let source_version = publish_model(&fixture, &signal_throw_model());
    let at_ms = chrono::Utc::now().timestamp_millis();
    let mut sender = None;
    if signal_first {
        sender = Some(start_version(&fixture, &source_version));
    }
    let message_total = if deny_signal { 1024 } else { 1023 };
    let mut sample_us = Vec::with_capacity(message_total);
    for index in 0..message_total {
        if signal_first && index == message_total - 1 {
            let pending: i64 = fixture
                .db
                .read()
                .unwrap()
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM bpmn_messages WHERE status='pending')\
                    +(SELECT COUNT(*) FROM bpmn_signal_emissions e WHERE EXISTS\
                        (SELECT 1 FROM bpmn_signal_receipts r WHERE r.signal_id=e.signal_id \
                            AND r.status IN ('pending','claimed')))",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(pending, 1023);
        }
        let message = envelope(
            catch_target(&message_version, None, None),
            json!({"index":index}),
        );
        let started = Instant::now();
        repository::send_message(
            &fixture.db,
            &fixture.owner,
            &stamp("admit a real pending message"),
            &message,
            at_ms,
        )
        .unwrap();
        sample_us.push(u64::try_from(started.elapsed().as_micros()).unwrap());
    }
    if deny_signal {
        assert!(!signal_first);
        let source = start_for_actor(&fixture, &fixture.owner, &source_version, true).unwrap();
        assert_eq!(source.status, ProcessInstanceStatus::Incident);
        assert_eq!(source.incidents.len(), 1);
        assert_eq!(source.incidents[0].code, "SIGNAL_PENDING_LIMIT");
        let (messages, emissions, receipts): (i64, i64, i64) = fixture
            .db
            .read()
            .unwrap()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM bpmn_messages WHERE status='pending'),\
                (SELECT COUNT(*) FROM bpmn_signal_emissions WHERE source_instance_id=?1),\
                (SELECT COUNT(*) FROM bpmn_signal_receipts)",
                [&source.instance_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!((messages, emissions, receipts), (1024, 0, 0));
        assert_eq!(
            repository::get_instance(&fixture.db, &fixture.owner, &recipient.instance_id, None)
                .unwrap()
                .subscriptions[0]
                .status,
            ProcessSubscriptionStatus::Open
        );
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        assert_eq!(
            repository::get_instance(&reopened, &fixture.owner, &source.instance_id, None)
                .unwrap()
                .incidents[0]
                .code,
            "SIGNAL_PENDING_LIMIT"
        );
        eprintln!(
            "BPMN_SIGNAL_MEASUREMENT {}",
            json!({"case":"shared_pending_count_message_first_signal_denied",
            "sample_count":sample_us.len(),"p50_us":percentile_us(&sample_us,50),
            "p95_us":percentile_us(&sample_us,95),"messages":messages,
            "emissions":emissions,"receipts":receipts,"storage":signal_storage(&fixture)})
        );
        return;
    }
    if !signal_first {
        let pending: i64 = fixture
            .db
            .read()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM bpmn_messages WHERE status='pending'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pending, 1023);
    }
    let sender = sender.unwrap_or_else(|| start_version(&fixture, &source_version));
    assert_eq!(sender.status, ProcessInstanceStatus::Completed);
    assert_eq!(
        repository::get_instance(&fixture.db, &fixture.owner, &recipient.instance_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Waiting
    );
    let (messages, signals, receipts): (i64, i64, i64) = fixture.db.read().unwrap().query_row(
        "SELECT (SELECT COUNT(*) FROM bpmn_messages WHERE status IN ('pending','blocked','ambiguous')),\
         (SELECT COUNT(*) FROM bpmn_signal_emissions WHERE status='pending'),\
         (SELECT COUNT(*) FROM bpmn_signal_receipts WHERE status IN ('pending','claimed'))",
        [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
    assert_eq!((messages, signals, receipts), (1023, 1, 1));
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let denied_message = envelope(catch_target(&message_version, None, None), json!("over"));
    let denied = repository::send_message(
        &fixture.db,
        &fixture.owner,
        &stamp("reject the next real pending message"),
        &denied_message,
        at_ms,
    )
    .unwrap_err();
    assert!(
        denied
            .to_string()
            .contains("pending message capacity exceeded"),
        "{denied:#}"
    );
    assert_eq!(
        super::signal_proof_tests::all_transition_rows(&fixture),
        before
    );
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert_eq!(
        repository::get_instance(&reopened, &fixture.owner, &recipient.instance_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Waiting
    );
    eprintln!(
        "BPMN_SIGNAL_MEASUREMENT {}",
        json!({"case":if signal_first {
        "shared_pending_count_signal_first" } else { "shared_pending_count_message_first" },
        "sample_count":sample_us.len(),"p50_us":percentile_us(&sample_us,50),
        "p95_us":percentile_us(&sample_us,95),"messages":messages,"signals":signals,
        "receipts":receipts,"denied_next_message":true,"storage":signal_storage(&fixture)})
    );
}

#[test]
fn signal_measurement_shared_pending_count_message_first_rejects_the_next_real_message() {
    measure_shared_pending_count(false, false);
}

#[test]
fn signal_measurement_shared_pending_count_signal_first_rejects_the_next_real_message() {
    measure_shared_pending_count(true, false);
}

#[test]
fn signal_measurement_shared_pending_count_message_first_parks_the_canonical_signal() {
    measure_shared_pending_count(false, true);
}

fn measure_org_pending_count(signal_first: bool) {
    use super::messages::test_support::{catch_target, envelope, receiving_model};
    let fixture = Fixture::new();
    let actors = [
        fixture.owner.clone(),
        actor(&fixture.db, "signal-org-count-1"),
        actor(&fixture.db, "signal-org-count-2"),
        actor(&fixture.db, "signal-org-count-3"),
    ];
    let mut sample_us = Vec::with_capacity(4096);
    let mut early_signal_recipient = None;
    if signal_first {
        let recipient_version = publish_for_actor(&fixture, &actors[0], &signal_catch_model());
        let recipient = start_for_actor(&fixture, &actors[0], &recipient_version, false).unwrap();
        let source_version = publish_for_actor(&fixture, &actors[0], &signal_throw_model());
        assert_eq!(
            start_for_actor(&fixture, &actors[0], &source_version, false)
                .unwrap()
                .status,
            ProcessInstanceStatus::Completed
        );
        early_signal_recipient = Some(recipient.instance_id);
    }
    let at_ms = chrono::Utc::now().timestamp_millis();
    for (sender_index, sender) in actors.iter().enumerate() {
        let version = publish_for_actor(&fixture, sender, &receiving_model(false, false));
        let total = if signal_first && sender_index == 0 {
            1023
        } else {
            1024
        };
        for ordinal in 0..total {
            if sender_index == 3 && ordinal == total - 1 {
                let count: i64 = fixture
                    .db
                    .read()
                    .unwrap()
                    .query_row(
                        "SELECT (SELECT COUNT(*) FROM bpmn_messages WHERE status='pending')\
                        +(SELECT COUNT(*) FROM bpmn_signal_emissions e WHERE EXISTS\
                            (SELECT 1 FROM bpmn_signal_receipts r WHERE r.signal_id=e.signal_id \
                                AND r.status IN ('pending','claimed')))",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(count, 4095);
            }
            let message = envelope(
                catch_target(&version, None, None),
                json!({"sender":sender_index,"ordinal":ordinal}),
            );
            let started = Instant::now();
            repository::send_message(
                &fixture.db,
                sender,
                &stamp("admit real org pending message"),
                &message,
                at_ms,
            )
            .unwrap();
            sample_us.push(u64::try_from(started.elapsed().as_micros()).unwrap());
        }
    }
    let (messages, signals): (i64, i64) = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT (SELECT COUNT(*) FROM bpmn_messages WHERE status='pending'),\
            (SELECT COUNT(*) FROM bpmn_signal_emissions e WHERE EXISTS\
                (SELECT 1 FROM bpmn_signal_receipts r WHERE r.signal_id=e.signal_id \
                    AND r.status IN ('pending','claimed')))",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(messages + signals, 4096);
    let mut denied_source_id = None;
    if signal_first {
        assert_eq!((messages, signals), (4095, 1));
        let fifth = actor(&fixture.db, "signal-org-count-fifth");
        let fifth_version = publish_for_actor(&fixture, &fifth, &receiving_model(false, false));
        let denied_message = envelope(catch_target(&fifth_version, None, None), json!("over"));
        let before = super::signal_proof_tests::all_transition_rows(&fixture);
        let denial = repository::send_message(
            &fixture.db,
            &fifth,
            &stamp("reject actual 4097th shared entry"),
            &denied_message,
            at_ms,
        )
        .unwrap_err();
        assert!(
            denial
                .to_string()
                .contains("pending message capacity exceeded"),
            "{denial:#}"
        );
        assert_eq!(
            super::signal_proof_tests::all_transition_rows(&fixture),
            before
        );
        assert_eq!(
            repository::get_instance(
                &fixture.db,
                &actors[0],
                early_signal_recipient.as_deref().unwrap(),
                None
            )
            .unwrap()
            .status,
            ProcessInstanceStatus::Waiting
        );
    } else {
        assert_eq!((messages, signals), (4096, 0));
        let fifth = actor(&fixture.db, "signal-org-count-fifth");
        let recipient_version = publish_for_actor(&fixture, &fifth, &signal_catch_model());
        let recipient = start_for_actor(&fixture, &fifth, &recipient_version, false).unwrap();
        let source_version = publish_for_actor(&fixture, &fifth, &signal_throw_model());
        let source = start_for_actor(&fixture, &fifth, &source_version, true).unwrap();
        denied_source_id = Some(source.instance_id.clone());
        assert_eq!(source.status, ProcessInstanceStatus::Incident);
        assert_eq!(source.incidents.len(), 1);
        assert_eq!(source.incidents[0].code, "SIGNAL_PENDING_LIMIT");
        let emissions: i64 = fixture
            .db
            .read()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM bpmn_signal_emissions WHERE source_instance_id=?1",
                [&source.instance_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(emissions, 0);
        assert_eq!(
            repository::get_instance(&fixture.db, &fifth, &recipient.instance_id, None)
                .unwrap()
                .subscriptions[0]
                .status,
            ProcessSubscriptionStatus::Open
        );
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let reopened_messages: i64 = reopened
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM bpmn_messages WHERE status='pending'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reopened_messages, messages);
    if let Some(source_id) = denied_source_id {
        let persisted: i64 = reopened.read().unwrap().query_row(
            "SELECT COUNT(*) FROM bpmn_incidents WHERE instance_id=?1 AND code='SIGNAL_PENDING_LIMIT'",
            [source_id], |row| row.get(0)).unwrap();
        assert_eq!(persisted, 1);
    }
    eprintln!(
        "BPMN_SIGNAL_MEASUREMENT {}",
        json!({"case":if signal_first {
        "org_shared_count_signal_first" } else { "org_shared_count_message_first" },
        "sample_count":sample_us.len(),"p50_us":percentile_us(&sample_us,50),
        "p95_us":percentile_us(&sample_us,95),"pending_messages":messages,
        "pending_signals":signals,"org_pending_total":messages + signals,
        "denied_4097th_entry":true,"storage":signal_storage(&fixture)})
    );
}

#[test]
fn signal_measurement_org_pending_count_message_first_parks_the_canonical_signal() {
    measure_org_pending_count(false);
}

#[test]
fn signal_measurement_org_pending_count_signal_first_rejects_the_next_real_message() {
    measure_org_pending_count(true);
}

#[test]
fn signal_measurement_shared_pending_bytes_signal_first_rejects_the_next_real_message() {
    use super::messages::test_support::{catch_target, envelope, receiving_model};
    use super::runtime::test_support::stamp;
    let fixture = Fixture::new();
    let message_version = publish_model(&fixture, &receiving_model(false, false));
    let recipient_version = publish_model(&fixture, &signal_catch_model());
    let recipient = start_version(&fixture, &recipient_version);
    let source_version = publish_model(&fixture, &signal_throw_model());
    let sender = start_version(&fixture, &source_version);
    assert_eq!(sender.status, ProcessInstanceStatus::Completed);
    let admitted_bytes: i64 = fixture.db.read().unwrap().query_row(
        "SELECT payload_bytes FROM bpmn_signal_emissions WHERE source_instance_id=?1 AND status='pending'",
        [&sender.instance_id], |row| row.get(0)).unwrap();
    let byte_limit = 64 * 1024 * 1024_i64;
    let per_message = 256 * 1024_i64;
    let remaining = byte_limit - admitted_bytes;
    assert!(remaining > per_message && remaining % per_message >= 2);
    let full_messages = remaining / per_message;
    let partial_bytes = remaining % per_message;
    let at_ms = chrono::Utc::now().timestamp_millis();
    let mut sample_us = Vec::with_capacity(usize::try_from(full_messages + 1).unwrap());
    for _ in 0..full_messages {
        let message = envelope(
            catch_target(&message_version, None, None),
            json!("x".repeat(usize::try_from(per_message - 2).unwrap())),
        );
        let started = Instant::now();
        repository::send_message(
            &fixture.db,
            &fixture.owner,
            &stamp("fill actual sender payload bytes"),
            &message,
            at_ms,
        )
        .unwrap();
        sample_us.push(u64::try_from(started.elapsed().as_micros()).unwrap());
    }
    let before_partial: i64 = fixture.db.read().unwrap().query_row(
        "SELECT (SELECT COALESCE(SUM(payload_bytes),0) FROM bpmn_messages WHERE status='pending')\
            +(SELECT COALESCE(SUM(e.payload_bytes),0) FROM bpmn_signal_emissions e \
                WHERE EXISTS (SELECT 1 FROM bpmn_signal_receipts r WHERE r.signal_id=e.signal_id \
                    AND r.status IN ('pending','claimed')))",
        [], |row| row.get(0)).unwrap();
    assert_eq!(before_partial, byte_limit - partial_bytes);
    let partial = envelope(
        catch_target(&message_version, None, None),
        json!("x".repeat(usize::try_from(partial_bytes - 2).unwrap())),
    );
    let started = Instant::now();
    repository::send_message(
        &fixture.db,
        &fixture.owner,
        &stamp("fill exact final sender payload bytes"),
        &partial,
        at_ms,
    )
    .unwrap();
    sample_us.push(u64::try_from(started.elapsed().as_micros()).unwrap());
    let (count, actual_bytes): (i64, i64) = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*),COALESCE(SUM(payload_bytes),0) FROM (\
         SELECT payload_bytes FROM bpmn_messages WHERE status IN ('pending','blocked','ambiguous')\
         UNION ALL SELECT e.payload_bytes FROM bpmn_signal_emissions e \
         WHERE EXISTS (SELECT 1 FROM bpmn_signal_receipts r WHERE r.signal_id=e.signal_id \
             AND r.status IN ('pending','claimed')))",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(actual_bytes, byte_limit);
    assert!(
        count < 1024,
        "the independent byte cap must precede the count cap"
    );
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let denied_message = envelope(catch_target(&message_version, None, None), json!("x"));
    let denied = repository::send_message(
        &fixture.db,
        &fixture.owner,
        &stamp("reject one extra actual payload byte"),
        &denied_message,
        at_ms,
    )
    .unwrap_err();
    assert!(
        denied
            .to_string()
            .contains("pending message capacity exceeded"),
        "{denied:#}"
    );
    assert_eq!(
        super::signal_proof_tests::all_transition_rows(&fixture),
        before
    );
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert_eq!(
        repository::get_instance(&reopened, &fixture.owner, &recipient.instance_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Waiting
    );
    eprintln!(
        "BPMN_SIGNAL_MEASUREMENT {}",
        json!({"case":"shared_pending_bytes_signal_first",
        "sample_count":sample_us.len(),"p50_us":percentile_us(&sample_us,50),
        "p95_us":percentile_us(&sample_us,95),"pending_count":count,
        "pending_bytes":actual_bytes,"limit_bytes":byte_limit,
        "denied_next_message":true,"storage":signal_storage(&fixture)})
    );
}

#[test]
fn signal_measurement_shared_pending_bytes_message_first_park_the_canonical_signal() {
    use super::messages::test_support::{catch_target, envelope, receiving_model};
    let fixture = Fixture::new();
    let message_version = publish_model(&fixture, &receiving_model(false, false));
    let recipient_version = publish_model(&fixture, &signal_catch_model());
    let recipient = start_version(&fixture, &recipient_version);
    let source_version = publish_model(&fixture, &signal_throw_model());
    let payload = json!("x".repeat(256 * 1024 - 2));
    let at_ms = chrono::Utc::now().timestamp_millis();
    let mut sample_us = Vec::with_capacity(256);
    for ordinal in 0..256 {
        if ordinal == 255 {
            let before_last: i64 = fixture
                .db
                .read()
                .unwrap()
                .query_row(
                    "SELECT SUM(payload_bytes) FROM bpmn_messages WHERE status='pending'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(before_last, 64 * 1024 * 1024 - 256 * 1024);
        }
        let message = envelope(catch_target(&message_version, None, None), payload.clone());
        let started = Instant::now();
        repository::send_message(
            &fixture.db,
            &fixture.owner,
            &stamp("fill sender byte budget before Signal"),
            &message,
            at_ms,
        )
        .unwrap();
        sample_us.push(u64::try_from(started.elapsed().as_micros()).unwrap());
    }
    let (messages, bytes): (i64, i64) = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*),SUM(payload_bytes) FROM bpmn_messages WHERE status='pending'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((messages, bytes), (256, 64 * 1024 * 1024));
    let source = start_for_actor(&fixture, &fixture.owner, &source_version, true).unwrap();
    assert_eq!(source.status, ProcessInstanceStatus::Incident);
    assert_eq!(source.incidents.len(), 1);
    assert_eq!(source.incidents[0].code, "SIGNAL_PENDING_LIMIT");
    assert_eq!(source.active_node_ids, vec!["Throw_1".to_string()]);
    let emissions: i64 = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM bpmn_signal_emissions WHERE source_instance_id=?1",
            [&source.instance_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(emissions, 0);
    assert_eq!(
        repository::get_instance(&fixture.db, &fixture.owner, &recipient.instance_id, None)
            .unwrap()
            .subscriptions[0]
            .status,
        ProcessSubscriptionStatus::Open
    );
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert_eq!(
        repository::get_instance(&reopened, &fixture.owner, &source.instance_id, None)
            .unwrap()
            .incidents[0]
            .code,
        "SIGNAL_PENDING_LIMIT"
    );
    eprintln!(
        "BPMN_SIGNAL_MEASUREMENT {}",
        json!({"case":"shared_pending_bytes_message_first_signal_denied",
        "sample_count":sample_us.len(),"p50_us":percentile_us(&sample_us,50),
        "p95_us":percentile_us(&sample_us,95),"messages":messages,"pending_bytes":bytes,
        "emissions":emissions,"storage":signal_storage(&fixture)})
    );
}

fn measure_org_pending_bytes(signal_first: bool) {
    use super::messages::test_support::{catch_target, envelope, receiving_model};
    let fixture = Fixture::new();
    let actors = [
        fixture.owner.clone(),
        actor(&fixture.db, "signal-byte-1"),
        actor(&fixture.db, "signal-byte-2"),
        actor(&fixture.db, "signal-byte-3"),
    ];
    let mut early_signal_recipient = None;
    let signal_bytes: i64 = if signal_first {
        let signal_recipient_version =
            publish_for_actor(&fixture, &actors[0], &signal_catch_model());
        let recipient =
            start_for_actor(&fixture, &actors[0], &signal_recipient_version, false).unwrap();
        let signal_source_version = publish_for_actor(&fixture, &actors[0], &signal_throw_model());
        assert_eq!(
            start_for_actor(&fixture, &actors[0], &signal_source_version, false)
                .unwrap()
                .status,
            ProcessInstanceStatus::Completed
        );
        early_signal_recipient = Some(recipient.instance_id);
        fixture
            .db
            .read()
            .unwrap()
            .query_row(
                "SELECT payload_bytes FROM bpmn_signal_emissions WHERE status='pending'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    } else {
        0
    };
    let actor_limit = 64 * 1024 * 1024_i64;
    let per_message = 256 * 1024_i64;
    let full_payload = json!("x".repeat(usize::try_from(per_message - 2).unwrap()));
    let at_ms = chrono::Utc::now().timestamp_millis();
    let mut sample_us = Vec::with_capacity(1024);
    for (index, sender) in actors.iter().enumerate() {
        let version = publish_for_actor(&fixture, sender, &receiving_model(false, false));
        let budget = if index == 0 {
            actor_limit - signal_bytes
        } else {
            actor_limit
        };
        let full_count = budget / per_message;
        for ordinal in 0..full_count {
            if index == 3 && ordinal == full_count - 1 {
                let before_last: i64 = fixture.db.read().unwrap().query_row(
                    "SELECT (SELECT COALESCE(SUM(payload_bytes),0) FROM bpmn_messages WHERE status='pending')\
                        +(SELECT COALESCE(SUM(e.payload_bytes),0) FROM bpmn_signal_emissions e \
                            WHERE EXISTS (SELECT 1 FROM bpmn_signal_receipts r \
                                WHERE r.signal_id=e.signal_id AND r.status IN ('pending','claimed')))",
                    [], |row| row.get(0)).unwrap();
                assert_eq!(before_last, 256 * 1024 * 1024 - per_message);
            }
            let message = envelope(catch_target(&version, None, None), full_payload.clone());
            let started = Instant::now();
            repository::send_message(
                &fixture.db,
                sender,
                &stamp("admit actual shared org bytes"),
                &message,
                at_ms,
            )
            .unwrap();
            sample_us.push(u64::try_from(started.elapsed().as_micros()).unwrap());
        }
        let remainder = budget % per_message;
        if remainder > 0 {
            assert!(remainder >= 2);
            let message = envelope(
                catch_target(&version, None, None),
                json!("x".repeat(usize::try_from(remainder - 2).unwrap())),
            );
            let started = Instant::now();
            repository::send_message(
                &fixture.db,
                sender,
                &stamp("fill actual sender byte remainder"),
                &message,
                at_ms,
            )
            .unwrap();
            sample_us.push(u64::try_from(started.elapsed().as_micros()).unwrap());
        }
    }
    let (message_count, message_bytes, pending_signal_bytes): (i64, i64, i64) = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT (SELECT COUNT(*) FROM bpmn_messages WHERE status='pending'),\
            (SELECT COALESCE(SUM(payload_bytes),0) FROM bpmn_messages WHERE status='pending'),\
            (SELECT COALESCE(SUM(e.payload_bytes),0) FROM bpmn_signal_emissions e \
                WHERE EXISTS (SELECT 1 FROM bpmn_signal_receipts r WHERE r.signal_id=e.signal_id \
                    AND r.status IN ('pending','claimed')))",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(message_count, 1024);
    assert_eq!(message_bytes + pending_signal_bytes, 256 * 1024 * 1024);
    assert_eq!(pending_signal_bytes, signal_bytes);
    let outsider = actor(&fixture.db, "signal-byte-over");
    let mut denied_source_id = None;
    if signal_first {
        let outsider_version =
            publish_for_actor(&fixture, &outsider, &receiving_model(false, false));
        let denied = envelope(catch_target(&outsider_version, None, None), json!("x"));
        let denied_result = repository::send_message(
            &fixture.db,
            &outsider,
            &stamp("deny actual org byte overrun"),
            &denied,
            at_ms,
        )
        .unwrap_err();
        assert!(
            denied_result
                .to_string()
                .contains("pending message capacity exceeded"),
            "{denied_result:#}"
        );
        let denied_rows: i64 = fixture
            .db
            .read()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM bpmn_messages WHERE sender_user_id=?1 AND message_id=?2",
                [&outsider.user_id, &denied.message_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(denied_rows, 0);
    } else {
        let recipient_version = publish_for_actor(&fixture, &outsider, &signal_catch_model());
        let recipient = start_for_actor(&fixture, &outsider, &recipient_version, false).unwrap();
        let source_version = publish_for_actor(&fixture, &outsider, &signal_throw_model());
        let denied = start_for_actor(&fixture, &outsider, &source_version, true).unwrap();
        denied_source_id = Some(denied.instance_id.clone());
        assert_eq!(denied.status, ProcessInstanceStatus::Incident);
        assert_eq!(denied.incidents.len(), 1);
        assert_eq!(denied.incidents[0].code, "SIGNAL_PENDING_LIMIT");
        let denied_emissions: i64 = fixture
            .db
            .read()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM bpmn_signal_emissions WHERE source_instance_id=?1",
                [&denied.instance_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(denied_emissions, 0);
        assert_eq!(
            repository::get_instance(&fixture.db, &outsider, &recipient.instance_id, None)
                .unwrap()
                .subscriptions[0]
                .status,
            ProcessSubscriptionStatus::Open
        );
    }
    let (after_count, after_bytes): (i64, i64) = fixture.db.read().unwrap().query_row(
        "SELECT COUNT(*),COALESCE(SUM(payload_bytes),0) FROM bpmn_messages WHERE status='pending'",
        [], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
    assert_eq!((after_count, after_bytes), (message_count, message_bytes));
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    if let Some(recipient_id) = early_signal_recipient {
        assert_eq!(
            repository::get_instance(&reopened, &actors[0], &recipient_id, None)
                .unwrap()
                .status,
            ProcessInstanceStatus::Waiting
        );
    }
    if let Some(source_id) = denied_source_id {
        let persisted: i64 = reopened.read().unwrap().query_row(
            "SELECT COUNT(*) FROM bpmn_incidents WHERE instance_id=?1 AND code='SIGNAL_PENDING_LIMIT'",
            [source_id], |row| row.get(0)).unwrap();
        assert_eq!(persisted, 1);
    }
    eprintln!(
        "BPMN_SIGNAL_MEASUREMENT {}",
        json!({"case":if signal_first {
        "org_bytes_256_mib_signal_first" } else { "org_bytes_256_mib_message_first" },
        "sample_count":sample_us.len(),"p50_us":percentile_us(&sample_us,50),
        "p95_us":percentile_us(&sample_us,95),"pending_messages":message_count,
        "pending_message_bytes":message_bytes,"pending_signal_bytes":pending_signal_bytes,
        "org_limit_bytes":256 * 1024 * 1024_i64,"denied_next_entry":true,
        "storage":signal_storage(&fixture)})
    );
}

#[test]
fn signal_measurement_org_bytes_signal_first_rejects_the_next_real_message() {
    measure_org_pending_bytes(true);
}

#[test]
fn signal_measurement_org_bytes_message_first_parks_the_canonical_signal() {
    measure_org_pending_bytes(false);
}

#[test]
fn signal_measurement_thirty_three_settled_payloads_prune_in_bounded_batches_after_seven_days() {
    let fixture = Fixture::new();
    let version = publish_model(&fixture, &signal_throw_model());
    let mut senders = Vec::with_capacity(33);
    for _ in 0..33 {
        let sender = start_version(&fixture, &version);
        assert_eq!(sender.status, ProcessInstanceStatus::Completed);
        senders.push(sender.instance_id);
    }
    let (earliest, latest, unpruned): (i64, i64, i64) = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT MIN(settled_at_ms),MAX(settled_at_ms),COUNT(*) \
            FROM bpmn_signal_emissions WHERE status='settled' AND payload_json IS NOT NULL",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(unpruned, 33);
    let retention_ms = 7 * 24 * 60 * 60 * 1000_i64;
    assert_eq!(
        repository::prune_signal_payloads(&fixture.db, earliest + retention_ms - 1).unwrap(),
        0
    );
    let started = Instant::now();
    assert_eq!(
        repository::prune_signal_payloads(&fixture.db, latest + retention_ms).unwrap(),
        32
    );
    let first_us = u64::try_from(started.elapsed().as_micros()).unwrap();
    let started = Instant::now();
    assert_eq!(
        repository::prune_signal_payloads(&fixture.db, latest + retention_ms + 1).unwrap(),
        1
    );
    let second_us = u64::try_from(started.elapsed().as_micros()).unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let (pruned, hashes, min_lag, max_lag): (i64, i64, i64, i64) = reopened.read().unwrap()
        .query_row("SELECT COUNT(*),COUNT(DISTINCT payload_sha256), \
            MIN(payload_pruned_at_ms-settled_at_ms-?1), \
            MAX(payload_pruned_at_ms-settled_at_ms-?1) \
            FROM bpmn_signal_emissions WHERE payload_json IS NULL AND payload_pruned_at_ms IS NOT NULL",
            [retention_ms], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
    assert_eq!(pruned, 33);
    assert_eq!(
        hashes, 1,
        "the immutable identical payload hash survives pruning"
    );
    assert!(min_lag >= 0);
    assert!(max_lag >= min_lag);
    assert_eq!(
        repository::prune_signal_payloads(&reopened, latest + retention_ms + 2).unwrap(),
        0
    );
    let replay = messages::drain_pending(&reopened, latest + retention_ms + 3);
    replay.completion.unwrap();
    assert_eq!(replay.delivered, 0);
    for sender_id in [&senders[0], &senders[32]] {
        assert_eq!(
            repository::list_events(&reopened, &fixture.owner, sender_id, 0, 200)
                .unwrap()
                .0
                .iter()
                .filter(|event| event.kind == "signal_admitted")
                .count(),
            1
        );
    }
    eprintln!(
        "BPMN_SIGNAL_MEASUREMENT {}",
        json!({"case":"thirty_three_settled_payloads_prune32",
        "sample_count":2,"p50_us":percentile_us(&[first_us,second_us],50),
        "p95_us":percentile_us(&[first_us,second_us],95),
        "prune_lag_ms_min":min_lag,"prune_lag_ms_max":max_lag,
        "pruned":pruned,"replay_delivered":replay.delivered,"storage":signal_storage(&fixture)})
    );
}

#[test]
fn signal_delivered_receipt_and_source_history_survive_payload_pruning_and_replay() {
    let fixture = Fixture::new();
    let recipient_version = publish_model(&fixture, &signal_catch_model());
    let recipient = start_version(&fixture, &recipient_version);
    let source_version = publish_model(&fixture, &signal_throw_model());
    let source_instance_id = Uuid::new_v4().to_string();
    let source_variables = serde_json::to_value(&source_version.model.variables).unwrap();
    let source_command = stamp("start signal whose delivered receipt will be pruned");
    let source_at_ms = chrono::Utc::now().timestamp_millis();
    let sender = repository::start_instance(
        &fixture.db,
        &fixture.owner,
        &source_command,
        &source_instance_id,
        &source_version.definition_id,
        source_version.version,
        &source_variables,
        repository::ProcessPlanInput::Canonical,
        source_at_ms,
    )
    .unwrap();
    assert_eq!(sender.status, ProcessInstanceStatus::Completed);
    let drained_at_ms = chrono::Utc::now().timestamp_millis() + 10;
    let drained = messages::drain_pending(&fixture.db, drained_at_ms);
    drained.completion.unwrap();
    assert_eq!(drained.delivered, 1);
    let (signal_id, settled_at_ms, source_event_id, payload_hash): (String, i64, String, String) =
        fixture
            .db
            .read()
            .unwrap()
            .query_row(
                "SELECT signal_id,settled_at_ms,source_event_id,payload_sha256 \
             FROM bpmn_signal_emissions WHERE source_instance_id=?1 AND status='settled'",
                [&sender.instance_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
    let (receipt_id, receipt_status, ordinal): (String, String, i64) = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT receipt_id,status,recipient_ordinal FROM bpmn_signal_receipts \
            WHERE signal_id=?1",
            [&signal_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let receipt_count: i64 = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM bpmn_signal_receipts WHERE signal_id=?1",
            [&signal_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(receipt_count, 1);
    assert_eq!(receipt_status, "delivered");
    assert_eq!(ordinal, 0);
    let retention_ms = 7 * 24 * 60 * 60 * 1000_i64;
    assert_eq!(
        repository::prune_signal_payloads(&fixture.db, settled_at_ms + retention_ms - 1).unwrap(),
        0
    );
    assert_eq!(
        repository::prune_signal_payloads(&fixture.db, settled_at_ms + retention_ms).unwrap(),
        1
    );
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let (payload, source_id, hash): (Option<String>, String, String) = reopened
        .read()
        .unwrap()
        .query_row(
            "SELECT payload_json,source_event_id,payload_sha256 \
            FROM bpmn_signal_emissions WHERE signal_id=?1",
            [&signal_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(payload, None);
    assert_eq!((source_id, hash), (source_event_id.clone(), payload_hash));
    let (retained_id, retained_status, retained_ordinal): (String, String, i64) = reopened
        .read()
        .unwrap()
        .query_row(
            "SELECT receipt_id,status,recipient_ordinal \
            FROM bpmn_signal_receipts WHERE signal_id=?1",
            [&signal_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (retained_id, retained_status, retained_ordinal),
        (receipt_id.clone(), "delivered".to_string(), ordinal)
    );
    assert_eq!(
        repository::get_instance(&reopened, &fixture.owner, &recipient.instance_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Completed
    );
    let before_replay = super::signal_proof_tests::all_transition_rows(&fixture);
    assert!(repository::claim_signal_receipt(
        &reopened,
        &receipt_id,
        settled_at_ms + retention_ms + 1
    )
    .unwrap()
    .is_none());
    let replay = messages::drain_pending(&reopened, settled_at_ms + retention_ms + 1);
    replay.completion.unwrap();
    assert_eq!(replay.delivered, 0);
    let replayed_sender = repository::start_instance(
        &reopened,
        &fixture.owner,
        &source_command,
        &source_instance_id,
        &source_version.definition_id,
        source_version.version,
        &source_variables,
        repository::ProcessPlanInput::Canonical,
        source_at_ms,
    )
    .unwrap();
    assert_eq!(replayed_sender.instance_id, sender.instance_id);
    assert_eq!(replayed_sender.status, ProcessInstanceStatus::Completed);
    assert_eq!(
        super::signal_proof_tests::all_transition_rows(&fixture),
        before_replay
    );
    let received =
        repository::list_events(&reopened, &fixture.owner, &recipient.instance_id, 0, 200)
            .unwrap()
            .0;
    let received: Vec<_> = received
        .iter()
        .filter(|event| event.kind == "signal_received")
        .collect();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].data["source_event_id"], source_event_id);
}

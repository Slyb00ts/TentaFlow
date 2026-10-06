// ============ File: send_boundary_tests.rs — Durable pending Send boundary behavior ============

use super::messages::test_support::start_version;
use super::repository::{self, ProcessPlanInput};
use super::runtime::test_support::{edge, publish_model, Fixture};
use tentaflow_protocol::processes::{
    ProcessInstanceStatus, ProcessNode, ProcessNodeKind, ProcessTimerSpec, ProcessTimerStatus,
};
use uuid::Uuid;

pub(super) fn timer_bound_send_model(
    definition_id: &str,
    instance_id: &str,
) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = super::send_receive_tests::send_model(definition_id, instance_id);
    model.timer_timezone = Some("UTC".into());
    model.nodes.push(ProcessNode {
        id: "SendTimer".into(),
        name: "Send admission deadline".into(),
        kind: ProcessNodeKind::BoundaryTimer {
            attached_to_id: "Send_1".into(),
            cancel_activity: true,
            timer: ProcessTimerSpec::Duration { seconds: 1 },
        },
        repeat: None,
    });
    model.nodes.push(ProcessNode {
        id: "TimeoutEnd".into(),
        name: "Deadline reached".into(),
        kind: ProcessNodeKind::End,
        repeat: None,
    });
    model
        .sequence_flows
        .push(edge("TimeoutFlow", "SendTimer", "TimeoutEnd"));
    model
}

fn message_bound_send_model(
    definition_id: &str,
    instance_id: &str,
) -> tentaflow_protocol::processes::ProcessModel {
    use std::collections::BTreeMap;
    let mut model = super::send_receive_tests::send_model(definition_id, instance_id);
    model.nodes.push(ProcessNode {
        id: "SendMessage".into(),
        name: "Send admission interrupted".into(),
        kind: ProcessNodeKind::BoundaryMessage {
            attached_to_id: "Send_1".into(),
            cancel_activity: true,
            message_ref: "Evidence".into(),
            correlation_expression: "'case-1'".into(),
            output_mapping: BTreeMap::new(),
        },
        repeat: None,
    });
    model.nodes.push(ProcessNode {
        id: "MessageEnd".into(),
        name: "Interrupted".into(),
        kind: ProcessNodeKind::End,
        repeat: None,
    });
    model
        .sequence_flows
        .push(edge("MessageFlow", "SendMessage", "MessageEnd"));
    model
}

pub(super) fn noninterrupting_timer_script_send_model(
    definition_id: &str,
    instance_id: &str,
) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = timer_bound_send_model(definition_id, instance_id);
    let boundary = model.nodes.iter_mut().find(|node| node.id == "SendTimer").unwrap();
    let ProcessNodeKind::BoundaryTimer { cancel_activity, .. } = &mut boundary.kind else {
        panic!("pinned timer boundary")
    };
    *cancel_activity = false;
    let script = model.nodes.iter_mut().find(|node| node.id == "TimeoutEnd").unwrap();
    script.kind = ProcessNodeKind::ScriptTask {
        script: "'timer branch'".into(),
        output_mapping: std::collections::BTreeMap::new(),
    };
    model.nodes.push(ProcessNode {
        id: "TimerBranchEnd".into(),
        name: "Timer branch completed".into(),
        kind: ProcessNodeKind::End,
        repeat: None,
    });
    model.sequence_flows.push(edge("ScriptEndFlow", "TimeoutEnd", "TimerBranchEnd"));
    model
}

#[test]
fn noninterrupting_timer_script_branch_keeps_pending_send_for_canonical_admission() {
    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let source = publish_model(&fixture, &noninterrupting_timer_script_send_model(
        &target.definition_id, &receiver.instance_id));
    let pending = start_version(&fixture, &source);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &pending.instance_id).unwrap();
    let waiting = snapshot.tokens.iter().find(|token|
        token.node_id == "Send_1" && token.status == "waiting").unwrap().token_id.clone();
    let timer = snapshot.timers.iter().find(|timer| timer.node_id == "SendTimer").unwrap();
    let due = timer.due_at_ms.unwrap();
    let drained = super::timers::drain_due(&fixture.db, due + 1);
    drained.completion.unwrap();
    assert_eq!(drained.fired, 1);
    let events = repository::list_events(&fixture.db, &fixture.owner,
        &pending.instance_id, 0, 200).unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "timer_fired").count(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "script_completed").count(), 1);
    assert!(events.iter().all(|event| event.kind != "send_task_admitted"));
    let after = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &pending.instance_id).unwrap();
    assert!(after.tokens.iter().any(|token| token.token_id == waiting
        && token.status == "waiting"));
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let admitted = repository::admit_pending_send(&reopened, &waiting, due + 2,
        ProcessPlanInput::Canonical).unwrap().unwrap();
    assert_eq!(admitted.instance.status, ProcessInstanceStatus::Completed);
    assert!(repository::admit_pending_send(&reopened, &waiting, due + 2,
        ProcessPlanInput::Canonical).unwrap().is_none());
}

#[test]
fn pending_send_admits_one_real_outbox_message_and_cancels_its_boundary() {
    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let sender = publish_model(
        &fixture,
        &timer_bound_send_model(&target.definition_id, &receiver.instance_id),
    );
    let pending = start_version(&fixture, &sender);
    assert_eq!(pending.status, ProcessInstanceStatus::Waiting);
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &pending.instance_id).unwrap();
    let waiting = snapshot
        .tokens
        .iter()
        .find(|token| token.node_id == "Send_1" && token.status == "waiting")
        .unwrap();
    let timer = snapshot
        .timers
        .iter()
        .find(|timer| timer.node_id == "SendTimer")
        .unwrap();
    assert_eq!(timer.status, ProcessTimerStatus::Pending);
    let before = repository::list_events(&fixture.db, &fixture.owner, &pending.instance_id, 0, 200)
        .unwrap()
        .0;
    assert_eq!(
        before
            .iter()
            .filter(|event| event.kind == "send_task_pending")
            .count(),
        1
    );
    assert_eq!(
        before
            .iter()
            .filter(|event| event.kind == "send_task_admitted")
            .count(),
        0
    );
    let at_ms = chrono::Utc::now().timestamp_millis();
    let committed = repository::admit_pending_send(
        &fixture.db,
        &waiting.token_id,
        at_ms,
        ProcessPlanInput::Canonical,
    )
    .unwrap()
    .unwrap();
    assert_eq!(committed.instance.status, ProcessInstanceStatus::Completed);
    let history =
        repository::list_events(&fixture.db, &fixture.owner, &pending.instance_id, 0, 200)
            .unwrap()
            .0;
    let admitted = history
        .iter()
        .find(|event| event.kind == "send_task_admitted")
        .unwrap();
    let message_id = admitted.data["message_id"].as_str().unwrap();
    assert_eq!(
        history
            .iter()
            .filter(|event| event.kind == "send_task_admitted")
            .count(),
        1
    );
    assert_eq!(
        history
            .iter()
            .filter(|event| event.kind == "send_admission_failed")
            .count(),
        0
    );
    assert_eq!(
        repository::get_message(
            &fixture.db,
            &fixture.owner,
            &fixture.owner.user_id,
            message_id
        )
        .unwrap()
        .message
        .status,
        tentaflow_protocol::processes::ProcessMessageStatus::Pending
    );
    let after =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &pending.instance_id).unwrap();
    assert_eq!(
        after
            .timers
            .iter()
            .find(|row| row.timer_id == timer.timer_id)
            .unwrap()
            .status,
        ProcessTimerStatus::Cancelled
    );
    let after_admission = super::signal_proof_tests::all_transition_rows(&fixture);
    let drained = super::timers::drain_due(&fixture.db, timer.due_at_ms.unwrap() + 1);
    drained.completion.unwrap();
    assert_eq!(drained.fired, 0);
    assert_eq!(
        super::signal_proof_tests::all_transition_rows(&fixture),
        after_admission,
        "a cancelled boundary timer changed the admitted Send"
    );
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::admit_pending_send(
        &reopened,
        &waiting.token_id,
        at_ms,
        ProcessPlanInput::Canonical
    )
    .unwrap()
    .is_none());
    assert_eq!(
        repository::get_message(
            &reopened,
            &fixture.owner,
            &fixture.owner.user_id,
            message_id
        )
        .unwrap()
        .message
        .status,
        tentaflow_protocol::processes::ProcessMessageStatus::Pending
    );
}

#[test]
fn real_boundary_timer_consumes_pending_send_before_outbox_admission() {
    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let sender = publish_model(
        &fixture,
        &timer_bound_send_model(&target.definition_id, &receiver.instance_id),
    );
    let pending = start_version(&fixture, &sender);
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &pending.instance_id).unwrap();
    let waiting = snapshot
        .tokens
        .iter()
        .find(|token| token.node_id == "Send_1" && token.status == "waiting")
        .unwrap();
    let due = snapshot
        .timers
        .iter()
        .find(|timer| timer.node_id == "SendTimer")
        .unwrap()
        .due_at_ms
        .unwrap();
    let drained = super::timers::drain_due(&fixture.db, due + 1);
    drained.completion.unwrap();
    assert_eq!(drained.fired, 1);
    let history =
        repository::list_events(&fixture.db, &fixture.owner, &pending.instance_id, 0, 200)
            .unwrap()
            .0;
    assert_eq!(
        history
            .iter()
            .filter(|event| event.kind == "timer_fired")
            .count(),
        1
    );
    assert_eq!(
        history
            .iter()
            .filter(|event| event.kind == "send_task_admitted")
            .count(),
        0
    );
    assert!(repository::admit_pending_send(
        &fixture.db,
        &waiting.token_id,
        due + 1,
        ProcessPlanInput::Canonical
    )
    .unwrap()
    .is_none());
    assert_eq!(
        repository::get_instance(&fixture.db, &fixture.owner, &pending.instance_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Completed
    );
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::admit_pending_send(
        &reopened,
        &waiting.token_id,
        due + 2,
        ProcessPlanInput::Canonical
    )
    .unwrap()
    .is_none());
}

#[test]
fn finite_dynamic_send_failure_is_recorded_once_and_never_retried_after_boundary_closure() {
    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let mut model = timer_bound_send_model(&target.definition_id, &receiver.instance_id);
    model.variables.insert("case".into(), serde_json::json!(42));
    let send = model
        .nodes
        .iter_mut()
        .find(|node| node.id == "Send_1")
        .unwrap();
    let ProcessNodeKind::SendTask {
        correlation_expression,
        ..
    } = &mut send.kind
    else {
        panic!("pinned SendTask")
    };
    *correlation_expression = "vars.case".into();
    let version = publish_model(&fixture, &model);
    let pending = start_version(&fixture, &version);
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &pending.instance_id).unwrap();
    let waiting = snapshot
        .tokens
        .iter()
        .find(|token| token.node_id == "Send_1" && token.status == "waiting")
        .unwrap();
    let timer = snapshot
        .timers
        .iter()
        .find(|timer| timer.node_id == "SendTimer")
        .unwrap();
    let admitted = repository::admit_pending_send(
        &fixture.db,
        &waiting.token_id,
        chrono::Utc::now().timestamp_millis(),
        ProcessPlanInput::Canonical,
    )
    .unwrap()
    .unwrap();
    assert_eq!(admitted.instance.status, ProcessInstanceStatus::Incident);
    let before = repository::list_events(&fixture.db, &fixture.owner, &pending.instance_id, 0, 200)
        .unwrap()
        .0;
    let failed = before
        .iter()
        .find(|event| event.kind == "send_admission_failed")
        .unwrap();
    assert_eq!(failed.data["code"], "MESSAGE_EXPRESSION_ERROR");
    assert_eq!(failed.data["pending_token_id"], waiting.token_id);
    assert_eq!(
        before
            .iter()
            .filter(|event| event.kind == "send_task_admitted")
            .count(),
        0
    );
    assert!(repository::admit_pending_send(
        &fixture.db,
        &waiting.token_id,
        chrono::Utc::now().timestamp_millis(),
        ProcessPlanInput::Canonical
    )
    .unwrap()
    .is_none());
    let due = timer.due_at_ms.unwrap();
    let drained = super::timers::drain_due(&fixture.db, due + 1);
    drained.completion.unwrap();
    assert_eq!(drained.fired, 1);
    let after = repository::list_events(&fixture.db, &fixture.owner, &pending.instance_id, 0, 200)
        .unwrap()
        .0;
    assert_eq!(
        after
            .iter()
            .filter(|event| event.kind == "send_admission_failed")
            .count(),
        1
    );
    assert_eq!(
        after
            .iter()
            .filter(|event| event.kind == "send_task_admitted")
            .count(),
        0
    );
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::admit_pending_send(
        &reopened,
        &waiting.token_id,
        due + 2,
        ProcessPlanInput::Canonical
    )
    .unwrap()
    .is_none());
}

#[test]
fn injected_resolved_incident_cannot_retry_an_immutable_failed_send_source() {
    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let mut model = timer_bound_send_model(&target.definition_id, &receiver.instance_id);
    model.variables.insert("case".into(), serde_json::json!(42));
    let send = model
        .nodes
        .iter_mut()
        .find(|node| node.id == "Send_1")
        .unwrap();
    let ProcessNodeKind::SendTask {
        correlation_expression,
        ..
    } = &mut send.kind
    else {
        panic!("pinned SendTask")
    };
    *correlation_expression = "vars.case".into();
    let version = publish_model(&fixture, &model);
    let pending = start_version(&fixture, &version);
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &pending.instance_id).unwrap();
    let waiting = snapshot
        .tokens
        .iter()
        .find(|token| token.node_id == "Send_1" && token.status == "waiting")
        .unwrap();
    repository::admit_pending_send(
        &fixture.db,
        &waiting.token_id,
        chrono::Utc::now().timestamp_millis(),
        ProcessPlanInput::Canonical,
    )
    .unwrap()
    .unwrap();
    let failed = repository::list_events(&fixture.db, &fixture.owner, &pending.instance_id, 0, 200)
        .unwrap()
        .0
        .into_iter()
        .find(|event| event.kind == "send_admission_failed")
        .unwrap();
    assert_eq!(failed.data["pending_token_id"], waiting.token_id);
    let affected = fixture
        .db
        .write()
        .unwrap()
        .execute(
            "UPDATE bpmn_incidents SET resolved_at_ms=?1 WHERE instance_id=?2 AND incident_id=?3",
            rusqlite::params![
                chrono::Utc::now().timestamp_millis(),
                pending.instance_id,
                failed.data["incident_id"].as_str().unwrap()
            ],
        )
        .unwrap();
    assert_eq!(
        affected, 1,
        "the injected resolved state must target the factual incident"
    );
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    assert!(repository::admit_pending_send(
        &fixture.db,
        &waiting.token_id,
        chrono::Utc::now().timestamp_millis(),
        ProcessPlanInput::Canonical
    )
    .unwrap()
    .is_none());
    assert_eq!(
        super::signal_proof_tests::all_transition_rows(&fixture),
        before
    );
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::admit_pending_send(
        &reopened,
        &waiting.token_id,
        chrono::Utc::now().timestamp_millis(),
        ProcessPlanInput::Canonical
    )
    .unwrap()
    .is_none());
}

#[test]
fn real_boundary_message_wins_against_pending_send_without_creating_an_outbox_row() {
    use super::messages::test_support::{catch_target, envelope, send};
    use tentaflow_protocol::processes::ProcessSubscriptionStatus;

    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let sender = publish_model(
        &fixture,
        &message_bound_send_model(&target.definition_id, &receiver.instance_id),
    );
    let pending = start_version(&fixture, &sender);
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &pending.instance_id).unwrap();
    let waiting = snapshot
        .tokens
        .iter()
        .find(|token| token.node_id == "Send_1" && token.status == "waiting")
        .unwrap();
    let boundary = snapshot
        .subscriptions
        .iter()
        .find(|subscription| subscription.node_id == "SendMessage")
        .unwrap();
    assert_eq!(boundary.status, ProcessSubscriptionStatus::Open);
    let message = envelope(
        catch_target(
            &sender,
            Some(&pending.instance_id),
            Some(&boundary.subscription_id),
        ),
        serde_json::Value::Null,
    );
    send(&fixture, &message);
    let drained =
        super::messages::drain_pending(&fixture.db, chrono::Utc::now().timestamp_millis());
    drained.completion.unwrap();
    assert_eq!(drained.delivered, 1);
    let history =
        repository::list_events(&fixture.db, &fixture.owner, &pending.instance_id, 0, 200)
            .unwrap()
            .0;
    assert_eq!(
        history
            .iter()
            .filter(|event| event.kind == "message_delivered")
            .count(),
        1
    );
    assert_eq!(
        history
            .iter()
            .filter(|event| event.kind == "send_task_admitted")
            .count(),
        0
    );
    assert!(repository::admit_pending_send(
        &fixture.db,
        &waiting.token_id,
        chrono::Utc::now().timestamp_millis(),
        ProcessPlanInput::Canonical
    )
    .unwrap()
    .is_none());
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert_eq!(
        repository::get_instance(&reopened, &fixture.owner, &pending.instance_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Completed
    );
}

#[test]
fn admitted_send_makes_its_message_boundary_a_stale_delivery_candidate() {
    use super::messages::test_support::{catch_target, envelope, send};

    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let sender = publish_model(
        &fixture,
        &message_bound_send_model(&target.definition_id, &receiver.instance_id),
    );
    let pending = start_version(&fixture, &sender);
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &pending.instance_id).unwrap();
    let waiting = snapshot
        .tokens
        .iter()
        .find(|token| token.node_id == "Send_1" && token.status == "waiting")
        .unwrap();
    let boundary = snapshot
        .subscriptions
        .iter()
        .find(|subscription| subscription.node_id == "SendMessage")
        .unwrap();
    let candidate = envelope(
        catch_target(
            &sender,
            Some(&pending.instance_id),
            Some(&boundary.subscription_id),
        ),
        serde_json::Value::Null,
    );
    assert_eq!(
        boundary.status,
        tentaflow_protocol::processes::ProcessSubscriptionStatus::Open
    );
    let sent = send(&fixture, &candidate);
    assert_eq!(sent.message_id, candidate.message_id);
    let committed = repository::admit_pending_send(
        &fixture.db,
        &waiting.token_id,
        chrono::Utc::now().timestamp_millis(),
        ProcessPlanInput::Canonical,
    )
    .unwrap()
    .unwrap();
    assert_eq!(committed.instance.status, ProcessInstanceStatus::Completed);
    let after_admission =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &pending.instance_id).unwrap();
    assert_eq!(
        after_admission
            .subscriptions
            .iter()
            .find(|row| row.subscription_id == boundary.subscription_id)
            .unwrap()
            .status,
        tentaflow_protocol::processes::ProcessSubscriptionStatus::Cancelled
    );
    let drained =
        super::messages::drain_pending(&fixture.db, chrono::Utc::now().timestamp_millis());
    drained.completion.unwrap();
    let stale = repository::get_message(
        &fixture.db,
        &fixture.owner,
        &fixture.owner.user_id,
        &candidate.message_id,
    )
    .unwrap();
    assert_eq!(
        stale.message.status,
        tentaflow_protocol::processes::ProcessMessageStatus::Cancelled
    );
    assert_eq!(
        stale.message.last_reason.as_deref(),
        Some("activation_closed")
    );
    let history =
        repository::list_events(&fixture.db, &fixture.owner, &pending.instance_id, 0, 200)
            .unwrap()
            .0;
    assert_eq!(
        history
            .iter()
            .filter(|event| event.kind == "send_task_admitted")
            .count(),
        1
    );
    let admitted_id = history
        .iter()
        .find(|event| event.kind == "send_task_admitted")
        .unwrap()
        .data["message_id"]
        .as_str()
        .unwrap();
    assert_ne!(admitted_id, candidate.message_id);
    assert_eq!(
        history
            .iter()
            .filter(|event| event.kind == "message_delivered"
                && event.data["message_id"].as_str() == Some(candidate.message_id.as_str()))
            .count(),
        0
    );
    assert_eq!(
        history
            .iter()
            .filter(|event| event.kind == "message_delivered"
                && event.data["message_id"].as_str() == Some(admitted_id))
            .count(),
        1
    );
    let delivered = repository::get_message(
        &fixture.db,
        &fixture.owner,
        &fixture.owner.user_id,
        admitted_id,
    )
    .unwrap();
    assert_eq!(
        delivered.message.status,
        tentaflow_protocol::processes::ProcessMessageStatus::Delivered
    );
    assert_eq!(
        delivered.message.source_instance_id.as_deref(),
        Some(pending.instance_id.as_str())
    );
    assert_eq!(
        delivered.message.matched_instance_id.as_deref(),
        Some(receiver.instance_id.as_str())
    );
    let receiver_history =
        repository::list_events(&fixture.db, &fixture.owner, &receiver.instance_id, 0, 200)
            .unwrap()
            .0;
    assert_eq!(
        receiver_history
            .iter()
            .filter(|event| event.kind == "message_delivered")
            .count(),
        1
    );
    assert_eq!(
        repository::get_instance(&fixture.db, &fixture.owner, &receiver.instance_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Completed
    );
    let admitted_outbox: i64 = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM bpmn_messages WHERE source_instance_id=?1",
            [&pending.instance_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(admitted_outbox, 1);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let retained = repository::get_message(
        &reopened,
        &fixture.owner,
        &fixture.owner.user_id,
        &candidate.message_id,
    )
    .unwrap();
    assert_eq!(
        retained.message.status,
        tentaflow_protocol::processes::ProcessMessageStatus::Cancelled
    );
    assert_eq!(
        retained.message.last_reason.as_deref(),
        Some("activation_closed")
    );
    assert_eq!(
        repository::get_message(
            &reopened,
            &fixture.owner,
            &fixture.owner.user_id,
            admitted_id,
        )
        .unwrap()
        .message
        .status,
        tentaflow_protocol::processes::ProcessMessageStatus::Delivered
    );
    assert_eq!(
        repository::get_instance(&reopened, &fixture.owner, &pending.instance_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Completed
    );
    assert_eq!(
        repository::get_instance(&reopened, &fixture.owner, &receiver.instance_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Completed
    );
}

#[test]
fn message_commit_wins_a_real_prewrite_race_against_pending_send_admission() {
    use super::messages::test_support::{catch_target, envelope, send};
    use std::sync::mpsc::sync_channel;
    use std::time::Duration;

    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let sender = publish_model(
        &fixture,
        &message_bound_send_model(&target.definition_id, &receiver.instance_id),
    );
    let pending = start_version(&fixture, &sender);
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &pending.instance_id).unwrap();
    let waiting_id = snapshot
        .tokens
        .iter()
        .find(|token| token.node_id == "Send_1" && token.status == "waiting")
        .unwrap()
        .token_id
        .clone();
    let boundary_id = snapshot
        .subscriptions
        .iter()
        .find(|subscription| subscription.node_id == "SendMessage")
        .unwrap()
        .subscription_id
        .clone();
    let candidate = envelope(
        catch_target(&sender, Some(&pending.instance_id), Some(&boundary_id)),
        serde_json::Value::Null,
    );
    send(&fixture, &candidate);
    let (ready_tx, ready_rx) = sync_channel(1);
    let (resume_tx, resume_rx) = sync_channel(1);
    let db = fixture.db.clone();
    let waiting_for_thread = waiting_id.clone();
    std::thread::scope(|scope| {
        let resume_tx = resume_tx;
        let worker = scope.spawn(move || {
            repository::SEND_ADMISSION_PREFLIGHT
                .with(|gate| *gate.borrow_mut() = Some((ready_tx, resume_rx)));
            repository::admit_pending_send(
                &db,
                &waiting_for_thread,
                chrono::Utc::now().timestamp_millis(),
                ProcessPlanInput::Canonical,
            )
        });
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let drained =
            super::messages::drain_pending(&fixture.db, chrono::Utc::now().timestamp_millis());
        let after_message = super::signal_proof_tests::all_transition_rows(&fixture);
        resume_tx.send(()).unwrap();
        drained.completion.unwrap();
        assert_eq!(drained.delivered, 1);
        assert!(worker.join().unwrap().unwrap().is_none());
        assert_eq!(
            super::signal_proof_tests::all_transition_rows(&fixture),
            after_message
        );
    });
    let history =
        repository::list_events(&fixture.db, &fixture.owner, &pending.instance_id, 0, 200)
            .unwrap()
            .0;
    assert_eq!(
        history
            .iter()
            .filter(|event| event.kind == "message_delivered")
            .count(),
        1
    );
    assert_eq!(
        history
            .iter()
            .filter(|event| event.kind == "send_task_admitted")
            .count(),
        0
    );
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::admit_pending_send(
        &reopened,
        &waiting_id,
        chrono::Utc::now().timestamp_millis(),
        ProcessPlanInput::Canonical
    )
    .unwrap()
    .is_none());
}

#[test]
fn pinned_called_send_boundary_is_refused_before_creating_a_child() {
    use std::collections::BTreeMap;

    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let called = publish_model(
        &fixture,
        &timer_bound_send_model(&target.definition_id, &receiver.instance_id),
    );
    let caller = publish_model(
        &fixture,
        &super::call_tests::caller(&called, BTreeMap::new()),
    );
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let instance_id = Uuid::new_v4().to_string();
    let variables = serde_json::to_value(&caller.model.variables).unwrap();
    let command = super::runtime::test_support::stamp("called pending Send refusal");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = super::runtime::plan_start(
        &caller.model,
        &instance_id,
        &fixture.owner,
        &caller.definition_id,
        caller.version,
        variables.clone(),
        super::runtime::StartCause::Manual,
        at_ms,
        super::runtime::test_support::manual_input(&command),
        None,
    )
    .unwrap();
    let rejected = repository::start_instance(
        &fixture.db,
        &fixture.owner,
        &command,
        &instance_id,
        &caller.definition_id,
        caller.version,
        &variables,
        ProcessPlanInput::Supplied(&plan),
        at_ms,
    )
    .unwrap_err();
    assert!(
        format!("{rejected:#}").contains(
            "boundary-attached Send in a called process requires a proved call admission profile"
        ),
        "called Send refusal changed: {rejected:#}"
    );
    assert_eq!(
        super::signal_proof_tests::all_transition_rows(&fixture),
        before
    );
    let normal = publish_model(
        &fixture,
        &super::send_receive_tests::send_model(&target.definition_id, &receiver.instance_id),
    );
    let normal_caller = publish_model(
        &fixture,
        &super::call_tests::caller(&normal, BTreeMap::new()),
    );
    let finished = start_version(&fixture, &normal_caller);
    assert_eq!(finished.status, ProcessInstanceStatus::Completed);
}

#[test]
fn timer_commit_wins_a_real_prewrite_race_against_pending_send_admission() {
    use std::sync::mpsc::sync_channel;
    use std::time::Duration;

    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let sender = publish_model(
        &fixture,
        &timer_bound_send_model(&target.definition_id, &receiver.instance_id),
    );
    let pending = start_version(&fixture, &sender);
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &pending.instance_id).unwrap();
    let waiting_id = snapshot
        .tokens
        .iter()
        .find(|token| token.node_id == "Send_1" && token.status == "waiting")
        .unwrap()
        .token_id
        .clone();
    let due = snapshot
        .timers
        .iter()
        .find(|timer| timer.node_id == "SendTimer")
        .unwrap()
        .due_at_ms
        .unwrap();
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let (ready_tx, ready_rx) = sync_channel(1);
    let (resume_tx, resume_rx) = sync_channel(1);
    let db = fixture.db.clone();
    let waiting_for_thread = waiting_id.clone();
    std::thread::scope(|scope| {
        let resume_tx = resume_tx;
        let worker = scope.spawn(move || {
            repository::SEND_ADMISSION_PREFLIGHT
                .with(|gate| *gate.borrow_mut() = Some((ready_tx, resume_rx)));
            repository::admit_pending_send(
                &db,
                &waiting_for_thread,
                due + 1,
                ProcessPlanInput::Canonical,
            )
        });
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let drained = super::timers::drain_due(&fixture.db, due + 1);
        let after_timer = super::signal_proof_tests::all_transition_rows(&fixture);
        resume_tx.send(()).unwrap();
        drained.completion.unwrap();
        assert_eq!(drained.fired, 1);
        assert!(worker.join().unwrap().unwrap().is_none());
        assert_eq!(
            super::signal_proof_tests::all_transition_rows(&fixture),
            after_timer
        );
    });
    assert_ne!(
        super::signal_proof_tests::all_transition_rows(&fixture),
        before
    );
    let history =
        repository::list_events(&fixture.db, &fixture.owner, &pending.instance_id, 0, 200)
            .unwrap()
            .0;
    assert_eq!(
        history
            .iter()
            .filter(|event| event.kind == "timer_fired")
            .count(),
        1
    );
    assert_eq!(
        history
            .iter()
            .filter(|event| event.kind == "send_task_admitted")
            .count(),
        0
    );
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::admit_pending_send(
        &reopened,
        &waiting_id,
        due + 2,
        ProcessPlanInput::Canonical
    )
    .unwrap()
    .is_none());
}

#[test]
fn cursor_revisits_older_pending_sends_across_physical_pages_and_continuing_inserts() {
    use std::time::Instant;

    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let source = publish_model(
        &fixture,
        &timer_bound_send_model(&target.definition_id, &receiver.instance_id),
    );
    let mut instances = (0..65)
        .map(|_| start_version(&fixture, &source).instance_id)
        .collect::<Vec<_>>();
    let mut cursor = super::messages::SendCursor::default();
    let query_plan = {
        let conn = fixture.db.read().unwrap();
        let mut statement = conn
            .prepare(
                "EXPLAIN QUERY PLAN SELECT rowid,token_id FROM bpmn_tokens \
             WHERE rowid>?1 AND rowid<=?2 ORDER BY rowid LIMIT 64",
            )
            .unwrap();
        statement
            .query_map(rusqlite::params![0_i64, i64::MAX], |row| {
                row.get::<_, String>(3)
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    eprintln!("pending Send cursor query plan: {query_plan:?}");
    let initial_page = repository::pending_send_page(
        &fixture.db,
        0,
        repository::pending_send_high_water(&fixture.db).unwrap(),
    )
    .unwrap();
    let first_started = Instant::now();
    let first = super::messages::drain_pending_sends(
        &fixture.db,
        &mut cursor,
        chrono::Utc::now().timestamp_millis(),
    )
    .unwrap();
    let first_inspected = initial_page
        .iter()
        .filter(|(rowid, _)| *rowid <= cursor.after_rowid)
        .count();
    eprintln!("pending Send wake 0: inspected {first_inspected} physical rows, admitted {first}, elapsed {:?}, cursor {}, high-water {}",
        first_started.elapsed(), cursor.after_rowid, cursor.high_water);
    assert!(first > 0 && first <= 32);
    instances.extend((0..5).map(|_| start_version(&fixture, &source).instance_id));
    let mut admitted = first;
    for wake in 1..=24 {
        if admitted == instances.len() {
            break;
        }
        let page =
            repository::pending_send_page(&fixture.db, cursor.after_rowid, cursor.high_water)
                .unwrap();
        let started = Instant::now();
        let count = super::messages::drain_pending_sends(
            &fixture.db,
            &mut cursor,
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
        let inspected = page
            .iter()
            .filter(|(rowid, _)| *rowid <= cursor.after_rowid)
            .count();
        eprintln!("pending Send wake {wake}: inspected {inspected} physical rows, admitted {count}, elapsed {:?}, cursor {}, high-water {}",
            started.elapsed(), cursor.after_rowid, cursor.high_water);
        assert!(count <= 32);
        admitted += count;
    }
    assert_eq!(
        admitted,
        instances.len(),
        "the high-water cursor starved an eligible Send"
    );
    for instance_id in &instances {
        let instance =
            repository::get_instance(&fixture.db, &fixture.owner, instance_id, None).unwrap();
        assert_eq!(instance.status, ProcessInstanceStatus::Completed);
        let events = repository::list_events(&fixture.db, &fixture.owner, instance_id, 0, 200)
            .unwrap()
            .0;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "send_task_pending")
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "send_task_admitted")
                .count(),
            1
        );
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let mut restarted_cursor = super::messages::SendCursor::default();
    let high_water = repository::pending_send_high_water(&reopened).unwrap();
    let sparse_page = repository::pending_send_page(&reopened, 0, high_water).unwrap();
    assert_eq!(
        sparse_page.len(),
        64,
        "the sparse probe needs a genuine full physical page"
    );
    let settled_before = super::signal_proof_tests::all_transition_rows(&fixture);
    assert_eq!(
        super::messages::drain_pending_sends(
            &reopened,
            &mut restarted_cursor,
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap(),
        0
    );
    assert_eq!(
        restarted_cursor.after_rowid, sparse_page[31].0,
        "a wake must inspect exactly 32 ineligible historical token rows"
    );
    assert_eq!(
        super::signal_proof_tests::all_transition_rows(&fixture),
        settled_before
    );
    assert_eq!(
        super::messages::drain_pending_sends(
            &reopened,
            &mut restarted_cursor,
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap(),
        0
    );
    assert_eq!(
        restarted_cursor.after_rowid, sparse_page[63].0,
        "the next wake must resume at the 33rd physical row"
    );
    assert_eq!(
        super::signal_proof_tests::all_transition_rows(&fixture),
        settled_before
    );
    for _ in 0..10 {
        assert_eq!(
            super::messages::drain_pending_sends(
                &reopened,
                &mut restarted_cursor,
                chrono::Utc::now().timestamp_millis()
            )
            .unwrap(),
            0
        );
    }
}

#[test]
fn noninterrupting_message_updates_the_factual_send_admission_variables() {
    use super::messages::test_support::{catch_target, envelope, send};

    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let mut model = message_bound_send_model(&target.definition_id, &receiver.instance_id);
    model
        .variables
        .insert("case".into(), serde_json::json!("before"));
    let send_node = model
        .nodes
        .iter_mut()
        .find(|node| node.id == "Send_1")
        .unwrap();
    let ProcessNodeKind::SendTask {
        correlation_expression,
        ..
    } = &mut send_node.kind
    else {
        panic!("pinned SendTask")
    };
    *correlation_expression = "vars.case".into();
    let boundary = model
        .nodes
        .iter_mut()
        .find(|node| node.id == "SendMessage")
        .unwrap();
    let ProcessNodeKind::BoundaryMessage {
        cancel_activity,
        output_mapping,
        ..
    } = &mut boundary.kind
    else {
        panic!("pinned message boundary")
    };
    *cancel_activity = false;
    output_mapping.insert("case".into(), "outputs".into());
    let source = publish_model(&fixture, &model);
    let pending = start_version(&fixture, &source);
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &pending.instance_id).unwrap();
    let waiting = snapshot
        .tokens
        .iter()
        .find(|token| token.node_id == "Send_1" && token.status == "waiting")
        .unwrap()
        .token_id
        .clone();
    let boundary = snapshot
        .subscriptions
        .iter()
        .find(|subscription| subscription.node_id == "SendMessage")
        .unwrap();
    let message = envelope(
        catch_target(
            &source,
            Some(&pending.instance_id),
            Some(&boundary.subscription_id),
        ),
        serde_json::json!("case-1"),
    );
    send(&fixture, &message);
    let drained =
        super::messages::drain_pending(&fixture.db, chrono::Utc::now().timestamp_millis());
    drained.completion.unwrap();
    assert_eq!(drained.delivered, 1);
    let after_boundary =
        repository::get_instance(&fixture.db, &fixture.owner, &pending.instance_id, None).unwrap();
    assert_eq!(after_boundary.variables["case"], "case-1");
    assert_eq!(after_boundary.status, ProcessInstanceStatus::Waiting);
    let admitted = repository::admit_pending_send(
        &fixture.db,
        &waiting,
        chrono::Utc::now().timestamp_millis(),
        ProcessPlanInput::Canonical,
    )
    .unwrap()
    .unwrap();
    assert_eq!(admitted.instance.status, ProcessInstanceStatus::Completed);
    let history =
        repository::list_events(&fixture.db, &fixture.owner, &pending.instance_id, 0, 200)
            .unwrap()
            .0;
    let delivered = history
        .iter()
        .position(|event| event.kind == "message_delivered")
        .unwrap();
    let admitted_index = history
        .iter()
        .position(|event| event.kind == "send_task_admitted")
        .unwrap();
    assert!(delivered < admitted_index);
    assert_eq!(history[admitted_index].data["correlation_key"], "case-1");
}

#[test]
fn revoked_initiator_cannot_dispatch_pending_send_and_gets_one_factual_failure() {
    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let source = publish_model(
        &fixture,
        &timer_bound_send_model(&target.definition_id, &receiver.instance_id),
    );
    let pending = start_version(&fixture, &source);
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &pending.instance_id).unwrap();
    let waiting = snapshot
        .tokens
        .iter()
        .find(|token| token.node_id == "Send_1" && token.status == "waiting")
        .unwrap()
        .token_id
        .clone();
    assert!(crate::services::org::repo::remove_membership(
        &fixture.db,
        &fixture.owner.org_id,
        &fixture.owner.user_id
    )
    .unwrap());
    let committed = repository::admit_pending_send(
        &fixture.db,
        &waiting,
        chrono::Utc::now().timestamp_millis(),
        ProcessPlanInput::Canonical,
    )
    .unwrap()
    .unwrap();
    assert_eq!(committed.instance.status, ProcessInstanceStatus::Incident);
    let conn = fixture.db.read().unwrap();
    let facts: i64 = conn.query_row(
        "SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1 AND kind='send_admission_failed' \
         AND json_extract(data_json,'$.code')='SEND_ADMISSION_AUTHORITY_DENIED'",
        [&pending.instance_id], |row| row.get(0)).unwrap();
    let outbox: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM bpmn_messages WHERE source_instance_id=?1",
            [&pending.instance_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(facts, 1);
    assert_eq!(outbox, 0);
    drop(conn);
    assert!(repository::admit_pending_send(
        &fixture.db,
        &waiting,
        chrono::Utc::now().timestamp_millis(),
        ProcessPlanInput::Canonical
    )
    .unwrap()
    .is_none());
}

#[test]
fn archived_start_target_yields_one_finite_unavailable_incident_without_outbox() {
    use tentaflow_protocol::processes::ProcessMessageTargetSpec;

    let fixture = Fixture::new();
    let target = publish_model(
        &fixture,
        &super::messages::test_support::receiving_model(true, false),
    );
    let mut model = timer_bound_send_model(&target.definition_id, &Uuid::new_v4().to_string());
    let node = model
        .nodes
        .iter_mut()
        .find(|node| node.id == "Send_1")
        .unwrap();
    let ProcessNodeKind::SendTask {
        target: address, ..
    } = &mut node.kind
    else {
        panic!("pinned SendTask")
    };
    *address = ProcessMessageTargetSpec::Start {
        definition_id: target.definition_id.clone(),
    };
    let source = publish_model(&fixture, &model);
    let pending = start_version(&fixture, &source);
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &pending.instance_id).unwrap();
    let waiting = snapshot
        .tokens
        .iter()
        .find(|token| token.node_id == "Send_1" && token.status == "waiting")
        .unwrap()
        .token_id
        .clone();
    let definition = repository::get_definition(&fixture.db, &fixture.owner, &target.definition_id)
        .unwrap()
        .0;
    repository::archive_definition(
        &fixture.db,
        &fixture.owner,
        &super::runtime::test_support::stamp("archive accepted target"),
        &target.definition_id,
        definition.draft_revision,
        true,
    )
    .unwrap();
    let result = repository::admit_pending_send(
        &fixture.db,
        &waiting,
        chrono::Utc::now().timestamp_millis(),
        ProcessPlanInput::Canonical,
    )
    .unwrap()
    .unwrap();
    assert_eq!(result.instance.status, ProcessInstanceStatus::Incident);
    let history =
        repository::list_events(&fixture.db, &fixture.owner, &pending.instance_id, 0, 200)
            .unwrap()
            .0;
    let failed = history
        .iter()
        .find(|event| event.kind == "send_admission_failed")
        .unwrap();
    assert_eq!(failed.data["code"], "SEND_ADMISSION_TARGET_UNAVAILABLE");
    assert_eq!(
        history
            .iter()
            .filter(|event| event.kind == "send_task_admitted")
            .count(),
        0
    );
    let conn = fixture.db.read().unwrap();
    let outbox: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM bpmn_messages WHERE source_instance_id=?1",
            [&pending.instance_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outbox, 0);
}

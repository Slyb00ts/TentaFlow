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
        activity_io: None,
    });
    model.nodes.push(ProcessNode {
        id: "TimeoutEnd".into(),
        name: "Deadline reached".into(),
        kind: ProcessNodeKind::End,
        repeat: None,
        activity_io: None,
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
        activity_io: None,
    });
    model.nodes.push(ProcessNode {
        id: "MessageEnd".into(),
        name: "Interrupted".into(),
        kind: ProcessNodeKind::End,
        repeat: None,
        activity_io: None,
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
        activity_io: None,
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
fn pinned_called_send_boundary_admits_after_a_real_child_wait_and_returns_once() {
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
    let parent = start_version(&fixture, &caller);
    assert_eq!(parent.status, ProcessInstanceStatus::Waiting);
    let child_id = super::call_tests::child_id(&fixture, &parent.instance_id);
    let child = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    let waiting = child.tokens.iter().find(|token|
        token.node_id == "Send_1" && token.status == "waiting").unwrap();
    let timer = child.timers.iter().find(|timer| timer.node_id == "SendTimer").unwrap();
    assert_eq!(timer.status, ProcessTimerStatus::Pending);
    let pending = repository::list_events(&fixture.db, &fixture.owner, &child_id, 0, 200)
        .unwrap().0;
    assert_eq!(pending.iter().filter(|event| event.kind == "send_task_pending").count(), 1);
    let definition = repository::get_definition(&fixture.db, &fixture.owner,
        &called.definition_id).unwrap().0;
    let mut changed_model = called.model.clone();
    changed_model.nodes.iter_mut().find(|node| node.id == "Send_1")
        .unwrap().name = "Send in a later published version".into();
    let draft = repository::save_definition(&fixture.db, &fixture.owner,
        &super::runtime::test_support::stamp("change called version after child start"),
        Some(&called.definition_id), definition.draft_revision,
        "Evidence process", "", &changed_model).unwrap();
    let published_v2 = repository::publish_definition(&fixture.db, &fixture.owner,
        &super::runtime::test_support::stamp("publish changed called version"),
        &called.definition_id, draft.draft_revision, &[], None).unwrap().1;
    assert!(published_v2.version > called.version);
    assert_eq!(child.instance.version, called.version);
    let committed = repository::admit_pending_send(&fixture.db, &waiting.token_id,
        chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical)
        .unwrap().unwrap();
    assert_eq!(committed.instance.status, ProcessInstanceStatus::Completed);
    let parent_after = repository::get_instance(&fixture.db, &fixture.owner,
        &parent.instance_id, None).unwrap();
    assert_eq!(parent_after.status, ProcessInstanceStatus::Completed);
    let child_history = repository::list_events(&fixture.db, &fixture.owner, &child_id, 0, 200)
        .unwrap().0;
    let admitted = child_history.iter().find(|event| event.kind == "send_task_admitted").unwrap();
    let message_id = admitted.data["message_id"].as_str().unwrap();
    assert_eq!(child_history.iter().filter(|event| event.kind == "send_task_admitted").count(), 1);
    assert_eq!(repository::get_message(&fixture.db, &fixture.owner,
        &fixture.owner.user_id, message_id).unwrap().message.status,
        tentaflow_protocol::processes::ProcessMessageStatus::Pending);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::admit_pending_send(&reopened, &waiting.token_id,
        chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical).unwrap().is_none());
    assert_eq!(repository::get_instance(&reopened, &fixture.owner,
        &parent.instance_id, None).unwrap().status, ProcessInstanceStatus::Completed);
}

#[test]
fn embedded_send_boundary_uses_its_child_scope_and_local_variables() {
    use serde_json::json;

    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let mut model = super::runtime::test_support::embedded_model(
        timer_bound_send_model(&target.definition_id, &receiver.instance_id), "InnerScope");
    model.variables.insert("payload_value".into(), json!("root"));
    let ProcessNodeKind::SubProcess { body, .. } = &mut model.nodes.iter_mut()
        .find(|node| node.id == "InnerScope").unwrap().kind else { unreachable!() };
    body.variables.insert("payload_value".into(), json!("child"));
    let send = body.nodes.iter_mut().find(|node| node.id == "Send_1").unwrap();
    let ProcessNodeKind::SendTask { payload_expression, .. } = &mut send.kind else { unreachable!() };
    *payload_expression = "vars.payload_value".into();
    let version = publish_model(&fixture, &model);
    let pending = start_version(&fixture, &version);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &pending.instance_id).unwrap();
    let waiting = snapshot.tokens.iter().find(|token|
        token.node_id == "Send_1" && token.status == "waiting").unwrap();
    assert_ne!(waiting.scope_id, pending.instance_id);
    let timer = snapshot.timers.iter().find(|timer| timer.node_id == "SendTimer").unwrap();
    assert_eq!(timer.scope_id.as_deref(), Some(waiting.scope_id.as_str()));
    let committed = repository::admit_pending_send(&fixture.db, &waiting.token_id,
        chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical)
        .unwrap().unwrap();
    assert_eq!(committed.instance.status, ProcessInstanceStatus::Completed);
    let history = repository::list_events(&fixture.db, &fixture.owner,
        &pending.instance_id, 0, 200).unwrap().0;
    let admitted = history.iter().find(|event| event.kind == "send_task_admitted").unwrap();
    assert_eq!(admitted.scope_id, waiting.scope_id);
    let message_id = admitted.data["message_id"].as_str().unwrap();
    assert_eq!(repository::get_message(&fixture.db, &fixture.owner,
        &fixture.owner.user_id, message_id).unwrap().payload, Some(json!("child")));
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::admit_pending_send(&reopened, &waiting.token_id,
        chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical).unwrap().is_none());
    assert_eq!(repository::get_instance(&reopened, &fixture.owner,
        &pending.instance_id, None).unwrap().status, ProcessInstanceStatus::Completed);
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
        process_id: None,
        start_node_id: None,
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

#[test]
fn embedded_boundary_timer_wins_before_child_send_admission() {
    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let model = super::runtime::test_support::embedded_model(
        timer_bound_send_model(&target.definition_id, &receiver.instance_id), "InnerScope");
    let version = publish_model(&fixture, &model);
    let pending = start_version(&fixture, &version);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &pending.instance_id).unwrap();
    let waiting = snapshot.tokens.iter().find(|token|
        token.node_id == "Send_1" && token.status == "waiting").unwrap();
    let timer = snapshot.timers.iter().find(|timer| timer.node_id == "SendTimer").unwrap();
    assert_eq!(timer.scope_id.as_deref(), Some(waiting.scope_id.as_str()));
    let drained = super::timers::drain_due(&fixture.db, timer.due_at_ms.unwrap() + 1);
    drained.completion.unwrap();
    assert_eq!(drained.fired, 1);
    let history = repository::list_events(&fixture.db, &fixture.owner,
        &pending.instance_id, 0, 200).unwrap().0;
    assert_eq!(history.iter().filter(|event| event.kind == "timer_fired"
        && event.scope_id == waiting.scope_id).count(), 1);
    assert_eq!(history.iter().filter(|event| event.kind == "send_task_admitted").count(), 0);
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::admit_pending_send(&reopened, &waiting.token_id,
        timer.due_at_ms.unwrap() + 2, ProcessPlanInput::Canonical).unwrap().is_none());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
    assert_eq!(repository::get_instance(&reopened, &fixture.owner,
        &pending.instance_id, None).unwrap().status, ProcessInstanceStatus::Completed);
}

#[test]
fn parent_call_cancellation_closes_child_send_without_admitting_an_outbox() {
    use std::collections::BTreeMap;

    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let called = publish_model(&fixture,
        &timer_bound_send_model(&target.definition_id, &receiver.instance_id));
    let caller = publish_model(&fixture,
        &super::call_tests::caller(&called, BTreeMap::new()));
    let parent = start_version(&fixture, &caller);
    let child_id = super::call_tests::child_id(&fixture, &parent.instance_id);
    let child = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    let waiting = child.tokens.iter().find(|token|
        token.node_id == "Send_1" && token.status == "waiting").unwrap();
    let cancelled = repository::cancel_instance(&fixture.db, &fixture.owner,
        &super::runtime::test_support::stamp("cancel pending called Send"),
        &parent.instance_id, parent.revision).unwrap();
    assert_eq!(cancelled.instance.status, ProcessInstanceStatus::Cancelled);
    let child_history = repository::list_events(&fixture.db, &fixture.owner,
        &child_id, 0, 200).unwrap().0;
    assert_eq!(child_history.iter().filter(|event| event.kind == "send_task_pending").count(), 1);
    assert_eq!(child_history.iter().filter(|event| event.kind == "send_task_admitted").count(), 0);
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::admit_pending_send(&reopened, &waiting.token_id,
        chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical).unwrap().is_none());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
}

#[test]
fn embedded_message_boundary_consumes_its_own_child_wait_before_send_admission() {
    use super::messages::test_support::{catch_target, envelope, send};
    use tentaflow_protocol::processes::ProcessSubscriptionStatus;

    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let model = super::runtime::test_support::embedded_model(
        message_bound_send_model(&target.definition_id, &receiver.instance_id), "InnerScope");
    let sender = publish_model(&fixture, &model);
    let pending = start_version(&fixture, &sender);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &pending.instance_id).unwrap();
    let waiting = snapshot.tokens.iter().find(|token|
        token.node_id == "Send_1" && token.status == "waiting").unwrap();
    let boundary = snapshot.subscriptions.iter().find(|subscription|
        subscription.node_id == "SendMessage").unwrap();
    assert_eq!(boundary.status, ProcessSubscriptionStatus::Open);
    assert_eq!(boundary.scope_id, waiting.scope_id);
    let message = envelope(catch_target(&sender, Some(&pending.instance_id),
        Some(&boundary.subscription_id)), serde_json::Value::Null);
    send(&fixture, &message);
    let drained = super::messages::drain_pending(&fixture.db,
        chrono::Utc::now().timestamp_millis());
    drained.completion.unwrap();
    assert_eq!(drained.delivered, 1);
    let history = repository::list_events(&fixture.db, &fixture.owner,
        &pending.instance_id, 0, 200).unwrap().0;
    assert_eq!(history.iter().filter(|event| event.kind == "message_delivered"
        && event.scope_id == waiting.scope_id).count(), 1);
    assert_eq!(history.iter().filter(|event| event.kind == "send_task_admitted").count(), 0);
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::admit_pending_send(&reopened, &waiting.token_id,
        chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical).unwrap().is_none());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
    assert_eq!(repository::get_instance(&reopened, &fixture.owner,
        &pending.instance_id, None).unwrap().status, ProcessInstanceStatus::Completed);
}

#[test]
fn called_child_message_boundary_wins_without_a_child_outbox_or_false_parent_return() {
    use super::messages::test_support::{catch_target, envelope, send};
    use std::collections::BTreeMap;

    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let called = publish_model(&fixture,
        &message_bound_send_model(&target.definition_id, &receiver.instance_id));
    let caller = publish_model(&fixture,
        &super::call_tests::caller(&called, BTreeMap::new()));
    let parent = start_version(&fixture, &caller);
    let child_id = super::call_tests::child_id(&fixture, &parent.instance_id);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    let waiting = snapshot.tokens.iter().find(|token|
        token.node_id == "Send_1" && token.status == "waiting").unwrap();
    let boundary = snapshot.subscriptions.iter().find(|subscription|
        subscription.node_id == "SendMessage").unwrap();
    let message = envelope(catch_target(&called, Some(&child_id),
        Some(&boundary.subscription_id)), serde_json::Value::Null);
    send(&fixture, &message);
    let drained = super::messages::drain_pending(&fixture.db,
        chrono::Utc::now().timestamp_millis());
    drained.completion.unwrap();
    assert_eq!(drained.delivered, 1);
    let history = repository::list_events(&fixture.db, &fixture.owner, &child_id, 0, 200)
        .unwrap().0;
    assert_eq!(history.iter().filter(|event| event.kind == "message_delivered").count(), 1);
    assert_eq!(history.iter().filter(|event| event.kind == "send_task_admitted").count(), 0);
    assert_eq!(repository::get_instance(&fixture.db, &fixture.owner,
        &parent.instance_id, None).unwrap().status, ProcessInstanceStatus::Completed);
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::admit_pending_send(&reopened, &waiting.token_id,
        chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical).unwrap().is_none());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
}

#[test]
fn admitted_called_child_outbox_survives_later_parent_cancellation() {
    use std::collections::BTreeMap;

    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let mut model = timer_bound_send_model(&target.definition_id, &receiver.instance_id);
    model.nodes.insert(2, ProcessNode {
        id: "AfterSendWork".into(), name: "Wait after Send admission".into(),
        kind: ProcessNodeKind::UserTask {
            assignee_user_id: None, output_mapping: BTreeMap::new(),
        }, repeat: None,
        activity_io: None,
    });
    model.sequence_flows.iter_mut().find(|edge| edge.id == "ToEnd")
        .unwrap().target_id = "AfterSendWork".into();
    model.sequence_flows.push(edge("WorkToEnd", "AfterSendWork", "End_1"));
    let called = publish_model(&fixture, &model);
    let caller = publish_model(&fixture,
        &super::call_tests::caller(&called, BTreeMap::new()));
    let parent = start_version(&fixture, &caller);
    let child_id = super::call_tests::child_id(&fixture, &parent.instance_id);
    let child = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    let waiting = child.tokens.iter().find(|token|
        token.node_id == "Send_1" && token.status == "waiting").unwrap();
    repository::admit_pending_send(&fixture.db, &waiting.token_id,
        chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical)
        .unwrap().unwrap();
    let history = repository::list_events(&fixture.db, &fixture.owner, &child_id, 0, 200)
        .unwrap().0;
    let admitted = history.iter().find(|event| event.kind == "send_task_admitted").unwrap();
    let message_id = admitted.data["message_id"].as_str().unwrap().to_owned();
    let parent_before = repository::get_instance(&fixture.db, &fixture.owner,
        &parent.instance_id, None).unwrap();
    assert_eq!(parent_before.status, ProcessInstanceStatus::Waiting);
    repository::cancel_instance(&fixture.db, &fixture.owner,
        &super::runtime::test_support::stamp("cancel parent after child Send admission"),
        &parent.instance_id, parent_before.revision).unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert_eq!(repository::get_message(&reopened, &fixture.owner,
        &fixture.owner.user_id, &message_id).unwrap().message.status,
        tentaflow_protocol::processes::ProcessMessageStatus::Pending);
    let child_after = repository::list_events(&reopened, &fixture.owner, &child_id, 0, 200)
        .unwrap().0;
    assert_eq!(child_after.iter().filter(|event| event.kind == "send_task_admitted").count(), 1);
    assert!(repository::admit_pending_send(&reopened, &waiting.token_id,
        chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical).unwrap().is_none());
    let delivered = super::messages::drain_pending(&reopened,
        chrono::Utc::now().timestamp_millis());
    delivered.completion.unwrap();
    assert_eq!(delivered.delivered, 1);
    assert_eq!(repository::get_message(&reopened, &fixture.owner,
        &fixture.owner.user_id, &message_id).unwrap().message.status,
        tentaflow_protocol::processes::ProcessMessageStatus::Delivered);
    assert_eq!(repository::get_instance(&reopened, &fixture.owner,
        &receiver.instance_id, None).unwrap().status, ProcessInstanceStatus::Completed);
}

#[test]
fn injected_closed_parent_with_live_child_send_is_an_invariant_error() {
    use std::collections::BTreeMap;

    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let called = publish_model(&fixture,
        &timer_bound_send_model(&target.definition_id, &receiver.instance_id));
    let caller = publish_model(&fixture,
        &super::call_tests::caller(&called, BTreeMap::new()));
    let parent = start_version(&fixture, &caller);
    let child_id = super::call_tests::child_id(&fixture, &parent.instance_id);
    let child = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    let waiting = child.tokens.iter().find(|token|
        token.node_id == "Send_1" && token.status == "waiting").unwrap();
    let changed = fixture.db.write().unwrap().execute(
        "UPDATE bpmn_calls SET status='cancelled' WHERE parent_instance_id=?1 AND child_instance_id=?2 AND status='waiting'",
        rusqlite::params![parent.instance_id, child_id]).unwrap();
    assert_eq!(changed, 1, "the injected corruption must target one actual Call");
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let error = repository::admit_pending_send(&fixture.db, &waiting.token_id,
        chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical).unwrap_err();
    assert!(format!("{error:#}").contains("historical called process is not factually terminal")
        || format!("{error:#}").contains("closed parent Call retains a live child Send wait"),
        "inconsistent child closure was misclassified: {error:#}");
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let error = repository::admit_pending_send(&reopened, &waiting.token_id,
        chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical).unwrap_err();
    assert!(!format!("{error:#}").contains("SEND_ADMISSION_AUTHORITY_DENIED"));
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
}

#[test]
fn embedded_noninterrupting_message_updates_local_send_variables_before_admission() {
    use super::messages::test_support::{catch_target, envelope, send};
    use serde_json::json;

    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let mut model = super::runtime::test_support::embedded_model(
        message_bound_send_model(&target.definition_id, &receiver.instance_id), "InnerScope");
    model.variables.insert("case".into(), json!("root"));
    let ProcessNodeKind::SubProcess { body, .. } = &mut model.nodes.iter_mut()
        .find(|node| node.id == "InnerScope").unwrap().kind else { unreachable!() };
    body.variables.insert("case".into(), json!("before"));
    let ProcessNodeKind::SendTask { correlation_expression, .. } = &mut body.nodes.iter_mut()
        .find(|node| node.id == "Send_1").unwrap().kind else { unreachable!() };
    *correlation_expression = "vars.case".into();
    let ProcessNodeKind::BoundaryMessage { cancel_activity, output_mapping, .. } =
        &mut body.nodes.iter_mut().find(|node| node.id == "SendMessage").unwrap().kind
        else { unreachable!() };
    *cancel_activity = false;
    output_mapping.insert("case".into(), "outputs".into());
    let source = publish_model(&fixture, &model);
    let pending = start_version(&fixture, &source);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &pending.instance_id).unwrap();
    let waiting = snapshot.tokens.iter().find(|token|
        token.node_id == "Send_1" && token.status == "waiting").unwrap();
    let boundary = snapshot.subscriptions.iter().find(|subscription|
        subscription.node_id == "SendMessage").unwrap();
    let message = envelope(catch_target(&source, Some(&pending.instance_id),
        Some(&boundary.subscription_id)), json!("case-1"));
    send(&fixture, &message);
    let drained = super::messages::drain_pending(&fixture.db,
        chrono::Utc::now().timestamp_millis());
    drained.completion.unwrap();
    assert_eq!(drained.delivered, 1);
    let after_boundary = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &pending.instance_id).unwrap();
    assert_eq!(after_boundary.instance.variables["case"], "root");
    assert_eq!(after_boundary.scope_variables.get(&waiting.scope_id).unwrap()["case"], "case-1");
    assert!(after_boundary.tokens.iter().any(|token|
        token.token_id == waiting.token_id && token.status == "waiting"));
    repository::admit_pending_send(&fixture.db, &waiting.token_id,
        chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical)
        .unwrap().unwrap();
    let history = repository::list_events(&fixture.db, &fixture.owner,
        &pending.instance_id, 0, 200).unwrap().0;
    let delivered = history.iter().position(|event| event.kind == "message_delivered"
        && event.scope_id == waiting.scope_id).unwrap();
    let admitted = history.iter().position(|event| event.kind == "send_task_admitted"
        && event.scope_id == waiting.scope_id).unwrap();
    assert!(delivered < admitted);
    assert_eq!(history[admitted].data["correlation_key"], "case-1");
}

#[test]
fn embedded_child_terminate_after_send_keeps_its_committed_outbox() {
    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let mut model = super::runtime::test_support::embedded_model(
        timer_bound_send_model(&target.definition_id, &receiver.instance_id), "InnerScope");
    let ProcessNodeKind::SubProcess { body, .. } = &mut model.nodes.iter_mut()
        .find(|node| node.id == "InnerScope").unwrap().kind else { unreachable!() };
    body.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::TerminateEnd;
    let source = publish_model(&fixture, &model);
    let parent = start_version(&fixture, &source);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &parent.instance_id).unwrap();
    let waiting = snapshot.tokens.iter().find(|token|
        token.node_id == "Send_1" && token.status == "waiting").unwrap();
    let committed = repository::admit_pending_send(&fixture.db, &waiting.token_id,
        chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical)
        .unwrap().unwrap();
    assert_eq!(committed.instance.status, ProcessInstanceStatus::Completed);
    let history = repository::list_events(&fixture.db, &fixture.owner,
        &parent.instance_id, 0, 200).unwrap().0;
    let admitted = history.iter().find(|event| event.kind == "send_task_admitted").unwrap();
    let message_id = admitted.data["message_id"].as_str().unwrap();
    assert_eq!(history.iter().filter(|event| event.kind == "terminate_end_reached"
        && event.scope_id == admitted.scope_id).count(), 1);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert_eq!(repository::get_message(&reopened, &fixture.owner,
        &fixture.owner.user_id, message_id).unwrap().message.status,
        tentaflow_protocol::processes::ProcessMessageStatus::Pending);
    assert!(repository::admit_pending_send(&reopened, &waiting.token_id,
        chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical).unwrap().is_none());
    let delivered = super::messages::drain_pending(&reopened,
        chrono::Utc::now().timestamp_millis());
    delivered.completion.unwrap();
    assert_eq!(delivered.delivered, 1);
    assert_eq!(repository::get_message(&reopened, &fixture.owner,
        &fixture.owner.user_id, message_id).unwrap().message.status,
        tentaflow_protocol::processes::ProcessMessageStatus::Delivered);
    assert_eq!(repository::get_instance(&reopened, &fixture.owner,
        &receiver.instance_id, None).unwrap().status, ProcessInstanceStatus::Completed);
}

#[test]
fn called_child_error_after_send_keeps_its_outbox_and_parent_wait_incident() {
    use std::collections::BTreeMap;
    use tentaflow_protocol::processes::ProcessErrorDeclaration;

    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let mut model = timer_bound_send_model(&target.definition_id, &receiver.instance_id);
    model.errors.push(ProcessErrorDeclaration {
        error_id: "Rejected".into(), name: "Evidence rejected".into(),
        error_code: "REJECTED".into(),
    });
    model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::ErrorEnd { error_ref: "Rejected".into() };
    let called = publish_model(&fixture, &model);
    let caller = publish_model(&fixture,
        &super::call_tests::caller(&called, BTreeMap::new()));
    let parent = start_version(&fixture, &caller);
    let child_id = super::call_tests::child_id(&fixture, &parent.instance_id);
    let child = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    let waiting = child.tokens.iter().find(|token|
        token.node_id == "Send_1" && token.status == "waiting").unwrap();
    let actual = repository::admit_pending_send(&fixture.db, &waiting.token_id,
        chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical)
        .unwrap().unwrap();
    assert_eq!(actual.instance.status, ProcessInstanceStatus::Error);
    let parent_after = repository::get_instance(&fixture.db, &fixture.owner,
        &parent.instance_id, None).unwrap();
    assert_eq!(parent_after.status, ProcessInstanceStatus::Incident);
    assert_eq!(parent_after.incidents[0].code, "CALL_CHILD_ERROR");
    let history = repository::list_events(&fixture.db, &fixture.owner, &child_id, 0, 200)
        .unwrap().0;
    let admitted = history.iter().find(|event| event.kind == "send_task_admitted").unwrap();
    let message_id = admitted.data["message_id"].as_str().unwrap();
    assert_eq!(history.iter().filter(|event| event.kind == "error_end_reached").count(), 1);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert_eq!(repository::get_message(&reopened, &fixture.owner,
        &fixture.owner.user_id, message_id).unwrap().message.status,
        tentaflow_protocol::processes::ProcessMessageStatus::Pending);
    assert!(repository::admit_pending_send(&reopened, &waiting.token_id,
        chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical).unwrap().is_none());
}

#[test]
fn called_child_send_admission_closes_a_preexisting_message_boundary_candidate() {
    use super::messages::test_support::{catch_target, envelope, send};
    use std::collections::BTreeMap;

    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let called = publish_model(&fixture,
        &message_bound_send_model(&target.definition_id, &receiver.instance_id));
    let caller = publish_model(&fixture,
        &super::call_tests::caller(&called, BTreeMap::new()));
    let parent = start_version(&fixture, &caller);
    let child_id = super::call_tests::child_id(&fixture, &parent.instance_id);
    let child = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    let waiting = child.tokens.iter().find(|token|
        token.node_id == "Send_1" && token.status == "waiting").unwrap();
    let boundary = child.subscriptions.iter().find(|subscription|
        subscription.node_id == "SendMessage").unwrap();
    let candidate = envelope(catch_target(&called, Some(&child_id),
        Some(&boundary.subscription_id)), serde_json::Value::Null);
    send(&fixture, &candidate);
    repository::admit_pending_send(&fixture.db, &waiting.token_id,
        chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical)
        .unwrap().unwrap();
    let drained = super::messages::drain_pending(&fixture.db,
        chrono::Utc::now().timestamp_millis());
    drained.completion.unwrap();
    let stale = repository::get_message(&fixture.db, &fixture.owner,
        &fixture.owner.user_id, &candidate.message_id).unwrap();
    assert_eq!(stale.message.status,
        tentaflow_protocol::processes::ProcessMessageStatus::Cancelled);
    assert_eq!(stale.message.last_reason.as_deref(), Some("activation_closed"));
    let history = repository::list_events(&fixture.db, &fixture.owner, &child_id, 0, 200)
        .unwrap().0;
    let admitted = history.iter().find(|event| event.kind == "send_task_admitted").unwrap();
    let admitted_id = admitted.data["message_id"].as_str().unwrap();
    assert_ne!(admitted_id, candidate.message_id);
    assert_eq!(history.iter().filter(|event| event.kind == "message_delivered"
        && event.data["message_id"].as_str() == Some(candidate.message_id.as_str())).count(), 0);
    assert_eq!(history.iter().filter(|event| event.kind == "send_task_admitted").count(), 1);
    assert_eq!(repository::get_instance(&fixture.db, &fixture.owner,
        &parent.instance_id, None).unwrap().status, ProcessInstanceStatus::Completed);
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::admit_pending_send(&reopened, &waiting.token_id,
        chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical).unwrap().is_none());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
}

pub(super) fn called_embedded_send_model(
    target_definition_id: &str,
    target_instance_id: &str,
    message_boundary: bool,
) -> tentaflow_protocol::processes::ProcessModel {
    let inner = if message_boundary {
        message_bound_send_model(target_definition_id, target_instance_id)
    } else {
        timer_bound_send_model(target_definition_id, target_instance_id)
    };
    super::runtime::test_support::embedded_model(inner, "InnerScope")
}

#[test]
fn called_embedded_send_admits_from_pinned_local_scope_after_v2_publication() {
    use serde_json::json;
    use std::collections::BTreeMap;

    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let mut child_model = called_embedded_send_model(
        &target.definition_id, &receiver.instance_id, false);
    child_model.variables.insert("payload_value".into(), json!("outer-child-root"));
    let ProcessNodeKind::SubProcess { body, .. } = &mut child_model.nodes.iter_mut()
        .find(|node| node.id == "InnerScope").unwrap().kind else { unreachable!() };
    body.variables.insert("payload_value".into(), json!("embedded-local"));
    let ProcessNodeKind::SendTask { payload_expression, .. } =
        &mut body.nodes.iter_mut().find(|node| node.id == "Send_1").unwrap().kind
        else { unreachable!() };
    *payload_expression = "vars.payload_value".into();
    let called = publish_model(&fixture, &child_model);
    let caller = publish_model(&fixture,
        &super::call_tests::caller(&called, BTreeMap::new()));
    let parent = start_version(&fixture, &caller);
    assert_eq!(parent.status, ProcessInstanceStatus::Waiting);
    let child_id = super::call_tests::child_id(&fixture, &parent.instance_id);
    let child = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    let waiting = child.tokens.iter().find(|token|
        token.node_id == "Send_1" && token.status == "waiting").unwrap();
    assert_ne!(waiting.scope_id, child_id);
    assert_eq!(child.scope_variables.get(&waiting.scope_id).unwrap()["payload_value"],
        "embedded-local");
    let timer = child.timers.iter().find(|timer| timer.node_id == "SendTimer").unwrap();
    assert_eq!(timer.scope_id.as_deref(), Some(waiting.scope_id.as_str()));
    let original = repository::get_definition(&fixture.db, &fixture.owner,
        &called.definition_id).unwrap().0;
    let mut newer = called.model.clone();
    newer.nodes.iter_mut().find(|node| node.id == "InnerScope").unwrap().name =
        "Later child body name".into();
    let draft = repository::save_definition(&fixture.db, &fixture.owner,
        &super::runtime::test_support::stamp("change nested called version"),
        Some(&called.definition_id), original.draft_revision,
        "Evidence process", "", &newer).unwrap();
    let published_v2 = repository::publish_definition(&fixture.db, &fixture.owner,
        &super::runtime::test_support::stamp("publish nested called version"),
        &called.definition_id, draft.draft_revision, &[], None).unwrap().1;
    assert!(published_v2.version > called.version);
    assert_eq!(child.instance.version, called.version);
    let committed = repository::admit_pending_send(&fixture.db, &waiting.token_id,
        chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical)
        .unwrap().unwrap();
    assert_eq!(committed.instance.status, ProcessInstanceStatus::Completed);
    assert_eq!(repository::get_instance(&fixture.db, &fixture.owner,
        &parent.instance_id, None).unwrap().status, ProcessInstanceStatus::Completed);
    let history = repository::list_events(&fixture.db, &fixture.owner, &child_id, 0, 200)
        .unwrap().0;
    let admitted = history.iter().find(|event| event.kind == "send_task_admitted").unwrap();
    assert_eq!(admitted.scope_id, waiting.scope_id);
    assert_eq!(history.iter().filter(|event| event.kind == "send_task_admitted").count(), 1);
    let message_id = admitted.data["message_id"].as_str().unwrap();
    assert_eq!(repository::get_message(&fixture.db, &fixture.owner,
        &fixture.owner.user_id, message_id).unwrap().payload, Some(json!("embedded-local")));
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::admit_pending_send(&reopened, &waiting.token_id,
        chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical).unwrap().is_none());
    assert_eq!(repository::get_instance(&reopened, &fixture.owner,
        &parent.instance_id, None).unwrap().status, ProcessInstanceStatus::Completed);
}

#[test]
fn called_embedded_timer_and_message_boundaries_each_win_before_send() {
    use super::messages::test_support::{catch_target, envelope, send};
    use std::collections::BTreeMap;

    for message_boundary in [false, true] {
        let fixture = Fixture::new();
        let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
        let receiver = start_version(&fixture, &target);
        let called = publish_model(&fixture,
            &called_embedded_send_model(&target.definition_id,
                &receiver.instance_id, message_boundary));
        let caller = publish_model(&fixture,
            &super::call_tests::caller(&called, BTreeMap::new()));
        let parent = start_version(&fixture, &caller);
        let child_id = super::call_tests::child_id(&fixture, &parent.instance_id);
        let child = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
        let waiting = child.tokens.iter().find(|token|
            token.node_id == "Send_1" && token.status == "waiting").unwrap();
        assert_ne!(waiting.scope_id, child_id);
        if message_boundary {
            let boundary = child.subscriptions.iter().find(|subscription|
                subscription.node_id == "SendMessage").unwrap();
            assert_eq!(boundary.scope_id, waiting.scope_id);
            let message = envelope(catch_target(&called, Some(&child_id),
                Some(&boundary.subscription_id)), serde_json::Value::Null);
            send(&fixture, &message);
            let drained = super::messages::drain_pending(&fixture.db,
                chrono::Utc::now().timestamp_millis());
            drained.completion.unwrap();
            assert_eq!(drained.delivered, 1);
        } else {
            let timer = child.timers.iter().find(|timer| timer.node_id == "SendTimer").unwrap();
            assert_eq!(timer.scope_id.as_deref(), Some(waiting.scope_id.as_str()));
            let drained = super::timers::drain_due(&fixture.db, timer.due_at_ms.unwrap() + 1);
            drained.completion.unwrap();
            assert_eq!(drained.fired, 1);
        }
        let history = repository::list_events(&fixture.db, &fixture.owner, &child_id, 0, 200)
            .unwrap().0;
        assert_eq!(history.iter().filter(|event| event.kind == "send_task_pending"
            && event.scope_id == waiting.scope_id).count(), 1);
        assert_eq!(history.iter().filter(|event| event.kind == "send_task_admitted").count(), 0);
        assert_eq!(repository::get_instance(&fixture.db, &fixture.owner,
            &parent.instance_id, None).unwrap().status, ProcessInstanceStatus::Completed);
        let before = super::signal_proof_tests::all_transition_rows(&fixture);
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        assert!(repository::admit_pending_send(&reopened, &waiting.token_id,
            chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical).unwrap().is_none());
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
    }
}

#[test]
fn called_embedded_admission_first_disarms_both_boundary_kinds_without_retracting_outbox() {
    use super::messages::test_support::{catch_target, envelope, send};
    use std::collections::BTreeMap;

    for message_boundary in [false, true] {
        let fixture = Fixture::new();
        let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
        let receiver = start_version(&fixture, &target);
        let called = publish_model(&fixture, &called_embedded_send_model(
            &target.definition_id, &receiver.instance_id, message_boundary));
        let caller = publish_model(&fixture,
            &super::call_tests::caller(&called, BTreeMap::new()));
        let parent = start_version(&fixture, &caller);
        let child_id = super::call_tests::child_id(&fixture, &parent.instance_id);
        let child = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
        let waiting = child.tokens.iter().find(|token|
            token.node_id == "Send_1" && token.status == "waiting").unwrap();
        assert_ne!(waiting.scope_id, child_id);
        let at_ms = chrono::Utc::now().timestamp_millis();
        let candidate = if message_boundary {
            let boundary = child.subscriptions.iter().find(|subscription|
                subscription.node_id == "SendMessage").unwrap();
            let message = envelope(catch_target(&called, Some(&child_id),
                Some(&boundary.subscription_id)), serde_json::Value::Null);
            send(&fixture, &message);
            Some(message.message_id)
        } else { None };
        let admitted = repository::admit_pending_send(&fixture.db, &waiting.token_id,
            at_ms, ProcessPlanInput::Canonical).unwrap().unwrap();
        assert_eq!(admitted.instance.status, ProcessInstanceStatus::Completed);
        let history = repository::list_events(&fixture.db, &fixture.owner, &child_id, 0, 200)
            .unwrap().0;
        let admission = history.iter().find(|event| event.kind == "send_task_admitted").unwrap();
        assert_eq!(admission.scope_id, waiting.scope_id);
        let outbox_id = admission.data["message_id"].as_str().unwrap().to_owned();
        if let Some(candidate_id) = candidate {
            let drained = super::messages::drain_pending(&fixture.db, at_ms + 1);
            drained.completion.unwrap();
            let stale = repository::get_message(&fixture.db, &fixture.owner,
                &fixture.owner.user_id, &candidate_id).unwrap();
            assert_eq!(stale.message.status,
                tentaflow_protocol::processes::ProcessMessageStatus::Cancelled);
            assert_eq!(stale.message.last_reason.as_deref(), Some("activation_closed"));
            let history = repository::list_events(&fixture.db, &fixture.owner, &child_id, 0, 200)
                .unwrap().0;
            assert_eq!(history.iter().filter(|event| event.kind == "message_delivered"
                && event.data["message_id"].as_str() == Some(candidate_id.as_str())).count(), 0);
        } else {
            let timer = child.timers.iter().find(|timer| timer.node_id == "SendTimer").unwrap();
            let drained = super::timers::drain_due(&fixture.db, timer.due_at_ms.unwrap() + 1);
            drained.completion.unwrap();
            assert_eq!(drained.fired, 0);
        }
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        assert_eq!(repository::get_instance(&reopened, &fixture.owner,
            &parent.instance_id, None).unwrap().status, ProcessInstanceStatus::Completed);
        assert_eq!(repository::get_message(&reopened, &fixture.owner,
            &fixture.owner.user_id, &outbox_id).unwrap().message.source_instance_id.as_deref(),
            Some(child_id.as_str()));
        assert!(repository::admit_pending_send(&reopened, &waiting.token_id,
            at_ms, ProcessPlanInput::Canonical).unwrap().is_none());
    }
}

#[test]
fn called_embedded_noninterrupting_message_maps_child_local_input_before_send() {
    use super::messages::test_support::{catch_target, envelope, send};
    use serde_json::json;
    use std::collections::BTreeMap;

    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let mut model = called_embedded_send_model(&target.definition_id, &receiver.instance_id, true);
    model.variables.insert("case".into(), json!("called-root"));
    let ProcessNodeKind::SubProcess { body, .. } = &mut model.nodes.iter_mut()
        .find(|node| node.id == "InnerScope").unwrap().kind else { unreachable!() };
    body.variables.insert("case".into(), json!("local-before"));
    let ProcessNodeKind::SendTask { correlation_expression, .. } = &mut body.nodes.iter_mut()
        .find(|node| node.id == "Send_1").unwrap().kind else { unreachable!() };
    *correlation_expression = "vars.case".into();
    let ProcessNodeKind::BoundaryMessage { cancel_activity, output_mapping, .. } =
        &mut body.nodes.iter_mut().find(|node| node.id == "SendMessage").unwrap().kind
        else { unreachable!() };
    *cancel_activity = false;
    output_mapping.insert("case".into(), "outputs".into());
    let called = publish_model(&fixture, &model);
    let caller = publish_model(&fixture, &super::call_tests::caller(&called, BTreeMap::new()));
    let parent = start_version(&fixture, &caller);
    let child_id = super::call_tests::child_id(&fixture, &parent.instance_id);
    let child = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    let waiting = child.tokens.iter().find(|token| token.node_id == "Send_1"
        && token.status == "waiting").unwrap();
    let boundary = child.subscriptions.iter().find(|subscription|
        subscription.node_id == "SendMessage").unwrap();
    let message = envelope(catch_target(&called, Some(&child_id),
        Some(&boundary.subscription_id)), json!("local-after"));
    send(&fixture, &message);
    let drained = super::messages::drain_pending(&fixture.db,
        chrono::Utc::now().timestamp_millis());
    drained.completion.unwrap();
    assert_eq!(drained.delivered, 1);
    let after = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    assert_eq!(after.instance.variables["case"], "called-root");
    assert_eq!(after.scope_variables.get(&waiting.scope_id).unwrap()["case"], "local-after");
    assert!(after.tokens.iter().any(|token| token.token_id == waiting.token_id
        && token.status == "waiting"));
    repository::admit_pending_send(&fixture.db, &waiting.token_id,
        chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical).unwrap().unwrap();
    let events = repository::list_events(&fixture.db, &fixture.owner, &child_id, 0, 200)
        .unwrap().0;
    let mapped = events.iter().position(|event| event.kind == "message_delivered"
        && event.scope_id == waiting.scope_id).unwrap();
    let admission = events.iter().position(|event| event.kind == "send_task_admitted"
        && event.scope_id == waiting.scope_id).unwrap();
    assert!(mapped < admission);
    assert_eq!(events[admission].data["correlation_key"], "local-after");
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert_eq!(repository::get_instance(&reopened, &fixture.owner,
        &parent.instance_id, None).unwrap().status, ProcessInstanceStatus::Completed);
}

#[test]
fn called_embedded_parent_cancel_closes_live_grandchild_and_retains_prior_outbox() {
    use std::collections::BTreeMap;

    for after_admission in [false, true] {
        let fixture = Fixture::new();
        let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
        let receiver = start_version(&fixture, &target);
        let mut model = called_embedded_send_model(
            &target.definition_id, &receiver.instance_id, false);
        if after_admission {
            let ProcessNodeKind::SubProcess { body, .. } = &mut model.nodes.iter_mut()
                .find(|node| node.id == "InnerScope").unwrap().kind else { unreachable!() };
            body.nodes.push(ProcessNode {
                id: "AfterSendWork".into(), name: "Retain child after Send".into(),
                kind: ProcessNodeKind::UserTask { assignee_user_id: None,
                    output_mapping: BTreeMap::new() }, repeat: None,
                    activity_io: None,
                });
            body.sequence_flows.iter_mut().find(|flow| flow.id == "ToEnd")
                .unwrap().target_id = "AfterSendWork".into();
            body.sequence_flows.push(edge("WorkEnd", "AfterSendWork", "End_1"));
        }
        let called = publish_model(&fixture, &model);
        let caller = publish_model(&fixture,
            &super::call_tests::caller(&called, BTreeMap::new()));
        let parent = start_version(&fixture, &caller);
        let child_id = super::call_tests::child_id(&fixture, &parent.instance_id);
        let child = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
        let waiting = child.tokens.iter().find(|token|
            token.node_id == "Send_1" && token.status == "waiting").unwrap();
        let waiting_id = waiting.token_id.clone();
        let scope_id = waiting.scope_id.clone();
        let admitted_id = if after_admission {
            repository::admit_pending_send(&fixture.db, &waiting_id,
                chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical)
                .unwrap().unwrap();
            let history = repository::list_events(&fixture.db, &fixture.owner, &child_id, 0, 200)
                .unwrap().0;
            let fact = history.iter().find(|event| event.kind == "send_task_admitted").unwrap();
            assert_eq!(fact.scope_id, scope_id);
            Some(fact.data["message_id"].as_str().unwrap().to_owned())
        } else { None };
        let parent_before = repository::get_instance(&fixture.db, &fixture.owner,
            &parent.instance_id, None).unwrap();
        assert_eq!(parent_before.status, ProcessInstanceStatus::Waiting);
        repository::cancel_instance(&fixture.db, &fixture.owner,
            &super::runtime::test_support::stamp("cancel parent with nested child"),
            &parent.instance_id, parent_before.revision).unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let child_after = repository::runtime_snapshot(&reopened, &fixture.owner, &child_id).unwrap();
        assert_eq!(child_after.instance.status, ProcessInstanceStatus::Cancelled);
        assert!(!child_after.tokens.iter().any(|token| token.token_id == waiting_id
            && token.status == "waiting"));
        assert!(repository::admit_pending_send(&reopened, &waiting_id,
            chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical).unwrap().is_none());
        if let Some(message_id) = admitted_id {
            assert_eq!(repository::get_message(&reopened, &fixture.owner,
                &fixture.owner.user_id, &message_id).unwrap().message.status,
                tentaflow_protocol::processes::ProcessMessageStatus::Pending);
        } else {
            let events = repository::list_events(&reopened, &fixture.owner, &child_id, 0, 200)
                .unwrap().0;
            assert!(events.iter().all(|event| event.kind != "send_task_admitted"));
        }
    }
}

#[test]
fn called_embedded_child_terminal_facts_preserve_an_already_admitted_outbox() {
    use std::collections::BTreeMap;
    use tentaflow_protocol::processes::ProcessErrorDeclaration;

    for error_end in [false, true] {
        let fixture = Fixture::new();
        let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
        let receiver = start_version(&fixture, &target);
        let mut model = called_embedded_send_model(
            &target.definition_id, &receiver.instance_id, false);
        let ProcessNodeKind::SubProcess { body, .. } = &mut model.nodes.iter_mut()
            .find(|node| node.id == "InnerScope").unwrap().kind else { unreachable!() };
        body.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind = if error_end {
            ProcessNodeKind::ErrorEnd { error_ref: "Rejected".into() }
        } else { ProcessNodeKind::TerminateEnd };
        if error_end {
            model.errors.push(ProcessErrorDeclaration {
                error_id: "Rejected".into(), name: "Rejected".into(),
                error_code: "REJECTED".into(),
            });
        }
        let called = publish_model(&fixture, &model);
        let caller = publish_model(&fixture,
            &super::call_tests::caller(&called, BTreeMap::new()));
        let parent = start_version(&fixture, &caller);
        let child_id = super::call_tests::child_id(&fixture, &parent.instance_id);
        let child = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
        let waiting = child.tokens.iter().find(|token| token.node_id == "Send_1"
            && token.status == "waiting").unwrap();
        let committed = repository::admit_pending_send(&fixture.db, &waiting.token_id,
            chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical)
            .unwrap().unwrap();
        let events = repository::list_events(&fixture.db, &fixture.owner, &child_id, 0, 200)
            .unwrap().0;
        let admitted = events.iter().find(|event| event.kind == "send_task_admitted").unwrap();
        assert_eq!(admitted.scope_id, waiting.scope_id);
        let terminal_kind = if error_end { "error_end_reached" } else { "terminate_end_reached" };
        assert_eq!(events.iter().filter(|event| event.kind == terminal_kind
            && event.scope_id == waiting.scope_id).count(), 1);
        if error_end {
            assert_eq!(committed.instance.status, ProcessInstanceStatus::Error);
            let parent_after = repository::get_instance(&fixture.db, &fixture.owner,
                &parent.instance_id, None).unwrap();
            assert_eq!(parent_after.status, ProcessInstanceStatus::Incident);
            assert_eq!(parent_after.incidents[0].code, "CALL_CHILD_ERROR");
        } else {
            assert_eq!(committed.instance.status, ProcessInstanceStatus::Completed);
        }
        let outbox_id = admitted.data["message_id"].as_str().unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        assert_eq!(repository::get_message(&reopened, &fixture.owner,
            &fixture.owner.user_id, outbox_id).unwrap().message.status,
            tentaflow_protocol::processes::ProcessMessageStatus::Pending);
        assert!(repository::admit_pending_send(&reopened, &waiting.token_id,
            chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical).unwrap().is_none());
    }
}

#[test]
fn injected_closed_parent_call_with_live_nested_send_is_not_a_stale_noop() {
    use std::collections::BTreeMap;

    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let called = publish_model(&fixture, &called_embedded_send_model(
        &target.definition_id, &receiver.instance_id, false));
    let caller = publish_model(&fixture,
        &super::call_tests::caller(&called, BTreeMap::new()));
    let parent = start_version(&fixture, &caller);
    let child_id = super::call_tests::child_id(&fixture, &parent.instance_id);
    let child = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    let waiting = child.tokens.iter().find(|token| token.node_id == "Send_1"
        && token.status == "waiting").unwrap();
    assert_ne!(waiting.scope_id, child_id);
    let changed = fixture.db.write().unwrap().execute(
        "UPDATE bpmn_calls SET status='cancelled' WHERE parent_instance_id=?1 AND child_instance_id=?2 AND status='waiting'",
        rusqlite::params![parent.instance_id, child_id]).unwrap();
    assert_eq!(changed, 1, "the injected fault must target the factual pinned Call");
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let error = repository::admit_pending_send(&fixture.db, &waiting.token_id,
        chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical).unwrap_err();
    assert!(format!("{error:#}").contains("historical called process is not factually terminal")
        || format!("{error:#}").contains("closed parent Call retains a live child Send wait"),
        "a live nested child was misclassified as stale: {error:#}");
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::admit_pending_send(&reopened, &waiting.token_id,
        chrono::Utc::now().timestamp_millis(), ProcessPlanInput::Canonical).is_err());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
}

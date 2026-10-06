// ============ File: send_receive_tests.rs — File-backed SendTask admission and ReceiveTask delivery ============

use super::messages::{self, test_support::*};
use super::repository::{self, MessageSelection};
use super::runtime::{self, test_support::{edge, embedded_model, human_input, publish_model, stamp, Fixture}};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tentaflow_protocol::processes::{ProcessInstanceStatus, ProcessMessageDeclaration,
    ProcessMessageStatus, ProcessMessageTargetSpec, ProcessNode, ProcessNodeKind,
    ProcessSubscriptionKind, ProcessSubscriptionStatus};

pub(super) fn receive_model() -> tentaflow_protocol::processes::ProcessModel {
    let mut model = receiving_model(false, false);
    let node = model.nodes.iter_mut().find(|node| node.id == "Catch_1").unwrap();
    node.kind = ProcessNodeKind::ReceiveTask {
        message_ref: "Message_1".into(),
        correlation_expression: "'case-1'".into(),
        output_mapping: BTreeMap::from([("received".into(), "outputs".into())]),
    };
    model
}

pub(super) fn send_model(target_definition: &str, target_instance: &str) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = super::model::starter_model();
    model.messages.push(ProcessMessageDeclaration {
        message_id: "Evidence".into(), name: "EvidenceReady".into(),
    });
    model.nodes.insert(1, ProcessNode { id: "Send_1".into(), name: "Admit evidence".into(),
        kind: ProcessNodeKind::SendTask {
            message_ref: "Evidence".into(),
            target: ProcessMessageTargetSpec::Catch {
                definition_id: target_definition.into(),
                instance_id_expression: Some(serde_json::to_string(target_instance).unwrap()),
                subscription_id_expression: None,
            },
            correlation_expression: "'case-1'".into(),
            payload_expression: "{'value': 42}".into(), ttl_seconds: 120,
        }, repeat: None });
    model.sequence_flows = vec![edge("ToSend", "Start_1", "Send_1"),
        edge("ToEnd", "Send_1", "End_1")];
    model
}

#[test]
fn send_admission_is_durable_before_receive_delivery_and_reopens_with_distinct_facts() {
    let fixture = Fixture::new();
    let receiving = publish_model(&fixture, &receive_model());
    let receiver = start_version(&fixture, &receiving);
    assert_eq!(receiver.subscriptions.len(), 1);
    assert_eq!(receiver.subscriptions[0].kind, ProcessSubscriptionKind::ReceiveTask);
    assert_eq!(receiver.subscriptions[0].status, ProcessSubscriptionStatus::Open);
    let source = publish_model(&fixture, &send_model(&receiving.definition_id, &receiver.instance_id));
    let sender = start_version(&fixture, &source);
    assert_eq!(sender.status, ProcessInstanceStatus::Completed);
    let source_events = repository::list_events(&fixture.db, &fixture.owner,
        &sender.instance_id, 0, 200).unwrap().0;
    let admitted = source_events.iter().find(|event| event.kind == "send_task_admitted").unwrap();
    let message_id = admitted.data["message_id"].as_str().unwrap();
    assert_eq!(source_events.iter().filter(|event| event.kind == "message_queued").count(), 0);
    let pending = repository::get_message(&fixture.db, &fixture.owner,
        &fixture.owner.user_id, message_id).unwrap();
    assert_eq!(pending.message.status, ProcessMessageStatus::Pending);
    assert_eq!(pending.payload, Some(json!({"value":42})));
    let candidate = repository::due_messages(&fixture.db, chrono::Utc::now().timestamp_millis(), 32)
        .unwrap().into_iter().find(|candidate| candidate.key.message_id == message_id).unwrap();
    let MessageSelection::Ready(prepared) = repository::message_snapshot(&fixture.db, &candidate).unwrap()
        else { panic!("the actual ReceiveTask must be selected") };
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = messages::plan_message_delivery(&prepared, at_ms, None).unwrap();
    let delivered_index = plan.events.iter().position(|event| event.kind == "message_delivered").unwrap();
    let completed_index = plan.events.iter().position(|event| event.kind == "receive_task_completed").unwrap();
    assert!(delivered_index < completed_index);
    let result = repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().unwrap();
    assert_eq!(result.transition.instance.status, ProcessInstanceStatus::Completed);
    let reopened = repository::get_instance(&fixture.db, &fixture.owner,
        &receiver.instance_id, None).unwrap();
    assert_eq!(reopened.status, ProcessInstanceStatus::Completed);
    assert_eq!(reopened.variables["received"], json!({"value":42}));
    let events = repository::list_events(&fixture.db, &fixture.owner,
        &receiver.instance_id, 0, 200).unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "receive_task_opened").count(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "message_armed").count(), 0);
    assert_eq!(events.iter().filter(|event| event.kind == "message_delivered").count(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "receive_task_completed").count(), 1);
    assert_eq!(repository::get_message(&fixture.db, &fixture.owner,
        &fixture.owner.user_id, message_id).unwrap().message.status, ProcessMessageStatus::Delivered);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert_eq!(repository::get_instance(&reopened, &fixture.owner,
        &receiver.instance_id, None).unwrap().status, ProcessInstanceStatus::Completed);
    assert_eq!(repository::get_message(&reopened, &fixture.owner,
        &fixture.owner.user_id, message_id).unwrap().message.status, ProcessMessageStatus::Delivered);
    assert!(repository::deliver_message(&reopened, &prepared, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().is_none());
}

#[test]
fn two_receive_waits_keep_a_broad_message_ambiguous_and_exact_delivery_single_consumer() {
    let fixture = Fixture::new();
    let version = publish_model(&fixture, &receive_model());
    let first = start_version(&fixture, &version);
    let second = start_version(&fixture, &version);
    let broad = envelope(catch_target(&version, None, None), Value::Null);
    send(&fixture, &broad);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap()
        .into_iter().find(|item| item.key.message_id == broad.message_id).unwrap();
    assert!(matches!(repository::message_snapshot(&fixture.db, &candidate).unwrap(),
        MessageSelection::Ambiguous));
    let exact = envelope(catch_target(&version, Some(&first.instance_id),
        Some(&first.subscriptions[0].subscription_id)), json!({"value":1}));
    send(&fixture, &exact);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap()
        .into_iter().find(|item| item.key.message_id == exact.message_id).unwrap();
    let MessageSelection::Ready(prepared) = repository::message_snapshot(&fixture.db, &candidate).unwrap()
        else { panic!("the exact subscription must be selected") };
    let plan = messages::plan_message_delivery(&prepared, at_ms, None).unwrap();
    repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().unwrap();
    assert_eq!(repository::get_instance(&fixture.db, &fixture.owner,
        &first.instance_id, None).unwrap().status, ProcessInstanceStatus::Completed);
    assert_eq!(repository::get_instance(&fixture.db, &fixture.owner,
        &second.instance_id, None).unwrap().subscriptions[0].status,
        ProcessSubscriptionStatus::Open);
    assert!(repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().is_none());
}

#[test]
fn cancelling_receive_wait_prevents_late_completion_and_preserves_pending_message_identity() {
    let fixture = Fixture::new();
    let version = publish_model(&fixture, &receive_model());
    let receiver = start_version(&fixture, &version);
    let message = envelope(catch_target(&version, Some(&receiver.instance_id),
        Some(&receiver.subscriptions[0].subscription_id)), json!({"value":7}));
    send(&fixture, &message);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap().into_iter()
        .find(|item| item.key.message_id == message.message_id).unwrap();
    repository::cancel_instance(&fixture.db, &fixture.owner,
        &stamp("cancel receive wait"), &receiver.instance_id, receiver.revision).unwrap();
    assert!(repository::message_snapshot(&fixture.db, &candidate).is_err());
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let instance = repository::get_instance(&reopened, &fixture.owner,
        &receiver.instance_id, None).unwrap();
    assert_eq!(instance.status, ProcessInstanceStatus::Cancelled);
    assert_eq!(instance.subscriptions[0].status, ProcessSubscriptionStatus::Cancelled);
    let events = repository::list_events(&reopened, &fixture.owner,
        &receiver.instance_id, 0, 200).unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "receive_task_opened").count(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "receive_task_completed").count(), 0);
    assert_eq!(events.iter().filter(|event| event.kind == "message_delivered").count(), 0);
}

#[test]
fn broad_receive_selection_skips_open_subscriptions_in_dead_scopes_across_cursor_pages() {
    for (recipient_count, stale_count) in [(3_usize, 2_usize), (4, 2), (66, 65)] {
        let fixture = Fixture::new();
        let version = publish_model(&fixture, &embedded_model(receive_model(), "ReceiveScope"));
        let mut recipients = (0..recipient_count)
            .map(|_| {
                let instance = start_version(&fixture, &version);
                assert_eq!(instance.status, ProcessInstanceStatus::Waiting);
                let subscription = instance.subscriptions.iter()
                    .find(|item| item.kind == ProcessSubscriptionKind::ReceiveTask).unwrap();
                let scope = instance.scopes.iter()
                    .find(|item| item.scope_id == subscription.scope_id).unwrap();
                assert_eq!(scope.status, ProcessInstanceStatus::Waiting);
                assert!(scope.parent_scope_id.is_some());
                (instance.instance_id, subscription.subscription_id.clone(), scope.scope_id.clone())
            })
            .collect::<Vec<_>>();
        recipients.sort_by(|left, right| left.1.cmp(&right.1));
        for (instance_id, subscription_id, scope_id) in recipients.iter().take(stale_count) {
            let conn = fixture.db.write().unwrap();
            assert_eq!(conn.execute(
                "UPDATE bpmn_scopes SET status='cancelled' WHERE instance_id=?1 AND scope_id=?2 AND parent_scope_id IS NOT NULL AND status='waiting'",
                rusqlite::params![instance_id, scope_id],
            ).unwrap(), 1);
            let (subscription_status, token_status): (String, String) = conn.query_row(
                "SELECT s.status,t.status FROM bpmn_event_subscriptions s JOIN bpmn_tokens t ON t.token_id=s.token_id WHERE s.subscription_id=?1 AND s.instance_id=?2",
                rusqlite::params![subscription_id, instance_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).unwrap();
            assert_eq!((subscription_status.as_str(), token_status.as_str()), ("open", "waiting"));
        }
        let broad = envelope(catch_target(&version, None, None), json!({"case":"selected"}));
        send(&fixture, &broad);
        let at_ms = chrono::Utc::now().timestamp_millis();
        let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap()
            .into_iter().find(|item| item.key.message_id == broad.message_id).unwrap();
        if recipient_count == 4 {
            assert!(matches!(repository::message_snapshot(&fixture.db, &candidate).unwrap(),
                MessageSelection::Ambiguous));
            assert_eq!(current(&fixture, &broad).message.status, ProcessMessageStatus::Pending);
            let exact = envelope(catch_target(&version, Some(&recipients[2].0),
                Some(&recipients[2].1)), json!({"case":"exact"}));
            send(&fixture, &exact);
            let exact_at_ms = chrono::Utc::now().timestamp_millis();
            let exact_candidate = repository::due_messages(&fixture.db, exact_at_ms, 32).unwrap()
                .into_iter().find(|item| item.key.message_id == exact.message_id).unwrap();
            let MessageSelection::Ready(prepared) = repository::message_snapshot(&fixture.db,
                &exact_candidate).unwrap() else { panic!("the later exact ReceiveTask is live") };
            let plan = messages::plan_message_delivery(&prepared, exact_at_ms, None).unwrap();
            repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&plan), exact_at_ms).unwrap().unwrap();
            assert_eq!(repository::get_instance(&fixture.db, &fixture.owner,
                &recipients[3].0, None).unwrap().status, ProcessInstanceStatus::Waiting);
        } else {
            let MessageSelection::Ready(prepared) = repository::message_snapshot(&fixture.db,
                &candidate).unwrap() else { panic!("the later live ReceiveTask must be selected") };
            let plan = messages::plan_message_delivery(&prepared, at_ms, None).unwrap();
            let completed = repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&plan), at_ms)
                .unwrap().unwrap();
            assert_eq!(completed.transition.instance.instance_id.as_str(),
                recipients.last().unwrap().0.as_str());
            assert_eq!(completed.transition.instance.status, ProcessInstanceStatus::Completed);
            let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
            assert_eq!(repository::get_instance(&reopened, &fixture.owner,
                &recipients.last().unwrap().0, None).unwrap().status,
                ProcessInstanceStatus::Completed);
            assert_eq!(repository::get_message(&reopened, &fixture.owner, &fixture.owner.user_id,
                &broad.message_id).unwrap().message.status, ProcessMessageStatus::Delivered);
        }
    }
}

#[test]
fn receive_task_uses_accepted_source_time_variables_for_key_and_output_mapping() {
    let fixture = Fixture::new();
    let mut model = receiving_model(false, true);
    model.variables.insert("case_key".into(), json!("before"));
    model.variables.insert("received".into(), Value::Null);
    let gate = model.nodes.iter_mut().find(|node| node.id == "Gate_1").unwrap();
    gate.kind = ProcessNodeKind::UserTask {
        assignee_user_id: Some(fixture.owner.user_id.clone()),
        output_mapping: BTreeMap::from([("case_key".into(), "outputs.case_key".into())]),
    };
    model.nodes.iter_mut().find(|node| node.id == "Catch_1").unwrap().kind =
        ProcessNodeKind::ReceiveTask {
            message_ref: "Message_1".into(),
            correlation_expression: "vars.case_key".into(),
            output_mapping: BTreeMap::from([("received".into(), "outputs".into())]),
        };
    let version = publish_model(&fixture, &model);
    let before = start_version(&fixture, &version);
    assert_eq!(before.status, ProcessInstanceStatus::Waiting);
    assert!(before.subscriptions.is_empty());
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &before.instance_id).unwrap();
    let task = snapshot.user_tasks.iter().find(|task| task.node_id == "Gate_1").unwrap();
    let command = stamp("accept source-time correlation key");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let outputs = json!({"case_key":"after"});
    let plan = runtime::plan_user_completion(&snapshot, &task.user_task_id, &outputs,
        None, at_ms, human_input(&snapshot, &task.user_task_id, &command), None).unwrap();
    let waiting = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &before.instance_id, &task.user_task_id, before.revision, &outputs, None, repository::ProcessPlanInput::Supplied(&plan), at_ms)
        .unwrap().instance;
    assert_eq!(waiting.status, ProcessInstanceStatus::Waiting);
    assert_eq!(waiting.variables["case_key"], "after");
    assert_eq!(waiting.subscriptions.len(), 1);
    assert_eq!(waiting.subscriptions[0].correlation_key.as_deref(), Some("after"));

    let mut wrong = envelope(catch_target(&version, None, None), json!({"source":"wrong"}));
    wrong.correlation_key = "before".into();
    send(&fixture, &wrong);
    let wrong_at_ms = chrono::Utc::now().timestamp_millis();
    let wrong_candidate = repository::due_messages(&fixture.db, wrong_at_ms, 32).unwrap()
        .into_iter().find(|item| item.key.message_id == wrong.message_id).unwrap();
    assert!(matches!(repository::message_snapshot(&fixture.db, &wrong_candidate).unwrap(),
        MessageSelection::NoMatch));

    let mut actual = envelope(catch_target(&version, Some(&waiting.instance_id),
        Some(&waiting.subscriptions[0].subscription_id)), json!({"source":"accepted"}));
    actual.correlation_key = "after".into();
    send(&fixture, &actual);
    let deliver_at_ms = chrono::Utc::now().timestamp_millis();
    let candidate = repository::due_messages(&fixture.db, deliver_at_ms, 32).unwrap()
        .into_iter().find(|item| item.key.message_id == actual.message_id).unwrap();
    let MessageSelection::Ready(prepared) = repository::message_snapshot(&fixture.db,
        &candidate).unwrap() else { panic!("the accepted key must select its pinned ReceiveTask") };
    let delivery = messages::plan_message_delivery(&prepared, deliver_at_ms, None).unwrap();
    let completed = repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&delivery), deliver_at_ms)
        .unwrap().unwrap().transition.instance;
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert_eq!(completed.variables["case_key"], "after");
    assert_eq!(completed.variables["received"], json!({"source":"accepted"}));
    assert_eq!(current(&fixture, &wrong).message.status, ProcessMessageStatus::Pending);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::get_instance(&reopened, &fixture.owner,
        &before.instance_id, None).unwrap();
    assert_eq!(persisted.variables, completed.variables);
    assert_eq!(repository::get_message(&reopened, &fixture.owner, &fixture.owner.user_id,
        &actual.message_id).unwrap().message.status, ProcessMessageStatus::Delivered);
}

#[test]
fn embedded_receive_completes_its_scope_and_maps_the_actual_delivery_once() {
    let fixture = Fixture::new();
    let mut child = receive_model();
    child.variables.insert("received".into(), Value::Null);
    let mut model = embedded_model(child, "ReceiveScope");
    model.variables.insert("received".into(), Value::Null);
    let scope = model.nodes.iter_mut().find(|node| node.id == "ReceiveScope").unwrap();
    let ProcessNodeKind::SubProcess { output_mapping, .. } = &mut scope.kind else {
        panic!("the actual receiving scope must be embedded")
    };
    output_mapping.insert("received".into(), "outputs.received".into());
    let version = publish_model(&fixture, &model);
    let waiting = start_version(&fixture, &version);
    assert_eq!(waiting.status, ProcessInstanceStatus::Waiting);
    let subscription = waiting.subscriptions.iter()
        .find(|item| item.kind == ProcessSubscriptionKind::ReceiveTask).unwrap();
    let child_scope_id = subscription.scope_id.clone();
    assert_ne!(child_scope_id, waiting.instance_id);
    let message = envelope(catch_target(&version, Some(&waiting.instance_id),
        Some(&subscription.subscription_id)), json!({"embedded":42}));
    send(&fixture, &message);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap()
        .into_iter().find(|item| item.key.message_id == message.message_id).unwrap();
    let MessageSelection::Ready(prepared) = repository::message_snapshot(&fixture.db,
        &candidate).unwrap() else { panic!("the embedded ReceiveTask must remain live") };
    let plan = messages::plan_message_delivery(&prepared, at_ms, None).unwrap();
    let completed = repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&plan), at_ms)
        .unwrap().unwrap().transition.instance;
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert_eq!(completed.variables["received"], json!({"embedded":42}));
    assert_eq!(repository::get_scope(&fixture.db, &fixture.owner,
        &waiting.instance_id, &child_scope_id).unwrap().1["received"], json!({"embedded":42}));
    let events = repository::list_events(&fixture.db, &fixture.owner,
        &waiting.instance_id, 0, 200).unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "receive_task_opened"
        && event.scope_id == child_scope_id).count(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "message_delivered"
        && event.scope_id == child_scope_id).count(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "receive_task_completed"
        && event.scope_id == child_scope_id).count(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "scope_completed"
        && event.scope_id == child_scope_id).count(), 1);
    let rows = super::call_tests::transition_rows(&fixture);
    assert!(repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().is_none());
    assert_eq!(super::call_tests::transition_rows(&fixture), rows);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert_eq!(repository::get_instance(&reopened, &fixture.owner,
        &waiting.instance_id, None).unwrap().variables["received"], json!({"embedded":42}));
}

#[test]
fn called_receive_returns_to_its_pinned_parent_and_cancelled_child_refuses_late_delivery() {
    let fixture = Fixture::new();
    let mut child_model = receive_model();
    child_model.variables.insert("received".into(), Value::Null);
    let child_version = publish_model(&fixture, &child_model);
    let mut caller = super::call_tests::caller(&child_version,
        BTreeMap::from([("received".into(), "outputs.received".into())]));
    caller.variables.insert("received".into(), Value::Null);
    let caller_version = publish_model(&fixture, &caller);
    let parent = start_version(&fixture, &caller_version);
    assert_eq!(parent.status, ProcessInstanceStatus::Waiting);
    let child_id = super::call_tests::child_id(&fixture, &parent.instance_id);
    let child = repository::get_instance(&fixture.db, &fixture.owner, &child_id, None).unwrap();
    assert_eq!(child.status, ProcessInstanceStatus::Waiting);
    assert_eq!(child.subscriptions[0].kind, ProcessSubscriptionKind::ReceiveTask);
    let current_child = repository::get_definition(&fixture.db, &fixture.owner,
        &child_version.definition_id).unwrap().0;
    let mut changed_model = child_model.clone();
    let changed_receive = changed_model.nodes.iter_mut()
        .find(|node| node.id == "Catch_1").unwrap();
    let ProcessNodeKind::ReceiveTask { correlation_expression, .. } = &mut changed_receive.kind else {
        panic!("the immutable called version must contain ReceiveTask")
    };
    *correlation_expression = "'case-v2'".into();
    let changed_draft = repository::save_definition(&fixture.db, &fixture.owner,
        &stamp("save called ReceiveTask V2"), Some(&child_version.definition_id),
        current_child.draft_revision, &current_child.name, "", &changed_model).unwrap();
    let (_, changed_version) = repository::publish_definition(&fixture.db, &fixture.owner,
        &stamp("publish called ReceiveTask V2"), &child_version.definition_id,
        changed_draft.draft_revision, &[], None).unwrap();
    assert_eq!(changed_version.version, child_version.version + 1);
    assert_eq!(child.version, child_version.version);
    assert_eq!(child.subscriptions[0].correlation_key.as_deref(), Some("case-1"));
    let message = envelope(catch_target(&child_version, Some(&child_id),
        Some(&child.subscriptions[0].subscription_id)), json!({"called":true}));
    send(&fixture, &message);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap()
        .into_iter().find(|item| item.key.message_id == message.message_id).unwrap();
    let MessageSelection::Ready(prepared) = repository::message_snapshot(&fixture.db,
        &candidate).unwrap() else { panic!("the pinned called ReceiveTask must be live") };
    let plan = messages::plan_message_delivery(&prepared, at_ms, None).unwrap();
    repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().unwrap();
    assert_eq!(repository::get_instance(&fixture.db, &fixture.owner,
        &child_id, None).unwrap().status, ProcessInstanceStatus::Completed);
    let returned = repository::get_instance(&fixture.db, &fixture.owner,
        &parent.instance_id, None).unwrap();
    assert_eq!(returned.status, ProcessInstanceStatus::Completed);
    assert_eq!(returned.variables["received"], json!({"called":true}));
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert_eq!(repository::get_instance(&reopened, &fixture.owner,
        &parent.instance_id, None).unwrap().variables["received"], json!({"called":true}));
    assert!(repository::deliver_message(&reopened, &prepared, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().is_none());

    let cancelled_parent = start_version(&fixture, &caller_version);
    let cancelled_child_id = super::call_tests::child_id(&fixture, &cancelled_parent.instance_id);
    let cancelled_child = repository::get_instance(&fixture.db, &fixture.owner,
        &cancelled_child_id, None).unwrap();
    let late = envelope(catch_target(&child_version, Some(&cancelled_child_id),
        Some(&cancelled_child.subscriptions[0].subscription_id)), json!({"late":true}));
    send(&fixture, &late);
    let late_at_ms = chrono::Utc::now().timestamp_millis();
    let late_candidate = repository::due_messages(&fixture.db, late_at_ms, 32).unwrap()
        .into_iter().find(|item| item.key.message_id == late.message_id).unwrap();
    repository::cancel_instance(&fixture.db, &fixture.owner, &stamp("cancel caller before receipt"),
        &cancelled_parent.instance_id, cancelled_parent.revision).unwrap();
    assert!(repository::message_snapshot(&fixture.db, &late_candidate).is_err());
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert_eq!(repository::get_instance(&reopened, &fixture.owner,
        &cancelled_parent.instance_id, None).unwrap().status, ProcessInstanceStatus::Cancelled);
    let cancelled_child = repository::get_instance(&reopened, &fixture.owner,
        &cancelled_child_id, None).unwrap();
    assert_eq!(cancelled_child.status, ProcessInstanceStatus::Cancelled);
    assert_eq!(cancelled_child.subscriptions[0].status, ProcessSubscriptionStatus::Cancelled);
    assert_eq!(current(&fixture, &late).message.status, ProcessMessageStatus::Pending);
    let child_events = repository::list_events(&reopened, &fixture.owner,
        &cancelled_child_id, 0, 200).unwrap().0;
    assert_eq!(child_events.iter().filter(|event| event.kind == "receive_task_completed").count(), 0);
}

pub(super) fn receive_boundary_model(message: bool, interrupt: bool)
    -> tentaflow_protocol::processes::ProcessModel {
    let mut model = receive_model();
    let (id, kind) = if message {
        ("ReceiveMessage", ProcessNodeKind::BoundaryMessage {
            attached_to_id: "Catch_1".into(), cancel_activity: interrupt,
            message_ref: "Message_1".into(), correlation_expression: "'case-1'".into(),
            output_mapping: BTreeMap::new(),
        })
    } else {
        model.timer_timezone = Some("UTC".into());
        ("ReceiveTimer", ProcessNodeKind::BoundaryTimer {
            attached_to_id: "Catch_1".into(), cancel_activity: interrupt,
            timer: tentaflow_protocol::processes::ProcessTimerSpec::Duration { seconds: 1 },
        })
    };
    model.nodes.push(ProcessNode { id: id.into(), name: "Receive boundary".into(),
        kind, repeat: None });
    model.nodes.push(ProcessNode { id: "BoundaryEnd".into(), name: "Boundary end".into(),
        kind: ProcessNodeKind::End, repeat: None });
    model.sequence_flows.push(edge("BoundaryFlow", id, "BoundaryEnd"));
    model
}

#[test]
fn receive_first_consumes_own_subscription_then_disarms_timer_and_message_siblings() {
    for message_boundary in [false,true] {
        let fixture = Fixture::new();
        let version = publish_model(&fixture, &receive_boundary_model(message_boundary, true));
        let started = start_version(&fixture, &version);
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let own = snapshot.subscriptions.iter().find(|sub|
            sub.kind == ProcessSubscriptionKind::ReceiveTask).unwrap();
        let message = envelope(catch_target(&version, Some(&started.instance_id),
            Some(&own.subscription_id)), json!({"source":"receive-first"}));
        send(&fixture, &message);
        let drained = messages::drain_pending(&fixture.db, chrono::Utc::now().timestamp_millis());
        drained.completion.unwrap();
        assert_eq!(drained.delivered, 1);
        let after = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        assert_eq!(after.instance.status, ProcessInstanceStatus::Completed);
        assert_eq!(after.instance.variables["received"], json!({"source":"receive-first"}));
        assert_eq!(after.subscriptions.iter().find(|sub|
            sub.subscription_id == own.subscription_id).unwrap().status,
            ProcessSubscriptionStatus::Consumed);
        if message_boundary {
            assert_eq!(after.subscriptions.iter().find(|sub|
                sub.node_id == "ReceiveMessage").unwrap().status,
                ProcessSubscriptionStatus::Cancelled);
        } else {
            let timer = snapshot.timers.iter().find(|timer|
                timer.node_id == "ReceiveTimer").unwrap();
            assert_eq!(after.timers.iter().find(|row| row.timer_id == timer.timer_id)
                .unwrap().status, tentaflow_protocol::processes::ProcessTimerStatus::Cancelled);
            let stale = super::timers::drain_due(&fixture.db, timer.due_at_ms.unwrap() + 1);
            stale.completion.unwrap();
            assert_eq!(stale.fired, 0);
        }
        let events = repository::list_events(&fixture.db, &fixture.owner,
            &started.instance_id, 0, 200).unwrap().0;
        let delivered = events.iter().position(|event| event.kind == "message_delivered"
            && event.data["subscription_id"] == own.subscription_id).unwrap();
        let completed = events.iter().position(|event| event.kind == "receive_task_completed")
            .unwrap();
        assert!(delivered < completed);
        assert_eq!(events.iter().filter(|event| event.kind == "receive_task_completed")
            .count(), 1);
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let replay = messages::drain_pending(&reopened, chrono::Utc::now().timestamp_millis());
        replay.completion.unwrap();
        assert_eq!(replay.delivered, 0);
    }
}

#[test]
fn interrupting_receive_boundary_cancels_only_its_own_receive_subscription() {
    for message_boundary in [false,true] {
        let fixture = Fixture::new();
        let version = publish_model(&fixture, &receive_boundary_model(message_boundary, true));
        let started = start_version(&fixture, &version);
        let independent_version = publish_model(&fixture, &receive_model());
        let independent = start_version(&fixture, &independent_version);
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let own = snapshot.subscriptions.iter().find(|sub|
            sub.kind == ProcessSubscriptionKind::ReceiveTask).unwrap();
        let stale = envelope(catch_target(&version, Some(&started.instance_id),
            Some(&own.subscription_id)), json!({"late":true}));
        send(&fixture, &stale);
        if message_boundary {
            let boundary = snapshot.subscriptions.iter().find(|sub|
                sub.node_id == "ReceiveMessage").unwrap();
            let message = envelope(catch_target(&version, Some(&started.instance_id),
                Some(&boundary.subscription_id)), Value::Null);
            send(&fixture, &message);
            let at_ms = chrono::Utc::now().timestamp_millis();
            let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap()
                .into_iter().find(|row| row.key.message_id == message.message_id).unwrap();
            let MessageSelection::Ready(selected) =
                repository::message_snapshot(&fixture.db, &candidate).unwrap()
                else { panic!("the selected boundary must remain open") };
            repository::deliver_message(&fixture.db, &selected,
                repository::ProcessPlanInput::Canonical, at_ms).unwrap().unwrap();
        } else {
            let timer = snapshot.timers.iter().find(|timer|
                timer.node_id == "ReceiveTimer").unwrap();
            let drained = super::timers::drain_due(&fixture.db, timer.due_at_ms.unwrap() + 1);
            drained.completion.unwrap();
            assert_eq!(drained.fired, 1);
        }
        let after = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        assert_eq!(after.instance.status, ProcessInstanceStatus::Completed);
        assert_eq!(after.subscriptions.iter().find(|sub|
            sub.subscription_id == own.subscription_id).unwrap().status,
            ProcessSubscriptionStatus::Cancelled);
        let independent_after = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &independent.instance_id).unwrap();
        assert_eq!(independent_after.subscriptions.iter().find(|sub|
            sub.kind == ProcessSubscriptionKind::ReceiveTask).unwrap().status,
            ProcessSubscriptionStatus::Open);
        let events = repository::list_events(&fixture.db, &fixture.owner,
            &started.instance_id, 0, 200).unwrap().0;
        assert!(events.iter().all(|event| event.kind != "receive_task_completed"));
        let drained = messages::drain_pending(&fixture.db,
            chrono::Utc::now().timestamp_millis());
        drained.completion.unwrap();
        assert_eq!(drained.delivered, 0);
        assert_eq!(repository::get_message(&fixture.db, &fixture.owner,
            &fixture.owner.user_id, &stale.message_id).unwrap().message.status,
            ProcessMessageStatus::Cancelled);
        let late = envelope(catch_target(&version, Some(&started.instance_id),
            Some(&own.subscription_id)), json!({"after closure":true}));
        let before_rejected = super::signal_proof_tests::all_transition_rows(&fixture);
        assert!(repository::send_message(&fixture.db, &fixture.owner,
            &stamp("late Receive envelope"), &late,
            chrono::Utc::now().timestamp_millis()).is_err());
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before_rejected);
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        assert_eq!(repository::get_instance(&reopened, &fixture.owner,
            &started.instance_id, None).unwrap().status, ProcessInstanceStatus::Completed);
    }
}

#[test]
fn noninterrupting_receive_boundary_keeps_own_wait_until_exact_delivery() {
    let fixture = Fixture::new();
    let version = publish_model(&fixture, &receive_boundary_model(true, false));
    let started = start_version(&fixture, &version);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let own = snapshot.subscriptions.iter().find(|sub|
        sub.kind == ProcessSubscriptionKind::ReceiveTask).unwrap();
    let boundary = snapshot.subscriptions.iter().find(|sub|
        sub.node_id == "ReceiveMessage").unwrap();
    let first = envelope(catch_target(&version, Some(&started.instance_id),
        Some(&boundary.subscription_id)), json!({"boundary":true}));
    send(&fixture, &first);
    let fired = messages::drain_pending(&fixture.db, chrono::Utc::now().timestamp_millis());
    fired.completion.unwrap();
    assert_eq!(fired.delivered, 1);
    let intermediate = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(intermediate.subscriptions.iter().find(|sub|
        sub.subscription_id == own.subscription_id).unwrap().status,
        ProcessSubscriptionStatus::Open);
    assert!(intermediate.tokens.iter().any(|token|
        token.token_id == own.token_id && token.status == "waiting"));
    let second = envelope(catch_target(&version, Some(&started.instance_id),
        Some(&own.subscription_id)), json!({"receive":true}));
    send(&fixture, &second);
    let delivered = messages::drain_pending(&fixture.db, chrono::Utc::now().timestamp_millis());
    delivered.completion.unwrap();
    assert_eq!(delivered.delivered, 1);
    let after = repository::get_instance(&fixture.db, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(after.status, ProcessInstanceStatus::Completed);
    assert_eq!(after.variables["received"], json!({"receive":true}));
    let events = repository::list_events(&fixture.db, &fixture.owner,
        &started.instance_id, 0, 200).unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "receive_task_completed").count(), 1);
}

#[test]
fn noninterrupting_receive_timer_preserves_own_subscription_for_later_delivery() {
    let fixture = Fixture::new();
    let version = publish_model(&fixture, &receive_boundary_model(false, false));
    let started = start_version(&fixture, &version);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let own = snapshot.subscriptions.iter().find(|row|
        row.kind == ProcessSubscriptionKind::ReceiveTask).unwrap();
    let timer = snapshot.timers.iter().find(|row|
        row.node_id == "ReceiveTimer").unwrap();
    let due = timer.due_at_ms.unwrap() + 1;
    let fired = super::timers::drain_due(&fixture.db, due);
    fired.completion.unwrap();
    assert_eq!(fired.fired, 1);
    let intermediate = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(intermediate.subscriptions.iter().find(|row|
        row.subscription_id == own.subscription_id).unwrap().status,
        ProcessSubscriptionStatus::Open);
    let message = envelope(catch_target(&version, Some(&started.instance_id),
        Some(&own.subscription_id)), json!({"later":true}));
    send(&fixture, &message);
    let delivered = messages::drain_pending(&fixture.db, due + 1);
    delivered.completion.unwrap();
    assert_eq!(delivered.delivered, 1);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let after = repository::get_instance(&reopened, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(after.status, ProcessInstanceStatus::Completed);
    assert_eq!(after.variables["received"], json!({"later":true}));
}

#[test]
fn noninterrupting_receive_message_maps_its_payload_without_replacing_later_receive_input() {
    let fixture = Fixture::new();
    let mut model = receive_boundary_model(true, false);
    model.variables.insert("boundary_payload".into(), Value::Null);
    let boundary_node = model.nodes.iter_mut().find(|node|
        node.id == "ReceiveMessage").unwrap();
    let ProcessNodeKind::BoundaryMessage { output_mapping, .. } =
        &mut boundary_node.kind else { unreachable!() };
    output_mapping.insert("boundary_payload".into(), "outputs".into());
    let version = publish_model(&fixture, &model);
    let started = start_version(&fixture, &version);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let own = snapshot.subscriptions.iter().find(|row|
        row.kind == ProcessSubscriptionKind::ReceiveTask).unwrap();
    let boundary = snapshot.subscriptions.iter().find(|row|
        row.node_id == "ReceiveMessage").unwrap();
    let first = envelope(catch_target(&version, Some(&started.instance_id),
        Some(&boundary.subscription_id)), json!({"boundary":1}));
    send(&fixture, &first);
    let first_drain = messages::drain_pending(&fixture.db,
        chrono::Utc::now().timestamp_millis());
    first_drain.completion.unwrap();
    assert_eq!(first_drain.delivered, 1);
    let between = repository::get_instance(&fixture.db, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(between.variables["boundary_payload"], json!({"boundary":1}));
    assert_eq!(between.variables["received"], Value::Null);
    let second = envelope(catch_target(&version, Some(&started.instance_id),
        Some(&own.subscription_id)), json!({"receive":2}));
    send(&fixture, &second);
    let second_drain = messages::drain_pending(&fixture.db,
        chrono::Utc::now().timestamp_millis());
    second_drain.completion.unwrap();
    assert_eq!(second_drain.delivered, 1);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let after = repository::get_instance(&reopened, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(after.variables["boundary_payload"], json!({"boundary":1}));
    assert_eq!(after.variables["received"], json!({"receive":2}));
}

#[test]
fn broad_message_to_receive_and_its_boundary_remains_ambiguous() {
    let fixture = Fixture::new();
    let version = publish_model(&fixture, &receive_boundary_model(true, true));
    let started = start_version(&fixture, &version);
    let message = envelope(catch_target(&version, Some(&started.instance_id), None), Value::Null);
    send(&fixture, &message);
    let candidate = repository::due_messages(&fixture.db,
        chrono::Utc::now().timestamp_millis(), 32).unwrap().into_iter()
        .find(|candidate| candidate.key.message_id == message.message_id).unwrap();
    assert!(matches!(repository::message_snapshot(&fixture.db, &candidate).unwrap(),
        MessageSelection::Ambiguous));
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(snapshot.subscriptions.iter().filter(|sub|
        sub.status == ProcessSubscriptionStatus::Open).count(), 2);
}

#[test]
fn embedded_and_called_receive_message_winners_bind_child_scope_and_parent_return() {
    for called in [false,true] {
        for receive_first in [false,true] {
            let fixture = Fixture::new();
            let model = embedded_model(receive_boundary_model(true, true), "InnerScope");
            let version = publish_model(&fixture, &model);
            let parent = if called {
                let caller = publish_model(&fixture,
                    &super::call_tests::caller(&version, BTreeMap::new()));
                start_version(&fixture, &caller)
            } else { start_version(&fixture, &version) };
            let child_id = if called {
                super::call_tests::child_id(&fixture, &parent.instance_id)
            } else { parent.instance_id.clone() };
            let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
                &child_id).unwrap();
            if called {
                let original = repository::get_definition(&fixture.db, &fixture.owner,
                    &version.definition_id).unwrap().0;
                let mut newer = version.model.clone();
                newer.nodes.iter_mut().find(|node| node.id == "InnerScope").unwrap().name =
                    "Later Receive body".into();
                let draft = repository::save_definition(&fixture.db, &fixture.owner,
                    &stamp("change Receive child version"), Some(&version.definition_id),
                    original.draft_revision, "Receive child", "", &newer).unwrap();
                let latest = repository::publish_definition(&fixture.db, &fixture.owner,
                    &stamp("publish Receive child version"), &version.definition_id,
                    draft.draft_revision, &[], None).unwrap().1;
                assert!(latest.version > version.version);
                assert_eq!(snapshot.instance.version, version.version);
            }
            let own = snapshot.subscriptions.iter().find(|sub|
                sub.kind == ProcessSubscriptionKind::ReceiveTask).unwrap();
            let boundary = snapshot.subscriptions.iter().find(|sub|
                sub.node_id == "ReceiveMessage").unwrap();
            assert_ne!(own.scope_id, child_id);
            assert_eq!(own.scope_id, boundary.scope_id);
            assert_eq!(own.token_id, boundary.token_id);
            let selected = if receive_first { own } else { boundary };
            let message = envelope(catch_target(&version, Some(&child_id),
                Some(&selected.subscription_id)), json!({"factual":true}));
            send(&fixture, &message);
            let drained = messages::drain_pending(&fixture.db,
                chrono::Utc::now().timestamp_millis());
            drained.completion.unwrap();
            assert_eq!(drained.delivered, 1);
            let after = repository::runtime_snapshot(&fixture.db, &fixture.owner,
                &child_id).unwrap();
            assert_eq!(after.subscriptions.iter().find(|row|
                row.subscription_id == own.subscription_id).unwrap().status,
                if receive_first { ProcessSubscriptionStatus::Consumed }
                else { ProcessSubscriptionStatus::Cancelled });
            let events = repository::list_events(&fixture.db, &fixture.owner,
                &child_id, 0, 200).unwrap().0;
            assert_eq!(events.iter().filter(|event| event.kind == "receive_task_completed"
                && event.scope_id == own.scope_id).count(), usize::from(receive_first));
            let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
            assert_eq!(repository::get_instance(&reopened, &fixture.owner,
                &parent.instance_id, None).unwrap().status, ProcessInstanceStatus::Completed);
            let replay = messages::drain_pending(&reopened,
                chrono::Utc::now().timestamp_millis());
            replay.completion.unwrap();
            assert_eq!(replay.delivered, 0);
        }
    }
}

#[test]
fn parent_cancel_fences_called_embedded_receive_and_both_open_subscriptions() {
    let fixture = Fixture::new();
    let child_version = publish_model(&fixture,
        &embedded_model(receive_boundary_model(true, true), "InnerScope"));
    let caller = publish_model(&fixture,
        &super::call_tests::caller(&child_version, BTreeMap::new()));
    let parent = start_version(&fixture, &caller);
    let child_id = super::call_tests::child_id(&fixture, &parent.instance_id);
    let before_child = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &child_id).unwrap();
    let own = before_child.subscriptions.iter().find(|row|
        row.kind == ProcessSubscriptionKind::ReceiveTask).unwrap();
    let boundary = before_child.subscriptions.iter().find(|row|
        row.node_id == "ReceiveMessage").unwrap();
    let before_parent = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &parent.instance_id).unwrap();
    let late = envelope(catch_target(&child_version, Some(&child_id),
        Some(&own.subscription_id)), json!({"late":true}));
    send(&fixture, &late);
    repository::cancel_instance(&fixture.db, &fixture.owner, &stamp("cancel parent Receive"),
        &parent.instance_id, before_parent.instance.revision).unwrap();
    let after = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    assert_eq!(after.instance.status, ProcessInstanceStatus::Cancelled);
    for id in [&own.subscription_id, &boundary.subscription_id] {
        assert_eq!(after.subscriptions.iter().find(|row| &row.subscription_id == id)
            .unwrap().status, ProcessSubscriptionStatus::Cancelled);
    }
    let drained = messages::drain_pending(&fixture.db,
        chrono::Utc::now().timestamp_millis());
    drained.completion.unwrap();
    assert_eq!(drained.delivered, 0);
    assert_eq!(repository::get_message(&fixture.db, &fixture.owner,
        &fixture.owner.user_id, &late.message_id).unwrap().message.status,
        ProcessMessageStatus::Cancelled);
    let child_events = repository::list_events(&fixture.db, &fixture.owner,
        &child_id, 0, 200).unwrap().0;
    assert_eq!(child_events.iter().filter(|event|
        event.kind == "receive_task_completed").count(), 0);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert_eq!(repository::get_instance(&reopened, &fixture.owner,
        &child_id, None).unwrap().status, ProcessInstanceStatus::Cancelled);
}

pub(super) fn receive_race_model() -> tentaflow_protocol::processes::ProcessModel {
    let mut model = super::messages::test_support::race_model();
    model.nodes.iter_mut().find(|node| node.id == "Catch_1").unwrap().kind =
        ProcessNodeKind::ReceiveTask {
            message_ref: "Message_1".into(),
            correlation_expression: "'case-1'".into(),
            output_mapping: BTreeMap::from([("received".into(), "outputs".into())]),
        };
    model
}

#[test]
fn direct_receive_and_timer_race_each_winner_closes_only_its_sibling() {
    for called in [false, true] {
        for receive_first in [false, true] {
            let fixture = Fixture::new();
            let child_version = publish_model(&fixture,
                &embedded_model(receive_race_model(), "InnerScope"));
            let parent = if called {
                let caller = publish_model(&fixture,
                    &super::call_tests::caller(&child_version, BTreeMap::new()));
                start_version(&fixture, &caller)
            } else { start_version(&fixture, &child_version) };
            let child_id = if called {
                super::call_tests::child_id(&fixture, &parent.instance_id)
            } else { parent.instance_id.clone() };
            let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
                &child_id).unwrap();
            let own = snapshot.subscriptions.iter().find(|row|
                row.kind == ProcessSubscriptionKind::ReceiveTask).unwrap();
            let timer = snapshot.timers.iter().find(|row|
                row.node_id == "Timer_1").unwrap();
            if called {
                let original = repository::get_definition(&fixture.db, &fixture.owner,
                    &child_version.definition_id).unwrap().0;
                let mut newer = child_version.model.clone();
                newer.nodes.iter_mut().find(|node| node.id == "InnerScope").unwrap().name =
                    "Later gateway child".into();
                let draft = repository::save_definition(&fixture.db, &fixture.owner,
                    &stamp("change gateway child"), Some(&child_version.definition_id),
                    original.draft_revision, "Gateway child", "", &newer).unwrap();
                let latest = repository::publish_definition(&fixture.db, &fixture.owner,
                    &stamp("publish gateway child"), &child_version.definition_id,
                    draft.draft_revision, &[], None).unwrap().1;
                assert!(latest.version > child_version.version);
                assert_eq!(snapshot.instance.version, child_version.version);
            }
            let race = snapshot.event_races.iter().find(|row|
                row.race_id == own.race_id.as_ref().unwrap().as_str()).unwrap();
            assert_eq!(timer.race_id.as_deref(), Some(race.race_id.as_str()));
            assert_eq!(own.scope_id, race.scope_id);
            let message = envelope(catch_target(&child_version, Some(&child_id),
                Some(&own.subscription_id)), json!({"winner":"Receive"}));
            send(&fixture, &message);
            if receive_first {
                let delivered = messages::drain_pending(&fixture.db,
                    chrono::Utc::now().timestamp_millis());
                delivered.completion.unwrap();
                assert_eq!(delivered.delivered, 1);
                let late = super::timers::drain_due(&fixture.db,
                    timer.due_at_ms.unwrap() + 1);
                late.completion.unwrap();
                assert_eq!(late.fired, 0);
            } else {
                let fired = super::timers::drain_due(&fixture.db,
                    timer.due_at_ms.unwrap() + 1);
                fired.completion.unwrap();
                assert_eq!(fired.fired, 1);
                let late = messages::drain_pending(&fixture.db,
                    timer.due_at_ms.unwrap() + 1);
                late.completion.unwrap();
                assert_eq!(late.delivered, 0);
            }
            let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
            let after = repository::runtime_snapshot(&reopened, &fixture.owner,
                &child_id).unwrap();
            assert_eq!(after.instance.status, ProcessInstanceStatus::Completed);
            assert_eq!(after.subscriptions.iter().find(|row|
                row.subscription_id == own.subscription_id).unwrap().status,
                if receive_first { ProcessSubscriptionStatus::Consumed }
                else { ProcessSubscriptionStatus::Cancelled });
            assert_eq!(after.event_races.iter().find(|row|
                row.race_id == race.race_id).unwrap().status,
                tentaflow_protocol::processes::ProcessEventRaceStatus::Won);
            let events = repository::list_events(&reopened, &fixture.owner,
                &child_id, 0, 200).unwrap().0;
            assert_eq!(events.iter().filter(|event| event.kind == "event_race_won"
                && event.scope_id == race.scope_id).count(), 1);
            assert_eq!(events.iter().filter(|event| event.kind == "receive_task_completed")
                .count(), usize::from(receive_first));
            assert_eq!(repository::get_instance(&reopened, &fixture.owner,
                &parent.instance_id, None).unwrap().status, ProcessInstanceStatus::Completed);
        }
    }
}

#[test]
fn two_direct_receives_are_ambiguous_broadly_but_exact_target_has_one_winner() {
    let fixture = Fixture::new();
    let mut model = receive_race_model();
    let second = model.nodes.iter_mut().find(|node| node.id == "Timer_1").unwrap();
    second.id = "Receive_2".into();
    second.name = "Other Receive".into();
    second.kind = ProcessNodeKind::ReceiveTask {
        message_ref: "Message_1".into(),
        correlation_expression: "'case-1'".into(),
        output_mapping: BTreeMap::new(),
    };
    for edge in &mut model.sequence_flows {
        if edge.target_id == "Timer_1" { edge.target_id = "Receive_2".into(); }
        if edge.source_id == "Timer_1" { edge.source_id = "Receive_2".into(); }
    }
    model.timer_timezone = None;
    let version = publish_model(&fixture, &model);
    let receiver = start_version(&fixture, &version);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &receiver.instance_id).unwrap();
    let own = snapshot.subscriptions.iter().filter(|row|
        row.kind == ProcessSubscriptionKind::ReceiveTask).collect::<Vec<_>>();
    assert_eq!(own.len(), 2);
    let broad = envelope(catch_target(&version, Some(&receiver.instance_id), None),
        json!({"ambiguous":true}));
    send(&fixture, &broad);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap().into_iter()
        .find(|row| row.key.message_id == broad.message_id).unwrap();
    assert!(matches!(repository::message_snapshot(&fixture.db, &candidate).unwrap(),
        MessageSelection::Ambiguous));
    let exact = envelope(catch_target(&version, Some(&receiver.instance_id),
        Some(&own[0].subscription_id)), json!({"exact":true}));
    send(&fixture, &exact);
    let candidate = repository::due_messages(&fixture.db,
        chrono::Utc::now().timestamp_millis(), 32).unwrap().into_iter()
        .find(|row| row.key.message_id == exact.message_id).unwrap();
    let MessageSelection::Ready(prepared) =
        repository::message_snapshot(&fixture.db, &candidate).unwrap()
        else { panic!("the exact Receive alternative must be ready") };
    repository::deliver_message(&fixture.db, &prepared,
        repository::ProcessPlanInput::Canonical,
        chrono::Utc::now().timestamp_millis()).unwrap().unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let after = repository::runtime_snapshot(&reopened, &fixture.owner,
        &receiver.instance_id).unwrap();
    assert_eq!(after.instance.status, ProcessInstanceStatus::Completed);
    assert_eq!(after.subscriptions.iter().filter(|row|
        row.status == ProcessSubscriptionStatus::Consumed).count(), 1);
    assert_eq!(after.subscriptions.iter().filter(|row|
        row.status == ProcessSubscriptionStatus::Cancelled).count(), 1);
    let stale = messages::drain_pending(&reopened,
        chrono::Utc::now().timestamp_millis() + 1);
    stale.completion.unwrap();
    assert_eq!(stale.delivered, 0);
}

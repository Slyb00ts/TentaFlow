// ============ File: send_receive_proof_tests.rs — Full-table provenance controls for durable SendTask and ReceiveTask ============

use super::call_tests::transition_rows;
use super::messages::{self, test_support::*};
use super::repository::{self, MessageSelection, TerminationAttempt, VariableEffect};
use super::runtime::{self, test_support::*};
use serde_json::json;
use tentaflow_protocol::processes::{ProcessInstanceStatus, ProcessNodeKind};
use uuid::Uuid;

#[test]
fn send_task_rejects_omitted_or_forged_admission_before_canonical_commit() {
    let fixture = Fixture::new();
    let receiving = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &receiving);
    let source = publish_model(&fixture,
        &super::send_receive_tests::send_model(&receiving.definition_id, &receiver.instance_id));
    let instance_id = Uuid::new_v4().to_string();
    let command = stamp("send-source-proof");
    let vars = serde_json::to_value(&source.model.variables).unwrap();
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&source.model, &instance_id, &fixture.owner,
        &source.definition_id, source.version, vars.clone(), runtime::StartCause::Manual,
        at_ms, manual_input(&command)).unwrap();
    assert_eq!(plan.create_messages.len(), 1);
    let before = transition_rows(&fixture);
    let mut omitted = plan.clone();
    omitted.events.iter_mut().find(|event| event.kind == "send_task_admitted")
        .unwrap().kind = "node_completed".into();
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &source.definition_id, source.version, &vars, &omitted, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut forged = plan.clone();
    forged.create_messages[0].message.payload = json!({"value":43});
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &source.definition_id, source.version, &vars, &forged, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut wrong_target = plan.clone();
    wrong_target.create_messages[0].message.target = catch_target(&receiving,
        Some(&Uuid::new_v4().to_string()), Some(&receiver.subscriptions[0].subscription_id));
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &source.definition_id, source.version, &vars, &wrong_target, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut wrong_ttl = plan.clone();
    wrong_ttl.create_messages[0].message.ttl_seconds += 1;
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &source.definition_id, source.version, &vars, &wrong_ttl, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut wrong_event = plan.clone();
    wrong_event.create_messages[0].source_event_index = plan.events.len() - 1;
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &source.definition_id, source.version, &vars, &wrong_event, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut foreign = plan.clone();
    foreign.create_messages[0].source_activation_id = Uuid::new_v4().to_string();
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &source.definition_id, source.version, &vars, &foreign, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut duplicate = plan.clone();
    duplicate.events.push(plan.events.iter().find(|event| event.kind == "send_task_admitted")
        .unwrap().clone());
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &source.definition_id, source.version, &vars, &duplicate, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut extra_successor = plan.clone();
    let direct = plan.create_tokens.iter().find(|token|
        token.node_id == "End_1").unwrap().clone();
    let source_id = plan.token_sources.get(&direct.token_id).unwrap().clone();
    let mut extra = direct;
    extra.token_id = Uuid::new_v4().to_string();
    extra_successor.token_sources.insert(extra.token_id.clone(), source_id);
    extra_successor.create_tokens.push(extra);
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &source.definition_id, source.version, &vars, &extra_successor, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &source.definition_id, source.version, &vars, &plan, at_ms).unwrap();
    assert_ne!(transition_rows(&fixture), before);
}

#[test]
fn receive_task_rejects_missing_duplicate_foreign_and_reordered_completion_before_commit() {
    let fixture = Fixture::new();
    let version = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let instance_id = Uuid::new_v4().to_string();
    let command = stamp("receive-arm-kind-proof");
    let vars = serde_json::to_value(&version.model.variables).unwrap();
    let at_ms = chrono::Utc::now().timestamp_millis();
    let start = runtime::plan_start(&version.model, &instance_id, &fixture.owner,
        &version.definition_id, version.version, vars.clone(), runtime::StartCause::Manual,
        at_ms, manual_input(&command)).unwrap();
    let opened = start.events.iter().find(|event| event.kind == "receive_task_opened").unwrap();
    assert_eq!(opened.data["kind"], "receive_task");
    let before_start = transition_rows(&fixture);
    let mut wrong_kind = start.clone();
    wrong_kind.events.iter_mut().find(|event| event.kind == "receive_task_opened")
        .unwrap().data["kind"] = json!("ReceiveTask");
    let error = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &vars, &wrong_kind, at_ms)
        .unwrap_err();
    assert!(format!("{error:#}").contains("ReceiveTask correlation differs from its pinned entry variables"));
    assert_eq!(transition_rows(&fixture), before_start);
    repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &vars, &start, at_ms).unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let events = repository::list_events(&reopened, &fixture.owner, &instance_id, 0, 200).unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "receive_task_opened"
        && event.data["kind"] == "receive_task").count(), 1);
    let committed_start = transition_rows(&fixture);
    repository::start_instance(&reopened, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &vars, &start, at_ms).unwrap();
    assert_eq!(transition_rows(&fixture), committed_start);
    let receiver = repository::runtime_snapshot(&fixture.db, &fixture.owner, &instance_id).unwrap();
    let foreign_receiver = start_version(&fixture, &version);
    let message = envelope(catch_target(&version, Some(&receiver.instance.instance_id),
        Some(&receiver.subscriptions[0].subscription_id)), json!({"value":42}));
    send(&fixture, &message);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap().into_iter()
        .find(|item| item.key.message_id == message.message_id).unwrap();
    let MessageSelection::Ready(prepared) = repository::message_snapshot(&fixture.db, &candidate).unwrap()
        else { panic!("the real ReceiveTask must be selected") };
    let plan = messages::plan_message_delivery(&prepared, at_ms).unwrap();
    let before = transition_rows(&fixture);
    let mut omitted = plan.clone();
    omitted.events.iter_mut().find(|event| event.kind == "receive_task_completed")
        .unwrap().kind = "node_completed".into();
    assert!(repository::deliver_message(&fixture.db, &prepared, &omitted, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut duplicate = plan.clone();
    duplicate.events.push(plan.events.iter().find(|event|
        event.kind == "receive_task_completed").unwrap().clone());
    assert!(repository::deliver_message(&fixture.db, &prepared, &duplicate, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut foreign = plan.clone();
    foreign.events.iter_mut().find(|event| event.kind == "receive_task_completed")
        .unwrap().data["subscription_id"] = json!(Uuid::new_v4().to_string());
    assert!(repository::deliver_message(&fixture.db, &prepared, &foreign, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut wrong_message = plan.clone();
    wrong_message.events.iter_mut().find(|event| event.kind == "receive_task_completed")
        .unwrap().data["message_id"] = json!(Uuid::new_v4().to_string());
    assert!(repository::deliver_message(&fixture.db, &prepared, &wrong_message, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut wrong_payload = plan.clone();
    wrong_payload.events.iter_mut().find(|event| event.kind == "message_delivered")
        .unwrap().data["payload"] = json!({"value":43});
    assert!(repository::deliver_message(&fixture.db, &prepared, &wrong_payload, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut foreign_cancellation = plan.clone();
    foreign_cancellation.cancel_token_ids.push(foreign_receiver.subscriptions[0].token_id.clone());
    assert!(repository::deliver_message(&fixture.db, &prepared, &foreign_cancellation, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut wrong_mapping = plan.clone();
    wrong_mapping.variables["received"] = json!({"value":44});
    assert!(repository::deliver_message(&fixture.db, &prepared, &wrong_mapping, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut wrong_cutoff = plan.clone();
    let completion_index = wrong_cutoff.events.iter().position(|event|
        event.kind == "receive_task_completed").unwrap();
    let mapped = wrong_cutoff.variable_effects.iter_mut().find(|effect|
        matches!(effect, VariableEffect::Mapped { node_id, .. } if node_id == "Catch_1")).unwrap();
    let VariableEffect::Mapped { event_index, .. } = mapped else { unreachable!() };
    assert_ne!(*event_index, completion_index);
    *event_index = completion_index;
    let error = repository::deliver_message(&fixture.db, &prepared, &wrong_cutoff, at_ms)
        .unwrap_err();
    assert!(format!("{error:#}").contains("mapped message output has no exact source-time delivered envelope"));
    assert_eq!(transition_rows(&fixture), before);
    let mut reordered = plan.clone();
    let delivery = reordered.events.iter().position(|event| event.kind == "message_delivered").unwrap();
    let completion = reordered.events.iter().position(|event| event.kind == "receive_task_completed").unwrap();
    reordered.events.swap(delivery, completion);
    assert!(repository::deliver_message(&fixture.db, &prepared, &reordered, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    repository::deliver_message(&fixture.db, &prepared, &plan, at_ms).unwrap().unwrap();
    assert_ne!(transition_rows(&fixture), before);
    let events = repository::list_events(&fixture.db, &fixture.owner,
        &receiver.instance.instance_id, 0, 200).unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "receive_task_completed").count(), 1);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::deliver_message(&reopened, &prepared, &plan, at_ms).unwrap().is_none());
}

#[test]
fn receive_task_termination_requires_its_exact_delivery_and_terminal_source() {
    let fixture = Fixture::new();
    let mut model = super::send_receive_tests::receive_model();
    model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::TerminateEnd;
    let version = publish_model(&fixture, &model);
    let receiver = start_version(&fixture, &version);
    let message = envelope(catch_target(&version, Some(&receiver.instance_id),
        Some(&receiver.subscriptions[0].subscription_id)), json!({"value":42}));
    let sent = send(&fixture, &message);
    let at_ms = sent.received_at_ms;
    let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap().into_iter()
        .find(|item| item.key.message_id == message.message_id).unwrap();
    let MessageSelection::Ready(prepared) = repository::message_snapshot(&fixture.db, &candidate).unwrap()
        else { panic!("the actual ReceiveTask must be selected") };
    let plan = messages::plan_message_delivery(&prepared, at_ms).unwrap();
    assert!(matches!(plan.termination_attempts.as_slice(), [TerminationAttempt::Success(_)]));
    let before = transition_rows(&fixture);
    let mut wrong_source = plan.clone();
    let TerminationAttempt::Success(source) = &mut wrong_source.termination_attempts[0]
        else { panic!("the root termination must be successful") };
    source.source_event_id = Uuid::new_v4().to_string();
    assert!(repository::deliver_message(&fixture.db, &prepared, &wrong_source, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut missing_completion = plan.clone();
    missing_completion.events.iter_mut().find(|event| event.kind == "receive_task_completed")
        .unwrap().kind = "node_completed".into();
    assert!(repository::deliver_message(&fixture.db, &prepared, &missing_completion, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut wrong_cutoff = plan.clone();
    let completion_index = wrong_cutoff.events.iter().position(|event|
        event.kind == "receive_task_completed").unwrap();
    let mapped = wrong_cutoff.variable_effects.iter_mut().find(|effect|
        matches!(effect, VariableEffect::Mapped { node_id, .. } if node_id == "Catch_1")).unwrap();
    let VariableEffect::Mapped { event_index, .. } = mapped else { unreachable!() };
    assert_ne!(*event_index, completion_index);
    *event_index = completion_index;
    let error = repository::deliver_message(&fixture.db, &prepared, &wrong_cutoff, at_ms)
        .unwrap_err();
    assert!(format!("{error:#}").contains("mapped message output has no exact source-time delivered envelope"));
    assert_eq!(transition_rows(&fixture), before);
    repository::deliver_message(&fixture.db, &prepared, &plan, at_ms).unwrap().unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert_eq!(repository::get_instance(&reopened, &fixture.owner,
        &receiver.instance_id, None).unwrap().status, ProcessInstanceStatus::Completed);
    let events = repository::list_events(&reopened, &fixture.owner,
        &receiver.instance_id, 0, 200).unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "receive_task_completed").count(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "terminate_end_reached").count(), 1);
    assert!(repository::deliver_message(&reopened, &prepared, &plan, at_ms).unwrap().is_none());
}

#[test]
fn embedded_receive_return_failure_keeps_its_factual_message_and_child_source() {
    let fixture = Fixture::new();
    let mut child = super::send_receive_tests::receive_model();
    child.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::TerminateEnd;
    let mut model = embedded_model(child, "Scope");
    let scope = model.nodes.iter_mut().find(|node| node.id == "Scope").unwrap();
    let ProcessNodeKind::SubProcess { output_mapping, .. } = &mut scope.kind
        else { panic!("the pinned child scope is missing") };
    output_mapping.insert("mapped_result".into(), "outputs.missing.required".into());
    let version = publish_model(&fixture, &model);
    let receiver = start_version(&fixture, &version);
    let message = envelope(catch_target(&version, Some(&receiver.instance_id),
        Some(&receiver.subscriptions[0].subscription_id)), json!({"value":42}));
    let sent = send(&fixture, &message);
    let at_ms = sent.received_at_ms;
    let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap().into_iter()
        .find(|item| item.key.message_id == message.message_id).unwrap();
    let MessageSelection::Ready(prepared) = repository::message_snapshot(&fixture.db, &candidate).unwrap()
        else { panic!("the embedded ReceiveTask must be selected") };
    let plan = messages::plan_message_delivery(&prepared, at_ms).unwrap();
    assert!(matches!(plan.termination_attempts.as_slice(), [TerminationAttempt::ReturnFailure(_)]));
    let before = transition_rows(&fixture);
    let mut wrong_child_source = plan.clone();
    let TerminationAttempt::ReturnFailure(source) = &mut wrong_child_source.termination_attempts[0]
        else { panic!("the failed return must be factual") };
    source.source_event_id = Uuid::new_v4().to_string();
    assert!(repository::deliver_message(&fixture.db, &prepared, &wrong_child_source, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut wrong_parent = plan.clone();
    let TerminationAttempt::ReturnFailure(source) = &mut wrong_parent.termination_attempts[0]
        else { panic!("the failed return must be factual") };
    source.parent_token_id = Uuid::new_v4().to_string();
    assert!(repository::deliver_message(&fixture.db, &prepared, &wrong_parent, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut wrong_cutoff = plan.clone();
    let completion_index = wrong_cutoff.events.iter().position(|event|
        event.kind == "receive_task_completed").unwrap();
    let mapped = wrong_cutoff.variable_effects.iter_mut().find(|effect|
        matches!(effect, VariableEffect::Mapped { node_id, .. } if node_id == "Catch_1")).unwrap();
    let VariableEffect::Mapped { event_index, .. } = mapped else { unreachable!() };
    assert_ne!(*event_index, completion_index);
    *event_index = completion_index;
    let error = repository::deliver_message(&fixture.db, &prepared, &wrong_cutoff, at_ms)
        .unwrap_err();
    assert!(format!("{error:#}").contains("mapped message output has no exact source-time delivered envelope"));
    assert_eq!(transition_rows(&fixture), before);
    repository::deliver_message(&fixture.db, &prepared, &plan, at_ms).unwrap().unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let actual = repository::runtime_snapshot(&reopened, &fixture.owner,
        &receiver.instance_id).unwrap();
    assert_eq!(actual.instance.status, ProcessInstanceStatus::Incident);
    let TerminationAttempt::ReturnFailure(source) = &plan.termination_attempts[0]
        else { unreachable!() };
    assert_eq!(actual.scopes.iter().find(|scope| scope.scope_id == source.source_scope_id)
        .unwrap().status, ProcessInstanceStatus::Incident);
    assert!(actual.tokens.iter().any(|token| token.token_id == source.parent_token_id
        && token.status == "waiting"));
    assert!(actual.tokens.iter().any(|token| token.token_id == source.waiting_token_id
        && token.scope_id == source.source_scope_id && token.status == "waiting"));
    assert_eq!(actual.incidents.iter().filter(|incident|
        incident.incident_id == source.incident_id && incident.code == "SCOPE_RETURN_ERROR").count(), 1);
    let events = repository::list_events(&reopened, &fixture.owner,
        &receiver.instance_id, 0, 200).unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "message_delivered").count(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "receive_task_completed").count(), 1);
    assert!(!events.iter().any(|event| event.kind == "terminate_end_reached"));
    let source_events = events.iter().filter(|event| event.kind == "incident"
        && event.event_id == source.source_event_id
        && event.data["source_kind"] == "terminate_end_return_failure"
        && event.data["parent_token_id"] == source.parent_token_id
        && event.data["source_event_id"] == source.source_event_id).collect::<Vec<_>>();
    assert_eq!(source_events.len(), 1);
    assert!(repository::deliver_message(&reopened, &prepared, &plan, at_ms).unwrap().is_none());
}

// ============ File: send_boundary_proof_tests.rs — Factual pending Send action and outbox proofs ============

use super::messages::test_support::start_version;
use super::repository::{self, AcceptedInputRef, ProcessPlanInput, RuntimePlan};
use super::runtime::{
    self,
    test_support::{publish_model, Fixture},
};
use uuid::Uuid;

#[test]
fn noninterrupting_timer_script_fact_requires_its_fenced_timer_and_occurrence() {
    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let source = publish_model(&fixture,
        &super::send_boundary_tests::noninterrupting_timer_script_send_model(
            &target.definition_id, &receiver.instance_id));
    let pending = start_version(&fixture, &source);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &pending.instance_id).unwrap();
    let timer = snapshot.timers.iter().find(|timer| timer.node_id == "SendTimer").unwrap();
    let due = timer.due_at_ms.unwrap() + 1;
    let candidate = repository::due_timers(&fixture.db, due, 32).unwrap().into_iter()
        .find(|candidate| candidate.timer_id == timer.timer_id).unwrap();
    let timer_snapshot = repository::timer_snapshot(&fixture.db, &candidate).unwrap();
    let plan = super::timers::plan_timer_fire(&timer_snapshot, due, None, None).unwrap();
    assert!(plan.termination_attempts.is_empty());
    assert!(plan.events.iter().any(|event| event.kind == "script_completed"));
    let fired = plan.events.iter().position(|event| event.kind == "timer_fired").unwrap();
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    for (case, forged) in [
        ("foreign timer", {
            let mut forged = plan.clone();
            forged.events[fired].data["timer_id"] =
                serde_json::json!(Uuid::new_v4().to_string());
            forged
        }),
        ("wrong occurrence", {
            let mut forged = plan.clone();
            forged.events[fired].data["occurrence"] =
                serde_json::json!(candidate.occurrence + 1);
            forged
        }),
        ("duplicate timer fact", {
            let mut forged = plan.clone();
            forged.events.push(plan.events[fired].clone());
            forged
        }),
    ] {
        let rejected = repository::fire_timer(&fixture.db, &candidate, &fixture.owner,
            Some(snapshot.instance.revision), ProcessPlanInput::Supplied(&forged), due)
            .unwrap_err();
        assert!(!format!("{rejected:#}").is_empty(), "{case} lacked a rejection reason");
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before,
            "{case} changed durable process rows");
    }
    let mut stale_revision = candidate.clone();
    stale_revision.revision += 1;
    assert!(repository::fire_timer(&fixture.db, &stale_revision, &fixture.owner,
        Some(snapshot.instance.revision), ProcessPlanInput::Supplied(&plan), due)
        .unwrap().is_none());
    let mut stale_occurrence = candidate.clone();
    stale_occurrence.occurrence += 1;
    assert!(repository::fire_timer(&fixture.db, &stale_occurrence, &fixture.owner,
        Some(snapshot.instance.revision), ProcessPlanInput::Supplied(&plan), due)
        .unwrap().is_none());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let committed = repository::fire_timer(&reopened, &candidate, &fixture.owner,
        Some(snapshot.instance.revision), ProcessPlanInput::Supplied(&plan), due)
        .unwrap().unwrap();
    assert_eq!(committed.instance.status,
        tentaflow_protocol::processes::ProcessInstanceStatus::Waiting);
    assert!(repository::fire_timer(&reopened, &candidate, &fixture.owner,
        Some(snapshot.instance.revision), ProcessPlanInput::Supplied(&plan), due)
        .unwrap().is_none());
}

fn pending_send_plan(fixture: &Fixture, instance_id: &str, at_ms: i64) -> (String, RuntimePlan) {
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, instance_id).unwrap();
    let waiting = snapshot
        .tokens
        .iter()
        .find(|token| token.node_id == "Send_1" && token.status == "waiting")
        .unwrap();
    let events = repository::list_events(&fixture.db, &fixture.owner, instance_id, 0, 200)
        .unwrap()
        .0;
    let pending = events
        .iter()
        .find(|event| event.kind == "send_task_pending")
        .unwrap();
    let input = AcceptedInputRef::SendAdmission {
        instance_id: instance_id.into(),
        scope_id: waiting.scope_id.clone(),
        node_id: waiting.node_id.clone(),
        pending_token_id: waiting.token_id.clone(),
        pending_event_id: pending.event_id.clone(),
        expected_instance_revision: snapshot.instance.revision,
    };
    let plan = runtime::plan_send_admission(
        &snapshot,
        &waiting.token_id,
        &pending.event_id,
        at_ms,
        input,
        None,
        None,
    )
    .unwrap();
    (waiting.token_id.clone(), plan)
}

#[test]
fn pending_send_rejects_forged_outbox_source_and_boundary_closure_before_canonical_commit() {
    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let source = publish_model(
        &fixture,
        &super::send_boundary_tests::timer_bound_send_model(
            &target.definition_id,
            &receiver.instance_id,
        ),
    );
    let sender = start_version(&fixture, &source);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let (waiting_id, plan) = pending_send_plan(&fixture, &sender.instance_id, at_ms);
    let admitted = plan
        .events
        .iter()
        .position(|event| event.kind == "send_task_admitted")
        .unwrap();
    assert_eq!(plan.create_messages.len(), 1);
    assert_eq!(plan.timer_updates.len(), 1);
    let mut cases: Vec<(&str, RuntimePlan)> = Vec::new();
    let mut wrong_payload = plan.clone();
    wrong_payload.create_messages[0].message.payload = serde_json::json!({"value": 43});
    cases.push(("changed observed payload", wrong_payload));
    let mut wrong_source = plan.clone();
    wrong_source.create_messages[0].source_activation_id = Uuid::new_v4().to_string();
    cases.push(("unrelated activation", wrong_source));
    let mut wrong_index = plan.clone();
    wrong_index.create_messages[0].source_event_index += 1;
    cases.push(("wrong admission index", wrong_index));
    let mut missing_action = plan.clone();
    missing_action.event_sources.remove(&admitted);
    cases.push(("missing action source", missing_action));
    let mut wrong_event = plan.clone();
    wrong_event.events[admitted].data["message_id"] = serde_json::json!(Uuid::new_v4().to_string());
    cases.push(("changed event message identity", wrong_event));
    let mut missing_outbox = plan.clone();
    missing_outbox.create_messages.clear();
    cases.push(("omitted outbox admission", missing_outbox));
    let mut missing_disarm = plan.clone();
    missing_disarm.timer_updates.clear();
    cases.push(("missing boundary disarm", missing_disarm));
    let mut duplicate_successor = plan.clone();
    duplicate_successor
        .create_tokens
        .push(plan.create_tokens.last().unwrap().clone());
    cases.push(("duplicate successor", duplicate_successor));
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    for (case, forged) in cases {
        let rejected = repository::admit_pending_send(
            &fixture.db,
            &waiting_id,
            at_ms,
            ProcessPlanInput::Supplied(&forged),
        )
        .unwrap_err();
        assert!(
            !format!("{rejected:#}").is_empty(),
            "{case} lacked a rejection reason"
        );
        assert_eq!(
            super::signal_proof_tests::all_transition_rows(&fixture),
            before,
            "{case} changed durable process rows"
        );
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let actual = repository::admit_pending_send(
        &reopened,
        &waiting_id,
        at_ms,
        ProcessPlanInput::Supplied(&plan),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        actual.instance.status,
        tentaflow_protocol::processes::ProcessInstanceStatus::Completed
    );
    assert!(repository::admit_pending_send(
        &reopened,
        &waiting_id,
        at_ms,
        ProcessPlanInput::Supplied(&plan)
    )
    .unwrap()
    .is_none());
}

#[test]
fn pending_send_entry_requires_one_source_event_one_wait_and_its_exact_boundary_arm() {
    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let source = publish_model(
        &fixture,
        &super::send_boundary_tests::timer_bound_send_model(
            &target.definition_id,
            &receiver.instance_id,
        ),
    );
    let instance_id = Uuid::new_v4().to_string();
    let at_ms = chrono::Utc::now().timestamp_millis();
    let command = runtime::test_support::stamp("start pending Send proof");
    let variables = serde_json::to_value(&source.model.variables).unwrap();
    let plan = runtime::plan_start(
        &source.model, &source.model.process_id, crate::processes::runtime::test_support::ordinary_start_id(&source.model),
        &instance_id,
        &fixture.owner,
        &source.definition_id,
        source.version,
        variables.clone(),
        runtime::StartCause::Manual,
        at_ms,
        runtime::test_support::manual_input(&command),
        None,
    )
    .unwrap();
    let pending = plan
        .events
        .iter()
        .position(|event| event.kind == "send_task_pending")
        .unwrap();
    assert_eq!(plan.create_messages.len(), 0);
    assert_eq!(plan.create_timers.len(), 1);
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let mut cases: Vec<(&str, RuntimePlan)> = Vec::new();
    let mut no_pending = plan.clone();
    no_pending.events.remove(pending);
    cases.push(("omitted immutable pending event", no_pending));
    let mut wrong_source = plan.clone();
    wrong_source
        .event_sources
        .insert(pending, Uuid::new_v4().to_string());
    cases.push(("foreign pending source", wrong_source));
    let mut wrong_wait = plan.clone();
    wrong_wait.events[pending].data["pending_token_id"] =
        serde_json::json!(Uuid::new_v4().to_string());
    cases.push(("foreign pending wait", wrong_wait));
    let mut no_arm = plan.clone();
    no_arm.create_timers.clear();
    cases.push(("omitted timer arm", no_arm));
    let mut duplicate_pending = plan.clone();
    duplicate_pending.events.push(plan.events[pending].clone());
    cases.push(("duplicate pending fact", duplicate_pending));
    for (case, forged) in cases {
        let rejected = repository::start_instance(
            &fixture.db,
            &fixture.owner,
            &command,
            &instance_id,
            &source.definition_id,
            source.version,
            &variables, None, None,
            ProcessPlanInput::Supplied(&forged),
            at_ms,
        )
        .unwrap_err();
        assert!(
            !format!("{rejected:#}").is_empty(),
            "{case} lacked a rejection reason"
        );
        assert_eq!(
            super::signal_proof_tests::all_transition_rows(&fixture),
            before,
            "{case} changed durable process rows"
        );
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let committed = repository::start_instance(
        &reopened,
        &fixture.owner,
        &command,
        &instance_id,
        &source.definition_id,
        source.version,
        &variables, None, None,
        ProcessPlanInput::Supplied(&plan),
        at_ms,
    )
    .unwrap();
    assert_eq!(
        committed.status,
        tentaflow_protocol::processes::ProcessInstanceStatus::Waiting
    );
    let replay = repository::start_instance(
        &reopened,
        &fixture.owner,
        &command,
        &instance_id,
        &source.definition_id,
        source.version,
        &variables, None, None,
        ProcessPlanInput::Supplied(&plan),
        at_ms,
    )
    .unwrap();
    assert_eq!(replay.instance_id, committed.instance_id);
}

#[test]
fn finite_send_failure_requires_the_exact_incident_code_pending_source_and_immutable_fact() {
    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let mut model = super::send_boundary_tests::timer_bound_send_model(
        &target.definition_id,
        &receiver.instance_id,
    );
    model.variables.insert("case".into(), serde_json::json!(42));
    let node = model
        .nodes
        .iter_mut()
        .find(|node| node.id == "Send_1")
        .unwrap();
    let tentaflow_protocol::processes::ProcessNodeKind::SendTask {
        correlation_expression,
        ..
    } = &mut node.kind
    else {
        panic!("pinned SendTask")
    };
    *correlation_expression = "vars.case".into();
    let source = publish_model(&fixture, &model);
    let instance = start_version(&fixture, &source);
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &instance.instance_id).unwrap();
    let wait = snapshot
        .tokens
        .iter()
        .find(|token| token.node_id == "Send_1" && token.status == "waiting")
        .unwrap();
    let persisted =
        repository::list_events(&fixture.db, &fixture.owner, &instance.instance_id, 0, 200)
            .unwrap()
            .0;
    let pending = persisted
        .iter()
        .find(|event| event.kind == "send_task_pending")
        .unwrap();
    let pinned = source
        .model
        .nodes
        .iter()
        .find(|node| node.id == "Send_1")
        .unwrap();
    let error = super::messages::prepare_throw(&source.model, pinned, &snapshot.instance.variables, &[])
        .unwrap_err();
    let reason = repository::bounded_failure_message(&error.to_string());
    let input = AcceptedInputRef::SendAdmission {
        instance_id: instance.instance_id.clone(),
        scope_id: wait.scope_id.clone(),
        node_id: wait.node_id.clone(),
        pending_token_id: wait.token_id.clone(),
        pending_event_id: pending.event_id.clone(),
        expected_instance_revision: snapshot.instance.revision,
    };
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_send_admission(
        &snapshot,
        &wait.token_id,
        &pending.event_id,
        at_ms,
        input,
        Some(runtime::SendAdmissionFailure {
            code: "MESSAGE_EXPRESSION_ERROR",
            reason,
        }),
        None,
    )
    .unwrap();
    let failure_index = plan
        .events
        .iter()
        .position(|event| event.kind == "send_admission_failed")
        .unwrap();
    assert_eq!(plan.create_messages.len(), 0);
    let mut cases: Vec<(&str, RuntimePlan)> = Vec::new();
    let mut wrong_code = plan.clone();
    wrong_code.events[failure_index].data["code"] =
        serde_json::json!("SEND_ADMISSION_TARGET_UNAVAILABLE");
    cases.push(("invented finite code", wrong_code));
    let mut wrong_pending = plan.clone();
    wrong_pending.events[failure_index].data["pending_event_id"] =
        serde_json::json!(Uuid::new_v4().to_string());
    cases.push(("foreign pending event", wrong_pending));
    let mut wrong_incident = plan.clone();
    wrong_incident.events[failure_index].data["incident_id"] =
        serde_json::json!(Uuid::new_v4().to_string());
    cases.push(("foreign incident identity", wrong_incident));
    let mut extra_incident = plan.clone();
    let incident = extra_incident
        .events
        .iter()
        .find(|event| event.kind == "incident")
        .unwrap()
        .clone();
    extra_incident.events.push(incident);
    cases.push(("duplicate incident fact", extra_incident));
    let mut missing_failure = plan.clone();
    missing_failure.events.remove(failure_index);
    cases.push(("omitted immutable failure fact", missing_failure));
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    for (case, forged) in cases {
        let rejected = repository::admit_pending_send(
            &fixture.db,
            &wait.token_id,
            at_ms,
            ProcessPlanInput::Supplied(&forged),
        )
        .unwrap_err();
        assert!(
            !format!("{rejected:#}").is_empty(),
            "{case} lacked a rejection reason"
        );
        assert_eq!(
            super::signal_proof_tests::all_transition_rows(&fixture),
            before,
            "{case} changed durable process rows"
        );
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let accepted = repository::admit_pending_send(
        &reopened,
        &wait.token_id,
        at_ms,
        ProcessPlanInput::Canonical,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        accepted.instance.status,
        tentaflow_protocol::processes::ProcessInstanceStatus::Incident
    );
    assert!(repository::admit_pending_send(
        &reopened,
        &wait.token_id,
        at_ms,
        ProcessPlanInput::Canonical
    )
    .unwrap()
    .is_none());
}

#[test]
fn nonexistent_factual_subscription_aborts_without_minting_a_finite_send_failure() {
    use tentaflow_protocol::processes::{ProcessMessageTargetSpec, ProcessNodeKind};

    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let mut model = super::send_boundary_tests::timer_bound_send_model(
        &target.definition_id,
        &receiver.instance_id,
    );
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
    *address = ProcessMessageTargetSpec::Catch {
        definition_id: target.definition_id.clone(),
        instance_id_expression: Some(serde_json::to_string(&receiver.instance_id).unwrap()),
        subscription_id_expression: Some(
            serde_json::to_string(&Uuid::new_v4().to_string()).unwrap(),
        ),
    };
    let source = publish_model(&fixture, &model);
    let pending = start_version(&fixture, &source);
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &pending.instance_id).unwrap();
    let waiting = snapshot
        .tokens
        .iter()
        .find(|token| token.node_id == "Send_1" && token.status == "waiting")
        .unwrap();
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let rejected = repository::admit_pending_send(
        &fixture.db,
        &waiting.token_id,
        chrono::Utc::now().timestamp_millis(),
        ProcessPlanInput::Canonical,
    )
    .unwrap_err();
    assert!(format!("{rejected:#}").contains("subscription"),
        "missing subscription should be a failed query, not an invented finite reason: {rejected:#}");
    assert_eq!(
        super::signal_proof_tests::all_transition_rows(&fixture),
        before
    );
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert_eq!(
        super::signal_proof_tests::all_transition_rows(&fixture),
        before
    );
    let still_waiting =
        repository::runtime_snapshot(&reopened, &fixture.owner, &pending.instance_id).unwrap();
    assert!(still_waiting
        .tokens
        .iter()
        .any(|token| token.token_id == waiting.token_id && token.status == "waiting"));
}

#[test]
fn embedded_pending_send_rejects_foreign_scope_and_boundary_facts_before_canonical_commit() {
    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let model = super::runtime::test_support::embedded_model(
        super::send_boundary_tests::timer_bound_send_model(
            &target.definition_id, &receiver.instance_id), "InnerScope");
    let source = publish_model(&fixture, &model);
    let instance = start_version(&fixture, &source);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let (waiting_id, plan) = pending_send_plan(&fixture, &instance.instance_id, at_ms);
    let child_scope = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &instance.instance_id).unwrap().tokens.into_iter()
        .find(|token| token.token_id == waiting_id).unwrap().scope_id;
    assert_ne!(child_scope, instance.instance_id);
    let admitted = plan.events.iter().position(|event|
        event.kind == "send_task_admitted").unwrap();
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let mut cases = Vec::new();
    let mut foreign_event_scope = plan.clone();
    foreign_event_scope.events[admitted].scope_id = instance.instance_id.clone();
    cases.push(("foreign admission scope", foreign_event_scope));
    let mut foreign_outbox_scope = plan.clone();
    foreign_outbox_scope.create_messages[0].source_scope_id = instance.instance_id.clone();
    cases.push(("foreign outbox source scope", foreign_outbox_scope));
    let mut moved_source = plan.clone();
    moved_source.event_sources.insert(admitted, Uuid::new_v4().to_string());
    cases.push(("foreign child activation", moved_source));
    let mut omitted_child_arm = plan.clone();
    omitted_child_arm.timer_updates.clear();
    cases.push(("omitted child boundary closure", omitted_child_arm));
    let mut foreign_successor = plan.clone();
    let successor = foreign_successor.create_tokens.iter_mut().find(|token|
        token.status == "ready").unwrap();
    successor.scope_id = instance.instance_id.clone();
    cases.push(("foreign successor scope", foreign_successor));
    for (name, forged) in cases {
        let error = repository::admit_pending_send(&fixture.db, &waiting_id, at_ms,
            ProcessPlanInput::Supplied(&forged)).unwrap_err();
        assert!(!format!("{error:#}").is_empty(), "{name} has no specific rejection");
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before,
            "{name} changed factual rows");
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let committed = repository::admit_pending_send(&reopened, &waiting_id, at_ms,
        ProcessPlanInput::Canonical).unwrap().unwrap();
    assert_eq!(committed.instance.status,
        tentaflow_protocol::processes::ProcessInstanceStatus::Completed);
    assert!(repository::admit_pending_send(&reopened, &waiting_id, at_ms,
        ProcessPlanInput::Canonical).unwrap().is_none());
}

#[test]
fn called_pending_send_rejects_foreign_child_source_before_actual_parent_return() {
    use std::collections::BTreeMap;

    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let called = publish_model(&fixture,
        &super::send_boundary_tests::timer_bound_send_model(
            &target.definition_id, &receiver.instance_id));
    let caller = publish_model(&fixture,
        &super::call_tests::caller(&called, BTreeMap::new()));
    let parent = start_version(&fixture, &caller);
    let child_id = super::call_tests::child_id(&fixture, &parent.instance_id);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let (waiting_id, plan) = pending_send_plan(&fixture, &child_id, at_ms);
    let admitted = plan.events.iter().position(|event|
        event.kind == "send_task_admitted").unwrap();
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let mut cases = Vec::new();
    let mut wrong_source = plan.clone();
    wrong_source.event_sources.insert(admitted, Uuid::new_v4().to_string());
    cases.push(("foreign child source", wrong_source));
    let mut wrong_token = plan.clone();
    wrong_token.consume_token_ids[0] = Uuid::new_v4().to_string();
    cases.push(("foreign child wait", wrong_token));
    let mut missing_disarm = plan.clone();
    missing_disarm.timer_updates.clear();
    cases.push(("missing called child boundary closure", missing_disarm));
    for (name, forged) in cases {
        let error = repository::admit_pending_send(&fixture.db, &waiting_id, at_ms,
            ProcessPlanInput::Supplied(&forged)).unwrap_err();
        assert!(!format!("{error:#}").is_empty(), "{name} has no rejection");
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before,
            "{name} changed factual rows");
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let committed = repository::admit_pending_send(&reopened, &waiting_id, at_ms,
        ProcessPlanInput::Canonical).unwrap().unwrap();
    assert_eq!(committed.instance.status,
        tentaflow_protocol::processes::ProcessInstanceStatus::Completed);
    assert_eq!(repository::get_instance(&reopened, &fixture.owner,
        &parent.instance_id, None).unwrap().status,
        tentaflow_protocol::processes::ProcessInstanceStatus::Completed);
    assert!(repository::admit_pending_send(&reopened, &waiting_id, at_ms,
        ProcessPlanInput::Canonical).unwrap().is_none());
}

#[test]
fn called_embedded_send_rejects_foreign_grandchild_facts_before_pinned_parent_return() {
    use std::collections::BTreeMap;

    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let called = publish_model(&fixture,
        &super::send_boundary_tests::called_embedded_send_model(
            &target.definition_id, &receiver.instance_id, false));
    let caller = publish_model(&fixture,
        &super::call_tests::caller(&called, BTreeMap::new()));
    let parent = start_version(&fixture, &caller);
    let child_id = super::call_tests::child_id(&fixture, &parent.instance_id);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let (waiting_id, plan) = pending_send_plan(&fixture, &child_id, at_ms);
    let child = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    let scope_id = child.tokens.iter().find(|token| token.token_id == waiting_id)
        .unwrap().scope_id.clone();
    assert_ne!(scope_id, child_id);
    let admission = plan.events.iter().position(|event|
        event.kind == "send_task_admitted").unwrap();
    assert_eq!(plan.events[admission].scope_id, scope_id);
    assert_eq!(plan.create_messages.len(), 1);
    let mut cases = Vec::new();
    let mut foreign_scope = plan.clone();
    foreign_scope.events[admission].scope_id = child_id.clone();
    cases.push(("parent scope substituted for grandchild", foreign_scope));
    let mut foreign_outbox_scope = plan.clone();
    foreign_outbox_scope.create_messages[0].source_scope_id = child_id.clone();
    cases.push(("parent scope substituted for outbox source", foreign_outbox_scope));
    let mut foreign_activation = plan.clone();
    foreign_activation.event_sources.insert(admission, Uuid::new_v4().to_string());
    cases.push(("foreign activation under valid Call", foreign_activation));
    let mut foreign_wait = plan.clone();
    foreign_wait.consume_token_ids[0] = Uuid::new_v4().to_string();
    cases.push(("foreign nested pending wait", foreign_wait));
    let mut missing_arm = plan.clone();
    missing_arm.timer_updates.clear();
    cases.push(("missing nested boundary closure", missing_arm));
    let mut foreign_successor = plan.clone();
    foreign_successor.create_tokens.iter_mut().find(|token| token.status == "ready")
        .unwrap().scope_id = child_id.clone();
    cases.push(("foreign successor scope", foreign_successor));
    let mut wrong_outbox_source = plan.clone();
    wrong_outbox_source.create_messages[0].source_activation_id = Uuid::new_v4().to_string();
    cases.push(("foreign durable outbox source", wrong_outbox_source));
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    for (name, forged) in cases {
        let error = repository::admit_pending_send(&fixture.db, &waiting_id, at_ms,
            ProcessPlanInput::Supplied(&forged)).unwrap_err();
        assert!(!format!("{error:#}").is_empty(), "{name} had no rejection reason");
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before,
            "{name} changed factual Call, child scope or outbox rows");
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let committed = repository::admit_pending_send(&reopened, &waiting_id, at_ms,
        ProcessPlanInput::Canonical).unwrap().unwrap();
    assert_eq!(committed.instance.status,
        tentaflow_protocol::processes::ProcessInstanceStatus::Completed);
    assert_eq!(repository::get_instance(&reopened, &fixture.owner,
        &parent.instance_id, None).unwrap().status,
        tentaflow_protocol::processes::ProcessInstanceStatus::Completed);
    let events = repository::list_events(&reopened, &fixture.owner, &child_id, 0, 200).unwrap().0;
    let admitted = events.iter().find(|event| event.kind == "send_task_admitted").unwrap();
    assert_eq!(admitted.scope_id, scope_id);
    let message_id = admitted.data["message_id"].as_str().unwrap();
    assert_eq!(repository::get_message(&reopened, &fixture.owner,
        &fixture.owner.user_id, message_id).unwrap().message.source_instance_id.as_deref(),
        Some(child_id.as_str()));
    let settled = super::signal_proof_tests::all_transition_rows(&fixture);
    assert!(repository::admit_pending_send(&reopened, &waiting_id, at_ms,
        ProcessPlanInput::Canonical).unwrap().is_none());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), settled);
}

#[test]
fn uncaught_called_embedded_error_end_requires_its_exact_terminal_scope_source() {
    use std::collections::BTreeMap;
    use tentaflow_protocol::processes::{
        ProcessErrorDeclaration, ProcessInstanceStatus, ProcessNodeKind, ProcessMessageStatus,
    };

    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &target);
    let mut model = super::send_boundary_tests::called_embedded_send_model(
        &target.definition_id, &receiver.instance_id, false);
    let ProcessNodeKind::SubProcess { body, .. } = &mut model.nodes.iter_mut()
        .find(|node| node.id == "InnerScope").unwrap().kind else { unreachable!() };
    body.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::ErrorEnd { error_ref: "Rejected".into() };
    model.errors.push(ProcessErrorDeclaration {
        error_id: "Rejected".into(), name: "Rejected".into(),
        error_code: "REJECTED".into(),
    });
    let called = publish_model(&fixture, &model);
    let caller = publish_model(&fixture,
        &super::call_tests::caller(&called, BTreeMap::new()));
    let parent = start_version(&fixture, &caller);
    let child_id = super::call_tests::child_id(&fixture, &parent.instance_id);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let (waiting_id, plan) = pending_send_plan(&fixture, &child_id, at_ms);
    let source_index = plan.events.iter().position(|event|
        event.kind == "error_end_reached").unwrap();
    let closure_index = plan.events.iter().position(|event|
        event.kind == "scope_cancelled").unwrap();
    let scope_id = plan.events[closure_index].scope_id.clone();
    assert!(source_index < closure_index);
    assert_eq!(plan.events[source_index].scope_id, scope_id);
    assert_eq!(plan.events[closure_index].data["boundary_id"], "error_end");
    assert_eq!(plan.scope_updates.iter().find(|update|
        update.scope_id == scope_id).unwrap().status, ProcessInstanceStatus::Error);
    assert_eq!(plan.terminal_error.as_ref(), plan.scope_terminal_errors.get(&scope_id));
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let mut cases = Vec::new();
    let mut foreign_source = plan.clone();
    foreign_source.event_sources.insert(source_index, Uuid::new_v4().to_string());
    cases.push(("foreign End activation", foreign_source));
    let mut wrong_event_id = plan.clone();
    wrong_event_id.event_ids.insert(source_index, Uuid::new_v4().to_string());
    cases.push(("foreign ErrorEnd event", wrong_event_id));
    let mut wrong_terminal_scope = plan.clone();
    wrong_terminal_scope.terminal_error.as_mut().unwrap().source_scope_id = child_id.clone();
    cases.push(("foreign terminal scope", wrong_terminal_scope));
    let mut wrong_scope_fact = plan.clone();
    wrong_scope_fact.scope_terminal_errors.get_mut(&scope_id).unwrap().source_event_id =
        Uuid::new_v4().to_string();
    cases.push(("foreign child terminal fact", wrong_scope_fact));
    let mut wrong_source_node = plan.clone();
    wrong_source_node.scope_terminal_errors.get_mut(&scope_id).unwrap().source_node_id =
        "Send_1".into();
    cases.push(("foreign terminal node", wrong_source_node));
    let mut wrong_error_code = plan.clone();
    wrong_error_code.events[source_index].data["error_code"] =
        serde_json::json!("FOREIGN");
    cases.push(("foreign pinned error code", wrong_error_code));
    let mut wrong_closure = plan.clone();
    wrong_closure.events[closure_index].data["boundary_id"] =
        serde_json::json!("foreign boundary");
    cases.push(("foreign closure cause", wrong_closure));
    let mut wrong_parent = plan.clone();
    wrong_parent.events[closure_index].data["parent_token_id"] =
        serde_json::json!(Uuid::new_v4().to_string());
    cases.push(("foreign embedded parent wait", wrong_parent));
    let mut duplicate_closure = plan.clone();
    duplicate_closure.events.push(plan.events[closure_index].clone());
    cases.push(("duplicate scope closure", duplicate_closure));
    let mut wrong_final_status = plan.clone();
    wrong_final_status.scope_updates.iter_mut().find(|update|
        update.scope_id == scope_id).unwrap().status = ProcessInstanceStatus::Cancelled;
    cases.push(("terminal scope status substituted", wrong_final_status));
    for (case, forged) in cases {
        let rejected = repository::admit_pending_send(&fixture.db, &waiting_id,
            at_ms, ProcessPlanInput::Supplied(&forged)).unwrap_err();
        assert!(!format!("{rejected:#}").is_empty(), "{case} lacked a rejection reason");
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before,
            "{case} changed durable process rows");
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let committed = repository::admit_pending_send(&reopened, &waiting_id,
        at_ms, ProcessPlanInput::Canonical).unwrap().unwrap();
    assert_eq!(committed.instance.status, ProcessInstanceStatus::Error);
    let parent_after = repository::get_instance(&reopened, &fixture.owner,
        &parent.instance_id, None).unwrap();
    assert_eq!(parent_after.status, ProcessInstanceStatus::Incident);
    assert_eq!(parent_after.incidents[0].code, "CALL_CHILD_ERROR");
    let history = repository::list_events(&reopened, &fixture.owner, &child_id, 0, 200)
        .unwrap().0;
    assert_eq!(history.iter().filter(|event| event.kind == "error_end_reached"
        && event.scope_id == scope_id).count(), 1);
    assert_eq!(history.iter().filter(|event| event.kind == "scope_cancelled"
        && event.scope_id == scope_id).count(), 1);
    let admitted = history.iter().find(|event|
        event.kind == "send_task_admitted").unwrap();
    let outbox_id = admitted.data["message_id"].as_str().unwrap();
    assert_eq!(repository::get_message(&reopened, &fixture.owner,
        &fixture.owner.user_id, outbox_id).unwrap().message.status,
        ProcessMessageStatus::Pending);
    let drained = super::messages::drain_pending(&reopened, at_ms + 1);
    drained.completion.unwrap();
    assert_eq!(drained.delivered, 1);
    assert_eq!(repository::get_message(&reopened, &fixture.owner,
        &fixture.owner.user_id, outbox_id).unwrap().message.status,
        ProcessMessageStatus::Delivered);
    assert_eq!(repository::get_instance(&reopened, &fixture.owner,
        &receiver.instance_id, None).unwrap().status, ProcessInstanceStatus::Completed);
    let settled = super::signal_proof_tests::all_transition_rows(&fixture);
    assert!(repository::admit_pending_send(&reopened, &waiting_id,
        at_ms, ProcessPlanInput::Canonical).unwrap().is_none());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), settled);
}

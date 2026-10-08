// ============ File: manual_proof_tests.rs — Manual acknowledgment provenance and rollback tests ============

use super::call_tests::transition_rows;
use super::manual_tests::{manual_entry, manual_model, start_manual};
use super::repository;
use super::runtime::{self, test_support::*};
use serde_json::json;
use tentaflow_protocol::processes::{ProcessInstanceStatus, ProcessUserTaskKind};
use uuid::Uuid;

#[test]
fn manual_wait_requires_one_pinned_open_fact_before_any_acknowledgment() {
    let fixture = Fixture::new();
    let model = manual_model(None, false);
    let version = publish_model(&fixture, &model);
    let variables = serde_json::to_value(&model.variables).unwrap();
    let instance_id = Uuid::new_v4().to_string();
    let command = stamp("manual-open-proof");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&version.model, &version.model.process_id, crate::processes::runtime::test_support::ordinary_start_id(&version.model), &instance_id, &fixture.owner,
        &version.definition_id, version.version, variables.clone(), runtime::StartCause::Manual,
        at_ms, manual_input(&command), None).unwrap();
    let opened = plan.events.iter().position(|event| event.kind == "manual_task_opened").unwrap();
    for variant in 0..4 {
        let mut forged = plan.clone();
        match variant {
            0 => { forged.events.remove(opened); }
            1 => { forged.events.push(forged.events[opened].clone()); }
            2 => { forged.events[opened].data["assignee_user_id"] =
                json!(fixture.participant.user_id); }
            3 => { forged.create_user_tasks[0].outputs = json!({"fabricated":true}); }
            _ => unreachable!(),
        }
        let before = transition_rows(&fixture);
        assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
            &instance_id, &version.definition_id, version.version, &variables, None, None,
            repository::ProcessPlanInput::Supplied(&forged), at_ms).is_err());
        assert_eq!(transition_rows(&fixture), before,
            "manual open forgery {variant} changed durable rows");
    }
    repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    let reopened = repository::get_instance(&fixture.db, &fixture.owner, &instance_id, None).unwrap();
    assert_eq!(reopened.user_tasks.iter().filter(|task|
        task.kind == ProcessUserTaskKind::Manual).count(), 1);
}

fn assert_forged_acknowledgments_roll_back(terminate: bool) {
    let fixture = Fixture::new();
    let model = manual_model(None, terminate);
    let (instance_id, _, _) = start_manual(&fixture, &model);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &instance_id).unwrap();
    let task = snapshot.user_tasks.iter().find(|task| task.kind == ProcessUserTaskKind::Manual).unwrap();
    let command = stamp("manual-proof");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_manual_acknowledgment(&snapshot, &task.user_task_id,
        &fixture.owner.user_id, at_ms,
        manual_entry(&snapshot, &task.user_task_id, &command), None).unwrap();
    let ack_index = plan.events.iter().position(|event|
        event.kind == "manual_task_acknowledged").unwrap();
    let actual_token = task.token_id.as_ref().unwrap();
    for variant in 0..8 {
        let mut forged = plan.clone();
        match variant {
            0 => forged.events[ack_index].data["user_task_id"] = json!(Uuid::new_v4().to_string()),
            1 => forged.events[ack_index].data["acknowledged_by_user_id"] = json!(fixture.participant.user_id),
            2 => forged.events[ack_index].scope_id = Uuid::new_v4().to_string(),
            3 => forged.events[ack_index].node_id = Some("Start_1".into()),
            4 => forged.events.push(forged.events[ack_index].clone()),
            5 => forged.complete_user_task_ids.push(task.user_task_id.clone()),
            6 => forged.consume_token_ids.retain(|id| id != actual_token),
            7 => {
                let successor = forged.create_tokens.iter_mut().find(|token|
                    token.node_id == "End_1").unwrap();
                successor.node_id = "Manual".into();
            }
            _ => unreachable!(),
        }
        let before = transition_rows(&fixture);
        let error = repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
            &command, &instance_id, &task.user_task_id, snapshot.instance.revision,
            repository::ProcessPlanInput::Supplied(&forged), at_ms).unwrap_err();
        assert!(!format!("{error:#}").is_empty());
        assert_eq!(transition_rows(&fixture), before,
            "manual forgery {variant} changed durable rows");
    }
    let committed = repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
        &command, &instance_id, &task.user_task_id, snapshot.instance.revision,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    assert_eq!(committed.instance.status, ProcessInstanceStatus::Completed);
    let reopened = repository::get_instance(&fixture.db, &fixture.owner, &instance_id, None).unwrap();
    assert_eq!(reopened.status, ProcessInstanceStatus::Completed);
    let before_replay = transition_rows(&fixture);
    repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
        &command, &instance_id, &task.user_task_id, snapshot.instance.revision,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    assert_eq!(transition_rows(&fixture), before_replay);
}

#[test]
fn ordinary_manual_acknowledgment_rejects_forged_facts_with_full_rollback() {
    assert_forged_acknowledgments_roll_back(false);
}

#[test]
fn terminating_manual_acknowledgment_rejects_forged_facts_with_full_rollback() {
    assert_forged_acknowledgments_roll_back(true);
}

#[test]
fn manual_acknowledgment_disarms_only_its_factual_boundary_before_commit() {
    let fixture = Fixture::new();
    let model = super::manual_tests::manual_boundary_model(false, true);
    let (instance_id, _, _) = start_manual(&fixture, &model);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &instance_id).unwrap();
    let task = snapshot.user_tasks.iter().find(|task| task.kind == ProcessUserTaskKind::Manual).unwrap();
    let timer = snapshot.timers.iter().find(|timer| timer.node_id == "ManualTimer").unwrap();
    let command = stamp("manual boundary proof");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_manual_acknowledgment(&snapshot, &task.user_task_id,
        &fixture.owner.user_id, at_ms,
        manual_entry(&snapshot, &task.user_task_id, &command), None).unwrap();
    assert!(plan.timer_updates.iter().any(|update| update.timer_id == timer.timer_id));
    let cancellation = plan.events.iter().position(|event|
        event.kind == "timer_cancelled" && event.data["timer_id"] == timer.timer_id).unwrap();
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    for (case, forged) in [
        ("omitted disarm", {
            let mut plan = plan.clone();
            plan.timer_updates.clear();
            plan
        }),
        ("foreign timer", {
            let mut plan = plan.clone();
            plan.events[cancellation].data["timer_id"] = json!(Uuid::new_v4().to_string());
            plan
        }),
        ("foreign attached token", {
            let mut plan = plan.clone();
            plan.events[cancellation].data["attached_token_id"] =
                json!(Uuid::new_v4().to_string());
            plan
        }),
        ("missing cancellation event", {
            let mut plan = plan.clone();
            plan.events.remove(cancellation);
            plan
        }),
        ("wrong acknowledged task", {
            let mut plan = plan.clone();
            plan.events.iter_mut().find(|event| event.kind == "manual_task_acknowledged")
                .unwrap().data["user_task_id"] = json!(Uuid::new_v4().to_string());
            plan
        }),
    ] {
        assert!(repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
            &command, &instance_id, &task.user_task_id, snapshot.instance.revision,
            repository::ProcessPlanInput::Supplied(&forged), at_ms).is_err(), "{case}");
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before, "{case}");
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    repository::acknowledge_manual_task(&reopened, &fixture.owner,
        &command, &instance_id, &task.user_task_id, snapshot.instance.revision,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    let committed = super::signal_proof_tests::all_transition_rows(&fixture);
    repository::acknowledge_manual_task(&reopened, &fixture.owner,
        &command, &instance_id, &task.user_task_id, snapshot.instance.revision,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), committed);
}

#[test]
fn interrupting_manual_timer_requires_exact_wait_cancellation_and_source() {
    let fixture = Fixture::new();
    let model = super::manual_tests::manual_boundary_model(false, true);
    let (instance_id, _, _) = start_manual(&fixture, &model);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &instance_id).unwrap();
    let task = snapshot.user_tasks.iter().find(|task| task.kind == ProcessUserTaskKind::Manual).unwrap();
    let timer = snapshot.timers.iter().find(|timer| timer.node_id == "ManualTimer").unwrap();
    let at_ms = timer.due_at_ms.unwrap() + 1;
    let candidate = repository::due_timers(&fixture.db, at_ms, 32).unwrap().into_iter()
        .find(|row| row.timer_id == timer.timer_id).unwrap();
    let selected = repository::timer_snapshot(&fixture.db, &candidate).unwrap();
    let plan = super::timers::plan_timer_fire(&selected, at_ms, None, None).unwrap();
    assert!(plan.cancel_user_task_ids.contains(&task.user_task_id));
    let fired = plan.events.iter().position(|event| event.kind == "timer_fired").unwrap();
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    for (case, forged) in [
        ("missing Manual cancellation", {
            let mut plan = plan.clone();
            plan.cancel_user_task_ids.clear();
            plan
        }),
        ("foreign Manual cancellation", {
            let mut plan = plan.clone();
            plan.cancel_user_task_ids[0] = Uuid::new_v4().to_string();
            plan
        }),
        ("wrong timer fact", {
            let mut plan = plan.clone();
            plan.events[fired].data["timer_id"] = json!(Uuid::new_v4().to_string());
            plan
        }),
        ("wrong attached scope", {
            let mut plan = plan.clone();
            plan.events[fired].scope_id = Uuid::new_v4().to_string();
            plan
        }),
        ("foreign source activation", {
            let mut plan = plan.clone();
            plan.event_sources.insert(fired, Uuid::new_v4().to_string());
            plan
        }),
        ("extra Manual acknowledgment", {
            let mut plan = plan.clone();
            plan.complete_user_task_ids.push(task.user_task_id.clone());
            plan
        }),
    ] {
        assert!(repository::fire_timer(&fixture.db, &candidate, &fixture.owner,
            Some(snapshot.instance.revision), repository::ProcessPlanInput::Supplied(&forged),
            at_ms).is_err(), "{case}");
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before, "{case}");
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    repository::fire_timer(&reopened, &candidate, &fixture.owner,
        Some(snapshot.instance.revision), repository::ProcessPlanInput::Supplied(&plan),
        at_ms).unwrap().unwrap();
    let committed = super::signal_proof_tests::all_transition_rows(&fixture);
    assert!(repository::fire_timer(&reopened, &candidate, &fixture.owner,
        Some(snapshot.instance.revision), repository::ProcessPlanInput::Supplied(&plan),
        at_ms).unwrap().is_none());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), committed);
}

#[test]
fn interrupting_manual_message_requires_exact_accepted_envelope_and_wait() {
    use super::messages::{self, test_support::{catch_target, envelope, send}};
    use super::repository::MessageSelection;

    let fixture = Fixture::new();
    let model = super::manual_tests::manual_boundary_model(true, true);
    let (instance_id, version, _) = start_manual(&fixture, &model);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &instance_id).unwrap();
    let task = snapshot.user_tasks.iter().find(|task| task.kind == ProcessUserTaskKind::Manual).unwrap();
    let boundary = snapshot.subscriptions.iter().find(|sub|
        sub.node_id == "ManualMessage").unwrap();
    let envelope = envelope(catch_target(&version, Some(&instance_id),
        Some(&boundary.subscription_id)), json!({"physical":true}));
    let sent = send(&fixture, &envelope);
    let at_ms = sent.received_at_ms;
    let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap().into_iter()
        .find(|row| row.key.message_id == envelope.message_id).unwrap();
    let MessageSelection::Ready(prepared) =
        repository::message_snapshot(&fixture.db, &candidate).unwrap()
        else { panic!("the actual Manual boundary must be selected") };
    let plan = messages::plan_message_delivery(&prepared, at_ms, None).unwrap();
    assert!(plan.cancel_user_task_ids.contains(&task.user_task_id));
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    for (case, forged) in [
        ("omitted Manual cancellation", {
            let mut plan = plan.clone();
            plan.cancel_user_task_ids.clear();
            plan
        }),
        ("wrong cancelled task", {
            let mut plan = plan.clone();
            plan.cancel_user_task_ids[0] = Uuid::new_v4().to_string();
            plan
        }),
        ("wrong envelope UUID", {
            let mut plan = plan.clone();
            plan.events.iter_mut().find(|event| event.kind == "message_delivered")
                .unwrap().data["message_id"] = json!(Uuid::new_v4().to_string());
            plan
        }),
        ("wrong selected scope", {
            let mut plan = plan.clone();
            plan.events.iter_mut().find(|event| event.kind == "message_delivered")
                .unwrap().scope_id = Uuid::new_v4().to_string();
            plan
        }),
        ("foreign accepted source", {
            let mut plan = plan.clone();
            let index = plan.events.iter().position(|event|
                event.kind == "message_delivered").unwrap();
            plan.event_sources.insert(index, Uuid::new_v4().to_string());
            plan
        }),
        ("fabricated Manual acknowledgment", {
            let mut plan = plan.clone();
            plan.complete_user_task_ids.push(task.user_task_id.clone());
            plan
        }),
    ] {
        assert!(repository::deliver_message(&fixture.db, &prepared,
            repository::ProcessPlanInput::Supplied(&forged), at_ms).is_err(), "{case}");
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before, "{case}");
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    repository::deliver_message(&reopened, &prepared,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().unwrap();
    let committed = super::signal_proof_tests::all_transition_rows(&fixture);
    assert!(repository::deliver_message(&reopened, &prepared,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().is_none());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), committed);
}

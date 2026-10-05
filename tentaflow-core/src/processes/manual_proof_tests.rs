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
    let plan = runtime::plan_start(&version.model, &instance_id, &fixture.owner,
        &version.definition_id, version.version, variables.clone(), runtime::StartCause::Manual,
        at_ms, manual_input(&command)).unwrap();
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
            &instance_id, &version.definition_id, version.version, &variables,
            &forged, at_ms).is_err());
        assert_eq!(transition_rows(&fixture), before,
            "manual open forgery {variant} changed durable rows");
    }
    repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables,
        &plan, at_ms).unwrap();
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
        manual_entry(&snapshot, &task.user_task_id, &command)).unwrap();
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
            &forged, at_ms).unwrap_err();
        assert!(!format!("{error:#}").is_empty());
        assert_eq!(transition_rows(&fixture), before,
            "manual forgery {variant} changed durable rows");
    }
    let committed = repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
        &command, &instance_id, &task.user_task_id, snapshot.instance.revision,
        &plan, at_ms).unwrap();
    assert_eq!(committed.instance.status, ProcessInstanceStatus::Completed);
    let reopened = repository::get_instance(&fixture.db, &fixture.owner, &instance_id, None).unwrap();
    assert_eq!(reopened.status, ProcessInstanceStatus::Completed);
    let before_replay = transition_rows(&fixture);
    repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
        &command, &instance_id, &task.user_task_id, snapshot.instance.revision,
        &plan, at_ms).unwrap();
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

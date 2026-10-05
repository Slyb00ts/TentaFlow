// ============ File: manual_tests.rs — File-backed ManualTask acknowledgment behavior ============

use super::call_tests::transition_rows;
use super::repository::{self, AcceptedInputRef};
use super::runtime::{self, test_support::*};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tentaflow_protocol::processes::{ProcessInstanceStatus, ProcessModel, ProcessNode, ProcessNodeKind,
    ProcessUserTaskKind, ProcessUserTaskStatus};
use uuid::Uuid;

pub(super) fn manual_model(assignee_user_id: Option<String>, terminate: bool) -> ProcessModel {
    let mut model = super::model::starter_model();
    model.nodes.insert(1, ProcessNode { id: "Manual".into(), name: "Inspect the physical item".into(),
        kind: ProcessNodeKind::ManualTask { assignee_user_id,
            instructions: "Inspect the physical item and acknowledge that the work is done.".into() },
        repeat: None });
    if terminate { model.nodes[2].kind = ProcessNodeKind::TerminateEnd; }
    model.sequence_flows = vec![edge("ToManual", "Start_1", "Manual"),
        edge("FromManual", "Manual", "End_1")];
    model
}

pub(super) fn start_manual(fixture: &Fixture, model: &ProcessModel,
) -> (String, tentaflow_protocol::processes::ProcessVersion, repository::CommandStamp) {
    let version = publish_model(fixture, model);
    let instance_id = Uuid::new_v4().to_string();
    let command = stamp("manual-start");
    let vars = serde_json::to_value(&model.variables).unwrap();
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&version.model, &instance_id, &fixture.owner,
        &version.definition_id, version.version, vars.clone(), runtime::StartCause::Manual,
        at_ms, manual_input(&command)).unwrap();
    assert_eq!(plan.events.iter().filter(|event| event.kind == "manual_task_opened").count(), 1);
    assert!(!plan.events.iter().any(|event| event.kind == "user_task_opened"));
    repository::start_instance(&fixture.db, &fixture.owner, &command, &instance_id,
        &version.definition_id, version.version, &vars, &plan, at_ms).unwrap();
    (instance_id, version, command)
}

pub(super) fn manual_entry(snapshot: &repository::RuntimeSnapshot, task_id: &str,
    command: &repository::CommandStamp) -> AcceptedInputRef {
    let task = snapshot.user_tasks.iter().find(|task| task.user_task_id == task_id).unwrap();
    AcceptedInputRef::ManualAcknowledgment { task_id: task_id.into(),
        expected_task_revision: task.revision,
        expected_instance_revision: snapshot.instance.revision,
        command_id: command.command_id.clone(), request_hash: command.request_hash.clone() }
}

#[test]
fn assigned_acknowledger_commits_distinct_manual_fact_and_replays_after_reopen() {
    let fixture = Fixture::new();
    let model = manual_model(Some(fixture.participant.user_id.clone()), false);
    let (instance_id, _, _) = start_manual(&fixture, &model);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.participant, &instance_id).unwrap();
    let task = snapshot.user_tasks.iter().find(|task| task.kind == ProcessUserTaskKind::Manual).unwrap();
    assert_eq!(task.status, ProcessUserTaskStatus::Open);
    assert_eq!(task.assignee_user_id, fixture.participant.user_id);
    assert_eq!(task.outputs, Value::Null);
    let detail = repository::get_user_task(&fixture.db, &fixture.participant,
        &instance_id, &task.user_task_id).unwrap();
    assert_eq!(detail.instructions.as_deref(), Some("Inspect the physical item and acknowledge that the work is done."));
    let command = stamp("manual-ack");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_manual_acknowledgment(&snapshot, &task.user_task_id,
        &fixture.participant.user_id, at_ms,
        manual_entry(&snapshot, &task.user_task_id, &command)).unwrap();
    assert_eq!(plan.events.iter().filter(|event| event.kind == "manual_task_acknowledged").count(), 1);
    assert!(!plan.events.iter().any(|event| event.kind == "user_task_completed"));
    let outcome = repository::acknowledge_manual_task(&fixture.db, &fixture.participant,
        &command, &instance_id, &task.user_task_id, snapshot.instance.revision,
        &plan, at_ms).unwrap();
    assert_eq!(outcome.instance.status, ProcessInstanceStatus::Completed);
    let reopened = repository::get_instance(&fixture.db, &fixture.participant, &instance_id, None).unwrap();
    assert_eq!(reopened.status, ProcessInstanceStatus::Completed);
    let events = repository::list_events(&fixture.db, &fixture.participant,
        &instance_id, 0, 200).unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "manual_task_opened"
        && event.data == json!({"user_task_id":task.user_task_id,
            "assignee_user_id":fixture.participant.user_id})).count(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "manual_task_acknowledged"
        && event.data == json!({"user_task_id":task.user_task_id,
            "acknowledged_by_user_id":fixture.participant.user_id})).count(), 1);
    let conn = fixture.db.read().unwrap();
    let (kind, outputs, status): (String,String,String) = conn.query_row(
        "SELECT kind,outputs_json,status FROM bpmn_user_tasks WHERE user_task_id=?1",
        [&task.user_task_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
    assert_eq!((kind.as_str(), outputs.as_str(), status.as_str()), ("manual", "null", "completed"));
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM bpmn_jobs WHERE instance_id=?1",
        [&instance_id], |row| row.get::<_,i64>(0)).unwrap(), 0);
    drop(conn);
    let before_replay = transition_rows(&fixture);
    repository::acknowledge_manual_task(&fixture.db, &fixture.participant,
        &command, &instance_id, &task.user_task_id, snapshot.instance.revision,
        &plan, at_ms).unwrap();
    assert_eq!(transition_rows(&fixture), before_replay);
    let conflicting = repository::CommandStamp { command_id: command.command_id.clone(),
        request_hash: repository::request_hash(&"different manual acknowledgment").unwrap() };
    assert!(repository::acknowledge_manual_task(&fixture.db, &fixture.participant,
        &conflicting, &instance_id, &task.user_task_id, snapshot.instance.revision,
        &plan, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before_replay);
}

#[test]
fn manual_acknowledgment_requires_its_assignee_and_distinct_command() {
    let fixture = Fixture::new();
    let model = manual_model(Some(fixture.participant.user_id.clone()), false);
    let (instance_id, _, _) = start_manual(&fixture, &model);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.participant, &instance_id).unwrap();
    let task = snapshot.user_tasks.iter().find(|task| task.kind == ProcessUserTaskKind::Manual).unwrap();
    let command = stamp("manual-auth");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_manual_acknowledgment(&snapshot, &task.user_task_id,
        &fixture.participant.user_id, at_ms,
        manual_entry(&snapshot, &task.user_task_id, &command)).unwrap();
    let before = transition_rows(&fixture);
    assert!(repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
        &command, &instance_id, &task.user_task_id, snapshot.instance.revision,
        &plan, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    assert!(runtime::plan_user_completion(&snapshot, &task.user_task_id, &json!({}),
        None, at_ms, human_input(&snapshot, &task.user_task_id, &command)).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut stale = plan.clone();
    stale.events.iter_mut().find(|event| event.kind == "manual_task_acknowledged")
        .unwrap().data["acknowledged_by_user_id"] = json!(fixture.owner.user_id);
    assert!(repository::acknowledge_manual_task(&fixture.db, &fixture.participant,
        &command, &instance_id, &task.user_task_id, snapshot.instance.revision,
        &stale, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
}

#[test]
fn manual_acknowledgment_advances_to_script_and_preserves_both_factual_actions() {
    let fixture = Fixture::new();
    let mut model = manual_model(None, false);
    model.variables.insert("answer".into(), Value::Null);
    model.nodes.insert(2, ProcessNode { id: "Compute".into(), name: "Compute".into(),
        kind: ProcessNodeKind::ScriptTask { script: "41 + 1".into(),
            output_mapping: BTreeMap::from([("answer".into(), "outputs".into())]) },
        repeat: None });
    model.sequence_flows = vec![edge("ToManual", "Start_1", "Manual"),
        edge("ToCompute", "Manual", "Compute"), edge("ToEnd", "Compute", "End_1")];
    let (instance_id, _, _) = start_manual(&fixture, &model);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &instance_id).unwrap();
    let task = snapshot.user_tasks.iter().find(|task| task.kind == ProcessUserTaskKind::Manual).unwrap();
    let command = stamp("manual-script");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_manual_acknowledgment(&snapshot, &task.user_task_id,
        &fixture.owner.user_id, at_ms,
        manual_entry(&snapshot, &task.user_task_id, &command)).unwrap();
    assert_eq!(plan.events.iter().filter(|event| event.kind == "manual_task_acknowledged").count(), 1);
    assert_eq!(plan.events.iter().filter(|event| event.kind == "script_completed"
        && event.data == json!({"outputs":42})).count(), 1);
    let before = transition_rows(&fixture);
    let mut forged = plan.clone();
    forged.events.iter_mut().find(|event| event.kind == "script_completed")
        .unwrap().data = json!({"outputs":43});
    assert!(repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
        &command, &instance_id, &task.user_task_id, snapshot.instance.revision,
        &forged, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let committed = repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
        &command, &instance_id, &task.user_task_id, snapshot.instance.revision,
        &plan, at_ms).unwrap();
    assert_eq!(committed.instance.variables["answer"], 42);
    assert_eq!(repository::get_instance(&fixture.db, &fixture.owner, &instance_id, None)
        .unwrap().variables["answer"], 42);
}

#[test]
fn terminating_sibling_preempts_manual_entry_without_fabricating_acknowledgment() {
    let fixture = Fixture::new();
    let mut model = manual_model(None, true);
    model.nodes.insert(1, ProcessNode { id: "Split".into(), name: "Fork".into(),
        kind: ProcessNodeKind::ParallelGateway, repeat: None });
    model.nodes.push(ProcessNode { id: "Stop".into(), name: "Stop".into(),
        kind: ProcessNodeKind::TerminateEnd, repeat: None });
    model.sequence_flows = vec![edge("ToSplit", "Start_1", "Split"),
        edge("A_Stop", "Split", "Stop"), edge("Z_Manual", "Split", "Manual"),
        edge("FromManual", "Manual", "End_1")];
    let started = start_model(&fixture, &model);
    assert_eq!(started.status, ProcessInstanceStatus::Completed);
    let events = repository::list_events(&fixture.db, &fixture.owner,
        &started.instance_id, 0, 200).unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "terminate_end_reached").count(), 1);
    assert!(!events.iter().any(|event| matches!(event.kind.as_str(),
        "manual_task_opened" | "manual_task_acknowledged")));
    assert_eq!(fixture.db.read().unwrap().query_row(
        "SELECT COUNT(*) FROM bpmn_user_tasks WHERE instance_id=?1",
        [&started.instance_id], |row| row.get::<_,i64>(0)).unwrap(), 0);
}

#[test]
fn embedded_manual_acknowledgment_returns_only_after_the_real_child_wait() {
    let fixture = Fixture::new();
    let mut model = embedded_model(manual_model(None, false), "Scope");
    model.nodes.insert(2, ProcessNode { id: "ParentScript".into(), name: "Continue after child".into(),
        kind: ProcessNodeKind::ScriptTask { script: "41 + 1".into(),
            output_mapping: BTreeMap::new() }, repeat: None });
    model.sequence_flows = vec![edge("Enter_Scope", "RootStart_Scope", "Scope"),
        edge("ScopeToScript", "Scope", "ParentScript"),
        edge("ScriptToEnd", "ParentScript", "RootEnd_Scope")];
    let (instance_id, _, _) = start_manual(&fixture, &model);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &instance_id).unwrap();
    let task = snapshot.user_tasks.iter().find(|task| task.kind == ProcessUserTaskKind::Manual).unwrap();
    assert_ne!(task.scope_id, instance_id);
    let command = stamp("embedded-manual");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_manual_acknowledgment(&snapshot, &task.user_task_id,
        &fixture.owner.user_id, at_ms,
        manual_entry(&snapshot, &task.user_task_id, &command)).unwrap();
    assert_eq!(plan.events.iter().filter(|event| event.kind == "scope_completed").count(), 1);
    assert_eq!(plan.events.iter().filter(|event| event.kind == "script_completed"
        && event.node_id.as_deref() == Some("ParentScript")).count(), 1);
    let mut forged = plan.clone();
    forged.events.iter_mut().find(|event| event.kind == "manual_task_acknowledged")
        .unwrap().scope_id = instance_id.clone();
    let before = transition_rows(&fixture);
    assert!(repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
        &command, &instance_id, &task.user_task_id, snapshot.instance.revision,
        &forged, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let child_end_index = plan.events.iter().position(|event|
        event.kind == "end_reached" && event.scope_id == task.scope_id).unwrap();
    let parent_end_index = plan.events.iter().position(|event|
        event.kind == "end_reached" && event.scope_id == instance_id).unwrap();
    let parent_script_index = plan.events.iter().position(|event|
        event.kind == "script_completed" && event.scope_id == instance_id).unwrap();
    let parent_wait_id = plan.events.iter().find(|event|
        event.kind == "scope_completed").unwrap().data["parent_token_id"]
        .as_str().unwrap().to_owned();
    let child_end_source = plan.event_sources.get(&child_end_index).unwrap().clone();
    let mut wrong_child_source = plan.clone();
    wrong_child_source.event_sources.insert(child_end_index, parent_wait_id);
    assert!(repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
        &command, &instance_id, &task.user_task_id, snapshot.instance.revision,
        &wrong_child_source, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut wrong_parent_source = plan.clone();
    wrong_parent_source.event_sources.insert(parent_script_index, child_end_source.clone());
    assert!(repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
        &command, &instance_id, &task.user_task_id, snapshot.instance.revision,
        &wrong_parent_source, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut wrong_parent_end = plan.clone();
    wrong_parent_end.event_sources.insert(parent_end_index, child_end_source);
    assert!(repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
        &command, &instance_id, &task.user_task_id, snapshot.instance.revision,
        &wrong_parent_end, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut missing_child_end = plan.clone();
    missing_child_end.events[child_end_index].kind = "node_completed".into();
    assert!(repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
        &command, &instance_id, &task.user_task_id, snapshot.instance.revision,
        &missing_child_end, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let committed = repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
        &command, &instance_id, &task.user_task_id, snapshot.instance.revision,
        &plan, at_ms).unwrap();
    assert_eq!(committed.instance.status, ProcessInstanceStatus::Completed);
    assert_eq!(repository::get_instance(&fixture.db, &fixture.owner, &instance_id, None)
        .unwrap().status, ProcessInstanceStatus::Completed);
}

#[test]
fn owner_cancel_closes_manual_wait_and_denies_late_acknowledgment() {
    let fixture = Fixture::new();
    let model = manual_model(None, false);
    let (instance_id, _, _) = start_manual(&fixture, &model);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &instance_id).unwrap();
    let task = snapshot.user_tasks.iter().find(|task| task.kind == ProcessUserTaskKind::Manual).unwrap();
    let command = stamp("manual-late");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_manual_acknowledgment(&snapshot, &task.user_task_id,
        &fixture.owner.user_id, at_ms,
        manual_entry(&snapshot, &task.user_task_id, &command)).unwrap();
    let cancelled = repository::cancel_instance(&fixture.db, &fixture.owner,
        &stamp("cancel-manual"), &instance_id, snapshot.instance.revision).unwrap();
    assert_eq!(cancelled.instance.status, ProcessInstanceStatus::Cancelled);
    let before = transition_rows(&fixture);
    assert!(repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
        &command, &instance_id, &task.user_task_id, snapshot.instance.revision,
        &plan, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let reopened = repository::get_instance(&fixture.db, &fixture.owner, &instance_id, None).unwrap();
    assert_eq!(reopened.status, ProcessInstanceStatus::Cancelled);
    let detail = repository::get_user_task(&fixture.db, &fixture.owner,
        &instance_id, &task.user_task_id).unwrap();
    assert_eq!(detail.status, ProcessUserTaskStatus::Cancelled);
}

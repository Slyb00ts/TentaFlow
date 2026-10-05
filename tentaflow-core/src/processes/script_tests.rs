// ============ File: script_tests.rs — File-backed ScriptTask behavior tests ============

use super::call_tests::transition_rows;
use super::repository;
use super::runtime::{self, test_support::*};
use super::jobs;
use crate::flow_engine::expr;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tentaflow_protocol::processes::{ProcessInstanceStatus, ProcessModel, ProcessNode, ProcessNodeKind};
use uuid::Uuid;
use tokio_util::sync::CancellationToken;
use tentaflow_protocol::processes::ActivityVerification;

fn script_model(script: &str, mapping: BTreeMap<String, String>, terminate: bool) -> ProcessModel {
    let mut model = super::model::starter_model();
    model.variables.insert("seed".into(), json!(4));
    model.variables.insert("answer".into(), Value::Null);
    model.nodes.insert(1, ProcessNode {
        id: "Compute".into(), name: "Compute".into(),
        kind: ProcessNodeKind::ScriptTask { script: script.into(), output_mapping: mapping },
        repeat: None,
    });
    if terminate {
        model.nodes[2].kind = ProcessNodeKind::TerminateEnd;
    }
    model.sequence_flows = vec![edge("ToCompute", "Start_1", "Compute"),
        edge("FromCompute", "Compute", "End_1")];
    model
}

fn plan_and_start(
    fixture: &Fixture, model: &ProcessModel,
) -> (repository::CommandStamp, String, super::repository::RuntimePlan, Value,
    tentaflow_protocol::processes::ProcessVersion, i64) {
    let version = publish_model(fixture, model);
    let instance_id = Uuid::new_v4().to_string();
    let command = stamp("script-start");
    let variables = serde_json::to_value(&model.variables).unwrap();
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&version.model, &instance_id, &fixture.owner,
        &version.definition_id, version.version, variables.clone(), runtime::StartCause::Manual,
        at_ms, manual_input(&command)).unwrap();
    (command, instance_id, plan, variables, version, at_ms)
}

#[test]
fn script_success_maps_one_json_result_and_reopens_without_reexecution() {
    let fixture = Fixture::new();
    let model = script_model("{\"value\": vars.seed + 1}",
        BTreeMap::from([("answer".into(), "outputs.value".into())]), false);
    let (command, instance_id, plan, variables, version, at_ms) = plan_and_start(&fixture, &model);
    let fact = plan.events.iter().find(|event| event.kind == "script_completed").unwrap();
    assert_eq!(fact.data, json!({"outputs":{"value":5}}));
    let committed = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, &plan,
        at_ms).unwrap();
    assert_eq!(committed.status, ProcessInstanceStatus::Completed);
    assert_eq!(committed.variables["answer"], 5);
    let persisted = repository::get_instance(&fixture.db, &fixture.owner, &instance_id, None).unwrap();
    assert_eq!(persisted.variables, committed.variables);
    let before_replay = transition_rows(&fixture);
    let replay = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, &plan,
        at_ms).unwrap();
    assert_eq!(replay.instance_id, instance_id);
    assert_eq!(transition_rows(&fixture), before_replay);
}

#[test]
fn script_array_result_without_mapping_preserves_parent_variables() {
    let fixture = Fixture::new();
    let model = script_model("[1,2,3]", BTreeMap::new(), false);
    let (command, instance_id, plan, variables, version, at_ms) = plan_and_start(&fixture, &model);
    assert_eq!(plan.events.iter().find(|event| event.kind == "script_completed")
        .unwrap().data, json!({"outputs":[1,2,3]}));
    assert!(!plan.variable_effects.iter().any(|effect| matches!(effect,
        repository::VariableEffect::Mapped { node_id, .. } if node_id == "Compute")));
    let committed = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, &plan,
        at_ms).unwrap();
    assert_eq!(committed.variables, variables);
}

#[test]
fn script_body_and_mapping_failures_park_exactly_one_activation() {
    for (script, mapping, code) in [
        ("1 / 0", BTreeMap::new(), "SCRIPT_EVALUATION_FAILED"),
        ("{\"value\": 1}", BTreeMap::from([("answer".into(), "1 / 0".into())]),
            "SCRIPT_MAPPING_FAILED"),
    ] {
        let fixture = Fixture::new();
        let model = script_model(script, mapping, false);
        let (command, instance_id, plan, variables, version, at_ms) = plan_and_start(&fixture, &model);
        assert_eq!(plan.add_incidents.len(), 1);
        assert_eq!(plan.add_incidents[0].code, code);
        assert!(!plan.events.iter().any(|event| event.kind == "script_completed"));
        assert!(plan.create_tokens.iter().any(|token|
            token.node_id == "Compute" && token.status == "waiting"));
        let committed = repository::start_instance(&fixture.db, &fixture.owner, &command,
            &instance_id, &version.definition_id, version.version, &variables, &plan,
            at_ms).unwrap();
        assert_eq!(committed.status, ProcessInstanceStatus::Incident);
        let reopened = repository::get_instance(&fixture.db, &fixture.owner, &instance_id, None).unwrap();
        assert_eq!(reopened.incidents.len(), 1);
        assert_eq!(reopened.incidents[0].code, code);
    }
}

#[test]
fn script_mapping_rejects_a_cumulative_local_overrun_without_partial_write() {
    let fixture = Fixture::new();
    let mut model = script_model("vars.large", BTreeMap::from([
        ("copy_one".into(), "outputs".into()),
        ("copy_two".into(), "outputs".into()),
    ]), false);
    model.variables.insert("large".into(), json!("x".repeat(120_000)));
    let (command, instance_id, plan, variables, version, at_ms) = plan_and_start(&fixture, &model);
    assert_eq!(plan.add_incidents.len(), 1);
    assert_eq!(plan.add_incidents[0].code, "SCRIPT_MAPPING_FAILED");
    assert_eq!(plan.variables, variables);
    assert!(!plan.events.iter().any(|event| event.kind == "script_completed"));
    let committed = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, &plan,
        at_ms).unwrap();
    assert_eq!(committed.variables, variables);
}

#[test]
fn script_mapping_then_terminate_retains_both_source_facts() {
    let fixture = Fixture::new();
    let model = script_model("{\"value\": vars.seed + 1}",
        BTreeMap::from([("answer".into(), "outputs.value".into())]), true);
    let (command, instance_id, plan, variables, version, at_ms) = plan_and_start(&fixture, &model);
    assert_eq!(plan.events.iter().filter(|event| event.kind == "script_completed").count(), 1);
    assert_eq!(plan.events.iter().filter(|event| event.kind == "terminate_end_reached").count(), 1);
    let committed = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, &plan,
        at_ms).unwrap();
    assert_eq!(committed.status, ProcessInstanceStatus::Completed);
    assert_eq!(committed.variables["answer"], 5);
}

#[tokio::test]
async fn observed_service_result_survives_joined_script_infrastructure_failure_without_retry() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("script retained result", None));
    let mut model = service_model(&flow_id, ActivityVerification::Condition {
        expression: "true".into(),
    });
    model.nodes.insert(2, ProcessNode {
        id: "Compute".into(), name: "Compute".into(),
        kind: ProcessNodeKind::ScriptTask {
            script: "vars.answer + \" b2i-infra-probe\"".into(), output_mapping: BTreeMap::new(),
        }, repeat: None,
    });
    model.sequence_flows = vec![edge("ToService", "Start_1", "Service"),
        edge("ToScript", "Service", "Compute"), edge("ToEnd", "Compute", "End_1")];
    let started = start_model(&fixture, &model);
    let worker = "script-infrastructure-worker";
    let claimed = repository::claim_job(&fixture.db, worker,
        chrono::Utc::now().timestamp_millis()).unwrap().unwrap();
    expr::inject_script_infrastructure_failure("vars.answer + \" b2i-infra-probe\"");
    jobs::execute_claimed(&fixture.db, fixture.dispatcher(), worker, claimed.clone(),
        CancellationToken::new()).await.unwrap();
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let retained = snapshot.jobs.iter().find(|job| job.job_id == claimed.job.job_id).unwrap();
    assert_eq!(retained.status, "error");
    assert!(retained.result.is_some());
    assert!(retained.result_origin.is_some());
    assert_eq!(snapshot.instance.status, ProcessInstanceStatus::Incident);
    let reopened = repository::get_instance(&fixture.db, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(reopened.incidents.len(), 1);
    assert_eq!(reopened.incidents[0].code, "SCRIPT_INFRASTRUCTURE_FAILED");
    assert!(!reopened.incidents[0].can_retry);
    assert!(!reopened.can_retry);
    let (listed, _, _) = repository::list_instances(&fixture.db, &fixture.owner,
        None, 0, 20).unwrap();
    assert!(!listed.iter().find(|item| item.instance_id == started.instance_id)
        .unwrap().can_retry);
    let history = repository::list_events(&fixture.db, &fixture.owner,
        &started.instance_id, 0, 200).unwrap().0;
    assert_eq!(history.iter().filter(|event| event.kind == "service_result").count(), 1);
    assert_eq!(history.iter().filter(|event| event.kind == "job_failed").count(), 1);
    let before = transition_rows(&fixture);
    let denied = repository::retry_job(&fixture.db, &fixture.owner, &stamp("retry script"),
        &started.instance_id, &claimed.job.job_id, reopened.revision).unwrap_err();
    assert!(denied.to_string().contains("retained after Script infrastructure failure"));
    assert_eq!(transition_rows(&fixture), before);
}

#[tokio::test]
async fn accepted_service_result_then_script_runs_without_external_redispatch() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("script success", None));
    let mut model = service_model(&flow_id, ActivityVerification::Condition {
        expression: "true".into(),
    });
    model.nodes.insert(2, ProcessNode {
        id: "Compute".into(), name: "Compute".into(),
        kind: ProcessNodeKind::ScriptTask {
            script: "vars.answer".into(),
            output_mapping: BTreeMap::from([("script_answer".into(), "outputs".into())]),
        }, repeat: None,
    });
    model.sequence_flows = vec![edge("ToService", "Start_1", "Service"),
        edge("ToScript", "Service", "Compute"), edge("ToEnd", "Compute", "End_1")];
    let started = start_model(&fixture, &model);
    let worker = "script-positive-worker";
    let claimed = repository::claim_job(&fixture.db, worker,
        chrono::Utc::now().timestamp_millis()).unwrap().unwrap();
    jobs::execute_claimed(&fixture.db, fixture.dispatcher(), worker, claimed.clone(),
        CancellationToken::new()).await.unwrap();
    let committed = repository::get_instance(&fixture.db, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(committed.status, ProcessInstanceStatus::Completed,
        "fenced Service continuation incident: {:?}", committed.incidents);
    assert_eq!(committed.variables["script_answer"], "script success");
    let history = repository::list_events(&fixture.db, &fixture.owner,
        &started.instance_id, 0, 200).unwrap().0;
    assert_eq!(history.iter().filter(|event| event.kind == "service_result").count(), 1);
    assert_eq!(history.iter().filter(|event| event.kind == "script_completed").count(), 1);
    assert_eq!(history.iter().filter(|event| event.kind == "service_queued").count(), 1);
}

#[tokio::test]
async fn approved_service_verification_runs_script_from_the_retained_result() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("verified script", None));
    let mut model = service_model(&flow_id, ActivityVerification::Human);
    model.variables.insert("script_answer".into(), Value::Null);
    model.nodes.insert(2, ProcessNode {
        id: "Compute".into(), name: "Compute".into(),
        kind: ProcessNodeKind::ScriptTask {
            script: "vars.answer".into(),
            output_mapping: BTreeMap::from([("script_answer".into(), "outputs".into())]),
        }, repeat: None,
    });
    model.sequence_flows = vec![edge("ToService", "Start_1", "Service"),
        edge("ToScript", "Service", "Compute"), edge("ToEnd", "Compute", "End_1")];
    let started = start_model(&fixture, &model);
    let worker = "verified-script-worker";
    let claimed = repository::claim_job(&fixture.db, worker,
        chrono::Utc::now().timestamp_millis()).unwrap().unwrap();
    jobs::execute_claimed(&fixture.db, fixture.dispatcher(), worker, claimed,
        CancellationToken::new()).await.unwrap();
    let waiting = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let task = waiting.user_tasks.iter().find(|task|
        task.status == tentaflow_protocol::processes::ProcessUserTaskStatus::Open
            && task.kind == tentaflow_protocol::processes::ProcessUserTaskKind::Verification)
        .unwrap();
    let command = stamp("approve script source");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_user_completion(&waiting, &task.user_task_id,
        &Value::Null, Some(true), at_ms,
        human_input(&waiting, &task.user_task_id, &command)).unwrap();
    assert_eq!(plan.events.iter().filter(|event|
        event.kind == "script_completed").count(), 1);
    let committed = repository::complete_user_task(&fixture.db, &fixture.owner,
        &command, &started.instance_id, &task.user_task_id,
        waiting.instance.revision, &Value::Null, Some(true), &plan, at_ms).unwrap().instance;
    assert_eq!(committed.status, ProcessInstanceStatus::Completed);
    assert_eq!(committed.variables["script_answer"], "verified script");
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(persisted.instance.variables, committed.variables);
    let history = repository::list_events(&reopened, &fixture.owner,
        &started.instance_id, 0, 200).unwrap().0;
    assert_eq!(history.iter().filter(|event| event.kind == "service_result").count(), 1);
    assert_eq!(history.iter().filter(|event| event.kind == "verification_approved").count(), 1);
    assert_eq!(history.iter().filter(|event| event.kind == "script_completed").count(), 1);
}

#[test]
fn script_result_selects_the_pinned_xor_edge_and_rejects_a_forged_choice() {
    let fixture = Fixture::new();
    let mut model = script_model("vars.seed + 1",
        BTreeMap::from([("answer".into(), "outputs".into())]), false);
    model.nodes.insert(2, ProcessNode { id: "Choice".into(), name: "Route result".into(),
        kind: ProcessNodeKind::ExclusiveGateway { default_flow_id: Some("Fallback".into()) },
        repeat: None });
    model.nodes.push(ProcessNode { id: "OtherEnd".into(), name: "Other result".into(),
        kind: ProcessNodeKind::End, repeat: None });
    model.sequence_flows = vec![edge("ToCompute", "Start_1", "Compute"),
        edge("ToChoice", "Compute", "Choice"), edge("Selected", "Choice", "End_1"),
        edge("Fallback", "Choice", "OtherEnd")];
    model.sequence_flows[2].condition = Some("vars.answer == 5".into());
    let (command, instance_id, plan, variables, version, at_ms) = plan_and_start(&fixture, &model);
    let selected = plan.events.iter().find(|event| event.kind == "exclusive_selected").unwrap();
    assert_eq!(selected.data["sequence_flow_id"], "Selected");
    let mut forged = plan.clone();
    let choice = forged.events.iter_mut().find(|event| event.kind == "exclusive_selected").unwrap();
    choice.data["sequence_flow_id"] = json!("Fallback");
    let before = transition_rows(&fixture);
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, &forged,
        at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let committed = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, &plan,
        at_ms).unwrap();
    assert_eq!(committed.status, ProcessInstanceStatus::Completed);
    assert_eq!(committed.variables["answer"], 5);
}

#[test]
fn embedded_script_maps_child_result_once_and_reopens() {
    let fixture = Fixture::new();
    let child = script_model("vars.seed + 1",
        BTreeMap::from([("answer".into(), "outputs".into())]), false);
    let mut model = embedded_model(child, "Scope");
    model.variables.insert("answer".into(), Value::Null);
    let scope = model.nodes.iter_mut().find(|node| node.id == "Scope").unwrap();
    let ProcessNodeKind::SubProcess { output_mapping, .. } = &mut scope.kind else { unreachable!() };
    output_mapping.insert("answer".into(), "outputs.answer".into());
    let (command, instance_id, plan, variables, version, at_ms) = plan_and_start(&fixture, &model);
    assert_eq!(plan.events.iter().filter(|event| event.kind == "script_completed").count(), 1);
    assert_eq!(plan.events.iter().filter(|event| event.kind == "scope_completed").count(), 1);
    let mut forged = plan.clone();
    let script_fact = forged.events.iter_mut().find(|event| event.kind == "script_completed").unwrap();
    script_fact.scope_id = instance_id.clone();
    let before = transition_rows(&fixture);
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, &forged,
        at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let committed = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, &plan,
        at_ms).unwrap();
    assert_eq!(committed.variables["answer"], 5);
    let reopened = repository::get_instance(&fixture.db, &fixture.owner, &instance_id, None).unwrap();
    assert_eq!(reopened.variables, committed.variables);
}

#[test]
fn script_output_selects_an_inclusive_branch_before_its_factual_join() {
    let fixture = Fixture::new();
    let mut model = script_model("true",
        BTreeMap::from([("choose_a".into(), "outputs".into())]), false);
    model.variables.insert("choose_a".into(), json!(false));
    model.nodes.insert(2, ProcessNode { id: "Split".into(), name: "Select work".into(),
        kind: ProcessNodeKind::InclusiveGateway { default_flow_id: None }, repeat: None });
    model.nodes.insert(3, ProcessNode { id: "A".into(), name: "Selected work".into(),
        kind: ProcessNodeKind::UserTask { assignee_user_id: Some(fixture.owner.user_id.clone()),
            output_mapping: BTreeMap::new() }, repeat: None });
    model.nodes.insert(4, ProcessNode { id: "B".into(), name: "Unselected work".into(),
        kind: ProcessNodeKind::UserTask { assignee_user_id: Some(fixture.owner.user_id.clone()),
            output_mapping: BTreeMap::new() }, repeat: None });
    model.nodes.insert(5, ProcessNode { id: "Join".into(), name: "Join selected work".into(),
        kind: ProcessNodeKind::InclusiveGateway { default_flow_id: None }, repeat: None });
    model.sequence_flows = vec![edge("ToCompute", "Start_1", "Compute"),
        edge("ToSplit", "Compute", "Split"), edge("ToA", "Split", "A"),
        edge("ToB", "Split", "B"), edge("FromA", "A", "Join"),
        edge("FromB", "B", "Join"), edge("ToEnd", "Join", "End_1")];
    model.sequence_flows[2].condition = Some("vars.choose_a".into());
    model.sequence_flows[3].condition = Some("!vars.choose_a".into());
    let (start_command, instance_id, start_plan, variables, version, at_ms) = plan_and_start(&fixture, &model);
    let mut forged = start_plan.clone();
    let split = forged.events.iter_mut().find(|event| event.kind == "inclusive_split").unwrap();
    split.data["selected_branch_edge_ids"] = json!(["ToB"]);
    let before = transition_rows(&fixture);
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &start_command,
        &instance_id, &version.definition_id, version.version, &variables, &forged,
        at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let started = repository::start_instance(&fixture.db, &fixture.owner, &start_command,
        &instance_id, &version.definition_id, version.version, &variables, &start_plan,
        at_ms).unwrap();
    assert_eq!(started.status, ProcessInstanceStatus::Waiting);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let task = snapshot.user_tasks.iter().find(|task| task.node_id == "A").unwrap();
    assert!(!snapshot.user_tasks.iter().any(|task| task.node_id == "B"));
    let command = stamp("finish Script-selected work");
    let outputs = json!({"done":true});
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_user_completion(&snapshot, &task.user_task_id, &outputs, None,
        at_ms,
        human_input(&snapshot, &task.user_task_id, &command)).unwrap();
    let committed = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &started.instance_id, &task.user_task_id, snapshot.instance.revision,
        &outputs, None, &plan, at_ms).unwrap();
    assert_eq!(committed.instance.status, ProcessInstanceStatus::Completed);
    let history = repository::list_events(&fixture.db, &fixture.owner,
        &started.instance_id, 0, 200).unwrap().0;
    assert_eq!(history.iter().filter(|event| event.kind == "inclusive_split").count(), 1);
    assert_eq!(history.iter().filter(|event| event.kind == "inclusive_joined").count(), 1);
}

#[test]
fn script_after_multi_instance_uses_the_accepted_aggregate() {
    let fixture = Fixture::new();
    let mut model = super::repetition_tests::sequential_human_model(&fixture.owner.user_id, 1);
    model.variables.insert("answer".into(), Value::Null);
    model.nodes.insert(2, ProcessNode { id: "Compute".into(), name: "Compute aggregate".into(),
        kind: ProcessNodeKind::ScriptTask {
            script: "size(vars.results)".into(),
            output_mapping: BTreeMap::from([("answer".into(), "outputs".into())]),
        }, repeat: None });
    model.sequence_flows = vec![edge("ToWork", "Start_1", "RepeatedWork"),
        edge("ToCompute", "RepeatedWork", "Compute"),
        edge("ToEnd", "Compute", "End_1")];
    let started = start_model(&fixture, &model);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let task = snapshot.user_tasks.iter().find(|task| task.node_id == "RepeatedWork").unwrap();
    let outputs = json!({"item":1});
    let command = stamp("accept repeated work before Script");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_user_completion(&snapshot, &task.user_task_id, &outputs, None,
        at_ms,
        human_input(&snapshot, &task.user_task_id, &command)).unwrap();
    assert_eq!(plan.events.iter().filter(|event| event.kind == "script_completed").count(), 1);
    let mut forged = plan.clone();
    let script = forged.events.iter_mut().find(|event| event.kind == "script_completed").unwrap();
    script.data = json!({"outputs":2});
    let before = transition_rows(&fixture);
    assert!(repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &started.instance_id, &task.user_task_id, snapshot.instance.revision,
        &outputs, None, &forged, at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let committed = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &started.instance_id, &task.user_task_id, snapshot.instance.revision,
        &outputs, None, &plan, at_ms).unwrap();
    assert_eq!(committed.instance.status, ProcessInstanceStatus::Completed);
    assert_eq!(committed.instance.variables["answer"], 1);
    assert_eq!(repository::get_instance(&fixture.db, &fixture.owner,
        &started.instance_id, None).unwrap().variables, committed.instance.variables);
}

#[test]
fn terminal_branch_cancels_the_script_branch_before_entry() {
    let fixture = Fixture::new();
    let mut model = script_model("1 / 0", BTreeMap::new(), false);
    model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::TerminateEnd;
    model.nodes.insert(1, ProcessNode { id: "Split".into(), name: "Fork".into(),
        kind: ProcessNodeKind::ParallelGateway, repeat: None });
    model.nodes.push(ProcessNode { id: "Terminate".into(), name: "Stop".into(),
        kind: ProcessNodeKind::TerminateEnd, repeat: None });
    model.sequence_flows = vec![edge("ToSplit", "Start_1", "Split"),
        edge("A_Stop", "Split", "Terminate"), edge("Z_Compute", "Split", "Compute"),
        edge("FromCompute", "Compute", "End_1")];
    let started = start_model(&fixture, &model);
    assert_eq!(started.status, ProcessInstanceStatus::Completed);
    let history = repository::list_events(&fixture.db, &fixture.owner,
        &started.instance_id, 0, 200).unwrap().0;
    assert_eq!(history.iter().filter(|event| event.kind == "terminate_end_reached").count(), 1);
    assert!(!history.iter().any(|event| event.kind == "script_completed"
        || event.kind == "incident" && event.node_id.as_deref() == Some("Compute")));
}

// ============ File: script_tests.rs — File-backed ScriptTask behavior tests ============

use super::call_tests::transition_rows;
use super::repository;
use super::runtime::{self, test_support::*};
use super::jobs;
use crate::flow_engine::expr;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tentaflow_protocol::processes::{ProcessInstanceStatus, ProcessModel, ProcessNode, ProcessNodeKind,
    ProcessMultiInstanceInput, ProcessMultiInstanceMode, ProcessRepeatSpec,
    ProcessRepetitionGroupStatus, ProcessRepetitionOccurrenceStatus, ProcessTimerSpec,
    ProcessTimerStatus, ProcessSubProcess, ProcessDiagram};
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
        activity_io: None,
    });
    if terminate {
        model.nodes[2].kind = ProcessNodeKind::TerminateEnd;
    }
    model.sequence_flows = vec![edge("ToCompute", "Start_1", "Compute"),
        edge("FromCompute", "Compute", "End_1")];
    model
}

pub(super) struct PersistedChildScriptHistory {
    pub instance_id: String,
    pub child_scope_id: String,
    pub ready_token_id: String,
    pub predecessor_token_id: String,
    pub revision: u64,
    pub at_ms: i64,
    pub plan: repository::RuntimePlan,
}

pub(super) fn persisted_child_script_history(fixture: &Fixture) -> PersistedChildScriptHistory {
    let mut child = script_model("vars.seed + 1",
        BTreeMap::from([("answer".into(), "outputs".into())]), false);
    child.nodes.insert(1, ProcessNode {
        id: "Review".into(), name: "Review child".into(),
        kind: ProcessNodeKind::UserTask {
            assignee_user_id: Some(fixture.owner.user_id.clone()),
            output_mapping: BTreeMap::new(),
        },
        repeat: None,
        activity_io: None,
    });
    child.sequence_flows = vec![edge("ToReview", "Start_1", "Review"),
        edge("ReviewToCompute", "Review", "Compute"),
        edge("FromCompute", "Compute", "End_1")];
    let mut model = embedded_model(child, "Scope");
    model.variables.insert("answer".into(), Value::Null);
    let scope = model.nodes.iter_mut().find(|node| node.id == "Scope").unwrap();
    let ProcessNodeKind::SubProcess { output_mapping, .. } = &mut scope.kind else { unreachable!() };
    output_mapping.insert("answer".into(), "outputs.answer".into());

    let started = start_model(fixture, &model);
    assert_eq!(started.status, ProcessInstanceStatus::Waiting);
    let before = repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
    let child_scope = before.scopes.iter().find(|scope|
        scope.subprocess_node_id.as_deref() == Some("Scope")).unwrap();
    let parent = before.tokens.iter().find(|token|
        token.node_id == "Scope" && token.scope_id == started.instance_id && token.status == "waiting").unwrap();
    assert_eq!(child_scope.parent_token_id.as_deref(), Some(parent.token_id.as_str()));
    let task = before.user_tasks.iter().find(|task|
        task.node_id == "Review" && task.scope_id == child_scope.scope_id &&
        task.status == tentaflow_protocol::processes::ProcessUserTaskStatus::Open).unwrap();
    let task_id = task.user_task_id.clone();
    let predecessor_token_id = task.token_id.as_deref().unwrap().to_owned();
    let predecessor = before.tokens.iter().find(|token|
        token.token_id == predecessor_token_id && token.status == "waiting").unwrap();
    assert_eq!(predecessor.scope_id, child_scope.scope_id);
    assert_eq!(predecessor.arrival_edge_id.as_deref(), Some("ToReview"));
    let ready_token_id = Uuid::new_v4().to_string();
    let at_ms = started.updated_at_ms + 1;

    // This isolated, FK-checked history represents a retained task completion, not a public task command.
    {
        let mut conn = fixture.db.write().unwrap();
        let tx = conn.transaction().unwrap();
        let changed = tx.execute(
            "UPDATE bpmn_user_tasks SET status='completed',outputs_json='null',revision=revision+1,updated_at_ms=?2 WHERE user_task_id=?1 AND instance_id=?3 AND scope_id=?4 AND token_id=?5 AND status='open' AND revision=?6",
            rusqlite::params![task.user_task_id, at_ms, started.instance_id, child_scope.scope_id,
                predecessor_token_id, i64::try_from(task.revision).unwrap()],
        ).unwrap();
        assert_eq!(changed, 1);
        let changed = tx.execute(
            "UPDATE bpmn_tokens SET status='consumed' WHERE token_id=?1 AND instance_id=?2 AND scope_id=?3 AND node_id='Review' AND status='waiting'",
            rusqlite::params![predecessor_token_id, started.instance_id, child_scope.scope_id],
        ).unwrap();
        assert_eq!(changed, 1);
        tx.execute(
            "INSERT INTO bpmn_tokens(token_id,instance_id,scope_id,node_id,arrival_edge_id,fork_stack_json,status,created_at_ms) VALUES(?1,?2,?3,'Compute','ReviewToCompute',?4,'ready',?5)",
            rusqlite::params![ready_token_id, started.instance_id, child_scope.scope_id,
                serde_json::to_string(&predecessor.fork_stack).unwrap(), at_ms],
        ).unwrap();
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",
            [&started.instance_id], |row| row.get(0),
        ).unwrap();
        tx.execute(
            "INSERT INTO bpmn_events(event_id,instance_id,scope_id,seq,at_ms,kind,node_id,actor_user_id,data_json) VALUES(?1,?2,?3,?4,?5,'user_task_completed','Review',?6,?7)",
            rusqlite::params![Uuid::new_v4().to_string(), started.instance_id, child_scope.scope_id,
                seq, at_ms, fixture.owner.user_id,
                json!({"user_task_id":task.user_task_id,"outputs":null}).to_string()],
        ).unwrap();
        assert_eq!(tx.execute(
            "UPDATE bpmn_instances SET revision=revision+1,updated_at_ms=?2 WHERE instance_id=?1 AND revision=?3 AND status='waiting'",
            rusqlite::params![started.instance_id, at_ms, i64::try_from(started.revision).unwrap()],
        ).unwrap(), 1);
        assert_eq!(tx.execute(
            "UPDATE bpmn_scopes SET revision=revision+1,updated_at_ms=?2 WHERE scope_id=?1 AND instance_id=?3 AND revision=?4",
            rusqlite::params![child_scope.scope_id, at_ms, started.instance_id,
                i64::try_from(child_scope.revision).unwrap()],
        ).unwrap(), 1);
        assert_eq!(tx.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [],
            |row| row.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(tx.query_row("PRAGMA integrity_check", [],
            |row| row.get::<_, String>(0)).unwrap(), "ok");
        tx.commit().unwrap();
    }
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
    assert_eq!(snapshot.instance.revision, started.revision + 1);
    assert!(snapshot.tokens.iter().any(|token|
        token.token_id == ready_token_id && token.scope_id == child_scope.scope_id &&
        token.arrival_edge_id.as_deref() == Some("ReviewToCompute") && token.status == "ready"));
    assert!(snapshot.user_tasks.iter().any(|task|
        task.user_task_id == task_id &&
        task.status == tentaflow_protocol::processes::ProcessUserTaskStatus::Completed));
    let plan = runtime::plan_advance(&snapshot, at_ms + 1, None).unwrap();
    PersistedChildScriptHistory {
        instance_id: started.instance_id, child_scope_id: child_scope.scope_id.clone(),
        ready_token_id, predecessor_token_id, revision: snapshot.instance.revision,
        at_ms: at_ms + 1, plan,
    }
}

#[test]
fn isolated_persisted_ready_child_script_completes_exactly_once_and_reopens() {
    let fixture = Fixture::new();
    let history = persisted_child_script_history(&fixture);
    assert_eq!(history.plan.events.iter().filter(|event| event.kind == "script_completed").count(), 1);
    assert_eq!(history.plan.events.iter().filter(|event| event.kind == "scope_completed").count(), 1);
    let script = history.plan.events.iter().find(|event| event.kind == "script_completed").unwrap();
    assert_eq!(script.scope_id, history.child_scope_id);
    assert_eq!(script.data, json!({"outputs":5}));
    let committed = repository::apply_transition(&fixture.db, &fixture.owner,
        &history.instance_id, history.revision,
        repository::ProcessPlanInput::Supplied(&history.plan), history.at_ms).unwrap();
    let disk = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let reopened = repository::get_instance(&disk, &fixture.owner,
        &history.instance_id, None).unwrap();
    assert_eq!(reopened.status, ProcessInstanceStatus::Completed);
    assert_eq!(reopened.variables["answer"], 5);
    assert_eq!(committed.instance.variables["answer"], 5);
    let events = repository::list_events(&disk, &fixture.owner,
        &history.instance_id, 0, 100).unwrap().0;
    for kind in ["script_completed", "scope_completed"] {
        assert_eq!(events.iter().filter(|event| event.kind == kind).count(), 1);
    }
    let before_replay = super::signal_proof_tests::all_transition_rows(&fixture);
    assert!(repository::apply_transition(&fixture.db, &fixture.owner,
        &history.instance_id, history.revision,
        repository::ProcessPlanInput::Supplied(&history.plan), history.at_ms).is_err());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before_replay);
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
    let plan = runtime::plan_start(&version.model, &version.model.process_id, crate::processes::runtime::test_support::ordinary_start_id(&version.model), &instance_id, &fixture.owner,
        &version.definition_id, version.version, variables.clone(), runtime::StartCause::Manual,
        at_ms, manual_input(&command), None).unwrap();
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
        &instance_id, &version.definition_id, version.version, &variables, None, None, repository::ProcessPlanInput::Supplied(&plan),
        at_ms).unwrap();
    assert_eq!(committed.status, ProcessInstanceStatus::Completed);
    assert_eq!(committed.variables["answer"], 5);
    let persisted = repository::get_instance(&fixture.db, &fixture.owner, &instance_id, None).unwrap();
    assert_eq!(persisted.variables, committed.variables);
    let before_replay = transition_rows(&fixture);
    let replay = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None, repository::ProcessPlanInput::Supplied(&plan),
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
        &instance_id, &version.definition_id, version.version, &variables, None, None, repository::ProcessPlanInput::Supplied(&plan),
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
            &instance_id, &version.definition_id, version.version, &variables, None, None, repository::ProcessPlanInput::Supplied(&plan),
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
        &instance_id, &version.definition_id, version.version, &variables, None, None, repository::ProcessPlanInput::Supplied(&plan),
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
        &instance_id, &version.definition_id, version.version, &variables, None, None, repository::ProcessPlanInput::Supplied(&plan),
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
        activity_io: None,
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
    assert_eq!(retained.status, "running");
    assert!(retained.result.is_none());
    let (invocation_id, phase, observed_json, reserved_event_id, attempt, fence,
        acceptance_owner): (String, String, String, String, u32, i64, Option<String>) =
        fixture.db.read().unwrap().query_row(
            "SELECT invocation_id,phase,observed_result_json,reserved_result_event_id,dispatch_attempt,dispatch_fence,acceptance_owner_id FROM bpmn_service_invocations WHERE job_id=?1",
            [&claimed.job.job_id], |row| Ok((row.get(0)?,row.get(1)?,
                row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?))
        ).unwrap();
    assert_eq!(phase, "observed");
    assert!(!observed_json.is_empty());
    let flow_executions_before: i64 = fixture.db.read().unwrap().query_row(
        "SELECT COUNT(*) FROM flow_executions WHERE flow_id=?1",
        [&flow_id], |row| row.get(0)).unwrap();
    assert_eq!(flow_executions_before, 1);
    assert_eq!(attempt, claimed.job.attempt);
    assert_eq!(fence, claimed.job.fence as i64);
    assert!(acceptance_owner.is_none());
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
    assert_eq!(history.iter().filter(|event| event.kind == "service_result").count(), 0);
    assert_eq!(history.iter().filter(|event| event.kind == "incident"
        && event.data["code"] == "SCRIPT_INFRASTRUCTURE_FAILED"
        && event.data["invocation_id"] == invocation_id
        && event.data["reserved_result_event_id"] == reserved_event_id).count(), 1);
    let before = transition_rows(&fixture);
    assert!(!repository::fail_job(&fixture.db, &claimed.job.job_id,
        claimed.job.attempt, claimed.job.fence, worker,
        "SCRIPT_INFRASTRUCTURE_FAILED", "stale worker replay",
        chrono::Utc::now().timestamp_millis(), None).unwrap());
    assert_eq!(transition_rows(&fixture), before);
    let denied = repository::retry_job(&fixture.db, &fixture.owner, &stamp("retry script"),
        &started.instance_id, &claimed.job.job_id, reopened.revision).unwrap_err();
    assert!(denied.to_string().contains("proved-no-effect invocation"));
    assert_eq!(transition_rows(&fixture), before);
    expr::inject_script_infrastructure_failure("vars.answer + \" b2i-infra-probe\"");
    let pending = repository::recover_jobs(&fixture.db, None,
        chrono::Utc::now().timestamp_millis()).unwrap_err();
    assert!(pending.to_string().contains("durably observed Service result"));
    assert_eq!(transition_rows(&fixture), before);
    let recovered_snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let recovered_job = recovered_snapshot.jobs.iter()
        .find(|job| job.job_id == claimed.job.job_id).unwrap();
    let observed: repository::ObservedActivityResult =
        serde_json::from_str(&observed_json).unwrap();
    let mut forged = runtime::plan_job_result(&recovered_snapshot, recovered_job,
        &observed, chrono::Utc::now().timestamp_millis(), None).unwrap();
    let forged_token = Uuid::new_v4().to_string();
    forged.recovered_service_incidents[0].source_token_id = forged_token.clone();
    let resolved = forged.events.iter_mut()
        .find(|event| event.kind == "incident_resolved").unwrap();
    resolved.data["source_token_id"] = json!(forged_token);
    let rejected = repository::accept_job_result(&fixture.db, &fixture.owner,
        &claimed.job.job_id, claimed.job.attempt, claimed.job.fence,
        "forged-script-recovery-controller", &observed,
        recovered_snapshot.instance.revision,
        repository::ProcessPlanInput::Supplied(&forged),
        chrono::Utc::now().timestamp_millis()).unwrap_err();
    assert!(rejected.to_string().contains("original infrastructure source"));
    assert_eq!(transition_rows(&fixture), before);
    let mut conflicting_observation = observed.clone();
    conflicting_observation.result.outputs = json!({"answer":"forged"});
    let rejected = repository::record_job_observation(&fixture.db, &claimed,
        worker, &conflicting_observation,
        chrono::Utc::now().timestamp_millis()).unwrap_err();
    assert!(rejected.to_string().contains("observed")
        || rejected.to_string().contains("result"));
    assert_eq!(transition_rows(&fixture), before);
    assert_eq!(repository::recover_jobs(&fixture.db, None,
        chrono::Utc::now().timestamp_millis()).unwrap(), 1);
    assert_eq!(repository::recover_jobs(&fixture.db, None,
        chrono::Utc::now().timestamp_millis()).unwrap(), 0);
    let completed = repository::get_instance(&fixture.db, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert!(completed.incidents.is_empty());
    let (final_phase, final_result, final_reserved_event, final_attempt, final_fence):
        (String, Option<String>, String, u32, i64) =
        fixture.db.read().unwrap().query_row(
            "SELECT v.phase,j.result_json,v.reserved_result_event_id,j.attempt,j.fence FROM bpmn_service_invocations v JOIN bpmn_jobs j ON j.job_id=v.job_id WHERE v.invocation_id=?1",
            [&invocation_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,
                row.get(3)?,row.get(4)?))).unwrap();
    assert_eq!(final_phase, "accepted");
    assert!(final_result.is_some());
    assert_eq!(final_reserved_event, reserved_event_id);
    assert_eq!(final_attempt, claimed.job.attempt);
    assert_eq!(final_fence, claimed.job.fence as i64);
    let history = repository::list_events(&fixture.db, &fixture.owner,
        &started.instance_id, 0, 200).unwrap().0;
    assert_eq!(history.iter().filter(|event| event.kind == "service_queued").count(), 1);
    assert_eq!(history.iter().filter(|event| event.kind == "service_result").count(), 1);
    assert_eq!(history.iter().filter(|event| event.kind == "script_completed").count(), 1);
    let flow_executions_after: i64 = fixture.db.read().unwrap().query_row(
        "SELECT COUNT(*) FROM flow_executions WHERE flow_id=?1",
        [&flow_id], |row| row.get(0)).unwrap();
    assert_eq!(flow_executions_after, flow_executions_before);
    assert_eq!(history.iter().filter(|event| event.kind == "incident_resolved"
        && event.data["incident_id"] == reopened.incidents[0].incident_id).count(), 1);
}

#[tokio::test]
async fn cancelling_recoverable_script_failure_closes_only_its_original_observed_dispatch() {
    for interrupting_boundary in [false, true] {
        let fixture = Fixture::new();
        let script = "vars.answer + \" b2i-cancel-probe\"";
        let flow_id = flow(&fixture.db, &fixture.owner, &graph("script cancel", None));
        let mut model = service_model(&flow_id, ActivityVerification::Condition {
            expression: "true".into(),
        });
        model.nodes.insert(2, ProcessNode {
            id: "Compute".into(), name: "Compute".into(),
            kind: ProcessNodeKind::ScriptTask {
                script: script.into(), output_mapping: BTreeMap::new(),
            }, repeat: None, activity_io: None,
        });
        model.sequence_flows = vec![edge("ToService", "Start_1", "Service"),
            edge("ToScript", "Service", "Compute"),
            edge("ToEnd", "Compute", "End_1")];
        if interrupting_boundary {
            model.timer_timezone = Some("UTC".into());
            model.nodes.push(ProcessNode {
                id: "ServiceDeadline".into(), name: "Service deadline".into(),
                kind: ProcessNodeKind::BoundaryTimer {
                    attached_to_id: "Service".into(), cancel_activity: true,
                    timer: ProcessTimerSpec::Duration { seconds: 1 },
                }, repeat: None, activity_io: None,
            });
            model.sequence_flows.push(edge("DeadlineEnd", "ServiceDeadline", "End_1"));
        }
        let started = start_model(&fixture, &model);
        let worker = "script-cancel-worker";
        let claimed = repository::claim_job(&fixture.db, worker,
            chrono::Utc::now().timestamp_millis()).unwrap().unwrap();
        expr::inject_script_infrastructure_failure(script);
        jobs::execute_claimed(&fixture.db, fixture.dispatcher(), worker, claimed.clone(),
            CancellationToken::new()).await.unwrap();
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        assert_eq!(snapshot.instance.status, ProcessInstanceStatus::Incident);
        let diagnostic = snapshot.incidents.iter().find(|incident|
            incident.code == "SCRIPT_INFRASTRUCTURE_FAILED").unwrap();
        let invocation: (String, String, String, i64) = fixture.db.read().unwrap().query_row(
            "SELECT invocation_id,observed_result_json,reserved_result_event_id,dispatch_fence FROM bpmn_service_invocations WHERE job_id=?1",
            [&claimed.job.job_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        ).unwrap();
        let observed: repository::ObservedActivityResult =
            serde_json::from_str(&invocation.1).unwrap();
        let original_plan = runtime::plan_job_result(&snapshot,
            snapshot.jobs.iter().find(|job| job.job_id == claimed.job.job_id).unwrap(),
            &observed, chrono::Utc::now().timestamp_millis(), None).unwrap();
        let before = super::signal_proof_tests::all_transition_rows(&fixture);
        if interrupting_boundary {
            let timer = snapshot.timers.iter().find(|timer|
                timer.node_id == "ServiceDeadline" && timer.status == ProcessTimerStatus::Pending)
                .unwrap();
            let due = timer.due_at_ms.unwrap() + 1;
            let candidate = repository::due_timers(&fixture.db, due, 32).unwrap()
                .into_iter().find(|row| row.timer_id == timer.timer_id).unwrap();
            let timer_snapshot = repository::timer_snapshot(&fixture.db, &candidate).unwrap();
            let plan = super::timers::plan_timer_fire(&timer_snapshot, due, None, None).unwrap();
            assert_eq!(plan.closed_service_dispatches.len(), 1);
            assert_eq!(plan.closed_service_dispatches[0].infrastructure_incident_id.as_deref(),
                Some(diagnostic.incident_id.as_str()));
            let mut forged = plan.clone();
            forged.closed_service_dispatches[0].infrastructure_incident_id =
                Some(Uuid::new_v4().to_string());
            assert!(repository::fire_timer(&fixture.db, &candidate, &fixture.owner,
                Some(snapshot.instance.revision), repository::ProcessPlanInput::Supplied(&forged),
                due).is_err());
            assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
            let mut forged_event = plan.clone();
            forged_event.events.iter_mut().find(|event|
                event.kind == "incident_resolved"
                    && event.data["incident_id"] == diagnostic.incident_id)
                .unwrap().data["reason"] = json!("script_infrastructure_recovered");
            assert!(repository::fire_timer(&fixture.db, &candidate, &fixture.owner,
                Some(snapshot.instance.revision),
                repository::ProcessPlanInput::Supplied(&forged_event), due).is_err());
            assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
            let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
            repository::fire_timer(&reopened, &candidate, &fixture.owner,
                Some(snapshot.instance.revision), repository::ProcessPlanInput::Supplied(&plan),
                due).unwrap().unwrap();
        } else {
            let command = stamp("cancel script infrastructure");
            repository::cancel_instance(&fixture.db, &fixture.owner,
                &command, &started.instance_id,
                snapshot.instance.revision).unwrap();
            let cancelled_rows = super::signal_proof_tests::all_transition_rows(&fixture);
            repository::cancel_instance(&fixture.db, &fixture.owner,
                &command, &started.instance_id, snapshot.instance.revision).unwrap();
            assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), cancelled_rows);
        }
        let closed_rows = super::signal_proof_tests::all_transition_rows(&fixture);
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        assert!(repository::accept_job_result(&reopened, &fixture.owner,
            &claimed.job.job_id, claimed.job.attempt, claimed.job.fence,
            "stale-script-controller", &observed, snapshot.instance.revision,
            repository::ProcessPlanInput::Supplied(&original_plan),
            chrono::Utc::now().timestamp_millis()).is_err());
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), closed_rows);
        assert!(matches!(repository::record_job_observation(&reopened, &claimed,
            worker, &observed, chrono::Utc::now().timestamp_millis()).unwrap(),
            repository::JobObservationState::Blocked));
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), closed_rows);
        assert_eq!(repository::recover_jobs(&reopened, None,
            chrono::Utc::now().timestamp_millis()).unwrap(), 0);
        let final_instance = repository::get_instance(&reopened, &fixture.owner,
            &started.instance_id, None).unwrap();
        assert_eq!(final_instance.status, if interrupting_boundary {
            ProcessInstanceStatus::Completed
        } else { ProcessInstanceStatus::Cancelled });
        assert!(!final_instance.can_retry);
        let events = repository::list_events(&reopened, &fixture.owner,
            &started.instance_id, 0, 200).unwrap().0;
        assert_eq!(events.iter().filter(|event| event.kind == "service_result").count(), 0);
        assert_eq!(events.iter().filter(|event| event.kind == "incident_resolved"
            && event.data["incident_id"] == diagnostic.incident_id
            && event.data["invocation_id"] == invocation.0
            && event.data["reserved_result_event_id"] == invocation.2
            && event.data["dispatch_fence"] == invocation.3
            && event.data["reason"] == "activity_cancelled_after_dispatch").count(), 1);
        let conn = reopened.read().unwrap();
        let (phase, blocked_reason, original_bytes, reserved_event, dispatch_fence):
            (String, String, String, String, i64) = conn.query_row(
            "SELECT phase,blocked_reason,observed_result_json,reserved_result_event_id,dispatch_fence FROM bpmn_service_invocations WHERE invocation_id=?1",
            [&invocation.0], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,
                row.get(3)?,row.get(4)?))).unwrap();
        assert_eq!((phase.as_str(), blocked_reason.as_str()),
            ("observed_blocked", "activation_closed"));
        assert_eq!((original_bytes, reserved_event, dispatch_fence),
            (invocation.1, invocation.2, invocation.3));
        let unresolved: i64 = conn.query_row(
            "SELECT COUNT(*) FROM bpmn_incidents WHERE instance_id=?1 AND resolved_at_ms IS NULL",
            [&started.instance_id], |row| row.get(0)).unwrap();
        assert_eq!(unresolved, 0);
        drop(conn);
        assert_eq!(crate::db::repository::list_flow_executions_for_flow(
            &reopened, &flow_id, 100).unwrap().len(), 1);
    }
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
        activity_io: None,
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
        activity_io: None,
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
        human_input(&waiting, &task.user_task_id, &command), None).unwrap();
    assert_eq!(plan.events.iter().filter(|event|
        event.kind == "script_completed").count(), 1);
    let committed = repository::complete_user_task(&fixture.db, &fixture.owner,
        &command, &started.instance_id, &task.user_task_id,
        waiting.instance.revision, &Value::Null, Some(true), repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().instance;
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
        repeat: None, activity_io: None,});
    model.nodes.push(ProcessNode { id: "OtherEnd".into(), name: "Other result".into(),
        kind: ProcessNodeKind::End, repeat: None, activity_io: None,});
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
        &instance_id, &version.definition_id, version.version, &variables, None, None, repository::ProcessPlanInput::Supplied(&forged),
        at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let committed = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None, repository::ProcessPlanInput::Supplied(&plan),
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
        &instance_id, &version.definition_id, version.version, &variables, None, None, repository::ProcessPlanInput::Supplied(&forged),
        at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let committed = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None, repository::ProcessPlanInput::Supplied(&plan),
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
        kind: ProcessNodeKind::InclusiveGateway { default_flow_id: None }, repeat: None, activity_io: None,});
    model.nodes.insert(3, ProcessNode { id: "A".into(), name: "Selected work".into(),
        kind: ProcessNodeKind::UserTask { assignee_user_id: Some(fixture.owner.user_id.clone()),
            output_mapping: BTreeMap::new() }, repeat: None, activity_io: None,});
    model.nodes.insert(4, ProcessNode { id: "B".into(), name: "Unselected work".into(),
        kind: ProcessNodeKind::UserTask { assignee_user_id: Some(fixture.owner.user_id.clone()),
            output_mapping: BTreeMap::new() }, repeat: None, activity_io: None,});
    model.nodes.insert(5, ProcessNode { id: "Join".into(), name: "Join selected work".into(),
        kind: ProcessNodeKind::InclusiveGateway { default_flow_id: None }, repeat: None, activity_io: None,});
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
        &instance_id, &version.definition_id, version.version, &variables, None, None, repository::ProcessPlanInput::Supplied(&forged),
        at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let started = repository::start_instance(&fixture.db, &fixture.owner, &start_command,
        &instance_id, &version.definition_id, version.version, &variables, None, None, repository::ProcessPlanInput::Supplied(&start_plan),
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
        human_input(&snapshot, &task.user_task_id, &command), None).unwrap();
    let committed = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &started.instance_id, &task.user_task_id, snapshot.instance.revision,
        &outputs, None, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
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
        }, repeat: None, activity_io: None,});
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
        human_input(&snapshot, &task.user_task_id, &command), None).unwrap();
    assert_eq!(plan.events.iter().filter(|event| event.kind == "script_completed").count(), 1);
    let mut forged = plan.clone();
    let script = forged.events.iter_mut().find(|event| event.kind == "script_completed").unwrap();
    script.data = json!({"outputs":2});
    let before = transition_rows(&fixture);
    assert!(repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &started.instance_id, &task.user_task_id, snapshot.instance.revision,
        &outputs, None, repository::ProcessPlanInput::Supplied(&forged), at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let committed = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &started.instance_id, &task.user_task_id, snapshot.instance.revision,
        &outputs, None, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
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
        kind: ProcessNodeKind::ParallelGateway, repeat: None, activity_io: None,});
    model.nodes.push(ProcessNode { id: "Terminate".into(), name: "Stop".into(),
        kind: ProcessNodeKind::TerminateEnd, repeat: None, activity_io: None,});
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

#[test]
fn repeated_script_uses_frozen_items_and_commits_distinct_body_sources() {
    let fixture = Fixture::new();
    let mut model = script_model("{\"value\": repeat.item + vars.seed}", BTreeMap::new(), false);
    model.variables.insert("items".into(), json!([2, 7]));
    model.variables.insert("results".into(), json!([]));
    model.nodes.iter_mut().find(|node| node.id == "Compute").unwrap().repeat =
        Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Parallel,
            input: ProcessMultiInstanceInput::CollectionExpression {
                expression: "vars.items".into(),
            },
            output_collection_variable: "results".into(),
        });
    let (command, instance_id, plan, variables, version, at_ms) = plan_and_start(&fixture, &model);
    let bodies = plan.events.iter().enumerate().filter(|(_, event)|
        event.kind == "script_completed").collect::<Vec<_>>();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0].1.data, json!({"outputs":{"value":6}}));
    assert_eq!(bodies[1].1.data, json!({"outputs":{"value":11}}));
    assert!(bodies.iter().all(|(index, _)| plan.event_ids.contains_key(index)));
    assert_eq!(plan.events.iter().filter(|event|
        event.kind == "repetition_occurrence_completed").count(), 2);
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let mut wrong_item = plan.clone();
    wrong_item.repetition_occurrences.iter_mut().find(|row| row.ordinal == 1).unwrap().item =
        json!(99);
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&wrong_item), at_ms).is_err());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
    let mut forged = plan.clone();
    forged.repetition_occurrences.iter_mut().find(|row| row.ordinal == 1).unwrap()
        .accepted_source_event_id = Some(plan.event_ids[&bodies[0].0].clone());
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&forged), at_ms).is_err());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
    let mut missing_body = plan.clone();
    missing_body.events[bodies[1].0].data = json!({"outputs":{"value":12}});
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&missing_body), at_ms).is_err());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
    let completed = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert_eq!(completed.variables["results"], json!([{"value":6},{"value":11}]));
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner, &instance_id).unwrap();
    assert_eq!(snapshot.repetition_groups[0].completed_count, 2);
    assert!(snapshot.repetition_occurrences.iter().all(|row|
        row.status == ProcessRepetitionOccurrenceStatus::Completed));
    let rows = super::signal_proof_tests::all_transition_rows(&fixture);
    repository::start_instance(&reopened, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), rows);
}

#[test]
fn failed_repeated_script_body_parks_unaccepted_ordinal_without_aggregate() {
    let fixture = Fixture::new();
    let mut model = script_model("1 / 0", BTreeMap::new(), false);
    model.variables.insert("results".into(), json!([]));
    model.nodes.iter_mut().find(|node| node.id == "Compute").unwrap().repeat =
        Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
    let (command, instance_id, plan, variables, version, at_ms) = plan_and_start(&fixture, &model);
    assert_eq!(plan.add_incidents.len(), 1);
    assert_eq!(plan.add_incidents[0].code, "SCRIPT_EVALUATION_FAILED");
    assert_eq!(plan.repetition_groups[0].status, ProcessRepetitionGroupStatus::Incident);
    assert_eq!(plan.repetition_groups[0].terminal_incident_id.as_deref(),
        Some(plan.add_incidents[0].incident_id.as_str()));
    assert!(plan.repetition_groups[0].terminal_event_id.is_none());
    assert_eq!(plan.repetition_occurrences[0].status, ProcessRepetitionOccurrenceStatus::Active);
    assert!(plan.repetition_occurrences[0].accepted_source_event_id.is_none());
    assert!(!plan.events.iter().any(|event| matches!(event.kind.as_str(),
        "script_completed" | "repetition_occurrence_completed" | "repetition_group_blocked")));
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let mut forged = plan.clone();
    forged.repetition_groups[0].terminal_incident_id = Some(Uuid::new_v4().to_string());
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&forged), at_ms).is_err());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
    let incident_index = plan.events.iter().position(|event| event.kind == "incident").unwrap();
    let ready_id = plan.token_sources[&plan.repetition_occurrences[0].token_id].clone();
    let mut wrong_message = plan.clone();
    wrong_message.events[incident_index].data["message"] = json!("forged failure");
    let mut wrong_ordinal = plan.clone();
    wrong_ordinal.repetition_occurrences[0].ordinal = 1;
    let mut wrong_input = plan.clone();
    wrong_input.repetition_occurrences[0].input_variables["seed"] = json!(999);
    let mut wrong_item = plan.clone();
    wrong_item.repetition_occurrences[0].item = json!("foreign item");
    let mut wrong_wait = plan.clone();
    wrong_wait.create_tokens.iter_mut().find(|token|
        token.token_id == plan.repetition_occurrences[0].token_id).unwrap().arrival_edge_id =
        Some("foreign edge".into());
    let mut forged_source = plan.clone();
    forged_source.event_sources.insert(incident_index, ready_id);
    for mutant in [wrong_message, wrong_ordinal, wrong_input, wrong_item, wrong_wait,
        forged_source] {
        assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
            &instance_id, &version.definition_id, version.version, &variables, None, None,
            repository::ProcessPlanInput::Supplied(&mutant), at_ms).is_err());
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
    }
    repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner, &instance_id).unwrap();
    assert_eq!(snapshot.instance.status, ProcessInstanceStatus::Incident);
    assert_eq!(snapshot.repetition_groups[0].completed_count, 0);
    assert_eq!(snapshot.repetition_occurrences[0].status, ProcessRepetitionOccurrenceStatus::Active);
    let committed_rows = super::signal_proof_tests::all_transition_rows(&fixture);
    repository::start_instance(&reopened, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), committed_rows);
    repository::cancel_instance(&reopened, &fixture.owner,
        &stamp("cancel failed repeated Script"), &instance_id, snapshot.instance.revision).unwrap();
    let cancelled = repository::runtime_snapshot(&reopened, &fixture.owner, &instance_id).unwrap();
    assert_eq!(cancelled.instance.status, ProcessInstanceStatus::Cancelled);
    assert_eq!(cancelled.repetition_groups[0].status, ProcessRepetitionGroupStatus::Cancelled);
    assert_eq!(cancelled.repetition_occurrences[0].status,
        ProcessRepetitionOccurrenceStatus::Cancelled);
}

#[test]
fn repeated_script_mapping_failure_retains_body_without_ordinal_completion() {
    let fixture = Fixture::new();
    let mut model = script_model("{\"value\": repeat.index}",
        BTreeMap::from([("step".into(), "1 / 0".into())]), false);
    model.variables.insert("results".into(), json!([]));
    model.variables.insert("step".into(), json!(0));
    model.nodes.iter_mut().find(|node| node.id == "Compute").unwrap().repeat =
        Some(ProcessRepeatSpec::StructuredLoop {
            condition: "vars.step < 2".into(), test_before: false,
            max_iterations: 3, output_collection_variable: "results".into(),
        });
    let (command, instance_id, plan, variables, version, at_ms) = plan_and_start(&fixture, &model);
    let body = plan.events.iter().position(|event| event.kind == "script_completed").unwrap();
    let incident = plan.events.iter().position(|event|
        event.kind == "incident" && event.data["code"] == "REPETITION_MAPPING_FAILED").unwrap();
    let blocked = plan.events.iter().position(|event| event.kind == "repetition_group_blocked").unwrap();
    assert!(body < incident && incident < blocked);
    assert_eq!(plan.repetition_occurrences[0].status,
        ProcessRepetitionOccurrenceStatus::AcceptedBlocked);
    assert_eq!(plan.repetition_occurrences[0].accepted_source_event_id.as_deref(),
        plan.event_ids.get(&body).map(String::as_str));
    assert_eq!(plan.repetition_groups[0].completed_count, 0);
    assert!(!plan.events.iter().any(|event| event.kind == "repetition_occurrence_completed"));
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let mut forged = plan.clone();
    forged.events[body].data = json!({"outputs":{"value":99}});
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&forged), at_ms).is_err());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
    repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner, &instance_id).unwrap();
    assert_eq!(snapshot.instance.status, ProcessInstanceStatus::Incident);
    assert_eq!(snapshot.repetition_groups[0].completed_count, 0);
    assert_eq!(snapshot.repetition_occurrences[0].status,
        ProcessRepetitionOccurrenceStatus::AcceptedBlocked);
}

#[test]
fn outer_timer_closes_the_real_wait_after_repeated_script_body_failure() {
    let fixture = Fixture::new();
    let mut model = script_model("1 / 0", BTreeMap::new(), false);
    model.variables.insert("results".into(), json!([]));
    model.timer_timezone = Some("UTC".into());
    model.nodes.iter_mut().find(|node| node.id == "Compute").unwrap().repeat =
        Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
    model.nodes.extend([
        ProcessNode { id: "OuterTimer".into(), name: "Outer timer".into(),
            kind: ProcessNodeKind::BoundaryTimer {
                attached_to_id: "Compute".into(), cancel_activity: true,
                timer: ProcessTimerSpec::Duration { seconds: 1 },
            }, repeat: None, activity_io: None,},
        ProcessNode { id: "TimeoutEnd".into(), name: "Timeout end".into(),
            kind: ProcessNodeKind::End, repeat: None, activity_io: None,},
    ]);
    model.sequence_flows.push(edge("ToTimeoutEnd", "OuterTimer", "TimeoutEnd"));
    let version = publish_model(&fixture, &model);
    let instance_id = Uuid::new_v4().to_string();
    let command = stamp("script outer timer");
    let variables = serde_json::to_value(&model.variables).unwrap();
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&version.model, &version.model.process_id, crate::processes::runtime::test_support::ordinary_start_id(&version.model), &instance_id, &fixture.owner,
        &version.definition_id, version.version, variables.clone(), runtime::StartCause::Manual,
        at_ms, manual_input(&command), None).unwrap();
    assert_eq!(plan.create_timers.iter().filter(|timer|
        timer.node_id == "OuterTimer").count(), 1);
    repository::start_instance(&fixture.db, &fixture.owner, &command, &instance_id,
        &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &instance_id).unwrap();
    assert_eq!(snapshot.instance.status, ProcessInstanceStatus::Incident);
    let timer = snapshot.timers.iter().find(|timer|
        timer.node_id == "OuterTimer" && timer.status == ProcessTimerStatus::Pending).unwrap();
    assert_eq!(timer.token_id.as_deref(),
        Some(snapshot.repetition_groups[0].parent_token_id.as_str()));
    let due = timer.due_at_ms.unwrap() + 1;
    let candidate = repository::due_timers(&fixture.db, due, 32).unwrap().into_iter()
        .find(|candidate| candidate.timer_id == timer.timer_id).unwrap();
    let timer_snapshot = repository::timer_snapshot(&fixture.db, &candidate).unwrap();
    let fire = super::timers::plan_timer_fire(&timer_snapshot, due, None, None).unwrap();
    assert_eq!(fire.repetition_groups.iter().filter(|group|
        group.group_id == snapshot.repetition_groups[0].group_id
            && group.status == ProcessRepetitionGroupStatus::Cancelled).count(), 1);
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let mut forged = fire.clone();
    forged.repetition_occurrences[0].status = ProcessRepetitionOccurrenceStatus::Active;
    assert!(repository::fire_timer(&fixture.db, &candidate, &fixture.owner,
        Some(snapshot.instance.revision), repository::ProcessPlanInput::Supplied(&forged),
        due).is_err());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    repository::fire_timer(&reopened, &candidate, &fixture.owner,
        Some(snapshot.instance.revision), repository::ProcessPlanInput::Supplied(&fire),
        due).unwrap();
    let closed = repository::runtime_snapshot(&reopened, &fixture.owner, &instance_id).unwrap();
    assert_eq!(closed.repetition_groups[0].status, ProcessRepetitionGroupStatus::Cancelled);
    assert!(closed.repetition_occurrences.iter().all(|row|
        row.status == ProcessRepetitionOccurrenceStatus::Cancelled));
    assert!(closed.timers.iter().all(|timer| timer.status != ProcessTimerStatus::Pending));
}

#[test]
fn repeated_script_joined_worker_failure_aborts_the_whole_start() {
    let fixture = Fixture::new();
    let script = "repeat.index + vars.seed + 4100";
    let mut model = script_model(script, BTreeMap::new(), false);
    model.variables.insert("results".into(), json!([]));
    model.nodes.iter_mut().find(|node| node.id == "Compute").unwrap().repeat =
        Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
    let version = publish_model(&fixture, &model);
    let instance_id = Uuid::new_v4().to_string();
    let command = stamp("joined repeated Script failure");
    let variables = serde_json::to_value(&model.variables).unwrap();
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    expr::inject_script_infrastructure_failure(script);
    let denied = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Canonical,
        chrono::Utc::now().timestamp_millis()).unwrap_err();
    assert!(format!("{denied:#}").contains("injected joined worker failure"));
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
}

#[test]
fn repeated_script_mapping_worker_failure_aborts_without_body_fact() {
    let fixture = Fixture::new();
    let mapping_expression = "outputs.value + 4101";
    let mut model = script_model("{\"value\": repeat.index}",
        BTreeMap::from([("step".into(), mapping_expression.into())]), false);
    model.variables.insert("results".into(), json!([]));
    model.variables.insert("step".into(), json!(0));
    model.nodes.iter_mut().find(|node| node.id == "Compute").unwrap().repeat =
        Some(ProcessRepeatSpec::StructuredLoop {
            condition: "vars.step < 2".into(), test_before: false,
            max_iterations: 3, output_collection_variable: "results".into(),
        });
    let version = publish_model(&fixture, &model);
    let instance_id = Uuid::new_v4().to_string();
    let command = stamp("joined repeated Script mapping failure");
    let variables = serde_json::to_value(&model.variables).unwrap();
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    expr::inject_script_infrastructure_failure(mapping_expression);
    let denied = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Canonical,
        chrono::Utc::now().timestamp_millis()).unwrap_err();
    assert!(format!("{denied:#}").contains("injected joined worker failure"));
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
}

#[test]
fn outer_message_cancels_the_failed_script_group_from_its_single_parent_arm() {
    use super::messages::test_support::{catch_target, envelope, send};
    use tentaflow_protocol::processes::ProcessSubscriptionStatus;

    let fixture = Fixture::new();
    let mut base = script_model("1 / 0", BTreeMap::new(), false);
    base.variables.insert("results".into(), json!([]));
    base.nodes.iter_mut().find(|node| node.id == "Compute").unwrap().repeat =
        Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
    let model = super::messages::test_support::boundary_messages(base, "Compute",
        &[("OuterMessage", true, "ScriptDeadline")]);
    let version = publish_model(&fixture, &model);
    let started = super::messages::test_support::start_version(&fixture, &version);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(snapshot.repetition_groups[0].status, ProcessRepetitionGroupStatus::Incident);
    let arms = snapshot.subscriptions.iter().filter(|sub|
        sub.node_id == "OuterMessage" && sub.status == ProcessSubscriptionStatus::Open)
        .collect::<Vec<_>>();
    assert_eq!(arms.len(), 1);
    assert_eq!(arms[0].token_id, snapshot.repetition_groups[0].parent_token_id);
    let mut message = envelope(catch_target(&version, Some(&started.instance_id),
        Some(&arms[0].subscription_id)), json!({"deadline":true}));
    message.message_name = "ScriptDeadline".into();
    let sent = send(&fixture, &message);
    let at = sent.received_at_ms + 1;
    let candidate = repository::due_messages(&fixture.db, at, 32).unwrap().into_iter()
        .find(|candidate| candidate.key.message_id == sent.message_id).unwrap();
    let repository::MessageSelection::Ready(prepared) =
        repository::message_snapshot(&fixture.db, &candidate).unwrap() else {
            panic!("outer Script message was not a factual candidate")
        };
    let delivery = super::messages::plan_message_delivery(&prepared, at, None).unwrap();
    assert_eq!(delivery.repetition_groups.iter().filter(|group|
        group.group_id == snapshot.repetition_groups[0].group_id
            && group.status == ProcessRepetitionGroupStatus::Cancelled).count(), 1);
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let mut forged = delivery.clone();
    forged.repetition_occurrences[0].status = ProcessRepetitionOccurrenceStatus::Active;
    assert!(repository::deliver_message(&fixture.db, &prepared,
        repository::ProcessPlanInput::Supplied(&forged), at).is_err());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    repository::deliver_message(&reopened, &prepared,
        repository::ProcessPlanInput::Supplied(&delivery), at).unwrap().unwrap();
    let after = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(after.repetition_groups[0].status, ProcessRepetitionGroupStatus::Cancelled);
    assert!(after.repetition_occurrences.iter().all(|row|
        row.status == ProcessRepetitionOccurrenceStatus::Cancelled));
    let rows = super::signal_proof_tests::all_transition_rows(&fixture);
    assert!(repository::deliver_message(&reopened, &prepared,
        repository::ProcessPlanInput::Supplied(&delivery), at).unwrap().is_none());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), rows);
}

#[test]
fn repeated_script_in_embedded_scope_closes_only_its_factual_child() {
    let fixture = Fixture::new();
    let mut model = super::model::starter_model();
    model.timer_timezone = Some("UTC".into());
    let body = ProcessSubProcess {
        nodes: vec![
            ProcessNode { id: "LocalStart".into(), name: "Start locally".into(),
                kind: ProcessNodeKind::Start, repeat: None, activity_io: None,},
            ProcessNode { id: "LocalCompute".into(), name: "Compute locally".into(),
                kind: ProcessNodeKind::ScriptTask {
                    script: "{\"index\": repeat.index}".into(),
                    output_mapping: BTreeMap::new(),
                },
                repeat: Some(ProcessRepeatSpec::MultiInstance {
                    mode: ProcessMultiInstanceMode::Sequential,
                    input: ProcessMultiInstanceInput::Cardinality { count: 2 },
                    output_collection_variable: "results".into(),
                }),
                activity_io: None,
            },
            ProcessNode { id: "LocalEnd".into(), name: "End locally".into(),
                kind: ProcessNodeKind::End, repeat: None, activity_io: None,},
            ProcessNode { id: "LocalOuterTimer".into(), name: "Local outer timer".into(),
                kind: ProcessNodeKind::BoundaryTimer {
                    attached_to_id: "LocalCompute".into(), cancel_activity: true,
                    timer: ProcessTimerSpec::Duration { seconds: 1 },
                }, repeat: None, activity_io: None,},
            ProcessNode { id: "LocalTimeoutEnd".into(), name: "Local timeout".into(),
                kind: ProcessNodeKind::End, repeat: None, activity_io: None,},
        ],
        sequence_flows: vec![edge("LocalToCompute", "LocalStart", "LocalCompute"),
            edge("LocalToEnd", "LocalCompute", "LocalEnd"),
            edge("LocalTimeout", "LocalOuterTimer", "LocalTimeoutEnd")],
        variables: BTreeMap::from([("results".into(), json!([]))]),
        diagram: ProcessDiagram::default(),
        modeling: None,
    };
    model.nodes.insert(1, ProcessNode { id: "Scope".into(), name: "Scope".into(),
        kind: ProcessNodeKind::SubProcess { body, input_mapping: BTreeMap::new(),
            output_mapping: BTreeMap::new() }, repeat: None, activity_io: None,});
    model.sequence_flows = vec![edge("ToScope", "Start_1", "Scope"),
        edge("FromScope", "Scope", "End_1")];
    let started = start_model(&fixture, &model);
    assert_eq!(started.status, ProcessInstanceStatus::Completed);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(snapshot.repetition_groups.len(), 1);
    assert_ne!(snapshot.repetition_groups[0].scope_id, started.instance_id);
    assert_eq!(snapshot.repetition_groups[0].completed_count, 2);
    assert_eq!(snapshot.repetition_occurrences.len(), 2);
    assert!(snapshot.repetition_occurrences.iter().all(|row|
        row.status == ProcessRepetitionOccurrenceStatus::Completed));
    assert_eq!(snapshot.timers.iter().filter(|timer|
        timer.node_id == "LocalOuterTimer").count(), 1);
    assert!(snapshot.timers.iter().filter(|timer|
        timer.node_id == "LocalOuterTimer").all(|timer|
            timer.status == ProcessTimerStatus::Cancelled));
    let history = repository::list_events(&reopened, &fixture.owner,
        &started.instance_id, 0, 200).unwrap().0;
    assert_eq!(history.iter().filter(|event| event.kind == "script_completed").count(), 2);
    assert_eq!(history.iter().filter(|event| event.kind == "scope_completed").count(), 1);
}

#[test]
fn repeated_script_in_pinned_called_child_returns_once() {
    let fixture = Fixture::new();
    let mut child = script_model("{\"index\": repeat.index}", BTreeMap::new(), false);
    child.variables.insert("results".into(), json!([]));
    child.timer_timezone = Some("UTC".into());
    child.nodes.iter_mut().find(|node| node.id == "Compute").unwrap().repeat =
        Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
    child.nodes.extend([
        ProcessNode { id: "ChildOuterTimer".into(), name: "Child outer timer".into(),
            kind: ProcessNodeKind::BoundaryTimer {
                attached_to_id: "Compute".into(), cancel_activity: true,
                timer: ProcessTimerSpec::Duration { seconds: 1 },
            }, repeat: None, activity_io: None,},
        ProcessNode { id: "ChildTimeoutEnd".into(), name: "Child timeout".into(),
            kind: ProcessNodeKind::End, repeat: None, activity_io: None,},
    ]);
    child.sequence_flows.push(edge("ChildTimeout", "ChildOuterTimer", "ChildTimeoutEnd"));
    let child_version = publish_model(&fixture, &child);
    let caller = super::call_tests::caller(&child_version, BTreeMap::new());
    let parent = start_model(&fixture, &caller);
    assert_eq!(parent.status, ProcessInstanceStatus::Completed);
    let child_id = super::call_tests::child_id(&fixture, &parent.instance_id);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let child_snapshot = repository::runtime_snapshot(&reopened, &fixture.owner, &child_id).unwrap();
    assert_eq!(child_snapshot.instance.version, child_version.version);
    assert_eq!(child_snapshot.instance.variables["results"], json!([{"index":0},{"index":1}]));
    assert_eq!(child_snapshot.repetition_groups[0].completed_count, 2);
    assert_eq!(child_snapshot.timers.iter().filter(|timer|
        timer.node_id == "ChildOuterTimer").count(), 1);
    assert!(child_snapshot.timers.iter().filter(|timer|
        timer.node_id == "ChildOuterTimer").all(|timer|
            timer.status == ProcessTimerStatus::Cancelled));
    let parent_snapshot = repository::runtime_snapshot(&reopened, &fixture.owner,
        &parent.instance_id).unwrap();
    assert_eq!(parent_snapshot.calls.len(), 1);
    assert_eq!(parent_snapshot.calls[0].child_instance_id, child_id);
    assert_eq!(parent_snapshot.instance.status, ProcessInstanceStatus::Completed);
    let history = repository::list_events(&reopened, &fixture.owner, &child_id, 0, 200).unwrap().0;
    assert_eq!(history.iter().filter(|event| event.kind == "script_completed").count(), 2);
    assert_eq!(history.iter().filter(|event| event.kind == "repetition_completed").count(), 1);
}

#[test]
fn noninterrupting_outer_timer_preserves_failed_script_wait() {
    let fixture = Fixture::new();
    let mut model = script_model("1 / 0", BTreeMap::new(), false);
    model.variables.insert("results".into(), json!([]));
    model.timer_timezone = Some("UTC".into());
    model.nodes.iter_mut().find(|node| node.id == "Compute").unwrap().repeat =
        Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
    model.nodes.extend([
        ProcessNode { id: "SideTimer".into(), name: "Side timer".into(),
            kind: ProcessNodeKind::BoundaryTimer {
                attached_to_id: "Compute".into(), cancel_activity: false,
                timer: ProcessTimerSpec::Duration { seconds: 1 },
            }, repeat: None, activity_io: None,},
        ProcessNode { id: "SideEnd".into(), name: "Side end".into(),
            kind: ProcessNodeKind::End, repeat: None, activity_io: None,},
    ]);
    model.sequence_flows.push(edge("SideFlow", "SideTimer", "SideEnd"));
    let version = publish_model(&fixture, &model);
    let started = super::messages::test_support::start_version(&fixture, &version);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let timer = snapshot.timers.iter().find(|timer|
        timer.node_id == "SideTimer" && timer.status == ProcessTimerStatus::Pending).unwrap();
    assert_eq!(timer.token_id.as_deref(),
        Some(snapshot.repetition_groups[0].parent_token_id.as_str()));
    let at = timer.due_at_ms.unwrap() + 1;
    let candidate = repository::due_timers(&fixture.db, at, 32).unwrap().into_iter()
        .find(|candidate| candidate.timer_id == timer.timer_id).unwrap();
    let timer_snapshot = repository::timer_snapshot(&fixture.db, &candidate).unwrap();
    let plan = super::timers::plan_timer_fire(&timer_snapshot, at, None, None).unwrap();
    assert!(plan.repetition_groups.iter().all(|group|
        group.status != ProcessRepetitionGroupStatus::Cancelled));
    repository::fire_timer(&fixture.db, &candidate, &fixture.owner,
        Some(snapshot.instance.revision), repository::ProcessPlanInput::Supplied(&plan),
        at).unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let after = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(after.repetition_groups[0].status, ProcessRepetitionGroupStatus::Incident);
    assert_eq!(after.repetition_occurrences[0].status, ProcessRepetitionOccurrenceStatus::Active);
    assert!(after.tokens.iter().any(|token|
        token.token_id == after.repetition_occurrences[0].token_id
            && token.status == "waiting"));
    assert_eq!(after.timers.iter().filter(|timer|
        timer.node_id == "SideTimer" && timer.status == ProcessTimerStatus::Fired).count(), 1);
}

#[test]
fn terminate_preempts_repeated_script_before_any_body_or_group() {
    let fixture = Fixture::new();
    let mut model = script_model("repeat.index + 1", BTreeMap::new(), false);
    model.variables.insert("results".into(), json!([]));
    model.nodes.iter_mut().find(|node| node.id == "Compute").unwrap().repeat =
        Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
    model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::TerminateEnd;
    model.nodes.insert(1, ProcessNode { id: "Split".into(), name: "Fork".into(),
        kind: ProcessNodeKind::ParallelGateway, repeat: None, activity_io: None,});
    model.nodes.push(ProcessNode { id: "Terminate".into(), name: "Stop".into(),
        kind: ProcessNodeKind::TerminateEnd, repeat: None, activity_io: None,});
    model.sequence_flows = vec![edge("ToSplit", "Start_1", "Split"),
        edge("A_Stop", "Split", "Terminate"), edge("Z_Compute", "Split", "Compute"),
        edge("FromCompute", "Compute", "End_1")];
    let started = start_model(&fixture, &model);
    assert_eq!(started.status, ProcessInstanceStatus::Completed);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    assert!(snapshot.repetition_groups.is_empty());
    assert!(snapshot.repetition_occurrences.is_empty());
    let history = repository::list_events(&reopened, &fixture.owner,
        &started.instance_id, 0, 200).unwrap().0;
    assert_eq!(history.iter().filter(|event|
        event.kind == "terminate_end_reached").count(), 1);
    assert!(!history.iter().any(|event| event.kind == "script_completed"));
}

#[test]
fn repeated_script_cardinality_edges_keep_one_ordered_parent_aggregate() {
    for count in [0, 1, 3, 16] {
        let fixture = Fixture::new();
        let mut model = script_model("repeat.index", BTreeMap::new(), false);
        model.variables.insert("results".into(), json!([]));
        model.nodes.iter_mut().find(|node| node.id == "Compute").unwrap().repeat =
            Some(ProcessRepeatSpec::MultiInstance {
                mode: ProcessMultiInstanceMode::Sequential,
                input: ProcessMultiInstanceInput::Cardinality { count },
                output_collection_variable: "results".into(),
            });
        let started = start_model(&fixture, &model);
        assert_eq!(started.status, ProcessInstanceStatus::Completed);
        let expected = Value::Array((0..count).map(|ordinal| json!(ordinal)).collect());
        assert_eq!(started.variables["results"], expected);
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner,
            &started.instance_id).unwrap();
        assert_eq!(snapshot.repetition_groups[0].completed_count, u32::from(count));
        assert_eq!(snapshot.repetition_occurrences.len(), usize::from(count));
        let mut history = Vec::new();
        let mut after_seq = 0;
        loop {
            let (page, next_seq, has_more) = repository::list_events(
                &reopened, &fixture.owner, &started.instance_id, after_seq, 200).unwrap();
            assert!(!has_more || next_seq > after_seq);
            history.extend(page);
            if !has_more { break; }
            after_seq = next_seq;
        }
        assert_eq!(history.iter().filter(|event| event.kind == "script_completed").count(),
            usize::from(count));
        assert_eq!(history.iter().filter(|event| event.kind == "repetition_completed").count(), 1);
    }
}

#[test]
fn repeated_script_null_body_is_a_present_accepted_item() {
    let fixture = Fixture::new();
    let mut model = script_model("null", BTreeMap::new(), false);
    model.variables.insert("results".into(), json!([]));
    model.nodes.iter_mut().find(|node| node.id == "Compute").unwrap().repeat =
        Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
    let started = start_model(&fixture, &model);
    assert_eq!(started.status, ProcessInstanceStatus::Completed);
    assert_eq!(started.variables["results"], json!([null, null]));
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(snapshot.repetition_groups[0].completed_count, 2);
    assert!(snapshot.repetition_occurrences.iter().all(|row|
        row.status == ProcessRepetitionOccurrenceStatus::Completed
            && row.aggregate_item.as_ref() == Some(&Value::Null)
            && row.accepted_source_event_id.is_some()));
}

#[test]
fn structured_script_maps_frozen_local_state_for_three_iterations() {
    let fixture = Fixture::new();
    let mut model = script_model("{\"value\": vars.step, \"ordinal\": repeat.index}",
        BTreeMap::from([("step".into(), "vars.step + 1".into())]), false);
    model.variables.insert("results".into(), json!([]));
    model.variables.insert("step".into(), json!(0));
    model.nodes.iter_mut().find(|node| node.id == "Compute").unwrap().repeat =
        Some(ProcessRepeatSpec::StructuredLoop {
            condition: "vars.step < 3".into(), test_before: false,
            max_iterations: 3, output_collection_variable: "results".into(),
        });
    let started = start_model(&fixture, &model);
    assert_eq!(started.status, ProcessInstanceStatus::Completed);
    assert_eq!(started.variables["step"], 0);
    assert_eq!(started.variables["results"], json!([
        {"value":0,"ordinal":0},{"value":1,"ordinal":1},{"value":2,"ordinal":2}
    ]));
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(snapshot.repetition_groups[0].completed_count, 3);
    assert_eq!(snapshot.repetition_groups[0].loop_state.as_ref().unwrap()["step"], 3);
    assert_eq!(snapshot.repetition_occurrences.iter().find(|row| row.ordinal == 2)
        .unwrap().state_after.as_ref().unwrap()["step"], 3);
    assert!(snapshot.repetition_occurrences.iter().all(|row|
        row.status == ProcessRepetitionOccurrenceStatus::Completed
            && row.state_patch.is_some() && row.state_after.is_some()));
}

#[test]
fn later_repeated_script_body_failure_preserves_only_prior_completed_ordinal() {
    let fixture = Fixture::new();
    let mut model = script_model("repeat.index == 0 ? 5 : 1 / 0", BTreeMap::new(), false);
    model.variables.insert("results".into(), json!([]));
    model.nodes.iter_mut().find(|node| node.id == "Compute").unwrap().repeat =
        Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
    let (command, instance_id, plan, variables, version, at_ms) = plan_and_start(&fixture, &model);
    assert_eq!(plan.events.iter().filter(|event| event.kind == "script_completed").count(), 1);
    assert_eq!(plan.events.iter().filter(|event|
        event.kind == "repetition_occurrence_completed").count(), 1);
    assert_eq!(plan.add_incidents.len(), 1);
    assert_eq!(plan.add_incidents[0].code, "SCRIPT_EVALUATION_FAILED");
    assert_eq!(plan.repetition_groups[0].completed_count, 1);
    assert_eq!(plan.repetition_groups[0].status, ProcessRepetitionGroupStatus::Incident);
    assert!(plan.repetition_occurrences.iter().any(|row|
        row.ordinal == 0 && row.status == ProcessRepetitionOccurrenceStatus::Completed));
    assert!(plan.repetition_occurrences.iter().any(|row|
        row.ordinal == 1 && row.status == ProcessRepetitionOccurrenceStatus::Active
            && row.accepted_source_event_id.is_none()));
    repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner, &instance_id).unwrap();
    assert_eq!(snapshot.repetition_groups[0].completed_count, 1);
    assert_eq!(snapshot.instance.variables["results"], json!([]));
}

#[test]
fn completed_repeated_script_disarms_outer_timer_without_a_race_window() {
    let fixture = Fixture::new();
    let mut model = script_model("repeat.index", BTreeMap::new(), false);
    model.variables.insert("results".into(), json!([]));
    model.timer_timezone = Some("UTC".into());
    model.nodes.iter_mut().find(|node| node.id == "Compute").unwrap().repeat =
        Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
    model.nodes.extend([
        ProcessNode { id: "OuterTimer".into(), name: "Outer timer".into(),
            kind: ProcessNodeKind::BoundaryTimer {
                attached_to_id: "Compute".into(), cancel_activity: true,
                timer: ProcessTimerSpec::Duration { seconds: 1 },
            }, repeat: None, activity_io: None,},
        ProcessNode { id: "TimeoutEnd".into(), name: "Timeout end".into(),
            kind: ProcessNodeKind::End, repeat: None, activity_io: None,},
    ]);
    model.sequence_flows.push(edge("ToTimeout", "OuterTimer", "TimeoutEnd"));
    let started = start_model(&fixture, &model);
    assert_eq!(started.status, ProcessInstanceStatus::Completed);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(snapshot.repetition_groups[0].status, ProcessRepetitionGroupStatus::Completed);
    assert_eq!(snapshot.repetition_groups[0].completed_count, 2);
    assert_eq!(snapshot.timers.iter().filter(|timer|
        timer.node_id == "OuterTimer").count(), 1);
    assert!(snapshot.timers.iter().filter(|timer|
        timer.node_id == "OuterTimer").all(|timer| timer.status == ProcessTimerStatus::Cancelled));
    assert!(repository::due_timers(&reopened,
        chrono::Utc::now().timestamp_millis() + 10_000, 32).unwrap().iter().all(|timer|
            snapshot.timers.iter().all(|actual| actual.timer_id != timer.timer_id)));
}

#[test]
fn parent_cancel_closes_pinned_called_script_group_after_body_failure() {
    let fixture = Fixture::new();
    let mut child = script_model("1 / 0", BTreeMap::new(), false);
    child.variables.insert("results".into(), json!([]));
    child.nodes.iter_mut().find(|node| node.id == "Compute").unwrap().repeat =
        Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
    let child_version = publish_model(&fixture, &child);
    let caller = super::call_tests::caller(&child_version, BTreeMap::new());
    let parent = start_model(&fixture, &caller);
    let child_id = super::call_tests::child_id(&fixture, &parent.instance_id);
    let parent_before = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &parent.instance_id).unwrap();
    let child_before = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    assert_eq!(child_before.repetition_groups[0].status, ProcessRepetitionGroupStatus::Incident);
    assert!(child_before.repetition_occurrences[0].accepted_source_event_id.is_none());
    repository::cancel_instance(&fixture.db, &fixture.owner,
        &stamp("cancel pinned Script child"), &parent.instance_id,
        parent_before.instance.revision).unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let parent_after = repository::runtime_snapshot(&reopened, &fixture.owner,
        &parent.instance_id).unwrap();
    let child_after = repository::runtime_snapshot(&reopened, &fixture.owner, &child_id).unwrap();
    assert_eq!(parent_after.instance.status, ProcessInstanceStatus::Cancelled);
    assert_eq!(child_after.instance.status, ProcessInstanceStatus::Cancelled);
    assert_eq!(child_after.repetition_groups[0].status, ProcessRepetitionGroupStatus::Cancelled);
    assert!(child_after.repetition_occurrences.iter().all(|row|
        row.status == ProcessRepetitionOccurrenceStatus::Cancelled));
    let history = repository::list_events(&reopened, &fixture.owner, &child_id, 0, 200).unwrap().0;
    assert!(!history.iter().any(|event| event.kind == "script_completed"));
}

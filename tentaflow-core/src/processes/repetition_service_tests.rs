// ============ File: repetition_service_tests.rs — file-backed repeated Service claim and verification regressions ============

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tentaflow_protocol::processes::{
    ActivityOutcome, ActivityResult, ActivityVerification, ProcessInstanceStatus,
    ProcessMultiInstanceInput, ProcessMultiInstanceMode, ProcessNodeKind,
    ProcessRepeatSpec, ProcessRepetitionGroupMode, ProcessRepetitionOccurrenceStatus, ProcessUserTaskKind,
    ProcessActivityIo,
    ProcessInputAssociation, ProcessIoDataInput,
    ProcessUserTaskStatus,
};
use tokio_util::sync::CancellationToken;

use super::jobs;
use super::repository::{self, ActivityResultOrigin, ClaimedProcessJob, ExpressionObservation, ObservedActivityResult};
use super::runtime::{self, test_support::{flow, graph, plan_recorded_result, service_model, stamp, start_model, Fixture}};
use crate::flow_engine::dispatcher::PinnedFlowSnapshot;
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue};
use crate::flow_engine::expr::flow_value_to_json;

pub(super) fn repeated_service_model(
    flow_id: &str,
    verification: ActivityVerification,
) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = service_model(flow_id, verification);
    model.variables.insert("items".into(), json!([{"case":"A"},{"case":"B"}]));
    model.variables.insert("results".into(), json!([]));
    let service = model.nodes.iter_mut().find(|node| node.id == "Service").unwrap();
    service.repeat = Some(ProcessRepeatSpec::MultiInstance {
        mode: ProcessMultiInstanceMode::Sequential,
        input: ProcessMultiInstanceInput::CollectionExpression {
            expression: "vars.items".into(),
        },
        output_collection_variable: "results".into(),
    });
    model
}

pub(super) async fn observed_flow_result(fixture: &Fixture, claim: &ClaimedProcessJob) -> ObservedActivityResult {
    let observed = original_flow_result(fixture, claim).await;
    let worker = claim.job.worker_id.as_deref().expect("claimed Service worker");
    assert_eq!(repository::record_job_observation(&fixture.db, claim, worker,
        &observed, chrono::Utc::now().timestamp_millis()).unwrap(),
        repository::JobObservationState::Observed);
    observed
}

pub(super) async fn original_flow_result(fixture: &Fixture, claim: &ClaimedProcessJob) -> ObservedActivityResult {
    let node = repository::scope_node(&claim.snapshot.model,
        &claim.snapshot.instance.process_id, &claim.snapshot.scopes,
        &claim.job.instance_id, &claim.job.scope_id, &claim.job.node_id).unwrap();
    let ProcessNodeKind::ServiceTask { flow_id, result_expression, .. } = &node.kind else {
        panic!("claimed job is not a ServiceTask");
    };
    let source = claim.snapshot.service_snapshots.iter().find(|source|
        source.info.node_id == node.id && source.info.flow_id == *flow_id)
        .expect("exact pinned Service graph");
    let pinned = PinnedFlowSnapshot {
        flow_id: source.info.flow_id.clone(),
        source_version: source.info.source_version,
        graph_json: source.graph_json.clone(),
        graph_sha256: source.info.graph_sha256.clone(),
    };
    let mut meta = fixture.dispatcher().authorize_process_flow(
        &pinned.flow_id, &claim.actor.user_id, &claim.actor.org_id,
    ).unwrap();
    let worker = claim.job.worker_id.as_deref().expect("claimed Service worker");
    assert!(repository::commit_job_dispatch_boundary(&fixture.db, claim, worker,
        chrono::Utc::now().timestamp_millis()).unwrap());
    meta.request_id = claim.request_id.clone();
    meta.correlation_id = Some(claim.job.instance_id.clone());
    let mut envelope = FlowEnvelope::empty();
    envelope.payload = FlowValue::Json(claim.job.input["payload"].clone());
    envelope.variables = claim.job.input["variables"].as_object().unwrap().iter()
        .map(|(key, value)| (key.clone(), FlowValue::Json(value.clone()))).collect();
    let executed = fixture.dispatcher().dispatch_pinned_flow(&pinned, envelope, meta).await.unwrap();
    assert!(executed.error.is_none(), "pinned Flow failed: {:?}", executed.error.as_deref());
    let outputs = json!({
        "payload": flow_value_to_json(&executed.final_envelope.payload),
        "variables": executed.final_envelope.variables.iter()
            .map(|(key, value)| (key.clone(), flow_value_to_json(value)))
            .collect::<std::collections::BTreeMap<_, _>>(),
        "artifacts": executed.final_envelope.artifacts.iter()
            .map(|(key, value)| (key.clone(), flow_value_to_json(value)))
            .collect::<std::collections::BTreeMap<_, _>>(),
    });
    if let Some(expression) = result_expression {
        let occurrence = claim.snapshot.repetition_occurrences.iter()
            .find(|row| row.job_id.as_deref() == Some(claim.job.job_id.as_str()));
        let (variables, extra) = if let Some(occurrence) = occurrence {
            let group = claim.snapshot.repetition_groups.iter()
                .find(|row| row.group_id == occurrence.group_id).unwrap();
            let variables = if group.mode == ProcessRepetitionGroupMode::StructuredLoop {
                occurrence.input_variables.clone()
            } else {
                group.entry_variables.clone()
            };
            (variables, vec![("repeat".to_owned(), json!({
                "group_id":group.group_id,"index":occurrence.ordinal,"item":occurrence.item
            }))])
        } else {
            (repository::effective_scope_variables(&claim.snapshot.scopes,
                &claim.snapshot.scope_variables, &claim.job.instance_id,
                &claim.snapshot.instance.variables, &claim.job.scope_id).unwrap(), Vec::new())
        };
        let result = jobs::parse_contract_result(runtime::evaluate(
            expression, &variables, &outputs, &extra).unwrap()).unwrap();
        let observed = ObservedActivityResult {
            result,
            origin: ActivityResultOrigin::Contract,
            expression_observation: Some(ExpressionObservation {
                normalized_outputs: outputs,
                evaluation_variables: variables,
            }),
        };
        return observed;
    }
    let observed = ObservedActivityResult {
        result: ActivityResult {
            outcome: ActivityOutcome::Completed,
            code: None,
            summary: "The pinned flow returned an activity result".into(),
            outputs,
            evidence: vec![format!("flow_execution_latency_ms:{}", executed.total_latency_ms)],
        },
        origin: ActivityResultOrigin::Envelope,
        expression_observation: None,
    };
    observed
}

fn claim_hash(claim: &ClaimedProcessJob) -> String {
    let occurrence = claim.snapshot.repetition_occurrences.iter()
        .find(|row| row.job_id.as_deref() == Some(claim.job.job_id.as_str())).unwrap();
    let group = claim.snapshot.repetition_groups.iter()
        .find(|row| row.group_id == occurrence.group_id).unwrap();
    let context = json!({
        "activity_node_id": claim.job.node_id,
        "attempt": claim.job.attempt,
        "definition_id": claim.snapshot.instance.definition_id,
        "definition_version": claim.snapshot.instance.version,
        "fence": claim.job.fence,
        "group_id": group.group_id,
        "instance_id": claim.job.instance_id,
        "item": occurrence.item,
        "job_id": claim.job.job_id,
        "ordinal": occurrence.ordinal,
        "scope_id": claim.job.scope_id,
        "vars": group.entry_variables,
    });
    let mut digest = Sha256::new();
    digest.update(b"tentaflow:repeat-claim:");
    digest.update(serde_json::to_vec(&context).unwrap());
    hex::encode(digest.finalize())
}

fn history(fixture: &Fixture, instance_id: &str) -> Vec<tentaflow_protocol::processes::ProcessEvent> {
    repository::list_events(&fixture.db, &fixture.owner, instance_id, 0, 200).unwrap().0
}

#[test]
fn repeated_service_reservation_follows_invocation_phase_after_cancellation() {
    let cases = [
        ("queued", "prepared", "no_boundary", false, (true, true, true)),
        ("running", "prepared", "no_boundary", false, (false, true, true)),
        ("running", "may_have_executed", "committed_boundary", false, (false, true, true)),
        ("cancelled", "uncertain", "committed_boundary", false, (false, true, true)),
        ("error", "uncertain", "committed_boundary", false, (false, true, true)),
        ("error", "uncertain", "boundary_unknown", false, (false, false, false)),
        ("running", "observed", "committed_boundary", true, (false, false, true)),
        ("error", "proved_no_effect", "no_boundary", false, (false, false, false)),
        ("error", "observed_blocked", "committed_boundary", true, (false, false, false)),
    ];
    for (status, phase, evidence, observed, expected) in cases {
        assert_eq!(runtime::repetition_service_reservation_flags(
            status, Some(phase), evidence, observed), expected,
            "{status}/{phase}/{evidence}");
    }
    let model = repeated_service_model("flow-approved", ActivityVerification::Human);
    let node = model.nodes.iter().find(|node| node.id == "Service").unwrap();
    let reserve = runtime::repetition_immediate_reservation(node, "group", 0,
        &json!({"case":"A"}), &json!({"items":[{"case":"A"}],"results":[]}), None).unwrap();
    assert!(reserve >= runtime::MAX_OBSERVED_SERVICE_RESULT_BYTES
        + runtime::MAX_REPEATED_SERVICE_INCIDENT_MESSAGE_BYTES
        + runtime::repetition_claim_reservation().unwrap());
}

#[tokio::test]
async fn repeated_observation_retains_exact_bytes_after_closed_dispatch_and_reopen() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("retained observation", None));
    let model = repeated_service_model(&flow_id, ActivityVerification::Human);
    let started = start_model(&fixture, &model);
    let worker = "retained-observation-worker";
    let claim = repository::claim_job(&fixture.db, worker,
        chrono::Utc::now().timestamp_millis()).unwrap().unwrap();
    let occurrence = claim.snapshot.repetition_occurrences.iter()
        .find(|row| row.job_id.as_deref() == Some(claim.job.job_id.as_str())).unwrap();
    let group_id = occurrence.group_id.clone();
    let observed = observed_flow_result(&fixture, &claim).await;
    let before_cancel: String = fixture.db.read().unwrap().query_row(
        "SELECT observed_result_json FROM bpmn_service_invocations WHERE job_id=?1",
        [&claim.job.job_id], |row| row.get(0)).unwrap();
    let before_bytes = super::repetition_capacity_tests::physical_retained_bytes(
        &fixture, &group_id);
    assert_eq!(repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap().repetition_groups[0].retained_bytes, before_bytes);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    repository::cancel_instance(&fixture.db, &fixture.owner,
        &stamp("close repeated observed Service"), &started.instance_id,
        snapshot.instance.revision).unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let (phase, retained): (String, String) = reopened.read().unwrap().query_row(
        "SELECT phase,observed_result_json FROM bpmn_service_invocations WHERE job_id=?1",
        [&claim.job.job_id], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
    assert_eq!(phase, "observed_blocked");
    assert_eq!(retained, before_cancel);
    let group = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap().repetition_groups.into_iter()
        .find(|group| group.group_id == group_id).unwrap();
    assert!(group.retained_bytes >= before_bytes);
    assert_eq!(group.retained_bytes,
        super::repetition_capacity_tests::physical_retained_bytes(&fixture, &group_id));
    assert_eq!(repository::record_job_observation(&reopened, &claim, worker, &observed,
        chrono::Utc::now().timestamp_millis()).unwrap(),
        repository::JobObservationState::Blocked);
    assert_eq!(repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap().repetition_groups[0].retained_bytes,
        group.retained_bytes);
}

#[tokio::test]
async fn parallel_needs_human_results_replan_a_stale_sibling_and_approve_both() {
    let fixture = Fixture::new();
    let business = json!({"outcome":"NeedsHuman","code":"REVIEW",
        "summary":"Review the actual result","outputs":{"answer":17},
        "evidence":["registered_flow_result"]});
    let graph = json!({"nodes":[{"id":"trigger","type":"trigger","config":{
        "output_mapping":{"actual_result":business.to_string()}}},
        {"id":"output","type":"output","config":{}}],
        "edges":[{"from":"trigger","to":"output","from_port":"text","to_port":"text"}],
        "variables":[{"name":"actual_result","type":"json"}]}).to_string();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph);
    let mut model = repeated_service_model(&flow_id, ActivityVerification::Human);
    let service = model.nodes.iter_mut().find(|node| node.id == "Service").unwrap();
    service.repeat = Some(ProcessRepeatSpec::MultiInstance {
        mode: ProcessMultiInstanceMode::Parallel,
        input: ProcessMultiInstanceInput::CollectionExpression {
            expression: "vars.items".into(),
        },
        output_collection_variable: "results".into(),
    });
    let ProcessNodeKind::ServiceTask { result_expression, output_mapping, .. } = &mut service.kind
        else { panic!("fixture Service node changed kind") };
    *result_expression = Some("outputs.variables.actual_result".into());
    output_mapping.insert("answer".into(), "outputs.answer".into());
    let started = start_model(&fixture, &model);
    let mut claimed = Vec::new();
    for index in 0..2 {
        let worker = format!("parallel-repeat-worker-{index}");
        let claim = repository::claim_job(&fixture.db, &worker,
            chrono::Utc::now().timestamp_millis()).unwrap().unwrap();
        let ordinal = claim.snapshot.repetition_occurrences.iter()
            .find(|row| row.job_id.as_deref() == Some(claim.job.job_id.as_str()))
            .unwrap().ordinal;
        claimed.push((ordinal, worker, claim));
    }
    claimed.sort_by_key(|(ordinal, _, _)| *ordinal);
    assert_eq!(claimed.iter().map(|(ordinal, _, _)| *ordinal).collect::<Vec<_>>(), [0, 1]);
    let (_, worker_zero, claim_zero) = &claimed[0];
    let (_, worker_one, claim_one) = &claimed[1];
    assert_eq!(claim_zero.snapshot.instance.revision, claim_one.snapshot.instance.revision);
    let shared = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let job_zero = shared.jobs.iter().find(|job| job.job_id == claim_zero.job.job_id).unwrap();
    let job_one = shared.jobs.iter().find(|job| job.job_id == claim_one.job.job_id).unwrap();
    assert_eq!(job_zero.status, "running");
    assert_eq!(job_one.status, "running");
    let observed_zero = observed_flow_result(&fixture, claim_zero).await;
    let observed_one = observed_flow_result(&fixture, claim_one).await;
    assert_eq!(observed_zero.origin, ActivityResultOrigin::Contract);
    assert_eq!(observed_one.origin, ActivityResultOrigin::Contract);
    assert_eq!(observed_zero.result, observed_one.result);
    assert_eq!(observed_zero.result.outcome, ActivityOutcome::NeedsHuman);
    let observed_snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let observed_zero_job = observed_snapshot.jobs.iter()
        .find(|job| job.job_id == claim_zero.job.job_id).unwrap();
    let observed_one_job = observed_snapshot.jobs.iter()
        .find(|job| job.job_id == claim_one.job.job_id).unwrap();
    let at = chrono::Utc::now().timestamp_millis();
    let stale_plan = runtime::plan_job_result(&observed_snapshot, observed_zero_job,
        &observed_zero, at, None).unwrap();
    let first_plan = runtime::plan_job_result(&observed_snapshot, observed_one_job,
        &observed_one, at, None).unwrap();
    assert_eq!(stale_plan.status, ProcessInstanceStatus::Running);
    assert_eq!(first_plan.status, ProcessInstanceStatus::Running);
    repository::accept_job_result(&fixture.db, &fixture.owner,
        &claim_one.job.job_id, claim_one.job.attempt, claim_one.job.fence, worker_one,
        &observed_one, shared.instance.revision, repository::ProcessPlanInput::Supplied(&first_plan), at).unwrap();
    let after_first = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(after_first.repetition_occurrences.iter()
        .filter(|row| row.status == ProcessRepetitionOccurrenceStatus::AwaitingVerification)
        .count(), 1);
    assert_eq!(after_first.repetition_occurrences.iter()
        .find(|row| row.ordinal == 0).unwrap().status,
        ProcessRepetitionOccurrenceStatus::Active);
    let before_stale = super::call_tests::transition_rows(&fixture);
    assert_eq!(before_stale.len(), 17);
    let error = repository::accept_job_result(&fixture.db, &fixture.owner,
        &claim_zero.job.job_id, claim_zero.job.attempt, claim_zero.job.fence, worker_zero,
        &observed_zero, shared.instance.revision, repository::ProcessPlanInput::Supplied(&stale_plan), at).unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("process instance revision conflict or closed instance"), "{message}");
    assert_eq!(super::call_tests::transition_rows(&fixture), before_stale);

    let fresh = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let fresh_job = fresh.jobs.iter().find(|job| job.job_id == claim_zero.job.job_id).unwrap();
    assert_eq!(fresh_job.status, "running");
    let fresh_plan = runtime::plan_job_result(&fresh, fresh_job, &observed_zero, at, None).unwrap();
    assert_eq!(fresh_plan.status, ProcessInstanceStatus::Waiting);
    repository::accept_job_result(&fixture.db, &fixture.owner,
        &claim_zero.job.job_id, claim_zero.job.attempt, claim_zero.job.fence, worker_zero,
        &observed_zero, fresh.instance.revision, repository::ProcessPlanInput::Supplied(&fresh_plan), at).unwrap();
    let both_waiting = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(both_waiting.repetition_occurrences.iter()
        .filter(|row| row.status == ProcessRepetitionOccurrenceStatus::AwaitingVerification)
        .count(), 2);
    assert_eq!(both_waiting.user_tasks.iter().filter(|task|
        task.kind == ProcessUserTaskKind::Verification
            && task.status == ProcessUserTaskStatus::Open).count(), 2);
    assert!(both_waiting.incidents.is_empty());
    assert_eq!(history(&fixture, &started.instance_id).iter()
        .filter(|event| event.kind == "service_result"
            && event.data["result_origin"] == "contract"
            && event.data["evidence"] == json!(["registered_flow_result"])).count(), 2);

    for ordinal in 0..2 {
        let waiting = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let occurrence = waiting.repetition_occurrences.iter()
            .find(|row| row.ordinal == ordinal).unwrap();
        assert_eq!(occurrence.status, ProcessRepetitionOccurrenceStatus::AwaitingVerification);
        let task_id = occurrence.verification_user_task_id.as_ref().unwrap();
        let task = waiting.user_tasks.iter().find(|task| task.user_task_id == *task_id).unwrap();
        assert_eq!(task.kind, ProcessUserTaskKind::Verification);
        assert_eq!(task.status, ProcessUserTaskStatus::Open);
        let command = stamp("approve actual parallel repeated result");
        let approval = runtime::plan_user_completion(&waiting, task_id, &Value::Null,
            Some(true), at, runtime::test_support::human_input(&waiting, task_id, &command), None).unwrap();
        repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &started.instance_id, task_id, waiting.instance.revision,
            &Value::Null, Some(true), repository::ProcessPlanInput::Supplied(&approval), at).unwrap();
        let after_approval = super::call_tests::transition_rows(&fixture);
        repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &started.instance_id, task_id, waiting.instance.revision,
            &Value::Null, Some(true), repository::ProcessPlanInput::Supplied(&approval), at).unwrap();
        assert_eq!(super::call_tests::transition_rows(&fixture), after_approval);
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let completed = repository::get_instance(&reopened, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert_eq!(completed.variables["results"], json!([{"answer":17},{"answer":17}]));
    let final_events = repository::list_events(&reopened, &fixture.owner,
        &started.instance_id, 0, 200).unwrap().0;
    assert_eq!(final_events.iter().filter(|event| event.kind == "service_result").count(), 2);
    assert_eq!(final_events.iter().filter(|event| event.kind == "verification_approved").count(), 2);
    assert_eq!(final_events.iter().filter(|event| event.kind == "repetition_completed").count(), 1);
    assert!(final_events.iter().all(|event| event.data["code"] != "RESULT_REJECTED"));
}

#[tokio::test]
async fn repeated_service_claims_bind_fence_ordinal_item_and_frozen_variables_before_two_approvals() {
    let fixture = Fixture::new();
    assert!(fixture.directory.path().join("processes.db").is_file());
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("repeated evidence", None));
    let model = repeated_service_model(&flow_id, ActivityVerification::Human);
    let started = start_model(&fixture, &model);
    let mut accepted_hashes = Vec::new();
    let mut earlier_service_source: Option<String> = None;
    for ordinal in 0_u32..2 {
        let worker = format!("repeated-service-worker-{ordinal}");
        let claim = repository::claim_job(&fixture.db, &worker, chrono::Utc::now().timestamp_millis())
            .unwrap().expect("actual repeated Service job");
        let occurrence = claim.snapshot.repetition_occurrences.iter()
            .find(|row| row.job_id.as_deref() == Some(claim.job.job_id.as_str())).unwrap();
        assert_eq!(occurrence.ordinal, ordinal);
        assert_eq!(occurrence.item, json!({"case": if ordinal == 0 { "A" } else { "B" }}));
        let claimed = history(&fixture, &started.instance_id).into_iter().rev()
            .find(|event| event.kind == "service_claimed"
                && event.data["job_id"] == claim.job.job_id).unwrap();
        assert_eq!(claimed.scope_id, occurrence.scope_id);
        let hash = claim_hash(&claim);
        assert_eq!(claimed.data["repeat_context_sha256"], hash);
        assert_eq!(claimed.data["attempt"], claim.job.attempt);
        assert_eq!(claimed.data["fence"], claim.job.fence);
        accepted_hashes.push(hash);

        let retained_before = claim.snapshot.repetition_groups.iter()
            .find(|group| group.group_id == occurrence.group_id).unwrap().retained_bytes;
        let observed = observed_flow_result(&fixture, &claim).await;
        let retained_after = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap().repetition_groups.into_iter()
            .find(|group| group.group_id == occurrence.group_id).unwrap().retained_bytes;
        let observed_bytes: i64 = fixture.db.read().unwrap().query_row(
            "SELECT length(CAST(observed_result_json AS BLOB)) FROM bpmn_service_invocations WHERE job_id=?1",
            [&claim.job.job_id], |row| row.get(0)).unwrap();
        assert_eq!(retained_after, retained_before + u64::try_from(observed_bytes).unwrap());
        assert_eq!(retained_after,
            super::repetition_capacity_tests::physical_retained_bytes(&fixture, &occurrence.group_id));
        assert_eq!(repository::record_job_observation(&fixture.db, &claim, &worker,
            &observed, chrono::Utc::now().timestamp_millis()).unwrap(),
            repository::JobObservationState::Observed);
        assert_eq!(repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap().repetition_groups.into_iter()
            .find(|group| group.group_id == occurrence.group_id).unwrap().retained_bytes,
            retained_after);
        let at = chrono::Utc::now().timestamp_millis();
        let plan = plan_recorded_result(&fixture, &claim, &observed, at).unwrap();
        let before = super::call_tests::transition_rows(&fixture);
        assert_eq!(before.len(), 17);
        let mut wrong_item = plan.clone();
        wrong_item.repetition_occurrences.iter_mut()
            .find(|row| row.occurrence_id == occurrence.occurrence_id).unwrap()
            .item = json!({"case":"foreign"});
        let error = repository::accept_job_result(&fixture.db, &fixture.owner,
            &claim.job.job_id, claim.job.attempt, claim.job.fence, &worker,
            &observed, claim.snapshot.instance.revision, repository::ProcessPlanInput::Supplied(&wrong_item), at).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("repetition occurrence changed immutable ordinal input"), "{message}");
        assert_eq!(super::call_tests::transition_rows(&fixture), before);
        let mut wrong_ordinal = plan.clone();
        wrong_ordinal.repetition_occurrences.iter_mut()
            .find(|row| row.occurrence_id == occurrence.occurrence_id).unwrap()
            .ordinal += 1;
        let error = repository::accept_job_result(&fixture.db, &fixture.owner,
            &claim.job.job_id, claim.job.attempt, claim.job.fence, &worker,
            &observed, claim.snapshot.instance.revision, repository::ProcessPlanInput::Supplied(&wrong_ordinal), at).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("occurrence changed its group scope or ordinal"), "{message}");
        assert_eq!(super::call_tests::transition_rows(&fixture), before);
        let mut wrong_variables = plan.clone();
        wrong_variables.repetition_occurrences.iter_mut()
            .find(|row| row.occurrence_id == occurrence.occurrence_id).unwrap()
            .input_variables = json!({"items":[],"results":[]});
        let error = repository::accept_job_result(&fixture.db, &fixture.owner,
            &claim.job.job_id, claim.job.attempt, claim.job.fence, &worker,
            &observed, claim.snapshot.instance.revision, repository::ProcessPlanInput::Supplied(&wrong_variables), at).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("repetition occurrence changed immutable ordinal input"), "{message}");
        assert_eq!(super::call_tests::transition_rows(&fixture), before);
        let mut wrong_source = plan.clone();
        wrong_source.repetition_occurrences.iter_mut()
            .find(|row| row.occurrence_id == occurrence.occurrence_id).unwrap()
            .accepted_source_event_id = Some(uuid::Uuid::new_v4().to_string());
        let error = repository::accept_job_result(&fixture.db, &fixture.owner,
            &claim.job.job_id, claim.job.attempt, claim.job.fence, &worker,
            &observed, claim.snapshot.instance.revision, repository::ProcessPlanInput::Supplied(&wrong_source), at).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("repeated Service source event differs from its fenced accepted entry"), "{message}");
        assert_eq!(super::call_tests::transition_rows(&fixture), before);
        let mut wrong_kind_source = plan.clone();
        wrong_kind_source.repetition_occurrences.iter_mut()
            .find(|row| row.occurrence_id == occurrence.occurrence_id).unwrap()
            .accepted_source_event_id = Some(claimed.event_id.clone());
        let error = repository::accept_job_result(&fixture.db, &fixture.owner,
            &claim.job.job_id, claim.job.attempt, claim.job.fence, &worker,
            &observed, claim.snapshot.instance.revision, repository::ProcessPlanInput::Supplied(&wrong_kind_source), at).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("repeated Service source event differs from its fenced accepted entry"), "{message}");
        assert_eq!(super::call_tests::transition_rows(&fixture), before);
        if let Some(source_id) = &earlier_service_source {
            let previous = history(&fixture, &started.instance_id).into_iter()
                .find(|event| event.event_id == *source_id).unwrap();
            assert_eq!(previous.kind, "service_result");
            assert_eq!(previous.scope_id, occurrence.scope_id);
            let mut wrong_prior_source = plan.clone();
            wrong_prior_source.repetition_occurrences.iter_mut()
                .find(|row| row.occurrence_id == occurrence.occurrence_id).unwrap()
                .accepted_source_event_id = Some(source_id.clone());
            let error = repository::accept_job_result(&fixture.db, &fixture.owner,
                &claim.job.job_id, claim.job.attempt, claim.job.fence, &worker,
                &observed, claim.snapshot.instance.revision, repository::ProcessPlanInput::Supplied(&wrong_prior_source), at).unwrap_err();
            let message = format!("{error:#}");
            assert!(message.contains("repeated Service source event differs from its fenced accepted entry"), "{message}");
            assert_eq!(super::call_tests::transition_rows(&fixture), before);
        }
        let error = repository::accept_job_result(&fixture.db, &fixture.owner,
            &claim.job.job_id, claim.job.attempt, claim.job.fence + 1, &worker,
            &observed, claim.snapshot.instance.revision, repository::ProcessPlanInput::Supplied(&plan), at).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("Service acceptance lacks its immutable original observation"), "{message}");
        assert_eq!(super::call_tests::transition_rows(&fixture), before);

        repository::accept_job_result(&fixture.db, &fixture.owner,
            &claim.job.job_id, claim.job.attempt, claim.job.fence, &worker,
            &observed, claim.snapshot.instance.revision, repository::ProcessPlanInput::Supplied(&plan), at).unwrap();
        let waiting = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let persisted = waiting.repetition_occurrences.iter()
            .find(|row| row.occurrence_id == occurrence.occurrence_id).unwrap();
        assert_eq!(persisted.status, ProcessRepetitionOccurrenceStatus::AwaitingVerification);
        assert_eq!(persisted.accepted_source_event_id.as_deref(),
            plan.event_ids.get(&0).map(String::as_str));
        if ordinal == 0 {
            earlier_service_source = persisted.accepted_source_event_id.clone();
        }
        let task = waiting.user_tasks.iter().find(|task|
            task.kind == ProcessUserTaskKind::Verification
                && task.status == ProcessUserTaskStatus::Open
                && task.user_task_id == *persisted.verification_user_task_id.as_ref().unwrap()).unwrap();
        let accepted_task_output: String = fixture.db.read().unwrap().query_row(
            "SELECT outputs_json FROM bpmn_user_tasks WHERE user_task_id=?1",
            [&task.user_task_id], |row| row.get(0)).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&accepted_task_output).unwrap(),
            serde_json::to_value(&observed.result).unwrap());
        let approval_outputs = json!({"client":"cannot replace service result"});
        let approval_output_bytes = serde_json::to_vec(&approval_outputs).unwrap();
        let replaced_bytes = accepted_task_output.as_bytes().len()
            .checked_sub(approval_output_bytes.len()).unwrap();
        assert!(replaced_bytes > 0);
        let command = stamp("approve exact repeated Service result");
        let approval = runtime::plan_user_completion(&waiting, &task.user_task_id,
            &approval_outputs, Some(true), at,
            runtime::test_support::human_input(&waiting, &task.user_task_id, &command), None).unwrap();
        let before_approval = super::call_tests::transition_rows(&fixture);
        let mut forged_decision = approval.clone();
        forged_decision.events.iter_mut().find(|event|
            event.kind == "verification_approved"
                && event.data["user_task_id"] == task.user_task_id).unwrap()
            .data["outputs"] = serde_json::to_value(&observed.result).unwrap();
        assert!(repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &started.instance_id, &task.user_task_id, waiting.instance.revision,
            &approval_outputs, Some(true), repository::ProcessPlanInput::Supplied(&forged_decision), at).is_err());
        assert_eq!(super::call_tests::transition_rows(&fixture), before_approval);
        assert!(repository::complete_user_task(&fixture.db, &fixture.participant, &command,
            &started.instance_id, &task.user_task_id, waiting.instance.revision,
            &approval_outputs, Some(true), repository::ProcessPlanInput::Supplied(&approval), at).is_err());
        assert_eq!(super::call_tests::transition_rows(&fixture), before_approval);
        repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &started.instance_id, &task.user_task_id, waiting.instance.revision,
            &approval_outputs, Some(true), repository::ProcessPlanInput::Supplied(&approval), at).unwrap();
        let after_approval = super::call_tests::transition_rows(&fixture);
        let replay = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &started.instance_id, &task.user_task_id, waiting.instance.revision,
            &approval_outputs, Some(true), repository::ProcessPlanInput::Supplied(&approval), at).unwrap();
        assert_eq!(replay.instance.revision,
            repository::get_instance(&fixture.db, &fixture.owner,
                &started.instance_id, None).unwrap().revision);
        assert_eq!(super::call_tests::transition_rows(&fixture), after_approval);
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let current = repository::runtime_snapshot(&reopened, &fixture.owner,
            &started.instance_id).unwrap();
        let completed_task_output: String = reopened.read().unwrap().query_row(
            "SELECT outputs_json FROM bpmn_user_tasks WHERE user_task_id=?1",
            [&task.user_task_id], |row| row.get(0)).unwrap();
        assert_eq!(completed_task_output, accepted_task_output);
        assert_eq!(completed_task_output.as_bytes().len(), accepted_task_output.as_bytes().len());
        assert_eq!(history(&fixture, &started.instance_id).into_iter()
            .filter(|event| event.kind == "verification_approved"
                && event.data["user_task_id"] == task.user_task_id)
            .count(), 1);
        assert!(history(&fixture, &started.instance_id).into_iter().any(|event|
            event.kind == "verification_approved"
                && event.data["user_task_id"] == task.user_task_id
                && event.data["outputs"] == approval_outputs));
        assert_eq!(current.repetition_groups[0].completed_count, ordinal + 1);
        assert_eq!(current.repetition_groups[0].retained_bytes,
            super::repetition_capacity_tests::physical_retained_bytes(
                &fixture, &current.repetition_groups[0].group_id));
    }
    assert_ne!(accepted_hashes[0], accepted_hashes[1]);
    let final_state = repository::get_instance(&fixture.db, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(final_state.status, ProcessInstanceStatus::Completed);
    assert_eq!(final_state.variables["results"].as_array().unwrap().len(), 2);
    for value in final_state.variables["results"].as_array().unwrap() {
        assert_eq!(value["variables"]["marker"], "repeated evidence");
    }
    let before_replay = super::call_tests::transition_rows(&fixture);
    let last = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(last.repetition_occurrences.iter()
        .filter(|row| row.status == ProcessRepetitionOccurrenceStatus::Completed).count(), 2);
    assert_eq!(super::call_tests::transition_rows(&fixture), before_replay);
}

#[tokio::test]
async fn repeated_contract_result_rejects_an_earlier_equal_payload_service_source() {
    let fixture = Fixture::new();
    let business = json!({"outcome":"Completed","code":null,
        "summary":"The pinned contract returned its result",
        "outputs":{"answer":17},"evidence":["registered_flow_result"]});
    let graph = json!({"nodes":[{"id":"trigger","type":"trigger","config":{
        "output_mapping":{"actual_result":business.to_string()}}},
        {"id":"output","type":"output","config":{}}],
        "edges":[{"from":"trigger","to":"output","from_port":"text","to_port":"text"}],
        "variables":[{"name":"actual_result","type":"json"}]}).to_string();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph);
    let mut model = repeated_service_model(&flow_id, ActivityVerification::Human);
    let service = model.nodes.iter_mut().find(|node| node.id == "Service").unwrap();
    let ProcessNodeKind::ServiceTask { result_expression, output_mapping, .. } = &mut service.kind
        else { panic!("fixture Service node changed kind") };
    *result_expression = Some("outputs.variables.actual_result".into());
    output_mapping.insert("answer".into(), "outputs.answer".into());
    let started = start_model(&fixture, &model);
    let mut previous_result: Option<tentaflow_protocol::processes::ProcessEvent> = None;

    for ordinal in 0_u32..2 {
        let worker = format!("static-contract-worker-{ordinal}");
        let claim = repository::claim_job(&fixture.db, &worker, chrono::Utc::now().timestamp_millis())
            .unwrap().expect("actual repeated Contract Service job");
        let occurrence = claim.snapshot.repetition_occurrences.iter()
            .find(|row| row.job_id.as_deref() == Some(claim.job.job_id.as_str())).unwrap();
        assert_eq!(occurrence.ordinal, ordinal);
        let observed = observed_flow_result(&fixture, &claim).await;
        assert_eq!(observed.origin, ActivityResultOrigin::Contract);
        assert_eq!(observed.result.outputs, business["outputs"]);
        let at = chrono::Utc::now().timestamp_millis();
        let plan = plan_recorded_result(&fixture, &claim, &observed, at).unwrap();
        let (event_index, event) = plan.events.iter().enumerate()
            .find(|(_, event)| event.kind == "service_result").unwrap();
        let current_source_id = plan.event_ids.get(&event_index).unwrap().clone();
        if let Some(previous) = &previous_result {
            assert_eq!(event.data, previous.data);
            assert_ne!(current_source_id, previous.event_id);
            assert_eq!(event.scope_id, previous.scope_id);
            let before = super::call_tests::transition_rows(&fixture);
            assert_eq!(before.len(), 17);
            let mut wrong_source = plan.clone();
            wrong_source.repetition_occurrences.iter_mut()
                .find(|row| row.occurrence_id == occurrence.occurrence_id).unwrap()
                .accepted_source_event_id = Some(previous.event_id.clone());
            let error = repository::accept_job_result(&fixture.db, &fixture.owner,
                &claim.job.job_id, claim.job.attempt, claim.job.fence, &worker,
                &observed, claim.snapshot.instance.revision, repository::ProcessPlanInput::Supplied(&wrong_source), at).unwrap_err();
            let message = format!("{error:#}");
            assert!(message.contains("repeated Service source event differs from its fenced accepted entry"), "{message}");
            assert_eq!(super::call_tests::transition_rows(&fixture), before);
        }

        repository::accept_job_result(&fixture.db, &fixture.owner,
            &claim.job.job_id, claim.job.attempt, claim.job.fence, &worker,
            &observed, claim.snapshot.instance.revision, repository::ProcessPlanInput::Supplied(&plan), at).unwrap();
        let current = history(&fixture, &started.instance_id).into_iter()
            .find(|event| event.event_id == current_source_id).unwrap();
        assert_eq!(current.kind, "service_result");
        assert_eq!(current.scope_id, occurrence.scope_id);
        if ordinal == 0 {
            previous_result = Some(current);
        }
        let waiting = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let persisted = waiting.repetition_occurrences.iter()
            .find(|row| row.occurrence_id == occurrence.occurrence_id).unwrap();
        assert_eq!(persisted.status, ProcessRepetitionOccurrenceStatus::AwaitingVerification);
        let task = waiting.user_tasks.iter().find(|task|
            task.user_task_id == *persisted.verification_user_task_id.as_ref().unwrap()).unwrap();
        let command = stamp("approve static Contract occurrence");
        let approval = runtime::plan_user_completion(&waiting, &task.user_task_id,
            &Value::Null, Some(true), at,
            runtime::test_support::human_input(&waiting, &task.user_task_id, &command), None).unwrap();
        repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &started.instance_id, &task.user_task_id, waiting.instance.revision,
            &Value::Null, Some(true), repository::ProcessPlanInput::Supplied(&approval), at).unwrap();
        let after = super::call_tests::transition_rows(&fixture);
        repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &started.instance_id, &task.user_task_id, waiting.instance.revision,
            &Value::Null, Some(true), repository::ProcessPlanInput::Supplied(&approval), at).unwrap();
        assert_eq!(super::call_tests::transition_rows(&fixture), after);
    }

    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let finished = repository::get_instance(&reopened, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(finished.status, ProcessInstanceStatus::Completed);
    assert_eq!(finished.variables["results"], json!([{"answer":17},{"answer":17}]));
}

#[tokio::test]
async fn repeated_needs_human_rejection_keeps_evidence_and_blocks_without_an_automatic_retry() {
    let fixture = Fixture::new();
    let business = json!({"outcome":"NeedsHuman","code":"REVIEW",
        "summary":"Review the actual result","outputs":{"answer":17},
        "evidence":["registered_flow_result"]});
    let graph = json!({"nodes":[{"id":"trigger","type":"trigger","config":{
        "output_mapping":{"actual_result":business.to_string()}}},
        {"id":"output","type":"output","config":{}}],
        "edges":[{"from":"trigger","to":"output","from_port":"text","to_port":"text"}],
        "variables":[{"name":"actual_result","type":"json"}]}).to_string();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph);
    let mut model = repeated_service_model(&flow_id, ActivityVerification::Human);
    let service = model.nodes.iter_mut().find(|node| node.id == "Service").unwrap();
    let ProcessNodeKind::ServiceTask { result_expression, output_mapping, .. } = &mut service.kind
        else { panic!("fixture Service node changed kind") };
    *result_expression = Some("outputs.variables.actual_result".into());
    output_mapping.insert("answer".into(), "outputs.answer".into());
    let started = start_model(&fixture, &model);
    let worker = "needs-human-repeat-worker";
    let claim = repository::claim_job(&fixture.db, worker, chrono::Utc::now().timestamp_millis())
        .unwrap().unwrap();
    jobs::execute_claimed(&fixture.db, fixture.dispatcher(), worker, claim.clone(),
        CancellationToken::new()).await.unwrap();
    let waiting = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let occurrence = waiting.repetition_occurrences.iter().find(|row| row.ordinal == 0).unwrap();
    let job = waiting.jobs.iter().find(|job| job.job_id == claim.job.job_id).unwrap();
    let incident_facts = waiting.incidents.iter()
        .map(|incident| (incident.code.as_str(), incident.message.as_str()))
        .collect::<Vec<_>>();
    let event_kinds = history(&fixture, &started.instance_id).into_iter()
        .map(|event| event.kind).collect::<Vec<_>>();
    assert_eq!(occurrence.status, ProcessRepetitionOccurrenceStatus::AwaitingVerification,
        "job status: {}; incidents: {incident_facts:?}; event kinds: {event_kinds:?}", job.status);
    assert_eq!(job.status, "completed");
    assert_eq!(job.result.as_ref().unwrap().outcome, ActivityOutcome::NeedsHuman);
    assert_eq!(job.result.as_ref().unwrap().evidence, vec!["registered_flow_result"]);
    let task = waiting.user_tasks.iter().find(|task|
        task.user_task_id == *occurrence.verification_user_task_id.as_ref().unwrap()).unwrap();
    let command = stamp("reject repeated NeedsHuman result");
    let at = chrono::Utc::now().timestamp_millis();
    let rejection = runtime::plan_user_completion(&waiting, &task.user_task_id,
        &Value::Null, Some(false), at,
        runtime::test_support::human_input(&waiting, &task.user_task_id, &command), None).unwrap();
    let before = super::call_tests::transition_rows(&fixture);
    assert!(repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &started.instance_id, &task.user_task_id, waiting.instance.revision,
        &Value::Null, Some(true), repository::ProcessPlanInput::Supplied(&rejection), at).is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), before);
    let rejected = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &started.instance_id, &task.user_task_id, waiting.instance.revision,
        &Value::Null, Some(false), repository::ProcessPlanInput::Supplied(&rejection), at).unwrap().instance;
    assert_eq!(rejected.status, ProcessInstanceStatus::Incident);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(persisted.repetition_occurrences[0].status,
        ProcessRepetitionOccurrenceStatus::AcceptedBlocked);
    assert_eq!(persisted.repetition_groups[0].completed_count, 0);
    assert_eq!(persisted.jobs.iter().find(|job| job.job_id == claim.job.job_id).unwrap()
        .result.as_ref().unwrap().evidence, vec!["registered_flow_result"]);
    assert!(repository::claim_job(&fixture.db, worker, chrono::Utc::now().timestamp_millis())
        .unwrap().is_none());
    let before_replay = super::call_tests::transition_rows(&fixture);
    let replay = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &started.instance_id, &task.user_task_id, waiting.instance.revision,
        &Value::Null, Some(false), repository::ProcessPlanInput::Supplied(&rejection), at).unwrap().instance;
    assert_eq!(replay.revision, rejected.revision);
    assert_eq!(super::call_tests::transition_rows(&fixture), before_replay);

    let mut approval_model = model;
    approval_model.variables.insert("items".into(), json!([{"case":"approved"}]));
    let approved_start = start_model(&fixture, &approval_model);
    let approved_claim = repository::claim_job(&fixture.db, worker,
        chrono::Utc::now().timestamp_millis()).unwrap().unwrap();
    jobs::execute_claimed(&fixture.db, fixture.dispatcher(), worker, approved_claim.clone(),
        CancellationToken::new()).await.unwrap();
    let approval_wait = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &approved_start.instance_id).unwrap();
    let approval_occurrence = &approval_wait.repetition_occurrences[0];
    assert_eq!(approval_occurrence.status,
        ProcessRepetitionOccurrenceStatus::AwaitingVerification);
    let approval_task = approval_wait.user_tasks.iter().find(|task|
        task.user_task_id == *approval_occurrence.verification_user_task_id.as_ref().unwrap()).unwrap();
    let approval_command = stamp("approve retained NeedsHuman result");
    let approval_at = chrono::Utc::now().timestamp_millis();
    let approval_plan = runtime::plan_user_completion(&approval_wait,
        &approval_task.user_task_id, &json!({"client":"ignored"}), Some(true), approval_at,
        runtime::test_support::human_input(&approval_wait,
            &approval_task.user_task_id, &approval_command), None).unwrap();
    repository::complete_user_task(&fixture.db, &fixture.owner, &approval_command,
        &approved_start.instance_id, &approval_task.user_task_id,
        approval_wait.instance.revision, &json!({"client":"ignored"}), Some(true),
        repository::ProcessPlanInput::Supplied(&approval_plan), approval_at).unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let approved = repository::get_instance(&reopened, &fixture.owner,
        &approved_start.instance_id, None).unwrap();
    assert_eq!(approved.status, ProcessInstanceStatus::Completed);
    assert_eq!(approved.variables["results"], json!([{"answer":17}]));
    let approved_history = repository::list_events(&reopened, &fixture.owner,
        &approved_start.instance_id, 0, 200).unwrap().0;
    assert_eq!(approved_history.iter().filter(|event|
        event.kind == "service_result" && event.data["result_origin"] == "contract"
            && event.data["evidence"] == json!(["registered_flow_result"])).count(), 1);
    assert_eq!(approved_history.iter().filter(|event|
        event.kind == "verification_approved").count(), 1);
}

#[tokio::test]
async fn ordinary_service_job_payload_uses_authenticated_input_after_reopen() {
    let fixture = Fixture::new();
    let body_graph = json!({
        "nodes": [
            {"id":"trigger","type":"trigger","config":{"output_mapping":{"marker":"payload"}}},
            {"id":"output","type":"output","config":{}}
        ],
        "edges": [{"from":"trigger","to":"output","from_port":"text","to_port":"text"}],
        "variables": [{"name":"marker","type":"json"}]
    }).to_string();
    let flow_id = flow(&fixture.db, &fixture.owner, &body_graph);
    let mut model = service_model(&flow_id, ActivityVerification::Condition {
        expression: "true".into(),
    });
    model.variables.insert("seed".into(), json!(4));
    let service = model.nodes.iter_mut().find(|node| node.id == "Service")
        .expect("configured Service node");
    let ProcessNodeKind::ServiceTask { input_mapping, .. } = &mut service.kind else {
        unreachable!("configured Service node has the ServiceTask kind");
    };
    input_mapping.insert("payload".into(), "inputs.Input_Service".into());
    service.activity_io = Some(ProcessActivityIo {
        data_inputs: vec![ProcessIoDataInput {
            id: "Input_Service".into(),
            name: Some("Service body input".into()),
        }],
        data_outputs: Vec::new(),
        input_set_id: "InputSet_Service".into(),
        input_set: vec!["Input_Service".into()],
        output_set_id: "OutputSet_Service".into(),
        output_set: Vec::new(),
        input_associations: vec![ProcessInputAssociation::CelAssignment {
            id: "Association_Service_Input".into(),
            from_expression: "vars.seed + 10".into(),
            target_input_id: "Input_Service".into(),
        }],
        output_associations: Vec::new(),
        coordinator_output: None,
    });

    let started = start_model(&fixture, &model);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id)
        .unwrap();
    let job = snapshot.jobs.iter().find(|job| job.node_id == "Service")
        .expect("persisted Service job");
    assert_eq!(job.input["payload"], json!(14));
    assert_eq!(job.input["variables"], json!({}));
    assert!(snapshot.activity_io_witnesses.iter().any(|witness|
        witness.activity_kind == "service"
            && witness.phase == "input_captured"
            && witness.input_values.as_ref().is_some_and(|inputs|
                matches!(inputs.as_slice(), [input]
                    if matches!(&input.observed,
                        repository::IoObservedValue::Present { value }
                            if value == &json!(14))))));
    let claimed = repository::claim_job(&reopened, "input-body-worker",
        chrono::Utc::now().timestamp_millis()).unwrap()
        .expect("persisted Service job must be claimable");
    assert_eq!(claimed.job.input["payload"], json!(14));
    jobs::execute_claimed(&reopened, fixture.dispatcher(), "input-body-worker", claimed,
        CancellationToken::new()).await.unwrap();
    let completed = repository::get_instance(&reopened, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert_eq!(completed.variables["answer"], json!(14));
}

#[tokio::test]
async fn ordinary_service_claim_keeps_its_three_field_event_without_repeat_context() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("ordinary evidence", None));
    let started = start_model(&fixture,
        &service_model(&flow_id, ActivityVerification::Condition {
            expression: "true".into(),
        }));
    let worker = "ordinary-claim-worker";
    let claim = repository::claim_job(&fixture.db, worker, chrono::Utc::now().timestamp_millis())
        .unwrap().unwrap();
    let event = history(&fixture, &started.instance_id).into_iter()
        .find(|event| event.kind == "service_claimed").unwrap();
    assert_eq!(event.data, json!({"job_id":claim.job.job_id,
        "attempt":claim.job.attempt,"fence":claim.job.fence}));
    let raw: String = fixture.db.read().unwrap().query_row(
        "SELECT data_json FROM bpmn_events WHERE event_id=?1 AND instance_id=?2",
        rusqlite::params![event.event_id, started.instance_id],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(raw, format!(
        "{{\"job_id\":\"{}\",\"attempt\":{},\"fence\":{}}}",
        claim.job.job_id, claim.job.attempt, claim.job.fence,
    ));
    assert!(claim.snapshot.repetition_groups.is_empty());
    assert!(claim.snapshot.repetition_occurrences.is_empty());
    jobs::execute_claimed(&fixture.db, fixture.dispatcher(), worker, claim,
        CancellationToken::new()).await.unwrap();
    let completed = repository::get_instance(&fixture.db, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
}

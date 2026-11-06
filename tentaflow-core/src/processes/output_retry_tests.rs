// ============ File: output_retry_tests.rs — BPMN B4.5 applying an accepted Service result again: same result, current variables, no second execution ============

use serde_json::{json, Value};
use std::collections::BTreeMap;
use tentaflow_protocol::processes::{
    ActivityVerification, ProcessActivityIo, ProcessBodyModeling, ProcessDataObject,
    ProcessDataObjectReference, ProcessInstance, ProcessInstanceStatus, ProcessIoDataOutput,
    ProcessModel, ProcessNode, ProcessNodeKind, ProcessOutputAssociation, ProcessRetryDenied,
    ProcessRetryKind,
};
use tokio_util::sync::CancellationToken;

use super::{call_tests, jobs, messages};
use super::monitor_tests::{is_not_found, member};
use super::repository::{self, ProcessOperationDenied};
use super::runtime::test_support::*;

fn now() -> i64 {
    super::jobs::now_ms()
}

/// A Service step whose configured output association divides by `vars.divisor`:
/// with the divisor at its starting value 0 the mapping of the accepted result
/// fails, and changing the variable is what lets it succeed later.
fn model_with(
    flow_id: &str,
    output_expression: &str,
    verification: ActivityVerification,
) -> ProcessModel {
    let mut model = service_model(flow_id, verification);
    model.variables.insert("divisor".into(), json!(0));
    model.variables.insert("noise".into(), json!(0));
    model.variables.insert("mapped".into(), Value::Null);
    model.modeling = Some(ProcessBodyModeling {
        data_objects: vec![ProcessDataObject {
            id: "Object_Mapped".into(),
            name: None,
        }],
        data_object_references: vec![ProcessDataObjectReference {
            id: "Ref_Mapped".into(),
            name: None,
            data_object_ref: "Object_Mapped".into(),
            variable_binding_key: Some("mapped".into()),
        }],
        ..ProcessBodyModeling::default()
    });
    let service = model
        .nodes
        .iter_mut()
        .find(|node| node.id == "Service")
        .unwrap();
    service.activity_io = Some(ProcessActivityIo {
        data_inputs: Vec::new(),
        data_outputs: vec![ProcessIoDataOutput {
            id: "Output_Mapped".into(),
            name: None,
            value_expression: output_expression.into(),
        }],
        input_set_id: "InputSet_Service".into(),
        input_set: Vec::new(),
        output_set_id: "OutputSet_Service".into(),
        output_set: vec!["Output_Mapped".into()],
        input_associations: Vec::new(),
        output_associations: vec![ProcessOutputAssociation {
            id: "Association_Mapped".into(),
            source_output_id: "Output_Mapped".into(),
            target_object_ref_id: "Ref_Mapped".into(),
        }],
        coordinator_output: None,
    });
    model
}

struct Blocked {
    instance: ProcessInstance,
    job_id: String,
}

/// Runs the Service job to completion; its accepted result then fails to map.
async fn run_until_blocked(fixture: &Fixture, model: &ProcessModel, label: &str) -> Blocked {
    let started = start_model(fixture, model);
    let claim = repository::claim_job(&fixture.db, label, now())
        .unwrap()
        .unwrap();
    let job_id = claim.job.job_id.clone();
    jobs::execute_claimed(
        &fixture.db,
        fixture.dispatcher(),
        label,
        claim,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let instance =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert_eq!(instance.status, ProcessInstanceStatus::Incident, "{label}");
    Blocked { instance, job_id }
}

fn set_divisor(fixture: &Fixture, instance_id: &str, divisor: i64) {
    let changed = fixture.db.write().unwrap().execute(
        "UPDATE bpmn_instances SET variables_json=json_set(variables_json,'$.divisor',?1) WHERE instance_id=?2",
        rusqlite::params![divisor, instance_id],
    ).unwrap();
    assert_eq!(changed, 1);
}

/// Changes a variable the mapping does not use: the run moved on, the mapping
/// still fails, and the next attempt is offered again.
fn touch(fixture: &Fixture, instance_id: &str, noise: i64) {
    let changed = fixture.db.write().unwrap().execute(
        "UPDATE bpmn_instances SET variables_json=json_set(variables_json,'$.noise',?1) WHERE instance_id=?2",
        rusqlite::params![noise, instance_id],
    ).unwrap();
    assert_eq!(changed, 1);
}

fn reload(fixture: &Fixture, instance_id: &str) -> ProcessInstance {
    repository::get_instance(&fixture.db, &fixture.owner, instance_id, None).unwrap()
}

fn count(fixture: &Fixture, sql: &str, instance_id: &str) -> i64 {
    fixture
        .db
        .read()
        .unwrap()
        .query_row(sql, [instance_id], |row| row.get(0))
        .unwrap()
}

fn retry_command(
    fixture: &Fixture,
    blocked: &ProcessInstance,
    job_id: &str,
    command: &repository::CommandStamp,
) -> anyhow::Result<repository::ProcessTransitionOutcome> {
    repository::retry_job(
        &fixture.db,
        &fixture.owner,
        command,
        &blocked.instance_id,
        job_id,
        blocked.revision,
    )
}

fn retry(
    fixture: &Fixture,
    blocked: &ProcessInstance,
    job_id: &str,
    label: &str,
) -> anyhow::Result<ProcessInstance> {
    retry_command(fixture, blocked, job_id, &stamp(label)).map(|outcome| outcome.instance)
}

/// What a replayed command must not move: events, witness rows and the revision.
fn footprint(fixture: &Fixture, instance_id: &str) -> (i64, i64, i64, i64) {
    let one = |sql: &str| count(fixture, sql, instance_id);
    (
        one("SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1"),
        one("SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1 AND kind IN ('output_retried','output_retry_failed')"),
        one("SELECT COUNT(*) FROM bpmn_activity_io_witnesses WHERE instance_id=?1"),
        one("SELECT revision FROM bpmn_instances WHERE instance_id=?1"),
    )
}

#[tokio::test]
async fn a_blocked_mapping_is_applied_again_to_the_current_variables_without_running_the_tool() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("work done", None));
    let model = model_with(
        &flow_id,
        "10 / vars.divisor",
        ActivityVerification::Condition {
            expression: "true".into(),
        },
    );
    let blocked = run_until_blocked(&fixture, &model, "first-pass").await;
    let instance_id = blocked.instance.instance_id.clone();
    let incident = blocked
        .instance
        .incidents
        .first()
        .expect("the mapping failure")
        .clone();
    assert_eq!(incident.code, "ACTIVITY_IO_OUTPUT_FAILED");
    // Nothing changed since the failure: applying the result again would fail the same way.
    assert_eq!(
        (
            incident.can_retry,
            incident.retry.kind,
            incident.retry.denied_reason
        ),
        (
            false,
            ProcessRetryKind::OutputMapping,
            Some(ProcessRetryDenied::VariablesUnchanged)
        )
    );
    assert!(!blocked.instance.can_retry);
    let refused = retry(&fixture, &blocked.instance, &blocked.job_id, "too early").unwrap_err();
    assert!(
        refused.downcast_ref::<ProcessOperationDenied>().is_some(),
        "{refused}"
    );
    assert_eq!(footprint(&fixture, &instance_id).1, 0, "a refused press uses no generation");
    assert_eq!(
        count(
            &fixture,
            "SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1 AND kind='service_result'",
            &instance_id
        ),
        1
    );

    set_divisor(&fixture, &instance_id, 2);
    let offered = reload(&fixture, &instance_id);
    assert_eq!(
        (
            offered.incidents[0].can_retry,
            offered.incidents[0].retry.denied_reason
        ),
        (true, None)
    );
    assert!(offered.can_retry);
    let command = stamp("apply again");
    let done = retry_command(&fixture, &blocked.instance, &blocked.job_id, &command)
        .unwrap()
        .instance;
    assert_eq!(done.status, ProcessInstanceStatus::Completed);
    assert_eq!(done.variables["mapped"], json!(5));
    assert_eq!(done.variables["answer"], json!("work done"));
    let kinds = history_kinds(&fixture, &instance_id);
    let retried = kinds.iter().position(|kind| kind == "output_retried").unwrap();
    assert_eq!(
        kinds.iter().filter(|kind| *kind == "verification_passed").count(),
        1,
        "the verification verdict given when the result was accepted is recorded as passed once the result is mapped"
    );
    assert!(kinds.iter().rposition(|kind| kind == "verification_passed").unwrap() > retried);
    assert!(done.incidents.iter().all(|incident| incident.retry.kind
        != ProcessRetryKind::OutputMapping
        || !incident.can_retry));

    // The work was not run again and nothing new was accepted.
    assert_eq!(
        count(
            &fixture,
            "SELECT COUNT(*) FROM bpmn_jobs WHERE instance_id=?1",
            &instance_id
        ),
        1
    );
    assert_eq!(
        count(
            &fixture,
            "SELECT COUNT(*) FROM bpmn_service_invocations WHERE instance_id=?1",
            &instance_id
        ),
        1
    );
    assert_eq!(
        count(
            &fixture,
            "SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1 AND kind='service_result'",
            &instance_id
        ),
        1
    );
    assert_eq!(
        count(
            &fixture,
            "SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1 AND kind='output_retried'",
            &instance_id
        ),
        1
    );
    assert_eq!(
        count(
            &fixture,
            "SELECT COUNT(*) FROM bpmn_incidents WHERE instance_id=?1 AND resolved_at_ms IS NULL",
            &instance_id
        ),
        0
    );
    assert_eq!(
        count(
            &fixture,
            "SELECT COUNT(*) FROM bpmn_incidents WHERE instance_id=?1",
            &instance_id
        ),
        1
    );
    // Both generations stay as evidence, the first one blocked, the second applied.
    let generations: Vec<(i64, String)> = fixture.db.read().unwrap().prepare(
        "SELECT generation,phase FROM bpmn_activity_io_witnesses WHERE instance_id=?1 ORDER BY generation",
    ).unwrap().query_map([&instance_id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap()
        .collect::<Result<_, _>>().unwrap();
    assert_eq!(
        generations,
        vec![
            (0, "output_blocked".to_owned()),
            (1, "output_applied".to_owned())
        ]
    );

    // The same command answers again with the same instance and writes nothing.
    let settled = footprint(&fixture, &instance_id);
    let replay = retry_command(&fixture, &blocked.instance, &blocked.job_id, &command)
        .unwrap();
    assert_eq!(replay.instance, done);
    assert!(replay.cancelled_claims.is_empty());
    assert_eq!(footprint(&fixture, &instance_id), settled);
    // Another command on the closed incident is refused instead.
    assert!(retry(&fixture, &blocked.instance, &blocked.job_id, "apply again").is_err());
}

#[tokio::test]
async fn a_failing_retry_keeps_the_one_incident_and_stops_at_the_limit() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("limit", None));
    let model = model_with(
        &flow_id,
        "10 / vars.divisor",
        ActivityVerification::Condition {
            expression: "true".into(),
        },
    );
    let blocked = run_until_blocked(&fixture, &model, "limit-pass").await;
    let instance_id = blocked.instance.instance_id.clone();
    let incident_id = blocked.instance.incidents[0].incident_id.clone();
    let notices = count(&fixture, "SELECT COUNT(*) FROM bpmn_incident_notices n JOIN bpmn_incidents i ON i.incident_id=n.incident_id WHERE i.instance_id=?1", &instance_id);

    let mut current = blocked.instance.clone();
    for attempt in 1..=5 {
        touch(&fixture, &instance_id, attempt);
        current = reload(&fixture, &instance_id);
        let command = stamp(&format!("failing {attempt}"));
        let after = retry_command(&fixture, &current, &blocked.job_id, &command)
            .unwrap()
            .instance;
        if attempt == 1 {
            // A failed attempt is a command too: its replay answers the same and adds
            // no event and no witness generation.
            let settled = footprint(&fixture, &instance_id);
            let replay = retry_command(&fixture, &current, &blocked.job_id, &command).unwrap();
            assert_eq!(replay.instance, after);
            assert_eq!(footprint(&fixture, &instance_id), settled);
        }
        assert_eq!(
            after.status,
            ProcessInstanceStatus::Incident,
            "attempt {attempt}"
        );
        assert!(after.revision > current.revision, "attempt {attempt}");
        assert_eq!(after.incidents.len(), 1);
        assert_eq!(after.incidents[0].incident_id, incident_id);
        current = after;
    }
    // The sixth is refused with its own reason, and the row never multiplied.
    let last = &current.incidents[0];
    assert_eq!(
        (last.can_retry, last.retry.kind, last.retry.denied_reason),
        (
            false,
            ProcessRetryKind::OutputMapping,
            Some(ProcessRetryDenied::OutputRetryLimit)
        )
    );
    let refused = retry(&fixture, &current, &blocked.job_id, "sixth").unwrap_err();
    assert!(
        refused.downcast_ref::<ProcessOperationDenied>().is_some(),
        "{refused}"
    );
    assert_eq!(
        count(
            &fixture,
            "SELECT COUNT(*) FROM bpmn_incidents WHERE instance_id=?1",
            &instance_id
        ),
        1
    );
    assert_eq!(count(&fixture, "SELECT COUNT(*) FROM bpmn_incident_notices n JOIN bpmn_incidents i ON i.incident_id=n.incident_id WHERE i.instance_id=?1", &instance_id), notices);
    assert_eq!(
        count(
            &fixture,
            "SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1 AND kind='output_retry_failed'",
            &instance_id
        ),
        5
    );
    assert_eq!(
        count(
            &fixture,
            "SELECT MAX(generation) FROM bpmn_activity_io_witnesses WHERE instance_id=?1",
            &instance_id
        ),
        5
    );
    assert_eq!(
        count(
            &fixture,
            "SELECT COUNT(*) FROM bpmn_jobs WHERE instance_id=?1",
            &instance_id
        ),
        1
    );
}

#[tokio::test]
async fn only_the_initiator_applies_the_result_again_and_a_stranger_cannot_tell_it_exists() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("who", None));
    let model = model_with(
        &flow_id,
        "10 / vars.divisor",
        ActivityVerification::Condition {
            expression: "true".into(),
        },
    );
    let blocked = run_until_blocked(&fixture, &model, "who-pass").await;
    set_divisor(&fixture, &blocked.instance.instance_id, 5);
    let monitor = member(&fixture, "auditor", "role-org-admin");
    let seen = repository::get_instance(&fixture.db, &monitor, &blocked.instance.instance_id, None)
        .unwrap();
    let incident = &seen.incidents[0];
    assert_eq!(
        (
            incident.can_retry,
            incident.retry.kind,
            incident.retry.denied_reason
        ),
        (
            false,
            ProcessRetryKind::OutputMapping,
            Some(ProcessRetryDenied::ViewOnly)
        )
    );
    let refused = repository::retry_job(
        &fixture.db,
        &monitor,
        &stamp("monitor"),
        &blocked.instance.instance_id,
        &blocked.job_id,
        seen.revision,
    ).map(|outcome| outcome.instance)
    .unwrap_err();
    assert!(
        refused.downcast_ref::<ProcessOperationDenied>().is_some(),
        "{refused}"
    );
    let stranger = member(&fixture, "stranger", "role-org-viewer");
    let hidden = repository::retry_job(
        &fixture.db,
        &stranger,
        &stamp("stranger"),
        &blocked.instance.instance_id,
        &blocked.job_id,
        seen.revision,
    ).map(|outcome| outcome.instance)
    .unwrap_err();
    assert!(is_not_found(&hidden), "{hidden}");
    assert_eq!(
        reload(&fixture, &blocked.instance.instance_id).status,
        ProcessInstanceStatus::Incident
    );
}

#[tokio::test]
async fn a_failing_mapping_of_a_step_without_configured_io_is_applied_again_too() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("plain", None));
    let mut model = service_model(
        &flow_id,
        ActivityVerification::Condition {
            expression: "true".into(),
        },
    );
    model.variables.insert("divisor".into(), json!(0));
    let ProcessNodeKind::ServiceTask { output_mapping, .. } = &mut model
        .nodes
        .iter_mut()
        .find(|node| node.id == "Service")
        .unwrap()
        .kind
    else {
        unreachable!()
    };
    output_mapping.insert("ratio".into(), "10 / vars.divisor".into());
    model.variables.insert("ratio".into(), Value::Null);
    model.variables.insert("noise".into(), json!(0));
    let blocked = run_until_blocked(&fixture, &model, "plain-pass").await;
    let incident = blocked.instance.incidents[0].clone();
    assert_eq!(incident.code, "EXPRESSION_ERROR");
    assert_eq!(
        (incident.can_retry, incident.retry.kind, incident.retry.denied_reason),
        (
            false,
            ProcessRetryKind::OutputMapping,
            Some(ProcessRetryDenied::VariablesUnchanged)
        )
    );
    touch(&fixture, &blocked.instance.instance_id, 1);
    let blocked = Blocked {
        instance: reload(&fixture, &blocked.instance.instance_id),
        job_id: blocked.job_id,
    };
    let again = retry(
        &fixture,
        &blocked.instance,
        &blocked.job_id,
        "plain fails again",
    )
    .unwrap();
    assert_eq!(again.status, ProcessInstanceStatus::Incident);
    assert_eq!(again.incidents.len(), 1);
    assert_eq!(again.incidents[0].incident_id, incident.incident_id);
    // The attempt that just failed saw these variables: pressing again is refused until something changes.
    assert_eq!(
        (again.incidents[0].can_retry, again.incidents[0].retry.denied_reason),
        (false, Some(ProcessRetryDenied::VariablesUnchanged))
    );
    set_divisor(&fixture, &blocked.instance.instance_id, 4);
    let again = reload(&fixture, &blocked.instance.instance_id);
    let done = retry(&fixture, &again, &blocked.job_id, "plain succeeds").unwrap();
    assert_eq!(done.status, ProcessInstanceStatus::Completed);
    assert_eq!(done.variables["ratio"], json!(2));
    assert_eq!(
        count(
            &fixture,
            "SELECT COUNT(*) FROM bpmn_jobs WHERE instance_id=?1",
            &blocked.instance.instance_id
        ),
        1
    );
}

#[tokio::test]
async fn a_failing_verification_condition_is_not_a_mapping_and_is_not_applied_again() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("condition", None));
    let mut model = service_model(
        &flow_id,
        ActivityVerification::Condition {
            expression: "10 / vars.divisor > 1".into(),
        },
    );
    model.variables.insert("divisor".into(), json!(0));
    let blocked = run_until_blocked(&fixture, &model, "condition-pass").await;
    let incident = &blocked.instance.incidents[0];
    assert_eq!(incident.code, "EXPRESSION_ERROR");
    assert_eq!(
        (
            incident.can_retry,
            incident.retry.kind,
            incident.retry.denied_reason
        ),
        (
            false,
            ProcessRetryKind::None,
            Some(ProcessRetryDenied::SameFailureRepeats)
        )
    );
    let refused = retry(&fixture, &blocked.instance, &blocked.job_id, "condition").unwrap_err();
    assert!(
        refused.downcast_ref::<ProcessOperationDenied>().is_some(),
        "{refused}"
    );
}

fn history_kinds(fixture: &Fixture, instance_id: &str) -> Vec<String> {
    repository::list_events(&fixture.db, &fixture.owner, instance_id, 0, 200)
        .unwrap()
        .0
        .into_iter()
        .map(|event| event.kind)
        .collect()
}

/// The blocked Service step followed by a call of `target`, then the end.
fn caller_after_service(
    flow_id: &str,
    target: &tentaflow_protocol::processes::ProcessVersion,
) -> ProcessModel {
    let mut model = model_with(
        flow_id,
        "10 / vars.divisor",
        ActivityVerification::Condition {
            expression: "true".into(),
        },
    );
    let call = call_tests::caller(target, BTreeMap::new())
        .nodes
        .into_iter()
        .find(|node| node.id == "Call_1")
        .unwrap();
    model.nodes.insert(2, call);
    model.sequence_flows = vec![
        edge("ToService", "Start_1", "Service"),
        edge("ToCall", "Service", "Call_1"),
        edge("CallEnd", "Call_1", "End_1"),
    ];
    model
}

async fn run_claimed(fixture: &Fixture, label: &str) -> String {
    let claim = repository::claim_job(&fixture.db, label, now())
        .unwrap()
        .unwrap();
    let job_id = claim.job.job_id.clone();
    jobs::execute_claimed(
        &fixture.db,
        fixture.dispatcher(),
        label,
        claim,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    job_id
}

#[tokio::test]
async fn a_retry_that_reaches_a_call_starts_the_child_and_the_parent_resumes_when_it_returns() {
    for child_has_service in [false, true] {
        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db, &fixture.owner, &graph("call after retry", None));
        let child_model = if child_has_service {
            service_model(
                &flow_id,
                ActivityVerification::Condition {
                    expression: "true".into(),
                },
            )
        } else {
            super::model::starter_model()
        };
        let target = publish_model(&fixture, &child_model);
        let parent_version = publish_model(&fixture, &caller_after_service(&flow_id, &target));
        let parent = messages::test_support::start_version(&fixture, &parent_version);
        let job_id = run_claimed(&fixture, "call-parent").await;
        let blocked = reload(&fixture, &parent.instance_id);
        assert_eq!(blocked.status, ProcessInstanceStatus::Incident);
        assert!(
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &parent.instance_id)
                .unwrap()
                .calls
                .is_empty(),
            "the call is not reached while the mapping is blocked"
        );

        set_divisor(&fixture, &parent.instance_id, 2);
        let done = retry(&fixture, &blocked, &job_id, "retry into a call").unwrap();
        let child_id = call_tests::child_id(&fixture, &parent.instance_id);
        let child = reload(&fixture, &child_id);
        if child_has_service {
            let open = |status: ProcessInstanceStatus| {
                matches!(status, ProcessInstanceStatus::Running | ProcessInstanceStatus::Waiting)
            };
            assert!(open(done.status.clone()), "{:?}", done.status);
            assert!(open(child.status.clone()), "{:?}", child.status);
            run_claimed(&fixture, "call-child").await;
            assert_eq!(
                reload(&fixture, &child_id).status,
                ProcessInstanceStatus::Completed
            );
        } else {
            assert_eq!(child.status, ProcessInstanceStatus::Completed);
        }
        assert_eq!(
            reload(&fixture, &parent.instance_id).status,
            ProcessInstanceStatus::Completed,
            "the call token resumed the parent (child has a service step: {child_has_service})"
        );
        assert_eq!(
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &parent.instance_id)
                .unwrap()
                .calls
                .len(),
            1
        );
    }
}

#[tokio::test]
async fn a_retry_that_reaches_a_terminate_end_cancels_the_running_job_of_a_parallel_branch() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("terminate after retry", None));
    let mut model = model_with(
        &flow_id,
        "10 / vars.divisor",
        ActivityVerification::Condition {
            expression: "true".into(),
        },
    );
    let ProcessNodeKind::ServiceTask { .. } = &model
        .nodes
        .iter()
        .find(|node| node.id == "Service")
        .unwrap()
        .kind
    else {
        unreachable!()
    };
    let mut other = model.nodes.iter().find(|node| node.id == "Service").unwrap().clone();
    other.id = "Other".into();
    other.activity_io = None;
    if let ProcessNodeKind::ServiceTask { output_mapping, .. } = &mut other.kind {
        output_mapping.clear();
    }
    model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::TerminateEnd;
    for (id, kind) in [
        ("Split", ProcessNodeKind::ParallelGateway),
        ("End_2", ProcessNodeKind::TerminateEnd),
    ] {
        model.nodes.push(ProcessNode {
            id: id.into(),
            name: id.into(),
            kind,
            repeat: None,
            activity_io: None,
        });
    }
    model.nodes.push(other);
    model.sequence_flows = vec![
        edge("ToSplit", "Start_1", "Split"),
        edge("SplitService", "Split", "Service"),
        edge("SplitOther", "Split", "Other"),
        edge("ServiceEnd", "Service", "End_1"),
        edge("OtherEnd", "Other", "End_2"),
    ];
    let started = start_model(&fixture, &model);

    let first = repository::claim_job(&fixture.db, "terminate-a", now())
        .unwrap()
        .unwrap();
    let second = repository::claim_job(&fixture.db, "terminate-b", now())
        .unwrap()
        .unwrap();
    let (blocked_claim, blocked_worker, running_claim) = if first.job.node_id == "Service" {
        (first, "terminate-a", second)
    } else {
        (second, "terminate-b", first)
    };
    let running_job = running_claim.job.job_id.clone();
    let blocked_job = blocked_claim.job.job_id.clone();
    jobs::execute_claimed(
        &fixture.db,
        fixture.dispatcher(),
        blocked_worker,
        blocked_claim,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let blocked = reload(&fixture, &started.instance_id);
    assert_eq!(blocked.incidents.len(), 1, "{:?}", blocked.status);

    set_divisor(&fixture, &started.instance_id, 2);
    let outcome = retry_command(&fixture, &blocked, &blocked_job, &stamp("retry into terminate"))
        .unwrap();
    assert_eq!(outcome.instance.status, ProcessInstanceStatus::Completed);
    assert!(
        outcome
            .cancelled_claims
            .iter()
            .any(|claim| claim.job_id == running_job),
        "the running branch job is handed to the worker as cancelled: {:?}",
        outcome.cancelled_claims
    );
    assert_eq!(
        count(
            &fixture,
            "SELECT COUNT(*) FROM bpmn_jobs WHERE instance_id=?1 AND job_id IN (SELECT job_id FROM bpmn_jobs WHERE status IN ('queued','running','claimed'))",
            &started.instance_id
        ),
        0,
        "no job of the ended run stays open"
    );
}

#[tokio::test]
async fn an_open_boundary_incident_on_the_activation_refuses_the_retry_with_its_own_reason() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("boundary incident", None));
    let mut model = model_with(
        &flow_id,
        "10 / vars.divisor",
        ActivityVerification::Condition {
            expression: "true".into(),
        },
    );
    for (id, kind) in [
        (
            "Guard",
            ProcessNodeKind::BoundaryTimer {
                attached_to_id: "Service".into(),
                cancel_activity: false,
                timer: tentaflow_protocol::processes::ProcessTimerSpec::Duration { seconds: 3600 },
            },
        ),
        ("End_Guard", ProcessNodeKind::End),
    ] {
        model.nodes.push(ProcessNode {
            id: id.into(),
            name: id.into(),
            kind,
            repeat: None,
            activity_io: None,
        });
    }
    model.sequence_flows.push(edge("GuardEnd", "Guard", "End_Guard"));
    model.timer_timezone = Some("UTC".into());
    let blocked = run_until_blocked(&fixture, &model, "boundary-pass").await;
    let instance_id = blocked.instance.instance_id.clone();
    {
        // The boundary timer of the waiting activation fails the way a storage
        // failure at its firing leaves it: status error, an open TIMER_ERROR
        // incident and the event that links the two.
        let conn = fixture.db.write().unwrap();
        let (timer_id, token_id): (String, String) = conn
            .query_row(
                "SELECT timer_id,token_id FROM bpmn_timers WHERE instance_id=?1 AND node_id='Guard'",
                [&instance_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        conn.execute(
            "UPDATE bpmn_timers SET status='error',last_reason='forged' WHERE timer_id=?1",
            [&timer_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO bpmn_incidents(incident_id,instance_id,scope_id,node_id,job_id,code,message,at_ms) VALUES('forged-incident',?1,?1,'Guard',NULL,'TIMER_ERROR','forged',99)",
            [&instance_id],
        )
        .unwrap();
        let next_seq: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",
                [&instance_id],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO bpmn_events(event_id,instance_id,seq,at_ms,kind,node_id,actor_user_id,data_json,scope_id) VALUES(?1,?2,?3,99,'timer_error','Guard',NULL,?4,?2)",
            rusqlite::params![
                uuid::Uuid::new_v4().to_string(),
                instance_id,
                next_seq,
                json!({"timer_id":timer_id,"incident_id":"forged-incident","attached_token_id":token_id})
                    .to_string()
            ],
        )
        .unwrap();
    }
    set_divisor(&fixture, &instance_id, 2);
    let seen = reload(&fixture, &instance_id);
    let mapping = seen
        .incidents
        .iter()
        .find(|incident| incident.code == "ACTIVITY_IO_OUTPUT_FAILED")
        .unwrap();
    assert_eq!(
        (mapping.can_retry, mapping.retry.kind, mapping.retry.denied_reason),
        (
            false,
            ProcessRetryKind::OutputMapping,
            Some(ProcessRetryDenied::ResultNotRetained)
        )
    );
    let refused = retry(&fixture, &seen, &blocked.job_id, "with a boundary incident").unwrap_err();
    assert!(
        refused.downcast_ref::<ProcessOperationDenied>().is_some(),
        "{refused}"
    );
    assert_eq!(footprint(&fixture, &instance_id).1, 0, "nothing was attempted");
}

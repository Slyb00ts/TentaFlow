// ============ File: output_retry_tests.rs — BPMN B4.5 applying an accepted Service result again: same result, current variables, no second execution ============

use serde_json::{json, Value};
use tentaflow_protocol::processes::{
    ActivityVerification, ProcessActivityIo, ProcessBodyModeling, ProcessDataObject,
    ProcessDataObjectReference, ProcessInstance, ProcessInstanceStatus, ProcessIoDataOutput,
    ProcessModel, ProcessNodeKind, ProcessOutputAssociation, ProcessRetryDenied, ProcessRetryKind,
};
use tokio_util::sync::CancellationToken;

use super::jobs;
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

fn retry(
    fixture: &Fixture,
    blocked: &ProcessInstance,
    job_id: &str,
    label: &str,
) -> anyhow::Result<ProcessInstance> {
    repository::retry_job(
        &fixture.db,
        &fixture.owner,
        &stamp(label),
        &blocked.instance_id,
        job_id,
        blocked.revision,
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
    assert_eq!(
        (
            incident.can_retry,
            incident.retry.kind,
            incident.retry.denied_reason
        ),
        (true, ProcessRetryKind::OutputMapping, None)
    );
    assert!(blocked.instance.can_retry);
    assert_eq!(
        count(
            &fixture,
            "SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1 AND kind='service_result'",
            &instance_id
        ),
        1
    );

    set_divisor(&fixture, &instance_id, 2);
    let done = retry(&fixture, &blocked.instance, &blocked.job_id, "apply again").unwrap();
    assert_eq!(done.status, ProcessInstanceStatus::Completed);
    assert_eq!(done.variables["mapped"], json!(5));
    assert_eq!(done.variables["answer"], json!("work done"));
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

    // The same command answers again with the same instance.
    let replay = repository::retry_job(
        &fixture.db,
        &fixture.owner,
        &stamp("apply again"),
        &instance_id,
        &blocked.job_id,
        blocked.instance.revision,
    );
    assert!(
        replay.is_err(),
        "a different stamp is a different command, and the incident is closed"
    );
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
        let after = retry(
            &fixture,
            &current,
            &blocked.job_id,
            &format!("failing {attempt}"),
        )
        .unwrap();
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
    )
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
    )
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
    let blocked = run_until_blocked(&fixture, &model, "plain-pass").await;
    let incident = blocked.instance.incidents[0].clone();
    assert_eq!(incident.code, "EXPRESSION_ERROR");
    assert_eq!(
        (incident.can_retry, incident.retry.kind),
        (true, ProcessRetryKind::OutputMapping)
    );
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
    set_divisor(&fixture, &blocked.instance.instance_id, 4);
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

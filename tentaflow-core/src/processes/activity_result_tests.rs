// ============ File: activity_result_tests.rs — actual pinned Flow activity results and atomic binding rejection ============

use std::collections::BTreeMap;

use serde_json::json;
use tentaflow_protocol::processes::{
    ActivityOutcome, ActivityVerification, PinnedFlowInfo, ProcessInstanceStatus, ProcessNode,
    ProcessNodeKind,
};
use tokio_util::sync::CancellationToken;

use super::repository::{self, PinnedServiceSnapshot};
use super::runtime::test_support::{edge, flow, service_model, stamp, start_model, Fixture};

fn result_graph(outcome: &str) -> String {
    let expression = format!("{{'outcome':'{outcome}','code':'REJECTED','summary':'Actual work result','outputs':{{'marker':'actual-result'}},'evidence':['log:work-1']}}");
    json!({"nodes":[{"id":"trigger","type":"trigger","config":{}},
        {"id":"result","type":"activity_result","config":{"result_expression":expression}}],
        "edges":[{"from":"trigger","to":"result","from_port":"text","to_port":"in"}]})
    .to_string()
}

#[tokio::test]
async fn real_activity_result_terminal_preserves_all_outcomes_and_survives_reopen() {
    for (name, expected) in [
        ("Completed", ActivityOutcome::Completed),
        ("Error", ActivityOutcome::Error),
        ("NeedsHuman", ActivityOutcome::NeedsHuman),
        ("Cancelled", ActivityOutcome::Cancelled),
    ] {
        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db, &fixture.owner, &result_graph(name));
        let mut model = service_model(
            &flow_id,
            ActivityVerification::Condition {
                expression: "true".into(),
            },
        );
        let ProcessNodeKind::ServiceTask {
            result_expression,
            output_mapping,
            ..
        } = &mut model.nodes[1].kind
        else {
            panic!("service fixture changed kind");
        };
        *result_expression = Some("outputs.payload".into());
        *output_mapping = BTreeMap::from([("answer".into(), "outputs.marker".into())]);
        let started = start_model(&fixture, &model);
        let worker = "activity-result-worker";
        let claim =
            repository::claim_job(&fixture.db, worker, chrono::Utc::now().timestamp_millis())
                .unwrap()
                .unwrap();
        super::jobs::execute_claimed(
            &fixture.db,
            fixture.dispatcher(),
            worker,
            claim.clone(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let actual =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        let job = actual
            .jobs
            .iter()
            .find(|job| job.job_id == claim.job.job_id)
            .unwrap();
        let result = job.result.as_ref().unwrap();
        assert_eq!(result.outcome, expected);
        assert_eq!(result.outputs, json!({"marker":"actual-result"}));
        assert_eq!(result.evidence, vec!["log:work-1"]);
        assert_eq!(result.summary, "Actual work result");
        if expected == ActivityOutcome::Completed {
            assert_eq!(actual.instance.status, ProcessInstanceStatus::Completed);
            assert_eq!(actual.instance.variables["answer"], "actual-result");
        } else {
            assert_ne!(actual.instance.status, ProcessInstanceStatus::Completed);
        }
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let retained =
            repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id).unwrap();
        assert_eq!(
            retained
                .jobs
                .iter()
                .find(|job| job.job_id == claim.job.job_id)
                .unwrap()
                .result,
            job.result
        );
    }
}

#[test]
fn publication_refuses_missing_result_binding_without_writing_process_rows() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &result_graph("Error"));
    let model = service_model(&flow_id, ActivityVerification::Human);
    let definition = repository::save_definition(
        &fixture.db,
        &fixture.owner,
        &stamp("save unbound result"),
        None,
        0,
        "Unbound result",
        "",
        &model,
    )
    .unwrap();
    let dispatcher = fixture.dispatcher();
    let meta = dispatcher
        .authorize_process_flow(&flow_id, &fixture.owner.user_id, &fixture.owner.org_id)
        .unwrap();
    let pinned = dispatcher.snapshot_flow(&flow_id, &meta).unwrap();
    let snapshots = vec![PinnedServiceSnapshot {
        info: PinnedFlowInfo {
            node_id: "Service".into(),
            flow_id: pinned.flow_id,
            source_version: pinned.source_version,
            graph_sha256: pinned.graph_sha256,
        },
        graph_json: pinned.graph_json,
        role: Default::default(),
        implementation_kind: Default::default(),
    }];
    let before = super::call_tests::transition_rows(&fixture);
    let error = repository::publish_definition(
        &fixture.db,
        &fixture.owner,
        &stamp("publish unbound result"),
        &definition.definition_id,
        definition.draft_revision,
        &snapshots,
        None,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("requires result expression outputs.payload"));
    assert_eq!(super::call_tests::transition_rows(&fixture), before);
}

#[tokio::test]
async fn explicit_result_error_reaches_the_real_boundary_and_review_task() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &result_graph("Error"));
    let mut model = service_model(
        &flow_id,
        ActivityVerification::Condition {
            expression: "true".into(),
        },
    );
    let ProcessNodeKind::ServiceTask {
        result_expression,
        output_mapping,
        ..
    } = &mut model.nodes[1].kind
    else {
        panic!("service fixture changed kind");
    };
    *result_expression = Some("outputs.payload".into());
    output_mapping.clear();
    model.nodes.push(ProcessNode {
        id: "CatchError".into(),
        name: "Catch explicit result error".into(),
        kind: ProcessNodeKind::BoundaryError {
            attached_to_id: "Service".into(),
            error_ref: None,
            output_mapping: BTreeMap::new(),
        },
        repeat: None,
        activity_io: None,
    });
    model.nodes.push(ProcessNode {
        id: "Review".into(),
        name: "Review failed work".into(),
        kind: ProcessNodeKind::UserTask {
            assignee_user_id: None,
            output_mapping: BTreeMap::new(),
        },
        repeat: None,
        activity_io: None,
    });
    model.sequence_flows.extend([
        edge("ErrorToReview", "CatchError", "Review"),
        edge("ReviewToEnd", "Review", "End_1"),
    ]);
    let started = start_model(&fixture, &model);
    let worker = "activity-result-boundary-worker";
    let claim = repository::claim_job(&fixture.db, worker, chrono::Utc::now().timestamp_millis())
        .unwrap()
        .unwrap();
    super::jobs::execute_claimed(
        &fixture.db,
        fixture.dispatcher(),
        worker,
        claim,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let actual =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
    assert!(actual
        .user_tasks
        .iter()
        .any(|task| task.node_id == "Review"));
    let history =
        repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 100)
            .unwrap()
            .0;
    assert_eq!(
        history
            .iter()
            .filter(|event| event.kind == "business_error_caught")
            .count(),
        1
    );
    assert_ne!(actual.instance.status, ProcessInstanceStatus::Completed);
}

fn completed_flow_fixture() -> (Fixture, tentaflow_protocol::processes::ProcessInstance) {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &result_graph("Completed"));
    let mut model = service_model(
        &flow_id,
        ActivityVerification::Condition { expression: "true".into() },
    );
    let ProcessNodeKind::ServiceTask { result_expression, output_mapping, .. } =
        &mut model.nodes[1].kind
    else {
        panic!("service fixture changed kind");
    };
    *result_expression = Some("outputs.payload".into());
    output_mapping.clear();
    let started = start_model(&fixture, &model);
    (fixture, started)
}

fn open_incident_codes(fixture: &Fixture, instance_id: &str) -> Vec<String> {
    let conn = fixture.db.read().unwrap();
    let mut statement = conn
        .prepare("SELECT code FROM bpmn_incidents WHERE instance_id=?1 AND resolved_at_ms IS NULL")
        .unwrap();
    statement.query_map([instance_id], |row| row.get(0)).unwrap()
        .collect::<Result<_, _>>().unwrap()
}

/// Arms a trigger that runs `statement` the moment the flow result is durably
/// observed, so the authorization that follows sees its effect.
fn change_state_when_observed(fixture: &Fixture, statement: &str) {
    fixture.db.write().unwrap().execute_batch(&format!(
        "CREATE TRIGGER change_state_when_observed AFTER UPDATE ON bpmn_service_invocations
         WHEN NEW.phase='observed' BEGIN {statement}; END;"
    )).unwrap();
}

#[tokio::test]
async fn an_authorization_read_that_fails_after_the_flow_ran_does_not_block_its_result() {
    let (fixture, started) = completed_flow_fixture();
    change_state_when_observed(&fixture, "UPDATE user_accounts SET email=X'00'");
    let worker = "unreadable-account-worker";
    let claim = repository::claim_job(&fixture.db, worker, chrono::Utc::now().timestamp_millis())
        .unwrap().unwrap();
    super::jobs::execute_claimed(&fixture.db, fixture.dispatcher(), worker, claim.clone(),
        CancellationToken::new()).await.unwrap();
    let after = repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
        .unwrap();
    assert!(open_incident_codes(&fixture, &started.instance_id).is_empty(),
        "a failed read is not a revocation");
    assert_ne!(after.instance.status, ProcessInstanceStatus::Incident);
    let phase: String = fixture.db.read().unwrap().query_row(
        "SELECT phase FROM bpmn_service_invocations WHERE job_id=?1",
        [&claim.job.job_id], |row| row.get(0)).unwrap();
    assert_eq!(phase, "observed", "the result stays recorded and recoverable");
}

#[tokio::test]
async fn a_true_denial_after_the_flow_ran_still_blocks_its_result() {
    let (fixture, started) = completed_flow_fixture();
    change_state_when_observed(&fixture, "UPDATE flows SET status='draft'");
    let worker = "revoked-account-worker";
    let claim = repository::claim_job(&fixture.db, worker, chrono::Utc::now().timestamp_millis())
        .unwrap().unwrap();
    super::jobs::execute_claimed(&fixture.db, fixture.dispatcher(), worker, claim,
        CancellationToken::new()).await.unwrap();
    assert_eq!(open_incident_codes(&fixture, &started.instance_id).len(), 1);
    let revoked: bool = fixture.db.read().unwrap().query_row(
        "SELECT EXISTS(SELECT 1 FROM bpmn_incidents WHERE instance_id=?1 AND message LIKE '%authority was revoked%')",
        [&started.instance_id], |row| row.get(0)).unwrap();
    assert!(revoked, "the result is blocked as observed after revocation");
}

#[tokio::test]
async fn an_unreadable_account_defers_a_flow_claim_instead_of_revoking_the_source() {
    let (fixture, started) = completed_flow_fixture();
    let worker = "deferring-flow-worker";
    let claim = repository::claim_job(&fixture.db, worker, chrono::Utc::now().timestamp_millis())
        .unwrap().unwrap();
    fixture.db.write().unwrap().execute("UPDATE user_accounts SET email=X'00'", []).unwrap();
    super::jobs::execute_claimed(&fixture.db, fixture.dispatcher(), worker, claim.clone(),
        CancellationToken::new()).await.unwrap();
    assert!(open_incident_codes(&fixture, &started.instance_id).is_empty());
    let status: String = fixture.db.read().unwrap().query_row(
        "SELECT status FROM bpmn_jobs WHERE job_id=?1", [&claim.job.job_id],
        |row| row.get(0)).unwrap();
    assert_eq!(status, "queued");
}

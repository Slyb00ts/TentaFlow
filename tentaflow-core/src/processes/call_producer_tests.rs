// ============ File: processes/call_producer_tests.rs — file-backed child timer, message, and verification call returns ============

use super::{call_tests, jobs, messages, repository, runtime, timers};
use runtime::test_support::{edge, flow, graph, publish_model, service_model, Fixture};
use serde_json::json;
use std::collections::BTreeMap;
use tentaflow_protocol::processes::{
    ActivityVerification, ProcessInstanceStatus, ProcessMessageStatus, ProcessNode,
    ProcessNodeKind, ProcessTimerSpec, ProcessUserTaskKind, ProcessUserTaskStatus,
};

fn return_count(fixture: &Fixture, instance_id: &str) -> usize {
    repository::list_events(&fixture.db, &fixture.owner, instance_id, 0, 200)
        .unwrap()
        .0
        .iter()
        .filter(|event| event.kind == "call_returned")
        .count()
}

#[test]
fn child_timer_catch_fires_and_returns_parent_once() {
    let fixture = Fixture::new();
    let mut child_model = super::model::starter_model();
    child_model.timer_timezone = Some("UTC".into());
    child_model.nodes.insert(
        1,
        ProcessNode {
            id: "Wait".into(),
            name: "Wait for evidence deadline".into(),
            kind: ProcessNodeKind::TimerCatch {
                timer: ProcessTimerSpec::Duration { seconds: 1 },
            },
        },
    );
    child_model.sequence_flows = vec![
        edge("ToWait", "Start_1", "Wait"),
        edge("WaitEnd", "Wait", "End_1"),
    ];
    let target = publish_model(&fixture, &child_model);
    let caller = publish_model(&fixture, &call_tests::caller(&target, BTreeMap::new()));
    let parent = messages::test_support::start_version(&fixture, &caller);
    let child_id = call_tests::child_id(&fixture, &parent.instance_id);
    let child = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    assert_eq!(child.instance.status, ProcessInstanceStatus::Waiting);
    assert_eq!(parent.status, ProcessInstanceStatus::Waiting);
    assert_eq!(child.timers.len(), 1);
    let due = child.timers[0].due_at_ms.unwrap();
    let drained = timers::drain_due(&fixture.db, due);
    drained.completion.unwrap();
    assert_eq!(drained.fired, 1);
    assert!(drained.cancelled_claims.is_empty());
    assert_eq!(
        repository::get_instance(&fixture.db, &fixture.owner, &child_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Completed
    );
    assert_eq!(
        repository::get_instance(&fixture.db, &fixture.owner, &parent.instance_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Completed
    );
    assert_eq!(return_count(&fixture, &parent.instance_id), 1);
    let repeated = timers::drain_due(&fixture.db, due + 1);
    repeated.completion.unwrap();
    assert_eq!(repeated.fired, 0);
    assert_eq!(return_count(&fixture, &parent.instance_id), 1);
}

#[test]
fn child_message_catch_delivers_to_exact_subscription_and_returns_parent_once() {
    let fixture = Fixture::new();
    let child_model = messages::test_support::receiving_model(false, false);
    let target = publish_model(&fixture, &child_model);
    let caller = publish_model(&fixture, &call_tests::caller(&target, BTreeMap::new()));
    let parent = messages::test_support::start_version(&fixture, &caller);
    let child_id = call_tests::child_id(&fixture, &parent.instance_id);
    let child = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    assert_eq!(child.instance.status, ProcessInstanceStatus::Waiting);
    assert_eq!(parent.status, ProcessInstanceStatus::Waiting);
    assert_eq!(child.subscriptions.len(), 1);
    let message = messages::test_support::envelope(
        messages::test_support::catch_target(
            &target,
            Some(&child_id),
            Some(&child.subscriptions[0].subscription_id),
        ),
        json!({"actual_evidence_ID": "case-1"}),
    );
    let admitted = messages::test_support::send(&fixture, &message);
    assert_eq!(admitted.status, ProcessMessageStatus::Pending);
    let drained = messages::drain_pending(&fixture.db, admitted.received_at_ms);
    drained.completion.unwrap();
    assert_eq!(drained.delivered, 1);
    assert_eq!(
        messages::test_support::current(&fixture, &message)
            .message
            .status,
        ProcessMessageStatus::Delivered
    );
    assert_eq!(
        repository::get_instance(&fixture.db, &fixture.owner, &child_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Completed
    );
    assert_eq!(
        repository::get_instance(&fixture.db, &fixture.owner, &parent.instance_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Completed
    );
    assert_eq!(return_count(&fixture, &parent.instance_id), 1);
    let repeated = messages::drain_pending(&fixture.db, admitted.received_at_ms + 1);
    repeated.completion.unwrap();
    assert_eq!(repeated.delivered, 0);
    assert_eq!(return_count(&fixture, &parent.instance_id), 1);
}

#[tokio::test]
async fn accepted_child_service_result_waits_for_human_verification_before_single_parent_return() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("accepted result", None));
    let child_model = service_model(&flow_id, ActivityVerification::Human);
    let target = publish_model(&fixture, &child_model);
    let caller = publish_model(&fixture, &call_tests::caller(&target, BTreeMap::new()));
    let parent = messages::test_support::start_version(&fixture, &caller);
    let child_id = call_tests::child_id(&fixture, &parent.instance_id);
    let claim = repository::claim_job(
        &fixture.db,
        "call-human-verification",
        chrono::Utc::now().timestamp_millis(),
    )
    .unwrap()
    .unwrap();
    jobs::execute_claimed(
        &fixture.db,
        fixture.dispatcher(),
        "call-human-verification",
        claim,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    let child = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    let task = child
        .user_tasks
        .iter()
        .find(|task| task.kind == ProcessUserTaskKind::Verification)
        .expect("real human verification task");
    assert_eq!(task.status, ProcessUserTaskStatus::Open);
    assert_eq!(child.jobs.len(), 1);
    assert_eq!(child.jobs[0].status, "completed");
    assert!(child.jobs[0].result.is_some());
    assert_eq!(child.instance.status, ProcessInstanceStatus::Waiting);
    assert_eq!(
        repository::get_instance(&fixture.db, &fixture.owner, &parent.instance_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Waiting
    );
    assert_eq!(return_count(&fixture, &parent.instance_id), 0);
    let at = chrono::Utc::now().timestamp_millis();
    let output = json!({});
    let plan =
        runtime::plan_user_completion(&child, &task.user_task_id, &output, Some(true), at).unwrap();
    let command = runtime::test_support::stamp("approve called service result");
    let approved = repository::complete_user_task(
        &fixture.db,
        &fixture.owner,
        &command,
        &child_id,
        &task.user_task_id,
        child.instance.revision,
        &output,
        Some(true),
        &plan,
        at,
    )
    .unwrap();
    assert_eq!(approved.instance.status, ProcessInstanceStatus::Completed);
    assert_eq!(
        repository::get_instance(&fixture.db, &fixture.owner, &parent.instance_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Completed
    );
    assert_eq!(return_count(&fixture, &parent.instance_id), 1);
    let replay = repository::complete_user_task(
        &fixture.db,
        &fixture.owner,
        &command,
        &child_id,
        &task.user_task_id,
        child.instance.revision,
        &output,
        Some(true),
        &plan,
        at,
    )
    .unwrap();
    assert_eq!(replay.instance, approved.instance);
    assert_eq!(return_count(&fixture, &parent.instance_id), 1);
}

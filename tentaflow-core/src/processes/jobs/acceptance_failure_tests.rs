// ============ File: acceptance_failure_tests.rs — what an observed result does when its acceptance fails ============

use std::sync::atomic::{AtomicUsize, Ordering};

use tentaflow_protocol::processes::{
    ActivityVerification, ProcessInstanceStatus, ProcessRepetitionOccurrenceStatus,
};

use super::super::repetition_group_incident_tests::{service_result, Member, ParallelGroup};
use super::super::repository::{self, ProcessPlanInput};
use super::{finish_observed, AccessCheck, ServiceJob};

fn condition_true() -> ActivityVerification {
    ActivityVerification::Condition { expression: "true".into() }
}

async fn two_completed_ordinals() -> ParallelGroup {
    ParallelGroup::start(
        [service_result("Completed", None, 1), service_result("Completed", None, 2)],
        condition_true()).await
}

/// Drives the worker's own publication step for an already observed result.
fn publish(group: &ParallelGroup, member: &Member, authorize: &(dyn Fn() -> AccessCheck + Send + Sync)) {
    let cancel = tokio_util::sync::CancellationToken::new();
    let effective = serde_json::Value::Null;
    let job = ServiceJob {
        pool: &group.fixture.db,
        dispatcher: group.fixture.dispatcher(),
        worker_id: &member.worker,
        claimed: &member.claim,
        cancel: &cancel,
        timeout: std::time::Duration::from_secs(10),
        result_expression: None,
        effective: &effective,
        repeat_extra: &[],
    };
    finish_observed(&job, authorize, member.observed.clone()).unwrap();
}

fn phase(group: &ParallelGroup, member: &Member) -> String {
    group.fixture.db.read().unwrap().query_row(
        "SELECT phase FROM bpmn_service_invocations WHERE job_id=?1",
        [&member.claim.job.job_id], |row| row.get(0)).unwrap()
}

fn incident_codes(group: &ParallelGroup) -> Vec<String> {
    group.snapshot().incidents.iter().map(|row| row.code.clone()).collect()
}

#[tokio::test]
async fn a_result_the_validator_refuses_raises_an_incident_and_stays_acceptable() {
    let group = two_completed_ordinals().await;
    let member = &group.members[0];
    let changed = group.fixture.db.write().unwrap().execute(
        "UPDATE bpmn_repetition_occurrences SET input_variables_json='{\"tampered\":true}' WHERE instance_id=?1 AND ordinal=0",
        [&group.instance_id]).unwrap();
    assert_eq!(changed, 1);
    let snapshot = group.snapshot();
    let refusal = repository::accept_job_result(&group.fixture.db, &group.fixture.owner,
        &member.claim.job.job_id, member.claim.job.attempt, member.claim.job.fence,
        &member.worker, &member.observed, snapshot.instance.revision,
        ProcessPlanInput::Canonical, chrono::Utc::now().timestamp_millis()).unwrap_err();
    assert!(repository::is_result_refusal(&refusal), "{refusal:#}");

    publish(&group, member, &|| AccessCheck::Allowed);
    let snapshot = group.snapshot();
    assert_eq!(incident_codes(&group), ["RESULT_RECOVERY_FAILED"]);
    assert_eq!(snapshot.incidents[0].job_id.as_deref(), Some(member.claim.job.job_id.as_str()));
    assert_eq!(snapshot.instance.status, ProcessInstanceStatus::Incident);
    assert_eq!(phase(&group, member), "observed", "the observed result stays recorded");

    group.fixture.db.write().unwrap().execute(
        "UPDATE bpmn_repetition_occurrences SET input_variables_json=(SELECT input_variables_json FROM bpmn_repetition_occurrences WHERE instance_id=?1 AND ordinal=1) WHERE instance_id=?1 AND ordinal=0",
        [&group.instance_id]).unwrap();
    group.accept(0).unwrap();
    let snapshot = group.snapshot();
    assert!(snapshot.incidents.is_empty(), "accepting the result closes the incident");
    assert_eq!(snapshot.repetition_occurrences.iter().filter(|row|
        row.status == ProcessRepetitionOccurrenceStatus::Completed).count(), 1);
}

#[tokio::test]
async fn an_acceptance_another_worker_holds_raises_no_incident() {
    let group = two_completed_ordinals().await;
    let member = &group.members[0];
    let held = group.fixture.db.write().unwrap().execute(
        "UPDATE bpmn_service_invocations SET acceptance_owner_id='recovery-pass',acceptance_lease_until_ms=?2,acceptance_revision=acceptance_revision+1,phase_revision=phase_revision+1 WHERE job_id=?1",
        rusqlite::params![member.claim.job.job_id, chrono::Utc::now().timestamp_millis() + 600_000]).unwrap();
    assert_eq!(held, 1);
    let polls = AtomicUsize::new(0);
    publish(&group, member, &|| { polls.fetch_add(1, Ordering::SeqCst); AccessCheck::Allowed });
    assert_eq!(polls.load(Ordering::SeqCst), 1, "a held acceptance is left to its owner, not retried");
    assert_eq!(incident_codes(&group), Vec::<String>::new());
    assert_eq!(group.snapshot().instance.status, ProcessInstanceStatus::Running);
    assert_eq!(phase(&group, member), "observed");
}

#[tokio::test]
async fn a_result_that_keeps_losing_its_race_shows_as_an_incident() {
    let group = two_completed_ordinals().await;
    let member = &group.members[0];
    let polls = AtomicUsize::new(0);
    let bump = || {
        polls.fetch_add(1, Ordering::SeqCst);
        group.fixture.db.write().unwrap().execute(
            "UPDATE bpmn_instances SET revision=revision+1 WHERE instance_id=?1",
            [&group.instance_id]).unwrap();
        AccessCheck::Allowed
    };
    publish(&group, member, &bump);
    assert_eq!(polls.load(Ordering::SeqCst), 8, "every attempt lost the race");
    assert_eq!(incident_codes(&group), ["RESULT_RECOVERY_FAILED"]);
    assert_eq!(phase(&group, member), "observed");
}

#[tokio::test]
async fn a_result_the_engine_cannot_plan_raises_an_incident() {
    let group = two_completed_ordinals().await;
    let member = &group.members[0];
    group.fixture.db.write().unwrap().execute(
        "UPDATE bpmn_repetition_groups SET status='cancelled' WHERE instance_id=?1",
        [&group.instance_id]).unwrap();
    let snapshot = group.snapshot();
    let job = snapshot.jobs.iter().find(|job| job.job_id == member.claim.job.job_id).unwrap();
    let error = super::super::runtime::plan_job_result(&snapshot, job, &member.observed,
        chrono::Utc::now().timestamp_millis(), None).unwrap_err();
    assert!(!repository::is_result_refusal(&error), "the planner's own error is not typed");

    publish(&group, member, &|| AccessCheck::Allowed);
    assert_eq!(incident_codes(&group), ["RESULT_RECOVERY_FAILED"]);
    assert_eq!(phase(&group, member), "observed");
}

#[tokio::test]
async fn two_siblings_publishing_at_the_same_time_both_land_under_one_group_incident() {
    let group = ParallelGroup::start(
        [service_result("Error", Some("CODE_A"), 1), service_result("Error", Some("CODE_B"), 2)],
        condition_true()).await;
    // Both workers read the same snapshot before either accepts, so one of them
    // loses its revision race and must plan again from the other's commit.
    let barrier = std::sync::Barrier::new(2);
    let first_look = [std::sync::Once::new(), std::sync::Once::new()];
    let polls = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for (index, member) in group.members.iter().enumerate() {
            let (group, barrier, first_look, polls) = (&group, &barrier, &first_look, &polls);
            scope.spawn(move || publish(group, member, &|| {
                polls.fetch_add(1, Ordering::SeqCst);
                first_look[index].call_once(|| { barrier.wait(); });
                AccessCheck::Allowed
            }));
        }
    });
    assert_eq!(polls.load(Ordering::SeqCst), 3, "exactly one worker planned a second time");
    let snapshot = group.snapshot();
    let mut codes = incident_codes(&group);
    codes.sort();
    assert_eq!(codes, ["CODE_A", "CODE_B"]);
    assert_eq!(snapshot.instance.status, ProcessInstanceStatus::Incident);
    assert!(snapshot.repetition_occurrences.iter().all(|row|
        row.status == ProcessRepetitionOccurrenceStatus::AcceptedBlocked));
    let named = snapshot.repetition_groups[0].terminal_incident_id.as_deref().unwrap();
    assert!(snapshot.incidents.iter().any(|incident| incident.incident_id == named),
        "the group names one of its two open incidents");
    for member in &group.members {
        assert_eq!(phase(&group, member), "accepted");
    }
}

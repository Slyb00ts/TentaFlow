// ============ File: acceptance_failure_tests.rs — what an observed result does when its acceptance fails ============

use std::sync::atomic::{AtomicUsize, Ordering};

use tentaflow_protocol::processes::{
    ActivityVerification, ProcessInstanceStatus, ProcessRepetitionOccurrenceStatus,
};

use super::super::repetition_group_incident_tests::{service_result, Member, ParallelGroup};
use super::super::repository::{self, ProcessPlanInput};
use crate::db::READ_POOL_ACQUIRE_TIMEOUT;
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
        verification: &tentaflow_protocol::processes::ActivityVerification::Human,
        implementation: &tentaflow_protocol::processes::ServiceImplementation::Flow,
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

    let polls = AtomicUsize::new(0);
    publish(&group, member, &|| { polls.fetch_add(1, Ordering::SeqCst); AccessCheck::Allowed });
    // The planner's refusal is permanent, so it is planned once and not eight times; the
    // incident is the same one an exhausted race raises (the observed result stays
    // recorded either way and recovery is the same), so the attempts tell them apart.
    assert_eq!(polls.load(Ordering::SeqCst), 1, "a result the engine cannot plan is not retried");
    assert_eq!(incident_codes(&group), ["RESULT_RECOVERY_FAILED"]);
    assert_eq!(phase(&group, member), "observed");
}

#[tokio::test]
async fn an_untyped_permanent_acceptance_failure_is_not_retried() {
    let group = two_completed_ordinals().await;
    let member = &group.members[0];
    let polls = AtomicUsize::new(0);
    let sever = || {
        polls.fetch_add(1, Ordering::SeqCst);
        // An undecided-result incident that recovery does not own stays open across the
        // acceptance: a fact of the stored state that planning again cannot change.
        group.fixture.db.write().unwrap().execute(
            "INSERT OR IGNORE INTO bpmn_incidents(incident_id,instance_id,scope_id,node_id,job_id,code,message,at_ms) SELECT 'undecided-incident',instance_id,scope_id,node_id,job_id,'TOOL_RESULT_UNDECIDED','left by another pass',0 FROM bpmn_jobs WHERE job_id=?1",
            [&member.claim.job.job_id]).unwrap();
        AccessCheck::Allowed
    };
    publish(&group, member, &sever);
    assert_eq!(polls.load(Ordering::SeqCst), 1, "a permanent failure is not retried eight times");
    assert_eq!(incident_codes(&group), ["TOOL_RESULT_UNDECIDED"],
        "the failure is already visible, so no second incident is raised");
    assert_eq!(phase(&group, member), "observed", "the result stays recorded");
    let failed_passes: i64 = group.fixture.db.read().unwrap().query_row(
        "SELECT failed_passes FROM bpmn_result_recovery WHERE invocation_id=(SELECT invocation_id FROM bpmn_service_invocations WHERE job_id=?1)",
        [&member.claim.job.job_id], |row| row.get(0)).unwrap();
    assert_eq!(failed_passes, 1, "the failure is counted once, as a failed acceptance");
}

#[tokio::test]
async fn a_revoked_authority_blocks_the_observed_result_instead_of_retrying() {
    let group = two_completed_ordinals().await;
    let member = &group.members[0];
    let polls = AtomicUsize::new(0);
    let revoke = || {
        polls.fetch_add(1, Ordering::SeqCst);
        // The actor still reads the instance, but no longer owns its definition.
        let conn = group.fixture.db.write().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO user_accounts(id,username,password_hash,is_active) VALUES('another-owner','another-owner','x',1)",
            []).unwrap();
        conn.execute(
            "UPDATE bpmn_definitions SET owner_user_id='another-owner' WHERE definition_id=(SELECT definition_id FROM bpmn_instances WHERE instance_id=?1)",
            [&group.instance_id]).unwrap();
        AccessCheck::Allowed
    };
    publish(&group, member, &revoke);
    assert_eq!(polls.load(Ordering::SeqCst), 1, "a revoked authority is not retried eight times");
    assert_ne!(phase(&group, member), "observed", "the result is blocked, not left hanging");
}

#[test]
fn only_a_lost_race_or_a_busy_store_is_worth_another_acceptance() {
    use rusqlite::ffi::{Error as SqliteError, ErrorCode};
    let sqlite = |code: ErrorCode| anyhow::Error::new(rusqlite::Error::SqliteFailure(
        SqliteError { code, extended_code: 0 }, None)).context("acceptance");
    for retry in [ErrorCode::DatabaseBusy, ErrorCode::DatabaseLocked] {
        assert!(repository::is_retryable_acceptance(&sqlite(retry)), "{retry:?}");
    }
    for stop in [ErrorCode::ReadOnly, ErrorCode::DatabaseCorrupt, ErrorCode::DiskFull,
        ErrorCode::CannotOpen, ErrorCode::ConstraintViolation] {
        assert!(!repository::is_retryable_acceptance(&sqlite(stop)), "{stop:?}");
    }
    assert!(repository::is_retryable_acceptance(&anyhow::Error::new(
        repository::RevisionConflict("moved on"))));
    assert!(!repository::is_retryable_acceptance(&anyhow::anyhow!(
        "Service acceptance lacks its immutable original observation")));
}

#[test]
fn a_failing_store_is_never_a_verdict_on_the_plan() {
    use rusqlite::ffi::{Error as SqliteError, ErrorCode};
    let sqlite = |code: ErrorCode| anyhow::Error::new(rusqlite::Error::SqliteFailure(
        SqliteError { code, extended_code: 0 }, None));
    for failing in [ErrorCode::DatabaseBusy, ErrorCode::DiskFull, ErrorCode::ReadOnly,
        ErrorCode::CannotOpen, ErrorCode::DatabaseCorrupt] {
        assert!(!repository::is_result_refusal(&repository::refuse_result(sqlite(failing))),
            "{failing:?}");
    }
    assert!(repository::is_result_refusal(&repository::refuse_result(
        sqlite(ErrorCode::ConstraintViolation))), "a plan the schema rejects is a refusal");
}

#[tokio::test]
async fn a_busy_store_is_waited_out_and_the_result_is_accepted_on_the_next_attempt() {
    let group = two_completed_ordinals().await;
    let member = &group.members[0];
    let path = group.fixture.directory.path().join("processes.db");
    let holder = std::sync::Mutex::new(None);
    let polls = AtomicUsize::new(0);
    let contend = || {
        if polls.fetch_add(1, Ordering::SeqCst) == 0 {
            let other = rusqlite::Connection::open(&path).unwrap();
            other.execute_batch("BEGIN IMMEDIATE").unwrap();
            *holder.lock().unwrap() = Some(other);
        } else {
            holder.lock().unwrap().take();
        }
        AccessCheck::Allowed
    };
    publish(&group, member, &contend);
    assert_eq!(polls.load(Ordering::SeqCst), 2, "the busy attempt is made again exactly once");
    assert_eq!(phase(&group, member), "accepted");
    assert_eq!(incident_codes(&group), Vec::<String>::new());
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

/// Lost race on the first look (so the worker reads the snapshot again) plus whatever
/// `disturb` does to the store before that second read.
fn lose_the_race_then<'a>(group: &'a ParallelGroup, disturb: impl Fn() + 'a) -> impl Fn() -> AccessCheck + 'a {
    let first = std::sync::Once::new();
    let disturb_once = move || first.call_once(|| {
        group.fixture.db.write().unwrap().execute(
            "UPDATE bpmn_instances SET revision=revision+1 WHERE instance_id=?1",
            [&group.instance_id]).unwrap();
        disturb();
    });
    move || { disturb_once(); AccessCheck::Allowed }
}

/// Publishes `member` while every read connection is taken for `hold`. The first
/// look loses its revision race and `damage` runs, so the acceptance that follows
/// fails on the empty pool and the NEXT snapshot read is the one that meets the
/// pool (when `hold` outlasts the 5 s acquisition timeout) and the damage.
fn publish_with_read_pool_held(
    group: &ParallelGroup, member: &Member, hold: std::time::Duration,
    damage: impl Fn() + Send + Sync,
) {
    let (start_tx, start_rx) = std::sync::mpsc::channel::<()>();
    let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
    let (start_tx, held_rx) = (std::sync::Mutex::new(start_tx), std::sync::Mutex::new(held_rx));
    let exhaust = || {
        damage();
        start_tx.lock().unwrap().send(()).unwrap();
        held_rx.lock().unwrap().recv().unwrap();
    };
    let authorize = lose_the_race_then(group, exhaust);
    std::thread::scope(|scope| {
        let db = group.fixture.db.clone();
        scope.spawn(move || {
            start_rx.recv().unwrap();
            let guards = (0..crate::db::read_pool_size()).map(|_| db.read().unwrap()).collect::<Vec<_>>();
            held_tx.send(()).unwrap();
            std::thread::sleep(hold);
            drop(guards);
        });
        publish(group, member, &authorize);
    });
}

fn open_incident_codes(group: &ParallelGroup) -> Vec<String> {
    let conn = group.fixture.db.read().unwrap();
    let mut statement = conn.prepare(
        "SELECT code FROM bpmn_incidents WHERE instance_id=?1 AND resolved_at_ms IS NULL").unwrap();
    statement.query_map([&group.instance_id], |row| row.get(0)).unwrap()
        .collect::<rusqlite::Result<_>>().unwrap()
}

#[tokio::test]
async fn a_snapshot_read_that_fails_without_a_denial_leaves_the_result_acceptable() {
    let group = two_completed_ordinals().await;
    let member = &group.members[0];
    let corrupt = || {
        let conn = group.fixture.db.write().unwrap();
        conn.execute_batch("PRAGMA ignore_check_constraints=ON").unwrap();
        conn.execute("UPDATE bpmn_repetition_groups SET status='bogus' WHERE instance_id=?1",
            [&group.instance_id]).unwrap();
        conn.execute_batch("PRAGMA ignore_check_constraints=OFF").unwrap();
    };
    publish_with_read_pool_held(&group, member, READ_POOL_ACQUIRE_TIMEOUT + std::time::Duration::from_secs(2), corrupt);
    assert_eq!(phase(&group, member), "observed", "an unreadable snapshot is not a revocation");
    assert_eq!(open_incident_codes(&group), ["RESULT_RECOVERY_FAILED"]);
}

#[tokio::test]
async fn a_snapshot_read_the_actor_is_denied_blocks_the_result() {
    let group = two_completed_ordinals().await;
    let member = &group.members[0];
    let deactivate = || {
        group.fixture.db.write().unwrap().execute(
            "UPDATE user_accounts SET is_active=0 WHERE id=?1",
            [&group.fixture.owner.user_id]).unwrap();
    };
    publish_with_read_pool_held(&group, member, READ_POOL_ACQUIRE_TIMEOUT + std::time::Duration::from_secs(2), deactivate);
    assert_eq!(phase(&group, member), "observed_blocked");
}

#[tokio::test]
async fn an_exhausted_read_pool_is_waited_out_instead_of_revoking_the_result() {
    let group = two_completed_ordinals().await;
    let member = &group.members[0];
    publish_with_read_pool_held(&group, member, READ_POOL_ACQUIRE_TIMEOUT * 2 + std::time::Duration::from_secs(2), || {});
    assert_eq!(phase(&group, member), "accepted");
    assert_eq!(open_incident_codes(&group), Vec::<String>::new());
}

#[tokio::test]
async fn a_recorded_flow_result_is_accepted_by_the_periodic_pass_once_its_cause_is_gone() {
    let group = two_completed_ordinals().await;
    let member = &group.members[0];
    let original: String = group.fixture.db.read().unwrap().query_row(
        "SELECT input_variables_json FROM bpmn_repetition_occurrences WHERE instance_id=?1 AND ordinal=0",
        [&group.instance_id], |row| row.get(0)).unwrap();
    group.fixture.db.write().unwrap().execute(
        "UPDATE bpmn_repetition_occurrences SET input_variables_json='{\"tampered\":true}' WHERE instance_id=?1 AND ordinal=0",
        [&group.instance_id]).unwrap();
    publish(&group, member, &|| AccessCheck::Allowed);
    assert_eq!(incident_codes(&group), ["RESULT_RECOVERY_FAILED"]);

    let pass = |at_ms: i64| repository::recover_deferred_results(&group.fixture.db, at_ms,
        &repository::no_tool_authority, &|_| false, repository::RECOVERY_PAGE).unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    assert_eq!(pass(now), 0);
    let later = now + repository::OPEN_INCIDENT_RETRY_AFTER_MS + 1_000;
    assert_eq!(pass(later), 0, "the cause is still there");
    assert_eq!(incident_codes(&group), ["RESULT_RECOVERY_FAILED"], "no second incident");
    assert_eq!(phase(&group, member), "observed");

    group.fixture.db.write().unwrap().execute(
        "UPDATE bpmn_repetition_occurrences SET input_variables_json=?2 WHERE instance_id=?1 AND ordinal=0",
        rusqlite::params![group.instance_id, original]).unwrap();
    assert_eq!(pass(later + 1), 0, "a result tried a moment ago waits for its backoff");
    assert_eq!(phase(&group, member), "observed");
    // The failed retry above doubled the delay.
    assert_eq!(pass(later + 2 * repository::OPEN_INCIDENT_RETRY_AFTER_MS + 1_000), 1);
    assert_eq!(phase(&group, member), "accepted");
    assert!(group.snapshot().incidents.is_empty(), "accepting the result closes the incident");
}

fn recovery_state(group: &ParallelGroup, member: &Member) -> (i64, i64) {
    group.fixture.db.read().unwrap().query_row(
        "SELECT last_checked_ms,failed_passes FROM bpmn_result_recovery WHERE invocation_id=(SELECT invocation_id FROM bpmn_service_invocations WHERE job_id=?1)",
        [&member.claim.job.job_id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap()
}

#[tokio::test]
async fn a_result_refused_for_good_is_retried_with_a_doubling_delay_up_to_an_hour() {
    let group = two_completed_ordinals().await;
    let member = &group.members[0];
    group.fixture.db.write().unwrap().execute(
        "UPDATE bpmn_repetition_occurrences SET input_variables_json='{\"tampered\":true}' WHERE instance_id=?1 AND ordinal=0",
        [&group.instance_id]).unwrap();
    publish(&group, member, &|| AccessCheck::Allowed);
    assert_eq!(incident_codes(&group), ["RESULT_RECOVERY_FAILED"]);

    const BASE: i64 = repository::OPEN_INCIDENT_RETRY_AFTER_MS;
    const CAP: i64 = repository::OPEN_INCIDENT_RETRY_MAX_MS;
    let from = 1_000_000_000_000_i64;
    let pass = |at_ms: i64| repository::recover_deferred_results(&group.fixture.db, at_ms,
        &repository::no_tool_authority, &|_| false, repository::RECOVERY_PAGE).unwrap();
    for (failed_passes, delay) in [(1, BASE), (2, 2 * BASE), (3, 4 * BASE), (6, 32 * BASE),
        (7, CAP), (8, CAP), (60, CAP)] {
        group.fixture.db.write().unwrap().execute(
            "UPDATE bpmn_result_recovery SET failed_passes=?2,last_checked_ms=?3 WHERE invocation_id=(SELECT invocation_id FROM bpmn_service_invocations WHERE job_id=?1)",
            rusqlite::params![member.claim.job.job_id, failed_passes, from]).unwrap();
        pass(from + delay - 1);
        assert_eq!(recovery_state(&group, member), (from, failed_passes),
            "{failed_passes} failed passes: tried {delay} ms early");
        pass(from + delay);
        assert_eq!(recovery_state(&group, member).1, failed_passes + 1,
            "{failed_passes} failed passes: not tried once {delay} ms passed");
    }
}

#[tokio::test]
async fn an_authority_read_that_fails_without_a_denial_leaves_the_result_acceptable() {
    let group = two_completed_ordinals().await;
    let member = &group.members[0];
    // The authority check reads the pinned version; an unreadable one says nothing
    // about the actor.
    let damaged = group.fixture.db.write().unwrap().execute(
        "UPDATE bpmn_versions SET service_snapshots_json='not json' WHERE definition_id=(SELECT definition_id FROM bpmn_instances WHERE instance_id=?1)",
        [&group.instance_id]).unwrap();
    assert_eq!(damaged, 1);
    publish(&group, member, &|| AccessCheck::Allowed);
    assert_eq!(phase(&group, member), "observed", "an unreadable row is not a revocation");
    assert_eq!(open_incident_codes(&group), ["RESULT_RECOVERY_FAILED"]);
}

#[tokio::test]
async fn an_ancestry_that_names_another_actor_is_a_typed_denial() {
    let group = two_completed_ordinals().await;
    let other = repository::ProcessActor {
        org_id: group.fixture.owner.org_id.clone(), user_id: "someone-else".into() };
    let conn = group.fixture.db.read().unwrap();
    let error = repository::require_call_control_authority_on(&conn, &other, &group.instance_id)
        .unwrap_err();
    assert!(repository::is_authority_denied(&error), "{error:#}");
}

#[test]
fn an_exhausted_attempt_budget_names_what_it_ran_out_against() {
    assert_eq!(super::exhausted_attempts_failure(true).0, "STORE_BUSY");
    assert_eq!(super::exhausted_attempts_failure(false).0, "REVISION_CONFLICT");
}

#[tokio::test]
async fn a_busy_store_failure_keeps_the_result_acceptable_and_shows_as_an_incident() {
    let group = two_completed_ordinals().await;
    let member = &group.members[0];
    let (code, message) = super::exhausted_attempts_failure(true);
    super::fail_claim(&group.fixture.db, &member.worker, &member.claim, code, message,
        Some(&member.observed)).unwrap();
    assert_eq!(phase(&group, member), "observed");
    assert_eq!(open_incident_codes(&group), ["RESULT_RECOVERY_FAILED"]);
}

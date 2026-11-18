// ============ File: retry_policy_tests.rs — BPMN B4.6 retry policy per activity: validation, schedule, automatic retries, one incident, restart ============

use rusqlite::params;
use tentaflow_protocol::processes::{
    ActivityVerification, ProcessInstance, ProcessInstanceStatus, ProcessNodeKind,
    ProcessRetryBackoff, ProcessRetryPolicy,
};

use super::model::{retry_delay_seconds, validate_model};
use super::repository::{self, ClaimedProcessJob};
use super::runtime::test_support::*;
use crate::db::DbPool;

fn fixed(max_attempts: u8, delay_seconds: u32) -> ProcessRetryPolicy {
    ProcessRetryPolicy {
        max_attempts,
        delay_seconds,
        backoff: ProcessRetryBackoff::Fixed,
    }
}

fn growing(
    max_attempts: u8,
    delay_seconds: u32,
    factor_percent: u32,
    max_delay_seconds: u32,
) -> ProcessRetryPolicy {
    ProcessRetryPolicy {
        max_attempts,
        delay_seconds,
        backoff: ProcessRetryBackoff::Exponential {
            factor_percent,
            max_delay_seconds,
        },
    }
}

fn model_with(policy: Option<ProcessRetryPolicy>) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = service_model("flow-1", ActivityVerification::Human);
    for node in &mut model.nodes {
        if let ProcessNodeKind::ServiceTask { retry, .. } = &mut node.kind {
            *retry = policy.clone();
        }
    }
    model
}

// ----- the model: limits and bytes -----

#[test]
fn the_policy_is_validated_against_the_limits_the_editor_offers() {
    assert!(validate_model(&model_with(None)).is_ok());
    for ok in [
        fixed(1, 1),
        fixed(10, 86_400),
        growing(3, 5, 101, 5),
        growing(10, 1, 1000, 86_400),
    ] {
        validate_model(&model_with(Some(ok.clone())))
            .unwrap_or_else(|error| panic!("{ok:?}: {error}"));
    }
    for (bad, why) in [
        (fixed(0, 30), "no attempts is no policy, not a policy"),
        (fixed(11, 30), "more attempts than a step may have"),
        (fixed(3, 0), "a zero delay would busy-loop a failing task"),
        (fixed(3, 86_401), "a delay beyond a day"),
        (
            growing(3, 30, 100, 600),
            "a factor of 100 percent is the fixed pattern",
        ),
        (growing(3, 30, 1001, 600), "a factor beyond 1000 percent"),
        (growing(3, 30, 200, 29), "a cap below the first wait"),
        (growing(3, 30, 200, 86_401), "a cap beyond a day"),
    ] {
        assert!(
            validate_model(&model_with(Some(bad.clone()))).is_err(),
            "{bad:?}: {why}"
        );
    }
}

#[test]
fn a_step_without_a_policy_keeps_its_bytes_and_a_policy_round_trips() {
    let plain = serde_json::to_string(&model_with(None)).unwrap();
    assert!(
        !plain.contains("retry"),
        "an absent policy is not written: {plain}"
    );
    let with = serde_json::to_string(&model_with(Some(growing(4, 10, 150, 300)))).unwrap();
    assert!(with.contains(r#""retry":{"max_attempts":4,"delay_seconds":10,"backoff":{"Exponential":{"factor_percent":150,"max_delay_seconds":300}}}"#), "{with}");
    let back: tentaflow_protocol::processes::ProcessModel = serde_json::from_str(&with).unwrap();
    assert_eq!(serde_json::to_string(&back).unwrap(), with);
    let old: tentaflow_protocol::processes::ProcessModel = serde_json::from_str(&plain).unwrap();
    assert_eq!(serde_json::to_string(&old).unwrap(), plain);
}

#[test]
fn the_schedule_is_fixed_or_grows_by_the_factor_up_to_the_cap() {
    let schedule = |policy: &ProcessRetryPolicy| -> Vec<u32> {
        (1..=u32::from(policy.max_attempts))
            .map(|n| retry_delay_seconds(policy, n))
            .collect()
    };
    assert_eq!(schedule(&fixed(4, 45)), [45, 45, 45, 45]);
    assert_eq!(
        schedule(&growing(7, 30, 200, 600)),
        [30, 60, 120, 240, 480, 600, 600]
    );
    assert_eq!(schedule(&growing(4, 100, 150, 1000)), [100, 150, 225, 338]);
    // A wait is rounded up, so a small delay still grows: truncation would hold 1 s at 1 s.
    assert_eq!(schedule(&growing(5, 1, 150, 100)), [1, 2, 3, 5, 8]);
    assert_eq!(schedule(&growing(4, 1, 101, 10)), [1, 2, 3, 4]);
    assert_eq!(schedule(&growing(4, 2, 110, 100)), [2, 3, 4, 5]);
    // The largest factor on the largest delay saturates at the cap instead of overflowing.
    assert_eq!(schedule(&growing(10, 86_400, 1000, 86_400)), [86_400; 10]);
}

// ----- the engine -----

fn count(db: &DbPool, sql: &str) -> i64 {
    db.read()
        .unwrap()
        .query_row(sql, [], |row| row.get(0))
        .unwrap()
}

/// `(rows, open, attempt_count, attempt_limit, next_attempt_at_ms)` of the instance's incidents.
fn incident_state(db: &DbPool) -> (i64, i64, i64, Option<i64>, Option<i64>) {
    let conn = db.read().unwrap();
    let (rows, open): (i64, i64) = conn
        .query_row(
            "SELECT COUNT(*),COALESCE(SUM(resolved_at_ms IS NULL),0) FROM bpmn_incidents",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let (made, limit, next): (i64, Option<i64>, Option<i64>) = conn
        .query_row("SELECT attempt_count,attempt_limit,next_attempt_at_ms FROM bpmn_incidents ORDER BY at_ms LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap();
    (rows, open, made, limit, next)
}

fn events(db: &DbPool, kind: &str) -> i64 {
    db.read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM bpmn_events WHERE kind=?1",
            [kind],
            |r| r.get(0),
        )
        .unwrap()
}

fn notices(db: &DbPool) -> i64 {
    count(db, "SELECT COUNT(*) FROM bpmn_incident_notices")
}

fn started(fixture: &Fixture, policy: Option<ProcessRetryPolicy>) -> ProcessInstance {
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("policy", None));
    let mut model = model_with(policy);
    for node in &mut model.nodes {
        if let ProcessNodeKind::ServiceTask {
            flow_id: slot,
            verification,
            ..
        } = &mut node.kind
        {
            *slot = flow_id.clone();
            *verification = ActivityVerification::Condition {
                expression: "true".into(),
            };
        }
    }
    start_model(fixture, &model)
}

fn claim(db: &DbPool, worker: &str, at_ms: i64) -> ClaimedProcessJob {
    repository::claim_job(db, worker, at_ms)
        .unwrap()
        .expect("a queued job")
}

/// A worker that stopped before the outside call: the failure is proven to have had no effect.
fn interrupt(db: &DbPool, claimed: &ClaimedProcessJob, worker: &str, at_ms: i64) {
    assert!(repository::fail_job(
        db,
        &claimed.job.job_id,
        claimed.job.attempt,
        claimed.job.fence,
        worker,
        "INTERRUPTED",
        "controlled interruption",
        at_ms,
        None
    )
    .unwrap());
}

fn fail_next_attempt(db: &DbPool, worker: &str, at_ms: i64) {
    let claimed = claim(db, worker, at_ms);
    interrupt(db, &claimed, worker, at_ms);
}

#[tokio::test]
async fn a_failure_with_a_policy_is_repeated_by_the_engine_and_stays_one_incident_until_it_runs_out(
) {
    let fixture = Fixture::new();
    let db = &fixture.db;
    let instance = started(&fixture, Some(fixed(2, 30)));
    let t0 = super::jobs::now_ms();
    fail_next_attempt(db, "w", t0);

    // Armed, not yet told to anyone: the failure may still mend itself.
    assert_eq!(incident_state(db), (1, 1, 0, Some(2), Some(t0 + 30_000)));
    assert_eq!(notices(db), 0, "no notice while a repeat is waiting");
    assert_eq!(events(db, "job_retry_scheduled"), 1);
    let shown = repository::get_instance(db, &fixture.owner, &instance.instance_id, None).unwrap();
    assert_eq!(shown.status, ProcessInstanceStatus::Incident);
    let attempts = shown.incidents[0].attempts.expect("attempts are shown");
    assert_eq!(
        (attempts.made, attempts.limit, attempts.next_attempt_at_ms),
        (0, 2, Some(t0 + 30_000))
    );

    assert_eq!(
        repository::drain_automatic_retries(db, t0 + 29_999, 32).unwrap(),
        0,
        "not yet due"
    );
    assert_eq!(
        repository::drain_automatic_retries(db, t0 + 30_000, 32).unwrap(),
        1
    );
    assert_eq!(
        incident_state(db),
        (1, 0, 1, Some(2), None),
        "the same row, closed while the repeat runs"
    );

    // The repeat fails again: the same incident opens again, with the next wait.
    let t1 = t0 + 30_001;
    fail_next_attempt(db, "w", t1);
    assert_eq!(incident_state(db), (1, 1, 1, Some(2), Some(t1 + 30_000)));
    assert_eq!(notices(db), 0, "still no notice");
    assert_eq!(
        repository::drain_automatic_retries(db, t1 + 30_000, 32).unwrap(),
        1
    );

    // The last repeat fails: nothing is left, and the person is told once.
    let t2 = t1 + 30_001;
    fail_next_attempt(db, "w", t2);
    assert_eq!(incident_state(db), (1, 1, 2, Some(2), None));
    assert_eq!(notices(db), 1, "one notice for the whole failure");
    assert_eq!(events(db, "job_retries_exhausted"), 1);
    assert_eq!(
        events(db, "job_retry_scheduled"),
        2,
        "one per repeat that was planned"
    );
    assert_eq!(
        repository::drain_automatic_retries(db, t2 + 10_000_000, 32).unwrap(),
        0
    );

    let shown = repository::get_instance(db, &fixture.owner, &instance.instance_id, None).unwrap();
    let incident = &shown.incidents[0];
    assert!(incident.can_retry, "a person can still retry by hand");
    let attempts = incident.attempts.unwrap();
    assert_eq!(
        (attempts.made, attempts.limit, attempts.next_attempt_at_ms),
        (2, 2, None)
    );

    // A manual retry counts, runs, and a failure after it neither multiplies rows nor notices.
    let job_id = incident.job_id.clone().unwrap();
    repository::retry_job(
        db,
        &fixture.owner,
        &stamp("manual"),
        &instance.instance_id,
        &job_id,
        shown.revision,
    )
    .unwrap();
    assert_eq!(incident_state(db), (1, 0, 3, Some(2), None));
    fail_next_attempt(db, "w", t2 + 1);
    assert_eq!(
        incident_state(db),
        (1, 1, 3, Some(2), None),
        "the budget was spent: nothing is armed again"
    );
    assert_eq!(notices(db), 1);
    assert_eq!(
        events(db, "job_retries_exhausted"),
        1,
        "reopening a spent incident ends nothing, so it says nothing again"
    );
    assert_eq!(events(db, "job_retry_stopped"), 0);
    assert_eq!(
        repository::drain_automatic_retries(db, t2 + 10_000_000, 32).unwrap(),
        0
    );
}

#[tokio::test]
async fn a_retry_that_came_due_while_the_node_was_down_runs_once_after_a_restart() {
    let fixture = Fixture::new();
    let _instance = started(&fixture, Some(growing(3, 20, 200, 600)));
    let t0 = super::jobs::now_ms();
    fail_next_attempt(&fixture.db, "w", t0);
    assert_eq!(incident_state(&fixture.db).4, Some(t0 + 20_000));

    let path = fixture.directory.path().join("processes.db");
    let Fixture {
        directory,
        db,
        router,
        ..
    } = fixture;
    drop(router);
    drop(db);
    let reopened = crate::db::init(&path).expect("reopen the durable process database");
    assert_eq!(
        incident_state(&reopened).4,
        Some(t0 + 20_000),
        "the deadline is on the row"
    );
    assert_eq!(
        repository::drain_automatic_retries(&reopened, t0 + 3_600_000, 32).unwrap(),
        1,
        "a deadline long past runs the repeat"
    );
    assert_eq!(
        repository::drain_automatic_retries(&reopened, t0 + 3_600_000, 32).unwrap(),
        0,
        "and does not run it a second time"
    );
    assert_eq!(incident_state(&reopened), (1, 0, 1, Some(3), None));
    assert_eq!(
        count(
            &reopened,
            "SELECT COUNT(*) FROM bpmn_jobs WHERE status='queued'"
        ),
        1
    );

    // The second wait of the growing schedule is 40 s, counted from the next failure.
    let t1 = t0 + 3_600_001;
    fail_next_attempt(&reopened, "w", t1);
    assert_eq!(incident_state(&reopened).4, Some(t1 + 40_000));
    drop(directory);
}

#[tokio::test]
async fn an_uncertain_outcome_is_never_repeated_by_the_engine() {
    let fixture = Fixture::new();
    let db = &fixture.db;
    let instance = started(&fixture, Some(fixed(5, 1)));
    let t0 = super::jobs::now_ms();
    let claimed = claim(db, "w", t0);
    assert!(repository::commit_job_dispatch_boundary(db, &claimed, "w", t0).unwrap());
    interrupt(db, &claimed, "w", t0 + 1);
    let shown = repository::get_instance(db, &fixture.owner, &instance.instance_id, None).unwrap();
    assert_eq!(shown.incidents[0].code, "EXTERNAL_OUTCOME_UNCERTAIN");
    assert_eq!(
        incident_state(db),
        (1, 1, 0, None, None),
        "nothing is armed"
    );
    assert!(shown.incidents[0].attempts.is_none());
    assert_eq!(
        notices(db),
        1,
        "a failure the engine will not repeat is told at once"
    );
    assert_eq!(
        repository::drain_automatic_retries(db, t0 + 100_000_000, 32).unwrap(),
        0
    );
}

#[tokio::test]
async fn a_step_without_a_policy_behaves_as_before() {
    let fixture = Fixture::new();
    let db = &fixture.db;
    let _instance = started(&fixture, None);
    let t0 = super::jobs::now_ms();
    fail_next_attempt(db, "w", t0);
    assert_eq!(incident_state(db), (1, 1, 0, None, None));
    assert_eq!(notices(db), 1);
    assert_eq!(events(db, "job_retry_scheduled"), 0);
    assert_eq!(
        repository::drain_automatic_retries(db, t0 + 100_000_000, 32).unwrap(),
        0
    );
}

#[tokio::test]
async fn a_person_retrying_a_waiting_incident_takes_its_place_and_the_count_carries_over() {
    let fixture = Fixture::new();
    let db = &fixture.db;
    let instance = started(&fixture, Some(fixed(2, 60)));
    let t0 = super::jobs::now_ms();
    fail_next_attempt(db, "w", t0);
    let shown = repository::get_instance(db, &fixture.owner, &instance.instance_id, None).unwrap();
    let job_id = shown.incidents[0].job_id.clone().unwrap();
    repository::retry_job(
        db,
        &fixture.owner,
        &stamp("by-hand"),
        &instance.instance_id,
        &job_id,
        shown.revision,
    )
    .unwrap();
    assert_eq!(
        incident_state(db),
        (1, 0, 1, Some(2), None),
        "the wait is over: the person did it"
    );
    assert_eq!(
        repository::drain_automatic_retries(db, t0 + 1_000_000, 32).unwrap(),
        0,
        "no second repeat for the same wait"
    );

    fail_next_attempt(db, "w", t0 + 1_000_001);
    assert_eq!(
        incident_state(db),
        (1, 1, 1, Some(2), Some(t0 + 1_000_001 + 60_000)),
        "one repeat of two is left"
    );
    assert_eq!(notices(db), 0);
}

#[tokio::test]
async fn a_repeat_the_engine_cannot_make_leaves_an_ordinary_incident_and_tells_the_person() {
    let fixture = Fixture::new();
    let db = &fixture.db;
    let _instance = started(&fixture, Some(fixed(3, 10)));
    let t0 = super::jobs::now_ms();
    fail_next_attempt(db, "w", t0);
    assert_eq!(notices(db), 0);
    db.write()
        .unwrap()
        .execute(
            "UPDATE user_accounts SET is_active=0 WHERE id=?1",
            params![fixture.owner.user_id],
        )
        .unwrap();
    assert_eq!(
        repository::drain_automatic_retries(db, t0 + 10_000, 32).unwrap(),
        0
    );
    assert_eq!(
        incident_state(db),
        (1, 1, 0, Some(3), None),
        "no longer armed"
    );
    assert_eq!(events(db, "job_retry_stopped"), 1);
    assert_eq!(
        notices(db),
        1,
        "the person is told now that nothing more will start by itself"
    );
}

/// Spends the whole budget of a one-repeat policy: the engine repeats the failed
/// step once and the new attempt is queued under the same, closed, incident.
fn spend_the_only_repeat(fixture: &Fixture, t0: i64) -> String {
    fail_next_attempt(&fixture.db, "w", t0);
    assert_eq!(
        repository::drain_automatic_retries(&fixture.db, t0 + 10_000, 32).unwrap(),
        1
    );
    assert_eq!(incident_state(&fixture.db), (1, 0, 1, Some(1), None));
    let job_id = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT job_id FROM bpmn_jobs WHERE status='queued'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        events(&fixture.db, "job_retries_exhausted"),
        0,
        "the last repeat is still running"
    );
    job_id
}

#[tokio::test]
async fn a_recovery_incident_on_the_last_repeat_records_that_the_retries_ran_out() {
    let fixture = Fixture::new();
    let _instance = started(&fixture, Some(fixed(1, 10)));
    let t0 = super::jobs::now_ms();
    let job_id = spend_the_only_repeat(&fixture, t0);

    repository::record_undecided_batch(&fixture.db, &[(job_id, true)], t0 + 20_000);

    let code: String = fixture
        .db
        .read()
        .unwrap()
        .query_row("SELECT code FROM bpmn_incidents", [], |r| r.get(0))
        .unwrap();
    assert_eq!(code, "TOOL_RESULT_UNDECIDED");
    assert_eq!(
        incident_state(&fixture.db),
        (1, 1, 1, Some(1), None),
        "never armed again: the code is not one to repeat"
    );
    assert_eq!(
        events(&fixture.db, "job_retry_scheduled"),
        1,
        "only the one repeat that was planned"
    );
    assert_eq!(
        events(&fixture.db, "job_retries_exhausted"),
        1,
        "the history says why nothing more starts by itself"
    );
    assert_eq!(notices(&fixture.db), 1);
}

#[tokio::test]
async fn an_uncertain_outcome_on_the_last_repeat_records_that_the_retries_ran_out() {
    let fixture = Fixture::new();
    let _instance = started(&fixture, Some(fixed(1, 10)));
    let t0 = super::jobs::now_ms();
    spend_the_only_repeat(&fixture, t0);

    let t1 = t0 + 20_000;
    let claimed = claim(&fixture.db, "w", t1);
    assert!(repository::commit_job_dispatch_boundary(&fixture.db, &claimed, "w", t1).unwrap());
    interrupt(&fixture.db, &claimed, "w", t1 + 1);

    let code: String = fixture
        .db
        .read()
        .unwrap()
        .query_row("SELECT code FROM bpmn_incidents", [], |r| r.get(0))
        .unwrap();
    assert_eq!(code, "EXTERNAL_OUTCOME_UNCERTAIN");
    assert_eq!(incident_state(&fixture.db), (1, 1, 1, Some(1), None));
    assert_eq!(events(&fixture.db, "job_retries_exhausted"), 1);
    assert_eq!(events(&fixture.db, "job_retry_scheduled"), 1);
    assert_eq!(
        repository::drain_automatic_retries(&fixture.db, t1 + 100_000_000, 32).unwrap(),
        0
    );
}

/// Two attempts of a three-repeat policy: the engine made the first repeat, and
/// the second attempt ran past its dispatch boundary before it was interrupted.
#[tokio::test]
async fn an_uncertain_outcome_with_repeats_left_records_why_the_engine_stopped() {
    let fixture = Fixture::new();
    let db = &fixture.db;
    let _instance = started(&fixture, Some(fixed(3, 10)));
    let t0 = super::jobs::now_ms();
    fail_next_attempt(db, "w", t0);
    assert_eq!(
        repository::drain_automatic_retries(db, t0 + 10_000, 32).unwrap(),
        1
    );

    let t1 = t0 + 10_001;
    let claimed = claim(db, "w", t1);
    assert!(repository::commit_job_dispatch_boundary(db, &claimed, "w", t1).unwrap());
    interrupt(db, &claimed, "w", t1 + 1);

    assert_eq!(
        incident_state(db),
        (1, 1, 1, Some(3), None),
        "one of three repeats made"
    );
    assert_eq!(
        events(db, "job_retries_exhausted"),
        0,
        "the budget was not spent"
    );
    assert_eq!(events(db, "job_retry_stopped"), 1);
    let reason: String = db
        .read()
        .unwrap()
        .query_row(
            "SELECT json_extract(data_json,'$.reason') FROM bpmn_events WHERE kind='job_retry_stopped'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(reason, "outcome_uncertain");
    assert_eq!(notices(db), 1);
}

// ----- a repeat that cannot start yet is put off, not spent -----

fn deferral_reasons(db: &DbPool) -> Vec<String> {
    let conn = db.read().unwrap();
    let mut statement = conn
        .prepare("SELECT json_extract(data_json,'$.reason') FROM bpmn_events WHERE kind='job_retry_deferred' ORDER BY seq")
        .unwrap();
    statement
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn set_flows(db: &DbPool, status: &str) {
    db.write()
        .unwrap()
        .execute("UPDATE flows SET status=?1", [status])
        .unwrap();
}

#[tokio::test]
async fn a_repeat_put_off_because_the_source_is_away_does_not_spend_the_budget() {
    let fixture = Fixture::new();
    let db = &fixture.db;
    let _instance = started(&fixture, Some(fixed(2, 10)));
    let t0 = super::jobs::now_ms();
    fail_next_attempt(db, "w", t0);
    set_flows(db, "draft");

    for round in 1..=3_i64 {
        let due = t0 + round * 10_000;
        assert_eq!(repository::drain_automatic_retries(db, due, 32).unwrap(), 0);
        assert_eq!(
            incident_state(db),
            (1, 1, 0, Some(2), Some(due + 10_000)),
            "round {round}: nothing was made, the deadline moved by the policy delay"
        );
        assert_eq!(events(db, "job_retry_deferred"), round);
    }
    assert_eq!(deferral_reasons(db), ["source_unavailable"; 3]);
    assert_eq!(
        events(db, "job_retries_exhausted"),
        0,
        "an unspent budget is not exhausted"
    );
    assert_eq!(events(db, "job_retry_stopped"), 0);
    assert_eq!(
        notices(db),
        0,
        "the person is not told while the engine still waits"
    );

    // The source is back: the first real repeat is repeat one of two.
    set_flows(db, "active");
    let due = t0 + 40_000;
    assert_eq!(repository::drain_automatic_retries(db, due, 32).unwrap(), 1);
    assert_eq!(incident_state(db), (1, 0, 1, Some(2), None));
}

#[tokio::test]
async fn a_repeat_put_off_because_another_step_is_running_waits_the_delay_of_the_repeat_still_to_come(
) {
    let fixture = Fixture::new();
    let db = &fixture.db;
    let instance = started(&fixture, Some(growing(3, 10, 200, 600)));
    let t0 = super::jobs::now_ms();
    fail_next_attempt(db, "w", t0);
    assert_eq!(
        repository::drain_automatic_retries(db, t0 + 10_000, 32).unwrap(),
        1
    );
    let t1 = t0 + 10_001;
    fail_next_attempt(db, "w", t1);
    assert_eq!(incident_state(db), (1, 1, 1, Some(3), Some(t1 + 20_000)));

    db.write()
        .unwrap()
        .execute(
            "INSERT INTO bpmn_tokens(token_id,instance_id,scope_id,node_id,arrival_edge_id,fork_stack_json,status,created_at_ms) VALUES('stray',?1,?1,'Start_1',NULL,'[]','ready',1)",
            [&instance.instance_id],
        )
        .unwrap();
    let due = t1 + 20_000;
    assert_eq!(repository::drain_automatic_retries(db, due, 32).unwrap(), 0);
    assert_eq!(
        incident_state(db),
        (1, 1, 1, Some(3), Some(due + 20_000)),
        "one repeat made, still one of three left, and the wait of the second repeat"
    );
    assert_eq!(deferral_reasons(db), ["activation_busy"]);
    assert_eq!(events(db, "job_retries_exhausted"), 0);

    db.write()
        .unwrap()
        .execute("DELETE FROM bpmn_tokens WHERE token_id='stray'", [])
        .unwrap();
    assert_eq!(
        repository::drain_automatic_retries(db, due + 20_000, 32).unwrap(),
        1
    );
    assert_eq!(incident_state(db), (1, 0, 2, Some(3), None));
}

#[tokio::test]
async fn a_source_that_never_comes_back_ends_the_automatic_repeats_after_the_bound() {
    let fixture = Fixture::new();
    let db = &fixture.db;
    let _instance = started(&fixture, Some(fixed(3, 10)));
    let t0 = super::jobs::now_ms();
    fail_next_attempt(db, "w", t0);
    set_flows(db, "draft");

    let mut due = t0 + 10_000;
    for _ in 0..repository::MAX_RETRY_DEFERRALS {
        assert_eq!(repository::drain_automatic_retries(db, due, 32).unwrap(), 0);
        due += 10_000;
    }
    assert_eq!(
        events(db, "job_retry_deferred"),
        i64::from(repository::MAX_RETRY_DEFERRALS)
    );
    assert_eq!(notices(db), 0);
    assert_eq!(repository::drain_automatic_retries(db, due, 32).unwrap(), 0);
    assert_eq!(
        incident_state(db),
        (1, 1, 0, Some(3), None),
        "disarmed, budget untouched"
    );
    assert_eq!(events(db, "job_retry_stopped"), 1);
    assert_eq!(events(db, "job_retries_exhausted"), 0);
    let reason: String = db
        .read()
        .unwrap()
        .query_row(
            "SELECT json_extract(data_json,'$.reason') FROM bpmn_events WHERE kind='job_retry_stopped'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(reason, "deferral_limit");
    assert_eq!(
        notices(db),
        1,
        "the person is told once the engine gives up"
    );
}

#[tokio::test]
async fn a_repeat_put_off_for_longer_than_the_window_ends_the_automatic_repeats() {
    let fixture = Fixture::new();
    let db = &fixture.db;
    let _instance = started(&fixture, Some(fixed(3, 10)));
    let t0 = super::jobs::now_ms();
    fail_next_attempt(db, "w", t0);
    set_flows(db, "draft");
    assert_eq!(
        repository::drain_automatic_retries(db, t0 + repository::MAX_RETRY_DEFERRAL_WINDOW_MS, 32)
            .unwrap(),
        0
    );
    assert_eq!(events(db, "job_retry_deferred"), 0);
    assert_eq!(events(db, "job_retry_stopped"), 1);
    assert_eq!(incident_state(db), (1, 1, 0, Some(3), None));
    assert_eq!(notices(db), 1);
}

// ----- the pass over due repeats -----

#[test]
fn only_a_store_failure_a_later_pass_can_get_past_is_transient() {
    let sqlite = |code: i32| {
        anyhow::Error::from(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(code),
            None,
        ))
    };
    for transient in [
        sqlite(rusqlite::ffi::SQLITE_BUSY),
        sqlite(rusqlite::ffi::SQLITE_LOCKED),
        sqlite(rusqlite::ffi::SQLITE_INTERRUPT),
        sqlite(rusqlite::ffi::SQLITE_IOERR),
        anyhow::Error::from(std::io::Error::other("disk")),
        sqlite(rusqlite::ffi::SQLITE_BUSY).context("while repeating a step"),
    ] {
        assert!(
            super::repository::is_transient_retry_error_for_tests(&transient),
            "{transient:#}"
        );
    }
    for permanent in [
        anyhow::anyhow!("the step lost its model"),
        sqlite(rusqlite::ffi::SQLITE_CONSTRAINT),
        sqlite(rusqlite::ffi::SQLITE_CORRUPT),
        anyhow::Error::from(rusqlite::Error::QueryReturnedNoRows),
    ] {
        assert!(
            !super::repository::is_transient_retry_error_for_tests(&permanent),
            "{permanent:#}"
        );
    }
}

/// A second armed incident that is due before the real one, so a pass meets it first.
fn add_earlier_incident(db: &DbPool, id: &str, next_attempt_at_ms: i64) {
    let conn = db.write().unwrap();
    // The extra row names a job that does not exist: only the scan and the injected step ever look at it.
    conn.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    conn.execute(
        "INSERT INTO bpmn_incidents(incident_id,instance_id,scope_id,node_id,job_id,code,message,at_ms,attempt_limit,next_attempt_at_ms) \
         SELECT ?1,instance_id,scope_id,node_id,'ghost-'||?1,code,message,at_ms,attempt_limit,?2 FROM bpmn_incidents LIMIT 1",
        params![id, next_attempt_at_ms],
    )
    .unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
}

fn next_attempt_of(db: &DbPool, id: &str) -> Option<i64> {
    db.read()
        .unwrap()
        .query_row(
            "SELECT next_attempt_at_ms FROM bpmn_incidents WHERE incident_id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
}

#[tokio::test]
async fn a_transient_failure_keeps_the_repeat_armed_and_the_rest_of_the_page_runs() {
    let fixture = Fixture::new();
    let db = &fixture.db;
    let _instance = started(&fixture, Some(fixed(3, 10)));
    let t0 = super::jobs::now_ms();
    fail_next_attempt(db, "w", t0);
    add_earlier_incident(db, "00-busy", t0 + 5_000);

    let step = |pool: &DbPool,
                incident: &str,
                instance: &str,
                job: &str,
                now: i64|
     -> anyhow::Result<bool> {
        if incident == "00-busy" {
            return Err(anyhow::Error::from(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                None,
            )));
        }
        repository::retry_due_incident_for_tests(pool, incident, instance, job, now)
    };
    let repeated = repository::drain_due_incidents_for_tests(db, t0 + 10_000, 32, &step).unwrap();
    assert_eq!(repeated, 1, "the incident behind the busy one was repeated");
    assert_eq!(
        next_attempt_of(db, "00-busy"),
        Some(t0 + 5_000),
        "the busy one keeps its deadline"
    );
    assert_eq!(
        events(db, "job_retry_stopped"),
        0,
        "a transient failure disarms nothing"
    );
}

#[tokio::test]
async fn an_engine_error_disarms_only_its_own_incident_and_a_failed_disarm_does_not_block_the_page()
{
    let fixture = Fixture::new();
    let db = &fixture.db;
    let _instance = started(&fixture, Some(fixed(3, 10)));
    let t0 = super::jobs::now_ms();
    fail_next_attempt(db, "w", t0);
    add_earlier_incident(db, "00-broken", t0 + 5_000);
    add_earlier_incident(db, "01-stuck", t0 + 6_000);
    // The disarm of the stuck incident cannot be written.
    db.write()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER refuse_disarm BEFORE UPDATE OF next_attempt_at_ms ON bpmn_incidents \
             WHEN NEW.next_attempt_at_ms IS NULL AND OLD.incident_id='01-stuck' \
             BEGIN SELECT RAISE(ABORT,'disarm refused'); END;",
        )
        .unwrap();

    let step = |pool: &DbPool,
                incident: &str,
                instance: &str,
                job: &str,
                now: i64|
     -> anyhow::Result<bool> {
        if incident.starts_with('0') {
            anyhow::bail!("the engine could not start {incident}");
        }
        repository::retry_due_incident_for_tests(pool, incident, instance, job, now)
    };
    let repeated = repository::drain_due_incidents_for_tests(db, t0 + 10_000, 32, &step).unwrap();
    assert_eq!(
        repeated, 1,
        "the real incident was repeated after both failures"
    );
    assert_eq!(
        next_attempt_of(db, "00-broken"),
        None,
        "the broken one is disarmed"
    );
    assert_eq!(
        next_attempt_of(db, "01-stuck"),
        Some(t0 + 6_000),
        "its disarm failed and was only logged"
    );
    assert_eq!(events(db, "job_retry_stopped"), 1);
}

#[tokio::test]
async fn two_passes_on_the_same_deadline_make_the_repeat_once() {
    let fixture = Fixture::new();
    let db = &fixture.db;
    let _instance = started(&fixture, Some(fixed(3, 10)));
    let t0 = super::jobs::now_ms();
    fail_next_attempt(db, "w", t0);

    let results: Vec<u32> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..2)
            .map(|_| {
                scope.spawn(|| repository::drain_automatic_retries(db, t0 + 10_000, 32).unwrap())
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(results.iter().sum::<u32>(), 1, "{results:?}");
    assert_eq!(incident_state(db), (1, 0, 1, Some(3), None));
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM bpmn_jobs WHERE status='queued'"),
        1,
        "one new attempt, not two"
    );
}

// ----- the row after the incident ends -----

#[tokio::test]
async fn cancelling_the_instance_clears_the_deadline_of_a_waiting_incident() {
    let fixture = Fixture::new();
    let db = &fixture.db;
    let instance = started(&fixture, Some(fixed(3, 600)));
    let t0 = super::jobs::now_ms();
    fail_next_attempt(db, "w", t0);
    assert_eq!(incident_state(db).4, Some(t0 + 600_000));
    let shown = repository::get_instance(db, &fixture.owner, &instance.instance_id, None).unwrap();
    repository::cancel_instance(
        db,
        &fixture.owner,
        &stamp("cancel"),
        &instance.instance_id,
        shown.revision,
    )
    .unwrap();
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM bpmn_incidents WHERE resolved_at_ms IS NOT NULL AND next_attempt_at_ms IS NULL"),
        1,
        "a resolved incident waits for nothing"
    );
    assert_eq!(
        repository::drain_automatic_retries(db, t0 + 100_000_000, 32).unwrap(),
        0
    );
}

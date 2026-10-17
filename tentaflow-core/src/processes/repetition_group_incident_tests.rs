// ============ File: repetition_group_incident_tests.rs — parallel repeated Service groups that already hold an incident ============

use serde_json::{json, Value};
use tentaflow_protocol::processes::{
    ActivityVerification, ProcessInstanceStatus, ProcessMultiInstanceInput,
    ProcessMultiInstanceMode, ProcessNodeKind, ProcessRepeatSpec,
    ProcessRepetitionGroupStatus, ProcessRepetitionOccurrenceStatus,
};

use super::repetition_service_tests::{observed_flow_result, repeated_service_model};
use super::repository::{self, ClaimedProcessJob, ObservedActivityResult, RuntimeSnapshot};
use super::runtime::{self, test_support::{flow, stamp, start_model, Fixture}};

type Status = ProcessRepetitionOccurrenceStatus;

pub(super) fn service_result(outcome: &str, code: Option<&str>, answer: u32) -> Value {
    json!({"outcome":outcome,"code":code,"summary":format!("{outcome} {answer}"),
        "outputs":{"answer":answer},"evidence":[]})
}

pub(super) struct Member {
    pub(super) worker: String,
    pub(super) claim: ClaimedProcessJob,
    pub(super) observed: ObservedActivityResult,
}

/// A Parallel two-ordinal repeated Service whose ordinal N returns `results[N]`;
/// both ordinals are claimed and observed, nothing is accepted yet.
pub(super) struct ParallelGroup {
    pub(super) fixture: Fixture,
    pub(super) instance_id: String,
    pub(super) members: Vec<Member>,
}

impl ParallelGroup {
    pub(super) async fn start(results: [Value; 2], verification: ActivityVerification) -> Self {
        Self::start_shaped(results, verification, |model| model).await
    }

    /// `shape` reshapes the model around the repeated Service (boundaries, a
    /// surrounding sub-process, extra branches) before it is published.
    pub(super) async fn start_shaped(
        results: [Value; 2], verification: ActivityVerification,
        shape: impl FnOnce(tentaflow_protocol::processes::ProcessModel)
            -> tentaflow_protocol::processes::ProcessModel,
    ) -> Self {
        let fixture = Fixture::new();
        let graph = json!({"nodes":[{"id":"trigger","type":"trigger","config":{
            "output_mapping":{"res0":results[0].to_string(),"res1":results[1].to_string()}}},
            {"id":"output","type":"output","config":{}}],
            "edges":[{"from":"trigger","to":"output","from_port":"text","to_port":"text"}],
            "variables":[{"name":"res0","type":"json"},{"name":"res1","type":"json"}]}).to_string();
        let flow_id = flow(&fixture.db, &fixture.owner, &graph);
        let mut model = repeated_service_model(&flow_id, verification);
        let service = model.nodes.iter_mut().find(|node| node.id == "Service").unwrap();
        service.repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Parallel,
            input: ProcessMultiInstanceInput::CollectionExpression {
                expression: "vars.items".into(),
            },
            output_collection_variable: "results".into(),
        });
        let ProcessNodeKind::ServiceTask { result_expression, output_mapping, .. } =
            &mut service.kind else { panic!("fixture Service node changed kind") };
        *result_expression = Some(
            "repeat.index == 0 ? outputs.variables.res0 : outputs.variables.res1".into());
        output_mapping.insert("answer".into(), "outputs.answer".into());
        let started = start_model(&fixture, &shape(model));
        let mut claimed = Vec::new();
        for index in 0..2 {
            let worker = format!("group-incident-worker-{index}");
            let claim = repository::claim_job(&fixture.db, &worker,
                chrono::Utc::now().timestamp_millis()).unwrap().unwrap();
            let ordinal = claim.snapshot.repetition_occurrences.iter()
                .find(|row| row.job_id.as_deref() == Some(claim.job.job_id.as_str()))
                .unwrap().ordinal;
            claimed.push((ordinal, worker, claim));
        }
        claimed.sort_by_key(|(ordinal, _, _)| *ordinal);
        let mut members = Vec::new();
        for (_, worker, claim) in claimed {
            let observed = observed_flow_result(&fixture, &claim).await;
            members.push(Member { worker, claim, observed });
        }
        Self { fixture, instance_id: started.instance_id, members }
    }

    pub(super) fn snapshot(&self) -> RuntimeSnapshot {
        repository::runtime_snapshot(&self.fixture.db, &self.fixture.owner,
            &self.instance_id).unwrap()
    }

    /// Plans from the current durable state, so a sibling accepted earlier is seen.
    pub(super) fn accept(&self, ordinal: usize) -> anyhow::Result<()> {
        let snapshot = self.snapshot();
        let member = &self.members[ordinal];
        let job = snapshot.jobs.iter().find(|job| job.job_id == member.claim.job.job_id).unwrap();
        let at = chrono::Utc::now().timestamp_millis();
        let plan = runtime::plan_job_result(&snapshot, job, &member.observed, at, None)?;
        repository::accept_job_result(&self.fixture.db, &self.fixture.owner,
            &member.claim.job.job_id, member.claim.job.attempt, member.claim.job.fence,
            &member.worker, &member.observed, snapshot.instance.revision,
            repository::ProcessPlanInput::Supplied(&plan), at).map(|_| ())
    }

    /// The plan the engine makes for `ordinal` when its Service node is pinned with
    /// `verification` instead of the configuration the instance really holds.
    pub(super) fn plan_pinned_as(
        &self, ordinal: usize, verification: ActivityVerification, at: i64,
    ) -> repository::RuntimePlan {
        let mut snapshot = self.snapshot();
        let service = snapshot.model.nodes.iter_mut().find(|node| node.id == "Service").unwrap();
        let ProcessNodeKind::ServiceTask { verification: pinned, .. } = &mut service.kind
            else { panic!("fixture Service node changed kind") };
        *pinned = verification;
        let member = &self.members[ordinal];
        let job = snapshot.jobs.iter().find(|job| job.job_id == member.claim.job.job_id).unwrap();
        runtime::plan_job_result(&snapshot, job, &member.observed, at, None).unwrap()
    }

    pub(super) fn accept_plan(
        &self, ordinal: usize, plan: &repository::RuntimePlan, at: i64,
    ) -> anyhow::Result<()> {
        let member = &self.members[ordinal];
        repository::accept_job_result(&self.fixture.db, &self.fixture.owner,
            &member.claim.job.job_id, member.claim.job.attempt, member.claim.job.fence,
            &member.worker, &member.observed, self.snapshot().instance.revision,
            repository::ProcessPlanInput::Supplied(plan), at).map(|_| ())
    }

    /// Fires the pending timer of boundary `node_id` once it is due.
    pub(super) fn fire_timer(&self, node_id: &str) -> anyhow::Result<()> {
        let snapshot = self.snapshot();
        let timer = snapshot.timers.iter().find(|timer| timer.node_id == node_id
            && timer.status == tentaflow_protocol::processes::ProcessTimerStatus::Pending)
            .expect("boundary timer is pending");
        let due = timer.due_at_ms.unwrap() + 1;
        let candidate = repository::due_timers(&self.fixture.db, due, 32).unwrap().into_iter()
            .find(|candidate| candidate.timer_id == timer.timer_id).expect("timer is due");
        let timer_snapshot = repository::timer_snapshot(&self.fixture.db, &candidate).unwrap();
        let plan = super::timers::plan_timer_fire(&timer_snapshot, due, None, None)?;
        repository::fire_timer(&self.fixture.db, &candidate, &self.fixture.owner,
            Some(snapshot.instance.revision), repository::ProcessPlanInput::Supplied(&plan), due)
            .map(|_| ())
    }

    pub(super) fn accept_all(&self, order: &[usize]) {
        for ordinal in order {
            self.accept(*ordinal).unwrap_or_else(|error|
                panic!("order {order:?}: ordinal {ordinal} was refused: {error:#}"));
        }
    }

    pub(super) fn decide(&self, ordinal: u32, approve: bool) -> anyhow::Result<()> {
        let task_id = self.snapshot().repetition_occurrences.iter().find(|row| row.ordinal == ordinal)
            .and_then(|row| row.verification_user_task_id.clone())
            .expect("ordinal waits for its Verification");
        self.complete_task(&task_id, &Value::Null, Some(approve))
    }

    /// Completes the open Work task of `node_id`.
    pub(super) fn complete_work(&self, node_id: &str) -> anyhow::Result<()> {
        let task_id = self.snapshot().user_tasks.iter().find(|task| task.node_id == node_id
            && task.status == tentaflow_protocol::processes::ProcessUserTaskStatus::Open)
            .map(|task| task.user_task_id.clone()).expect("Work task is open");
        self.complete_task(&task_id, &json!({}), None)
    }

    fn complete_task(&self, task_id: &str, outputs: &Value, approve: Option<bool>) -> anyhow::Result<()> {
        let snapshot = self.snapshot();
        let command = stamp("complete parallel group task");
        let at = chrono::Utc::now().timestamp_millis();
        let plan = runtime::plan_user_completion(&snapshot, task_id, outputs, approve, at,
            runtime::test_support::human_input(&snapshot, task_id, &command), None)?;
        repository::complete_user_task(&self.fixture.db, &self.fixture.owner, &command,
            &self.instance_id, task_id, snapshot.instance.revision, outputs,
            approve, repository::ProcessPlanInput::Supplied(&plan), at).map(|_| ())
    }
}

fn statuses(snapshot: &RuntimeSnapshot) -> Vec<Status> {
    let mut rows = snapshot.repetition_occurrences.iter().collect::<Vec<_>>();
    rows.sort_by_key(|row| row.ordinal);
    rows.into_iter().map(|row| row.status).collect()
}

fn codes(snapshot: &RuntimeSnapshot) -> Vec<String> {
    let mut codes = snapshot.incidents.iter().map(|row| row.code.clone()).collect::<Vec<_>>();
    codes.sort();
    codes
}

/// The status a Service job keeps after its result was accepted, per outcome.
fn job_status_of(snapshot: &RuntimeSnapshot, member: &Member) -> String {
    snapshot.jobs.iter().find(|job| job.job_id == member.claim.job.job_id).unwrap().status.clone()
}

fn condition_true() -> ActivityVerification {
    ActivityVerification::Condition { expression: "true".into() }
}

#[tokio::test]
async fn a_sibling_still_completes_under_a_group_parked_by_an_unhandled_result() {
    for order in [[0, 1], [1, 0]] {
        let group = ParallelGroup::start(
            [service_result("Error", Some("CODE_A"), 1), service_result("Completed", None, 2)],
            condition_true()).await;
        group.accept_all(&order);
        let snapshot = group.snapshot();
        assert_eq!(snapshot.instance.status, ProcessInstanceStatus::Incident, "{order:?}");
        assert_eq!(codes(&snapshot), ["CODE_A"], "{order:?}");
        let incident = &snapshot.incidents[0];
        assert_eq!(incident.job_id.as_deref(), Some(group.members[0].claim.job.job_id.as_str()));
        let parked = &snapshot.repetition_groups[0];
        assert_eq!(parked.status, ProcessRepetitionGroupStatus::Incident, "{order:?}");
        assert_eq!(parked.terminal_incident_id.as_deref(), Some(incident.incident_id.as_str()),
            "{order:?}: the sibling must not replace or drop the group's incident");
        assert_eq!(parked.completed_count, 1, "{order:?}");
        assert_eq!(statuses(&snapshot), [Status::AcceptedBlocked, Status::Completed], "{order:?}");
        assert_eq!([job_status_of(&snapshot, &group.members[0]), job_status_of(&snapshot, &group.members[1])],
            ["error", "completed"], "{order:?}: no result may stay observed");
    }
}

#[tokio::test]
async fn two_unhandled_results_keep_the_first_incident_as_the_group_incident_and_raise_the_second() {
    for (results, expected_codes) in [
        ([service_result("Error", Some("CODE_A"), 1), service_result("Error", Some("CODE_B"), 2)],
            ["CODE_A", "CODE_B"]),
        ([service_result("Cancelled", Some("STOPPED"), 1), service_result("Error", Some("CODE_B"), 2)],
            ["CODE_B", "STOPPED"]),
    ] {
        for order in [[0, 1], [1, 0]] {
            let group = ParallelGroup::start(results.clone(), condition_true()).await;
            group.accept_all(&order);
            let snapshot = group.snapshot();
            let label = format!("{results:?} {order:?}");
            assert_eq!(codes(&snapshot), expected_codes, "{label}");
            let first_job = &group.members[order[0]].claim.job.job_id;
            let first = snapshot.incidents.iter()
                .find(|incident| incident.job_id.as_deref() == Some(first_job.as_str())).unwrap();
            assert_eq!(snapshot.repetition_groups[0].terminal_incident_id.as_deref(),
                Some(first.incident_id.as_str()), "{label}: the first incident stays the group's");
            assert_eq!(statuses(&snapshot), [Status::AcceptedBlocked, Status::AcceptedBlocked], "{label}");
            assert!(snapshot.jobs.iter().all(|job| matches!(job.status.as_str(), "error" | "cancelled")),
                "{label}: no result may stay observed");
            assert_eq!(snapshot.repetition_groups[0].completed_count, 0, "{label}");
        }
    }
}

#[tokio::test]
async fn a_rejected_verification_and_an_unhandled_result_park_the_group_in_either_order() {
    for order in [["error", "reject"], ["reject", "error"]] {
        let group = ParallelGroup::start(
            [service_result("NeedsHuman", Some("REVIEW"), 1), service_result("Error", Some("CODE_B"), 2)],
            condition_true()).await;
        for step in order {
            match step {
                "error" => group.accept(1).unwrap_or_else(|error|
                    panic!("{order:?}: the erroring sibling was refused: {error:#}")),
                _ => {
                    if group.snapshot().repetition_occurrences[0].verification_user_task_id.is_none() {
                        group.accept(0).unwrap_or_else(|error|
                            panic!("{order:?}: the NeedsHuman sibling was refused: {error:#}"));
                    }
                    group.decide(0, false).unwrap_or_else(|error|
                        panic!("{order:?}: the rejection was refused: {error:#}"));
                }
            }
        }
        let snapshot = group.snapshot();
        assert_eq!(codes(&snapshot), ["CODE_B", "HUMAN_REJECTED"], "{order:?}");
        assert_eq!(statuses(&snapshot), [Status::AcceptedBlocked, Status::AcceptedBlocked], "{order:?}");
        assert_eq!(snapshot.repetition_groups[0].status, ProcessRepetitionGroupStatus::Incident);
    }
}

#[tokio::test]
async fn a_sibling_verification_is_decided_under_a_rejected_one() {
    let group = ParallelGroup::start(
        [service_result("NeedsHuman", Some("REVIEW"), 1), service_result("NeedsHuman", Some("REVIEW"), 2)],
        condition_true()).await;
    group.accept_all(&[0, 1]);
    group.decide(0, false).unwrap();
    group.decide(1, true).unwrap_or_else(|error|
        panic!("the sibling's approval was refused after a rejection: {error:#}"));
    let snapshot = group.snapshot();
    assert_eq!(statuses(&snapshot), [Status::AcceptedBlocked, Status::Completed]);
    assert_eq!(codes(&snapshot), ["HUMAN_REJECTED"]);
    assert_eq!(snapshot.repetition_groups[0].status, ProcessRepetitionGroupStatus::Incident);
    assert_eq!(snapshot.repetition_groups[0].completed_count, 1);
}

// A plan that blocks a result the pinned configuration does not block is read as an
// ordinary completion, and refused for lacking what a completion needs.
const UNMET_VERIFICATION: &str = "repeated Service completion differs from its required factual Verification";
const UNMATCHED_BLOCK: &str = "accepted repetition mapping failure differs from pinned source and state";
const UNSUPPORTED_AGGREGATE_INCIDENT: &str = "blocked repetition group has an unsupported aggregate incident";

fn condition(expression: &str) -> ActivityVerification {
    ActivityVerification::Condition { expression: expression.into() }
}

/// Each case pins `really`, then hands the validator the plan the engine makes for
/// `forged`: a plan that is well formed but not what the pinned configuration produces.
#[tokio::test]
async fn a_plan_the_pinned_verification_would_not_make_is_refused() {
    let cases = [
        // (really, forged, why it must be refused, the refusal it gets)
        (ActivityVerification::Human, condition("false"),
            "a Human verification cannot be skipped by blocking the result", UNMET_VERIFICATION),
        (condition("true"), condition("false"),
            "a result its Condition accepts cannot be blocked", UNMATCHED_BLOCK),
        (condition("true"), condition("outputs.missing.deeper"),
            "a result its Condition accepts cannot be blocked by an expression error", UNMATCHED_BLOCK),
        (condition("false"), condition("outputs.missing.deeper"),
            "VERIFICATION_FAILED cannot be relabelled EXPRESSION_ERROR", UNMATCHED_BLOCK),
        (condition("outputs.missing.deeper"), condition("false"),
            "EXPRESSION_ERROR cannot be relabelled VERIFICATION_FAILED", UNMATCHED_BLOCK),
        (condition("outputs.missing.deeper"), condition("outputs.other.deeper"),
            "an expression error keeps the message the pinned expression raises", UNMATCHED_BLOCK),
    ];
    for (really, forged, why, reason) in cases {
        let group = ParallelGroup::start(
            [service_result("Completed", None, 1), service_result("Completed", None, 2)],
            really).await;
        let at = chrono::Utc::now().timestamp_millis();
        let before = super::call_tests::transition_rows(&group.fixture);
        let blocked = group.plan_pinned_as(0, forged, at);
        assert!(!blocked.add_incidents.is_empty(), "{why}: the forged plan blocks the result");
        let refusal = group.accept_plan(0, &blocked, at).unwrap_err();
        assert!(format!("{refusal:#}").contains(reason), "{why}: refused for {refusal:#}");
        let mut retryable = blocked.clone();
        for occurrence in &mut retryable.repetition_occurrences {
            occurrence.status = Status::RetryableError;
        }
        let refusal = group.accept_plan(0, &retryable, at).unwrap_err();
        assert!(format!("{refusal:#}").contains(UNSUPPORTED_AGGREGATE_INCIDENT),
            "{why}: the retryable status gives the blocked result no way past the check: {refusal:#}");
        assert_eq!(super::call_tests::transition_rows(&group.fixture), before,
            "{why}: a refused plan leaves no trace");
        group.accept(0).unwrap_or_else(|error|
            panic!("{why}: the genuine plan was refused too: {error:#}"));
    }
}

#[tokio::test]
async fn a_blocked_result_keeps_the_status_of_a_blocked_ordinal() {
    let group = ParallelGroup::start(
        [service_result("Error", Some("CODE_A"), 1), service_result("Completed", None, 2)],
        condition_true()).await;
    let at = chrono::Utc::now().timestamp_millis();
    let mut plan = group.plan_pinned_as(0, condition_true(), at);
    assert!(plan.repetition_occurrences.iter().all(|row| row.status == Status::AcceptedBlocked));
    for occurrence in &mut plan.repetition_occurrences {
        occurrence.status = Status::RetryableError;
    }
    let before = super::call_tests::transition_rows(&group.fixture);
    let refusal = group.accept_plan(0, &plan, at).unwrap_err();
    assert!(format!("{refusal:#}").contains(UNSUPPORTED_AGGREGATE_INCIDENT), "{refusal:#}");
    assert_eq!(super::call_tests::transition_rows(&group.fixture), before);
    group.accept(0).unwrap();
}

#[tokio::test]
async fn accepting_the_same_blocked_result_twice_changes_nothing() {
    let group = ParallelGroup::start(
        [service_result("Error", Some("CODE_A"), 1), service_result("Completed", None, 2)],
        condition_true()).await;
    let at = chrono::Utc::now().timestamp_millis();
    let plan = group.plan_pinned_as(0, condition_true(), at);
    group.accept_plan(0, &plan, at).unwrap();
    let after_first = super::call_tests::transition_rows(&group.fixture);
    group.accept_plan(0, &plan, at).unwrap();
    assert_eq!(super::call_tests::transition_rows(&group.fixture), after_first);
    assert_eq!(codes(&group.snapshot()), ["CODE_A"]);
    let sibling = group.plan_pinned_as(1, condition_true(), at);
    group.accept_plan(1, &sibling, at).unwrap();
    let again = super::call_tests::transition_rows(&group.fixture);
    group.accept_plan(1, &sibling, at).unwrap();
    assert_eq!(super::call_tests::transition_rows(&group.fixture), again);
    assert_eq!(statuses(&group.snapshot()), [Status::AcceptedBlocked, Status::Completed]);
}

#[tokio::test]
async fn a_plan_cannot_replace_the_incident_a_group_already_names() {
    let group = ParallelGroup::start(
        [service_result("Error", Some("CODE_A"), 1), service_result("Error", Some("CODE_B"), 2)],
        condition_true()).await;
    group.accept(0).unwrap();
    let first = group.snapshot().repetition_groups[0].terminal_incident_id.clone().unwrap();
    let at = chrono::Utc::now().timestamp_millis();
    let mut plan = group.plan_pinned_as(1, condition_true(), at);
    assert_eq!(plan.repetition_groups[0].terminal_incident_id.as_deref(), Some(first.as_str()));
    plan.repetition_groups[0].terminal_incident_id = Some(plan.add_incidents[0].incident_id.clone());
    let before = super::call_tests::transition_rows(&group.fixture);
    let refusal = group.accept_plan(1, &plan, at).unwrap_err();
    assert!(format!("{refusal:#}").contains(UNMET_VERIFICATION), "{refusal:#}");
    assert_eq!(super::call_tests::transition_rows(&group.fixture), before);
    group.accept(1).unwrap();
    assert_eq!(group.snapshot().repetition_groups[0].terminal_incident_id.as_deref(), Some(first.as_str()));
}

#[tokio::test]
async fn an_interrupting_boundary_resolves_every_incident_of_a_parked_group() {
    for results in [
        [service_result("Error", Some("CODE_A"), 1), service_result("Error", Some("CODE_B"), 2)],
        [service_result("Error", Some("CODE_A"), 1), service_result("Completed", None, 2)],
    ] {
        let group = ParallelGroup::start_shaped(results.clone(), condition_true(), |model|
            runtime::test_support::with_boundaries(model, "Service", &[("Deadline", true, 1)])).await;
        group.accept_all(&[0, 1]);
        let parked = group.snapshot();
        assert_eq!(parked.instance.status, ProcessInstanceStatus::Incident);
        assert_eq!(codes(&parked).len(), results.iter().filter(|r| r["outcome"] == "Error").count());
        group.fire_timer("Deadline").unwrap_or_else(|error|
            panic!("{results:?}: the deadline was refused: {error:#}"));
        let snapshot = group.snapshot();
        assert_eq!(snapshot.instance.status, ProcessInstanceStatus::Completed, "{results:?}");
        assert_eq!(codes(&snapshot), Vec::<String>::new(),
            "{results:?}: no incident may outlive the interrupted group");
        let open: i64 = group.fixture.db.read().unwrap().query_row(
            "SELECT COUNT(*) FROM bpmn_incidents WHERE instance_id=?1 AND resolved_at_ms IS NULL",
            [&group.instance_id], |row| row.get(0)).unwrap();
        assert_eq!(open, 0, "{results:?}: an unresolved incident row is left behind");
        assert_eq!(snapshot.repetition_groups[0].status, ProcessRepetitionGroupStatus::Cancelled);
    }
}

#[tokio::test]
async fn an_interrupting_boundary_resolves_a_rejected_verification_and_an_unhandled_result() {
    let group = ParallelGroup::start_shaped(
        [service_result("NeedsHuman", Some("REVIEW"), 1), service_result("Error", Some("CODE_B"), 2)],
        condition_true(), |model|
            runtime::test_support::with_boundaries(model, "Service", &[("Deadline", true, 1)])).await;
    group.accept_all(&[0, 1]);
    group.decide(0, false).unwrap();
    assert_eq!(codes(&group.snapshot()), ["CODE_B", "HUMAN_REJECTED"]);
    group.fire_timer("Deadline").unwrap();
    let snapshot = group.snapshot();
    assert_eq!(snapshot.instance.status, ProcessInstanceStatus::Completed);
    assert_eq!(codes(&snapshot), Vec::<String>::new());
}

/// Start -> Split -> { repeated Service, Review } -> End_1, which terminates the instance.
fn with_terminating_branch(
    mut model: tentaflow_protocol::processes::ProcessModel,
) -> tentaflow_protocol::processes::ProcessModel {
    use tentaflow_protocol::processes::ProcessNode;
    let node = |id: &str, kind| ProcessNode {
        id: id.into(), name: id.into(), kind, repeat: None, activity_io: None };
    model.nodes.extend([
        node("Split", ProcessNodeKind::ParallelGateway),
        node("Review", ProcessNodeKind::UserTask {
            assignee_user_id: None, output_mapping: Default::default() }),
    ]);
    model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::TerminateEnd;
    model.sequence_flows.retain(|flow| flow.id != "ToService");
    model.sequence_flows.extend([
        runtime::test_support::edge("ToSplit", "Start_1", "Split"),
        runtime::test_support::edge("SplitToService", "Split", "Service"),
        runtime::test_support::edge("SplitToReview", "Split", "Review"),
        runtime::test_support::edge("ReviewToEnd", "Review", "End_1"),
    ]);
    model
}

#[tokio::test]
async fn a_terminate_end_closes_a_group_parked_by_an_unhandled_result() {
    for results in [
        [service_result("Error", Some("CODE_A"), 1), service_result("Completed", None, 2)],
        [service_result("NeedsHuman", Some("REVIEW"), 1), service_result("Error", Some("CODE_B"), 2)],
    ] {
        let group = ParallelGroup::start_shaped(results.clone(), condition_true(),
            with_terminating_branch).await;
        group.accept_all(&[0, 1]);
        if results[0]["outcome"] == "NeedsHuman" {
            group.decide(0, false).unwrap();
        }
        assert_eq!(group.snapshot().instance.status, ProcessInstanceStatus::Incident);
        group.complete_work("Review").unwrap_or_else(|error|
            panic!("{results:?}: the TerminateEnd was refused: {error:#}"));
        let snapshot = group.snapshot();
        assert_eq!(snapshot.instance.status, ProcessInstanceStatus::Completed, "{results:?}");
        assert_eq!(codes(&snapshot), Vec::<String>::new(), "{results:?}: no incident may outlive it");
    }
}

#[tokio::test]
async fn an_enclosing_boundary_closes_a_group_parked_by_an_unhandled_result() {
    for results in [
        [service_result("Error", Some("CODE_A"), 1), service_result("Completed", None, 2)],
        [service_result("Error", Some("CODE_A"), 1), service_result("Error", Some("CODE_B"), 2)],
    ] {
        let group = ParallelGroup::start_shaped(results.clone(), condition_true(), |model|
            runtime::test_support::with_boundaries(
                runtime::test_support::embedded_model(model, "Sub"), "Sub",
                &[("Deadline", true, 1)])).await;
        group.accept_all(&[0, 1]);
        assert_eq!(group.snapshot().instance.status, ProcessInstanceStatus::Incident);
        group.fire_timer("Deadline").unwrap_or_else(|error|
            panic!("{results:?}: the enclosing deadline was refused: {error:#}"));
        let snapshot = group.snapshot();
        assert_eq!(snapshot.instance.status, ProcessInstanceStatus::Completed, "{results:?}");
        assert_eq!(codes(&snapshot), Vec::<String>::new(), "{results:?}");
    }
}

fn reword_incidents(group: &ParallelGroup, code: &str) {
    let changed = group.fixture.db.write().unwrap().execute(
        "UPDATE bpmn_incidents SET message='wording of an earlier release' WHERE instance_id=?1 AND code=?2",
        rusqlite::params![group.instance_id, code]).unwrap();
    assert_eq!(changed, 1, "{code} incident was reworded");
}

#[tokio::test]
async fn an_incident_written_with_other_wording_still_matches_its_stored_row() {
    let group = ParallelGroup::start(
        [service_result("Completed", None, 1), service_result("Completed", None, 2)],
        condition("outputs.missing.deeper")).await;
    group.accept(0).unwrap();
    reword_incidents(&group, "EXPRESSION_ERROR");
    group.accept(1).unwrap_or_else(|error|
        panic!("a sibling was refused over an earlier release's wording: {error:#}"));
    assert_eq!(codes(&group.snapshot()), ["EXPRESSION_ERROR", "EXPRESSION_ERROR"]);

    let group = ParallelGroup::start(
        [service_result("NeedsHuman", Some("REVIEW"), 1), service_result("NeedsHuman", Some("REVIEW"), 2)],
        condition_true()).await;
    group.accept_all(&[0, 1]);
    group.decide(0, false).unwrap();
    reword_incidents(&group, "HUMAN_REJECTED");
    group.decide(1, true).unwrap_or_else(|error|
        panic!("a sibling was refused over an earlier release's wording: {error:#}"));
    assert_eq!(statuses(&group.snapshot()), [Status::AcceptedBlocked, Status::Completed]);
}

#[tokio::test]
async fn a_capacity_closed_group_is_not_parked_again_by_a_sibling_result() {
    let group = ParallelGroup::start(
        [service_result("Error", Some("CODE_A"), 1), service_result("Completed", None, 2)],
        condition_true()).await;
    let mut snapshot = group.snapshot();
    snapshot.repetition_groups[0].terminal_capacity = true;
    let job = snapshot.jobs.iter().find(|job| job.job_id == group.members[0].claim.job.job_id).unwrap();
    // Only a Contract result is checked against its live coordinator beforehand.
    let mut observed = group.members[0].observed.clone();
    observed.origin = repository::ActivityResultOrigin::Envelope;
    let error = runtime::plan_job_result(&snapshot, job, &observed,
        chrono::Utc::now().timestamp_millis(), None).unwrap_err();
    assert!(format!("{error:#}").contains("capacity-closed"), "{error:#}");
}

/// A repeated Service's Error does not climb out of its own scope: only a handler on
/// the repeated coordinator takes it, so with an enclosing handler alone the group
/// is parked under an incident that an operator can see.
#[tokio::test]
async fn an_error_of_a_repeated_service_does_not_climb_to_the_enclosing_sub_process() {
    use tentaflow_protocol::processes::{ProcessErrorDeclaration, ProcessNode};
    let group = ParallelGroup::start_shaped(
        [service_result("Error", Some("CODE_A"), 1), service_result("Completed", None, 2)],
        condition_true(), |model| {
            let mut model = runtime::test_support::embedded_model(model, "Sub");
            model.errors.push(ProcessErrorDeclaration {
                error_id: "Rejected".into(), name: "Rejected".into(), error_code: "CODE_A".into() });
            model.nodes.push(ProcessNode { id: "Handle".into(), name: "Handle".into(),
                kind: ProcessNodeKind::BoundaryError {
                    attached_to_id: "Sub".into(), error_ref: Some("Rejected".into()),
                    output_mapping: Default::default() },
                repeat: None, activity_io: None });
            model.sequence_flows.push(
                runtime::test_support::edge("HandleToEnd", "Handle", "RootEnd_Sub"));
            model
        }).await;
    group.accept_all(&[0, 1]);
    let snapshot = group.snapshot();
    assert_eq!(codes(&snapshot), ["CODE_A"]);
    assert_eq!(snapshot.instance.status, ProcessInstanceStatus::Incident);
    assert_eq!(snapshot.repetition_groups[0].status, ProcessRepetitionGroupStatus::Incident);
    assert_eq!(statuses(&snapshot), [Status::AcceptedBlocked, Status::Completed]);
}

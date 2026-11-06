// ============ File: retry_tests.rs — BPMN B4.4 retry projection: kind and reason per incident, who may retry, one incident per failing activity ============

use tentaflow_protocol::processes::{
    ActivityVerification, ProcessIncidentRetry, ProcessInstance, ProcessInstanceAccess,
    ProcessInstanceStatus, ProcessRetryDenied, ProcessRetryKind,
};

use super::monitor_tests::{is_not_found, member, move_definition_to};
use super::repository::{self, ClaimedProcessJob, ProcessActor, ProcessOperationDenied};
use super::runtime::test_support::*;

fn now() -> i64 {
    super::jobs::now_ms()
}

/// A service task whose worker was interrupted before the outside call: the
/// invocation is proven unexecuted, so the incident is a plain `Job` retry.
fn failed_before_dispatch(fixture: &Fixture, label: &str) -> (ProcessInstance, ClaimedProcessJob) {
    failed_before_dispatch_of(fixture, label, &graph(label, None), None)
}

/// The same, for a flow whose answer is read through `result_expression`.
fn failed_before_dispatch_of(
    fixture: &Fixture,
    label: &str,
    flow_graph: &str,
    result_expression: Option<&str>,
) -> (ProcessInstance, ClaimedProcessJob) {
    let flow_id = flow(&fixture.db, &fixture.owner, flow_graph);
    let mut model = service_model(
        &flow_id,
        ActivityVerification::Condition {
            expression: "true".into(),
        },
    );
    for node in &mut model.nodes {
        if let tentaflow_protocol::processes::ProcessNodeKind::ServiceTask {
            result_expression: slot,
            ..
        } = &mut node.kind
        {
            *slot = result_expression.map(str::to_owned);
        }
    }
    let started = start_model(fixture, &model);
    let claim = repository::claim_job(&fixture.db, label, now())
        .unwrap()
        .unwrap();
    assert!(repository::fail_job(
        &fixture.db,
        &claim.job.job_id,
        claim.job.attempt,
        claim.job.fence,
        label,
        "INTERRUPTED",
        "controlled interruption",
        now(),
        None
    )
    .unwrap());
    (started, claim)
}

fn first_incident(
    fixture: &Fixture,
    actor: &ProcessActor,
    instance_id: &str,
) -> (bool, ProcessIncidentRetry, ProcessInstance) {
    let instance = repository::get_instance(&fixture.db, actor, instance_id, None).unwrap();
    let incident = instance
        .incidents
        .first()
        .expect("an open incident")
        .clone();
    assert!(
        !incident.can_retry
            || (incident.retry.kind == ProcessRetryKind::Job
                && incident.retry.denied_reason.is_none()),
        "an offered retry is a Job retry with no reason: {incident:?}"
    );
    (incident.can_retry, incident.retry, instance)
}

fn job(denied_reason: Option<ProcessRetryDenied>) -> ProcessIncidentRetry {
    ProcessIncidentRetry {
        kind: ProcessRetryKind::Job,
        denied_reason,
    }
}

fn insert_assigned_task(fixture: &Fixture, instance_id: &str, assignee: &ProcessActor) {
    fixture.db.write().unwrap().execute(
        "INSERT INTO bpmn_user_tasks(user_task_id,instance_id,scope_id,node_id,name,assignee_user_id,kind,status,outputs_json,revision,created_at_ms,updated_at_ms) VALUES('assigned-task',?1,?1,'Service','Look',?2,'work','completed','{}',1,1,1)",
        rusqlite::params![instance_id, assignee.user_id]).unwrap();
}

#[tokio::test]
async fn who_is_offered_a_retry_follows_the_ground_of_reading_and_each_refusal_says_why() {
    let fixture = Fixture::new();
    let (started, claim) = failed_before_dispatch(&fixture, "matrix");
    let monitor = member(&fixture, "auditor", "role-org-admin");
    let stranger = member(&fixture, "stranger", "role-org-viewer");
    let assignee = member(&fixture, "assignee", "role-org-viewer");
    insert_assigned_task(&fixture, &started.instance_id, &assignee);
    let other = crate::services::org::repo::create_organization(
        &fixture.db,
        "Other organization",
        "other-organization",
        None,
        None,
        None,
        None,
    )
    .expect("second active organization");
    let outsider_id = crate::db::repository::create_user_account(
        &fixture.db,
        "outsider",
        "test-password-hash",
        "outsider",
        "outsider@example.test",
    )
    .unwrap();
    crate::services::org::repo::add_membership(
        &fixture.db,
        &other.org_id,
        &outsider_id,
        "role-org-admin",
        &outsider_id,
    )
    .unwrap();
    let outsider = ProcessActor {
        org_id: other.org_id.clone(),
        user_id: outsider_id,
    };

    // Initiator who owns the definition: offered, no reason.
    let (can, retry, instance) = first_incident(&fixture, &fixture.owner, &started.instance_id);
    assert_eq!((can, retry), (true, job(None)));
    assert!(instance.can_retry);

    // A monitor and an assignee read it and are told they may only look.
    for (label, actor, access) in [
        ("monitor", &monitor, ProcessInstanceAccess::Monitor),
        ("assignee", &assignee, ProcessInstanceAccess::Assignee),
    ] {
        let (can, retry, instance) = first_incident(&fixture, actor, &started.instance_id);
        assert_eq!(instance.access, Some(access), "{label}");
        assert_eq!(
            (can, retry),
            (false, job(Some(ProcessRetryDenied::ViewOnly))),
            "{label}"
        );
        assert!(!instance.can_retry, "{label}");
        let refused = repository::retry_job(
            &fixture.db,
            actor,
            &stamp(label),
            &started.instance_id,
            &claim.job.job_id,
            instance.revision,
        ).map(|outcome| outcome.instance)
        .unwrap_err();
        assert!(
            refused.downcast_ref::<ProcessOperationDenied>().is_some(),
            "{label}: {refused}"
        );
    }

    // A stranger and a member of another organization cannot tell the instance exists.
    for (label, actor) in [("stranger", &stranger), ("other organization", &outsider)] {
        let error =
            repository::get_instance(&fixture.db, actor, &started.instance_id, None).unwrap_err();
        assert!(is_not_found(&error), "{label}: {error}");
        let error = repository::retry_job(
            &fixture.db,
            actor,
            &stamp(label),
            &started.instance_id,
            &claim.job.job_id,
            instance.revision,
        ).map(|outcome| outcome.instance)
        .unwrap_err();
        assert!(
            is_not_found(&error) && error.downcast_ref::<ProcessOperationDenied>().is_none(),
            "{label}: {error}"
        );
    }

    // The definition moves to another owner: the work would run in the initiator's
    // name, who no longer owns it, so neither the initiator nor the new owner is offered it.
    let steward = member(&fixture, "steward", "role-org-viewer");
    move_definition_to(&fixture, &started.definition_id, &steward);
    for (label, actor, access) in [
        (
            "initiator",
            &fixture.owner,
            ProcessInstanceAccess::Initiator,
        ),
        ("owner", &steward, ProcessInstanceAccess::Owner),
    ] {
        let (can, retry, instance) = first_incident(&fixture, actor, &started.instance_id);
        assert_eq!(instance.access, Some(access), "{label}");
        assert_eq!(
            (can, retry),
            (false, job(Some(ProcessRetryDenied::InitiatorNotOwner))),
            "{label}"
        );
        assert!(!instance.can_retry, "{label}");
        let refused = repository::retry_job(
            &fixture.db,
            actor,
            &stamp(label),
            &started.instance_id,
            &claim.job.job_id,
            instance.revision,
        ).map(|outcome| outcome.instance)
        .unwrap_err();
        assert!(
            refused.downcast_ref::<ProcessOperationDenied>().is_some(),
            "{label}: {refused}"
        );
    }
    let (_, _, after) = first_incident(&fixture, &steward, &started.instance_id);
    assert_eq!(
        (after.revision, after.status),
        (instance.revision, ProcessInstanceStatus::Incident)
    );
}

#[tokio::test]
async fn an_inactive_initiator_and_a_second_ready_step_are_named_as_the_reason() {
    let fixture = Fixture::new();
    let (started, _) = failed_before_dispatch(&fixture, "reasons");
    let steward = member(&fixture, "steward", "role-org-viewer");
    move_definition_to(&fixture, &started.definition_id, &steward);
    move_definition_to(&fixture, &started.definition_id, &fixture.owner);

    fixture.db.write().unwrap().execute(
        "INSERT INTO bpmn_tokens(token_id,instance_id,scope_id,node_id,arrival_edge_id,fork_stack_json,status,created_at_ms) VALUES('stray',?1,?1,'Start_1',NULL,'[]','ready',1)",
        [&started.instance_id]).unwrap();
    assert_eq!(
        first_incident(&fixture, &fixture.owner, &started.instance_id).1,
        job(Some(ProcessRetryDenied::ActivationBusy))
    );
    fixture
        .db
        .write()
        .unwrap()
        .execute("DELETE FROM bpmn_tokens WHERE token_id='stray'", [])
        .unwrap();
    assert_eq!(
        first_incident(&fixture, &fixture.owner, &started.instance_id).1,
        job(None)
    );

    // A flow that went away is named as the source, not as a lost ownership.
    fixture
        .db
        .write()
        .unwrap()
        .execute("UPDATE flows SET status='draft'", [])
        .unwrap();
    assert_eq!(
        first_incident(&fixture, &fixture.owner, &started.instance_id).1,
        job(Some(ProcessRetryDenied::SourceUnavailable))
    );
    fixture
        .db
        .write()
        .unwrap()
        .execute("UPDATE flows SET status='active'", [])
        .unwrap();

    fixture
        .db
        .write()
        .unwrap()
        .execute(
            "UPDATE user_accounts SET is_active=0 WHERE id=?1",
            [&fixture.owner.user_id],
        )
        .unwrap();
    // The inactive initiator cannot read; the owner can, and is told why.
    move_definition_to(&fixture, &started.definition_id, &steward);
    assert_eq!(
        first_incident(&fixture, &steward, &started.instance_id).1,
        job(Some(ProcessRetryDenied::InitiatorUnavailable))
    );
    // The command says the same, typed, so no substring of its text routes it.
    let (_, _, instance) = first_incident(&fixture, &steward, &started.instance_id);
    let (job_id, revision) = (
        instance.incidents[0].job_id.clone().unwrap(),
        instance.revision,
    );
    let refused = repository::retry_job(
        &fixture.db,
        &steward,
        &stamp("inactive initiator"),
        &started.instance_id,
        &job_id,
        revision,
    ).map(|outcome| outcome.instance)
    .unwrap_err();
    assert!(
        refused.downcast_ref::<ProcessOperationDenied>().is_some(),
        "{refused}"
    );
}

#[tokio::test]
async fn a_failed_check_of_the_process_itself_is_named_apart_from_ownership_and_the_source() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("version", None));
    let mut model = service_model(
        &flow_id,
        ActivityVerification::Condition {
            expression: "true".into(),
        },
    );
    model
        .nodes
        .push(tentaflow_protocol::processes::ProcessNode {
            id: "Work".into(),
            name: "Look at it".into(),
            kind: tentaflow_protocol::processes::ProcessNodeKind::UserTask {
                assignee_user_id: Some(fixture.participant.user_id.clone()),
                output_mapping: Default::default(),
            },
            repeat: None,
            activity_io: None,
        });
    model.sequence_flows = vec![
        edge("ToService", "Start_1", "Service"),
        edge("ToWork", "Service", "Work"),
        edge("ToEnd", "Work", "End_1"),
    ];
    let started = start_model(&fixture, &model);
    let claim = repository::claim_job(&fixture.db, "version", now())
        .unwrap()
        .unwrap();
    assert!(repository::fail_job(
        &fixture.db,
        &claim.job.job_id,
        claim.job.attempt,
        claim.job.fence,
        "version",
        "INTERRUPTED",
        "controlled interruption",
        now(),
        None
    )
    .unwrap());
    assert_eq!(
        first_incident(&fixture, &fixture.owner, &started.instance_id).1,
        job(None)
    );

    // A person the process still needs later on is gone: the run cannot go on in the initiator's name.
    fixture
        .db
        .write()
        .unwrap()
        .execute(
            "UPDATE user_accounts SET is_active=0 WHERE id=?1",
            [&fixture.participant.user_id],
        )
        .unwrap();
    assert_eq!(
        first_incident(&fixture, &fixture.owner, &started.instance_id).1,
        job(Some(ProcessRetryDenied::ExecutionUnavailable))
    );
}

#[tokio::test]
async fn a_failure_a_repeat_would_meet_again_is_not_offered_even_when_nothing_ran() {
    let fixture = Fixture::new();
    let (started, _) = failed_before_dispatch(&fixture, "identical");
    for code in [
        "UNBOUND_IMPLEMENTATION",
        "TOOL_ARGUMENTS_INVALID",
        "BLOCK_INPUT_INVALID",
        "AGENT_GOAL_INVALID",
        "TOOL_PERMISSION_DENIED",
    ] {
        fixture
            .db
            .write()
            .unwrap()
            .execute("UPDATE bpmn_incidents SET code=?1", [code])
            .unwrap();
        let (can, retry, _) = first_incident(&fixture, &fixture.owner, &started.instance_id);
        assert_eq!(
            (can, retry),
            (
                false,
                ProcessIncidentRetry {
                    kind: ProcessRetryKind::None,
                    denied_reason: Some(ProcessRetryDenied::SameFailureRepeats)
                }
            ),
            "{code}"
        );
    }
    // A source that went away is state, not a certainty: it is offered again once it is back.
    fixture
        .db
        .write()
        .unwrap()
        .execute("UPDATE bpmn_incidents SET code='SOURCE_ACCESS_REVOKED'", [])
        .unwrap();
    assert_eq!(
        first_incident(&fixture, &fixture.owner, &started.instance_id).1,
        job(None)
    );
}

#[tokio::test]
async fn an_unknown_outcome_is_classified_for_the_owner_and_the_command_refuses_it_typed() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("uncertain", None));
    let started = start_model(
        &fixture,
        &service_model(
            &flow_id,
            ActivityVerification::Condition {
                expression: "true".into(),
            },
        ),
    );
    let claim = repository::claim_job(&fixture.db, "uncertain", now())
        .unwrap()
        .unwrap();
    assert!(
        repository::commit_job_dispatch_boundary(&fixture.db, &claim, "uncertain", now()).unwrap()
    );
    assert!(repository::fail_job(
        &fixture.db,
        &claim.job.job_id,
        claim.job.attempt,
        claim.job.fence,
        "uncertain",
        "INTERRUPTED",
        "worker stopped after the call",
        now(),
        None
    )
    .unwrap());

    let monitor = member(&fixture, "auditor", "role-org-admin");
    let (can, retry, instance) = first_incident(&fixture, &fixture.owner, &started.instance_id);
    assert_eq!(instance.incidents[0].code, "EXTERNAL_OUTCOME_UNCERTAIN");
    assert_eq!(
        (can, retry),
        (
            false,
            ProcessIncidentRetry {
                kind: ProcessRetryKind::JobUncertain,
                denied_reason: Some(ProcessRetryDenied::OutcomeUncertain)
            }
        )
    );
    assert_eq!(
        first_incident(&fixture, &monitor, &started.instance_id).1,
        ProcessIncidentRetry {
            kind: ProcessRetryKind::JobUncertain,
            denied_reason: Some(ProcessRetryDenied::ViewOnly)
        }
    );
    assert!(!instance.can_retry);

    let before = now();
    let refused = repository::retry_job(
        &fixture.db,
        &fixture.owner,
        &stamp("repeat an unknown outcome"),
        &started.instance_id,
        &claim.job.job_id,
        instance.revision,
    ).map(|outcome| outcome.instance)
    .unwrap_err();
    assert!(
        refused.downcast_ref::<ProcessOperationDenied>().is_some(),
        "{refused}"
    );
    assert!(
        repository::claim_job(&fixture.db, "uncertain-again", before)
            .unwrap()
            .is_none(),
        "a refused retry queues nothing, so the call is never repeated"
    );
}

#[test]
fn each_incident_code_shows_the_kind_and_reason_of_its_class_to_the_owner() {
    use ProcessRetryDenied as Denied;
    use ProcessRetryKind as Kind;
    let fixture = Fixture::new();
    let started = start_model(&fixture, &user_model(Some(&fixture.participant.user_id)));
    let expectations = [
        (
            "EXTERNAL_RESULT_NOT_APPLIED",
            Kind::JobUncertain,
            Some(Denied::OutcomeUncertain),
        ),
        ("ACTIVITY_IO_OUTPUT_FAILED", Kind::OutputMapping, Some(Denied::ResultNotRetained)),
        ("SCRIPT_MAPPING_FAILED", Kind::None, Some(Denied::SameFailureRepeats)),
        ("SERVICE_ERROR", Kind::Job, Some(Denied::NoProofOfNoEffect)),
        (
            "UNBOUND_IMPLEMENTATION",
            Kind::None,
            Some(Denied::SameFailureRepeats),
        ),
        (
            "TOOL_ARGUMENTS_INVALID",
            Kind::None,
            Some(Denied::SameFailureRepeats),
        ),
        (
            "BLOCK_INPUT_INVALID",
            Kind::None,
            Some(Denied::SameFailureRepeats),
        ),
        (
            "AGENT_GOAL_INVALID",
            Kind::None,
            Some(Denied::SameFailureRepeats),
        ),
        (
            "TOOL_PERMISSION_DENIED",
            Kind::None,
            Some(Denied::SameFailureRepeats),
        ),
        (
            "TOOL_RESULT_UNDECIDED",
            Kind::None,
            Some(Denied::NeedsAdministrator),
        ),
        (
            "SOURCE_ACCESS_REVOKED",
            Kind::None,
            Some(Denied::SourceUnavailable),
        ),
        (
            "ACTIVITY_IO_INPUT_FAILED",
            Kind::None,
            Some(Denied::SameFailureRepeats),
        ),
        (
            "NO_MATCHING_FLOW",
            Kind::None,
            Some(Denied::SameFailureRepeats),
        ),
        (
            "AMBIGUOUS_GATEWAY",
            Kind::None,
            Some(Denied::SameFailureRepeats),
        ),
        (
            "EXPRESSION_ERROR",
            Kind::None,
            Some(Denied::SameFailureRepeats),
        ),
        (
            "IMMEDIATE_TRANSITION_LIMIT",
            Kind::None,
            Some(Denied::SameFailureRepeats),
        ),
        ("VERIFICATION_FAILED", Kind::None, Some(Denied::NeedsPerson)),
        ("HUMAN_REJECTED", Kind::None, Some(Denied::NeedsPerson)),
        ("STORE_BUSY", Kind::None, Some(Denied::NeedsAdministrator)),
        ("TIMER_ERROR", Kind::None, Some(Denied::OtherMechanism)),
        ("CALL_CHILD_ERROR", Kind::None, Some(Denied::OtherMechanism)),
        ("A_CODE_NOBODY_CLASSIFIED", Kind::None, None),
    ];
    {
        let conn = fixture.db.write().unwrap();
        for (index, (code, _, _)) in expectations.iter().enumerate() {
            conn.execute(
                "INSERT INTO bpmn_incidents(incident_id,instance_id,scope_id,node_id,job_id,code,message,at_ms) VALUES(?1,?2,?2,NULL,NULL,?3,'m',?4)",
                rusqlite::params![format!("incident-{index:02}"), started.instance_id, code, 10 + index as i64],
            ).unwrap();
        }
    }
    let mut listed = Vec::new();
    for offset in [0, 20] {
        let pages: tentaflow_protocol::processes::ProcessInstancePageRequest =
            serde_json::from_value(serde_json::json!({"incidents":{"offset":offset,"limit":20}}))
                .unwrap();
        listed.extend(
            repository::get_instance(
                &fixture.db,
                &fixture.owner,
                &started.instance_id,
                Some(&pages),
            )
            .unwrap()
            .incidents,
        );
    }
    for (index, (code, kind, reason)) in expectations.iter().enumerate() {
        let incident = listed
            .iter()
            .find(|incident| incident.incident_id == format!("incident-{index:02}"))
            .unwrap_or_else(|| panic!("{code} listed"));
        assert_eq!(
            incident.retry,
            ProcessIncidentRetry {
                kind: *kind,
                denied_reason: *reason
            },
            "{code}"
        );
        assert!(!incident.can_retry, "{code} has no job to retry");
    }
}

#[tokio::test]
async fn the_same_command_twice_makes_one_job() {
    let fixture = Fixture::new();
    let (started, claim) = failed_before_dispatch(&fixture, "idempotent");
    let instance =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    let command = stamp("retry once");
    let first = repository::retry_job(
        &fixture.db,
        &fixture.owner,
        &command,
        &started.instance_id,
        &claim.job.job_id,
        instance.revision,
    ).map(|outcome| outcome.instance)
    .unwrap();
    let second = repository::retry_job(
        &fixture.db,
        &fixture.owner,
        &command,
        &started.instance_id,
        &claim.job.job_id,
        instance.revision,
    ).map(|outcome| outcome.instance)
    .unwrap();
    assert_eq!(
        (first.revision, first.status),
        (second.revision, second.status)
    );
    let jobs: i64 = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM bpmn_jobs WHERE instance_id=?1",
            [&started.instance_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(jobs, 2, "the failed job and exactly one retry");
    let retried: i64 = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1 AND kind='job_retried'",
            [&started.instance_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(retried, 1);
}

#[tokio::test]
async fn a_retry_that_fails_again_keeps_the_one_incident_and_queues_no_second_notice() {
    let fixture = Fixture::new();
    let (started, first_claim) = failed_before_dispatch(&fixture, "again");
    let incident_rows = |fixture: &Fixture| -> Vec<(String, Option<String>, bool)> {
        let conn = fixture.db.read().unwrap();
        let mut statement = conn.prepare(
            "SELECT incident_id,job_id,resolved_at_ms IS NULL FROM bpmn_incidents WHERE instance_id=?1 ORDER BY at_ms,incident_id").unwrap();
        statement
            .query_map([&started.instance_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    let notice_rows = |fixture: &Fixture| -> (i64, i64) {
        fixture.db.read().unwrap().query_row(
            "SELECT COUNT(*),COUNT(*) FILTER (WHERE notified_at_ms IS NULL) FROM bpmn_incident_notices", [],
            |row| Ok((row.get(0)?, row.get(1)?))).unwrap()
    };
    let original = incident_rows(&fixture);
    assert_eq!(original.len(), 1);
    let (total_notices, _) = notice_rows(&fixture);
    // The person was told the first time.
    fixture
        .db
        .write()
        .unwrap()
        .execute("UPDATE bpmn_incident_notices SET notified_at_ms=5", [])
        .unwrap();

    let mut job_id = first_claim.job.job_id.clone();
    for attempt in 1..=3 {
        let instance =
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None)
                .unwrap();
        assert_eq!(instance.incidents.len(), 1, "attempt {attempt}");
        assert!(instance.incidents[0].can_retry, "attempt {attempt}");
        repository::retry_job(
            &fixture.db,
            &fixture.owner,
            &stamp(&format!("retry {attempt}")),
            &started.instance_id,
            &job_id,
            instance.revision,
        ).map(|outcome| outcome.instance)
        .unwrap();
        let during = incident_rows(&fixture);
        assert_eq!(
            during.len(),
            1,
            "the retry closes the incident, it adds none"
        );
        assert!(!during[0].2, "closed while the new attempt is queued");

        let claim = repository::claim_job(&fixture.db, "again", now())
            .unwrap()
            .unwrap();
        assert_ne!(claim.job.job_id, job_id);
        assert!(repository::fail_job(
            &fixture.db,
            &claim.job.job_id,
            claim.job.attempt,
            claim.job.fence,
            "again",
            "INTERRUPTED",
            "interrupted once more",
            now(),
            None
        )
        .unwrap());
        let after = incident_rows(&fixture);
        assert_eq!(after.len(), 1, "attempt {attempt}: still one incident row");
        assert_eq!(after[0].0, original[0].0, "the same incident, reopened");
        assert_eq!(
            after[0].1.as_deref(),
            Some(claim.job.job_id.as_str()),
            "pointing at the newest attempt"
        );
        assert!(after[0].2, "open again");
        job_id = claim.job.job_id;
    }
    assert_eq!(
        notice_rows(&fixture),
        (total_notices, 0),
        "no new notice row, nothing pending"
    );
    let events: i64 = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1 AND kind='job_retried'",
            [&started.instance_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(events, 3);
    let last = first_incident(&fixture, &fixture.owner, &started.instance_id);
    assert_eq!(
        (last.0, last.1),
        (true, job(None)),
        "and the newest failure is retryable again"
    );
}

#[test]
fn the_wire_form_of_an_incident_without_a_retry_decodes_to_no_class() {
    let old = serde_json::json!({
        "incident_id":"i","node_id":null,"node_name":null,"job_id":null,"code":"X","message":"m",
        "at_ms":1,"can_retry":false,"scope_id":"s"});
    let decoded: tentaflow_protocol::processes::ProcessIncident =
        serde_json::from_value(old).unwrap();
    assert_eq!(decoded.retry, ProcessIncidentRetry::default());
    let current = serde_json::to_value(&decoded).unwrap();
    assert_eq!(current["retry"], serde_json::json!({"kind":"None"}));
    let strict =
        serde_json::from_value::<ProcessIncidentRetry>(serde_json::json!({"kind":"Job","extra":1}));
    assert!(strict.is_err(), "a view struct refuses unknown fields");
}

fn incident_rows(
    fixture: &Fixture,
    instance_id: &str,
) -> Vec<(String, Option<String>, bool, String, i64)> {
    let conn = fixture.db.read().unwrap();
    let mut statement = conn.prepare(
        "SELECT incident_id,job_id,resolved_at_ms IS NULL,code,at_ms FROM bpmn_incidents WHERE instance_id=?1 ORDER BY incident_id").unwrap();
    statement
        .query_map([instance_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn notice_count(fixture: &Fixture) -> i64 {
    fixture
        .db
        .read()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM bpmn_incident_notices", [], |row| {
            row.get(0)
        })
        .unwrap()
}

/// The first failure is old and its notice was delivered; a retry is started.
fn retried_after_a_delivered_notice(
    fixture: &Fixture,
    label: &str,
    flow_graph: &str,
    result_expression: Option<&str>,
) -> (ProcessInstance, String, ClaimedProcessJob, i64) {
    let (started, first) = failed_before_dispatch_of(fixture, label, flow_graph, result_expression);
    fixture
        .db
        .write()
        .unwrap()
        .execute("UPDATE bpmn_incidents SET at_ms=1", [])
        .unwrap();
    fixture
        .db
        .write()
        .unwrap()
        .execute("UPDATE bpmn_incident_notices SET notified_at_ms=5", [])
        .unwrap();
    let notices = notice_count(fixture);
    let incident_id = incident_rows(fixture, &started.instance_id)[0].0.clone();
    let instance =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    repository::retry_job(
        &fixture.db,
        &fixture.owner,
        &stamp("retry once"),
        &started.instance_id,
        &first.job.job_id,
        instance.revision,
    ).map(|outcome| outcome.instance)
    .unwrap();
    let claim = repository::claim_job(&fixture.db, label, now())
        .unwrap()
        .unwrap();
    assert_ne!(claim.job.job_id, first.job.job_id);
    (started, incident_id, claim, notices)
}

#[tokio::test]
async fn a_retry_that_fails_after_the_call_reopens_the_one_incident_as_uncertain_without_a_notice()
{
    let fixture = Fixture::new();
    let (started, incident_id, claim, notices) =
        retried_after_a_delivered_notice(&fixture, "after-call", &graph("after-call", None), None);
    assert!(
        repository::commit_job_dispatch_boundary(&fixture.db, &claim, "after-call", now()).unwrap()
    );
    assert!(repository::fail_job(
        &fixture.db,
        &claim.job.job_id,
        claim.job.attempt,
        claim.job.fence,
        "after-call",
        "INTERRUPTED",
        "worker stopped after the call",
        now(),
        None
    )
    .unwrap());

    let rows = incident_rows(&fixture, &started.instance_id);
    assert_eq!(rows.len(), 1, "still one incident row: {rows:?}");
    assert_eq!(
        (
            rows[0].0.as_str(),
            rows[0].1.as_deref(),
            rows[0].2,
            rows[0].3.as_str()
        ),
        (
            incident_id.as_str(),
            Some(claim.job.job_id.as_str()),
            true,
            "EXTERNAL_OUTCOME_UNCERTAIN"
        )
    );
    assert!(rows[0].4 > 1, "the time moved to the newest failure");
    assert_eq!(notice_count(&fixture), notices, "no notice row added");
    let pending: i64 = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM bpmn_incident_notices WHERE notified_at_ms IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pending, 0, "and nothing to deliver again");
    let uncertain: String = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT uncertainty_incident_id FROM bpmn_service_invocations WHERE job_id=?1",
            [&claim.job.job_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        uncertain, incident_id,
        "the invocation names the reopened incident"
    );
    assert_eq!(
        first_incident(&fixture, &fixture.owner, &started.instance_id).1,
        ProcessIncidentRetry {
            kind: ProcessRetryKind::JobUncertain,
            denied_reason: Some(ProcessRetryDenied::OutcomeUncertain)
        }
    );
}

#[tokio::test]
async fn a_retry_whose_result_is_an_accepted_error_reopens_the_one_incident_and_is_the_steps_own_answer(
) {
    let fixture = Fixture::new();
    let (started, incident_id, claim, notices) = retried_after_a_delivered_notice(
        &fixture,
        "accepted-error",
        &business_graph(),
        Some("outputs.variables.actual_result"),
    );
    assert!(
        repository::commit_job_dispatch_boundary(&fixture.db, &claim, "accepted-error", now())
            .unwrap()
    );
    // The step answers with a code that happens to be spelled like an engine one.
    let observed = repository::ObservedActivityResult {
        origin: repository::ActivityResultOrigin::Contract,
        expression_observation: None,
        result: tentaflow_protocol::processes::ActivityResult {
            outcome: tentaflow_protocol::processes::ActivityOutcome::Error,
            code: Some("EXTERNAL_OUTCOME_UNCERTAIN".into()),
            summary: "the contract was refused".into(),
            outputs: serde_json::Value::Null,
            evidence: Vec::new(),
        },
        verification: None,
    };
    assert_eq!(
        repository::record_job_observation(&fixture.db, &claim, "accepted-error", &observed, now())
            .unwrap(),
        repository::JobObservationState::Observed
    );
    let at = now();
    let plan = plan_recorded_result(&fixture, &claim, &observed, at).unwrap();
    repository::accept_job_result(
        &fixture.db,
        &fixture.owner,
        &claim.job.job_id,
        claim.job.attempt,
        claim.job.fence,
        "accepted-error",
        &observed,
        claim.snapshot.instance.revision,
        repository::ProcessPlanInput::Supplied(&plan),
        at,
    )
    .unwrap();

    let rows = incident_rows(&fixture, &started.instance_id);
    assert_eq!(rows.len(), 1, "still one incident row: {rows:?}");
    assert_eq!(
        (
            rows[0].0.as_str(),
            rows[0].1.as_deref(),
            rows[0].2,
            rows[0].3.as_str()
        ),
        (
            incident_id.as_str(),
            Some(claim.job.job_id.as_str()),
            true,
            "EXTERNAL_OUTCOME_UNCERTAIN"
        )
    );
    assert!(rows[0].4 > 1, "the time moved to the newest failure");
    assert_eq!(notice_count(&fixture), notices, "no notice row added");
    let referenced: i64 = fixture.db.read().unwrap().query_row(
        "SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1 AND kind='incident' AND json_extract(data_json,'$.incident_id')<>?2",
        rusqlite::params![started.instance_id, incident_id], |row| row.get(0)).unwrap();
    assert_eq!(referenced, 0, "no event names another incident id");
    // The code is the step's answer, so it says nothing about the engine's own outcome.
    assert_eq!(
        first_incident(&fixture, &fixture.owner, &started.instance_id).1,
        job(Some(ProcessRetryDenied::NoProofOfNoEffect))
    );
}

/// A flow whose pinned output contract is the business answer `actual_result`.
fn business_graph() -> String {
    let answer = serde_json::json!({"outcome":"Error","code":"EXTERNAL_OUTCOME_UNCERTAIN",
        "summary":"the contract was refused","outputs":{},"evidence":[]});
    serde_json::json!({"nodes":[{"id":"trigger","type":"trigger","config":{"output_mapping":{"actual_result":answer.to_string()}}},
        {"id":"output","type":"output","config":{}}],
        "edges":[{"from":"trigger","to":"output","from_port":"text","to_port":"text"}],
        "variables":[{"name":"actual_result","type":"json"}]}).to_string()
}

/// A retried attempt whose result was recorded but never applied.
async fn observed_after_retry(fixture: &Fixture, label: &str)
    -> (ProcessInstance, String, ClaimedProcessJob, i64)
{
    let (started, incident_id, claim, notices) = retried_after_a_delivered_notice(
        fixture, label, &business_graph(), Some("outputs.variables.actual_result"));
    assert!(repository::commit_job_dispatch_boundary(&fixture.db, &claim, label, now()).unwrap());
    let observed = repository::ObservedActivityResult {
        origin: repository::ActivityResultOrigin::Contract,
        expression_observation: None,
        result: tentaflow_protocol::processes::ActivityResult {
            outcome: tentaflow_protocol::processes::ActivityOutcome::Completed,
            code: None,
            summary: "done".into(),
            outputs: serde_json::Value::Null,
            evidence: Vec::new(),
        },
        verification: None,
    };
    assert_eq!(repository::record_job_observation(&fixture.db, &claim, label, &observed, now()).unwrap(),
        repository::JobObservationState::Observed);
    (started, incident_id, claim, notices)
}

fn assert_the_one_incident_reopened(fixture: &Fixture, started: &ProcessInstance, incident_id: &str,
    claim: &ClaimedProcessJob, notices: i64, code: &str)
{
    let rows = incident_rows(fixture, &started.instance_id);
    assert_eq!(rows.len(), 1, "still one incident row: {rows:?}");
    assert_eq!((rows[0].0.as_str(), rows[0].1.as_deref(), rows[0].2, rows[0].3.as_str()),
        (incident_id, Some(claim.job.job_id.as_str()), true, code));
    assert!(rows[0].4 > 1, "the time moved to the newest failure");
    assert_eq!(notice_count(fixture), notices, "no notice row added");
    let pending: i64 = fixture.db.read().unwrap().query_row(
        "SELECT COUNT(*) FROM bpmn_incident_notices WHERE notified_at_ms IS NULL", [], |row| row.get(0)).unwrap();
    assert_eq!(pending, 0, "and nothing to deliver again");
}

#[tokio::test]
async fn a_retried_attempt_whose_recovery_keeps_failing_reopens_the_one_incident() {
    let fixture = Fixture::new();
    let (started, incident_id, claim, notices) = observed_after_retry(&fixture, "recovery-failed").await;
    repository::report_recovery_failure(&fixture.db, &claim.job.job_id, &anyhow::anyhow!("not recoverable"), now(), 1);
    assert_the_one_incident_reopened(&fixture, &started, &incident_id, &claim, notices, "RESULT_RECOVERY_FAILED");
}

#[tokio::test]
async fn a_retried_attempt_whose_tool_result_stays_undecided_reopens_the_one_incident() {
    let fixture = Fixture::new();
    let (started, incident_id, claim, notices) = observed_after_retry(&fixture, "undecided").await;
    repository::record_undecided_batch(&fixture.db, &[(claim.job.job_id.clone(), true)], now());
    assert_the_one_incident_reopened(&fixture, &started, &incident_id, &claim, notices, "TOOL_RESULT_UNDECIDED");
}

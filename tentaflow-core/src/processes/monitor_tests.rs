// ============ File: monitor_tests.rs — BPMN B4.1 monitor reader: access grounds, redaction, list filters, token view ============

use serde_json::json;
use tentaflow_protocol::processes::{
    ActivityVerification, ProcessInstanceAccess, ProcessInstanceListScope, ProcessInstanceStatus,
    ProcessNode, ProcessNodeKind, ProcessTokenStatus, ProcessTokenWait,
};

use super::repository::{self, InstanceFilter, ProcessActor, ProcessAuthorityDenied, ProcessOperationDenied};
use super::runtime::test_support::*;

fn member(fixture: &Fixture, name: &str, role_id: &str) -> ProcessActor {
    let id = crate::db::repository::create_user_account(
        &fixture.db, name, "test-password-hash", name, &format!("{name}@example.test"),
    ).expect("create process user");
    crate::services::org::repo::add_membership(
        &fixture.db, crate::services::org::DEFAULT_ORG_ID, &id, role_id, &id,
    ).expect("grant organization membership");
    ProcessActor { org_id: crate::services::org::DEFAULT_ORG_ID.into(), user_id: id }
}

/// The product lets only the owner start an instance, so a different owner is
/// reached by moving the definition in the database.
fn move_definition_to(fixture: &Fixture, definition_id: &str, owner: &ProcessActor) {
    fixture.db.write().unwrap().execute(
        "UPDATE bpmn_definitions SET owner_user_id=?1 WHERE definition_id=?2",
        rusqlite::params![owner.user_id, definition_id],
    ).unwrap();
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error.downcast_ref::<ProcessAuthorityDenied>().is_some()
        && error.to_string().contains("not found")
}

fn waiting_instance(fixture: &Fixture) -> tentaflow_protocol::processes::ProcessInstance {
    let mut model = user_model(Some(&fixture.participant.user_id));
    model.variables.insert("customer_note".into(), json!("hunter2"));
    model.variables.insert("amount".into(), json!(42));
    start_model(fixture, &model)
}

#[test]
fn the_definition_owner_reads_a_foreign_instance_with_values_and_a_stranger_cannot() {
    let fixture = Fixture::new();
    let started = waiting_instance(&fixture);
    let steward = member(&fixture, "steward", "role-org-viewer");
    let stranger = member(&fixture, "stranger", "role-org-viewer");
    move_definition_to(&fixture, &started.definition_id, &steward);

    let seen = repository::get_instance(&fixture.db, &steward, &started.instance_id, None).unwrap();
    assert_eq!(seen.access, Some(ProcessInstanceAccess::Owner));
    assert!(!seen.variables_redacted);
    assert_eq!(seen.variables["customer_note"], "hunter2");
    assert!(!seen.can_cancel, "the owner reads and retries, only the initiator cancels");
    assert_eq!(repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None)
        .unwrap().access, Some(ProcessInstanceAccess::Initiator));
    assert_eq!(repository::get_instance(&fixture.db, &fixture.participant, &started.instance_id, None)
        .unwrap().access, Some(ProcessInstanceAccess::Assignee));

    let events = repository::list_events(&fixture.db, &steward, &started.instance_id, 0, 50).unwrap().0;
    assert!(events.iter().any(|event| !event.data.is_null()));
    repository::get_scope(&fixture.db, &steward, &started.instance_id, &started.instance_id).unwrap();
    repository::get_instance_tokens(&fixture.db, &steward, &started.instance_id, None, None).unwrap();

    for error in [
        repository::get_instance(&fixture.db, &stranger, &started.instance_id, None).unwrap_err(),
        repository::list_events(&fixture.db, &stranger, &started.instance_id, 0, 50).unwrap_err(),
        repository::get_scope(&fixture.db, &stranger, &started.instance_id, &started.instance_id).unwrap_err(),
        repository::get_instance_tokens(&fixture.db, &stranger, &started.instance_id, None, None).unwrap_err(),
    ] {
        assert!(is_not_found(&error), "{error}");
    }
}

#[test]
fn a_monitor_reads_names_and_types_never_values_and_cannot_operate() {
    let fixture = Fixture::new();
    let started = waiting_instance(&fixture);
    let monitor = member(&fixture, "auditor", "role-org-admin");

    let seen = repository::get_instance(&fixture.db, &monitor, &started.instance_id, None).unwrap();
    assert_eq!(seen.access, Some(ProcessInstanceAccess::Monitor));
    assert!(seen.variables_redacted);
    assert_eq!(seen.variables, json!({"customer_note": "string", "amount": "number"}));
    assert!(!seen.can_cancel && !seen.can_retry);
    assert!(!serde_json::to_string(&seen).unwrap().contains("hunter2"));

    let (_, variables, _, scope_access) = repository::get_scope(
        &fixture.db, &monitor, &started.instance_id, &started.instance_id).unwrap();
    assert_eq!(scope_access, ProcessInstanceAccess::Monitor);
    assert_eq!(variables, json!({"customer_note": "string", "amount": "number"}));
    let (events, _, _, access) = repository::list_events(&fixture.db, &monitor, &started.instance_id, 0, 50).unwrap();
    assert_eq!(access, ProcessInstanceAccess::Monitor);
    assert!(!events.is_empty() && events.iter().all(|event| event.data.is_null()));

    let retry = repository::retry_job(&fixture.db, &monitor, &stamp("monitor retry"),
        &started.instance_id, "any-job", seen.revision).unwrap_err();
    assert!(retry.downcast_ref::<ProcessOperationDenied>().is_some(), "{retry}");
    assert!(repository::cancel_instance(&fixture.db, &monitor, &stamp("monitor cancel"),
        &started.instance_id, seen.revision).is_err());
    let task = repository::get_user_task(&fixture.db, &monitor, &started.instance_id,
        &started.user_tasks[0].user_task_id).unwrap_err();
    assert!(task.to_string().contains("not found"), "{task}");
}

#[test]
fn processes_monitor_is_read_from_the_stored_role_not_assumed() {
    let fixture = Fixture::new();
    let started = waiting_instance(&fixture);
    let viewer = member(&fixture, "plain-viewer", "role-org-viewer");
    assert!(is_not_found(&repository::get_instance(
        &fixture.db, &viewer, &started.instance_id, None).unwrap_err()));
    assert!(repository::list_instances(&fixture.db, &viewer, &InstanceFilter {
        scope: Some(ProcessInstanceListScope::Monitor), ..InstanceFilter::default()
    }, 0, 10).unwrap_err().downcast_ref::<ProcessOperationDenied>().is_some());

    // The same role with the grant added reads the instance as a monitor.
    fixture.db.write().unwrap().execute(
        "UPDATE roles SET permissions_json=json_insert(permissions_json,'$[#]','processes.monitor') WHERE role_id='role-org-viewer'",
        [],
    ).unwrap();
    assert_eq!(repository::get_instance(&fixture.db, &viewer, &started.instance_id, None).unwrap().access,
        Some(ProcessInstanceAccess::Monitor));
}

#[test]
fn list_scopes_and_filters_select_the_expected_instances() {
    let fixture = Fixture::new();
    let first = waiting_instance(&fixture);
    let steward = member(&fixture, "steward", "role-org-viewer");
    let monitor = member(&fixture, "auditor", "role-org-admin");
    move_definition_to(&fixture, &first.definition_id, &steward);

    let ids = |actor: &ProcessActor, filter: InstanceFilter| -> Vec<String> {
        repository::list_instances(&fixture.db, actor, &filter, 0, 50).unwrap().0
            .into_iter().map(|item| item.instance_id).collect()
    };
    let scoped = |scope| InstanceFilter { scope: Some(scope), ..InstanceFilter::default() };

    assert_eq!(ids(&fixture.owner, InstanceFilter::default()), vec![first.instance_id.clone()]);
    assert_eq!(ids(&fixture.participant, InstanceFilter::default()), vec![first.instance_id.clone()]);
    assert!(ids(&steward, InstanceFilter::default()).is_empty());
    assert_eq!(ids(&steward, scoped(ProcessInstanceListScope::Owned)), vec![first.instance_id.clone()]);
    assert!(ids(&fixture.owner, scoped(ProcessInstanceListScope::Owned)).is_empty());
    assert!(ids(&monitor, InstanceFilter::default()).is_empty());
    let (rows, total, _) = repository::list_instances(
        &fixture.db, &monitor, &scoped(ProcessInstanceListScope::Monitor), 0, 50).unwrap();
    assert_eq!((rows.len(), total), (1, 1));
    assert_eq!(rows[0].access, Some(ProcessInstanceAccess::Monitor));
    assert!(!rows[0].can_cancel && !rows[0].can_retry);

    let monitored = |filter: InstanceFilter| ids(&monitor, InstanceFilter {
        scope: Some(ProcessInstanceListScope::Monitor), ..filter });
    assert_eq!(monitored(InstanceFilter::of_definition(&first.definition_id)).len(), 1);
    assert!(monitored(InstanceFilter::of_definition("another-definition")).is_empty());
    assert_eq!(monitored(InstanceFilter { version: Some(first.version), ..Default::default() }).len(), 1);
    assert!(monitored(InstanceFilter { version: Some(first.version + 1), ..Default::default() }).is_empty());
    assert_eq!(monitored(InstanceFilter { status: Some(first.status.clone()), ..Default::default() }).len(), 1);
    assert!(monitored(InstanceFilter { status: Some(ProcessInstanceStatus::Completed), ..Default::default() })
        .is_empty());
    assert!(monitored(InstanceFilter { has_incident: Some(true), ..Default::default() }).is_empty());
    assert_eq!(monitored(InstanceFilter { has_incident: Some(false), ..Default::default() }).len(), 1);
}

#[tokio::test]
async fn can_retry_is_one_projection_for_the_detail_the_incident_and_the_list() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("monitor retry", None));
    let model = service_model(&flow_id, ActivityVerification::Condition { expression: "true".into() });
    let started = start_model(&fixture, &model);
    let claim = repository::claim_job(&fixture.db, "monitor-worker", super::jobs::now_ms())
        .unwrap().unwrap();
    assert!(repository::fail_job(&fixture.db, &claim.job.job_id, claim.job.attempt, claim.job.fence,
        "monitor-worker", "INTERRUPTED", "controlled interruption", super::jobs::now_ms(), None).unwrap());

    let steward = member(&fixture, "steward", "role-org-viewer");
    let monitor = member(&fixture, "auditor", "role-org-admin");
    move_definition_to(&fixture, &started.definition_id, &steward);

    let list_flag = |actor: &ProcessActor, scope| {
        repository::list_instances(&fixture.db, actor, &InstanceFilter {
            scope: Some(scope), ..InstanceFilter::default() }, 0, 10).unwrap().0
            .into_iter().find(|item| item.instance_id == started.instance_id).unwrap().can_retry
    };
    let detail = |actor: &ProcessActor| {
        let instance = repository::get_instance(&fixture.db, actor, &started.instance_id, None).unwrap();
        assert_eq!(instance.status, ProcessInstanceStatus::Incident);
        (instance.can_retry, instance.incidents[0].can_retry)
    };

    // The work runs in the initiator's name and a worker claims it only for the
    // owner of the definition, so once the definition moved on nobody can retry.
    assert_eq!(detail(&fixture.owner), (false, false));
    assert_eq!(detail(&steward), (false, false));
    assert_eq!(detail(&monitor), (false, false));
    assert!(!list_flag(&fixture.owner, ProcessInstanceListScope::Mine));
    assert!(!list_flag(&steward, ProcessInstanceListScope::Owned));
    assert!(!list_flag(&monitor, ProcessInstanceListScope::Monitor));
    let participant = repository::get_instance(&fixture.db, &fixture.participant, &started.instance_id, None);
    assert!(participant.is_err(), "a reader without a task of this instance has no view");

    // The projection and the command agree: the refusal is typed, not a not-found.
    let revision = repository::get_instance(&fixture.db, &steward, &started.instance_id, None).unwrap().revision;
    for asker in [&steward, &fixture.owner] {
        let refused = repository::retry_job(&fixture.db, asker, &stamp("retry after the move"),
            &started.instance_id, &claim.job.job_id, revision).unwrap_err();
        assert!(refused.downcast_ref::<ProcessOperationDenied>().is_some()
            || refused.downcast_ref::<ProcessAuthorityDenied>().is_some(), "{refused}");
    }
    let unchanged = repository::get_instance(&fixture.db, &steward, &started.instance_id, None).unwrap();
    assert_eq!((unchanged.revision, unchanged.status), (revision, ProcessInstanceStatus::Incident));
}

#[test]
fn tokens_report_the_wait_the_path_taken_and_answer_not_modified_by_revision() {
    let fixture = Fixture::new();
    let started = waiting_instance(&fixture);
    let monitor = member(&fixture, "auditor", "role-org-admin");

    let view = repository::get_instance_tokens(&fixture.db, &monitor, &started.instance_id, None, None).unwrap();
    assert!(!view.not_modified && !view.truncated);
    assert_eq!((view.version, view.instance_revision), (started.version, started.revision));
    assert_eq!(view.tokens.len(), 1);
    let token = &view.tokens[0];
    assert_eq!((token.node_id.as_str(), token.status), ("Work", ProcessTokenStatus::Waiting));
    assert_eq!(token.scope_id, started.instance_id);
    assert_eq!(token.wait, Some(ProcessTokenWait::UserTask {
        user_task_id: started.user_tasks[0].user_task_id.clone() }));
    assert!(token.incident_ids.is_empty() && token.occurrence.is_none());
    assert_eq!(view.taken_edge_ids, vec!["ToWork".to_string()]);
    assert_eq!(view.visited_node_ids, vec!["Start_1".to_string(), "Work".to_string()]);

    let same = repository::get_instance_tokens(
        &fixture.db, &monitor, &started.instance_id, None, Some(started.revision)).unwrap();
    assert!(same.not_modified);
    assert_eq!(same.instance_revision, started.revision);
    assert!(same.tokens.is_empty() && same.taken_edge_ids.is_empty() && same.visited_node_ids.is_empty());
    let stale = repository::get_instance_tokens(
        &fixture.db, &monitor, &started.instance_id, None, Some(started.revision + 1)).unwrap();
    assert!(!stale.not_modified && stale.tokens.len() == 1);

    // Finishing the task moves the revision, so the cached answer is stale at once.
    repository::complete_user_task(
        &fixture.db, &fixture.participant, &stamp("finish"), &started.instance_id,
        &started.user_tasks[0].user_task_id, started.revision, &json!({"answer": "ok"}), None,
        repository::ProcessPlanInput::Canonical, super::jobs::now_ms(),
    ).unwrap();
    let after = repository::get_instance_tokens(
        &fixture.db, &monitor, &started.instance_id, None, Some(started.revision)).unwrap();
    assert!(!after.not_modified);
    assert!(after.instance_revision > started.revision);

    let unknown = repository::get_instance_tokens(
        &fixture.db, &monitor, &started.instance_id, Some("missing-scope"), None).unwrap_err();
    assert!(unknown.to_string().contains("not found"), "{unknown}");
}

#[test]
fn an_embedded_subprocess_token_counts_its_inner_tokens_and_the_scope_filter_shows_them() {
    let fixture = Fixture::new();
    let model = embedded_model(user_model(Some(&fixture.participant.user_id)), "Sub");
    let started = start_model(&fixture, &model);
    let child = started.scopes.iter().find(|scope| scope.scope_id != started.instance_id)
        .expect("running child scope").scope_id.clone();

    let outer = repository::get_instance_tokens(&fixture.db, &fixture.owner, &started.instance_id, None, None).unwrap();
    assert_eq!(outer.tokens.len(), 1);
    assert_eq!(outer.tokens[0].node_id, "Sub");
    assert_eq!(outer.tokens[0].wait, Some(ProcessTokenWait::Scope { scope_id: child.clone(), active_tokens: 1 }));

    let inner = repository::get_instance_tokens(
        &fixture.db, &fixture.owner, &started.instance_id, Some(&child), None).unwrap();
    assert_eq!(inner.tokens.len(), 1);
    assert_eq!(inner.tokens[0].scope_id, child);
    assert!(matches!(inner.tokens[0].wait, Some(ProcessTokenWait::UserTask { .. })));
    assert!(inner.visited_node_ids.iter().any(|node| node == "Work"));
}

#[tokio::test]
async fn an_incident_is_attached_to_the_token_of_its_job() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("token incident", None));
    let model = service_model(&flow_id, ActivityVerification::Condition { expression: "true".into() });
    let started = start_model(&fixture, &model);
    let claim = repository::claim_job(&fixture.db, "token-worker", super::jobs::now_ms()).unwrap().unwrap();
    assert!(repository::fail_job(&fixture.db, &claim.job.job_id, claim.job.attempt, claim.job.fence,
        "token-worker", "INTERRUPTED", "controlled interruption", super::jobs::now_ms(), None).unwrap());

    let instance = repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    let view = repository::get_instance_tokens(&fixture.db, &fixture.owner, &started.instance_id, None, None).unwrap();
    assert_eq!(view.tokens.len(), 1);
    assert_eq!(view.tokens[0].wait, Some(ProcessTokenWait::Job { job_id: claim.job.job_id.clone() }));
    assert_eq!(view.tokens[0].incident_ids, vec![instance.incidents[0].incident_id.clone()]);
}

#[test]
fn a_long_history_is_cut_and_reported_never_silently() {
    let fixture = Fixture::new();
    let started = waiting_instance(&fixture);
    {
        let conn = fixture.db.write().unwrap();
        for index in 0..2105 {
            conn.execute(
                "INSERT INTO bpmn_tokens(token_id,instance_id,scope_id,node_id,arrival_edge_id,fork_stack_json,status,created_at_ms) VALUES(?1,?2,?2,?3,?4,'[]','consumed',1)",
                rusqlite::params![format!("old-{index}"), started.instance_id, format!("N{index}"), format!("E{index}")],
            ).unwrap();
        }
    }
    let view = repository::get_instance_tokens(&fixture.db, &fixture.owner, &started.instance_id, None, None).unwrap();
    assert!(view.truncated);
    assert_eq!(view.taken_edge_ids.len(), 2000);
    assert_eq!(view.visited_node_ids.len(), 2000);
    assert_eq!(view.tokens.len(), 1, "the live token is never cut by the history limit");
}

#[tokio::test]
async fn a_retry_in_the_name_of_an_initiator_who_lost_the_definition_is_refused_and_never_offered() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("steward retry", None));
    let model = service_model(&flow_id, ActivityVerification::Condition { expression: "true".into() });
    let started = start_model(&fixture, &model);
    let claim = repository::claim_job(&fixture.db, "steward-worker", super::jobs::now_ms()).unwrap().unwrap();
    assert!(repository::fail_job(&fixture.db, &claim.job.job_id, claim.job.attempt, claim.job.fence,
        "steward-worker", "INTERRUPTED", "controlled interruption", super::jobs::now_ms(), None).unwrap());
    let steward = member(&fixture, "steward", "role-org-viewer");
    move_definition_to(&fixture, &started.definition_id, &steward);

    let seen = repository::get_instance(&fixture.db, &steward, &started.instance_id, None).unwrap();
    assert_eq!(seen.access, Some(ProcessInstanceAccess::Owner));
    assert!(!seen.can_retry && !seen.incidents[0].can_retry);
    let refused = repository::retry_job(&fixture.db, &steward, &stamp("steward asks"),
        &started.instance_id, &claim.job.job_id, seen.revision).unwrap_err();
    assert!(refused.downcast_ref::<ProcessOperationDenied>().is_some(), "{refused}");
    let after = repository::get_instance(&fixture.db, &steward, &started.instance_id, None).unwrap();
    assert_eq!((after.revision, after.status), (seen.revision, ProcessInstanceStatus::Incident));
    assert!(repository::claim_job(&fixture.db, "steward-worker", super::jobs::now_ms()).unwrap().is_none(),
        "a refused retry queues no job");

    // A deactivated initiator turns the request into a refusal too.
    fixture.db.write().unwrap().execute(
        "UPDATE user_accounts SET is_active=0 WHERE id=?1", [&fixture.owner.user_id]).unwrap();
    let inactive = repository::get_instance(&fixture.db, &steward, &started.instance_id, None).unwrap();
    assert!(!inactive.can_retry && !inactive.incidents[0].can_retry);
    let refused = repository::retry_job(&fixture.db, &steward, &stamp("steward asks again"),
        &started.instance_id, &claim.job.job_id, seen.revision).unwrap_err();
    assert!(refused.downcast_ref::<ProcessAuthorityDenied>().is_some(), "{refused}");
}

#[tokio::test]
async fn another_ready_activation_takes_the_retry_offer_back() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("ready retry", None));
    let model = service_model(&flow_id, ActivityVerification::Condition { expression: "true".into() });
    let started = start_model(&fixture, &model);
    let claim = repository::claim_job(&fixture.db, "ready-worker", super::jobs::now_ms()).unwrap().unwrap();
    assert!(repository::fail_job(&fixture.db, &claim.job.job_id, claim.job.attempt, claim.job.fence,
        "ready-worker", "INTERRUPTED", "controlled interruption", super::jobs::now_ms(), None).unwrap());
    let read = || repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert!(read().can_retry && read().incidents[0].can_retry);

    fixture.db.write().unwrap().execute(
        "INSERT INTO bpmn_tokens(token_id,instance_id,scope_id,node_id,arrival_edge_id,fork_stack_json,status,created_at_ms) VALUES('stray',?1,?1,'Start_1',NULL,'[]','ready',1)",
        [&started.instance_id]).unwrap();
    let blocked = read();
    assert!(!blocked.can_retry && !blocked.incidents[0].can_retry);
    assert!(repository::retry_job(&fixture.db, &fixture.owner, &stamp("blocked retry"),
        &started.instance_id, &claim.job.job_id, blocked.revision).is_err());
}

#[test]
fn only_a_participant_may_send_a_message_into_the_instance() {
    let fixture = Fixture::new();
    let started = waiting_instance(&fixture);
    let steward = member(&fixture, "steward", "role-org-viewer");
    let monitor = member(&fixture, "auditor", "role-org-admin");
    move_definition_to(&fixture, &started.definition_id, &steward);
    let can_send = |actor: &ProcessActor| repository::get_instance(&fixture.db, actor, &started.instance_id, None)
        .unwrap().can_send_message;
    assert_eq!(can_send(&fixture.participant), Some(true));
    assert_eq!(can_send(&steward), Some(false));
    assert_eq!(can_send(&monitor), Some(false));
    move_definition_to(&fixture, &started.definition_id, &fixture.owner);
    assert_eq!(can_send(&fixture.owner), Some(true));
}

#[test]
fn a_monitor_reads_no_correlation_key_failure_text_or_reason() {
    let fixture = Fixture::new();
    let mut model = super::send_receive_tests::receive_model();
    model.variables.insert("customer_note".into(), json!("secret-key-77"));
    let ProcessNodeKind::ReceiveTask { correlation_expression, .. } =
        &mut model.nodes.iter_mut().find(|node| node.id == "Catch_1").unwrap().kind
        else { panic!("fixture node is not a ReceiveTask") };
    *correlation_expression = "vars.customer_note".into();
    let version = publish_model(&fixture, &model);
    let started = super::messages::test_support::start_version(&fixture, &version);
    fixture.db.write().unwrap().execute(
        "UPDATE bpmn_event_subscriptions SET last_reason='mismatch for secret-key-77' WHERE instance_id=?1",
        [&started.instance_id]).unwrap();
    let monitor = member(&fixture, "auditor", "role-org-admin");

    let owner_view = repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert_eq!(owner_view.subscriptions[0].correlation_key.as_deref(), Some("secret-key-77"));
    assert_eq!(owner_view.subscriptions[0].last_reason.as_deref(), Some("mismatch for secret-key-77"));
    let seen = repository::get_instance(&fixture.db, &monitor, &started.instance_id, None).unwrap();
    assert_eq!(seen.subscriptions.len(), 1);
    assert_eq!(seen.subscriptions[0].message_name.as_deref(), owner_view.subscriptions[0].message_name.as_deref());
    assert!(seen.subscriptions[0].correlation_key.is_none() && seen.subscriptions[0].last_reason.is_none());
    assert!(!serde_json::to_string(&seen).unwrap().contains("secret-key-77"));
}

#[tokio::test]
async fn a_monitor_reads_the_incident_code_never_the_worker_failure_text() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("secret failure", None));
    let model = service_model(&flow_id, ActivityVerification::Condition { expression: "true".into() });
    let started = start_model(&fixture, &model);
    let claim = repository::claim_job(&fixture.db, "secret-worker", super::jobs::now_ms()).unwrap().unwrap();
    assert!(repository::fail_job(&fixture.db, &claim.job.job_id, claim.job.attempt, claim.job.fence,
        "secret-worker", "INTERRUPTED", "upstream rejected token sk-live-4242", super::jobs::now_ms(), None).unwrap());
    let monitor = member(&fixture, "auditor", "role-org-admin");

    let owner_view = repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    let incident_id = owner_view.incidents[0].incident_id.clone();
    assert!(owner_view.incidents[0].message.contains("sk-live-4242"));
    let pages: tentaflow_protocol::processes::ProcessInstancePageRequest =
        serde_json::from_value(json!({"selected_incident_id": incident_id})).unwrap();
    let seen = repository::get_instance(&fixture.db, &monitor, &started.instance_id, Some(&pages)).unwrap();
    assert_eq!(seen.incidents[0].code, owner_view.incidents[0].code);
    assert_eq!(seen.incidents[0].message, seen.incidents[0].code);
    let selected = seen.selected_incident.as_ref().expect("selected incident");
    assert_eq!(selected.incident.message, selected.incident.code);
    assert!(!serde_json::to_string(&seen).unwrap().contains("sk-live-4242"));
}

#[test]
fn the_scope_is_validated_before_a_matching_revision_answers_not_modified() {
    let fixture = Fixture::new();
    let started = waiting_instance(&fixture);
    let error = repository::get_instance_tokens(
        &fixture.db, &fixture.owner, &started.instance_id, Some("missing-scope"), Some(started.revision),
    ).unwrap_err();
    assert!(error.to_string().contains("not found"), "{error}");
}

fn insert_live_token(fixture: &Fixture, instance_id: &str, token_id: &str, node_id: &str, status: &str, at_ms: i64) {
    fixture.db.write().unwrap().execute(
        "INSERT INTO bpmn_tokens(token_id,instance_id,scope_id,node_id,arrival_edge_id,fork_stack_json,status,created_at_ms) VALUES(?1,?2,?2,?3,NULL,'[]',?4,?5)",
        rusqlite::params![token_id, instance_id, node_id, status, at_ms],
    ).unwrap();
}

#[test]
fn a_crowded_token_view_is_cut_from_the_end_to_fit_one_frame() {
    let fixture = Fixture::new();
    let started = waiting_instance(&fixture);
    let node = "N".repeat(400);
    for index in 0..1900 {
        insert_live_token(&fixture, &started.instance_id, &format!("fan-{index:04}-{}", "t".repeat(48)),
            &node, "waiting", i64::from(index));
    }
    let view = repository::get_instance_tokens(&fixture.db, &fixture.owner, &started.instance_id, None, None).unwrap();
    assert!(view.truncated);
    assert!(view.tokens.len() > 500 && view.tokens.len() < 1900, "{}", view.tokens.len());
    assert!(view.tokens.iter().enumerate().all(|(index, token)| token.token_id.starts_with(&format!("fan-{index:04}-"))),
        "the cut keeps the oldest tokens in order");
    let frame = tentaflow_protocol::cbor::encode(&tentaflow_protocol::message_body::MessageBody::ProcessBody(
        tentaflow_protocol::processes::ProcessPayload::InstanceTokensGetResponse {
            instance_revision: view.instance_revision, not_modified: false, version: view.version,
            process_id: view.process_id.clone(), tokens: view.tokens.clone(),
            taken_edge_ids: view.taken_edge_ids.clone(), visited_node_ids: view.visited_node_ids.clone(),
            truncated: view.truncated,
        })).unwrap();
    assert!(frame.len() <= 900 * 1024, "{}", frame.len());
}

#[test]
fn a_node_incident_is_attached_once_and_parks_its_token_at_the_gateway() {
    let fixture = Fixture::new();
    let started = waiting_instance(&fixture);
    for (index, token) in ["gate-a", "gate-b", "gate-c"].into_iter().enumerate() {
        insert_live_token(&fixture, &started.instance_id, token, "Gate", "waiting", 10 + index as i64);
    }
    {
        let conn = fixture.db.write().unwrap();
        for (id, code, at_ms) in [("inc-1", "NO_MATCHING_FLOW", 1), ("inc-2", "AMBIGUOUS_GATEWAY", 2)] {
            conn.execute(
                "INSERT INTO bpmn_incidents(incident_id,instance_id,scope_id,node_id,job_id,code,message,at_ms) VALUES(?1,?2,?2,'Gate',NULL,?3,'m',?4)",
                rusqlite::params![id, started.instance_id, code, at_ms],
            ).unwrap();
        }
    }
    let view = repository::get_instance_tokens(&fixture.db, &fixture.owner, &started.instance_id, None, None).unwrap();
    let token = |id: &str| view.tokens.iter().find(|token| token.token_id == id).unwrap();
    let mut attached = view.tokens.iter().flat_map(|token| token.incident_ids.clone()).collect::<Vec<_>>();
    attached.sort();
    assert_eq!(attached, vec!["inc-1".to_string(), "inc-2".to_string()], "each incident exactly once");
    assert_eq!(token("gate-a").wait, Some(ProcessTokenWait::Gateway));
    assert_eq!(token("gate-b").wait, Some(ProcessTokenWait::Gateway));
    assert_eq!(token("gate-c").wait, None);
    assert!(matches!(view.tokens.iter().find(|token| token.node_id == "Work").unwrap().wait,
        Some(ProcessTokenWait::UserTask { .. })));
}

#[test]
fn repeated_edges_are_listed_once() {
    let fixture = Fixture::new();
    let started = waiting_instance(&fixture);
    {
        let conn = fixture.db.write().unwrap();
        for index in 0..5 {
            conn.execute(
                "INSERT INTO bpmn_tokens(token_id,instance_id,scope_id,node_id,arrival_edge_id,fork_stack_json,status,created_at_ms) VALUES(?1,?2,?2,'Loop',?3,'[]','consumed',1)",
                rusqlite::params![format!("lap-{index}"), started.instance_id, if index % 2 == 0 { "Back" } else { "Forth" }],
            ).unwrap();
        }
    }
    let view = repository::get_instance_tokens(&fixture.db, &fixture.owner, &started.instance_id, None, None).unwrap();
    assert_eq!(view.taken_edge_ids, vec!["Back".to_string(), "Forth".to_string(), "ToWork".to_string()]);
    assert_eq!(view.visited_node_ids.iter().filter(|node| *node == "Loop").count(), 1);
}

fn fork_join_model(assignee: &str) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = super::model::starter_model();
    for (id, kind) in [
        ("Split", ProcessNodeKind::ParallelGateway),
        ("Join", ProcessNodeKind::ParallelGateway),
        ("Left", ProcessNodeKind::UserTask {
            assignee_user_id: Some(assignee.into()), output_mapping: Default::default() }),
        ("Right", ProcessNodeKind::UserTask {
            assignee_user_id: Some(assignee.into()), output_mapping: Default::default() }),
    ] {
        model.nodes.push(ProcessNode { id: id.into(), name: id.into(), kind, repeat: None, activity_io: None });
    }
    model.sequence_flows = vec![
        edge("StartSplit", "Start_1", "Split"),
        edge("SplitLeft", "Split", "Left"),
        edge("SplitRight", "Split", "Right"),
        edge("LeftJoin", "Left", "Join"),
        edge("RightJoin", "Right", "Join"),
        edge("JoinEnd", "Join", "End_1"),
    ];
    model
}

#[test]
fn a_parallel_join_parks_the_first_arrival_at_the_gateway_while_the_other_branch_works() {
    let fixture = Fixture::new();
    let started = start_model(&fixture, &fork_join_model(&fixture.owner.user_id));
    let view = repository::get_instance_tokens(&fixture.db, &fixture.owner, &started.instance_id, None, None).unwrap();
    assert_eq!(view.tokens.len(), 2);
    assert!(view.tokens.iter().all(|token| matches!(token.wait, Some(ProcessTokenWait::UserTask { .. }))));

    let left = started.user_tasks.iter().find(|task| task.node_id == "Left").unwrap();
    repository::complete_user_task(
        &fixture.db, &fixture.owner, &stamp("finish left"), &started.instance_id,
        &left.user_task_id, started.revision, &json!({}), None,
        repository::ProcessPlanInput::Canonical, super::jobs::now_ms(),
    ).unwrap();
    let view = repository::get_instance_tokens(&fixture.db, &fixture.owner, &started.instance_id, None, None).unwrap();
    let joining = view.tokens.iter().find(|token| token.status == ProcessTokenStatus::Joining).expect("joining token");
    assert_eq!((joining.node_id.as_str(), joining.wait.clone()), ("Join", Some(ProcessTokenWait::Gateway)));
    let right = view.tokens.iter().find(|token| token.node_id == "Right").expect("other branch");
    assert!(matches!(right.wait, Some(ProcessTokenWait::UserTask { .. })));
    assert!(view.taken_edge_ids.contains(&"LeftJoin".to_string()));
}

/// A repetition keeps one coordinator token on its element beside one token per
/// live occurrence; only the occurrence tokens carry an occurrence and a wait.
fn split_coordinator(
    tokens: Vec<tentaflow_protocol::processes::ProcessTokenView>,
) -> Vec<tentaflow_protocol::processes::ProcessTokenView> {
    let (coordinators, occurrences): (Vec<_>, Vec<_>) =
        tokens.into_iter().partition(|token| token.occurrence.is_none());
    assert_eq!(coordinators.len(), 1, "{coordinators:?}");
    assert!(coordinators[0].wait.is_none() && coordinators[0].status == ProcessTokenStatus::Waiting);
    occurrences
}

#[test]
fn sequential_and_parallel_repetition_report_their_occurrence() {
    let fixture = Fixture::new();
    let owner = fixture.owner.user_id.clone();
    let sequential = start_model(&fixture, &super::repetition_tests::sequential_human_model(&owner, 3));
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &sequential.instance_id).unwrap();
    let view = repository::get_instance_tokens(&fixture.db, &fixture.owner, &sequential.instance_id, None, None).unwrap();
    let live = split_coordinator(view.tokens);
    assert_eq!(live.len(), 1, "a sequential group has one live occurrence");
    let occurrence = live[0].occurrence.clone().expect("occurrence");
    assert_eq!(occurrence.ordinal, 0);
    assert_eq!(occurrence.group_id, snapshot.repetition_groups[0].group_id);
    assert!(matches!(live[0].wait, Some(ProcessTokenWait::UserTask { .. })));

    let mut model = super::repetition_tests::sequential_human_model(&owner, 3);
    let tentaflow_protocol::processes::ProcessRepeatSpec::MultiInstance { mode, .. } =
        model.nodes.iter_mut().find(|node| node.id == "RepeatedWork").unwrap().repeat.as_mut().unwrap()
        else { panic!("fixture repetition is not multi-instance") };
    *mode = tentaflow_protocol::processes::ProcessMultiInstanceMode::Parallel;
    let parallel = start_model(&fixture, &model);
    let view = repository::get_instance_tokens(&fixture.db, &fixture.owner, &parallel.instance_id, None, None).unwrap();
    let live = split_coordinator(view.tokens);
    let mut ordinals = live.iter().map(|token| token.occurrence.clone().expect("occurrence").ordinal).collect::<Vec<_>>();
    ordinals.sort_unstable();
    assert_eq!(ordinals, vec![0, 1, 2]);
    let mut tasks = live.iter().map(|token| match &token.wait {
        Some(ProcessTokenWait::UserTask { user_task_id }) => user_task_id.clone(),
        other => panic!("expected a task wait, got {other:?}"),
    }).collect::<Vec<_>>();
    tasks.sort();
    tasks.dedup();
    assert_eq!(tasks.len(), 3, "each occurrence waits for its own task");
}

#[test]
fn tokens_on_one_element_in_different_scopes_are_told_apart_by_scope() {
    let fixture = Fixture::new();
    let inner = super::manual_tests::manual_model(None, false);
    let mut model = embedded_model(inner, "RepeatedScope");
    model.variables.insert("results".into(), json!([]));
    model.nodes.iter_mut().find(|node| node.id == "RepeatedScope").unwrap().repeat =
        Some(tentaflow_protocol::processes::ProcessRepeatSpec::MultiInstance {
            mode: tentaflow_protocol::processes::ProcessMultiInstanceMode::Parallel,
            input: tentaflow_protocol::processes::ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
    let started = start_model(&fixture, &model);

    let root = repository::get_instance_tokens(&fixture.db, &fixture.owner, &started.instance_id, None, None).unwrap();
    let occurrences = split_coordinator(root.tokens);
    assert_eq!(occurrences.len(), 2);
    let children = occurrences.iter().map(|token| match &token.wait {
        Some(ProcessTokenWait::Scope { scope_id, active_tokens }) => {
            assert_eq!(*active_tokens, 1);
            scope_id.clone()
        }
        other => panic!("expected a scope wait, got {other:?}"),
    }).collect::<Vec<_>>();
    assert_ne!(children[0], children[1]);
    let mut manual_tokens = Vec::new();
    for child in &children {
        let inner = repository::get_instance_tokens(&fixture.db, &fixture.owner, &started.instance_id, Some(child), None).unwrap();
        assert_eq!(inner.tokens.len(), 1);
        assert_eq!((inner.tokens[0].node_id.as_str(), &inner.tokens[0].scope_id), ("Manual", child));
        manual_tokens.push(inner.tokens[0].token_id.clone());
    }
    assert_ne!(manual_tokens[0], manual_tokens[1]);
}

#[test]
fn a_call_a_timer_a_message_and_a_boundary_host_each_report_their_wait() {
    let fixture = Fixture::new();

    let leaf = publish_model(&fixture, &user_model(Some(&fixture.participant.user_id)));
    let outer = publish_model(&fixture, &super::call_tests::caller(&leaf, Default::default()));
    let parent = super::messages::test_support::start_version(&fixture, &outer);
    let view = repository::get_instance_tokens(&fixture.db, &fixture.owner, &parent.instance_id, None, None).unwrap();
    assert_eq!(view.tokens.len(), 1);
    let child = super::call_tests::child_id(&fixture, &parent.instance_id);
    assert_eq!(view.tokens[0].wait, Some(ProcessTokenWait::Call { child_instance_id: child.clone() }));

    // A monitor who opens the parent may open the called instance too.
    let auditor = member(&fixture, "auditor", "role-org-admin");
    let seen = repository::get_instance(&fixture.db, &auditor, &parent.instance_id, None).unwrap();
    assert_eq!(seen.access, Some(ProcessInstanceAccess::Monitor));
    let tentaflow_protocol::processes::ProcessCallSummary::Outgoing { child: related, .. } = &seen.calls[0]
        else { panic!("the parent holds an outgoing call") };
    let related = related.as_ref().expect("the observer gate reaches the child");
    assert_eq!((related.instance_id.as_str(), related.can_open), (child.as_str(), true));

    let mut timer = super::model::starter_model();
    timer.timer_timezone = Some("UTC".into());
    timer.nodes.insert(1, ProcessNode { id: "Wait".into(), name: "Wait".into(),
        kind: ProcessNodeKind::TimerCatch {
            timer: tentaflow_protocol::processes::ProcessTimerSpec::Duration { seconds: 3600 } },
        repeat: None, activity_io: None });
    timer.sequence_flows = vec![edge("ToWait", "Start_1", "Wait"), edge("WaitEnd", "Wait", "End_1")];
    let timed = start_model(&fixture, &timer);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &timed.instance_id).unwrap();
    let view = repository::get_instance_tokens(&fixture.db, &fixture.owner, &timed.instance_id, None, None).unwrap();
    assert_eq!(view.tokens.len(), 1);
    assert_eq!(view.tokens[0].wait, Some(ProcessTokenWait::Timer {
        timer_id: snapshot.timers[0].timer_id.clone(), due_at_ms: snapshot.timers[0].due_at_ms }));

    let received = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let waiting = super::messages::test_support::start_version(&fixture, &received);
    let view = repository::get_instance_tokens(&fixture.db, &fixture.owner, &waiting.instance_id, None, None).unwrap();
    assert_eq!(view.tokens[0].wait, Some(ProcessTokenWait::Message {
        subscription_id: waiting.subscriptions[0].subscription_id.clone() }));

    let hosted = start_model(&fixture, &with_boundaries(
        user_model(Some(&fixture.participant.user_id)), "Work", &[("Deadline", false, 3600)]));
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &hosted.instance_id).unwrap();
    assert_eq!(snapshot.timers.len(), 1, "the boundary armed a timer for the host");
    let view = repository::get_instance_tokens(&fixture.db, &fixture.owner, &hosted.instance_id, None, None).unwrap();
    assert_eq!(view.tokens.len(), 1);
    assert_eq!(view.tokens[0].node_id, "Work");
    assert!(matches!(view.tokens[0].wait, Some(ProcessTokenWait::UserTask { .. })),
        "the person outranks the deadline armed beside them");
}

#[test]
fn the_pinned_model_goes_whole_to_its_owner_and_structure_only_to_every_other_reader() {
    let fixture = Fixture::new();
    let started = waiting_instance(&fixture);
    let monitor = member(&fixture, "auditor", "role-org-admin");
    let stranger = member(&fixture, "stranger", "role-org-viewer");
    let steward = member(&fixture, "steward", "role-org-viewer");
    let read = |actor: &ProcessActor| repository::get_instance_model(&fixture.db, actor, &started.instance_id);

    // The initiator who also owns the definition sees the model as published.
    let (version, whole, structure_only) = read(&fixture.owner).unwrap();
    assert_eq!((version, structure_only), (started.version, false));
    let whole_json = serde_json::to_string(&whole).unwrap();
    assert!(whole_json.contains("outputs.answer") && whole_json.contains("hunter2"));

    for (label, reader) in [("monitor", &monitor), ("assignee", &fixture.participant)] {
        let (version, model, structure_only) = read(reader).unwrap_or_else(|error| panic!("{label}: {error}"));
        assert_eq!((version, structure_only), (started.version, true), "{label}");
        let json = serde_json::to_string(&model).unwrap();
        assert!(!json.contains("outputs.answer") && !json.contains("hunter2"), "{label}: {json}");
        assert!(model.variables.is_empty(), "{label}");
        assert_eq!(model.nodes.iter().map(|node| node.id.as_str()).collect::<Vec<_>>(),
            whole.nodes.iter().map(|node| node.id.as_str()).collect::<Vec<_>>(), "{label}");
        assert_eq!(model.diagram, whole.diagram, "{label}");
    }

    // An owner who did not start the instance still gets the whole model, and
    // the former owner (now only the initiator) gets the projection.
    move_definition_to(&fixture, &started.definition_id, &steward);
    assert!(!read(&steward).unwrap().2);
    assert!(read(&fixture.owner).unwrap().2);

    assert!(is_not_found(&read(&stranger).unwrap_err()));
    let other_org = ProcessActor {
        org_id: "organization-that-does-not-exist".into(),
        user_id: fixture.owner.user_id.clone(),
    };
    assert!(read(&other_org).is_err());
    let missing = repository::get_instance_model(&fixture.db, &monitor, "no-such-instance").unwrap_err();
    assert!(is_not_found(&missing), "{missing}");
}

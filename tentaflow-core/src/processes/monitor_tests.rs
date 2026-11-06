// ============ File: monitor_tests.rs — BPMN B4.1 monitor reader: access grounds, redaction, list filters, token view ============

use serde_json::json;
use tentaflow_protocol::processes::{
    ActivityVerification, ProcessInstanceAccess, ProcessInstanceListScope, ProcessInstanceStatus,
    ProcessTokenStatus, ProcessTokenWait,
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

    let (_, variables, _) = repository::get_scope(
        &fixture.db, &monitor, &started.instance_id, &started.instance_id).unwrap();
    assert_eq!(variables, json!({"customer_note": "string", "amount": "number"}));
    let events = repository::list_events(&fixture.db, &monitor, &started.instance_id, 0, 50).unwrap().0;
    assert!(!events.is_empty() && events.iter().all(|event| event.data.is_null()));
    assert_eq!(repository::instance_access(&fixture.db, &monitor, &started.instance_id).unwrap(),
        ProcessInstanceAccess::Monitor);

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
    assert_eq!(repository::instance_access(&fixture.db, &viewer, &started.instance_id).unwrap(),
        ProcessInstanceAccess::Monitor);
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

    // The initiator keeps the right to ask for a retry even when the definition moved on.
    assert_eq!(detail(&fixture.owner), (true, true));
    assert_eq!(detail(&steward), (true, true));
    assert_eq!(detail(&monitor), (false, false));
    assert!(list_flag(&fixture.owner, ProcessInstanceListScope::Mine));
    assert!(list_flag(&steward, ProcessInstanceListScope::Owned));
    assert!(!list_flag(&monitor, ProcessInstanceListScope::Monitor));
    let participant = repository::get_instance(&fixture.db, &fixture.participant, &started.instance_id, None);
    assert!(participant.is_err(), "a reader without a task of this instance has no view");
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

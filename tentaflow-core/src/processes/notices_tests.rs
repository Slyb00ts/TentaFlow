// ============ File: notices_tests.rs — BPMN B4.3 incident notices: recipients, dedup, redaction, simulation silence, crash safety ============

use std::cell::RefCell;
use std::panic::{catch_unwind, AssertUnwindSafe};

use tentaflow_protocol::processes::ActivityVerification;
use tentaflow_protocol::SystemEventPayload;

use super::notices::{deliver_pending, INCIDENT_NOTICE_KIND};
use super::repository::{self, ProcessActor};
use super::runtime::test_support::*;

fn member(fixture: &Fixture, name: &str, role_id: &str) -> ProcessActor {
    let id = crate::db::repository::create_user_account(
        &fixture.db,
        name,
        "test-password-hash",
        name,
        &format!("{name}@example.test"),
    )
    .expect("create process user");
    crate::services::org::repo::add_membership(
        &fixture.db,
        crate::services::org::DEFAULT_ORG_ID,
        &id,
        role_id,
        &id,
    )
    .expect("grant organization membership");
    ProcessActor {
        org_id: crate::services::org::DEFAULT_ORG_ID.into(),
        user_id: id,
    }
}

fn move_definition_to(fixture: &Fixture, definition_id: &str, owner: &ProcessActor) {
    fixture
        .db
        .write()
        .unwrap()
        .execute(
            "UPDATE bpmn_definitions SET owner_user_id=?1 WHERE definition_id=?2",
            rusqlite::params![owner.user_id, definition_id],
        )
        .unwrap();
}

/// Starts a Service instance and fails its job, which raises one incident.
fn failed_instance(
    fixture: &Fixture,
    failure: &str,
) -> tentaflow_protocol::processes::ProcessInstance {
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("notice", None));
    let mut model = service_model(
        &flow_id,
        ActivityVerification::Condition {
            expression: "true".into(),
        },
    );
    model
        .variables
        .insert("customer_note".into(), serde_json::json!("hunter2"));
    let started = start_model(fixture, &model);
    fail_next_job(fixture, failure);
    started
}

fn fail_next_job(fixture: &Fixture, failure: &str) {
    let claim = repository::claim_job(&fixture.db, "notice-worker", super::jobs::now_ms())
        .unwrap()
        .unwrap();
    assert!(repository::fail_job(
        &fixture.db,
        &claim.job.job_id,
        claim.job.attempt,
        claim.job.fence,
        "notice-worker",
        "INTERRUPTED",
        failure,
        super::jobs::now_ms(),
        None
    )
    .unwrap());
}

/// An incident raised after the definition changed hands: a job of a moved
/// definition is no longer claimable, so the row is raised as the engine would.
fn raise_incident(fixture: &Fixture, instance_id: &str) {
    fixture.db.write().unwrap().execute(
        "INSERT INTO bpmn_incidents(incident_id,instance_id,scope_id,node_id,job_id,code,message,at_ms) VALUES(?1,?2,?2,NULL,NULL,'INTERRUPTED','worker failed',?3)",
        rusqlite::params![uuid::Uuid::new_v4().to_string(), instance_id, super::jobs::now_ms()],
    ).unwrap();
}

struct Pushed(RefCell<Vec<SystemEventPayload>>);

impl Pushed {
    fn new() -> Self {
        Self(RefCell::new(Vec::new()))
    }
    fn deliver(&self, fixture: &Fixture) -> usize {
        deliver_pending(&fixture.db, super::jobs::now_ms(), &|event| {
            self.0.borrow_mut().push(event)
        })
        .unwrap()
    }
    fn users(&self) -> Vec<String> {
        let mut users: Vec<String> = self
            .0
            .borrow()
            .iter()
            .map(|event| match event {
                SystemEventPayload::UserNotification { user_id, .. } => user_id.clone(),
                other => panic!("unexpected event {other:?}"),
            })
            .collect();
        users.sort();
        users
    }
}

fn count(fixture: &Fixture, sql: &str) -> i64 {
    fixture
        .db
        .read()
        .unwrap()
        .query_row(sql, [], |row| row.get(0))
        .unwrap()
}

#[tokio::test]
async fn an_incident_reaches_its_initiator_and_the_definition_owner_once_and_nobody_else() {
    let fixture = Fixture::new();
    let steward = member(&fixture, "steward", "role-org-viewer");
    let monitor = member(&fixture, "auditor", "role-org-admin");
    let started = start_model(&fixture, &user_model(Some(&fixture.participant.user_id)));
    move_definition_to(&fixture, &started.definition_id, &steward);
    raise_incident(&fixture, &started.instance_id);

    let pushed = Pushed::new();
    assert_eq!(pushed.deliver(&fixture), 2);
    let mut expected = vec![fixture.owner.user_id.clone(), steward.user_id.clone()];
    expected.sort();
    assert_eq!(pushed.users(), expected);
    assert!(
        !pushed.users().contains(&monitor.user_id),
        "a monitor reads the list, it is not paged"
    );
    assert!(!pushed.users().contains(&fixture.participant.user_id));
    for event in pushed.0.borrow().iter() {
        let SystemEventPayload::UserNotification {
            kind,
            project_id,
            link_json,
            notification_id,
            ..
        } = event
        else {
            unreachable!()
        };
        assert_eq!(kind, INCIDENT_NOTICE_KIND);
        assert!(project_id.is_empty());
        let link: serde_json::Value = serde_json::from_str(link_json).unwrap();
        assert_eq!(link["instanceId"], started.instance_id);
        assert_eq!(link["code"], "INTERRUPTED");
        assert!(notification_id.starts_with("process-incident:"));
    }
    assert_eq!(
        pushed.deliver(&fixture),
        0,
        "a settled notice is not pushed again"
    );
    assert_eq!(count(&fixture, "SELECT COUNT(*) FROM bpmn_incident_notices WHERE notified_at_ms IS NOT NULL AND suppressed=0"), 2);
}

#[tokio::test]
async fn an_initiator_who_owns_the_definition_is_notified_once() {
    let fixture = Fixture::new();
    failed_instance(&fixture, "worker failed");
    let pushed = Pushed::new();
    assert_eq!(pushed.deliver(&fixture), 1);
    assert_eq!(pushed.users(), vec![fixture.owner.user_id.clone()]);
}

#[tokio::test]
async fn a_notice_survives_a_restart_and_is_not_repeated_after_it_was_pushed() {
    let fixture = Fixture::new();
    failed_instance(&fixture, "worker failed");
    let path = fixture.directory.path().join("processes.db");
    let before_restart = Pushed::new();
    assert_eq!(before_restart.deliver(&fixture), 1);

    let restarted = crate::db::init(&path).expect("reopen the process database");
    let after_restart = Pushed::new();
    let pushed = deliver_pending(&restarted, super::jobs::now_ms(), &|event| {
        after_restart.0.borrow_mut().push(event)
    })
    .unwrap();
    assert_eq!(pushed, 0);

    // An incident that was raised and not yet pushed when the node stopped is pushed after the restart.
    fail_second_incident(&fixture);
    let restarted_again = crate::db::init(&path).expect("reopen the process database again");
    let pushed = deliver_pending(&restarted_again, super::jobs::now_ms(), &|event| {
        after_restart.0.borrow_mut().push(event)
    })
    .unwrap();
    assert_eq!(pushed, 1);
}

/// A second incident on its own instance of the same definition.
fn fail_second_incident(fixture: &Fixture) {
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("second", None));
    start_model(
        fixture,
        &service_model(
            &flow_id,
            ActivityVerification::Condition {
                expression: "true".into(),
            },
        ),
    );
    fail_next_job(fixture, "second failure");
}

#[tokio::test]
async fn every_incident_is_its_own_notice_and_a_replayed_queue_insert_changes_nothing() {
    let fixture = Fixture::new();
    failed_instance(&fixture, "first");
    let pushed = Pushed::new();
    assert_eq!(pushed.deliver(&fixture), 1);
    fail_second_incident(&fixture);
    assert_eq!(pushed.deliver(&fixture), 1);
    let ids: Vec<String> = pushed
        .0
        .borrow()
        .iter()
        .map(|event| match event {
            SystemEventPayload::UserNotification {
                notification_id, ..
            } => notification_id.clone(),
            _ => unreachable!(),
        })
        .collect();
    assert_ne!(ids[0], ids[1]);

    // The race of a second raise queueing the same pair loses to the primary key.
    let changed = fixture.db.write().unwrap().execute(
        "INSERT OR IGNORE INTO bpmn_incident_notices(incident_id,user_id,queued_at_ms) SELECT incident_id,user_id,0 FROM bpmn_incident_notices", [],
    ).unwrap();
    assert_eq!(changed, 0);
    assert_eq!(pushed.deliver(&fixture), 0);
}

#[tokio::test]
async fn the_event_carries_the_code_and_the_process_name_never_a_value_or_the_failure_text() {
    let fixture = Fixture::new();
    failed_instance(&fixture, "upstream rejected token sk-live-4242");
    let pushed = Pushed::new();
    assert_eq!(pushed.deliver(&fixture), 1);
    let event = format!("{:?}", pushed.0.borrow());
    for secret in ["sk-live-4242", "hunter2", "upstream rejected"] {
        assert!(!event.contains(secret), "{secret} leaked into {event}");
    }
    let SystemEventPayload::UserNotification { title, body, .. } = &pushed.0.borrow()[0] else {
        unreachable!()
    };
    assert_eq!(body, "INTERRUPTED");
    assert!(!title.is_empty());
}

#[tokio::test]
async fn a_resolved_incident_is_settled_without_a_push_and_resolution_does_not_notify_again() {
    let fixture = Fixture::new();
    failed_instance(&fixture, "worker failed");
    fixture
        .db
        .write()
        .unwrap()
        .execute("UPDATE bpmn_incidents SET resolved_at_ms=1", [])
        .unwrap();
    let pushed = Pushed::new();
    assert_eq!(pushed.deliver(&fixture), 0);
    assert_eq!(count(&fixture, "SELECT COUNT(*) FROM bpmn_incident_notices WHERE suppressed=1 AND notified_at_ms IS NOT NULL"), 1);

    let again = Pushed::new();
    fail_second_incident(&fixture);
    assert_eq!(again.deliver(&fixture), 1);
    fixture
        .db
        .write()
        .unwrap()
        .execute(
            "UPDATE bpmn_incidents SET resolved_at_ms=2 WHERE resolved_at_ms IS NULL",
            [],
        )
        .unwrap();
    assert_eq!(
        again.deliver(&fixture),
        0,
        "closing a pushed incident sends nothing"
    );
    assert_eq!(again.0.borrow().len(), 1);
}

#[tokio::test]
async fn an_incident_inserted_already_resolved_queues_nothing() {
    let fixture = Fixture::new();
    failed_instance(&fixture, "worker failed");
    let before = count(&fixture, "SELECT COUNT(*) FROM bpmn_incident_notices");
    fixture.db.write().unwrap().execute(
        "INSERT INTO bpmn_incidents(incident_id,instance_id,scope_id,node_id,job_id,code,message,at_ms,resolved_at_ms) SELECT 'closed-on-arrival',instance_id,scope_id,node_id,NULL,code,message,at_ms,at_ms FROM bpmn_incidents LIMIT 1",
        [],
    ).unwrap();
    assert_eq!(
        count(&fixture, "SELECT COUNT(*) FROM bpmn_incident_notices"),
        before
    );
}

#[tokio::test]
async fn a_recipient_who_cannot_read_the_instance_any_more_is_not_paged() {
    let fixture = Fixture::new();
    let steward = member(&fixture, "steward", "role-org-viewer");
    let started = start_model(&fixture, &user_model(Some(&fixture.participant.user_id)));
    move_definition_to(&fixture, &started.definition_id, &steward);
    raise_incident(&fixture, &started.instance_id);
    fixture
        .db
        .write()
        .unwrap()
        .execute(
            "UPDATE user_accounts SET is_active=0 WHERE id=?1",
            [&steward.user_id],
        )
        .unwrap();
    fixture
        .db
        .write()
        .unwrap()
        .execute(
            "DELETE FROM org_memberships WHERE user_id=?1",
            [&fixture.owner.user_id],
        )
        .unwrap();

    let pushed = Pushed::new();
    assert_eq!(pushed.deliver(&fixture), 0);
    assert_eq!(
        count(
            &fixture,
            "SELECT COUNT(*) FROM bpmn_incident_notices WHERE suppressed=1"
        ),
        2
    );
}

#[tokio::test]
async fn a_notice_is_queued_with_its_incident_or_not_at_all() {
    let fixture = Fixture::new();
    let started = failed_instance(&fixture, "worker failed");
    let before = count(&fixture, "SELECT COUNT(*) FROM bpmn_incident_notices");
    {
        let mut conn = fixture.db.write().unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute(
            "INSERT INTO bpmn_incidents(incident_id,instance_id,scope_id,node_id,job_id,code,message,at_ms) VALUES('rolled-back',?1,?1,NULL,NULL,'INTERRUPTED','x',1)",
            [&started.instance_id],
        ).unwrap();
        let inside: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM bpmn_incident_notices WHERE incident_id='rolled-back'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            inside, 1,
            "the notice is part of the incident's own transaction"
        );
    }
    assert_eq!(
        count(&fixture, "SELECT COUNT(*) FROM bpmn_incident_notices"),
        before,
        "a rolled-back incident leaves no notice"
    );
}

#[tokio::test]
async fn a_crash_between_the_push_and_the_settling_repeats_the_push_with_the_same_id() {
    let fixture = Fixture::new();
    failed_instance(&fixture, "worker failed");
    let seen = RefCell::new(Vec::new());
    let crashed = catch_unwind(AssertUnwindSafe(|| {
        deliver_pending(&fixture.db, super::jobs::now_ms(), &|event| {
            seen.borrow_mut().push(event);
            panic!("process stopped after the push");
        })
    }));
    assert!(crashed.is_err());
    assert_eq!(
        count(
            &fixture,
            "SELECT COUNT(*) FROM bpmn_incident_notices WHERE notified_at_ms IS NULL"
        ),
        1
    );

    assert_eq!(
        deliver_pending(&fixture.db, super::jobs::now_ms(), &|event| seen
            .borrow_mut()
            .push(event))
        .unwrap(),
        1
    );
    let ids: Vec<String> = seen
        .borrow()
        .iter()
        .map(|event| match event {
            SystemEventPayload::UserNotification {
                notification_id, ..
            } => notification_id.clone(),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], ids[1], "the dashboard drops the repeat by this id");
}

#[tokio::test]
async fn nothing_is_queued_or_pushed_inside_a_private_simulation_database() {
    let fixture = Fixture::new();
    failed_instance(&fixture, "worker failed");
    let pending = count(
        &fixture,
        "SELECT COUNT(*) FROM bpmn_incident_notices WHERE notified_at_ms IS NULL",
    );
    assert_eq!(pending, 1);
    // The production schema never holds simulation_meta; with it the same database is a private one.
    fixture
        .db
        .write()
        .unwrap()
        .execute_batch("CREATE TABLE simulation_meta(id TEXT)")
        .unwrap();
    let pushed = Pushed::new();
    assert_eq!(pushed.deliver(&fixture), 0);
    assert!(pushed.0.borrow().is_empty());
    assert_eq!(
        count(
            &fixture,
            "SELECT COUNT(*) FROM bpmn_incident_notices WHERE notified_at_ms IS NULL"
        ),
        1
    );
    fail_second_incident(&fixture);
    assert_eq!(
        count(&fixture, "SELECT COUNT(*) FROM bpmn_incident_notices"),
        1,
        "the trigger queues nothing there"
    );
}

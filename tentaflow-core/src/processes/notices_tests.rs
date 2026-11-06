// ============ File: notices_tests.rs — BPMN B4.3 incident notices: recipients, dedup, redaction, simulation silence, crash safety ============

use std::cell::RefCell;
use std::panic::{catch_unwind, AssertUnwindSafe};

use tentaflow_protocol::processes::ActivityVerification;
use tentaflow_protocol::SystemEventPayload;

use super::notices::{deliver_pending, INCIDENT_NOTICE_KIND, PASS_SIZE, SINGLE_NOTICE_LIMIT};
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
async fn every_incident_is_its_own_notice() {
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

fn open_instance(fixture: &Fixture) -> tentaflow_protocol::processes::ProcessInstance {
    start_model(fixture, &user_model(Some(&fixture.participant.user_id)))
}

fn link_of(event: &SystemEventPayload) -> serde_json::Value {
    let SystemEventPayload::UserNotification { link_json, .. } = event else {
        panic!("not a notification")
    };
    serde_json::from_str(link_json).unwrap()
}

#[test]
fn an_idle_pass_never_takes_the_writer_lock_even_with_a_big_settled_table() {
    let fixture = Fixture::new();
    let started = open_instance(&fixture);
    {
        let conn = fixture.db.write().unwrap();
        conn.execute("PRAGMA foreign_keys=OFF", []).unwrap();
        for index in 0..3000 {
            conn.execute(
                "INSERT INTO bpmn_incident_notices(incident_id,user_id,queued_at_ms,notified_at_ms,suppressed) VALUES(?1,?2,1,2,0)",
                rusqlite::params![format!("settled-{index}"), started.instance_id],
            ).unwrap();
        }
        conn.execute("PRAGMA foreign_keys=ON", []).unwrap();
    }
    let writer = fixture.db.write().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let db = fixture.db.clone();
    let worker = std::thread::spawn(move || {
        sender
            .send(deliver_pending(&db, 1, &|_| panic!("nothing is pending")).unwrap())
            .unwrap();
    });
    let pushed = receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("an idle pass must not wait for the writer lock");
    assert_eq!(pushed, 0);
    drop(writer);
    worker.join().unwrap();
}

#[test]
fn notices_leave_with_their_incident_and_instance() {
    let fixture = Fixture::new();
    let started = open_instance(&fixture);
    raise_incident(&fixture, &started.instance_id);
    assert_eq!(
        count(&fixture, "SELECT COUNT(*) FROM bpmn_incident_notices"),
        1
    );
    fixture
        .db
        .write()
        .unwrap()
        .execute("DELETE FROM bpmn_incidents", [])
        .unwrap();
    assert_eq!(
        count(&fixture, "SELECT COUNT(*) FROM bpmn_incident_notices"),
        0
    );
}

#[test]
fn a_recipient_who_fails_the_check_does_not_hold_back_the_next_one() {
    let fixture = Fixture::new();
    let steward = member(&fixture, "steward", "role-org-viewer");
    let started = open_instance(&fixture);
    move_definition_to(&fixture, &started.definition_id, &steward);
    raise_incident(&fixture, &started.instance_id);
    fixture
        .db
        .write()
        .unwrap()
        .execute(
            "UPDATE user_accounts SET is_active=0 WHERE id=?1",
            [&fixture.owner.user_id],
        )
        .unwrap();
    let pushed = Pushed::new();
    assert_eq!(pushed.deliver(&fixture), 1);
    assert_eq!(pushed.users(), vec![steward.user_id.clone()]);
    assert_eq!(
        count(
            &fixture,
            "SELECT COUNT(*) FROM bpmn_incident_notices WHERE notified_at_ms IS NULL"
        ),
        0
    );
}

/// Behaviour on purpose: a notice is addressed to the owner at the time of the
/// incident. When the definition changes hands before delivery the previous
/// owner no longer reads the run and is not paged, and the new owner is not
/// paged for an incident that predates the transfer; the attention list is
/// where the new owner finds it.
#[test]
fn a_definition_that_changes_hands_after_queueing_pages_neither_owner() {
    let fixture = Fixture::new();
    let first = member(&fixture, "first-owner", "role-org-viewer");
    let second = member(&fixture, "second-owner", "role-org-viewer");
    let started = open_instance(&fixture);
    move_definition_to(&fixture, &started.definition_id, &first);
    raise_incident(&fixture, &started.instance_id);
    move_definition_to(&fixture, &started.definition_id, &second);

    let pushed = Pushed::new();
    assert_eq!(pushed.deliver(&fixture), 1);
    assert_eq!(
        pushed.users(),
        vec![fixture.owner.user_id.clone()],
        "only the initiator is paged"
    );
    assert_eq!(
        count(
            &fixture,
            "SELECT COUNT(*) FROM bpmn_incident_notices WHERE suppressed=1"
        ),
        1
    );
}

#[test]
fn a_recipient_of_a_suspended_organization_is_not_paged() {
    let fixture = Fixture::new();
    let started = open_instance(&fixture);
    raise_incident(&fixture, &started.instance_id);
    fixture
        .db
        .write()
        .unwrap()
        .execute("UPDATE organizations SET status='suspended'", [])
        .unwrap();
    let pushed = Pushed::new();
    assert_eq!(pushed.deliver(&fixture), 0);
    assert_eq!(
        count(
            &fixture,
            "SELECT COUNT(*) FROM bpmn_incident_notices WHERE suppressed=1"
        ),
        1
    );
}

#[test]
fn an_incident_in_an_organization_the_dashboard_does_not_open_is_not_paged() {
    let fixture = Fixture::new();
    let started = open_instance(&fixture);
    let other = crate::services::org::repo::create_organization(
        &fixture.db,
        "Other",
        "other-org",
        None,
        None,
        None,
        None,
    )
    .unwrap();
    crate::services::org::repo::add_membership(
        &fixture.db,
        &other.org_id,
        &fixture.owner.user_id,
        "role-org-viewer",
        &fixture.owner.user_id,
    )
    .unwrap();
    {
        let conn = fixture.db.write().unwrap();
        conn.execute("PRAGMA foreign_keys=OFF", []).unwrap();
        conn.execute("UPDATE bpmn_instances SET org_id=?1", [&other.org_id])
            .unwrap();
        conn.execute("UPDATE bpmn_definitions SET org_id=?1", [&other.org_id])
            .unwrap();
        conn.execute("PRAGMA foreign_keys=ON", []).unwrap();
    }
    raise_incident(&fixture, &started.instance_id);
    let pushed = Pushed::new();
    assert_eq!(
        pushed.deliver(&fixture),
        0,
        "the toast could only lead to a not-found run"
    );
    assert_eq!(
        count(
            &fixture,
            "SELECT COUNT(*) FROM bpmn_incident_notices WHERE suppressed=1"
        ),
        1
    );
}

#[test]
fn a_mass_failure_is_a_few_summaries_not_a_toast_per_run() {
    let fixture = Fixture::new();
    let started = open_instance(&fixture);
    let total = PASS_SIZE as usize * 2 + 100;
    for _ in 0..total {
        raise_incident(&fixture, &started.instance_id);
    }
    let pushed = Pushed::new();
    let mut passes = 0;
    while pushed.deliver(&fixture) > 0 {
        passes += 1;
    }
    assert_eq!(
        passes, 3,
        "more than one pass drains a backlog larger than a pass"
    );
    let events = pushed.0.borrow();
    assert_eq!(events.len(), 3);
    let summed: u64 = events
        .iter()
        .map(|event| {
            let link = link_of(event);
            assert_eq!(link["summary"], true);
            assert!(link.get("instanceId").is_none() && link.get("code").is_none());
            link["count"].as_u64().unwrap()
        })
        .sum();
    assert_eq!(summed as usize, total);
    assert_eq!(
        count(
            &fixture,
            "SELECT COUNT(*) FROM bpmn_incident_notices WHERE notified_at_ms IS NULL"
        ),
        0
    );
}

#[test]
fn a_few_incidents_in_one_pass_are_still_individual_notices() {
    let fixture = Fixture::new();
    let started = open_instance(&fixture);
    for _ in 0..SINGLE_NOTICE_LIMIT {
        raise_incident(&fixture, &started.instance_id);
    }
    let pushed = Pushed::new();
    assert_eq!(pushed.deliver(&fixture), SINGLE_NOTICE_LIMIT);
    assert!(pushed
        .0
        .borrow()
        .iter()
        .all(|event| link_of(event)["instanceId"] == started.instance_id));
}

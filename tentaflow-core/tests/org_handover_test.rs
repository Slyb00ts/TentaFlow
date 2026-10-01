// ============ File: tests/org_handover_test.rs ============
//
// The handover of everything a person holds ("Do przekazania") against a real
// organization database and REAL Project Studio databases (a registry plus one
// project.db per project): the three reasons and who may ask for them, the
// suggestions, the one-transaction structure part, the resumable Project Studio
// part (a failing step is recorded and finished by `retry`), the audit entry
// that carries a digest of the note and not the note, and the return of an
// absence handover on its return day (`run_due` takes the day, so the test
// runs it for a day that has not come yet).

use std::collections::HashSet;
use std::sync::{Arc, Once};

use chrono::{Days, NaiveDate};
use tentaflow_core::addon::event_bus::EventBus;
use tentaflow_core::addon::event_publish;
use tentaflow_core::dispatch::org_structure::org_structure_dispatch;
use tentaflow_core::dispatch::state::AppState;
use tentaflow_core::dispatch::HandlerContext;
use tentaflow_core::project_studio::{db as ps_db, project_db, repository, runs, tasks};
use tentaflow_core::services::org::{self, DEFAULT_ORG_ID};
use tentaflow_core::services::org_structure as svc;
use tentaflow_core::services::org_structure::availability::DeputyScope;
use tentaflow_core::services::org_structure::handover;
use tentaflow_core::services::rbac::OrgContext;
use tentaflow_protocol::org_structure::OrgStructurePayload as P;
use tentaflow_protocol::org_structure_handover::{
    OrgHandoverAction, OrgHandoverCategory as Cat, OrgHandoverChoice, OrgHandoverItem,
    OrgHandoverItemResult, OrgHandoverReason as Reason,
};
use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode, SessionAuth};

static PROJECT_STUDIO: Once = Once::new();

fn project_studio() {
    PROJECT_STUDIO.call_once(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        ps_db::init(&dir.path().join("projects.db")).expect("project studio registry");
        // The registry and every project database live as long as the test binary.
        std::mem::forget(dir);
    });
}

fn event_bus() -> Arc<EventBus> {
    if let Some(bus) = event_publish::global() {
        return bus;
    }
    event_publish::init_global(Arc::new(EventBus::new()));
    event_publish::global().expect("bus installed")
}

struct World {
    state: Arc<AppState>,
    admin: String,
    boss: String,
    leaver: String,
    peer: String,
    deputy_head: String,
    outsider: String,
    today: NaiveDate,
    unit: String,
    boss_position: String,
    leaver_position: String,
    leaver_assignment: String,
}

fn add_user(state: &AppState, member: bool, name: &str) -> String {
    let id = tentaflow_core::db::repository::create_user_account(
        &state.db,
        name,
        "hash",
        name,
        &format!("{name}@example.test"),
    )
    .unwrap();
    if member {
        let role_id: String = state
            .db
            .read()
            .unwrap()
            .query_row("SELECT role_id FROM roles LIMIT 1", [], |r| r.get(0))
            .unwrap();
        org::add_membership(&state.db, DEFAULT_ORG_ID, &id, &role_id, "test").unwrap();
    }
    id
}

fn plus(day: NaiveDate, n: u64) -> NaiveDate {
    day + Days::new(n)
}

fn s(day: NaiveDate) -> String {
    day.format("%Y-%m-%d").to_string()
}

fn world() -> World {
    event_bus();
    project_studio();
    let state = AppState::for_test();
    let admin = add_user(&state, true, "ho-admin");
    let boss = add_user(&state, true, "ho-boss");
    let leaver = add_user(&state, true, "ho-leaver");
    let peer = add_user(&state, true, "ho-peer");
    let deputy_head = add_user(&state, true, "ho-deputy-head");
    let outsider = add_user(&state, false, "ho-outsider");
    let pool = &state.db;
    let today = svc::org_today(pool, DEFAULT_ORG_ID).unwrap();
    let from = today.checked_sub_days(Days::new(60)).unwrap();
    let ctx = svc::WriteCtx {
        org_id: DEFAULT_ORG_ID,
        actor_user_id: &admin,
        confirm_backdated: true,
    };
    let unit = svc::create_unit(
        pool,
        &ctx,
        &svc::NewUnit {
            name: "IT".into(),
            code: None,
            type_id: None,
            parent_unit_id: None,
            color: None,
            valid_from: from,
            valid_to: None,
        },
    )
    .unwrap()
    .value
    .unit_id;
    let position = |name: &str, parent: Option<&str>| {
        svc::create_position(
            pool,
            &ctx,
            &svc::NewPosition {
                unit_id: unit.clone(),
                name: name.into(),
                code: None,
                role_id: None,
                is_manager: None,
                is_staff: false,
                parent_position_id: parent.map(str::to_string),
                valid_from: from,
                valid_to: None,
            },
        )
        .unwrap()
        .value
        .position_id
    };
    let boss_position = position("Kierownik", None);
    let leaver_position = position("Programista", Some(&boss_position));
    let peer_position = position("Analityk", Some(&boss_position));
    let deputy_position = position("Zastepca", Some(&boss_position));
    svc::set_head(pool, &ctx, &unit, Some(&boss_position), from).unwrap();
    svc::set_deputy_heads(
        pool,
        &ctx,
        &unit,
        std::slice::from_ref(&deputy_position),
        from,
    )
    .unwrap();
    let mut leaver_assignment = String::new();
    for (user, position_id) in [
        (&boss, &boss_position),
        (&leaver, &leaver_position),
        (&peer, &peer_position),
        (&deputy_head, &deputy_position),
    ] {
        let a = svc::assign(
            pool,
            &ctx,
            &svc::NewAssignment {
                position_id: position_id.clone(),
                subject: svc::Subject::User(user.clone()),
                kind: svc::AssignmentType::Permanent,
                share: 1.0,
                is_primary: None,
                valid_from: from,
                valid_to: None,
            },
        )
        .unwrap()
        .value;
        if user == &leaver {
            leaver_assignment = a.id;
        }
    }
    World {
        state,
        admin,
        boss,
        leaver,
        peer,
        deputy_head,
        outsider,
        today,
        unit,
        boss_position,
        leaver_position,
        leaver_assignment,
    }
}

fn ctx(w: &World, user: &str, admin: bool) -> HandlerContext {
    HandlerContext {
        session: SessionAuth::UserSession {
            user_id: [7u8; 16],
            role: Some("user".to_string()),
        },
        correlation_id: 1,
        connection_id: 0,
        resume_secret: None,
        state: w.state.clone(),
        origin: tentaflow_core::dispatch::RequestOrigin::Local,
        org_context: Some(OrgContext {
            user_id: user.to_string(),
            org_id: DEFAULT_ORG_ID.to_string(),
            role_id: "role-test".to_string(),
            permissions: if admin {
                HashSet::from(["org.admin".to_string()])
            } else {
                HashSet::new()
            },
        }),
    }
}

async fn run(ctx: &HandlerContext, payload: P) -> Result<P, ProtocolError> {
    match org_structure_dispatch(&MessageBody::OrgStructureBody(payload), ctx).await? {
        MessageBody::OrgStructureBody(p) => Ok(p),
        other => panic!("expected an OrgStructureBody answer, got {other:?}"),
    }
}

struct Listing {
    groups: Vec<(Cat, Vec<OrgHandoverItem>)>,
    date: String,
    ended: Option<String>,
}

async fn list(
    ctx: &HandlerContext,
    user: &str,
    reason: Reason,
    project: Option<&str>,
    date: Option<String>,
) -> Result<Listing, ProtocolError> {
    match run(
        ctx,
        P::HandoverListRequest {
            user_id: user.to_string(),
            reason,
            project_id: project.map(str::to_string),
            date,
            return_date: None,
        },
    )
    .await?
    {
        P::HandoverListResponse {
            groups,
            date,
            assignment_ended_on,
            ..
        } => Ok(Listing {
            groups: groups.into_iter().map(|g| (g.category, g.items)).collect(),
            date,
            ended: assignment_ended_on,
        }),
        other => panic!("expected a listing, got {other:?}"),
    }
}

impl Listing {
    fn items(&self, category: Cat) -> &[OrgHandoverItem] {
        self.groups
            .iter()
            .find(|(c, _)| *c == category)
            .map_or(&[][..], |(_, items)| items.as_slice())
    }

    fn titled(&self, category: Cat, part: &str) -> &OrgHandoverItem {
        self.items(category)
            .iter()
            .find(|i| i.title.contains(part))
            .unwrap_or_else(|| panic!("no {category:?} item with {part:?} in {:?}", self.groups))
    }
}

#[derive(Debug)]
struct Applied {
    ok: bool,
    handover_id: Option<String>,
    error_code: Option<String>,
    items: Vec<OrgHandoverItemResult>,
}

impl Applied {
    fn status_of(&self, part: &str) -> (&str, Option<&str>) {
        let item = self
            .items
            .iter()
            .find(|i| i.title.contains(part) || i.key.contains(part))
            .unwrap_or_else(|| panic!("no result for {part:?} in {:?}", self.items));
        (item.status.as_str(), item.reason.as_deref())
    }
}

fn choice(item: &OrgHandoverItem, taker: Option<&str>) -> OrgHandoverChoice {
    OrgHandoverChoice {
        key: item.key.clone(),
        taker_user_id: taker.map(str::to_string),
    }
}

#[allow(clippy::too_many_arguments)]
async fn apply(
    ctx: &HandlerContext,
    user: &str,
    reason: Reason,
    project: Option<&str>,
    date: Option<String>,
    return_date: Option<String>,
    note: &str,
    items: Vec<OrgHandoverChoice>,
) -> Result<Applied, ProtocolError> {
    match run(
        ctx,
        P::HandoverApplyRequest {
            user_id: user.to_string(),
            reason,
            project_id: project.map(str::to_string),
            date,
            return_date,
            note: note.to_string(),
            items,
        },
    )
    .await?
    {
        P::HandoverApplyResponse {
            ok,
            handover_id,
            error,
            items,
            ..
        } => Ok(Applied {
            ok,
            handover_id,
            error_code: error.map(|e| e.code),
            items,
        }),
        other => panic!("expected an apply answer, got {other:?}"),
    }
}

fn expect_code<T: std::fmt::Debug>(
    result: Result<T, ProtocolError>,
    code: ProtocolErrorCode,
    what: &str,
) {
    match result {
        Err(e) => assert_eq!(e.code, code, "{what}: {e:?}"),
        Ok(answer) => panic!("{what} was answered: {answer:?}"),
    }
}

/// A project with real rows: the owner and members, and the tasks and a test
/// run item of the person handing over.
struct Project {
    id: String,
    task_a: String,
    task_b: String,
    item: String,
}

fn project(w: &World, name: &str, owner: &str, members: &[(&str, &str)], holder: &str) -> Project {
    let id = uuid::Uuid::new_v4().to_string();
    let dir = tempfile::tempdir().expect("project dir");
    let path = dir.path().to_path_buf();
    std::mem::forget(dir);
    repository::create_project(
        &id,
        DEFAULT_ORG_ID,
        &format!("{name}-{}", &id[..6]),
        "",
        "custom",
        "[\"knowledge\",\"tasks\",\"tests\"]",
        owner,
        &path.to_string_lossy(),
        "",
        &members
            .iter()
            .filter(|(user, _)| *user != owner)
            .map(|(user, functions)| tentaflow_core::project_studio::models::MemberInput {
                user_id: user.to_string(),
                functions: if *functions == "administrator" { vec!["pm".to_string()] }
                    else { functions.split(',').map(str::to_string).collect() },
                project_admin: *functions == "administrator", expires_at: None,
            })
            .collect::<Vec<_>>(),
    )
    .expect("create project");
    let _ = w;
    let pool = project_db::open(&id).expect("project db");
    let task = |title: &str, status: &str, assignee: &str| {
        tasks::create_task(
            &pool,
            &tasks::TaskInput {
                task_type: "technical",
                title,
                description_md: "",
                severity: "",
                priority: "medium",
                status,
                assigned_to: assignee,
                due_date: "",
                links_json: "[]",
                attachments_json: "[]",
                parent_task_id: None,
            },
            owner,
        )
        .expect("task")
        .task_id
    };
    let task_a = task("Import OPC", "in_progress", holder);
    let task_b = task("Nakladka Gora", "todo", holder);
    task("Zrobione dawno", "done", holder);
    let item = uuid::Uuid::new_v4().to_string();
    {
        let conn = pool.write().unwrap();
        conn.execute(
            "INSERT INTO test_runs (run_id, run_no, name, assignment_mode, status, created_by) \
             VALUES ('run-1', 1, 'Regresja', 'single', 'running', ?1)",
            [owner],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO test_run_items (item_id, run_id, case_id, case_title, case_version, \
                position, assigned_to, status) VALUES (?1, 'run-1', 'c-1', 'Logowanie', 1, 0, ?2, 'pending')",
            [&item, holder],
        )
        .unwrap();
    }
    Project {
        id,
        task_a,
        task_b,
        item,
    }
}

fn assignee_of(project: &str, task_id: &str) -> String {
    let pool = project_db::open(project).unwrap();
    tasks::get_task(&pool, task_id)
        .unwrap()
        .unwrap()
        .assigned_to
}

fn item_assignee(project: &str, item_id: &str) -> String {
    let pool = project_db::open(project).unwrap();
    runs::get_run_item(&pool, item_id)
        .unwrap()
        .unwrap()
        .assigned_to
}

fn audit_details(w: &World, action: &str) -> Vec<String> {
    let conn = w.state.db.read().unwrap();
    let mut stmt = conn
        .prepare("SELECT details FROM audit_log WHERE action = ?1 ORDER BY rowid")
        .unwrap();
    stmt.query_map([action], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

// =============================================================================
// Departure
// =============================================================================

#[tokio::test]
async fn a_departure_lists_everything_the_person_holds_with_a_proposal_for_each() {
    let w = world();
    let p = project(
        &w,
        "dep",
        &w.boss,
        &[
            (&w.boss, "owner"),
            (&w.leaver, "developer,tester"),
            (&w.peer, "developer,tester"),
            (&w.deputy_head, "observer"),
        ],
        &w.leaver,
    );
    // The peer covers the leaver in that project.
    svc::set_deputy(
        &w.state.db,
        &svc::WriteCtx {
            org_id: DEFAULT_ORG_ID,
            actor_user_id: &w.admin,
            confirm_backdated: true,
        },
        &svc::NewDeputy {
            user_id: w.leaver.clone(),
            deputy_user_id: w.peer.clone(),
            scope: DeputyScope::Project(p.id.clone()),
            valid_from: w.today,
            valid_to: None,
        },
    )
    .unwrap();
    let admin = ctx(&w, &w.admin, true);
    let listing = list(&admin, &w.leaver, Reason::Departure, None, None)
        .await
        .unwrap();

    // Two open tasks (the finished one is not held) and one test item, by project.
    assert_eq!(listing.items(Cat::Task).len(), 2);
    assert_eq!(listing.items(Cat::TestItem).len(), 1);
    let task = listing.titled(Cat::Task, "Import OPC");
    assert_eq!(task.state, "in_progress");
    assert_eq!(task.action, OrgHandoverAction::Transfer);
    // The deputy of the project comes before the manager and before the project owner.
    let suggestion = task.suggestion.as_ref().expect("a proposal");
    assert_eq!(
        (suggestion.user_id.as_str(), suggestion.reason.as_str()),
        (w.peer.as_str(), "deputy")
    );
    // Only members of the project may take it.
    let eligible = task.eligible_user_ids.as_ref().unwrap();
    assert!(
        eligible.contains(&w.peer) && !eligible.contains(&w.leaver) && !eligible.contains(&w.admin)
    );
    // The test item needs a tester or better: the viewer is not offered.
    let test_item = &listing.items(Cat::TestItem)[0];
    assert!(!test_item
        .eligible_user_ids
        .as_ref()
        .unwrap()
        .contains(&w.deputy_head));

    let membership = &listing.items(Cat::Membership)[0];
    assert_eq!(
        (membership.role.as_str(), membership.action),
        ("member", OrgHandoverAction::End)
    );
    let position = listing.titled(Cat::Position, "Programista");
    assert_eq!(position.action, OrgHandoverAction::TransferOrEnd);
    // Not a head seat: the cover first, and there is none for "all", so the manager.
    assert_eq!(position.suggestion.as_ref().unwrap().reason, "manager");
    assert_eq!(listing.items(Cat::Deputy).len(), 1);
    assert_eq!(listing.items(Cat::Deputy)[0].role, "covered");
    assert_eq!(listing.date, s(w.today));
}

#[tokio::test]
async fn only_an_administrator_hands_over_a_departure_and_a_note_is_required() {
    let w = world();
    let p = project(
        &w,
        "perm",
        &w.boss,
        &[
            (&w.boss, "owner"),
            (&w.leaver, "developer,tester"),
            (&w.peer, "developer,tester"),
        ],
        &w.leaver,
    );
    let member = ctx(&w, &w.boss, false);
    expect_code(
        list(&member, &w.leaver, Reason::Departure, None, None)
            .await
            .map(|l| l.date),
        ProtocolErrorCode::PolicyDenied,
        "a manager lists a departure",
    );
    let outsider = ctx(&w, &w.outsider, false);
    expect_code(
        list(&outsider, &w.leaver, Reason::Departure, None, None)
            .await
            .map(|l| l.date),
        ProtocolErrorCode::NotFound,
        "an outsider lists a departure",
    );
    let admin = ctx(&w, &w.admin, true);
    let listing = list(&admin, &w.leaver, Reason::Departure, None, None)
        .await
        .unwrap();
    let task = listing.titled(Cat::Task, "Import OPC");
    let blank = apply(
        &admin,
        &w.leaver,
        Reason::Departure,
        None,
        None,
        None,
        "   ",
        vec![choice(task, Some(&w.peer))],
    )
    .await
    .unwrap();
    assert!(!blank.ok);
    assert_eq!(blank.error_code.as_deref(), Some("empty_field"));
    assert_eq!(
        assignee_of(&p.id, &p.task_a),
        w.leaver,
        "nothing moved without a note"
    );
    expect_code(
        apply(
            &member,
            &w.leaver,
            Reason::Departure,
            None,
            None,
            None,
            "x",
            vec![choice(task, Some(&w.peer))],
        )
        .await
        .map(|a| a.ok),
        ProtocolErrorCode::PolicyDenied,
        "a manager applies a departure",
    );
}

#[tokio::test]
async fn a_departure_moves_the_work_the_seat_and_the_covers_and_leaves_an_audited_record() {
    let w = world();
    let p = project(
        &w,
        "move",
        &w.boss,
        &[
            (&w.boss, "owner"),
            (&w.leaver, "developer,tester"),
            (&w.peer, "developer,tester"),
            (&w.deputy_head, "tester"),
        ],
        &w.leaver,
    );
    let ctx_admin = SvcCtx::new(&w);
    // The leaver covers the peer, who is covered by nobody else.
    svc::set_deputy(
        &w.state.db,
        &ctx_admin.write(),
        &svc::NewDeputy {
            user_id: w.peer.clone(),
            deputy_user_id: w.leaver.clone(),
            scope: DeputyScope::All,
            valid_from: w.today,
            valid_to: Some(plus(w.today, 30)),
        },
    )
    .unwrap();
    let admin = ctx(&w, &w.admin, true);
    let listing = list(&admin, &w.leaver, Reason::Departure, None, None)
        .await
        .unwrap();
    let note = "Galaz feature/opc, MR !12 czeka na przeglad";
    let mut choices = vec![
        choice(listing.titled(Cat::Task, "Import OPC"), Some(&w.peer)),
        choice(listing.titled(Cat::Task, "Nakladka"), Some(&w.peer)),
        choice(&listing.items(Cat::TestItem)[0], Some(&w.deputy_head)),
        choice(&listing.items(Cat::Membership)[0], None),
        choice(
            listing.titled(Cat::Position, "Programista"),
            Some(&w.deputy_head),
        ),
    ];
    let deputy = &listing.items(Cat::Deputy)[0];
    assert_eq!(deputy.role, "deputy");
    choices.push(choice(deputy, Some(&w.boss)));

    let applied = apply(
        &admin,
        &w.leaver,
        Reason::Departure,
        None,
        None,
        None,
        note,
        choices.clone(),
    )
    .await
    .unwrap();
    assert!(applied.ok, "{:?}", applied.items);
    assert!(
        applied.items.iter().all(|i| i.status == "done"),
        "{:?}",
        applied.items
    );

    assert_eq!(assignee_of(&p.id, &p.task_a), w.peer);
    assert_eq!(assignee_of(&p.id, &p.task_b), w.peer);
    assert_eq!(item_assignee(&p.id, &p.item), w.deputy_head);
    assert!(
        repository::member_access(&p.id, &w.leaver)
            .unwrap()
            .is_none(),
        "the membership ended with the work"
    );
    // The note is on the task the taker opens.
    let pool = project_db::open(&p.id).unwrap();
    let comments = tasks::list_comments(&pool, &p.task_a).unwrap();
    assert!(comments
        .iter()
        .any(|c| c.body_md == note && c.author_user_id == w.admin));
    let events = tasks::list_task_events(&pool, &p.task_a, None, 100).unwrap().0;
    let handover_event = events.iter().find(|event| event.kind == "handed_over").expect("real atomic handover history");
    assert_eq!(handover_event.actor_id, w.admin);
    assert_eq!(handover_event.actor_kind, "user");
    let after: serde_json::Value = serde_json::from_str(&handover_event.after_json).unwrap();
    assert_eq!(after["assigned_to"], w.peer);
    assert!(comments.iter().any(|comment| Some(comment.comment_id.as_str()) == after["comment_id"].as_str()));


    // The seat: the leaver's assignment ends today and the taker stands in as acting.
    let snapshot =
        svc::query::Snapshot::load(&w.state.db.read().unwrap(), DEFAULT_ORG_ID, w.today).unwrap();
    assert!(snapshot.assignments_of(&w.leaver).next().is_none());
    let acting: Vec<_> = snapshot
        .holders_of(&w.leaver_position)
        .filter(|a| a.subject == svc::Subject::User(w.deputy_head.clone()))
        .collect();
    assert_eq!(acting.len(), 1);
    assert_eq!(acting[0].kind, svc::AssignmentType::Acting);
    // The cover the leaver gave now belongs to the boss.
    let avail =
        svc::availability::Availability::load(&w.state.db.read().unwrap(), DEFAULT_ORG_ID, w.today)
            .unwrap();
    assert!(avail
        .deputies
        .iter()
        .any(|d| d.user_id == w.peer && d.deputy_user_id == w.boss));
    assert!(!avail.deputies.iter().any(|d| d.deputy_user_id == w.leaver));

    // The audit chain has ONE entry for the handover: a digest and the length of the note, not the note.
    let entries = audit_details(&w, "org.handover.apply");
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0].contains("note_sha256")
            && entries[0].contains(&format!("\"note_chars\":{}", note.chars().count())),
        "{}",
        entries[0]
    );
    assert!(
        !entries[0].contains("feature/opc"),
        "the note is not in the audit chain"
    );
    // The structure part is audited by its own write session: ONE entry that lists the operations.
    let structure = audit_details(&w, "org.handover.org_items");
    assert_eq!(structure.len(), 1);
    assert!(
        structure[0].contains("org.assignment.end") && structure[0].contains("org.deputy.set"),
        "{}",
        structure[0]
    );

    // The record has the note and the state of every item.
    let records = handover::records(
        &w.state.db,
        DEFAULT_ORG_ID,
        &handover::Actor {
            user_id: &w.admin,
            is_admin: true,
        },
        &w.leaver,
    )
    .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].note, note);
    assert_eq!(records[0].items.len(), 6);
    assert!(records[0].items.iter().all(|i| i.status == "done"));

    // Repeating the request finds nothing held any more: nothing is moved twice and no second record is made.
    let again = apply(
        &admin,
        &w.leaver,
        Reason::Departure,
        None,
        None,
        None,
        note,
        choices,
    )
    .await
    .unwrap();
    assert!(!again.ok && again.handover_id.is_none());
    assert!(
        again.items.iter().all(|i| i.status == "skipped"),
        "{:?}",
        again.items
    );
    assert_eq!(assignee_of(&p.id, &p.task_a), w.peer);
}

struct SvcCtx {
    admin: String,
}

impl SvcCtx {
    fn new(w: &World) -> Self {
        Self {
            admin: w.admin.clone(),
        }
    }

    fn write(&self) -> svc::WriteCtx<'_> {
        svc::WriteCtx {
            org_id: DEFAULT_ORG_ID,
            actor_user_id: &self.admin,
            confirm_backdated: true,
        }
    }
}

#[tokio::test]
async fn a_taker_who_cannot_take_the_item_is_refused_before_anything_moves() {
    let w = world();
    let p = project(
        &w,
        "elig",
        &w.boss,
        &[
            (&w.boss, "owner"),
            (&w.leaver, "developer,tester"),
            (&w.deputy_head, "observer"),
        ],
        &w.leaver,
    );
    let admin = ctx(&w, &w.admin, true);
    let listing = list(&admin, &w.leaver, Reason::Departure, None, None)
        .await
        .unwrap();
    let task = listing.titled(Cat::Task, "Import OPC");
    let item = &listing.items(Cat::TestItem)[0];
    // The peer is not a member of the project; the viewer may not run tests; nobody is their own taker.
    for (choice_of, taker) in [
        (task, &w.peer),
        (item, &w.deputy_head),
        (task, &w.leaver),
        (task, &w.outsider),
    ] {
        let answer = apply(
            &admin,
            &w.leaver,
            Reason::Departure,
            None,
            None,
            None,
            "n",
            vec![
                choice(choice_of, Some(taker)),
                choice(listing.titled(Cat::Task, "Nakladka"), Some(&w.boss)),
            ],
        )
        .await
        .unwrap();
        assert!(!answer.ok && answer.handover_id.is_none(), "{:?}", answer);
        assert_eq!(answer.error_code.as_deref(), Some("invalid_value"));
        assert_eq!(
            answer.status_of(&choice_of.key),
            ("failed", Some("taker_not_eligible"))
        );
    }
    assert_eq!(
        assignee_of(&p.id, &p.task_b),
        w.leaver,
        "the valid choice next to a bad one was not applied either"
    );
    // A choice without a taker where one is required.
    let missing = apply(
        &admin,
        &w.leaver,
        Reason::Departure,
        None,
        None,
        None,
        "n",
        vec![choice(task, None)],
    )
    .await
    .unwrap();
    assert_eq!(
        missing.status_of(&task.key),
        ("failed", Some("taker_required"))
    );
}

#[tokio::test]
async fn a_refused_seat_leaves_the_structure_and_everything_else_untouched() {
    let w = world();
    let p = project(
        &w,
        "atomic",
        &w.boss,
        &[
            (&w.boss, "owner"),
            (&w.leaver, "developer,tester"),
            (&w.peer, "developer,tester"),
        ],
        &w.leaver,
    );
    let admin = ctx(&w, &w.admin, true);
    // A departure dated in the past needs a confirmation the handover does not give: the seat is refused.
    let past = s(w.today.checked_sub_days(Days::new(3)).unwrap());
    let listing = list(
        &admin,
        &w.leaver,
        Reason::Departure,
        None,
        Some(past.clone()),
    )
    .await
    .unwrap();
    let position = listing.titled(Cat::Position, "Programista");
    let task = listing.titled(Cat::Task, "Import OPC");
    let answer = apply(
        &admin,
        &w.leaver,
        Reason::Departure,
        None,
        Some(past),
        None,
        "n",
        vec![choice(position, Some(&w.peer)), choice(task, Some(&w.peer))],
    )
    .await
    .unwrap();
    assert!(!answer.ok);
    let handover_id = answer.handover_id.clone().expect("the attempt is recorded");
    assert_eq!(
        answer.status_of("Programista"),
        ("failed", Some("backdated_confirmation_required"))
    );
    assert_eq!(
        answer.status_of("Import OPC"),
        ("not_started", Some("org_failed"))
    );
    assert_eq!(assignee_of(&p.id, &p.task_a), w.leaver);
    let snapshot =
        svc::query::Snapshot::load(&w.state.db.read().unwrap(), DEFAULT_ORG_ID, w.today).unwrap();
    assert!(
        snapshot
            .assignments_of(&w.leaver)
            .any(|a| a.id == w.leaver_assignment),
        "the seat is still held"
    );

    // The record kept what failed and what never started: the work is not lost in between.
    let records = handover::records(
        &w.state.db,
        DEFAULT_ORG_ID,
        &handover::Actor {
            user_id: &w.admin,
            is_admin: true,
        },
        &w.leaver,
    )
    .unwrap();
    let statuses: Vec<(&str, &str)> = records[0]
        .items
        .iter()
        .map(|i| (i.title.as_str(), i.status.as_str()))
        .collect();
    assert!(
        statuses
            .iter()
            .any(|(t, st)| t.contains("Programista") && *st == "failed"),
        "{statuses:?}"
    );
    assert!(
        statuses
            .iter()
            .any(|(t, st)| t.contains("Import OPC") && *st == "pending"),
        "{statuses:?}"
    );
    let _ = handover_id;
}

#[tokio::test]
async fn a_failing_project_step_is_recorded_and_a_retry_finishes_it() {
    let w = world();
    let p = project(
        &w,
        "retry",
        &w.boss,
        &[
            (&w.boss, "owner"),
            (&w.leaver, "developer,tester"),
            (&w.peer, "developer,tester"),
        ],
        &w.leaver,
    );
    // A trigger makes exactly one task refuse the write, as a full disk or a locked file would.
    {
        let pool = project_db::open(&p.id).unwrap();
        pool.write()
            .unwrap()
            .execute_batch(&format!(
                "CREATE TRIGGER refuse_one BEFORE UPDATE OF assigned_to ON tasks \
                 WHEN OLD.task_id = '{}' BEGIN SELECT RAISE(ABORT, 'disk is full'); END;",
                p.task_b
            ))
            .unwrap();
    }
    let admin = ctx(&w, &w.admin, true);
    let listing = list(&admin, &w.leaver, Reason::Departure, None, None)
        .await
        .unwrap();
    let answer = apply(
        &admin,
        &w.leaver,
        Reason::Departure,
        None,
        None,
        None,
        "notatka",
        vec![
            choice(listing.titled(Cat::Task, "Import OPC"), Some(&w.peer)),
            choice(listing.titled(Cat::Task, "Nakladka"), Some(&w.peer)),
            choice(&listing.items(Cat::TestItem)[0], Some(&w.peer)),
            choice(&listing.items(Cat::Membership)[0], None),
        ],
    )
    .await
    .unwrap();
    // No silent partial success: the answer says which moved and which did not.
    assert!(!answer.ok);
    let handover_id = answer.handover_id.clone().expect("recorded");
    assert_eq!(answer.status_of("Import OPC").0, "done");
    assert_eq!(answer.status_of("Nakladka"), ("failed", Some("internal")));
    // The membership stays while the person still holds open work in the project.
    assert_eq!(
        answer.status_of("member:"),
        ("failed", Some("still_holds_work"))
    );
    assert_eq!(assignee_of(&p.id, &p.task_a), w.peer);
    assert_eq!(assignee_of(&p.id, &p.task_b), w.leaver);

    // The fault goes away; the retry takes the recorded takers and finishes only what failed.
    project_db::open(&p.id)
        .unwrap()
        .write()
        .unwrap()
        .execute_batch("DROP TRIGGER refuse_one;")
        .unwrap();
    let retried = match run(
        &admin,
        P::HandoverRetryRequest {
            handover_id: handover_id.clone(),
            keys: vec![],
        },
    )
    .await
    .unwrap()
    {
        P::HandoverApplyResponse { ok, items, .. } => (ok, items),
        other => panic!("{other:?}"),
    };
    assert!(retried.0, "{:?}", retried.1);
    assert!(
        retried.1.iter().all(|i| i.status == "done"),
        "{:?}",
        retried.1
    );
    assert_eq!(assignee_of(&p.id, &p.task_b), w.peer);
    assert!(repository::member_access(&p.id, &w.leaver)
        .unwrap()
        .is_none());
    // Nothing to retry any more: a normal refusal, not a protocol error.
    match run(
        &admin,
        P::HandoverRetryRequest {
            handover_id,
            keys: vec![],
        },
    )
    .await
    .unwrap()
    {
        P::HandoverApplyResponse {
            ok: false,
            error: Some(error),
            ..
        } => assert_eq!(error.code, "invalid_value"),
        other => panic!("{other:?}"),
    }
    // Somebody who may not hand this over cannot retry it either.
    let member = ctx(&w, &w.peer, false);
    expect_code(
        run(
            &member,
            P::HandoverRetryRequest {
                handover_id: "no-such-handover".into(),
                keys: vec![],
            },
        )
        .await,
        ProtocolErrorCode::NotFound,
        "an unknown handover",
    );
}

#[tokio::test]
async fn a_membership_scheduled_for_the_departure_day_ends_on_that_day() {
    let w = world();
    let p = project(
        &w,
        "sched",
        &w.boss,
        &[
            (&w.boss, "owner"),
            (&w.leaver, "developer,tester"),
            (&w.peer, "developer,tester"),
        ],
        &w.leaver,
    );
    let admin = ctx(&w, &w.admin, true);
    let leaving = plus(w.today, 10);
    let listing = list(&admin, &w.leaver, Reason::Departure, None, Some(s(leaving)))
        .await
        .unwrap();
    let answer = apply(
        &admin,
        &w.leaver,
        Reason::Departure,
        None,
        Some(s(leaving)),
        None,
        "odchodze",
        vec![
            choice(listing.titled(Cat::Task, "Import OPC"), Some(&w.peer)),
            choice(listing.titled(Cat::Task, "Nakladka"), Some(&w.peer)),
            choice(&listing.items(Cat::TestItem)[0], Some(&w.peer)),
            choice(&listing.items(Cat::Membership)[0], None),
        ],
    )
    .await
    .unwrap();
    assert!(answer.ok, "{:?}", answer.items);
    assert_eq!(answer.status_of("member:").0, "scheduled");
    assert!(
        repository::member_access(&p.id, &w.leaver)
            .unwrap()
            .is_some(),
        "still a member until the day"
    );

    let due = |day: NaiveDate| handover::run_due(&w.state.db, DEFAULT_ORG_ID, day).unwrap();
    assert_eq!(
        due(plus(w.today, 9)),
        handover::DueReport::default(),
        "not yet"
    );
    assert!(repository::member_access(&p.id, &w.leaver)
        .unwrap()
        .is_some());
    let report = due(leaving);
    assert_eq!(report.ended, 1, "{report:?}");
    assert!(repository::member_access(&p.id, &w.leaver)
        .unwrap()
        .is_none());
    // Running the day again finds nothing left to do.
    assert_eq!(due(leaving), handover::DueReport::default());
}

// =============================================================================
// Absence
// =============================================================================

#[tokio::test]
async fn an_absence_handover_is_temporary_and_the_work_comes_back_unless_the_taker_closed_or_changed_it(
) {
    let w = world();
    let p = project(
        &w,
        "abs",
        &w.boss,
        &[
            (&w.boss, "owner"),
            (&w.leaver, "developer,tester"),
            (&w.peer, "developer,tester"),
            (&w.deputy_head, "developer,tester"),
        ],
        &w.leaver,
    );
    let me = ctx(&w, &w.leaver, false);
    let back = plus(w.today, 7);
    // Only work is on the list for an absence: no seat, membership or cover moves.
    let listing = list(&me, &w.leaver, Reason::Absence, None, None)
        .await
        .unwrap();
    assert_eq!(
        listing.groups.iter().map(|(c, _)| *c).collect::<Vec<_>>(),
        vec![Cat::Task, Cat::TestItem]
    );

    // The return day is required, and must come after today.
    let no_day = apply(
        &me,
        &w.leaver,
        Reason::Absence,
        None,
        None,
        None,
        "urlop",
        vec![choice(
            listing.titled(Cat::Task, "Import OPC"),
            Some(&w.peer),
        )],
    )
    .await
    .unwrap();
    assert_eq!(no_day.error_code.as_deref(), Some("empty_field"));
    let past = apply(
        &me,
        &w.leaver,
        Reason::Absence,
        None,
        None,
        Some(s(w.today)),
        "urlop",
        vec![choice(
            listing.titled(Cat::Task, "Import OPC"),
            Some(&w.peer),
        )],
    )
    .await
    .unwrap();
    assert_eq!(past.error_code.as_deref(), Some("invalid_value"));

    // Somebody else may not hand it over (a colleague), the manager and the person may.
    let colleague = ctx(&w, &w.peer, false);
    expect_code(
        list(&colleague, &w.leaver, Reason::Absence, None, None)
            .await
            .map(|l| l.date),
        ProtocolErrorCode::PolicyDenied,
        "a colleague hands over an absence",
    );
    let boss = ctx(&w, &w.boss, false);
    assert!(list(&boss, &w.leaver, Reason::Absence, None, None)
        .await
        .is_ok());

    let answer = apply(
        &me,
        &w.leaver,
        Reason::Absence,
        None,
        None,
        Some(s(back)),
        "Wracam 7 dni",
        vec![
            choice(listing.titled(Cat::Task, "Import OPC"), Some(&w.peer)),
            choice(listing.titled(Cat::Task, "Nakladka"), Some(&w.peer)),
            choice(&listing.items(Cat::TestItem)[0], Some(&w.peer)),
        ],
    )
    .await
    .unwrap();
    assert!(answer.ok, "{:?}", answer.items);
    assert_eq!(assignee_of(&p.id, &p.task_a), w.peer);
    assert_eq!(item_assignee(&p.id, &p.item), w.peer);
    assert!(
        repository::member_access(&p.id, &w.leaver)
            .unwrap()
            .is_some(),
        "the person stays a member"
    );

    // While away: the taker closes one task, somebody gives another to a third person.
    {
        let pool = project_db::open(&p.id).unwrap();
        tasks::set_task_status(&pool, &p.task_a, "done", &w.peer).unwrap();
        assert!(tasks::reassign_open(&pool, &p.task_b, &w.peer, &w.deputy_head, &tasks::TaskHandoverInput {
            actor: &w.peer, note_md: "Changed assignment while away", mention_user_ids: &[],
            direction: tasks::TaskHandoverDirection::Over, handover_id: None,
        }).unwrap().expect("transferred").changed);
    }

    let due = |day: NaiveDate| handover::run_due(&w.state.db, DEFAULT_ORG_ID, day).unwrap();
    assert_eq!(
        due(plus(w.today, 6)),
        handover::DueReport::default(),
        "before the return day nothing moves"
    );
    let report = due(back);
    assert_eq!(
        (report.returned, report.kept, report.failed),
        (1, 2, 0),
        "{report:?}"
    );
    assert_eq!(
        item_assignee(&p.id, &p.item),
        w.leaver,
        "an untouched item goes back"
    );
    assert_eq!(
        assignee_of(&p.id, &p.task_a),
        w.peer,
        "a closed task stays with the taker"
    );
    assert_eq!(
        assignee_of(&p.id, &p.task_b),
        w.deputy_head,
        "a task somebody moved on is not taken back"
    );

    let records = handover::records(
        &w.state.db,
        DEFAULT_ORG_ID,
        &handover::Actor {
            user_id: &w.leaver,
            is_admin: false,
        },
        &w.leaver,
    )
    .unwrap();
    let state_of = |part: &str| {
        let item = records[0]
            .items
            .iter()
            .find(|i| i.title.contains(part))
            .unwrap();
        (item.status.clone(), item.reason.clone())
    };
    assert_eq!(
        state_of("Import OPC"),
        ("kept".into(), Some("closed".into()))
    );
    assert_eq!(
        state_of("Nakladka"),
        ("kept".into(), Some("changed".into()))
    );
    assert_eq!(state_of("Logowanie").0, "returned");
    // A second run does not touch a returned item again.
    assert_eq!(due(plus(back, 1)), handover::DueReport::default());
}

// =============================================================================
// Project removal
// =============================================================================

#[tokio::test]
async fn a_project_removal_moves_only_that_projects_items_and_needs_its_manager() {
    let w = world();
    let mine = project(
        &w,
        "rem-a",
        &w.boss,
        &[
            (&w.boss, "owner"),
            (&w.leaver, "developer,tester"),
            (&w.peer, "developer,tester"),
        ],
        &w.leaver,
    );
    let other = project(
        &w,
        "rem-b",
        &w.boss,
        &[
            (&w.boss, "owner"),
            (&w.leaver, "developer,tester"),
            (&w.peer, "developer,tester"),
        ],
        &w.leaver,
    );
    let boss = ctx(&w, &w.boss, false);
    let listing = list(
        &boss,
        &w.leaver,
        Reason::ProjectRemoval,
        Some(&mine.id),
        None,
    )
    .await
    .unwrap();
    assert!(listing
        .items(Cat::Task)
        .iter()
        .all(|i| i.project_id.as_deref() == Some(mine.id.as_str())));
    assert!(
        listing.items(Cat::Position).is_empty() && listing.items(Cat::Deputy).is_empty(),
        "the seat is not part of a project removal"
    );
    assert_eq!(listing.items(Cat::Membership).len(), 1);

    // A plain editor of the project, the administrator without a role in it and an outsider are refused.
    for who in [&w.peer, &w.outsider] {
        let c = ctx(&w, who, false);
        assert!(
            list(&c, &w.leaver, Reason::ProjectRemoval, Some(&mine.id), None)
                .await
                .is_err()
        );
    }
    let admin = ctx(&w, &w.admin, true);
    assert!(
        list(
            &admin,
            &w.leaver,
            Reason::ProjectRemoval,
            Some(&mine.id),
            None
        )
        .await
        .is_err(),
        "an administrator has no content rights in a project they are not in"
    );

    let answer = apply(
        &boss,
        &w.leaver,
        Reason::ProjectRemoval,
        Some(&mine.id),
        None,
        None,
        "zmiana zadan",
        vec![
            choice(listing.titled(Cat::Task, "Import OPC"), Some(&w.peer)),
            choice(listing.titled(Cat::Task, "Nakladka"), Some(&w.peer)),
            choice(&listing.items(Cat::TestItem)[0], Some(&w.peer)),
            choice(&listing.items(Cat::Membership)[0], None),
        ],
    )
    .await
    .unwrap();
    assert!(answer.ok, "{:?}", answer.items);
    assert_eq!(assignee_of(&mine.id, &mine.task_a), w.peer);
    assert!(repository::member_access(&mine.id, &w.leaver)
        .unwrap()
        .is_none());
    // The other project, the seat and everything else are as they were.
    assert_eq!(assignee_of(&other.id, &other.task_a), w.leaver);
    assert!(repository::member_access(&other.id, &w.leaver)
        .unwrap()
        .is_some());
    let snapshot =
        svc::query::Snapshot::load(&w.state.db.read().unwrap(), DEFAULT_ORG_ID, w.today).unwrap();
    assert!(snapshot.assignments_of(&w.leaver).next().is_some());
    // The project's own activity log tells who handed over what to whom.
    let pool = project_db::open(&mine.id).unwrap();
    let (activity, _) = repository::list_activity(&pool, None, 50).unwrap();
    assert!(
        activity
            .iter()
            .any(|a| a.action == "task.handed_over" && a.actor_user_id == w.boss),
        "{activity:?}"
    );
}

#[tokio::test]
async fn project_administrators_manage_non_owner_admins_and_owners_transfer_first() {
    let w = world();
    let p = project(
        &w,
        "roles",
        &w.boss,
        &[
            (&w.boss, "owner"),
            (&w.leaver, "administrator"),
            (&w.peer, "administrator"),
            (&w.deputy_head, "developer"),
        ],
        &w.leaver,
    );
    assert!(
        list(
            &ctx(&w, &w.deputy_head, false),
            &w.leaver,
            Reason::ProjectRemoval,
            Some(&p.id),
            None
        )
        .await
        .is_err(),
        "an ordinary member cannot manage project administration"
    );

    // Project administrators manage non-owner administrators after handing over their work.
    let peer = ctx(&w, &w.peer, false);
    let listing = list(&peer, &w.leaver, Reason::ProjectRemoval, Some(&p.id), None)
        .await
        .unwrap();
    let answer = apply(
        &peer,
        &w.leaver,
        Reason::ProjectRemoval,
        Some(&p.id),
        None,
        None,
        "n",
        vec![
            choice(listing.titled(Cat::Task, "Import OPC"), Some(&w.peer)),
            choice(listing.titled(Cat::Task, "Nakladka"), Some(&w.peer)),
            choice(&listing.items(Cat::TestItem)[0], Some(&w.peer)),
            choice(&listing.items(Cat::Membership)[0], None),
        ],
    )
    .await
    .unwrap();
    assert_eq!(answer.status_of("member:"), ("done", None));
    assert!(repository::member_access(&p.id, &w.leaver)
        .unwrap()
        .is_none());

    // The owner leaving must name who takes the project.
    let admin = ctx(&w, &w.admin, true);
    let listing = list(&admin, &w.boss, Reason::Departure, None, None)
        .await
        .unwrap();
    let ownership = listing
        .items(Cat::Membership)
        .iter()
        .find(|m| m.project_id.as_deref() == Some(p.id.as_str()))
        .unwrap();
    assert_eq!(ownership.action, OrgHandoverAction::Transfer);
    let without = apply(
        &admin,
        &w.boss,
        Reason::Departure,
        None,
        None,
        None,
        "n",
        vec![choice(ownership, None)],
    )
    .await
    .unwrap();
    assert_eq!(
        without.status_of("member:"),
        ("failed", Some("taker_required")),
        "{:?}",
        without.items
    );
    let with = apply(
        &admin,
        &w.boss,
        Reason::Departure,
        None,
        None,
        None,
        "n",
        vec![choice(ownership, Some(&w.peer))],
    )
    .await
    .unwrap();
    assert!(with.ok, "{:?}", with.items);
    assert!(repository::member_access(&p.id, &w.boss).unwrap().is_none());
    let project = repository::get_project(DEFAULT_ORG_ID, &p.id)
        .unwrap()
        .unwrap();
    assert_eq!(project.owner_user_id, w.peer);
    let new_owner = repository::member_access(&p.id, &w.peer).unwrap().unwrap();
    assert!(new_owner.project_admin);
    assert!(new_owner.expires_at.is_none());
}
// =============================================================================
// After the assignment ended
// =============================================================================

#[tokio::test]
async fn people_whose_assignment_ended_and_who_still_hold_work_are_counted_for_the_administrator() {
    let w = world();
    let p = project(
        &w,
        "pend",
        &w.boss,
        &[
            (&w.boss, "owner"),
            (&w.leaver, "developer,tester"),
            (&w.peer, "developer,tester"),
        ],
        &w.leaver,
    );
    let admin = ctx(&w, &w.admin, true);
    let pending = || async {
        match run(&admin, P::HandoverPendingRequest {}).await.unwrap() {
            P::HandoverPendingResponse { people } => people,
            other => panic!("{other:?}"),
        }
    };
    assert!(
        pending().await.is_empty(),
        "somebody who still has a seat is not pending"
    );
    svc::end_assignment(
        &w.state.db,
        &SvcCtx::new(&w).write(),
        &w.leaver_assignment,
        w.today,
    )
    .unwrap();
    let people = pending().await;
    assert_eq!(people.len(), 1, "{people:?}");
    assert_eq!(
        (people[0].user_id.as_str(), people[0].count),
        (w.leaver.as_str(), 4),
        "two tasks, a test item and a membership"
    );
    assert_eq!(people[0].ended_on.as_deref(), Some(s(w.today).as_str()));

    // The screen for that person defaults to the day the assignment ended and has no seat left to move.
    let listing = list(&admin, &w.leaver, Reason::Departure, None, None)
        .await
        .unwrap();
    assert_eq!(listing.ended.as_deref(), Some(s(w.today).as_str()));
    assert!(listing.items(Cat::Position).is_empty());
    let task = listing.titled(Cat::Task, "Import OPC");
    // The manager of the seat the person left is still proposed: the structure of the last day is read.
    assert_eq!(task.suggestion.as_ref().unwrap().reason, "manager");

    // Handing everything over empties the list of pending people.
    let answer = apply(
        &admin,
        &w.leaver,
        Reason::Departure,
        None,
        None,
        None,
        "n",
        vec![
            choice(task, Some(&w.peer)),
            choice(listing.titled(Cat::Task, "Nakladka"), Some(&w.peer)),
            choice(&listing.items(Cat::TestItem)[0], Some(&w.peer)),
            choice(&listing.items(Cat::Membership)[0], None),
        ],
    )
    .await
    .unwrap();
    assert!(answer.ok, "{:?}", answer.items);
    assert!(pending().await.is_empty());
    let _ = (p, &w.unit, &w.boss_position, &w.leaver_position);
}

#[tokio::test]
async fn the_pending_list_is_the_administrators() {
    let w = world();
    let member = ctx(&w, &w.boss, false);
    expect_code(
        run(&member, P::HandoverPendingRequest {}).await,
        ProtocolErrorCode::PolicyDenied,
        "a member lists pending people",
    );
}

#[tokio::test]
async fn a_head_seat_is_proposed_to_the_deputy_head() {
    let w = world();
    let admin = ctx(&w, &w.admin, true);
    let listing = list(&admin, &w.boss, Reason::Departure, None, None)
        .await
        .unwrap();
    let seat = listing.titled(Cat::Position, "Kierownik");
    assert_eq!(seat.role, "head");
    let suggestion = seat.suggestion.as_ref().unwrap();
    assert_eq!(
        (suggestion.user_id.as_str(), suggestion.reason.as_str()),
        (w.deputy_head.as_str(), "deputy_head")
    );
}


#[tokio::test]
async fn temporary_task_handover_and_return_record_actual_actor_and_atomic_notes_once() {
    let w = world();
    let p = project(
        &w,
        "history",
        &w.boss,
        &[
            (&w.boss, "owner"),
            (&w.leaver, "developer,tester"),
            (&w.peer, "developer,tester"),
        ],
        &w.leaver,
    );
    let me = ctx(&w, &w.leaver, false);
    let back = plus(w.today, 3);
    let listing = list(&me, &w.leaver, Reason::Absence, None, None)
        .await
        .unwrap();
    let note = "Finish the pending review while I am away";
    let applied = apply(
        &me,
        &w.leaver,
        Reason::Absence,
        None,
        None,
        Some(s(back)),
        note,
        vec![choice(
            listing.titled(Cat::Task, "Import OPC"),
            Some(&w.peer),
        )],
    )
    .await
    .unwrap();
    assert!(applied.ok, "{:?}", applied.items);
    let pool = project_db::open(&p.id).unwrap();
    let before = tasks::list_task_events(&pool, &p.task_a, None, 100)
        .unwrap()
        .0;
    let over = before
        .iter()
        .find(|event| event.kind == "handed_over")
        .expect("forward event");
    assert_eq!(over.actor_id, w.leaver);
    let pin = tasks::latest_handover_comment_id(&pool, &p.task_a)
        .unwrap()
        .expect("forward note pin");
    assert!(over.after_json.contains(&pin));
    let report = handover::run_due(&w.state.db, DEFAULT_ORG_ID, back).unwrap();
    assert_eq!((report.returned, report.kept, report.failed), (1, 0, 0));
    assert_eq!(assignee_of(&p.id, &p.task_a), w.leaver);
    let events = tasks::list_task_events(&pool, &p.task_a, None, 100)
        .unwrap()
        .0;
    let returned = events
        .iter()
        .find(|event| event.kind == "handed_back")
        .expect("return event");
    assert_eq!(returned.actor_id, w.leaver);
    assert_eq!(returned.actor_kind, "user");
    let new_pin = tasks::latest_handover_comment_id(&pool, &p.task_a)
        .unwrap()
        .expect("return note pin");
    assert_ne!(pin, new_pin);
    assert!(returned.after_json.contains(&new_pin));
    let comments = tasks::list_comments(&pool, &p.task_a).unwrap();
    assert_eq!(comments.len(), 2);
    assert!(comments
        .iter()
        .all(|comment| comment.author_user_id == w.leaver && comment.body_md == note));
    assert_eq!(
        handover::run_due(&w.state.db, DEFAULT_ORG_ID, back).unwrap(),
        handover::DueReport::default()
    );
    assert_eq!(
        tasks::list_task_events(&pool, &p.task_a, None, 100)
            .unwrap()
            .0
            .len(),
        events.len()
    );
}

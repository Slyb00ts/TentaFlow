//! Deputies, absences, the escalation chain and who may see what (WP9), on
//! real databases. The chain tests follow the list in docs §2.1a.

use std::collections::HashSet;

use super::availability::{self, Absence, AbsenceKind, Availability, DeputyScope};
use super::escalation::{
    effective_manager, escalation_chain, Chain, Problem, SkipReason, Step, Via, MAX_LEVELS,
};
use super::privacy::{
    self,
    can_view_person_data, visibility_of, who_can_view, Area, PersonDataKind, Verdict, ViewRule,
};
use super::query::{ManagerSource, Snapshot};
use super::tests::{add_user, captures_of, day, Fixture, ORG};
use super::*;

/// Board { CEO(alice) head, deputy heads: Deputy CEO 1 (dora), Deputy CEO 2 (erin) }
/// Finance { CFO(bob) head -> CEO, Analyst(carol) -> CFO, Intern(ian) -> Analyst }
/// Ops { Ops head (zed) }, with a functional line Analyst -> Ops head.
struct Co {
    f: Fixture,
    finance: Unit,
    board: Unit,
    ceo: Position,
    cfo: Position,
    analyst: Position,
    intern: Position,
    ops_head: Position,
    alice: String,
    bob: String,
    carol: String,
    ian: String,
    dora: String,
    erin: String,
    zed: String,
    tom: String,
}

fn co() -> Co {
    let f = Fixture::new();
    let mut user = |name: &str| add_user(&f.pool, ORG, name);
    let (alice, bob, carol, ian) = (user("alice"), user("bob"), user("carol"), user("ian"));
    let (dora, erin, zed, tom) = (user("dora"), user("erin"), user("zed"), user("tom"));
    let board = f.unit("Board", None);
    let finance = f.unit("Finance", Some(&board.unit_id));
    let ops = f.unit("Ops", Some(&board.unit_id));
    let ceo = f.position(&board, "CEO", None);
    let deputy1 = f.position(&board, "Deputy CEO 1", Some(&ceo));
    let deputy2 = f.position(&board, "Deputy CEO 2", Some(&ceo));
    let cfo = f.position(&finance, "CFO", Some(&ceo));
    let analyst = f.position(&finance, "Analyst", Some(&cfo));
    let intern = f.position(&finance, "Intern", Some(&analyst));
    let ops_head = f.position(&ops, "Ops head", Some(&ceo));
    set_head(
        &f.pool,
        &f.ctx(),
        &board.unit_id,
        Some(&ceo.position_id),
        day(0),
    )
    .unwrap();
    set_head(
        &f.pool,
        &f.ctx(),
        &finance.unit_id,
        Some(&cfo.position_id),
        day(0),
    )
    .unwrap();
    set_deputy_heads(
        &f.pool,
        &f.ctx(),
        &board.unit_id,
        &[deputy1.position_id.clone(), deputy2.position_id.clone()],
        day(0),
    )
    .unwrap();
    for (position, who) in [
        (&ceo, &alice),
        (&cfo, &bob),
        (&analyst, &carol),
        (&intern, &ian),
        (&deputy1, &dora),
        (&deputy2, &erin),
        (&ops_head, &zed),
    ] {
        f.assign_user(position, who, 1.0, None).unwrap();
    }
    set_reporting_line(
        &f.pool,
        &f.ctx(),
        &NewLine {
            position_id: analyst.position_id.clone(),
            parent_position_id: ops_head.position_id.clone(),
            kind: LineKind::Functional,
            priority: 0,
            valid_from: day(0),
            valid_to: None,
        },
    )
    .unwrap();
    Co {
        f,
        finance,
        board,
        ceo,
        cfo,
        analyst,
        intern,
        ops_head,
        alice,
        bob,
        carol,
        ian,
        dora,
        erin,
        zed,
        tom,
    }
}

impl Co {
    fn absent(&self, user: &str, from: i64, to: Option<i64>) {
        add_absence(
            &self.f.pool,
            &self.f.confirmed(),
            Actor { is_admin: true },
            &NewAbsence {
                user_id: user.to_string(),
                valid_from: day(from),
                valid_to: to.map(day),
                kind: AbsenceKind::Leave,
            },
        )
        .unwrap();
    }

    fn deputy(&self, user: &str, deputy: &str, scope: DeputyScope) {
        set_deputy(
            &self.f.pool,
            &self.f.ctx(),
            &NewDeputy {
                user_id: user.to_string(),
                deputy_user_id: deputy.to_string(),
                scope,
                valid_from: day(0),
                valid_to: None,
            },
        )
        .unwrap();
    }

    fn deactivate(&self, user: &str) {
        self.f
            .pool
            .write()
            .unwrap()
            .execute(
                "UPDATE user_accounts SET is_active = 0 WHERE id = ?1",
                [user],
            )
            .unwrap();
    }

    fn chain(&self, user: &str, scope: &DeputyScope) -> Chain {
        let conn = self.f.pool.read().unwrap();
        let snap = Snapshot::load(&conn, ORG, day(0)).unwrap();
        let avail = Availability::load(&conn, ORG, day(0)).unwrap();
        escalation_chain(&snap, &avail, user, scope)
    }

    fn escalations(&self, user: &str) -> Chain {
        self.chain(user, &DeputyScope::Escalations)
    }
}

macro_rules! u {
    ($($x:expr),* $(,)?) => { vec![$($x.as_str()),*] };
}

fn who(chain: &Chain) -> Vec<&str> {
    chain.steps.iter().map(|s| s.user_id.as_str()).collect()
}

fn step<'a>(chain: &'a Chain, user: &str) -> &'a Step {
    chain
        .steps
        .iter()
        .find(|s| s.user_id == user)
        .unwrap_or_else(|| panic!("{user} is not in {:?}", chain.steps))
}

// ---------------------------------------------------------------------------
// Escalation chain (docs §2.1a)
// ---------------------------------------------------------------------------

#[test]
fn everybody_present_the_chain_is_the_managers_one_level_at_a_time() {
    let c = co();
    let chain = c.escalations(&c.ian);
    assert_eq!(who(&chain), u![c.carol, c.bob, c.alice]);
    assert!(chain.steps.iter().all(|s| s.via == Via::Holder));
    assert_eq!(
        chain.steps.iter().map(|s| s.level).collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert!(chain.skipped.is_empty() && chain.problem.is_none());
}

#[test]
fn an_absent_manager_without_cover_is_skipped_and_the_walk_goes_up() {
    let c = co();
    c.absent(&c.bob, 0, Some(3));
    let chain = c.escalations(&c.carol);
    assert_eq!(who(&chain), u![c.alice]);
    assert_eq!(step(&chain, &c.alice).level, 2);
    assert_eq!(chain.skipped.len(), 1);
    assert_eq!(chain.skipped[0].level, 1);
    assert_eq!(chain.skipped[0].position_id, c.cfo.position_id);
    assert_eq!(chain.skipped[0].reason, SkipReason::Unavailable);
}

#[test]
fn a_deputy_head_is_asked_only_while_the_head_is_away() {
    let c = co();
    assert_eq!(who(&c.escalations(&c.bob)), u![c.alice]);

    c.absent(&c.alice, 0, Some(2));
    let chain = c.escalations(&c.bob);
    assert_eq!(who(&chain), u![c.dora]);
    let dora = step(&chain, &c.dora);
    assert_eq!(dora.via, Via::DeputyHead);
    assert_eq!(dora.covering_user_id.as_deref(), Some(c.alice.as_str()));
    assert_eq!(dora.level, 1);
}

#[test]
fn deputy_heads_are_tried_in_order_and_an_absent_one_is_passed_over() {
    let c = co();
    c.absent(&c.alice, 0, Some(2));
    c.absent(&c.dora, 0, Some(2));
    assert_eq!(who(&c.escalations(&c.bob)), u![c.erin]);
}

#[test]
fn head_and_every_deputy_head_away_the_walk_goes_a_level_higher() {
    let c = co();
    c.absent(&c.bob, 0, Some(2));
    for user in [&c.alice, &c.dora, &c.erin] {
        c.absent(user, 0, Some(2));
    }
    // Carol -> CFO (away, no deputy heads) -> CEO (away, deputy heads away) -> top.
    let chain = c.escalations(&c.carol);
    assert!(chain.steps.is_empty());
    assert_eq!(
        chain.skipped.iter().map(|s| s.level).collect::<Vec<_>>(),
        [1, 2]
    );
    assert!(chain.problem.is_none());
}

#[test]
fn a_deputy_head_comes_before_a_temporary_deputy() {
    let c = co();
    c.absent(&c.alice, 0, Some(2));
    c.deputy(&c.alice, &c.tom, DeputyScope::All);
    let chain = c.escalations(&c.bob);
    assert_eq!(who(&chain), u![c.dora]);

    // With the deputy heads away too, the temporary deputy answers.
    c.absent(&c.dora, 0, Some(2));
    c.absent(&c.erin, 0, Some(2));
    let chain = c.escalations(&c.bob);
    let tom = step(&chain, &c.tom);
    assert_eq!(tom.via, Via::Deputy);
    assert_eq!(tom.covering_user_id.as_deref(), Some(c.alice.as_str()));
}

#[test]
fn a_temporary_deputy_answers_only_for_a_scope_that_covers_the_question() {
    let c = co();
    c.absent(&c.bob, 0, Some(2));
    c.deputy(&c.bob, &c.tom, DeputyScope::Approvals);
    assert_eq!(
        who(&c.chain(&c.carol, &DeputyScope::Escalations)),
        u![c.alice]
    );
    let approvals = c.chain(&c.carol, &DeputyScope::Approvals);
    assert_eq!(who(&approvals), u![c.tom, c.alice]);
    assert_eq!(step(&approvals, &c.tom).via, Via::Deputy);
    // A project scope covers only that project.
    c.deputy(&c.bob, &c.dora, DeputyScope::Project("p-1".into()));
    assert!(
        !who(&c.chain(&c.carol, &DeputyScope::Project("p-2".into()))).contains(&c.dora.as_str())
    );
    assert!(who(&c.chain(&c.carol, &DeputyScope::Project("p-1".into()))).contains(&c.dora.as_str()));
}

#[test]
fn an_absent_or_ended_temporary_deputy_does_not_answer() {
    let c = co();
    c.absent(&c.bob, 0, Some(2));
    c.deputy(&c.bob, &c.tom, DeputyScope::All);
    c.absent(&c.tom, 0, Some(2));
    assert_eq!(who(&c.escalations(&c.carol)), u![c.alice]);

    // Not started yet: the deputy is valid from tomorrow.
    let c = co();
    c.absent(&c.bob, 0, Some(2));
    set_deputy(
        &c.f.pool,
        &c.f.ctx(),
        &NewDeputy {
            user_id: c.bob.clone(),
            deputy_user_id: c.tom.clone(),
            scope: DeputyScope::All,
            valid_from: day(1),
            valid_to: None,
        },
    )
    .unwrap();
    assert_eq!(who(&c.escalations(&c.carol)), u![c.alice]);
}

#[test]
fn a_vacancy_on_the_path_is_skipped() {
    let c = co();
    let lead = c.f.position(&c.finance, "Lead", Some(&c.analyst));
    move_position(
        &c.f.pool,
        &c.f.ctx(),
        &c.intern.position_id,
        Some(&lead.position_id),
        day(0),
    )
    .unwrap();
    let chain = c.escalations(&c.ian);
    assert_eq!(who(&chain), u![c.carol, c.bob, c.alice]);
    assert_eq!(step(&chain, &c.carol).level, 2);
    assert_eq!(chain.skipped.len(), 1);
    assert_eq!(chain.skipped[0].reason, SkipReason::Vacant);
    assert_eq!(chain.skipped[0].position_id, lead.position_id);
}

#[test]
fn a_deactivated_account_is_a_vacancy_and_its_temporary_deputy_is_not_asked() {
    let c = co();
    c.deputy(&c.bob, &c.tom, DeputyScope::All);
    c.deactivate(&c.bob);
    let chain = c.escalations(&c.carol);
    assert_eq!(who(&chain), u![c.alice]);
    assert_eq!(chain.skipped[0].reason, SkipReason::Unavailable);
}

#[test]
fn a_person_already_in_the_chain_is_not_asked_twice() {
    let c = co();
    // Bob also holds Deputy CEO 1, so he would be the answer at two levels.
    let deputy1 =
        c.f.pool
            .read()
            .unwrap()
            .query_row(
                "SELECT position_id FROM org_positions WHERE name = 'Deputy CEO 1'",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap();
    assign(
        &c.f.pool,
        &c.f.ctx(),
        &NewAssignment {
            position_id: deputy1,
            subject: Subject::User(c.bob.clone()),
            kind: AssignmentType::Acting,
            share: 0.1,
            is_primary: Some(false),
            valid_from: day(0),
            valid_to: None,
        },
    )
    .unwrap();
    c.absent(&c.alice, 0, Some(2));
    c.absent(&c.dora, 0, Some(2));
    // Level 1 is Bob (CFO); at level 2 the head and Dora are away and the
    // next holder of Deputy CEO 1 is Bob again, who has already answered, so
    // Erin does.
    let chain = c.escalations(&c.carol);
    assert_eq!(who(&chain), u![c.bob, c.erin]);
    let chain = c.escalations(&c.ian);
    assert!(who(&chain).iter().filter(|u| **u == c.bob).count() == 1);
}

#[test]
fn the_asking_person_is_never_their_own_escalation() {
    let c = co();
    // Bob is the CFO and the only one above Carol; when Bob asks, Bob is not offered.
    assert!(!who(&c.escalations(&c.bob)).contains(&c.bob.as_str()));
    // A deputy head asking while the head is away is not sent to themselves.
    c.absent(&c.alice, 0, Some(2));
    let chain = c.escalations(&c.dora);
    assert!(!who(&chain).contains(&c.dora.as_str()));
}

#[test]
fn a_functional_line_never_enters_the_chain() {
    let c = co();
    let chain = c.escalations(&c.carol);
    assert_eq!(who(&chain), u![c.bob, c.alice]);
    assert!(!who(&chain).contains(&c.zed.as_str()));
}

#[test]
fn a_person_without_a_primary_position_has_no_chain() {
    let c = co();
    let stranger = add_user(&c.f.pool, ORG, "stranger");
    assert_eq!(c.escalations(&stranger), Chain::default());
}

#[test]
fn a_cycle_in_the_data_is_reported_and_the_walk_ends() {
    let c = co();
    // The write path refuses this; two nodes merging offline could produce it.
    c.f.pool
        .write()
        .unwrap()
        .execute(
            "INSERT INTO org_reporting_lines \
             (id, org_id, position_id, parent_position_id, kind, priority, valid_from) \
             VALUES ('bad', ?1, ?2, ?3, 'primary', 0, ?4)",
            [
                ORG,
                &c.ceo.position_id,
                &c.intern.position_id,
                &super::validate::format_date(day(-1)),
            ],
        )
        .unwrap();
    let chain = c.escalations(&c.ian);
    assert!(
        matches!(chain.problem, Some(Problem::Cycle { .. })),
        "{chain:?}"
    );
    assert!(chain.steps.len() <= MAX_LEVELS as usize);
}

#[test]
fn a_chain_deeper_than_the_limit_stops_at_it() {
    let f = Fixture::new();
    let unit = f.unit("Deep", None);
    let mut parent: Option<Position> = None;
    let mut positions = Vec::new();
    for n in 0..(MAX_LEVELS + 5) {
        let p = f.position(&unit, &format!("L{n}"), parent.as_ref());
        parent = Some(p.clone());
        positions.push(p);
    }
    // The bottom of the tree is the LAST created; give it a holder.
    let asker = add_user(&f.pool, ORG, "asker");
    f.assign_user(positions.last().unwrap(), &asker, 1.0, None)
        .unwrap();
    let conn = f.pool.read().unwrap();
    let snap = Snapshot::load(&conn, ORG, day(0)).unwrap();
    let avail = Availability::load(&conn, ORG, day(0)).unwrap();
    let chain = escalation_chain(&snap, &avail, &asker, &DeputyScope::Escalations);
    assert_eq!(chain.problem, Some(Problem::TooDeep));
    assert_eq!(chain.skipped.len(), MAX_LEVELS as usize);
    assert!(chain.steps.is_empty());
}

// ---------------------------------------------------------------------------
// Availability and the manager with deputies
// ---------------------------------------------------------------------------

#[test]
fn a_person_is_unavailable_exactly_on_the_days_of_an_absence() {
    let c = co();
    add_absence(
        &c.f.pool,
        &c.f.confirmed(),
        Actor { is_admin: true },
        &NewAbsence {
            user_id: c.carol.clone(),
            valid_from: day(2),
            valid_to: Some(day(5)),
            kind: AbsenceKind::Training,
        },
    )
    .unwrap();
    let at =
        |offset| availability::is_available(&c.f.pool, ORG, &c.carol, Some(day(offset))).unwrap();
    assert!(at(1) && !at(2) && !at(4) && at(5));
    assert!(!availability::is_available(&c.f.pool, ORG, "no-such-user", None).unwrap());
    c.deactivate(&c.ian);
    assert!(!availability::is_available(&c.f.pool, ORG, &c.ian, None).unwrap());
}

#[test]
fn the_manager_is_the_structural_one_unless_absent_and_then_the_cover() {
    let c = co();
    let manager = |user: &str| {
        let conn = c.f.pool.read().unwrap();
        let snap = Snapshot::load(&conn, ORG, day(0)).unwrap();
        let avail = Availability::load(&conn, ORG, day(0)).unwrap();
        effective_manager(&snap, &avail, user)
    };
    assert_eq!(manager(&c.carol).unwrap().user_id, c.bob);

    c.absent(&c.bob, 0, Some(3));
    // Nobody covers Bob: he is still the manager.
    assert_eq!(manager(&c.carol).unwrap().user_id, c.bob);

    // An approvals deputy does not stand in for the manager in general.
    c.deputy(&c.bob, &c.tom, DeputyScope::Approvals);
    assert_eq!(manager(&c.carol).unwrap().user_id, c.bob);

    c.deputy(&c.bob, &c.tom, DeputyScope::All);
    let m = manager(&c.carol).unwrap();
    assert_eq!(
        (m.user_id.as_str(), m.source),
        (c.tom.as_str(), ManagerSource::Deputy)
    );

    // The pool-level query agrees, and the projection's structural manager does not move.
    let via_pool = query::get_manager(&c.f.pool, ORG, &c.carol, None)
        .unwrap()
        .unwrap();
    assert_eq!(via_pool.user_id, c.tom);
    let conn = c.f.pool.read().unwrap();
    let snap = Snapshot::load(&conn, ORG, day(0)).unwrap();
    assert_eq!(snap.manager_of(&c.carol).unwrap().user_id, c.bob);
}

#[test]
fn a_vacant_head_is_answered_by_the_deputy_head_in_the_projection_but_leave_moves_nothing() {
    let c = co();
    let profile = |user: &str| -> Option<String> {
        c.f.pool
            .read()
            .unwrap()
            .query_row(
                "SELECT manager_user_id FROM sync_user_org_profiles WHERE org_id = ?1 AND user_id = ?2",
                [ORG, user],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(profile(&c.bob).as_deref(), Some(c.alice.as_str()));

    // Alice on leave: rights do not move, even after a recompute.
    c.absent(&c.alice, 0, Some(5));
    projection::recompute_all(&c.f.pool, ORG).unwrap();
    assert_eq!(profile(&c.bob).as_deref(), Some(c.alice.as_str()));

    // Alice leaves the position: the seat is vacant and the first deputy head takes over.
    let assignment_id: String =
        c.f.pool
            .read()
            .unwrap()
            .query_row(
                "SELECT id FROM org_assignments WHERE user_id = ?1",
                [&c.alice],
                |r| r.get(0),
            )
            .unwrap();
    end_assignment(&c.f.pool, &c.f.confirmed(), &assignment_id, day(0)).unwrap();
    assert_eq!(profile(&c.bob).as_deref(), Some(c.dora.as_str()));
    let manager = query::get_manager(&c.f.pool, ORG, &c.bob, None)
        .unwrap()
        .unwrap();
    assert_eq!(manager.source, ManagerSource::DeputyHead);

    // A deputy head does not get the other deputy head as manager.
    assert_ne!(profile(&c.dora).as_deref(), Some(c.erin.as_str()));
}

// ---------------------------------------------------------------------------
// Privacy
// ---------------------------------------------------------------------------

fn decide(c: &Co, viewer: &str, subject: &str, kind: PersonDataKind, admin: bool) -> bool {
    let conn = c.f.pool.read().unwrap();
    let snap = Snapshot::load(&conn, ORG, day(0)).unwrap();
    can_view_person_data(&snap, viewer, subject, kind, admin).allowed
}

#[test]
fn the_privacy_matrix_of_the_plan_for_a_person_in_the_middle_of_the_line() {
    let c = co();
    c.deputy(&c.bob, &c.tom, DeputyScope::All);
    let subject = &c.carol;
    // (viewer, is_admin, [dates, utilization, history])
    let cases: [(&str, bool, [bool; 3]); 8] = [
        (&c.carol, false, [true, true, true]), // the person
        (&c.bob, false, [true, true, false]),  // manager on the primary line
        (&c.alice, false, [true, true, false]), // higher up the line
        (&c.zed, false, [false, false, false]), // functional line only
        (&c.tom, false, [false, false, false]), // deputy of the manager
        (&c.ian, false, [false, false, false]), // a subordinate
        (&c.dora, false, [false, false, false]), // a stranger
        (&c.dora, true, [true, false, true]),  // an administrator
    ];
    let kinds = [
        PersonDataKind::AbsenceDates,
        PersonDataKind::TimeUtilization,
        PersonDataKind::PositionHistory,
    ];
    for (viewer, admin, expected) in cases {
        for (kind, want) in kinds.iter().zip(expected) {
            assert_eq!(
                decide(&c, viewer, subject, *kind, admin),
                want,
                "viewer {viewer} admin={admin} kind {kind:?}"
            );
        }
    }
}

#[test]
fn the_inverse_view_lists_exactly_the_viewers_the_check_admits() {
    let c = co();
    let everybody = [
        &c.alice, &c.bob, &c.carol, &c.ian, &c.dora, &c.erin, &c.zed, &c.tom,
    ];
    let admins = vec![c.dora.clone()];
    let conn = c.f.pool.read().unwrap();
    let snap = Snapshot::load(&conn, ORG, day(0)).unwrap();
    for kind in PersonDataKind::ALL {
        let listed: HashSet<String> = who_can_view(&snap, &c.carol, kind, &admins)
            .into_iter()
            .map(|v| v.user_id)
            .collect();
        for user in everybody {
            let admin = admins.contains(user);
            let allowed = can_view_person_data(&snap, user, &c.carol, kind, admin).allowed;
            assert_eq!(listed.contains(user), allowed, "{kind:?} viewer {user}");
        }
    }
    // The rule shown next to each viewer is the one the check names.
    let dates = who_can_view(&snap, &c.carol, PersonDataKind::AbsenceDates, &admins);
    let rule_of = |user: &str| dates.iter().find(|v| v.user_id == user).map(|v| v.rule);
    assert_eq!(rule_of(&c.carol), Some(ViewRule::Owner));
    assert_eq!(rule_of(&c.bob), Some(ViewRule::PrimaryManager));
    assert_eq!(rule_of(&c.alice), Some(ViewRule::Supervisor));
    assert_eq!(rule_of(&c.dora), Some(ViewRule::Administrator));
    assert_eq!(rule_of(&c.ian), None);
}

#[test]
fn the_visibility_of_a_manager_names_the_subtree_and_the_direct_reports() {
    let c = co();
    let conn = c.f.pool.read().unwrap();
    let snap = Snapshot::load(&conn, ORG, day(0)).unwrap();
    let v = visibility_of(&snap, &c.bob, false);
    let mut want = vec![c.carol.clone(), c.ian.clone()];
    want.sort();
    assert_eq!(v.subtree, want);
    assert_eq!(v.direct, vec![c.carol.clone()]);
    let row = |area: Area| v.rows.iter().find(|r| r.area == area).unwrap();
    assert_eq!(row(Area::Structure).verdict, Verdict::All);
    assert_eq!(row(Area::Utilization).verdict, Verdict::Subtree);
    assert_eq!(row(Area::AbsenceDates).verdict, Verdict::Subtree);
    assert_eq!(row(Area::PositionHistory).verdict, Verdict::Own);
    assert_eq!(row(Area::EveryoneElse).verdict, Verdict::None);

    let leaf = visibility_of(&snap, &c.ian, false);
    assert!(leaf.subtree.is_empty());
    let dates_of = |v: &super::privacy::Visibility| {
        v.rows
            .iter()
            .find(|r| r.area == Area::AbsenceDates)
            .unwrap()
            .verdict
    };
    assert_eq!(dates_of(&leaf), Verdict::Own);
    assert_eq!(dates_of(&visibility_of(&snap, &c.dora, true)), Verdict::All);
}

// ---------------------------------------------------------------------------
// Writes and who may do them
// ---------------------------------------------------------------------------

fn new_absence(user: &str, from: i64, to: Option<i64>) -> NewAbsence {
    NewAbsence {
        user_id: user.to_string(),
        valid_from: day(from),
        valid_to: to.map(day),
        kind: AbsenceKind::Other,
    }
}

const PERSON: Actor = Actor { is_admin: false };
const ADMIN: Actor = Actor { is_admin: true };

fn ctx_of<'a>(user: &'a str, confirmed: bool) -> WriteCtx<'a> {
    WriteCtx {
        org_id: ORG,
        actor_user_id: user,
        confirm_backdated: confirmed,
    }
}

#[test]
fn a_person_writes_their_own_absences_and_nobody_elses() {
    let c = co();
    let own = add_absence(
        &c.f.pool,
        &ctx_of(&c.carol, false),
        PERSON,
        &new_absence(&c.carol, 1, Some(3)),
    )
    .unwrap()
    .value;
    assert_eq!(own.source, "manual");
    assert_eq!(own.created_by.as_deref(), Some(c.carol.as_str()));

    let refused = add_absence(
        &c.f.pool,
        &ctx_of(&c.carol, false),
        PERSON,
        &new_absence(&c.ian, 1, Some(3)),
    )
    .unwrap_err();
    assert_eq!(refused.code(), "not_permitted");

    // Change and delete: own yes, another's no.
    let patch = AbsencePatch {
        kind: Some(AbsenceKind::Leave),
        ..Default::default()
    };
    let changed = update_absence(&c.f.pool, &ctx_of(&c.carol, false), PERSON, &own.id, &patch)
        .unwrap()
        .value;
    assert_eq!(changed.kind, AbsenceKind::Leave);

    let theirs = add_absence(
        &c.f.pool,
        &c.f.ctx(),
        ADMIN,
        &new_absence(&c.ian, 1, Some(3)),
    )
    .unwrap()
    .value;
    for result in [
        update_absence(
            &c.f.pool,
            &ctx_of(&c.carol, false),
            PERSON,
            &theirs.id,
            &patch,
        )
        .map(|_| ()),
        delete_absence(&c.f.pool, &ctx_of(&c.carol, false), PERSON, &theirs.id).map(|_| ()),
    ] {
        assert_eq!(result.unwrap_err().code(), "not_permitted");
    }
    delete_absence(&c.f.pool, &ctx_of(&c.carol, false), PERSON, &own.id).unwrap();
    assert!(availability::get_absence(&c.f.pool, ORG, &own.id).is_err());
    // The administrator may change anyone's.
    update_absence(&c.f.pool, &c.f.ctx(), ADMIN, &theirs.id, &patch).unwrap();
}

#[test]
fn a_person_cannot_touch_an_absence_that_came_from_another_source() {
    let c = co();
    c.f.pool
        .write()
        .unwrap()
        .execute(
            "INSERT INTO org_absences (id, org_id, user_id, valid_from, valid_to, kind, source) \
             VALUES ('imp', ?1, ?2, ?3, ?4, 'leave', 'edokumenty')",
            [
                ORG,
                &c.carol,
                &super::validate::format_date(day(1)),
                &super::validate::format_date(day(4)),
            ],
        )
        .unwrap();
    let patch = AbsencePatch {
        kind: Some(AbsenceKind::Other),
        ..Default::default()
    };
    assert_eq!(
        update_absence(&c.f.pool, &ctx_of(&c.carol, false), PERSON, "imp", &patch)
            .unwrap_err()
            .code(),
        "not_permitted"
    );
    assert_eq!(
        delete_absence(&c.f.pool, &ctx_of(&c.carol, false), PERSON, "imp")
            .unwrap_err()
            .code(),
        "not_permitted"
    );
    update_absence(&c.f.pool, &c.f.ctx(), ADMIN, "imp", &patch).unwrap();
}

#[test]
fn absence_input_is_validated_before_anything_is_written() {
    let c = co();
    let ctx = c.f.ctx();
    let bad_interval = new_absence(&c.carol, 3, Some(3));
    assert_eq!(
        add_absence(&c.f.pool, &ctx, ADMIN, &bad_interval)
            .unwrap_err()
            .code(),
        "invalid_interval"
    );
    // Somebody who is not a member of the organization.
    let outsider = crate::db::repository::create_user_account(
        &c.f.pool, "outsider", "h", "Outsider", "o@x.test",
    )
    .unwrap();
    assert_eq!(
        add_absence(&c.f.pool, &ctx, ADMIN, &new_absence(&outsider, 1, Some(2)))
            .unwrap_err()
            .code(),
        "not_found"
    );
    assert!(
        availability::absences_of(&c.f.pool, ORG, &c.ian, day(0), true)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn an_absence_dated_before_today_needs_the_backdating_confirmation() {
    let c = co();
    let err = add_absence(
        &c.f.pool,
        &c.f.ctx(),
        ADMIN,
        &new_absence(&c.carol, -2, Some(1)),
    )
    .unwrap_err();
    assert_eq!(err.code(), "backdated_confirmation_required");
    let ok = add_absence(
        &c.f.pool,
        &c.f.confirmed(),
        ADMIN,
        &new_absence(&c.carol, -2, Some(1)),
    )
    .unwrap()
    .value;

    let patch = AbsencePatch {
        valid_to: Some(Some(day(-1))),
        ..Default::default()
    };
    // Shortening a future end to today stays inside the present.
    let future = add_absence(
        &c.f.pool,
        &c.f.ctx(),
        ADMIN,
        &new_absence(&c.carol, 1, Some(9)),
    )
    .unwrap()
    .value;
    update_absence(
        &c.f.pool,
        &c.f.ctx(),
        ADMIN,
        &future.id,
        &AbsencePatch {
            valid_to: Some(Some(day(3))),
            ..Default::default()
        },
    )
    .unwrap();
    // Moving the end of one that already ran backwards in time does rewrite it.
    let err = update_absence(&c.f.pool, &c.f.ctx(), ADMIN, &ok.id, &patch).unwrap_err();
    assert_eq!(err.code(), "backdated_confirmation_required");
    // Deleting one that has begun does too.
    assert_eq!(
        delete_absence(&c.f.pool, &c.f.ctx(), ADMIN, &ok.id)
            .unwrap_err()
            .code(),
        "backdated_confirmation_required"
    );
    delete_absence(&c.f.pool, &c.f.ctx(), ADMIN, &future.id).unwrap();
}

#[test]
fn deputies_are_validated_and_do_not_overlap() {
    let c = co();
    plain_carol_admin_actor(&c);
    let base = NewDeputy {
        user_id: c.bob.clone(),
        deputy_user_id: c.tom.clone(),
        scope: DeputyScope::All,
        valid_from: day(0),
        valid_to: Some(day(10)),
    };
    let ctx = c.f.ctx();
    let made = set_deputy(&c.f.pool, &ctx, &base).unwrap().value;
    assert_eq!(made.created_by.as_deref(), Some(c.f.actor.as_str()));
    // The same cover twice in overlapping days.
    assert_eq!(
        set_deputy(
            &c.f.pool,
            &ctx,
            &NewDeputy {
                valid_from: day(5),
                valid_to: None,
                ..base.clone()
            }
        )
        .unwrap_err()
        .code(),
        "duplicate"
    );
    // Another scope or another day is fine.
    set_deputy(
        &c.f.pool,
        &ctx,
        &NewDeputy {
            scope: DeputyScope::Approvals,
            ..base.clone()
        },
    )
    .unwrap();
    set_deputy(
        &c.f.pool,
        &ctx,
        &NewDeputy {
            valid_from: day(10),
            valid_to: None,
            ..base.clone()
        },
    )
    .unwrap();
    // Self, empty interval, past start, a stranger.
    assert_eq!(
        set_deputy(
            &c.f.pool,
            &ctx,
            &NewDeputy {
                deputy_user_id: c.bob.clone(),
                ..base.clone()
            }
        )
        .unwrap_err()
        .code(),
        "invalid_value"
    );
    assert_eq!(
        set_deputy(
            &c.f.pool,
            &ctx,
            &NewDeputy {
                valid_to: Some(day(0)),
                scope: DeputyScope::Escalations,
                ..base.clone()
            }
        )
        .unwrap_err()
        .code(),
        "invalid_interval"
    );
    assert_eq!(
        set_deputy(
            &c.f.pool,
            &ctx,
            &NewDeputy {
                valid_from: day(-1),
                scope: DeputyScope::Escalations,
                ..base.clone()
            }
        )
        .unwrap_err()
        .code(),
        "backdated_confirmation_required"
    );
    let outsider = crate::db::repository::create_user_account(
        &c.f.pool, "outsider", "h", "Outsider", "o@x.test",
    )
    .unwrap();
    assert_eq!(
        set_deputy(
            &c.f.pool,
            &ctx,
            &NewDeputy {
                deputy_user_id: outsider,
                scope: DeputyScope::Escalations,
                ..base
            }
        )
        .unwrap_err()
        .code(),
        "not_found"
    );
}

#[test]
fn a_deputy_is_changed_and_ended_from_a_day() {
    let c = co();
    let ctx = c.f.ctx();
    let made = set_deputy(
        &c.f.pool,
        &ctx,
        &NewDeputy {
            user_id: c.bob.clone(),
            deputy_user_id: c.tom.clone(),
            scope: DeputyScope::All,
            valid_from: day(0),
            valid_to: None,
        },
    )
    .unwrap()
    .value;
    let updated = update_deputy(
        &c.f.pool,
        &ctx,
        &made.id,
        &DeputyPatch {
            scope: Some(DeputyScope::Approvals),
            valid_to: Some(Some(day(20))),
            ..Default::default()
        },
    )
    .unwrap()
    .value;
    assert_eq!(
        (updated.scope, updated.valid_to),
        (DeputyScope::Approvals, Some(day(20)))
    );
    assert_eq!(
        update_deputy(&c.f.pool, &ctx, &made.id, &DeputyPatch::default())
            .unwrap_err()
            .code(),
        "invalid_value"
    );

    end_deputy(&c.f.pool, &ctx, &made.id, day(5)).unwrap();
    assert_eq!(
        availability::get_deputy(&c.f.pool, ORG, &made.id)
            .unwrap()
            .valid_to,
        Some(day(5))
    );
    assert_eq!(
        end_deputy(&c.f.pool, &ctx, &made.id, day(6))
            .unwrap_err()
            .code(),
        "not_valid_at"
    );

    // Ending on its first day removes it: it never covered anybody.
    let fresh = set_deputy(
        &c.f.pool,
        &ctx,
        &NewDeputy {
            user_id: c.bob.clone(),
            deputy_user_id: c.dora.clone(),
            scope: DeputyScope::All,
            valid_from: day(2),
            valid_to: None,
        },
    )
    .unwrap()
    .value;
    end_deputy(&c.f.pool, &ctx, &fresh.id, day(2)).unwrap();
    assert!(availability::get_deputy(&c.f.pool, ORG, &fresh.id).is_err());
    assert_eq!(
        update_deputy(
            &c.f.pool,
            &ctx,
            "missing",
            &DeputyPatch {
                scope: Some(DeputyScope::All),
                ..Default::default()
            }
        )
        .unwrap_err()
        .code(),
        "not_found"
    );
}

// ---------------------------------------------------------------------------
// Replication, audit, projection
// ---------------------------------------------------------------------------

#[test]
fn deputy_and_absence_writes_replicate_and_leave_the_projection_alone() {
    let c = co();
    let profiles_before = captures_of(&c.f.pool, "core.sync_user_org_profile").len();
    c.deputy(&c.bob, &c.tom, DeputyScope::All);
    let made = add_absence(
        &c.f.pool,
        &c.f.ctx(),
        ADMIN,
        &new_absence(&c.carol, 1, Some(3)),
    )
    .unwrap()
    .value;
    assert_eq!(captures_of(&c.f.pool, "core.org_deputy").len(), 1);
    assert_eq!(captures_of(&c.f.pool, "core.org_absence").len(), 1);
    assert_eq!(
        captures_of(&c.f.pool, "core.sync_user_org_profile").len(),
        profiles_before
    );

    delete_absence(&c.f.pool, &c.f.ctx(), ADMIN, &made.id).unwrap();
    let captures = captures_of(&c.f.pool, "core.org_absence");
    assert_eq!(captures.len(), 2, "the delete is a tombstone");
}

#[test]
fn an_absence_has_no_reason_in_its_row_its_capture_or_its_audit_entry() {
    let c = co();
    add_absence(
        &c.f.pool,
        &c.f.ctx(),
        ADMIN,
        &new_absence(&c.carol, 1, Some(3)),
    )
    .unwrap();
    let conn = c.f.pool.read().unwrap();
    let has_reason_column: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('org_absences') WHERE name = 'reason')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!has_reason_column);
    let captured = format!("{:?}", captures_of(&c.f.pool, "core.org_absence"));
    assert!(!captured.contains("reason"), "{captured}");
    let recorded: String = conn
        .query_row(
            "SELECT details FROM audit_log WHERE action = 'org.absence.add'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!recorded.contains("reason"), "{recorded}");
}

#[test]
fn deleting_the_organization_takes_deputies_and_absences_with_it() {
    let c = co();
    c.deputy(&c.bob, &c.tom, DeputyScope::All);
    add_absence(
        &c.f.pool,
        &c.f.ctx(),
        ADMIN,
        &new_absence(&c.carol, 1, Some(3)),
    )
    .unwrap();
    let mut conn = c.f.pool.write().unwrap();
    let tx = conn.transaction().unwrap();
    purge_org_structure_tx(&tx, ORG).unwrap();
    tx.commit().unwrap();
    for table in ["org_deputies", "org_absences"] {
        let left: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0, "{table}");
    }
}

#[test]
fn availability_reads_of_the_pool_agree_with_the_rows() {
    let c = co();
    c.absent(&c.carol, 0, Some(2));
    c.deputy(&c.bob, &c.tom, DeputyScope::All);
    let (day_read, absent, deputies) = availability::availability_on(&c.f.pool, ORG, None).unwrap();
    assert_eq!(day_read, day(0));
    assert_eq!(absent, vec![c.carol.clone()]);
    assert_eq!(deputies.len(), 1);
    let (covered_by, covering) =
        availability::deputies_around(&c.f.pool, ORG, &c.bob, day(0)).unwrap();
    assert_eq!((covered_by.len(), covering.len()), (1, 0));
    let (_, covering) = availability::deputies_around(&c.f.pool, ORG, &c.tom, day(0)).unwrap();
    assert_eq!(covering.len(), 1);
    let listed: Vec<Absence> =
        availability::absences_of(&c.f.pool, ORG, &c.carol, day(0), false).unwrap();
    assert_eq!(listed.len(), 1);
    assert!(
        availability::absences_of(&c.f.pool, ORG, &c.carol, day(5), false)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        availability::absences_of(&c.f.pool, ORG, &c.carol, day(5), true)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn only_the_covered_person_or_an_administrator_may_write_a_deputy() {
    assert!(ensure_may_write_deputy("u-1", false, "u-1").is_ok());
    assert!(ensure_may_write_deputy("admin", true, "u-1").is_ok());
    assert_eq!(
        ensure_may_write_deputy("manager", false, "u-1")
            .unwrap_err()
            .code(),
        "not_permitted"
    );
}

// ---------------------------------------------------------------------------
// Who may be a deputy, and what presence on another day gives away
// ---------------------------------------------------------------------------

#[test]
fn a_deputy_must_be_an_active_member() {
    let c = co();
    c.deactivate(&c.tom);
    let refused = set_deputy(
        &c.f.pool,
        &c.f.ctx(),
        &NewDeputy {
            user_id: c.bob.clone(),
            deputy_user_id: c.tom.clone(),
            scope: DeputyScope::All,
            valid_from: day(0),
            valid_to: None,
        },
    )
    .unwrap_err();
    assert_eq!(refused.code(), "invalid_value");
    assert!(refused.to_string().contains("deputy_user_id"), "{refused}");
    c.deputy(&c.bob, &c.dora, DeputyScope::All);
}

fn seen_absent(c: &Co, viewer: &str, admin: bool, offset: i64) -> Vec<String> {
    privacy::visible_availability(&c.f.pool, ORG, viewer, admin, Some(day(offset)))
        .unwrap()
        .absent
}

#[test]
fn presence_on_another_day_is_shown_only_for_people_whose_dates_the_viewer_may_see() {
    let c = co();
    // Bob (CFO) is away on days 5-8; Erin is away today and tomorrow.
    c.absent(&c.bob, 5, Some(8));
    c.absent(&c.erin, 0, Some(2));

    // Alice is Bob's manager and an administrator may see everything.
    assert!(seen_absent(&c, &c.alice, false, 6).contains(&c.bob));
    assert!(seen_absent(&c, &c.tom, true, 6).contains(&c.bob));
    // Carol works under Bob: she may not learn when he is away, only that he is not away today.
    let carol = seen_absent(&c, &c.carol, false, 6);
    assert!(!carol.contains(&c.bob), "{carol:?}");
    // Today's absence of anybody is no secret, and it is not turned into another day's.
    assert!(seen_absent(&c, &c.carol, false, 0).contains(&c.erin));
    assert!(seen_absent(&c, &c.carol, false, 6).contains(&c.erin));
    assert!(!seen_absent(&c, &c.alice, false, 6).contains(&c.erin));

    let ask = |viewer: &str, user: &str, offset| {
        privacy::is_available_for(&c.f.pool, ORG, viewer, false, user, Some(day(offset))).unwrap()
    };
    assert!(!ask(&c.alice, &c.bob, 6));
    assert!(ask(&c.carol, &c.bob, 6));
}

#[test]
fn the_chain_asked_about_another_day_does_not_reveal_an_absence_the_asker_may_not_see() {
    let c = co();
    c.absent(&c.bob, 5, Some(8));
    let chain_for = |viewer: &str| {
        let (snap, avail, _) =
            privacy::limited_availability(&c.f.pool, ORG, viewer, false, Some(day(6))).unwrap();
        escalation_chain(&snap, &avail, &c.carol, &DeputyScope::Escalations)
    };
    // Alice sees Bob's dates: his level is skipped as unavailable.
    let alice = chain_for(&c.alice);
    assert!(alice.skipped.iter().any(|s| s.reason == SkipReason::Unavailable));
    assert!(!who(&alice).contains(&c.bob.as_str()));
    // Ian, under Carol, sees Bob as at his desk.
    let ian = chain_for(&c.ian);
    assert_eq!(who(&ian)[0], c.bob.as_str());
    assert!(ian.skipped.is_empty());
}

#[test]
fn a_cover_shows_no_dates_of_a_person_whose_absence_dates_are_private() {
    let c = co();
    c.deputy(&c.bob, &c.dora, DeputyScope::Approvals);
    set_deputy(
        &c.f.pool,
        &c.f.ctx(),
        &NewDeputy {
            user_id: c.bob.clone(),
            deputy_user_id: c.erin.clone(),
            scope: DeputyScope::Escalations,
            valid_from: day(5),
            valid_to: Some(day(8)),
        },
    )
    .unwrap();

    let cover = |viewer: &str, offset: i64| {
        privacy::person_cover(&c.f.pool, ORG, viewer, false, &c.bob, Some(day(offset)), false)
            .unwrap()
    };
    // The manager sees both covers, with their dates.
    let boss = cover(&c.alice, 0);
    assert_eq!(boss.covered_by.len(), 2);
    assert!(boss.covered_by.iter().any(|d| d.valid_to == Some(day(8))));

    // Carol sees the cover in force today, without its dates, and not the one that starts later.
    let carol = cover(&c.carol, 6);
    assert!(!carol.can_see_absences);
    assert_eq!(carol.today, day(0), "she is told about today only");
    assert_eq!(carol.covered_by.len(), 1);
    assert_eq!(carol.covered_by[0].deputy_user_id, c.dora);
    assert_eq!(carol.covered_by[0].valid_to, None);
    assert_eq!(carol.covered_by[0].valid_from, day(0));
    assert!(carol.absences.is_empty());

    // The deputy herself knows the dates she covers.
    let own = privacy::person_cover(&c.f.pool, ORG, &c.erin, false, &c.bob, Some(day(0)), false)
        .unwrap();
    assert!(own.covered_by.iter().any(|d| d.valid_to == Some(day(8))));

    // Availability lists carry the same limits.
    let seen = privacy::visible_availability(&c.f.pool, ORG, &c.carol, false, Some(day(6))).unwrap();
    assert!(seen.deputies.iter().all(|d| d.valid_to.is_none()));
    assert!(!seen.deputies.iter().any(|d| d.deputy_user_id == c.erin));
}

// ---------------------------------------------------------------------------
// Backdating is the administrator's
// ---------------------------------------------------------------------------

/// `carol` loses every administrator right and the fixture's actor gains them.
fn plain_carol_admin_actor(c: &Co) {
    let conn = c.f.pool.write().unwrap();
    let plain: String = conn
        .query_row(
            "SELECT role_id FROM roles WHERE permissions_json NOT LIKE '%org.admin%' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let admin: String = conn
        .query_row("SELECT role_id FROM roles WHERE name = 'org_admin'", [], |r| {
            r.get(0)
        })
        .unwrap();
    conn.execute(
        "UPDATE org_memberships SET role_id = ?1 WHERE user_id = ?2",
        [&plain, &c.carol],
    )
    .unwrap();
    conn.execute(
        "UPDATE org_memberships SET role_id = ?1 WHERE user_id = ?2",
        [&admin, &c.f.actor],
    )
    .unwrap();
}

#[test]
fn only_an_administrator_dates_an_absence_before_today_and_confirming_does_not_help_anyone_else() {
    let c = co();
    plain_carol_admin_actor(&c);
    let past = new_absence(&c.carol, -2, Some(3));
    for confirmed in [false, true] {
        let refused = add_absence(&c.f.pool, &ctx_of(&c.carol, confirmed), PERSON, &past)
            .unwrap_err();
        assert_eq!(refused.code(), "backdating_admin_only", "confirmed={confirmed}");
    }
    // From today on a person enters their own.
    let own = add_absence(
        &c.f.pool,
        &ctx_of(&c.carol, false),
        PERSON,
        &new_absence(&c.carol, 0, Some(3)),
    )
    .unwrap()
    .value;
    // Moving the start into the past, or deleting one that has begun, is refused as well.
    for confirmed in [false, true] {
        let moved = update_absence(
            &c.f.pool,
            &ctx_of(&c.carol, confirmed),
            PERSON,
            &own.id,
            &AbsencePatch {
                valid_from: Some(day(-1)),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(moved.code(), "backdating_admin_only");
    }
    let started = add_absence(
        &c.f.pool,
        &c.f.confirmed(),
        ADMIN,
        &new_absence(&c.carol, -1, Some(3)),
    )
    .unwrap()
    .value;
    let deleted = delete_absence(&c.f.pool, &ctx_of(&c.carol, true), PERSON, &started.id)
        .unwrap_err();
    assert_eq!(deleted.code(), "backdating_admin_only");
    // Ending it today is how a person comes back early.
    update_absence(
        &c.f.pool,
        &ctx_of(&c.carol, false),
        PERSON,
        &started.id,
        &AbsencePatch {
            valid_to: Some(Some(day(1))),
            ..Default::default()
        },
    )
    .unwrap();
    // An administrator still asks for the confirmation, and gets the past with it.
    assert_eq!(
        add_absence(&c.f.pool, &c.f.ctx(), ADMIN, &past)
            .unwrap_err()
            .code(),
        "backdated_confirmation_required"
    );
    add_absence(&c.f.pool, &c.f.confirmed(), ADMIN, &past).unwrap();
}

#[test]
fn only_an_administrator_dates_a_deputy_before_today() {
    let c = co();
    plain_carol_admin_actor(&c);
    let deputy = |from: i64| NewDeputy {
        user_id: c.carol.clone(),
        deputy_user_id: c.ian.clone(),
        scope: DeputyScope::All,
        valid_from: day(from),
        valid_to: None,
    };
    for confirmed in [false, true] {
        let refused = set_deputy(&c.f.pool, &ctx_of(&c.carol, confirmed), &deputy(-2)).unwrap_err();
        assert_eq!(refused.code(), "backdating_admin_only", "confirmed={confirmed}");
    }
    let made = set_deputy(&c.f.pool, &ctx_of(&c.carol, false), &deputy(0))
        .unwrap()
        .value;
    let moved = update_deputy(
        &c.f.pool,
        &ctx_of(&c.carol, true),
        &made.id,
        &DeputyPatch {
            valid_from: Some(day(-1)),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(moved.code(), "backdating_admin_only");
    let ended = end_deputy(&c.f.pool, &ctx_of(&c.carol, true), &made.id, day(-1)).unwrap_err();
    assert_eq!(ended.code(), "backdating_admin_only");
    // The administrator confirms and gets the past.
    let past = NewDeputy {
        scope: DeputyScope::Approvals,
        ..deputy(-2)
    };
    assert_eq!(
        set_deputy(&c.f.pool, &c.f.ctx(), &past)
            .unwrap_err()
            .code(),
        "backdated_confirmation_required"
    );
    set_deputy(&c.f.pool, &c.f.confirmed(), &past).unwrap();
}

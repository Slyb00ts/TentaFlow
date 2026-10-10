//! Queries, projection and the nightly decision, on real databases.

use std::collections::HashMap;
use std::time::Instant;

use chrono::{NaiveDate, TimeZone, Utc};

use super::query::{Direction, SeatScope, Target};
use super::tests::{add_user, capture_count, captures_of, day, Fixture, ORG};
use super::*;
use crate::db::repository as repo_db;
use crate::db::DbPool;

/// The manager as the dispatch layer answers it: the snapshot and the presence of one day.
fn effective_manager_on(pool: &DbPool, user: &str, offset: i64) -> Option<query::Manager> {
    let conn = pool.read().unwrap();
    let snap = query::Snapshot::load(&conn, ORG, day(offset)).unwrap();
    let avail = availability::Availability::load(&conn, ORG, day(offset)).unwrap();
    escalation::effective_manager(&snap, &avail, user)
}

/// A member who is not an organization admin, so the permission checks have to
/// decide from the structure alone.
fn person(f: &Fixture, name: &str) -> String {
    let id = add_user(&f.pool, ORG, name);
    f.pool
        .write()
        .unwrap()
        .execute(
            "UPDATE org_memberships SET role_id = \
             (SELECT role_id FROM roles WHERE role_id <> 'role-org-admin' LIMIT 1) \
             WHERE org_id = ?1 AND user_id = ?2",
            [ORG, &id],
        )
        .unwrap();
    id
}

fn profile_captures(pool: &DbPool) -> usize {
    captures_of(pool, "core.sync_user_org_profile").len()
}

fn profile_of(pool: &DbPool, user: &str) -> Option<(String, Option<String>, bool)> {
    pool.read()
        .unwrap()
        .query_row(
            "SELECT department_id, manager_user_id, is_department_manager \
             FROM sync_user_org_profiles WHERE org_id = ?1 AND user_id = ?2",
            [ORG, user],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok()
}

fn allowed(f: &Fixture, user: &str, scope: &str, department: Option<&str>, subject: &str) -> bool {
    let resource = format!("{scope}-{}-{subject}", department.unwrap_or("none"));
    repo_db::upsert_sync_resource_acl(
        &f.pool,
        ORG,
        "test-addon",
        "doc",
        &resource,
        None,
        Some(subject),
        department,
        None,
        scope,
    )
    .unwrap();
    repo_db::can_user_access_sync_resource(
        &f.pool,
        user,
        ORG,
        "test-addon",
        "doc",
        &resource,
        "read",
    )
    .unwrap()
    .allowed
}

fn line(position: &Position, parent: &Position, kind: LineKind) -> NewLine {
    NewLine {
        position_id: position.position_id.clone(),
        parent_position_id: parent.position_id.clone(),
        kind,
        priority: 0,
        valid_from: day(0),
        valid_to: None,
    }
}

/// Board { CEO(alice) <- Assistant(staff, dave), CFO(bob) }, Finance { Analyst(carol) -> CFO,
/// Vacant analyst -> CFO }, plus a functional line Analyst -> CEO.
struct Company {
    board: Unit,
    finance: Unit,
    ceo: Position,
    assistant: Position,
    cfo: Position,
    analyst: Position,
    vacant: Position,
    alice: String,
    bob: String,
    carol: String,
    dave: String,
}

fn company(f: &Fixture) -> Company {
    let alice = person(f, "alice");
    let bob = person(f, "bob");
    let carol = person(f, "carol");
    let dave = person(f, "dave");
    let board = f.unit("Board", None);
    let finance = f.unit("Finance", Some(&board.unit_id));
    let ceo = f.position(&board, "CEO", None);
    let assistant = f.position_with(&board, "Assistant", Some(&ceo), true);
    let cfo = f.position(&finance, "CFO", Some(&ceo));
    let analyst = f.position(&finance, "Analyst", Some(&cfo));
    let vacant = f.position(&finance, "Analyst 2", Some(&cfo));
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
    f.assign_user(&ceo, &alice, 1.0, None).unwrap();
    f.assign_user(&cfo, &bob, 1.0, None).unwrap();
    f.assign_user(&analyst, &carol, 1.0, None).unwrap();
    f.assign_user(&assistant, &dave, 1.0, None).unwrap();
    set_reporting_line(
        &f.pool,
        &f.ctx(),
        &line(&analyst, &ceo, LineKind::Functional),
    )
    .unwrap();
    Company {
        board,
        finance,
        ceo,
        assistant,
        cfo,
        analyst,
        vacant,
        alice,
        bob,
        carol,
        dave,
    }
}

fn ids(links: &[query::Link]) -> Vec<&str> {
    links.iter().map(|l| l.position_id.as_str()).collect()
}

#[test]
fn every_write_projects_department_manager_and_head_flag() {
    let f = Fixture::new();
    let c = company(&f);

    assert_eq!(
        profile_of(&f.pool, &c.carol),
        Some((c.finance.unit_id.clone(), Some(c.bob.clone()), false))
    );
    assert_eq!(
        profile_of(&f.pool, &c.bob),
        Some((c.finance.unit_id.clone(), Some(c.alice.clone()), true))
    );
    assert_eq!(
        profile_of(&f.pool, &c.alice),
        Some((c.board.unit_id.clone(), None, true))
    );
    // The staff position reports to the CEO like anyone else.
    assert_eq!(
        profile_of(&f.pool, &c.dave),
        Some((c.board.unit_id.clone(), Some(c.alice.clone()), false))
    );
}

#[test]
fn permission_checks_follow_the_structure_after_each_write() {
    let f = Fixture::new();
    let c = company(&f);
    let eve = person(&f, "eve");

    // Manager subtree: the CEO reaches the analyst through the CFO, the CFO does
    // not reach the CEO, someone outside the structure reaches nobody.
    assert!(allowed(&f, &c.alice, "manager_subtree", None, &c.carol));
    assert!(allowed(&f, &c.bob, "manager_subtree", None, &c.carol));
    assert!(!allowed(&f, &c.bob, "manager_subtree", None, &c.alice));
    assert!(!allowed(&f, &eve, "manager_subtree", None, &c.carol));
    // Department scope reads the unit of the primary position.
    assert!(allowed(
        &f,
        &c.carol,
        "department",
        Some(&c.finance.unit_id),
        &c.bob
    ));
    assert!(!allowed(
        &f,
        &c.carol,
        "department",
        Some(&c.board.unit_id),
        &c.bob
    ));
    assert!(!allowed(
        &f,
        &eve,
        "department",
        Some(&c.finance.unit_id),
        &c.bob
    ));

    // The analyst moves directly under the CEO: the CFO stops managing her.
    move_position(
        &f.pool,
        &f.ctx(),
        &c.analyst.position_id,
        Some(&c.ceo.position_id),
        day(0),
    )
    .unwrap();
    assert!(!allowed(&f, &c.bob, "manager_subtree", None, &c.carol));
    assert!(allowed(&f, &c.alice, "manager_subtree", None, &c.carol));

    // Ending her assignment removes her from the department too.
    end_tomorrow(&f, &c.carol);
    assert!(!allowed(
        &f,
        &c.carol,
        "department",
        Some(&c.finance.unit_id),
        &c.bob
    ));
    assert_eq!(profile_of(&f.pool, &c.carol), None);
}

/// Ends the assignment tomorrow (one that began today cannot end today: the
/// interval would be empty) and projects that day.
fn end_tomorrow(f: &Fixture, user: &str) {
    end_assignment(&f.pool, &f.ctx(), &get_assignment_id(f, user), day(1)).unwrap();
    projection::recompute_profiles(&f.pool, ORG, None, day(1)).unwrap();
}

fn get_assignment_id(f: &Fixture, user: &str) -> String {
    query::get_assignment(&f.pool, ORG, user, None)
        .unwrap()
        .primary
        .unwrap()
        .id
}

#[test]
fn a_dated_change_waits_for_its_day_and_the_recompute_of_that_day_applies_it() {
    let f = Fixture::new();
    let c = company(&f);
    let tomorrow = day(1);
    move_position(
        &f.pool,
        &f.ctx(),
        &c.analyst.position_id,
        Some(&c.ceo.position_id),
        tomorrow,
    )
    .unwrap();

    // Today's projection is untouched by a change that starts tomorrow.
    assert_eq!(
        profile_of(&f.pool, &c.carol).unwrap().1,
        Some(c.bob.clone())
    );
    let before = profile_captures(&f.pool);
    let report = projection::recompute_profiles(&f.pool, ORG, None, day(0)).unwrap();
    assert_eq!(report.written + report.removed, 0);
    assert_eq!(profile_captures(&f.pool), before);

    // Once the day comes, the recompute writes exactly the one changed row.
    let report = projection::recompute_profiles(&f.pool, ORG, None, tomorrow).unwrap();
    assert_eq!(report.written, 1);
    assert_eq!(profile_captures(&f.pool), before + 1);
    assert_eq!(
        profile_of(&f.pool, &c.carol).unwrap().1,
        Some(c.alice.clone())
    );
}

#[test]
fn recomputing_an_unchanged_structure_writes_and_captures_nothing() {
    let f = Fixture::new();
    let c = company(&f);
    let captures = capture_count(&f.pool);

    let report = projection::recompute_all(&f.pool, ORG).unwrap();

    assert_eq!((report.written, report.removed), (0, 0));
    assert_eq!(report.unchanged, 4);
    assert_eq!(capture_count(&f.pool), captures);

    // A wrong row is repaired, and only that row.
    f.pool
        .write()
        .unwrap()
        .execute(
            "UPDATE sync_user_org_profiles SET manager_user_id = NULL WHERE user_id = ?1",
            [&c.carol],
        )
        .unwrap();
    let report = projection::recompute_all(&f.pool, ORG).unwrap();
    assert_eq!((report.written, report.unchanged), (1, 3));
    assert_eq!(
        profile_of(&f.pool, &c.carol).unwrap().1,
        Some(c.bob.clone())
    );
}

#[test]
fn a_vacant_manager_position_projects_no_manager() {
    let f = Fixture::new();
    let c = company(&f);
    let erin = person(&f, "erin");
    f.assign_user(&c.vacant, &erin, 1.0, None).unwrap();
    assert_eq!(profile_of(&f.pool, &erin).unwrap().1, Some(c.bob.clone()));

    end_tomorrow(&f, &c.bob);

    // The CFO seat is empty: nobody to name (deputies arrive with WP9).
    assert_eq!(profile_of(&f.pool, &erin).unwrap().1, None);
    assert_eq!(profile_of(&f.pool, &c.carol).unwrap().1, None);
    assert_eq!(effective_manager_on(&f.pool, &erin, 1), None);
}

#[test]
fn only_the_primary_assignment_is_projected() {
    let f = Fixture::new();
    let c = company(&f);
    // Carol also works part time on the board, as a second, non-primary position.
    let second = f.position(&c.board, "Adviser", Some(&c.ceo));
    f.assign_user(&second, &c.carol, 0.2, Some(false)).unwrap();

    assert_eq!(
        profile_of(&f.pool, &c.carol),
        Some((c.finance.unit_id.clone(), Some(c.bob.clone()), false))
    );
    let held = query::get_assignment(&f.pool, ORG, &c.carol, None).unwrap();
    assert_eq!(held.primary.unwrap().position_id, c.analyst.position_id);
    assert_eq!(held.others.len(), 1);
    assert_eq!(held.others[0].position_id, second.position_id);
}

/// Moves the primary mark to `assignment_id`: the API refuses two at once, so
/// the old mark is dropped first.
fn make_primary(f: &Fixture, assignment_id: &str, user: &str) {
    let old = get_assignment_id(f, user);
    update_assignment(
        &f.pool,
        &f.ctx(),
        &old,
        &AssignmentPatch {
            is_primary: Some(false),
            ..AssignmentPatch::default()
        },
        day(0),
    )
    .unwrap();
    update_assignment(
        &f.pool,
        &f.ctx(),
        assignment_id,
        &AssignmentPatch {
            is_primary: Some(true),
            ..AssignmentPatch::default()
        },
        day(0),
    )
    .unwrap();
}

#[test]
fn people_who_report_to_each_other_through_different_seats_are_cut_loose() {
    let f = Fixture::new();
    let c = company(&f);
    // Bob keeps the CFO seat (carol's manager) and takes a second seat UNDER
    // carol's: the positions form a tree, the people form a loop.
    let under_carol = f.position(&c.finance, "Assistant analyst", Some(&c.analyst));
    let second = f
        .assign_user(&under_carol, &c.bob, 0.5, Some(false))
        .unwrap()
        .value;
    // While the CFO seat is primary there is no loop.
    assert_eq!(
        profile_of(&f.pool, &c.bob).unwrap().1,
        Some(c.alice.clone())
    );

    make_primary(&f, &second.id, &c.bob);

    // bob -> carol (via the analyst seat) and carol -> bob (via the CFO seat).
    let report = projection::recompute_all(&f.pool, ORG).unwrap();
    let bob = profile_of(&f.pool, &c.bob).unwrap().1;
    let carol = profile_of(&f.pool, &c.carol).unwrap().1;
    assert_eq!(report.cycles_broken.len(), 1);
    // The smallest id is the one cut, so every node cuts the same edge.
    let cut = report.cycles_broken[0].clone();
    assert_eq!(cut, c.bob.clone().min(c.carol.clone()));
    assert_eq!(
        [bob.is_none(), carol.is_none()]
            .iter()
            .filter(|n| **n)
            .count(),
        1
    );
    // The subtree query of the permission check terminates and answers.
    let _ = allowed(&f, &c.alice, "manager_subtree", None, &c.carol);
}

#[test]
fn a_person_holding_two_seats_in_one_chain_reports_past_themselves() {
    let f = Fixture::new();
    let c = company(&f);
    let junior = f.position(&c.finance, "Junior", Some(&c.cfo));
    let assignment = f
        .assign_user(&junior, &c.bob, 1.0, Some(false))
        .unwrap()
        .value;
    make_primary(&f, &assignment.id, &c.bob);

    // Primary seat is Junior, its parent is the CFO seat, held by bob himself:
    // the manager is the next one up, never bob.
    let manager = effective_manager_on(&f.pool, &c.bob, 0).unwrap();
    assert_eq!(manager.user_id, c.alice);
    assert_eq!(manager.position_id, c.ceo.position_id);
    assert_eq!(
        profile_of(&f.pool, &c.bob).unwrap().1,
        Some(c.alice.clone())
    );
}

#[test]
fn reports_chain_follows_primary_lines_only_and_shows_vacancies() {
    let f = Fixture::new();
    let c = company(&f);

    let up = query::get_reports_chain(
        &f.pool,
        ORG,
        &Target::User(c.carol.clone()),
        Direction::Up,
        SeatScope::Primary,
        None,
    )
    .unwrap();
    // The functional line to the CEO is not a step: CFO first, then CEO.
    assert_eq!(
        ids(&up),
        [c.cfo.position_id.as_str(), c.ceo.position_id.as_str()]
    );
    assert_eq!(up[0].depth, 1);
    assert_eq!(up[1].holders, [Subject::User(c.alice.clone())]);

    let up_from_position = query::get_reports_chain(
        &f.pool,
        ORG,
        &Target::Position(c.vacant.position_id.clone()),
        Direction::Up,
        SeatScope::Primary,
        None,
    )
    .unwrap();
    assert_eq!(up_from_position.len(), 2);

    let down = query::get_reports_chain(
        &f.pool,
        ORG,
        &Target::Position(c.ceo.position_id.clone()),
        Direction::Down,
        SeatScope::Primary,
        None,
    )
    .unwrap();
    let mut got = ids(&down);
    got.sort_unstable();
    let mut want = vec![
        c.assistant.position_id.as_str(),
        c.cfo.position_id.as_str(),
        c.analyst.position_id.as_str(),
        c.vacant.position_id.as_str(),
    ];
    want.sort_unstable();
    assert_eq!(got, want);
    let vacancy = down
        .iter()
        .find(|l| l.position_id == c.vacant.position_id)
        .unwrap();
    assert!(vacancy.holders.is_empty());
    assert_eq!(vacancy.depth, 2);

    // Unknown position on the day: an error, not an empty answer.
    let err = query::get_reports_chain(
        &f.pool,
        ORG,
        &Target::Position("nope".into()),
        Direction::Up,
        SeatScope::Primary,
        None,
    )
    .unwrap_err();
    assert!(
        matches!(err, OrgStructureError::NotValidAt { .. }),
        "{err:?}"
    );
}

#[test]
fn subordinates_direct_or_transitive_and_staff_have_none_of_their_own() {
    let f = Fixture::new();
    let c = company(&f);
    let target = Target::User(c.alice.clone());

    let direct =
        query::get_subordinates(&f.pool, ORG, &target, false, SeatScope::Primary, None).unwrap();
    let mut direct_ids = ids(&direct);
    direct_ids.sort_unstable();
    let mut want = vec![c.assistant.position_id.as_str(), c.cfo.position_id.as_str()];
    want.sort_unstable();
    assert_eq!(direct_ids, want);

    let all =
        query::get_subordinates(&f.pool, ORG, &target, true, SeatScope::Primary, None).unwrap();
    assert_eq!(all.len(), 4);

    let staff = query::get_subordinates(
        &f.pool,
        ORG,
        &Target::User(c.dave.clone()),
        true,
        SeatScope::Primary,
        None,
    )
    .unwrap();
    assert!(staff.is_empty());

    assert!(
        query::is_subordinate_of(&f.pool, ORG, &c.carol, &c.alice, SeatScope::Primary, None)
            .unwrap()
    );
    assert!(
        query::is_subordinate_of(&f.pool, ORG, &c.dave, &c.alice, SeatScope::Primary, None)
            .unwrap()
    );
    assert!(
        !query::is_subordinate_of(&f.pool, ORG, &c.alice, &c.carol, SeatScope::Primary, None)
            .unwrap()
    );
    assert!(
        !query::is_subordinate_of(&f.pool, ORG, &c.alice, &c.alice, SeatScope::Primary, None)
            .unwrap()
    );
    // The staff assistant does not manage the person they work next to.
    assert!(
        !query::is_subordinate_of(&f.pool, ORG, &c.bob, &c.dave, SeatScope::Primary, None).unwrap()
    );
}

#[test]
fn the_snapshot_of_a_past_and_a_future_day_shows_that_day() {
    let f = Fixture::new();
    let c = company(&f);
    // A reorganization next week: the analyst moves under the CEO and the CFO leaves.
    move_position(
        &f.pool,
        &f.ctx(),
        &c.analyst.position_id,
        Some(&c.ceo.position_id),
        day(7),
    )
    .unwrap();
    end_assignment(&f.pool, &f.ctx(), &get_assignment_id(&f, &c.bob), day(7)).unwrap();
    // A unit that existed only in the past, with its seat and its holder.
    let legacy_person = person(&f, "legacy");
    let ctx = f.confirmed();
    let legacy = create_unit(
        &f.pool,
        &ctx,
        &NewUnit {
            name: "Legacy".into(),
            code: None,
            type_id: None,
            parent_unit_id: None,
            color: None,
            valid_from: day(-10),
            valid_to: Some(day(-2)),
        },
    )
    .unwrap()
    .value;
    let seat = create_position(
        &f.pool,
        &ctx,
        &NewPosition {
            unit_id: legacy.unit_id.clone(),
            name: "Old seat".into(),
            code: None,
            role_id: None,
            is_manager: None,
            is_staff: false,
            parent_position_id: None,
            valid_from: day(-10),
            valid_to: Some(day(-2)),
        },
    )
    .unwrap()
    .value;
    assign(
        &f.pool,
        &ctx,
        &NewAssignment {
            position_id: seat.position_id.clone(),
            subject: Subject::User(legacy_person.clone()),
            kind: AssignmentType::Permanent,
            share: 1.0,
            is_primary: None,
            valid_from: day(-10),
            valid_to: Some(day(-2)),
        },
    )
    .unwrap();

    let today = query::structure_as_of(&f.pool, ORG, None).unwrap();
    let next_week = query::structure_as_of(&f.pool, ORG, Some(day(7))).unwrap();
    let last_week = query::structure_as_of(&f.pool, ORG, Some(day(-5))).unwrap();

    let parent_of = |view: &query::StructureView, position: &Position| {
        view.positions
            .iter()
            .find(|p| p.position.position_id == position.position_id)
            .unwrap()
            .primary_parent_position_id
            .clone()
    };
    assert_eq!(
        parent_of(&today, &c.analyst),
        Some(c.cfo.position_id.clone())
    );
    assert_eq!(
        parent_of(&next_week, &c.analyst),
        Some(c.ceo.position_id.clone())
    );

    // Next week the CFO seat is empty; today it is not.
    assert!(!today.vacancies.contains(&c.cfo.position_id));
    assert!(next_week.vacancies.contains(&c.cfo.position_id));
    assert!(today.vacancies.contains(&c.vacant.position_id));

    // The past: the legacy unit and its holder are there, the company is not
    // yet (everything else starts today).
    assert_eq!(last_week.units.len(), 1);
    assert_eq!(last_week.units[0].unit.name, "Legacy");
    assert_eq!(last_week.positions.len(), 1);
    assert_eq!(last_week.assignments.len(), 1);
    assert_eq!(last_week.assignments[0].person.display_name, "legacy");
    assert_eq!(last_week.at, day(-5));
    assert!(today.units.iter().all(|u| u.unit.name != "Legacy"));
    assert_eq!(today.units.len(), 2);

    // Persons carry a display name; units carry their deputies list.
    let carol = today
        .assignments
        .iter()
        .find(|a| a.assignment.subject == Subject::User(c.carol.clone()))
        .unwrap();
    assert_eq!(carol.person.display_name, "carol");
    assert!(today
        .units
        .iter()
        .all(|u| u.deputy_head_position_ids.is_empty()));
    assert_eq!(today.timezone, DEFAULT_TIMEZONE);
    let analyst = today
        .positions
        .iter()
        .find(|p| p.position.position_id == c.analyst.position_id)
        .unwrap();
    assert_eq!(
        analyst.functional_parent_position_ids,
        std::slice::from_ref(&c.ceo.position_id)
    );
    assert!(today.positions.iter().any(|p| p.is_head));
}

#[test]
fn the_snapshot_warns_about_units_without_head_overbooking_and_missing_primary() {
    let f = Fixture::new();
    let c = company(&f);
    let headless = f.unit("Headless", Some(&c.board.unit_id));
    let a = f.position(&headless, "A", Some(&c.ceo));
    let b = f.position(&headless, "B", Some(&c.ceo));
    let x = person(&f, "x");
    f.assign_user(&a, &x, 0.8, None).unwrap();
    f.assign_user(&b, &x, 0.8, Some(false)).unwrap();
    // Two seats, none primary: the administrator has not chosen.
    f.pool
        .write()
        .unwrap()
        .execute(
            "UPDATE org_assignments SET is_primary = 0 WHERE user_id = ?1",
            [&x],
        )
        .unwrap();

    let view = query::structure_as_of(&f.pool, ORG, None).unwrap();
    assert!(view.warnings.contains(&Warning::UnitWithoutHead {
        unit_id: headless.unit_id.clone(),
        from: day(0),
    }));
    assert!(view
        .warnings
        .iter()
        .any(|w| matches!(w, Warning::ShareOverbooked { subject, .. } if *subject == Subject::User(x.clone()))));
    assert!(view
        .warnings
        .iter()
        .any(|w| matches!(w, Warning::PersonWithoutPrimary { subject, .. } if *subject == Subject::User(x.clone()))));
}

#[test]
fn nightly_runs_once_per_local_day_after_five_past_midnight() {
    let at = |h, m| Utc.with_ymd_and_hms(2026, 3, 1, h, m, 0).unwrap();
    // Warsaw is UTC+1 in early March: 23:04 UTC is 00:04 the next local day.
    let too_early = nightly::due_day(at(23, 4), "Europe/Warsaw", None).unwrap();
    assert_eq!(too_early, None);
    let due = nightly::due_day(at(23, 5), "Europe/Warsaw", None).unwrap();
    let local_day = NaiveDate::from_ymd_opt(2026, 3, 2).unwrap();
    assert_eq!(due, Some(local_day));
    // Already done that day; and a node that was down catches up later in the day.
    assert_eq!(
        nightly::due_day(at(23, 5), "Europe/Warsaw", Some(local_day)).unwrap(),
        None
    );
    assert_eq!(
        nightly::due_day(at(9, 0), "Europe/Warsaw", None).unwrap(),
        Some(NaiveDate::from_ymd_opt(2026, 3, 1).unwrap())
    );
    // The organization's day, not the server's: Los Angeles is still on the 1st.
    assert_eq!(
        nightly::due_day(at(23, 5), "America/Los_Angeles", None).unwrap(),
        Some(NaiveDate::from_ymd_opt(2026, 3, 1).unwrap())
    );
    assert!(nightly::due_day(at(1, 0), "Nowhere/Land", None).is_err());
}

#[test]
fn the_nightly_tick_recomputes_a_due_organization_once() {
    let f = Fixture::new();
    let c = company(&f);
    move_position(
        &f.pool,
        &f.ctx(),
        &c.analyst.position_id,
        Some(&c.ceo.position_id),
        day(1),
    )
    .unwrap();
    let tomorrow_noon = Utc::now() + chrono::Duration::hours(30);

    let mut last_run = HashMap::new();
    let before = profile_captures(&f.pool);
    nightly::run_due(&f.pool, tomorrow_noon, &mut last_run);
    assert_eq!(profile_captures(&f.pool), before + 1);
    assert_eq!(
        profile_of(&f.pool, &c.carol).unwrap().1,
        Some(c.alice.clone())
    );
    assert!(last_run.contains_key(ORG));

    // The same day again: nothing to do.
    nightly::run_due(&f.pool, tomorrow_noon, &mut last_run);
    assert_eq!(profile_captures(&f.pool), before + 1);
}

#[test]
fn two_thousand_people_are_read_and_projected_quickly() {
    let f = Fixture::new();
    const PEOPLE: usize = 2000;
    let users: Vec<String> = (0..PEOPLE)
        .map(|i| add_user(&f.pool, ORG, &format!("u{i}")))
        .collect();
    {
        let mut conn = f.pool.write().unwrap();
        let tx = conn.transaction().unwrap();
        let from = validate::format_date(day(-30));
        // 40 units of 50 seats; seat i reports to seat (i - 1) / 5, a tree of depth ~5.
        for unit in 0..40 {
            tx.execute(
                "INSERT INTO org_units (id, org_id, unit_id, name, head_position_id, valid_from) \
                 VALUES (?1, ?2, ?1, ?3, ?4, ?5)",
                rusqlite::params![
                    format!("un{unit}"),
                    ORG,
                    format!("Unit {unit}"),
                    format!("p{}", unit * 50),
                    from
                ],
            )
            .unwrap();
        }
        for (i, user) in users.iter().enumerate() {
            tx.execute(
                "INSERT INTO org_positions (id, org_id, position_id, unit_id, name, is_staff, valid_from) \
                 VALUES (?1, ?2, ?1, ?3, ?4, 0, ?5)",
                rusqlite::params![format!("p{i}"), ORG, format!("un{}", i / 50), format!("Seat {i}"), from],
            )
            .unwrap();
            if i > 0 {
                tx.execute(
                    "INSERT INTO org_reporting_lines (id, org_id, position_id, parent_position_id, kind, valid_from) \
                     VALUES (?1, ?2, ?3, ?4, 'primary', ?5)",
                    rusqlite::params![format!("l{i}"), ORG, format!("p{i}"), format!("p{}", (i - 1) / 5), from],
                )
                .unwrap();
            }
            tx.execute(
                "INSERT INTO org_assignments (id, org_id, position_id, user_id, type, share, is_primary, valid_from) \
                 VALUES (?1, ?2, ?3, ?4, 'permanent', 1.0, 1, ?5)",
                rusqlite::params![format!("a{i}"), ORG, format!("p{i}"), user, from],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }

    let started = Instant::now();
    let view = query::structure_as_of(&f.pool, ORG, None).unwrap();
    let snapshot_ms = started.elapsed().as_millis();
    assert_eq!(view.assignments.len(), PEOPLE);
    assert_eq!(view.positions.len(), PEOPLE);

    let started = Instant::now();
    let below = query::get_subordinates(
        &f.pool,
        ORG,
        &Target::Position("p0".into()),
        true,
        SeatScope::Primary,
        None,
    )
    .unwrap();
    let subtree_ms = started.elapsed().as_millis();
    assert_eq!(below.len(), PEOPLE - 1);

    let started = Instant::now();
    let report = projection::recompute_all(&f.pool, ORG).unwrap();
    let projection_ms = started.elapsed().as_millis();
    assert_eq!(report.written, PEOPLE);
    let started = Instant::now();
    let report = projection::recompute_all(&f.pool, ORG).unwrap();
    let idle_ms = started.elapsed().as_millis();
    assert_eq!((report.written, report.unchanged), (0, PEOPLE));

    eprintln!(
        "2000 people: snapshot {snapshot_ms} ms, subtree {subtree_ms} ms, \
         first projection {projection_ms} ms, idle projection {idle_ms} ms"
    );
    // Generous: this is a debug build on a shared machine; release is far below
    // the 100 ms target.
    assert!(snapshot_ms < 1000, "snapshot took {snapshot_ms} ms");
    assert!(subtree_ms < 1000, "subtree took {subtree_ms} ms");
    assert!(idle_ms < 2000, "idle recompute took {idle_ms} ms");
}

#[test]
fn a_secondary_seat_counts_as_subordination_only_when_asked_for() {
    let f = Fixture::new();
    let c = company(&f);
    // Alice (CEO, primary) also holds a seat under the CFO: bob is her manager
    // there, but the permission grant follows her primary seat only.
    let under_cfo = f.position(&c.finance, "Adviser", Some(&c.cfo));
    f.assign_user(&under_cfo, &c.alice, 0.3, Some(false))
        .unwrap();

    let sub = |user: &str, manager: &str, scope| {
        query::is_subordinate_of(&f.pool, ORG, user, manager, scope, None).unwrap()
    };
    assert!(!sub(&c.alice, &c.bob, SeatScope::Primary));
    assert!(sub(&c.alice, &c.bob, SeatScope::All));
    assert!(!allowed(&f, &c.bob, "manager_subtree", None, &c.alice));

    let below = |scope| {
        query::get_subordinates(
            &f.pool,
            ORG,
            &Target::User(c.bob.clone()),
            true,
            scope,
            None,
        )
        .unwrap()
        .len()
    };
    // Bob also leads a small side team through a second seat.
    let lead = f.position(&c.board, "Side lead", None);
    f.position(&c.board, "Side clerk", Some(&lead));
    f.assign_user(&lead, &c.bob, 0.2, Some(false)).unwrap();
    // Primary: below the CFO seat (analysts and alice's adviser seat). All: plus the side team.
    assert_eq!(below(SeatScope::Primary), 3);
    assert_eq!(below(SeatScope::All), 4);
}

#[test]
fn a_manager_who_left_the_organization_is_not_projected() {
    let f = Fixture::new();
    let c = company(&f);
    assert_eq!(
        profile_of(&f.pool, &c.carol).unwrap().1,
        Some(c.bob.clone())
    );

    f.pool
        .write()
        .unwrap()
        .execute(
            "DELETE FROM org_memberships WHERE org_id = ?1 AND user_id = ?2",
            [ORG, &c.bob],
        )
        .unwrap();
    let report = projection::recompute_all(&f.pool, ORG).unwrap();

    // Bob's own row goes with his membership; carol's manager is NULL, exactly
    // as for a vacancy.
    assert_eq!(profile_of(&f.pool, &c.bob), None);
    assert_eq!(profile_of(&f.pool, &c.carol).unwrap().1, None);
    assert!(report.written >= 1 && report.removed == 1, "{report:?}");
}

#[test]
fn the_manager_subtree_check_terminates_on_a_replicated_loop() {
    let f = Fixture::new();
    let (a, b, c, outsider) = (
        person(&f, "a"),
        person(&f, "b"),
        person(&f, "c"),
        person(&f, "outsider"),
    );
    // Rows as two nodes may have left them: a -> b -> c -> a, plus c's report.
    for (user, manager) in [(&a, &b), (&b, &c), (&c, &a)] {
        f.pool
            .write()
            .unwrap()
            .execute(
                "INSERT INTO sync_user_org_profiles (org_id, user_id, department_id, manager_user_id) \
                 VALUES (?1, ?2, 'd', ?3)",
                [ORG, user, manager],
            )
            .unwrap();
    }
    assert!(allowed(&f, &a, "manager_subtree", None, &c));
    assert!(allowed(&f, &c, "manager_subtree", None, &b));
    assert!(!allowed(&f, &a, "manager_subtree", None, &outsider));
}

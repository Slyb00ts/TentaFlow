//! History and diff: the pure comparison of two views, and the audit trail read
//! back through the versioned rows with the privacy rule of §6.3.

use tentaflow_protocol::org_structure::{
    OrgAssignment, OrgAssignmentType, OrgPosition, OrgStructureView, OrgSubject, OrgUnit,
};

use super::history::{self, DiffContext, HistoryQuery, Privacy};
use super::tests::{add_user, day, s, Fixture, ORG};
use super::*;

const ADMIN: Privacy<'static> = Privacy {
    personal_visible: true,
    viewer_user_id: "admin",
};

fn viewer(id: &str) -> Privacy<'_> {
    Privacy {
        personal_visible: false,
        viewer_user_id: id,
    }
}

fn unit(id: &str, name: &str, parent: Option<&str>, head: Option<&str>) -> OrgUnit {
    OrgUnit {
        id: format!("row-{id}"),
        unit_id: id.into(),
        name: name.into(),
        parent_unit_id: parent.map(str::to_string),
        head_position_id: head.map(str::to_string),
        valid_from: "2026-01-01".into(),
        ..Default::default()
    }
}

fn position(id: &str, unit_id: &str, name: &str, parent: Option<&str>) -> OrgPosition {
    OrgPosition {
        id: format!("row-{id}"),
        position_id: id.into(),
        unit_id: unit_id.into(),
        name: name.into(),
        primary_parent_position_id: parent.map(str::to_string),
        valid_from: "2026-01-01".into(),
        ..Default::default()
    }
}

fn holder(position_id: &str, user: &str, name: &str) -> OrgAssignment {
    OrgAssignment {
        id: format!("a-{position_id}-{user}"),
        position_id: position_id.into(),
        subject: OrgSubject::User(user.into()),
        assignment_type: OrgAssignmentType::Permanent,
        share: 1.0,
        is_primary: true,
        valid_from: "2026-01-01".into(),
        valid_to: None,
        display_name: name.into(),
    }
}

fn view(at: &str) -> OrgStructureView {
    OrgStructureView {
        at: at.into(),
        timezone: "Europe/Warsaw".into(),
        ..Default::default()
    }
}

/// A company: Board > IT (head: CTO held by Adam) > DevOps (lead held by Ewa).
fn before() -> OrgStructureView {
    let mut v = view("2026-09-30");
    v.units = vec![
        unit("board", "Board", None, None),
        unit("it", "IT", Some("board"), Some("cto")),
        unit("devops", "DevOps", Some("it"), Some("lead")),
    ];
    v.positions = vec![
        position("cto", "it", "CTO", None),
        position("lead", "devops", "Lead", Some("cto")),
    ];
    v.assignments = vec![holder("cto", "adam", "Adam"), holder("lead", "ewa", "Ewa")];
    v
}

fn item<'a>(
    items: &'a [tentaflow_protocol::org_history::OrgDiffItem],
    entity: &str,
    id: &str,
    field: Option<&str>,
) -> &'a tentaflow_protocol::org_history::OrgDiffItem {
    items
        .iter()
        .find(|i| i.entity == entity && i.id == id && i.field.as_deref() == field)
        .unwrap_or_else(|| panic!("no {entity} {id} {field:?} in {items:#?}"))
}

#[test]
fn diff_names_what_changed_in_units_positions_and_holders() {
    let a = before();
    let mut b = before();
    b.at = "2026-11-01".into();
    b.units[2].parent_unit_id = Some("board".into());
    b.units[2].name = "Platform".into();
    b.units.push(unit("qa", "Quality", Some("board"), None));
    b.positions[1].primary_parent_position_id = None;
    b.positions.push(position("tester", "qa", "Tester", None));
    b.assignments.retain(|x| x.position_id != "lead");
    b.assignments.push(holder("tester", "kasia", "Kasia"));

    let items = history::diff_views(&a, &b, &ADMIN, &DiffContext::default());

    assert_eq!(item(&items, "unit", "qa", None).change, "added");
    let moved = item(&items, "unit", "devops", Some("parent_unit_id"));
    assert_eq!(
        (moved.before_label.as_deref(), moved.after_label.as_deref()),
        (Some("IT"), Some("Board")),
        "a reference is shown by name"
    );
    assert_eq!(
        item(&items, "unit", "devops", Some("name"))
            .after
            .as_deref(),
        Some("Platform")
    );
    let line = item(
        &items,
        "position",
        "lead",
        Some("primary_parent_position_id"),
    );
    assert_eq!(
        line.before_label.as_deref(),
        Some("Adam"),
        "the holder, not the title, on a line"
    );
    assert_eq!(line.after, None);
    assert_eq!(item(&items, "position", "tester", None).change, "added");
    let left = item(&items, "assignment", "lead", None);
    assert_eq!(
        (left.change.as_str(), left.subject_name.as_deref()),
        ("removed", Some("Ewa"))
    );
    let joined = item(&items, "assignment", "tester", None);
    assert_eq!(
        (joined.change.as_str(), joined.subject_name.as_deref()),
        ("added", Some("Kasia"))
    );
}

#[test]
fn diff_of_identical_views_is_empty() {
    assert!(history::diff_views(&before(), &before(), &ADMIN, &DiffContext::default()).is_empty());
}

#[test]
fn diff_can_be_limited_to_a_unit_and_its_subtree() {
    let a = before();
    let mut b = before();
    b.units[0].name = "Zarząd".into();
    b.positions[1].name = "Team lead".into();

    let all = history::diff_views(&a, &b, &ADMIN, &DiffContext::default());
    assert!(all.iter().any(|i| i.id == "board") && all.iter().any(|i| i.id == "lead"));

    let it = history::diff_views(
        &a,
        &b,
        &ADMIN,
        &DiffContext {
            unit_id: Some("it"),
            ..Default::default()
        },
    );
    assert!(
        it.iter().all(|i| i.id != "board"),
        "the parent is outside: {it:?}"
    );
    assert!(
        it.iter().any(|i| i.id == "lead"),
        "a position of a sub-unit is inside: {it:?}"
    );
}

#[test]
fn a_viewer_sees_no_other_persons_history_in_a_diff() {
    let a = before();
    let mut b = before();
    b.assignments.retain(|x| x.position_id != "lead");
    b.assignments.push(holder("lead", "kasia", "Kasia"));
    b.units[1].head_position_id = Some("lead".into());

    let for_ewa = history::diff_views(&a, &b, &viewer("ewa"), &DiffContext::default());
    let people: Vec<_> = for_ewa
        .iter()
        .filter(|i| i.entity == "assignment")
        .collect();
    assert_eq!(people.len(), 1, "only her own leaving: {people:?}");
    assert_eq!(people[0].subject, Some(OrgSubject::User("ewa".into())));

    let for_stranger = history::diff_views(&a, &b, &viewer("nobody"), &DiffContext::default());
    assert!(
        for_stranger.iter().all(|i| i.entity != "assignment"),
        "{for_stranger:?}"
    );
    let head = |items: &[tentaflow_protocol::org_history::OrgDiffItem]| {
        let i = item(items, "unit", "it", Some("head_position_id")).clone();
        (i.before_label.unwrap(), i.after_label.unwrap())
    };
    assert_eq!(
        head(&for_stranger),
        ("CTO".to_string(), "Lead".to_string()),
        "labels name positions, not people, for a viewer who may not see them"
    );
    let for_admin = history::diff_views(&a, &b, &ADMIN, &DiffContext::default());
    assert_eq!(head(&for_admin), ("Adam".to_string(), "Kasia".to_string()));
}

#[test]
fn a_past_snapshot_hides_other_people_but_not_the_structure() {
    let mut v = before();
    v.at = "2026-01-15".into();
    let today = day(0);
    history::pseudonymize_past_holders(&mut v, today, &viewer("ewa"));
    let names: Vec<_> = v
        .assignments
        .iter()
        .map(|a| a.display_name.as_str())
        .collect();
    assert_eq!(
        names,
        ["Osoba #1", "Ewa"],
        "her own seat stays, the other is numbered"
    );
    assert!(v
        .assignments
        .iter()
        .all(|a| a.subject != OrgSubject::User("adam".into())));
    assert_eq!(v.positions.len(), 2, "the structure is open to everyone");

    let mut admin_view = before();
    admin_view.at = "2026-01-15".into();
    history::pseudonymize_past_holders(&mut admin_view, today, &ADMIN);
    assert_eq!(admin_view.assignments[0].display_name, "Adam");

    let mut current = before();
    current.at = s(today);
    history::pseudonymize_past_holders(&mut current, today, &viewer("ewa"));
    assert_eq!(
        current.assignments[0].display_name, "Adam",
        "today is not history"
    );
}

// ---------------------------------------------------------------------------
// The audit trail
// ---------------------------------------------------------------------------

struct World {
    f: Fixture,
    other: String,
}

fn world() -> World {
    let f = Fixture::new();
    let other = add_user(&f.pool, ORG, "other");
    World { f, other }
}

fn page(w: &World, privacy: &Privacy<'_>, query: HistoryQuery) -> history::HistoryPage {
    history::list_changes(&w.f.pool, ORG, privacy, &query).unwrap()
}

fn admin(w: &World) -> Privacy<'_> {
    Privacy {
        personal_visible: true,
        viewer_user_id: &w.f.actor,
    }
}

#[test]
fn the_audit_trail_reads_back_as_named_changes_with_their_effective_day() {
    let w = world();
    let f = &w.f;
    let root = f.unit("Board", None);
    let it = f.unit("IT", Some(root.unit_id.as_str()));
    let devops = f.unit("DevOps", Some(it.unit_id.as_str()));
    move_unit(
        &f.pool,
        &f.ctx(),
        &devops.unit_id,
        Some(root.unit_id.as_str()),
        day(40),
    )
    .unwrap();

    let planned = page(&w, &admin(&w), HistoryQuery::default());
    let first = &planned.entries[0];
    assert_eq!(
        first.action, "org.unit.move",
        "the planned change comes first"
    );
    assert_eq!(first.effective_date.as_deref(), Some(s(day(40)).as_str()));
    assert_eq!(first.target_name.as_deref(), Some("DevOps"));
    assert_eq!(first.actor_user_id.as_deref(), Some(f.actor.as_str()));
    assert!(first.actor_name.is_some());
    let change = &first.changes[0];
    assert_eq!(change.field, "parent_unit_id");
    assert_eq!(
        (
            change.before_label.as_deref(),
            change.after_label.as_deref()
        ),
        (Some("IT"), Some("Board"))
    );
    assert_eq!(planned.total, 4);
    assert_eq!(planned.today, day(0));

    // The bounds are on the EFFECTIVE day, not on when it was written.
    let only_today = page(
        &w,
        &admin(&w),
        HistoryQuery {
            to: Some(day(0)),
            ..Default::default()
        },
    );
    assert_eq!(only_today.total, 3);
    let only_future = page(
        &w,
        &admin(&w),
        HistoryQuery {
            from: Some(day(1)),
            ..Default::default()
        },
    );
    assert_eq!(only_future.total, 1);
}

#[test]
fn the_history_can_be_limited_to_a_unit_and_paged() {
    let w = world();
    let f = &w.f;
    let a = f.unit("A", None);
    let b = f.unit("B", None);
    let pos_a = f.position(&a, "In A", None);
    f.position(&b, "In B", None);
    update_unit(
        &f.pool,
        &f.ctx(),
        &a.unit_id,
        &UnitPatch {
            name: Some("A2".into()),
            ..Default::default()
        },
        day(0),
    )
    .unwrap();

    let of_a = page(
        &w,
        &admin(&w),
        HistoryQuery {
            unit_id: Some(a.unit_id.clone()),
            ..Default::default()
        },
    );
    assert!(of_a
        .entries
        .iter()
        .all(|e| e.unit_id.as_deref() == Some(a.unit_id.as_str())));
    assert!(of_a
        .entries
        .iter()
        .any(|e| e.target_id == pos_a.position_id));
    assert_eq!(of_a.total, 3, "A created, its position, its rename");

    let paged = page(
        &w,
        &admin(&w),
        HistoryQuery {
            limit: 2,
            offset: 1,
            ..Default::default()
        },
    );
    assert_eq!((paged.entries.len(), paged.total), (2, 5));
}

#[test]
fn position_history_of_a_person_is_the_persons_and_an_administrators() {
    let w = world();
    let f = &w.f;
    let unit = f.unit("IT", None);
    let pos = f.position(&unit, "Dev", None);
    f.assign_user(&pos, &w.other, 1.0, None).unwrap();
    let boss = f.position(&unit, "Boss", None);
    f.assign_user(&boss, &f.actor, 1.0, None).unwrap();
    create_external_person(
        &f.pool,
        &f.ctx(),
        &NewExternalPerson {
            display_name: "Kontraktor".into(),
            email: None,
            note: None,
        },
    )
    .unwrap();

    let assignments = |privacy: &Privacy<'_>| -> Vec<String> {
        page(&w, privacy, HistoryQuery::default())
            .entries
            .into_iter()
            .filter(|e| e.target_kind == "assignment")
            .map(|e| e.subject_name.unwrap_or_default())
            .collect()
    };
    let everything = page(&w, &admin(&w), HistoryQuery::default());
    assert!(everything
        .entries
        .iter()
        .any(|e| e.target_kind == "external_person"));
    assert_eq!(assignments(&admin(&w)).len(), 2);

    let as_other = viewer(&w.other);
    let own = assignments(&as_other);
    assert_eq!(own.len(), 1, "only the viewer's own seat: {own:?}");
    let seen = page(&w, &as_other, HistoryQuery::default());
    assert!(
        seen.entries
            .iter()
            .all(|e| e.target_kind != "external_person"),
        "an external person is personal data"
    );
    assert!(
        seen.entries.iter().any(|e| e.target_kind == "position"),
        "the structural changes stay"
    );
    assert_eq!(
        seen.total,
        everything.total - 2,
        "the other assignment and the external person"
    );

    let as_stranger = viewer("nobody");
    assert!(assignments(&as_stranger).is_empty());
}

#[test]
fn a_batch_entry_lists_its_operations_minus_the_personal_ones_for_a_viewer() {
    use super::batch::{self, Decision, Op};
    let w = world();
    let f = &w.f;
    let unit = f.unit("IT", None);
    let pos = f.position(&unit, "Dev", None);
    batch::run(&f.pool, &f.ctx(), true, |b| {
        b.apply(
            &Op::Assign(NewAssignment {
                position_id: pos.position_id.clone(),
                subject: Subject::User(w.other.clone()),
                kind: AssignmentType::Permanent,
                share: 1.0,
                is_primary: None,
                valid_from: day(0),
                valid_to: None,
            }),
            false,
        )
        .unwrap();
        b.apply(
            &Op::PositionUpdate {
                position_id: pos.position_id.clone(),
                patch: PositionPatch {
                    name: Some("Senior Dev".into()),
                    ..Default::default()
                },
                from: day(0),
            },
            false,
        )
        .unwrap();
        Ok(Decision {
            value: (),
            keep: true,
            summary: serde_json::json!({ "ops": 2, "preview_at": s(day(0)) }),
        })
    })
    .unwrap();

    let batch_of = |privacy: &Privacy<'_>| {
        page(&w, privacy, HistoryQuery::default())
            .entries
            .into_iter()
            .find(|e| e.target_kind == "structure")
            .expect("the batch entry")
    };
    let full = batch_of(&admin(&w));
    assert_eq!((full.ops.len(), full.hidden_ops), (2, 0));
    assert_eq!(full.source.as_deref(), Some("batch"));

    let seen = batch_of(&viewer("nobody"));
    assert_eq!((seen.ops.len(), seen.hidden_ops), (1, 1));
    assert_eq!(seen.ops[0].action, "org.position.update");
    assert_eq!(seen.ops[0].target_name.as_deref(), Some("Senior Dev"));
}

#[test]
fn the_batch_of_a_planned_reorganization_is_the_administrators_in_every_state() {
    use super::batch::{self, Decision, Op};
    let w = world();
    let f = &w.f;
    let unit = f.unit("IT", None);
    let pos = f.position(&unit, "Dev", None);
    batch::run(&f.pool, &f.ctx(), true, |b| {
        b.apply(
            &Op::PositionUpdate {
                position_id: pos.position_id.clone(),
                patch: PositionPatch {
                    name: Some("Senior Dev".into()),
                    ..Default::default()
                },
                from: day(0),
            },
            false,
        )
        .unwrap();
        Ok(Decision {
            value: (),
            keep: true,
            summary: serde_json::json!({ "ops": 1, "preview_at": s(day(0)), "change_set": true }),
        })
    })
    .unwrap();

    let structure = |privacy: &Privacy<'_>| {
        page(&w, privacy, HistoryQuery::default())
            .entries
            .into_iter()
            .filter(|e| e.target_kind == "structure")
            .count()
    };
    assert_eq!(structure(&admin(&w)), 1);
    assert_eq!(structure(&viewer("nobody")), 0);
}

#[test]
fn another_organizations_changes_are_not_listed() {
    let w = world();
    w.f.unit("Mine", None);
    let elsewhere = history::list_changes(
        &w.f.pool,
        "another-org",
        &admin(&w),
        &HistoryQuery::default(),
    );
    assert!(matches!(elsewhere, Ok(p) if p.total == 0));
}

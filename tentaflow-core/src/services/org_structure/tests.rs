use std::collections::BTreeMap;

use chrono::{Days, NaiveDate};
use tempfile::TempDir;

use super::*;
use crate::db::DbPool;
use crate::services::org::{self, DEFAULT_ORG_ID};
use crate::sync::core_capture::load_core_write_capture;
use crate::sync::ledger::{
    ActionType, BaselineEpoch, FieldValue, HybridLogicalTimestamp, OperationId, PartitionId,
    SyncOperation, SyncOperationBody,
};
use crate::sync::runtime::SqlWriteAction;

pub(super) const ORG: &str = DEFAULT_ORG_ID;

pub(super) struct Fixture {
    _dir: TempDir,
    pub(super) pool: DbPool,
    pub(super) actor: String,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let pool = crate::db::init(&dir.path().join("org_structure.db")).unwrap();
        let actor = add_user(&pool, ORG, "admin");
        Self {
            _dir: dir,
            pool,
            actor,
        }
    }

    pub(super) fn ctx(&self) -> WriteCtx<'_> {
        self.ctx_in(ORG)
    }

    pub(super) fn ctx_in<'a>(&'a self, org_id: &'a str) -> WriteCtx<'a> {
        WriteCtx {
            org_id,
            actor_user_id: &self.actor,
            confirm_backdated: false,
        }
    }

    pub(super) fn confirmed(&self) -> WriteCtx<'_> {
        WriteCtx {
            confirm_backdated: true,
            ..self.ctx()
        }
    }

    pub(super) fn unit(&self, name: &str, parent: Option<&str>) -> Unit {
        create_unit(
            &self.pool,
            &self.ctx(),
            &NewUnit {
                name: name.into(),
                code: None,
                type_id: None,
                parent_unit_id: parent.map(str::to_string),
                color: None,
                valid_from: day(0),
                valid_to: None,
            },
        )
        .unwrap()
        .value
    }

    pub(super) fn position(&self, unit: &Unit, name: &str, parent: Option<&Position>) -> Position {
        self.position_with(unit, name, parent, false)
    }

    pub(super) fn position_with(
        &self,
        unit: &Unit,
        name: &str,
        parent: Option<&Position>,
        is_staff: bool,
    ) -> Position {
        create_position(
            &self.pool,
            &self.ctx(),
            &NewPosition {
                unit_id: unit.unit_id.clone(),
                name: name.into(),
                code: None,
                role_id: None,
                is_manager: None,
                is_staff,
                parent_position_id: parent.map(|p| p.position_id.clone()),
                valid_from: day(0),
                valid_to: None,
            },
        )
        .unwrap()
        .value
    }

    pub(super) fn assign_user(
        &self,
        position: &Position,
        user: &str,
        share: f64,
        is_primary: Option<bool>,
    ) -> Result<Written<Assignment>> {
        assign(
            &self.pool,
            &self.ctx(),
            &NewAssignment {
                position_id: position.position_id.clone(),
                subject: Subject::User(user.to_string()),
                kind: AssignmentType::Permanent,
                share,
                is_primary,
                valid_from: day(0),
                valid_to: None,
            },
        )
    }

    fn lines(&self, position: &Position) -> Vec<(String, String, Option<String>)> {
        let conn = self.pool.read().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT parent_position_id, valid_from, valid_to FROM org_reporting_lines \
                 WHERE position_id = ?1 AND kind = 'primary' ORDER BY valid_from",
            )
            .unwrap();
        stmt.query_map([&position.position_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    }
}

pub(super) fn add_user(pool: &DbPool, org_id: &str, username: &str) -> String {
    let id = crate::db::repository::create_user_account(
        pool,
        &format!("{username}-{org_id}"),
        "hash",
        username,
        &format!("{username}@example.test"),
    )
    .unwrap();
    let role_id: String = pool
        .read()
        .unwrap()
        .query_row(
            "SELECT role_id FROM roles WHERE org_id = ?1 OR org_id IS NULL LIMIT 1",
            [org_id],
            |r| r.get(0),
        )
        .or_else(|_| {
            pool.read()
                .unwrap()
                .query_row("SELECT role_id FROM roles LIMIT 1", [], |r| r.get(0))
        })
        .unwrap();
    org::add_membership(pool, org_id, &id, &role_id, "test").unwrap();
    id
}

pub(super) fn today() -> NaiveDate {
    validate::today_in_zone(DEFAULT_TIMEZONE).unwrap()
}

/// The organization's today plus `offset` days.
pub(super) fn day(offset: i64) -> NaiveDate {
    if offset >= 0 {
        today() + Days::new(offset as u64)
    } else {
        today() - Days::new(offset.unsigned_abs())
    }
}

pub(super) fn s(date: NaiveDate) -> String {
    validate::format_date(date)
}

#[test]
fn a_change_from_a_date_closes_the_old_line_and_opens_the_new_one_without_gap_or_overlap() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let ceo = f.position(&unit, "CEO", None);
    let cfo = f.position(&unit, "CFO", Some(&ceo));
    let analyst = f.position(&unit, "Analyst", Some(&ceo));

    move_position(
        &f.pool,
        &f.ctx(),
        &analyst.position_id,
        Some(&cfo.position_id),
        day(10),
    )
    .unwrap();

    assert_eq!(
        f.lines(&analyst),
        vec![
            (ceo.position_id.clone(), s(day(0)), Some(s(day(10)))),
            (cfo.position_id.clone(), s(day(10)), None),
        ]
    );
    let before = list_reporting_lines_at(&f.pool, ORG, day(9)).unwrap();
    let after = list_reporting_lines_at(&f.pool, ORG, day(10)).unwrap();
    let parent_of = |lines: &[ReportingLine]| {
        lines
            .iter()
            .find(|l| l.position_id == analyst.position_id)
            .map(|l| l.parent_position_id.clone())
    };
    assert_eq!(parent_of(&before), Some(ceo.position_id.clone()));
    assert_eq!(parent_of(&after), Some(cfo.position_id.clone()));
}

#[test]
fn a_move_keeps_a_later_planned_change_instead_of_overwriting_it() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let ceo = f.position(&unit, "CEO", None);
    let cfo = f.position(&unit, "CFO", Some(&ceo));
    let coo = f.position(&unit, "COO", Some(&ceo));
    let analyst = f.position(&unit, "Analyst", Some(&ceo));

    move_position(
        &f.pool,
        &f.ctx(),
        &analyst.position_id,
        Some(&coo.position_id),
        day(30),
    )
    .unwrap();
    move_position(
        &f.pool,
        &f.ctx(),
        &analyst.position_id,
        Some(&cfo.position_id),
        day(10),
    )
    .unwrap();

    assert_eq!(
        f.lines(&analyst),
        vec![
            (ceo.position_id.clone(), s(day(0)), Some(s(day(10)))),
            (cfo.position_id.clone(), s(day(10)), Some(s(day(30)))),
            (coo.position_id.clone(), s(day(30)), None),
        ]
    );
}

#[test]
fn a_cycle_that_exists_only_in_a_future_interval_is_rejected_and_leaves_no_trace() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let a = f.position(&unit, "A", None);
    let b = f.position(&unit, "B", Some(&a));
    let c = f.position(&unit, "C", Some(&b));

    // Today A is the root; from day 30 it would report to C, closing A -> C -> B -> A.
    let err = move_position(
        &f.pool,
        &f.ctx(),
        &a.position_id,
        Some(&c.position_id),
        day(30),
    )
    .unwrap_err();

    assert_eq!(err, OrgStructureError::ReportingCycle { date: s(day(30)) });
    assert!(
        f.lines(&a).is_empty(),
        "the rejected write must not persist"
    );
}

#[test]
fn a_second_primary_line_on_the_same_day_is_rejected() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let ceo = f.position(&unit, "CEO", None);
    let cfo = f.position(&unit, "CFO", None);
    let analyst = f.position(&unit, "Analyst", Some(&ceo));

    let err = set_reporting_line(
        &f.pool,
        &f.ctx(),
        &NewLine {
            position_id: analyst.position_id.clone(),
            parent_position_id: cfo.position_id.clone(),
            kind: LineKind::Primary,
            priority: 0,
            valid_from: day(5),
            valid_to: None,
        },
    )
    .unwrap_err();

    assert!(
        matches!(err, OrgStructureError::PrimaryLineOverlap { .. }),
        "{err:?}"
    );
    assert_eq!(f.lines(&analyst).len(), 1);

    // A functional line beside the primary one is the matrix case and is fine.
    set_reporting_line(
        &f.pool,
        &f.ctx(),
        &NewLine {
            position_id: analyst.position_id.clone(),
            parent_position_id: cfo.position_id.clone(),
            kind: LineKind::Functional,
            priority: 1,
            valid_from: day(0),
            valid_to: None,
        },
    )
    .unwrap();
}

#[test]
fn a_staff_position_cannot_be_the_manager_in_the_line() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let assistant = f.position_with(&unit, "Assistant", None, true);
    let clerk = f.position(&unit, "Clerk", None);

    let err = move_position(
        &f.pool,
        &f.ctx(),
        &clerk.position_id,
        Some(&assistant.position_id),
        day(0),
    )
    .unwrap_err();

    assert_eq!(
        err,
        OrgStructureError::StaffPositionCannotManage(assistant.position_id)
    );
}

#[test]
fn the_first_assignment_is_primary_and_a_second_primary_one_is_rejected() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let p1 = f.position(&unit, "P1", None);
    let p2 = f.position(&unit, "P2", None);
    let user = add_user(&f.pool, ORG, "anna");

    let first = f.assign_user(&p1, &user, 0.5, None).unwrap().value;
    let second = f.assign_user(&p2, &user, 0.5, None).unwrap().value;
    assert!(
        first.is_primary,
        "a person's only position is the primary one"
    );
    assert!(!second.is_primary);

    let p3 = f.position(&unit, "P3", None);
    let err = f.assign_user(&p3, &user, 0.1, Some(true)).unwrap_err();
    assert!(
        matches!(err, OrgStructureError::PrimaryAssignmentOverlap { .. }),
        "{err:?}"
    );
    assert_eq!(
        list_assignments_at(&f.pool, ORG, day(0)).unwrap().len(),
        2,
        "the rejected assignment must not persist"
    );
}

#[test]
fn shares_above_a_full_time_are_a_warning_not_an_error() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let p1 = f.position(&unit, "P1", None);
    let p2 = f.position(&unit, "P2", None);
    let user = add_user(&f.pool, ORG, "anna");

    let ok = f.assign_user(&p1, &user, 0.6, None).unwrap();
    assert!(ok.warnings.is_empty());
    let over = f.assign_user(&p2, &user, 0.6, None).unwrap();

    assert_eq!(over.warnings.len(), 1);
    match &over.warnings[0] {
        Warning::ShareOverbooked {
            subject,
            from,
            total,
        } => {
            assert_eq!(subject, &Subject::User(user.clone()));
            assert_eq!(*from, day(0));
            assert!((total - 1.2).abs() < 1e-9);
        }
        other => panic!("unexpected warning {other:?}"),
    }
    assert_eq!(list_assignments_at(&f.pool, ORG, day(0)).unwrap().len(), 2);
}

#[test]
fn a_share_outside_zero_to_one_is_an_error() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let p = f.position(&unit, "P", None);
    let user = add_user(&f.pool, ORG, "anna");

    for share in [0.0, -0.5, 1.5] {
        assert!(matches!(
            f.assign_user(&p, &user, share, None).unwrap_err(),
            OrgStructureError::InvalidValue { field: "share", .. }
        ));
    }
}

#[test]
fn a_position_without_an_assignment_is_a_valid_vacancy() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let vacant = f.position(&unit, "Analyst", None);

    let positions = list_positions_at(&f.pool, ORG, day(0)).unwrap();
    assert!(positions
        .iter()
        .any(|p| p.position_id == vacant.position_id));
    assert!(list_assignments_at(&f.pool, ORG, day(0))
        .unwrap()
        .is_empty());
}

#[test]
fn ending_an_assignment_turns_the_position_into_a_vacancy_from_that_day() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let p = f.position(&unit, "P", None);
    let user = add_user(&f.pool, ORG, "anna");
    let held = f.assign_user(&p, &user, 1.0, None).unwrap().value;

    end_assignment(&f.pool, &f.ctx(), &held.id, day(7)).unwrap();

    assert_eq!(list_assignments_at(&f.pool, ORG, day(6)).unwrap().len(), 1);
    assert!(list_assignments_at(&f.pool, ORG, day(7))
        .unwrap()
        .is_empty());
    assert!(list_positions_at(&f.pool, ORG, day(7))
        .unwrap()
        .iter()
        .any(|x| x.position_id == p.position_id));
}

#[test]
fn changing_a_share_from_a_date_keeps_the_earlier_share() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let p = f.position(&unit, "P", None);
    let user = add_user(&f.pool, ORG, "anna");
    let held = f.assign_user(&p, &user, 1.0, None).unwrap().value;

    update_assignment(
        &f.pool,
        &f.ctx(),
        &held.id,
        &AssignmentPatch {
            share: Some(0.5),
            ..Default::default()
        },
        day(10),
    )
    .unwrap();

    let share_at = |offset| list_assignments_at(&f.pool, ORG, day(offset)).unwrap()[0].share;
    assert_eq!(share_at(9), 1.0);
    assert_eq!(share_at(10), 0.5);
}

#[test]
fn a_unit_has_one_head_per_day_and_a_change_is_dated() {
    let f = Fixture::new();
    let unit = f.unit("Sales", None);
    let old_head = f.position(&unit, "Director", None);
    let new_head = f.position(&unit, "New director", None);

    let created = create_unit(
        &f.pool,
        &f.ctx(),
        &NewUnit {
            name: "Ops".into(),
            code: None,
            type_id: None,
            parent_unit_id: None,
            color: None,
            valid_from: day(0),
            valid_to: None,
        },
    )
    .unwrap();
    assert_eq!(
        created.warnings,
        vec![Warning::UnitWithoutHead {
            unit_id: created.value.unit_id.clone(),
            from: day(0)
        }],
        "a unit without a head is a warning"
    );

    set_head(
        &f.pool,
        &f.ctx(),
        &unit.unit_id,
        Some(&old_head.position_id),
        day(0),
    )
    .unwrap();
    set_head(
        &f.pool,
        &f.ctx(),
        &unit.unit_id,
        Some(&new_head.position_id),
        day(20),
    )
    .unwrap();

    let head_at = |offset| {
        let units: Vec<Unit> = list_units_at(&f.pool, ORG, day(offset))
            .unwrap()
            .into_iter()
            .filter(|u| u.unit_id == unit.unit_id)
            .collect();
        assert_eq!(units.len(), 1, "exactly one version of the unit per day");
        units[0].head_position_id.clone()
    };
    assert_eq!(head_at(19), Some(old_head.position_id.clone()));
    assert_eq!(head_at(20), Some(new_head.position_id.clone()));
}

#[test]
fn a_head_of_another_unit_is_refused() {
    let f = Fixture::new();
    let a = f.unit("A", None);
    let b = f.unit("B", None);
    let in_b = f.position(&b, "Boss", None);

    let err = set_head(
        &f.pool,
        &f.ctx(),
        &a.unit_id,
        Some(&in_b.position_id),
        day(0),
    )
    .unwrap_err();

    assert!(
        matches!(err, OrgStructureError::PositionNotInUnit { .. }),
        "{err:?}"
    );
}

#[test]
fn deputy_heads_keep_their_order_and_cannot_include_the_head() {
    let f = Fixture::new();
    let unit = f.unit("Sales", None);
    let head = f.position(&unit, "Director", None);
    let d1 = f.position(&unit, "Deputy 1", Some(&head));
    let d2 = f.position(&unit, "Deputy 2", Some(&head));
    set_head(
        &f.pool,
        &f.ctx(),
        &unit.unit_id,
        Some(&head.position_id),
        day(0),
    )
    .unwrap();

    set_deputy_heads(
        &f.pool,
        &f.ctx(),
        &unit.unit_id,
        &[d2.position_id.clone(), d1.position_id.clone()],
        day(0),
    )
    .unwrap();
    let listed = list_deputy_heads_at(&f.pool, ORG, day(0)).unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|d| d.position_id.clone())
            .collect::<Vec<_>>(),
        vec![d2.position_id.clone(), d1.position_id.clone()]
    );

    let err = set_deputy_heads(
        &f.pool,
        &f.ctx(),
        &unit.unit_id,
        &[head.position_id.clone()],
        day(0),
    )
    .unwrap_err();
    assert_eq!(
        err,
        OrgStructureError::HeadIsDeputy(head.position_id.clone())
    );

    // From day 10 there are no deputies: the earlier list stays for earlier days.
    set_deputy_heads(&f.pool, &f.ctx(), &unit.unit_id, &[], day(10)).unwrap();
    assert_eq!(list_deputy_heads_at(&f.pool, ORG, day(9)).unwrap().len(), 2);
    assert!(list_deputy_heads_at(&f.pool, ORG, day(10))
        .unwrap()
        .is_empty());
}

#[test]
fn moving_a_unit_under_its_own_descendant_is_a_cycle() {
    let f = Fixture::new();
    let root = f.unit("Root", None);
    let child = f.unit("Child", Some(&root.unit_id));

    let err = move_unit(
        &f.pool,
        &f.ctx(),
        &root.unit_id,
        Some(&child.unit_id),
        day(15),
    )
    .unwrap_err();

    assert_eq!(err, OrgStructureError::UnitCycle { date: s(day(15)) });
}

#[test]
fn a_unit_can_be_moved_and_keeps_its_earlier_parent_in_history() {
    let f = Fixture::new();
    let a = f.unit("A", None);
    let b = f.unit("B", None);
    let child = f.unit("Child", Some(&a.unit_id));

    move_unit(&f.pool, &f.ctx(), &child.unit_id, Some(&b.unit_id), day(10)).unwrap();

    let parent_at = |offset| {
        list_units_at(&f.pool, ORG, day(offset))
            .unwrap()
            .into_iter()
            .find(|u| u.unit_id == child.unit_id)
            .unwrap()
            .parent_unit_id
    };
    assert_eq!(parent_at(9), Some(a.unit_id.clone()));
    assert_eq!(parent_at(10), Some(b.unit_id.clone()));
}

#[test]
fn a_liquidated_unit_disappears_from_its_date_and_needs_to_be_empty() {
    let f = Fixture::new();
    let unit = f.unit("Old", None);
    let position = f.position(&unit, "Clerk", None);

    let err = end_unit(&f.pool, &f.ctx(), &unit.unit_id, day(5)).unwrap_err();
    assert!(
        matches!(err, OrgStructureError::UnitNotEmpty { positions: 1, .. }),
        "{err:?}"
    );

    end_position(&f.pool, &f.ctx(), &position.position_id, day(5)).unwrap();
    end_unit(&f.pool, &f.ctx(), &unit.unit_id, day(5)).unwrap();

    let has_unit = |offset| {
        list_units_at(&f.pool, ORG, day(offset))
            .unwrap()
            .iter()
            .any(|u| u.unit_id == unit.unit_id)
    };
    assert!(has_unit(4));
    assert!(!has_unit(5));
}

#[test]
fn ending_a_position_takes_its_assignments_and_refuses_while_it_has_subordinates() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let boss = f.position(&unit, "Boss", None);
    let clerk = f.position(&unit, "Clerk", Some(&boss));
    let user = add_user(&f.pool, ORG, "anna");
    f.assign_user(&clerk, &user, 1.0, None).unwrap();

    let err = end_position(&f.pool, &f.ctx(), &boss.position_id, day(5)).unwrap_err();
    assert!(
        matches!(
            err,
            OrgStructureError::PositionHasSubordinates {
                subordinates: 1,
                ..
            }
        ),
        "{err:?}"
    );

    end_position(&f.pool, &f.ctx(), &clerk.position_id, day(5)).unwrap();
    assert_eq!(list_assignments_at(&f.pool, ORG, day(4)).unwrap().len(), 1);
    assert!(list_assignments_at(&f.pool, ORG, day(5))
        .unwrap()
        .is_empty());
}

#[test]
fn a_change_dated_before_today_needs_confirmation() {
    let f = Fixture::new();
    let backdated_unit = NewUnit {
        name: "Board".into(),
        code: None,
        type_id: None,
        parent_unit_id: None,
        color: None,
        valid_from: day(-10),
        valid_to: None,
    };
    assert!(matches!(
        create_unit(&f.pool, &f.ctx(), &backdated_unit).unwrap_err(),
        OrgStructureError::BackdatedConfirmationRequired { .. }
    ));
    let unit = create_unit(&f.pool, &f.confirmed(), &backdated_unit)
        .unwrap()
        .value;
    let new_position = |name: &str| NewPosition {
        unit_id: unit.unit_id.clone(),
        name: name.into(),
        code: None,
        role_id: None,
        is_manager: None,
        is_staff: false,
        parent_position_id: None,
        valid_from: day(-10),
        valid_to: None,
    };
    let ceo = create_position(&f.pool, &f.confirmed(), &new_position("CEO"))
        .unwrap()
        .value;
    let clerk = create_position(&f.pool, &f.confirmed(), &new_position("Clerk"))
        .unwrap()
        .value;

    let err = move_position(
        &f.pool,
        &f.ctx(),
        &clerk.position_id,
        Some(&ceo.position_id),
        day(-3),
    )
    .unwrap_err();
    assert_eq!(
        err,
        OrgStructureError::BackdatedConfirmationRequired {
            date: s(day(-3)),
            today: s(day(0)),
        }
    );
    assert!(f.lines(&clerk).is_empty());

    move_position(
        &f.pool,
        &f.confirmed(),
        &clerk.position_id,
        Some(&ceo.position_id),
        day(-3),
    )
    .unwrap();
    assert_eq!(f.lines(&clerk).len(), 1);
    let details: String = f
        .pool
        .read()
        .unwrap()
        .query_row(
            "SELECT details FROM audit_log WHERE action = 'org.position.move' ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(details.contains("\"backdated\":true"), "{details}");
}

#[test]
fn references_to_another_organization_are_rejected() {
    let f = Fixture::new();
    let other = org::create_organization(&f.pool, "Other", "other", None, None, None, None)
        .unwrap()
        .org_id;
    let unit = f.unit("Board", None);
    let position = f.position(&unit, "CEO", None);
    let outsider = add_user(&f.pool, &other, "eve");
    let foreign = f.ctx_in(&other);

    // A unit of org A used from org B.
    let err = create_position(
        &f.pool,
        &foreign,
        &NewPosition {
            unit_id: unit.unit_id.clone(),
            name: "Spy".into(),
            code: None,
            role_id: None,
            is_manager: None,
            is_staff: false,
            parent_position_id: None,
            valid_from: day(0),
            valid_to: None,
        },
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            OrgStructureError::CrossOrgReference { entity: "unit", .. }
        ),
        "{err:?}"
    );

    // A position of org A edited from org B.
    let err = update_position(
        &f.pool,
        &foreign,
        &position.position_id,
        &PositionPatch {
            name: Some("Hijacked".into()),
            ..Default::default()
        },
        day(0),
    )
    .unwrap_err();
    assert!(
        matches!(err, OrgStructureError::CrossOrgReference { .. }),
        "{err:?}"
    );

    // A member of org B assigned to a position of org A.
    let err = assign(
        &f.pool,
        &f.ctx(),
        &NewAssignment {
            position_id: position.position_id.clone(),
            subject: Subject::User(outsider),
            kind: AssignmentType::Permanent,
            share: 1.0,
            is_primary: None,
            valid_from: day(0),
            valid_to: None,
        },
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            OrgStructureError::CrossOrgReference { entity: "user", .. }
        ),
        "{err:?}"
    );

    // A unit type of org A on a unit of org B.
    let unit_type = create_unit_type(&f.pool, &f.ctx(), "Division", None, None)
        .unwrap()
        .value;
    let err = create_unit(
        &f.pool,
        &foreign,
        &NewUnit {
            name: "X".into(),
            code: None,
            type_id: Some(unit_type.id),
            parent_unit_id: None,
            color: None,
            valid_from: day(0),
            valid_to: None,
        },
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            OrgStructureError::CrossOrgReference {
                entity: "unit type",
                ..
            }
        ),
        "{err:?}"
    );

    // A role of another organization on a position.
    let role_id: String = f
        .pool
        .read()
        .unwrap()
        .query_row(
            "SELECT id FROM role_catalog WHERE org_id = ?1 LIMIT 1",
            [ORG],
            |r| r.get(0),
        )
        .unwrap();
    let other_unit = create_unit(
        &f.pool,
        &foreign,
        &NewUnit {
            name: "Own".into(),
            code: None,
            type_id: None,
            parent_unit_id: None,
            color: None,
            valid_from: day(0),
            valid_to: None,
        },
    )
    .unwrap()
    .value;
    let err = create_position(
        &f.pool,
        &foreign,
        &NewPosition {
            unit_id: other_unit.unit_id,
            name: "P".into(),
            code: None,
            role_id: Some(role_id),
            is_manager: None,
            is_staff: false,
            parent_position_id: None,
            valid_from: day(0),
            valid_to: None,
        },
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            OrgStructureError::CrossOrgReference { entity: "role", .. }
        ),
        "{err:?}"
    );
}

#[test]
fn every_write_appends_an_audit_entry_and_the_chain_verifies() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let ceo = f.position(&unit, "CEO", None);
    let user = add_user(&f.pool, ORG, "anna");
    f.assign_user(&ceo, &user, 1.0, None).unwrap();
    let external = create_external_person(
        &f.pool,
        &f.ctx(),
        &NewExternalPerson {
            display_name: "Jan Kowalski".into(),
            email: Some("jan@example.test".into()),
            note: None,
        },
    )
    .unwrap()
    .value;
    set_timezone(&f.pool, &f.ctx(), "Europe/London").unwrap();

    let conn = f.pool.read().unwrap();
    let actions: Vec<String> = conn
        .prepare("SELECT action FROM audit_log WHERE action LIKE 'org.%' ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        actions,
        [
            "org.unit.create",
            "org.position.create",
            "org.assignment.create",
            "org.external_person.create",
            "org.settings.timezone_set",
        ]
    );
    let personal: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM audit_log WHERE action LIKE 'org.%' \
             AND (details LIKE '%Kowalski%' OR details LIKE '%jan@example%' OR resource LIKE '%Kowalski%')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(personal, 0, "the audit trail keeps ids, not personal data");
    let user_recorded: String = conn
        .query_row(
            "SELECT user_id FROM audit_log WHERE action = 'org.unit.create'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(user_recorded, f.actor);
    assert!(external.id.len() > 10);
    let report = crate::audit::verify::verify_chain(&conn).unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert!(report.chained_ok >= 5);
}

#[test]
fn a_rejected_write_leaves_neither_rows_captures_nor_audit_behind() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let a = f.position(&unit, "A", None);
    let b = f.position(&unit, "B", Some(&a));
    let audit_before = audit_count(&f.pool);
    let captures_before = capture_count(&f.pool);

    move_position(
        &f.pool,
        &f.ctx(),
        &a.position_id,
        Some(&b.position_id),
        day(3),
    )
    .unwrap_err();

    assert_eq!(audit_count(&f.pool), audit_before);
    assert_eq!(capture_count(&f.pool), captures_before);
}

fn audit_count(pool: &DbPool) -> i64 {
    pool.read()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0))
        .unwrap()
}

pub(super) fn capture_count(pool: &DbPool) -> i64 {
    pool.read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM __tentaflow_core_sync_captures",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

pub(super) fn captures_of(
    pool: &DbPool,
    resource_type: &str,
) -> Vec<crate::sync::core_capture::CoreWriteCapture> {
    let conn = pool.read().unwrap();
    let ids: Vec<String> = conn
        .prepare(
            "SELECT capture_id FROM __tentaflow_core_sync_captures WHERE resource_type = ?1 \
             ORDER BY created_at_ms, rowid",
        )
        .unwrap()
        .query_map([resource_type], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    ids.iter()
        .map(|id| load_core_write_capture(&conn, id).unwrap().unwrap())
        .collect()
}

#[test]
fn every_write_records_a_core_capture_for_each_row_it_touched() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let ceo = f.position(&unit, "CEO", None);
    let clerk = f.position(&unit, "Clerk", Some(&ceo));
    let cfo = f.position(&unit, "CFO", Some(&ceo));

    let before = captures_of(&f.pool, "core.org_reporting_line").len();
    move_position(
        &f.pool,
        &f.ctx(),
        &clerk.position_id,
        Some(&cfo.position_id),
        day(10),
    )
    .unwrap();
    let captures = captures_of(&f.pool, "core.org_reporting_line");

    // The closed row is an update, the opened one an insert.
    let fresh = &captures[before..];
    assert_eq!(fresh.len(), 2);
    assert_eq!(fresh[0].action, SqlWriteAction::Update);
    assert_eq!(fresh[1].action, SqlWriteAction::Insert);
    assert_eq!(fresh[1].org_id, ORG);
    assert_eq!(fresh[1].actor_user_id.as_deref(), Some(f.actor.as_str()));
    assert_eq!(
        fresh[1].changed_fields.get("parent_position_id"),
        Some(&FieldValue::String(cfo.position_id.clone()))
    );
    assert_eq!(
        fresh[1].changed_fields.get("valid_from"),
        Some(&FieldValue::String(s(day(10))))
    );
    assert_eq!(captures_of(&f.pool, "core.org_unit").len(), 1);
    assert_eq!(captures_of(&f.pool, "core.org_position").len(), 3);
}

#[test]
fn a_timezone_change_is_captured_and_shifts_the_organizations_day() {
    let f = Fixture::new();
    assert_eq!(
        get_settings(&f.pool, ORG).unwrap().timezone,
        DEFAULT_TIMEZONE
    );

    set_timezone(&f.pool, &f.ctx(), "Pacific/Auckland").unwrap();
    set_timezone(&f.pool, &f.ctx(), "America/Los_Angeles").unwrap();

    assert_eq!(
        get_settings(&f.pool, ORG).unwrap().timezone,
        "America/Los_Angeles"
    );
    let captures = captures_of(&f.pool, "core.org_structure_settings");
    assert_eq!(captures.len(), 2);
    assert_eq!(captures[0].action, SqlWriteAction::Insert);
    assert_eq!(captures[1].action, SqlWriteAction::Update);
    assert!(matches!(
        set_timezone(&f.pool, &f.ctx(), "Mars/Olympus").unwrap_err(),
        OrgStructureError::InvalidTimezone(_)
    ));
}

#[test]
fn unit_types_are_unique_per_organization_and_protected_while_used() {
    let f = Fixture::new();
    let division = create_unit_type(
        &f.pool,
        &f.ctx(),
        "Division",
        Some("#123456"),
        Some("building"),
    )
    .unwrap()
    .value;
    assert!(matches!(
        create_unit_type(&f.pool, &f.ctx(), "division", None, None).unwrap_err(),
        OrgStructureError::Duplicate { .. }
    ));

    let renamed = update_unit_type(
        &f.pool,
        &f.ctx(),
        &division.id,
        &UnitTypePatch {
            name: Some("Branch".into()),
            color: Some(None),
            ..Default::default()
        },
    )
    .unwrap()
    .value;
    assert_eq!(
        (
            renamed.name.as_str(),
            renamed.color.as_deref(),
            renamed.icon.as_deref()
        ),
        ("Branch", None, Some("building"))
    );

    create_unit(
        &f.pool,
        &f.ctx(),
        &NewUnit {
            name: "North".into(),
            code: Some("N".into()),
            type_id: Some(division.id.clone()),
            parent_unit_id: None,
            color: None,
            valid_from: day(0),
            valid_to: None,
        },
    )
    .unwrap();
    assert_eq!(
        delete_unit_type(&f.pool, &f.ctx(), &division.id).unwrap_err(),
        OrgStructureError::UnitTypeInUse(division.id.clone())
    );

    let spare = create_unit_type(&f.pool, &f.ctx(), "Spare", None, None)
        .unwrap()
        .value;
    delete_unit_type(&f.pool, &f.ctx(), &spare.id).unwrap();
    assert_eq!(list_unit_types(&f.pool, ORG).unwrap().len(), 1);
    let last = captures_of(&f.pool, "core.org_unit_type").pop().unwrap();
    assert_eq!(last.action, SqlWriteAction::Delete);
}

pub(super) fn operation_from(capture: &crate::sync::core_capture::CoreWriteCapture) -> SyncOperation {
    SyncOperation {
        op_id: OperationId::from_hash([9; 32]),
        operation_hash: [9; 32],
        body: SyncOperationBody {
            org_id: capture.org_id.clone(),
            partition_id: PartitionId::new(format!("core/org/{ORG}/org-structure")).unwrap(),
            node_seq: 1,
            addon_id: crate::sync::core_registry::CORE_SYNC_ADDON_ID.to_string(),
            resource_type: capture.resource_type.clone(),
            resource_id: capture.resource_id.clone(),
            table_name: capture.table_name.clone(),
            primary_key: capture.primary_key.clone(),
            action: match capture.action {
                SqlWriteAction::Insert => ActionType::Insert,
                SqlWriteAction::Update => ActionType::Update,
                SqlWriteAction::Delete => ActionType::Delete,
            },
            changed_fields: capture.changed_fields.clone(),
            before_hash: None,
            after_hash: None,
            actor_user_id: String::new(),
            actor_device_id: "peer".to_string(),
            actor_node_id: "peer".to_string(),
            hlc_timestamp: HybridLogicalTimestamp {
                wall_time_ms: capture.hlc.wall_time_ms,
                logical: capture.hlc.logical,
                node_id: "peer".to_string(),
            },
            epoch: BaselineEpoch::default(),
            environment: crate::sync::ledger::NodeEnvironment::default(),
            prev_node_hash: None,
            payload_hash: [0; 32],
            acl_snapshot_hash: [0; 32],
            policy_epoch: 0,
            encryption_info: None,
        },
        signature: Vec::new(),
    }
}

/// Replays every capture of a source node on a second, empty node and expects
/// the same structure at every date. This is the whole replication path:
/// capture shape -> registry -> materializer -> tables.
#[test]
fn a_peer_that_replays_the_captures_ends_up_with_the_same_structure() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let ceo = f.position(&unit, "CEO", None);
    let clerk = f.position(&unit, "Clerk", Some(&ceo));
    let cfo = f.position(&unit, "CFO", Some(&ceo));
    set_head(
        &f.pool,
        &f.ctx(),
        &unit.unit_id,
        Some(&ceo.position_id),
        day(0),
    )
    .unwrap();
    set_deputy_heads(
        &f.pool,
        &f.ctx(),
        &unit.unit_id,
        &[cfo.position_id.clone()],
        day(0),
    )
    .unwrap();
    move_position(
        &f.pool,
        &f.ctx(),
        &clerk.position_id,
        Some(&cfo.position_id),
        day(10),
    )
    .unwrap();
    let user = add_user(&f.pool, ORG, "anna");
    let held = f.assign_user(&clerk, &user, 0.75, None).unwrap().value;
    end_assignment(&f.pool, &f.ctx(), &held.id, day(20)).unwrap();
    let division = create_unit_type(&f.pool, &f.ctx(), "Division", None, Some("building"))
        .unwrap()
        .value;
    let doomed = create_unit_type(&f.pool, &f.ctx(), "Doomed", None, None)
        .unwrap()
        .value;
    delete_unit_type(&f.pool, &f.ctx(), &doomed.id).unwrap();
    create_external_person(
        &f.pool,
        &f.ctx(),
        &NewExternalPerson {
            display_name: "Jan".into(),
            email: None,
            note: Some("contractor".into()),
        },
    )
    .unwrap();
    set_timezone(&f.pool, &f.ctx(), "Europe/London").unwrap();

    let peer_dir = TempDir::new().unwrap();
    let peer = crate::db::init(&peer_dir.path().join("peer.db")).unwrap();
    let all: Vec<_> = {
        let conn = f.pool.read().unwrap();
        let ids: Vec<String> = conn
            .prepare(
                "SELECT capture_id FROM __tentaflow_core_sync_captures \
                 WHERE resource_type LIKE 'core.org_%' AND resource_type NOT IN ('core.org_membership','core.organization') \
                 ORDER BY created_at_ms, rowid",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        ids.iter()
            .map(|id| load_core_write_capture(&conn, id).unwrap().unwrap())
            .collect()
    };
    for capture in &all {
        crate::sync::core_materializer::apply_core_operation(&peer, &operation_from(capture))
            .unwrap_or_else(|e| panic!("applying {}: {e:?}", capture.resource_type));
    }

    for offset in [0, 9, 10, 19, 20, 40] {
        assert_eq!(
            list_units_at(&f.pool, ORG, day(offset)).unwrap(),
            list_units_at(&peer, ORG, day(offset)).unwrap(),
            "units at +{offset}"
        );
        assert_eq!(
            list_positions_at(&f.pool, ORG, day(offset)).unwrap(),
            list_positions_at(&peer, ORG, day(offset)).unwrap(),
            "positions at +{offset}"
        );
        assert_eq!(
            list_reporting_lines_at(&f.pool, ORG, day(offset)).unwrap(),
            list_reporting_lines_at(&peer, ORG, day(offset)).unwrap(),
            "lines at +{offset}"
        );
        assert_eq!(
            list_assignments_at(&f.pool, ORG, day(offset)).unwrap(),
            list_assignments_at(&peer, ORG, day(offset)).unwrap(),
            "assignments at +{offset}"
        );
        assert_eq!(
            list_deputy_heads_at(&f.pool, ORG, day(offset)).unwrap(),
            list_deputy_heads_at(&peer, ORG, day(offset)).unwrap(),
            "deputies at +{offset}"
        );
    }
    assert_eq!(list_unit_types(&peer, ORG).unwrap(), vec![division]);
    assert_eq!(
        list_external_persons(&f.pool, ORG).unwrap(),
        list_external_persons(&peer, ORG).unwrap()
    );
    assert_eq!(get_settings(&peer, ORG).unwrap().timezone, "Europe/London");
}

/// A node that joins later gets the structure from the baseline reseed rather
/// than from the operations it never saw.
#[test]
fn the_baseline_reseed_captures_every_existing_row() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let ceo = f.position(&unit, "CEO", None);
    f.position(&unit, "Clerk", Some(&ceo));
    set_timezone(&f.pool, &f.ctx(), "Europe/London").unwrap();
    f.pool
        .write()
        .unwrap()
        .execute("DELETE FROM __tentaflow_core_sync_captures", [])
        .unwrap();

    crate::db::repository::reseed_core_state_from_current_rows(&f.pool).unwrap();

    let by_type = |t: &str| captures_of(&f.pool, t).len();
    assert_eq!(by_type("core.org_unit"), 1);
    assert_eq!(by_type("core.org_position"), 2);
    assert_eq!(by_type("core.org_reporting_line"), 1);
    assert_eq!(by_type("core.org_structure_settings"), 1);
    let mut fields: BTreeMap<String, FieldValue> =
        captures_of(&f.pool, "core.org_structure_settings")
            .remove(0)
            .changed_fields;
    assert_eq!(
        fields.remove("timezone"),
        Some(FieldValue::String("Europe/London".into()))
    );
}

// ---------------------------------------------------------------------------
// Review round: interval containment, head/deputy collisions, cleanup on
// removal, primary assignment, dated renames, ended-position report.
// ---------------------------------------------------------------------------

fn new_position(unit: &Unit, name: &str, valid_to: Option<NaiveDate>) -> NewPosition {
    NewPosition {
        unit_id: unit.unit_id.clone(),
        name: name.into(),
        code: None,
        role_id: None,
        is_manager: None,
        is_staff: false,
        parent_position_id: None,
        valid_from: day(0),
        valid_to,
    }
}

impl Fixture {
    fn bounded_position(&self, unit: &Unit, name: &str, end: NaiveDate) -> Position {
        create_position(
            &self.pool,
            &self.ctx(),
            &new_position(unit, name, Some(end)),
        )
        .unwrap()
        .value
    }
}

#[test]
fn a_line_cannot_outlive_its_manager() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let temp_boss = f.bounded_position(&unit, "Interim boss", day(20));
    let clerk = f.position(&unit, "Clerk", None);

    // Created under a manager who leaves earlier.
    let err = create_position(
        &f.pool,
        &f.ctx(),
        &NewPosition {
            parent_position_id: Some(temp_boss.position_id.clone()),
            ..new_position(&unit, "New clerk", None)
        },
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            OrgStructureError::OutsideValidity {
                entity: "position",
                ..
            }
        ),
        "{err:?}"
    );

    // Moved under him, open-ended.
    let err = move_position(
        &f.pool,
        &f.ctx(),
        &clerk.position_id,
        Some(&temp_boss.position_id),
        day(5),
    )
    .unwrap_err();
    assert!(
        matches!(err, OrgStructureError::OutsideValidity { .. }),
        "{err:?}"
    );
    assert!(
        f.lines(&clerk).is_empty(),
        "the rejected move leaves nothing behind"
    );

    // A functional line is held to the same rule.
    let err = set_reporting_line(
        &f.pool,
        &f.ctx(),
        &NewLine {
            position_id: clerk.position_id.clone(),
            parent_position_id: temp_boss.position_id.clone(),
            kind: LineKind::Functional,
            priority: 0,
            valid_from: day(0),
            valid_to: None,
        },
    )
    .unwrap_err();
    assert!(
        matches!(err, OrgStructureError::OutsideValidity { .. }),
        "{err:?}"
    );

    // Bounded by his end date it is fine.
    set_reporting_line(
        &f.pool,
        &f.ctx(),
        &NewLine {
            position_id: clerk.position_id.clone(),
            parent_position_id: temp_boss.position_id.clone(),
            kind: LineKind::Functional,
            priority: 0,
            valid_from: day(0),
            valid_to: Some(day(20)),
        },
    )
    .unwrap();
}

#[test]
fn a_moved_line_is_clamped_to_the_end_of_the_position_it_belongs_to() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let boss = f.position(&unit, "Boss", None);
    let contractor = f.bounded_position(&unit, "Contractor", day(30));

    move_position(
        &f.pool,
        &f.ctx(),
        &contractor.position_id,
        Some(&boss.position_id),
        day(5),
    )
    .unwrap();

    assert_eq!(
        f.lines(&contractor),
        vec![(boss.position_id.clone(), s(day(5)), Some(s(day(30))))]
    );
}

#[test]
fn a_scheduled_head_change_cannot_collide_with_a_deputy_in_either_order() {
    let f = Fixture::new();
    let unit = f.unit("Sales", None);
    let head = f.position(&unit, "Director", None);
    let x = f.position(&unit, "X", Some(&head));
    set_head(
        &f.pool,
        &f.ctx(),
        &unit.unit_id,
        Some(&head.position_id),
        day(0),
    )
    .unwrap();
    set_head(
        &f.pool,
        &f.ctx(),
        &unit.unit_id,
        Some(&x.position_id),
        day(30),
    )
    .unwrap();

    let err = set_deputy_heads(
        &f.pool,
        &f.ctx(),
        &unit.unit_id,
        &[x.position_id.clone()],
        day(5),
    )
    .unwrap_err();
    assert_eq!(err, OrgStructureError::HeadIsDeputy(x.position_id.clone()));

    // Symmetric: X a deputy first, then made head from day 40 (deputy row is open-ended).
    let y = f.position(&unit, "Y", Some(&head));
    set_deputy_heads(
        &f.pool,
        &f.ctx(),
        &unit.unit_id,
        &[y.position_id.clone()],
        day(0),
    )
    .unwrap();
    let err = set_head(
        &f.pool,
        &f.ctx(),
        &unit.unit_id,
        Some(&y.position_id),
        day(40),
    )
    .unwrap_err();
    assert_eq!(err, OrgStructureError::HeadIsDeputy(y.position_id.clone()));

    // Once the deputy list is replaced before that day the head change is fine.
    set_deputy_heads(&f.pool, &f.ctx(), &unit.unit_id, &[], day(35)).unwrap();
    set_head(
        &f.pool,
        &f.ctx(),
        &unit.unit_id,
        Some(&y.position_id),
        day(40),
    )
    .unwrap();
}

#[test]
fn a_deputy_list_change_keeps_the_next_planned_list() {
    let f = Fixture::new();
    let unit = f.unit("Sales", None);
    let head = f.position(&unit, "Director", None);
    let d1 = f.position(&unit, "Deputy 1", Some(&head));
    let d2 = f.position(&unit, "Deputy 2", Some(&head));
    set_head(
        &f.pool,
        &f.ctx(),
        &unit.unit_id,
        Some(&head.position_id),
        day(0),
    )
    .unwrap();
    set_deputy_heads(
        &f.pool,
        &f.ctx(),
        &unit.unit_id,
        &[d1.position_id.clone()],
        day(0),
    )
    .unwrap();
    set_deputy_heads(
        &f.pool,
        &f.ctx(),
        &unit.unit_id,
        &[d2.position_id.clone()],
        day(20),
    )
    .unwrap();

    set_deputy_heads(&f.pool, &f.ctx(), &unit.unit_id, &[], day(10)).unwrap();

    let at = |offset| {
        list_deputy_heads_at(&f.pool, ORG, day(offset))
            .unwrap()
            .into_iter()
            .map(|d| d.position_id)
            .collect::<Vec<_>>()
    };
    assert_eq!(at(5), vec![d1.position_id.clone()]);
    assert!(at(15).is_empty());
    assert_eq!(
        at(25),
        vec![d2.position_id.clone()],
        "the later planned list survives"
    );
}

#[test]
fn a_position_cannot_head_two_units_at_once() {
    let f = Fixture::new();
    let a = f.unit("A", None);
    let b = f.unit("B", None);
    let boss = f.position(&a, "Boss", None);
    let other = f.position(&b, "Other", None);
    set_head(
        &f.pool,
        &f.ctx(),
        &b.unit_id,
        Some(&other.position_id),
        day(0),
    )
    .unwrap();
    // Replicated data that names A's boss as B's head.
    f.pool
        .write()
        .unwrap()
        .execute(
            "UPDATE org_units SET head_position_id = ?1 WHERE unit_id = ?2",
            [&boss.position_id, &b.unit_id],
        )
        .unwrap();

    let err = set_head(
        &f.pool,
        &f.ctx(),
        &a.unit_id,
        Some(&boss.position_id),
        day(0),
    )
    .unwrap_err();

    assert_eq!(
        err,
        OrgStructureError::PositionHeadsAnotherUnit {
            position_id: boss.position_id.clone()
        }
    );
}

#[test]
fn renames_are_dated_and_earlier_days_keep_the_old_name() {
    let f = Fixture::new();
    let unit = f.unit("Sales", None);
    let position = f.position(&unit, "Clerk", None);

    let err = update_unit(
        &f.pool,
        &f.ctx(),
        &unit.unit_id,
        &UnitPatch {
            name: Some("Revenue".into()),
            ..Default::default()
        },
        day(-1),
    )
    .unwrap_err();
    assert!(matches!(
        err,
        OrgStructureError::BackdatedConfirmationRequired { .. }
    ));

    update_unit(
        &f.pool,
        &f.ctx(),
        &unit.unit_id,
        &UnitPatch {
            name: Some("Revenue".into()),
            code: Some(Some("REV".into())),
            ..Default::default()
        },
        day(10),
    )
    .unwrap();
    update_position(
        &f.pool,
        &f.ctx(),
        &position.position_id,
        &PositionPatch {
            name: Some("Senior clerk".into()),
            is_manager: Some(Some(true)),
            ..Default::default()
        },
        day(10),
    )
    .unwrap();

    let unit_at = |offset| {
        list_units_at(&f.pool, ORG, day(offset))
            .unwrap()
            .into_iter()
            .find(|u| u.unit_id == unit.unit_id)
            .unwrap()
    };
    assert_eq!(unit_at(9).name, "Sales");
    assert_eq!(unit_at(9).code, None);
    assert_eq!(unit_at(10).name, "Revenue");
    assert_eq!(unit_at(10).code.as_deref(), Some("REV"));
    let position_at = |offset| {
        list_positions_at(&f.pool, ORG, day(offset))
            .unwrap()
            .into_iter()
            .find(|p| p.position_id == position.position_id)
            .unwrap()
    };
    assert_eq!(position_at(9).name, "Clerk");
    assert_eq!(position_at(9).is_manager, None);
    assert_eq!(position_at(10).name, "Senior clerk");
    assert_eq!(position_at(10).is_manager, Some(true));
    assert_eq!(position_at(10).position_id, position_at(9).position_id);
}

#[test]
fn ending_the_primary_assignment_promotes_the_only_remaining_one_or_warns() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let p1 = f.position(&unit, "P1", None);
    let p2 = f.position(&unit, "P2", None);
    let p3 = f.position(&unit, "P3", None);
    let user = add_user(&f.pool, ORG, "anna");
    let primary = f.assign_user(&p1, &user, 0.5, None).unwrap().value;
    f.assign_user(&p2, &user, 0.5, None).unwrap();

    let ended = end_assignment(&f.pool, &f.ctx(), &primary.id, day(5)).unwrap();

    assert!(ended.warnings.is_empty());
    let primary_at = |offset| {
        list_assignments_at(&f.pool, ORG, day(offset))
            .unwrap()
            .into_iter()
            .map(|a| (a.position_id, a.is_primary))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        primary_at(4),
        vec![
            (p1.position_id.clone(), true),
            (p2.position_id.clone(), false)
        ]
    );
    assert_eq!(primary_at(5), vec![(p2.position_id.clone(), true)]);

    // With two left, the choice is the administrator's: a warning, not a guess.
    let user2 = add_user(&f.pool, ORG, "jan");
    let first = f.assign_user(&p1, &user2, 0.3, None).unwrap().value;
    f.assign_user(&p2, &user2, 0.3, None).unwrap();
    f.assign_user(&p3, &user2, 0.3, None).unwrap();
    let ended = end_assignment(&f.pool, &f.ctx(), &first.id, day(7)).unwrap();
    assert_eq!(
        ended.warnings,
        vec![Warning::PersonWithoutPrimary {
            subject: Subject::User(user2.clone()),
            from: day(7)
        }]
    );
}

#[test]
fn ending_a_position_reports_and_audits_everything_it_ended() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let boss = f.position(&unit, "Boss", None);
    let clerk = f.position(&unit, "Clerk", Some(&boss));
    let user = add_user(&f.pool, ORG, "anna");
    let held = f.assign_user(&clerk, &user, 1.0, None).unwrap().value;
    set_deputy_heads(
        &f.pool,
        &f.ctx(),
        &unit.unit_id,
        &[clerk.position_id.clone()],
        day(0),
    )
    .unwrap();

    let ended = end_position(&f.pool, &f.ctx(), &clerk.position_id, day(5))
        .unwrap()
        .value;

    assert_eq!(ended.assignments, vec![held.id.clone()]);
    assert_eq!(ended.reporting_lines.len(), 1);
    assert_eq!(ended.deputy_heads.len(), 1);
    let details: String = f
        .pool
        .read()
        .unwrap()
        .query_row(
            "SELECT details FROM audit_log WHERE action = 'org.position.end'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(details.contains(&held.id), "{details}");
    assert!(details.contains(&ended.reporting_lines[0]), "{details}");
}

fn any_role(pool: &DbPool, org_id: &str, except: Option<&str>) -> String {
    pool.read()
        .unwrap()
        .query_row(
            "SELECT id FROM role_catalog WHERE org_id = ?1 AND is_active = 1 AND id <> ?2 LIMIT 1",
            [org_id, except.unwrap_or("")],
            |r| r.get(0),
        )
        .unwrap()
}

fn member_role(pool: &DbPool) -> String {
    pool.read()
        .unwrap()
        .query_row("SELECT role_id FROM roles LIMIT 1", [], |r| r.get(0))
        .unwrap()
}

/// A unit, a position and a user who has held it since day -5 in `org_id`.
fn long_standing(f: &Fixture, org_id: &str, user: &str) -> (Position, Assignment) {
    let ctx = WriteCtx {
        confirm_backdated: true,
        ..f.ctx_in(org_id)
    };
    let unit = create_unit(
        &f.pool,
        &ctx,
        &NewUnit {
            name: "Board".into(),
            code: None,
            type_id: None,
            parent_unit_id: None,
            color: None,
            valid_from: day(-5),
            valid_to: None,
        },
    )
    .unwrap()
    .value;
    let position = create_position(
        &f.pool,
        &ctx,
        &NewPosition {
            valid_from: day(-5),
            ..new_position(&unit, "Clerk", None)
        },
    )
    .unwrap()
    .value;
    let assignment = assign(
        &f.pool,
        &ctx,
        &NewAssignment {
            position_id: position.position_id.clone(),
            subject: Subject::User(user.to_string()),
            kind: AssignmentType::Permanent,
            share: 1.0,
            is_primary: None,
            valid_from: day(-5),
            valid_to: None,
        },
    )
    .unwrap()
    .value;
    (position, assignment)
}

fn assignment_counts(pool: &DbPool, org_id: &str) -> (usize, usize) {
    (
        list_assignments_at(pool, org_id, day(-1)).unwrap().len(),
        list_assignments_at(pool, org_id, day(0)).unwrap().len(),
    )
}

#[test]
fn leaving_the_organization_ends_the_assignments_and_keeps_their_history() {
    let f = Fixture::new();
    let user = add_user(&f.pool, ORG, "anna");
    let (position, _) = long_standing(&f, ORG, &user);
    // Planned to start later: never held a day, so it is removed rather than "ended".
    assign(
        &f.pool,
        &f.ctx(),
        &NewAssignment {
            position_id: f
                .position(&f.unit("Other", None), "Later", None)
                .position_id,
            subject: Subject::User(user.clone()),
            kind: AssignmentType::Acting,
            share: 0.2,
            is_primary: Some(false),
            valid_from: day(3),
            valid_to: None,
        },
    )
    .unwrap();

    assert!(org::remove_membership(&f.pool, ORG, &user).unwrap());

    assert_eq!(
        assignment_counts(&f.pool, ORG),
        (1, 0),
        "held yesterday, not today"
    );
    assert!(list_assignments_at(&f.pool, ORG, day(3))
        .unwrap()
        .is_empty());
    assert!(list_positions_at(&f.pool, ORG, day(0))
        .unwrap()
        .iter()
        .any(|p| p.position_id == position.position_id));
    let conn = f.pool.read().unwrap();
    let audited: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM audit_log WHERE action = 'org.assignment.end_for_user'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(audited, 1);
    drop(conn);
    assert!(captures_of(&f.pool, "core.org_assignment")
        .iter()
        .any(|c| c.action == SqlWriteAction::Update && c.actor_user_id.is_none()));
    assert!(crate::audit::verify::verify_chain(&f.pool.read().unwrap())
        .unwrap()
        .is_clean());
}

#[test]
fn deleting_a_user_ends_their_assignments_in_every_organization() {
    let f = Fixture::new();
    let other = org::create_organization(&f.pool, "Other", "other", None, None, None, None)
        .unwrap()
        .org_id;
    let user = add_user(&f.pool, ORG, "anna");
    org::add_membership(&f.pool, &other, &user, &member_role(&f.pool), "test").unwrap();
    long_standing(&f, ORG, &user);
    long_standing(&f, &other, &user);

    crate::db::repository::delete_user_account(&f.pool, &user, Some(&f.actor)).unwrap();

    assert_eq!(assignment_counts(&f.pool, ORG), (1, 0));
    assert_eq!(assignment_counts(&f.pool, &other), (1, 0));
}

#[test]
fn a_role_used_by_a_position_cannot_be_deactivated() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let role = any_role(&f.pool, ORG, None);
    let position = create_position(
        &f.pool,
        &f.ctx(),
        &NewPosition {
            role_id: Some(role.clone()),
            ..new_position(&unit, "Clerk", None)
        },
    )
    .unwrap()
    .value;

    let err =
        crate::services::role_catalog::deactivate_role(&f.pool, &f.actor, ORG, &role).unwrap_err();
    assert_eq!(
        err,
        crate::services::role_catalog::RoleCatalogError::InUse {
            id: role.clone(),
            positions: 1
        }
    );

    // With the reference gone the role is free again.
    update_position(
        &f.pool,
        &f.ctx(),
        &position.position_id,
        &PositionPatch {
            role_id: Some(None),
            ..Default::default()
        },
        day(0),
    )
    .unwrap();
    crate::services::role_catalog::deactivate_role(&f.pool, &f.actor, ORG, &role).unwrap();
}

#[test]
fn deleting_an_organization_deletes_its_structure_and_only_its_own() {
    let f = Fixture::new();
    let other = org::create_organization(&f.pool, "Other", "other", None, None, None, None)
        .unwrap()
        .org_id;
    let mine = f.unit("Mine", None);
    let theirs = create_unit(
        &f.pool,
        &f.ctx_in(&other),
        &NewUnit {
            name: "Theirs".into(),
            code: None,
            type_id: None,
            parent_unit_id: None,
            color: None,
            valid_from: day(0),
            valid_to: None,
        },
    )
    .unwrap()
    .value;
    create_unit_type(&f.pool, &f.ctx_in(&other), "Division", None, None).unwrap();
    set_timezone(&f.pool, &f.ctx_in(&other), "Europe/London").unwrap();

    assert!(org::delete_organization(&f.pool, &other).unwrap());

    assert!(list_units_at(&f.pool, &other, day(0)).unwrap().is_empty());
    assert!(list_unit_types(&f.pool, &other).unwrap().is_empty());
    assert_eq!(
        get_settings(&f.pool, &other).unwrap().timezone,
        DEFAULT_TIMEZONE
    );
    assert_eq!(list_units_at(&f.pool, ORG, day(0)).unwrap().len(), 1);
    assert_eq!(
        list_units_at(&f.pool, ORG, day(0)).unwrap()[0].unit_id,
        mine.unit_id
    );
    let tombstones = captures_of(&f.pool, "core.org_unit")
        .into_iter()
        .filter(|c| c.action == SqlWriteAction::Delete && c.resource_id == theirs.id)
        .count();
    assert_eq!(tombstones, 1);
}

/// Every write that takes an id must refuse an id of another organization, and
/// say so with the typed error rather than "not found".
#[test]
fn every_write_refuses_ids_of_another_organization() {
    let f = Fixture::new();
    let other = org::create_organization(&f.pool, "Other", "other", None, None, None, None)
        .unwrap()
        .org_id;
    let foreign = f.ctx_in(&other);
    let unit_type = create_unit_type(&f.pool, &f.ctx(), "Division", None, None)
        .unwrap()
        .value;
    let unit = f.unit("Board", None);
    let boss = f.position(&unit, "Boss", None);
    let clerk = f.position(&unit, "Clerk", Some(&boss));
    let user = add_user(&f.pool, ORG, "anna");
    let held = f.assign_user(&clerk, &user, 1.0, None).unwrap().value;
    let external = create_external_person(
        &f.pool,
        &f.ctx(),
        &NewExternalPerson {
            display_name: "Jan".into(),
            email: None,
            note: None,
        },
    )
    .unwrap()
    .value;
    let pool = &f.pool;
    let ctx = &foreign;

    type Attempt<'a> = Box<dyn Fn() -> Result<()> + 'a>;
    let attempts: Vec<(&str, Attempt<'_>)> = vec![
        (
            "update_unit_type",
            Box::new(|| {
                update_unit_type(
                    pool,
                    ctx,
                    &unit_type.id,
                    &UnitTypePatch {
                        name: Some("X".into()),
                        ..Default::default()
                    },
                )
                .map(drop)
            }),
        ),
        (
            "delete_unit_type",
            Box::new(|| delete_unit_type(pool, ctx, &unit_type.id).map(drop)),
        ),
        (
            "create_unit(type)",
            Box::new(|| {
                create_unit(
                    pool,
                    ctx,
                    &NewUnit {
                        name: "X".into(),
                        code: None,
                        type_id: Some(unit_type.id.clone()),
                        parent_unit_id: None,
                        color: None,
                        valid_from: day(0),
                        valid_to: None,
                    },
                )
                .map(drop)
            }),
        ),
        (
            "create_unit(parent)",
            Box::new(|| {
                create_unit(
                    pool,
                    ctx,
                    &NewUnit {
                        name: "X".into(),
                        code: None,
                        type_id: None,
                        parent_unit_id: Some(unit.unit_id.clone()),
                        color: None,
                        valid_from: day(0),
                        valid_to: None,
                    },
                )
                .map(drop)
            }),
        ),
        (
            "update_unit",
            Box::new(|| {
                update_unit(
                    pool,
                    ctx,
                    &unit.unit_id,
                    &UnitPatch {
                        name: Some("X".into()),
                        ..Default::default()
                    },
                    day(0),
                )
                .map(drop)
            }),
        ),
        (
            "move_unit",
            Box::new(|| move_unit(pool, ctx, &unit.unit_id, None, day(0)).map(drop)),
        ),
        (
            "end_unit",
            Box::new(|| end_unit(pool, ctx, &unit.unit_id, day(5)).map(drop)),
        ),
        (
            "set_head",
            Box::new(|| set_head(pool, ctx, &unit.unit_id, None, day(0)).map(drop)),
        ),
        (
            "set_deputy_heads",
            Box::new(|| set_deputy_heads(pool, ctx, &unit.unit_id, &[], day(0)).map(drop)),
        ),
        (
            "create_position",
            Box::new(|| create_position(pool, ctx, &new_position(&unit, "X", None)).map(drop)),
        ),
        (
            "update_position",
            Box::new(|| {
                update_position(
                    pool,
                    ctx,
                    &clerk.position_id,
                    &PositionPatch {
                        name: Some("X".into()),
                        ..Default::default()
                    },
                    day(0),
                )
                .map(drop)
            }),
        ),
        (
            "move_position",
            Box::new(|| move_position(pool, ctx, &clerk.position_id, None, day(0)).map(drop)),
        ),
        (
            "set_reporting_line",
            Box::new(|| {
                set_reporting_line(
                    pool,
                    ctx,
                    &NewLine {
                        position_id: clerk.position_id.clone(),
                        parent_position_id: boss.position_id.clone(),
                        kind: LineKind::Functional,
                        priority: 0,
                        valid_from: day(0),
                        valid_to: None,
                    },
                )
                .map(drop)
            }),
        ),
        (
            "end_position",
            Box::new(|| end_position(pool, ctx, &clerk.position_id, day(5)).map(drop)),
        ),
        (
            "assign(position)",
            Box::new(|| {
                assign(
                    pool,
                    ctx,
                    &NewAssignment {
                        position_id: clerk.position_id.clone(),
                        subject: Subject::External(external.id.clone()),
                        kind: AssignmentType::Contractor,
                        share: 1.0,
                        is_primary: None,
                        valid_from: day(0),
                        valid_to: None,
                    },
                )
                .map(drop)
            }),
        ),
        (
            "update_assignment",
            Box::new(|| {
                update_assignment(
                    pool,
                    ctx,
                    &held.id,
                    &AssignmentPatch {
                        share: Some(0.5),
                        ..Default::default()
                    },
                    day(0),
                )
                .map(drop)
            }),
        ),
        (
            "end_assignment",
            Box::new(|| end_assignment(pool, ctx, &held.id, day(5)).map(drop)),
        ),
    ];
    for (name, attempt) in attempts {
        let err = attempt().expect_err(name);
        assert!(
            matches!(err, OrgStructureError::CrossOrgReference { .. }),
            "{name}: {err:?}"
        );
    }
    // The external person is a reference too, from a position of the caller's own org.
    let own_unit = create_unit(
        pool,
        ctx,
        &NewUnit {
            name: "Own".into(),
            code: None,
            type_id: None,
            parent_unit_id: None,
            color: None,
            valid_from: day(0),
            valid_to: None,
        },
    )
    .unwrap()
    .value;
    let own_position = create_position(pool, ctx, &new_position(&own_unit, "Own", None))
        .unwrap()
        .value;
    let err = assign(
        pool,
        ctx,
        &NewAssignment {
            position_id: own_position.position_id,
            subject: Subject::External(external.id.clone()),
            kind: AssignmentType::Contractor,
            share: 1.0,
            is_primary: None,
            valid_from: day(0),
            valid_to: None,
        },
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            OrgStructureError::CrossOrgReference {
                entity: "external person",
                ..
            }
        ),
        "{err:?}"
    );
    // Nothing of org A changed.
    assert_eq!(
        list_positions_at(pool, ORG, day(0))
            .unwrap()
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Boss", "Clerk"]
    );
}

#[test]
fn a_deactivated_role_cannot_be_given_to_a_position() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let role = any_role(&f.pool, ORG, None);
    crate::services::role_catalog::deactivate_role(&f.pool, &f.actor, ORG, &role).unwrap();

    let err = create_position(
        &f.pool,
        &f.ctx(),
        &NewPosition {
            role_id: Some(role.clone()),
            ..new_position(&unit, "Clerk", None)
        },
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            OrgStructureError::InvalidValue {
                field: "role_id",
                ..
            }
        ),
        "{err:?}"
    );

    let position = f.position(&unit, "Clerk", None);
    let err = update_position(
        &f.pool,
        &f.ctx(),
        &position.position_id,
        &PositionPatch {
            role_id: Some(Some(role)),
            ..Default::default()
        },
        day(0),
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            OrgStructureError::InvalidValue {
                field: "role_id",
                ..
            }
        ),
        "{err:?}"
    );
}

#[test]
fn a_position_ending_today_in_the_org_timezone_no_longer_blocks_its_role() {
    let f = Fixture::new();
    set_timezone(&f.pool, &f.ctx(), "Pacific/Kiritimati").unwrap();
    let org_day = org_today(&f.pool, ORG).unwrap();
    let confirmed = f.confirmed();
    let unit = create_unit(
        &f.pool,
        &confirmed,
        &NewUnit {
            name: "Board".into(),
            code: None,
            type_id: None,
            parent_unit_id: None,
            color: None,
            valid_from: org_day - Days::new(5),
            valid_to: None,
        },
    )
    .unwrap()
    .value;
    let role = any_role(&f.pool, ORG, None);
    create_position(
        &f.pool,
        &confirmed,
        &NewPosition {
            role_id: Some(role.clone()),
            valid_from: org_day - Days::new(5),
            valid_to: Some(org_day),
            ..new_position(&unit, "Interim", None)
        },
    )
    .unwrap();

    crate::services::role_catalog::deactivate_role(&f.pool, &f.actor, ORG, &role).unwrap();
}

#[test]
fn a_consistent_structure_has_an_empty_integrity_report() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let boss = f.position(&unit, "Boss", None);
    let clerk = f.position(&unit, "Clerk", Some(&boss));
    set_head(
        &f.pool,
        &f.ctx(),
        &unit.unit_id,
        Some(&boss.position_id),
        day(0),
    )
    .unwrap();
    let user = add_user(&f.pool, ORG, "anna");
    f.assign_user(&clerk, &user, 1.0, None).unwrap();

    assert!(integrity_report(&f.pool, ORG, None).unwrap().is_empty());
    assert!(integrity_report(&f.pool, ORG, Some(day(3)))
        .unwrap()
        .is_empty());
}

fn raw(pool: &DbPool, sql: &str, params: &[&str]) {
    pool.write()
        .unwrap()
        .execute(sql, rusqlite::params_from_iter(params.iter()))
        .unwrap();
}

/// Rows of the kind a peer's offline edit could deliver: each one passed the
/// peer's own checks, and only the merge is inconsistent.
#[test]
fn the_integrity_report_finds_what_concurrent_offline_edits_can_break() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let other_unit = f.unit("Other", None);
    let boss = f.position(&unit, "Boss", None);
    let clerk = f.position(&unit, "Clerk", Some(&boss));
    let outsider = f.position(&other_unit, "Outsider", None);
    set_head(
        &f.pool,
        &f.ctx(),
        &unit.unit_id,
        Some(&boss.position_id),
        day(0),
    )
    .unwrap();
    set_deputy_heads(
        &f.pool,
        &f.ctx(),
        &unit.unit_id,
        &[clerk.position_id.clone()],
        day(0),
    )
    .unwrap();
    let user = add_user(&f.pool, ORG, "anna");
    f.assign_user(&clerk, &user, 1.0, None).unwrap();
    let from = s(day(0));

    // Second primary line for the clerk, to somebody else.
    raw(&f.pool, "INSERT INTO org_reporting_lines (id, org_id, position_id, parent_position_id, kind, priority, valid_from) VALUES ('l2', ?1, ?2, ?3, 'primary', 0, ?4)", &[ORG, &clerk.position_id, &outsider.position_id, &from]);
    // The boss is made a root's child of the clerk: loop boss -> clerk -> boss.
    raw(&f.pool, "INSERT INTO org_reporting_lines (id, org_id, position_id, parent_position_id, kind, priority, valid_from) VALUES ('l3', ?1, ?2, ?3, 'primary', 0, ?4)", &[ORG, &boss.position_id, &clerk.position_id, &from]);
    // The clerk also becomes the head of the other unit, and of its own (deputy is head).
    raw(
        &f.pool,
        "UPDATE org_units SET head_position_id = ?1 WHERE unit_id = ?2",
        &[&clerk.position_id, &other_unit.unit_id],
    );
    raw(&f.pool, "INSERT INTO org_units (id, org_id, unit_id, name, head_position_id, valid_from) VALUES ('u2', ?1, ?2, 'Board', ?3, ?4)", &[ORG, &unit.unit_id, &clerk.position_id, &from]);
    // A second primary assignment for the same person, to a position that does not exist.
    raw(&f.pool, "INSERT INTO org_assignments (id, org_id, position_id, user_id, type, share, is_primary, valid_from) VALUES ('a2', ?1, ?2, ?3, 'permanent', 0.5, 1, ?4)", &[ORG, &boss.position_id, &user, &from]);
    raw(&f.pool, "INSERT INTO org_assignments (id, org_id, position_id, user_id, type, share, is_primary, valid_from) VALUES ('a3', ?1, 'ghost', 'stranger', 'permanent', 0.5, 0, ?2)", &[ORG, &from]);

    let report = integrity_report(&f.pool, ORG, Some(day(1))).unwrap();
    let has = |pred: &dyn Fn(&Violation) -> bool| report.iter().any(pred);

    assert!(
        has(
            &|v| matches!(v, Violation::PrimaryLinesOverlap { position_id, .. } if *position_id == clerk.position_id)
        ),
        "{report:#?}"
    );
    assert!(has(&|v| matches!(v, Violation::PositionCycle { .. })));
    assert_eq!(
        report
            .iter()
            .filter(|v| matches!(v, Violation::PositionCycle { .. }))
            .count(),
        1,
        "a loop is reported once"
    );
    assert!(has(
        &|v| matches!(v, Violation::VersionsOverlap { entity, id, .. } if entity == "unit" && *id == unit.unit_id)
    ));
    assert!(has(
        &|v| matches!(v, Violation::PositionHeadsMultipleUnits { position_id, .. } if *position_id == clerk.position_id)
    ));
    assert!(has(
        &|v| matches!(v, Violation::DeputyIsHead { position_id, .. } if *position_id == clerk.position_id)
    ));
    assert!(has(
        &|v| matches!(v, Violation::PrimaryAssignmentsOverlap { subject, .. } if *subject == Subject::User(user.clone()))
    ));
    assert!(has(
        &|v| matches!(v, Violation::DanglingReference { entity, id, field, missing_id } if entity == "assignment" && id == "a3" && field == "position_id" && missing_id == "ghost")
    ));
    assert!(has(
        &|v| matches!(v, Violation::DanglingReference { id, field, .. } if id == "a3" && field == "user_id")
    ));

    // Looking at a day before any of it existed finds nothing; the whole timeline finds it.
    assert!(integrity_report(&f.pool, ORG, Some(day(-30)))
        .unwrap()
        .is_empty());
    assert!(integrity_report(&f.pool, ORG, None).unwrap().len() >= report.len());
    // Same input, same order.
    assert_eq!(
        report,
        integrity_report(&f.pool, ORG, Some(day(1))).unwrap()
    );
    // Another organization's view is not affected.
    let other = org::create_organization(&f.pool, "Other", "other", None, None, None, None)
        .unwrap()
        .org_id;
    assert!(integrity_report(&f.pool, &other, None).unwrap().is_empty());
}

#[test]
fn a_position_pointing_at_a_missing_role_or_unit_is_dangling() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let position = f.position(&unit, "Clerk", None);
    raw(&f.pool, "UPDATE org_positions SET role_id = 'gone-role', unit_id = 'gone-unit' WHERE position_id = ?1", &[&position.position_id]);

    let report = integrity_report(&f.pool, ORG, None).unwrap();

    let fields: Vec<&str> = report
        .iter()
        .filter_map(|v| match v {
            Violation::DanglingReference { field, .. } => Some(field.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(fields, vec!["role_id", "unit_id"]);
}

// ---------------------------------------------------------------------------
// Ending on the day something starts removes it
// ---------------------------------------------------------------------------

pub(super) fn count(f: &Fixture, table: &str) -> i64 {
    f.pool
        .read()
        .unwrap()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn projection_rows(f: &Fixture) -> Vec<(String, Option<String>, Option<String>)> {
    let conn = f.pool.read().unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT user_id, department_id, manager_user_id FROM sync_user_org_profiles ORDER BY user_id",
        )
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn ending_an_assignment_on_its_first_day_removes_it_and_leaves_the_projection_as_before() {
    let f = Fixture::new();
    let anna = add_user(&f.pool, ORG, "anna");
    let unit = f.unit("U", None);
    let boss = f.position(&unit, "Boss", None);
    let seat = f.position(&unit, "Seat", Some(&boss));
    let before = projection_rows(&f);
    let captures_before = count(&f, "__tentaflow_core_sync_captures");

    let assignment = f.assign_user(&seat, &anna, 1.0, None).unwrap().value;
    assert_eq!(list_assignments_at(&f.pool, ORG, today()).unwrap().len(), 1);
    end_assignment(&f.pool, &f.ctx(), &assignment.id, today()).unwrap();

    assert_eq!(list_assignments_at(&f.pool, ORG, today()).unwrap().len(), 0);
    assert_eq!(
        count(&f, "org_assignments"),
        0,
        "the row is gone, not a [D, D) interval"
    );
    assert_eq!(projection_rows(&f), before);
    assert!(
        count(&f, "__tentaflow_core_sync_captures") > captures_before,
        "the delete is captured for replication"
    );
    let audit: String = f
        .pool
        .read()
        .unwrap()
        .query_row(
            "SELECT details FROM audit_log WHERE action = 'org.assignment.end' ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(audit.contains("removed same-day creation"), "{audit}");
}

#[test]
fn ending_a_position_or_a_unit_on_its_first_day_removes_them_and_what_hangs_on_them() {
    let f = Fixture::new();
    let anna = add_user(&f.pool, ORG, "anna");
    let parent = f.unit("Parent", None);
    let unit = f.unit("Child", Some(&parent.unit_id));
    let boss = f.position(&unit, "Boss", None);
    let seat = f.position(&unit, "Seat", Some(&boss));
    f.assign_user(&seat, &anna, 1.0, None).unwrap();
    set_head(
        &f.pool,
        &f.ctx(),
        &unit.unit_id,
        Some(&boss.position_id),
        day(0),
    )
    .unwrap();

    // The head has to be cleared first, as on any other day.
    set_head(&f.pool, &f.ctx(), &unit.unit_id, None, day(0)).unwrap();
    let ended = end_position(&f.pool, &f.ctx(), &seat.position_id, today())
        .unwrap()
        .value;
    assert_eq!(
        ended.assignments.len(),
        1,
        "the holder goes with the position"
    );
    end_position(&f.pool, &f.ctx(), &boss.position_id, today()).unwrap();
    end_unit(&f.pool, &f.ctx(), &unit.unit_id, today()).unwrap();

    assert_eq!(count(&f, "org_positions"), 0);
    assert_eq!(count(&f, "org_reporting_lines"), 0);
    assert_eq!(count(&f, "org_assignments"), 0);
    assert_eq!(count(&f, "org_units"), 1, "only the parent is left");
    assert!(integrity_report(&f.pool, ORG, Some(today()))
        .unwrap()
        .is_empty());
    // Before the day it starts there is nothing to end.
    let later = f.unit("Later", None);
    assert!(end_unit(&f.pool, &f.ctx(), &later.unit_id, day(-1)).is_err());
}

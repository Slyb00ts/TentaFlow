//! Planned reorganizations at the service layer: the state machine, the
//! two-person rule and — the point of `mark_applied_in` — that the state change
//! commits or rolls back together with the operations of the batch it rides on.
//! Running the operations from their wire form belongs to the dispatch layer
//! and is tested there.

use super::batch::{self, Decision, Op};
use super::change_set::{self as cs, ChangeSetError as CE, State};
use super::history::{self, HistoryQuery, Privacy};
use super::tests::{add_user, day, Fixture, ORG};
use super::*;

const NO_OPS: &str = "[]";

struct World {
    f: Fixture,
    second: String,
}

fn world() -> World {
    let f = Fixture::new();
    let second = add_user(&f.pool, ORG, "second");
    let w = World { f, second };
    // Two administrators: the author of a plan may not approve it.
    set_admins(&w, &[w.f.actor.as_str(), w.second.as_str()]);
    w
}

fn ctx_as(user: &str) -> WriteCtx<'_> {
    WriteCtx {
        org_id: ORG,
        actor_user_id: user,
        confirm_backdated: false,
    }
}

fn draft(w: &World, name: &str, from: i64) -> cs::ChangeSet {
    cs::save(&w.f.pool, ORG, &w.f.actor, None, name, day(from), NO_OPS).unwrap()
}

fn pending(w: &World, name: &str, from: i64) -> cs::ChangeSet {
    let set = draft(w, name, from);
    cs::submit(&w.f.pool, ORG, &w.f.actor, &set.id).unwrap()
}

fn state_of(w: &World, id: &str) -> State {
    cs::get(&w.f.pool, ORG, id).unwrap().state
}

/// What the dispatcher does on approval: the state change on the batch's own transaction.
fn record(b: &mut batch::Batch<'_, '_>, approver: &str, id: &str) -> Result<()> {
    b.with_transaction(|tx, org| {
        cs::mark_applied_in(tx, org, approver, id, &cs::Undo::default()).map_err(|e| {
            OrgStructureError::InvalidValue {
                field: "change_set",
                reason: e.to_string(),
            }
        })
    })
}

fn new_unit(name: &str, from: i64) -> Op {
    Op::UnitCreate(NewUnit {
        name: name.into(),
        code: None,
        type_id: None,
        parent_unit_id: None,
        color: None,
        valid_from: day(from),
        valid_to: None,
    })
}

#[test]
fn a_draft_belongs_to_its_author_and_editing_hands_it_to_the_editor() {
    let w = world();
    let set = draft(&w, "Q4", 30);
    assert_eq!(
        (set.state, set.author_user_id.as_str(), set.op_count),
        (State::Draft, w.f.actor.as_str(), 0)
    );
    assert!(set.author_name.is_some());

    let edited = cs::save(
        &w.f.pool,
        ORG,
        &w.second,
        Some(&set.id),
        "Q4 v2",
        day(31),
        r#"[{"temp_id":null,"request":{"UnitMoveRequest":{"unit_id":"u","from":"2026-11-01","new_parent_unit_id":null}}}]"#,
    )
    .unwrap();
    assert_eq!(edited.id, set.id);
    assert_eq!(edited.name, "Q4 v2");
    assert_eq!(edited.effective_date, day(31));
    assert_eq!(edited.op_count, 1);
    assert_eq!(
        edited.author_user_id, w.second,
        "whoever last changed the content is the one who may not approve it"
    );
}

#[test]
fn editing_a_pending_set_puts_it_back_to_draft() {
    let w = world();
    let set = pending(&w, "Q4", 30);
    assert_eq!(set.state, State::Pending);
    let edited = cs::save(
        &w.f.pool,
        ORG,
        &w.second,
        Some(&set.id),
        "Q4",
        day(30),
        NO_OPS,
    )
    .unwrap();
    assert_eq!(
        edited.state,
        State::Draft,
        "the approver must read the new content"
    );
    assert_eq!(edited.approver_user_id, None);
}

#[test]
fn only_a_draft_or_a_pending_set_can_be_edited_or_withdrawn() {
    let w = world();
    let withdrawn = draft(&w, "old", 30);
    cs::withdraw(&w.f.pool, ORG, &w.f.actor, &withdrawn.id).unwrap();
    for result in [
        cs::save(
            &w.f.pool,
            ORG,
            &w.f.actor,
            Some(&withdrawn.id),
            "x",
            day(30),
            NO_OPS,
        ),
        cs::submit(&w.f.pool, ORG, &w.f.actor, &withdrawn.id),
        cs::withdraw(&w.f.pool, ORG, &w.f.actor, &withdrawn.id),
    ] {
        let error = result.unwrap_err();
        assert_eq!(error.code(), "change_set_state", "{error}");
    }
    assert_eq!(state_of(&w, &withdrawn.id), State::Withdrawn);
}

#[test]
fn a_bad_document_is_refused_before_anything_is_stored() {
    let w = world();
    let refuse = |name: &str, payload: &str| {
        cs::save(&w.f.pool, ORG, &w.f.actor, None, name, day(30), payload)
            .unwrap_err()
            .code()
    };
    assert_eq!(refuse("  ", NO_OPS), "empty_field");
    assert_eq!(refuse("ok", "{\"not\":\"a list\"}"), "invalid_value");
    assert_eq!(refuse("ok", "not json"), "invalid_value");
    assert_eq!(refuse(&"x".repeat(300), NO_OPS), "invalid_value");
    assert!(cs::list(&w.f.pool, ORG).unwrap().is_empty());
}

#[test]
fn a_plan_dated_in_the_past_cannot_be_submitted_or_approved() {
    let w = world();
    let old = draft(&w, "late", -1);
    assert_eq!(
        cs::submit(&w.f.pool, ORG, &w.f.actor, &old.id)
            .unwrap_err()
            .code(),
        "effective_date_passed"
    );
    let today = pending(&w, "today", 0);
    assert!(
        cs::check_approvable(&w.f.pool, ORG, &w.second, &today.id).is_ok(),
        "a plan for today is still ahead"
    );
}

#[test]
fn nobody_approves_their_own_plan_and_only_a_pending_plan_is_approvable() {
    let w = world();
    let set = pending(&w, "Q4", 30);
    let own = cs::check_approvable(&w.f.pool, ORG, &w.f.actor, &set.id).unwrap_err();
    assert_eq!(own, CE::SelfApproval);
    assert_eq!(state_of(&w, &set.id), State::Pending);
    assert!(cs::check_approvable(&w.f.pool, ORG, &w.second, &set.id).is_ok());

    let unsent = draft(&w, "draft", 30);
    assert_eq!(
        cs::check_approvable(&w.f.pool, ORG, &w.second, &unsent.id)
            .unwrap_err()
            .code(),
        "change_set_state"
    );
    assert_eq!(
        cs::check_approvable(&w.f.pool, ORG, &w.second, "missing")
            .unwrap_err()
            .code(),
        "not_found"
    );
}

#[test]
fn recording_the_approval_rides_on_the_transaction_of_the_batch() {
    let w = world();
    let set = pending(&w, "Q4", 30);

    // The batch keeps its changes: the unit and the state change are both there.
    batch::run(&w.f.pool, &ctx_as(&w.second), true, |b| {
        b.apply(&new_unit("Quality", 30), false).unwrap();
        record(b, &w.second, &set.id).unwrap();
        Ok(Decision {
            value: (),
            keep: true,
            summary: serde_json::json!({}),
        })
    })
    .unwrap();
    let applied = cs::get(&w.f.pool, ORG, &set.id).unwrap();
    assert_eq!(applied.state, State::Applied);
    assert_eq!(applied.approver_user_id.as_deref(), Some(w.second.as_str()));
    let names = |offset: i64| -> Vec<String> {
        query::structure_as_of(&w.f.pool, ORG, Some(day(offset)))
            .unwrap()
            .units
            .into_iter()
            .map(|u| u.unit.name)
            .collect()
    };
    assert_eq!(
        names(30),
        ["Quality"],
        "the change takes effect on its day by itself"
    );
    assert!(names(0).is_empty(), "and not before it");
}

#[test]
fn a_batch_that_is_not_kept_leaves_the_plan_pending() {
    let w = world();
    let set = pending(&w, "Q4", 30);
    batch::run(&w.f.pool, &ctx_as(&w.second), true, |b| {
        b.apply(&new_unit("Quality", 30), false).unwrap();
        record(b, &w.second, &set.id).unwrap();
        Ok(Decision {
            value: (),
            keep: false,
            summary: serde_json::json!({}),
        })
    })
    .unwrap();
    assert_eq!(state_of(&w, &set.id), State::Pending);
    assert!(query::structure_as_of(&w.f.pool, ORG, Some(day(30)))
        .unwrap()
        .units
        .is_empty());
}

#[test]
fn the_author_cannot_record_the_approval_even_inside_a_batch() {
    let w = world();
    let set = pending(&w, "Q4", 30);
    batch::run(&w.f.pool, &w.f.ctx(), true, |b| {
        let error = record(b, &w.f.actor, &set.id).unwrap_err();
        assert!(error.to_string().contains("cannot approve"), "{error}");
        Ok(Decision {
            value: (),
            keep: false,
            summary: serde_json::json!({}),
        })
    })
    .unwrap();
    assert_eq!(state_of(&w, &set.id), State::Pending);
}

#[test]
fn withdrawing_leaves_the_structure_alone_and_a_plan_whose_day_came_cannot_be_withdrawn() {
    let w = world();
    w.f.unit("Existing", None);
    let before = query::structure_as_of(&w.f.pool, ORG, Some(day(30)))
        .unwrap()
        .units;
    let set = pending(&w, "Q4", 30);
    let withdrawn = cs::withdraw(&w.f.pool, ORG, &w.second, &set.id).unwrap();
    assert_eq!(withdrawn.state, State::Withdrawn);
    assert_eq!(
        query::structure_as_of(&w.f.pool, ORG, Some(day(30)))
            .unwrap()
            .units,
        before
    );

    let applied = pending(&w, "Q5", 0);
    batch::run(&w.f.pool, &ctx_as(&w.second), true, |b| {
        record(b, &w.second, &applied.id).unwrap();
        Ok(Decision {
            value: (),
            keep: true,
            summary: serde_json::json!({}),
        })
    })
    .unwrap();
    assert_eq!(
        cs::withdraw(&w.f.pool, ORG, &w.second, &applied.id)
            .unwrap_err()
            .code(),
        "change_set_started"
    );
    assert_eq!(state_of(&w, &applied.id), State::Applied);
}

#[test]
fn a_plan_of_another_organization_does_not_exist_here() {
    let w = world();
    let set = draft(&w, "mine", 30);
    assert_eq!(
        cs::get(&w.f.pool, "another-org", &set.id)
            .unwrap_err()
            .code(),
        "not_found"
    );
    assert!(cs::list(&w.f.pool, "another-org").unwrap().is_empty());
    assert_eq!(
        cs::withdraw(&w.f.pool, "another-org", &w.f.actor, &set.id)
            .unwrap_err()
            .code(),
        "not_found"
    );
    assert_eq!(state_of(&w, &set.id), State::Draft);
}

#[test]
fn the_listing_carries_no_operations_and_names_the_people() {
    let w = world();
    let set = cs::save(
        &w.f.pool,
        ORG,
        &w.f.actor,
        None,
        "Q4",
        day(30),
        r#"[{"temp_id":null,"request":{}},{"temp_id":null,"request":{}}]"#,
    )
    .unwrap();
    let listed = cs::list(&w.f.pool, ORG).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!((listed[0].op_count, listed[0].payload.as_str()), (2, ""));
    assert!(listed[0].author_name.is_some());
    assert!(cs::get(&w.f.pool, ORG, &set.id).unwrap().payload.len() > 2);
}

#[test]
fn the_lifecycle_is_audited_for_administrators_only() {
    let w = world();
    let set = pending(&w, "Q4", 30);
    cs::withdraw(&w.f.pool, ORG, &w.second, &set.id).unwrap();

    let entries = |privacy: &Privacy<'_>| {
        history::list_changes(&w.f.pool, ORG, privacy, &HistoryQuery::default())
            .unwrap()
            .entries
    };
    let admin = Privacy {
        personal_visible: true,
        viewer_user_id: &w.f.actor,
    };
    let actions: Vec<String> = entries(&admin).into_iter().map(|e| e.action).collect();
    assert_eq!(
        actions,
        [
            "org.change_set.withdraw",
            "org.change_set.submit",
            "org.change_set.create"
        ]
    );
    assert!(entries(&Privacy {
        personal_visible: false,
        viewer_user_id: &w.second,
    })
    .is_empty());
}

#[test]
fn every_state_change_is_captured_for_replication_with_the_row_as_it_then_was() {
    use crate::sync::core_capture::load_core_write_capture;

    let w = world();
    let captures = || -> Vec<_> {
        let conn = w.f.pool.read().unwrap();
        let ids: Vec<String> = conn
            .prepare(
                "SELECT capture_id FROM __tentaflow_core_sync_captures \
                 WHERE resource_type = 'core.org_change_set' ORDER BY created_at_ms, rowid",
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

    let set = pending(&w, "Q4", 30);
    batch::run(&w.f.pool, &ctx_as(&w.second), true, |b| {
        record(b, &w.second, &set.id).unwrap();
        Ok(Decision {
            value: (),
            keep: true,
            summary: serde_json::json!({}),
        })
    })
    .unwrap();

    // Created, submitted, approved: three captures of one row, the last carrying the approver.
    let all = captures();
    assert_eq!(all.len(), 3);
    let last = format!("{:?}", all.last().unwrap());
    assert!(last.contains("applied"), "{last}");
    assert!(last.contains(&w.second), "{last}");
}

fn set_admins(w: &World, admins: &[&str]) {
    let conn = w.f.pool.write().unwrap();
    let admin_role: String = conn
        .query_row(
            "SELECT role_id FROM roles WHERE name = 'org_admin'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let plain_role: String = conn
        .query_row(
            "SELECT role_id FROM roles WHERE permissions_json NOT LIKE '%org.admin%' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    conn.execute("UPDATE org_memberships SET role_id = ?1", [&plain_role])
        .unwrap();
    // The bootstrap account cannot be demoted, so it is deactivated.
    conn.execute(
        "UPDATE user_accounts SET is_active = 0 WHERE id = '00000000-0000-4000-8000-000000000002'",
        [],
    )
    .unwrap();
    for user in admins {
        conn.execute(
            "UPDATE org_memberships SET role_id = ?1 WHERE user_id = ?2",
            [&admin_role, &user.to_string()],
        )
        .unwrap();
    }
}

#[test]
fn the_only_active_administrator_may_approve_their_own_plan() {
    let w = world();
    set_admins(&w, &[w.f.actor.as_str()]);
    let set = pending(&w, "Q4", 30);
    assert!(cs::is_sole_admin(&w.f.pool, ORG).unwrap());
    assert!(cs::check_approvable(&w.f.pool, ORG, &w.f.actor, &set.id).is_ok());
    batch::run(&w.f.pool, &ctx_as(&w.f.actor), true, |b| {
        record(b, &w.f.actor, &set.id).unwrap();
        Ok(Decision {
            value: (),
            keep: true,
            summary: serde_json::json!({}),
        })
    })
    .unwrap();
    assert_eq!(state_of(&w, &set.id), State::Applied);
}

#[test]
fn with_two_active_administrators_the_author_is_denied_again() {
    let w = world();
    set_admins(&w, &[w.f.actor.as_str()]);
    let set = pending(&w, "Q4", 30);
    set_admins(&w, &[w.f.actor.as_str(), w.second.as_str()]);
    assert!(!cs::is_sole_admin(&w.f.pool, ORG).unwrap());
    assert_eq!(
        cs::check_approvable(&w.f.pool, ORG, &w.f.actor, &set.id).unwrap_err(),
        CE::SelfApproval
    );
    assert!(cs::check_approvable(&w.f.pool, ORG, &w.second, &set.id).is_ok());
}

#[test]
fn a_deactivated_administrator_does_not_count() {
    let w = world();
    set_admins(&w, &[w.f.actor.as_str(), w.second.as_str()]);
    w.f.pool
        .write()
        .unwrap()
        .execute(
            "UPDATE user_accounts SET is_active = 0 WHERE id = ?1",
            [&w.second],
        )
        .unwrap();
    assert!(cs::is_sole_admin(&w.f.pool, ORG).unwrap());
}

// ---------------------------------------------------------------------------
// Two nodes approving the same plan, and the take-back that already happened
// ---------------------------------------------------------------------------

/// The same structure on a fresh node, made with ids that follow from `scope`
/// alone, so two nodes that start from it start from identical rows.
fn node_with_base() -> World {
    let w = world();
    batch::run(&w.f.pool, &w.f.ctx(), true, |b| {
        b.scope_ids(Some("base:0".into()));
        let unit = match b.apply(&new_unit("Board", 0), false).unwrap() {
            batch::OpValue::Unit(unit) => unit,
            other => panic!("{other:?}"),
        };
        b.scope_ids(Some("base:1".into()));
        let _ = b
            .apply(
                &Op::PositionCreate(NewPosition {
                    unit_id: unit.unit_id.clone(),
                    name: "CEO".into(),
                    code: None,
                    role_id: None,
                    is_manager: None,
                    is_staff: false,
                    parent_position_id: None,
                    valid_from: day(0),
                    valid_to: None,
                }),
                false,
            )
            .unwrap();
        Ok(Decision {
            value: (),
            keep: true,
            summary: serde_json::json!({}),
        })
    })
    .unwrap();
    w
}

/// What the dispatcher does on approval: the plan's operations on one batch, ids scoped to the plan and
/// the operation, the approval recorded with what it changed.
fn approve_plan(w: &World, approver: &str, set_id: &str, scope: Option<&str>) {
    batch::run(&w.f.pool, &ctx_as(approver), true, |b| {
        let before = b
            .with_transaction(|tx, org| {
                cs::snapshot_in(tx, org).map_err(|e| OrgStructureError::Db(e.to_string()))
            })
            .unwrap();
        let ids = |index: usize| scope.map(|plan| format!("{plan}:{index}"));
        b.scope_ids(ids(0));
        let unit = match b.apply(&new_unit("Quality", 30), true).unwrap() {
            batch::OpValue::Unit(unit) => unit,
            other => panic!("{other:?}"),
        };
        b.scope_ids(ids(1));
        b.apply(
            &Op::PositionCreate(NewPosition {
                unit_id: unit.unit_id.clone(),
                name: "Inspector".into(),
                code: None,
                role_id: None,
                is_manager: None,
                is_staff: false,
                parent_position_id: None,
                valid_from: day(30),
                valid_to: None,
            }),
            true,
        )
        .unwrap();
        b.scope_ids(ids(2));
        b.apply(
            &Op::ExternalPersonCreate(NewExternalPerson {
                display_name: "Jan".into(),
                email: None,
                note: None,
            }),
            true,
        )
        .unwrap();
        b.scope_ids(None);
        b.with_transaction(|tx, org| {
            let now = cs::snapshot_in(tx, org).map_err(|e| OrgStructureError::Db(e.to_string()))?;
            cs::mark_applied_in(tx, org, approver, set_id, &cs::undo_between(&before, &now))
                .map_err(|e| OrgStructureError::InvalidValue {
                    field: "change_set",
                    reason: e.to_string(),
                })
        })
        .unwrap();
        Ok(Decision {
            value: (),
            keep: true,
            summary: serde_json::json!({}),
        })
    })
    .unwrap();
}

fn rows_of(w: &World) -> cs::Snapshot {
    let mut conn = w.f.pool.write().unwrap();
    let tx = conn.transaction().unwrap();
    cs::snapshot_in(&tx, ORG).unwrap()
}

#[test]
fn two_nodes_approving_the_same_plan_at_once_make_the_same_rows() {
    let (a, b) = (node_with_base(), node_with_base());
    assert_eq!(
        rows_of(&a),
        rows_of(&b),
        "both nodes start from the same rows"
    );
    let (set_a, set_b) = (pending(&a, "Q4", 30), pending(&b, "Q4", 30));
    approve_plan(&a, &a.second, &set_a.id, Some("plan-7"));
    approve_plan(&b, &b.second, &set_b.id, Some("plan-7"));
    let (rows_a, rows_b) = (rows_of(&a), rows_of(&b));
    assert_eq!(rows_a, rows_b);
    assert_eq!(rows_a["org_units"].len(), 2);
    assert_eq!(rows_a["org_positions"].len(), 2);
    assert_eq!(rows_a["org_external_persons"].len(), 1);
}

#[test]
fn without_a_plan_scope_two_nodes_would_make_different_rows() {
    let (a, b) = (node_with_base(), node_with_base());
    let (set_a, set_b) = (pending(&a, "Q4", 30), pending(&b, "Q4", 30));
    approve_plan(&a, &a.second, &set_a.id, None);
    approve_plan(&b, &b.second, &set_b.id, None);
    assert_ne!(rows_of(&a), rows_of(&b));
}

#[test]
fn withdrawing_a_plan_whose_rows_another_node_already_took_back_only_records_the_state() {
    let w = node_with_base();
    let set = pending(&w, "Q4", 30);
    approve_plan(&w, &w.second, &set.id, Some("plan-7"));
    let applied = cs::get(&w.f.pool, ORG, &set.id).unwrap();
    assert_eq!(applied.state, State::Applied);

    // What the other node's withdrawal does here when it replicates: the rows the plan made are deleted.
    {
        let conn = w.f.pool.write().unwrap();
        conn.execute("DELETE FROM org_positions WHERE name = 'Inspector'", [])
            .unwrap();
        conn.execute("DELETE FROM org_units WHERE name = 'Quality'", [])
            .unwrap();
        conn.execute(
            "DELETE FROM org_external_persons WHERE display_name = 'Jan'",
            [],
        )
        .unwrap();
    }
    let withdrawn = cs::withdraw(&w.f.pool, ORG, &w.second, &set.id).unwrap();
    assert_eq!(withdrawn.state, State::Withdrawn);
    let rows = rows_of(&w);
    assert_eq!(rows["org_units"].len(), 1, "only the base unit is left");
}

#[test]
fn withdrawing_an_applied_plan_still_takes_back_what_it_made() {
    let w = node_with_base();
    let set = pending(&w, "Q4", 30);
    approve_plan(&w, &w.second, &set.id, Some("plan-7"));
    assert_eq!(rows_of(&w)["org_units"].len(), 2);
    let withdrawn = cs::withdraw(&w.f.pool, ORG, &w.second, &set.id).unwrap();
    assert_eq!(withdrawn.state, State::Withdrawn);
    let rows = rows_of(&w);
    assert_eq!(rows["org_units"].len(), 1);
    assert_eq!(rows["org_positions"].len(), 1);
    assert!(rows
        .get("org_external_persons")
        .is_none_or(|t| t.is_empty()));
}

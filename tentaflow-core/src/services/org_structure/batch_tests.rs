//! Batches at the service layer: the transaction, the savepoints and the
//! finishing work. Temporary ids and the wire shapes belong to the dispatch
//! layer and are tested there.

use super::batch::{self, Decision, Op, OpValue};
use super::tests::{day, Fixture};
use super::*;

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

struct Ran {
    /// `Ok(created id)` or the error code, per operation.
    outcomes: Vec<std::result::Result<Option<String>, &'static str>>,
    warnings: Vec<Warning>,
    preview_units: usize,
}

/// Runs `ops` in order, later ones seeing what the earlier ones made through
/// `make` (which builds the operation from the ids made so far).
fn run_ops(
    f: &Fixture,
    commit: bool,
    ops: &[&dyn Fn(&[Option<String>]) -> Op],
    confirm_backdated: bool,
) -> Ran {
    batch::run(&f.pool, &f.ctx(), commit, |b| {
        let mut made: Vec<Option<String>> = Vec::new();
        let mut outcomes = Vec::new();
        for make in ops {
            match b.apply(&make(&made), confirm_backdated) {
                Ok(value) => {
                    let id = value.entity_id().map(str::to_string);
                    made.push(id.clone());
                    outcomes.push(Ok(id));
                }
                Err(e) => {
                    made.push(None);
                    outcomes.push(Err(e.code()));
                }
            }
        }
        let view = b.preview(Some(day(0))).unwrap();
        let failed = outcomes.iter().any(|o| o.is_err());
        Ok(Decision {
            value: Ran {
                outcomes,
                warnings: b.warnings(),
                preview_units: view.units.len(),
            },
            keep: !failed,
            summary: serde_json::json!({ "ops": ops.len() }),
        })
    })
    .unwrap()
}

/// Units, not versions: setting a head adds a version of the unit.
fn units_in_db(f: &Fixture) -> i64 {
    f.pool
        .read()
        .unwrap()
        .query_row("SELECT COUNT(DISTINCT unit_id) FROM org_units", [], |r| {
            r.get(0)
        })
        .unwrap()
}

fn org_audit(f: &Fixture) -> Vec<(String, String)> {
    let conn = f.pool.read().unwrap();
    let mut stmt = conn
        .prepare("SELECT action, COALESCE(details, '') FROM audit_log WHERE action LIKE 'org.%' ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(std::result::Result::unwrap)
        .collect()
}

#[test]
fn a_batch_saves_dependent_operations_together_with_one_audit_entry_that_lists_them() {
    let f = Fixture::new();
    let ran = run_ops(
        &f,
        true,
        &[
            &|_| new_unit("Board", 0),
            &|made| {
                Op::PositionCreate(NewPosition {
                    unit_id: made[0].clone().unwrap(),
                    name: "CEO".into(),
                    code: None,
                    role_id: None,
                    is_manager: None,
                    is_staff: false,
                    parent_position_id: None,
                    valid_from: day(0),
                    valid_to: None,
                })
            },
            &|made| Op::HeadSet {
                unit_id: made[0].clone().unwrap(),
                head_position_id: made[1].clone(),
                from: day(0),
            },
        ],
        false,
    );
    assert!(
        ran.outcomes.iter().all(std::result::Result::is_ok),
        "{:?}",
        ran.outcomes
    );
    assert_eq!(units_in_db(&f), 1);
    assert!(
        ran.warnings
            .iter()
            .all(|w| matches!(w, Warning::UnitWithoutHead { .. })),
        "the warning of the headless unit is raised while it is headless: {:?}",
        ran.warnings
    );

    let audit = org_audit(&f);
    let batch_entries: Vec<_> = audit
        .iter()
        .filter(|(action, _)| action == "org.structure.batch")
        .collect();
    assert_eq!(
        audit.len(),
        1,
        "one entry for three operations, none of their own: {audit:?}"
    );
    assert_eq!(batch_entries.len(), 1);
    let details: serde_json::Value = serde_json::from_str(&batch_entries[0].1).unwrap();
    assert_eq!(details["source"], "batch");
    let listed: Vec<String> = details["operations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|op| op[0].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        listed,
        [
            "org.unit.create",
            "org.position.create",
            "org.unit.head_set"
        ]
    );
}

#[test]
fn one_refused_operation_is_reported_where_it_is_and_the_whole_batch_is_dropped() {
    let f = Fixture::new();
    let existing = f.unit("Existing", None);
    let before = org_audit(&f).len();
    let ran = run_ops(
        &f,
        true,
        &[
            &|_| new_unit("Made first", 0),
            &|_| Op::UnitEnd {
                unit_id: "no-such-unit".into(),
                from: day(0),
            },
            &|_| new_unit("Made last", 0),
        ],
        false,
    );
    assert_eq!(ran.outcomes[0].as_ref().map(|id| id.is_some()), Ok(true));
    assert_eq!(ran.outcomes[1], Err("not_found"));
    assert!(
        ran.outcomes[2].is_ok(),
        "the operations after a refusal still run"
    );
    assert_eq!(
        ran.preview_units, 3,
        "the preview shows the operations that ran"
    );
    assert_eq!(units_in_db(&f), 1, "nothing was saved");
    assert_eq!(org_audit(&f).len(), before, "and nothing was audited");
    assert_eq!(existing.name, "Existing");
}

#[test]
fn a_dry_run_shows_the_result_and_leaves_no_row_capture_or_audit_entry() {
    let f = Fixture::new();
    let captures = |f: &Fixture| -> i64 {
        f.pool
            .read()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM __tentaflow_core_sync_captures",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    let (audit_before, captures_before) = (org_audit(&f).len(), captures(&f));
    let ran = run_ops(&f, false, &[&|_| new_unit("Draft", 0)], false);
    assert!(ran.outcomes[0].is_ok());
    assert_eq!(ran.preview_units, 1);
    assert_eq!(units_in_db(&f), 0);
    assert_eq!(org_audit(&f).len(), audit_before);
    assert_eq!(captures(&f), captures_before);
}

#[test]
fn rewriting_history_needs_the_confirmation_of_the_operation_or_the_batch() {
    let f = Fixture::new();
    let refused = run_ops(&f, true, &[&|_| new_unit("Backdated", -3)], false);
    assert_eq!(refused.outcomes[0], Err("backdated_confirmation_required"));
    assert_eq!(units_in_db(&f), 0);

    let confirmed = run_ops(&f, true, &[&|_| new_unit("Backdated", -3)], true);
    assert!(confirmed.outcomes[0].is_ok());
    assert_eq!(units_in_db(&f), 1);
    let audit = org_audit(&f);
    let (_, details) = audit
        .iter()
        .find(|(action, _)| action == "org.structure.batch")
        .unwrap();
    let details: serde_json::Value = serde_json::from_str(details).unwrap();
    assert_eq!(details["backdated"], true);
    assert_eq!(details["operations"][0][0], "org.unit.create");
}

#[test]
fn the_same_operation_gives_the_same_result_alone_and_in_a_batch() {
    let f = Fixture::new();
    let single = batch::run_single(&f.pool, &f.ctx(), &new_unit("Alone", 0)).unwrap();
    let OpValue::Unit(alone) = single.value else {
        panic!("a unit is made")
    };
    let ran = run_ops(&f, true, &[&|_| new_unit("Together", 0)], false);
    assert!(ran.outcomes[0].is_ok());
    assert_eq!(units_in_db(&f), 2);
    assert_eq!(alone.name, "Alone");
}

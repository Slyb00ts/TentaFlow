//! The import end to end on a real database: parse, plan, dry run, apply,
//! export, and the round trip between them.

use std::time::Instant;

use super::columns::Column;
use super::report::{IssueKind, Report, RowStatus};
use super::*;
use crate::services::org;
use crate::services::org_structure::tests::{day, s, today, Fixture, ORG};
use crate::services::org_structure::types::*;
use crate::services::org_structure::{self as svc};

const HEADER: &str = "kod jednostki;nazwa jednostki;kod nadrzędnej;typ;kod stanowiska;stanowisko;\
sztabowe;kierownik jednostki;stanowisko główne;login/e-mail osoby;część etatu;\
przełożony (kod stanowiska);zastępca kierownika (kolejność);od kiedy";

/// One row of the test file, by column; an unset field is an empty cell.
#[derive(Default, Clone)]
struct R<'a> {
    unit: &'a str,
    name: &'a str,
    parent: &'a str,
    kind: &'a str,
    pos: &'a str,
    title: &'a str,
    staff: &'a str,
    head: &'a str,
    primary: &'a str,
    person: &'a str,
    share: &'a str,
    manager: &'a str,
    deputy: &'a str,
    from: &'a str,
}

fn line(r: &R) -> String {
    [
        r.unit, r.name, r.parent, r.kind, r.pos, r.title, r.staff, r.head, r.primary, r.person,
        r.share, r.manager, r.deputy, r.from,
    ]
    .join(";")
}

fn csv(rows: &[R]) -> Vec<u8> {
    let mut text = String::from(HEADER);
    for row in rows {
        text.push('\n');
        text.push_str(&line(row));
    }
    text.into_bytes()
}

fn member(f: &Fixture, login: &str, name: &str) -> String {
    let id = crate::db::repository::create_user_account(
        &f.pool,
        login,
        "hash",
        name,
        &format!("{login}@firma.pl"),
    )
    .unwrap();
    let role_id: String = f
        .pool
        .read()
        .unwrap()
        .query_row("SELECT role_id FROM roles LIMIT 1", [], |r| r.get(0))
        .unwrap();
    org::add_membership(&f.pool, ORG, &id, &role_id, "test").unwrap();
    id
}

fn request<'a>(bytes: &'a [u8], mode: Mode) -> ImportRequest<'a> {
    ImportRequest {
        format: FileFormat::Csv,
        bytes,
        mode,
        as_of: None,
        resolutions: &[],
        confirm_ended: false,
    }
}

fn dry(f: &Fixture, bytes: &[u8]) -> Report {
    run(&f.pool, &f.ctx(), &request(bytes, Mode::Upsert), false).unwrap()
}

fn apply(f: &Fixture, bytes: &[u8]) -> Report {
    run(&f.pool, &f.ctx(), &request(bytes, Mode::Upsert), true).unwrap()
}

fn kinds(issues: &[report::Issue]) -> Vec<(u32, IssueKind)> {
    issues.iter().map(|i| (i.row, i.kind)).collect()
}

fn count(f: &Fixture, table: &str) -> i64 {
    f.pool
        .read()
        .unwrap()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn structure_rows(f: &Fixture) -> i64 {
    [
        "org_units",
        "org_positions",
        "org_assignments",
        "org_reporting_lines",
        "org_unit_deputy_heads",
    ]
    .iter()
    .map(|t| count(f, t))
    .sum()
}

fn units(f: &Fixture) -> Vec<Unit> {
    svc::list_units_at(&f.pool, ORG, today()).unwrap()
}

fn position_by_code(f: &Fixture, code: &str) -> Position {
    svc::list_positions_at(&f.pool, ORG, today())
        .unwrap()
        .into_iter()
        .find(|p| p.code.as_deref() == Some(code))
        .unwrap_or_else(|| panic!("no position {code}"))
}

fn unit_by_code(f: &Fixture, code: &str) -> Unit {
    units(f)
        .into_iter()
        .find(|u| u.code.as_deref() == Some(code))
        .unwrap_or_else(|| panic!("no unit {code}"))
}

/// A small company: IT with a head, a deputy and a developer, and a Dev team
/// under it with a lead.
fn company() -> Vec<R<'static>> {
    vec![
        R {
            unit: "IT",
            name: "Dział IT",
            pos: "IT-DIR",
            title: "Dyrektor",
            head: "tak",
            person: "anna",
            ..R::default()
        },
        R {
            unit: "IT",
            pos: "IT-DEP",
            title: "Zastępca",
            manager: "IT-DIR",
            deputy: "1",
            person: "bartek",
            ..R::default()
        },
        R {
            unit: "DEV",
            name: "Zespół Dev",
            parent: "IT",
            pos: "DEV-LEAD",
            title: "Kierownik zespołu",
            head: "tak",
            manager: "IT-DIR",
            person: "celina",
            share: "0,5",
            ..R::default()
        },
        R {
            unit: "DEV",
            pos: "DEV-1",
            title: "Developer",
            manager: "DEV-LEAD",
            ..R::default()
        },
        R {
            unit: "DEV",
            pos: "DEV-2",
            title: "Developer",
            manager: "DEV-LEAD",
            person: "darek",
            ..R::default()
        },
    ]
}

fn people(f: &Fixture) {
    for (login, name) in [
        ("anna", "Anna Nowak"),
        ("bartek", "Bartek Kowal"),
        ("celina", "Celina Wiśniewska"),
        ("darek", "Darek Zając"),
    ] {
        member(f, login, name);
    }
}

// ---------------------------------------------------------------------------
// Dry run and apply
// ---------------------------------------------------------------------------

#[test]
fn a_dry_run_reports_everything_and_writes_nothing() {
    let f = Fixture::new();
    people(&f);
    let before = structure_rows(&f);

    let report = dry(&f, &csv(&company()));

    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(!report.applied);
    assert_eq!(report.counts.rows, 5);
    assert_eq!(report.counts.added, 5);
    assert_eq!(report.counts.units_added, 2);
    assert_eq!(report.counts.positions_added, 5);
    assert_eq!(report.counts.assignments_added, 4);
    assert_eq!(
        structure_rows(&f),
        before,
        "a dry run leaves the database as it was"
    );
    // The audit trail has nothing of it either.
    assert_eq!(count(&f, "audit_log WHERE action LIKE 'org.%'"), 0);

    // The preview is the structure the file would leave, ids and all.
    let preview = report.preview.expect("a preview");
    assert_eq!(preview.units.len(), 2);
    assert_eq!(preview.positions.len(), 5);
    assert_eq!(preview.assignments.len(), 4);
    assert_eq!(preview.vacancies.len(), 1, "DEV-1 has nobody");
    let dev_lead = preview
        .positions
        .iter()
        .find(|p| p.position.code.as_deref() == Some("DEV-LEAD"))
        .unwrap();
    let it_dir = preview
        .positions
        .iter()
        .find(|p| p.position.code.as_deref() == Some("IT-DIR"))
        .unwrap();
    assert_eq!(
        dev_lead.primary_parent_position_id.as_deref(),
        Some(it_dir.position.position_id.as_str())
    );
    assert!(it_dir.is_head);
    // A row knows its entities in the preview.
    let row = report.rows.iter().find(|r| r.row == 4).unwrap();
    assert_eq!(
        row.position_id.as_deref(),
        Some(dev_lead.position.position_id.as_str())
    );
}

#[test]
fn an_apply_writes_the_structure_in_one_go_and_recomputes_the_projection_once() {
    let f = Fixture::new();
    people(&f);

    let report = apply(&f, &csv(&company()));

    assert!(report.applied, "{:?}", report.errors);
    assert_eq!(units(&f).len(), 2);
    let dev = unit_by_code(&f, "DEV");
    let it = unit_by_code(&f, "IT");
    assert_eq!(dev.parent_unit_id.as_deref(), Some(it.unit_id.as_str()));
    assert_eq!(
        dev.head_position_id,
        Some(position_by_code(&f, "DEV-LEAD").position_id)
    );
    assert_eq!(
        it.head_position_id,
        Some(position_by_code(&f, "IT-DIR").position_id)
    );
    let deputies = svc::list_deputy_heads_at(&f.pool, ORG, today()).unwrap();
    assert_eq!(deputies.len(), 1);
    assert_eq!(
        deputies[0].position_id,
        position_by_code(&f, "IT-DEP").position_id
    );
    let assignments = svc::list_assignments_at(&f.pool, ORG, today()).unwrap();
    let share_of_celina = assignments
        .iter()
        .find(|a| a.position_id == position_by_code(&f, "DEV-LEAD").position_id)
        .unwrap()
        .share;
    assert_eq!(share_of_celina, 0.5, "0,5 with a decimal comma");

    // The projection ran: Darek's manager is Celina, Celina's is Anna.
    let manager_of = |login: &str| -> Option<String> {
        let conn = f.pool.read().unwrap();
        conn.query_row(
            "SELECT p.manager_user_id FROM sync_user_org_profiles p \
             JOIN user_accounts u ON u.id = p.user_id WHERE u.username = ?1",
            [login],
            |r| r.get(0),
        )
        .ok()
    };
    let id_of = |login: &str| -> String {
        f.pool
            .read()
            .unwrap()
            .query_row(
                "SELECT id FROM user_accounts WHERE username = ?1",
                [login],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(manager_of("darek"), Some(id_of("celina")));
    assert_eq!(manager_of("celina"), Some(id_of("anna")));
}

#[test]
fn an_apply_is_one_audit_entry_with_the_source_import_and_the_operations_listed() {
    let f = Fixture::new();
    people(&f);
    let before: i64 = count(&f, "audit_log");
    apply(&f, &csv(&company()));

    let conn = f.pool.read().unwrap();
    let entries: Vec<String> = conn
        .prepare("SELECT details FROM audit_log WHERE action = 'org.structure.import'")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(entries.len(), 1);
    let details: serde_json::Value = serde_json::from_str(&entries[0]).unwrap();
    assert_eq!(details["source"], "import");
    assert_eq!(details["org_id"], ORG);
    assert_eq!(details["mode"], "upsert");
    assert_eq!(details["counts"]["units_added"], 2);
    let operations = details["operations"].as_array().unwrap();
    let of = |action: &str| operations.iter().filter(|o| o[0] == action).count();
    assert_eq!(
        (
            of("org.unit.create"),
            of("org.position.create"),
            of("org.assignment.create"),
            of("org.unit.head_set"),
            of("org.unit.deputies_set")
        ),
        (2, 5, 4, 2, 1)
    );
    assert!(
        operations
            .iter()
            .all(|o| o[1].as_str().unwrap().contains(':')),
        "each names its target"
    );
    drop(conn);
    // One entry for the whole file, not one per row.
    assert_eq!(count(&f, "audit_log"), before + 1);
    // A hand edit is a plain entry of its own.
    f.unit("Manual", None);
    let last: String = f
        .pool
        .read()
        .unwrap()
        .query_row(
            "SELECT details FROM audit_log WHERE action = 'org.unit.create'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!last.contains("import"));
}

#[test]
fn importing_the_same_file_again_changes_nothing() {
    let f = Fixture::new();
    people(&f);
    let file = csv(&company());
    apply(&f, &file);
    let rows = structure_rows(&f);

    let again = apply(&f, &file);

    assert!(again.applied);
    assert_eq!(again.counts.unchanged, 5, "{:?}", again.rows);
    assert_eq!(
        (
            again.counts.added,
            again.counts.changed,
            again.counts.errors
        ),
        (0, 0, 0)
    );
    assert!(again.rows.is_empty(), "unchanged rows are only counted");
    assert_eq!(structure_rows(&f), rows);
}

#[test]
fn a_changed_row_is_reported_with_what_changed_and_only_that_is_written() {
    let f = Fixture::new();
    people(&f);
    apply(&f, &csv(&company()));

    // Rows: IT-DIR 2, IT-DEP 3, DEV-LEAD 4, DEV-1 5, DEV-2 6.
    let mut edited = company();
    edited[2].share = "1";
    edited[3] = R {
        title: "Starszy developer",
        person: "darek",
        share: "0,5",
        ..edited[3].clone()
    };
    let report = apply(&f, &csv(&edited));

    assert!(report.applied, "{:?}", report.errors);
    let by_row = |n: u32| report.rows.iter().find(|r| r.row == n).unwrap();
    let share = by_row(4);
    assert_eq!(share.status, RowStatus::Changed);
    assert_eq!(share.changes.len(), 1);
    assert_eq!(
        (
            share.changes[0].entity,
            share.changes[0].field,
            share.changes[0].before.as_deref(),
            share.changes[0].after.as_deref()
        ),
        ("assignment", "share", Some("0.5"), Some("1"))
    );
    let senior = by_row(5);
    assert_eq!(
        senior.status,
        RowStatus::Added,
        "a row that makes a holder is an added row"
    );
    let fields: Vec<_> = senior.changes.iter().map(|c| (c.entity, c.field)).collect();
    assert!(
        fields.contains(&("position", "name")) && fields.contains(&("assignment", "created")),
        "{fields:?}"
    );
    assert_eq!(report.counts.unchanged, 3);
    assert_eq!(position_by_code(&f, "DEV-1").name, "Starszy developer");
    // Darek now holds two positions, one of them primary, and the report says the sum is over.
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.kind == IssueKind::ShareOverbooked && w.row == 5),
        "{:?}",
        report.warnings
    );
    let darek_id: String = f
        .pool
        .read()
        .unwrap()
        .query_row(
            "SELECT id FROM user_accounts WHERE username = 'darek'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let held: Vec<_> = svc::list_assignments_at(&f.pool, ORG, today())
        .unwrap()
        .into_iter()
        .filter(|a| a.subject == Subject::User(darek_id.clone()))
        .collect();
    assert_eq!(held.len(), 2);
    assert_eq!(held.iter().filter(|a| a.is_primary).count(), 1);
}

#[test]
fn an_upsert_never_ends_what_the_file_leaves_out() {
    let f = Fixture::new();
    people(&f);
    apply(&f, &csv(&company()));

    // A file with only the IT head: nothing else of the structure is touched.
    let report = apply(&f, &csv(&company()[..1]));

    assert!(report.applied);
    assert_eq!(report.counts.unchanged, 1);
    assert_eq!(units(&f).len(), 2);
    assert_eq!(
        svc::list_positions_at(&f.pool, ORG, today()).unwrap().len(),
        5
    );
    assert_eq!(
        svc::list_assignments_at(&f.pool, ORG, today())
            .unwrap()
            .len(),
        4
    );
}

#[test]
fn an_upsert_adds_a_holder_next_to_the_one_the_file_does_not_name_and_says_so() {
    let f = Fixture::new();
    people(&f);
    apply(&f, &csv(&company()));

    let mut edited = company();
    edited[4].person = "bartek";
    let report = apply(&f, &csv(&edited));

    assert!(report.applied, "{:?}", report.errors);
    assert_eq!(
        svc::list_assignments_at(&f.pool, ORG, today())
            .unwrap()
            .len(),
        5
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.kind == IssueKind::PositionHasOtherHolder && w.row == 6),
        "{:?}",
        report.warnings
    );
}

// ---------------------------------------------------------------------------
// Replace
// ---------------------------------------------------------------------------

fn replace(f: &Fixture, rows: &[R]) -> Report {
    let file = csv(rows);
    let request = ImportRequest {
        confirm_ended: true,
        ..request(&file, Mode::Replace)
    };
    run(&f.pool, &f.ctx(), &request, true).unwrap()
}

#[test]
fn a_replace_ends_what_the_file_leaves_out_and_clears_what_it_leaves_empty() {
    let f = Fixture::new();
    people(&f);
    apply(&f, &csv(&company()));

    // The file keeps IT and Dev but drops DEV-2 (and Darek with it), drops
    // the deputy, and states no manager for DEV-1 any more. Everything
    // was created today, so it ends tomorrow: a position cannot end on the
    // day it starts.
    let mut edited = company();
    edited.remove(4);
    edited.remove(1);
    edited[2].manager = "";
    let file = csv(&edited);
    let report = run(
        &f.pool,
        &f.ctx(),
        &ImportRequest {
            as_of: Some(day(1)),
            confirm_ended: true,
            ..request(&file, Mode::Replace)
        },
        true,
    )
    .unwrap();

    assert!(report.applied, "{:?}", report.errors);
    assert_eq!(report.counts.positions_ended, 2, "{:?}", report.counts);
    assert_eq!(
        report.counts.assignments_ended, 2,
        "the holders of the two ended positions leave with them"
    );
    let codes_on = |d| {
        svc::list_positions_at(&f.pool, ORG, d)
            .unwrap()
            .into_iter()
            .filter_map(|p| p.code)
            .collect::<Vec<_>>()
    };
    assert!(
        codes_on(day(0)).contains(&"DEV-2".to_string()),
        "history is kept"
    );
    assert!(!codes_on(day(1)).contains(&"DEV-2".to_string()));
    assert!(!codes_on(day(1)).contains(&"IT-DEP".to_string()));
    assert!(svc::list_deputy_heads_at(&f.pool, ORG, day(1))
        .unwrap()
        .is_empty());
    let lines = svc::list_reporting_lines_at(&f.pool, ORG, day(1)).unwrap();
    let dev1 = position_by_code(&f, "DEV-1");
    assert!(
        !lines
            .iter()
            .any(|l| l.position_id == dev1.position_id && l.kind == LineKind::Primary),
        "an empty manager cell clears in a replace"
    );
}

#[test]
fn a_replace_ends_a_unit_the_file_leaves_out_with_its_positions_and_head() {
    let f = Fixture::new();
    people(&f);
    apply(&f, &csv(&company()));

    let file = csv(&company()[..2]);
    let report = run(
        &f.pool,
        &f.ctx(),
        &ImportRequest {
            as_of: Some(day(1)),
            confirm_ended: true,
            ..request(&file, Mode::Replace)
        },
        true,
    )
    .unwrap();

    assert!(report.applied, "{:?}", report.errors);
    assert_eq!(report.counts.units_ended, 1);
    assert_eq!(report.counts.positions_ended, 3);
    let on = |d| svc::list_units_at(&f.pool, ORG, d).unwrap().len();
    assert_eq!((on(day(0)), on(day(1))), (2, 1));
}

#[test]
fn a_replace_that_matches_nothing_is_refused_instead_of_ending_everything() {
    let f = Fixture::new();
    people(&f);
    apply(&f, &csv(&company()));
    let other = [R {
        unit: "X",
        name: "Inna firma",
        pos: "X-1",
        title: "Prezes",
        head: "tak",
        ..R::default()
    }];

    let report = replace(&f, &other);

    assert!(!report.applied);
    assert_eq!(
        kinds(&report.errors),
        [(0, IssueKind::ReplaceMatchesNothing)]
    );
    assert_eq!(units(&f).len(), 2);
}

#[test]
fn a_replace_keeps_what_the_file_mentions_even_on_a_row_it_could_not_use() {
    let f = Fixture::new();
    people(&f);
    apply(&f, &csv(&company()));
    // DEV-2 is on a row with a bad share: it is not "left out".
    let mut edited = company();
    edited[4].share = "much";
    let report = replace(&f, &edited);

    assert!(!report.applied);
    assert_eq!(kinds(&report.errors), [(6, IssueKind::InvalidShare)]);
    assert_eq!(report.counts.positions_ended, 0, "{:?}", report.counts);
}

// ---------------------------------------------------------------------------
// Mistakes in the file
// ---------------------------------------------------------------------------

/// Runs the rows as a dry run and returns the errors as (row, kind) — a
/// stable way to say "this file has exactly these faults".
fn faults(f: &Fixture, rows: &[R]) -> Vec<(u32, IssueKind)> {
    kinds(&dry(f, &csv(rows)).errors)
}

#[test]
fn an_unknown_login_is_an_error_with_the_closest_account_as_the_suggestion() {
    let f = Fixture::new();
    member(&f, "j.kowalski", "Jan Kowalski");
    member(&f, "a.nowak", "Anna Nowak");
    let rows = [
        R {
            unit: "IT",
            name: "IT",
            pos: "IT-1",
            title: "Dyrektor",
            head: "tak",
            ..R::default()
        },
        R {
            unit: "IT",
            pos: "IT-2",
            title: "Specjalista ds. wdrożeń",
            manager: "IT-1",
            person: "j.kowlski",
            ..R::default()
        },
    ];

    let report = dry(&f, &csv(&rows));

    assert_eq!(kinds(&report.errors), [(3, IssueKind::UnknownPerson)]);
    let error = &report.errors[0];
    assert_eq!(error.column, Some(Column::Person));
    assert_eq!(error.value.as_deref(), Some("j.kowlski"));
    assert_eq!(error.suggestion.as_deref(), Some("j.kowalski"));
    assert_eq!(error.suggestion_label.as_deref(), Some("Jan Kowalski"));
    // The rest of the file is still planned: the preview has the unit and both positions.
    let preview = report.preview.unwrap();
    assert!(report.preview_partial);
    assert_eq!(preview.positions.len(), 2);
    assert_eq!(
        preview.assignments.len(),
        0,
        "the unknown person is left out of the preview"
    );
    assert_eq!(
        report.rows.iter().find(|r| r.row == 3).unwrap().status,
        RowStatus::Error
    );
}

#[test]
fn a_unit_parent_cycle_names_the_rows_and_the_one_to_fix() {
    let f = Fixture::new();
    let rows = [
        R {
            unit: "DEVOPS",
            name: "Zespół DevOps",
            parent: "REAL",
            ..R::default()
        },
        R {
            unit: "REAL",
            name: "Dział Realizacji",
            parent: "DEVOPS",
            ..R::default()
        },
    ];

    let report = dry(&f, &csv(&rows));

    assert_eq!(kinds(&report.errors), [(3, IssueKind::UnitCycle)]);
    assert_eq!(report.errors[0].rows, [2, 3]);
    assert_eq!(report.errors[0].column, Some(Column::ParentCode));
    assert!(
        faults(
            &f,
            &[R {
                unit: "A",
                name: "A",
                parent: "A",
                ..R::default()
            }]
        )
        .contains(&(2, IssueKind::UnitCycle)),
        "a unit that is its own parent"
    );
}

#[test]
fn a_position_manager_cycle_is_an_error_on_the_last_row_of_it() {
    let f = Fixture::new();
    let rows = [
        R {
            unit: "U",
            name: "U",
            pos: "A",
            title: "A",
            manager: "B",
            ..R::default()
        },
        R {
            unit: "U",
            pos: "B",
            title: "B",
            manager: "C",
            ..R::default()
        },
        R {
            unit: "U",
            pos: "C",
            title: "C",
            manager: "A",
            ..R::default()
        },
    ];
    let report = dry(&f, &csv(&rows));
    assert_eq!(kinds(&report.errors), [(4, IssueKind::ReportingCycle)]);
    assert_eq!(report.errors[0].rows, [2, 3, 4]);
}

#[test]
fn a_missing_parent_unit_and_a_missing_manager_are_errors_that_name_the_code() {
    let f = Fixture::new();
    let rows = [R {
        unit: "U",
        name: "U",
        parent: "GHOST",
        pos: "P",
        title: "P",
        manager: "NOBODY",
        ..R::default()
    }];
    let report = dry(&f, &csv(&rows));
    assert_eq!(
        kinds(&report.errors),
        [
            (2, IssueKind::MissingParentUnit),
            (2, IssueKind::MissingManager)
        ]
    );
    assert_eq!(report.errors[0].value.as_deref(), Some("GHOST"));
    assert_eq!(report.errors[1].value.as_deref(), Some("NOBODY"));
}

#[test]
fn two_heads_two_primaries_and_a_deputy_order_used_twice_are_errors() {
    let f = Fixture::new();
    member(&f, "anna", "Anna Nowak");
    let rows = [
        R {
            unit: "U",
            name: "U",
            pos: "H1",
            title: "H1",
            head: "tak",
            ..R::default()
        },
        R {
            unit: "U",
            pos: "H2",
            title: "H2",
            head: "tak",
            ..R::default()
        },
        R {
            unit: "U",
            pos: "D1",
            title: "D1",
            deputy: "1",
            ..R::default()
        },
        R {
            unit: "U",
            pos: "D2",
            title: "D2",
            deputy: "1",
            ..R::default()
        },
        R {
            unit: "V",
            name: "V",
            pos: "A",
            title: "A",
            person: "anna",
            primary: "tak",
            ..R::default()
        },
        R {
            unit: "V",
            pos: "B",
            title: "B",
            person: "anna",
            primary: "tak",
            ..R::default()
        },
    ];
    let report = dry(&f, &csv(&rows));
    assert_eq!(
        kinds(&report.errors),
        [
            (3, IssueKind::TwoHeads),
            (5, IssueKind::DuplicateDeputyOrder),
            (7, IssueKind::TwoPrimaryPositions),
        ]
    );
    assert_eq!(report.errors[0].rows, [2, 3]);
    assert_eq!(report.errors[2].rows, [6, 7]);
}

#[test]
fn cells_that_do_not_parse_are_errors_of_their_row_and_column() {
    let f = Fixture::new();
    let rows = [
        R {
            unit: "U",
            name: "U",
            pos: "P",
            title: "P",
            share: "2",
            ..R::default()
        },
        R {
            unit: "U",
            pos: "Q",
            title: "Q",
            staff: "może",
            ..R::default()
        },
        R {
            unit: "U",
            pos: "R",
            title: "R",
            from: "jutro",
            ..R::default()
        },
        R {
            unit: "U",
            pos: "S",
            title: "S",
            deputy: "pierwszy",
            ..R::default()
        },
    ];
    let report = dry(&f, &csv(&rows));
    assert_eq!(
        report
            .errors
            .iter()
            .map(|e| (e.row, e.kind, e.column))
            .collect::<Vec<_>>(),
        [
            (2, IssueKind::InvalidShare, Some(Column::Share)),
            (3, IssueKind::InvalidBoolean, Some(Column::Staff)),
            (4, IssueKind::InvalidDate, Some(Column::From)),
            (5, IssueKind::InvalidNumber, Some(Column::DeputyOrder)),
        ]
    );
    assert_eq!(report.errors[0].value.as_deref(), Some("2"));
}

#[test]
fn conflicting_rows_missing_codes_and_names_and_an_unknown_type_are_errors() {
    let f = Fixture::new();
    let rows = [
        R {
            unit: "U",
            name: "Jedna",
            pos: "P",
            title: "P",
            ..R::default()
        },
        R {
            unit: "U",
            name: "Druga",
            pos: "Q",
            title: "Q",
            ..R::default()
        },
        R {
            unit: "",
            name: "Bez kodu",
            ..R::default()
        },
        R {
            unit: "V",
            ..R::default()
        },
        R {
            unit: "W",
            name: "W",
            kind: "Oddziałek",
            pos: "",
            title: "Bez kodu stanowiska",
            ..R::default()
        },
    ];
    let report = dry(&f, &csv(&rows));
    assert_eq!(
        kinds(&report.errors),
        [
            (3, IssueKind::ConflictingValues),
            (4, IssueKind::MissingUnitCode),
            (5, IssueKind::MissingUnitName),
            (6, IssueKind::MissingPositionCode),
            (6, IssueKind::UnknownUnitType),
        ]
    );
    assert_eq!(report.errors[0].rows, [2, 3]);
}

#[test]
fn an_unknown_unit_type_suggests_the_closest_name() {
    let f = Fixture::new();
    svc::create_unit_type(&f.pool, &f.ctx(), "Dział", None, None).unwrap();
    let rows = [R {
        unit: "U",
        name: "U",
        kind: "Dzial",
        ..R::default()
    }];
    let report = dry(&f, &csv(&rows));
    assert_eq!(kinds(&report.errors), [(2, IssueKind::UnknownUnitType)]);
    assert_eq!(report.errors[0].suggestion.as_deref(), Some("Dział"));
    let known = [R {
        unit: "U",
        name: "U",
        kind: "dział",
        ..R::default()
    }];
    assert!(
        dry(&f, &csv(&known)).errors.is_empty(),
        "a type matches without case"
    );
}

#[test]
fn a_staff_position_cannot_be_named_as_a_manager() {
    let f = Fixture::new();
    let rows = [
        R {
            unit: "U",
            name: "U",
            pos: "ASYSTENT",
            title: "Asystent",
            staff: "tak",
            ..R::default()
        },
        R {
            unit: "U",
            pos: "P",
            title: "P",
            manager: "ASYSTENT",
            ..R::default()
        },
    ];
    assert_eq!(faults(&f, &rows), [(3, IssueKind::StaffManager)]);
}

#[test]
fn a_person_two_accounts_answer_to_is_ambiguous_not_guessed() {
    let f = Fixture::new();
    member(&f, "jan1", "Jan Kowalski");
    member(&f, "jan2", "Jan Kowalski");
    let rows = [R {
        unit: "U",
        name: "U",
        pos: "P",
        title: "P",
        person: "Jan Kowalski",
        ..R::default()
    }];
    assert_eq!(faults(&f, &rows), [(2, IssueKind::AmbiguousPerson)]);
}

#[test]
fn the_same_person_twice_on_one_position_is_an_error() {
    let f = Fixture::new();
    member(&f, "anna", "Anna Nowak");
    let rows = [
        R {
            unit: "U",
            name: "U",
            pos: "P",
            title: "P",
            person: "anna",
            ..R::default()
        },
        R {
            unit: "U",
            pos: "P",
            person: "ANNA@firma.pl",
            ..R::default()
        },
    ];
    assert_eq!(faults(&f, &rows), [(3, IssueKind::DuplicateAssignment)]);
}

#[test]
fn a_row_dated_before_today_needs_the_backdating_confirmation() {
    let f = Fixture::new();
    member(&f, "anna", "Anna Nowak");
    let past = s(day(-30));
    let rows = [R {
        unit: "U",
        name: "U",
        pos: "P",
        title: "P",
        person: "anna",
        from: &past,
        ..R::default()
    }];
    let file = csv(&rows);

    let refused = run(&f.pool, &f.ctx(), &request(&file, Mode::Upsert), true).unwrap();
    assert!(!refused.applied);
    assert_eq!(
        kinds(&refused.errors),
        [(2, IssueKind::BackdatedConfirmationRequired)]
    );
    assert_eq!(refused.errors[0].column, Some(Column::From));
    assert_eq!(structure_rows(&f), 0);

    let confirmed = run(&f.pool, &f.confirmed(), &request(&file, Mode::Upsert), true).unwrap();
    assert!(confirmed.applied, "{:?}", confirmed.errors);
    // The history starts where the file says: the unit existed a month ago.
    assert_eq!(svc::list_units_at(&f.pool, ORG, day(-30)).unwrap().len(), 1);
    assert_eq!(
        count(&f, "audit_log WHERE details LIKE '%\"backdated\":true%'"),
        1
    );
}

#[test]
fn a_planned_change_takes_effect_on_its_day_and_repeats_as_no_change() {
    let f = Fixture::new();
    member(&f, "anna", "Anna Nowak");
    apply(
        &f,
        &csv(&[R {
            unit: "U",
            name: "U",
            pos: "P",
            title: "Kierownik",
            person: "anna",
            ..R::default()
        }]),
    );
    let planned = s(day(30));
    let rows = [R {
        unit: "U",
        pos: "P",
        title: "Dyrektor",
        from: &planned,
        ..R::default()
    }];

    let first = apply(&f, &csv(&rows));
    assert!(first.applied, "{:?}", first.errors);
    assert_eq!(first.counts.changed, 1);
    assert_eq!(
        first.preview_at,
        day(30),
        "the preview shows the day the change takes effect"
    );
    let names = |d| {
        svc::list_positions_at(&f.pool, ORG, d).unwrap()[0]
            .name
            .clone()
    };
    assert_eq!(names(day(0)), "Kierownik");
    assert_eq!(names(day(30)), "Dyrektor");

    let second = apply(&f, &csv(&rows));
    assert_eq!(second.counts.changed, 0, "{:?}", second.rows);
    assert_eq!(second.counts.unchanged, 1);
}

#[test]
fn a_code_that_belongs_to_a_unit_that_does_not_exist_that_day_is_reserved() {
    let f = Fixture::new();
    let unit = svc::create_unit(
        &f.pool,
        &f.ctx(),
        &NewUnit {
            name: "Stary".into(),
            code: Some("OLD".into()),
            type_id: None,
            parent_unit_id: None,
            color: None,
            valid_from: day(0),
            valid_to: None,
        },
    )
    .unwrap()
    .value;
    svc::end_unit(&f.pool, &f.ctx(), &unit.unit_id, day(5)).unwrap();
    let later = s(day(10));
    let rows = [R {
        unit: "OLD",
        name: "Nowy",
        from: &later,
        ..R::default()
    }];
    assert_eq!(faults(&f, &rows), [(2, IssueKind::CodeReserved)]);
}

#[test]
fn a_unit_without_a_head_is_a_warning_not_an_error() {
    let f = Fixture::new();
    let rows = [R {
        unit: "U",
        name: "U",
        pos: "P",
        title: "P",
        ..R::default()
    }];
    let report = dry(&f, &csv(&rows));
    assert!(report.errors.is_empty());
    assert_eq!(
        report
            .warnings
            .iter()
            .map(|w| (w.row, w.kind))
            .collect::<Vec<_>>(),
        [(2, IssueKind::UnitWithoutHead)]
    );
    assert!(apply(&f, &csv(&rows)).applied);
}

#[test]
fn unknown_columns_are_ignored_with_a_warning() {
    let f = Fixture::new();
    let file = b"kod jednostki;nazwa jednostki;komentarz\nU;U;co\xc5\x9b\n".to_vec();
    let report = dry(&f, &file);
    assert!(report.errors.is_empty());
    assert!(report
        .warnings
        .iter()
        .any(|w| w.kind == IssueKind::UnknownColumn && w.value.as_deref() == Some("komentarz")));
}

#[test]
fn a_file_that_cannot_be_used_is_a_typed_file_error_not_a_failure() {
    let f = Fixture::new();
    for (bytes, expected) in [
        (b"nazwa jednostki\nIT\n".to_vec(), "missing_column"),
        (
            b"kod jednostki;kod jednostki\nA;B\n".to_vec(),
            "duplicate_column",
        ),
        (Vec::new(), "empty_file"),
        (vec![b'a'; MAX_FILE_BYTES + 1], "file_too_large"),
    ] {
        let report = dry(&f, &bytes);
        assert_eq!(
            report.file_error.as_ref().map(FileError::code),
            Some(expected)
        );
        assert!(report.preview.is_none() && !report.applied && report.errors.is_empty());
    }
}

// ---------------------------------------------------------------------------
// The administrator's decisions
// ---------------------------------------------------------------------------

fn with_typo() -> Vec<R<'static>> {
    vec![
        R {
            unit: "IT",
            name: "IT",
            pos: "IT-1",
            title: "Dyrektor",
            head: "tak",
            ..R::default()
        },
        R {
            unit: "IT",
            pos: "IT-2",
            title: "Wdrożeniowiec",
            manager: "IT-1",
            person: "j.kowlski",
            ..R::default()
        },
        R {
            unit: "IT",
            pos: "IT-3",
            title: "Analityk",
            manager: "IT-1",
            person: "ghost",
            ..R::default()
        },
    ]
}

fn resolved(f: &Fixture, resolutions: &[Resolution], commit: bool) -> Report {
    let file = csv(&with_typo());
    run(
        &f.pool,
        &f.ctx(),
        &ImportRequest {
            resolutions,
            ..request(&file, Mode::Upsert)
        },
        commit,
    )
    .unwrap()
}

#[test]
fn the_decisions_on_a_dry_run_turn_its_errors_into_a_clean_apply() {
    let f = Fixture::new();
    let kowalski = member(&f, "j.kowalski", "Jan Kowalski");
    let unresolved = resolved(&f, &[], true);
    assert!(!unresolved.applied, "an error blocks the apply");
    assert_eq!(
        kinds(&unresolved.errors),
        [(3, IssueKind::UnknownPerson), (4, IssueKind::UnknownPerson)]
    );
    assert_eq!(structure_rows(&f), 0, "and nothing is written");

    let decisions = [
        Resolution {
            row: 3,
            action: ResolutionAction::UseSuggestedLogin,
            login: None,
        },
        Resolution {
            row: 4,
            action: ResolutionAction::LeaveVacant,
            login: None,
        },
    ];
    let dry_run = resolved(&f, &decisions, false);
    assert!(dry_run.errors.is_empty(), "{:?}", dry_run.errors);
    assert!(!dry_run.preview_partial);

    let applied = resolved(&f, &decisions, true);
    assert!(applied.applied, "{:?}", applied.errors);
    let held = svc::list_assignments_at(&f.pool, ORG, today()).unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].subject, Subject::User(kowalski));
    assert_eq!(
        held[0].position_id,
        position_by_code(&f, "IT-2").position_id
    );
    assert_eq!(
        svc::list_positions_at(&f.pool, ORG, today()).unwrap().len(),
        3,
        "the vacancy exists"
    );
}

#[test]
fn a_skipped_row_is_not_imported_and_what_referred_to_it_becomes_an_error() {
    let f = Fixture::new();
    let decisions = [Resolution {
        row: 3,
        action: ResolutionAction::SkipRow,
        login: None,
    }];
    let rows = [
        R {
            unit: "IT",
            name: "IT",
            pos: "IT-1",
            title: "Dyrektor",
            head: "tak",
            ..R::default()
        },
        R {
            unit: "IT",
            pos: "IT-2",
            title: "Skipped",
            manager: "IT-1",
            ..R::default()
        },
        R {
            unit: "IT",
            pos: "IT-3",
            title: "Refers",
            manager: "IT-2",
            ..R::default()
        },
    ];
    let file = csv(&rows);
    let report = run(
        &f.pool,
        &f.ctx(),
        &ImportRequest {
            resolutions: &decisions,
            ..request(&file, Mode::Upsert)
        },
        true,
    )
    .unwrap();
    assert_eq!(kinds(&report.errors), [(4, IssueKind::MissingManager)]);
    assert_eq!(report.counts.rows, 2);
}

#[test]
fn a_decision_that_does_not_fit_its_row_is_an_error_not_ignored() {
    let f = Fixture::new();
    member(&f, "anna", "Anna Nowak");
    let file = csv(&[
        R {
            unit: "IT",
            name: "IT",
            pos: "IT-1",
            title: "Dyrektor",
            head: "tak",
            person: "anna",
            ..R::default()
        },
        R {
            unit: "IT",
            pos: "IT-2",
            title: "X",
            person: "ghostly-name-with-no-neighbour",
            ..R::default()
        },
    ]);
    let decisions = [
        // Anna resolves: nothing to leave vacant or suggest.
        Resolution {
            row: 2,
            action: ResolutionAction::LeaveVacant,
            login: None,
        },
        Resolution {
            row: 2,
            action: ResolutionAction::UseSuggestedLogin,
            login: None,
        },
        // No account is close to the ghost: there is no suggestion to use.
        Resolution {
            row: 3,
            action: ResolutionAction::UseSuggestedLogin,
            login: None,
        },
        // There is no row 99.
        Resolution {
            row: 99,
            action: ResolutionAction::SkipRow,
            login: None,
        },
    ];
    let report = run(
        &f.pool,
        &f.ctx(),
        &ImportRequest {
            resolutions: &decisions,
            ..request(&file, Mode::Upsert)
        },
        true,
    )
    .unwrap();
    assert!(!report.applied);
    let refused: Vec<u32> = report
        .errors
        .iter()
        .filter(|e| e.kind == IssueKind::ResolutionNotApplicable)
        .map(|e| e.row)
        .collect();
    assert_eq!(refused, [2, 2, 3, 99]);
    assert_eq!(structure_rows(&f), 0);
}

// ---------------------------------------------------------------------------
// All or nothing
// ---------------------------------------------------------------------------

#[test]
fn one_bad_row_and_nothing_of_the_file_is_written() {
    let f = Fixture::new();
    people(&f);
    let mut rows = company();
    rows.push(R {
        unit: "DEV",
        pos: "DEV-9",
        title: "Nikt",
        person: "nobody-like-this",
        ..R::default()
    });

    // Adding the members left captures of its own; only the file's must be absent.
    let captures = "__tentaflow_core_sync_captures";
    let before = (count(&f, captures), count(&f, "audit_log"));

    let report = apply(&f, &csv(&rows));

    assert!(!report.applied);
    assert_eq!(kinds(&report.errors), [(7, IssueKind::UnknownPerson)]);
    assert_eq!(
        structure_rows(&f),
        0,
        "the six good rows are not written either"
    );
    assert_eq!(
        (count(&f, captures), count(&f, "audit_log")),
        before,
        "no capture and no audit entry of the six good rows"
    );
}

#[test]
fn an_operation_the_structure_refuses_is_reported_on_its_row_and_rolls_the_whole_file_back() {
    let f = Fixture::new();
    member(&f, "anna", "Anna Nowak");
    // The position exists from today; a holder from a month ago is before it.
    apply(
        &f,
        &csv(&[R {
            unit: "U",
            name: "U",
            pos: "P",
            title: "P",
            head: "tak",
            ..R::default()
        }]),
    );
    let past = s(day(-30));
    let rows = [
        R {
            unit: "U",
            pos: "P",
            title: "P",
            person: "anna",
            from: &past,
            ..R::default()
        },
        R {
            unit: "V",
            name: "V",
            pos: "Q",
            title: "Q",
            head: "tak",
            ..R::default()
        },
    ];
    let file = csv(&rows);

    let report = run(&f.pool, &f.confirmed(), &request(&file, Mode::Upsert), true).unwrap();

    assert!(!report.applied);
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert_eq!(report.errors[0].kind, IssueKind::Rejected);
    assert_eq!(report.errors[0].row, 2);
    assert_eq!(report.errors[0].code, Some("outside_validity"));
    assert_eq!(units(&f).len(), 1, "unit V of the good row is not there");
}

#[test]
fn a_dry_run_and_an_apply_of_the_same_file_agree() {
    let f = Fixture::new();
    people(&f);
    let file = csv(&company());
    let dry_run = dry(&f, &file);
    let applied = apply(&f, &file);
    assert_eq!(dry_run.counts, applied.counts);
    assert_eq!(dry_run.rows.len(), applied.rows.len());
    assert_eq!(
        dry_run.preview.as_ref().unwrap().positions.len(),
        applied.preview.as_ref().unwrap().positions.len()
    );
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

fn export(f: &Fixture, format: FileFormat, include_contact: bool) -> export::ExportFile {
    export::export_structure(
        &f.pool,
        ORG,
        format,
        None,
        include_contact,
        columns::HeaderLanguage::Polish,
    )
    .unwrap()
}

#[test]
fn an_export_of_the_structure_imports_as_no_change() {
    let f = Fixture::new();
    people(&f);
    apply(&f, &csv(&company()));

    for format in [FileFormat::Csv, FileFormat::Xlsx] {
        let file = export(&f, format, true);
        let report = run(
            &f.pool,
            &f.ctx(),
            &ImportRequest {
                format,
                ..request(&file.bytes, Mode::Upsert)
            },
            true,
        )
        .unwrap();
        assert!(report.errors.is_empty(), "{format:?}: {:?}", report.errors);
        assert_eq!(
            report.counts.unchanged, report.counts.rows,
            "{format:?}: {:?}",
            report.rows
        );
        assert!(report.rows.is_empty(), "{format:?}");
        // A replace of the export does not end anything either.
        let replaced = run(
            &f.pool,
            &f.ctx(),
            &ImportRequest {
                format,
                ..request(&file.bytes, Mode::Replace)
            },
            true,
        )
        .unwrap();
        assert!(
            replaced.errors.is_empty(),
            "{format:?}: {:?}",
            replaced.errors
        );
        assert_eq!(
            (
                replaced.counts.units_ended,
                replaced.counts.positions_ended,
                replaced.counts.assignments_ended,
                replaced.counts.changed
            ),
            (0, 0, 0, 0),
            "{format:?}"
        );
    }
}

#[test]
fn a_structure_built_in_the_editor_exports_with_derived_codes_and_round_trips() {
    let f = Fixture::new();
    let anna = member(&f, "anna", "Anna Nowak");
    let board = f.unit("Zarząd", None);
    let it = f.unit("IT", Some(&board.unit_id));
    let ceo = f.position(&board, "Prezes", None);
    let cto = f.position(&it, "CTO", Some(&ceo));
    f.position_with(&board, "Asystent", Some(&ceo), true);
    f.assign_user(&ceo, &anna, 1.0, None).unwrap();
    svc::set_head(
        &f.pool,
        &f.ctx(),
        &board.unit_id,
        Some(&ceo.position_id),
        day(0),
    )
    .unwrap();
    svc::set_head(
        &f.pool,
        &f.ctx(),
        &it.unit_id,
        Some(&cto.position_id),
        day(0),
    )
    .unwrap();

    let file = export(&f, FileFormat::Csv, true);
    let text = String::from_utf8(file.bytes.clone()).unwrap();
    assert!(text.starts_with('\u{feff}'), "a BOM, so Excel reads UTF-8");
    let derived = format!("P-{}", &ceo.position_id.replace('-', "")[..8]);
    assert!(
        text.contains(&derived),
        "no code stored: {derived} is derived from the id\n{text}"
    );

    let report = apply(&f, &file.bytes);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(
        report.counts.unchanged, report.counts.rows,
        "{:?}",
        report.rows
    );
}

#[test]
fn a_member_who_is_not_an_administrator_gets_no_logins_and_no_emails() {
    let f = Fixture::new();
    people(&f);
    apply(&f, &csv(&company()));

    let admin_text = String::from_utf8(export(&f, FileFormat::Csv, true).bytes).unwrap();
    let member_text = String::from_utf8(export(&f, FileFormat::Csv, false).bytes).unwrap();
    let header = |text: &str| {
        text.trim_start_matches('\u{feff}')
            .lines()
            .next()
            .unwrap()
            .to_string()
    };
    assert!(
        header(&admin_text).contains("login/e-mail osoby")
            && header(&admin_text).contains(";e-mail;")
    );
    assert!(
        !header(&member_text).contains("login")
            && !header(&member_text).split(';').any(|h| h == "e-mail")
    );
    assert!(admin_text.contains("anna@firma.pl") && admin_text.contains(";anna;"));
    assert!(!member_text.contains("anna@firma.pl"), "no address");
    assert!(!member_text.contains(";anna;"), "no login");
    assert!(
        member_text.contains("Anna Nowak"),
        "the name is what the tree already shows"
    );

    let xlsx = export(&f, FileFormat::Xlsx, false);
    let table = parse::read(FileFormat::Xlsx, &xlsx.bytes).unwrap();
    assert!(
        !table
            .headers
            .iter()
            .any(|h| h.contains("login") || h == "e-mail"),
        "{:?}",
        table.headers
    );
}

#[test]
fn an_export_uses_the_headers_the_import_reads_and_the_org_day() {
    let f = Fixture::new();
    people(&f);
    apply(&f, &csv(&company()));
    let file = export(&f, FileFormat::Csv, true);
    assert_eq!(file.file_name, format!("org-structure-{}.csv", s(today())));
    assert_eq!(file.mime, "text/csv; charset=utf-8");
    let table = parse::read(FileFormat::Csv, &file.bytes).unwrap();
    let mapping = columns::Mapping::from_headers(&table.headers).unwrap();
    assert!(
        mapping.ignored.is_empty(),
        "every exported header is a known column"
    );
    let row = |n: usize, column| mapping.cell(column, &table.rows[n].cells).to_string();
    // The tree order: IT first, its head first, then the deputy.
    assert_eq!(row(0, Column::UnitCode), "IT");
    assert_eq!(row(0, Column::PositionCode), "IT-DIR");
    assert_eq!(row(0, Column::Head), "tak");
    assert_eq!(row(1, Column::PositionCode), "IT-DEP");
    assert_eq!(row(1, Column::DeputyOrder), "1");
    assert_eq!(row(2, Column::ParentCode), "IT");
    assert_eq!(row(2, Column::Manager), "IT-DIR");
    assert_eq!(row(2, Column::Share), "0,5");
    assert_eq!(row(0, Column::From), s(today()));
    let vacant = table
        .rows
        .iter()
        .find(|r| mapping.cell(Column::PositionCode, &r.cells) == "DEV-1")
        .unwrap();
    assert_eq!(
        mapping.cell(Column::Person, &vacant.cells),
        "",
        "a vacancy is a row without a person"
    );

    let xlsx = export(&f, FileFormat::Xlsx, true);
    let xtable = parse::read(FileFormat::Xlsx, &xlsx.bytes).unwrap();
    assert_eq!(xtable.headers, table.headers);
    assert_eq!(xtable.rows.len(), table.rows.len());
    let xmapping = columns::Mapping::from_headers(&xtable.headers).unwrap();
    assert_eq!(
        xmapping.cell(Column::Share, &xtable.rows[2].cells),
        "0.5",
        "a number cell"
    );
}

#[test]
fn a_name_that_looks_like_a_formula_is_written_inert_and_read_back_as_typed() {
    let f = Fixture::new();
    let unit = f.unit("=HYPERLINK(\"http://x\")", None);
    let boss = f.position(&unit, "-Szef", None);
    svc::set_head(
        &f.pool,
        &f.ctx(),
        &unit.unit_id,
        Some(&boss.position_id),
        day(0),
    )
    .unwrap();

    let file = export(&f, FileFormat::Csv, true);
    let text = String::from_utf8(file.bytes.clone()).unwrap();
    assert!(
        text.contains("'=HYPERLINK") && text.contains("'-Szef"),
        "{text}"
    );

    let report = apply(&f, &file.bytes);
    assert_eq!(
        report.counts.unchanged, report.counts.rows,
        "{:?} {:?}",
        report.rows, report.errors
    );
    assert_eq!(units(&f)[0].name, "=HYPERLINK(\"http://x\")");
}

#[test]
fn full_width_and_padded_formula_names_are_written_inert_and_read_back_as_typed() {
    let f = Fixture::new();
    f.unit("\u{ff1d}HYPERLINK(\"http://x\")", None);
    f.unit("\u{ff20}SUM(A1)", None);
    f.unit("\u{a0}+1", None);

    let file = export(&f, FileFormat::Csv, true);
    let text = String::from_utf8(file.bytes.clone()).unwrap();
    assert!(
        text.contains("'\u{ff1d}HYPERLINK") && text.contains("'\u{ff20}SUM"),
        "{text}"
    );
    assert!(
        !text.contains(";\u{ff1d}HYPERLINK") && !text.contains(";\u{ff20}SUM"),
        "{text}"
    );

    let report = apply(&f, &file.bytes);
    assert_eq!(
        report.counts.unchanged, report.counts.rows,
        "{:?} {:?}",
        report.rows, report.errors
    );
    let names: Vec<String> = units(&f).into_iter().map(|u| u.name).collect();
    assert!(
        names.contains(&"\u{ff1d}HYPERLINK(\"http://x\")".to_string()),
        "{names:?}"
    );
    assert!(names.contains(&"\u{ff20}SUM(A1)".to_string()), "{names:?}");
}

#[test]
fn the_error_report_lists_row_column_kind_and_suggestion() {
    let f = Fixture::new();
    member(&f, "j.kowalski", "Jan Kowalski");
    let report = dry(&f, &csv(&with_typo()));
    let file = export::export_errors(&report.errors, report.as_of).unwrap();
    let text = String::from_utf8(file.bytes).unwrap();
    let lines: Vec<&str> = text.trim_start_matches('\u{feff}').lines().collect();
    assert_eq!(
        lines[0],
        "row;related rows;column;kind;value;message;suggestion"
    );
    assert!(
        lines[1].starts_with("3;3;login/e-mail osoby;unknown_person;j.kowlski;"),
        "{}",
        lines[1]
    );
    assert!(lines[1].ends_with(";j.kowalski"), "{}", lines[1]);
    assert_eq!(lines.len(), 3);
    assert!(file.file_name.starts_with("org-structure-import-errors-"));
}

// ---------------------------------------------------------------------------
// Files as Excel writes them
// ---------------------------------------------------------------------------

#[test]
fn an_xlsx_with_english_headers_real_dates_and_numbers_imports() {
    use chrono::Datelike;
    use rust_xlsxwriter::{ExcelDateTime, Format, Workbook};
    let f = Fixture::new();
    member(&f, "anna", "Anna Nowak");
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    for (col, header) in [
        "Unit Code",
        "Unit Name",
        "Position Code",
        "Position",
        "Head",
        "Person",
        "Share",
        "Valid From",
    ]
    .iter()
    .enumerate()
    {
        sheet.write_string(0, col as u16, *header).unwrap();
    }
    let date = ExcelDateTime::from_ymd(
        today().year() as u16,
        today().month() as u8,
        today().day() as u8,
    )
    .unwrap();
    let date_format = Format::new().set_num_format("dd.mm.yyyy");
    // A blank row in the middle and cells with padding, as people leave them.
    sheet.write_string(1, 0, " HR ").unwrap();
    sheet.write_string(1, 1, "Kadry").unwrap();
    sheet.write_string(1, 2, "HR-1").unwrap();
    sheet.write_string(1, 3, "Kierownik").unwrap();
    sheet.write_string(1, 4, "TAK").unwrap();
    sheet.write_string(1, 5, "Anna@Firma.pl ").unwrap();
    sheet.write_number(1, 6, 0.75).unwrap();
    sheet
        .write_datetime_with_format(1, 7, &date, &date_format)
        .unwrap();
    sheet.write_string(3, 0, "HR").unwrap();
    sheet.write_string(3, 2, "HR-2").unwrap();
    sheet.write_string(3, 3, "Referent").unwrap();
    let bytes = workbook.save_to_buffer().unwrap();

    let report = run(
        &f.pool,
        &f.ctx(),
        &ImportRequest {
            format: FileFormat::Xlsx,
            ..request(&bytes, Mode::Upsert)
        },
        true,
    )
    .unwrap();

    assert!(report.applied, "{:?}", report.errors);
    assert_eq!(report.counts.rows, 2);
    assert_eq!(unit_by_code(&f, "HR").name, "Kadry");
    let held = svc::list_assignments_at(&f.pool, ORG, today()).unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].share, 0.75);
    assert!(
        report.rows.iter().any(|r| r.row == 4),
        "row numbers are the sheet's, blank row included"
    );
}

// ---------------------------------------------------------------------------
// Size
// ---------------------------------------------------------------------------

#[test]
fn a_two_thousand_row_import_dry_runs_applies_and_repeats_within_bounds() {
    let f = Fixture::new();
    for n in 0..1000 {
        member(&f, &format!("user{n}"), &format!("User {n}"));
    }
    // 40 units of 50 positions; the first of each unit is its head, the
    // others report to it; every fifth position has a holder.
    let mut text = String::from(HEADER);
    let mut position = 0;
    for unit in 0..40 {
        for slot in 0..50 {
            let parent = if unit == 0 {
                String::new()
            } else {
                format!("U{}", (unit - 1) / 4)
            };
            let unit_cells = if slot == 0 {
                format!("U{unit};Jednostka {unit};{parent};")
            } else {
                format!("U{unit};;;")
            };
            let manager = if slot == 0 {
                String::new()
            } else {
                format!("P{}", position - slot)
            };
            let person = if position % 2 == 0 {
                format!("user{}", position / 2)
            } else {
                String::new()
            };
            let head = if slot == 0 { "tak" } else { "" };
            text.push_str(&format!(
                "\n{unit_cells};P{position};Stanowisko {position};nie;{head};;{person};;{manager};;"
            ));
            position += 1;
        }
    }
    let file = text.into_bytes();
    assert_eq!(position, 2000);

    let started = Instant::now();
    let dry_run = dry(&f, &file);
    let dry_took = started.elapsed();
    assert!(
        dry_run.errors.is_empty(),
        "{:?}",
        &dry_run.errors[..dry_run.errors.len().min(5)]
    );
    assert_eq!(dry_run.counts.added, 2000);
    assert_eq!(structure_rows(&f), 0);

    let started = Instant::now();
    let applied = apply(&f, &file);
    let apply_took = started.elapsed();
    assert!(applied.applied);

    let started = Instant::now();
    let again = apply(&f, &file);
    let again_took = started.elapsed();
    assert_eq!(
        again.counts.unchanged,
        2000,
        "{:?}",
        &again.rows[..again.rows.len().min(3)]
    );

    eprintln!("2000 rows: dry run {dry_took:?}, apply {apply_took:?}, re-import {again_took:?}");
    // About a second each in a debug build. The bound leaves twenty times for
    // a loaded machine and still fails on a step that went quadratic.
    for took in [dry_took, apply_took, again_took] {
        assert!(took.as_secs() < 20, "{took:?}");
    }
}

// ---------------------------------------------------------------------------
// Hostile and awkward files
// ---------------------------------------------------------------------------

fn xlsx_from_sheet_xml(sheet_xml: &str) -> Vec<u8> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    for (name, body) in [
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#,
        ),
        (
            "_rels/.rels",
            r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
        ),
        (
            "xl/workbook.xml",
            r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="S" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
        ),
    ] {
        writer.start_file(name, options).unwrap();
        writer.write_all(body.as_bytes()).unwrap();
    }
    writer
        .start_file("xl/worksheets/sheet1.xml", options)
        .unwrap();
    writer.write_all(sheet_xml.as_bytes()).unwrap();
    writer.finish().unwrap().into_inner()
}

const SHEET_HEAD: &str = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>kod jednostki</t></is></c><c r="B1" t="inlineStr"><is><t>nazwa jednostki</t></is></c></row>"#;

fn xlsx_error(f: &Fixture, bytes: &[u8]) -> Option<&'static str> {
    let report = run(
        &f.pool,
        &f.ctx(),
        &ImportRequest {
            format: FileFormat::Xlsx,
            ..request(bytes, Mode::Upsert)
        },
        false,
    )
    .unwrap();
    report.file_error.as_ref().map(FileError::code)
}

#[test]
fn a_workbook_with_one_cell_in_the_far_corner_is_refused_before_anything_is_allocated() {
    let f = Fixture::new();
    let sheet = format!(
        r#"{SHEET_HEAD}<row r="1048576"><c r="XFD1048576" t="inlineStr"><is><t>z</t></is></c></row></sheetData></worksheet>"#
    );
    assert_eq!(
        xlsx_error(&f, &xlsx_from_sheet_xml(&sheet)),
        Some("sheet_too_large")
    );
    // Too many columns on a row that is otherwise near the top.
    let wide = format!(
        r#"{SHEET_HEAD}<row r="2"><c r="CZ2" t="inlineStr"><is><t>z</t></is></c></row></sheetData></worksheet>"#
    );
    assert_eq!(
        xlsx_error(&f, &xlsx_from_sheet_xml(&wide)),
        Some("sheet_too_large")
    );
}

#[test]
fn a_workbook_that_inflates_past_the_cap_is_refused_by_what_comes_out_not_what_it_declares() {
    let f = Fixture::new();
    // ~40 MiB of spaces deflates to a few KB.
    let padding = " ".repeat(40 << 20);
    let bomb = xlsx_from_sheet_xml(&format!("{SHEET_HEAD}</sheetData></worksheet>{padding}"));
    assert!(
        bomb.len() < MAX_FILE_BYTES,
        "the bomb itself is small: {}",
        bomb.len()
    );
    assert_eq!(xlsx_error(&f, &bomb), Some("file_expands_too_large"));

    // The same file with the size in the archive's directory falsified to 1000.
    let mut lying = bomb.clone();
    let mut at = 0;
    while let Some(found) = lying[at..].windows(4).position(|w| w == b"PK\x01\x02") {
        let header = at + found;
        let name_len = u16::from_le_bytes([lying[header + 28], lying[header + 29]]) as usize;
        if &lying[header + 46..header + 46 + name_len] == b"xl/worksheets/sheet1.xml" {
            lying[header + 24..header + 28].copy_from_slice(&1000u32.to_le_bytes());
        }
        at = header + 4;
    }
    assert!(
        matches!(
            xlsx_error(&f, &lying),
            Some("file_expands_too_large" | "unreadable_file")
        ),
        "a lying directory is refused, never inflated"
    );
}

#[test]
fn data_on_other_sheets_is_reported_and_the_sheet_read_is_named() {
    use rust_xlsxwriter::Workbook;
    let f = Fixture::new();
    let mut workbook = Workbook::new();
    let first = workbook.add_worksheet();
    first.set_name("Jednostki").unwrap();
    first.write_string(0, 0, "kod jednostki").unwrap();
    first.write_string(0, 1, "nazwa jednostki").unwrap();
    first.write_string(1, 0, "U").unwrap();
    first.write_string(1, 1, "U").unwrap();
    let second = workbook.add_worksheet();
    second.set_name("Osoby").unwrap();
    second.write_string(0, 0, "kod jednostki").unwrap();
    second.write_string(1, 0, "V").unwrap();
    let bytes = workbook.save_to_buffer().unwrap();

    let report = run(
        &f.pool,
        &f.ctx(),
        &ImportRequest {
            format: FileFormat::Xlsx,
            ..request(&bytes, Mode::Upsert)
        },
        false,
    )
    .unwrap();

    assert_eq!(report.sheet.as_deref(), Some("Jednostki"));
    assert_eq!(report.counts.rows, 1, "only the first sheet is read");
    let warning = report
        .warnings
        .iter()
        .find(|w| w.kind == IssueKind::OtherSheetsIgnored)
        .unwrap();
    assert_eq!(warning.value.as_deref(), Some("Osoby"));
}

#[test]
fn text_that_is_neither_utf8_nor_safely_windows_1250_is_a_typed_encoding_error() {
    let f = Fixture::new();
    // 0x81 is not defined in Windows-1250.
    let undefined = [
        b"kod jednostki;nazwa jednostki\nA;Dzia".as_slice(),
        &[0x81],
        b"\n",
    ]
    .concat();
    // A valid UTF-8 letter next to a lone Windows-1250 one: two encodings in one file.
    let mixed = [b"kod jednostki;nazwa jednostki\nA;\xc5\x82\xf3d\xbf\n".as_slice()].concat();
    for (bytes, name) in [(undefined, "undefined byte"), (mixed, "mixed")] {
        let report = dry(&f, &bytes);
        assert_eq!(
            report.file_error.as_ref().map(FileError::code),
            Some("invalid_encoding"),
            "{name}"
        );
    }
    // Plain Windows-1250 with no UTF-8 in it still reads.
    let (cp1250, _, _) =
        encoding_rs::WINDOWS_1250.encode("kod jednostki;nazwa jednostki\nA;Łódź\n");
    assert!(dry(&f, &cp1250).file_error.is_none());
}

#[test]
fn a_row_number_is_the_row_a_spreadsheet_shows_even_after_a_cell_that_spans_lines() {
    let f = Fixture::new();
    // Row 2 has a note over three lines; row 3 is blank; row 4 has the fault.
    let file =
        "kod jednostki;nazwa jednostki;typ\nA;\"Dział\nw trzech\nliniach\";\n\nB;B;Nieznany typ\n";
    let report = dry(&f, file.as_bytes());
    assert_eq!(
        kinds(&report.errors),
        [(4, IssueKind::UnknownUnitType)],
        "{:?}",
        report.errors
    );
}

#[test]
fn a_suggested_login_is_pinned_to_the_one_the_administrator_saw() {
    let f = Fixture::new();
    member(&f, "j.kowalski", "Jan Kowalski");
    member(&f, "zzz", "Inny Człowiek");
    let file = csv(&with_typo());
    let decide = |login: &str| {
        run(
            &f.pool,
            &f.ctx(),
            &ImportRequest {
                resolutions: &[Resolution {
                    row: 3,
                    action: ResolutionAction::UseSuggestedLogin,
                    login: Some(login.to_string()),
                }],
                ..request(&file, Mode::Upsert)
            },
            false,
        )
        .unwrap()
    };
    let shown = decide("j.kowalski");
    assert!(
        !shown
            .errors
            .iter()
            .any(|e| e.kind == IssueKind::ResolutionNotApplicable),
        "{:?}",
        shown.errors
    );
    let stale = decide("someone.who.left");
    assert_eq!(
        stale
            .errors
            .iter()
            .filter(|e| e.kind == IssueKind::ResolutionNotApplicable)
            .map(|e| e.row)
            .collect::<Vec<_>>(),
        [3]
    );
}

#[test]
fn a_member_export_never_falls_back_to_the_login_for_the_name() {
    let f = Fixture::new();
    let id = member(&f, "no.display", "");
    let bosses = f.unit("U", None);
    let seat = f.position(&bosses, "Seat", None);
    f.assign_user(&seat, &id, 1.0, None).unwrap();
    let member_file = String::from_utf8(export(&f, FileFormat::Csv, false).bytes).unwrap();
    assert!(!member_file.contains("no.display"), "{member_file}");
    let admin_file = String::from_utf8(export(&f, FileFormat::Csv, true).bytes).unwrap();
    assert!(admin_file.contains("no.display"));
}

#[test]
fn a_holder_who_left_the_organization_exports_as_a_marker_not_as_a_vacancy() {
    let f = Fixture::new();
    let id = member(&f, "leaver", "Lea Ver");
    let unit = f.unit("U", None);
    let seat = f.position(&unit, "Seat", None);
    f.assign_user(&seat, &id, 1.0, None).unwrap();
    f.pool
        .write()
        .unwrap()
        .execute("DELETE FROM org_memberships WHERE user_id = ?1", [&id])
        .unwrap();

    let text = String::from_utf8(export(&f, FileFormat::Csv, true).bytes).unwrap();
    assert!(text.contains("[outside the organization:"), "{text}");

    // Imported back, the marker is an unknown person the administrator must
    // settle; a replace does not silently end the holder.
    let report = run(
        &f.pool,
        &f.ctx(),
        &ImportRequest {
            confirm_ended: true,
            ..request(text.as_bytes(), Mode::Replace)
        },
        true,
    )
    .unwrap();
    assert!(!report.applied);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.kind == IssueKind::UnknownPerson),
        "{:?}",
        report.errors
    );
    assert_eq!(
        svc::list_assignments_at(&f.pool, ORG, today())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn a_name_that_already_starts_with_a_quote_and_a_formula_character_comes_back_as_typed() {
    for name in ["'=x", "''-y", "=z", "'plain"] {
        let fixture = Fixture::new();
        fixture.unit(name, None);
        let file = export(&fixture, FileFormat::Csv, true);
        let report = apply(&fixture, &file.bytes);
        assert_eq!(
            report.counts.unchanged, report.counts.rows,
            "{name}: {:?}",
            report.rows
        );
        assert_eq!(units(&fixture)[0].name, name);
    }
}

// ---------------------------------------------------------------------------
// What a replace ends
// ---------------------------------------------------------------------------

#[test]
fn a_replace_lists_what_it_ends_and_needs_the_confirmation_to_apply() {
    let f = Fixture::new();
    people(&f);
    apply(&f, &csv(&company()));
    let mut kept = company();
    kept.remove(4); // DEV-2 and Darek with it
    let file = csv(&kept);
    let unconfirmed = ImportRequest {
        as_of: Some(day(1)),
        ..request(&file, Mode::Replace)
    };

    let refused = run(&f.pool, &f.ctx(), &unconfirmed, true).unwrap();

    assert!(!refused.applied);
    assert_eq!(
        kinds(&refused.errors),
        [(0, IssueKind::EndedConfirmationRequired)]
    );
    assert_eq!(refused.ended.len(), 1);
    let ended = &refused.ended[0];
    assert_eq!(
        (ended.kind, ended.code.as_str(), ended.name.as_str()),
        ("position", "DEV-2", "Developer")
    );
    assert_eq!(ended.holders, ["Darek Zając"]);
    assert_eq!(refused.counts.positions_ended, 1);
    assert_eq!(
        refused.counts.assignments_ended, 1,
        "the holder that leaves with the position is counted"
    );
    assert_eq!(
        svc::list_positions_at(&f.pool, ORG, day(1)).unwrap().len(),
        5,
        "nothing written"
    );

    let confirmed = run(
        &f.pool,
        &f.ctx(),
        &ImportRequest {
            confirm_ended: true,
            ..unconfirmed
        },
        true,
    )
    .unwrap();
    assert!(confirmed.applied, "{:?}", confirmed.errors);
    assert_eq!(confirmed.ended, refused.ended);
    assert_eq!(
        svc::list_positions_at(&f.pool, ORG, day(1)).unwrap().len(),
        4
    );
    // An upsert lists nothing.
    assert!(dry(&f, &csv(&kept)).ended.is_empty());
}

#[test]
fn a_replace_the_same_day_takes_back_what_started_that_day_without_an_error_flood() {
    let f = Fixture::new();
    people(&f);
    apply(&f, &csv(&company()));

    // A bad import is fixed the day it was made: everything the file leaves
    // out started today, so it is removed, not "ended" on its first day.
    let report = replace(&f, &company()[..1]);

    assert!(
        report.applied,
        "{:?}",
        &report.errors[..report.errors.len().min(5)]
    );
    assert_eq!(report.errors.len(), 0);
    assert_eq!(units(&f).len(), 1);
    assert_eq!(
        svc::list_positions_at(&f.pool, ORG, today()).unwrap().len(),
        1
    );
    assert_eq!(
        svc::list_assignments_at(&f.pool, ORG, today())
            .unwrap()
            .len(),
        1
    );
    assert!(svc::list_deputy_heads_at(&f.pool, ORG, today())
        .unwrap()
        .is_empty());
    assert_eq!(report.counts.units_ended, 1);
    assert_eq!(report.counts.positions_ended, 4);
    assert_eq!(report.counts.assignments_ended, 3);
    // Nothing is left dangling: the structure passes its own integrity check.
    assert!(svc::integrity_report(&f.pool, ORG, Some(today()))
        .unwrap()
        .is_empty());
}

#[test]
fn the_counts_agree_with_each_other_even_when_rows_have_errors_and_rows_carry_their_cells() {
    let f = Fixture::new();
    member(&f, "j.kowalski", "Jan Kowalski");
    let report = dry(&f, &csv(&with_typo()));

    // Every row is counted once by what it does; the rows with errors overlap.
    let counts = &report.counts;
    assert_eq!(
        counts.added + counts.changed + counts.unchanged,
        counts.rows
    );
    assert_eq!(counts.rows, 3);
    assert!(counts.added > 0 && counts.units_added > 0 && counts.positions_added > 0);
    assert_eq!((counts.errors, counts.issues), (2, 2));

    // The row shows what the file has in it, by header, empty cells left out.
    let row = report.rows.iter().find(|r| r.row == 3).unwrap();
    assert_eq!(
        (row.status, row.effect),
        (RowStatus::Error, RowStatus::Added)
    );
    let cell = |header: &str| {
        row.cells
            .iter()
            .find(|(h, _)| h == header)
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(cell("kod stanowiska"), Some("IT-2"));
    assert_eq!(cell("login/e-mail osoby"), Some("j.kowlski"));
    assert_eq!(cell("kod nadrzędnej"), None);
}

// ---------------------------------------------------------------------------
// Header language
// ---------------------------------------------------------------------------

#[test]
fn the_export_header_follows_the_readers_language_and_either_one_imports_as_no_change() {
    use columns::HeaderLanguage as L;
    assert_eq!(L::of_preference(None), L::Polish);
    assert_eq!(L::of_preference(Some("pl")), L::Polish);
    assert_eq!(L::of_preference(Some("PL-pl")), L::Polish);
    for other in ["en", "de", "fr", "es", "en-GB"] {
        assert_eq!(L::of_preference(Some(other)), L::English, "{other}");
    }

    let f = Fixture::new();
    people(&f);
    apply(&f, &csv(&company()));
    let first_line = |bytes: &[u8]| {
        String::from_utf8(bytes.to_vec())
            .unwrap()
            .trim_start_matches('\u{feff}')
            .lines()
            .next()
            .unwrap()
            .to_string()
    };
    for (language, format) in [
        (L::English, FileFormat::Csv),
        (L::English, FileFormat::Xlsx),
        (L::Polish, FileFormat::Csv),
    ] {
        let file = export::export_structure(&f.pool, ORG, format, None, true, language).unwrap();
        let headers: Vec<String> = match format {
            FileFormat::Csv => first_line(&file.bytes)
                .split(';')
                .map(str::to_string)
                .collect(),
            FileFormat::Xlsx => parse::read(FileFormat::Xlsx, &file.bytes).unwrap().headers,
        };
        let expected: Vec<&str> = columns::COLUMNS
            .iter()
            .map(|spec| match language {
                L::Polish => spec.header,
                L::English => spec.header_en,
            })
            .collect();
        assert_eq!(headers, expected, "{language:?} {format:?}");
        assert_eq!(
            headers.iter().any(|h| h == "kod jednostki"),
            language == L::Polish
        );
        let report = run(
            &f.pool,
            &f.ctx(),
            &ImportRequest {
                format,
                ..request(&file.bytes, Mode::Upsert)
            },
            true,
        )
        .unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(
            report.counts.unchanged, report.counts.rows,
            "{language:?} {format:?} {:?}",
            report.rows
        );
    }
}

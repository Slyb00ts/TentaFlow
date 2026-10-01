//! Import and export of the structure as a spreadsheet (docs §2.5).
//!
//! The file has one row per holder of a position (a vacant position and a
//! unit without positions get a row of their own); `columns` lists the
//! columns, Polish and English. An import is always tried first: `run` with
//! `commit == false` executes every operation the file asks for in a
//! transaction it then drops, so the report — counts, per-row outcomes, errors
//! with row numbers, the resulting tree — is what an apply would produce.
//! Apply is the same run, kept only when the file has no error.
//!
//! Modes. `Upsert` (the default) makes what the file says true and touches
//! nothing else: a position, a person or a link the file does not mention is
//! left as it is, and an empty cell means "not stated", not "clear". `Replace`
//! makes the file the whole structure: units, positions and holders it does
//! not mention end on the import day, and an empty cell clears.
//!
//! Provenance. The structure keeps no per-field source (the plan's
//! `org_field_provenance` table is a later phase), so `import` is recorded
//! in the audit trail: ONE entry per apply (`org.structure.import`, source
//! `import`, the counts and the operations as `[action, target]`) — an entry
//! per operation would cost a hash-chain row each and hold the write
//! connection for thousands of them. The finishing step (sync captures,
//! projection, audit) runs for a dry run too, in the transaction it drops.

mod apply;
pub mod codes;
pub mod columns;
pub mod export;
pub mod parse;
pub mod people;
pub mod plan;
pub mod report;
pub mod rows;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};

use chrono::NaiveDate;
use serde_json::json;

use self::columns::{Column, Mapping};
use self::people::{Directory, Lookup};
use self::plan::{Inputs, PositionRef, Settings, Snapshots, UnitRef};
use self::report::{Counts, FileError, Issue, IssueKind, Report, RowOutcome, RowStatus};
use self::rows::Row;
use super::error::Result;
use super::query::{self, StructureView};
use super::repo::{self, BatchOutcome};
use super::types::{Subject, Warning, WriteCtx};
use super::validate;
use crate::db::DbPool;

/// One frame of the binary protocol carries the file, and the dashboard's
/// WebSocket closes the connection (1009) on a frame over 1 MiB. The file
/// leaves 100 KiB of that for the envelope, the other fields and the
/// decisions (see `MAX_RESOLUTIONS`). The screen checks the size before it
/// sends: the server cannot answer a frame the socket refused.
pub const MAX_FILE_BYTES: usize = 900 * 1024;
/// Rows per file: what an apply finishes well inside the request timeout.
pub const MAX_ROWS: usize = 5_000;
/// Decisions per request; at ~30 bytes each they fit the headroom of the frame.
pub const MAX_RESOLUTIONS: usize = 2_000;
/// Longest cell value the report echoes back for a row.
const MAX_CELL_ECHO_CHARS: usize = 200;
/// The extent of an XLSX sheet; calamine allocates every cell of it.
pub const MAX_SHEET_ROWS: usize = MAX_ROWS + 100;
pub const MAX_SHEET_COLS: usize = 64;
/// What a workbook may inflate to, counted as it inflates (a zip bomb is a few KB).
pub const MAX_EXPANDED_XLSX_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileFormat {
    Csv,
    Xlsx,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Upsert,
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionAction {
    /// Use the login the report suggested for an unknown one.
    UseSuggestedLogin,
    /// Import the row as a vacant position.
    LeaveVacant,
    /// Leave the row out of the import.
    SkipRow,
}

/// The administrator's decision about one row of a dry run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub row: u32,
    pub action: ResolutionAction,
    /// For `UseSuggestedLogin`: the login the administrator was shown. It
    /// must still be the suggestion, or the decision is refused — the file or
    /// the accounts changed since.
    pub login: Option<String>,
}

pub struct ImportRequest<'a> {
    pub format: FileFormat,
    pub bytes: &'a [u8],
    pub mode: Mode,
    /// The day rows without "od kiedy" take effect and the day the file is
    /// compared with; today in the organization's timezone by default.
    pub as_of: Option<NaiveDate>,
    pub resolutions: &'a [Resolution],
    /// The administrator saw what a replace ends and accepts it.
    pub confirm_ended: bool,
}

/// Tries the file (`commit == false`) or applies it. `ctx.confirm_backdated`
/// allows rows dated before today. Only storage failures are `Err`: a file
/// that cannot be used and the mistakes in it are in the report.
pub fn run(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    request: &ImportRequest<'_>,
    commit: bool,
) -> Result<Report> {
    let today = repo::org_today(pool, ctx.org_id)?;
    let as_of = request.as_of.unwrap_or(today);
    let failed = |error: FileError| Report::failed_file(request.format, request.mode, as_of, error);

    let table = match parse::read(request.format, request.bytes) {
        Ok(table) => table,
        Err(error) => return Ok(failed(error)),
    };
    let mapping = match Mapping::from_headers(&table.headers) {
        Ok(mapping) => mapping,
        Err(error) => return Ok(failed(error)),
    };
    let (rows, parse_issues) = rows::parse(&table, &mapping);
    let unknown_columns: Vec<Issue> = mapping
        .ignored
        .iter()
        .map(|header| {
            Issue::new(
                0,
                IssueKind::UnknownColumn,
                "this column is not used by the import",
            )
            .value(header.clone())
        })
        .collect();
    let other_sheets: Vec<Issue> = (!table.other_sheets.is_empty())
        .then(|| {
            Issue::new(
                0,
                IssueKind::OtherSheetsIgnored,
                "only one sheet is imported; the other sheets have data too",
            )
            .value(table.other_sheets.join(", "))
        })
        .into_iter()
        .collect();
    let sheet = table.sheet.clone();

    repo::run_batch(
        pool,
        ctx,
        "import",
        commit,
        repo::BatchMode {
            backdating_checked: true,
            allow_withdraw: true,
        },
        |s| {
            let tx = s.tx;
            let org_id = s.org_id;
            let directory = Directory::load(tx, org_id)?;
            let types = repo::unit_types_in(tx, org_id)?;
            let ever = plan::EverCodes::load(tx, org_id)?;
            let (rows, resolution_issues) =
                apply_resolutions(rows, request.resolutions, &directory);
            let settings = Settings {
                mode: request.mode,
                as_of,
                today,
                confirm_backdated: ctx.confirm_backdated,
            };
            let mut snapshots = Snapshots::new(tx, org_id);
            let plan = plan::build(
                &Inputs {
                    rows: &rows,
                    settings: &settings,
                    directory: &directory,
                    types: &types,
                    ever: &ever,
                },
                &mut snapshots,
            )?;
            drop(snapshots);

            let executed = apply::execute(s, &plan.ops)?;
            let view = query::structure_in(tx, org_id, Some(plan.preview_at))?;

            let mut errors = parse_issues;
            errors.extend(resolution_issues);
            errors.extend(plan.errors.iter().cloned());
            if !plan.ended.is_empty() && !request.confirm_ended {
                errors.push(Issue::new(
                    0,
                    IssueKind::EndedConfirmationRequired,
                    format!(
                    "this replace ends {} unit(s), position(s) or holder(s); confirm to end them",
                    plan.ended.len()
                ),
                ));
            }
            errors.extend(executed.failures.iter().cloned());
            errors.sort_by_key(|issue| issue.row);

            let mut warnings = unknown_columns;
            warnings.extend(other_sheets);
            warnings.extend(plan.warnings.iter().cloned());
            warnings.extend(preview_warnings(&plan, &executed, &view));
            warnings.sort_by_key(|issue| issue.row);

            let (rows_out, counts) = outcomes(&plan, &executed, &errors, &warnings, &table);
            let keep = commit && errors.is_empty();
            // Recorded for a dry run too: the batch's finishing step folds the
            // operations into this entry, and a dry run must run that step.
            s.record_summary(
            "org.structure.import",
            format!("org:{org_id}"),
            json!({
                "format": match request.format { FileFormat::Csv => "csv", FileFormat::Xlsx => "xlsx" },
                "mode": match request.mode { Mode::Upsert => "upsert", Mode::Replace => "replace" },
                "as_of": validate::format_date(as_of),
                "counts": counts_json(&counts),
                "ended": plan.ended.len(),
            }),
        );
            let preview_partial = !errors.is_empty();
            Ok(BatchOutcome {
                value: Report {
                    format: request.format,
                    mode: request.mode,
                    as_of,
                    preview_at: plan.preview_at,
                    applied: keep,
                    file_error: None,
                    sheet,
                    counts,
                    ended: plan.ended,
                    rows: rows_out,
                    errors,
                    warnings,
                    preview: Some(view),
                    preview_partial,
                },
                keep,
            })
        },
    )
}

pub fn counts_json(counts: &Counts) -> serde_json::Value {
    json!({
        "rows": counts.rows,
        "added": counts.added,
        "changed": counts.changed,
        "unchanged": counts.unchanged,
        "errors": counts.errors,
        "issues": counts.issues,
        "units_added": counts.units_added,
        "units_changed": counts.units_changed,
        "units_ended": counts.units_ended,
        "positions_added": counts.positions_added,
        "positions_changed": counts.positions_changed,
        "positions_ended": counts.positions_ended,
        "assignments_added": counts.assignments_added,
        "assignments_changed": counts.assignments_changed,
        "assignments_ended": counts.assignments_ended,
    })
}

/// The decisions the administrator took on a dry run, applied to the rows.
/// A decision that does not fit the row (the file changed since, or the row
/// has nothing to decide) is an error, not silently ignored: applying a
/// different file than the one the decisions were made on is not safe.
fn apply_resolutions(
    mut rows: Vec<Row>,
    resolutions: &[Resolution],
    directory: &Directory,
) -> (Vec<Row>, Vec<Issue>) {
    let mut issues = Vec::new();
    let mut skipped: HashSet<u32> = HashSet::new();
    let index: HashMap<u32, usize> = rows
        .iter()
        .enumerate()
        .map(|(position, row)| (row.number, position))
        .collect();
    for resolution in resolutions {
        let refused = |why: &str| {
            Issue::new(
                resolution.row,
                IssueKind::ResolutionNotApplicable,
                why.to_string(),
            )
        };
        let Some(&position) = index.get(&resolution.row) else {
            issues.push(refused("the file has no such row"));
            continue;
        };
        let row = &mut rows[position];
        let unresolved = row.person.as_deref().map(|cell| directory.resolve(cell));
        match resolution.action {
            ResolutionAction::SkipRow => {
                skipped.insert(row.number);
            }
            ResolutionAction::LeaveVacant => match unresolved {
                Some(Lookup::Unknown | Lookup::Ambiguous(_)) => {
                    row.person = None;
                    row.share = None;
                    row.primary = None;
                }
                _ => issues.push(refused("the person of this row is not in question")),
            },
            ResolutionAction::UseSuggestedLogin => match (unresolved, row.person.clone()) {
                (Some(Lookup::Unknown), Some(cell)) => match directory.suggest(&cell) {
                    Some(suggestion)
                        if resolution
                            .login
                            .as_deref()
                            .is_none_or(|shown| shown.eq_ignore_ascii_case(&suggestion.login)) =>
                    {
                        row.person = Some(suggestion.login)
                    }
                    Some(_) => issues.push(refused(
                        "the suggested login is no longer the one that was shown",
                    )),
                    None => issues.push(refused("there is no suggested login for this row")),
                },
                _ => issues.push(refused("this row has no unknown login")),
            },
        }
    }
    rows.retain(|row| !skipped.contains(&row.number));
    (rows, issues)
}

/// The warnings the resulting structure carries about what the file touches.
/// The structure's own warnings cover the whole organization; the ones
/// about people and units the file does not mention are not the import's.
fn preview_warnings(
    plan: &plan::Plan,
    executed: &apply::Executed,
    view: &StructureView,
) -> Vec<Issue> {
    // The first row that names a unit or a person is where its warning goes.
    let mut unit_rows: HashMap<String, u32> = HashMap::new();
    for (row, reference) in &plan.units {
        let id = match reference {
            UnitRef::Existing(id) => Some(id.clone()),
            UnitRef::New(key) => executed.unit_ids.get(key).cloned(),
        };
        if let Some(id) = id {
            unit_rows.entry(id).or_insert(*row);
        }
    }
    let mut subject_rows: HashMap<&Subject, u32> = HashMap::new();
    for (row, subject) in &plan.subjects {
        subject_rows.entry(subject).or_insert(*row);
    }
    let mut issues = Vec::new();
    for warning in &view.warnings {
        match warning {
            Warning::UnitWithoutHead { unit_id, .. } => {
                if let Some(row) = unit_rows.get(unit_id) {
                    issues.push(
                        Issue::new(*row, IssueKind::UnitWithoutHead, "the unit has no head")
                            .column(Column::Head),
                    );
                }
            }
            Warning::ShareOverbooked { subject, total, .. } => {
                if let Some(row) = subject_rows.get(subject) {
                    issues.push(
                        Issue::new(
                            *row,
                            IssueKind::ShareOverbooked,
                            format!("the person's shares add up to {total:.2}"),
                        )
                        .column(Column::Share)
                        .value(format!("{total:.2}")),
                    );
                }
            }
            Warning::PersonWithoutPrimary { subject, .. } => {
                if let Some(row) = subject_rows.get(subject) {
                    issues.push(
                        Issue::new(
                            *row,
                            IssueKind::PersonWithoutPrimary,
                            "the person holds several positions and none is primary",
                        )
                        .column(Column::Primary),
                    );
                }
            }
        }
    }
    issues
}

fn outcomes(
    plan: &plan::Plan,
    executed: &apply::Executed,
    errors: &[Issue],
    warnings: &[Issue],
    table: &parse::Table,
) -> (Vec<RowOutcome>, Counts) {
    let warned_rows: HashSet<u32> = warnings
        .iter()
        .flat_map(|w| w.rows.iter().copied())
        .collect();
    let raw: HashMap<u32, &parse::RawRow> = table.rows.iter().map(|r| (r.number, r)).collect();
    let cells_of = |row: u32| -> Vec<(String, String)> {
        raw.get(&row)
            .map(|raw| {
                table
                    .headers
                    .iter()
                    .zip(&raw.cells)
                    .filter(|(_, value)| !value.is_empty())
                    .map(|(header, value)| {
                        (
                            header.clone(),
                            value.chars().take(MAX_CELL_ECHO_CHARS).collect(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let failed_rows: HashSet<u32> = errors.iter().flat_map(|e| e.rows.iter().copied()).collect();
    let mut by_row: HashMap<u32, Vec<usize>> = HashMap::new();
    for (index, op) in plan.ops.iter().enumerate() {
        if executed.done[index] {
            by_row.entry(op.row).or_default().push(index);
        }
    }
    let unit_id = |reference: &UnitRef| match reference {
        UnitRef::Existing(id) => Some(id.clone()),
        UnitRef::New(key) => executed.unit_ids.get(key).cloned(),
    };
    let position_id = |reference: &PositionRef| match reference {
        PositionRef::Existing(id) => Some(id.clone()),
        PositionRef::New(key) => executed.position_ids.get(key).cloned(),
    };

    let mut counts = Counts {
        rows: plan.rows.len() as u32,
        issues: errors.len() as u32,
        ..Counts::default()
    };
    let mut rows = Vec::new();
    for info in &plan.rows {
        let ops: Vec<&plan::Op> = by_row
            .get(&info.number)
            .map(|indexes| indexes.iter().map(|i| &plan.ops[*i]).collect())
            .unwrap_or_default();
        let effect = if ops.iter().any(|op| op.created) {
            counts.added += 1;
            RowStatus::Added
        } else if !ops.is_empty() {
            counts.changed += 1;
            RowStatus::Changed
        } else {
            counts.unchanged += 1;
            RowStatus::Unchanged
        };
        let failed = failed_rows.contains(&info.number);
        if failed {
            counts.errors += 1;
        }
        if effect == RowStatus::Unchanged && !failed && !warned_rows.contains(&info.number) {
            continue;
        }
        let status = if failed { RowStatus::Error } else { effect };
        rows.push(RowOutcome {
            row: info.number,
            status,
            effect,
            cells: cells_of(info.number),
            unit_code: info.unit_code.clone(),
            position_code: info.position_code.clone(),
            person: info.person.clone(),
            changes: ops.iter().flat_map(|op| op.changes.clone()).collect(),
            unit_id: info.unit.as_ref().and_then(unit_id),
            position_id: info.position.as_ref().and_then(position_id),
        });
    }

    let mut units_changed: HashSet<String> = HashSet::new();
    let mut positions_changed: HashSet<String> = HashSet::new();
    let mut assignments_changed: HashSet<String> = HashSet::new();
    for (index, op) in plan.ops.iter().enumerate() {
        if !executed.done[index] {
            continue;
        }
        use plan::OpKind as K;
        match &op.kind {
            K::CreateUnit { .. } => counts.units_added += 1,
            K::UpdateUnit { unit_id, .. } | K::MoveUnit { unit_id, .. } => {
                units_changed.insert(unit_id.clone());
            }
            K::SetHead { unit, .. } | K::SetDeputies { unit, .. } => {
                if let UnitRef::Existing(id) = unit {
                    units_changed.insert(id.clone());
                }
            }
            K::CreatePosition { .. } => counts.positions_added += 1,
            K::UpdatePosition { position_id, .. } | K::MovePosition { position_id, .. } => {
                positions_changed.insert(position_id.clone());
            }
            K::Assign { .. } => counts.assignments_added += 1,
            K::UpdateAssignment { assignment_id, .. } | K::DemotePrimary { assignment_id, .. } => {
                assignments_changed.insert(assignment_id.clone());
            }
            K::EndAssignment { .. } => counts.assignments_ended += 1,
            K::EndPosition { position_id, .. } => {
                counts.positions_ended += 1;
                counts.assignments_ended +=
                    plan.cascaded_holders.get(position_id).copied().unwrap_or(0) as u32;
            }
            K::EndUnit { .. } => counts.units_ended += 1,
        }
    }
    counts.units_changed = units_changed.len() as u32;
    counts.positions_changed = positions_changed.len() as u32;
    counts.assignments_changed = assignments_changed.len() as u32;
    (rows, counts)
}

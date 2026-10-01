//! The structure as a file, in the columns the import reads, and the report
//! of an import's errors.

use std::collections::{HashMap, HashSet};

use chrono::NaiveDate;
use rust_xlsxwriter::{Format, Workbook};

use super::codes;
use super::columns::{Column, HeaderLanguage, COLUMNS};
use super::parse::FORMULA_STARTS;
use super::people::Directory;
use super::report::Issue;
use super::FileFormat;
use crate::db::DbPool;
use crate::services::org_structure::error::{OrgStructureError as E, Result};
use crate::services::org_structure::query::Snapshot;
use crate::services::org_structure::repo::{self, timezone_of};
use crate::services::org_structure::types::{Assignment, Position, Subject, Unit};
use crate::services::org_structure::validate;

pub struct ExportFile {
    pub file_name: String,
    pub mime: &'static str,
    pub bytes: Vec<u8>,
}

const CSV_MIME: &str = "text/csv; charset=utf-8";
const XLSX_MIME: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
const SHEET_NAME: &str = "Struktura";

enum Cell {
    Text(String),
    /// A number the spreadsheet keeps as a number (the share).
    Number(f64),
}

impl Cell {
    fn empty() -> Self {
        Self::Text(String::new())
    }

    fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }
}

fn yes_no(value: bool) -> Cell {
    Cell::text(if value { "tak" } else { "nie" })
}

/// A cell a spreadsheet would run as a formula (`=`, `+`, `-`, `@`) is written
/// with a leading `'`, which a spreadsheet does not show and the import
/// removes; a text that already starts with quotes before such a character
/// gets one more, so removing one gives back what was typed. A name typed by a user is not trusted to be harmless.
pub fn guard_formula(text: &str) -> String {
    if text.trim_start_matches('\'').starts_with(FORMULA_STARTS) {
        format!("'{text}")
    } else {
        text.to_string()
    }
}

/// The columns a caller may see: a member who is not an administrator gets
/// the structure but not the logins and e-mail addresses of other people.
fn visible_columns(include_contact: bool) -> Vec<Column> {
    COLUMNS
        .iter()
        .map(|spec| spec.column)
        .filter(|column| include_contact || !matches!(column, Column::Person | Column::Email))
        .collect()
}

/// Units in tree order (a parent before its children), so the file reads
/// like the tree.
fn unit_order(snap: &Snapshot) -> Vec<&Unit> {
    let mut children: HashMap<Option<&str>, Vec<&Unit>> = HashMap::new();
    let known: HashSet<&str> = snap.units.iter().map(|u| u.unit_id.as_str()).collect();
    for unit in &snap.units {
        let parent = unit
            .parent_unit_id
            .as_deref()
            .filter(|parent| known.contains(parent));
        children.entry(parent).or_default().push(unit);
    }
    let mut ordered = Vec::with_capacity(snap.units.len());
    let mut seen: HashSet<&str> = HashSet::new();
    let mut stack: Vec<&Unit> = children.remove(&None).unwrap_or_default();
    stack.reverse();
    while let Some(unit) = stack.pop() {
        if !seen.insert(unit.unit_id.as_str()) {
            continue;
        }
        ordered.push(unit);
        if let Some(mut below) = children.remove(&Some(unit.unit_id.as_str())) {
            below.reverse();
            stack.extend(below);
        }
    }
    // Units of a cycle merged from two nodes hang off nothing; they still belong in the file.
    for unit in &snap.units {
        if seen.insert(unit.unit_id.as_str()) {
            ordered.push(unit);
        }
    }
    ordered
}

struct Context<'a> {
    snap: &'a Snapshot,
    directory: &'a Directory,
    type_names: HashMap<&'a str, &'a str>,
    day: NaiveDate,
    include_contact: bool,
}

impl Context<'_> {
    /// Login (or identity), name and e-mail of a holder. A holder who is no
    /// longer in the organization is written as an explicit marker, never as
    /// an empty cell: an empty cell reads as a vacancy, and a replace would
    /// end the position's holder without a word. The import reports the
    /// marker as an unknown person, for the administrator to settle.
    fn person_cells(&self, subject: &Subject) -> (String, String, String) {
        let gone = |id: &str| {
            (
                format!("[outside the organization: {id}]"),
                "(?)".to_string(),
                String::new(),
            )
        };
        match subject {
            Subject::User(id) => match self.directory.member(id) {
                // Without contact rights the stored display name is all there
                // is: falling back to the login would hand it out.
                Some(m) => (
                    m.login.clone(),
                    if self.include_contact {
                        m.name.clone()
                    } else {
                        m.display.clone()
                    },
                    m.email.clone(),
                ),
                None => gone(id),
            },
            Subject::External(id) => match self.directory.external(id) {
                Some(e) => {
                    let identity = if e.email.is_empty() {
                        &e.name
                    } else {
                        &e.email
                    };
                    (identity.clone(), e.name.clone(), e.email.clone())
                }
                None => gone(id),
            },
        }
    }

    fn row(&self, unit: &Unit, position: Option<(&Position, Option<&Assignment>)>) -> Vec<Cell> {
        let parent_code = unit
            .parent_unit_id
            .as_deref()
            .and_then(|id| self.snap.unit(id))
            .map(codes::unit_code)
            .unwrap_or_default();
        let unit_type = unit
            .type_id
            .as_deref()
            .and_then(|id| self.type_names.get(id))
            .copied()
            .unwrap_or_default();
        visible_columns(self.include_contact)
            .into_iter()
            .map(|column| match column {
                Column::UnitCode => Cell::text(codes::unit_code(unit)),
                Column::UnitName => Cell::text(unit.name.clone()),
                Column::ParentCode => Cell::text(parent_code.clone()),
                Column::UnitType => Cell::text(unit_type),
                Column::From => Cell::text(validate::format_date(self.day)),
                _ => match position {
                    None => Cell::empty(),
                    Some((position, held)) => self.position_cell(column, unit, position, held),
                },
            })
            .collect()
    }

    fn position_cell(
        &self,
        column: Column,
        unit: &Unit,
        position: &Position,
        held: Option<&Assignment>,
    ) -> Cell {
        let person = held.map(|a| self.person_cells(&a.subject));
        match column {
            Column::PositionCode => Cell::text(codes::position_code(position)),
            Column::Position => Cell::text(position.name.clone()),
            Column::Staff => yes_no(position.is_staff),
            Column::Head => yes_no(unit.head_position_id.as_deref() == Some(&position.position_id)),
            Column::Primary => held.map_or_else(Cell::empty, |a| yes_no(a.is_primary)),
            Column::Person => person.map_or_else(Cell::empty, |(login, _, _)| Cell::text(login)),
            Column::PersonName => person.map_or_else(Cell::empty, |(_, name, _)| Cell::text(name)),
            Column::Email => person.map_or_else(Cell::empty, |(_, _, email)| Cell::text(email)),
            Column::Share => held.map_or_else(Cell::empty, |a| Cell::Number(a.share)),
            Column::Manager => self
                .snap
                .primary_parent_of(&position.position_id)
                .and_then(|id| self.snap.position(id))
                .map_or_else(Cell::empty, |p| Cell::text(codes::position_code(p))),
            Column::DeputyOrder => self
                .snap
                .deputy_heads
                .iter()
                .find(|d| d.unit_id == unit.unit_id && d.position_id == position.position_id)
                .map_or_else(Cell::empty, |d| Cell::text((d.ord + 1).to_string())),
            _ => Cell::empty(),
        }
    }
}

fn structure_rows(
    conn: &rusqlite::Connection,
    org_id: &str,
    day: NaiveDate,
    include_contact: bool,
) -> Result<Vec<Vec<Cell>>> {
    let snap = Snapshot::load(conn, org_id, day)?;
    let directory = Directory::load(conn, org_id)?;
    let types = repo::unit_types_in(conn, org_id)?;
    let context = Context {
        snap: &snap,
        directory: &directory,
        type_names: types
            .iter()
            .map(|t| (t.id.as_str(), t.name.as_str()))
            .collect(),
        day,
        include_contact,
    };
    let mut rows = Vec::new();
    for unit in unit_order(&snap) {
        let mut positions: Vec<&Position> = snap
            .positions
            .iter()
            .filter(|p| p.unit_id == unit.unit_id)
            .collect();
        // The head first, then the deputy heads in order, then the rest.
        positions.sort_by_key(|p| {
            let head = unit.head_position_id.as_deref() == Some(&p.position_id);
            let deputy = snap
                .deputy_heads
                .iter()
                .find(|d| d.unit_id == unit.unit_id && d.position_id == p.position_id)
                .map(|d| d.ord);
            (
                !head,
                deputy.unwrap_or(i64::MAX),
                p.name.clone(),
                p.position_id.clone(),
            )
        });
        if positions.is_empty() {
            rows.push(context.row(unit, None));
        }
        for position in positions {
            let holders: Vec<&Assignment> = snap.holders_of(&position.position_id).collect();
            if holders.is_empty() {
                rows.push(context.row(unit, Some((position, None))));
            }
            for held in holders {
                rows.push(context.row(unit, Some((position, Some(held)))));
            }
        }
    }
    Ok(rows)
}

fn share_text(share: f64) -> String {
    format!("{share}").replace('.', ",")
}

fn header_row(include_contact: bool, language: HeaderLanguage) -> Vec<&'static str> {
    visible_columns(include_contact)
        .into_iter()
        .map(|column| column.header(language))
        .collect()
}

/// The structure on `at` (today in the organization's timezone by default).
/// `include_contact` adds the login and e-mail columns; `language` picks the
/// header row (the import reads either).
pub fn export_structure(
    pool: &DbPool,
    org_id: &str,
    format: FileFormat,
    at: Option<NaiveDate>,
    include_contact: bool,
    language: HeaderLanguage,
) -> Result<ExportFile> {
    let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
    let day = match at {
        Some(day) => day,
        None => validate::today_in_zone(&timezone_of(&conn, org_id)?)?,
    };
    let rows = structure_rows(&conn, org_id, day, include_contact)?;
    let headers = header_row(include_contact, language);
    let stem = format!("org-structure-{}", validate::format_date(day));
    match format {
        FileFormat::Csv => {
            let table: Vec<Vec<String>> = rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|cell| match cell {
                            Cell::Text(text) => guard_formula(text),
                            Cell::Number(number) => share_text(*number),
                        })
                        .collect()
                })
                .collect();
            Ok(ExportFile {
                file_name: format!("{stem}.csv"),
                mime: CSV_MIME,
                bytes: write_csv(&headers, &table)?,
            })
        }
        FileFormat::Xlsx => Ok(ExportFile {
            file_name: format!("{stem}.xlsx"),
            mime: XLSX_MIME,
            bytes: write_xlsx(&headers, &rows)?,
        }),
    }
}

/// UTF-8 with a BOM and `;`: what a Polish Excel opens without asking. The
/// import finds the delimiter itself, so a file saved with `,` reads too.
fn write_csv(headers: &[&str], rows: &[Vec<String>]) -> Result<Vec<u8>> {
    let mut writer = csv::WriterBuilder::new()
        .delimiter(b';')
        .from_writer(b"\xef\xbb\xbf".to_vec());
    let internal = |e: csv::Error| E::Db(format!("csv export: {e}"));
    writer.write_record(headers).map_err(internal)?;
    for row in rows {
        writer.write_record(row).map_err(internal)?;
    }
    writer
        .into_inner()
        .map_err(|e| E::Db(format!("csv export: {e}")))
}

fn write_xlsx(headers: &[&str], rows: &[Vec<Cell>]) -> Result<Vec<u8>> {
    let internal = |e: rust_xlsxwriter::XlsxError| E::Db(format!("xlsx export: {e}"));
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet.set_name(SHEET_NAME).map_err(internal)?;
    let bold = Format::new().set_bold();
    for (column, header) in headers.iter().enumerate() {
        let column = column as u16;
        sheet
            .write_string_with_format(0, column, *header, &bold)
            .map_err(internal)?;
        sheet
            .set_column_width(column, (header.chars().count() as f64 + 4.0).max(14.0))
            .map_err(internal)?;
    }
    sheet.set_freeze_panes(1, 0).map_err(internal)?;
    for (index, row) in rows.iter().enumerate() {
        let excel_row = index as u32 + 1;
        for (column, cell) in row.iter().enumerate() {
            let column = column as u16;
            match cell {
                Cell::Text(text) if text.is_empty() => {}
                // A string cell is never evaluated as a formula.
                Cell::Text(text) => {
                    sheet
                        .write_string(excel_row, column, text)
                        .map_err(internal)?;
                }
                Cell::Number(number) => {
                    sheet
                        .write_number(excel_row, column, *number)
                        .map_err(internal)?;
                }
            }
        }
    }
    workbook.save_to_buffer().map_err(internal)
}

/// The errors of a dry run as a CSV to fix in a spreadsheet: where (row,
/// column), what (kind) and what to do about it (message, suggestion).
pub fn export_errors(errors: &[Issue], as_of: NaiveDate) -> Result<ExportFile> {
    let headers = [
        "row",
        "related rows",
        "column",
        "kind",
        "value",
        "message",
        "suggestion",
    ];
    let rows: Vec<Vec<String>> = errors
        .iter()
        .map(|issue| {
            vec![
                issue.row.to_string(),
                issue
                    .rows
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(" "),
                issue
                    .column
                    .map_or_else(String::new, |c| c.spec().header.to_string()),
                issue.code.unwrap_or(issue.kind.code()).to_string(),
                guard_formula(issue.value.as_deref().unwrap_or("")),
                guard_formula(&issue.message),
                guard_formula(issue.suggestion.as_deref().unwrap_or("")),
            ]
        })
        .collect();
    Ok(ExportFile {
        file_name: format!(
            "org-structure-import-errors-{}.csv",
            validate::format_date(as_of)
        ),
        mime: CSV_MIME,
        bytes: write_csv(&headers, &rows)?,
    })
}

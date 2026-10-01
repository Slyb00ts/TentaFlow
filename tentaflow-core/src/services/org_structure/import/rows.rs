//! Cells into typed values. A row with a cell that does not parse is marked
//! `broken`: the rest of the run leaves it out but still knows which codes it
//! declares, so one bad cell does not make every reference to it an error too.

use chrono::{Days, NaiveDate};

use super::columns::{fold, Column, Mapping};
use super::parse::Table;
use super::report::{Issue, IssueKind};
use crate::services::org_structure::validate::{MAX_CODE_CHARS, MAX_NAME_CHARS};

const MAX_CELL_CHARS: usize = 500;

/// Days between the Excel serial epoch (1899-12-30) and a date; a serial in
/// this range is a date typed into a cell formatted as a number.
const MIN_SERIAL: f64 = 20_000.0;
const MAX_SERIAL: f64 = 80_000.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub number: u32,
    pub unit_code: Option<String>,
    pub unit_name: Option<String>,
    pub parent_code: Option<String>,
    pub unit_type: Option<String>,
    pub position_code: Option<String>,
    pub position: Option<String>,
    pub staff: Option<bool>,
    pub head: Option<bool>,
    pub primary: Option<bool>,
    /// Login or e-mail; the e-mail column stands in when the login is empty.
    pub person: Option<String>,
    pub person_column: Column,
    pub share: Option<f64>,
    pub manager: Option<String>,
    pub deputy_order: Option<u32>,
    pub from: Option<NaiveDate>,
    pub broken: bool,
}

impl Row {
    /// Anything that says something about a position, as opposed to the unit.
    pub fn is_position_row(&self) -> bool {
        self.position_code.is_some()
            || self.position.is_some()
            || self.person.is_some()
            || self.manager.is_some()
            || self.staff.is_some()
            || self.head == Some(true)
            || self.primary.is_some()
            || self.share.is_some()
            || self.deputy_order.is_some()
    }
}

fn text(cell: &str) -> Option<String> {
    (!cell.is_empty()).then(|| cell.to_string())
}

fn parse_bool(cell: &str) -> Option<bool> {
    match fold(cell).as_str() {
        "tak" | "t" | "yes" | "y" | "true" | "1" | "x" | "prawda" => Some(true),
        "nie" | "n" | "no" | "false" | "0" | "falsz" => Some(false),
        _ => None,
    }
}

/// `0.5`, `0,5`, `50%` and `1/2` are all one half.
fn parse_share(cell: &str) -> Option<f64> {
    let cell = cell.replace(',', ".");
    let value = if let Some(percent) = cell.strip_suffix('%') {
        percent.trim().parse::<f64>().ok()? / 100.0
    } else if let Some((top, bottom)) = cell.split_once('/') {
        let bottom = bottom.trim().parse::<f64>().ok()?;
        if bottom == 0.0 {
            return None;
        }
        top.trim().parse::<f64>().ok()? / bottom
    } else {
        cell.trim().parse::<f64>().ok()?
    };
    // 1.0 is the ceiling of one assignment (the table refuses more); an
    // over-booked person is the SUM of their assignments, and only a warning.
    (value.is_finite() && value > 0.0 && value <= 1.0 + 1e-9).then_some(value.min(1.0))
}

fn parse_order(cell: &str) -> Option<u32> {
    let value = cell.replace(',', ".").parse::<f64>().ok()?;
    (value.fract() == 0.0 && (1.0..=1000.0).contains(&value)).then_some(value as u32)
}

pub fn parse_day(cell: &str) -> Option<NaiveDate> {
    const FORMATS: [&str; 6] = [
        "%Y-%m-%d", "%d.%m.%Y", "%d-%m-%Y", "%d/%m/%Y", "%Y/%m/%d", "%Y.%m.%d",
    ];
    // A date-time cell keeps only its day.
    let day_part = cell.split(['T', ' ']).next().unwrap_or(cell);
    for format in FORMATS {
        if let Ok(date) = NaiveDate::parse_from_str(day_part, format) {
            return Some(date);
        }
    }
    let serial = cell.replace(',', ".").parse::<f64>().ok()?;
    if (MIN_SERIAL..=MAX_SERIAL).contains(&serial) {
        return NaiveDate::from_ymd_opt(1899, 12, 30)?.checked_add_days(Days::new(serial as u64));
    }
    None
}

pub fn parse(table: &Table, mapping: &Mapping) -> (Vec<Row>, Vec<Issue>) {
    let mut rows = Vec::with_capacity(table.rows.len());
    let mut issues = Vec::new();
    for raw in &table.rows {
        let cell = |column| mapping.cell(column, &raw.cells);
        let mut broken = false;
        let mut fail = |issue: Issue| {
            broken = true;
            issues.push(issue);
        };

        let mut bounded = |column: Column, max: usize| -> Option<String> {
            let value = cell(column);
            if value.chars().count() > max.min(MAX_CELL_CHARS) {
                fail(
                    Issue::new(
                        raw.number,
                        IssueKind::ValueTooLong,
                        format!("longer than {max} characters"),
                    )
                    .column(column),
                );
                return None;
            }
            text(value)
        };
        let unit_code = bounded(Column::UnitCode, MAX_CODE_CHARS);
        let unit_name = bounded(Column::UnitName, MAX_NAME_CHARS);
        let parent_code = bounded(Column::ParentCode, MAX_CODE_CHARS);
        let unit_type = bounded(Column::UnitType, MAX_NAME_CHARS);
        let position_code = bounded(Column::PositionCode, MAX_CODE_CHARS);
        let position = bounded(Column::Position, MAX_NAME_CHARS);
        let manager = bounded(Column::Manager, MAX_CODE_CHARS);
        let login = bounded(Column::Person, MAX_NAME_CHARS);
        let email = bounded(Column::Email, MAX_NAME_CHARS);
        let (person, person_column) = match login {
            Some(login) => (Some(login), Column::Person),
            None => (email, Column::Email),
        };

        let mut flag = |column: Column| -> Option<bool> {
            let value = cell(column);
            if value.is_empty() {
                return None;
            }
            let parsed = parse_bool(value);
            if parsed.is_none() {
                fail(
                    Issue::new(
                        raw.number,
                        IssueKind::InvalidBoolean,
                        "expected tak/nie (yes/no)",
                    )
                    .column(column)
                    .value(value),
                );
            }
            parsed
        };
        let staff = flag(Column::Staff);
        let head = flag(Column::Head);
        let primary = flag(Column::Primary);

        let share = match cell(Column::Share) {
            "" => None,
            value => {
                let parsed = parse_share(value);
                if parsed.is_none() {
                    fail(
                        Issue::new(
                            raw.number,
                            IssueKind::InvalidShare,
                            "expected a share above 0 and up to 1 (0.5, 0,5, 50% or 1/2)",
                        )
                        .column(Column::Share)
                        .value(value),
                    );
                }
                parsed
            }
        };
        let deputy_order = match cell(Column::DeputyOrder) {
            "" => None,
            value => {
                let parsed = parse_order(value);
                if parsed.is_none() {
                    fail(
                        Issue::new(
                            raw.number,
                            IssueKind::InvalidNumber,
                            "expected the order of the deputy: 1, 2, 3...",
                        )
                        .column(Column::DeputyOrder)
                        .value(value),
                    );
                }
                parsed
            }
        };
        let from = match cell(Column::From) {
            "" => None,
            value => {
                let parsed = parse_day(value);
                if parsed.is_none() {
                    fail(
                        Issue::new(
                            raw.number,
                            IssueKind::InvalidDate,
                            "expected a date such as 2026-11-01 or 01.11.2026",
                        )
                        .column(Column::From)
                        .value(value),
                    );
                }
                parsed
            }
        };
        rows.push(Row {
            number: raw.number,
            unit_code,
            unit_name,
            parent_code,
            unit_type,
            position_code,
            position,
            staff,
            head,
            primary,
            person,
            person_column,
            share,
            manager,
            deputy_order,
            from,
            broken,
        });
    }
    (rows, issues)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_are_read_the_way_people_write_them() {
        for (cell, expected) in [
            ("1", 1.0),
            ("0.5", 0.5),
            ("0,5", 0.5),
            ("50%", 0.5),
            ("1/2", 0.5),
            (" 0,25 ", 0.25),
        ] {
            assert_eq!(parse_share(cell.trim()), Some(expected), "{cell}");
        }
        for cell in ["0", "-0.5", "1.5", "abc", "1/0", "150%", "NaN", ""] {
            assert_eq!(parse_share(cell), None, "{cell}");
        }
    }

    #[test]
    fn dates_come_in_every_shape_a_spreadsheet_produces() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 1);
        for cell in [
            "2026-11-01",
            "01.11.2026",
            "1.11.2026",
            "01-11-2026",
            "01/11/2026",
            "2026/11/01",
            "2026-11-01 00:00:00",
            "2026-11-01T00:00:00",
            // The Excel serial of 2026-11-01.
            "46327",
        ] {
            assert_eq!(parse_day(cell), day, "{cell}");
        }
        for cell in ["", "yesterday", "32.13.2026", "12"] {
            assert_eq!(parse_day(cell), None, "{cell}");
        }
    }

    #[test]
    fn booleans_are_polish_or_english() {
        for cell in ["tak", "TAK", "Tak", "yes", "true", "1", "x"] {
            assert_eq!(parse_bool(cell), Some(true), "{cell}");
        }
        for cell in ["nie", "no", "false", "0", "Fałsz"] {
            assert_eq!(parse_bool(cell), Some(false), "{cell}");
        }
        assert_eq!(parse_bool("może"), None);
    }
}

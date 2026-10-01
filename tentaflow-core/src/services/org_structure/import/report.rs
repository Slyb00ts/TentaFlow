//! What an import run tells the administrator: typed file failures, issues
//! with row numbers, per-row outcomes and the counts the panel shows.

use chrono::NaiveDate;
use thiserror::Error;

use super::columns::Column;
use super::{FileFormat, Mode};
use crate::services::org_structure::query::StructureView;

/// The file itself cannot be used; nothing else can be said about it.
#[derive(Debug, Error, PartialEq, Eq, Clone)]
pub enum FileError {
    #[error("the file is {bytes} bytes; the limit is {max}")]
    TooLarge { bytes: usize, max: usize },

    #[error("the workbook expands to more than {max} bytes")]
    ExpandsTooLarge { max: u64 },

    #[error("the file has more than {max} rows")]
    TooManyRows { max: usize },

    #[error("the file cannot be read as {format}: {detail}")]
    Unreadable {
        format: &'static str,
        detail: String,
    },

    #[error("the sheet reaches row {rows}, column {cols}; the limit is {max_rows} rows by {max_cols} columns")]
    SheetTooLarge {
        rows: u32,
        cols: u32,
        max_rows: u32,
        max_cols: u32,
    },

    #[error("the file's text encoding cannot be read safely: {detail}")]
    InvalidEncoding { detail: String },

    #[error("the file has no rows")]
    Empty,

    #[error("the header row has no '{}' column", .0.spec().header)]
    MissingColumn(Column),

    #[error("the column '{}' appears more than once", .0.spec().header)]
    DuplicateColumn(Column),
}

impl FileError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::TooLarge { .. } => "file_too_large",
            Self::ExpandsTooLarge { .. } => "file_expands_too_large",
            Self::TooManyRows { .. } => "too_many_rows",
            Self::Unreadable { .. } => "unreadable_file",
            Self::SheetTooLarge { .. } => "sheet_too_large",
            Self::InvalidEncoding { .. } => "invalid_encoding",
            Self::Empty => "empty_file",
            Self::MissingColumn(_) => "missing_column",
            Self::DuplicateColumn(_) => "duplicate_column",
        }
    }

    pub fn column(&self) -> Option<Column> {
        match self {
            Self::MissingColumn(column) | Self::DuplicateColumn(column) => Some(*column),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueKind {
    // A cell that does not parse.
    InvalidBoolean,
    InvalidNumber,
    InvalidShare,
    InvalidDate,
    ValueTooLong,
    // What the row names.
    MissingUnitCode,
    MissingPositionCode,
    MissingUnitName,
    MissingPositionName,
    ConflictingValues,
    DuplicateAssignment,
    AmbiguousCode,
    CodeReserved,
    UnknownUnitType,
    UnknownPerson,
    AmbiguousPerson,
    // Shape of the structure.
    MissingParentUnit,
    UnitCycle,
    MissingManager,
    ReportingCycle,
    StaffManager,
    TwoHeads,
    HeadIsDeputy,
    DuplicateDeputyOrder,
    TwoPrimaryPositions,
    PositionUnitMismatch,
    // The run itself.
    BackdatedConfirmationRequired,
    ResolutionNotApplicable,
    ReplaceMatchesNothing,
    /// A replace would end something and the request did not say it may.
    EndedConfirmationRequired,
    /// The repository refused an operation; `Issue::code` names the rule.
    Rejected,
    // Warnings.
    ShareOverbooked,
    UnitWithoutHead,
    PersonWithoutPrimary,
    PositionHasOtherHolder,
    UnknownColumn,
    /// The workbook has data on other sheets than the one that was read.
    OtherSheetsIgnored,
}

impl IssueKind {
    /// Stable snake_case identifier; the screen keys its message on it.
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidBoolean => "invalid_boolean",
            Self::InvalidNumber => "invalid_number",
            Self::InvalidShare => "invalid_share",
            Self::InvalidDate => "invalid_date",
            Self::ValueTooLong => "value_too_long",
            Self::MissingUnitCode => "missing_unit_code",
            Self::MissingPositionCode => "missing_position_code",
            Self::MissingUnitName => "missing_unit_name",
            Self::MissingPositionName => "missing_position_name",
            Self::ConflictingValues => "conflicting_values",
            Self::DuplicateAssignment => "duplicate_assignment",
            Self::AmbiguousCode => "ambiguous_code",
            Self::CodeReserved => "code_reserved",
            Self::UnknownUnitType => "unknown_unit_type",
            Self::UnknownPerson => "unknown_person",
            Self::AmbiguousPerson => "ambiguous_person",
            Self::MissingParentUnit => "missing_parent_unit",
            Self::UnitCycle => "unit_cycle",
            Self::MissingManager => "missing_manager",
            Self::ReportingCycle => "reporting_cycle",
            Self::StaffManager => "staff_manager",
            Self::TwoHeads => "two_heads",
            Self::HeadIsDeputy => "head_is_deputy",
            Self::DuplicateDeputyOrder => "duplicate_deputy_order",
            Self::TwoPrimaryPositions => "two_primary_positions",
            Self::PositionUnitMismatch => "position_unit_mismatch",
            Self::BackdatedConfirmationRequired => "backdated_confirmation_required",
            Self::ResolutionNotApplicable => "resolution_not_applicable",
            Self::ReplaceMatchesNothing => "replace_matches_nothing",
            Self::EndedConfirmationRequired => "ended_confirmation_required",
            Self::Rejected => "rejected",
            Self::ShareOverbooked => "share_overbooked",
            Self::UnitWithoutHead => "unit_without_head",
            Self::PersonWithoutPrimary => "person_without_primary",
            Self::PositionHasOtherHolder => "position_has_other_holder",
            Self::UnknownColumn => "unknown_column",
            Self::OtherSheetsIgnored => "other_sheets_ignored",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    /// The row to look at (the last of `rows` for a cycle: the one to change).
    pub row: u32,
    /// Every row the issue is about, `row` included.
    pub rows: Vec<u32>,
    pub column: Option<Column>,
    pub kind: IssueKind,
    /// What the file says there.
    pub value: Option<String>,
    pub message: String,
    /// What to use instead: a login for `UnknownPerson`, a name for `UnknownUnitType`.
    pub suggestion: Option<String>,
    /// Who the suggested login belongs to.
    pub suggestion_label: Option<String>,
    /// The rule of the repository that refused an operation (`Rejected`).
    pub code: Option<&'static str>,
}

impl Issue {
    pub fn new(row: u32, kind: IssueKind, message: impl Into<String>) -> Self {
        Self {
            row,
            rows: vec![row],
            column: None,
            kind,
            value: None,
            message: message.into(),
            suggestion: None,
            suggestion_label: None,
            code: None,
        }
    }

    pub fn column(mut self, column: Column) -> Self {
        self.column = Some(column);
        self
    }

    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
        self
    }

    pub fn rows(mut self, rows: impl IntoIterator<Item = u32>) -> Self {
        let mut rows: Vec<u32> = rows.into_iter().collect();
        if !rows.contains(&self.row) {
            rows.push(self.row);
        }
        rows.sort_unstable();
        rows.dedup();
        self.rows = rows;
        self
    }

    pub fn suggest(mut self, suggestion: String, label: Option<String>) -> Self {
        self.suggestion = Some(suggestion);
        self.suggestion_label = label;
        self
    }

    pub fn code(mut self, code: &'static str) -> Self {
        self.code = Some(code);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    Added,
    Changed,
    Unchanged,
    Error,
}

/// One value the run changes (or sets on a new entity), for the "Zmienione"
/// list of the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldChange {
    /// `unit`, `position`, `assignment`.
    pub entity: &'static str,
    pub field: &'static str,
    pub before: Option<String>,
    pub after: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RowOutcome {
    pub row: u32,
    /// `Error` when any error names the row; otherwise what the row does.
    pub status: RowStatus,
    /// What the row's operations do (`Added`, `Changed` or `Unchanged`),
    /// whether or not the row also has an error.
    pub effect: RowStatus,
    /// The non-empty cells of the row as the file has them, by header.
    pub cells: Vec<(String, String)>,
    pub unit_code: Option<String>,
    pub position_code: Option<String>,
    /// The person as the file names them.
    pub person: Option<String>,
    pub changes: Vec<FieldChange>,
    /// Ids of the entities the row stands for in `preview` — new ones exist
    /// only there, the run never commits them unless it applies.
    pub unit_id: Option<String>,
    pub position_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counts {
    /// Rows of the file with content. `added + changed + unchanged == rows`:
    /// each row is counted once by what its operations do, errors or not.
    pub rows: u32,
    pub added: u32,
    pub changed: u32,
    pub unchanged: u32,
    /// Rows some error names (a row can be `added` and have an error).
    pub errors: u32,
    /// Error issues — one row can have several, a cycle names several rows.
    pub issues: u32,
    pub units_added: u32,
    pub units_changed: u32,
    pub units_ended: u32,
    pub positions_added: u32,
    pub positions_changed: u32,
    pub positions_ended: u32,
    pub assignments_added: u32,
    pub assignments_changed: u32,
    pub assignments_ended: u32,
}

/// Something a replace ends, listed so the administrator sees it before
/// confirming: a unit, a position with the people who hold it, or one
/// person leaving a position that stays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndedItem {
    /// `unit`, `position` or `assignment`.
    pub kind: &'static str,
    pub code: String,
    pub name: String,
    /// The people who stop holding a position with it.
    pub holders: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Report {
    pub format: FileFormat,
    pub mode: Mode,
    pub as_of: NaiveDate,
    /// The day `preview` shows: the latest a row takes effect.
    pub preview_at: NaiveDate,
    /// The changes are committed.
    pub applied: bool,
    pub file_error: Option<FileError>,
    /// The worksheet the rows were read from (XLSX).
    pub sheet: Option<String>,
    pub counts: Counts,
    /// What a replace ends. Empty for an upsert.
    pub ended: Vec<EndedItem>,
    /// Added, changed and error rows; unchanged rows are only counted.
    pub rows: Vec<RowOutcome>,
    pub errors: Vec<Issue>,
    pub warnings: Vec<Issue>,
    /// The structure the file would leave, in the shape of `structure_as_of`.
    pub preview: Option<StructureView>,
    /// Rows with errors are left out of `preview` (or only the part of the
    /// row that is wrong is).
    pub preview_partial: bool,
}

impl Report {
    pub fn failed_file(format: FileFormat, mode: Mode, as_of: NaiveDate, error: FileError) -> Self {
        Self {
            format,
            mode,
            as_of,
            preview_at: as_of,
            applied: false,
            file_error: Some(error),
            sheet: None,
            counts: Counts::default(),
            ended: Vec::new(),
            rows: Vec::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
            preview: None,
            preview_partial: false,
        }
    }
}

// =============================================================================
// File: org_structure.rs
// Purpose: Binary CBOR protocol for the organizational structure — units,
//          positions, reporting lines and who holds which position, all
//          effective-dated (docs/ORG_STRUCTURE_PLAN.md §1, §2).
//
//          Reads are open to every member of the organization; every write
//          needs `org.admin`. Dates travel as `YYYY-MM-DD` strings: they are
//          calendar days of the organization's timezone, never instants.
//          Validation failures are answered in the response body as an
//          `OrgOpError` so the screen can name the rule that refused, while
//          authorization failures are protocol errors.
//
//          File import and export (docs §2.5) carry the file as a CBOR byte
//          string; the answer to an import is a typed report, not an error,
//          because a mistake in the file is the normal outcome of a dry run.
//
//          Append-only, and a rename is the one change that breaks every
//          deployed peer while the round-trip tests stay green — ciborium tags
//          by NAME. A field added later MUST carry `#[serde(default)]`.
// Example: MessageBody::OrgStructureBody(OrgStructurePayload::StructureRequest { at: None })
// =============================================================================

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgUnitType {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
}

/// One VERSION of a unit; `unit_id` is the identity everything else points at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgUnit {
    pub id: String,
    pub unit_id: String,
    pub name: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub type_id: Option<String>,
    #[serde(default)]
    pub parent_unit_id: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub head_position_id: Option<String>,
    /// Ordered; filled by the structure read only.
    #[serde(default)]
    pub deputy_head_position_ids: Vec<String>,
    pub valid_from: String,
    #[serde(default)]
    pub valid_to: Option<String>,
}

/// One VERSION of a position; `position_id` is the identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgPosition {
    pub id: String,
    pub position_id: String,
    pub unit_id: String,
    pub name: String,
    /// The key the file import and export match on; unique in the organization.
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub role_id: Option<String>,
    /// `None` = follow the role's `is_manager`.
    #[serde(default)]
    pub is_manager: Option<bool>,
    #[serde(default)]
    pub is_staff: bool,
    pub valid_from: String,
    #[serde(default)]
    pub valid_to: Option<String>,
    /// The next position up the primary line. Filled by the structure read only.
    #[serde(default)]
    pub primary_parent_position_id: Option<String>,
    #[serde(default)]
    pub functional_parent_position_ids: Vec<String>,
    #[serde(default)]
    pub is_head: bool,
    #[serde(default)]
    pub is_vacant: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OrgLineKind {
    #[default]
    Primary,
    Functional,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgReportingLine {
    pub id: String,
    pub position_id: String,
    pub parent_position_id: String,
    pub kind: OrgLineKind,
    pub priority: i64,
    pub valid_from: String,
    #[serde(default)]
    pub valid_to: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgDeputyHead {
    pub id: String,
    pub unit_id: String,
    pub position_id: String,
    pub ord: i64,
    pub valid_from: String,
    #[serde(default)]
    pub valid_to: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OrgAssignmentType {
    #[default]
    Permanent,
    Acting,
    Contractor,
}

/// Who holds a position: an account of the platform or a person without one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum OrgSubject {
    User(String),
    External(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgAssignment {
    pub id: String,
    pub position_id: String,
    pub subject: OrgSubject,
    pub assignment_type: OrgAssignmentType,
    pub share: f64,
    pub is_primary: bool,
    pub valid_from: String,
    #[serde(default)]
    pub valid_to: Option<String>,
    /// Filled by the structure read only.
    #[serde(default)]
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgExternalPerson {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgSettings {
    pub org_id: String,
    pub timezone: String,
}

/// Something the administrator should look at that does not block the write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OrgWarning {
    ShareOverbooked {
        subject: OrgSubject,
        from: String,
        total: f64,
    },
    UnitWithoutHead {
        unit_id: String,
        from: String,
    },
    PersonWithoutPrimary {
        subject: OrgSubject,
        from: String,
    },
}

/// A broken rule found by the integrity report. The write path rejects every
/// one of these; they can still appear when two nodes edit offline and merge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OrgViolation {
    VersionsOverlap {
        entity: String,
        id: String,
        from: String,
    },
    PrimaryLinesOverlap {
        position_id: String,
        from: String,
    },
    PrimaryAssignmentsOverlap {
        subject: OrgSubject,
        from: String,
    },
    PositionHeadsMultipleUnits {
        position_id: String,
        from: String,
    },
    DeputyIsHead {
        unit_id: String,
        position_id: String,
        from: String,
    },
    PositionCycle {
        position_id: String,
        from: String,
    },
    UnitCycle {
        unit_id: String,
        from: String,
    },
    DanglingReference {
        entity: String,
        id: String,
        field: String,
        missing_id: String,
    },
}

/// What ending a position took with it, so the screen can say so.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgEnded {
    #[serde(default)]
    pub reporting_lines: Vec<String>,
    #[serde(default)]
    pub deputy_heads: Vec<String>,
    #[serde(default)]
    pub assignments: Vec<String>,
}

/// The rule that refused a write. `code` is stable and snake_case (the screen
/// keys its message on it), `message` is English for logs and support.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgOpError {
    pub code: String,
    pub message: String,
    /// The entity the rule names, when there is one.
    #[serde(default)]
    pub id: Option<String>,
    /// The request field the rule refused, when it is about one.
    #[serde(default)]
    pub field: Option<String>,
    #[serde(default)]
    pub date: Option<String>,
}

/// The whole structure on a day.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgStructureView {
    pub at: String,
    pub timezone: String,
    #[serde(default)]
    pub units: Vec<OrgUnit>,
    #[serde(default)]
    pub positions: Vec<OrgPosition>,
    #[serde(default)]
    pub assignments: Vec<OrgAssignment>,
    /// Positions nobody holds on the day.
    #[serde(default)]
    pub vacancies: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<OrgWarning>,
}

/// One position met on a chain or in a subtree. `holders` is empty for a
/// vacancy: the position is still part of the line, nobody sits on it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgLink {
    pub position_id: String,
    pub unit_id: String,
    pub name: String,
    /// Steps from the starting position (1 = the next one up or down).
    pub depth: u32,
    #[serde(default)]
    pub holders: Vec<OrgSubject>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgManager {
    pub user_id: String,
    /// The position the manager holds in this relation.
    pub position_id: String,
    /// `primary_holder`, `deputy_head` (a vacant unit head, a deputy head of
    /// the unit stands in) or `deputy` (the holder is away, their deputy does).
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum OrgTarget {
    User(String),
    Position(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrgDirection {
    Up,
    Down,
}

/// Which of a person's seats a downward walk (subordinates, or a reports chain
/// `Down`) starts from; a chain `Up` always climbs from the primary seat. The
/// permission checks follow the primary seat only, so that is the default; the
/// tree shows the secondary and acting seats too and asks for `All`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OrgSeatScope {
    #[default]
    Primary,
    All,
}

/// What a successful write produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum OrgWriteResult {
    Unit(OrgUnit),
    UnitType(OrgUnitType),
    Position(OrgPosition),
    /// `None` when the move left the position without a parent.
    ReportingLine(Option<OrgReportingLine>),
    Assignment(OrgAssignment),
    ExternalPerson(OrgExternalPerson),
    DeputyHeads(Vec<OrgDeputyHead>),
    Settings(OrgSettings),
    Ended(OrgEnded),
    /// The write has no entity to return (an end date, a deleted type).
    Done,
    Deputy(crate::org_structure_cover::OrgDeputy),
    Absence(crate::org_structure_cover::OrgAbsence),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrgFileFormat {
    Csv,
    Xlsx,
}

/// `Upsert` makes what the file says true and touches nothing else, an empty
/// cell meaning "not stated". `Replace` makes the file the whole structure:
/// what it does not mention ends on the import day, an empty cell clears.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OrgImportMode {
    #[default]
    Upsert,
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrgImportAction {
    /// Use the login the report suggested for an unknown one.
    UseSuggestedLogin,
    /// Import the row as a vacant position.
    LeaveVacant,
    /// Leave the row out of the import.
    SkipRow,
}

/// The administrator's decision about one row of a dry run. An apply that
/// carries a decision that does not fit its row is refused in the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgImportResolution {
    pub row: u32,
    pub action: OrgImportAction,
    /// For `use_suggested_login`: the login the administrator was shown. The
    /// decision is refused when it is no longer the suggestion.
    #[serde(default)]
    pub login: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrgImportRowStatus {
    Added,
    Changed,
    Unchanged,
    Error,
}

/// One value a row changes; `entity` is `unit`, `position` or `assignment`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgImportFieldChange {
    pub entity: String,
    pub field: String,
    #[serde(default)]
    pub before: Option<String>,
    #[serde(default)]
    pub after: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgImportCell {
    pub header: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgImportRow {
    /// The row number the spreadsheet shows.
    pub row: u32,
    /// `error` when any error names the row, otherwise what the row does.
    pub status: OrgImportRowStatus,
    /// What the row's operations do (`added`, `changed`, `unchanged`), with
    /// or without an error on it.
    pub effect: OrgImportRowStatus,
    /// The non-empty cells of the row as the file has them, by header, for
    /// "show in file". The worksheet is `OrgImportReport::sheet`.
    #[serde(default)]
    pub cells: Vec<OrgImportCell>,
    #[serde(default)]
    pub unit_code: Option<String>,
    #[serde(default)]
    pub position_code: Option<String>,
    /// The person as the file names them.
    #[serde(default)]
    pub person: Option<String>,
    #[serde(default)]
    pub changes: Vec<OrgImportFieldChange>,
    /// The entities of the row in `preview`.
    #[serde(default)]
    pub unit_id: Option<String>,
    #[serde(default)]
    pub position_id: Option<String>,
}

/// A mistake (or, in `warnings`, a thing to look at) with its place in the file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgImportIssue {
    /// The row to look at; 0 = the file as a whole. For a cycle it is the
    /// last of `rows`, the one to change.
    pub row: u32,
    /// Every row the issue is about, `row` included.
    #[serde(default)]
    pub rows: Vec<u32>,
    /// `unit_code`, `parent_code`, `person`, `share`, `from`... — the column
    /// keys of the file, which the screen translates.
    #[serde(default)]
    pub column: Option<String>,
    /// Stable snake_case kind (`unknown_person`, `unit_cycle`,
    /// `missing_parent_unit`, `two_heads`, `backdated_confirmation_required`,
    /// `rejected`...); the screen keys its message on it.
    pub kind: String,
    /// What the file says there.
    #[serde(default)]
    pub value: Option<String>,
    /// English, for logs and the CSV report.
    pub message: String,
    /// What to use instead: a login for `unknown_person`, a name for `unknown_unit_type`.
    #[serde(default)]
    pub suggestion: Option<String>,
    /// Who the suggested login belongs to.
    #[serde(default)]
    pub suggestion_label: Option<String>,
    /// The rule of the structure that refused an operation (`rejected`),
    /// the same codes as `OrgOpError::code`.
    #[serde(default)]
    pub code: Option<String>,
}

/// Every count defaults to zero, so a counter added later decodes from a peer
/// that does not send it yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct OrgImportCounts {
    /// Rows of the file with content; `added + changed + unchanged == rows`
    /// (each row counted once by what its operations do).
    pub rows: u32,
    pub added: u32,
    pub changed: u32,
    pub unchanged: u32,
    /// ROWS some error names (they overlap with the three above).
    pub errors: u32,
    /// Error ISSUES in `errors` of the report: several per row are possible,
    /// and a cycle names several rows.
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

/// Something a replace ends: `kind` is `unit`, `position` or `assignment`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgImportEnded {
    pub kind: String,
    pub code: String,
    pub name: String,
    /// The people who stop holding a position along with it.
    #[serde(default)]
    pub holders: Vec<String>,
}

/// What a dry run or an apply found. The file itself may be unusable
/// (`file_error`); otherwise every mistake is in `errors` and nothing was
/// written unless `applied`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgImportReport {
    pub mode: OrgImportMode,
    /// The day rows without "od kiedy" take effect, and the day the file is
    /// compared with.
    pub as_of: String,
    /// The day `preview` shows: the latest a row takes effect.
    pub preview_at: String,
    pub applied: bool,
    #[serde(default)]
    pub file_error: Option<OrgOpError>,
    /// The worksheet the rows come from (XLSX); row numbers are that sheet's.
    #[serde(default)]
    pub sheet: Option<String>,
    pub counts: OrgImportCounts,
    /// What a replace ends, cascades included; empty for an upsert.
    #[serde(default)]
    pub ended: Vec<OrgImportEnded>,
    /// The largest file and the most rows an import takes. The file is one
    /// frame and the dashboard socket closes on a frame over 1 MiB, so the
    /// screen must check the size BEFORE it sends.
    #[serde(default)]
    pub max_file_bytes: u32,
    #[serde(default)]
    pub max_rows: u32,
    /// Added, changed and error rows; unchanged rows are only counted.
    #[serde(default)]
    pub rows: Vec<OrgImportRow>,
    #[serde(default)]
    pub errors: Vec<OrgImportIssue>,
    #[serde(default)]
    pub warnings: Vec<OrgImportIssue>,
    /// The structure the file leaves, as `StructureResponse` shows it.
    /// New units and positions have ids that exist only in the preview
    /// until the file is applied.
    #[serde(default)]
    pub preview: Option<OrgStructureView>,
    /// The rows with errors are left out of `preview`, or only the part of
    /// them that is wrong is.
    #[serde(default)]
    pub preview_partial: bool,
}

/// One edit of a batch: any of the write requests below, unchanged, plus the
/// temporary id the edit gives to what it makes.
///
/// `request` is one of `UnitTypeCreateRequest`, `UnitTypeUpdateRequest`,
/// `UnitTypeDeleteRequest`, `UnitCreateRequest`, `UnitUpdateRequest`,
/// `UnitMoveRequest`, `UnitEndRequest`, `HeadSetRequest`,
/// `DeputyHeadsSetRequest`, `PositionCreateRequest`, `PositionUpdateRequest`,
/// `PositionMoveRequest`, `PositionEndRequest`, `ReportingLineSetRequest`,
/// `ExternalPersonCreateRequest`, `AssignRequest`, `AssignmentUpdateRequest`
/// or `AssignmentEndRequest`; any other variant is `not_a_batch_op`.
///
/// Temporary ids: an operation that makes a unit type, a unit, a position, an
/// external person or an assignment may carry `temp_id` = `"tmp:"` + anything
/// (`"tmp:1"`). Every LATER operation of the batch may write that string
/// wherever its request takes the id of such a thing (`parent_unit_id`,
/// `unit_id`, `position_id`, `parent_position_id`, `head_position_id`,
/// `position_ids`, `type_id`, `assignment_id`, the external `subject`); the
/// server swaps it for the real id the earlier operation made. A temporary id
/// is defined once (`duplicate_temp_id`), used only after it is defined
/// (`unknown_temp_id`) and is useless when its operation failed
/// (`temp_id_not_made`). Real ids are never rewritten.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgWriteOp {
    #[serde(default)]
    pub temp_id: Option<String>,
    pub request: OrgStructurePayload,
}

/// What one operation of a batch did. `index` is its position in `ops`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgBatchOpResult {
    pub index: u32,
    pub ok: bool,
    /// The rule that refused the operation; every field is named as in
    /// `WriteResponse::error`.
    #[serde(default)]
    pub error: Option<OrgOpError>,
    /// What the operation returned, as the single write does.
    #[serde(default)]
    pub result: Option<OrgWriteResult>,
    /// The `temp_id` the operation carried, echoed.
    #[serde(default)]
    pub temp_id: Option<String>,
    /// The real id of what the operation made: `unit_id`, `position_id`, the
    /// id of the unit type, external person or assignment.
    #[serde(default)]
    pub created_id: Option<String>,
}

/// Every write request repeats `confirm_backdated`: a change dated before
/// today in the organization's timezone rewrites history and is refused
/// (`backdated_confirmation_required`) until the caller states it meant it.
///
/// In the `*Update` requests a field is changed when its value is `Some`, and
/// cleared when its name is listed in `clear`; a field in neither is left alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OrgStructurePayload {
    // ----- Reads (any member of the organization) -----
    /// The whole structure on a day (default: today in the organization's
    /// timezone), plus the unit types and what the caller may do.
    StructureRequest {
        #[serde(default)]
        at: Option<String>,
    },
    StructureResponse {
        view: OrgStructureView,
        #[serde(default)]
        unit_types: Vec<OrgUnitType>,
        /// What the CALLER may do here (`org.admin`). The dashboard must not
        /// derive it from the session role: the role→permission mapping is
        /// editable, so a client-side copy would drift into offering buttons
        /// the server then refuses.
        #[serde(default)]
        my_permissions: Vec<String>,
        /// The import limits, so the screen checks a file before sending it.
        #[serde(default)]
        import_max_file_bytes: u32,
        #[serde(default)]
        import_max_rows: u32,
        /// The most operations one `BatchRequest` takes.
        #[serde(default)]
        batch_max_ops: u32,
    },
    ReportsChainRequest {
        target: OrgTarget,
        direction: OrgDirection,
        #[serde(default)]
        seat_scope: OrgSeatScope,
        #[serde(default)]
        at: Option<String>,
    },
    ReportsChainResponse {
        #[serde(default)]
        links: Vec<OrgLink>,
    },
    SubordinatesRequest {
        target: OrgTarget,
        #[serde(default)]
        transitive: bool,
        #[serde(default)]
        seat_scope: OrgSeatScope,
        #[serde(default)]
        at: Option<String>,
    },
    SubordinatesResponse {
        #[serde(default)]
        links: Vec<OrgLink>,
    },
    ManagerRequest {
        user_id: String,
        #[serde(default)]
        at: Option<String>,
    },
    ManagerResponse {
        #[serde(default)]
        manager: Option<OrgManager>,
    },
    AssignmentRequest {
        user_id: String,
        #[serde(default)]
        at: Option<String>,
    },
    AssignmentResponse {
        #[serde(default)]
        primary: Option<OrgAssignment>,
        #[serde(default)]
        others: Vec<OrgAssignment>,
    },
    IntegrityReportRequest {
        #[serde(default)]
        at: Option<String>,
    },
    IntegrityReportResponse {
        #[serde(default)]
        violations: Vec<OrgViolation>,
    },

    // ----- Writes (`org.admin`); every one is answered with `WriteResponse` -----
    UnitTypeCreateRequest {
        name: String,
        #[serde(default)]
        color: Option<String>,
        #[serde(default)]
        icon: Option<String>,
    },
    /// `clear` names: `color`, `icon`.
    UnitTypeUpdateRequest {
        id: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        color: Option<String>,
        #[serde(default)]
        icon: Option<String>,
        #[serde(default)]
        clear: Vec<String>,
    },
    UnitTypeDeleteRequest {
        id: String,
    },
    UnitCreateRequest {
        name: String,
        #[serde(default)]
        code: Option<String>,
        #[serde(default)]
        type_id: Option<String>,
        #[serde(default)]
        parent_unit_id: Option<String>,
        #[serde(default)]
        color: Option<String>,
        valid_from: String,
        #[serde(default)]
        valid_to: Option<String>,
        #[serde(default)]
        confirm_backdated: bool,
    },
    /// `clear` names: `code`, `type_id`, `color`.
    UnitUpdateRequest {
        unit_id: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        code: Option<String>,
        #[serde(default)]
        type_id: Option<String>,
        #[serde(default)]
        color: Option<String>,
        #[serde(default)]
        clear: Vec<String>,
        from: String,
        #[serde(default)]
        confirm_backdated: bool,
    },
    /// `new_parent_unit_id: None` makes the unit a root.
    UnitMoveRequest {
        unit_id: String,
        #[serde(default)]
        new_parent_unit_id: Option<String>,
        from: String,
        #[serde(default)]
        confirm_backdated: bool,
    },
    UnitEndRequest {
        unit_id: String,
        from: String,
        #[serde(default)]
        confirm_backdated: bool,
    },
    /// `head_position_id: None` leaves the unit without a head.
    HeadSetRequest {
        unit_id: String,
        #[serde(default)]
        head_position_id: Option<String>,
        from: String,
        #[serde(default)]
        confirm_backdated: bool,
    },
    /// The whole ordered list; an empty list removes every deputy.
    DeputyHeadsSetRequest {
        unit_id: String,
        #[serde(default)]
        position_ids: Vec<String>,
        from: String,
        #[serde(default)]
        confirm_backdated: bool,
    },
    PositionCreateRequest {
        unit_id: String,
        name: String,
        #[serde(default)]
        code: Option<String>,
        #[serde(default)]
        role_id: Option<String>,
        #[serde(default)]
        is_manager: Option<bool>,
        #[serde(default)]
        is_staff: bool,
        /// Creates the primary reporting line together with the position.
        #[serde(default)]
        parent_position_id: Option<String>,
        valid_from: String,
        #[serde(default)]
        valid_to: Option<String>,
        #[serde(default)]
        confirm_backdated: bool,
    },
    /// `clear` names: `code`, `role_id`, `is_manager`.
    PositionUpdateRequest {
        position_id: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        code: Option<String>,
        #[serde(default)]
        role_id: Option<String>,
        #[serde(default)]
        is_manager: Option<bool>,
        #[serde(default)]
        is_staff: Option<bool>,
        #[serde(default)]
        clear: Vec<String>,
        from: String,
        #[serde(default)]
        confirm_backdated: bool,
    },
    /// Re-points the primary reporting line; `None` leaves the position
    /// without a manager.
    PositionMoveRequest {
        position_id: String,
        #[serde(default)]
        new_parent_position_id: Option<String>,
        from: String,
        #[serde(default)]
        confirm_backdated: bool,
    },
    PositionEndRequest {
        position_id: String,
        from: String,
        #[serde(default)]
        confirm_backdated: bool,
    },
    ReportingLineSetRequest {
        position_id: String,
        parent_position_id: String,
        #[serde(default)]
        kind: OrgLineKind,
        #[serde(default)]
        priority: i64,
        valid_from: String,
        #[serde(default)]
        valid_to: Option<String>,
        #[serde(default)]
        confirm_backdated: bool,
    },
    ExternalPersonCreateRequest {
        display_name: String,
        #[serde(default)]
        email: Option<String>,
        #[serde(default)]
        note: Option<String>,
    },
    AssignRequest {
        position_id: String,
        subject: OrgSubject,
        #[serde(default)]
        assignment_type: OrgAssignmentType,
        share: f64,
        /// `None` = primary when the person holds nothing else in the interval.
        #[serde(default)]
        is_primary: Option<bool>,
        valid_from: String,
        #[serde(default)]
        valid_to: Option<String>,
        #[serde(default)]
        confirm_backdated: bool,
    },
    AssignmentUpdateRequest {
        assignment_id: String,
        #[serde(default)]
        assignment_type: Option<OrgAssignmentType>,
        #[serde(default)]
        share: Option<f64>,
        #[serde(default)]
        is_primary: Option<bool>,
        from: String,
        #[serde(default)]
        confirm_backdated: bool,
    },
    AssignmentEndRequest {
        assignment_id: String,
        from: String,
        #[serde(default)]
        confirm_backdated: bool,
    },
    TimezoneSetRequest {
        timezone: String,
    },
    /// Rewrites the derived profiles (department, manager) of the whole
    /// organization from the structure ("Przelicz"). Idempotent.
    RecomputeRequest {},
    RecomputeResponse {
        written: u32,
        removed: u32,
        unchanged: u32,
        /// People whose manager was cleared because their positions make the
        /// reporting of people circular.
        #[serde(default)]
        cycles_broken: Vec<String>,
    },
    WriteResponse {
        ok: bool,
        #[serde(default)]
        error: Option<OrgOpError>,
        #[serde(default)]
        warnings: Vec<OrgWarning>,
        #[serde(default)]
        result: Option<OrgWriteResult>,
    },

    // ----- File import and export (docs §2.5) -----
    /// Tries a file without writing anything (`org.admin`). `as_of` defaults
    /// to today in the organization's timezone. A row dated before today is an
    /// error (`backdated_confirmation_required`) unless `confirm_backdated`.
    /// The file travels in ONE frame of at most 4 MiB; a larger one is
    /// `file_too_large` in the report.
    ImportDryRunRequest {
        format: OrgFileFormat,
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
        #[serde(default)]
        mode: OrgImportMode,
        #[serde(default)]
        as_of: Option<String>,
        #[serde(default)]
        confirm_backdated: bool,
        /// A replace that ends anything is an error until the request says the
        /// administrator saw the report's `ended` list and accepts it.
        #[serde(default)]
        confirm_ended: bool,
        #[serde(default)]
        resolutions: Vec<OrgImportResolution>,
    },
    /// The same run as the dry run, kept when the file has no error: ONE
    /// transaction (all or nothing), one `org.structure_imported` event.
    ImportApplyRequest {
        format: OrgFileFormat,
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
        #[serde(default)]
        mode: OrgImportMode,
        #[serde(default)]
        as_of: Option<String>,
        #[serde(default)]
        confirm_backdated: bool,
        /// A replace that ends anything is an error until the request says the
        /// administrator saw the report's `ended` list and accepts it.
        #[serde(default)]
        confirm_ended: bool,
        #[serde(default)]
        resolutions: Vec<OrgImportResolution>,
    },
    /// The structure on a day in the columns the import reads. Open to every
    /// member; the login and e-mail columns are only in an administrator's file.
    ExportRequest {
        format: OrgFileFormat,
        #[serde(default)]
        at: Option<String>,
    },
    /// The errors of a dry run of the same file, as a CSV (`org.admin`).
    ExportErrorsRequest {
        format: OrgFileFormat,
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
        #[serde(default)]
        mode: OrgImportMode,
        #[serde(default)]
        as_of: Option<String>,
        #[serde(default)]
        confirm_backdated: bool,
        /// A replace that ends anything is an error until the request says the
        /// administrator saw the report's `ended` list and accepts it.
        #[serde(default)]
        confirm_ended: bool,
        #[serde(default)]
        resolutions: Vec<OrgImportResolution>,
    },
    ImportReportResponse {
        report: OrgImportReport,
    },
    ExportResponse {
        file_name: String,
        mime: String,
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
    },

    // ----- Batch of edits (`org.admin`) -----
    /// The edit mode's draft, saved in ONE call: every operation runs in one
    /// transaction, each on its own savepoint, and the batch is kept only when
    /// none failed. `dry_run` runs the same operations and drops them, so the
    /// answer says what saving would do — per-operation errors and the
    /// resulting structure (`preview`) — without changing anything. At most
    /// 500 operations and one frame of 900 KiB; a request over the count is
    /// `too_many_ops`. An operation's own `confirm_backdated` and the batch's
    /// are both honoured (either confirms that operation).
    BatchRequest {
        ops: Vec<OrgWriteOp>,
        #[serde(default)]
        dry_run: bool,
        #[serde(default)]
        confirm_backdated: bool,
    },
    BatchResponse {
        /// Every operation ran and, unless it was a dry run, the batch was saved.
        ok: bool,
        /// Committed. Never true for a dry run or when any operation failed.
        applied: bool,
        /// The batch itself was refused before anything ran (`too_many_ops`).
        #[serde(default)]
        error: Option<OrgOpError>,
        /// One entry per operation, in the order of `ops`.
        #[serde(default)]
        results: Vec<OrgBatchOpResult>,
        /// The warnings that still hold in the resulting structure.
        #[serde(default)]
        warnings: Vec<OrgWarning>,
        /// The day `preview` shows: the latest an operation takes effect,
        /// today when none has a date.
        #[serde(default)]
        preview_at: Option<String>,
        /// The structure the batch leaves on `preview_at`; a dry run only.
        #[serde(default)]
        preview: Option<OrgStructureView>,
        #[serde(default)]
        max_ops: u32,
    },

    // ----- History and planned reorganizations (docs §2.4, §3.4, §6.3) -----
    /// The changes of the structure, newest effective day first, planned ones
    /// (dated after today) on top. `from`/`to` bound the EFFECTIVE day (the day
    /// the entry was written when it has none), `unit_id` limits it to a unit
    /// and the positions and assignments in it. Open to every member; the
    /// position history of a person is left out for anyone but an
    /// administrator and that person (`personal_visible`).
    HistoryListRequest {
        #[serde(default)]
        from: Option<String>,
        #[serde(default)]
        to: Option<String>,
        #[serde(default)]
        unit_id: Option<String>,
        #[serde(default)]
        offset: u32,
        /// 0 = the server's default page; larger values are cut to its maximum.
        #[serde(default)]
        limit: u32,
    },
    HistoryListResponse {
        #[serde(default)]
        entries: Vec<crate::org_history::OrgHistoryEntry>,
        /// Entries matching the filter, before the page was cut.
        #[serde(default)]
        total: u32,
        #[serde(default)]
        personal_visible: bool,
        /// Today in the organization's timezone: where "planned" begins.
        #[serde(default)]
        today: String,
    },
    /// What differs between the structure on two days (`from` may be after
    /// `to`; the answer says what turns the first into the second). Open to
    /// every member with the same privacy rule as `HistoryListRequest`.
    HistoryDiffRequest {
        from: String,
        to: String,
        #[serde(default)]
        unit_id: Option<String>,
    },
    HistoryDiffResponse {
        from: String,
        to: String,
        #[serde(default)]
        items: Vec<crate::org_history::OrgDiffItem>,
        #[serde(default)]
        personal_visible: bool,
    },
    /// The planned reorganizations of the organization (`org.admin`), without
    /// their operations.
    ChangeSetListRequest {},
    ChangeSetListResponse {
        #[serde(default)]
        items: Vec<crate::org_history::OrgChangeSet>,
        /// The caller is the organization's only active `org.admin`, so may
        /// approve a reorganization of their own.
        #[serde(default)]
        sole_admin: bool,
        #[serde(default)]
        today: String,
    },
    /// One planned reorganization with its operations (`org.admin`): what the
    /// edit mode loads to go on writing it. Answered with `ChangeSetResponse`.
    ChangeSetGetRequest {
        id: String,
    },
    /// Creates a draft (`id` absent) or replaces the content of a `draft` or
    /// `pending` one, which puts it back to `draft` and makes the caller its
    /// author (`org.admin`). The draft is kept even when its operations do not
    /// pass a dry run: the answer says which fail (`valid`, `results`), and only
    /// `ChangeSetSubmitRequest` insists on a clean run. Every operation dated
    /// before `effective_date` is reported as `op_before_effective_date`.
    ChangeSetSaveRequest {
        #[serde(default)]
        id: Option<String>,
        name: String,
        effective_date: String,
        ops: Vec<OrgWriteOp>,
    },
    /// draft -> pending; refused (`change_set_invalid`) unless the operations
    /// pass a dry run now and (`effective_date_passed`) the day is still ahead.
    ChangeSetSubmitRequest {
        id: String,
    },
    /// pending -> applied by an administrator who is not its author
    /// (`self_approval`): the operations run as ONE transaction through the
    /// batch machinery together with the state change. A run that fails leaves
    /// the structure and the state untouched (`change_set_conflict`, with the
    /// per-operation `results`).
    ChangeSetApproveRequest {
        id: String,
    },
    /// draft or pending -> withdrawn; the live structure is not touched. An
    /// applied one cannot be withdrawn (`change_set_state`).
    ChangeSetWithdrawRequest {
        id: String,
    },
    /// The structure on the reorganization's day with and without it: a dry run
    /// of its operations (`org.admin`), the differences and the operations that
    /// would fail today. `unit_id` limits `items`.
    ChangeSetPreviewRequest {
        id: String,
        #[serde(default)]
        unit_id: Option<String>,
    },
    ChangeSetResponse {
        ok: bool,
        #[serde(default)]
        error: Option<OrgOpError>,
        #[serde(default)]
        change_set: Option<crate::org_history::OrgChangeSet>,
        /// The operations pass a dry run against the live structure now.
        #[serde(default)]
        valid: bool,
        #[serde(default)]
        results: Vec<OrgBatchOpResult>,
        #[serde(default)]
        warnings: Vec<OrgWarning>,
    },
    ChangeSetPreviewResponse {
        ok: bool,
        #[serde(default)]
        error: Option<OrgOpError>,
        #[serde(default)]
        change_set: Option<crate::org_history::OrgChangeSet>,
        #[serde(default)]
        valid: bool,
        #[serde(default)]
        results: Vec<OrgBatchOpResult>,
        #[serde(default)]
        warnings: Vec<OrgWarning>,
        /// The day both views show.
        #[serde(default)]
        at: String,
        /// The live structure on `at`, without the reorganization.
        #[serde(default)]
        live: Option<OrgStructureView>,
        /// The structure on `at` with the reorganization applied.
        #[serde(default)]
        preview: Option<OrgStructureView>,
        #[serde(default)]
        items: Vec<crate::org_history::OrgDiffItem>,
    },

    // ----- Deputies, absences, escalation and visibility (docs §1, §2.1a, §6.3) -----
    // Types are in `org_structure_cover`. Reads are open to every member and
    // filtered by privacy on the server; `DeputySet`/`Update`/`End` need
    // `org.admin`, the absence writes are the person's own or `org.admin`.
    /// A person's absences and deputies. `user_id` defaults to the caller.
    /// `include_past` also lists absences that ended before `at`.
    CoverRequest {
        #[serde(default)]
        user_id: Option<String>,
        #[serde(default)]
        at: Option<String>,
        #[serde(default)]
        include_past: bool,
    },
    CoverResponse {
        user_id: String,
        #[serde(default)]
        display_name: String,
        /// `org.is_available` on `today`: an active member, no absence.
        available: bool,
        /// The day the answer is for.
        today: String,
        /// Empty unless the caller may see the dates (`can_see_absences`).
        #[serde(default)]
        absences: Vec<crate::org_structure_cover::OrgAbsence>,
        /// Deputies (in force on `today` or later) who cover the person.
        #[serde(default)]
        covered_by: Vec<crate::org_structure_cover::OrgDeputy>,
        /// The people this person covers.
        #[serde(default)]
        covering: Vec<crate::org_structure_cover::OrgDeputy>,
        #[serde(default)]
        can_see_absences: bool,
        /// Deprecated, always false: an absence has no reason.
        #[serde(default)]
        can_see_reason: bool,
        /// The caller may add, change and delete this person's absences.
        #[serde(default)]
        can_edit_absences: bool,
        /// The caller may add, change and end this person's deputies: the person
        /// themselves or `org.admin`. A manager may not.
        #[serde(default)]
        can_edit_deputies: bool,
        /// The caller holds `org.admin` (may also change deputies of others).
        #[serde(default)]
        is_admin: bool,
    },
    /// Who is away and which deputies are in force on a day — what the tree
    /// badges. No reason and no kind.
    AvailabilityRequest {
        #[serde(default)]
        at: Option<String>,
    },
    AvailabilityResponse {
        at: String,
        #[serde(default)]
        absent_user_ids: Vec<String>,
        #[serde(default)]
        deputies: Vec<crate::org_structure_cover::OrgDeputy>,
    },
    /// `scope` is `all`, `approvals`, `escalations` (the default) or `project:<id>`.
    EscalationChainRequest {
        user_id: String,
        #[serde(default)]
        scope: Option<String>,
        #[serde(default)]
        at: Option<String>,
    },
    EscalationChainResponse {
        #[serde(default)]
        steps: Vec<crate::org_structure_cover::OrgEscalationStep>,
        #[serde(default)]
        skipped: Vec<crate::org_structure_cover::OrgEscalationSkip>,
        #[serde(default)]
        problem: Option<crate::org_structure_cover::OrgEscalationProblem>,
    },
    IsAvailableRequest {
        user_id: String,
        #[serde(default)]
        at: Option<String>,
    },
    IsAvailableResponse {
        available: bool,
    },
    /// `org.can_view_person_data`. `kind`: `absence_dates`,
    /// `time_utilization`, `position_history`. Asking for a viewer other than
    /// the caller needs `org.admin`.
    CanViewPersonDataRequest {
        #[serde(default)]
        viewer_user_id: Option<String>,
        subject_user_id: String,
        kind: String,
        /// The day the structure is read on; default today.
        #[serde(default)]
        at: Option<String>,
    },
    CanViewPersonDataResponse {
        allowed: bool,
        /// `owner`, `primary_manager`, `supervisor`, `administrator` or `none`.
        rule: String,
    },
    /// What a person sees, area by area, with the rule behind each answer.
    /// `user_id` defaults to the caller; another person needs `org.admin`.
    VisibilityRequest {
        #[serde(default)]
        user_id: Option<String>,
        #[serde(default)]
        at: Option<String>,
    },
    VisibilityResponse {
        user: crate::org_structure_cover::OrgPersonRef,
        #[serde(default)]
        manager: Option<crate::org_structure_cover::OrgPersonRef>,
        /// Everybody below the person on the primary line.
        #[serde(default)]
        subtree: Vec<crate::org_structure_cover::OrgPersonRef>,
        /// The ones one step below.
        #[serde(default)]
        direct: Vec<crate::org_structure_cover::OrgPersonRef>,
        #[serde(default)]
        rows: Vec<crate::org_structure_cover::OrgVisibilityRow>,
    },
    /// Who sees the personal data of a person. `subject_user_id` defaults to
    /// the caller; another person needs `org.admin`.
    WhoSeesRequest {
        #[serde(default)]
        subject_user_id: Option<String>,
        #[serde(default)]
        at: Option<String>,
    },
    WhoSeesResponse {
        subject: crate::org_structure_cover::OrgPersonRef,
        #[serde(default)]
        viewers: Vec<crate::org_structure_cover::OrgViewer>,
    },
    /// The covered person themselves (`user_id` = caller) or `org.admin`.
    /// `valid_to` is exclusive.
    DeputySetRequest {
        user_id: String,
        deputy_user_id: String,
        /// `all`, `approvals`, `escalations` or `project:<id>`.
        scope: String,
        valid_from: String,
        #[serde(default)]
        valid_to: Option<String>,
        #[serde(default)]
        confirm_backdated: bool,
    },
    /// The covered person or `org.admin`. `clear` names: `valid_to`.
    DeputyUpdateRequest {
        id: String,
        #[serde(default)]
        scope: Option<String>,
        #[serde(default)]
        valid_from: Option<String>,
        #[serde(default)]
        valid_to: Option<String>,
        #[serde(default)]
        clear: Vec<String>,
        #[serde(default)]
        confirm_backdated: bool,
    },
    /// The covered person or `org.admin`. Covering stops on `from`.
    DeputyEndRequest {
        id: String,
        from: String,
        #[serde(default)]
        confirm_backdated: bool,
    },
    /// The caller's own absence, or anybody's for `org.admin`. `user_id`
    /// defaults to the caller; `valid_to` is exclusive.
    AbsenceAddRequest {
        #[serde(default)]
        user_id: Option<String>,
        valid_from: String,
        #[serde(default)]
        valid_to: Option<String>,
        kind: crate::org_structure_cover::OrgAbsenceKind,
        /// Deprecated: an absence has no reason; a non-empty value is refused.
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        confirm_backdated: bool,
    },
    /// The caller's own manual absence, or any for `org.admin`. `clear` names:
    /// `valid_to`.
    AbsenceUpdateRequest {
        id: String,
        #[serde(default)]
        valid_from: Option<String>,
        #[serde(default)]
        valid_to: Option<String>,
        #[serde(default)]
        kind: Option<crate::org_structure_cover::OrgAbsenceKind>,
        /// Deprecated: an absence has no reason; a non-empty value is refused.
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        clear: Vec<String>,
        #[serde(default)]
        confirm_backdated: bool,
    },
    AbsenceDeleteRequest {
        id: String,
        #[serde(default)]
        confirm_backdated: bool,
    },

    // ----- Handover of everything a person holds (docs §2.6) -----
    // Types are in `org_structure_handover`. `Departure` needs `org.admin`,
    // `ProjectRemoval` the manager role in that project, `Absence` the person,
    // their manager on the primary line or `org.admin`.
    /// What `user_id` holds for `reason`, grouped, with a suggested taker for
    /// each item. `date` is the departure day (default today); `return_date`
    /// the first day back for an absence.
    HandoverListRequest {
        user_id: String,
        reason: crate::org_structure_handover::OrgHandoverReason,
        #[serde(default)]
        project_id: Option<String>,
        #[serde(default)]
        date: Option<String>,
        #[serde(default)]
        return_date: Option<String>,
    },
    HandoverListResponse {
        user: crate::org_structure_cover::OrgPersonRef,
        reason: crate::org_structure_handover::OrgHandoverReason,
        date: String,
        #[serde(default)]
        return_date: Option<String>,
        /// The day the person's last assignment ended, when it already has.
        #[serde(default)]
        assignment_ended_on: Option<String>,
        #[serde(default)]
        project_name: Option<String>,
        #[serde(default)]
        groups: Vec<crate::org_structure_handover::OrgHandoverGroup>,
        /// Everyone who may take something: active members but the person.
        #[serde(default)]
        takers: Vec<crate::org_structure_cover::OrgPersonRef>,
        /// Names of projects left out of `groups` because their task index did not settle.
        #[serde(default)]
        skipped_projects: Vec<String>,
    },
    /// Moves the chosen items and records the handover. `note` is required.
    /// Not atomic across stores: see `HandoverApplyResponse`.
    HandoverApplyRequest {
        user_id: String,
        reason: crate::org_structure_handover::OrgHandoverReason,
        #[serde(default)]
        project_id: Option<String>,
        #[serde(default)]
        date: Option<String>,
        #[serde(default)]
        return_date: Option<String>,
        note: String,
        #[serde(default)]
        items: Vec<crate::org_structure_handover::OrgHandoverChoice>,
    },
    /// `ok: false` with an `error` = nothing was moved. `ok: false` with a
    /// `handover_id` = some items failed: retry them with `HandoverRetryRequest`.
    HandoverApplyResponse {
        ok: bool,
        #[serde(default)]
        handover_id: Option<String>,
        #[serde(default)]
        error: Option<OrgOpError>,
        #[serde(default)]
        items: Vec<crate::org_structure_handover::OrgHandoverItemResult>,
        #[serde(default)]
        applied: u32,
        #[serde(default)]
        scheduled: u32,
        #[serde(default)]
        failed: u32,
    },
    /// Tries the failed items of a handover again, with the takers recorded.
    HandoverRetryRequest {
        handover_id: String,
        #[serde(default)]
        keys: Vec<String>,
    },
    /// `org.admin`: the people whose assignment ended and who still hold work.
    HandoverPendingRequest {},
    HandoverPendingResponse {
        #[serde(default)]
        people: Vec<crate::org_structure_handover::OrgHandoverPending>,
    },
    /// The handovers made for `user_id` (default the caller), newest first.
    HandoverRecordsRequest {
        #[serde(default)]
        user_id: Option<String>,
    },
    HandoverRecordsResponse {
        #[serde(default)]
        records: Vec<crate::org_structure_handover::OrgHandoverRecord>,
    },
    /// The active members of the organization, for choosing a deputy. Open to
    /// every member: the person being covered picks their own deputy.
    MemberListRequest {},
    MemberListResponse {
        #[serde(default)]
        members: Vec<crate::org_structure_cover::OrgPersonRef>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_body::MessageBody;

    fn roundtrip(payload: OrgStructurePayload) {
        let body = MessageBody::OrgStructureBody(payload);
        let mut buf = Vec::new();
        ciborium::into_writer(&body, &mut buf).unwrap();
        let decoded: MessageBody = ciborium::from_reader(buf.as_slice()).unwrap();
        assert_eq!(decoded, body);
    }

    fn s(v: &str) -> String {
        v.to_string()
    }

    fn unit() -> OrgUnit {
        OrgUnit {
            id: s("u-row"),
            unit_id: s("u-1"),
            name: s("Dział IT"),
            code: Some(s("IT")),
            type_id: Some(s("t-1")),
            parent_unit_id: Some(s("u-0")),
            color: Some(s("#3b82f6")),
            head_position_id: Some(s("p-1")),
            deputy_head_position_ids: vec![s("p-2"), s("p-3")],
            valid_from: s("2026-01-01"),
            valid_to: Some(s("2027-01-01")),
        }
    }

    fn position() -> OrgPosition {
        OrgPosition {
            id: s("p-row"),
            position_id: s("p-1"),
            unit_id: s("u-1"),
            name: s("Kierownik"),
            code: Some(s("IT-KIER")),
            role_id: Some(s("role-1")),
            is_manager: Some(true),
            is_staff: false,
            valid_from: s("2026-01-01"),
            valid_to: None,
            primary_parent_position_id: Some(s("p-0")),
            functional_parent_position_ids: vec![s("p-9")],
            is_head: true,
            is_vacant: false,
        }
    }

    fn assignment() -> OrgAssignment {
        OrgAssignment {
            id: s("a-1"),
            position_id: s("p-1"),
            subject: OrgSubject::User(s("user-1")),
            assignment_type: OrgAssignmentType::Acting,
            share: 0.5,
            is_primary: true,
            valid_from: s("2026-01-01"),
            valid_to: None,
            display_name: s("Anna Nowak"),
        }
    }

    fn line() -> OrgReportingLine {
        OrgReportingLine {
            id: s("l-1"),
            position_id: s("p-1"),
            parent_position_id: s("p-0"),
            kind: OrgLineKind::Functional,
            priority: 2,
            valid_from: s("2026-01-01"),
            valid_to: Some(s("2026-06-01")),
        }
    }

    fn write_response(result: Option<OrgWriteResult>) -> OrgStructurePayload {
        OrgStructurePayload::WriteResponse {
            ok: true,
            error: None,
            warnings: vec![OrgWarning::UnitWithoutHead {
                unit_id: s("u-1"),
                from: s("2026-01-01"),
            }],
            result,
        }
    }

    #[test]
    fn read_requests_roundtrip() {
        use OrgStructurePayload as P;
        roundtrip(P::StructureRequest {
            at: Some(s("2026-05-01")),
        });
        roundtrip(P::StructureRequest { at: None });
        roundtrip(P::ReportsChainRequest {
            target: OrgTarget::User(s("user-1")),
            direction: OrgDirection::Up,
            seat_scope: OrgSeatScope::Primary,
            at: None,
        });
        roundtrip(P::ReportsChainRequest {
            target: OrgTarget::Position(s("p-1")),
            direction: OrgDirection::Down,
            seat_scope: OrgSeatScope::All,
            at: Some(s("2026-05-01")),
        });
        roundtrip(P::SubordinatesRequest {
            target: OrgTarget::Position(s("p-1")),
            transitive: true,
            seat_scope: OrgSeatScope::All,
            at: None,
        });
        roundtrip(P::ManagerRequest {
            user_id: s("user-1"),
            at: None,
        });
        roundtrip(P::AssignmentRequest {
            user_id: s("user-1"),
            at: Some(s("2026-05-01")),
        });
        roundtrip(P::IntegrityReportRequest { at: None });
        roundtrip(P::RecomputeRequest {});
    }

    #[test]
    fn read_responses_roundtrip() {
        use OrgStructurePayload as P;
        roundtrip(P::StructureResponse {
            view: OrgStructureView {
                at: s("2026-05-01"),
                timezone: s("Europe/Warsaw"),
                units: vec![unit()],
                positions: vec![position()],
                assignments: vec![assignment()],
                vacancies: vec![s("p-7")],
                warnings: vec![
                    OrgWarning::ShareOverbooked {
                        subject: OrgSubject::External(s("ext-1")),
                        from: s("2026-05-01"),
                        total: 1.25,
                    },
                    OrgWarning::PersonWithoutPrimary {
                        subject: OrgSubject::User(s("user-1")),
                        from: s("2026-05-01"),
                    },
                ],
            },
            unit_types: vec![OrgUnitType {
                id: s("t-1"),
                name: s("Dział"),
                color: None,
                icon: Some(s("building")),
            }],
            my_permissions: vec![s("org.admin")],
            import_max_file_bytes: 921_600,
            import_max_rows: 5_000,
            batch_max_ops: 500,
        });
        let link = OrgLink {
            position_id: s("p-0"),
            unit_id: s("u-0"),
            name: s("Dyrektor"),
            depth: 1,
            holders: vec![
                OrgSubject::User(s("user-9")),
                OrgSubject::External(s("ext-1")),
            ],
        };
        roundtrip(P::ReportsChainResponse {
            links: vec![link.clone()],
        });
        roundtrip(P::SubordinatesResponse { links: vec![link] });
        roundtrip(P::ManagerResponse {
            manager: Some(OrgManager {
                user_id: s("user-9"),
                position_id: s("p-0"),
                source: s("deputy_head"),
            }),
        });
        roundtrip(P::ManagerResponse { manager: None });
        roundtrip(P::AssignmentResponse {
            primary: Some(assignment()),
            others: vec![assignment()],
        });
        roundtrip(P::IntegrityReportResponse {
            violations: vec![
                OrgViolation::VersionsOverlap {
                    entity: s("unit"),
                    id: s("u-1"),
                    from: s("2026-01-01"),
                },
                OrgViolation::PrimaryLinesOverlap {
                    position_id: s("p-1"),
                    from: s("2026-01-01"),
                },
                OrgViolation::PrimaryAssignmentsOverlap {
                    subject: OrgSubject::User(s("user-1")),
                    from: s("2026-01-01"),
                },
                OrgViolation::PositionHeadsMultipleUnits {
                    position_id: s("p-1"),
                    from: s("2026-01-01"),
                },
                OrgViolation::DeputyIsHead {
                    unit_id: s("u-1"),
                    position_id: s("p-1"),
                    from: s("2026-01-01"),
                },
                OrgViolation::PositionCycle {
                    position_id: s("p-1"),
                    from: s("2026-01-01"),
                },
                OrgViolation::UnitCycle {
                    unit_id: s("u-1"),
                    from: s("2026-01-01"),
                },
                OrgViolation::DanglingReference {
                    entity: s("position"),
                    id: s("p-1"),
                    field: s("unit_id"),
                    missing_id: s("u-404"),
                },
            ],
        });
        roundtrip(P::RecomputeResponse {
            written: 3,
            removed: 1,
            unchanged: 40,
            cycles_broken: vec![s("user-3")],
        });
    }

    #[test]
    fn write_requests_roundtrip() {
        use OrgStructurePayload as P;
        roundtrip(P::UnitTypeCreateRequest {
            name: s("Dział"),
            color: Some(s("#fff")),
            icon: None,
        });
        roundtrip(P::UnitTypeUpdateRequest {
            id: s("t-1"),
            name: Some(s("Pion")),
            color: None,
            icon: Some(s("building")),
            clear: vec![s("color")],
        });
        roundtrip(P::UnitTypeDeleteRequest { id: s("t-1") });
        roundtrip(P::UnitCreateRequest {
            name: s("IT"),
            code: Some(s("IT")),
            type_id: Some(s("t-1")),
            parent_unit_id: Some(s("u-0")),
            color: None,
            valid_from: s("2026-01-01"),
            valid_to: Some(s("2030-01-01")),
            confirm_backdated: true,
        });
        roundtrip(P::UnitUpdateRequest {
            unit_id: s("u-1"),
            name: Some(s("IT 2")),
            code: None,
            type_id: None,
            color: None,
            clear: vec![s("code"), s("type_id")],
            from: s("2026-02-01"),
            confirm_backdated: false,
        });
        roundtrip(P::UnitMoveRequest {
            unit_id: s("u-1"),
            new_parent_unit_id: None,
            from: s("2026-02-01"),
            confirm_backdated: false,
        });
        roundtrip(P::UnitEndRequest {
            unit_id: s("u-1"),
            from: s("2026-02-01"),
            confirm_backdated: true,
        });
        roundtrip(P::HeadSetRequest {
            unit_id: s("u-1"),
            head_position_id: Some(s("p-1")),
            from: s("2026-02-01"),
            confirm_backdated: false,
        });
        roundtrip(P::DeputyHeadsSetRequest {
            unit_id: s("u-1"),
            position_ids: vec![s("p-2"), s("p-3")],
            from: s("2026-02-01"),
            confirm_backdated: false,
        });
        roundtrip(P::PositionCreateRequest {
            unit_id: s("u-1"),
            name: s("Analityk"),
            code: Some(s("IT-AN")),
            role_id: Some(s("role-1")),
            is_manager: Some(false),
            is_staff: true,
            parent_position_id: Some(s("p-1")),
            valid_from: s("2026-01-01"),
            valid_to: None,
            confirm_backdated: false,
        });
        roundtrip(P::PositionUpdateRequest {
            position_id: s("p-1"),
            name: Some(s("Starszy analityk")),
            code: None,
            role_id: None,
            is_manager: None,
            is_staff: Some(false),
            clear: vec![s("code"), s("role_id"), s("is_manager")],
            from: s("2026-02-01"),
            confirm_backdated: false,
        });
        roundtrip(P::PositionMoveRequest {
            position_id: s("p-1"),
            new_parent_position_id: Some(s("p-0")),
            from: s("2026-02-01"),
            confirm_backdated: false,
        });
        roundtrip(P::PositionEndRequest {
            position_id: s("p-1"),
            from: s("2026-02-01"),
            confirm_backdated: false,
        });
        roundtrip(P::ReportingLineSetRequest {
            position_id: s("p-1"),
            parent_position_id: s("p-0"),
            kind: OrgLineKind::Functional,
            priority: 1,
            valid_from: s("2026-01-01"),
            valid_to: None,
            confirm_backdated: false,
        });
        roundtrip(P::ExternalPersonCreateRequest {
            display_name: s("Jan Kowalski"),
            email: Some(s("jan@example.com")),
            note: None,
        });
        roundtrip(P::AssignRequest {
            position_id: s("p-1"),
            subject: OrgSubject::External(s("ext-1")),
            assignment_type: OrgAssignmentType::Contractor,
            share: 0.4,
            is_primary: Some(false),
            valid_from: s("2026-01-01"),
            valid_to: Some(s("2026-12-31")),
            confirm_backdated: false,
        });
        roundtrip(P::AssignmentUpdateRequest {
            assignment_id: s("a-1"),
            assignment_type: Some(OrgAssignmentType::Permanent),
            share: Some(1.0),
            is_primary: Some(true),
            from: s("2026-02-01"),
            confirm_backdated: false,
        });
        roundtrip(P::AssignmentEndRequest {
            assignment_id: s("a-1"),
            from: s("2026-02-01"),
            confirm_backdated: true,
        });
        roundtrip(P::TimezoneSetRequest {
            timezone: s("Europe/Berlin"),
        });
    }

    #[test]
    fn write_responses_roundtrip() {
        use OrgStructurePayload as P;
        roundtrip(write_response(Some(OrgWriteResult::Unit(unit()))));
        roundtrip(write_response(Some(OrgWriteResult::UnitType(
            OrgUnitType {
                id: s("t-1"),
                name: s("Dział"),
                color: Some(s("#fff")),
                icon: None,
            },
        ))));
        roundtrip(write_response(Some(OrgWriteResult::Position(position()))));
        roundtrip(write_response(Some(OrgWriteResult::ReportingLine(Some(
            line(),
        )))));
        roundtrip(write_response(Some(OrgWriteResult::ReportingLine(None))));
        roundtrip(write_response(Some(OrgWriteResult::Assignment(
            assignment(),
        ))));
        roundtrip(write_response(Some(OrgWriteResult::ExternalPerson(
            OrgExternalPerson {
                id: s("ext-1"),
                display_name: s("Jan Kowalski"),
                email: None,
                note: Some(s("kontraktor")),
            },
        ))));
        roundtrip(write_response(Some(OrgWriteResult::DeputyHeads(vec![
            OrgDeputyHead {
                id: s("d-1"),
                unit_id: s("u-1"),
                position_id: s("p-2"),
                ord: 1,
                valid_from: s("2026-01-01"),
                valid_to: None,
            },
        ]))));
        roundtrip(write_response(Some(OrgWriteResult::Settings(
            OrgSettings {
                org_id: s("org-default"),
                timezone: s("Europe/Warsaw"),
            },
        ))));
        roundtrip(write_response(Some(OrgWriteResult::Ended(OrgEnded {
            reporting_lines: vec![s("l-1")],
            deputy_heads: vec![s("d-1")],
            assignments: vec![s("a-1")],
        }))));
        roundtrip(write_response(Some(OrgWriteResult::Done)));
        roundtrip(P::WriteResponse {
            ok: false,
            error: Some(OrgOpError {
                code: s("backdated_confirmation_required"),
                message: s("change dated 2026-01-01 is before today"),
                id: None,
                field: Some(s("valid_from")),
                date: Some(s("2026-01-01")),
            }),
            warnings: Vec::new(),
            result: None,
        });
    }

    fn import_issue() -> OrgImportIssue {
        OrgImportIssue {
            row: 47,
            rows: vec![47],
            column: Some(s("person")),
            kind: s("unknown_person"),
            value: Some(s("j.kowlski")),
            message: s("no account 'j.kowlski'"),
            suggestion: Some(s("j.kowalski")),
            suggestion_label: Some(s("Jan Kowalski")),
            code: None,
        }
    }

    #[test]
    fn import_requests_roundtrip_with_the_file_as_a_byte_string() {
        use OrgStructurePayload as P;
        let bytes = b"kod jednostki;nazwa jednostki\nIT;Dzia\xc5\x82 IT\n".to_vec();
        for payload in [
            P::ImportDryRunRequest {
                format: OrgFileFormat::Csv,
                bytes: bytes.clone(),
                mode: OrgImportMode::Replace,
                as_of: Some(s("2026-11-01")),
                confirm_backdated: true,
                confirm_ended: true,
                resolutions: vec![OrgImportResolution {
                    row: 47,
                    action: OrgImportAction::UseSuggestedLogin,
                    login: Some(s("j.kowalski")),
                }],
            },
            P::ImportApplyRequest {
                format: OrgFileFormat::Xlsx,
                bytes: bytes.clone(),
                mode: OrgImportMode::Upsert,
                as_of: None,
                confirm_backdated: false,
                confirm_ended: false,
                resolutions: vec![
                    OrgImportResolution {
                        row: 3,
                        action: OrgImportAction::LeaveVacant,
                        login: None,
                    },
                    OrgImportResolution {
                        row: 4,
                        action: OrgImportAction::SkipRow,
                        login: None,
                    },
                ],
            },
            P::ExportRequest {
                format: OrgFileFormat::Xlsx,
                at: Some(s("2026-01-01")),
            },
            P::ExportErrorsRequest {
                format: OrgFileFormat::Csv,
                bytes: bytes.clone(),
                mode: OrgImportMode::Upsert,
                as_of: None,
                confirm_backdated: false,
                confirm_ended: false,
                resolutions: vec![],
            },
        ] {
            roundtrip(payload);
        }

        // A CBOR byte string is length-prefixed; an array of integers would
        // be about twice the size and the upload limit is counted in bytes.
        let mut buf = Vec::new();
        ciborium::into_writer(
            &MessageBody::OrgStructureBody(P::ImportDryRunRequest {
                format: OrgFileFormat::Csv,
                bytes: vec![200; 1000],
                mode: OrgImportMode::Upsert,
                as_of: None,
                confirm_backdated: false,
                confirm_ended: false,
                resolutions: vec![],
            }),
            &mut buf,
        )
        .unwrap();
        assert!(
            buf.len() < 1200,
            "file bytes encoded in {} bytes",
            buf.len()
        );
    }

    #[test]
    fn import_responses_roundtrip() {
        use OrgStructurePayload as P;
        let report = OrgImportReport {
            mode: OrgImportMode::Upsert,
            as_of: s("2026-11-01"),
            preview_at: s("2026-11-01"),
            applied: false,
            file_error: None,
            sheet: Some(s("Osoby")),
            ended: vec![OrgImportEnded {
                kind: s("position"),
                code: s("IT-2"),
                name: s("Analityk"),
                holders: vec![s("Anna Nowak")],
            }],
            max_file_bytes: 921_600,
            max_rows: 5_000,
            counts: OrgImportCounts {
                rows: 212,
                added: 184,
                changed: 12,
                unchanged: 13,
                errors: 3,
                assignments_added: 180,
                ..Default::default()
            },
            rows: vec![OrgImportRow {
                row: 47,
                status: OrgImportRowStatus::Error,
                effect: OrgImportRowStatus::Added,
                cells: vec![OrgImportCell {
                    header: s("login/e-mail osoby"),
                    value: s("j.kowlski"),
                }],
                unit_code: Some(s("NX")),
                position_code: Some(s("NX-1")),
                person: Some(s("j.kowlski")),
                changes: vec![OrgImportFieldChange {
                    entity: s("assignment"),
                    field: s("share"),
                    before: Some(s("1")),
                    after: None,
                }],
                unit_id: Some(s("u-1")),
                position_id: None,
            }],
            errors: vec![import_issue()],
            warnings: vec![OrgImportIssue {
                kind: s("unit_without_head"),
                ..import_issue()
            }],
            preview: Some(OrgStructureView {
                at: s("2026-11-01"),
                timezone: s("Europe/Warsaw"),
                units: vec![unit()],
                positions: vec![position()],
                assignments: vec![assignment()],
                vacancies: vec![],
                warnings: vec![],
            }),
            preview_partial: true,
        };
        roundtrip(P::ImportReportResponse {
            report: report.clone(),
        });
        roundtrip(P::ImportReportResponse {
            report: OrgImportReport {
                file_error: Some(OrgOpError {
                    code: s("file_too_large"),
                    message: s("the file is too large"),
                    ..Default::default()
                }),
                preview: None,
                ..report
            },
        });
        roundtrip(P::ExportResponse {
            file_name: s("org-structure-2026-11-01.csv"),
            mime: s("text/csv; charset=utf-8"),
            bytes: vec![0xef, 0xbb, 0xbf, b'a'],
        });
    }

    #[test]
    fn import_requests_default_their_optional_fields_when_absent() {
        // A peer that omits `mode` and the resolutions asks for a plain upsert.
        let json = serde_json::json!({
            "ImportDryRunRequest": { "format": "csv", "bytes": [97, 59, 98] }
        });
        let decoded: OrgStructurePayload = serde_json::from_value(json).unwrap();
        assert_eq!(
            decoded,
            OrgStructurePayload::ImportDryRunRequest {
                format: OrgFileFormat::Csv,
                bytes: b"a;b".to_vec(),
                mode: OrgImportMode::Upsert,
                as_of: None,
                confirm_backdated: false,
                confirm_ended: false,
                resolutions: vec![],
            }
        );
    }

    #[test]
    fn optional_fields_default_when_absent() {
        // A peer built before an optional field existed omits the key.
        let json = serde_json::json!({ "UnitCreateRequest": { "name": "IT", "valid_from": "2026-01-01" } });
        let decoded: OrgStructurePayload = serde_json::from_value(json).unwrap();
        assert_eq!(
            decoded,
            OrgStructurePayload::UnitCreateRequest {
                name: s("IT"),
                code: None,
                type_id: None,
                parent_unit_id: None,
                color: None,
                valid_from: s("2026-01-01"),
                valid_to: None,
                confirm_backdated: false,
            }
        );
    }

    /// A rename or a reorder is invisible to the round-trip tests above and
    /// breaks every deployed peer, so the names are pinned in declaration order.
    #[test]
    fn org_structure_payload_variant_names_are_pinned_in_order() {
        const SOURCE: &str = include_str!("org_structure.rs");
        crate::wire_pin::assert_parseable(SOURCE);
        let payload = crate::wire_pin::wire_enums(SOURCE)
            .into_iter()
            .find(|item| item.name == "OrgStructurePayload")
            .expect("OrgStructurePayload is declared in org_structure.rs");
        let live: Vec<String> = payload
            .members
            .iter()
            .map(|m| {
                let head = m.split(" | ").next().unwrap_or(m);
                head.split_whitespace()
                    .filter(|token| *token != "{}")
                    .last()
                    .unwrap_or(head)
                    .to_string()
            })
            .collect();
        let pinned = [
            "StructureRequest",
            "StructureResponse",
            "ReportsChainRequest",
            "ReportsChainResponse",
            "SubordinatesRequest",
            "SubordinatesResponse",
            "ManagerRequest",
            "ManagerResponse",
            "AssignmentRequest",
            "AssignmentResponse",
            "IntegrityReportRequest",
            "IntegrityReportResponse",
            "UnitTypeCreateRequest",
            "UnitTypeUpdateRequest",
            "UnitTypeDeleteRequest",
            "UnitCreateRequest",
            "UnitUpdateRequest",
            "UnitMoveRequest",
            "UnitEndRequest",
            "HeadSetRequest",
            "DeputyHeadsSetRequest",
            "PositionCreateRequest",
            "PositionUpdateRequest",
            "PositionMoveRequest",
            "PositionEndRequest",
            "ReportingLineSetRequest",
            "ExternalPersonCreateRequest",
            "AssignRequest",
            "AssignmentUpdateRequest",
            "AssignmentEndRequest",
            "TimezoneSetRequest",
            "RecomputeRequest",
            "RecomputeResponse",
            "WriteResponse",
            "ImportDryRunRequest",
            "ImportApplyRequest",
            "ExportRequest",
            "ExportErrorsRequest",
            "ImportReportResponse",
            "ExportResponse",
            "BatchRequest",
            "BatchResponse",
            "HistoryListRequest",
            "HistoryListResponse",
            "HistoryDiffRequest",
            "HistoryDiffResponse",
            "ChangeSetListRequest",
            "ChangeSetListResponse",
            "ChangeSetGetRequest",
            "ChangeSetSaveRequest",
            "ChangeSetSubmitRequest",
            "ChangeSetApproveRequest",
            "ChangeSetWithdrawRequest",
            "ChangeSetPreviewRequest",
            "ChangeSetResponse",
            "ChangeSetPreviewResponse",
            "CoverRequest",
            "CoverResponse",
            "AvailabilityRequest",
            "AvailabilityResponse",
            "EscalationChainRequest",
            "EscalationChainResponse",
            "IsAvailableRequest",
            "IsAvailableResponse",
            "CanViewPersonDataRequest",
            "CanViewPersonDataResponse",
            "VisibilityRequest",
            "VisibilityResponse",
            "WhoSeesRequest",
            "WhoSeesResponse",
            "DeputySetRequest",
            "DeputyUpdateRequest",
            "DeputyEndRequest",
            "AbsenceAddRequest",
            "AbsenceUpdateRequest",
            "AbsenceDeleteRequest",
            "HandoverListRequest",
            "HandoverListResponse",
            "HandoverApplyRequest",
            "HandoverApplyResponse",
            "HandoverRetryRequest",
            "HandoverPendingRequest",
            "HandoverPendingResponse",
            "HandoverRecordsRequest",
            "HandoverRecordsResponse",
            "MemberListRequest",
            "MemberListResponse",
        ];
        assert_eq!(
            live, pinned,
            "OrgStructurePayload variants were renamed, reordered or inserted mid-enum — append only"
        );
    }

    #[test]
    fn a_batch_and_its_answer_round_trip_with_the_requests_it_wraps() {
        roundtrip(OrgStructurePayload::BatchRequest {
            ops: vec![
                OrgWriteOp {
                    temp_id: Some(s("tmp:1")),
                    request: OrgStructurePayload::UnitCreateRequest {
                        name: s("IT"),
                        code: None,
                        type_id: None,
                        parent_unit_id: None,
                        color: None,
                        valid_from: s("2026-10-01"),
                        valid_to: None,
                        confirm_backdated: false,
                    },
                },
                OrgWriteOp {
                    temp_id: None,
                    request: OrgStructurePayload::HeadSetRequest {
                        unit_id: s("tmp:1"),
                        head_position_id: None,
                        from: s("2026-10-01"),
                        confirm_backdated: false,
                    },
                },
            ],
            dry_run: true,
            confirm_backdated: true,
        });
        roundtrip(OrgStructurePayload::BatchResponse {
            ok: false,
            applied: false,
            error: None,
            results: vec![
                OrgBatchOpResult {
                    index: 0,
                    ok: true,
                    error: None,
                    result: Some(OrgWriteResult::Done),
                    temp_id: Some(s("tmp:1")),
                    created_id: Some(s("u-1")),
                },
                OrgBatchOpResult {
                    index: 1,
                    ok: false,
                    error: Some(OrgOpError {
                        code: s("unknown_temp_id"),
                        message: s("tmp:9 is not the id of an earlier operation"),
                        id: Some(s("tmp:9")),
                        ..Default::default()
                    }),
                    result: None,
                    temp_id: None,
                    created_id: None,
                },
            ],
            warnings: vec![OrgWarning::UnitWithoutHead {
                unit_id: s("u-1"),
                from: s("2026-10-01"),
            }],
            preview_at: Some(s("2026-10-01")),
            preview: Some(OrgStructureView::default()),
            max_ops: 500,
        });
    }

    #[test]
    fn a_batch_is_the_json_the_encoder_builds_and_a_client_may_leave_the_flags_out() {
        let json = serde_json::json!({
            "BatchRequest": {
                "ops": [
                    { "temp_id": "tmp:a", "request": { "UnitCreateRequest": { "name": "IT", "valid_from": "2026-10-01" } } },
                    { "request": { "UnitEndRequest": { "unit_id": "tmp:a", "from": "2026-11-01" } } }
                ]
            }
        });
        let payload: OrgStructurePayload = serde_json::from_value(json).unwrap();
        let OrgStructurePayload::BatchRequest {
            ops,
            dry_run,
            confirm_backdated,
        } = payload
        else {
            panic!("a batch")
        };
        assert!(!dry_run && !confirm_backdated);
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].temp_id.as_deref(), Some("tmp:a"));
        assert_eq!(ops[1].temp_id, None);
        assert!(matches!(
            ops[1].request,
            OrgStructurePayload::UnitEndRequest { .. }
        ));
    }

    #[test]
    fn deputies_absences_escalation_and_visibility_round_trip() {
        use crate::org_structure_cover::*;
        use OrgStructurePayload as P;
        let deputy = OrgDeputy {
            id: s("d-1"),
            user_id: s("u-1"),
            user_name: s("Anna"),
            deputy_user_id: s("u-2"),
            deputy_name: s("Marek"),
            scope: s("project:p-7"),
            valid_from: s("2026-10-05"),
            valid_to: Some(s("2026-10-12")),
        };
        let absence = OrgAbsence {
            id: s("a-1"),
            user_id: s("u-1"),
            valid_from: s("2026-10-05"),
            valid_to: None,
            kind: OrgAbsenceKind::Training,
            reason: Some(s("szkolenie")),
            source: s("manual"),
        };
        roundtrip(P::CoverRequest {
            user_id: None,
            at: Some(s("2026-10-06")),
            include_past: true,
        });
        roundtrip(P::CoverResponse {
            user_id: s("u-1"),
            display_name: s("Anna"),
            available: false,
            today: s("2026-10-06"),
            absences: vec![absence.clone()],
            covered_by: vec![deputy.clone()],
            covering: vec![],
            can_see_absences: true,
            can_see_reason: true,
            can_edit_absences: true,
            can_edit_deputies: false,
            is_admin: false,
        });
        roundtrip(P::MemberListRequest {});
        roundtrip(P::MemberListResponse {
            members: vec![OrgPersonRef {
                user_id: s("u-2"),
                display_name: s("Marek"),
            }],
        });
        roundtrip(P::AvailabilityRequest { at: None });
        roundtrip(P::AvailabilityResponse {
            at: s("2026-10-06"),
            absent_user_ids: vec![s("u-1")],
            deputies: vec![deputy.clone()],
        });
        roundtrip(P::EscalationChainRequest {
            user_id: s("u-1"),
            scope: Some(s("approvals")),
            at: None,
        });
        roundtrip(P::EscalationChainResponse {
            steps: vec![OrgEscalationStep {
                level: 2,
                user_id: s("u-3"),
                display_name: s("Ewa"),
                position_id: s("p-1"),
                position_name: s("Dyrektor"),
                via: s("deputy_head"),
                covering_user_id: Some(s("u-4")),
                covering_name: Some(s("Jan")),
            }],
            skipped: vec![OrgEscalationSkip {
                level: 1,
                position_id: s("p-0"),
                position_name: s("Kierownik"),
                reason: s("unavailable"),
            }],
            problem: Some(OrgEscalationProblem {
                kind: s("cycle"),
                position_id: Some(s("p-9")),
            }),
        });
        roundtrip(P::IsAvailableRequest {
            user_id: s("u-1"),
            at: None,
        });
        roundtrip(P::IsAvailableResponse { available: true });
        roundtrip(P::CanViewPersonDataRequest {
            viewer_user_id: None,
            subject_user_id: s("u-1"),
            kind: s("absence_reason"),
            at: Some(s("2026-10-06")),
        });
        roundtrip(P::CanViewPersonDataResponse {
            allowed: true,
            rule: s("primary_manager"),
        });
        roundtrip(P::VisibilityRequest {
            user_id: Some(s("u-1")),
            at: None,
        });
        roundtrip(P::VisibilityResponse {
            user: OrgPersonRef {
                user_id: s("u-1"),
                display_name: s("Anna"),
            },
            manager: None,
            subtree: vec![OrgPersonRef {
                user_id: s("u-2"),
                display_name: s("Marek"),
            }],
            direct: vec![],
            rows: vec![OrgVisibilityRow {
                area: s("absence_reasons"),
                verdict: s("direct"),
                rule: s("primary_manager"),
            }],
        });
        roundtrip(P::WhoSeesRequest {
            subject_user_id: None,
            at: None,
        });
        roundtrip(P::WhoSeesResponse {
            subject: OrgPersonRef {
                user_id: s("u-2"),
                display_name: s("Marek"),
            },
            viewers: vec![OrgViewer {
                user_id: s("u-1"),
                display_name: s("Anna"),
                rule: s("primary_manager"),
                kinds: vec![s("absence_reason"), s("absence_dates")],
            }],
        });
        roundtrip(P::DeputySetRequest {
            user_id: s("u-1"),
            deputy_user_id: s("u-2"),
            scope: s("all"),
            valid_from: s("2026-10-05"),
            valid_to: None,
            confirm_backdated: false,
        });
        roundtrip(P::DeputyUpdateRequest {
            id: s("d-1"),
            scope: None,
            valid_from: None,
            valid_to: None,
            clear: vec![s("valid_to")],
            confirm_backdated: true,
        });
        roundtrip(P::DeputyEndRequest {
            id: s("d-1"),
            from: s("2026-10-08"),
            confirm_backdated: false,
        });
        roundtrip(P::AbsenceAddRequest {
            user_id: None,
            valid_from: s("2026-10-05"),
            valid_to: Some(s("2026-10-08")),
            kind: OrgAbsenceKind::Leave,
            reason: None,
            confirm_backdated: false,
        });
        roundtrip(P::AbsenceUpdateRequest {
            id: s("a-1"),
            valid_from: None,
            valid_to: None,
            kind: Some(OrgAbsenceKind::Other),
            reason: None,
            clear: vec![s("reason"), s("valid_to")],
            confirm_backdated: false,
        });
        roundtrip(P::AbsenceDeleteRequest {
            id: s("a-1"),
            confirm_backdated: false,
        });
        roundtrip(P::WriteResponse {
            ok: true,
            error: None,
            warnings: vec![],
            result: Some(OrgWriteResult::Deputy(deputy)),
        });
        roundtrip(P::WriteResponse {
            ok: true,
            error: None,
            warnings: vec![],
            result: Some(OrgWriteResult::Absence(absence)),
        });
    }

    #[test]
    fn a_handover_and_its_answers_round_trip() {
        use crate::org_structure_cover::OrgPersonRef;
        use crate::org_structure_handover::*;
        use OrgStructurePayload as P;
        let item = OrgHandoverItem {
            key: s("task:p-1:t-1"),
            category: OrgHandoverCategory::Task,
            title: s("NA-231 Import OPC"),
            role: s("assignee"),
            state: s("in_progress"),
            project_id: Some(s("p-1")),
            project_name: Some(s("NextApp")),
            unit_name: None,
            valid_to: None,
            action: OrgHandoverAction::Transfer,
            suggestion: Some(OrgHandoverSuggestion {
                user_id: s("u-2"),
                reason: s("deputy"),
            }),
            eligible_user_ids: Some(vec![s("u-2"), s("u-3")]),
            blocked: None,
        };
        roundtrip(P::HandoverListRequest {
            user_id: s("u-1"),
            reason: OrgHandoverReason::Absence,
            project_id: None,
            date: Some(s("2026-10-05")),
            return_date: Some(s("2026-10-12")),
        });
        roundtrip(P::HandoverListResponse {
            user: OrgPersonRef {
                user_id: s("u-1"),
                display_name: s("Piotr"),
            },
            reason: OrgHandoverReason::Departure,
            date: s("2026-10-31"),
            return_date: None,
            assignment_ended_on: None,
            project_name: None,
            groups: vec![OrgHandoverGroup {
                category: OrgHandoverCategory::Task,
                items: vec![item],
            }],
            skipped_projects: vec![s("Alpha")],
            takers: vec![OrgPersonRef {
                user_id: s("u-2"),
                display_name: s("Anna"),
            }],
        });
        roundtrip(P::HandoverApplyRequest {
            user_id: s("u-1"),
            reason: OrgHandoverReason::ProjectRemoval,
            project_id: Some(s("p-1")),
            date: None,
            return_date: None,
            note: s("Gałąź feature/opc, MR !12"),
            items: vec![OrgHandoverChoice {
                key: s("task:p-1:t-1"),
                taker_user_id: Some(s("u-2")),
            }],
        });
        roundtrip(P::HandoverApplyResponse {
            ok: false,
            handover_id: Some(s("h-1")),
            error: None,
            items: vec![OrgHandoverItemResult {
                key: s("task:p-1:t-1"),
                category: OrgHandoverCategory::Task,
                title: s("NA-231"),
                status: s("failed"),
                reason: Some(s("taker_not_eligible")),
                taker_user_id: Some(s("u-2")),
                project_name: Some(s("NextApp")),
            }],
            applied: 0,
            scheduled: 0,
            failed: 1,
        });
        roundtrip(P::HandoverRetryRequest {
            handover_id: s("h-1"),
            keys: vec![s("task:p-1:t-1")],
        });
        roundtrip(P::HandoverPendingRequest {});
        roundtrip(P::HandoverPendingResponse {
            people: vec![OrgHandoverPending {
                user_id: s("u-1"),
                display_name: s("Piotr"),
                count: 3,
                ended_on: Some(s("2026-10-01")),
            }],
        });
        roundtrip(P::HandoverRecordsRequest { user_id: None });
        roundtrip(P::HandoverRecordsResponse {
            records: vec![OrgHandoverRecord {
                id: s("h-1"),
                user_id: s("u-1"),
                reason: OrgHandoverReason::Absence,
                project_id: None,
                project_name: None,
                date: s("2026-10-05"),
                return_date: Some(s("2026-10-12")),
                note: s("notatka"),
                created_by_name: s("Anna"),
                created_at_ms: 1_790_000_000_000,
                items: vec![],
            }],
        });
    }
}

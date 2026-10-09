//! Typed failures of the org-structure write path. Each validation variant
//! maps one-to-one to a message the administrator can act on; nothing here
//! carries personal data beyond ids.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, OrgStructureError>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum OrgStructureError {
    #[error("{entity} not found: {id}")]
    NotFound { entity: &'static str, id: String },

    /// The id exists, but in another organization. Kept apart from `NotFound`
    /// so a caller can tell a typo from an attempt to reach across tenants.
    #[error("{entity} {id} belongs to another organization")]
    CrossOrgReference { entity: &'static str, id: String },

    #[error("invalid date '{0}' (expected YYYY-MM-DD)")]
    InvalidDate(String),

    #[error("invalid interval: valid_to {to} must be after valid_from {from}")]
    InvalidInterval { from: String, to: String },

    #[error("{field} must not be empty")]
    EmptyField { field: &'static str },

    #[error("invalid value for {field}: {reason}")]
    InvalidValue { field: &'static str, reason: String },

    #[error("unknown timezone '{0}'")]
    InvalidTimezone(String),

    /// A change dated before today in the organization's timezone rewrites
    /// history, so it needs an explicit confirmation from the caller.
    #[error("change dated {date} is before today ({today}); confirm_backdated is required")]
    BackdatedConfirmationRequired { date: String, today: String },

    /// Only an administrator dates a change of an absence or a deputy before
    /// today; for everybody else the first day is today.
    #[error("change dated {date} is before today ({today}); only an administrator may date a change before today")]
    BackdatingAdminOnly { date: String, today: String },

    #[error("{entity} {id} is not valid on {date}")]
    NotValidAt {
        entity: &'static str,
        id: String,
        date: String,
    },

    #[error("interval starting {from} is not covered by {entity} {id}")]
    OutsideValidity {
        entity: &'static str,
        id: String,
        from: String,
    },

    #[error("position {position_id} already has a primary reporting line overlapping {from}")]
    PrimaryLineOverlap { position_id: String, from: String },

    #[error("position {position_id} already reports to {parent_position_id} (functional) in an overlapping interval")]
    DuplicateFunctionalLine {
        position_id: String,
        parent_position_id: String,
    },

    #[error("primary reporting lines form a cycle from {date}")]
    ReportingCycle { date: String },

    #[error("unit hierarchy forms a cycle from {date}")]
    UnitCycle { date: String },

    #[error("staff position {0} cannot be the manager of a position in the line")]
    StaffPositionCannotManage(String),

    #[error(
        "position {position_id} is already assigned to this person in an overlapping interval"
    )]
    AssignmentOverlap { position_id: String },

    #[error("person already has a primary assignment overlapping {from}")]
    PrimaryAssignmentOverlap { from: String },

    #[error("unit {unit_id} still has {child_units} child unit(s) and {positions} position(s) after {date}")]
    UnitNotEmpty {
        unit_id: String,
        child_units: usize,
        positions: usize,
        date: String,
    },

    #[error(
        "position {position_id} is still the manager of {subordinates} position(s) after {date}"
    )]
    PositionHasSubordinates {
        position_id: String,
        subordinates: usize,
        date: String,
    },

    #[error("position {0} is the head of its unit")]
    PositionIsHead(String),

    #[error("position {position_id} already heads another unit in an overlapping interval")]
    PositionHeadsAnotherUnit { position_id: String },

    #[error("position {0} cannot be both head and deputy head")]
    HeadIsDeputy(String),

    #[error("position {position_id} does not belong to unit {unit_id}")]
    PositionNotInUnit {
        position_id: String,
        unit_id: String,
    },

    #[error("unit type {0} is still used by units")]
    UnitTypeInUse(String),

    #[error("duplicate {entity} '{name}'")]
    Duplicate { entity: &'static str, name: String },

    /// The caller may not do this to this row (a person changing somebody
    /// else's absence, or one that came from another source).
    #[error("not permitted: {0}")]
    NotPermitted(&'static str),

    #[error("database error: {0}")]
    Db(String),
}

impl OrgStructureError {
    /// Stable snake_case identifier of the rule, for screens and reports to
    /// key their message on. `NotFound` covers a reference to another
    /// organization too, so the answer cannot confirm that an id exists elsewhere.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound { .. } | Self::CrossOrgReference { .. } => "not_found",
            Self::InvalidDate(_) => "invalid_date",
            Self::InvalidInterval { .. } => "invalid_interval",
            Self::EmptyField { .. } => "empty_field",
            Self::InvalidValue { .. } => "invalid_value",
            Self::InvalidTimezone(_) => "invalid_timezone",
            Self::BackdatedConfirmationRequired { .. } => "backdated_confirmation_required",
            Self::BackdatingAdminOnly { .. } => "backdating_admin_only",
            Self::NotValidAt { .. } => "not_valid_at",
            Self::OutsideValidity { .. } => "outside_validity",
            Self::PrimaryLineOverlap { .. } => "primary_line_overlap",
            Self::DuplicateFunctionalLine { .. } => "duplicate_functional_line",
            Self::ReportingCycle { .. } => "reporting_cycle",
            Self::UnitCycle { .. } => "unit_cycle",
            Self::StaffPositionCannotManage(_) => "staff_position_cannot_manage",
            Self::AssignmentOverlap { .. } => "assignment_overlap",
            Self::PrimaryAssignmentOverlap { .. } => "primary_assignment_overlap",
            Self::UnitNotEmpty { .. } => "unit_not_empty",
            Self::PositionHasSubordinates { .. } => "position_has_subordinates",
            Self::PositionIsHead(_) => "position_is_head",
            Self::PositionHeadsAnotherUnit { .. } => "position_heads_another_unit",
            Self::HeadIsDeputy(_) => "head_is_deputy",
            Self::PositionNotInUnit { .. } => "position_not_in_unit",
            Self::UnitTypeInUse(_) => "unit_type_in_use",
            Self::Duplicate { .. } => "duplicate",
            Self::NotPermitted(_) => "not_permitted",
            Self::Db(_) => "internal",
        }
    }
}

impl From<rusqlite::Error> for OrgStructureError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(e.to_string())
    }
}

impl From<anyhow::Error> for OrgStructureError {
    fn from(e: anyhow::Error) -> Self {
        Self::Db(e.to_string())
    }
}

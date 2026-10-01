//! Typed rows and inputs of the org-structure repository.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitType {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
    pub icon: Option<String>,
}

/// One VERSION of a unit. `unit_id` is the identity positions and child units
/// point at; `id` names this row of its history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unit {
    pub id: String,
    pub unit_id: String,
    pub name: String,
    pub code: Option<String>,
    pub type_id: Option<String>,
    pub parent_unit_id: Option<String>,
    pub color: Option<String>,
    pub head_position_id: Option<String>,
    pub valid_from: NaiveDate,
    pub valid_to: Option<NaiveDate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    /// One VERSION of the position; `position_id` is the identity that lines,
    /// assignments and units point at.
    pub id: String,
    pub position_id: String,
    pub unit_id: String,
    pub name: String,
    /// Stable key for the file import and export; unique in the organization.
    pub code: Option<String>,
    pub role_id: Option<String>,
    /// `None` = follow the role's `is_manager`.
    pub is_manager: Option<bool>,
    pub is_staff: bool,
    pub valid_from: NaiveDate,
    pub valid_to: Option<NaiveDate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeputyHead {
    pub id: String,
    pub unit_id: String,
    pub position_id: String,
    pub ord: i64,
    pub valid_from: NaiveDate,
    pub valid_to: Option<NaiveDate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    Primary,
    Functional,
}

impl LineKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Functional => "functional",
        }
    }

    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw {
            "primary" => Some(Self::Primary),
            "functional" => Some(Self::Functional),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportingLine {
    pub id: String,
    pub position_id: String,
    pub parent_position_id: String,
    pub kind: LineKind,
    pub priority: i64,
    pub valid_from: NaiveDate,
    pub valid_to: Option<NaiveDate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentType {
    Permanent,
    Acting,
    Contractor,
}

impl AssignmentType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Permanent => "permanent",
            Self::Acting => "acting",
            Self::Contractor => "contractor",
        }
    }

    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw {
            "permanent" => Some(Self::Permanent),
            "acting" => Some(Self::Acting),
            "contractor" => Some(Self::Contractor),
            _ => None,
        }
    }
}

/// Who holds a position: an account of the platform or a person without one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Subject {
    User(String),
    External(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assignment {
    pub id: String,
    pub position_id: String,
    pub subject: Subject,
    #[serde(rename = "type")]
    pub kind: AssignmentType,
    pub share: f64,
    pub is_primary: bool,
    pub valid_from: NaiveDate,
    pub valid_to: Option<NaiveDate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalPerson {
    pub id: String,
    pub display_name: String,
    pub email: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub org_id: String,
    pub timezone: String,
}

pub const DEFAULT_TIMEZONE: &str = "Europe/Warsaw";

/// Something the administrator should look at that does not block the write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Warning {
    /// The person's shares add up to more than a full time from `from`.
    ShareOverbooked {
        subject: Subject,
        from: NaiveDate,
        total: f64,
    },
    /// The unit has no head from `from`.
    UnitWithoutHead { unit_id: String, from: NaiveDate },
    /// The person holds several positions from `from` and none is primary.
    PersonWithoutPrimary { subject: Subject, from: NaiveDate },
}

/// A broken rule found by `integrity_report`. The write path rejects every one
/// of these; they can still appear when two nodes edit offline and merge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Violation {
    /// Two versions of one unit or position are valid on the same day.
    VersionsOverlap {
        entity: String,
        id: String,
        from: NaiveDate,
    },
    /// A position has two primary reporting lines on the same day.
    PrimaryLinesOverlap {
        position_id: String,
        from: NaiveDate,
    },
    /// A person has two primary assignments on the same day.
    PrimaryAssignmentsOverlap {
        subject: Subject,
        from: NaiveDate,
    },
    /// One position heads two units on the same day.
    PositionHeadsMultipleUnits {
        position_id: String,
        from: NaiveDate,
    },
    /// A position is head and deputy head of the same unit on the same day.
    DeputyIsHead {
        unit_id: String,
        position_id: String,
        from: NaiveDate,
    },
    PositionCycle {
        position_id: String,
        from: NaiveDate,
    },
    UnitCycle {
        unit_id: String,
        from: NaiveDate,
    },
    /// A row names something that does not exist (in the window looked at).
    DanglingReference {
        entity: String,
        id: String,
        field: String,
        missing_id: String,
    },
}

/// What ending a position took with it, so the UI can say so.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ended {
    pub reporting_lines: Vec<String>,
    pub deputy_heads: Vec<String>,
    pub assignments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Written<T> {
    pub value: T,
    pub warnings: Vec<Warning>,
}

/// Who writes and whether they accepted rewriting history.
#[derive(Debug, Clone, Copy)]
pub struct WriteCtx<'a> {
    pub org_id: &'a str,
    pub actor_user_id: &'a str,
    pub confirm_backdated: bool,
}

#[derive(Debug, Clone)]
pub struct NewUnit {
    pub name: String,
    pub code: Option<String>,
    pub type_id: Option<String>,
    pub parent_unit_id: Option<String>,
    pub color: Option<String>,
    pub valid_from: NaiveDate,
    pub valid_to: Option<NaiveDate>,
}

/// `Some(None)` clears a value, `None` leaves it. Applied from a date, like
/// every other change, so earlier days keep the old name.
#[derive(Debug, Clone, Default)]
pub struct UnitPatch {
    pub name: Option<String>,
    pub code: Option<Option<String>>,
    pub type_id: Option<Option<String>>,
    pub color: Option<Option<String>>,
}

#[derive(Debug, Clone, Default)]
pub struct UnitTypePatch {
    pub name: Option<String>,
    pub color: Option<Option<String>>,
    pub icon: Option<Option<String>>,
}

#[derive(Debug, Clone)]
pub struct NewPosition {
    pub unit_id: String,
    pub name: String,
    pub code: Option<String>,
    pub role_id: Option<String>,
    pub is_manager: Option<bool>,
    pub is_staff: bool,
    /// Creates the primary reporting line together with the position.
    pub parent_position_id: Option<String>,
    pub valid_from: NaiveDate,
    pub valid_to: Option<NaiveDate>,
}

/// Applied from a date; earlier days keep the old values.
#[derive(Debug, Clone, Default)]
pub struct PositionPatch {
    pub name: Option<String>,
    pub code: Option<Option<String>>,
    pub role_id: Option<Option<String>>,
    pub is_manager: Option<Option<bool>>,
    pub is_staff: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct NewLine {
    pub position_id: String,
    pub parent_position_id: String,
    pub kind: LineKind,
    pub priority: i64,
    pub valid_from: NaiveDate,
    pub valid_to: Option<NaiveDate>,
}

#[derive(Debug, Clone)]
pub struct NewAssignment {
    pub position_id: String,
    pub subject: Subject,
    pub kind: AssignmentType,
    pub share: f64,
    /// `None` = primary when the person holds nothing else in the interval.
    pub is_primary: Option<bool>,
    pub valid_from: NaiveDate,
    pub valid_to: Option<NaiveDate>,
}

#[derive(Debug, Clone, Default)]
pub struct AssignmentPatch {
    pub kind: Option<AssignmentType>,
    pub share: Option<f64>,
    pub is_primary: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct NewExternalPerson {
    pub display_name: String,
    pub email: Option<String>,
    pub note: Option<String>,
}

// =============================================================================
// File: org_structure_handover.rs
// Purpose: Wire types of the handover screen "Do przekazania" of the
//          organizational structure protocol (docs/ORG_STRUCTURE_PLAN.md §2.6,
//          docs/PROJECT_STUDIO_WORKFLOW_PLAN.md §4.5). The requests and
//          responses themselves are variants of `OrgStructurePayload`.
//
//          The server decides what a person holds and who may take it: the
//          client sends only the keys it wants moved and to whom. Codes
//          (`role`, `state`, suggestion `reason`, item `status`/`reason`) are
//          stable identifiers the screen translates.
//
//          Append-only, like the payload: a field added later MUST carry
//          `#[serde(default)]`.
// =============================================================================

use serde::{Deserialize, Serialize};

/// Why the work changes hands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OrgHandoverReason {
    /// The person leaves: the handover is permanent.
    #[default]
    Departure,
    /// The person is away: the work comes back after `return_date`, unless the
    /// taker closed or changed it.
    Absence,
    /// The person leaves one project: only that project's items move.
    ProjectRemoval,
}

/// Where an item lives; also the order of the groups on the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OrgHandoverCategory {
    #[default]
    Task,
    TestItem,
    Membership,
    Position,
    Deputy,
}

/// What choosing the item does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OrgHandoverAction {
    /// The item goes to the chosen person, who is required.
    #[default]
    Transfer,
    /// With a person the item goes to them; without one it ends (a position
    /// becomes a vacancy, a deputy stops covering).
    TransferOrEnd,
    /// The item ends; nobody takes it (a membership).
    End,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgHandoverSuggestion {
    pub user_id: String,
    /// `deputy`, `manager`, `project_manager`, `project_owner`, `deputy_head`
    /// or `next_deputy`.
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgHandoverItem {
    /// Stable across calls: `task:<project>:<task>`, `test:<project>:<item>`,
    /// `member:<project>`, `position:<assignment>`, `deputy:<row>`.
    pub key: String,
    pub category: OrgHandoverCategory,
    pub title: String,
    /// What the person is in it: `assignee`, a project role, `head`,
    /// `deputy_head`, `member`, `deputy` or `covered`.
    pub role: String,
    /// Its state: a task or test-item status, or empty.
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub project_name: Option<String>,
    /// The unit of a position.
    #[serde(default)]
    pub unit_name: Option<String>,
    /// The end of a deputy interval, when it has one.
    #[serde(default)]
    pub valid_to: Option<String>,
    pub action: OrgHandoverAction,
    #[serde(default)]
    pub suggestion: Option<OrgHandoverSuggestion>,
    /// The only people who may take it; `None` = any active member.
    #[serde(default)]
    pub eligible_user_ids: Option<Vec<String>>,
    /// Why it cannot be handed over now (`project_archived`).
    #[serde(default)]
    pub blocked: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgHandoverGroup {
    pub category: OrgHandoverCategory,
    pub items: Vec<OrgHandoverItem>,
}

/// The caller's choice for one item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgHandoverChoice {
    pub key: String,
    #[serde(default)]
    pub taker_user_id: Option<String>,
}

/// What became of one item. `status`: `done`, `scheduled`, `failed`, `skipped`,
/// `not_started`, and for a temporary handover `returned` or `kept`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgHandoverItemResult {
    pub key: String,
    pub category: OrgHandoverCategory,
    pub title: String,
    pub status: String,
    /// The rule behind `failed`, `skipped`, `not_started` or `kept`.
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub taker_user_id: Option<String>,
    #[serde(default)]
    pub project_name: Option<String>,
}

/// A handover that was made, as the screens list it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgHandoverRecord {
    pub id: String,
    pub user_id: String,
    pub reason: OrgHandoverReason,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub project_name: Option<String>,
    pub date: String,
    #[serde(default)]
    pub return_date: Option<String>,
    pub note: String,
    #[serde(default)]
    pub created_by_name: String,
    pub created_at_ms: i64,
    pub items: Vec<OrgHandoverItemResult>,
}

/// A person who still holds something after their assignment ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgHandoverPending {
    pub user_id: String,
    #[serde(default)]
    pub display_name: String,
    pub count: u32,
    /// The day the last assignment ended.
    #[serde(default)]
    pub ended_on: Option<String>,
}

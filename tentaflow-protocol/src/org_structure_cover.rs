// =============================================================================
// File: org_structure_cover.rs
// Purpose: Wire types of the deputies, absences, escalation chain and
//          visibility part of the organizational structure protocol
//          (docs/ORG_STRUCTURE_PLAN.md §1, §2.1a, §6.3). The requests and
//          responses themselves are variants of `OrgStructurePayload`.
//
//          Privacy is decided by the server: absences are listed only to those
//          who may see their dates. Everybody else gets `available: false` and
//          nothing more. An absence has no reason: `OrgAbsence::reason` is
//          deprecated, kept only because wire fields are append-only, and is
//          never sent.
//
//          Append-only, like the payload: a field added later MUST carry
//          `#[serde(default)]`.
// =============================================================================

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OrgAbsenceKind {
    Leave,
    Training,
    #[default]
    Other,
}

/// A person away. `valid_to` is EXCLUSIVE, like every end date of the
/// structure: `2026-10-05` .. `2026-10-08` is away on the 5th, 6th and 7th.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgAbsence {
    pub id: String,
    pub user_id: String,
    pub valid_from: String,
    #[serde(default)]
    pub valid_to: Option<String>,
    pub kind: OrgAbsenceKind,
    /// Deprecated and always absent: an absence has no reason.
    #[serde(default)]
    pub reason: Option<String>,
    /// `manual` or the name of the integration that brought it.
    pub source: String,
}

/// A person covered by another. `scope` is `all`, `approvals`, `escalations`
/// or `project:<id>`; `valid_to` is exclusive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgDeputy {
    pub id: String,
    pub user_id: String,
    #[serde(default)]
    pub user_name: String,
    pub deputy_user_id: String,
    #[serde(default)]
    pub deputy_name: String,
    pub scope: String,
    pub valid_from: String,
    #[serde(default)]
    pub valid_to: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgPersonRef {
    pub user_id: String,
    #[serde(default)]
    pub display_name: String,
}

/// One person to ask. `via` is `holder`, `deputy_head` or `deputy`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgEscalationStep {
    pub level: u32,
    pub user_id: String,
    #[serde(default)]
    pub display_name: String,
    pub position_id: String,
    #[serde(default)]
    pub position_name: String,
    pub via: String,
    /// Whose place the person takes, for `deputy_head` and `deputy`.
    #[serde(default)]
    pub covering_user_id: Option<String>,
    #[serde(default)]
    pub covering_name: Option<String>,
}

/// A level nobody answered. `reason` is `vacant` or `unavailable`; never why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgEscalationSkip {
    pub level: u32,
    pub position_id: String,
    #[serde(default)]
    pub position_name: String,
    pub reason: String,
}

/// A defect of the structure the walk met: `cycle` (with the position) or `too_deep`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgEscalationProblem {
    pub kind: String,
    #[serde(default)]
    pub position_id: Option<String>,
}

/// One area of data and what the person sees of it. `area`: `structure`,
/// `utilization`, `absence_dates`, `position_history`,
/// `everyone_else`. `verdict`: `all`, `subtree`, `direct`, `own`, `none`.
/// `rule`: `owner`, `primary_manager`, `supervisor`, `administrator`,
/// `every_member`, `none`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgVisibilityRow {
    pub area: String,
    pub verdict: String,
    pub rule: String,
}

/// One person who may see data of another, with the strongest rule that
/// allows it and the kinds they may see (`absence_dates`,
/// `time_utilization`, `position_history`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgViewer {
    pub user_id: String,
    #[serde(default)]
    pub display_name: String,
    pub rule: String,
    #[serde(default)]
    pub kinds: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_without_the_optional_fields_still_reads() {
        let mut buf = Vec::new();
        ciborium::into_writer(
            &serde_json::json!({ "id": "a", "user_id": "u", "valid_from": "2026-10-05", "kind": "leave", "source": "manual" }),
            &mut buf,
        )
        .unwrap();
        let absence: OrgAbsence = ciborium::from_reader(buf.as_slice()).unwrap();
        assert_eq!(absence.valid_to, None);
        assert_eq!(absence.reason, None);
    }
}

// =============================================================================
// File: org_history.rs
// Purpose: Types of the organizational structure's history and planned
//          reorganizations (docs/ORG_STRUCTURE_PLAN.md §2.4, §3.4, §6.3). The
//          requests and responses themselves are variants of
//          `OrgStructurePayload` (org_structure.rs); the shapes they carry are
//          here so that file stays readable.
//
//          History is read by every member of the organization, but the
//          position history of a person is an administrator's and the
//          person's own: `personal_visible` in an answer says whether the
//          caller got it, so the screen can say what is left out instead of
//          showing a gap. A planned reorganization is an administrator's
//          document from its first draft to its approval.
//
//          Append-only, like the payload: a field added later MUST carry
//          `#[serde(default)]`; a rename breaks every deployed peer.
// =============================================================================

use serde::{Deserialize, Serialize};

use crate::org_structure::OrgSubject;
use crate::org_structure::OrgWriteOp;

/// One field of an entity that changed. Values are the stored ones (an id for a
/// reference); the `*_label` is what the screen shows in its place (a unit or
/// position name) and is absent for plain values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgFieldChange {
    pub field: String,
    #[serde(default)]
    pub before: Option<String>,
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub before_label: Option<String>,
    #[serde(default)]
    pub after_label: Option<String>,
}

/// One operation of a batch or an import, as the audit trail lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgHistoryOp {
    pub action: String,
    /// `unit`, `position`, `assignment`, `unit_type`, `external_person` ...
    pub target_kind: String,
    pub target_id: String,
    #[serde(default)]
    pub target_name: Option<String>,
}

/// One entry of the audit trail of the structure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgHistoryEntry {
    /// The id of the audit row.
    pub id: i64,
    /// When it was written (UTC, `YYYY-MM-DD HH:MM:SS`).
    pub at: String,
    #[serde(default)]
    pub actor_user_id: Option<String>,
    #[serde(default)]
    pub actor_name: Option<String>,
    /// The audit action, e.g. `org.unit.move`.
    pub action: String,
    /// `unit`, `position`, `assignment`, `unit_type`, `external_person`,
    /// `structure` (a batch or an import), `change_set`, `settings`.
    pub target_kind: String,
    pub target_id: String,
    #[serde(default)]
    pub target_name: Option<String>,
    /// The unit the change belongs to, when it belongs to one.
    #[serde(default)]
    pub unit_id: Option<String>,
    #[serde(default)]
    pub unit_name: Option<String>,
    /// The day the change takes (took) effect; the day it was written when the
    /// change has no date of its own.
    #[serde(default)]
    pub effective_date: Option<String>,
    #[serde(default)]
    pub changes: Vec<OrgFieldChange>,
    /// The operations of a batch or an import, in order.
    #[serde(default)]
    pub ops: Vec<OrgHistoryOp>,
    /// Operations of a batch left out because they are personal data the
    /// caller may not see.
    #[serde(default)]
    pub hidden_ops: u32,
    /// Who holds (held) the position, for an assignment entry.
    #[serde(default)]
    pub subject: Option<OrgSubject>,
    #[serde(default)]
    pub subject_name: Option<String>,
    /// The position an assignment entry is about.
    #[serde(default)]
    pub position_id: Option<String>,
    #[serde(default)]
    pub position_name: Option<String>,
    /// `batch` or `import` when the entry summarises many operations.
    #[serde(default)]
    pub source: Option<String>,
}

/// One difference between two states of the structure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgDiffItem {
    /// `added`, `removed` or `changed`.
    pub change: String,
    /// `unit`, `position` or `assignment`.
    pub entity: String,
    /// `unit_id`, `position_id` or the assignment's position id.
    pub id: String,
    /// The unit's or the position's name (the position's for an assignment).
    pub name: String,
    /// The unit the entity belongs to (its own id for a unit).
    #[serde(default)]
    pub unit_id: Option<String>,
    /// The field of a `changed` item.
    #[serde(default)]
    pub field: Option<String>,
    #[serde(default)]
    pub before: Option<String>,
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub before_label: Option<String>,
    #[serde(default)]
    pub after_label: Option<String>,
    /// The person of an assignment item.
    #[serde(default)]
    pub subject: Option<OrgSubject>,
    #[serde(default)]
    pub subject_name: Option<String>,
}

/// A planned reorganization: a named set of dated operations that waits for one
/// approval by an administrator other than its author, then takes effect on its
/// day (the operations are dated writes, so nothing switches over at night).
///
/// States: `draft` (being written), `pending` (waits for the approval),
/// `approved` (approved, not applied — transient), `applied` (its operations are
/// in the structure, dated), `withdrawn`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OrgChangeSet {
    pub id: String,
    pub name: String,
    pub effective_date: String,
    pub state: String,
    /// The administrator who last changed the content: the one who may not approve it.
    pub author_user_id: String,
    #[serde(default)]
    pub author_name: Option<String>,
    #[serde(default)]
    pub approver_user_id: Option<String>,
    #[serde(default)]
    pub approver_name: Option<String>,
    pub created_at_ms: i64,
    pub op_count: u32,
    /// The operations; only in the answers that ask for one change set.
    #[serde(default)]
    pub ops: Vec<OrgWriteOp>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_body::MessageBody;
    use crate::org_structure::{OrgOpError, OrgStructurePayload as P, OrgStructureView};

    fn roundtrip(payload: P) {
        let body = MessageBody::OrgStructureBody(payload);
        let mut buf = Vec::new();
        ciborium::into_writer(&body, &mut buf).unwrap();
        let decoded: MessageBody = ciborium::from_reader(buf.as_slice()).unwrap();
        assert_eq!(decoded, body);
    }

    fn s(v: &str) -> String {
        v.to_string()
    }

    fn change_set() -> OrgChangeSet {
        OrgChangeSet {
            id: s("cs-1"),
            name: s("Reorganizacja Q4"),
            effective_date: s("2026-11-01"),
            state: s("pending"),
            author_user_id: s("u-1"),
            author_name: Some(s("Hanna")),
            approver_user_id: None,
            approver_name: None,
            created_at_ms: 1_790_000_000_000,
            op_count: 1,
            ops: vec![OrgWriteOp {
                temp_id: Some(s("tmp:a")),
                request: P::UnitMoveRequest {
                    unit_id: s("u-2"),
                    new_parent_unit_id: None,
                    from: s("2026-11-01"),
                    confirm_backdated: false,
                },
            }],
        }
    }

    fn entry() -> OrgHistoryEntry {
        OrgHistoryEntry {
            id: 7,
            at: s("2026-09-29 10:00:00"),
            actor_user_id: Some(s("u-1")),
            actor_name: Some(s("Hanna")),
            action: s("org.unit.move"),
            target_kind: s("unit"),
            target_id: s("u-2"),
            target_name: Some(s("DevOps")),
            unit_id: Some(s("u-2")),
            unit_name: Some(s("DevOps")),
            effective_date: Some(s("2026-07-01")),
            changes: vec![OrgFieldChange {
                field: s("parent_unit_id"),
                before: Some(s("u-0")),
                after: Some(s("u-9")),
                before_label: Some(s("IT")),
                after_label: Some(s("Realizacja")),
            }],
            ops: vec![OrgHistoryOp {
                action: s("org.unit.move"),
                target_kind: s("unit"),
                target_id: s("u-2"),
                target_name: None,
            }],
            hidden_ops: 2,
            subject: Some(OrgSubject::User(s("u-5"))),
            subject_name: Some(s("Ewa")),
            position_id: Some(s("p-1")),
            position_name: Some(s("Tester")),
            source: Some(s("batch")),
        }
    }

    fn item() -> OrgDiffItem {
        OrgDiffItem {
            change: s("changed"),
            entity: s("position"),
            id: s("p-1"),
            name: s("Tester"),
            unit_id: Some(s("u-2")),
            field: Some(s("primary_parent_position_id")),
            before: Some(s("p-0")),
            after: Some(s("p-9")),
            before_label: Some(s("Anna")),
            after_label: Some(s("Karolina")),
            subject: Some(OrgSubject::External(s("x-1"))),
            subject_name: Some(s("Kontraktor")),
        }
    }

    #[test]
    fn history_and_diff_round_trip() {
        roundtrip(P::HistoryListRequest {
            from: Some(s("2026-01-01")),
            to: None,
            unit_id: Some(s("u-2")),
            offset: 10,
            limit: 50,
        });
        roundtrip(P::HistoryListResponse {
            entries: vec![entry()],
            total: 1,
            personal_visible: false,
            today: s("2026-09-30"),
        });
        roundtrip(P::HistoryDiffRequest {
            from: s("2026-09-30"),
            to: s("2026-11-01"),
            unit_id: None,
        });
        roundtrip(P::HistoryDiffResponse {
            from: s("2026-09-30"),
            to: s("2026-11-01"),
            items: vec![item()],
            personal_visible: true,
        });
    }

    #[test]
    fn change_set_requests_and_answers_round_trip() {
        let ops = change_set().ops;
        roundtrip(P::ChangeSetListRequest {});
        roundtrip(P::ChangeSetListResponse {
            items: vec![OrgChangeSet {
                ops: Vec::new(),
                ..change_set()
            }],
            today: s("2026-09-30"),
            sole_admin: true,
        });
        roundtrip(P::ChangeSetGetRequest { id: s("cs-1") });
        roundtrip(P::ChangeSetSaveRequest {
            id: None,
            name: s("Q4"),
            effective_date: s("2026-11-01"),
            ops,
        });
        roundtrip(P::ChangeSetSubmitRequest { id: s("cs-1") });
        roundtrip(P::ChangeSetApproveRequest { id: s("cs-1") });
        roundtrip(P::ChangeSetWithdrawRequest { id: s("cs-1") });
        roundtrip(P::ChangeSetPreviewRequest {
            id: s("cs-1"),
            unit_id: Some(s("u-2")),
        });
        roundtrip(P::ChangeSetResponse {
            ok: false,
            error: Some(OrgOpError {
                code: s("self_approval"),
                message: s("the author cannot approve"),
                ..Default::default()
            }),
            change_set: Some(change_set()),
            valid: true,
            results: Vec::new(),
            warnings: Vec::new(),
        });
        roundtrip(P::ChangeSetPreviewResponse {
            ok: true,
            error: None,
            change_set: Some(change_set()),
            valid: true,
            results: Vec::new(),
            warnings: Vec::new(),
            at: s("2026-11-01"),
            live: Some(OrgStructureView::default()),
            preview: Some(OrgStructureView::default()),
            items: vec![item()],
        });
    }

    #[test]
    fn an_answer_without_the_optional_fields_still_decodes() {
        // A peer that predates a field sends the answer without it.
        let json = serde_json::json!({ "ChangeSetResponse": { "ok": true } });
        let payload: P = serde_json::from_value(json).unwrap();
        assert!(matches!(
            payload,
            P::ChangeSetResponse {
                ok: true,
                valid: false,
                ..
            }
        ));
    }
}

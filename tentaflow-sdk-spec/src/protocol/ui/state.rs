// =============================================================================
// File: protocol/ui/state.rs — state-channel wire messages (§6.4)
// Purpose: StateSnapshot (0x0120), StatePatch (0x0121), StateReset (0x0122),
// PatchRejected (0x0123).
// =============================================================================

use minicbor::{Decode, Encode};

use super::patch::PatchOp;
use super::slot::StateEntry;

/// `StateSnapshot` (0x0120). Addon→Frontend.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct StateSnapshot {
    #[n(0)]
    pub addon_id: String,
    #[n(1)]
    pub panel_id: String,
    #[n(2)]
    pub panel_epoch: u64,
    #[n(3)]
    pub state_revision: u64,
    /// Caller MUST sort entries by canonical encoded StatePath bytes (§6.4).
    #[n(4)]
    pub entries: Vec<StateEntry>,
    /// True for every chunk except the last when snapshot is split across messages.
    #[n(5)]
    pub truncated: bool,
}

/// `StatePatch` (0x0121). Addon→Frontend.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct StatePatch {
    #[n(0)]
    pub addon_id: String,
    #[n(1)]
    pub panel_id: String,
    #[n(2)]
    pub panel_epoch: u64,
    #[n(3)]
    pub base_revision: u64,
    #[n(4)]
    pub new_revision: u64,
    #[n(5)]
    pub ops: Vec<PatchOp>,
}

/// `StateReset` (0x0122). Addon→Frontend.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct StateReset {
    #[n(0)]
    pub addon_id: String,
    #[n(1)]
    pub panel_id: String,
    #[n(2)]
    pub panel_epoch: u64,
    #[n(3)]
    pub new_revision: u64,
}

string_enum! {
    /// Reason frontend rejects a StatePatch (§6.4).
    pub enum PatchRejectReason {
        RevisionMismatch = "revision_mismatch",
        PathOwnershipViolation = "path_ownership_violation",
        PathOutOfNamespace = "path_out_of_namespace",
        TypeMismatch = "type_mismatch",
        ArrayBounds = "array_bounds",
        DepthExceeded = "depth_exceeded",
        StructuralLimit = "structural_limit",
    }
}

/// `PatchRejected` (0x0123). Frontend→Core.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct PatchRejected {
    #[n(0)]
    pub addon_id: String,
    #[n(1)]
    pub panel_id: String,
    #[n(2)]
    pub panel_epoch: u64,
    #[n(3)]
    pub rejected_msg_id: u64,
    #[n(4)]
    pub reason: PatchRejectReason,
    /// Present when `reason == RevisionMismatch` so the addon knows the
    /// authoritative client revision.
    #[n(5)]
    pub current_revision: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ui::bind::{PathSegment, StatePath};
    use crate::protocol::ui::patch::PatchOpKind;
    use crate::protocol::value::Value;

    fn rt<T>(v: T)
    where
        T: minicbor::Encode<()> + for<'b> minicbor::Decode<'b, ()> + PartialEq + core::fmt::Debug,
    {
        let mut b1 = Vec::new();
        minicbor::encode(&v, &mut b1).unwrap();
        let d: T = minicbor::decode(&b1).unwrap();
        assert_eq!(d, v);
        let mut b2 = Vec::new();
        minicbor::encode(&d, &mut b2).unwrap();
        assert_eq!(b1, b2);
    }

    fn p(seg: &str) -> StatePath {
        StatePath::new(vec![PathSegment::Key(seg.into())])
    }

    #[test]
    fn state_snapshot_roundtrip() {
        rt(StateSnapshot {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            state_revision: 42,
            entries: vec![StateEntry {
                path: p("k"),
                value: Value::U64(7),
            }],
            truncated: false,
        });
    }

    #[test]
    fn state_patch_roundtrip() {
        rt(StatePatch {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            base_revision: 5,
            new_revision: 6,
            ops: vec![PatchOp {
                path: p("count"),
                op: PatchOpKind::Increment { delta: 1 },
            }],
        });
    }

    #[test]
    fn state_reset_roundtrip() {
        rt(StateReset {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            new_revision: 0,
        });
    }

    #[test]
    fn patch_rejected_roundtrip() {
        rt(PatchRejected {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            rejected_msg_id: 100,
            reason: PatchRejectReason::RevisionMismatch,
            current_revision: Some(7),
        });
        rt(PatchRejected {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            rejected_msg_id: 200,
            reason: PatchRejectReason::DepthExceeded,
            current_revision: None,
        });
    }

    #[test]
    fn patch_reject_reason_unknown_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.str("not_a_reason").unwrap();
        let res: Result<PatchRejectReason, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }
}

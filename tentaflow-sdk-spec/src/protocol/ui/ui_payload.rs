// =============================================================================
// File: protocol/ui/ui_payload.rs — UI channel payload union + Batch (§6.1, §6.8)
// Purpose: discriminated union over all 19 UI-channel wire messages keyed by
// the §6.1 tag table (6 panel + 4 slot + 4 state + Action/ActionAck +
// Command + Event + Batch), plus Batch which carries a Vec<BatchMember> that
// recursively holds inner UiPayload variants but rejects nested Batch.
// =============================================================================

use minicbor::{Decode, Decoder, Encode, Encoder};

use super::action::{Action, ActionAck};
use super::command::Command;
use super::event::Event;
use super::panel::{PanelClose, PanelError, PanelOpen, PanelReady, PanelReset, PanelShell};
use super::slot_msg::{SlotClear, SlotContent, SlotHide, SlotShow};
use super::state::{PatchRejected, StatePatch, StateReset, StateSnapshot};

/// Maximum number of members per Batch (§6.8).
pub const BATCH_MAX_MEMBERS: usize = 64;

/// Wire tags for UI-channel payloads (§6.1 table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum UiTag {
    PanelOpen = 0x0101,
    PanelShell = 0x0102,
    PanelReady = 0x0103,
    PanelError = 0x0104,
    PanelClose = 0x0105,
    PanelReset = 0x0106,
    SlotContent = 0x0110,
    SlotClear = 0x0111,
    SlotShow = 0x0112,
    SlotHide = 0x0113,
    StateSnapshot = 0x0120,
    StatePatch = 0x0121,
    StateReset = 0x0122,
    PatchRejected = 0x0123,
    Action = 0x0130,
    ActionAck = 0x0131,
    Command = 0x0140,
    Event = 0x0150,
    Batch = 0x0160,
}

impl UiTag {
    pub const fn as_u16(self) -> u16 {
        self as u16
    }

    pub const fn from_u16(v: u16) -> Option<Self> {
        Some(match v {
            0x0101 => Self::PanelOpen,
            0x0102 => Self::PanelShell,
            0x0103 => Self::PanelReady,
            0x0104 => Self::PanelError,
            0x0105 => Self::PanelClose,
            0x0106 => Self::PanelReset,
            0x0110 => Self::SlotContent,
            0x0111 => Self::SlotClear,
            0x0112 => Self::SlotShow,
            0x0113 => Self::SlotHide,
            0x0120 => Self::StateSnapshot,
            0x0121 => Self::StatePatch,
            0x0122 => Self::StateReset,
            0x0123 => Self::PatchRejected,
            0x0130 => Self::Action,
            0x0131 => Self::ActionAck,
            0x0140 => Self::Command,
            0x0150 => Self::Event,
            0x0160 => Self::Batch,
            _ => return None,
        })
    }
}

/// Discriminated union over all §6 UI-channel payloads.
///
/// Wire form: CBOR array `[tag: u16, body]` (Envelope.payload contract).
#[derive(Debug, Clone, PartialEq)]
pub enum UiPayload {
    PanelOpen(PanelOpen),
    PanelShell(PanelShell),
    PanelReady(PanelReady),
    PanelError(PanelError),
    PanelClose(PanelClose),
    PanelReset(PanelReset),
    SlotContent(SlotContent),
    SlotClear(SlotClear),
    SlotShow(SlotShow),
    SlotHide(SlotHide),
    StateSnapshot(StateSnapshot),
    StatePatch(StatePatch),
    StateReset(StateReset),
    PatchRejected(PatchRejected),
    Action(Action),
    ActionAck(ActionAck),
    Command(Command),
    Event(Event),
    Batch(Batch),
}

impl UiPayload {
    pub fn tag(&self) -> UiTag {
        match self {
            Self::PanelOpen(_) => UiTag::PanelOpen,
            Self::PanelShell(_) => UiTag::PanelShell,
            Self::PanelReady(_) => UiTag::PanelReady,
            Self::PanelError(_) => UiTag::PanelError,
            Self::PanelClose(_) => UiTag::PanelClose,
            Self::PanelReset(_) => UiTag::PanelReset,
            Self::SlotContent(_) => UiTag::SlotContent,
            Self::SlotClear(_) => UiTag::SlotClear,
            Self::SlotShow(_) => UiTag::SlotShow,
            Self::SlotHide(_) => UiTag::SlotHide,
            Self::StateSnapshot(_) => UiTag::StateSnapshot,
            Self::StatePatch(_) => UiTag::StatePatch,
            Self::StateReset(_) => UiTag::StateReset,
            Self::PatchRejected(_) => UiTag::PatchRejected,
            Self::Action(_) => UiTag::Action,
            Self::ActionAck(_) => UiTag::ActionAck,
            Self::Command(_) => UiTag::Command,
            Self::Event(_) => UiTag::Event,
            Self::Batch(_) => UiTag::Batch,
        }
    }
}

impl<C> Encode<C> for UiPayload {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.array(2)?;
        e.u16(self.tag().as_u16())?;
        match self {
            Self::PanelOpen(v) => v.encode(e, ctx)?,
            Self::PanelShell(v) => v.encode(e, ctx)?,
            Self::PanelReady(v) => v.encode(e, ctx)?,
            Self::PanelError(v) => v.encode(e, ctx)?,
            Self::PanelClose(v) => v.encode(e, ctx)?,
            Self::PanelReset(v) => v.encode(e, ctx)?,
            Self::SlotContent(v) => v.encode(e, ctx)?,
            Self::SlotClear(v) => v.encode(e, ctx)?,
            Self::SlotShow(v) => v.encode(e, ctx)?,
            Self::SlotHide(v) => v.encode(e, ctx)?,
            Self::StateSnapshot(v) => v.encode(e, ctx)?,
            Self::StatePatch(v) => v.encode(e, ctx)?,
            Self::StateReset(v) => v.encode(e, ctx)?,
            Self::PatchRejected(v) => v.encode(e, ctx)?,
            Self::Action(v) => v.encode(e, ctx)?,
            Self::ActionAck(v) => v.encode(e, ctx)?,
            Self::Command(v) => v.encode(e, ctx)?,
            Self::Event(v) => v.encode(e, ctx)?,
            Self::Batch(v) => v.encode(e, ctx)?,
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for UiPayload {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let n = d
            .array()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length array forbidden"))?;
        if n != 2 {
            return Err(minicbor::decode::Error::message(
                "Envelope payload tuple MUST be [tag, body]",
            ));
        }
        let tag_raw = d.u16()?;
        let tag = UiTag::from_u16(tag_raw)
            .ok_or_else(|| minicbor::decode::Error::message("unknown UI-channel tag"))?;
        Ok(match tag {
            UiTag::PanelOpen => Self::PanelOpen(PanelOpen::decode(d, ctx)?),
            UiTag::PanelShell => Self::PanelShell(PanelShell::decode(d, ctx)?),
            UiTag::PanelReady => Self::PanelReady(PanelReady::decode(d, ctx)?),
            UiTag::PanelError => Self::PanelError(PanelError::decode(d, ctx)?),
            UiTag::PanelClose => Self::PanelClose(PanelClose::decode(d, ctx)?),
            UiTag::PanelReset => Self::PanelReset(PanelReset::decode(d, ctx)?),
            UiTag::SlotContent => Self::SlotContent(SlotContent::decode(d, ctx)?),
            UiTag::SlotClear => Self::SlotClear(SlotClear::decode(d, ctx)?),
            UiTag::SlotShow => Self::SlotShow(SlotShow::decode(d, ctx)?),
            UiTag::SlotHide => Self::SlotHide(SlotHide::decode(d, ctx)?),
            UiTag::StateSnapshot => Self::StateSnapshot(StateSnapshot::decode(d, ctx)?),
            UiTag::StatePatch => Self::StatePatch(StatePatch::decode(d, ctx)?),
            UiTag::StateReset => Self::StateReset(StateReset::decode(d, ctx)?),
            UiTag::PatchRejected => Self::PatchRejected(PatchRejected::decode(d, ctx)?),
            UiTag::Action => Self::Action(Action::decode(d, ctx)?),
            UiTag::ActionAck => Self::ActionAck(ActionAck::decode(d, ctx)?),
            UiTag::Command => Self::Command(Command::decode(d, ctx)?),
            UiTag::Event => Self::Event(Event::decode(d, ctx)?),
            UiTag::Batch => Self::Batch(Batch::decode(d, ctx)?),
        })
    }
}

/// `Batch` (0x0160). Bidirectional. Members share the outer envelope's
/// metadata (msg_id, correlation_id, ts_ms, session_id, flags, …) — no
/// per-member overrides. `members` MUST NOT contain another Batch.
#[derive(Debug, Clone, PartialEq)]
pub struct Batch {
    pub atomic: bool,
    pub members: Vec<BatchMember>,
}

/// Member of a Batch (§6.8). `tag` is the §6.1 wire tag, `body` is the
/// matching typed UiPayload variant — nesting Batch inside Batch is rejected.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchMember {
    pub tag: UiTag,
    pub body: UiPayload,
}

impl<C> Encode<C> for Batch {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        if self.members.len() > BATCH_MAX_MEMBERS {
            return Err(minicbor::encode::Error::message(
                "Batch.members exceeds BATCH_MAX_MEMBERS (64)",
            ));
        }
        for m in &self.members {
            if m.tag == UiTag::Batch || matches!(m.body, UiPayload::Batch(_)) {
                return Err(minicbor::encode::Error::message(
                    "Batch may not contain another Batch as a member (§6.8)",
                ));
            }
            if m.tag != m.body.tag() {
                return Err(minicbor::encode::Error::message(
                    "BatchMember.tag does not match BatchMember.body.tag()",
                ));
            }
        }
        // Canonical: atomic(0x66..) > kind(0x64..)? No: 0x66 > 0x64. So
        // "atomic" sorts after "members". Recompute:
        //   "atomic"  = 0x66 61 ..
        //   "members" = 0x67 6d ..
        // → atomic(0x66) < members(0x67). Emit atomic first.
        e.map(2)?;
        e.str("atomic")?.bool(self.atomic)?;
        e.str("members")?;
        e.array(self.members.len() as u64)?;
        for m in &self.members {
            // Each member encoded inline as [tag, body] — same shape as UiPayload.
            e.array(2)?;
            e.u16(m.tag.as_u16())?;
            // Encode just the body (NOT wrapped in [tag, body] again).
            encode_payload_body(&m.body, e, ctx)?;
        }
        Ok(())
    }
}

fn encode_payload_body<W: minicbor::encode::Write, C>(
    body: &UiPayload,
    e: &mut Encoder<W>,
    ctx: &mut C,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    match body {
        UiPayload::PanelOpen(v) => v.encode(e, ctx),
        UiPayload::PanelShell(v) => v.encode(e, ctx),
        UiPayload::PanelReady(v) => v.encode(e, ctx),
        UiPayload::PanelError(v) => v.encode(e, ctx),
        UiPayload::PanelClose(v) => v.encode(e, ctx),
        UiPayload::PanelReset(v) => v.encode(e, ctx),
        UiPayload::SlotContent(v) => v.encode(e, ctx),
        UiPayload::SlotClear(v) => v.encode(e, ctx),
        UiPayload::SlotShow(v) => v.encode(e, ctx),
        UiPayload::SlotHide(v) => v.encode(e, ctx),
        UiPayload::StateSnapshot(v) => v.encode(e, ctx),
        UiPayload::StatePatch(v) => v.encode(e, ctx),
        UiPayload::StateReset(v) => v.encode(e, ctx),
        UiPayload::PatchRejected(v) => v.encode(e, ctx),
        UiPayload::Action(v) => v.encode(e, ctx),
        UiPayload::ActionAck(v) => v.encode(e, ctx),
        UiPayload::Command(v) => v.encode(e, ctx),
        UiPayload::Event(v) => v.encode(e, ctx),
        UiPayload::Batch(_) => Err(minicbor::encode::Error::message(
            "Batch may not contain another Batch as a member (§6.8)",
        )),
    }
}

impl<'b, C> Decode<'b, C> for Batch {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut atomic: Option<bool> = None;
        let mut members: Option<Vec<BatchMember>> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "atomic" => atomic = Some(d.bool()?),
                "members" => {
                    let n = d.array()?.ok_or_else(|| {
                        minicbor::decode::Error::message("indefinite-length array forbidden")
                    })?;
                    if (n as usize) > BATCH_MAX_MEMBERS {
                        return Err(minicbor::decode::Error::message(
                            "Batch.members exceeds BATCH_MAX_MEMBERS (64)",
                        ));
                    }
                    let mut v = Vec::with_capacity(n as usize);
                    for _ in 0..n {
                        let n2 = d.array()?.ok_or_else(|| {
                            minicbor::decode::Error::message("indefinite-length array forbidden")
                        })?;
                        if n2 != 2 {
                            return Err(minicbor::decode::Error::message(
                                "BatchMember MUST be [tag, body]",
                            ));
                        }
                        let tag_raw = d.u16()?;
                        let tag = UiTag::from_u16(tag_raw).ok_or_else(|| {
                            minicbor::decode::Error::message(
                                "BatchMember.tag is not a known UI tag",
                            )
                        })?;
                        if tag == UiTag::Batch {
                            return Err(minicbor::decode::Error::message(
                                "Batch may not contain another Batch as a member (§6.8)",
                            ));
                        }
                        let body = decode_payload_body(tag, d, ctx)?;
                        v.push(BatchMember { tag, body });
                    }
                    members = Some(v);
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown Batch key: {other}"
                    )))
                }
            }
        }
        Ok(Batch {
            atomic: atomic
                .ok_or_else(|| minicbor::decode::Error::message("Batch missing atomic"))?,
            members: members
                .ok_or_else(|| minicbor::decode::Error::message("Batch missing members"))?,
        })
    }
}

fn decode_payload_body<'b, C>(
    tag: UiTag,
    d: &mut Decoder<'b>,
    ctx: &mut C,
) -> Result<UiPayload, minicbor::decode::Error> {
    Ok(match tag {
        UiTag::PanelOpen => UiPayload::PanelOpen(PanelOpen::decode(d, ctx)?),
        UiTag::PanelShell => UiPayload::PanelShell(PanelShell::decode(d, ctx)?),
        UiTag::PanelReady => UiPayload::PanelReady(PanelReady::decode(d, ctx)?),
        UiTag::PanelError => UiPayload::PanelError(PanelError::decode(d, ctx)?),
        UiTag::PanelClose => UiPayload::PanelClose(PanelClose::decode(d, ctx)?),
        UiTag::PanelReset => UiPayload::PanelReset(PanelReset::decode(d, ctx)?),
        UiTag::SlotContent => UiPayload::SlotContent(SlotContent::decode(d, ctx)?),
        UiTag::SlotClear => UiPayload::SlotClear(SlotClear::decode(d, ctx)?),
        UiTag::SlotShow => UiPayload::SlotShow(SlotShow::decode(d, ctx)?),
        UiTag::SlotHide => UiPayload::SlotHide(SlotHide::decode(d, ctx)?),
        UiTag::StateSnapshot => UiPayload::StateSnapshot(StateSnapshot::decode(d, ctx)?),
        UiTag::StatePatch => UiPayload::StatePatch(StatePatch::decode(d, ctx)?),
        UiTag::StateReset => UiPayload::StateReset(StateReset::decode(d, ctx)?),
        UiTag::PatchRejected => UiPayload::PatchRejected(PatchRejected::decode(d, ctx)?),
        UiTag::Action => UiPayload::Action(Action::decode(d, ctx)?),
        UiTag::ActionAck => UiPayload::ActionAck(ActionAck::decode(d, ctx)?),
        UiTag::Command => UiPayload::Command(Command::decode(d, ctx)?),
        UiTag::Event => UiPayload::Event(Event::decode(d, ctx)?),
        UiTag::Batch => {
            return Err(minicbor::decode::Error::message(
                "Batch may not contain another Batch as a member (§6.8)",
            ))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::control::CborMap;
    use crate::protocol::envelope::{Channel, Envelope, Flags, Priority, ProtocolVersion};
    use crate::protocol::ids::SessionId;
    use crate::protocol::ui::action::{Action, ActionAck, ActionStatus};
    use crate::protocol::ui::panel::{PanelReady, PanelReset};

    fn envelope_with(payload: UiPayload) -> Envelope<UiPayload> {
        Envelope {
            protocol_version: ProtocolVersion::V1,
            channel: Channel::Ui,
            msg_id: 1,
            correlation_id: None,
            ts_ms: 1_700_000_000_000,
            session_id: SessionId::from_bytes([0; 16]),
            trace_id: None,
            deadline_ms: None,
            priority: Priority::Normal,
            flags: Flags::RELIABLE,
            payload,
        }
    }

    fn rt(env: &Envelope<UiPayload>) {
        let mut b1 = Vec::new();
        minicbor::encode(env, &mut b1).unwrap();
        let d: Envelope<UiPayload> = minicbor::decode(&b1).unwrap();
        assert_eq!(&d, env);
        let mut b2 = Vec::new();
        minicbor::encode(&d, &mut b2).unwrap();
        assert_eq!(b1, b2, "re-encode bit-identical");
    }

    #[test]
    fn envelope_with_panel_ready_roundtrip() {
        rt(&envelope_with(UiPayload::PanelReady(PanelReady {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            first_paint_ms: 7,
        })));
    }

    #[test]
    fn envelope_with_action_ack_roundtrip() {
        rt(&envelope_with(UiPayload::ActionAck(ActionAck {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            action_id: "save".into(),
            client_action_id: crate::protocol::ids::ClientActionId::from_bytes([1; 16]),
            status: ActionStatus::Ok,
        })));
    }

    #[test]
    fn batch_with_two_members_roundtrip() {
        let batch = Batch {
            atomic: true,
            members: vec![
                BatchMember {
                    tag: UiTag::PanelReady,
                    body: UiPayload::PanelReady(PanelReady {
                        addon_id: "a".into(),
                        panel_id: "p".into(),
                        panel_epoch: 1,
                        first_paint_ms: 5,
                    }),
                },
                BatchMember {
                    tag: UiTag::PanelReset,
                    body: UiPayload::PanelReset(PanelReset {
                        addon_id: "a".into(),
                        panel_id: "p".into(),
                        new_panel_epoch: 2,
                        reason: "x".into(),
                    }),
                },
            ],
        };
        rt(&envelope_with(UiPayload::Batch(batch)));
    }

    #[test]
    fn batch_nested_rejected_on_encode() {
        let inner = UiPayload::Batch(Batch {
            atomic: false,
            members: vec![],
        });
        let outer = Batch {
            atomic: false,
            members: vec![BatchMember {
                tag: UiTag::Batch,
                body: inner,
            }],
        };
        let mut buf = Vec::new();
        let res = minicbor::encode(&outer, &mut buf);
        assert!(res.is_err());
    }

    #[test]
    fn batch_member_tag_mismatch_rejected() {
        let bad = Batch {
            atomic: false,
            members: vec![BatchMember {
                tag: UiTag::PanelReady,
                body: UiPayload::Command(crate::protocol::ui::command::Command::Focus {
                    component_id: "x".into(),
                }),
            }],
        };
        let mut buf = Vec::new();
        let res = minicbor::encode(&bad, &mut buf);
        assert!(res.is_err());
    }

    #[test]
    fn unknown_ui_tag_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).unwrap().u16(0x01FE).unwrap().map(0).unwrap();
        let res: Result<UiPayload, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn nested_batch_rejected_on_decode() {
        // Hand-craft `{atomic: false, members: [[0x0160, {atomic:false,members:[]}]]}`
        // — a Batch whose only member is itself a Batch. Decode MUST reject.
        let mut body = Vec::new();
        let mut e = minicbor::Encoder::new(&mut body);
        e.map(2).unwrap();
        e.str("atomic").unwrap().bool(false).unwrap();
        e.str("members").unwrap();
        e.array(1).unwrap();
        e.array(2).unwrap();
        e.u16(0x0160).unwrap();
        e.map(2).unwrap();
        e.str("atomic").unwrap().bool(false).unwrap();
        e.str("members").unwrap().array(0).unwrap();
        let res: Result<Batch, _> = minicbor::decode(&body);
        assert!(res.is_err(), "decode must reject nested Batch");
    }

    fn rt_envelope_variant(payload: UiPayload) {
        rt(&envelope_with(payload));
    }

    #[test]
    fn envelope_all_19_ui_variants_roundtrip() {
        use crate::protocol::ui::action::{ActionAck, ActionStatus};
        use crate::protocol::ui::bind::{PathSegment, StatePath};
        use crate::protocol::ui::command::Command as Cmd;
        use crate::protocol::ui::component::{Component as Comp, FieldMap};
        use crate::protocol::ui::error_code::ErrorCode;
        use crate::protocol::ui::event::{Event as Ev, Topic, TopicSegment};
        use crate::protocol::ui::panel::{
            CloseReason, PanelClose, PanelError, PanelOpen, PanelOpenContext, PanelShell, Viewport,
        };
        use crate::protocol::ui::patch::{PatchOp, PatchOpKind};
        use crate::protocol::ui::slot::StateEntry;
        use crate::protocol::ui::slot_msg::{SlotClear, SlotContent, SlotHide, SlotShow};
        use crate::protocol::ui::state::{
            PatchRejectReason, PatchRejected, StatePatch, StateReset, StateSnapshot,
        };
        use crate::protocol::value::Value;

        fn empty_comp() -> Comp {
            Comp {
                tag: 0x0001,
                id: "r".into(),
                fields: FieldMap::default(),
                handlers: None,
                bind: None,
                a11y: None,
                visibility: None,
                test_id: None,
            }
        }
        fn p() -> StatePath {
            StatePath::new(vec![PathSegment::Key("k".into())])
        }
        let cid = crate::protocol::ids::ClientActionId::from_bytes([3; 16]);

        rt_envelope_variant(UiPayload::PanelOpen(PanelOpen {
            addon_id: "a".into(),
            panel_id: "p".into(),
            ctx: PanelOpenContext {
                user_id: "u".into(),
                locale: "pl-PL".into(),
                theme: "dark".into(),
                viewport: Viewport {
                    width_px: 100,
                    height_px: 100,
                    density: 1.0,
                },
                deep_link: None,
                prefers_reduced_motion: false,
                prefers_high_contrast: false,
                assigned_epoch: 1,
            },
        }));
        rt_envelope_variant(UiPayload::PanelShell(PanelShell {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            layout: empty_comp(),
            slots: vec![],
            initial_state: vec![],
            initial_commands: vec![],
        }));
        rt_envelope_variant(UiPayload::PanelReady(PanelReady {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            first_paint_ms: 5,
        }));
        rt_envelope_variant(UiPayload::PanelError(PanelError {
            addon_id: "a".into(),
            panel_id: "p".into(),
            code: ErrorCode::AddonTimeout,
            message: "x".into(),
        }));
        rt_envelope_variant(UiPayload::PanelClose(PanelClose {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            reason: CloseReason::UserNavigated,
        }));
        rt_envelope_variant(UiPayload::PanelReset(PanelReset {
            addon_id: "a".into(),
            panel_id: "p".into(),
            new_panel_epoch: 2,
            reason: "r".into(),
        }));
        rt_envelope_variant(UiPayload::SlotContent(SlotContent {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            slot_id: "main".into(),
            fragment: empty_comp(),
            state_overlay: None,
        }));
        rt_envelope_variant(UiPayload::SlotClear(SlotClear {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            slot_id: "m".into(),
        }));
        rt_envelope_variant(UiPayload::SlotShow(SlotShow {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            slot_id: "m".into(),
        }));
        rt_envelope_variant(UiPayload::SlotHide(SlotHide {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            slot_id: "m".into(),
        }));
        rt_envelope_variant(UiPayload::StateSnapshot(StateSnapshot {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            state_revision: 0,
            entries: vec![StateEntry {
                path: p(),
                value: Value::Null,
            }],
            truncated: false,
        }));
        rt_envelope_variant(UiPayload::StatePatch(StatePatch {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            base_revision: 0,
            new_revision: 1,
            ops: vec![PatchOp {
                path: p(),
                op: PatchOpKind::Delete,
            }],
        }));
        rt_envelope_variant(UiPayload::StateReset(StateReset {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            new_revision: 0,
        }));
        rt_envelope_variant(UiPayload::PatchRejected(PatchRejected {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            rejected_msg_id: 1,
            reason: PatchRejectReason::TypeMismatch,
            current_revision: None,
        }));
        rt_envelope_variant(UiPayload::Action(Action {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            action_id: "x".into(),
            params: CborMap::default(),
            form_values: None,
            user_gesture: true,
            client_action_id: cid,
        }));
        rt_envelope_variant(UiPayload::ActionAck(ActionAck {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            action_id: "x".into(),
            client_action_id: cid,
            status: ActionStatus::Ok,
        }));
        rt_envelope_variant(UiPayload::Command(Cmd::Focus {
            component_id: "x".into(),
        }));
        rt_envelope_variant(UiPayload::Event(Ev {
            source_addon_id: "a".into(),
            topic: Topic::new(vec![TopicSegment::Literal { value: "x".into() }]),
            payload: Value::U64(1),
            ts_ms: 0,
        }));
        rt_envelope_variant(UiPayload::Batch(Batch {
            atomic: false,
            members: vec![],
        }));
    }

    #[test]
    fn envelope_with_action_roundtrip() {
        rt(&envelope_with(UiPayload::Action(Action {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            action_id: "click".into(),
            params: CborMap::default(),
            form_values: None,
            user_gesture: true,
            client_action_id: crate::protocol::ids::ClientActionId::from_bytes([2; 16]),
        })));
    }
}

// =============================================================================
// File: protocol/ui/handler.rs — Handler + LocalAction + FailurePolicy (§10.3)
// Purpose: typed recursive event-handler tree the addon attaches to Component
// instances. Local actions run client-side; Backend actions emit `Action` to
// the addon; Both emit Action AND apply an optimistic patch. Recursion limits
// (depth ≤8, total steps ≤16, Sequence ≤8 items, no nested Sequence, no
// cycles) are enforced by `Handler::validate()`.
// =============================================================================

use minicbor::{Decode, Decoder, Encode, Encoder};

use crate::protocol::control::CborMap;

use super::bind::StatePath;
use super::patch::PatchOp;
use super::tokens::ScrollBehavior;
use super::validation::{StateCondition, ValidationRule};
use crate::protocol::ui::typed_field::assert_no_dup_tstr;

/// Total recursion depth limit (Confirm/Conditional/Debounce nesting). §10.3.
pub const HANDLER_MAX_RECURSION_DEPTH: usize = 8;
/// Maximum total step count across the whole handler tree. §10.3.
pub const HANDLER_MAX_TOTAL_STEPS: usize = 16;
/// Maximum items in a Sequence (no nesting allowed). §10.3.
pub const SEQUENCE_MAX_ITEMS: usize = 8;
/// Maximum Debounce delay milliseconds. §10.3.
pub const DEBOUNCE_MAX_MS: u32 = 5000;

/// What happens after a Backend/Both action fails on the wire (§10.3).
#[derive(Debug, Clone, PartialEq)]
pub enum FailurePolicy {
    Toast,
    RevertOptimistic,
    Custom { action: LocalAction },
}

impl<C> Encode<C> for FailurePolicy {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Canonical: "action"(0x66..) > "kind"(0x64..). Emit kind first.
        match self {
            FailurePolicy::Toast => {
                e.map(1)?;
                e.str("kind")?.str("toast")?;
            }
            FailurePolicy::RevertOptimistic => {
                e.map(1)?;
                e.str("kind")?.str("revert_optimistic")?;
            }
            FailurePolicy::Custom { action } => {
                e.map(2)?;
                e.str("kind")?.str("custom")?;
                e.str("action")?;
                action.encode(e, ctx)?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for FailurePolicy {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut action: Option<LocalAction> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    assert_no_dup_tstr(&kind, "FailurePolicy", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "action" => {
                    assert_no_dup_tstr(&action, "FailurePolicy", "action")?;
                    action = Some(LocalAction::decode(d, ctx)?);
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown FailurePolicy key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("FailurePolicy missing kind"))?;
        match kind.as_str() {
            "toast" => {
                if action.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "FailurePolicy.toast must not carry action",
                    ));
                }
                Ok(FailurePolicy::Toast)
            }
            "revert_optimistic" => {
                if action.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "FailurePolicy.revert_optimistic must not carry action",
                    ));
                }
                Ok(FailurePolicy::RevertOptimistic)
            }
            "custom" => Ok(FailurePolicy::Custom {
                action: action.ok_or_else(|| {
                    minicbor::decode::Error::message("FailurePolicy.custom missing action")
                })?,
            }),
            other => Err(minicbor::decode::Error::message(format!(
                "unknown FailurePolicy.kind: {other}"
            ))),
        }
    }
}

/// LocalAction tagged union (§10.3). Capability-gated by the addon manifest.
#[derive(Debug, Clone, PartialEq)]
pub enum LocalAction {
    ShowModal {
        slot_id: String,
    },
    HideModal {
        slot_id: String,
    },
    ToggleSlot {
        slot_id: String,
    },
    SetState {
        path: StatePath,
        value: crate::protocol::value::Value,
    },
    DeleteState {
        path: StatePath,
    },
    Toggle {
        path: StatePath,
    },
    Increment {
        path: StatePath,
        delta: i64,
    },
    Navigate {
        panel_id: String,
    },
    Focus {
        component_id: String,
    },
    Scroll {
        component_id: String,
        behavior: ScrollBehavior,
    },
    Copy {
        value: String,
    },
    Confirm {
        title: String,
        message: String,
        destructive: bool,
        then: Box<Handler>,
    },
    Validate {
        field_component_id: String,
        rules: Vec<ValidationRule>,
        on_invalid: Box<LocalAction>,
    },
    Debounce {
        ms: u32,
        then: Box<Handler>,
    },
    Sequence {
        steps: Vec<Handler>,
    },
    Conditional {
        when: StateCondition,
        then: Box<Handler>,
        else_branch: Option<Box<Handler>>,
    },
    Noop,
}

impl<C> Encode<C> for LocalAction {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        match self {
            LocalAction::ShowModal { slot_id } => {
                emit_single_tstr(e, "show_modal", "slot_id", slot_id)?;
            }
            LocalAction::HideModal { slot_id } => {
                emit_single_tstr(e, "hide_modal", "slot_id", slot_id)?;
            }
            LocalAction::ToggleSlot { slot_id } => {
                emit_single_tstr(e, "toggle_slot", "slot_id", slot_id)?;
            }
            LocalAction::SetState { path, value } => {
                // Canonical: kind(0x64..) < path(0x64..) < value(0x65..).
                e.map(3)?;
                e.str("kind")?.str("set_state")?;
                e.str("path")?;
                path.encode(e, ctx)?;
                e.str("value")?;
                value.encode(e, ctx)?;
            }
            LocalAction::DeleteState { path } => {
                e.map(2)?;
                e.str("kind")?.str("delete_state")?;
                e.str("path")?;
                path.encode(e, ctx)?;
            }
            LocalAction::Toggle { path } => {
                e.map(2)?;
                e.str("kind")?.str("toggle")?;
                e.str("path")?;
                path.encode(e, ctx)?;
            }
            LocalAction::Increment { path, delta } => {
                // Encoded keys with full byte prefix:
                //   "kind"=0x64 6b.., "path"=0x64 70.., "delta"=0x65 64..
                // Canonical sort: kind < path < delta.
                e.map(3)?;
                e.str("kind")?.str("increment")?;
                e.str("path")?;
                path.encode(e, ctx)?;
                e.str("delta")?.i64(*delta)?;
            }
            LocalAction::Navigate { panel_id } => {
                emit_single_tstr(e, "navigate", "panel_id", panel_id)?;
            }
            LocalAction::Focus { component_id } => {
                emit_single_tstr(e, "focus", "component_id", component_id)?;
            }
            LocalAction::Scroll {
                component_id,
                behavior,
            } => {
                // Canonical: behavior(0x68..) > kind(0x64..) > component_id(0x6c..) — recheck:
                //   "behavior"=0x68 ..; "kind"=0x64 ..; "component_id"=0x6c ..
                // Order: kind(0x64) < behavior(0x68) < component_id(0x6c).
                e.map(3)?;
                e.str("kind")?.str("scroll")?;
                e.str("behavior")?;
                behavior.encode(e, ctx)?;
                e.str("component_id")?.str(component_id)?;
            }
            LocalAction::Copy { value } => {
                e.map(2)?;
                e.str("kind")?.str("copy")?;
                e.str("value")?.str(value)?;
            }
            LocalAction::Confirm {
                title,
                message,
                destructive,
                then,
            } => {
                // Keys: destructive(0x6b..), kind(0x64..), message(0x67..), then(0x64..), title(0x65..)
                // Sort by full bytes:
                //   "kind" 0x64 6b..
                //   "then" 0x64 74..        ('t' > 'k' so kind < then)
                //   "title" 0x65 74..
                //   "message" 0x67 6d..
                //   "destructive" 0x6b 64..
                e.map(5)?;
                e.str("kind")?.str("confirm")?;
                e.str("then")?;
                then.encode(e, ctx)?;
                e.str("title")?.str(title)?;
                e.str("message")?.str(message)?;
                e.str("destructive")?.bool(*destructive)?;
            }
            LocalAction::Validate {
                field_component_id,
                rules,
                on_invalid,
            } => {
                // Keys: field_component_id(0x72..), kind(0x64..), on_invalid(0x6a..), rules(0x65..)
                //   "kind" 0x64..
                //   "rules" 0x65..
                //   "on_invalid" 0x6a..
                //   "field_component_id" 0x72..
                e.map(4)?;
                e.str("kind")?.str("validate")?;
                e.str("rules")?;
                e.array(rules.len() as u64)?;
                for r in rules {
                    r.encode(e, ctx)?;
                }
                e.str("on_invalid")?;
                on_invalid.encode(e, ctx)?;
                e.str("field_component_id")?.str(field_component_id)?;
            }
            LocalAction::Debounce { ms, then } => {
                // Keys: kind(0x64..), ms(0x62..), then(0x64..)
                //   "ms" 0x62 6d 73          (header 0x62 — shortest)
                //   "kind" 0x64..
                //   "then" 0x64..
                // → ms, kind, then.
                e.map(3)?;
                e.str("ms")?.u32(*ms)?;
                e.str("kind")?.str("debounce")?;
                e.str("then")?;
                then.encode(e, ctx)?;
            }
            LocalAction::Sequence { steps } => {
                // Keys: kind(0x64..), steps(0x65..)
                e.map(2)?;
                e.str("kind")?.str("sequence")?;
                e.str("steps")?;
                e.array(steps.len() as u64)?;
                for s in steps {
                    s.encode(e, ctx)?;
                }
            }
            LocalAction::Conditional {
                when,
                then,
                else_branch,
            } => {
                // Keys: else(0x64..), kind(0x64..), then(0x64..), when(0x64..)
                //   "else" 0x64 65 6c 73 65   ('e' < 'k', 't', 'w')
                //   "kind" 0x64 6b ..
                //   "then" 0x64 74 ..
                //   "when" 0x64 77 ..
                // → else, kind, then, when.
                let n = if else_branch.is_some() { 4 } else { 3 };
                e.map(n)?;
                if let Some(eb) = else_branch {
                    e.str("else")?;
                    eb.encode(e, ctx)?;
                }
                e.str("kind")?.str("conditional")?;
                e.str("then")?;
                then.encode(e, ctx)?;
                e.str("when")?;
                when.encode(e, ctx)?;
            }
            LocalAction::Noop => {
                e.map(1)?;
                e.str("kind")?.str("noop")?;
            }
        }
        Ok(())
    }
}

fn emit_single_tstr<W: minicbor::encode::Write>(
    e: &mut Encoder<W>,
    kind: &str,
    key: &str,
    value: &str,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    // 'key' name was chosen so that it sorts AFTER 'kind' canonically — we
    // assert that statically by emitting kind first.
    e.map(2)?;
    e.str("kind")?.str(kind)?;
    e.str(key)?.str(value)?;
    Ok(())
}

impl<'b, C> Decode<'b, C> for LocalAction {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut slot_id: Option<String> = None;
        let mut path: Option<StatePath> = None;
        let mut value: Option<crate::protocol::value::Value> = None;
        let mut delta: Option<i64> = None;
        let mut panel_id: Option<String> = None;
        let mut component_id: Option<String> = None;
        let mut behavior: Option<ScrollBehavior> = None;
        let mut copy_value: Option<String> = None;
        let mut title: Option<String> = None;
        let mut message: Option<String> = None;
        let mut destructive: Option<bool> = None;
        let mut then: Option<Box<Handler>> = None;
        let mut field_component_id: Option<String> = None;
        let mut rules: Option<Vec<ValidationRule>> = None;
        let mut on_invalid: Option<Box<LocalAction>> = None;
        let mut ms: Option<u32> = None;
        let mut steps: Option<Vec<Handler>> = None;
        let mut when_cond: Option<StateCondition> = None;
        let mut else_branch: Option<Box<Handler>> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    assert_no_dup_tstr(&kind, "LocalAction", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "slot_id" => {
                    assert_no_dup_tstr(&slot_id, "LocalAction", "slot_id")?;
                    slot_id = Some(d.str()?.to_string());
                }
                "path" => {
                    assert_no_dup_tstr(&path, "LocalAction", "path")?;
                    path = Some(StatePath::decode(d, ctx)?);
                }
                "value" => {
                    if value.is_some() || copy_value.is_some() {
                        return Err(minicbor::decode::Error::message(
                            "LocalAction: duplicate key 'value'",
                        ));
                    }
                    match kind.as_deref() {
                        Some("copy") => copy_value = Some(d.str()?.to_string()),
                        _ => {
                            value = Some(crate::protocol::value::Value::decode(d, ctx)?);
                        }
                    }
                }
                "delta" => {
                    assert_no_dup_tstr(&delta, "LocalAction", "delta")?;
                    delta = Some(d.i64()?);
                }
                "panel_id" => {
                    assert_no_dup_tstr(&panel_id, "LocalAction", "panel_id")?;
                    panel_id = Some(d.str()?.to_string());
                }
                "component_id" => {
                    assert_no_dup_tstr(&component_id, "LocalAction", "component_id")?;
                    component_id = Some(d.str()?.to_string());
                }
                "behavior" => {
                    assert_no_dup_tstr(&behavior, "LocalAction", "behavior")?;
                    behavior = Some(ScrollBehavior::decode(d, ctx)?);
                }
                "title" => {
                    assert_no_dup_tstr(&title, "LocalAction", "title")?;
                    title = Some(d.str()?.to_string());
                }
                "message" => {
                    assert_no_dup_tstr(&message, "LocalAction", "message")?;
                    message = Some(d.str()?.to_string());
                }
                "destructive" => {
                    assert_no_dup_tstr(&destructive, "LocalAction", "destructive")?;
                    destructive = Some(d.bool()?);
                }
                "then" => {
                    assert_no_dup_tstr(&then, "LocalAction", "then")?;
                    then = Some(Box::new(Handler::decode(d, ctx)?));
                }
                "field_component_id" => {
                    assert_no_dup_tstr(&field_component_id, "LocalAction", "field_component_id")?;
                    field_component_id = Some(d.str()?.to_string());
                }
                "rules" => {
                    assert_no_dup_tstr(&rules, "LocalAction", "rules")?;
                    let n = d.array()?.ok_or_else(|| {
                        minicbor::decode::Error::message("indefinite-length array forbidden")
                    })?;
                    let mut v = Vec::with_capacity(n as usize);
                    for _ in 0..n {
                        v.push(ValidationRule::decode(d, ctx)?);
                    }
                    rules = Some(v);
                }
                "on_invalid" => {
                    assert_no_dup_tstr(&on_invalid, "LocalAction", "on_invalid")?;
                    on_invalid = Some(Box::new(LocalAction::decode(d, ctx)?));
                }
                "ms" => {
                    assert_no_dup_tstr(&ms, "LocalAction", "ms")?;
                    ms = Some(d.u32()?);
                }
                "steps" => {
                    assert_no_dup_tstr(&steps, "LocalAction", "steps")?;
                    let n = d.array()?.ok_or_else(|| {
                        minicbor::decode::Error::message("indefinite-length array forbidden")
                    })?;
                    let mut v = Vec::with_capacity(n as usize);
                    for _ in 0..n {
                        v.push(Handler::decode(d, ctx)?);
                    }
                    steps = Some(v);
                }
                "when" => {
                    assert_no_dup_tstr(&when_cond, "LocalAction", "when")?;
                    when_cond = Some(StateCondition::decode(d, ctx)?);
                }
                "else" => {
                    assert_no_dup_tstr(&else_branch, "LocalAction", "else")?;
                    else_branch = Some(Box::new(Handler::decode(d, ctx)?));
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown LocalAction key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("LocalAction missing kind"))?;
        // Helper to assert only specified groups of fields are present.
        // We define a compact closure with bool flags per slot.
        struct Allow {
            slot_id: bool,
            path: bool,
            value: bool,
            delta: bool,
            panel_id: bool,
            component_id: bool,
            behavior: bool,
            copy_value: bool,
            title: bool,
            message: bool,
            destructive: bool,
            then: bool,
            field_component_id: bool,
            rules: bool,
            on_invalid: bool,
            ms: bool,
            steps: bool,
            when_cond: bool,
            else_branch: bool,
        }
        let check_extras = |a: Allow| -> Result<(), minicbor::decode::Error> {
            if !a.slot_id && slot_id.is_some()
                || !a.path && path.is_some()
                || !a.value && value.is_some()
                || !a.delta && delta.is_some()
                || !a.panel_id && panel_id.is_some()
                || !a.component_id && component_id.is_some()
                || !a.behavior && behavior.is_some()
                || !a.copy_value && copy_value.is_some()
                || !a.title && title.is_some()
                || !a.message && message.is_some()
                || !a.destructive && destructive.is_some()
                || !a.then && then.is_some()
                || !a.field_component_id && field_component_id.is_some()
                || !a.rules && rules.is_some()
                || !a.on_invalid && on_invalid.is_some()
                || !a.ms && ms.is_some()
                || !a.steps && steps.is_some()
                || !a.when_cond && when_cond.is_some()
                || !a.else_branch && else_branch.is_some()
            {
                return Err(minicbor::decode::Error::message(
                    "LocalAction variant carries fields not allowed by its kind",
                ));
            }
            Ok(())
        };
        let none = Allow {
            slot_id: false,
            path: false,
            value: false,
            delta: false,
            panel_id: false,
            component_id: false,
            behavior: false,
            copy_value: false,
            title: false,
            message: false,
            destructive: false,
            then: false,
            field_component_id: false,
            rules: false,
            on_invalid: false,
            ms: false,
            steps: false,
            when_cond: false,
            else_branch: false,
        };
        match kind.as_str() {
            "show_modal" => {
                check_extras(Allow {
                    slot_id: true,
                    ..none
                })?;
                Ok(LocalAction::ShowModal {
                    slot_id: slot_id.ok_or_else(|| {
                        minicbor::decode::Error::message("show_modal missing slot_id")
                    })?,
                })
            }
            "hide_modal" => {
                check_extras(Allow {
                    slot_id: true,
                    ..none
                })?;
                Ok(LocalAction::HideModal {
                    slot_id: slot_id.ok_or_else(|| {
                        minicbor::decode::Error::message("hide_modal missing slot_id")
                    })?,
                })
            }
            "toggle_slot" => {
                check_extras(Allow {
                    slot_id: true,
                    ..none
                })?;
                Ok(LocalAction::ToggleSlot {
                    slot_id: slot_id.ok_or_else(|| {
                        minicbor::decode::Error::message("toggle_slot missing slot_id")
                    })?,
                })
            }
            "set_state" => {
                check_extras(Allow {
                    path: true,
                    value: true,
                    ..none
                })?;
                Ok(LocalAction::SetState {
                    path: path.ok_or_else(|| {
                        minicbor::decode::Error::message("set_state missing path")
                    })?,
                    value: value.ok_or_else(|| {
                        minicbor::decode::Error::message("set_state missing value")
                    })?,
                })
            }
            "delete_state" => {
                check_extras(Allow { path: true, ..none })?;
                Ok(LocalAction::DeleteState {
                    path: path.ok_or_else(|| {
                        minicbor::decode::Error::message("delete_state missing path")
                    })?,
                })
            }
            "toggle" => {
                check_extras(Allow { path: true, ..none })?;
                Ok(LocalAction::Toggle {
                    path: path
                        .ok_or_else(|| minicbor::decode::Error::message("toggle missing path"))?,
                })
            }
            "increment" => {
                check_extras(Allow {
                    path: true,
                    delta: true,
                    ..none
                })?;
                Ok(LocalAction::Increment {
                    path: path.ok_or_else(|| {
                        minicbor::decode::Error::message("increment missing path")
                    })?,
                    delta: delta.ok_or_else(|| {
                        minicbor::decode::Error::message("increment missing delta")
                    })?,
                })
            }
            "navigate" => {
                check_extras(Allow {
                    panel_id: true,
                    ..none
                })?;
                Ok(LocalAction::Navigate {
                    panel_id: panel_id.ok_or_else(|| {
                        minicbor::decode::Error::message("navigate missing panel_id")
                    })?,
                })
            }
            "focus" => {
                check_extras(Allow {
                    component_id: true,
                    ..none
                })?;
                Ok(LocalAction::Focus {
                    component_id: component_id.ok_or_else(|| {
                        minicbor::decode::Error::message("focus missing component_id")
                    })?,
                })
            }
            "scroll" => {
                check_extras(Allow {
                    component_id: true,
                    behavior: true,
                    ..none
                })?;
                Ok(LocalAction::Scroll {
                    component_id: component_id.ok_or_else(|| {
                        minicbor::decode::Error::message("scroll missing component_id")
                    })?,
                    behavior: behavior.ok_or_else(|| {
                        minicbor::decode::Error::message("scroll missing behavior")
                    })?,
                })
            }
            "copy" => {
                check_extras(Allow {
                    copy_value: true,
                    ..none
                })?;
                Ok(LocalAction::Copy {
                    value: copy_value
                        .ok_or_else(|| minicbor::decode::Error::message("copy missing value"))?,
                })
            }
            "confirm" => {
                check_extras(Allow {
                    title: true,
                    message: true,
                    destructive: true,
                    then: true,
                    ..none
                })?;
                Ok(LocalAction::Confirm {
                    title: title
                        .ok_or_else(|| minicbor::decode::Error::message("confirm missing title"))?,
                    message: message.ok_or_else(|| {
                        minicbor::decode::Error::message("confirm missing message")
                    })?,
                    destructive: destructive.ok_or_else(|| {
                        minicbor::decode::Error::message("confirm missing destructive")
                    })?,
                    then: then
                        .ok_or_else(|| minicbor::decode::Error::message("confirm missing then"))?,
                })
            }
            "validate" => {
                check_extras(Allow {
                    field_component_id: true,
                    rules: true,
                    on_invalid: true,
                    ..none
                })?;
                Ok(LocalAction::Validate {
                    field_component_id: field_component_id.ok_or_else(|| {
                        minicbor::decode::Error::message("validate missing field_component_id")
                    })?,
                    rules: rules.ok_or_else(|| {
                        minicbor::decode::Error::message("validate missing rules")
                    })?,
                    on_invalid: on_invalid.ok_or_else(|| {
                        minicbor::decode::Error::message("validate missing on_invalid")
                    })?,
                })
            }
            "debounce" => {
                check_extras(Allow {
                    ms: true,
                    then: true,
                    ..none
                })?;
                Ok(LocalAction::Debounce {
                    ms: ms
                        .ok_or_else(|| minicbor::decode::Error::message("debounce missing ms"))?,
                    then: then
                        .ok_or_else(|| minicbor::decode::Error::message("debounce missing then"))?,
                })
            }
            "sequence" => {
                check_extras(Allow {
                    steps: true,
                    ..none
                })?;
                Ok(LocalAction::Sequence {
                    steps: steps.ok_or_else(|| {
                        minicbor::decode::Error::message("sequence missing steps")
                    })?,
                })
            }
            "conditional" => {
                check_extras(Allow {
                    when_cond: true,
                    then: true,
                    else_branch: true,
                    ..none
                })?;
                Ok(LocalAction::Conditional {
                    when: when_cond.ok_or_else(|| {
                        minicbor::decode::Error::message("conditional missing when")
                    })?,
                    then: then.ok_or_else(|| {
                        minicbor::decode::Error::message("conditional missing then")
                    })?,
                    else_branch,
                })
            }
            "noop" => {
                check_extras(none)?;
                Ok(LocalAction::Noop)
            }
            other => Err(minicbor::decode::Error::message(format!(
                "unknown LocalAction.kind: {other}"
            ))),
        }
    }
}

/// Top-level handler attached to a Component event (§10.3).
#[derive(Debug, Clone, PartialEq)]
pub enum Handler {
    Local(LocalAction),
    Backend {
        action_id: String,
        params: CborMap,
        optimistic: Option<Vec<PatchOp>>,
        on_failure: FailurePolicy,
    },
    Both {
        action_id: String,
        params: CborMap,
        optimistic: Vec<PatchOp>,
        on_failure: FailurePolicy,
    },
}

impl<C> Encode<C> for Handler {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Keys across Handler variants:
        //   "action"     (0x66..)
        //   "action_id"  (0x69..)
        //   "kind"       (0x64..)
        //   "on_failure" (0x6a..)
        //   "optimistic" (0x6a..)
        //   "params"     (0x66..)
        // Canonical bytewise:
        //   "kind" (0x64..) <
        //   "action" (0x66 61..) < "params" (0x66 70..) <
        //   "action_id" (0x69..) <
        //   "on_failure" (0x6a 6f..) < "optimistic" (0x6a 70..)
        match self {
            Handler::Local(action) => {
                e.map(2)?;
                e.str("kind")?.str("local")?;
                e.str("action")?;
                action.encode(e, ctx)?;
            }
            Handler::Backend {
                action_id,
                params,
                optimistic,
                on_failure,
            } => {
                let n = if optimistic.is_some() { 5 } else { 4 };
                e.map(n)?;
                e.str("kind")?.str("backend")?;
                e.str("params")?;
                params.encode(e, ctx)?;
                e.str("action_id")?.str(action_id)?;
                e.str("on_failure")?;
                on_failure.encode(e, ctx)?;
                if let Some(ops) = optimistic {
                    e.str("optimistic")?;
                    e.array(ops.len() as u64)?;
                    for op in ops {
                        op.encode(e, ctx)?;
                    }
                }
            }
            Handler::Both {
                action_id,
                params,
                optimistic,
                on_failure,
            } => {
                e.map(5)?;
                e.str("kind")?.str("both")?;
                e.str("params")?;
                params.encode(e, ctx)?;
                e.str("action_id")?.str(action_id)?;
                e.str("on_failure")?;
                on_failure.encode(e, ctx)?;
                e.str("optimistic")?;
                e.array(optimistic.len() as u64)?;
                for op in optimistic {
                    op.encode(e, ctx)?;
                }
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for Handler {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut local_action: Option<LocalAction> = None;
        let mut action_id: Option<String> = None;
        let mut params: Option<CborMap> = None;
        let mut optimistic: Option<Vec<PatchOp>> = None;
        let mut on_failure: Option<FailurePolicy> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    assert_no_dup_tstr(&kind, "Handler", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "action" => {
                    assert_no_dup_tstr(&local_action, "Handler", "action")?;
                    local_action = Some(LocalAction::decode(d, ctx)?);
                }
                "action_id" => {
                    assert_no_dup_tstr(&action_id, "Handler", "action_id")?;
                    action_id = Some(d.str()?.to_string());
                }
                "params" => {
                    assert_no_dup_tstr(&params, "Handler", "params")?;
                    params = Some(CborMap::decode(d, ctx)?);
                }
                "optimistic" => {
                    assert_no_dup_tstr(&optimistic, "Handler", "optimistic")?;
                    let n = d.array()?.ok_or_else(|| {
                        minicbor::decode::Error::message("indefinite-length array forbidden")
                    })?;
                    let mut v = Vec::with_capacity(n as usize);
                    for _ in 0..n {
                        v.push(PatchOp::decode(d, ctx)?);
                    }
                    optimistic = Some(v);
                }
                "on_failure" => {
                    assert_no_dup_tstr(&on_failure, "Handler", "on_failure")?;
                    on_failure = Some(FailurePolicy::decode(d, ctx)?);
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown Handler key: {other}"
                    )))
                }
            }
        }
        let kind = kind.ok_or_else(|| minicbor::decode::Error::message("Handler missing kind"))?;
        match kind.as_str() {
            "local" => {
                if action_id.is_some()
                    || params.is_some()
                    || optimistic.is_some()
                    || on_failure.is_some()
                {
                    return Err(minicbor::decode::Error::message(
                        "Handler.local must only carry action",
                    ));
                }
                Ok(Handler::Local(local_action.ok_or_else(|| {
                    minicbor::decode::Error::message("Handler.local missing action")
                })?))
            }
            "backend" => {
                if local_action.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "Handler.backend must not carry action",
                    ));
                }
                Ok(Handler::Backend {
                    action_id: action_id.ok_or_else(|| {
                        minicbor::decode::Error::message("Handler.backend missing action_id")
                    })?,
                    params: params.ok_or_else(|| {
                        minicbor::decode::Error::message("Handler.backend missing params")
                    })?,
                    optimistic,
                    on_failure: on_failure.ok_or_else(|| {
                        minicbor::decode::Error::message("Handler.backend missing on_failure")
                    })?,
                })
            }
            "both" => {
                if local_action.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "Handler.both must not carry action",
                    ));
                }
                Ok(Handler::Both {
                    action_id: action_id.ok_or_else(|| {
                        minicbor::decode::Error::message("Handler.both missing action_id")
                    })?,
                    params: params.ok_or_else(|| {
                        minicbor::decode::Error::message("Handler.both missing params")
                    })?,
                    optimistic: optimistic.ok_or_else(|| {
                        minicbor::decode::Error::message("Handler.both missing optimistic")
                    })?,
                    on_failure: on_failure.ok_or_else(|| {
                        minicbor::decode::Error::message("Handler.both missing on_failure")
                    })?,
                })
            }
            other => Err(minicbor::decode::Error::message(format!(
                "unknown Handler.kind: {other}"
            ))),
        }
    }
}

/// Reasons `Handler::validate` rejects a tree (§10.3 limits).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerValidationError {
    DepthExceeded,
    StepsExceeded,
    SequenceTooLarge,
    NestedSequence,
    DebounceMsTooLarge,
    DebounceMsZero,
}

impl core::fmt::Display for HandlerValidationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DepthExceeded => write!(
                f,
                "Handler tree recursion depth exceeds HANDLER_MAX_RECURSION_DEPTH ({HANDLER_MAX_RECURSION_DEPTH})"
            ),
            Self::StepsExceeded => write!(
                f,
                "Handler tree step count exceeds HANDLER_MAX_TOTAL_STEPS ({HANDLER_MAX_TOTAL_STEPS})"
            ),
            Self::SequenceTooLarge => write!(
                f,
                "Sequence carries more than SEQUENCE_MAX_ITEMS ({SEQUENCE_MAX_ITEMS}) items"
            ),
            Self::NestedSequence => write!(
                f,
                "Sequence may not contain another Sequence anywhere in its subtree (linear chains only — sticky across Confirm/Debounce/Conditional then/else)"
            ),
            Self::DebounceMsTooLarge => write!(
                f,
                "Debounce.ms exceeds DEBOUNCE_MAX_MS ({DEBOUNCE_MAX_MS})"
            ),
            Self::DebounceMsZero => write!(f, "Debounce.ms must be > 0"),
        }
    }
}

impl std::error::Error for HandlerValidationError {}

impl Handler {
    /// Validate the handler tree against §10.3 limits.
    pub fn validate(&self) -> Result<(), HandlerValidationError> {
        let mut steps = 0usize;
        validate_handler(self, 0, &mut steps, false)
    }
}

impl LocalAction {
    /// Validate a stand-alone local action against §10.3 limits.
    pub fn validate(&self) -> Result<(), HandlerValidationError> {
        let mut steps = 0usize;
        validate_local(self, 0, &mut steps, false)
    }
}

// Step counting: each action node (LocalAction variant or Backend/Both Handler)
// counts as exactly one step. Handler::Local is a thin wrapper around a single
// LocalAction; we count it once, in validate_local. inside_sequence is sticky
// once entered — any Sequence in any descendant of an outer Sequence (through
// Confirm.then, Debounce.then, Conditional.then/else, custom FailurePolicy
// action) is rejected.

fn validate_handler(
    h: &Handler,
    depth: usize,
    steps: &mut usize,
    inside_sequence: bool,
) -> Result<(), HandlerValidationError> {
    if depth > HANDLER_MAX_RECURSION_DEPTH {
        return Err(HandlerValidationError::DepthExceeded);
    }
    match h {
        Handler::Local(action) => validate_local(action, depth, steps, inside_sequence),
        Handler::Backend { on_failure, .. } | Handler::Both { on_failure, .. } => {
            *steps += 1;
            if *steps > HANDLER_MAX_TOTAL_STEPS {
                return Err(HandlerValidationError::StepsExceeded);
            }
            if let FailurePolicy::Custom { action } = on_failure {
                validate_local(action, depth + 1, steps, inside_sequence)?;
            }
            Ok(())
        }
    }
}

fn validate_local(
    a: &LocalAction,
    depth: usize,
    steps: &mut usize,
    inside_sequence: bool,
) -> Result<(), HandlerValidationError> {
    if depth > HANDLER_MAX_RECURSION_DEPTH {
        return Err(HandlerValidationError::DepthExceeded);
    }
    *steps += 1;
    if *steps > HANDLER_MAX_TOTAL_STEPS {
        return Err(HandlerValidationError::StepsExceeded);
    }
    match a {
        LocalAction::ShowModal { .. }
        | LocalAction::HideModal { .. }
        | LocalAction::ToggleSlot { .. }
        | LocalAction::SetState { .. }
        | LocalAction::DeleteState { .. }
        | LocalAction::Toggle { .. }
        | LocalAction::Increment { .. }
        | LocalAction::Navigate { .. }
        | LocalAction::Focus { .. }
        | LocalAction::Scroll { .. }
        | LocalAction::Copy { .. }
        | LocalAction::Noop => Ok(()),
        LocalAction::Confirm { then, .. } => {
            validate_handler(then, depth + 1, steps, inside_sequence)
        }
        LocalAction::Debounce { ms, then } => {
            if *ms == 0 {
                return Err(HandlerValidationError::DebounceMsZero);
            }
            if *ms > DEBOUNCE_MAX_MS {
                return Err(HandlerValidationError::DebounceMsTooLarge);
            }
            validate_handler(then, depth + 1, steps, inside_sequence)
        }
        LocalAction::Validate { on_invalid, .. } => {
            validate_local(on_invalid, depth + 1, steps, inside_sequence)
        }
        LocalAction::Sequence { steps: items } => {
            if inside_sequence {
                return Err(HandlerValidationError::NestedSequence);
            }
            if items.len() > SEQUENCE_MAX_ITEMS {
                return Err(HandlerValidationError::SequenceTooLarge);
            }
            for item in items {
                validate_handler(item, depth + 1, steps, true)?;
            }
            Ok(())
        }
        LocalAction::Conditional {
            then, else_branch, ..
        } => {
            validate_handler(then, depth + 1, steps, inside_sequence)?;
            if let Some(eb) = else_branch {
                validate_handler(eb, depth + 1, steps, inside_sequence)?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ui::bind::{PathSegment, StatePath};

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

    #[test]
    fn local_action_leaves_roundtrip() {
        rt(LocalAction::ShowModal {
            slot_id: "x".into(),
        });
        rt(LocalAction::Noop);
        rt(LocalAction::Increment {
            path: StatePath::new(vec![PathSegment::Key("__draft".into())]),
            delta: -1,
        });
        rt(LocalAction::Scroll {
            component_id: "scrl".into(),
            behavior: ScrollBehavior::Smooth,
        });
    }

    #[test]
    fn handler_local_roundtrip() {
        rt(Handler::Local(LocalAction::Noop));
    }

    #[test]
    fn handler_backend_roundtrip() {
        rt(Handler::Backend {
            action_id: "save_camera".into(),
            params: CborMap::default(),
            optimistic: Some(vec![]),
            on_failure: FailurePolicy::Toast,
        });
    }

    #[test]
    fn handler_both_with_custom_failure_roundtrip() {
        rt(Handler::Both {
            action_id: "send".into(),
            params: CborMap::default(),
            optimistic: vec![],
            on_failure: FailurePolicy::Custom {
                action: LocalAction::Noop,
            },
        });
    }

    #[test]
    fn confirm_with_nested_handler_roundtrip() {
        rt(Handler::Local(LocalAction::Confirm {
            title: "Delete?".into(),
            message: "This is permanent.".into(),
            destructive: true,
            then: Box::new(Handler::Backend {
                action_id: "delete_item".into(),
                params: CborMap::default(),
                optimistic: None,
                on_failure: FailurePolicy::RevertOptimistic,
            }),
        }));
    }

    #[test]
    fn sequence_with_two_steps_validates_ok() {
        let seq = LocalAction::Sequence {
            steps: vec![
                Handler::Local(LocalAction::Noop),
                Handler::Local(LocalAction::Focus {
                    component_id: "a".into(),
                }),
            ],
        };
        assert!(seq.validate().is_ok());
    }

    #[test]
    fn sequence_exceeding_max_items_rejected() {
        let steps = (0..(SEQUENCE_MAX_ITEMS + 1))
            .map(|_| Handler::Local(LocalAction::Noop))
            .collect::<Vec<_>>();
        let seq = LocalAction::Sequence { steps };
        assert_eq!(
            seq.validate(),
            Err(HandlerValidationError::SequenceTooLarge)
        );
    }

    #[test]
    fn nested_sequence_rejected() {
        let inner = LocalAction::Sequence {
            steps: vec![Handler::Local(LocalAction::Noop)],
        };
        let outer = LocalAction::Sequence {
            steps: vec![Handler::Local(inner)],
        };
        assert_eq!(
            outer.validate(),
            Err(HandlerValidationError::NestedSequence)
        );
    }

    #[test]
    fn debounce_ms_zero_rejected() {
        let d = LocalAction::Debounce {
            ms: 0,
            then: Box::new(Handler::Local(LocalAction::Noop)),
        };
        assert_eq!(d.validate(), Err(HandlerValidationError::DebounceMsZero));
    }

    #[test]
    fn debounce_ms_over_limit_rejected() {
        let d = LocalAction::Debounce {
            ms: DEBOUNCE_MAX_MS + 1,
            then: Box::new(Handler::Local(LocalAction::Noop)),
        };
        assert_eq!(
            d.validate(),
            Err(HandlerValidationError::DebounceMsTooLarge)
        );
    }

    #[test]
    fn sequence_with_eight_local_steps_passes_step_budget() {
        // Sequence (1 step) + 8 Local actions = 9 total. Must stay under
        // HANDLER_MAX_TOTAL_STEPS = 16. Regression: earlier validator
        // double-counted Handler::Local + LocalAction inner, making this 17.
        let steps = (0..SEQUENCE_MAX_ITEMS)
            .map(|_| Handler::Local(LocalAction::Noop))
            .collect::<Vec<_>>();
        let seq = LocalAction::Sequence { steps };
        assert_eq!(seq.validate(), Ok(()));
    }

    #[test]
    fn sequence_inside_confirm_inside_sequence_rejected() {
        // Sequence([ Confirm{then: Local(Sequence(...))} ]) — sticky
        // inside_sequence must catch the inner Sequence.
        let inner = LocalAction::Sequence {
            steps: vec![Handler::Local(LocalAction::Noop)],
        };
        let outer = LocalAction::Sequence {
            steps: vec![Handler::Local(LocalAction::Confirm {
                title: "t".into(),
                message: "m".into(),
                destructive: false,
                then: Box::new(Handler::Local(inner)),
            })],
        };
        assert_eq!(
            outer.validate(),
            Err(HandlerValidationError::NestedSequence)
        );
    }

    #[test]
    fn sequence_inside_debounce_inside_sequence_rejected() {
        let inner = LocalAction::Sequence {
            steps: vec![Handler::Local(LocalAction::Noop)],
        };
        let outer = LocalAction::Sequence {
            steps: vec![Handler::Local(LocalAction::Debounce {
                ms: 100,
                then: Box::new(Handler::Local(inner)),
            })],
        };
        assert_eq!(
            outer.validate(),
            Err(HandlerValidationError::NestedSequence)
        );
    }

    #[test]
    fn depth_exceeded_rejected() {
        // Build a long Confirm chain. Each Confirm adds 1 depth via its then handler.
        let mut h = Handler::Local(LocalAction::Noop);
        for _ in 0..(HANDLER_MAX_RECURSION_DEPTH + 2) {
            h = Handler::Local(LocalAction::Confirm {
                title: "t".into(),
                message: "m".into(),
                destructive: false,
                then: Box::new(h),
            });
        }
        let result = h.validate();
        assert!(matches!(
            result,
            Err(HandlerValidationError::DepthExceeded) | Err(HandlerValidationError::StepsExceeded)
        ));
    }
}

// =============================================================================
// File: protocol/ui/command.rs — Command discriminated union (§6.6)
// Purpose: typed side-effects shipped from addon to frontend — modals,
// toasts, navigation, focus/scroll, copy, download, confirm, form helpers,
// toast dismissal. Used both as wire message 0x0140 body and as a member of
// PanelShell.initial_commands.
// =============================================================================

use minicbor::{Decode, Decoder, Encode, Encoder};

use crate::protocol::control::CborMap;
use crate::protocol::value::Value;

use super::tokens::{DrawerSide, NavigateTarget, ScrollBehavior, Tone};

/// Allowed filename character class for Command::Download (§6.6).
fn is_valid_download_filename(s: &str) -> bool {
    if s.is_empty() || s.len() > 128 {
        return false;
    }
    s.bytes()
        .all(|b| matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
}

/// Side-effect requested by the addon. Renderer executes synchronously when
/// possible (per §6.6 docs).
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    ShowModal {
        slot_id: String,
    },
    HideModal {
        slot_id: String,
    },
    ShowDrawer {
        slot_id: String,
        side: DrawerSide,
    },
    HideDrawer {
        slot_id: String,
    },
    Toast {
        tone: Tone,
        title: String,
        body: Option<String>,
        duration_ms: Option<u32>,
        action_label: Option<String>,
        action_id: Option<String>,
    },
    Navigate {
        panel_id: String,
        deep_link: Option<String>,
    },
    NavigateAddon {
        addon_id: String,
        panel_id: String,
    },
    NavigateExternal {
        url: String,
        target: NavigateTarget,
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
    Download {
        signed_url_ref: String,
        filename: String,
    },
    SetTitle {
        value: String,
    },
    Confirm {
        title: String,
        message: String,
        confirm_label: String,
        cancel_label: String,
        destructive: bool,
        on_confirm_action: Option<String>,
        on_confirm_params: Option<CborMap>,
    },
    ResetForm {
        component_id: String,
    },
    SetFormFieldValue {
        component_id: String,
        value: Value,
    },
    DismissToasts {
        tag: Option<String>,
    },
}

impl<C> Encode<C> for Command {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Per-variant emit order documented inline with byte-prefix reasoning.
        match self {
            Command::ShowModal { slot_id } => {
                // "kind"=0x64.. < "slot_id"=0x67..
                e.map(2)?;
                e.str("kind")?.str("show_modal")?;
                e.str("slot_id")?.str(slot_id)?;
            }
            Command::HideModal { slot_id } => {
                e.map(2)?;
                e.str("kind")?.str("hide_modal")?;
                e.str("slot_id")?.str(slot_id)?;
            }
            Command::ShowDrawer { slot_id, side } => {
                // "kind"=0x64, "side"=0x64.., "slot_id"=0x67..
                // Both kind and side start 0x64. "kind"=0x64 6b, "side"=0x64 73 → kind < side.
                e.map(3)?;
                e.str("kind")?.str("show_drawer")?;
                e.str("side")?;
                side.encode(e, ctx)?;
                e.str("slot_id")?.str(slot_id)?;
            }
            Command::HideDrawer { slot_id } => {
                e.map(2)?;
                e.str("kind")?.str("hide_drawer")?;
                e.str("slot_id")?.str(slot_id)?;
            }
            Command::Toast {
                tone,
                title,
                body,
                duration_ms,
                action_label,
                action_id,
            } => {
                // Keys present: kind, tone, body, title, duration_ms, action_label, action_id.
                //   "body"        0x64 62..
                //   "kind"        0x64 6b..
                //   "tone"        0x64 74..
                //   "title"       0x65 74..
                //   "action_id"   0x69 61 63 74 69 6f 6e 5f 69 64
                //   "duration_ms" 0x6b 64..
                //   "action_label" 0x6c 61..
                // Sort: body(0x64 62) < kind(0x64 6b) < tone(0x64 74)
                //       < title(0x65)
                //       < action_id(0x69)
                //       < duration_ms(0x6b)
                //       < action_label(0x6c)
                let mut n: u64 = 3; // kind, tone, title always present
                if body.is_some() {
                    n += 1;
                }
                if duration_ms.is_some() {
                    n += 1;
                }
                if action_label.is_some() {
                    n += 1;
                }
                if action_id.is_some() {
                    n += 1;
                }
                e.map(n)?;
                if let Some(b) = body {
                    e.str("body")?.str(b)?;
                }
                e.str("kind")?.str("toast")?;
                e.str("tone")?;
                tone.encode(e, ctx)?;
                e.str("title")?.str(title)?;
                if let Some(a) = action_id {
                    e.str("action_id")?.str(a)?;
                }
                if let Some(d) = duration_ms {
                    e.str("duration_ms")?.u32(*d)?;
                }
                if let Some(l) = action_label {
                    e.str("action_label")?.str(l)?;
                }
            }
            Command::Navigate {
                panel_id,
                deep_link,
            } => {
                // Keys: kind(0x64..), panel_id(0x68..), deep_link(0x69..).
                let n = if deep_link.is_some() { 3 } else { 2 };
                e.map(n)?;
                e.str("kind")?.str("navigate")?;
                e.str("panel_id")?.str(panel_id)?;
                if let Some(dl) = deep_link {
                    e.str("deep_link")?.str(dl)?;
                }
            }
            Command::NavigateAddon { addon_id, panel_id } => {
                // Keys: addon_id(0x68..), kind(0x64..), panel_id(0x68..).
                //   "addon_id"=0x68 61.., "kind"=0x64 6b.., "panel_id"=0x68 70..
                // Sort: kind(0x64) < addon_id(0x68 61) < panel_id(0x68 70).
                e.map(3)?;
                e.str("kind")?.str("navigate_addon")?;
                e.str("addon_id")?.str(addon_id)?;
                e.str("panel_id")?.str(panel_id)?;
            }
            Command::NavigateExternal { url, target } => {
                // Validate scheme. §6.6 says https:// only.
                if !url.starts_with("https://") {
                    return Err(minicbor::encode::Error::message(
                        "Command::NavigateExternal.url must use https:// scheme",
                    ));
                }
                // Keys: kind(0x64..), target(0x66..), url(0x63..).
                //   "url"=0x63.., "kind"=0x64.., "target"=0x66..
                e.map(3)?;
                e.str("url")?.str(url)?;
                e.str("kind")?.str("navigate_external")?;
                e.str("target")?;
                target.encode(e, ctx)?;
            }
            Command::Focus { component_id } => {
                // Keys: component_id(0x6c..), kind(0x64..). → kind, component_id.
                e.map(2)?;
                e.str("kind")?.str("focus")?;
                e.str("component_id")?.str(component_id)?;
            }
            Command::Scroll {
                component_id,
                behavior,
            } => {
                // Keys: behavior(0x68..), kind(0x64..), component_id(0x6c..).
                e.map(3)?;
                e.str("kind")?.str("scroll")?;
                e.str("behavior")?;
                behavior.encode(e, ctx)?;
                e.str("component_id")?.str(component_id)?;
            }
            Command::Copy { value } => {
                // Keys: kind(0x64..), value(0x65..).
                e.map(2)?;
                e.str("kind")?.str("copy")?;
                e.str("value")?.str(value)?;
            }
            Command::Download {
                signed_url_ref,
                filename,
            } => {
                if !is_valid_download_filename(filename) {
                    return Err(minicbor::encode::Error::message(
                        "Command::Download.filename must match [a-zA-Z0-9._-]+, length 1..=128",
                    ));
                }
                // Keys: filename(0x68..), kind(0x64..), signed_url_ref(0x6e..).
                e.map(3)?;
                e.str("kind")?.str("download")?;
                e.str("filename")?.str(filename)?;
                e.str("signed_url_ref")?.str(signed_url_ref)?;
            }
            Command::SetTitle { value } => {
                e.map(2)?;
                e.str("kind")?.str("set_title")?;
                e.str("value")?.str(value)?;
            }
            Command::Confirm {
                title,
                message,
                confirm_label,
                cancel_label,
                destructive,
                on_confirm_action,
                on_confirm_params,
            } => {
                // Keys: kind, title, message, confirm_label, cancel_label,
                //       destructive, on_confirm_action, on_confirm_params.
                //   "kind"               0x64 6b..
                //   "title"              0x65 74..
                //   "cancel_label"       0x6c 63..
                //   "message"            0x67 6d..
                //   "destructive"        0x6b 64..
                //   "confirm_label"      0x6d 63..
                //   "on_confirm_action"  0x71 6f..
                //   "on_confirm_params"  0x71 6f..
                // Sort by bytes:
                //   kind(0x64) < title(0x65) < message(0x67) < destructive(0x6b)
                //   < cancel_label(0x6c) < confirm_label(0x6d)
                //   < on_confirm_action(0x71 ... 0x61 ..) < on_confirm_params(0x71 ... 0x70 ..)
                let mut n: u64 = 5; // kind, title, message, confirm_label, cancel_label, destructive (6 required)
                n += 1; // destructive
                if on_confirm_action.is_some() {
                    n += 1;
                }
                if on_confirm_params.is_some() {
                    n += 1;
                }
                e.map(n)?;
                e.str("kind")?.str("confirm")?;
                e.str("title")?.str(title)?;
                e.str("message")?.str(message)?;
                e.str("destructive")?.bool(*destructive)?;
                e.str("cancel_label")?.str(cancel_label)?;
                e.str("confirm_label")?.str(confirm_label)?;
                if let Some(a) = on_confirm_action {
                    e.str("on_confirm_action")?.str(a)?;
                }
                if let Some(p) = on_confirm_params {
                    e.str("on_confirm_params")?;
                    p.encode(e, ctx)?;
                }
            }
            Command::ResetForm { component_id } => {
                e.map(2)?;
                e.str("kind")?.str("reset_form")?;
                e.str("component_id")?.str(component_id)?;
            }
            Command::SetFormFieldValue {
                component_id,
                value,
            } => {
                // Keys: kind(0x64..), value(0x65..), component_id(0x6c..).
                e.map(3)?;
                e.str("kind")?.str("set_form_field_value")?;
                e.str("value")?;
                value.encode(e, ctx)?;
                e.str("component_id")?.str(component_id)?;
            }
            Command::DismissToasts { tag } => {
                // Keys: kind(0x64..), tag(0x63..).
                //   "tag"=0x63.., "kind"=0x64..
                // Sort: tag < kind.
                let n = if tag.is_some() { 2 } else { 1 };
                e.map(n)?;
                if let Some(t) = tag {
                    e.str("tag")?.str(t)?;
                }
                e.str("kind")?.str("dismiss_toasts")?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for Command {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut slot_id: Option<String> = None;
        let mut side: Option<DrawerSide> = None;
        let mut tone: Option<Tone> = None;
        let mut title: Option<String> = None;
        let mut body: Option<String> = None;
        let mut duration_ms: Option<u32> = None;
        let mut action_label: Option<String> = None;
        let mut action_id: Option<String> = None;
        let mut panel_id: Option<String> = None;
        let mut deep_link: Option<String> = None;
        let mut addon_id: Option<String> = None;
        let mut url: Option<String> = None;
        let mut target: Option<NavigateTarget> = None;
        let mut component_id: Option<String> = None;
        let mut behavior: Option<ScrollBehavior> = None;
        let mut copy_value: Option<String> = None;
        let mut signed_url_ref: Option<String> = None;
        let mut filename: Option<String> = None;
        let mut value: Option<Value> = None;
        let mut message: Option<String> = None;
        let mut confirm_label: Option<String> = None;
        let mut cancel_label: Option<String> = None;
        let mut destructive: Option<bool> = None;
        let mut on_confirm_action: Option<String> = None;
        let mut on_confirm_params: Option<CborMap> = None;
        let mut tag: Option<String> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => kind = Some(d.str()?.to_string()),
                "slot_id" => slot_id = Some(d.str()?.to_string()),
                "side" => side = Some(DrawerSide::decode(d, ctx)?),
                "tone" => tone = Some(Tone::decode(d, ctx)?),
                "title" => title = Some(d.str()?.to_string()),
                "body" => body = Some(d.str()?.to_string()),
                "duration_ms" => duration_ms = Some(d.u32()?),
                "action_label" => action_label = Some(d.str()?.to_string()),
                "action_id" => action_id = Some(d.str()?.to_string()),
                "panel_id" => panel_id = Some(d.str()?.to_string()),
                "deep_link" => deep_link = Some(d.str()?.to_string()),
                "addon_id" => addon_id = Some(d.str()?.to_string()),
                "url" => url = Some(d.str()?.to_string()),
                "target" => target = Some(NavigateTarget::decode(d, ctx)?),
                "component_id" => component_id = Some(d.str()?.to_string()),
                "behavior" => behavior = Some(ScrollBehavior::decode(d, ctx)?),
                "value" => match kind.as_deref() {
                    Some("copy") | Some("set_title") => copy_value = Some(d.str()?.to_string()),
                    _ => value = Some(Value::decode(d, ctx)?),
                },
                "signed_url_ref" => signed_url_ref = Some(d.str()?.to_string()),
                "filename" => filename = Some(d.str()?.to_string()),
                "message" => message = Some(d.str()?.to_string()),
                "confirm_label" => confirm_label = Some(d.str()?.to_string()),
                "cancel_label" => cancel_label = Some(d.str()?.to_string()),
                "destructive" => destructive = Some(d.bool()?),
                "on_confirm_action" => on_confirm_action = Some(d.str()?.to_string()),
                "on_confirm_params" => on_confirm_params = Some(CborMap::decode(d, ctx)?),
                "tag" => tag = Some(d.str()?.to_string()),
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown Command key: {other}"
                    )))
                }
            }
        }
        let kind = kind.ok_or_else(|| minicbor::decode::Error::message("Command missing kind"))?;
        // Per-variant whitelist check + variant construction.
        // To avoid an N×M boolean matrix we use a closure that asserts a tight
        // set of expected fields.
        // Fixed-width whitelist mask. Order matches `Command::ALL_FIELDS` below.
        // Length is asserted statically so a short array would fail to compile.
        const FIELD_COUNT: usize = 26;
        let present: [bool; FIELD_COUNT] = [
            slot_id.is_some(),           //  0
            side.is_some(),              //  1
            tone.is_some(),              //  2
            title.is_some(),             //  3
            body.is_some(),              //  4
            duration_ms.is_some(),       //  5
            action_label.is_some(),      //  6
            action_id.is_some(),         //  7
            panel_id.is_some(),          //  8
            deep_link.is_some(),         //  9
            addon_id.is_some(),          // 10
            url.is_some(),               // 11
            target.is_some(),            // 12
            component_id.is_some(),      // 13
            behavior.is_some(),          // 14
            copy_value.is_some(),        // 15
            signed_url_ref.is_some(),    // 16
            filename.is_some(),          // 17
            value.is_some(),             // 18
            message.is_some(),           // 19
            confirm_label.is_some(),     // 20
            cancel_label.is_some(),      // 21
            destructive.is_some(),       // 22
            on_confirm_action.is_some(), // 23
            on_confirm_params.is_some(), // 24
            tag.is_some(),               // 25
        ];
        let want_only = |allowed: &[bool; FIELD_COUNT]| -> Result<(), minicbor::decode::Error> {
            for i in 0..FIELD_COUNT {
                if !allowed[i] && present[i] {
                    return Err(minicbor::decode::Error::message(
                        "Command variant carries a field not allowed by its kind",
                    ));
                }
            }
            Ok(())
        };
        // Indices: [slot_id, side, tone, title, body, duration_ms, action_label,
        //   action_id, panel_id, deep_link, addon_id, url, target, component_id,
        //   behavior, copy_value, signed_url_ref, filename, value, message,
        //   confirm_label, cancel_label, destructive, on_confirm_action,
        //   on_confirm_params, tag].
        match kind.as_str() {
            "show_modal" => {
                want_only(&[
                    true, false, false, false, false, false, false, false, false, false, false,
                    false, false, false, false, false, false, false, false, false, false, false,
                    false, false, false, false,
                ])?;
                Ok(Command::ShowModal {
                    slot_id: slot_id.ok_or_else(|| {
                        minicbor::decode::Error::message("show_modal missing slot_id")
                    })?,
                })
            }
            "hide_modal" => {
                want_only(&[
                    true, false, false, false, false, false, false, false, false, false, false,
                    false, false, false, false, false, false, false, false, false, false, false,
                    false, false, false, false,
                ])?;
                Ok(Command::HideModal {
                    slot_id: slot_id.ok_or_else(|| {
                        minicbor::decode::Error::message("hide_modal missing slot_id")
                    })?,
                })
            }
            "show_drawer" => {
                want_only(&[
                    true, true, false, false, false, false, false, false, false, false, false,
                    false, false, false, false, false, false, false, false, false, false, false,
                    false, false, false, false,
                ])?;
                Ok(Command::ShowDrawer {
                    slot_id: slot_id.ok_or_else(|| {
                        minicbor::decode::Error::message("show_drawer missing slot_id")
                    })?,
                    side: side.ok_or_else(|| {
                        minicbor::decode::Error::message("show_drawer missing side")
                    })?,
                })
            }
            "hide_drawer" => {
                want_only(&[
                    true, false, false, false, false, false, false, false, false, false, false,
                    false, false, false, false, false, false, false, false, false, false, false,
                    false, false, false, false,
                ])?;
                Ok(Command::HideDrawer {
                    slot_id: slot_id.ok_or_else(|| {
                        minicbor::decode::Error::message("hide_drawer missing slot_id")
                    })?,
                })
            }
            "toast" => {
                want_only(&[
                    false, false, true, true, true, true, true, true, false, false, false, false,
                    false, false, false, false, false, false, false, false, false, false, false,
                    false, false, false,
                ])?;
                Ok(Command::Toast {
                    tone: tone
                        .ok_or_else(|| minicbor::decode::Error::message("toast missing tone"))?,
                    title: title
                        .ok_or_else(|| minicbor::decode::Error::message("toast missing title"))?,
                    body,
                    duration_ms,
                    action_label,
                    action_id,
                })
            }
            "navigate" => {
                want_only(&[
                    false, false, false, false, false, false, false, false, true, true, false,
                    false, false, false, false, false, false, false, false, false, false, false,
                    false, false, false, false,
                ])?;
                Ok(Command::Navigate {
                    panel_id: panel_id.ok_or_else(|| {
                        minicbor::decode::Error::message("navigate missing panel_id")
                    })?,
                    deep_link,
                })
            }
            "navigate_addon" => {
                want_only(&[
                    false, false, false, false, false, false, false, false, true, false, true,
                    false, false, false, false, false, false, false, false, false, false, false,
                    false, false, false, false,
                ])?;
                Ok(Command::NavigateAddon {
                    addon_id: addon_id.ok_or_else(|| {
                        minicbor::decode::Error::message("navigate_addon missing addon_id")
                    })?,
                    panel_id: panel_id.ok_or_else(|| {
                        minicbor::decode::Error::message("navigate_addon missing panel_id")
                    })?,
                })
            }
            "navigate_external" => {
                want_only(&[
                    false, false, false, false, false, false, false, false, false, false, false,
                    true, true, false, false, false, false, false, false, false, false, false,
                    false, false, false, false,
                ])?;
                let url = url.ok_or_else(|| {
                    minicbor::decode::Error::message("navigate_external missing url")
                })?;
                if !url.starts_with("https://") {
                    return Err(minicbor::decode::Error::message(
                        "navigate_external.url must use https:// scheme",
                    ));
                }
                Ok(Command::NavigateExternal {
                    url,
                    target: target.ok_or_else(|| {
                        minicbor::decode::Error::message("navigate_external missing target")
                    })?,
                })
            }
            "focus" => {
                want_only(&[
                    false, false, false, false, false, false, false, false, false, false, false,
                    false, false, true, false, false, false, false, false, false, false, false,
                    false, false, false, false,
                ])?;
                Ok(Command::Focus {
                    component_id: component_id.ok_or_else(|| {
                        minicbor::decode::Error::message("focus missing component_id")
                    })?,
                })
            }
            "scroll" => {
                want_only(&[
                    false, false, false, false, false, false, false, false, false, false, false,
                    false, false, true, true, false, false, false, false, false, false, false,
                    false, false, false, false,
                ])?;
                Ok(Command::Scroll {
                    component_id: component_id.ok_or_else(|| {
                        minicbor::decode::Error::message("scroll missing component_id")
                    })?,
                    behavior: behavior.ok_or_else(|| {
                        minicbor::decode::Error::message("scroll missing behavior")
                    })?,
                })
            }
            "copy" => {
                want_only(&[
                    false, false, false, false, false, false, false, false, false, false, false,
                    false, false, false, false, true, false, false, false, false, false, false,
                    false, false, false, false,
                ])?;
                Ok(Command::Copy {
                    value: copy_value
                        .ok_or_else(|| minicbor::decode::Error::message("copy missing value"))?,
                })
            }
            "download" => {
                want_only(&[
                    false, false, false, false, false, false, false, false, false, false, false,
                    false, false, false, false, false, true, true, false, false, false, false,
                    false, false, false, false,
                ])?;
                let filename = filename
                    .ok_or_else(|| minicbor::decode::Error::message("download missing filename"))?;
                if !is_valid_download_filename(&filename) {
                    return Err(minicbor::decode::Error::message(
                        "download.filename must match [a-zA-Z0-9._-]+, length 1..=128",
                    ));
                }
                Ok(Command::Download {
                    signed_url_ref: signed_url_ref.ok_or_else(|| {
                        minicbor::decode::Error::message("download missing signed_url_ref")
                    })?,
                    filename,
                })
            }
            "set_title" => {
                want_only(&[
                    false, false, false, false, false, false, false, false, false, false, false,
                    false, false, false, false, true, false, false, false, false, false, false,
                    false, false, false, false,
                ])?;
                Ok(Command::SetTitle {
                    value: copy_value.ok_or_else(|| {
                        minicbor::decode::Error::message("set_title missing value")
                    })?,
                })
            }
            "confirm" => {
                want_only(&[
                    false, false, false, true, false, false, false, false, false, false, false,
                    false, false, false, false, false, false, false, false, true, true, true, true,
                    true, true, false,
                ])?;
                Ok(Command::Confirm {
                    title: title
                        .ok_or_else(|| minicbor::decode::Error::message("confirm missing title"))?,
                    message: message.ok_or_else(|| {
                        minicbor::decode::Error::message("confirm missing message")
                    })?,
                    confirm_label: confirm_label.ok_or_else(|| {
                        minicbor::decode::Error::message("confirm missing confirm_label")
                    })?,
                    cancel_label: cancel_label.ok_or_else(|| {
                        minicbor::decode::Error::message("confirm missing cancel_label")
                    })?,
                    destructive: destructive.ok_or_else(|| {
                        minicbor::decode::Error::message("confirm missing destructive")
                    })?,
                    on_confirm_action,
                    on_confirm_params,
                })
            }
            "reset_form" => {
                want_only(&[
                    false, false, false, false, false, false, false, false, false, false, false,
                    false, false, true, false, false, false, false, false, false, false, false,
                    false, false, false, false,
                ])?;
                Ok(Command::ResetForm {
                    component_id: component_id.ok_or_else(|| {
                        minicbor::decode::Error::message("reset_form missing component_id")
                    })?,
                })
            }
            "set_form_field_value" => {
                want_only(&[
                    false, false, false, false, false, false, false, false, false, false, false,
                    false, false, true, false, false, false, false, true, false, false, false,
                    false, false, false, false,
                ])?;
                Ok(Command::SetFormFieldValue {
                    component_id: component_id.ok_or_else(|| {
                        minicbor::decode::Error::message(
                            "set_form_field_value missing component_id",
                        )
                    })?,
                    value: value.ok_or_else(|| {
                        minicbor::decode::Error::message("set_form_field_value missing value")
                    })?,
                })
            }
            "dismiss_toasts" => {
                want_only(&[
                    false, false, false, false, false, false, false, false, false, false, false,
                    false, false, false, false, false, false, false, false, false, false, false,
                    false, false, false, true,
                ])?;
                Ok(Command::DismissToasts { tag })
            }
            other => Err(minicbor::decode::Error::message(format!(
                "unknown Command.kind: {other}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(v: Command) {
        let mut b1 = Vec::new();
        minicbor::encode(&v, &mut b1).unwrap();
        let d: Command = minicbor::decode(&b1).unwrap();
        assert_eq!(d, v);
        let mut b2 = Vec::new();
        minicbor::encode(&d, &mut b2).unwrap();
        assert_eq!(b1, b2);
    }

    #[test]
    fn all_simple_commands_roundtrip() {
        rt(Command::ShowModal {
            slot_id: "m".into(),
        });
        rt(Command::HideModal {
            slot_id: "m".into(),
        });
        rt(Command::ShowDrawer {
            slot_id: "d".into(),
            side: DrawerSide::Right,
        });
        rt(Command::HideDrawer {
            slot_id: "d".into(),
        });
        rt(Command::Navigate {
            panel_id: "main".into(),
            deep_link: Some("/x".into()),
        });
        rt(Command::Navigate {
            panel_id: "main".into(),
            deep_link: None,
        });
        rt(Command::NavigateAddon {
            addon_id: "contacts".into(),
            panel_id: "list".into(),
        });
        rt(Command::Focus {
            component_id: "input1".into(),
        });
        rt(Command::Scroll {
            component_id: "list".into(),
            behavior: ScrollBehavior::Smooth,
        });
        rt(Command::Copy {
            value: "secret-hash".into(),
        });
        rt(Command::SetTitle {
            value: "New Title".into(),
        });
        rt(Command::ResetForm {
            component_id: "form1".into(),
        });
        rt(Command::DismissToasts { tag: None });
        rt(Command::DismissToasts {
            tag: Some("upload".into()),
        });
    }

    #[test]
    fn toast_full_roundtrip() {
        rt(Command::Toast {
            tone: Tone::Success,
            title: "Saved".into(),
            body: Some("Camera 1 added".into()),
            duration_ms: Some(3000),
            action_label: Some("Undo".into()),
            action_id: Some("undo_save".into()),
        });
    }

    #[test]
    fn navigate_external_https_roundtrip() {
        rt(Command::NavigateExternal {
            url: "https://example.com/docs".into(),
            target: NavigateTarget::NewTab,
        });
    }

    #[test]
    fn navigate_external_non_https_rejected_on_encode() {
        let bad = Command::NavigateExternal {
            url: "http://example.com".into(),
            target: NavigateTarget::NewTab,
        };
        let mut buf = Vec::new();
        let res = minicbor::encode(&bad, &mut buf);
        assert!(res.is_err());
    }

    #[test]
    fn download_filename_validation() {
        rt(Command::Download {
            signed_url_ref: "ref-abc".into(),
            filename: "report_2026-05.csv".into(),
        });
        let bad = Command::Download {
            signed_url_ref: "r".into(),
            filename: "../etc/passwd".into(),
        };
        let mut buf = Vec::new();
        assert!(minicbor::encode(&bad, &mut buf).is_err());
    }

    #[test]
    fn confirm_full_roundtrip() {
        rt(Command::Confirm {
            title: "Delete camera?".into(),
            message: "Permanent.".into(),
            confirm_label: "Delete".into(),
            cancel_label: "Cancel".into(),
            destructive: true,
            on_confirm_action: Some("delete_camera".into()),
            on_confirm_params: Some(CborMap(vec![("id".into(), Value::U64(1))])),
        });
    }

    #[test]
    fn show_modal_with_extra_tone_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(3)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("show_modal")
            .unwrap()
            .str("slot_id")
            .unwrap()
            .str("m")
            .unwrap()
            .str("tone")
            .unwrap()
            .str("primary")
            .unwrap();
        let res: Result<Command, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }
}

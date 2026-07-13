// =============================================================================
// File: protocol/ui/slot.rs — SlotDecl + slot enums + StateEntry (§6.2)
// Purpose: PanelShell's slot declarations, default-state/cache/visibility
// policies, and the tuple (path, value) used for initial_state and
// state_overlay arrays.
// =============================================================================

use minicbor::{Decode, Decoder, Encode, Encoder};

use crate::protocol::value::Value;

use super::bind::StatePath;
use super::component::Component;

string_enum! {
    /// Slot role hint that guides the renderer.
    pub enum SlotSemantics {
        MainContent = "main_content",
        Modal = "modal",
        Drawer = "drawer",
        Toast = "toast",
        SidePanel = "side_panel",
        TabPane = "tab_pane",
        Popover = "popover",
        Custom = "custom",
    }
}

/// Initial content of a slot before the addon sends its first SlotContent.
#[derive(Debug, Clone, PartialEq)]
pub enum SlotDefault {
    Empty,
    Loading,
    Static { fragment: Component },
}

impl<C> Encode<C> for SlotDefault {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Canonical: fragment(0x66..) < kind(0x64..)? No: 0x64 < 0x66.
        // So kind first, then fragment.
        match self {
            SlotDefault::Empty => {
                e.map(1)?;
                e.str("kind")?.str("empty")?;
            }
            SlotDefault::Loading => {
                e.map(1)?;
                e.str("kind")?.str("loading")?;
            }
            SlotDefault::Static { fragment } => {
                e.map(2)?;
                e.str("kind")?.str("static")?;
                e.str("fragment")?;
                fragment.encode(e, ctx)?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for SlotDefault {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut fragment: Option<Component> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => kind = Some(d.str()?.to_string()),
                "fragment" => fragment = Some(Component::decode(d, ctx)?),
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown SlotDefault key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("SlotDefault missing kind"))?;
        match kind.as_str() {
            "empty" => {
                if fragment.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "SlotDefault.empty must not carry fragment",
                    ));
                }
                Ok(SlotDefault::Empty)
            }
            "loading" => {
                if fragment.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "SlotDefault.loading must not carry fragment",
                    ));
                }
                Ok(SlotDefault::Loading)
            }
            "static" => Ok(SlotDefault::Static {
                fragment: fragment.ok_or_else(|| {
                    minicbor::decode::Error::message("SlotDefault.static missing fragment")
                })?,
            }),
            other => Err(minicbor::decode::Error::message(format!(
                "unknown SlotDefault.kind: {other}"
            ))),
        }
    }
}

/// Cache lifecycle for slot content.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CachePolicy {
    None,
    OnNavigateBack,
    TtlSeconds { value: u32 },
}

impl<C> Encode<C> for CachePolicy {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        match self {
            CachePolicy::None => {
                e.map(1)?;
                e.str("kind")?.str("none")?;
            }
            CachePolicy::OnNavigateBack => {
                e.map(1)?;
                e.str("kind")?.str("on_navigate_back")?;
            }
            CachePolicy::TtlSeconds { value } => {
                e.map(2)?;
                e.str("kind")?.str("ttl_seconds")?;
                e.str("value")?.u32(*value)?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for CachePolicy {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut value: Option<u32> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => kind = Some(d.str()?.to_string()),
                "value" => value = Some(d.u32()?),
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown CachePolicy key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("CachePolicy missing kind"))?;
        match kind.as_str() {
            "none" => {
                if value.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "CachePolicy.none must not carry value",
                    ));
                }
                Ok(CachePolicy::None)
            }
            "on_navigate_back" => {
                if value.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "CachePolicy.on_navigate_back must not carry value",
                    ));
                }
                Ok(CachePolicy::OnNavigateBack)
            }
            "ttl_seconds" => Ok(CachePolicy::TtlSeconds {
                value: value.ok_or_else(|| {
                    minicbor::decode::Error::message("CachePolicy.ttl_seconds missing value")
                })?,
            }),
            other => Err(minicbor::decode::Error::message(format!(
                "unknown CachePolicy.kind: {other}"
            ))),
        }
    }
}

/// Visibility policy for a slot.
#[derive(Debug, Clone, PartialEq)]
pub enum SlotVisibility {
    Always,
    Hidden,
    Conditional { path: StatePath },
}

impl<C> Encode<C> for SlotVisibility {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        match self {
            SlotVisibility::Always => {
                e.map(1)?;
                e.str("kind")?.str("always")?;
            }
            SlotVisibility::Hidden => {
                e.map(1)?;
                e.str("kind")?.str("hidden")?;
            }
            SlotVisibility::Conditional { path } => {
                e.map(2)?;
                e.str("kind")?.str("conditional")?;
                e.str("path")?;
                path.encode(e, ctx)?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for SlotVisibility {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut path: Option<StatePath> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => kind = Some(d.str()?.to_string()),
                "path" => path = Some(StatePath::decode(d, ctx)?),
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown SlotVisibility key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("SlotVisibility missing kind"))?;
        match kind.as_str() {
            "always" => {
                if path.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "SlotVisibility.always must not carry path",
                    ));
                }
                Ok(SlotVisibility::Always)
            }
            "hidden" => {
                if path.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "SlotVisibility.hidden must not carry path",
                    ));
                }
                Ok(SlotVisibility::Hidden)
            }
            "conditional" => Ok(SlotVisibility::Conditional {
                path: path.ok_or_else(|| {
                    minicbor::decode::Error::message("SlotVisibility.conditional missing path")
                })?,
            }),
            other => Err(minicbor::decode::Error::message(format!(
                "unknown SlotVisibility.kind: {other}"
            ))),
        }
    }
}

/// Per-slot declaration in a PanelShell (§6.2).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct SlotDecl {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub semantics: SlotSemantics,
    #[n(2)]
    pub default_state: SlotDefault,
    #[n(3)]
    pub cache_policy: CachePolicy,
    #[n(4)]
    pub visibility: SlotVisibility,
    #[n(5)]
    pub max_payload_bytes: Option<u32>,
}

/// Tuple `(path, value)` used in state arrays where map keys would otherwise be
/// complex CBOR structures (§6.4 explanation).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct StateEntry {
    #[n(0)]
    pub path: StatePath,
    #[n(1)]
    pub value: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ui::bind::PathSegment;
    use crate::protocol::ui::component::{Component, FieldMap};

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

    fn empty_component() -> Component {
        Component {
            tag: 0x0001,
            id: "root".into(),
            fields: FieldMap::default(),
            handlers: None,
            bind: None,
            a11y: None,
            visibility: None,
            test_id: None,
        }
    }

    #[test]
    fn slot_default_variants_roundtrip() {
        rt(SlotDefault::Empty);
        rt(SlotDefault::Loading);
        rt(SlotDefault::Static {
            fragment: empty_component(),
        });
    }

    #[test]
    fn cache_policy_variants_roundtrip() {
        rt(CachePolicy::None);
        rt(CachePolicy::OnNavigateBack);
        rt(CachePolicy::TtlSeconds { value: 300 });
    }

    #[test]
    fn slot_visibility_variants_roundtrip() {
        rt(SlotVisibility::Always);
        rt(SlotVisibility::Hidden);
        rt(SlotVisibility::Conditional {
            path: StatePath::new(vec![PathSegment::Key("show".into())]),
        });
    }

    #[test]
    fn slot_decl_full_roundtrip() {
        rt(SlotDecl {
            id: "main".into(),
            semantics: SlotSemantics::MainContent,
            default_state: SlotDefault::Loading,
            cache_policy: CachePolicy::TtlSeconds { value: 60 },
            visibility: SlotVisibility::Always,
            max_payload_bytes: Some(64 * 1024),
        });
    }

    #[test]
    fn state_entry_roundtrip() {
        rt(StateEntry {
            path: StatePath::new(vec![PathSegment::Key("count".into())]),
            value: Value::U64(42),
        });
    }

    #[test]
    fn cache_policy_none_with_value_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(2)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("none")
            .unwrap()
            .str("value")
            .unwrap()
            .u32(1)
            .unwrap();
        let res: Result<CachePolicy, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }
}

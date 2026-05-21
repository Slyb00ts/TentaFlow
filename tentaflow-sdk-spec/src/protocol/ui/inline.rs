// =============================================================================
// File: protocol/ui/inline.rs — cross-referenced inline struct types (§1.5)
// Purpose: IconRef, AvatarRef, Badge (inline form), Trend, Footnote,
// SelectValue, BreadcrumbItem, NavTab, TabItem, MenuItem, SidebarItem,
// SelectOption + SelectGroup. Used as fields inside Component-tag schemas
// (e.g. Header.meta_chips, SidePanel.items). Pure inline structs — NO
// tag/id/handlers — interactive variants of Chip/Badge ship as Component
// instances instead.
// =============================================================================

use minicbor::{Decode, Decoder, Encode, Encoder};

use crate::protocol::value::Value;

use super::bind::{BindRef, StatePath};
use super::handler::LocalAction;
use super::icon_name::IconName;
use super::tokens::{BadgeVariant, IconSize, Tone};

// -----------------------------------------------------------------------------
// IconRef
// -----------------------------------------------------------------------------

/// Reference to an icon — either a named SVG sprite or an addon-supplied asset.
#[derive(Debug, Clone, PartialEq)]
pub enum IconRef {
    Named {
        name: IconName,
        size: Option<IconSize>,
        tone: Option<Tone>,
    },
    Asset {
        ref_: String,
        size_px: Option<u16>,
        alt: Option<String>,
    },
}

impl<C> Encode<C> for IconRef {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Canonical key sort across both variants:
        //   "alt"     0x63..
        //   "kind"    0x64..
        //   "name"    0x64..
        //   "ref"     0x63..
        //   "size"    0x64..
        //   "size_px" 0x67..
        //   "tone"    0x64..
        // Sorted by bytes:
        //   "alt"(0x63 61) < "ref"(0x63 72)
        //   < "kind"(0x64 6b) < "name"(0x64 6e) < "size"(0x64 73) < "tone"(0x64 74)
        //   < "size_px"(0x67 73)
        match self {
            IconRef::Named { name, size, tone } => {
                let mut n: u64 = 2; // kind, name
                if size.is_some() {
                    n += 1;
                }
                if tone.is_some() {
                    n += 1;
                }
                e.map(n)?;
                e.str("kind")?.str("named")?;
                e.str("name")?;
                name.encode(e, ctx)?;
                if let Some(s) = size {
                    e.str("size")?;
                    s.encode(e, ctx)?;
                }
                if let Some(t) = tone {
                    e.str("tone")?;
                    t.encode(e, ctx)?;
                }
            }
            IconRef::Asset { ref_, size_px, alt } => {
                let mut n: u64 = 2; // kind, ref
                if alt.is_some() {
                    n += 1;
                }
                if size_px.is_some() {
                    n += 1;
                }
                e.map(n)?;
                if let Some(a) = alt {
                    e.str("alt")?.str(a)?;
                }
                e.str("ref")?.str(ref_)?;
                e.str("kind")?.str("asset")?;
                if let Some(sp) = size_px {
                    e.str("size_px")?.u16(*sp)?;
                }
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for IconRef {
    fn decode(
        d: &mut Decoder<'b>,
        ctx: &mut C,
    ) -> Result<Self, minicbor::decode::Error> {
        let len = d.map()?.ok_or_else(|| {
            minicbor::decode::Error::message("indefinite-length map forbidden")
        })?;
        let mut kind: Option<String> = None;
        let mut name: Option<IconName> = None;
        let mut size: Option<IconSize> = None;
        let mut tone: Option<Tone> = None;
        let mut ref_: Option<String> = None;
        let mut size_px: Option<u16> = None;
        let mut alt: Option<String> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => kind = Some(d.str()?.to_string()),
                "name" => name = Some(IconName::decode(d, ctx)?),
                "size" => size = Some(IconSize::decode(d, ctx)?),
                "tone" => tone = Some(Tone::decode(d, ctx)?),
                "ref" => ref_ = Some(d.str()?.to_string()),
                "size_px" => size_px = Some(d.u16()?),
                "alt" => alt = Some(d.str()?.to_string()),
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown IconRef key: {other}"
                    )))
                }
            }
        }
        let kind = kind.ok_or_else(|| minicbor::decode::Error::message("IconRef missing kind"))?;
        match kind.as_str() {
            "named" => {
                if ref_.is_some() || size_px.is_some() || alt.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "IconRef.named must not carry ref/size_px/alt",
                    ));
                }
                Ok(IconRef::Named {
                    name: name.ok_or_else(|| {
                        minicbor::decode::Error::message("IconRef.named missing name")
                    })?,
                    size,
                    tone,
                })
            }
            "asset" => {
                if name.is_some() || size.is_some() || tone.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "IconRef.asset must not carry name/size/tone",
                    ));
                }
                Ok(IconRef::Asset {
                    ref_: ref_.ok_or_else(|| {
                        minicbor::decode::Error::message("IconRef.asset missing ref")
                    })?,
                    size_px,
                    alt,
                })
            }
            other => Err(minicbor::decode::Error::message(format!(
                "unknown IconRef.kind: {other}"
            ))),
        }
    }
}

// -----------------------------------------------------------------------------
// AvatarRef
// -----------------------------------------------------------------------------

/// Avatar source — image asset, initials, or an icon fallback.
#[derive(Debug, Clone, PartialEq)]
pub enum AvatarRef {
    Image { ref_: String },
    Initials { initials: String },
    Icon { icon: IconRef },
}

impl<C> Encode<C> for AvatarRef {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Keys: icon(0x64..), initials(0x68..), kind(0x64..), ref(0x63..).
        // Sort: ref(0x63) < icon(0x64..69) < kind(0x64..6b) < initials(0x68).
        match self {
            AvatarRef::Image { ref_ } => {
                e.map(2)?;
                e.str("ref")?.str(ref_)?;
                e.str("kind")?.str("image")?;
            }
            AvatarRef::Initials { initials } => {
                e.map(2)?;
                e.str("kind")?.str("initials")?;
                e.str("initials")?.str(initials)?;
            }
            AvatarRef::Icon { icon } => {
                e.map(2)?;
                e.str("icon")?;
                icon.encode(e, ctx)?;
                e.str("kind")?.str("icon")?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for AvatarRef {
    fn decode(
        d: &mut Decoder<'b>,
        ctx: &mut C,
    ) -> Result<Self, minicbor::decode::Error> {
        let len = d.map()?.ok_or_else(|| {
            minicbor::decode::Error::message("indefinite-length map forbidden")
        })?;
        let mut kind: Option<String> = None;
        let mut ref_: Option<String> = None;
        let mut initials: Option<String> = None;
        let mut icon: Option<IconRef> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => kind = Some(d.str()?.to_string()),
                "ref" => ref_ = Some(d.str()?.to_string()),
                "initials" => initials = Some(d.str()?.to_string()),
                "icon" => icon = Some(IconRef::decode(d, ctx)?),
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown AvatarRef key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("AvatarRef missing kind"))?;
        match kind.as_str() {
            "image" => {
                if initials.is_some() || icon.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "AvatarRef.image must only carry ref",
                    ));
                }
                Ok(AvatarRef::Image {
                    ref_: ref_.ok_or_else(|| {
                        minicbor::decode::Error::message("AvatarRef.image missing ref")
                    })?,
                })
            }
            "initials" => {
                if ref_.is_some() || icon.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "AvatarRef.initials must only carry initials",
                    ));
                }
                Ok(AvatarRef::Initials {
                    initials: initials.ok_or_else(|| {
                        minicbor::decode::Error::message("AvatarRef.initials missing initials")
                    })?,
                })
            }
            "icon" => {
                if ref_.is_some() || initials.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "AvatarRef.icon must only carry icon",
                    ));
                }
                Ok(AvatarRef::Icon {
                    icon: icon.ok_or_else(|| {
                        minicbor::decode::Error::message("AvatarRef.icon missing icon")
                    })?,
                })
            }
            other => Err(minicbor::decode::Error::message(format!(
                "unknown AvatarRef.kind: {other}"
            ))),
        }
    }
}

// -----------------------------------------------------------------------------
// Badge (inline form), Trend, Footnote
// -----------------------------------------------------------------------------

/// Inline Badge structure used as a field in inline struct types
/// (e.g. `MenuItem.badge`). Pure data — NO tag/id/handlers. For interactive
/// badges with a click handler use the Component-form (tag `0x020A`).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct InlineBadge {
    #[n(0)]
    pub variant: BadgeVariant,
    #[n(1)]
    pub tone: Tone,
    #[n(2)]
    pub label: Option<BindRef>,
    #[n(3)]
    pub count: Option<BindRef>,
    #[n(4)]
    pub icon: Option<IconRef>,
    #[n(5)]
    pub pulse: bool,
}

string_enum! {
    /// Direction component of `Trend` (§1.5).
    pub enum TrendDirection {
        Up = "up",
        Down = "down",
        Flat = "flat",
    }
}

/// Numeric trend indicator shown alongside `StatCard` / `Stat`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct Trend {
    #[n(0)]
    pub direction: TrendDirection,
    #[n(1)]
    pub percent: f32,
    #[n(2)]
    pub label: Option<BindRef>,
    #[n(3)]
    pub tone: Option<Tone>,
}

/// Caption line rendered below a component (e.g. helper text under an input).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct Footnote {
    #[n(0)]
    pub tone: Tone,
    #[n(1)]
    pub icon: Option<IconRef>,
    #[n(2)]
    pub content: BindRef,
}

// -----------------------------------------------------------------------------
// SelectValue (tagged union of tstr | u32 | i32 | bool)
// -----------------------------------------------------------------------------

/// Discriminated value used by `SelectOption` / `RadioOption`.
#[derive(Debug, Clone, PartialEq)]
pub enum SelectValue {
    Text(String),
    UInt(u32),
    Int(i32),
    Bool(bool),
}

impl<C> Encode<C> for SelectValue {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Canonical: kind(0x64..) < value(0x65..).
        e.map(2)?;
        e.str("kind")?;
        match self {
            SelectValue::Text(_) => {
                e.str("tstr")?;
            }
            SelectValue::UInt(_) => {
                e.str("u32")?;
            }
            SelectValue::Int(_) => {
                e.str("i32")?;
            }
            SelectValue::Bool(_) => {
                e.str("bool")?;
            }
        }
        e.str("value")?;
        match self {
            SelectValue::Text(s) => {
                e.str(s)?;
            }
            SelectValue::UInt(n) => {
                e.u32(*n)?;
            }
            SelectValue::Int(n) => {
                e.i32(*n)?;
            }
            SelectValue::Bool(b) => {
                e.bool(*b)?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for SelectValue {
    fn decode(
        d: &mut Decoder<'b>,
        _ctx: &mut C,
    ) -> Result<Self, minicbor::decode::Error> {
        let len = d.map()?.ok_or_else(|| {
            minicbor::decode::Error::message("indefinite-length map forbidden")
        })?;
        let mut kind: Option<String> = None;
        let mut value_raw: Option<Value> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => kind = Some(d.str()?.to_string()),
                "value" => value_raw = Some(Value::decode(d, _ctx)?),
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown SelectValue key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("SelectValue missing kind"))?;
        let value =
            value_raw.ok_or_else(|| minicbor::decode::Error::message("SelectValue missing value"))?;
        match (kind.as_str(), value) {
            ("tstr", Value::Text(s)) => Ok(SelectValue::Text(s)),
            ("u32", Value::U64(n)) if n <= u32::MAX as u64 => Ok(SelectValue::UInt(n as u32)),
            ("i32", Value::I64(n)) if n >= i32::MIN as i64 && n <= i32::MAX as i64 => {
                Ok(SelectValue::Int(n as i32))
            }
            ("i32", Value::U64(n)) if n <= i32::MAX as u64 => Ok(SelectValue::Int(n as i32)),
            ("bool", Value::Bool(b)) => Ok(SelectValue::Bool(b)),
            (other, _) => Err(minicbor::decode::Error::message(format!(
                "SelectValue.kind '{other}' does not match value type"
            ))),
        }
    }
}

// -----------------------------------------------------------------------------
// BreadcrumbItem, NavTab, TabItem, MenuItem, SidebarItem, SelectOption(+Group)
// -----------------------------------------------------------------------------

/// One breadcrumb entry. `action_id` and `local_action` are mutually
/// exclusive (§1.5) — decode rejects payloads carrying both.
#[derive(Debug, Clone, PartialEq, Encode)]
#[cbor(map)]
pub struct BreadcrumbItem {
    #[n(0)]
    pub label: BindRef,
    #[n(1)]
    pub icon: Option<IconRef>,
    #[n(2)]
    pub action_id: Option<String>,
    #[n(3)]
    pub local_action: Option<LocalAction>,
    /// Last item in trail; rendered as non-clickable current location.
    #[n(4)]
    pub is_current: bool,
}

impl<'b, C> Decode<'b, C> for BreadcrumbItem {
    fn decode(
        d: &mut Decoder<'b>,
        ctx: &mut C,
    ) -> Result<Self, minicbor::decode::Error> {
        let len = d.map()?.ok_or_else(|| {
            minicbor::decode::Error::message("indefinite-length map forbidden")
        })?;
        let mut label: Option<BindRef> = None;
        let mut icon: Option<IconRef> = None;
        let mut action_id: Option<String> = None;
        let mut local_action: Option<LocalAction> = None;
        let mut is_current: Option<bool> = None;
        for _ in 0..len {
            let k = d.u8()?;
            match k {
                0 => label = Some(BindRef::decode(d, ctx)?),
                1 => icon = Some(IconRef::decode(d, ctx)?),
                2 => action_id = Some(d.str()?.to_string()),
                3 => local_action = Some(LocalAction::decode(d, ctx)?),
                4 => is_current = Some(d.bool()?),
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown BreadcrumbItem key: {other}"
                    )))
                }
            }
        }
        if action_id.is_some() && local_action.is_some() {
            return Err(minicbor::decode::Error::message(
                "BreadcrumbItem: action_id and local_action are mutually exclusive (§1.5)",
            ));
        }
        Ok(BreadcrumbItem {
            label: label
                .ok_or_else(|| minicbor::decode::Error::message("BreadcrumbItem missing label"))?,
            icon,
            action_id,
            local_action,
            is_current: is_current.ok_or_else(|| {
                minicbor::decode::Error::message("BreadcrumbItem missing is_current")
            })?,
        })
    }
}

/// Top-level navigation tab (switches panel).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct NavTab {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub label: BindRef,
    #[n(2)]
    pub icon: Option<IconRef>,
    #[n(3)]
    pub badge: Option<InlineBadge>,
    #[n(4)]
    pub panel_id: Option<String>,
    #[n(5)]
    pub locked: bool,
}

/// In-panel tab (content swap).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct TabItem {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub label: BindRef,
    #[n(2)]
    pub icon: Option<IconRef>,
    #[n(3)]
    pub badge: Option<InlineBadge>,
    #[n(4)]
    pub locked: bool,
    #[n(5)]
    pub content_template_id: Option<String>,
}

/// Item in a `MenuButton` dropdown.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct MenuItem {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub label: BindRef,
    #[n(2)]
    pub icon: Option<IconRef>,
    #[n(3)]
    pub badge: Option<InlineBadge>,
    /// Display-only shortcut hint (e.g. "Ctrl+S").
    #[n(4)]
    pub shortcut: Option<String>,
    /// Visual emphasis (critical color).
    #[n(5)]
    pub danger: bool,
    #[n(6)]
    pub disabled: Option<BindRef>,
    /// Render a visual separator after this item.
    #[n(7)]
    pub divider_after: bool,
}

/// Item in a `Sidebar`. `action_id` and `local_action` are mutually exclusive
/// (§1.5) — decode rejects payloads carrying both. Supports one level of
/// nested children; the 1-level depth constraint is enforced by the host
/// validator (Krok 4).
#[derive(Debug, Clone, PartialEq, Encode)]
#[cbor(map)]
pub struct SidebarItem {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub icon: Option<IconRef>,
    #[n(2)]
    pub label: BindRef,
    #[n(3)]
    pub badge: Option<InlineBadge>,
    /// Path bound to a bool — when true the item renders in active state.
    #[n(4)]
    pub active_path: Option<StatePath>,
    #[n(5)]
    pub action_id: Option<String>,
    #[n(6)]
    pub local_action: Option<LocalAction>,
    /// Nested items (renderer enforces 1-level limit; nested children MUST be None).
    #[n(7)]
    pub children: Option<Vec<SidebarItem>>,
}

impl<'b, C> Decode<'b, C> for SidebarItem {
    fn decode(
        d: &mut Decoder<'b>,
        ctx: &mut C,
    ) -> Result<Self, minicbor::decode::Error> {
        let len = d.map()?.ok_or_else(|| {
            minicbor::decode::Error::message("indefinite-length map forbidden")
        })?;
        let mut id: Option<String> = None;
        let mut icon: Option<IconRef> = None;
        let mut label: Option<BindRef> = None;
        let mut badge: Option<InlineBadge> = None;
        let mut active_path: Option<StatePath> = None;
        let mut action_id: Option<String> = None;
        let mut local_action: Option<LocalAction> = None;
        let mut children: Option<Vec<SidebarItem>> = None;
        for _ in 0..len {
            let k = d.u8()?;
            match k {
                0 => id = Some(d.str()?.to_string()),
                1 => icon = Some(IconRef::decode(d, ctx)?),
                2 => label = Some(BindRef::decode(d, ctx)?),
                3 => badge = Some(InlineBadge::decode(d, ctx)?),
                4 => active_path = Some(StatePath::decode(d, ctx)?),
                5 => action_id = Some(d.str()?.to_string()),
                6 => local_action = Some(LocalAction::decode(d, ctx)?),
                7 => {
                    let n = d.array()?.ok_or_else(|| {
                        minicbor::decode::Error::message("indefinite-length array forbidden")
                    })?;
                    let mut v = Vec::with_capacity(n as usize);
                    for _ in 0..n {
                        v.push(SidebarItem::decode(d, ctx)?);
                    }
                    children = Some(v);
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown SidebarItem key: {other}"
                    )))
                }
            }
        }
        if action_id.is_some() && local_action.is_some() {
            return Err(minicbor::decode::Error::message(
                "SidebarItem: action_id and local_action are mutually exclusive (§1.5)",
            ));
        }
        Ok(SidebarItem {
            id: id.ok_or_else(|| minicbor::decode::Error::message("SidebarItem missing id"))?,
            icon,
            label: label
                .ok_or_else(|| minicbor::decode::Error::message("SidebarItem missing label"))?,
            badge,
            active_path,
            action_id,
            local_action,
            children,
        })
    }
}

/// Option inside a `Select` / `Combobox`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct SelectOption {
    #[n(0)]
    pub value: SelectValue,
    #[n(1)]
    pub label: BindRef,
    #[n(2)]
    pub icon: Option<IconRef>,
    #[n(3)]
    pub disabled: bool,
    #[n(4)]
    pub group_id: Option<String>,
    #[n(5)]
    pub description: Option<BindRef>,
}

/// Group header for `SelectOption.group_id` references.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct SelectGroup {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub label: BindRef,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::value::Value;

    fn rt<T>(v: T)
    where
        T: minicbor::Encode<()>
            + for<'b> minicbor::Decode<'b, ()>
            + PartialEq
            + core::fmt::Debug,
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
    fn icon_ref_named_roundtrip() {
        rt(IconRef::Named {
            name: IconName::Search,
            size: Some(IconSize::Md),
            tone: Some(Tone::Primary),
        });
        rt(IconRef::Named {
            name: IconName::Brain,
            size: None,
            tone: None,
        });
    }

    #[test]
    fn icon_ref_asset_roundtrip() {
        rt(IconRef::Asset {
            ref_: "signed-url-123".into(),
            size_px: Some(32),
            alt: Some("Custom logo".into()),
        });
    }

    #[test]
    fn icon_ref_named_with_asset_field_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(3)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("named")
            .unwrap()
            .str("name")
            .unwrap()
            .str("search")
            .unwrap()
            .str("ref")
            .unwrap()
            .str("x")
            .unwrap();
        let res: Result<IconRef, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn avatar_ref_all_variants_roundtrip() {
        rt(AvatarRef::Image {
            ref_: "url-1".into(),
        });
        rt(AvatarRef::Initials {
            initials: "PJ".into(),
        });
        rt(AvatarRef::Icon {
            icon: IconRef::Named {
                name: IconName::User,
                size: None,
                tone: None,
            },
        });
    }

    #[test]
    fn inline_badge_roundtrip() {
        rt(InlineBadge {
            variant: BadgeVariant::Pulse,
            tone: Tone::Critical,
            label: Some(BindRef::Literal(Value::Text("3".into()))),
            count: None,
            icon: None,
            pulse: true,
        });
    }

    #[test]
    fn trend_roundtrip() {
        rt(Trend {
            direction: TrendDirection::Up,
            percent: 12.5,
            label: None,
            tone: Some(Tone::Success),
        });
    }

    #[test]
    fn footnote_roundtrip() {
        rt(Footnote {
            tone: Tone::Muted,
            icon: None,
            content: BindRef::Literal(Value::Text("Last updated 5m ago".into())),
        });
    }

    #[test]
    fn select_value_all_variants_roundtrip() {
        rt(SelectValue::Text("foo".into()));
        rt(SelectValue::UInt(42));
        rt(SelectValue::Int(-7));
        rt(SelectValue::Bool(true));
    }

    #[test]
    fn select_value_kind_type_mismatch_rejected() {
        // kind="u32" but value is a string — must reject.
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(2)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("u32")
            .unwrap()
            .str("value")
            .unwrap()
            .str("not_an_int")
            .unwrap();
        let res: Result<SelectValue, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn breadcrumb_item_roundtrip() {
        rt(BreadcrumbItem {
            label: BindRef::Literal(Value::Text("Cameras".into())),
            icon: None,
            action_id: Some("nav.cameras".into()),
            local_action: None,
            is_current: false,
        });
    }

    #[test]
    fn nav_tab_roundtrip() {
        rt(NavTab {
            id: "main".into(),
            label: BindRef::Literal(Value::Text("Main".into())),
            icon: None,
            badge: None,
            panel_id: Some("main_panel".into()),
            locked: false,
        });
    }

    #[test]
    fn menu_item_roundtrip() {
        rt(MenuItem {
            id: "save".into(),
            label: BindRef::Literal(Value::Text("Save".into())),
            icon: Some(IconRef::Named {
                name: IconName::Save,
                size: None,
                tone: None,
            }),
            badge: None,
            shortcut: Some("Ctrl+S".into()),
            danger: false,
            disabled: None,
            divider_after: false,
        });
    }

    #[test]
    fn sidebar_item_with_children_roundtrip() {
        rt(SidebarItem {
            id: "root".into(),
            icon: None,
            label: BindRef::Literal(Value::Text("Apps".into())),
            badge: None,
            active_path: None,
            action_id: None,
            local_action: None,
            children: Some(vec![SidebarItem {
                id: "child".into(),
                icon: None,
                label: BindRef::Literal(Value::Text("Tentavision".into())),
                badge: None,
                active_path: None,
                action_id: Some("open.tentavision".into()),
                local_action: None,
                children: None,
            }]),
        });
    }

    #[test]
    fn breadcrumb_item_action_id_and_local_action_rejected() {
        // Carry both action_id (key 2) and local_action (key 3) — decode MUST fail.
        // Entries: label(0), action_id(2), local_action(3), is_current(4).
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(4).unwrap();
        enc.u8(0).unwrap();
        enc.map(2).unwrap().str("kind").unwrap().str("literal").unwrap();
        enc.str("value").unwrap().str("Home").unwrap();
        enc.u8(2).unwrap().str("nav.home").unwrap();
        enc.u8(3).unwrap();
        enc.map(1).unwrap().str("kind").unwrap().str("noop").unwrap();
        enc.u8(4).unwrap().bool(false).unwrap();
        let res: Result<BreadcrumbItem, _> = minicbor::decode(&buf);
        assert!(res.is_err(), "decode must reject action_id+local_action coexistence");
    }

    #[test]
    fn sidebar_item_action_id_and_local_action_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(4).unwrap();
        enc.u8(0).unwrap().str("id1").unwrap();
        enc.u8(2).unwrap();
        enc.map(2).unwrap().str("kind").unwrap().str("literal").unwrap();
        enc.str("value").unwrap().str("X").unwrap();
        enc.u8(5).unwrap().str("nav.x").unwrap();
        enc.u8(6).unwrap();
        enc.map(1).unwrap().str("kind").unwrap().str("noop").unwrap();
        let res: Result<SidebarItem, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn select_option_and_group_roundtrip() {
        rt(SelectOption {
            value: SelectValue::Text("opt1".into()),
            label: BindRef::Literal(Value::Text("Option 1".into())),
            icon: None,
            disabled: false,
            group_id: Some("g1".into()),
            description: None,
        });
        rt(SelectGroup {
            id: "g1".into(),
            label: BindRef::Literal(Value::Text("Group 1".into())),
        });
    }
}

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
use crate::protocol::ui::typed_field::assert_no_dup_tstr;

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
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
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
                "kind" => {
                    assert_no_dup_tstr(&kind, "IconRef", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "name" => {
                    assert_no_dup_tstr(&name, "IconRef", "name")?;
                    name = Some(IconName::decode(d, ctx)?);
                }
                "size" => {
                    assert_no_dup_tstr(&size, "IconRef", "size")?;
                    size = Some(IconSize::decode(d, ctx)?);
                }
                "tone" => {
                    assert_no_dup_tstr(&tone, "IconRef", "tone")?;
                    tone = Some(Tone::decode(d, ctx)?);
                }
                "ref" => {
                    assert_no_dup_tstr(&ref_, "IconRef", "ref")?;
                    ref_ = Some(d.str()?.to_string());
                }
                "size_px" => {
                    assert_no_dup_tstr(&size_px, "IconRef", "size_px")?;
                    size_px = Some(d.u16()?);
                }
                "alt" => {
                    assert_no_dup_tstr(&alt, "IconRef", "alt")?;
                    alt = Some(d.str()?.to_string());
                }
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
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut ref_: Option<String> = None;
        let mut initials: Option<String> = None;
        let mut icon: Option<IconRef> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    assert_no_dup_tstr(&kind, "AvatarRef", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "ref" => {
                    assert_no_dup_tstr(&ref_, "AvatarRef", "ref")?;
                    ref_ = Some(d.str()?.to_string());
                }
                "initials" => {
                    assert_no_dup_tstr(&initials, "AvatarRef", "initials")?;
                    initials = Some(d.str()?.to_string());
                }
                "icon" => {
                    assert_no_dup_tstr(&icon, "AvatarRef", "icon")?;
                    icon = Some(IconRef::decode(d, ctx)?);
                }
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

/// Numeric trend indicator shown alongside `StatCard` / `Stat`. `percent`
/// is `f64` (catalog Trend) — straight numeric fields use f64 to survive the
/// `Value`-roundtrip pathway typed components use when populating `FieldMap`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct Trend {
    #[n(0)]
    pub direction: TrendDirection,
    #[n(1)]
    pub percent: f64,
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
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut value_raw: Option<Value> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    assert_no_dup_tstr(&kind, "SelectValue", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "value" => {
                    assert_no_dup_tstr(&value_raw, "SelectValue", "value")?;
                    value_raw = Some(Value::decode(d, _ctx)?);
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown SelectValue key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("SelectValue missing kind"))?;
        let value = value_raw
            .ok_or_else(|| minicbor::decode::Error::message("SelectValue missing value"))?;
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
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
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
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
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
        enc.map(2)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("literal")
            .unwrap();
        enc.str("value").unwrap().str("Home").unwrap();
        enc.u8(2).unwrap().str("nav.home").unwrap();
        enc.u8(3).unwrap();
        enc.map(1)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("noop")
            .unwrap();
        enc.u8(4).unwrap().bool(false).unwrap();
        let res: Result<BreadcrumbItem, _> = minicbor::decode(&buf);
        assert!(
            res.is_err(),
            "decode must reject action_id+local_action coexistence"
        );
    }

    #[test]
    fn sidebar_item_action_id_and_local_action_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(4).unwrap();
        enc.u8(0).unwrap().str("id1").unwrap();
        enc.u8(2).unwrap();
        enc.map(2)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("literal")
            .unwrap();
        enc.str("value").unwrap().str("X").unwrap();
        enc.u8(5).unwrap().str("nav.x").unwrap();
        enc.u8(6).unwrap();
        enc.map(1)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("noop")
            .unwrap();
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

// -----------------------------------------------------------------------------
// Form items: RadioOption, RadioCardOption, SliderMark
// -----------------------------------------------------------------------------

/// Option inside a `RadioGroup` (catalog §1.5).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct RadioOption {
    #[n(0)]
    pub value: SelectValue,
    #[n(1)]
    pub label: BindRef,
    #[n(2)]
    pub hint: Option<BindRef>,
    #[n(3)]
    pub disabled: bool,
}

/// Card-style radio option for `RadioCardGroup`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct RadioCardOption {
    #[n(0)]
    pub value: SelectValue,
    #[n(1)]
    pub icon: IconRef,
    #[n(2)]
    pub title: BindRef,
    #[n(3)]
    pub description: Option<BindRef>,
    #[n(4)]
    pub badge: Option<InlineBadge>,
    #[n(5)]
    pub disabled: bool,
}

/// Visible mark on a Slider track.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct SliderMark {
    #[n(0)]
    pub value: f64,
    #[n(1)]
    pub label: Option<BindRef>,
}

// -----------------------------------------------------------------------------
// Layout: GridChild (uses Component — handled inline)
// -----------------------------------------------------------------------------

/// Cell in a `Grid` layout — wraps a child Component with span/positioning.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GridChild {
    #[n(0)]
    pub component: super::component::Component,
    #[n(1)]
    pub col_span: u8,
    #[n(2)]
    pub row_span: u8,
    #[n(3)]
    pub col_start: Option<u8>,
    #[n(4)]
    pub row_start: Option<u8>,
    #[n(5)]
    pub align_self: Option<super::tokens::FlexAlign>,
    #[n(6)]
    pub justify_self: Option<super::tokens::FlexJustify>,
}

// -----------------------------------------------------------------------------
// Data: KvItem
// -----------------------------------------------------------------------------

/// Item in a `KeyValue` list.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct KvItem {
    #[n(0)]
    pub label: BindRef,
    #[n(1)]
    pub value: BindRef,
    #[n(2)]
    pub hint: Option<BindRef>,
    #[n(3)]
    pub icon: Option<IconRef>,
    #[n(4)]
    pub action_id: Option<String>,
    #[n(5)]
    pub format: Option<super::value_format::ValueFormat>,
}

// -----------------------------------------------------------------------------
// Wizard / Feature / Timeline / Accordion / Alarm / Inbox / Decision
// -----------------------------------------------------------------------------

/// Step descriptor for `WizardShell`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct StepDef {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub label: BindRef,
    #[n(2)]
    pub optional: bool,
    #[n(3)]
    pub status: Option<BindRef>,
    #[n(4)]
    pub description: Option<BindRef>,
}

/// Item in a `FeatureList`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct FeatureItem {
    #[n(0)]
    pub icon: IconRef,
    #[n(1)]
    pub title: BindRef,
    #[n(2)]
    pub description: Option<BindRef>,
}

/// Item in a `Timeline` component.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct TimelineItem {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub ts_ms: i64,
    #[n(2)]
    pub title: BindRef,
    #[n(3)]
    pub description: Option<BindRef>,
    #[n(4)]
    pub icon: Option<IconRef>,
    #[n(5)]
    pub tone: Option<Tone>,
    #[n(6)]
    pub action_id: Option<String>,
}

/// Item in an `Accordion`. Body is a `Vec<Component>` of arbitrary children.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct AccordionItem {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub header: super::component::Component,
    #[n(2)]
    pub body: Vec<super::component::Component>,
    #[n(3)]
    pub default_expanded: bool,
}

/// Item in an `AlarmList`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct AlarmItem {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub ts_ms: i64,
    #[n(2)]
    pub tone: Tone,
    #[n(3)]
    pub title: BindRef,
    #[n(4)]
    pub description: Option<BindRef>,
    #[n(5)]
    pub icon: Option<IconRef>,
    #[n(6)]
    pub action_id: Option<String>,
    #[n(7)]
    pub acknowledged: bool,
}

/// Item in an `Inbox` list.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct InboxItem {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub ts_ms: i64,
    #[n(2)]
    pub read: bool,
    #[n(3)]
    pub title: BindRef,
    #[n(4)]
    pub preview: Option<BindRef>,
    #[n(5)]
    pub avatar: Option<AvatarRef>,
    #[n(6)]
    pub badge: Option<InlineBadge>,
    #[n(7)]
    pub action_id: String,
}

/// Option presented to the user in a decision dialog (multi-card chooser).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct DecisionOption {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub icon: IconRef,
    #[n(2)]
    pub title: BindRef,
    #[n(3)]
    pub description: Option<BindRef>,
    #[n(4)]
    pub tone: Option<Tone>,
    #[n(5)]
    pub disabled: bool,
}

// -----------------------------------------------------------------------------
// Permission / Role / Map / Graph / Segment / FilterChip / Heatmap / Gauge / Stack
// -----------------------------------------------------------------------------

/// Permission catalog entry (used in `PermissionMatrix`).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct PermissionDef {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub label: BindRef,
    #[n(2)]
    pub description: Option<BindRef>,
    #[n(3)]
    pub category: Option<String>,
}

/// Role catalog entry (used in `PermissionMatrix`).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct RoleDef {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub label: BindRef,
    #[n(2)]
    pub color: Option<Tone>,
    #[n(3)]
    pub description: Option<BindRef>,
}

/// Pin on a `MapView`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct MapMarker {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub lat: f64,
    #[n(2)]
    pub lng: f64,
    #[n(3)]
    pub icon: Option<IconRef>,
    #[n(4)]
    pub label: Option<BindRef>,
    #[n(5)]
    pub tone: Option<Tone>,
    #[n(6)]
    pub popup_content: Option<BindRef>,
}

/// Node in a `RelationGraph`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GraphNode {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub label: BindRef,
    #[n(2)]
    pub node_type: String,
    #[n(3)]
    pub icon: Option<IconRef>,
    #[n(4)]
    pub tone: Option<Tone>,
}

/// Edge in a `RelationGraph`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GraphEdge {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub source_id: String,
    #[n(2)]
    pub target_id: String,
    #[n(3)]
    pub label: Option<BindRef>,
    #[n(4)]
    pub weight: Option<f64>,
    #[n(5)]
    pub tone: Option<Tone>,
}

/// Option in a `SegmentedControl`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct SegmentOption {
    #[n(0)]
    pub value: SelectValue,
    #[n(1)]
    pub label: Option<BindRef>,
    #[n(2)]
    pub icon: Option<IconRef>,
    #[n(3)]
    pub badge: Option<InlineBadge>,
}

/// Definition of a chip in a `FilterChipBar`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct FilterChipDef {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub label: BindRef,
    #[n(2)]
    pub icon: Option<IconRef>,
    #[n(3)]
    pub badge: Option<InlineBadge>,
    #[n(4)]
    pub count_path: Option<StatePath>,
}

/// Row label in a `Heatmap`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct HeatmapRow {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub label: BindRef,
}

/// Column label in a `Heatmap`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct HeatmapColumn {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub label: BindRef,
}

/// Bucket in a categorical Heatmap scale.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct HeatmapBucket {
    #[n(0)]
    pub threshold: f64,
    #[n(1)]
    pub tone: Tone,
    #[n(2)]
    pub label: Option<BindRef>,
}

/// Threshold tick on a `Gauge`. `value` is f64 (catalog Gauge) — naked
/// numeric fields use f64 to survive the `Value`-roundtrip pathway.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GaugeThreshold {
    #[n(0)]
    pub value: f64,
    #[n(1)]
    pub tone: Tone,
    #[n(2)]
    pub label: Option<BindRef>,
}

/// Segment in a `StackedBar`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct StackSegment {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub value: BindRef,
    #[n(2)]
    pub label: Option<BindRef>,
    #[n(3)]
    pub tone: Tone,
}

/// Entry in a `DataDefinitionList`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct DefItem {
    #[n(0)]
    pub term: BindRef,
    #[n(1)]
    pub definition: BindRef,
}

// -----------------------------------------------------------------------------
// File / Date / Range
// -----------------------------------------------------------------------------

/// FileInput row state (catalog §1.5 FileMeta).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct FileMeta {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub name: String,
    #[n(2)]
    pub size_bytes: u64,
    #[n(3)]
    pub mime: String,
    #[n(4)]
    pub ts_ms: i64,
    /// Upload progress 0.0..=1.0.
    #[n(5)]
    pub upload_progress: f64,
    #[n(6)]
    pub status: super::tokens::FileUploadStatus,
    #[n(7)]
    pub signed_url_ref: Option<String>,
    #[n(8)]
    pub error_message: Option<String>,
}

/// Calendar range preset's inline range descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct RangePresetRange {
    #[n(0)]
    pub from_offset_days: i32,
    #[n(1)]
    pub to_offset_days: i32,
}

/// Preset entry for a Range/`DateRangePicker`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct RangePreset {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub label: BindRef,
    #[n(2)]
    pub range: RangePresetRange,
}

// -----------------------------------------------------------------------------
// Chart family — leaf structs (sub-enums already declared in tokens.rs)
// -----------------------------------------------------------------------------

/// One data series in a chart.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct ChartSeries {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub name: BindRef,
    #[n(2)]
    pub data_path: StatePath,
    #[n(3)]
    pub tone: Option<Tone>,
    #[n(4)]
    pub style: super::tokens::ChartSeriesStyle,
    #[n(5)]
    pub show_in_legend: bool,
}

/// Chart axis descriptor.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct ChartAxis {
    #[n(0)]
    pub label: Option<BindRef>,
    #[n(1)]
    pub format: Option<super::value_format::ValueFormat>,
    #[n(2)]
    pub min: Option<f64>,
    #[n(3)]
    pub max: Option<f64>,
    /// Suggested tick count (renderer hint).
    #[n(4)]
    pub ticks: Option<u8>,
    #[n(5)]
    pub scale: super::tokens::ChartAxisScale,
}

/// Chart legend configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct ChartLegend {
    #[n(0)]
    pub position: super::tokens::ChartLegendPosition,
    #[n(1)]
    pub alignment: super::tokens::ChartLegendAlign,
}

/// Chart tooltip configuration.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct ChartTooltip {
    #[n(0)]
    pub enabled: bool,
    #[n(1)]
    pub format: Option<super::value_format::ValueFormat>,
}

// -----------------------------------------------------------------------------
// Table family — TablePagination, TableSort, TableColumn
// -----------------------------------------------------------------------------

/// Pagination config for a Table.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct TablePagination {
    #[n(0)]
    pub page_size: u32,
    /// Bound u32 (current 1-based page index).
    #[n(1)]
    pub current_page_path: StatePath,
    #[n(2)]
    pub show_size_picker: bool,
}

/// Active sort hint for a Table.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct TableSort {
    #[n(0)]
    pub column_id: String,
    #[n(1)]
    pub direction: super::tokens::SortDirection,
}

/// Column descriptor for a Table.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct TableColumn {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub header: BindRef,
    /// Path relative to a row; segments-array (not StatePath because rooted at row).
    #[n(2)]
    pub field_path: Vec<super::bind::PathSegment>,
    #[n(3)]
    pub width: TableColumnWidth,
    #[n(4)]
    pub render: super::tokens::ColumnRender,
    #[n(5)]
    pub format: Option<super::value_format::ValueFormat>,
    #[n(6)]
    pub align: Option<super::tokens::TextAlign>,
    #[n(7)]
    pub sortable: bool,
    #[n(8)]
    pub hidden_by_default: bool,
    #[n(9)]
    pub sticky_left: bool,
}

// -----------------------------------------------------------------------------
// DimensionToken — discriminated union, always CBOR map z `kind`.
// -----------------------------------------------------------------------------

/// CSS dimension token (catalog §1.5). Always wire-encoded as a CBOR map with
/// `kind` key — even for unit variants (`{kind:"auto"}`, etc.).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DimensionToken {
    Auto,
    Full,
    FitContent,
    Px { value: u32 },
    Vh { value: u8 },
    Vw { value: u8 },
    Fr { value: u8 },
    Percent { value: u8 },
    Spacing { value: super::tokens::Spacing },
}

impl<C> Encode<C> for DimensionToken {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Canonical key order: kind(0x64..) < value(0x65..).
        match self {
            DimensionToken::Auto => {
                e.map(1)?;
                e.str("kind")?.str("auto")?;
            }
            DimensionToken::Full => {
                e.map(1)?;
                e.str("kind")?.str("full")?;
            }
            DimensionToken::FitContent => {
                e.map(1)?;
                e.str("kind")?.str("fit_content")?;
            }
            DimensionToken::Px { value } => {
                e.map(2)?;
                e.str("kind")?.str("px")?;
                e.str("value")?.u32(*value)?;
            }
            DimensionToken::Vh { value } => {
                e.map(2)?;
                e.str("kind")?.str("vh")?;
                e.str("value")?.u8(*value)?;
            }
            DimensionToken::Vw { value } => {
                e.map(2)?;
                e.str("kind")?.str("vw")?;
                e.str("value")?.u8(*value)?;
            }
            DimensionToken::Fr { value } => {
                e.map(2)?;
                e.str("kind")?.str("fr")?;
                e.str("value")?.u8(*value)?;
            }
            DimensionToken::Percent { value } => {
                e.map(2)?;
                e.str("kind")?.str("percent")?;
                e.str("value")?.u8(*value)?;
            }
            DimensionToken::Spacing { value } => {
                e.map(2)?;
                e.str("kind")?.str("spacing")?;
                e.str("value")?;
                value.encode(e, ctx)?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for DimensionToken {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        // Two-pass: buffer `value` as generic `Value` so decode stays
        // independent of `kind` vs `value` ordering on the wire; the
        // `spacing` token text is resolved after `kind` is known.
        let mut kind: Option<String> = None;
        let mut value_raw: Option<Value> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    assert_no_dup_tstr(&kind, "DimensionToken", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "value" => {
                    assert_no_dup_tstr(&value_raw, "DimensionToken", "value")?;
                    value_raw = Some(Value::decode(d, ctx)?);
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown DimensionToken key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("DimensionToken missing kind"))?;
        let need_no_value = |has: bool, k: &str| -> Result<(), minicbor::decode::Error> {
            if has {
                return Err(minicbor::decode::Error::message(format!(
                    "DimensionToken.{k} must not carry value"
                )));
            }
            Ok(())
        };
        match kind.as_str() {
            "auto" => {
                need_no_value(value_raw.is_some(), "auto")?;
                Ok(DimensionToken::Auto)
            }
            "full" => {
                need_no_value(value_raw.is_some(), "full")?;
                Ok(DimensionToken::Full)
            }
            "fit_content" => {
                need_no_value(value_raw.is_some(), "fit_content")?;
                Ok(DimensionToken::FitContent)
            }
            other => {
                let take_u = || -> Result<u64, minicbor::decode::Error> {
                    match value_raw {
                        Some(Value::U64(n)) => Ok(n),
                        _ => Err(minicbor::decode::Error::message(
                            "DimensionToken numeric variant requires u-integer value",
                        )),
                    }
                };
                match other {
                    "px" => Ok(DimensionToken::Px {
                        value: take_u()?.try_into().map_err(|_| {
                            minicbor::decode::Error::message(
                                "DimensionToken.px value out of u32 range",
                            )
                        })?,
                    }),
                    "vh" => Ok(DimensionToken::Vh {
                        value: take_u()?.try_into().map_err(|_| {
                            minicbor::decode::Error::message(
                                "DimensionToken.vh value out of u8 range",
                            )
                        })?,
                    }),
                    "vw" => Ok(DimensionToken::Vw {
                        value: take_u()?.try_into().map_err(|_| {
                            minicbor::decode::Error::message(
                                "DimensionToken.vw value out of u8 range",
                            )
                        })?,
                    }),
                    "fr" => Ok(DimensionToken::Fr {
                        value: take_u()?.try_into().map_err(|_| {
                            minicbor::decode::Error::message(
                                "DimensionToken.fr value out of u8 range",
                            )
                        })?,
                    }),
                    "percent" => Ok(DimensionToken::Percent {
                        value: take_u()?.try_into().map_err(|_| {
                            minicbor::decode::Error::message(
                                "DimensionToken.percent value out of u8 range",
                            )
                        })?,
                    }),
                    "spacing" => match value_raw {
                        Some(Value::Text(s)) => Ok(DimensionToken::Spacing {
                            value: super::tokens::Spacing::from_wire(&s).ok_or_else(|| {
                                minicbor::decode::Error::message(
                                    "DimensionToken.spacing: unknown Spacing token",
                                )
                            })?,
                        }),
                        Some(_) => Err(minicbor::decode::Error::message(
                            "DimensionToken.spacing value must be a Spacing token (tstr)",
                        )),
                        None => Err(minicbor::decode::Error::message(
                            "DimensionToken.spacing missing value",
                        )),
                    },
                    other => Err(minicbor::decode::Error::message(format!(
                        "unknown DimensionToken.kind: {other}"
                    ))),
                }
            }
        }
    }
}

// -----------------------------------------------------------------------------
// AspectRatio — discriminated union, always CBOR map z `kind`.
// -----------------------------------------------------------------------------

/// Aspect-ratio token (catalog §1.5). Always wire-encoded as `{kind: "1:1"|"16:9"|...}`
/// or `{kind: "custom", ratio: f64}`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AspectRatio {
    R1To1,
    R16To9,
    R4To3,
    R21To9,
    R3To2,
    R2To1,
    R9To16,
    R3To4,
    Custom { ratio: f64 },
}

impl AspectRatio {
    const fn wire_kind(self) -> &'static str {
        match self {
            Self::R1To1 => "1:1",
            Self::R16To9 => "16:9",
            Self::R4To3 => "4:3",
            Self::R21To9 => "21:9",
            Self::R3To2 => "3:2",
            Self::R2To1 => "2:1",
            Self::R9To16 => "9:16",
            Self::R3To4 => "3:4",
            Self::Custom { .. } => "custom",
        }
    }
}

impl<C> Encode<C> for AspectRatio {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        match self {
            AspectRatio::Custom { ratio } => {
                e.map(2)?;
                e.str("kind")?.str("custom")?;
                e.str("ratio")?.f64(*ratio)?;
            }
            other => {
                e.map(1)?;
                e.str("kind")?.str(other.wire_kind())?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for AspectRatio {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut ratio: Option<f64> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    assert_no_dup_tstr(&kind, "AspectRatio", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "ratio" => {
                    assert_no_dup_tstr(&ratio, "AspectRatio", "ratio")?;
                    ratio = Some(d.f64()?);
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown AspectRatio key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("AspectRatio missing kind"))?;
        let no_ratio = |has: bool, k: &str| -> Result<(), minicbor::decode::Error> {
            if has {
                return Err(minicbor::decode::Error::message(format!(
                    "AspectRatio.{k} must not carry ratio"
                )));
            }
            Ok(())
        };
        match kind.as_str() {
            "1:1" => {
                no_ratio(ratio.is_some(), "1:1")?;
                Ok(AspectRatio::R1To1)
            }
            "16:9" => {
                no_ratio(ratio.is_some(), "16:9")?;
                Ok(AspectRatio::R16To9)
            }
            "4:3" => {
                no_ratio(ratio.is_some(), "4:3")?;
                Ok(AspectRatio::R4To3)
            }
            "21:9" => {
                no_ratio(ratio.is_some(), "21:9")?;
                Ok(AspectRatio::R21To9)
            }
            "3:2" => {
                no_ratio(ratio.is_some(), "3:2")?;
                Ok(AspectRatio::R3To2)
            }
            "2:1" => {
                no_ratio(ratio.is_some(), "2:1")?;
                Ok(AspectRatio::R2To1)
            }
            "9:16" => {
                no_ratio(ratio.is_some(), "9:16")?;
                Ok(AspectRatio::R9To16)
            }
            "3:4" => {
                no_ratio(ratio.is_some(), "3:4")?;
                Ok(AspectRatio::R3To4)
            }
            "custom" => Ok(AspectRatio::Custom {
                ratio: ratio.ok_or_else(|| {
                    minicbor::decode::Error::message("AspectRatio.custom missing ratio")
                })?,
            }),
            other => Err(minicbor::decode::Error::message(format!(
                "unknown AspectRatio.kind: {other}"
            ))),
        }
    }
}

// -----------------------------------------------------------------------------
// TableColumnWidth — discriminated union.
// -----------------------------------------------------------------------------

/// Width spec for a TableColumn (catalog §1.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableColumnWidth {
    Auto,
    MinContent,
    MaxContent,
    Px { value: u32 },
    Fr { value: u8 },
}

impl<C> Encode<C> for TableColumnWidth {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        match self {
            TableColumnWidth::Auto => {
                e.map(1)?;
                e.str("kind")?.str("auto")?;
            }
            TableColumnWidth::MinContent => {
                e.map(1)?;
                e.str("kind")?.str("min_content")?;
            }
            TableColumnWidth::MaxContent => {
                e.map(1)?;
                e.str("kind")?.str("max_content")?;
            }
            TableColumnWidth::Px { value } => {
                e.map(2)?;
                e.str("kind")?.str("px")?;
                e.str("value")?.u32(*value)?;
            }
            TableColumnWidth::Fr { value } => {
                e.map(2)?;
                e.str("kind")?.str("fr")?;
                e.str("value")?.u8(*value)?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for TableColumnWidth {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut value: Option<u64> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    assert_no_dup_tstr(&kind, "TableColumnWidth", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "value" => {
                    assert_no_dup_tstr(&value, "TableColumnWidth", "value")?;
                    value = Some(d.u64()?);
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown TableColumnWidth key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("TableColumnWidth missing kind"))?;
        let no_val = |has: bool, k: &str| -> Result<(), minicbor::decode::Error> {
            if has {
                return Err(minicbor::decode::Error::message(format!(
                    "TableColumnWidth.{k} must not carry value"
                )));
            }
            Ok(())
        };
        match kind.as_str() {
            "auto" => {
                no_val(value.is_some(), "auto")?;
                Ok(TableColumnWidth::Auto)
            }
            "min_content" => {
                no_val(value.is_some(), "min_content")?;
                Ok(TableColumnWidth::MinContent)
            }
            "max_content" => {
                no_val(value.is_some(), "max_content")?;
                Ok(TableColumnWidth::MaxContent)
            }
            "px" => Ok(TableColumnWidth::Px {
                value: value
                    .ok_or_else(|| {
                        minicbor::decode::Error::message("TableColumnWidth.px missing value")
                    })?
                    .try_into()
                    .map_err(|_| {
                        minicbor::decode::Error::message(
                            "TableColumnWidth.px value out of u32 range",
                        )
                    })?,
            }),
            "fr" => Ok(TableColumnWidth::Fr {
                value: value
                    .ok_or_else(|| {
                        minicbor::decode::Error::message("TableColumnWidth.fr missing value")
                    })?
                    .try_into()
                    .map_err(|_| {
                        minicbor::decode::Error::message(
                            "TableColumnWidth.fr value out of u8 range",
                        )
                    })?,
            }),
            other => Err(minicbor::decode::Error::message(format!(
                "unknown TableColumnWidth.kind: {other}"
            ))),
        }
    }
}

// -----------------------------------------------------------------------------
// HeatmapScale — discriminated union.
// -----------------------------------------------------------------------------

/// Color-mapping scale for a `Heatmap`.
#[derive(Debug, Clone, PartialEq)]
pub enum HeatmapScale {
    Linear {
        min: f64,
        max: f64,
        color_from: Tone,
        color_to: Tone,
    },
    Logarithmic {
        min: f64,
        max: f64,
        base: f64,
    },
    Categorical {
        buckets: Vec<HeatmapBucket>,
    },
}

impl<C> Encode<C> for HeatmapScale {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        match self {
            HeatmapScale::Linear {
                min,
                max,
                color_from,
                color_to,
            } => {
                // Keys with full encoded prefix:
                //   "max"=0x63 6d 61.., "min"=0x63 6d 69..,
                //   "kind"=0x64 6b.., "color_to"=0x68 63.., "color_from"=0x6a 63..
                // Canonical sort: max < min < kind < color_to < color_from.
                e.map(5)?;
                e.str("max")?.f64(*max)?;
                e.str("min")?.f64(*min)?;
                e.str("kind")?.str("linear")?;
                e.str("color_to")?;
                color_to.encode(e, ctx)?;
                e.str("color_from")?;
                color_from.encode(e, ctx)?;
            }
            HeatmapScale::Logarithmic { min, max, base } => {
                // Keys: base(0x64..), kind(0x64..), max(0x63..), min(0x63..).
                // Sort: max(0x63 6d 61) < min(0x63 6d 69) < base(0x64 62) < kind(0x64 6b).
                e.map(4)?;
                e.str("max")?.f64(*max)?;
                e.str("min")?.f64(*min)?;
                e.str("base")?.f64(*base)?;
                e.str("kind")?.str("logarithmic")?;
            }
            HeatmapScale::Categorical { buckets } => {
                // Keys: buckets(0x67..), kind(0x64..). Sort: kind < buckets.
                e.map(2)?;
                e.str("kind")?.str("categorical")?;
                e.str("buckets")?;
                e.array(buckets.len() as u64)?;
                for b in buckets {
                    b.encode(e, ctx)?;
                }
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for HeatmapScale {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut min: Option<f64> = None;
        let mut max: Option<f64> = None;
        let mut color_from: Option<Tone> = None;
        let mut color_to: Option<Tone> = None;
        let mut base: Option<f64> = None;
        let mut buckets: Option<Vec<HeatmapBucket>> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    assert_no_dup_tstr(&kind, "HeatmapScale", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "min" => {
                    assert_no_dup_tstr(&min, "HeatmapScale", "min")?;
                    min = Some(d.f64()?);
                }
                "max" => {
                    assert_no_dup_tstr(&max, "HeatmapScale", "max")?;
                    max = Some(d.f64()?);
                }
                "color_from" => {
                    assert_no_dup_tstr(&color_from, "HeatmapScale", "color_from")?;
                    color_from = Some(Tone::decode(d, ctx)?);
                }
                "color_to" => {
                    assert_no_dup_tstr(&color_to, "HeatmapScale", "color_to")?;
                    color_to = Some(Tone::decode(d, ctx)?);
                }
                "base" => {
                    assert_no_dup_tstr(&base, "HeatmapScale", "base")?;
                    base = Some(d.f64()?);
                }
                "buckets" => {
                    assert_no_dup_tstr(&buckets, "HeatmapScale", "buckets")?;
                    let n = d.array()?.ok_or_else(|| {
                        minicbor::decode::Error::message("indefinite-length array forbidden")
                    })?;
                    let mut v = Vec::with_capacity(n as usize);
                    for _ in 0..n {
                        v.push(HeatmapBucket::decode(d, ctx)?);
                    }
                    buckets = Some(v);
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown HeatmapScale key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("HeatmapScale missing kind"))?;
        match kind.as_str() {
            "linear" => {
                if base.is_some() || buckets.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "HeatmapScale.linear must not carry base/buckets",
                    ));
                }
                Ok(HeatmapScale::Linear {
                    min: min.ok_or_else(|| {
                        minicbor::decode::Error::message("HeatmapScale.linear missing min")
                    })?,
                    max: max.ok_or_else(|| {
                        minicbor::decode::Error::message("HeatmapScale.linear missing max")
                    })?,
                    color_from: color_from.ok_or_else(|| {
                        minicbor::decode::Error::message("HeatmapScale.linear missing color_from")
                    })?,
                    color_to: color_to.ok_or_else(|| {
                        minicbor::decode::Error::message("HeatmapScale.linear missing color_to")
                    })?,
                })
            }
            "logarithmic" => {
                if color_from.is_some() || color_to.is_some() || buckets.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "HeatmapScale.logarithmic must not carry color_from/color_to/buckets",
                    ));
                }
                Ok(HeatmapScale::Logarithmic {
                    min: min.ok_or_else(|| {
                        minicbor::decode::Error::message("HeatmapScale.logarithmic missing min")
                    })?,
                    max: max.ok_or_else(|| {
                        minicbor::decode::Error::message("HeatmapScale.logarithmic missing max")
                    })?,
                    base: base.ok_or_else(|| {
                        minicbor::decode::Error::message("HeatmapScale.logarithmic missing base")
                    })?,
                })
            }
            "categorical" => {
                if min.is_some()
                    || max.is_some()
                    || color_from.is_some()
                    || color_to.is_some()
                    || base.is_some()
                {
                    return Err(minicbor::decode::Error::message(
                        "HeatmapScale.categorical must only carry buckets",
                    ));
                }
                Ok(HeatmapScale::Categorical {
                    buckets: buckets.ok_or_else(|| {
                        minicbor::decode::Error::message("HeatmapScale.categorical missing buckets")
                    })?,
                })
            }
            other => Err(minicbor::decode::Error::message(format!(
                "unknown HeatmapScale.kind: {other}"
            ))),
        }
    }
}

// -----------------------------------------------------------------------------
// DatePresetResolve + DatePreset
// -----------------------------------------------------------------------------

/// How a `DatePreset` resolves to an actual date (catalog §1.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatePresetResolve {
    Today,
    Yesterday,
    Last7Days,
    Last30Days,
    ThisMonth,
    LastMonth,
    Custom { offset_days: i32 },
}

impl<C> Encode<C> for DatePresetResolve {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        match self {
            DatePresetResolve::Today => {
                e.map(1)?;
                e.str("kind")?.str("today")?;
            }
            DatePresetResolve::Yesterday => {
                e.map(1)?;
                e.str("kind")?.str("yesterday")?;
            }
            DatePresetResolve::Last7Days => {
                e.map(1)?;
                e.str("kind")?.str("last_7_days")?;
            }
            DatePresetResolve::Last30Days => {
                e.map(1)?;
                e.str("kind")?.str("last_30_days")?;
            }
            DatePresetResolve::ThisMonth => {
                e.map(1)?;
                e.str("kind")?.str("this_month")?;
            }
            DatePresetResolve::LastMonth => {
                e.map(1)?;
                e.str("kind")?.str("last_month")?;
            }
            DatePresetResolve::Custom { offset_days } => {
                // Keys: kind(0x64..), offset_days(0x6b..). Sort: kind < offset_days.
                e.map(2)?;
                e.str("kind")?.str("custom")?;
                e.str("offset_days")?.i32(*offset_days)?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for DatePresetResolve {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut offset_days: Option<i32> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    assert_no_dup_tstr(&kind, "DatePresetResolve", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "offset_days" => {
                    assert_no_dup_tstr(&offset_days, "DatePresetResolve", "offset_days")?;
                    offset_days = Some(d.i32()?);
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown DatePresetResolve key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("DatePresetResolve missing kind"))?;
        let no_offset = |has: bool, k: &str| -> Result<(), minicbor::decode::Error> {
            if has {
                return Err(minicbor::decode::Error::message(format!(
                    "DatePresetResolve.{k} must not carry offset_days"
                )));
            }
            Ok(())
        };
        match kind.as_str() {
            "today" => {
                no_offset(offset_days.is_some(), "today")?;
                Ok(DatePresetResolve::Today)
            }
            "yesterday" => {
                no_offset(offset_days.is_some(), "yesterday")?;
                Ok(DatePresetResolve::Yesterday)
            }
            "last_7_days" => {
                no_offset(offset_days.is_some(), "last_7_days")?;
                Ok(DatePresetResolve::Last7Days)
            }
            "last_30_days" => {
                no_offset(offset_days.is_some(), "last_30_days")?;
                Ok(DatePresetResolve::Last30Days)
            }
            "this_month" => {
                no_offset(offset_days.is_some(), "this_month")?;
                Ok(DatePresetResolve::ThisMonth)
            }
            "last_month" => {
                no_offset(offset_days.is_some(), "last_month")?;
                Ok(DatePresetResolve::LastMonth)
            }
            "custom" => Ok(DatePresetResolve::Custom {
                offset_days: offset_days.ok_or_else(|| {
                    minicbor::decode::Error::message("DatePresetResolve.custom missing offset_days")
                })?,
            }),
            other => Err(minicbor::decode::Error::message(format!(
                "unknown DatePresetResolve.kind: {other}"
            ))),
        }
    }
}

/// Preset entry for a Date picker.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct DatePreset {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub label: BindRef,
    #[n(2)]
    pub resolve: DatePresetResolve,
}

#[cfg(test)]
mod tests_chunk_1_7b {
    use super::*;
    use crate::protocol::ui::component::{Component, FieldMap};
    use crate::protocol::ui::tokens::{
        ChartAxisScale, ChartLegendAlign, ChartLegendPosition, ChartSeriesStyle, ColumnRender,
        FileUploadStatus, FlexAlign, FlexJustify, SortDirection, Spacing, TextAlign,
    };

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
    fn split_size_decode_value_before_kind() {
        // Wire form with `value` BEFORE `kind` (non-canonical order on input,
        // host validator handles canonical wire enforcement separately).
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(2)
            .unwrap()
            .str("value")
            .unwrap()
            .f64(42.5)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("percent")
            .unwrap();
        let v: SplitSize = minicbor::decode(&buf).unwrap();
        assert_eq!(v, SplitSize::Percent { value: 42.5 });
    }

    #[test]
    fn split_size_decode_px_value_before_kind() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(2)
            .unwrap()
            .str("value")
            .unwrap()
            .u32(120)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("px")
            .unwrap();
        let v: SplitSize = minicbor::decode(&buf).unwrap();
        assert_eq!(v, SplitSize::Px { value: 120 });
    }

    fn empty_comp() -> Component {
        Component {
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

    #[test]
    fn dimension_token_all_variants_roundtrip() {
        rt(DimensionToken::Auto);
        rt(DimensionToken::Full);
        rt(DimensionToken::FitContent);
        rt(DimensionToken::Px { value: 42 });
        rt(DimensionToken::Vh { value: 80 });
        rt(DimensionToken::Vw { value: 50 });
        rt(DimensionToken::Fr { value: 2 });
        rt(DimensionToken::Percent { value: 75 });
        rt(DimensionToken::Spacing { value: Spacing::Md });
    }

    #[test]
    fn dimension_token_decode_value_before_kind() {
        // Non-canonical key order on input (`value` before `kind`) must decode
        // for both the numeric and the spacing-token variants.
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(2)
            .unwrap()
            .str("value")
            .unwrap()
            .str("md")
            .unwrap()
            .str("kind")
            .unwrap()
            .str("spacing")
            .unwrap();
        let v: DimensionToken = minicbor::decode(&buf).unwrap();
        assert_eq!(v, DimensionToken::Spacing { value: Spacing::Md });

        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(2)
            .unwrap()
            .str("value")
            .unwrap()
            .u32(320)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("px")
            .unwrap();
        let v: DimensionToken = minicbor::decode(&buf).unwrap();
        assert_eq!(v, DimensionToken::Px { value: 320 });
    }

    #[test]
    fn dimension_token_spacing_unknown_token_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(2)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("spacing")
            .unwrap()
            .str("value")
            .unwrap()
            .str("gigantic")
            .unwrap();
        let res: Result<DimensionToken, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn dimension_token_auto_with_value_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(2).unwrap();
        enc.str("kind").unwrap().str("auto").unwrap();
        enc.str("value").unwrap().u32(1).unwrap();
        let res: Result<DimensionToken, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn aspect_ratio_all_variants_roundtrip() {
        for r in [
            AspectRatio::R1To1,
            AspectRatio::R16To9,
            AspectRatio::R4To3,
            AspectRatio::R21To9,
            AspectRatio::R3To2,
            AspectRatio::R2To1,
            AspectRatio::R9To16,
            AspectRatio::R3To4,
        ] {
            rt(r);
        }
        rt(AspectRatio::Custom { ratio: 1.618 });
    }

    #[test]
    fn aspect_ratio_unit_with_ratio_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(2).unwrap();
        enc.str("kind").unwrap().str("1:1").unwrap();
        enc.str("ratio").unwrap().f32(2.0).unwrap();
        let res: Result<AspectRatio, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn table_column_width_variants_roundtrip() {
        rt(TableColumnWidth::Auto);
        rt(TableColumnWidth::MinContent);
        rt(TableColumnWidth::MaxContent);
        rt(TableColumnWidth::Px { value: 200 });
        rt(TableColumnWidth::Fr { value: 1 });
    }

    #[test]
    fn heatmap_scale_variants_roundtrip() {
        rt(HeatmapScale::Linear {
            min: 0.0,
            max: 100.0,
            color_from: Tone::Info,
            color_to: Tone::Critical,
        });
        rt(HeatmapScale::Logarithmic {
            min: 1.0,
            max: 1000.0,
            base: 10.0,
        });
        rt(HeatmapScale::Categorical {
            buckets: vec![HeatmapBucket {
                threshold: 50.0,
                tone: Tone::Warning,
                label: None,
            }],
        });
    }

    #[test]
    fn date_preset_resolve_variants_roundtrip() {
        for r in [
            DatePresetResolve::Today,
            DatePresetResolve::Yesterday,
            DatePresetResolve::Last7Days,
            DatePresetResolve::Last30Days,
            DatePresetResolve::ThisMonth,
            DatePresetResolve::LastMonth,
        ] {
            rt(r);
        }
        rt(DatePresetResolve::Custom { offset_days: -7 });
    }

    #[test]
    fn simple_struct_smoke_roundtrips() {
        use crate::protocol::ui::bind::PathSegment;
        use crate::protocol::value::Value;

        rt(RadioOption {
            value: SelectValue::Text("a".into()),
            label: BindRef::Literal(Value::Text("A".into())),
            hint: None,
            disabled: false,
        });
        rt(SliderMark {
            value: 50.0,
            label: None,
        });
        rt(KvItem {
            label: BindRef::Literal(Value::Text("L".into())),
            value: BindRef::Literal(Value::U64(1)),
            hint: None,
            icon: None,
            action_id: None,
            format: None,
        });
        rt(StepDef {
            id: "s1".into(),
            label: BindRef::Literal(Value::Text("Step".into())),
            optional: false,
            status: None,
            description: None,
        });
        rt(AccordionItem {
            id: "a".into(),
            header: empty_comp(),
            body: vec![empty_comp()],
            default_expanded: true,
        });
        rt(InboxItem {
            id: "i".into(),
            ts_ms: 1,
            read: false,
            title: BindRef::Literal(Value::Text("T".into())),
            preview: None,
            avatar: None,
            badge: None,
            action_id: "open".into(),
        });
        rt(MapMarker {
            id: "m".into(),
            lat: 52.0,
            lng: 21.0,
            icon: None,
            label: None,
            tone: None,
            popup_content: None,
        });
        rt(TableColumn {
            id: "c1".into(),
            header: BindRef::Literal(Value::Text("Name".into())),
            field_path: vec![PathSegment::Key("name".into())],
            width: TableColumnWidth::Auto,
            render: ColumnRender::Text,
            format: None,
            align: Some(TextAlign::Start),
            sortable: true,
            hidden_by_default: false,
            sticky_left: true,
        });
        rt(ChartSeries {
            id: "s".into(),
            name: BindRef::Literal(Value::Text("X".into())),
            data_path: StatePath::default(),
            tone: None,
            style: ChartSeriesStyle::Solid,
            show_in_legend: true,
        });
        rt(ChartAxis {
            label: None,
            format: None,
            min: None,
            max: None,
            ticks: None,
            scale: ChartAxisScale::Linear,
        });
        rt(ChartLegend {
            position: ChartLegendPosition::Bottom,
            alignment: ChartLegendAlign::Center,
        });
        rt(ChartTooltip {
            enabled: true,
            format: None,
        });
        rt(GridChild {
            component: empty_comp(),
            col_span: 1,
            row_span: 1,
            col_start: None,
            row_start: None,
            align_self: Some(FlexAlign::Center),
            justify_self: Some(FlexJustify::SpaceBetween),
        });
        rt(FileMeta {
            id: "f".into(),
            name: "x.txt".into(),
            size_bytes: 100,
            mime: "text/plain".into(),
            ts_ms: 0,
            upload_progress: 0.5,
            status: FileUploadStatus::Uploading,
            signed_url_ref: None,
            error_message: None,
        });
        rt(DatePreset {
            id: "today".into(),
            label: BindRef::Literal(Value::Text("Today".into())),
            resolve: DatePresetResolve::Today,
        });
        rt(RangePreset {
            id: "wk".into(),
            label: BindRef::Literal(Value::Text("Week".into())),
            range: RangePresetRange {
                from_offset_days: -7,
                to_offset_days: 0,
            },
        });
        rt(TableSort {
            column_id: "name".into(),
            direction: SortDirection::Asc,
        });
    }
}

#[cfg(test)]
mod tests_accordion_nontrivial {
    use super::*;
    use crate::protocol::ui::a11y::{Accessibility, EventKind};
    use crate::protocol::ui::bind::{BindSpec, PathSegment, StatePath};
    use crate::protocol::ui::component::{Component, FieldMap, HandlerMap, TestId};
    use crate::protocol::ui::handler::{Handler, LocalAction};
    use crate::protocol::value::Value;

    #[test]
    fn accordion_item_with_populated_component_roundtrip() {
        let header = Component {
            tag: 0x0002, // SectionHeader
            id: "h1".into(),
            fields: FieldMap(vec![(0, Value::Text("Settings".into()))]),
            handlers: Some(HandlerMap(vec![(
                EventKind::Click,
                Handler::Local(LocalAction::Focus {
                    component_id: "input1".into(),
                }),
            )])),
            bind: Some(BindSpec::Show {
                path: StatePath::new(vec![PathSegment::Key("expanded".into())]),
                negate: false,
            }),
            a11y: Some(Accessibility {
                role: Some("region".into()),
                ..Default::default()
            }),
            visibility: None,
            test_id: Some(TestId::new("section-1").unwrap()),
        };
        let body_child = Component {
            tag: 0x0203, // Paragraph
            id: "b1".into(),
            fields: FieldMap(vec![(0, Value::Text("body line".into()))]),
            handlers: None,
            bind: None,
            a11y: None,
            visibility: None,
            test_id: None,
        };
        let item = AccordionItem {
            id: "acc1".into(),
            header,
            body: vec![body_child],
            default_expanded: true,
        };
        let mut b1 = Vec::new();
        minicbor::encode(&item, &mut b1).unwrap();
        let d: AccordionItem = minicbor::decode(&b1).unwrap();
        assert_eq!(d, item);
        let mut b2 = Vec::new();
        minicbor::encode(&d, &mut b2).unwrap();
        assert_eq!(b1, b2);
    }
}

// -----------------------------------------------------------------------------
// InlineChip — non-interactive Chip form used in fields like Header.meta_chips
// -----------------------------------------------------------------------------

/// Inline Chip structure used as a field in other inline struct types
/// (e.g. `Header.meta_chips`). Pure data — NO tag/id/handlers. For an
/// interactive Chip with click/remove handlers use the Component-form
/// (tag `0x020B`).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct InlineChip {
    #[n(0)]
    pub variant: super::tokens::ChipVariant,
    #[n(1)]
    pub tone: Tone,
    #[n(2)]
    pub label: BindRef,
    #[n(3)]
    pub icon: Option<IconRef>,
    #[n(4)]
    pub avatar: Option<AvatarRef>,
    #[n(5)]
    pub selected: Option<BindRef>,
    #[n(6)]
    pub removable: bool,
}

// -----------------------------------------------------------------------------
// BorderToken — discriminated union (catalog §1.5).
// -----------------------------------------------------------------------------

/// Border style token (catalog §1.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderToken {
    None,
    Hairline,
    Thin,
    Strong,
    Accent { tone: Tone },
}

impl<C> Encode<C> for BorderToken {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        match self {
            BorderToken::None => {
                e.map(1)?;
                e.str("kind")?.str("none")?;
            }
            BorderToken::Hairline => {
                e.map(1)?;
                e.str("kind")?.str("hairline")?;
            }
            BorderToken::Thin => {
                e.map(1)?;
                e.str("kind")?.str("thin")?;
            }
            BorderToken::Strong => {
                e.map(1)?;
                e.str("kind")?.str("strong")?;
            }
            BorderToken::Accent { tone } => {
                e.map(2)?;
                e.str("kind")?.str("accent")?;
                e.str("tone")?;
                tone.encode(e, ctx)?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for BorderToken {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut tone: Option<Tone> = None;
        let mut seen_kind = false;
        let mut seen_tone = false;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    if seen_kind {
                        return Err(minicbor::decode::Error::message(
                            "BorderToken: duplicate `kind` key",
                        ));
                    }
                    seen_kind = true;
                    kind = Some(d.str()?.to_string());
                }
                "tone" => {
                    if seen_tone {
                        return Err(minicbor::decode::Error::message(
                            "BorderToken: duplicate `tone` key",
                        ));
                    }
                    seen_tone = true;
                    tone = Some(Tone::decode(d, ctx)?);
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown BorderToken key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("BorderToken missing kind"))?;
        let no_tone = |has: bool, k: &str| -> Result<(), minicbor::decode::Error> {
            if has {
                return Err(minicbor::decode::Error::message(format!(
                    "BorderToken.{k} must not carry tone"
                )));
            }
            Ok(())
        };
        match kind.as_str() {
            "none" => {
                no_tone(tone.is_some(), "none")?;
                Ok(BorderToken::None)
            }
            "hairline" => {
                no_tone(tone.is_some(), "hairline")?;
                Ok(BorderToken::Hairline)
            }
            "thin" => {
                no_tone(tone.is_some(), "thin")?;
                Ok(BorderToken::Thin)
            }
            "strong" => {
                no_tone(tone.is_some(), "strong")?;
                Ok(BorderToken::Strong)
            }
            "accent" => Ok(BorderToken::Accent {
                tone: tone.ok_or_else(|| {
                    minicbor::decode::Error::message("BorderToken.accent missing tone")
                })?,
            }),
            other => Err(minicbor::decode::Error::message(format!(
                "unknown BorderToken.kind: {other}"
            ))),
        }
    }
}

// -----------------------------------------------------------------------------
// SplitSize — discriminated union (catalog §3 0x0105 Split).
// -----------------------------------------------------------------------------

/// Primary-pane size for `Split` layout (catalog §3 0x0105).
///
/// `Percent.value` MUST be finite (no NaN/Inf) and within `0.0..=100.0`.
/// Both encode and decode enforce this.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SplitSize {
    Auto,
    Px { value: u32 },
    Percent { value: f64 },
}

fn validate_split_percent(value: f64) -> Result<(), &'static str> {
    if !value.is_finite() {
        return Err("SplitSize.percent value must be finite (no NaN/Inf)");
    }
    if !(0.0..=100.0).contains(&value) {
        return Err("SplitSize.percent value out of range 0.0..=100.0");
    }
    Ok(())
}

impl<C> Encode<C> for SplitSize {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        match self {
            SplitSize::Auto => {
                e.map(1)?;
                e.str("kind")?.str("auto")?;
            }
            SplitSize::Px { value } => {
                e.map(2)?;
                e.str("kind")?.str("px")?;
                e.str("value")?.u32(*value)?;
            }
            SplitSize::Percent { value } => {
                validate_split_percent(*value).map_err(minicbor::encode::Error::message)?;
                e.map(2)?;
                e.str("kind")?.str("percent")?;
                e.str("value")?.f64(*value)?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for SplitSize {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        // Two-pass approach: buffer `value` as a generic `Value` regardless of
        // key order, then resolve to u32/f64 in the variant match below using
        // `kind`. This makes the decoder independent of `kind` vs `value`
        // ordering on the wire.
        let mut kind: Option<String> = None;
        let mut value: Option<Value> = None;
        let mut seen_kind = false;
        let mut seen_value = false;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    if seen_kind {
                        return Err(minicbor::decode::Error::message(
                            "SplitSize: duplicate `kind` key",
                        ));
                    }
                    seen_kind = true;
                    kind = Some(d.str()?.to_string());
                }
                "value" => {
                    if seen_value {
                        return Err(minicbor::decode::Error::message(
                            "SplitSize: duplicate `value` key",
                        ));
                    }
                    seen_value = true;
                    value = Some(Value::decode(d, ctx)?);
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown SplitSize key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("SplitSize missing kind"))?;
        match kind.as_str() {
            "auto" => {
                if value.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "SplitSize.auto must not carry value",
                    ));
                }
                Ok(SplitSize::Auto)
            }
            "px" => {
                let v = value.ok_or_else(|| {
                    minicbor::decode::Error::message("SplitSize.px missing value")
                })?;
                let n: u32 = match v {
                    Value::U64(n) => u32::try_from(n).map_err(|_| {
                        minicbor::decode::Error::message("SplitSize.px value out of u32 range")
                    })?,
                    _ => {
                        return Err(minicbor::decode::Error::message(
                            "SplitSize.px value must be a non-negative integer",
                        ))
                    }
                };
                Ok(SplitSize::Px { value: n })
            }
            "percent" => {
                let v = value.ok_or_else(|| {
                    minicbor::decode::Error::message("SplitSize.percent missing value")
                })?;
                // Strict: percent value MUST be CBOR float (RFC 8949 major
                // type 7 float). Integer encodings are rejected — the SDK
                // contract is `Percent { value: f64 }` and accepting integers
                // would silently widen on encode/decode round-trip.
                let f: f64 = match v {
                    Value::F64(f) => f,
                    _ => {
                        return Err(minicbor::decode::Error::message(
                            "SplitSize.percent value must be a CBOR float (f64)",
                        ))
                    }
                };
                validate_split_percent(f).map_err(minicbor::decode::Error::message)?;
                Ok(SplitSize::Percent { value: f })
            }
            other => Err(minicbor::decode::Error::message(format!(
                "unknown SplitSize.kind: {other}"
            ))),
        }
    }
}

// -----------------------------------------------------------------------------
// GridCol — discriminated union (catalog §3 0x0102 Grid).
// -----------------------------------------------------------------------------

/// Single-column track sizing (catalog §3 0x0102 Grid GridCol).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridCol {
    Auto,
    Fill,
    MinContent,
    MaxContent,
    Fr { value: u8 },
    Px { value: u32 },
}

impl<C> Encode<C> for GridCol {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        match self {
            GridCol::Auto => {
                e.map(1)?;
                e.str("kind")?.str("auto")?;
            }
            GridCol::Fill => {
                e.map(1)?;
                e.str("kind")?.str("fill")?;
            }
            GridCol::MinContent => {
                e.map(1)?;
                e.str("kind")?.str("min_content")?;
            }
            GridCol::MaxContent => {
                e.map(1)?;
                e.str("kind")?.str("max_content")?;
            }
            GridCol::Fr { value } => {
                e.map(2)?;
                e.str("kind")?.str("fr")?;
                e.str("value")?.u8(*value)?;
            }
            GridCol::Px { value } => {
                e.map(2)?;
                e.str("kind")?.str("px")?;
                e.str("value")?.u32(*value)?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for GridCol {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut value: Option<u64> = None;
        let mut seen_kind = false;
        let mut seen_value = false;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    if seen_kind {
                        return Err(minicbor::decode::Error::message(
                            "GridCol: duplicate `kind` key",
                        ));
                    }
                    seen_kind = true;
                    kind = Some(d.str()?.to_string());
                }
                "value" => {
                    if seen_value {
                        return Err(minicbor::decode::Error::message(
                            "GridCol: duplicate `value` key",
                        ));
                    }
                    seen_value = true;
                    value = Some(d.u64()?);
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown GridCol key: {other}"
                    )))
                }
            }
        }
        let kind = kind.ok_or_else(|| minicbor::decode::Error::message("GridCol missing kind"))?;
        let no_val = |has: bool, k: &str| -> Result<(), minicbor::decode::Error> {
            if has {
                return Err(minicbor::decode::Error::message(format!(
                    "GridCol.{k} must not carry value"
                )));
            }
            Ok(())
        };
        match kind.as_str() {
            "auto" => {
                no_val(value.is_some(), "auto")?;
                Ok(GridCol::Auto)
            }
            "fill" => {
                no_val(value.is_some(), "fill")?;
                Ok(GridCol::Fill)
            }
            "min_content" => {
                no_val(value.is_some(), "min_content")?;
                Ok(GridCol::MinContent)
            }
            "max_content" => {
                no_val(value.is_some(), "max_content")?;
                Ok(GridCol::MaxContent)
            }
            "fr" => Ok(GridCol::Fr {
                value: value
                    .ok_or_else(|| minicbor::decode::Error::message("GridCol.fr missing value"))?
                    .try_into()
                    .map_err(|_| {
                        minicbor::decode::Error::message("GridCol.fr value out of u8 range")
                    })?,
            }),
            "px" => Ok(GridCol::Px {
                value: value
                    .ok_or_else(|| minicbor::decode::Error::message("GridCol.px missing value"))?
                    .try_into()
                    .map_err(|_| {
                        minicbor::decode::Error::message("GridCol.px value out of u32 range")
                    })?,
            }),
            other => Err(minicbor::decode::Error::message(format!(
                "unknown GridCol.kind: {other}"
            ))),
        }
    }
}

// -----------------------------------------------------------------------------
// GridTrack — discriminated union (catalog §3 0x0102 Grid).
// -----------------------------------------------------------------------------

/// Column-track spec for `Grid` (catalog §3 0x0102).
#[derive(Debug, Clone, PartialEq)]
pub enum GridTrack {
    Equal { count: u8 },
    Explicit { cols: Vec<GridCol> },
}

impl<C> Encode<C> for GridTrack {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        match self {
            GridTrack::Equal { count } => {
                // Keys: "count"(0x65..), "kind"(0x64..). Sort: kind < count.
                e.map(2)?;
                e.str("kind")?.str("equal")?;
                e.str("count")?.u8(*count)?;
            }
            GridTrack::Explicit { cols } => {
                // Keys: "cols"(0x64..), "kind"(0x64..).
                //   "cols" = 0x64 63.., "kind" = 0x64 6b.. → cols < kind.
                e.map(2)?;
                e.str("cols")?;
                e.array(cols.len() as u64)?;
                for col in cols {
                    col.encode(e, ctx)?;
                }
                e.str("kind")?.str("explicit")?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for GridTrack {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut count: Option<u8> = None;
        let mut cols: Option<Vec<GridCol>> = None;
        let mut seen_kind = false;
        let mut seen_count = false;
        let mut seen_cols = false;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    if seen_kind {
                        return Err(minicbor::decode::Error::message(
                            "GridTrack: duplicate `kind` key",
                        ));
                    }
                    seen_kind = true;
                    kind = Some(d.str()?.to_string());
                }
                "count" => {
                    if seen_count {
                        return Err(minicbor::decode::Error::message(
                            "GridTrack: duplicate `count` key",
                        ));
                    }
                    seen_count = true;
                    count = Some(d.u8()?);
                }
                "cols" => {
                    if seen_cols {
                        return Err(minicbor::decode::Error::message(
                            "GridTrack: duplicate `cols` key",
                        ));
                    }
                    seen_cols = true;
                    let n = d.array()?.ok_or_else(|| {
                        minicbor::decode::Error::message("indefinite-length array forbidden")
                    })?;
                    let mut v = Vec::with_capacity(n as usize);
                    for _ in 0..n {
                        v.push(GridCol::decode(d, ctx)?);
                    }
                    cols = Some(v);
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown GridTrack key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("GridTrack missing kind"))?;
        match kind.as_str() {
            "equal" => {
                if cols.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "GridTrack.equal must not carry cols",
                    ));
                }
                Ok(GridTrack::Equal {
                    count: count.ok_or_else(|| {
                        minicbor::decode::Error::message("GridTrack.equal missing count")
                    })?,
                })
            }
            "explicit" => {
                if count.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "GridTrack.explicit must not carry count",
                    ));
                }
                Ok(GridTrack::Explicit {
                    cols: cols.ok_or_else(|| {
                        minicbor::decode::Error::message("GridTrack.explicit missing cols")
                    })?,
                })
            }
            other => Err(minicbor::decode::Error::message(format!(
                "unknown GridTrack.kind: {other}"
            ))),
        }
    }
}

// -----------------------------------------------------------------------------
// LogEvent — inline struct for VirtualizedLog (catalog §8 0x0611).
// -----------------------------------------------------------------------------

// -----------------------------------------------------------------------------
// SpaceValue / RadiusValue — discriminated unions (catalog §1.5 BoxStyle).
// -----------------------------------------------------------------------------

/// Spacing value for `BoxStyle` margins/paddings: semantic `Spacing` token or
/// a raw pixel count. Wire: `{kind:"token", value: Spacing}` /
/// `{kind:"px", value: u16}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceValue {
    Token { value: super::tokens::Spacing },
    Px { value: u16 },
}

impl<C> Encode<C> for SpaceValue {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Canonical key order: kind(0x64..) < value(0x65..).
        e.map(2)?;
        match self {
            SpaceValue::Token { value } => {
                e.str("kind")?.str("token")?;
                e.str("value")?;
                value.encode(e, ctx)?;
            }
            SpaceValue::Px { value } => {
                e.str("kind")?.str("px")?;
                e.str("value")?.u16(*value)?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for SpaceValue {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        // Two-pass: buffer `value` as generic `Value` so decode stays
        // independent of `kind` vs `value` ordering on the wire.
        let mut kind: Option<String> = None;
        let mut value: Option<Value> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    assert_no_dup_tstr(&kind, "SpaceValue", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "value" => {
                    assert_no_dup_tstr(&value, "SpaceValue", "value")?;
                    value = Some(Value::decode(d, ctx)?);
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown SpaceValue key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("SpaceValue missing kind"))?;
        let value =
            value.ok_or_else(|| minicbor::decode::Error::message("SpaceValue missing value"))?;
        match (kind.as_str(), value) {
            ("token", Value::Text(s)) => Ok(SpaceValue::Token {
                value: super::tokens::Spacing::from_wire(&s).ok_or_else(|| {
                    minicbor::decode::Error::message("SpaceValue.token: unknown Spacing token")
                })?,
            }),
            ("px", Value::U64(n)) => Ok(SpaceValue::Px {
                value: u16::try_from(n).map_err(|_| {
                    minicbor::decode::Error::message("SpaceValue.px value out of u16 range")
                })?,
            }),
            (other, _) => Err(minicbor::decode::Error::message(format!(
                "SpaceValue.kind '{other}' does not match value type"
            ))),
        }
    }
}

impl From<super::tokens::Spacing> for SpaceValue {
    fn from(value: super::tokens::Spacing) -> Self {
        SpaceValue::Token { value }
    }
}

impl From<u16> for SpaceValue {
    fn from(value: u16) -> Self {
        SpaceValue::Px { value }
    }
}

/// Corner radius value for `BoxStyle`: semantic `RadiusToken` or raw pixels.
/// Wire: `{kind:"token", value: RadiusToken}` / `{kind:"px", value: u16}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadiusValue {
    Token { value: super::tokens::RadiusToken },
    Px { value: u16 },
}

impl<C> Encode<C> for RadiusValue {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Canonical key order: kind(0x64..) < value(0x65..).
        e.map(2)?;
        match self {
            RadiusValue::Token { value } => {
                e.str("kind")?.str("token")?;
                e.str("value")?;
                value.encode(e, ctx)?;
            }
            RadiusValue::Px { value } => {
                e.str("kind")?.str("px")?;
                e.str("value")?.u16(*value)?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for RadiusValue {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut value: Option<Value> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    assert_no_dup_tstr(&kind, "RadiusValue", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "value" => {
                    assert_no_dup_tstr(&value, "RadiusValue", "value")?;
                    value = Some(Value::decode(d, ctx)?);
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown RadiusValue key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("RadiusValue missing kind"))?;
        let value =
            value.ok_or_else(|| minicbor::decode::Error::message("RadiusValue missing value"))?;
        match (kind.as_str(), value) {
            ("token", Value::Text(s)) => Ok(RadiusValue::Token {
                value: super::tokens::RadiusToken::from_wire(&s).ok_or_else(|| {
                    minicbor::decode::Error::message("RadiusValue.token: unknown RadiusToken")
                })?,
            }),
            ("px", Value::U64(n)) => Ok(RadiusValue::Px {
                value: u16::try_from(n).map_err(|_| {
                    minicbor::decode::Error::message("RadiusValue.px value out of u16 range")
                })?,
            }),
            (other, _) => Err(minicbor::decode::Error::message(format!(
                "RadiusValue.kind '{other}' does not match value type"
            ))),
        }
    }
}

impl From<super::tokens::RadiusToken> for RadiusValue {
    fn from(value: super::tokens::RadiusToken) -> Self {
        RadiusValue::Token { value }
    }
}

impl From<u16> for RadiusValue {
    fn from(value: u16) -> Self {
        RadiusValue::Px { value }
    }
}

/// Container-width threshold for a `ResponsiveRule`: a semantic `Breakpoint`
/// token or a raw pixel width measured against the CONTAINER (not the viewport).
/// Wire: `{kind:"token", value: Breakpoint}` / `{kind:"px", value: u16}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerWidth {
    Token(super::tokens::Breakpoint),
    Px(u16),
}

impl<C> Encode<C> for ContainerWidth {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Canonical key order: kind(0x64..) < value(0x65..).
        e.map(2)?;
        match self {
            ContainerWidth::Token(value) => {
                e.str("kind")?.str("token")?;
                e.str("value")?;
                value.encode(e, ctx)?;
            }
            ContainerWidth::Px(value) => {
                e.str("kind")?.str("px")?;
                e.str("value")?.u16(*value)?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for ContainerWidth {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut value: Option<Value> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    assert_no_dup_tstr(&kind, "ContainerWidth", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "value" => {
                    assert_no_dup_tstr(&value, "ContainerWidth", "value")?;
                    value = Some(Value::decode(d, ctx)?);
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown ContainerWidth key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("ContainerWidth missing kind"))?;
        let value = value
            .ok_or_else(|| minicbor::decode::Error::message("ContainerWidth missing value"))?;
        match (kind.as_str(), value) {
            ("token", Value::Text(s)) => Ok(ContainerWidth::Token(
                super::tokens::Breakpoint::from_wire(&s).ok_or_else(|| {
                    minicbor::decode::Error::message("ContainerWidth.token: unknown Breakpoint")
                })?,
            )),
            ("px", Value::U64(n)) => Ok(ContainerWidth::Px(u16::try_from(n).map_err(|_| {
                minicbor::decode::Error::message("ContainerWidth.px value out of u16 range")
            })?)),
            (other, _) => Err(minicbor::decode::Error::message(format!(
                "ContainerWidth.kind '{other}' does not match value type"
            ))),
        }
    }
}

impl From<super::tokens::Breakpoint> for ContainerWidth {
    fn from(value: super::tokens::Breakpoint) -> Self {
        ContainerWidth::Token(value)
    }
}

impl From<u16> for ContainerWidth {
    fn from(value: u16) -> Self {
        ContainerWidth::Px(value)
    }
}

// -----------------------------------------------------------------------------
// BoxStyle — generic container styling (catalog §1.5).
// -----------------------------------------------------------------------------

/// One border edge: width in px + semantic color token + line style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct BorderSide {
    #[n(0)]
    pub width_px: u8,
    #[n(1)]
    pub color: super::tokens::BorderColor,
    #[n(2)]
    pub style: super::tokens::BorderLineStyle,
}

impl BorderSide {
    pub fn new(width_px: u8, color: super::tokens::BorderColor) -> Self {
        Self {
            width_px,
            color,
            style: super::tokens::BorderLineStyle::Solid,
        }
    }
}

/// Per-edge spacing values (margin / padding). Absent edge = renderer leaves
/// the container default untouched. `all`/`x`/`y` shorthands are resolved by
/// SDK builders into explicit edges — the wire carries edges only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct EdgeValues {
    #[n(0)]
    pub top: Option<SpaceValue>,
    #[n(1)]
    pub right: Option<SpaceValue>,
    #[n(2)]
    pub bottom: Option<SpaceValue>,
    #[n(3)]
    pub left: Option<SpaceValue>,
}

impl EdgeValues {
    pub fn all(v: impl Into<SpaceValue>) -> Self {
        let v = v.into();
        Self {
            top: Some(v),
            right: Some(v),
            bottom: Some(v),
            left: Some(v),
        }
    }

    pub fn x(v: impl Into<SpaceValue>) -> Self {
        let v = v.into();
        Self {
            left: Some(v),
            right: Some(v),
            ..Self::default()
        }
    }

    pub fn y(v: impl Into<SpaceValue>) -> Self {
        let v = v.into();
        Self {
            top: Some(v),
            bottom: Some(v),
            ..Self::default()
        }
    }
}

/// Per-edge border sides. Absent edge = no border on that edge.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct BorderEdges {
    #[n(0)]
    pub top: Option<BorderSide>,
    #[n(1)]
    pub right: Option<BorderSide>,
    #[n(2)]
    pub bottom: Option<BorderSide>,
    #[n(3)]
    pub left: Option<BorderSide>,
}

impl BorderEdges {
    pub fn all(side: BorderSide) -> Self {
        Self {
            top: Some(side),
            right: Some(side),
            bottom: Some(side),
            left: Some(side),
        }
    }
}

/// Per-corner radius values. Absent corner = container default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct CornerValues {
    #[n(0)]
    pub top_left: Option<RadiusValue>,
    #[n(1)]
    pub top_right: Option<RadiusValue>,
    #[n(2)]
    pub bottom_right: Option<RadiusValue>,
    #[n(3)]
    pub bottom_left: Option<RadiusValue>,
}

impl CornerValues {
    pub fn all(v: impl Into<RadiusValue>) -> Self {
        let v = v.into();
        Self {
            top_left: Some(v),
            top_right: Some(v),
            bottom_right: Some(v),
            bottom_left: Some(v),
        }
    }
}

/// Shared container styling (catalog §1.5): margins, paddings, borders,
/// background, radii, dimensions and overflow — HTML-like box control without
/// raw CSS. Every field optional; `None` keeps the container's own defaults.
/// Attached to layout containers via their `style` field; renderer applies it
/// on top of the container's token-level fields.
#[derive(Debug, Clone, Default, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct BoxStyle {
    #[n(0)]
    pub margin: Option<EdgeValues>,
    #[n(1)]
    pub padding: Option<EdgeValues>,
    #[n(2)]
    pub border: Option<BorderEdges>,
    #[n(3)]
    pub background: Option<super::tokens::BackgroundToken>,
    #[n(4)]
    pub radius: Option<CornerValues>,
    #[n(5)]
    pub width: Option<DimensionToken>,
    #[n(6)]
    pub height: Option<DimensionToken>,
    #[n(7)]
    pub min_width: Option<DimensionToken>,
    #[n(8)]
    pub min_height: Option<DimensionToken>,
    #[n(9)]
    pub max_width: Option<DimensionToken>,
    #[n(10)]
    pub max_height: Option<DimensionToken>,
    #[n(11)]
    pub overflow_x: Option<super::tokens::Overflow>,
    #[n(12)]
    pub overflow_y: Option<super::tokens::Overflow>,
    #[n(13)]
    pub shadow: Option<super::tokens::ShadowToken>,
}

impl BoxStyle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn margin(mut self, edges: EdgeValues) -> Self {
        self.margin = Some(edges);
        self
    }

    pub fn margin_all(self, v: impl Into<SpaceValue>) -> Self {
        self.margin(EdgeValues::all(v))
    }

    pub fn margin_x(self, v: impl Into<SpaceValue>) -> Self {
        self.margin(EdgeValues::x(v))
    }

    pub fn margin_y(self, v: impl Into<SpaceValue>) -> Self {
        self.margin(EdgeValues::y(v))
    }

    pub fn padding(mut self, edges: EdgeValues) -> Self {
        self.padding = Some(edges);
        self
    }

    pub fn padding_all(self, v: impl Into<SpaceValue>) -> Self {
        self.padding(EdgeValues::all(v))
    }

    pub fn padding_x(self, v: impl Into<SpaceValue>) -> Self {
        self.padding(EdgeValues::x(v))
    }

    pub fn padding_y(self, v: impl Into<SpaceValue>) -> Self {
        self.padding(EdgeValues::y(v))
    }

    pub fn border(mut self, width_px: u8, color: super::tokens::BorderColor) -> Self {
        self.border = Some(BorderEdges::all(BorderSide::new(width_px, color)));
        self
    }

    pub fn border_edges(mut self, edges: BorderEdges) -> Self {
        self.border = Some(edges);
        self
    }

    pub fn bg(mut self, token: super::tokens::BackgroundToken) -> Self {
        self.background = Some(token);
        self
    }

    pub fn radius(mut self, v: impl Into<RadiusValue>) -> Self {
        self.radius = Some(CornerValues::all(v));
        self
    }

    pub fn radius_corners(mut self, corners: CornerValues) -> Self {
        self.radius = Some(corners);
        self
    }

    pub fn width(mut self, v: DimensionToken) -> Self {
        self.width = Some(v);
        self
    }

    pub fn height(mut self, v: DimensionToken) -> Self {
        self.height = Some(v);
        self
    }

    pub fn min_width(mut self, v: DimensionToken) -> Self {
        self.min_width = Some(v);
        self
    }

    pub fn min_height(mut self, v: DimensionToken) -> Self {
        self.min_height = Some(v);
        self
    }

    pub fn max_width(mut self, v: DimensionToken) -> Self {
        self.max_width = Some(v);
        self
    }

    pub fn max_height(mut self, v: DimensionToken) -> Self {
        self.max_height = Some(v);
        self
    }

    pub fn overflow(mut self, v: super::tokens::Overflow) -> Self {
        self.overflow_x = Some(v);
        self.overflow_y = Some(v);
        self
    }

    pub fn overflow_x(mut self, v: super::tokens::Overflow) -> Self {
        self.overflow_x = Some(v);
        self
    }

    pub fn overflow_y(mut self, v: super::tokens::Overflow) -> Self {
        self.overflow_y = Some(v);
        self
    }

    pub fn shadow(mut self, v: super::tokens::ShadowToken) -> Self {
        self.shadow = Some(v);
        self
    }
}

/// One responsive override applied when the CONTAINER's own width is `<=
/// max_width`. Addon declares layout adaptation semantically (container-query
/// style) instead of shipping media-query CSS. Every override field is optional;
/// `None` leaves the base layout value untouched at that breakpoint. Multiple
/// rules on a container are evaluated smallest-`max_width`-first by the renderer.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct ResponsiveRule {
    #[n(0)]
    pub max_width: ContainerWidth,
    #[n(1)]
    pub direction: Option<super::tokens::FlexDirection>,
    #[n(2)]
    pub gap: Option<super::tokens::Spacing>,
    #[n(3)]
    pub align: Option<super::tokens::FlexAlign>,
    #[n(4)]
    pub justify: Option<super::tokens::FlexJustify>,
    #[n(5)]
    pub padding: Option<EdgeValues>,
    #[n(6)]
    pub min_height: Option<DimensionToken>,
    #[n(7)]
    pub order: Option<i32>,
    #[n(8)]
    pub hidden: Option<bool>,
    /// Width override below the threshold (e.g. a fixed side panel going
    /// full-width once the row stacks into a column).
    #[n(9)]
    pub width: Option<DimensionToken>,
}

impl ResponsiveRule {
    /// New rule keyed on the container width threshold; all overrides start unset.
    pub fn at(max_width: impl Into<ContainerWidth>) -> Self {
        Self {
            max_width: max_width.into(),
            direction: None,
            gap: None,
            align: None,
            justify: None,
            padding: None,
            min_height: None,
            order: None,
            hidden: None,
            width: None,
        }
    }

    pub fn direction(mut self, v: super::tokens::FlexDirection) -> Self {
        self.direction = Some(v);
        self
    }

    pub fn gap(mut self, v: super::tokens::Spacing) -> Self {
        self.gap = Some(v);
        self
    }

    pub fn align(mut self, v: super::tokens::FlexAlign) -> Self {
        self.align = Some(v);
        self
    }

    pub fn justify(mut self, v: super::tokens::FlexJustify) -> Self {
        self.justify = Some(v);
        self
    }

    pub fn padding(mut self, v: EdgeValues) -> Self {
        self.padding = Some(v);
        self
    }

    pub fn min_height(mut self, v: DimensionToken) -> Self {
        self.min_height = Some(v);
        self
    }

    pub fn order(mut self, v: i32) -> Self {
        self.order = Some(v);
        self
    }

    pub fn hidden(mut self, v: bool) -> Self {
        self.hidden = Some(v);
        self
    }

    pub fn width(mut self, v: DimensionToken) -> Self {
        self.width = Some(v);
        self
    }
}

#[cfg(test)]
mod tests_box_style {
    use super::*;
    use crate::protocol::ui::tokens::{
        BackgroundToken, BorderColor, BorderLineStyle, Breakpoint, FlexAlign, FlexDirection,
        FlexJustify, Overflow, RadiusToken, ShadowToken, Spacing,
    };

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
    fn space_value_variants_roundtrip() {
        rt(SpaceValue::Token { value: Spacing::Md });
        rt(SpaceValue::Px { value: 0 });
        rt(SpaceValue::Px { value: 65535 });
    }

    #[test]
    fn space_value_kind_type_mismatch_rejected() {
        // kind="px" but value is a string — must reject.
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(2)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("px")
            .unwrap()
            .str("value")
            .unwrap()
            .str("md")
            .unwrap();
        let res: Result<SpaceValue, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn space_value_unknown_token_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(2)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("token")
            .unwrap()
            .str("value")
            .unwrap()
            .str("gigantic")
            .unwrap();
        let res: Result<SpaceValue, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn space_value_px_out_of_u16_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(2)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("px")
            .unwrap()
            .str("value")
            .unwrap()
            .u32(70_000)
            .unwrap();
        let res: Result<SpaceValue, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn space_value_decode_value_before_kind() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(2)
            .unwrap()
            .str("value")
            .unwrap()
            .u16(12)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("px")
            .unwrap();
        let v: SpaceValue = minicbor::decode(&buf).unwrap();
        assert_eq!(v, SpaceValue::Px { value: 12 });
    }

    #[test]
    fn radius_value_variants_roundtrip() {
        rt(RadiusValue::Token {
            value: RadiusToken::Pill,
        });
        rt(RadiusValue::Px { value: 6 });
    }

    #[test]
    fn border_side_and_edges_roundtrip() {
        let side = BorderSide {
            width_px: 2,
            color: BorderColor::Accent,
            style: BorderLineStyle::Dashed,
        };
        rt(side);
        rt(BorderEdges::all(side));
        rt(BorderEdges {
            bottom: Some(side),
            ..BorderEdges::default()
        });
    }

    #[test]
    fn edge_and_corner_values_roundtrip() {
        rt(EdgeValues::all(Spacing::Lg));
        rt(EdgeValues::x(SpaceValue::Px { value: 12 }));
        rt(EdgeValues::y(Spacing::Sm));
        rt(CornerValues::all(RadiusToken::Md));
        rt(CornerValues {
            top_left: Some(RadiusValue::Px { value: 4 }),
            ..CornerValues::default()
        });
    }

    #[test]
    fn box_style_empty_and_full_roundtrip() {
        rt(BoxStyle::default());
        rt(BoxStyle::new()
            .margin_y(Spacing::Md)
            .padding_all(12u16)
            .border(1, BorderColor::Default)
            .bg(BackgroundToken::Subtle)
            .radius(RadiusToken::Lg)
            .width(DimensionToken::Full)
            .height(DimensionToken::Px { value: 240 })
            .min_width(DimensionToken::Px { value: 100 })
            .max_height(DimensionToken::Percent { value: 80 })
            .overflow_y(Overflow::Auto)
            .shadow(ShadowToken::AccentGlow));
    }

    #[test]
    fn box_style_builder_shorthands_resolve_edges() {
        let s = BoxStyle::new().margin_x(Spacing::Sm).padding_all(8u16);
        let m = s.margin.unwrap();
        assert_eq!(m.left, Some(SpaceValue::Token { value: Spacing::Sm }));
        assert_eq!(m.right, Some(SpaceValue::Token { value: Spacing::Sm }));
        assert_eq!(m.top, None);
        assert_eq!(m.bottom, None);
        let p = s.padding.unwrap();
        assert_eq!(p.top, Some(SpaceValue::Px { value: 8 }));
        assert_eq!(p.bottom, Some(SpaceValue::Px { value: 8 }));
    }

    #[test]
    fn container_width_variants_roundtrip() {
        rt(ContainerWidth::Token(Breakpoint::Md));
        rt(ContainerWidth::Px(460));
        rt(ContainerWidth::Px(680));
    }

    #[test]
    fn container_width_kind_type_mismatch_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(2)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("px")
            .unwrap()
            .str("value")
            .unwrap()
            .str("md")
            .unwrap();
        let res: Result<ContainerWidth, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn responsive_rule_full_roundtrip() {
        rt(ResponsiveRule::at(460u16)
            .direction(FlexDirection::Column)
            .gap(Spacing::Sm)
            .align(FlexAlign::Start)
            .justify(FlexJustify::SpaceBetween)
            .padding(EdgeValues::all(Spacing::Md))
            .min_height(DimensionToken::Px { value: 120 })
            .order(-1)
            .hidden(true));
    }

    #[test]
    fn responsive_rule_minimal_roundtrip() {
        rt(ResponsiveRule::at(Breakpoint::Sm));
    }
}

/// One entry in a `VirtualizedLog.events_path` stream (catalog §8 0x0611).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct LogEvent {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub ts_ms: i64,
    #[n(2)]
    pub level: super::tokens::LogLevel,
    #[n(3)]
    pub source: Option<String>,
    #[n(4)]
    pub message: BindRef,
    #[n(5)]
    pub details: Option<crate::protocol::control::CborMap>,
    #[n(6)]
    pub trace_id: Option<String>,
}

// =============================================================================
// File: protocol/ui/data/avatar.rs — Avatar/AvatarGroup (catalog §4)
// =============================================================================

use super::super::super::value::Value;
use super::super::component::{Component, FieldMap};
use super::super::inline::AvatarRef;
use super::super::tokens::{AvatarOverlap, AvatarShape, AvatarSize, AvatarStatus, Tone};
use super::super::typed_field::{
    decode_from_value, encode_to_value, ensure_no_duplicate_keys, ensure_ref_tag_decode,
    ensure_ref_tag_encode, ensure_tag, missing_field, unknown_field, IntoComponentError,
};

#[inline]
fn component(tag: u16, id: impl Into<String>, fields: Vec<(u8, Value)>) -> Component {
    Component {
        tag,
        id: id.into(),
        fields: FieldMap(fields),
        handlers: None,
        bind: None,
        a11y: None,
        visibility: None,
        test_id: None,
    }
}

// -----------------------------------------------------------------------------
// Avatar
// -----------------------------------------------------------------------------
/// User avatar (catalog §4 0x020D).
#[derive(Debug, Clone, PartialEq)]
pub struct Avatar {
    pub source: AvatarRef,
    pub size: AvatarSize,
    pub shape: AvatarShape,
    pub status: Option<AvatarStatus>,
    pub tone: Option<Tone>,
}

impl Avatar {
    pub const TAG: u16 = 0x020D;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(5);
        entries.push((0, encode_to_value(&self.source)?));
        entries.push((1, encode_to_value(&self.size)?));
        entries.push((2, encode_to_value(&self.shape)?));
        if let Some(s) = &self.status {
            entries.push((3, encode_to_value(s)?));
        }
        if let Some(t) = &self.tone {
            entries.push((4, encode_to_value(t)?));
        }
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Avatar")?;
        ensure_no_duplicate_keys("Avatar", &c.fields.0)?;
        let mut source = None;
        let mut size = None;
        let mut shape = None;
        let mut status = None;
        let mut tone = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => source = Some(decode_from_value(v)?),
                1 => size = Some(decode_from_value(v)?),
                2 => shape = Some(decode_from_value(v)?),
                3 => status = Some(decode_from_value(v)?),
                4 => tone = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Avatar", *other)),
            }
        }
        Ok(Avatar {
            source: source.ok_or_else(|| missing_field("Avatar", "source"))?,
            size: size.ok_or_else(|| missing_field("Avatar", "size"))?,
            shape: shape.ok_or_else(|| missing_field("Avatar", "shape"))?,
            status,
            tone,
        })
    }
}

// -----------------------------------------------------------------------------
// AvatarGroup
// -----------------------------------------------------------------------------
/// Stack of avatars with overflow indicator (catalog §4 0x020E).
#[derive(Debug, Clone, PartialEq)]
pub struct AvatarGroup {
    /// `ComponentRef<Avatar>` entries (tag 0x020D).
    pub avatars: Vec<Component>,
    pub max_visible: u8,
    pub overlap: AvatarOverlap,
    pub size: AvatarSize,
}

impl AvatarGroup {
    pub const TAG: u16 = 0x020E;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        for a in &self.avatars {
            ensure_ref_tag_encode(a.tag, Avatar::TAG, "AvatarGroup", "avatars")?;
        }
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(4);
        entries.push((0, encode_to_value(&self.avatars)?));
        entries.push((1, encode_to_value(&self.max_visible)?));
        entries.push((2, encode_to_value(&self.overlap)?));
        entries.push((3, encode_to_value(&self.size)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "AvatarGroup")?;
        ensure_no_duplicate_keys("AvatarGroup", &c.fields.0)?;
        let mut avatars = None;
        let mut max_visible = None;
        let mut overlap = None;
        let mut size = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => avatars = Some(decode_from_value(v)?),
                1 => max_visible = Some(decode_from_value(v)?),
                2 => overlap = Some(decode_from_value(v)?),
                3 => size = Some(decode_from_value(v)?),
                other => return Err(unknown_field("AvatarGroup", *other)),
            }
        }
        let avatars: Vec<Component> = avatars.unwrap_or_default();
        for a in &avatars {
            ensure_ref_tag_decode(a.tag, Avatar::TAG, "AvatarGroup", "avatars")?;
        }
        Ok(AvatarGroup {
            avatars,
            max_visible: max_visible.ok_or_else(|| missing_field("AvatarGroup", "max_visible"))?,
            overlap: overlap.ok_or_else(|| missing_field("AvatarGroup", "overlap"))?,
            size: size.ok_or_else(|| missing_field("AvatarGroup", "size"))?,
        })
    }
}

// =============================================================================
// File: protocol/ui/specialized/iframe.rs — IFrame (catalog §8 0x060A)
// Sandboxed embed; full security gating (https-only URL, manifest allowlist,
// sandbox token blacklist) is enforced by the host validator (Krok 4). The
// typed SDK layer reflects the on-wire schema only.
// =============================================================================

use super::super::super::value::Value;
use super::super::component::{Component, FieldMap};
use super::super::inline::DimensionToken;
use super::super::tokens::{IFrameReferrerPolicy, IFrameSandbox};
use super::super::typed_field::{
    decode_from_value, encode_to_value, ensure_no_duplicate_keys, ensure_tag, missing_field,
    unknown_field, IntoComponentError,
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

/// Sandboxed iframe (catalog §8 0x060A).
#[derive(Debug, Clone, PartialEq)]
pub struct IFrame {
    pub src: String,
    pub sandbox: Vec<IFrameSandbox>,
    pub width: DimensionToken,
    pub height: DimensionToken,
    pub title: String,
    pub referrer_policy: IFrameReferrerPolicy,
}

impl IFrame {
    pub const TAG: u16 = 0x060A;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(6);
        e.push((0, encode_to_value(&self.src)?));
        e.push((1, encode_to_value(&self.sandbox)?));
        e.push((2, encode_to_value(&self.width)?));
        e.push((3, encode_to_value(&self.height)?));
        e.push((4, encode_to_value(&self.title)?));
        e.push((5, encode_to_value(&self.referrer_policy)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "IFrame")?;
        ensure_no_duplicate_keys("IFrame", &c.fields.0)?;
        let mut src = None;
        let mut sandbox = None;
        let mut width = None;
        let mut height = None;
        let mut title = None;
        let mut referrer_policy = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => src = Some(decode_from_value(v)?),
                1 => sandbox = Some(decode_from_value(v)?),
                2 => width = Some(decode_from_value(v)?),
                3 => height = Some(decode_from_value(v)?),
                4 => title = Some(decode_from_value(v)?),
                5 => referrer_policy = Some(decode_from_value(v)?),
                other => return Err(unknown_field("IFrame", *other)),
            }
        }
        Ok(IFrame {
            src: src.ok_or_else(|| missing_field("IFrame", "src"))?,
            sandbox: sandbox.ok_or_else(|| missing_field("IFrame", "sandbox"))?,
            width: width.ok_or_else(|| missing_field("IFrame", "width"))?,
            height: height.ok_or_else(|| missing_field("IFrame", "height"))?,
            title: title.ok_or_else(|| missing_field("IFrame", "title"))?,
            referrer_policy: referrer_policy
                .ok_or_else(|| missing_field("IFrame", "referrer_policy"))?,
        })
    }
}

// =============================================================================
// File: protocol/ui/specialized/telemetry.rs — FpsCounter/Stopwatch (catalog §8)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::StatePath;
use super::super::component::{Component, FieldMap};
use super::super::tokens::{FpsVariant, StopwatchVariant, Tone};
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

// -----------------------------------------------------------------------------
// 0x060E — FpsCounter
// -----------------------------------------------------------------------------

/// Telemetry FPS overlay (catalog §8 0x060E).
#[derive(Debug, Clone, PartialEq)]
pub struct FpsCounter {
    pub source_path: StatePath,
    pub variant: FpsVariant,
    pub history_secs: u8,
}

impl FpsCounter {
    pub const TAG: u16 = 0x060E;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(3);
        e.push((0, encode_to_value(&self.source_path)?));
        e.push((1, encode_to_value(&self.variant)?));
        e.push((2, encode_to_value(&self.history_secs)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "FpsCounter")?;
        ensure_no_duplicate_keys("FpsCounter", &c.fields.0)?;
        let mut source_path = None;
        let mut variant = None;
        let mut history_secs = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => source_path = Some(decode_from_value(v)?),
                1 => variant = Some(decode_from_value(v)?),
                2 => history_secs = Some(decode_from_value(v)?),
                other => return Err(unknown_field("FpsCounter", *other)),
            }
        }
        Ok(FpsCounter {
            source_path: source_path.ok_or_else(|| missing_field("FpsCounter", "source_path"))?,
            variant: variant.ok_or_else(|| missing_field("FpsCounter", "variant"))?,
            history_secs: history_secs
                .ok_or_else(|| missing_field("FpsCounter", "history_secs"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0610 — Stopwatch
// -----------------------------------------------------------------------------

/// Live timer (catalog §8 0x0610).
#[derive(Debug, Clone, PartialEq)]
pub struct Stopwatch {
    pub started_at_path: StatePath,
    pub variant: StopwatchVariant,
    pub tone: Tone,
}

impl Stopwatch {
    pub const TAG: u16 = 0x0610;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(3);
        e.push((0, encode_to_value(&self.started_at_path)?));
        e.push((1, encode_to_value(&self.variant)?));
        e.push((2, encode_to_value(&self.tone)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Stopwatch")?;
        ensure_no_duplicate_keys("Stopwatch", &c.fields.0)?;
        let mut started_at_path = None;
        let mut variant = None;
        let mut tone = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => started_at_path = Some(decode_from_value(v)?),
                1 => variant = Some(decode_from_value(v)?),
                2 => tone = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Stopwatch", *other)),
            }
        }
        Ok(Stopwatch {
            started_at_path: started_at_path
                .ok_or_else(|| missing_field("Stopwatch", "started_at_path"))?,
            variant: variant.ok_or_else(|| missing_field("Stopwatch", "variant"))?,
            tone: tone.ok_or_else(|| missing_field("Stopwatch", "tone"))?,
        })
    }
}

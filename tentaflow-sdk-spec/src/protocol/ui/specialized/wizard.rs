// =============================================================================
// File: protocol/ui/specialized/wizard.rs — StepProgress (catalog §8 0x060F)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::StatePath;
use super::super::component::{Component, FieldMap};
use super::super::inline::StepDef;
use super::super::tokens::StepProgressVariant;
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

/// Visual stepper for wizards (catalog §8 0x060F).
#[derive(Debug, Clone, PartialEq)]
pub struct StepProgress {
    pub steps: Vec<StepDef>,
    pub current_id_path: StatePath,
    pub variant: StepProgressVariant,
    pub clickable_completed: bool,
}

impl StepProgress {
    pub const TAG: u16 = 0x060F;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(4);
        e.push((0, encode_to_value(&self.steps)?));
        e.push((1, encode_to_value(&self.current_id_path)?));
        e.push((2, encode_to_value(&self.variant)?));
        e.push((3, encode_to_value(&self.clickable_completed)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "StepProgress")?;
        ensure_no_duplicate_keys("StepProgress", &c.fields.0)?;
        let mut steps = None;
        let mut current_id_path = None;
        let mut variant = None;
        let mut clickable_completed = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => steps = Some(decode_from_value(v)?),
                1 => current_id_path = Some(decode_from_value(v)?),
                2 => variant = Some(decode_from_value(v)?),
                3 => clickable_completed = Some(decode_from_value(v)?),
                other => return Err(unknown_field("StepProgress", *other)),
            }
        }
        Ok(StepProgress {
            steps: steps.ok_or_else(|| missing_field("StepProgress", "steps"))?,
            current_id_path: current_id_path
                .ok_or_else(|| missing_field("StepProgress", "current_id_path"))?,
            variant: variant.ok_or_else(|| missing_field("StepProgress", "variant"))?,
            clickable_completed: clickable_completed
                .ok_or_else(|| missing_field("StepProgress", "clickable_completed"))?,
        })
    }
}

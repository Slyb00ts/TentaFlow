// =============================================================================
// File: protocol/ui/action/bars.rs — ActionBar/SegmentedControl/FilterChips/WizardFooter (catalog §6)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::StatePath;
use super::super::component::{Component, FieldMap};
use super::super::inline::{FilterChipDef, SegmentOption};
use super::super::tokens::{FilterChipsMode, SegmentSize};
use super::super::typed_field::{
    decode_from_value, encode_to_value, ensure_no_duplicate_keys, ensure_ref_tag_decode,
    ensure_ref_tag_encode, ensure_tag, missing_field, unknown_field, IntoComponentError,
};
use super::buttons::Button;

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
// 0x0408 — ActionBar
// -----------------------------------------------------------------------------

/// Bar of leading/trailing actions (catalog §6 0x0408).
#[derive(Debug, Clone, PartialEq)]
pub struct ActionBar {
    pub leading_actions: Vec<Component>,
    pub trailing_actions: Vec<Component>,
    pub divider_between: bool,
    pub sticky: bool,
}

impl ActionBar {
    pub const TAG: u16 = 0x0408;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        for b in &self.leading_actions {
            ensure_ref_tag_encode(b.tag, Button::TAG, "ActionBar", "leading_actions")?;
        }
        for b in &self.trailing_actions {
            ensure_ref_tag_encode(b.tag, Button::TAG, "ActionBar", "trailing_actions")?;
        }
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(4);
        e.push((0, encode_to_value(&self.leading_actions)?));
        e.push((1, encode_to_value(&self.trailing_actions)?));
        e.push((2, encode_to_value(&self.divider_between)?));
        e.push((3, encode_to_value(&self.sticky)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "ActionBar")?;
        ensure_no_duplicate_keys("ActionBar", &c.fields.0)?;
        let mut leading_actions: Option<Vec<Component>> = None;
        let mut trailing_actions: Option<Vec<Component>> = None;
        let mut divider_between = None;
        let mut sticky = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => leading_actions = Some(decode_from_value(v)?),
                1 => trailing_actions = Some(decode_from_value(v)?),
                2 => divider_between = Some(decode_from_value(v)?),
                3 => sticky = Some(decode_from_value(v)?),
                other => return Err(unknown_field("ActionBar", *other)),
            }
        }
        let leading_actions =
            leading_actions.ok_or_else(|| missing_field("ActionBar", "leading_actions"))?;
        let trailing_actions =
            trailing_actions.ok_or_else(|| missing_field("ActionBar", "trailing_actions"))?;
        for b in &leading_actions {
            ensure_ref_tag_decode(b.tag, Button::TAG, "ActionBar", "leading_actions")?;
        }
        for b in &trailing_actions {
            ensure_ref_tag_decode(b.tag, Button::TAG, "ActionBar", "trailing_actions")?;
        }
        Ok(ActionBar {
            leading_actions,
            trailing_actions,
            divider_between: divider_between
                .ok_or_else(|| missing_field("ActionBar", "divider_between"))?,
            sticky: sticky.ok_or_else(|| missing_field("ActionBar", "sticky"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0409 — SegmentedControl
// -----------------------------------------------------------------------------

/// Toggle-like multi-option selector (catalog §6 0x0409).
#[derive(Debug, Clone, PartialEq)]
pub struct SegmentedControl {
    pub bind_path: StatePath,
    pub options: Vec<SegmentOption>,
    pub size: SegmentSize,
    pub full_width: bool,
}

impl SegmentedControl {
    pub const TAG: u16 = 0x0409;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(4);
        e.push((0, encode_to_value(&self.bind_path)?));
        e.push((1, encode_to_value(&self.options)?));
        e.push((2, encode_to_value(&self.size)?));
        e.push((3, encode_to_value(&self.full_width)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "SegmentedControl")?;
        ensure_no_duplicate_keys("SegmentedControl", &c.fields.0)?;
        let mut bind_path = None;
        let mut options = None;
        let mut size = None;
        let mut full_width = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => bind_path = Some(decode_from_value(v)?),
                1 => options = Some(decode_from_value(v)?),
                2 => size = Some(decode_from_value(v)?),
                3 => full_width = Some(decode_from_value(v)?),
                other => return Err(unknown_field("SegmentedControl", *other)),
            }
        }
        Ok(SegmentedControl {
            bind_path: bind_path.ok_or_else(|| missing_field("SegmentedControl", "bind_path"))?,
            options: options.ok_or_else(|| missing_field("SegmentedControl", "options"))?,
            size: size.ok_or_else(|| missing_field("SegmentedControl", "size"))?,
            full_width: full_width
                .ok_or_else(|| missing_field("SegmentedControl", "full_width"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x040A — FilterChips
// -----------------------------------------------------------------------------

/// Row of selectable filter chips (catalog §6 0x040A).
#[derive(Debug, Clone, PartialEq)]
pub struct FilterChips {
    pub chips: Vec<FilterChipDef>,
    pub selected_ids: StatePath,
    pub mode: FilterChipsMode,
    pub clearable: bool,
}

impl FilterChips {
    pub const TAG: u16 = 0x040A;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(4);
        e.push((0, encode_to_value(&self.chips)?));
        e.push((1, encode_to_value(&self.selected_ids)?));
        e.push((2, encode_to_value(&self.mode)?));
        e.push((3, encode_to_value(&self.clearable)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "FilterChips")?;
        ensure_no_duplicate_keys("FilterChips", &c.fields.0)?;
        let mut chips = None;
        let mut selected_ids = None;
        let mut mode = None;
        let mut clearable = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => chips = Some(decode_from_value(v)?),
                1 => selected_ids = Some(decode_from_value(v)?),
                2 => mode = Some(decode_from_value(v)?),
                3 => clearable = Some(decode_from_value(v)?),
                other => return Err(unknown_field("FilterChips", *other)),
            }
        }
        Ok(FilterChips {
            chips: chips.ok_or_else(|| missing_field("FilterChips", "chips"))?,
            selected_ids: selected_ids
                .ok_or_else(|| missing_field("FilterChips", "selected_ids"))?,
            mode: mode.ok_or_else(|| missing_field("FilterChips", "mode"))?,
            clearable: clearable.ok_or_else(|| missing_field("FilterChips", "clearable"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x040B — WizardFooter
// -----------------------------------------------------------------------------

/// Navigation footer for wizards (catalog §6 0x040B).
#[derive(Debug, Clone, PartialEq)]
pub struct WizardFooter {
    pub back_action: Option<Component>,
    pub next_action: Option<Component>,
    pub cancel_action: Option<Component>,
    pub skip_action: Option<Component>,
    pub extra_actions: Vec<Component>,
}

impl WizardFooter {
    pub const TAG: u16 = 0x040B;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        if let Some(b) = &self.back_action {
            ensure_ref_tag_encode(b.tag, Button::TAG, "WizardFooter", "back_action")?;
        }
        if let Some(b) = &self.next_action {
            ensure_ref_tag_encode(b.tag, Button::TAG, "WizardFooter", "next_action")?;
        }
        if let Some(b) = &self.cancel_action {
            ensure_ref_tag_encode(b.tag, Button::TAG, "WizardFooter", "cancel_action")?;
        }
        if let Some(b) = &self.skip_action {
            ensure_ref_tag_encode(b.tag, Button::TAG, "WizardFooter", "skip_action")?;
        }
        for b in &self.extra_actions {
            ensure_ref_tag_encode(b.tag, Button::TAG, "WizardFooter", "extra_actions")?;
        }
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(5);
        if let Some(v) = &self.back_action {
            e.push((0, encode_to_value(v)?));
        }
        if let Some(v) = &self.next_action {
            e.push((1, encode_to_value(v)?));
        }
        if let Some(v) = &self.cancel_action {
            e.push((2, encode_to_value(v)?));
        }
        if let Some(v) = &self.skip_action {
            e.push((3, encode_to_value(v)?));
        }
        e.push((4, encode_to_value(&self.extra_actions)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "WizardFooter")?;
        ensure_no_duplicate_keys("WizardFooter", &c.fields.0)?;
        let mut back_action: Option<Component> = None;
        let mut next_action: Option<Component> = None;
        let mut cancel_action: Option<Component> = None;
        let mut skip_action: Option<Component> = None;
        let mut extra_actions: Option<Vec<Component>> = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => back_action = Some(decode_from_value(v)?),
                1 => next_action = Some(decode_from_value(v)?),
                2 => cancel_action = Some(decode_from_value(v)?),
                3 => skip_action = Some(decode_from_value(v)?),
                4 => extra_actions = Some(decode_from_value(v)?),
                other => return Err(unknown_field("WizardFooter", *other)),
            }
        }
        let extra_actions =
            extra_actions.ok_or_else(|| missing_field("WizardFooter", "extra_actions"))?;
        if let Some(b) = &back_action {
            ensure_ref_tag_decode(b.tag, Button::TAG, "WizardFooter", "back_action")?;
        }
        if let Some(b) = &next_action {
            ensure_ref_tag_decode(b.tag, Button::TAG, "WizardFooter", "next_action")?;
        }
        if let Some(b) = &cancel_action {
            ensure_ref_tag_decode(b.tag, Button::TAG, "WizardFooter", "cancel_action")?;
        }
        if let Some(b) = &skip_action {
            ensure_ref_tag_decode(b.tag, Button::TAG, "WizardFooter", "skip_action")?;
        }
        for b in &extra_actions {
            ensure_ref_tag_decode(b.tag, Button::TAG, "WizardFooter", "extra_actions")?;
        }
        Ok(WizardFooter {
            back_action,
            next_action,
            cancel_action,
            skip_action,
            extra_actions,
        })
    }
}

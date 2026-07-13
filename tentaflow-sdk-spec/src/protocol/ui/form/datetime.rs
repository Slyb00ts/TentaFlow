// =============================================================================
// File: protocol/ui/form/datetime.rs — DatePicker/DateRangePicker/TimePicker/DateTimePicker (catalog §5)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::{BindRef, StatePath};
use super::super::component::{Component, FieldMap};
use super::super::inline::{DatePreset, RangePreset};
use super::super::tokens::{DayOfWeek, TimePrecision};
use super::super::typed_field::{
    decode_from_value, encode_to_value, ensure_no_duplicate_keys, ensure_tag, missing_field,
    unknown_field, IntoComponentError,
};
use super::super::value_format::{DateStyle, TimeStyle};

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
// 0x0314 — DatePicker
// -----------------------------------------------------------------------------

/// Single-date picker (catalog §5 0x0314).
#[derive(Debug, Clone, PartialEq)]
pub struct DatePicker {
    pub bind_path: StatePath,
    pub label: Option<BindRef>,
    pub min_date: Option<String>,
    pub max_date: Option<String>,
    pub locale: Option<String>,
    pub format: DateStyle,
    pub first_day_of_week: DayOfWeek,
    pub disabled_dates: Option<Vec<String>>,
    pub presets: Option<Vec<DatePreset>>,
    pub placeholder: Option<BindRef>,
}

impl DatePicker {
    pub const TAG: u16 = 0x0314;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(10);
        e.push((0, encode_to_value(&self.bind_path)?));
        if let Some(v) = &self.label {
            e.push((1, encode_to_value(v)?));
        }
        if let Some(v) = &self.min_date {
            e.push((2, encode_to_value(v)?));
        }
        if let Some(v) = &self.max_date {
            e.push((3, encode_to_value(v)?));
        }
        if let Some(v) = &self.locale {
            e.push((4, encode_to_value(v)?));
        }
        e.push((5, encode_to_value(&self.format)?));
        e.push((6, encode_to_value(&self.first_day_of_week)?));
        if let Some(v) = &self.disabled_dates {
            e.push((7, encode_to_value(v)?));
        }
        if let Some(v) = &self.presets {
            e.push((8, encode_to_value(v)?));
        }
        if let Some(v) = &self.placeholder {
            e.push((9, encode_to_value(v)?));
        }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "DatePicker")?;
        ensure_no_duplicate_keys("DatePicker", &c.fields.0)?;
        let mut bind_path = None;
        let mut label = None;
        let mut min_date = None;
        let mut max_date = None;
        let mut locale = None;
        let mut format = None;
        let mut first_day_of_week = None;
        let mut disabled_dates = None;
        let mut presets = None;
        let mut placeholder = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => bind_path = Some(decode_from_value(v)?),
                1 => label = Some(decode_from_value(v)?),
                2 => min_date = Some(decode_from_value(v)?),
                3 => max_date = Some(decode_from_value(v)?),
                4 => locale = Some(decode_from_value(v)?),
                5 => format = Some(decode_from_value(v)?),
                6 => first_day_of_week = Some(decode_from_value(v)?),
                7 => disabled_dates = Some(decode_from_value(v)?),
                8 => presets = Some(decode_from_value(v)?),
                9 => placeholder = Some(decode_from_value(v)?),
                other => return Err(unknown_field("DatePicker", *other)),
            }
        }
        Ok(DatePicker {
            bind_path: bind_path.ok_or_else(|| missing_field("DatePicker", "bind_path"))?,
            label,
            min_date,
            max_date,
            locale,
            format: format.ok_or_else(|| missing_field("DatePicker", "format"))?,
            first_day_of_week: first_day_of_week
                .ok_or_else(|| missing_field("DatePicker", "first_day_of_week"))?,
            disabled_dates,
            presets,
            placeholder,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0315 — DateRangePicker
// -----------------------------------------------------------------------------

/// Date range picker (catalog §5 0x0315).
#[derive(Debug, Clone, PartialEq)]
pub struct DateRangePicker {
    pub from_path: StatePath,
    pub to_path: StatePath,
    pub label: Option<BindRef>,
    pub min_date: Option<String>,
    pub max_date: Option<String>,
    pub locale: Option<String>,
    pub format: DateStyle,
    pub first_day_of_week: DayOfWeek,
    pub disabled_dates: Option<Vec<String>>,
    pub presets: Option<Vec<RangePreset>>,
    pub placeholder_from: Option<BindRef>,
    pub placeholder_to: Option<BindRef>,
    pub max_range_days: Option<u16>,
}

impl DateRangePicker {
    pub const TAG: u16 = 0x0315;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(13);
        e.push((0, encode_to_value(&self.from_path)?));
        e.push((1, encode_to_value(&self.to_path)?));
        if let Some(v) = &self.label {
            e.push((2, encode_to_value(v)?));
        }
        if let Some(v) = &self.min_date {
            e.push((3, encode_to_value(v)?));
        }
        if let Some(v) = &self.max_date {
            e.push((4, encode_to_value(v)?));
        }
        if let Some(v) = &self.locale {
            e.push((5, encode_to_value(v)?));
        }
        e.push((6, encode_to_value(&self.format)?));
        e.push((7, encode_to_value(&self.first_day_of_week)?));
        if let Some(v) = &self.disabled_dates {
            e.push((8, encode_to_value(v)?));
        }
        if let Some(v) = &self.presets {
            e.push((9, encode_to_value(v)?));
        }
        if let Some(v) = &self.placeholder_from {
            e.push((10, encode_to_value(v)?));
        }
        if let Some(v) = &self.placeholder_to {
            e.push((11, encode_to_value(v)?));
        }
        if let Some(v) = &self.max_range_days {
            e.push((12, encode_to_value(v)?));
        }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "DateRangePicker")?;
        ensure_no_duplicate_keys("DateRangePicker", &c.fields.0)?;
        let mut from_path = None;
        let mut to_path = None;
        let mut label = None;
        let mut min_date = None;
        let mut max_date = None;
        let mut locale = None;
        let mut format = None;
        let mut first_day_of_week = None;
        let mut disabled_dates = None;
        let mut presets = None;
        let mut placeholder_from = None;
        let mut placeholder_to = None;
        let mut max_range_days = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => from_path = Some(decode_from_value(v)?),
                1 => to_path = Some(decode_from_value(v)?),
                2 => label = Some(decode_from_value(v)?),
                3 => min_date = Some(decode_from_value(v)?),
                4 => max_date = Some(decode_from_value(v)?),
                5 => locale = Some(decode_from_value(v)?),
                6 => format = Some(decode_from_value(v)?),
                7 => first_day_of_week = Some(decode_from_value(v)?),
                8 => disabled_dates = Some(decode_from_value(v)?),
                9 => presets = Some(decode_from_value(v)?),
                10 => placeholder_from = Some(decode_from_value(v)?),
                11 => placeholder_to = Some(decode_from_value(v)?),
                12 => max_range_days = Some(decode_from_value(v)?),
                other => return Err(unknown_field("DateRangePicker", *other)),
            }
        }
        Ok(DateRangePicker {
            from_path: from_path.ok_or_else(|| missing_field("DateRangePicker", "from_path"))?,
            to_path: to_path.ok_or_else(|| missing_field("DateRangePicker", "to_path"))?,
            label,
            min_date,
            max_date,
            locale,
            format: format.ok_or_else(|| missing_field("DateRangePicker", "format"))?,
            first_day_of_week: first_day_of_week
                .ok_or_else(|| missing_field("DateRangePicker", "first_day_of_week"))?,
            disabled_dates,
            presets,
            placeholder_from,
            placeholder_to,
            max_range_days,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0316 — TimePicker
// -----------------------------------------------------------------------------

/// Time picker (catalog §5 0x0316).
#[derive(Debug, Clone, PartialEq)]
pub struct TimePicker {
    pub bind_path: StatePath,
    pub precision: TimePrecision,
    pub format: TimeStyle,
    pub step_minutes: u16,
    pub label: Option<BindRef>,
}

impl TimePicker {
    pub const TAG: u16 = 0x0316;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(5);
        e.push((0, encode_to_value(&self.bind_path)?));
        e.push((1, encode_to_value(&self.precision)?));
        e.push((2, encode_to_value(&self.format)?));
        e.push((3, encode_to_value(&self.step_minutes)?));
        if let Some(v) = &self.label {
            e.push((4, encode_to_value(v)?));
        }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "TimePicker")?;
        ensure_no_duplicate_keys("TimePicker", &c.fields.0)?;
        let mut bind_path = None;
        let mut precision = None;
        let mut format = None;
        let mut step_minutes = None;
        let mut label = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => bind_path = Some(decode_from_value(v)?),
                1 => precision = Some(decode_from_value(v)?),
                2 => format = Some(decode_from_value(v)?),
                3 => step_minutes = Some(decode_from_value(v)?),
                4 => label = Some(decode_from_value(v)?),
                other => return Err(unknown_field("TimePicker", *other)),
            }
        }
        Ok(TimePicker {
            bind_path: bind_path.ok_or_else(|| missing_field("TimePicker", "bind_path"))?,
            precision: precision.ok_or_else(|| missing_field("TimePicker", "precision"))?,
            format: format.ok_or_else(|| missing_field("TimePicker", "format"))?,
            step_minutes: step_minutes
                .ok_or_else(|| missing_field("TimePicker", "step_minutes"))?,
            label,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0317 — DateTimePicker
// -----------------------------------------------------------------------------

/// Combined date + time picker (catalog §5 0x0317).
#[derive(Debug, Clone, PartialEq)]
pub struct DateTimePicker {
    pub bind_path: StatePath,
    pub label: Option<BindRef>,
    pub min_datetime: Option<String>,
    pub max_datetime: Option<String>,
    pub date_format: DateStyle,
    pub time_format: TimeStyle,
    pub time_precision: TimePrecision,
    pub step_minutes: u16,
    pub locale: Option<String>,
    pub first_day_of_week: DayOfWeek,
    pub placeholder: Option<BindRef>,
    pub timezone: Option<String>,
}

impl DateTimePicker {
    pub const TAG: u16 = 0x0317;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(12);
        e.push((0, encode_to_value(&self.bind_path)?));
        if let Some(v) = &self.label {
            e.push((1, encode_to_value(v)?));
        }
        if let Some(v) = &self.min_datetime {
            e.push((2, encode_to_value(v)?));
        }
        if let Some(v) = &self.max_datetime {
            e.push((3, encode_to_value(v)?));
        }
        e.push((4, encode_to_value(&self.date_format)?));
        e.push((5, encode_to_value(&self.time_format)?));
        e.push((6, encode_to_value(&self.time_precision)?));
        e.push((7, encode_to_value(&self.step_minutes)?));
        if let Some(v) = &self.locale {
            e.push((8, encode_to_value(v)?));
        }
        e.push((9, encode_to_value(&self.first_day_of_week)?));
        if let Some(v) = &self.placeholder {
            e.push((10, encode_to_value(v)?));
        }
        if let Some(v) = &self.timezone {
            e.push((11, encode_to_value(v)?));
        }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "DateTimePicker")?;
        ensure_no_duplicate_keys("DateTimePicker", &c.fields.0)?;
        let mut bind_path = None;
        let mut label = None;
        let mut min_datetime = None;
        let mut max_datetime = None;
        let mut date_format = None;
        let mut time_format = None;
        let mut time_precision = None;
        let mut step_minutes = None;
        let mut locale = None;
        let mut first_day_of_week = None;
        let mut placeholder = None;
        let mut timezone = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => bind_path = Some(decode_from_value(v)?),
                1 => label = Some(decode_from_value(v)?),
                2 => min_datetime = Some(decode_from_value(v)?),
                3 => max_datetime = Some(decode_from_value(v)?),
                4 => date_format = Some(decode_from_value(v)?),
                5 => time_format = Some(decode_from_value(v)?),
                6 => time_precision = Some(decode_from_value(v)?),
                7 => step_minutes = Some(decode_from_value(v)?),
                8 => locale = Some(decode_from_value(v)?),
                9 => first_day_of_week = Some(decode_from_value(v)?),
                10 => placeholder = Some(decode_from_value(v)?),
                11 => timezone = Some(decode_from_value(v)?),
                other => return Err(unknown_field("DateTimePicker", *other)),
            }
        }
        Ok(DateTimePicker {
            bind_path: bind_path.ok_or_else(|| missing_field("DateTimePicker", "bind_path"))?,
            label,
            min_datetime,
            max_datetime,
            date_format: date_format
                .ok_or_else(|| missing_field("DateTimePicker", "date_format"))?,
            time_format: time_format
                .ok_or_else(|| missing_field("DateTimePicker", "time_format"))?,
            time_precision: time_precision
                .ok_or_else(|| missing_field("DateTimePicker", "time_precision"))?,
            step_minutes: step_minutes
                .ok_or_else(|| missing_field("DateTimePicker", "step_minutes"))?,
            locale,
            first_day_of_week: first_day_of_week
                .ok_or_else(|| missing_field("DateTimePicker", "first_day_of_week"))?,
            placeholder,
            timezone,
        })
    }
}

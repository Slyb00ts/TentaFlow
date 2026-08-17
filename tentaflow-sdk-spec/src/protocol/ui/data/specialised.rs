// =============================================================================
// File: protocol/ui/data/specialised.rs — CalendarMonth/Image/VisuallyHidden/LiveRegionComponent (catalog §4)
// =============================================================================
//
// LiveRegionComponent: typed Rust struct for the wire component
// `LiveRegion` (catalog §4 0x0226). Renamed to avoid collision with the
// `LiveRegion` token enum (§1.1).
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::{BindRef, StatePath};
use super::super::component::{Component, FieldMap};
use super::super::inline::{AspectRatio, DimensionToken, IconRef};
use super::super::tokens::{
    DayOfWeek, ImageFit, LiveRegion as LiveRegionPoliteness, RadiusToken, Tone,
};
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
// 0x0223 — CalendarMonth
// -----------------------------------------------------------------------------

/// Static month view (catalog §4 0x0223). Handler: `"day_click"`.
#[derive(Debug, Clone, PartialEq)]
pub struct CalendarMonth {
    /// `"YYYY-MM"` ISO month identifier.
    pub month: BindRef,
    pub events_path: Option<StatePath>,
    pub show_week_numbers: bool,
    pub first_day_of_week: DayOfWeek,
}

impl CalendarMonth {
    pub const TAG: u16 = 0x0223;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(4);
        e.push((0, encode_to_value(&self.month)?));
        if let Some(ep) = &self.events_path {
            e.push((1, encode_to_value(ep)?));
        }
        e.push((2, encode_to_value(&self.show_week_numbers)?));
        e.push((3, encode_to_value(&self.first_day_of_week)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "CalendarMonth")?;
        ensure_no_duplicate_keys("CalendarMonth", &c.fields.0)?;
        let mut month = None;
        let mut events_path = None;
        let mut show_week_numbers = None;
        let mut first_day_of_week = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => month = Some(decode_from_value(v)?),
                1 => events_path = Some(decode_from_value(v)?),
                2 => show_week_numbers = Some(decode_from_value(v)?),
                3 => first_day_of_week = Some(decode_from_value(v)?),
                other => return Err(unknown_field("CalendarMonth", *other)),
            }
        }
        Ok(CalendarMonth {
            month: month.ok_or_else(|| missing_field("CalendarMonth", "month"))?,
            events_path,
            show_week_numbers: show_week_numbers
                .ok_or_else(|| missing_field("CalendarMonth", "show_week_numbers"))?,
            first_day_of_week: first_day_of_week
                .ok_or_else(|| missing_field("CalendarMonth", "first_day_of_week"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0224 — Image
// -----------------------------------------------------------------------------

/// Inline image with signed_url_ref (catalog §4 0x0224). Handler: `"click"`.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub src_ref: BindRef,
    pub alt: String,
    pub width: Option<DimensionToken>,
    pub height: Option<DimensionToken>,
    pub fit: ImageFit,
    pub aspect_ratio: Option<AspectRatio>,
    pub radius: Option<RadiusToken>,
    pub clickable: bool,
    pub lazy_load: bool,
}

impl Image {
    pub const TAG: u16 = 0x0224;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(9);
        e.push((0, encode_to_value(&self.src_ref)?));
        e.push((1, encode_to_value(&self.alt)?));
        if let Some(w) = &self.width {
            e.push((2, encode_to_value(w)?));
        }
        if let Some(h) = &self.height {
            e.push((3, encode_to_value(h)?));
        }
        e.push((4, encode_to_value(&self.fit)?));
        if let Some(ar) = &self.aspect_ratio {
            e.push((5, encode_to_value(ar)?));
        }
        if let Some(r) = &self.radius {
            e.push((6, encode_to_value(r)?));
        }
        e.push((7, encode_to_value(&self.clickable)?));
        e.push((8, encode_to_value(&self.lazy_load)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Image")?;
        ensure_no_duplicate_keys("Image", &c.fields.0)?;
        let mut src_ref = None;
        let mut alt = None;
        let mut width = None;
        let mut height = None;
        let mut fit = None;
        let mut aspect_ratio = None;
        let mut radius = None;
        let mut clickable = None;
        let mut lazy_load = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => src_ref = Some(decode_from_value(v)?),
                1 => alt = Some(decode_from_value(v)?),
                2 => width = Some(decode_from_value(v)?),
                3 => height = Some(decode_from_value(v)?),
                4 => fit = Some(decode_from_value(v)?),
                5 => aspect_ratio = Some(decode_from_value(v)?),
                6 => radius = Some(decode_from_value(v)?),
                7 => clickable = Some(decode_from_value(v)?),
                8 => lazy_load = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Image", *other)),
            }
        }
        Ok(Image {
            src_ref: src_ref.ok_or_else(|| missing_field("Image", "src_ref"))?,
            alt: alt.ok_or_else(|| missing_field("Image", "alt"))?,
            width,
            height,
            fit: fit.ok_or_else(|| missing_field("Image", "fit"))?,
            aspect_ratio,
            radius,
            clickable: clickable.ok_or_else(|| missing_field("Image", "clickable"))?,
            lazy_load: lazy_load.ok_or_else(|| missing_field("Image", "lazy_load"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0225 — VisuallyHidden
// -----------------------------------------------------------------------------

/// Screen-reader-only content (catalog §4 0x0225).
#[derive(Debug, Clone, PartialEq)]
pub struct VisuallyHidden {
    pub content: BindRef,
    pub as_live: Option<LiveRegionPoliteness>,
}

impl VisuallyHidden {
    pub const TAG: u16 = 0x0225;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(2);
        e.push((0, encode_to_value(&self.content)?));
        if let Some(l) = &self.as_live {
            e.push((1, encode_to_value(l)?));
        }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "VisuallyHidden")?;
        ensure_no_duplicate_keys("VisuallyHidden", &c.fields.0)?;
        let mut content = None;
        let mut as_live = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => content = Some(decode_from_value(v)?),
                1 => as_live = Some(decode_from_value(v)?),
                other => return Err(unknown_field("VisuallyHidden", *other)),
            }
        }
        Ok(VisuallyHidden {
            content: content.ok_or_else(|| missing_field("VisuallyHidden", "content"))?,
            as_live,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0226 — LiveRegionComponent (wire tag name: `LiveRegion`)
// -----------------------------------------------------------------------------

/// Stand-alone ARIA live region (catalog §4 0x0226). Wire tag name is
/// `LiveRegion`; Rust struct is `LiveRegionComponent` to avoid collision
/// with the `LiveRegion` token enum (§1.1). `politeness` accepts the full
/// `LiveRegion` enum but catalog narrows valid values to `Polite`/`Assertive`
/// — host validator (Krok 4) rejects `Off`.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveRegionComponent {
    pub politeness: LiveRegionPoliteness,
    pub content: BindRef,
    pub visible: bool,
    pub tone: Option<Tone>,
    pub icon: Option<IconRef>,
    pub clear_after_ms: Option<u32>,
}

impl LiveRegionComponent {
    pub const TAG: u16 = 0x0226;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(6);
        e.push((0, encode_to_value(&self.politeness)?));
        e.push((1, encode_to_value(&self.content)?));
        e.push((2, encode_to_value(&self.visible)?));
        if let Some(t) = &self.tone {
            e.push((3, encode_to_value(t)?));
        }
        if let Some(ic) = &self.icon {
            e.push((4, encode_to_value(ic)?));
        }
        if let Some(ca) = &self.clear_after_ms {
            e.push((5, encode_to_value(ca)?));
        }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "LiveRegionComponent")?;
        ensure_no_duplicate_keys("LiveRegionComponent", &c.fields.0)?;
        let mut politeness = None;
        let mut content = None;
        let mut visible = None;
        let mut tone = None;
        let mut icon = None;
        let mut clear_after_ms = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => politeness = Some(decode_from_value(v)?),
                1 => content = Some(decode_from_value(v)?),
                2 => visible = Some(decode_from_value(v)?),
                3 => tone = Some(decode_from_value(v)?),
                4 => icon = Some(decode_from_value(v)?),
                5 => clear_after_ms = Some(decode_from_value(v)?),
                other => return Err(unknown_field("LiveRegionComponent", *other)),
            }
        }
        Ok(LiveRegionComponent {
            politeness: politeness
                .ok_or_else(|| missing_field("LiveRegionComponent", "politeness"))?,
            content: content.ok_or_else(|| missing_field("LiveRegionComponent", "content"))?,
            visible: visible.ok_or_else(|| missing_field("LiveRegionComponent", "visible"))?,
            tone,
            icon,
            clear_after_ms,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0F01 — ZoneEditor (TentaFlow extension; OUTSIDE catalog v1)
// -----------------------------------------------------------------------------

/// Polygon zone editor drawn over a still camera frame. The operator clicks to
/// place vertices, closes a polygon, and the renderer emits the full set on the
/// `"commit"` handler.
///
/// Zones travel as a JSON STRING (`[[[x,y],...], ...]`, normalized 0.0-1.0)
/// rather than nested arrays: it is the exact shape already persisted in
/// `cameras.zones_json`, so no lossy conversion sits between what the operator
/// draws and what the vision engine filters on.
///
/// This tag lives outside the frozen v1 catalog on purpose — it is a product
/// component, not part of the addon-UI contract other SDKs generate bindings for.
#[derive(Debug, Clone, PartialEq)]
pub struct ZoneEditor {
    /// Background still frame (signed snapshot URL) the zones are drawn on.
    pub image_ref: BindRef,
    /// Existing zones as the normalized JSON string described above.
    pub zones_json: BindRef,
}

impl ZoneEditor {
    pub const TAG: u16 = 0x0F01;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(2);
        e.push((0, encode_to_value(&self.image_ref)?));
        e.push((1, encode_to_value(&self.zones_json)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "ZoneEditor")?;
        ensure_no_duplicate_keys("ZoneEditor", &c.fields.0)?;
        let mut image_ref = None;
        let mut zones_json = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => image_ref = Some(decode_from_value(v)?),
                1 => zones_json = Some(decode_from_value(v)?),
                other => return Err(unknown_field("ZoneEditor", *other)),
            }
        }
        Ok(Self {
            image_ref: image_ref.ok_or_else(|| missing_field("ZoneEditor", "image_ref"))?,
            zones_json: zones_json.ok_or_else(|| missing_field("ZoneEditor", "zones_json"))?,
        })
    }
}

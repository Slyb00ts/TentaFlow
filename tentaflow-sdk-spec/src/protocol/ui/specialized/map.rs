// =============================================================================
// File: protocol/ui/specialized/map.rs — MapView (catalog §8 0x0606)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::StatePath;
use super::super::component::{Component, FieldMap};
use super::super::inline::DimensionToken;
use super::super::tokens::TileProvider;
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

/// Geographic map (catalog §8 0x0606).
#[derive(Debug, Clone, PartialEq)]
pub struct MapView {
    pub center_path: StatePath,
    pub zoom_path: StatePath,
    pub tile_provider: TileProvider,
    pub tile_server_url: Option<String>,
    pub height: DimensionToken,
    pub markers_path: StatePath,
    pub polygons_path: Option<StatePath>,
    pub heatmap_path: Option<StatePath>,
    pub interactive: bool,
    pub show_attribution: bool,
}

impl MapView {
    pub const TAG: u16 = 0x0606;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(10);
        e.push((0, encode_to_value(&self.center_path)?));
        e.push((1, encode_to_value(&self.zoom_path)?));
        e.push((2, encode_to_value(&self.tile_provider)?));
        if let Some(v) = &self.tile_server_url {
            e.push((3, encode_to_value(v)?));
        }
        e.push((4, encode_to_value(&self.height)?));
        e.push((5, encode_to_value(&self.markers_path)?));
        if let Some(v) = &self.polygons_path {
            e.push((6, encode_to_value(v)?));
        }
        if let Some(v) = &self.heatmap_path {
            e.push((7, encode_to_value(v)?));
        }
        e.push((8, encode_to_value(&self.interactive)?));
        e.push((9, encode_to_value(&self.show_attribution)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "MapView")?;
        ensure_no_duplicate_keys("MapView", &c.fields.0)?;
        let mut center_path = None;
        let mut zoom_path = None;
        let mut tile_provider = None;
        let mut tile_server_url = None;
        let mut height = None;
        let mut markers_path = None;
        let mut polygons_path = None;
        let mut heatmap_path = None;
        let mut interactive = None;
        let mut show_attribution = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => center_path = Some(decode_from_value(v)?),
                1 => zoom_path = Some(decode_from_value(v)?),
                2 => tile_provider = Some(decode_from_value(v)?),
                3 => tile_server_url = Some(decode_from_value(v)?),
                4 => height = Some(decode_from_value(v)?),
                5 => markers_path = Some(decode_from_value(v)?),
                6 => polygons_path = Some(decode_from_value(v)?),
                7 => heatmap_path = Some(decode_from_value(v)?),
                8 => interactive = Some(decode_from_value(v)?),
                9 => show_attribution = Some(decode_from_value(v)?),
                other => return Err(unknown_field("MapView", *other)),
            }
        }
        Ok(MapView {
            center_path: center_path.ok_or_else(|| missing_field("MapView", "center_path"))?,
            zoom_path: zoom_path.ok_or_else(|| missing_field("MapView", "zoom_path"))?,
            tile_provider: tile_provider
                .ok_or_else(|| missing_field("MapView", "tile_provider"))?,
            tile_server_url,
            height: height.ok_or_else(|| missing_field("MapView", "height"))?,
            markers_path: markers_path.ok_or_else(|| missing_field("MapView", "markers_path"))?,
            polygons_path,
            heatmap_path,
            interactive: interactive.ok_or_else(|| missing_field("MapView", "interactive"))?,
            show_attribution: show_attribution
                .ok_or_else(|| missing_field("MapView", "show_attribution"))?,
        })
    }
}

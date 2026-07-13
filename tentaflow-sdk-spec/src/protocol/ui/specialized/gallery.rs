// =============================================================================
// File: protocol/ui/specialized/gallery.rs — ImageGallery/Carousel/PdfViewer (catalog §8)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::StatePath;
use super::super::component::{Component, FieldMap};
use super::super::inline::{AspectRatio, DimensionToken};
use super::super::tokens::{CarouselGestures, PdfZoomMode, Spacing};
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
// 0x060B — ImageGallery
// -----------------------------------------------------------------------------

/// Grid of images with optional lightbox (catalog §8 0x060B).
#[derive(Debug, Clone, PartialEq)]
pub struct ImageGallery {
    pub images_path: StatePath,
    pub columns: u8,
    pub aspect_ratio: AspectRatio,
    pub gap: Spacing,
    pub lightbox: bool,
    pub lazy_load: bool,
}

impl ImageGallery {
    pub const TAG: u16 = 0x060B;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(6);
        e.push((0, encode_to_value(&self.images_path)?));
        e.push((1, encode_to_value(&self.columns)?));
        e.push((2, encode_to_value(&self.aspect_ratio)?));
        e.push((3, encode_to_value(&self.gap)?));
        e.push((4, encode_to_value(&self.lightbox)?));
        e.push((5, encode_to_value(&self.lazy_load)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "ImageGallery")?;
        ensure_no_duplicate_keys("ImageGallery", &c.fields.0)?;
        let mut images_path = None;
        let mut columns = None;
        let mut aspect_ratio = None;
        let mut gap = None;
        let mut lightbox = None;
        let mut lazy_load = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => images_path = Some(decode_from_value(v)?),
                1 => columns = Some(decode_from_value(v)?),
                2 => aspect_ratio = Some(decode_from_value(v)?),
                3 => gap = Some(decode_from_value(v)?),
                4 => lightbox = Some(decode_from_value(v)?),
                5 => lazy_load = Some(decode_from_value(v)?),
                other => return Err(unknown_field("ImageGallery", *other)),
            }
        }
        Ok(ImageGallery {
            images_path: images_path.ok_or_else(|| missing_field("ImageGallery", "images_path"))?,
            columns: columns.ok_or_else(|| missing_field("ImageGallery", "columns"))?,
            aspect_ratio: aspect_ratio
                .ok_or_else(|| missing_field("ImageGallery", "aspect_ratio"))?,
            gap: gap.ok_or_else(|| missing_field("ImageGallery", "gap"))?,
            lightbox: lightbox.ok_or_else(|| missing_field("ImageGallery", "lightbox"))?,
            lazy_load: lazy_load.ok_or_else(|| missing_field("ImageGallery", "lazy_load"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x060C — Carousel
// -----------------------------------------------------------------------------

/// Slideshow (catalog §8 0x060C).
#[derive(Debug, Clone, PartialEq)]
pub struct Carousel {
    pub items_path: StatePath,
    pub current_index_path: StatePath,
    pub autoplay: bool,
    pub autoplay_ms: u16,
    pub r#loop: bool,
    pub show_indicators: bool,
    pub show_arrows: bool,
    pub gestures: CarouselGestures,
}

impl Carousel {
    pub const TAG: u16 = 0x060C;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(8);
        e.push((0, encode_to_value(&self.items_path)?));
        e.push((1, encode_to_value(&self.current_index_path)?));
        e.push((2, encode_to_value(&self.autoplay)?));
        e.push((3, encode_to_value(&self.autoplay_ms)?));
        e.push((4, encode_to_value(&self.r#loop)?));
        e.push((5, encode_to_value(&self.show_indicators)?));
        e.push((6, encode_to_value(&self.show_arrows)?));
        e.push((7, encode_to_value(&self.gestures)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Carousel")?;
        ensure_no_duplicate_keys("Carousel", &c.fields.0)?;
        let mut items_path = None;
        let mut current_index_path = None;
        let mut autoplay = None;
        let mut autoplay_ms = None;
        let mut r#loop = None;
        let mut show_indicators = None;
        let mut show_arrows = None;
        let mut gestures = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => items_path = Some(decode_from_value(v)?),
                1 => current_index_path = Some(decode_from_value(v)?),
                2 => autoplay = Some(decode_from_value(v)?),
                3 => autoplay_ms = Some(decode_from_value(v)?),
                4 => r#loop = Some(decode_from_value(v)?),
                5 => show_indicators = Some(decode_from_value(v)?),
                6 => show_arrows = Some(decode_from_value(v)?),
                7 => gestures = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Carousel", *other)),
            }
        }
        Ok(Carousel {
            items_path: items_path.ok_or_else(|| missing_field("Carousel", "items_path"))?,
            current_index_path: current_index_path
                .ok_or_else(|| missing_field("Carousel", "current_index_path"))?,
            autoplay: autoplay.ok_or_else(|| missing_field("Carousel", "autoplay"))?,
            autoplay_ms: autoplay_ms.ok_or_else(|| missing_field("Carousel", "autoplay_ms"))?,
            r#loop: r#loop.ok_or_else(|| missing_field("Carousel", "loop"))?,
            show_indicators: show_indicators
                .ok_or_else(|| missing_field("Carousel", "show_indicators"))?,
            show_arrows: show_arrows.ok_or_else(|| missing_field("Carousel", "show_arrows"))?,
            gestures: gestures.ok_or_else(|| missing_field("Carousel", "gestures"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x060D — PdfViewer
// -----------------------------------------------------------------------------

/// Inline PDF viewer (catalog §8 0x060D).
#[derive(Debug, Clone, PartialEq)]
pub struct PdfViewer {
    pub src_ref: String,
    pub page_path: Option<StatePath>,
    pub height: DimensionToken,
    pub zoom_mode: PdfZoomMode,
    pub searchable: bool,
}

impl PdfViewer {
    pub const TAG: u16 = 0x060D;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(5);
        e.push((0, encode_to_value(&self.src_ref)?));
        if let Some(p) = &self.page_path {
            e.push((1, encode_to_value(p)?));
        }
        e.push((2, encode_to_value(&self.height)?));
        e.push((3, encode_to_value(&self.zoom_mode)?));
        e.push((4, encode_to_value(&self.searchable)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "PdfViewer")?;
        ensure_no_duplicate_keys("PdfViewer", &c.fields.0)?;
        let mut src_ref = None;
        let mut page_path = None;
        let mut height = None;
        let mut zoom_mode = None;
        let mut searchable = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => src_ref = Some(decode_from_value(v)?),
                1 => page_path = Some(decode_from_value(v)?),
                2 => height = Some(decode_from_value(v)?),
                3 => zoom_mode = Some(decode_from_value(v)?),
                4 => searchable = Some(decode_from_value(v)?),
                other => return Err(unknown_field("PdfViewer", *other)),
            }
        }
        Ok(PdfViewer {
            src_ref: src_ref.ok_or_else(|| missing_field("PdfViewer", "src_ref"))?,
            page_path,
            height: height.ok_or_else(|| missing_field("PdfViewer", "height"))?,
            zoom_mode: zoom_mode.ok_or_else(|| missing_field("PdfViewer", "zoom_mode"))?,
            searchable: searchable.ok_or_else(|| missing_field("PdfViewer", "searchable"))?,
        })
    }
}

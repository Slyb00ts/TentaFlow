// =============================================================================
// File: protocol/ui/specialized/media.rs — VideoStream/LiveCameraTile/Audio (catalog §8)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::BindRef;
use super::super::component::{Component, FieldMap};
use super::super::inline::AspectRatio;
use super::super::tokens::{AudioControls, AudioVariant, ImageFit, VideoControls};
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
// 0x0604 — VideoStream
// -----------------------------------------------------------------------------

/// MSE-based fMP4 video player (catalog §8 0x0604).
#[derive(Debug, Clone, PartialEq)]
pub struct VideoStream {
    pub stream_id: BindRef,
    pub width_px: Option<u16>,
    pub aspect_ratio: AspectRatio,
    pub controls: VideoControls,
    pub autoplay: bool,
    pub muted: bool,
    pub object_fit: ImageFit,
    pub poster_ref: Option<String>,
}

impl VideoStream {
    pub const TAG: u16 = 0x0604;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(8);
        e.push((0, encode_to_value(&self.stream_id)?));
        if let Some(v) = &self.width_px {
            e.push((1, encode_to_value(v)?));
        }
        e.push((2, encode_to_value(&self.aspect_ratio)?));
        e.push((3, encode_to_value(&self.controls)?));
        e.push((4, encode_to_value(&self.autoplay)?));
        e.push((5, encode_to_value(&self.muted)?));
        e.push((6, encode_to_value(&self.object_fit)?));
        if let Some(v) = &self.poster_ref {
            e.push((7, encode_to_value(v)?));
        }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "VideoStream")?;
        ensure_no_duplicate_keys("VideoStream", &c.fields.0)?;
        let mut stream_id = None;
        let mut width_px = None;
        let mut aspect_ratio = None;
        let mut controls = None;
        let mut autoplay = None;
        let mut muted = None;
        let mut object_fit = None;
        let mut poster_ref = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => stream_id = Some(decode_from_value(v)?),
                1 => width_px = Some(decode_from_value(v)?),
                2 => aspect_ratio = Some(decode_from_value(v)?),
                3 => controls = Some(decode_from_value(v)?),
                4 => autoplay = Some(decode_from_value(v)?),
                5 => muted = Some(decode_from_value(v)?),
                6 => object_fit = Some(decode_from_value(v)?),
                7 => poster_ref = Some(decode_from_value(v)?),
                other => return Err(unknown_field("VideoStream", *other)),
            }
        }
        Ok(VideoStream {
            stream_id: stream_id.ok_or_else(|| missing_field("VideoStream", "stream_id"))?,
            width_px,
            aspect_ratio: aspect_ratio
                .ok_or_else(|| missing_field("VideoStream", "aspect_ratio"))?,
            controls: controls.ok_or_else(|| missing_field("VideoStream", "controls"))?,
            autoplay: autoplay.ok_or_else(|| missing_field("VideoStream", "autoplay"))?,
            muted: muted.ok_or_else(|| missing_field("VideoStream", "muted"))?,
            object_fit: object_fit.ok_or_else(|| missing_field("VideoStream", "object_fit"))?,
            poster_ref,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0605 — LiveCameraTile
// -----------------------------------------------------------------------------

/// Specialised live camera tile (catalog §8 0x0605).
#[derive(Debug, Clone, PartialEq)]
pub struct LiveCameraTile {
    pub stream_id: BindRef,
    pub camera_label: BindRef,
    pub status: BindRef,
    pub fps: Option<BindRef>,
    pub show_overlay: bool,
    pub show_fullscreen_button: bool,
    pub aspect_ratio: AspectRatio,
}

impl LiveCameraTile {
    pub const TAG: u16 = 0x0605;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(7);
        e.push((0, encode_to_value(&self.stream_id)?));
        e.push((1, encode_to_value(&self.camera_label)?));
        e.push((2, encode_to_value(&self.status)?));
        if let Some(v) = &self.fps {
            e.push((3, encode_to_value(v)?));
        }
        e.push((4, encode_to_value(&self.show_overlay)?));
        e.push((5, encode_to_value(&self.show_fullscreen_button)?));
        e.push((6, encode_to_value(&self.aspect_ratio)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "LiveCameraTile")?;
        ensure_no_duplicate_keys("LiveCameraTile", &c.fields.0)?;
        let mut stream_id = None;
        let mut camera_label = None;
        let mut status = None;
        let mut fps = None;
        let mut show_overlay = None;
        let mut show_fullscreen_button = None;
        let mut aspect_ratio = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => stream_id = Some(decode_from_value(v)?),
                1 => camera_label = Some(decode_from_value(v)?),
                2 => status = Some(decode_from_value(v)?),
                3 => fps = Some(decode_from_value(v)?),
                4 => show_overlay = Some(decode_from_value(v)?),
                5 => show_fullscreen_button = Some(decode_from_value(v)?),
                6 => aspect_ratio = Some(decode_from_value(v)?),
                other => return Err(unknown_field("LiveCameraTile", *other)),
            }
        }
        Ok(LiveCameraTile {
            stream_id: stream_id.ok_or_else(|| missing_field("LiveCameraTile", "stream_id"))?,
            camera_label: camera_label
                .ok_or_else(|| missing_field("LiveCameraTile", "camera_label"))?,
            status: status.ok_or_else(|| missing_field("LiveCameraTile", "status"))?,
            fps,
            show_overlay: show_overlay
                .ok_or_else(|| missing_field("LiveCameraTile", "show_overlay"))?,
            show_fullscreen_button: show_fullscreen_button
                .ok_or_else(|| missing_field("LiveCameraTile", "show_fullscreen_button"))?,
            aspect_ratio: aspect_ratio
                .ok_or_else(|| missing_field("LiveCameraTile", "aspect_ratio"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0609 — Audio
// -----------------------------------------------------------------------------

/// Audio player (catalog §8 0x0609).
#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    pub src_ref: BindRef,
    pub controls: AudioControls,
    pub autoplay: bool,
    pub r#loop: bool,
    pub variant: AudioVariant,
}

impl Audio {
    pub const TAG: u16 = 0x0609;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(5);
        e.push((0, encode_to_value(&self.src_ref)?));
        e.push((1, encode_to_value(&self.controls)?));
        e.push((2, encode_to_value(&self.autoplay)?));
        e.push((3, encode_to_value(&self.r#loop)?));
        e.push((4, encode_to_value(&self.variant)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Audio")?;
        ensure_no_duplicate_keys("Audio", &c.fields.0)?;
        let mut src_ref = None;
        let mut controls = None;
        let mut autoplay = None;
        let mut r#loop = None;
        let mut variant = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => src_ref = Some(decode_from_value(v)?),
                1 => controls = Some(decode_from_value(v)?),
                2 => autoplay = Some(decode_from_value(v)?),
                3 => r#loop = Some(decode_from_value(v)?),
                4 => variant = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Audio", *other)),
            }
        }
        Ok(Audio {
            src_ref: src_ref.ok_or_else(|| missing_field("Audio", "src_ref"))?,
            controls: controls.ok_or_else(|| missing_field("Audio", "controls"))?,
            autoplay: autoplay.ok_or_else(|| missing_field("Audio", "autoplay"))?,
            r#loop: r#loop.ok_or_else(|| missing_field("Audio", "loop"))?,
            variant: variant.ok_or_else(|| missing_field("Audio", "variant"))?,
        })
    }
}

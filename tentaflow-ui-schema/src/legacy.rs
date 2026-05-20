// === File: addon/ui/legacy.rs — pre-2.1 components (15 variants) kept until Chunks 2.2-2.6 land replacements ===

use serde::{Deserialize, Serialize};

// =============================================================================
// LegacyComponent — pre-rozszerzenie SDK
// =============================================================================

/// Komponenty pre-rozszerzenie SDK (UI v1 — sprzed Chunka 2.1). Każdy
/// wariant ma swój odpowiednik w nowych kategoriach (DataDisplay/Form/
/// Action/Specialized) i będzie usunięty w kolejnych chunkach 2.2-2.6.
/// Addony używające starych nazw (np. TentaVision) renderują się tą ścieżką,
/// dopóki ich nie zmigrujemy do nowej hierarchii.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LegacyComponent {
    Text {
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        style: Option<String>,
    },

    Input {
        id: String,
        label: String,
        input_type: String,
        #[serde(default)]
        value: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
    },

    Button {
        id: String,
        label: String,
        action: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        style: Option<String>,
    },

    Select {
        id: String,
        label: String,
        options: Vec<(String, String)>,
        #[serde(default)]
        selected: String,
    },

    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },

    Card {
        title: String,
        children: Vec<super::UiComponent>,
    },

    Tabs {
        tabs: Vec<(String, Vec<super::UiComponent>)>,
    },

    Image {
        src: String,
        alt: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        width: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        height: Option<String>,
    },

    List {
        items: Vec<super::UiComponent>,
    },

    Form {
        id: String,
        children: Vec<super::UiComponent>,
        submit_action: String,
    },

    Divider,

    Progress {
        value: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },

    Code {
        language: String,
        content: String,
    },

    Badge {
        text: String,
        #[serde(default = "default_badge_color")]
        color: String,
    },

    /// Live snapshot tile — host renders `<img>` pointed at signed
    /// `frame_url(camera_id, ttl_secs)` and refreshes every `ttl_secs/2`.
    LiveCameraTile {
        camera_id: String,
        #[serde(default = "default_live_tile_ttl")]
        ttl_secs: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        height_px: Option<u32>,
    },

    /// Live fMP4 video stream — front opens binary WS subscription on
    /// `stream_id` and feeds MSE.
    VideoStream {
        stream_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        height_px: Option<u32>,
    },
}

fn default_live_tile_ttl() -> u32 {
    30
}

fn default_badge_color() -> String {
    "blue".to_string()
}

/// Validation constants for `LegacyComponent::LiveCameraTile`.
pub const LIVE_CAMERA_TILE_TTL_MIN: u32 = 5;
pub const LIVE_CAMERA_TILE_TTL_MAX: u32 = 300;

/// Stream-id prefix accepted by `LegacyComponent::VideoStream`. Mirrors the
/// permission gate in `dispatch::stream`.
pub const VIDEO_STREAM_CAMERA_PREFIX: &str = "camera:";

// =============================================================================
// Validation (kept from ui_framework.rs — identical contract)
// =============================================================================

/// Walks the tree clamping `LiveCameraTile.ttl_secs` to the 5..=300 range
/// and rejecting malformed `camera_id` / `stream_id`. Recurses into
/// container variants (Card/Tabs/List/Form).
pub fn validate_and_normalize(component: &mut LegacyComponent) -> anyhow::Result<()> {
    match component {
        LegacyComponent::LiveCameraTile {
            camera_id,
            ttl_secs,
            ..
        } => {
            validate_camera_id(camera_id)?;
            if *ttl_secs < LIVE_CAMERA_TILE_TTL_MIN {
                *ttl_secs = LIVE_CAMERA_TILE_TTL_MIN;
            } else if *ttl_secs > LIVE_CAMERA_TILE_TTL_MAX {
                *ttl_secs = LIVE_CAMERA_TILE_TTL_MAX;
            }
            Ok(())
        }
        LegacyComponent::VideoStream { stream_id, .. } => {
            validate_video_stream_id(stream_id)?;
            Ok(())
        }
        LegacyComponent::Card { children, .. } => {
            for c in children {
                super::validate_and_normalize_component(c)?;
            }
            Ok(())
        }
        LegacyComponent::Tabs { tabs } => {
            for (_, children) in tabs {
                for c in children {
                    super::validate_and_normalize_component(c)?;
                }
            }
            Ok(())
        }
        LegacyComponent::List { items } => {
            for c in items {
                super::validate_and_normalize_component(c)?;
            }
            Ok(())
        }
        LegacyComponent::Form { children, .. } => {
            for c in children {
                super::validate_and_normalize_component(c)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Camera id contract: `cam_<uuid v4>` (length 40). See ui_framework.rs
/// pre-2.1 for original docstring — error messages never echo input.
pub(super) fn validate_camera_id(id: &str) -> anyhow::Result<()> {
    if id.len() != 40 || !id.starts_with("cam_") {
        anyhow::bail!("LiveCameraTile.camera_id invalid format");
    }
    let uuid = &id[4..];
    let bytes = uuid.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        let dash_pos = matches!(i, 8 | 13 | 18 | 23);
        if dash_pos {
            if b != b'-' {
                anyhow::bail!("LiveCameraTile.camera_id invalid format");
            }
        } else if !(b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            anyhow::bail!("LiveCameraTile.camera_id invalid format");
        }
    }
    if bytes[14] != b'4' {
        anyhow::bail!("LiveCameraTile.camera_id invalid format");
    }
    if !matches!(bytes[19], b'8' | b'9' | b'a' | b'b') {
        anyhow::bail!("LiveCameraTile.camera_id invalid format");
    }
    Ok(())
}

fn validate_video_stream_id(id: &str) -> anyhow::Result<()> {
    if !id.starts_with(VIDEO_STREAM_CAMERA_PREFIX) {
        anyhow::bail!("VideoStream.stream_id unsupported prefix");
    }
    let suffix = &id[VIDEO_STREAM_CAMERA_PREFIX.len()..];
    validate_camera_id(suffix)
        .map_err(|_| anyhow::anyhow!("VideoStream.stream_id invalid camera id"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good_cam_id() -> String {
        "cam_550e8400-e29b-41d4-a716-446655440000".to_string()
    }

    #[test]
    fn legacy_text_round_trip() {
        let t = LegacyComponent::Text {
            content: "hello".to_string(),
            style: None,
        };
        let j = serde_json::to_value(&t).expect("serialize");
        assert_eq!(j["type"], "text");
        let back: LegacyComponent = serde_json::from_value(j).expect("deserialize");
        assert_eq!(back, t);
    }

    #[test]
    fn live_camera_tile_clamps_ttl() {
        let mut c = LegacyComponent::LiveCameraTile {
            camera_id: good_cam_id(),
            ttl_secs: 10_000,
            label: None,
            height_px: None,
        };
        validate_and_normalize(&mut c).expect("ok");
        match c {
            LegacyComponent::LiveCameraTile { ttl_secs, .. } => {
                assert_eq!(ttl_secs, LIVE_CAMERA_TILE_TTL_MAX);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn live_camera_tile_rejects_bad_uuid() {
        let mut c = LegacyComponent::LiveCameraTile {
            camera_id: "not-a-uuid".to_string(),
            ttl_secs: 30,
            label: None,
            height_px: None,
        };
        assert!(validate_and_normalize(&mut c).is_err());
    }

    #[test]
    fn video_stream_rejects_unknown_prefix() {
        let mut c = LegacyComponent::VideoStream {
            stream_id: format!("audio:{}", good_cam_id()),
            label: None,
            height_px: None,
        };
        assert!(validate_and_normalize(&mut c).is_err());
    }

    #[test]
    fn live_camera_tile_default_ttl_via_serde() {
        let json = serde_json::json!({
            "type": "live_camera_tile",
            "camera_id": good_cam_id(),
        });
        let c: LegacyComponent = serde_json::from_value(json).expect("deserialize");
        match c {
            LegacyComponent::LiveCameraTile { ttl_secs, .. } => assert_eq!(ttl_secs, 30),
            _ => panic!(),
        }
    }
}

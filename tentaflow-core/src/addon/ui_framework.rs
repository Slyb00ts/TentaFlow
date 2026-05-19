// =============================================================================
// Plik: addon/ui_framework.rs
// Opis: Deklaratywny framework UI dla addonow — model komponentow, renderowanie
//       na HTML (dla obecnego backendu) i serializacja do JSON (dla WGPU).
//       Addon opisuje UI jako strukture danych, Core renderuje odpowiednio.
// =============================================================================

use serde::{Deserialize, Serialize};

// =============================================================================
// UiComponent — deklaratywny komponent UI
// =============================================================================

/// Komponent UI addonu — deklaratywny opis elementu interfejsu.
/// Addon nie generuje HTML bezposrednio — opisuje co chce wyrenderowac.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiComponent {
    /// Blok tekstu
    Text {
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        style: Option<String>,
    },

    /// Pole wejsciowe
    Input {
        id: String,
        label: String,
        input_type: String,
        #[serde(default)]
        value: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
    },

    /// Przycisk
    Button {
        id: String,
        label: String,
        action: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        style: Option<String>,
    },

    /// Lista rozwijana
    Select {
        id: String,
        label: String,
        options: Vec<(String, String)>,
        #[serde(default)]
        selected: String,
    },

    /// Tabela danych
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },

    /// Karta (kontener z tytulem)
    Card {
        title: String,
        children: Vec<UiComponent>,
    },

    /// Zakladki
    Tabs {
        tabs: Vec<(String, Vec<UiComponent>)>,
    },

    /// Obraz
    Image {
        src: String,
        alt: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        width: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        height: Option<String>,
    },

    /// Lista elementow
    List { items: Vec<UiComponent> },

    /// Formularz
    Form {
        id: String,
        children: Vec<UiComponent>,
        submit_action: String,
    },

    /// Separator (linia horyzontalna)
    Divider,

    /// Pasek postepu
    Progress {
        value: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },

    /// Blok kodu
    Code { language: String, content: String },

    /// Etykieta statusu (badge)
    Badge {
        text: String,
        #[serde(default = "default_badge_color")]
        color: String,
    },

    /// Live preview of a single camera. The host renders an `<img>` element
    /// pointed at a signed `frame_url(camera_id, ttl_secs)` and refreshes the
    /// `src` attribute every `ttl_secs / 2` seconds — there is zero round
    /// trip back into the addon WASM module.
    LiveCameraTile {
        /// Camera id (UUID v4, matches `camera_add`).
        camera_id: String,
        /// Validity of each signed URL in seconds. Refresh cadence is
        /// `ttl_secs / 2`. Allowed range: 5..=300; out-of-range values are
        /// clamped on validation.
        #[serde(default = "default_live_tile_ttl")]
        ttl_secs: u32,
        /// Optional label rendered above the preview (e.g. camera name).
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        /// Optional fixed height in pixels (default: auto, square aspect).
        #[serde(skip_serializing_if = "Option::is_none")]
        height_px: Option<u32>,
    },
}

fn default_live_tile_ttl() -> u32 {
    30
}

/// Validation constants for `UiComponent::LiveCameraTile`.
pub const LIVE_CAMERA_TILE_TTL_MIN: u32 = 5;
pub const LIVE_CAMERA_TILE_TTL_MAX: u32 = 300;

fn default_badge_color() -> String {
    "blue".to_string()
}

// =============================================================================
// UiPanel — panel UI addonu
// =============================================================================

/// Panel UI addonu — kontener najwyzszego poziomu z metadanymi
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiPanel {
    /// ID addonu wlasciciela
    pub addon_id: String,
    /// Unikalny ID panelu
    pub panel_id: String,
    /// Tytul panelu
    pub title: String,
    /// Komponenty UI
    pub components: Vec<UiComponent>,
}

impl UiPanel {
    /// Serializuje panel do JSON — to format wysylany frontendowi przez
    /// `AddonUiPanelGetRequest`. Frontend GUI renderuje drzewo przez tf-*
    /// komponenty; host nie produkuje HTML.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

// HTML rendering po stronie hosta zostal usuniety w UI v2 — frontend GUI
// renderuje drzewo komponentow przez tf-* komponenty (pseudokod ponizej
// zachowany w bloku #[cfg(any())] nigdy nie kompilowanym, tylko jako
// dokumentacja semantyki kazdego UiComponent).
#[cfg(any())]
fn render_component_html(html: &mut String, component: &UiComponent, indent: usize) {
    let pad = " ".repeat(indent);

    match component {
        UiComponent::Text { content, style } => {
            let style_attr = style
                .as_ref()
                .map(|s| format!(" style=\"{}\"", escape_html(s)))
                .unwrap_or_default();
            html.push_str(&format!(
                "{}<p class=\"addon-text\"{}>{}</p>\n",
                pad,
                style_attr,
                escape_html(content)
            ));
        }

        UiComponent::Input {
            id,
            label,
            input_type,
            value,
            placeholder,
        } => {
            html.push_str(&format!("{}<div class=\"addon-input-group\">\n", pad));
            html.push_str(&format!(
                "{}  <label for=\"addon-{}\">{}</label>\n",
                pad,
                escape_html(id),
                escape_html(label)
            ));
            let ph = placeholder
                .as_ref()
                .map(|p| format!(" placeholder=\"{}\"", escape_html(p)))
                .unwrap_or_default();
            html.push_str(&format!(
                "{}  <input type=\"{}\" id=\"addon-{}\" name=\"{}\" value=\"{}\"{}>\n",
                pad,
                escape_html(input_type),
                escape_html(id),
                escape_html(id),
                escape_html(value),
                ph
            ));
            html.push_str(&format!("{}</div>\n", pad));
        }

        UiComponent::Button {
            id,
            label,
            action,
            style,
        } => {
            let class = match style.as_deref() {
                Some("primary") => "addon-btn addon-btn-primary",
                Some("danger") => "addon-btn addon-btn-danger",
                Some("success") => "addon-btn addon-btn-success",
                _ => "addon-btn",
            };
            html.push_str(&format!(
                "{}<button class=\"{}\" id=\"addon-{}\" data-action=\"{}\">{}</button>\n",
                pad,
                class,
                escape_html(id),
                escape_html(action),
                escape_html(label)
            ));
        }

        UiComponent::Select {
            id,
            label,
            options,
            selected,
        } => {
            html.push_str(&format!("{}<div class=\"addon-select-group\">\n", pad));
            html.push_str(&format!(
                "{}  <label for=\"addon-{}\">{}</label>\n",
                pad,
                escape_html(id),
                escape_html(label)
            ));
            html.push_str(&format!(
                "{}  <select id=\"addon-{}\" name=\"{}\">\n",
                pad,
                escape_html(id),
                escape_html(id)
            ));
            for (value, display) in options {
                let sel = if value == selected { " selected" } else { "" };
                html.push_str(&format!(
                    "{}    <option value=\"{}\"{}>{}</option>\n",
                    pad,
                    escape_html(value),
                    sel,
                    escape_html(display)
                ));
            }
            html.push_str(&format!("{}  </select>\n", pad));
            html.push_str(&format!("{}</div>\n", pad));
        }

        UiComponent::Table { headers, rows } => {
            html.push_str(&format!("{}<table class=\"addon-table\">\n", pad));
            html.push_str(&format!("{}  <thead><tr>\n", pad));
            for header in headers {
                html.push_str(&format!("{}    <th>{}</th>\n", pad, escape_html(header)));
            }
            html.push_str(&format!("{}  </tr></thead>\n", pad));
            html.push_str(&format!("{}  <tbody>\n", pad));
            for row in rows {
                html.push_str(&format!("{}    <tr>\n", pad));
                for cell in row {
                    html.push_str(&format!("{}      <td>{}</td>\n", pad, escape_html(cell)));
                }
                html.push_str(&format!("{}    </tr>\n", pad));
            }
            html.push_str(&format!("{}  </tbody>\n", pad));
            html.push_str(&format!("{}</table>\n", pad));
        }

        UiComponent::Card { title, children } => {
            html.push_str(&format!("{}<div class=\"addon-card\">\n", pad));
            html.push_str(&format!(
                "{}  <h3 class=\"addon-card-title\">{}</h3>\n",
                pad,
                escape_html(title)
            ));
            html.push_str(&format!("{}  <div class=\"addon-card-body\">\n", pad));
            for child in children {
                render_component_html(html, child, indent + 4);
            }
            html.push_str(&format!("{}  </div>\n", pad));
            html.push_str(&format!("{}</div>\n", pad));
        }

        UiComponent::Tabs { tabs } => {
            html.push_str(&format!("{}<div class=\"addon-tabs\">\n", pad));
            html.push_str(&format!("{}  <div class=\"addon-tabs-nav\">\n", pad));
            for (i, (label, _)) in tabs.iter().enumerate() {
                let active = if i == 0 { " active" } else { "" };
                html.push_str(&format!(
                    "{}    <button class=\"addon-tab-btn{}\" data-tab=\"{}\">{}</button>\n",
                    pad,
                    active,
                    i,
                    escape_html(label)
                ));
            }
            html.push_str(&format!("{}  </div>\n", pad));
            for (i, (_, content)) in tabs.iter().enumerate() {
                let display = if i == 0 {
                    ""
                } else {
                    " style=\"display:none\""
                };
                html.push_str(&format!(
                    "{}  <div class=\"addon-tab-pane\" data-tab=\"{}\"{}>\n",
                    pad, i, display
                ));
                for child in content {
                    render_component_html(html, child, indent + 4);
                }
                html.push_str(&format!("{}  </div>\n", pad));
            }
            html.push_str(&format!("{}</div>\n", pad));
        }

        UiComponent::Image {
            src,
            alt,
            width,
            height,
        } => {
            let w = width
                .as_ref()
                .map(|w| format!(" width=\"{}\"", escape_html(w)))
                .unwrap_or_default();
            let h = height
                .as_ref()
                .map(|h| format!(" height=\"{}\"", escape_html(h)))
                .unwrap_or_default();
            html.push_str(&format!(
                "{}<img class=\"addon-image\" src=\"{}\" alt=\"{}\"{}{}>\n",
                pad,
                escape_html(src),
                escape_html(alt),
                w,
                h
            ));
        }

        UiComponent::List { items } => {
            html.push_str(&format!("{}<ul class=\"addon-list\">\n", pad));
            for item in items {
                html.push_str(&format!("{}  <li>\n", pad));
                render_component_html(html, item, indent + 4);
                html.push_str(&format!("{}  </li>\n", pad));
            }
            html.push_str(&format!("{}</ul>\n", pad));
        }

        UiComponent::Form {
            id,
            children,
            submit_action,
        } => {
            html.push_str(&format!(
                "{}<form class=\"addon-form\" id=\"addon-form-{}\" data-action=\"{}\">\n",
                pad,
                escape_html(id),
                escape_html(submit_action)
            ));
            for child in children {
                render_component_html(html, child, indent + 2);
            }
            html.push_str(&format!(
                "{}  <button type=\"submit\" class=\"addon-btn addon-btn-primary\">Wyslij</button>\n",
                pad
            ));
            html.push_str(&format!("{}</form>\n", pad));
        }

        UiComponent::Divider => {
            html.push_str(&format!("{}<hr class=\"addon-divider\">\n", pad));
        }

        UiComponent::Progress { value, label } => {
            let pct = (value * 100.0).min(100.0).max(0.0);
            let lbl = label
                .as_ref()
                .map(|l| escape_html(l))
                .unwrap_or_else(|| format!("{:.0}%", pct));
            html.push_str(&format!(
                "{}<div class=\"addon-progress\">\n\
                 {}  <div class=\"addon-progress-bar\" style=\"width:{:.0}%\">{}</div>\n\
                 {}</div>\n",
                pad, pad, pct, lbl, pad
            ));
        }

        UiComponent::Code { language, content } => {
            html.push_str(&format!(
                "{}<pre class=\"addon-code\"><code class=\"language-{}\">{}</code></pre>\n",
                pad,
                escape_html(language),
                escape_html(content)
            ));
        }

        UiComponent::Badge { text, color } => {
            html.push_str(&format!(
                "{}<span class=\"addon-badge addon-badge-{}\">{}</span>\n",
                pad,
                escape_html(color),
                escape_html(text)
            ));
        }

        UiComponent::LiveCameraTile {
            camera_id,
            ttl_secs,
            label,
            height_px,
        } => {
            let height_attr = height_px
                .map(|h| format!(" data-height-px=\"{}\"", h))
                .unwrap_or_default();
            let label_block = label
                .as_ref()
                .map(|l| {
                    format!(
                        "  <div class=\"tf-live-camera-label\">{}</div>\n",
                        escape_html(l)
                    )
                })
                .unwrap_or_default();
            html.push_str(&format!(
                "{}<div class=\"tf-live-camera-tile\" data-camera-id=\"{}\" data-ttl-secs=\"{}\"{}>\n\
                 {}{}  <img class=\"tf-live-camera-img\" alt=\"Live preview\" />\n\
                 {}</div>\n",
                pad,
                escape_html(camera_id),
                ttl_secs,
                height_attr,
                pad,
                label_block,
                pad
            ));
        }
    }
}

// =============================================================================
// Parsowanie komponentow z JSON
// =============================================================================

/// Parsuje komponenty UI z wartosci JSON (uzywane przez host function ui_render)
pub fn parse_components_from_json(json: &serde_json::Value) -> Vec<UiComponent> {
    if let Some(components) = json.get("components").and_then(|v| v.as_array()) {
        components
            .iter()
            .filter_map(|v| serde_json::from_value::<UiComponent>(v.clone()).ok())
            .collect()
    } else if let Ok(component) = serde_json::from_value::<UiComponent>(json.clone()) {
        vec![component]
    } else {
        Vec::new()
    }
}

// =============================================================================
// Helpers
// =============================================================================

// =============================================================================
// Validation
// =============================================================================

/// Validates a `UiComponent` tree in place. Today only `LiveCameraTile`
/// carries host-enforced invariants; other variants are structurally validated
/// at deserialization time. `ttl_secs` outside `5..=300` is clamped (the host
/// accepts the tile but warns via the returned `Result::Ok` so the frontend
/// sees the clamped value). Hard errors (empty/invalid `camera_id`, wrong
/// length) bubble up as `Err`.
pub fn validate_and_normalize_component(component: &mut UiComponent) -> anyhow::Result<()> {
    match component {
        UiComponent::LiveCameraTile {
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
        UiComponent::Card { children, .. } => {
            for c in children {
                validate_and_normalize_component(c)?;
            }
            Ok(())
        }
        UiComponent::Tabs { tabs } => {
            for (_, children) in tabs {
                for c in children {
                    validate_and_normalize_component(c)?;
                }
            }
            Ok(())
        }
        UiComponent::List { items } => {
            for c in items {
                validate_and_normalize_component(c)?;
            }
            Ok(())
        }
        UiComponent::Form { children, .. } => {
            for c in children {
                validate_and_normalize_component(c)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Camera id contract: `cam_<uuid v4>` (length 40, `cam_` prefix + canonical
/// 8-4-4-4-12 UUID v4 layout, lowercase hex, version nibble `4` at index 14
/// of the uuid suffix, RFC 4122 variant nibble in `8..=b` at index 19).
/// Matches the mint format used by `camera_add_onvif` and the addon `camera_add`
/// host fn. Strict — addons that pass non-conformant values fail fast before
/// any signed URL is minted. Error messages never echo the input value to
/// keep audit/install logs PII-free.
fn validate_camera_id(id: &str) -> anyhow::Result<()> {
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

/// Strict counterpart of `parse_components_from_json` used by `ui_render`:
/// returns an error on any malformed component (instead of silently dropping
/// it via `filter_map`) and runs `validate_and_normalize_component` on every
/// node in the tree. On success the returned JSON value mirrors the input
/// shape (single component, `{components: [...]}`, or full panel) with
/// normalized fields (e.g. clamped `ttl_secs`) so the cached panel reflects
/// what the host actually accepted.
pub fn parse_and_validate_ui_json(json: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
    if let Some(arr) = json.get("components").and_then(|v| v.as_array()) {
        let mut normalized = Vec::with_capacity(arr.len());
        for v in arr {
            let mut c: UiComponent = serde_json::from_value(v.clone())
                .map_err(|e| anyhow::anyhow!("invalid component: {}", e))?;
            validate_and_normalize_component(&mut c)?;
            normalized.push(serde_json::to_value(&c)?);
        }
        let mut out = json.clone();
        if let Some(obj) = out.as_object_mut() {
            obj.insert(
                "components".to_string(),
                serde_json::Value::Array(normalized),
            );
        }
        Ok(out)
    } else {
        let mut c: UiComponent = serde_json::from_value(json.clone())
            .map_err(|e| anyhow::anyhow!("invalid component: {}", e))?;
        validate_and_normalize_component(&mut c)?;
        Ok(serde_json::to_value(&c)?)
    }
}

/// Escapuje znaki specjalne HTML
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_panel_to_json() {
        let panel = UiPanel {
            addon_id: "test".to_string(),
            panel_id: "p1".to_string(),
            title: "Test".to_string(),
            components: vec![UiComponent::Badge {
                text: "OK".to_string(),
                color: "green".to_string(),
            }],
        };

        let json = panel.to_json();
        assert!(json.is_object());
        assert_eq!(json["addon_id"], "test");
    }

    #[test]
    fn test_parse_components_from_json() {
        let json = serde_json::json!({
            "components": [
                {
                    "type": "text",
                    "content": "Test"
                },
                {
                    "type": "button",
                    "id": "btn",
                    "label": "Click",
                    "action": "do_thing"
                }
            ]
        });

        let components = parse_components_from_json(&json);
        assert_eq!(components.len(), 2);
    }

    #[test]
    fn live_camera_tile_serde_round_trip() {
        let cam = "550e8400-e29b-41d4-a716-446655440000".to_string();
        let original = UiComponent::LiveCameraTile {
            camera_id: cam.clone(),
            ttl_secs: 45,
            label: Some("Front Door".to_string()),
            height_px: Some(240),
        };
        let json = serde_json::to_value(&original).expect("serialize");
        assert_eq!(json["type"], "live_camera_tile");
        assert_eq!(json["camera_id"], cam.as_str());
        assert_eq!(json["ttl_secs"], 45);
        let back: UiComponent = serde_json::from_value(json).expect("deserialize");
        match back {
            UiComponent::LiveCameraTile {
                camera_id,
                ttl_secs,
                label,
                height_px,
            } => {
                assert_eq!(camera_id, cam);
                assert_eq!(ttl_secs, 45);
                assert_eq!(label.as_deref(), Some("Front Door"));
                assert_eq!(height_px, Some(240));
            }
            _ => panic!("variant changed during round-trip"),
        }
    }

    #[test]
    fn live_camera_tile_clamps_ttl_below_min() {
        let mut c = UiComponent::LiveCameraTile {
            camera_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            ttl_secs: 1,
            label: None,
            height_px: None,
        };
        validate_and_normalize_component(&mut c).expect("ok");
        match c {
            UiComponent::LiveCameraTile { ttl_secs, .. } => {
                assert_eq!(ttl_secs, LIVE_CAMERA_TILE_TTL_MIN);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn live_camera_tile_clamps_ttl_above_max() {
        let mut c = UiComponent::LiveCameraTile {
            camera_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            ttl_secs: 10_000,
            label: None,
            height_px: None,
        };
        validate_and_normalize_component(&mut c).expect("ok");
        match c {
            UiComponent::LiveCameraTile { ttl_secs, .. } => {
                assert_eq!(ttl_secs, LIVE_CAMERA_TILE_TTL_MAX);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn live_camera_tile_rejects_non_v4_uuid() {
        let mut c = UiComponent::LiveCameraTile {
            camera_id: "550e8400-e29b-11d4-a716-446655440000".to_string(),
            ttl_secs: 30,
            label: None,
            height_px: None,
        };
        assert!(validate_and_normalize_component(&mut c).is_err());
    }

    #[test]
    fn live_camera_tile_rejects_invalid_variant() {
        let mut c = UiComponent::LiveCameraTile {
            camera_id: "550e8400-e29b-41d4-c716-446655440000".to_string(),
            ttl_secs: 30,
            label: None,
            height_px: None,
        };
        assert!(validate_and_normalize_component(&mut c).is_err());
    }

    #[test]
    fn live_camera_tile_rejects_bad_uuid() {
        let mut c = UiComponent::LiveCameraTile {
            camera_id: "not-a-uuid".to_string(),
            ttl_secs: 30,
            label: None,
            height_px: None,
        };
        assert!(validate_and_normalize_component(&mut c).is_err());
    }

    #[test]
    fn live_camera_tile_default_ttl_via_serde() {
        let json = serde_json::json!({
            "type": "live_camera_tile",
            "camera_id": "550e8400-e29b-41d4-a716-446655440000",
        });
        let c: UiComponent = serde_json::from_value(json).expect("deserialize");
        match c {
            UiComponent::LiveCameraTile { ttl_secs, .. } => assert_eq!(ttl_secs, 30),
            _ => panic!(),
        }
    }

    #[test]
    fn ui_render_rejects_invalid_live_camera_tile() {
        // Simulates the WASM ABI path: addon ships a `live_camera_tile`
        // with a path-traversal payload as `camera_id`. The strict
        // parse+validate must refuse it so nothing reaches the UI cache.
        let panel_json = serde_json::json!({
            "components": [
                {
                    "type": "live_camera_tile",
                    "camera_id": "../etc/passwd",
                    "ttl_secs": 30
                }
            ]
        });
        let err = parse_and_validate_ui_json(&panel_json)
            .expect_err("path traversal camera_id must be rejected");
        let msg = format!("{}", err);
        assert!(
            !msg.contains("../etc/passwd"),
            "error message must not echo addon input"
        );
    }

    #[test]
    fn ui_render_accepts_valid_live_camera_tile_and_normalizes_ttl() {
        let panel_json = serde_json::json!({
            "components": [
                {
                    "type": "live_camera_tile",
                    "camera_id": "550e8400-e29b-41d4-a716-446655440000",
                    "ttl_secs": 9999
                }
            ]
        });
        let out = parse_and_validate_ui_json(&panel_json).expect("must accept valid tile");
        assert_eq!(
            out["components"][0]["ttl_secs"],
            serde_json::json!(LIVE_CAMERA_TILE_TTL_MAX)
        );
    }

    #[test]
    fn test_table_round_trips_through_json() {
        let panel = UiPanel {
            addon_id: "t".to_string(),
            panel_id: "p".to_string(),
            title: "T".to_string(),
            components: vec![UiComponent::Table {
                headers: vec!["Nazwa".to_string(), "Wartosc".to_string()],
                rows: vec![vec!["klucz".to_string(), "123".to_string()]],
            }],
        };

        let json = panel.to_json();
        assert_eq!(json["components"][0]["type"], "table");
        assert_eq!(json["components"][0]["headers"][0], "Nazwa");
        assert_eq!(json["components"][0]["rows"][0][0], "klucz");
    }
}

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
}

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

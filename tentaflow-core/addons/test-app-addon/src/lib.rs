// =============================================================================
// Plik: addons/test-app-addon/src/lib.rs
// Opis: End-to-end test addon stack v2 — pokrycie:
//   - [application] tile w Apps menu (on_start renders panel "main")
//   - [service] tick loop (on_tick increments counter, re-renders co 2s)
//   - Wszystkie 14 wariantow UiComponent (text/input/button/select/table/
//     card/tabs/image/list/form/divider/progress/code/badge)
//   - 3 actions: refresh / increment / submit_form (button click + form submit)
//   - Flow block "uppercase" przez `on_request` z tool = "block.uppercase"
//   - publish_event po kazdej akcji
//   - storage_get/set dla persistent counter
// =============================================================================

use tentaflow_addon_sdk::prelude::*;

// =============================================================================
// Lifecycle
// =============================================================================

#[no_mangle]
pub extern "C" fn on_install() -> i32 {
    log::info("test-app-addon zainstalowany");
    0
}

#[no_mangle]
pub extern "C" fn on_start() -> i32 {
    log::info("test-app-addon uruchomiony — renderuje panel main");
    let _ = render_main_panel();
    0
}

#[no_mangle]
pub extern "C" fn on_stop() -> i32 {
    log::info("test-app-addon zatrzymany");
    0
}

// =============================================================================
// on_tick — service mode driver
// =============================================================================

/// Wywolywane co [service].tick_interval_ms (2000ms). Increment counter w
/// storage, re-render panelu z nowa wartoscia. Pokazuje persistent state
/// miedzy tickami + per-tick refresh UI.
#[no_mangle]
pub extern "C" fn on_tick(_timestamp_ms: i64) -> i32 {
    // Bump counter w storage (validates storage host functions + persistent
    // state across tick instance lifetime).
    let current: u32 = store_get("tick_counter")
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let next = current + 1;
    let _ = store_set("tick_counter", &next.to_string());

    // Re-render — frontend pobiera fresh state przy nastepnym ReqPanelGet.
    let _ = render_main_panel();
    0
}

// =============================================================================
// on_event — przyjmuje subskrypcje
// =============================================================================

#[no_mangle]
pub extern "C" fn on_event(event_ptr: i32, event_len: i32) -> i32 {
    let event_json = read_string(event_ptr, event_len);
    log::info(&format!("test-app-addon on_event: {}", event_json));
    0
}

// =============================================================================
// on_request — UI actions + flow block dispatch
// =============================================================================

#[no_mangle]
pub extern "C" fn on_request(
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let input_json = read_string(input_ptr, input_len);
    let request: Value = match serde_json::from_str(&input_json) {
        Ok(v) => v,
        Err(e) => {
            log::error(&format!("on_request: invalid JSON: {}", e));
            return 1;
        }
    };
    let tool = request
        .get("tool")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let params = request
        .get("params")
        .cloned()
        .unwrap_or(Value::Null);

    let response = if let Some(stripped) = tool.strip_prefix("ui.main.") {
        handle_ui_action(stripped, &params)
    } else if let Some(block_type) = tool.strip_prefix("block.") {
        handle_flow_block(block_type, &params)
    } else {
        json!({ "error": format!("unknown tool '{}'", tool) })
    };

    // Write response back
    let response_str = response.to_string();
    let response_bytes = response_str.as_bytes();
    let n = write_string(out_ptr, out_cap, &response_str);
    if n < 0 {
        return 2;
    }
    // out_len_ptr is i32*
    unsafe {
        let p = out_len_ptr as *mut i32;
        *p = response_bytes.len() as i32;
    }
    0
}

// =============================================================================
// UI actions
// =============================================================================

fn handle_ui_action(action: &str, params: &Value) -> Value {
    log::info(&format!("UI action '{}' with params: {}", action, params));

    match action {
        "refresh" => {
            // Re-render — counter zostaje, ale UI sie odswiezy.
            let _ = render_main_panel();
            let _ = publish_event(
                "test.refresh",
                json!({ "addon": "test-app-addon" }),
            );
            notify("Test App", "Panel odswiezony");
            json!({ "ok": true })
        }
        "increment" => {
            // Manual counter bump (independent od tick).
            let current: u32 = store_get("tick_counter")
                .ok()
                .flatten()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let next = current + 10; // +10 zeby roznic od tick (+1)
            let _ = store_set("tick_counter", &next.to_string());
            let _ = render_main_panel();
            json!({ "ok": true, "counter": next })
        }
        "submit_form" => {
            // Form submit z params zebranymi z tf-input/tf-select.
            // params = { "username": "...", "color": "red", ... }
            let username = params
                .get("username")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let color = params
                .get("color")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            log::info(&format!(
                "Form submitted: username={}, color={}",
                username, color
            ));
            // Zachowaj w storage zeby panel pokazal po refresh.
            let _ = store_set("last_username", username);
            let _ = store_set("last_color", color);
            let _ = render_main_panel();
            notify(
                "Test App",
                &format!("Form: {} ({})", username, color),
            );
            json!({ "ok": true, "echo": params.clone() })
        }
        _ => json!({ "error": format!("unknown action '{}'", action) }),
    }
}

// =============================================================================
// Flow block: uppercase
// =============================================================================

fn handle_flow_block(block_type: &str, params: &Value) -> Value {
    log::info(&format!("Flow block '{}' invoked", block_type));

    match block_type {
        "uppercase" => {
            // params = FlowEnvelope serialized as JSON. Wyciagamy payload.text.
            let text = params
                .get("payload")
                .and_then(|p| p.get("Text"))
                .and_then(|t| t.as_str())
                .unwrap_or("");
            let upper = text.to_uppercase();
            // Zwracamy FlowEnvelope-shaped JSON z payload zmienionym.
            let mut response = params.clone();
            if let Some(obj) = response.as_object_mut() {
                obj.insert("payload".into(), json!({ "Text": upper }));
            }
            response
        }
        _ => json!({ "error": format!("unknown flow block '{}'", block_type) }),
    }
}

// =============================================================================
// Panel rendering — wszystkie 14 wariantow UiComponent
// =============================================================================

fn render_main_panel() -> Result<(), String> {
    let counter: u32 = store_get("tick_counter")
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let last_username = store_get("last_username")
        .ok()
        .flatten()
        .unwrap_or_else(|| "(brak)".to_string());
    let last_color = store_get("last_color")
        .ok()
        .flatten()
        .unwrap_or_else(|| "(brak)".to_string());

    let panel = json!({
        "addon_id": "test-app-addon",
        "panel_id": "main",
        "title": "Test App — wszystkie 14 wariantow UiComponent",
        "components": [
            // 1. Text — domyslny + warianty stylu
            { "type": "text", "content": "Witaj w Test App. Tick licznik (auto-increment co 2s):" },
            { "type": "text", "content": format!("Counter: {}", counter), "style": "bold" },
            { "type": "text", "content": "(muted variant)", "style": "muted" },
            { "type": "text", "content": "(error variant)", "style": "error" },

            // 2. Divider
            { "type": "divider" },

            // 3. Card z zagniezdzonymi komponentami
            {
                "type": "card",
                "title": "Form test (recursive children)",
                "children": [
                    {
                        "type": "form",
                        "id": "test-form",
                        "submit_action": "submit_form",
                        "children": [
                            // 4. Input
                            {
                                "type": "input",
                                "id": "username",
                                "label": "Username",
                                "input_type": "text",
                                "value": "",
                                "placeholder": "wpisz cos..."
                            },
                            // 5. Select
                            {
                                "type": "select",
                                "id": "color",
                                "label": "Color",
                                "options": [["red", "Czerwony"], ["green", "Zielony"], ["blue", "Niebieski"]],
                                "selected": "red"
                            }
                        ]
                    },
                    { "type": "text", "content": format!("Ostatni submit: username={}, color={}", last_username, last_color), "style": "muted" }
                ]
            },

            // 6. Buttons — primary + secondary + danger
            {
                "type": "card",
                "title": "Actions",
                "children": [
                    { "type": "button", "id": "btn-refresh", "label": "Refresh", "action": "refresh", "style": "primary" },
                    { "type": "button", "id": "btn-inc", "label": "Increment +10", "action": "increment", "style": "secondary" }
                ]
            },

            // 7. Tabs (recursive content per tab)
            {
                "type": "tabs",
                "tabs": [
                    ["Tab 1", [
                        { "type": "text", "content": "Zawartosc tab 1" },
                        { "type": "badge", "text": "OK", "color": "green" }
                    ]],
                    ["Tab 2", [
                        { "type": "text", "content": "Zawartosc tab 2" },
                        { "type": "badge", "text": "WARN", "color": "yellow" }
                    ]],
                    ["Tab 3", [
                        { "type": "code", "language": "json", "content": format!("{{\"counter\": {}}}", counter) }
                    ]]
                ]
            },

            // 8. Table
            {
                "type": "table",
                "headers": ["Klucz", "Wartosc"],
                "rows": [
                    ["counter", counter.to_string()],
                    ["last_username", last_username.clone()],
                    ["last_color", last_color.clone()]
                ]
            },

            // 9. List (recursive items)
            {
                "type": "list",
                "items": [
                    { "type": "text", "content": "Pierwszy element listy" },
                    { "type": "text", "content": "Drugi element listy" },
                    { "type": "badge", "text": "trzeci", "color": "blue" }
                ]
            },

            // 10. Progress (counter as 0-100% mod 100)
            {
                "type": "progress",
                "value": (counter % 100) as f32 / 100.0,
                "label": format!("{}%", counter % 100)
            },

            // 11. Image (same-origin path zeby przejsc walidacje renderImage)
            { "type": "image", "src": "/tentaflow.png", "alt": "TentaFlow logo" },

            // 12. Code
            {
                "type": "code",
                "language": "rust",
                "content": "fn render() {\n    log::info(\"hello from test-app\");\n}"
            },

            // 13. Badge variants
            { "type": "badge", "text": "info", "color": "blue" },
            { "type": "badge", "text": "success", "color": "green" },
            { "type": "badge", "text": "warning", "color": "yellow" },
            { "type": "badge", "text": "danger", "color": "red" }
        ]
    });

    render_panel("main", panel)
}

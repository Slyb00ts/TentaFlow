// =============================================================================
// Plik: lib.rs
// Opis: Szablon addonu TentaFlow — demonstracja SDK, lifecycle hooks,
//       tool calling i obslugi eventow. Kompilowany do WASM (cdylib).
// =============================================================================

use tentaflow_addon_sdk::prelude::*;

// =============================================================================
// Lifecycle hooks — wymagane eksporty WASM
// =============================================================================

/// Wywolywane przy instalacji addonu.
/// Miejsce na jednorazowa inicjalizacje (np. tworzenie kluczy w storage).
#[no_mangle]
pub extern "C" fn on_install() -> i32 {
    log::info("Template addon zainstalowany");

    // Zapisz domyslna konfiguracje w storage
    if let Err(e) = store_set("initialized", "true") {
        log::error(&format!("Blad inicjalizacji storage: {}", e));
        return 1;
    }

    0
}

/// Wywolywane przy uruchomieniu instancji addonu.
/// Rejestracja narzedzi, subskrypcja eventow, renderowanie UI.
#[no_mangle]
pub extern "C" fn on_start() -> i32 {
    log::info("Template addon uruchomiony");

    // Zarejestruj narzedzie "hello" dla LLM tool calling
    register_tool(
        "hello",
        "Zwraca przykladowe powitanie. Uzyj gdy uzytkownik prosi o demonstracje addonu.",
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Imie osoby do powitania (opcjonalne)"
                }
            }
        }),
    );

    // Render the addon UI panel using typed primitives. `render_panel_typed`
    // serializes the `PanelTree` straight to JSON — no intermediate
    // `serde_json::Value` allocation versus the legacy `json!({...})` macro
    // path (see `notes/addon-ui-perf-plan.md`).
    use ui::legacy::LegacyComponent;
    use ui::{PanelTree, UiComponent};

    let panel = PanelTree {
        root: vec![UiComponent::Legacy(LegacyComponent::Card {
            title: "Template Addon".to_string(),
            children: vec![
                UiComponent::Legacy(LegacyComponent::Text {
                    content: "Status: aktywny".to_string(),
                    style: None,
                }),
                UiComponent::Legacy(LegacyComponent::Button {
                    id: "greet_button".to_string(),
                    label: "Wyslij powitanie".to_string(),
                    action: "greet".to_string(),
                    style: None,
                }),
            ],
        })],
        overlays: vec![],
        navigation: None,
    };

    if let Err(e) = render_panel_typed("main", &panel) {
        log::warn(&format!("Blad renderowania panelu UI: {}", e));
    }

    0
}

/// Wywolywane przy zatrzymaniu instancji addonu.
/// Czyszczenie zasobow, zapisanie stanu.
#[no_mangle]
pub extern "C" fn on_stop() -> i32 {
    log::info("Template addon zatrzymany");
    0
}

// =============================================================================
// Obsluga eventow
// =============================================================================

/// Wywolywane periodycznie przez AddonManager gdy manifest deklaruje sekcje
/// `[service]` z `tick_interval_ms`. Persistent state addonu (statyki Rust,
/// guest memory) przezywa miedzy tickami — tu jest miejsce na pull/polling
/// long-running pracy (np. odczyt kolejnej klatki z kamery i jej analiza).
///
/// `timestamp_ms` to UTC unix ms z momentu wywolania.
/// Zwroc 0 dla success, niezero = blad (host loguje + emit'uje event
/// "addon.tick_error", petla kontynuuje).
///
/// Brak eksportu tej funkcji jest OK — host wykryje przez `get_typed_func`
/// i pominie tick (addon dziala wtedy tylko event-driven, ale ma persistent
/// state).
#[no_mangle]
pub extern "C" fn on_tick(_timestamp_ms: i64) -> i32 {
    // Template nie ma service mode — placeholder. Removeable.
    0
}

/// Wywolywane gdy addon otrzyma event z event bus.
/// Parametry: wskaznik i dlugosc JSON eventu w pamieci WASM.
#[no_mangle]
pub extern "C" fn on_event(event_ptr: i32, event_len: i32) -> i32 {
    let event_json = tentaflow_addon_sdk::read_string(event_ptr, event_len);

    // Parsuj event
    let event: Value = match serde_json::from_str(&event_json) {
        Ok(v) => v,
        Err(e) => {
            log::error(&format!("Blad parsowania eventu: {}", e));
            return 1;
        }
    };

    let event_type = event.get("event_type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    log::info(&format!("Otrzymano event: {}", event_type));

    0
}

// =============================================================================
// Obsluga requestow (tool calls, UI actions)
// =============================================================================

/// Glowny handler requestow z hosta — obsluguje tool calls i akcje UI.
/// Parametry:
/// - input_ptr/input_len: JSON requestu (tool, params, user_id)
/// - out_ptr/out_cap: bufor na odpowiedz
/// - out_len_ptr: wskaznik na dlugosc odpowiedzi (4 bajty LE)
#[no_mangle]
pub extern "C" fn on_request(
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let input_json = tentaflow_addon_sdk::read_string(input_ptr, input_len);

    // Parsuj request
    let request: Value = match serde_json::from_str(&input_json) {
        Ok(v) => v,
        Err(e) => {
            let error_response = json!({"error": format!("Blad parsowania requestu: {}", e)});
            return write_response(out_ptr, out_cap, out_len_ptr, &error_response);
        }
    };

    let tool_name = request.get("tool")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let params = request.get("params")
        .cloned()
        .unwrap_or(json!({}));

    // Dispatchuj po nazwie narzedzia
    let result = match tool_name {
        "hello" => handle_hello(&params),
        _ => json!({"error": format!("Nieznane narzedzie: {}", tool_name)}),
    };

    write_response(out_ptr, out_cap, out_len_ptr, &result)
}

// =============================================================================
// Implementacje narzedzi
// =============================================================================

/// Narzedzie "hello" — zwraca przykladowe powitanie
fn handle_hello(params: &Value) -> Value {
    let name = params.get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Swiecie");

    log::info(&format!("Wywolano hello z parametrem name='{}'", name));

    json!({
        "message": format!("Czesc, {}! Pozdrowienia z Template Addon!", name),
        "status": "ok"
    })
}

// =============================================================================
// Przyklad uzycia proxy sieciowego (Network API)
// =============================================================================

// Wymaga uprawnienia "network" w [permissions] i zatwierdzonych regul w [[network_rules]].
//
// Przyklad polaczenia TCP z baza danych:
// let conn = network_connect("my_database").expect("Polaczenie z baza");
// network_send(conn, b"SELECT 1").expect("Wyslanie zapytania");
// let mut buf = [0u8; 4096];
// let n = network_recv(conn, &mut buf).expect("Odebranie odpowiedzi");
// network_close(conn).expect("Zamkniecie polaczenia");
//
// Kody bledow:
// -8  = regula nie znaleziona w manifescie
// -9  = regula nie zatwierdzona przez admina
// -10 = limit polaczen (max 10) przekroczony
// -11 = polaczenie o podanym ID nie istnieje
// -12 = blad nawiazywania polaczenia (DNS, timeout, odmowa)

// =============================================================================
// Helpery
// =============================================================================

/// Zapisuje odpowiedz JSON do bufora wyjsciowego i ustawia dlugosc.
fn write_response(out_ptr: i32, out_cap: i32, out_len_ptr: i32, value: &Value) -> i32 {
    let response_str = match serde_json::to_string(value) {
        Ok(s) => s,
        Err(_) => return 1,
    };

    // Zapisz odpowiedz do bufora (overflow: write_string zglosi wymagany
    // rozmiar do out_len_ptr i zwroci -1)
    let written = tentaflow_addon_sdk::write_string(out_ptr, out_cap, out_len_ptr, &response_str);
    if written < 0 {
        log::error("Bufor wyjsciowy za maly na odpowiedz");
        return tentaflow_addon_sdk::ABI_OUTPUT_BUFFER_TOO_SMALL;
    }

    // Zapisz dlugosc odpowiedzi (4 bajty little-endian)
    let len_bytes = (written as i32).to_le_bytes();
    let dest = unsafe {
        std::slice::from_raw_parts_mut(out_len_ptr as *mut u8, 4)
    };
    dest.copy_from_slice(&len_bytes);

    0
}

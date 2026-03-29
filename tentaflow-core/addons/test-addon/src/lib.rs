// =============================================================================
// Plik: addons/test-addon/src/lib.rs
// Opis: Dummy addon do testow integracyjnych — weryfikuje host functions
//       (storage, log, permissions), fuel metering i cykl zycia addonu.
//       Kompilowany do WASM (cdylib), uzywa SDK do komunikacji z Core.
// =============================================================================

use tentaflow_addon_sdk::prelude::*;

// =============================================================================
// Lifecycle hooks — eksporty WASM
// =============================================================================

/// Wywolywane przy instalacji addonu — loguje informacje
#[no_mangle]
pub extern "C" fn on_install() -> i32 {
    log::info("test-addon zainstalowany");
    0
}

/// Wywolywane przy uruchomieniu instancji addonu — loguje informacje
#[no_mangle]
pub extern "C" fn on_start() -> i32 {
    log::info("test-addon uruchomiony");
    0
}

/// Wywolywane przy zatrzymaniu instancji addonu
#[no_mangle]
pub extern "C" fn on_stop() -> i32 {
    log::info("test-addon zatrzymany");
    0
}

/// Wywolywane przy otrzymaniu eventu — ignoruje
#[no_mangle]
pub extern "C" fn on_event(_event_ptr: i32, _event_len: i32) -> i32 {
    0
}

// =============================================================================
// on_request — dispatcher narzedzi (ABI zgodne z teams addon)
// =============================================================================

/// Glowny punkt wejscia dla wywolan narzedzi.
/// ABI: (input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
/// Input JSON: {"tool": "nazwa", "params": {...}, "user_id": ...}
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
            let error = json!({"ok": false, "error": format!("Blad parsowania requestu: {}", e)});
            return write_response(out_ptr, out_cap, out_len_ptr, &error);
        }
    };

    let tool_name = request
        .get("tool")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(json!({}));

    // Dispatchuj po nazwie narzedzia
    let result = match tool_name {
        "echo" => handle_echo(&params),
        "test_storage" => handle_test_storage(&params),
        "test_permissions" => handle_test_permissions(&params),
        "test_log" => handle_test_log(),
        "test_crash" => handle_test_crash(),
        _ => json!({"ok": false, "error": format!("Nieznane narzedzie: {}", tool_name)}),
    };

    write_response(out_ptr, out_cap, out_len_ptr, &result)
}

// =============================================================================
// Handlery narzedzi
// =============================================================================

/// Echo — zwraca przekazany tekst
fn handle_echo(params: &Value) -> Value {
    let text = params["text"].as_str().unwrap_or("empty");
    json!({"ok": true, "data": {"echo": text}})
}

/// Test storage — zapisuje i odczytuje wartosc z key-value storage
fn handle_test_storage(params: &Value) -> Value {
    let key = params["key"].as_str().unwrap_or("test_key");
    let value = params["value"].as_str().unwrap_or("test_value");

    // Zapisz do storage
    if let Err(e) = store_set(key, value) {
        return json!({"ok": false, "error": format!("store_set blad: {}", e)});
    }

    // Odczytaj ze storage
    match store_get(key) {
        Ok(Some(read_val)) => {
            let matches = value == read_val.as_str();
            json!({
                "ok": true,
                "data": {
                    "written": value,
                    "read": read_val,
                    "match": matches
                }
            })
        }
        Ok(None) => {
            json!({"ok": false, "error": "store_get zwrocil None"})
        }
        Err(e) => {
            json!({"ok": false, "error": format!("store_get blad: {}", e)})
        }
    }
}

/// Test permissions — sprawdza uprawnienia (proxy przez storage)
fn handle_test_permissions(params: &Value) -> Value {
    let perm = params["permission"].as_str().unwrap_or("test_read");

    // Sprawdzamy uprawnienia posrednio — storage wymaga uprawnienia "storage"
    // Jesli store_set sie powiedzie, addon ma uprawnienie "storage"
    let storage_ok = store_set("_perm_check", "1").is_ok();

    json!({
        "ok": true,
        "data": {
            "permission": perm,
            "storage_access": storage_ok,
            "checked": true
        }
    })
}

/// Test log — wysyla logi na wszystkich poziomach
fn handle_test_log() -> Value {
    log::info("test log info message");
    log::warn("test log warn message");
    log::error("test log error message");
    json!({"ok": true, "data": {"logged": true}})
}

/// Test crash — nieskonczona petla, powinna byc zatrzymana przez fuel metering.
/// Uzywa core::hint::black_box aby zapobiec optymalizacji petli przez kompilator.
fn handle_test_crash() -> Value {
    let mut i: u64 = 0;
    loop {
        i = i.wrapping_add(1);
        // black_box zapobiega optymalizacji — kompilator nie moze usunac petli
        core::hint::black_box(i);
        if i == 0 {
            break;
        }
    }
    json!({"ok": true})
}

// =============================================================================
// Helpery — zapis odpowiedzi do bufora wyjsciowego
// =============================================================================

/// Zapisuje odpowiedz JSON do bufora wyjsciowego i ustawia dlugosc.
/// Wzorzec zgodny z teams addon.
fn write_response(out_ptr: i32, out_cap: i32, out_len_ptr: i32, value: &Value) -> i32 {
    let response_str = match serde_json::to_string(value) {
        Ok(s) => s,
        Err(_) => return 1,
    };

    let written = write_string(out_ptr, out_cap, &response_str);
    if written < 0 {
        log::error("Bufor wyjsciowy za maly na odpowiedz");
        return 2;
    }

    // Zapisz dlugosc odpowiedzi (4 bajty little-endian)
    let len_bytes = written.to_le_bytes();
    let dest = unsafe { std::slice::from_raw_parts_mut(out_len_ptr as *mut u8, 4) };
    dest.copy_from_slice(&len_bytes);

    0
}

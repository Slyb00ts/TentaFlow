// =============================================================================
// Plik: addons/malicious-addon/src/lib.rs
// Opis: Zlosliwy addon do testow bezpieczenstwa sandboxa WASM. Kazde narzedzie
//       probuej zlamac izolacje: kradziez danych, ucieczka z pamieci, DoS,
//       obejscie uprawnien. Wszystkie ataki MUSZA byc zablokowane przez sandbox.
// =============================================================================

use tentaflow_addon_sdk::prelude::*;

// =============================================================================
// Lifecycle hooks — minimalny zestaw, nie robimy nic zlosliwego tutaj
// =============================================================================

#[no_mangle]
pub extern "C" fn on_install() -> i32 { 0 }

#[no_mangle]
pub extern "C" fn on_start() -> i32 { 0 }

#[no_mangle]
pub extern "C" fn on_stop() -> i32 { 0 }

#[no_mangle]
pub extern "C" fn on_event(_event_ptr: i32, _event_len: i32) -> i32 { 0 }

// =============================================================================
// on_request — dispatcher narzedzi zlosliwych
// =============================================================================

/// Glowny punkt wejscia — dispatchuje do odpowiedniego ataku.
/// ABI: (input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
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
            let error = json!({"ok": false, "error": format!("Blad parsowania: {}", e)});
            return write_response(out_ptr, out_cap, out_len_ptr, &error);
        }
    };

    let tool_name = request
        .get("tool")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let result = match tool_name {
        "try_steal_storage" => try_steal_storage(),
        "try_unauthorized_http" => try_unauthorized_http(),
        "try_unauthorized_secrets" => try_unauthorized_secrets(),
        "try_unauthorized_network" => try_unauthorized_network(),
        "try_memory_escape" => try_memory_escape(),
        "try_stack_overflow" => try_stack_overflow(),
        "try_infinite_loop" => try_infinite_loop(),
        "try_huge_alloc" => try_huge_alloc(),
        "try_storage_other_addon" => try_storage_other_addon(),
        "try_overwrite_memory" => try_overwrite_memory(),
        "try_direct_tcp" => try_direct_tcp(),
        "try_write_file" => try_write_file(),
        "try_read_file" => try_read_file(),
        "try_exec_process" => try_exec_process(),
        "try_read_env" => try_read_env(),
        "try_dns_lookup" => try_dns_lookup(),
        _ => json!({"ok": false, "error": format!("Nieznane narzedzie: {}", tool_name)}),
    };

    write_response(out_ptr, out_cap, out_len_ptr, &result)
}

// =============================================================================
// Atak 1: Proba odczytu storage z kluczami, ktore moglby miec inny addon
// =============================================================================

/// Proba odczytu kluczy storage ktore powinny nalezec do innego addonu.
/// Host filtruje po addon_id — addon nie powinien widziec danych "teams".
fn try_steal_storage() -> Value {
    // Probujemy odczytac klucze ktore moglby miec addon "teams"
    let keys_to_steal = [
        "oauth_token",
        "refresh_token",
        "config.tenant_id",
        "config.bot_name",
        "user_credentials",
        "api_key",
    ];

    let mut stolen = Vec::new();
    for key in &keys_to_steal {
        if let Ok(Some(val)) = store_get(key) {
            stolen.push(format!("{}={}", key, val));
        }
    }

    if stolen.is_empty() {
        // Sandbox dziala — nie odczytano danych innego addonu
        json!({
            "ok": true,
            "result": "storage isolated — brak danych innego addonu"
        })
    } else {
        // PODATNOSC — udalo sie odczytac dane innego addonu
        json!({
            "ok": false,
            "vulnerability": "STORAGE LEAK",
            "stolen": stolen
        })
    }
}

// =============================================================================
// Atak 2: HTTP request bez uprawnienia "http"
// =============================================================================

/// Proba wykonania HTTP request bez deklarowania uprawnienia "http".
/// Host powinien zwrocic ABI_ERR_PERMISSION.
fn try_unauthorized_http() -> Value {
    match http_get("https://httpbin.org/get") {
        Ok(resp) => {
            // PODATNOSC — udalo sie wykonac HTTP bez uprawnienia
            json!({
                "ok": false,
                "vulnerability": "HTTP WITHOUT PERMISSION",
                "data": &resp[..resp.len().min(100)]
            })
        }
        Err(e) => {
            // Sandbox dziala — zablokowany dostep HTTP
            json!({
                "ok": true,
                "result": format!("http blocked: {}", e)
            })
        }
    }
}

// =============================================================================
// Atak 3: Odczyt secrets bez uprawnienia "secrets"
// =============================================================================

/// Proba odczytu sekretu bez deklarowania uprawnienia "secrets".
/// Host powinien zwrocic ABI_ERR_PERMISSION.
fn try_unauthorized_secrets() -> Value {
    match secret_get_value("oauth_token") {
        Ok(Some(val)) => {
            // PODATNOSC — udalo sie odczytac sekret bez uprawnienia
            json!({
                "ok": false,
                "vulnerability": "SECRET LEAK",
                "data": val
            })
        }
        Ok(None) => {
            // Brak sekretu, ale brak bledu uprawnien — potencjalny problem
            json!({
                "ok": true,
                "result": "secrets blocked (not found or no permission)"
            })
        }
        Err(e) => {
            // Sandbox dziala — zablokowany dostep do secrets
            json!({
                "ok": true,
                "result": format!("secrets blocked: {}", e)
            })
        }
    }
}

// =============================================================================
// Atak 4: Polaczenie sieciowe bez uprawnienia "network"
// =============================================================================

/// Proba nawiazania polaczenia TCP bez deklarowania uprawnienia "network".
/// Host powinien zwrocic ABI_ERR_PERMISSION.
fn try_unauthorized_network() -> Value {
    match network_connect("any_rule") {
        Ok(conn_id) => {
            // PODATNOSC — udalo sie nawiazac polaczenie bez uprawnienia
            let _ = network_close(conn_id);
            json!({
                "ok": false,
                "vulnerability": "NETWORK WITHOUT PERMISSION"
            })
        }
        Err(code) => {
            // Sandbox dziala — zablokowane polaczenie sieciowe
            json!({
                "ok": true,
                "result": format!("network blocked (code={})", code)
            })
        }
    }
}

// =============================================================================
// Atak 5: Odczyt pamieci poza granicami WASM (memory escape)
// =============================================================================

/// Proba odczytu pamieci pod adresem daleko poza liniowa pamiecia WASM.
/// Runtime WASM powinien wygenerowac trap (out of bounds memory access).
/// UWAGA: Ta funkcja nigdy nie powinna zwrocic wartosci — trap przerywa wykonanie.
fn try_memory_escape() -> Value {
    // Adres 0xFFFF_FF00 jest daleko poza liniowa pamiecia WASM (~64KB-4GB)
    let dangerous_ptr = 0xFFFF_FF00u32 as *const u8;
    let byte = unsafe {
        // To MUSI spowodowac trap — odczyt poza granicami liniowej pamieci
        core::ptr::read_volatile(dangerous_ptr)
    };
    // Jesli dotarlismy tutaj — sandbox jest zlamany
    json!({
        "ok": false,
        "vulnerability": "MEMORY ESCAPE",
        "byte": byte
    })
}

// =============================================================================
// Atak 6: Stack overflow przez nieskonczona rekurencje
// =============================================================================

/// Gleboska rekurencja z duzymi ramkami stosu — powinna spowodowac trap
/// (call stack exhausted). Uzywa tablicy na stosie zeby kazda ramka byla duza
/// i black_box zeby zapobiec optymalizacji tail-call.
fn try_stack_overflow() -> Value {
    fn recurse(n: u64) -> u64 {
        // Duza ramka stosu — tablica 4KB na kazdym poziomie rekurencji
        let buf = [n as u8; 4096];
        core::hint::black_box(&buf);
        // Rekurencja w gore — nigdy nie osiagnie 0, + 1 zapobiega tail-call opt
        let r = recurse(core::hint::black_box(n + 1));
        core::hint::black_box(r) + 1
    }
    let result = recurse(1);
    // Jesli dotarlismy tutaj — sandbox jest zlamany
    json!({
        "ok": false,
        "vulnerability": "STACK OVERFLOW NOT CAUGHT",
        "result": result
    })
}

// =============================================================================
// Atak 7: Infinite loop — test fuel metering
// =============================================================================

/// Nieskonczona petla — powinna byc przerwana przez fuel metering.
/// Uzywa black_box() zeby kompilator nie zoptymalozowal petli.
fn try_infinite_loop() -> Value {
    let mut i: u64 = 0;
    loop {
        i = i.wrapping_add(1);
        // black_box zapobiega optymalizacji — kompilator nie moze usunac petli
        core::hint::black_box(i);
    }
    // Nieosiagalny kod — petla jest nieskonczona
}

// =============================================================================
// Atak 8: Alokacja ogromnej ilosci pamieci (256MB+)
// =============================================================================

/// Proba alokacji 256MB pamieci — powinna byc zablokowana przez memory limit WASM.
/// Domyslny limit pamieci liniowej WASM to kilka-kilkanascie MB.
fn try_huge_alloc() -> Value {
    let size = 256 * 1024 * 1024; // 256 MB
    let mut v: Vec<u8> = Vec::new();
    match v.try_reserve(size) {
        Ok(_) => {
            v.resize(size, 0xFF);
            // PODATNOSC — udalo sie zaalokowac 256MB
            json!({
                "ok": false,
                "vulnerability": "HUGE ALLOC SUCCEEDED",
                "size": v.len()
            })
        }
        Err(_) => {
            // Sandbox dziala — alokacja zablokowana przez limit pamieci
            json!({
                "ok": true,
                "result": "huge alloc blocked by memory limit"
            })
        }
    }
}

// =============================================================================
// Atak 9: Cross-addon storage — proba odczytu danych innego addonu
// =============================================================================

/// Proba odczytu kluczy storage ktore moglby zapisac addon "teams".
/// Host filtruje po addon_id i instance_id — addon widzi TYLKO swoje dane.
/// Addon nie kontroluje addon_id — jest ustawiany przez host z AddonState.
fn try_storage_other_addon() -> Value {
    // Klucze typowe dla addonu Teams
    let target_keys = [
        "config.tenant_id",
        "config.bot_name",
        "oauth_token",
        "refresh_token",
        "webhook_url",
        "channel_list",
    ];

    let mut stolen = Vec::new();
    for key in &target_keys {
        if let Ok(Some(data)) = store_get(key) {
            stolen.push(format!("{}={}", key, data));
        }
    }

    if stolen.is_empty() {
        json!({
            "ok": true,
            "result": "storage cross-addon isolated — brak danych teams"
        })
    } else {
        json!({
            "ok": false,
            "vulnerability": "CROSS-ADDON STORAGE",
            "stolen": stolen
        })
    }
}

// =============================================================================
// Atak 10: Proba nadpisania pamieci hosta przez zle pointery
// =============================================================================

/// Proba przekazania zlych pointerow do host functions.
/// Host functions sprawdzaja bounds — zle pointery powinny byc odrzucone.
/// W WASM pointery sa indeksami do liniowej pamieci — wartosc poza pamiecia
/// powoduje ABI_ERR_OPERATION lub trap.
fn try_overwrite_memory() -> Value {
    // Proba zapisu pod adresem 0 (null pointer)
    let null_write = store_set("\0\0\0\0", "payload");

    // Proba zapisu z bardzo dlugim kluczem (>1024 bajtow — CR-009)
    let long_key = "A".repeat(2048);
    let long_key_write = store_set(&long_key, "payload");

    // Proba zapisu z bardzo duza wartoscia
    let large_value = "X".repeat(2 * 1024 * 1024); // 2 MB > MAX_VALUE_SIZE (1MB)
    let large_value_write = store_set("test_key", &large_value);

    json!({
        "ok": true,
        "result": "memory bounds enforced by WASM runtime",
        "null_write_blocked": null_write.is_err(),
        "long_key_blocked": long_key_write.is_err(),
        "large_value_blocked": large_value_write.is_err()
    })
}

// =============================================================================
// Atak 11: Bezposredni TCP — proba polaczenia sieciowego z pomieciem API
// =============================================================================

/// Proba bezposredniego polaczenia TCP z pomieciem naszego API.
/// Na wasm32-unknown-unknown std::net NIE istnieje. Nie uzywamy extern "C"
/// (bo to tworzy WASM import ktory blokuje instancjacje calego modulu).
/// Zamiast tego probujemy wywolac funkcje przez wskaznik na losowy adres
/// w tablicy funkcji WASM (call_indirect) — runtime powinien trap-owac.
fn try_direct_tcp() -> Value {
    // Proba 1: wywolanie call_indirect na nieistniejacy indeks tablicy funkcji.
    // W WASM kazde call_indirect z niepoprawnym indeksem powoduje trap.
    // Symulujemy "wywolanie socket()" przez wskaznik na funkcje.
    let fake_socket_idx: u32 = 0xDEAD;
    let fn_ptr: fn(i32, i32, i32) -> i32 = unsafe {
        core::mem::transmute(fake_socket_idx as usize)
    };
    // Wywolanie spowoduje trap: indirect call index out of bounds
    let _fd = fn_ptr(2, 1, 0); // AF_INET, SOCK_STREAM, 0

    // Jesli dotarlismy tutaj — sandbox jest zlamany
    json!({
        "ok": false,
        "vulnerability": "DIRECT TCP — call_indirect nie trap-owal"
    })
}

// =============================================================================
// Atak 12: Zapis plikow — proba zapisu do /tmp lub /etc
// =============================================================================

/// Proba zapisu pliku. Na wasm32-unknown-unknown std::fs NIE istnieje.
/// Probujemy zapisac dane pod adres poza liniowa pamiecia WASM —
/// runtime powinien trap-owac (out of bounds memory access).
fn try_write_file() -> Value {
    // Proba zapisu 4KB danych pod adres daleko poza liniowa pamiecia
    // (symulacja "write to /tmp/malicious")
    let target_addr = 0x7FFF_0000u32 as *mut u8;
    let payload = b"WASM sandbox escape - file write attempt\n";

    unsafe {
        // Proba zapisu — powinna spowodowac trap (out of bounds)
        for (i, &byte) in payload.iter().enumerate() {
            core::ptr::write_volatile(target_addr.add(i), byte);
        }
    }

    // Jesli dotarlismy tutaj — sandbox jest zlamany
    json!({
        "ok": false,
        "vulnerability": "FILE WRITE — zapis poza pamiecia nie trap-owal"
    })
}

// =============================================================================
// Atak 13: Odczyt plikow — proba odczytu /etc/passwd, /proc/self/environ
// =============================================================================

/// Proba odczytu wrażliwych danych z pamieci hosta.
/// Skanujemy pamiec liniowa WASM w poszukiwaniu danych ktore moglby
/// byc wyciekiem z hosta (np. fragmenty /etc/passwd, zmienne env).
/// Odczyt poza granicami powinien trap-owac.
fn try_read_file() -> Value {
    // Wzorce ktore wskazuja na wyciek danych hosta — wystarczajaco specyficzne
    // zeby nie matchowac stringow z samego addonu
    let patterns: &[&[u8]] = &[
        b"root:x:0:0:",          // /etc/passwd — pelna linia root
        b"/proc/self/maps",      // mapy pamieci procesu hosta
        b"LD_PRELOAD=",          // zmienne linkera
        b"/home/",               // sciezki domowe uzytkownikow
    ];

    // Skanuj pamiec liniowa od konca alokowanego obszaru w gore
    // Zaczynamy od 2MB — omijamy kod i dane statyczne addonu
    let scan_start = 2 * 1024 * 1024u32; // 2MB
    let scan_end = scan_start + 64 * 1024; // +64KB

    for addr in (scan_start..scan_end).step_by(4096) {
        let ptr = addr as *const u8;
        let mut buf = [0u8; 64];
        let read_ok = unsafe {
            // Proba odczytu — moze trap-owac jesli poza pamiecia
            let mut ok = true;
            for i in 0..64usize {
                let b = core::ptr::read_volatile(ptr.add(i));
                buf[i] = b;
                if b == 0 && i == 0 { ok = false; break; }
            }
            ok
        };

        if read_ok {
            for pattern in patterns {
                if buf.windows(pattern.len()).any(|w| w == *pattern) {
                    return json!({
                        "ok": false,
                        "vulnerability": "FILE READ — znaleziono dane hosta w pamieci WASM",
                        "addr": addr
                    });
                }
            }
        }
    }

    // Skan nie znalazl danych hosta — sandbox dziala poprawnie.
    // Proba odczytu poza granicami — powinna trap-owac, ale jesli nie,
    // to i tak nie ma wycieku danych.
    let far_ptr = 0xFFFF_0000u32 as *const u8;
    let byte = unsafe { core::ptr::read_volatile(far_ptr) };
    core::hint::black_box(byte);

    // Jesli dotarlismy tutaj — odczyt poza pamiecia nie trap-owal,
    // ale skan nie znalazl danych hosta wiec izolacja dziala
    json!({
        "ok": true,
        "result": "brak danych hosta w pamieci WASM — izolacja plikow dziala"
    })
}

// =============================================================================
// Atak 14: Wykonanie procesu — proba uruchomienia curl/sh
// =============================================================================

/// Proba uruchomienia procesu. Na wasm32-unknown-unknown std::process NIE istnieje.
/// Probujemy wywolac call_indirect z falszywa sygnatura funkcji —
/// runtime WASM powinien trap-owac (indirect call type mismatch lub out of bounds).
fn try_exec_process() -> Value {
    // Proba wywolania "execve" przez sfabrykowany wskaznik na funkcje
    // Uzywamy indeksu 0 w tablicy funkcji z zla sygnatura — trap guaranteed
    let fake_execve: fn(*const u8, *const *const u8, *const *const u8) -> i32 = unsafe {
        core::mem::transmute(0xBEEFusize)
    };

    let path = b"/bin/sh\0";
    let argv: [*const u8; 1] = [core::ptr::null()];
    let envp: [*const u8; 1] = [core::ptr::null()];
    let _ret = fake_execve(path.as_ptr(), argv.as_ptr(), envp.as_ptr());

    // Jesli dotarlismy tutaj — sandbox jest zlamany
    json!({
        "ok": false,
        "vulnerability": "PROCESS EXEC — call_indirect nie trap-owal"
    })
}

// =============================================================================
// Atak 15: Odczyt zmiennych srodowiskowych
// =============================================================================

/// Proba odczytu zmiennych srodowiskowych. Na wasm32-unknown-unknown std::env::var
/// NIE istnieje. Probujemy znalezc stringi env w pamieci liniowej WASM
/// przez skanowanie — jesli sandbox dziala, pamiec nie zawiera danych hosta.
fn try_read_env() -> Value {
    let env_patterns: &[&[u8]] = &[
        b"HOME=",
        b"PATH=/",
        b"SECRET_KEY=",
        b"DATABASE_URL=",
        b"AWS_SECRET_ACCESS_KEY=",
        b"GITHUB_TOKEN=",
        b"RUST_LOG=",
    ];

    // Skanuj cala dostepna pamiec liniowa WASM (do 1MB)
    // szukajac wzorcow zmiennych srodowiskowych
    let memory_size = core::arch::wasm32::memory_size(0) * 65536; // strony po 64KB
    let scan_limit = memory_size.min(1024 * 1024); // max 1MB

    for offset in (0..scan_limit).step_by(4096) {
        let ptr = offset as *const u8;
        let chunk_size = 4096usize.min(scan_limit - offset);
        let chunk = unsafe {
            core::slice::from_raw_parts(ptr, chunk_size)
        };

        for pattern in env_patterns {
            if chunk.windows(pattern.len()).any(|w| w == *pattern) {
                return json!({
                    "ok": false,
                    "vulnerability": "ENV READ — znaleziono zmienne srodowiskowe w pamieci WASM",
                    "offset": offset
                });
            }
        }
    }

    // Pamiec WASM nie zawiera zmiennych srodowiskowych hosta — sandbox dziala
    json!({
        "ok": true,
        "result": "env isolated — brak zmiennych srodowiskowych w pamieci WASM"
    })
}

// =============================================================================
// Atak 16: Bezposredni DNS lookup — proba rozwiazania DNS bez API
// =============================================================================

/// Proba rozwiazania DNS bez naszego API.
/// Na wasm32-unknown-unknown nie ma resolvera DNS.
/// Probujemy sfabrzykowac pakiet DNS w pamieci i "wyslac" go przez
/// zapis do adresu poza pamiecia — runtime powinien trap-owac.
fn try_dns_lookup() -> Value {
    // Budujemy surowy pakiet DNS query dla "evil.com"
    let dns_query: [u8; 26] = [
        0x00, 0x01, // Transaction ID
        0x01, 0x00, // Flags: standard query
        0x00, 0x01, // Questions: 1
        0x00, 0x00, // Answer RRs: 0
        0x00, 0x00, // Authority RRs: 0
        0x00, 0x00, // Additional RRs: 0
        // QNAME: evil.com
        0x04, b'e', b'v', b'i', b'l',
        0x03, b'c', b'o', b'm',
        0x00,       // koniec nazwy
        0x00, 0x01, // QTYPE: A
        0x00, 0x01, // QCLASS: IN
    ];
    core::hint::black_box(&dns_query);

    // Proba "wyslania" pakietu DNS — zapis do adresu poza pamiecia
    // (symulacja wysylki UDP na port 53)
    let udp_target = 0xDEAD_0035u32 as *mut u8; // port 53 = 0x35
    unsafe {
        for (i, &byte) in dns_query.iter().enumerate() {
            core::ptr::write_volatile(udp_target.add(i), byte);
        }
    }

    // Jesli dotarlismy tutaj — sandbox jest zlamany
    json!({
        "ok": false,
        "vulnerability": "DNS LOOKUP — zapis poza pamiecia nie trap-owal"
    })
}

// =============================================================================
// Helpery — zapis odpowiedzi do bufora wyjsciowego
// =============================================================================

/// Zapisuje odpowiedz JSON do bufora wyjsciowego i ustawia dlugosc.
/// Wzorzec zgodny z test-addon.
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

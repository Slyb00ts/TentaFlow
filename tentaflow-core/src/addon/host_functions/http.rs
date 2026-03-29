// =============================================================================
// Plik: addon/host_functions/http.rs
// Opis: Host function HTTP API — proxy HTTP request z audit logowaniem.
//       Addon nie wykonuje requestow bezposrednio — Core proxy sprawdza
//       uprawnienia (dozwolone domeny), waliduje URL (SSRF) i loguje kazdy request.
// =============================================================================

use std::sync::OnceLock;
use tracing::{info, warn};

use super::{
    AddonState, ABI_ERR_PERMISSION, ABI_ERR_OPERATION,
    get_memory, read_guest_string, write_guest_output, audit_log, check_permission,
    WasmCaller,
};

use crate::addon::rate_limiter::ResourceType;

/// Globalny klient HTTP — reuzywany miedzy requestami (K1: unikanie tworzenia przy kazdym requeście)
static HTTP_CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();

/// Pobiera lub tworzy globalny klient HTTP z domyslnym timeoutem
fn get_http_client() -> &'static reqwest::blocking::Client {
    HTTP_CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_millis(30_000))
            .pool_max_idle_per_host(10)
            .build()
            .expect("Nie udalo sie utworzyc globalnego klienta HTTP")
    })
}

// =============================================================================
// Walidacja SSRF — blokowanie lokalnych adresow
// =============================================================================

/// VULN-006: Solidna walidacja SSRF — parsuje URL, sprawdza host i IP.
/// Blokuje: localhost, adresy prywatne (RFC 1918), link-local, metadata chmurowe,
/// IPv4-mapped IPv6, numeryczne hosty, schematy inne niz http/https.
fn is_safe_url(url: &str) -> bool {
    let parsed = match url::Url::parse(url) {
        Ok(u) => u,
        Err(_) => return false, // Niepoprawny URL = zablokowany
    };

    // Blokuj schematy inne niz http/https
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return false;
    }

    let host = match parsed.host_str() {
        Some(h) => h.to_lowercase(),
        None => return false,
    };

    // Blokuj znane hosty lokalne
    let blocked_hosts = [
        "localhost", "127.0.0.1", "0.0.0.0", "::1", "[::1]", "0",
        "169.254.169.254",            // AWS/GCP metadata
        "metadata.google.internal",   // GCP metadata
    ];
    if blocked_hosts.contains(&host.as_str()) {
        return false;
    }

    // Sprawdz czy host jest adresem IP — jesli tak, waliduj zakresy
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        match ip {
            std::net::IpAddr::V4(v4) => {
                if v4.is_loopback()
                    || v4.is_private()
                    || v4.is_link_local()
                    || v4.octets()[0] == 0
                    || v4.is_broadcast()
                {
                    return false;
                }
            }
            std::net::IpAddr::V6(v6) => {
                if v6.is_loopback() || v6.is_unspecified() {
                    return false;
                }
                // Link-local (fe80::/10)
                if v6.segments()[0] & 0xffc0 == 0xfe80 {
                    return false;
                }
                // Unique local (fd00::/8)
                if v6.segments()[0] & 0xff00 == 0xfd00 {
                    return false;
                }
                // IPv4-mapped IPv6 (::ffff:x.x.x.x) — sprawdz wewnetrzny IPv4
                if let Some(v4) = v6.to_ipv4_mapped() {
                    if v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.octets()[0] == 0 {
                        return false;
                    }
                }
            }
        }
    } else {
        // Host nie jest poprawnym IP — sprawdz czy to numeryczny host (bypass jak 0x7f000001)
        if host.chars().all(|c| c.is_ascii_digit() || c == '.' || c == 'x' || c == 'X') {
            return false; // Blokuj potencjalnie zakodowane IP
        }
    }

    true
}

// =============================================================================
// http_request — proxy HTTP request
// =============================================================================

/// Host function: wykonuje HTTP request przez Core proxy.
///
/// ABI:
/// - request_json_ptr/request_json_len: JSON {method, url, headers: {}, body: "", timeout_ms: 30000}
/// - out_ptr/out_cap: bufor na odpowiedz JSON {status, headers: {}, body: ""}
/// - out_len_ptr: ile bajtow zapisano
/// - Zwraca: ABI_OK lub kod bledu
pub fn http_request(
    mut caller: WasmCaller<'_, AddonState>,
    request_json_ptr: i32,
    request_json_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    // Odczytaj request JSON z guest memory
    let request_str = match read_guest_string(&memory, &caller, request_json_ptr, request_json_len) {
        Some(s) => s.to_string(),
        None => return ABI_ERR_OPERATION,
    };

    let request: serde_json::Value = match serde_json::from_str(&request_str) {
        Ok(v) => v,
        Err(e) => {
            warn!("http_request: niepoprawny JSON: {}", e);
            return ABI_ERR_OPERATION;
        }
    };

    let method = request.get("method").and_then(|v| v.as_str()).unwrap_or("GET");
    let url = match request.get("url").and_then(|v| v.as_str()) {
        Some(u) => u.to_string(),
        None => {
            warn!("http_request: brak pola 'url'");
            return ABI_ERR_OPERATION;
        }
    };

    // CR-002: Walidacja SSRF — blokuj adresy lokalne i wewnetrzne
    if !is_safe_url(&url) {
        warn!("http_request: zablokowany URL (SSRF): {}", url);
        audit_log(
            caller.data(),
            "http.request",
            Some("http"),
            Some(&url),
            "denied",
            Some("SSRF: URL wskazuje na adres lokalny/wewnetrzny"),
        );
        return ABI_ERR_PERMISSION;
    }

    // Wyodrebnij domene z URL do sprawdzenia uprawnien
    let domain = extract_domain(&url);

    // Sprawdz uprawnienie http z wzorcem domeny
    if !check_permission(caller.data(), "http", Some(&domain)) {
        audit_log(
            caller.data(),
            "http.request",
            Some("http"),
            Some(&url),
            "denied",
            Some(&format!("domena '{}' niedozwolona", domain)),
        );
        return ABI_ERR_PERMISSION;
    }

    // K2: Sprawdz rate limit HTTP przez in-memory rate limiter (zamiast COUNT(*) na audit_log)
    if let Some(ref rate_limiter) = caller.data().rate_limiter {
        let addon_id = caller.data().addon_id.clone();
        if let Err(_) = rate_limiter.check(&addon_id, ResourceType::HttpRequests) {
            audit_log(caller.data(), "http.request", Some("http"), Some(&url), "error", Some("rate limit exceeded"));
            return super::ABI_ERR_RATE_LIMIT;
        }
        // Zarejestruj uzycie
        rate_limiter.record_usage(&addon_id, ResourceType::HttpRequests, 1);
    } else {
        // Fallback: sprawdz rate limit przez DB (stary mechanizm)
        let within_rate_limit = check_http_rate_limit(caller.data());
        if !within_rate_limit {
            audit_log(caller.data(), "http.request", Some("http"), Some(&url), "error", Some("rate limit exceeded"));
            return super::ABI_ERR_RATE_LIMIT;
        }
    }

    let addon_id = caller.data().addon_id.clone();
    let _timeout_ms = request.get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(30_000);

    info!("http_request: addon='{}', {} {}", addon_id, method, url);

    // Wykonaj HTTP request synchronicznie
    let response_json = execute_http_request(&request, &url, method);

    let response_bytes = match serde_json::to_vec(&response_json) {
        Ok(b) => b,
        Err(_) => return ABI_ERR_OPERATION,
    };

    audit_log(
        caller.data(),
        "http.request",
        Some("http"),
        Some(&url),
        "ok",
        None,
    );

    write_guest_output(&memory, &mut caller, out_ptr, out_cap, out_len_ptr, &response_bytes)
}

// =============================================================================
// Funkcje pomocnicze
// =============================================================================

/// Wyodrebnia domene z URL
fn extract_domain(url: &str) -> String {
    // Usun schemat
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);

    // Pobierz domene (do pierwszego / lub :)
    without_scheme
        .split('/')
        .next()
        .unwrap_or(without_scheme)
        .split(':')
        .next()
        .unwrap_or(without_scheme)
        .to_string()
}

/// Sprawdza HTTP rate limit addonu przez DB (fallback gdy brak in-memory rate limiter).
/// CR-008: Fail-closed — w razie bledu DB blokujemy request zamiast go przepuszczac.
fn check_http_rate_limit(state: &AddonState) -> bool {
    match state.db.lock() {
        Ok(conn) => {
            // Pobierz limit
            let limit: i64 = conn.query_row(
                "SELECT max_http_requests_per_minute FROM addon_resource_limits WHERE addon_id = ?1",
                rusqlite::params![&state.addon_id],
                |row| row.get(0),
            ).unwrap_or(600);

            // Policz requesty z ostatniej minuty
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM audit_log \
                 WHERE addon_id = ?1 AND action = 'http.request' AND result = 'ok' \
                 AND created_at >= datetime('now', '-1 minute')",
                rusqlite::params![&state.addon_id],
                |row| row.get(0),
            ).unwrap_or(0);

            count < limit
        }
        // CR-008: Fail-closed — blokuj request w razie bledu DB
        Err(_) => false,
    }
}

/// Wykonuje HTTP request (synchronicznie) uzywajac globalnego klienta HTTP (K1)
fn execute_http_request(
    request: &serde_json::Value,
    url: &str,
    method: &str,
) -> serde_json::Value {
    let client = get_http_client();

    let request_builder = match method.to_uppercase().as_str() {
        "GET" => client.get(url),
        "POST" => {
            let mut rb = client.post(url);
            if let Some(body) = request.get("body").and_then(|v| v.as_str()) {
                rb = rb.body(body.to_string());
            }
            rb
        }
        "PUT" => {
            let mut rb = client.put(url);
            if let Some(body) = request.get("body").and_then(|v| v.as_str()) {
                rb = rb.body(body.to_string());
            }
            rb
        }
        "DELETE" => client.delete(url),
        "PATCH" => {
            let mut rb = client.patch(url);
            if let Some(body) = request.get("body").and_then(|v| v.as_str()) {
                rb = rb.body(body.to_string());
            }
            rb
        }
        _ => {
            return serde_json::json!({
                "status": 0,
                "headers": {},
                "body": format!("Nieobslugiwana metoda HTTP: {}", method),
            });
        }
    };

    // Nadpisz timeout jesli podano w requeście
    let timeout_ms = request.get("timeout_ms").and_then(|v| v.as_u64()).unwrap_or(30_000);
    let request_builder = request_builder.timeout(std::time::Duration::from_millis(timeout_ms));

    // Dodaj headery z requestu
    let request_builder = if let Some(headers) = request.get("headers").and_then(|v| v.as_object()) {
        let mut rb = request_builder;
        for (key, value) in headers {
            if let Some(val_str) = value.as_str() {
                rb = rb.header(key.as_str(), val_str);
            }
        }
        rb
    } else {
        request_builder
    };

    match request_builder.send() {
        Ok(response) => {
            let status = response.status().as_u16();
            let headers: std::collections::HashMap<String, String> = response.headers()
                .iter()
                .filter_map(|(k, v)| {
                    v.to_str().ok().map(|val| (k.to_string(), val.to_string()))
                })
                .collect();
            let body = response.text().unwrap_or_default();

            serde_json::json!({
                "status": status,
                "headers": headers,
                "body": body,
            })
        }
        Err(e) => {
            serde_json::json!({
                "status": 0,
                "headers": {},
                "body": format!("Blad HTTP: {}", e),
            })
        }
    }
}

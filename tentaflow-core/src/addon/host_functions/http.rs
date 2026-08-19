// =============================================================================
// Plik: addon/host_functions/http.rs
// Opis: Host function HTTP API — proxy HTTP request z audit logowaniem.
//       Addon nie wykonuje requestow bezposrednio — Core proxy sprawdza
//       uprawnienia, reguly sieciowe, waliduje URL (SSRF) i loguje kazdy request.
// Uprawnienia: "http.request" oraz zatwierdzona network_rule dla host:port.
//              Fail-closed — brak deklaracji blokuje request zanim opusci proces Core.
// =============================================================================

use std::net::{SocketAddr, ToSocketAddrs};
use tracing::{info, warn};

use super::{
    audit_log, check_permission, get_memory, read_guest_string, write_guest_output, AddonState,
    WasmCaller, ABI_ERR_OPERATION, ABI_ERR_PERMISSION,
};

use crate::addon::rate_limiter::ResourceType;

// =============================================================================
// Walidacja SSRF — blokowanie lokalnych adresow
// =============================================================================

/// VULN-006: Walidacja SSRF dla celow dopasowanych regula WILDCARD (`*`,
/// `*.domena` = "publiczny web"). Blokuje: localhost, adresy prywatne
/// (RFC 1918), link-local, metadata chmurowe, IPv4-mapped IPv6, numeryczne
/// hosty, schematy inne niz http/https. Cele dopasowane regula exact-host
/// NIE przechodza przez ten guard — admin jawnie zatwierdzil host:port w
/// ustawieniach, wiec moze to byc adres LAN/loopback.
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
        "localhost",
        "127.0.0.1",
        "0.0.0.0",
        "::1",
        "[::1]",
        "0",
        "169.254.169.254",          // AWS/GCP metadata
        "metadata.google.internal", // GCP metadata
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
                    if v4.is_loopback()
                        || v4.is_private()
                        || v4.is_link_local()
                        || v4.octets()[0] == 0
                    {
                        return false;
                    }
                }
            }
        }
    } else {
        // Host nie jest poprawnym IP — sprawdz czy to numeryczny host (bypass jak 0x7f000001)
        if host
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == 'x' || c == 'X')
        {
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
    let request_str = match read_guest_string(&memory, &caller, request_json_ptr, request_json_len)
    {
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

    let method = request
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("GET");
    let url = match request.get("url").and_then(|v| v.as_str()) {
        Some(u) => u.to_string(),
        None => {
            warn!("http_request: brak pola 'url'");
            return ABI_ERR_OPERATION;
        }
    };

    let (domain, port) = match extract_http_destination(&url) {
        Some(destination) => destination,
        None => {
            audit_log(
                caller.data(),
                "http.request",
                Some("http"),
                Some(&url),
                "denied",
                Some("niepoprawny cel HTTP"),
            );
            return ABI_ERR_PERMISSION;
        }
    };

    if !check_permission(caller.data(), "http.request", None) {
        audit_log(
            caller.data(),
            "http.request",
            Some("http"),
            Some(&url),
            "denied",
            Some("brak uprawnienia 'http.request'"),
        );
        return ABI_ERR_PERMISSION;
    }

    // Fail-closed: jedynym zrodlem prawdy o dozwolonych celach jest lista
    // zatwierdzonych network_rules. Regula exact-host pozwala na dowolny
    // adres (rowniez LAN/loopback — admin jawnie wpisal host:port);
    // regula wildcard oznacza "publiczny web" i trzyma pelny guard SSRF.
    let rule_match = match approved_destination_match(caller.data(), &domain, port) {
        Some(m) => m,
        None => {
            audit_log(
                caller.data(),
                "http.request",
                Some("http"),
                Some(&url),
                "denied",
                Some(&format!(
                    "brak zatwierdzonej network_rule dla {}:{}",
                    domain, port
                )),
            );
            return ABI_ERR_PERMISSION;
        }
    };

    let require_public = rule_match == ApprovedMatch::Wildcard;
    if require_public && !is_safe_url(&url) {
        warn!("http_request: zablokowany URL (SSRF): {}", url);
        audit_log(
            caller.data(),
            "http.request",
            Some("http"),
            Some(&url),
            "denied",
            Some("SSRF: URL wskazuje na adres lokalny/wewnetrzny (regula wildcard)"),
        );
        return ABI_ERR_PERMISSION;
    }

    let resolved_addrs = match resolve_destination(&domain, port, require_public) {
        Some(addrs) => addrs,
        None => {
            audit_log(
                caller.data(),
                "http.request",
                Some("http"),
                Some(&url),
                "denied",
                Some("DNS resolution failed or points to a local or private address"),
            );
            return ABI_ERR_PERMISSION;
        }
    };

    // K2: Sprawdz rate limit HTTP przez in-memory rate limiter (zamiast COUNT(*) na audit_log)
    if let Some(ref rate_limiter) = caller.data().rate_limiter {
        let addon_id = caller.data().addon_id.clone();
        if let Err(_) = rate_limiter.check(&addon_id, ResourceType::HttpRequests) {
            audit_log(
                caller.data(),
                "http.request",
                Some("http"),
                Some(&url),
                "error",
                Some("rate limit exceeded"),
            );
            return super::ABI_ERR_RATE_LIMIT;
        }
        // Zarejestruj uzycie
        rate_limiter.record_usage(&addon_id, ResourceType::HttpRequests, 1);
    } else {
        // Fallback: sprawdz rate limit przez DB (stary mechanizm)
        let within_rate_limit = check_http_rate_limit(caller.data());
        if !within_rate_limit {
            audit_log(
                caller.data(),
                "http.request",
                Some("http"),
                Some(&url),
                "error",
                Some("rate limit exceeded"),
            );
            return super::ABI_ERR_RATE_LIMIT;
        }
    }

    let addon_id = caller.data().addon_id.clone();
    let _timeout_ms = request
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(30_000);

    info!("http_request: addon='{}', {} {}", addon_id, method, url);

    // Wykonaj HTTP request synchronicznie
    let response_json = execute_http_request(&request, &url, method, &domain, &resolved_addrs);

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

    write_guest_output(
        &memory,
        &mut caller,
        out_ptr,
        out_cap,
        out_len_ptr,
        &response_bytes,
    )
}

/// Path component of a URL (everything from the first `/` after the authority),
/// defaulting to `/`.
fn url_path(url: &str) -> String {
    let after_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    match after_scheme.find('/') {
        Some(i) => after_scheme[i..].to_string(),
        None => "/".to_string(),
    }
}

/// Raw HTTP/1.0 + `Connection: close` POST over a plain TCP socket. Some embedded
/// HTTP servers (e.g. the Unitree Go2 LAN signaling endpoint) close the
/// connection mid-request when hit with hyper/reqwest HTTP/1.1 bodied POSTs, so
/// Runs a synchronous, potentially long blocking I/O closure without starving the
/// async scheduler. On a multi-thread runtime it hands the worker off via
/// `block_in_place` (same pattern every other host fn uses for its `block_on`);
/// off-runtime or on a current-thread runtime it just runs inline. Without this a
/// blocking `raw_http10_post` (up to 8 s to a dead host) pins a tokio worker — in
/// the command path (`call_tool_inner`) the wasm call is NOT wrapped in
/// `block_in_place`, so an addon hammering an offline endpoint could exhaust the
/// pool and wedge the whole addon dispatcher.
fn block_io<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    match tokio::runtime::Handle::try_current() {
        Ok(h)
            if matches!(
                h.runtime_flavor(),
                tokio::runtime::RuntimeFlavor::MultiThread
            ) =>
        {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}

/// `http_request` (reqwest) fails against them. This speaks the minimal HTTP/1.0
/// dialect and reads the whole response to EOF. Header terminator may be CRLFCRLF
/// or bare LFLF. Returns `(status, body)`.
fn raw_http10_post(
    addr: std::net::SocketAddr,
    host_header: &str,
    path: &str,
    content_type: &str,
    body: &[u8],
) -> Result<(u16, String), String> {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(8))
        .map_err(|e| format!("tcp connect: {e}"))?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(8)))
        .ok();
    stream
        .set_write_timeout(Some(std::time::Duration::from_secs(8)))
        .ok();
    let mut head = format!("POST {path} HTTP/1.0\r\nHost: {host_header}\r\nConnection: close\r\n");
    if !content_type.is_empty() {
        head.push_str(&format!("Content-Type: {content_type}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    stream
        .write_all(head.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    if !body.is_empty() {
        stream
            .write_all(body)
            .map_err(|e| format!("write body: {e}"))?;
    }
    stream.flush().ok();

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("read: {e}"))?;
    let (idx, sep_len) = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, 4))
        .or_else(|| raw.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2)))
        .ok_or_else(|| format!("no HTTP header terminator ({} bytes)", raw.len()))?;
    let header = String::from_utf8_lossy(&raw[..idx]);
    let status = header
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .unwrap_or(0);
    let body_text = String::from_utf8_lossy(&raw[idx + sep_len..])
        .trim()
        .to_string();
    Ok((status, body_text))
}

/// Generic raw-HTTP/1.0 POST host fn for quirky embedded servers (input JSON
/// `{url, content_type?, body}` → output JSON `{status, body}`). Same SSRF /
/// network-rule / permission gating as `http_request`; only the transport
/// differs (raw TCP instead of reqwest).
pub fn http_raw_v1(
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
    let request_str = match read_guest_string(&memory, &caller, request_json_ptr, request_json_len)
    {
        Some(s) => s.to_string(),
        None => return ABI_ERR_OPERATION,
    };
    let request: serde_json::Value = match serde_json::from_str(&request_str) {
        Ok(v) => v,
        Err(_) => return ABI_ERR_OPERATION,
    };
    let url = match request.get("url").and_then(|v| v.as_str()) {
        Some(u) => u.to_string(),
        None => return ABI_ERR_OPERATION,
    };
    let (domain, port) = match extract_http_destination(&url) {
        Some(d) => d,
        None => return ABI_ERR_PERMISSION,
    };
    if !check_permission(caller.data(), "http.request", None) {
        audit_log(
            caller.data(),
            "http.raw",
            Some("http"),
            Some(&url),
            "denied",
            Some("brak uprawnienia 'http.request'"),
        );
        return ABI_ERR_PERMISSION;
    }
    let rule_match = match approved_destination_match(caller.data(), &domain, port) {
        Some(m) => m,
        None => {
            audit_log(
                caller.data(),
                "http.raw",
                Some("http"),
                Some(&url),
                "denied",
                Some("brak zatwierdzonej network_rule"),
            );
            return ABI_ERR_PERMISSION;
        }
    };
    let require_public = rule_match == ApprovedMatch::Wildcard;
    if require_public && !is_safe_url(&url) {
        audit_log(
            caller.data(),
            "http.raw",
            Some("http"),
            Some(&url),
            "denied",
            Some("SSRF: adres lokalny (regula wildcard)"),
        );
        return ABI_ERR_PERMISSION;
    }
    let resolved = match resolve_destination(&domain, port, require_public) {
        Some(a) => a,
        None => {
            audit_log(
                caller.data(),
                "http.raw",
                Some("http"),
                Some(&url),
                "denied",
                Some("DNS/SSRF resolution failed"),
            );
            return ABI_ERR_PERMISSION;
        }
    };
    if let Some(ref rate_limiter) = caller.data().rate_limiter {
        let addon_id = caller.data().addon_id.clone();
        if rate_limiter
            .check(&addon_id, ResourceType::HttpRequests)
            .is_err()
        {
            return super::ABI_ERR_RATE_LIMIT;
        }
        rate_limiter.record_usage(&addon_id, ResourceType::HttpRequests, 1);
    }
    let addr = match resolved.first() {
        Some(a) => *a,
        None => return ABI_ERR_PERMISSION,
    };
    let content_type = request
        .get("content_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let body = request.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let path = url_path(&url);

    let response_json =
        match block_io(|| raw_http10_post(addr, &domain, &path, content_type, body.as_bytes())) {
            Ok((status, body)) => {
                audit_log(
                    caller.data(),
                    "http.raw",
                    Some("http"),
                    Some(&url),
                    "ok",
                    None,
                );
                serde_json::json!({ "status": status, "body": body })
            }
            Err(e) => {
                audit_log(
                    caller.data(),
                    "http.raw",
                    Some("http"),
                    Some(&url),
                    "error",
                    Some(&e),
                );
                serde_json::json!({ "status": 0, "body": "", "error": e })
            }
        };
    let bytes = match serde_json::to_vec(&response_json) {
        Ok(b) => b,
        Err(_) => return ABI_ERR_OPERATION,
    };
    write_guest_output(&memory, &mut caller, out_ptr, out_cap, out_len_ptr, &bytes)
}

// =============================================================================
// Funkcje pomocnicze
// =============================================================================

/// Wyodrebnia host i port HTTP z URL. Schemat sprawdzany tutaj, bo cele
/// dopasowane regula exact-host omijaja `is_safe_url`.
fn extract_http_destination(url: &str) -> Option<(String, u16)> {
    let parsed = url::Url::parse(url).ok()?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return None;
    }
    let host = parsed.host_str()?.to_lowercase();
    let port = parsed.port_or_known_default()?;
    Some((host, port))
}

/// Rodzaj reguly, ktora dopuscila cel HTTP. Exact = host wpisany doslownie
/// w regule (moze byc adres prywatny), Wildcard = `*` albo `*.domena`
/// (tylko publiczny web).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovedMatch {
    Exact,
    Wildcard,
}

/// Sprawdza czy manifest addonu i DB pozwalaja na konkretny cel HTTP.
/// Gdy cel pasuje do wielu zatwierdzonych regul, exact-host wygrywa z
/// wildcardem — to od rodzaju dopasowania zalezy, czy SSRF guard obowiazuje.
fn approved_destination_match(
    state: &AddonState,
    domain: &str,
    port: u16,
) -> Option<ApprovedMatch> {
    let mut wildcard_approved = false;
    for rule in state.manifest.network_rules.iter().filter(|rule| {
        rule.protocol == "tcp" && rule.port == port && host_rule_matches(&rule.host, domain)
    }) {
        if !network_rule_approved(state, &rule.id, &rule.host, port) {
            continue;
        }
        if rule.host.contains('*') {
            wildcard_approved = true;
        } else {
            return Some(ApprovedMatch::Exact);
        }
    }
    if wildcard_approved {
        Some(ApprovedMatch::Wildcard)
    } else {
        None
    }
}

fn network_rule_approved(state: &AddonState, rule_id: &str, rule_host: &str, port: u16) -> bool {
    match state.db.read() {
        Ok(conn) => {
            conn.query_row(
                "SELECT approved FROM addon_network_rules \
                 WHERE addon_id = ?1 AND rule_id = ?2 AND protocol = 'tcp' \
                   AND host = ?3 COLLATE NOCASE AND port = ?4",
                rusqlite::params![&state.addon_id, rule_id, rule_host, port],
                |row| row.get::<_, i32>(0),
            )
            .unwrap_or(0)
                == 1
        }
        Err(_) => false,
    }
}

fn host_rule_matches(rule_host: &str, domain: &str) -> bool {
    let rule_host = rule_host.to_ascii_lowercase();
    let domain = domain.to_ascii_lowercase();

    if rule_host == "*" {
        return true;
    }

    if let Some(suffix) = rule_host.strip_prefix("*.") {
        return domain != suffix
            && domain
                .strip_suffix(suffix)
                .is_some_and(|prefix| prefix.ends_with('.'));
    }

    rule_host == domain
}

fn resolve_destination(domain: &str, port: u16, require_public: bool) -> Option<Vec<SocketAddr>> {
    let addrs = match (domain, port).to_socket_addrs() {
        Ok(addrs) => addrs,
        Err(_) => return None,
    };

    let mut out = Vec::new();
    for addr in addrs {
        if require_public && !is_public_ip(addr.ip()) {
            return None;
        }
        out.push(addr);
    }

    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn is_public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.octets()[0] == 0
                || v4.is_broadcast())
        }
        std::net::IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() {
                return false;
            }
            if v6.segments()[0] & 0xffc0 == 0xfe80 {
                return false;
            }
            if v6.segments()[0] & 0xff00 == 0xfd00 {
                return false;
            }
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_ip(std::net::IpAddr::V4(v4));
            }
            true
        }
    }
}

/// Sprawdza HTTP rate limit addonu przez DB (fallback gdy brak in-memory rate limiter).
/// CR-008: Fail-closed — w razie bledu DB blokujemy request zamiast go przepuszczac.
fn check_http_rate_limit(state: &AddonState) -> bool {
    match state.db.read() {
        Ok(conn) => {
            // Pobierz limit
            let limit: i64 = conn.query_row(
                "SELECT max_http_requests_per_minute FROM addon_resource_limits WHERE addon_id = ?1",
                rusqlite::params![&state.addon_id],
                |row| row.get(0),
            ).unwrap_or(600);

            // Policz requesty z ostatniej minuty
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM audit_log \
                 WHERE addon_id = ?1 AND action = 'http.request' AND result = 'ok' \
                 AND created_at >= datetime('now', '-1 minute')",
                    rusqlite::params![&state.addon_id],
                    |row| row.get(0),
                )
                .unwrap_or(0);

            count < limit
        }
        // CR-008: Fail-closed — blokuj request w razie bledu DB
        Err(_) => false,
    }
}

/// Wykonuje HTTP request (synchronicznie) uzywajac zweryfikowanych adresow DNS.
fn execute_http_request(
    request: &serde_json::Value,
    url: &str,
    method: &str,
    domain: &str,
    resolved_addrs: &[SocketAddr],
) -> serde_json::Value {
    // Redirecty wylaczone: walidacja i pinning DNS obejmuja tylko pierwotny
    // host — follow na 3xx pozwolilby zatwierdzonemu hostowi przekierowac
    // request na adres spoza zatwierdzonych regul (redirect SSRF). Addon
    // dostaje surowy 3xx i moze podazyc recznie przez zatwierdzone hosty.
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_millis(30_000))
        .pool_max_idle_per_host(10)
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(domain, resolved_addrs)
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            return serde_json::json!({
                "status": 0,
                "headers": {},
                "body": format!("Blad HTTP client: {}", e),
            });
        }
    };

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
    let timeout_ms = request
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(30_000);
    let request_builder = request_builder.timeout(std::time::Duration::from_millis(timeout_ms));

    // Dodaj headery z requestu
    let request_builder = if let Some(headers) = request.get("headers").and_then(|v| v.as_object())
    {
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
            let headers: std::collections::HashMap<String, String> = response
                .headers()
                .iter()
                .filter_map(|(k, v)| v.to_str().ok().map(|val| (k.to_string(), val.to_string())))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::event_bus::EventBus;
    use crate::addon::host_functions::check_permission;
    use crate::addon::host_functions::network::NetworkConnectionManager;
    use crate::addon::permissions::PermissionChecker;
    use crate::addon::{AddonManifest, ManifestNetworkRule};
    use parking_lot::Mutex;
    use std::path::Path;
    use std::sync::Arc;

    fn make_state(permissions: Vec<String>, network_rules: Vec<ManifestNetworkRule>) -> AddonState {
        let db = crate::db::init(Path::new(":memory:")).unwrap();
        AddonState {
            addon_id: "http-test-addon".to_string(),
            instance_id: "t".to_string(),
            user_id: None,
            org_id: None,
            db: db.clone(),
            permissions,
            event_bus: Arc::new(EventBus::new()),
            permission_checker: Arc::new(PermissionChecker::new(db)),
            fuel_consumed: 0,
            is_system_call: true,
            call_provenance: crate::addon::AddonCallProvenance::addon(),
            rate_limiter: None,
            net_manager: Arc::new(Mutex::new(NetworkConnectionManager::new())),
            settings_cipher: Arc::new(crate::crypto::SettingsCipher::new(&[0u8; 32])),
            manifest: Arc::new(AddonManifest {
                network_rules,
                ..AddonManifest::default()
            }),
            memory_limit: 64 * 1024 * 1024,
            oauth_refresh_guard: std::sync::Arc::new(
                crate::addon::oauth_refresh_guard::OAuthRefreshGuard::new(),
            ),
            router: None,
            ui_panels: None,
            #[cfg(not(any(target_os = "ios", target_os = "android")))]
            wasi: wasmtime_wasi::WasiCtxBuilder::new().build_p1(),
        }
    }

    #[test]
    fn http_request_denied_without_permission() {
        let state = make_state(vec!["storage".to_string()], Vec::new());
        assert!(
            !check_permission(&state, "http.request", None),
            "Brak 'http.request' w permissions odrzuca request"
        );
    }

    #[test]
    fn http_destination_denied_without_network_rule() {
        let state = make_state(
            vec!["http.request".to_string(), "http".to_string()],
            Vec::new(),
        );

        assert!(approved_destination_match(&state, "example.com", 443).is_none());
    }

    #[test]
    fn http_destination_requires_approved_network_rule() {
        let rule = ManifestNetworkRule {
            id: "example-https".to_string(),
            protocol: "tcp".to_string(),
            host: "example.com".to_string(),
            port: 443,
            description: Some("Test".to_string()),
            required: true,
        };
        let state = make_state(
            vec!["http.request".to_string(), "http".to_string()],
            vec![rule],
        );

        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "INSERT INTO addon_network_rules \
                 (addon_id, rule_id, protocol, host, port, description, required, approved) \
                 VALUES (?1, 'example-https', 'tcp', 'example.com', 443, 'Test', 1, 0)",
                rusqlite::params![&state.addon_id],
            )
            .unwrap();
        }

        assert!(approved_destination_match(&state, "example.com", 443).is_none());

        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "UPDATE addon_network_rules SET approved = 1 \
                 WHERE addon_id = ?1 AND rule_id = 'example-https'",
                rusqlite::params![&state.addon_id],
            )
            .unwrap();
        }

        assert_eq!(
            approved_destination_match(&state, "example.com", 443),
            Some(ApprovedMatch::Exact)
        );
        assert!(approved_destination_match(&state, "other.example", 443).is_none());
        assert!(approved_destination_match(&state, "example.com", 80).is_none());
    }

    #[test]
    fn host_rule_matches_public_wildcard() {
        assert!(host_rule_matches("*", "example.com"));
        assert!(host_rule_matches("*", "sub.example.com"));
    }

    #[test]
    fn host_rule_matches_subdomain_wildcard() {
        assert!(host_rule_matches("*.example.com", "www.example.com"));
        assert!(host_rule_matches("*.example.com", "a.b.example.com"));
        assert!(!host_rule_matches("*.example.com", "example.com"));
        assert!(!host_rule_matches("*.example.com", "badexample.com"));
    }

    #[test]
    fn wildcard_destination_requires_approved_rule_pattern() {
        let rule = ManifestNetworkRule {
            id: "public-web-https".to_string(),
            protocol: "tcp".to_string(),
            host: "*".to_string(),
            port: 443,
            description: Some("Public web".to_string()),
            required: true,
        };
        let state = make_state(
            vec!["http.request".to_string(), "http".to_string()],
            vec![rule],
        );

        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "INSERT INTO addon_network_rules \
                 (addon_id, rule_id, protocol, host, port, description, required, approved) \
                 VALUES (?1, 'public-web-https', 'tcp', '*', 443, 'Public web', 1, 1)",
                rusqlite::params![&state.addon_id],
            )
            .unwrap();
        }

        assert_eq!(
            approved_destination_match(&state, "example.com", 443),
            Some(ApprovedMatch::Wildcard)
        );
        assert_eq!(
            approved_destination_match(&state, "docs.rs", 443),
            Some(ApprovedMatch::Wildcard)
        );
        assert!(approved_destination_match(&state, "example.com", 80).is_none());
    }

    /// Regula exact-host z prywatnym adresem LAN jest dozwolona po
    /// zatwierdzeniu, a exact wygrywa z rownoczesnie pasujacym wildcardem.
    #[test]
    fn exact_private_rule_allows_lan_and_beats_wildcard() {
        let exact = ManifestNetworkRule {
            id: "lan-mcp".to_string(),
            protocol: "tcp".to_string(),
            host: "192.168.11.122".to_string(),
            port: 443,
            description: Some("LAN MCP server".to_string()),
            required: false,
        };
        let wildcard = ManifestNetworkRule {
            id: "public-web-https".to_string(),
            protocol: "tcp".to_string(),
            host: "*".to_string(),
            port: 443,
            description: Some("Public web".to_string()),
            required: true,
        };
        let state = make_state(
            vec!["http.request".to_string(), "http".to_string()],
            vec![wildcard, exact],
        );

        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "INSERT INTO addon_network_rules \
                 (addon_id, rule_id, protocol, host, port, description, required, approved) \
                 VALUES (?1, 'public-web-https', 'tcp', '*', 443, 'Public web', 1, 1)",
                rusqlite::params![&state.addon_id],
            )
            .unwrap();
        }

        // Tylko wildcard zatwierdzony — LAN host dopasowany jako Wildcard,
        // czyli guard SSRF go zablokuje.
        assert_eq!(
            approved_destination_match(&state, "192.168.11.122", 443),
            Some(ApprovedMatch::Wildcard)
        );
        assert!(!is_safe_url("https://192.168.11.122/mcp"));

        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "INSERT INTO addon_network_rules \
                 (addon_id, rule_id, protocol, host, port, description, required, approved) \
                 VALUES (?1, 'lan-mcp', 'tcp', '192.168.11.122', 443, 'LAN MCP', 0, 1)",
                rusqlite::params![&state.addon_id],
            )
            .unwrap();
        }

        assert_eq!(
            approved_destination_match(&state, "192.168.11.122", 443),
            Some(ApprovedMatch::Exact)
        );
    }

    #[test]
    fn resolve_destination_gates_private_only_when_required() {
        let public_only = resolve_destination("127.0.0.1", 8080, true);
        assert!(
            public_only.is_none(),
            "wildcard match musi odrzucic loopback"
        );
        let exact =
            resolve_destination("127.0.0.1", 8080, false).expect("exact match pozwala na loopback");
        assert!(!exact.is_empty());
    }

    #[test]
    fn extract_http_destination_rejects_non_http_schemes() {
        assert!(extract_http_destination("ftp://example.com/x").is_none());
        assert!(extract_http_destination("file:///etc/passwd").is_none());
        assert_eq!(
            extract_http_destination("https://example.com/x"),
            Some(("example.com".to_string(), 443))
        );
        assert_eq!(
            extract_http_destination("http://192.168.11.122:8080/mcp"),
            Some(("192.168.11.122".to_string(), 8080))
        );
    }

    #[test]
    fn public_ip_check_blocks_local_and_private_ranges() {
        assert!(!is_public_ip("127.0.0.1".parse().unwrap()));
        assert!(!is_public_ip("10.0.0.1".parse().unwrap()));
        assert!(!is_public_ip("192.168.1.1".parse().unwrap()));
        assert!(!is_public_ip("169.254.169.254".parse().unwrap()));
        assert!(is_public_ip("93.184.216.34".parse().unwrap()));
    }
}

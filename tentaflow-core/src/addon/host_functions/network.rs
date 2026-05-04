// =============================================================================
// Plik: addon/host_functions/network.rs
// Opis: Host functions sieciowe — proxy TCP/UDP z walidacja regul, auditlogiem.
//       Addon nie laczy sie bezposrednio z siecią — Core proxy sprawdza reguly,
//       zatwierdzenia, waliduje DNS/IP (SSRF) i loguje kazda operacje.
// Uprawnienia: "network" (connect/send/recv/close). Fail-closed — brak
//              uprawnienia blokuje operacje zanim dotknie socketu.
// =============================================================================

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::Duration;

use tracing::{info, warn};

use super::{
    audit_log, check_permission, get_memory, read_guest_bytes, read_guest_string,
    write_guest_bytes, AddonState, WasmCaller, ABI_ERR_OPERATION, ABI_ERR_PERMISSION, ABI_OK,
};

// =============================================================================
// Kody bledow sieciowych
// =============================================================================

/// Regula sieciowa nie znaleziona w manifescie addonu
pub const ABI_ERR_NETWORK_RULE_NOT_FOUND: i32 = -8;
/// Regula sieciowa nie zostala zatwierdzona przez admina
pub const ABI_ERR_NETWORK_RULE_NOT_APPROVED: i32 = -9;
/// Przekroczono limit polaczen per addon (max 10)
pub const ABI_ERR_MAX_CONNECTIONS: i32 = -10;
/// Polaczenie o podanym ID nie istnieje
pub const ABI_ERR_CONNECTION_NOT_FOUND: i32 = -11;
/// Nie udalo sie nawiazac polaczenia (DNS, timeout, odmowa)
pub const ABI_ERR_CONNECTION_FAILED: i32 = -12;

// =============================================================================
// Stale konfiguracyjne
// =============================================================================

/// Maksymalna liczba jednoczesnych polaczen per instancja addonu
const MAX_CONNECTIONS_PER_ADDON: usize = 10;
/// Timeout nawiazywania polaczenia TCP
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Timeout odczytu danych z socketu
const RECV_TIMEOUT: Duration = Duration::from_secs(60);
/// Timeout wysylania danych przez socket
const SEND_TIMEOUT: Duration = Duration::from_secs(30);
/// VULN-047: Maksymalny rozmiar danych w jednej operacji net_send (1 MB)
const MAX_SEND_SIZE: usize = 1_048_576;

// =============================================================================
// NetworkConnectionManager — menedzer polaczen sieciowych
// =============================================================================

/// Rodzaj transportu sieciowego
enum NetTransport {
    Tcp(TcpStream),
    Udp(UdpSocket),
}

/// Polaczenie sieciowe — transport + metadane (rule_id do weryfikacji approved)
struct NetConnection {
    transport: NetTransport,
    /// Identyfikator reguly sieciowej — do sprawdzania approved przy send/recv (VULN-048)
    rule_id: String,
}

/// Menedzer polaczen sieciowych per instancja addonu.
/// Przechowuje aktywne sockety TCP/UDP z limitem MAX_CONNECTIONS_PER_ADDON.
/// VULN-045: Per-instance counter zamiast globalnego AtomicU32 (unika overflow miedzy instancjami)
pub struct NetworkConnectionManager {
    connections: HashMap<u32, NetConnection>,
    /// VULN-045: Per-instance counter ID polaczen — unika globalnego overflow
    next_id: u32,
}

impl NetworkConnectionManager {
    /// Tworzy nowy menedzer polaczen
    pub fn new() -> Self {
        Self {
            connections: HashMap::new(),
            next_id: 0,
        }
    }

    /// Zwraca aktualna liczbe aktywnych polaczen
    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    /// VULN-045: Generuje nastepny unikalny ID polaczenia (per-instance, wrapping)
    fn next_conn_id(&mut self) -> u32 {
        self.next_id = self.next_id.wrapping_add(1);
        // Unikaj 0 — zarezerwowane jako "brak polaczenia"
        if self.next_id == 0 {
            self.next_id = 1;
        }
        self.next_id
    }

    /// VULN-046: Zamyka wszystkie aktywne polaczenia (przy stop_addon)
    pub fn close_all(&mut self) {
        self.connections.clear();
    }
}

// =============================================================================
// Walidacja IP — blokowanie adresow prywatnych/loopback (SSRF)
// =============================================================================

/// Sprawdza czy adres IP jest bezpieczny (publiczny).
/// Blokuje: loopback, prywatne (RFC 1918), link-local, metadata chmurowe.
fn is_safe_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.octets()[0] == 0
                || v4.is_broadcast()
            {
                return false;
            }
            // Metadata chmurowe (169.254.169.254)
            if v4.octets() == [169, 254, 169, 254] {
                return false;
            }
            true
        }
        IpAddr::V6(v6) => {
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
                if v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.octets()[0] == 0
                {
                    return false;
                }
            }
            true
        }
    }
}

/// VULN-043: Sprawdza czy adres IP jest prywatny/loopback (odwrotnosc is_safe_ip).
/// Uzywa tej samej logiki co is_safe_ip — do weryfikacji peer_addr po polaczeniu (DNS rebinding).
fn is_private_ip(ip: &IpAddr) -> bool {
    !is_safe_ip(ip)
}

// =============================================================================
// host_net_connect — nawiazanie polaczenia TCP/UDP
// =============================================================================

/// Host function: nawiazuje polaczenie sieciowe TCP/UDP wedlug reguly z manifestu.
///
/// ABI:
/// - rule_id_ptr/rule_id_len: identyfikator reguly sieciowej (UTF-8)
/// - Zwraca: conn_id (>0) lub kod bledu (<0)
///
/// Przebieg:
/// 1. Odczytaj rule_id z pamieci guest
/// 2. Sprawdz uprawnienie "network"
/// 3. Znajdz regule w manifescie addonu
/// 4. Sprawdz zatwierdzenie reguly w DB (approved=1)
/// 5. Sprawdz limit polaczen
/// 6. DNS resolve + walidacja IP (SSRF)
/// 7. Nawiaz polaczenie TCP/UDP
/// 8. Zapisz w ConnectionManager, zwroc conn_id
pub fn host_net_connect(
    mut caller: WasmCaller<'_, AddonState>,
    rule_id_ptr: i32,
    rule_id_len: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    // 1. Odczytaj rule_id z pamieci guest
    let rule_id = match read_guest_string(&memory, &caller, rule_id_ptr, rule_id_len) {
        Some(s) => s.to_string(),
        None => return ABI_ERR_OPERATION,
    };

    // 2. Sprawdz uprawnienie "network"
    if !check_permission(caller.data(), "network", None) {
        audit_log(
            caller.data(),
            "net.connect",
            Some("network"),
            Some(&rule_id),
            "denied",
            Some("brak uprawnienia 'network'"),
        );
        return ABI_ERR_PERMISSION;
    }

    // 3. Znajdz regule w manifescie addonu
    let manifest = caller.data().manifest.clone();
    let rule = match manifest.network_rules.iter().find(|r| r.id == rule_id) {
        Some(r) => r.clone(),
        None => {
            warn!(
                "net_connect: regula '{}' nie znaleziona w manifescie",
                rule_id
            );
            audit_log(
                caller.data(),
                "net.connect",
                Some("network"),
                Some(&rule_id),
                "error",
                Some("regula nie znaleziona w manifescie"),
            );
            return ABI_ERR_NETWORK_RULE_NOT_FOUND;
        }
    };

    // 4. Sprawdz zatwierdzenie reguly w DB
    let approved = {
        let addon_id = caller.data().addon_id.clone();
        match caller.data().db.lock() {
            Ok(conn) => conn
                .query_row(
                    "SELECT approved FROM addon_network_rules \
                     WHERE addon_id = ?1 AND rule_id = ?2",
                    rusqlite::params![&addon_id, &rule_id],
                    |row| row.get::<_, i32>(0),
                )
                .unwrap_or(0),
            Err(_) => {
                // Fail-closed — blokuj przy bledzie DB
                audit_log(
                    caller.data(),
                    "net.connect",
                    Some("network"),
                    Some(&rule_id),
                    "error",
                    Some("blad dostepu do DB"),
                );
                return ABI_ERR_OPERATION;
            }
        }
    };

    if approved != 1 {
        warn!("net_connect: regula '{}' nie jest zatwierdzona", rule_id);
        audit_log(
            caller.data(),
            "net.connect",
            Some("network"),
            Some(&rule_id),
            "denied",
            Some("regula nie zatwierdzona (approved=0)"),
        );
        return ABI_ERR_NETWORK_RULE_NOT_APPROVED;
    }

    // 5. Sprawdz limit polaczen
    let net_manager = caller.data().net_manager.clone();
    {
        let mgr = net_manager.lock();
        if mgr.connection_count() >= MAX_CONNECTIONS_PER_ADDON {
            audit_log(
                caller.data(),
                "net.connect",
                Some("network"),
                Some(&rule_id),
                "error",
                Some(&format!(
                    "limit polaczen przekroczony (max {})",
                    MAX_CONNECTIONS_PER_ADDON
                )),
            );
            return ABI_ERR_MAX_CONNECTIONS;
        }
    }

    // 6. DNS resolve + walidacja IP (SSRF)
    let addr_str = format!("{}:{}", rule.host, rule.port);
    let addrs: Vec<std::net::SocketAddr> = match addr_str.to_socket_addrs() {
        Ok(a) => a.collect(),
        Err(e) => {
            warn!("net_connect: blad DNS resolve '{}': {}", addr_str, e);
            audit_log(
                caller.data(),
                "net.connect",
                Some("network"),
                Some(&rule_id),
                "error",
                Some(&format!("blad DNS resolve: {}", e)),
            );
            return ABI_ERR_CONNECTION_FAILED;
        }
    };

    if addrs.is_empty() {
        audit_log(
            caller.data(),
            "net.connect",
            Some("network"),
            Some(&rule_id),
            "error",
            Some("DNS resolve zwrocil 0 adresow"),
        );
        return ABI_ERR_CONNECTION_FAILED;
    }

    // Walidacja IP — blokuj adresy prywatne/loopback
    for addr in &addrs {
        if !is_safe_ip(&addr.ip()) {
            warn!(
                "net_connect: zablokowany adres prywatny/loopback: {}",
                addr.ip()
            );
            audit_log(
                caller.data(),
                "net.connect",
                Some("network"),
                Some(&rule_id),
                "denied",
                Some(&format!("SSRF: adres {} jest prywatny/loopback", addr.ip())),
            );
            return ABI_ERR_PERMISSION;
        }
    }

    let target_addr = addrs[0];

    // 7. Nawiaz polaczenie TCP/UDP
    let transport = match rule.protocol.as_str() {
        "tcp" => {
            match TcpStream::connect_timeout(&target_addr, CONNECT_TIMEOUT) {
                Ok(stream) => {
                    // VULN-043: Sprawdz peer_addr po polaczeniu — ochrona przed DNS rebinding
                    if let Ok(peer) = stream.peer_addr() {
                        if is_private_ip(&peer.ip()) {
                            warn!(
                                "net_connect: peer_addr {} prywatny po polaczeniu (DNS rebinding)",
                                peer
                            );
                            audit_log(
                                caller.data(),
                                "net.connect",
                                Some("network"),
                                Some(&rule_id),
                                "denied",
                                Some(&format!(
                                    "SSRF/DNS-rebinding: peer_addr {} jest prywatny",
                                    peer
                                )),
                            );
                            return ABI_ERR_PERMISSION;
                        }
                    }
                    // Ustaw timeouty
                    stream.set_read_timeout(Some(RECV_TIMEOUT)).ok();
                    stream.set_write_timeout(Some(SEND_TIMEOUT)).ok();
                    // Wylacz Nagle — mniejsze opoznienie
                    stream.set_nodelay(true).ok();
                    NetTransport::Tcp(stream)
                }
                Err(e) => {
                    warn!("net_connect: blad TCP connect '{}': {}", addr_str, e);
                    audit_log(
                        caller.data(),
                        "net.connect",
                        Some("network"),
                        Some(&rule_id),
                        "error",
                        Some(&format!("blad TCP connect: {}", e)),
                    );
                    return ABI_ERR_CONNECTION_FAILED;
                }
            }
        }
        "udp" => {
            // Binduj na losowym porcie, potem connect do celu
            match UdpSocket::bind("0.0.0.0:0") {
                Ok(socket) => {
                    if let Err(e) = socket.connect(target_addr) {
                        warn!("net_connect: blad UDP connect '{}': {}", addr_str, e);
                        audit_log(
                            caller.data(),
                            "net.connect",
                            Some("network"),
                            Some(&rule_id),
                            "error",
                            Some(&format!("blad UDP connect: {}", e)),
                        );
                        return ABI_ERR_CONNECTION_FAILED;
                    }
                    socket.set_read_timeout(Some(RECV_TIMEOUT)).ok();
                    socket.set_write_timeout(Some(SEND_TIMEOUT)).ok();
                    NetTransport::Udp(socket)
                }
                Err(e) => {
                    warn!("net_connect: blad UDP bind: {}", e);
                    audit_log(
                        caller.data(),
                        "net.connect",
                        Some("network"),
                        Some(&rule_id),
                        "error",
                        Some(&format!("blad UDP bind: {}", e)),
                    );
                    return ABI_ERR_CONNECTION_FAILED;
                }
            }
        }
        other => {
            warn!("net_connect: nieobslugiwany protokol '{}'", other);
            audit_log(
                caller.data(),
                "net.connect",
                Some("network"),
                Some(&rule_id),
                "error",
                Some(&format!("nieobslugiwany protokol: {}", other)),
            );
            return ABI_ERR_OPERATION;
        }
    };

    // 8. Zapisz w ConnectionManager i zwroc conn_id
    // VULN-045: Per-instance counter zamiast globalnego AtomicU32
    let conn_id = {
        let mut mgr = net_manager.lock();
        let id = mgr.next_conn_id();
        let connection = NetConnection {
            transport,
            rule_id: rule_id.clone(),
        };
        mgr.connections.insert(id, connection);
        id
    };

    info!(
        "net_connect: addon='{}' polaczono {}://{}:{} (conn_id={})",
        caller.data().addon_id,
        rule.protocol,
        rule.host,
        rule.port,
        conn_id
    );

    audit_log(
        caller.data(),
        "net.connect",
        Some("network"),
        Some(&rule_id),
        "ok",
        None,
    );

    conn_id as i32
}

// =============================================================================
// host_net_send — wysylanie danych przez polaczenie
// =============================================================================

/// Host function: wysyla dane przez aktywne polaczenie sieciowe.
///
/// ABI:
/// - conn_id: identyfikator polaczenia (zwrocony przez net_connect)
/// - data_ptr/data_len: dane do wyslania (bajty z pamieci guest)
/// - Zwraca: liczbe wyslanych bajtow (>0) lub kod bledu (<0)
pub fn host_net_send(
    mut caller: WasmCaller<'_, AddonState>,
    conn_id: i32,
    data_ptr: i32,
    data_len: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    // Fail-closed: sprawdz uprawnienie "network" zanim dotkniemy socketu lub pamieci guest
    if !check_permission(caller.data(), "network", None) {
        audit_log(
            caller.data(),
            "net.send",
            Some("network"),
            Some(&conn_id.to_string()),
            "denied",
            Some("brak uprawnienia 'network'"),
        );
        return ABI_ERR_PERMISSION;
    }

    // VULN-047: Sprawdz limit rozmiaru danych przed odczytem z pamieci guest
    if data_len as usize > MAX_SEND_SIZE {
        warn!(
            "net_send: rozmiar danych {} przekracza limit {}",
            data_len, MAX_SEND_SIZE
        );
        audit_log(
            caller.data(),
            "net.send",
            Some("network"),
            Some(&conn_id.to_string()),
            "error",
            Some(&format!(
                "rozmiar danych {} przekracza limit {}",
                data_len, MAX_SEND_SIZE
            )),
        );
        return ABI_ERR_OPERATION;
    }

    // Odczytaj dane z pamieci guest
    let data = match read_guest_bytes(&memory, &caller, data_ptr, data_len) {
        Some(b) => b.to_vec(),
        None => return ABI_ERR_OPERATION,
    };

    // VULN-048: Sprawdz czy regula jest nadal approved przed wyslaniem
    let net_manager = caller.data().net_manager.clone();
    let conn_id_u32 = conn_id as u32;

    // Pobierz rule_id z polaczenia
    let conn_rule_id = {
        let mgr = net_manager.lock();
        match mgr.connections.get(&conn_id_u32) {
            Some(c) => c.rule_id.clone(),
            None => {
                audit_log(
                    caller.data(),
                    "net.send",
                    Some("network"),
                    Some(&conn_id.to_string()),
                    "error",
                    Some("polaczenie nie znalezione"),
                );
                return ABI_ERR_CONNECTION_NOT_FOUND;
            }
        }
    };

    // Sprawdz approved w DB — jesli regula cofnieta, zamknij polaczenie
    {
        let addon_id = caller.data().addon_id.clone();
        let approved = match caller.data().db.lock() {
            Ok(conn) => conn
                .query_row(
                    "SELECT approved FROM addon_network_rules WHERE addon_id = ?1 AND rule_id = ?2",
                    rusqlite::params![&addon_id, &conn_rule_id],
                    |row| row.get::<_, i32>(0),
                )
                .unwrap_or(0),
            Err(_) => 0,
        };
        if approved != 1 {
            warn!(
                "net_send: regula '{}' nie jest juz zatwierdzona — zamykam polaczenie {}",
                conn_rule_id, conn_id
            );
            // VULN-048: Zamknij polaczenie przy cofnietej regule
            net_manager.lock().connections.remove(&conn_id_u32);
            audit_log(
                caller.data(),
                "net.send",
                Some("network"),
                Some(&conn_id.to_string()),
                "denied",
                Some(&format!(
                    "regula '{}' cofnieta — polaczenie zamkniete",
                    conn_rule_id
                )),
            );
            return ABI_ERR_NETWORK_RULE_NOT_APPROVED;
        }
    }

    let mut mgr = net_manager.lock();
    let connection = match mgr.connections.get_mut(&conn_id_u32) {
        Some(c) => c,
        None => {
            audit_log(
                caller.data(),
                "net.send",
                Some("network"),
                Some(&conn_id.to_string()),
                "error",
                Some("polaczenie nie znalezione"),
            );
            return ABI_ERR_CONNECTION_NOT_FOUND;
        }
    };

    let bytes_sent = match &mut connection.transport {
        NetTransport::Tcp(ref mut stream) => match stream.write_all(&data) {
            Ok(()) => data.len(),
            Err(e) => {
                warn!("net_send: blad TCP write (conn_id={}): {}", conn_id, e);
                audit_log(
                    caller.data(),
                    "net.send",
                    Some("network"),
                    Some(&conn_id.to_string()),
                    "error",
                    Some(&format!("blad TCP write: {}", e)),
                );
                return ABI_ERR_OPERATION;
            }
        },
        NetTransport::Udp(ref socket) => match socket.send(&data) {
            Ok(n) => n,
            Err(e) => {
                warn!("net_send: blad UDP send (conn_id={}): {}", conn_id, e);
                audit_log(
                    caller.data(),
                    "net.send",
                    Some("network"),
                    Some(&conn_id.to_string()),
                    "error",
                    Some(&format!("blad UDP send: {}", e)),
                );
                return ABI_ERR_OPERATION;
            }
        },
    };

    audit_log(
        caller.data(),
        "net.send",
        Some("network"),
        Some(&conn_id.to_string()),
        "ok",
        None,
    );

    bytes_sent as i32
}

// =============================================================================
// host_net_recv — odbieranie danych z polaczenia
// =============================================================================

/// Host function: odbiera dane z aktywnego polaczenia sieciowego.
///
/// ABI:
/// - conn_id: identyfikator polaczenia
/// - out_ptr/out_capacity: bufor w pamieci guest na odebrane dane
/// - Zwraca: packed i64 = (status << 32) | bytes_read
///   - status = ABI_OK (0) przy sukcesie
///   - bytes_read = liczba odebranych bajtow
///   - Przy bledzie: (error_code << 32) | 0
pub fn host_net_recv(
    mut caller: WasmCaller<'_, AddonState>,
    conn_id: i32,
    out_ptr: i32,
    out_capacity: i32,
) -> i64 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return (ABI_ERR_OPERATION as i64) << 32,
    };

    if out_capacity <= 0 {
        return (ABI_ERR_OPERATION as i64) << 32;
    }

    // Fail-closed: sprawdz uprawnienie "network" zanim odczytamy z socketu
    if !check_permission(caller.data(), "network", None) {
        audit_log(
            caller.data(),
            "net.recv",
            Some("network"),
            Some(&conn_id.to_string()),
            "denied",
            Some("brak uprawnienia 'network'"),
        );
        return (ABI_ERR_PERMISSION as i64) << 32;
    }

    let net_manager = caller.data().net_manager.clone();
    let conn_id_u32 = conn_id as u32;

    // VULN-048: Sprawdz czy regula jest nadal approved przed odbiorem
    let conn_rule_id = {
        let mgr = net_manager.lock();
        match mgr.connections.get(&conn_id_u32) {
            Some(c) => c.rule_id.clone(),
            None => {
                audit_log(
                    caller.data(),
                    "net.recv",
                    Some("network"),
                    Some(&conn_id.to_string()),
                    "error",
                    Some("polaczenie nie znalezione"),
                );
                return (ABI_ERR_CONNECTION_NOT_FOUND as i64) << 32;
            }
        }
    };

    {
        let addon_id = caller.data().addon_id.clone();
        let approved = match caller.data().db.lock() {
            Ok(conn) => conn
                .query_row(
                    "SELECT approved FROM addon_network_rules WHERE addon_id = ?1 AND rule_id = ?2",
                    rusqlite::params![&addon_id, &conn_rule_id],
                    |row| row.get::<_, i32>(0),
                )
                .unwrap_or(0),
            Err(_) => 0,
        };
        if approved != 1 {
            warn!(
                "net_recv: regula '{}' nie jest juz zatwierdzona — zamykam polaczenie {}",
                conn_rule_id, conn_id
            );
            net_manager.lock().connections.remove(&conn_id_u32);
            audit_log(
                caller.data(),
                "net.recv",
                Some("network"),
                Some(&conn_id.to_string()),
                "denied",
                Some(&format!(
                    "regula '{}' cofnieta — polaczenie zamkniete",
                    conn_rule_id
                )),
            );
            return (ABI_ERR_NETWORK_RULE_NOT_APPROVED as i64) << 32;
        }
    }

    // Przygotuj bufor tymczasowy
    let capacity = out_capacity as usize;
    let mut buf = vec![0u8; capacity];

    let bytes_read = {
        let mut mgr = net_manager.lock();
        let connection = match mgr.connections.get_mut(&conn_id_u32) {
            Some(c) => c,
            None => {
                audit_log(
                    caller.data(),
                    "net.recv",
                    Some("network"),
                    Some(&conn_id.to_string()),
                    "error",
                    Some("polaczenie nie znalezione"),
                );
                return (ABI_ERR_CONNECTION_NOT_FOUND as i64) << 32;
            }
        };

        match &mut connection.transport {
            NetTransport::Tcp(ref mut stream) => {
                match stream.read(&mut buf) {
                    Ok(n) => n,
                    Err(e) => {
                        // Timeout nie jest bledem krytycznym — zwroc 0 bajtow
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::TimedOut
                        {
                            0
                        } else {
                            warn!("net_recv: blad TCP read (conn_id={}): {}", conn_id, e);
                            audit_log(
                                caller.data(),
                                "net.recv",
                                Some("network"),
                                Some(&conn_id.to_string()),
                                "error",
                                Some(&format!("blad TCP read: {}", e)),
                            );
                            return (ABI_ERR_OPERATION as i64) << 32;
                        }
                    }
                }
            }
            NetTransport::Udp(ref socket) => match socket.recv(&mut buf) {
                Ok(n) => n,
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut
                    {
                        0
                    } else {
                        warn!("net_recv: blad UDP recv (conn_id={}): {}", conn_id, e);
                        audit_log(
                            caller.data(),
                            "net.recv",
                            Some("network"),
                            Some(&conn_id.to_string()),
                            "error",
                            Some(&format!("blad UDP recv: {}", e)),
                        );
                        return (ABI_ERR_OPERATION as i64) << 32;
                    }
                }
            },
        }
    };

    // Zapisz dane do pamieci guest
    if bytes_read > 0 {
        let written = write_guest_bytes(
            &memory,
            &mut caller,
            out_ptr,
            out_capacity,
            &buf[..bytes_read],
        );
        if written < 0 {
            return (ABI_ERR_OPERATION as i64) << 32;
        }
    }

    audit_log(
        caller.data(),
        "net.recv",
        Some("network"),
        Some(&conn_id.to_string()),
        "ok",
        None,
    );

    // Packed result: (ABI_OK << 32) | bytes_read
    (ABI_OK as i64) << 32 | bytes_read as i64
}

// =============================================================================
// host_net_close — zamkniecie polaczenia
// =============================================================================

/// Host function: zamyka aktywne polaczenie sieciowe.
///
/// ABI:
/// - conn_id: identyfikator polaczenia
/// - Zwraca: ABI_OK (0) lub kod bledu (<0)
///
/// Usuniecie z ConnectionManager powoduje drop socketu (automatyczne zamkniecie).
pub fn host_net_close(caller: WasmCaller<'_, AddonState>, conn_id: i32) -> i32 {
    // Fail-closed: bez uprawnienia "network" addon nie moze zarzadzac polaczeniami
    if !check_permission(caller.data(), "network", None) {
        audit_log(
            caller.data(),
            "net.close",
            Some("network"),
            Some(&conn_id.to_string()),
            "denied",
            Some("brak uprawnienia 'network'"),
        );
        return ABI_ERR_PERMISSION;
    }

    let net_manager = caller.data().net_manager.clone();
    let conn_id_u32 = conn_id as u32;

    let removed = {
        let mut mgr = net_manager.lock();
        mgr.connections.remove(&conn_id_u32).is_some()
    };

    if !removed {
        audit_log(
            caller.data(),
            "net.close",
            Some("network"),
            Some(&conn_id.to_string()),
            "error",
            Some("polaczenie nie znalezione"),
        );
        return ABI_ERR_CONNECTION_NOT_FOUND;
    }

    info!(
        "net_close: addon='{}' zamknieto conn_id={}",
        caller.data().addon_id,
        conn_id
    );

    audit_log(
        caller.data(),
        "net.close",
        Some("network"),
        Some(&conn_id.to_string()),
        "ok",
        None,
    );

    ABI_OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::event_bus::EventBus;
    use crate::addon::host_functions::check_permission;
    use crate::addon::permissions::PermissionChecker;
    use crate::addon::AddonManifest;
    use parking_lot::Mutex;
    use std::path::Path;
    use std::sync::Arc;

    /// Tworzy in-memory DB z pelnym schematem do testow
    fn create_test_db() -> crate::db::DbPool {
        crate::db::init(Path::new(":memory:")).expect("Nie udalo sie utworzyc test DB")
    }

    /// Tworzy minimalny AddonState do testowania check_permission w host_net_*
    fn make_state(permissions: Vec<String>, is_system_call: bool) -> AddonState {
        let db = create_test_db();
        let event_bus = Arc::new(EventBus::new());
        let permission_checker = Arc::new(PermissionChecker::new(db.clone()));
        let settings_cipher = Arc::new(crate::crypto::SettingsCipher::new(&[0u8; 32]));

        AddonState {
            addon_id: "net-test-addon".to_string(),
            instance_id: "test-instance".to_string(),
            user_id: None,
            db,
            permissions,
            event_bus,
            permission_checker,
            fuel_consumed: 0,
            is_system_call,
            rate_limiter: None,
            net_manager: Arc::new(Mutex::new(NetworkConnectionManager::new())),
            settings_cipher,
            manifest: Arc::new(AddonManifest::default()),
            memory_limit: 64 * 1024 * 1024,
            oauth_refresh_guard: std::sync::Arc::new(
                crate::addon::oauth_refresh_guard::OAuthRefreshGuard::new(),
            ),
            router: None,
            #[cfg(not(any(target_os = "ios", target_os = "android")))]
            wasi: wasmtime_wasi::WasiCtxBuilder::new().build_p1(),
        }
    }

    #[test]
    fn net_send_denied_without_network_permission() {
        // Addon bez "network" w permissions nie moze wywolac net_send.
        let state = make_state(vec!["llm".to_string()], true);
        assert!(!check_permission(&state, "network", None),
            "Brak 'network' → check_permission zwraca false (host_net_send zwroci ABI_ERR_PERMISSION)");
    }

    #[test]
    fn net_recv_denied_without_network_permission() {
        // Addon bez "network" nie moze wywolac net_recv — fail-closed.
        let state = make_state(vec![], true);
        assert!(
            !check_permission(&state, "network", None),
            "Brak deklaracji 'network' blokuje recv"
        );
    }

    #[test]
    fn net_close_denied_without_network_permission() {
        // Addon bez "network" nie moze nawet zamknac polaczenia.
        let state = make_state(vec!["storage".to_string()], true);
        assert!(
            !check_permission(&state, "network", None),
            "net_close rowniez wymaga 'network'"
        );
    }

    #[test]
    fn net_allowed_when_network_permission_declared() {
        // Pozytywna sciezka: addon z "network" i is_system_call=true dostaje Granted.
        let state = make_state(vec!["network".to_string()], true);
        assert!(
            check_permission(&state, "network", None),
            "Addon z zadeklarowanym 'network' (system call) powinien przejsc"
        );
    }
}

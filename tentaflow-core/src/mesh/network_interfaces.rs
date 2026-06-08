// =============================================================================
// Plik: mesh/network_interfaces.rs — enumeracja IPv4 NIC hosta + filtry advertise dla iroh mesh
// =============================================================================

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use tentaflow_protocol::NetworkInterfaceInfo;

use crate::db::{repository, DbPool};

/// Klucze settings wspoldzielone z dispatch/handlers.rs — pojedyncze zrodlo prawdy
/// dla nazw, zeby pipeline + handler nie rozjechaly sie po literce.
pub const SETTING_BIND_MODE: &str = "mesh.bind_mode";
pub const SETTING_BIND_IPV4: &str = "mesh.bind_ipv4";
pub const SETTING_HIDE_DOCKER: &str = "mesh.advertise_hide_docker";
pub const SETTING_HIDE_LINK_LOCAL: &str = "mesh.advertise_hide_link_local";
pub const SETTING_HIDE_LOOPBACK: &str = "mesh.advertise_hide_loopback";
pub const SETTING_HIDE_CGNAT: &str = "mesh.advertise_hide_cgnat";
pub const SETTING_PREFER_SAME_SUBNET: &str = "mesh.advertise_prefer_same_subnet";
/// JSON array nazw interfejsow wykluczonych per-karta z advertise (np.
/// `["eth3","tun0"]`). Globalne `hide_*` filtruja po kategorii IP; to filtruje
/// po konkretnej nazwie karty, niezaleznie od kategorii.
pub const SETTING_EXCLUDED_INTERFACES: &str = "mesh.advertise_excluded_interfaces";

/// Buduje snapshot wszystkich interfejsow sieciowych hosta z adresami IPv4.
/// IPv6 jest pomijane swiadomie — mesh bind/advertise dziala wylacznie po v4.
pub fn list_interfaces() -> Vec<NetworkInterfaceInfo> {
    netdev::get_interfaces()
        .into_iter()
        .map(|iface| {
            let name = iface.name.clone();
            let kind = classify_interface(&iface);
            let ipv4_addrs: Vec<String> = iface
                .ipv4
                .iter()
                .map(|net| net.addr().to_string())
                .collect();
            let mac = iface
                .mac_addr
                .as_ref()
                .map(|m| m.address())
                .unwrap_or_default();
            let description = iface
                .friendly_name
                .clone()
                .or_else(|| iface.description.clone())
                .unwrap_or_default();

            NetworkInterfaceInfo {
                name: name.clone(),
                mac,
                ipv4_addrs,
                mtu: read_mtu(&name),
                kind,
                is_up: iface.is_up(),
                description,
            }
        })
        .collect()
}

/// Filtry decydujace ktore adresy IPv4 wysylamy peerom jako kandydatow.
/// Kazdy `hide_*` wylacza konkretny zakres; puste filtry => wszystko advertise.
/// `excluded_interfaces` wyklucza konkretne karty po nazwie, niezaleznie od
/// kategorii IP.
#[derive(Debug, Clone, Default)]
pub struct AdvertiseFilters {
    pub hide_docker: bool,
    pub hide_link_local: bool,
    pub hide_loopback: bool,
    pub hide_cgnat: bool,
    pub excluded_interfaces: Vec<String>,
}

/// Prosta decyzja po samym IP — dla callerow ktorzy nie znaja kind interfejsu.
pub fn should_advertise_ipv4(ip: Ipv4Addr, f: &AdvertiseFilters) -> bool {
    should_advertise_interface_ipv4(ip, "unknown", f)
}

/// Decyzja z kontekstem interfejsu: tailscale/wg/tun (kind=="tunnel") przepuszcza
/// `100.64.0.0/10` nawet gdy `hide_cgnat=true` — tailscale uzywa CGNAT-a jako
/// legalnej wewnetrznej przestrzeni adresowej i schowanie go zepsulo by peering.
pub fn should_advertise_interface_ipv4(
    ip: Ipv4Addr,
    iface_kind: &str,
    f: &AdvertiseFilters,
) -> bool {
    if f.hide_loopback && is_loopback_v4(ip) {
        return false;
    }
    if f.hide_link_local && is_link_local_v4(ip) {
        return false;
    }
    if f.hide_docker && is_docker_range_v4(ip) {
        return false;
    }
    if f.hide_cgnat && is_cgnat_v4(ip) && iface_kind != "tunnel" {
        return false;
    }
    true
}

/// Gdy znamy IP peera (np. z ostatniego handshake), wypychamy kandydata z tej
/// samej `/24` na front listy — iroh probuje adresy po kolei, same-subnet daje
/// sub-milisekundowe RTT vs kilkaset ms dla WAN roundtripu.
pub fn sort_prefer_same_subnet(addrs: &mut Vec<String>, peer_ip: Option<&str>) {
    let Some(peer_raw) = peer_ip else {
        return;
    };
    let Some(peer_v4) = parse_ipv4_from_addr(peer_raw) else {
        return;
    };

    if let Some(pos) = addrs.iter().position(|entry| {
        parse_ipv4_from_addr(entry)
            .map(|v4| same_slash24(v4, peer_v4))
            .unwrap_or(false)
    }) {
        if pos != 0 {
            let picked = addrs.remove(pos);
            addrs.insert(0, picked);
        }
    }
}

// =============================================================================
// Klasyfikacja interfejsu wg nazwy + InterfaceType fallback
// =============================================================================

fn classify_interface(iface: &netdev::Interface) -> String {
    let n = iface.name.to_lowercase();

    if n.starts_with("docker")
        || n.starts_with("br-")
        || n.starts_with("veth")
        || n == "br0" && iface.if_type != netdev::interface::types::InterfaceType::Ethernet
    {
        return "docker".to_string();
    }
    if n.starts_with("tailscale")
        || n.starts_with("wg")
        || n.starts_with("tun")
        || n.starts_with("tap")
    {
        return "tunnel".to_string();
    }
    if n.starts_with("virbr")
        || n.starts_with("vmnet")
        || n.starts_with("vnet")
        || n.starts_with("vboxnet")
    {
        return "virtual".to_string();
    }
    if n.starts_with("wl") || n.starts_with("wlan") || n.starts_with("wlp") {
        return "wifi".to_string();
    }
    if n == "lo" || iface.if_type == netdev::interface::types::InterfaceType::Loopback {
        return "loopback".to_string();
    }
    if !iface.ipv4.is_empty() {
        return "ethernet".to_string();
    }
    "unknown".to_string()
}

// =============================================================================
// Zakresy IPv4 — manualne zeby uniknac zewnetrznej biblioteki CIDR
// =============================================================================

fn is_loopback_v4(ip: Ipv4Addr) -> bool {
    ip.octets()[0] == 127
}

fn is_link_local_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 169 && o[1] == 254
}

/// `172.16.0.0/12` — obejmuje cala prywatna /12 (docker domyslny bridge to
/// 172.17.0.0/16, compose tworzy 172.18+/16, wszystkie mieszcza sie w /12).
fn is_docker_range_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 172 && (o[1] & 0xF0) == 0x10
}

/// CGNAT `100.64.0.0/10` — od 100.64.0.0 do 100.127.255.255.
fn is_cgnat_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 100 && (64..=127).contains(&o[1])
}

fn same_slash24(a: Ipv4Addr, b: Ipv4Addr) -> bool {
    let ao = a.octets();
    let bo = b.octets();
    ao[0] == bo[0] && ao[1] == bo[1] && ao[2] == bo[2]
}

/// Akceptuje zarowno "1.2.3.4" jak i "1.2.3.4:5678".
fn parse_ipv4_from_addr(raw: &str) -> Option<Ipv4Addr> {
    let trimmed = raw.split(':').next()?;
    trimmed.parse::<Ipv4Addr>().ok()
}

// =============================================================================
// MTU — netdev 0.31 nie eksponuje MTU, czytamy z sysfs na Linux, 0 dla pozostalych
// =============================================================================

#[cfg(target_os = "linux")]
fn read_mtu(name: &str) -> u32 {
    let path = format!("/sys/class/net/{}/mtu", name);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0)
}

#[cfg(not(target_os = "linux"))]
fn read_mtu(_name: &str) -> u32 {
    0
}

// =============================================================================
// Integracja z DB — resolve bind + filtry advertise z settings
// =============================================================================

fn parse_bool(raw: Option<String>, default: bool) -> bool {
    match raw.as_deref() {
        Some("1") | Some("true") => true,
        Some("0") | Some("false") => false,
        _ => default,
    }
}

/// Laduje `AdvertiseFilters` z tabeli settings. Domyslne wartosci musza sie
/// zgadzac z migracja V57 i z dispatch/handlers.rs, inaczej logika pipeline
/// zachowywalaby sie inaczej niz to co widzi user w GUI.
pub fn load_advertise_filters(db: &DbPool) -> AdvertiseFilters {
    let hide_docker = parse_bool(
        repository::get_setting(db, SETTING_HIDE_DOCKER)
            .ok()
            .flatten(),
        true,
    );
    let hide_link_local = parse_bool(
        repository::get_setting(db, SETTING_HIDE_LINK_LOCAL)
            .ok()
            .flatten(),
        true,
    );
    let hide_loopback = parse_bool(
        repository::get_setting(db, SETTING_HIDE_LOOPBACK)
            .ok()
            .flatten(),
        true,
    );
    let hide_cgnat = parse_bool(
        repository::get_setting(db, SETTING_HIDE_CGNAT)
            .ok()
            .flatten(),
        false,
    );
    let excluded_interfaces = repository::get_setting(db, SETTING_EXCLUDED_INTERFACES)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .unwrap_or_default();
    AdvertiseFilters {
        hide_docker,
        hide_link_local,
        hide_loopback,
        hide_cgnat,
        excluded_interfaces,
    }
}

/// Laduje flage `advertise_prefer_same_subnet` z settings (default `true`).
pub fn load_prefer_same_subnet(db: &DbPool) -> bool {
    parse_bool(
        repository::get_setting(db, SETTING_PREFER_SAME_SUBNET)
            .ok()
            .flatten(),
        true,
    )
}

/// Mapa `IPv4 -> kind` zbudowana z `list_interfaces()`. Potrzebna zeby filtr
/// CGNAT przepuszczal adresy na interfejsie typu tunnel (tailscale/wg) nawet
/// gdy `hide_cgnat=true`.
pub fn ipv4_kind_map() -> std::collections::HashMap<Ipv4Addr, String> {
    let mut map = std::collections::HashMap::new();
    for iface in list_interfaces() {
        for addr in &iface.ipv4_addrs {
            if let Ok(ip) = addr.parse::<Ipv4Addr>() {
                map.insert(ip, iface.kind.clone());
            }
        }
    }
    map
}

/// Mapa `IPv4 -> nazwa interfejsu` z `list_interfaces()`. Potrzebna zeby filtr
/// per-karta (`excluded_interfaces`) wiedzial do ktorej karty nalezy dany adres.
pub fn ipv4_name_map() -> std::collections::HashMap<Ipv4Addr, String> {
    let mut map = std::collections::HashMap::new();
    for iface in list_interfaces() {
        for addr in &iface.ipv4_addrs {
            if let Ok(ip) = addr.parse::<Ipv4Addr>() {
                map.insert(ip, iface.name.clone());
            }
        }
    }
    map
}

/// Pelna decyzja advertise dla adresu: najpierw per-karta exclude (po nazwie),
/// potem filtry kategorii IP. `kind_map`/`name_map` z `ipv4_kind_map()` /
/// `ipv4_name_map()`. Wspoldzielona przez `filter_advertise_ips` i sanitizer
/// trusted-contacts, zeby logika wykluczania nie rozjechala sie miedzy nimi.
pub fn should_advertise_ip(
    ip: Ipv4Addr,
    filters: &AdvertiseFilters,
    kind_map: &std::collections::HashMap<Ipv4Addr, String>,
    name_map: &std::collections::HashMap<Ipv4Addr, String>,
) -> bool {
    if let Some(name) = name_map.get(&ip) {
        if filters.excluded_interfaces.iter().any(|n| n == name) {
            return false;
        }
    }
    let kind = kind_map.get(&ip).map(String::as_str).unwrap_or("unknown");
    should_advertise_interface_ipv4(ip, kind, filters)
}

/// Wyznacza bind SocketAddr dla iroh endpoint na podstawie settings.
///
/// - `auto` (default) → `0.0.0.0:port`, iroh sluchal bedzie na wszystkich IPv4.
/// - `custom` z poprawnym `bind_ipv4` ktore istnieje w aktualnych interfejsach
///   → `bind_ipv4:port`. Inaczej fallback do `0.0.0.0:port` z warnem.
///
/// `port == 0` → przekazujemy dalej, iroh sam wybierze wolny port.
pub fn resolve_bind_addr(db: &DbPool, mesh_port: u16) -> SocketAddr {
    let mode = repository::get_setting(db, SETTING_BIND_MODE)
        .ok()
        .flatten()
        .unwrap_or_else(|| "auto".to_string());

    if mode != "custom" {
        return SocketAddr::from(([0u8, 0, 0, 0], mesh_port));
    }

    let raw = repository::get_setting(db, SETTING_BIND_IPV4)
        .ok()
        .flatten()
        .unwrap_or_default();

    let parsed: Option<Ipv4Addr> = raw.parse().ok();
    let exists_on_iface = parsed
        .map(|ip| {
            list_interfaces()
                .into_iter()
                .flat_map(|i| i.ipv4_addrs)
                .any(|a| a.parse::<Ipv4Addr>().map(|v| v == ip).unwrap_or(false))
        })
        .unwrap_or(false);

    match (parsed, exists_on_iface) {
        (Some(ip), true) => SocketAddr::new(IpAddr::V4(ip), mesh_port),
        _ => {
            tracing::warn!(
                requested = %raw,
                "mesh.bind_mode=custom ale bind_ipv4 nieobecny na zadnym interfejsie — fallback 0.0.0.0"
            );
            SocketAddr::from(([0u8, 0, 0, 0], mesh_port))
        }
    }
}

/// Tryb bind iroh wczytany raz przy starcie pipeline. `Custom(ip)` oznacza pin
/// na konkretny interfejs (uzytkownik wybral go w GUI); `Auto` => bez pinu.
#[derive(Debug, Clone)]
pub enum BindModeSnapshot {
    Auto,
    Custom(Ipv4Addr),
}

/// Migawka calej logiki advertise zebrana RAZ przy starcie pipeline. iroh
/// `AddrFilter` wymaga closure `Send + Sync + 'static`, wiec nie mozemy
/// trzymac w niej `DbPool` — zamiast tego materializujemy filtry + mapy NIC
/// tutaj i `move`'ujemy ten snapshot do filtra. Zmiana ustawien i tak wymaga
/// restartu mesh pipeline, wiec statyczny snapshot na czas zycia endpointu jest
/// poprawny.
#[derive(Debug, Clone)]
pub struct AddrFilterSnapshot {
    pub bind_mode: BindModeSnapshot,
    pub filters: AdvertiseFilters,
    pub kind_map: std::collections::HashMap<Ipv4Addr, String>,
    pub name_map: std::collections::HashMap<Ipv4Addr, String>,
    /// Nazwy kart, na ktorych zyje pinowane `bind_ipv4`. Pusta gdy `Auto`.
    /// Pin custom przepuszcza WSZYSTKIE adresy IPv4 tej samej karty (jedna karta
    /// moze miec kilka adresow), nie tylko dokladnie `bind_ipv4`.
    pub pinned_iface_names: Vec<String>,
}

impl AddrFilterSnapshot {
    /// Czysta decyzja dla pojedynczego IPv4 (bez `TransportAddr`, testowalna).
    /// Relay nigdy nie przechodzi przez te funkcje — caller przepuszcza relay
    /// bezwarunkowo, zeby nie zabic fallbacku NAT.
    pub fn keep_transport_ip(&self, ip: Ipv4Addr) -> bool {
        match &self.bind_mode {
            BindModeSnapshot::Custom(pinned) => {
                if ip == *pinned {
                    return true;
                }
                // Ta sama karta co pin (kilka adresow na jednym interfejsie).
                match self.name_map.get(&ip) {
                    Some(name) => self.pinned_iface_names.iter().any(|n| n == name),
                    None => false,
                }
            }
            BindModeSnapshot::Auto => {
                should_advertise_ip(ip, &self.filters, &self.kind_map, &self.name_map)
            }
        }
    }
}

/// Buduje snapshot decyzyjny dla iroh `AddrFilter` z ustawien GUI. Wczytuje
/// `bind_mode` + `bind_ipv4` + filtry advertise + mapy NIC RAZ, zeby filtr byl
/// statyczny i thread-safe.
pub fn build_addr_filter_snapshot(db: &DbPool) -> AddrFilterSnapshot {
    let filters = load_advertise_filters(db);
    let kind_map = ipv4_kind_map();
    let name_map = ipv4_name_map();

    let mode = repository::get_setting(db, SETTING_BIND_MODE)
        .ok()
        .flatten()
        .unwrap_or_else(|| "auto".to_string());

    let (bind_mode, pinned_iface_names) = if mode == "custom" {
        let parsed: Option<Ipv4Addr> = repository::get_setting(db, SETTING_BIND_IPV4)
            .ok()
            .flatten()
            .and_then(|raw| raw.parse().ok());
        match parsed {
            Some(ip) => {
                // Nazwa karty pinowanego adresu — wszystkie jej IP przejda filtr.
                let names: Vec<String> = name_map
                    .get(&ip)
                    .map(|n| vec![n.clone()])
                    .unwrap_or_default();
                if names.is_empty() {
                    tracing::warn!(
                        pinned = %ip,
                        "mesh.bind_mode=custom: pinowany bind_ipv4 zniknal z interfejsow — filtr odrzuci wszystkie kandydatury IP, transport iroh zejdzie do relay-only"
                    );
                }
                (BindModeSnapshot::Custom(ip), names)
            }
            None => (BindModeSnapshot::Auto, Vec::new()),
        }
    } else {
        (BindModeSnapshot::Auto, Vec::new())
    };

    AddrFilterSnapshot {
        bind_mode,
        filters,
        kind_map,
        name_map,
        pinned_iface_names,
    }
}

/// Filtr listy `IpAddr` (typowo z `collect_local_addresses`) przez
/// `AdvertiseFilters`. IPv6 jest wycinane — mesh advertise dziala tylko po v4.
/// Adresy nieznane w `ipv4_kind_map()` (np. ghost addresses z sysinfo) traktujemy
/// jak `"unknown"`.
pub fn filter_advertise_ips(
    addrs: &[IpAddr],
    filters: &AdvertiseFilters,
    kind_map: &std::collections::HashMap<Ipv4Addr, String>,
    name_map: &std::collections::HashMap<Ipv4Addr, String>,
) -> Vec<IpAddr> {
    addrs
        .iter()
        .filter_map(|ip| match ip {
            IpAddr::V4(v4) => {
                if should_advertise_ip(*v4, filters, kind_map, name_map) {
                    Some(IpAddr::V4(*v4))
                } else {
                    None
                }
            }
            IpAddr::V6(_) => None,
        })
        .collect()
}

// =============================================================================
// Testy
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn all_on() -> AdvertiseFilters {
        AdvertiseFilters {
            hide_docker: true,
            hide_link_local: true,
            hide_loopback: true,
            hide_cgnat: true,
            excluded_interfaces: vec![],
        }
    }

    fn all_off() -> AdvertiseFilters {
        AdvertiseFilters {
            hide_docker: false,
            hide_link_local: false,
            hide_loopback: false,
            hide_cgnat: false,
            excluded_interfaces: vec![],
        }
    }

    #[test]
    fn loopback_hidden_when_flag_on() {
        assert!(!should_advertise_ipv4(
            Ipv4Addr::new(127, 0, 0, 1),
            &all_on()
        ));
    }

    #[test]
    fn loopback_allowed_when_flag_off() {
        assert!(should_advertise_ipv4(
            Ipv4Addr::new(127, 0, 0, 1),
            &all_off()
        ));
    }

    #[test]
    fn link_local_hidden_when_flag_on() {
        assert!(!should_advertise_ipv4(
            Ipv4Addr::new(169, 254, 1, 5),
            &all_on()
        ));
    }

    #[test]
    fn link_local_allowed_when_flag_off() {
        assert!(should_advertise_ipv4(
            Ipv4Addr::new(169, 254, 1, 5),
            &all_off()
        ));
    }

    #[test]
    fn docker_range_hidden_when_flag_on() {
        assert!(!should_advertise_ipv4(
            Ipv4Addr::new(172, 17, 0, 1),
            &all_on()
        ));
        assert!(!should_advertise_ipv4(
            Ipv4Addr::new(172, 28, 5, 10),
            &all_on()
        ));
    }

    #[test]
    fn docker_range_allowed_when_flag_off() {
        assert!(should_advertise_ipv4(
            Ipv4Addr::new(172, 17, 0, 1),
            &all_off()
        ));
    }

    #[test]
    fn cgnat_hidden_when_flag_on_for_non_tunnel() {
        assert!(!should_advertise_interface_ipv4(
            Ipv4Addr::new(100, 100, 1, 1),
            "ethernet",
            &all_on()
        ));
    }

    #[test]
    fn cgnat_allowed_when_flag_off() {
        assert!(should_advertise_ipv4(
            Ipv4Addr::new(100, 100, 1, 1),
            &all_off()
        ));
    }

    #[test]
    fn cgnat_allowed_on_tunnel_even_when_flag_on() {
        assert!(should_advertise_interface_ipv4(
            Ipv4Addr::new(100, 100, 1, 1),
            "tunnel",
            &all_on()
        ));
    }

    #[test]
    fn regular_private_not_affected_by_filters() {
        assert!(should_advertise_ipv4(
            Ipv4Addr::new(192, 168, 1, 10),
            &all_on()
        ));
        assert!(should_advertise_ipv4(
            Ipv4Addr::new(10, 0, 0, 42),
            &all_on()
        ));
    }

    #[test]
    fn sort_prefer_same_subnet_moves_match_to_front() {
        let mut addrs = vec![
            "10.0.0.5:4000".to_string(),
            "192.168.1.20:4000".to_string(),
            "172.20.0.3:4000".to_string(),
        ];
        sort_prefer_same_subnet(&mut addrs, Some("192.168.1.99:1234"));
        assert_eq!(addrs[0], "192.168.1.20:4000");
        assert_eq!(addrs.len(), 3);
    }

    #[test]
    fn sort_prefer_same_subnet_noop_without_peer() {
        let mut addrs = vec!["10.0.0.5:4000".to_string(), "192.168.1.20:4000".to_string()];
        let snapshot = addrs.clone();
        sort_prefer_same_subnet(&mut addrs, None);
        assert_eq!(addrs, snapshot);
    }

    #[test]
    fn filter_advertise_ips_drops_ipv6_and_applies_filters() {
        use std::collections::HashMap;
        use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
        let filters = all_on();
        let mut kind_map: HashMap<Ipv4Addr, String> = HashMap::new();
        kind_map.insert(Ipv4Addr::new(100, 100, 1, 1), "tunnel".to_string());
        kind_map.insert(Ipv4Addr::new(192, 168, 1, 10), "ethernet".to_string());
        let input = vec![
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10)),
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(172, 17, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(100, 100, 1, 1)),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ];
        let name_map: HashMap<Ipv4Addr, String> = HashMap::new();
        let out = filter_advertise_ips(&input, &filters, &kind_map, &name_map);
        assert_eq!(
            out,
            vec![
                IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10)),
                IpAddr::V4(Ipv4Addr::new(100, 100, 1, 1)),
            ]
        );
    }

    #[test]
    fn excluded_interface_drops_all_its_ips_regardless_of_kind() {
        use std::collections::HashMap;
        let filters = AdvertiseFilters {
            excluded_interfaces: vec!["eth3".to_string()],
            ..Default::default()
        };
        let mut kind_map: HashMap<Ipv4Addr, String> = HashMap::new();
        kind_map.insert(Ipv4Addr::new(192, 168, 1, 10), "ethernet".to_string());
        kind_map.insert(Ipv4Addr::new(10, 0, 0, 5), "ethernet".to_string());
        let mut name_map: HashMap<Ipv4Addr, String> = HashMap::new();
        name_map.insert(Ipv4Addr::new(192, 168, 1, 10), "eth2".to_string());
        name_map.insert(Ipv4Addr::new(10, 0, 0, 5), "eth3".to_string());
        let input = vec![
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10)),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)),
        ];
        let out = filter_advertise_ips(&input, &filters, &kind_map, &name_map);
        // eth3 wykluczony per-karta, eth2 zostaje — mimo ze oba to ethernet.
        assert_eq!(out, vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10))]);
    }

    fn pinned_snapshot() -> AddrFilterSnapshot {
        use std::collections::HashMap;
        // Pin na karcie "eth0" (192.168.1.50); docker + tailscale na osobnych
        // kartach. eth0 ma drugi adres alias 192.168.1.51 — pin custom musi
        // przepuscic oba (ta sama karta), ale juz nie docker/tailscale.
        let mut name_map: HashMap<Ipv4Addr, String> = HashMap::new();
        name_map.insert(Ipv4Addr::new(192, 168, 1, 50), "eth0".to_string());
        name_map.insert(Ipv4Addr::new(192, 168, 1, 51), "eth0".to_string());
        name_map.insert(Ipv4Addr::new(172, 17, 0, 1), "docker0".to_string());
        name_map.insert(Ipv4Addr::new(100, 100, 1, 1), "tailscale0".to_string());
        let mut kind_map: HashMap<Ipv4Addr, String> = HashMap::new();
        kind_map.insert(Ipv4Addr::new(192, 168, 1, 50), "ethernet".to_string());
        kind_map.insert(Ipv4Addr::new(192, 168, 1, 51), "ethernet".to_string());
        kind_map.insert(Ipv4Addr::new(172, 17, 0, 1), "docker".to_string());
        kind_map.insert(Ipv4Addr::new(100, 100, 1, 1), "tunnel".to_string());
        AddrFilterSnapshot {
            bind_mode: BindModeSnapshot::Custom(Ipv4Addr::new(192, 168, 1, 50)),
            filters: AdvertiseFilters::default(),
            kind_map,
            name_map,
            pinned_iface_names: vec!["eth0".to_string()],
        }
    }

    #[test]
    fn custom_pin_keeps_only_pinned_iface_ips() {
        let snap = pinned_snapshot();
        assert!(snap.keep_transport_ip(Ipv4Addr::new(192, 168, 1, 50)));
        assert!(snap.keep_transport_ip(Ipv4Addr::new(192, 168, 1, 51)));
        assert!(!snap.keep_transport_ip(Ipv4Addr::new(172, 17, 0, 1)));
        assert!(!snap.keep_transport_ip(Ipv4Addr::new(100, 100, 1, 1)));
        // Adres spoza zadnej znanej karty (np. WAN portmap kandydat) — odrzucony.
        assert!(!snap.keep_transport_ip(Ipv4Addr::new(8, 8, 8, 8)));
    }

    #[test]
    fn auto_mode_applies_advertise_filters() {
        use std::collections::HashMap;
        let mut kind_map: HashMap<Ipv4Addr, String> = HashMap::new();
        kind_map.insert(Ipv4Addr::new(192, 168, 1, 50), "ethernet".to_string());
        kind_map.insert(Ipv4Addr::new(172, 17, 0, 1), "docker".to_string());
        let snap = AddrFilterSnapshot {
            bind_mode: BindModeSnapshot::Auto,
            filters: all_on(),
            kind_map,
            name_map: HashMap::new(),
            pinned_iface_names: Vec::new(),
        };
        assert!(snap.keep_transport_ip(Ipv4Addr::new(192, 168, 1, 50)));
        // docker schowany przez hide_docker w trybie auto.
        assert!(!snap.keep_transport_ip(Ipv4Addr::new(172, 17, 0, 1)));
    }

    #[test]
    fn transport_addr_filter_keeps_relay_and_pinned_only() {
        use iroh::TransportAddr;
        let snap = pinned_snapshot();
        let input: Vec<TransportAddr> = vec![
            TransportAddr::Relay("https://relay.example".parse().unwrap()),
            TransportAddr::Ip("192.168.1.50:8090".parse().unwrap()),
            TransportAddr::Ip("172.17.0.1:8090".parse().unwrap()),
            TransportAddr::Ip("100.100.1.1:8090".parse().unwrap()),
        ];
        let kept: Vec<TransportAddr> = input
            .iter()
            .filter(|a| match a {
                TransportAddr::Relay(_) => true,
                TransportAddr::Ip(sa) => match sa.ip() {
                    std::net::IpAddr::V4(v4) => snap.keep_transport_ip(v4),
                    std::net::IpAddr::V6(_) => true,
                },
                _ => true,
            })
            .cloned()
            .collect();
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().any(|a| a.is_relay()));
        assert!(kept
            .iter()
            .any(|a| matches!(a, TransportAddr::Ip(sa) if sa.ip() == std::net::IpAddr::V4(Ipv4Addr::new(192,168,1,50)))));
    }

    #[test]
    fn sort_prefer_same_subnet_noop_without_match() {
        let mut addrs = vec!["10.0.0.5:4000".to_string(), "192.168.1.20:4000".to_string()];
        let snapshot = addrs.clone();
        sort_prefer_same_subnet(&mut addrs, Some("172.16.1.1:9000"));
        assert_eq!(addrs, snapshot);
    }
}

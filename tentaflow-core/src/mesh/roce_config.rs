// =============================================================================
// Plik: mesh/roce_config.rs
// Opis: Node-lokalna enumeracja RoCE/RDMA interfejsow dla cluster-create
//       network auto-config. Czyta /sys, mapuje netdev -> urzadzenie RoCE,
//       wykrywa "twins" (dwa netdevy jednego portu QSFP) i ich brak adresu IP.
// Przyklad: enumerate_roce_interfaces() -> Vec<RoceInterfaceInfo>
// =============================================================================

use std::net::IpAddr;

use tentaflow_protocol::mesh::RoceInterfaceInfo;

/// Enumeruje wszystkie netdevy posiadajace urzadzenie RoCE/IB (czyli karty
/// zdolne do RDMA). Na DGX Spark jeden fizyczny port QSFP eksponuje DWA takie
/// netdevy ("twins") dzielace jeden link PCIe — jeden zwykle ma adres IP, drugi
/// jest UP bez adresu. Orkiestrator cluster-create dokonfigurowuje ten drugi.
///
/// Implementacja jest linuxowa (sciezki /sys); na innych platformach zwraca
/// pusta liste — DGX Spark / serwery RoCE to wylacznie Linux.
pub fn enumerate_roce_interfaces() -> Vec<RoceInterfaceInfo> {
    #[cfg(target_os = "linux")]
    {
        enumerate_linux()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Vec::new()
    }
}

#[cfg(target_os = "linux")]
fn enumerate_linux() -> Vec<RoceInterfaceInfo> {
    let ip_map = collect_ipv4_map();
    let mut out = Vec::new();

    let entries = match std::fs::read_dir("/sys/class/net") {
        Ok(e) => e,
        Err(_) => return out,
    };

    for entry in entries.flatten() {
        let netdev = entry.file_name().to_string_lossy().to_string();
        if netdev == "lo" {
            continue;
        }

        // Urzadzenie RoCE noda zyje pod /sys/class/net/<netdev>/device/infiniband/<ibdev>.
        // Brak tego katalogu => karta nie ma RDMA, pomijamy.
        let Some(roce_device) = read_roce_device(&netdev) else {
            continue;
        };

        // Wszystkie adresy IPv4 karty — interconnect klastra moze byc adresem
        // sekundarnym, wiec nie wolno gubic pozostalych (P2-2).
        let addrs = ip_map.get(&netdev).cloned().unwrap_or_default();
        let (ipv4, netmask, ipv4_aliases) = match addrs.split_first() {
            Some(((ip, mask), rest)) => (
                Some(ip.clone()),
                Some(mask.clone()),
                rest.iter().map(|(a, _)| a.clone()).collect(),
            ),
            None => (None, None, Vec::new()),
        };

        let pci_slot = read_pci_slot(&netdev);
        out.push(RoceInterfaceInfo {
            netdev: netdev.clone(),
            roce_device,
            ipv4,
            netmask,
            ipv4_aliases,
            mtu: read_u32(&format!("/sys/class/net/{}/mtu", netdev)).unwrap_or(1500),
            link_up: read_link_up(&netdev),
            speed_mbps: read_speed_mbps(&netdev),
            group_key: compute_group_key(&netdev, &pci_slot),
            pci_slot,
        });
    }

    // Stabilna kolejnosc (po netdev) zeby plan IP byl deterministyczny.
    out.sort_by(|a, b| a.netdev.cmp(&b.netdev));
    out
}

/// Odczytuje nazwe urzadzenia RoCE/IB powiazanego z netdev. Zwraca pierwszy
/// (zwykle jedyny) wpis katalogu `device/infiniband`.
#[cfg(target_os = "linux")]
fn read_roce_device(netdev: &str) -> Option<String> {
    let path = format!("/sys/class/net/{}/device/infiniband", netdev);
    let mut entries = std::fs::read_dir(&path).ok()?;
    entries
        .find_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
}

/// Link UP gdy carrier == "1" (preferowane) albo operstate == "up".
#[cfg(target_os = "linux")]
fn read_link_up(netdev: &str) -> bool {
    if let Ok(c) = std::fs::read_to_string(format!("/sys/class/net/{}/carrier", netdev)) {
        return c.trim() == "1";
    }
    std::fs::read_to_string(format!("/sys/class/net/{}/operstate", netdev))
        .map(|s| s.trim() == "up")
        .unwrap_or(false)
}

/// Predkosc linku w Mbps. `/sys/.../speed` bywa -1 gdy link down — wtedy 0.
#[cfg(target_os = "linux")]
fn read_speed_mbps(netdev: &str) -> u64 {
    std::fs::read_to_string(format!("/sys/class/net/{}/speed", netdev))
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|v| *v > 0)
        .map(|v| v as u64)
        .unwrap_or(0)
}

#[cfg(target_os = "linux")]
fn read_u32(path: &str) -> Option<u32> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
}

/// Realpath symlinku `device` daje sciezke PCI (np. .../0001:01:00.0).
/// Referencyjna sciezka slotu; grupowanie twins idzie przez `compute_group_key`.
#[cfg(target_os = "linux")]
fn read_pci_slot(netdev: &str) -> String {
    std::fs::canonicalize(format!("/sys/class/net/{}/device", netdev))
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Klucz grupowania "twins" jednego fizycznego portu QSFP. Najpewniejszy sygnal
/// to `phys_switch_id` — porty tego samego ASIC ConnectX maja go identyczny, NAWET
/// gdy netdevy siedza w roznych domenach PCI (przypadek DGX Spark:
/// `enP2p1s0f0np0` w domenie 2 vs `enp1s0f0np0` w domenie 0). Gdy `phys_switch_id`
/// pusty (NIC w trybie legacy), fallback to RODZIC sciezki PCI (upstream port),
/// ktory grupuje funkcje/porty jednej karty, a osobne karty rozdziela. Pusty gdy
/// brak obu sygnalow — planer wtedy nie laczy takiej karty z zadnym twinem.
#[cfg(target_os = "linux")]
fn compute_group_key(netdev: &str, pci_slot: &str) -> String {
    let switch_id = std::fs::read_to_string(format!("/sys/class/net/{}/phys_switch_id", netdev))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if !switch_id.is_empty() {
        return format!("switch:{}", switch_id);
    }
    // Rodzic endpointu PCI = upstream bridge wspoldzielony przez porty jednej karty.
    match pci_slot.rfind('/') {
        Some(idx) if idx > 0 => format!("pci:{}", &pci_slot[..idx]),
        _ => String::new(),
    }
}

/// Mapuje netdev -> WSZYSTKIE adresy IPv4 (ip, netmask dotted). Karta moze miec
/// adresy sekundarne — interconnect klastra moze byc jednym z nich, wiec
/// zwracamy komplet (P2-2), a nie tylko pierwszy.
#[cfg(target_os = "linux")]
fn collect_ipv4_map() -> std::collections::HashMap<String, Vec<(String, String)>> {
    let mut map: std::collections::HashMap<String, Vec<(String, String)>> =
        std::collections::HashMap::new();
    let networks = sysinfo::Networks::new_with_refreshed_list();
    for (name, data) in networks.iter() {
        for net in data.ip_networks() {
            if let IpAddr::V4(v4) = net.addr {
                if v4.is_loopback() {
                    continue;
                }
                let prefix = net.prefix;
                let mask = if prefix >= 32 {
                    u32::MAX
                } else {
                    u32::MAX << (32 - prefix)
                };
                let netmask = std::net::Ipv4Addr::from(mask);
                let entry = map.entry(name.clone()).or_default();
                let ip_str = v4.to_string();
                if !entry.iter().any(|(a, _)| a == &ip_str) {
                    entry.push((ip_str, netmask.to_string()));
                }
            }
        }
    }
    map
}

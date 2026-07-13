// =============================================================================
// Plik: mesh/cluster_rdma.rs
// Opis: Czysta logika planu RDMA auto-config klastra. Wyznacza cluster-wide
//       (wszyscy czlonkowie naraz), ktore "twins" jednego portu QSFP dostaja
//       jaki adres IP i MTU. Bezpieczne dla realnej sieci: grupuje twins po
//       fizycznym porcie, NIE nadpisuje aktywnych kart, odrzuca kolizje adresow
//       i adresy nielegalne (wraparound /24, host .0/.255). Deterministyczne,
//       idempotentne.
// Przyklad: plan_cluster(&inputs, 9000, &reserved)
// =============================================================================

use std::collections::HashSet;
use std::net::Ipv4Addr;

use tentaflow_protocol::mesh::RoceInterfaceInfo;

/// Domyslne MTU dla kart RoCE (jumbo frames) gdy nie podano innego.
pub const DEFAULT_RDMA_MTU: u32 = 9000;

/// Wejscie planu dla jednego czlonka klastra.
pub struct MemberRoceInput {
    pub node_id: String,
    /// Adres interconnectu wybrany przez network-probe (`cluster_members.interface_ip`).
    pub primary_ip: String,
    /// Karty RoCE zebrane z noda (`RoceProbe`).
    pub roce: Vec<RoceInterfaceInfo>,
}

/// Pojedynczy interfejs w planie konfiguracji RDMA noda.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RdmaPlanInterface {
    pub netdev: String,
    pub roce_device: String,
    /// "primary" (juz nosi adres interconnectu klastra) | "secondary"
    /// (drugi twin tego samego portu QSFP, dokonfigurowywany).
    pub role: &'static str,
    /// Adres docelowy, ktory karta ma miec po konfiguracji.
    pub ipv4: String,
    pub netmask: String,
    pub mtu: u32,
    /// Czy adres trzeba przypisac (twin bez adresu, rozny od docelowego).
    pub needs_ip_change: bool,
    /// Czy MTU trzeba zmienic (rozni sie od docelowego).
    pub needs_mtu_change: bool,
}

/// Plan RDMA dla jednego noda klastra.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRdmaPlan {
    pub interfaces: Vec<RdmaPlanInterface>,
    /// Lista urzadzen RoCE (OBA twins) do `NCCL_IB_HCA`, np.
    /// "roceP2p1s0f0,rocep1s0f0". Primary pierwszy. To INTENCJA — handler
    /// utrwala ja dopiero po pelnym sukcesie aplikacji.
    pub rdma_devices: String,
    /// Adres podstawowy, do ktorego wpina sie distributed deploy.
    pub primary_ip: String,
    /// Netdev portu QSFP nosacy `primary_ip` (NCCL_SOCKET_IFNAME bootstrap).
    pub socket_ifname: String,
}

/// Wyznacza plan RDMA dla CALEGO klastra naraz.
///
/// Cluster-wide jest konieczne dla bezpieczenstwa: najpierw zbieramy WSZYSTKIE
/// istniejace adresy (primary + sekundarne) wszystkich nodow plus `reserved_ips`
/// (np. znane z DB adresy interconnectu nodow, ktorych nie udalo sie odpytac),
/// a potem kazdy przydzial sekundarnego adresu sprawdzamy przeciw temu zbiorowi
/// ORAZ przeciw juz zaplanowanym adresom. Dzieki temu zaden zaplanowany adres nie
/// zderzy sie z cudzym primary ani z innym zaplanowanym (P1-2).
///
/// Zwraca wynik per node w kolejnosci `members` (Err = ten node nie zostal
/// skonfigurowany; reszta nadal moze sie powiesc).
pub fn plan_cluster(
    members: &[MemberRoceInput],
    target_mtu: u32,
    reserved_ips: &[String],
) -> Vec<(String, Result<MemberRdmaPlan, String>)> {
    let mut existing: HashSet<Ipv4Addr> = HashSet::new();
    for m in members {
        for r in &m.roce {
            if let Some(ip) = r.ipv4.as_deref().and_then(|s| s.parse().ok()) {
                existing.insert(ip);
            }
            for a in &r.ipv4_aliases {
                if let Ok(ip) = a.parse() {
                    existing.insert(ip);
                }
            }
        }
        if let Ok(ip) = m.primary_ip.parse() {
            existing.insert(ip);
        }
    }
    for r in reserved_ips {
        if let Ok(ip) = r.parse() {
            existing.insert(ip);
        }
    }

    let mut planned: HashSet<Ipv4Addr> = HashSet::new();
    members
        .iter()
        .map(|m| {
            let res = plan_one(m, target_mtu, &existing, &mut planned);
            (m.node_id.clone(), res)
        })
        .collect()
}

/// Czy dany interfejs nosi (jako primary lub alias) podany adres.
fn iface_has_ip(iface: &RoceInterfaceInfo, ip: &str) -> bool {
    iface.ipv4.as_deref() == Some(ip) || iface.ipv4_aliases.iter().any(|a| a == ip)
}

/// Czy interfejs ma JAKIKOLWIEK adres IPv4.
fn iface_has_any_ip(iface: &RoceInterfaceInfo) -> bool {
    iface.ipv4.is_some() || !iface.ipv4_aliases.is_empty()
}

fn plan_one(
    member: &MemberRoceInput,
    target_mtu: u32,
    existing: &HashSet<Ipv4Addr>,
    planned: &mut HashSet<Ipv4Addr>,
) -> Result<MemberRdmaPlan, String> {
    let primary_ip = member.primary_ip.as_str();
    let up: Vec<&RoceInterfaceInfo> = member.roce.iter().filter(|r| r.link_up).collect();
    if up.is_empty() {
        return Err("node nie ma zadnej karty RoCE z aktywnym linkiem".to_string());
    }

    // Primary = karta nosaca adres interconnectu klastra (takze jako alias).
    let primary = up
        .iter()
        .copied()
        .find(|r| iface_has_ip(r, primary_ip))
        .ok_or_else(|| {
            format!(
                "zaden interfejs RoCE noda nie nosi adresu interconnectu {} \
                 (network-probe musi najpierw wybrac karte RoCE)",
                primary_ip
            )
        })?;

    let base: Ipv4Addr = primary_ip
        .parse()
        .map_err(|_| format!("primary_ip nie jest poprawnym IPv4: {}", primary_ip))?;
    let o = base.octets();
    // Host octet .0/.255 dalby adres sieci/broadcast na sekundarnej podsieci (P1-2).
    if o[3] == 0 || o[3] == 255 {
        return Err(format!(
            "host octet adresu {} to .{} (siec/broadcast) — nie da sie z niego \
             wyprowadzic bezpiecznych adresow RDMA",
            primary_ip, o[3]
        ));
    }

    // Twins = karty tego samego fizycznego portu QSFP. Grupujemy po `group_key`
    // (phys_switch_id albo upstream PCI), zeby NIE ruszyc niepowiazanych aktywnych
    // kart RDMA (P1-1). Gdy node nie dostarczyl sygnalu grupowania (pusty key),
    // fallback do wszystkich UP RoCE != primary — i tak chroni nas guard clobber
    // nizej (karta z innym adresem nie zostanie nadpisana).
    let pgroup = primary.group_key.as_str();
    let mut twins: Vec<&RoceInterfaceInfo> = if !pgroup.is_empty() {
        up.iter()
            .copied()
            .filter(|r| r.netdev != primary.netdev && r.group_key == pgroup)
            .collect()
    } else {
        up.iter()
            .copied()
            .filter(|r| r.netdev != primary.netdev)
            .collect()
    };
    twins.sort_by(|a, b| a.netdev.cmp(&b.netdev));

    if twins.is_empty() {
        return Err(format!(
            "nie znaleziono twina RoCE w grupie portu interconnectu {} \
             (oczekiwano drugiej karty tego samego QSFP)",
            primary.netdev
        ));
    }

    let primary_mask = primary
        .netmask
        .clone()
        .unwrap_or_else(|| "255.255.255.0".to_string());

    let mut interfaces = Vec::new();
    let mut roce_devices = vec![primary.roce_device.clone()];

    // Primary: adres zostaje, ewentualnie korygujemy MTU (non-destructive — patrz
    // handler: MTU-only nie przepisuje konfiguracji IP).
    interfaces.push(RdmaPlanInterface {
        netdev: primary.netdev.clone(),
        roce_device: primary.roce_device.clone(),
        role: "primary",
        ipv4: primary_ip.to_string(),
        netmask: primary_mask,
        mtu: target_mtu,
        needs_ip_change: false,
        needs_mtu_change: primary.mtu != target_mtu,
    });

    for (idx, twin) in twins.iter().enumerate() {
        // Sekundarna podsiec: trzeci oktet primary + (idx+1), ten sam host oktet.
        let new_third = o[2] as u16 + (idx as u16) + 1;
        if new_third > 255 {
            return Err(format!(
                "wyprowadzenie podsieci RDMA z {} przekroczyloby trzeci oktet 255 \
                 (wraparound) — przenies interconnect na nizsza podsiec",
                primary_ip
            ));
        }
        let target = Ipv4Addr::new(o[0], o[1], new_third as u8, o[3]);
        let target_str = target.to_string();

        let needs_ip_change = if iface_has_ip(twin, &target_str) {
            // Idempotentne: twin juz ma docelowy adres.
            false
        } else if iface_has_any_ip(twin) {
            // Twin MA inny adres — to aktywny interfejs. NIE nadpisujemy (P1-1).
            return Err(format!(
                "twin {} ma juz adres {} — odmawiam nadpisania aktywnego interfejsu",
                twin.netdev,
                twin.ipv4.clone().unwrap_or_default()
            ));
        } else {
            // Twin bez adresu — bezpieczny do przypisania. Sprawdz kolizje (P1-2).
            if existing.contains(&target) || planned.contains(&target) {
                return Err(format!(
                    "zaplanowany adres RDMA {} dla {} koliduje z istniejacym/innym \
                     zaplanowanym adresem w klastrze",
                    target_str, twin.netdev
                ));
            }
            planned.insert(target);
            true
        };

        roce_devices.push(twin.roce_device.clone());
        interfaces.push(RdmaPlanInterface {
            netdev: twin.netdev.clone(),
            roce_device: twin.roce_device.clone(),
            role: "secondary",
            ipv4: target_str,
            netmask: "255.255.255.0".to_string(),
            mtu: target_mtu,
            needs_ip_change,
            needs_mtu_change: twin.mtu != target_mtu,
        });
    }

    Ok(MemberRdmaPlan {
        interfaces,
        rdma_devices: roce_devices.join(","),
        primary_ip: primary_ip.to_string(),
        socket_ifname: primary.netdev.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iface(
        netdev: &str,
        roce: &str,
        ip: Option<&str>,
        mtu: u32,
        group: &str,
    ) -> RoceInterfaceInfo {
        RoceInterfaceInfo {
            netdev: netdev.to_string(),
            roce_device: roce.to_string(),
            ipv4: ip.map(String::from),
            netmask: ip.map(|_| "255.255.255.0".to_string()),
            ipv4_aliases: Vec::new(),
            mtu,
            link_up: true,
            speed_mbps: 200_000,
            pci_slot: "/sys/devices/pci/x".to_string(),
            group_key: group.to_string(),
        }
    }

    fn member(node: &str, primary_ip: &str, roce: Vec<RoceInterfaceInfo>) -> MemberRoceInput {
        MemberRoceInput {
            node_id: node.to_string(),
            primary_ip: primary_ip.to_string(),
            roce,
        }
    }

    fn ok_plan(
        results: Vec<(String, Result<MemberRdmaPlan, String>)>,
        node: &str,
    ) -> MemberRdmaPlan {
        results
            .into_iter()
            .find(|(n, _)| n == node)
            .unwrap()
            .1
            .unwrap()
    }

    #[test]
    fn twin_gets_second_subnet_same_host_octet() {
        let roce = vec![
            iface(
                "enP2p1s0f0np0",
                "roceP2p1s0f0",
                Some("10.10.10.24"),
                1500,
                "switch:abc",
            ),
            iface("enp1s0f0np0", "rocep1s0f0", None, 1500, "switch:abc"),
        ];
        let res = plan_cluster(&[member("n24", "10.10.10.24", roce)], 9000, &[]);
        let plan = ok_plan(res, "n24");

        assert_eq!(plan.socket_ifname, "enP2p1s0f0np0");
        assert_eq!(plan.rdma_devices, "roceP2p1s0f0,rocep1s0f0");
        let secondary = &plan.interfaces[1];
        assert_eq!(secondary.ipv4, "10.10.11.24");
        assert!(secondary.needs_ip_change);
        assert!(plan.interfaces[0].needs_mtu_change);
    }

    #[test]
    fn idempotent_when_twin_already_configured() {
        let roce = vec![
            iface(
                "enP2p1s0f0np0",
                "roceP2p1s0f0",
                Some("10.10.10.25"),
                9000,
                "switch:z",
            ),
            iface(
                "enp1s0f0np0",
                "rocep1s0f0",
                Some("10.10.11.25"),
                9000,
                "switch:z",
            ),
        ];
        let res = plan_cluster(&[member("n25", "10.10.10.25", roce)], 9000, &[]);
        let plan = ok_plan(res, "n25");
        for i in &plan.interfaces {
            assert!(!i.needs_ip_change, "{:?}", i);
            assert!(!i.needs_mtu_change, "{:?}", i);
        }
    }

    #[test]
    fn matches_primary_on_secondary_address() {
        // Interconnect IP is a SECONDARY address on the netdev (P2-2).
        let mut prim = iface(
            "enP2p1s0f0np0",
            "roceP2p1s0f0",
            Some("10.0.0.1"),
            1500,
            "switch:s",
        );
        prim.ipv4_aliases = vec!["10.10.10.24".to_string()];
        let roce = vec![
            prim,
            iface("enp1s0f0np0", "rocep1s0f0", None, 1500, "switch:s"),
        ];
        let res = plan_cluster(&[member("n", "10.10.10.24", roce)], 9000, &[]);
        let plan = ok_plan(res, "n");
        assert_eq!(plan.socket_ifname, "enP2p1s0f0np0");
        assert_eq!(plan.interfaces[1].ipv4, "10.10.11.24");
    }

    #[test]
    fn does_not_clobber_unrelated_active_rdma_iface() {
        // Second RoCE iface in the SAME group already carries a different address.
        let roce = vec![
            iface(
                "enP2p1s0f0np0",
                "roceP2p1s0f0",
                Some("10.10.10.24"),
                1500,
                "switch:s",
            ),
            iface(
                "enp1s0f0np0",
                "rocep1s0f0",
                Some("192.168.50.7"),
                1500,
                "switch:s",
            ),
        ];
        let res = plan_cluster(&[member("n", "10.10.10.24", roce)], 9000, &[]);
        let err = res.into_iter().next().unwrap().1.unwrap_err();
        assert!(err.contains("odmawiam nadpisania"), "{}", err);
    }

    #[test]
    fn ignores_roce_iface_in_other_group() {
        // A spare unconfigured RoCE port in a DIFFERENT physical group must not be
        // treated as the twin (P1-1). Here the only same-group twin is missing.
        let roce = vec![
            iface(
                "enP2p1s0f0np0",
                "roceP2p1s0f0",
                Some("10.10.10.24"),
                1500,
                "switch:A",
            ),
            iface("enp9s0f0np0", "rocep9s0f0", None, 1500, "switch:B"),
        ];
        let res = plan_cluster(&[member("n", "10.10.10.24", roce)], 9000, &[]);
        let err = res.into_iter().next().unwrap().1.unwrap_err();
        assert!(err.contains("twina RoCE w grupie"), "{}", err);
    }

    #[test]
    fn rejects_third_octet_wraparound() {
        let roce = vec![
            iface(
                "enP2p1s0f0np0",
                "roceP2p1s0f0",
                Some("10.10.255.24"),
                1500,
                "switch:s",
            ),
            iface("enp1s0f0np0", "rocep1s0f0", None, 1500, "switch:s"),
        ];
        let res = plan_cluster(&[member("n", "10.10.255.24", roce)], 9000, &[]);
        let err = res.into_iter().next().unwrap().1.unwrap_err();
        assert!(err.contains("wraparound"), "{}", err);
    }

    #[test]
    fn rejects_host_octet_zero_or_broadcast() {
        let roce = vec![
            iface(
                "enP2p1s0f0np0",
                "roceP2p1s0f0",
                Some("10.10.10.255"),
                1500,
                "switch:s",
            ),
            iface("enp1s0f0np0", "rocep1s0f0", None, 1500, "switch:s"),
        ];
        let res = plan_cluster(&[member("n", "10.10.10.255", roce)], 9000, &[]);
        let err = res.into_iter().next().unwrap().1.unwrap_err();
        assert!(err.contains("siec/broadcast"), "{}", err);
    }

    #[test]
    fn rejects_cross_member_collision() {
        // Node A's planned secondary (10.10.11.24) equals node B's existing primary.
        let a = member(
            "A",
            "10.10.10.24",
            vec![
                iface(
                    "enP2p1s0f0np0",
                    "roceA0",
                    Some("10.10.10.24"),
                    1500,
                    "switch:a",
                ),
                iface("enp1s0f0np0", "roceA1", None, 1500, "switch:a"),
            ],
        );
        let b = member(
            "B",
            "10.10.11.24",
            vec![
                iface(
                    "enP2p1s0f0np0",
                    "roceB0",
                    Some("10.10.11.24"),
                    1500,
                    "switch:b",
                ),
                iface("enp1s0f0np0", "roceB1", None, 1500, "switch:b"),
            ],
        );
        let res = plan_cluster(&[a, b], 9000, &[]);
        let a_res = res.iter().find(|(n, _)| n == "A").unwrap().1.clone();
        assert!(a_res.unwrap_err().contains("koliduje"));
    }

    #[test]
    fn deterministic_non_conflicting_across_nodes() {
        let n24 = member(
            "n24",
            "10.10.10.24",
            vec![
                iface(
                    "enP2p1s0f0np0",
                    "roceP2p1s0f0",
                    Some("10.10.10.24"),
                    1500,
                    "switch:a",
                ),
                iface("enp1s0f0np0", "rocep1s0f0", None, 1500, "switch:a"),
            ],
        );
        let n25 = member(
            "n25",
            "10.10.10.25",
            vec![
                iface(
                    "enP2p1s0f0np0",
                    "roceP2p1s0f0",
                    Some("10.10.10.25"),
                    1500,
                    "switch:b",
                ),
                iface("enp1s0f0np0", "rocep1s0f0", None, 1500, "switch:b"),
            ],
        );
        let res = plan_cluster(&[n24, n25], 9000, &[]);
        assert_eq!(
            ok_plan(res.clone(), "n24").interfaces[1].ipv4,
            "10.10.11.24"
        );
        assert_eq!(ok_plan(res, "n25").interfaces[1].ipv4, "10.10.11.25");
    }

    #[test]
    fn errors_when_primary_ip_not_on_roce() {
        let roce = vec![iface("enp1s0f0np0", "rocep1s0f0", None, 1500, "switch:s")];
        let res = plan_cluster(&[member("n", "192.168.11.24", roce)], 9000, &[]);
        let err = res.into_iter().next().unwrap().1.unwrap_err();
        assert!(err.contains("interconnect"));
    }
}

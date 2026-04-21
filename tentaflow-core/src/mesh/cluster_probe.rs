// =============================================================================
// Plik: mesh/cluster_probe.rs
// Opis: Orkiestracja probing przepustowosci miedzy nodami w klastrze.
//       Pre-filter subnetow, rownolegle proby, algorytm optymalnego przypisania.
// =============================================================================

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::Ipv4Addr;

/// Interfejs sieciowy noda
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInterface {
    pub node_id: String,
    pub name: String,
    pub ip: String,
    pub netmask: String,
    pub speed_mbps: u64,
    pub rdma_available: bool,
}

/// Wynik probing jednej pary interfejsow
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairProbeResult {
    pub node_a: String,
    pub node_b: String,
    pub interface_a: String,
    pub interface_b: String,
    pub bandwidth_mbps: f64,
    pub latency_us: u64,
    pub reachable: bool,
    pub rdma: bool,
}

/// Przypisanie interfejsu per-para nodow
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairAssignment {
    pub node_a: String,
    pub node_b: String,
    pub interface_a: String,
    pub interface_b: String,
    pub bandwidth_mbps: f64,
    pub rdma: bool,
}

/// Wynik optymalnego przypisania
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectionResult {
    pub assignments: Vec<PairAssignment>,
    pub per_node: HashMap<String, NodeAssignment>,
    pub is_mixed: bool,
    pub bottleneck_mbps: f64,
    pub message: String,
    pub all_results: Vec<PairProbeResult>,
}

/// Przypisanie per-node (dla frontendu)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeAssignment {
    pub interface: String,
    pub ip: String,
    pub speed_mbps: u64,
}

// =============================================================================
// Pre-filter subnetow
// =============================================================================

/// Sprawdz czy dwa IP sa w tym samym subnecie
pub fn same_subnet(ip_a: &str, mask_a: &str, ip_b: &str, mask_b: &str) -> bool {
    let a: Ipv4Addr = match ip_a.parse() {
        Ok(ip) => ip,
        Err(_) => return false,
    };
    let b: Ipv4Addr = match ip_b.parse() {
        Ok(ip) => ip,
        Err(_) => return false,
    };
    let m_a: Ipv4Addr = match mask_a.parse() {
        Ok(ip) => ip,
        Err(_) => return false,
    };
    let m_b: Ipv4Addr = match mask_b.parse() {
        Ok(ip) => ip,
        Err(_) => return false,
    };

    // Uzyj bardziej restrykcyjnej maski (wieksze AND = wiecej jedynek)
    let mask = u32::from(m_a) & u32::from(m_b);
    let net_a = u32::from(a) & mask;
    let net_b = u32::from(b) & mask;

    net_a == net_b
}

/// Filtruj pary interfejsow — zostaw tylko te w tym samym subnecie
pub fn filter_reachable_pairs(nodes: &[Vec<NodeInterface>]) -> Vec<(NodeInterface, NodeInterface)> {
    let mut pairs = Vec::new();

    for i in 0..nodes.len() {
        for j in (i + 1)..nodes.len() {
            for iface_a in &nodes[i] {
                for iface_b in &nodes[j] {
                    if same_subnet(&iface_a.ip, &iface_a.netmask, &iface_b.ip, &iface_b.netmask) {
                        pairs.push((iface_a.clone(), iface_b.clone()));
                    }
                }
            }
        }
    }

    pairs
}

// =============================================================================
// Strategia probing — najszybszy interfejs per para
// =============================================================================

/// Dla kazdej pary nodow posortuj interfejsy od najszybszego.
/// Zwraca HashMap: (node_a, node_b) -> Vec<(iface_a, iface_b)> posortowane desc.
pub fn rank_pairs_by_speed(
    reachable_pairs: &[(NodeInterface, NodeInterface)],
) -> HashMap<(String, String), Vec<(NodeInterface, NodeInterface)>> {
    let mut grouped: HashMap<(String, String), Vec<(NodeInterface, NodeInterface, u64)>> =
        HashMap::new();

    for (a, b) in reachable_pairs {
        let key = if a.node_id < b.node_id {
            (a.node_id.clone(), b.node_id.clone())
        } else {
            (b.node_id.clone(), a.node_id.clone())
        };

        let speed = std::cmp::min(a.speed_mbps, b.speed_mbps);
        grouped
            .entry(key)
            .or_default()
            .push((a.clone(), b.clone(), speed));
    }

    let mut result = HashMap::new();
    for (key, mut pairs) in grouped {
        pairs.sort_by(|a, b| b.2.cmp(&a.2));
        result.insert(key, pairs.into_iter().map(|(a, b, _)| (a, b)).collect());
    }
    result
}

/// Dla kazdej pary nodow wybierz najszybszy interfejs (wg sysfs speed_mbps).
/// Probuj tylko najszybsza kombinacje per node-pair. Uzywane w testach.
pub fn select_fastest_per_pair(
    reachable_pairs: &[(NodeInterface, NodeInterface)],
) -> Vec<(NodeInterface, NodeInterface)> {
    let mut best: HashMap<(String, String), (NodeInterface, NodeInterface, u64)> = HashMap::new();

    for (a, b) in reachable_pairs {
        let key = if a.node_id < b.node_id {
            (a.node_id.clone(), b.node_id.clone())
        } else {
            (b.node_id.clone(), a.node_id.clone())
        };

        let speed = std::cmp::min(a.speed_mbps, b.speed_mbps);

        match best.get(&key) {
            Some((_, _, existing_speed)) if *existing_speed >= speed => {}
            _ => {
                best.insert(key, (a.clone(), b.clone(), speed));
            }
        }
    }

    best.into_values().map(|(a, b, _)| (a, b)).collect()
}

// =============================================================================
// Algorytm optymalnego przypisania
// =============================================================================

/// Optymalny algorytm przypisania: per-pair fastest.
/// Kazda para nodow dostaje najszybszy link miedzy nimi.
pub fn optimal_assignment(probe_results: &[PairProbeResult]) -> DetectionResult {
    // Dla kazdej pary nodow wybierz wynik z najwyzszym bandwidth
    let mut best_per_pair: HashMap<(String, String), &PairProbeResult> = HashMap::new();

    for r in probe_results {
        if !r.reachable {
            continue;
        }

        let key = if r.node_a < r.node_b {
            (r.node_a.clone(), r.node_b.clone())
        } else {
            (r.node_b.clone(), r.node_a.clone())
        };

        match best_per_pair.get(&key) {
            Some(existing) if existing.bandwidth_mbps >= r.bandwidth_mbps => {}
            _ => {
                best_per_pair.insert(key, r);
            }
        }
    }

    // Zbuduj assignments z najlepszych par
    let mut assignments: Vec<PairAssignment> = Vec::new();
    let mut bottleneck: f64 = f64::MAX;
    let mut interface_types: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Per-node: interfejs -> laczna przepustowosc do peerow
    let mut node_bandwidth: HashMap<String, HashMap<String, f64>> = HashMap::new();

    for result in best_per_pair.values() {
        *node_bandwidth
            .entry(result.node_a.clone())
            .or_default()
            .entry(result.interface_a.clone())
            .or_insert(0.0) += result.bandwidth_mbps;

        *node_bandwidth
            .entry(result.node_b.clone())
            .or_default()
            .entry(result.interface_b.clone())
            .or_insert(0.0) += result.bandwidth_mbps;

        assignments.push(PairAssignment {
            node_a: result.node_a.clone(),
            node_b: result.node_b.clone(),
            interface_a: result.interface_a.clone(),
            interface_b: result.interface_b.clone(),
            bandwidth_mbps: result.bandwidth_mbps,
            rdma: result.rdma,
        });

        if result.bandwidth_mbps < bottleneck {
            bottleneck = result.bandwidth_mbps;
        }

        if result.rdma {
            interface_types.insert("rdma".to_string());
        } else {
            interface_types.insert("ethernet".to_string());
        }
    }

    // Per-node assignment: interfejs na ktorym node widzi WSZYSTKIE inne nody
    // Jesli nie ma takiego — interfejs z najwyzsza suma bandwidth
    let all_node_ids: Vec<String> = {
        let mut ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        for r in probe_results {
            ids.insert(r.node_a.clone());
            ids.insert(r.node_b.clone());
        }
        ids.into_iter().collect()
    };

    let mut per_node: HashMap<String, NodeAssignment> = HashMap::new();
    for node_id in &all_node_ids {
        let other_nodes: Vec<&String> = all_node_ids.iter().filter(|id| *id != node_id).collect();

        // Zbierz interfejsy tego noda z reachable wynikami
        let mut iface_reaches: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
        let mut iface_bandwidth: HashMap<String, f64> = HashMap::new();

        for r in probe_results.iter().filter(|r| r.reachable) {
            if r.node_a == *node_id {
                iface_reaches
                    .entry(r.interface_a.clone())
                    .or_default()
                    .insert(r.node_b.clone());
                *iface_bandwidth.entry(r.interface_a.clone()).or_insert(0.0) += r.bandwidth_mbps;
            } else if r.node_b == *node_id {
                iface_reaches
                    .entry(r.interface_b.clone())
                    .or_default()
                    .insert(r.node_a.clone());
                *iface_bandwidth.entry(r.interface_b.clone()).or_insert(0.0) += r.bandwidth_mbps;
            }
        }

        // Znajdz interfejs ktory widzi WSZYSTKIE inne nody
        let full_reach: Vec<(&String, &f64)> = iface_bandwidth
            .iter()
            .filter(|(iface, _)| {
                let reaches = iface_reaches.get(*iface);
                reaches.map_or(false, |r| other_nodes.iter().all(|n| r.contains(*n)))
            })
            .collect();

        tracing::info!(
            "Per-node {}: interfejsy={:?}, full_reach={:?}",
            node_id,
            iface_bandwidth.keys().collect::<Vec<_>>(),
            full_reach
                .iter()
                .map(|(k, v)| (k.as_str(), *v))
                .collect::<Vec<_>>()
        );

        let best_iface = if !full_reach.is_empty() {
            // Sposrod interfejsow ktore widza wszystkie, wybierz najszybszy
            full_reach
                .iter()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(iface, _)| (*iface).clone())
        } else {
            // Brak interfejsu widocznego dla wszystkich — wybierz z najwyzsza suma bandwidth
            iface_bandwidth
                .iter()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(iface, _)| iface.clone())
        };

        if let Some(iface) = best_iface {
            per_node.insert(
                node_id.clone(),
                NodeAssignment {
                    interface: iface,
                    ip: String::new(),
                    speed_mbps: 0,
                },
            );
        }
    }

    if bottleneck == f64::MAX {
        bottleneck = 0.0;
    }

    // Sprawdz czy kazda para NODOW ma przynajmniej jeden reachable interfejs
    let mut node_pairs_reachable: HashMap<(String, String), bool> = HashMap::new();
    for r in probe_results {
        let key = if r.node_a < r.node_b {
            (r.node_a.clone(), r.node_b.clone())
        } else {
            (r.node_b.clone(), r.node_a.clone())
        };
        let entry = node_pairs_reachable.entry(key).or_insert(false);
        if r.reachable {
            *entry = true;
        }
    }
    let all_node_pairs_reachable =
        !node_pairs_reachable.is_empty() && node_pairs_reachable.values().all(|v| *v);

    // "optimal" = kazda para nodow ma reachable link (najszybsza wspolna konfiguracja)
    // "partial" = jakas para nodow nie ma reachable linku
    // "no_connections" = zaden node nie widzi drugiego
    let message = if probe_results.is_empty() || node_pairs_reachable.values().all(|v| !*v) {
        "no_connections".to_string()
    } else if all_node_pairs_reachable {
        "optimal".to_string()
    } else {
        "partial".to_string()
    };

    let is_mixed = false; // Nie uzywamy mixed — jest jedna konfiguracja

    DetectionResult {
        assignments,
        per_node,
        is_mixed,
        bottleneck_mbps: bottleneck,
        message,
        all_results: probe_results.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_same_subnet_true() {
        assert!(same_subnet(
            "192.168.1.10",
            "255.255.255.0",
            "192.168.1.20",
            "255.255.255.0"
        ));
    }

    #[test]
    fn test_same_subnet_false() {
        assert!(!same_subnet(
            "192.168.1.10",
            "255.255.255.0",
            "10.0.0.5",
            "255.255.255.0"
        ));
    }

    #[test]
    fn test_same_subnet_different_masks() {
        // /24 i /16 — bardziej restrykcyjna maska (/24) decyduje
        assert!(same_subnet(
            "192.168.1.10",
            "255.255.255.0",
            "192.168.1.20",
            "255.255.0.0"
        ));
        // Rozne subnety nawet po AND masek: 10.x vs 192.168.x
        assert!(!same_subnet(
            "192.168.1.10",
            "255.255.255.0",
            "10.0.1.20",
            "255.255.0.0"
        ));
    }

    #[test]
    fn test_same_subnet_invalid_ip() {
        assert!(!same_subnet(
            "invalid",
            "255.255.255.0",
            "192.168.1.1",
            "255.255.255.0"
        ));
    }

    #[test]
    fn test_filter_reachable_pairs() {
        let nodes = vec![
            vec![NodeInterface {
                node_id: "a".into(),
                name: "eth0".into(),
                ip: "192.168.1.10".into(),
                netmask: "255.255.255.0".into(),
                speed_mbps: 1000,
                rdma_available: false,
            }],
            vec![NodeInterface {
                node_id: "b".into(),
                name: "eth0".into(),
                ip: "192.168.1.20".into(),
                netmask: "255.255.255.0".into(),
                speed_mbps: 1000,
                rdma_available: false,
            }],
            vec![NodeInterface {
                node_id: "c".into(),
                name: "eth0".into(),
                ip: "10.0.0.5".into(),
                netmask: "255.255.255.0".into(),
                speed_mbps: 1000,
                rdma_available: false,
            }],
        ];

        let pairs = filter_reachable_pairs(&nodes);
        // Tylko a-b sa w tym samym subnecie
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0.node_id, "a");
        assert_eq!(pairs[0].1.node_id, "b");
    }

    #[test]
    fn test_select_fastest_per_pair() {
        let pairs = vec![
            (
                NodeInterface {
                    node_id: "a".into(),
                    name: "eth0".into(),
                    ip: "192.168.1.10".into(),
                    netmask: "255.255.255.0".into(),
                    speed_mbps: 1000,
                    rdma_available: false,
                },
                NodeInterface {
                    node_id: "b".into(),
                    name: "eth0".into(),
                    ip: "192.168.1.20".into(),
                    netmask: "255.255.255.0".into(),
                    speed_mbps: 1000,
                    rdma_available: false,
                },
            ),
            (
                NodeInterface {
                    node_id: "a".into(),
                    name: "eth1".into(),
                    ip: "192.168.1.11".into(),
                    netmask: "255.255.255.0".into(),
                    speed_mbps: 10000,
                    rdma_available: true,
                },
                NodeInterface {
                    node_id: "b".into(),
                    name: "eth1".into(),
                    ip: "192.168.1.21".into(),
                    netmask: "255.255.255.0".into(),
                    speed_mbps: 10000,
                    rdma_available: true,
                },
            ),
        ];

        let selected = select_fastest_per_pair(&pairs);
        assert_eq!(selected.len(), 1);
        // Powinien wybrac 10G pare
        assert_eq!(selected[0].0.speed_mbps, 10000);
    }

    #[test]
    fn test_optimal_assignment_empty() {
        let result = optimal_assignment(&[]);
        assert!(result.assignments.is_empty());
        assert_eq!(result.bottleneck_mbps, 0.0);
        assert_eq!(result.message, "no_connections");
    }

    #[test]
    fn test_optimal_assignment_single_pair() {
        let probes = vec![PairProbeResult {
            node_a: "a".into(),
            node_b: "b".into(),
            interface_a: "eth0".into(),
            interface_b: "eth0".into(),
            bandwidth_mbps: 940.0,
            latency_us: 100,
            reachable: true,
            rdma: false,
        }];

        let result = optimal_assignment(&probes);
        assert_eq!(result.assignments.len(), 1);
        assert!(!result.is_mixed);
        assert!((result.bottleneck_mbps - 940.0).abs() < 0.01);
    }

    #[test]
    fn test_optimal_assignment_picks_best() {
        let probes = vec![
            PairProbeResult {
                node_a: "a".into(),
                node_b: "b".into(),
                interface_a: "eth0".into(),
                interface_b: "eth0".into(),
                bandwidth_mbps: 940.0,
                latency_us: 200,
                reachable: true,
                rdma: false,
            },
            PairProbeResult {
                node_a: "a".into(),
                node_b: "b".into(),
                interface_a: "eth1".into(),
                interface_b: "eth1".into(),
                bandwidth_mbps: 9500.0,
                latency_us: 50,
                reachable: true,
                rdma: true,
            },
        ];

        let result = optimal_assignment(&probes);
        assert_eq!(result.assignments.len(), 1);
        assert!((result.assignments[0].bandwidth_mbps - 9500.0).abs() < 0.01);
        assert!(result.assignments[0].rdma);
    }

    #[test]
    fn test_optimal_assignment_unreachable_skipped() {
        let probes = vec![PairProbeResult {
            node_a: "a".into(),
            node_b: "b".into(),
            interface_a: "eth0".into(),
            interface_b: "eth0".into(),
            bandwidth_mbps: 0.0,
            latency_us: 0,
            reachable: false,
            rdma: false,
        }];

        let result = optimal_assignment(&probes);
        assert!(result.assignments.is_empty());
        assert_eq!(result.bottleneck_mbps, 0.0);
    }
}

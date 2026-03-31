// =============================================================================
// Plik: mesh/discovery.rs
// Opis: Odkrywanie peerow w sieci lokalnej przez mDNS (Zeroconf). Rejestruje
//       wlasny serwis i nasluchuje na nowe wezly mesh TentaFlow.
// =============================================================================

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::error::CoreError;

// Typ serwisu mDNS dla mesh TentaFlow
const SERVICE_TYPE: &str = "_tentaflow-mesh._udp.local.";

// Wersja protokolu mesh
const PROTOCOL_VERSION: &str = "1";

/// Peer odkryty przez mDNS w sieci lokalnej.
#[derive(Debug, Clone)]
pub struct DiscoveredPeer {
    pub node_id: String,
    pub addresses: Vec<IpAddr>,
    pub port: u16,
    pub properties: HashMap<String, String>,
}

/// Zdarzenie dotyczace peera — pojawienie sie lub znikniecie z sieci.
#[derive(Debug, Clone)]
pub enum PeerEvent {
    /// Nowy peer zostal odkryty i rozwiazany (resolved)
    Discovered(DiscoveredPeer),
    /// Peer zniknal z sieci
    Removed { fullname: String },
}

/// Wrapper na mdns-sd ServiceDaemon — rejestracja wlasnego serwisu
/// i odkrywanie peerow w sieci lokalnej.
pub struct MdnsDiscovery {
    daemon: ServiceDaemon,
    node_id: String,
    registered_name: String,
    browse_active: Mutex<bool>,
}

impl MdnsDiscovery {
    /// Tworzy nowy daemon mDNS i rejestruje serwis na podanym porcie.
    ///
    /// `node_id` — unikalny identyfikator wezla w mesh
    /// `mesh_port` — port na ktorym wezel nasluchuje polaczen mesh
    pub fn new(node_id: &str, mesh_port: u16) -> Result<Self, CoreError> {
        let daemon = ServiceDaemon::new().map_err(|e| CoreError::PeerError {
            peer_id: node_id.to_string(),
            message: format!("Nie udalo sie uruchomic mDNS daemon: {e}"),
            source: None,
        })?;

        let local_hostname = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        // Nazwa instancji serwisu — unikalna w sieci
        let instance_name = format!("tentaflow-{node_id}");

        // TXT records z metadanymi wezla
        let properties: HashMap<String, String> = [
            ("node_id".to_string(), node_id.to_string()),
            ("version".to_string(), PROTOCOL_VERSION.to_string()),
            ("role".to_string(), "router".to_string()),
            ("hostname".to_string(), local_hostname.clone()),
        ]
        .into();

        let host_fqdn = format!("{local_hostname}.local.");

        let service_info = ServiceInfo::new(
            SERVICE_TYPE,
            &instance_name,
            &host_fqdn,
            "",
            mesh_port,
            properties,
        )
        .map_err(|e| CoreError::PeerError {
            peer_id: node_id.to_string(),
            message: format!("Nie udalo sie utworzyc ServiceInfo: {e}"),
            source: None,
        })?
        .enable_addr_auto();

        let registered_name = service_info.get_fullname().to_string();

        daemon.register(service_info).map_err(|e| CoreError::PeerError {
            peer_id: node_id.to_string(),
            message: format!("Nie udalo sie zarejestrowac serwisu mDNS: {e}"),
            source: None,
        })?;

        info!(
            node_id = node_id,
            port = mesh_port,
            fullname = %registered_name,
            "Serwis mDNS zarejestrowany"
        );

        Ok(Self {
            daemon,
            node_id: node_id.to_string(),
            registered_name,
            browse_active: Mutex::new(false),
        })
    }

    /// Uruchamia browse mDNS i wysyla odkryte peery na kanal `tx`.
    ///
    /// Dziala w tle w osobnym tasku tokio. Filtruje wlasny wezel,
    /// zeby nie raportowac samego siebie.
    pub fn start_discovery(
        &self,
        tx: mpsc::UnboundedSender<PeerEvent>,
    ) -> Result<(), CoreError> {
        let receiver = self.daemon.browse(SERVICE_TYPE).map_err(|e| CoreError::PeerError {
            peer_id: self.node_id.clone(),
            message: format!("Nie udalo sie uruchomic mDNS browse: {e}"),
            source: None,
        })?;

        {
            let mut active = self.browse_active.lock();
            *active = true;
        }

        info!(service_type = SERVICE_TYPE, "Rozpoczeto mDNS browse");

        let own_node_id = self.node_id.clone();
        let own_fullname = self.registered_name.clone();

        tokio::spawn(async move {
            browse_loop(receiver, tx, &own_node_id, &own_fullname).await;
        });

        Ok(())
    }

    /// Zatrzymuje rejestracje serwisu i browse mDNS.
    pub fn stop(&self) -> Result<(), CoreError> {
        {
            let mut active = self.browse_active.lock();
            if !*active {
                // Browse nie byl uruchomiony lub juz zatrzymany
            } else {
                let _ = self.daemon.stop_browse(SERVICE_TYPE);
                *active = false;
            }
        }

        self.daemon
            .unregister(&self.registered_name)
            .map_err(|e| CoreError::PeerError {
                peer_id: self.node_id.clone(),
                message: format!("Nie udalo sie wyrejestrowac serwisu mDNS: {e}"),
                source: None,
            })?;

        info!(
            node_id = %self.node_id,
            fullname = %self.registered_name,
            "Serwis mDNS wyrejestrowany"
        );

        Ok(())
    }

    /// Zwraca pelna nazwe zarejestrowanego serwisu mDNS.
    pub fn registered_name(&self) -> &str {
        &self.registered_name
    }

    /// Zwraca identyfikator wezla.
    pub fn node_id(&self) -> &str {
        &self.node_id
    }
}

/// Glowna petla przetwarzajaca zdarzenia mDNS browse.
///
/// Filtruje wlasny serwis i konwertuje ServiceInfo na DiscoveredPeer.
/// Konczy sie gdy kanal `tx` zostanie zamkniety lub browse zatrzymany.
async fn browse_loop(
    receiver: mdns_sd::Receiver<ServiceEvent>,
    tx: mpsc::UnboundedSender<PeerEvent>,
    own_node_id: &str,
    own_fullname: &str,
) {
    // Zbiór juz odkrytych peerow — emitujemy Discovered tylko RAZ per node_id
    let mut discovered_peers: HashSet<String> = HashSet::new();

    loop {
        let event = match receiver.recv_async().await {
            Ok(event) => event,
            Err(_) => {
                debug!("Kanal mDNS browse zamkniety — koniec petli");
                break;
            }
        };

        match event {
            ServiceEvent::ServiceResolved(info) => {
                let fullname = info.get_fullname().to_string();

                // Pomijamy wlasny serwis
                if fullname == own_fullname {
                    continue;
                }

                let peer = service_info_to_peer(&info);

                // Pomijamy peery bez node_id (brak TXT records)
                if peer.node_id == "unknown" {
                    debug!(fullname = %fullname, "Pominieto peer bez node_id");
                    continue;
                }

                // Pomijamy wlasny wezel
                if peer.node_id == own_node_id {
                    continue;
                }

                // Nie blokuj re-discovery dla peerow bez adresow
                if peer.addresses.is_empty() {
                    let _ = tx.send(PeerEvent::Discovered(peer));
                    continue;
                }
                if !discovered_peers.insert(peer.node_id.clone()) {
                    debug!(node_id = %peer.node_id, "mDNS: peer juz odkryty, pomijam duplikat");
                    continue;
                }

                info!(
                    node_id = %peer.node_id,
                    addresses = ?peer.addresses,
                    port = peer.port,
                    "Odkryto nowy peer w sieci"
                );

                if tx.send(PeerEvent::Discovered(peer)).is_err() {
                    debug!("Odbiorca zamkniety — koniec browse");
                    break;
                }
            }

            ServiceEvent::ServiceRemoved(_service_type, fullname) => {
                if fullname == own_fullname {
                    continue;
                }

                // Wyczysc z discovered_peers zeby peer mogl byc ponownie odkryty
                // Fullname ma format "tentaflow-{node_id}._tentaflow-mesh._udp.local."
                if let Some(node_id) = fullname.strip_prefix("tentaflow-")
                    .and_then(|s| s.split('.').next())
                {
                    discovered_peers.remove(node_id);
                }

                debug!(
                    fullname = %fullname,
                    "Peer zniknal z sieci"
                );

                if tx.send(PeerEvent::Removed { fullname }).is_err() {
                    debug!("Odbiorca zamkniety — koniec browse");
                    break;
                }
            }

            ServiceEvent::SearchStarted(service_type) => {
                debug!(service_type = %service_type, "mDNS search rozpoczety");
            }

            ServiceEvent::SearchStopped(service_type) => {
                debug!(service_type = %service_type, "mDNS search zatrzymany");
                break;
            }

            _ => {}
        }
    }
}

/// Konwertuje ServiceInfo z mDNS na DiscoveredPeer.
fn service_info_to_peer(info: &ServiceInfo) -> DiscoveredPeer {
    // IPv4, bez loopback/Docker-bridge/link-local — unikamy falszywych adresow
    let addresses: Vec<IpAddr> = info.get_addresses().iter()
        .filter(|a| {
            if let IpAddr::V4(v4) = a {
                !v4.is_loopback()
                    && !v4.is_link_local()
                    && !(v4.octets()[0] == 172 && v4.octets()[1] >= 16 && v4.octets()[1] <= 31)
            } else {
                false
            }
        })
        .copied()
        .collect();
    let port = info.get_port();

    // Odczytaj TXT records
    let mut properties = HashMap::new();
    for property in info.get_properties().iter() {
        properties.insert(
            property.key().to_string(),
            property.val_str().to_string(),
        );
    }

    let node_id = properties
        .get("node_id")
        .cloned()
        .unwrap_or_else(|| "unknown".to_string());

    DiscoveredPeer {
        node_id,
        addresses,
        port,
        properties,
    }
}

impl Drop for MdnsDiscovery {
    fn drop(&mut self) {
        if let Err(e) = self.stop() {
            warn!("Blad podczas zamykania mDNS discovery: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discovered_peer_creation() {
        let peer = DiscoveredPeer {
            node_id: "test-node-1".to_string(),
            addresses: vec!["192.168.1.10".parse().unwrap()],
            port: 9100,
            properties: HashMap::from([
                ("version".to_string(), "1".to_string()),
                ("role".to_string(), "router".to_string()),
            ]),
        };

        assert_eq!(peer.node_id, "test-node-1");
        assert_eq!(peer.port, 9100);
        assert_eq!(peer.addresses.len(), 1);
        assert_eq!(peer.properties.get("role").unwrap(), "router");
    }

    #[test]
    fn test_peer_event_variants() {
        let peer = DiscoveredPeer {
            node_id: "node-a".to_string(),
            addresses: vec![],
            port: 9200,
            properties: HashMap::new(),
        };

        let event = PeerEvent::Discovered(peer);
        assert!(matches!(event, PeerEvent::Discovered(_)));

        let event = PeerEvent::Removed {
            fullname: "tentaflow-node-a._tentaflow-mesh._udp.local.".to_string(),
        };
        assert!(matches!(event, PeerEvent::Removed { .. }));
    }

    #[test]
    fn test_service_type_format() {
        assert!(SERVICE_TYPE.starts_with('_'));
        assert!(SERVICE_TYPE.ends_with('.'));
        assert!(SERVICE_TYPE.contains("._udp."));
    }
}

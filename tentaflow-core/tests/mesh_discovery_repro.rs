// =============================================================================
// Plik: mesh_discovery_repro.rs
// Opis: Reproducer bug mesh discovery. Demonstruje ze dwa iroh::Endpoints
//       startujace z MdnsAddressLookup::builder() domyslnie publikuja sie
//       do LAN, ale tentaflow-core nie subskrybuje DiscoveryEvent-ow wiec
//       zadna warstwa (peer_manager, gossip, pairing) nie dostaje info
//       o nowym peerze. Test sprawdza dwie rzeczy:
//
//       1. RAW IROH: dwa endpointy na tej samej maszynie z mDNS powinny
//          widziec siebie na Stream<DiscoveryEvent>.
//       2. INTEGRACJA: IrohMeshManager::new() nie udostepnia DiscoveryEvent
//          i nie spawnuje taska ktory dialowalby odkryte peery.
//
// Uruchomienie: cargo test --test mesh_discovery_repro --features dashboard-api -- --nocapture
// =============================================================================

use std::net::SocketAddr;
use std::time::Duration;

use futures::StreamExt;
use iroh::{
    address_lookup::{DiscoveryEvent, MdnsAddressLookup},
    endpoint::{presets, Endpoint},
    SecretKey,
};
use tentaflow_core::net::iroh::{IrohConfig, IrohEndpoint};

/// Sprawdza czy iroh sam z siebie emituje DiscoveryEvent::Discovered miedzy
/// dwoma endpointami bind'ed localnie z MdnsAddressLookup. To jest sanity test
/// warstwy transportu — jesli tu zawodzi, problem jest w srodowisku (mDNS
/// zablokowany, brak multicast na loopbacku itp.).
#[tokio::test]
async fn raw_iroh_mdns_should_discover_peer_on_lan() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("iroh=info,mesh_discovery_repro=info")
        .try_init();

    let sk_a = SecretKey::generate();
    let sk_b = SecretKey::generate();

    let ep_a = Endpoint::builder(presets::N0::default())
        .secret_key(sk_a)
        .bind()
        .await
        .expect("bind A");
    let ep_b = Endpoint::builder(presets::N0::default())
        .secret_key(sk_b)
        .bind()
        .await
        .expect("bind B");

    let mdns_a = MdnsAddressLookup::builder()
        .build(ep_a.id())
        .expect("mdns A");
    let mdns_b = MdnsAddressLookup::builder()
        .build(ep_b.id())
        .expect("mdns B");

    ep_a.address_lookup().unwrap().add(mdns_a.clone());
    ep_b.address_lookup().unwrap().add(mdns_b.clone());

    let target_b = ep_b.id();

    let mut events_a = mdns_a.subscribe().await;

    let discovered = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(ev) = events_a.next().await {
            if let DiscoveryEvent::Discovered { endpoint_info, .. } = ev {
                if endpoint_info.endpoint_id == target_b {
                    return true;
                }
            }
        }
        false
    })
    .await;

    match discovered {
        Ok(true) => {
            eprintln!("RAW IROH: A wykryl B przez mDNS — transport dziala");
        }
        Ok(false) => panic!("stream zamkniety bez wykrycia B"),
        Err(_) => panic!(
            "TIMEOUT 10s — RAW IROH nie wykrywa B przez mDNS. \
             Mozliwe przyczyny: mDNS zablokowany przez firewall, brak multicast \
             na interfejsie, docker bridge bez mDNS, WSL2 bez multicast."
        ),
    }
}

/// Sprawdza fix: dwa `IrohEndpoint` (wrapper tentaflow) powinny przez nowe
/// API `mdns_discovery_events()` zobaczyc siebie nawzajem. Test pokrywa
/// wlasnie to, co wczesniej bylo luka — wrapper tentaflow zachowuje
/// `MdnsAddressLookup` jako pole i eksponuje stream subskrybcji.
#[tokio::test]
async fn iroh_endpoint_exposes_discovery_events_for_mesh() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("iroh=warn,tentaflow_core=info")
        .try_init();

    let cfg_a = IrohConfig {
        secret_key: SecretKey::generate(),
        bind_addr: SocketAddr::from(([0, 0, 0, 0], 0)),
        relay_url: None,
        enable_lan_discovery: true,
        enable_dht_discovery: false,
    };
    let cfg_b = IrohConfig {
        secret_key: SecretKey::generate(),
        bind_addr: SocketAddr::from(([0, 0, 0, 0], 0)),
        relay_url: None,
        enable_lan_discovery: true,
        enable_dht_discovery: false,
    };

    let ep_a = IrohEndpoint::bind(cfg_a).await.expect("bind A");
    let ep_b = IrohEndpoint::bind(cfg_b).await.expect("bind B");
    let target_b = ep_b.id();

    assert!(ep_a.has_mdns(), "ep_a powinien miec mDNS lookup");
    assert!(ep_b.has_mdns(), "ep_b powinien miec mDNS lookup");

    let mut stream_a = ep_a
        .mdns_discovery_events()
        .await
        .expect("ep_a udostepnia discovery stream");

    let found = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(ev) = stream_a.next().await {
            if let DiscoveryEvent::Discovered { endpoint_info, .. } = ev {
                if endpoint_info.endpoint_id == target_b {
                    return true;
                }
            }
        }
        false
    })
    .await;

    match found {
        Ok(true) => eprintln!("FIX OK: IrohEndpoint::mdns_discovery_events() propaguje discovery"),
        Ok(false) => panic!("stream zamkniety bez wykrycia B"),
        Err(_) => panic!("TIMEOUT — IrohEndpoint nie udostepnia DiscoveryEvent-ow"),
    }
}

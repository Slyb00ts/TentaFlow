// =============================================================================
// Plik: net/iroh/endpoint.rs
// Opis: Tworzenie i konfiguracja `iroh::Endpoint`. `IrohEndpoint::bind` zwraca
//       obiekt nasluchujacy na skonfigurowanych ALPN-ach, z LAN mDNS, DHT
//       pkarr oraz relayami z wbudowanego presetu N0 (4 regiony
//       `*.relay.n0.iroh-canary.iroh.link`). Custom relay mozna wstrzyknac
//       przez `IrohConfig::relay_url` (z configu albo z DB settings).
//       `IrohEndpoint::connect` otwiera polaczenie do peera po `EndpointId`
//       albo `EndpointAddr`.
// =============================================================================

use std::net::SocketAddr;

use futures::Stream;
use iroh::{
    address_lookup::{
        DhtAddressLookup, DiscoveryEvent, DnsAddressLookup, MdnsAddressLookup, PkarrPublisher,
    },
    endpoint::{presets, QuicTransportConfig},
    protocol::Router,
    Endpoint, EndpointAddr, EndpointId, RelayMap, RelayMode, RelayUrl, SecretKey,
};
use std::time::Duration;

use super::{ALPN_API, ALPN_MESH, ALPN_PAIRING};

/// Konfiguracja uruchomienia iroh endpointa dla daemona.
#[derive(Clone)]
pub struct IrohConfig {
    /// Ed25519 secret key serwisu — rowniez tozsamosc w mesh.
    pub secret_key: SecretKey,
    /// Adres bind dla QUIC UDP. `0.0.0.0:0` → dowolny wolny port.
    pub bind_addr: SocketAddr,
    /// URL custom relay; `None` oznacza zostawienie presetu N0 iroh
    /// (4 regiony `*.relay.n0.iroh-canary.iroh.link`).
    pub relay_url: Option<RelayUrl>,
    /// Wlacz LAN discovery przez swarm-discovery mDNS.
    pub enable_lan_discovery: bool,
    /// Wlacz DHT (pkarr-mainline) dla internetu.
    pub enable_dht_discovery: bool,
}

impl IrohConfig {
    /// Minimalna konfiguracja z wygenerowanym SecretKey.
    pub fn new_ephemeral() -> Self {
        Self {
            secret_key: SecretKey::generate(),
            bind_addr: SocketAddr::from(([0, 0, 0, 0], 0)),
            relay_url: None,
            enable_lan_discovery: true,
            enable_dht_discovery: true,
        }
    }
}

/// Opakowanie na `iroh::Endpoint` + ewentualny `Router` obslugujacy ALPN-y.
/// Trzyma tez uchwyt na `MdnsAddressLookup` zeby udostepnic strumien
/// `DiscoveryEvent` warstwie mesh — bez tego autodiscovery po LAN nie
/// propaguje sie do gossip/peer_manager.
pub struct IrohEndpoint {
    endpoint: Endpoint,
    router: Option<Router>,
    mdns: Option<MdnsAddressLookup>,
}

#[derive(Debug, thiserror::Error)]
pub enum IrohEndpointError {
    #[error("invalid bind address: {0}")]
    InvalidBind(String),
    #[error("iroh bind failed: {0}")]
    Bind(String),
}

impl IrohEndpoint {
    /// Tworzy i bind'uje endpoint z podana konfiguracja.
    pub async fn bind(config: IrohConfig) -> Result<Self, IrohEndpointError> {
        // QUIC keep-alive + idle timeout. Bez tego iroh zostawia polaczenie
        // kompletnie na lasce ruchu aplikacyjnego — gdy peer na chwile zamilczy
        // (NAT rebind, wifi roam, krotki packet-loss), Quinn nie wysyla PINGow,
        // idle timer po swojemu zabija sesje a my dopiero po 30s liveness
        // wykrywamy zgon. Keep-alive co 10s + idle 25s daje stabilna sesje i
        // szybkie wykrycie realnej awarii bez spamu rekonektow.
        let transport_config = QuicTransportConfig::builder()
            .keep_alive_interval(Duration::from_secs(10))
            .max_idle_timeout(Some(
                Duration::from_secs(25)
                    .try_into()
                    .expect("25s fits in VarInt"),
            ))
            .build();

        // `presets::Minimal` daje tylko CryptoProvider (TLS) bez dodawania
        // publishera pkarr ani relayow N0. Wczesniej uzywalismy `presets::N0`
        // ktory zawsze pchal `PkarrPublisher::n0_dns()` -> publish do
        // `https://dns.iroh.link/pkarr` i logowal `UnknownIssuer` w sieciach
        // gdzie cert chain n0 nie jest zaufany. Skoro mamy wlasny relay
        // (`relay.nextapp.pl`) i wlasne discovery (LAN mDNS + DHT mainline +
        // trusted_contact hints), pkarr-DNS n0 nie jest nam potrzebny.
        let mut builder = Endpoint::builder(presets::Minimal)
            .secret_key(config.secret_key.clone())
            .alpns(vec![
                ALPN_MESH.to_vec(),
                ALPN_PAIRING.to_vec(),
                ALPN_API.to_vec(),
            ])
            .transport_config(transport_config)
            .bind_addr(config.bind_addr)
            .map_err(|e| IrohEndpointError::InvalidBind(format!("{e:?}")))?;

        // Relay: zawsze ustawiamy custom map. `load_relay_url` w wywolawcy
        // gwarantuje fallback do `relay.nextapp.pl`; jezeli mimo to dostaniemy
        // None, wylaczamy relay calkowicie zamiast pozwolic iroh wpasc na
        // staging/canary preset N0.
        builder = match config.relay_url.clone() {
            Some(url) => builder.relay_mode(RelayMode::Custom(RelayMap::from(url))),
            None => builder.relay_mode(RelayMode::Disabled),
        };

        // Pkarr/DNS n0 swiadomie nie podlaczone — patrz komentarz przy
        // `presets::Minimal` wyzej. `_` na importy zostawia je w prelude
        // gdyby pozniejsza konfiguracja chciala je opcjonalnie dolaczyc.
        let _ = PkarrPublisher::n0_dns;
        let _ = DnsAddressLookup::n0_dns;

        let endpoint = builder
            .bind()
            .await
            .map_err(|e| IrohEndpointError::Bind(format!("{e:?}")))?;

        // mDNS musi byc zbudowane z endpoint.id() i zachowane jako instancja,
        // zeby mozna bylo subskrybowac DiscoveryEvent-y. Builder przekazany
        // do Endpoint pre-bind nie daje uchwytu do subscribe — bug 137#.
        let mdns = if config.enable_lan_discovery {
            let m = MdnsAddressLookup::builder()
                .build(endpoint.id())
                .map_err(|e| IrohEndpointError::Bind(format!("mdns build: {e:?}")))?;
            endpoint
                .address_lookup()
                .map_err(|e| IrohEndpointError::Bind(format!("address_lookup: {e:?}")))?
                .add(m.clone());
            Some(m)
        } else {
            None
        };

        if config.enable_dht_discovery {
            let dht = DhtAddressLookup::builder()
                .build()
                .map_err(|e| IrohEndpointError::Bind(format!("dht build: {e:?}")))?;
            endpoint
                .address_lookup()
                .map_err(|e| IrohEndpointError::Bind(format!("address_lookup: {e:?}")))?
                .add(dht);
        }

        Ok(Self {
            endpoint,
            router: None,
            mdns,
        })
    }

    /// Subskrypcja strumienia DiscoveryEvent z LAN mDNS. `None` jesli
    /// `enable_lan_discovery = false`. Mesh manager uzywa tego do auto-dial
    /// kazdego nowego peera na LAN.
    pub async fn mdns_discovery_events(
        &self,
    ) -> Option<impl Stream<Item = DiscoveryEvent> + Unpin + Send> {
        let mdns = self.mdns.as_ref()?;
        Some(mdns.subscribe().await)
    }

    /// Zwraca `EndpointId` (Ed25519 public key) tego endpointa.
    pub fn id(&self) -> EndpointId {
        self.endpoint.id()
    }

    /// Podpina router z handlerami dla wszystkich trzech ALPN-ow.
    pub fn with_handlers<M, P, A>(mut self, mesh: M, pairing: P, api: A) -> Self
    where
        M: iroh::protocol::ProtocolHandler,
        P: iroh::protocol::ProtocolHandler,
        A: iroh::protocol::ProtocolHandler,
    {
        let router = Router::builder(self.endpoint.clone())
            .accept(ALPN_MESH, mesh)
            .accept(ALPN_PAIRING, pairing)
            .accept(ALPN_API, api)
            .spawn();
        self.router = Some(router);
        self
    }

    /// Zwraca `true` jesli endpoint ma aktywny mDNS lookup.
    pub fn has_mdns(&self) -> bool {
        self.mdns.is_some()
    }

    /// Otwiera wychodzace polaczenie do peera (rozwiazanie adresu przez
    /// discovery jezeli podano tylko `EndpointId`).
    pub async fn connect(
        &self,
        addr: impl Into<EndpointAddr>,
        alpn: &[u8],
    ) -> Result<iroh::endpoint::Connection, iroh::endpoint::ConnectError> {
        self.endpoint.connect(addr.into(), alpn).await
    }

    /// Zwraca referencje do bazowego `iroh::Endpoint`.
    pub fn inner(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Szybkie zamkniecie endpointa + routera (jeśli pod pięty).
    pub async fn shutdown(self) {
        if let Some(router) = self.router {
            let _ = router.shutdown().await;
        }
        self.endpoint.close().await;
    }
}

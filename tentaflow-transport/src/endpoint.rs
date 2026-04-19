// =============================================================================
// Plik: tentaflow-transport/src/endpoint.rs
// Opis: Builder `iroh::Endpoint` dla klienta i serwera TentaFlow. Klient uzywa
//       `build_client_endpoint` (bez SecretKey, zbinduje efemeryczny), serwer
//       `build_server_endpoint` z wlasna tozsamoscia Ed25519. `parse_iroh_url`
//       wyciaga `EndpointId` z URL `iroh://<hex>` albo z czystego hex.
// =============================================================================

use std::net::SocketAddr;
use std::time::Duration;

use iroh::{
    address_lookup::{DhtAddressLookup, MdnsAddressLookup},
    endpoint::presets,
    Endpoint, EndpointId, RelayUrl, SecretKey,
};

use crate::error::TransportError;

/// Konfiguracja bindu endpointa po stronie serwera (sidecar, node).
#[derive(Clone)]
pub struct ServerEndpointConfig {
    /// Ed25519 identity — wystawiana jako `EndpointId`.
    pub secret_key: SecretKey,
    /// Adres UDP do bindowania. `0.0.0.0:0` → dowolny wolny port.
    pub bind_addr: SocketAddr,
    /// ALPN-y, ktore serwer przyjmie.
    pub alpns: Vec<Vec<u8>>,
    /// Opcjonalny relay URL — jeśli `None`, uzywany jest default `presets::N0`.
    pub relay_url: Option<RelayUrl>,
    /// Wlacza mDNS na LAN.
    pub enable_lan_discovery: bool,
    /// Wlacza DHT pkarr-mainline.
    pub enable_dht_discovery: bool,
}

impl ServerEndpointConfig {
    /// Minimalna konfiguracja z ephemeral key i bindem na dowolny port.
    pub fn ephemeral(alpns: Vec<Vec<u8>>) -> Self {
        Self {
            secret_key: SecretKey::generate(),
            bind_addr: SocketAddr::from(([0, 0, 0, 0], 0)),
            alpns,
            relay_url: None,
            enable_lan_discovery: true,
            enable_dht_discovery: true,
        }
    }
}

/// Tworzy client endpoint — efemeryczna tozsamosc, bind `0.0.0.0:0`, wszystkie
/// wymienione ALPN-y. Klient laczy sie do serwera po `EndpointId`; relay i
/// discovery pochodza z presetu `presets::N0`.
pub async fn build_client_endpoint(alpns: Vec<Vec<u8>>) -> Result<Endpoint, TransportError> {
    let builder = Endpoint::builder(presets::N0::default())
        .alpns(alpns)
        .address_lookup(MdnsAddressLookup::builder())
        .address_lookup(DhtAddressLookup::builder());

    builder
        .bind()
        .await
        .map_err(|e| TransportError::bind(format!("{e:?}")))
}

/// Tworzy server endpoint z podaną tozsamoscia i listą ALPN-ow.
pub async fn build_server_endpoint(
    config: ServerEndpointConfig,
) -> Result<Endpoint, TransportError> {
    let mut builder = Endpoint::builder(presets::N0::default())
        .secret_key(config.secret_key.clone())
        .alpns(config.alpns.clone());

    builder = builder
        .bind_addr(config.bind_addr)
        .map_err(|e| TransportError::invalid_config(format!("bind_addr: {e:?}")))?;

    if config.enable_lan_discovery {
        builder = builder.address_lookup(MdnsAddressLookup::builder());
    }
    if config.enable_dht_discovery {
        builder = builder.address_lookup(DhtAddressLookup::builder());
    }

    if config.relay_url.is_some() {
        // iroh 0.98: override relay wymaga osobnego API (relay_mode). Preset N0
        // ustawia `use.iroh.network`. Do uzupelnienia po wyjsciu iroh 1.0.
    }

    builder
        .bind()
        .await
        .map_err(|e| TransportError::bind(format!("{e:?}")))
}

/// Parsuje URL w formacie `iroh://<hex>` albo czysty hex (z lub bez prefixu
/// `0x`) i zwraca `EndpointId`. Biale znaki i `/` na koncu sa ignorowane.
pub fn parse_iroh_url(url: &str) -> Result<EndpointId, TransportError> {
    let raw = url.trim();
    let hex_str = raw
        .strip_prefix("iroh://")
        .unwrap_or(raw)
        .trim_end_matches('/')
        .trim_start_matches("0x");

    let bytes = hex::decode(hex_str).map_err(|e| {
        TransportError::invalid_config(format!("hex EndpointId: {e}"))
    })?;
    if bytes.len() != 32 {
        return Err(TransportError::invalid_config(format!(
            "EndpointId musi miec 32 bajty, ma {}",
            bytes.len()
        )));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    EndpointId::from_bytes(&arr).map_err(|e| {
        TransportError::invalid_config(format!("niepoprawny EndpointId: {e}"))
    })
}

/// Krotki timeout uzywany w kilku miejscach na default.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

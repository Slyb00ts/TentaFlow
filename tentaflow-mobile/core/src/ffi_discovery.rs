// =============================================================================
// Plik: ffi_discovery.rs
// Opis: FFI dla LAN discovery — iOS nie moze robic raw multicast mDNS bez
//       Apple entitlementa, wiec Swift NWBrowser znajduje peerow przez
//       systemowy Bonjour i karmi Rust przez te funkcje. Rust buduje
//       EndpointAddr z znanego EndpointId + direct IP i wola iroh.
// =============================================================================

use std::ffi::CStr;
use std::net::{IpAddr, SocketAddr};
use std::os::raw::c_char;
use std::sync::Arc;
use std::sync::OnceLock;

use tentaflow_core::mesh::iroh_manager::IrohMeshManager;
use tracing::{debug, warn};

/// Globalny uchwyt do IrohMeshManager — ustawiany raz po starcie mesh pipeline,
/// odczytywany przez FFI z watkow Swift (NWBrowser callback).
static MESH_HANDLE: OnceLock<Arc<IrohMeshManager>> = OnceLock::new();

/// Ustawia globalny uchwyt. Wolane raz z runtime.rs po udanym starcie mesh.
pub fn set_mesh_handle(handle: Arc<IrohMeshManager>) {
    let _ = MESH_HANDLE.set(handle);
}

/// Parsuje EndpointId z z-base32 (52 znaki, format iroh mDNS) albo hex (64 znaki).
/// iroh advertisuje EndpointId jako z-base32 NOPAD lowercase w nazwie instancji
/// Bonjour, wiec Swift bedzie nam to zwykle przekazywal w tym formacie.
fn parse_endpoint_id_str(s: &str) -> Option<String> {
    // Hex = 64 znaki heksadecymalne.
    if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(s.to_ascii_lowercase());
    }

    // z-base32 (data_encoding BASE32_NOPAD) lowercase, 52 znaki na 32 bajty.
    if s.len() == 52 {
        let upper = s.to_ascii_uppercase();
        if let Ok(bytes) = data_encoding::BASE32_NOPAD.decode(upper.as_bytes()) {
            if bytes.len() == 32 {
                return Some(hex::encode(&bytes));
            }
        }
    }

    None
}

/// Dodaje peera znalezionego przez Swift Bonjour do iroh mesh. Zwraca true
/// jesli udalo sie zaczac laczenie (rzeczywiste polaczenie dochodzi async).
///
/// # Safety
/// Wszystkie pointery musza byc waznymi C-stringami (NUL-terminated UTF-8).
/// Wolane z Swift — Apple gwarantuje poprawnosc pointerow dla swych stringow.
#[no_mangle]
pub unsafe extern "C" fn tentaflow_mobile_add_discovered_peer(
    endpoint_id_ptr: *const c_char,
    ip_ptr: *const c_char,
    port: u16,
) -> bool {
    if endpoint_id_ptr.is_null() || ip_ptr.is_null() || port == 0 {
        return false;
    }

    let endpoint_id_raw = match CStr::from_ptr(endpoint_id_ptr).to_str() {
        Ok(s) => s,
        Err(_) => return false,
    };
    let ip_str = match CStr::from_ptr(ip_ptr).to_str() {
        Ok(s) => s,
        Err(_) => return false,
    };

    let endpoint_id_hex = match parse_endpoint_id_str(endpoint_id_raw) {
        Some(h) => h,
        None => {
            warn!("discovered peer: niepoprawny EndpointId: {}", endpoint_id_raw);
            return false;
        }
    };

    let ip: IpAddr = match ip_str.parse() {
        Ok(ip) => ip,
        Err(_) => {
            warn!("discovered peer: niepoprawny IP: {}", ip_str);
            return false;
        }
    };

    let socket_addr = SocketAddr::new(ip, port);

    let manager = match MESH_HANDLE.get() {
        Some(m) => m.clone(),
        None => {
            debug!("discovered peer: mesh jeszcze nie gotowy");
            return false;
        }
    };

    debug!(
        endpoint_id = %&endpoint_id_hex[..16.min(endpoint_id_hex.len())],
        addr = %socket_addr,
        "Dodaje peera znalezionego przez Bonjour"
    );

    // Uruchamiamy laczenie w tle — nie blokujemy watku Swift.
    // tokio::spawn wymaga aktywnego runtime; uzywamy Handle::try_current
    // bo funkcja moze byc wolana z dowolnego watku.
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                if let Err(e) = manager.connect_to_peer_direct(&endpoint_id_hex, socket_addr).await {
                    debug!(endpoint_id = %&endpoint_id_hex[..16.min(endpoint_id_hex.len())],
                           "connect_to_peer_direct: {}", e);
                }
            });
            true
        }
        Err(_) => {
            // Brak biezacego runtime — prawdopodobnie wolane z czystego watku Swift.
            // Szukamy dedykowanego runtime'u w ktorym mesh dziala.
            match MESH_RUNTIME.get() {
                Some(rt) => {
                    rt.spawn(async move {
                        if let Err(e) = manager.connect_to_peer_direct(&endpoint_id_hex, socket_addr).await {
                            debug!(endpoint_id = %&endpoint_id_hex[..16.min(endpoint_id_hex.len())],
                                   "connect_to_peer_direct: {}", e);
                        }
                    });
                    true
                }
                None => {
                    warn!("discovered peer: brak aktywnego tokio runtime");
                    false
                }
            }
        }
    }
}

/// Globalny uchwyt do runtime'u tokio w ktorym dziala mesh. Ustawiany raz
/// razem z MESH_HANDLE — FFI uzywa go do spawnu async connect_to_peer_direct.
static MESH_RUNTIME: OnceLock<tokio::runtime::Handle> = OnceLock::new();

pub fn set_mesh_runtime(handle: tokio::runtime::Handle) {
    let _ = MESH_RUNTIME.set(handle);
}

// =============================================================================
// Plik: ffi_discovery.rs
// Opis: FFI dla LAN discovery — iOS nie moze robic raw multicast mDNS bez
//       Apple entitlementa, wiec Swift NWBrowser znajduje peerow przez
//       systemowy Bonjour i karmi Rust przez te funkcje. Rust buduje
//       EndpointAddr z znanego EndpointId + direct IP i wola iroh.
// =============================================================================

use std::collections::HashSet;
use std::ffi::CStr;
use std::net::{IpAddr, SocketAddr};
use std::os::raw::c_char;
use std::sync::{Arc, Mutex, OnceLock};

use tentaflow_core::mesh::iroh_manager::IrohMeshManager;
use tracing::{debug, info, warn};

/// Globalny uchwyt do IrohMeshManager — ustawiany raz po starcie mesh pipeline,
/// odczytywany przez FFI z watkow Swift (NWBrowser callback).
static MESH_HANDLE: OnceLock<Arc<IrohMeshManager>> = OnceLock::new();

/// Peer id (hex) dla ktorych w tej chwili leci async connect_to_peer_direct.
/// NWBrowser callback moze zawolac FFI wielokrotnie dla tego samego peera
/// zanim pierwszy handshake sie zakonczy — bez tego zestawu dostajemy
/// connection storm (8 QUIC connectow w 30ms).
static INFLIGHT_CONNECTS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn inflight() -> &'static Mutex<HashSet<String>> {
    INFLIGHT_CONNECTS.get_or_init(|| Mutex::new(HashSet::new()))
}

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

    // Dedup — dla tego samego peer_id moze lecieć co najwyzej 1 connect na raz.
    // Swift wola FFI wielokrotnie (browser update + retry loop) — bez tego
    // dostajemy 8 QUIC connectow zanim pierwsze handshake sie skonczy.
    {
        let mut set = inflight().lock().expect("inflight poisoned");
        if set.contains(&endpoint_id_hex) {
            debug!(
                endpoint_id = %&endpoint_id_hex[..16.min(endpoint_id_hex.len())],
                "skip — connect w toku"
            );
            return true;
        }
        set.insert(endpoint_id_hex.clone());
    }

    info!(
        endpoint_id = %&endpoint_id_hex[..16.min(endpoint_id_hex.len())],
        addr = %socket_addr,
        "Dial peera z Bonjour"
    );

    // Preferuj biezacy tokio runtime (gdy FFI leci z Rust taska), w przeciwnym
    // razie uzyj globalnego MESH_RUNTIME ustawionego po starcie mesh.
    let rt = match tokio::runtime::Handle::try_current()
        .ok()
        .or_else(|| MESH_RUNTIME.get().cloned())
    {
        Some(h) => h,
        None => {
            warn!("discovered peer: brak aktywnego tokio runtime");
            // Zwolnij slot, inaczej kolejne FFI dla tego peera nigdy nie dojda.
            let _ = inflight().lock().map(|mut s| s.remove(&endpoint_id_hex));
            return false;
        }
    };

    rt.spawn(async move {
        let ep_short = endpoint_id_hex
            .get(..16)
            .unwrap_or(&endpoint_id_hex)
            .to_string();
        let result = manager
            .connect_to_peer_direct(&endpoint_id_hex, socket_addr)
            .await;
        match result {
            Ok(()) => info!(endpoint_id = %ep_short, "connect_to_peer_direct OK"),
            Err(e) => debug!(endpoint_id = %ep_short, "connect_to_peer_direct: {}", e),
        }
        // Odblokuj slot w inflight — ok albo error, obie sciezki pozwalaja
        // na kolejne proby (ok → nastepne FFI bede odfiltrowane przez
        // is_connected; err → retry z NWBrowser cache ma sens).
        let _ = inflight().lock().map(|mut s| s.remove(&endpoint_id_hex));
    });
    true
}

/// Globalny uchwyt do runtime'u tokio w ktorym dziala mesh. Ustawiany raz
/// razem z MESH_HANDLE — FFI uzywa go do spawnu async connect_to_peer_direct.
static MESH_RUNTIME: OnceLock<tokio::runtime::Handle> = OnceLock::new();

pub fn set_mesh_runtime(handle: tokio::runtime::Handle) {
    let _ = MESH_RUNTIME.set(handle);
}

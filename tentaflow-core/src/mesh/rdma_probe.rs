// =============================================================================
// Plik: mesh/rdma_probe.rs
// Opis: RDMA bandwidth probe — pomiar przepustowosci przez RDMA.
//       Linux: libibverbs (rdma-sys), feature-gated "rdma-probe".
//       macOS: Swift NWConnection bridge (Network.framework / Thunderbolt 5).
//       Wzorowany na ibv_rc_pingpong z rdma-core (Linux) oraz
//       mlx_swift_bridge.rs (macOS FFI pattern).
// =============================================================================

use anyhow::Result;

/// Wynik probing RDMA
#[derive(Debug, Clone)]
pub struct RdmaProbeResult {
    pub bytes_transferred: u64,
    pub duration_ms: u64,
    pub bandwidth_mbps: f64,
    pub latency_us: f64,
    pub rdma_device: String,
}

// =============================================================================
// Cross-platform API — dispatchuje do platformowych implementacji
// =============================================================================

/// Sprawdz czy RDMA jest dostepne na tym urzadzeniu
pub fn is_rdma_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new("/sys/class/infiniband").exists()
            && std::fs::read_dir("/sys/class/infiniband")
                .map(|mut d| d.next().is_some())
                .unwrap_or(false)
    }
    #[cfg(target_os = "macos")]
    {
        macos_rdma::is_rdma_available_macos()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        false
    }
}

/// Lista dostepnych urzadzen RDMA
pub fn list_rdma_devices() -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        let path = std::path::Path::new("/sys/class/infiniband");
        if !path.exists() {
            return Vec::new();
        }

        std::fs::read_dir(path)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }
    #[cfg(target_os = "macos")]
    {
        macos_rdma::list_rdma_devices_macos()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Vec::new()
    }
}

/// Znajdz urzadzenie RDMA powiazane z danym interfejsem sieciowym
pub fn find_rdma_device_for_interface(interface_name: &str) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let path = format!("/sys/class/net/{}/device/infiniband", interface_name);
        let path = std::path::Path::new(&path);
        if !path.exists() {
            return None;
        }

        std::fs::read_dir(path)
            .ok()?
            .filter_map(|e| e.ok())
            .next()
            .map(|e| e.file_name().to_string_lossy().to_string())
    }
    #[cfg(target_os = "macos")]
    {
        macos_rdma::find_rdma_device_for_interface_macos(interface_name)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = interface_name;
        None
    }
}

/// RDMA probe server — nasluchuje i mierzy przepustowosc.
/// Na Linuxie wymaga rdma_device, na macOS uzywa Thunderbolt/NWConnection.
pub async fn start_rdma_probe_server(
    bind_ip: &str,
    rdma_device: &str,
    nonce: &[u8; 32],
    duration_ms: u32,
) -> Result<(u16, tokio::task::JoinHandle<Result<RdmaProbeResult>>)> {
    #[cfg(target_os = "linux")]
    {
        linux_rdma::start_rdma_probe_server_linux(bind_ip, rdma_device, nonce, duration_ms).await
    }
    #[cfg(target_os = "macos")]
    {
        let _ = rdma_device;
        macos_rdma::start_rdma_probe_server_macos(bind_ip, nonce, duration_ms).await
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (bind_ip, rdma_device, nonce, duration_ms);
        Err(anyhow!("RDMA nie jest obslugiwane na tej platformie"))
    }
}

/// RDMA probe client — laczy sie z serwerem i mierzy przepustowosc
pub async fn start_rdma_probe_client(
    target_addr: &str,
    target_port: u16,
    rdma_device: &str,
    nonce: &[u8; 32],
    duration_ms: u32,
) -> Result<RdmaProbeResult> {
    #[cfg(target_os = "linux")]
    {
        linux_rdma::start_rdma_probe_client_linux(
            target_addr,
            target_port,
            rdma_device,
            nonce,
            duration_ms,
        )
        .await
    }
    #[cfg(target_os = "macos")]
    {
        let _ = rdma_device;
        macos_rdma::start_rdma_probe_client_macos(target_addr, target_port, nonce, duration_ms)
            .await
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (target_addr, target_port, rdma_device, nonce, duration_ms);
        Err(anyhow!("RDMA nie jest obslugiwane na tej platformie"))
    }
}

// =============================================================================
// macOS: Swift NWConnection bridge (Thunderbolt 5 RDMA via Network.framework)
// Pattern identyczny jak inference/mlx_swift_bridge.rs
// =============================================================================

#[cfg(target_os = "macos")]
mod macos_rdma {
    use std::ffi::{c_char, c_void, CStr, CString};
    use std::sync::OnceLock;

    use anyhow::{anyhow, Result};

    // =========================================================================
    // Typy callbackow FFI — musza pasowac do Swift Bridging Header
    // =========================================================================

    /// Callback wynikowy — Swift wywoluje po zakonczeniu probing
    type RdmaResultCallbackFn = extern "C" fn(
        bytes_transferred: u64,
        duration_ms: u64,
        bandwidth_mbps: f64,
        callback_ctx: *mut c_void,
    );

    /// Uruchom RDMA probe server. Zwraca port (>0) lub kod bledu (<0).
    type RdmaProbeServerFn = extern "C" fn(
        bind_ip: *const c_char,
        nonce: *const u8,
        nonce_len: u32,
        duration_ms: u32,
        result_callback: RdmaResultCallbackFn,
        callback_ctx: *mut c_void,
    ) -> i32;

    /// Uruchom RDMA probe client. Zwraca 0=OK, <0=blad.
    type RdmaProbeClientFn = extern "C" fn(
        target_ip: *const c_char,
        target_port: u16,
        nonce: *const u8,
        nonce_len: u32,
        duration_ms: u32,
        result_callback: RdmaResultCallbackFn,
        callback_ctx: *mut c_void,
    ) -> i32;

    /// Sprawdz dostepnosc RDMA (Thunderbolt 5 + Network.framework)
    type RdmaAvailableFn = extern "C" fn() -> bool;

    /// Lista urzadzen RDMA — zwraca JSON array jako C string (caller zwalnia)
    type RdmaListDevicesFn = extern "C" fn() -> *mut c_char;

    // =========================================================================
    // Globalne function pointery (rejestrowane przez Swift przy starcie)
    // =========================================================================

    static RDMA_PROBE_SERVER: OnceLock<RdmaProbeServerFn> = OnceLock::new();
    static RDMA_PROBE_CLIENT: OnceLock<RdmaProbeClientFn> = OnceLock::new();
    static RDMA_AVAILABLE: OnceLock<RdmaAvailableFn> = OnceLock::new();
    static RDMA_LIST_DEVICES: OnceLock<RdmaListDevicesFn> = OnceLock::new();

    // =========================================================================
    // Rejestracja FFI — wywolywane z Swift przy inicjalizacji
    // =========================================================================

    /// Rejestruje callback serwera RDMA probe
    #[no_mangle]
    pub extern "C" fn tentaflow_register_rdma_probe_server(f: RdmaProbeServerFn) {
        let _ = RDMA_PROBE_SERVER.set(f);
    }

    /// Rejestruje callback klienta RDMA probe
    #[no_mangle]
    pub extern "C" fn tentaflow_register_rdma_probe_client(f: RdmaProbeClientFn) {
        let _ = RDMA_PROBE_CLIENT.set(f);
    }

    /// Rejestruje callback sprawdzania dostepnosci RDMA
    #[no_mangle]
    pub extern "C" fn tentaflow_register_rdma_available(f: RdmaAvailableFn) {
        let _ = RDMA_AVAILABLE.set(f);
    }

    /// Rejestruje callback listowania urzadzen RDMA
    #[no_mangle]
    pub extern "C" fn tentaflow_register_rdma_list_devices(f: RdmaListDevicesFn) {
        let _ = RDMA_LIST_DEVICES.set(f);
    }

    // =========================================================================
    // Wrapper na raw pointer — bezpieczne przesylanie miedzy watkami
    // =========================================================================

    /// Opakowanie na `*mut c_void` jako usize — umozliwia przesylanie miedzy watkami.
    /// SAFETY: Swift side gwarantuje thread-safety przez DispatchQueue.
    #[derive(Clone, Copy)]
    struct SendPtr(usize);

    impl SendPtr {
        fn from_raw(ptr: *mut c_void) -> Self {
            Self(ptr as usize)
        }

        fn as_ptr(self) -> *mut c_void {
            self.0 as *mut c_void
        }
    }

    // =========================================================================
    // Publiczne API macOS
    // =========================================================================

    pub fn is_rdma_available_macos() -> bool {
        RDMA_AVAILABLE.get().map(|f| f()).unwrap_or(false)
    }

    pub fn list_rdma_devices_macos() -> Vec<String> {
        let list_fn = match RDMA_LIST_DEVICES.get() {
            Some(f) => f,
            None => return Vec::new(),
        };

        let json_ptr = list_fn();
        if json_ptr.is_null() {
            return Vec::new();
        }

        let json_str = unsafe { CStr::from_ptr(json_ptr) }
            .to_string_lossy()
            .to_string();

        // Zwolnij pamiec zaalokowana po stronie Swift
        unsafe {
            libc_free(json_ptr as *mut c_void);
        }

        serde_json::from_str(&json_str).unwrap_or_default()
    }

    pub fn find_rdma_device_for_interface_macos(interface_name: &str) -> Option<String> {
        // Na macOS Thunderbolt interfejsy to en* (np. en5, en6)
        // Jesli bridge jest zarejestrowany, sprawdz czy interfejs jest Thunderbolt
        let devices = list_rdma_devices_macos();
        if devices.is_empty() {
            return None;
        }

        // Thunderbolt interfejsy — zwroc pierwszy pasujacy
        if interface_name.starts_with("en") || interface_name.starts_with("bridge") {
            devices.into_iter().next()
        } else {
            None
        }
    }

    pub async fn start_rdma_probe_server_macos(
        bind_ip: &str,
        nonce: &[u8; 32],
        duration_ms: u32,
    ) -> Result<(u16, tokio::task::JoinHandle<Result<super::RdmaProbeResult>>)> {
        let server_fn = *RDMA_PROBE_SERVER
            .get()
            .ok_or_else(|| anyhow!("Swift RDMA bridge nie zarejestrowany — brak probe server"))?;

        let ip = CString::new(bind_ip).map_err(|_| anyhow!("Niepoprawny bind_ip: zawiera NUL"))?;
        let nonce_copy = *nonce;
        let dur = duration_ms;

        // Wywolaj Swift na dedykowanym watku (moze blokowac)
        let port = tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel::<super::RdmaProbeResult>();
            let ctx = Box::into_raw(Box::new(tx)) as *mut c_void;

            let port = server_fn(
                ip.as_ptr(),
                nonce_copy.as_ptr(),
                32,
                dur,
                rdma_result_callback_sync,
                ctx,
            );

            if port < 0 {
                return Err(anyhow!("RDMA server probe nie powiodl sie: kod {}", port));
            }

            // Czekaj na wynik z callbacka
            let result = rx
                .recv_timeout(std::time::Duration::from_millis(dur as u64 + 5000))
                .map_err(|_| anyhow!("Timeout oczekiwania na RDMA server callback"))?;

            Ok((port as u16, result))
        })
        .await
        .map_err(|e| anyhow!("Blad watku RDMA server: {}", e))??;

        let (bound_port, result) = port;
        let handle = tokio::spawn(async move { Ok(result) });

        Ok((bound_port, handle))
    }

    pub async fn start_rdma_probe_client_macos(
        target_ip: &str,
        target_port: u16,
        nonce: &[u8; 32],
        duration_ms: u32,
    ) -> Result<super::RdmaProbeResult> {
        let client_fn = *RDMA_PROBE_CLIENT
            .get()
            .ok_or_else(|| anyhow!("Swift RDMA bridge nie zarejestrowany — brak probe client"))?;

        let ip =
            CString::new(target_ip).map_err(|_| anyhow!("Niepoprawny target_ip: zawiera NUL"))?;
        let nonce_copy = *nonce;
        let port = target_port;
        let dur = duration_ms;

        let result = tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel::<super::RdmaProbeResult>();
            let ctx = Box::into_raw(Box::new(tx)) as *mut c_void;

            let ret = client_fn(
                ip.as_ptr(),
                port,
                nonce_copy.as_ptr(),
                32,
                dur,
                rdma_result_callback_sync,
                ctx,
            );

            if ret < 0 {
                return Err(anyhow!("RDMA client probe nie powiodl sie: kod {}", ret));
            }

            rx.recv_timeout(std::time::Duration::from_millis(dur as u64 + 5000))
                .map_err(|_| anyhow!("Timeout oczekiwania na RDMA client callback"))
        })
        .await
        .map_err(|e| anyhow!("Blad watku RDMA client: {}", e))??;

        Ok(result)
    }

    // =========================================================================
    // Callback extern "C" — wolany przez Swift po zakonczeniu probing
    // =========================================================================

    /// Callback dla synchronicznego kanalu (std::sync::mpsc).
    /// Uzywany w spawn_blocking — nie wymaga tokio runtime.
    /// Kontrakt: callback wolany dokladnie raz przez Swift — Box::from_raw zwalnia pamiec.
    /// Zabezpieczenie: AtomicBool zapobiega podwojnemu wywolaniu (double-free).
    extern "C" fn rdma_result_callback_sync(
        bytes_transferred: u64,
        duration_ms: u64,
        bandwidth_mbps: f64,
        callback_ctx: *mut c_void,
    ) {
        use std::sync::atomic::{AtomicBool, Ordering};
        static CALLBACK_FIRED: AtomicBool = AtomicBool::new(false);

        if callback_ctx.is_null() {
            return;
        }

        // Atomicznie ustaw flage — jesli juz byla true, ktos nas ubiegl
        if CALLBACK_FIRED.swap(true, Ordering::SeqCst) {
            return;
        }

        // SAFETY: callback_ctx to Box<std::sync::mpsc::Sender<RdmaProbeResult>>
        // zaalokowany w start_rdma_probe_*_macos. Zwalniamy tutaj.
        let tx = unsafe {
            Box::from_raw(callback_ctx as *mut std::sync::mpsc::Sender<super::RdmaProbeResult>)
        };

        let _ = tx.send(super::RdmaProbeResult {
            bytes_transferred,
            duration_ms,
            bandwidth_mbps,
            latency_us: 0.0,
            rdma_device: "thunderbolt5-nw".to_string(),
        });

        // Zresetuj flage dla nastepnego uzycia
        CALLBACK_FIRED.store(false, Ordering::SeqCst);
    }

    // =========================================================================
    // Pomocnicza funkcja do zwalniania pamieci C
    // =========================================================================

    extern "C" {
        #[link_name = "free"]
        fn libc_free(ptr: *mut c_void);
    }
}

// =============================================================================
// Linux: libibverbs (reczne FFI) — pelna implementacja RDMA probe
// Feature-gated: kompilowany tylko z cfg(feature = "rdma-probe")
// =============================================================================

#[cfg(target_os = "linux")]
mod linux_rdma {
    use anyhow::{anyhow, Result};
    use std::net::SocketAddr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use super::RdmaProbeResult;
    use crate::mesh::ibverbs_ffi::*;

    /// Znajdz GID index dla RoCE v2 z IPv4-mapped adresem.
    /// Iteruje po sysfs i szuka wpisu z typem "RoCE v2" i GID zawierajacym "ffff:" (IPv4-mapped).
    /// Zwraca None jesli nie znaleziono pasujacego GID.
    fn find_rocev2_gid_index(device_name: &str) -> Option<u8> {
        for i in 0..16u8 {
            let type_path = format!(
                "/sys/class/infiniband/{}/ports/1/gid_attrs/types/{}",
                device_name, i
            );
            let gid_path = format!("/sys/class/infiniband/{}/ports/1/gids/{}", device_name, i);

            let gid_type = match std::fs::read_to_string(&type_path) {
                Ok(t) => t,
                Err(_) => continue,
            };
            if !gid_type.trim().contains("RoCE v2") {
                continue;
            }

            let gid = match std::fs::read_to_string(&gid_path) {
                Ok(g) => g,
                Err(_) => continue,
            };
            // IPv4-mapped GID: 0000:0000:0000:0000:0000:ffff:XXXX:XXXX
            if gid.contains("ffff:") {
                return Some(i);
            }
        }
        None
    }

    /// Informacje o QP potrzebne do nawiazania polaczenia RC
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    struct QpInfo {
        lid: u16,
        qpn: u32,
        psn: u32,
        gid: [u8; 16],
    }

    /// Kontekst RDMA — opakowuje urzadzenie, PD, CQ, QP, MR.
    /// Zarzadza cyklem zycia zasobow libibverbs.
    struct RdmaContext {
        device_name: String,
        ctx: *mut ibv_context,
        pd: *mut ibv_pd,
        cq: *mut ibv_cq,
        qp: *mut ibv_qp,
        mr: *mut ibv_mr,
        buf: Vec<u8>,
        local_psn: u32,
        gid_index: u8,
        active_mtu: u32,
    }

    // Surowe wskazniki nie sa Send domyslnie.
    // Bezpieczne poniewaz RdmaContext jest uzywany wylacznie w spawn_blocking
    // (jeden watek na raz), a zasoby sa zwalniane w Drop.
    unsafe impl Send for RdmaContext {}

    /// 64 MB bufor RDMA — duzy bufor minimalizuje overhead na operacje
    const RDMA_BUF_SIZE: usize = 64 * 1024 * 1024;

    pub async fn start_rdma_probe_server_linux(
        bind_ip: &str,
        rdma_device: &str,
        nonce: &[u8; 32],
        duration_ms: u32,
    ) -> Result<(u16, tokio::task::JoinHandle<Result<RdmaProbeResult>>)> {
        let addr: SocketAddr = format!("{}:0", bind_ip).parse()?;
        let listener = TcpListener::bind(addr).await?;
        let port = listener.local_addr()?.port();

        let device = rdma_device.to_string();
        let nonce_copy = *nonce;
        let dur = duration_ms;

        let handle = tokio::spawn(async move {
            let timeout = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                run_rdma_server(listener, &device, &nonce_copy, dur),
            )
            .await;

            match timeout {
                Ok(r) => r,
                Err(_) => Ok(RdmaProbeResult {
                    bytes_transferred: 0,
                    duration_ms: 0,
                    bandwidth_mbps: 0.0,
                    latency_us: 0.0,
                    rdma_device: device,
                }),
            }
        });

        Ok((port, handle))
    }

    async fn run_rdma_server(
        listener: TcpListener,
        rdma_device: &str,
        nonce: &[u8; 32],
        duration_ms: u32,
    ) -> Result<RdmaProbeResult> {
        let (mut stream, _) = listener.accept().await?;

        // Weryfikuj nonce
        let mut nonce_buf = [0u8; 32];
        stream.read_exact(&mut nonce_buf).await?;
        if nonce_buf != *nonce {
            return Err(anyhow!("Niepoprawny nonce RDMA"));
        }

        // RDMA setup w spawn_blocking (operacje synchroniczne)
        let device = rdma_device.to_string();
        let dur = duration_ms;

        let (local_info, rdma_ctx) =
            tokio::task::spawn_blocking(move || -> Result<(QpInfo, RdmaContext)> {
                let ctx = RdmaContext::new(&device)?;
                let info = ctx.get_local_info()?;
                Ok((info, ctx))
            })
            .await??;

        // Wyslij lokalne QP info
        let info_bytes = serde_json::to_vec(&local_info)?;
        let len = info_bytes.len() as u32;
        stream.write_all(&len.to_le_bytes()).await?;
        stream.write_all(&info_bytes).await?;

        // Odbierz remote QP info
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await?;
        let remote_len = u32::from_le_bytes(len_buf) as usize;
        if remote_len > 4096 {
            return Err(anyhow!("QP info zbyt duze: {} bajtow", remote_len));
        }
        let mut remote_buf = vec![0u8; remote_len];
        stream.read_exact(&mut remote_buf).await?;
        let remote_info: QpInfo = serde_json::from_slice(&remote_buf)?;

        // Polacz QP i odbieraj dane
        let result = tokio::task::spawn_blocking(move || -> Result<RdmaProbeResult> {
            rdma_ctx.connect_and_receive(&remote_info, dur)
        })
        .await??;

        Ok(result)
    }

    pub async fn start_rdma_probe_client_linux(
        target_addr: &str,
        target_port: u16,
        rdma_device: &str,
        nonce: &[u8; 32],
        duration_ms: u32,
    ) -> Result<RdmaProbeResult> {
        let addr: SocketAddr = format!("{}:{}", target_addr, target_port).parse()?;
        let mut stream = TcpStream::connect(addr).await?;

        // Wyslij nonce
        stream.write_all(nonce).await?;

        // RDMA setup
        let device = rdma_device.to_string();
        let dur = duration_ms;

        let (local_info, rdma_ctx) =
            tokio::task::spawn_blocking(move || -> Result<(QpInfo, RdmaContext)> {
                let ctx = RdmaContext::new(&device)?;
                let info = ctx.get_local_info()?;
                Ok((info, ctx))
            })
            .await??;

        // Odbierz remote QP info
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await?;
        let remote_len = u32::from_le_bytes(len_buf) as usize;
        if remote_len > 4096 {
            return Err(anyhow!("QP info zbyt duze: {} bajtow", remote_len));
        }
        let mut remote_buf = vec![0u8; remote_len];
        stream.read_exact(&mut remote_buf).await?;
        let remote_info: QpInfo = serde_json::from_slice(&remote_buf)?;

        // Wyslij lokalne QP info
        let info_bytes = serde_json::to_vec(&local_info)?;
        let len = info_bytes.len() as u32;
        stream.write_all(&len.to_le_bytes()).await?;
        stream.write_all(&info_bytes).await?;

        // Polacz QP i wysylaj dane
        let result = tokio::task::spawn_blocking(move || -> Result<RdmaProbeResult> {
            rdma_ctx.connect_and_send(&remote_info, dur)
        })
        .await??;

        Ok(result)
    }

    impl RdmaContext {
        fn new(device_name: &str) -> Result<Self> {
            // Walidacja rozmiaru ibv_send_wr na runtime — jesli jest zly, RDMA probe nie zadziala poprawnie
            let wr_size = std::mem::size_of::<ibv_send_wr>();
            if wr_size < 100 || wr_size > 300 {
                tracing::warn!(
                    "ibv_send_wr size = {} — moze byc niepoprawny dla tej wersji libibverbs",
                    wr_size
                );
            }

            unsafe {
                // Pobierz liste urzadzen RDMA
                let mut num_devices: i32 = 0;
                let device_list = ibv_get_device_list(&mut num_devices);
                if device_list.is_null() || num_devices == 0 {
                    return Err(anyhow!("Brak urzadzen RDMA"));
                }

                // Znajdz urzadzenie po nazwie
                let mut target_dev = std::ptr::null_mut();
                for i in 0..num_devices as isize {
                    let dev = *device_list.offset(i);
                    if dev.is_null() {
                        continue;
                    }
                    let name = std::ffi::CStr::from_ptr(ibv_get_device_name(dev));
                    if name.to_str().unwrap_or("") == device_name {
                        target_dev = dev;
                        break;
                    }
                }

                if target_dev.is_null() {
                    ibv_free_device_list(device_list);
                    return Err(anyhow!("Nie znaleziono urzadzenia RDMA: {}", device_name));
                }

                // Otworz kontekst urzadzenia
                let ctx = ibv_open_device(target_dev);
                ibv_free_device_list(device_list);
                if ctx.is_null() {
                    return Err(anyhow!("ibv_open_device failed"));
                }

                // Alokuj Protection Domain
                let pd = ibv_alloc_pd(ctx);
                if pd.is_null() {
                    ibv_close_device(ctx);
                    return Err(anyhow!("ibv_alloc_pd failed"));
                }

                // Stworz Completion Queue (128 wpisow)
                let cq = ibv_create_cq(ctx, 128, std::ptr::null_mut(), std::ptr::null_mut(), 0);
                if cq.is_null() {
                    ibv_dealloc_pd(pd);
                    ibv_close_device(ctx);
                    return Err(anyhow!("ibv_create_cq failed"));
                }

                // Bufor danych wypelniony wzorcem
                let mut buf = vec![0xABu8; RDMA_BUF_SIZE];

                // Zarejestruj region pamieci z dostepem local write + remote write + remote read
                let access =
                    IBV_ACCESS_LOCAL_WRITE | IBV_ACCESS_REMOTE_WRITE | IBV_ACCESS_REMOTE_READ;
                let mr = ibv_reg_mr(pd, buf.as_mut_ptr().cast(), RDMA_BUF_SIZE, access as i32);
                if mr.is_null() {
                    ibv_destroy_cq(cq);
                    ibv_dealloc_pd(pd);
                    ibv_close_device(ctx);
                    return Err(anyhow!("ibv_reg_mr failed"));
                }

                // Stworz Queue Pair (RC — Reliable Connection)
                let mut qp_init_attr: ibv_qp_init_attr = std::mem::zeroed();
                qp_init_attr.send_cq = cq;
                qp_init_attr.recv_cq = cq;
                qp_init_attr.qp_type = IBV_QPT_RC;
                qp_init_attr.cap.max_send_wr = 128;
                qp_init_attr.cap.max_recv_wr = 128;
                qp_init_attr.cap.max_send_sge = 1;
                qp_init_attr.cap.max_recv_sge = 1;

                let qp = ibv_create_qp(pd, &mut qp_init_attr);
                if qp.is_null() {
                    ibv_dereg_mr(mr);
                    ibv_destroy_cq(cq);
                    ibv_dealloc_pd(pd);
                    ibv_close_device(ctx);
                    return Err(anyhow!("ibv_create_qp failed"));
                }

                // Przejdz QP do stanu INIT
                let mut attr: ibv_qp_attr = std::mem::zeroed();
                attr.qp_state = IBV_QPS_INIT;
                attr.pkey_index = 0;
                attr.port_num = 1;
                attr.qp_access_flags = access;

                let mask = IBV_QP_STATE | IBV_QP_PKEY_INDEX | IBV_QP_PORT | IBV_QP_ACCESS_FLAGS;

                let ret = ibv_modify_qp(qp, &mut attr, mask);
                if ret != 0 {
                    ibv_destroy_qp(qp);
                    ibv_dereg_mr(mr);
                    ibv_destroy_cq(cq);
                    ibv_dealloc_pd(pd);
                    ibv_close_device(ctx);
                    return Err(anyhow!("ibv_modify_qp(INIT) failed: {}", ret));
                }

                // Losowy PSN (24-bit)
                let local_psn = rand::random::<u32>() & 0x00FF_FFFF;

                // Wykryj GID index dla RoCE v2 (fallback na 0 dla IB)
                let gid_index = find_rocev2_gid_index(device_name).unwrap_or(0);

                // Odczytaj aktywne MTU z portu
                let mut port_attr: ibv_port_attr = std::mem::zeroed();
                let ret = ibv_query_port(ctx, 1, &mut port_attr);
                let active_mtu = if ret == 0
                    && port_attr.active_mtu >= IBV_MTU_256
                    && port_attr.active_mtu <= IBV_MTU_4096
                {
                    port_attr.active_mtu
                } else {
                    IBV_MTU_1024
                };

                Ok(RdmaContext {
                    device_name: device_name.to_string(),
                    ctx,
                    pd,
                    cq,
                    qp,
                    mr,
                    buf,
                    local_psn,
                    gid_index,
                    active_mtu,
                })
            }
        }

        fn get_local_info(&self) -> Result<QpInfo> {
            unsafe {
                // Pobierz atrybuty portu (LID)
                let mut port_attr: ibv_port_attr = std::mem::zeroed();
                let ret = ibv_query_port(self.ctx, 1, &mut port_attr);
                if ret != 0 {
                    return Err(anyhow!("ibv_query_port failed: {}", ret));
                }

                // Pobierz GID (potrzebne dla RoCE) — uzywamy wykrytego indeksu
                let mut gid: ibv_gid = std::mem::zeroed();
                ibv_query_gid(self.ctx, 1, self.gid_index as i32, &mut gid);

                Ok(QpInfo {
                    lid: port_attr.lid,
                    qpn: (*self.qp).qp_num,
                    psn: self.local_psn,
                    gid: gid.raw,
                })
            }
        }

        /// Przejdz QP przez stany INIT -> RTR -> RTS
        fn connect_qp(&self, remote: &QpInfo) -> Result<()> {
            unsafe {
                // RTR (Ready to Receive)
                let mut attr: ibv_qp_attr = std::mem::zeroed();
                attr.qp_state = IBV_QPS_RTR;
                attr.path_mtu = self.active_mtu;
                attr.dest_qp_num = remote.qpn;
                attr.rq_psn = remote.psn;
                attr.max_dest_rd_atomic = 1;
                attr.min_rnr_timer = 12;

                attr.ah_attr.dlid = remote.lid;
                attr.ah_attr.sl = 0;
                attr.ah_attr.src_path_bits = 0;
                attr.ah_attr.port_num = 1;

                // RoCE (lid == 0) wymaga Global Routing Header
                if remote.lid == 0 {
                    attr.ah_attr.is_global = 1;
                    attr.ah_attr.grh.dgid.raw = remote.gid;
                    attr.ah_attr.grh.sgid_index = self.gid_index;
                    attr.ah_attr.grh.hop_limit = 64;
                }

                let mask = IBV_QP_STATE
                    | IBV_QP_AV
                    | IBV_QP_PATH_MTU
                    | IBV_QP_DEST_QPN
                    | IBV_QP_RQ_PSN
                    | IBV_QP_MAX_DEST_RD_ATOMIC
                    | IBV_QP_MIN_RNR_TIMER;

                let ret = ibv_modify_qp(self.qp, &mut attr, mask);
                if ret != 0 {
                    return Err(anyhow!("ibv_modify_qp(RTR) failed: {}", ret));
                }

                // RTS (Ready to Send)
                let mut attr: ibv_qp_attr = std::mem::zeroed();
                attr.qp_state = IBV_QPS_RTS;
                attr.timeout = 14;
                attr.retry_cnt = 7;
                attr.rnr_retry = 7;
                attr.sq_psn = self.local_psn;
                attr.max_rd_atomic = 1;

                let mask = IBV_QP_STATE
                    | IBV_QP_TIMEOUT
                    | IBV_QP_RETRY_CNT
                    | IBV_QP_RNR_RETRY
                    | IBV_QP_SQ_PSN
                    | IBV_QP_MAX_QP_RD_ATOMIC;

                let ret = ibv_modify_qp(self.qp, &mut attr, mask);
                if ret != 0 {
                    return Err(anyhow!("ibv_modify_qp(RTS) failed: {}", ret));
                }
            }

            Ok(())
        }

        /// Pomiar RDMA latency: warmup + inline SEND/poll mediana
        /// Wzorowany na ib_send_lat: 100 warmup + 1000 pomiarow, mediana RTT/2
        fn measure_rdma_latency(&self) -> Result<f64> {
            const WARMUP: usize = 100;
            const ITERS: usize = 1000;
            const PING_SIZE: u32 = 2;

            unsafe {
                // Warmup — rozgrzej QP, cache, TLB
                for _ in 0..WARMUP {
                    let mut sge: ibv_sge = std::mem::zeroed();
                    sge.addr = self.buf.as_ptr() as u64;
                    sge.length = PING_SIZE;
                    sge.lkey = (*self.mr).lkey;

                    let mut wr: ibv_send_wr = std::mem::zeroed();
                    wr.sg_list = &mut sge;
                    wr.num_sge = 1;
                    wr.opcode = IBV_WR_SEND;
                    wr.send_flags = IBV_SEND_SIGNALED | 0x8; // IBV_SEND_INLINE = 0x8

                    let mut bad_wr: *mut ibv_send_wr = std::ptr::null_mut();
                    if ibv_post_send(self.qp, &mut wr, &mut bad_wr) != 0 {
                        break;
                    }

                    let mut wc: ibv_wc = std::mem::zeroed();
                    loop {
                        let n = ibv_poll_cq(self.cq, 1, &mut wc);
                        if n > 0 {
                            break;
                        }
                        if n < 0 {
                            return Ok(0.0);
                        }
                    }
                }

                // Pomiar — zbierz RTT z ITERS iteracji
                let mut samples = Vec::with_capacity(ITERS);
                for _ in 0..ITERS {
                    let mut sge: ibv_sge = std::mem::zeroed();
                    sge.addr = self.buf.as_ptr() as u64;
                    sge.length = PING_SIZE;
                    sge.lkey = (*self.mr).lkey;

                    let mut wr: ibv_send_wr = std::mem::zeroed();
                    wr.sg_list = &mut sge;
                    wr.num_sge = 1;
                    wr.opcode = IBV_WR_SEND;
                    wr.send_flags = IBV_SEND_SIGNALED | 0x8;

                    let t = std::time::Instant::now();
                    let mut bad_wr: *mut ibv_send_wr = std::ptr::null_mut();
                    if ibv_post_send(self.qp, &mut wr, &mut bad_wr) != 0 {
                        break;
                    }

                    let mut wc: ibv_wc = std::mem::zeroed();
                    loop {
                        let n = ibv_poll_cq(self.cq, 1, &mut wc);
                        if n > 0 {
                            if wc.status == IBV_WC_SUCCESS {
                                samples.push(t.elapsed().as_nanos() as f64 / 1000.0);
                            }
                            break;
                        }
                        if n < 0 {
                            break;
                        }
                    }
                }

                if samples.is_empty() {
                    return Ok(0.0);
                }

                // Mediana
                samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let median = samples[samples.len() / 2];

                // SEND latency = one-way (nie RTT, bo to jest post_send -> completion)
                Ok(median)
            }
        }

        /// Polacz QP i wysylaj dane (strona klienta)
        fn connect_and_send(&self, remote: &QpInfo, duration_ms: u32) -> Result<RdmaProbeResult> {
            self.connect_qp(remote)?;

            // Pomiar latency: warmup + inline SEND RTT (jak ib_send_lat)
            let latency_us = self.measure_rdma_latency()?;

            let start = std::time::Instant::now();
            let deadline = start + std::time::Duration::from_millis(duration_ms as u64);
            let mut total_bytes: u64 = 0;

            unsafe {
                // Glowna petla: wysylaj pelne 64MB bufory
                while std::time::Instant::now() < deadline {
                    let mut sge: ibv_sge = std::mem::zeroed();
                    sge.addr = self.buf.as_ptr() as u64;
                    sge.length = RDMA_BUF_SIZE as u32;
                    sge.lkey = (*self.mr).lkey;

                    let mut wr: ibv_send_wr = std::mem::zeroed();
                    wr.wr_id = 0;
                    wr.sg_list = &mut sge;
                    wr.num_sge = 1;
                    wr.opcode = IBV_WR_SEND;
                    wr.send_flags = IBV_SEND_SIGNALED;

                    let mut bad_wr: *mut ibv_send_wr = std::ptr::null_mut();
                    let ret = ibv_post_send(self.qp, &mut wr, &mut bad_wr);
                    if ret != 0 {
                        break;
                    }

                    let mut wc: ibv_wc = std::mem::zeroed();
                    loop {
                        let n = ibv_poll_cq(self.cq, 1, &mut wc);
                        if n > 0 {
                            if wc.status != IBV_WC_SUCCESS {
                                return Err(anyhow!("RDMA send WC error: status={}", wc.status));
                            }
                            total_bytes += RDMA_BUF_SIZE as u64;
                            break;
                        }
                        if n < 0 {
                            return Err(anyhow!("ibv_poll_cq failed"));
                        }
                    }
                }
            }

            let elapsed = start.elapsed().as_millis() as u64;
            let bandwidth_mbps = if elapsed > 0 {
                (total_bytes as f64 * 8.0) / (elapsed as f64) / 1000.0
            } else {
                0.0
            };

            Ok(RdmaProbeResult {
                bytes_transferred: total_bytes,
                duration_ms: elapsed,
                bandwidth_mbps,
                latency_us,
                rdma_device: self.device_name.clone(),
            })
        }

        /// Polacz QP i odbieraj dane (strona serwera)
        fn connect_and_receive(
            &self,
            remote: &QpInfo,
            duration_ms: u32,
        ) -> Result<RdmaProbeResult> {
            self.connect_qp(remote)?;

            let start = std::time::Instant::now();
            // Dodatkowe 3s na setup i ostatnie pakiety
            let deadline = start + std::time::Duration::from_millis(duration_ms as u64 + 3000);
            let mut total_bytes: u64 = 0;

            unsafe {
                // Pre-post 128 receive buforow
                for i in 0..128u64 {
                    let mut sge: ibv_sge = std::mem::zeroed();
                    sge.addr = self.buf.as_ptr() as u64;
                    sge.length = RDMA_BUF_SIZE as u32;
                    sge.lkey = (*self.mr).lkey;

                    let mut wr: ibv_recv_wr = std::mem::zeroed();
                    wr.wr_id = i;
                    wr.sg_list = &mut sge;
                    wr.num_sge = 1;

                    let mut bad_wr: *mut ibv_recv_wr = std::ptr::null_mut();
                    let ret = ibv_post_recv(self.qp, &mut wr, &mut bad_wr);
                    if ret != 0 {
                        return Err(anyhow!("ibv_post_recv failed: {}", ret));
                    }
                }

                // Odbieraj az do deadline
                while std::time::Instant::now() < deadline {
                    let mut wc: ibv_wc = std::mem::zeroed();
                    let n = ibv_poll_cq(self.cq, 1, &mut wc);
                    if n > 0 {
                        if wc.status == IBV_WC_SUCCESS {
                            total_bytes += wc.byte_len as u64;

                            // Re-post receive bufor
                            let mut sge: ibv_sge = std::mem::zeroed();
                            sge.addr = self.buf.as_ptr() as u64;
                            sge.length = RDMA_BUF_SIZE as u32;
                            sge.lkey = (*self.mr).lkey;

                            let mut wr: ibv_recv_wr = std::mem::zeroed();
                            wr.wr_id = wc.wr_id;
                            wr.sg_list = &mut sge;
                            wr.num_sge = 1;

                            let mut bad_wr: *mut ibv_recv_wr = std::ptr::null_mut();
                            ibv_post_recv(self.qp, &mut wr, &mut bad_wr);
                        }
                    } else if n < 0 {
                        break;
                    }
                }
            }

            let elapsed = start.elapsed().as_millis() as u64;
            let bandwidth_mbps = if elapsed > 0 {
                (total_bytes as f64 * 8.0) / (elapsed as f64) / 1000.0
            } else {
                0.0
            };

            Ok(RdmaProbeResult {
                bytes_transferred: total_bytes,
                duration_ms: elapsed,
                bandwidth_mbps,
                latency_us: 0.0,
                rdma_device: self.device_name.clone(),
            })
        }
    }

    impl Drop for RdmaContext {
        fn drop(&mut self) {
            unsafe {
                // Zwalniaj w odwrotnej kolejnosci tworzenia
                if !self.qp.is_null() {
                    ibv_destroy_qp(self.qp);
                }
                if !self.mr.is_null() {
                    ibv_dereg_mr(self.mr);
                }
                if !self.cq.is_null() {
                    ibv_destroy_cq(self.cq);
                }
                if !self.pd.is_null() {
                    ibv_dealloc_pd(self.pd);
                }
                if !self.ctx.is_null() {
                    ibv_close_device(self.ctx);
                }
            }
        }
    }
}

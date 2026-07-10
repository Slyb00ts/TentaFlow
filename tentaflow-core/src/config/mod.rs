// =============================================================================
// Plik: config/mod.rs
// Opis: Konfiguracja wezla — NodeConfig (dawniej RouterConfig). Parsowanie i
//       walidacja config.toml. Obsluguje konfiguracje routera, mesh networking
//       oraz lokalnej inferencji.
// Przyklad:
//   let config = NodeConfig::from_file("config.toml")?;
//   println!("Port: {}", config.protocols.openai_api.bind);
// =============================================================================

use crate::error::{CoreError, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

// =============================================================================
// Alias kompatybilnosci — istniejacy kod uzywajacy RouterConfig dziala dalej
// =============================================================================

pub type RouterConfig = NodeConfig;

// =============================================================================
// Glowna struktura konfiguracji wezla
// =============================================================================

/// Glowna struktura konfiguracji wezla.
///
/// Odpowiada strukturze pliku config.toml. Wszystkie pola sa deserializowane
/// automatycznie przez serde z TOML. Dawniej `RouterConfig` — alias zachowany
/// dla backward compatibility.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NodeConfig {
    /// Ogolne ustawienia serwera (limity polaczen, watki)
    pub server: ServerConfig,

    /// Konfiguracja protokolow wejsciowych (OpenAI API, gRPC, QUIC)
    pub protocols: ProtocolsConfig,

    /// Middleware (request/response validation, rate limiting)
    pub middleware: MiddlewareConfig,

    /// Rate limiting (limity per-second, burst)
    #[serde(default)]
    pub rate_limiting: RateLimitingConfig,

    /// Load balancing (health checks, circuit breaker)
    pub load_balancing: LoadBalancingConfig,

    /// Monitoring (health checks)
    #[serde(default)]
    pub monitoring: MonitoringConfig,

    /// Memory management (opcjonalne, dla przyszlych optymalizacji)
    #[serde(default)]
    pub memory: Option<MemoryConfig>,

    /// Security (CORS, IP whitelist, API keys)
    #[serde(default)]
    pub security: Option<SecurityConfig>,

    /// Rola wezla w mesh (router, desktop, mobile)
    #[serde(default)]
    pub node_role: NodeRole,

    /// Konfiguracja mesh networking (opcjonalna)
    #[serde(default)]
    pub mesh: Option<MeshConfig>,

    /// Konfiguracja lokalnej inferencji (opcjonalna)
    #[serde(default)]
    pub inference: Option<InferenceConfig>,

    /// Runtime services subsystem (port range, supervisor cadence, restart policy).
    /// Used by the unified services refactor (services_repo + services::deploy/supervisor).
    #[serde(default)]
    pub services_runtime: ServicesRuntimeConfig,

    /// Metryki zuzycia tokenow + egzekwowanie limitow (sekcja `[token_metrics]`).
    #[serde(default)]
    pub token_metrics: TokenMetricsConfig,

    /// Multi-process vision worker sharding (sekcja `[vision]`).
    #[serde(default)]
    pub vision: VisionConfig,
}

// =============================================================================
// Konfiguracja vision workers (sekcja [vision])
// =============================================================================

/// Vision / camera-CV runtime configuration (docs/VISION_WORKER_SHARDING.md
/// for the worker fleet). The config TOML is the ONLY operator mechanism for
/// every knob here — there are deliberately NO environment variables. Every
/// default matches the historical behavior, so a node without a `[vision]`
/// section behaves exactly as before. The parsed section is frozen process-wide
/// via `vision::settings::init` at startup; vision worker processes receive it
/// as a serialized CLI argument from the spawning supervisor.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VisionConfig {
    /// Vision worker processes spawned per GPU in the vision GPU set. `0`
    /// (default) disables the worker fleet entirely: no processes are spawned
    /// and no link socket is bound.
    #[serde(default)]
    pub workers_per_gpu: usize,

    /// CUDA device set the vision session pools spread across. Grammar:
    /// empty (default) → device `[0]`; a bare count `"2"` → devices `[0, 1]`;
    /// an explicit comma list `"0,2,3"` → exactly those device ids.
    #[serde(default)]
    pub gpus: String,

    /// RF-DETR detector session-pool size (each session ≈2.6 GB VRAM).
    #[serde(default = "default_vision_sessions")]
    pub detector_sessions: usize,

    /// State-classifier (nalepka-stan) session-pool size.
    #[serde(default = "default_vision_sessions")]
    pub stan_sessions: usize,

    /// Plate-OCR session-pool size.
    #[serde(default = "default_vision_sessions")]
    pub plate_sessions: usize,

    /// ADR-OCR session-pool size.
    #[serde(default = "default_vision_sessions")]
    pub adr_sessions: usize,

    /// PP-OCRv5 fallback OCR session-pool size (shared by det/rec/cls heads).
    #[serde(default = "default_ppocr_sessions")]
    pub ppocr_sessions: usize,

    /// Max resident dynamic `onnx-cv` registry models (LRU eviction).
    #[serde(default = "default_vision_sessions")]
    pub onnx_cv_max_models: usize,

    /// Session-pool size per resident `onnx-cv` registry model.
    #[serde(default = "default_one")]
    pub onnx_cv_sessions_per_model: usize,

    /// TensorRT shape-profile optimization batch for the fixed camera-CV
    /// engines. `None` keeps each model's built-in default (detector 8,
    /// classifier/plate 8). Changing it forces a TRT engine rebuild on load.
    #[serde(default)]
    pub opt_batch: Option<usize>,

    /// TensorRT shape-profile max batch. `None` keeps each model's built-in
    /// default (detector = opt, classifier/plate = max(opt, 16)).
    #[serde(default)]
    pub max_batch: Option<usize>,

    /// Concurrent detect forwards K. `None` (default) follows
    /// `detector_sessions` — N pooled sessions pipeline N batched forwards.
    #[serde(default)]
    pub inflight: Option<usize>,

    /// Cross-camera inference-batcher flush window in microseconds.
    #[serde(default = "default_batch_window_us")]
    pub batch_window_us: u64,

    /// Max cameras whose cold enrichment runs concurrently (1..=1024).
    #[serde(default = "default_cold_workers")]
    pub cold_workers: usize,

    /// Run the small OCR/classifier CRNN heads in TensorRT FP16. Default OFF —
    /// fp16 rounding corrupts character reads; ON only for A/B measurement.
    #[serde(default)]
    pub ocr_fp16: bool,

    /// Per-session TensorRT workspace cap in MiB (clamped 128..=8192). The TRT
    /// 10.x default (0 = all free VRAM) makes pooled sessions unusable at N>1.
    #[serde(default = "default_trt_workspace_mib")]
    pub trt_workspace_mib: usize,

    /// Capture + replay the TRT forward as one CUDA Graph. Opt-in: graph
    /// capture requires stable shapes per session; mixed batches can regress.
    #[serde(default)]
    pub trt_cuda_graph: bool,

    /// Zero-copy DETECT branch: feed the NVDEC device surface straight to the
    /// detector (no download/re-upload round-trip). Opt-in.
    #[serde(default)]
    pub zerocopy_detect: bool,

    /// Zero-copy CROPS path: enrichment cuts crops off the device surface
    /// instead of downloading the full frame to host every stream frame. Opt-in.
    #[serde(default)]
    pub zerocopy_crops: bool,

    /// Trust that `gst_memory_map(GST_MAP_CUDA)` already synced the decode
    /// surface and skip the `cudaDeviceSynchronize` barrier (lower latency;
    /// enable only once confirmed on the target GStreamer build).
    #[serde(default)]
    pub zerocopy_map_sync: bool,

    /// Verify the zero-copy DETECT preprocess against the download path on the
    /// first few frames (correctness gate; logged, never fatal).
    #[serde(default)]
    pub zerocopy_verify: bool,

    /// Verify the zero-copy CROPS path against host crops on the first few
    /// crops (correctness gate; logged, never fatal).
    #[serde(default)]
    pub zerocopy_crops_verify: bool,

    /// GPU-resident NV12 detect path for NVDEC ingest. Default ON; `false`
    /// forces the NVDEC + CPU-convert path (detector reads an RGB frame).
    #[serde(default = "default_true")]
    pub nv12_detect: bool,

    /// GPU scaling branch for the detect frame (cudascale). Default ON;
    /// `false` forces the CPU resize path.
    #[serde(default = "default_true")]
    pub gpu_resize: bool,

    /// When set, OCR calls dump their raw/rectified crops + model-input
    /// tensors as PNGs into this directory. `None` (default) disables dumps.
    #[serde(default)]
    pub ocr_dump_dir: Option<std::path::PathBuf>,

    /// Perspective-deskew plate crops before OCR. Default ON; `false` keeps
    /// the plain bilinear-stretch path (A/B toggle).
    #[serde(default = "default_true")]
    pub ocr_deskew: bool,

    /// Content-trim each ADR placard row before the 32×128 resize. Default ON.
    #[serde(default = "default_true")]
    pub adr_row_trim: bool,

    /// Placard orientations tried by ADR OCR (1 = upright only, up to 4 =
    /// full 0/90/180/270 rotation search). Stationary cameras see placards
    /// upright, so extra rotations are usually wasted forwards.
    #[serde(default = "default_one")]
    pub adr_orientations: usize,

    /// Minimum mean OCR confidence (0..1) the WINNING plate read must reach
    /// before a plate is reported. The plate OCR's `decode_logits_scored`
    /// returns the mean softmax probability of the chosen character across the
    /// non-pad slots: a genuine sharp plate scores ~0.8-0.99, while an occluded
    /// or blurry plate produces low-probability argmaxes (~0.3-0.6). Below this
    /// floor the plate is reported as "unreadable" (`None`) instead of a
    /// fabricated guess. Default 0.5 keeps genuine reads while rejecting the
    /// low-evidence misreads that a naive count vote used to surface.
    #[serde(default = "default_plate_min_confidence")]
    pub plate_min_confidence: f32,

    /// Minimum vote AGREEMENT (winner_weight / total_weight, 0..1) before a
    /// plate is reported. Each frame's read is weighted by its OCR confidence;
    /// a plate whose reads disagree frame-to-frame (occlusion/blur produces
    /// several different low-confidence strings) never lets one variant reach a
    /// clear majority of the weight and is reported "unreadable". A single
    /// confident read has agreement 1.0, so one strong frame still passes.
    /// Default 0.5.
    #[serde(default = "default_plate_min_agreement")]
    pub plate_min_agreement: f32,

    /// Minimum mean OCR confidence (0..1) the winning ADR code must reach. The
    /// ADR CRNN's CTC decode reports the mean softmax-max over the selected
    /// steps (same 0..1 scale as the plate OCR). The UN number already passes
    /// the `snap_adr` catalog snap before it can vote, so this floor is a
    /// secondary guard and defaults lower (0.35) than the plate floor.
    #[serde(default = "default_adr_min_confidence")]
    pub adr_min_confidence: f32,

    /// Minimum vote agreement before an ADR code is reported (same math as
    /// `plate_min_agreement`). Default 0.5.
    #[serde(default = "default_adr_min_agreement")]
    pub adr_min_agreement: f32,

    /// One-shot depth-calibration capture: dump the metric depth map + pose +
    /// lidar cloud to `/tmp/tf_calib/` for the offline `depth_calib` example.
    #[serde(default)]
    pub calib_dump: bool,

    /// Extra seconds (past the camera's connect timeout) an RTSP ingest path
    /// waits for FIRST frames before degrading to the next rung (NVDEC → CPU
    /// convert → CPU decode). Cameras recovering from RTSP-session stress can
    /// take tens of seconds to start delivering, and too small a window makes a
    /// perfectly healthy GPU path fall back to software decode for the whole
    /// session.
    #[serde(default = "default_warmup_extra_secs")]
    pub warmup_extra_secs: u32,

    /// Per-vehicle event recording: whenever detections appear on a camera
    /// (scene non-empty), record fMP4 passthrough video until the scene stays
    /// empty for `event_stop_hysteresis_secs`. Default ON per operator request;
    /// it only ever engages on cameras that actually publish detections (i.e.
    /// have a resolvable analysis pipeline), so nodes without CV are unaffected.
    #[serde(default = "default_true")]
    pub event_recording: bool,

    /// Seconds the scene must stay empty (no detections) before an event
    /// recording is finalized. A new detection inside the window extends the
    /// same recording.
    #[serde(default = "default_event_stop_hysteresis_secs")]
    pub event_stop_hysteresis_secs: u64,

    /// Seconds of video kept in a per-camera rolling buffer while idle and
    /// prepended to each event recording (the vehicle is visible BEFORE the
    /// first detection lands). `0` disables the buffer — the recording then
    /// starts at the first fragment after the trigger. Non-zero keeps the
    /// camera's passthrough mux branch attached permanently (cheap: no
    /// transcode, ~`preroll × bitrate` bytes of RAM per camera).
    #[serde(default = "default_event_preroll_secs")]
    pub event_preroll_secs: u64,

    /// Upper bound for one event recording file. A scene that never empties
    /// (busy gate, stuck detection) rotates to a fresh file at this boundary
    /// instead of growing one unbounded mp4; no video is lost across the cut.
    #[serde(default = "default_event_max_duration_secs")]
    pub event_max_duration_secs: u64,

    /// RTSP lower transport for `rtsp://` cameras (GstRTSPLowerTrans flags
    /// string). Default `"udp+udp-mcast+tcp"` (UDP preferred). Interleaved
    /// `"tcp"` was tried as a fix for UDP media dying silently across routed
    /// networks, but rtspsrc pushes interleaved RTP synchronously from its
    /// connection task and our tee chain's startup backpressure left the socket
    /// unread (Recv-Q grew, session never came online) — measured on the live
    /// camera. Until that is made compatible on a bench, UDP stays the default
    /// and the 10 s mid-session stall watchdog covers silent UDP death.
    /// `rtsps://` URLs always use `"tcp+tls"` regardless.
    #[serde(default = "default_rtsp_protocols")]
    pub rtsp_protocols: String,
}

fn default_warmup_extra_secs() -> u32 {
    20
}

fn default_rtsp_protocols() -> String {
    "udp+udp-mcast+tcp".to_string()
}

impl Default for VisionConfig {
    fn default() -> Self {
        Self {
            workers_per_gpu: 0,
            gpus: String::new(),
            detector_sessions: default_vision_sessions(),
            stan_sessions: default_vision_sessions(),
            plate_sessions: default_vision_sessions(),
            adr_sessions: default_vision_sessions(),
            ppocr_sessions: default_ppocr_sessions(),
            onnx_cv_max_models: default_vision_sessions(),
            onnx_cv_sessions_per_model: default_one(),
            opt_batch: None,
            max_batch: None,
            inflight: None,
            batch_window_us: default_batch_window_us(),
            cold_workers: default_cold_workers(),
            ocr_fp16: false,
            trt_workspace_mib: default_trt_workspace_mib(),
            trt_cuda_graph: false,
            zerocopy_detect: false,
            zerocopy_crops: false,
            zerocopy_map_sync: false,
            zerocopy_verify: false,
            zerocopy_crops_verify: false,
            nv12_detect: default_true(),
            gpu_resize: default_true(),
            ocr_dump_dir: None,
            ocr_deskew: default_true(),
            adr_row_trim: default_true(),
            adr_orientations: default_one(),
            plate_min_confidence: default_plate_min_confidence(),
            plate_min_agreement: default_plate_min_agreement(),
            adr_min_confidence: default_adr_min_confidence(),
            adr_min_agreement: default_adr_min_agreement(),
            calib_dump: false,
            warmup_extra_secs: default_warmup_extra_secs(),
            event_recording: default_true(),
            event_stop_hysteresis_secs: default_event_stop_hysteresis_secs(),
            event_preroll_secs: default_event_preroll_secs(),
            event_max_duration_secs: default_event_max_duration_secs(),
            rtsp_protocols: default_rtsp_protocols(),
        }
    }
}

fn default_event_stop_hysteresis_secs() -> u64 {
    10
}
fn default_event_preroll_secs() -> u64 {
    5
}
fn default_event_max_duration_secs() -> u64 {
    3600
}

fn default_vision_sessions() -> usize {
    4
}
fn default_ppocr_sessions() -> usize {
    2
}
fn default_one() -> usize {
    1
}
fn default_batch_window_us() -> u64 {
    2000
}
fn default_plate_min_confidence() -> f32 {
    0.5
}
fn default_plate_min_agreement() -> f32 {
    0.5
}
fn default_adr_min_confidence() -> f32 {
    0.35
}
fn default_adr_min_agreement() -> f32 {
    0.5
}
fn default_cold_workers() -> usize {
    64
}
fn default_trt_workspace_mib() -> usize {
    1024
}

// =============================================================================
// Konfiguracja runtime serwisow (additive — wariant B refactor unifikacji)
// =============================================================================

/// Konfiguracja podsystemu runtime'u serwisow zarzadzanych przez `services_repo`
/// i `services::deploy/supervisor`. Sekcja TOML: `[services_runtime]`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServicesRuntimeConfig {
    /// Inclusive zakres portow ktore allocator moze rozdawac dla deploymentow.
    #[serde(default = "default_services_port_range")]
    pub port_range: (u16, u16),

    /// Interwal probek health-check w milisekundach.
    #[serde(default = "default_services_health_interval_ms")]
    pub health_check_interval_ms: u64,

    /// Maksymalna liczba prob restartow zanim supervisor oznaczy serwis jako Failed.
    #[serde(default = "default_services_max_restart_attempts")]
    pub max_restart_attempts: u32,

    /// Gorny limit (cap) dla exponential backoff miedzy restartami, w milisekundach.
    #[serde(default = "default_services_restart_backoff_max_ms")]
    pub restart_backoff_max_ms: u64,
}

impl Default for ServicesRuntimeConfig {
    fn default() -> Self {
        Self {
            port_range: default_services_port_range(),
            health_check_interval_ms: default_services_health_interval_ms(),
            max_restart_attempts: default_services_max_restart_attempts(),
            restart_backoff_max_ms: default_services_restart_backoff_max_ms(),
        }
    }
}

// =============================================================================
// Konfiguracja metryk tokenow (sekcja [token_metrics])
// =============================================================================

/// Ustawienia rozliczania tokenow, egzekwowania limitow oraz koordynatora
/// dzierzaw. Wszystkie pola maja domyslne wartosci, wiec brak sekcji
/// `[token_metrics]` daje pelna domyslna konfiguracje.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TokenMetricsConfig {
    /// Czy egzekwowac limity i zliczac zuzycie tokenow.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Interwal flushera capture'ow zuzycia do Sync Ledger (sekundy).
    #[serde(default = "default_token_flush_secs")]
    pub flush_secs: u64,

    /// Interwal cyklu koordynatora dzierzaw (sekundy).
    #[serde(default = "default_token_lease_secs")]
    pub lease_secs: u64,

    /// TTL pojedynczej dzierzawy tokenow (sekundy).
    #[serde(default = "default_token_lease_ttl_secs")]
    pub lease_ttl_secs: u64,

    /// Minimalna pula tokenow przydzielana w jednej dzierzawie.
    #[serde(default = "default_token_min_lease")]
    pub min_lease: i64,
}

impl Default for TokenMetricsConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            flush_secs: default_token_flush_secs(),
            lease_secs: default_token_lease_secs(),
            lease_ttl_secs: default_token_lease_ttl_secs(),
            min_lease: default_token_min_lease(),
        }
    }
}

fn default_token_flush_secs() -> u64 {
    60
}

fn default_token_lease_secs() -> u64 {
    30
}

fn default_token_lease_ttl_secs() -> u64 {
    120
}

fn default_token_min_lease() -> i64 {
    1000
}

fn default_services_port_range() -> (u16, u16) {
    (5000, 6000)
}

fn default_services_health_interval_ms() -> u64 {
    2_000
}

fn default_services_max_restart_attempts() -> u32 {
    5
}

fn default_services_restart_backoff_max_ms() -> u64 {
    60_000
}

// =============================================================================
// Rola wezla w mesh
// =============================================================================

/// Rola wezla w sieci mesh
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NodeRole {
    /// Centralny router — przyjmuje requesty, deleguje do backendow
    #[default]
    Router,
    /// Stacja robocza z lokalnym GPU — moze uruchamiac inferencje
    Desktop,
    /// Urzadzenie mobilne — lekki klient mesh
    Mobile,
}

// =============================================================================
// Konfiguracja mesh networking
// =============================================================================

/// Konfiguracja sieci mesh miedzy wezlami TentaFlow
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MeshConfig {
    /// Wlacz mesh networking
    #[serde(default)]
    pub enabled: bool,

    /// Port QUIC dla komunikacji mesh (domyslnie 8090)
    #[serde(default = "default_mesh_port")]
    pub port: u16,

    /// Statyczni peerzy (adresy QUIC do polaczenia)
    #[serde(default)]
    pub static_peers: Vec<String>,

    /// Wlacz mDNS discovery
    #[serde(default = "default_true")]
    pub mdns_enabled: bool,

    /// Wlacz BitTorrent DHT (pkarr-mainline) discovery dla peerow w internecie
    /// bez wspolnego relay. Defaultowo true. Wylacz (`false`) gdy ISP/router
    /// blokuje BitTorrent UDP traffic — wtedy mainline floodowac bedzie logi
    /// ostrzezeniami "os error 10060". mDNS (LAN) i N0 relays (WAN) dzialaja
    /// dalej bez DHT.
    #[serde(default = "default_true")]
    pub dht_enabled: bool,

    /// Interwal heartbeat QUIC w milisekundach
    #[serde(default = "default_heartbeat_interval_ms")]
    pub heartbeat_interval_ms: u64,

    /// Timeout po ktorym peer jest uznawany za dead (ms)
    #[serde(default = "default_peer_timeout_ms")]
    pub peer_timeout_ms: u64,

    /// Nazwa klastra (tylko peery z ta sama nazwa sie lacza)
    #[serde(default = "default_cluster_name")]
    pub cluster_name: String,

    /// Liczba dni bezskutecznych prob polaczenia po ktorej zaufany peer jest
    /// automatycznie odparowywany. Chroni przed "martwymi" tozsamosciami: gdy
    /// node zostaje wyczyszczony i re-provisionowany dostaje nowy klucz ed25519,
    /// a stara tozsamosc utknelaby w trusted store i meshu w nieskonczonej petli
    /// reconnectu. Peer ktory polaczyl sie w tym oknie NIGDY nie jest usuwany.
    #[serde(default = "default_trust_expiry_days")]
    pub trust_expiry_days: u64,

    /// URL serwera relay iroh uzywanego gdy bezposrednie QUIC hole punching
    /// nie jest mozliwe (NAT, firewall). Pusty string (domyslnie) oznacza
    /// uzycie wbudowanego presetu N0 iroh (4 produkcyjne regiony
    /// `*.relay.n0.iroh-canary.iroh.link`). Niepusta wartosc zastepuje preset
    /// podanym URL; override mozna tez zrobic wpisem
    /// `settings.mesh.iroh_relay_url` w DB.
    #[serde(default = "default_iroh_relay_url")]
    pub iroh_relay_url: String,
}

/// Domyslnie pusty string — iroh uzyje wbudowanego presetu N0.
fn default_iroh_relay_url() -> String {
    String::new()
}

// =============================================================================
// Konfiguracja lokalnej inferencji
// =============================================================================

/// Konfiguracja lokalnej inferencji LLM na wezle
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct InferenceConfig {
    /// Wlacz lokalna inferencje
    #[serde(default)]
    pub enabled: bool,

    /// Sciezka do katalogu z modelami
    #[serde(default = "default_models_dir")]
    pub models_dir: String,

    /// Modele do zaladowania przy starcie
    #[serde(default)]
    pub autoload_models: Vec<String>,

    /// Maksymalna ilosc GPU layers do offload
    #[serde(default)]
    pub gpu_layers: Option<u32>,

    /// Preferowany backend: "llamacpp" lub "mlx"
    #[serde(default = "default_inference_backend")]
    pub backend: String,
}

// =============================================================================
// Konfiguracja serwera
// =============================================================================

/// Konfiguracja ogolna serwera
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    /// Maksymalna liczba jednoczesnych polaczen TCP
    pub max_total_connections: usize,

    /// Maksymalna liczba rownoleglych requestow (active + queued)
    pub max_concurrent_requests: usize,

    /// Maksymalna liczba requestow w kolejce (oczekujacych na backend)
    pub max_queued_requests: usize,

    /// Liczba watkow w thread pool (0 = auto = num_cpus)
    #[serde(default)]
    pub worker_threads: usize,

    /// Czy przypinac watki do rdzeni CPU (NUMA-aware)
    #[serde(default = "default_true")]
    pub cpu_affinity: bool,

    /// Level logowania (trace, debug, info, warn, error)
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// Format logow (json lub pretty)
    #[serde(default = "default_log_format")]
    pub log_format: String,

    /// Opcjonalna konfiguracja mTLS pinning dla Service-to-Core endpointu
    /// `/core/frame/pickup`. Domyslnie wylaczona (F1a/F1b compat) — production
    /// deploy powinien wlaczyc `pickup_required = true` i wpisac fingerprinty.
    #[serde(default)]
    pub mtls: Option<MtlsConfig>,
}

/// Pinning client certificates for HTTP REST tier endpoints. SHA-256
/// fingerprints of DER leaf certs (lower-case hex, with or without colons).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct MtlsConfig {
    /// Gdy `true`, rustls request client cert na handshake'u i `/core/frame/pickup`
    /// odrzuca polaczenia bez pasujacego fingerprintu.
    #[serde(default)]
    pub pickup_required: bool,
    /// Lista SHA-256 fingerprintow dozwolonych client certow.
    #[serde(default)]
    pub client_cert_fingerprints: Vec<String>,
}

// =============================================================================
// Konfiguracja protokolow
// =============================================================================

/// Konfiguracja wszystkich protokolow
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProtocolsConfig {
    /// OpenAI API (REST + SSE)
    pub openai_api: ProtocolConfig,

    /// gRPC (NVIDIA NIM compatible) - opcjonalne w Fazie 0
    #[serde(default)]
    pub grpc: Option<ProtocolConfig>,

    /// QUIC + CBOR - opcjonalne w Fazie 0
    #[serde(default)]
    pub quic: Option<QuicProtocolConfig>,
}

/// Konfiguracja pojedynczego protokolu (OpenAI API lub gRPC)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProtocolConfig {
    /// Czy protokol jest wlaczony
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Adres i port nasluchiwania (np. "0.0.0.0:8080")
    pub bind: String,

    /// Sciezka do certyfikatu TLS (opcjonalne - dla testow lokalnych mozna pominac)
    #[serde(default)]
    pub tls_cert: Option<String>,

    /// Sciezka do klucza TLS (opcjonalne - dla testow lokalnych mozna pominac)
    #[serde(default)]
    pub tls_key: Option<String>,

    /// Maksymalna liczba polaczen dla tego protokolu
    pub max_connections: usize,

    /// Timeout na request (milisekundy)
    pub request_timeout_ms: u64,

    /// Max rozmiar body (bajty) - dla OpenAI API
    #[serde(default = "default_body_limit")]
    pub body_limit_bytes: usize,

    /// Opcjonalny CA dla mTLS (client authentication)
    #[serde(default)]
    pub mtls_client_ca: Option<String>,
}

/// Konfiguracja protokolu QUIC (rozszerzenie ProtocolConfig)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct QuicProtocolConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub bind: String,
    #[serde(default)]
    pub tls_cert: Option<String>,
    #[serde(default)]
    pub tls_key: Option<String>,
    pub max_connections: usize,

    /// Max liczba streamow per connection (QUIC multiplexing)
    #[serde(default = "default_quic_streams")]
    pub max_streams_per_connection: usize,

    /// Idle timeout dla polaczenia QUIC (ms)
    #[serde(default = "default_quic_idle_timeout")]
    pub idle_timeout_ms: u64,
}

// =============================================================================
// Middleware i rate limiting
// =============================================================================

/// Konfiguracja middleware. Po Krok 6 zostaje tylko rate-limit + audit
/// — request/response filtering przeszło do flow_engine (`pii_filter`
/// node), nie ma już osobnych knobów.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MiddlewareConfig {
    /// Czy rate limiting jest wlaczony
    #[serde(default = "default_true")]
    pub rate_limiting_enabled: bool,

    /// Czy audit logging jest wlaczony
    #[serde(default = "default_true")]
    pub audit_logging_enabled: bool,
}

/// Konfiguracja rate limiting
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RateLimitingConfig {
    /// Domyslny limit requestow per sekunda (per API key)
    #[serde(default = "default_rate_limit_rps")]
    pub default_requests_per_second: u32,

    /// Burst capacity (token bucket)
    #[serde(default = "default_rate_limit_burst")]
    pub default_burst: u32,
}

// =============================================================================
// Load balancing
// =============================================================================

/// Konfiguracja load balancingu
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LoadBalancingConfig {
    /// Interwal health checkow (ms)
    #[serde(default = "default_health_check_interval")]
    pub health_check_interval_ms: u64,

    /// Timeout na health check (ms)
    #[serde(default = "default_health_check_timeout")]
    pub health_check_timeout_ms: u64,

    /// Ile failed health checks zanim backend zostanie oznaczony jako unhealthy
    #[serde(default = "default_unhealthy_threshold")]
    pub unhealthy_threshold: u32,

    /// Ile successful health checks zanim backend zostanie oznaczony jako healthy
    #[serde(default = "default_healthy_threshold")]
    pub healthy_threshold: u32,

    /// Max czas oczekiwania w kolejce (ms)
    #[serde(default = "default_queue_timeout")]
    pub queue_timeout_ms: u64,

    /// Czy circuit breaker jest wlaczony
    #[serde(default = "default_true")]
    pub circuit_breaker_enabled: bool,

    /// Prog bledow dla circuit breaker (ile bledow -> OPEN)
    #[serde(default = "default_circuit_breaker_threshold")]
    pub circuit_breaker_threshold: u32,

    /// Czas w stanie OPEN przed przejsciem do HALF_OPEN (ms)
    #[serde(default = "default_circuit_breaker_timeout")]
    pub circuit_breaker_timeout_ms: u64,
}

// =============================================================================
// Runtime types reused przez warstwe transport_client / backend client
// =============================================================================

/// Typ polaczenia do backendu AI
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ConnectionType {
    /// OpenAI API compatible (HTTP/HTTPS REST API)
    #[serde(rename = "openai_api")]
    OpenAIApi {
        /// URL backendu (np. https://api.openai.com/v1)
        url: String,

        /// API key bezposrednio (opcjonalny, ma priorytet nad api_key_env)
        #[serde(default)]
        api_key: Option<String>,

        /// Zmienna srodowiskowa z API key
        #[serde(default)]
        api_key_env: Option<String>,

        /// Custom HTTP headers (dla specjalnych API jak Anthropic)
        #[serde(default)]
        extra_headers: Vec<(String, String)>,

        /// Custom endpoint path (np. "/infer" dla PaddleOCR, "/audio/speech" dla TTS)
        #[serde(default)]
        custom_endpoint: Option<String>,

        /// Request format transformation ("openai", "paddleocr", etc.)
        #[serde(default)]
        request_format: Option<String>,

        /// Dodatkowe parametry dla TTS (voice, model, speed, format)
        #[serde(default)]
        tts_config: Option<TTSParameters>,
    },

    /// QUIC connection (dla TentaFlow.Embeddings, TentaFlow.TTS)
    QUIC {
        /// QUIC URL (quic://host:port)
        quic_url: String,

        /// CA cert dla weryfikacji serwera (opcjonalne - jesli None, uzywa systemowych CA)
        #[serde(default)]
        tls_ca: Option<String>,

        /// Auto-reconnect po utracie polaczenia
        #[serde(default = "default_true")]
        auto_reconnect: bool,

        /// Interwal reconnect (ms)
        #[serde(default = "default_reconnect_interval")]
        reconnect_interval_ms: u64,

        /// Keepalive interval (ms)
        #[serde(default = "default_keepalive")]
        keepalive_interval_ms: u64,

        /// Dodatkowe parametry dla TTS (voice, speed) - opcjonalne
        #[serde(default)]
        tts_config: Option<TTSParameters>,
    },
}

/// Parametry specyficzne dla TTS (uzywane w ConnectionType::OpenAIApi)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TTSParameters {
    /// Model TTS: "tts-1" (szybki) lub "tts-1-hd" (wysoka jakosc)
    #[serde(default = "default_tts_model")]
    pub model: String,

    /// Glos: "alloy", "echo", "fable", "onyx", "nova", "shimmer"
    #[serde(default = "default_tts_voice")]
    pub voice: String,

    /// Format audio: "opus", "mp3", "aac", "flac", "wav", "pcm"
    #[serde(default = "default_tts_format")]
    pub response_format: String,

    /// Predkosc mowy (0.25-4.0)
    #[serde(default = "default_tts_speed")]
    pub speed: f32,
}

/// Pojedynczy backend w ramach serwisu (dla load balancing)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServiceBackend {
    /// Typ i parametry polaczenia
    #[serde(flatten)]
    pub connection: ConnectionType,

    /// Max rownoczesnych requestow dla tego backendu
    pub max_concurrent: usize,

    /// Timeout dla requestow do tego backendu (ms)
    pub timeout_ms: u64,

    /// Waga dla weighted load balancing
    #[serde(default = "default_weight")]
    pub weight: u32,

    /// Override nazwy modelu dla tego backendu (dla LLM)
    #[serde(default)]
    pub model_name_override: Option<String>,

    /// Custom health check path (opcjonalny)
    #[serde(default)]
    pub health_check_path: Option<String>,
}

/// Kategoria modelu LLM (dla KV Cache / Prefix Caching)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LlmModelCategory {
    /// Glowny LLM (bielik-11b) — odpowiedzi uzytkownikowi
    #[default]
    Main,
    /// Analyzer LLM (bielik-1.5b) — analiza dla Memory, tools
    Analyzer,
}

// =============================================================================
// Monitoring
// =============================================================================

/// Konfiguracja monitoringu
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MonitoringConfig {
    /// Czy health check endpoint jest wlaczony
    #[serde(default = "default_true")]
    pub health_check_enabled: bool,

    /// Adres dla health check endpoint
    #[serde(default = "default_health_bind")]
    pub health_check_bind: String,

    /// Sciezka dla health check
    #[serde(default = "default_health_path")]
    pub health_check_path: String,

    /// Czy OpenTelemetry tracing jest wlaczony
    #[serde(default)]
    pub tracing_enabled: bool,

    /// Endpoint dla tracingu (opcjonalny)
    #[serde(default)]
    pub tracing_endpoint: Option<String>,
}

// =============================================================================
// Memory management i security
// =============================================================================

/// Konfiguracja memory management (opcjonalne, dla Fazy 3)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MemoryConfig {
    pub total_ram_percentage: u8,
    pub connection_pool_percentage: u8,
    pub request_buffers_percentage: u8,
    pub response_cache_percentage: u8,
    pub other_percentage: u8,
    pub max_request_buffer_kb: usize,
    pub max_response_buffer_kb: usize,
}

/// Konfiguracja security (opcjonalne)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SecurityConfig {
    /// CORS enabled
    #[serde(default = "default_true")]
    pub cors_enabled: bool,

    /// CORS allowed origins
    #[serde(default)]
    pub cors_allowed_origins: Vec<String>,

    /// CORS allowed methods
    #[serde(default)]
    pub cors_allowed_methods: Vec<String>,

    /// CORS allowed headers
    #[serde(default)]
    pub cors_allowed_headers: Vec<String>,
}

// =============================================================================
// Wartosci domyslne — funkcje dla serde
// =============================================================================

fn default_true() -> bool {
    true
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_log_format() -> String {
    "json".to_string()
}

fn default_body_limit() -> usize {
    1_048_576 // 1 MB
}

fn default_quic_streams() -> usize {
    100
}

fn default_quic_idle_timeout() -> u64 {
    30_000 // 30 sekund
}

fn default_rate_limit_rps() -> u32 {
    100
}

fn default_rate_limit_burst() -> u32 {
    200
}

fn default_health_check_interval() -> u64 {
    5_000 // 5 sekund
}

fn default_health_check_timeout() -> u64 {
    2_000 // 2 sekundy
}

fn default_unhealthy_threshold() -> u32 {
    3
}

fn default_healthy_threshold() -> u32 {
    2
}

fn default_queue_timeout() -> u64 {
    30_000 // 30 sekund
}

fn default_circuit_breaker_threshold() -> u32 {
    5
}

fn default_circuit_breaker_timeout() -> u64 {
    60_000 // 60 sekund
}

fn default_weight() -> u32 {
    1
}

fn default_health_bind() -> String {
    "0.0.0.0:8888".to_string()
}

fn default_health_path() -> String {
    "/health".to_string()
}

fn default_reconnect_interval() -> u64 {
    5_000 // 5 sekund
}

fn default_keepalive() -> u64 {
    10_000 // 10 sekund
}

fn default_tts_model() -> String {
    "tts-1".to_string()
}

fn default_tts_voice() -> String {
    "alloy".to_string()
}

fn default_tts_format() -> String {
    "opus".to_string()
}

fn default_tts_speed() -> f32 {
    1.0
}

fn default_mesh_port() -> u16 {
    8090
}

fn default_heartbeat_interval_ms() -> u64 {
    500
}

fn default_peer_timeout_ms() -> u64 {
    3000
}

fn default_cluster_name() -> String {
    "tentaflow".to_string()
}

fn default_trust_expiry_days() -> u64 {
    30
}

fn default_models_dir() -> String {
    // Portable layout: shared models cache under <tentaflow_home>/models so
    // every backend (Docker, native venv, in-process) hits the same HF cache.
    crate::paths::models_root().to_string_lossy().into_owned()
}

fn default_inference_backend() -> String {
    "llamacpp".to_string()
}

// =============================================================================
// Implementacja — metody NodeConfig
// =============================================================================

impl NodeConfig {
    /// Wczytuje konfiguracje z pliku TOML.
    ///
    /// Algorytm:
    /// 1. Wczytaj plik jako String
    /// 2. Parsuj TOML -> NodeConfig
    /// 3. Zwaliduj wszystkie wartosci
    /// 4. Zwroc zwalidowana konfiguracje
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path).map_err(|e| CoreError::ConfigError {
            message: format!("Nie mozna wczytac pliku konfiguracji: {:?}", path),
            source: e.into(),
        })?;

        let config: NodeConfig = toml::from_str(&content).map_err(|e| CoreError::ConfigError {
            message: "Blad parsowania TOML".to_string(),
            source: e.into(),
        })?;

        config.validate()?;
        Ok(config)
    }

    /// Waliduje poprawnosc wszystkich wartosci w konfiguracji.
    fn validate(&self) -> Result<()> {
        if self.server.max_total_connections == 0 {
            return Err(CoreError::ConfigError {
                message: "max_total_connections musi byc > 0".to_string(),
                source: anyhow::anyhow!("Niepoprawna wartosc: 0"),
            }
            .into());
        }

        if self.protocols.openai_api.enabled {
            self.validate_protocol_config(&self.protocols.openai_api, "openai_api")?;
        }

        // Walidacja mesh config jesli obecna
        if let Some(ref mesh) = self.mesh {
            if mesh.enabled && mesh.port == 0 {
                return Err(CoreError::ConfigError {
                    message: "mesh.port musi byc > 0 gdy mesh jest wlaczony".to_string(),
                    source: anyhow::anyhow!("Niepoprawna wartosc portu: 0"),
                }
                .into());
            }
        }

        Ok(())
    }

    /// Waliduje konfiguracje pojedynczego protokolu
    fn validate_protocol_config(&self, config: &ProtocolConfig, protocol_name: &str) -> Result<()> {
        if !config.bind.contains(':') {
            return Err(CoreError::ConfigError {
                message: format!(
                    "Niepoprawny bind address dla {}: '{}'",
                    protocol_name, config.bind
                ),
                source: anyhow::anyhow!("Oczekiwano formatu 'host:port'"),
            }
            .into());
        }

        Ok(())
    }

    /// Serializuje konfiguracje do formatu TOML.
    pub fn to_toml_string(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|e| {
            CoreError::ConfigError {
                message: "Blad serializacji konfiguracji do TOML".to_string(),
                source: e.into(),
            }
            .into()
        })
    }
}

// =============================================================================
// Implementacje Default
// =============================================================================

impl Default for MiddlewareConfig {
    fn default() -> Self {
        Self {
            rate_limiting_enabled: true,
            audit_logging_enabled: true,
        }
    }
}

impl Default for RateLimitingConfig {
    fn default() -> Self {
        Self {
            default_requests_per_second: default_rate_limit_rps(),
            default_burst: default_rate_limit_burst(),
        }
    }
}

impl Default for LoadBalancingConfig {
    fn default() -> Self {
        Self {
            health_check_interval_ms: default_health_check_interval(),
            health_check_timeout_ms: default_health_check_timeout(),
            unhealthy_threshold: default_unhealthy_threshold(),
            healthy_threshold: default_healthy_threshold(),
            queue_timeout_ms: default_queue_timeout(),
            circuit_breaker_enabled: true,
            circuit_breaker_threshold: default_circuit_breaker_threshold(),
            circuit_breaker_timeout_ms: default_circuit_breaker_timeout(),
        }
    }
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            server: ServerConfig {
                max_total_connections: 1000,
                max_concurrent_requests: 100,
                max_queued_requests: 50,
                worker_threads: 0,
                cpu_affinity: true,
                log_level: "info".to_string(),
                log_format: "json".to_string(),
                mtls: None,
            },
            protocols: ProtocolsConfig {
                openai_api: ProtocolConfig {
                    enabled: true,
                    bind: "0.0.0.0:8090".to_string(),
                    tls_cert: None,
                    tls_key: None,
                    max_connections: 500,
                    request_timeout_ms: 120_000,
                    body_limit_bytes: 1_048_576,
                    mtls_client_ca: None,
                },
                grpc: None,
                quic: Some(QuicProtocolConfig {
                    enabled: true,
                    bind: "0.0.0.0:8090".to_string(),
                    tls_cert: None,
                    tls_key: None,
                    max_connections: 100,
                    max_streams_per_connection: 100,
                    idle_timeout_ms: 30_000,
                }),
            },
            middleware: MiddlewareConfig::default(),
            rate_limiting: RateLimitingConfig::default(),
            load_balancing: LoadBalancingConfig::default(),
            monitoring: MonitoringConfig::default(),
            memory: None,
            security: None,
            node_role: NodeRole::default(),
            mesh: Some(MeshConfig {
                enabled: true,
                port: 8090,
                static_peers: vec![],
                mdns_enabled: true,
                dht_enabled: true,
                heartbeat_interval_ms: default_heartbeat_interval_ms(),
                peer_timeout_ms: default_peer_timeout_ms(),
                cluster_name: "tentaflow".to_string(),
                iroh_relay_url: default_iroh_relay_url(),
                trust_expiry_days: default_trust_expiry_days(),
            }),
            inference: None,
            services_runtime: ServicesRuntimeConfig::default(),
            token_metrics: TokenMetricsConfig::default(),
            vision: VisionConfig::default(),
        }
    }
}

impl Default for MonitoringConfig {
    fn default() -> Self {
        Self {
            health_check_enabled: true,
            health_check_bind: default_health_bind(),
            health_check_path: default_health_path(),
            tracing_enabled: false,
            tracing_endpoint: None,
        }
    }
}

#[cfg(test)]
mod vision_config_tests {
    use super::*;

    /// An absent `[vision]` section must yield exactly `VisionConfig::default()`
    /// — the serde field defaults and the manual `Default` impl share the same
    /// default fns, so a node without the section keeps today's behavior.
    #[test]
    fn absent_vision_section_equals_default() {
        let parsed: VisionConfig = toml::from_str("").expect("empty [vision] parses");
        let default = VisionConfig::default();
        assert_eq!(
            serde_json::to_value(&parsed).unwrap(),
            serde_json::to_value(&default).unwrap()
        );
    }

    /// The default config file written on first boot must serialize cleanly
    /// with the extended `[vision]` section (Option fields skipped by TOML) and
    /// survive a JSON round-trip — the supervisor hands the section to vision
    /// workers as `--vision-config` JSON.
    #[test]
    fn vision_config_serializes_to_toml_and_json() {
        let toml_str = NodeConfig::default()
            .to_toml_string()
            .expect("default config serializes");
        assert!(toml_str.contains("[vision]"));

        let json = serde_json::to_string(&VisionConfig::default()).expect("to json");
        let back: VisionConfig = serde_json::from_str(&json).expect("from json");
        assert_eq!(
            serde_json::to_value(&back).unwrap(),
            serde_json::to_value(&VisionConfig::default()).unwrap()
        );
    }
}

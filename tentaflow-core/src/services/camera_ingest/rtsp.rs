// =============================================================================
// File: services/camera_ingest/rtsp.rs — RTSP camera connector (F1b P1.B)
// =============================================================================
//
// GStreamer pipeline:
//   rtspsrc location=<url> ! rtph264depay ! h264parse ! avdec_h264 !
//   videoconvert ! video/x-raw,format=RGB ! appsink
//
// Decoded RGB24 frames flow through the same `FrameMailbox` + `FrameStorage` +
// `StreamingBus` plumbing as the fakefile path. On bus Error / Eos the
// session-level supervisor tears the pipeline down and reconnects with
// exponential backoff (capped) and ±20% jitter — internal rtspsrc retry is
// disabled so we control the policy at one layer only.

use std::sync::Arc;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use rand::RngExt;
use regex::Regex;
use std::sync::OnceLock;
use tokio::sync::{mpsc, watch};

use super::credentials::{credentials_cipher, overlay_credentials};
use super::decoder_detect::{
    cuda_scale_available, detect_profile, gpu_resident_available, nvdec_decode_available, HwDecoder,
};
use super::error::{CameraIngestError, Result};
use super::fakefile::{
    ensure_gst_initialized, install_detect_frame_callback, install_detect_frame_callback_nv12,
    install_frame_callback_nv12, FrameCounters, FrameMailbox, LatestFrame, DETECT_RESIZE_DIM,
};
use super::session::{
    CameraConfig, CameraHealth, CameraStatus, PixelFormat, SessionCommand, SnapshotData,
};
use super::stream_publisher::Mp4StreamPublisher;
use crate::services::frame_storage::{FrameMetadata, FramePixelFormat, StoredFrame};
use crate::services::{frame_storage, streaming_bus};

/// Reconnection policy for RTSP sessions. Backoff is multiplied by 2 each
/// attempt, capped at `max_backoff`. Jitter is applied as a symmetric
/// fraction of the current backoff (so e.g. 1s ±20% → 800-1200ms).
#[derive(Debug, Clone)]
pub struct ReconnectPolicy {
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub jitter_pct: f64,
    pub max_attempts: Option<u32>,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
            jitter_pct: 0.20,
            max_attempts: None,
        }
    }
}

/// Replace `user:password` credentials in an RTSP URL with `***:***`.
/// Operates on the canonical `rtsp[s]://[user[:pass]@]host[:port]/path` form
/// and is safe to call on already-redacted or scheme-less strings (those are
/// returned unchanged or passed through the regex-based fallback).
pub fn redact_rtsp_url(url: &str) -> String {
    if let Some(scheme_end) = url.find("://") {
        let after_scheme = &url[scheme_end + 3..];
        if let Some(at_pos) = after_scheme.find('@') {
            let host_part = &after_scheme[at_pos..];
            return format!("{}://***:***{}", &url[..scheme_end], host_part);
        }
    }
    url.to_string()
}

/// Redact any RTSP/HTTP credentials embedded inside a free-form string (e.g.
/// a GStreamer error message that quoted the original location). Anchored on
/// `rtsp://`, `rtsps://`, `http://` or `https://` followed by anything up to
/// `@` — HTTP(S) covers the MJPEG connector's souphttpsrc location.
pub fn redact_url_in_text(text: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(rtsps?|https?)://[^@\s/]+@").expect("redact regex must compile")
    });
    re.replace_all(text, "$1://***:***@").into_owned()
}

/// Compute the next sleep duration before a reconnect attempt. Pure function
/// so unit tests can pin behavior without a running session. The jitter draw
/// uses `rng` so callers may pass a seeded RNG for deterministic tests.
pub fn compute_backoff_with_jitter<R: rand::Rng + ?Sized>(
    base: Duration,
    jitter_pct: f64,
    rng: &mut R,
) -> Duration {
    if base.is_zero() {
        return Duration::from_millis(0);
    }
    let base_ms = base.as_millis() as i64;
    // Symmetric draw in [-jitter_pct, +jitter_pct]. f64 → i64 ms is safe for
    // the policy bounds we permit (max_backoff 60s ⇒ 60_000 ms).
    let span = (base_ms as f64) * jitter_pct;
    let draw: f64 = rng.random_range(-span..=span);
    let out_ms = (base_ms as f64 + draw).round() as i64;
    // Floor at 100ms so we never busy-loop on misconfiguration.
    Duration::from_millis(out_ms.max(100) as u64)
}

fn next_backoff(current: Duration, max: Duration) -> Duration {
    let doubled = current.saturating_mul(2);
    if doubled > max {
        max
    } else {
        doubled
    }
}

/// Validate an RTSP URL well enough to reject obvious garbage before we hand
/// the string to GStreamer. We do NOT canonicalize or parse credentials here.
pub fn validate_rtsp_url(url: &str) -> Result<()> {
    if url.is_empty() {
        return Err(CameraIngestError::InvalidUrl("empty".into()));
    }
    // Accept rtsp:// and rtsps:// (TLS) — both are routed through rtspsrc.
    if !(url.starts_with("rtsp://") || url.starts_with("rtsps://")) {
        return Err(CameraIngestError::InvalidUrl(format!(
            "missing rtsp:// or rtsps:// scheme: {}",
            redact_rtsp_url(url)
        )));
    }
    // After the scheme there must be at least one host character.
    let after_scheme = url
        .strip_prefix("rtsp://")
        .or_else(|| url.strip_prefix("rtsps://"))
        .unwrap_or("");
    if after_scheme.is_empty() {
        return Err(CameraIngestError::InvalidUrl(format!(
            "missing host: {}",
            redact_rtsp_url(url)
        )));
    }
    Ok(())
}

/// Pipeline plus the handles the session needs to attach an on-demand
/// fMP4 mux branch (Branch B). `tee` is the always-present RTP fan-out and
/// `rtp_filter_src_pad` is its sink-side reference — both are kept alive
/// for the lifetime of the pipeline so the attach helper can request a new
/// source pad and link a fresh mux branch without rebuilding from scratch.
pub struct RtspPipelineHandles {
    pub pipeline: gst::Pipeline,
    pub tee: gst::Element,
    /// Post-`cudadownload` decode tee (raw NV12 in host memory) on the
    /// GPU-resident NVDEC path — the attach point for the on-demand RGB branch
    /// (Stage 3). `None` on every other path (crops are already RGB, so no
    /// on-demand convert is needed).
    ///
    /// On the zero-copy CROPS path this is instead the CUDA-memory tee
    /// (`tee_cuda`, BEFORE `cudadownload`): the on-demand RGB branch then carries
    /// its OWN `cudadownload` so the 4K download runs ONLY while a viewer watches.
    /// [`RtspPipelineHandles::decode_tee_is_cuda`] tells the two apart.
    pub decode_tee: Option<gst::Element>,
    /// `true` when `decode_tee` is the CUDA-memory tee (zero-copy crops): the
    /// on-demand RGB attach must insert `cudadownload` itself
    /// ([`attach_rgb_branch_cuda`]) rather than start from host NV12
    /// ([`attach_rgb_branch`]).
    pub decode_tee_is_cuda: bool,
}

/// Którą ścieżką dekodowania budujemy pipeline dla bieżącej próby. `Cpu` to
/// zawsze działający fallback (decodebin → videoconvert). `GpuResidentNvidia`
/// to wariant NVIDIA, w którym dekoding I konwersja kolorów dzieją się na GPU
/// (nvhXdec → cudaconvert → cudadownload), a na CPU schodzi dopiero pełna
/// klatka RGB. `NvdecCpuConvert` to wariant pośredni: sam DEKOD schodzi na GPU
/// (nvhXdec → cudadownload), a konwersja kolorów NV12→RGB zostaje na CPU
/// (videoconvert) — działa na nvcodec bez `cudaconvert`/`cudascale` (GStreamer
/// 1.24), gdzie pełny GPU-resident jest niedostępny, a mimo to zdejmuje z CPU
/// najdroższy koszt (programowy dekod 4K H.264). Wybór i kaskadę fallbacków
/// (GPU-resident → NvdecCpuConvert → CPU) obsługuje `run_rtsp_session`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IngestPath {
    GpuResidentNvidia,
    /// GPU-resident detect: NVDEC decode → `cudadownload` → tee into a raw-NV12
    /// DETECT appsink (detector does YUV→RGB + resize on the GPU) plus the
    /// existing NV12→RGB `videoconvert` crops/display branch. Kills the
    /// detector's CPU resize; the crops `videoconvert` still runs this stage
    /// (removed in Stage 3). Requires NVDEC + the ort GPU detect features.
    NvdecNv12,
    NvdecCpuConvert,
    Cpu,
}

impl IngestPath {
    /// Krótka etykieta do logów — by od razu było widać, którą ścieżką poszedł
    /// ingest danej kamery.
    fn label(self) -> &'static str {
        match self {
            IngestPath::GpuResidentNvidia => "GPU-resident CUDA",
            IngestPath::NvdecNv12 => "NVDEC + GPU NV12 detect",
            IngestPath::NvdecCpuConvert => "NVDEC + CPU convert",
            IngestPath::Cpu => "CPU decode",
        }
    }
}

/// Whether the GPU-resident NV12 detect path is usable: the ort GPU detect
/// features must be compiled (`detect_batch_gpu` exists to consume the raw NV12
/// frame) and the operator must not have disabled it via
/// `[vision] nv12_detect = false`. When false, NVDEC ingest uses the
/// CPU-convert path (the detector then reads an RGB frame). Decode availability
/// is checked separately (`nvdec_decode_available`).
fn nv12_gpu_detect_available() -> bool {
    crate::vision::settings::get().nv12_detect
        && cfg!(all(
            feature = "inference-vision-gpu",
            feature = "inference-supertonic"
        ))
}

/// Next ingest path to try after a hardware path fails to build/negotiate. The
/// NV12 detect path degrades to the deployed NVDEC+CPU-convert path first (keeps
/// GPU decode, drops only the GPU detect); every other hardware path degrades
/// straight to CPU — the historical "always works" floor. A camera never goes
/// dark.
fn degrade_ingest_path(p: IngestPath) -> IngestPath {
    match p {
        IngestPath::NvdecNv12 => IngestPath::NvdecCpuConvert,
        _ => IngestPath::Cpu,
    }
}

/// Rozstrzyga, czy NA STARCIE próbować dekodowania sprzętowego dla tej kamery.
/// `decoder_override` z konfiguracji ma pierwszeństwo nad auto-detekcją:
///   * `Some(Software)` → wymuś CPU (nigdy nie próbuj HW),
///   * `Some(hw)`       → wymuś próbę HW (operator wie lepiej),
///   * `None`           → auto: HW gdy profil sprzętowy go preferuje.
/// Zwraca `true`, gdy pierwsza próba ma użyć dekodera sprzętowego. Fallback na
/// CPU po nieudanej negocjacji obsługuje `run_rtsp_session` (stąd „pierwsza próba").
fn resolve_use_hw_decode(config: &CameraConfig) -> bool {
    match config.decoder_override {
        Some(HwDecoder::Software) => false,
        Some(hw) => hw.is_hardware(),
        None => detect_profile().prefer_hw,
    }
}

/// Rozstrzyga ścieżkę ingestu NA STARCIE sesji, gdy operator nie wymusił
/// dekodera programowego (`decoder_override = Some(Software)` → zawsze CPU).
/// Preferencja od najwydajniejszej ścieżki w dół:
///   1. `GpuResidentNvidia` — pełny łańcuch CUDA (`gpu_resident_available`:
///      wymaga `cudaconvert`, obecny dopiero w nvcodec ≥1.26); dekod I
///      konwersja kolorów na GPU.
///   2. `NvdecCpuConvert` — NVDEC + `cudadownload` (`nvdec_decode_available`,
///      bez `cudaconvert`/`cudascale`); dekod na GPU, konwersja NV12→RGB na CPU.
///      To zdejmuje z CPU programowy dekod 4K H.264 tam, gdzie pełny
///      GPU-resident jest niedostępny (GStreamer 1.24).
///   3. `Cpu` — decodebin → videoconvert; działa na każdej platformie.
/// Kaskadę fallbacków w runtime (wariant nie zbuduje się / nie znegocjuje)
/// obsługuje `run_rtsp_session`, stąd „na starcie".
fn resolve_ingest_path(config: &CameraConfig) -> IngestPath {
    let forced_software = matches!(config.decoder_override, Some(HwDecoder::Software));
    if forced_software {
        IngestPath::Cpu
    } else if gpu_resident_available() {
        IngestPath::GpuResidentNvidia
    } else if nvdec_decode_available() {
        // Prefer the GPU-resident NV12 detect path when the ort GPU features are
        // built (detector consumes raw NV12); else the deployed CPU-convert
        // NVDEC path. Runtime fallback (build/negotiation failure) steps
        // NvdecNv12 → NvdecCpuConvert → Cpu via `degrade_ingest_path`.
        if nv12_gpu_detect_available() {
            IngestPath::NvdecNv12
        } else {
            IngestPath::NvdecCpuConvert
        }
    } else {
        IngestPath::Cpu
    }
}

/// Buduje i konfiguruje `rtspsrc` — wspólny początek obu wariantów pipeline'u
/// (CPU i GPU-resident). Czyści `?enableSrtp`, dobiera maskę `protocols`
/// (TLS dla rtsps://), wyłącza wewnętrzny retry (reconnect zarządzamy na
/// poziomie sesji) i filtruje strumienie do samego wideo przez `select-stream`.
/// Zwraca gotowy element — dynamic linking pada wideo robi
/// `connect_rtspsrc_video_pad`.
fn build_rtspsrc(url: &str, timeout_secs: u32) -> Result<gst::Element> {
    // Strip vendor-specific query parameters that ask the server to wrap RTP
    // in SRTP/SRTCP (e.g. UniFi Protect `?enableSrtp`). Our pipeline only
    // handles plain RTP — rtspsrc honors the cipher suite advertised in SDP
    // `a=crypto` but cannot decrypt SRTP without an explicit srtpdec branch.
    // rtsps:// already gives transport-layer TLS encryption, which is what
    // matters for confidentiality on the camera link. Leaving `?enableSrtp`
    // in the URL caused UniFi to wrap every RTP packet in SRTP, which the
    // downstream rtph264depay/h264parse could not parse — pipeline failed
    // with `not-negotiated (-4)` immediately after PLAY.
    let url_owned: String;
    let url: &str = if let Some(stripped) = url.split_once("?enableSrtp") {
        url_owned = format!("{}{}", stripped.0, stripped.1.trim_start_matches('&'));
        let trimmed = url_owned.trim_end_matches(|c| c == '?' || c == '&');
        tracing::info!("rtsp: stripped ?enableSrtp from URL");
        trimmed
    } else {
        url
    };

    // `protocols` is GstRTSPLowerTrans (GFlags) — can't be set as raw u32.
    // We pass through stringified flags which gst-rs parses via GFlags::from_str:
    //   - rtsp://  -> `[vision] rtsp_protocols`, default "tcp" (interleaved).
    //     Across routed networks UDP media dies SILENTLY (conntrack/NAT idle
    //     timeouts) while the RTSP control TCP stays healthy — the session then
    //     sat "ONLINE" with a black tile. Interleaved TCP cannot die silently;
    //     the mid-session stall watchdog remains as defense in depth.
    //   - rtsps:// -> "tcp+tls" (TLS over TCP; udp-over-tls is rare)
    // Without `tls` in the mask, rtspsrc would silently fail on rtsps:// URLs.
    let is_tls = url.starts_with("rtsps://");
    let configured = crate::vision::settings::get().rtsp_protocols.clone();
    let protocols_str = if is_tls { "tcp+tls".to_string() } else { configured };

    let rtspsrc = gst::ElementFactory::make("rtspsrc")
        .property("location", url)
        .property("latency", 200u32)
        // rtspsrc timeout is in microseconds (GstClockTimeDiff).
        .property("timeout", (timeout_secs as u64).saturating_mul(1_000_000))
        // Disable rtspsrc's internal retry — we manage reconnect at session level.
        .property("retry", 0u32)
        .property_from_str("protocols", protocols_str.as_str())
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("rtspsrc: {e}")))?;

    // Self-signed certs are common in surveillance NVRs (UniFi Protect,
    // Hikvision, Dahua). `tls-validation-flags` is GTlsCertificateFlags
    // (GFlags); empty string parses to 0 = no validation. Operator-installed
    // cameras live on trusted LAN segments and `[[network_rule]]` ACL (when
    // wired) gates which hosts can be reached anyway. Documented trade-off:
    // an on-path attacker between tentaflow and the NVR could MITM the
    // RTSPS session. Acceptable for typical surveillance deployments where
    // the camera link is L2.
    if is_tls {
        // Empty string panics in gst-rs GFlags parser; use the canonical
        // GTlsCertificateFlags value name `no-flags` (matches the 0x0 entry
        // shown by `gst-inspect-1.0 rtspsrc | grep tls`).
        rtspsrc.set_property_from_str("tls-validation-flags", "no-flags");
    }

    // `select-stream` is emitted PRE-SETUP for every stream advertised in the
    // server SDP. Returning false tells rtspsrc to skip subscribing entirely
    // for that stream — no RTP/RTCP traffic, no pad creation downstream. We
    // accept video only. Without this filter, rtspsrc creates dynamic pads
    // for audio streams (AAC + OPUS on UniFi Protect), nothing is linked to
    // those pads, and the next sample push fails with `not-linked (-1)` →
    // `Internal data stream error` → pipeline restart loop.
    rtspsrc.connect("select-stream", false, |values| {
        let caps = match values.get(2).and_then(|v| v.get::<gst::Caps>().ok()) {
            Some(c) => c,
            None => return Some(true.to_value()),
        };
        let media = caps
            .structure(0)
            .and_then(|s| s.get::<String>("media").ok())
            .unwrap_or_default();
        let include = media == "video";
        if !include {
            tracing::info!("rtsp: select-stream skipping {} stream", media);
        }
        Some(include.to_value())
    });
    tracing::info!(
        "rtsp: built rtspsrc url_scheme={} protocols={}",
        if is_tls { "rtsps" } else { "rtsp" },
        protocols_str
    );
    Ok(rtspsrc)
}

/// Podłącza dynamiczny pad wideo z `rtspsrc` do statycznego `sink` elementu
/// `target` (w obu wariantach jest to RTP capsfilter przed `tee`). rtspsrc
/// emituje `pad-added`, zanim caps RTP są znegocjowane, więc próbujemy zlinkować
/// od razu, a gdy caps jeszcze nie ma — dowieszamy jednorazowy watcher
/// `notify::caps`. Linkujemy tylko pad `media=video`. Wspólne dla CPU i
/// GPU-resident, bo oba mają identyczny front RTP.
fn connect_rtspsrc_video_pad(rtspsrc: &gst::Element, target: &gst::Element) {
    let depay_weak = target.downgrade();
    let try_link =
        std::sync::Arc::new(move |src_pad: &gst::Pad| -> std::ops::ControlFlow<(), ()> {
            // ControlFlow::Break = handled (linked, skipped, or impossible) — no
            // need to keep watching. ControlFlow::Continue = caps not yet known.
            let Some(depay) = depay_weak.upgrade() else {
                return std::ops::ControlFlow::Break(());
            };
            let Some(sink_pad) = depay.static_pad("sink") else {
                return std::ops::ControlFlow::Break(());
            };
            if sink_pad.is_linked() {
                return std::ops::ControlFlow::Break(());
            }
            let Some(caps) = src_pad.current_caps() else {
                return std::ops::ControlFlow::Continue(());
            };
            let Some(structure) = caps.structure(0) else {
                return std::ops::ControlFlow::Break(());
            };
            let media: Option<String> = structure.get::<String>("media").ok();
            if media.as_deref() != Some("video") {
                tracing::debug!("rtsp: skipping non-video pad (media={:?})", media);
                return std::ops::ControlFlow::Break(());
            }
            if let Err(e) = src_pad.link(&sink_pad) {
                tracing::warn!("rtsp: failed to link rtspsrc → depay: {e:?}");
            } else {
                tracing::info!("rtsp: video pad linked");
            }
            std::ops::ControlFlow::Break(())
        });
    let try_link_pad = try_link.clone();
    rtspsrc.connect_pad_added(move |_src, src_pad| {
        if try_link_pad(src_pad).is_break() {
            return;
        }
        // Caps not negotiated yet — re-try on every caps change. We rely on
        // the sink_pad.is_linked() check inside try_link to keep this
        // idempotent across multiple `notify::caps` emissions.
        let try_link_notify = try_link_pad.clone();
        src_pad.connect_notify_local(Some("caps"), move |pad, _spec| {
            let _ = try_link_notify(pad);
        });
    });
}

/// Buduje RTP capsfilter + tee + queue branchu A — wspólny front fan-outu RTP
/// dla obu wariantów. `tee` pozwala dowiesić Branch B (fMP4) bez przebudowy
/// pipeline'u; `queue_a` odcina branch A (dekod) od branchu B. Zwraca elementy
/// w kolejności linkowania; caller dodaje je do pipeline'u, linkuje
/// `rtp_filter → tee → queue_a` i podpina ogon branchu A do `queue_a`.
fn build_rtp_front() -> Result<(gst::Element, gst::Element, gst::Element)> {
    // RTP input capsfilter — pins the branch to `application/x-rtp,
    // media=video`. Without this, the downstream decoder may briefly see
    // ambiguous caps during rtspsrc setup and abort the pipeline with
    // `not-negotiated (-4)` before its first output pad is exposed.
    let rtp_caps = gst::Caps::builder("application/x-rtp")
        .field("media", "video")
        .build();
    let rtp_filter = gst::ElementFactory::make("capsfilter")
        .property("caps", &rtp_caps)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("rtp capsfilter: {e}")))?;

    // `tee` fans the RTP stream out between Branch A (decode → RGB → appsink,
    // always present, drives the existing frame_storage / streaming_bus path)
    // and an optional Branch B (rtph264depay → h264parse → mp4mux → appsink)
    // attached on demand by `attach_mp4_branch`.
    let tee = gst::ElementFactory::make("tee")
        .property("name", "rtp_tee")
        // allow-not-linked=true tolerates the gap between pipeline start and
        // first Branch B attach (and between Branch B detach and end of
        // session) — without it tee would push to a vanished pad and trip
        // `not-linked (-1)` immediately after request_pad release.
        .property("allow-not-linked", true)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("tee: {e}")))?;

    // Kolejka gałęzi A — odcina fan-out RTP od latencji dekodu. NIE-leaky:
    // bufory to surowe pakiety RTP (przed depay), więc zrzucenie któregoś
    // wycina fragment elementary stream i psuje access-unity aż do
    // najbliższego IDR (artefakty/przeskoki). Gubienie klatek odbywa się
    // dopiero ZA dekoderem (patrz `build_raw_leaky_queue`), gdzie bufor to
    // pełna, zdekodowana klatka.
    let queue_a = gst::ElementFactory::make("queue")
        .property("name", "queue_branch_a")
        .property("max-size-buffers", 100u32)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("queue_a: {e}")))?;
    Ok((rtp_filter, tee, queue_a))
}

/// Buduje kolejkę leaky=downstream na surowe klatki wideo — jedyne bezpieczne
/// miejsce gubienia przy spiętrzeniu (ZA dekoderem: drop zdekodowanej klatki
/// nie psuje referencji strumienia, w przeciwieństwie do dropu pakietów RTP
/// czy access-unitów przed dekoderem). Limit wyłącznie w buforach — surowa
/// klatka 1080p RGB to ~6 MB, więc domyślny limit bajtowy kolejki (10 MB)
/// zadziałałby już po 1 klatce; zerujemy limity bajtów i czasu.
pub(super) fn build_raw_leaky_queue(name: &str) -> Result<gst::Element> {
    let queue = gst::ElementFactory::make("queue")
        .property("name", name)
        .property("max-size-buffers", 5u32)
        .property("max-size-bytes", 0u32)
        .property("max-size-time", 0u64)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("{name}: {e}")))?;
    // `leaky` to enum GstQueueLeaky (nie surowy uint). "downstream" =
    // po zapełnieniu zrzuca najstarszy bufor.
    queue.set_property_from_str("leaky", "downstream");
    Ok(queue)
}

/// Buduje appsink z kontraktem ingestu (RGB24, max-buffers=1, drop=true) i
/// instaluje callback ramki. Cienki wrapper na `build_appsink_crops` — zostaje,
/// bo konektor MJPEG i inne miejsca wołają go bezpośrednio.
pub(super) fn build_appsink(
    camera_id: String,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) -> Result<gst::Element> {
    build_appsink_crops(camera_id, mailbox, counters)
}

/// Appsink gałęzi CROPS: pełna/1440p klatka RGB → mailbox (`LatestFrame`),
/// FrameStorage, StreamingBus. Wspólny dla obu wariantów RTSP oraz MJPEG —
/// kontrakt downstream jest niezależny od źródła i ścieżki dekodowania.
pub(super) fn build_appsink_crops(
    camera_id: String,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) -> Result<gst::Element> {
    let appsink = gst::ElementFactory::make("appsink")
        .property("name", "sink")
        .property("emit-signals", false)
        // RTSP frames arrive at network cadence; sync=false avoids stalling
        // when the clock and the RTSP source disagree on timestamps.
        .property("sync", false)
        .property("max-buffers", 1u32)
        .property("drop", true)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("appsink: {e}")))?;
    let appsink_app = appsink
        .clone()
        .downcast::<gst_app::AppSink>()
        .map_err(|_| CameraIngestError::PipelineBuild("appsink downcast failed".into()))?;
    install_frame_callback(&appsink_app, camera_id, mailbox, counters);
    Ok(appsink)
}

/// Crops appsink for the GPU-resident NV12 path (Stage 3): delivers RAW NV12 (no
/// per-frame `videoconvert` — that full-4K NV12→RGB is the CPU cost Stage 3
/// removes) into the mailbox only. Enrichment cuts crops from NV12; snapshots
/// convert lazily; the RGB streaming/`FrameStorage` path is served by the
/// on-demand RGB branch, only while a viewer is watching.
pub(super) fn build_appsink_crops_nv12(
    camera_id: String,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) -> Result<gst::Element> {
    let appsink = gst::ElementFactory::make("appsink")
        .property("name", "sink")
        .property("emit-signals", false)
        .property("sync", false)
        .property("max-buffers", 1u32)
        .property("drop", true)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("appsink nv12: {e}")))?;
    let appsink_app = appsink
        .clone()
        .downcast::<gst_app::AppSink>()
        .map_err(|_| CameraIngestError::PipelineBuild("appsink nv12 downcast failed".into()))?;
    install_frame_callback_nv12(&appsink_app, camera_id, mailbox, counters);
    Ok(appsink)
}

/// Appsink gałęzi DETECT: GPU-skalowana klatka RGB 560×560 → mailbox slot
/// detekcji (`put_detect`). Analizowy wyłącznie — NIE trafia do FrameStorage /
/// StreamingBus / counters (to domena gałęzi crops).
pub(super) fn build_appsink_detect(mailbox: Arc<FrameMailbox>) -> Result<gst::Element> {
    let appsink = gst::ElementFactory::make("appsink")
        .property("name", "sink_detect")
        .property("emit-signals", false)
        .property("sync", false)
        .property("max-buffers", 1u32)
        .property("drop", true)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("appsink_detect: {e}")))?;
    let appsink_app = appsink
        .clone()
        .downcast::<gst_app::AppSink>()
        .map_err(|_| CameraIngestError::PipelineBuild("appsink_detect downcast failed".into()))?;
    install_detect_frame_callback(&appsink_app, mailbox);
    Ok(appsink)
}

/// Appsink gałęzi DETECT w wariancie GPU-resident: SUROWA klatka NV12 → mailbox
/// slot detekcji (`put_detect`) z metadanymi NV12 (strides + colorimetry).
/// Detektor robi YUV→RGB + resize na GPU (`detect_batch_gpu`) — brak
/// videoconvert/resize na CPU. Analizowy wyłącznie (poza FrameStorage /
/// StreamingBus / counters).
pub(super) fn build_appsink_detect_nv12(mailbox: Arc<FrameMailbox>) -> Result<gst::Element> {
    let appsink = gst::ElementFactory::make("appsink")
        .property("name", "sink_detect_nv12")
        .property("emit-signals", false)
        .property("sync", false)
        .property("max-buffers", 1u32)
        .property("drop", true)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("appsink_detect_nv12: {e}")))?;
    let appsink_app = appsink
        .clone()
        .downcast::<gst_app::AppSink>()
        .map_err(|_| {
            CameraIngestError::PipelineBuild("appsink_detect_nv12 downcast failed".into())
        })?;
    install_detect_frame_callback_nv12(&appsink_app, mailbox);
    Ok(appsink)
}

/// Buduje i wpina gałąź DETECT NV12 do już dodanego `tee` z surowym wideo w
/// pamięci HOSTA (po `cudadownload`):
///
///   tee → queue(leaky) → capsfilter(video/x-raw,format=NV12) → appsink_detect_nv12
///
/// Bez `videoconvert`/`cudascale`: detektor dostaje pełnorozdzielczą klatkę NV12
/// i sam robi YUV→RGB + resize do 560 na GPU. Capsfilter pinuje format NV12
/// (cudadownload domyślnie oddaje NV12, ale wymuszamy jawnie). Budowane przed
/// Playing, więc bez `sync_state_with_parent`.
pub(super) fn attach_detect_branch_nv12(
    pipeline: &gst::Pipeline,
    tee: &gst::Element,
    mailbox: Arc<FrameMailbox>,
) -> Result<()> {
    let queue = build_raw_leaky_queue("queue_detect_nv12")?;
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "NV12")
        .build();
    let capsfilter = gst::ElementFactory::make("capsfilter")
        .property("caps", &caps)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("capsfilter detect nv12: {e}")))?;
    let appsink = build_appsink_detect_nv12(mailbox)?;

    pipeline
        .add_many([&queue, &capsfilter, &appsink])
        .map_err(|e| CameraIngestError::PipelineBuild(format!("add_many detect nv12: {e}")))?;

    let tee_src = tee.request_pad_simple("src_%u").ok_or_else(|| {
        CameraIngestError::PipelineBuild("tee src_%u (detect nv12) request failed".into())
    })?;
    let queue_sink = queue.static_pad("sink").ok_or_else(|| {
        CameraIngestError::PipelineBuild("queue_detect_nv12 sink pad missing".into())
    })?;
    tee_src
        .link(&queue_sink)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("tee → queue_detect_nv12: {e:?}")))?;
    gst::Element::link_many([&queue, &capsfilter, &appsink]).map_err(|e| {
        CameraIngestError::PipelineBuild(format!("link_many detect nv12 branch: {e}"))
    })?;
    Ok(())
}

/// Stage-4 zero-copy DETECT appsink: consumes NVDEC output IN DEVICE MEMORY
/// (`video/x-raw(memory:CUDAMemory), format=NV12`) — no `cudadownload`. The
/// callback maps the CUDA surface, runs the fused device preprocess, and hands
/// the detector an owned device tensor (host-download fallback per frame on any
/// map failure). Only built with the GPU inference features.
#[cfg(all(feature = "inference-vision-gpu", feature = "inference-supertonic"))]
pub(super) fn build_appsink_detect_cuda(mailbox: Arc<FrameMailbox>) -> Result<gst::Element> {
    let appsink = gst::ElementFactory::make("appsink")
        .property("name", "sink_detect_cuda")
        .property("emit-signals", false)
        .property("sync", false)
        .property("max-buffers", 1u32)
        .property("drop", true)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("appsink_detect_cuda: {e}")))?;
    let appsink_app = appsink
        .clone()
        .downcast::<gst_app::AppSink>()
        .map_err(|_| {
            CameraIngestError::PipelineBuild("appsink_detect_cuda downcast failed".into())
        })?;
    super::fakefile::install_detect_frame_callback_cuda(&appsink_app, mailbox);
    Ok(appsink)
}

/// Attaches the Stage-4 zero-copy DETECT branch to a CUDA-memory `tee`
/// (`tee_cuda`), placed BEFORE `cudadownload` so the decoder's device NV12 never
/// touches the CPU on this branch:
///
///   tee_cuda → queue(leaky) → capsfilter(video/x-raw(memory:CUDAMemory),NV12)
///     → appsink_detect_cuda
///
/// The capsfilter pins the CUDA memory feature + NV12 so negotiation keeps the
/// frame on the GPU. Built before Playing (no `sync_state_with_parent`).
#[cfg(all(feature = "inference-vision-gpu", feature = "inference-supertonic"))]
pub(super) fn attach_detect_branch_cuda(
    pipeline: &gst::Pipeline,
    tee: &gst::Element,
    mailbox: Arc<FrameMailbox>,
) -> Result<()> {
    let queue = build_raw_leaky_queue("queue_detect_cuda")?;
    let caps = gst::Caps::builder("video/x-raw")
        .features(["memory:CUDAMemory"])
        .field("format", "NV12")
        .build();
    let capsfilter = gst::ElementFactory::make("capsfilter")
        .property("caps", &caps)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("capsfilter detect cuda: {e}")))?;
    let appsink = build_appsink_detect_cuda(mailbox)?;

    pipeline
        .add_many([&queue, &capsfilter, &appsink])
        .map_err(|e| CameraIngestError::PipelineBuild(format!("add_many detect cuda: {e}")))?;

    let tee_src = tee.request_pad_simple("src_%u").ok_or_else(|| {
        CameraIngestError::PipelineBuild("tee_cuda src_%u (detect) request failed".into())
    })?;
    let queue_sink = queue.static_pad("sink").ok_or_else(|| {
        CameraIngestError::PipelineBuild("queue_detect_cuda sink pad missing".into())
    })?;
    tee_src
        .link(&queue_sink)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("tee_cuda → queue_detect_cuda: {e:?}")))?;
    gst::Element::link_many([&queue, &capsfilter, &appsink]).map_err(|e| {
        CameraIngestError::PipelineBuild(format!("link_many detect cuda branch: {e}"))
    })?;
    Ok(())
}

/// Zero-copy CROPS appsink: consumes NVDEC output IN DEVICE MEMORY
/// (`video/x-raw(memory:CUDAMemory), NV12`) off `tee_cuda` — no `cudadownload` on
/// the per-frame path. The callback ([`install_frame_callback_crops_cuda`]) maps
/// the CUDA surface and stores a device reference in the mailbox (host-download
/// fallback per frame on map failure). Only built with the GPU inference features.
#[cfg(all(feature = "inference-vision-gpu", feature = "inference-supertonic"))]
pub(super) fn build_appsink_crops_cuda(
    camera_id: String,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) -> Result<gst::Element> {
    let appsink = gst::ElementFactory::make("appsink")
        .property("name", "sink")
        .property("emit-signals", false)
        .property("sync", false)
        .property("max-buffers", 1u32)
        .property("drop", true)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("appsink crops cuda: {e}")))?;
    let appsink_app = appsink
        .clone()
        .downcast::<gst_app::AppSink>()
        .map_err(|_| CameraIngestError::PipelineBuild("appsink crops cuda downcast failed".into()))?;
    super::fakefile::install_frame_callback_crops_cuda(&appsink_app, camera_id, mailbox, counters);
    Ok(appsink)
}

/// Attaches the zero-copy CROPS branch to the CUDA-memory `tee` (`tee_cuda`),
/// BEFORE `cudadownload`, so the decoder's device NV12 never touches the CPU on
/// the crops path:
///
///   tee_cuda → queue(max-buffers=1, leaky) → capsfilter(CUDAMemory, NV12)
///     → appsink_crops_cuda
///
/// The queue caps at ONE buffer (leaky downstream) so the branch pins at most one
/// extra decode surface (plus the mailbox's latest) — never starving the pool.
/// Built before Playing (no `sync_state_with_parent`).
#[cfg(all(feature = "inference-vision-gpu", feature = "inference-supertonic"))]
pub(super) fn attach_crops_branch_cuda(
    pipeline: &gst::Pipeline,
    tee: &gst::Element,
    camera_id: String,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) -> Result<()> {
    // Minimal-pinning queue: a single device buffer, dropped when superseded, so
    // this branch never holds more than one NVDEC surface.
    let queue = gst::ElementFactory::make("queue")
        .property("name", "queue_crops_cuda")
        .property("max-size-buffers", 1u32)
        .property("max-size-bytes", 0u32)
        .property("max-size-time", 0u64)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("queue_crops_cuda: {e}")))?;
    queue.set_property_from_str("leaky", "downstream");
    let caps = gst::Caps::builder("video/x-raw")
        .features(["memory:CUDAMemory"])
        .field("format", "NV12")
        .build();
    let capsfilter = gst::ElementFactory::make("capsfilter")
        .property("caps", &caps)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("capsfilter crops cuda: {e}")))?;
    let appsink = build_appsink_crops_cuda(camera_id, mailbox, counters)?;

    pipeline
        .add_many([&queue, &capsfilter, &appsink])
        .map_err(|e| CameraIngestError::PipelineBuild(format!("add_many crops cuda: {e}")))?;

    let tee_src = tee.request_pad_simple("src_%u").ok_or_else(|| {
        CameraIngestError::PipelineBuild("tee_cuda src_%u (crops) request failed".into())
    })?;
    let queue_sink = queue.static_pad("sink").ok_or_else(|| {
        CameraIngestError::PipelineBuild("queue_crops_cuda sink pad missing".into())
    })?;
    tee_src
        .link(&queue_sink)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("tee_cuda → queue_crops_cuda: {e:?}")))?;
    gst::Element::link_many([&queue, &capsfilter, &appsink]).map_err(|e| {
        CameraIngestError::PipelineBuild(format!("link_many crops cuda branch: {e}"))
    })?;
    Ok(())
}

/// Czy dobudować gałąź GPU-owego skalowania klatki detekcji. `true` gdy runtime
/// ma komplet elementów CUDA (`cudaupload/cudaconvert/cudascale/cudadownload`)
/// ORAZ operator nie wymusił ścieżki CPU przez `[vision] gpu_resize = false`.
/// Gdy `false`, pipeline zostaje przy pojedynczym appsinku (crops) i detektor
/// resize'uje 4K→560 na CPU jak dotąd. Fallback runtime (negocjacja CUDA pada
/// przy Playing) obsługuje `run_rtsp_session` przez ponowną budowę z `false`.
pub(super) fn gpu_resize_enabled() -> bool {
    crate::vision::settings::get().gpu_resize && cuda_scale_available()
}

/// Whether to use the Stage-4 ZERO-COPY detect branch (device NV12 straight to
/// the detector, no `cudadownload`/re-upload round-trip). Opt-in via
/// `[vision] zerocopy_detect = true` and only meaningful with the GPU inference
/// features built in (they link cudart and provide the device-tensor detect
/// path). OFF by default: the deployed host-download NV12 detect is the
/// guaranteed fallback.
pub(super) fn zerocopy_enabled() -> bool {
    #[cfg(all(feature = "inference-vision-gpu", feature = "inference-supertonic"))]
    {
        crate::vision::settings::get().zerocopy_detect
    }
    #[cfg(not(all(feature = "inference-vision-gpu", feature = "inference-supertonic")))]
    {
        false
    }
}

/// Buduje i wpina gałąź DETECT do już dodanego do pipeline'u `tee` z surowym
/// wideo w pamięci HOSTA (po dekodzie, ewentualnym `cudadownload`):
///
///   tee → queue(leaky) → cudaupload → cudaconvert → cudascale →
///     cudadownload → videoconvert → capsfilter(RGB, DIM×DIM) → appsink_detect
///
/// Rozmiar 560 pinujemy WYŁĄCZNIE na końcowym capsfilterze RGB w pamięci hosta —
/// jedynym elementem skalującym w łańcuchu jest `cudascale`, więc negocjacja
/// caps przeciąga wymaganie rozmiaru w górę do niego (tak samo jak ogon
/// GPU-resident pinuje tylko format RGB i pozwala `cudaconvert` negocjować).
/// `cudaconvert` normalizuje natywny format dekodera (I420/NV12/RGBA) do postaci,
/// którą `cudascale` obsługuje niezawodnie — lustro działającego ogona
/// GpuResidentNvidia. Budowane przed Playing, więc bez `sync_state_with_parent`.
pub(super) fn attach_detect_branch(
    pipeline: &gst::Pipeline,
    tee: &gst::Element,
    mailbox: Arc<FrameMailbox>,
) -> Result<()> {
    let dim = DETECT_RESIZE_DIM as i32;
    let queue = build_raw_leaky_queue("queue_detect")?;
    let cudaupload = gst::ElementFactory::make("cudaupload")
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("cudaupload: {e}")))?;
    let cudaconvert = gst::ElementFactory::make("cudaconvert")
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("cudaconvert detect: {e}")))?;
    let cudascale = gst::ElementFactory::make("cudascale")
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("cudascale: {e}")))?;
    let cudadownload = gst::ElementFactory::make("cudadownload")
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("cudadownload detect: {e}")))?;
    let convert = gst::ElementFactory::make("videoconvert")
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("videoconvert detect: {e}")))?;
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "RGB")
        .field("width", dim)
        .field("height", dim)
        .build();
    let capsfilter = gst::ElementFactory::make("capsfilter")
        .property("caps", &caps)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("capsfilter detect: {e}")))?;
    let appsink = build_appsink_detect(mailbox)?;

    pipeline
        .add_many([
            &queue,
            &cudaupload,
            &cudaconvert,
            &cudascale,
            &cudadownload,
            &convert,
            &capsfilter,
            &appsink,
        ])
        .map_err(|e| CameraIngestError::PipelineBuild(format!("add_many detect: {e}")))?;

    let tee_src = tee.request_pad_simple("src_%u").ok_or_else(|| {
        CameraIngestError::PipelineBuild("tee src_%u (detect) request failed".into())
    })?;
    let queue_sink = queue
        .static_pad("sink")
        .ok_or_else(|| CameraIngestError::PipelineBuild("queue_detect sink pad missing".into()))?;
    tee_src
        .link(&queue_sink)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("tee → queue_detect: {e:?}")))?;
    gst::Element::link_many([
        &queue,
        &cudaupload,
        &cudaconvert,
        &cudascale,
        &cudadownload,
        &convert,
        &capsfilter,
        &appsink,
    ])
    .map_err(|e| CameraIngestError::PipelineBuild(format!("link_many detect branch: {e}")))?;
    Ok(())
}

/// Buduje `tee` (host raw video) rozgałęziający klatki dekodera na gałąź CROPS i
/// gałąź DETECT. `allow-not-linked=true` — okna między linkowaniem gałęzi.
pub(super) fn build_decode_tee(name: &str) -> Result<gst::Element> {
    gst::ElementFactory::make("tee")
        .property("name", name)
        .property("allow-not-linked", true)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("{name}: {e}")))
}

/// Dispatcher budowy pipeline'u RTSP. Wybiera wariant wg `ingest_path`:
///   * `GpuResidentNvidia` → `build_rtsp_pipeline_gpu_resident` (dekod +
///     konwersja kolorów na GPU, na CPU schodzi dopiero pełna klatka RGB),
///   * `NvdecCpuConvert`   → `build_rtsp_pipeline_nvdec` (dekod na GPU przez
///     NVDEC → cudadownload, konwersja kolorów NV12→RGB na CPU; bez wymogu
///     `cudaconvert`/`cudascale`, więc działa na nvcodec GStreamer 1.24),
///   * `Cpu`               → `build_rtsp_pipeline_cpu` (decodebin → videoconvert,
///     działa na każdej platformie; `use_hw_decode` pozwala decodebinowi
///     autoplugować dekoder sprzętowy best-effort).
/// Oba warianty mają identyczne wyjście (RGB, ten sam appsink callback), więc
/// reszta systemu (frame storage, fMP4 publisher, addon pickup) jest bez zmian.
///
/// TODO(GPU-resident inne platformy): analogiczne warianty GPU-resident dla
/// macOS/iOS (vtdec → metalconvert/glcolorconvert → gldownload), Windows
/// (d3d11h26Xdec → d3d11convert → d3d11download) i Linux VA-API
/// (vah26Xdec → vapostproc → vadownload) można dodać jako kolejne gałęzie
/// `IngestPath`. Dziś te platformy idą ścieżką CPU (działa, decodebin
/// autopluguje dekoder HW best-effort), więc to optymalizacja, nie brak funkcji.
fn build_rtsp_pipeline(
    camera_id: String,
    url: &str,
    timeout_secs: u32,
    ingest_path: IngestPath,
    use_hw_decode: bool,
    gpu_resize: bool,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) -> Result<RtspPipelineHandles> {
    match ingest_path {
        IngestPath::GpuResidentNvidia => build_rtsp_pipeline_gpu_resident(
            camera_id,
            url,
            timeout_secs,
            gpu_resize,
            mailbox,
            counters,
        ),
        // NvdecNv12 and NvdecCpuConvert share the same builder; `nv12_detect`
        // switches the DETECT branch to raw NV12 (no videoconvert/resize) and
        // `zerocopy` (opt-in, Stage 4) further keeps the detect NV12 on the GPU
        // (no `cudadownload` round-trip) — see `zerocopy_enabled`.
        IngestPath::NvdecNv12 => build_rtsp_pipeline_nvdec(
            camera_id,
            url,
            timeout_secs,
            gpu_resize,
            true,
            zerocopy_enabled(),
            super::fakefile::zerocopy_crops_enabled(),
            mailbox,
            counters,
        ),
        IngestPath::NvdecCpuConvert => build_rtsp_pipeline_nvdec(
            camera_id,
            url,
            timeout_secs,
            gpu_resize,
            false,
            false,
            false,
            mailbox,
            counters,
        ),
        IngestPath::Cpu => build_rtsp_pipeline_cpu(
            camera_id,
            url,
            timeout_secs,
            use_hw_decode,
            gpu_resize,
            mailbox,
            counters,
        ),
    }
}

/// Wariant CPU pipeline'u RTSP — `decodebin` autopluguje depay+parse+dekoder
/// wg kodeka strumienia, a `videoconvert` daje RGB na CPU. Zawsze działa
/// (każda platforma). `rtspsrc`'s source pad is dynamic (it appears once SDP
/// negotiation completes), so we register a `pad-added` handler that links it
/// to the RTP capsfilter only for video streams.
fn build_rtsp_pipeline_cpu(
    camera_id: String,
    url: &str,
    timeout_secs: u32,
    use_hw_decode: bool,
    gpu_resize: bool,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) -> Result<RtspPipelineHandles> {
    let pipeline = gst::Pipeline::new();
    let rtspsrc = build_rtspsrc(url, timeout_secs)?;

    // `decodebin` autoplugs the right depayloader + parser + decoder based
    // on the RTP caps actually delivered by the server. Previous pipeline
    // hard-wired rtph264depay+h264parse+avdec_h264 which `not-negotiated`s
    // out when the NVR streams H.265 (UniFi G4/G5, Hikvision IPC-Bxxx,
    // Dahua N-series with HEVC profile) or MJPEG. Using decodebin keeps the
    // pipeline codec-agnostic; downstream we still cap to RGB so the appsink
    // contract (raw RGB24 frames) is unchanged.
    let decodebin = gst::ElementFactory::make("decodebin")
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("decodebin: {e}")))?;
    // Wybór dekodera: `force-sw-decoders=false` pozwala decodebinowi
    // autoplugować dekoder sprzętowy (NVIDIA / VA-API / D3D11 / VideoToolbox /
    // MediaCodec) wykryty przez `decoder_detect`. Większość tych dekoderów
    // udostępnia jednak ramki w pamięci GPU (np. CUDAMemory NV12), której
    // `videoconvert` (CPU) nie odczyta — wtedy pipeline wyłoży się na
    // `not-negotiated (-4)` przy żądaniu `format=RGB`. Dlatego HW jest
    // próbą „best effort": gdy negocjacja padnie, `run_rtsp_session`
    // przebudowuje pipeline z `use_hw_decode=false` (zawsze działający
    // fallback CPU). Dekodowanie programowe 1080p H.264 to ~3-5% rdzenia.
    // Setujemy przez `set_property` po build, bo builder-side
    // `.property("force-sw-decoders", ...)` bywał ignorowany w gstreamer-rs 0.23.
    decodebin.set_property("force-sw-decoders", !use_hw_decode);
    let fsd_active: bool = decodebin.property("force-sw-decoders");
    tracing::info!(
        "rtsp: decodebin force-sw-decoders={} (use_hw_decode={})",
        fsd_active,
        use_hw_decode
    );
    let (rtp_filter, tee, queue_a) = build_rtp_front()?;

    // Kolejka leaky ZA dekoderem — jedyne miejsce gubienia klatek przy
    // spiętrzeniu (drop zdekodowanej klatki jest bezpieczny, patrz
    // `build_raw_leaky_queue`).
    let queue_dec = build_raw_leaky_queue("queue_decoded_a")?;

    let convert = gst::ElementFactory::make("videoconvert")
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("videoconvert: {e}")))?;

    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "RGB")
        .build();
    let capsfilter = gst::ElementFactory::make("capsfilter")
        .property("caps", &caps)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("capsfilter: {e}")))?;

    let appsink = build_appsink_crops(camera_id, mailbox.clone(), counters)?;

    // Optional GPU detect tee inserted between the decoder and the crops tail.
    // When present, the decoder's dynamic pad feeds THIS tee (not queue_dec
    // directly); the tee fans out to the crops tail and the GPU detect branch.
    let decode_tee = if gpu_resize {
        Some(build_decode_tee("tee_decode")?)
    } else {
        None
    };

    pipeline
        .add_many([
            &rtspsrc,
            &rtp_filter,
            &tee,
            &queue_a,
            &decodebin,
            &queue_dec,
            &convert,
            &capsfilter,
            &appsink,
        ])
        .map_err(|e| CameraIngestError::PipelineBuild(format!("add_many: {e}")))?;
    if let Some(t) = &decode_tee {
        pipeline
            .add_many([t])
            .map_err(|e| CameraIngestError::PipelineBuild(format!("add_many tee_decode: {e}")))?;
    }

    // Static segments:
    //   rtp_filter → tee (capsfilter pins RTP video before fan-out)
    //   tee.src_0 → queue_a → decodebin (request pad, Branch A always-on)
    //   queue_dec → convert → capsfilter → appsink (after decode)
    // rtspsrc → rtp_filter is dynamic (pad-added below) and decodebin → decode_out
    // is dynamic (decoder src pad appears after autoplug).
    gst::Element::link(&rtp_filter, &tee)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("rtp_filter → tee: {e}")))?;
    let tee_src_a = tee
        .request_pad_simple("src_%u")
        .ok_or_else(|| CameraIngestError::PipelineBuild("tee src_%u request failed".into()))?;
    let queue_a_sink = queue_a
        .static_pad("sink")
        .ok_or_else(|| CameraIngestError::PipelineBuild("queue_a sink pad missing".into()))?;
    tee_src_a
        .link(&queue_a_sink)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("tee → queue_a: {e:?}")))?;
    gst::Element::link(&queue_a, &decodebin)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("queue_a → decodebin: {e}")))?;
    gst::Element::link_many([&queue_dec, &convert, &capsfilter, &appsink])
        .map_err(|e| CameraIngestError::PipelineBuild(format!("link_many tail: {e}")))?;

    // With the detect tee present, link tee → queue_dec (crops) and attach the
    // GPU detect branch. `decode_out` (the sink for the decoder's dynamic pad)
    // becomes the tee; otherwise it stays queue_dec (original direct wiring).
    let decode_out = if let Some(t) = &decode_tee {
        let tee_crops = t.request_pad_simple("src_%u").ok_or_else(|| {
            CameraIngestError::PipelineBuild("tee_decode src_%u (crops) request failed".into())
        })?;
        let queue_dec_sink = queue_dec
            .static_pad("sink")
            .ok_or_else(|| CameraIngestError::PipelineBuild("queue_dec sink pad missing".into()))?;
        tee_crops.link(&queue_dec_sink).map_err(|e| {
            CameraIngestError::PipelineBuild(format!("tee_decode → queue_dec: {e:?}"))
        })?;
        attach_detect_branch(&pipeline, t, mailbox.clone())?;
        t.clone()
    } else {
        queue_dec.clone()
    };

    // decodebin's video output pad appears dynamically once the codec is
    // identified. Wire it into `decode_out` (detect tee or queue_dec) when caps
    // say video/x-raw.
    let decode_out_weak = decode_out.downgrade();
    decodebin.connect_pad_added(move |_dec, src_pad| {
        let Some(decode_out) = decode_out_weak.upgrade() else {
            return;
        };
        let Some(sink_pad) = decode_out.static_pad("sink") else {
            return;
        };
        if sink_pad.is_linked() {
            return;
        }
        let Some(caps) = src_pad.current_caps() else {
            return;
        };
        let Some(structure) = caps.structure(0) else {
            return;
        };
        if !structure.name().starts_with("video/") {
            tracing::debug!(
                "rtsp: decodebin produced non-video pad ({})",
                structure.name()
            );
            return;
        }
        if let Err(e) = src_pad.link(&sink_pad) {
            tracing::warn!("rtsp: decodebin → decode_out link failed: {e:?}");
        } else {
            tracing::info!("rtsp: decodebin video pad linked (codec auto-detected)");
        }
    });

    // The dynamic pad from rtspsrc feeds the RTP capsfilter, statically linked
    // to `decodebin` above.
    connect_rtspsrc_video_pad(&rtspsrc, &rtp_filter);

    Ok(RtspPipelineHandles {
        pipeline,
        tee,
        decode_tee: None,
        decode_tee_is_cuda: false,
    })
}

/// Wariant GPU-resident NVIDIA. Branch A dekoduje I konwertuje kolory na GPU:
///
///   rtspsrc → rtp_filter → tee → queue_a → rtphXdepay → hXparse →
///     nvhXdec (klatka w `video/x-raw(memory:CUDAMemory),NV12`) →
///     cudaconvert (NV12→RGBA na GPU) → cudadownload (CUDAMemory→host) →
///     queue (leaky, gubienie całych klatek przy spiętrzeniu) →
///     videoconvert (siatka bezpieczeństwa do RGB) → capsfilter RGB → appsink
///
/// `nvhXdec`/`rtphXdepay`/`hXparse` dobierane są w runtime wg kodeka strumienia
/// (`encoding-name` z caps RTP): H264 → rtph264depay+h264parse+nvh264dec,
/// H265/HEVC → rtph265depay+h265parse+nvh265dec. Branch A dobudowywany jest
/// dynamicznie po negocjacji caps RTP, bo kodek znamy dopiero wtedy. Gdy kodek
/// nie ma odpowiednika NVDEC (np. MJPEG), branch A się nie zbuduje, pipeline
/// nie da klatek i `run_rtsp_session` przełączy się na ścieżkę CPU.
///
/// Wyjście jest identyczne jak w wariancie CPU (RGB, te same caps, ten sam
/// appsink callback) — reszta systemu jest bez zmian. `cudaconvert` celuje w
/// RGBA na GPU (natywny format wyjścia NVDEC→CUDA), a końcowy `videoconvert`
/// gwarantuje RGB nawet gdy `cudadownload` odda inny układ — to ostatnia
/// konwersja na CPU, dużo tańsza niż pełny dekod programowy.
fn build_rtsp_pipeline_gpu_resident(
    camera_id: String,
    url: &str,
    timeout_secs: u32,
    gpu_resize: bool,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) -> Result<RtspPipelineHandles> {
    let pipeline = gst::Pipeline::new();
    let rtspsrc = build_rtspsrc(url, timeout_secs)?;
    let (rtp_filter, tee, queue_a) = build_rtp_front()?;

    // Statyczny ogon branchu A: konwersja+download na GPU, potem RGB na CPU.
    // `cudaconvert` robi NV12→RGBA w pamięci CUDA; `cudadownload` przenosi
    // gotową klatkę do pamięci hosta. Końcowy `videoconvert` jest siatką
    // bezpieczeństwa do RGB (cudadownload eksponuje RGBA/RGB/itd.).
    let cudaconvert = gst::ElementFactory::make("cudaconvert")
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("cudaconvert: {e}")))?;
    let cudadownload = gst::ElementFactory::make("cudadownload")
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("cudadownload: {e}")))?;
    // Kolejka leaky ZA dekoderem (po zejściu klatki do pamięci hosta) —
    // jedyne bezpieczne miejsce gubienia przy spiętrzeniu, patrz
    // `build_raw_leaky_queue`.
    let queue_dec = build_raw_leaky_queue("queue_decoded_a")?;
    let convert = gst::ElementFactory::make("videoconvert")
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("videoconvert: {e}")))?;
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "RGB")
        .build();
    let capsfilter = gst::ElementFactory::make("capsfilter")
        .property("caps", &caps)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("capsfilter: {e}")))?;
    let appsink = build_appsink_crops(camera_id, mailbox.clone(), counters)?;

    // Optional GPU detect tee inserted after `cudadownload` (host raw video).
    // The full frame is already in host memory here (crops need it), so the
    // detect branch re-uploads it to CUDA for the 560 scale — cheaper than the
    // detector's ~4 ms CPU resize of the full 4K frame.
    let decode_tee = if gpu_resize {
        Some(build_decode_tee("tee_decode")?)
    } else {
        None
    };

    pipeline
        .add_many([
            &rtspsrc,
            &rtp_filter,
            &tee,
            &queue_a,
            &cudaconvert,
            &cudadownload,
            &queue_dec,
            &convert,
            &capsfilter,
            &appsink,
        ])
        .map_err(|e| CameraIngestError::PipelineBuild(format!("add_many gpu: {e}")))?;
    if let Some(t) = &decode_tee {
        pipeline.add_many([t]).map_err(|e| {
            CameraIngestError::PipelineBuild(format!("add_many tee_decode gpu: {e}"))
        })?;
    }

    // Statyczne segmenty znane przed negocjacją:
    //   rtp_filter → tee → queue_a (front RTP)
    //   cudaconvert → cudadownload → [tee_decode →] queue_dec → videoconvert →
    //   capsfilter → appsink (ogon)
    // Środek (depay → parse → nvhXdec) dobudowujemy dynamicznie po poznaniu
    // kodeka i wpinamy między queue_a a cudaconvert.
    gst::Element::link(&rtp_filter, &tee)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("rtp_filter → tee: {e}")))?;
    let tee_src_a = tee
        .request_pad_simple("src_%u")
        .ok_or_else(|| CameraIngestError::PipelineBuild("tee src_%u request failed".into()))?;
    let queue_a_sink = queue_a
        .static_pad("sink")
        .ok_or_else(|| CameraIngestError::PipelineBuild("queue_a sink pad missing".into()))?;
    tee_src_a
        .link(&queue_a_sink)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("tee → queue_a: {e:?}")))?;
    // Ogon: cudaconvert → cudadownload, potem albo wprost do queue_dec (crops),
    // albo przez tee_decode rozgałęziający na crops + detect.
    gst::Element::link(&cudaconvert, &cudadownload).map_err(|e| {
        CameraIngestError::PipelineBuild(format!("cudaconvert → cudadownload: {e}"))
    })?;
    if let Some(t) = &decode_tee {
        gst::Element::link(&cudadownload, t).map_err(|e| {
            CameraIngestError::PipelineBuild(format!("cudadownload → tee_decode: {e}"))
        })?;
        let tee_crops = t.request_pad_simple("src_%u").ok_or_else(|| {
            CameraIngestError::PipelineBuild("tee_decode src_%u (crops) request failed".into())
        })?;
        let queue_dec_sink = queue_dec
            .static_pad("sink")
            .ok_or_else(|| CameraIngestError::PipelineBuild("queue_dec sink pad missing".into()))?;
        tee_crops.link(&queue_dec_sink).map_err(|e| {
            CameraIngestError::PipelineBuild(format!("tee_decode → queue_dec: {e:?}"))
        })?;
        attach_detect_branch(&pipeline, t, mailbox.clone())?;
    } else {
        gst::Element::link(&cudadownload, &queue_dec).map_err(|e| {
            CameraIngestError::PipelineBuild(format!("cudadownload → queue_dec: {e}"))
        })?;
    }
    gst::Element::link_many([&queue_dec, &convert, &capsfilter, &appsink])
        .map_err(|e| CameraIngestError::PipelineBuild(format!("link_many gpu tail: {e}")))?;

    // Dobudowa dekodera NVDEC po negocjacji RTP. queue_a ma stałe caps
    // `application/x-rtp` (rtp_filter wymusza video), ale `encoding-name`
    // (H264 vs H265) znamy dopiero po SETUP rtspsrc. Wieszamy więc watcher na
    // src padzie queue_a: gdy caps się pojawią, tworzymy odpowiedni
    // depay+parse+nvhXdec, dodajemy do pipeline'u, linkujemy
    // queue_a → depay → parse → nvhXdec → cudaconvert i podnosimy stan.
    let pipeline_weak = pipeline.downgrade();
    let cudaconvert_weak = cudaconvert.downgrade();
    let queue_a_src = queue_a
        .static_pad("src")
        .ok_or_else(|| CameraIngestError::PipelineBuild("queue_a src pad missing".into()))?;
    let built = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let build_decoder = move |caps: &gst::Caps| {
        if built.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let Some(pipeline) = pipeline_weak.upgrade() else {
            return;
        };
        let Some(cudaconvert) = cudaconvert_weak.upgrade() else {
            return;
        };
        let Some(queue_a) = pipeline.by_name("queue_branch_a") else {
            return;
        };
        let encoding = caps
            .structure(0)
            .and_then(|s| s.get::<String>("encoding-name").ok())
            .unwrap_or_default();
        if let Err(e) = link_nvdec_branch(&pipeline, &queue_a, &cudaconvert, &encoding) {
            // Nie panikujemy: brak NVDEC dla tego kodeka (np. MJPEG) oznacza,
            // że branch A nie ruszy, pipeline nie da klatek, a sesja zejdzie
            // na ścieżkę CPU. Logujemy, żeby było jasne dlaczego.
            tracing::warn!(
                encoding = %encoding,
                error = %e,
                "rtsp: nie udało się zbudować gałęzi NVDEC — fallback CPU nastąpi przez sesję"
            );
        }
    };
    let build_decoder = std::sync::Arc::new(build_decoder);
    if let Some(caps) = queue_a_src.current_caps() {
        build_decoder(&caps);
    } else {
        let build_notify = build_decoder.clone();
        // `notify::caps` is emitted on the GStreamer streaming thread, but this
        // watcher is registered from the pipeline-build (tokio) thread. The
        // `_local` variant binds the closure to its registration thread and
        // glib's ThreadGuard aborts the process when the signal later fires on
        // the streaming thread ("Value accessed from different thread…"). The
        // closure only captures Send+Sync state (AtomicBool + WeakRefs) and does
        // thread-safe gst ops, so the cross-thread-safe `connect_notify` is correct.
        queue_a_src.connect_notify(Some("caps"), move |pad, _spec| {
            if let Some(caps) = pad.current_caps() {
                build_notify(&caps);
            }
        });
    }

    // Front RTP identyczny jak w wariancie CPU.
    connect_rtspsrc_video_pad(&rtspsrc, &rtp_filter);

    Ok(RtspPipelineHandles {
        pipeline,
        tee,
        decode_tee: None,
        decode_tee_is_cuda: false,
    })
}

/// Wariant NvdecCpuConvert — dekod na GPU (NVDEC), konwersja kolorów na CPU.
/// Pośredni między pełnym GPU-resident (wymaga `cudaconvert`/`cudascale`,
/// obecnych dopiero w nvcodec GStreamer ≥1.26) a czystym CPU. Branch A:
///
///   rtspsrc → rtp_filter → tee → queue_a → rtphXdepay → hXparse →
///     nvhXdec (klatka w `video/x-raw(memory:CUDAMemory),NV12`) →
///     cudadownload (CUDAMemory→host) → queue (leaky) →
///     videoconvert (NV12→RGB na CPU) → capsfilter RGB → appsink
///
/// Kluczowa różnica względem GPU-resident: BRAK `cudaconvert` (konwersja NV12→RGB
/// schodzi na CPU do `videoconvert`), więc pipeline zbuduje się na nvcodec bez
/// `cudaconvert`/`cudascale`. Mimo to najdroższy koszt — programowy dekod 4K
/// H.264 (~15-20% rdzenia na kamerę) — schodzi na NVDEC; na CPU zostaje tylko
/// dużo tańsza konwersja kolorów.
///
/// `nvhXdec`/`rtphXdepay`/`hXparse` dobierane są w runtime wg `encoding-name`
/// z caps RTP (H264 → nvh264dec, H265/HEVC → nvh265dec). Branch A dobudowywany
/// dynamicznie po negocjacji caps RTP (kodek znamy dopiero wtedy). Gdy kodek nie
/// ma odpowiednika NVDEC (np. MJPEG), branch A się nie zbuduje, pipeline nie da
/// klatek i `run_rtsp_session` przełączy się na ścieżkę CPU. Wyjście identyczne
/// jak w wariantach CPU/GPU-resident (RGB, ten sam appsink callback) — reszta
/// systemu jest bez zmian.
///
/// `nv12_detect` selects the GPU-resident DETECT branch: instead of the RGB-560
/// GPU-resize tee, the decoded host NV12 is teed straight into a raw-NV12 detect
/// appsink (no videoconvert/resize) and the detector does YUV→RGB + resize on
/// the GPU (`detect_batch_gpu`). The crops/display branch is unchanged
/// (`videoconvert` NV12→RGB). `false` is the deployed NvdecCpuConvert path,
/// byte-identical to before.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
fn build_rtsp_pipeline_nvdec(
    camera_id: String,
    url: &str,
    timeout_secs: u32,
    gpu_resize: bool,
    nv12_detect: bool,
    zerocopy: bool,
    zerocopy_crops: bool,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) -> Result<RtspPipelineHandles> {
    // Zero-copy detect is a sub-mode of the NV12 detect path (Stage 3). Without
    // `nv12_detect` there is no device NV12 detect branch to keep on the GPU.
    let zerocopy = zerocopy && nv12_detect;
    // Zero-copy CROPS builds on zero-copy detect: the crops appsink reads DEVICE
    // NV12 off `tee_cuda` (no per-frame `cudadownload`), so it requires the CUDA
    // tee. Without `zerocopy` there is no `tee_cuda` to hang the crops on.
    let zerocopy_crops = zerocopy_crops && zerocopy;
    let pipeline = gst::Pipeline::new();
    let rtspsrc = build_rtspsrc(url, timeout_secs)?;
    let (rtp_filter, tee, queue_a) = build_rtp_front()?;

    // Statyczny ogon branchu A. `cudadownload` to punkt wpięcia dekodera:
    // przenosi zdekodowaną klatkę z pamięci CUDA do pamięci hosta, a NV12→RGB
    // robi już `videoconvert` na CPU (brak cudaconvert w tym wariancie).
    //
    // Zero-copy crops: NO always-on `cudadownload`/`queue_dec`/crops appsink here
    // — the crops appsink reads DEVICE NV12 off `tee_cuda`, and the on-demand RGB
    // display branch carries its own `cudadownload` (attached only while watched).
    let cudadownload = if zerocopy_crops {
        None
    } else {
        Some(
            gst::ElementFactory::make("cudadownload")
                .build()
                .map_err(|e| CameraIngestError::PipelineBuild(format!("cudadownload: {e}")))?,
        )
    };
    // Kolejka leaky ZA dekoderem (po zejściu klatki do pamięci hosta) — jedyne
    // bezpieczne miejsce gubienia przy spiętrzeniu, patrz `build_raw_leaky_queue`.
    let queue_dec = if zerocopy_crops {
        None
    } else {
        Some(build_raw_leaky_queue("queue_decoded_a")?)
    };
    // Stage 3: on the GPU-resident NV12 path the crops appsink delivers RAW NV12
    // (no per-frame `videoconvert` — the full-4K NV12→RGB was ~90% of a core per
    // camera), and RGB is produced only on-demand by `attach_rgb_branch`. On the
    // NvdecCpuConvert path (nv12_detect=false) the crops tail stays
    // `videoconvert → RGB` — byte-identical to before.
    let (convert, capsfilter) = if nv12_detect {
        (None, None)
    } else {
        let convert = gst::ElementFactory::make("videoconvert")
            .build()
            .map_err(|e| CameraIngestError::PipelineBuild(format!("videoconvert: {e}")))?;
        let caps = gst::Caps::builder("video/x-raw")
            .field("format", "RGB")
            .build();
        let capsfilter = gst::ElementFactory::make("capsfilter")
            .property("caps", &caps)
            .build()
            .map_err(|e| CameraIngestError::PipelineBuild(format!("capsfilter: {e}")))?;
        (Some(convert), Some(capsfilter))
    };
    // Zero-copy crops: the crops appsink is built + attached off `tee_cuda` below
    // (device NV12). Otherwise it sits on the always-on host tail here.
    let appsink = if zerocopy_crops {
        None
    } else if nv12_detect {
        Some(build_appsink_crops_nv12(
            camera_id.clone(),
            mailbox.clone(),
            counters.clone(),
        )?)
    } else {
        Some(build_appsink_crops(
            camera_id.clone(),
            mailbox.clone(),
            counters.clone(),
        )?)
    };

    // Opcjonalny tee po `cudadownload` (surowe wideo w pamięci hosta) na gałąź
    // CROPS + gałąź DETECT. Potrzebny gdy:
    //   * `nv12_detect` — gałąź detekcji surowego NV12 (ścieżka GPU-resident),
    //   * `gpu_resize`  — gałąź GPU-skalowania klatki detekcji do RGB 560
    //     (w praktyce false tutaj: `cuda_scale_available` wymaga
    //     `cudaconvert`/`cudascale`, których brak — inaczej byłby GPU-resident).
    // `nv12_detect` wygrywa nad `gpu_resize`: raw NV12 idzie prosto do detektora
    // (YUV→RGB + resize na GPU), więc RGB-560 tee jest zbędny.
    // Zero-copy crops has no post-`cudadownload` host tee — display attaches its
    // own `cudadownload` off `tee_cuda` on demand.
    let decode_tee = if !zerocopy_crops && (nv12_detect || gpu_resize) {
        Some(build_decode_tee("tee_decode")?)
    } else {
        None
    };

    // Stage 4: a CUDA-memory `tee` placed BEFORE `cudadownload`. One branch still
    // downloads to host (crops + on-demand RGB, unchanged); the other keeps the
    // decoder's device NV12 and feeds the zero-copy detect appsink — no
    // GPU→CPU→GPU round-trip for detection. Only when `zerocopy`.
    let tee_cuda = if zerocopy {
        Some(build_decode_tee("tee_cuda")?)
    } else {
        None
    };

    pipeline
        .add_many([&rtspsrc, &rtp_filter, &tee, &queue_a])
        .map_err(|e| CameraIngestError::PipelineBuild(format!("add_many nvdec: {e}")))?;
    if let Some(cd) = &cudadownload {
        pipeline
            .add_many([cd])
            .map_err(|e| CameraIngestError::PipelineBuild(format!("add_many cudadownload: {e}")))?;
    }
    if let Some(qd) = &queue_dec {
        pipeline
            .add_many([qd])
            .map_err(|e| CameraIngestError::PipelineBuild(format!("add_many queue_dec: {e}")))?;
    }
    if let Some(a) = &appsink {
        pipeline
            .add_many([a])
            .map_err(|e| CameraIngestError::PipelineBuild(format!("add_many crops appsink: {e}")))?;
    }
    if let Some(tc) = &tee_cuda {
        pipeline.add_many([tc]).map_err(|e| {
            CameraIngestError::PipelineBuild(format!("add_many tee_cuda nvdec: {e}"))
        })?;
    }
    // RGB crops path adds the `videoconvert → capsfilter`; the NV12 path omits them.
    if let (Some(convert), Some(capsfilter)) = (&convert, &capsfilter) {
        pipeline.add_many([convert, capsfilter]).map_err(|e| {
            CameraIngestError::PipelineBuild(format!("add_many nvdec convert: {e}"))
        })?;
    }
    if let Some(t) = &decode_tee {
        pipeline.add_many([t]).map_err(|e| {
            CameraIngestError::PipelineBuild(format!("add_many tee_decode nvdec: {e}"))
        })?;
    }

    // Statyczne segmenty znane przed negocjacją:
    //   rtp_filter → tee → queue_a (front RTP)
    //   cudadownload → [tee_decode →] queue_dec → videoconvert → capsfilter →
    //   appsink (ogon)
    // Środek (depay → parse → nvhXdec) dobudowujemy dynamicznie po poznaniu
    // kodeka i wpinamy między queue_a a cudadownload.
    gst::Element::link(&rtp_filter, &tee)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("rtp_filter → tee: {e}")))?;
    let tee_src_a = tee
        .request_pad_simple("src_%u")
        .ok_or_else(|| CameraIngestError::PipelineBuild("tee src_%u request failed".into()))?;
    let queue_a_sink = queue_a
        .static_pad("sink")
        .ok_or_else(|| CameraIngestError::PipelineBuild("queue_a sink pad missing".into()))?;
    tee_src_a
        .link(&queue_a_sink)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("tee → queue_a: {e:?}")))?;
    // Zero-copy: the decoder feeds `tee_cuda`; one src keeps device NV12 (detect)
    // and — on the zero-copy CROPS path — a second src also keeps device NV12 for
    // the crops appsink (no per-frame `cudadownload`). Otherwise one src links to
    // `cudadownload` so the host crops/on-demand-RGB tail below is unchanged.
    if let Some(tc) = &tee_cuda {
        #[allow(unused_mut)]
        let mut crops_on_cuda = false;
        #[cfg(all(feature = "inference-vision-gpu", feature = "inference-supertonic"))]
        {
            if zerocopy_crops {
                // Crops read DEVICE NV12 off the CUDA tee — the full-4K download is
                // gone from the per-frame path (mailbox holds a device reference).
                attach_crops_branch_cuda(
                    &pipeline,
                    tc,
                    camera_id.clone(),
                    mailbox.clone(),
                    counters.clone(),
                )?;
                crops_on_cuda = true;
            }
        }
        if !crops_on_cuda {
            let cudadownload = cudadownload
                .as_ref()
                .ok_or_else(|| CameraIngestError::PipelineBuild("cudadownload missing".into()))?;
            let tc_src = tc.request_pad_simple("src_%u").ok_or_else(|| {
                CameraIngestError::PipelineBuild("tee_cuda src_%u (crops) request failed".into())
            })?;
            let cudl_sink = cudadownload.static_pad("sink").ok_or_else(|| {
                CameraIngestError::PipelineBuild("cudadownload sink pad missing".into())
            })?;
            tc_src.link(&cudl_sink).map_err(|e| {
                CameraIngestError::PipelineBuild(format!("tee_cuda → cudadownload: {e:?}"))
            })?;
        }
        // Device NV12 detect off the CUDA tee (no download for the detect branch).
        #[cfg(all(feature = "inference-vision-gpu", feature = "inference-supertonic"))]
        attach_detect_branch_cuda(&pipeline, tc, mailbox.clone())?;
    }
    // Host crops/display tail (only when NOT zero-copy crops — otherwise crops are
    // on the CUDA tee above and there is no always-on `cudadownload`).
    if !zerocopy_crops {
        let cudadownload = cudadownload
            .as_ref()
            .ok_or_else(|| CameraIngestError::PipelineBuild("cudadownload missing".into()))?;
        let queue_dec = queue_dec
            .as_ref()
            .ok_or_else(|| CameraIngestError::PipelineBuild("queue_dec missing".into()))?;
        let appsink = appsink
            .as_ref()
            .ok_or_else(|| CameraIngestError::PipelineBuild("crops appsink missing".into()))?;
        // Ogon: cudadownload wprost do queue_dec (crops) albo przez tee_decode
        // rozgałęziający na crops + detect.
        if let Some(t) = &decode_tee {
            gst::Element::link(cudadownload, t).map_err(|e| {
                CameraIngestError::PipelineBuild(format!("cudadownload → tee_decode: {e}"))
            })?;
            let tee_crops = t.request_pad_simple("src_%u").ok_or_else(|| {
                CameraIngestError::PipelineBuild("tee_decode src_%u (crops) request failed".into())
            })?;
            let queue_dec_sink = queue_dec.static_pad("sink").ok_or_else(|| {
                CameraIngestError::PipelineBuild("queue_dec sink pad missing".into())
            })?;
            tee_crops.link(&queue_dec_sink).map_err(|e| {
                CameraIngestError::PipelineBuild(format!("tee_decode → queue_dec: {e:?}"))
            })?;
            // DETECT branch: on zero-copy the detect is already wired to `tee_cuda`
            // (device NV12, above); otherwise it hangs off the host tee — raw NV12
            // straight to the detector when `nv12_detect`, else the RGB-560 branch.
            if zerocopy {
                // Detect handled by the CUDA tee; host tee serves crops + RGB only.
            } else if nv12_detect {
                attach_detect_branch_nv12(&pipeline, t, mailbox.clone())?;
            } else {
                attach_detect_branch(&pipeline, t, mailbox.clone())?;
            }
        } else {
            gst::Element::link(cudadownload, queue_dec).map_err(|e| {
                CameraIngestError::PipelineBuild(format!("cudadownload → queue_dec: {e}"))
            })?;
        }
        // NV12 crops tail: `queue_dec → appsink` (raw NV12, no convert). RGB crops
        // tail: `queue_dec → videoconvert → capsfilter → appsink` (unchanged).
        match (&convert, &capsfilter) {
            (Some(convert), Some(capsfilter)) => {
                gst::Element::link_many([queue_dec, convert, capsfilter, appsink]).map_err(|e| {
                    CameraIngestError::PipelineBuild(format!("link_many nvdec tail: {e}"))
                })?;
            }
            _ => {
                gst::Element::link(queue_dec, appsink).map_err(|e| {
                    CameraIngestError::PipelineBuild(format!("link nvdec nv12 tail: {e}"))
                })?;
            }
        }
    }

    // Dobudowa dekodera NVDEC po negocjacji RTP — identyczna mechanika jak w
    // wariancie GPU-resident, tylko wyjście dekodera wpinamy w `cudadownload`
    // (nie `cudaconvert`). Kodek (H264 vs H265) znamy dopiero po SETUP rtspsrc,
    // więc wieszamy watcher na src padzie queue_a.
    let pipeline_weak = pipeline.downgrade();
    // Decoder output goes into `tee_cuda` on the zero-copy path (device NV12 is
    // kept for detect; `cudadownload` sits downstream of the tee), else straight
    // into `cudadownload` (host download) as before.
    let downstream_weak = match (&tee_cuda, &cudadownload) {
        (Some(tc), _) => tc.downgrade(),
        (None, Some(cd)) => cd.downgrade(),
        // Unreachable: `tee_cuda` is None only when `!zerocopy_crops`, where
        // `cudadownload` is always built. Fall back to the pipeline weak-ref shape
        // by pointing at `tee` so the closure still type-checks.
        (None, None) => tee.downgrade(),
    };
    let queue_a_src = queue_a
        .static_pad("src")
        .ok_or_else(|| CameraIngestError::PipelineBuild("queue_a src pad missing".into()))?;
    let built = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let build_decoder = move |caps: &gst::Caps| {
        if built.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let Some(pipeline) = pipeline_weak.upgrade() else {
            return;
        };
        let Some(downstream) = downstream_weak.upgrade() else {
            return;
        };
        let Some(queue_a) = pipeline.by_name("queue_branch_a") else {
            return;
        };
        let encoding = caps
            .structure(0)
            .and_then(|s| s.get::<String>("encoding-name").ok())
            .unwrap_or_default();
        if let Err(e) = link_nvdec_branch(&pipeline, &queue_a, &downstream, &encoding) {
            // Nie panikujemy: brak NVDEC dla tego kodeka (np. MJPEG) oznacza,
            // że branch A nie ruszy, pipeline nie da klatek, a sesja zejdzie na
            // ścieżkę CPU. Logujemy, żeby było jasne dlaczego.
            tracing::warn!(
                encoding = %encoding,
                error = %e,
                "rtsp: nie udało się zbudować gałęzi NVDEC — fallback CPU nastąpi przez sesję"
            );
        }
    };
    let build_decoder = std::sync::Arc::new(build_decoder);
    if let Some(caps) = queue_a_src.current_caps() {
        build_decoder(&caps);
    } else {
        let build_notify = build_decoder.clone();
        // Patrz komentarz w `build_rtsp_pipeline_gpu_resident`: `connect_notify`
        // (nie `_local`), bo `notify::caps` fired jest na wątku streamingu, a
        // domknięcie łapie tylko stan Send+Sync (AtomicBool + WeakRef).
        queue_a_src.connect_notify(Some("caps"), move |pad, _spec| {
            if let Some(caps) = pad.current_caps() {
                build_notify(&caps);
            }
        });
    }

    // Front RTP identyczny jak w pozostałych wariantach.
    connect_rtspsrc_video_pad(&rtspsrc, &rtp_filter);

    // Expose the tee the on-demand RGB display branch attaches to:
    //   * zero-copy crops → the CUDA tee (`tee_cuda`); the display branch carries
    //     its own `cudadownload` (`attach_rgb_branch_cuda`), so downloads run only
    //     while a viewer watches;
    //   * NV12 crops (host) → the post-`cudadownload` tee (`attach_rgb_branch`);
    //   * RGB crops (gpu_resize / NvdecCpuConvert) → none (crops already RGB).
    let (decode_tee, decode_tee_is_cuda) = if zerocopy_crops {
        (tee_cuda.clone(), true)
    } else if nv12_detect {
        (decode_tee, false)
    } else {
        (None, false)
    };
    Ok(RtspPipelineHandles {
        pipeline,
        tee,
        decode_tee,
        decode_tee_is_cuda,
    })
}

/// Tworzy i wpina łańcuch dekodera NVDEC `depay → parse → nvhXdec` między
/// `queue_a` a `downstream`, dobierając elementy wg `encoding-name` RTP:
/// H264 → rtph264depay+h264parse+nvh264dec, H265/HEVC → rtph265depay+
/// h265parse+nvh265dec. `downstream` to element, do którego wpina się wyjście
/// dekodera: `cudaconvert` w wariancie GPU-resident (konwersja kolorów na GPU)
/// lub `cudadownload` w wariancie NvdecCpuConvert (od razu zejście do pamięci
/// hosta, konwersja na CPU). Po dodaniu elementów podnosi ich stan do stanu
/// pipeline'u (`sync_state_with_parent`), bo dokładamy je po starcie. Zwraca
/// błąd dla kodeków bez odpowiednika NVDEC (np. MJPEG) — wtedy branch A nie
/// rusza i sesja schodzi na CPU.
fn link_nvdec_branch(
    pipeline: &gst::Pipeline,
    queue_a: &gst::Element,
    downstream: &gst::Element,
    encoding: &str,
) -> std::result::Result<(), String> {
    let (depay_name, parse_name, dec_name) = if encoding.eq_ignore_ascii_case("H264") {
        ("rtph264depay", "h264parse", "nvh264dec")
    } else if encoding.eq_ignore_ascii_case("H265") || encoding.eq_ignore_ascii_case("HEVC") {
        ("rtph265depay", "h265parse", "nvh265dec")
    } else {
        return Err(format!("brak dekodera NVDEC dla kodeka {encoding}"));
    };

    let depay = gst::ElementFactory::make(depay_name)
        .build()
        .map_err(|e| format!("{depay_name}: {e}"))?;
    let parse = gst::ElementFactory::make(parse_name)
        .build()
        .map_err(|e| format!("{parse_name}: {e}"))?;
    let dec = gst::ElementFactory::make(dec_name)
        .build()
        .map_err(|e| format!("{dec_name}: {e}"))?;
    // Pin NVDEC to CUDA device 0 so its output device pointer shares the same
    // primary context ORT's CUDA/TRT provider runs on — required for the Stage-4
    // zero-copy detect (a device pointer from another device fails validation and
    // falls back). Harmless on the download path. Guarded: skip if the decoder
    // build lacks the property.
    // Only set when actually settable NOW: on some nvcodec builds `cuda-device-id`
    // exists but is read-only or CONSTRUCT_ONLY (fixed at element creation), and
    // `set_property_from_str` PANICS on a non-writable prop. Skip silently otherwise
    // — nvh264dec then uses its default device (0); a non-device-0 pointer just fails
    // zero-copy validation and falls back to the download path, never crashing.
    if dec
        .find_property("cuda-device-id")
        .map(|p| p.flags())
        .is_some_and(|f| {
            f.contains(gst::glib::ParamFlags::WRITABLE)
                && !f.contains(gst::glib::ParamFlags::CONSTRUCT_ONLY)
        })
    {
        dec.set_property_from_str("cuda-device-id", "0");
    }

    pipeline
        .add_many([&depay, &parse, &dec])
        .map_err(|e| format!("add_many nvdec: {e}"))?;
    gst::Element::link_many([queue_a, &depay, &parse, &dec, downstream])
        .map_err(|e| format!("link nvdec branch: {e}"))?;

    for el in [&depay, &parse, &dec] {
        el.sync_state_with_parent()
            .map_err(|e| format!("sync_state nvdec element: {e}"))?;
    }
    tracing::info!(
        encoding = %encoding,
        decoder = dec_name,
        "rtsp: gałąź NVDEC GPU-resident wpięta (dekod + konwersja kolorów na GPU)"
    );
    Ok(())
}

fn install_frame_callback(
    appsink: &gst_app::AppSink,
    camera_id: String,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) {
    let mailbox_cb = mailbox.clone();
    let counters_cb = counters.clone();
    let camera_id_cb = camera_id;
    let logged_first = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let logged_first_cb = logged_first.clone();
    let camera_id_log = camera_id_cb.clone();
    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let caps = sample.caps().ok_or(gst::FlowError::Error)?;
                let s = caps.structure(0).ok_or(gst::FlowError::Error)?;
                let width: i32 = s.get("width").map_err(|_| gst::FlowError::Error)?;
                let height: i32 = s.get("height").map_err(|_| gst::FlowError::Error)?;
                if !logged_first_cb.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    tracing::info!(
                        camera_id = %camera_id_log,
                        width,
                        height,
                        "rtsp: first frame in appsink callback"
                    );
                }
                let pts_ns = buffer.pts().map(|t| t.nseconds());
                let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                let ts_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                // Single copy: build the shared frame directly from the
                // GStreamer map (no intermediate Vec). `Arc::from(&[u8])`
                // allocates once and memcpy's the slice in place.
                let shared: Arc<[u8]> = Arc::from(map.as_slice());
                let frame_size = shared.len();
                mailbox_cb.put(LatestFrame {
                    width: width as u32,
                    height: height as u32,
                    timestamp_unix_ms: ts_ms,
                    pts_ns,
                    data: shared.clone(),
                    format: super::fakefile::DetectFrameFormat::Rgb24,
                    device: None,
                });
                counters_cb.increment_public(ts_ms / 1000);

                let metadata = FrameMetadata {
                    camera_id: camera_id_cb.clone(),
                    width: width as u32,
                    height: height as u32,
                    pixel_format: FramePixelFormat::Rgb24,
                    timestamp_unix_ms: ts_ms,
                    pts: pts_ns,
                    frame_size_bytes: frame_size,
                };
                let stored = StoredFrame {
                    metadata: metadata.clone(),
                    data: shared,
                    created_at: std::time::Instant::now(),
                };
                let frame_ref = frame_storage().insert(stored);
                streaming_bus().broadcast(&camera_id_cb, frame_ref, metadata);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
}

/// Live handle to an attached fMP4 mux branch. The session keeps one of
/// these in `Option<Mp4BranchState>` while a consumer is subscribed; on
/// detach we walk these elements back to NULL and remove them from the
/// pipeline so the mux state machine resets cleanly for the next attach.
pub(super) struct Mp4BranchState {
    pub(super) tee_src_pad: gst::Pad,
    pub(super) elements: Vec<gst::Element>,
}

/// Wire an `mp4mux` appsink so every fragment buffer is forwarded into the
/// publisher through a `Weak` ref. Shared by the RTSP and webrtc Branch B
/// builders so the publisher contract (init-segment sealing + chunk fan-out)
/// is identical regardless of source. The `Weak` ref keeps the pipeline from
/// pinning the hub-side `Arc` alive past the last subscriber.
pub(super) fn wire_mp4_appsink(
    sink: &gst::Element,
    publisher: &Arc<Mp4StreamPublisher>,
) -> std::result::Result<(), String> {
    let appsink_b = sink
        .clone()
        .downcast::<gst_app::AppSink>()
        .map_err(|_| "appsink_b downcast failed".to_string())?;
    let pub_weak = std::sync::Arc::downgrade(publisher);
    appsink_b.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                let bytes = map.as_slice().to_vec();
                if let Some(pub_arc) = pub_weak.upgrade() {
                    pub_arc.push_chunk(bytes);
                }
                // No-op when publisher has been dropped — the session will
                // soon receive DetachMp4Branch and tear this appsink down.
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    Ok(())
}

/// Odstep klatek kluczowych (`key-int-max`) transkodera x264enc, skalowany do
/// fps zrodla: keyframe co ~2 s osi czasu, w granicach 2..=120 klatek. Dolna
/// granica jest w klatkach odpowiadajacych ~2 s przy 1 fps — granica w
/// wiekszej liczbie klatek wydluzalaby GOP w SEKUNDACH przy niskim fps (np.
/// 10 klatek przy 1 fps = GOP 10 s), a klient przycina bufor MSE wzgledem
/// currentTime i kasowalby keyframe aktywnego GOP (stall). MSE potrzebuje
/// regularnych punktow wejscia.
pub(super) fn transcoder_key_int_max(source_fps: u32) -> u32 {
    (2 * source_fps.max(1)).clamp(2, 120)
}

/// Pad-probe ustalajacy baze PTS publishera: PIERWSZY bufor z ustawionym PTS
/// na podanym padzie ustala `mux_base_pts_ns` i probe sam sie odpina.
fn install_base_pts_probe(pad: gst::Pad, publisher: &Arc<Mp4StreamPublisher>) {
    let pub_weak = std::sync::Arc::downgrade(publisher);
    pad.add_probe(gst::PadProbeType::BUFFER, move |_pad, info| {
        let Some(gst::PadProbeData::Buffer(buffer)) = info.data.as_ref() else {
            return gst::PadProbeReturn::Ok;
        };
        let Some(pts) = buffer.pts() else {
            // Bufor bez PTS — czekamy na kolejny, nie odpinamy jeszcze probe.
            return gst::PadProbeReturn::Ok;
        };
        if let Some(pub_arc) = pub_weak.upgrade() {
            pub_arc.set_base_pts_ns(pts.nseconds());
        }
        gst::PadProbeReturn::Remove
    });
}

/// Pad-probe na src-padzie h264parse (bufory wchodzace do mp4mux): PIERWSZY
/// bufor z ustawionym PTS ustala `mux_base_pts_ns` w publisherze i probe sam
/// sie odpina. To ta sama oś czasu (media-timeline) co PTS appsink Branch A,
/// bo Branch A i B dziela `tee` przed dekodem/muxem — klient odejmuje te baze
/// od PTS detekcji, kotwiczac overlay na wlasciwej klatce MSE. WYLACZNIE dla
/// gałęzi passthrough (bez transkodu) — za x264enc PTS jest juz przesuniety.
pub(super) fn install_mux_base_pts_probe(
    parse: &gst::Element,
    publisher: &Arc<Mp4StreamPublisher>,
) {
    if let Some(parse_src) = parse.static_pad("src") {
        install_base_pts_probe(parse_src, publisher);
    }
}

/// Pad-probe na src-padzie pierwszej kolejki gałęzi B ZA tee (przed
/// depay/dekodem/transkodem): PIERWSZY bufor z ustawionym PTS ustala
/// `mux_base_pts_ns` i probe sam sie odpina. Dla gałęzi TRANSKODUJACYCH
/// (preview RTSP, oba warianty MJPEG) baza musi byc zdjeta na WEJSCIU gałęzi:
/// x264enc przesuwa timestampy o staly offset (ochrona przed ujemnym DTS),
/// wiec baza zdjeta za enkoderem lezy w innej osi czasu niz PTS detekcji
/// (Branch A) i klient w trybie PTS uznaje kazda detekcje za spozniona.
/// PTS pierwszej klatki wchodzacej do gałęzi to oś detekcji, z dokladnoscia
/// do opoznienia enkodera (1-2 klatki).
pub(super) fn install_branch_input_base_pts_probe(
    queue: &gst::Element,
    publisher: &Arc<Mp4StreamPublisher>,
) {
    if let Some(queue_src) = queue.static_pad("src") {
        install_base_pts_probe(queue_src, publisher);
    }
}

/// Build and link Branch B (RTP → rtph264depay → h264parse → mp4mux → appsink)
/// onto the running pipeline. Returns the branch state for later teardown,
/// or `None` if the RTP caps say the stream is not H.264 (HEVC, MJPEG, …).
///
/// The mp4mux output goes through an appsink whose callback forwards each
/// buffer to the supplied publisher via a `Weak` ref — that keeps the
/// pipeline from accidentally pinning the hub-side `Arc` alive past the
/// last subscriber.
fn attach_mp4_branch(
    pipeline: &gst::Pipeline,
    tee: &gst::Element,
    publisher: &Arc<Mp4StreamPublisher>,
) -> std::result::Result<Mp4BranchState, String> {
    // Codec gate: branch B muxer is hardwired for H.264 ES. We probe the RTP
    // caps via the static sink pad on `tee` — by the time the first
    // subscriber subscribes the pipeline is already PLAYING and caps have
    // been negotiated. A missing caps probe means rtspsrc has not produced
    // a pad yet; we treat that as transient and refuse politely.
    let tee_sink = tee
        .static_pad("sink")
        .ok_or_else(|| "tee has no sink pad".to_string())?;
    let caps = tee_sink
        .current_caps()
        .ok_or_else(|| "tee caps not yet negotiated".to_string())?;
    let s = caps
        .structure(0)
        .ok_or_else(|| "empty caps on tee sink".to_string())?;
    let encoding_name: String = s
        .get::<String>("encoding-name")
        .map_err(|e| format!("rtp caps missing encoding-name: {e}"))?;
    if !encoding_name.eq_ignore_ascii_case("H264") {
        return Err(format!(
            "mp4 streaming requires H.264 (rtp encoding={})",
            encoding_name
        ));
    }

    // Kolejka gałęzi B — NIE-leaky: bufory to surowe pakiety RTP, a mux fMP4
    // wymaga kompletnego elementary stream (drop pakietu = uszkodzone AU =
    // artefakty w MSE). Gdy klient nie nadąża, przepełnienie obsługuje
    // broadcast/subscriber_lagged wyżej (po stronie publishera), nie gstreamer.
    let queue_b = gst::ElementFactory::make("queue")
        .property("name", "queue_branch_b")
        .property("max-size-buffers", 120u32)
        .build()
        .map_err(|e| format!("queue_b build: {e}"))?;
    let depay = gst::ElementFactory::make("rtph264depay")
        .build()
        .map_err(|e| format!("rtph264depay build: {e}"))?;
    let parse = gst::ElementFactory::make("h264parse")
        // mp4mux needs AVC sample format (length-prefixed NALUs) — the
        // default byte-stream out of rtph264depay would otherwise force
        // mp4mux to refuse with `not-negotiated`.
        .property_from_str("config-interval", "-1")
        .build()
        .map_err(|e| format!("h264parse build: {e}"))?;
    // `streamable=true` writes ftyp+moov on the first fragment (init segment
    // for MSE) and produces moof+mdat fragments thereafter, without an
    // ending mfra/mvex finalize — exactly what a live MSE consumer needs.
    // `fragment-duration` is in milliseconds; 200 ms strikes a balance
    // between latency and overhead.
    let mux = gst::ElementFactory::make("mp4mux")
        .property("fragment-duration", 200u32)
        .property("streamable", true)
        .build()
        .map_err(|e| format!("mp4mux build: {e}"))?;
    let sink = gst::ElementFactory::make("appsink")
        .property("name", "sink_mp4")
        .property("emit-signals", false)
        // Mux fragments must be drained promptly to keep latency bounded —
        // sync=false lets the appsink consume as fast as the muxer emits.
        .property("sync", false)
        .property("max-buffers", 8u32)
        .property("drop", false)
        .build()
        .map_err(|e| format!("appsink_b build: {e}"))?;

    pipeline
        .add_many([&queue_b, &depay, &parse, &mux, &sink])
        .map_err(|e| format!("add_many branch B: {e}"))?;

    let queue_b_sink = queue_b
        .static_pad("sink")
        .ok_or_else(|| "queue_b sink pad missing".to_string())?;
    gst::Element::link_many([&queue_b, &depay, &parse, &mux, &sink])
        .map_err(|e| format!("link branch B: {e}"))?;

    wire_mp4_appsink(&sink, publisher)?;

    // Rebuild pipeline'u (reconnect) resetuje oś PTS mediów wraz z nowym
    // init-segmentem. Ten sam publisher moze przezyc reconnect, wiec kasujemy
    // stara baze — inaczej `set_base_pts_ns` (ustawia tylko gdy pusto) zostawilby
    // baze z poprzedniej osi i overlay rozjechalby sie po reconnectcie. Po
    // skasowaniu pierwszy bufor tej Branch B ustali baze spojna z init-segmentem.
    publisher.reset_base_pts_ns();

    install_mux_base_pts_probe(&parse, publisher);

    // Bring every new element up to the pipeline's current state so the
    // mux branch starts producing without needing a full pipeline restart.
    for el in [&queue_b, &depay, &parse, &mux, &sink] {
        el.sync_state_with_parent()
            .map_err(|e| format!("sync_state branch B element: {e}"))?;
    }

    // Pad tee linkujemy DOPIERO po aktywacji całej gałęzi. Push tee w okno
    // między linkiem a aktywacją queue_b zwraca FLUSHING, a tee trwale
    // oznacza taki pad jako usunięty i nigdy więcej do niego nie pcha —
    // gałąź wygląda na wpiętą, ale mux nie dostaje ani bajta i init segment
    // nigdy nie powstaje.
    // Gałąź jest już AKTYWNA w pipeline — przy błędzie tego kroku trzeba ją
    // rozebrać (Null + remove), inaczej kolejny attach wywali się na kolizji
    // stałych nazw elementów, a osierocone elementy dalej mieliłyby dane.
    let Some(tee_src_pad) = tee.request_pad_simple("src_%u") else {
        for el in [&queue_b, &depay, &parse, &mux, &sink] {
            let _ = el.set_state(gst::State::Null);
        }
        let _ = pipeline.remove_many([&queue_b, &depay, &parse, &mux, &sink]);
        return Err("tee src_%u request for branch B failed".to_string());
    };
    if let Err(e) = tee_src_pad.link(&queue_b_sink) {
        detach_mp4_branch(
            pipeline,
            tee,
            Mp4BranchState {
                tee_src_pad,
                elements: vec![queue_b, depay, parse, mux, sink],
            },
        );
        return Err(format!("tee → queue_b: {e:?}"));
    }

    // Wymuś natychmiastową klatkę kluczową w GÓRĘ pipeline'u. Nowy widz podpina
    // się do już działającego strumienia — bez tego `mp4mux`/`h264parse` czeka
    // na najbliższy naturalny keyframe (IDR) kamery zanim wyemituje init
    // segment (ftyp+moov+IDR), co daje ~2 s czarnego ekranu w podglądzie MSE.
    // Zdarzenie force-key-unit puszczone upstream na `tee` propaguje się do
    // źródła RTP (rtspsrc) — dla RTSP tłumaczone na RTCP PLI/żądanie IDR;
    // wiele kamer (w tym UniFi) oraz nasz symulator MediaMTX reagują od razu,
    // skracając czas do pierwszej klatki. `all_headers=true` wymusza dołączenie
    // SPS/PPS przy nowym keyframie, dzięki czemu init segment jest kompletny.
    let force_key_unit = gst_video::UpstreamForceKeyUnitEvent::builder()
        .all_headers(true)
        .build();
    if !tee.send_event(force_key_unit) {
        // Nie każde źródło honoruje force-key-unit — to best-effort. Jeśli tee
        // nie połknęło zdarzenia, spróbuj na całym pipeline (trafi do sink pada
        // propagującego upstream). Brak reakcji nie jest błędem: strumień i tak
        // ruszy przy najbliższym naturalnym IDR.
        let fallback = gst_video::UpstreamForceKeyUnitEvent::builder()
            .all_headers(true)
            .build();
        let _ = pipeline.send_event(fallback);
    }

    Ok(Mp4BranchState {
        tee_src_pad,
        elements: vec![queue_b, depay, parse, mux, sink],
    })
}

/// Dowiesza wariant PODGLĄDU gałęzi B: zamiast passthroughu pełnego strumienia
/// źródła (1080p, ~4-8 Mbit/s) transkodujemy do 720p/~1,5 Mbit/s —
///
///   tee → queue_b → rtph264depay → h264parse → avdec_h264 →
///     queue (leaky, gubienie całych klatek) → videoconvert → videoscale →
///     video/x-raw,1280x720 → x264enc (zerolatency/veryfast/1500) →
///     h264parse → mp4mux → appsink
///
/// Kafelki Live view są małe (~600 px), więc pełna jakość marnuje pasmo WAN
/// i głodzi WebSocket detekcji na tym samym łączu. Kontrakt publishera (init
/// segment, fragmenty, baza PTS) jest identyczny jak w gałęzi pełnej jakości
/// — reużywamy `wire_mp4_appsink`; bazę PTS zdejmuje jednak
/// `install_branch_input_base_pts_probe` na WEJŚCIU gałęzi (oś detekcji),
/// bo x264enc przesuwa timestampy za sobą o stały offset.
/// Dekoder jest osobny od Branch A (własny avdec za tee), więc podgląd nie
/// zakłóca ścieżki detekcji. Ta sama bramka kodeka co passthrough: H.264.
fn attach_mp4_branch_preview(
    pipeline: &gst::Pipeline,
    tee: &gst::Element,
    publisher: &Arc<Mp4StreamPublisher>,
    source_fps: u32,
) -> std::result::Result<Mp4BranchState, String> {
    // Bramka kodeka jak w `attach_mp4_branch`: depay jest przykręcony do
    // H.264, inne kodeki (HEVC, MJPEG) odrzucamy zanim ruszy budowa gałęzi.
    let tee_sink = tee
        .static_pad("sink")
        .ok_or_else(|| "tee has no sink pad".to_string())?;
    let caps = tee_sink
        .current_caps()
        .ok_or_else(|| "tee caps not yet negotiated".to_string())?;
    let s = caps
        .structure(0)
        .ok_or_else(|| "empty caps on tee sink".to_string())?;
    let encoding_name: String = s
        .get::<String>("encoding-name")
        .map_err(|e| format!("rtp caps missing encoding-name: {e}"))?;
    if !encoding_name.eq_ignore_ascii_case("H264") {
        return Err(format!(
            "mp4 preview streaming requires H.264 (rtp encoding={})",
            encoding_name
        ));
    }

    // Kolejka przed dekoderem — NIE-leaky: bufory to surowe pakiety RTP, drop
    // psułby access-unity aż do najbliższego IDR. Gubienie przy spiętrzeniu
    // dopiero ZA dekoderem (queue_dec niżej), na pełnych klatkach.
    let queue_b = gst::ElementFactory::make("queue")
        .property("name", "queue_branch_b_preview")
        .property("max-size-buffers", 120u32)
        .build()
        .map_err(|e| format!("queue_b preview build: {e}"))?;
    let depay = gst::ElementFactory::make("rtph264depay")
        .build()
        .map_err(|e| format!("rtph264depay build: {e}"))?;
    let parse_in = gst::ElementFactory::make("h264parse")
        .build()
        .map_err(|e| format!("h264parse (dec) build: {e}"))?;
    let dec = gst::ElementFactory::make("avdec_h264")
        .build()
        .map_err(|e| format!("avdec_h264 build: {e}"))?;
    let queue_dec = build_raw_leaky_queue("queue_decoded_b_preview").map_err(|e| e.to_string())?;
    let convert = gst::ElementFactory::make("videoconvert")
        .build()
        .map_err(|e| format!("videoconvert build: {e}"))?;
    let scale = gst::ElementFactory::make("videoscale")
        .build()
        .map_err(|e| format!("videoscale build: {e}"))?;
    let scale_caps = gst::Caps::builder("video/x-raw")
        .field("width", 1280i32)
        .field("height", 720i32)
        .build();
    let scale_filter = gst::ElementFactory::make("capsfilter")
        .property("caps", &scale_caps)
        .build()
        .map_err(|e| format!("preview capsfilter build: {e}"))?;
    // Transkoder CPU podglądu: zerolatency (bez B-ramek — live), veryfast
    // (niski koszt CPU), 1,5 Mbit/s, keyframe co ~2 s (skalowane do fps
    // źródła) — MSE potrzebuje regularnych punktów wejścia. Te same parametry
    // co transkoder MJPEG, tylko niższy bitrate pod kafelki Live view przez WAN.
    let enc = gst::ElementFactory::make("x264enc")
        .property("bitrate", 1500u32)
        .property("key-int-max", transcoder_key_int_max(source_fps))
        .build()
        .map_err(|e| format!("x264enc build: {e}"))?;
    enc.set_property_from_str("tune", "zerolatency");
    enc.set_property_from_str("speed-preset", "veryfast");
    // mp4mux wymaga AVC (NALU z prefiksem długości) — jak w gałęzi passthrough.
    let parse_out = gst::ElementFactory::make("h264parse")
        .property_from_str("config-interval", "-1")
        .build()
        .map_err(|e| format!("h264parse (mux) build: {e}"))?;
    // Te same parametry fMP4 co passthrough: ftyp+moov na pierwszym fragmencie
    // (init segment MSE), fragmenty moof+mdat co 200 ms.
    let mux = gst::ElementFactory::make("mp4mux")
        .property("fragment-duration", 200u32)
        .property("streamable", true)
        .build()
        .map_err(|e| format!("mp4mux build: {e}"))?;
    let sink = gst::ElementFactory::make("appsink")
        .property("name", "sink_mp4_preview")
        .property("emit-signals", false)
        .property("sync", false)
        .property("max-buffers", 8u32)
        .property("drop", false)
        .build()
        .map_err(|e| format!("appsink_b preview build: {e}"))?;

    let elements = [
        &queue_b,
        &depay,
        &parse_in,
        &dec,
        &queue_dec,
        &convert,
        &scale,
        &scale_filter,
        &enc,
        &parse_out,
        &mux,
        &sink,
    ];
    pipeline
        .add_many(elements)
        .map_err(|e| format!("add_many branch B preview: {e}"))?;

    let queue_b_sink = queue_b
        .static_pad("sink")
        .ok_or_else(|| "queue_b preview sink pad missing".to_string())?;
    gst::Element::link_many(elements).map_err(|e| format!("link branch B preview: {e}"))?;

    wire_mp4_appsink(&sink, publisher)?;

    // Ta sama semantyka resetu bazy PTS co w gałęzi passthrough (rebuild =
    // nowa oś), ale probe stoi na WEJŚCIU gałęzi (src pad queue_b za tee,
    // przed depay/dekodem): x264enc przesuwa timestampy o stały offset, więc
    // baza zdjęta za enkoderem leżałaby w innej osi niż PTS detekcji i klient
    // w trybie PTS nie rysowałby boxów. PTS pierwszej klatki wchodzącej do
    // gałęzi to oś detekcji (Branch A), spójna z media-time pierwszej próbki
    // fragmentu z dokładnością do opóźnienia enkodera (1-2 klatki).
    publisher.reset_base_pts_ns();
    install_branch_input_base_pts_probe(&queue_b, publisher);

    for el in elements {
        el.sync_state_with_parent()
            .map_err(|e| format!("sync_state branch B preview element: {e}"))?;
    }

    // Pad tee linkujemy DOPIERO po aktywacji całej gałęzi. Push tee w okno
    // między linkiem a aktywacją queue_b zwraca FLUSHING, a tee trwale
    // oznacza taki pad jako usunięty i nigdy więcej do niego nie pcha —
    // gałąź wygląda na wpiętą, ale mux nie dostaje ani bajta i init segment
    // nigdy nie powstaje.
    // Gałąź jest już AKTYWNA w pipeline — przy błędzie tego kroku trzeba ją
    // rozebrać (Null + remove), inaczej kolejny attach wywali się na kolizji
    // stałych nazw elementów, a osierocone elementy dalej mieliłyby dane.
    let Some(tee_src_pad) = tee.request_pad_simple("src_%u") else {
        for el in elements {
            let _ = el.set_state(gst::State::Null);
        }
        let _ = pipeline.remove_many(elements);
        return Err("tee src_%u request for branch B preview failed".to_string());
    };
    if let Err(e) = tee_src_pad.link(&queue_b_sink) {
        detach_mp4_branch(
            pipeline,
            tee,
            Mp4BranchState {
                tee_src_pad,
                elements: elements.iter().map(|el| (*el).clone()).collect(),
            },
        );
        return Err(format!("tee → queue_b preview: {e:?}"));
    }

    // Jak w gałęzi passthrough: nowy widz wpina się w działający strumień, a
    // avdec potrzebuje IDR, by zacząć dekodować — bez force-key-unit podgląd
    // czekałby na najbliższy naturalny keyframe kamery (~2 s czarnego ekranu).
    let force_key_unit = gst_video::UpstreamForceKeyUnitEvent::builder()
        .all_headers(true)
        .build();
    if !tee.send_event(force_key_unit) {
        let fallback = gst_video::UpstreamForceKeyUnitEvent::builder()
            .all_headers(true)
            .build();
        let _ = pipeline.send_event(fallback);
    }

    Ok(Mp4BranchState {
        tee_src_pad,
        elements: vec![
            queue_b,
            depay,
            parse_in,
            dec,
            queue_dec,
            convert,
            scale,
            scale_filter,
            enc,
            parse_out,
            mux,
            sink,
        ],
    })
}

/// Walk Branch B's elements back to NULL and remove them from the pipeline.
/// Idempotent: the caller is expected to `.take()` the state once.
pub(super) fn detach_mp4_branch(
    pipeline: &gst::Pipeline,
    tee: &gst::Element,
    state: Mp4BranchState,
) {
    // Unlink the request pad first so the upstream tee stops pushing into
    // a half-disposed branch. `unlink` on an already-unlinked pad is a no-op.
    if let Some(peer) = state.tee_src_pad.peer() {
        let _ = state.tee_src_pad.unlink(&peer);
    }
    for el in &state.elements {
        let _ = el.set_state(gst::State::Null);
    }
    let refs: Vec<&gst::Element> = state.elements.iter().collect();
    if let Err(e) = pipeline.remove_many(refs) {
        tracing::warn!("rtsp: branch B remove_many failed: {e:?}");
    }
    tee.release_request_pad(&state.tee_src_pad);
}

/// Attach the ON-DEMAND RGB streaming branch (Stage 3) off the post-`cudadownload`
/// decode tee (raw NV12 in host memory). Modeled exactly on [`attach_mp4_branch`]:
/// build + add + link + `sync_state_with_parent` the whole branch, THEN request a
/// tee src pad and link it last (a tee pad linked before the branch is active gets
/// permanently marked flushing). Branch:
///   `tee_decode → queue(leaky) → videoconvert(NV12→RGB) → capsfilter(RGB) → appsink`
/// The appsink feeds `FrameStorage` + `StreamingBus` (the raw-frame consumers),
/// so this single per-frame full videoconvert runs ONLY while a viewer is
/// subscribed — steady state (no viewer) does zero full converts. Reuses
/// [`Mp4BranchState`] + [`detach_mp4_branch`] for teardown (same shape).
fn attach_rgb_branch(
    pipeline: &gst::Pipeline,
    decode_tee: &gst::Element,
    camera_id: &str,
) -> std::result::Result<Mp4BranchState, String> {
    let queue = build_raw_leaky_queue("queue_rgb_stream").map_err(|e| e.to_string())?;
    let convert = gst::ElementFactory::make("videoconvert")
        .build()
        .map_err(|e| format!("videoconvert rgb branch: {e}"))?;
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "RGB")
        .build();
    let capsfilter = gst::ElementFactory::make("capsfilter")
        .property("caps", &caps)
        .build()
        .map_err(|e| format!("capsfilter rgb branch: {e}"))?;
    let sink = gst::ElementFactory::make("appsink")
        .property("name", "sink_rgb_stream")
        .property("emit-signals", false)
        .property("sync", false)
        .property("max-buffers", 1u32)
        .property("drop", true)
        .build()
        .map_err(|e| format!("appsink rgb branch: {e}"))?;
    let sink_app = sink
        .clone()
        .downcast::<gst_app::AppSink>()
        .map_err(|_| "rgb branch appsink downcast failed".to_string())?;
    super::fakefile::install_rgb_stream_callback(&sink_app, camera_id.to_string());

    let elements = [&queue, &convert, &capsfilter, &sink];
    pipeline
        .add_many(elements)
        .map_err(|e| format!("add_many rgb branch: {e}"))?;
    gst::Element::link_many(elements).map_err(|e| format!("link rgb branch: {e}"))?;
    for el in elements {
        el.sync_state_with_parent()
            .map_err(|e| format!("sync_state rgb branch element: {e}"))?;
    }

    // Link the tee pad LAST (after the branch is active) — same rationale as
    // Branch B. On failure tear the just-activated branch down cleanly so a later
    // attach does not collide on the fixed element names.
    let queue_sink = queue
        .static_pad("sink")
        .ok_or_else(|| "rgb branch queue sink pad missing".to_string())?;
    let Some(tee_src_pad) = decode_tee.request_pad_simple("src_%u") else {
        for el in elements {
            let _ = el.set_state(gst::State::Null);
        }
        let _ = pipeline.remove_many(elements);
        return Err("tee_decode src_%u request for rgb branch failed".to_string());
    };
    if let Err(e) = tee_src_pad.link(&queue_sink) {
        detach_mp4_branch(
            pipeline,
            decode_tee,
            Mp4BranchState {
                tee_src_pad,
                elements: vec![queue, convert, capsfilter, sink],
            },
        );
        return Err(format!("tee_decode → rgb queue: {e:?}"));
    }

    Ok(Mp4BranchState {
        tee_src_pad,
        elements: vec![queue, convert, capsfilter, sink],
    })
}

/// On-demand RGB streaming branch for the ZERO-COPY CROPS path. Identical to
/// [`attach_rgb_branch`] but starts from the CUDA-memory tee (`tee_cuda`), so it
/// carries its OWN `cudadownload` (device NV12 → host) before the NV12→RGB
/// convert:
///   `tee_cuda → queue(leaky) → cudadownload → videoconvert(NV12→RGB)
///     → capsfilter(RGB) → appsink`
/// The full 4K download therefore runs ONLY while a viewer is subscribed — steady
/// state (no viewer) does zero downloads AND zero converts, which is the whole
/// point of the zero-copy crops path. Reuses [`Mp4BranchState`] +
/// [`detach_mp4_branch`] for teardown (same shape).
fn attach_rgb_branch_cuda(
    pipeline: &gst::Pipeline,
    tee_cuda: &gst::Element,
    camera_id: &str,
) -> std::result::Result<Mp4BranchState, String> {
    let queue = build_raw_leaky_queue("queue_rgb_stream_cuda").map_err(|e| e.to_string())?;
    let cudadownload = gst::ElementFactory::make("cudadownload")
        .build()
        .map_err(|e| format!("cudadownload rgb branch: {e}"))?;
    let convert = gst::ElementFactory::make("videoconvert")
        .build()
        .map_err(|e| format!("videoconvert rgb branch (cuda): {e}"))?;
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "RGB")
        .build();
    let capsfilter = gst::ElementFactory::make("capsfilter")
        .property("caps", &caps)
        .build()
        .map_err(|e| format!("capsfilter rgb branch (cuda): {e}"))?;
    let sink = gst::ElementFactory::make("appsink")
        .property("name", "sink_rgb_stream")
        .property("emit-signals", false)
        .property("sync", false)
        .property("max-buffers", 1u32)
        .property("drop", true)
        .build()
        .map_err(|e| format!("appsink rgb branch (cuda): {e}"))?;
    let sink_app = sink
        .clone()
        .downcast::<gst_app::AppSink>()
        .map_err(|_| "rgb branch (cuda) appsink downcast failed".to_string())?;
    super::fakefile::install_rgb_stream_callback(&sink_app, camera_id.to_string());

    let elements = [&queue, &cudadownload, &convert, &capsfilter, &sink];
    pipeline
        .add_many(elements)
        .map_err(|e| format!("add_many rgb branch (cuda): {e}"))?;
    gst::Element::link_many(elements).map_err(|e| format!("link rgb branch (cuda): {e}"))?;
    for el in elements {
        el.sync_state_with_parent()
            .map_err(|e| format!("sync_state rgb branch (cuda) element: {e}"))?;
    }

    let queue_sink = queue
        .static_pad("sink")
        .ok_or_else(|| "rgb branch (cuda) queue sink pad missing".to_string())?;
    let Some(tee_src_pad) = tee_cuda.request_pad_simple("src_%u") else {
        for el in elements {
            let _ = el.set_state(gst::State::Null);
        }
        let _ = pipeline.remove_many(elements);
        return Err("tee_cuda src_%u request for rgb branch failed".to_string());
    };
    if let Err(e) = tee_src_pad.link(&queue_sink) {
        detach_mp4_branch(
            pipeline,
            tee_cuda,
            Mp4BranchState {
                tee_src_pad,
                elements: vec![queue, cudadownload, convert, capsfilter, sink],
            },
        );
        return Err(format!("tee_cuda → rgb queue: {e:?}"));
    }

    Ok(Mp4BranchState {
        tee_src_pad,
        elements: vec![queue, cudadownload, convert, capsfilter, sink],
    })
}

/// Zamyka strumień fMP4 aktywnej gałęzi B przy rozbiórce pipeline'u (reconnect,
/// restart, stop). Protokół WS niesie `base_pts_ns` WYŁĄCZNIE w
/// `StreamSubscribeResponse`, więc po rebuildzie pipeline'u nie da się dostarczyć
/// świeżej bazy PTS już podłączonym subskrybentom — zamiast tego zamykamy ich
/// strumień (`mark_unsupported` → odbiorcy broadcastu widzą `Closed`, warstwa WS
/// wysyła `Closed(reason=source_unregistered)`), a frontend robi resubscribe:
/// nowy attach gałęzi B da świeży init segment i świeżą bazę PTS w nowym
/// `SubscribeResponse`. Bez tego klient wisiałby na wiecznie pustym strumieniu,
/// a detekcje byłyby odrzucane przez starą bazę PTS.
fn close_mp4_stream_on_teardown(
    cam_id: &str,
    publisher: &Option<std::sync::Weak<Mp4StreamPublisher>>,
) {
    if let Some(pub_) = publisher.as_ref().and_then(|w| w.upgrade()) {
        tracing::info!(
            camera_id = %cam_id,
            "rtsp: pipeline rozebrany — zamykam strumień fMP4 gałęzi B; subskrybenci \
             wykonają resubscribe i dostaną świeży init segment + bazę PTS"
        );
        pub_.mark_unsupported();
    }
}

/// Entry point invoked by `spawn_session` for `vendor='rtsp'`. Drives the
/// reconnect loop, owns the active pipeline, and translates control messages
/// and bus events into health updates. Exits cleanly on
/// `SessionCommand::Stop` or when the cancel signal fires.
pub async fn run_rtsp_session(
    mut config: CameraConfig,
    policy: ReconnectPolicy,
    mut cmd_rx: mpsc::Receiver<SessionCommand>,
    health_tx: watch::Sender<CameraHealth>,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) {
    let cam_id = config.camera_id.clone();
    let timeout_secs = 10u32;

    // Sesja obsługuje dwa vendory: `rtsp` (rtspsrc + dekod) i `mjpeg`
    // (souphttpsrc + multipartdemux + jpegdec). Pętla reconnect/health jest
    // wspólna; różni się tylko builder pipeline'u i attach gałęzi B.
    let is_mjpeg = config.vendor == "mjpeg";

    // Ścieżka ingestu bieżącej próby. Start wg `resolve_ingest_path`:
    // preferuje GPU-resident NVIDIA (dekod + konwersja kolorów na GPU), gdy
    // sprzęt i elementy CUDA są obecne; inaczej CPU (decodebin). Po nieudanej
    // negocjacji GPU-resident (pipeline pada zanim wejdzie Online) schodzimy na
    // CPU i zostajemy tam do końca życia sesji — „musi działać". MJPEG dekoduje
    // zawsze na CPU (jpegdec) — ścieżki GPU/HW go nie dotyczą.
    let mut ingest_path = if is_mjpeg {
        IngestPath::Cpu
    } else {
        resolve_ingest_path(&config)
    };
    // The path this session PREFERS. When the session ends up ONLINE but on a
    // lower rung (a camera recovering from RTSP-session stress delivers its
    // first frames too slowly for the warmup window and lands on CPU decode),
    // one automatic upgrade reconnect is attempted after the stream has been
    // stable for PATH_UPGRADE_AFTER — by then the camera is warm and the
    // preferred-path warmup succeeds where the cold start didn't. Without this
    // a degraded session stays on software decode until an unrelated reconnect.
    let preferred_path = ingest_path;
    let mut path_upgrade_attempted = false;
    const PATH_UPGRADE_AFTER: Duration = Duration::from_secs(300);
    // Czy w wariancie CPU pozwolić decodebinowi autoplugować dekoder sprzętowy
    // (best-effort). Nieużywane dla ścieżki GPU-resident. Po nieudanej
    // negocjacji HW w wariancie CPU schodzimy na dekodowanie programowe.
    let mut use_hw_decode = if is_mjpeg {
        false
    } else {
        resolve_use_hw_decode(&config)
    };
    // Czy dobudować gałąź GPU-owego skalowania klatki detekcji (4K→560 na GPU,
    // zdejmuje ~4 ms resize'u z detektora). Start wg `gpu_resize_enabled`
    // (elementy CUDA obecne + brak `[vision] gpu_resize = false`). Po
    // nieudanej negocjacji CUDA przy Playing schodzimy na pojedynczy appsink
    // (detektor resize'uje na CPU) i zostajemy tam do końca sesji — „musi
    // działać". Dotyczy MJPEG i RTSP jednakowo (skalowanie działa też przy
    // dekodzie CPU).
    let mut gpu_resize = gpu_resize_enabled();
    tracing::info!(
        camera_id = %cam_id,
        path = ingest_path.label(),
        gpu_resize,
        "rtsp: wybrana ścieżka ingestu (fallback na CPU przy błędzie negocjacji)"
    );

    // Connection attempt counter — reset when we successfully reach Online.
    let mut attempt: u32 = 0;
    let mut backoff = policy.initial_backoff;
    // Sticky last failure reason — survives across health ticks while the
    // pipeline is in pre-Online state. Cleared when first frame arrives.
    // Without this the UI sees `status_message = None` 99% of the time
    // because health ticks publish every second but failures publish once.
    let mut last_error: Option<String> = None;

    publish(
        &health_tx,
        &cam_id,
        CameraStatus::Starting,
        Some("łączenie z kamerą…".to_string()),
        &counters,
        None,
    );
    // Initial sticky reason — overwritten on first failure with the actual
    // GStreamer error. Without this the UI sees an empty `Komunikat` between
    // T=0 (pipeline build) and T=warmup_deadline (first failure publish),
    // which can be 5–30 seconds while user thinks nothing is happening.
    last_error = Some("łączenie z kamerą…".to_string());

    'outer: loop {
        // Resolve credentials at every (re)build so an in-flight
        // `camera_credentials_rotate_v1` takes effect on the next reconnect
        // without us holding a stale plaintext across iterations. RTSP nakłada
        // `user:pass` na URL (rtspsrc); MJPEG zostawia URL bez zmian i podaje
        // parę poświadczeń do souphttpsrc (`user-id`/`user-pw`).
        let resolved = if is_mjpeg {
            super::mjpeg::resolve_http_credentials(&config).map(|c| (config.url.clone(), c))
        } else {
            resolve_pipeline_url(&config).map(|u| (u, None))
        };
        let (final_url, http_creds) = match resolved {
            Ok(v) => v,
            Err(e) => {
                let reason = redact_url_in_text(&format!("creds: {e}"));
                publish(
                    &health_tx,
                    &cam_id,
                    CameraStatus::Error,
                    Some(reason.clone()),
                    &counters,
                    None,
                );
                streaming_bus().close_camera(&cam_id, &reason).await;
                drain_until_stop(&mut cmd_rx, &health_tx).await;
                return;
            }
        };
        tracing::info!(
            camera_id = %cam_id,
            attempt = attempt,
            path = ingest_path.label(),
            "rtsp: building pipeline"
        );
        let build_result = if is_mjpeg {
            super::mjpeg::build_mjpeg_pipeline(
                cam_id.clone(),
                &final_url,
                http_creds.as_ref(),
                timeout_secs,
                gpu_resize,
                mailbox.clone(),
                counters.clone(),
            )
        } else {
            build_rtsp_pipeline(
                cam_id.clone(),
                &final_url,
                timeout_secs,
                ingest_path,
                use_hw_decode,
                gpu_resize,
                mailbox.clone(),
                counters.clone(),
            )
        };
        let handles = match build_result {
            Ok(h) => h,
            Err(e) => {
                let reason = redact_url_in_text(&format!("build failed: {e}"));
                // Fallback budowy: najpierw zdejmij gałąź GPU-resize (najświeższe
                // ryzyko — element CUDA zniknął / caps), potem ewentualnie
                // GPU-resident → CPU. Rozbieramy stopniowo do „zawsze działa".
                if gpu_resize {
                    tracing::warn!(
                        camera_id = %cam_id,
                        reason = %reason,
                        "rtsp: budowa gałęzi GPU-resize nie powiodła się — wyłączam GPU-resize (CPU resize w detektorze)"
                    );
                    gpu_resize = false;
                    continue 'outer;
                }
                // Fallback budowy GPU (GPU-resident lub NVDEC) → niższa ścieżka.
                // Gdy wariant sprzętowy nie zbuduje się (np. element CUDA/NVDEC
                // zniknął z rejestru), schodzimy o jeden szczebel
                // (`degrade_ingest_path`: NvdecNv12 → NvdecCpuConvert → Cpu)
                // zamiast kończyć sesję błędem.
                if matches!(
                    ingest_path,
                    IngestPath::GpuResidentNvidia
                        | IngestPath::NvdecNv12
                        | IngestPath::NvdecCpuConvert
                ) {
                    let next = degrade_ingest_path(ingest_path);
                    tracing::warn!(
                        camera_id = %cam_id,
                        reason = %reason,
                        path = ingest_path.label(),
                        next = next.label(),
                        "rtsp: budowa pipeline GPU nie powiodła się — schodzę o szczebel niżej"
                    );
                    ingest_path = next;
                    continue 'outer;
                }
                tracing::error!(camera_id = %cam_id, reason = %reason, "rtsp: pipeline build failed");
                publish(
                    &health_tx,
                    &cam_id,
                    CameraStatus::Error,
                    Some(reason.clone()),
                    &counters,
                    None,
                );
                streaming_bus().close_camera(&cam_id, &reason).await;
                drain_until_stop(&mut cmd_rx, &health_tx).await;
                return;
            }
        };
        let pipeline = &handles.pipeline;
        let tee = handles.tee.clone();
        // On-demand RGB streaming branch (Stage 3): present only on the
        // GPU-resident NV12 crops path (`decode_tee` is `Some`). Attached in the
        // tick when a raw-frame subscriber appears, detached when the last one
        // leaves — so the full NV12→RGB videoconvert runs ONLY while watched. Dies
        // with the pipeline on teardown/reconnect (no explicit teardown needed,
        // like Branch B's elements).
        let decode_tee = handles.decode_tee.clone();
        // Whether `decode_tee` is the CUDA-memory tee (zero-copy crops): the
        // on-demand RGB attach then inserts its own `cudadownload`.
        let decode_tee_is_cuda = handles.decode_tee_is_cuda;
        let mut rgb_branch: Option<Mp4BranchState> = None;
        // Branch B mux state — `Some` whenever a consumer is subscribed.
        // Dwa niezależne sloty na tym samym tee: pełna jakość (passthrough,
        // klucz hubu `camera:<id>`) i podgląd (transkod 720p, klucz
        // `camera:<id>#preview`) — dwóch subskrybentów z różnymi wariantami to
        // dwóch publisherów i dwie gałęzie. Gałąź B NIE przeżywa rebuildu
        // pipeline'u: przy rozbiórce zamykamy strumień publishera
        // (`close_mp4_stream_on_teardown`), subskrybenci dostają
        // `Closed(source_unregistered)` i robią resubscribe — świeży attach na
        // nowym pipeline daje nowy init segment i nową bazę PTS.
        let mut branch_b_full: Option<Mp4BranchState> = None;
        let mut branch_b_preview: Option<Mp4BranchState> = None;
        // Słabe referencje do publisherów aktualnie wpiętych gałęzi B. Służą do:
        // (a) zamknięcia strumieni przy rozbiórce pipeline'u, (b) ignorowania
        // przeterminowanych `DetachMp4Branch` od STARYCH publisherów (komenda
        // niesie tylko wariant, nie tożsamość nadawcy), które inaczej zrywałyby
        // świeżą gałąź tego wariantu.
        let mut branch_b_full_publisher: Option<std::sync::Weak<Mp4StreamPublisher>> = None;
        let mut branch_b_preview_publisher: Option<std::sync::Weak<Mp4StreamPublisher>> = None;

        tracing::info!(camera_id = %cam_id, "rtsp: setting pipeline state -> Playing");
        if let Err(e) = super::session::set_state_blocking(pipeline, gst::State::Playing).await {
            let raw_reason = format!("set_state(Playing) failed: {e}");
            let reason = redact_url_in_text(&raw_reason);
            tracing::error!(camera_id = %cam_id, reason = %reason, "rtsp: set_state Playing failed");
            let _ = super::session::set_state_blocking(pipeline, gst::State::Null).await;
            // Gałąź GPU-resize padła przy przejściu do Playing (negocjacja caps
            // cudascale) — najpierw ją zdejmujemy (bez backoffu). Detekcja dalej
            // działa: mailbox `get_detect` zwraca wtedy klatkę crops i detektor
            // resize'uje na CPU. Rozbieramy najświeższe ryzyko przed dekoderem.
            if gpu_resize {
                tracing::warn!(
                    camera_id = %cam_id,
                    reason = %reason,
                    "rtsp: gałąź GPU-resize zawiodła przy starcie (set_state) — wyłączam GPU-resize (CPU resize w detektorze)"
                );
                gpu_resize = false;
                streaming_bus().close_camera(&cam_id, &reason).await;
                backoff = policy.initial_backoff;
                continue 'outer;
            }
            // Ścieżka GPU (GPU-resident lub NVDEC) padła już przy przejściu do
            // Playing — schodzimy o jeden szczebel (NvdecNv12 → NvdecCpuConvert
            // → Cpu) bez backoffu.
            if matches!(
                ingest_path,
                IngestPath::GpuResidentNvidia | IngestPath::NvdecNv12 | IngestPath::NvdecCpuConvert
            ) {
                let next = degrade_ingest_path(ingest_path);
                tracing::warn!(
                    camera_id = %cam_id,
                    reason = %reason,
                    path = ingest_path.label(),
                    next = next.label(),
                    "rtsp: ścieżka GPU zawiodła przy starcie (set_state) — schodzę o szczebel niżej"
                );
                ingest_path = next;
                streaming_bus().close_camera(&cam_id, &reason).await;
                backoff = policy.initial_backoff;
                continue 'outer;
            }
            // Próba HW padła już przy przejściu do Playing — natychmiast
            // schodzimy na dekodowanie programowe (bez backoffu), zamiast
            // wpadać w pętlę reconnectów na niedziałającym dekoderze sprzętowym.
            if use_hw_decode {
                tracing::warn!(
                    camera_id = %cam_id,
                    reason = %reason,
                    "rtsp: dekodowanie sprzętowe zawiodło przy starcie (set_state) — przełączam na programowe (CPU)"
                );
                use_hw_decode = false;
                streaming_bus().close_camera(&cam_id, &reason).await;
                backoff = policy.initial_backoff;
                continue 'outer;
            }
            streaming_bus().close_camera(&cam_id, &reason).await;
            // A pure state-set failure is recoverable in principle, but it
            // usually means a misconfigured element — fall into the
            // reconnect path so the operator's intervention (e.g. fixing
            // the URL) is observed without a process restart.
            attempt = attempt.saturating_add(1);
            if reached_max(&policy, attempt) {
                publish(
                    &health_tx,
                    &cam_id,
                    CameraStatus::Error,
                    Some(format!("max reconnect attempts exceeded: {reason}")),
                    &counters,
                    None,
                );
                drain_until_stop(&mut cmd_rx, &health_tx).await;
                return;
            }
            let wait = jittered(&policy, backoff);
            publish(
                &health_tx,
                &cam_id,
                CameraStatus::Starting,
                Some(format!("reconnect attempt {attempt} in {wait:?}: {reason}")),
                &counters,
                None,
            );
            if !sleep_with_cancel(&mut cmd_rx, &health_tx, wait, &mut config).await {
                return;
            }
            backoff = next_backoff(backoff, policy.max_backoff);
            continue 'outer;
        }

        let bus = pipeline.bus().expect("pipeline has bus");
        let mut online = false;
        let mut online_at: Option<tokio::time::Instant> = None;
        let mut last_total: u64 = 0;
        let mut fps_window: std::collections::VecDeque<f32> =
            std::collections::VecDeque::with_capacity(30);
        let started_at = tokio::time::Instant::now();
        // `[vision] warmup_extra_secs` (default 20): grace past the connect
        // timeout for FIRST frames before the path degrades a rung — a camera
        // recovering from RTSP-session stress starts delivering slowly, and too
        // little patience here drops a healthy NVDEC path to software decode.
        let warmup_extra = crate::vision::settings::get().warmup_extra_secs as u64;
        let warmup_deadline = started_at + Duration::from_secs(timeout_secs as u64 + warmup_extra);
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        // Tracks whether the inner loop exited because of an operator-driven
        // restart (e.g. credentials rotation). On restart we want to skip
        // the reconnect backoff and try the new config immediately.
        let mut restart_requested = false;
        // Inner loop owns the running pipeline. Terminate it by `break` →
        // outer reconnects; or `return` for a final stop.
        let inner_reason: Option<String> = loop {
            tokio::select! {
                biased;
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(SessionCommand::Stop) | None => {
                            publish(
                                &health_tx,
                                &cam_id,
                                CameraStatus::Stopping,
                                None,
                                &counters,
                                fps_window.back().copied(),
                            );
                            let _ = super::session::set_state_blocking(pipeline, gst::State::Null).await;
                            close_mp4_stream_on_teardown(&cam_id, &branch_b_full_publisher);
                            close_mp4_stream_on_teardown(&cam_id, &branch_b_preview_publisher);
                            publish(&health_tx, &cam_id, CameraStatus::Offline, None, &counters, None);
                            streaming_bus().close_camera(&cam_id, "stopped").await;
                            return;
                        }
                        Some(SessionCommand::UpdateConfig(_)) => {
                            // Hot reconfigure not yet implemented for RTSP —
                            // operator must remove+re-add the camera.
                        }
                        Some(SessionCommand::Restart(new_config)) => {
                            // Credentials rotation (or other supervisor-driven
                            // restart) — swap config and rebuild the pipeline
                            // immediately. The new config carries the freshly
                            // encrypted credentials blob the rotate-v1 host
                            // function just persisted.
                            config = new_config;
                            restart_requested = true;
                            break None;
                        }
                        Some(SessionCommand::GetHealth(reply)) => {
                            let _ = reply.send(health_tx.borrow().clone());
                        }
                        Some(SessionCommand::Snapshot(reply)) => {
                            let deadline = tokio::time::Instant::now() + Duration::from_millis(4500);
                            let snap = loop {
                                if let Some(f) = mailbox.get() {
                                    // Detect frame (Arc-shared) rides alongside
                                    // the crops frame in ONE round-trip; falls
                                    // back to crops when the GPU detect branch is
                                    // absent, so detection always has an input.
                                    break Ok(SnapshotData {
                                        camera_id: cam_id.clone(),
                                        width: f.width,
                                        height: f.height,
                                        pixel_format: PixelFormat::Rgb24,
                                        timestamp_unix_ms: f.timestamp_unix_ms,
                                        pts_ns: f.pts_ns,
                                        data: f.data.to_vec(),
                                        crops_format: f.format,
                                        detect: mailbox.get_detect(),
                                        // Zero-copy crops: pass the device handle
                                        // through without downloading (analysis
                                        // crops per-detection; `rgb_data`
                                        // downloads full only on a UI snapshot).
                                        crops_device: f.device.clone(),
                                    });
                                }
                                let h = health_tx.borrow().clone();
                                if matches!(h.status, CameraStatus::Error) {
                                    break Err(CameraIngestError::SnapshotFailed(
                                        h.status_message.unwrap_or_else(|| "session error".into()),
                                    ));
                                }
                                if tokio::time::Instant::now() >= deadline {
                                    break Err(CameraIngestError::SnapshotTimeout);
                                }
                                tokio::time::sleep(Duration::from_millis(50)).await;
                            };
                            let _ = reply.send(snap);
                        }
                        Some(SessionCommand::AttachMp4Branch(publisher)) => {
                            // Wariant publishera wybiera slot: pełna jakość i
                            // podgląd to dwie niezależne gałęzie na tym samym tee.
                            let preview = publisher.is_preview();
                            let slot = if preview { &mut branch_b_preview } else { &mut branch_b_full };
                            if slot.is_some() {
                                // Already attached for a previous subscriber.
                                // The hub guarantees only one publisher per
                                // stream id at a time, so this is a stray
                                // signal — fail the new publisher cleanly.
                                tracing::debug!(
                                    camera_id = %cam_id,
                                    preview,
                                    "rtsp: attach_mp4_branch ignored — branch already active"
                                );
                                publisher.mark_unsupported();
                            } else {
                                // MJPEG transkoduje JPEG → H.264 (x264enc);
                                // RTSP w pełnej jakości przepakowuje H.264 z RTP
                                // bez rekompresji, a w podglądzie transkoduje do
                                // 720p/1,5 Mbit/s (kafelki Live view przez WAN).
                                let attach_result = if is_mjpeg {
                                    super::mjpeg::attach_mp4_branch_mjpeg(pipeline, &tee, &publisher, preview, config.target_fps)
                                } else if preview {
                                    attach_mp4_branch_preview(pipeline, &tee, &publisher, config.target_fps)
                                } else {
                                    attach_mp4_branch(pipeline, &tee, &publisher)
                                };
                                match attach_result {
                                    Ok(state) => {
                                        tracing::info!(
                                            camera_id = %cam_id,
                                            preview,
                                            "rtsp: branch B attached (fMP4 mux)"
                                        );
                                        *slot = Some(state);
                                        let weak = Some(std::sync::Arc::downgrade(&publisher));
                                        if preview {
                                            branch_b_preview_publisher = weak;
                                        } else {
                                            branch_b_full_publisher = weak;
                                        }
                                    }
                                    Err(reason) => {
                                        tracing::warn!(
                                            camera_id = %cam_id,
                                            preview,
                                            reason = %reason,
                                            "rtsp: branch B attach refused"
                                        );
                                        publisher.mark_unsupported();
                                    }
                                }
                            }
                        }
                        Some(SessionCommand::DetachMp4Branch { preview }) => {
                            // Komenda niesie tylko wariant, nie tożsamość nadawcy.
                            // Gdy publisher aktualnie wpiętej gałęzi tego wariantu
                            // wciąż żyje (hub/subskrybenci trzymają Arc), detach
                            // pochodzi od STAREGO publishera (np. Drop po zamknięciu
                            // strumienia przy reconnectcie) — ignorujemy go, żeby
                            // nie zrywać świeżej gałęzi.
                            let (slot, slot_publisher) = if preview {
                                (&mut branch_b_preview, &mut branch_b_preview_publisher)
                            } else {
                                (&mut branch_b_full, &mut branch_b_full_publisher)
                            };
                            let current_alive = slot_publisher
                                .as_ref()
                                .is_some_and(|w| w.upgrade().is_some());
                            if current_alive {
                                tracing::debug!(
                                    camera_id = %cam_id,
                                    preview,
                                    "rtsp: ignoruję przeterminowany DetachMp4Branch — \
                                     aktualny publisher gałęzi B wciąż żyje"
                                );
                            } else if let Some(state) = slot.take() {
                                detach_mp4_branch(pipeline, &tee, state);
                                *slot_publisher = None;
                                tracing::info!(
                                    camera_id = %cam_id,
                                    preview,
                                    "rtsp: branch B detached"
                                );
                            }
                        }
                    }
                }
                _ = tick.tick() => {
                    let mut terminate: Option<String> = None;
                    while let Some(msg) = bus.pop() {
                        use gst::MessageView;
                        match msg.view() {
                            MessageView::Eos(_) => {
                                terminate = Some("eos".into());
                                break;
                            }
                            MessageView::Error(err) => {
                                let raw = format!(
                                    "{} ({})",
                                    err.error(),
                                    err.debug().unwrap_or_default()
                                );
                                terminate = Some(redact_url_in_text(&raw));
                                break;
                            }
                            _ => {}
                        }
                    }
                    if let Some(reason) = terminate {
                        break Some(reason);
                    }

                    // On-demand RGB branch (Stage 3, NV12 crops path only): attach
                    // when a raw-frame subscriber appears, detach when the last one
                    // leaves. Steady state (no viewer) = zero full videoconvert; the
                    // analysis path stays NV12 regardless.
                    if let Some(decode_tee) = decode_tee.as_ref() {
                        let watched = !streaming_bus().list_subscribers(&cam_id).is_empty();
                        if watched && rgb_branch.is_none() {
                            // Zero-copy crops: the on-demand branch carries its own
                            // `cudadownload` off the CUDA tee. Host NV12 crops path:
                            // convert straight off the post-download tee.
                            let attached = if decode_tee_is_cuda {
                                attach_rgb_branch_cuda(pipeline, decode_tee, &cam_id)
                            } else {
                                attach_rgb_branch(pipeline, decode_tee, &cam_id)
                            };
                            match attached {
                                Ok(state) => {
                                    rgb_branch = Some(state);
                                    tracing::info!(camera_id = %cam_id, "rtsp: on-demand RGB branch attached (viewer subscribed)");
                                }
                                Err(e) => tracing::warn!(camera_id = %cam_id, error = %e, "rtsp: on-demand RGB branch attach failed"),
                            }
                        } else if !watched {
                            if let Some(state) = rgb_branch.take() {
                                detach_mp4_branch(pipeline, decode_tee, state);
                                tracing::info!(camera_id = %cam_id, "rtsp: on-demand RGB branch detached (no viewers)");
                            }
                        }
                    }

                    let (total, dropped, last_at) = counters.snapshot();
                    let delta = total.saturating_sub(last_total) as f32;
                    last_total = total;
                    if fps_window.len() == 30 {
                        fps_window.pop_front();
                    }
                    fps_window.push_back(delta);
                    let avg = if fps_window.is_empty() {
                        None
                    } else {
                        Some(fps_window.iter().sum::<f32>() / fps_window.len() as f32)
                    };

                    if !online {
                        if total > 0 {
                            online = true;
                            online_at = Some(tokio::time::Instant::now());
                            // Successful connect — clear backoff state so the
                            // next disconnect starts the schedule fresh.
                            attempt = 0;
                            backoff = policy.initial_backoff;
                            last_error = None;
                            tracing::info!(
                                camera_id = %cam_id,
                                total,
                                "rtsp: camera ONLINE — first frames flowing"
                            );
                        } else if tokio::time::Instant::now() >= warmup_deadline {
                            break Some("no frames within warmup window".into());
                        }
                    } else if !path_upgrade_attempted
                        && ingest_path != preferred_path
                        && online_at.is_some_and(|t| t.elapsed() >= PATH_UPGRADE_AFTER)
                    {
                        // Self-heal a degraded path: stable ONLINE on a lower rung →
                        // one reconnect at the preferred path this session. The
                        // camera is warm now, so the preferred-path warmup gets
                        // frames immediately; if it still fails, the normal degrade
                        // cascade brings the stream back and no further upgrade is
                        // attempted until the next session.
                        path_upgrade_attempted = true;
                        ingest_path = preferred_path;
                        tracing::info!(
                            camera_id = %cam_id,
                            path = preferred_path.label(),
                            "rtsp: stream stable on a degraded path — retrying the preferred path"
                        );
                        break Some("upgrading back to the preferred ingest path".into());
                    }

                    // Mid-session stall watchdog: a pipeline can stop delivering
                    // frames WITHOUT any bus error (camera stops sending RTP, a
                    // branch wedges) — before this check the session stayed
                    // "ONLINE" with a black tile forever. 10 s without a frame on
                    // a continuous RTSP stream means dead: reconnect through the
                    // normal path (the warmup/degrade cascade takes it from there).
                    if online {
                        let now_unix = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        if let Some(t) = last_at {
                            if now_unix.saturating_sub(t) > 10 {
                                tracing::warn!(
                                    camera_id = %cam_id,
                                    stalled_for_s = now_unix.saturating_sub(t),
                                    "rtsp: stream stalled (frames stopped) — reconnecting"
                                );
                                break Some("stream stalled — no frames for 10 s".into());
                            }
                        }
                    }

                    let status = if online {
                        CameraStatus::Online
                    } else {
                        CameraStatus::Starting
                    };
                    // Keep sticky last_error visible while still warming up so
                    // the UI shows the actual failure reason instead of empty.
                    let msg = if online { None } else { last_error.clone() };
                    let _ = health_tx.send(CameraHealth {
                        camera_id: cam_id.clone(),
                        status,
                        status_message: msg,
                        fps_actual: avg,
                        last_frame_at: last_at,
                        frames_total: total,
                        frames_dropped: dropped,
                    });
                }
            }
        };

        // Pipeline tear-down (either operator-driven restart or pipeline
        // failure). On a restart we skip backoff entirely and reset the
        // attempt counter so the new credentials are tried immediately.
        let _ = super::session::set_state_blocking(pipeline, gst::State::Null).await;
        // Gałęzie B (pełna jakość i podgląd) umierają razem z pipeline'em —
        // zamknij strumienie publisherów, żeby subskrybenci WS dostali
        // `Closed(source_unregistered)` i zrobili resubscribe zamiast wisieć
        // na pustym strumieniu ze starą bazą PTS.
        close_mp4_stream_on_teardown(&cam_id, &branch_b_full_publisher);
        close_mp4_stream_on_teardown(&cam_id, &branch_b_preview_publisher);
        if restart_requested {
            tracing::info!(camera_id = %cam_id, "rtsp session restart requested; rebuilding pipeline");
            streaming_bus().close_camera(&cam_id, "restart").await;
            attempt = 0;
            backoff = policy.initial_backoff;
            continue 'outer;
        }
        let reason =
            redact_url_in_text(&inner_reason.unwrap_or_else(|| "unknown pipeline failure".into()));

        // Fallback GPU-resize → CPU resize. Pipeline padł na busie ZANIM wszedł
        // Online (brak ani jednej klatki) — gałąź detekcji negocjuje caps przy
        // prerollu (PAUSED→PLAYING), więc `not-negotiated` na `cudascale`/
        // `cudaupload` wywala pipeline zanim klatka crops dojdzie do appsinku.
        // Zdejmujemy gałąź GPU-resize PRZED fallbackami dekodera (jest w obu
        // wariantach dekodowania — inaczej GPU-resident↔CPU wpadłyby w pętlę na
        // tej samej wadliwej gałęzi). Detekcja dalej działa (CPU resize).
        // Gdy klatki już poszły (`online`), to awaria sieciowa — zostawiamy
        // GPU-resize włączone.
        if gpu_resize && !online {
            tracing::warn!(
                camera_id = %cam_id,
                reason = %reason,
                "rtsp: gałąź GPU-resize zawiodła na starcie — wyłączam GPU-resize (CPU resize w detektorze)"
            );
            gpu_resize = false;
            streaming_bus().close_camera(&cam_id, &reason).await;
            backoff = policy.initial_backoff;
            continue 'outer;
        }

        // Fallback GPU (GPU-resident lub NVDEC) → niższa ścieżka. Pipeline
        // sprzętowy padł na busie, ZANIM wszedł Online (brak ani jednej klatki)
        // — typowy objaw nieudanej negocjacji łańcucha CUDA/NVDEC albo kodeka
        // bez NVDEC (np. MJPEG, gdy gałąź NVDEC się nie wpięła). Schodzimy o
        // jeden szczebel (NvdecNv12 → NvdecCpuConvert → Cpu) bez backoffu i bez
        // liczenia do limitu prób. Gdy ścieżka sprzętowa zdążyła dać klatki
        // (`online`), traktujemy awarię jako sieciową i zostajemy.
        if matches!(
            ingest_path,
            IngestPath::GpuResidentNvidia | IngestPath::NvdecNv12 | IngestPath::NvdecCpuConvert
        ) && !online
        {
            let next = degrade_ingest_path(ingest_path);
            tracing::warn!(
                camera_id = %cam_id,
                reason = %reason,
                path = ingest_path.label(),
                next = next.label(),
                "rtsp: ścieżka GPU zawiodła na starcie — schodzę o szczebel niżej"
            );
            ingest_path = next;
            streaming_bus().close_camera(&cam_id, &reason).await;
            backoff = policy.initial_backoff;
            continue 'outer;
        }

        // Fallback HW → CPU. Pipeline z dekoderem sprzętowym padł, ZANIM
        // zdążył wejść Online (brak ani jednej klatki) — to typowy objaw
        // nieudanej negocjacji HW (np. ramki w pamięci GPU, których
        // `videoconvert` nie odczyta → `not-negotiated`). Przebudowujemy
        // natychmiast na dekodowanie programowe (bez backoffu, bez liczenia
        // do limitu prób), żeby kamera zadziałała zamiast wpaść w pętlę
        // reconnectów. Jeśli HW zdążyło dać klatki (`online`), traktujemy
        // awarię jako sieciową i zostajemy na HW.
        if use_hw_decode && !online {
            tracing::warn!(
                camera_id = %cam_id,
                reason = %reason,
                "rtsp: dekodowanie sprzętowe zawiodło na starcie — przełączam na programowe (CPU)"
            );
            use_hw_decode = false;
            streaming_bus().close_camera(&cam_id, &reason).await;
            backoff = policy.initial_backoff;
            continue 'outer;
        }

        tracing::warn!(camera_id = %cam_id, reason = %reason, "rtsp pipeline failed; reconnecting");
        streaming_bus().close_camera(&cam_id, &reason).await;

        attempt = attempt.saturating_add(1);
        if reached_max(&policy, attempt) {
            publish(
                &health_tx,
                &cam_id,
                CameraStatus::Error,
                Some(format!("max reconnect attempts exceeded: {reason}")),
                &counters,
                None,
            );
            drain_until_stop(&mut cmd_rx, &health_tx).await;
            return;
        }

        let wait = jittered(&policy, backoff);
        let msg = format!("reconnect attempt {attempt} in {:?}: {reason}", wait);
        // Persist as sticky so subsequent in-pipeline health ticks keep it
        // visible until the camera comes online.
        last_error = Some(msg.clone());
        publish(
            &health_tx,
            &cam_id,
            CameraStatus::Starting,
            Some(msg),
            &counters,
            None,
        );
        if !sleep_with_cancel(&mut cmd_rx, &health_tx, wait, &mut config).await {
            return;
        }
        backoff = next_backoff(backoff, policy.max_backoff);
    }
}

/// Build the URL handed to GStreamer's `rtspsrc`. When the camera has an
/// encrypted credentials blob attached, we decrypt it on demand and overlay
/// `user:pass` into the URL right before the pipeline is wired. The
/// resulting plaintext lives only on the stack of this helper — it never
/// touches the DB and never appears in logs (we route any error through
/// `redact_url_in_text` at the call site so a malformed credential cannot
/// leak via the status_message field).
fn resolve_pipeline_url(config: &CameraConfig) -> std::result::Result<String, String> {
    let Some(blob) = config.credentials_encrypted.as_ref() else {
        return Ok(config.url.clone());
    };
    let creds = credentials_cipher()
        .decrypt(blob)
        .map_err(|e| e.to_string())?;
    overlay_credentials(&config.url, &creds).map_err(|e| e.to_string())
}

fn jittered(policy: &ReconnectPolicy, base: Duration) -> Duration {
    let mut rng = rand::rng();
    compute_backoff_with_jitter(base, policy.jitter_pct, &mut rng)
}

fn reached_max(policy: &ReconnectPolicy, attempt: u32) -> bool {
    matches!(policy.max_attempts, Some(max) if attempt > max)
}

/// Sleep `wait`, but respond promptly to a `Stop` arriving on `cmd_rx`.
/// Returns `false` if the caller should exit immediately (Stop received or
/// channel closed); `true` if the wait completed normally OR a `Restart`
/// arrived (in which case `config` has been updated in place so the caller
/// reconnects on the new credentials).
async fn sleep_with_cancel(
    cmd_rx: &mut mpsc::Receiver<SessionCommand>,
    health_tx: &watch::Sender<CameraHealth>,
    wait: Duration,
    config: &mut CameraConfig,
) -> bool {
    let sleeper = tokio::time::sleep(wait);
    tokio::pin!(sleeper);
    loop {
        tokio::select! {
            biased;
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(SessionCommand::Stop) | None => {
                        let mut h = health_tx.borrow().clone();
                        h.status = CameraStatus::Offline;
                        h.status_message = None;
                        let _ = health_tx.send(h);
                        return false;
                    }
                    Some(SessionCommand::GetHealth(reply)) => {
                        let _ = reply.send(health_tx.borrow().clone());
                    }
                    Some(SessionCommand::Snapshot(reply)) => {
                        let _ = reply.send(Err(CameraIngestError::SnapshotTimeout));
                    }
                    Some(SessionCommand::UpdateConfig(_)) => {}
                    Some(SessionCommand::Restart(new_config)) => {
                        *config = new_config;
                        return true;
                    }
                    Some(SessionCommand::AttachMp4Branch(publisher)) => {
                        // No pipeline is running during reconnect backoff;
                        // refuse the attach cleanly so the hub-side waiter
                        // unblocks immediately and the caller sees
                        // `FactoryFailed`. The next subscriber after
                        // reconnect succeeds will trigger a fresh attach.
                        publisher.mark_unsupported();
                    }
                    Some(SessionCommand::DetachMp4Branch { .. }) => {
                        // Branch already absent (no pipeline) — no-op.
                    }
                }
            }
            _ = &mut sleeper => return true,
        }
    }
}

fn publish(
    tx: &watch::Sender<CameraHealth>,
    cam_id: &str,
    status: CameraStatus,
    msg: Option<String>,
    counters: &FrameCounters,
    fps: Option<f32>,
) {
    let (total, dropped, last_at) = counters.snapshot();
    let _ = tx.send(CameraHealth {
        camera_id: cam_id.to_string(),
        status,
        status_message: msg,
        fps_actual: fps,
        last_frame_at: last_at,
        frames_total: total,
        frames_dropped: dropped,
    });
}

/// Mirror of `session::drain_until_stop` — kept local because the helper in
/// session.rs is private and tightly coupled to its module. After a terminal
/// failure we still service GetHealth / Snapshot so callers see a sensible
/// status instead of timing out at the supervisor's outer 5s wrap.
async fn drain_until_stop(
    rx: &mut mpsc::Receiver<SessionCommand>,
    health_tx: &watch::Sender<CameraHealth>,
) {
    while let Some(cmd) = rx.recv().await {
        match cmd {
            SessionCommand::Stop => return,
            SessionCommand::GetHealth(reply) => {
                let _ = reply.send(health_tx.borrow().clone());
            }
            SessionCommand::Snapshot(reply) => {
                let h = health_tx.borrow().clone();
                let msg = h
                    .status_message
                    .unwrap_or_else(|| "session in terminal error state".into());
                let _ = reply.send(Err(CameraIngestError::SnapshotFailed(msg)));
            }
            SessionCommand::UpdateConfig(_) | SessionCommand::Restart(_) => {}
            SessionCommand::AttachMp4Branch(publisher) => {
                publisher.mark_unsupported();
            }
            SessionCommand::DetachMp4Branch { .. } => {}
        }
    }
}

/// Spawn the RTSP/MJPEG session task. Used by `session::spawn_session` when
/// `vendor == "rtsp"` or `vendor == "mjpeg"` (oba vendory współdzielą pętlę
/// sesji `run_rtsp_session`; różni się builder pipeline'u). Returns the
/// channels the supervisor stores in the `CameraHandle`.
pub fn spawn_rtsp_session(
    config: CameraConfig,
    policy: ReconnectPolicy,
) -> Result<(
    mpsc::Sender<SessionCommand>,
    watch::Receiver<CameraHealth>,
    tokio::task::JoinHandle<()>,
)> {
    if config.vendor == "mjpeg" {
        super::mjpeg::validate_mjpeg_url(&config.url)?;
    } else {
        validate_rtsp_url(&config.url)?;
    }
    if !(1..=60).contains(&config.target_fps) {
        return Err(CameraIngestError::InvalidConfig(format!(
            "target_fps must be 1..=60, got {}",
            config.target_fps
        )));
    }
    ensure_gst_initialized()?;

    let (cmd_tx, cmd_rx) = mpsc::channel::<SessionCommand>(32);
    let (health_tx, health_rx) = watch::channel(CameraHealth::initial(&config.camera_id));
    let mailbox = Arc::new(FrameMailbox::new());
    let counters = Arc::new(FrameCounters::new());

    let join = tokio::spawn(run_rtsp_session(
        config, policy, cmd_rx, health_tx, mailbox, counters,
    ));
    Ok((cmd_tx, health_rx, join))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_rtsp_url_accepts_rtsp() {
        assert!(validate_rtsp_url("rtsp://camera.local/stream").is_ok());
        assert!(validate_rtsp_url("rtsps://camera.local/stream").is_ok());
        assert!(validate_rtsp_url("rtsp://user:pass@10.0.0.5:554/h264").is_ok());
    }

    #[test]
    fn test_validate_rtsp_url_rejects_other_schemes() {
        for bad in [
            "",
            "http://cam/stream",
            "file:///tmp/foo.mp4",
            "rtsp://",
            "rtsps://",
            "camera.local/stream",
        ] {
            assert!(validate_rtsp_url(bad).is_err(), "should reject: {bad}");
        }
    }

    #[test]
    fn test_next_backoff_doubles_until_cap() {
        let max = Duration::from_secs(60);
        let mut b = Duration::from_secs(1);
        let mut seen = Vec::new();
        for _ in 0..10 {
            seen.push(b);
            b = next_backoff(b, max);
        }
        assert_eq!(seen[0], Duration::from_secs(1));
        assert_eq!(seen[1], Duration::from_secs(2));
        assert_eq!(seen[2], Duration::from_secs(4));
        assert_eq!(seen[3], Duration::from_secs(8));
        assert_eq!(seen[4], Duration::from_secs(16));
        assert_eq!(seen[5], Duration::from_secs(32));
        // 64 > 60 ⇒ capped at 60.
        assert_eq!(seen[6], Duration::from_secs(60));
        assert_eq!(seen[7], Duration::from_secs(60));
    }

    #[test]
    fn test_jitter_within_bounds() {
        // 1s ±20% must always lie in [800, 1200] ms. Run many draws to
        // exercise the symmetric distribution.
        let mut rng = rand::rng();
        let base = Duration::from_secs(1);
        for _ in 0..1000 {
            let out = compute_backoff_with_jitter(base, 0.20, &mut rng);
            let ms = out.as_millis();
            assert!(
                (800..=1200).contains(&ms),
                "jitter out of bounds: {ms}ms (base=1s, ±20%)"
            );
        }
    }

    #[test]
    fn test_jitter_floor_at_100ms() {
        // Even with absurd negative jitter, the helper must never sleep less
        // than 100ms — protects against tight reconnect loops.
        let mut rng = rand::rng();
        let base = Duration::from_millis(50);
        // 50ms ±200% would otherwise dip into negatives; floor kicks in.
        let out = compute_backoff_with_jitter(base, 2.0, &mut rng);
        assert!(out >= Duration::from_millis(100));
    }

    /// Rozbiórka pipeline'u z aktywną gałęzią B musi zamknąć strumień
    /// publishera: odbiorcy broadcastu widzą `Closed` (→ warstwa WS wysyła
    /// `Closed(source_unregistered)` i frontend robi resubscribe), a hub
    /// widzi źródło jako terminalne (`chunk_broadcaster() == None`), więc
    /// kolejny subscribe zbuduje świeżego publishera z nową bazą PTS.
    #[tokio::test]
    async fn test_teardown_closes_active_mp4_stream() {
        use crate::services::stream_hub::BinaryStreamSource;
        let (cmd_tx, _cmd_rx) = mpsc::channel(8);
        let publisher = Arc::new(Mp4StreamPublisher::new(
            "cam_teardown".into(),
            cmd_tx,
            false,
        ));
        let mut rx = publisher
            .chunk_broadcaster()
            .expect("broadcaster live")
            .subscribe();
        let weak = Some(Arc::downgrade(&publisher));

        close_mp4_stream_on_teardown("cam_teardown", &weak);

        assert!(
            rx.recv().await.is_err(),
            "subskrybent musi zobaczyć Closed po rozbiórce pipeline'u"
        );
        assert!(
            publisher.chunk_broadcaster().is_none(),
            "hub musi widzieć źródło jako terminalne, by kolejny subscribe zbudował nowego publishera"
        );
    }

    /// Brak aktywnej gałęzi B (None) i martwy publisher (Weak bez silnych
    /// referencji) — rozbiórka jest no-opem i nie może panikować.
    #[test]
    fn test_teardown_noop_without_active_publisher() {
        close_mp4_stream_on_teardown("cam_none", &None);

        let (cmd_tx, _cmd_rx) = mpsc::channel(8);
        let publisher = Arc::new(Mp4StreamPublisher::new("cam_dead".into(), cmd_tx, false));
        let weak = Some(Arc::downgrade(&publisher));
        drop(publisher);
        close_mp4_stream_on_teardown("cam_dead", &weak);
    }

    #[test]
    fn test_reconnect_policy_defaults() {
        let p = ReconnectPolicy::default();
        assert_eq!(p.initial_backoff, Duration::from_secs(1));
        assert_eq!(p.max_backoff, Duration::from_secs(60));
        assert!((p.jitter_pct - 0.20).abs() < 1e-9);
        assert!(p.max_attempts.is_none());
    }

    #[test]
    fn redact_rtsp_url_strips_credentials() {
        assert_eq!(
            redact_rtsp_url("rtsp://alice:s3cret@cam.local:554/h264"),
            "rtsp://***:***@cam.local:554/h264"
        );
        assert_eq!(
            redact_rtsp_url("rtsps://bob:p%40ss@10.0.0.5/stream"),
            "rtsps://***:***@10.0.0.5/stream"
        );
        // No credentials → unchanged.
        assert_eq!(
            redact_rtsp_url("rtsp://cam.local/stream"),
            "rtsp://cam.local/stream"
        );
        // Non-rtsp scheme → unchanged (caller's responsibility to validate).
        assert_eq!(redact_rtsp_url("http://x/y"), "http://x/y");
    }

    #[test]
    fn redact_url_in_text_handles_embedded_url() {
        let err = "rtspsrc: could not open resource: rtsp://u:p@cam:554/x (server unreachable)";
        let out = redact_url_in_text(err);
        assert!(!out.contains("u:p"), "credentials leaked: {out}");
        assert!(out.contains("rtsp://***:***@cam:554/x"));
    }

    #[test]
    fn validate_rtsp_url_error_does_not_leak_credentials() {
        // rtsp:// with empty host is rejected — the formatted error must not
        // echo any credentials should the caller pass an oddly-shaped URL.
        let err = validate_rtsp_url("rtsp://").unwrap_err();
        let msg = err.to_string();
        assert!(!msg.contains("password"), "leaked: {msg}");
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn sleep_with_cancel_responds_to_stop_during_backoff() {
        let (tx, mut rx) = mpsc::channel::<SessionCommand>(4);
        let (htx, _hrx) = watch::channel(CameraHealth::initial("cam_test"));
        let cancel_after = Duration::from_millis(100);
        let total_wait = Duration::from_secs(30);

        let cancel_tx = tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(cancel_after).await;
            let _ = cancel_tx.send(SessionCommand::Stop).await;
        });

        let mut cfg = CameraConfig::new_unowned("cam_test", "rtsp", "rtsp://x/y", 30, None);
        let start = tokio::time::Instant::now();
        let completed = sleep_with_cancel(&mut rx, &htx, total_wait, &mut cfg).await;
        let elapsed = start.elapsed();
        assert!(!completed, "sleep_with_cancel must return false on Stop");
        assert!(
            elapsed < Duration::from_secs(1),
            "stop should interrupt backoff promptly, took {elapsed:?}"
        );
        drop(tx);
    }

    #[test]
    fn forced_software_override_always_picks_cpu_path() {
        // Operator wymusił dekoder programowy — ścieżka MUSI być CPU bez
        // względu na obecność sprzętu/elementów CUDA. Deterministyczne, bo
        // `Some(Software)` zwiera warunek `resolve_ingest_path` przed
        // odpytaniem runtime'u.
        let mut cfg = CameraConfig::new_unowned("cam_sw", "rtsp", "rtsp://x/y", 30, None);
        cfg.decoder_override = Some(HwDecoder::Software);
        assert_eq!(resolve_ingest_path(&cfg), IngestPath::Cpu);
    }

    #[test]
    fn ingest_path_default_matches_runtime_gpu_availability() {
        // Bez override ścieżka startowa zależy wyłącznie od tego, które elementy
        // runtime ma: pełny łańcuch GPU-resident, samo NVDEC + cudadownload, czy
        // nic z tego. Test pilnuje spójności tej kaskady precedencji z
        // `gpu_resident_available`/`nvdec_decode_available`, nie konkretnego
        // sprzętu CI.
        let cfg = CameraConfig::new_unowned("cam_auto", "rtsp", "rtsp://x/y", 30, None);
        let expected = if gpu_resident_available() {
            IngestPath::GpuResidentNvidia
        } else if nvdec_decode_available() {
            // NVDEC ingest prefers the GPU NV12 detect path when the ort GPU
            // features are built; else the CPU-convert NVDEC path.
            if nv12_gpu_detect_available() {
                IngestPath::NvdecNv12
            } else {
                IngestPath::NvdecCpuConvert
            }
        } else {
            IngestPath::Cpu
        };
        assert_eq!(resolve_ingest_path(&cfg), expected);
    }

    #[test]
    fn degrade_ingest_steps_down_one_rung() {
        // NV12 detect degrades to the deployed NVDEC+CPU-convert path first
        // (keeps GPU decode); every other hardware path degrades to CPU.
        assert_eq!(
            degrade_ingest_path(IngestPath::NvdecNv12),
            IngestPath::NvdecCpuConvert
        );
        assert_eq!(
            degrade_ingest_path(IngestPath::NvdecCpuConvert),
            IngestPath::Cpu
        );
        assert_eq!(
            degrade_ingest_path(IngestPath::GpuResidentNvidia),
            IngestPath::Cpu
        );
    }

    #[test]
    fn ingest_path_labels_are_distinct() {
        let labels = [
            IngestPath::GpuResidentNvidia.label(),
            IngestPath::NvdecNv12.label(),
            IngestPath::NvdecCpuConvert.label(),
            IngestPath::Cpu.label(),
        ];
        let mut deduped = labels.to_vec();
        deduped.sort_unstable();
        deduped.dedup();
        assert_eq!(deduped.len(), labels.len(), "etykiety muszą być unikalne");
        assert!(IngestPath::GpuResidentNvidia.label().contains("GPU"));
        assert!(IngestPath::NvdecNv12.label().contains("NV12"));
        assert!(IngestPath::NvdecCpuConvert.label().contains("NVDEC"));
    }

    #[test]
    fn test_reached_max_logic() {
        let mut p = ReconnectPolicy::default();
        p.max_attempts = Some(3);
        assert!(!reached_max(&p, 1));
        assert!(!reached_max(&p, 3));
        assert!(reached_max(&p, 4));
        p.max_attempts = None;
        assert!(!reached_max(&p, 1_000_000));
    }
}

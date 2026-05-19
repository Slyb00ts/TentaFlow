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
use rand::RngExt;
use regex::Regex;
use std::sync::OnceLock;
use tokio::sync::{mpsc, watch};

use super::credentials::{credentials_cipher, overlay_credentials};
use super::error::{CameraIngestError, Result};
use super::fakefile::{ensure_gst_initialized, FrameCounters, FrameMailbox, LatestFrame};
use super::session::{
    CameraConfig, CameraHealth, CameraStatus, PixelFormat, SessionCommand, SnapshotData,
};
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

/// Redact any RTSP credentials embedded inside a free-form string (e.g. a
/// GStreamer error message that quoted the original location). Anchored on
/// `rtsp://` or `rtsps://` followed by anything up to `@`.
pub fn redact_url_in_text(text: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re =
        RE.get_or_init(|| Regex::new(r"(rtsps?)://[^@\s/]+@").expect("redact regex must compile"));
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

/// Build the typed-element RTSP pipeline. `rtspsrc`'s source pad is dynamic
/// (it appears once SDP negotiation completes), so we register a
/// `pad-added` handler that links it to `rtph264depay` only for video
/// streams.
pub fn build_rtsp_pipeline(
    camera_id: String,
    url: &str,
    timeout_secs: u32,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) -> Result<gst::Pipeline> {
    let pipeline = gst::Pipeline::new();

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
    //   - rtsp://  -> "udp+udp-mcast+tcp" (default behavior, UDP preferred)
    //   - rtsps:// -> "tcp+tls" (TLS over TCP; udp-over-tls is rare)
    // Without `tls` in the mask, rtspsrc would silently fail on rtsps:// URLs.
    let is_tls = url.starts_with("rtsps://");
    let protocols_str = if is_tls {
        "tcp+tls"
    } else {
        "udp+udp-mcast+tcp"
    };

    let rtspsrc = gst::ElementFactory::make("rtspsrc")
        .property("location", url)
        .property("latency", 200u32)
        // rtspsrc timeout is in microseconds (GstClockTimeDiff).
        .property("timeout", (timeout_secs as u64).saturating_mul(1_000_000))
        // Disable rtspsrc's internal retry — we manage reconnect at session level.
        .property("retry", 0u32)
        .property_from_str("protocols", protocols_str)
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
        "rtsp: built pipeline url_scheme={} protocols={}",
        if is_tls { "rtsps" } else { "rtsp" },
        protocols_str
    );

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
    // Force software decoders. If decodebin autoplugs an NVIDIA / VAAPI /
    // QSV decoder, output caps land in GPU memory (e.g. CUDAMemory NV12)
    // which `videoconvert` (CPU-only) cannot read — pipeline aborts with
    // `not-negotiated (-4)` when downstream demands `format=RGB`. Software
    // decode of 1080p H.264 costs ~3-5% of a CPU core, acceptable for
    // MVP; a GPU-aware path with `cudadownload` / `vaapipostproc` is a
    // later optimization. Setting via `set_property` after build because
    // the builder-side `.property("force-sw-decoders", true)` silently
    // failed to take effect in gstreamer-rs 0.23 (verified by HW decoder
    // still autoplugged after restart).
    decodebin.set_property("force-sw-decoders", true);
    let fsd_active: bool = decodebin.property("force-sw-decoders");
    tracing::info!("rtsp: decodebin force-sw-decoders={}", fsd_active);
    // RTP input capsfilter — pins decodebin's input to `application/x-rtp,
    // media=video`. Without this, decodebin may briefly see ambiguous caps
    // during rtspsrc setup and abort the pipeline with `not-negotiated (-4)`
    // before its first output pad is exposed. Verified by replicating the
    // exact pipeline in gst-launch: bare `rtspsrc ! decodebin` failed, while
    // `rtspsrc ! application/x-rtp,media=video ! decodebin` succeeded.
    let rtp_caps = gst::Caps::builder("application/x-rtp")
        .field("media", "video")
        .build();
    let rtp_filter = gst::ElementFactory::make("capsfilter")
        .property("caps", &rtp_caps)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("rtp capsfilter: {e}")))?;

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

    pipeline
        .add_many([
            &rtspsrc,
            &rtp_filter,
            &decodebin,
            &convert,
            &capsfilter,
            &appsink,
        ])
        .map_err(|e| CameraIngestError::PipelineBuild(format!("add_many: {e}")))?;

    // Static segments:
    //   rtp_filter → decodebin (capsfilter pins RTP video before autoplug)
    //   convert → capsfilter → appsink (after decode)
    // rtspsrc → rtp_filter is dynamic (pad-added below) and decodebin → convert
    // is dynamic (decoder src pad appears after autoplug).
    gst::Element::link(&rtp_filter, &decodebin)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("rtp_filter → decodebin: {e}")))?;
    gst::Element::link_many([&convert, &capsfilter, &appsink])
        .map_err(|e| CameraIngestError::PipelineBuild(format!("link_many tail: {e}")))?;

    // decodebin's video output pad appears dynamically once the codec is
    // identified. Wire it into videoconvert when caps say video/x-raw.
    let convert_weak = convert.downgrade();
    decodebin.connect_pad_added(move |_dec, src_pad| {
        let Some(convert) = convert_weak.upgrade() else {
            return;
        };
        let Some(sink_pad) = convert.static_pad("sink") else {
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
            tracing::warn!("rtsp: decodebin → videoconvert link failed: {e:?}");
        } else {
            tracing::info!("rtsp: decodebin video pad linked (codec auto-detected)");
        }
    });

    // Wire the appsink frame callback before pad-added so the very first
    // sample is captured.
    let appsink_app = appsink
        .downcast::<gst_app::AppSink>()
        .map_err(|_| CameraIngestError::PipelineBuild("appsink downcast failed".into()))?;
    install_frame_callback(&appsink_app, camera_id, mailbox, counters);

    // Alias for rtspsrc dynamic linking below — points at the rtp capsfilter
    // (was `depay`/`decodebin` in earlier revisions). The dynamic pad from
    // rtspsrc now feeds into `rtp_filter`, which is statically linked to
    // `decodebin` above.
    let depay = rtp_filter.clone();

    // Dynamic pad-added handler — link only the video RTP pad.
    //
    // rtspsrc emits `pad-added` as soon as the pad is created, but `current_caps()`
    // can still be None at that moment — RTP caps are negotiated asynchronously
    // and may only land on the pad after a `notify::caps` signal. For multi-
    // stream sources (UniFi Protect publishes 2 audio + 1 video pads, Hikvision
    // similar) every pad arrives without caps, so the `current_caps() -> None ->
    // return` early-exit silently drops the video pad and the pipeline never
    // produces frames. We therefore try to link immediately if caps are present,
    // and fall back to a one-shot `notify::caps` watcher otherwise.
    let depay_weak = depay.downgrade();
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

    Ok(pipeline)
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
                let bytes = map.as_slice().to_vec();
                let ts_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let shared: Arc<[u8]> = Arc::from(bytes.into_boxed_slice());
                let frame_size = shared.len();
                mailbox_cb.put(LatestFrame {
                    width: width as u32,
                    height: height as u32,
                    timestamp_unix_ms: ts_ms,
                    data: shared.clone(),
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
        // without us holding a stale plaintext across iterations.
        let final_url = match resolve_pipeline_url(&config) {
            Ok(u) => u,
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
            "rtsp: building pipeline"
        );
        let pipeline = match build_rtsp_pipeline(
            cam_id.clone(),
            &final_url,
            timeout_secs,
            mailbox.clone(),
            counters.clone(),
        ) {
            Ok(p) => p,
            Err(e) => {
                let reason = redact_url_in_text(&format!("build failed: {e}"));
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

        tracing::info!(camera_id = %cam_id, "rtsp: setting pipeline state -> Playing");
        if let Err(e) = pipeline.set_state(gst::State::Playing) {
            let raw_reason = format!("set_state(Playing) failed: {e}");
            let reason = redact_url_in_text(&raw_reason);
            tracing::error!(camera_id = %cam_id, reason = %reason, "rtsp: set_state Playing failed");
            let _ = pipeline.set_state(gst::State::Null);
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
        let mut last_total: u64 = 0;
        let mut fps_window: std::collections::VecDeque<f32> =
            std::collections::VecDeque::with_capacity(30);
        let started_at = tokio::time::Instant::now();
        let warmup_deadline = started_at + Duration::from_secs(timeout_secs as u64 + 5);
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
                            let _ = pipeline.set_state(gst::State::Null);
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
                                    break Ok(SnapshotData {
                                        camera_id: cam_id.clone(),
                                        width: f.width,
                                        height: f.height,
                                        pixel_format: PixelFormat::Rgb24,
                                        timestamp_unix_ms: f.timestamp_unix_ms,
                                        data: f.data.to_vec(),
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
        let _ = pipeline.set_state(gst::State::Null);
        if restart_requested {
            tracing::info!(camera_id = %cam_id, "rtsp session restart requested; rebuilding pipeline");
            streaming_bus().close_camera(&cam_id, "restart").await;
            attempt = 0;
            backoff = policy.initial_backoff;
            continue 'outer;
        }
        let reason =
            redact_url_in_text(&inner_reason.unwrap_or_else(|| "unknown pipeline failure".into()));
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
        }
    }
}

/// Spawn the RTSP session task. Used by `session::spawn_session` when
/// `vendor == "rtsp"`. Returns the channels the supervisor stores in the
/// `CameraHandle`.
pub fn spawn_rtsp_session(
    config: CameraConfig,
    policy: ReconnectPolicy,
) -> Result<(
    mpsc::Sender<SessionCommand>,
    watch::Receiver<CameraHealth>,
    tokio::task::JoinHandle<()>,
)> {
    validate_rtsp_url(&config.url)?;
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

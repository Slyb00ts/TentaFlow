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
use tokio::sync::{mpsc, watch};

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
            "missing rtsp:// or rtsps:// scheme: {url}"
        )));
    }
    // After the scheme there must be at least one host character.
    let after_scheme = url
        .strip_prefix("rtsp://")
        .or_else(|| url.strip_prefix("rtsps://"))
        .unwrap_or("");
    if after_scheme.is_empty() {
        return Err(CameraIngestError::InvalidUrl(format!(
            "missing host: {url}"
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

    let rtspsrc = gst::ElementFactory::make("rtspsrc")
        .property("location", url)
        .property("latency", 200u32)
        // rtspsrc timeout is in microseconds (GstClockTimeDiff).
        .property("timeout", (timeout_secs as u64).saturating_mul(1_000_000))
        // Disable rtspsrc's internal retry — we manage reconnect at session level.
        .property("retry", 0u32)
        // TCP fallback if UDP setup fails — improves NAT/firewall traversal.
        // 0=udp, 1=udp-mcast, 2=tcp, 3=http, 4=tls — passed as bitmask via
        // protocols property (default 0x7).
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("rtspsrc: {e}")))?;

    let depay = gst::ElementFactory::make("rtph264depay")
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("rtph264depay: {e}")))?;
    let parser = gst::ElementFactory::make("h264parse")
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("h264parse: {e}")))?;
    let decoder = gst::ElementFactory::make("avdec_h264")
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("avdec_h264: {e}")))?;
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
        .add_many([&rtspsrc, &depay, &parser, &decoder, &convert, &capsfilter, &appsink])
        .map_err(|e| CameraIngestError::PipelineBuild(format!("add_many: {e}")))?;

    // Static section: depay → parser → decoder → convert → capsfilter → appsink.
    gst::Element::link_many([&depay, &parser, &decoder, &convert, &capsfilter, &appsink])
        .map_err(|e| CameraIngestError::PipelineBuild(format!("link_many: {e}")))?;

    // Wire the appsink frame callback before pad-added so the very first
    // sample is captured.
    let appsink_app = appsink
        .downcast::<gst_app::AppSink>()
        .map_err(|_| CameraIngestError::PipelineBuild("appsink downcast failed".into()))?;
    install_frame_callback(&appsink_app, camera_id, mailbox, counters);

    // Dynamic pad-added handler — link only the video RTP pad.
    let depay_weak = depay.downgrade();
    rtspsrc.connect_pad_added(move |_src, src_pad| {
        let Some(depay) = depay_weak.upgrade() else {
            return;
        };
        let Some(sink_pad) = depay.static_pad("sink") else {
            return;
        };
        if sink_pad.is_linked() {
            return;
        }
        // Filter on media=video so audio/metadata streams do not get wired
        // into the H.264 decoder.
        if let Some(caps) = src_pad.current_caps() {
            if let Some(s) = caps.structure(0) {
                let media: std::result::Result<String, _> = s.get("media");
                if media.as_deref().ok() != Some("video") {
                    return;
                }
            }
        }
        if let Err(e) = src_pad.link(&sink_pad) {
            tracing::warn!("rtsp: failed to link rtspsrc → depay: {e:?}");
        }
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
    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let caps = sample.caps().ok_or(gst::FlowError::Error)?;
                let s = caps.structure(0).ok_or(gst::FlowError::Error)?;
                let width: i32 = s.get("width").map_err(|_| gst::FlowError::Error)?;
                let height: i32 = s.get("height").map_err(|_| gst::FlowError::Error)?;
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
    config: CameraConfig,
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

    publish(&health_tx, &cam_id, CameraStatus::Starting, None, &counters, None);

    'outer: loop {
        let pipeline = match build_rtsp_pipeline(
            cam_id.clone(),
            &config.url,
            timeout_secs,
            mailbox.clone(),
            counters.clone(),
        ) {
            Ok(p) => p,
            Err(e) => {
                let reason = format!("build failed: {e}");
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

        if let Err(e) = pipeline.set_state(gst::State::Playing) {
            let reason = format!("set_state(Playing) failed: {e}");
            publish(
                &health_tx,
                &cam_id,
                CameraStatus::Error,
                Some(reason.clone()),
                &counters,
                None,
            );
            let _ = pipeline.set_state(gst::State::Null);
            streaming_bus().close_camera(&cam_id, &reason).await;
            // A pure state-set failure is recoverable in principle, but it
            // usually means a misconfigured element — fall into the
            // reconnect path so the operator's intervention (e.g. fixing
            // the URL) is observed without a process restart.
            if !sleep_with_cancel(&mut cmd_rx, &health_tx, jittered(&policy, backoff)).await {
                return;
            }
            attempt = attempt.saturating_add(1);
            if reached_max(&policy, attempt) {
                publish(
                    &health_tx,
                    &cam_id,
                    CameraStatus::Error,
                    Some("max reconnect attempts exceeded".into()),
                    &counters,
                    None,
                );
                drain_until_stop(&mut cmd_rx, &health_tx).await;
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
                                let text = format!(
                                    "{} ({})",
                                    err.error(),
                                    err.debug().unwrap_or_default()
                                );
                                terminate = Some(text);
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
                        } else if tokio::time::Instant::now() >= warmup_deadline {
                            break Some("no frames within warmup window".into());
                        }
                    }

                    let status = if online {
                        CameraStatus::Online
                    } else {
                        CameraStatus::Starting
                    };
                    let _ = health_tx.send(CameraHealth {
                        camera_id: cam_id.clone(),
                        status,
                        status_message: None,
                        fps_actual: avg,
                        last_frame_at: last_at,
                        frames_total: total,
                        frames_dropped: dropped,
                    });
                }
            }
        };

        // Pipeline failed — tear it down and schedule a reconnect.
        let _ = pipeline.set_state(gst::State::Null);
        let reason = inner_reason.unwrap_or_else(|| "unknown pipeline failure".into());
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
        publish(
            &health_tx,
            &cam_id,
            CameraStatus::Starting,
            Some(format!("reconnect attempt {attempt} in {:?}: {reason}", wait)),
            &counters,
            None,
        );
        if !sleep_with_cancel(&mut cmd_rx, &health_tx, wait).await {
            return;
        }
        backoff = next_backoff(backoff, policy.max_backoff);
    }
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
/// channel closed); `true` if the wait completed normally.
async fn sleep_with_cancel(
    cmd_rx: &mut mpsc::Receiver<SessionCommand>,
    health_tx: &watch::Sender<CameraHealth>,
    wait: Duration,
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
            SessionCommand::UpdateConfig(_) => {}
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

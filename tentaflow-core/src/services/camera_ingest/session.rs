// =============================================================================
// File: services/camera_ingest/session.rs — per-camera session task + lifecycle
// =============================================================================
//
// Each camera registered with the supervisor owns one `CameraSession`. The
// session spawns a tokio task that:
//   1. resolves + validates the source URL,
//   2. builds and starts a GStreamer pipeline (fakefile),
//   3. publishes `CameraHealth` updates through a `watch::Sender`,
//   4. handles control commands (`Stop`, `GetHealth`) over an mpsc,
//   5. replays the source on EOS and surfaces fatal bus errors as `Error`.

use std::sync::Arc;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};

use super::error::{CameraIngestError, Result};
use super::fakefile::{
    build_pipeline, ensure_gst_initialized, resolve_file_url, seek_to_start, FrameCounters,
    FrameMailbox,
};

/// Pixel format of the latest frame. F1a only emits RGB24 because the
/// pipeline forces `video/x-raw,format=RGB`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelFormat {
    Rgb24,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CameraStatus {
    Offline,
    Starting,
    Online,
    Error,
    Stopping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraHealth {
    pub camera_id: String,
    pub status: CameraStatus,
    pub status_message: Option<String>,
    pub fps_actual: Option<f32>,
    pub last_frame_at: Option<u64>,
    pub frames_total: u64,
    pub frames_dropped: u64,
}

impl CameraHealth {
    pub fn initial(camera_id: &str) -> Self {
        Self {
            camera_id: camera_id.to_string(),
            status: CameraStatus::Offline,
            status_message: None,
            fps_actual: None,
            last_frame_at: None,
            frames_total: 0,
            frames_dropped: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CameraConfig {
    pub camera_id: String,
    pub vendor: String,
    pub url: String,
    pub target_fps: u32,
    pub resolution: Option<(u32, u32)>,
    /// Owning addon id — set by `camera_add_v1` so the supervisor can
    /// enforce per-addon DoS quotas. `None` only in pre-quota call sites
    /// (legacy tests); global cap still applies.
    #[doc(hidden)]
    pub owner_addon_id: Option<String>,
    /// AES-GCM blob carrying the RTSP `user:pass` portion. `None` for
    /// open streams (e.g. `fake_file` or a public RTSP camera). The RTSP
    /// connector decrypts this on each pipeline build and overlays the
    /// resulting credentials onto `url` before handing the URL to
    /// GStreamer — `url` itself never persists credentials in plaintext.
    pub credentials_encrypted: Option<Vec<u8>>,
}

impl CameraConfig {
    /// Minimal constructor for tests + internal callers that do not need
    /// owner tracking. Production `camera_add_v1` sets `owner_addon_id`
    /// explicitly.
    pub fn new_unowned(
        camera_id: impl Into<String>,
        vendor: impl Into<String>,
        url: impl Into<String>,
        target_fps: u32,
        resolution: Option<(u32, u32)>,
    ) -> Self {
        Self {
            camera_id: camera_id.into(),
            vendor: vendor.into(),
            url: url.into(),
            target_fps,
            resolution,
            owner_addon_id: None,
            credentials_encrypted: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SnapshotData {
    pub camera_id: String,
    pub width: u32,
    pub height: u32,
    pub pixel_format: PixelFormat,
    pub timestamp_unix_ms: u64,
    pub data: Vec<u8>,
}

/// Control-plane messages sent into a session task.
#[derive(Debug)]
pub enum SessionCommand {
    Stop,
    UpdateConfig(CameraConfig),
    GetHealth(oneshot::Sender<CameraHealth>),
    Snapshot(oneshot::Sender<std::result::Result<SnapshotData, CameraIngestError>>),
}

/// External handle to a running session, stored in the supervisor registry.
#[derive(Debug)]
pub struct CameraHandle {
    pub id: String,
    pub vendor: String,
    /// Addon that called `camera_add_v1`. Used by the supervisor to count
    /// per-addon cameras for the DoS quota.
    pub owner_addon_id: Option<String>,
    pub cmd_tx: mpsc::Sender<SessionCommand>,
    pub health_rx: watch::Receiver<CameraHealth>,
    pub join_handle: tokio::task::JoinHandle<()>,
}

impl CameraHandle {
    pub fn health(&self) -> CameraHealth {
        self.health_rx.borrow().clone()
    }
}

/// Spawn a session task driving a single camera. Returns a handle the
/// supervisor stores under `camera_id`.
pub fn spawn_session(config: CameraConfig) -> Result<CameraHandle> {
    if !(1..=60).contains(&config.target_fps) {
        return Err(CameraIngestError::InvalidConfig(format!(
            "target_fps must be 1..=60, got {}",
            config.target_fps
        )));
    }

    let id = config.camera_id.clone();
    let vendor = config.vendor.clone();
    let owner_addon_id = config.owner_addon_id.clone();

    let (cmd_tx, health_rx, join_handle) = match vendor.as_str() {
        "fake_file" => spawn_fakefile_inner(config)?,
        "rtsp" => {
            use super::rtsp::{spawn_rtsp_session, ReconnectPolicy};
            spawn_rtsp_session(config, ReconnectPolicy::default())?
        }
        other => return Err(CameraIngestError::UnsupportedVendor(other.to_string())),
    };

    Ok(CameraHandle {
        id,
        vendor,
        owner_addon_id,
        cmd_tx,
        health_rx,
        join_handle,
    })
}

fn spawn_fakefile_inner(
    config: CameraConfig,
) -> Result<(
    mpsc::Sender<SessionCommand>,
    watch::Receiver<CameraHealth>,
    tokio::task::JoinHandle<()>,
)> {
    let path = resolve_file_url(&config.url)?;
    ensure_gst_initialized()?;

    let (cmd_tx, cmd_rx) = mpsc::channel::<SessionCommand>(32);
    let (health_tx, health_rx) = watch::channel(CameraHealth::initial(&config.camera_id));
    let mailbox = Arc::new(FrameMailbox::new());
    let counters = Arc::new(FrameCounters::new());

    let join_handle = tokio::spawn(run_session(
        config, path, cmd_rx, health_tx, mailbox, counters,
    ));
    Ok((cmd_tx, health_rx, join_handle))
}

async fn run_session(
    config: CameraConfig,
    path: std::path::PathBuf,
    mut cmd_rx: mpsc::Receiver<SessionCommand>,
    health_tx: watch::Sender<CameraHealth>,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) {
    let cam_id = config.camera_id.clone();
    publish(&health_tx, &cam_id, CameraStatus::Starting, None, &counters, None);

    let pipeline = match build_pipeline(&path, cam_id.clone(), mailbox.clone(), counters.clone()) {
        Ok(p) => p,
        Err(e) => {
            let reason = e.to_string();
            publish(
                &health_tx,
                &cam_id,
                CameraStatus::Error,
                Some(reason.clone()),
                &counters,
                None,
            );
            crate::services::streaming_bus().close_camera(&cam_id, &reason).await;
            // Drain commands until Stop so the supervisor's join completes
            // cleanly even on early failure.
            drain_until_stop(&mut cmd_rx, &health_tx).await;
            return;
        }
    };

    if let Err(e) = pipeline.pipeline.set_state(gst::State::Playing) {
        let reason = format!("set_state(Playing) failed: {e}");
        publish(
            &health_tx,
            &cam_id,
            CameraStatus::Error,
            Some(reason.clone()),
            &counters,
            None,
        );
        let _ = pipeline.pipeline.set_state(gst::State::Null);
        crate::services::streaming_bus().close_camera(&cam_id, &reason).await;
        drain_until_stop(&mut cmd_rx, &health_tx).await;
        return;
    }

    let bus = pipeline.pipeline.bus().expect("pipeline has bus");

    // FPS moving-average state. Sampled every second from the counters.
    let mut last_total: u64 = 0;
    let mut fps_window: std::collections::VecDeque<f32> = std::collections::VecDeque::with_capacity(30);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // Promote to Online as soon as the first frame arrives (or after a
    // bounded warmup window). Track separately to avoid spurious flapping.
    let mut online = false;
    let started_at = tokio::time::Instant::now();
    let warmup_deadline = started_at + Duration::from_secs(10);

    loop {
        tokio::select! {
            biased;
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(SessionCommand::Stop) | None => {
                        publish(&health_tx, &cam_id, CameraStatus::Stopping, None, &counters, fps_window.back().copied());
                        let _ = pipeline.pipeline.set_state(gst::State::Null);
                        publish(&health_tx, &cam_id, CameraStatus::Offline, None, &counters, None);
                        crate::services::streaming_bus().close_camera(&cam_id, "stopped").await;
                        return;
                    }
                    Some(SessionCommand::UpdateConfig(_new)) => {
                        // F1a: hot config update is a no-op. F1b will tear
                        // down and rebuild the pipeline when source params
                        // change.
                    }
                    Some(SessionCommand::GetHealth(reply)) => {
                        let h = health_tx.borrow().clone();
                        let _ = reply.send(h);
                    }
                    Some(SessionCommand::Snapshot(reply)) => {
                        // Wait up to 4.5s for the first frame to land (the
                        // supervisor wrap-timeout is 5s — leave headroom).
                        // We poll the mailbox + health watch so terminal
                        // Error short-circuits without waiting the full
                        // window.
                        let deadline =
                            tokio::time::Instant::now() + Duration::from_millis(4500);
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
                // Drain bus for state-change / EOS / error events.
                while let Some(msg) = bus.pop() {
                    use gst::MessageView;
                    match msg.view() {
                        MessageView::Eos(_) => {
                            // Seek back to start to implement replay loop.
                            if let Err(e) = seek_to_start(&pipeline.pipeline) {
                                let reason = e.to_string();
                                publish(&health_tx, &cam_id, CameraStatus::Error, Some(reason.clone()), &counters, fps_window.back().copied());
                                let _ = pipeline.pipeline.set_state(gst::State::Null);
                                crate::services::streaming_bus().close_camera(&cam_id, &reason).await;
                                drain_until_stop(&mut cmd_rx, &health_tx).await;
                                return;
                            }
                        }
                        MessageView::Error(err) => {
                            let text = format!("{} ({})", err.error(), err.debug().unwrap_or_default());
                            publish(&health_tx, &cam_id, CameraStatus::Error, Some(text.clone()), &counters, fps_window.back().copied());
                            let _ = pipeline.pipeline.set_state(gst::State::Null);
                            crate::services::streaming_bus().close_camera(&cam_id, &text).await;
                            drain_until_stop(&mut cmd_rx, &health_tx).await;
                            return;
                        }
                        _ => {}
                    }
                }

                // FPS sampling: frames in the last second.
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

                // Promote to Online once we have a frame, or surface Error
                // if warmup elapsed with zero frames.
                if !online {
                    if total > 0 {
                        online = true;
                    } else if tokio::time::Instant::now() >= warmup_deadline {
                        let reason = "no frames within warmup window";
                        publish(&health_tx, &cam_id, CameraStatus::Error, Some(reason.into()), &counters, None);
                        let _ = pipeline.pipeline.set_state(gst::State::Null);
                        crate::services::streaming_bus().close_camera(&cam_id, reason).await;
                        drain_until_stop(&mut cmd_rx, &health_tx).await;
                        return;
                    }
                }

                let status = if online { CameraStatus::Online } else { CameraStatus::Starting };
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

/// After a terminal error we keep the task alive so the supervisor can
/// observe the failure state and issue Stop in its own time. Snapshot and
/// GetHealth must still reply (with the cached terminal status) — silently
/// dropping the oneshot would force every caller to wait the supervisor's
/// outer 5 s timeout for every probe.
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
            SessionCommand::UpdateConfig(_) => {
                // Terminal state: config updates are no-ops; the supervisor
                // is expected to remove/re-add the camera to recover.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_vendor_whitelist_rejects_onvif() {
        // ONVIF is reserved for F1b P1.D and must be rejected today.
        let err = spawn_session(CameraConfig {
            camera_id: "c1".into(),
            vendor: "onvif".into(),
            url: "http://example/onvif/device_service".into(),
            target_fps: 30,
            resolution: None,
            owner_addon_id: None,
            credentials_encrypted: None,
        })
        .unwrap_err();
        assert!(matches!(err, CameraIngestError::UnsupportedVendor(_)));
    }

    #[tokio::test]
    async fn test_rtsp_invalid_url_rejected() {
        // RTSP vendor accepted, but URL must carry the rtsp:// scheme.
        let err = spawn_session(CameraConfig {
            camera_id: "c1".into(),
            vendor: "rtsp".into(),
            url: "http://example/foo".into(),
            target_fps: 30,
            resolution: None,
            owner_addon_id: None,
            credentials_encrypted: None,
        })
        .unwrap_err();
        assert!(matches!(err, CameraIngestError::InvalidUrl(_)));
    }

    #[tokio::test]
    async fn test_path_nonexistent_rejected() {
        let err = spawn_session(CameraConfig {
            camera_id: "c1".into(),
            vendor: "fake_file".into(),
            url: "/definitely/not/here.mp4".into(),
            target_fps: 30,
            resolution: None,
            owner_addon_id: None,
            credentials_encrypted: None,
        })
        .unwrap_err();
        assert!(matches!(err, CameraIngestError::FileNotFound(_)));
    }

    #[tokio::test]
    async fn test_symlink_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.mp4");
        std::fs::write(&target, b"x").unwrap();
        let link = dir.path().join("link.mp4");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = spawn_session(CameraConfig {
            camera_id: "c1".into(),
            vendor: "fake_file".into(),
            url: link.to_string_lossy().to_string(),
            target_fps: 30,
            resolution: None,
            owner_addon_id: None,
            credentials_encrypted: None,
        })
        .unwrap_err();
        assert!(matches!(err, CameraIngestError::SymlinkNotAllowed(_)));
    }

    #[tokio::test]
    async fn test_target_fps_zero_rejected() {
        let err = spawn_session(CameraConfig {
            camera_id: "c1".into(),
            vendor: "fake_file".into(),
            url: "/tmp/whatever.mp4".into(),
            target_fps: 0,
            resolution: None,
            owner_addon_id: None,
            credentials_encrypted: None,
        })
        .unwrap_err();
        assert!(matches!(err, CameraIngestError::InvalidConfig(_)));
    }

    #[tokio::test]
    async fn test_target_fps_over_60_rejected() {
        let err = spawn_session(CameraConfig {
            camera_id: "c1".into(),
            vendor: "fake_file".into(),
            url: "/tmp/whatever.mp4".into(),
            target_fps: 61,
            resolution: None,
            owner_addon_id: None,
            credentials_encrypted: None,
        })
        .unwrap_err();
        assert!(matches!(err, CameraIngestError::InvalidConfig(_)));
    }

    #[tokio::test]
    async fn test_symlink_in_parent_rejected() {
        // The leaf is a regular file, but a parent directory on the path is
        // a symlink. resolve_file_url must reject the path before
        // canonicalize collapses the indirection.
        let dir = tempfile::tempdir().unwrap();
        let real_subdir = dir.path().join("real_dir");
        std::fs::create_dir(&real_subdir).unwrap();
        let target = real_subdir.join("file.mp4");
        std::fs::write(&target, b"x").unwrap();
        let link_dir = dir.path().join("link_dir");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real_subdir, &link_dir).unwrap();
        let path_via_link = link_dir.join("file.mp4");
        let err = super::super::fakefile::resolve_file_url(path_via_link.to_str().unwrap())
            .unwrap_err();
        assert!(
            matches!(err, CameraIngestError::SymlinkNotAllowed(_)),
            "expected SymlinkNotAllowed, got: {err:?}"
        );
    }
}

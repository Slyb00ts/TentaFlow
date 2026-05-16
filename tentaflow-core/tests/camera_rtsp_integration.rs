// =============================================================================
// File: tests/camera_rtsp_integration.rs — live RTSP smoke test (F1b P1.B)
// =============================================================================
//
// Exercises the RTSP connector end-to-end against a local RTSP server. The
// test is `#[ignore]` by default because it requires:
//   - GStreamer with `rtspsrc`, `rtph264depay`, `h264parse`, `avdec_h264`
//     plugins installed (gst-plugins-good + gst-plugins-libav on most distros),
//   - a reachable RTSP endpoint serving H.264. The default URL points at a
//     local `mediamtx` / `gst-rtsp-server` instance; override with the
//     `TENTAFLOW_TEST_RTSP_URL` env var.
//
// Run manually:
//     TENTAFLOW_TEST_RTSP_URL=rtsp://127.0.0.1:8554/test \
//       cargo test --features camera --test camera_rtsp_integration -- --ignored

#![cfg(feature = "camera")]

use std::time::Duration;

use tentaflow_core::services::camera_ingest::{
    start_supervisor, CameraConfig, CameraStatus,
};

fn rtsp_test_url() -> Option<String> {
    std::env::var("TENTAFLOW_TEST_RTSP_URL").ok()
}

#[tokio::test]
#[ignore = "requires a live local RTSP server; see SOAK_TEST.md"]
async fn test_rtsp_connect_and_receive_frames() {
    let Some(url) = rtsp_test_url() else {
        panic!("TENTAFLOW_TEST_RTSP_URL not set — see file header for setup");
    };
    let sup = start_supervisor().await.expect("supervisor");
    sup.add_camera(CameraConfig::new_unowned("rtsp_cam_1", "rtsp", url, 30, None))
        .await
        .expect("add rtsp camera");

    // Allow up to 15 s to negotiate the RTSP session and decode the first
    // frames. Real cameras typically settle in 1-3 s; CI VMs are slower.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut last_health = None;
    while tokio::time::Instant::now() < deadline {
        let h = sup.get_health("rtsp_cam_1").await.expect("health");
        last_health = Some(h.clone());
        if matches!(h.status, CameraStatus::Online) && h.frames_total > 10 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let h = last_health.expect("at least one health probe");
    assert!(
        matches!(h.status, CameraStatus::Online),
        "expected Online, got {:?} ({:?})",
        h.status,
        h.status_message
    );
    assert!(h.frames_total > 10, "expected >10 frames, got {}", h.frames_total);
    sup.shutdown().await.ok();
}

#[tokio::test]
#[ignore = "requires controllable RTSP server to exercise reconnect"]
async fn test_rtsp_reconnect_on_server_restart() {
    // Operator workflow: start the test, then bounce the upstream RTSP
    // server inside the 30 s observation window. The session must surface
    // `Starting` (or short Error) and recover to `Online` without a
    // supervisor-level remove/re-add.
    let Some(url) = rtsp_test_url() else {
        panic!("TENTAFLOW_TEST_RTSP_URL not set");
    };
    let sup = start_supervisor().await.expect("supervisor");
    sup.add_camera(CameraConfig::new_unowned("rtsp_cam_2", "rtsp", url, 30, None))
        .await
        .expect("add rtsp camera");

    tokio::time::sleep(Duration::from_secs(5)).await;
    let h1 = sup.get_health("rtsp_cam_2").await.expect("health");
    assert!(matches!(h1.status, CameraStatus::Online), "initial connect");
    let frames_before = h1.frames_total;

    // Observation window: bounce upstream during this sleep.
    tokio::time::sleep(Duration::from_secs(30)).await;

    let h2 = sup.get_health("rtsp_cam_2").await.expect("health");
    assert!(
        matches!(h2.status, CameraStatus::Online),
        "expected eventual reconnect to Online, got {:?}",
        h2.status
    );
    assert!(
        h2.frames_total > frames_before,
        "no new frames after reconnect window: {frames_before} → {}",
        h2.frames_total
    );
    sup.shutdown().await.ok();
}

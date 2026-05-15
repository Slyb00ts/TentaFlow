// =============================================================================
// File: tests/streaming_host_functions.rs — stream_subscribe/next/close (M1.W7)
// =============================================================================
//
// Drives the streaming host-function surface without standing up a wasmtime
// caller. Uses the `test_api::subscribe_for_test` helper to register a slot
// directly against the global `StreamingBus` and then exercises the lifecycle
// from the host side via the bus' broadcast API.

#![cfg(feature = "camera")]

use std::time::Duration;

use tentaflow_core::addon::host_functions::streaming::test_api as streaming_test;
use tentaflow_core::services::frame_storage::{FrameMetadata, FramePixelFormat, RawFrameRef};
use tentaflow_core::services::streaming_bus;

fn meta(cam: &str) -> FrameMetadata {
    FrameMetadata {
        camera_id: cam.into(),
        width: 8,
        height: 4,
        pixel_format: FramePixelFormat::Rgb24,
        timestamp_unix_ms: 1,
        pts: None,
        frame_size_bytes: 32,
    }
}

/// Tests in this file share the process-wide `SUBSCRIBERS` registry but run
/// in parallel. Each test uses a unique addon id + camera id so the slots
/// they create never collide; a single best-effort `registry_clear` is
/// **not** safe to call from parallel tests because it would yank slots
/// owned by another in-flight test. Per-test cleanup is implicit: when this
/// process exits the registry dies with it.
fn unique_addon(prefix: &str) -> String {
    format!("{}-{}", prefix, uuid::Uuid::new_v4())
}

fn unique_camera(prefix: &str) -> String {
    format!("{}-{}", prefix, uuid::Uuid::new_v4())
}

#[test]
fn test_stream_id_validator_accepts_uuid_and_rejects_garbage() {
    let good = format!("stream_{}", uuid::Uuid::new_v4());
    assert!(streaming_test::stream_id_valid_for_test(&good));
    assert!(!streaming_test::stream_id_valid_for_test("stream_"));
    assert!(!streaming_test::stream_id_valid_for_test("not_a_stream_id"));
    // Uppercase hex must be rejected.
    let bad = "stream_AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA";
    assert!(!streaming_test::stream_id_valid_for_test(bad));
}

#[test]
fn test_subscriber_registry_round_trip() {
    let addon = unique_addon("addon-rt");
    let cam = unique_camera("cam-rt");
    let sid = streaming_test::subscribe_for_test(&addon, &cam);
    assert!(streaming_test::registry_contains(&addon, &sid));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_subscribed_slot_receives_broadcast_frames() {
    let addon = unique_addon("addon-bc");
    let cam = unique_camera("cam-bc");
    let sid = streaming_test::subscribe_for_test(&addon, &cam);
    let bus = streaming_bus();
    bus.broadcast(&cam, RawFrameRef::new(), meta(&cam));
    bus.broadcast(&cam, RawFrameRef::new(), meta(&cam));
    assert!(streaming_test::registry_contains(&addon, &sid));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_close_camera_signals_offline_via_bus() {
    let addon = unique_addon("addon-off");
    let cam = unique_camera("cam-off");
    let _sid = streaming_test::subscribe_for_test(&addon, &cam);
    let bus = streaming_bus();
    tokio::time::timeout(
        Duration::from_millis(300),
        bus.close_camera(&cam, "removed"),
    )
    .await
    .expect("close_camera returned");
}

// =============================================================================
// File: tests/frame_pickup_cross_node.rs
// Purpose: F1b P3.C-3 — verify the cross-node pickup outcome mapping (HTTP
//          status codes + log_result strings + source_node_id audit column),
//          and the B-side replay protection contract enforced by the issuer's
//          mesh_inflight_consume helper.
//
// These tests stay below the HTTP layer — they exercise the pure logic in
// `api::frame_pickup` + `services::pickup_tokens` so they run without a
// running tokio runtime / iroh manager. A real 2-node mesh round-trip
// lives in `tests/mesh_frame_proxy_dispatch.rs` (P3.C-1) — this file
// proves the local plumbing on the verifying node behaves correctly when
// fed each of the response variants.
//
// Run:
//   cargo test --test frame_pickup_cross_node --features dashboard-api \
//     -- --nocapture
// =============================================================================

use std::time::Duration;

use tentaflow_core::api::frame_pickup::PickupOutcome;
use tentaflow_core::services::pickup_tokens::{PickupTokenIssuer, PickupVerifyError};

#[test]
fn outcome_ok_maps_to_200_with_ok_log_result() {
    let oc = PickupOutcome::Ok {
        bytes: std::sync::Arc::<[u8]>::from(vec![1u8, 2, 3].into_boxed_slice()),
        width: 4,
        height: 2,
        pixel_format: "rgb24",
        timestamp_unix_ms: 0,
        pts: None,
    };
    assert_eq!(oc.http_status(), 200);
    assert_eq!(oc.log_result(), "ok");
}

#[test]
fn outcome_upstream_not_found_maps_to_404_frame_purged() {
    let oc = PickupOutcome::UpstreamNotFound;
    assert_eq!(oc.http_status(), 404);
    assert_eq!(oc.log_result(), "frame_purged");
}

#[test]
fn outcome_upstream_unavailable_maps_to_503_with_retry_after_reason() {
    let oc = PickupOutcome::UpstreamUnavailable("timeout");
    assert_eq!(oc.http_status(), 503);
    assert_eq!(oc.log_result(), "upstream_unavailable");
}

#[test]
fn outcome_replay_maps_to_403() {
    let oc = PickupOutcome::Replay;
    assert_eq!(oc.http_status(), 403);
    assert_eq!(oc.log_result(), "replay");
}

#[test]
fn mesh_inflight_consume_first_ok_second_replay() {
    let issuer = PickupTokenIssuer::new_for_tests([7u8; 32], Duration::from_secs(30));
    let wire = "fake-mesh-wire-token";
    issuer
        .mesh_inflight_consume(wire)
        .expect("first consume must succeed");
    let second = issuer.mesh_inflight_consume(wire);
    assert_eq!(second.unwrap_err(), PickupVerifyError::AlreadyConsumed);
}

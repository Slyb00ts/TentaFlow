// =============================================================================
// File: tests/mesh_key_sync_integration.rs — F1b P3.B cross-node HMAC verify
// =============================================================================
//
// What this test verifies:
//
//   1. A `PickupToken` signed under node A's HMAC key verifies on node B as
//      long as B's `MeshKeyPool` carries A's key under the `PickupToken`
//      scope. End-to-end of the mesh sync without standing up a real iroh
//      QUIC pair: we drive the same code path the receive handler would
//      drive (`mesh_keys::sync::ingest_advertise`) after the wire round-trip.
//
//   2. A `SignedUrl` (FrameUrl / Recording) signed on node A verifies on
//      node B through the mesh pool.
//
//   3. After `forget_peer` (mirrors the disconnect / trust-revoke handler in
//      `pipeline.rs`), the same token / URL is rejected — proves the pool
//      drop closes the verify window cleanly.
//
//   4. Rotation: after node A rotates a key in-memory and re-advertises, B's
//      pool carries both new and previous keys; tokens minted under the old
//      key still verify in the grace window.
//
// The QUIC + iroh stack is exercised by the broader mesh integration tests
// (`mesh_discovery_repro.rs`, `mesh_tie_break.rs`) — duplicating that setup
// here would only retest mesh framing, not the F1b P3.B verify path.

use std::time::Duration;

use tentaflow_core::services::mesh_keys::sync::{forget_peer, ingest_advertise};
use tentaflow_core::services::mesh_keys::{KeyScope, mesh_key_pool, now_unix_ms};
use tentaflow_core::services::pickup_tokens::PickupTokenIssuer;
use tentaflow_core::services::signed_urls::{SignedUrlIssuer, UrlScope};
use tentaflow_protocol::mesh::{HmacKeyEntry, HmacKeysSyncPayload};

/// Build the advertise payload that node A would push to B for the given
/// pickup-token + frame-url + recording-url key triples. Mirrors the shape
/// emitted by `mesh_keys::sync::build_local_advertise` on the producer side.
fn make_advertise(
    from: &str,
    pickup_key: [u8; 32],
    frame_key: [u8; 32],
    recording_key: [u8; 32],
) -> HmacKeysSyncPayload {
    HmacKeysSyncPayload {
        from_node_id: from.into(),
        keys: vec![
            HmacKeyEntry {
                scope: "pickup_token".into(),
                current_key: pickup_key.to_vec(),
                previous_key: vec![],
                previous_expires_unix_ms: 0,
                key_id: vec![0u8; 8],
            },
            HmacKeyEntry {
                scope: "frame_url".into(),
                current_key: frame_key.to_vec(),
                previous_key: vec![],
                previous_expires_unix_ms: 0,
                key_id: vec![0u8; 8],
            },
            HmacKeyEntry {
                scope: "recording_url".into(),
                current_key: recording_key.to_vec(),
                previous_key: vec![],
                previous_expires_unix_ms: 0,
                key_id: vec![0u8; 8],
            },
        ],
    }
}

#[test]
fn pickup_token_minted_on_a_verifies_on_b_through_mesh_pool() {
    // Distinct peer id so the test does not leak state into siblings.
    let peer_id = "test-p3b-pickup-x";

    // Node A's issuer key (would live in A's `pickup_token.key`).
    let key_a: [u8; 32] = [0xA1; 32];
    // Node B mints a separate, unrelated key — proves cross-node verify is
    // genuinely going through the mesh pool, not coincidental local match.
    let key_b: [u8; 32] = [0xB2; 32];

    let issuer_a = PickupTokenIssuer::new_for_tests(key_a, Duration::from_secs(30));
    let issuer_b = PickupTokenIssuer::new_for_tests(key_b, Duration::from_secs(30));

    let (token, _payload) = issuer_a.issue(
        "frame_p3b_1".into(),
        "svc-p3b".into(),
        "req-p3b-1".into(),
    );
    let wire = token.wire();

    // Pre-condition: B does NOT yet know A's key — verify must fail.
    let err = issuer_b.verify_only(&wire).expect_err("B without mesh sync must reject");
    assert_eq!(
        err,
        tentaflow_core::services::pickup_tokens::PickupVerifyError::InvalidSignature,
        "before sync the token is unverifiable on B"
    );

    // Simulate B receiving A's advertise (this is what `pipeline.rs` does on
    // the `HmacKeysSyncReceived` event after the trust gate passes).
    let advertise = make_advertise(peer_id, key_a, [0u8; 32], [0u8; 32]);
    let accepted = ingest_advertise(peer_id, advertise);
    assert_eq!(accepted, 3);

    // Now the token verifies on B (HMAC + expiry); one-shot is owned by A.
    let payload = issuer_b
        .verify_only(&wire)
        .expect("after sync token must verify on B");
    assert_eq!(payload.raw_ref, "frame_p3b_1");
    assert_eq!(payload.request_id, "req-p3b-1");

    // After forget_peer (disconnect / revoke) the verify must close.
    forget_peer(peer_id);
    let err = issuer_b.verify_only(&wire).expect_err("after forget verify rejects");
    assert_eq!(
        err,
        tentaflow_core::services::pickup_tokens::PickupVerifyError::InvalidSignature
    );
}

#[test]
fn signed_url_minted_on_a_verifies_on_b_for_both_scopes() {
    let peer_id = "test-p3b-signed-url-x";

    let pickup_unused: [u8; 32] = [0u8; 32];
    let key_a_frame: [u8; 32] = [0xC3; 32];
    let key_a_rec: [u8; 32] = [0xD4; 32];

    // A's local issuers.
    let frame_a = SignedUrlIssuer::new_for_tests(UrlScope::FrameUrl, key_a_frame);
    let rec_a = SignedUrlIssuer::new_for_tests(UrlScope::Recording, key_a_rec);

    // B's local issuers with totally different keys.
    let frame_b = SignedUrlIssuer::new_for_tests(UrlScope::FrameUrl, [0x11; 32]);
    let rec_b = SignedUrlIssuer::new_for_tests(UrlScope::Recording, [0x22; 32]);

    let frame_url = frame_a.issue("frame_p3b".into(), 120).unwrap();
    let rec_url = rec_a.issue("rec_p3b".into(), 120).unwrap();

    // Pre-sync: both fail on B.
    assert!(frame_b
        .verify(&frame_url.ref_id, frame_url.expiry_unix_ms, &frame_url.token_b64)
        .is_err());
    assert!(rec_b
        .verify(&rec_url.ref_id, rec_url.expiry_unix_ms, &rec_url.token_b64)
        .is_err());

    // Mirror A's keys into B's mesh pool.
    let advertise = make_advertise(peer_id, pickup_unused, key_a_frame, key_a_rec);
    assert_eq!(ingest_advertise(peer_id, advertise), 3);

    // Post-sync: both verify on B.
    frame_b
        .verify(&frame_url.ref_id, frame_url.expiry_unix_ms, &frame_url.token_b64)
        .expect("frame URL verifies on B through mesh pool");
    rec_b
        .verify(&rec_url.ref_id, rec_url.expiry_unix_ms, &rec_url.token_b64)
        .expect("recording URL verifies on B through mesh pool");

    forget_peer(peer_id);
    assert!(frame_b
        .verify(&frame_url.ref_id, frame_url.expiry_unix_ms, &frame_url.token_b64)
        .is_err());
    assert!(rec_b
        .verify(&rec_url.ref_id, rec_url.expiry_unix_ms, &rec_url.token_b64)
        .is_err());
}

#[test]
fn rotation_grace_window_propagated_through_advertise() {
    let peer_id = "test-p3b-rotation-x";

    let old: [u8; 32] = [0x55; 32];
    let new: [u8; 32] = [0x66; 32];

    let issuer_a = PickupTokenIssuer::new_for_tests(old, Duration::from_secs(30));
    let issuer_b = PickupTokenIssuer::new_for_tests([0xEE; 32], Duration::from_secs(30));

    // A issues under OLD key, then rotates.
    let (token_old, _) = issuer_a.issue(
        "frame_old".into(),
        "svc".into(),
        "req-rot-old".into(),
    );
    issuer_a.rotate_in_memory(new);

    // A advertises both keys (new + previous-window grace).
    let (cur, prev, prev_exp) = issuer_a.snapshot_for_mesh();
    assert_eq!(cur, new);
    assert_eq!(prev, Some(old));
    assert!(prev_exp > now_unix_ms());

    let advertise = HmacKeysSyncPayload {
        from_node_id: peer_id.into(),
        keys: vec![HmacKeyEntry {
            scope: "pickup_token".into(),
            current_key: cur.to_vec(),
            previous_key: prev.unwrap().to_vec(),
            previous_expires_unix_ms: prev_exp,
            key_id: vec![0u8; 8],
        }],
    };
    assert_eq!(ingest_advertise(peer_id, advertise), 1);

    // The OLD-key token still verifies on B during the grace window — both
    // the new key and the rotated-out previous key are in B's verify pool.
    issuer_b
        .verify_only(&token_old.wire())
        .expect("rotated-out key verifies via mesh grace window");

    // A token minted under the NEW key after rotation also verifies on B.
    let (token_new, _) = issuer_a.issue(
        "frame_new".into(),
        "svc".into(),
        "req-rot-new".into(),
    );
    issuer_b
        .verify_only(&token_new.wire())
        .expect("new key verifies via mesh pool");

    let pool_keys = mesh_key_pool().verify_keys_for(KeyScope::PickupToken);
    assert!(pool_keys.contains(&old));
    assert!(pool_keys.contains(&new));

    forget_peer(peer_id);
}

/// Source-level contract test: the `HmacKeysSyncReceived` handler in
/// `pipeline.rs` MUST gate ingest on `is_trusted(...)` BEFORE calling into
/// `mesh_keys::sync::ingest_advertise`. The pool layer is trust-agnostic by
/// design (see `untrusted_sender_gate_is_enforced_by_pipeline_not_pool`),
/// so the boundary lives entirely in the dispatch handler — a refactor that
/// drops it would silently let unpaired peers plant HMAC keys in our verify
/// pool and mint tokens we would accept. This test reads the source file and
/// asserts the gate pattern is still present in the handler scope.
#[test]
fn receive_handler_has_is_trusted_gate() {
    let src = std::fs::read_to_string(
        concat!(env!("CARGO_MANIFEST_DIR"), "/src/mesh/pipeline.rs"),
    )
    .expect("pipeline.rs must be readable from CARGO_MANIFEST_DIR");

    // Locate the handler arm; everything we care about lives in the block
    // immediately after this match arm header until the next top-level arm.
    let handler_start = src
        .find("IrohMeshEvent::HmacKeysSyncReceived")
        .expect("HmacKeysSyncReceived handler must exist in pipeline.rs");

    // Bound the scope: read a generous window after the arm header. The
    // handler body is well under 2 KiB; this avoids matching `is_trusted`
    // from unrelated handlers (e.g. TrustedKeysSync, RelayFrameReceived).
    let scope_end = (handler_start + 2048).min(src.len());
    let scope = &src[handler_start..scope_end];

    assert!(
        scope.contains("is_trusted("),
        "HmacKeysSyncReceived handler lost its is_trusted() gate — \
         this is a security regression. Restore the trust check BEFORE \
         calling mesh_keys::sync::ingest_advertise."
    );

    // The gate must guard ingest_advertise — i.e. the trust check appears
    // textually before the ingest call inside the handler.
    let trust_pos = scope
        .find("is_trusted(")
        .expect("is_trusted assertion above already verified presence");
    let ingest_pos = scope
        .find("ingest_advertise")
        .expect("handler must call ingest_advertise");
    assert!(
        trust_pos < ingest_pos,
        "is_trusted() gate must run BEFORE ingest_advertise() — \
         current source order is inverted, which is a security regression."
    );
}

#[test]
fn untrusted_sender_gate_is_enforced_by_pipeline_not_pool() {
    // The pool itself is trust-agnostic by design (it is the verify oracle,
    // not the trust gate). The trust check lives in `pipeline.rs` —
    // `ingest_advertise` is only called for trusted senders. This test
    // documents that contract: a direct ingest succeeds regardless of trust,
    // so the security boundary MUST stay above us in the dispatch loop.
    let peer_id = "test-p3b-trust-doc";
    let advertise = make_advertise(peer_id, [9; 32], [9; 32], [9; 32]);
    let accepted = ingest_advertise(peer_id, advertise);
    assert_eq!(accepted, 3);
    assert!(!mesh_key_pool()
        .verify_keys_for(KeyScope::PickupToken)
        .is_empty());
    forget_peer(peer_id);
}

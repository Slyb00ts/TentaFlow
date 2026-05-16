// =============================================================================
// File: services/mesh_keys/sync.rs — advertise + receive logic for mesh HMAC sync
// =============================================================================
//
// Glue between the local HMAC issuers (`pickup_tokens` + `signed_urls`),
// the rkyv wire payload (`tentaflow_protocol::mesh::HmacKeysSyncPayload`),
// and the in-memory `MeshKeyPool`. The actual QUIC send path is in
// `mesh::iroh_manager::send_hmac_keys_sync`; this module only deals with
// payload construction + post-receive ingestion.

use tracing::{debug, warn};

use tentaflow_protocol::mesh::{HmacKeyEntry, HmacKeysSyncPayload};

use crate::services::mesh_keys::{
    mesh_key_pool, short_key_id, KeyScope, PeerKeyState,
};
use crate::services::{frame_url_issuer, pickup_token_issuer, recording_url_issuer};

/// Build the advertise payload from the live local issuer state. Called every
/// time we want to push our keys to a peer — initial advertise on
/// `PeerConnected`, and re-advertise after a local key rotation.
pub fn build_local_advertise(local_node_id: &str) -> HmacKeysSyncPayload {
    let mut entries = Vec::with_capacity(3);
    {
        let (cur, prev, prev_exp) = pickup_token_issuer().snapshot_for_mesh();
        entries.push(make_entry(KeyScope::PickupToken, cur, prev, prev_exp));
    }
    {
        let (cur, prev, prev_exp) = frame_url_issuer().snapshot_for_mesh();
        entries.push(make_entry(KeyScope::FrameUrl, cur, prev, prev_exp));
    }
    {
        let (cur, prev, prev_exp) = recording_url_issuer().snapshot_for_mesh();
        entries.push(make_entry(KeyScope::RecordingUrl, cur, prev, prev_exp));
    }
    HmacKeysSyncPayload {
        from_node_id: local_node_id.to_string(),
        keys: entries,
    }
}

fn make_entry(
    scope: KeyScope,
    current: [u8; 32],
    previous: Option<[u8; 32]>,
    previous_expires_unix_ms: u64,
) -> HmacKeyEntry {
    HmacKeyEntry {
        scope: scope.as_str().to_string(),
        current_key: current.to_vec(),
        previous_key: previous.map(|p| p.to_vec()).unwrap_or_default(),
        previous_expires_unix_ms,
        key_id: short_key_id(&current).to_vec(),
    }
}

/// Encode an advertise payload into rkyv bytes ready for
/// `IrohMeshManager::send_to_peer`. Returns `None` on serialization failure
/// (logged) so callers can simply skip the send.
pub fn encode_advertise(payload: &HmacKeysSyncPayload) -> Option<Vec<u8>> {
    match rkyv::to_bytes::<rkyv::rancor::Error>(payload) {
        Ok(b) => Some(b.to_vec()),
        Err(e) => {
            warn!("mesh_keys: failed to encode HmacKeysSync: {}", e);
            None
        }
    }
}

/// Ingest an advertise we just received from `peer_id`. Caller is responsible
/// for the trust gate — this function assumes the sender is already trusted
/// (mirror of the `TrustedKeysSync` accept path in `pipeline.rs`).
pub fn ingest_advertise(peer_id: &str, payload: HmacKeysSyncPayload) -> u32 {
    let pool = mesh_key_pool();
    let mut accepted = 0u32;
    for entry in payload.keys {
        let Some(scope) = KeyScope::from_str(&entry.scope) else {
            warn!(peer = peer_id, scope = %entry.scope, "mesh_keys: unknown scope");
            continue;
        };
        let Some(current) = bytes_to_key(&entry.current_key) else {
            warn!(
                peer = peer_id,
                scope = entry.scope.as_str(),
                got = entry.current_key.len(),
                "mesh_keys: current_key wrong length, expected 32"
            );
            continue;
        };
        let previous = if entry.previous_key.is_empty() {
            None
        } else {
            bytes_to_key(&entry.previous_key)
        };
        let state = PeerKeyState {
            current,
            previous,
            previous_expires_unix_ms: entry.previous_expires_unix_ms,
        };
        let old_id = pool.upsert(peer_id, scope, state);
        let new_id = short_key_id(&current);
        debug!(
            peer = peer_id,
            scope = entry.scope.as_str(),
            new_key_id = %hex8(&new_id),
            old_key_id = old_id.as_ref().map(|b| hex8(b)).unwrap_or_default(),
            has_previous = previous.is_some(),
            "mesh_keys: advertise ingested",
        );
        accepted += 1;
    }
    accepted
}

/// Drop every scope held for `peer_id` — called on disconnect or trust revoke.
pub fn forget_peer(peer_id: &str) {
    mesh_key_pool().remove_peer(peer_id);
    debug!(peer = peer_id, "mesh_keys: forgot peer keys");
}

fn bytes_to_key(b: &[u8]) -> Option<[u8; 32]> {
    if b.len() != 32 {
        return None;
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(b);
    Some(out)
}

fn hex8(b: &[u8; 8]) -> String {
    let mut s = String::with_capacity(16);
    for byte in b {
        s.push_str(&format!("{:02x}", byte));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_then_pool_returns_keys() {
        // Use a unique peer id so the test does not interact with the
        // process-wide singleton across other tests.
        let peer = "test-ingest-peer";
        let payload = HmacKeysSyncPayload {
            from_node_id: peer.into(),
            keys: vec![
                HmacKeyEntry {
                    scope: KeyScope::PickupToken.as_str().into(),
                    current_key: vec![1u8; 32],
                    previous_key: vec![2u8; 32],
                    previous_expires_unix_ms: crate::services::mesh_keys::now_unix_ms()
                        + 30_000,
                    key_id: vec![0u8; 8],
                },
                HmacKeyEntry {
                    scope: KeyScope::FrameUrl.as_str().into(),
                    current_key: vec![3u8; 32],
                    previous_key: vec![],
                    previous_expires_unix_ms: 0,
                    key_id: vec![0u8; 8],
                },
            ],
        };
        let accepted = ingest_advertise(peer, payload);
        assert_eq!(accepted, 2);

        let keys = mesh_key_pool().verify_keys_for(KeyScope::PickupToken);
        assert!(keys.contains(&[1u8; 32]));
        assert!(keys.contains(&[2u8; 32]));

        forget_peer(peer);
    }

    #[test]
    fn ingest_rejects_wrong_length_key() {
        let peer = "test-bad-length-peer";
        let payload = HmacKeysSyncPayload {
            from_node_id: peer.into(),
            keys: vec![HmacKeyEntry {
                scope: KeyScope::FrameUrl.as_str().into(),
                current_key: vec![1u8; 16], // wrong
                previous_key: vec![],
                previous_expires_unix_ms: 0,
                key_id: vec![0u8; 8],
            }],
        };
        let accepted = ingest_advertise(peer, payload);
        assert_eq!(accepted, 0);
        forget_peer(peer);
    }

    #[test]
    fn ingest_skips_unknown_scope() {
        let peer = "test-unknown-scope-peer";
        let payload = HmacKeysSyncPayload {
            from_node_id: peer.into(),
            keys: vec![HmacKeyEntry {
                scope: "totally_made_up".into(),
                current_key: vec![1u8; 32],
                previous_key: vec![],
                previous_expires_unix_ms: 0,
                key_id: vec![0u8; 8],
            }],
        };
        assert_eq!(ingest_advertise(peer, payload), 0);
        forget_peer(peer);
    }
}

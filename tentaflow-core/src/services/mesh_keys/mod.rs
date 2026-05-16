// =============================================================================
// File: services/mesh_keys/mod.rs — in-memory pool of peer-supplied HMAC keys.
// =============================================================================
//
// F1b P3.B — multi-node mesh sync of the three HMAC issuer keys
// (pickup_token, frame_url, recording_url). After two peers complete pairing
// (P3.A guarantees on-disk persistence on each side), every node also pushes
// its current + previous-window HMAC keys to every trust-paired peer over the
// mTLS-protected mesh stream. The receiver loads those keys into this pool;
// the local issuers' verify paths fold them into the candidate list so a
// token minted on node A is acceptable when picked up at node B.
//
// Layout:
//
//   MeshKeyPool
//     └── peers: RwLock<HashMap<NodeId, PerPeerKeys>>
//                                     ├── pickup_token: Option<PeerKeyState>
//                                     ├── frame_url:    Option<PeerKeyState>
//                                     └── recording_url:Option<PeerKeyState>
//
// Storage choice — `RwLock<HashMap>` rather than `DashMap`:
//   * Writers (advertise receive, peer disconnect, trust revoke) fire on the
//     order of seconds-to-minutes. Almost zero contention.
//   * Hot path is the verifier: `pickup_token_issuer().verify_only()` calls
//     `verify_keys_for()` on every token. That path takes a single read lock,
//     collects up to (1 + 2*N_peers) candidate keys into a SmallVec-sized
//     Vec, and drops the lock before any HMAC work. With N_peers ≤ ~100 the
//     allocation is a few hundred bytes — cheaper than the DashMap shard
//     traversal we would otherwise pay per scope.
//
// Persistence — none. Peer keys live only while the peer is connected; on
// disconnect they are dropped and re-acquired on the next reconnect's
// advertise. This keeps the trust lifecycle clean: a revoked peer cannot
// leave stale keys behind on disk.

pub mod sync;

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use sha2::{Digest, Sha256};

use crate::services::pickup_tokens::KEY_NAME as PICKUP_TOKEN_KEY_NAME;
use crate::services::signed_urls::UrlScope;

/// Issuer scopes whose keys are mirrored to peers. Each maps 1:1 to the
/// on-disk key file name under `services::key_storage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyScope {
    PickupToken,
    FrameUrl,
    RecordingUrl,
}

impl KeyScope {
    /// Wire-stable scope identifier — matches `services::key_storage` file
    /// stems. Used as the `scope` field in `HmacKeyEntry`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PickupToken => PICKUP_TOKEN_KEY_NAME,
            Self::FrameUrl => UrlScope::FrameUrl.key_name(),
            Self::RecordingUrl => UrlScope::Recording.key_name(),
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        if s == PICKUP_TOKEN_KEY_NAME {
            Some(Self::PickupToken)
        } else if s == UrlScope::FrameUrl.key_name() {
            Some(Self::FrameUrl)
        } else if s == UrlScope::Recording.key_name() {
            Some(Self::RecordingUrl)
        } else {
            None
        }
    }

    pub const ALL: [KeyScope; 3] = [
        KeyScope::PickupToken,
        KeyScope::FrameUrl,
        KeyScope::RecordingUrl,
    ];
}

/// One peer's key state for one scope: current signing key plus an optional
/// previous-window key with absolute expiry. Mirrors the in-memory shape of
/// the local `KeyState` in `pickup_tokens` / `signed_urls`, but holds the
/// *peer's* secrets and is verifier-only.
#[derive(Debug, Clone)]
pub struct PeerKeyState {
    pub current: [u8; 32],
    pub previous: Option<[u8; 32]>,
    /// Absolute unix-ms past which `previous` is no longer accepted. 0 means
    /// no previous key is in play.
    pub previous_expires_unix_ms: u64,
}

impl PeerKeyState {
    /// Iterate over the currently-valid candidate keys at `now_unix_ms`.
    /// Always yields `current`, plus `previous` if the rotation grace window
    /// is still open.
    pub fn candidates(&self, now_unix_ms: u64) -> impl Iterator<Item = &[u8; 32]> {
        let prev = self
            .previous
            .as_ref()
            .filter(|_| now_unix_ms < self.previous_expires_unix_ms);
        std::iter::once(&self.current).chain(prev)
    }
}

#[derive(Debug, Default)]
struct PerPeerKeys {
    pickup_token: Option<PeerKeyState>,
    frame_url: Option<PeerKeyState>,
    recording_url: Option<PeerKeyState>,
}

impl PerPeerKeys {
    fn slot_mut(&mut self, scope: KeyScope) -> &mut Option<PeerKeyState> {
        match scope {
            KeyScope::PickupToken => &mut self.pickup_token,
            KeyScope::FrameUrl => &mut self.frame_url,
            KeyScope::RecordingUrl => &mut self.recording_url,
        }
    }

    fn slot(&self, scope: KeyScope) -> &Option<PeerKeyState> {
        match scope {
            KeyScope::PickupToken => &self.pickup_token,
            KeyScope::FrameUrl => &self.frame_url,
            KeyScope::RecordingUrl => &self.recording_url,
        }
    }
}

/// Process-wide pool of peer-supplied HMAC keys. Single read lock per verify
/// hot-path call site.
pub struct MeshKeyPool {
    peers: RwLock<HashMap<String, PerPeerKeys>>,
}

impl MeshKeyPool {
    fn new() -> Self {
        Self {
            peers: RwLock::new(HashMap::new()),
        }
    }

    /// Replace the `scope` entry for `peer_id` with `state` — used when an
    /// advertise lands. Returns the previous key_id (if any) for logging.
    pub fn upsert(&self, peer_id: &str, scope: KeyScope, state: PeerKeyState) -> Option<[u8; 8]> {
        let mut peers = self.peers.write();
        let entry = peers.entry(peer_id.to_string()).or_default();
        let old = entry.slot(scope).as_ref().map(|s| short_key_id(&s.current));
        *entry.slot_mut(scope) = Some(state);
        old
    }

    /// Drop every scope for `peer_id` — called on disconnect or trust revoke.
    pub fn remove_peer(&self, peer_id: &str) {
        self.peers.write().remove(peer_id);
    }

    /// Hot-path: collect every currently-valid candidate key for `scope`
    /// across all peers. Single read-lock acquisition; no HMAC work happens
    /// while the lock is held.
    pub fn verify_keys_for(&self, scope: KeyScope) -> Vec<[u8; 32]> {
        let now = now_unix_ms();
        let peers = self.peers.read();
        let mut out = Vec::with_capacity(peers.len() * 2);
        for per in peers.values() {
            if let Some(state) = per.slot(scope) {
                for k in state.candidates(now) {
                    out.push(*k);
                }
            }
        }
        out
    }

    /// Like `verify_keys_for` but also returns, for each candidate key, the
    /// node id that contributed it. Used by `verify_only_with_source` so the
    /// caller can record which peer's HMAC key matched a verified token
    /// (audit chain for mesh-fallback pickups, F1b P3.C-2).
    pub fn verify_keys_with_peers_for(&self, scope: KeyScope) -> Vec<(String, [u8; 32])> {
        let now = now_unix_ms();
        let peers = self.peers.read();
        let mut out = Vec::with_capacity(peers.len() * 2);
        for (node_id, per) in peers.iter() {
            if let Some(state) = per.slot(scope) {
                for k in state.candidates(now) {
                    out.push((node_id.clone(), *k));
                }
            }
        }
        out
    }

    /// Diagnostic — number of peers currently contributing to the pool.
    pub fn peer_count(&self) -> usize {
        self.peers.read().len()
    }

    /// Diagnostic — number of (peer, scope) entries.
    #[doc(hidden)]
    pub fn entries_for(&self, scope: KeyScope) -> usize {
        self.peers
            .read()
            .values()
            .filter(|p| p.slot(scope).is_some())
            .count()
    }
}

static MESH_KEY_POOL: OnceLock<Arc<MeshKeyPool>> = OnceLock::new();

/// Process-wide singleton. First call lazily constructs an empty pool.
pub fn mesh_key_pool() -> &'static Arc<MeshKeyPool> {
    MESH_KEY_POOL.get_or_init(|| Arc::new(MeshKeyPool::new()))
}

/// 8-byte truncated SHA-256 of a key — used solely for log correlation
/// ("which key fingerprint did peer X advertise?"). Never used as trust input.
pub fn short_key_id(key: &[u8; 32]) -> [u8; 8] {
    let digest = Sha256::digest(key);
    let mut out = [0u8; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(b: u8) -> [u8; 32] {
        [b; 32]
    }

    #[test]
    fn upsert_and_verify_keys_for_returns_current_and_active_previous() {
        let pool = MeshKeyPool::new();
        pool.upsert(
            "peer-A",
            KeyScope::PickupToken,
            PeerKeyState {
                current: k(1),
                previous: Some(k(2)),
                previous_expires_unix_ms: now_unix_ms() + 60_000,
            },
        );
        pool.upsert(
            "peer-B",
            KeyScope::PickupToken,
            PeerKeyState {
                current: k(3),
                previous: None,
                previous_expires_unix_ms: 0,
            },
        );

        let keys = pool.verify_keys_for(KeyScope::PickupToken);
        assert_eq!(keys.len(), 3);
        assert!(keys.contains(&k(1)));
        assert!(keys.contains(&k(2)));
        assert!(keys.contains(&k(3)));
    }

    #[test]
    fn expired_previous_window_excluded() {
        let pool = MeshKeyPool::new();
        pool.upsert(
            "peer-A",
            KeyScope::FrameUrl,
            PeerKeyState {
                current: k(1),
                previous: Some(k(9)),
                // already-past expiry
                previous_expires_unix_ms: now_unix_ms().saturating_sub(1_000),
            },
        );
        let keys = pool.verify_keys_for(KeyScope::FrameUrl);
        assert_eq!(keys, vec![k(1)]);
    }

    #[test]
    fn remove_peer_drops_all_scopes() {
        let pool = MeshKeyPool::new();
        for scope in KeyScope::ALL {
            pool.upsert(
                "peer-A",
                scope,
                PeerKeyState {
                    current: k(1),
                    previous: None,
                    previous_expires_unix_ms: 0,
                },
            );
        }
        assert_eq!(pool.entries_for(KeyScope::PickupToken), 1);
        pool.remove_peer("peer-A");
        for scope in KeyScope::ALL {
            assert_eq!(pool.verify_keys_for(scope).len(), 0);
            assert_eq!(pool.entries_for(scope), 0);
        }
    }

    #[test]
    fn scope_from_str_roundtrip() {
        for scope in KeyScope::ALL {
            assert_eq!(KeyScope::from_str(scope.as_str()), Some(scope));
        }
        assert_eq!(KeyScope::from_str("bogus"), None);
    }

    #[test]
    fn second_upsert_replaces_previous_state() {
        let pool = MeshKeyPool::new();
        pool.upsert(
            "peer-A",
            KeyScope::RecordingUrl,
            PeerKeyState {
                current: k(1),
                previous: None,
                previous_expires_unix_ms: 0,
            },
        );
        pool.upsert(
            "peer-A",
            KeyScope::RecordingUrl,
            PeerKeyState {
                current: k(2),
                previous: Some(k(1)),
                previous_expires_unix_ms: now_unix_ms() + 30_000,
            },
        );
        let keys = pool.verify_keys_for(KeyScope::RecordingUrl);
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&k(1)));
        assert!(keys.contains(&k(2)));
    }
}

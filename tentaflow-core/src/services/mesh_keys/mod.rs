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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::{Mutex, RwLock};
use sha2::{Digest, Sha256};

use crate::mesh::iroh_manager::IrohMeshManager;

use crate::services::pickup_tokens::KEY_NAME as PICKUP_TOKEN_KEY_NAME;
use crate::services::signed_urls::UrlScope;

/// Issuer scopes whose keys are mirrored to peers. Each maps 1:1 to the
/// on-disk key file name under `services::key_storage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyScope {
    PickupToken,
    FrameUrl,
    RecordingUrl,
    LegalUrl,
}

impl KeyScope {
    /// Wire-stable scope identifier — matches `services::key_storage` file
    /// stems. Used as the `scope` field in `HmacKeyEntry`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PickupToken => PICKUP_TOKEN_KEY_NAME,
            Self::FrameUrl => UrlScope::FrameUrl.key_name(),
            Self::RecordingUrl => UrlScope::Recording.key_name(),
            Self::LegalUrl => UrlScope::LegalUrl.key_name(),
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        if s == PICKUP_TOKEN_KEY_NAME {
            Some(Self::PickupToken)
        } else if s == UrlScope::FrameUrl.key_name() {
            Some(Self::FrameUrl)
        } else if s == UrlScope::Recording.key_name() {
            Some(Self::RecordingUrl)
        } else if s == UrlScope::LegalUrl.key_name() {
            Some(Self::LegalUrl)
        } else {
            None
        }
    }

    pub const ALL: [KeyScope; 4] = [
        KeyScope::PickupToken,
        KeyScope::FrameUrl,
        KeyScope::RecordingUrl,
        KeyScope::LegalUrl,
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
    legal_url: Option<PeerKeyState>,
}

impl PerPeerKeys {
    fn slot_mut(&mut self, scope: KeyScope) -> &mut Option<PeerKeyState> {
        match scope {
            KeyScope::PickupToken => &mut self.pickup_token,
            KeyScope::FrameUrl => &mut self.frame_url,
            KeyScope::RecordingUrl => &mut self.recording_url,
            KeyScope::LegalUrl => &mut self.legal_url,
        }
    }

    fn slot(&self, scope: KeyScope) -> &Option<PeerKeyState> {
        match scope {
            KeyScope::PickupToken => &self.pickup_token,
            KeyScope::FrameUrl => &self.frame_url,
            KeyScope::RecordingUrl => &self.recording_url,
            KeyScope::LegalUrl => &self.legal_url,
        }
    }
}

/// Minimum spacing between two `broadcast_on_rotate` calls for the same key
/// name. Defends against a scripted rotation loop turning into a mesh storm.
const BROADCAST_RATE_LIMIT: Duration = Duration::from_secs(5);

/// Result of a successful `broadcast_on_rotate`. `peer_count` is the number
/// of trusted peers the broadcast was attempted against; `ok_count` /
/// `err_count` split the per-peer outcomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastResult {
    pub peer_count: usize,
    pub ok_count: usize,
    pub err_count: usize,
}

/// Failure modes for `broadcast_on_rotate`. `RateLimited` is the only one a
/// caller may need to surface to the operator; the rest are diagnostic.
#[derive(Debug)]
pub enum BroadcastError {
    RateLimited { retry_after_secs: u64 },
    BuildPayloadFailed,
    NoTrustedPeers,
    Internal(String),
}

impl std::fmt::Display for BroadcastError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BroadcastError::RateLimited { retry_after_secs } => {
                write!(f, "rate limited (retry after {}s)", retry_after_secs)
            }
            BroadcastError::BuildPayloadFailed => write!(f, "failed to encode advertise payload"),
            BroadcastError::NoTrustedPeers => write!(f, "no trusted peers to broadcast to"),
            BroadcastError::Internal(msg) => write!(f, "internal error: {}", msg),
        }
    }
}

impl std::error::Error for BroadcastError {}

/// Process-wide pool of peer-supplied HMAC keys. Single read lock per verify
/// hot-path call site.
pub struct MeshKeyPool {
    peers: RwLock<HashMap<String, PerPeerKeys>>,
    /// Per-key-name timestamp of the last successful broadcast (used for the
    /// 5 s rate limit). Keyed by the scope's wire name (`pickup_token`,
    /// `frame_url`, `recording_url`). Bounded to 3 entries — never grows.
    last_broadcast: Mutex<HashMap<String, Instant>>,
}

impl MeshKeyPool {
    fn new() -> Self {
        Self {
            peers: RwLock::new(HashMap::new()),
            last_broadcast: Mutex::new(HashMap::new()),
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

    /// Acquire the per-name 5 s rate-limit slot. Public-in-crate so the
    /// gating can be exercised independently of an actual mesh handle.
    pub(crate) fn reserve_rate_limit_slot(&self, rotated_name: &str) -> Result<(), BroadcastError> {
        let mut last = self.last_broadcast.lock();
        let now = Instant::now();
        if let Some(prev) = last.get(rotated_name) {
            let elapsed = now.saturating_duration_since(*prev);
            if elapsed < BROADCAST_RATE_LIMIT {
                let retry_after_secs = (BROADCAST_RATE_LIMIT - elapsed).as_secs().saturating_add(1);
                return Err(BroadcastError::RateLimited { retry_after_secs });
            }
        }
        last.insert(rotated_name.to_string(), now);
        Ok(())
    }

    /// Test-only — reset the per-name cooldown so unit tests do not have to
    /// sleep 5 s between cases when reusing the same `MeshKeyPool` instance.
    #[cfg(test)]
    pub(crate) fn reset_rate_limit_for_tests(&self) {
        self.last_broadcast.lock().clear();
    }

    /// Trigger an immediate advertise to every trust-paired peer with the
    /// current local issuer keys. Called from the key-storage watcher right
    /// after `tentaflow-cli keys rotate <name>` lands the new bytes on disk
    /// (the issuer's own in-memory rotation has already happened by the time
    /// this fires; see `services::mod`).
    ///
    /// `rotated_name` is one of `pickup_token` / `frame_url` / `recording_url`
    /// — the wire scope identifier from `KeyScope::as_str()`. It only drives
    /// the per-name rate limit and the audit details payload: the payload
    /// itself always carries all three current keys, so peers that missed an
    /// earlier rotation catch up here too.
    ///
    /// Rate-limited to one broadcast per `rotated_name` per
    /// `BROADCAST_RATE_LIMIT` (5 s). The limit is best-effort defence against
    /// scripted rotation loops, not a security boundary.
    pub async fn broadcast_on_rotate(
        &self,
        local_node_id: &str,
        rotated_name: &str,
        iroh_manager: &Arc<IrohMeshManager>,
    ) -> Result<BroadcastResult, BroadcastError> {
        // Step 1 — rate limit window check, per scope name.
        self.reserve_rate_limit_slot(rotated_name)?;

        // Step 2 — build + encode the advertise payload from live issuer state.
        let payload = sync::build_local_advertise(local_node_id);
        let bytes = match sync::encode_advertise(&payload) {
            Some(b) => b,
            None => return Err(BroadcastError::BuildPayloadFailed),
        };

        // Step 3 — fan out to every trusted peer in parallel. `broadcast_to_trusted`
        // returns one (peer_id, Result) tuple per attempted peer.
        let results = iroh_manager
            .broadcast_ufp2_to_trusted(
                tentaflow_protocol::mesh::MESH_MSG_HMAC_KEYS_SYNC,
                &bytes,
                None,
            )
            .await;

        if results.is_empty() {
            return Err(BroadcastError::NoTrustedPeers);
        }

        let mut ok = 0usize;
        let mut err = 0usize;
        for (_peer, r) in &results {
            if r.is_ok() {
                ok += 1;
            } else {
                err += 1;
            }
        }
        Ok(BroadcastResult {
            peer_count: results.len(),
            ok_count: ok,
            err_count: err,
        })
    }
}

/// Process-wide handle the key watcher uses to drive `broadcast_on_rotate`
/// from a non-mesh call site. Populated by mesh startup
/// (`mesh::pipeline::run_mesh`) once `IrohMeshManager::new()` succeeds; the
/// watcher reads it best-effort and silently skips the broadcast when the
/// mesh is not running (single-node deployment).
pub struct BroadcastHook {
    pub local_node_id: String,
    pub iroh: Arc<IrohMeshManager>,
}

static BROADCAST_HOOK: OnceLock<BroadcastHook> = OnceLock::new();

/// Register the broadcast hook. Idempotent — second call is ignored so a
/// mesh restart inside the same process (rare; only tests) keeps the first
/// handle. Returns `true` when the registration won.
pub fn register_broadcast_hook(local_node_id: String, iroh: Arc<IrohMeshManager>) -> bool {
    BROADCAST_HOOK
        .set(BroadcastHook {
            local_node_id,
            iroh,
        })
        .is_ok()
}

/// Read the broadcast hook. `None` on single-node deployments where the mesh
/// pipeline never ran.
pub fn broadcast_hook() -> Option<&'static BroadcastHook> {
    BROADCAST_HOOK.get()
}

/// Emit one audit row describing the broadcast outcome. Best-effort: a DB
/// failure here is logged at debug level and otherwise ignored — the
/// rotation itself has already succeeded on disk, and the broadcast either
/// landed or didn't independently of audit bookkeeping.
///
/// Action: `mesh.keys.broadcast_on_rotate`. Risk class B (key-material
/// adjacent). Result is one of `ok` / `denied` / `error`; the `details`
/// JSON carries the rotated scope name plus per-outcome counts.
pub fn emit_broadcast_audit(
    rotated_name: &str,
    outcome: Result<&BroadcastResult, &BroadcastError>,
) {
    let pool = match crate::db::global_pool() {
        Some(p) => p,
        None => return,
    };
    // (Hold onto `pool` past the match scope so its temporary write guard
    // below can borrow it for the duration of the INSERT.)

    let (result, error_message, details) = match outcome {
        Ok(br) => {
            let details = serde_json::json!({
                "name": rotated_name,
                "peer_count": br.peer_count,
                "ok_count": br.ok_count,
                "err_count": br.err_count,
            })
            .to_string();
            ("ok", String::new(), details)
        }
        Err(BroadcastError::RateLimited { retry_after_secs }) => {
            let details = serde_json::json!({
                "name": rotated_name,
                "reason": "rate_limited",
                "retry_after_secs": *retry_after_secs,
            })
            .to_string();
            (
                "denied",
                format!("rate_limited retry_after={retry_after_secs}s"),
                details,
            )
        }
        Err(BroadcastError::NoTrustedPeers) => {
            let details = serde_json::json!({
                "name": rotated_name,
                "reason": "no_trusted_peers",
            })
            .to_string();
            ("denied", "no_trusted_peers".to_string(), details)
        }
        Err(BroadcastError::BuildPayloadFailed) => {
            let details = serde_json::json!({
                "name": rotated_name,
                "reason": "build_payload_failed",
            })
            .to_string();
            ("error", "build_payload_failed".to_string(), details)
        }
        Err(BroadcastError::Internal(msg)) => {
            let details = serde_json::json!({
                "name": rotated_name,
                "reason": "internal",
                "message": msg,
            })
            .to_string();
            ("error", format!("internal: {msg}"), details)
        }
    };

    let guard = pool.write();
    if let Ok(conn) = guard {
        let severity = if result == "ok" { "info" } else { "warn" };
        let _ = conn.execute(
            "INSERT INTO audit_log \
                (timestamp, action, resource_type, resource_id, result, error_message, \
                 severity, risk_class, details) \
             VALUES (datetime('now'), 'mesh.keys.broadcast_on_rotate', \
                     'hmac_key', ?1, ?2, ?3, ?4, 'B', ?5)",
            rusqlite::params![rotated_name, result, error_message, severity, details],
        );
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
    fn broadcast_on_rotate_rate_limited_within_5s() {
        let pool = MeshKeyPool::new();
        // First reservation must win.
        assert!(pool.reserve_rate_limit_slot("pickup_token").is_ok());
        // A second reservation for the same name inside the 5 s window is
        // rejected with RateLimited carrying a non-zero retry hint.
        match pool.reserve_rate_limit_slot("pickup_token") {
            Err(BroadcastError::RateLimited { retry_after_secs }) => {
                assert!(retry_after_secs > 0 && retry_after_secs <= 6);
            }
            other => panic!("expected RateLimited, got {:?}", other),
        }
    }

    #[test]
    fn broadcast_on_rotate_rate_limit_is_per_name() {
        let pool = MeshKeyPool::new();
        // pickup_token reservation does not block frame_url / recording_url.
        assert!(pool.reserve_rate_limit_slot("pickup_token").is_ok());
        assert!(pool.reserve_rate_limit_slot("frame_url").is_ok());
        assert!(pool.reserve_rate_limit_slot("recording_url").is_ok());
        // Each name is now locked for itself.
        assert!(matches!(
            pool.reserve_rate_limit_slot("pickup_token"),
            Err(BroadcastError::RateLimited { .. })
        ));
        assert!(matches!(
            pool.reserve_rate_limit_slot("frame_url"),
            Err(BroadcastError::RateLimited { .. })
        ));
    }

    #[test]
    fn broadcast_on_rotate_allowed_after_reset() {
        let pool = MeshKeyPool::new();
        assert!(pool.reserve_rate_limit_slot("frame_url").is_ok());
        assert!(matches!(
            pool.reserve_rate_limit_slot("frame_url"),
            Err(BroadcastError::RateLimited { .. })
        ));
        // Equivalent to "5 s elapsed" — the production path uses a real clock.
        pool.reset_rate_limit_for_tests();
        assert!(pool.reserve_rate_limit_slot("frame_url").is_ok());
    }

    #[test]
    fn broadcast_error_display_is_human_readable() {
        let e = BroadcastError::RateLimited {
            retry_after_secs: 3,
        };
        assert!(e.to_string().contains("rate limited"));
        let e = BroadcastError::NoTrustedPeers;
        assert!(e.to_string().contains("no trusted peers"));
        let e = BroadcastError::BuildPayloadFailed;
        assert!(e.to_string().contains("encode"));
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

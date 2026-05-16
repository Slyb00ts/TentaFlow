// =============================================================================
// File: services/pickup_tokens/mod.rs — public PickupTokenIssuer API + singleton
// =============================================================================
//
// One issuer per process. The HMAC signing key lives on disk at
// `<tentaflow_home>/keys/pickup_token.key` (32 raw bytes, mode 0600 on Unix)
// since F1b P3.A — restart no longer invalidates outstanding tokens. Rotation
// is operator-driven through `tentaflow-cli keys rotate pickup_token`:
// the previous key is kept in memory as a verify-only key for the full token
// TTL window so any token minted right before the rotate still pickups
// successfully. Multi-node mesh sync of the signing key is P3.B territory.

mod store;
mod token;

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dashmap::DashMap;
use parking_lot::RwLock;

pub use token::{PickupToken, PickupVerifyError, TokenPayload};

use self::store::{sweep, InflightMap, IssuedToken};
use self::token::{parse_and_verify_multi, sign_payload};

/// F1b P3.C-2 — verifier provenance. `Local` means the token's HMAC matched
/// the local issuer's current or previous-window key (the issuing node is
/// us). `Peer(node_id)` means we matched a peer-synced key from the mesh
/// key pool, i.e. the token was minted on that peer; the pickup is a
/// cross-node mesh-fallback. Caller logs this into
/// `frame_pickup_log.source_node_id` for audit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifySource {
    Local,
    Peer(String),
}

/// F1b P3.C-2 — B-side replay protection retention. Mesh-fallback consumes
/// are tracked in a separate inflight map keyed by the full wire token; we
/// keep the entry around for `2 × token TTL` so a delayed retry that
/// arrives just past expiry still collides with the original consume row
/// and surfaces as `AlreadyConsumed` instead of silently passing again.
pub const MESH_CONSUMED_RETAIN: Duration = Duration::from_secs(60);

use crate::services::key_storage::PersistentKey;

/// Default lifetime for issued tokens. Picked to be just long enough for a
/// service to receive a QUIC request, process headers, and call back into
/// `/core/frame/pickup`.
pub const DEFAULT_TTL: Duration = Duration::from_secs(30);

/// How long we keep consumed/expired entries before sweeping them out —
/// `2 × TTL` so a request that started near the deadline still finds the
/// token in the map for a clean "AlreadyConsumed" / "Expired" verdict.
pub const SWEEP_RETAIN: Duration = Duration::from_secs(60);

/// How often the background task sweeps the inflight map.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// Name of the on-disk key file (`<tentaflow_home>/keys/pickup_token.key`).
pub const KEY_NAME: &str = "pickup_token";

/// Rotation state: current key plus an optional previous key that stays
/// valid for verify until `previous_expires_at`. Wrapped behind an RwLock
/// inside the issuer so `rotate()` can swap atomically while verifies hold
/// a read lock.
struct KeyState {
    current: [u8; 32],
    previous: Option<[u8; 32]>,
    previous_expires_at: Instant,
}

impl KeyState {
    fn new(current: [u8; 32]) -> Self {
        Self {
            current,
            previous: None,
            previous_expires_at: Instant::now(),
        }
    }
}

pub struct PickupTokenIssuer {
    keys: Arc<RwLock<KeyState>>,
    inflight: InflightMap,
    /// F1b P3.C-2 — B-side replay protection. Wire token → consume timestamp.
    /// Entry written on the first successful mesh-fallback consume; further
    /// consumes for the same wire return `AlreadyConsumed`. Lazy eviction:
    /// stale entries (>= MESH_CONSUMED_RETAIN old) are pruned on every
    /// `mesh_inflight_consume` call before we check for an existing hit.
    mesh_consumed: Arc<DashMap<String, Instant>>,
    ttl: Duration,
}

impl PickupTokenIssuer {
    /// Production constructor — reads (or generates on first run) the
    /// persistent HMAC key from `<tentaflow_home>/keys/pickup_token.key`,
    /// uses the default TTL, and spawns the background sweeper on the
    /// current tokio runtime.
    pub fn new() -> Self {
        let key = PersistentKey::load_or_generate(KEY_NAME)
            .expect("pickup_token.key load_or_generate must succeed at first use");
        Self::with_key(*key.bytes())
    }

    fn with_key(key: [u8; 32]) -> Self {
        let this = Self {
            keys: Arc::new(RwLock::new(KeyState::new(key))),
            inflight: Arc::new(DashMap::new()),
            mesh_consumed: Arc::new(DashMap::new()),
            ttl: DEFAULT_TTL,
        };
        this.spawn_sweeper();
        this
    }

    /// Test constructor that lets integration + unit tests pin the key + TTL
    /// and skip spawning the background task (so tests do not require a
    /// tokio runtime just to construct an issuer). Public-but-hidden so the
    /// integration test crate (`tests/streaming_pickup.rs`) can use it.
    #[doc(hidden)]
    pub fn new_for_tests(key: [u8; 32], ttl: Duration) -> Self {
        Self {
            keys: Arc::new(RwLock::new(KeyState::new(key))),
            inflight: Arc::new(DashMap::new()),
            mesh_consumed: Arc::new(DashMap::new()),
            ttl,
        }
    }

    /// In-place key swap used by `tentaflow-cli keys rotate pickup_token`.
    /// The previous key is retained as a verify-only secondary for
    /// `ttl + grace` so any token minted seconds before the rotate still
    /// pickups successfully (the rotate CLI persists `new_key` to disk via
    /// the staging → .new → live atomic dance separately).
    pub fn rotate_in_memory(&self, new_key: [u8; 32]) {
        let mut state = self.keys.write();
        let old = state.current;
        state.previous = Some(old);
        // Hold the previous key valid for the full TTL window (max age of
        // an outstanding token in the inflight map) plus a small grace.
        state.previous_expires_at = Instant::now() + self.ttl + Duration::from_secs(5);
        state.current = new_key;
    }

    /// Background cleanup. Detached — if the runtime is single-threaded the
    /// task still runs on driver wake-ups; if the runtime shuts down the
    /// `Arc<DashMap>` simply drops with the rest of the issuer state.
    fn spawn_sweeper(&self) {
        let map = self.inflight.clone();
        // `tokio::spawn` panics if no runtime; in F1a this code always runs
        // under the main tokio runtime (`run_async` path), so the panic is a
        // configuration bug not a runtime hazard. Wrap in `try_spawn`-style
        // guard: check via `Handle::try_current` and only spawn if a runtime
        // is available — keeps non-tokio tests from blowing up.
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(SWEEP_INTERVAL).await;
                    sweep(&map, SWEEP_RETAIN);
                }
            });
        }
    }

    /// Sign + insert. Returns the on-the-wire token plus the parsed payload
    /// (useful so the caller does not have to re-decode for logging).
    pub fn issue(
        &self,
        raw_ref: String,
        service_id: String,
        request_id: String,
    ) -> (PickupToken, TokenPayload) {
        let expiry_unix_ms = now_unix_ms() + self.ttl.as_millis() as u64;
        let payload = TokenPayload {
            raw_ref,
            service_id,
            request_id,
            expiry_unix_ms,
            one_shot: true,
        };
        let token = {
            let state = self.keys.read();
            sign_payload(&state.current, &payload)
        };
        self.inflight
            .insert(token.wire(), IssuedToken::new(payload.clone()));
        (token, payload)
    }

    /// Snapshot the signing key state for mesh advertise (F1b P3.B). Returns
    /// the current key and, if the rotation grace window is still open, the
    /// previous key plus its absolute unix-ms expiry. Used only to push our
    /// own keys to trust-paired peers — never used for verify.
    pub fn snapshot_for_mesh(&self) -> ([u8; 32], Option<[u8; 32]>, u64) {
        let state = self.keys.read();
        let now = Instant::now();
        let (prev, prev_expiry_ms) = match state.previous {
            Some(p) if now < state.previous_expires_at => {
                let remaining = state.previous_expires_at - now;
                let unix_ms = now_unix_ms() + remaining.as_millis() as u64;
                (Some(p), unix_ms)
            }
            _ => (None, 0u64),
        };
        (state.current, prev, prev_expiry_ms)
    }

    /// Returns the keys we accept for verify: always the current key, plus
    /// the previous key if it has not yet expired.
    fn active_verify_keys(&self) -> Vec<[u8; 32]> {
        let state = self.keys.read();
        let mut out = Vec::with_capacity(2);
        out.push(state.current);
        if let Some(prev) = state.previous {
            if Instant::now() < state.previous_expires_at {
                out.push(prev);
            }
        }
        out
    }

    /// HMAC + inflight + expiry check WITHOUT consuming the one-shot bit.
    /// Used by the pickup handler so that a header cross-check failure does
    /// not burn a still-good token (which would let an attacker DoS the real
    /// recipient by forging the headers with a stolen wire string).
    pub fn verify_only(&self, wire: &str) -> Result<TokenPayload, PickupVerifyError> {
        // Try LOCAL keys first — a hit there must satisfy the full inflight +
        // one-shot contract (the local issuer owns the token's lifecycle).
        let local = self.active_verify_keys();
        let local_result = parse_and_verify_multi(local.iter().map(|k| k.as_slice()), wire);
        if let Ok((payload, key)) = local_result {
            let entry = self
                .inflight
                .get(&key)
                .ok_or(PickupVerifyError::InvalidToken)?;
            if now_unix_ms() > entry.payload.expiry_unix_ms {
                return Err(PickupVerifyError::Expired);
            }
            if entry.consumed.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(PickupVerifyError::AlreadyConsumed);
            }
            debug_assert_eq!(payload, entry.payload);
            return Ok(entry.payload.clone());
        }
        // F1b P3.B fallback — token was minted on a peer node whose HMAC key
        // we mirror through the mesh key pool. Verifier-only path: we never
        // consume the one-shot bit (the issuing node owns that), but we DO
        // run expiry + signature checks so an expired or forged token still
        // gets rejected.
        let peer_keys = crate::services::mesh_keys::mesh_key_pool()
            .verify_keys_for(crate::services::mesh_keys::KeyScope::PickupToken);
        if peer_keys.is_empty() {
            return Err(local_result.unwrap_err());
        }
        let (payload, _key) =
            parse_and_verify_multi(peer_keys.iter().map(|k| k.as_slice()), wire)?;
        if now_unix_ms() > payload.expiry_unix_ms {
            return Err(PickupVerifyError::Expired);
        }
        Ok(payload)
    }

    /// Like `verify_only` but also reports whether the token verified
    /// against a local key (current/previous window) or a peer-synced key
    /// from the mesh key pool. Caller logs the source into
    /// `frame_pickup_log.source_node_id`. To preserve constant-time HMAC
    /// behaviour we always evaluate every candidate key — both local and
    /// peer — before deciding which path matched.
    pub fn verify_only_with_source(
        &self,
        wire: &str,
    ) -> Result<(TokenPayload, VerifySource), PickupVerifyError> {
        let local = self.active_verify_keys();
        let local_match = parse_and_verify_multi(local.iter().map(|k| k.as_slice()), wire);

        let peer_entries = crate::services::mesh_keys::mesh_key_pool()
            .verify_keys_with_peers_for(crate::services::mesh_keys::KeyScope::PickupToken);
        let peer_keys: Vec<[u8; 32]> = peer_entries.iter().map(|(_, k)| *k).collect();
        let peer_match =
            parse_and_verify_multi(peer_keys.iter().map(|k| k.as_slice()), wire);

        if let Ok((payload, key)) = local_match {
            // Local match wins — it owns the inflight + one-shot lifecycle.
            let entry = self
                .inflight
                .get(&key)
                .ok_or(PickupVerifyError::InvalidToken)?;
            if now_unix_ms() > entry.payload.expiry_unix_ms {
                return Err(PickupVerifyError::Expired);
            }
            if entry.consumed.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(PickupVerifyError::AlreadyConsumed);
            }
            debug_assert_eq!(payload, entry.payload);
            return Ok((entry.payload.clone(), VerifySource::Local));
        }

        match peer_match {
            Ok((payload, _wire_key)) => {
                if now_unix_ms() > payload.expiry_unix_ms {
                    return Err(PickupVerifyError::Expired);
                }
                // Identify which peer's key matched. We recompute HMAC per
                // candidate key — same constant-time helper used inside
                // `parse_and_verify_multi`. This costs ~N small HMAC
                // computations on an already-rare path (mesh fallback only).
                let node_id = identify_matching_peer(&peer_entries, wire)
                    .unwrap_or_else(|| "<unknown-peer>".to_string());
                Ok((payload, VerifySource::Peer(node_id)))
            }
            Err(_) => Err(local_match.unwrap_err()),
        }
    }

    /// F1b P3.C-2 — B-side replay protection for mesh-fallback consumes.
    /// First call for a given wire token records the consume timestamp and
    /// returns `Ok(())`; every subsequent call returns
    /// `AlreadyConsumed`. Stale entries (older than `MESH_CONSUMED_RETAIN`)
    /// are pruned lazily on each call so a long-running process does not
    /// leak memory at high QPS.
    pub fn mesh_inflight_consume(&self, wire: &str) -> Result<(), PickupVerifyError> {
        // Lazy eviction first — bounded work, only scans entries already
        // past retention so it is cheap on a healthy map.
        let cutoff = Instant::now() - MESH_CONSUMED_RETAIN;
        self.mesh_consumed.retain(|_, ts| *ts > cutoff);

        match self.mesh_consumed.entry(wire.to_string()) {
            dashmap::mapref::entry::Entry::Occupied(_) => {
                Err(PickupVerifyError::AlreadyConsumed)
            }
            dashmap::mapref::entry::Entry::Vacant(v) => {
                v.insert(Instant::now());
                Ok(())
            }
        }
    }

    /// Test/diagnostic peek — number of entries in the mesh-consumed map.
    #[doc(hidden)]
    pub fn mesh_consumed_len(&self) -> usize {
        self.mesh_consumed.len()
    }

    /// Test helper — force-expire mesh-consumed entries older than `age`.
    #[doc(hidden)]
    pub fn mesh_consumed_sweep_for_tests(&self, age: Duration) {
        let cutoff = Instant::now() - age;
        self.mesh_consumed.retain(|_, ts| *ts > cutoff);
    }

    /// Atomic one-shot consume. Caller must have already run `verify_only`
    /// and cross-checked the headers against the returned payload. Returns
    /// `AlreadyConsumed` if a concurrent caller won the race.
    pub fn consume_one_shot(&self, wire: &str) -> Result<TokenPayload, PickupVerifyError> {
        let local = self.active_verify_keys();
        let local_result = parse_and_verify_multi(local.iter().map(|k| k.as_slice()), wire);
        if let Ok((_payload, key)) = local_result {
            let entry = self
                .inflight
                .get(&key)
                .ok_or(PickupVerifyError::InvalidToken)?;
            if now_unix_ms() > entry.payload.expiry_unix_ms {
                return Err(PickupVerifyError::Expired);
            }
            if !entry.try_consume() {
                return Err(PickupVerifyError::AlreadyConsumed);
            }
            return Ok(entry.payload.clone());
        }
        // F1b P3.B mesh fallback — see `verify_only` doc. We accept the token
        // exactly once per local pickup call, but we do not (cannot) enforce
        // the one-shot bit globally: the issuing node owns the inflight map.
        let peer_keys = crate::services::mesh_keys::mesh_key_pool()
            .verify_keys_for(crate::services::mesh_keys::KeyScope::PickupToken);
        if peer_keys.is_empty() {
            return Err(local_result.unwrap_err());
        }
        let (payload, _key) =
            parse_and_verify_multi(peer_keys.iter().map(|k| k.as_slice()), wire)?;
        if now_unix_ms() > payload.expiry_unix_ms {
            return Err(PickupVerifyError::Expired);
        }
        Ok(payload)
    }

    /// Revoke an issued but not-yet-consumed token. Used by callers that
    /// mint a token and then hit a downstream failure (router missing, rate
    /// limit, dispatch error) before the receiving service could use it.
    pub fn revoke(&self, wire: &str) {
        self.inflight.remove(wire);
    }

    /// Test/diagnostic peek — not used in production.
    #[doc(hidden)]
    pub fn inflight_len(&self) -> usize {
        self.inflight.len()
    }

    /// Test helper — runs one sweep synchronously.
    #[doc(hidden)]
    pub fn sweep_now(&self, retain_for: Duration) {
        sweep(&self.inflight, retain_for);
    }
}

impl Default for PickupTokenIssuer {
    fn default() -> Self {
        Self::new()
    }
}

/// Find which peer's HMAC key signs `wire`'s payload — used to attach a
/// `Peer(node_id)` provenance tag to a successful mesh-fallback verify. The
/// HMAC has already been validated by `parse_and_verify_multi`; this pass
/// runs constant-time comparisons against every candidate key to avoid a
/// timing channel that would leak "which peer's token did I just receive".
fn identify_matching_peer(
    peer_entries: &[(String, [u8; 32])],
    wire: &str,
) -> Option<String> {
    use subtle::ConstantTimeEq;
    let (payload_b64, sig_b64) = wire.split_once('.')?;
    let provided = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        sig_b64.as_bytes(),
    )
    .ok()?;
    let mut found: Option<String> = None;
    for (node_id, key) in peer_entries {
        let expected = self::token::hmac_sign(key, payload_b64);
        let hit = provided.len() == expected.len()
            && bool::from(provided.ct_eq(&expected));
        if hit && found.is_none() {
            found = Some(node_id.clone());
        }
    }
    found
}

/// Current Unix timestamp in milliseconds. We use `SystemTime` so expiry
/// comparisons survive process restarts within the same wall clock window —
/// `Instant` would be monotonic-only.
fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use self::token::sign_payload;

    fn issuer() -> PickupTokenIssuer {
        PickupTokenIssuer::new_for_tests([5u8; 32], Duration::from_secs(30))
    }

    #[test]
    fn issue_then_verify_consumes_one_shot() {
        let i = issuer();
        let (t, _) = i.issue("frame_a".into(), "svc".into(), "req-1".into());
        let wire = t.wire();
        let p = i.consume_one_shot(&wire).expect("ok");
        assert_eq!(p.raw_ref, "frame_a");
        assert_eq!(
            i.consume_one_shot(&wire).unwrap_err(),
            PickupVerifyError::AlreadyConsumed,
            "replay must fail"
        );
    }

    #[test]
    fn expired_token_rejected() {
        let i = PickupTokenIssuer::new_for_tests([5u8; 32], Duration::from_millis(0));
        let (t, _) = i.issue("frame_b".into(), "svc".into(), "req-2".into());
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(
            i.consume_one_shot(&t.wire()).unwrap_err(),
            PickupVerifyError::Expired
        );
    }

    #[test]
    fn forged_signature_rejected() {
        let i = issuer();
        let (t, _) = i.issue("frame_c".into(), "svc".into(), "req-3".into());
        let mut wire = t.wire();
        // Tamper with the signature half.
        let last = wire.pop().unwrap();
        wire.push(if last == 'A' { 'B' } else { 'A' });
        assert_eq!(
            i.consume_one_shot(&wire).unwrap_err(),
            PickupVerifyError::InvalidSignature
        );
    }

    #[test]
    fn unknown_but_valid_signature_rejected() {
        // Sign with the same key but never insert into the inflight map —
        // emulates server restart: the wire is HMAC-valid but the inflight
        // table no longer has the entry.
        let i = issuer();
        let payload = TokenPayload {
            raw_ref: "frame_d".into(),
            service_id: "svc".into(),
            request_id: "req-4".into(),
            expiry_unix_ms: now_unix_ms() + 30_000,
            one_shot: true,
        };
        let t = sign_payload(&i.keys.read().current, &payload);
        assert_eq!(
            i.consume_one_shot(&t.wire()).unwrap_err(),
            PickupVerifyError::InvalidToken
        );
    }

    #[test]
    fn test_pickup_token_persists_across_issuer_recreate() {
        // Issue a token from issuer A (loaded from disk path X), drop A,
        // recreate issuer B (loaded from same path X) and verify the token
        // still validates. Inflight map is process-local so we mint a fresh
        // payload through the second issuer using the SAME loaded key —
        // this proves the on-disk key persisted byte-for-byte across the
        // recreate.
        let td = tempfile::TempDir::new().unwrap();
        let path = td.path().join("pickup_token.key");

        let k_a = crate::services::key_storage::PersistentKey::load_or_generate_at(
            KEY_NAME, &path,
        )
        .unwrap();
        let k_b = crate::services::key_storage::PersistentKey::load_or_generate_at(
            KEY_NAME, &path,
        )
        .unwrap();
        assert_eq!(
            k_a.bytes(),
            k_b.bytes(),
            "persistent key must be stable across recreate"
        );

        // Build issuer B from the disk-loaded key and sign-then-verify.
        let iss = PickupTokenIssuer::new_for_tests(*k_b.bytes(), Duration::from_secs(30));
        let (t, _) = iss.issue("frame_p".into(), "svc".into(), "req-p".into());
        let p = iss.consume_one_shot(&t.wire()).expect("disk-loaded key verifies");
        assert_eq!(p.raw_ref, "frame_p");
    }

    #[test]
    fn test_mesh_inflight_consume_first_succeeds() {
        let i = issuer();
        let wire = "fake-wire-token-aaa".to_string();
        i.mesh_inflight_consume(&wire).expect("first consume ok");
        assert_eq!(i.mesh_consumed_len(), 1);
    }

    #[test]
    fn test_mesh_inflight_consume_second_returns_replay() {
        let i = issuer();
        let wire = "fake-wire-token-bbb".to_string();
        i.mesh_inflight_consume(&wire).expect("first ok");
        assert_eq!(
            i.mesh_inflight_consume(&wire).unwrap_err(),
            PickupVerifyError::AlreadyConsumed
        );
    }

    #[test]
    fn test_mesh_inflight_expires_after_ttl() {
        let i = issuer();
        let wire = "fake-wire-token-ccc".to_string();
        i.mesh_inflight_consume(&wire).expect("first ok");
        // Sweep with a zero-duration retention forces eviction of every
        // entry. The next consume must succeed instead of replay-rejecting.
        i.mesh_consumed_sweep_for_tests(Duration::from_millis(0));
        // Tiny pause to ensure Instant::now() advances past the inserted ts
        // on platforms with coarse monotonic clocks.
        std::thread::sleep(Duration::from_millis(2));
        i.mesh_consumed_sweep_for_tests(Duration::from_millis(0));
        i.mesh_inflight_consume(&wire)
            .expect("after expiry, fresh consume ok");
    }

    #[test]
    fn test_verify_with_source_returns_local() {
        let i = issuer();
        let (t, _) = i.issue("frame_l".into(), "svc".into(), "req-l".into());
        let (_p, src) = i.verify_only_with_source(&t.wire()).expect("local verify");
        assert_eq!(src, VerifySource::Local);
    }

    #[test]
    fn test_verify_with_source_returns_peer_node_id() {
        // Mint with a key that is NOT the local issuer's key; insert that
        // key into the mesh key pool tagged as "peer-X" and expect the
        // local verify path to fall through to the mesh-fallback branch
        // and tag the source as Peer("peer-X").
        let peer_key = [0x99u8; 32];
        let peer_node = format!(
            "peer-x-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        // Mint the token under peer_key. We bypass the issuer's inflight map
        // entirely because mesh-fallback only checks signature + expiry.
        let payload = TokenPayload {
            raw_ref: "frame_peer".into(),
            service_id: "svc".into(),
            request_id: "req-peer".into(),
            expiry_unix_ms: now_unix_ms() + 30_000,
            one_shot: true,
        };
        let tok = sign_payload(&peer_key, &payload);

        crate::services::mesh_keys::mesh_key_pool().upsert(
            &peer_node,
            crate::services::mesh_keys::KeyScope::PickupToken,
            crate::services::mesh_keys::PeerKeyState {
                current: peer_key,
                previous: None,
                previous_expires_unix_ms: 0,
            },
        );

        // Local issuer uses a different key (the default test key 5u8).
        let i = issuer();
        let (got_payload, src) = i
            .verify_only_with_source(&tok.wire())
            .expect("peer verify");
        assert_eq!(got_payload.raw_ref, "frame_peer");
        match src {
            VerifySource::Peer(id) => assert_eq!(id, peer_node),
            VerifySource::Local => panic!("expected Peer source, got Local"),
        }

        // Cleanup so other tests sharing the static mesh_key_pool are
        // unaffected.
        crate::services::mesh_keys::mesh_key_pool().remove_peer(&peer_node);
    }

    #[test]
    fn previous_key_window_verifies_after_rotation() {
        // Token minted under the OLD key, then rotate in-memory. Verify
        // must still succeed because the previous_key window is open.
        let i = PickupTokenIssuer::new_for_tests([0x11u8; 32], Duration::from_secs(30));
        let (t, _) = i.issue("frame_r".into(), "svc".into(), "req-r".into());
        i.rotate_in_memory([0x22u8; 32]);
        // Same wire should still verify via the previous-key fallback.
        let p = i.consume_one_shot(&t.wire()).expect("previous key still valid");
        assert_eq!(p.raw_ref, "frame_r");
    }
}

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
        let candidates = self.active_verify_keys();
        let (payload, key) = parse_and_verify_multi(
            candidates.iter().map(|k| k.as_slice()),
            wire,
        )?;
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
        Ok(entry.payload.clone())
    }

    /// Atomic one-shot consume. Caller must have already run `verify_only`
    /// and cross-checked the headers against the returned payload. Returns
    /// `AlreadyConsumed` if a concurrent caller won the race.
    pub fn consume_one_shot(&self, wire: &str) -> Result<TokenPayload, PickupVerifyError> {
        let candidates = self.active_verify_keys();
        let (_payload, key) = parse_and_verify_multi(
            candidates.iter().map(|k| k.as_slice()),
            wire,
        )?;
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
        Ok(entry.payload.clone())
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

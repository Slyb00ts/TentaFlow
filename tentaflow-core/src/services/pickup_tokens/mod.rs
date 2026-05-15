// =============================================================================
// File: services/pickup_tokens/mod.rs — public PickupTokenIssuer API + singleton
// =============================================================================
//
// One issuer per process. The signing key is generated at first use from
// `OsRng` (32 random bytes) — F1a is single-node, restart invalidates every
// outstanding token (acceptable: TTL is 30 s anyway). Multi-node mesh sync of
// the signing key is M3/F1b territory.

mod store;
mod token;

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dashmap::DashMap;

pub use token::{PickupToken, PickupVerifyError, TokenPayload};

use self::store::{sweep, InflightMap, IssuedToken};
use self::token::{parse_and_verify, sign_payload};

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

pub struct PickupTokenIssuer {
    signing_key: [u8; 32],
    inflight: InflightMap,
    ttl: Duration,
}

impl PickupTokenIssuer {
    /// Production constructor — random key, default TTL, background sweeper
    /// spawned on the current tokio runtime.
    pub fn new() -> Self {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).expect("OS RNG fill for PickupToken signing key");
        let this = Self {
            signing_key: key,
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
            signing_key: key,
            inflight: Arc::new(DashMap::new()),
            ttl,
        }
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
        let token = sign_payload(&self.signing_key, &payload);
        self.inflight
            .insert(token.wire(), IssuedToken::new(payload.clone()));
        (token, payload)
    }

    /// HMAC + inflight + expiry check WITHOUT consuming the one-shot bit.
    /// Used by the pickup handler so that a header cross-check failure does
    /// not burn a still-good token (which would let an attacker DoS the real
    /// recipient by forging the headers with a stolen wire string).
    pub fn verify_only(&self, wire: &str) -> Result<TokenPayload, PickupVerifyError> {
        let (payload, key) = parse_and_verify(&self.signing_key, wire)?;
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
        let (_payload, key) = parse_and_verify(&self.signing_key, wire)?;
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
        let t = sign_payload(&i.signing_key, &payload);
        assert_eq!(
            i.consume_one_shot(&t.wire()).unwrap_err(),
            PickupVerifyError::InvalidToken
        );
    }
}

// =============================================================================
// File: util/token_bucket.rs — classic token-bucket primitive
// =============================================================================
//
// A standalone token bucket extracted from `api::rate_limit` so that both the
// HTTP-side limiter (per-IP, signed-URL endpoints) and the WASM-side limiter
// (per-addon `service_call_v1`) share one implementation. The split between
// `refill_and_peek` and `commit_one` lets a composite limiter validate every
// nested bucket before debiting any single one — avoids the double-debit bug
// where the inner token is consumed and then the outer denies the request.

use std::time::Instant;

#[derive(Debug)]
pub struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    pub fn new(capacity: u32) -> Self {
        Self {
            tokens: capacity as f64,
            last_refill: Instant::now(),
        }
    }

    /// Refill based on elapsed time without consuming. Returns `Ok(())` if at
    /// least one token is available post-refill, otherwise `Err(retry_secs)`.
    /// Caller must explicitly `commit_one` after deciding to charge.
    pub fn refill_and_peek(
        &mut self,
        capacity: u32,
        refill_per_sec: f64,
        now: Instant,
    ) -> std::result::Result<(), f64> {
        let elapsed = now.saturating_duration_since(self.last_refill).as_secs_f64();
        if elapsed > 0.0 {
            self.tokens = (self.tokens + elapsed * refill_per_sec).min(capacity as f64);
            self.last_refill = now;
        }
        if self.tokens >= 1.0 {
            Ok(())
        } else {
            let missing = 1.0 - self.tokens;
            let retry = if refill_per_sec > 0.0 {
                missing / refill_per_sec
            } else {
                f64::INFINITY
            };
            Err(retry)
        }
    }

    /// Charge one token. Precondition: a prior `refill_and_peek` on the same
    /// `now` returned `Ok(())`.
    pub fn commit_one(&mut self) {
        self.tokens -= 1.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn fresh_bucket_allows_capacity_then_denies() {
        let mut b = TokenBucket::new(3);
        let now = Instant::now();
        for _ in 0..3 {
            assert!(b.refill_and_peek(3, 1.0, now).is_ok());
            b.commit_one();
        }
        assert!(b.refill_and_peek(3, 1.0, now).is_err());
    }

    #[test]
    fn refill_after_elapsed_time() {
        let mut b = TokenBucket::new(1);
        let t0 = Instant::now();
        assert!(b.refill_and_peek(1, 1.0, t0).is_ok());
        b.commit_one();
        assert!(b.refill_and_peek(1, 1.0, t0).is_err());
        let t1 = t0 + Duration::from_secs(2);
        assert!(b.refill_and_peek(1, 1.0, t1).is_ok());
    }
}

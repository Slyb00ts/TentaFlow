// =============================================================================
// Plik: auth/rate_limit.rs
// Opis: In-memory rate limiter dla logowania. Per-username 10 prob/min.
//       Mapa ograniczona do MAX_TRACKED_KEYS — ewikcja zamiast czyszczenia.
// =============================================================================

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Instant;

const WINDOW_SECS: u64 = 60;
const MAX_TRACKED_KEYS: usize = 10_000;

pub struct LoginRateLimiter {
    attempts: Mutex<HashMap<String, Vec<Instant>>>,
}

impl LoginRateLimiter {
    pub fn new() -> Self {
        Self {
            attempts: Mutex::new(HashMap::new()),
        }
    }

    /// Sprawdza i rejestruje probe. Zwraca true jesli dozwolona, false gdy zablokowana.
    /// Bucket = `key`, max `max_attempts` w 60s window.
    pub fn check_and_record(&self, key: &str, max_attempts: usize) -> bool {
        let mut map = self.attempts.lock();
        let now = Instant::now();

        if map.len() >= MAX_TRACKED_KEYS && !map.contains_key(key) {
            Self::evict(&mut map, now);
        }

        let attempts = map.entry(key.to_string()).or_default();
        attempts.retain(|t| now.duration_since(*t).as_secs() < WINDOW_SECS);

        if attempts.is_empty() {
            map.remove(key);
            map.entry(key.to_string()).or_default().push(now);
            return true;
        }

        if attempts.len() >= max_attempts {
            return false;
        }

        attempts.push(now);
        true
    }

    /// Bounds memory under a unique-key flood without ever wiping live
    /// counters wholesale: a full reset would hand a brute-forcer a fresh
    /// budget against every account. Expired buckets go first; if the map is
    /// still full, the buckets with the fewest attempts are dropped, so a
    /// victim's saturated bucket outlives the single-attempt flood entries.
    fn evict(map: &mut HashMap<String, Vec<Instant>>, now: Instant) {
        map.retain(|_, attempts| {
            attempts.retain(|t| now.duration_since(*t).as_secs() < WINDOW_SECS);
            !attempts.is_empty()
        });
        if map.len() < MAX_TRACKED_KEYS {
            return;
        }
        let mut by_weight: Vec<(usize, String)> =
            map.iter().map(|(k, v)| (v.len(), k.clone())).collect();
        by_weight.sort_unstable();
        for (_, key) in by_weight.into_iter().take(MAX_TRACKED_KEYS / 10) {
            map.remove(&key);
        }
    }
}

impl Default for LoginRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Globalny rate limiter dla logowania (binary handler `auth_login`).
pub static LOGIN_RATE_LIMITER: LazyLock<LoginRateLimiter> = LazyLock::new(LoginRateLimiter::new);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_flood_does_not_reset_a_blocked_bucket() {
        let limiter = LoginRateLimiter::new();
        for _ in 0..10 {
            assert!(limiter.check_and_record("victim", 10));
        }
        assert!(!limiter.check_and_record("victim", 10));

        for i in 0..(MAX_TRACKED_KEYS * 2) {
            limiter.check_and_record(&format!("flood-{i}"), 10);
        }

        assert!(!limiter.check_and_record("victim", 10));
        assert!(limiter.attempts.lock().len() <= MAX_TRACKED_KEYS);
    }
}

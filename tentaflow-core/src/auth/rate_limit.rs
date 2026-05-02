// =============================================================================
// Plik: auth/rate_limit.rs
// Opis: In-memory rate limiter dla logowania. Per-username 10 prob/min.
//       Awaryjne czyszczenie mapy gdy > 10000 kluczy (ochrona przed OOM).
// =============================================================================

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Instant;

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

        // Awaryjne czyszczenie mapy — lepsze niz OOM przy ataku spamem unikalnych kluczy.
        if map.len() > 10000 {
            map.clear();
        }

        let attempts = map.entry(key.to_string()).or_default();
        attempts.retain(|t| now.duration_since(*t).as_secs() < 60);

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
}

impl Default for LoginRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Globalny rate limiter dla logowania (binary handler `auth_login`).
pub static LOGIN_RATE_LIMITER: LazyLock<LoginRateLimiter> = LazyLock::new(LoginRateLimiter::new);

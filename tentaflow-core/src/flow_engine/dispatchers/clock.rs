// =============================================================================
// Plik: flow_engine/dispatchers/clock.rs
// Opis: Clock trait — testowalna abstrakcja czasu. Adapter który potrzebuje
//       timestamp używa ctx.clock zamiast chrono::Utc::now bezpośrednio.
// =============================================================================

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub trait Clock: Send + Sync {
    /// Unix epoch milliseconds.
    fn now_ms(&self) -> u64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

impl SystemClock {
    pub fn arc() -> Arc<dyn Clock> {
        Arc::new(SystemClock)
    }
}

#[cfg(test)]
pub struct FixedClock(pub u64);

#[cfg(test)]
impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_returns_nonzero() {
        let c = SystemClock;
        assert!(c.now_ms() > 1_700_000_000_000);
    }

    #[test]
    fn fixed_clock_returns_value() {
        let c = FixedClock(42);
        assert_eq!(c.now_ms(), 42);
    }
}

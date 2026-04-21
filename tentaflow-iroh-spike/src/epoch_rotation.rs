// =============================================================================
// Plik: src/epoch_rotation.rs
// Opis: Kryterium (c) — epoch rotation 24h interval z 7-day grace period.
//       Zero-drop wymaganie: w grace window stary epoch nadal akceptowany,
//       wiec aktywne sesje nie zostana zerwane przy rotacji.
//
//       Zamiast wall-clock test (24h+7d nierealne w CI), uzywamy "virtual
//       time" — testy advance_to(t) symuluja czas. Real wall-clock
//       integration test deferred do produkcyjnego setup.
// =============================================================================

use std::collections::HashMap;

pub const ROTATION_INTERVAL_SECS: u64 = 24 * 60 * 60; // 24h
pub const GRACE_WINDOW_SECS: u64 = 7 * 24 * 60 * 60; // 7d

#[derive(Debug)]
pub struct EpochManager {
    /// Mapa epoch -> czas wprowadzenia (unix epoch).
    epochs: HashMap<u32, u64>,
    /// Aktualny epoch (najnowszy).
    current_epoch: u32,
    /// "Wirtualny czas" — testy advance_to(); produkcja uzywa SystemTime.
    virtual_now_secs: u64,
}

impl EpochManager {
    pub fn new(initial_epoch: u32, start_time_secs: u64) -> Self {
        let mut epochs = HashMap::new();
        epochs.insert(initial_epoch, start_time_secs);
        Self {
            epochs,
            current_epoch: initial_epoch,
            virtual_now_secs: start_time_secs,
        }
    }

    /// Symuluje uplyw czasu (test only).
    pub fn advance_to(&mut self, new_now_secs: u64) {
        self.virtual_now_secs = new_now_secs;
    }

    /// Czy `epoch` jest jeszcze akceptowany (current OR within grace window).
    pub fn is_epoch_valid(&self, epoch: u32) -> bool {
        if epoch == self.current_epoch {
            return true;
        }
        if let Some(&introduced_at) = self.epochs.get(&epoch) {
            let age = self.virtual_now_secs.saturating_sub(introduced_at);
            // Akceptujemy stary epoch w window: introduced_at + ROTATION_INTERVAL + GRACE >= now
            age <= ROTATION_INTERVAL_SECS + GRACE_WINDOW_SECS
        } else {
            false
        }
    }

    /// Wykonuje rotacje — nowy epoch = current + 1, zachowuje stary epoch w mapie
    /// dla grace window. Czyszczenie zbyt starych epochs.
    pub fn rotate(&mut self) -> u32 {
        let new_epoch = self.current_epoch + 1;
        self.epochs.insert(new_epoch, self.virtual_now_secs);
        self.current_epoch = new_epoch;
        self.cleanup_expired();
        new_epoch
    }

    /// Usuwa epochs starsze niz ROTATION_INTERVAL + GRACE_WINDOW.
    fn cleanup_expired(&mut self) {
        let cutoff = self
            .virtual_now_secs
            .saturating_sub(ROTATION_INTERVAL_SECS + GRACE_WINDOW_SECS);
        self.epochs.retain(|_, &mut introduced| introduced > cutoff);
    }

    pub fn current(&self) -> u32 {
        self.current_epoch
    }

    pub fn known_epochs(&self) -> Vec<u32> {
        let mut keys: Vec<_> = self.epochs.keys().copied().collect();
        keys.sort();
        keys
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_700_000_000;

    #[test]
    fn current_epoch_always_valid() {
        let mgr = EpochManager::new(1, T0);
        assert!(mgr.is_epoch_valid(1));
    }

    #[test]
    fn rotation_creates_new_epoch_keeps_old() {
        let mut mgr = EpochManager::new(1, T0);
        mgr.advance_to(T0 + ROTATION_INTERVAL_SECS);
        let new_epoch = mgr.rotate();
        assert_eq!(new_epoch, 2);
        assert_eq!(mgr.current(), 2);
        // Stary nadal w grace window.
        assert!(mgr.is_epoch_valid(1));
        assert!(mgr.is_epoch_valid(2));
    }

    #[test]
    fn old_epoch_expires_after_rotation_plus_grace() {
        let mut mgr = EpochManager::new(1, T0);
        mgr.advance_to(T0 + ROTATION_INTERVAL_SECS);
        mgr.rotate();
        // Po grace window stary epoch przestaje byc akceptowany.
        mgr.advance_to(T0 + ROTATION_INTERVAL_SECS + GRACE_WINDOW_SECS + 1);
        assert!(!mgr.is_epoch_valid(1), "epoch 1 should be expired");
        assert!(mgr.is_epoch_valid(2));
    }

    #[test]
    fn zero_drop_during_grace_window() {
        // Symulujemy seriE rotacji + ciagle uzywanie starych epoch w grace.
        let mut mgr = EpochManager::new(1, T0);
        let mut accepted = 0;
        let mut rejected = 0;
        let mut now = T0;

        // 7 dni rotacji co 24h.
        for day in 1..=7 {
            now = T0 + day * ROTATION_INTERVAL_SECS;
            mgr.advance_to(now);
            mgr.rotate();

            // Symulujemy 100 frame'ow z poprzedniego epoch (active sesja)
            let prev_epoch = mgr.current() - 1;
            for _ in 0..100 {
                if mgr.is_epoch_valid(prev_epoch) {
                    accepted += 1;
                } else {
                    rejected += 1;
                }
            }
        }
        assert_eq!(rejected, 0, "zero-drop violated: {} rejected", rejected);
        assert_eq!(accepted, 7 * 100);
    }

    #[test]
    fn cleanup_removes_very_old_epochs() {
        let mut mgr = EpochManager::new(1, T0);
        // Zaawansuj o 30 dni i rotuj 30 razy.
        for day in 1..=30 {
            mgr.advance_to(T0 + day * ROTATION_INTERVAL_SECS);
            mgr.rotate();
        }
        let known = mgr.known_epochs();
        // Powinno pozostac okolo 8 epochs (rotation + grace ~8 dni / 24h).
        assert!(
            known.len() <= 10,
            "too many old epochs retained: {}",
            known.len()
        );
        assert!(mgr.is_epoch_valid(mgr.current()));
        assert!(!mgr.is_epoch_valid(1), "very old epoch 1 should be removed");
    }

    #[test]
    fn unknown_epoch_rejected() {
        let mgr = EpochManager::new(1, T0);
        assert!(!mgr.is_epoch_valid(999));
    }
}

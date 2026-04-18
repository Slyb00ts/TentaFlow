// =============================================================================
// Plik: src/replay.rs
// Opis: Kryterium (d) — sliding-window replay protection.
//       Implementacja jest niezalezna od iroh — chroni przed atakiem replay
//       gdzie attacker wysyla starszy frame ponownie. Window track ostatnich
//       N seen sequence numbers; nowy frame musi byc albo wyzszy od max,
//       albo w window i niewidziany.
// =============================================================================

use std::collections::VecDeque;

/// Default rozmiar sliding window. Powinien byc >= max-out-of-order delivery
/// aby nie blokowac legitimate frames.
pub const DEFAULT_WINDOW_SIZE: usize = 128;

#[derive(Debug)]
pub struct ReplayWindow {
    /// Najwyzszy seen sequence (rośnie monotonicznie).
    max_seen: u64,
    /// Sliding window ostatnich N sequences seen niedawno (deque dla O(1) push/pop).
    seen: VecDeque<u64>,
    window_size: usize,
}

impl ReplayWindow {
    pub fn new() -> Self {
        Self::with_window(DEFAULT_WINDOW_SIZE)
    }

    pub fn with_window(window_size: usize) -> Self {
        Self {
            max_seen: 0,
            seen: VecDeque::with_capacity(window_size),
            window_size,
        }
    }

    /// Sprawdza czy `seq` jest dozwolone (nowe lub w window i niewidziane).
    /// Jesli tak, dodaje do window i zwraca true.
    /// Jesli nie (replay lub poza oknem), zwraca false (frame odrzucony).
    pub fn check_and_record(&mut self, seq: u64) -> bool {
        if seq > self.max_seen {
            // Nowy najwyzszy — zaakceptuj.
            self.max_seen = seq;
            self.seen.push_back(seq);
            while self.seen.len() > self.window_size {
                self.seen.pop_front();
            }
            true
        } else if self.max_seen.saturating_sub(seq) > self.window_size as u64 {
            // Za stary — poza window. Reject jako potential replay.
            false
        } else if self.seen.contains(&seq) {
            // W window ale juz widziany — replay attack.
            false
        } else {
            // W window, niewidziany — out-of-order delivery, OK.
            self.seen.push_back(seq);
            while self.seen.len() > self.window_size {
                self.seen.pop_front();
            }
            true
        }
    }
}

impl Default for ReplayWindow {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_monotonic_sequence() {
        let mut w = ReplayWindow::new();
        for i in 1..=100 {
            assert!(w.check_and_record(i), "seq {} should accept", i);
        }
    }

    #[test]
    fn rejects_exact_replay() {
        let mut w = ReplayWindow::new();
        assert!(w.check_and_record(42));
        assert!(!w.check_and_record(42), "duplicate seq must be rejected");
    }

    #[test]
    fn accepts_out_of_order_within_window() {
        let mut w = ReplayWindow::with_window(8);
        // Wprowadz max=20
        assert!(w.check_and_record(20));
        // Out-of-order ale w window (20-15=5 <= 8)
        assert!(w.check_and_record(15));
        assert!(w.check_and_record(18));
        // Replay 15
        assert!(!w.check_and_record(15));
    }

    #[test]
    fn rejects_out_of_window_old_sequence() {
        let mut w = ReplayWindow::with_window(8);
        assert!(w.check_and_record(100));
        // 100 - 50 = 50 > window 8 → reject
        assert!(!w.check_and_record(50), "very old seq must be rejected");
    }
}

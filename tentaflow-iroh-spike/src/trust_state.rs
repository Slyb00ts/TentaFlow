// =============================================================================
// Plik: src/trust_state.rs
// Opis: Kryterium (e) — TrustRevoked + TrustedKeysSync events processing.
//       Trust state per node_id (32-byte Ed25519 pubkey). Eventy:
//         - TrustedKeysSync(keys[]): replace local trusted set z bulk
//         - TrustRevoked(node_id): usun klucz, dodaj do revoked blacklist
//       Real ALPN routing przez iroh::Endpoint to thin glue — core logic
//       (trust set + revocation) jest iroh-independent.
// =============================================================================

use std::collections::HashSet;

#[derive(Debug, Default)]
pub struct TrustState {
    trusted: HashSet<[u8; 32]>,
    /// Revoked keys (czarna lista — nawet jesli sync je doda, odrzucamy).
    revoked: HashSet<[u8; 32]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustEvent {
    /// Bulk sync: zamien local trusted set tymi keyami (z poszanowaniem revoked blacklist).
    Sync { keys: Vec<[u8; 32]> },
    /// Revoke pojedynczy klucz.
    Revoke { node_id: [u8; 32] },
}

impl TrustState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sprawdza czy klucz jest aktualnie zaufany.
    pub fn is_trusted(&self, node_id: &[u8; 32]) -> bool {
        !self.revoked.contains(node_id) && self.trusted.contains(node_id)
    }

    pub fn is_revoked(&self, node_id: &[u8; 32]) -> bool {
        self.revoked.contains(node_id)
    }

    /// Liczba zaufanych kluczy.
    pub fn trusted_count(&self) -> usize {
        self.trusted.len()
    }

    /// Aplikuje event. Zwraca true jesli stan sie zmienil.
    pub fn apply(&mut self, event: TrustEvent) -> bool {
        match event {
            TrustEvent::Sync { keys } => {
                let new_set: HashSet<_> = keys
                    .into_iter()
                    .filter(|k| !self.revoked.contains(k))
                    .collect();
                if new_set == self.trusted {
                    false
                } else {
                    self.trusted = new_set;
                    true
                }
            }
            TrustEvent::Revoke { node_id } => {
                let was_trusted = self.trusted.remove(&node_id);
                let was_added = self.revoked.insert(node_id);
                was_trusted || was_added
            }
        }
    }

    /// Manualne dodanie zaufanego klucza (np. po pomyslnym pairing).
    pub fn add_trusted(&mut self, node_id: [u8; 32]) -> bool {
        if self.revoked.contains(&node_id) {
            return false;
        }
        self.trusted.insert(node_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    #[test]
    fn empty_state_trusts_nothing() {
        let s = TrustState::new();
        assert!(!s.is_trusted(&k(1)));
        assert_eq!(s.trusted_count(), 0);
    }

    #[test]
    fn add_and_check_trusted() {
        let mut s = TrustState::new();
        assert!(s.add_trusted(k(1)));
        assert!(s.is_trusted(&k(1)));
        assert!(!s.is_trusted(&k(2)));
    }

    #[test]
    fn revoke_removes_trust_permanently() {
        let mut s = TrustState::new();
        s.add_trusted(k(1));
        assert!(s.is_trusted(&k(1)));
        s.apply(TrustEvent::Revoke { node_id: k(1) });
        assert!(!s.is_trusted(&k(1)));
        assert!(s.is_revoked(&k(1)));
        // Re-add po revoke = no-op (revoked blacklist sticky).
        assert!(!s.add_trusted(k(1)));
        assert!(!s.is_trusted(&k(1)));
    }

    #[test]
    fn sync_replaces_trusted_set() {
        let mut s = TrustState::new();
        s.add_trusted(k(1));
        s.add_trusted(k(2));
        let changed = s.apply(TrustEvent::Sync {
            keys: vec![k(2), k(3), k(4)],
        });
        assert!(changed);
        assert!(!s.is_trusted(&k(1)));
        assert!(s.is_trusted(&k(2)));
        assert!(s.is_trusted(&k(3)));
        assert!(s.is_trusted(&k(4)));
    }

    #[test]
    fn sync_respects_revoked_blacklist() {
        let mut s = TrustState::new();
        s.apply(TrustEvent::Revoke { node_id: k(1) });
        // Sync probuje dodac k(1) ponownie — odrzucone.
        s.apply(TrustEvent::Sync {
            keys: vec![k(1), k(2)],
        });
        assert!(!s.is_trusted(&k(1)), "revoked key must stay revoked");
        assert!(s.is_trusted(&k(2)));
    }

    #[test]
    fn idempotent_sync_returns_false() {
        let mut s = TrustState::new();
        s.apply(TrustEvent::Sync { keys: vec![k(1), k(2)] });
        let changed = s.apply(TrustEvent::Sync { keys: vec![k(1), k(2)] });
        assert!(!changed, "identical sync should not report change");
    }
}

// =============================================================================
// Plik: addon/utils.rs
// Opis: Wspolne funkcje pomocnicze dla modulu addonow.
//       D5: Przeniesiony tu zduplikowany fnv1a_hash z mod.rs i host_functions/mod.rs.
// =============================================================================

/// Hash FNV-1a — szybki hash do indeksowania audit logow i wyszukiwania.
/// Uzywa stalych FNV-1a: offset basis 0xcbf29ce484222325, prime 0x100000001b3.
pub fn fnv1a_hash(s: &str) -> i64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in s.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fnv1a_deterministic() {
        let h1 = fnv1a_hash("addon.install");
        let h2 = fnv1a_hash("addon.install");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_fnv1a_different_strings() {
        let h1 = fnv1a_hash("addon.install");
        let h2 = fnv1a_hash("addon.uninstall");
        assert_ne!(h1, h2);
    }
}

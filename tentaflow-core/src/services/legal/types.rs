// ============ File: legal/types.rs — RODO document variant + DTOs ============
//
// Variant taxonomy mirrors the three RODO/GDPR document templates the
// generator emits:
//   * `short`    — one-pager summary for end-user consent UI
//   * `standard` — full information clause, default for production deploys
//   * `full`     — extended legal pack including DPO contact + SCC clauses
//
// String form is stable and lands in the DB CHECK constraint
// (`legal_documents.variant`) and on the wire (signed URL references), so it
// must not change without a migration.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RodoVariant {
    Short,
    Standard,
    Full,
}

impl RodoVariant {
    /// Canonical wire / DB representation. Pair with [`RodoVariant::from_str`]
    /// to round-trip values through SQLite TEXT columns.
    pub fn as_str(self) -> &'static str {
        match self {
            RodoVariant::Short => "short",
            RodoVariant::Standard => "standard",
            RodoVariant::Full => "full",
        }
    }

    /// Parse the canonical lowercase form. Returns `None` for any other
    /// input — callers translate that into a typed error at the boundary.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "short" => Some(RodoVariant::Short),
            "standard" => Some(RodoVariant::Standard),
            "full" => Some(RodoVariant::Full),
            _ => None,
        }
    }
}

impl std::fmt::Display for RodoVariant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rodo_variant_round_trips_through_str() {
        for v in [RodoVariant::Short, RodoVariant::Standard, RodoVariant::Full] {
            assert_eq!(RodoVariant::from_str(v.as_str()), Some(v));
        }
    }

    #[test]
    fn rodo_variant_rejects_unknown_input() {
        assert_eq!(RodoVariant::from_str(""), None);
        assert_eq!(RodoVariant::from_str("SHORT"), None);
        assert_eq!(RodoVariant::from_str("medium"), None);
    }
}

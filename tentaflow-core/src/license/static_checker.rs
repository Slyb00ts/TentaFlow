// =============================================================================
// Plik: static_checker.rs
// Opis: Stub LicenseChecker — zwraca staly tier z konfiguracji. W v1 hardcoded
//       na Free. W przyszlosci konfigurowalne (env var TENTAFLOW_LICENSE_TIER
//       lub plik konfiguracyjny).
// =============================================================================

use super::checker::{LicenseChecker, LicenseTier};

pub struct StaticLicenseChecker {
    tier: LicenseTier,
}

impl StaticLicenseChecker {
    pub fn new(tier: LicenseTier) -> Self {
        Self { tier }
    }

    pub fn free() -> Self {
        Self {
            tier: LicenseTier::Free,
        }
    }

    pub fn pro() -> Self {
        Self {
            tier: LicenseTier::Pro,
        }
    }
}

impl Default for StaticLicenseChecker {
    fn default() -> Self {
        Self::free()
    }
}

impl LicenseChecker for StaticLicenseChecker {
    fn tier(&self) -> LicenseTier {
        self.tier
    }
}

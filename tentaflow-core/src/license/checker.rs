// =============================================================================
// Plik: checker.rs
// Opis: Trait LicenseChecker oraz typy LicenseTier i LicenseError. Definiuje
//       abstrakcje sprawdzajaca aktualny tier licencji uzytkownika.
// =============================================================================

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LicenseTier {
    Free,
    Pro,
    Enterprise,
}

#[derive(Debug, thiserror::Error)]
pub enum LicenseError {
    #[error("Funkcja '{feature}' wymaga licencji {required:?}, posiadana: {actual:?}")]
    Insufficient {
        feature: String,
        required: LicenseTier,
        actual: LicenseTier,
    },
}

/// Trait sprawdzajacy poziom licencji. Implementacje:
/// - StaticLicenseChecker (stub Free, v1)
/// - HttpLicenseChecker (przyszlosc — sprawdzanie online)
pub trait LicenseChecker: Send + Sync {
    /// Aktualny tier licencji.
    fn tier(&self) -> LicenseTier;

    /// Sprawdza czy aktualny tier pozwala na funkcje wymagajaca min `required`.
    fn allows(&self, required: LicenseTier) -> bool {
        let actual = self.tier();
        matches!(
            (actual, required),
            (LicenseTier::Enterprise, _)
                | (LicenseTier::Pro, LicenseTier::Pro | LicenseTier::Free)
                | (LicenseTier::Free, LicenseTier::Free)
        )
    }
}

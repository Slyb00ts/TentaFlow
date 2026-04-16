// =============================================================================
// Plik: checker.rs
// Opis: Trait LicenseChecker oraz typy LicenseTier i LicenseError. Definiuje
//       abstrakcje sprawdzajaca aktualny tier licencji oraz wymagania licencyjne
//       dla wariantow manifestu serwisow.
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

    /// Wygodna funkcja: sprawdza wariant manifestu, zwraca Err jezeli download
    /// wymaga wyzszego tieru niz aktualny.
    fn check_variant_download(
        &self,
        variant: &crate::services::manifest::Variant,
        feature_name: &str,
    ) -> Result<(), LicenseError> {
        let Some(download) = &variant.download else {
            return Ok(());
        };
        let required = match download.license_required {
            crate::services::manifest::RequiredLicenseTier::Pro => LicenseTier::Pro,
            crate::services::manifest::RequiredLicenseTier::Enterprise => LicenseTier::Enterprise,
        };
        if self.allows(required) {
            Ok(())
        } else {
            Err(LicenseError::Insufficient {
                feature: feature_name.to_string(),
                required,
                actual: self.tier(),
            })
        }
    }
}

// =============================================================================
// Plik: mod.rs
// Opis: Modul licencji TentaFlow — abstrakcja sprawdzajaca tier uzytkownika.
//       W v1 stub zwraca zawsze Free. Pelna integracja z backendem licencji
//       w przyszlej wersji Pro/Enterprise.
// =============================================================================

mod checker;
mod static_checker;

pub use checker::{LicenseChecker, LicenseError, LicenseTier};
pub use static_checker::StaticLicenseChecker;

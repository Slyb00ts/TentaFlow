// =============================================================================
// Plik: mod.rs
// Opis: Modul manifestu serwisow — laduje wbudowane manifesty z TOML
//       (tentaflow-containers/*/_services/), waliduje semantyke i udostepnia
//       dane runtime przez globalny rejestr.
// =============================================================================

mod registry;
mod types;
mod validate;

pub use registry::{registry, ManifestRegistry};
pub use types::*;
pub use validate::{validate_engine, ValidationError};

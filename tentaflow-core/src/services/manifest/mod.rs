// =============================================================================
// Plik: mod.rs
// Opis: Modul manifestu serwisow — laduje wbudowane manifesty z TOML
//       (tentaflow-containers/*/_services/), waliduje semantyke i udostepnia
//       dane runtime przez globalny rejestr.
// =============================================================================

mod registry;
pub mod runtime_validate;
mod types;
mod validate;
mod vocabulary;

pub use vocabulary::{
    VALID_INPUT_MODALITIES, VALID_OUTPUT_MODALITIES, VALID_SERVICE_SURFACES,
};

#[cfg(test)]
mod tests;

pub use registry::{registry, ManifestRegistry};
pub use types::*;
pub use validate::{validate_engine, validate_engine_id, ValidationError};

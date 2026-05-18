// ============ File: legal/mod.rs — F2 P8.a RODO/GDPR legal document module ============
//
// Foundation for the F2 P8 legal-pack feature. This chunk (P8.a) covers the
// document variant taxonomy only; the PDF renderer, signed-URL minting and
// the dashboard surface land in later P8 chunks.
//
// Storage lives in `db::legal_documents` (table created by migration v37).
// The two permission keys `legal.read` / `legal.write` are seeded into the
// roles preseed by migration v32 (admin / dpo: read+write; operator / viewer:
// read only) and gate every host-fn touching this module.

pub mod rodo_generator;
pub mod types;

pub use rodo_generator::{
    generate as generate_rodo, RodoGenerationError, RodoGenerationInput, RodoGenerationOutput,
};
pub use types::RodoVariant;

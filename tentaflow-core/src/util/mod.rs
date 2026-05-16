// =============================================================================
// File: util/mod.rs — shared low-level primitives reused across crate modules
// =============================================================================
//
// Holds tiny self-contained types used by multiple subsystems (rate limiters,
// schedulers, ...). Anything here MUST stay dependency-free except for `std`
// — adding heavyweight imports forces every consumer to pull them.

pub mod token_bucket;

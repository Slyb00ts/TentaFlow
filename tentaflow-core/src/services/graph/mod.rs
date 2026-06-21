// ===== Plik: services/graph/mod.rs — wbudowana baza grafowa (CozoDB) =====
//
// Per-addon per-collection grafy oparte o embedded CozoDB (jeden plik Cozo na
// kolekcję, trwały na dysku). API dla addona dochodzi w slice B1
// (`addon::host_functions::graph`); ten moduł posiada abstrakcję trait,
// implementację Cozo, rejestr `(org, addon, collection) -> Backend`, quoty per
// addon oraz PPR liczony w Rust nad CSR z Cozo. Lustro `services::vector`.

pub mod backend;
pub mod collection;
pub mod csr;
pub mod error;
pub mod ppr;

pub use backend::{CozoBackend, GraphBackend, GraphEngine, NeighborDir, TOMBSTONE_LABEL};
pub use collection::{
    GraphManager, MAX_COLLECTIONS_PER_ADDON, MAX_EDGES_PER_ADDON, MAX_NODES_PER_ADDON,
};
pub use csr::Csr;
pub use error::{GraphError, Result as GraphResult};
pub use ppr::{personalized_pagerank, PprScores};

#[cfg(test)]
mod tests;

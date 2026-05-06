// =============================================================================
// Plik: flow_engine/node_adapters/mod.rs
// Opis: Nowe adaptery dla flow_engine clean rewrite (plan v4.1). Każdy
//       implementuje `flow_engine::node_adapter::NodeAdapter` (single execute
//       method). Stage 1b: standalone — koegzystują z legacy `flow_engine::
//       adapters` do czasu executor rewrite w stage 1c.
// =============================================================================

pub mod output;

pub use output::OutputNodeAdapter;

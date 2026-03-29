// =============================================================================
// Plik: main.rs
// Opis: Punkt wejscia TentaFlow Desktop — deleguje do lib::run().
// =============================================================================

fn main() -> anyhow::Result<()> {
    tentaflow_desktop_core::run()
}

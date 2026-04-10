// =============================================================================
// Plik: examples/inspect_model.rs
// Opis: Narzedzie CLI do inspekcji pliku .onnx — wypisuje liste tensorow
//       z ich nazwami i shape'ami. Uzywa do rekonstrukcji architektury modelu.
//
// Uzycie: cargo run --release --example inspect_model -- <path_to_onnx>
// =============================================================================

use tentaflow_voice::OnnxWeights;

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .expect("Uzycie: inspect_model <sciezka_do_modelu.onnx>");

    println!("Ladowanie {}...", path);
    let weights = OnnxWeights::load(&path)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    println!("Zaladowano {} tensorow\n", weights.len());

    let mut names = weights.names();
    names.sort();

    println!("{:<60} {:<20} {:>12}", "NAME", "SHAPE", "NUM_ELEMENTS");
    println!("{}", "-".repeat(96));
    for name in &names {
        if let Ok(t) = weights.get(name) {
            let shape_str = format!("{:?}", t.shape);
            println!(
                "{:<60} {:<20} {:>12}",
                name,
                shape_str,
                t.num_elements()
            );
        }
    }

    Ok(())
}

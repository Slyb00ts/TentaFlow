// =============================================================================
// Plik: examples/bootstrap_python_bundle.rs
// Opis: Uruchamia `deploy::python_venv::bootstrap(engine)` — pobiera Pythona,
//       tworzy venv, instaluje wheels. Silnik NIE jest spawnowany —
//       sprawdzamy sam bootstrap.
// Uzycie: cargo run --release --example bootstrap_python_bundle \
//           --features inference-diarization,docker,dashboard-api -- voxcpm
// =============================================================================

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let engine = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "voxcpm".to_string());
    println!("Bootstrap bundle: {}", engine);

    let started = std::time::Instant::now();
    let result = tentaflow_core::deploy::python_venv::bootstrap(&engine);
    match result {
        Ok(boot) => {
            println!("\n=== OK po {:.1?} ===", started.elapsed());
            println!("engine:        {}", boot.engine);
            println!("venv_dir:      {}", boot.venv_dir.display());
            println!("python_bin:    {}", boot.python_bin.display());
            println!("internal_port: {}", boot.internal_port);
        }
        Err(e) => {
            eprintln!("\n=== BLAD po {:.1?} ===", started.elapsed());
            eprintln!("{:#}", e);
            std::process::exit(1);
        }
    }
    Ok(())
}

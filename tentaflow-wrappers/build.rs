// =============================================================================
// Plik: build.rs
// Opis: Ustawia flagi linkera potrzebne przy jednoczesnym linkowaniu wrapperów
//       nad llama.cpp i whisper.cpp.
// Przykład: cargo test --manifest-path tentaflow-wrappers/Cargo.toml --features all
// =============================================================================

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match target_os.as_str() {
        "linux" => {
            println!("cargo:rustc-link-arg=-Wl,--allow-multiple-definition");
        }
        "windows" => {
            println!("cargo:rustc-link-arg=/FORCE:MULTIPLE");
        }
        _ => {}
    }
}

// ===== File: gguf_loads.rs — ten sam model, drugi format =====
//
// Bielik istnieje w dwóch postaciach: eksport MLX (affine 4-bit, grupa 64,
// skale bf16) i GGUF Q4_K_M (Q4_K na większości wag, Q6_K na attn_v, ffn_down
// i głowie). To ten sam model, więc jedyne, co ten test sprawdza, to czy droga
// wejścia jest naprawdę wspólna — czy model wczyta jedno i drugie bez ani
// jednej gałęzi po swojej stronie.
#![cfg(all(feature = "metal", any(target_os = "macos", target_os = "ios")))]

use std::path::{Path, PathBuf};

use forge_hal::metal_device::MetalDevice;
use forge_model::mlx_dense::MlxDense;

const GGUF: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../.runtime/models/bielik-minitron-7b-v3-gguf/minitron-Bielik-7B-v3.0-Instruct-Q4_K_M.gguf"
);

fn gguf() -> Option<PathBuf> {
    let p = PathBuf::from(GGUF);
    p.is_file().then_some(p)
}

#[test]
#[ignore]
fn the_gguf_build_of_the_same_model_loads_and_generates() {
    let Some(path) = gguf() else {
        eprintln!("pomijam: brak pliku GGUF");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };

    let mut model = MlxDense::load(device, Path::new(&path)).expect("wczytanie GGUF");
    let shape = model.shape();
    assert_eq!(shape.layers, 40, "inna liczba warstw niż w MLX");
    assert_eq!(shape.hidden, 4096);
    assert_eq!(shape.kv_heads * shape.head_dim, 1024);

    // Prompt przechodzi przez prefill (forma blokowa i macierzowa) oraz
    // dekodowanie (wektorowa), czyli przez wszystkie trzy formy na obu
    // szerokościach kodu naraz.
    let prompt: Vec<u32> = vec![1, 4234, 8123, 302, 15];
    let first = model.prefill(&prompt).expect("prefill");
    let mut out = vec![first];
    for _ in 0..3 {
        out.push(model.step_argmax(*out.last().unwrap()).expect("krok"));
    }
    eprintln!("wygenerowane tokeny: {out:?}");
    assert!(
        out.iter().all(|t| *t < shape.vocab),
        "token poza słownikiem: {out:?}"
    );
    // Model zwracający w kółko ten sam token przeszedł ścieżkę, ale nie
    // policzył — a to wygląda identycznie w statystykach.
    assert!(
        out.iter().collect::<std::collections::HashSet<_>>().len() > 1,
        "wszystkie tokeny identyczne, model nie liczy: {out:?}"
    );
}

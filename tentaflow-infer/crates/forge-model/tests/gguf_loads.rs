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

/// Ten sam model w dwóch formatach, na tej samej maszynie, w jednym przebiegu.
///
/// Przeplatane, bo maszyna dryfuje termicznie — dwa osobne uruchomienia
/// porównywałyby temperaturę, a nie format.
#[test]
#[ignore]
fn the_two_formats_of_the_same_model_side_by_side() {
    let Some(gguf_path) = gguf() else {
        eprintln!("pomijam: brak pliku GGUF");
        return;
    };
    let mlx_path = {
        let snapshots = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../.runtime/models/models--agentGreg--Bielik-Minitron-7B-v3.0-Instruct-MLX-4bit/snapshots"
        ));
        let Some(dir) = std::fs::read_dir(&snapshots).ok().and_then(|mut d| d.next()) else {
            eprintln!("pomijam: brak checkpointu MLX");
            return;
        };
        dir.expect("wpis katalogu").path()
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };

    // Tokenizer bierzemy Z PLIKU GGUF — to ten sam, który niesie model, więc
    // porównanie nie zależy od tego, czy obok leży `tokenizer.json`.
    let gg = forge_formats::gguf::Gguf::open(&gguf_path).expect("gguf");
    let vocab = forge_tokenize::gguf_vocab(&gg).expect("słownik z gguf");
    let tokenizer = forge_tokenize::Tokenizer::from_gguf_vocab(&vocab).expect("tokenizer");
    let mut prompt = tokenizer.encode("Stolica Polski to", false).expect("encode");
    prompt.insert(0, 1);

    let mut row = |label: &str, path: &Path| {
        let mut m = MlxDense::load(device.clone(), path).expect("wczytanie");
        // Rozgrzewka, potem mediana z trzech — jak w pozostałych pomiarach.
        let mut prefills = Vec::new();
        let mut first = 0;
        for r in 0..4 {
            m.reset();
            let t = std::time::Instant::now();
            first = m.prefill(&prompt).expect("prefill");
            if r > 0 {
                prefills.push(t.elapsed().as_secs_f64());
            }
        }
        prefills.sort_by(f64::total_cmp);
        let prefill = prefills[prefills.len() / 2];

        let t = std::time::Instant::now();
        let mut tok = first;
        let mut out = vec![first];
        for _ in 0..15 {
            tok = m.step_argmax(tok).expect("krok");
            out.push(tok);
        }
        let decode = t.elapsed().as_secs_f64();
        eprintln!(
            "{label:6}: prefill {:.3} s ({:.1} tok/s), dekodowanie {:.1} tok/s",
            prefill,
            prompt.len() as f64 / prefill,
            15.0 / decode
        );
        eprintln!("        {:?}", tokenizer.decode(&out, true).unwrap_or_default());
        out
    };

    let mlx = row("MLX", &mlx_path);
    let gguf_out = row("GGUF", &gguf_path);

    // Dwie NIEZALEŻNE kwantyzacje tego samego modelu różnią się na wadze o
    // około 11% — zmierzone — więc po czterdziestu warstwach mają pełne prawo
    // pisać co innego. Zgodność tokenów NIE jest tu kontraktem.
    //
    // Kontraktem jest to, że oba piszą JĘZYKIEM. Bez permutacji wierszy Q i K
    // ścieżka GGUF dawała „mušić,|\n\n\n\n /******/" — płynne śmieci, które
    // przechodzą każdy test na „tokeny są różne i w słowniku".
    for (label, out) in [("MLX", &mlx), ("GGUF", &gguf_out)] {
        let text = tokenizer.decode(out, true).expect("detokenizacja");
        let letters = text.chars().filter(|c| c.is_alphabetic() || *c == ' ').count();
        assert!(
            letters * 10 >= text.chars().count() * 6,
            "{label}: {letters} liter na {} znaków — to nie jest tekst: {text:?}",
            text.chars().count()
        );
    }
}

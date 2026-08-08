// =============================================================================
// Plik: moe_prefill_gpu.rs
// Opis: Porównuje logity prefillu mieszanki z krokiem po kroku po tych samych tokenach.
// Przykład: FORGE_TEST_MOE_GGUF=model.gguf cargo test -p forge-engine --release --test moe_prefill_gpu -- --nocapture
// =============================================================================

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use forge_engine::model::{Model, ModelConfig};
use forge_formats::Gguf;
use forge_hal::Device;
use forge_hal::{PoolSizes, gpu};
use forge_tokenize::{Tokenizer, gguf_vocab};

mod common;

type TestResult<T> = Result<T, Box<dyn Error>>;

const SKIP: &str = "parity prefillu mieszanki";

const CONTINUATION: usize = 16;

/// Realny tekst, a nie losowe identyfikatory.
///
/// Na losowych tokenach model wpada w pętlę powtórzeń, w której argmax stoi na
/// remisach — wtedy dowolna różnica zaokrąglenia przestawia fazę pętli i test
/// mierzy tę pętlę zamiast matematyki warstwy.
const PROMPT_TEXT: &str = "The quick brown fox jumps over the lazy dog. \
Paged attention keeps one page per block of tokens, so a sequence grows without \
copying what it already holds. A mixture of experts routes each token to a few \
of its many feed-forward blocks, which is why its cost per token stays flat as \
the parameter count grows. This paragraph exists to give the model something \
ordinary to continue.";

fn argmax(logits: &[f32]) -> u32 {
    logits
        .iter()
        .enumerate()
        .max_by(|left, right| left.1.total_cmp(right.1))
        .map(|(index, _)| index as u32)
        .expect("niepusty rozkład")
}

fn prompt(path: &Path) -> TestResult<Vec<u32>> {
    let gguf = Gguf::open(path)?;
    let tokenizer = Tokenizer::from_gguf_vocab(&gguf_vocab(&gguf)?)?;
    let tokens = tokenizer.encode(PROMPT_TEXT, true).map_err(|e| e.to_string())?;
    assert!(tokens.len() >= 32, "prompt testowy jest za krótki");
    Ok(tokens)
}

fn load(path: &Path) -> Option<Model> {
    let free = match gpu::free_vram(0) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("pominięto {SKIP}: brak CUDA: {error}");
            return None;
        }
    };
    let activations = 1usize << 30;
    let kv_cache = 256usize << 20;
    let weights = common::weights_pool(path, free, activations + kv_cache, SKIP)?;
    let device: Arc<dyn Device> = match gpu::open(
        0,
        PoolSizes {
            weights,
            kv_cache,
            activations,
            kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
        },
    ) {
        Ok(device) => device,
        Err(error) => {
            eprintln!("pominięto {SKIP}: nie można utworzyć CUDA: {error}");
            return None;
        }
    };
    Some(
        Model::load_gguf(
            device,
            path,
            ModelConfig {
                kv_page_size: 32,
                kv_pages: 16,
                max_seq_len: 256,
                ..ModelConfig::default()
            },
        )
        .expect("model mieszanki powinien się załadować"),
    )
}

/// Ten sam prompt policzony wsadowo i token po tokenie musi dać ten sam rozkład.
///
/// Prefill i decode liczą TĘ SAMĄ funkcję, więc rozjazd większy niż szum
/// zaokrągleń oznacza, że któraś ze ścieżek liczy inną matematykę — a nie da
/// się tego zobaczyć po samych wygenerowanych tokenach, bo argmax długo maskuje
/// przesunięty rozkład.
#[test]
fn prefill_mieszanki_zgadza_sie_z_krokiem_po_kroku() -> TestResult<()> {
    let Some(path) = std::env::var_os("FORGE_TEST_MOE_GGUF").map(PathBuf::from) else {
        eprintln!("pominięto {SKIP}: brak FORGE_TEST_MOE_GGUF");
        return Ok(());
    };
    assert!(path.is_file(), "FORGE_TEST_MOE_GGUF nie wskazuje pliku");
    let Some(mut model) = load(&path) else {
        return Ok(());
    };
    assert!(
        model.weights.is_moe(),
        "FORGE_TEST_MOE_GGUF musi wskazywać model z mieszanką ekspertów"
    );
    let tokens = prompt(&path)?;

    // Jedna sekwencja naraz. Model hybrydowy trzyma stan rekurencyjny DeltaNet w
    // puli per sekwencja, więc dwie żywe naraz mierzyłyby politykę tej puli, a
    // nie prefill.
    let mut walk = |model: &mut Model, seq: &mut _, first: &[f32]| -> TestResult<Vec<u32>> {
        let mut ids = vec![argmax(first)];
        for _ in 1..CONTINUATION {
            let next = model.step(seq, *ids.last().expect("niepusty ciąg"))?;
            ids.push(argmax(&next));
        }
        Ok(ids)
    };

    let mut batched = model.new_seq();
    let batched_logits = model.prefill_chunk(&mut batched, &tokens)?;
    let batched_walk = walk(&mut model, &mut batched, &batched_logits)?;
    model.release_seq(&mut batched);

    let mut serial = model.new_seq();
    let mut serial_logits = Vec::new();
    for &token in &tokens {
        serial_logits = model.step(&mut serial, token)?;
    }
    let serial_walk = walk(&mut model, &mut serial, &serial_logits)?;
    model.release_seq(&mut serial);

    // Połowa bramki odporna na próg: chciwe wyjście z obu stanów musi iść tymi
    // samymi tokenami. Rozkład przesunięty na tyle, żeby to miało znaczenie,
    // zmienia to, co model mówi — i widać to bez zgadywania granicy.
    assert_eq!(
        batched_walk, serial_walk,
        "prefill wsadowy prowadzi do innego ciągu"
    );

    assert_eq!(batched_logits.len(), serial_logits.len());
    let mut max_abs = 0.0f32;
    let mut max_rel = 0.0f32;
    for (batched, serial) in batched_logits.iter().zip(&serial_logits) {
        assert!(batched.is_finite() && serial.is_finite());
        let absolute = (batched - serial).abs();
        max_abs = max_abs.max(absolute);
        max_rel = max_rel.max(absolute / serial.abs().max(1e-3));
    }
    let top1 = |logits: &[f32]| {
        logits
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.total_cmp(right.1))
            .map(|(index, _)| index)
    };
    println!("parity prefill/decode: max_abs={max_abs} max_rel={max_rel}");
    assert_eq!(
        top1(&batched_logits),
        top1(&serial_logits),
        "prefill i decode wskazują inny token"
    );
    // The grouped path keeps the sum in f32 across all top_k and rounds once,
    // where the per-token route rounded after each expert, and a tile reduces in
    // a different order than a GEMV — so the two agree to rounding, not to the
    // bit. Measured on this machine over this prompt: Qwen3-30B-A3B Q4_K 0,30.
    assert!(max_abs <= 0.9, "logity rozjeżdżają się o {max_abs}");
    Ok(())
}

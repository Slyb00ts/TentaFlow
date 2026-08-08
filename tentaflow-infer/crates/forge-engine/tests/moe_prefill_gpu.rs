// =============================================================================
// Plik: moe_prefill_gpu.rs
// Opis: Porównuje logity prefillu mieszanki z krokiem po kroku po tych samych tokenach.
// Przykład: FORGE_TEST_MOE_GGUF=model.gguf cargo test -p forge-engine --release --test moe_prefill_gpu -- --nocapture
// =============================================================================

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use forge_engine::model::{Model, ModelConfig};
use forge_hal::Device;
use forge_hal::{PoolSizes, gpu};

mod common;

type TestResult<T> = Result<T, Box<dyn Error>>;

const SKIP: &str = "parity prefillu mieszanki";

const PROMPT_LEN: usize = 96;

fn prompt(vocab: usize, len: usize) -> Vec<u32> {
    assert!(vocab > 2, "model testowy musi mieć niepusty słownik");
    (0..len)
        .map(|index| 1 + ((1709 + index * 17) % (vocab - 1)) as u32)
        .collect()
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
                max_seq_len: PROMPT_LEN + 8,
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
    let tokens = prompt(model.weights.descriptor.params.vocab_size, PROMPT_LEN);

    let mut batched = model.new_seq();
    let batched_logits = model.prefill_chunk(&mut batched, &tokens)?;
    model.release_seq(&mut batched);

    let mut serial = model.new_seq();
    let mut serial_logits = Vec::new();
    for &token in &tokens {
        serial_logits = model.step(&mut serial, token)?;
    }
    model.release_seq(&mut serial);

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
    assert!(max_abs <= 0.35, "logity rozjeżdżają się o {max_abs}");
    Ok(())
}

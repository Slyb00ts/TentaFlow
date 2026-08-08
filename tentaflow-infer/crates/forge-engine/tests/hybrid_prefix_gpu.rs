// =============================================================================
// Plik: hybrid_prefix_gpu.rs
// Opis: Pozyczony prefiks hybrydy musi dac te same tokeny co zimny przebieg.
// Przyklad: FORGE_TEST_PARITY_GGUF=model.gguf cargo test -p forge-engine --release --test hybrid_prefix_gpu -- --nocapture
// =============================================================================

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use forge_engine::model::{Model, ModelConfig, MAX_PREFILL_CHUNK};
use forge_formats::Gguf;
use forge_hal::Device;
use forge_hal::{PoolSizes, gpu};
use forge_tokenize::{Tokenizer, gguf_vocab};

mod common;

type TestResult<T> = Result<T, Box<dyn Error>>;

const SKIP: &str = "prefiks hybrydy";

/// Tyle tokenow generujemy po prefillu. Rozjazd stanu rekurencyjnego nie widac
/// na pierwszym tokenie — stan wchodzi w KAZDY nastepny, wiec dopiero ciag
/// pokazuje, czy przywrocony checkpoint jest tym samym stanem.
const CONTINUATION: usize = 24;

/// Prompt musi przekroczyc kilka stron, zeby pozyczka w ogole miala co objac.
const PARAGRAPH: &str = "Paged attention keeps one page per block of tokens, so a \
sequence grows without copying what it already holds. A mixture of experts routes \
each token to a few of its many feed-forward blocks, which is why its cost per \
token stays flat as the parameter count grows. A recurrent layer keeps none of \
that: everything the sequence has said is folded into one state matrix that the \
next token overwrites in place. ";

fn argmax(logits: &[f32]) -> u32 {
    logits
        .iter()
        .enumerate()
        .max_by(|left, right| left.1.total_cmp(right.1))
        .map(|(index, _)| index as u32)
        .expect("niepusty rozkład")
}

fn prompt(path: &Path, lead: &str, repeats: usize, tail: &str) -> TestResult<Vec<u32>> {
    let gguf = Gguf::open(path)?;
    let tokenizer = Tokenizer::from_gguf_vocab(&gguf_vocab(&gguf)?)?;
    let text = lead.to_string() + &PARAGRAPH.repeat(repeats) + tail;
    let tokens = tokenizer.encode(&text, true).map_err(|e| e.to_string())?;
    assert!(tokens.len() > 256, "prompt testowy jest za krótki");
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
    let activations = 2usize << 30;
    let kv_cache = 512usize << 20;
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
                kv_pages: 512,
                max_seq_len: 4096,
                ..ModelConfig::default()
            },
        )
        .expect("model powinien się załadować"),
    )
}

/// Chciwy przebieg przez prefiks: ile tokenów przyszło z cache'u, ile trwał
/// prefill i co model powiedział.
fn greedy(model: &mut Model, tokens: &[u32]) -> TestResult<(Vec<u32>, usize, f64)> {
    let mut seq = model.new_seq();
    let cache_read = model.acquire_prefix(&mut seq, tokens);
    let started = Instant::now();
    let mut logits = Vec::new();
    for chunk in tokens[cache_read..].chunks(MAX_PREFILL_CHUNK) {
        logits = model.prefill_chunk(&mut seq, chunk)?;
    }
    let prefill_s = started.elapsed().as_secs_f64();
    let mut ids = vec![argmax(&logits)];
    for _ in 1..CONTINUATION {
        let next = model.step(&mut seq, *ids.last().expect("niepusty ciąg"))?;
        ids.push(argmax(&next));
    }
    model.release_seq(&mut seq);
    Ok((ids, cache_read, prefill_s))
}

/// Prefiks hybrydy niesie stan, którego żadna strona nie opisuje.
///
/// Pożyczka bez niego wznawiałaby sekwencję w połowie myśli — cicho, bo strony
/// K/V byłyby poprawne, a różnicę widać dopiero w tym, co model mówi dalej.
/// Dlatego bramką nie jest „czy trafiło", tylko czy trafiony przebieg idzie
/// TYMI SAMYMI tokenami co zimny.
#[test]
fn pozyczony_prefiks_daje_ten_sam_ciag_co_zimny() -> TestResult<()> {
    let Some(path) = std::env::var_os("FORGE_TEST_PARITY_GGUF").map(PathBuf::from) else {
        eprintln!("pominięto {SKIP}: brak FORGE_TEST_PARITY_GGUF");
        return Ok(());
    };
    assert!(path.is_file(), "FORGE_TEST_PARITY_GGUF nie wskazuje pliku");
    let Some(mut model) = load(&path) else {
        return Ok(());
    };
    if !model.is_hybrid() {
        eprintln!("pominięto {SKIP}: checkpoint nie jest hybrydowy");
        return Ok(());
    }
    assert!(
        model.prefix_enabled(),
        "hybryda z jedną rangą ma mieć aktywny prefiks współdzielony"
    );
    const LEAD: &str = "This inventory describes one machine. ";
    let tokens = prompt(&path, LEAD, 12, "In one sentence, what does the last paragraph say?")?;

    // Rozgrzewka łapie kompilację grafów i zegar karty, ale MUSI iść innym
    // tekstem od pierwszego tokenu — inaczej to ona zapełniłaby drzewo i
    // „zimny" przebieg zaczynałby od pożyczki.
    let warmup = prompt(&path, "A different machine entirely. ", 9, "Name one property.")?;
    greedy(&mut model, &warmup)?;

    let (cold, cold_read, cold_s) = greedy(&mut model, &tokens)?;
    assert_eq!(cold_read, 0, "zimny przebieg nie ma czego pożyczyć");

    let (warm, warm_read, warm_s) = greedy(&mut model, &tokens)?;
    let page = 32;
    assert_eq!(
        warm_read,
        tokens.len() / page * page,
        "pożyczka ma objąć wszystkie pełne strony promptu"
    );
    println!(
        "prefiks hybrydy: {warm_read}/{} tok z cache'u, prefill {:.1} ms → {:.1} ms",
        tokens.len(),
        cold_s * 1e3,
        warm_s * 1e3
    );
    assert_eq!(cold, warm, "pożyczony prefiks zmienił wygenerowany ciąg");
    assert!(
        warm_s < cold_s,
        "pożyczka nie skróciła prefillu: {warm_s} wobec {cold_s}"
    );

    // Rodzeństwo: ten sam początek, rozjazd w POŁOWIE — dokładnie ten przypadek,
    // dla którego jeden checkpoint na końcu promptu jest bezużyteczny. Pożyczka
    // musi tu stanąć na checkpoincie pośrednim.
    let sibling = prompt(&path, LEAD, 8, "In one sentence, what is a mixture of experts?")?;
    let (_, sibling_read, sibling_s) = greedy(&mut model, &sibling)?;
    println!("rodzeństwo prefiksu: {sibling_read} tok z cache'u, prefill {sibling_s:.3} s");
    assert!(
        sibling_read >= 512,
        "rozjazd w połowie powinien pożyczyć checkpoint pośredni, wziął {sibling_read}"
    );
    Ok(())
}

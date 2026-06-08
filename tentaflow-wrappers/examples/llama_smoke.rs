// =============================================================================
// Plik: llama_smoke.rs
// Opis: Narzędzie smoke-test dla wrappera llama.cpp na lokalnym modelu GGUF.
// Przykład: cargo run --example llama_smoke --features llama -- --model model.gguf --metadata-only
// =============================================================================

use std::path::PathBuf;

use tentaflow_wrappers::llama::{inspect_gguf, silence_llama_logs, FlashAttentionMode};
use tentaflow_wrappers::llama_engine::{
    EngineConfig, FinishReason, GenRequest, LlamaEngine, SamplingParams, SpeculativeMode,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse()?;
    if args.spec_probe {
        return run_speculative_probe();
    }
    let info = inspect_gguf(&args.model)?;
    println!("model: {}", info.name);
    println!("path: {}", info.path.display());
    println!("size_mb: {}", info.size_bytes / 1024 / 1024);
    println!(
        "architecture: {}",
        info.architecture.as_deref().unwrap_or("-")
    );
    println!("tensors: {}", info.tensor_count);
    println!("metadata: {}", info.metadata_count);
    println!("context: {}", fmt_option(info.context_length));
    println!("embedding: {}", fmt_option(info.embedding_length));
    println!("vocab: {}", fmt_option(info.vocab_size));
    println!("mtp_layers: {}", info.mtp_layers);

    if args.metadata_only {
        return Ok(());
    }

    if !args.verbose_llama {
        silence_llama_logs();
    }

    // Generację prowadzi silnik continuous batching (jedyna ścieżka generacji).
    // Smoke wysyła pojedynczy request na jeden slot i konsumuje strumień tokenów.
    // MTP bez głowy nextn w modelu degradujemy do Off już tu (z nagłówka GGUF),
    // żeby nie polegać na błędzie ładowania silnika dla modelu bez MTP.
    let speculative = if matches!(args.speculative, SpeculativeMode::Mtp { .. }) && info.mtp_layers == 0
    {
        SpeculativeMode::Off
    } else {
        args.speculative
    };

    let config = EngineConfig {
        n_seq_max: 1,
        ctx_per_seq: args.ctx_size,
        n_gpu_layers: args.gpu_layers,
        threads: args.threads,
        flash_attn: args.flash_attn,
        speculative,
        ..EngineConfig::default()
    };
    let engine = LlamaEngine::load(&args.model, config)?;

    let request = GenRequest {
        prompt: args.prompt.clone(),
        system_prompt: None,
        sampling: SamplingParams {
            temperature: args.temperature,
            top_p: 0.9,
            top_k: 40,
            repeat_penalty: 1.0,
            seed: 0,
        },
        max_tokens: args.max_tokens,
        stop_sequences: vec!["</s>".to_string()],
    };

    println!("--- output ---");
    let stream = engine.submit(request)?;
    let mut generated_tokens = 0_u32;
    let mut finish = FinishReason::Error("brak finału".to_string());
    while let Some(token) = stream.recv() {
        if !token.text.is_empty() {
            print!("{}", token.text);
        }
        if token.is_final {
            generated_tokens = token.generated_tokens;
            if let Some(reason) = token.finish_reason {
                finish = reason;
            }
            break;
        }
    }
    println!();
    println!("--- stats ---");
    println!("generated_tokens: {generated_tokens}");
    println!("stop: {finish:?}");
    if let FinishReason::Error(msg) = finish {
        return Err(format!("generacja zakończona błędem: {msg}").into());
    }
    Ok(())
}

// Dowód linkowania i runtime C-ABI common_speculative: inicjalizuje kontekst
// ngram-self (nie wymaga modelu draftu) dla jednej sekwencji i zwalnia go.
// Potwierdza, że symbole z libllama-common.a są zlinkowane i wywoływalne.
// Dodatkowo regresja CR-001/CR-002/CR-003: init z n_seq=2 i globalny draft dla
// OBU sekwencji nie może crashować (per-seq bufory prompt/result), a błędny
// seq_id musi być bezpiecznie odrzucony bez aborta z biblioteki.
fn run_speculative_probe() -> Result<(), Box<dyn std::error::Error>> {
    use tentaflow_wrappers::llama::sys;

    let params = sys::llama_rs_speculative_params {
        type_: sys::LLAMA_RS_SPECULATIVE_TYPE_NGRAM_SIMPLE,
        n_max: 4,
        n_min: 1,
    };

    // SAFETY: przekazujemy poprawny wskaźnik na params, n_seq=1, n_rs_seq=0
    // (ngram-self nie wymaga snapshotów stanu rekurencyjnego). ctx_tgt/ctx_dft są
    // null, bo ngram nie używa modelu draftującego. Zwrócony wskaźnik jest
    // zwalniany przez llama_rs_speculative_free.
    let spec = unsafe {
        sys::llama_rs_speculative_init(
            &params,
            1,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if spec.is_null() {
        return Err("llama_rs_speculative_init zwrócił NULL".into());
    }

    let n_max = unsafe { sys::llama_rs_speculative_n_max(spec) };
    let need_embd = unsafe { sys::llama_rs_speculative_need_embd(spec) };
    let need_embd_nextn = unsafe { sys::llama_rs_speculative_need_embd_nextn(spec) };

    println!("spec_probe: init OK (type=ngram-simple, n_seq=1)");
    println!("spec_probe: n_max={n_max}");
    println!("spec_probe: need_embd={need_embd} need_embd_nextn={need_embd_nextn}");

    unsafe { sys::llama_rs_speculative_free(spec) };
    println!("spec_probe: free OK");

    run_speculative_multiseq_regression()?;

    Ok(())
}

// Regresja na wieloma sekwencjami: dowodzi, że poprawki CR-001/CR-002/CR-003
// eliminują null-deref i korupcję cross-seq w globalnej pętli draft oraz że
// błędny seq_id jest bezpiecznie odrzucany. Nie wymaga modelu: ngram-simple
// z pustym promptem draftuje pusty wynik bez dotykania kontekstu llama.
fn run_speculative_multiseq_regression() -> Result<(), Box<dyn std::error::Error>> {
    use tentaflow_wrappers::llama::sys;

    const N_SEQ: u32 = 2;

    let params = sys::llama_rs_speculative_params {
        type_: sys::LLAMA_RS_SPECULATIVE_TYPE_NGRAM_SIMPLE,
        n_max: 4,
        n_min: 1,
    };

    // SAFETY: poprawny wskaźnik na params, n_seq=2, ngram bez kontekstów (null);
    // uchwyt zwalniany na końcu.
    let spec = unsafe {
        sys::llama_rs_speculative_init(
            &params,
            N_SEQ,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if spec.is_null() {
        return Err("llama_rs_speculative_init(n_seq=2) zwrócił NULL".into());
    }

    // Draft dla OBU sekwencji. Każde wywołanie uruchamia GLOBALNĄ pętlę
    // common_speculative_draft(), która dereferencjuje *dp.result dla KAŻDEGO
    // seq_id. Przed poprawką seq bez ustawionego result dawał null-deref.
    // Pusty prompt → ngram-simple zwraca pusty draft, bez crasha.
    for seq in 0..N_SEQ as i32 {
        // SAFETY: spec niepusty, seq w zakresie [0, N_SEQ), prompt NULL/len=0.
        unsafe {
            sys::llama_rs_speculative_draft(
                spec,
                seq,
                4,
                0,
                0,
                std::ptr::null(),
                0,
            );
        }
        // SAFETY: odczyt długości draftu dla danego seq_id (out=NULL, cap=0).
        let len = unsafe { sys::llama_rs_speculative_draft_result(spec, seq, std::ptr::null_mut(), 0) };
        println!("spec_probe: multiseq draft OK seq={seq} draft_len={len}");
    }

    // Walidacja CR-003: seq_id spoza zakresu musi być no-op, bez aborta.
    let bad_seq: i32 = 5;
    // SAFETY: spec niepusty; shim odrzuca bad_seq przed wejściem do biblioteki.
    unsafe {
        sys::llama_rs_speculative_draft(spec, bad_seq, 4, 0, 0, std::ptr::null(), 0);
    }
    let bad_len = unsafe { sys::llama_rs_speculative_draft_result(spec, bad_seq, std::ptr::null_mut(), 0) };
    if bad_len != 0 {
        unsafe { sys::llama_rs_speculative_free(spec) };
        return Err(format!("draft_result(seq=5) powinien zwrócić 0, zwrócił {bad_len}").into());
    }
    // SAFETY: accept dla złego seq_id również musi być no-op (brak aborta).
    unsafe { sys::llama_rs_speculative_accept(spec, bad_seq, 1) };
    println!("spec_probe: bad seq_id={bad_seq} odrzucony bezpiecznie (no-op)");

    // SAFETY: zwolnienie uchwytu.
    unsafe { sys::llama_rs_speculative_free(spec) };
    println!("spec_probe: multiseq regression OK (n_seq=2, no crash)");
    Ok(())
}

#[derive(Debug)]
struct Args {
    model: PathBuf,
    prompt: String,
    max_tokens: u32,
    ctx_size: u32,
    gpu_layers: u32,
    threads: Option<u32>,
    temperature: f32,
    metadata_only: bool,
    verbose_llama: bool,
    flash_attn: FlashAttentionMode,
    speculative: SpeculativeMode,
    spec_probe: bool,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut args = std::env::args().skip(1);
        let mut parsed = Self {
            model: PathBuf::new(),
            prompt: "Napisz jedno krótkie zdanie po polsku.".to_string(),
            max_tokens: 32,
            ctx_size: 1024,
            gpu_layers: 0,
            threads: None,
            temperature: 0.2,
            metadata_only: false,
            verbose_llama: false,
            flash_attn: FlashAttentionMode::Auto,
            speculative: SpeculativeMode::Off,
            spec_probe: false,
        };

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--model" => parsed.model = PathBuf::from(next_value(&mut args, "--model")?),
                "--prompt" => parsed.prompt = next_value(&mut args, "--prompt")?,
                "--max-tokens" => {
                    parsed.max_tokens = parse_value(&mut args, "--max-tokens")?;
                }
                "--ctx-size" => parsed.ctx_size = parse_value(&mut args, "--ctx-size")?,
                "--gpu-layers" => parsed.gpu_layers = parse_value(&mut args, "--gpu-layers")?,
                "--threads" => parsed.threads = Some(parse_value(&mut args, "--threads")?),
                "--temperature" => {
                    parsed.temperature = parse_value(&mut args, "--temperature")?;
                }
                "--metadata-only" => parsed.metadata_only = true,
                "--verbose-llama" => parsed.verbose_llama = true,
                "--flash-attn" => {
                    parsed.flash_attn = parse_flash_attention(&next_value(&mut args, "--flash-attn")?)?;
                }
                "--ngram-simple" => {
                    parsed.speculative = SpeculativeMode::NgramSimple { n_max: 4, n_min: 1 };
                }
                "--mtp" => {
                    parsed.speculative = SpeculativeMode::Mtp { n_max: 4 };
                }
                "--spec-probe" => parsed.spec_probe = true,
                "--help" | "-h" => return Err(usage()),
                other => return Err(format!("nieznany argument: {other}\n\n{}", usage())),
            }
        }

        if !parsed.spec_probe && parsed.model.as_os_str().is_empty() {
            return Err(format!("brak --model\n\n{}", usage()));
        }
        Ok(parsed)
    }
}

fn next_value(args: &mut impl Iterator<Item = String>, name: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("brak wartości dla {name}\n\n{}", usage()))
}

fn parse_value<T: std::str::FromStr>(
    args: &mut impl Iterator<Item = String>,
    name: &str,
) -> Result<T, String> {
    next_value(args, name)?
        .parse()
        .map_err(|_| format!("niepoprawna wartość dla {name}\n\n{}", usage()))
}

fn parse_flash_attention(value: &str) -> Result<FlashAttentionMode, String> {
    match value {
        "auto" => Ok(FlashAttentionMode::Auto),
        "off" | "disabled" => Ok(FlashAttentionMode::Off),
        "on" | "enabled" => Ok(FlashAttentionMode::On),
        _ => Err(format!("nieprawidłowe --flash-attn: {value}, użyj auto|off|on")),
    }
}

fn fmt_option(value: Option<u64>) -> String {
    value
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn usage() -> String {
    "Użycie: llama_smoke --model <plik.gguf> [--metadata-only] [--max-tokens N] [--ngram-simple] [--mtp] [--verbose-llama] [--flash-attn auto|off|on]".to_string()
}

// =============================================================================
// Plik: llama_engine_smoke.rs
// Opis: Smoke-test silnika continuous batching (LlamaEngine). Wysyła N równoległych
//       zapytań na jeden model/kontekst z wieloma slotami i odbiera streamy
//       współbieżnie; dowodzi że wolny konsument nie blokuje pozostałych.
// Przykład: cargo run --release --example llama_engine_smoke --features llama -- \
//           --model model.gguf --gpu-layers 99 --requests 8 --max-tokens 80
// =============================================================================

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tentaflow_wrappers::llama::{silence_llama_logs, FlashAttentionMode};
use tentaflow_wrappers::llama_engine::{
    EngineConfig, FinishReason, GenRequest, LlamaEngine, SamplingParams, SpeculativeMode,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse()?;

    if !args.verbose_llama {
        silence_llama_logs();
    }

    // Scenariusze regresji z review (CR-001) — uruchamiane zamiast zwykłego biegu:
    //  --drop-mid       (a) konsument porzuca Receiver w połowie → slot zwolniony,
    //                       inflight wraca do 0;
    //  --silent-consumer(b) konsument nigdy nie czyta i nie dropuje → po
    //                       stream_stall_timeout slot zwolniony z Error, inne działają;
    //  --queue-overflow (c) #requestów > #slotów → wszystkie się kolejkują i kończą,
    //                       inflight wraca do 0 (brak wycieku).
    if args.drop_mid || args.silent_consumer || args.queue_overflow {
        return run_regression_scenarios(&args);
    }

    let config = EngineConfig {
        n_seq_max: args.seq_max,
        ctx_per_seq: args.ctx_per_seq,
        n_batch: args.n_batch,
        n_ubatch: args.n_ubatch,
        n_gpu_layers: args.gpu_layers,
        threads: args.threads,
        flash_attn: args.flash_attn,
        kv_unified: false,
        n_rs_seq: 0,
        speculative: args.speculative,
        ..EngineConfig::default()
    };

    println!("ładowanie modelu: {}", args.model.display());
    println!(
        "config: n_seq_max={} ctx_per_seq={} n_batch={} n_ubatch={} gpu_layers={} speculative={:?}",
        config.n_seq_max,
        config.ctx_per_seq,
        config.n_batch,
        config.n_ubatch,
        config.n_gpu_layers,
        config.speculative
    );

    let load_start = Instant::now();
    let engine = Arc::new(LlamaEngine::load(&args.model, config)?);
    println!("model załadowany w {:.2}s", load_start.elapsed().as_secs_f64());

    let prompts = match &args.prompt {
        Some(p) => vec![p.clone(); args.requests],
        None => build_prompts(args.requests),
    };
    let slow_index: Option<usize> = if args.slow_consumer { Some(0) } else { None };

    let run_start = Instant::now();
    let mut handles = Vec::with_capacity(prompts.len());

    for (i, prompt) in prompts.into_iter().enumerate() {
        let request = GenRequest {
            prompt,
            system_prompt: None,
            sampling: SamplingParams {
                temperature: 0.7,
                top_p: 0.9,
                top_k: 40,
                repeat_penalty: 1.1,
                seed: 1000 + i as u32,
            },
            max_tokens: args.max_tokens,
            stop_sequences: vec!["</s>".to_string()],
        };

        let stream = engine.submit(request)?;
        let is_slow = slow_index == Some(i);

        let handle = std::thread::Builder::new()
            .name(format!("consumer-{i}"))
            .spawn(move || {
                let started = Instant::now();
                let mut text = String::new();
                let mut tokens = 0_u32;
                let mut finish = FinishReason::Error("brak finału".to_string());
                let mut first_token_at: Option<Duration> = None;

                while let Some(item) = stream.recv() {
                    if is_slow {
                        // Celowo wolny konsument: śpi przy każdym tokenie, by
                        // wypełnić swój kanał i sprawdzić, że scheduler nie blokuje
                        // pozostałych slotów.
                        std::thread::sleep(Duration::from_millis(120));
                    }
                    if !item.text.is_empty() {
                        if first_token_at.is_none() {
                            first_token_at = Some(started.elapsed());
                        }
                        text.push_str(&item.text);
                    }
                    if item.is_final {
                        // Liczba REALNYCH tokenów modelu pochodzi z tokena finalnego.
                        // Przy speculative jeden fragment tekstu może nieść wiele
                        // tokenów, więc liczenie fragmentów zaniżałoby tok/s.
                        tokens = item.generated_tokens;
                        if let Some(reason) = item.finish_reason {
                            finish = reason;
                        }
                        break;
                    }
                }

                ConsumerResult {
                    index: i,
                    is_slow,
                    tokens,
                    text,
                    finish,
                    elapsed: started.elapsed(),
                    first_token_at,
                }
            })?;
        handles.push(handle);
    }

    let mut results: Vec<ConsumerResult> = Vec::with_capacity(handles.len());
    for handle in handles {
        results.push(handle.join().map_err(|_| "wątek konsumenta panikował")?);
    }
    results.sort_by_key(|r| r.index);

    let total_elapsed = run_start.elapsed();
    let total_tokens: u32 = results.iter().map(|r| r.tokens).sum();

    println!("\n=== wyniki per-request ===");
    let mut all_finished = true;
    for r in &results {
        let ok = matches!(
            r.finish,
            FinishReason::EndOfText | FinishReason::MaxTokens | FinishReason::StopSequence(_)
        );
        if !ok {
            all_finished = false;
        }
        let preview: String = r.text.chars().take(70).collect();
        let preview = preview.replace('\n', " ");
        // tok/s tej sekwencji liczony od pierwszego tokena do końca (czyste tempo
        // generacji bez czasu prefillu/TTFT), żeby porównanie baseline vs MTP
        // odzwierciedlało realne przyspieszenie dekodowania.
        let gen_secs = match r.first_token_at {
            Some(ttft) => (r.elapsed.saturating_sub(ttft)).as_secs_f64(),
            None => r.elapsed.as_secs_f64(),
        };
        let tok_s = if gen_secs > 1e-9 && r.tokens > 1 {
            (r.tokens.saturating_sub(1)) as f64 / gen_secs
        } else {
            0.0
        };
        println!(
            "[#{:<2}{}] tokens={:<4} {:.2}s ttft={} tok/s={:>6.1} stop={:?}\n      \"{}\"",
            r.index,
            if r.is_slow { " SLOW" } else { "" },
            r.tokens,
            r.elapsed.as_secs_f64(),
            r.first_token_at
                .map(|d| format!("{:.2}s", d.as_secs_f64()))
                .unwrap_or_else(|| "-".to_string()),
            tok_s,
            r.finish,
            preview,
        );
    }

    println!("\n=== podsumowanie ===");
    println!("requestów: {}", results.len());
    println!("łączny czas: {:.2}s", total_elapsed.as_secs_f64());
    println!("łączne tokeny: {total_tokens}");
    println!(
        "łączny throughput: {:.1} tok/s",
        total_tokens as f64 / total_elapsed.as_secs_f64().max(1e-9)
    );
    println!(
        "wszystkie zakończone poprawnie: {}",
        if all_finished { "TAK" } else { "NIE" }
    );

    if args.slow_consumer {
        // Dowód anty-hangu: szybkie requesty współbieżne ze slotem wolnego
        // konsumenta muszą skończyć przed nim — czyli scheduler ich nie blokował.
        // Przy #requestów > #slotów porównujemy TYLKO pierwszą falę (requesty z
        // małym ttft, uruchomione razem z wolnym), bo kolejne fale czekają na
        // zwolnienie slotu i naturalnie kończą później (kolejkowanie, nie blokada).
        if let Some(slow) = results.iter().find(|r| r.is_slow) {
            let slow_ttft = slow.first_token_at.unwrap_or_default();
            // Pierwsza fala = szybkie requesty, które ruszyły mniej-więcej razem z
            // wolnym (ttft nie później niż ttft wolnego + margines 1s).
            let wave_margin = slow_ttft + Duration::from_secs(1);
            let fast_first_wave: Vec<&ConsumerResult> = results
                .iter()
                .filter(|r| !r.is_slow && r.first_token_at.unwrap_or_default() <= wave_margin)
                .collect();
            let fast_max = fast_first_wave
                .iter()
                .map(|r| r.elapsed)
                .max()
                .unwrap_or_default();
            println!(
                "\nslow-consumer: wolny={:.2}s, najwolniejszy szybki (1. fala, {} szt.)={:.2}s",
                slow.elapsed.as_secs_f64(),
                fast_first_wave.len(),
                fast_max.as_secs_f64()
            );
            if fast_max < slow.elapsed {
                println!("anty-hang POTWIERDZONY: szybkie requesty NIE były blokowane przez wolnego konsumenta");
            } else {
                println!("UWAGA: szybkie requesty nie skończyły przed wolnym — sprawdź konfigurację");
            }
        }
    }

    if !all_finished {
        return Err("nie wszystkie requesty zakończyły się poprawnie".into());
    }

    Ok(())
}

// Buduje konfigurację silnika z argumentów; pozwala nadpisać stall-timeout dla
// scenariusza (b), gdzie ustawiamy krótki próg (np. 3s) żeby test był szybki.
fn build_config(args: &Args, stall_timeout: Option<Duration>) -> EngineConfig {
    EngineConfig {
        n_seq_max: args.seq_max,
        ctx_per_seq: args.ctx_per_seq,
        n_batch: args.n_batch,
        n_ubatch: args.n_ubatch,
        n_gpu_layers: args.gpu_layers,
        threads: args.threads,
        flash_attn: args.flash_attn,
        kv_unified: false,
        n_rs_seq: 0,
        speculative: args.speculative,
        stream_stall_timeout: stall_timeout.unwrap_or(Duration::from_secs(60)),
        ..EngineConfig::default()
    }
}

// Czeka aż inflight spadnie do 0 albo upłynie deadline; zwraca finalny inflight.
fn wait_inflight_zero(engine: &LlamaEngine, deadline: Duration) -> usize {
    let start = Instant::now();
    while engine.inflight() != 0 && start.elapsed() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    engine.inflight()
}

fn run_regression_scenarios(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    if args.drop_mid {
        scenario_drop_mid(args)?;
    }
    if args.silent_consumer {
        scenario_silent_consumer(args)?;
    }
    if args.queue_overflow {
        scenario_queue_overflow(args)?;
    }
    Ok(())
}

// (a) Konsument porzuca Receiver w połowie strumienia. Slot musi się zwolnić
// (flush_pending wykrywa Closed sink), a inflight wrócić do 0 bez wycieku.
fn scenario_drop_mid(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n=== scenariusz (a): konsument dropuje Receiver w połowie ===");
    let engine = LlamaEngine::load(&args.model, build_config(args, None))?;
    let request = GenRequest {
        prompt: "Opisz krótko historię komputerów osobistych.".to_string(),
        system_prompt: None,
        sampling: SamplingParams::default(),
        max_tokens: 400,
        stop_sequences: vec!["</s>".to_string()],
    };
    let stream = engine.submit(request)?;
    let mut received = 0_u32;
    while let Some(token) = stream.recv() {
        if !token.text.is_empty() {
            received += 1;
        }
        if received >= 5 {
            // Porzucamy strumień (Receiver) w połowie — drop zamyka kanał.
            break;
        }
    }
    drop(stream);
    println!("odebrano {received} fragmentów, porzucono Receiver");

    let inflight = wait_inflight_zero(&engine, Duration::from_secs(30));
    println!("inflight po porzuceniu: {inflight}");
    if inflight != 0 {
        return Err(format!("(a) FAIL: inflight={inflight} (oczekiwano 0 — wyciek slotu)").into());
    }
    println!("(a) OK: slot zwolniony, inflight=0");
    Ok(())
}

// (b) Konsument NIGDY nie czyta i nie dropuje. Po stream_stall_timeout (krótkim,
// 3s) slot musi zostać zwolniony z FinishReason::Error, a RÓWNOLEGŁY normalny
// request musi zakończyć się poprawnie (silnik nie zablokowany).
fn scenario_silent_consumer(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n=== scenariusz (b): konsument żywy ale niemy (stall timeout 3s) ===");
    let engine = Arc::new(LlamaEngine::load(
        &args.model,
        build_config(args, Some(Duration::from_secs(3))),
    )?);

    // Niemy konsument: trzymamy Receiver żywy (nie dropujemy), ale NIGDY z niego
    // nie czytamy — kanał szybko się zapełnia i pozostaje pełny w nieskończoność.
    let silent_request = GenRequest {
        prompt: "Wymień dziesięć języków programowania z krótkim opisem każdego.".to_string(),
        system_prompt: None,
        sampling: SamplingParams::default(),
        max_tokens: 600,
        stop_sequences: vec!["</s>".to_string()],
    };
    let silent_stream = engine.submit(silent_request)?;

    // Równoległy normalny konsument w osobnym wątku — musi się zakończyć poprawnie.
    let engine_for_fast = Arc::clone(&engine);
    let fast = std::thread::spawn(move || -> (bool, u32) {
        let request = GenRequest {
            prompt: "Napisz dwa zdania o językach programowania.".to_string(),
            system_prompt: None,
            sampling: SamplingParams::default(),
            max_tokens: 60,
            stop_sequences: vec!["</s>".to_string()],
        };
        let Ok(stream) = engine_for_fast.submit(request) else {
            return (false, 0);
        };
        let mut tokens = 0_u32;
        let mut ok = false;
        while let Some(token) = stream.recv() {
            if token.is_final {
                tokens = token.generated_tokens;
                ok = matches!(
                    token.finish_reason,
                    Some(FinishReason::EndOfText | FinishReason::MaxTokens | FinishReason::StopSequence(_))
                );
                break;
            }
        }
        (ok, tokens)
    });

    let (fast_ok, fast_tokens) = fast.join().map_err(|_| "wątek szybkiego konsumenta panikował")?;
    println!("równoległy normalny request: ok={fast_ok} tokens={fast_tokens}");

    // Po timeout (3s) niemy slot powinien zostać zwolniony — inflight wraca do 0
    // gdy go odczytamy (deferred-finish odda Error gdy znów czytamy) ALBO slot
    // zostanie wymuszenie zwolniony. Sprawdzamy zarówno inflight, jak i to, że
    // odczytany finał niemego strumienia (po wznowieniu czytania) niesie Error.
    let inflight = wait_inflight_zero(&engine, Duration::from_secs(20));
    println!("inflight po stall timeout: {inflight}");

    // Wznawiamy czytanie niemego strumienia — drenujemy zaległości; jeśli slot
    // został zwolniony przez stall, finał (jeśli dotarł) jest Error, a kanał się
    // zamyka. Akceptujemy też brak finału (slot zwolniony, sink dropowany).
    let mut silent_finish: Option<FinishReason> = None;
    while let Some(token) = silent_stream.recv() {
        if token.is_final {
            silent_finish = token.finish_reason;
            break;
        }
    }
    println!("niemy strumień finish={silent_finish:?}");

    if !fast_ok {
        return Err("(b) FAIL: równoległy normalny request NIE zakończył się poprawnie (silnik zablokowany)".into());
    }
    if inflight != 0 {
        return Err(format!("(b) FAIL: inflight={inflight} po stall timeout (oczekiwano 0)").into());
    }
    match silent_finish {
        Some(FinishReason::Error(_)) | None => {
            println!("(b) OK: niemy slot zwolniony przez stall timeout, inne sloty działają");
        }
        other => {
            return Err(format!("(b) FAIL: niemy slot zakończył się {other:?}, oczekiwano Error/None").into());
        }
    }
    Ok(())
}

// (c) queue_capacity > n_seq_max: 12 requestów na (domyślnie) 4 sloty. Wszystkie
// muszą się zakolejkować i zakończyć poprawnie, inflight wraca do 0 (brak wycieku).
fn scenario_queue_overflow(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let total = 12_usize;
    println!(
        "\n=== scenariusz (c): {total} requestów na {} slotów (kolejkowanie) ===",
        args.seq_max
    );
    let engine = Arc::new(LlamaEngine::load(&args.model, build_config(args, None))?);
    let prompts = build_prompts(total);

    let mut handles = Vec::with_capacity(total);
    for (i, prompt) in prompts.into_iter().enumerate() {
        let engine = Arc::clone(&engine);
        handles.push(std::thread::spawn(move || -> (usize, bool) {
            let request = GenRequest {
                prompt,
                system_prompt: None,
                sampling: SamplingParams {
                    seed: 2000 + i as u32,
                    ..SamplingParams::default()
                },
                max_tokens: 48,
                stop_sequences: vec!["</s>".to_string()],
            };
            let Ok(stream) = engine.submit(request) else {
                return (i, false);
            };
            let mut ok = false;
            while let Some(token) = stream.recv() {
                if token.is_final {
                    ok = matches!(
                        token.finish_reason,
                        Some(FinishReason::EndOfText | FinishReason::MaxTokens | FinishReason::StopSequence(_))
                    );
                    break;
                }
            }
            (i, ok)
        }));
    }

    let mut all_ok = true;
    for h in handles {
        let (i, ok) = h.join().map_err(|_| "wątek konsumenta panikował")?;
        if !ok {
            all_ok = false;
            println!("request #{i} NIE zakończył się poprawnie");
        }
    }

    let inflight = wait_inflight_zero(&engine, Duration::from_secs(60));
    println!("wszystkie poprawne: {all_ok}, inflight końcowy: {inflight}");
    if !all_ok {
        return Err("(c) FAIL: nie wszystkie requesty zakończyły się poprawnie".into());
    }
    if inflight != 0 {
        return Err(format!("(c) FAIL: inflight={inflight} (oczekiwano 0 — wyciek slotu)").into());
    }
    println!("(c) OK: wszystkie {total} requestów zakończone, inflight=0");
    Ok(())
}

struct ConsumerResult {
    index: usize,
    is_slow: bool,
    tokens: u32,
    text: String,
    finish: FinishReason,
    elapsed: Duration,
    first_token_at: Option<Duration>,
}

fn build_prompts(count: usize) -> Vec<String> {
    let base = [
        "Wymień trzy zalety języka Rust.",
        "Opisz krótko czym jest fotosynteza.",
        "Napisz dwa zdania o historii Internetu.",
        "Wyjaśnij prosto pojęcie rekurencji.",
        "Podaj przepis na klasyczną herbatę.",
        "Czym różni się stos od kolejki?",
        "Opowiedz krótko o Układzie Słonecznym.",
        "Wyjaśnij, czym jest kompilator.",
        "Podaj trzy ciekawostki o oceanach.",
        "Opisz w skrócie cykl wody w przyrodzie.",
        "Czym jest sztuczna inteligencja?",
        "Wytłumacz pojęcie wskaźnika w programowaniu.",
    ];
    (0..count)
        .map(|i| base[i % base.len()].to_string())
        .collect()
}

struct Args {
    model: PathBuf,
    requests: usize,
    max_tokens: u32,
    seq_max: u32,
    ctx_per_seq: u32,
    n_batch: u32,
    n_ubatch: u32,
    gpu_layers: u32,
    threads: Option<u32>,
    flash_attn: FlashAttentionMode,
    slow_consumer: bool,
    verbose_llama: bool,
    speculative: SpeculativeMode,
    prompt: Option<String>,
    drop_mid: bool,
    silent_consumer: bool,
    queue_overflow: bool,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut args = std::env::args().skip(1);
        let mut parsed = Self {
            model: PathBuf::new(),
            requests: 8,
            max_tokens: 80,
            seq_max: 4,
            ctx_per_seq: 2048,
            n_batch: 2048,
            n_ubatch: 512,
            gpu_layers: 0,
            threads: None,
            flash_attn: FlashAttentionMode::Auto,
            slow_consumer: false,
            verbose_llama: false,
            speculative: SpeculativeMode::Off,
            prompt: None,
            drop_mid: false,
            silent_consumer: false,
            queue_overflow: false,
        };

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--model" => parsed.model = PathBuf::from(next_value(&mut args, "--model")?),
                "--requests" => parsed.requests = parse_value(&mut args, "--requests")?,
                "--max-tokens" => parsed.max_tokens = parse_value(&mut args, "--max-tokens")?,
                "--seq-max" => parsed.seq_max = parse_value(&mut args, "--seq-max")?,
                "--ctx-per-seq" => parsed.ctx_per_seq = parse_value(&mut args, "--ctx-per-seq")?,
                "--n-batch" => parsed.n_batch = parse_value(&mut args, "--n-batch")?,
                "--n-ubatch" => parsed.n_ubatch = parse_value(&mut args, "--n-ubatch")?,
                "--gpu-layers" => parsed.gpu_layers = parse_value(&mut args, "--gpu-layers")?,
                "--threads" => parsed.threads = Some(parse_value(&mut args, "--threads")?),
                "--flash-attn" => {
                    parsed.flash_attn = parse_flash_attention(&next_value(&mut args, "--flash-attn")?)?;
                }
                "--slow-consumer" => parsed.slow_consumer = true,
                "--verbose-llama" => parsed.verbose_llama = true,
                "--speculative" => {
                    parsed.speculative = parse_speculative(&next_value(&mut args, "--speculative")?)?;
                }
                "--prompt" => parsed.prompt = Some(next_value(&mut args, "--prompt")?),
                "--drop-mid" => parsed.drop_mid = true,
                "--silent-consumer" => parsed.silent_consumer = true,
                "--queue-overflow" => parsed.queue_overflow = true,
                "--help" | "-h" => return Err(usage()),
                other => return Err(format!("nieznany argument: {other}\n\n{}", usage())),
            }
        }

        if parsed.model.as_os_str().is_empty() {
            return Err(format!("brak --model\n\n{}", usage()));
        }
        if parsed.requests == 0 {
            return Err("--requests musi być > 0".to_string());
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

fn parse_speculative(value: &str) -> Result<SpeculativeMode, String> {
    match value {
        "off" | "none" => Ok(SpeculativeMode::Off),
        "ngram" => Ok(SpeculativeMode::NgramSimple { n_max: 4, n_min: 1 }),
        "mtp" => Ok(SpeculativeMode::Mtp { n_max: 4 }),
        _ => Err(format!("nieprawidłowe --speculative: {value}, użyj off|ngram|mtp")),
    }
}

fn parse_flash_attention(value: &str) -> Result<FlashAttentionMode, String> {
    match value {
        "auto" => Ok(FlashAttentionMode::Auto),
        "off" | "disabled" => Ok(FlashAttentionMode::Off),
        "on" | "enabled" => Ok(FlashAttentionMode::On),
        _ => Err(format!("nieprawidłowe --flash-attn: {value}, użyj auto|off|on")),
    }
}

fn usage() -> String {
    "Użycie: llama_engine_smoke --model <plik.gguf> [--requests N] [--max-tokens N] \
     [--seq-max N] [--ctx-per-seq N] [--gpu-layers N] [--slow-consumer] [--flash-attn auto|off|on] \
     [--speculative off|ngram|mtp] [--drop-mid] [--silent-consumer] [--queue-overflow]"
        .to_string()
}

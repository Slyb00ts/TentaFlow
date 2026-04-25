// =============================================================================
// Plik: examples/bundle_smoke.rs
// Opis: Smoke-test pojedynczego silnika z manifestu services bez uruchamiania
//       calego dashboardu. Wywoluje deploy::python_venv::deploy_with_logs()
//       bezposrednio, czeka az HTTP serwer odpowie, robi minimalny request
//       (chat completion / health), zapisuje raport do pliku, ubija proces.
//
//       Uzycie:
//         cargo run --example bundle_smoke --features dashboard-api -- \
//             <engine_id> [<MODEL_REPO>]
//
//       MODEL_REPO przekazywany jako env `MODEL` do bundla; dla TTS/STT moze
//       byc pominiety (silniki maja domyslne modele wbudowane przez biblioteki).
//
//       Cache + HF home na /mnt/d/tentaflow-bundle-tests (wymagane export
//       TENTAFLOW_CACHE_DIR + HF_HOME przez wrapper skrypt). HF_TOKEN tez
//       z env (z ~/.bashrc albo z linii skryptu).
// =============================================================================

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use tentaflow_core::deploy::python_venv::{deploy_with_logs, NativeDeployRequest};

const HEALTH_TIMEOUT_SECS: u64 = 600; // 10 min na ladowanie modelu
// sglang/vllm/trtllm robia JIT compile triton kerneli przy pierwszym chat
// request — moze trwac kilka min. Drugi request bedzie szybki (cached).
const SMOKE_REQUEST_TIMEOUT_SECS: u64 = 600;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let engine = args
        .next()
        .ok_or_else(|| anyhow!("usage: bundle_smoke <engine_id> [<MODEL_REPO>]"))?;
    let model = args.next();

    eprintln!("=== bundle_smoke: {} ===", engine);
    eprintln!("cache_root = {:?}", std::env::var("TENTAFLOW_CACHE_DIR").ok());
    eprintln!("hf_home    = {:?}", std::env::var("HF_HOME").ok());
    eprintln!("hf_token   = {}", if std::env::var("HF_TOKEN").is_ok() { "present" } else { "MISSING" });
    eprintln!("model      = {:?}", model);
    eprintln!();

    let mut env: HashMap<String, String> = HashMap::new();
    if let Some(m) = &model {
        env.insert("MODEL".to_string(), m.clone());
        // Setujemy tez w naszym procesie zeby smoke_llm widzial nazwe modelu
        // przy budowaniu requestu (vllm /v1/chat/completions wymaga model name).
        std::env::set_var("MODEL", m);
    }
    // Coqui XTTS v2 wymaga jawnej akceptacji licencji CPML — bez tego
    // `from TTS.api import TTS; TTS(...)` rzuca exception przy pierwszym
    // call'u i serwer pada zanim odpowie na health probe.
    if engine == "xtts" {
        env.insert("COQUI_TOS_AGREED".to_string(), "1".to_string());
    }
    // Domyslny UV_HTTP_TIMEOUT=30s nie wystarcza dla nvidia-cublas (>1 GB)
    // i innych ciezkich wheels CUDA. Bezpieczne 10 min na pobranie.
    std::env::set_var("UV_HTTP_TIMEOUT", "600");
    // sglang 0.5.10+ zalezy od flash-attn-4 ktory jest pre-release (>=4.0.0b4).
    // Bez UV_PRERELEASE=allow uv resolver odrzuca i deploy pada na install
    // glownego pakietu. Allow obejmuje wszystkie silniki — pre-release deps
    // zdarzaja sie regularnie w nowoczesnym ML stacku (vllm, trtllm itd.).
    std::env::set_var("UV_PRERELEASE", "allow");
    // tensorrt-llm wewnetrznie ladowal UCX (InfiniBand transport) i probowal
    // alokowac registered memory; bez `ulimit -l unlimited` proba `ibv_reg_mr`
    // pada, a serwer zostaje w petli logow bez odpowiedzi. Wymuszamy TCP-only
    // transport — szkodzi tylko gdy ktos REALNIE chce IB, czego w smoke tescie
    // RTX 4090 nie testujemy.
    if engine == "tensorrt-llm" {
        env.insert("UCX_TLS".to_string(), "tcp".to_string());
        env.insert("OMPI_MCA_opal_cuda_support".to_string(), "false".to_string());
        // Wymus loopback tylko — bez tego trtllm/MPI prubuje dialowac na docker
        // bridge (172.17.0.1) i ginie z "Connection timed out" na port 1027.
        env.insert("OMPI_MCA_btl_tcp_if_include".to_string(), "lo".to_string());
        env.insert("OMPI_MCA_oob_tcp_if_include".to_string(), "lo".to_string());
        // Single-GPU bez MPI runtime — trtllm wymusi sam tp=1 ale to ucisza
        // MPI socket discovery przy starcie.
        env.insert("OMPI_MCA_btl".to_string(), "self,vader,tcp".to_string());
    }
    // sglang specific env (TVM_FFI_GPU_BACKEND=cuda) jest teraz w bundle.toml
    // [launch.env] — domyslny dla wszystkich deployow przez GUI rowniez.
    if let Ok(token) = std::env::var("HF_TOKEN") {
        env.insert("HF_TOKEN".to_string(), token);
    }
    // Probowac wymusic mniejsze GPU mem footprint zeby kilka silnikow pasowalo
    // pod jeden 24 GB karte przy iteracji testow.
    env.entry("GPU_MEMORY_UTILIZATION".to_string())
        .or_insert_with(|| "0.6".to_string());
    env.entry("MAX_MODEL_LEN".to_string())
        .or_insert_with(|| "2048".to_string());

    let req = NativeDeployRequest {
        engine: engine.clone(),
        instance_name: Some(format!("smoke-{}", engine)),
        env,
    };

    let log_sink: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|line: &str| {
        eprintln!("[deploy] {}", line);
    });

    let started_at = Instant::now();
    // deploy_with_logs jest blokujace (uruchamia uv/git/python sync). W tokio
    // runtime trzeba owinac w spawn_blocking — inaczej panicuje przy dropie
    // wewnetrznych blocking runtime'ow.
    let req_for_deploy = req.clone();
    let log_sink_clone = log_sink.clone();
    let deploy_handle = tokio::task::spawn_blocking(move || {
        deploy_with_logs(&req_for_deploy, &log_sink_clone)
    });
    let mut running = match deploy_handle.await.unwrap_or_else(|e| Err(anyhow!("join error: {e}"))) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("\n!!! DEPLOY FAILED for {} after {:.1}s: {:#}", engine, started_at.elapsed().as_secs_f32(), e);
            std::process::exit(2);
        }
    };

    eprintln!(
        "\n+++ deploy() returned ok in {:.1}s, internal_port={}",
        started_at.elapsed().as_secs_f32(),
        running.internal_port
    );

    // Subprocess silnika (uvicorn / vllm / sglang / trtllm) ma piped stdout +
    // stderr (Stdio::piped() w spawn_engine). Bez aktywnego czytania pipe
    // moze sie zapelnic i proces zawiesi sie cicho — a my nie zobaczymy
    // dlaczego nie odpowiada na health probe. Spawnujemy 2 watki ktore
    // forwarduja kazda linie do naszego stderr z prefiksem [engine].
    use std::io::{BufRead, BufReader};
    if let Some(out) = running.child.stdout.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().flatten() {
                eprintln!("[engine-out] {}", line);
            }
        });
    }
    if let Some(err) = running.child.stderr.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines().flatten() {
                eprintln!("[engine-err] {}", line);
            }
        });
    }

    let port = running.internal_port;
    let base_url = format!("http://127.0.0.1:{port}");

    let healthy = match wait_for_health(&base_url, HEALTH_TIMEOUT_SECS).await {
        Ok(probe) => {
            eprintln!("+++ health OK po {:.1}s — probe '{}' zwrocil 200", started_at.elapsed().as_secs_f32(), probe);
            true
        }
        Err(e) => {
            eprintln!("!!! health timeout: {:#}", e);
            false
        }
    };

    let smoke_ok = if healthy {
        run_smoke(&base_url, &engine).await
    } else {
        false
    };

    let pid = running.child.id();
    eprintln!("\n--- killing process pid={} (+ children) ---", pid);
    // Engine'y typu vllm spawn'uja child process EngineCore poza grupa parenta.
    // Najpewniejsza metoda na czystki: pkill -P (children parenta) + kill parent.
    let _ = std::process::Command::new("pkill")
        .args(["-9", "-P", &pid.to_string()])
        .status();
    let _ = running.child.kill();
    let _ = running.child.wait();
    // Drugi sweep — dziadek-dzieci (np. vllm.engine spawnowany przez api_server,
    // ktorzy biegna z `--port <N>` w argv). Pattern musi zawierac caly fragment
    // bo pkill nie traktuje "--" jako separatora.
    let port_pattern = format!("port {}", running.internal_port);
    let _ = std::process::Command::new("pkill")
        .args(["-9", "-f", &port_pattern])
        .status();

    eprintln!(
        "\n=== SUMMARY {}: deploy_ok=true health_ok={} smoke_ok={} elapsed={:.1}s ===",
        engine, healthy, smoke_ok, started_at.elapsed().as_secs_f32()
    );

    if !healthy || !smoke_ok {
        std::process::exit(1);
    }
    Ok(())
}

async fn wait_for_health(base_url: &str, max_secs: u64) -> Result<&'static str> {
    let probes = ["/v1/models", "/health", "/healthz", "/", "/docs"];
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let deadline = Instant::now() + Duration::from_secs(max_secs);
    let mut last_err = String::new();
    while Instant::now() < deadline {
        for p in probes {
            let url = format!("{base_url}{p}");
            match client.get(&url).send().await {
                Ok(resp) if resp.status().is_success() => return Ok(p),
                Ok(resp) => last_err = format!("{} -> HTTP {}", p, resp.status()),
                Err(e) => last_err = format!("{} -> {}", p, e),
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("health timeout, last={}", last_err))
}

async fn run_smoke(base_url: &str, engine: &str) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(SMOKE_REQUEST_TIMEOUT_SECS))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("!!! cannot build http client: {}", e);
            return false;
        }
    };

    // Detekcja typu API po zachowaniu /v1/models. LLM bundli (sglang/vllm)
    // zwraca OpenAI-compatible — wol /v1/chat/completions. STT/TTS pominiete
    // (kazdy ma wlasny custom endpoint, smoke ograniczony do health check).
    let category = engine_category(engine);
    eprintln!("--- smoke test ({} / {}) ---", engine, category);

    match category {
        "llm" => smoke_llm(&client, base_url).await,
        "stt" | "tts" => {
            eprintln!("(category={category}: smoke ograniczony do health check, skip request body)");
            true
        }
        _ => {
            eprintln!("(category={category}: brak smoke testu)");
            true
        }
    }
}

async fn smoke_llm(client: &reqwest::Client, base_url: &str) -> bool {
    let url = format!("{base_url}/v1/chat/completions");
    let payload = serde_json::json!({
        "model": std::env::var("MODEL").unwrap_or_else(|_| "default".to_string()),
        "messages": [{"role": "user", "content": "Reply with exactly one word: OK"}],
        "max_tokens": 16,
        "temperature": 0.0,
        "stream": false,
    });

    eprintln!("POST {} ...", url);
    let started = Instant::now();
    let resp = match client.post(&url).json(&payload).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("!!! request error: {}", e);
            return false;
        }
    };
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    eprintln!("--> HTTP {} in {:.2}s", status, started.elapsed().as_secs_f32());
    if !status.is_success() {
        eprintln!("body: {}", &body.chars().take(500).collect::<String>());
        return false;
    }
    eprintln!("body (first 400 chars): {}", &body.chars().take(400).collect::<String>());
    true
}

fn engine_category(engine: &str) -> &'static str {
    match engine {
        "llama-cpp" | "sglang" | "vllm" | "vllm-metal" | "tensorrt-llm" | "ollama" | "mlx" => "llm",
        "whisper" | "parakeet" | "qwen-asr" => "stt",
        "sherpa-onnx" | "voxcpm" | "xtts" => "tts",
        _ => "unknown",
    }
}

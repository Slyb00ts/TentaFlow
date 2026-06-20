// =============================================================================
// Plik: launch_dialect.rs
// Opis: Per-engine "dialekt" komendy startowej silnika. Jedno zrodlo prawdy dla
//       argumentow CLI (vLLM / sglang / llama.cpp / MLX) oraz dla podgladu
//       finalnej komendy w wizardzie. Generyczne ustawienia (kontekst, seqs,
//       gpu-mem, TP/PP) mapowane sa na natywne flagi konkretnego silnika.
// =============================================================================

use crate::deploy::vram_calculator::{
    build_llamacpp_args_string, build_vllm_args_string, ModelSpec, VramEstimateInput,
};

/// Metoda deployu — rozstrzyga baze komendy (docker uruchamia `vllm serve`,
/// native odpala `python -m vllm.entrypoints.openai.api_server`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployMethod {
    Docker,
    Native,
}

impl DeployMethod {
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(|s| s.to_lowercase()) {
            Some(ref s) if s == "native" || s == "python-bundle" || s == "bundle" => {
                DeployMethod::Native
            }
            _ => DeployMethod::Docker,
        }
    }
}

/// Rodzina CLI silnika. Rozne silniki maja inne nazwy flag dla tych samych
/// pojec (kontekst, gpu-mem), wiec dialekt wybieramy raz i kierujemy przez
/// niego zarowno generacje argumentow, jak i podglad komendy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Vllm,
    Sglang,
    LlamaCpp,
    Mlx,
    /// Silniki bez strojonych argumentow (tts/stt/vision/tools) — komenda jest
    /// deterministyczna z entrypointu/bundle, podglad pokazuje tylko baze.
    Generic,
}

/// Mapuje `engine_id` na dialekt. `vllm-spark`/`vllm-metal`/`qwen3-vl`/`granite*`
/// dziela CLI vLLM (uruchamiaja vllm serve), `sglang` ma wlasny, GGUF idzie na
/// llama.cpp, `mlx*` na MLX. Nieznane LLM-y traktujemy jak vLLM (najszerszy
/// wspolny mianownik OpenAI-compatible), reszta to Generic.
pub fn dialect_for(engine_id: &str) -> Dialect {
    let id = engine_id.to_lowercase().replace('.', "-");
    match id.as_str() {
        "sglang" => Dialect::Sglang,
        "llama-cpp" | "llamacpp" => Dialect::LlamaCpp,
        "mlx" | "mlx-lm" | "mlx-vlm" | "qwen3-vl-mlx" => Dialect::Mlx,
        "vllm" | "vllm-spark" | "vllm-metal" | "qwen3-vl" => Dialect::Vllm,
        other if other.starts_with("granite") => Dialect::Vllm,
        _ => Dialect::Generic,
    }
}

/// Buduje argumenty CLI silnika w jego natywnym dialekcie z dopasowanej
/// konfiguracji. Dla vLLM/llama.cpp reuzywa istniejacych builderow; dla sglang
/// tlumaczy generyczne ustawienia na flagi sglang.
pub fn build_args(dialect: Dialect, spec: &ModelSpec, input: &VramEstimateInput) -> Vec<String> {
    match dialect {
        Dialect::Vllm => split_args(&build_vllm_args_string(spec, input)),
        Dialect::Sglang => build_sglang_args(spec, input),
        Dialect::LlamaCpp => split_args(&build_llamacpp_args_string(spec, input)),
        Dialect::Mlx => vec![
            "--max-tokens".into(),
            input.max_model_len.to_string(),
            "--max-kv-size".into(),
            (input.max_num_seqs.max(1) * input.max_model_len).to_string(),
        ],
        Dialect::Generic => Vec::new(),
    }
}

/// Argumenty sglang (`sglang.launch_server`). Nazwy flag rozne od vLLM:
///   --max-model-len            → --context-length
///   --gpu-memory-utilization   → --mem-fraction-static
///   --max-num-seqs             → --max-running-requests
///   --max-num-batched-tokens   → --chunked-prefill-size
///   --tensor-parallel-size     → --tp
///   --pipeline-parallel-size   → --pp
/// Radix cache (prefix caching) i chunked prefill sa w sglang domyslnie
/// wlaczone, wiec vLLM-owe `--enable-*` nie maja odpowiednika i sa pomijane.
fn build_sglang_args(spec: &ModelSpec, input: &VramEstimateInput) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();

    parts.push("--trust-remote-code".into());
    parts.push("--mem-fraction-static".into());
    parts.push(format!("{:.2}", input.gpu_memory_utilization));
    parts.push("--context-length".into());
    parts.push(input.max_model_len.to_string());
    parts.push("--max-running-requests".into());
    parts.push(input.max_num_seqs.to_string());
    // chunked-prefill-size = ten sam CAP co vLLM max-num-batched-tokens: pelny
    // kontekst (np. 262144) w jednym kroku schedulera wysadza aktywacje → OOM.
    parts.push("--chunked-prefill-size".into());
    parts.push(input.max_model_len.min(8192).to_string());

    if input.tensor_parallel > 1 {
        parts.push("--tp".into());
        parts.push(input.tensor_parallel.to_string());
    }
    if input.pipeline_parallel > 1 {
        parts.push("--pp".into());
        parts.push(input.pipeline_parallel.to_string());
    }
    if input.kv_cache_dtype != "auto" {
        parts.push("--kv-cache-dtype".into());
        parts.push(input.kv_cache_dtype.clone());
    }
    if let Some(q) = &spec.quantization {
        let q_norm = q.to_lowercase().replace('-', "_");
        match q_norm.as_str() {
            "awq" => parts.extend(["--quantization".into(), "awq".into()]),
            "gptq" => parts.extend(["--quantization".into(), "gptq".into()]),
            "fp8" | "modelopt_fp8" => parts.extend(["--quantization".into(), "fp8".into()]),
            "w8a8" | "w8a16" | "w4a16" | "compressed_tensors_4bit" => {
                parts.extend(["--quantization".into(), "compressed-tensors".into()])
            }
            // nvfp4/fp4/mxfp4: sglang sam wykrywa z config.json (jak vLLM) —
            // wymuszanie flagi odrzuca repo o innym packingu.
            _ => {}
        }
    }
    parts
}

/// Baza komendy (bez argumentow strojonych) z placeholderami env, ktore powloka
/// w entrypoincie/`sh -c` rozwija przy starcie. Dzieki temu edytowalna komenda z
/// wizarda == realna komenda kontenera (te same `$MODEL`/`$PORT`).
fn base_command(dialect: Dialect, method: DeployMethod) -> Vec<String> {
    let s = |v: &str| v.to_string();
    match (dialect, method) {
        (Dialect::Vllm, DeployMethod::Docker) => vec![
            s("vllm"),
            s("serve"),
            s("\"$MODEL\""),
            s("--host"),
            s("0.0.0.0"),
            s("--port"),
            s("\"$PORT\""),
            s("--served-model-name"),
            s("\"$SERVED_MODEL_NAME\""),
        ],
        (Dialect::Vllm, DeployMethod::Native) => vec![
            s("python"),
            s("-m"),
            s("vllm.entrypoints.openai.api_server"),
            s("--model"),
            s("\"$MODEL\""),
            s("--host"),
            s("127.0.0.1"),
            s("--port"),
            s("\"$PORT\""),
            s("--served-model-name"),
            s("\"$SERVED_MODEL_NAME\""),
        ],
        (Dialect::Sglang, method) => {
            let host = if method == DeployMethod::Docker {
                "0.0.0.0"
            } else {
                "127.0.0.1"
            };
            vec![
                s("python3"),
                s("-m"),
                s("sglang.launch_server"),
                s("--model-path"),
                s("\"$MODEL\""),
                s("--host"),
                s(host),
                s("--port"),
                s("\"$PORT\""),
            ]
        }
        (Dialect::LlamaCpp, method) => {
            let host = if method == DeployMethod::Docker {
                "0.0.0.0"
            } else {
                "127.0.0.1"
            };
            vec![
                s("llama-server"),
                s("--model"),
                s("\"$MODEL\""),
                s("--host"),
                s(host),
                s("--port"),
                s("\"$PORT\""),
            ]
        }
        (Dialect::Mlx, _) => vec![
            s("python"),
            s("-m"),
            s("mlx_lm.server"),
            s("--model"),
            s("\"$MODEL\""),
            s("--port"),
            s("\"$PORT\""),
        ],
        (Dialect::Generic, _) => Vec::new(),
    }
}

/// Bezpieczny baseline argumentow dla deployu docker, gdy wizard nie przysle
/// zadnych strojonych argow (tryb manual/raw bez recommendera). Native bierze
/// ten baseline z bundle.toml `[launch] args`; docker nie ma bundle, wiec sieje
/// te same defaulty Rust-side jako POCZATEK argv (dedup last-wins pozwala je
/// nadpisac). Idzie w natywnym dialekcie silnika.
pub fn docker_baseline_args(engine_id: &str) -> Vec<String> {
    let s = |v: &str| v.to_string();
    match dialect_for(engine_id) {
        Dialect::Vllm => vec![
            s("--dtype"),
            s("auto"),
            s("--max-model-len"),
            s("8192"),
            s("--max-num-batched-tokens"),
            s("8192"),
            s("--enable-prefix-caching"),
            s("--enable-chunked-prefill"),
            s("--enable-flashinfer-autotune"),
        ],
        Dialect::Sglang => vec![
            s("--tp"),
            s("1"),
            s("--mem-fraction-static"),
            s("0.85"),
        ],
        // llama.cpp / MLX / Generic — baza/args sa zarzadzane przez entrypoint
        // wzglednie wlasny runner; brak Rust-side baseline.
        _ => Vec::new(),
    }
}

/// Sama baza komendy (bez argumentow) jako string — handler doklei do niej
/// finalny string argumentow (ktory dla vLLM zawiera juz flagi z recipe), zeby
/// podglad `launch_command` byl identyczny z tym, co realnie leci do deployu.
/// Pusty string dla Generic (brak strojonej komendy).
pub fn base_command_string(engine_id: &str, method: DeployMethod) -> String {
    base_command(dialect_for(engine_id), method).join(" ")
}

/// Pelna finalna komenda startowa (baza + argumenty) jako string do podgladu i
/// edycji w wizardzie. Generic (brak bazy) zwraca pusty string — wizard pokaze
/// wtedy komunikat "komenda zarzadzana przez entrypoint".
pub fn build_command_string(
    engine_id: &str,
    method: DeployMethod,
    spec: &ModelSpec,
    input: &VramEstimateInput,
) -> String {
    let dialect = dialect_for(engine_id);
    let mut parts = base_command(dialect, method);
    if parts.is_empty() {
        return String::new();
    }
    parts.extend(build_args(dialect, spec, input));
    parts.join(" ")
}

/// Dzieli string argumentow na tokeny, zachowujac kompaktowy JSON
/// (`--speculative-config {...}`) jako pojedynczy element. shlex respektuje
/// cudzyslowy; fallback to prosty whitespace split.
fn split_args(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    shlex::split(trimmed).unwrap_or_else(|| trimmed.split_whitespace().map(String::from).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> VramEstimateInput {
        VramEstimateInput {
            max_model_len: 32768,
            max_num_seqs: 64,
            gpu_memory_utilization: 0.85,
            tensor_parallel: 2,
            ..Default::default()
        }
    }

    #[test]
    fn dialect_detection() {
        assert_eq!(dialect_for("sglang"), Dialect::Sglang);
        assert_eq!(dialect_for("vllm"), Dialect::Vllm);
        assert_eq!(dialect_for("vllm-spark"), Dialect::Vllm);
        assert_eq!(dialect_for("qwen3-vl"), Dialect::Vllm);
        assert_eq!(dialect_for("granite-vision"), Dialect::Vllm);
        assert_eq!(dialect_for("llama.cpp"), Dialect::LlamaCpp);
        assert_eq!(dialect_for("qwen3-vl-mlx"), Dialect::Mlx);
        assert_eq!(dialect_for("whisper"), Dialect::Generic);
    }

    #[test]
    fn sglang_uses_native_flag_names() {
        let spec = ModelSpec::default();
        let args = build_sglang_args(&spec, &input()).join(" ");
        assert!(args.contains("--context-length 32768"));
        assert!(args.contains("--mem-fraction-static 0.85"));
        assert!(args.contains("--max-running-requests 64"));
        assert!(args.contains("--chunked-prefill-size 8192"));
        assert!(args.contains("--tp 2"));
        // Zadnych vLLM-izmow.
        assert!(!args.contains("--max-model-len"));
        assert!(!args.contains("--gpu-memory-utilization"));
        assert!(!args.contains("--enable-flashinfer-autotune"));
    }

    #[test]
    fn command_string_has_base_and_placeholders() {
        let spec = ModelSpec::default();
        let cmd = build_command_string("sglang", DeployMethod::Docker, &spec, &input());
        assert!(cmd.starts_with("python3 -m sglang.launch_server --model-path \"$MODEL\""));
        assert!(cmd.contains("--port \"$PORT\""));
        let vcmd = build_command_string("vllm", DeployMethod::Docker, &spec, &input());
        assert!(vcmd.starts_with("vllm serve \"$MODEL\""));
        // Generic → pusta komenda (zarzadzana przez entrypoint).
        assert!(build_command_string("whisper", DeployMethod::Docker, &spec, &input()).is_empty());
    }
}

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
        Dialect::Sglang => vec![s("--tp"), s("1"), s("--mem-fraction-static"), s("0.85")],
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

/// Post-deploy tuning edit (`ServiceUpdate`). `None` = field untouched.
#[derive(Debug, Clone, Default)]
pub struct ServiceTuningPatch {
    pub gpu_memory_utilization: Option<f32>,
    pub max_model_len: Option<u32>,
    pub max_num_seqs: Option<u32>,
    pub max_num_batched_tokens: Option<u32>,
    pub kv_cache_dtype: Option<String>,
    pub chunked_prefill: Option<bool>,
    pub vllm_args_override: Option<String>,
}

/// Merges a tuning patch into a service `config_json` object. The scalar keys
/// are written as before (docker/native still read `gpu_memory_utilization`
/// from them) and, for engines whose argv is built from the stored `vllm_args`
/// string (vLLM and sglang dialects), the matching flags are rewritten inside
/// that string — otherwise a post-deploy edit never reaches the engine.
pub fn apply_service_tuning(
    engine_id: &str,
    cfg_obj: &mut serde_json::Map<String, serde_json::Value>,
    patch: &ServiceTuningPatch,
) {
    use serde_json::Value;
    if let Some(util) = patch.gpu_memory_utilization {
        if let Some(num) = serde_json::Number::from_f64(util as f64) {
            cfg_obj.insert("gpu_memory_utilization".into(), Value::Number(num));
        }
    }
    if let Some(v) = patch.max_model_len {
        cfg_obj.insert("max_model_len".into(), Value::Number(v.into()));
    }
    if let Some(v) = patch.max_num_seqs {
        cfg_obj.insert("max_num_seqs".into(), Value::Number(v.into()));
    }
    if let Some(v) = patch.max_num_batched_tokens {
        cfg_obj.insert("max_num_batched_tokens".into(), Value::Number(v.into()));
    }
    if let Some(dt) = patch.kv_cache_dtype.as_ref() {
        cfg_obj.insert("kv_cache_dtype".into(), Value::String(dt.clone()));
    }
    if let Some(b) = patch.chunked_prefill {
        cfg_obj.insert("chunked_prefill".into(), Value::Bool(b));
    }
    if let Some(args) = patch.vllm_args_override.as_ref() {
        cfg_obj.insert("vllm_args".into(), Value::String(args.clone()));
    }
    let dialect = dialect_for(engine_id);
    if !matches!(dialect, Dialect::Vllm | Dialect::Sglang) {
        return;
    }
    let existing = cfg_obj
        .get("vllm_args")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let rewritten = rewrite_engine_args(dialect, &existing, patch);
    if rewritten != existing {
        cfg_obj.insert("vllm_args".into(), Value::String(rewritten));
    }
}

/// Rewrites the tuned flags inside an engine args string in the engine's own
/// dialect: an existing occurrence is replaced (dedup last-wins), a missing one
/// is appended, quoting is preserved through shlex. Fields left `None` keep
/// whatever the string already carried.
pub fn rewrite_engine_args(dialect: Dialect, existing: &str, patch: &ServiceTuningPatch) -> String {
    let mut toks = split_args(existing);
    let mut push = |flag: &str, value: Option<String>| {
        toks.push(flag.to_string());
        if let Some(v) = value {
            toks.push(v);
        }
    };
    let mut changed = false;
    match dialect {
        Dialect::Vllm => {
            if let Some(v) = patch.max_model_len {
                push("--max-model-len", Some(v.to_string()));
                changed = true;
            }
            if let Some(v) = patch.max_num_seqs {
                push("--max-num-seqs", Some(v.to_string()));
                changed = true;
            }
            if let Some(v) = patch.max_num_batched_tokens {
                push("--max-num-batched-tokens", Some(v.to_string()));
                changed = true;
            }
            if let Some(dt) = patch.kv_cache_dtype.as_deref().filter(|d| !d.is_empty()) {
                push("--kv-cache-dtype", Some(dt.to_string()));
                changed = true;
            }
            if let Some(u) = patch.gpu_memory_utilization {
                push("--gpu-memory-utilization", Some(format!("{u:.2}")));
                changed = true;
            }
            if let Some(b) = patch.chunked_prefill {
                push(
                    if b {
                        "--enable-chunked-prefill"
                    } else {
                        "--no-enable-chunked-prefill"
                    },
                    None,
                );
                changed = true;
            }
        }
        Dialect::Sglang => {
            if let Some(v) = patch.max_model_len {
                push("--context-length", Some(v.to_string()));
                changed = true;
            }
            if let Some(v) = patch.max_num_seqs {
                push("--max-running-requests", Some(v.to_string()));
                changed = true;
            }
            if let Some(v) = patch.max_num_batched_tokens {
                push("--chunked-prefill-size", Some(v.to_string()));
                changed = true;
            }
            if let Some(dt) = patch.kv_cache_dtype.as_deref().filter(|d| !d.is_empty()) {
                push("--kv-cache-dtype", Some(dt.to_string()));
                changed = true;
            }
            if let Some(u) = patch.gpu_memory_utilization {
                push("--mem-fraction-static", Some(format!("{u:.2}")));
                changed = true;
            }
            // sglang has chunked prefill on by default; `-1` is its off switch
            // (re-enabling drops that switch below).
            match patch.chunked_prefill {
                Some(false) => {
                    push("--chunked-prefill-size", Some("-1".to_string()));
                    changed = true;
                }
                Some(true) => changed = true,
                None => {}
            }
        }
        Dialect::LlamaCpp | Dialect::Mlx | Dialect::Generic => {}
    }
    if !changed {
        return existing.to_string();
    }
    let mut deduped = crate::deploy::python_venv::dedup_cli_args_last_wins(toks);
    if dialect == Dialect::Sglang
        && patch.chunked_prefill == Some(true)
        && patch.max_num_batched_tokens.is_none()
    {
        // Re-enabling chunked prefill means dropping a stored `-1` off switch.
        if let Some(i) = deduped
            .windows(2)
            .position(|w| w[0] == "--chunked-prefill-size" && w[1] == "-1")
        {
            deduped.drain(i..i + 2);
        }
    }
    shlex::try_join(deduped.iter().map(String::as_str)).unwrap_or_else(|_| deduped.join(" "))
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
    fn rewrite_engine_args_replaces_and_appends_vllm_flags() {
        let existing = "--max-model-len 8192 --enable-chunked-prefill --speculative-config '{\"method\":\"mtp\",\"num_speculative_tokens\":2}' --tool-call-parser qwen3_coder";
        let patch = ServiceTuningPatch {
            max_model_len: Some(65536),
            max_num_seqs: Some(64),
            kv_cache_dtype: Some("fp8".into()),
            chunked_prefill: Some(false),
            ..Default::default()
        };
        let out = rewrite_engine_args(Dialect::Vllm, existing, &patch);
        let toks = shlex::split(&out).unwrap();
        // Replaced in place, not duplicated.
        assert_eq!(toks.iter().filter(|t| *t == "--max-model-len").count(), 1);
        assert!(toks
            .windows(2)
            .any(|w| w[0] == "--max-model-len" && w[1] == "65536"));
        assert!(toks
            .windows(2)
            .any(|w| w[0] == "--max-num-seqs" && w[1] == "64"));
        assert!(toks
            .windows(2)
            .any(|w| w[0] == "--kv-cache-dtype" && w[1] == "fp8"));
        // enable/no-enable pair collapses to the patched variant.
        assert!(toks.iter().any(|t| t == "--no-enable-chunked-prefill"));
        assert!(!toks.iter().any(|t| t == "--enable-chunked-prefill"));
        // Untouched flags survive, JSON stays one token.
        assert!(toks
            .iter()
            .any(|t| t == "{\"method\":\"mtp\",\"num_speculative_tokens\":2}"));
        assert!(toks
            .windows(2)
            .any(|w| w[0] == "--tool-call-parser" && w[1] == "qwen3_coder"));
        // Empty patch is a no-op.
        assert_eq!(
            rewrite_engine_args(Dialect::Vllm, existing, &ServiceTuningPatch::default()),
            existing
        );
    }

    #[test]
    fn rewrite_engine_args_uses_sglang_dialect() {
        let patch = ServiceTuningPatch {
            max_model_len: Some(16384),
            gpu_memory_utilization: Some(0.8),
            chunked_prefill: Some(false),
            ..Default::default()
        };
        let out = rewrite_engine_args(Dialect::Sglang, "--tp 2 --context-length 4096", &patch);
        assert_eq!(
            out,
            "--tp 2 --context-length 16384 --mem-fraction-static 0.80 --chunked-prefill-size -1"
        );
        // Re-enabling drops the `-1` off switch.
        let on = ServiceTuningPatch {
            chunked_prefill: Some(true),
            ..Default::default()
        };
        assert_eq!(
            rewrite_engine_args(Dialect::Sglang, &out, &on),
            "--tp 2 --context-length 16384 --mem-fraction-static 0.80"
        );
        // llama.cpp is not an args-string engine here — untouched.
        assert_eq!(
            rewrite_engine_args(Dialect::LlamaCpp, "-c 4096", &patch),
            "-c 4096"
        );
    }

    #[test]
    fn apply_service_tuning_writes_scalars_and_vllm_args() {
        let mut cfg = serde_json::json!({"vllm_args": "--max-model-len 4096", "model_repo": "x/y"});
        let obj = cfg.as_object_mut().unwrap();
        let patch = ServiceTuningPatch {
            max_model_len: Some(32768),
            max_num_batched_tokens: Some(4096),
            ..Default::default()
        };
        apply_service_tuning("vllm", obj, &patch);
        assert_eq!(obj["max_model_len"], 32768);
        assert_eq!(obj["max_num_batched_tokens"], 4096);
        assert_eq!(
            obj["vllm_args"],
            "--max-model-len 32768 --max-num-batched-tokens 4096"
        );
        // An explicit override is the base the flags are rewritten into.
        let patch2 = ServiceTuningPatch {
            vllm_args_override: Some("--dtype auto".into()),
            max_num_seqs: Some(8),
            ..Default::default()
        };
        apply_service_tuning("vllm", obj, &patch2);
        assert_eq!(obj["vllm_args"], "--dtype auto --max-num-seqs 8");
        // Engines without an args string keep only the scalar keys.
        let mut lc = serde_json::json!({});
        let lc_obj = lc.as_object_mut().unwrap();
        apply_service_tuning("llama-cpp", lc_obj, &patch);
        assert_eq!(lc_obj["max_model_len"], 32768);
        assert!(lc_obj.get("vllm_args").is_none());
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

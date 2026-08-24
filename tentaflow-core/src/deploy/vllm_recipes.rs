// ===== File: vllm_recipes.rs — vendored vLLM deployment recipes (offline-first) =====
//
// Mirrors the expert launch flags + env vars that https://recipes.vllm.ai serves
// per model. The rendered JSON is vendored into `vllm-recipes/recipes.json.gz`
// (refresh via `scripts/update-vllm-recipes.sh`) and embedded here, so offline /
// HF-only deploys get the same recipe. `vllm-recipes/supplement.json` carries
// hand-curated entries for models the upstream index does not have yet; a
// snapshot refresh never drops them. When the network reaches recipes.vllm.ai,
// `fetch_live` overrides the embedded entry with a fresh copy for that one model.
//
// Resolution is a cascade (`resolve_embedded`): exact repo id → repo id with
// quantization/packaging suffixes stripped (same org, then any org) → per
// architecture family defaults (`resolve_for_architecture`), so a community
// NVFP4/FP8/AWQ repack still gets the tool-call and reasoning parsers.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

const EMBEDDED_GZ: &[u8] = include_bytes!("../../vllm-recipes/recipes.json.gz");
const SUPPLEMENT_JSON: &str = include_str!("../../vllm-recipes/supplement.json");
const RECIPES_BASE_URL: &str = "https://recipes.vllm.ai";

/// Repo-name tokens that describe weight packing, not the model. Stripping them
/// maps `Inferact/Qwen3.8-27B-NVFP4` onto the `qwen3.8-27b` recipe.
const PACKAGING_TOKENS: &[&str] = &[
    "fp8",
    "nvfp4",
    "fp4",
    "mxfp4",
    "awq",
    "gptq",
    "int4",
    "int8",
    "w4a16",
    "w8a8",
    "w8a16",
    "bnb",
    "4bit",
    "8bit",
    "bf16",
    "fp16",
    "unsloth",
    "dynamic",
    "compressed",
    "tensors",
    "autoround",
    "marlin",
    "exl2",
    "gguf",
];

/// Orgs whose recipe wins when several orgs publish the same base name.
const CANONICAL_ORGS: &[&str] = &[
    "qwen/",
    "meta-llama/",
    "google/",
    "deepseek-ai/",
    "mistralai/",
    "zai-org/",
    "moonshotai/",
    "nvidia/",
];

#[derive(Debug, Clone, Default, Deserialize)]
pub struct HwOverride {
    #[serde(default)]
    pub extra_args: Vec<String>,
    #[serde(default)]
    pub extra_env: HashMap<String, String>,
}

/// One recipe normalized to the fields Core consumes. `base_argv` already has
/// the upstream `vllm serve <model>` prefix stripped by the vendor script.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RecipeEntry {
    #[serde(default)]
    pub hf_id: String,
    #[serde(default)]
    pub base_argv: Vec<String>,
    #[serde(default)]
    pub base_env: HashMap<String, String>,
    #[serde(default)]
    pub hardware_overrides: HashMap<String, HwOverride>,
    /// Extra env when the resolved id is a model variant (e.g. an FP4 repo).
    #[serde(default, rename = "_variant_env")]
    pub variant_env: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct Snapshot {
    #[serde(default)]
    recipes: HashMap<String, RecipeEntry>,
}

fn embedded() -> &'static HashMap<String, RecipeEntry> {
    static CELL: OnceLock<HashMap<String, RecipeEntry>> = OnceLock::new();
    CELL.get_or_init(|| {
        use flate2::read::GzDecoder;
        use std::io::Read;
        let mut s = String::new();
        if GzDecoder::new(EMBEDDED_GZ).read_to_string(&mut s).is_err() {
            return HashMap::new();
        }
        let mut recipes = serde_json::from_str::<Snapshot>(&s)
            .map(|x| x.recipes)
            .unwrap_or_default();
        // Curated entries only fill gaps: a refreshed upstream snapshot that
        // gained the model keeps its own (more current) recipe.
        if let Ok(extra) = serde_json::from_str::<Snapshot>(SUPPLEMENT_JSON) {
            for (k, v) in extra.recipes {
                recipes.entry(k.to_lowercase()).or_insert(v);
            }
        }
        recipes
    })
}

/// vLLM kernel family for a GPU model name. Drives `hardware_overrides`
/// (Hopper FP8 MoE, Blackwell FP4 MoE, AMD AITER). Datacenter + Blackwell
/// consumer/Spark map to a family; pre-Blackwell consumer (RTX 30/40 = Ampere/
/// Ada) and unknowns return None → only the model's base recipe flags apply, no
/// MoE env (those kernels don't exist there, so injecting them would break load).
pub fn gpu_family(gpu_name: &str) -> Option<&'static str> {
    let n = gpu_name.to_lowercase();
    // Blackwell: B100/B200/B300, GB200/GB300, GB10 (DGX Spark, sm_121),
    // RTX 50xx + RTX PRO 6000 Blackwell.
    if n.contains("b300")
        || n.contains("b200")
        || n.contains("b100")
        || n.contains("gb300")
        || n.contains("gb200")
        || n.contains("gb10")
        || n.contains("spark")
        || n.contains("rtx 50")
        || n.contains("rtx pro 6000")
    {
        return Some("blackwell");
    }
    // Hopper: H100/H200/H800, GH200 (Grace-Hopper).
    if n.contains("h300")
        || n.contains("h200")
        || n.contains("h100")
        || n.contains("h800")
        || n.contains("gh200")
    {
        return Some("hopper");
    }
    if n.contains("mi300") || n.contains("mi325") || n.contains("mi355") || n.contains("instinct") {
        return Some("amd");
    }
    None
}

/// Which cascade level produced a recipe (debug logging + the GUI badge).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipeMatch {
    /// Exact lowercase repo id.
    Exact,
    /// Packaging suffixes stripped, same org.
    NormalizedSameOrg,
    /// Packaging suffixes stripped, recipe from another (preferably canonical) org.
    NormalizedAnyOrg,
    /// Architecture-family defaults (no repo-specific recipe at all).
    Architecture,
}

/// Lowercase repo name with quantization/packaging tokens removed. Tokens are
/// separated by `-`/`_`; separators of the surviving tokens are preserved so
/// `internvl3_5-8b` keeps its underscore. Returns the name part only (no org).
pub fn normalize_repo_base(model_repo: &str) -> String {
    let lower = model_repo.trim().to_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    let mut out = String::with_capacity(name.len());
    let mut token = String::new();
    // Separator that preceded the token currently being collected.
    let mut sep: Option<char> = None;
    let flush = |out: &mut String, token: &mut String, sep: Option<char>| {
        if !token.is_empty() && !PACKAGING_TOKENS.contains(&token.as_str()) {
            if let (Some(c), false) = (sep, out.is_empty()) {
                out.push(c);
            }
            out.push_str(token);
        }
        token.clear();
    };
    for ch in name.chars() {
        if ch == '-' || ch == '_' {
            flush(&mut out, &mut token, sep);
            sep = Some(ch);
        } else {
            token.push(ch);
        }
    }
    flush(&mut out, &mut token, sep);
    out
}

/// Resolve the embedded recipe for a HF repo id (case-insensitive) through the
/// cascade: exact id → normalized id in the same org → normalized id in any org
/// (canonical orgs first). Variant model ids resolve to their parent recipe
/// with the variant env folded in. Architecture defaults are a separate step
/// (`resolve_for_architecture`) because they need the parsed config.
pub fn resolve_embedded(model_repo: &str) -> Option<(RecipeEntry, RecipeMatch)> {
    let table = embedded();
    let id = model_repo.trim().to_lowercase();
    if let Some(e) = table.get(&id) {
        return Some((e.clone(), RecipeMatch::Exact));
    }
    let base = normalize_repo_base(&id);
    if base.is_empty() {
        return None;
    }
    let (org, name) = match id.rsplit_once('/') {
        Some((o, n)) => (Some(o), n),
        None => (None, id.as_str()),
    };
    // With nothing stripped the same-org lookup would just repeat the exact miss.
    if base != name {
        if let Some(o) = org {
            if let Some(e) = table.get(&format!("{o}/{base}")) {
                return Some((e.clone(), RecipeMatch::NormalizedSameOrg));
            }
        }
    }
    let mut candidates: Vec<&String> = table
        .keys()
        .filter(|k| *k != &id)
        .filter(|k| k.rsplit('/').next() == Some(base.as_str()))
        .collect();
    if candidates.is_empty() {
        return None;
    }
    candidates.sort();
    let pick = CANONICAL_ORGS
        .iter()
        .find_map(|org| candidates.iter().find(|k| k.starts_with(org)))
        .or_else(|| candidates.first())?;
    table
        .get(*pick)
        .map(|e| (e.clone(), RecipeMatch::NormalizedAnyOrg))
}

/// Architecture-family defaults for repos with no recipe at all: the parser
/// flags every model of the family needs, nothing hardware- or size-specific.
/// Parser names follow what recipes.vllm.ai publishes for the family: Qwen3.5+
/// use the XML tool format (`qwen3_coder`), older Qwen3/Qwen3-Next/Qwen3-VL
/// speak hermes JSON. Multimodal Qwen3.5+ also get the data-parallel vision
/// encoder like the upstream 27B recipe. Gemma stays out on purpose — the
/// gemma-4 flags are applied by the caller for the whole family.
pub fn resolve_for_architecture(
    architectures: &[String],
    model_type: &str,
    has_vision: bool,
) -> Option<RecipeEntry> {
    let arch = architectures.first().map(String::as_str).unwrap_or("");
    let arch_lc = arch.to_lowercase();
    let mt = model_type.to_lowercase();
    let s = |v: &str| v.to_string();
    let parsers = |tool: &str, reasoning: Option<&str>| -> Vec<String> {
        let mut v = vec![
            s("--enable-auto-tool-choice"),
            s("--tool-call-parser"),
            s(tool),
        ];
        if let Some(r) = reasoning {
            v.push(s("--reasoning-parser"));
            v.push(s(r));
        }
        v
    };
    let qwen35_plus = arch_lc.starts_with("qwen3_5")
        || arch_lc.starts_with("qwen3_6")
        || arch_lc.starts_with("qwen3_7")
        || arch_lc.starts_with("qwen3_8")
        || mt.starts_with("qwen3_5")
        || mt.starts_with("qwen3_6")
        || mt.starts_with("qwen3_7")
        || mt.starts_with("qwen3_8");
    let mut argv: Vec<String> = if qwen35_plus {
        let mut v = vec![s("--trust-remote-code")];
        v.extend(parsers("qwen3_coder", Some("qwen3")));
        if has_vision || arch_lc.contains("forconditionalgeneration") {
            v.push(s("--mm-encoder-tp-mode"));
            v.push(s("data"));
        }
        v
    } else if arch_lc.starts_with("qwen3vl") || mt.starts_with("qwen3_vl") {
        let mut v = parsers("hermes", Some("qwen3"));
        v.push(s("--mm-encoder-tp-mode"));
        v.push(s("data"));
        v
    } else if arch_lc.starts_with("qwen3") || mt.starts_with("qwen3") {
        parsers("hermes", Some("qwen3"))
    } else if arch_lc.starts_with("glm4") || mt.starts_with("glm4") {
        let mut v = vec![s("--trust-remote-code")];
        v.extend(parsers("glm45", Some("glm45")));
        v
    } else if arch_lc.starts_with("glm") || mt.starts_with("glm") {
        let mut v = vec![s("--trust-remote-code")];
        v.extend(parsers("glm47", Some("glm45")));
        v
    } else if arch_lc.starts_with("deepseekv4") || mt == "deepseek_v4" {
        let mut v = vec![
            s("--trust-remote-code"),
            s("--tokenizer-mode"),
            s("deepseek_v4"),
        ];
        v.extend(parsers("deepseek_v4", Some("deepseek_v4")));
        v
    } else if arch_lc.starts_with("deepseekv3") || mt == "deepseek_v3" {
        let mut v = vec![s("--trust-remote-code")];
        v.extend(parsers("deepseek_v31", Some("deepseek_v3")));
        v
    } else if arch_lc.starts_with("llama4") || mt == "llama4" {
        parsers("llama4_pythonic", None)
    } else if arch_lc.starts_with("llama") || mt == "llama" {
        parsers("llama3_json", None)
    } else if arch_lc.starts_with("mistral") || arch_lc.starts_with("mixtral") || mt == "mistral" {
        parsers("mistral", None)
    } else if arch_lc.starts_with("gptoss") || mt == "gpt_oss" {
        parsers("openai", None)
    } else {
        return None;
    };
    argv.dedup();
    Some(RecipeEntry {
        hf_id: format!(
            "architecture:{}",
            if arch.is_empty() { model_type } else { arch }
        ),
        base_argv: argv,
        ..RecipeEntry::default()
    })
}

/// Fetch a fresh recipe straight from recipes.vllm.ai for a single model. Two
/// requests (index for the case-correct path, then the recipe). Returns None on
/// any network/parse error — callers fall back to `resolve_embedded`.
pub async fn fetch_live(client: &reqwest::Client, model_repo: &str) -> Option<RecipeEntry> {
    #[derive(Deserialize)]
    struct IndexEntry {
        hf_id: String,
        json: String,
    }
    let index: Vec<IndexEntry> = client
        .get(format!("{RECIPES_BASE_URL}/models.json"))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let target = model_repo.trim().to_lowercase();
    let json_path = index
        .iter()
        .find(|e| e.hf_id.to_lowercase() == target)
        .map(|e| e.json.clone())?;
    let raw: serde_json::Value = client
        .get(format!("{RECIPES_BASE_URL}{json_path}"))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    Some(parse_raw_recipe(&raw))
}

/// Normalize a raw recipes.vllm.ai recipe JSON into a `RecipeEntry` (strips the
/// `vllm serve <model>` argv prefix, keeps base env + hardware overrides).
fn parse_raw_recipe(v: &serde_json::Value) -> RecipeEntry {
    let rc = v.get("recommended_command");
    let argv: Vec<String> = rc
        .and_then(|r| r.get("argv"))
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let base_env: HashMap<String, String> = rc
        .and_then(|r| r.get("env"))
        .and_then(|e| serde_json::from_value(e.clone()).ok())
        .unwrap_or_default();
    let hardware_overrides: HashMap<String, HwOverride> = v
        .get("hardware_overrides")
        .and_then(|h| serde_json::from_value(h.clone()).ok())
        .unwrap_or_default();
    RecipeEntry {
        hf_id: v
            .get("hf_id")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string(),
        base_argv: strip_serve_prefix(argv),
        base_env,
        hardware_overrides,
        variant_env: HashMap::new(),
    }
}

fn strip_serve_prefix(argv: Vec<String>) -> Vec<String> {
    if argv.len() >= 3 && argv[0] == "vllm" && argv[1] == "serve" {
        return argv[3..].to_vec();
    }
    if argv.len() >= 2 && argv[0] == "vllm" && argv[1] == "serve" {
        return argv[2..].to_vec();
    }
    argv
}

/// Curated recipe CLI args + env for a model on a GPU family. Drops repo-relative
/// `--chat-template` paths (absent from our deploy tree), and forces TP/PP to the
/// values Core computed from the actual GPUs (recipe values are node-specific).
/// In vLLM a tool-call parser without `--enable-auto-tool-choice` is inert: the
/// server keeps the OpenAI `tools` field but never routes a generation through
/// the parser, so it answers with prose and NEVER emits `tool_calls`. Nothing
/// logs a complaint — an agent simply looks like a model that will not use its
/// tools.
///
/// The pairing is mechanical, so it is repaired rather than reported: a recipe
/// that names a parser meant tool calling. The upstream snapshot is regenerated
/// by `scripts/update-vllm-recipes.sh`, so patching the data would be undone on
/// the next refresh; normalising on load survives it.
fn normalise_tool_flags(argv: &mut Vec<String>) {
    let has_parser = argv.iter().any(|a| a == "--tool-call-parser");
    let has_auto = argv.iter().any(|a| a == "--enable-auto-tool-choice");
    if has_parser && !has_auto {
        argv.push("--enable-auto-tool-choice".to_string());
    }
}

/// Why the assembled argv will not serve tool calls, or `None` when it will.
///
/// A deploy that silently cannot call tools is the worst failure mode we have:
/// every agent on that model degrades to a chatbot and the logs stay clean.
/// This is what the deploy path warns on (`handlers.rs`), and it is deliberately
/// a diagnosis rather than a repair — the right parser depends on the model
/// family (`hermes`, `qwen3_coder`, `qwen3_xml`, `deepseek_v3`…) and guessing it
/// wrong is worse than saying nothing works.
pub fn tool_calling_gap(argv: &[String]) -> Option<&'static str> {
    if !argv.iter().any(|a| a == "--tool-call-parser") {
        return Some(
            "no --tool-call-parser: vLLM will accept the `tools` field and never return a tool call",
        );
    }
    if !argv.iter().any(|a| a == "--enable-auto-tool-choice") {
        return Some("--tool-call-parser without --enable-auto-tool-choice: the parser is inert");
    }
    None
}

pub fn build_args(
    entry: &RecipeEntry,
    family: Option<&str>,
    tensor_parallel: u32,
    pipeline_parallel: u32,
) -> (Vec<String>, HashMap<String, String>) {
    let mut env = entry.base_env.clone();
    let mut raw = entry.base_argv.clone();
    if let Some(f) = family {
        if let Some(o) = entry.hardware_overrides.get(f) {
            raw.extend(o.extra_args.clone());
            for (k, v) in &o.extra_env {
                env.insert(k.clone(), v.clone());
            }
        }
    }
    for (k, v) in &entry.variant_env {
        env.insert(k.clone(), v.clone());
    }

    let mut argv: Vec<String> = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--chat-template" => {
                let val = raw.get(i + 1).map(String::as_str).unwrap_or("");
                // Repo-relative example templates ship with the vLLM source, not
                // our bundle — keep only absolute/explicit template paths.
                if val.starts_with("examples/") || val.ends_with(".jinja") {
                    i += 2;
                    continue;
                }
                argv.push(raw[i].clone());
                i += 1;
            }
            "--tensor-parallel-size" | "-tp" => {
                // Recipe value is for its reference node; Core owns parallelism.
                i += 2;
            }
            "--pipeline-parallel-size" | "-pp" => {
                i += 2;
            }
            _ => {
                argv.push(raw[i].clone());
                i += 1;
            }
        }
    }
    if tensor_parallel > 1 {
        argv.push("--tensor-parallel-size".to_string());
        argv.push(tensor_parallel.to_string());
    }
    if pipeline_parallel > 1 {
        argv.push("--pipeline-parallel-size".to_string());
        argv.push(pipeline_parallel.to_string());
    }
    // After the merge, so a hardware override that adds the parser is seen too.
    normalise_tool_flags(&mut argv);
    (argv, env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_snapshot_loads() {
        let m = embedded();
        assert!(!m.is_empty(), "embedded recipe snapshot should decode");
    }

    /// A parser without auto-tool-choice is inert in vLLM, so the pair is
    /// repaired on the way out of `build_args` — the upstream snapshot ships at
    /// least one recipe like that (`xiaomimimo/mimo-v2-flash`) and regenerating
    /// it would bring the gap back.
    #[test]
    fn a_parser_without_auto_tool_choice_is_repaired() {
        let entry = RecipeEntry {
            hf_id: "x/y".into(),
            base_argv: vec!["--tool-call-parser".into(), "hermes".into()],
            base_env: HashMap::new(),
            hardware_overrides: HashMap::new(),
            variant_env: HashMap::new(),
        };
        let (argv, _) = build_args(&entry, None, 1, 1);
        assert!(argv.iter().any(|a| a == "--enable-auto-tool-choice"));
        assert_eq!(tool_calling_gap(&argv), None);
    }

    /// The repair must not invent a parser: which one is right depends on the
    /// model family, and a wrong parser fails as silently as none.
    #[test]
    fn a_recipe_without_a_parser_is_reported_not_guessed() {
        let entry = RecipeEntry {
            hf_id: "x/y".into(),
            base_argv: vec!["--max-model-len".into(), "auto".into()],
            base_env: HashMap::new(),
            hardware_overrides: HashMap::new(),
            variant_env: HashMap::new(),
        };
        let (argv, _) = build_args(&entry, None, 1, 1);
        assert!(!argv.iter().any(|a| a == "--tool-call-parser"));
        assert!(
            tool_calling_gap(&argv).is_some(),
            "the gap must be reported"
        );
    }

    /// Tool flags live in `base_argv`, never in a hardware override, so tool
    /// calling cannot depend on which GPU the model lands on. A recipe that
    /// broke this would serve tools on an H100 and silently not on a 4090.
    #[test]
    fn tool_flags_never_depend_on_the_gpu() {
        for (id, entry) in embedded() {
            let base_has = entry.base_argv.iter().any(|a| a == "--tool-call-parser");
            for (family, over) in &entry.hardware_overrides {
                let only_here = over.extra_args.iter().any(|a| a == "--tool-call-parser");
                assert!(
                    !(only_here && !base_has),
                    "{id}: --tool-call-parser only under hardware override '{family}'",
                );
            }
        }
    }

    #[test]
    fn normalize_repo_base_strips_packaging_suffixes() {
        assert_eq!(
            normalize_repo_base("Inferact/Qwen3.8-27B-NVFP4"),
            "qwen3.8-27b"
        );
        assert_eq!(
            normalize_repo_base("unsloth/Qwen3.8-27B-FP8-Dynamic"),
            "qwen3.8-27b"
        );
        assert_eq!(
            normalize_repo_base("Qwen/Qwen3.5-27B-AWQ-Int4"),
            "qwen3.5-27b"
        );
        assert_eq!(
            normalize_repo_base("RedHatAI/gemma-4-31b-it-FP8-dynamic"),
            "gemma-4-31b-it"
        );
        // Separators of surviving tokens are kept verbatim.
        assert_eq!(
            normalize_repo_base("OpenGVLab/InternVL3_5-8B"),
            "internvl3_5-8b"
        );
        assert_eq!(
            normalize_repo_base("meta-llama/Llama-3.3-70B-Instruct"),
            "llama-3.3-70b-instruct"
        );
    }

    #[test]
    fn resolve_embedded_cascades_over_normalized_ids() {
        let (e, level) = resolve_embedded("Qwen/Qwen3.6-27B").expect("exact");
        assert_eq!(level, RecipeMatch::Exact);
        assert_eq!(e.hf_id, "Qwen/Qwen3.6-27B");

        // Same org, packaging suffix stripped.
        let (e, level) = resolve_embedded("Qwen/Qwen3.5-27B-AWQ-Int4").expect("same org");
        assert_eq!(level, RecipeMatch::NormalizedSameOrg);
        assert_eq!(e.hf_id, "Qwen/Qwen3.5-27B");

        // Community repack → canonical org wins.
        let (e, level) = resolve_embedded("Inferact/Qwen3.8-27B-NVFP4").expect("any org");
        assert_eq!(level, RecipeMatch::NormalizedAnyOrg);
        assert_eq!(e.hf_id, "Qwen/Qwen3.8-27B");
        assert!(e.base_argv.iter().any(|a| a == "qwen3_coder"));
        let (e, level) = resolve_embedded("unsloth/Qwen3.8-27B-FP8-Dynamic").expect("any org");
        assert_eq!(level, RecipeMatch::NormalizedAnyOrg);
        assert_eq!(e.hf_id, "Qwen/Qwen3.8-27B");

        // Supplement entries are exact hits and mirror the 3.6 recipe.
        let (fp8, level) = resolve_embedded("Qwen/Qwen3.8-27B-FP8").expect("supplement");
        assert_eq!(level, RecipeMatch::Exact);
        assert!(fp8.base_argv.iter().any(|a| a == "--reasoning-parser"));

        assert!(resolve_embedded("acme/totally-unknown-7b-fp8").is_none());
    }

    #[test]
    fn architecture_fallback_covers_known_families() {
        let q =
            resolve_for_architecture(&["Qwen3_5ForConditionalGeneration".into()], "qwen3_5", true)
                .expect("qwen3.5");
        let argv = q.base_argv.join(" ");
        assert!(argv.contains("--tool-call-parser qwen3_coder"));
        assert!(argv.contains("--reasoning-parser qwen3"));
        assert!(argv.contains("--mm-encoder-tp-mode data"));
        assert!(argv.contains("--enable-auto-tool-choice"));

        let dense =
            resolve_for_architecture(&["Qwen3ForCausalLM".into()], "qwen3", false).expect("qwen3");
        assert!(dense
            .base_argv
            .join(" ")
            .contains("--tool-call-parser hermes"));
        assert!(!dense.base_argv.join(" ").contains("--mm-encoder-tp-mode"));

        let glm = resolve_for_architecture(&["Glm4MoeForCausalLM".into()], "glm4_moe", false)
            .expect("glm4");
        assert!(glm.base_argv.join(" ").contains("--tool-call-parser glm45"));

        let ds = resolve_for_architecture(&["DeepseekV3ForCausalLM".into()], "deepseek_v3", false)
            .expect("deepseek");
        assert!(ds
            .base_argv
            .join(" ")
            .contains("--tool-call-parser deepseek_v31"));
        assert!(ds
            .base_argv
            .join(" ")
            .contains("--reasoning-parser deepseek_v3"));

        let llama =
            resolve_for_architecture(&["LlamaForCausalLM".into()], "llama", false).expect("llama");
        assert!(llama
            .base_argv
            .join(" ")
            .contains("--tool-call-parser llama3_json"));

        let mistral = resolve_for_architecture(&["MistralForCausalLM".into()], "mistral", false)
            .expect("mistral");
        assert!(mistral
            .base_argv
            .join(" ")
            .contains("--tool-call-parser mistral"));

        assert!(resolve_for_architecture(
            &["Gemma4ForConditionalGeneration".into()],
            "gemma4",
            true
        )
        .is_none());
        assert!(resolve_for_architecture(&[], "", false).is_none());
    }

    #[test]
    fn gpu_family_maps_blackwell_and_hopper() {
        assert_eq!(gpu_family("NVIDIA B300"), Some("blackwell"));
        assert_eq!(gpu_family("NVIDIA HGX H200"), Some("hopper"));
        assert_eq!(gpu_family("AMD Instinct MI300X"), Some("amd"));
        assert_eq!(gpu_family("NVIDIA RTX 4090"), None);
    }

    #[test]
    fn build_args_curates_chat_template_and_forces_tp() {
        let entry = RecipeEntry {
            hf_id: "x".into(),
            base_argv: vec![
                "--trust-remote-code".into(),
                "--tensor-parallel-size".into(),
                "8".into(),
                "--chat-template".into(),
                "examples/tool_chat_template_deepseekv3.jinja".into(),
                "--tool-call-parser".into(),
                "deepseek_v3".into(),
            ],
            base_env: HashMap::new(),
            hardware_overrides: HashMap::from([(
                "blackwell".to_string(),
                HwOverride {
                    extra_args: vec![],
                    extra_env: HashMap::from([(
                        "VLLM_USE_FLASHINFER_MOE_FP4".to_string(),
                        "1".to_string(),
                    )]),
                },
            )]),
            variant_env: HashMap::new(),
        };
        let (argv, env) = build_args(&entry, Some("blackwell"), 4, 1);
        assert!(
            !argv.iter().any(|a| a == "--chat-template"),
            "repo template dropped"
        );
        assert!(argv
            .windows(2)
            .any(|w| w[0] == "--tensor-parallel-size" && w[1] == "4"));
        assert!(argv.iter().any(|a| a == "--tool-call-parser"));
        assert_eq!(
            env.get("VLLM_USE_FLASHINFER_MOE_FP4").map(String::as_str),
            Some("1")
        );
    }
}

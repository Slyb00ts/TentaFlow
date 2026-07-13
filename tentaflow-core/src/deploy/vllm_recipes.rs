// ===== File: vllm_recipes.rs — vendored vLLM deployment recipes (offline-first) =====
//
// Mirrors the expert launch flags + env vars that https://recipes.vllm.ai serves
// per model. The rendered JSON is vendored into `vllm-recipes/recipes.json.gz`
// (refresh via `scripts/update-vllm-recipes.sh`) and embedded here, so offline /
// HF-only deploys get the same recipe. When the network reaches recipes.vllm.ai,
// `fetch_live` overrides the embedded entry with a fresh copy for that one model.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

const EMBEDDED_GZ: &[u8] = include_bytes!("../../vllm-recipes/recipes.json.gz");
const RECIPES_BASE_URL: &str = "https://recipes.vllm.ai";

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
        serde_json::from_str::<Snapshot>(&s)
            .map(|x| x.recipes)
            .unwrap_or_default()
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

/// Resolve the embedded recipe for a HF repo id (case-insensitive). Variant
/// model ids resolve to their parent recipe with the variant env folded in.
pub fn resolve_embedded(model_repo: &str) -> Option<RecipeEntry> {
    embedded().get(&model_repo.trim().to_lowercase()).cloned()
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

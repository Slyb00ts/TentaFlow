// ===== File: hf_config.rs — HuggingFace config.json parsing =====

use std::path::Path;

use forge_types::{ForgeError, Result};
use serde::Deserialize;

/// Subset of HF `config.json` that the loader and arch registry consume.
/// Unknown fields are ignored; `quantization_config` is passed through raw so
/// quant-scheme detectors (e.g. NVFP4) can inspect vendor-specific layouts.
#[derive(Debug, Clone, Deserialize)]
pub struct HfConfig {
    #[serde(default)]
    pub architectures: Vec<String>,
    #[serde(default)]
    pub model_type: Option<String>,
    pub hidden_size: usize,
    pub num_hidden_layers: usize,
    pub num_attention_heads: usize,
    /// Absent means MHA: one KV head per attention head.
    #[serde(default)]
    pub num_key_value_heads: Option<usize>,
    /// Absent means hidden_size / num_attention_heads.
    #[serde(default)]
    pub head_dim: Option<usize>,
    pub intermediate_size: usize,
    pub vocab_size: usize,
    #[serde(default = "default_rope_theta")]
    pub rope_theta: f32,
    #[serde(default = "default_rms_norm_eps")]
    pub rms_norm_eps: f32,
    pub max_position_embeddings: usize,
    #[serde(default)]
    pub tie_word_embeddings: bool,
    /// transformers ≥ 4.56 writes `dtype`, older versions `torch_dtype`.
    #[serde(default, alias = "dtype")]
    pub torch_dtype: Option<String>,
    #[serde(default)]
    pub quantization_config: Option<serde_json::Value>,
}

fn default_rope_theta() -> f32 {
    10_000.0
}

fn default_rms_norm_eps() -> f32 {
    1e-5
}

impl HfConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let bytes = std::fs::read(path.as_ref())?;
        serde_json::from_slice(&bytes).map_err(|e| ForgeError::Format(format!("config.json: {e}")))
    }

    pub fn num_key_value_heads(&self) -> usize {
        self.num_key_value_heads.unwrap_or(self.num_attention_heads)
    }

    pub fn head_dim(&self) -> usize {
        self.head_dim
            .unwrap_or(self.hidden_size / self.num_attention_heads.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_config_with_dtype_alias() {
        let cfg: HfConfig = serde_json::from_str(
            r#"{
                "architectures": ["LlamaForCausalLM"],
                "model_type": "llama",
                "hidden_size": 1536,
                "num_hidden_layers": 32,
                "num_attention_heads": 12,
                "num_key_value_heads": 2,
                "head_dim": 128,
                "intermediate_size": 8960,
                "vocab_size": 32000,
                "rope_theta": 1000000,
                "rms_norm_eps": 1e-6,
                "max_position_embeddings": 8192,
                "dtype": "bfloat16"
            }"#,
        )
        .unwrap();
        assert_eq!(cfg.torch_dtype.as_deref(), Some("bfloat16"));
        assert_eq!(cfg.num_key_value_heads(), 2);
        assert_eq!(cfg.head_dim(), 128);
        assert!(!cfg.tie_word_embeddings);
    }

    #[test]
    fn defaults_for_optional_fields() {
        let cfg: HfConfig = serde_json::from_str(
            r#"{
                "hidden_size": 1024,
                "num_hidden_layers": 4,
                "num_attention_heads": 16,
                "intermediate_size": 3072,
                "vocab_size": 32000,
                "max_position_embeddings": 4096
            }"#,
        )
        .unwrap();
        assert_eq!(cfg.num_key_value_heads(), 16);
        assert_eq!(cfg.head_dim(), 64);
        assert_eq!(cfg.rope_theta, 10_000.0);
        assert_eq!(cfg.rms_norm_eps, 1e-5);
    }
}

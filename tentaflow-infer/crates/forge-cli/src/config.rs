// ===== File: config.rs — shared runtime defaults for every Forge entrypoint =====

pub const DEFAULT_PREFILL_CHUNK: usize = 1024;
pub const DEFAULT_MAX_ACTIVE: usize = 1;
pub const DEFAULT_BATCH_MIN: usize = 2;
pub const DEFAULT_KV_PAGES: usize = 0;
pub const DEFAULT_PREFIX_CACHE: &str = "off";
pub const DEFAULT_SPECULATIVE: &str = "off";
pub const DEFAULT_REPS: usize = 5;

pub fn sampling_from_profile(
    profile: forge_engine::model_profile::ModelProfile,
) -> forge_engine::sample::SamplingParams {
    forge_engine::sample::SamplingParams {
        temperature: profile.temperature,
        top_p: profile.top_p,
        top_k: profile.top_k,
        min_p: profile.min_p,
        repetition_penalty: profile.repetition_penalty,
        ..Default::default()
    }
}

pub fn chat_template(arch: &str, template: String) -> anyhow::Result<String> {
    if matches!(arch, "muse-glimmer" | "muse_glimmer") {
        forge_tokenize::builtin_chat_template("muse_glimmer")
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("brak wbudowanego szablonu Muse"))
    } else {
        Ok(template)
    }
}

#[cfg(test)]
mod tests {
    use super::chat_template;

    #[test]
    fn muse_uses_builtin_template() {
        let template = chat_template("muse_glimmer", "LOCAL".into()).unwrap();
        assert!(template.contains("<|start|>assistant"));
        assert!(!template.contains("LOCAL"));
    }
}

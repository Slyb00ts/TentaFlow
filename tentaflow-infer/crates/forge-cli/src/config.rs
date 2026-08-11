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

pub fn chat_template(arch: &str, template: String) -> String {
    if matches!(arch, "muse-glimmer" | "muse_glimmer") {
        "{{ bos_token }}{% for message in messages %}<|start|>{{ message['role'] }}<|message|>{{ message['content'] }}<|eot|>{% endfor %}<|start|>assistant<|message|>".to_string()
    } else {
        template
    }
}

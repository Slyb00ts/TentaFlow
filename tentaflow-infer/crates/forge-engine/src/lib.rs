// ===== File: lib.rs — forge-engine: LLM model execution over HAL + kernels =====
// v0 scope (PLAN chunk 5): load qwen3/llama-family weights from GGUF or
// safetensors (incl. NVFP4), run decode-path forward on one GPU, greedy /
// top-k/top-p sampling on CPU, streaming generation with stop handling.

pub mod deepseek;
pub mod expert_spill;
pub mod generate;
pub mod gguf_vocab;
pub mod kv;
pub mod metrics;
pub mod cluster;
pub mod model_profile;
pub mod multi_gpu;
pub mod tensor_parallel;
pub mod topology;
pub mod model;
pub mod moe_residency;
pub mod mtp;
pub mod prefix;
pub mod sample;
pub mod server;
pub mod speculation;
pub mod tier;
pub mod weight_tier;
pub mod weights;

pub use generate::{GenerateRequest, Generated, StreamEvent};
pub use model::Model;
pub use sample::SamplingParams;

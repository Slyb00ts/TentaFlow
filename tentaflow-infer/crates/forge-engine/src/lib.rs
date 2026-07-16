// ===== File: lib.rs — forge-engine: LLM model execution over HAL + kernels =====
// v0 scope (PLAN chunk 5): load qwen3/llama-family weights from GGUF or
// safetensors (incl. NVFP4), run decode-path forward on one GPU, greedy /
// top-k/top-p sampling on CPU, streaming generation with stop handling.

pub mod generate;
pub mod gguf_vocab;
pub mod kv;
pub mod model;
pub mod sample;
pub mod server;
pub mod weights;

pub use generate::{GenerateRequest, Generated, StreamEvent};
pub use model::Model;
pub use sample::SamplingParams;

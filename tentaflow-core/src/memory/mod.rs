// =============================================================================
// Plik: memory/mod.rs
// Opis: Centralne zarzadzanie pamiecia GPU/RAM dla wszystkich silnikow
//       AI (embedded MLX/llama.cpp, python-bundle vllm/sglang/xtts, docker).
//       Lazy load + LRU eviction gdy brak budzetu VRAM. Pinned services
//       (np. STT whisper, TTS sherpa, orchestrator Qwen 0.8B) sa zawsze
//       w pamieci. Paused services nie startuja z programem.
// =============================================================================

mod engine;
mod guard;
pub mod impls;
mod state;

pub use engine::LoadableEngine;
pub use guard::{global as guard_global, MemoryGuard};
pub use impls::{estimate_vram_for_model, DockerEngine, EmbeddedEngine, PythonBundleEngine};
pub use state::{LoadState, ServiceMemState};

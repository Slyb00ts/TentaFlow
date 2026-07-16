// ===== File: lib.rs — forge-formats: GGUF/safetensors loaders, reference dequant, arch registry =====
//
// Loaders in this crate are a trust boundary (spec §9.5): model files are
// untrusted input. Every offset and length coming from a file is
// bounds-checked with checked arithmetic and parse failures surface as
// `ForgeError::Format` — never panics.

pub mod arch;
pub mod dequant;
pub mod gguf;
pub mod hf_config;
pub mod nvfp4;
pub mod safetensors;

pub use arch::{ArchSpec, Hyperparams, ModelDescriptor, WeightRole};
pub use dequant::dequantize_to_f32;
pub use gguf::{Gguf, GgufTensor, MetaValue};
pub use hf_config::HfConfig;
pub use nvfp4::{NvFp4Scheme, NvFp4TensorNames};
pub use safetensors::{SafeTensors, ShardedSafeTensors, StTensor};

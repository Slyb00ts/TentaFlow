// ===== File: lib.rs — forge-formats: GGUF/safetensors loaders, reference dequant, arch registry =====
//
// Loaders in this crate are a trust boundary (spec §9.5): model files are
// untrusted input. Every offset and length coming from a file is
// bounds-checked with checked arithmetic and parse failures surface as
// `ForgeError::Format` — never panics.

pub mod arch;
pub mod deltanet;
pub mod dequant;
pub mod gguf;
pub mod hf_config;
pub mod iq_tables;
pub mod nvfp4;
pub mod safetensors;
pub mod speculation_manifest;
pub mod w4a8;

pub use arch::{
    AltAttnParams, ArchSpec, BlockMatrix, FfnActivation, Hyperparams, LayerKind, ModelDescriptor,
    MoeParams, MtpDescriptor, MtpWeightRole, PoolingType, RoleShard, SsmParams, TpShard, WeightRole,
};
pub use dequant::dequantize_to_f32;
pub use gguf::{Gguf, GgufTensor, MetaValue};
pub use hf_config::HfConfig;
pub use nvfp4::{
    nvfp4_ct_s0_from_e4m3, nvfp4_ct_s0_to_f32, NvFp4Scheme, NvFp4TensorNames,
    NVFP4_CT_S0_NAN,
};
pub use safetensors::{SafeTensors, ShardedSafeTensors, StTensor};
pub use speculation_manifest::{
    ArtifactRole, ArtifactSpec, CompositionMode, ConfidenceCalibration, ConfidenceMethod,
    FeatureDimension, Fingerprint, FingerprintAlgorithm, LicenseInfo, NeuralProposerKind,
    Quantization, SamplingMode, SharedTensor, SourceInfo, SpeculationDType, SpeculationManifest,
    TargetModel, TensorMapping, VerifiedArtifact, VerifiedSpeculationManifest,
};

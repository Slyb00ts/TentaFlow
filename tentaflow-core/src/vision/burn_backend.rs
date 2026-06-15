// =============================================================================
// File: vision/burn_backend.rs — compile-time Burn backend selection for vision
// =============================================================================
//
// One backend type for all camera-CV models, chosen by feature so the same
// codegen'd models run natively per platform: CUDA (NVIDIA, NVRTC), Metal
// (Apple), ROCm (AMD), or wgpu (Vulkan/DX12/WebGPU) as the universal fallback.
// The model architectures are vendored in `generated/` (build-time ONNX→Burn
// codegen); only the `.bpk` weights load at runtime.

#![cfg(feature = "vision-burn")]

#[cfg(feature = "vision-cuda")]
pub type VisionBackend = burn::backend::Cuda<f32, i32>;
#[cfg(feature = "vision-cuda")]
pub type VisionDevice = burn::backend::cuda::CudaDevice;

#[cfg(all(feature = "vision-metal", not(feature = "vision-cuda")))]
pub type VisionBackend = burn::backend::Metal<f32, i32>;
#[cfg(all(feature = "vision-metal", not(feature = "vision-cuda")))]
pub type VisionDevice = burn::backend::wgpu::WgpuDevice;

#[cfg(all(
    feature = "vision-rocm",
    not(any(feature = "vision-cuda", feature = "vision-metal"))
))]
pub type VisionBackend = burn::backend::Rocm<f32, i32>;
#[cfg(all(
    feature = "vision-rocm",
    not(any(feature = "vision-cuda", feature = "vision-metal"))
))]
pub type VisionDevice = burn::backend::rocm::RocmDevice;

#[cfg(not(any(feature = "vision-cuda", feature = "vision-metal", feature = "vision-rocm")))]
pub type VisionBackend = burn::backend::wgpu::Wgpu<f32, i32>;
#[cfg(not(any(feature = "vision-cuda", feature = "vision-metal", feature = "vision-rocm")))]
pub type VisionDevice = burn::backend::wgpu::WgpuDevice;

/// Default device for the selected backend.
pub fn device() -> VisionDevice {
    Default::default()
}

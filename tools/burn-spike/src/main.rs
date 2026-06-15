// Burn spike — RF-DETR on a selectable backend, to compare the universal path
// against ORT-CUDA. Build variants:
//   (default)        Burn-wgpu (Vulkan) f32
//   --features wgpuf16  Burn-wgpu (Vulkan) f16
//   --features cuda     Burn-CUDA (CubeCL/NVRTC) f32  — same model, NVIDIA JIT
mod rfdetr {
    include!(concat!(env!("OUT_DIR"), "/model/rfdetr-base.rs"));
}
use burn::tensor::Tensor;

#[cfg(feature = "cuda")]
mod backend {
    pub type B = burn::backend::Cuda<f32, i32>;
    pub fn device() -> burn::backend::cuda::CudaDevice { Default::default() }
    pub const NAME: &str = "Burn-CUDA (CubeCL/NVRTC) f32";
}
#[cfg(all(feature = "wgpuf16", not(feature = "cuda")))]
mod backend {
    pub type B = burn::backend::wgpu::Wgpu<burn::tensor::f16, i32>;
    pub fn device() -> burn::backend::wgpu::WgpuDevice { Default::default() }
    pub const NAME: &str = "Burn-wgpu/Vulkan f16";
}
#[cfg(all(feature = "vulkan", not(feature = "cuda")))]
mod backend {
    pub type B = burn::backend::Vulkan<f32, i32>;
    pub fn device() -> burn::backend::wgpu::WgpuDevice { Default::default() }
    pub const NAME: &str = "Burn-Vulkan (SPIR-V) f32";
}
#[cfg(not(any(feature = "cuda", feature = "wgpuf16", feature = "vulkan")))]
mod backend {
    pub type B = burn::backend::wgpu::Wgpu<f32, i32>;
    pub fn device() -> burn::backend::wgpu::WgpuDevice { Default::default() }
    pub const NAME: &str = "Burn-wgpu/Vulkan f32";
}

fn main() {
    let device = backend::device();
    let model: rfdetr::Model<backend::B> = rfdetr::Model::new(&device);
    let input = Tensor::<backend::B, 4>::zeros([1, 3, 560, 560], &device);

    // Warm up until the JIT autotuner settles.
    for _ in 0..15 {
        let (d, l) = model.forward(input.clone());
        let _ = d.to_data();
        let _ = l.to_data();
    }
    let mut ms: Vec<f64> = Vec::new();
    for _ in 0..20 {
        let t = std::time::Instant::now();
        let (d, l) = model.forward(input.clone());
        let _ = d.to_data();
        let _ = l.to_data();
        ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "{}: median {:.1} ms  min {:.1} ms  ({:.0} img/s)",
        backend::NAME, ms[ms.len() / 2], ms[0], 1000.0 / ms[ms.len() / 2]
    );
}

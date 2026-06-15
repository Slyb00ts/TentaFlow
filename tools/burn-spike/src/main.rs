// Burn vision spike / validation harness. No arg = RF-DETR timing; image path =
// run RF-DETR with real weights and print detections (validates the codegen).
// Backend via features: (default) wgpu, --features cuda|vulkan.
mod rfdetr {
    include!(concat!(env!("OUT_DIR"), "/model/rfdetr-base.rs"));
}
use burn::tensor::{Tensor, TensorData};

#[cfg(feature = "cuda")]
mod backend {
    pub type B = burn::backend::Cuda<f32, i32>;
    pub fn device() -> burn::backend::cuda::CudaDevice { Default::default() }
    pub const NAME: &str = "Burn-CUDA";
}
#[cfg(all(feature = "vulkan", not(feature = "cuda")))]
mod backend {
    pub type B = burn::backend::Vulkan<f32, i32>;
    pub fn device() -> burn::backend::wgpu::WgpuDevice { Default::default() }
    pub const NAME: &str = "Burn-Vulkan";
}
#[cfg(not(any(feature = "cuda", feature = "vulkan")))]
mod backend {
    pub type B = burn::backend::wgpu::Wgpu<f32, i32>;
    pub fn device() -> burn::backend::wgpu::WgpuDevice { Default::default() }
    pub const NAME: &str = "Burn-wgpu";
}

fn main() {
    let device = backend::device();
    let model: rfdetr::Model<backend::B> = rfdetr::Model::default();
    let input = Tensor::<backend::B, 4>::zeros([1, 3, 560, 560], &device);
    for _ in 0..10 {
        let (d, l) = model.forward(input.clone());
        let _ = d.to_data();
        let _ = l.to_data();
    }
    let t = std::time::Instant::now();
    let (d, _l) = model.forward(input.clone());
    let _ = d.to_data();
    println!("{}: {:.1} ms", backend::NAME, t.elapsed().as_secs_f64() * 1000.0);
}

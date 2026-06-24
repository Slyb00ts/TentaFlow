// Burn vision spike / validation harness. Times a forward pass of the vendored
// Depth-Anything-V2-Metric model (518×518 → metric depth) on the selected backend.
// Backend via features: (default) wgpu, --features cuda|vulkan.
mod depth {
    // The vendored, hand-patched model (grid 36→37) — portable across checkouts.
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tentaflow-core/src/vision/generated/depth_anything.rs"));
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
    let t_load = std::time::Instant::now();
    let model: depth::Model<backend::B> = depth::Model::default();
    println!("[{}] weights loaded in {:.0} ms", backend::NAME, t_load.elapsed().as_secs_f64() * 1000.0);
    let input = Tensor::<backend::B, 4>::zeros([1, 3, 518, 518], &device);

    // Warmup (kernel compile / autotune).
    let t_warm = std::time::Instant::now();
    for _ in 0..3 {
        let d = model.forward(input.clone());
        let _ = d.to_data();
    }
    println!("[{}] warmup (3 incl. autotune) {:.0} ms", backend::NAME, t_warm.elapsed().as_secs_f64() * 1000.0);

    // Timed steady-state: 10 runs, report avg + min.
    let mut times = Vec::new();
    for _ in 0..10 {
        let t = std::time::Instant::now();
        let d = model.forward(input.clone());
        let _ = d.to_data(); // force sync (read back)
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let avg = times.iter().sum::<f64>() / times.len() as f64;
    let min = times.iter().cloned().fold(f64::INFINITY, f64::min);
    println!("[{}] depth 518x518: avg {:.1} ms  min {:.1} ms (10 runs)", backend::NAME, avg, min);
}

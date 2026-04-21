// =============================================================================
// Plik: examples/system_check.rs
// Opis: Uruchamia detekcje srodowiska i wypisuje co maszyna potrafi uruchomic.
//       Uzycie: cargo run --example system_check --features inference-diarization,docker,dashboard-api
// =============================================================================

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    let caps = tentaflow_core::system_check::collect();

    println!("=== Platforma ===");
    println!("OS:   {} ({})", caps.platform, caps.arch);
    println!(
        "CPU:  {} logical, avx2={}, avx512={}, neon={}",
        caps.cpu_features.logical_cores,
        caps.cpu_features.avx2,
        caps.cpu_features.avx512,
        caps.cpu_features.neon
    );
    println!(
        "RAM:  {} MB total ({} available)",
        caps.memory.total_mb, caps.memory.available_mb
    );

    println!("\n=== GPU ===");
    println!("Preferowany backend: {:?}", caps.gpu.preferred_backend);
    if !caps.gpu.nvidia.is_empty() {
        for g in &caps.gpu.nvidia {
            println!(
                "  NVIDIA [{}]: {} ({} MB, CC {:?}, driver {:?}, CUDA {:?})",
                g.index, g.name, g.vram_mb, g.compute_capability, g.driver_version, g.cuda_version
            );
        }
    }
    if !caps.gpu.amd.is_empty() {
        for g in &caps.gpu.amd {
            println!(
                "  AMD [{}]: {} (ROCm {:?})",
                g.index, g.name, g.rocm_version
            );
        }
    }
    if !caps.gpu.intel.is_empty() {
        for g in &caps.gpu.intel {
            println!("  Intel: {} (oneAPI {:?})", g.name, g.oneapi_version);
        }
    }
    println!("  Metal:  {}", caps.gpu.metal_available);
    println!("  Vulkan: {}", caps.gpu.vulkan_available);

    println!("\n=== Runtimes ===");
    println!("docker: {:?}", caps.runtimes.docker);
    println!("podman: {:?}", caps.runtimes.podman);
    println!("python: {:?}", caps.runtimes.python);
    println!("nvcc:   {:?}", caps.runtimes.cuda_toolkit);

    println!("\n=== Silniki ===");
    for e in &caps.supported_engines {
        let mark = if e.available { "OK " } else { "--" };
        println!(
            " [{}] {:<16} ({}): {}",
            mark, e.engine, e.category, e.reason
        );
    }

    println!("\n=== Deploy backendy ===");
    for b in &caps.deploy_backends {
        println!("  - {:?}", b);
    }
}

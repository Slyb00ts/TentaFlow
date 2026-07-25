// ===== File: hip_backend.rs — HIP backend: caps, memory, module load, launch =====
// Test buduje code object w locie przez `hipcc --genco`, więc nie zależy od
// artefaktów Mojo ani od niczego w drzewie. Pomija się czysto, gdy nie ma
// urządzenia HIP albo ROCm.

use std::io::Write;
use std::process::Command;
use std::sync::Arc;

use forge_hal::hip::HipDevice;
use forge_hal::{Device, LaunchArgs, LaunchConfig, Pool};
use forge_types::MemKind;

fn device() -> Option<Arc<HipDevice>> {
    match HipDevice::new(0) {
        Ok(dev) => Some(dev),
        Err(err) => {
            eprintln!("pominięto: brak urządzenia HIP ({err})");
            None
        }
    }
}

fn rocm_path() -> String {
    std::env::var("ROCM_PATH").unwrap_or_else(|_| "/opt/rocm".to_string())
}

#[test]
fn hip_reports_device_caps() {
    let Some(dev) = device() else { return };
    let caps = dev.caps();
    assert_eq!(caps.vendor, forge_types::Vendor::Amd);
    assert!(
        caps.arch.starts_with("gfx"),
        "architektura powinna być nazwą gfx, jest {}",
        caps.arch
    );
    assert!(
        caps.warp_size == 32 || caps.warp_size == 64,
        "wavefront {}",
        caps.warp_size
    );
    assert!(caps.sm_count > 0 && caps.total_memory > (1 << 30));
    assert!(caps.max_threads_per_block >= 256);
    // RDNA nie ma potoku FP8/FP4, a grafy nie są jeszcze wpięte — bramki wyżej
    // opierają się na tych polach, więc muszą mówić prawdę.
    assert!(!caps.fp8_native && !caps.fp4_native && !caps.supports_graph_capture);
    eprintln!(
        "HIP: {} / {} / {} CU / wavefront {} / {} MiB",
        caps.name,
        caps.arch,
        caps.sm_count,
        caps.warp_size,
        caps.total_memory >> 20
    );
}

#[test]
fn hip_memory_roundtrip_and_device_copy() {
    let Some(dev) = device() else { return };
    let stream = dev.create_stream().unwrap();
    let source: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();

    let a = dev.alloc(source.len(), MemKind::Device, Pool::Activations).unwrap();
    let b = dev.alloc(source.len(), MemKind::Device, Pool::Activations).unwrap();
    dev.write(&source, &a, 0).unwrap();
    dev.copy(&a, 0, &b, 0, source.len(), &stream).unwrap();
    stream.synchronize().unwrap();

    let mut back = vec![0u8; source.len()];
    dev.read(&b, 0, &mut back).unwrap();
    assert_eq!(back, source, "kopia D2D nie odtworzyła danych");

    // Pinned host jest adresowalny z hosta i widoczny dla kerneli (UVA).
    let pinned = dev.alloc(64, MemKind::PinnedHost, Pool::Activations).unwrap();
    assert!(pinned.host_ptr().is_some());
    assert!(dev.read(&b, 0, &mut vec![0u8; source.len() + 1]).is_err());
}

#[test]
fn hip_loads_code_object_and_launches() {
    let Some(dev) = device() else { return };
    let rocm = rocm_path();
    let hipcc = format!("{rocm}/bin/hipcc");
    if !std::path::Path::new(&hipcc).is_file() {
        eprintln!("pominięto: brak {hipcc}");
        return;
    }
    let arch = dev.caps().arch.clone();
    let dir = std::env::temp_dir().join(format!("forge-hip-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("k.hip");
    let obj = dir.join("k.hsaco");
    let mut file = std::fs::File::create(&src).unwrap();
    file.write_all(
        br#"#include <hip/hip_runtime.h>
extern "C" __global__ void scale_add(float* out, const float* in, float k, int n) {
  int i = blockIdx.x * blockDim.x + threadIdx.x;
  if (i < n) out[i] = in[i] * k + 1.0f;
}
"#,
    )
    .unwrap();
    drop(file);

    let build = Command::new(&hipcc)
        .args(["--genco", &format!("--offload-arch={arch}")])
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .output()
        .expect("uruchomienie hipcc");
    assert!(
        build.status.success(),
        "hipcc --genco nie zbudował code objectu: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let image = std::fs::read(&obj).unwrap();
    let module = dev.load_module(&image).unwrap();
    let kernel = module.kernel("scale_add").unwrap();
    assert!(
        module.kernel("nie_ma_takiego").is_err(),
        "nieistniejący symbol musi być błędem, nie ciszą"
    );

    const N: usize = 1024;
    let input: Vec<f32> = (0..N).map(|i| i as f32 * 0.5).collect();
    let stream = dev.create_stream().unwrap();
    let din = dev.alloc(N * 4, MemKind::Device, Pool::Activations).unwrap();
    let dout = dev.alloc(N * 4, MemKind::Device, Pool::Activations).unwrap();
    dev.write(bytemuck_cast(&input), &din, 0).unwrap();

    let cfg = LaunchConfig {
        grid: ((N as u32).div_ceil(256), 1, 1),
        block: (256, 1, 1),
        shared_mem_bytes: 0,
    };
    let args = LaunchArgs::new()
        .buf(&dout)
        .buf(&din)
        .scalar(3.0f32)
        .scalar(N as i64);
    dev.launch(&kernel, &cfg, &args, &stream).unwrap();
    stream.synchronize().unwrap();

    let mut raw = vec![0u8; N * 4];
    dev.read(&dout, 0, &mut raw).unwrap();
    let got: Vec<f32> = raw
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    for (i, (value, source)) in got.iter().zip(&input).enumerate() {
        let want = source * 3.0 + 1.0;
        assert!(
            (value - want).abs() < 1e-5,
            "element {i}: {value} zamiast {want}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn bytemuck_cast(values: &[f32]) -> &[u8] {
    // SAFETY: f32 nie ma wypełnień, a długość jest przeliczana wprost.
    unsafe { std::slice::from_raw_parts(values.as_ptr() as *const u8, values.len() * 4) }
}

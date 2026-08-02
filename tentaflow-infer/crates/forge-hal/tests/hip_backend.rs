// ===== File: hip_backend.rs — HIP backend: caps, memory, module load, launch =====
// Gated on the feature it tests: without it the crate has no `hip` module and
// the whole test target failed to compile, which took the rest of the suite
// down with it on every machine that is not building for ROCm.
#![cfg(feature = "hip")]

// Test buduje code object w locie przez `hipcc --genco`, więc nie zależy od
// artefaktów Mojo ani od niczego w drzewie. Pomija się czysto, gdy nie ma
// urządzenia HIP albo ROCm.

use std::io::Write;
use std::process::Command;
use std::sync::Arc;

use forge_hal::hip::HipDevice;
use forge_hal::{Device, LaunchArgs, LaunchConfig, Pool, PoolSizes};
use forge_types::MemKind;

const TEST_POOLS: PoolSizes = PoolSizes {
    weights: 64 * 1024 * 1024,
    kv_cache: 32 * 1024 * 1024,
    kv_page_size: 256 * 1024,
    activations: 32 * 1024 * 1024,
};

fn device() -> Option<Arc<HipDevice>> {
    match HipDevice::new(0, TEST_POOLS) {
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
    assert!(!caps.fp8_native && !caps.fp4_native);
    assert!(caps.supports_graph_capture, "grafy HIP są wpięte");
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

#[test]
fn hip_pools_report_budgets_and_recycle_activations() {
    let Some(dev) = device() else { return };
    let weights_before = dev.pool_available(Pool::Weights).unwrap();
    let acts_before = dev.pool_available(Pool::Activations).unwrap();
    assert!(weights_before >= 60 * 1024 * 1024 && acts_before >= 30 * 1024 * 1024);

    // Bump: pula wag nie oddaje pamięci po zwolnieniu bufora.
    let w = dev.alloc(1 << 20, MemKind::Device, Pool::Weights).unwrap();
    let after_alloc = dev.pool_available(Pool::Weights).unwrap();
    assert!(after_alloc < weights_before);
    drop(w);
    assert_eq!(dev.pool_available(Pool::Weights).unwrap(), after_alloc);

    // Pierścień: aktywacje wracają dopiero po `reset_activations`.
    let a = dev.alloc(1 << 20, MemKind::Device, Pool::Activations).unwrap();
    assert!(dev.pool_available(Pool::Activations).unwrap() < acts_before);
    drop(a);
    dev.reset_activations().unwrap();
    assert_eq!(dev.pool_available(Pool::Activations).unwrap(), acts_before);

    // Wyczerpanie puli musi być typowanym OutOfMemory, nie ogólnym błędem.
    let err = dev
        .alloc(TEST_POOLS.kv_cache + 1, MemKind::Device, Pool::KvCache)
        .unwrap_err();
    assert!(
        matches!(err, forge_types::ForgeError::OutOfMemory { .. }),
        "otrzymano {err:?}"
    );
}

#[test]
fn hip_captures_and_replays_a_graph() {
    let Some(dev) = device() else { return };
    let Some((module, _dir)) = build_test_module(&dev) else { return };
    let kernel = module.kernel("scale_add").unwrap();

    const N: usize = 256;
    let input: Vec<f32> = (0..N).map(|i| i as f32).collect();
    let stream = dev.create_stream().unwrap();
    let din = dev.alloc(N * 4, MemKind::Device, Pool::Activations).unwrap();
    let dout = dev.alloc(N * 4, MemKind::Device, Pool::Activations).unwrap();
    dev.write(bytemuck_cast(&input), &din, 0).unwrap();

    let cfg = LaunchConfig {
        grid: ((N as u32).div_ceil(64), 1, 1),
        block: (64, 1, 1),
        shared_mem_bytes: 0,
    };
    dev.begin_capture(&stream).unwrap();
    dev.launch(
        &kernel,
        &cfg,
        &LaunchArgs::new()
            .buf(&dout)
            .buf(&din)
            .scalar(2.0f32)
            .scalar(N as i64),
        &stream,
    )
    .unwrap();
    let graph = dev.end_capture(&stream).unwrap();

    // Dwa odtworzenia z tego samego grafu muszą dać ten sam wynik.
    for _ in 0..2 {
        dev.launch_graph(&graph, &stream).unwrap();
        stream.synchronize().unwrap();
        let mut raw = vec![0u8; N * 4];
        dev.read(&dout, 0, &mut raw).unwrap();
        let got: Vec<f32> = raw
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        for (i, (value, source)) in got.iter().zip(&input).enumerate() {
            let want = source * 2.0 + 1.0;
            assert!((value - want).abs() < 1e-5, "element {i}: {value} != {want}");
        }
    }
}

/// Buduje moduł testowy przez `hipcc --genco`; `None` = brak ROCm.
fn build_test_module(dev: &Arc<HipDevice>) -> Option<(forge_hal::Module, std::path::PathBuf)> {
    let hipcc = format!("{}/bin/hipcc", rocm_path());
    if !std::path::Path::new(&hipcc).is_file() {
        eprintln!("pominięto: brak {hipcc}");
        return None;
    }
    let arch = dev.caps().arch.clone();
    let dir = std::env::temp_dir().join(format!("forge-hip-graph-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("k.hip");
    let obj = dir.join("k.hsaco");
    std::fs::write(
        &src,
        br#"#include <hip/hip_runtime.h>
extern "C" __global__ void scale_add(float* out, const float* in, float k, int n) {
  int i = blockIdx.x * blockDim.x + threadIdx.x;
  if (i < n) out[i] = in[i] * k + 1.0f;
}
"#,
    )
    .unwrap();
    let build = Command::new(&hipcc)
        .args(["--genco", &format!("--offload-arch={arch}")])
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .output()
        .expect("uruchomienie hipcc");
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));
    let image = std::fs::read(&obj).unwrap();
    Some((dev.load_module(&image).unwrap(), dir))
}

/// Uruchamia kernel z artefaktu ZBUDOWANEGO PRZEZ MOJO, a nie przez `hipcc`.
///
/// Rozróżnienie nie jest akademickie: te dwa code objecty mają inne metadane
/// argumentów (Mojo nie emituje ukrytych argumentów HIP), a `hipModuleLaunchKernel`
/// pakuje kernarg właśnie po metadanych. Ten test jest kontraktem między
/// generatorem artefaktów a backendem — jeśli pęknie, silnik przestaje ruszać na
/// AMD, choć test z `hipcc` przechodzi.
#[test]
fn mojo_artifact_launches() {
    let Some(dev) = device() else { return };
    let arch = dev.caps().arch.clone();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../kernels/mojo/build")
        .join(&arch);
    let manifest_path = dir.join("manifest.json");
    if !manifest_path.is_file() {
        eprintln!("pominięto: brak katalogu kerneli dla {arch}");
        return;
    }
    // `gather_rows_f16(out, table, ids, n_rows, n_cols)` — pierwszy kernel, jaki
    // uruchamia silnik, i zarazem najprostszy do sprawdzenia bez matematyki.
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    let entry = &manifest["kernels"]["gather_rows_f16"];
    if entry.is_null() {
        eprintln!("pominięto: katalog {arch} nie ma gather_rows_f16");
        return;
    }
    let image = std::fs::read(dir.join(entry["file"].as_str().unwrap())).unwrap();
    // Uchwyt MUSI przeżyć porzucenie `Module` — dokładnie tak robi rejestr
    // kerneli, który trzyma tylko uchwyty.
    let kernel = {
        let module = dev.load_module(&image).unwrap();
        module.kernel(entry["entry"].as_str().unwrap()).unwrap()
    };

    let cols = 8usize;
    let rows = 4usize;
    let table = dev
        .alloc(rows * cols * 2, MemKind::Device, Pool::Weights)
        .unwrap();
    let ids = dev.alloc(2 * 4, MemKind::Device, Pool::Weights).unwrap();
    let out = dev
        .alloc(2 * cols * 2, MemKind::Device, Pool::Weights)
        .unwrap();
    // Wiersz 2 tabeli wypełniony wartością 1.0 w f16 (0x3C00), reszta zerami.
    let mut host = vec![0u8; rows * cols * 2];
    for i in 0..cols {
        host[(2 * cols + i) * 2] = 0x00;
        host[(2 * cols + i) * 2 + 1] = 0x3C;
    }
    dev.write(&host, &table, 0).unwrap();
    dev.write(&2u32.to_le_bytes(), &ids, 0).unwrap();
    dev.write(&2u32.to_le_bytes(), &ids, 4).unwrap();
    let stream = dev.create_stream().unwrap();

    let cfg = LaunchConfig {
        grid: (2, 1, 1),
        block: (cols as u32, 1, 1),
        shared_mem_bytes: 0,
    };
    let args = LaunchArgs::new()
        .buf(&out)
        .buf(&table)
        .buf(&ids)
        .scalar(rows as i64)
        .scalar(cols as i64);
    // Silnik uruchamia kernele z wątku roboczego, a wybór urządzenia w HIP jest
    // stanem wątku — dlatego launch idzie tu z osobnego wątku, nie z głównego.
    std::thread::scope(|scope| {
        scope.spawn(|| {
            dev.launch(&kernel, &cfg, &args, &stream).unwrap();
            dev.synchronize().unwrap();
        });
    });

    let mut got = vec![0u8; 2 * cols * 2];
    dev.read(&out, 0, &mut got).unwrap();
    for token in 0..2 {
        for i in 0..cols {
            let bits = u16::from_le_bytes([got[(token * cols + i) * 2], got[(token * cols + i) * 2 + 1]]);
            assert_eq!(bits, 0x3C00, "token {token}, element {i}: {bits:#06x}");
        }
    }
}

/// Ładuje CAŁY katalog kerneli i zwalnia go — silnik robi dokładnie to przy
/// starcie i rozbiórce modelu. Test istnieje, bo `hipModuleUnload` powtórzone
/// kilkaset razy potrafi rozjechać stertę hosta, a objaw pojawia się dopiero
/// przy wychodzeniu z procesu i wygląda wtedy na błąd zupełnie innej warstwy.
#[test]
fn whole_catalog_loads_and_unloads() {
    let Some(dev) = device() else { return };
    let arch = dev.caps().arch.clone();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../kernels/mojo/build")
        .join(&arch);
    let manifest_path = dir.join("manifest.json");
    if !manifest_path.is_file() {
        eprintln!("pominięto: brak katalogu kerneli dla {arch}");
        return;
    }
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    let kernels = manifest["kernels"].as_object().unwrap();
    let mut handles = Vec::with_capacity(kernels.len());
    for (name, entry) in kernels {
        let image = std::fs::read(dir.join(entry["file"].as_str().unwrap())).unwrap();
        let module = dev.load_module(&image).unwrap();
        handles.push(
            module
                .kernel(entry["entry"].as_str().unwrap())
                .unwrap_or_else(|e| panic!("{name}: {e}")),
        );
    }
    eprintln!("załadowano {} kerneli dla {arch}", handles.len());
    drop(handles);
    dev.synchronize().unwrap();
}

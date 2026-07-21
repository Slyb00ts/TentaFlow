// ===== File: cuda_backend.rs — CUDA backend tests: alloc/copy roundtrips, PTX saxpy, events, arena reuse, graph capture =====
//
// Every test skips gracefully (with a stderr note) when no CUDA device is
// present, so the suite stays green on GPU-less CI while fully exercising the
// backend on real hardware.

#![cfg(feature = "cuda")]

use std::sync::Arc;

use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::{Device, LaunchArgs, LaunchConfig, Pool};
use forge_types::{ForgeError, MemKind, Vendor};

const TEST_PTX: &[u8] = include_bytes!("kernels/hal_test_kernels.ptx");

const TEST_POOLS: PoolSizes = PoolSizes {
    weights: 32 * 1024 * 1024,
    kv_cache: 16 * 1024 * 1024,
    kv_page_size: 64 * 1024,
    activations: 16 * 1024 * 1024,
};

fn device() -> Option<Arc<CudaDevice>> {
    match CudaDevice::new(0, TEST_POOLS) {
        Ok(dev) => Some(dev),
        Err(e) => {
            eprintln!("skipping CUDA test: no usable CUDA device ({e})");
            None
        }
    }
}

fn write_f32(dev: &CudaDevice, buf: &forge_hal::DevBuffer, data: &[f32]) {
    let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
    dev.write(&bytes, buf, 0).unwrap();
}

fn read_f32(dev: &CudaDevice, buf: &forge_hal::DevBuffer, n: usize) -> Vec<f32> {
    let mut bytes = vec![0u8; n * 4];
    dev.read(buf, 0, &mut bytes).unwrap();
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

#[test]
fn caps_report() {
    let Some(dev) = device() else { return };
    let caps = dev.caps();
    assert_eq!(caps.vendor, Vendor::Nvidia);
    assert!(caps.arch.starts_with("sm_"));
    assert_eq!(caps.warp_size, 32);
    assert!(caps.total_memory > 1024 * 1024 * 1024);
    assert!(caps.max_shared_mem_per_block >= 48 * 1024);
    assert!(caps.supports_graph_capture);
    let sm: u32 = caps.arch.trim_start_matches("sm_").parse().unwrap();
    assert_eq!(caps.bf16_native, sm >= 80);
    assert_eq!(caps.fp8_native, sm >= 89);
    assert_eq!(caps.fp4_native, sm >= 100);
}

#[test]
fn h2d_d2h_roundtrip_async() {
    let Some(dev) = device() else { return };
    let stream = dev.create_stream().unwrap();
    let n = 4096usize;

    let staging_in = dev.alloc(n, MemKind::PinnedHost, Pool::Activations).unwrap();
    let staging_out = dev.alloc(n, MemKind::PinnedHost, Pool::Activations).unwrap();
    let device_a = dev.alloc(n, MemKind::Device, Pool::KvCache).unwrap();
    let device_b = dev.alloc(n, MemKind::Device, Pool::KvCache).unwrap();

    let payload: Vec<u8> = (0..n).map(|i| (i * 31 % 251) as u8).collect();
    dev.write(&payload, &staging_in, 0).unwrap();

    // Pinned → device → device → pinned, all stream-ordered.
    dev.copy(&staging_in, 0, &device_a, 0, n, &stream).unwrap();
    dev.copy(&device_a, 0, &device_b, 0, n, &stream).unwrap();
    dev.copy(&device_b, 0, &staging_out, 0, n, &stream).unwrap();
    stream.synchronize().unwrap();

    let mut out = vec![0u8; n];
    dev.read(&staging_out, 0, &mut out).unwrap();
    assert_eq!(out, payload);
}

#[test]
fn saxpy_kernel_launch() {
    let Some(dev) = device() else { return };
    let stream = dev.create_stream().unwrap();
    let module = dev.load_module(TEST_PTX).unwrap();
    let saxpy = module.kernel("saxpy").unwrap();
    assert_eq!(saxpy.name(), "saxpy");

    let n = 1024u32;
    let x = dev
        .alloc(n as usize * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    let y = dev
        .alloc(n as usize * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    let x_host: Vec<f32> = (0..n).map(|i| i as f32).collect();
    let y_host: Vec<f32> = (0..n).map(|i| (i as f32) * 0.5).collect();
    write_f32(&dev, &x, &x_host);
    write_f32(&dev, &y, &y_host);

    let args = LaunchArgs::new().scalar(2.0f32).buf(&x).buf(&y).scalar(n);
    dev.launch(&saxpy, &LaunchConfig::linear(n, 256), &args, &stream)
        .unwrap();
    stream.synchronize().unwrap();

    let result = read_f32(&dev, &y, n as usize);
    for i in 0..n as usize {
        let expected = 2.0 * x_host[i] + y_host[i];
        assert_eq!(result[i], expected, "mismatch at {i}");
    }
}

#[test]
fn event_ordering_across_streams() {
    let Some(dev) = device() else { return };
    let producer = dev.create_stream().unwrap();
    let consumer = dev.create_stream().unwrap();
    let module = dev.load_module(TEST_PTX).unwrap();
    let scale2 = module.kernel("scale2").unwrap();
    let add3 = module.kernel("add3").unwrap();

    let n = 256u32;
    let x = dev
        .alloc(n as usize * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    write_f32(&dev, &x, &vec![1.0f32; n as usize]);

    // producer: x *= 2; consumer waits on the event, then x += 3.
    // Correct ordering yields 5.0; a race could observe 4.0 (add-then-scale).
    let event = dev.create_event().unwrap();
    let cfg = LaunchConfig::linear(n, 128);
    dev.launch(&scale2, &cfg, &LaunchArgs::new().buf(&x).scalar(n), &producer)
        .unwrap();
    dev.record_event(&event, &producer).unwrap();
    dev.wait_event(&consumer, &event).unwrap();
    dev.launch(&add3, &cfg, &LaunchArgs::new().buf(&x).scalar(n), &consumer)
        .unwrap();
    consumer.synchronize().unwrap();

    assert!(event.is_complete().unwrap());
    event.synchronize().unwrap();
    let result = read_f32(&dev, &x, n as usize);
    assert!(result.iter().all(|&v| v == 5.0), "ordering violated: {result:?}");
}

#[test]
fn kv_arena_reuses_freed_page() {
    let Some(dev) = device() else { return };
    let a = dev
        .alloc(TEST_POOLS.kv_page_size, MemKind::Device, Pool::KvCache)
        .unwrap();
    let a_ptr = a.device_ptr();
    let b = dev
        .alloc(TEST_POOLS.kv_page_size, MemKind::Device, Pool::KvCache)
        .unwrap();
    assert_ne!(a_ptr, b.device_ptr());
    drop(a);
    // Alloc-free-alloc must return the same page (free-list reuse, no growth).
    let c = dev
        .alloc(TEST_POOLS.kv_page_size, MemKind::Device, Pool::KvCache)
        .unwrap();
    assert_eq!(a_ptr, c.device_ptr());
}

#[test]
fn activations_ring_generation_reset() {
    let Some(dev) = device() else { return };
    let a = dev.alloc(1024, MemKind::Device, Pool::Activations).unwrap();
    let a_ptr = a.device_ptr();
    // Reset refuses while generation-live buffers exist.
    assert!(matches!(
        dev.reset_activations(),
        Err(ForgeError::Device(_))
    ));
    drop(a);
    let generation = dev.reset_activations().unwrap();
    assert!(generation >= 1);
    // Cursor rewound: the next allocation reuses the same address.
    let b = dev.alloc(1024, MemKind::Device, Pool::Activations).unwrap();
    assert_eq!(a_ptr, b.device_ptr());
    drop(b);
    dev.reset_activations().unwrap();
}

#[test]
fn weights_pool_is_bump_only() {
    let Some(dev) = device() else { return };
    let a = dev.alloc(1024, MemKind::Device, Pool::Weights).unwrap();
    let a_ptr = a.device_ptr();
    drop(a);
    // Bump pool never recycles: dropping a weights buffer must not hand the
    // address back out.
    let b = dev.alloc(1024, MemKind::Device, Pool::Weights).unwrap();
    assert_ne!(a_ptr, b.device_ptr());
}

#[test]
fn pool_exhaustion_reports_oom() {
    let Some(dev) = device() else { return };
    let err = dev
        .alloc(TEST_POOLS.kv_cache + 1, MemKind::Device, Pool::KvCache)
        .unwrap_err();
    assert!(matches!(err, ForgeError::OutOfMemory { .. }));
}

#[test]
fn graph_capture_and_replay() {
    let Some(dev) = device() else { return };
    let stream = dev.create_stream().unwrap();
    let module = dev.load_module(TEST_PTX).unwrap();
    let scale2 = module.kernel("scale2").unwrap();
    let add3 = module.kernel("add3").unwrap();

    let n = 512u32;
    let x = dev
        .alloc(n as usize * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    write_f32(&dev, &x, &vec![1.0f32; n as usize]);

    // Capture two dependent kernel launches: x = x * 2 + 3.
    let cfg = LaunchConfig::linear(n, 128);
    dev.begin_capture(&stream).unwrap();
    dev.launch(&scale2, &cfg, &LaunchArgs::new().buf(&x).scalar(n), &stream)
        .unwrap();
    dev.launch(&add3, &cfg, &LaunchArgs::new().buf(&x).scalar(n), &stream)
        .unwrap();
    let graph = dev.end_capture(&stream).unwrap();

    // Capture only records; the data is untouched until replay.
    assert!(read_f32(&dev, &x, 1)[0] == 1.0);

    // Replay twice: 1 → 5 → 13.
    dev.launch_graph(&graph, &stream).unwrap();
    stream.synchronize().unwrap();
    assert!(read_f32(&dev, &x, n as usize).iter().all(|&v| v == 5.0));

    dev.launch_graph(&graph, &stream).unwrap();
    stream.synchronize().unwrap();
    assert!(read_f32(&dev, &x, n as usize).iter().all(|&v| v == 13.0));
}

#[test]
fn graph_survives_handle_drop_during_replay() {
    let Some(dev) = device() else { return };
    let stream = dev.create_stream().unwrap();
    let module = dev.load_module(TEST_PTX).unwrap();
    let scale2 = module.kernel("scale2").unwrap();

    let n = 4096u32;
    let x = dev
        .alloc(n as usize * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    write_f32(&dev, &x, &vec![1.0f32; n as usize]);

    let cfg = LaunchConfig::linear(n, 128);
    dev.begin_capture(&stream).unwrap();
    // Enough sequential work that the replay is still in flight when the
    // ExecGraph handle drops below.
    for _ in 0..64 {
        dev.launch(&scale2, &cfg, &LaunchArgs::new().buf(&x).scalar(n), &stream)
            .unwrap();
    }
    let graph = dev.end_capture(&stream).unwrap();

    // Dropping the last handle right after launch must not free the retained
    // buffers/kernels while the asynchronous replay still uses them.
    dev.launch_graph(&graph, &stream).unwrap();
    drop(graph);
    stream.synchronize().unwrap();

    let expected = 2.0f32.powi(64);
    assert!(read_f32(&dev, &x, n as usize).iter().all(|&v| v == expected));
    // Device-level synchronize prunes the pending-launch retention list.
    dev.synchronize().unwrap();
}

#[test]
fn kernel_and_graph_survive_module_drop() {
    let Some(dev) = device() else { return };
    let stream = dev.create_stream().unwrap();
    let module = dev.load_module(TEST_PTX).unwrap();
    let add3 = module.kernel("add3").unwrap();
    // The kernel handle must pin the module image: unloading it would leave
    // the CUfunction dangling.
    drop(module);

    let n = 256u32;
    let x = dev
        .alloc(n as usize * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    write_f32(&dev, &x, &vec![1.0f32; n as usize]);
    let cfg = LaunchConfig::linear(n, 128);
    dev.launch(&add3, &cfg, &LaunchArgs::new().buf(&x).scalar(n), &stream)
        .unwrap();
    stream.synchronize().unwrap();
    assert!(read_f32(&dev, &x, n as usize).iter().all(|&v| v == 4.0));

    // A captured graph must in turn pin the kernels (and thus modules) it
    // references, so replay works after every direct handle is gone.
    dev.begin_capture(&stream).unwrap();
    dev.launch(&add3, &cfg, &LaunchArgs::new().buf(&x).scalar(n), &stream)
        .unwrap();
    let graph = dev.end_capture(&stream).unwrap();
    drop(add3);
    dev.launch_graph(&graph, &stream).unwrap();
    stream.synchronize().unwrap();
    assert!(read_f32(&dev, &x, n as usize).iter().all(|&v| v == 7.0));
}

#[test]
fn bounds_check_rejects_offset_overflow() {
    let Some(dev) = device() else { return };
    let stream = dev.create_stream().unwrap();
    let a = dev.alloc(64, MemKind::Device, Pool::KvCache).unwrap();
    let b = dev.alloc(64, MemKind::Device, Pool::KvCache).unwrap();
    // `offset + bytes` wrapping past usize::MAX must fail the bound, not
    // slip through and issue an out-of-range CUDA copy.
    assert!(dev.copy(&a, usize::MAX - 8, &b, 0, 64, &stream).is_err());
    assert!(dev.copy(&a, 0, &b, usize::MAX - 8, 64, &stream).is_err());
    assert!(dev.write(&[0u8; 16], &a, usize::MAX - 8).is_err());
    let mut out = [0u8; 16];
    assert!(dev.read(&a, usize::MAX - 8, &mut out).is_err());
}

#[test]
fn cross_device_handles_rejected() {
    let Some(dev) = device() else { return };
    let Ok(other) = CudaDevice::new(1, TEST_POOLS) else {
        eprintln!("skipping cross-device test: no second CUDA device");
        return;
    };
    let stream = dev.create_stream().unwrap();
    let local = dev.alloc(64, MemKind::Device, Pool::Weights).unwrap();
    let foreign = other.alloc(64, MemKind::Device, Pool::Weights).unwrap();
    let foreign_stream = other.create_stream().unwrap();
    let foreign_event = other.create_event().unwrap();
    // Same backend type, different device: the downcast passes but the
    // owning-context check must reject the mix.
    assert!(dev.copy(&foreign, 0, &local, 0, 64, &stream).is_err());
    assert!(dev.copy(&local, 0, &local, 0, 64, &foreign_stream).is_err());
    assert!(dev.write(&[0u8; 64], &foreign, 0).is_err());
    assert!(dev.record_event(&foreign_event, &stream).is_err());
    assert!(dev.begin_capture(&foreign_stream).is_err());
}

#[test]
fn end_capture_without_begin_errors() {
    let Some(dev) = device() else { return };
    let stream = dev.create_stream().unwrap();
    assert!(dev.end_capture(&stream).is_err());
}

#[test]
fn cross_backend_handles_rejected() {
    let Some(dev) = device() else { return };
    let cpu = forge_hal::cpu::CpuDevice::new();
    let cpu_buf = cpu.alloc(64, MemKind::Device, Pool::Weights).unwrap();
    let cuda_buf = dev.alloc(64, MemKind::Device, Pool::Weights).unwrap();
    let stream = dev.create_stream().unwrap();
    assert!(dev.copy(&cpu_buf, 0, &cuda_buf, 0, 64, &stream).is_err());
}

#[test]
fn pinned_and_managed_are_host_visible() {
    let Some(dev) = device() else { return };
    let pinned = dev.alloc(64, MemKind::PinnedHost, Pool::Weights).unwrap();
    let managed = dev.alloc(64, MemKind::Managed, Pool::Weights).unwrap();
    assert!(pinned.host_ptr().is_some());
    assert!(managed.host_ptr().is_some());
    let device_buf = dev.alloc(64, MemKind::Device, Pool::Weights).unwrap();
    assert!(device_buf.host_ptr().is_none());
}

#[test]
fn auto_pool_sizing_stays_within_free_vram() {
    let Some(dev) = device() else { return };
    let (free, _total) = dev.mem_info().unwrap();
    let sizes = PoolSizes::auto_from_free(free);
    let total = sizes.weights + sizes.kv_cache + sizes.activations;
    assert!(total <= free / 10 * 9);
    assert!(sizes.weights > sizes.kv_cache);
    assert!(sizes.kv_cache > sizes.activations);
}

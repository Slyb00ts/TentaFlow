// ===== File: cpu_backend.rs — CPU backend contract tests (always run, no GPU required) =====

use forge_hal::cpu::CpuDevice;
use forge_hal::{Device, LaunchArgs, LaunchConfig, Pool};
use forge_types::{ForgeError, MemKind, Vendor};

#[test]
fn caps_report() {
    let dev = CpuDevice::new();
    let caps = dev.caps();
    assert_eq!(caps.vendor, Vendor::Cpu);
    assert!(!caps.supports_graph_capture);
    assert_eq!(caps.warp_size, 1);
    #[cfg(target_os = "linux")]
    assert!(caps.total_memory > 0);
}

#[test]
fn write_read_roundtrip() {
    let dev = CpuDevice::new();
    let buf = dev.alloc(1024, MemKind::Device, Pool::Weights).unwrap();
    assert_eq!(buf.len(), 1024);
    // 64-byte alignment promised for SIMD-friendly host tensors.
    assert_eq!(buf.device_ptr() % 64, 0);

    let data: Vec<u8> = (0..255u8).collect();
    dev.write(&data, &buf, 128).unwrap();
    let mut out = vec![0u8; data.len()];
    dev.read(&buf, 128, &mut out).unwrap();
    assert_eq!(out, data);
}

#[test]
fn copy_between_buffers() {
    let dev = CpuDevice::new();
    let stream = dev.create_stream().unwrap();
    let src = dev
        .alloc(256, MemKind::PinnedHost, Pool::Activations)
        .unwrap();
    let dst = dev.alloc(256, MemKind::Device, Pool::Activations).unwrap();

    let payload = vec![0xABu8; 200];
    dev.write(&payload, &src, 0).unwrap();
    dev.copy(&src, 0, &dst, 56, 200, &stream).unwrap();
    stream.synchronize().unwrap();

    let mut out = vec![0u8; 200];
    dev.read(&dst, 56, &mut out).unwrap();
    assert_eq!(out, payload);
}

#[test]
fn copy_rejects_out_of_bounds() {
    let dev = CpuDevice::new();
    let stream = dev.create_stream().unwrap();
    let a = dev.alloc(64, MemKind::Device, Pool::KvCache).unwrap();
    let b = dev.alloc(64, MemKind::Device, Pool::KvCache).unwrap();
    assert!(dev.copy(&a, 32, &b, 0, 64, &stream).is_err());
    assert!(dev.write(&[0u8; 65], &a, 0).is_err());
}

#[test]
fn bounds_check_rejects_offset_overflow() {
    let dev = CpuDevice::new();
    let stream = dev.create_stream().unwrap();
    let a = dev.alloc(64, MemKind::Device, Pool::KvCache).unwrap();
    let b = dev.alloc(64, MemKind::Device, Pool::KvCache).unwrap();
    // `offset + bytes` wrapping past usize::MAX must fail the bound, not
    // slip through into raw pointer arithmetic.
    assert!(dev.copy(&a, usize::MAX - 8, &b, 0, 64, &stream).is_err());
    assert!(dev.copy(&a, 0, &b, usize::MAX - 8, 64, &stream).is_err());
    assert!(dev.write(&[0u8; 16], &a, usize::MAX - 8).is_err());
    let mut out = [0u8; 16];
    assert!(dev.read(&a, usize::MAX - 8, &mut out).is_err());
}

#[test]
fn events_are_immediately_complete() {
    let dev = CpuDevice::new();
    let stream = dev.create_stream().unwrap();
    let event = dev.create_event().unwrap();
    dev.record_event(&event, &stream).unwrap();
    dev.wait_event(&stream, &event).unwrap();
    assert!(event.is_complete().unwrap());
    event.synchronize().unwrap();
}

#[test]
fn launch_and_capture_are_unsupported() {
    let dev = CpuDevice::new();
    let stream = dev.create_stream().unwrap();
    assert!(matches!(
        dev.load_module(b"not-a-module"),
        Err(ForgeError::Unsupported(_))
    ));
    assert!(matches!(
        dev.begin_capture(&stream),
        Err(ForgeError::Unsupported(_))
    ));
    // A KernelHandle cannot exist for the CPU backend (load_module refuses),
    // so exercising `launch` requires none; the unsupported surface above is
    // the complete contract.
    let _ = (LaunchArgs::new(), LaunchConfig::linear(1, 1));
}

#[test]
fn host_ptr_visible_for_all_kinds() {
    let dev = CpuDevice::new();
    for kind in [MemKind::Device, MemKind::PinnedHost, MemKind::Managed] {
        let buf = dev.alloc(16, kind, Pool::Weights).unwrap();
        assert!(buf.host_ptr().is_some());
        assert_eq!(buf.kind(), kind);
    }
}

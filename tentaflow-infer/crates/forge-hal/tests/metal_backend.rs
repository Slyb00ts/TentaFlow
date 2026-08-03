// ===== File: metal_backend.rs — the Metal backend through the public Device trait =====
//
// Exercised the way the engine uses it: allocate, write, load a module, launch
// on a stream, synchronize, read back. Skips cleanly when no Metal device is
// present so the suite stays green elsewhere.
#![cfg(all(feature = "metal", target_os = "macos"))]

use std::sync::Arc;

use forge_hal::metal_device::MetalDevice;
use forge_hal::{Device, LaunchArgs, LaunchConfig, Pool};
use forge_types::{MemKind, Vendor};

const SRC: &str = r#"
    #include <metal_stdlib>
    using namespace metal;
    kernel void scale_add(device float* out [[buffer(0)]],
                          device const float* in [[buffer(1)]],
                          constant float& k [[buffer(2)]],
                          constant uint& n [[buffer(3)]],
                          uint gid [[thread_position_in_grid]]) {
        if (gid < n) { out[gid] = in[gid] * k + 1.0f; }
    }
"#;

fn device() -> Option<Arc<MetalDevice>> {
    match MetalDevice::new() {
        Ok(dev) => Some(dev),
        Err(e) => {
            eprintln!("pomijam test Metal: {e}");
            None
        }
    }
}

fn write_f32(dev: &dyn Device, buf: &forge_hal::DevBuffer, values: &[f32]) {
    let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    dev.write(&bytes, buf, 0).unwrap();
}

fn read_f32(dev: &dyn Device, buf: &forge_hal::DevBuffer, n: usize) -> Vec<f32> {
    let mut bytes = vec![0u8; n * 4];
    dev.read(buf, 0, &mut bytes).unwrap();
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

#[test]
fn caps_report_what_was_measured_not_what_was_hoped() {
    let Some(dev) = device() else { return };
    let caps = dev.caps();
    assert_eq!(caps.vendor, Vendor::Apple);
    assert_eq!(caps.warp_size, 32);
    assert!(caps.max_threads_per_block >= 256);
    assert!(caps.total_memory > 0);
    // Zmierzone w EKS-A2: fp8 jest emulowany, fp4 nie istnieje, bf16 kosztuje
    // tyle co f16. Rejestr możliwości ma mówić prawdę, a nie życzenia.
    assert!(!caps.fp8_native);
    assert!(!caps.fp4_native);
    assert!(caps.bf16_native);
    assert!(!caps.supports_graph_capture);
}

#[test]
fn allocation_is_host_visible_because_memory_is_unified() {
    let Some(dev) = device() else { return };
    let buf = dev.alloc(256, MemKind::Device, Pool::Weights).unwrap();
    assert!(
        buf.host_ptr().is_some(),
        "na pamięci unified alokacja urządzenia JEST pamięcią hosta"
    );
    write_f32(dev.as_ref(), &buf, &[1.0, 2.0, 3.0, 4.0]);
    assert_eq!(read_f32(dev.as_ref(), &buf, 4), vec![1.0, 2.0, 3.0, 4.0]);
}

#[test]
fn launches_a_kernel_through_the_trait() {
    let Some(dev) = device() else { return };
    let n = 1024usize;
    let input = dev.alloc(n * 4, MemKind::Device, Pool::Weights).unwrap();
    let output = dev.alloc(n * 4, MemKind::Device, Pool::Weights).unwrap();
    let values: Vec<f32> = (0..n).map(|i| i as f32).collect();
    write_f32(dev.as_ref(), &input, &values);

    let module = dev.load_module(SRC.as_bytes()).unwrap();
    let kernel = module.kernel("scale_add").unwrap();
    let stream = dev.create_stream().unwrap();

    let args = LaunchArgs::new()
        .buf(&output)
        .buf(&input)
        .scalar(3.0f32)
        .scalar(n as u32);
    dev.launch(&kernel, &LaunchConfig::linear(n as u32, 256), &args, &stream)
        .unwrap();
    stream.synchronize().unwrap();

    let got = read_f32(dev.as_ref(), &output, n);
    for (i, v) in got.iter().enumerate() {
        assert_eq!(*v, i as f32 * 3.0 + 1.0, "element {i}");
    }
}

#[test]
fn many_launches_ride_one_command_buffer() {
    // The property the backend exists for: several launches, one submission.
    // Correctness here also proves the launches stay ordered.
    let Some(dev) = device() else { return };
    let n = 256usize;
    let a = dev.alloc(n * 4, MemKind::Device, Pool::Weights).unwrap();
    let b = dev.alloc(n * 4, MemKind::Device, Pool::Weights).unwrap();
    write_f32(dev.as_ref(), &a, &vec![1.0f32; n]);

    let module = dev.load_module(SRC.as_bytes()).unwrap();
    let kernel = module.kernel("scale_add").unwrap();
    let stream = dev.create_stream().unwrap();
    let cfg = LaunchConfig::linear(n as u32, 64);

    for (dst, src) in [(&b, &a), (&a, &b), (&b, &a)] {
        let args = LaunchArgs::new()
            .buf(dst)
            .buf(src)
            .scalar(2.0f32)
            .scalar(n as u32);
        dev.launch(&kernel, &cfg, &args, &stream).unwrap();
    }
    stream.synchronize().unwrap();

    // 1 -> 3 -> 7 -> 15
    assert!(read_f32(dev.as_ref(), &b, n).iter().all(|v| *v == 15.0));
}

#[test]
fn a_sub_buffer_addresses_a_window_of_its_parent() {
    let Some(dev) = device() else { return };
    let n = 16usize;
    let parent = dev.alloc(n * 4, MemKind::Device, Pool::Weights).unwrap();
    write_f32(dev.as_ref(), &parent, &(0..n).map(|i| i as f32).collect::<Vec<_>>());

    let child = dev.sub_buffer(&parent, 8 * 4, 8 * 4).unwrap();
    assert_eq!(child.len(), 32);
    assert_eq!(read_f32(dev.as_ref(), &child, 8), vec![
        8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0
    ]);

    // Zapis przez dziecko musi być widoczny w rodzicu — to ta sama pamięć.
    write_f32(dev.as_ref(), &child, &[100.0]);
    assert_eq!(read_f32(dev.as_ref(), &parent, n)[8], 100.0);

    assert!(dev.sub_buffer(&parent, 8 * 4, 9 * 4).is_err(), "poza zakresem");
}

#[test]
fn a_buffer_argument_can_start_at_an_offset() {
    let Some(dev) = device() else { return };
    let n = 8usize;
    let input = dev.alloc(n * 4, MemKind::Device, Pool::Weights).unwrap();
    let output = dev.alloc(n * 4, MemKind::Device, Pool::Weights).unwrap();
    write_f32(dev.as_ref(), &input, &(0..n).map(|i| i as f32).collect::<Vec<_>>());

    let module = dev.load_module(SRC.as_bytes()).unwrap();
    let kernel = module.kernel("scale_add").unwrap();
    let stream = dev.create_stream().unwrap();

    // Czyta od czwartego elementu wejścia, pisze od początku wyjścia.
    let args = LaunchArgs::new()
        .buf(&output)
        .buf_at(&input, 4 * 4)
        .unwrap()
        .scalar(1.0f32)
        .scalar(4u32);
    dev.launch(&kernel, &LaunchConfig::linear(4, 64), &args, &stream)
        .unwrap();
    stream.synchronize().unwrap();

    assert_eq!(read_f32(dev.as_ref(), &output, 4), vec![5.0, 6.0, 7.0, 8.0]);
}

#[test]
fn events_order_work_and_report_completion() {
    let Some(dev) = device() else { return };
    let n = 64usize;
    let a = dev.alloc(n * 4, MemKind::Device, Pool::Weights).unwrap();
    let b = dev.alloc(n * 4, MemKind::Device, Pool::Weights).unwrap();
    write_f32(dev.as_ref(), &a, &vec![2.0f32; n]);

    let module = dev.load_module(SRC.as_bytes()).unwrap();
    let kernel = module.kernel("scale_add").unwrap();
    let stream = dev.create_stream().unwrap();
    let event = dev.create_event().unwrap();

    let args = LaunchArgs::new()
        .buf(&b)
        .buf(&a)
        .scalar(5.0f32)
        .scalar(n as u32);
    dev.launch(&kernel, &LaunchConfig::linear(n as u32, 64), &args, &stream)
        .unwrap();
    dev.record_event(&event, &stream).unwrap();
    dev.wait_event(&stream, &event).unwrap();
    event.synchronize().unwrap();

    assert!(event.is_complete().unwrap());
    assert!(read_f32(dev.as_ref(), &b, n).iter().all(|v| *v == 11.0));
}

#[test]
fn unsupported_paths_say_so_instead_of_pretending() {
    let Some(dev) = device() else { return };
    let stream = dev.create_stream().unwrap();
    assert!(dev.begin_capture(&stream).is_err());
    assert!(dev.end_capture(&stream).is_err());

    // Druga oś siatki JEST obsługiwana — używa jej batchowy matmul, który
    // kafelkuje wiersze wyjścia i tokeny naraz. Trzecia oś i wielowymiarowy
    // blok nie mają dziś kernela, który by ich potrzebował, więc są odrzucane
    // zamiast po cichu spłaszczane. Pamięć grupy roboczej deklarowana przy
    // wywołaniu to kontrakt CUDA; w Metalu deklaruje ją kernel.
    let module = dev.load_module(SRC.as_bytes()).unwrap();
    let kernel = module.kernel("scale_add").unwrap();
    let buf = dev.alloc(64, MemKind::Device, Pool::Weights).unwrap();
    let args = LaunchArgs::new().buf(&buf).buf(&buf).scalar(1.0f32).scalar(1u32);

    let two_d = LaunchConfig {
        grid: (1, 2, 1),
        block: (64, 1, 1),
        shared_mem_bytes: 0,
    };
    assert!(dev.launch(&kernel, &two_d, &args, &stream).is_ok());

    let three_d = LaunchConfig {
        grid: (1, 1, 2),
        block: (64, 1, 1),
        shared_mem_bytes: 0,
    };
    assert!(dev.launch(&kernel, &three_d, &args, &stream).is_err());

    let two_d_block = LaunchConfig {
        grid: (1, 1, 1),
        block: (64, 2, 1),
        shared_mem_bytes: 0,
    };
    assert!(dev.launch(&kernel, &two_d_block, &args, &stream).is_err());

    let with_smem = LaunchConfig {
        grid: (1, 1, 1),
        block: (64, 1, 1),
        shared_mem_bytes: 1024,
    };
    assert!(dev.launch(&kernel, &with_smem, &args, &stream).is_err());
}

#[test]
fn a_missing_kernel_name_is_an_error_not_a_crash() {
    let Some(dev) = device() else { return };
    let module = dev.load_module(SRC.as_bytes()).unwrap();
    let err = module.kernel("nie_ma_takiego").unwrap_err();
    assert!(format!("{err}").contains("nie_ma_takiego"));
}

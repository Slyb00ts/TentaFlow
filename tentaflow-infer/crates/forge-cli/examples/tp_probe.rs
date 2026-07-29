// Sprawdza rdzen tensor parallel na REALNYCH kartach: macierz Q8_0 dzielona po
// wierszach miedzy dwie karty, wynik zbierany na karcie 0 i porownywany z
// wynikiem policzonym w calosci na jednej karcie. Pierwszym kryterium jest
// ZGODNOSC, dopiero potem czas.
use forge_engine::cluster::Cluster;
use forge_engine::multi_gpu::{DeviceCapability, WorkKind};
use forge_engine::tensor_parallel::{gemv_q8_0_row_split, upload_row_split};
use forge_hal::{Pool, PoolSizes};
use forge_types::{MemKind, QuantKind};
use std::time::Instant;

const ROWS: usize = 8192;
const COLS: usize = 4096;

fn main() {
    let pools = PoolSizes {
        weights: 2 << 30,
        kv_cache: 16 << 20,
        activations: 256 << 20,
        kv_page_size: 256 << 10,
    };
    let cluster = Cluster::open(2, pools).expect("klaster");
    println!("P2P: {}", cluster.peer_access());

    let caps: Vec<DeviceCapability> = cluster.calibrate(QuantKind::Q8_0).expect("kalibracja");
    for (i, c) in caps.iter().enumerate() {
        println!("  dev{i}: pasmo {:.0} GB/s", c.stream_bytes_per_s / 1e9);
    }

    // Wagi Q8_0: 34 B na 32 wartosci.
    let row_bytes = (COLS / 32) * 34;
    let mut data = vec![0u8; ROWS * row_bytes];
    let mut seed = 0x2b7e_1516u32;
    for byte in data.iter_mut() {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        *byte = (seed >> 24) as u8;
    }
    // Skale bloku ustawiamy na male dodatnie f16, zeby wyniki byly porownywalne.
    for row in 0..ROWS {
        for block in 0..COLS / 32 {
            let at = row * row_bytes + block * 34;
            data[at] = 0x00;
            data[at + 1] = 0x2C;
        }
    }

    let x_host: Vec<u8> = (0..COLS * 2)
        .map(|i| if i % 2 == 0 { (i % 251) as u8 } else { 0x2C })
        .collect();

    let shards = upload_row_split(&cluster, &caps, &data, ROWS, row_bytes, WorkKind::MemoryBound)
        .expect("podzial");
    println!(
        "podzial wierszy: {:?} (razem {})",
        (0..cluster.len()).map(|i| shards.rows_on(i)).collect::<Vec<_>>(),
        shards.total_rows()
    );

    let mut x_copies = Vec::new();
    let mut y_parts = Vec::new();
    for index in 0..cluster.len() {
        let entry = cluster.device(index).unwrap();
        let x = entry.device.alloc(COLS * 2, MemKind::Device, Pool::Activations).unwrap();
        entry.device.write(&x_host, &x, 0).unwrap();
        let rows = shards.rows_on(index).max(1);
        let y = entry.device.alloc(rows * 4, MemKind::Device, Pool::Activations).unwrap();
        x_copies.push(x);
        y_parts.push(y);
    }
    let y_full = cluster
        .device(0).unwrap()
        .device.alloc(ROWS * 4, MemKind::Device, Pool::Activations).unwrap();

    gemv_q8_0_row_split(&cluster, &shards, &x_copies, &y_parts, &y_full, COLS, 0).expect("gemv");
    cluster.synchronize().unwrap();

    // Referencja: cala macierz na karcie 0.
    let entry0 = cluster.device(0).unwrap();
    let w_all = entry0.device.alloc(data.len(), MemKind::Device, Pool::Weights).unwrap();
    entry0.device.write(&data, &w_all, 0).unwrap();
    let y_ref = entry0.device.alloc(ROWS * 4, MemKind::Device, Pool::Activations).unwrap();
    entry0.kernels
        .gemv_q8_0_out_f32(&y_ref, &w_all, &x_copies[0], ROWS, COLS, &entry0.stream)
        .unwrap();
    entry0.stream.synchronize().unwrap();

    let mut split = vec![0u8; ROWS * 4];
    let mut single = vec![0u8; ROWS * 4];
    entry0.device.read(&y_full, 0, &mut split).unwrap();
    entry0.device.read(&y_ref, 0, &mut single).unwrap();
    let to_f32 = |b: &[u8]| -> Vec<f32> {
        b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
    };
    let a = to_f32(&split);
    let b = to_f32(&single);
    let mut worst = 0.0f32;
    let mut worst_at = 0usize;
    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        let d = (x - y).abs();
        if d > worst {
            worst = d;
            worst_at = i;
        }
    }
    println!(
        "zgodnosc: najgorsza roznica {worst:.6} w wierszu {worst_at} (wartosc {:.4})",
        b[worst_at]
    );
    assert!(worst == 0.0, "podzial musi dawac BITOWO ten sam wynik");

    const ITERS: usize = 200;
    let started = Instant::now();
    for _ in 0..ITERS {
        gemv_q8_0_row_split(&cluster, &shards, &x_copies, &y_parts, &y_full, COLS, 0).unwrap();
    }
    cluster.synchronize().unwrap();
    let split_us = started.elapsed().as_secs_f64() / ITERS as f64 * 1e6;

    let started = Instant::now();
    for _ in 0..ITERS {
        entry0.kernels
            .gemv_q8_0_out_f32(&y_ref, &w_all, &x_copies[0], ROWS, COLS, &entry0.stream)
            .unwrap();
    }
    entry0.stream.synchronize().unwrap();
    let single_us = started.elapsed().as_secs_f64() / ITERS as f64 * 1e6;
    println!(
        "dwie karty {split_us:.1} us, sama 6900 XT {single_us:.1} us, przyspieszenie {:.2}x",
        single_us / split_us
    );
}

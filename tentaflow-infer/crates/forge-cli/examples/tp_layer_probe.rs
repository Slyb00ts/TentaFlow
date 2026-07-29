// UWAGA: to jest sonda CZASU, nie poprawnosci. Odtwarza wzorzec obliczen i
// komunikacji pelnej warstwy (cztery macierze, dwie wymiany), ale nie laczy ich
// w poprawny numerycznie lancuch — wejscia sa stale. Zgodnosc wyniku sprawdza
// osobno `tp_probe`, ktory porownuje podzielony GEMV z jednokartowym bit w bit.
//
// Mierzy PELNA warstwe tensor parallel na ksztaltach Bielika 7B: podzielone QKV,
// uwaga na wlasnych glowicach, projekcja O, wymiana — potem gate/up, down i
// druga wymiana. Dwie wymiany na warstwe, nie na macierz: to jest granularnosc,
// przy ktorej TP ma sens, i dopiero ona pokazuje realny stosunek liczenia do
// komunikacji.
use forge_engine::cluster::Cluster;
use forge_engine::multi_gpu::{DeviceCapability, WorkKind};
use forge_engine::tensor_parallel::upload_row_split;
use forge_hal::{DevBuffer, Pool, PoolSizes};
use forge_types::{MemKind, QuantKind};
use std::time::Instant;

const HIDDEN: usize = 4096;
const QKV: usize = 6144; // 32 glowice Q + 8 KV + 8 V, head_dim 128
const FFN: usize = 11264;

fn q8_row_bytes(cols: usize) -> usize {
    (cols / 32) * 34
}

fn synth(rows: usize, cols: usize, seed0: u32) -> Vec<u8> {
    let row_bytes = q8_row_bytes(cols);
    let mut data = vec![0u8; rows * row_bytes];
    let mut seed = seed0;
    for byte in data.iter_mut() {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        *byte = (seed >> 24) as u8;
    }
    for row in 0..rows {
        for block in 0..cols / 32 {
            let at = row * row_bytes + block * 34;
            data[at] = 0x00;
            data[at + 1] = 0x2C;
        }
    }
    data
}

fn main() {
    let pools = PoolSizes {
        weights: 4 << 30,
        kv_cache: 16 << 20,
        activations: 512 << 20,
        kv_page_size: 256 << 10,
    };
    let cluster = Cluster::open(2, pools).expect("klaster");
    let caps: Vec<DeviceCapability> = cluster.calibrate(QuantKind::Q8_0).expect("kalibracja");
    println!(
        "pasma: {:?} GB/s",
        caps.iter().map(|c| (c.stream_bytes_per_s / 1e9) as u32).collect::<Vec<_>>()
    );

    // Cztery macierze warstwy. QKV i gate/up dzielone po WIERSZACH (kolumnowo
    // rownolegle), O i down licza obie karty na swoim fragmencie wejscia.
    let w_qkv = synth(QKV, HIDDEN, 0x1111);
    let w_o = synth(HIDDEN, HIDDEN, 0x2222);
    let w_gate_up = synth(2 * FFN, HIDDEN, 0x3333);
    let w_down = synth(HIDDEN, FFN, 0x4444);

    let s_qkv = upload_row_split(&cluster, &caps, &w_qkv, QKV, q8_row_bytes(HIDDEN), WorkKind::MemoryBound).unwrap();
    let s_o = upload_row_split(&cluster, &caps, &w_o, HIDDEN, q8_row_bytes(HIDDEN), WorkKind::MemoryBound).unwrap();
    let s_gu = upload_row_split(&cluster, &caps, &w_gate_up, 2 * FFN, q8_row_bytes(HIDDEN), WorkKind::MemoryBound).unwrap();
    let s_dn = upload_row_split(&cluster, &caps, &w_down, HIDDEN, q8_row_bytes(FFN), WorkKind::MemoryBound).unwrap();
    println!(
        "podzial QKV {:?}, gate/up {:?}",
        (0..2).map(|i| s_qkv.rows_on(i)).collect::<Vec<_>>(),
        (0..2).map(|i| s_gu.rows_on(i)).collect::<Vec<_>>()
    );

    let alloc = |dev: usize, bytes: usize| -> DevBuffer {
        cluster.device(dev).unwrap().device
            .alloc(bytes, MemKind::Device, Pool::Activations).unwrap()
    };
    let x: Vec<DevBuffer> = (0..2).map(|d| alloc(d, HIDDEN * 2)).collect();
    let xf: Vec<DevBuffer> = (0..2).map(|d| alloc(d, FFN * 2)).collect();
    let out_qkv: Vec<DevBuffer> = (0..2).map(|d| alloc(d, s_qkv.rows_on(d).max(1) * 4)).collect();
    let out_gu: Vec<DevBuffer> = (0..2).map(|d| alloc(d, s_gu.rows_on(d).max(1) * 4)).collect();
    let part: Vec<DevBuffer> = (0..2).map(|d| alloc(d, HIDDEN * 4)).collect();
    let peer = alloc(0, HIDDEN * 4);
    let sum = alloc(0, HIDDEN * 4);

    let layer = |iters: usize| -> f64 {
        let started = Instant::now();
        for _ in 0..iters {
            for stage in [&s_qkv, &s_gu] {
                let (outs, src, cols) = if std::ptr::eq(stage, &s_qkv) {
                    (&out_qkv, &x, HIDDEN)
                } else {
                    (&out_gu, &x, HIDDEN)
                };
                for d in 0..2 {
                    let rows = stage.rows_on(d);
                    if rows == 0 { continue; }
                    let e = cluster.device(d).unwrap();
                    e.kernels.gemv_q8_0_out_f32(&outs[d], stage.shard(d).unwrap(), &src[d], rows, cols, &e.stream).unwrap();
                }
                // Projekcja wierszowo-rownolegla: kazda karta liczy CALY wynik
                // ze swojego fragmentu wejscia, wiec potem trzeba je dodac.
                let (proj, cols_in, input) = if std::ptr::eq(stage, &s_qkv) {
                    (&s_o, HIDDEN, &x)
                } else {
                    (&s_dn, FFN, &xf)
                };
                for d in 0..2 {
                    let rows = proj.rows_on(d);
                    if rows == 0 { continue; }
                    let e = cluster.device(d).unwrap();
                    e.kernels.gemv_q8_0_out_f32(&part[d], proj.shard(d).unwrap(), &input[d], rows, cols_in, &e.stream).unwrap();
                }
                // JEDNA wymiana + redukcja na koniec bloku.
                cluster.exchange(1, &part[1], 0, 0, &peer, 0, HIDDEN * 4).unwrap();
                cluster.wait_for(0, 1).unwrap();
                let e0 = cluster.device(0).unwrap();
                e0.kernels.add_f32(&sum, &part[0], &peer, HIDDEN, &e0.stream).unwrap();
            }
            cluster.synchronize().unwrap();
        }
        started.elapsed().as_secs_f64() / iters as f64 * 1e6
    };

    layer(5);
    let tp_us = layer(100);

    // Referencja: cala warstwa na mocniejszej karcie.
    let e1 = cluster.device(1).unwrap();
    let full = |data: &[u8], rows: usize, cols: usize| -> DevBuffer {
        let b = e1.device.alloc(data.len(), MemKind::Device, Pool::Weights).unwrap();
        e1.device.write(data, &b, 0).unwrap();
        let _ = (rows, cols);
        b
    };
    let f_qkv = full(&w_qkv, QKV, HIDDEN);
    let f_o = full(&w_o, HIDDEN, HIDDEN);
    let f_gu = full(&w_gate_up, 2 * FFN, HIDDEN);
    let f_dn = full(&w_down, HIDDEN, FFN);
    let big = e1.device.alloc(2 * FFN * 4, MemKind::Device, Pool::Activations).unwrap();
    let single = |iters: usize| -> f64 {
        let started = Instant::now();
        for _ in 0..iters {
            e1.kernels.gemv_q8_0_out_f32(&big, &f_qkv, &x[1], QKV, HIDDEN, &e1.stream).unwrap();
            e1.kernels.gemv_q8_0_out_f32(&part[1], &f_o, &x[1], HIDDEN, HIDDEN, &e1.stream).unwrap();
            e1.kernels.gemv_q8_0_out_f32(&big, &f_gu, &x[1], 2 * FFN, HIDDEN, &e1.stream).unwrap();
            e1.kernels.gemv_q8_0_out_f32(&part[1], &f_dn, &xf[1], HIDDEN, FFN, &e1.stream).unwrap();
            e1.stream.synchronize().unwrap();
        }
        started.elapsed().as_secs_f64() / iters as f64 * 1e6
    };
    single(5);
    let one_us = single(100);

    println!("warstwa TP (2 karty): {tp_us:.1} us");
    println!("warstwa na samej 7900 XT: {one_us:.1} us");
    println!("przyspieszenie: {:.2}x", one_us / tp_us);
}

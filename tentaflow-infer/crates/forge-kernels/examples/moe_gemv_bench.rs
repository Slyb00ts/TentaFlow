// ===== File: moe_gemv_bench.rs — co naprawdę osiąga GEMV ekspertów =====
//
// Generacja czyta 3,258 GB na token dla Qwen3.6-35B, a llama.cpp robi 68,15
// tok/s, czyli 222 GB/s — praktycznie sufit tej pamięci. Ścieżka gęsta jest już
// przy 217 GB/s; cały dystans siedzi w GEMV ekspertów. Ten pomiar odpowiada na
// jedno pytanie: ile bajtów na sekundę ten kernel wyciąga na KAŻDYM z trzech
// kształtów, których model używa, bez reszty modelu w tle.

use std::sync::Arc;
use std::time::Instant;

use forge_hal::cuda::PoolSizes;
use forge_hal::{DevBuffer, Device, Pool};
use forge_kernels::Kernels;
use forge_types::{MemKind, QuantKind};

const EXPERTS: usize = 256;
const TOP_K: usize = 8;
const REPEATS: usize = 21;

/// `(etykieta, wiersze, kolumny, ile kolejnych selekcji czyta ten sam wiersz)`
const SHAPES: [(&str, usize, usize, usize); 2] = [
    ("gate/up 512x2048", 512, 2048, 1),
    ("down 2048x512", 2048, 512, 1),
];

/// Ten sam kształt w innym formacie odpowiada na pytanie, czy wąskie gardło
/// należy do MXFP4, czy do kształtu wywołania.
const FORMATS: [(QuantKind, usize, usize); 3] = [
    (QuantKind::MXFP4, 32, 17),
    (QuantKind::Q4K, 256, 144),
    (QuantKind::Q6K, 256, 210),
];

fn main() {
    let dev: Arc<dyn Device> = match forge_hal::gpu::open(
        0,
        PoolSizes {
            weights: 8 << 30,
            kv_cache: 16 << 20,
            activations: 256 << 20,
            kv_page_size: 256 << 10,
        },
    ) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("brak urządzenia: {e}");
            return;
        }
    };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    println!("{:>18} {:>10} {:>10} {:>10}", "kształt", "us", "GB/s", "MB");
    for (label, rows, cols, share) in SHAPES {
        for (quant, block_elems, block_bytes) in FORMATS {
            let per_expert = rows * cols / block_elems * block_bytes;
            let stack: DevBuffer = dev
                .alloc(EXPERTS * per_expert, MemKind::Device, Pool::Weights)
                .unwrap();
            // Scales that decode to something, codes that are not all zero.
            dev.write(&vec![0x77u8; EXPERTS * per_expert], &stack, 0)
                .unwrap();

            // The expert pointer table the routed kernels read.
            let base = stack.device_ptr();
            let addrs: Vec<u64> = (0..EXPERTS as u64)
                .map(|e| base + e * per_expert as u64)
                .collect();
            let table = dev
                .alloc(EXPERTS * 8, MemKind::Device, Pool::Weights)
                .unwrap();
            dev.write(bytemuck::cast_slice(&addrs), &table, 0).unwrap();

            // Eight experts spread across the stack, so no two selections share a
            // cache line of weights — which is what routing actually produces.
            let ids: Vec<i32> = (0..TOP_K as i32).map(|k| k * 31 + 3).collect();
            let ids_dev = dev
                .alloc(TOP_K * 4, MemKind::Device, Pool::Activations)
                .unwrap();
            dev.write(bytemuck::cast_slice(&ids), &ids_dev, 0).unwrap();

            let x = dev
                .alloc(TOP_K * cols * 2, MemKind::Device, Pool::Activations)
                .unwrap();
            dev.write(&vec![0x11u8; TOP_K * cols * 2], &x, 0).unwrap();
            let y = dev
                .alloc(TOP_K * rows * 2, MemKind::Device, Pool::Activations)
                .unwrap();

            let run = || {
                kernels
                    .gemv_gidx_batch(
                        quant, &y, &table, &x, rows, cols, &ids_dev, TOP_K, share, &stream,
                    )
                    .unwrap();
            };
            run();
            stream.synchronize().unwrap();
            let mut times = Vec::with_capacity(REPEATS);
            for _ in 0..REPEATS {
                let t = Instant::now();
                run();
                stream.synchronize().unwrap();
                times.push(t.elapsed().as_secs_f64());
            }
            times.sort_by(f64::total_cmp);
            let median = times[REPEATS / 2];
            let bytes = (TOP_K * per_expert) as f64;
            println!(
                "{:>18} {:>10.1} {:>10.1} {:>10.2}",
                format!("{label} {quant:?}"),
                median * 1e6,
                bytes / median / 1e9,
                bytes / 1e6
            );
        }
    }
}

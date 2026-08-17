// ===== File: gemm_fp4_bench.rs — cztery bity wobec ścieżki, którą zastępują =====
//
// Instrukcja retiruje się dwa razy szybciej niż `e4m3` i cztery razy szybciej
// niż `f16` (`examples/mma_rate.rs`). Ile z tego zostaje w kaflu, który musi
// jeszcze przeczytać wagi z pamięci, jest osobnym pytaniem — i to ono decyduje,
// czy ta ścieżka wchodzi do modelu.
//
// Kształty są te, które naprawdę liczy ThinkingCap Qwen3.6-27B NVFP4: projekcje
// QKV, wyjścia i FFN. Porównanie jest z `gemm_nvfp4_gguf_f16`, czyli z tym, co
// dziś liczy ten sam iloczyn na tych samych wagach.

use std::sync::Arc;
use std::time::Instant;

use forge_hal::cuda::PoolSizes;
use forge_hal::{DevBuffer, Device, Pool, Stream};
use forge_kernels::Kernels;
use forge_types::MemKind;
use half::f16;

const REPEATS: usize = 7;

/// `(wiersze, kolumny)` projekcji, plus etykieta.
const SHAPES: [(&str, usize, usize); 4] = [
    ("qkv 5120x5120", 5120, 5120),
    ("o 5120x4096", 5120, 4096),
    ("gate/up 17408x5120", 17408, 5120),
    ("down 5120x17408", 5120, 17408),
];

const TOKENS: [usize; 3] = [128, 512, 2048];

fn bench(label: &str, stream: &Stream, mut run: impl FnMut()) -> f64 {
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
    let _ = label;
    times[REPEATS / 2]
}

fn main() {
    let dev: Arc<dyn Device> = match forge_hal::gpu::open(
        0,
        PoolSizes {
            weights: 4 << 30,
            kv_cache: 16 << 20,
            activations: 2 << 30,
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
    if !kernels.supports_mxf4_block_scale() {
        eprintln!("brak artefaktów blokowo-skalowanego FP4");
        return;
    }
    let stream = dev.create_stream().unwrap();

    println!(
        "{:>20} {:>6} {:>10} {:>10} {:>10} {:>8}",
        "kształt", "tokeny", "f16 ms", "fp4 ms", "fp4 TF/s", "zysk"
    );
    for (label, rows, cols) in SHAPES {
        let block_bytes = cols / 64 * 36;
        let w: DevBuffer = dev
            .alloc(rows * block_bytes, MemKind::Device, Pool::Weights)
            .unwrap();
        // Codes that decode to something, so no denormal path is measured.
        dev.write(&vec![0x37u8; rows * block_bytes], &w, 0).unwrap();

        for tokens in TOKENS {
            let x_f16: Vec<f16> = (0..tokens * cols)
                .map(|i| f16::from_f32(((i % 23) as f32 - 11.0) / 16.0))
                .collect();
            let x = dev
                .alloc(tokens * cols * 2, MemKind::Device, Pool::Activations)
                .unwrap();
            dev.write(bytemuck::cast_slice(&x_f16), &x, 0).unwrap();
            let xq = dev
                .alloc(tokens * block_bytes, MemKind::Device, Pool::Activations)
                .unwrap();
            let xs = dev
                .alloc(tokens * 4, MemKind::Device, Pool::Activations)
                .unwrap();
            let y = dev
                .alloc(tokens * rows * 2, MemKind::Device, Pool::Activations)
                .unwrap();

            let f16_ms = bench(label, &stream, || {
                kernels
                    .gemm_nvfp4_gguf_f16(&y, &w, &x, rows, cols, tokens, 1.0, &stream)
                    .unwrap();
            });
            // The quantization pass is INSIDE the measurement: a model that
            // could not amortize it across the layer's projections would still
            // have to pay it, and pretending otherwise would flatter the number.
            let fp4_ms = bench(label, &stream, || {
                kernels
                    .quantize_act_nvfp4(&xq, &xs, &x, cols, tokens, &stream)
                    .unwrap();
                kernels
                    .gemm_nvfp4_mma_f16(&y, &w, &xq, &xs, rows, cols, tokens, 1.0, &stream)
                    .unwrap();
            });
            let flops = 2.0 * rows as f64 * cols as f64 * tokens as f64;
            println!(
                "{:>20} {:>6} {:>10.3} {:>10.3} {:>10.1} {:>7.2}x",
                label,
                tokens,
                f16_ms * 1e3,
                fp4_ms * 1e3,
                flops / fp4_ms / 1e12,
                f16_ms / fp4_ms
            );
        }
    }
}

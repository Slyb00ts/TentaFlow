// ===== File: mma_rate.rs — how fast this card retires each mma kind =====
//
// The question a four-bit tile has to answer before it is worth writing: does
// the matrix unit retire `kind::mxf4nvf4` at twice the rate of the `e4m3` op
// the FP8 path already uses, as the k64-versus-k32 shape suggests, or at the
// same rate — in which case four bits buy bandwidth and nothing else, and the
// existing FP8 second form already collects that.
//
// Operands live in registers for the whole loop, so no part of the memory
// system is in the number. That makes it a CEILING and not a prediction: a real
// tile reaches some fraction of it, but never more.

use std::sync::Arc;
use std::time::Instant;

use forge_hal::cuda::PoolSizes;
use forge_hal::{Device, Pool};
use forge_kernels::{Kernels, MmaKind, MMA_RATE_OPS};
use forge_types::MemKind;

/// Blocks per launch. Enough to give every SM several, so the number is a
/// throughput and not a latency.
const BLOCKS: u32 = 512;
const THREADS: u32 = 128;
const REPEATS: usize = 7;

fn main() {
    let dev: Arc<dyn Device> = match forge_hal::gpu::open(
        0,
        PoolSizes {
            weights: 16 << 20,
            kv_cache: 4 << 20,
            activations: 64 << 20,
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

    let a = dev.alloc(32 * 16, MemKind::Device, Pool::Weights).unwrap();
    dev.write(&[0x22u8; 32 * 16], &a, 0).unwrap();
    let d = dev
        .alloc(
            (BLOCKS * THREADS) as usize * 4,
            MemKind::Device,
            Pool::Activations,
        )
        .unwrap();

    println!("{:>6}  {:>10}  {:>10}", "rodzaj", "ms", "TFLOP/s");
    for kind in [MmaKind::F16, MmaKind::E4m3, MmaKind::Mxf4, MmaKind::Nvf4] {
        // One warm launch: the first touch of an artifact loads its module.
        kernels
            .mma_rate(kind, &d, &a, BLOCKS, THREADS, &stream)
            .unwrap();
        stream.synchronize().unwrap();

        let mut times = Vec::with_capacity(REPEATS);
        for _ in 0..REPEATS {
            let t = Instant::now();
            kernels
                .mma_rate(kind, &d, &a, BLOCKS, THREADS, &stream)
                .unwrap();
            stream.synchronize().unwrap();
            times.push(t.elapsed().as_secs_f64());
        }
        times.sort_by(f64::total_cmp);
        let median = times[REPEATS / 2];

        let warps = u64::from(BLOCKS) * u64::from(THREADS) / 32;
        let flops = 2 * warps * MMA_RATE_OPS * kind.macs();
        println!(
            "{:>6}  {:>10.3}  {:>10.1}",
            format!("{kind:?}"),
            median * 1e3,
            flops as f64 / median / 1e12
        );
    }
}

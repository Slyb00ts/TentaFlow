// ===== File: examples/gpu_bench.rs — wgpu vs CPU state-vector timings and the 2^28 spike =====
//
// Plan 16, Faza 0, spike D asks two things of the GPU backend: how long one
// kernel takes on 2^28 amplitudes on Vulkan, and whether the result still
// agrees with the CPU. This measures both, plus the same gate and a whole GHZ
// circuit at the sizes the capacity table of plan 4.2 cares about.
//
//   cargo run --release --features wgpu --example gpu_bench
//   cargo run --release --features wgpu --example gpu_bench -- --max-qubits 26
//
// `--cpu-max-qubits` caps the CPU timing column alone. `--compare-qubits` caps
// the amplitude-by-amplitude cross-check, which is the memory-hungry step: it
// holds a `complex128` CPU register and a read-back copy of both states at
// once, five times what the GPU register costs on its own.

use std::time::{Duration, Instant};

use num_complex::Complex64;
use tentaflow_quantum::sim::cpu::CpuBackend;
use tentaflow_quantum::sim::wgpu::{adapter_report, WgpuBackend};
use tentaflow_quantum::sim::{Backend, GateOp};

/// Register widths the tables report, from the T1 default of plan 4.2 up to the
/// spike-D size.
const SIZES: [usize; 4] = [20, 24, 26, 28];

/// Repetitions of the single-gate measurement. The gate is a full pass over the
/// state, so a handful of runs is enough to see past scheduling noise.
const GATE_REPEATS: usize = 5;

struct Args {
    max_qubits: usize,
    cpu_max_qubits: usize,
    compare_qubits: usize,
}

fn parse_args() -> Args {
    let mut args = Args {
        max_qubits: 28,
        cpu_max_qubits: 28,
        compare_qubits: 24,
    };
    let mut argv = std::env::args().skip(1);
    while let Some(flag) = argv.next() {
        let mut value = || {
            argv.next()
                .unwrap_or_else(|| panic!("{flag} needs a number"))
                .parse::<usize>()
                .unwrap_or_else(|error| panic!("{flag}: {error}"))
        };
        match flag.as_str() {
            "--max-qubits" => args.max_qubits = value(),
            "--cpu-max-qubits" => args.cpu_max_qubits = value(),
            "--compare-qubits" => args.compare_qubits = value(),
            other => panic!("unknown flag {other}"),
        }
    }
    args
}

fn hadamard(qubit: usize) -> GateOp {
    let h = std::f64::consts::FRAC_1_SQRT_2;
    GateOp::One {
        qubit,
        matrix: [
            Complex64::new(h, 0.0),
            Complex64::new(h, 0.0),
            Complex64::new(h, 0.0),
            Complex64::new(-h, 0.0),
        ],
    }
}

fn cnot(control: usize, target: usize) -> GateOp {
    let one = Complex64::new(1.0, 0.0);
    let zero = Complex64::new(0.0, 0.0);
    let mut matrix = [zero; 16];
    matrix[0] = one;
    matrix[5] = one;
    matrix[11] = one;
    matrix[14] = one;
    GateOp::Two {
        qubits: (control, target),
        matrix,
    }
}

/// The GHZ circuit as a gate batch: one Hadamard and `n - 1` CNOTs.
fn ghz_ops(num_qubits: usize) -> Vec<GateOp> {
    let mut ops = vec![hadamard(0)];
    ops.extend((1..num_qubits).map(|q| cnot(q - 1, q)));
    ops
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e3
}

/// Median of `GATE_REPEATS` single-gate applications, with `sync` folded in so
/// the number is the kernel and not the queuing of it.
fn time_gate<B: Backend>(backend: &mut B, sync: impl Fn(&B)) -> f64 {
    let op = [hadamard(0)];
    backend.apply(&op);
    sync(backend);
    let mut samples: Vec<f64> = Vec::with_capacity(GATE_REPEATS);
    for _ in 0..GATE_REPEATS {
        let start = Instant::now();
        backend.apply(&op);
        sync(backend);
        samples.push(millis(start.elapsed()));
    }
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}

fn time_ghz<B: Backend>(backend: &mut B, sync: impl Fn(&B)) -> f64 {
    let ops = ghz_ops(backend.num_qubits());
    backend.reset_to_zero();
    let start = Instant::now();
    backend.apply(&ops);
    sync(backend);
    millis(start.elapsed())
}

/// The GPU queues work, so a timing has to close the queue by hand; every
/// read-back path already orders itself behind it.
fn gpu_sync(backend: &WgpuBackend) {
    backend.sync();
}

/// The CPU kernels return when they are done.
fn cpu_sync(_: &CpuBackend<f32>) {}

fn main() {
    let args = parse_args();
    let report = match adapter_report() {
        Ok(report) => report,
        Err(reason) => {
            eprintln!("no GPU adapter on this machine: {reason}");
            std::process::exit(1);
        }
    };
    println!(
        "adapter: {} ({}, {}, driver {})",
        report.name, report.backend, report.device_type, report.driver
    );
    println!();
    println!("| Qubits | Amplitudes | One Hadamard (wgpu) | One Hadamard (cpu f32) | GHZ (wgpu) | GHZ (cpu f32) |");
    println!("|---|---|---|---|---|---|");

    for num_qubits in SIZES.into_iter().filter(|n| *n <= args.max_qubits) {
        let mut gpu = match WgpuBackend::new(num_qubits) {
            Ok(gpu) => gpu,
            Err(error) => {
                println!("| {num_qubits} | 2^{num_qubits} | refused: {error} | | | |");
                continue;
            }
        };
        let gate_gpu = time_gate(&mut gpu, gpu_sync);
        let ghz_gpu = time_ghz(&mut gpu, gpu_sync);
        let shards = gpu.shard_layout().shards();
        drop(gpu);

        let (gate_cpu, ghz_cpu) = if num_qubits <= args.cpu_max_qubits {
            let mut cpu = CpuBackend::<f32>::new(num_qubits);
            let gate = time_gate(&mut cpu, cpu_sync);
            let ghz = time_ghz(&mut cpu, cpu_sync);
            (format!("{gate:.1} ms"), format!("{ghz:.1} ms"))
        } else {
            ("not measured".to_string(), "not measured".to_string())
        };
        let shard_note = if shards > 1 {
            format!(" ({shards} shards)")
        } else {
            String::new()
        };
        println!(
            "| {num_qubits} | {} | {gate_gpu:.1} ms{shard_note} | {gate_cpu} | {ghz_gpu:.1} ms | {ghz_cpu} |",
            1u64 << num_qubits
        );
    }

    println!();
    spike_d(&args);
}

/// Spike D: one kernel over 2^28 amplitudes on the GPU, and the agreement with
/// the CPU.
///
/// The agreement is asserted twice, because the two checks answer different
/// questions and only one of them fits in memory at 2^28. At the largest size
/// the CPU register also fits, the whole state is compared amplitude by
/// amplitude. At 2^28 the GHZ state is compared against its analytic form
/// through GPU-side reductions, which needs no host copy of a 4 GiB state.
fn spike_d(args: &Args) {
    let spike = 28usize.min(args.max_qubits);
    println!("## Spike D — one kernel on 2^{spike} amplitudes");

    let mut gpu = match WgpuBackend::new(spike) {
        Ok(gpu) => gpu,
        Err(error) => {
            println!("the adapter refused a {spike}-qubit register: {error}");
            return;
        }
    };
    let gate = time_gate(&mut gpu, gpu_sync);
    println!("one Hadamard on 2^{spike} amplitudes: {gate:.1} ms");

    gpu.reset_to_zero();
    let ops = ghz_ops(spike);
    let start = Instant::now();
    gpu.apply(&ops);
    gpu.sync();
    println!(
        "GHZ over {spike} qubits ({} gates): {:.1} ms",
        ops.len(),
        millis(start.elapsed())
    );

    // Every qubit of a GHZ state is |0> and |1> with equal weight, and the only
    // two basis states carrying any mass are the all-zero and the all-one one.
    let mut worst = 0.0f64;
    for qubit in 0..spike {
        worst = worst.max((gpu.probability_of_one(qubit) - 0.5).abs());
    }
    let picks = gpu.sample(&[0.25, 0.75]);
    println!("largest deviation of P(1) from the analytic 0.5: {worst:.3e}");
    println!(
        "sampled basis states: {:?} (analytic: [0, {}])",
        picks,
        (1u64 << spike) - 1
    );

    let compare = args.compare_qubits.min(args.max_qubits);
    if compare < 4 {
        return;
    }
    let mut gpu = WgpuBackend::new(compare).expect("the register fits");
    let mut cpu = CpuBackend::<f64>::new(compare);
    let ops = ghz_ops(compare);
    gpu.apply(&ops);
    cpu.apply(&ops);
    let worst = gpu
        .amplitudes()
        .into_iter()
        .zip(cpu.amplitudes())
        .map(|(a, b)| (a - b).norm())
        .fold(0.0f64, f64::max);
    println!(
        "largest amplitude difference against the CPU over the whole 2^{compare} state: {worst:.3e}"
    );
}

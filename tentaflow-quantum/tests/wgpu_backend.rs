// ===== File: tests/wgpu_backend.rs — the GPU state-vector backend against the CPU one =====
//
// Plan 6.3 asks for one number from this backend: the same state as the CPU to
// 1e-5 on the golden set. Everything here is that comparison, on circuits the
// rest of the suite already trusts (Bell, GHZ, QFT, teleportation, random
// Clifford and random parametric), plus the paths only a device has — the split
// across storage buffers, collapse on the device and sampling from a prefix
// reduction.
//
// A machine with no Vulkan / Metal / DX12 adapter reports the reason and skips.
// It never passes quietly: a skipped test prints the adapter error, so "green
// on a machine without a GPU" cannot be mistaken for "the GPU agrees".
#![cfg(feature = "wgpu")]

mod common;

use std::f64::consts::PI;

use num_complex::Complex64;
use rand::rngs::StdRng;
use rand::SeedableRng;
use tentaflow_quantum::ir::Circuit;
use tentaflow_quantum::parse::{parse_qasm3, InputValues};
use tentaflow_quantum::sim::cpu::CpuBackend;
use tentaflow_quantum::sim::statevector::{
    self, compile, fuse, Instruction, KeyframeOptions, SimOptions, Simulator,
};
use tentaflow_quantum::sim::wgpu::{adapter_report, WgpuBackend};
use tentaflow_quantum::sim::{Backend, Cancel, Device, Precision};

/// Tolerance of every CPU/GPU comparison here, straight from plan 6.3.
const TOLERANCE: f64 = 1e-5;

/// Yields the adapter, or ends the test with the reason there is none.
macro_rules! gpu_or_skip {
    () => {
        match adapter_report() {
            Ok(report) => report,
            Err(reason) => {
                eprintln!("SKIPPED: this machine has no usable GPU adapter — {reason}");
                return;
            }
        }
    };
}

fn parse(source: &str) -> Circuit {
    parse_qasm3(source, &InputValues::new()).expect("the program is inside the supported subset")
}

fn header(qubits: usize) -> String {
    format!("OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[{qubits}] q;\n")
}

fn ghz(qubits: usize) -> Circuit {
    let mut source = header(qubits);
    source.push_str("h q[0];\n");
    for q in 1..qubits {
        source.push_str(&format!("cx q[{}], q[{q}];\n", q - 1));
    }
    parse(&source)
}

fn qft(qubits: usize) -> Circuit {
    let mut source = header(qubits);
    for j in (0..qubits).rev() {
        source.push_str(&format!("h q[{j}];\n"));
        for k in 0..j {
            let angle = PI / (1u64 << (j - k)) as f64;
            source.push_str(&format!("cp({angle}) q[{k}], q[{j}];\n"));
        }
    }
    for i in 0..qubits / 2 {
        source.push_str(&format!("swap q[{i}], q[{}];\n", qubits - 1 - i));
    }
    parse(&source)
}

const TELEPORTATION: &str = r#"
OPENQASM 3.0;
include "stdgates.inc";
qubit[3] q;
bit[3] c;
x q[0];
h q[1];
cx q[1], q[2];
cx q[0], q[1];
h q[0];
c[0] = measure q[0];
c[1] = measure q[1];
if (c[1] == 1) { x q[2]; }
if (c[0] == 1) { z q[2]; }
c[2] = measure q[2];
"#;

/// Apply a unitary circuit straight to a backend, so a test can drive a backend
/// the public entry points cannot build — a forced shard split, for instance.
fn apply_circuit(backend: &mut dyn Backend, circuit: &Circuit) {
    for step in fuse(&compile(circuit).expect("the circuit compiles")) {
        match step.instruction {
            Instruction::Unitary(op) => backend.apply(&[op]),
            Instruction::GlobalPhase(angle) => backend.apply_global_phase(angle),
            Instruction::Barrier => {}
            Instruction::Measure { .. } | Instruction::Reset { .. } => {
                panic!("this helper only takes unitary circuits")
            }
        }
    }
}

fn cpu_state(circuit: &Circuit) -> Vec<Complex64> {
    statevector::statevector(circuit, &SimOptions::default(), Device::Cpu, Cancel::none())
        .expect("the circuit is unitary")
}

fn gpu_state(circuit: &Circuit) -> Vec<Complex64> {
    statevector::statevector(
        circuit,
        &SimOptions {
            precision: Precision::Single,
            ..SimOptions::default()
        },
        Device::Wgpu,
        Cancel::none(),
    )
    .expect("the circuit is unitary")
}

// =============================================================================
// Device selection
// =============================================================================

#[test]
fn the_gpu_simulator_names_its_adapter_and_its_precision() {
    let report = gpu_or_skip!();
    let circuit = ghz(4);
    let simulator = Simulator::with_device(
        &circuit,
        &SimOptions {
            precision: Precision::Single,
            ..SimOptions::default()
        },
        Device::Wgpu,
    )
    .expect("the register fits");
    let description = simulator.describe();
    assert_eq!(description.backend, "wgpu");
    assert_eq!(description.adapter.as_deref(), Some(report.name.as_str()));
    assert_eq!(description.precision, Precision::Single);
    assert_eq!(description.num_qubits, 4);

    let cpu = Simulator::with_device(&circuit, &SimOptions::default(), Device::Cpu)
        .expect("the register fits");
    assert_eq!(cpu.describe().backend, "cpu");
    assert_eq!(cpu.describe().adapter, None);
}

#[test]
fn auto_picks_the_gpu_for_single_precision_and_the_cpu_for_double() {
    gpu_or_skip!();
    assert_eq!(Device::Auto.resolve(Precision::Single), Device::Wgpu);
    // WGSL has no f64 (plan 18.11), so a complex128 request must not silently
    // land on a backend that would halve its precision.
    assert_eq!(Device::Auto.resolve(Precision::Double), Device::Cpu);
}

#[test]
fn the_gpu_refuses_double_precision_instead_of_halving_it() {
    gpu_or_skip!();
    let Err(error) = Simulator::with_device(&ghz(3), &SimOptions::default(), Device::Wgpu) else {
        panic!("complex128 has no wgpu implementation, so the constructor must refuse");
    };
    assert!(
        error.to_string().contains("double precision"),
        "unexpected refusal: {error}"
    );
}

// =============================================================================
// The golden set
// =============================================================================

#[test]
fn bell_and_ghz_match_the_cpu() {
    gpu_or_skip!();
    let bell = parse(&format!("{}h q[0];\ncx q[0], q[1];\n", header(2)));
    common::assert_close(&gpu_state(&bell), &cpu_state(&bell), TOLERANCE);
    for qubits in [3usize, 8, 12] {
        let circuit = ghz(qubits);
        common::assert_close(&gpu_state(&circuit), &cpu_state(&circuit), TOLERANCE);
    }
}

#[test]
fn the_fourier_transform_matches_the_cpu() {
    gpu_or_skip!();
    for qubits in [3usize, 7, 10] {
        let circuit = qft(qubits);
        common::assert_close(&gpu_state(&circuit), &cpu_state(&circuit), TOLERANCE);
    }
}

#[test]
fn the_dense_unitary_matches_the_cpu() {
    gpu_or_skip!();
    let circuit = qft(4);
    let gpu = statevector::circuit_unitary(
        &circuit,
        &SimOptions {
            precision: Precision::Single,
            ..SimOptions::default()
        },
        Device::Wgpu,
    )
    .expect("the circuit is unitary");
    common::assert_close(&gpu, &common::dense_unitary(&circuit), TOLERANCE);
}

#[test]
fn random_parametric_circuits_match_the_cpu() {
    gpu_or_skip!();
    let mut rng = StdRng::seed_from_u64(20260906);
    for num_qubits in [1usize, 2, 5, 9, 12] {
        for _ in 0..3 {
            let circuit = common::random_universal_circuit(&mut rng, num_qubits, 40);
            common::assert_close(&gpu_state(&circuit), &cpu_state(&circuit), TOLERANCE);
        }
    }
}

#[test]
fn random_clifford_circuits_match_the_cpu() {
    gpu_or_skip!();
    let mut rng = StdRng::seed_from_u64(77);
    for num_qubits in [2usize, 6, 12] {
        for _ in 0..3 {
            // The generator appends a measurement per qubit; the state before
            // them is what the two devices have to agree on.
            let measured = common::random_clifford_circuit(&mut rng, num_qubits, 40);
            let mut unitary = Circuit::new();
            unitary.add_qubit_register("q", num_qubits).unwrap();
            for op in measured.ops() {
                if let tentaflow_quantum::ir::OpKind::Gate { gate, qubits } = &op.kind {
                    unitary.push_gate(*gate, qubits).unwrap();
                }
            }
            common::assert_close(&gpu_state(&unitary), &cpu_state(&unitary), TOLERANCE);
        }
    }
}

/// `Backend::apply` takes a SLICE so a device can submit one command buffer for
/// a whole run of gates. Every dispatch in that submission writes the buffer the
/// next one reads, so this is where a missing barrier between them would show
/// up — and the scheduler, which submits one gate at a time, would never find
/// it.
#[test]
fn a_batch_of_gates_equals_the_same_gates_one_at_a_time() {
    gpu_or_skip!();
    let circuit = common::random_universal_circuit(&mut StdRng::seed_from_u64(64), 7, 60);
    let program = fuse(&compile(&circuit).expect("the circuit compiles"));
    let ops: Vec<_> = program
        .into_iter()
        .map(|step| match step.instruction {
            Instruction::Unitary(op) => op,
            other => panic!("the generator emits gates only, got {other:?}"),
        })
        .collect();

    let mut batched = WgpuBackend::new(7).expect("the register fits");
    batched.apply(&ops);
    let mut stepped = WgpuBackend::new(7).expect("the register fits");
    for op in &ops {
        stepped.apply(std::slice::from_ref(op));
    }
    common::assert_close(&batched.amplitudes(), &stepped.amplitudes(), 1e-12);
    common::assert_close(&batched.amplitudes(), &cpu_state(&circuit), TOLERANCE);
}

// =============================================================================
// Sharding
// =============================================================================

/// The shard split only happens on a register bigger than one storage binding,
/// which no test can allocate. `with_shard_limit` forces the same split at a
/// size that runs in a second, so the split kernels are covered rather than
/// assumed.
#[test]
fn a_forced_shard_split_computes_the_same_state() {
    gpu_or_skip!();
    let mut rng = StdRng::seed_from_u64(31337);
    for (num_qubits, shard_amplitudes, expected_shards) in
        [(6usize, 32u64, 2usize), (8, 32, 8), (10, 128, 8)]
    {
        let circuit = common::random_universal_circuit(&mut rng, num_qubits, 40);
        let mut gpu = WgpuBackend::with_shard_limit(num_qubits, shard_amplitudes)
            .expect("the forced split fits eight shards");
        assert_eq!(
            gpu.shard_layout().shards(),
            expected_shards,
            "{num_qubits} qubits should split into {expected_shards} shards"
        );
        apply_circuit(&mut gpu, &circuit);
        common::assert_close(&gpu.amplitudes(), &cpu_state(&circuit), TOLERANCE);
    }
}

#[test]
fn a_split_state_measures_and_collapses_like_the_cpu() {
    gpu_or_skip!();
    let num_qubits = 8;
    // 32 amplitudes per shard puts qubits 0..5 inside a shard and 5..8 across
    // shards, so both the local and the split collapse kernels run.
    let circuit = common::random_universal_circuit(&mut StdRng::seed_from_u64(9), num_qubits, 40);
    for qubit in 0..num_qubits {
        for outcome in [false, true] {
            let mut gpu = WgpuBackend::with_shard_limit(num_qubits, 32).expect("the split fits");
            let mut cpu = CpuBackend::<f64>::new(num_qubits);
            apply_circuit(&mut gpu, &circuit);
            apply_circuit(&mut cpu, &circuit);

            let gpu_p = gpu.probability_of_one(qubit);
            let cpu_p = cpu.probability_of_one(qubit);
            assert!(
                (gpu_p - cpu_p).abs() < TOLERANCE,
                "P(1) on qubit {qubit}: gpu {gpu_p} vs cpu {cpu_p}"
            );
            if (outcome && cpu_p < 1e-6) || (!outcome && cpu_p > 1.0 - 1e-6) {
                continue;
            }
            gpu.collapse(qubit, outcome);
            cpu.collapse(qubit, outcome);
            common::assert_close(&gpu.amplitudes(), &cpu.amplitudes(), TOLERANCE);
        }
    }
}

#[test]
fn a_split_state_resets_to_the_zero_register() {
    gpu_or_skip!();
    let mut gpu = WgpuBackend::with_shard_limit(7, 16).expect("the split fits");
    assert_eq!(gpu.shard_layout().shards(), 8);
    apply_circuit(
        &mut gpu,
        &common::random_universal_circuit(&mut StdRng::seed_from_u64(5), 7, 30),
    );
    gpu.reset_to_zero();
    let amps = gpu.amplitudes();
    assert_eq!(amps[0], Complex64::new(1.0, 0.0));
    assert!(amps[1..].iter().all(|a| a.norm() == 0.0), "{amps:?}");
}

// =============================================================================
// Measurement, sampling and the analytics on top of a read-back state
// =============================================================================

#[test]
fn sampling_reproduces_the_probability_vector() {
    gpu_or_skip!();
    let circuit = common::random_universal_circuit(&mut StdRng::seed_from_u64(2), 6, 40);
    let mut gpu = WgpuBackend::new(6).expect("the register fits");
    apply_circuit(&mut gpu, &circuit);

    let expected = gpu.probabilities();
    let reference: Vec<f64> = cpu_state(&circuit).iter().map(|a| a.norm_sqr()).collect();
    for (index, (a, b)) in expected.iter().zip(&reference).enumerate() {
        assert!((a - b).abs() < TOLERANCE, "P({index}) = {a} vs {b}");
    }

    // The shot stream is the caller's, so the same seed on the two devices need
    // not give the same shots — but the distribution it is drawn from has to be
    // the same one, which is what this checks.
    let shots = 200_000usize;
    let mut draws: Vec<f64> = (0..shots)
        .map(|i| (i as f64 + 0.5) / shots as f64)
        .collect();
    draws.sort_by(f64::total_cmp);
    let picks = gpu.sample(&draws);
    assert_eq!(picks.len(), shots);
    let mut observed = vec![0.0f64; expected.len()];
    for index in picks {
        observed[index] += 1.0 / shots as f64;
    }
    let distance: f64 = observed
        .iter()
        .zip(&expected)
        .map(|(a, b)| (a - b).abs())
        .sum::<f64>()
        / 2.0;
    assert!(
        distance < 1e-3,
        "GPU sampling drifted from its own distribution by {distance}"
    );
}

#[test]
fn a_measured_circuit_runs_shot_by_shot_on_the_gpu() {
    gpu_or_skip!();
    let circuit = parse(TELEPORTATION);
    let options = SimOptions {
        precision: Precision::Single,
        seed: 7,
        ..SimOptions::default()
    };
    let result = statevector::run(&circuit, &options, Device::Wgpu, 512, Cancel::none())
        .expect("teleportation replays per shot");
    assert_eq!(result.shots, 512);
    // The receiver is bit 2, rendered leftmost. Teleporting |1> must land a 1
    // there on every shot whatever the two measured bits were.
    for (key, count) in &result.counts {
        assert!(
            key.starts_with('1'),
            "teleported |1> arrived as {key} on {count} shots"
        );
    }
    assert_eq!(result.counts.values().sum::<u64>(), 512);
}

#[test]
fn a_sampled_circuit_agrees_with_the_cpu_histogram() {
    gpu_or_skip!();
    let mut source = header(6);
    source.push_str("bit[6] c;\n");
    source.push_str("h q[0];\nry(0.7) q[1];\nrx(1.1) q[2];\n");
    for q in 3..6 {
        source.push_str(&format!("cx q[{}], q[{q}];\n", q - 3));
    }
    source.push_str("c = measure q;\n");
    let circuit = parse(&source);
    let shots = 40_000;
    let options = SimOptions {
        seed: 4242,
        ..SimOptions::default()
    };
    let cpu = statevector::run(&circuit, &options, Device::Cpu, shots, Cancel::none()).unwrap();
    let gpu = statevector::run(
        &circuit,
        &SimOptions {
            precision: Precision::Single,
            ..options
        },
        Device::Wgpu,
        shots,
        Cancel::none(),
    )
    .unwrap();
    let distance = tentaflow_quantum::grade::total_variation_distance(&cpu.counts, &gpu.counts)
        .expect("both histograms carry the same shot count");
    assert!(
        distance < 0.02,
        "cpu and gpu histograms differ by {distance}"
    );
}

#[test]
fn the_stepper_and_its_keyframes_agree_across_devices() {
    gpu_or_skip!();
    let circuit = common::random_universal_circuit(&mut StdRng::seed_from_u64(1234), 5, 30);
    let mut cpu = Simulator::with_device(&circuit, &SimOptions::default(), Device::Cpu).unwrap();
    let mut gpu = Simulator::with_device(
        &circuit,
        &SimOptions {
            precision: Precision::Single,
            ..SimOptions::default()
        },
        Device::Wgpu,
    )
    .unwrap();

    let options = KeyframeOptions {
        pairs: tentaflow_quantum::sim::statevector::PairSelection::All,
        ..KeyframeOptions::default()
    };
    while cpu.step() {
        assert!(gpu.step());
        common::assert_close(&gpu.amplitudes(), &cpu.amplitudes(), TOLERANCE);

        let cpu_frame = cpu.keyframe(&options).unwrap();
        let gpu_frame = gpu.keyframe(&options).unwrap();
        assert_eq!(cpu_frame.step, gpu_frame.step);
        for (a, b) in cpu_frame.bloch.iter().zip(&gpu_frame.bloch) {
            for axis in 0..3 {
                assert!(
                    (a[axis] - b[axis]).abs() < TOLERANCE,
                    "bloch {a:?} vs {b:?}"
                );
            }
        }
        for (a, b) in cpu_frame.purity.iter().zip(&gpu_frame.purity) {
            assert!((a - b).abs() < TOLERANCE, "purity {a} vs {b}");
        }
        for (a, b) in cpu_frame.pairs.iter().zip(&gpu_frame.pairs) {
            assert_eq!(a.qubits, b.qubits);
            assert!((a.mutual_information - b.mutual_information).abs() < 1e-4);
            assert!((a.concurrence - b.concurrence).abs() < 1e-4);
            common::assert_close(&a.rho, &b.rho, TOLERANCE);
        }
        // The two lists cannot be compared key for key: at the tail of the
        // top-K the entries are separated by less than the f32 error, so which
        // of two near-equal basis states makes the cut is not a property either
        // device owns. What both must agree on is the value they report.
        assert_eq!(cpu_frame.probs_top.len(), gpu_frame.probs_top.len());
        assert_eq!(
            cpu_frame.probs_top.first().map(|(key, _)| key),
            gpu_frame.probs_top.first().map(|(key, _)| key),
            "the heaviest basis state differs"
        );
        let reference = cpu.probabilities();
        for (key, probability) in &gpu_frame.probs_top {
            let index = usize::from_str_radix(key, 2).expect("a count key is binary");
            assert!(
                (reference[index] - probability).abs() < TOLERANCE,
                "P({key}) = {probability} on the gpu, {} on the cpu",
                reference[index]
            );
        }
    }
    assert!(!gpu.step());
}

#[test]
fn a_fractional_step_previews_on_the_same_device() {
    gpu_or_skip!();
    let circuit = common::random_universal_circuit(&mut StdRng::seed_from_u64(808), 4, 20);
    let mut gpu = Simulator::with_device(
        &circuit,
        &SimOptions {
            precision: Precision::Single,
            ..SimOptions::default()
        },
        Device::Wgpu,
    )
    .unwrap();
    let mut cpu = Simulator::with_device(&circuit, &SimOptions::default(), Device::Cpu).unwrap();
    while cpu.position() < cpu.step_count() {
        let half_gpu = gpu.step_fraction(0.5).unwrap();
        let half_cpu = cpu.step_fraction(0.5).unwrap();
        common::assert_close(&half_gpu, &half_cpu, TOLERANCE);
        // A whole fractional step is the step itself, on the GPU as on the CPU.
        let whole = gpu.step_fraction(1.0).unwrap();
        gpu.step();
        cpu.step();
        common::assert_close(&whole, &gpu.amplitudes(), TOLERANCE);
    }
}

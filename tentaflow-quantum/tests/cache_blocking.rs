// ===== File: tests/cache_blocking.rs — a batched run equals the same gates applied one by one =====
//
// The CPU backend reorders gates inside a batch (disjoint qubits commute) and
// applies the cache-local ones chunk by chunk. That only starts above the chunk
// size, wider than the dense reference of the other suites can follow, so the
// check here is the backend against itself: one batch versus one gate at a time.

mod common;

use rand::rngs::StdRng;
use rand::SeedableRng;
use tentaflow_quantum::sim::cpu::CpuBackend;
use tentaflow_quantum::sim::statevector::{compile, Instruction};
use tentaflow_quantum::sim::{Backend, GateOp};

fn gate_ops(seed: u64, num_qubits: usize, depth: usize) -> Vec<GateOp> {
    let mut rng = StdRng::seed_from_u64(seed);
    let circuit = common::random_universal_circuit(&mut rng, num_qubits, depth);
    compile(&circuit)
        .expect("a random circuit compiles")
        .into_iter()
        .filter_map(|step| match step.instruction {
            Instruction::Unitary(op) => Some(op),
            _ => None,
        })
        .collect()
}

fn max_difference(a: &dyn Backend, b: &dyn Backend) -> f64 {
    a.amplitudes()
        .iter()
        .zip(b.amplitudes().iter())
        .map(|(x, y)| (x - y).norm())
        .fold(0.0, f64::max)
}

#[test]
fn a_batch_matches_the_gates_applied_one_at_a_time_in_double_precision() {
    for seed in 0..3 {
        let ops = gate_ops(seed, 17, 300);
        let mut batched = CpuBackend::<f64>::new(17);
        batched.apply(&ops);
        let mut single = CpuBackend::<f64>::new(17);
        for op in &ops {
            single.apply(std::slice::from_ref(op));
        }
        let difference = max_difference(&batched, &single);
        assert!(
            difference < 1e-12,
            "seed {seed}: batches drift by {difference}"
        );
    }
}

#[test]
fn a_batch_matches_the_gates_applied_one_at_a_time_in_single_precision() {
    let ops = gate_ops(9, 18, 300);
    let mut batched = CpuBackend::<f32>::new(18);
    batched.apply(&ops);
    let mut single = CpuBackend::<f32>::new(18);
    for op in &ops {
        single.apply(std::slice::from_ref(op));
    }
    let difference = max_difference(&batched, &single);
    assert!(difference < 1e-4, "batches drift by {difference}");
}

#[test]
fn a_batch_keeps_the_state_normalised() {
    let ops = gate_ops(4, 17, 400);
    let mut backend = CpuBackend::<f64>::new(17);
    backend.apply(&ops);
    let norm: f64 = backend.probabilities().iter().sum();
    assert!((norm - 1.0).abs() < 1e-10, "norm is {norm}");
}

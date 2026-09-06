// ===== File: tests/common/mod.rs — dense reference simulator used to check the bit-indexed kernels =====
//
// Every integration test binary compiles this module on its own, so a helper
// only some of them need still looks unused from the others.
#![allow(dead_code)]

use num_complex::Complex64;
use tentaflow_quantum::gate::Gate;
use tentaflow_quantum::ir::{Circuit, OpKind};

/// Build the full 2^n x 2^n matrix of one gate by writing it out column by
/// column. This deliberately avoids the pair/quad bit indexing the simulator
/// uses, so a mistake in either one shows up as a disagreement.
pub fn embed(matrix: &[Complex64], qubits: &[usize], num_qubits: usize) -> Vec<Complex64> {
    let dim = 1usize << num_qubits;
    let sub_dim = 1usize << qubits.len();
    let mut full = vec![Complex64::new(0.0, 0.0); dim * dim];
    for column in 0..dim {
        let column_sub = sub_index(column, qubits);
        for row_sub in 0..sub_dim {
            let row = with_sub_index(column, qubits, row_sub);
            full[row * dim + column] = matrix[row_sub * sub_dim + column_sub];
        }
    }
    full
}

fn sub_index(index: usize, qubits: &[usize]) -> usize {
    let k = qubits.len();
    let mut sub = 0usize;
    for (position, qubit) in qubits.iter().enumerate() {
        if index >> qubit & 1 == 1 {
            sub |= 1 << (k - 1 - position);
        }
    }
    sub
}

fn with_sub_index(index: usize, qubits: &[usize], sub: usize) -> usize {
    let k = qubits.len();
    let mut out = index;
    for (position, qubit) in qubits.iter().enumerate() {
        let bit = sub >> (k - 1 - position) & 1;
        if bit == 1 {
            out |= 1 << qubit;
        } else {
            out &= !(1 << qubit);
        }
    }
    out
}

fn multiply(a: &[Complex64], b: &[Complex64], dim: usize) -> Vec<Complex64> {
    let mut out = vec![Complex64::new(0.0, 0.0); dim * dim];
    for i in 0..dim {
        for k in 0..dim {
            let aik = a[i * dim + k];
            if aik == Complex64::new(0.0, 0.0) {
                continue;
            }
            for j in 0..dim {
                out[i * dim + j] += aik * b[k * dim + j];
            }
        }
    }
    out
}

/// Dense unitary of a circuit without measurements, resets or guards.
pub fn dense_unitary(circuit: &Circuit) -> Vec<Complex64> {
    let n = circuit.num_qubits();
    let dim = 1usize << n;
    let mut acc = vec![Complex64::new(0.0, 0.0); dim * dim];
    for i in 0..dim {
        acc[i * dim + i] = Complex64::new(1.0, 0.0);
    }
    for op in circuit.ops() {
        match &op.kind {
            OpKind::Gate { gate, qubits } => {
                let full = embed(gate.matrix().as_slice(), qubits, n);
                acc = multiply(&full, &acc, dim);
            }
            OpKind::GlobalPhase(angle) => {
                let factor = Complex64::from_polar(1.0, *angle);
                acc.iter_mut().for_each(|z| *z *= factor);
            }
            OpKind::Barrier { .. } => {}
            OpKind::Measure { .. } | OpKind::Reset { .. } => {
                panic!("the dense reference only handles unitary circuits")
            }
        }
    }
    acc
}

/// Dense final state of a circuit started in |0...0>.
pub fn dense_state(circuit: &Circuit) -> Vec<Complex64> {
    let dim = 1usize << circuit.num_qubits();
    let unitary = dense_unitary(circuit);
    (0..dim).map(|row| unitary[row * dim]).collect()
}

pub fn assert_close(actual: &[Complex64], expected: &[Complex64], tolerance: f64) {
    assert_eq!(actual.len(), expected.len(), "dimension mismatch");
    for (index, (a, b)) in actual.iter().zip(expected).enumerate() {
        assert!(
            (a - b).norm() <= tolerance,
            "entry {index}: {a} vs {b} (tolerance {tolerance})"
        );
    }
}

/// A random circuit over the universal gate set, used to compare the simulator
/// against the dense reference.
pub fn random_universal_circuit(
    rng: &mut impl rand::RngExt,
    num_qubits: usize,
    depth: usize,
) -> Circuit {
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", num_qubits).unwrap();
    for _ in 0..depth {
        let single = [
            Gate::Id,
            Gate::X,
            Gate::Y,
            Gate::Z,
            Gate::H,
            Gate::S,
            Gate::Sdg,
            Gate::T,
            Gate::Tdg,
            Gate::Sx,
            Gate::SxDg,
        ];
        let choice = rng.random_range(0..6usize);
        match choice {
            0 => {
                let gate = single[rng.random_range(0..single.len())];
                circuit
                    .push_gate(gate, &[rng.random_range(0..num_qubits)])
                    .unwrap();
            }
            1 => {
                let gate = match rng.random_range(0..5usize) {
                    0 => Gate::P(rng_angle(rng)),
                    1 => Gate::Rx(rng_angle(rng)),
                    2 => Gate::Ry(rng_angle(rng)),
                    3 => Gate::Rz(rng_angle(rng)),
                    _ => Gate::U(rng_angle(rng), rng_angle(rng), rng_angle(rng)),
                };
                circuit
                    .push_gate(gate, &[rng.random_range(0..num_qubits)])
                    .unwrap();
            }
            _ => {
                if num_qubits < 2 {
                    continue;
                }
                let (a, b) = distinct_pair(rng, num_qubits);
                let gate = match rng.random_range(0..10usize) {
                    0 => Gate::Cx,
                    1 => Gate::Cy,
                    2 => Gate::Cz,
                    3 => Gate::Ch,
                    4 => Gate::Swap,
                    5 => Gate::Cp(rng_angle(rng)),
                    6 => Gate::Crx(rng_angle(rng)),
                    7 => Gate::Cry(rng_angle(rng)),
                    8 => Gate::Crz(rng_angle(rng)),
                    _ => Gate::Cu(
                        rng_angle(rng),
                        rng_angle(rng),
                        rng_angle(rng),
                        rng_angle(rng),
                    ),
                };
                circuit.push_gate(gate, &[a, b]).unwrap();
            }
        }
    }
    circuit
}

/// A random Clifford circuit; the stabilizer tableau and the state vector must
/// agree on it.
pub fn random_clifford_circuit(
    rng: &mut impl rand::RngExt,
    num_qubits: usize,
    depth: usize,
) -> Circuit {
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", num_qubits).unwrap();
    circuit.add_clbit_register("c", num_qubits).unwrap();
    let single = [
        Gate::Id,
        Gate::X,
        Gate::Y,
        Gate::Z,
        Gate::H,
        Gate::S,
        Gate::Sdg,
        Gate::Sx,
        Gate::SxDg,
        Gate::Rx(std::f64::consts::FRAC_PI_2),
        Gate::Ry(std::f64::consts::FRAC_PI_2),
        Gate::Rz(std::f64::consts::FRAC_PI_2),
        Gate::P(std::f64::consts::PI),
    ];
    for _ in 0..depth {
        if rng.random_range(0..3usize) == 0 && num_qubits >= 2 {
            let (a, b) = distinct_pair(rng, num_qubits);
            let gate = match rng.random_range(0..4usize) {
                0 => Gate::Cx,
                1 => Gate::Cy,
                2 => Gate::Cz,
                _ => Gate::Swap,
            };
            circuit.push_gate(gate, &[a, b]).unwrap();
        } else {
            let gate = single[rng.random_range(0..single.len())];
            circuit
                .push_gate(gate, &[rng.random_range(0..num_qubits)])
                .unwrap();
        }
    }
    for q in 0..num_qubits {
        circuit.push_measure(q, q).unwrap();
    }
    circuit
}

fn distinct_pair(rng: &mut impl rand::RngExt, num_qubits: usize) -> (usize, usize) {
    let a = rng.random_range(0..num_qubits);
    let mut b = rng.random_range(0..num_qubits);
    while b == a {
        b = rng.random_range(0..num_qubits);
    }
    (a, b)
}

fn rng_angle(rng: &mut impl rand::RngExt) -> f64 {
    rng.random_range(-3.0..3.0f64)
}

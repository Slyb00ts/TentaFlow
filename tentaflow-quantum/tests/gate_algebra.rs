// ===== File: tests/gate_algebra.rs — gate matrices, adjoints, powers and single-qubit fusion =====

mod common;

use std::f64::consts::PI;

use num_complex::Complex64;
use rand::rngs::StdRng;
use rand::SeedableRng;
use tentaflow_quantum::gate::{Gate, Matrix};
use tentaflow_quantum::ir::Circuit;
use tentaflow_quantum::linalg;
use tentaflow_quantum::sim::statevector::{compile, fuse, statevector, Instruction, SimOptions};

fn all_gates() -> Vec<Gate> {
    vec![
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
        Gate::P(0.7),
        Gate::Rx(0.7),
        Gate::Ry(-1.3),
        Gate::Rz(2.1),
        Gate::U(0.3, 0.4, 0.5),
        Gate::Cx,
        Gate::Cy,
        Gate::Cz,
        Gate::Ch,
        Gate::Swap,
        Gate::Cp(1.1),
        Gate::Crx(0.9),
        Gate::Cry(-0.4),
        Gate::Crz(1.7),
        Gate::Cu(0.3, 0.4, 0.5, 0.6),
    ]
}

fn identity(dim: usize) -> Vec<Complex64> {
    linalg::identity(dim)
}

#[test]
fn every_gate_matrix_is_unitary() {
    for gate in all_gates() {
        let matrix = gate.matrix();
        let dim = matrix.dim();
        let product = linalg::matmul(
            matrix.as_slice(),
            &linalg::dagger(matrix.as_slice(), dim),
            dim,
        );
        common::assert_close(&product, &identity(dim), 1e-12);
    }
}

#[test]
fn the_adjoint_undoes_the_gate() {
    for gate in all_gates() {
        let matrix = gate.matrix();
        let dim = matrix.dim();
        let inverse = gate.adjoint().matrix();
        assert_eq!(inverse.dim(), dim);
        let product = linalg::matmul(inverse.as_slice(), matrix.as_slice(), dim);
        common::assert_close(&product, &identity(dim), 1e-12);
    }
}

#[test]
fn a_controlled_gate_is_the_block_diagonal_of_its_target() {
    for gate in all_gates().into_iter().filter(|g| g.arity() == 1) {
        let Some(controlled) = gate.controlled() else {
            continue;
        };
        let target = match gate.matrix() {
            Matrix::One(m) => m,
            Matrix::Two(_) => unreachable!(),
        };
        let full = match controlled.matrix() {
            Matrix::Two(m) => m,
            Matrix::One(_) => panic!("a controlled gate acts on two qubits"),
        };
        let mut expected = identity(4);
        expected[2 * 4 + 2] = target[0];
        expected[2 * 4 + 3] = target[1];
        expected[3 * 4 + 2] = target[2];
        expected[3 * 4 + 3] = target[3];
        // `ctrl @ g` may differ from the block form by a phase on the control
        // when the gate itself carries one; compare up to that global phase.
        assert!(
            tentaflow_quantum::grade::unitaries_equal(&full, &expected, 1e-9).unwrap(),
            "ctrl @ {} does not match its block form",
            gate.qasm_name()
        );
    }
}

#[test]
fn a_unit_power_reproduces_the_gate() {
    for gate in all_gates() {
        let matrix = gate.matrix();
        let dim = matrix.dim();
        let powered = linalg::unitary_power(matrix.as_slice(), dim, 1.0);
        common::assert_close(&powered, matrix.as_slice(), 1e-10);
        let identity_power = linalg::unitary_power(matrix.as_slice(), dim, 0.0);
        common::assert_close(&identity_power, &identity(dim), 1e-10);
    }
}

#[test]
fn a_half_power_squares_back_to_the_gate() {
    for gate in all_gates() {
        let matrix = gate.matrix();
        let dim = matrix.dim();
        let half = linalg::unitary_power(matrix.as_slice(), dim, 0.5);
        let squared = linalg::matmul(&half, &half, dim);
        common::assert_close(&squared, matrix.as_slice(), 1e-9);
    }
}

#[test]
fn rotation_gates_scale_their_angle_under_a_fractional_power() {
    let cases = [
        (Gate::Rx(1.2), Gate::Rx(0.3)),
        (Gate::Ry(1.2), Gate::Ry(0.3)),
        (Gate::Rz(1.2), Gate::Rz(0.3)),
        (Gate::P(1.2), Gate::P(0.3)),
        (Gate::Cp(1.2), Gate::Cp(0.3)),
        (Gate::Crz(1.2), Gate::Crz(0.3)),
    ];
    for (gate, quarter) in cases {
        assert_eq!(gate.powered(0.25), Some(quarter));
    }
    assert_eq!(Gate::H.powered(0.5), None);
}

#[test]
fn an_integer_power_repeats_the_gate() {
    assert_eq!(Gate::X.integer_power(3).unwrap(), vec![Gate::X; 3]);
    assert_eq!(Gate::T.integer_power(-2).unwrap(), vec![Gate::Tdg; 2]);
    assert_eq!(Gate::Rz(1.0).integer_power(2).unwrap(), vec![Gate::Rz(2.0)]);
    assert!(Gate::X.integer_power(100_000).is_err());
}

#[test]
fn clifford_detection_follows_the_angle() {
    assert!(Gate::Rz(PI / 2.0).is_clifford());
    assert!(Gate::Rz(-PI).is_clifford());
    assert!(!Gate::Rz(PI / 3.0).is_clifford());
    assert!(Gate::Cp(PI).is_clifford());
    assert!(!Gate::Cp(PI / 2.0).is_clifford());
    assert!(!Gate::T.is_clifford());
    assert!(Gate::Swap.is_clifford());
}

#[test]
fn fusing_single_qubit_gates_keeps_the_state() {
    let mut rng = StdRng::seed_from_u64(9090);
    for _ in 0..20 {
        let circuit = common::random_universal_circuit(&mut rng, 4, 40);
        let program = compile(&circuit).unwrap();
        let fused = fuse(&program);
        assert!(fused.len() <= program.len());
        let state = statevector(&circuit, &SimOptions::default()).unwrap();
        common::assert_close(&state, &common::dense_state(&circuit), 1e-11);
    }
}

#[test]
fn fusion_merges_a_run_of_single_qubit_gates() {
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", 2).unwrap();
    circuit.push_gate(Gate::H, &[0]).unwrap();
    circuit.push_gate(Gate::T, &[0]).unwrap();
    circuit.push_gate(Gate::S, &[0]).unwrap();
    circuit.push_gate(Gate::Cx, &[0, 1]).unwrap();
    circuit.push_gate(Gate::X, &[1]).unwrap();
    circuit.push_gate(Gate::Y, &[1]).unwrap();
    let fused = fuse(&compile(&circuit).unwrap());
    assert_eq!(fused.len(), 3, "three h/t/s, one cx, two x/y");
    assert_eq!(fused[0].label, "fused");
    assert!(matches!(fused[1].instruction, Instruction::Unitary(_)));
    assert_eq!(fused[2].label, "fused");
}

#[test]
fn a_measurement_stops_fusion() {
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", 1).unwrap();
    circuit.add_clbit_register("c", 1).unwrap();
    circuit.push_gate(Gate::H, &[0]).unwrap();
    circuit.push_measure(0, 0).unwrap();
    circuit.push_gate(Gate::H, &[0]).unwrap();
    let fused = fuse(&compile(&circuit).unwrap());
    assert_eq!(fused.len(), 3);
}

#[test]
fn a_gate_with_a_non_finite_angle_is_refused() {
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", 1).unwrap();
    assert!(circuit.push_gate(Gate::Rz(f64::NAN), &[0]).is_err());
    assert!(circuit.push_gate(Gate::Rz(f64::INFINITY), &[0]).is_err());
}

#[test]
fn a_two_qubit_gate_needs_two_different_qubits() {
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", 2).unwrap();
    assert!(circuit.push_gate(Gate::Cx, &[0, 0]).is_err());
    assert!(circuit.push_gate(Gate::Cx, &[0]).is_err());
    assert!(circuit.push_gate(Gate::Cx, &[0, 5]).is_err());
    assert!(circuit.push_gate(Gate::Cx, &[1, 0]).is_ok());
}

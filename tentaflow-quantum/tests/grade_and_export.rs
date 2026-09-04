// ===== File: tests/grade_and_export.rs — grading primitives and the Qiskit exporter =====

use std::collections::BTreeMap;
use std::f64::consts::{FRAC_1_SQRT_2, PI};

use num_complex::Complex64;
use tentaflow_quantum::export::qiskit_python;
use tentaflow_quantum::gate::Gate;
use tentaflow_quantum::grade;
use tentaflow_quantum::parse::{parse_qasm3, InputValues};
use tentaflow_quantum::sim::statevector::{circuit_unitary, statevector, SimOptions};
use tentaflow_quantum::sim::Cancel;

fn counts(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), *value))
        .collect()
}

#[test]
fn states_are_equal_up_to_a_global_phase() {
    let bell = vec![
        Complex64::new(FRAC_1_SQRT_2, 0.0),
        Complex64::new(0.0, 0.0),
        Complex64::new(0.0, 0.0),
        Complex64::new(FRAC_1_SQRT_2, 0.0),
    ];
    let phase = Complex64::from_polar(1.0, 1.234);
    let rotated: Vec<Complex64> = bell.iter().map(|z| z * phase).collect();
    assert!(grade::states_equal(&bell, &rotated, 1e-12).unwrap());
    assert!((grade::state_fidelity(&bell, &rotated).unwrap() - 1.0).abs() < 1e-12);

    let mut different = bell.clone();
    different[1] = Complex64::new(0.01, 0.0);
    assert!(!grade::states_equal(&bell, &different, 1e-6).unwrap());
    assert!(grade::states_equal(&bell, &bell, 1e-12).unwrap());
    assert!(grade::states_equal(&bell, &different, 0.02).unwrap());
}

#[test]
fn an_orthogonal_state_has_zero_fidelity() {
    let zero = vec![Complex64::new(1.0, 0.0), Complex64::new(0.0, 0.0)];
    let one = vec![Complex64::new(0.0, 0.0), Complex64::new(1.0, 0.0)];
    assert!(grade::state_fidelity(&zero, &one).unwrap() < 1e-15);
    assert!(!grade::states_equal(&zero, &one, 1e-9).unwrap());
}

#[test]
fn unitaries_are_compared_up_to_a_global_phase() {
    let mut circuit = tentaflow_quantum::ir::Circuit::new();
    circuit.add_qubit_register("q", 1).unwrap();
    circuit.push_gate(Gate::Rz(PI), &[0]).unwrap();
    let rz = circuit_unitary(&circuit, &SimOptions::default()).unwrap();

    let mut other = tentaflow_quantum::ir::Circuit::new();
    other.add_qubit_register("q", 1).unwrap();
    other.push_gate(Gate::Z, &[0]).unwrap();
    let z = circuit_unitary(&other, &SimOptions::default()).unwrap();

    // rz(pi) is Z up to a global phase, and only up to it: the entries differ.
    assert!(grade::unitaries_equal(&rz, &z, 1e-9).unwrap());
    assert!(rz.iter().zip(&z).any(|(a, b)| (a - b).norm() > 1e-6));

    let mut third = tentaflow_quantum::ir::Circuit::new();
    third.add_qubit_register("q", 1).unwrap();
    third.push_gate(Gate::X, &[0]).unwrap();
    let x = circuit_unitary(&third, &SimOptions::default()).unwrap();
    assert!(!grade::unitaries_equal(&rz, &x, 1e-9).unwrap());
}

#[test]
fn mismatched_dimensions_are_an_error_not_a_verdict() {
    let a = vec![Complex64::new(1.0, 0.0)];
    let b = vec![Complex64::new(1.0, 0.0), Complex64::new(0.0, 0.0)];
    assert!(grade::states_equal(&a, &b, 1e-9).is_err());
    assert!(grade::unitaries_equal(&a, &b, 1e-9).is_err());
    assert!(grade::state_fidelity(&a, &b).is_err());
}

#[test]
fn total_variation_distance_matches_the_definition() {
    let a = counts(&[("00", 500), ("11", 500)]);
    let b = counts(&[("00", 500), ("11", 500)]);
    assert!(grade::total_variation_distance(&a, &b).unwrap() < 1e-15);
    assert!((grade::hellinger_fidelity(&a, &b).unwrap() - 1.0).abs() < 1e-15);

    let c = counts(&[("00", 1000)]);
    assert!((grade::total_variation_distance(&a, &c).unwrap() - 0.5).abs() < 1e-12);

    let d = counts(&[("01", 1000)]);
    assert!((grade::total_variation_distance(&c, &d).unwrap() - 1.0).abs() < 1e-12);
    assert!(grade::hellinger_fidelity(&c, &d).unwrap() < 1e-15);

    // Different shot totals still compare as distributions.
    let e = counts(&[("00", 250), ("11", 250)]);
    assert!(grade::total_variation_distance(&a, &e).unwrap() < 1e-15);

    assert!(grade::total_variation_distance(&a, &BTreeMap::new()).is_err());
}

#[test]
fn an_empty_histogram_is_rejected() {
    let a = counts(&[("0", 1)]);
    assert!(grade::hellinger_fidelity(&BTreeMap::new(), &a).is_err());
}

#[test]
fn a_kata_solution_is_graded_against_the_reference_state() {
    let reference = parse_qasm3(
        "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[2] q;\nh q[0];\ncx q[0], q[1];\n",
        &InputValues::new(),
    )
    .unwrap();
    // The same Bell pair reached the long way round: the state matches even
    // though the circuit is a different operation.
    let other_route = parse_qasm3(
        "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[2] q;\nh q[1];\ncx q[1], q[0];\nswap q[0], q[1];\n",
        &InputValues::new(),
    )
    .unwrap();
    let expected = statevector(&reference, &SimOptions::default(), Cancel::none()).unwrap();
    let actual = statevector(&other_route, &SimOptions::default(), Cancel::none()).unwrap();
    assert!(grade::states_equal(&expected, &actual, 1e-9).unwrap());

    // cx = (I (x) h) cz (I (x) h), so this one is the same operation on every input.
    let same_operation = parse_qasm3(
        "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[2] q;\nh q[0];\nh q[1];\ncz q[0], q[1];\nh q[1];\n",
        &InputValues::new(),
    )
    .unwrap();
    assert!(grade::unitaries_equal(
        &circuit_unitary(&reference, &SimOptions::default()).unwrap(),
        &circuit_unitary(&same_operation, &SimOptions::default()).unwrap(),
        1e-9
    )
    .unwrap());
    assert!(!grade::unitaries_equal(
        &circuit_unitary(&reference, &SimOptions::default()).unwrap(),
        &circuit_unitary(&other_route, &SimOptions::default()).unwrap(),
        1e-9
    )
    .unwrap());
}

#[test]
fn the_qiskit_export_carries_registers_gates_and_control_flow() {
    let circuit = parse_qasm3(
        r#"
OPENQASM 3.0;
include "stdgates.inc";
qubit[2] q;
bit[2] c;
h q[0];
cx q[0], q[1];
u3(0.1, 0.2, 0.3) q[0];
inv @ sx q[1];
barrier q;
c[0] = measure q[0];
reset q[1];
if (c[0] == 1) { x q[1]; } else { z q[1]; }
if (c == 3) { h q[0]; } else { s q[0]; }
c[1] = measure q[1];
"#,
        &InputValues::new(),
    )
    .unwrap();
    let text = qiskit_python(&circuit);
    for expected in [
        "q = QuantumRegister(2, \"q\")",
        "c = ClassicalRegister(2, \"c\")",
        "circuit = QuantumCircuit(q, c)",
        "circuit.h(q[0])",
        "circuit.cx(q[0], q[1])",
        "circuit.u(0.1, 0.2, 0.3, q[0])",
        "circuit.sxdg(q[1])",
        "circuit.global_phase +=",
        "circuit.barrier(q[0], q[1])",
        "circuit.measure(q[0], c[0])",
        "circuit.reset(q[1])",
        "with circuit.if_test((c[0], 1)):",
        "with circuit.if_test((c[0], 0)):",
        "with circuit.if_test((c, 3)):",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }
    // The negated register guard becomes an else block.
    assert!(
        text.contains("as _else0:"),
        "missing else block in:\n{text}"
    );
    assert!(text.contains("with _else0:"));
}

#[test]
fn the_qiskit_export_indents_nested_guards() {
    let circuit = parse_qasm3(
        "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[1] q;\nbit[2] c;\nc[0] = measure q[0];\nif (c[0] == 1) { if (c[1] == 0) { x q[0]; } }\n",
        &InputValues::new(),
    )
    .unwrap();
    let text = qiskit_python(&circuit);
    assert!(text.contains("        circuit.x(q[0])"), "in:\n{text}");
}

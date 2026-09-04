// ===== File: tests/golden_circuits.rs — analytic golden states and the dense-matrix cross check =====

mod common;

use std::f64::consts::{FRAC_1_SQRT_2, PI, TAU};

use num_complex::Complex64;
use rand::rngs::StdRng;
use rand::SeedableRng;
use tentaflow_quantum::grade;
use tentaflow_quantum::parse::{parse_qasm3, InputValues};
use tentaflow_quantum::sim::statevector::{self, SimOptions};
use tentaflow_quantum::sim::Cancel;

fn parse(source: &str) -> tentaflow_quantum::ir::Circuit {
    parse_qasm3(source, &InputValues::new()).expect("the program is inside the supported subset")
}

fn header(qubits: usize) -> String {
    format!("OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[{qubits}] q;\n")
}

fn final_state(source: &str) -> Vec<Complex64> {
    let circuit = parse(source);
    let simulated =
        statevector::statevector(&circuit, &SimOptions::default(), Cancel::none()).unwrap();
    common::assert_close(&simulated, &common::dense_state(&circuit), 1e-12);
    simulated
}

#[test]
fn bell_state_is_analytic() {
    let state = final_state(&format!("{}h q[0];\ncx q[0], q[1];\n", header(2)));
    let expected = vec![
        Complex64::new(FRAC_1_SQRT_2, 0.0),
        Complex64::new(0.0, 0.0),
        Complex64::new(0.0, 0.0),
        Complex64::new(FRAC_1_SQRT_2, 0.0),
    ];
    common::assert_close(&state, &expected, 1e-12);
}

#[test]
fn ghz_state_is_analytic() {
    let state = final_state(&format!(
        "{}h q[0];\ncx q[0], q[1];\ncx q[1], q[2];\n",
        header(3)
    ));
    let mut expected = vec![Complex64::new(0.0, 0.0); 8];
    expected[0] = Complex64::new(FRAC_1_SQRT_2, 0.0);
    expected[7] = Complex64::new(FRAC_1_SQRT_2, 0.0);
    common::assert_close(&state, &expected, 1e-12);
}

fn qft_source(n: usize) -> String {
    let mut source = header(n);
    for j in (0..n).rev() {
        source.push_str(&format!("h q[{j}];\n"));
        for k in 0..j {
            let angle = PI / (1u64 << (j - k)) as f64;
            source.push_str(&format!("cp({angle}) q[{k}], q[{j}];\n"));
        }
    }
    for i in 0..n / 2 {
        source.push_str(&format!("swap q[{i}], q[{}];\n", n - 1 - i));
    }
    source
}

fn discrete_fourier_matrix(n: usize) -> Vec<Complex64> {
    let dim = 1usize << n;
    let norm = 1.0 / (dim as f64).sqrt();
    let mut matrix = vec![Complex64::new(0.0, 0.0); dim * dim];
    for row in 0..dim {
        for column in 0..dim {
            matrix[row * dim + column] =
                Complex64::from_polar(norm, TAU * (row * column) as f64 / dim as f64);
        }
    }
    matrix
}

#[test]
fn qft_on_three_qubits_is_the_fourier_transform() {
    let circuit = parse(&qft_source(3));
    let unitary = statevector::circuit_unitary(&circuit, &SimOptions::default()).unwrap();
    common::assert_close(&unitary, &common::dense_unitary(&circuit), 1e-12);
    common::assert_close(&unitary, &discrete_fourier_matrix(3), 1e-12);
    assert!(grade::unitaries_equal(&unitary, &discrete_fourier_matrix(3), 1e-9).unwrap());
}

#[test]
fn qft_state_amplitudes_carry_the_expected_phases() {
    // Prepare |5> and transform it: amplitude k must be exp(2 pi i * 5 k / 8)/sqrt(8).
    let mut source = header(3);
    source.push_str("x q[0];\nx q[2];\n");
    source.push_str(&qft_source(3)[header(3).len()..]);
    let state = final_state(&source);
    let norm = 1.0 / (8.0f64).sqrt();
    let expected: Vec<Complex64> = (0..8)
        .map(|k| Complex64::from_polar(norm, TAU * (5 * k) as f64 / 8.0))
        .collect();
    common::assert_close(&state, &expected, 1e-12);
}

const TELEPORTATION: &str = r#"
OPENQASM 3.0;
include "stdgates.inc";
qubit[3] q;
bit[3] c;
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

#[test]
fn teleportation_moves_a_basis_state_deterministically() {
    let source = TELEPORTATION.replace("h q[1];", "x q[0];\nh q[1];");
    let circuit = parse(&source);
    let options = SimOptions {
        seed: 7,
        ..SimOptions::default()
    };
    let result = statevector::run(&circuit, &options, 4096, Cancel::none()).unwrap();
    assert_eq!(result.shots, 4096);
    // The receiver is bit 2, rendered leftmost; it must be 1 on every shot.
    for (key, count) in &result.counts {
        assert!(
            key.starts_with('1'),
            "teleported |1> arrived as {key} on {count} shots"
        );
    }
    assert_eq!(result.counts.values().sum::<u64>(), 4096);
}

#[test]
fn teleportation_preserves_a_superposition_marginal() {
    let theta = PI / 3.0;
    let source = TELEPORTATION.replace("h q[1];", &format!("ry({theta}) q[0];\nh q[1];"));
    let circuit = parse(&source);
    let options = SimOptions {
        seed: 11,
        ..SimOptions::default()
    };
    let shots = 40_000;
    let result = statevector::run(&circuit, &options, shots, Cancel::none()).unwrap();
    let ones: u64 = result
        .counts
        .iter()
        .filter(|(key, _)| key.starts_with('1'))
        .map(|(_, count)| *count)
        .sum();
    let observed = ones as f64 / shots as f64;
    let expected = (theta / 2.0).sin().powi(2);
    assert!(
        (observed - expected).abs() < 0.01,
        "teleported P(1) = {observed}, expected {expected}"
    );
}

#[test]
fn simulator_agrees_with_the_dense_reference_on_random_circuits() {
    let mut rng = StdRng::seed_from_u64(20260903);
    for num_qubits in 1..=4 {
        for _ in 0..12 {
            let circuit = common::random_universal_circuit(&mut rng, num_qubits, 24);
            let simulated =
                statevector::statevector(&circuit, &SimOptions::default(), Cancel::none()).unwrap();
            common::assert_close(&simulated, &common::dense_state(&circuit), 1e-11);
            let unitary = statevector::circuit_unitary(&circuit, &SimOptions::default()).unwrap();
            common::assert_close(&unitary, &common::dense_unitary(&circuit), 1e-11);
        }
    }
}

#[test]
fn single_precision_tracks_double_precision() {
    let mut rng = StdRng::seed_from_u64(4242);
    let circuit = common::random_universal_circuit(&mut rng, 4, 30);
    let double =
        statevector::statevector(&circuit, &SimOptions::default(), Cancel::none()).unwrap();
    let single = statevector::statevector(
        &circuit,
        &SimOptions {
            precision: tentaflow_quantum::sim::Precision::Single,
            ..SimOptions::default()
        },
        Cancel::none(),
    )
    .unwrap();
    common::assert_close(&single, &double, 1e-5);
}

#[test]
fn a_circuit_with_measurements_has_no_single_state_vector() {
    let circuit = parse(&format!(
        "{}bit[1] c;\nh q[0];\nc[0] = measure q[0];\n",
        header(1)
    ));
    assert!(statevector::statevector(&circuit, &SimOptions::default(), Cancel::none()).is_err());
}

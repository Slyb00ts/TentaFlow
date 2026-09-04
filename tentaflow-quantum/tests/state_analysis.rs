// ===== File: tests/state_analysis.rs — stepping, fractional gates, keyframes and entanglement measures =====

mod common;

use std::f64::consts::{FRAC_1_SQRT_2, PI};

use num_complex::Complex64;
use rand::rngs::StdRng;
use rand::SeedableRng;
use tentaflow_quantum::gate::{Gate, Matrix};
use tentaflow_quantum::ir::Circuit;
use tentaflow_quantum::linalg;
use tentaflow_quantum::parse::{parse_qasm3, InputValues};
use tentaflow_quantum::sim::analysis::{self, Pauli};
use tentaflow_quantum::sim::statevector::{KeyframeOptions, PairSelection, SimOptions, Simulator};
use tentaflow_quantum::sim::Cancel;

fn parse(source: &str) -> Circuit {
    parse_qasm3(source, &InputValues::new()).expect("supported subset")
}

const MIXED: &str = r#"
OPENQASM 3.0;
include "stdgates.inc";
qubit[3] q;
h q[0];
t q[1];
cx q[0], q[1];
ry(0.7) q[2];
cp(1.1) q[1], q[2];
sx q[0];
cx q[2], q[0];
u3(0.3, 0.4, 0.5) q[1];
swap q[0], q[2];
"#;

#[test]
fn a_full_fraction_of_a_step_equals_the_step() {
    let circuit = parse(MIXED);
    let mut simulator = Simulator::new(&circuit, &SimOptions::default()).unwrap();
    while simulator.position() < simulator.step_count() {
        let preview = simulator.step_fraction(1.0).unwrap();
        assert!(simulator.step());
        common::assert_close(&preview, &simulator.amplitudes(), 1e-12);
    }
}

#[test]
fn a_zero_fraction_of_a_step_leaves_the_state_alone() {
    let circuit = parse(MIXED);
    let mut simulator = Simulator::new(&circuit, &SimOptions::default()).unwrap();
    for _ in 0..4 {
        let before = simulator.amplitudes();
        let preview = simulator.step_fraction(0.0).unwrap();
        common::assert_close(&preview, &before, 1e-12);
        assert!(simulator.step());
    }
}

#[test]
fn a_fractional_gate_stays_a_normalised_state() {
    let circuit = parse(MIXED);
    let mut simulator = Simulator::new(&circuit, &SimOptions::default()).unwrap();
    for _ in 0..circuit.ops().len() {
        for tenth in 0..=10 {
            let state = simulator.step_fraction(tenth as f64 / 10.0).unwrap();
            let norm: f64 = state.iter().map(|a| a.norm_sqr()).sum();
            assert!((norm - 1.0).abs() < 1e-10, "norm drifted to {norm}");
        }
        assert!(simulator.step());
    }
}

#[test]
fn half_of_a_rotation_is_the_half_angle_rotation() {
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", 1).unwrap();
    circuit.push_gate(Gate::Rx(PI / 2.0), &[0]).unwrap();
    let mut simulator = Simulator::new(&circuit, &SimOptions::default()).unwrap();
    let half = simulator.step_fraction(0.5).unwrap();

    let mut reference = Circuit::new();
    reference.add_qubit_register("q", 1).unwrap();
    reference.push_gate(Gate::Rx(PI / 4.0), &[0]).unwrap();
    let expected = tentaflow_quantum::sim::statevector::statevector(
        &reference,
        &SimOptions::default(),
        Cancel::none(),
    )
    .unwrap();
    common::assert_close(&half, &expected, 1e-12);
}

/// A bare `cx` is the smallest gate whose Cayley branch search lands off the
/// principal branch. |00> sits in its `+1` eigenspace, so the amplitude there
/// must not move at all while the slider crosses the gate; before the eigenphase
/// was folded into `(-pi, pi]` it wound a whole turn (1, -i, -1, +i, 1) between
/// the endpoints — invisible in every probability, plain in a phase-coloured
/// amplitude bar.
#[test]
fn a_bare_cx_never_winds_the_phase_of_the_state_it_leaves_alone() {
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", 2).unwrap();
    circuit.push_gate(Gate::Cx, &[0, 1]).unwrap();
    let mut simulator = Simulator::new(&circuit, &SimOptions::default()).unwrap();

    for twentieth in 0..=20 {
        let t = twentieth as f64 / 20.0;
        let state = simulator.step_fraction(t).unwrap();
        assert!(
            (state[0] - Complex64::new(1.0, 0.0)).norm() < 1e-12,
            "amp[0] at t = {t} is {}, not 1",
            state[0]
        );
    }

    let full = simulator.step_fraction(1.0).unwrap();
    assert!(simulator.step());
    common::assert_close(&full, &simulator.amplitudes(), 1e-12);
}

/// The short arc is a property of `unitary_power` itself, so it is asserted on
/// the matrix a gate WITHOUT an angle to scale takes: a plain rotation reached
/// through the Cayley branch still has to interpolate as the half-angle
/// rotation, not as its `2 pi` detour.
#[test]
fn a_powered_rotation_matrix_follows_the_short_arc() {
    let Matrix::One(full) = Gate::Rx(PI / 2.0).matrix() else {
        panic!("rx acts on one qubit")
    };
    let Matrix::One(quarter) = Gate::Rx(PI / 4.0).matrix() else {
        panic!("rx acts on one qubit")
    };
    common::assert_close(&linalg::unitary_power(&full, 2, 0.5), &quarter, 1e-12);
    common::assert_close(&linalg::unitary_power(&full, 2, 1.0), &full, 1e-12);
    common::assert_close(
        &linalg::unitary_power(&full, 2, 0.0),
        &linalg::identity(2),
        1e-12,
    );

    // Along the short arc the angle grows with `t`; the long way round would
    // overshoot and come back.
    let mut previous = 0.0;
    for twentieth in 1..=20 {
        let t = twentieth as f64 / 20.0;
        let powered = linalg::unitary_power(&full, 2, t);
        let angle = (2.0 * powered[0].re.clamp(-1.0, 1.0).acos()).abs();
        assert!(
            angle >= previous - 1e-12,
            "the rotation angle fell back from {previous} to {angle} at t = {t}"
        );
        previous = angle;
    }
    assert!(
        (previous - PI / 2.0).abs() < 1e-12,
        "the full power is not the full rotation: {previous}"
    );
}

#[test]
fn keyframe_bloch_vectors_match_the_reduced_density_matrices() {
    let circuit = parse(MIXED);
    let mut simulator = Simulator::new(&circuit, &SimOptions::default()).unwrap();
    let options = KeyframeOptions {
        pairs: PairSelection::All,
        top_k: 8,
        probs_top: 4,
    };
    loop {
        let keyframe = simulator.keyframe(&options).unwrap();
        assert_eq!(keyframe.step, simulator.position());
        assert_eq!(keyframe.bloch.len(), circuit.num_qubits());
        for qubit in 0..circuit.num_qubits() {
            let rho = simulator.reduced_density_matrix(&[qubit]).unwrap();
            let expected = linalg::bloch_vector(&rho);
            for (axis, (actual, wanted)) in keyframe.bloch[qubit].iter().zip(expected).enumerate() {
                assert!(
                    (actual - wanted).abs() < 1e-12,
                    "qubit {qubit} axis {axis}: {actual} vs {wanted}"
                );
            }
            assert!((keyframe.purity[qubit] - linalg::purity(&rho, 2)).abs() < 1e-12);
        }
        assert_eq!(keyframe.pairs.len(), 3);
        assert!(!keyframe.probs_top.is_empty() && keyframe.probs_top.len() <= 4);
        if !simulator.step() {
            break;
        }
    }
}

#[test]
fn keyframe_partners_are_the_indices_the_last_gate_mixed() {
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", 3).unwrap();
    circuit.push_gate(Gate::H, &[0]).unwrap();
    circuit.push_gate(Gate::Cx, &[0, 2]).unwrap();
    let mut simulator = Simulator::new(&circuit, &SimOptions::default()).unwrap();
    simulator.run_to_end();
    let keyframe = simulator
        .keyframe(&KeyframeOptions {
            pairs: PairSelection::GateQubits,
            top_k: 8,
            probs_top: 2,
        })
        .unwrap();
    let gate = keyframe.gate.expect("a gate was applied");
    assert_eq!(gate.name, "cx");
    assert_eq!(gate.qubits, vec![0, 2]);
    for group in &keyframe.top {
        assert_eq!(group.partners.len(), 3);
        for (index, _) in &group.partners {
            assert_ne!(*index, group.index);
            // Partners differ from the index only in the gate's qubits.
            assert_eq!((index ^ group.index) & !0b101, 0);
        }
    }
    assert_eq!(keyframe.pairs.len(), 1);
    assert_eq!(keyframe.pairs[0].qubits, (0, 2));
}

#[test]
fn a_bell_pair_is_maximally_entangled() {
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", 2).unwrap();
    circuit.push_gate(Gate::H, &[0]).unwrap();
    circuit.push_gate(Gate::Cx, &[0, 1]).unwrap();
    let mut simulator = Simulator::new(&circuit, &SimOptions::default()).unwrap();
    simulator.run_to_end();
    assert!((simulator.concurrence(0, 1).unwrap() - 1.0).abs() < 1e-9);
    assert!((simulator.mutual_information(0, 1).unwrap() - 2.0).abs() < 1e-9);
    for qubit in 0..2 {
        let bloch = simulator.bloch_vectors().unwrap()[qubit];
        let length = (bloch[0].powi(2) + bloch[1].powi(2) + bloch[2].powi(2)).sqrt();
        assert!(length < 1e-9, "an entangled qubit has no Bloch vector");
    }
}

#[test]
fn a_product_state_has_no_entanglement() {
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", 2).unwrap();
    circuit.push_gate(Gate::H, &[0]).unwrap();
    circuit.push_gate(Gate::Ry(0.9), &[1]).unwrap();
    let mut simulator = Simulator::new(&circuit, &SimOptions::default()).unwrap();
    simulator.run_to_end();
    assert!(simulator.concurrence(0, 1).unwrap() < 1e-9);
    assert!(simulator.mutual_information(0, 1).unwrap().abs() < 1e-9);
}

#[test]
fn pauli_expectations_match_the_analytic_values() {
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", 2).unwrap();
    circuit.push_gate(Gate::H, &[0]).unwrap();
    circuit.push_gate(Gate::Cx, &[0, 1]).unwrap();
    let mut simulator = Simulator::new(&circuit, &SimOptions::default()).unwrap();
    simulator.run_to_end();
    let cases = [
        (vec![(0, Pauli::Z), (1, Pauli::Z)], 1.0),
        (vec![(0, Pauli::X), (1, Pauli::X)], 1.0),
        (vec![(0, Pauli::Y), (1, Pauli::Y)], -1.0),
        (vec![(0, Pauli::Z)], 0.0),
        (vec![(0, Pauli::I)], 1.0),
    ];
    for (terms, expected) in cases {
        let value = simulator.pauli_expectation(&terms).unwrap();
        assert!(
            (value - expected).abs() < 1e-12,
            "{terms:?} gave {value}, expected {expected}"
        );
    }
}

#[test]
fn a_single_qubit_reduced_density_matrix_is_the_state_itself() {
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", 1).unwrap();
    circuit.push_gate(Gate::H, &[0]).unwrap();
    let mut simulator = Simulator::new(&circuit, &SimOptions::default()).unwrap();
    simulator.run_to_end();
    let rho = simulator.reduced_density_matrix(&[0]).unwrap();
    let half = Complex64::new(0.5, 0.0);
    common::assert_close(&rho, &[half, half, half, half], 1e-12);
    let bloch = linalg::bloch_vector(&rho);
    assert!((bloch[0] - 1.0).abs() < 1e-12);
    assert!(bloch[1].abs() < 1e-12);
    assert!(bloch[2].abs() < 1e-12);
}

#[test]
fn analysis_matches_a_direct_computation_on_random_states() {
    let mut rng = StdRng::seed_from_u64(777);
    for _ in 0..20 {
        let circuit = common::random_universal_circuit(&mut rng, 4, 20);
        let state = tentaflow_quantum::sim::statevector::statevector(
            &circuit,
            &SimOptions::default(),
            Cancel::none(),
        )
        .unwrap();
        let bloch = analysis::bloch_vectors(&state, 4).unwrap();
        for (qubit, actual) in bloch.iter().enumerate() {
            let rho = analysis::reduced_density_matrix(&state, 4, &[qubit]).unwrap();
            let expected = linalg::bloch_vector(&rho);
            for (component, wanted) in actual.iter().zip(expected) {
                assert!((component - wanted).abs() < 1e-11);
            }
            // A single-qubit reduced state always has unit trace.
            assert!((rho[0].re + rho[3].re - 1.0).abs() < 1e-11);
        }
        for i in 0..4 {
            for j in (i + 1)..4 {
                let information = analysis::mutual_information(&state, 4, i, j).unwrap();
                assert!(
                    (-1e-9..=2.0 + 1e-9).contains(&information),
                    "mutual information {information} out of range"
                );
                let concurrence = analysis::concurrence(&state, 4, i, j).unwrap();
                assert!((-1e-9..=1.0 + 1e-9).contains(&concurrence));
            }
        }
    }
}

#[test]
fn stepping_a_guarded_operation_respects_the_measured_bit() {
    let circuit = parse(
        "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[1] q;\nbit[1] c;\nx q[0];\nc[0] = measure q[0];\nif (c[0] == 0) { h q[0]; }\n",
    );
    let mut simulator = Simulator::new(&circuit, &SimOptions::default()).unwrap();
    simulator.run_to_end();
    assert_eq!(simulator.clbits(), &[true]);
    // The guard is false, so the state stays |1>.
    let amps = simulator.amplitudes();
    assert!((amps[1].norm() - 1.0).abs() < 1e-12);
}

#[test]
fn rewinding_restarts_the_program() {
    let circuit = parse(MIXED);
    let mut simulator = Simulator::new(&circuit, &SimOptions::default()).unwrap();
    simulator.run_to_end();
    let end = simulator.amplitudes();
    simulator.rewind();
    assert_eq!(simulator.position(), 0);
    assert!((simulator.amplitudes()[0].re - 1.0).abs() < 1e-12);
    simulator.run_to_end();
    common::assert_close(&simulator.amplitudes(), &end, 1e-12);
}

#[test]
fn a_hadamard_puts_the_bloch_vector_on_the_x_axis() {
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", 2).unwrap();
    circuit.push_gate(Gate::H, &[1]).unwrap();
    let mut simulator = Simulator::new(&circuit, &SimOptions::default()).unwrap();
    simulator.run_to_end();
    let bloch = simulator.bloch_vectors().unwrap();
    assert!((bloch[1][0] - 1.0).abs() < 1e-12);
    assert!((bloch[0][2] - 1.0).abs() < 1e-12);
    let amps = simulator.amplitudes();
    assert!((amps[0].re - FRAC_1_SQRT_2).abs() < 1e-12);
    assert!((amps[2].re - FRAC_1_SQRT_2).abs() < 1e-12);
}

#[test]
fn a_keyframe_reports_exactly_the_largest_amplitudes() {
    // The bounded selection has to pick the same entries a full sort would, ties
    // included, without ever sorting the whole state (plan 13.6 budgets one pass
    // per gate).
    let mut rng = StdRng::seed_from_u64(4711);
    let circuit = common::random_universal_circuit(&mut rng, 4, 24);
    let mut simulator = Simulator::new(&circuit, &SimOptions::default()).unwrap();
    simulator.run_to_end();
    let amps = simulator.amplitudes();
    let mut expected: Vec<usize> = (0..amps.len()).collect();
    expected.sort_by(|&a, &b| {
        amps[b]
            .norm_sqr()
            .total_cmp(&amps[a].norm_sqr())
            .then(a.cmp(&b))
    });

    for top_k in [0usize, 1, 5, 16, 40] {
        let keyframe = simulator
            .keyframe(&KeyframeOptions {
                pairs: PairSelection::None,
                top_k,
                probs_top: top_k,
            })
            .unwrap();
        let mut wanted: Vec<usize> = expected.iter().copied().take(top_k).collect();
        wanted.sort_unstable();
        let got: Vec<usize> = keyframe.top.iter().map(|group| group.index).collect();
        assert_eq!(got, wanted, "top {top_k} amplitudes");

        // Probabilities come back heaviest first and skip the impossible states.
        let probs: Vec<f64> = keyframe.probs_top.iter().map(|(_, p)| *p).collect();
        assert!(probs.windows(2).all(|pair| pair[0] >= pair[1]));
        assert!(probs.iter().all(|p| *p > 0.0));
        assert_eq!(
            probs.len(),
            top_k.min(amps.iter().filter(|a| a.norm_sqr() > 0.0).count())
        );
    }

    // A uniform superposition is all ties, and a tie goes to the lower index.
    let mut uniform = Circuit::new();
    uniform.add_qubit_register("q", 3).unwrap();
    for qubit in 0..3 {
        uniform.push_gate(Gate::H, &[qubit]).unwrap();
    }
    let mut simulator = Simulator::new(&uniform, &SimOptions::default()).unwrap();
    simulator.run_to_end();
    let keyframe = simulator
        .keyframe(&KeyframeOptions {
            pairs: PairSelection::None,
            top_k: 3,
            probs_top: 3,
        })
        .unwrap();
    let indices: Vec<usize> = keyframe.top.iter().map(|group| group.index).collect();
    assert_eq!(indices, vec![0, 1, 2]);
    let keys: Vec<&str> = keyframe
        .probs_top
        .iter()
        .map(|(key, _)| key.as_str())
        .collect();
    assert_eq!(keys, vec!["000", "001", "010"]);
}

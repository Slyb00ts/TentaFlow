// ===== File: tests/stabilizer_agreement.rs — the tableau must reproduce the state vector =====

mod common;

use rand::rngs::StdRng;
use rand::SeedableRng;
use tentaflow_quantum::gate::Gate;
use tentaflow_quantum::grade;
use tentaflow_quantum::ir::Circuit;
use tentaflow_quantum::sim::stabilizer::{self, StabilizerSim};
use tentaflow_quantum::sim::statevector::{self, SimOptions};
use tentaflow_quantum::sim::{Cancel, Device};

#[test]
fn stabilizer_and_statevector_agree_on_random_clifford_circuits() {
    let mut rng = StdRng::seed_from_u64(90210);
    for num_qubits in 1..=4 {
        for round in 0..8u64 {
            let circuit = common::random_clifford_circuit(&mut rng, num_qubits, 20);
            assert!(
                circuit.is_clifford(),
                "generator produced a non-Clifford gate"
            );
            let options = SimOptions {
                seed: 1000 + round,
                ..SimOptions::default()
            };
            let shots = 30_000;
            let exact =
                statevector::run(&circuit, &options, Device::Cpu, shots, Cancel::none()).unwrap();
            let tableau = stabilizer::run(&circuit, &options, shots, Cancel::none()).unwrap();
            let distance = grade::total_variation_distance(&exact.counts, &tableau.counts).unwrap();
            assert!(
                distance < 0.03,
                "TVD {distance} between state vector and tableau on {num_qubits} qubits"
            );
        }
    }
}

#[test]
fn tableau_reproduces_bell_correlations_exactly() {
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", 2).unwrap();
    circuit.add_clbit_register("c", 2).unwrap();
    circuit.push_gate(Gate::H, &[0]).unwrap();
    circuit.push_gate(Gate::Cx, &[0, 1]).unwrap();
    circuit.push_measure(0, 0).unwrap();
    circuit.push_measure(1, 1).unwrap();
    let result = stabilizer::run(&circuit, &SimOptions::default(), 2000, Cancel::none()).unwrap();
    assert_eq!(result.counts.len(), 2, "only 00 and 11 may appear");
    assert!(result.counts.contains_key("00"));
    assert!(result.counts.contains_key("11"));
}

#[test]
fn tableau_handles_reset_and_classical_control() {
    // Measure a superposition, reset it and steer a second qubit from the bit.
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", 2).unwrap();
    circuit.add_clbit_register("c", 2).unwrap();
    circuit.push_gate(Gate::H, &[0]).unwrap();
    circuit.push_measure(0, 0).unwrap();
    circuit.push_reset(0).unwrap();
    circuit
        .push(tentaflow_quantum::ir::Operation::with_conditions(
            tentaflow_quantum::ir::OpKind::Gate {
                gate: Gate::X,
                qubits: vec![1],
            },
            vec![tentaflow_quantum::ir::Condition::Bit {
                clbit: 0,
                value: true,
            }],
        ))
        .unwrap();
    circuit.push_measure(1, 1).unwrap();
    let options = SimOptions {
        seed: 5,
        ..SimOptions::default()
    };
    let tableau = stabilizer::run(&circuit, &options, 20_000, Cancel::none()).unwrap();
    // The second bit copies the first, so only 00 and 11 can occur.
    assert_eq!(tableau.counts.len(), 2);
    assert!(tableau.counts.contains_key("00"));
    assert!(tableau.counts.contains_key("11"));
    let exact = statevector::run(&circuit, &options, Device::Cpu, 20_000, Cancel::none()).unwrap();
    let distance = grade::total_variation_distance(&exact.counts, &tableau.counts).unwrap();
    assert!(distance < 0.03, "TVD {distance}");
}

#[test]
fn a_non_clifford_gate_is_refused_by_the_tableau() {
    let mut sim = StabilizerSim::new(2, 0);
    assert!(sim.apply_gate(Gate::T, &[0]).is_err());
    assert!(sim.apply_gate(Gate::H, &[0]).is_ok());

    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", 1).unwrap();
    circuit.add_clbit_register("c", 1).unwrap();
    circuit.push_gate(Gate::T, &[0]).unwrap();
    circuit.push_measure(0, 0).unwrap();
    assert!(!circuit.is_clifford());
    assert!(stabilizer::run(&circuit, &SimOptions::default(), 10, Cancel::none()).is_err());
}

/// Registers wider than a machine word are the tableau's headline workload
/// (plan 4.2 puts thousands of qubits on this path), so the count key may not be
/// packed into a `usize`.
fn wide_ghz(width: usize) -> Circuit {
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", width).unwrap();
    circuit.add_clbit_register("c", width).unwrap();
    circuit.push_gate(Gate::H, &[0]).unwrap();
    for target in 1..width {
        circuit.push_gate(Gate::Cx, &[0, target]).unwrap();
    }
    for qubit in 0..width {
        circuit.push_measure(qubit, qubit).unwrap();
    }
    circuit
}

#[test]
fn the_tableau_samples_a_register_wider_than_a_machine_word() {
    let width = 70;
    let shots = 64;
    let result = stabilizer::run(
        &wide_ghz(width),
        &SimOptions::default(),
        shots,
        Cancel::none(),
    )
    .unwrap();
    assert_eq!(result.counts.values().sum::<u64>(), shots);
    for key in result.counts.keys() {
        assert_eq!(key.len(), width);
        assert!(
            key.chars().all(|bit| bit == '0') || key.chars().all(|bit| bit == '1'),
            "a GHZ state only ever measures all-zero or all-one, got {key}"
        );
    }
}

#[test]
fn a_wide_count_key_puts_every_bit_at_its_own_position() {
    let width = 70;
    let excited = 64;
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", width).unwrap();
    circuit.add_clbit_register("c", width).unwrap();
    circuit.push_gate(Gate::X, &[excited]).unwrap();
    for qubit in 0..width {
        circuit.push_measure(qubit, qubit).unwrap();
    }
    let result = stabilizer::run(&circuit, &SimOptions::default(), 8, Cancel::none()).unwrap();
    assert_eq!(result.counts.len(), 1);
    let key = result.counts.keys().next().unwrap();
    assert_eq!(key.len(), width);
    assert_eq!(key.matches('1').count(), 1);
    assert_eq!(key.chars().rev().nth(excited), Some('1'));
}

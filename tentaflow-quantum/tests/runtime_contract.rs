// ===== File: tests/runtime_contract.rs — limits, error paths and wire serialisation of the IR =====

use tentaflow_quantum::error::Error;
use tentaflow_quantum::gate::Gate;
use tentaflow_quantum::ir::{Circuit, Condition, OpKind, Operation};
use tentaflow_quantum::parse::{parse_qasm3, InputValues};
use tentaflow_quantum::sim::statevector::{
    self, KeyframeOptions, PairSelection, SimOptions, Simulator,
};
use tentaflow_quantum::sim::Precision;

fn bell_with_measurement() -> Circuit {
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", 2).unwrap();
    circuit.add_clbit_register("c", 2).unwrap();
    circuit.push_gate(Gate::H, &[0]).unwrap();
    circuit.push_gate(Gate::Cx, &[0, 1]).unwrap();
    circuit.push_measure(0, 0).unwrap();
    circuit.push_measure(1, 1).unwrap();
    circuit
}

#[test]
fn a_circuit_wider_than_the_limit_is_refused_before_allocation() {
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", 12).unwrap();
    let options = SimOptions {
        max_qubits: 8,
        ..SimOptions::default()
    };
    match statevector::statevector(&circuit, &options).unwrap_err() {
        Error::TooManyQubits { qubits, limit } => {
            assert_eq!(qubits, 12);
            assert_eq!(limit, 8);
        }
        other => panic!("unexpected error {other}"),
    }
}

#[test]
fn sampling_needs_classical_bits_and_at_least_one_shot() {
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", 1).unwrap();
    circuit.push_gate(Gate::H, &[0]).unwrap();
    assert!(statevector::run(&circuit, &SimOptions::default(), 100).is_err());

    let with_bits = bell_with_measurement();
    assert!(statevector::run(&with_bits, &SimOptions::default(), 0).is_err());
    assert!(statevector::run(&with_bits, &SimOptions::default(), 1).is_ok());
}

#[test]
fn the_same_seed_gives_the_same_counts() {
    let circuit = bell_with_measurement();
    let options = SimOptions {
        seed: 12345,
        ..SimOptions::default()
    };
    let first = statevector::run(&circuit, &options, 5000).unwrap();
    let second = statevector::run(&circuit, &options, 5000).unwrap();
    assert_eq!(first.counts, second.counts);
    let other = statevector::run(
        &circuit,
        &SimOptions {
            seed: 999,
            ..options
        },
        5000,
    )
    .unwrap();
    assert_ne!(first.counts, other.counts);
    assert_eq!(first.counts.values().sum::<u64>(), 5000);
    assert_eq!(first.counts.keys().count(), 2);
}

#[test]
fn a_zero_qubit_circuit_cannot_be_simulated() {
    let circuit = Circuit::new();
    assert!(Simulator::new(&circuit, &SimOptions::default()).is_err());
}

#[test]
fn registers_must_be_named_and_sized() {
    let mut circuit = Circuit::new();
    assert!(circuit.add_qubit_register("q", 0).is_err());
    assert!(circuit.add_qubit_register("2q", 1).is_err());
    assert!(circuit.add_qubit_register("q", 2).is_ok());
    assert!(circuit.add_qubit_register("q", 2).is_err());
    assert_eq!(circuit.qubit_ref(1).unwrap(), "q[1]");
    assert!(circuit.qubit_ref(9).is_err());
}

#[test]
fn a_guard_on_a_missing_bit_is_refused() {
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", 1).unwrap();
    circuit.add_clbit_register("c", 1).unwrap();
    let bad = Operation::with_conditions(
        OpKind::Gate {
            gate: Gate::X,
            qubits: vec![0],
        },
        vec![Condition::Bit {
            clbit: 4,
            value: true,
        }],
    );
    assert!(circuit.push(bad).is_err());
}

#[test]
fn the_backend_reports_its_name_and_precision() {
    let circuit = bell_with_measurement();
    let double = Simulator::new(&circuit, &SimOptions::default()).unwrap();
    assert_eq!(double.backend_name(), "cpu");
    assert_eq!(double.precision(), Precision::Double);
    let single = Simulator::new(
        &circuit,
        &SimOptions {
            precision: Precision::Single,
            ..SimOptions::default()
        },
    )
    .unwrap();
    assert_eq!(single.precision(), Precision::Single);
}

#[test]
fn stepping_past_the_end_reports_it() {
    let circuit = bell_with_measurement();
    let mut simulator = Simulator::new(&circuit, &SimOptions::default()).unwrap();
    for _ in 0..simulator.step_count() {
        assert!(simulator.step());
    }
    assert!(!simulator.step());
    assert!(simulator.step_fraction(0.5).is_err());
}

#[test]
fn a_measurement_has_no_fractional_form() {
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", 1).unwrap();
    circuit.add_clbit_register("c", 1).unwrap();
    circuit.push_measure(0, 0).unwrap();
    let mut simulator = Simulator::new(&circuit, &SimOptions::default()).unwrap();
    assert!(simulator.step_fraction(0.5).is_err());
}

#[test]
fn the_ir_survives_a_json_round_trip() {
    let circuit = parse_qasm3(
        "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[2] q;\nbit[2] c;\nh q[0];\ncp(0.3) q[0], q[1];\nc[0] = measure q[0];\nif (c[0] == 1) { x q[1]; }\n",
        &InputValues::new(),
    )
    .unwrap();
    let encoded = serde_json::to_string(&circuit).unwrap();
    let decoded: Circuit = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, circuit);
    assert_eq!(decoded.to_qasm3(), circuit.to_qasm3());
}

#[test]
fn a_keyframe_survives_a_json_round_trip() {
    let circuit = bell_with_measurement();
    let mut simulator = Simulator::new(&circuit, &SimOptions::default()).unwrap();
    simulator.step();
    simulator.step();
    let keyframe = simulator
        .keyframe(&KeyframeOptions {
            pairs: PairSelection::Explicit(vec![(0, 1)]),
            top_k: 4,
            probs_top: 4,
        })
        .unwrap();
    let encoded = serde_json::to_string(&keyframe).unwrap();
    let decoded: tentaflow_quantum::sim::statevector::Keyframe =
        serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, keyframe);
    assert_eq!(decoded.pairs[0].qubits, (0, 1));
}

#[test]
fn an_invalid_pair_selection_is_refused() {
    let circuit = bell_with_measurement();
    let simulator = Simulator::new(&circuit, &SimOptions::default()).unwrap();
    assert!(simulator
        .keyframe(&KeyframeOptions {
            pairs: PairSelection::Explicit(vec![(0, 9)]),
            ..KeyframeOptions::default()
        })
        .is_err());
    assert!(simulator.reduced_density_matrix(&[0, 0]).is_err());
    assert!(simulator.reduced_density_matrix(&[0, 1, 2]).is_err());
}

#[test]
fn an_empty_barrier_is_refused_because_it_cannot_be_read_back() {
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", 2).unwrap();
    assert!(circuit.push_barrier(&[]).is_err());
    circuit.push_barrier(&[0, 1]).unwrap();
    let text = circuit.to_qasm3();
    assert!(text.contains("barrier q[0], q[1];"), "{text}");
    assert_eq!(
        parse_qasm3(&text, &InputValues::new()).unwrap(),
        circuit,
        "{text}"
    );
}

#[test]
fn the_backend_samples_the_distribution_it_holds() {
    use tentaflow_quantum::sim::cpu::CpuBackend;
    use tentaflow_quantum::sim::{Backend, GateOp};

    let mut backend: CpuBackend<f64> = CpuBackend::new(2);
    match Gate::H.matrix() {
        tentaflow_quantum::gate::Matrix::One(matrix) => {
            backend.apply(&[GateOp::One { qubit: 0, matrix }])
        }
        tentaflow_quantum::gate::Matrix::Two(_) => unreachable!("H is a 1-qubit gate"),
    }
    // Half the mass sits on |00>, half on |01>; a draw below the first half
    // lands on |00>, the rest on |01>, and a draw past the accumulated total
    // belongs to the last outcome that carried any mass.
    let draws = [0.0, 0.25, 0.499_999, 0.500_001, 0.9, 1.0 - f64::EPSILON];
    assert_eq!(backend.sample(&draws), vec![0, 0, 0, 1, 1, 1]);
    assert!(backend.sample(&[]).is_empty());
}

#[test]
fn the_ir_refuses_a_guarded_measurement_into_its_own_guard_bit() {
    // The invariant belongs to the IR, not only to the front end: a circuit
    // built programmatically (a generated kata solution) must not be able to
    // express a block that rewrites the bit its guard is evaluated against.
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", 1).unwrap();
    circuit.add_clbit_register("c", 2).unwrap();
    let error = circuit
        .push(Operation::with_conditions(
            OpKind::Measure { qubit: 0, clbit: 0 },
            vec![Condition::Bit {
                clbit: 0,
                value: true,
            }],
        ))
        .unwrap_err();
    assert!(
        error.to_string().contains("its own condition reads"),
        "unexpected error {error}"
    );

    // The same measurement under a register guard that covers the bit.
    let error = circuit
        .push(Operation::with_conditions(
            OpKind::Measure { qubit: 0, clbit: 1 },
            vec![Condition::Register {
                register: 0,
                value: 1,
                equal: true,
            }],
        ))
        .unwrap_err();
    assert!(
        error.to_string().contains("its own condition reads"),
        "unexpected error {error}"
    );
}

#[test]
fn the_ir_refuses_a_guard_on_a_register_wider_than_the_compared_value() {
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", 1).unwrap();
    circuit.add_clbit_register("c", 65).unwrap();
    let error = circuit
        .push(Operation::with_conditions(
            OpKind::Gate {
                gate: Gate::X,
                qubits: vec![0],
            },
            vec![Condition::Register {
                register: 0,
                value: 1,
                equal: true,
            }],
        ))
        .unwrap_err();
    assert!(
        error.to_string().contains("compares at most 64"),
        "unexpected error {error}"
    );
}

#[test]
fn a_guarded_block_runs_to_its_end() {
    // Every operation of an entered block must run: the guard is read once, at
    // block entry, and nothing inside it may change that answer.
    let circuit = parse_qasm3(
        concat!(
            "OPENQASM 3.0;\n",
            "include \"stdgates.inc\";\n",
            "qubit[2] q;\n",
            "bit[2] c;\n",
            "x q[0];\n",
            "c[0] = measure q[0];\n",
            "if (c[0] == 1) { x q[1]; z q[1]; }\n",
            "c[1] = measure q[1];\n"
        ),
        &InputValues::new(),
    )
    .unwrap();
    let result = statevector::run(&circuit, &SimOptions::default(), 64).unwrap();
    assert_eq!(
        result.counts.get("11"),
        Some(&64),
        "counts {:?}",
        result.counts
    );
}

#[test]
fn a_wide_classical_register_keeps_an_exact_count_key() {
    // The key is rendered from the bit image, so a register wider than a machine
    // word - the case the stabilizer path exists for - is neither truncated nor
    // wrapped on the state-vector path either.
    let width = 20;
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", width).unwrap();
    circuit.add_clbit_register("c", width).unwrap();
    circuit.push_gate(Gate::X, &[width - 1]).unwrap();
    for qubit in 0..width {
        circuit.push_measure(qubit, qubit).unwrap();
    }
    let result = statevector::run(&circuit, &SimOptions::default(), 8).unwrap();
    assert_eq!(result.counts.len(), 1);
    let key = result.counts.keys().next().unwrap();
    assert_eq!(key.len(), width);
    assert_eq!(key.matches('1').count(), 1);
    assert_eq!(key.chars().next(), Some('1'));
}

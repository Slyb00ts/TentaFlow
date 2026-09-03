// ===== File: tests/parse_and_qasm3.rs — subset coverage, diagnostics and canonical round trips =====

mod common;

use std::f64::consts::PI;

use rand::rngs::StdRng;
use rand::SeedableRng;
use tentaflow_quantum::error::Error;
use tentaflow_quantum::gate::Gate;
use tentaflow_quantum::ir::{Circuit, Condition, OpKind};
use tentaflow_quantum::parse::{parse_qasm3, InputValues};

fn parse(source: &str) -> Circuit {
    parse_qasm3(source, &InputValues::new()).expect("the program is inside the supported subset")
}

fn roundtrip(circuit: &Circuit) {
    let text = circuit.to_qasm3();
    let reparsed = parse_qasm3(&text, &InputValues::new())
        .unwrap_or_else(|error| panic!("canonical output does not parse: {error}\n{text}"));
    assert_eq!(
        &reparsed, circuit,
        "round trip changed the circuit:\n{text}"
    );
    assert_eq!(reparsed.to_qasm3(), text, "emission is not idempotent");
}

#[test]
fn every_supported_construct_round_trips() {
    let source = r#"
OPENQASM 3.0;
include "stdgates.inc";
input float theta;
const int width = 3;
qubit[3] q;
bit[3] c;
gate entangler(a) x, y { rz(a) x; cx x, y; }
for int i in [0:width - 1] { h q[i]; }
for int i in {0, 2} { rx(pi / 4) q[i]; }
entangler(theta) q[0], q[1];
ccx q[0], q[1], q[2];
cswap q[0], q[1], q[2];
u2(0.1, 0.2) q[0];
u3(0.3, 0.4, 0.5) q[1];
U(0.6, 0.7, 0.8) q[2];
gphase(0.25);
ctrl @ x q[0], q[1];
negctrl @ z q[1], q[2];
inv @ s q[0];
inv @ sx q[1];
pow(3) @ t q[2];
barrier q;
c[0] = measure q[0];
reset q[1];
if (c[0] == 1) { x q[1]; } else { z q[1]; }
if (c == 5) { y q[2]; } else { h q[2]; }
if (c[2]) { s q[0]; }
c = measure q;
"#;
    let mut inputs = InputValues::new();
    inputs.insert("theta".to_string(), 0.5);
    let circuit = parse_qasm3(source, &inputs).expect("supported subset");
    roundtrip(&circuit);
}

#[test]
fn random_circuits_round_trip_through_canonical_text() {
    let mut rng = StdRng::seed_from_u64(31337);
    for num_qubits in 1..=4 {
        for _ in 0..10 {
            let circuit = common::random_universal_circuit(&mut rng, num_qubits, 20);
            roundtrip(&circuit);
        }
    }
}

#[test]
fn awkward_float_values_survive_the_round_trip() {
    let mut circuit = Circuit::new();
    circuit.add_qubit_register("q", 1).unwrap();
    for angle in [
        0.0,
        1.0,
        -1.0,
        1e-7,
        1.5e12,
        PI,
        std::f64::consts::TAU,
        0.1 + 0.2,
        -3.999999999999999,
    ] {
        circuit.push_gate(Gate::Rz(angle), &[0]).unwrap();
    }
    roundtrip(&circuit);
}

#[test]
fn broadcast_applies_a_gate_to_a_whole_register() {
    let circuit = parse("OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[3] q;\nh q;\n");
    assert_eq!(circuit.ops().len(), 3);
    for (index, op) in circuit.ops().iter().enumerate() {
        match &op.kind {
            OpKind::Gate { gate, qubits } => {
                assert_eq!(*gate, Gate::H);
                assert_eq!(qubits, &vec![index]);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[test]
fn an_else_branch_becomes_the_negated_guard() {
    let circuit = parse(
        "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[1] q;\nbit[1] c;\nc[0] = measure q[0];\nif (c[0] == 1) { x q[0]; } else { h q[0]; }\n",
    );
    let guards: Vec<&Condition> = circuit
        .ops()
        .iter()
        .flat_map(|op| op.conditions.iter())
        .collect();
    assert_eq!(
        guards,
        vec![
            &Condition::Bit {
                clbit: 0,
                value: true
            },
            &Condition::Bit {
                clbit: 0,
                value: false
            }
        ]
    );
}

#[test]
fn an_unbound_input_is_reported_by_name() {
    let error = parse_qasm3(
        "OPENQASM 3.0;\ninclude \"stdgates.inc\";\ninput float theta;\nqubit[1] q;\nrx(theta) q[0];\n",
        &InputValues::new(),
    )
    .unwrap_err();
    assert!(
        error.position().is_none(),
        "a binding error has no position"
    );
    match error {
        Error::UnboundInput { name } => assert_eq!(name, "theta"),
        other => panic!("unexpected error {other}"),
    }
}

fn unsupported(source: &str) -> (String, u32) {
    let error = parse_qasm3(source, &InputValues::new()).unwrap_err();
    let position = error
        .position()
        .expect("a subset diagnostic always points at the source");
    match error {
        Error::Unsupported { pos, construct } => {
            assert_eq!(pos, position);
            (construct, pos.line)
        }
        other => panic!("expected an unsupported-construct error, got {other}"),
    }
}

#[test]
fn constructs_outside_the_subset_are_reported_with_a_line() {
    let prelude = "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[1] q;\nbit[1] c;\n";
    let cases = [
        ("while (c == 0) { x q[0]; }\n", "while"),
        ("duration d = 100ns;\n", "duration"),
        ("box { x q[0]; }\n", "box"),
        ("box[100ns] { x q[0]; }\n", "box"),
        ("cal { }\n", "cal"),
        ("defcal rx(angle) $0 { }\n", "defcal"),
        ("defcalgrammar \"openpulse\";\n", "defcalgrammar"),
        ("extern f(int[32]) -> int[32];\n", "extern"),
        ("delay[100ns] q[0];\n", "delay"),
        ("def f(int a) -> int { }\n", "def"),
        ("switch (c) { case 0 { x q[0]; } }\n", "switch"),
        ("let alias = q[0];\n", "let alias"),
        ("output bit o;\n", "output"),
    ];
    for (tail, expected) in cases {
        let (construct, line) = unsupported(&format!("{prelude}{tail}"));
        assert_eq!(construct, expected, "for source `{tail}`");
        assert_eq!(line, 5, "the diagnostic must point at the offending line");
    }
}

#[test]
fn a_diagnostic_points_at_the_column_too() {
    let (_, line) = unsupported(
        "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[1] q;\nbit[1] c;\n  while (c == 0) { x q[0]; }\n",
    );
    assert_eq!(line, 5);
    let error = parse_qasm3(
        "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[1] q;\nbit[1] c;\n  while (c == 0) { x q[0]; }\n",
        &InputValues::new(),
    )
    .unwrap_err();
    assert_eq!(error.position().unwrap().column, 3);
}

#[test]
fn only_stdgates_may_be_included() {
    let (construct, line) = unsupported(
        "OPENQASM 3.0;\ninclude \"stdgates.inc\";\ninclude \"secrets.qasm\";\nqubit[1] q;\n",
    );
    assert!(construct.contains("include"));
    assert_eq!(line, 3);
}

#[test]
fn a_syntax_error_carries_its_position() {
    let error = parse_qasm3(
        "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[1] q\nh q[0];\n",
        &InputValues::new(),
    )
    .unwrap_err();
    assert!(error.position().is_some());
    match error {
        Error::Syntax { pos, .. } => assert!(pos.line >= 3),
        other => panic!("expected a syntax error, got {other}"),
    }
}

#[test]
fn the_parser_panic_on_logic_operators_becomes_an_error() {
    let error = parse_qasm3(
        "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[1] q;\nbit[2] c;\nif (c[0] == 1 && c[1] == 0) { x q[0]; }\n",
        &InputValues::new(),
    )
    .unwrap_err();
    assert!(
        matches!(error, Error::ParserPanic { .. }),
        "unexpected error {error}"
    );
}

#[test]
fn a_three_qubit_gate_is_decomposed_into_one_and_two_qubit_gates() {
    let circuit =
        parse("OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[3] q;\nccx q[0], q[1], q[2];\n");
    assert!(circuit.ops().len() > 1);
    for op in circuit.ops() {
        assert!(
            op.qubits().len() <= 2,
            "{op:?} touches more than two qubits"
        );
    }
}

#[test]
fn ccx_is_the_standard_toffoli() {
    use tentaflow_quantum::sim::statevector::{circuit_unitary, SimOptions};
    let circuit =
        parse("OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[3] q;\nccx q[0], q[1], q[2];\n");
    let unitary = circuit_unitary(&circuit, &SimOptions::default()).unwrap();
    let dim = 8;
    for column in 0..dim {
        // Operand order is (control, control, target); q[0] and q[1] control q[2].
        let expected_row = if column & 0b011 == 0b011 {
            column ^ 0b100
        } else {
            column
        };
        for row in 0..dim {
            let value = unitary[row * dim + column];
            let target = if row == expected_row { 1.0 } else { 0.0 };
            assert!(
                (value.re - target).abs() < 1e-9 && value.im.abs() < 1e-9,
                "ccx[{row}][{column}] = {value}"
            );
        }
    }
}

#[test]
fn a_negated_control_over_nothing_leaves_the_state_alone() {
    use tentaflow_quantum::sim::statevector::{statevector, SimOptions};
    // `negctrl @` wraps its expansion in X on the control. An expansion that is
    // empty (a controlled identity) must not leave that X behind.
    let circuit =
        parse("OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[2] q;\nnegctrl @ id q[0], q[1];\n");
    assert!(
        circuit.ops().is_empty(),
        "a negated control on the identity is the identity: {:?}",
        circuit.ops()
    );
    let state = statevector(&circuit, &SimOptions::default()).unwrap();
    assert!((state[0].re - 1.0).abs() < 1e-12 && state[0].im.abs() < 1e-12);
    assert!(state[1..].iter().all(|a| a.norm() < 1e-12));
}

#[test]
fn a_negated_control_on_a_phase_only_gate_phases_the_zero_branch() {
    use tentaflow_quantum::sim::statevector::{statevector, SimOptions};
    let circuit = parse(
        "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[2] q;\ngate onlyphase a { gphase(0.3); }\nnegctrl @ onlyphase q[0], q[1];\n",
    );
    // X on the control, the phase, X back: three operations, not two.
    assert_eq!(circuit.ops().len(), 3, "{:?}", circuit.ops());
    let state = statevector(&circuit, &SimOptions::default()).unwrap();
    assert!((state[0].re - 0.3f64.cos()).abs() < 1e-12);
    assert!((state[0].im - 0.3f64.sin()).abs() < 1e-12);
    assert!(state[1..].iter().all(|a| a.norm() < 1e-12));
    roundtrip(&circuit);
}

#[test]
fn a_power_of_a_rotation_scales_its_angle() {
    let circuit =
        parse("OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[1] q;\npow(3) @ rz(0.2) q[0];\n");
    match circuit.ops() {
        [op] => match op.kind {
            OpKind::Gate {
                gate: Gate::Rz(angle),
                ref qubits,
            } => {
                assert!((angle - 0.6).abs() < 1e-12, "angle {angle}");
                assert_eq!(qubits, &vec![0]);
            }
            ref other => panic!("expected a scaled rotation, got {other:?}"),
        },
        other => panic!("expected one scaled rotation, got {other:?}"),
    }
}

#[test]
fn a_lowering_diagnostic_points_at_its_statement() {
    let prelude = "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[2] q;\nbit[2] c;\n";
    let cases = [
        "rx(0.1, 0.2) q[0];\n",
        "cx q[0], q[0];\n",
        "x q[5];\n",
        "ctrl @ cx q[0], q[1], q[0];\n",
        "barrier q[3];\n",
    ];
    for tail in cases {
        let error = parse_qasm3(&format!("{prelude}{tail}"), &InputValues::new()).unwrap_err();
        let pos = error
            .position()
            .unwrap_or_else(|| panic!("`{tail}` reported no position: {error}"));
        assert_eq!(pos.line, 5, "for source `{tail}`: {error}");
        assert_eq!(pos.column, 1, "for source `{tail}`: {error}");
    }
}

#[test]
fn a_diagnostic_inside_a_loop_points_at_the_loop() {
    let error = parse_qasm3(
        "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[2] q;\nfor int i in [0:2] {\n  x q[i];\n}\n",
        &InputValues::new(),
    )
    .unwrap_err();
    assert_eq!(error.position().unwrap().line, 4, "{error}");
}

#[test]
fn a_rejected_keyword_inside_a_comment_or_a_name_is_not_a_diagnostic() {
    // The lexical scan that names `box`, `cal`, `defcal`, `defcalgrammar` and
    // `extern` must not fire on prose or on an identifier that merely starts
    // with one of them.
    let circuit = parse(
        r#"
OPENQASM 3.0;
include "stdgates.inc";
// box and extern are words here
/* defcal too */
@my.note box cal extern
qubit[1] q;
gate boxed a { x a; }
boxed q[0];
"#,
    );
    assert_eq!(circuit.ops().len(), 1);
    roundtrip(&circuit);
}

#[test]
fn a_block_may_not_rewrite_the_bit_its_own_condition_reads() {
    // OpenQASM 3 evaluates the condition once, at block entry; the IR carries it
    // on every operation of the block, so a block that rewrites its own guard is
    // refused rather than executed halfway.
    let prelude = concat!(
        "OPENQASM 3.0;\n",
        "include \"stdgates.inc\";\n",
        "qubit[2] q;\n",
        "bit[2] c;\n",
        "x q[0];\n",
        "c[0] = measure q[0];\n"
    );
    let cases = [
        "if (c[0] == 1) { c[0] = measure q[0]; x q[1]; }\n",
        "if (c[0] == 1) { x q[1]; } else { c[0] = measure q[0]; }\n",
        "if (c == 1) { c[1] = measure q[1]; }\n",
        "if (c[0] == 1) { for int i in [0:0] { c[0] = measure q[0]; } }\n",
    ];
    for tail in cases {
        let error = parse_qasm3(&format!("{prelude}{tail}"), &InputValues::new())
            .expect_err(tail)
            .to_string();
        assert!(
            error.contains("its own condition reads"),
            "for source `{tail}`: {error}"
        );
        assert!(
            error.starts_with("line 7,"),
            "the diagnostic must point at the `if`: {error}"
        );
    }
}

#[test]
fn an_unguarded_measurement_into_a_bit_a_later_condition_reads_is_fine() {
    // Only a write from inside the guarded block is ambiguous; a measurement
    // before the `if` is exactly how a kata sets the bit it branches on.
    let circuit = parse(concat!(
        "OPENQASM 3.0;\n",
        "include \"stdgates.inc\";\n",
        "qubit[2] q;\n",
        "bit[2] c;\n",
        "h q[0];\n",
        "c[0] = measure q[0];\n",
        "if (c[0] == 1) { x q[1]; c[1] = measure q[1]; }\n"
    ));
    roundtrip(&circuit);
}

#[test]
fn a_condition_on_a_register_wider_than_the_compared_value_is_refused() {
    let source = concat!(
        "OPENQASM 3.0;\n",
        "include \"stdgates.inc\";\n",
        "qubit[1] q;\n",
        "bit[65] c;\n",
        "if (c == 1) { x q[0]; }\n"
    );
    let error = parse_qasm3(source, &InputValues::new()).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("compares at most 64"),
        "unexpected error {message}"
    );
    assert_eq!(error.position().unwrap().line, 5);
}

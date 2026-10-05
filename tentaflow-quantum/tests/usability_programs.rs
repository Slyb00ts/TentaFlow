// ===== File: tests/usability_programs.rs — programs a user would really write, checked against theory =====
//
// The other suites pin kernels and the parser one feature at a time. This one
// answers the question a laboratory user asks: "can I type a textbook
// algorithm in OpenQASM 3 and get the textbook answer?" Every program goes
// through the public path — text → parse_qasm3 → run / statevector — and is
// compared with a value derived on paper, never with the simulator itself.

use std::f64::consts::PI;

use num_complex::Complex64;
use tentaflow_quantum::parse::{parse_qasm3, InputValues};
use tentaflow_quantum::sim::statevector::{run, statevector, RunResult, SimOptions};
use tentaflow_quantum::sim::{stabilizer, Cancel, Device};
use tentaflow_quantum::Circuit;

fn parse(source: &str) -> Circuit {
    parse_qasm3(source, &InputValues::new())
        .unwrap_or_else(|error| panic!("a valid textbook program was rejected: {error}\n{source}"))
}

fn sample(source: &str, shots: u64, seed: u64) -> RunResult {
    let options = SimOptions {
        seed,
        ..SimOptions::default()
    };
    run(&parse(source), &options, Device::Cpu, shots, Cancel::none()).expect("the run finishes")
}

fn share(result: &RunResult, key: &str) -> f64 {
    *result.counts.get(key).unwrap_or(&0) as f64 / result.shots as f64
}

#[test]
fn bell_pair_gives_only_correlated_outcomes_in_equal_halves() {
    let result = sample(
        r#"
OPENQASM 3.0;
include "stdgates.inc";
qubit[2] q;
bit[2] c;
h q[0];
cx q[0], q[1];
c = measure q;
"#,
        20_000,
        7,
    );
    assert_eq!(result.counts.len(), 2, "only 00 and 11 may appear");
    assert!((share(&result, "00") - 0.5).abs() < 0.02);
    assert!((share(&result, "11") - 0.5).abs() < 0.02);
}

#[test]
fn ghz_on_twenty_qubits_collapses_to_all_zeros_or_all_ones() {
    let result = sample(
        r#"
OPENQASM 3.0;
include "stdgates.inc";
qubit[20] q;
bit[20] c;
h q[0];
for int i in [0:18] { cx q[i], q[i + 1]; }
c = measure q;
"#,
        5_000,
        1,
    );
    assert_eq!(result.counts.len(), 2);
    assert!((share(&result, &"0".repeat(20)) - 0.5).abs() < 0.03);
    assert!((share(&result, &"1".repeat(20)) - 0.5).abs() < 0.03);
}

#[test]
fn stabilizer_path_handles_a_hundred_qubit_ghz() {
    let mut source = String::from("OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[100] q;\nbit[100] c;\nh q[0];\n");
    source.push_str("for int i in [0:98] { cx q[i], q[i + 1]; }\nc = measure q;\n");
    let circuit = parse(&source);
    assert!(circuit.is_clifford());
    let result = stabilizer::run(&circuit, &SimOptions::default(), 100, Cancel::none())
        .expect("a Clifford circuit of a hundred qubits runs");
    assert_eq!(result.counts.len(), 2, "GHZ never yields a mixed string");
    assert!(result.counts.contains_key(&"0".repeat(100)));
    assert!(result.counts.contains_key(&"1".repeat(100)));
}

#[test]
fn bernstein_vazirani_recovers_the_secret_string_in_one_query() {
    // Secret s = 1011 (q0 is the rightmost character of the result).
    let result = sample(
        r#"
OPENQASM 3.0;
include "stdgates.inc";
qubit[5] q;
bit[4] c;
x q[4];
h q;
cx q[0], q[4];
cx q[1], q[4];
cx q[3], q[4];
h q[0:3];
c = measure q[0:3];
"#,
        500,
        3,
    );
    assert_eq!(result.counts.len(), 1, "the oracle is deterministic");
    assert_eq!(share(&result, "1011"), 1.0);
}

#[test]
fn grover_on_three_qubits_finds_the_marked_item_with_theoretical_probability() {
    // Marked |101>; the optimal number of iterations for N = 8 is 2 and then
    // sin^2(5 * asin(1/sqrt(8))) = 0.9453.
    let result = sample(
        r#"
OPENQASM 3.0;
include "stdgates.inc";
qubit[3] q;
bit[3] c;
h q;
for int round in [0:1] {
    x q[1];
    h q[2]; ccx q[0], q[1], q[2]; h q[2];
    x q[1];
    h q; x q;
    h q[2]; ccx q[0], q[1], q[2]; h q[2];
    x q; h q;
}
c = measure q;
"#,
        20_000,
        11,
    );
    let theory = (5.0 * (1.0f64 / 8.0f64.sqrt()).asin()).sin().powi(2);
    assert!(
        (share(&result, "101") - theory).abs() < 0.02,
        "marked item share {} vs theory {theory}",
        share(&result, "101")
    );
}

#[test]
fn quantum_fourier_transform_matches_the_analytic_amplitudes() {
    // QFT|k> = 1/sqrt(N) * sum_j exp(2 pi i j k / N) |j>, here N = 16, k = 5.
    let n = 4usize;
    let k = 5usize;
    let mut source = String::from("OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[4] q;\n");
    // |k> = |0101>: qubit 0 and qubit 2 set.
    source.push_str("x q[0]; x q[2];\n");
    for target in (0..n).rev() {
        source.push_str(&format!("h q[{target}];\n"));
        for control in (0..target).rev() {
            let angle = PI / (1u32 << (target - control)) as f64;
            source.push_str(&format!("cp({angle}) q[{control}], q[{target}];\n"));
        }
    }
    source.push_str("swap q[0], q[3]; swap q[1], q[2];\n");
    let amplitudes = statevector(
        &parse(&source),
        &SimOptions::default(),
        Device::Cpu,
        Cancel::none(),
    )
    .expect("a unitary circuit has a final state");
    let dim = 1usize << n;
    let mut reference: Vec<Complex64> = (0..dim)
        .map(|j| Complex64::from_polar(1.0 / (dim as f64).sqrt(), 2.0 * PI * (j * k) as f64 / dim as f64))
        .collect();
    // Compare up to a global phase, which the program may legitimately carry.
    let phase = amplitudes[0] / reference[0];
    for (got, want) in amplitudes.iter().zip(reference.iter_mut()) {
        assert!((*got - *want * phase).norm() < 1e-9, "amplitude {got} vs {want}");
    }
}

#[test]
fn teleportation_with_classical_feed_forward_moves_the_state() {
    // Alice teleports ry(theta)|0>; Bob's qubit must end with P(1) = sin^2(theta/2)
    // whatever the two mid-circuit measurements returned.
    let theta = 0.7f64;
    let source = format!(
        r#"
OPENQASM 3.0;
include "stdgates.inc";
qubit[3] q;
bit[2] m;
bit[1] out;
ry({theta}) q[0];
h q[1];
cx q[1], q[2];
cx q[0], q[1];
h q[0];
m[0] = measure q[0];
m[1] = measure q[1];
if (m[1] == 1) {{ x q[2]; }}
if (m[0] == 1) {{ z q[2]; }}
ry({neg}) q[2];
out[0] = measure q[2];
"#,
        neg = -theta
    );
    // Undoing the preparation on Bob's side must give |0> with certainty.
    let result = sample(&source, 4_000, 5);
    // Keys are the whole classical register (`out` is the leftmost bit), and
    // the two measured bits must come out uniformly random.
    let bob_zero: f64 = result
        .counts
        .iter()
        .filter(|(key, _)| key.starts_with('0'))
        .map(|(_, count)| *count as f64)
        .sum::<f64>()
        / result.shots as f64;
    assert_eq!(bob_zero, 1.0, "teleported state was damaged: {:?}", result.counts);
    assert_eq!(result.counts.len(), 4, "Alice's two bits must take all four values");
}

#[test]
fn parameters_gate_definitions_and_loops_work_together() {
    let source = r#"
OPENQASM 3.0;
include "stdgates.inc";
input float theta;
gate rot(a) x { rx(a) x; }
qubit[1] q;
bit[1] c;
rot(theta) q[0];
c[0] = measure q[0];
"#;
    let mut inputs = InputValues::new();
    inputs.insert("theta".to_string(), PI / 2.0);
    let circuit = parse_qasm3(source, &inputs).expect("bound input");
    let result = run(&circuit, &SimOptions::default(), Device::Cpu, 20_000, Cancel::none()).unwrap();
    // rx(pi/2)|0> has P(1) = sin^2(pi/4) = 0.5.
    assert!((share(&result, "1") - 0.5).abs() < 0.02);
}

#[test]
fn a_mistake_is_reported_with_its_line_instead_of_a_panic() {
    let error = parse_qasm3(
        "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[2] q;\nh q[0];\nfrobnicate q[1];\n",
        &InputValues::new(),
    )
    .expect_err("an unknown gate is a user error");
    let position = error.position().expect("the diagnostic points at the line");
    assert_eq!(position.line, 5);
}

#[test]
fn the_same_seed_reproduces_counts_and_another_seed_does_not() {
    let program = "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[1] q;\nbit[1] c;\nh q[0];\nc[0] = measure q[0];\n";
    let first = sample(program, 1_000, 42);
    let again = sample(program, 1_000, 42);
    let other = sample(program, 1_000, 43);
    assert_eq!(first.counts, again.counts);
    assert_ne!(first.counts, other.counts);
}

#[test]
fn a_circuit_over_the_ceiling_is_refused_before_allocating() {
    let options = SimOptions {
        max_qubits: 10,
        ..SimOptions::default()
    };
    let circuit = parse("OPENQASM 3.0;\nqubit[12] q;\nbit[12] c;\nc = measure q;\n");
    assert!(run(&circuit, &options, Device::Cpu, 10, Cancel::none()).is_err());
}

#[test]
fn a_subset_of_a_register_can_be_measured_into_a_register() {
    // Index sets keep their order: bit 0 gets qubit 2, bit 1 gets qubit 0.
    let result = sample(
        r#"
OPENQASM 3.0;
include "stdgates.inc";
qubit[3] q;
bit[2] c;
x q[2];
c = measure q[{2, 0}];
"#,
        100,
        1,
    );
    assert_eq!(share(&result, "01"), 1.0, "{:?}", result.counts);
}

#[test]
fn a_slice_of_the_wrong_size_is_refused_with_a_message() {
    let error = parse_qasm3(
        "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[5] q;\nbit[4] c;\nc = measure q[0:2];\n",
        &InputValues::new(),
    )
    .expect_err("three qubits cannot fill four bits");
    assert!(error.to_string().contains("3 bit(s) into 4"), "{error}");
}

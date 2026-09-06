// ===== File: tests/wasm_api.rs — browser-surface behaviour only wasm can assert =====
//
// `tests/wasm_parity.rs` pins the NUMBERS the simulator produces; this file pins
// the wasm-bindgen layer around them, which exists only under the `wasm` feature
// and only means anything inside a JavaScript engine:
//
//   cargo test --target wasm32-unknown-unknown --features wasm --test wasm_api
//
// Two things there are easy to get wrong and silent when they are:
//
//  * `amplitudes_to_js` reinterprets `&[Complex64]` as `&[f64]` and copies it in
//    one memcpy. A const assertion in `src/wasm.rs` pins the layout; only a run
//    can pin the ORDER, i.e. that real parts land on even indices.
//  * the Bloch pass is `O(n * 2^n)`, so its result is cached and every method
//    that moves the register must drop the cache. A stale vector would draw the
//    previous step's sphere with nothing reporting an error.
//
// Natively the whole file compiles away: there is no JavaScript to bind to.

#![cfg(all(target_arch = "wasm32", feature = "wasm"))]

use wasm_bindgen_test::wasm_bindgen_test;

use tentaflow_quantum::wasm::{parse, WasmSimulator};

/// How close two amplitudes have to be. The register is `f64` and the circuits
/// below are a handful of gates, so the slack is for rounding only.
const TOLERANCE: f64 = 1e-12;

fn ir(qasm: &str) -> String {
    let envelope: serde_json::Value =
        serde_json::from_str(&parse(qasm, None).expect("the parser runs")).expect("valid JSON");
    assert_eq!(
        envelope["status"], "parsed",
        "fixture circuit was rejected: {}",
        envelope["errors"]
    );
    envelope["circuit"].to_string()
}

fn simulator(qasm: &str) -> WasmSimulator {
    WasmSimulator::new(&ir(qasm), None).expect("the register fits")
}

fn close(got: f64, want: f64, what: &str) {
    assert!(
        (got - want).abs() < TOLERANCE,
        "{what}: got {got}, want {want}"
    );
}

/// `h; s` makes (|0> + i|1>)/sqrt(2), whose two amplitudes have their weight in
/// DIFFERENT components — the only shape that can tell an interleaved
/// `[re, im, re, im]` from a transposed `[re, re, im, im]` or a swapped pair.
#[wasm_bindgen_test]
fn amplitudes_cross_as_interleaved_real_then_imaginary() {
    let mut sim =
        simulator("OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[1] q;\nh q[0];\ns q[0];\n");
    sim.run_to_end();
    let flat = sim.amplitudes().to_vec();
    assert_eq!(flat.len(), 4, "one qubit is two amplitudes, four numbers");
    let half = std::f64::consts::FRAC_1_SQRT_2;
    close(flat[0], half, "Re(a0)");
    close(flat[1], 0.0, "Im(a0)");
    close(flat[2], 0.0, "Re(a1)");
    close(flat[3], half, "Im(a1)");
}

/// Every qubit's vector, in qubit order, in one array of `3n` numbers.
#[wasm_bindgen_test]
fn bloch_vectors_carry_every_qubit_in_index_order() {
    let mut sim =
        simulator("OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[3] q;\nh q[0];\nx q[1];\n");
    sim.run_to_end();
    let all = sim
        .bloch_vectors()
        .expect("the state is available")
        .to_vec();
    assert_eq!(all.len(), 9, "three qubits is nine numbers");
    // q0 sits on +X after the Hadamard, q1 on -Z after the X, q2 is untouched.
    for (qubit, want) in [[1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 0.0, 1.0]]
        .iter()
        .enumerate()
    {
        for axis in 0..3 {
            close(all[qubit * 3 + axis], want[axis], "blochVectors");
        }
        // The single-qubit accessor must read out of the same pass.
        let one = sim.bloch(qubit).expect("qubit is in range").to_vec();
        assert_eq!(one, all[qubit * 3..qubit * 3 + 3], "bloch({qubit})");
    }
    assert!(
        sim.bloch(3).is_err(),
        "a qubit past the register is refused"
    );
}

/// The cache exists for speed; this is the part that makes it correct.
#[wasm_bindgen_test]
fn every_move_of_the_register_drops_the_bloch_cache() {
    let mut sim = simulator(
        "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[2] q;\nh q[0];\ncx q[0], q[1];\n",
    );
    let z = |sim: &mut WasmSimulator| sim.bloch(0).expect("qubit 0").to_vec()[2];
    let x = |sim: &mut WasmSimulator| sim.bloch(0).expect("qubit 0").to_vec()[0];

    close(z(&mut sim), 1.0, "|0> before any step");

    assert!(sim.step(), "the Hadamard is pending");
    close(x(&mut sim), 1.0, "step must drop the cache");
    close(z(&mut sim), 0.0, "|+> has no Z component");

    assert!(sim.step(), "the CNOT is pending");
    close(x(&mut sim), 0.0, "a Bell half is maximally mixed");

    sim.rewind();
    close(z(&mut sim), 1.0, "rewind must drop the cache");

    sim.run_to_end();
    close(x(&mut sim), 0.0, "runToEnd must drop the cache");
}

/// A keyframe runs the same pass and reports the same vectors, so it fills the
/// cache instead of leaving a stale one behind.
#[wasm_bindgen_test]
fn a_keyframe_leaves_the_cache_agreeing_with_what_it_reported() {
    let mut sim = simulator(
        "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[2] q;\nh q[0];\ncx q[0], q[1];\n",
    );
    sim.step();
    let keyframe: serde_json::Value =
        serde_json::from_str(&sim.keyframe(None).expect("the state is available"))
            .expect("valid JSON");
    let reported = keyframe["bloch"].as_array().expect("bloch is a list");
    let cached = sim
        .bloch_vectors()
        .expect("the state is available")
        .to_vec();
    assert_eq!(reported.len() * 3, cached.len());
    for (qubit, vector) in reported.iter().enumerate() {
        for axis in 0..3 {
            let want = vector[axis].as_f64().expect("a component is a number");
            assert_eq!(cached[qubit * 3 + axis], want, "q{qubit} axis {axis}");
        }
    }
}

// ===== File: tests/wasm_parity.rs — native<->wasm shot-stream parity (plan 6.2) =====
//
// The browser (T0) and a node (T1) must produce the SAME counts for the same
// circuit and the same seed, or "wyniki są bitowo zgodne z T0" (plan 16, Faza 1)
// is not a criterion anyone can check. Nothing about the shot stream is
// platform-dependent: `StdRng` is ChaCha12 over the seed, the draws are ordered
// with `f64::total_cmp` and the state vector is IEEE-754 f32/f64 arithmetic in a
// fixed order, on wasm32 as on a native target.
//
// The SAME test therefore runs on both. Natively it is a plain `cargo test`;
// on wasm32 `wasm_bindgen_test` runs it inside the module:
//
//   cargo test --target wasm32-unknown-unknown --features wasm --test wasm_parity
//
// The expectations live in `tests/golden/wasm_parity.json` and are compiled in
// with `include_str!`, because wasm32 has no filesystem to read them from.
// `scripts/wasm-bench.mjs` checks the same file through the JavaScript API, so
// the simulator and the bindings around it are both pinned to one artefact.

use std::collections::BTreeMap;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen_test::wasm_bindgen_test;

use serde::Deserialize;
use tentaflow_quantum::parse::{parse_qasm3, InputValues};
use tentaflow_quantum::sim::statevector::{run, SimOptions};
use tentaflow_quantum::sim::{Cancel, Precision};

#[derive(Debug, Deserialize)]
struct Fixture {
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
struct Case {
    name: String,
    qasm: String,
    shots: u64,
    seed: u64,
    precision: String,
    counts: BTreeMap<String, u64>,
}

const FIXTURE: &str = include_str!("golden/wasm_parity.json");

fn fixture() -> Fixture {
    serde_json::from_str(FIXTURE).expect("the parity fixture is valid JSON")
}

#[test]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn this_build_reproduces_the_parity_fixture() {
    let fixture = fixture();
    assert!(
        !fixture.cases.is_empty(),
        "the parity fixture must carry at least one case"
    );
    for case in &fixture.cases {
        let circuit = parse_qasm3(&case.qasm, &InputValues::new())
            .unwrap_or_else(|e| panic!("case `{}` is outside the subset: {e}", case.name));
        let options = SimOptions {
            precision: match case.precision.as_str() {
                "single" => Precision::Single,
                "double" => Precision::Double,
                other => panic!("case `{}` names precision `{other}`", case.name),
            },
            max_qubits: tentaflow_quantum::sim::statevector::DEFAULT_MAX_QUBITS,
            seed: case.seed,
        };
        let result = run(&circuit, &options, case.shots, Cancel::none())
            .unwrap_or_else(|e| panic!("case `{}` failed to run: {e}", case.name));
        assert_eq!(
            result.counts, case.counts,
            "case `{}` no longer produces the recorded counts",
            case.name
        );
        assert_eq!(result.shots, case.shots, "case `{}`", case.name);
    }
}

#[test]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn the_shot_stream_only_depends_on_the_seed() {
    let case = &fixture().cases[0];
    let circuit = parse_qasm3(&case.qasm, &InputValues::new()).expect("in subset");
    let options = SimOptions {
        precision: Precision::Double,
        max_qubits: tentaflow_quantum::sim::statevector::DEFAULT_MAX_QUBITS,
        seed: case.seed,
    };
    let first = run(&circuit, &options, case.shots, Cancel::none()).expect("runs");
    let second = run(&circuit, &options, case.shots, Cancel::none()).expect("runs");
    assert_eq!(first.counts, second.counts);

    let other = SimOptions {
        seed: case.seed + 1,
        ..options
    };
    let shifted = run(&circuit, &other, case.shots, Cancel::none()).expect("runs");
    assert_ne!(
        first.counts, shifted.counts,
        "a different seed must draw a different shot stream"
    );
}

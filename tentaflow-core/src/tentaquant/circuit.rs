// ===== File: tentaquant/circuit.rs — the OpenQASM 3 front end of tier T1 =====
//
// Validation, export and the simulation itself all start here, and all three
// run the SAME crate the browser runs (`tentaflow-quantum`, compiled natively
// for Core and to wasm32 for the editor). That is the point of plan §4.1: one
// artefact, one front end, one set of numbers — a circuit rejected in the
// editor is rejected here with the same message and the same line.
//
// Capacity is decided BEFORE anything is allocated (plan §4.2): a circuit over
// the laboratory's `max_qubits_core` is a validation error naming the ceiling
// and the tiers above, never an out-of-memory kill halfway through a run.

use anyhow::{anyhow, Result};
use tentaflow_protocol::tentaquant::{
    CircuitDiagnostic, SimulateOptions, KEYFRAME_ALL_PAIRS_QUBITS, KEYFRAME_DEFAULT_QUBITS,
    MAX_KEYFRAME_PROBS_TOP, MAX_KEYFRAME_TOP_K,
};
use tentaflow_quantum::parse::{parse_qasm3, InputValues};
use tentaflow_quantum::sim::statevector::{SimOptions, DEFAULT_MAX_QUBITS};
use tentaflow_quantum::sim::Precision;
use tentaflow_quantum::{Circuit, Error};

/// What the front end made of one program.
#[derive(Debug)]
pub struct Parsed {
    pub circuit: Circuit,
    pub ir_json: String,
}

/// Diagnostic class the editor colours by. The kinds are the crate's error
/// variants, not a reinterpretation of their text.
fn kind_of(error: &Error) -> &'static str {
    match error {
        Error::Syntax { .. } => "syntax",
        Error::Semantic { .. } => "semantic",
        Error::Unsupported { .. } => "unsupported",
        Error::ParserPanic { .. } => "parser",
        Error::UnboundInput { .. } => "input",
        Error::Invalid(_) => "invalid",
        Error::TooManyQubits { .. } => "capacity",
        Error::NotClifford { .. } => "not_clifford",
        // Not a diagnostic about the program: the run was stopped from
        // outside. `execute` turns it into the run's outcome and never asks
        // the editor to draw it.
        Error::Cancelled => "cancelled",
    }
}

/// One rejection as the editor draws it: class, message and the 1-based
/// position when the front end could place it.
pub fn diagnostic(error: &Error) -> CircuitDiagnostic {
    let position = error.position();
    CircuitDiagnostic {
        kind: kind_of(error).to_string(),
        message: error.to_string(),
        line: position.map(|p| p.line),
        column: position.map(|p| p.column),
    }
}

/// A capacity refusal, phrased as the diagnostic the run view shows instead of
/// starting a run it cannot finish (plan §4.2). It names the ceiling that was
/// exceeded AND where a bigger circuit would have to go, because "too large"
/// without a next step is not an answer a user can act on.
pub fn capacity_diagnostic(qubits: u32, max_qubits: u32) -> CircuitDiagnostic {
    CircuitDiagnostic {
        kind: "capacity".to_string(),
        message: format!(
            "circuit needs {qubits} qubits, this laboratory allows at most {max_qubits} on the \
             Core tier (T1); the tiers that could take it — T2 (Python) and T3 (GPU) — are not \
             available in this laboratory yet"
        ),
        line: None,
        column: None,
    }
}

/// Binds `input float` parameters from the JSON object the client sent. An
/// empty string means "no parameters", which is the common case.
pub fn input_values(inputs_json: &str) -> Result<InputValues> {
    if inputs_json.trim().is_empty() {
        return Ok(InputValues::new());
    }
    serde_json::from_str::<InputValues>(inputs_json)
        .map_err(|e| anyhow!("input values must be a JSON object of name → number: {e}"))
}

/// Parses one program into the IR plus the JSON the editor draws. The JSON is
/// the crate's own serialization of `Circuit`, so it is byte-for-byte the shape
/// the browser tier produces for the same source.
pub fn parse(qasm3: &str, inputs_json: &str) -> std::result::Result<Parsed, CircuitDiagnostic> {
    let inputs = input_values(inputs_json).map_err(|e| CircuitDiagnostic {
        kind: "input".to_string(),
        message: e.to_string(),
        line: None,
        column: None,
    })?;
    let circuit = parse_qasm3(qasm3, &inputs).map_err(|e| diagnostic(&e))?;
    let ir_json = serde_json::to_string(&circuit).map_err(|e| CircuitDiagnostic {
        kind: "invalid".to_string(),
        message: format!("circuit IR could not be serialized: {e}"),
        line: None,
        column: None,
    })?;
    Ok(Parsed { circuit, ir_json })
}

/// Simulator options for one run, with the laboratory's qubit ceiling folded
/// in: the crate refuses a register over `max_qubits` before it allocates one,
/// so the limit holds even if a caller reaches the simulator another way.
pub fn sim_options(options: &SimulateOptions, max_qubits: u32) -> SimOptions {
    SimOptions {
        precision: if options.precision == "single" {
            Precision::Single
        } else {
            Precision::Double
        },
        // Never ABOVE the simulator's own ceiling. A laboratory may lower the
        // limit, and raising it past what the crate is willing to allocate
        // would turn the refusal plan §4.2 asks for into the out-of-memory
        // kill it exists to prevent.
        max_qubits: (max_qubits.min(MAX_CORE_QUBITS)) as usize,
        seed: options.seed,
    }
}

/// The highest `max_qubits_core` this build accepts: the simulator's own
/// ceiling, which is where the allocation actually happens.
pub const MAX_CORE_QUBITS: u32 = DEFAULT_MAX_QUBITS as u32;

/// Whether this run records its evolution (plan §13.6).
///
/// The wire field is three-valued. An explicit choice is obeyed at any size —
/// that is what "record evolution" is for. "Not decided" follows the plan's two
/// rules at once: a keyframe is one pass over the state per gate, so it is the
/// default up to [`KEYFRAME_DEFAULT_QUBITS`] and an opt-in above it; and a
/// keyframe needs amplitudes, so a Clifford circuit that `auto` would answer on
/// the tableau — no state vector allocated at all (§6.1) — is not silently
/// moved onto the state-vector engine to produce an animation nobody asked for.
pub fn records_evolution(options: &SimulateOptions, num_qubits: u32, is_clifford: bool) -> bool {
    if let Some(choice) = options.record_evolution {
        return choice;
    }
    let wants_amplitudes = options.want_state || options.want_probabilities;
    let on_tableau = options.method != "statevector" && is_clifford && !wants_amplitudes;
    !on_tableau && num_qubits <= KEYFRAME_DEFAULT_QUBITS
}

/// Ceiling on the recorded evolution of one run, in bytes. The series is held
/// in memory while the run executes and stored as ONE CBOR artifact, so this
/// is both an allocation bound and the size of the object that lands in the
/// content store.
pub const MAX_KEYFRAME_BUNDLE_BYTES: u64 = 64 * 1024 * 1024;

/// Upper bound on the CBOR of one keyframe with these budgets.
///
/// Deliberately an over-estimate of the encoding: every float is counted as a
/// full 9-byte CBOR double and every collection pays its header. The number is
/// used to REFUSE a run before it allocates anything, so erring high refuses a
/// run that would have just fitted, while erring low would let a 26-qubit
/// "full entanglement map" request eat the node.
fn keyframe_bytes(num_qubits: u32, options: &SimulateOptions) -> u64 {
    let n = num_qubits as u64;
    let dimension = 1u64 << num_qubits.min(63);
    let top_k = (options.keyframe_top_k as u64).min(dimension);
    let probs_top = (options.keyframe_probs_top as u64).min(dimension);
    let pairs = match options.keyframe_pairs.as_str() {
        "none" => 0,
        "all" => n.saturating_mul(n.saturating_sub(1)) / 2,
        _ => 1,
    };
    // Bloch vector + purity per qubit; a 4x4 density matrix plus its two
    // scalars per pair; an amplitude with up to three gate partners per entry
    // of `top`; a bitstring and a probability per entry of `probs_top`.
    let bloch = n.saturating_mul(4 * 9);
    let pairs = pairs.saturating_mul(16 * 2 * 9 + 32);
    let top = top_k.saturating_mul(4 * (9 + 18) + 16);
    let probs = probs_top.saturating_mul(n + 12);
    bloch
        .saturating_add(pairs)
        .saturating_add(top)
        .saturating_add(probs)
        + 64
}

/// Refuses a keyframe budget that would not fit before anything is allocated.
///
/// Every number here comes from the wire and is used as an allocation size:
/// `top_k` and `probs_top` size a heap inside the simulator, `pairs = "all"`
/// multiplies the per-frame work by n(n-1)/2 reduced density matrices (plan
/// §13.6 makes the full map an on-demand query above 16 qubits), and the
/// series itself is accumulated in memory before it is stored.
pub fn validate_keyframe_budget(
    num_qubits: u32,
    steps: usize,
    options: &SimulateOptions,
) -> std::result::Result<(), String> {
    if options.keyframe_top_k > MAX_KEYFRAME_TOP_K {
        return Err(format!(
            "keyframe_top_k is {} and this laboratory allows at most \
             {MAX_KEYFRAME_TOP_K} amplitudes per frame",
            options.keyframe_top_k
        ));
    }
    if options.keyframe_probs_top > MAX_KEYFRAME_PROBS_TOP {
        return Err(format!(
            "keyframe_probs_top is {} and this laboratory allows at most \
             {MAX_KEYFRAME_PROBS_TOP} probabilities per frame",
            options.keyframe_probs_top
        ));
    }
    if options.keyframe_pairs == "all" && num_qubits > KEYFRAME_ALL_PAIRS_QUBITS {
        return Err(format!(
            "the full entanglement map costs one reduced density matrix per qubit pair per \
             gate, so it is only recorded up to {KEYFRAME_ALL_PAIRS_QUBITS} qubits \
             ({num_qubits} here); record the evolution with the gate's own qubits and ask for \
             the full map on the finished state"
        ));
    }
    let total = keyframe_bytes(num_qubits, options).saturating_mul(steps as u64);
    if total > MAX_KEYFRAME_BUNDLE_BYTES {
        return Err(format!(
            "recording {steps} steps with this keyframe budget would produce about {total} \
             bytes of evolution and the limit is {MAX_KEYFRAME_BUNDLE_BYTES}; lower \
             keyframe_top_k, keyframe_probs_top or keyframe_pairs, or run without recording \
             the evolution"
        ));
    }
    Ok(())
}

/// Bytes the JSON amplitude artifact of this state occupies, over-estimated.
///
/// The stored artifact is a JSON array of `[re, im]` pairs, so the number that
/// has to respect the storage ceiling (§18 decision 9) is THIS one and not the
/// 16 bytes per amplitude the state vector occupies in memory. A double prints
/// to at most 24 characters, and a pair costs two of them plus its brackets
/// and separators.
pub fn state_json_bytes(num_qubits: u32) -> u64 {
    const BYTES_PER_AMPLITUDE: u64 = 2 * 24 + 4;
    BYTES_PER_AMPLITUDE.saturating_mul(1u64 << num_qubits.min(63))
}

/// Bytes a full amplitude read-back occupies. It is always `Complex64`,
/// whatever the simulator's own precision is, and it lives NEXT TO the
/// simulator's state — so a run that stores its state peaks at both.
pub fn read_back_bytes(num_qubits: u32) -> u64 {
    (std::mem::size_of::<num_complex::Complex64>() as u64)
        .saturating_mul(1u64 << num_qubits.min(63))
}

/// Bytes one state vector of this circuit occupies — the `memory` figure of
/// `runs.metrics_json` and the number plan §4.2 tabulates.
pub fn state_bytes(num_qubits: u32, precision: Precision) -> u64 {
    let per_amplitude: u64 = match precision {
        Precision::Single => 8,
        Precision::Double => 16,
    };
    per_amplitude.saturating_mul(1u64 << num_qubits.min(63))
}

/// The textual forms a circuit can be exported to. All three come out of the
/// same IR, so an exported program is the program that ran.
pub fn export(circuit: &Circuit, ir_json: &str, format: &str) -> Result<(String, String)> {
    match format {
        "qasm3" => Ok((circuit.to_qasm3(), "circuit.qasm".to_string())),
        "qiskit" => Ok((
            tentaflow_quantum::export::qiskit_python(circuit),
            "circuit.py".to_string(),
        )),
        "ir" => Ok((ir_json.to_string(), "circuit.json".to_string())),
        other => Err(anyhow!(
            "unknown export format '{other}' (expected 'qasm3', 'qiskit' or 'ir')"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BELL: &str = "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[2] q;\nbit[2] c;\n\
                        h q[0];\ncx q[0], q[1];\nc = measure q;\n";

    #[test]
    fn a_valid_program_parses_into_ir_the_editor_can_read() {
        let parsed = parse(BELL, "").expect("Bell parses");
        assert_eq!(parsed.circuit.num_qubits(), 2);
        assert_eq!(parsed.circuit.num_clbits(), 2);
        let value: serde_json::Value = serde_json::from_str(&parsed.ir_json).expect("IR is JSON");
        assert!(value.is_object());
    }

    /// A rejection has to point AT something: the editor underlines the line
    /// and the column, so a diagnostic without them is only half an answer.
    #[test]
    fn a_rejected_program_reports_the_line_it_broke_on() {
        let source = "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[2] q;\nnosuchgate q[0];\n";
        let error = parse(source, "").expect_err("unknown gate is refused");
        assert!(error.line.is_some(), "diagnostic without a line: {error:?}");
        assert_eq!(error.line, Some(4));
        assert!(error.column.is_some());
        assert!(!error.message.is_empty());
    }

    #[test]
    fn malformed_input_bindings_are_a_diagnostic_not_a_panic() {
        let error = parse(BELL, "not json").expect_err("bad bindings are refused");
        assert_eq!(error.kind, "input");
    }

    #[test]
    fn export_produces_every_declared_form_and_refuses_the_rest() {
        let parsed = parse(BELL, "").expect("parses");
        let (qasm, name) = export(&parsed.circuit, &parsed.ir_json, "qasm3").expect("qasm3");
        assert!(qasm.contains("OPENQASM 3"));
        assert_eq!(name, "circuit.qasm");
        let (python, name) = export(&parsed.circuit, &parsed.ir_json, "qiskit").expect("qiskit");
        assert!(python.contains("QuantumCircuit"));
        assert_eq!(name, "circuit.py");
        let (ir, _) = export(&parsed.circuit, &parsed.ir_json, "ir").expect("ir");
        assert_eq!(ir, parsed.ir_json);
        assert!(export(&parsed.circuit, &parsed.ir_json, "png").is_err());
    }

    /// The capacity table of plan §4.2: `complex128` is 16 bytes per amplitude,
    /// `complex64` is 8, and 28 qubits is the 2 GiB the default ceiling names.
    #[test]
    fn state_size_follows_the_capacity_table() {
        assert_eq!(state_bytes(28, Precision::Single), 2 * 1024 * 1024 * 1024);
        assert_eq!(state_bytes(20, Precision::Single), 8 * 1024 * 1024);
        assert_eq!(state_bytes(20, Precision::Double), 16 * 1024 * 1024);
    }

    /// The storage ceiling of §18 decision 9 has to be measured on the artifact
    /// that is WRITTEN. At 22 qubits the amplitudes occupy exactly 64 MiB in
    /// memory — inside the limit — while their JSON is several times that, so a
    /// ceiling applied to the in-memory size would have stored it.
    #[test]
    fn the_state_ceiling_is_measured_on_the_stored_json() {
        assert_eq!(state_bytes(22, Precision::Double), 64 * 1024 * 1024);
        assert!(state_json_bytes(22) > 3 * state_bytes(22, Precision::Double));
        assert!(state_json_bytes(18) < 64 * 1024 * 1024);
    }

    /// Every keyframe budget is an allocation size taken from the wire, so an
    /// out-of-range one is refused before a run starts rather than discovered
    /// by the allocator.
    #[test]
    fn keyframe_budgets_are_bounded_before_anything_is_allocated() {
        let sane = SimulateOptions::default();
        assert!(validate_keyframe_budget(4, 12, &sane).is_ok());

        let huge_top_k = SimulateOptions {
            keyframe_top_k: 4_000_000_000,
            ..SimulateOptions::default()
        };
        let refusal = validate_keyframe_budget(2, 2, &huge_top_k).expect_err("refused");
        assert!(refusal.contains("keyframe_top_k"), "{refusal}");

        let huge_probs = SimulateOptions {
            keyframe_probs_top: 100_000,
            ..SimulateOptions::default()
        };
        assert!(validate_keyframe_budget(2, 2, &huge_probs).is_err());

        // The full entanglement map is n(n-1)/2 density matrices per gate: a
        // query on a finished state above 16 qubits, never a recording.
        let all_pairs = SimulateOptions {
            keyframe_pairs: "all".to_string(),
            ..SimulateOptions::default()
        };
        assert!(validate_keyframe_budget(8, 20, &all_pairs).is_ok());
        let refusal = validate_keyframe_budget(28, 20, &all_pairs).expect_err("refused");
        assert!(refusal.contains("entanglement map"), "{refusal}");

        // A budget each of whose parts is legal, in a series long enough to
        // exceed the memory the recording may occupy.
        let long = validate_keyframe_budget(20, 200_000, &sane).expect_err("refused");
        assert!(long.contains("evolution"), "{long}");
    }

    #[test]
    fn options_carry_the_laboratory_ceiling_into_the_simulator() {
        let wire = SimulateOptions {
            precision: "single".to_string(),
            seed: 7,
            ..SimulateOptions::default()
        };
        let options = sim_options(&wire, 26);
        assert_eq!(options.max_qubits, 26);
        assert_eq!(options.seed, 7);
        assert_eq!(options.precision, Precision::Single);

        // A laboratory may lower the ceiling; it may not raise it past what the
        // simulator is willing to allocate.
        assert_eq!(
            sim_options(&wire, 40).max_qubits,
            MAX_CORE_QUBITS as usize,
            "the simulator's own ceiling is the last word"
        );
    }
}

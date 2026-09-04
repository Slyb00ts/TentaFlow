// ===== File: wasm.rs — wasm-bindgen surface for the browser tier (T0) =====
//
// The browser runs the SAME parser, IR and state-vector code as Core (plan 4.1,
// tier T0); this module only converts between JavaScript values and the crate's
// types. Nothing here does numerics, so a keyframe computed in the editor and
// one computed on a node are the same numbers by construction.
//
// Two conversion rules hold everywhere below:
//
//  * anything small (the IR, diagnostics, keyframes, counts) crosses as JSON,
//    with camelCase field names in both directions — an options object, an IR
//    and a keyframe are spelled the same way on either side of the boundary,
//  * anything sized 2^n (amplitudes, probabilities) crosses as a `Float64Array`,
//    amplitudes interleaved `[re0, im0, re1, im1, ...]`. A 24-qubit state is
//    16.7 M amplitudes; as JSON that would be hundreds of megabytes of text.
//
// `wasm32-unknown-unknown` compiles with `panic = "abort"`, so the
// `catch_unwind` guard inside `parse::parse_qasm3` does NOT catch an upstream
// parser panic here — it traps the instance instead. `www/js/quantum/index.js`
// owns the recovery: a trap poisons the module and the next call instantiates a
// fresh one.

use std::collections::BTreeMap;

use js_sys::{Float64Array, Object, Reflect};
use num_complex::Complex64;
use serde::Deserialize;
use wasm_bindgen::prelude::*;

use crate::error::{Error, SourcePos};
use crate::ir::Circuit;
use crate::parse::{parse_qasm3, InputValues};
use crate::sim::statevector::{self, KeyframeOptions, PairSelection, RunResult, SimOptions};
use crate::sim::{stabilizer, Precision};

/// Qubit ceiling the browser applies when the caller names none.
///
/// 24 qubits is the upper practical bound of plan 4.2: 128 MiB of amplitudes in
/// `Single`, 256 MiB in the default `Double`, and wasm32 addresses 4 GiB — but
/// the stepper's preview register and its transfer buffer are two more copies of
/// the state. Above this the answer is a higher tier, not a bigger allocation.
pub const MAX_QUBITS_BROWSER: usize = 24;

// =============================================================================
// Error conversion
// =============================================================================

/// Machine-readable discriminant of a crate error, so the editor can style a
/// syntax squiggle differently from a capacity refusal without matching on
/// English prose.
fn error_kind(error: &Error) -> &'static str {
    match error {
        Error::Syntax { .. } => "syntax",
        Error::Semantic { .. } => "semantic",
        Error::Unsupported { .. } => "unsupported",
        Error::ParserPanic { .. } => "parserPanic",
        Error::UnboundInput { .. } => "unboundInput",
        Error::Invalid(_) => "invalid",
        Error::TooManyQubits { .. } => "tooManyQubits",
        Error::NotClifford { .. } => "notClifford",
    }
}

/// A thrown crate error. `line`/`column` are present only for diagnostics that
/// point into the source, so the editor can tell "this line is wrong" from
/// "this circuit does not fit".
fn throw(error: Error) -> JsValue {
    let js_error = js_sys::Error::new(&error.to_string());
    js_error.set_name("QuantumError");
    let object: &Object = js_error.as_ref();
    set(object, "kind", &JsValue::from_str(error_kind(&error)));
    if let Some(SourcePos { line, column }) = error.position() {
        set(object, "line", &JsValue::from_f64(f64::from(line)));
        set(object, "column", &JsValue::from_f64(f64::from(column)));
    }
    js_error.into()
}

/// A malformed argument from JavaScript — a broken options object, an IR that
/// is not this crate's IR. Distinct from `throw` because it is a caller bug,
/// not a diagnostic about the user's circuit.
fn throw_argument(what: &str, detail: impl std::fmt::Display) -> JsValue {
    let js_error = js_sys::Error::new(&format!("{what}: {detail}"));
    js_error.set_name("QuantumError");
    set(js_error.as_ref(), "kind", &JsValue::from_str("argument"));
    js_error.into()
}

/// `Reflect::set` on an object this function just created cannot fail: it is
/// extensible and the key is a plain string.
fn set(object: &Object, key: &str, value: &JsValue) {
    Reflect::set(object, &JsValue::from_str(key), value)
        .expect("a freshly created object accepts every string property");
}

// =============================================================================
// Options carried across the boundary
// =============================================================================

/// Amplitude precision as JavaScript spells it. The crate's own `Precision`
/// serialises as `"Single"`/`"Double"`; the browser API takes the lowercase
/// names the UI already shows next to a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum PrecisionName {
    Single,
    #[default]
    Double,
}

impl From<PrecisionName> for Precision {
    fn from(name: PrecisionName) -> Precision {
        match name {
            PrecisionName::Single => Precision::Single,
            PrecisionName::Double => Precision::Double,
        }
    }
}

/// Which simulator answers. `Auto` picks the stabilizer tableau for a Clifford
/// circuit that is only being sampled, because the tableau has no amplitudes to
/// give: asking for the state or the probabilities forces the state vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum MethodName {
    #[default]
    Auto,
    Statevector,
    Stabilizer,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
struct SimulateOptions {
    /// `0` means "do not sample"; the run then only produces state or
    /// probabilities.
    shots: u64,
    /// Seed of the shot stream. JavaScript numbers are exact below 2^53, which
    /// every seed the UI mints stays under.
    seed: u64,
    precision: PrecisionName,
    max_qubits: usize,
    method: MethodName,
    /// Ask for the final state vector as interleaved `[re, im]` pairs.
    state: bool,
    /// Ask for the probability of every basis state.
    probs: bool,
}

impl Default for SimulateOptions {
    fn default() -> SimulateOptions {
        SimulateOptions {
            shots: 0,
            seed: 0,
            precision: PrecisionName::default(),
            max_qubits: MAX_QUBITS_BROWSER,
            method: MethodName::default(),
            state: false,
            probs: false,
        }
    }
}

impl SimulateOptions {
    fn sim_options(&self) -> SimOptions {
        SimOptions {
            precision: self.precision.into(),
            max_qubits: self.max_qubits,
            seed: self.seed,
        }
    }
}

/// Options of a held simulator: the same knobs minus everything about shots,
/// which the stepper takes per call.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
struct SimulatorOptions {
    seed: u64,
    precision: PrecisionName,
    max_qubits: usize,
}

impl Default for SimulatorOptions {
    fn default() -> SimulatorOptions {
        SimulatorOptions {
            seed: 0,
            precision: PrecisionName::default(),
            max_qubits: MAX_QUBITS_BROWSER,
        }
    }
}

/// Which qubit pairs a keyframe carries. Either one of the named policies or an
/// explicit list, so `{pairs: "gate"}` and `{pairs: [[0, 1]]}` both work.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum PairsSpec {
    Named(String),
    Explicit(Vec<(usize, usize)>),
}

impl Default for PairsSpec {
    fn default() -> PairsSpec {
        PairsSpec::Named("gate".to_string())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
struct KeyframeRequest {
    pairs: PairsSpec,
    top_k: usize,
    probs_top: usize,
}

impl Default for KeyframeRequest {
    fn default() -> KeyframeRequest {
        let defaults = KeyframeOptions::default();
        KeyframeRequest {
            pairs: PairsSpec::default(),
            top_k: defaults.top_k,
            probs_top: defaults.probs_top,
        }
    }
}

impl KeyframeRequest {
    fn into_options(self) -> Result<KeyframeOptions, JsValue> {
        let pairs = match &self.pairs {
            PairsSpec::Named(name) => match name.as_str() {
                "none" => PairSelection::None,
                "gate" => PairSelection::GateQubits,
                "all" => PairSelection::All,
                other => {
                    return Err(throw_argument(
                        "keyframe options",
                        format!("`pairs` is \"none\", \"gate\", \"all\" or a list, not {other:?}"),
                    ))
                }
            },
            PairsSpec::Explicit(list) => PairSelection::Explicit(list.clone()),
        };
        Ok(KeyframeOptions {
            pairs,
            top_k: self.top_k,
            probs_top: self.probs_top,
        })
    }
}

/// Parse an optional JSON options argument, falling back to the defaults when
/// JavaScript passed nothing.
fn options_from_json<T: Default + for<'de> Deserialize<'de>>(
    json: Option<String>,
    what: &str,
) -> Result<T, JsValue> {
    match json {
        None => Ok(T::default()),
        Some(text) => serde_json::from_str(&text).map_err(|e| throw_argument(what, e)),
    }
}

fn circuit_from_json(ir: &str) -> Result<Circuit, JsValue> {
    serde_json::from_str(ir).map_err(|e| throw_argument("circuit IR", e))
}

// =============================================================================
// Value conversion
// =============================================================================

/// `Complex64` is `#[repr(C)]` over two `f64`, so a slice of them already has
/// the interleaved `[re, im, re, im, ...]` layout the browser wants. Asserting
/// it here keeps `amplitudes_to_js` free of a per-element transposition.
const _: () = assert!(size_of::<Complex64>() == 2 * size_of::<f64>());
const _: () = assert!(align_of::<Complex64>() == align_of::<f64>());

/// Copy a run of `f64` into a JavaScript-owned array with one memcpy.
///
/// The copy itself is unavoidable — growing the wasm heap detaches every view
/// into it, so a view handed to JavaScript would go stale the moment the next
/// circuit runs — but it is ONE copy, not one JavaScript call per element. At 24
/// qubits a state vector is 33.5 M numbers; the element-at-a-time form spent
/// more time crossing the boundary than the simulator spent producing the state.
fn f64s_to_js(values: &[f64]) -> Float64Array {
    // SAFETY: `view` borrows wasm linear memory and stays valid only until the
    // heap grows or moves. Nothing between it and `slice` allocates, and `slice`
    // copies into a JavaScript buffer, so the borrow never outlives `values`.
    unsafe { Float64Array::view(values) }.slice(0, values.len() as u32)
}

/// Amplitudes as interleaved `[re, im]` pairs.
fn amplitudes_to_js(amps: &[Complex64]) -> Float64Array {
    // SAFETY: the assertions above pin `Complex64` to exactly two contiguous
    // `f64` with `f64` alignment, so the reinterpretation covers the same bytes
    // with the same validity — every bit pattern is a valid `f64`.
    let flat: &[f64] =
        unsafe { std::slice::from_raw_parts(amps.as_ptr().cast::<f64>(), amps.len() * 2) };
    f64s_to_js(flat)
}

fn counts_to_js(counts: &BTreeMap<String, u64>) -> Object {
    let object = Object::new();
    for (key, value) in counts {
        set(&object, key, &JsValue::from_f64(*value as f64));
    }
    object
}

fn precision_name(precision: Precision) -> &'static str {
    match precision {
        Precision::Single => "single",
        Precision::Double => "double",
    }
}

// =============================================================================
// Free functions
// =============================================================================

/// Parse OpenQASM 3 into the circuit IR.
///
/// Returns a JSON envelope rather than throwing, because the editor calls this
/// on every keystroke and a rejected program is the normal case, not an
/// exception: `{"status":"parsed", "circuit":…, "numQubits":…, "numClbits":…,
/// "isClifford":…}` or `{"status":"rejected", "errors":[{"kind":…, "message":…,
/// "line":…, "column":…}]}`. Anything this function throws is a real fault.
///
/// The front end stops at the first diagnostic, so `errors` holds exactly one
/// entry; it is a list because the editor renders a list either way.
///
/// `inputs` binds `input float` parameters, as a JSON object of name → number.
#[wasm_bindgen(js_name = parse)]
pub fn parse(source: &str, inputs: Option<String>) -> Result<String, JsValue> {
    let inputs: InputValues = options_from_json(inputs, "input values")?;
    let outcome = match parse_qasm3(source, &inputs) {
        Ok(circuit) => serde_json::json!({
            "status": "parsed",
            "circuit": circuit,
            "numQubits": circuit.num_qubits(),
            "numClbits": circuit.num_clbits(),
            "isClifford": circuit.is_clifford(),
        }),
        Err(error) => {
            let position = error.position();
            serde_json::json!({
                "status": "rejected",
                "errors": [{
                    "kind": error_kind(&error),
                    "message": error.to_string(),
                    "line": position.map(|p| p.line),
                    "column": position.map(|p| p.column),
                }],
            })
        }
    };
    serde_json::to_string(&outcome).map_err(|e| throw_argument("parse result", e))
}

/// Canonical OpenQASM 3 of an IR circuit — the round-trip partner of `parse`,
/// and how the visual editor turns a dragged circuit back into the artefact
/// that Core stores (plan 0, "artefakt kanoniczny").
#[wasm_bindgen(js_name = toQasm3)]
pub fn to_qasm3(ir: &str) -> Result<String, JsValue> {
    Ok(circuit_from_json(ir)?.to_qasm3())
}

/// Whether the stabilizer tableau can run this circuit. The editor offers the
/// Clifford mode on the strength of this (plan 4.2).
#[wasm_bindgen(js_name = isClifford)]
pub fn is_clifford(ir: &str) -> Result<bool, JsValue> {
    Ok(circuit_from_json(ir)?.is_clifford())
}

/// Run a circuit once and report everything the caller asked for.
///
/// The result object carries `method`, `isClifford`, `numQubits`, `numClbits`,
/// `shots`, `counts` (or `null`), `state` (or `null`), `probs` (or `null`) and
/// `stateReason` — set when `state`/`probs` were asked for and the circuit
/// cannot have a single state vector, naming which construct blocked it.
///
/// Counts and state are two separate passes over the circuit, because a circuit
/// that has both a measurement and a final state does not exist: the state is
/// defined only for a unitary circuit. Asking for both on the same circuit is
/// therefore rare and pays for it honestly.
#[wasm_bindgen(js_name = simulate)]
pub fn simulate(ir: &str, options: Option<String>) -> Result<Object, JsValue> {
    let circuit = circuit_from_json(ir)?;
    let options: SimulateOptions = options_from_json(options, "simulate options")?;
    let sim_options = options.sim_options();
    let clifford = circuit.is_clifford();

    let wants_amplitudes = options.state || options.probs;
    let stabilizer = match options.method {
        MethodName::Statevector => false,
        MethodName::Stabilizer => {
            if !clifford {
                return Err(throw(Error::NotClifford {
                    reason: "the circuit uses a gate outside the Clifford group".to_string(),
                }));
            }
            true
        }
        MethodName::Auto => clifford && !wants_amplitudes,
    };

    let result = Object::new();
    set(
        &result,
        "method",
        &JsValue::from_str(if stabilizer {
            "stabilizer"
        } else {
            "statevector"
        }),
    );
    set(&result, "isClifford", &JsValue::from_bool(clifford));
    set(
        &result,
        "numQubits",
        &JsValue::from_f64(circuit.num_qubits() as f64),
    );
    set(
        &result,
        "numClbits",
        &JsValue::from_f64(circuit.num_clbits() as f64),
    );

    if options.shots > 0 {
        let run: RunResult = if stabilizer {
            stabilizer::run(&circuit, &sim_options, options.shots).map_err(throw)?
        } else {
            statevector::run(&circuit, &sim_options, options.shots).map_err(throw)?
        };
        set(&result, "shots", &JsValue::from_f64(run.shots as f64));
        set(&result, "counts", counts_to_js(&run.counts).as_ref());
    } else {
        set(&result, "shots", &JsValue::from_f64(0.0));
        set(&result, "counts", &JsValue::NULL);
    }

    if wants_amplitudes {
        if stabilizer {
            return Err(throw_argument(
                "simulate options",
                "the stabilizer tableau has no amplitudes; ask for `method: \"statevector\"`",
            ));
        }
        // A measurement, a reset or a classical guard leaves the circuit without
        // one state vector. That is a property of the circuit the editor should
        // explain, not a failed call — the stepper below is how such a circuit
        // is watched. Every other failure (a register that does not fit) is a
        // refusal the caller has to handle.
        if let Err(reason) = statevector::require_unitary(&circuit) {
            set(&result, "state", &JsValue::NULL);
            set(&result, "probs", &JsValue::NULL);
            set(
                &result,
                "stateReason",
                &JsValue::from_str(&reason.to_string()),
            );
        } else {
            let amps = statevector::statevector(&circuit, &sim_options).map_err(throw)?;
            set(&result, "stateReason", &JsValue::NULL);
            if options.state {
                set(&result, "state", amplitudes_to_js(&amps).as_ref());
            } else {
                set(&result, "state", &JsValue::NULL);
            }
            if options.probs {
                let probs: Vec<f64> = amps.iter().map(|a| a.norm_sqr()).collect();
                set(&result, "probs", f64s_to_js(&probs).as_ref());
            } else {
                set(&result, "probs", &JsValue::NULL);
            }
        }
    } else {
        set(&result, "state", &JsValue::NULL);
        set(&result, "probs", &JsValue::NULL);
        set(&result, "stateReason", &JsValue::NULL);
    }

    Ok(result)
}

// =============================================================================
// Held simulator — the circuit editor's stepper
// =============================================================================

/// A circuit loaded into a live register: the object the Studio holds while the
/// user drags the time slider (plan 13.6).
///
/// JavaScript owns this handle and must `free()` it; the state vector it holds
/// is up to 256 MiB and the garbage collector does not know that.
#[wasm_bindgen(js_name = Simulator)]
pub struct WasmSimulator {
    inner: statevector::Simulator,
    /// Default seed of `counts`, so a histogram is reproducible without the
    /// caller repeating the seed on every refresh.
    seed: u64,
    clifford: bool,
    /// Bloch vectors of the state currently in the register.
    ///
    /// The pass that produces them reads the whole state vector once and comes
    /// out with all `n` vectors (`analysis::bloch_vectors`), so it costs
    /// `O(n * 2^n)` whether one qubit is asked for or all of them — 660 ms at 24
    /// qubits. The per-qubit sphere row of plan 13.6 asks `n` times per frame,
    /// which without this cache would be `n` full passes. Dropped by every
    /// method that changes the register.
    bloch: Option<Vec<[f64; 3]>>,
}

#[wasm_bindgen(js_class = Simulator)]
impl WasmSimulator {
    /// Load an IR circuit. `options` is `{seed, precision, maxQubits}`.
    #[wasm_bindgen(constructor)]
    pub fn new(ir: &str, options: Option<String>) -> Result<WasmSimulator, JsValue> {
        let circuit = circuit_from_json(ir)?;
        let options: SimulatorOptions = options_from_json(options, "simulator options")?;
        let sim_options = SimOptions {
            precision: options.precision.into(),
            max_qubits: options.max_qubits,
            seed: options.seed,
        };
        Ok(WasmSimulator {
            clifford: circuit.is_clifford(),
            inner: statevector::Simulator::new(&circuit, &sim_options).map_err(throw)?,
            seed: options.seed,
            bloch: None,
        })
    }

    #[wasm_bindgen(getter, js_name = numQubits)]
    pub fn num_qubits(&self) -> usize {
        self.inner.num_qubits()
    }

    #[wasm_bindgen(getter, js_name = stepCount)]
    pub fn step_count(&self) -> usize {
        self.inner.step_count()
    }

    /// How many steps have been applied.
    #[wasm_bindgen(getter)]
    pub fn position(&self) -> usize {
        self.inner.position()
    }

    #[wasm_bindgen(getter)]
    pub fn precision(&self) -> String {
        precision_name(self.inner.precision()).to_string()
    }

    #[wasm_bindgen(getter, js_name = backendName)]
    pub fn backend_name(&self) -> String {
        self.inner.backend_name().to_string()
    }

    #[wasm_bindgen(getter, js_name = isClifford)]
    pub fn is_clifford(&self) -> bool {
        self.clifford
    }

    /// Back to |0...0> with a cleared classical register.
    pub fn rewind(&mut self) {
        self.bloch = None;
        self.inner.rewind();
    }

    /// Apply the next operation; `false` once the program is exhausted.
    pub fn step(&mut self) -> bool {
        self.bloch = None;
        self.inner.step()
    }

    #[wasm_bindgen(js_name = runToEnd)]
    pub fn run_to_end(&mut self) {
        self.bloch = None;
        self.inner.run_to_end();
    }

    /// The state after the fraction `t` of the PENDING operation, without
    /// consuming it — the frames the time slider draws between two gates.
    #[wasm_bindgen(js_name = stepFraction)]
    pub fn step_fraction(&mut self, t: f64) -> Result<Float64Array, JsValue> {
        let amps = self.inner.step_fraction(t).map_err(throw)?;
        Ok(amplitudes_to_js(&amps))
    }

    /// Full state as interleaved `[re, im]` pairs.
    pub fn amplitudes(&self) -> Float64Array {
        amplitudes_to_js(&self.inner.amplitudes())
    }

    /// Probability of every basis state.
    pub fn probabilities(&self) -> Float64Array {
        f64s_to_js(&self.inner.probabilities())
    }

    /// Classical register image after the steps applied so far, one byte per
    /// bit, index 0 = `c[0]`.
    pub fn clbits(&self) -> Vec<u8> {
        self.inner
            .clbits()
            .iter()
            .map(|bit| u8::from(*bit))
            .collect()
    }

    /// Bloch vectors of EVERY qubit, flattened `[x0, y0, z0, x1, y1, z1, ...]`.
    ///
    /// This is the affordable shape and the one the per-qubit sphere row of plan
    /// 13.6 should call: the underlying pass reads the state vector once and
    /// yields all `n` vectors, so asking for the row costs exactly what asking
    /// for a single qubit costs. The result is cached until the register moves.
    #[wasm_bindgen(js_name = blochVectors)]
    pub fn bloch_vectors(&mut self) -> Result<Float64Array, JsValue> {
        let vectors = self.bloch_cached()?;
        // SAFETY: `[f64; 3]` is three contiguous `f64` with `f64` alignment, so a
        // slice of them covers exactly `3 * len` valid `f64`.
        let flat: &[f64] = unsafe {
            std::slice::from_raw_parts(vectors.as_ptr().cast::<f64>(), vectors.len() * 3)
        };
        Ok(f64s_to_js(flat))
    }

    /// Bloch vector `[x, y, z]` of one qubit. A qubit entangled with the rest
    /// has a vector shorter than 1, which is what the sphere draws.
    ///
    /// Served from the same cached pass as `blochVectors`, so a loop over the
    /// qubits costs one pass in total rather than one per qubit.
    pub fn bloch(&mut self, qubit: usize) -> Result<Float64Array, JsValue> {
        let vector = *self
            .bloch_cached()?
            .get(qubit)
            .ok_or_else(|| throw(Error::Invalid(format!("qubit {qubit} is out of range"))))?;
        Ok(f64s_to_js(&vector))
    }

    /// Reduced density matrix of one or two qubits, row-major, as interleaved
    /// `[re, im]` pairs.
    #[wasm_bindgen(js_name = reducedDensityMatrix)]
    pub fn reduced_density_matrix(&self, qubits: Vec<usize>) -> Result<Float64Array, JsValue> {
        let rho = self.inner.reduced_density_matrix(&qubits).map_err(throw)?;
        Ok(amplitudes_to_js(&rho))
    }

    /// The Bloch pass for the register as it stands, computed at most once per
    /// applied step.
    fn bloch_cached(&mut self) -> Result<&[[f64; 3]], JsValue> {
        if self.bloch.is_none() {
            self.bloch = Some(self.inner.bloch_vectors().map_err(throw)?);
        }
        Ok(self.bloch.as_ref().expect("filled just above"))
    }

    /// Everything the run view draws at the current step, as JSON: the gate that
    /// was applied, Bloch vectors, per-qubit purity, the requested pair density
    /// matrices with their mutual information and concurrence, the largest
    /// amplitudes with the partners the gate mixed them with, and the top
    /// probabilities. `options` is `{pairs, topK, probsTop}` where `pairs` is
    /// `"none"`, `"gate"`, `"all"` or a list of `[i, j]`.
    ///
    /// Complex numbers serialise as `[re, im]` pairs, and the field names are
    /// camelCase (`probsTop`, `mutualInformation`) like everything else that
    /// crosses this boundary.
    pub fn keyframe(&mut self, options: Option<String>) -> Result<String, JsValue> {
        let request: KeyframeRequest = options_from_json(options, "keyframe options")?;
        let keyframe = self
            .inner
            .keyframe(&request.into_options()?)
            .map_err(throw)?;
        // A keyframe already ran the Bloch pass; keep it so a following sphere
        // query does not run the same O(n * 2^n) walk a second time.
        self.bloch = Some(keyframe.bloch.clone());
        serde_json::to_string(&keyframe).map_err(|e| throw_argument("keyframe", e))
    }

    /// Sample the CURRENT state `shots` times and return the histogram as a
    /// JSON object of bitstring → count. The state is not collapsed, so the
    /// shot slider never changes what the next `step` does; keys name qubits
    /// (bit 0 rightmost). `seed` defaults to the simulator's seed and crosses as
    /// a JavaScript number, which is exact below 2^53.
    pub fn counts(&self, shots: u32, seed: Option<f64>) -> Result<String, JsValue> {
        let run = self
            .inner
            .sample_counts(u64::from(shots), seed.map_or(self.seed, |s| s as u64))
            .map_err(throw)?;
        serde_json::to_string(&run.counts).map_err(|e| throw_argument("counts", e))
    }
}

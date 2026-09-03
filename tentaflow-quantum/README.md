# tentaflow-quantum

OpenQASM 3 front end, circuit IR and quantum simulators for **TentaQuant**
(`docs/TENTAQUANT_PLAN.md`, §6.1–§6.2, §13.6).

The crate is pure computation: no async runtime, no I/O, no network. It parses
OpenQASM 3 text into a typed circuit, simulates it on the CPU, and produces the
state analytics the run view animates. It builds unchanged for `wasm32`
(single-threaded) and for native targets (rayon), so a keyframe computed in the
browser (T0) and one computed on a node (T1) are the same numbers.

```bash
cd tentaflow-quantum
cargo test
cargo clippy --all-targets
cargo check --target wasm32-unknown-unknown
```

## Modules

| Module | What it owns |
|---|---|
| `error` | `Error`, `Result`, `SourcePos` — every diagnostic that can carry a source position does |
| `gate` | the gate set, its matrices, adjoints, controlled forms, integer and fractional powers, Clifford test |
| `ir` | `Circuit`, `Operation`, `Condition` and canonical OpenQASM 3 emission |
| `parse` | OpenQASM 3 → `Circuit` through `oq3_syntax` / `oq3_semantics` 0.7.0 |
| `sim` | the `Backend` trait, the CPU state-vector backend, the stabilizer tableau, state analytics |
| `linalg` | dense complex algebra for 2×2 and 4×4 matrices: Hermitian eigensolver, `U^t`, entropy, concurrence |
| `grade` | state / unitary equality up to a global phase, TVD and Hellinger fidelity between count histograms |
| `export` | Qiskit-Python rendering (canonical OpenQASM 3 is `Circuit::to_qasm3`) |

## Parsing

```rust
use tentaflow_quantum::parse::{parse_qasm3, InputValues};

let mut inputs = InputValues::new();
inputs.insert("theta".to_string(), 0.5);
let circuit = parse_qasm3(source, &inputs)?;
```

The supported subset is:

* `qubit[n]` / `bit[n]` declarations (a scalar `qubit q;` is a register of one),
* every `stdgates.inc` gate plus the builtin `U` and `gphase`,
* user `gate` definitions, inlined at the call site,
* gate modifiers `inv @`, `pow(k) @` (integer `k`), `ctrl @` and `negctrl @`
  with a single control on a one-qubit gate,
* register broadcast (`h q;` applies `h` to every qubit of `q`),
* `measure` into a bit or a whole bit register, `reset`, `barrier`,
* `if` on a bit or on a bit register, with `else`,
* `for` over a constant range or set, unrolled,
* `input float` parameters, bound by the caller,
* constant classical declarations (`const int n = 3;`) usable in angles and
  loop bounds.

Everything else — `defcal`, `duration`, `delay`, `extern`, `while`, `box`,
`switch`, `def`, aliases, arrays, hardware qubits, `output` — is a validation
error carrying the line and column it appears on. `include` resolves only
`"stdgates.inc"`; no other file is ever read.

Five of those (`box`, `cal`, `defcal`, `defcalgrammar`, `extern`) are found by a
lexical scan that runs before the parser, because `oq3_syntax` cannot build a
tree for them and fails them with a message that names nothing ("Expecting
semicolon terminating statement"). All five are OpenQASM 3 keywords, so a
program inside the subset can never carry one as an identifier; the scan skips
comments, string literals and annotation lines.

Diagnostics raised while lowering (a wrong number of parameters, a qubit index
out of range, the same qubit twice on a two-qubit gate) carry the position of
the top-level statement they came from: the semantic graph has no source ranges
of its own, so that is the finest granularity available. `barrier` needs at
least one qubit — a bare `barrier;` is not readable by the front end, so the IR
refuses to hold one and the canonical text always names its qubits.

`ccx` and `cswap` are lowered to their standard `stdgates.inc` decomposition, so
the IR and the simulators only ever see one- and two-qubit gates.

An unbound `input float` is `Error::UnboundInput`, not a default value.

## The IR

`Circuit` holds qubit and classical registers plus a flat list of `Operation`s.
Each operation carries a conjunction of `Condition`s (empty means
unconditional); an `else` branch becomes the negated condition, and nested `if`s
become several conditions on the same operation.

Bit order is little-endian throughout: `c[0]` is the least significant bit of a
`Condition::Register` value and the rightmost character of a count key, which is
what Qiskit does. Count keys are rendered straight from the bit image, so a
register wider than a machine word (the stabilizer path runs thousands of
qubits) keeps an exact key on a 32-bit target as well as on a 64-bit one.

Two shapes the IR refuses to hold, because it would have to answer them wrongly:

* a guard on a register of more than 64 bits — `Condition::Register` compares a
  `u64`, and a wider register could only be compared on a truncated image;
* a guarded measurement into a bit its own condition reads. OpenQASM 3 evaluates
  an `if` condition once, at block entry, while a flat operation list re-reads it
  before every operation of the block; the two agree exactly as long as a block
  leaves its own guard bits alone, so `if (c[0] == 1) { c[0] = measure q[0]; x
  q[1]; }` is a validation error (positioned at the `if`) instead of a block that
  silently stops halfway. Measuring into any other bit inside the block, and
  measuring into a guard bit before the `if`, are both fine.

`Circuit::to_qasm3()` is deterministic and round-trips: parsing its output
yields an equal `Circuit`, and emitting that again is byte-identical. Angles are
written in the shortest form that reads back as the same `f64`. The editor's
layout JSON is deliberately NOT part of the IR.

## Simulation

```rust
use tentaflow_quantum::sim::statevector::{run, statevector, SimOptions, Simulator};

let counts = run(&circuit, &SimOptions::default(), 4096)?.counts;
let amplitudes = statevector(&circuit, &SimOptions::default())?;   // no measurements
let mut stepper = Simulator::new(&circuit, &SimOptions::default())?;
```

`SimOptions` carries the precision (`Single` / `Double`), the qubit ceiling
(refused before allocation, never as an OOM) and the seed. The same seed and the
same circuit always give the same counts.

`run` picks its path: a circuit whose final state does not depend on any
measurement outcome is simulated once and sampled from its distribution;
anything with a reset, a classical guard or work after a measurement is replayed
per shot.

`Simulator` is the stepper behind the circuit editor. It executes one IR
operation per `step()`, and at every stop it can report:

| Call | Result |
|---|---|
| `amplitudes()` / `probabilities()` | the raw state |
| `bloch_vectors()` | Bloch vector of every qubit, in one pass over the state |
| `reduced_density_matrix(&[i])` / `(&[i, j])` | 2×2 or 4×4 reduced density matrix |
| `mutual_information(i, j)` / `concurrence(i, j)` | the entanglement map's edge weights |
| `pauli_expectation(&[(q, Pauli::Z), …])` | expectation of a Pauli product |
| `step_fraction(t)` | the state after the fraction `t` of the PENDING operation |
| `keyframe(&options)` | everything above packed as one `StateKeyframe` |

`step_fraction(1.0)` reproduces `step()` exactly. A one-angle rotation scales
its angle; every other gate goes through `U^t` from the eigen-decomposition of
its matrix, so the animation follows one fixed continuous path from the identity
to the gate. A measurement or a reset has no fractional form and says so. It
takes `&mut self` because the preview register is built once and reused: the
time slider redraws a frame at a time and must not allocate a state per frame.

The top-K amplitudes and the top basis-state probabilities are selected with a
bounded heap: one pass over the state and memory proportional to K, never a sort
of 2ⁿ entries, because plan §13.6 budgets a keyframe at a single pass per gate.

`Keyframe` carries the step index, the gate that was applied with its matrix,
Bloch vectors and purity per qubit, reduced density matrices of the selected
pairs (with mutual information and concurrence), the top-K amplitudes with the
partner amplitudes the last gate mixed them with, and the top basis-state
probabilities. It serialises with serde, which is how it travels as a
`RunEvent`.

### Backends

`sim::Backend` is the device boundary of plan §6.3: allocation, a batch of
gates, a global phase, single-qubit measurement probability and collapse,
probabilities, `sample` and amplitude read-back. Sampling is a backend primitive
so a device backend can answer a batch of sorted uniform draws from a prefix
reduction on the device instead of shipping 2^n probabilities to the host.
`sim::cpu::CpuBackend<S>` is the first implementation, generic over `f32` /
`f64`; `cuda` and `wgpu` plug in here without touching the IR, the scheduler or
the analytics. There is no GPU code in this crate yet.

The CPU kernels address amplitudes by bit indexing, with adjacent single-qubit
gates fused into one matrix before they reach the backend. On native targets the
outer blocks run under rayon (and the inner loop too, once a block is large
enough); under `cfg(target_arch = "wasm32")` the same code runs single-threaded.

### Stabilizer

`sim::stabilizer` is an Aaronson–Gottesman tableau: `O(n²)` bits instead of
`2ⁿ` amplitudes, so a Clifford circuit scales to thousands of qubits.
`Circuit::is_clifford()` decides whether it applies — conservatively, so a
`false` only means this crate will not try. Measurement, reset and classical
control behave exactly as in the state-vector simulator, and the test suite
compares the two on random Clifford circuits.

## Grading

`grade` compares a solution with a reference:

* `states_equal` / `unitaries_equal` — equality up to a global phase, with the
  phase fixed on the largest component,
* `state_fidelity` — `|⟨a|b⟩|²`, for "almost right" feedback,
* `total_variation_distance` and `hellinger_fidelity` — between count
  histograms, for simulation ↔ QPU comparison.

## Export

`Circuit::to_qasm3()` is the canonical OpenQASM 3 form.
`export::qiskit_python(&circuit)` renders a Qiskit program, including `if_test`
control flow (a negated register guard becomes the `else` block). QIR and vendor
dialects are produced by the Python service, not here.

## Tests

`cargo test` covers:

* analytic golden states — Bell, GHZ, QFT on 3 qubits against the discrete
  Fourier matrix, teleportation with classical control (deterministic and
  statistical),
* an independent dense-matrix reference simulator (`tests/common`) that builds
  each gate's full 2ⁿ×2ⁿ matrix, cross-checked against the bit-indexed kernels on
  random circuits up to 4 qubits,
* stabilizer ↔ state vector agreement on random Clifford circuits,
* OpenQASM 3 round trips, including awkward float values, the diagnostics for
  every construct outside the subset and the statement position a lowering
  diagnostic carries,
* the two refused IR shapes (a block rewriting its own guard bit, a guard on a
  register wider than 64 bits) from both the front end and a hand-built circuit,
  next to a guarded block that must run to its end,
* count keys of a register wider than a machine word (a 70-qubit GHZ and a
  single excitation on bit 64 through the tableau),
* the bounded top-K selection against a full sort of the state,
* the modifier edge cases (`negctrl @` of an identity and of a phase-only gate,
  `pow(k) @` folded into a rotation angle),
* `step_fraction(1.0) == step()`, keyframe Bloch vectors against the reduced
  density matrices, gate algebra (unitarity, adjoints, powers, fusion),
* grading, the Qiskit exporter and serde round trips of the IR and a keyframe.

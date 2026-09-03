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
cargo check --target wasm32-unknown-unknown --features wasm   # browser bindings
cargo test  --target wasm32-unknown-unknown --features wasm   # parity + the JS surface
./scripts/wasm-bench.sh                                       # build the glue and measure it
```

The wasm target needs `rustup target add wasm32-unknown-unknown`, and the last
two lines need `cargo install wasm-bindgen-cli --version 0.2.125 --locked`
(which also installs the `wasm-bindgen-test-runner` that `.cargo/config.toml`
points the wasm test runner at).

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
| `wasm` | the browser bindings, behind the `wasm` feature — conversion only, no numerics |

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
* grading, the Qiskit exporter and serde round trips of the IR and a keyframe,
* native ↔ wasm parity of the shot stream — the same test file runs on both
  targets against `tests/golden/wasm_parity.json` (see below).

## The browser build (feature `wasm`)

Tier T0 of plan §4.1: the dashboard runs this crate itself, so the circuit
Studio answers a keystroke without a round trip and the run view animates a
state nobody had to stream. `src/wasm.rs` is the whole browser surface and it
does conversion only — the parser, the IR and the simulator are the same code
the native build runs, which is what makes a keyframe from T0 and a keyframe
from T1 the same numbers rather than two implementations that agree for now.

The feature is off by default; `tentaflow-core/build.rs` turns it on. It adds
`wasm-bindgen`, `js-sys` and `serde_json` and nothing else. `[lib] crate-type`
carries `cdylib` (what wasm-bindgen links) next to `rlib` (what Core, the tests
and the benches link), because Cargo cannot make `crate-type` depend on the
target.

### Building it

`tentaflow-core/build.rs::build_quantum_wasm_bindings` runs

```
cargo build --target wasm32-unknown-unknown --release --features wasm
wasm-bindgen --target web --out-dir www/js/quantum --out-name quantum_glue --no-typescript
```

with the same skip-if-missing contract as `wasm_glue` and `voxel_glue`: no
`wasm32-unknown-unknown` target, no `wasm-bindgen` CLI in `PATH` or a failed
cargo build is a `cargo:warning` and no artefact, never a build failure. The
Studio then falls back to T1 through the binary protocol (plan §6.1). Only a
`wasm-bindgen` that runs and fails is fatal, because that is the case where the
`.js` and the `.wasm` can disagree.

`wasm-bindgen-cli` must be exactly the version pinned in `Cargo.toml`
(0.2.125, same as `tentaflow-protocol-wasm`):

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.125 --locked
```

Output is `tentaflow-core/www/js/quantum/quantum_glue.js` (~24 KiB) and
`quantum_glue_bg.wasm` (~890 KiB, release, LTO). Both are generated, both are
gitignored like the protocol glue, and both are embedded into the binary by
`generate_wwwroot_embed`.

To build the glue without touching `www/`, use `./scripts/wasm-bench.sh
[out-dir]` — it produces the identical artefact in a scratch directory.

### The API

`tentaflow-core/www/js/quantum/index.js` is the loader: it lazy-imports the
glue on first use and exposes a typed async facade. The glue is served from the
dashboard's own origin (and the service worker's precache); nothing is fetched
from a CDN.

```js
import { available, parse, simulate, createSimulator } from '/js/quantum/index.js';

if (!(await available())) { /* run this circuit on T1 instead */ }

const parsed = await parse(source);           // {status:'parsed'|'rejected', ...}
if (parsed.status === 'rejected') {
  const [{ message, line, column, kind }] = parsed.errors;
}

const run = await simulate(parsed.circuit, { shots: 4096, seed: 7 });
const sim = await createSimulator(parsed.circuit, { seed: 7 });
sim.step();
sim.stepFraction(0.5);                        // Float64Array, interleaved [re, im]
sim.keyframe({ pairs: 'gate' });
sim.blochVectors();                           // [x0,y0,z0, x1,y1,z1, ...]
sim.counts(4096);
sim.free();                                   // the register is not GC-visible
```

Two conversion rules hold across the whole surface: anything sized 2ⁿ
(amplitudes, probabilities, density matrices) crosses as a `Float64Array`,
amplitudes interleaved `[re0, im0, re1, im1, …]`; everything else crosses as
JSON, with complex numbers as `[re, im]` pairs. A 24-qubit state as JSON text
would be hundreds of megabytes.

`parse` returns a rejection instead of throwing, because a half-typed program is
the normal case in an editor. Everything that does throw carries `name:
'QuantumError'` and a `kind` (`syntax`, `semantic`, `unsupported`,
`tooManyQubits`, `notClifford`, `argument`, `aborted`), plus `line`/`column`
when the diagnostic points into the source.

Every per-qubit quantity (Bloch vectors, purity) comes out of ONE pass over the
whole state vector, so the API hands out the whole set: `blochVectors()` returns
all `n` vectors flattened, and that is what the sphere row of plan §13.6 should
call. `bloch(q)` exists for a single sphere and is served from the same pass,
cached until `step`, `rewind`, `runToEnd` or a fresh `keyframe` moves the
register — a loop over the qubits therefore costs one pass, not `n` (measured
below: 38 ms rather than 733 ms at 20 qubits).

`simulate` reports `stateReason` when a requested state vector does not exist:
a measured or classically guarded circuit has no single final state, and the
held simulator is the way to watch one. The default qubit ceiling in the browser
is 24 (`MAX_QUBITS_BROWSER`), refused before allocation rather than as an OOM.

### `panic = "abort"` and the recovery path

`wasm32-unknown-unknown` compiles with `panic = "abort"`, so the `catch_unwind`
guard that keeps an upstream `oq3_syntax` panic from taking down a native
process does NOT catch it in the browser: the panic traps the instance. So does
an allocation the wasm heap cannot serve. A trapped instance can never be used
again, which is why `index.js` wraps every call: a `WebAssembly.RuntimeError`
drops the module, bumps a generation counter so the next dynamic import yields a
fresh namespace, and throws a `QuantumError` with `kind: 'aborted'`. One
pathological circuit costs a reload of the module, not a reload of the
dashboard.

## Spike A — do the `oq3_*` crates build for `wasm32-unknown-unknown`?

**Yes, unmodified.** `oq3_parser`, `oq3_syntax`, `oq3_source_file` and
`oq3_semantics` 0.7.0 all compile for `wasm32-unknown-unknown` with no patch,
no fork and no feature surgery, and the resulting parser runs in the browser:
`scripts/wasm-bench.mjs` parses every fixture circuit through the wasm build.

Plan §2.4 kept a fallback for the other outcome — Core parses and ships the IR,
the browser only simulates. That fallback is not needed: `parse()` is part of
the browser API. The IR is still the wire form between Core and the browser
(`simulate` and `Simulator` both take IR JSON, not text), so a circuit built in
the visual editor never round-trips through OpenQASM 3 text just to run.

The one caveat is the `panic = "abort"` behaviour above: the constructs the
upstream parser panics on are rejected *lexically* before it ever sees them
(`box`, `cal`, `defcal`, `defcalgrammar`, `extern`), and the rest of the subset
check runs on the syntax tree, so the panic path needs input that gets past
both. It is a module reload when it happens, not a lost dashboard.

## Spike B — 24 qubits in the browser

Measured with `./scripts/wasm-bench.sh` on the release glue (LTO, no wasm SIMD)
under Node 22 (V8, the engine Chrome runs), on an aarch64 Linux host
(Cortex-X925/A725, 20 cores — only one of which the wasm build uses). Numbers on
an x86-64 laptop differ; the shape does not.

Plan §16 Faza 0 asks one question: **is a Hadamard on 24 qubits under 100 ms?**

| Qubits | Amplitudes | One Hadamard (`double`) | One Hadamard (`single`) | Allocate the register |
|---|---|---|---|---|
| 20 | 1 048 576 | **1.0 ms** | 1.3 ms | 0.2 ms |
| 22 | 4 194 304 | **4.5 ms** | 5.0 ms | 1.0 ms |
| 24 | 16 777 216 | **17.9 ms** | 20.2 ms | 4.6 ms |

**Yes — 18 ms, a factor of five under the bar.** The gate is linear in 2ⁿ, as it
must be, and the register allocation is not the cost.

A whole circuit and the analytics on top of it are the interesting numbers (all
figures from the same run, so they add up):

| Qubits | GHZ (n gates), state kept in wasm | GHZ, state handed to JavaScript | One keyframe (`pairs: 'gate'`) | Bloch vectors of every qubit | 4096-shot live histogram |
|---|---|---|---|---|---|
| 20 | 35 ms | 43 ms | **48 ms** | 38 ms | 1.0 ms |
| 22 | 161 ms | 191 ms | **191 ms** | 160 ms | 2.8 ms |
| 24 | 702 ms | 804 ms | **804 ms** | 672 ms | 10.0 ms |

Memory (`double`, i.e. `complex128`): the register alone grows the wasm heap to
337 MiB at 24 qubits, and asking `simulate` for the state takes the peak to
594 MiB — because `Backend::amplitudes` copies the register into a fresh vector
before it is converted, so the state exists twice inside wasm for the duration
of the hand-over. The `Float64Array` the caller receives is a third copy, on the
JavaScript heap. A `single` register is half the size, but the state still
crosses the boundary as `f64`.

**Is 24 qubits practical? For a run, yes; for feedback on every keystroke, no —
and that is exactly what plan §4.2 and §13.6 already say.** Concretely:

* editing with a fresh keyframe on every change stays comfortable to
  **20 qubits**: 35 ms to re-run the circuit plus 48 ms for the keyframe is
  under a tenth of a second, which reads as immediate. This is plan §4.2's
  `T0 ≤ 20 q` default, confirmed rather than assumed;
* **24 qubits works as a one-shot run** — press play, wait about a second — but
  a keyframe per gate at 0.8 s each makes "record the evolution" a 24-gate,
  20-second job. Plan §13.6 already makes keyframes an option above 24 q on T1;
  the same reasoning applies in the browser above 20 q;
* above 24 the answer is a higher tier, not a bigger allocation:
  `MAX_QUBITS_BROWSER = 24` refuses before allocating.

Four things worth recording because they were not obvious. The first three are
measurements that changed the code; the numbers in the tables above are the ones
after those changes.

* **The `O(n · 2ⁿ)` Bloch pass, not the gates, is what a state view costs.** It
  is 400 M inner iterations at 24 qubits — 672 ms, as much as the whole GHZ
  circuit that produced the state and about 37× a single gate — and it yields
  the Bloch vector of EVERY qubit in one go. Two consequences, both of which
  needed a fix:
  * a keyframe reported the vectors and the purities and computed the pass twice
    to do it, costing 1.48 s at 24 qubits — twice the circuit. It now runs the
    pass once and derives the purities from the vectors already in hand
    (`analysis::purity_from_bloch`): the same arithmetic on the same inputs, one
    pass less, 1480 → 804 ms at 24 qubits and 88 → 48 ms at 20;
  * a first cut of the wasm API exposed only `bloch(q)`, which ran the whole
    pass and discarded `n − 1` of its results. Plan §13.6's per-qubit sphere row
    is `n` such calls, so drawing it cost `n` full passes: 733 ms at 20 qubits
    and 15.9 s at 24, i.e. 17× and 23× one keyframe that already returns every
    vector. The API now exposes `blochVectors()` and caches the pass until the
    register moves, so the row costs exactly one pass — 37.6 ms at 20 q and
    671.9 ms at 24, within noise of the single `blochVectors()` call in the
    table above.
* **`single` precision is consistently 10–25 % *slower* than `double`, not
  faster.** Halving the memory traffic buys nothing here, so the wasm build is
  compute-bound rather than memory-bound: it has no SIMD (`simd128` is off), and
  every `f32` gate matrix is converted from the `Complex64` the IR carries. The
  lever for T0 speed is therefore `-C target-feature=+simd128`, not lower
  precision. `single` is still the right choice for memory, and stays the only
  option the future `wgpu` backend can offer (plan §6.3).
* **Handing the state to JavaScript is a memcpy and has to be written as one.**
  The copy itself is unavoidable — growing the wasm heap detaches every view
  into it, so a view lent to JavaScript would go stale on the next allocation —
  but a first cut wrote the 33.5 M numbers of a 24-qubit state with one
  `Float64Array` index assignment each and spent 570 ms there, as much as
  computing the state. `Complex64` is `#[repr(C)]`, so the amplitude slice is
  already the interleaved `[re, im, …]` layout: `Float64Array::view` over it
  plus one `slice()` is a single memcpy. The hand-over is now 102 ms at 24
  qubits (804 − 702) and 8.5 ms at 20 (43 − 35) — 15 % and 20 % of the compute,
  not 45 %. That matters most for `stepFraction`, which returns a whole state
  per frame of the time slider: at 20 qubits its hand-over fits inside a 16 ms
  frame budget where the old form did not.
* **The run view should still ask for a keyframe rather than the state, but for
  a different reason than the timings first suggested.** With the memcpy the
  boundary is no longer the argument; the argument is size. A keyframe is tens
  of kilobytes of exactly what the view draws (plan §13.6 budgets ~70 KB at 32
  qubits, and it is bounded by `topK`/`probsTop`), while the state is 268 MB of
  `Float64Array` on the JavaScript heap at 24 qubits — a third live copy on top
  of the register and the vector `Backend::amplitudes` hands over. Ask for the
  state when something actually reads all of it.

### Native ↔ wasm parity

`tests/golden/wasm_parity.json` holds five circuits — Bell, GHZ-5, a
non-Clifford rotation circuit in both precisions, and one with `reset` and a
classical guard that forces the per-shot path — with the counts a fixed seed
produces. `tests/wasm_parity.rs` asserts them, and it is ONE test compiled for
two targets:

```bash
cargo test --test wasm_parity                                  # native
cargo test --target wasm32-unknown-unknown --features wasm    # inside wasm
```

The wasm run goes through `wasm_bindgen_test` and `wasm-bindgen-test-runner`
(plan §6.2); the fixture is `include_str!`-ed because wasm32 has no filesystem.
On top of that, `scripts/wasm-bench.mjs` checks the same file through the
JavaScript API, so the JSON options, the counts object and the bindings around
the simulator are pinned too. All three pass, and the counts are identical, not
close.

`tests/wasm_api.rs` covers what only a JavaScript engine can answer and compiles
away on every other target: that amplitudes arrive interleaved real-then-
imaginary (the memcpy above reinterprets `&[Complex64]` as `&[f64]`, and only a
run can pin the order), that `blochVectors()` carries every qubit in index
order, and that `step`, `rewind`, `runToEnd` and `keyframe` all leave the Bloch
cache agreeing with the register.

They can be, and the tests assert it rather than hoping: the shot stream is
`StdRng` (ChaCha12) over a `u64` seed, the draws are ordered with
`f64::total_cmp`, and the state vector is IEEE-754 arithmetic in a fixed order
with no fused-multiply-add contraction. None of that varies between `wasm32` and
a native target. This is the "wyniki są bitowo zgodne z T0" criterion of plan
§16, Faza 1, reduced to one artefact every half checks against.

## Open: the global phase `stepFraction` travels through

Surfaced by wiring the stepper up to the browser, not introduced by it, and left
alone because it is a change to simulator semantics that needs a product
decision and its own golden test.

`step_fraction(t)` raises the pending gate to the power `t` through
`linalg::unitary_power`, which fixes the branch of that power by rotating `u` by
the global phase `best_alpha` that keeps `det(I + e^{iα} u)` largest and then
dividing `e^{-i·best_alpha·t}` back out. When `best_alpha > π` the branch it
picks is not the principal one, and the compensating phase winds a full turn
between the endpoints. A bare `cx` is the smallest case: `amp[0]` is `1` at
`t = 0`, `−i` at `0.25`, `−1` at `0.5`, `+i` at `0.75` and `1` again at `t = 1`.

Endpoints are exact, and counts, probabilities, Bloch vectors and every reduced
density matrix are invariant under a global phase, so nothing measurable is
wrong. The one thing that would show it is plan §13.6's phase-coloured amplitude
bars: during a single gate every bar's colour would rotate through the whole
wheel and come back. The fix is small — fold `phi − best_alpha` into `(−π, π]`
before scaling by `t`, so the path taken is the short one — but it changes what
every intermediate frame of the slider looks like, which is a decision about the
animation rather than a bug fix.

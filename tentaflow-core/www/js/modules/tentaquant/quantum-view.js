// ===== File: modules/tentaquant/quantum-view.js — simulator readouts → what the state panel draws =====
//
// Everything the circuit Studio (Q07) and the notebook state panel (Q06) show is
// derived from three things the browser simulator hands back — the amplitude
// vector, the Bloch vectors and a counts map — and every derivation lives here,
// as a pure function, so the animation contract of plan §13.6 can be tested
// without a canvas, a wasm module or a clock.
//
// The step slider runs over GATES (plan §13.1 item 5) while `tf-quantum-circuit`
// counts COLUMNS of its ASAP schedule, so the mapping between the two is a
// function of the grid the component itself builds — never a second schedule of
// our own.

import {
  GATE_INFO, buildGrid, describeCell, formatAngle,
} from '/js/components/tf-quantum-circuit.js';
import { STATE_MIME, COUNTS_MIME } from '/js/components/tf-mime-output.js';

export { STATE_MIME, COUNTS_MIME };

// One gate of the evolution animation lasts this long; the playhead crosses the
// column in that time and the amplitudes morph with it (§13.6).
export const MS_PER_GATE = 700;

// The whole per-frame state readout is O(2^n): the amplitude vector is COPIED
// out of the wasm heap (16 B per basis state), `amplitudeRows` builds one row
// object per non-negligible entry and sorts them, and the JavaScript Bloch pass
// walks the same vector n times. At 12 qubits that is a 64 KB copy and 4096
// rows, which fits in a 16 ms frame; at the tier's own ceiling of 24 it is a
// 268 MB copy and ~16.7M objects, which does not fit anywhere. Above this width
// nothing pulls the vector out of wasm at all: the spheres come from the
// simulator's own pass and the amplitude card says why it is empty.
export const MAX_LIVE_STATE_QUBITS = 12;

// The refusal ceiling the browser tier runs with (plan §21, spike B: 24 qubits
// in wasm). Every `simulate` / `createSimulator` call of both screens passes it
// explicitly instead of leaning on the facade default, so the width the target
// select promises and the width the simulator accepts are one constant.
export const T0_MAX_QUBITS = 24;

// ---------------------------------------------------------------------------
// Grid ⇄ step
// ---------------------------------------------------------------------------

export function gridOf(circuit) {
  return buildGrid(circuit || {});
}

/// How many schedule columns are fully applied once the first `opCount`
/// operations have run. ASAP scheduling can place a later operation in an
/// earlier column, so this is a maximum over the applied prefix, not a lookup.
export function appliedColumns(grid, opCount) {
  let columns = 0;
  for (const cell of grid.cells) {
    if (cell.index < opCount) columns = Math.max(columns, cell.column + 1);
  }
  return columns;
}

export function cellOfOp(grid, opIndex) {
  return grid.cells.find((cell) => cell.index === opIndex) || null;
}

/// Fractional column the playhead sits at while operation `opIndex` is `t`
/// through. Null once the program is exhausted: there is nothing left to point
/// at, and the head has to disappear rather than stick to the last gate.
export function playheadAt(grid, opIndex, t) {
  const cell = cellOfOp(grid, opIndex);
  if (!cell) return null;
  return cell.column + Math.max(0, Math.min(1, Number(t) || 0));
}

/// The first operation of the schedule column a user clicked, so a click on the
/// circuit and a drag of the slider mean the same thing.
export function opAtColumn(grid, column) {
  const cells = grid.cells.filter((cell) => cell.column === column);
  if (!cells.length) return null;
  return Math.min(...cells.map((cell) => cell.index));
}

/// What the gate-properties card of Q07 shows for the current selection: the
/// operation itself when exactly one is selected, otherwise just how many are.
/// Pure, so the card's contents are tested without a canvas.
export function gateDetails(grid, indices, labels) {
  const cells = (Array.isArray(indices) ? indices : [])
    .map((index) => cellOfOp(grid, Number(index)))
    .filter(Boolean);
  if (cells.length !== 1) {
    return { count: cells.length, index: null, label: '', column: 0, qubits: '', params: [] };
  }
  const cell = cells[0];
  const info = GATE_INFO[cell.id] || null;
  return {
    count: 1,
    index: cell.index,
    label: describeCell(cell, labels),
    column: cell.column + 1,
    qubits: cell.qubits.map((qubit) => `q${qubit}`).join(', '),
    params: (info ? info.params : []).map((name, i) => ({ name, value: formatAngle(cell.params[i]) })),
  };
}

export function stepSummary(grid, step, total, labels) {
  const cell = cellOfOp(grid, step - 1);
  const applied = cell ? describeCell(cell, labels) : '';
  return { step, total, applied };
}

// ---------------------------------------------------------------------------
// Amplitudes
// ---------------------------------------------------------------------------

/// Bloch vectors of every qubit from an interleaved `[re, im, ...]` state,
/// flattened `[x0, y0, z0, x1, ...]` — the same shape and the same summation
/// order as `sim::analysis::bloch_vectors`, because the animation interpolates
/// between frames the wasm module produced and frames this function produced.
export function blochFromAmplitudes(amplitudes, numQubits) {
  const n = Math.max(0, Number(numQubits) || 0);
  const dim = 1 << n;
  const out = new Float64Array(n * 3);
  if (!amplitudes || amplitudes.length < dim * 2) return out;
  const cohRe = new Float64Array(n);
  const cohIm = new Float64Array(n);
  const z = new Float64Array(n);
  for (let index = 0; index < dim; index += 1) {
    const re = amplitudes[index * 2];
    const im = amplitudes[index * 2 + 1];
    const weight = re * re + im * im;
    for (let q = 0; q < n; q += 1) {
      if (((index >> q) & 1) === 0) {
        z[q] += weight;
        const partner = index | (1 << q);
        const pre = amplitudes[partner * 2];
        const pim = amplitudes[partner * 2 + 1];
        // a * conj(b)
        cohRe[q] += re * pre + im * pim;
        cohIm[q] += im * pre - re * pim;
      } else {
        z[q] -= weight;
      }
    }
  }
  for (let q = 0; q < n; q += 1) {
    out[q * 3] = 2 * cohRe[q];
    out[q * 3 + 1] = -2 * cohIm[q];
    out[q * 3 + 2] = z[q];
  }
  return out;
}

/// The measurement frame of §13.6: a measurement has no fractional form, so the
/// collapse is drawn by fading the branch that was not measured out and letting
/// the measured one grow back to norm. `t = 0` is the state before the
/// measurement, `t = 1` the state after it, and everything between is the
/// normalised interpolation of the two.
export function collapseFrame(before, after, t) {
  const fraction = Math.max(0, Math.min(1, Number(t) || 0));
  const length = Math.min(before.length, after.length);
  const out = new Float64Array(length);
  let norm = 0;
  for (let i = 0; i < length; i += 1) {
    const value = before[i] * (1 - fraction) + after[i] * fraction;
    out[i] = value;
    norm += value * value;
  }
  if (norm > 0) {
    const scale = 1 / Math.sqrt(norm);
    for (let i = 0; i < length; i += 1) out[i] *= scale;
  }
  return out;
}

/// A gate boundary the animation has to stop interpolating at: a measurement or
/// a reset, which `stepFraction` refuses (and rightly: there is no fractional
/// projection).
export function isCollapsing(cell) {
  return Boolean(cell) && (cell.type === 'measure' || cell.type === 'reset');
}

// ---------------------------------------------------------------------------
// The animation clock
// ---------------------------------------------------------------------------

/// One frame of the playhead. `apply` is how many operations the caller must
/// step the simulator through before drawing; `done` says the program ended.
/// `t` is a CLOCK — the elapsed fraction of the pending gate — and always runs
/// on real time, so a gate takes `msPerGate` whatever the motion preference is.
/// What gets DRAWN from it is `renderFraction`'s decision.
export function advance(frame, deltaMs, options = {}) {
  const stepCount = Math.max(0, Number(options.stepCount) || 0);
  const msPerGate = Math.max(1, Number(options.msPerGate) || MS_PER_GATE);
  const index = Math.max(0, Number(frame && frame.index) || 0);
  const t = Math.max(0, Number(frame && frame.t) || 0);
  if (index >= stepCount) return { index: stepCount, t: 0, apply: 0, done: true };
  const delta = Math.max(0, Number(deltaMs) || 0) / msPerGate;
  let position = index;
  let fraction = t + delta;
  let apply = 0;
  while (fraction >= 1 && position < stepCount) {
    apply += 1;
    position += 1;
    fraction -= 1;
  }
  if (position >= stepCount) return { index: stepCount, t: 0, apply, done: true };
  return { index: position, t: Math.min(fraction, 0.999), apply, done: false };
}

/// The fraction the playhead and the amplitudes should be DRAWN at. Under
/// `prefers-reduced-motion` it is pinned to the gate boundary: the evolution
/// still advances at one gate per `msPerGate`, it just stops gliding between
/// them (plan §13.4) instead of racing through the circuit.
export function renderFraction(frame, reducedMotion) {
  if (reducedMotion) return 0;
  return Math.max(0, Number(frame && frame.t) || 0);
}

// ---------------------------------------------------------------------------
// Mime bundles
// ---------------------------------------------------------------------------

export function stateBundle({ amplitudes, numQubits, bloch, purity }) {
  const value = { numQubits: Number(numQubits) || 0 };
  if (amplitudes) value.amplitudes = amplitudes;
  if (bloch) value.bloch = bloch;
  if (purity) value.purity = purity;
  return { [STATE_MIME]: value };
}

export function countsBundle(counts, shots) {
  return { [COUNTS_MIME]: { counts: counts || {}, shots: Number(shots) || 0 } };
}

// Shots arrive in batches so the histogram fills as the run proceeds (§13.6)
// instead of appearing whole at the end.
export const SHOT_BATCH = 256;

// ...but every batch is a real, independent run: `simulate` evolves the
// register from |0...0> once per call and draws its shots from the final
// distribution (`run_sampled`), so the evolution is a FIXED cost per batch, not
// per shot. 100 000 shots in 256-shot batches would therefore evolve a 24-qubit
// state 391 times to draw one run's worth of samples. Bounding the batch COUNT
// keeps both properties: a wide run still fills in visible steps, and it pays
// for at most this many evolutions.
export const MAX_RUN_BATCHES = 8;

/// Shots per batch of a run of `wanted` shots: `SHOT_BATCH` for as long as that
/// stays inside `MAX_RUN_BATCHES`, an evenly larger batch above it.
export function shotBatchSize(wanted, batches = MAX_RUN_BATCHES) {
  const total = Math.max(0, Math.floor(Number(wanted) || 0));
  const cap = Math.max(1, Math.floor(Number(batches) || 1));
  return Math.max(SHOT_BATCH, Math.ceil(total / cap));
}

/// A fresh seed for one run.
///
/// The measurement stream is a pure function of (seed, shots) on BOTH tiers —
/// the browser's `simulate` and the node's sampler both draw off a seeded RNG,
/// and `SimulateOptions::default().seed` is 0 — so a run that sent no seed
/// would hand back the previous run's histogram, bit for bit. "Run again"
/// means "sample again", so every run mints one and the run stores it
/// (`RunMetrics.seed`), which is what makes it reproducible on purpose rather
/// than by accident.
export function runSeed() {
  return Math.floor(Math.random() * 2 ** 32);
}

/// The batches one run of `shots` is drawn in.
///
/// Every batch carries its OWN seed. The simulator's shot stream is a pure
/// function of (seed, shots) — `sorted_draws(seed, shots)` off a seeded RNG —
/// so a repeated seed replays the identical draw: merging four such batches
/// would multiply ONE 256-shot sample by four and label it 1024, with the shot
/// noise of 256. `base` is minted per run, and the seeds stay far below 2^53,
/// where a JavaScript number still crosses the wasm boundary exactly.
export function shotPlan(wanted, batchSize, base) {
  const total = Math.max(0, Math.floor(Number(wanted) || 0));
  const size = Math.max(1, Math.floor(Number(batchSize) || 1));
  const seed0 = Math.max(0, Math.floor(Number(base) || 0));
  const batches = [];
  for (let drawn = 0; drawn < total; drawn += size) {
    batches.push({ shots: Math.min(size, total - drawn), seed: seed0 + batches.length });
  }
  return batches;
}

/// Accumulates sampled shots so the histogram can fill as the draws arrive
/// (§13.6) instead of appearing whole at the end.
export function mergeCounts(into, more) {
  const out = { ...(into || {}) };
  for (const [key, value] of Object.entries(more || {})) {
    out[key] = (Number(out[key]) || 0) + (Number(value) || 0);
  }
  return out;
}

/// Whether a run of this circuit may draw shots.
///
/// Sampling writes into the classical register, so a circuit that declares none
/// cannot be sampled at all: the engine refuses such a run with an English
/// message, and both screens ask this BEFORE the call rather than translating a
/// refusal afterwards. Such a circuit still has a state — that is what the
/// panel and the cell's state output show instead.
export function canSample(circuit) {
  return Number(circuit && circuit.numClbits) > 0;
}

export function totalShots(counts) {
  return Object.values(counts || {}).reduce((sum, v) => sum + (Number(v) || 0), 0);
}

// ---------------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------------

/// Bytes the full state vector occupies: one complex per basis state, 8 B in
/// single precision (complex64) and 16 B in double.
export function stateMemoryBytes(numQubits, precision) {
  const n = Math.max(0, Number(numQubits) || 0);
  return 2 ** n * (precision === 'double' ? 16 : 8);
}

export function resourceSummary(circuit, precision) {
  const grid = gridOf(circuit);
  const ops = Array.isArray(circuit && circuit.ops) ? circuit.ops.length : 0;
  const gates = grid.cells.filter((cell) => cell.type === 'gate').length;
  return {
    numQubits: grid.numQubits,
    numClbits: grid.numClbits,
    ops,
    gates,
    depth: grid.columns,
    memoryBytes: stateMemoryBytes(grid.numQubits, precision),
  };
}

// ---------------------------------------------------------------------------
// Export helpers
// ---------------------------------------------------------------------------

export function slugify(name) {
  const out = String(name || '')
    .normalize('NFKD')
    .replace(/[\u0300-\u036f]/g, '')
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '');
  return out || 'circuit';
}

/// Both the project file and the download are `.qasm`: the plan names the
/// canonical circuit artefact `circuit.qasm` (§12.1, §13.6), so a file downloaded
/// from the Studio re-uploads under the extension the Studio itself writes.
/// Q07's mockup labels the button `.oq3`, which `fileKindOf` maps to the same
/// kind — one extension is what keeps the two paths one artefact.
export function qasmFileName(name) {
  return `${slugify(name)}.qasm`;
}

export function svgFileName(name) {
  return `${slugify(name)}.svg`;
}

/// The Qiskit export of §6.1. A `.py` next to the `.qasm`: the two are the same
/// circuit in the two languages the plan names, not two artefacts.
export function pyFileName(name) {
  return `${slugify(name)}.py`;
}

// =============================================================================
// File: modules/tentaquant/quantum-view.test.js
// Description: The mapping between what the browser simulator returns and what
// the Studio (Q07) and the notebook state panel (Q06) draw — the gate ⇄ column
// mapping of the step slider, the frames of the evolution animation (plan
// §13.6), the mime bundles and the export names. Pure functions, no wasm.
// =============================================================================

import './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const {
  MS_PER_GATE, advance, appliedColumns, blochFromAmplitudes, cellOfOp, collapseFrame,
  canSample, countsBundle, gateDetails, gridOf, isCollapsing, mergeCounts, opAtColumn,
  playheadAt, pyFileName, qasmFileName, renderFraction, resourceSummary, shotBatchSize,
  shotPlan, slugify,
  stateBundle, stateMemoryBytes, stepSummary, svgFileName, totalShots, COUNTS_MIME,
  MAX_RUN_BATCHES, SHOT_BATCH, STATE_MIME,
} = await import('./quantum-view.js');

const op = (kind) => ({ kind, conditions: [] });
const bell = {
  numQubits: 2,
  numClbits: 2,
  qubitRegisters: [{ name: 'q', start: 0, size: 2 }],
  clbitRegisters: [{ name: 'c', start: 0, size: 2 }],
  ops: [
    op({ Gate: { gate: 'H', qubits: [0] } }),
    op({ Gate: { gate: 'Cx', qubits: [0, 1] } }),
    op({ Measure: { qubit: 0, clbit: 0 } }),
    op({ Measure: { qubit: 1, clbit: 1 } }),
  ],
};

const SQRT_HALF = Math.SQRT1_2;

// ---------------------------------------------------------------------------
// Gates ⇄ columns
// ---------------------------------------------------------------------------

test('the applied-column count grows with the gate prefix the slider names', () => {
  const grid = gridOf(bell);
  assert.equal(appliedColumns(grid, 0), 0);
  assert.equal(appliedColumns(grid, 1), 1);
  assert.equal(appliedColumns(grid, 2), 2);
  assert.equal(appliedColumns(grid, 4), grid.columns);
});

test('an ASAP schedule that puts a later gate in an earlier column stays monotone', () => {
  // The X on q1 is written after the H on q0 but is free to run beside it.
  const circuit = {
    numQubits: 2,
    numClbits: 0,
    qubitRegisters: [{ name: 'q', start: 0, size: 2 }],
    clbitRegisters: [],
    ops: [
      op({ Gate: { gate: 'H', qubits: [0] } }),
      op({ Gate: { gate: 'X', qubits: [1] } }),
      op({ Gate: { gate: 'Cx', qubits: [0, 1] } }),
    ],
  };
  const grid = gridOf(circuit);
  assert.equal(cellOfOp(grid, 1).column, 0, 'the X shares the first column');
  assert.equal(appliedColumns(grid, 1), 1);
  assert.equal(appliedColumns(grid, 2), 1, 'both gates of column 0 are applied');
  assert.equal(appliedColumns(grid, 3), 2);
});

test('the playhead sits on the column of the pending gate and disappears at the end', () => {
  const grid = gridOf(bell);
  assert.equal(playheadAt(grid, 0, 0), 0);
  assert.equal(playheadAt(grid, 1, 0.5), 1.5);
  assert.equal(playheadAt(grid, 4, 0), null);
  // A fraction outside [0, 1] cannot push the head into the next column.
  assert.equal(playheadAt(grid, 1, 4), 2);
});

test('clicking a column selects the first gate scheduled in it', () => {
  const grid = gridOf(bell);
  assert.equal(opAtColumn(grid, 0), 0);
  assert.equal(opAtColumn(grid, 1), 1);
  assert.equal(opAtColumn(grid, 99), null);
});

test('the step summary names the gate that was just applied', () => {
  const grid = gridOf(bell);
  const labels = { measure: 'pomiar', control: 'kontrola', target: 'cel', conditional: 'warunkowa' };
  assert.deepEqual(stepSummary(grid, 0, 4, labels), { step: 0, total: 4, applied: '' });
  assert.match(stepSummary(grid, 1, 4, labels).applied, /^H q0/);
  assert.match(stepSummary(grid, 3, 4, labels).applied, /pomiar q0/);
});

test('the gate card describes one selected operation and only counts the rest', () => {
  const labels = { measure: 'pomiar', control: 'kontrola', target: 'cel', conditional: 'warunkowa' };
  const grid = gridOf(bell);
  const empty = gateDetails(grid, [], labels);
  assert.equal(empty.count, 0);
  assert.equal(empty.index, null);

  const cx = gateDetails(grid, [1], labels);
  assert.equal(cx.count, 1);
  assert.equal(cx.index, 1);
  assert.equal(cx.column, 2, 'columns are counted from one for a reader');
  assert.equal(cx.qubits, 'q0, q1');
  assert.deepEqual(cx.params, [], 'a cx carries no angle');
  assert.match(cx.label, /kontrola/);

  // A parametric gate names its angle the way the popover writes it.
  const rotated = gridOf({
    numQubits: 1,
    numClbits: 0,
    qubitRegisters: [{ name: 'q', start: 0, size: 1 }],
    clbitRegisters: [],
    ops: [op({ Gate: { gate: { Rz: Math.PI / 4 }, qubits: [0] } })],
  });
  assert.deepEqual(gateDetails(rotated, [0], labels).params, [{ name: 'θ', value: 'π/4' }]);

  // Two selected gates are a count, not a description: they have no one angle.
  const many = gateDetails(grid, [0, 1], labels);
  assert.equal(many.count, 2);
  assert.equal(many.index, null);
  // An index that no longer exists (a deleted gate) is not a selection.
  assert.equal(gateDetails(grid, [99], labels).count, 0);
});

// ---------------------------------------------------------------------------
// Amplitudes
// ---------------------------------------------------------------------------

test('the Bloch pass reproduces the simulator convention for |+>', () => {
  const bloch = blochFromAmplitudes(Float64Array.from([SQRT_HALF, 0, SQRT_HALF, 0]), 1);
  assert.ok(Math.abs(bloch[0] - 1) < 1e-12, 'x = 1');
  assert.ok(Math.abs(bloch[1]) < 1e-12, 'y = 0');
  assert.ok(Math.abs(bloch[2]) < 1e-12, 'z = 0');
});

test('|0> points up, |1> points down and |i> lies on +y', () => {
  const near = (vector, expected) => vector.forEach((v, i) => assert.ok(Math.abs(v - expected[i]) < 1e-12, `${v} ~ ${expected[i]}`));
  near(blochFromAmplitudes(Float64Array.from([1, 0, 0, 0]), 1), [0, 0, 1]);
  near(blochFromAmplitudes(Float64Array.from([0, 0, 1, 0]), 1), [0, 0, -1]);
  const plusI = blochFromAmplitudes(Float64Array.from([SQRT_HALF, 0, 0, SQRT_HALF]), 1);
  assert.ok(Math.abs(plusI[1] - 1) < 1e-12, 'y = 1');
});

test('an entangled qubit gets a short vector, qubit by qubit', () => {
  // (|00> + |11>)/sqrt(2): both reduced states are maximally mixed.
  const bell2 = Float64Array.from([SQRT_HALF, 0, 0, 0, 0, 0, SQRT_HALF, 0]);
  const bloch = blochFromAmplitudes(bell2, 2);
  for (const value of bloch) assert.ok(Math.abs(value) < 1e-12);
});

test('a state shorter than the register answers zeros instead of reading past it', () => {
  assert.deepEqual(Array.from(blochFromAmplitudes(Float64Array.from([1, 0]), 2)), [0, 0, 0, 0, 0, 0]);
  assert.deepEqual(Array.from(blochFromAmplitudes(null, 1)), [0, 0, 0]);
});

test('the collapse frame interpolates from the superposition to the measured branch', () => {
  const before = Float64Array.from([SQRT_HALF, 0, SQRT_HALF, 0]);
  const after = Float64Array.from([1, 0, 0, 0]);
  assert.ok(Math.abs(collapseFrame(before, after, 0)[0] - SQRT_HALF) < 1e-12);
  assert.deepEqual(Array.from(collapseFrame(before, after, 1)), [1, 0, 0, 0]);
  const mid = collapseFrame(before, after, 0.5);
  // Every frame is a state: the branch that is fading still leaves a unit norm.
  const norm = mid.reduce((sum, v) => sum + v * v, 0);
  assert.ok(Math.abs(norm - 1) < 1e-12);
  assert.ok(mid[0] > SQRT_HALF && mid[2] < SQRT_HALF);
});

test('a measurement and a reset are the boundaries the animation may not interpolate', () => {
  const grid = gridOf(bell);
  assert.equal(isCollapsing(cellOfOp(grid, 0)), false);
  assert.equal(isCollapsing(cellOfOp(grid, 2)), true);
  assert.equal(isCollapsing(null), false);
});

// ---------------------------------------------------------------------------
// The animation clock
// ---------------------------------------------------------------------------

test('a frame moves the playhead without applying a gate until the column ends', () => {
  const frame = advance({ index: 0, t: 0 }, MS_PER_GATE / 2, { stepCount: 4 });
  assert.equal(frame.index, 0);
  assert.ok(Math.abs(frame.t - 0.5) < 1e-9);
  assert.equal(frame.apply, 0);
  assert.equal(frame.done, false);
});

test('crossing a gate boundary asks the caller to step the simulator exactly once', () => {
  const frame = advance({ index: 0, t: 0.9 }, MS_PER_GATE * 0.2, { stepCount: 4 });
  assert.equal(frame.apply, 1);
  assert.equal(frame.index, 1);
  assert.ok(frame.t < 0.2);
});

test('a long frame after a stall applies every gate it skipped over', () => {
  const frame = advance({ index: 0, t: 0 }, MS_PER_GATE * 2.5, { stepCount: 4 });
  assert.equal(frame.apply, 2);
  assert.equal(frame.index, 2);
});

test('the animation stops at the end of the program instead of wrapping', () => {
  const frame = advance({ index: 3, t: 0.5 }, MS_PER_GATE * 10, { stepCount: 4 });
  assert.equal(frame.done, true);
  assert.equal(frame.index, 4);
  assert.equal(frame.apply, 1);
  assert.deepEqual(advance({ index: 4, t: 0 }, 16, { stepCount: 4 }), { index: 4, t: 0, apply: 0, done: true });
});

test('reduced motion pins the drawn fraction to the gate boundary', () => {
  assert.equal(renderFraction({ index: 1, t: 0.42 }, true), 0);
  assert.ok(Math.abs(renderFraction({ index: 1, t: 0.42 }, false) - 0.42) < 1e-9);
  assert.equal(renderFraction(null, false), 0);
});

test('reduced motion keeps the pace: a gate still costs a full msPerGate of frames', () => {
  // A 60 fps frame is a fraction of a gate whatever the motion preference is —
  // the accessibility setting must calm the evolution, never speed it up.
  let frame = { index: 0, t: 0 };
  let applied = 0;
  for (let i = 0; i < 40; i += 1) {
    const next = advance(frame, 1000 / 60, { stepCount: 4 });
    applied += next.apply;
    frame = { index: next.index, t: next.t };
    assert.equal(renderFraction(frame, true), 0);
  }
  // 40 frames = ~667 ms, still inside the first gate.
  assert.equal(applied, 0);
  assert.equal(frame.index, 0);
});

// ---------------------------------------------------------------------------
// Bundles
// ---------------------------------------------------------------------------

test('a state bundle carries only the parts the panel actually got', () => {
  const bundle = stateBundle({ amplitudes: Float64Array.from([1, 0]), numQubits: 1 });
  assert.deepEqual(Object.keys(bundle), [STATE_MIME]);
  assert.equal(bundle[STATE_MIME].numQubits, 1);
  assert.equal('bloch' in bundle[STATE_MIME], false);
  assert.equal('purity' in bundle[STATE_MIME], false);
});

test('a counts bundle names its shots so the histogram can label itself', () => {
  const bundle = countsBundle({ '00': 512, '11': 512 }, 1024);
  assert.equal(bundle[COUNTS_MIME].shots, 1024);
  assert.equal(bundle[COUNTS_MIME].counts['11'], 512);
  assert.deepEqual(countsBundle(null)[COUNTS_MIME], { counts: {}, shots: 0 });
});

test('every shot batch of one run draws with its own seed', () => {
  // The simulator's shot stream is a pure function of (seed, shots): four
  // batches of 256 sharing one seed are the SAME 256 draws four times over, a
  // histogram with the noise of 256 shots wearing the label of 1024.
  const plan = shotPlan(1024, 256, 7);
  assert.equal(plan.length, 4);
  assert.deepEqual(plan.map((b) => b.shots), [256, 256, 256, 256]);
  assert.equal(new Set(plan.map((b) => b.seed)).size, 4, 'no batch repeats a draw');
  assert.deepEqual(plan.map((b) => b.seed), [7, 8, 9, 10]);

  // The last batch is the remainder, never a rounded-up one: the run samples
  // exactly the number of shots the button promised.
  const odd = shotPlan(700, 256, 0);
  assert.deepEqual(odd.map((b) => b.shots), [256, 256, 188]);
  assert.equal(odd.reduce((sum, b) => sum + b.shots, 0), 700);

  // The seeds of the widest run the UI allows stay exact as JavaScript numbers.
  const widest = shotPlan(100000, 256, 2 ** 32);
  assert.equal(widest.length, 391);
  assert.ok(widest[widest.length - 1].seed < Number.MAX_SAFE_INTEGER);
  assert.deepEqual(shotPlan(0, 256, 1), []);
});

test('a wide run gets wider batches instead of more of them', () => {
  // Every batch is one `simulate` call, and a `simulate` call evolves the
  // register from |0...0> once and then draws its shots from the final
  // distribution: the evolution is paid per BATCH. 100 000 shots cut into
  // 256-shot batches would evolve a 24-qubit state 391 times to draw one run.
  assert.equal(shotBatchSize(1024), SHOT_BATCH, 'a default run keeps the fine-grained fill');
  assert.equal(shotPlan(1024, shotBatchSize(1024), 0).length, 4);

  const wide = shotPlan(100000, shotBatchSize(100000), 5);
  assert.ok(wide.length <= MAX_RUN_BATCHES, `${wide.length} batches is ${wide.length} evolutions`);
  assert.ok(wide.length > 1, 'and the histogram still fills in visible steps');
  // Nothing about the sample itself changes: every shot asked for is drawn, and
  // no batch replays another batch's draws.
  assert.equal(wide.reduce((sum, b) => sum + b.shots, 0), 100000);
  assert.equal(new Set(wide.map((b) => b.seed)).size, wide.length);
});

test('sampled batches accumulate so the histogram fills as the draws arrive', () => {
  const first = mergeCounts({}, { '00': 10, '11': 6 });
  const second = mergeCounts(first, { '11': 4, '01': 1 });
  assert.deepEqual(second, { '00': 10, '11': 10, '01': 1 });
  assert.equal(totalShots(second), 21);
  assert.equal(totalShots(null), 0);
  assert.deepEqual(first, { '00': 10, '11': 6 }, 'merging does not mutate the accumulator');
});

// ---------------------------------------------------------------------------
// Resources and export names
// ---------------------------------------------------------------------------

test('the resource card counts the circuit, not the simulator', () => {
  const summary = resourceSummary(bell, 'single');
  assert.equal(summary.numQubits, 2);
  assert.equal(summary.numClbits, 2);
  assert.equal(summary.ops, 4);
  assert.equal(summary.gates, 2);
  assert.equal(summary.depth, 3);
  assert.equal(summary.memoryBytes, 32);
});

test('state memory follows the precision the simulator reports', () => {
  assert.equal(stateMemoryBytes(4, 'single'), 128);
  assert.equal(stateMemoryBytes(4, 'double'), 256);
  assert.equal(stateMemoryBytes(20, 'single'), 8 * 1024 * 1024);
});

test('a circuit with no classical register cannot be sampled', () => {
  // Sampling writes into the classical register; without one the engine refuses
  // the run outright, so both screens have to know before they call it.
  assert.equal(canSample(bell), true);
  assert.equal(canSample({ ...bell, numClbits: 0, clbitRegisters: [] }), false);
  assert.equal(canSample({ numQubits: 2 }), false, 'an IR without the field is not sampleable');
  assert.equal(canSample(null), false);
});

test('export names are slugs of the circuit name and never empty', () => {
  assert.equal(qasmFileName('Grover 4-kubitowy'), 'grover-4-kubitowy.qasm');
  assert.equal(svgFileName('Grover 4-kubitowy'), 'grover-4-kubitowy.svg');
  assert.equal(pyFileName('Grover 4-kubitowy'), 'grover-4-kubitowy.py');
  assert.equal(slugify('  ***  '), 'circuit');
  assert.equal(slugify('Splątanie ĄĆŻ'), 'splatanie-acz');
});

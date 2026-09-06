// =============================================================================
// File: modules/tentaquant/explain.test.js
// Description: The "Wyjaśnij" sentences are generated from the state and from
// nothing else, so the tests pin the CHOICE of sentence for a state worked out
// by hand — |0⟩, |+⟩, a Bell pair, a measurement — and the fact that the
// descriptors are locale-free until something translates them.
// =============================================================================

import './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const {
  explainGate, explainHistogram, explainState, explainText, gateChanges, stateFeatures,
  ENTANGLED_PURITY,
} = await import('./explain.js');

const R2 = Math.SQRT1_2;
const keys = (list) => list.map((s) => s.key);

const ground = () => ({
  bloch: [[0, 0, 1]],
  purity: [1],
  pairs: [],
  amplitudes: new Map([[0, [1, 0]]]),
  collapsing: false,
  gate: null,
});

const plus = () => ({
  bloch: [[1, 0, 0]],
  purity: [1],
  pairs: [],
  amplitudes: new Map([[0, [R2, 0]], [1, [R2, 0]]]),
  collapsing: false,
  gate: { name: 'h', qubits: [0] },
});

const bell = () => ({
  bloch: [[0, 0, 0], [0, 0, 0]],
  purity: [0.5, 0.5],
  pairs: [{ qubits: [0, 1], concurrence: 1, mutualInformation: 2 }],
  amplitudes: new Map([[0, [R2, 0]], [3, [R2, 0]]]),
  collapsing: false,
  gate: { name: 'cx', qubits: [0, 1] },
});

// ---- features --------------------------------------------------------------

test('the ground state is one basis state, pure, and not in superposition', () => {
  const features = stateFeatures(ground());
  assert.equal(features.numQubits, 1);
  assert.equal(features.qubits[0].superposed, false);
  assert.equal(features.entangled.length, 0);
  assert.deepEqual(features.amplitudes.map((a) => a.key), ['0']);
});

test('|+> is a superposition of a pure qubit', () => {
  const [qubit] = stateFeatures(plus()).qubits;
  assert.equal(qubit.superposed, true);
  assert.ok(qubit.purity >= ENTANGLED_PURITY);
});

test('a Bell pair is two mixed qubits with a concurrence-1 pair', () => {
  const features = stateFeatures(bell());
  assert.deepEqual(features.entangled, [0, 1]);
  assert.equal(features.entangledPairs.length, 1);
  assert.equal(features.qubits[0].superposed, false, 'a vector at the origin is mixed, not superposed');
});

test('amplitudes below the noise floor are not features at all', () => {
  const features = stateFeatures({
    bloch: [[0, 0, 1]], purity: [1], pairs: [], amplitudes: new Map([[0, [1, 0]], [1, [1e-6, 0]]]),
  });
  assert.equal(features.amplitudes.length, 1);
});

// ---- sentences -------------------------------------------------------------

test('a single basis state gets one sentence and no talk of superposition', () => {
  assert.deepEqual(keys(explainState(ground())), ['explain.single']);
});

test('|+> is described as a spread and a superposition', () => {
  assert.deepEqual(keys(explainState(plus())), ['explain.spread', 'explain.superposition']);
});

test('a Bell pair names the pair and its concurrence, not the purity fallback', () => {
  const sentences = explainState(bell());
  assert.deepEqual(keys(sentences), ['explain.spread', 'explain.entangled_pair']);
  assert.deepEqual(sentences[1].params, { a: 'q0', b: 'q1', c: 1 });
});

test('without a pair matrix the entanglement is reported from purity alone', () => {
  const frame = bell();
  frame.pairs = [];
  const sentences = explainState(frame);
  assert.ok(keys(sentences).includes('explain.entangled_purity'));
  assert.ok(!keys(sentences).includes('explain.entangled_pair'));
});

test('a relative phase is named only when the two heaviest amplitudes differ in phase', () => {
  const aligned = plus();
  assert.ok(!keys(explainState(aligned)).includes('explain.relative_phase'));
  const minus = plus();
  minus.amplitudes = new Map([[0, [R2, 0]], [1, [-R2, 0]]]);
  const sentence = explainState(minus).find((s) => s.key === 'explain.relative_phase');
  assert.ok(sentence, 'a sign flip is a phase of pi');
  assert.equal(sentence.params.phase, 1);
});

// ---- the gate --------------------------------------------------------------

test('the gate sentence highlights what actually moved', () => {
  const sentences = explainGate(plus(), ground());
  assert.equal(sentences[0].key, 'explain.gate_changed');
  assert.deepEqual(sentences[0].params, { gate: 'H', qubits: 'q0' });
  assert.ok(keys(sentences).includes('explain.change_superposed'));
});

test('a gate that changed nothing measurable says so instead of inventing a change', () => {
  assert.deepEqual(keys(explainGate(plus(), plus())), ['explain.gate_plain']);
});

test('entangling a qubit is reported as entanglement, not as a turn', () => {
  const changes = gateChanges(bell(), {
    bloch: [[1, 0, 0], [0, 0, 1]],
    purity: [1, 1],
    pairs: [],
    amplitudes: new Map([[0, [R2, 0]], [1, [R2, 0]]]),
  });
  assert.ok(keys(changes).includes('explain.change_entangled'));
});

test('a measurement step is its own sentence and never a diff', () => {
  const frame = { ...ground(), collapsing: true, gate: { name: 'measure', qubits: [0] } };
  assert.deepEqual(keys(explainGate(frame, plus())), ['explain.gate_measure']);
});

test('a frame with no gate gets no gate sentence', () => {
  assert.deepEqual(explainGate(ground(), null), []);
});

// Fraction 0 is the playhead sitting BEFORE the step. "Changed nothing" would
// be a verdict on a gate that has not run; the sentence has to say which it is.
test('a gate the playhead has not reached yet is pending, not ineffective', () => {
  const pending = explainGate({ ...ground(), fraction: 0, gate: { name: 'h', qubits: [0] } }, ground());
  assert.deepEqual(keys(pending), ['explain.gate_pending']);
  assert.deepEqual(pending[0].params, { gate: 'H', qubits: 'q0' });
  const applied = explainGate({ ...plus(), fraction: 1 }, ground());
  assert.equal(applied[0].key, 'explain.gate_changed');
  const text = explainText(pending);
  assert.ok(!text.includes('explain.'), text);
});

// ---- the histogram ---------------------------------------------------------

test('the histogram sentence separates a close measurement from a far one', () => {
  const close = explainHistogram({ shots: 1024, topState: '00', topCount: 506, tvd: 0.01, fidelity: 0.99 });
  assert.deepEqual(keys(close), ['explain.hist_top', 'explain.hist_close']);
  const far = explainHistogram({ shots: 64, topState: '00', topCount: 30, tvd: 0.2, fidelity: 0.8 });
  assert.deepEqual(keys(far), ['explain.hist_top', 'explain.hist_far']);
});

test('without an exact distribution the histogram says only what it measured', () => {
  assert.deepEqual(keys(explainHistogram({ shots: 100, topState: '0', topCount: 60 })), ['explain.hist_top']);
});

// ---- rendering -------------------------------------------------------------

test('descriptors become one paragraph through the translator they are given', () => {
  const text = explainText(
    [{ key: 'a', params: { n: 1 } }, { key: 'b', params: {} }],
    (key, params) => `${key}:${JSON.stringify(params)}`,
  );
  assert.equal(text, 'a:{"n":1} b:{}');
});

test('the app locale renders the Polish sentence, so the keys really exist', () => {
  const text = explainText(explainState(bell()));
  assert.ok(!text.includes('explain.'), text);
  assert.match(text, /splątane/);
});

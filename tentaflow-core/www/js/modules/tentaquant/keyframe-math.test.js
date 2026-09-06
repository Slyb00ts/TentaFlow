// =============================================================================
// File: modules/tentaquant/keyframe-math.test.js
// Description: The frames between two recorded keyframes are computed, not
// guessed, so the maths behind them is checked against states worked out by
// hand: the matrix power of a gate, the reduced density matrix it moves, and
// the amplitudes it mixes inside one gate sub-block.
// =============================================================================

import './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const {
  bitstring, blendFrames, blochFromRho, conjugateBy, densityFromAmplitudes, flatMatrix, frameAt,
  gatePower, hermitianEigen, initialFrame, interpolateFrame, localIndex, measurementSteps,
  readFrame, rhoFromBloch, tracePair, unitaryEigen,
} = await import('./keyframe-math.js');

const R2 = Math.SQRT1_2;
const close = (a, b, tolerance = 1e-9) => Math.abs(a - b) <= tolerance;
const closeVector = (a, b, tolerance = 1e-9) => a.every((v, i) => close(v, b[i], tolerance));

const H_MATRIX = [[R2, 0], [R2, 0], [R2, 0], [-R2, 0]];
const CX_MATRIX = [
  [1, 0], [0, 0], [0, 0], [0, 0],
  [0, 0], [1, 0], [0, 0], [0, 0],
  [0, 0], [0, 0], [0, 0], [1, 0],
  [0, 0], [0, 0], [1, 0], [0, 0],
];
const rz = (theta) => [
  [Math.cos(theta / 2), -Math.sin(theta / 2)], [0, 0],
  [0, 0], [Math.cos(theta / 2), Math.sin(theta / 2)],
];

// ---------------------------------------------------------------------------
// Linear algebra
// ---------------------------------------------------------------------------

test('the Jacobi sweep diagonalises a real symmetric matrix', () => {
  const { values } = hermitianEigen(flatMatrix([[2, 0], [1, 0], [1, 0], [2, 0]]), 2);
  assert.deepEqual(Array.from(values).map((v) => Math.round(v * 1e9) / 1e9).sort((a, b) => a - b), [1, 3]);
});

test('the Jacobi sweep diagonalises a complex Hermitian matrix', () => {
  // [[1, i], [-i, 1]] has eigenvalues 0 and 2.
  const { values, vectors } = hermitianEigen(flatMatrix([[1, 0], [0, 1], [0, -1], [1, 0]]), 2);
  const sorted = Array.from(values).sort((a, b) => a - b);
  assert.ok(close(sorted[0], 0, 1e-12) && close(sorted[1], 2, 1e-12), sorted.join(' '));
  // The eigenvectors come back orthonormal, which is what every use here needs.
  const dot = vectors[0] * vectors[2] + vectors[1] * vectors[3]
    + vectors[4] * vectors[6] + vectors[5] * vectors[7];
  assert.ok(Math.abs(dot) < 1e-9);
});

test('a unitary is reconstructed from the eigenvectors and phases found for it', () => {
  for (const matrix of [H_MATRIX, CX_MATRIX, rz(0.7)]) {
    const n = matrix.length === 4 ? 2 : 4;
    const decomposition = unitaryEigen(flatMatrix(matrix), n);
    assert.ok(decomposition, 'a unitary always has one');
    const power = gatePower({ matrix, qubits: [0] }, 1);
    const flat = flatMatrix(matrix);
    for (let i = 0; i < flat.length; i += 1) assert.ok(close(power.matrix[i], flat[i], 1e-9));
  }
});

test('the zeroth power of a gate is the identity and the half power of RZ halves its angle', () => {
  const identity = gatePower({ matrix: H_MATRIX, qubits: [0] }, 0).matrix;
  assert.ok(closeVector(Array.from(identity), [1, 0, 0, 0, 0, 0, 1, 0], 1e-9));
  const half = gatePower({ matrix: rz(0.8), qubits: [0] }, 0.5).matrix;
  const expected = flatMatrix(rz(0.4));
  for (let i = 0; i < expected.length; i += 1) assert.ok(close(half[i], expected[i], 1e-9));
});

test('a measurement step has no matrix and therefore no fractional form', () => {
  assert.equal(gatePower({ name: 'measure', qubits: [0], matrix: [] }, 0.5), null);
  assert.equal(gatePower(null, 0.5), null);
});

test('the Bloch vector and its density matrix are the same state read two ways', () => {
  for (const vector of [[0, 0, 1], [1, 0, 0], [0, -1, 0], [0.3, -0.2, 0.5]]) {
    assert.ok(closeVector(blochFromRho(rhoFromBloch(vector)), vector, 1e-12));
  }
});

test('the pair trace keeps the qubit it is asked for', () => {
  // |0> (x) |+>: the high bit is |0>, the low one is |+>.
  const rho = new Float64Array(32);
  for (const [row, col] of [[0, 0], [0, 1], [1, 0], [1, 1]]) rho[(row * 4 + col) * 2] = 0.5;
  assert.ok(closeVector(blochFromRho(tracePair(rho, 0)), [0, 0, 1], 1e-12));
  assert.ok(closeVector(blochFromRho(tracePair(rho, 1)), [1, 0, 0], 1e-12));
});

test('a basis index is packed with the first gate qubit as the high bit', () => {
  // index 0b10 = qubit 1 set; with qubits [1, 0] that is the local index 0b10.
  assert.equal(localIndex(0b10, [1, 0]), 0b10);
  assert.equal(localIndex(0b10, [0, 1]), 0b01);
  assert.equal(localIndex(0b101, [2, 0]), 0b11);
  assert.equal(bitstring(2, 3), '010');
});

// ---------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------

/// The keyframe a node sends after `H q0` on a one-qubit register.
const hFrame = () => ({
  step: 1,
  gate: { name: 'h', qubits: [0], matrix: H_MATRIX },
  bloch: [[1, 0, 0]],
  purity: [1],
  pairs: [],
  top: [{ index: 0, amplitude: [R2, 0], partners: [{ index: 1, amplitude: [R2, 0] }] }],
  probsTop: [{ bitstring: '0', probability: 0.5 }, { bitstring: '1', probability: 0.5 }],
});

/// ...and after `CX q0, q1` on top of it: the Bell state.
const cxFrame = () => ({
  step: 2,
  gate: { name: 'cx', qubits: [0, 1], matrix: CX_MATRIX },
  bloch: [[0, 0, 0], [0, 0, 0]],
  purity: [0.5, 0.5],
  pairs: [{
    qubits: [0, 1],
    rho: [
      [0.5, 0], [0, 0], [0, 0], [0.5, 0],
      [0, 0], [0, 0], [0, 0], [0, 0],
      [0, 0], [0, 0], [0, 0], [0, 0],
      [0.5, 0], [0, 0], [0, 0], [0.5, 0],
    ],
    mutualInformation: 2,
    concurrence: 1,
  }],
  top: [{ index: 0, amplitude: [R2, 0], partners: [{ index: 1, amplitude: [0, 0] }, { index: 2, amplitude: [0, 0] }, { index: 3, amplitude: [R2, 0] }] }],
  probsTop: [{ bitstring: '00', probability: 0.5 }, { bitstring: '11', probability: 0.5 }],
});

test('half of an H is half a turn around the H axis, not the shortest way to |+>', () => {
  const frame = interpolateFrame(hFrame(), 0.5);
  // H rotates the Bloch sphere by pi about (x+z)/sqrt(2). Halfway through, the
  // state has LEFT the x-z plane: (1/2, -sqrt(2)/2, 1/2), and not the
  // great-circle midpoint (sqrt(2)/2, 0, sqrt(2)/2) an eye would guess.
  assert.ok(closeVector(frame.bloch[0], [0.5, -R2, 0.5], 1e-9), frame.bloch[0].join(' '));
  const length = Math.hypot(...frame.bloch[0]);
  assert.ok(close(length, 1, 1e-12), 'a pure state stays pure through the gate');
  assert.equal(frame.exact, true);
});

test('the ends of a step are the states the two keyframes recorded', () => {
  const start = interpolateFrame(hFrame(), 0);
  assert.ok(closeVector(start.bloch[0], [0, 0, 1], 1e-9), 'before H the qubit is |0>');
  assert.ok(close(start.amplitudes.get(0)[0], 1, 1e-9));
  assert.ok(close(start.amplitudes.get(1)[0], 0, 1e-9));
  const end = interpolateFrame(hFrame(), 1);
  assert.ok(closeVector(end.bloch[0], [1, 0, 0], 1e-9));
  assert.ok(close(end.amplitudes.get(1)[0], R2, 1e-9));
});

test('a two-qubit gate moves the pair matrix, so both marginals are exact', () => {
  const start = interpolateFrame(cxFrame(), 0);
  assert.ok(closeVector(start.bloch[0], [1, 0, 0], 1e-9), 'the control was |+> before CX');
  assert.ok(closeVector(start.bloch[1], [0, 0, 1], 1e-9), 'the target was |0>');
  assert.ok(close(start.amplitudes.get(1)[0], R2, 1e-9), 'the amplitude sat on |01> before CX');
  assert.ok(close(start.amplitudes.get(3)[0], 0, 1e-9));
  const middle = interpolateFrame(cxFrame(), 0.5);
  assert.ok(Math.hypot(...middle.bloch[0]) < 1 + 1e-9);
  assert.ok(Math.hypot(...middle.bloch[1]) < 1 + 1e-9, 'a qubit never leaves the sphere');
});

test('a two-qubit step without its pair matrix is not interpolated at all', () => {
  const frame = cxFrame();
  frame.pairs = [];
  assert.equal(interpolateFrame(frame, 0.5), null, 'the marginals are simply not determined');
});

test('the position picks the frame, and an integer position is the recording itself', () => {
  const frames = [hFrame(), cxFrame()];
  const second = frameAt(frames, 2);
  assert.equal(second.exact, true);
  assert.equal(second.step, 2);
  assert.ok(closeVector(second.bloch[0], [0, 0, 0], 1e-12), 'the stored frame is used verbatim');
  const first = frameAt(frames, 1);
  assert.equal(first.step, 1);
  const between = frameAt(frames, 1.5);
  assert.equal(between.step, 2, 'the second half of the recording is inside the CX');
  assert.ok(closeVector(frameAt(frames, 0).bloch[0], [0, 0, 1], 1e-9), 'position 0 is the register before the run');
  assert.equal(frameAt([], 1), null);
});

test('a measurement is drawn as a collapse between the two frames around it', () => {
  const measured = {
    step: 2,
    gate: { name: 'measure', qubits: [0], matrix: [] },
    bloch: [[0, 0, -1]],
    purity: [1],
    pairs: [],
    top: [{ index: 1, amplitude: [1, 0], partners: [] }],
    probsTop: [{ bitstring: '1', probability: 1 }],
  };
  const frames = [hFrame(), measured];
  const middle = frameAt(frames, 1.5);
  assert.equal(middle.exact, false);
  assert.equal(middle.collapsing, true);
  // Halfway the measured branch already outweighs the one that faded.
  const one = middle.amplitudes.get(1)[0];
  const zero = middle.amplitudes.get(0)[0];
  assert.ok(one > zero, `${one} <= ${zero}`);
  assert.ok(close(one * one + zero * zero, 1, 1e-9), 'the drawn state stays normalised');
  assert.deepEqual(measurementSteps(frames), [2]);
});

test('the initial frame is the register a run starts from and nothing more', () => {
  const frame = initialFrame(2);
  assert.deepEqual(frame.bloch, [[0, 0, 1], [0, 0, 1]]);
  assert.deepEqual(frame.probs, [{ bitstring: '00', probability: 1 }]);
});

test('reading a frame keeps the purity the node measured', () => {
  const frame = readFrame(cxFrame());
  assert.deepEqual(frame.purity, [0.5, 0.5]);
  assert.equal(frame.pairs.length, 1);
  assert.equal(frame.amplitudes.size, 4);
});

test('blending two frames is a straight line between them, renormalised', () => {
  const blended = blendFrames(initialFrame(1), readFrame(hFrame()), 1);
  assert.ok(close(blended.amplitudes.get(0)[0], R2, 1e-9));
  assert.ok(closeVector(blended.bloch[0], [1, 0, 0], 1e-9));
});

test('conjugation by a unitary keeps the trace of a density matrix', () => {
  const rho = rhoFromBloch([0.2, -0.3, 0.4]);
  const moved = conjugateBy(flatMatrix(H_MATRIX), rho, 2);
  assert.ok(close(moved[0] + moved[6], 1, 1e-12));
});

test('the full density matrix of a state vector is |psi><psi|', () => {
  // The Bell state: 0.5 on the four corners of a 4x4, zero elsewhere.
  const { dim, rho } = densityFromAmplitudes([R2, 0, 0, 0, 0, 0, R2, 0], 2);
  assert.equal(dim, 4);
  const at = (row, col) => [rho[(row * 4 + col) * 2], rho[(row * 4 + col) * 2 + 1]];
  for (const [row, col] of [[0, 0], [0, 3], [3, 0], [3, 3]]) {
    assert.ok(closeVector(at(row, col), [0.5, 0], 1e-12), `${row},${col}`);
  }
  assert.ok(closeVector(at(1, 1), [0, 0], 1e-12));
  // The trace of a normalised state is one.
  let trace = 0;
  for (let i = 0; i < 4; i += 1) trace += rho[(i * 4 + i) * 2];
  assert.ok(close(trace, 1, 1e-12));
});

test('a phase in the state vector lands in the off-diagonal of rho, not on its trace', () => {
  // (|0> + i|1>)/sqrt(2) on one qubit.
  const { dim, rho } = densityFromAmplitudes([R2, 0, 0, R2], 1);
  assert.equal(dim, 2);
  // rho[0][1] = a0 * conj(a1) = (1/sqrt2)(−i/sqrt2) = −i/2.
  assert.ok(closeVector([rho[2], rho[3]], [0, -0.5], 1e-12));
  assert.ok(close(rho[0] + rho[6], 1, 1e-12));
});

test('a state vector shorter than the register it claims is refused', () => {
  assert.equal(densityFromAmplitudes([1, 0], 2).dim, 0);
});

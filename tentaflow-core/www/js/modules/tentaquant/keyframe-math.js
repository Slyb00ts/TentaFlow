// ===== File: modules/tentaquant/keyframe-math.js — exact frames between two state keyframes =====
//
// Plan §13.6 promises an ANIMATION and not a slideshow: a run recorded on a
// node sends one `StateKeyframe` per program step, and the browser has to draw
// everything in between. It can do that EXACTLY, without ever seeing the state
// vector, because a gate acting on the qubits `A` commutes with the partial
// trace over the rest:
//
//     rho_A(t) = U^t rho_A(0) U^t†,   U^t = V diag(lambda^t) V†
//
// so the reduced quantities of the gate's own qubits follow from the 2x2 or
// 4x4 matrix the keyframe carries, and every other qubit does not move at all.
// The same argument holds for the amplitudes: a 1-qubit gate mixes only pairs
// of indices differing in that qubit's bit, a 2-qubit gate only quadruples —
// which is exactly what `top[].partners` carries.
//
// The interpolation runs BACKWARD from the frame it belongs to. A keyframe is
// taken AFTER its gate and carries the pair matrix and the amplitude partners
// of THAT gate, so the state at fraction `f` of the step is
// `U^(f-1) rho(1) U^(f-1)†` — the frame before it does not have to carry
// anything about the gate that follows, and none of this needs a second frame.
//
// Everything here is pure: no DOM, no wasm, no clock. `U^t` is the principal
// matrix power (`lambda^t = exp(i·t·arg lambda)`), so a rotation gate's power
// is the same rotation with a scaled angle and `H^0.5` is half a turn around
// the H axis — NOT the shortest path from |0> to |+>, which is a different
// state altogether (see the tests).

import { purityFromVector } from '/js/components/tf-bloch-sphere.js';

/// Off-diagonal norm under which the Jacobi sweep is done, and the tolerance a
/// reconstructed eigendecomposition has to meet before it is trusted.
const JACOBI_EPS = 1e-13;
const RECONSTRUCT_EPS = 1e-8;

// ---------------------------------------------------------------------------
// Dense complex matrices — flat `[re, im, re, im, ...]`, row-major
// ---------------------------------------------------------------------------

/// The wire shape of a gate matrix (`Vec<[f64; 2]>`) as a flat array.
export function flatMatrix(entries) {
  const list = Array.isArray(entries) ? entries : [];
  const out = new Float64Array(list.length * 2);
  for (let i = 0; i < list.length; i += 1) {
    const cell = list[i] || [];
    out[i * 2] = Number(cell[0]) || 0;
    out[i * 2 + 1] = Number(cell[1]) || 0;
  }
  return out;
}

export function matmul(a, b, n) {
  const out = new Float64Array(n * n * 2);
  for (let i = 0; i < n; i += 1) {
    for (let j = 0; j < n; j += 1) {
      let re = 0;
      let im = 0;
      for (let k = 0; k < n; k += 1) {
        const ar = a[(i * n + k) * 2];
        const ai = a[(i * n + k) * 2 + 1];
        const br = b[(k * n + j) * 2];
        const bi = b[(k * n + j) * 2 + 1];
        re += ar * br - ai * bi;
        im += ar * bi + ai * br;
      }
      out[(i * n + j) * 2] = re;
      out[(i * n + j) * 2 + 1] = im;
    }
  }
  return out;
}

export function conjugateTranspose(a, n) {
  const out = new Float64Array(n * n * 2);
  for (let i = 0; i < n; i += 1) {
    for (let j = 0; j < n; j += 1) {
      out[(j * n + i) * 2] = a[(i * n + j) * 2];
      out[(j * n + i) * 2 + 1] = -a[(i * n + j) * 2 + 1];
    }
  }
  return out;
}

/// `U rho U†` — how every reduced density matrix here is moved.
export function conjugateBy(u, rho, n) {
  return matmul(matmul(u, rho, n), conjugateTranspose(u, n), n);
}

function identity(n) {
  const out = new Float64Array(n * n * 2);
  for (let i = 0; i < n; i += 1) out[(i * n + i) * 2] = 1;
  return out;
}

// ---------------------------------------------------------------------------
// Eigenvalues
// ---------------------------------------------------------------------------

/// Cyclic Jacobi for a HERMITIAN complex matrix. Answers the real eigenvalues
/// and the unitary whose COLUMNS are the eigenvectors. `n` is 2 or 4 here, so
/// the O(n^3) sweep is nothing; what matters is that it needs no library and
/// is exact enough to reconstruct the matrix it came from.
export function hermitianEigen(matrix, n) {
  const a = Float64Array.from(matrix);
  const v = identity(n);
  for (let sweep = 0; sweep < 60; sweep += 1) {
    let off = 0;
    for (let i = 0; i < n; i += 1) {
      for (let j = i + 1; j < n; j += 1) {
        const re = a[(i * n + j) * 2];
        const im = a[(i * n + j) * 2 + 1];
        off += re * re + im * im;
      }
    }
    if (Math.sqrt(off) < JACOBI_EPS) break;
    for (let p = 0; p < n; p += 1) {
      for (let q = p + 1; q < n; q += 1) {
        const zr = a[(p * n + q) * 2];
        const zi = a[(p * n + q) * 2 + 1];
        const r = Math.hypot(zr, zi);
        if (r < 1e-18) continue;
        const phi = Math.atan2(zi, zr);
        // The phase is rotated out first, which leaves the real symmetric
        // 2x2 [[app, r], [r, aqq]] the textbook Jacobi angle diagonalises.
        const theta = 0.5 * Math.atan2(-2 * r, a[(p * n + p) * 2] - a[(q * n + q) * 2]);
        const c = Math.cos(theta);
        const s = Math.sin(theta);
        const gpq = [s * Math.cos(phi), s * Math.sin(phi)];
        const gqp = [-s * Math.cos(phi), s * Math.sin(phi)];
        applyRotation(a, n, p, q, c, gpq, gqp);
        rotateColumns(v, n, p, q, c, gpq, gqp);
      }
    }
  }
  const values = new Float64Array(n);
  for (let i = 0; i < n; i += 1) values[i] = a[(i * n + i) * 2];
  return { values, vectors: v };
}

/// `A <- G† A G` for the plane rotation `G` that is the identity outside
/// (p, q), with `G[p][q] = gpq` and `G[q][p] = gqp`.
function applyRotation(a, n, p, q, c, gpq, gqp) {
  rotateColumns(a, n, p, q, c, gpq, gqp);
  rotateRowsConj(a, n, p, q, c, gpq, gqp);
}

function rotateColumns(a, n, p, q, c, gpq, gqp) {
  for (let i = 0; i < n; i += 1) {
    const pr = a[(i * n + p) * 2];
    const pi = a[(i * n + p) * 2 + 1];
    const qr = a[(i * n + q) * 2];
    const qi = a[(i * n + q) * 2 + 1];
    // column p <- c * col_p + gqp * col_q ; column q <- gpq * col_p + c * col_q
    a[(i * n + p) * 2] = c * pr + (gqp[0] * qr - gqp[1] * qi);
    a[(i * n + p) * 2 + 1] = c * pi + (gqp[0] * qi + gqp[1] * qr);
    a[(i * n + q) * 2] = (gpq[0] * pr - gpq[1] * pi) + c * qr;
    a[(i * n + q) * 2 + 1] = (gpq[0] * pi + gpq[1] * pr) + c * qi;
  }
}

function rotateRowsConj(a, n, p, q, c, gpq, gqp) {
  for (let j = 0; j < n; j += 1) {
    const pr = a[(p * n + j) * 2];
    const pi = a[(p * n + j) * 2 + 1];
    const qr = a[(q * n + j) * 2];
    const qi = a[(q * n + j) * 2 + 1];
    // row p <- conj(c) * row_p + conj(gqp) * row_q, and the mirror for q.
    a[(p * n + j) * 2] = c * pr + (gqp[0] * qr + gqp[1] * qi);
    a[(p * n + j) * 2 + 1] = c * pi + (gqp[0] * qi - gqp[1] * qr);
    a[(q * n + j) * 2] = (gpq[0] * pr + gpq[1] * pi) + c * qr;
    a[(q * n + j) * 2 + 1] = (gpq[0] * pi - gpq[1] * pr) + c * qi;
  }
}

/// Eigendecomposition of a UNITARY: the eigenvectors of the Hermitian
/// `cos(phi)·(U+U†)/2 + sin(phi)·(U-U†)/2i` are eigenvectors of `U` for every
/// phi, because both parts are functions of U. A phi that makes two DIFFERENT
/// eigenvalues of U collapse onto one eigenvalue of that Hermitian mixes their
/// eigenvectors, so the result is verified by reconstruction and the next
/// angle is tried instead of trusting it.
export function unitaryEigen(matrix, n) {
  const dagger = conjugateTranspose(matrix, n);
  const real = new Float64Array(n * n * 2);
  const imaginary = new Float64Array(n * n * 2);
  for (let i = 0; i < n * n * 2; i += 1) real[i] = (matrix[i] + dagger[i]) / 2;
  for (let i = 0; i < n * n; i += 1) {
    // (U - U†)/(2i): dividing by i turns (re, im) into (im, -re).
    const re = (matrix[i * 2] - dagger[i * 2]) / 2;
    const im = (matrix[i * 2 + 1] - dagger[i * 2 + 1]) / 2;
    imaginary[i * 2] = im;
    imaginary[i * 2 + 1] = -re;
  }
  for (const phi of [0.4, 1.3, 2.5, 0.9]) {
    const mixed = new Float64Array(n * n * 2);
    const cp = Math.cos(phi);
    const sp = Math.sin(phi);
    for (let i = 0; i < mixed.length; i += 1) mixed[i] = cp * real[i] + sp * imaginary[i];
    const { vectors } = hermitianEigen(mixed, n);
    const phases = eigenPhases(matrix, vectors, n);
    if (phases && reconstructionError(matrix, vectors, phases, n) < RECONSTRUCT_EPS) {
      return { vectors, phases };
    }
  }
  return null;
}

/// `arg(v† U v)` per eigenvector, or null when one of them is not an
/// eigenvector after all (the magnitude of a true eigenvalue is 1).
function eigenPhases(u, vectors, n) {
  const phases = new Float64Array(n);
  for (let k = 0; k < n; k += 1) {
    let re = 0;
    let im = 0;
    for (let i = 0; i < n; i += 1) {
      // (U v)_i
      let ur = 0;
      let ui = 0;
      for (let j = 0; j < n; j += 1) {
        const ar = u[(i * n + j) * 2];
        const ai = u[(i * n + j) * 2 + 1];
        const vr = vectors[(j * n + k) * 2];
        const vi = vectors[(j * n + k) * 2 + 1];
        ur += ar * vr - ai * vi;
        ui += ar * vi + ai * vr;
      }
      const cr = vectors[(i * n + k) * 2];
      const ci = -vectors[(i * n + k) * 2 + 1];
      re += cr * ur - ci * ui;
      im += cr * ui + ci * ur;
    }
    if (Math.abs(Math.hypot(re, im) - 1) > 1e-6) return null;
    phases[k] = Math.atan2(im, re);
  }
  return phases;
}

function reconstructionError(u, vectors, phases, n) {
  const rebuilt = spectralPower(vectors, phases, n, 1);
  let worst = 0;
  for (let i = 0; i < u.length; i += 1) worst = Math.max(worst, Math.abs(u[i] - rebuilt[i]));
  return worst;
}

/// `V diag(exp(i·t·phase)) V†`.
function spectralPower(vectors, phases, n, t) {
  const out = new Float64Array(n * n * 2);
  for (let k = 0; k < n; k += 1) {
    const angle = phases[k] * t;
    const lr = Math.cos(angle);
    const li = Math.sin(angle);
    for (let i = 0; i < n; i += 1) {
      const vr = vectors[(i * n + k) * 2];
      const vi = vectors[(i * n + k) * 2 + 1];
      for (let j = 0; j < n; j += 1) {
        const wr = vectors[(j * n + k) * 2];
        const wi = -vectors[(j * n + k) * 2 + 1];
        // lambda^t * v_i * conj(v_j)
        const pr = vr * wr - vi * wi;
        const pi = vr * wi + vi * wr;
        out[(i * n + j) * 2] += lr * pr - li * pi;
        out[(i * n + j) * 2 + 1] += lr * pi + li * pr;
      }
    }
  }
  return out;
}

// The decomposition is the expensive part and a keyframe's gate object is
// stable for the life of the frame, so it is computed once per gate (plan
// §13.6: "liczony raz per bramka").
const DECOMPOSITIONS = new WeakMap();

/// `U^t` of a keyframe gate, or null when the step has no matrix to raise —
/// a measurement or a reset, which has no fractional form at all.
export function gatePower(gate, t) {
  if (!gate || !Array.isArray(gate.matrix) || !gate.matrix.length) return null;
  const n = gate.matrix.length === 4 ? 2 : (gate.matrix.length === 16 ? 4 : 0);
  if (!n) return null;
  let decomposition = DECOMPOSITIONS.get(gate);
  if (decomposition === undefined) {
    decomposition = unitaryEigen(flatMatrix(gate.matrix), n) || null;
    DECOMPOSITIONS.set(gate, decomposition);
  }
  if (!decomposition) return null;
  return { matrix: spectralPower(decomposition.vectors, decomposition.phases, n, t), dim: n };
}

// ---------------------------------------------------------------------------
// One-qubit and two-qubit reduced states
// ---------------------------------------------------------------------------

/// rho = (I + x·X + y·Y + z·Z) / 2.
export function rhoFromBloch(vector) {
  const [x = 0, y = 0, z = 0] = Array.from(vector || [], Number);
  return Float64Array.from([
    (1 + z) / 2, 0, x / 2, -y / 2,
    x / 2, y / 2, (1 - z) / 2, 0,
  ]);
}

export function blochFromRho(rho) {
  return [
    2 * rho[2],
    -2 * rho[3],
    rho[0] - rho[6],
  ];
}

/// Partial trace of a 4x4 pair matrix. `keep` is 0 for the qubit that is the
/// HIGH bit of the pair index and 1 for the low one — the convention
/// `analysis::reduced_density_matrix` and the gate matrices share
/// (`qubits[0]` is the most significant bit).
export function tracePair(rho, keep) {
  const out = new Float64Array(8);
  for (let a = 0; a < 2; a += 1) {
    for (let b = 0; b < 2; b += 1) {
      let re = 0;
      let im = 0;
      for (let t = 0; t < 2; t += 1) {
        const row = keep === 0 ? a * 2 + t : t * 2 + a;
        const col = keep === 0 ? b * 2 + t : t * 2 + b;
        re += rho[(row * 4 + col) * 2];
        im += rho[(row * 4 + col) * 2 + 1];
      }
      out[(a * 2 + b) * 2] = re;
      out[(a * 2 + b) * 2 + 1] = im;
    }
  }
  return out;
}

/// Position of a basis index inside its gate sub-block: bit of `qubits[0]` is
/// the most significant, as in the matrices the simulator applies.
export function localIndex(index, qubits) {
  const list = Array.from(qubits || [], Number);
  let local = 0;
  for (let b = 0; b < list.length; b += 1) {
    local = (local << 1) | ((Number(index) >> list[b]) & 1);
  }
  return local;
}

// ---------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------

const blochList = (frame) => (frame && Array.isArray(frame.bloch) ? frame.bloch : [])
  .map((v) => Array.from(v || [], Number).slice(0, 3));

const probsOf = (frame) => ((frame && (frame.probsTop ?? frame.probs_top)) || [])
  .map((row) => ({ bitstring: String(row.bitstring), probability: Number(row.probability) || 0 }));

/// The amplitudes a keyframe carries, flattened out of its `top` groups.
export function frameAmplitudes(frame) {
  const out = new Map();
  for (const group of (frame && frame.top) || []) {
    const index = Number(group.index) || 0;
    const amplitude = group.amplitude || [0, 0];
    out.set(index, [Number(amplitude[0]) || 0, Number(amplitude[1]) || 0]);
    for (const partner of group.partners || []) {
      const key = Number(partner.index) || 0;
      const value = partner.amplitude || [0, 0];
      if (!out.has(key)) out.set(key, [Number(value[0]) || 0, Number(value[1]) || 0]);
    }
  }
  return out;
}

/// One keyframe as the views read it, with nothing interpolated.
export function readFrame(frame) {
  const bloch = blochList(frame);
  return {
    step: Number(frame && frame.step) || 0,
    gate: (frame && frame.gate) || null,
    bloch,
    purity: bloch.map((v, i) => {
      const stored = Number((frame.purity || [])[i]);
      return Number.isFinite(stored) ? stored : purityFromVector(v);
    }),
    amplitudes: frameAmplitudes(frame),
    probs: probsOf(frame),
    pairs: (frame && frame.pairs) || [],
    fraction: 1,
    exact: true,
    collapsing: false,
  };
}

/// The state at fraction `f` of the step that produced `frame`, computed
/// backward with `U^(f-1)`. Returns null when the gate has no matrix.
export function interpolateFrame(frame, f) {
  const gate = frame && frame.gate;
  const power = gatePower(gate, f - 1);
  if (!power) return null;
  const qubits = Array.from(gate.qubits || [], Number);
  if (!qubits.length || (power.dim === 2 && qubits.length !== 1) || (power.dim === 4 && qubits.length !== 2)) {
    return null;
  }
  const bloch = blochList(frame);
  const amplitudes = movedAmplitudes(frame, power, qubits);
  if (!amplitudes) return null;
  if (power.dim === 2) {
    const q = qubits[0];
    if (!bloch[q]) return null;
    bloch[q] = blochFromRho(conjugateBy(power.matrix, rhoFromBloch(bloch[q]), 2));
  } else {
    const pair = (frame.pairs || []).find((p) => {
      const list = Array.from(p.qubits || [], Number);
      return list[0] === qubits[0] && list[1] === qubits[1];
    });
    // Without the pair matrix the two marginals are simply not determined by
    // anything the frame carries: the caller falls back to a blend rather than
    // drawing a guess.
    if (!pair || !Array.isArray(pair.rho) || pair.rho.length !== 16) return null;
    const moved = conjugateBy(power.matrix, flatMatrix(pair.rho), 4);
    if (!bloch[qubits[0]] || !bloch[qubits[1]]) return null;
    bloch[qubits[0]] = blochFromRho(tracePair(moved, 0));
    bloch[qubits[1]] = blochFromRho(tracePair(moved, 1));
  }
  return {
    step: Number(frame.step) || 0,
    gate,
    bloch,
    purity: bloch.map(purityFromVector),
    amplitudes,
    probs: probsFromAmplitudes(amplitudes, bloch.length),
    pairs: [],
    fraction: f,
    exact: true,
    collapsing: false,
  };
}

/// Every amplitude the frame carries, moved by `U^(f-1)` inside its own gate
/// sub-block. The block is closed under the gate, which is the whole reason
/// `top[].partners` exists.
function movedAmplitudes(frame, power, qubits) {
  const stored = frameAmplitudes(frame);
  if (!stored.size) return null;
  const out = new Map();
  const size = power.dim;
  for (const group of frame.top || []) {
    const indices = new Array(size).fill(null);
    const members = [Number(group.index) || 0, ...(group.partners || []).map((p) => Number(p.index) || 0)];
    for (const index of members) indices[localIndex(index, qubits)] = index;
    if (indices.some((index) => index === null || !stored.has(index))) continue;
    for (let row = 0; row < size; row += 1) {
      let re = 0;
      let im = 0;
      for (let col = 0; col < size; col += 1) {
        const [vr, vi] = stored.get(indices[col]);
        const ar = power.matrix[(row * size + col) * 2];
        const ai = power.matrix[(row * size + col) * 2 + 1];
        re += ar * vr - ai * vi;
        im += ar * vi + ai * vr;
      }
      out.set(indices[row], [re, im]);
    }
  }
  return out.size ? out : null;
}

function probsFromAmplitudes(amplitudes, numQubits) {
  const rows = [];
  for (const [index, [re, im]] of amplitudes) {
    const probability = re * re + im * im;
    if (probability < 1e-12) continue;
    rows.push({ bitstring: bitstring(index, numQubits), probability });
  }
  rows.sort((a, b) => b.probability - a.probability);
  return rows;
}

/// Basis label of an index, bit 0 rightmost — the same key the simulator's
/// counts use (`sim::statevector::bitstring`).
export function bitstring(index, numQubits) {
  const width = Math.max(1, Number(numQubits) || 0);
  let out = '';
  for (let bit = width - 1; bit >= 0; bit -= 1) out += ((Number(index) >> bit) & 1) ? '1' : '0';
  return out;
}

/// The state |0...0> a run starts from, in the shape of a read frame. It is
/// not a guess: a run always begins there, and it is what the first step is
/// interpolated away from when that step is a measurement.
export function initialFrame(numQubits) {
  const n = Math.max(0, Number(numQubits) || 0);
  return {
    step: 0,
    gate: null,
    bloch: Array.from({ length: n }, () => [0, 0, 1]),
    purity: Array.from({ length: n }, () => 1),
    amplitudes: new Map([[0, [1, 0]]]),
    probs: [{ bitstring: bitstring(0, n), probability: 1 }],
    pairs: [],
    fraction: 1,
    exact: true,
    collapsing: false,
  };
}

/// The full density matrix rho = |psi><psi| of a state vector, flat row-major
/// `[re, im, ...]`. It is O(4^n) in both time and memory, which is why §13.6
/// draws it only up to six qubits and pair matrices above that — the caller
/// enforces the ceiling, this function does the arithmetic.
export function densityFromAmplitudes(amplitudes, numQubits) {
  const n = Math.max(0, Number(numQubits) || 0);
  const dim = 1 << n;
  const flat = Array.from(amplitudes || [], Number);
  if (flat.length < dim * 2) return { dim: 0, rho: new Float64Array(0) };
  const rho = new Float64Array(dim * dim * 2);
  for (let i = 0; i < dim; i += 1) {
    const ar = flat[i * 2];
    const ai = flat[i * 2 + 1];
    for (let j = 0; j < dim; j += 1) {
      // a_i * conj(a_j)
      const br = flat[j * 2];
      const bi = -flat[j * 2 + 1];
      rho[(i * dim + j) * 2] = ar * br - ai * bi;
      rho[(i * dim + j) * 2 + 1] = ar * bi + ai * br;
    }
  }
  return { dim, rho };
}

/// A step with no fractional form — a measurement or a reset — drawn as the
/// collapse of §13.6: the branch that was not measured fades out, the measured
/// one grows back to norm, and the spheres travel over the sphere rather than
/// through it.
export function blendFrames(before, after, f) {
  const t = Math.max(0, Math.min(1, Number(f) || 0));
  const bloch = after.bloch.map((vector, i) => mixVector(before.bloch[i] || vector, vector, t));
  const amplitudes = new Map();
  const keys = new Set([...before.amplitudes.keys(), ...after.amplitudes.keys()]);
  let norm = 0;
  for (const key of keys) {
    const [ar, ai] = before.amplitudes.get(key) || [0, 0];
    const [br, bi] = after.amplitudes.get(key) || [0, 0];
    const re = ar + (br - ar) * t;
    const im = ai + (bi - ai) * t;
    amplitudes.set(key, [re, im]);
    norm += re * re + im * im;
  }
  if (norm > 0) {
    const scale = 1 / Math.sqrt(norm);
    for (const [key, value] of amplitudes) amplitudes.set(key, [value[0] * scale, value[1] * scale]);
  }
  return {
    step: after.step,
    gate: after.gate,
    bloch,
    purity: bloch.map(purityFromVector),
    amplitudes,
    probs: probsFromAmplitudes(amplitudes, bloch.length),
    pairs: [],
    fraction: t,
    exact: false,
    collapsing: true,
  };
}

/// Linear on the way in, but renormalised to the shorter of the two lengths so
/// a shrinking vector never bulges on the way.
function mixVector(from, to, t) {
  return [0, 1, 2].map((i) => (Number(from[i]) || 0) + ((Number(to[i]) || 0) - (Number(from[i]) || 0)) * t);
}

/// The frame to draw at `position`, a continuous coordinate over the recorded
/// steps: an integer `k` lands exactly on `frames[k-1]`, `0` is the register
/// before the first step, and anything between is inside one step.
export function frameAt(frames, position) {
  const list = Array.isArray(frames) ? frames : [];
  if (!list.length) return null;
  const total = list.length;
  const p = Math.max(0, Math.min(Number(position) || 0, total));
  const upper = Math.max(1, Math.ceil(p < 1e-9 ? 1 : p));
  const f = p - (upper - 1);
  const frame = list[upper - 1];
  if (f >= 1 - 1e-9) return readFrame(frame);
  const exact = interpolateFrame(frame, f);
  if (exact) return exact;
  const after = readFrame(frame);
  const before = upper >= 2 ? readFrame(list[upper - 2]) : initialFrame(after.bloch.length);
  return blendFrames(before, after, f);
}

/// How many program steps the recording holds, and how many of them collapse
/// the register — what the shot histogram fills with (§13.6: the histogram
/// fills at the measurement, not before it).
export function measurementSteps(frames) {
  return (Array.isArray(frames) ? frames : [])
    .map((frame, index) => ({ frame, index }))
    .filter(({ frame }) => isCollapsingGate(frame.gate))
    .map(({ index }) => index + 1);
}

export function isCollapsingGate(gate) {
  const name = String((gate && gate.name) || '');
  return name === 'measure' || name === 'reset';
}

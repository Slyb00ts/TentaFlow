// =============================================================================
// File: index.test.js — the JS facade over the TentaQuant wasm simulator
// Description: Exercises www/js/quantum/index.js against the REAL glue that
// tentaflow-core/build.rs emits (quantum_glue.js + quantum_glue_bg.wasm). Those
// artefacts are generated, not committed, so when they are absent every test
// here reports skip with that reason — a facade test that passes without a
// simulator behind it would be worse than no test at all.
//
// The browser initialises the glue by fetching its .wasm from the dashboard
// origin; Node has no such origin, so `fetch` is answered from disk. That keeps
// index.js on exactly the load path it takes in a browser instead of testing a
// second, test-only one.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const gluePath = fileURLToPath(new URL('./quantum_glue.js', import.meta.url));
const wasmPath = fileURLToPath(new URL('./quantum_glue_bg.wasm', import.meta.url));

const missing = [gluePath, wasmPath].filter((path) => !existsSync(path));
const skip = missing.length
  ? `the wasm glue is not built (${missing.join(', ')}); run \`cargo check\` in tentaflow-core `
    + 'with the wasm32-unknown-unknown target and wasm-bindgen-cli installed'
  : false;
const options = skip ? { skip } : {};

if (!skip) {
  globalThis.fetch = async (url) =>
    new Response(readFileSync(fileURLToPath(url)), {
      headers: { 'content-type': 'application/wasm' },
    });
}

const quantum = await import('./index.js');

const BELL = 'OPENQASM 3.0;\ninclude "stdgates.inc";\nqubit[2] q;\nbit[2] c;\nh q[0];\ncx q[0], q[1];\nc = measure q;\n';
const CX = 'OPENQASM 3.0;\ninclude "stdgates.inc";\nqubit[2] q;\ncx q[0], q[1];\n';

/**
 * Every object key under `value` still spelled the Rust way, with its path.
 * Field names on this boundary are camelCase; the PascalCase keys that remain
 * are serde's enum discriminants (`kind: {Gate: …}`), which name IR variants
 * and stay as the crate and the README spell them.
 */
function rustSpelledKeys(value, path = '$') {
  if (Array.isArray(value)) {
    return value.flatMap((entry, index) => rustSpelledKeys(entry, `${path}[${index}]`));
  }
  if (value === null || typeof value !== 'object') return [];
  return Object.entries(value).flatMap(([key, entry]) => {
    const found = rustSpelledKeys(entry, `${path}.${key}`);
    return key.includes('_') ? [`${path}.${key}`, ...found] : found;
  });
}

test('the glue on disk loads and reports itself available', options, async () => {
  assert.equal(await quantum.available(), true);
  assert.ok(await quantum.ready());
});

test('parse returns a camelCase envelope and a camelCase IR', options, async () => {
  const parsed = await quantum.parse(BELL);
  assert.equal(parsed.status, 'parsed');
  assert.equal(parsed.numQubits, 2);
  assert.equal(parsed.numClbits, 2);
  assert.equal(parsed.isClifford, true);
  assert.deepEqual(rustSpelledKeys(parsed), []);
  assert.equal(parsed.circuit.qubitRegisters[0].name, 'q');
  assert.equal(parsed.circuit.clbitRegisters[0].size, 2);
  assert.deepEqual(parsed.circuit.ops[0].kind, { Gate: { gate: 'H', qubits: [0] } });
});

test('the IR round-trips back through toQasm3 and isClifford', options, async () => {
  const parsed = await quantum.parse(BELL);
  const emitted = await quantum.toQasm3(parsed.circuit);
  const reparsed = await quantum.parse(emitted);
  assert.equal(reparsed.status, 'parsed');
  assert.deepEqual(reparsed.circuit, parsed.circuit);
  assert.equal(await quantum.isClifford(parsed.circuit), true);
});

test('the IR also exports as a Qiskit program', options, async () => {
  const parsed = await quantum.parse(BELL);
  const python = await quantum.exportQiskitPython(parsed.circuit);
  // The registers and the gates of the fixture, in the shape `export.rs` emits:
  // the browser must not carry a second renderer of its own.
  assert.match(python, /^from qiskit import QuantumCircuit, QuantumRegister, ClassicalRegister$/m);
  assert.match(python, /^q = QuantumRegister\(2, "q"\)$/m);
  assert.match(python, /^c = ClassicalRegister\(2, "c"\)$/m);
  assert.match(python, /^circuit = QuantumCircuit\(q, c\)$/m);
  assert.match(python, /^circuit\.h\(q\[0\]\)$/m);
  assert.match(python, /^circuit\.cx\(q\[0\], q\[1\]\)$/m);
  assert.match(python, /measure/);
});

test('a rejected program comes back as a diagnostic, not an exception', options, async () => {
  const parsed = await quantum.parse('OPENQASM 3.0;\nqubit[2] q\n');
  assert.equal(parsed.status, 'rejected');
  const [first] = parsed.errors;
  assert.equal(typeof first.kind, 'string');
  assert.equal(typeof first.message, 'string');
  assert.deepEqual(rustSpelledKeys(parsed), []);
});

test('simulate reports counts, a state and camelCase field names', options, async () => {
  const parsed = await quantum.parse(BELL);
  const run = await quantum.simulate(parsed.circuit, { shots: 512, seed: 7 });
  assert.equal(run.method, 'stabilizer');
  assert.equal(run.isClifford, true);
  assert.equal(run.numQubits, 2);
  assert.equal(run.shots, 512);
  const total = Object.values(run.counts).reduce((sum, value) => sum + value, 0);
  assert.equal(total, 512);

  const unitary = await quantum.parse(CX);
  const state = await quantum.simulate(unitary.circuit, { state: true, probs: true });
  assert.equal(state.stateReason, null);
  assert.ok(state.state instanceof Float64Array);
  assert.equal(state.state.length, 8, 'two qubits is four amplitudes, eight numbers');
  assert.equal(state.probs.length, 4);

  // A measured circuit has no single final state; that is reported, not thrown.
  const measured = await quantum.simulate(parsed.circuit, { state: true, method: 'statevector' });
  assert.equal(measured.state, null);
  assert.equal(typeof measured.stateReason, 'string');
});

test('a keyframe is camelCase all the way down', options, async () => {
  const parsed = await quantum.parse(CX);
  const sim = await quantum.createSimulator(parsed.circuit, { seed: 7 });
  try {
    sim.step();
    const keyframe = sim.keyframe({ pairs: 'all', topK: 4, probsTop: 4 });
    assert.deepEqual(rustSpelledKeys(keyframe), []);
    assert.equal(keyframe.step, 1);
    assert.ok(Array.isArray(keyframe.probsTop));
    assert.equal(typeof keyframe.pairs[0].mutualInformation, 'number');
  } finally {
    sim.free();
  }
});

test('the stepper interpolates a bare cx without winding its phase', options, async () => {
  const parsed = await quantum.parse(CX);
  const sim = await quantum.createSimulator(parsed.circuit, { seed: 7 });
  try {
    assert.equal(sim.numQubits, 2);
    assert.equal(sim.stepCount, 1);
    // |00> sits in the +1 eigenspace of cx, so its amplitude must hold still
    // through the whole fraction rather than travel a full turn of phase.
    for (let tenth = 0; tenth <= 10; tenth += 1) {
      const state = sim.stepFraction(tenth / 10);
      assert.ok(Math.abs(state[0] - 1) < 1e-12, `Re(amp[0]) at t=${tenth / 10} is ${state[0]}`);
      assert.ok(Math.abs(state[1]) < 1e-12, `Im(amp[0]) at t=${tenth / 10} is ${state[1]}`);
    }
    assert.equal(sim.step(), true);
    assert.equal(sim.position, 1);
    const blochVectors = sim.blochVectors();
    assert.equal(blochVectors.length, 6, 'three components per qubit');
  } finally {
    sim.free();
    sim.free(); // freeing twice is part of the contract
  }
});

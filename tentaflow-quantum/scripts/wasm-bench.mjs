// ===== File: scripts/wasm-bench.mjs — spike B harness: browser-tier timings and native<->wasm parity =====
//
// Loads the wasm-bindgen glue that tentaflow-core/build.rs produces (target=web,
// the exact artefact the dashboard ships) and answers what plan 16 Faza 0 asks
// of spike B: how long does one Hadamard take on 20 and 24 qubits in wasm, does
// a full 24-qubit GHZ circuit run, and at what memory cost.
//
// It also checks the wasm build against tests/golden/wasm_parity.json, whose
// native half tests/wasm_parity.rs pins — the two together are the "bitowo
// zgodne z T0" criterion of plan 16, Faza 1.
//
// Driven by scripts/wasm-bench.sh, which builds the glue first. Node runs the
// same V8 wasm engine as Chrome, headlessly; the web glue would fetch its .wasm
// by URL, which Node cannot do for a file path, so the bytes are handed to the
// initialiser directly.

import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
import { resolve, join } from 'node:path';
import process from 'node:process';

const glueDir = process.argv[2];
if (!glueDir) {
  console.error('usage: node scripts/wasm-bench.mjs <dir with quantum_glue.js>');
  process.exit(2);
}

const gluePath = resolve(glueDir, 'quantum_glue.js');
const wasmPath = resolve(glueDir, 'quantum_glue_bg.wasm');
const fixturePath = resolve(join(import.meta.dirname, '..', 'tests', 'golden', 'wasm_parity.json'));

const quantum = await import(pathToFileURL(gluePath).href);
const wasmExports = await quantum.default({ module_or_path: readFileSync(wasmPath) });

const heapBytes = () => wasmExports.memory.buffer.byteLength;
const mib = (bytes) => (bytes / (1024 * 1024)).toFixed(1);

/** Median of `runs` timings of `fn`, in milliseconds. */
function median(runs, fn) {
  const samples = [];
  for (let i = 0; i < runs; i += 1) {
    const started = process.hrtime.bigint();
    fn();
    samples.push(Number(process.hrtime.bigint() - started) / 1e6);
  }
  samples.sort((a, b) => a - b);
  return samples[Math.floor(samples.length / 2)];
}

function ir(qasm) {
  const outcome = JSON.parse(quantum.parse(qasm));
  if (outcome.status !== 'parsed') {
    throw new Error(`circuit rejected: ${JSON.stringify(outcome.errors)}`);
  }
  return JSON.stringify(outcome.circuit);
}

const header = (qubits) => `OPENQASM 3.0;\ninclude "stdgates.inc";\nqubit[${qubits}] q;\n`;

/** GHZ: one Hadamard and n-1 CNOTs, i.e. n passes over 2^n amplitudes. */
function ghz(qubits) {
  let source = `${header(qubits)}h q[0];\n`;
  for (let q = 1; q < qubits; q += 1) source += `cx q[${q - 1}], q[${q}];\n`;
  return source;
}

// -----------------------------------------------------------------------------
// Parity: the wasm build must reproduce the counts the native build recorded
// -----------------------------------------------------------------------------

function sameCounts(got, want) {
  const gotKeys = Object.keys(got).sort();
  const wantKeys = Object.keys(want).sort();
  if (gotKeys.length !== wantKeys.length) return false;
  return gotKeys.every((key, index) => key === wantKeys[index] && got[key] === want[key]);
}

function checkParity() {
  const fixture = JSON.parse(readFileSync(fixturePath, 'utf8'));
  let failures = 0;
  for (const testCase of fixture.cases) {
    const result = quantum.simulate(ir(testCase.qasm), JSON.stringify({
      shots: testCase.shots,
      seed: testCase.seed,
      precision: testCase.precision,
      method: 'statevector',
    }));
    if (sameCounts(result.counts, testCase.counts)) {
      console.log(`  ok   ${testCase.name} (${testCase.shots} shots, seed ${testCase.seed}, ${testCase.precision})`);
    } else {
      failures += 1;
      console.error(`  FAIL ${testCase.name}`);
      console.error(`    wasm:   ${JSON.stringify(result.counts)}`);
      console.error(`    native: ${JSON.stringify(testCase.counts)}`);
    }
  }
  return failures;
}

// -----------------------------------------------------------------------------
// Spike B measurements
// -----------------------------------------------------------------------------

// The held Simulator does not fuse adjacent single-qubit gates (only `run` and
// `statevector` do), so a run of Hadamards on one qubit really is that many
// passes over the state — which is what makes the per-gate cost measurable.
const HADAMARD_REPEATS = 10;

function hadamard(qubits, precision) {
  let source = header(qubits);
  for (let i = 0; i < HADAMARD_REPEATS; i += 1) source += 'h q[0];\n';
  const sim = new quantum.Simulator(ir(source), JSON.stringify({ precision, maxQubits: qubits }));
  // `rewind` is itself a pass over the state, so it is measured and removed
  // instead of being charged to the gates.
  const whole = median(5, () => {
    sim.rewind();
    sim.runToEnd();
  });
  const rewind = median(5, () => sim.rewind());
  sim.free();
  return (whole - rewind) / HADAMARD_REPEATS;
}

function allocation(qubits, precision) {
  const program = ir(`${header(qubits)}h q[0];\n`);
  const options = JSON.stringify({ precision, maxQubits: qubits });
  let sim = null;
  const elapsed = median(3, () => {
    if (sim) sim.free();
    sim = new quantum.Simulator(program, options);
  });
  sim.free();
  return elapsed;
}

function ghzCircuit(qubits, precision) {
  const program = ir(ghz(qubits));
  const options = JSON.stringify({ precision, maxQubits: qubits, state: true });
  let amplitudes = 0;
  const elapsed = median(3, () => {
    const result = quantum.simulate(program, options);
    amplitudes = result.state.length / 2;
  });
  return { elapsed, amplitudes };
}

/**
 * The same GHZ circuit inside a held simulator: the gates are applied but the
 * state never crosses into JavaScript, which separates the cost of computing
 * from the cost of handing over 2^n amplitudes.
 */
function ghzStepped(qubits, precision) {
  const sim = new quantum.Simulator(ir(ghz(qubits)), JSON.stringify({ precision, maxQubits: qubits }));
  const whole = median(3, () => {
    sim.rewind();
    sim.runToEnd();
  });
  const rewind = median(3, () => sim.rewind());
  sim.rewind();
  sim.runToEnd();
  const keyframe = median(3, () => sim.keyframe(JSON.stringify({ pairs: 'gate', topK: 256, probsTop: 16 })));
  const histogram = median(3, () => sim.counts(4096));
  // The Bloch pass reads every amplitude, so its cost does not depend on the
  // state — measuring it on the rewound register keeps the sample clean. It is
  // cached per applied step, so each sample must invalidate the cache first;
  // `rewind` is the cheapest way to do that and is subtracted.
  const blochAll = median(3, () => {
    sim.rewind();
    sim.blochVectors();
  }) - rewind;
  const blochRow = median(3, () => {
    sim.rewind();
    for (let q = 0; q < qubits; q += 1) sim.bloch(q);
  }) - rewind;
  sim.free();
  return { circuit: whole - rewind, keyframe, histogram, blochAll, blochRow };
}

const QUBITS = [20, 22, 24];

console.log(`node ${process.version}, wasm heap at start ${mib(heapBytes())} MiB\n`);

console.log('parity with the native build (tests/golden/wasm_parity.json):');
const parityFailures = checkParity();

console.log(`\none Hadamard, median of ${HADAMARD_REPEATS} gates x 5 runs, rewind subtracted:`);
for (const precision of ['double', 'single']) {
  for (const qubits of QUBITS) {
    const gate = hadamard(qubits, precision);
    const allocate = allocation(qubits, precision);
    console.log(
      `  ${String(qubits).padStart(2)} q ${precision.padEnd(6)}` +
      ` gate ${gate.toFixed(2).padStart(8)} ms   allocate ${allocate.toFixed(2).padStart(8)} ms` +
      `   heap ${mib(heapBytes()).padStart(7)} MiB`,
    );
  }
}

console.log('\nGHZ, whole circuit through simulate() with the state vector returned:');
for (const precision of ['double', 'single']) {
  for (const qubits of QUBITS) {
    const { elapsed, amplitudes } = ghzCircuit(qubits, precision);
    console.log(
      `  ${String(qubits).padStart(2)} q ${precision.padEnd(6)} ${elapsed.toFixed(1).padStart(9)} ms` +
      `   ${amplitudes} amplitudes   heap ${mib(heapBytes()).padStart(7)} MiB`,
    );
  }
}

console.log('\nGHZ inside a held Simulator (no state handed to JavaScript), plus one keyframe,');
console.log('one 4096-shot live histogram, one Bloch pass and the whole per-qubit sphere row:');
for (const precision of ['double', 'single']) {
  for (const qubits of QUBITS) {
    const r = ghzStepped(qubits, precision);
    console.log(
      `  ${String(qubits).padStart(2)} q ${precision.padEnd(6)}` +
      ` circuit ${r.circuit.toFixed(1).padStart(8)} ms` +
      `   keyframe ${r.keyframe.toFixed(1).padStart(7)} ms` +
      `   histogram ${r.histogram.toFixed(1).padStart(7)} ms` +
      `   blochVectors ${r.blochAll.toFixed(1).padStart(7)} ms` +
      `   sphere row ${r.blochRow.toFixed(1).padStart(7)} ms`,
    );
  }
}

console.log(`\npeak wasm heap ${mib(heapBytes())} MiB`);

if (parityFailures > 0) {
  console.error(`\n${parityFailures} parity case(s) diverged from the native build`);
  process.exit(1);
}

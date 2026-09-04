// =============================================================================
// File: components/tf-mime-output.test.js
// Description: The three things <tf-mime-output> must never get wrong: which
// representation of a bundle it picks, what survives the HTML allowlist, and
// how the TentaQuant payloads (counts, state, circuit, traceback) turn into
// components — plus the collapse that keeps one long output from eating the
// notebook.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const { window } = await import('../sdk-runtime/_dom-test-harness.js');
if (typeof globalThis.ResizeObserver !== 'function') {
  globalThis.ResizeObserver = window.ResizeObserver
    || class { observe() {} unobserve() {} disconnect() {} };
}
if (typeof globalThis.MutationObserver !== 'function' && window.MutationObserver) {
  globalThis.MutationObserver = window.MutationObserver;
}

const {
  pickMimeType, sanitizeHtml, sanitizeStyle, phaseColor, amplitudeRows, MIME_PREFERENCE,
  COUNTS_MIME, STATE_MIME, CIRCUIT_MIME, TRACEBACK_MIME, TfMimeOutput,
} = await import('./tf-mime-output.js');

// ---- dispatch --------------------------------------------------------------

test('the richest representation of a bundle wins', () => {
  assert.equal(pickMimeType({ 'text/plain': 'x', 'text/html': '<b>x</b>' }), 'text/html');
  assert.equal(pickMimeType({ 'text/plain': 'x', [COUNTS_MIME]: {} }), COUNTS_MIME);
  assert.equal(pickMimeType({ 'text/plain': 'x' }), 'text/plain');
});

test('a null part is not a representation', () => {
  assert.equal(pickMimeType({ 'text/html': null, 'text/plain': 'x' }), 'text/plain');
  assert.equal(pickMimeType({}), null);
  assert.equal(pickMimeType(null), null);
});

test('preferred types are tried before the built-in order', () => {
  const bundle = { 'text/plain': 'x', 'text/html': '<b>x</b>' };
  assert.equal(pickMimeType(bundle, ['text/plain']), 'text/plain');
  assert.ok(MIME_PREFERENCE.indexOf('text/html') < MIME_PREFERENCE.indexOf('text/plain'));
});

// ---- sanitiser -------------------------------------------------------------

function sanitizedHtml(source) {
  const holder = document.createElement('div');
  holder.appendChild(sanitizeHtml(source));
  return holder.innerHTML;
}

test('allowed markup survives the allowlist intact', () => {
  const html = sanitizedHtml('<p title="lead">a <strong>b</strong> <code>c</code></p>');
  assert.match(html, /<p title="lead">/);
  assert.match(html, /<strong>b<\/strong>/);
  assert.match(html, /<code>c<\/code>/);
});

test('a class cannot be borrowed from the dashboard by kernel output', () => {
  const html = sanitizedHtml('<p class="tf-modal__backdrop">x</p>');
  assert.ok(!html.includes('class'));
  assert.match(html, />x</);
});

test('a script is dropped with its content, not unwrapped into text', () => {
  const html = sanitizedHtml('<p>before</p><script>steal()</script><p>after</p>');
  assert.ok(!html.includes('script'));
  assert.ok(!html.includes('steal'));
  assert.match(html, /before/);
  assert.match(html, /after/);
});

test('event handler attributes never reach the document', () => {
  const html = sanitizedHtml('<p onclick="steal()" onmouseover="x()">hi</p>');
  assert.ok(!html.includes('onclick'));
  assert.ok(!html.includes('onmouseover'));
  assert.match(html, />hi</);
});

test('a link keeps an http href and loses a javascript one', () => {
  const safe = sanitizedHtml('<a href="https://example.org/x">go</a>');
  assert.match(safe, /href="https:\/\/example\.org\/x"/);
  assert.match(safe, /rel="noopener noreferrer nofollow"/);
  const unsafe = sanitizedHtml('<a href="javascript:steal()">go</a>');
  assert.ok(!unsafe.includes('href'));
  assert.match(unsafe, />go</);
});

test('an image may be a data URI or https, and nothing else', () => {
  assert.match(sanitizedHtml('<img src="data:image/png;base64,AAA" alt="p">'), /src="data:image\/png/);
  assert.ok(!sanitizedHtml('<img src="file:///etc/passwd">').includes('src'));
});

test('inline style is a property allowlist, not a denylist of known tricks', () => {
  assert.match(sanitizedHtml('<span style="color:red">x</span>'), /style="color:red"/);
  assert.ok(!sanitizedHtml('<span style="background:url(http://x/y)">x</span>').includes('style'));
  assert.ok(!sanitizedHtml('<span style="@import url(x)">x</span>').includes('style'));
  // The escape this replaced: an in-flow-looking declaration set that covers
  // the whole dashboard, using nothing a URL denylist would notice.
  const cover = sanitizedHtml('<span style="position:fixed;inset:0;z-index:9999;background:#fff">x</span>');
  assert.ok(!cover.includes('style'), cover);
});

test('a mixed declaration set keeps the decorative half and drops the rest', () => {
  assert.equal(
    sanitizeStyle('color: red; position: fixed; PADDING:2px; transform: scale(90)'),
    'color:red;padding:2px',
  );
  assert.equal(sanitizeStyle('color: red\\2f *'), '', 'a CSS escape voids the declaration');
  assert.equal(sanitizeStyle('nonsense'), '');
  assert.equal(sanitizeStyle(null), '');
});

test('an unknown but harmless element is unwrapped so its text survives', () => {
  const html = sanitizedHtml('<article><p>kept</p></article>');
  assert.ok(!html.includes('article'));
  assert.match(html, /<p>kept<\/p>/);
});

test('an iframe is not a harmless wrapper', () => {
  const html = sanitizedHtml('<iframe src="https://evil.example"><p>x</p></iframe>');
  assert.ok(!html.includes('iframe'));
  assert.ok(!html.includes('evil.example'));
});

// ---- amplitudes ------------------------------------------------------------

test('phaseColor walks the wheel once per turn of phase', () => {
  assert.equal(phaseColor(0), 'hsl(250.0 80% 66%)');
  assert.equal(phaseColor(Math.PI), 'hsl(70.0 80% 66%)');
  assert.equal(phaseColor(2 * Math.PI), 'hsl(250.0 80% 66%)');
});

test('amplitudeRows reads the flat interleaved vector and drops the zeros', () => {
  // (|00> + |11>)/sqrt(2)
  const amplitude = Math.SQRT1_2;
  const rows = amplitudeRows({ amplitudes: [amplitude, 0, 0, 0, 0, 0, amplitude, 0] }, 2);
  assert.equal(rows.length, 2);
  assert.deepEqual(rows.map((row) => row.key), ['00', '11']);
  assert.ok(Math.abs(rows[0].probability - 0.5) < 1e-12);
  assert.equal(rows[0].phase, 0);
});

// `top` entries are AmplitudeGroup objects — `{index, amplitude: [re, im],
// partners}` — because num-complex serialises a complex as a two-element tuple.
test('amplitudeRows also reads the sparse top-k of a keyframe, biggest first', () => {
  const rows = amplitudeRows({
    top: [
      { index: 3, amplitude: [0.2, 0], partners: [] },
      { index: 0, amplitude: [0.9, 0], partners: [] },
    ],
  }, 2);
  assert.deepEqual(rows.map((row) => row.key), ['00', '11']);
  assert.ok(rows[0].probability > rows[1].probability);
});

test('a negative amplitude carries a phase of pi', () => {
  const [row] = amplitudeRows({ top: [{ index: 0, amplitude: [-1, 0], partners: [] }] }, 1);
  assert.ok(Math.abs(Math.abs(row.phase) - Math.PI) < 1e-12);
});

// ---- the element -----------------------------------------------------------

/// Waits for something a dynamic import inside a component produces.
async function until(predicate, what) {
  for (let attempt = 0; attempt < 200; attempt += 1) {
    const value = predicate();
    if (value) return value;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  throw new Error(`timed out waiting for ${what}`);
}

function mount(bundle, attributes = {}) {
  const el = new TfMimeOutput();
  for (const [name, value] of Object.entries(attributes)) el.setAttribute(name, value);
  document.body.appendChild(el);
  el.bundle = bundle;
  return el;
}

test('an empty bundle says so instead of rendering nothing', () => {
  const el = mount({});
  assert.ok(el.querySelector('.tf-mime__empty'));
  el.remove();
});

test('plain text is escaped into a <pre>, never parsed', () => {
  const el = mount({ 'text/plain': '<b>not bold</b>' });
  const pre = el.querySelector('.tf-mime__text');
  assert.equal(pre.textContent, '<b>not bold</b>');
  assert.equal(pre.querySelector('b'), null);
  el.remove();
});

test('a long output collapses and the button opens it for good', () => {
  const long = Array.from({ length: 120 }, (_, i) => `line ${i}`).join('\n');
  const el = mount({ 'text/plain': long }, { 'max-lines': '10' });
  assert.ok(el.querySelector('.tf-mime__body--clamped'), 'clamped while collapsed');
  let expanded = null;
  el.addEventListener('expand', (event) => { expanded = event.detail.mime; });
  el.querySelector('.tf-mime__more').dispatchEvent(new window.Event('click', { bubbles: true }));
  assert.equal(expanded, 'text/plain');
  assert.equal(el.querySelector('.tf-mime__body--clamped'), null);
  assert.equal(el.querySelector('.tf-mime__more'), null);
  el.remove();
});

test('a short output is neither clamped nor given a button', () => {
  const el = mount({ 'text/plain': 'one line' });
  assert.equal(el.querySelector('.tf-mime__body--clamped'), null);
  assert.equal(el.querySelector('.tf-mime__more'), null);
  el.remove();
});

test('counts become one histogram series per named run', () => {
  const el = mount({
    [COUNTS_MIME]: {
      shots: 1024,
      series: [
        { id: 'ideal', name: 'ideal', counts: { '00': 512, '11': 512 } },
        { id: 'qpu', name: 'ibm_torino', counts: { '00': 480, '01': 20, '11': 524 } },
      ],
    },
  });
  const chart = el.querySelector('tf-bar-chart');
  assert.ok(chart);
  assert.equal(chart._series.length, 2);
  // Every series is projected onto the union of the bitstrings, so the bars line up.
  assert.deepEqual(chart._series[0].points.map((point) => point.x), ['00', '01', '11']);
  assert.deepEqual(chart._series[0].points.map((point) => point.y), [512, 0, 512]);
  assert.match(el.querySelector('.tf-mime__foot').textContent, /1024 shots/);
  el.remove();
});

test('a bare counts map is drawn as a single series', () => {
  const el = mount({ [COUNTS_MIME]: { counts: { '0': 3, '1': 7 } } });
  assert.equal(el.querySelector('tf-bar-chart')._series.length, 1);
  el.remove();
});

test('a state output pairs a Bloch row with the amplitude table', () => {
  const amplitude = Math.SQRT1_2;
  const el = mount({
    [STATE_MIME]: {
      numQubits: 2,
      bloch: new Float64Array([0, 0, 0, 0, 0, 0]),
      amplitudes: [amplitude, 0, 0, 0, 0, 0, amplitude, 0],
    },
  });
  const spheres = el.querySelectorAll('tf-bloch-sphere');
  assert.equal(spheres.length, 2);
  assert.equal(spheres[0].getAttribute('label'), 'q0');
  assert.equal(spheres[0].entangled, true, 'a maximally mixed qubit is chipped');
  const rows = el.querySelectorAll('.tf-mime__amps tr');
  assert.equal(rows.length, 3, 'a header plus the two non-zero amplitudes');
  assert.match(rows[1].textContent, /\|00⟩/);
  el.remove();
});

test('the amplitude table truncates until the reader asks for the rest', () => {
  const top = Array.from({ length: 40 },
    (_, i) => ({ index: i, amplitude: [1 / Math.sqrt(40), 0], partners: [] }));
  const el = mount({ [STATE_MIME]: { numQubits: 6, top } }, { 'max-rows': '8' });
  assert.equal(el.querySelectorAll('.tf-mime__amps tr').length, 9);
  el.querySelector('.tf-mime__more').dispatchEvent(new window.Event('click', { bubbles: true }));
  assert.equal(el.querySelectorAll('.tf-mime__amps tr').length, 41);
  el.remove();
});

test('a circuit output is a read-only editor with no palette', () => {
  const circuit = {
    qubitRegisters: [{ name: 'q', start: 0, size: 2 }],
    clbitRegisters: [],
    numQubits: 2,
    numClbits: 0,
    ops: [{ kind: { Gate: { gate: 'H', qubits: [0] } }, conditions: [] }],
  };
  const el = mount({ [CIRCUIT_MIME]: { circuit } });
  const editor = el.querySelector('tf-quantum-circuit');
  assert.ok(editor.hasAttribute('readonly'));
  assert.equal(editor.getAttribute('palette'), 'none');
  assert.equal(editor.circuit.ops.length, 1);
  el.remove();
});

test('a traceback is coloured line by line, with the exception standing out', () => {
  const el = mount({
    [TRACEBACK_MIME]: [
      'Traceback (most recent call last):',
      '  File "cell.py", line 3, in <module>',
      '    run(circuit)',
      'ValueError: 25 qubits exceeds max_qubits',
    ],
  });
  const kinds = Array.from(el.querySelectorAll('.tf-mime__tb'))
    .map((line) => line.className.split('--')[1]);
  assert.deepEqual(kinds, ['head', 'file', 'plain', 'error']);
  el.remove();
});

test('json becomes a tree whose root is open and whose keys are paths', () => {
  const el = mount({ 'application/json': { shots: 1024, counts: { '00': 512 } } });
  const tree = el.querySelector('tf-tree');
  assert.ok(tree);
  assert.deepEqual(Array.from(tree.expandedIds), ['$']);
  const [root] = tree.nodes;
  assert.equal(root.id, '$');
  assert.equal(root.label, '$ {2}');
  assert.deepEqual(root.children.map((child) => child.id), ['$.shots', '$.counts']);
  assert.equal(root.children[0].label, 'shots: 1024');
  assert.equal(root.children[1].children[0].label, '00: 512');
  el.remove();
});

test('an image is served as a data URI whether or not the bundle already is one', () => {
  const bare = mount({ 'image/png': 'AAAA' });
  assert.equal(bare.querySelector('img').getAttribute('src'), 'data:image/png;base64,AAAA');
  bare.remove();
  const full = mount({ 'image/png': 'data:image/png;base64,BBBB' });
  assert.equal(full.querySelector('img').getAttribute('src'), 'data:image/png;base64,BBBB');
  full.remove();
});

test('markdown shows its source until the renderer arrives, then renders it', async () => {
  // The renderer is imported behind an absolute dashboard path, so the output
  // exists for at least one turn without it: the source itself is a correct
  // rendering of markdown and a renderer that never loads (or rejects, which
  // node:test would otherwise see as an unhandled rejection) leaves it there.
  const el = mount({ 'text/markdown': '# Title\n\nbody' });
  const holder = el.querySelector('.tf-mime__markdown');
  assert.ok(holder);
  assert.equal(holder.textContent, '# Title\n\nbody');
  // Polling, not a fixed delay: the first load of a module graph takes as long
  // as it takes, and a hard-coded wait turns this contract into a coin toss.
  const heading = await until(() => holder.querySelector('h1'), 'the rendered markdown');
  assert.equal(heading.textContent, 'Title');
  el.remove();
});

// ---- the real producer -----------------------------------------------------
// The state renderer's only in-repo producer is QuantumSimulator.keyframe(),
// whose `bloch` is NESTED (`Vec<[f64; 3]>`) and whose `top` is a list of
// AmplitudeGroup objects. Hand-written fixtures cannot catch a mismatch with
// that serde shape, so this drives the real wasm simulator; the artefacts are
// generated, not committed, so an unbuilt tree skips with the reason.

const gluePath = fileURLToPath(new URL('../quantum/quantum_glue.js', import.meta.url));
const wasmPath = fileURLToPath(new URL('../quantum/quantum_glue_bg.wasm', import.meta.url));
const missingGlue = [gluePath, wasmPath].filter((path) => !existsSync(path));
const quantumOptions = missingGlue.length
  ? { skip: `the wasm glue is not built (${missingGlue.join(', ')})` }
  : {};

test('a keyframe straight from the simulator draws its spheres and its amplitudes',
  quantumOptions, async () => {
    const { parse, createSimulator } = await import('../quantum/index.js');
    const parsed = await parse('OPENQASM 3.0;\ninclude "stdgates.inc";\nqubit[2] q;\n'
      + 'h q[0];\ncx q[0], q[1];\n');
    assert.equal(parsed.status, 'parsed');
    const sim = await createSimulator(parsed.circuit);
    try {
      sim.runToEnd();
      const keyframe = sim.keyframe({ pairs: 'gate', topK: 8, probsTop: 4 });
      assert.ok(Array.isArray(keyframe.bloch[0]), 'the keyframe nests its Bloch vectors');
      assert.equal(typeof keyframe.top[0].index, 'number');

      const el = mount({ [STATE_MIME]: keyframe });
      const spheres = el.querySelectorAll('tf-bloch-sphere');
      assert.equal(spheres.length, 2, 'one sphere per qubit of the register');
      assert.ok(spheres[0].vector.every(Number.isFinite));
      assert.equal(spheres[0].entangled, true, 'a Bell pair reads as entangled');
      const rows = el.querySelectorAll('.tf-mime__amps tr');
      assert.equal(rows.length, 3, 'a header plus |00> and |11>');
      assert.match(rows[1].textContent, /\|(00|11)⟩/);
      el.remove();
    } finally {
      sim.free();
    }
  });

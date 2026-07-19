// =============================================================================
// File: sdk-runtime/specialized-audio-capture-renderer.test.js
// Description: Tests for AudioCapture (0x0612) — field parsing, mode classes,
// reactive disabled, and a getUserMedia-mocked start.
// =============================================================================

import './_dom-test-harness.js';

// The renderer imports ApiBinary → codec.js, whose top-level `codecReady`
// eagerly initializes the protocol WASM via fetch(). In Node there is no
// fetch target, so that promise rejects. It is unrelated to AudioCapture logic
// (no upload runs in these tests), so we ignore exactly that rejection.
if (typeof process !== 'undefined') {
  process.on('unhandledRejection', (err) => {
    const msg = String((err && err.message) || err);
    if (msg.includes('fetch failed') || msg.includes('not implemented')) return;
    throw err;
  });
}

import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { AUDIO_CAPTURE_TAG } from './specialized-audio-capture-renderer.js';

const CASES = [];
function test(name, fn) { CASES.push({ name, fn }); }
function assert(cond, m) { if (!cond) throw new Error(m || 'assert failed'); }
function assertEq(a, e, m) {
  if (JSON.stringify(a) !== JSON.stringify(e)) {
    throw new Error(`${m || 'assertEq'}: expected ${JSON.stringify(e)}, got ${JSON.stringify(a)}`);
  }
}
function assertThrows(fn, m) {
  let t = false; try { fn(); } catch { t = true; }
  if (!t) throw new Error(m || 'expected throw');
}

const PATH = (...segs) => segs.map((s) =>
  typeof s === 'number' ? { kind: 'index', value: s } : { kind: 'key', value: s });

function makeStore() { return new StateStore({ addon_id: 'aud-addon', panel_id: 'p', panel_epoch: 1n }); }
function makeEngine(store, dispatcher) {
  return new ComponentRenderer({
    store: store || makeStore(),
    eventDispatcher: dispatcher || { emit() {} },
    locale: 'en-US',
  });
}
function comp(tag, fields, extra = {}) {
  return {
    tag, id: extra.id ?? 'ac1', fields,
    handlers: extra.handlers ?? null,
    bind: extra.bind ?? null,
    a11y: extra.a11y ?? null,
    visibility: extra.visibility ?? null,
    test_id: extra.test_id ?? null,
  };
}
function setup() {
  _clearComponentRendererRegistry();
  bootstrapSdkRuntime();
  document.body.innerHTML = '';
}
function mount(el) { document.body.appendChild(el); return el; }

// ============================================================================
// Field parsing + rendering
// ============================================================================

test('AudioCapture renders a button with the mode class (push_to_talk)', () => {
  setup();
  const el = mount(makeEngine().render(comp(AUDIO_CAPTURE_TAG, [
    [0, 'onUtterance'], [1, 'push_to_talk'],
  ])));
  assert(el.classList.contains('tf-audio-capture--push_to_talk'), 'mode class');
  const btn = el.querySelector('.tf-audio-capture__button');
  assert(btn != null, 'button exists');
  // The label sits below the mic (a wrapper child, not inside the button) so it
  // can render under the concentric pulse rings.
  assertEq(el.querySelector('.tf-audio-capture__label').textContent, 'Przytrzymaj, aby mówić');
});

test('AudioCapture vad mode uses the toggle label', () => {
  setup();
  const el = mount(makeEngine().render(comp(AUDIO_CAPTURE_TAG, [
    [0, 'onUtterance'], [1, 'vad'],
  ])));
  assert(el.classList.contains('tf-audio-capture--vad'), 'mode class');
  assertEq(el.querySelector('.tf-audio-capture__label').textContent, 'Rozpocznij nagrywanie');
});

test('AudioCapture missing action_id throws', () => {
  setup();
  assertThrows(() => makeEngine().render(comp(AUDIO_CAPTURE_TAG, [[1, 'vad']])), 'action_id required');
});

test('AudioCapture unknown field key throws', () => {
  setup();
  assertThrows(() => makeEngine().render(comp(AUDIO_CAPTURE_TAG, [
    [0, 'a'], [1, 'vad'], [9, 'nope'],
  ])), 'unknown field rejected');
});

test('AudioCapture docked variant adds the variant class', () => {
  setup();
  const el = mount(makeEngine().render(comp(AUDIO_CAPTURE_TAG, [
    [0, 'a'], [1, 'vad'], [8, 'docked'],
  ])));
  assert(el.classList.contains('tf-audio-capture--docked'), 'variant class');
});

test('AudioCapture invalid variant throws', () => {
  setup();
  assertThrows(() => makeEngine().render(comp(AUDIO_CAPTURE_TAG, [
    [0, 'a'], [1, 'vad'], [8, 'floating'],
  ])), 'variant enum enforced');
});

test('AudioCapture invalid mode throws', () => {
  setup();
  assertThrows(() => makeEngine().render(comp(AUDIO_CAPTURE_TAG, [
    [0, 'a'], [1, 'always_on'],
  ])), 'mode enum enforced');
});

test('AudioCapture reactive disabled BindRef disables the button', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('locked'), value: true }],
    state_revision: 0, truncated: false,
  });
  const el = mount(makeEngine(store).render(comp(AUDIO_CAPTURE_TAG, [
    [0, 'a'], [1, 'vad'],
    [6, { kind: 'bound', path: PATH('locked') }],
  ])));
  assertEq(el.querySelector('.tf-audio-capture__button').disabled, true);
});

// ============================================================================
// Microphone capture (getUserMedia mocked)
// ============================================================================

// Minimal AudioContext + AudioWorklet mock sufficient for openMicSource.
function installAudioMocks() {
  const saved = {
    navigator: Object.getOwnPropertyDescriptor(globalThis, 'navigator'),
    AudioContext: globalThis.AudioContext,
    AudioWorkletNode: globalThis.AudioWorkletNode,
    Blob: globalThis.Blob,
    URL: globalThis.URL,
    performance: globalThis.performance,
    windowAudioContext: globalThis.window ? globalThis.window.AudioContext : undefined,
  };
  let getUserMediaCalls = 0;
  const track = { stop() {} };
  const stream = { getTracks: () => [track], getAudioTracks: () => [track] };
  // `navigator` is a read-only getter global in modern Node — override with a
  // configurable data property so the module's bare `navigator` reference hits
  // our mock, then restore the original descriptor afterwards.
  Object.defineProperty(globalThis, 'navigator', {
    value: { mediaDevices: { getUserMedia: async () => { getUserMediaCalls += 1; return stream; } } },
    configurable: true,
    writable: true,
  });
  const node = () => ({ connect: (n) => n, disconnect() {}, port: {} });
  class AudioContextMock {
    constructor() { this.sampleRate = 48000; this.destination = {}; this.audioWorklet = { addModule: async () => {} }; }
    createMediaStreamSource() { return node(); }
    createAnalyser() { return { fftSize: 1024, smoothingTimeConstant: 0.6, connect: (n) => n, disconnect() {}, getFloatTimeDomainData() {} }; }
    createGain() { return { gain: { value: 0 }, connect: (n) => n, disconnect() {} }; }
    close() {}
  }
  globalThis.AudioContext = AudioContextMock;
  if (globalThis.window) globalThis.window.AudioContext = AudioContextMock;
  let lastWorklet = null;
  globalThis.AudioWorkletNode = class {
    constructor() { this.port = { onmessage: null }; lastWorklet = this; }
    connect(n) { return n; }
    disconnect() {}
  };
  globalThis.Blob = class { constructor() {} };
  globalThis.URL = { createObjectURL: () => 'blob:x', revokeObjectURL() {} };
  if (!globalThis.performance) globalThis.performance = { now: () => 0 };
  return {
    getCalls: () => getUserMediaCalls,
    lastWorklet: () => lastWorklet,
    restore: () => {
      if (saved.navigator) Object.defineProperty(globalThis, 'navigator', saved.navigator);
      else delete globalThis.navigator;
      globalThis.AudioContext = saved.AudioContext;
      globalThis.AudioWorkletNode = saved.AudioWorkletNode;
      globalThis.Blob = saved.Blob;
      globalThis.URL = saved.URL;
      globalThis.performance = saved.performance;
      if (globalThis.window) globalThis.window.AudioContext = saved.windowAudioContext;
    },
  };
}

test('AudioCapture push_to_talk pointerdown opens the mic (getUserMedia)', async () => {
  setup();
  const mocks = installAudioMocks();
  try {
    const el = mount(makeEngine().render(comp(AUDIO_CAPTURE_TAG, [
      [0, 'onUtterance'], [1, 'push_to_talk'],
    ])));
    const btn = el.querySelector('.tf-audio-capture__button');
    btn.dispatchEvent(new (globalThis.Event)('pointerdown', { bubbles: true, cancelable: true }));
    // startMic awaits getUserMedia + addModule — let the async chain settle.
    await new Promise((r) => setTimeout(r, 10));
    assert(mocks.getCalls() >= 1, 'getUserMedia must be requested on pointerdown');
    assert(btn.classList.contains('tf-audio-capture__button--recording'), 'recording state armed');
  } finally {
    mocks.restore();
  }
});

test('AudioCapture disabled button ignores pointerdown (no getUserMedia)', async () => {
  setup();
  const mocks = installAudioMocks();
  try {
    const store = makeStore();
    store.applySnapshot({
      entries: [{ path: PATH('locked'), value: true }],
      state_revision: 0, truncated: false,
    });
    const el = mount(makeEngine(store).render(comp(AUDIO_CAPTURE_TAG, [
      [0, 'a'], [1, 'push_to_talk'],
      [6, { kind: 'bound', path: PATH('locked') }],
    ])));
    const btn = el.querySelector('.tf-audio-capture__button');
    btn.dispatchEvent(new (globalThis.Event)('pointerdown', { bubbles: true, cancelable: true }));
    await Promise.resolve();
    assertEq(mocks.getCalls(), 0, 'disabled control must not request the mic');
  } finally {
    mocks.restore();
  }
});

test('AudioCapture active_path=true arms VAD listening; false stops it', async () => {
  setup();
  const mocks = installAudioMocks();
  try {
    const store = makeStore();
    store.applySnapshot({
      entries: [{ path: PATH('dic', 'active'), value: false }],
      state_revision: 0, truncated: false,
    });
    const el = mount(makeEngine(store).render(comp(AUDIO_CAPTURE_TAG, [
      [0, 'a'], [1, 'vad'],
      [7, PATH('dic', 'active')],
    ])));
    const label = el.querySelector('.tf-audio-capture__label');
    assertEq(label.textContent, 'Rozpocznij nagrywanie');
    store.applyOverlay([{ path: PATH('dic', 'active'), value: true }]);
    await new Promise((r) => setTimeout(r, 10));
    assert(mocks.getCalls() >= 1, 'resume must open the mic');
    assertEq(label.textContent, 'Zatrzymaj nagrywanie');
    store.applyOverlay([{ path: PATH('dic', 'active'), value: false }]);
    await new Promise((r) => setTimeout(r, 10));
    assertEq(label.textContent, 'Rozpocznij nagrywanie', 'pause must stop listening');
  } finally {
    mocks.restore();
  }
});

test('AudioCapture user VAD toggle publishes active_path back to the store', async () => {
  setup();
  const mocks = installAudioMocks();
  try {
    const store = makeStore();
    store.applySnapshot({
      entries: [{ path: PATH('dic', 'active'), value: false }],
      state_revision: 0, truncated: false,
    });
    const el = mount(makeEngine(store).render(comp(AUDIO_CAPTURE_TAG, [
      [0, 'a'], [1, 'vad'],
      [7, PATH('dic', 'active')],
    ])));
    const btn = el.querySelector('.tf-audio-capture__button');
    btn.dispatchEvent(new (globalThis.Event)('pointerdown', { bubbles: true, cancelable: true }));
    await new Promise((r) => setTimeout(r, 10));
    assertEq(store.read(PATH('dic', 'active')), true, 'toggle-on mirrored');
    btn.dispatchEvent(new (globalThis.Event)('pointerdown', { bubbles: true, cancelable: true }));
    await new Promise((r) => setTimeout(r, 10));
    assertEq(store.read(PATH('dic', 'active')), false, 'toggle-off mirrored');
  } finally {
    mocks.restore();
  }
});

test('AudioCapture utterances carry seq and deliver strictly in order', async () => {
  setup();
  const mocks = installAudioMocks();
  const { ApiBinary } = await import('../protocol/api-binary-shim.js');
  const origOne = ApiBinary.one;
  const emitted = [];
  let firstResolve = null;
  let uploadCalls = 0;
  // First upload hangs until released — the second utterance is already
  // captured by then, so ordering is only correct if deliveries are chained.
  ApiBinary.one = () => {
    uploadCalls += 1;
    if (uploadCalls === 1) {
      return new Promise((res) => { firstResolve = () => res({ docRef: 'doc-first' }); });
    }
    return Promise.resolve({ docRef: `doc-${uploadCalls}` });
  };
  try {
    const dispatcher = { emit: (e) => emitted.push(e.dom_event.detail) };
    const el = mount(makeEngine(makeStore(), dispatcher).render(comp(AUDIO_CAPTURE_TAG, [
      [0, 'onUtterance'], [1, 'push_to_talk'],
    ])));
    const btn = el.querySelector('.tf-audio-capture__button');
    const speak = async () => {
      btn.dispatchEvent(new (globalThis.Event)('pointerdown', { bubbles: true, cancelable: true }));
      await new Promise((r) => setTimeout(r, 10));
      // 1 s of audio at the mocked 48 kHz rate (over min_speech_ms).
      mocks.lastWorklet().port.onmessage({ data: new Float32Array(48000) });
      btn.dispatchEvent(new (globalThis.Event)('pointerup', { bubbles: true, cancelable: true }));
      await new Promise((r) => setTimeout(r, 10));
    };
    await speak();
    await speak();
    // The first upload is still pending — utterance 2 must NOT overtake it.
    assertEq(emitted.length, 0, 'second delivery must wait for the first');
    assertEq(uploadCalls, 1, 'uploads are serialized');
    firstResolve();
    await new Promise((r) => setTimeout(r, 20));
    assertEq(emitted.length, 2, 'both utterances delivered');
    assertEq(emitted[0].doc_ref, 'doc-first');
    assertEq(emitted[0].seq, 0);
    assertEq(emitted[1].seq, 1, 'seq is monotonic');
  } finally {
    ApiBinary.one = origOne;
    mocks.restore();
  }
});

// ---- sequential runner ----
const _run = (async () => {
  let pass = 0, fail = 0;
  const lines = [];
  for (const c of CASES) {
    try {
      await c.fn();
      pass++; lines.push(`✓ ${c.name}`);
    } catch (err) {
      fail++; lines.push(`✗ ${c.name}\n    ${err && err.stack ? err.stack : err}`);
    }
  }
  lines.push('');
  lines.push(`${pass}/${pass + fail} tests passed${fail ? ` — ${fail} FAILED` : ''}`);
  console.log(lines.join('\n'));
  if (typeof process !== 'undefined') process.exit(fail > 0 ? 1 : 0);
})();

export { _run };

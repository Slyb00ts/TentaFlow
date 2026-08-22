// =============================================================================
// File: modules/catalog/deploy-progress-modal.test.js
// Description: What the deploy progress window promises while a deploy runs.
//       The window used to render the DB replay and then freeze: any terminal
//       frame that was not a DeploymentStreamEnd (the `StreamClosed` the WS
//       client synthesizes when the socket dies, the server's bodyless end when
//       the log bus is gone) was silently discarded and nothing ever
//       resubscribed, so a live docker build streamed into a dead listener.
//       Pinned here: live append, replay swap without duplicates, resubscribe
//       across a reconnect, a verdict that survives a disconnect, the bounded
//       buffer and ANSI/CR scrubbing.
//       The module imports the wasm codec transitively, so its source is
//       evaluated with injected fakes — the code tested here is the code that
//       ships.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
// utils.js has no imports of its own, so the real escaper can be used to prove
// that switching the log box to innerHTML did not open a hole.
import { escapeHtml as realEscapeHtml } from '../../utils.js';

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, 'deploy-progress-modal.js'), 'utf8')
  .replace(/^import[\s\S]*?;$/gm, '')
  .replace(/^export /gm, '');

const makeModule = new Function(
  'ApiBinary',
  'I18n',
  'escapeHtml',
  'toast',
  'document',
  'requestAnimationFrame',
  `${source}\nreturn { openDeployProgressModal, cleanLogLine };`,
);

// --------------------------------------------------------------------- fakes

/**
 * Minimal element. `innerHTML` is not parsed — the module only ever looks up
 * `[data-*]` hooks, so every data attribute in the markup becomes one stub
 * child reachable by that exact selector.
 */
class FakeEl {
  constructor(tag) {
    this.tagName = tag;
    this.attributes = new Map();
    this.children = [];
    this.listeners = new Map();
    this.textContent = '';
    this._html = '';
    this.hidden = false;
    this.style = {};
    this.scrollTop = 0;
    this.scrollHeight = 0;
    this.clientHeight = 0;
    this._hooks = new Map();
    this.closedForce = null;
  }

  set innerHTML(html) {
    this._html = String(html);
    this._hooks = new Map();
    for (const match of String(html).matchAll(/data-[a-z-]+/g)) {
      this._hooks.set(`[${match[0]}]`, new FakeEl('div'));
    }
  }

  get innerHTML() {
    return this._html;
  }

  querySelector(selector) {
    return this._hooks.get(selector) ?? null;
  }

  appendChild(child) {
    this.children.push(child);
    return child;
  }

  setAttribute(name, value) {
    this.attributes.set(name, String(value));
  }

  removeAttribute(name) {
    this.attributes.delete(name);
  }

  addEventListener(type, fn) {
    if (!this.listeners.has(type)) this.listeners.set(type, []);
    this.listeners.get(type).push(fn);
  }

  emit(type, detail) {
    for (const fn of this.listeners.get(type) ?? []) fn(detail);
  }

  close(force) {
    this.closedForce = force;
  }
}

function stripTags(html) {
  return String(html).replace(/<[^>]*>/g, '');
}

function chunk(deployId, line, kind = 'log', { phase = '', progressPct = 0 } = {}) {
  return {
    variant: 'DeploymentStreamChunk',
    deployId,
    kind,
    line,
    phase,
    progressPct,
    tsMs: 0,
  };
}

/**
 * One window under test plus the handles a test needs to drive it: the fake
 * transport's captured stream handlers, the lifecycle emitter and the log box.
 */
function mount({ deployId = 'dep-1', summaries = [], engineId = 'teams-bot', escapeHtml = (v) => String(v) } = {}) {
  const streams = [];
  const lifecycle = [];
  const toasts = [];
  const statusCalls = [];
  let summaryIndex = 0;

  const ApiBinary = {
    async one(kind, payload) {
      assert.equal(kind, 'deploymentStatusRequest');
      statusCalls.push(payload);
      const summary = summaries[Math.min(summaryIndex, summaries.length - 1)];
      summaryIndex += 1;
      return { deployment: summary };
    },
    async subscribe(kind, payload, handlers) {
      assert.equal(kind, 'deploymentLogStreamRequest');
      assert.equal(payload.replayTail, true);
      const entry = { payload, handlers, closed: false };
      entry.close = () => { entry.closed = true; };
      streams.push(entry);
      return entry.close;
    },
    onLifecycle(cb) {
      lifecycle.push(cb);
      return () => {
        const idx = lifecycle.indexOf(cb);
        if (idx >= 0) lifecycle.splice(idx, 1);
      };
    },
  };

  const created = [];
  const document = {
    createElement: (tag) => {
      const el = new FakeEl(tag);
      created.push(el);
      return el;
    },
    body: new FakeEl('body'),
  };

  const mod = makeModule(
    ApiBinary,
    { t: (key) => key },
    escapeHtml,
    (message, kind) => toasts.push({ message, kind }),
    document,
    (fn) => fn(),
  );

  mod.openDeployProgressModal({ deployId, engineId, deployMethod: 'docker', nodeId: 'n1' });

  const win = document.body.children[0];
  const bodyEl = win.children[0];
  return {
    mod,
    win,
    streams,
    toasts,
    statusCalls,
    lines: () => stripTags(bodyEl.querySelector('[data-log-box]').innerHTML).split('\n').filter(Boolean),
    stepLines: () => [...bodyEl.querySelector('[data-log-box]').innerHTML
      .matchAll(/<span class="deploy-log-line is-step">([\s\S]*?)<\/span>/g)].map((m) => m[1]),
    phase: () => bodyEl.querySelector('[data-phase-label]').textContent,
    barWidth: () => bodyEl.querySelector('[data-progress-bar]').style.width,
    chip: () => bodyEl.querySelector('[data-status-chip]').textContent,
    logBox: () => bodyEl.querySelector('[data-log-box]'),
    rawHtml: () => bodyEl.querySelector('[data-log-box]').innerHTML,
    emitLifecycle: (type) => { for (const cb of lifecycle) cb({ type }); },
    settle: () => new Promise((resolve) => setTimeout(resolve, 0)),
    // The window owns a watchdog interval; closing it is what a user does and
    // what lets the test runner's event loop drain.
    dispose: () => win.emit('close-request'),
  };
}

const DEPLOYING = { status: 'deploying', phase: 'build', progressPct: 10, logTail: 'seed-1\nseed-2\nseed-3' };

// ---------------------------------------------------------------------- tests

test('live chunks keep appending after the replay', async (t) => {
  const h = mount({ summaries: [DEPLOYING] });
  t.after(() => h.dispose());
  await h.settle();
  assert.equal(h.streams.length, 1);
  const { handlers } = h.streams[0];
  // Replay of log_tail replaces the seed instead of duplicating it.
  for (const line of ['seed-1', 'seed-2', 'seed-3']) handlers.onChunk(chunk('dep-1', line));
  assert.deepEqual(h.lines(), ['seed-1', 'seed-2', 'seed-3']);
  for (let i = 0; i < 500; i += 1) handlers.onChunk(chunk('dep-1', `live-${i}`));
  const lines = h.lines();
  assert.equal(lines.length, 503);
  assert.equal(lines[502], 'live-499');
});

test('a non-deployment terminal frame is an interruption, not a verdict', async (t) => {
  const h = mount({ summaries: [DEPLOYING] });
  t.after(() => h.dispose());
  await h.settle();
  const first = h.streams[0];
  first.handlers.onChunk(chunk('dep-1', 'building'));
  // Exactly what BinaryWsClient._endAllSubscriptions() delivers on socket death.
  first.handlers.onEnd({ variant: 'StreamClosed', reason: 'transport_closed' });
  assert.deepEqual(h.toasts, [], 'a cut stream must not report a deploy result');
  assert.equal(h.chip(), 'deploy.status_reconnecting');
  // Backoff is 1s for the first attempt.
  await new Promise((resolve) => setTimeout(resolve, 1300));
  assert.equal(h.streams.length, 2, 'the window must come back for the rest of the deploy');
  h.streams[1].handlers.onChunk(chunk('dep-1', 'building'));
  h.streams[1].handlers.onChunk(chunk('dep-1', 'done'));
  assert.deepEqual(h.lines(), ['building', 'done'], 'replay swap leaves no duplicates');
});

test('a reconnect resubscribes and refills from the replay', async (t) => {
  const h = mount({ summaries: [DEPLOYING] });
  t.after(() => h.dispose());
  await h.settle();
  h.streams[0].handlers.onChunk(chunk('dep-1', 'step-1'));
  h.emitLifecycle('disconnected');
  assert.equal(h.chip(), 'deploy.status_reconnecting');
  assert.equal(h.streams[0].closed, true, 'the dead subscription is dropped');
  h.emitLifecycle('open');
  await h.settle();
  assert.equal(h.streams.length, 2);
  h.streams[1].handlers.onChunk(chunk('dep-1', 'step-1'));
  h.streams[1].handlers.onChunk(chunk('dep-1', 'step-2'));
  assert.deepEqual(h.lines().slice(0, 2), ['step-1', 'step-2']);
});

test('a verdict reached while disconnected is not lost', async (t) => {
  const h = mount({
    summaries: [DEPLOYING, { status: 'success', phase: 'ready', progressPct: 100, logTail: 'done' }],
  });
  t.after(() => h.dispose());
  await h.settle();
  h.emitLifecycle('disconnected');
  h.emitLifecycle('open');
  await h.settle();
  assert.deepEqual(h.toasts, [{ message: 'deploy.success', kind: 'success' }]);
  assert.equal(h.chip(), 'deploy.status_success');
  assert.equal(h.streams.length, 1, 'a finished deploy needs no second subscription');
});

test('DeploymentStreamEnd reports the failure and stops the stream', async (t) => {
  const h = mount({ summaries: [DEPLOYING] });
  t.after(() => h.dispose());
  await h.settle();
  h.streams[0].handlers.onEnd({
    variant: 'DeploymentStreamEnd',
    deployId: 'dep-1',
    finalStatus: 'failed',
    errorMessage: 'build exited 1',
    durationMs: 42,
  });
  assert.equal(h.toasts.length, 1);
  assert.equal(h.toasts[0].kind, 'error');
  assert.equal(h.chip(), 'deploy.status_failed');
  await new Promise((resolve) => setTimeout(resolve, 1300));
  assert.equal(h.streams.length, 1, 'a finished deploy must not resubscribe');
});

test('the buffer is bounded and the view follows the newest line', async (t) => {
  const h = mount({ summaries: [{ status: 'deploying', phase: '', progressPct: 0, logTail: '' }] });
  t.after(() => h.dispose());
  await h.settle();
  const { handlers } = h.streams[0];
  for (let i = 0; i < 2500; i += 1) handlers.onChunk(chunk('dep-1', `line-${i}`));
  const lines = h.lines();
  assert.equal(lines.length, 2000);
  assert.equal(lines[0], 'line-500');
  assert.equal(lines[1999], 'line-2499');
  const box = h.logBox();
  assert.equal(box.scrollTop, box.scrollHeight, 'auto-scroll sticks to the bottom');
});

test('a build step arrives as progress and shows up exactly once', async (t) => {
  const h = mount({ summaries: [DEPLOYING] });
  t.after(() => h.dispose());
  await h.settle();
  const { handlers } = h.streams[0];
  handlers.onChunk(chunk('dep-1', 'seed-1'));
  // docker.rs emits a step line ONLY as `progress` — never also as `info`.
  handlers.onChunk(chunk('dep-1', '[docker build] Step 15/24 : COPY . /app', 'progress', {
    phase: 'build-image',
    progressPct: 62,
  }));
  handlers.onChunk(chunk('dep-1', '[docker build]  ---> a1b2c3', 'info'));
  assert.deepEqual(h.lines(), [
    'seed-1',
    '[docker build] Step 15/24 : COPY . /app',
    '[docker build]  ---> a1b2c3',
  ]);
  assert.equal(h.barWidth(), '62%');
  assert.equal(h.phase(), 'build-image');
  assert.deepEqual(h.stepLines(), ['[docker build] Step 15/24 : COPY . /app'],
    'a step line is the progress heading of the wall of log');
});

test('a phase carries its own text and never the slug', async (t) => {
  const h = mount({ summaries: [DEPLOYING] });
  t.after(() => h.dispose());
  await h.settle();
  const { handlers } = h.streams[0];
  handlers.onChunk(chunk('dep-1', '[docker build] pobieranie obrazu bazowego', 'phase', {
    phase: 'pull-base',
  }));
  // A phase with no text only moves the indicator — the log stays clean.
  handlers.onChunk(chunk('dep-1', '', 'phase', { phase: 'health-check' }));
  assert.deepEqual(h.lines(), ['[docker build] pobieranie obrazu bazowego']);
  assert.equal(h.phase(), 'health-check');
  handlers.onChunk(chunk('dep-1', '[docker build] obraz gotowy', 'progress', {
    phase: 'image-built',
    progressPct: 100,
  }));
  assert.equal(h.barWidth(), '100%');
});

test('replayed step lines are not duplicated by the live progress chunk', async (t) => {
  const h = mount({ summaries: [DEPLOYING] });
  t.after(() => h.dispose());
  await h.settle();
  h.streams[0].handlers.onChunk(chunk('dep-1', '[docker build] Step 1/24 : FROM x', 'progress', {
    phase: 'build-image',
    progressPct: 4,
  }));
  h.emitLifecycle('disconnected');
  h.emitLifecycle('open');
  await h.settle();
  // The DB replay carries the very same text as a plain `log` line.
  h.streams[1].handlers.onChunk(chunk('dep-1', '[docker build] Step 1/24 : FROM x'));
  h.streams[1].handlers.onChunk(chunk('dep-1', '[docker build] Step 2/24 : RUN y', 'progress', {
    phase: 'build-image',
    progressPct: 8,
  }));
  assert.deepEqual(h.lines(), [
    '[docker build] Step 1/24 : FROM x',
    '[docker build] Step 2/24 : RUN y',
  ]);
});

test('a log line is escaped before it reaches the log box', async (t) => {
  const h = mount({ summaries: [DEPLOYING], escapeHtml: realEscapeHtml });
  t.after(() => h.dispose());
  await h.settle();
  h.streams[0].handlers.onChunk(chunk('dep-1', '<img src=x onerror=alert(1)> & "q"'));
  const html = h.rawHtml();
  assert.ok(!html.includes('<img'), 'markup from the build output must not be parsed');
  assert.ok(html.includes('&lt;img src=x onerror=alert(1)&gt; &amp; &quot;q&quot;'));
});

test('ANSI colours and CR repaints never reach the console', () => {
  const { cleanLogLine } = makeModule({}, { t: (k) => k }, String, () => {}, {}, (fn) => fn());
  const ESC = '\u001B';
  assert.equal(cleanLogLine(`${ESC}[91mERROR${ESC}[0m: boom`), 'ERROR: boom');
  assert.equal(cleanLogLine('#5 12.3 downloading   '), '#5 12.3 downloading');
  assert.equal(cleanLogLine('pull 10%\rpull 55%\rpull 100%'), 'pull 100%');
  assert.equal(cleanLogLine(`${ESC}]0;title\u0007plain`), 'plain');
  assert.equal(cleanLogLine(`${ESC}[2K${ESC}[1G done`), ' done');
});

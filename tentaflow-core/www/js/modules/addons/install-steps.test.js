// =============================================================================
// File: modules/addons/install-steps.test.js
// Description: The app steps of the install wizard against a stubbed transport:
// rendering of the declared form fields, running a step with its collected
// values, ok / warning / failed results with a re-run, required fields, the
// stop-at-first-failure of "run all" and the notice when closing incomplete.
// Runs under happy-dom with the `/js/` resolver hook.
// =============================================================================

import { window } from '../../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { register } from 'node:module';
import { pathToFileURL, fileURLToPath } from 'node:url';
import { dirname, resolve as pathResolve } from 'node:path';
import { readFileSync } from 'node:fs';

const here = fileURLToPath(import.meta.url);
const WWW_ROOT = pathResolve(dirname(here), '..', '..', '..');
const hookSource = `
  const WWW_ROOT_URL = ${JSON.stringify(pathToFileURL(WWW_ROOT + '/').href)};
  export async function resolve(specifier, context, nextResolve) {
    if (specifier.startsWith('/js/')) {
      return { url: new URL('.' + specifier, WWW_ROOT_URL).href, shortCircuit: true };
    }
    return nextResolve(specifier, context);
  }
`;
register('data:text/javascript,' + encodeURIComponent(hookSource), import.meta.url);

if (typeof globalThis.ResizeObserver !== 'function') {
  globalThis.ResizeObserver = window.ResizeObserver
    || class { observe() {} unobserve() {} disconnect() {} };
}
if (typeof globalThis.MutationObserver !== 'function' && window.MutationObserver) {
  globalThis.MutationObserver = window.MutationObserver;
}
if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;
// The window addresses its fields through CSS.escape, as a browser would.
if (typeof globalThis.CSS === 'undefined' && window.CSS) globalThis.CSS = window.CSS;
// Locale files come from disk so the dialog renders real strings (the
// dependents line interpolates names into the translation); every other
// fetch (component stylesheets) answers empty.
globalThis.fetch = (url) => {
  const m = /^\/i18n\/(\w+)\.json$/.exec(String(url));
  if (m) {
    const text = readFileSync(pathResolve(WWW_ROOT, 'i18n', `${m[1]}.json`), 'utf8');
    return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(JSON.parse(text)), text: () => Promise.resolve(text) });
  }
  return Promise.resolve({ ok: true, text: () => Promise.resolve('') });
};
// i18n persists the chosen language; Node has no Web Storage.
if (typeof globalThis.localStorage === 'undefined') {
  const store = new Map();
  globalThis.localStorage = {
    getItem: (k) => (store.has(k) ? store.get(k) : null),
    setItem: (k, v) => store.set(k, String(v)),
    removeItem: (k) => store.delete(k),
  };
}
// codec.js starts a WASM fetch at import time that rejects under Node; the
// dialog under test never touches the codec because the transport is stubbed.
globalThis.addEventListener?.('unhandledrejection', (e) => e.preventDefault?.());
process.on('unhandledRejection', () => {});


// The window is built from these; install-steps.js imports no components.
await import('../../components/tf-window.js');
await import('../../components/tf-button.js');
await import('../../components/tf-input.js');
await import('../../components/tf-select.js');
await import('../../components/tf-multiselect.js');
await import('../../components/tf-checkbox.js');
await import('../../components/tf-alert.js');
await import('../../components/tf-spinner.js');

const { ApiBinary } = await import('../../protocol/api-binary-shim.js');
const { I18n } = await import('../../i18n.js');
const { openInstallSteps, collectStepValues } = await import('./install-steps.js');
await import('../../protocol/codec.js').then((m) => m.codecReady).catch(() => {});
// The default language is already 'en' (setLanguage('en') is a no-op that
// loads nothing), so go through Polish to get the locale files fetched.
await I18n.setLanguage('pl');
await I18n.setLanguage('en');

const flush = () => new Promise((r) => setTimeout(r, 0));
const settle = async () => { for (let i = 0; i < 5; i += 1) await flush(); };

const calls = [];
function stubTransport(handler) {
  ApiBinary.action = (kind, payload) => {
    calls.push({ kind, payload });
    try {
      return Promise.resolve(handler(kind, payload));
    } catch (e) {
      return Promise.reject(e);
    }
  };
}

const bell = { id: 'bell-test', titleKey: 'apps.tentaquant.install.bell_test.title', descriptionKey: 'apps.tentaquant.install.bell_test.desc', fields: [] };
const form = {
  id: 'form',
  titleKey: 'test.form.title',
  fields: [
    { id: 'mode', kind: 'select', labelKey: 'test.mode', required: true, defaultValue: 'fast',
      options: [{ value: 'fast', labelKey: 'test.fast' }, { value: 'slow', labelKey: 'test.slow' }] },
    { id: 'nodes', kind: 'multiselect', labelKey: 'test.nodes', required: false,
      options: [{ value: 'a', labelKey: 'test.a' }, { value: 'b', labelKey: 'test.b' }] },
    { id: 'agree', kind: 'checkbox', labelKey: 'test.agree', required: true },
    { id: 'note', kind: 'text', labelKey: 'test.note', required: false, defaultValue: 'hello' },
  ],
};

function open(steps, extra = {}) {
  calls.length = 0;
  // Windows only: the toast container is cached by utils.js and must stay attached.
  document.querySelectorAll('tf-window').forEach((w) => w.remove());
  return openInstallSteps({ addonId: 'tentaquant-1a2b3c4d', packageName: 'TentaQuant', steps, ...extra });
}

const section = (win, id) => win.querySelector(`[data-step="${id}"]`);
const run = (win, id) => section(win, id).querySelector('[data-role="run"]');
const alertOf = (win, id) => section(win, id).querySelector('tf-alert');

test('renders one section per declared step with its translated title, description and form', async () => {
  const win = open([bell, form]);
  await settle();
  assert.equal(win.querySelectorAll('[data-step]').length, 2);
  assert.match(section(win, 'bell-test').textContent, /Bell test/);
  assert.match(section(win, 'bell-test').textContent, /two-qubit Bell circuit/);
  assert.equal(section(win, 'bell-test').querySelectorAll('[data-field]').length, 0, 'a step without fields has no form');

  const f = section(win, 'form');
  assert.equal(f.querySelectorAll('[data-field]').length, 4);
  assert.equal(f.querySelector('[data-field="mode"]').value, 'fast', 'the declared default is selected');
  assert.equal(f.querySelector('[data-field="note"]').value, 'hello');
  assert.equal(f.querySelector('[data-field="agree"]').checked, false);
  assert.equal(f.querySelector('[data-field="nodes"]').options.length, 2);
  assert.equal(win.querySelectorAll('[data-role="result"]:not(:empty)').length, 0, 'nothing has run yet');
  win.remove();
});

test('running a step sends the instance, the step id and the collected form values', async () => {
  stubTransport(() => ({ status: 'ok', message: 'done', details: [] }));
  const win = open([form]);
  await settle();
  const f = section(win, 'form');
  f.querySelector('[data-field="mode"]').value = 'slow';
  f.querySelector('[data-field="nodes"]').value = ['b', 'a'];
  f.querySelector('[data-field="agree"]').checked = true;
  f.querySelector('[data-field="note"]').value = '  hi  ';

  run(win, 'form').click();
  await settle();
  assert.equal(calls.length, 1);
  assert.equal(calls[0].kind, 'addonInstanceInstallStepRequest');
  assert.deepEqual(calls[0].payload, {
    addonId: 'tentaquant-1a2b3c4d',
    stepId: 'form',
    values: [['mode', 'slow'], ['nodes', 'b,a'], ['agree', 'true'], ['note', 'hi']],
  });
  win.remove();
});

test('an ok result shows its message and measurements, and the button offers a re-run', async () => {
  stubTransport(() => ({
    status: 'ok',
    message: 'Bell test passed',
    details: [{ name: '00', value: '2051' }, { name: 'duration_ms', value: '3' }],
  }));
  const win = open([bell]);
  await settle();
  run(win, 'bell-test').click();
  await settle();
  assert.equal(alertOf(win, 'bell-test').getAttribute('tone'), 'success');
  assert.equal(alertOf(win, 'bell-test').getAttribute('message'), 'Bell test passed');
  const detail = section(win, 'bell-test').querySelector('[data-role="result"]').textContent;
  assert.match(detail, /00\s*2051/);
  assert.match(detail, /duration_ms\s*3/);
  assert.match(run(win, 'bell-test').textContent, /Run again/);
  win.remove();
});

test('a failed step is shown as failed, offers a retry, and the retry replaces the result', async () => {
  let attempt = 0;
  stubTransport(() => {
    attempt += 1;
    return attempt === 1
      ? { status: 'failed', message: 'simulator fault', details: [] }
      : { status: 'warning', message: 'slow but fine', details: [] };
  });
  const win = open([bell]);
  await settle();
  run(win, 'bell-test').click();
  await settle();
  assert.equal(alertOf(win, 'bell-test').getAttribute('tone'), 'danger');
  assert.equal(alertOf(win, 'bell-test').getAttribute('message'), 'simulator fault');
  assert.match(run(win, 'bell-test').textContent, /Retry/);

  run(win, 'bell-test').click();
  await settle();
  assert.equal(calls.length, 2, 'the step ran again');
  assert.equal(alertOf(win, 'bell-test').getAttribute('tone'), 'warning');
  assert.equal(win.querySelectorAll('tf-alert').length, 1, 'the new result replaced the old one');
  win.remove();
});

test('a transport error is a failed result, not a silent success', async () => {
  stubTransport(() => { throw new Error('connection lost'); });
  const win = open([bell]);
  await settle();
  run(win, 'bell-test').click();
  await settle();
  assert.equal(alertOf(win, 'bell-test').getAttribute('tone'), 'danger');
  assert.match(alertOf(win, 'bell-test').getAttribute('message'), /connection lost/);
  win.remove();
});

test('a required field that is not answered sends nothing', async () => {
  stubTransport(() => ({ status: 'ok', message: 'x', details: [] }));
  const win = open([form]);
  await settle();
  run(win, 'form').click();
  await settle();
  assert.equal(calls.length, 0, 'the required checkbox is not ticked');
  assert.match(document.body.textContent, /Field "[^"]+" is required/);
  win.remove();
});

test('run all goes in order and stops at the first failed step', async () => {
  stubTransport((_kind, { stepId }) => (stepId === 'first'
    ? { status: 'failed', message: 'no', details: [] }
    : { status: 'ok', message: 'yes', details: [] }));
  const win = open([{ ...bell, id: 'first' }, { ...bell, id: 'second' }]);
  await settle();
  win.querySelector('[data-role="run-all"]').click();
  await settle();
  assert.deepEqual(calls.map((c) => c.payload.stepId), ['first'], 'the second step never ran');
  assert.equal(alertOf(win, 'first').getAttribute('tone'), 'danger');
  assert.equal(alertOf(win, 'second'), null);
  win.remove();
});

test('run all skips steps that already passed', async () => {
  stubTransport(() => ({ status: 'ok', message: 'yes', details: [] }));
  const win = open([{ ...bell, id: 'first' }, { ...bell, id: 'second' }]);
  await settle();
  run(win, 'first').click();
  await settle();
  calls.length = 0;
  win.querySelector('[data-role="run-all"]').click();
  await settle();
  assert.deepEqual(calls.map((c) => c.payload.stepId), ['second']);
  win.remove();
});

test('finishing with a step that did not pass says so; finishing green is silent', async () => {
  stubTransport(() => ({ status: 'failed', message: 'no', details: [] }));
  let closed = 0;
  const win = open([bell], { onClose: () => { closed += 1; } });
  await settle();
  run(win, 'bell-test').click();
  await settle();
  win.querySelector('[data-role="finish"]').click();
  await settle();
  assert.equal(closed, 1);
  assert.ok(!document.body.contains(win), 'the window is gone');
  assert.match(document.querySelector('.toast-warning').textContent, /TentaQuant is installed, but 1 setup step has not passed/);

  document.querySelectorAll('.toast').forEach((t) => t.remove());
  stubTransport(() => ({ status: 'ok', message: 'yes', details: [] }));
  const green = open([bell]);
  await settle();
  run(green, 'bell-test').click();
  await settle();
  green.querySelector('[data-role="finish"]').click();
  await settle();
  assert.equal(document.querySelector('.toast-warning'), null, 'nothing to warn about');
});

test('collectStepValues reads every field kind in wire spelling', async () => {
  const win = open([form]);
  await settle();
  const values = collectStepValues(form, section(win, 'form'));
  assert.deepEqual(values, [['mode', 'fast'], ['nodes', ''], ['agree', 'false'], ['note', 'hello']]);
  win.remove();
});

// ============ File: flows-builder/process-retry.test.js — retry offer of an incident: button, confirmation, busy state, result, refusal in words ============

import { window } from '../../sdk-runtime/_dom-test-harness.js';
import test, { after, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

for (const name of ['MutationObserver', 'ResizeObserver', 'Document', 'CSS', 'navigator']) {
  if (globalThis[name] === undefined) globalThis[name] = window[name];
}
Object.defineProperty(globalThis, 'localStorage', { value: window.localStorage, configurable: true });
window.HTMLCanvasElement.prototype.getContext = () => ({ measureText: (value) => ({ width: String(value).length * 7 }) });
const hostFetch = globalThis.fetch;
globalThis.fetch = (url, options) => {
  const path = String(url);
  if (/^\/(i18n|css)\//.test(path)) return Promise.resolve(new Response(readFileSync(new URL(`../../..${path}`, import.meta.url)), { headers: { 'Content-Type': path.endsWith('.json') ? 'application/json' : 'text/css' } }));
  return hostFetch(url, options);
};
const { I18n } = await import('../../i18n.js');
const { ApiBinary } = await import('../../protocol/api-binary-shim.js');
const { TfWindow } = await import('../../components/tf-window.js');
const { openProcessInstance } = await import('./process-monitor.js');
const { RETRY_REASONS, retryOffered, retryRefusal } = await import('./process-retry.js');
const { processIncidentText } = await import('./process-monitor.js');
localStorage.setItem('tentaflow_lang', 'en');
await I18n.init();

const calls = [];
let responder = () => ({});
ApiBinary.one = async (kind, payload) => {
  calls.push({ kind, payload: structuredClone(payload) });
  if (kind === 'mePreferencesUpdateRequest') return {};
  return responder(kind, payload);
};
ApiBinary.action = ApiBinary.one;
const confirm = TfWindow.confirm;
let confirmations = [];
TfWindow.confirm = async (options) => { confirmations.push(options); return true; };
const intervals = new Map();
const setIntervalOriginal = globalThis.setInterval;
const clearIntervalOriginal = globalThis.clearInterval;
globalThis.setInterval = (callback, delay, ...args) => {
  if (delay === 3000 || delay === 10000) { const token = {}; intervals.set(token, { callback, delay }); return token; }
  return setIntervalOriginal(callback, delay, ...args);
};
globalThis.clearInterval = (token) => { if (!intervals.delete(token)) clearIntervalOriginal(token); };
const poll = () => [...intervals.values()].find((row) => row.delay === 3000)?.callback();
const flush = async (count = 6) => { for (let i = 0; i < count; i += 1) await new Promise((resolve) => setTimeout(resolve, 0)); };
const click = (element) => { assert.ok(element, 'the actual control exists'); element.dispatchEvent(new MouseEvent('click', { bubbles: true })); };

afterEach(async () => {
  document.body.replaceChildren();
  await flush(2);
  calls.length = 0; intervals.clear(); confirmations = []; created.length = 0; responder = () => ({});
  await I18n.setLanguage('en');
});
after(async () => {
  document.createElement = createElement; TfWindow.confirm = confirm; globalThis.setInterval = setIntervalOriginal; globalThis.clearInterval = clearIntervalOriginal;
  await window.happyDOM.close();
});

const incident = (overrides = {}) => ({ incidentId: 'inc-1', nodeId: 'Check', nodeName: 'Check contract', jobId: 'job-1', code: 'INTERRUPTED',
  message: 'Worker error', atMs: 1, canRetry: true, scopeId: 'run-1', retry: { kind: 'Job' }, ...overrides });
function run(incidents, overrides = {}) {
  const value = { instanceId: 'run-1', definitionId: 'definition-one', definitionName: 'Document approval', initiatorUserId: 'owner', version: 2, revision: 11,
    status: 'Incident', variables: {}, activeNodeIds: ['Check'], userTasks: [], incidents, timers: [], subscriptions: [], eventRaces: [],
    outgoingMessages: [], messageNames: [], scopes: [], calls: [], repetitionGroups: [], repetitionOccurrences: [], selectedRepetitionOccurrence: null,
    canSendMessage: false, createdAtMs: 1000, updatedAtMs: 2000, canCancel: false, canRetry: incidents.some((row) => row.canRetry), ...overrides };
  value.pages = Object.fromEntries(['userTasks', 'incidents', 'timers', 'subscriptions', 'eventRaces', 'outgoingMessages', 'scopes', 'calls', 'repetitionGroups', 'repetitionOccurrences']
    .map((name) => [name, { offset: 0, total: value[name].length, nextOffset: null, hasMore: false }]));
  return value;
}
async function open(current, additions = {}) {
  responder = (kind, payload) => {
    if (kind in additions) return typeof additions[kind] === 'function' ? additions[kind](payload) : structuredClone(additions[kind]);
    if (kind === 'processInstanceGetRequest') return { instance: current };
    if (kind === 'processHistoryRequest') return { events: [], nextSeq: 0, hasMore: false };
    throw new Error(`Unexpected request ${kind}`);
  };
  const win = await openProcessInstance(current.instanceId, current);
  await flush();
  return win;
}
// Toasts live in a container the page keeps across tests, so the ones made are collected as they are created.
const created = [];
const createElement = document.createElement;
document.createElement = (...args) => { const node = createElement.apply(document, args); created.push(node); return node; };
const toastTexts = () => created.filter((node) => String(node.className).startsWith('toast ')).map((node) => node.textContent);

test('a retryable task incident offers Retry, asks first, sends the revision it saw and says it started', async () => {
  const current = run([incident()]);
  const after = run([], { revision: 12, status: 'Running' });
  let latest = current;
  const win = await open(current, { processInstanceGetRequest: () => ({ instance: latest }), processJobRetryRequest: () => { latest = after; return { instance: after }; } });
  click(win.querySelector('[data-retry]')); await flush();
  assert.equal(confirmations.length, 1);
  assert.equal(confirmations[0].message, I18n.t('bpmn.retry_hint'));
  const sent = calls.find((row) => row.kind === 'processJobRetryRequest').payload;
  assert.deepEqual([sent.instanceId, sent.jobId, sent.expectedRevision], ['run-1', 'job-1', 11]);
  assert.ok(sent.commandId);
  assert.ok(toastTexts().includes(I18n.t('bpmn.retry_started')), toastTexts().join('|'));
  assert.equal(win.querySelector('[data-retry]'), null, 'the incident went away with the retry');
});

test('while the retry is in flight the control is disabled and the window is inert, and a second click sends nothing', async () => {
  let release;
  const gate = new Promise((resolve) => { release = resolve; });
  const win = await open(run([incident()]), { processJobRetryRequest: async () => { await gate; return { instance: run([], { revision: 12 }) }; } });
  const button = win.querySelector('[data-retry]');
  click(button); await flush();
  assert.equal(win.querySelector('[data-retry]').hasAttribute('disabled'), true);
  assert.equal(win.querySelector('[data-content]').hasAttribute('inert'), true);
  click(win.querySelector('[data-retry]')); await flush();
  release(); await flush(10);
  assert.equal(calls.filter((row) => row.kind === 'processJobRetryRequest').length, 1);
  assert.equal(win.querySelector('[data-content]').hasAttribute('inert'), false);
});

test('a declined confirmation sends nothing', async () => {
  TfWindow.confirm = async () => false;
  try {
    const win = await open(run([incident()]));
    click(win.querySelector('[data-retry]')); await flush();
    assert.equal(calls.some((row) => row.kind === 'processJobRetryRequest'), false);
  } finally { TfWindow.confirm = async (options) => { confirmations.push(options); return true; }; }
});

test('a refused retry shows a sentence in the reader language, never the server words, and reloads', async () => {
  const win = await open(run([incident()]), {
    processJobRetryRequest: () => { const error = new Error('the initiator no longer owns the definition, so the work cannot run in their name'); error.code = 'PolicyDenied'; throw error; },
  });
  const reads = () => calls.filter((row) => row.kind === 'processInstanceGetRequest').length;
  const before = reads();
  click(win.querySelector('[data-retry]')); await flush(10);
  const alert = win.querySelector('[data-error]');
  assert.equal(alert.hidden, false);
  assert.match(alert.getAttribute('message'), new RegExp(I18n.t('bpmn.retry_refused').slice(0, 20)));
  assert.doesNotMatch(alert.getAttribute('message'), /initiator no longer owns/);
  assert.ok(reads() > before, 'the instance is read again');
  assert.equal(toastTexts().includes(I18n.t('bpmn.retry_started')), false);
});

test('focus stays on the Retry control when the same incident is drawn again', async () => {
  const current = run([incident(), incident({ incidentId: 'inc-2', jobId: 'job-2', nodeName: 'Second step' })]);
  let latest = current;
  const win = await open(current, { processInstanceGetRequest: () => ({ instance: latest }) });
  const second = win.querySelector('[data-focus-key="retry:inc-2"]');
  assert.ok(second);
  second.querySelector('button').focus();
  assert.ok(document.activeElement === second.querySelector('button'), 'focus is on the control before the redraw');
  latest = { ...current, revision: 12, updatedAtMs: 3000 };
  await poll(); await flush();
  const redrawn = win.querySelector('[data-focus-key="retry:inc-2"]');
  assert.ok(redrawn !== second, 'the row was rebuilt');
  assert.ok(document.activeElement === redrawn.querySelector('button'), 'focus is back on the rebuilt control');
});

test('an accepted result that failed to map offers Apply result again in its own words and sends the same retry', async () => {
  const mapping = incident({ code: 'ACTIVITY_IO_OUTPUT_FAILED', retry: { kind: 'OutputMapping' } });
  const after = run([], { revision: 12, status: 'Running' });
  let latest = run([mapping]);
  const win = await open(latest, { processInstanceGetRequest: () => ({ instance: latest }), processJobRetryRequest: () => { latest = after; return { instance: after }; } });
  assert.equal(win.querySelector('[data-retry]').textContent.trim(), I18n.t('bpmn.retry_output'));
  click(win.querySelector('[data-retry]')); await flush();
  assert.equal(confirmations.length, 1);
  assert.deepEqual([confirmations[0].title, confirmations[0].message, confirmations[0].confirmLabel],
    [I18n.t('bpmn.retry_output'), I18n.t('bpmn.retry_output_hint'), I18n.t('bpmn.retry_output')]);
  const sent = calls.filter((row) => row.kind === 'processJobRetryRequest');
  assert.equal(sent.length, 1);
  assert.deepEqual([sent[0].payload.jobId, sent[0].payload.expectedRevision], ['job-1', 11]);
  assert.ok(toastTexts().includes(I18n.t('bpmn.retry_output_started')), toastTexts().join('|'));
  assert.equal(toastTexts().includes(I18n.t('bpmn.retry_started')), false);
});

test('an exhausted output retry says so in words and offers nothing', async () => {
  const win = await open(run([incident({ code: 'ACTIVITY_IO_OUTPUT_FAILED', canRetry: false, retry: { kind: 'OutputMapping', deniedReason: 'OutputRetryLimit' } })], { canRetry: false }));
  assert.equal(win.querySelector('[data-retry]'), null);
  assert.equal(win.querySelector('[data-retry-note]').textContent, I18n.t('bpmn.retry_denied_output_retry_limit'));
});

test('an incident without an offer says why, in words, and has no Retry button', async () => {
  const win = await open(run([
    incident({ incidentId: 'a', canRetry: false, retry: { kind: 'Job', deniedReason: 'ViewOnly' } }),
    incident({ incidentId: 'b', jobId: null, code: 'NO_MATCHING_FLOW', canRetry: false, retry: { kind: 'None', deniedReason: 'SameFailureRepeats' } }),
    incident({ incidentId: 'c', jobId: null, code: 'ACTIVITY_IO_OUTPUT_FAILED', canRetry: false, retry: { kind: 'OutputMapping', deniedReason: 'ResultNotRetained' } }),
    incident({ incidentId: 'd', jobId: null, code: 'X_UNKNOWN', canRetry: false }),
  ], { canRetry: false }));
  assert.equal(win.querySelector('[data-retry]'), null);
  const notes = [...win.querySelectorAll('[data-retry-note]')].map((node) => node.textContent);
  assert.deepEqual(notes, [I18n.t('bpmn.retry_denied_view_only'), I18n.t('bpmn.retry_denied_same_failure_repeats'), I18n.t('bpmn.retry_denied_result_not_retained')]);
  for (const note of notes) assert.doesNotMatch(note, /ViewOnly|SameFailureRepeats|OutputMapping|_/);
});

test('retryOffered and retryRefusal read the server answer and tolerate an old server without it', () => {
  assert.equal(retryOffered(incident()), true);
  assert.equal(retryOffered(incident({ canRetry: false })), false);
  assert.equal(retryOffered(incident({ jobId: null })), false);
  assert.equal(retryRefusal(incident()), null);
  assert.equal(retryRefusal(incident({ canRetry: false, retry: undefined })), null);
  assert.equal(retryRefusal(incident({ canRetry: false, retry: { kind: 'Job', deniedReason: 'FromTheFuture' } })), I18n.t('bpmn.retry_denied_generic'));
});

test('every reason the server can give has a sentence in all five locales and the list matches the wire enum', () => {
  const protocol = readFileSync(new URL('../../../../../tentaflow-protocol/src/processes.rs', import.meta.url), 'utf8');
  const body = protocol.match(/pub enum ProcessRetryDenied \{([\s\S]*?)\n\}/)[1];
  const variants = [...body.matchAll(/^\s{4}([A-Z][A-Za-z]+),/gm)].map((match) => match[1]);
  assert.ok(variants.length >= 10, `read ${variants.length} variants`);
  assert.deepEqual([...RETRY_REASONS].sort(), [...variants].sort());
  const keys = [...RETRY_REASONS.map((reason) => `retry_denied_${reason.replace(/([a-z])([A-Z])/g, '$1_$2').toLowerCase()}`),
    'retry_denied_generic', 'retry_denied_quota', 'retry_denied_permission', 'incident_token_quota_exceeded', 'incident_tool_permission_denied',
    'retry_output', 'retry_output_hint', 'retry_output_started', 'event_output_retried', 'event_output_retry_failed', 'retry_started', 'retry_refused', 'retry_hint'];
  const english = JSON.parse(readFileSync(new URL('../../../i18n/en.json', import.meta.url), 'utf8')).bpmn;
  for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
    const bpmn = JSON.parse(readFileSync(new URL(`../../../i18n/${language}.json`, import.meta.url), 'utf8')).bpmn;
    for (const key of keys) {
      assert.equal(typeof bpmn[key], 'string', `${language}.${key}`);
      assert.ok(bpmn[key].length > 10, `${language}.${key}`);
      if (language !== 'en') assert.notEqual(bpmn[key], english[key], `${language}.${key} is translated`);
    }
    for (const key of keys.filter((name) => name.startsWith('retry_denied_'))) assert.doesNotMatch(bpmn[key], /węzeł/i, `${language}.${key}`);
  }
});

// A confirmation held open while the window is clicked again: the first answer is the only question.
function gatedConfirm() {
  let answer;
  const gate = new Promise((resolve) => { answer = resolve; });
  TfWindow.confirm = async (options) => { confirmations.push(options); return gate; };
  return answer;
}
const restoreConfirm = () => { TfWindow.confirm = async (options) => { confirmations.push(options); return true; }; };

test('clicking Retry again while the question is open asks once and sends once', async () => {
  const answer = gatedConfirm();
  try {
    const win = await open(run([incident()]), { processJobRetryRequest: () => ({ instance: run([], { revision: 12 }) }) });
    click(win.querySelector('[data-retry]')); await flush();
    click(win.querySelector('[data-retry]')); await flush();
    assert.equal(confirmations.length, 1, 'one question');
    answer(true); await flush(10);
    assert.equal(calls.filter((row) => row.kind === 'processJobRetryRequest').length, 1, 'one request');
  } finally { restoreConfirm(); }
});

test('what the window shows after the answer decides: an incident that moved on to another job sends nothing and is read again', async () => {
  const answer = gatedConfirm();
  try {
    const current = run([incident()]);
    let latest = current;
    const win = await open(current, { processInstanceGetRequest: () => ({ instance: latest }) });
    click(win.querySelector('[data-retry]')); await flush();
    latest = run([incident({ jobId: 'job-2' })], { revision: 12, updatedAtMs: 3000 });
    await poll(); await flush();
    const reads = calls.filter((row) => row.kind === 'processInstanceGetRequest').length;
    answer(true); await flush(10);
    assert.equal(calls.some((row) => row.kind === 'processJobRetryRequest'), false, 'nothing is sent for the job the reader no longer sees');
    assert.ok(calls.filter((row) => row.kind === 'processInstanceGetRequest').length > reads, 'the instance is read again');
  } finally { restoreConfirm(); }
});

test('an incident the reader can no longer retry after the answer sends nothing', async () => {
  const answer = gatedConfirm();
  try {
    const current = run([incident()]);
    let latest = current;
    const win = await open(current, { processInstanceGetRequest: () => ({ instance: latest }) });
    click(win.querySelector('[data-retry]')); await flush();
    latest = run([incident({ canRetry: false, retry: { kind: 'Job', deniedReason: 'ActivationBusy' } })], { revision: 12, updatedAtMs: 3000 });
    await poll(); await flush();
    answer(true); await flush(10);
    assert.equal(calls.some((row) => row.kind === 'processJobRetryRequest'), false);
  } finally { restoreConfirm(); }
});

test('after a retry the focus is still inside the window, and Retry selects no incident for the detail', async () => {
  const win = await open(run([incident()]), { processJobRetryRequest: () => ({ instance: run([], { revision: 12, status: 'Running' }) }) });
  const button = win.querySelector('[data-retry] button');
  button.focus();
  assert.ok(document.activeElement === button, 'focus starts on the control');
  click(win.querySelector('[data-retry]')); await flush(10);
  assert.equal(win.querySelector('[data-retry]'), null, 'the control is gone');
  assert.ok(win.contains(document.activeElement) && document.activeElement !== document.body, 'focus did not fall to the page');
  assert.equal(calls.filter((row) => row.kind === 'processInstanceGetRequest')
    .some((row) => JSON.stringify(row.payload).includes('"selectedIncidentId":"inc-1"')), false, 'Retry does not open the incident detail');
});

test('a spent token quota and a missing permission are told apart in the incident and in the reason', async () => {
  for (const [code, keyIncident, keyReason] of [['TOKEN_QUOTA_EXCEEDED', 'incident_token_quota_exceeded', 'retry_denied_quota'],
    ['TOOL_PERMISSION_DENIED', 'incident_tool_permission_denied', 'retry_denied_permission']]) {
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      const refused = incident({ code, canRetry: false, retry: { kind: 'None', deniedReason: 'SameFailureRepeats' } });
      assert.equal(processIncidentText(refused), I18n.t(`bpmn.${keyIncident}`), `${language} ${code}`);
      assert.equal(retryRefusal(refused), I18n.t(`bpmn.${keyReason}`), `${language} ${code}`);
      assert.notEqual(retryRefusal(refused), I18n.t('bpmn.retry_denied_same_failure_repeats'), `${language} ${code}`);
    }
  }
  await I18n.setLanguage('en');
  assert.equal(retryRefusal(incident({ code: 'NO_MATCHING_FLOW', canRetry: false, retry: { kind: 'None', deniedReason: 'SameFailureRepeats' } })),
    I18n.t('bpmn.retry_denied_same_failure_repeats'));
});

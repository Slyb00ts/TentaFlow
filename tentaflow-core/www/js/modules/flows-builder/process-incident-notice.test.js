// ============ File: flows-builder/process-incident-notice.test.js — incident toast listener, its link to the run, and the "Needs attention" list filter ============

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
const { init: initSystemEvents, showProcessIncidentNotice, PROCESS_INCIDENT_KIND } = await import('../system-events.js');
const { openProcessInstances } = await import('./process-monitor.js');
const { loadAttentionInstances, mergeAttentionInstances } = await import('./process-attention.js');
localStorage.setItem('tentaflow_lang', 'en');
await I18n.init();

const calls = [];
let responder = () => ({});
ApiBinary.one = async (kind, payload) => {
  if (kind === 'mePreferencesUpdateRequest') return {};
  calls.push({ kind, payload: structuredClone(payload) });
  return responder(kind, payload);
};
ApiBinary.action = ApiBinary.one;
const flush = async (count = 8) => { for (let i = 0; i < count; i += 1) await new Promise((resolve) => setTimeout(resolve, 0)); };
const toasts = () => [...document.querySelectorAll('tf-toast')];

afterEach(async () => {
  document.body.replaceChildren();
  await flush(2);
  calls.length = 0;
  responder = () => ({});
  await I18n.setLanguage('en');
});
after(async () => { await window.happyDOM.close(); });

let sequence = 0;
const notice = (fields = {}, link = {}) => {
  sequence += 1;
  return { _variant: 'SystemEventBody.UserNotification', userId: 'u1', notificationId: `process-incident:inc-${sequence}:u1`,
    projectId: '', kind: PROCESS_INCIDENT_KIND, title: 'Order approval', body: 'NO_MATCHING_FLOW',
    linkJson: JSON.stringify({ instanceId: `inst-${sequence}`, incidentId: `inc-${sequence}`, nodeId: 'Gate', code: 'NO_MATCHING_FLOW', processName: 'Order approval', ...link }), ...fields };
};

test('an incident notice becomes a warning toast built from the code, in the reader language', async () => {
  const event = notice({ body: 'raw worker text sk-live-4242' });
  assert.equal(await showProcessIncidentNotice(event), true);
  const [shown] = toasts();
  assert.ok(shown, 'a toast is on the screen');
  assert.equal(shown.getAttribute('tone'), 'warning');
  assert.equal(shown.getAttribute('title'), 'A process run needs attention: Order approval');
  assert.match(shown.getAttribute('message'), /./);
  assert.ok(!shown.outerHTML.includes('sk-live-4242'), 'the server body is never shown');

  await I18n.setLanguage('pl');
  await showProcessIncidentNotice(notice());
  const polish = toasts().at(-1);
  assert.equal(polish.getAttribute('title'), 'Wykonanie procesu wymaga uwagi: Order approval');
});

test('the toast action opens the instance of the notice', async () => {
  responder = () => { throw new Error('stop after the first read'); };
  const event = notice({}, { instanceId: 'inst-open-me' });
  await showProcessIncidentNotice(event);
  const action = document.querySelector('tf-toast tf-button[slot="action"], tf-toast tf-button');
  assert.ok(action, 'the toast carries the open action');
  assert.equal(action.getAttribute('label'), 'Open run');
  action.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  await flush();
  assert.ok(calls.some((call) => call.kind === 'processInstanceGetRequest' && call.payload.instanceId === 'inst-open-me'),
    JSON.stringify(calls.map((call) => call.kind)));
});

test('a repeated notice id is shown once and a foreign or broken notice is not a process toast', async () => {
  const event = notice();
  await showProcessIncidentNotice(event);
  await showProcessIncidentNotice(event);
  assert.equal(toasts().length, 1);

  assert.equal(await showProcessIncidentNotice(notice({ kind: 'task_assigned' })), false);
  assert.equal(await showProcessIncidentNotice(notice({ linkJson: 'not json' })), true);
  assert.equal(await showProcessIncidentNotice(notice({ linkJson: JSON.stringify({ code: 'X' }) })), true);
  assert.equal(toasts().length, 1, 'only the first, valid notice reached the screen');
});

const run = (id, updatedAtMs, extra = {}) => ({ instanceId: id, definitionName: `Process ${id}`, version: 1, status: 'Incident', updatedAtMs, ...extra });

test('the attention view merges the runs of mine and owned without a duplicate, newest first', async () => {
  assert.deepEqual(mergeAttentionInstances([[run('a', 10), run('b', 30)], [run('b', 30), run('c', 20)]]).map((item) => item.instanceId), ['b', 'c', 'a']);
  const asked = [];
  const found = await loadAttentionInstances(async (scope) => { asked.push(scope); return { instances: [run(scope, 1)], hasMore: scope === 'Owned' }; });
  assert.deepEqual(asked, ['Mine', 'Owned']);
  assert.equal(found.instances.length, 2);
  assert.equal(found.more, true);
});

test('the run list filter asks only for open incidents in both scopes and returns to the plain list', async () => {
  responder = (kind, payload) => {
    if (kind !== 'processInstanceListRequest') return {};
    if (payload.hasIncident) return { instances: payload.scope === 'Mine' ? [run('mine-1', 20)] : [run('owned-1', 30)], total: 1, hasMore: false };
    return { instances: [run('plain-1', 10, { status: 'Running' }), run('plain-2', 5, { status: 'Running' })], total: 2, hasMore: false };
  };
  const win = await openProcessInstances();
  const table = win.querySelector('tf-table');
  assert.equal(table.rows.length, 2);
  assert.ok(calls.every((call) => call.payload.hasIncident === undefined), 'the plain list does not filter');

  const filter = win.querySelector('tf-segmented[data-filter]');
  assert.deepEqual([...filter.querySelectorAll('option')].map((option) => option.textContent), ['All runs', 'Needs attention']);
  calls.length = 0;
  filter.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { value: 'attention' } }));
  await flush();
  assert.deepEqual(calls.map((call) => [call.payload.scope, call.payload.hasIncident]).sort(), [['Mine', true], ['Owned', true]]);
  assert.deepEqual(table.rows.map((row) => row.instanceId), ['owned-1', 'mine-1']);
  assert.equal(win.querySelector('[data-attention-count]').textContent, '2 runs need attention');

  calls.length = 0;
  filter.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { value: 'all' } }));
  await flush();
  assert.deepEqual(table.rows.map((row) => row.instanceId), ['plain-1', 'plain-2']);
  assert.equal(win.querySelector('[data-attention-count]').hidden, true);
});

test('with nothing open the attention view says so', async () => {
  responder = (kind, payload) => (kind === 'processInstanceListRequest'
    ? { instances: payload.hasIncident ? [] : [run('p', 1)], total: payload.hasIncident ? 0 : 1, hasMore: false } : {});
  const win = await openProcessInstances();
  win.querySelector('tf-segmented[data-filter]').dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { value: 'attention' } }));
  await flush();
  assert.equal(win.querySelector('[data-attention-count]').textContent, 'No run needs attention');
});

test('the notice and filter wording exists in all five locales with the same keys and a Polish plural of three forms', () => {
  const keys = ['incident_notice_title', 'instances_filter_label', 'instances_filter_all', 'instances_filter_attention', 'instances_attention_count', 'instances_attention_empty'];
  for (const language of ['pl', 'en', 'de', 'es', 'fr']) {
    const bpmn = JSON.parse(readFileSync(new URL(`../../../i18n/${language}.json`, import.meta.url), 'utf8')).bpmn;
    for (const key of keys) assert.equal(typeof bpmn[key], 'string', `${language}.${key}`);
    assert.match(bpmn.incident_notice_title, /\{name\}/, language);
    const plural = bpmn.instances_attention_count.match(/\{count\|([^}]*)\}/);
    assert.ok(plural, language);
    assert.equal(plural[1].split('|').length, language === 'pl' ? 3 : 2, language);
  }
});

test('the listener follows the client that a login replaces, so personal events reach the signed-in socket', async () => {
  const fakeClient = () => { const listeners = []; return { listeners, addUnsolicitedListener: (listener) => listeners.push(listener) }; };
  const anonymous = fakeClient();
  const signedIn = fakeClient();
  let current = anonymous;
  let lifecycle = () => {};
  ApiBinary.client = async () => current;
  ApiBinary.onLifecycle = (callback) => { lifecycle = callback; };
  initSystemEvents();
  await flush();
  assert.equal(anonymous.listeners.length, 1);

  current = signedIn;
  lifecycle({ type: 'open' });
  lifecycle({ type: 'open' });
  lifecycle({ type: 'disconnected' });
  await flush();
  assert.equal(signedIn.listeners.length, 1, 'one listener per client, however often it reopens');

  signedIn.listeners[0]({ body: notice() });
  await flush();
  assert.equal(toasts().length, 1);
});

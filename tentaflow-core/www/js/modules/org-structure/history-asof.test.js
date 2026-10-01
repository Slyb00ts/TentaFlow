// =============================================================================
// File: modules/org-structure/history-asof.test.js
// Description: The "Stan na" control of the Drzewo tab against a stubbed
//   transport: asking for another day reads that day and what differs from today,
//   marks the differences on the chart's model, holds the screen's own refreshes
//   back while it is on, and goes back to today's structure without asking for it
//   again; a day the server cannot give leaves the chart on today.
//   Runs under happy-dom.
// =============================================================================

import { window } from '../../sdk-runtime/_dom-test-harness.js';
import { test, beforeEach } from 'node:test';
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
  globalThis.ResizeObserver = window.ResizeObserver || class { observe() {} unobserve() {} disconnect() {} };
}
if (typeof globalThis.MutationObserver !== 'function' && window.MutationObserver) globalThis.MutationObserver = window.MutationObserver;
if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;
if (typeof globalThis.CSS === 'undefined' && window.CSS) globalThis.CSS = window.CSS;
globalThis.fetch = (url) => {
  const m = /^\/i18n\/(\w+)\.json$/.exec(String(url));
  if (m) {
    const text = readFileSync(pathResolve(WWW_ROOT, 'i18n', `${m[1]}.json`), 'utf8');
    return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(JSON.parse(text)), text: () => Promise.resolve(text) });
  }
  return Promise.resolve({ ok: true, text: () => Promise.resolve('') });
};
if (typeof globalThis.localStorage === 'undefined') {
  const store = new Map();
  globalThis.localStorage = {
    getItem: (k) => (store.has(k) ? store.get(k) : null),
    setItem: (k, v) => store.set(k, String(v)),
    removeItem: (k) => store.delete(k),
  };
}
globalThis.addEventListener?.('unhandledrejection', (e) => e.preventDefault?.());
process.on('unhandledRejection', () => {});

const { I18n } = await import('../../i18n.js');
await I18n.setLanguage('pl');
const { ApiBinary } = await import('../../protocol/api-binary-shim.js');
const { mountTreeTab, refreshTreeTab, unmountTreeTab } = await import('./tree-tab.js');
const { requestTreeAsOf } = await import('./history-asof.js');

const TODAY = '2026-09-30';
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const settle = async () => { for (let i = 0; i < 8; i += 1) await sleep(0); };
const t = (key, params) => I18n.t(`org_structure.history.${key}`, params);
const fire = (target, type, detail) => target.dispatchEvent(new window.CustomEvent(type, { bubbles: true, detail }));

const view = (at, positions) => ({
  at,
  timezone: 'Europe/Warsaw',
  units: [{ unit_id: 'u-it', name: 'Dział IT', parent_unit_id: null, head_position_id: 'p-cto', deputy_head_position_ids: [] }],
  positions: positions.map((id) => ({ position_id: id, unit_id: 'u-it', name: id, primary_parent_position_id: id === 'p-cto' ? null : 'p-cto', is_staff: false, valid_from: '2026-01-01' })),
  assignments: [],
  vacancies: positions,
  warnings: [],
});

let requests;
let failDay;

beforeEach(() => {
  requests = [];
  failDay = null;
  ApiBinary.one = async (kind, payload) => {
    requests.push({ kind, payload });
    if (kind === 'authMeRequest') return { userId: 'u-me', username: 'me' };
    if (kind === 'orgStructureRequest') {
      if (payload?.at && payload.at === failDay) throw new Error('boom');
      return { view: view(payload?.at ?? TODAY, payload?.at === '2026-11-01' ? ['p-cto', 'p-dev', 'p-new'] : ['p-cto', 'p-dev']), unit_types: [], my_permissions: ['org.admin'] };
    }
    if (kind === 'orgHistoryListRequest') return { entries: [], total: 0, personal_visible: true, today: TODAY };
    if (kind === 'orgChangeSetListRequest') {
      return { items: [{ id: 'cs-1', name: 'Reorganizacja Q4', state: 'pending', effective_date: '2026-11-01', op_count: 2 }], today: TODAY };
    }
    if (kind === 'orgChangeSetPreviewRequest') {
      return {
        ok: true, valid: true, at: '2026-11-01', results: [], warnings: [], live: view('2026-11-01', ['p-cto', 'p-dev']),
        preview: view('2026-11-01', ['p-cto', 'p-dev', 'p-plan']),
        items: [{ change: 'added', entity: 'position', id: 'p-plan', name: 'p-plan', unit_id: 'u-it' }],
      };
    }
    if (kind === 'orgHistoryDiffRequest') {
      return { items: [{ change: 'added', entity: 'position', id: 'p-new', name: 'p-new', unit_id: 'u-it' }], personal_visible: true };
    }
    return {};
  };
  ApiBinary.action = async () => ({});
});

const today = () => ({ view: view(TODAY, ['p-cto', 'p-dev']), unitTypes: [], myPermissions: ['org.admin'] });

async function mount() {
  unmountTreeTab();
  document.body.innerHTML = '<div id="host"></div><span id="org-header-actions"></span>';
  await mountTreeTab(document.getElementById('host'), today());
  await settle();
}

const chart = () => document.getElementById('org-tree');
const asof = () => document.getElementById('org-tree-asof');
const note = () => document.getElementById('org-tree-asof-note');
const nodeIds = () => chart().model.nodes.map((n) => n.id).sort();

test('the control offers "today" until another day is asked for', async () => {
  await mount();
  assert.match(asof().textContent, new RegExp(t('asof_today')));
  assert.equal(note().querySelector('[data-act="asof-back"]'), null);
});

test('asking for a planned day reads it, marks the new position and keeps the screen\'s refreshes away', async () => {
  await mount();
  requestTreeAsOf('2026-11-01');
  await settle();
  assert.deepEqual(requests.filter((r) => r.kind === 'orgStructureRequest').at(-1).payload, { at: '2026-11-01' });
  assert.deepEqual(requests.filter((r) => r.kind === 'orgHistoryDiffRequest').at(-1).payload, { from: TODAY, to: '2026-11-01' });
  assert.deepEqual(nodeIds(), ['p-cto', 'p-dev', 'p-new']);
  const marks = Object.fromEntries(chart().model.nodes.map((n) => [n.id, n.mark]));
  assert.deepEqual(marks, { 'p-cto': null, 'p-dev': null, 'p-new': 'added' });
  assert.match(asof().textContent, /01\.11\.2026/);
  assert.match(note().textContent, /dodano 1/);

  refreshTreeTab(today());
  await settle();
  assert.deepEqual(nodeIds(), ['p-cto', 'p-dev', 'p-new'], 'a refresh of today does not pull the chart back');
});

test('going back restores today\'s structure without asking for it again, and unmarks the cards', async () => {
  await mount();
  requestTreeAsOf('2026-11-01');
  await settle();
  const asked = requests.filter((r) => r.kind === 'orgStructureRequest').length;
  note().querySelector('[data-act="asof-back"]').click();
  await settle();
  assert.equal(requests.filter((r) => r.kind === 'orgStructureRequest').length, asked);
  assert.deepEqual(nodeIds(), ['p-cto', 'p-dev']);
  assert.ok(chart().model.nodes.every((n) => !n.mark));
  assert.match(asof().textContent, new RegExp(t('asof_today')));
});

test('asking for today itself is going back', async () => {
  await mount();
  requestTreeAsOf('2026-11-01');
  await settle();
  requestTreeAsOf(TODAY);
  await settle();
  assert.equal(note().querySelector('[data-act="asof-back"]'), null);
});

test('a day the server cannot give leaves the chart on today', async () => {
  await mount();
  failDay = '2026-12-01';
  requestTreeAsOf('2026-12-01');
  await settle();
  assert.deepEqual(nodeIds(), ['p-cto', 'p-dev']);
  assert.equal(note().querySelector('[data-act="asof-back"]'), null);
});

test('a reorganization that is not approved can be looked at through its preview, marked, and left again', async () => {
  await mount();
  asof().querySelector('[data-act="asof"]').click();
  await settle();
  await sleep(20);
  const item = [...document.querySelectorAll('tf-menu-item')].find((el) => el.getAttribute('label').includes('Reorganizacja Q4'));
  assert.ok(item, 'the menu offers the open reorganization');
  fire(item.closest('tf-menu'), 'action', { action: item.getAttribute('action') });
  await settle();
  await sleep(20);
  assert.deepEqual(requests.filter((r) => r.kind === 'orgChangeSetPreviewRequest').at(-1).payload, { id: 'cs-1', unitId: null });
  assert.deepEqual(nodeIds(), ['p-cto', 'p-dev', 'p-plan']);
  assert.equal(chart().model.nodes.find((n) => n.id === 'p-plan').mark, 'added');
  assert.match(asof().textContent, /Reorganizacja Q4/);
  assert.equal(requests.filter((r) => r.kind === 'orgStructureRequest' && r.payload?.at === '2026-11-01').length, 0, 'the structure itself is not asked for: it holds nothing of the plan');

  note().querySelector('[data-act="asof-back"]').click();
  await settle();
  assert.deepEqual(nodeIds(), ['p-cto', 'p-dev']);
});

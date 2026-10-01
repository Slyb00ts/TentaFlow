// =============================================================================
// File: modules/org-structure/index.test.js
// Description: The org-structure screen against a stubbed transport. What has
// to hold: the five tabs come in the mockups' order and Katalog ról leaves for
// its own screen, the Drzewo and Lista tabs draw what the wire returned (no
// invented rows), the Drzewo toolbar searches, jumps to the caller's seat,
// switches views, presents and exports, a structure that never arrives says so instead of drawing
// zeros, and picking a day in Historia asks the server for THAT day.
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
// codec.js starts a WASM fetch at import time that rejects under Node; the
// screen under test never reaches the codec because the transport is stubbed.
globalThis.addEventListener?.('unhandledrejection', (e) => e.preventDefault?.());
process.on('unhandledRejection', () => {});

const { I18n } = await import('../../i18n.js');
await I18n.setLanguage('pl');

const { ApiBinary } = await import('../../protocol/api-binary-shim.js');
const { formatDay } = await import('../../lib/date-format.js');
const { Router } = await import('../../router.js');
const { default: OrgStructureScreen } = await import('./index.js');


const flush = () => new Promise((r) => setTimeout(r, 0));
const t = (key, params) => I18n.t(`org_structure.${key}`, params);

const navigated = [];
Router.replaceParams = () => {};
Router.navigate = async (id, params) => { navigated.push({ id, params }); return true; };

const calls = [];
function stubTransport(fixtures) {
  ApiBinary.one = (kind, payload) => {
    calls.push({ kind, payload });
    if (!(kind in fixtures)) return Promise.reject(new Error(`unexpected request ${kind}`));
    const f = fixtures[kind];
    try {
      return Promise.resolve(typeof f === 'function' ? f(payload) : f);
    } catch (e) {
      return Promise.reject(e);
    }
  };
}

const anna = { kind: 'user', id: 'u-anna' };

const view = (at) => ({
  at,
  timezone: 'Europe/Warsaw',
  units: [{ unit_id: 'unit-it', name: 'Dział IT', code: 'IT', type_id: 'ty-1', head_position_id: 'pos-1', deputy_head_position_ids: [] }],
  positions: [
    { position_id: 'pos-1', unit_id: 'unit-it', name: 'Kierownik IT', primary_parent_position_id: null, is_staff: false, valid_from: '2026-01-01' },
    { position_id: 'pos-2', unit_id: 'unit-it', name: 'Tester', primary_parent_position_id: 'pos-1', is_staff: false, valid_from: '2026-01-01' },
  ],
  assignments: [{ position_id: 'pos-1', subject: anna, display_name: 'Anna Nowak', share: 1 }],
  vacancies: ['pos-2'],
  warnings: [{ kind: 'unit_without_head', unit_id: 'unit-it', from: at }],
});

const answer = (at) => ({
  view: view(at),
  unit_types: [{ id: 'ty-1', name: 'Dział' }],
  my_permissions: ['org.admin'],
});

async function mountScreen(over = {}, params = {}) {
  // A test that failed before its own unmount must not leave its chart to the next one.
  OrgStructureScreen.unmount();
  calls.length = 0;
  navigated.length = 0;
  stubTransport({ orgStructureRequest: (p) => answer(p?.at ?? '2026-09-30'), ...over });
  document.body.innerHTML = '<div id="main"></div>';
  document.getElementById('main').innerHTML = OrgStructureScreen.render();
  await OrgStructureScreen.mount(params);
  for (let i = 0; i < 4; i += 1) await flush();
}

test('the tabs come in the mockups order and the whole strip is on screen', async () => {
  await mountScreen();

  const ids = [...document.querySelectorAll('#org-tabs tf-tab')].map((tab) => tab.id);
  assert.deepEqual(ids, ['tree', 'list', 'visibility', 'history', 'roles']);
  const labels = [...document.querySelectorAll('#org-tabs tf-tab')].map((tab) => tab.textContent.trim());
  assert.deepEqual(labels, ['tree', 'list', 'visibility', 'history', 'roles'].map((id) => t(`tab_${id}`)));

  OrgStructureScreen.unmount();
});

test('the tree tab draws the chart of what the wire returned, with its numbers and warnings', async () => {
  await mountScreen();

  assert.equal(calls.filter((c) => c.kind === 'orgStructureRequest').length, 1, 'one snapshot read');
  const chart = document.getElementById('org-tree');
  assert.deepEqual(chart.model.nodes.map((n) => [n.role, n.name, n.vacant]), [
    ['Kierownik IT', 'Anna Nowak', false],
    ['Tester', t('vacancy'), true],
  ]);
  assert.equal(chart.querySelectorAll('.ot-card').length, 2);
  const chips = [...document.querySelectorAll('#org-header-badges tf-chip')].map((c) => c.textContent.trim());
  assert.deepEqual(chips, [
    t('people_count', { count: 1 }),
    t('summary_units', { count: 1 }),
    t('vacancies_count', { count: 1 }),
    t('as_of_today', { date: formatDay('2026-09-30') }),
  ]);
  assert.equal(document.getElementById('org-header').getAttribute('subtitle'),
    ['Dział IT', t('people_count', { count: 1 }), t('summary_units', { count: 1 }), t('header_today')].join(' · '));
  assert.match(document.getElementById('org-tree-legend').textContent, /Dział IT/);
  assert.match(document.getElementById('org-panel-tree').textContent, /Dział IT/, 'the unit warning names the unit');

  OrgStructureScreen.unmount();
});

test('the list tab shows a row per holder and per vacancy, with the manager above each', async () => {
  await mountScreen({}, { tab: 'list' });

  assert.equal(document.getElementById('org-panel-tree').hidden, true);
  assert.equal(document.getElementById('org-panel-list').hidden, false);
  const rows = document.querySelector('.org-list-table').rows;
  assert.deepEqual(rows.map((r) => [r.person.value, r.reportsTo]), [
    ['Anna Nowak', '—'],
    [t('vacancy'), 'Anna Nowak'],
  ]);
  assert.equal(document.getElementById('org-list-actions').hidden, false);

  OrgStructureScreen.unmount();
});

test('an empty structure still opens the list with the import, so a structure can begin from a file', async () => {
  const empty = { view: { at: '2026-09-30', timezone: 'Europe/Warsaw', units: [], positions: [], assignments: [], vacancies: [], warnings: [] }, unit_types: [], my_permissions: ['org.admin'] };
  await mountScreen({ orgStructureRequest: empty }, { tab: 'list' });
  assert.ok(document.querySelector('.org-list-table'), 'the table is drawn, not an empty-state card');
  assert.equal(document.querySelector('#org-panel-list tf-empty-state'), null);
  assert.ok(document.querySelector('#org-list-actions [data-act="import"]'));
  OrgStructureScreen.unmount();
});

test('after an import the header and the list show the new structure', async () => {
  let applied = false;
  const bigger = () => {
    const a = answer('2026-09-30');
    if (applied) {
      a.view.positions.push({ position_id: 'pos-3', unit_id: 'unit-it', name: 'Analityk', primary_parent_position_id: 'pos-1', is_staff: false, valid_from: '2026-09-30' });
      a.view.vacancies.push('pos-3');
    }
    return a;
  };
  const report = { mode: 'upsert', as_of: '2026-09-30', preview_at: '2026-09-30', applied: false, counts: { rows: 3, added: 1, errors: 0 }, errors: [], rows: [] };
  await mountScreen({
    orgStructureRequest: bigger,
    orgImportDryRunRequest: { report },
    orgImportApplyRequest: () => { applied = true; return { report: { ...report, applied: true } }; },
  }, { tab: 'list' });
  assert.equal(document.querySelector('.org-list-table').rows.length, 2);

  document.querySelector('#org-list-actions [data-act="import"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  const win = document.querySelector('tf-window.org-imp-window');
  win.querySelector('[data-role="file"]').dispatchEvent(new window.CustomEvent('change', {
    bubbles: true, detail: { files: [{ name: 'a.csv', size: 3, arrayBuffer: async () => new Uint8Array([1]).buffer }] },
  }));
  for (let i = 0; i < 4; i += 1) await flush();
  win.querySelector('[data-act="apply"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  for (let i = 0; i < 6; i += 1) await flush();

  assert.equal(document.querySelector('.org-list-table').rows.length, 3, 'the list read the structure again');
  assert.ok(document.getElementById('org-header-badges').textContent.includes(t('vacancies_count', { count: 2 })), 'and so did the header');
  OrgStructureScreen.unmount();
});

test('an unknown tab in the address falls back to the tree', async () => {
  await mountScreen({}, { tab: 'nonsense' });
  assert.equal(document.getElementById('org-panel-tree').hidden, false);
  OrgStructureScreen.unmount();
});

test('the role catalog tab navigates to its own screen and other tabs switch in place', async () => {
  await mountScreen();
  const tabs = document.getElementById('org-tabs');

  tabs.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value: 'roles' } }));
  assert.deepEqual(navigated, [{ id: 'roles-catalog', params: undefined }]);

  tabs.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value: 'visibility' } }));
  assert.equal(document.getElementById('org-panel-visibility').hidden, false);
  // Widoczność is the inspector: an administrator gets the person picker.
  assert.match(document.getElementById('org-panel-visibility').textContent, new RegExp(t('visibility.pick_label')));
  assert.equal(document.getElementById('org-panel-tree').hidden, true);
  assert.equal(navigated.length, 1, 'switching inside the screen does not navigate');

  OrgStructureScreen.unmount();
});

test('a day picked in the history tab is what the server is asked for, with the differences from today', async () => {
  await mountScreen({
    authMeRequest: { userId: 'u-anna' },
    orgHistoryListRequest: { entries: [], total: 0, personal_visible: true, today: '2026-09-30' },
    orgChangeSetListRequest: { items: [], today: '2026-09-30' },
    orgHistoryDiffRequest: { items: [], personal_visible: true },
  }, { tab: 'history' });

  const input = document.querySelector('#org-panel-history [data-role="day"]');
  assert.equal(input.getAttribute('value'), '2026-09-30', 'starts on the structure of today');
  input.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value: '2026-01-15' } }));
  for (let i = 0; i < 4; i += 1) await flush();

  const asked = calls.filter((c) => c.kind === 'orgStructureRequest').map((c) => c.payload);
  assert.deepEqual(asked[asked.length - 1], { at: '2026-01-15' });
  const diffed = calls.filter((c) => c.kind === 'orgHistoryDiffRequest').map((c) => c.payload);
  assert.deepEqual(diffed[diffed.length - 1], { from: '2026-09-30', to: '2026-01-15' });
  assert.match(document.querySelector('#org-panel-history [data-role="big"]').textContent, /15\.01\.2026|2026/);

  const before = calls.filter((c) => c.kind === 'orgStructureRequest').length;
  input.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value: 'yesterday' } }));
  await flush();
  assert.equal(input.getAttribute('error'), I18n.t('org_structure.history.date_invalid', { format: 'DD.MM.RRRR' }));
  assert.equal(calls.filter((c) => c.kind === 'orgStructureRequest').length, before, 'a malformed day is not sent');

  OrgStructureScreen.unmount();
});

test('an empty structure says so, and a structure that never arrives is not drawn as zeros', async () => {
  await mountScreen({
    orgStructureRequest: { view: { at: '2026-09-30', timezone: 'Europe/Warsaw', units: [], positions: [], assignments: [], vacancies: [], warnings: [] }, unit_types: [], my_permissions: [] },
  });
  assert.equal(document.querySelector('#org-panel-tree tf-empty-state').getAttribute('title'), t('empty_title'));
  assert.equal(document.querySelector('#org-panel-tree tf-stat-card'), null);
  OrgStructureScreen.unmount();

  await mountScreen({ orgStructureRequest: () => { throw new Error('organization not found'); } });
  assert.match(document.getElementById('org-loading').textContent, /organization not found/);
  assert.equal(document.getElementById('org-panels').hidden, true);
  OrgStructureScreen.unmount();
});

// ---- Drzewo tab: the chart and its toolbar --------------------------------

const ME_BYTES = Uint8Array.from([0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00]);
const ME = { kind: 'user', id: '11223344-5566-7788-99aa-bbccddeeff00' };

const chartAnswer = () => {
  const position = (id, name, parent, unit = 'unit-it', extra = {}) => ({
    position_id: id, unit_id: unit, name, primary_parent_position_id: parent, is_staff: false, valid_from: '2026-01-01',
    functional_parent_position_ids: [], ...extra,
  });
  const holder = (positionId, subject, name) => ({ position_id: positionId, subject, display_name: name, share: 1, assignment_type: 'permanent', is_primary: true });
  return {
    view: {
      at: '2026-09-30',
      timezone: 'Europe/Warsaw',
      units: [{ unit_id: 'unit-it', name: 'Dział IT', code: 'IT', type_id: null, head_position_id: 'pos-cto', deputy_head_position_ids: [] }],
      positions: [
        position('pos-cto', 'Dyrektor IT', null),
        position('pos-dev-a', 'Developer', 'pos-cto'),
        position('pos-dev-b', 'Developer', 'pos-cto'),
        position('pos-qa', 'Tester', 'pos-dev-a', 'unit-it', { functional_parent_position_ids: ['pos-dev-b'] }),
      ],
      assignments: [
        holder('pos-cto', { kind: 'external', id: 'x-1' }, 'Zofia Żak'),
        holder('pos-dev-a', ME, 'Anna Nowak'),
        holder('pos-dev-b', { kind: 'external', id: 'x-2' }, 'Piotr Wójcik'),
      ],
      vacancies: ['pos-qa'],
      warnings: [],
    },
    unit_types: [],
    my_permissions: [],
  };
};

const chartFixtures = (over = {}) => ({ orgStructureRequest: chartAnswer(), authMeRequest: { userId: ME_BYTES }, ...over });
const strip = () => document.getElementById('org-tree-path');
const dispatch = (id, type, detail) => document.getElementById(id).dispatchEvent(new window.CustomEvent(type, { bubbles: true, detail }));
const chartCard = (id) => document.querySelector(`#org-tree .ot-card[data-node="${id}"]`);

test('search finds a person regardless of diacritics and shows the path from the root to them', async () => {
  await mountScreen(chartFixtures());
  dispatch('org-tree-search', 'search', { value: 'zak' });
  assert.equal(strip().hidden, false);

  dispatch('org-tree-search', 'search', { value: 'tester' });
  const chips = [...strip().querySelectorAll('.org-path-chip')];
  assert.deepEqual(chips.map((c) => c.dataset.node), ['pos-cto', 'pos-dev-a', 'pos-qa'], 'root first, the hit last');
  assert.match(chips[2].textContent, new RegExp(`Tester — ${t('vacancy')}`));
  assert.equal(strip().querySelector('.org-path-chip:last-of-type').getAttribute('status'), 'accent');
  const chart = document.getElementById('org-tree');
  chart._render();
  assert.match(chartCard('pos-qa').getAttribute('class'), /ot-path/);
  assert.match(chartCard('pos-cto').getAttribute('class'), /ot-match|ot-path/);
  assert.equal(strip().querySelector('.org-path-matches'), null, 'one hit needs no stepper');

  OrgStructureScreen.unmount();
});

test('typing a name and pressing Enter selects the hit: path bar and detail panel show like after a card click', async () => {
  await mountScreen(chartFixtures());
  const input = document.querySelector('#org-tree-search input');
  input.value = 'tester';
  input.dispatchEvent(new window.Event('input', { bubbles: true }));
  input.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
  // A real browser follows Enter in a type=search input with its own detail-less `search` event.
  input.dispatchEvent(new window.Event('search', { bubbles: true }));
  assert.equal(strip().hidden, false);
  assert.deepEqual([...strip().querySelectorAll('.org-path-chip')].map((c) => c.dataset.node), ['pos-cto', 'pos-dev-a', 'pos-qa']);
  assert.equal(document.getElementById('org-tree').selectedId, 'pos-qa', 'the hit is the selected card');
  OrgStructureScreen.unmount();
});

test('several hits get a stepper, and it walks them in turn', async () => {
  await mountScreen(chartFixtures());
  dispatch('org-tree-search', 'search', { value: 'developer' });
  const counter = () => strip().querySelector('.org-path-matches span').textContent.trim();
  assert.equal(counter(), t('match_counter', { current: 1, total: 2 }));
  const target = () => [...strip().querySelectorAll('.org-path-chip')].pop().dataset.node;
  const first = target();
  strip().querySelector('[data-act="match-next"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.equal(counter(), t('match_counter', { current: 2, total: 2 }));
  assert.notEqual(target(), first);
  strip().querySelector('[data-act="match-next"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.equal(counter(), t('match_counter', { current: 1, total: 2 }), 'wraps around');
  OrgStructureScreen.unmount();
});

test('a search with no hit says so and clearing it removes the strip', async () => {
  await mountScreen(chartFixtures());
  dispatch('org-tree-search', 'search', { value: 'nobody here' });
  assert.equal(strip().textContent.trim(), t('match_none'));
  dispatch('org-tree-search', 'search', { value: '' });
  assert.equal(strip().hidden, true);
  OrgStructureScreen.unmount();
});

test('"Moja pozycja" selects the seat of the caller and opens its details', async () => {
  await mountScreen(chartFixtures());
  assert.match(chartCard('pos-dev-a').textContent, new RegExp(t('badge_me')), 'the caller carries the badge');
  document.getElementById('org-tree-me').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  const chart = document.getElementById('org-tree');
  assert.equal(chart.selectedId, 'pos-dev-a');
  const panel = document.getElementById('org-tree-detail');
  assert.equal(panel.hidden, false);
  assert.match(panel.textContent, /Anna Nowak/);
  assert.deepEqual(document.getElementById('org-tree-detail-kv').entries.map((e) => [e.key, e.value]).slice(0, 4), [
    [t('col_position'), 'Developer'],
    [t('col_unit'), 'Dział IT'],
    [t('col_reports_to'), 'Zofia Żak'],
    [t('col_since'), formatDay('2026-01-01')],
  ]);
  OrgStructureScreen.unmount();
});

test('a person with no seat gets a notice instead of a jump', async () => {
  await mountScreen(chartFixtures({ authMeRequest: { userId: new Uint8Array(16) } }));
  document.getElementById('org-tree-me').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.equal(document.getElementById('org-tree').selectedId, null);
  assert.equal(document.getElementById('org-tree-detail').hidden, true);
  OrgStructureScreen.unmount();
});

test('the chart still opens when the session user cannot be read', async () => {
  await mountScreen({ orgStructureRequest: chartAnswer(), authMeRequest: () => { throw new Error('no session'); } });
  assert.ok(chartCard('pos-cto'));
  assert.equal(chartCard('pos-dev-a').textContent.includes(t('badge_me')), false);
  OrgStructureScreen.unmount();
});

test('selecting a card shows its details, a vacancy says it is vacant, and the panel closes', async () => {
  await mountScreen(chartFixtures());
  dispatch('org-tree', 'node-select', { id: 'pos-qa', kind: 'position' });
  const panel = document.getElementById('org-tree-detail');
  assert.equal(panel.hidden, false);
  const entries = document.getElementById('org-tree-detail-kv').entries;
  assert.deepEqual(entries.find((e) => e.key === t('detail_holder')).value, t('detail_vacant'));
  assert.equal(entries.find((e) => e.key === t('col_reports_to')).value, 'Anna Nowak');
  panel.querySelector('[data-act="detail-close"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.equal(panel.hidden, true);
  OrgStructureScreen.unmount();
});

const viewAction = (action) => {
  document.getElementById('org-tree-options').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  document.getElementById('org-tree-view-menu').dispatchEvent(new window.CustomEvent('action', { bubbles: true, detail: { action } }));
};

test('the units view is one click away and the functional lines menu entry switches off with it', async () => {
  await mountScreen(chartFixtures());
  const chart = document.getElementById('org-tree');
  const items = () => [...document.querySelectorAll('#org-tree-view-menu tf-menu-item')];
  viewAction('functional');
  assert.equal(chart.functional, true);
  document.getElementById('org-tree-options').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.equal(items()[1].getAttribute('icon'), 'check', 'the entry shows its state');
  dispatch('org-tree-mode', 'change', { value: 'units' });
  assert.equal(chart.mode, 'units');
  document.getElementById('org-tree-options').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.equal(items()[1].hasAttribute('disabled'), true, 'nothing to join in the units view');
  dispatch('org-tree-mode', 'change', { value: 'persons' });
  assert.equal(chart.mode, 'persons');
  OrgStructureScreen.unmount();
});

test('presentation pins the stage, enlarges the chart and leaves on the button and on Escape', async () => {
  await mountScreen(chartFixtures());
  const stage = document.getElementById('org-tree-stage');
  const chart = document.getElementById('org-tree');
  const width = chart._layout.byId.get('pos-cto').w;
  document.getElementById('org-tree-present').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  await flush();
  assert.equal(stage.classList.contains('org-present'), true);
  assert.equal(chart.presentation, true);
  assert.ok(chart._layout.byId.get('pos-cto').w > width);

  document.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Escape' }));
  await flush();
  assert.equal(stage.classList.contains('org-present'), false);
  assert.equal(chart.presentation, false);

  document.getElementById('org-tree-present').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  await flush();
  stage.querySelector('[data-act="present-exit"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  await flush();
  assert.equal(stage.classList.contains('org-present'), false);
  OrgStructureScreen.unmount();
  assert.equal(document.documentElement.classList.contains('org-presenting'), false);
});

test('the export menu saves an SVG of the whole chart', async () => {
  await mountScreen(chartFixtures());
  const saved = [];
  URL.createObjectURL = (blob) => { saved.push(blob); return 'blob:test'; };
  URL.revokeObjectURL = () => {};
  const menu = document.getElementById('org-tree-export-menu');
  document.getElementById('org-tree-export').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.equal(menu.hasAttribute('open'), true);
  menu.dispatchEvent(new window.CustomEvent('action', { bubbles: true, detail: { action: 'svg' } }));
  await flush();
  assert.equal(saved.length, 1);
  assert.equal(saved[0].type, 'image/svg+xml;charset=utf-8');
  const text = await saved[0].text();
  assert.match(text, /^<svg /);
  assert.ok(text.includes('Zofia Żak') && text.includes('Piotr Wójcik'), 'fully expanded, every card');
  OrgStructureScreen.unmount();
});

test('the tab keeps its chart while another tab is shown and reads the structure again on return', async () => {
  await mountScreen(chartFixtures());
  const chart = document.getElementById('org-tree');
  const tabs = document.getElementById('org-tabs');
  tabs.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value: 'list' } }));
  tabs.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value: 'tree' } }));
  for (let i = 0; i < 4; i += 1) await flush();
  assert.equal(document.getElementById('org-tree'), chart);
  assert.equal(calls.filter((c) => c.kind === 'orgStructureRequest').length, 2, 'once on load, once on return');
  OrgStructureScreen.unmount();
});

test('the PDF export prints a page for the whole structure and one per unit, each with the day in the footer', async () => {
  await mountScreen(chartFixtures());
  const menu = document.getElementById('org-tree-export-menu');
  menu.dispatchEvent(new window.CustomEvent('action', { bubbles: true, detail: { action: 'pdf' } }));
  const frame = document.querySelector('iframe.org-print-frame');
  assert.ok(frame, 'the print document is prepared');
  const html = frame.getAttribute('srcdoc') ?? frame.srcdoc;
  assert.equal((html.match(/<section class="page">/g) ?? []).length, 2, 'the overview and the one unit');
  assert.match(html, /<h1>Dział IT<\/h1>/);
  assert.equal((html.match(new RegExp(t('pdf_footer', { date: formatDay('2026-09-30') }), 'g')) ?? []).length, 2);
  frame.remove();
  OrgStructureScreen.unmount();
});

test('a structure that changed while another tab was shown is drawn when the tab returns, with the selection kept', async () => {
  let answer = chartAnswer();
  await mountScreen({ orgStructureRequest: () => answer, authMeRequest: { userId: ME_BYTES } });
  dispatch('org-tree', 'node-select', { id: 'pos-dev-b', kind: 'position' });
  document.getElementById('org-tree').selectedId = 'pos-dev-b';
  const tabs = document.getElementById('org-tabs');
  tabs.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value: 'list' } }));
  answer = chartAnswer();
  answer.view.positions.push({
    position_id: 'pos-new', unit_id: 'unit-it', name: 'Analityk', primary_parent_position_id: 'pos-cto', is_staff: false,
    valid_from: '2026-09-30', functional_parent_position_ids: [],
  });
  tabs.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value: 'tree' } }));
  for (let i = 0; i < 4; i += 1) await flush();
  const chart = document.getElementById('org-tree');
  assert.ok(chart.model.nodes.some((n) => n.id === 'pos-new'));
  assert.equal(chart.selectedId, 'pos-dev-b');
  assert.equal(document.getElementById('org-tree-detail').hidden, false);
  assert.match(document.getElementById('org-header-badges').textContent, /Analityk|\d/);
  OrgStructureScreen.unmount();
});

test('the header card names the structure and carries the actions of the Drzewo tab only', async () => {
  await mountScreen(chartFixtures());
  const actions = document.getElementById('org-header-actions');
  assert.deepEqual([...actions.querySelectorAll('tf-button')].map((b) => b.dataset.act), ['export', 'present']);
  assert.equal(actions.hidden, false);
  document.getElementById('org-tabs').dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value: 'list' } }));
  assert.equal(actions.hidden, true);
  OrgStructureScreen.unmount();
});

test('"Edytuj strukturę" needs a registered edit mode and org.admin, and opens it with the chart, the inspector slot and the model', async () => {
  const { registerEditEntry } = await import('./tree-tab.js');
  const { openEditMode } = await import('./edit-mode.js');
  const opened = [];
  registerEditEntry(null);
  await mountScreen(chartFixtures());
  assert.equal(document.querySelector('[data-act="edit"]'), null);
  OrgStructureScreen.unmount();

  registerEditEntry({
    open(api) {
      opened.push(Object.keys(api));
      return { dispose() {}, renderInspector() {}, actionsHtml: () => '<tf-button data-act="edit-exit">x</tf-button>', act() {}, canLeave: async () => true };
    },
  });
  await mountScreen(chartFixtures({ orgStructureRequest: { ...chartAnswer(), my_permissions: [] } }));
  assert.equal(document.querySelector('[data-act="edit"]'), null, 'a reader never sees it');
  OrgStructureScreen.unmount();

  await mountScreen(chartFixtures({ orgStructureRequest: { ...chartAnswer(), my_permissions: ['org.admin'] } }));
  const edit = document.querySelector('#org-header-actions [data-act="edit"]');
  assert.ok(edit);
  assert.equal(edit.hasAttribute('disabled'), false);
  edit.dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.equal(opened.length, 1);
  for (const key of ['chart', 'detail', 'top', 'below', 'data', 'selection', 'select', 'apply', 'exit', 'renderActions', 'notifyChanged']) {
    assert.ok(opened[0].includes(key), `the edit mode is given ${key}`);
  }
  assert.ok(document.querySelector('#org-header-actions [data-act="edit-exit"]'), 'the header offers to finish editing');
  assert.equal(document.querySelector('#org-header-actions [data-act="edit"]'), null);
  OrgStructureScreen.unmount();
  registerEditEntry({ open: openEditMode });
});

test('"/" reaches the search box from anywhere on the tab, but not while typing elsewhere', async () => {
  await mountScreen(chartFixtures());
  const input = document.querySelector('#org-tree-search input');
  let focused = 0;
  input.focus = () => { focused += 1; };
  document.body.dispatchEvent(new window.KeyboardEvent('keydown', { key: '/', bubbles: true, cancelable: true }));
  assert.equal(focused, 1);
  input.dispatchEvent(new window.KeyboardEvent('keydown', { key: '/', bubbles: true, cancelable: true }));
  assert.equal(focused, 1, 'typing a slash into a field is left alone');
  document.getElementById('org-tabs').dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value: 'list' } }));
  document.body.dispatchEvent(new window.KeyboardEvent('keydown', { key: '/', bubbles: true, cancelable: true }));
  assert.equal(focused, 1, 'not while another tab is showing');
  OrgStructureScreen.unmount();
});

test('the export menu offers the branch only when something is selected, and PDF in A4 and A3', async () => {
  await mountScreen(chartFixtures());
  const menu = document.getElementById('org-tree-export-menu');
  const actions = () => [...menu.querySelectorAll('tf-menu-item')].map((i) => i.getAttribute('action'));
  document.getElementById('org-tree-export').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.deepEqual(actions(), ['svg', 'png', 'pdf-a4', 'pdf-a3']);
  dispatch('org-tree', 'node-select', { id: 'pos-dev-a', kind: 'position' });
  document.getElementById('org-tree-export').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.deepEqual(actions(), ['svg', 'png', 'pdf-a4', 'pdf-a3', 'branch-svg', 'branch-png', 'branch-pdf-a4']);

  const saved = [];
  URL.createObjectURL = (blob) => { saved.push(blob); return 'blob:test'; };
  URL.revokeObjectURL = () => {};
  menu.dispatchEvent(new window.CustomEvent('action', { bubbles: true, detail: { action: 'branch-svg' } }));
  await flush();
  const text = await saved[0].text();
  assert.ok(text.includes('Anna Nowak') && text.includes('Tester'));
  assert.equal(text.includes('Piotr Wójcik'), false, 'only the branch of the selected position');
  menu.dispatchEvent(new window.CustomEvent('action', { bubbles: true, detail: { action: 'pdf-a3' } }));
  const frame = document.querySelector('iframe.org-print-frame');
  assert.match(frame.getAttribute('srcdoc') ?? frame.srcdoc, /@page\{size:A3 landscape/);
  frame.remove();
  OrgStructureScreen.unmount();
});

test('the horizontal layout is an entry of the view menu', async () => {
  await mountScreen(chartFixtures());
  const chart = document.getElementById('org-tree');
  viewAction('horizontal');
  assert.equal(chart.horizontal, true);
  const cto = chart._layout.byId.get('pos-cto');
  const dev = chart._layout.byId.get('pos-dev-a');
  assert.ok(dev.x > cto.x + cto.w, 'the level runs along x');
  viewAction('horizontal');
  assert.ok(chart._layout.byId.get('pos-dev-a').y > chart._layout.byId.get('pos-cto').y);
  OrgStructureScreen.unmount();
});

// ----- Do przekazania (handover) ------------------------------------------------

const hv = (key, params) => I18n.t(`org_structure.handover.${key}`, params);

const pendingPeople = [
  { user_id: 'u-pz', display_name: 'Piotr Zieliński', count: 4, ended_on: '2026-09-25' },
  { user_id: 'u-ew', display_name: 'Ewa Wiśniewska', count: 1, ended_on: '2026-09-20' },
];

test('an administrator sees the counter of people who still hold work and it opens the handover screen', async () => {
  await mountScreen({ orgHandoverPendingRequest: { people: [pendingPeople[0]] } });
  const button = document.querySelector('#org-header-badges [data-act="handover-pending"]');
  assert.ok(button, 'the counter is in the header');
  assert.equal(button.textContent.trim(), hv('pending_button', { count: 1 }));
  button.dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.deepEqual(navigated, [{ id: 'org-structure', params: { tab: 'list', handover: 'u-pz', reason: 'departure' } }]);
  OrgStructureScreen.unmount();
});

test('with several people the counter offers each of them with how much they still hold', async () => {
  await mountScreen({ orgHandoverPendingRequest: { people: pendingPeople } });
  document.querySelector('[data-act="handover-pending"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  // The chart owns a tf-menu of its own (export); the counter's menu is the one opened last.
  const menu = [...document.querySelectorAll('tf-menu')].pop();
  const labels = [...menu.querySelectorAll('tf-menu-item')].map((i) => i.getAttribute('label'));
  assert.deepEqual(labels, pendingPeople.map((p) => hv('pending_item', { name: p.display_name, count: p.count })));
  OrgStructureScreen.unmount();
});

test('nobody pending, no counter; a member never asks the server for the list', async () => {
  await mountScreen({ orgHandoverPendingRequest: { people: [] } });
  assert.equal(document.querySelector('[data-act="handover-pending"]'), null);
  OrgStructureScreen.unmount();
  await mountScreen({ orgStructureRequest: () => ({ ...answer('2026-09-30'), my_permissions: [] }) });
  assert.equal(calls.filter((c) => c.kind === 'orgHandoverPendingRequest').length, 0);
  OrgStructureScreen.unmount();
});

test('route parameters open the handover screen in place of the list', async () => {
  const listing = {
    user: { user_id: 'u-pz', display_name: 'Piotr Zieliński' }, reason: 'departure', date: '2026-09-30', assignment_ended_on: null,
    groups: [], takers: [],
  };
  await mountScreen(
    { orgHandoverPendingRequest: { people: [] }, orgHandoverListRequest: listing },
    { tab: 'list', handover: 'u-pz', reason: 'departure' },
  );
  assert.equal(document.getElementById('org-panel-handover').hidden, false);
  assert.equal(document.getElementById('org-panel-list').hidden, true);
  assert.equal(document.getElementById('org-tabs').getAttribute('value'), 'list');
  assert.deepEqual(calls.find((c) => c.kind === 'orgHandoverListRequest').payload, {
    userId: 'u-pz', reason: 'departure', projectId: null, date: null,
  });
  assert.match(document.querySelector('.org-ho-title h2').textContent, /Piotr Zieliński/);
  // "Wróć do listy" navigates back to the list of the same screen.
  document.querySelector('[data-act="back"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.deepEqual(navigated.at(-1), { id: 'org-structure', params: { tab: 'list' } });
  OrgStructureScreen.unmount();
});

test('without those parameters the list is shown and the handover panel stays empty', async () => {
  await mountScreen({ orgHandoverPendingRequest: { people: [] } }, { tab: 'list' });
  assert.equal(document.getElementById('org-panel-handover').hidden, true);
  assert.equal(document.getElementById('org-panel-list').hidden, false);
  assert.equal(calls.filter((c) => c.kind === 'orgHandoverListRequest').length, 0);
  OrgStructureScreen.unmount();
});

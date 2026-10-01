// =============================================================================
// File: modules/org-structure/history-tab.test.js
// Description: The Historia tab against a stubbed transport: the timeline draws
//   what the server listed (before → after, planned ones marked), a day picked
//   asks for that day and its differences from today, the privacy note and the
//   missing reorganizations for anyone but an administrator, and every action of
//   a planned reorganization sends its own request — approve (with the typed
//   refusal shown in the window that stays open, and the author's button off),
//   submit, withdraw, edit (the edit mode when one is registered, a basic window
//   when not) and a new one. Runs under happy-dom.
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
const { mountHistoryTab, unmountHistoryTab } = await import('./history-tab.js');
const { registerChangeSetEditor } = await import('./history-bridge.js');

const TODAY = '2026-09-30';
const ME = '11111111-1111-1111-1111-111111111111';
const OTHER = '22222222-2222-2222-2222-222222222222';
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const settle = async () => { for (let i = 0; i < 8; i += 1) await sleep(0); };
const t = (key, params) => I18n.t(`org_structure.history.${key}`, params);
const fire = (target, type, detail) => target.dispatchEvent(new window.CustomEvent(type, { bubbles: true, detail }));
const q = (sel) => document.querySelector(`#host ${sel}`);
const all = (sel) => [...document.querySelectorAll(`#host ${sel}`)];
const windowEl = () => [...document.querySelectorAll('tf-window')].at(-1) ?? null;

const view = (at) => ({
  at,
  timezone: 'Europe/Warsaw',
  units: [
    { unit_id: 'u-board', name: 'Zarząd', parent_unit_id: null, head_position_id: 'p-ceo', deputy_head_position_ids: [] },
    { unit_id: 'u-it', name: 'Dział IT', parent_unit_id: 'u-board', head_position_id: 'p-cto', deputy_head_position_ids: [] },
  ],
  positions: [
    { position_id: 'p-ceo', unit_id: 'u-board', name: 'Prezes', primary_parent_position_id: null, is_staff: false, valid_from: '2026-01-01' },
    { position_id: 'p-cto', unit_id: 'u-it', name: 'CTO', primary_parent_position_id: 'p-ceo', is_staff: false, valid_from: '2026-01-01' },
  ],
  assignments: [{ position_id: 'p-ceo', subject: { kind: 'user', id: ME }, display_name: 'Anna Nowak', share: 1, assignment_type: 'permanent', is_primary: true }],
  vacancies: ['p-cto'],
  warnings: [],
});

const entry = (over) => ({
  id: 1, at: '2026-09-29 10:00:00', actor_name: 'Hanna', action: 'org.unit.move', target_kind: 'unit', target_id: 'u-it',
  target_name: 'Dział IT', unit_id: 'u-it', unit_name: 'Dział IT', effective_date: '2026-07-01',
  changes: [{ field: 'parent_unit_id', before: 'u-x', after: 'u-board', before_label: 'Realizacja', after_label: 'Zarząd' }],
  ...over,
});

const changeSet = (over) => ({
  id: 'cs-1', name: 'Reorganizacja Q4', effective_date: '2026-11-01', state: 'pending', author_user_id: OTHER, author_name: 'Hanna',
  approver_user_id: null, approver_name: null, created_at_ms: 1, op_count: 2, ops: [], ...over,
});

const previewOf = (over = {}) => ({
  ok: true, valid: true, results: [], warnings: [], at: '2026-11-01',
  live: view('2026-11-01'), preview: view('2026-11-01'),
  items: [
    { change: 'added', entity: 'unit', id: 'u-q', name: 'Zespół Jakości', unit_id: 'u-q' },
    { change: 'changed', entity: 'position', id: 'p-cto', name: 'CTO', unit_id: 'u-it', field: 'primary_parent_position_id', before: 'p-ceo', after: null, before_label: 'Anna Nowak' },
  ],
  change_set: changeSet(), error: null,
  ...over,
});

let requests;
let world;

function stubTransport() {
  requests = [];
  ApiBinary.one = async (kind, payload) => {
    requests.push({ kind, payload });
    const answer = world.answers[kind];
    if (answer === undefined) throw new Error(`unexpected request ${kind}`);
    return typeof answer === 'function' ? answer(payload) : answer;
  };
}

const sent = (kind) => requests.filter((r) => r.kind === kind).map((r) => r.payload);

beforeEach(() => {
  world = {
    answers: {
      authMeRequest: { userId: ME, username: 'anna' },
      orgStructureRequest: (p) => ({ view: view(p?.at ?? TODAY), unit_types: [], my_permissions: ['org.admin'] }),
      orgHistoryListRequest: {
        entries: [
          entry({ id: 3, effective_date: '2026-11-01', action: 'org.structure.batch', target_kind: 'structure', target_name: null, changes: [], source: 'batch', ops: [{ action: 'org.unit.create', target_kind: 'unit', target_name: 'Zespół Jakości' }] }),
          entry({ id: 2 }),
        ],
        total: 2, personal_visible: true, today: TODAY,
      },
      orgHistoryDiffRequest: { items: [], personal_visible: true, from: TODAY, to: TODAY },
      orgChangeSetListRequest: { items: [changeSet()], today: TODAY },
      orgChangeSetPreviewRequest: previewOf(),
    },
  };
});

async function mount({ admin = true } = {}) {
  unmountHistoryTab();
  registerChangeSetEditor(null);
  stubTransport();
  document.body.innerHTML = '<div id="host"></div>';
  await mountHistoryTab(document.getElementById('host'), {
    view: view(TODAY), unitTypes: [], myPermissions: admin ? ['org.admin'] : [],
  }, { reload: async () => {} });
  await settle();
  await sleep(20);
  await settle();
}

const confirm = async () => {
  windowEl().querySelector('[data-act="submit"]').click();
  await sleep(30);
  await settle();
};

test('the timeline draws what the server listed, planned changes marked, each with its before and after', async () => {
  await mount();
  const items = all('.org-hist-item');
  assert.equal(items.length, 2);
  assert.match(items[0].textContent, /01\.11\.2026/);
  assert.match(items[0].textContent, new RegExp(t('planned_tag')));
  assert.match(items[0].textContent, /Zespół Jakości/, 'the operations of a batch are listed');
  assert.match(items[1].textContent, /Realizacja/);
  assert.match(items[1].textContent, /Zarząd/);
  assert.match(q('[data-role="count"]').textContent, /2 zmiany/);
  assert.deepEqual(sent('orgHistoryListRequest').find((p) => p.limit === 30), { from: null, to: null, unitId: null, offset: 0, limit: 30 });
});

test('a day picked, or a change clicked, asks for that day and what differs from today', async () => {
  await mount();
  world.answers.orgHistoryDiffRequest = {
    items: [{ change: 'added', entity: 'unit', id: 'u-q', name: 'Zespół Jakości', unit_id: 'u-q' }], personal_visible: true,
  };
  q('[data-act="jump"]').click();
  await settle();
  assert.deepEqual(sent('orgStructureRequest').at(-1), { at: '2026-11-01' });
  assert.deepEqual(sent('orgHistoryDiffRequest').at(-1), { from: TODAY, to: '2026-11-01' });
  assert.equal(q('[data-role="big"]').textContent, '01.11.2026');
  assert.match(q('[data-role="stats"]').textContent, /dodano: 1/);
  assert.match(q('[data-role="relative"]').textContent, /za 32 dni/);

  const before = sent('orgStructureRequest').length;
  fire(q('[data-role="day"]'), 'change', 'not a day');
  await settle();
  assert.equal(q('[data-role="day"]').getAttribute('error'), t('date_invalid', { format: 'DD.MM.RRRR' }));
  assert.equal(sent('orgStructureRequest').length, before, 'a malformed day is not sent');
});

test('for anyone but an administrator the tab says what is left out and draws no reorganizations', async () => {
  world.answers.orgHistoryListRequest = { entries: [entry()], total: 1, personal_visible: false, today: TODAY };
  await mount({ admin: false });
  assert.match(q('[data-role="privacy"]').textContent, /tylko dla administratorów/);
  assert.equal(q('[data-role="reorg"]'), null);
  assert.equal(q('[data-act="new-reorg"]'), null);
  assert.equal(sent('orgChangeSetListRequest').length, 0, 'nothing of the plans is even asked for');
});

test('a reorganization card lists what it would change and who has to approve it', async () => {
  await mount();
  const card = q('.org-reorg');
  assert.match(card.textContent, /Reorganizacja Q4/);
  assert.match(card.textContent, /Zespół Jakości/);
  assert.match(card.textContent, /Anna Nowak/, 'the person a line was held by');
  assert.match(card.querySelector('.org-reorg-approval').textContent, /Hanna/);
  assert.equal(card.querySelector('[data-act="approve"]').hasAttribute('disabled'), false);
  assert.deepEqual(sent('orgChangeSetPreviewRequest').at(-1), { id: 'cs-1', unitId: null });
});

test('the author gets the approval button off, with the reason, and no request', async () => {
  world.answers.orgChangeSetListRequest = { items: [changeSet({ author_user_id: ME })], today: TODAY };
  await mount();
  const approve = q('.org-reorg [data-act="approve"]');
  assert.equal(approve.hasAttribute('disabled'), true);
  assert.equal(approve.getAttribute('title'), t('btn_approve_blocked'));
  approve.click();
  await settle();
  assert.equal(windowEl(), null);
  assert.equal(sent('orgChangeSetApproveRequest').length, 0);
});

test('approving asks, sends the approval, and says when it takes effect', async () => {
  await mount();
  world.answers.orgChangeSetApproveRequest = { ok: true, change_set: changeSet({ state: 'applied', approver_name: 'Anna' }), valid: true, results: [], warnings: [] };
  world.answers.orgChangeSetListRequest = { items: [changeSet({ state: 'applied', approver_name: 'Anna' })], today: TODAY };
  q('.org-reorg [data-act="approve"]').click();
  await settle();
  assert.equal(sent('orgChangeSetApproveRequest').length, 0, 'nothing before the confirmation');
  assert.match(windowEl().textContent, /01\.11\.2026/);
  await confirm();
  assert.deepEqual(sent('orgChangeSetApproveRequest'), [{ id: 'cs-1' }]);
  await sleep(350);
  assert.equal(windowEl()?.isConnected ?? false, false, 'the window closed');
  assert.equal(q('.org-reorg [data-act="approve"]'), null, 'an applied reorganization is closed');
});

test('a refused approval stays in the window with its own sentence and changes nothing', async () => {
  await mount();
  world.answers.orgChangeSetApproveRequest = {
    ok: false, error: { code: 'change_set_conflict', message: 'x' }, change_set: changeSet(), valid: false,
    results: [{ index: 1, ok: false, error: { code: 'not_valid_at', message: 'm', date: '2026-11-01' } }], warnings: [],
  };
  q('.org-reorg [data-act="approve"]').click();
  await settle();
  await confirm();
  const win = windowEl();
  assert.ok(win?.isConnected, 'the window stays open');
  assert.equal(win.querySelector('.tf-act__error').getAttribute('message'), t('error.change_set_conflict'));
  assert.ok(q('.org-reorg [data-act="approve"]'), 'the card still offers it');
});

test('submitting a draft and withdrawing a reorganization each send their request', async () => {
  world.answers.orgChangeSetListRequest = { items: [changeSet({ state: 'draft', author_user_id: ME })], today: TODAY };
  await mount();
  world.answers.orgChangeSetSubmitRequest = { ok: true, change_set: changeSet({ state: 'pending' }), valid: true, results: [], warnings: [] };
  q('.org-reorg [data-act="submit"]').click();
  await settle();
  assert.deepEqual(sent('orgChangeSetSubmitRequest'), [{ id: 'cs-1' }]);

  world.answers.orgChangeSetWithdrawRequest = { ok: true, change_set: changeSet({ state: 'withdrawn' }), valid: false, results: [], warnings: [] };
  world.answers.orgChangeSetListRequest = { items: [changeSet({ state: 'withdrawn' })], today: TODAY };
  q('.org-reorg [data-act="withdraw"]').click();
  await settle();
  assert.equal(sent('orgChangeSetWithdrawRequest').length, 0, 'asks first');
  assert.match(windowEl().textContent, new RegExp(t('withdraw_consequence').slice(0, 30)));
  await confirm();
  assert.deepEqual(sent('orgChangeSetWithdrawRequest'), [{ id: 'cs-1' }]);
});

const stored = [
  { temp_id: 'tmp:q', request: { UnitCreateRequest: { name: 'Zespół Jakości', valid_from: '2026-11-01' } } },
  { temp_id: null, request: { PositionMoveRequest: { position_id: 'p-cto', new_parent_position_id: null, from: '2026-11-01' } } },
];

test('"Edytuj w trybie edycji" hands the reorganization to the edit mode when there is one', async () => {
  await mount();
  world.answers.orgChangeSetGetRequest = { ok: true, change_set: changeSet({ ops: stored }), valid: false, results: [], warnings: [] };
  const opened = [];
  registerChangeSetEditor({ open: async (request) => { opened.push(request); return true; } });
  q('.org-reorg [data-act="edit"]').click();
  await settle();
  assert.equal(windowEl(), null, 'no window of its own when the edit mode took it');
  assert.equal(opened.length, 1);
  assert.equal(opened[0].id, 'cs-1');
  assert.equal(opened[0].effectiveDate, '2026-11-01');
  assert.equal(opened[0].ops[0].kind, 'unitCreate', 'the operations arrive in the edit mode\'s shape');
  assert.equal(opened[0].ops[0].validFrom, '2026-11-01');
});

test('without an edit mode to take it the reorganization opens in a basic editor that can drop a change and save', async () => {
  await mount();
  world.answers.orgChangeSetGetRequest = { ok: true, change_set: changeSet({ ops: stored }), valid: false, results: [], warnings: [] };
  world.answers.orgChangeSetSaveRequest = { ok: true, change_set: changeSet({ state: 'draft' }), valid: true, results: [], warnings: [] };
  q('.org-reorg [data-act="edit"]').click();
  await settle();
  const win = windowEl();
  assert.equal(win.querySelectorAll('.org-reorg-op').length, 2);
  win.querySelector('[data-drop="0"]').click();
  assert.equal(win.querySelectorAll('.org-reorg-op').length, 1);
  await confirm();
  const [save] = sent('orgChangeSetSaveRequest');
  assert.equal(save.id, 'cs-1');
  assert.equal(save.effectiveDate, '2026-11-01');
  assert.deepEqual(save.ops.map((op) => op.kind), ['positionMove']);
});

test('a new reorganization is a name and a day; the draft goes to the edit mode when there is one', async () => {
  await mount();
  const opened = [];
  registerChangeSetEditor({ open: async (request) => { opened.push(request); return true; } });
  world.answers.orgChangeSetSaveRequest = { ok: true, change_set: changeSet({ id: 'cs-2', name: 'Nowa', state: 'draft', ops: [] }), valid: true, results: [], warnings: [] };
  q('[data-act="new-reorg"]').click();
  await settle();
  await confirm();
  assert.equal(windowEl().querySelector('tf-input').getAttribute('error'), t('error.empty_field'), 'a name is required');
  assert.equal(sent('orgChangeSetSaveRequest').length, 0);

  const inputs = [...windowEl().querySelectorAll('tf-input')];
  inputs[0].setAttribute('value', 'Nowa');
  await confirm();
  const [save] = sent('orgChangeSetSaveRequest');
  assert.equal(save.id, null);
  assert.equal(save.name, 'Nowa');
  assert.deepEqual(save.ops, []);
  assert.match(save.effectiveDate, /^\d{4}-\d{2}-\d{2}$/);
  assert.equal(opened.length, 1);
  assert.equal(opened[0].id, 'cs-2');
});

test('"Przed i po" draws the reorganization\'s two states and its differences', async () => {
  await mount();
  q('.org-reorg [data-act="compare"]').click();
  await settle();
  await sleep(20);
  assert.match(q('[data-role="compare"] [data-role="title"]').textContent, /Przed i po — /);
  assert.equal(all('[data-role="compare"] tf-org-tree').length, 2);
  assert.match(q('[data-role="after-label"]').textContent, /01\.11\.2026/);
  assert.equal(q('[data-role="big"]').textContent, '01.11.2026', 'the day of the reorganization is shown');
  assert.equal(sent('orgHistoryDiffRequest').length > 0, true);
});

test('leaving the tab leaves nothing behind', async () => {
  await mount();
  unmountHistoryTab();
  assert.equal(document.getElementById('host').children.length, 0);
});

test('an approved reorganization whose day has not come can still be withdrawn, and the window says what that undoes', async () => {
  world.answers.orgChangeSetListRequest = { items: [changeSet({ state: 'applied', approver_name: 'Anna' })], today: TODAY };
  await mount();
  world.answers.orgChangeSetWithdrawRequest = { ok: true, change_set: changeSet({ state: 'withdrawn' }), valid: false, results: [], warnings: [] };
  q('.org-reorg [data-act="withdraw"]').click();
  await settle();
  assert.equal(windowEl().querySelector('.tf-act__note').getAttribute('message'), t('withdraw_applied_consequence'));
  await confirm();
  assert.deepEqual(sent('orgChangeSetWithdrawRequest'), [{ id: 'cs-1' }]);
});

test('a withdrawal the server refuses because later changes lean on the plan stays in the window with its reason', async () => {
  world.answers.orgChangeSetListRequest = { items: [changeSet({ state: 'applied' })], today: TODAY };
  await mount();
  world.answers.orgChangeSetWithdrawRequest = { ok: false, error: { code: 'change_set_dependents', message: 'x' }, change_set: changeSet({ state: 'applied' }), valid: false, results: [], warnings: [] };
  q('.org-reorg [data-act="withdraw"]').click();
  await settle();
  await confirm();
  assert.equal(windowEl().querySelector('.tf-act__error').getAttribute('message'), t('error.change_set_dependents'));
});

test('the only administrator gets the approval button on their own plan, with the note that says why', async () => {
  world.answers.orgChangeSetListRequest = { items: [changeSet({ author_user_id: ME })], today: TODAY, sole_admin: true };
  await mount();
  const approve = q('.org-reorg [data-act="approve"]');
  assert.equal(approve.hasAttribute('disabled'), false);
  assert.match(q('.org-reorg').textContent, new RegExp(t('sole_admin_note')));
  assert.doesNotMatch(q('.org-reorg').textContent, new RegExp(t('appr_pending')), 'nobody else is to decide');
});

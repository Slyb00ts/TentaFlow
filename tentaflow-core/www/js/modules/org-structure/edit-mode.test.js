// =============================================================================
// File: modules/org-structure/edit-mode.test.js
// Description: The edit mode on the real Drzewo tab against a stubbed transport that
//   answers `orgBatchRequest` like the server (per operation, with the structure
//   the batch leaves as the preview): who gets it, that every action goes into a
//   DRAFT and the canvas draws its preview while nothing is applied, the
//   confirmation with the count of people a move touches, a cycle refused on the
//   client and by the server, a person dropped on a vacancy, the inspector's
//   fields folding into single rows, the list of pending changes with "Cofnij",
//   undo/redo, saving as one dry run and one apply, discarding, the questions
//   asked before unsaved changes are lost, the effective day (planned, backdated,
//   saved as a reorganization), the "Stan na dziś" switch, templates as draft
//   operations, menus per kind of card, and a reorganization opened from Historia.
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
const { formatDay, dateFormatHint } = await import('/js/lib/date-format.js');
const { Router } = await import('../../router.js');
const { default: OrgStructureScreen } = await import('./index.js');
const { sampleView } = await import('./tree-fixture.js');

Router.replaceParams = () => {};
Router.navigate = async () => true;

const TODAY = '2026-09-30';
const t = (key, params) => I18n.t(`org_structure.edit.${key}`, params);
const base = (key, params) => I18n.t(`org_structure.${key}`, params);
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const settle = async () => { for (let i = 0; i < 6; i += 1) await sleep(0); };
const $ = (id) => document.getElementById(id);
const fire = (target, type, detail, init = {}) => target.dispatchEvent(new window.CustomEvent(type, { bubbles: true, detail, ...init }));

let requests;
let world;

// The server's side of a batch, for the operations these tests use.
function runBatch(payload) {
  const view = structuredClone(world.view);
  const map = new Map();
  const real = (id) => map.get(id) ?? id;
  let latest = null;
  const results = payload.ops.map((op, index) => {
    const day = op.from ?? op.validFrom;
    if (day && (!latest || day > latest)) latest = day;
    const error = world.refuse?.(op, index, payload) ?? null;
    if (error) return { index, ok: false, error };
    const result = { index, ok: true, error: null, temp_id: op.tempId ?? null, created_id: null };
    const unit = (id) => view.units.find((u) => u.unit_id === real(id));
    const position = (id) => view.positions.find((p) => p.position_id === real(id));
    switch (op.kind) {
      case 'positionMove': position(op.positionId).primary_parent_position_id = op.newParentPositionId ? real(op.newParentPositionId) : null; break;
      case 'unitMove': unit(op.unitId).parent_unit_id = op.newParentUnitId ? real(op.newParentUnitId) : null; break;
      case 'positionUpdate': if (op.name) position(op.positionId).name = op.name; if ('isStaff' in op) position(op.positionId).is_staff = op.isStaff; break;
      case 'unitUpdate': if (op.name) unit(op.unitId).name = op.name; break;
      case 'headSet': unit(op.unitId).head_position_id = op.headPositionId ? real(op.headPositionId) : null; break;
      case 'deputyHeadsSet': unit(op.unitId).deputy_head_position_ids = op.positionIds.map(real); break;
      case 'unitCreate': {
        const id = `real-${world.counter += 1}`;
        map.set(op.tempId, id);
        result.created_id = id;
        view.units.push({ id: `v-${id}`, unit_id: id, name: op.name, code: op.code, type_id: null, parent_unit_id: op.parentUnitId ? real(op.parentUnitId) : null, color: null, head_position_id: null, deputy_head_position_ids: [], valid_from: day });
        break;
      }
      case 'positionCreate': {
        const id = `real-${world.counter += 1}`;
        map.set(op.tempId, id);
        result.created_id = id;
        view.positions.push({ id: `v-${id}`, position_id: id, unit_id: real(op.unitId), name: op.name, code: op.code, primary_parent_position_id: op.parentPositionId ? real(op.parentPositionId) : null, functional_parent_position_ids: [], is_staff: op.isStaff, valid_from: day });
        view.vacancies.push(id);
        break;
      }
      case 'externalPersonCreate': {
        const id = `real-${world.counter += 1}`;
        map.set(op.tempId, id);
        result.created_id = id;
        world.externals.set(id, op.displayName);
        break;
      }
      case 'assign': {
        const id = `real-${world.counter += 1}`;
        map.set(op.tempId, id);
        result.created_id = id;
        const subject = { kind: op.subject.kind, id: real(op.subject.id) };
        const name = subject.kind === 'user' ? world.users.find((u) => u.id === subject.id)?.displayName : world.externals.get(subject.id);
        view.assignments.push({ id, position_id: real(op.positionId), subject, assignment_type: op.assignmentType, share: op.share, is_primary: true, valid_from: day, display_name: name ?? '?' });
        view.vacancies = view.vacancies.filter((v) => v !== real(op.positionId));
        break;
      }
      case 'assignmentEnd': view.assignments = view.assignments.filter((a) => a.id !== real(op.assignmentId)); break;
      case 'unitEnd': view.units = view.units.filter((u) => u.unit_id !== real(op.unitId)); break;
      case 'assignmentUpdate': Object.assign(view.assignments.find((a) => a.id === real(op.assignmentId)), Object.fromEntries(['share', 'assignmentType'].filter((k) => k in op).map((k) => [k === 'assignmentType' ? 'assignment_type' : k, op[k]]))); break;
      default:
    }
    return result;
  });
  const ok = results.every((r) => r.ok);
  view.at = latest ?? TODAY;
  if (ok && !payload.dryRun) world.view = view;
  return {
    ok, applied: ok && !payload.dryRun, error: null, results, warnings: view.warnings ?? [], preview_at: view.at, preview: payload.dryRun ? view : null, max_ops: 500,
  };
}

function stubTransport() {
  requests = [];
  ApiBinary.one = async (kind, payload) => {
    requests.push({ kind, payload });
    if (kind === 'authMeRequest') return { userId: new Uint8Array(16), username: 'admin' };
    if (kind === 'orgStructureRequest') {
      return { view: { ...structuredClone(world.view), at: payload?.at ?? TODAY }, unit_types: [{ id: 'ty-1', name: 'Dział' }], my_permissions: world.permissions };
    }
    if (kind === 'orgBatchRequest') return runBatch(payload);
    const answer = world.answers[kind];
    const result = typeof answer === 'function' ? answer(payload) : answer;
    return result ?? { ok: true, warnings: [], result: { kind: 'done' } };
  };
  ApiBinary.action = async (kind) => {
    if (kind === 'iamListUsersRequest') return { users: world.users };
    if (kind === 'roleCatalogListRequest') return { roles: [{ id: 'role-1', slug: 'qa', nameTranslations: [['pl', 'Jakość']] }] };
    return {};
  };
}

const batches = () => requests.filter((r) => r.kind === 'orgBatchRequest');
const dryRuns = () => batches().filter((r) => r.payload.dryRun);
const applies = () => batches().filter((r) => !r.payload.dryRun);
const lastDry = () => dryRuns().at(-1);
const draftOps = () => lastDry()?.payload.ops ?? [];

function freshWorld(over = {}) {
  const view = sampleView();
  // A second deputy of Technologia, so the order of deputies means something.
  view.positions.push({
    id: 'v-p-cto-deputy-2', position_id: 'p-cto-deputy-2', unit_id: 'u-tech', name: 'Zastępca do spraw sprzedaży',
    primary_parent_position_id: 'p-cto', functional_parent_position_ids: [], is_staff: false, valid_from: '2026-01-01',
  });
  view.units.find((u) => u.unit_id === 'u-tech').deputy_head_position_ids = ['p-cto-deputy', 'p-cto-deputy-2'];
  view.vacancies.push('p-cto-deputy-2');
  return {
    view,
    permissions: ['org.admin'],
    users: [
      { id: 'u-free', displayName: 'Rafał Kowal', email: 'rafal@example.org', isActive: true },
      { id: 'u-off', displayName: 'Nieaktywny', email: '', isActive: false },
    ],
    answers: {},
    counter: 0,
    externals: new Map(),
    refuse: null,
    ...over,
  };
}

async function mountScreen() {
  OrgStructureScreen.unmount();
  stubTransport();
  document.body.innerHTML = '<div id="main"></div>';
  $('main').innerHTML = OrgStructureScreen.render();
  await OrgStructureScreen.mount({});
  for (let i = 0; i < 4; i += 1) await settle();
}

async function enterEdit() {
  await mountScreen();
  document.querySelector('#org-header-actions [data-act="edit"]').click();
  await settle();
  await sleep(10);
  await settle();
}

// The topmost window: a confirmation can open over the window that asked for it.
const windowEl = () => [...document.querySelectorAll('tf-window')].at(-1) ?? null;
const submit = async () => {
  windowEl().querySelector('[data-act="submit"]').click();
  await sleep(320);
  await settle();
};
const toasts = () => [...document.querySelectorAll('tf-toast')];
const toastText = () => toasts().map((x) => x.getAttribute('message') ?? '');
// A draft change is checked by a debounced dry run; this waits for it.
const preview = async () => { await sleep(300); await settle(); };
const chart = () => $('org-tree');
const nodeOf = (id) => chart().model.nodes.find((n) => n.id === id);
const draftChip = () => $('org-edit-draft-chip').getAttribute('label');

beforeEach(() => {
  world = freshWorld();
  document.querySelectorAll('tf-toast, tf-window, tf-menu, .tf-window-backdrop').forEach((el) => el.remove());
});

const drop = (source, target, kind = 'position') => fire($('org-tree'), 'node-drop', {
  source: { id: source, kind }, target: { id: target, kind }, verdict: { ok: true },
});

// ---- entry and chrome -----------------------------------------------------------

test('the mode bar, the palette, the strip of people and the header buttons of the draft appear', async () => {
  await enterEdit();
  const bar = document.querySelector('#org-edit-top .org-edit-bar');
  assert.ok(bar, 'the mode is clearly marked');
  assert.match(bar.textContent, new RegExp(t('mode_label')));
  assert.match(bar.textContent, new RegExp(t('mode_admin', { name: 'admin' })));
  assert.equal(bar.querySelector('#org-edit-date').getAttribute('value'), TODAY);
  assert.equal($('org-tree').hasAttribute('editing'), true);

  const labels = [...document.querySelectorAll('#org-edit-top .org-edit-tools [data-act]')].map((b) => b.textContent.trim()).filter(Boolean);
  assert.deepEqual(labels.slice(0, 3), [t('add_position'), t('add_unit'), t('load_template')]);
  assert.equal($('org-edit-undo').hasAttribute('disabled'), true);
  assert.equal($('org-edit-redo').hasAttribute('disabled'), true);
  assert.equal(draftChip(), t('draft_chip', { count: 0 }));
  assert.equal($('org-edit-view'), null, 'nothing to compare the structure with yet');

  assert.match($('org-edit-people').textContent, /Rafał Kowal/);
  assert.doesNotMatch($('org-edit-people').textContent, /Nieaktywny/, 'an inactive account is not offered');

  const header = [...document.querySelectorAll('#org-header-actions [data-act]')].map((b) => b.dataset.act);
  assert.deepEqual(header, ['export', 'edit-exit', 'edit-save']);
  assert.equal($('org-edit-save').hasAttribute('disabled'), true, 'nothing to save yet');
  assert.equal($('org-edit-save').textContent.trim(), t('save'));
  assert.equal(document.querySelector('#org-header-actions [data-act="present"]'), null, 'presentation is off while editing');
  OrgStructureScreen.unmount();
});

test('every card has a "⋯" handle in the edit mode and none outside it', async () => {
  await enterEdit();
  $('org-tree')._render();
  assert.ok($('org-tree').querySelector('.ot-card [data-menu]'));
  $('org-edit-exit').click();
  await settle();
  await sleep(10);
  $('org-tree')._render();
  assert.equal($('org-tree').querySelector('[data-menu]'), null);
  OrgStructureScreen.unmount();
});

// ---- a change is a row of the draft, drawn at once and written never (until saved) ----

test('dropping a card on a new manager asks first, says how many people it touches, and adds ONE row to the draft', async () => {
  await enterEdit();
  drop('p-lead', 'p-sales');
  await settle();
  assert.ok(windowEl(), 'a confirmation, not a silent change');
  assert.equal(batches().length, 0, 'nothing is even checked before the confirmation');
  // The lead, the acting deputy, two developers and the tester: five people whose chain of command changes.
  assert.equal(windowEl().querySelector('.tf-act__note').getAttribute('message'), t('reparent_consequence', { count: 5, target: 'Michał Zając' }));
  assert.match(windowEl().querySelector('.tf-act__note').getAttribute('message'), /5 osób/);

  await submit();
  assert.deepEqual(draftOps(), [{ kind: 'positionMove', positionId: 'p-lead', newParentPositionId: 'p-sales', from: TODAY }]);
  assert.equal(lastDry().payload.dryRun, true);
  assert.equal(applies().length, 0, 'nothing is applied');
  assert.equal(draftChip(), t('draft_chip', { count: 1 }));
  assert.equal(nodeOf('p-lead').parentId, 'p-sales', 'the canvas draws the structure the draft would leave');
  assert.equal(world.view.positions.find((p) => p.position_id === 'p-lead').primary_parent_position_id, 'p-delivery', 'and the structure itself is untouched');
  assert.equal($('org-edit-save').hasAttribute('disabled'), false);
  assert.ok($('org-edit-view'), '"Stan na dziś / Stan po zmianach" appears once there is a draft');
  OrgStructureScreen.unmount();
});

test('one person is "1 osoby", and the count follows the branch, not the card', async () => {
  await enterEdit();
  drop('p-tester', 'p-sales');
  await settle();
  assert.equal(windowEl().querySelector('.tf-act__note').getAttribute('message'), t('reparent_consequence', { count: 1, target: 'Michał Zając' }));
  OrgStructureScreen.unmount();
});

test('a cycle is refused on the client before anything is asked or sent', async () => {
  await enterEdit();
  drop('p-lead', 'p-dev1');
  await settle();
  assert.equal(windowEl(), null, 'no confirmation for an impossible move');
  assert.equal(batches().length, 0);
  assert.ok(toastText().includes(t('drop_cycle')));
  drop('p-tester', 'p-assistant');
  await settle();
  assert.ok(toastText().includes(t('drop_staff')), 'a staff position cannot manage');
  OrgStructureScreen.unmount();
});

test('the server refusing a change stays in its window, and nothing enters the draft', async () => {
  world.refuse = (op) => (op.kind === 'positionMove' ? { code: 'reporting_cycle', message: 'cycle', date: TODAY } : null);
  await enterEdit();
  drop('p-tester', 'p-sales');
  await settle();
  await submit();
  assert.ok(windowEl(), 'the window stays');
  assert.equal(windowEl().querySelector('.tf-act__error').getAttribute('message'), t('err_reporting_cycle'));
  assert.equal(draftChip(), t('draft_chip', { count: 0 }));
  OrgStructureScreen.unmount();
});

test('dropping a unit frame on another unit adds a unit move, and a cycle among units is refused', async () => {
  await enterEdit();
  drop('u-delivery', 'u-sales', 'unit');
  await settle();
  assert.match(windowEl().querySelector('.tf-act__note').getAttribute('message'), /osób|osoby|osoba/);
  await submit();
  assert.deepEqual(draftOps(), [{ kind: 'unitMove', unitId: 'u-delivery', newParentUnitId: 'u-sales', from: TODAY }]);
  drop('u-board', 'u-tech', 'unit');
  await settle();
  assert.ok(toastText().includes(t('drop_unit_cycle')));
  OrgStructureScreen.unmount();
});

// ---- a person dropped on a vacancy ----------------------------------------------

function dragChip(userId, overId) {
  const c = $('org-tree');
  c.nodeAt = () => (overId ? { id: overId, kind: 'position' } : null);
  const chip = document.querySelector(`.org-person-chip[data-user="${userId}"]`);
  const pointer = (type, x) => chip.dispatchEvent(Object.assign(new window.Event(type, { bubbles: true }), { pointerId: 1, pointerType: 'mouse', button: 0, clientX: x, clientY: 10 }));
  pointer('pointerdown', 10);
  pointer('pointermove', 60);
  pointer('pointerup', 60);
}

test('a person dropped on a vacancy is an assign row, and leaves the strip in the preview', async () => {
  await enterEdit();
  dragChip('u-free', 'p-tester-auto');
  await settle();
  await sleep(10);
  assert.equal(draftOps().length, 1);
  assert.equal(draftOps()[0].kind, 'assign');
  assert.match(draftOps()[0].tempId, /^tmp:a\d+$/);
  assert.deepEqual({ ...draftOps()[0], tempId: undefined }, {
    kind: 'assign', tempId: undefined, positionId: 'p-tester-auto', subject: { kind: 'user', id: 'u-free' }, assignmentType: 'permanent', share: 1, isPrimary: null, validFrom: TODAY,
  });
  assert.equal(nodeOf('p-tester-auto').vacant, false, 'the canvas shows the seat taken');
  assert.equal(document.querySelector('.org-person-chip[data-user="u-free"]'), null, 'and the person is no longer without a position');
  assert.equal(applies().length, 0);
  OrgStructureScreen.unmount();
});

test('a person dropped on a filled position, or on nothing, adds nothing', async () => {
  await enterEdit();
  dragChip('u-free', 'p-tester');
  await settle();
  assert.equal(batches().length, 0);
  assert.ok(toastText().includes(t('drop_person_occupied')));
  dragChip('u-free', null);
  await settle();
  assert.equal(batches().length, 0);
  OrgStructureScreen.unmount();
});

test('the keyboard reaches the same assignment: Enter on a chip offers the vacancies', async () => {
  await enterEdit();
  document.querySelector('.org-person-chip[data-user="u-free"]').dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
  await settle();
  assert.match(windowEl().textContent, /Tester automatyzujący/);
  OrgStructureScreen.unmount();
});

// ---- the inspector ---------------------------------------------------------------

const select = async (id, kind = 'position') => {
  fire($('org-tree'), 'node-select', { id, kind });
  await settle();
};
const edit = (name) => $('org-tree-detail').querySelector(`[data-edit="${name}"]`);
const change = async (name, detail) => {
  fire(edit(name), 'change', detail);
  await preview();
};

test('the inspector of a position shows the mockup\'s sections and the position\'s own values', async () => {
  await enterEdit();
  await select('p-tester');
  const detail = $('org-tree-detail');
  assert.equal(detail.hidden, false);
  const titles = [...detail.querySelectorAll('.org-insp-title')].map((h) => h.textContent);
  assert.deepEqual(titles, [t('sec_position'), t('sec_assignment'), t('sec_unit', { unit: 'Realizacja' }), t('sec_leadership', { unit: 'Realizacja' }), t('sec_since')]);
  assert.equal(edit('position.name').getAttribute('value'), 'Tester');
  assert.equal(edit('position.parent').getAttribute('value'), 'p-lead');
  assert.equal(edit('assignment.share').getAttribute('value'), '1');
  const parents = [...edit('position.parent').querySelectorAll('option')].map((o) => o.value);
  assert.equal(parents.includes('p-tester'), false, 'not itself');
  assert.equal(parents.includes('p-assistant'), false, 'not a staff position');
  OrgStructureScreen.unmount();
});

test('edits of one thing fold into ONE row, whichever fields of the inspector made them', async () => {
  await enterEdit();
  await select('p-lead');
  await change('position.name', { value: 'Kierownik QA' });
  await change('position.code', { value: '  QA-1 ' });
  await change('position.staff', { checked: true });
  await change('position.role', { value: 'role-1' });
  assert.deepEqual(draftOps(), [{ kind: 'positionUpdate', positionId: 'p-lead', from: TODAY, name: 'Kierownik QA', code: 'QA-1', isStaff: true, roleId: 'role-1', clear: [] }]);
  assert.equal(draftChip(), t('draft_chip', { count: 1 }));
  assert.equal(nodeOf('p-lead').role, 'Kierownik QA', 'and the card is already renamed on the canvas');

  const anna = world.view.assignments.find((a) => a.position_id === 'p-lead');
  await change('assignment.share', { value: '0,5' });
  await change('assignment.type', { value: 'contractor' });
  assert.deepEqual(draftOps().at(-1), { kind: 'assignmentUpdate', assignmentId: anna.id, from: TODAY, share: 0.5, assignmentType: 'contractor' });

  await change('unit.name', { value: 'Realizacja 2' });
  await change('unit.type', { value: 'ty-1' });
  assert.deepEqual(draftOps().at(-1), { kind: 'unitUpdate', unitId: 'u-delivery', from: TODAY, name: 'Realizacja 2', typeId: 'ty-1', clear: [] });
  assert.equal(draftOps().length, 3);
  OrgStructureScreen.unmount();
});

test('an invalid share or an empty name is refused in the field and nothing is added', async () => {
  await enterEdit();
  await select('p-lead');
  fire(edit('assignment.share'), 'change', { value: '2' });
  fire(edit('position.name'), 'change', { value: '   ' });
  await settle();
  assert.equal(batches().length, 0);
  assert.equal(edit('assignment.share').getAttribute('error'), t('share_invalid'));
  assert.equal(edit('position.name').getAttribute('error'), t('name_required'));
  OrgStructureScreen.unmount();
});

test('"Raportuje do" goes through the same confirmation as a drop, and a cancelled one leaves the field as it was', async () => {
  await enterEdit();
  await select('p-tester');
  fire(edit('position.parent'), 'change', { value: 'p-sales' });
  await settle();
  assert.match(windowEl().querySelector('.tf-act__note').getAttribute('message'), /1 osoby/);
  windowEl().querySelector('[data-act="cancel"]').click();
  await sleep(320);
  await settle();
  assert.equal(batches().length, 0);
  assert.equal(edit('position.parent').getAttribute('value'), 'p-lead');
  OrgStructureScreen.unmount();
});

test('the deputy heads of a unit are listed in order, removed with × and reordered from the keyboard, each one row', async () => {
  await enterEdit();
  await select('u-tech', 'unit');
  const rows = () => [...$('org-tree-detail').querySelectorAll('.org-insp-deputy')];
  assert.deepEqual(rows().map((r) => r.dataset.pos), ['p-cto-deputy', 'p-cto-deputy-2']);
  rows()[0].dispatchEvent(new window.KeyboardEvent('keydown', { key: 'ArrowDown', altKey: true, bubbles: true, cancelable: true }));
  await preview();
  assert.deepEqual(draftOps(), [{ kind: 'deputyHeadsSet', unitId: 'u-tech', positionIds: ['p-cto-deputy-2', 'p-cto-deputy'], from: TODAY }]);
  rows()[1].querySelector('[data-act="deputy-remove"]').click();
  await preview();
  assert.deepEqual(draftOps(), [{ kind: 'deputyHeadsSet', unitId: 'u-tech', positionIds: ['p-cto-deputy-2'], from: TODAY }], 'the newest list replaces the earlier');
  OrgStructureScreen.unmount();
});

test('the head of a unit is chosen among its own positions, deputies excluded', async () => {
  await enterEdit();
  await select('u-tech', 'unit');
  $('org-tree-detail').querySelector('[data-act="head-pick"]').click();
  await settle();
  const byValue = new Map([...windowEl().querySelectorAll('tf-radio')].map((r) => [r.getAttribute('value'), r]));
  assert.ok(byValue.has('__none'));
  assert.equal(byValue.get('p-cto-deputy').hasAttribute('disabled'), true);
  assert.equal(byValue.get('p-cto').hasAttribute('disabled'), false);
  OrgStructureScreen.unmount();
});

// ---- the list of pending changes, "Cofnij", undo and redo -----------------------------

const openList = async () => {
  $('org-edit-draft-chip').click();
  await settle();
  return document.querySelector('tf-window .org-edit-panel');
};

test('the list names each pending change, and "Cofnij" removes that row and re-checks the rest', async () => {
  await enterEdit();
  drop('p-tester', 'p-sales');
  await settle();
  await submit();
  await select('p-lead');
  await change('position.name', { value: 'Kierownik QA' });
  const panel = await openList();
  assert.match(panel.querySelector('.org-insp-title').textContent, new RegExp(t('draft_title', { date: formatDay(TODAY) })));
  const rows = [...panel.querySelectorAll('.org-edit-draft-row')];
  assert.equal(rows.length, 2);
  assert.match(rows[0].textContent, /Nowy przełożony stanowiska Tester/);

  rows[0].querySelector('[data-act="edit-remove"]').click();
  await preview();
  assert.deepEqual(draftOps().map((o) => o.kind), ['positionUpdate']);
  assert.equal(nodeOf('p-tester').parentId, 'p-lead', 'the canvas is back to the live structure for that card');
  assert.equal(document.querySelectorAll('tf-window .org-edit-draft-row').length, 1);
  OrgStructureScreen.unmount();
});

test('"Cofnij" on a created unit takes the positions made in it along, and says so', async () => {
  await enterEdit();
  document.querySelector('[data-act="edit-add-unit"]').click();
  await settle();
  windowEl().querySelector('[data-field="name"] tf-input').setAttribute('value', 'Nowy Dział');
  await submit();
  const unitId = chart().model.units.find((u) => u.name === 'Nowy Dział').id;
  document.querySelector('[data-act="edit-add-position"]').click();
  await settle();
  windowEl().querySelector('[data-field="name"] tf-input').setAttribute('value', 'Analityk');
  windowEl().querySelector('[data-field="unit"] tf-select').setAttribute('value', unitId);
  await submit();
  assert.deepEqual(draftOps().map((o) => o.kind), ['unitCreate', 'positionCreate']);
  assert.equal(draftOps()[1].unitId, draftOps()[0].tempId, 'the position refers to the unit by the draft\'s temporary id');
  assert.match(draftOps()[0].tempId, /^tmp:u\d+$/);

  const panel = await openList();
  panel.querySelector('[data-act="edit-remove"]').click();
  await preview();
  assert.equal(draftChip(), t('draft_chip', { count: 0 }));
  assert.ok(toastText().includes(t('removed_with_dependents', { count: 1 })));
  OrgStructureScreen.unmount();
});

test('undo and redo work on the draft, and even a created unit can be taken back', async () => {
  await enterEdit();
  drop('p-tester', 'p-sales');
  await settle();
  await submit();
  assert.equal($('org-edit-undo').hasAttribute('disabled'), false);
  $('org-edit-undo').click();
  await preview();
  assert.equal(draftChip(), t('draft_chip', { count: 0 }));
  assert.equal(nodeOf('p-tester').parentId, 'p-lead');
  assert.equal($('org-edit-redo').hasAttribute('disabled'), false);
  $('org-edit-redo').click();
  await preview();
  assert.equal(nodeOf('p-tester').parentId, 'p-sales');
  assert.equal(applies().length, 0, 'undo and redo never write');
  OrgStructureScreen.unmount();
});

test('a refused row is named on the row, on the chip and in the inspector of its card, and saving is blocked until it is undone', async () => {
  await enterEdit();
  drop('p-tester', 'p-sales');
  await settle();
  await submit();
  // The rule changes under the draft (another administrator, a day passing): the next dry run refuses the row.
  world.refuse = (op) => (op.kind === 'positionMove' ? { code: 'not_valid_at', message: 'x', id: 'p-tester' } : null);
  await select('p-lead');
  await change('position.name', { value: 'Inny' });
  assert.equal(draftChip(), t('draft_chip_errors', { count: 2, errors: 1 }));
  await select('p-tester');
  assert.match($('org-tree-detail').querySelector('.org-insp-error').getAttribute('message'), new RegExp(t('err_not_valid_at')));
  const panel = await openList();
  assert.match(panel.querySelector('.is-refused').textContent, new RegExp(t('err_not_valid_at')));

  $('org-edit-save').click();
  await settle();
  assert.equal(applies().length, 0, 'saving with a refused row applies nothing');
  assert.ok(toastText().includes(t('save_blocked', { count: 1 })));
  OrgStructureScreen.unmount();
});

// ---- saving, discarding, leaving --------------------------------------------------------------

test('"Zapisz zmiany" is one dry run and one apply of the whole draft, and then the draft is empty', async () => {
  await enterEdit();
  drop('p-tester', 'p-sales');
  await settle();
  await submit();
  dragChip('u-free', 'p-tester-auto');
  await settle();
  await sleep(300);
  const before = batches().length;
  $('org-edit-save').click();
  await settle();
  await sleep(50);
  const sent = batches().slice(before);
  assert.deepEqual(sent.map((b) => b.payload.dryRun), [true, false], 'a dry run first, then the apply');
  assert.deepEqual(sent[1].payload.ops, sent[0].payload.ops);
  assert.deepEqual(sent[1].payload.ops.map((o) => o.kind), ['positionMove', 'assign']);
  assert.equal(world.view.positions.find((p) => p.position_id === 'p-tester').primary_parent_position_id, 'p-sales', 'and now the structure has it');
  assert.equal(draftChip(), t('draft_chip', { count: 0 }));
  assert.ok(toastText().includes(t('saved', { count: 2 })));
  assert.equal($('org-edit-save').hasAttribute('disabled'), true);
  assert.equal($('org-edit-undo').hasAttribute('disabled'), true, 'a saved draft has nothing left to undo');
  OrgStructureScreen.unmount();
});

test('"Odrzuć zmiany" asks, empties the draft and writes nothing — and can itself be undone', async () => {
  await enterEdit();
  drop('p-tester', 'p-sales');
  await settle();
  await submit();
  const panel = await openList();
  panel.querySelector('[data-act="edit-discard"]').click();
  await settle();
  assert.equal(windowEl().querySelector('.tf-act__note').getAttribute('message'), t('discard_consequence', { count: 1 }));
  await submit();
  await preview();
  assert.equal(draftChip(), t('draft_chip', { count: 0 }));
  assert.equal(applies().length, 0);
  $('org-edit-undo').click();
  await preview();
  assert.equal(draftChip(), t('draft_chip', { count: 1 }));
  OrgStructureScreen.unmount();
});

test('finishing the edit with unsaved changes asks; staying keeps the draft, leaving drops it', async () => {
  await enterEdit();
  drop('p-tester', 'p-sales');
  await settle();
  await submit();
  $('org-edit-exit').click();
  await settle();
  assert.equal(windowEl().querySelector('.tf-act__note').getAttribute('message'), t('leave_consequence', { count: 1 }));
  windowEl().querySelector('[data-act="cancel"]').click();
  await sleep(320);
  await settle();
  assert.ok($('org-edit-top').querySelector('.org-edit-bar'), 'still editing');
  assert.equal(draftChip(), t('draft_chip', { count: 1 }));

  $('org-edit-exit').click();
  await settle();
  await submit();
  await sleep(50);
  assert.equal($('org-tree').hasAttribute('editing'), false);
  assert.ok(document.querySelector('#org-header-actions [data-act="edit"]'));
  assert.equal(applies().length, 0);
  OrgStructureScreen.unmount();
});

test('leaving the screen asks too, through the router\'s hook; a clean draft leaves without a question', async () => {
  await enterEdit();
  assert.equal(await OrgStructureScreen.canUnmount(), true);
  drop('p-tester', 'p-sales');
  await settle();
  await submit();
  const answer = OrgStructureScreen.canUnmount();
  await settle();
  assert.match(windowEl().querySelector('.tf-act__note').getAttribute('message'), /1/);
  windowEl().querySelector('[data-act="cancel"]').click();
  assert.equal(await answer, false, 'the router stays on the screen');
  OrgStructureScreen.unmount();
});

// ---- menus ---------------------------------------------------------------------------

const menuLabels = async (id, kind) => {
  document.querySelectorAll('tf-menu').forEach((m) => m.close?.());
  fire($('org-tree'), 'node-menu', { id, kind, rect: { left: 40, top: 40, width: 20, height: 20 } });
  await settle();
  return [...document.querySelectorAll('tf-menu tf-menu-item')].map((el) => el.getAttribute('label'));
};

test('the "⋯" menu of an occupied position offers edit, change person, move, head and the two endings', async () => {
  await enterEdit();
  assert.deepEqual(await menuLabels('p-tester', 'position'), [
    t('menu_edit'), t('menu_replace'), t('menu_move'), t('menu_set_head'), t('menu_add_deputy'), t('menu_end_assignment'), t('menu_end_position'),
  ]);
  OrgStructureScreen.unmount();
});

test('the menu of a vacancy assigns instead of replacing and has no assignment to end', async () => {
  await enterEdit();
  const labels = await menuLabels('p-tester-auto', 'position');
  assert.ok(labels.includes(t('menu_assign')));
  assert.equal(labels.includes(t('menu_replace')), false);
  assert.equal(labels.includes(t('menu_end_assignment')), false);
  OrgStructureScreen.unmount();
});

test('the menu of a unit head can take the role away, and of a deputy cannot make them head', async () => {
  await enterEdit();
  assert.ok((await menuLabels('p-cto', 'position')).includes(t('menu_unset_head')));
  document.querySelectorAll('tf-menu').forEach((m) => m.close?.());
  fire($('org-tree'), 'node-menu', { id: 'p-cto-deputy', kind: 'position', rect: { left: 1, top: 1, width: 1, height: 1 } });
  await settle();
  const setHead = [...document.querySelectorAll('tf-menu tf-menu-item')].find((el) => el.getAttribute('label') === t('menu_set_head'));
  assert.equal(setHead.hasAttribute('disabled'), true);
  OrgStructureScreen.unmount();
});

test('the menu of a unit edits it, adds to it, sets its head, moves it and liquidates it', async () => {
  await enterEdit();
  assert.deepEqual(await menuLabels('u-sales', 'unit'), [
    t('menu_edit_unit'), t('menu_add_position'), t('menu_add_subunit'), t('menu_pick_head'), t('menu_move_unit'), t('menu_end_unit'),
  ]);
  OrgStructureScreen.unmount();
});

test('ending an assignment or a unit asks once, says what follows on the effective day, and adds the end to the draft', async () => {
  await enterEdit();
  await select('p-tester');
  $('org-tree-detail').querySelector('[data-act="assignment-end"]').click();
  await settle();
  assert.match(windowEl().querySelector('.tf-act__note').getAttribute('message'), new RegExp(`od ${formatDay(TODAY).replaceAll('.', '\\.')}.*nie są przekazywane automatycznie`));
  await submit();
  const tester = world.view.assignments.find((a) => a.position_id === 'p-tester');
  assert.deepEqual(draftOps(), [{ kind: 'assignmentEnd', assignmentId: tester.id, from: TODAY }]);
  assert.equal(nodeOf('p-tester').vacant, true);

  await select('u-fin', 'unit');
  $('org-tree-detail').querySelector('[data-act="unit-end"]').click();
  await settle();
  await submit();
  assert.deepEqual(draftOps().at(-1), { kind: 'unitEnd', unitId: 'u-fin', from: TODAY });
  OrgStructureScreen.unmount();
});

// ---- the palette ----------------------------------------------------------------------

test('"Dodaj jednostkę" and "Dodaj stanowisko" add creating rows with temporary ids, and select the new card', async () => {
  await enterEdit();
  document.querySelector('[data-act="edit-add-unit"]').click();
  await settle();
  windowEl().querySelector('[data-field="name"] tf-input').setAttribute('value', 'Nowy Dział');
  await submit();
  assert.equal(draftOps()[0].kind, 'unitCreate');
  assert.equal(draftOps()[0].name, 'Nowy Dział');
  assert.equal(draftOps()[0].validFrom, TODAY);
  assert.ok($('org-tree-detail').textContent.includes('Nowy Dział'), 'the new unit is selected in the inspector');
  OrgStructureScreen.unmount();
});

// ---- the effective day ---------------------------------------------------------------------

test('a future day is a planned reorganization: the chart shows that day, every row is redated, and saving offers a reorganization', async () => {
  await enterEdit();
  drop('p-tester', 'p-sales');
  await settle();
  await submit();
  fire($('org-edit-date'), 'change', { value: '2026-11-01' });
  await preview();
  await preview();
  assert.equal(draftOps()[0].from, '2026-11-01', 'the whole draft moves to the day');
  assert.match($('org-edit-day-chip').textContent, new RegExp(t('planned_chip')));
  assert.equal($('org-edit-view').querySelector('option[value="draft"]').textContent, t('view_on', { date: formatDay('2026-11-01') }));
  const header = [...document.querySelectorAll('#org-header-actions [data-act]')].map((b) => b.dataset.act);
  assert.deepEqual(header, ['export', 'edit-exit', 'edit-save-plan'], 'a planned day is saved as a reorganization, never applied directly');
  OrgStructureScreen.unmount();
});

test('"Zapisz jako reorganizację…" stores the draft as a change set of Historia and leaves the edit mode', async () => {
  world.answers.orgChangeSetSaveRequest = (payload) => ({ ok: true, valid: true, change_set: { id: 'cs-1', name: payload.name, effective_date: payload.effectiveDate, ops: [] }, results: [], warnings: [] });
  await enterEdit();
  drop('p-tester', 'p-sales');
  await settle();
  await submit();
  fire($('org-edit-date'), 'change', { value: '2026-11-01' });
  await preview();
  await preview();
  document.querySelector('#org-header-actions [data-act="edit-save-plan"]').click();
  await settle();
  windowEl().querySelector('[data-field="name"] tf-input').setAttribute('value', 'Reorganizacja listopadowa');
  await submit();
  await sleep(50);
  const saved = requests.find((r) => r.kind === 'orgChangeSetSaveRequest');
  assert.ok(saved);
  assert.equal(saved.payload.name, 'Reorganizacja listopadowa');
  assert.equal(saved.payload.effectiveDate, '2026-11-01');
  assert.deepEqual(saved.payload.ops.map((o) => [o.kind, o.from]), [['positionMove', '2026-11-01']]);
  assert.equal(applies().length, 0, 'nothing is applied to the live structure');
  assert.ok(toastText().some((x) => x.includes('Reorganizacja listopadowa')));
  assert.equal($('org-tree').hasAttribute('editing'), false, 'the edit mode is left');
  OrgStructureScreen.unmount();
});

test('a malformed day is refused and nothing is read', async () => {
  await enterEdit();
  const before = requests.length;
  fire($('org-edit-date'), 'change', { value: '2026-13-40' });
  await settle();
  assert.equal(requests.length, before);
  assert.equal($('org-edit-date').getAttribute('error'), t('date_invalid', { format: dateFormatHint() }));
  OrgStructureScreen.unmount();
});

test('a backdated change asks for confirmation, and the draft is then saved with confirmBackdated', async () => {
  world.refuse = (op, i, payload) => (payload.confirmBackdated ? null : { code: 'backdated_confirmation_required', message: 'before today', date: '2026-01-05' });
  await enterEdit();
  fire($('org-edit-date'), 'change', { value: '2026-01-05' });
  await preview();
  await preview();
  assert.match($('org-edit-day-chip').textContent, new RegExp(t('backdated_chip')));
  drop('p-tester', 'p-sales');
  await settle();
  await submit(); // the reparent confirmation; the dry run then wants the backdating confirmed
  await settle();
  assert.match(windowEl().querySelector('.tf-act__note').getAttribute('message'), /05\.01\.2026/);
  await submit();
  await settle();
  assert.equal(draftOps().length, 1);
  assert.equal(lastDry().payload.confirmBackdated, true);
  $('org-edit-save').click();
  await settle();
  await sleep(50);
  assert.equal(applies().at(-1).payload.confirmBackdated, true);
  OrgStructureScreen.unmount();
});

test('"Stan na dziś" shows the structure without the draft and switches editing off; "Stan po zmianach" brings the draft back', async () => {
  await enterEdit();
  drop('p-tester', 'p-sales');
  await settle();
  await submit();
  assert.equal(nodeOf('p-tester').parentId, 'p-sales');
  fire($('org-edit-view'), 'change', { value: 'today' });
  await settle();
  assert.equal(nodeOf('p-tester').parentId, 'p-lead', 'today\'s structure');
  assert.equal($('org-tree').hasAttribute('editing'), false, 'nothing is edited on a structure that is not the draft');
  assert.equal($('org-edit-undo').hasAttribute('disabled'), true);
  fire($('org-edit-view'), 'change', { value: 'draft' });
  await settle();
  assert.equal(nodeOf('p-tester').parentId, 'p-sales');
  assert.equal($('org-tree').hasAttribute('editing'), true);
  OrgStructureScreen.unmount();
});

// ---- templates --------------------------------------------------------------------------------

test('an empty structure offers a template or a first unit; a template becomes draft rows with temporary ids, drawn before it is saved', async () => {
  world.view = { at: TODAY, timezone: 'Europe/Warsaw', units: [], positions: [], assignments: [], vacancies: [], warnings: [] };
  await mountScreen();
  const overlay = $('org-tree-empty');
  assert.equal(overlay.hidden, false);
  overlay.querySelector('[data-act="empty-template"]').click();
  await settle();
  await sleep(10);
  const win = windowEl();
  assert.equal(win.querySelectorAll('tf-radio').length, 3);
  assert.match(win.querySelector('.org-tpl-outline').textContent, /Firma/, 'with a preview of what will be created');
  assert.equal(win.querySelector('tf-checkbox'), null, 'the draft can be reviewed and undone, so there is no confirmation to tick');

  await submit();
  const ops = draftOps();
  assert.equal(ops.filter((o) => o.kind === 'unitCreate').length, 1);
  assert.equal(ops.filter((o) => o.kind === 'positionCreate').length, 4);
  assert.ok(ops.every((o) => !o.tempId || o.tempId.startsWith('tmp:')));
  assert.equal(dryRuns().length, 1, 'the whole template is ONE dry run');
  assert.equal(applies().length, 0);
  assert.equal(chart().model.nodes.length, 4, 'the canvas draws the template');
  assert.equal($('org-tree-empty').hidden, true);
  assert.equal(draftChip(), t('draft_chip', { count: ops.length }));

  $('org-edit-undo').click();
  await preview();
  assert.equal(chart().model.nodes.length, 0, 'and one undo takes the whole template back');
  OrgStructureScreen.unmount();
});

test('on an occupied structure a template is added next to it, with no question asked', async () => {
  await enterEdit();
  document.querySelector('[data-act="edit-template"]').click();
  await settle();
  assert.equal(windowEl().querySelector('tf-checkbox'), null);
  windowEl().querySelector('tf-radio:nth-of-type(2) .tf-radio-label').click();
  await submit();
  assert.ok(draftOps().length >= 18, 'the departments template: units, positions and heads');
  assert.ok(chart().model.units.some((u) => u.code === 'FD'));
  OrgStructureScreen.unmount();
});

// ---- a reorganization opened from Historia -------------------------------------------------

test('Historia can hand a stored reorganization to the edit mode: it opens on its day with its rows, and offers to save the reorganization', async () => {
  const { openChangeSetInEditor } = await import('./history-bridge.js');
  await mountScreen();
  const taken = await openChangeSetInEditor({
    id: 'cs-7', name: 'Reorganizacja Q4', effectiveDate: '2026-11-01',
    ops: [{ kind: 'positionMove', positionId: 'p-tester', newParentPositionId: 'p-sales', from: '2026-11-01' }],
  });
  assert.equal(taken, true);
  await settle();
  await preview();
  await preview();
  assert.ok($('org-edit-top').querySelector('.org-edit-bar'));
  assert.equal($('org-edit-date').getAttribute('value'), '2026-11-01');
  assert.match($('org-edit-top').textContent, /Reorganizacja Q4/);
  assert.equal(draftChip(), t('draft_chip', { count: 1 }));
  assert.equal(nodeOf('p-tester').parentId, 'p-sales');
  const header = [...document.querySelectorAll('#org-header-actions [data-act]')].map((b) => b.dataset.act);
  assert.deepEqual(header, ['export', 'edit-exit', 'edit-save-plan'], 'a stored reorganization is saved back as one, not applied');
  assert.equal($('org-edit-save-plan').textContent.trim(), t('plan_update'));
  OrgStructureScreen.unmount();
});

test('the menu of a seat held by an account offers "Zakończ przypisanie i przekaż…" and opens the handover screen for them', async () => {
  await enterEdit();
  const label = I18n.t('org_structure.handover.menu');
  assert.equal((await menuLabels('p-tester', 'position')).includes(label), false, 'a person without an account holds no work');
  const labels = await menuLabels('p-lead', 'position');
  assert.ok(labels.includes(label));
  assert.ok(labels.indexOf(label) < labels.indexOf(t('menu_end_assignment')), 'the handover comes before the plain end');
  const navigations = [];
  Router.navigate = async (id, params) => { navigations.push([id, params]); return true; };
  [...document.querySelectorAll('tf-menu tf-menu-item')].find((el) => el.getAttribute('label') === label).querySelector('.tf-menu-item').click();
  await settle();
  assert.equal(navigations.length, 1);
  assert.equal(navigations[0][0], 'org-structure');
  assert.deepEqual([navigations[0][1].tab, navigations[0][1].reason], ['list', 'departure']);
  assert.ok(navigations[0][1].handover, 'the account of the seat');
  Router.navigate = async () => true;
  OrgStructureScreen.unmount();
});

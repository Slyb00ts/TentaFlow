// =============================================================================
// File: modules/org-structure/list-tab.test.js
// Description: The Lista tab against a stubbed transport. What has to hold: the
//   table draws a row per holder and per vacancy; the search, the chips and the
//   sort work on it; a reader gets no row menu, no "Dodaj stanowisko" and no
//   import; an administrator's menu items open windows whose submit sends the
//   right request with the right payload; the undo of an edit and of an end
//   really sends the inverse write; a backdated change is confirmed once; a
//   refusal stays in the window as a sentence; the export asks for the format.
// =============================================================================

import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import { window, I18n, sleep, closed, cleanBody } from '../../lib/actions/_test-setup.js';

process.on('unhandledRejection', () => {});
URL.createObjectURL = () => 'blob:test';
URL.revokeObjectURL = () => {};

const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
const { formatDay } = await import('/js/lib/date-format.js');
const { mountListTab, refreshListTab, unmountListTab } = await import('./list-tab.js');

const lt = (key, params) => I18n.t(`org_structure.list.${key}`, params);
const anna = { kind: 'user', id: 'u-anna' };
const jan = { kind: 'user', id: 'u-jan' };
const ola = { kind: 'external', id: 'x-ola' };

const view = () => ({
  at: '2026-09-30',
  timezone: 'Europe/Warsaw',
  units: [
    { unit_id: 'unit-board', name: 'Board', head_position_id: 'pos-ceo', deputy_head_position_ids: [] },
    { unit_id: 'unit-it', name: 'IT', head_position_id: null, deputy_head_position_ids: [] },
  ],
  positions: [
    { position_id: 'pos-ceo', unit_id: 'unit-board', name: 'CEO', primary_parent_position_id: null, valid_from: '2024-01-01' },
    { position_id: 'pos-dev', unit_id: 'unit-it', name: 'Developer', primary_parent_position_id: 'pos-ceo', valid_from: '2025-03-01' },
    { position_id: 'pos-qa', unit_id: 'unit-it', name: 'Tester', primary_parent_position_id: 'pos-ceo', valid_from: '2026-09-20' },
    { position_id: 'pos-ops', unit_id: 'unit-board', name: 'Assistant', primary_parent_position_id: 'pos-ceo', valid_from: '2026-01-01' },
  ],
  assignments: [
    { id: 'a-ceo', position_id: 'pos-ceo', subject: anna, display_name: 'Anna Nowak', share: 1, assignment_type: 'permanent', is_primary: true, valid_from: '2024-01-01' },
    { id: 'a-dev', position_id: 'pos-dev', subject: jan, display_name: 'Jan Zawisza', share: 0.75, assignment_type: 'permanent', is_primary: true, valid_from: '2025-03-01' },
    { id: 'a-qa', position_id: 'pos-qa', subject: ola, display_name: 'Ola Kowal', share: 1, assignment_type: 'contractor', is_primary: true, valid_from: '2026-09-20' },
  ],
  vacancies: ['pos-ops'],
  warnings: [],
});

const input = (permissions = ['org.admin']) => ({ view: view(), unitTypes: [], myPermissions: permissions });

const calls = [];
let script = {};
let reloads = 0;

function stubTransport(handlers = {}) {
  script = handlers;
  ApiBinary.one = (kind, payload) => {
    calls.push({ kind, payload });
    const handler = script[kind];
    if (!handler) return Promise.reject(new Error(`unexpected request ${kind}`));
    return Promise.resolve(typeof handler === 'function' ? handler(payload) : handler);
  };
  ApiBinary.list = () => Promise.resolve([
    { id: 'u-new', username: 'nowak', display_name: 'Zofia Nowak', is_active: true },
    { id: 'u-off', username: 'off', display_name: 'Inactive One', is_active: false },
  ]);
}

const ok = (result = null, warnings = []) => ({ ok: true, error: null, warnings, result });
const refused = (code, extra = {}) => ({ ok: false, error: { code, message: 'x', ...extra }, warnings: [], result: null });

async function mount(permissions) {
  cleanBody();
  calls.length = 0;
  reloads = 0;
  document.body.innerHTML = '<div id="org-root"><span id="org-list-actions"></span><div id="host"></div></div>';
  mountListTab(document.getElementById('host'), input(permissions), { reload: async () => { reloads += 1; } });
  await sleep(0);
}

beforeEach(() => {
  unmountListTab();
  stubTransport({});
});

const table = () => document.querySelector('[data-role="table"]');
const shownIds = () => table().rows.map((r) => r._id);
const dispatch = (el, type, detail) => el.dispatchEvent(new window.CustomEvent(type, { bubbles: true, detail }));
const searchFor = (value) => dispatch(document.querySelector('tf-searchbox'), 'search', { value });
const pickChip = (id) => dispatch(document.querySelector('[data-role="chips"]'), 'change', { id });
const headerButton = (act) => document.querySelector(`#org-list-actions [data-act="${act}"]`);
const toolButton = (act) => document.querySelector(`.org-list-tools [data-act="${act}"]`);

function openMenu(rowId) {
  const row = table().rows.find((r) => r._id === rowId);
  const button = table().rowActions(row, 0, () => row);
  document.body.appendChild(button);
  button.click();
  return document.querySelector('tf-menu');
}

const itemLabels = (menu) => [...menu.querySelectorAll('tf-menu-item')].map((i) => i.getAttribute('label'));
const choose = (menu, label) => [...menu.querySelectorAll('tf-menu-item')]
  .find((i) => i.getAttribute('label') === label).querySelector('.tf-menu-item').click();
const lastWindow = () => [...document.querySelectorAll('tf-window.tf-act-window')].pop();
const field = (win, label) => win.querySelector(`[label="${label}"]`);
const submit = (win) => win.querySelector('[data-act="submit"]').click();
const toastUndo = () => [...document.querySelectorAll('tf-toast tf-button')].pop();

// --- the table ---------------------------------------------------------------------------------

test('the table has a row per holder and per vacancy, with the manager above each', async () => {
  await mount();
  assert.deepEqual(table().rows.map((r) => [r.person.value, r.reportsTo]), [
    ['Anna Nowak', '—'],
    [I18n.t('org_structure.vacancy'), 'Anna Nowak'],
    ['Jan Zawisza', 'Anna Nowak'],
    ['Ola Kowal', 'Anna Nowak'],
  ]);
});

test('the columns and the counts follow the mockup: chips with counts, and "showing N of M"', async () => {
  await mount();
  const labels = [...table().querySelectorAll('tf-column')].map((c) => c.getAttribute('label'));
  assert.deepEqual(labels, [lt('col_person'), I18n.t('org_structure.col_unit'), I18n.t('org_structure.col_reports_to'), lt('col_since')]);
  const chips = document.querySelector('[data-role="chips"]').filters;
  assert.deepEqual(chips.map((c) => [c.id, c.count]), [['all', 4], ['vacant', 1], ['changes', 1], ['no_account', 1]]);
  assert.equal(document.querySelector('[data-role="footer"]').textContent, lt('showing', { shown: 4, total: 4 }));
});

test('the search narrows the rows and the footer says how many are shown', async () => {
  await mount();
  searchFor('zawisza');
  assert.deepEqual(shownIds(), ['a-dev']);
  assert.equal(document.querySelector('[data-role="footer"]').textContent, lt('showing', { shown: 1, total: 4 }));
  searchFor('');
  assert.equal(shownIds().length, 4);
});

test('the chips keep vacancies, changes since a day, or people without an account', async () => {
  await mount();
  pickChip('vacant');
  assert.deepEqual(shownIds(), ['vacant:pos-ops']);
  pickChip('no_account');
  assert.deepEqual(shownIds(), ['a-qa']);
  pickChip('changes');
  assert.equal(document.querySelector('[data-role="since"]').hidden, false, 'the day picker appears with the chip');
  assert.deepEqual(shownIds(), ['a-qa'], 'the last 30 days: the tester started on 20.09');
  dispatch(document.querySelector('[data-role="since"]'), 'change', { value: '2026-09-25' });
  assert.deepEqual(shownIds(), [], 'nothing changed since 25.09');
  dispatch(document.querySelector('[data-role="since"]'), 'change', { value: '2025-01-01' });
  assert.deepEqual(shownIds().sort(), ['a-dev', 'a-qa', 'vacant:pos-ops'].sort());
  pickChip('all');
  assert.equal(document.querySelector('[data-role="since"]').hidden, true);
});

test('clicking a sortable header sorts by it, again reverses it', async () => {
  await mount();
  const header = () => table().shadowRoot.querySelector('th[data-key="person"]');
  header().click();
  assert.deepEqual(table()._sortedRows().map((r) => r._id), ['a-ceo', 'a-dev', 'a-qa', 'vacant:pos-ops']);
  header().click();
  assert.deepEqual(table()._sortedRows().map((r) => r._id), ['vacant:pos-ops', 'a-qa', 'a-dev', 'a-ceo']);
});

test('a filter that matches nothing says so instead of drawing an empty frame', async () => {
  await mount();
  searchFor('zzz');
  assert.equal(table().rows.length, 0);
  assert.equal(table().getAttribute('empty-message'), lt('empty_filtered'));
});

test('an empty structure still draws the tab, so a file can be imported into it', async () => {
  cleanBody();
  document.body.innerHTML = '<div id="org-root"><span id="org-list-actions"></span><div id="host"></div></div>';
  const empty = { view: { at: '2026-09-30', timezone: 'Europe/Warsaw', units: [], positions: [], assignments: [], vacancies: [], warnings: [] }, unitTypes: [], myPermissions: ['org.admin'] };
  mountListTab(document.getElementById('host'), empty, { reload: async () => {} });
  assert.equal(table().rows.length, 0);
  assert.equal(document.querySelector('[data-role="footer"]').textContent, lt('empty_structure'));
  assert.ok(headerButton('import'), 'the import is there');
  assert.equal(toolButton('add').hasAttribute('disabled'), true, 'a position needs a unit');
  assert.equal(toolButton('add').getAttribute('title'), lt('add_no_units'));
});

// --- permissions ---------------------------------------------------------------------------------

test('a reader sees the list and can export it, and nothing that could only be refused', async () => {
  await mount([]);
  assert.equal(shownIds().length, 4);
  assert.equal(table().rowActions, null, 'no row menu');
  assert.equal(table().hasAttribute('actions-label'), false);
  assert.ok(toolButton('export'));
  assert.equal(toolButton('add'), null);
  assert.ok(headerButton('export'));
  assert.equal(headerButton('import'), null);
});

test('an administrator gets the row menu, "Dodaj stanowisko" and the import', async () => {
  await mount(['org.admin']);
  assert.equal(typeof table().rowActions, 'function');
  assert.ok(toolButton('add'));
  assert.ok(headerButton('import'));
});

test('a refresh that changes the permission redraws the controls', async () => {
  await mount(['org.admin']);
  refreshListTab(input([]));
  assert.equal(toolButton('add'), null);
  assert.equal(headerButton('import'), null);
  refreshListTab(input(['org.admin']));
  assert.ok(toolButton('add'));
});

test('a refresh keeps the search and the chip', async () => {
  await mount();
  searchFor('ola');
  refreshListTab(input());
  assert.deepEqual(shownIds(), ['a-qa']);
});

// --- the row menu ---------------------------------------------------------------------------------

test('a person row offers edit, move, deputy and end; a vacancy offers assign and end position', async () => {
  await mount();
  const person = itemLabels(openMenu('a-dev'));
  // Other work packages add their own entries to this menu; the deputy one sits after "move" and before "end".
  assert.deepEqual([lt('menu_edit'), lt('menu_move'), lt('menu_deputy'), lt('menu_end')].map((l) => person.indexOf(l)).every((i, n, all) => i >= 0 && (n === 0 || i > all[n - 1])), true, JSON.stringify(person));
  document.querySelector('tf-menu').close();
  // A person without an account has nobody to be covered.
  assert.deepEqual(itemLabels(openMenu('a-qa')), [lt('menu_edit'), lt('menu_move'), lt('menu_end')]);
  document.querySelector('tf-menu').close();
  assert.deepEqual(itemLabels(openMenu('vacant:pos-ops')), [lt('menu_assign'), lt('menu_end_position')]);
});

test('a person with an account can be ended with a handover, which opens the handover screen for them', async () => {
  await mount();
  const label = I18n.t('org_structure.handover.menu');
  assert.ok(itemLabels(openMenu('a-dev')).includes(label));
  document.querySelector('tf-menu').close();
  assert.equal(itemLabels(openMenu('a-qa')).includes(label), false, 'a person without an account holds no work');
  document.querySelector('tf-menu').close();
  const { Router } = await import('/js/router.js');
  const navigations = [];
  Router.navigate = (id, params) => { navigations.push([id, params]); return Promise.resolve(true); };
  choose(openMenu('a-dev'), label);
  assert.deepEqual(navigations, [['org-structure', { tab: 'list', handover: 'u-jan', reason: 'departure' }]]);
});

// --- writes -----------------------------------------------------------------------------------------

test('edit assignment: only the changed values are sent, from the chosen day, and the undo sends the old ones', async () => {
  await mount();
  stubTransport({ orgAssignmentUpdateRequest: ok({ kind: 'assignment', value: { id: 'a-dev-2' } }) });
  choose(openMenu('a-dev'), lt('menu_edit'));
  const win = lastWindow();
  assert.equal(field(win, `${lt('f_share')} *`).value, '0.75');
  field(win, `${lt('f_share')} *`).value = '0,5';
  submit(win);
  await closed();

  assert.deepEqual(calls.map((c) => c.kind), ['orgAssignmentUpdateRequest']);
  assert.deepEqual(calls[0].payload, {
    assignmentId: 'a-dev', assignmentType: undefined, share: 0.5, isPrimary: undefined, from: '2026-09-30',
  });
  assert.equal(reloads, 1, 'the list is read again');

  toastUndo().click();
  await sleep(0);
  assert.equal(calls.length, 2);
  assert.deepEqual(calls[1].payload, {
    assignmentId: 'a-dev-2', assignmentType: undefined, share: 0.75, isPrimary: undefined, from: '2026-09-30', confirmBackdated: false,
  });
});

test('edit assignment: saving without a change is refused in the window and nothing is sent', async () => {
  await mount();
  choose(openMenu('a-dev'), lt('menu_edit'));
  const win = lastWindow();
  submit(win);
  await sleep(0);
  assert.equal(calls.length, 0);
  assert.equal(win.querySelector('.tf-act__error').getAttribute('message'), lt('nothing_changed'));
});

test('edit assignment: an end date ends it in the same save, and that save has no undo', async () => {
  await mount();
  stubTransport({
    orgAssignmentUpdateRequest: ok({ kind: 'assignment', value: { id: 'a-dev-2' } }),
    orgAssignmentEndRequest: ok({ kind: 'done' }),
  });
  choose(openMenu('a-dev'), lt('menu_edit'));
  const win = lastWindow();
  field(win, `${lt('f_type')} *`).value = 'acting';
  field(win, lt('f_to')).value = '2026-12-31';
  submit(win);
  await closed();
  assert.deepEqual(calls.map((c) => c.kind), ['orgAssignmentUpdateRequest', 'orgAssignmentEndRequest']);
  assert.deepEqual(calls[1].payload, { assignmentId: 'a-dev-2', from: '2026-12-31' });
  assert.equal(toastUndo(), undefined, 'no undo button');
});

test('edit assignment: when the end is refused the changed values go back and the window shows why', async () => {
  await mount();
  stubTransport({
    orgAssignmentUpdateRequest: ok({ kind: 'assignment', value: { id: 'a-dev-2' } }),
    orgAssignmentEndRequest: refused('invalid_interval'),
  });
  choose(openMenu('a-dev'), lt('menu_edit'));
  const win = lastWindow();
  field(win, `${lt('f_type')} *`).value = 'acting';
  field(win, lt('f_to')).value = '2025-01-01';
  submit(win);
  await sleep(10);
  assert.deepEqual(calls.map((c) => c.kind), ['orgAssignmentUpdateRequest', 'orgAssignmentEndRequest', 'orgAssignmentUpdateRequest']);
  assert.equal(calls[2].payload.assignmentType, 'permanent', 'the old type is put back');
  assert.equal(win.querySelector('.tf-act__error').getAttribute('message'), lt('errors.invalid_interval'));
  assert.equal(win.isConnected, true);
});

test('end assignment: the window lists the consequences and its submit ends from the chosen day; undo assigns again', async () => {
  await mount();
  stubTransport({ orgAssignmentEndRequest: ok({ kind: 'done' }), orgAssignRequest: ok({ kind: 'assignment', value: { id: 'a-new' } }) });
  choose(openMenu('a-dev'), lt('menu_end'));
  const win = lastWindow();
  const note = win.querySelector('.tf-act__note').getAttribute('message');
  assert.ok(note.includes(lt('consequence_becomes_vacant', { position: 'Developer', unit: 'IT' })), note);
  assert.ok(note.includes(lt('end_no_handover')), 'no handover is promised');
  submit(win);
  await closed();
  assert.deepEqual(calls[0], { kind: 'orgAssignmentEndRequest', payload: { assignmentId: 'a-dev', from: '2026-09-30' } });

  toastUndo().click();
  await sleep(0);
  assert.equal(calls[1].kind, 'orgAssignRequest');
  assert.deepEqual(calls[1].payload, {
    positionId: 'pos-dev', subject: jan, assignmentType: 'permanent', share: 0.75, isPrimary: true,
    validFrom: '2026-09-30', validTo: null, confirmBackdated: false,
  });
});

test('move: the person leaves the seat and takes a vacant one from today; the undo removes the new seat and restores the old', async () => {
  await mount();
  stubTransport({ orgAssignmentEndRequest: ok({ kind: 'done' }), orgAssignRequest: ok({ kind: 'assignment', value: { id: 'a-new' } }) });
  choose(openMenu('a-dev'), lt('menu_move'));
  const win = lastWindow();
  win.querySelector('tf-radio-group').value = 'pos-ops';
  submit(win);
  await closed();
  assert.deepEqual(calls.map((c) => c.kind), ['orgAssignmentEndRequest', 'orgAssignRequest']);
  assert.deepEqual(calls[0].payload, { assignmentId: 'a-dev', from: '2026-09-30' });
  assert.deepEqual(calls[1].payload, {
    positionId: 'pos-ops', subject: jan, assignmentType: 'permanent', share: 0.75, isPrimary: true,
    validFrom: '2026-09-30', confirmBackdated: false,
  });
  toastUndo().click();
  await sleep(10);
  assert.deepEqual(calls.slice(2).map((c) => [c.kind, c.payload.assignmentId ?? c.payload.positionId]), [
    ['orgAssignmentEndRequest', 'a-new'], ['orgAssignRequest', 'pos-dev'],
  ]);
  assert.equal(calls[3].payload.validTo, null);
});

test('move: when the new seat is refused the old assignment is put back', async () => {
  await mount();
  stubTransport({
    orgAssignmentEndRequest: ok({ kind: 'done' }),
    orgAssignRequest: (p) => (p.positionId === 'pos-ops' ? refused('assignment_overlap') : ok({ kind: 'assignment', value: { id: 'a-back' } })),
  });
  choose(openMenu('a-dev'), lt('menu_move'));
  const win = lastWindow();
  win.querySelector('tf-radio-group').value = 'pos-ops';
  submit(win);
  await sleep(10);
  assert.deepEqual(calls.map((c) => [c.kind, c.payload.positionId]), [
    ['orgAssignmentEndRequest', undefined], ['orgAssignRequest', 'pos-ops'], ['orgAssignRequest', 'pos-dev'],
  ]);
  assert.equal(win.querySelector('.tf-act__error').getAttribute('message'), lt('errors.assignment_overlap'));
});

test('move: a vacancy in the person’s own unit is a target too', async () => {
  await mount();
  choose(openMenu('a-ceo'), lt('menu_move'));
  assert.deepEqual([...lastWindow().querySelectorAll('tf-radio')].map((r) => r.getAttribute('value')), ['pos-ops']);
});

test('move: with no vacant seat at all the window says so instead of offering nothing', async () => {
  await mount();
  unmountListTab();
  document.body.innerHTML = '<div id="org-root"><span id="org-list-actions"></span><div id="host"></div></div>';
  const full = input();
  full.view.assignments.push({ id: 'a-ops', position_id: 'pos-ops', subject: jan, display_name: 'Jan Ops', share: 1, assignment_type: 'permanent', is_primary: true, valid_from: '2026-01-01' });
  full.view.vacancies = [];
  mountListTab(document.getElementById('host'), full, { reload: async () => {} });
  choose(openMenu('a-ceo'), lt('menu_move'));
  assert.equal(lastWindow().querySelector('tf-alert[tone="warning"]').getAttribute('message'), I18n.t('actions.move.empty'));
});

test('assign a person to a vacancy: the chosen account, share, type and day are sent', async () => {
  await mount();
  stubTransport({ orgAssignRequest: ok({ kind: 'assignment', value: { id: 'a-new' } }) });
  choose(openMenu('vacant:pos-ops'), lt('menu_assign'));
  await sleep(0);
  const win = lastWindow();
  assert.deepEqual([...win.querySelectorAll('tf-person-picker [data-id]')].map((n) => n.dataset.id), ['u-new'], 'only active accounts');
  win.querySelector('tf-person-picker [data-id="u-new"]').click();
  submit(win);
  await closed();
  assert.deepEqual(calls[0], {
    kind: 'orgAssignRequest',
    payload: { positionId: 'pos-ops', subject: { kind: 'user', id: 'u-new' }, assignmentType: 'permanent', share: 1, validFrom: '2026-09-30' },
  });
  toastUndo().click();
  await sleep(10);
  assert.deepEqual(calls[1], { kind: 'orgAssignmentEndRequest', payload: { assignmentId: 'a-new', from: '2026-09-30', confirmBackdated: false } });
});

test('end a vacant position sends its end from the chosen day', async () => {
  await mount();
  stubTransport({ orgPositionEndRequest: ok({ kind: 'ended', value: {} }) });
  choose(openMenu('vacant:pos-ops'), lt('menu_end_position'));
  submit(lastWindow());
  await closed();
  assert.deepEqual(calls[0], { kind: 'orgPositionEndRequest', payload: { positionId: 'pos-ops', from: '2026-09-30' } });
  stubTransport({ orgPositionCreateRequest: ok({ kind: 'position', value: { position_id: 'pos-back' } }) });
  toastUndo().click();
  await sleep(10);
  assert.deepEqual(calls[1].payload, {
    unitId: 'unit-board', name: 'Assistant', parentPositionId: 'pos-ceo', isStaff: false, validFrom: '2026-09-30', confirmBackdated: false,
  });
});

test('add a position: it is created under the chosen unit and manager, then the person is assigned to it', async () => {
  await mount();
  stubTransport({
    orgPositionCreateRequest: ok({ kind: 'position', value: { position_id: 'pos-new' } }),
    orgAssignRequest: ok({ kind: 'assignment', value: { id: 'a-new' } }),
  });
  toolButton('add').click();
  await sleep(0);
  const win = lastWindow();
  field(win, `${lt('f_position_name')} *`).value = 'Analyst';
  field(win, `${I18n.t('org_structure.col_unit')} *`).value = 'unit-it';
  field(win, lt('f_reports_to')).value = 'pos-ceo';
  field(win, lt('f_person')).value = 'u-new';
  submit(win);
  await closed();
  assert.deepEqual(calls.map((c) => c.kind), ['orgPositionCreateRequest', 'orgAssignRequest']);
  assert.deepEqual(calls[0].payload, { unitId: 'unit-it', name: 'Analyst', parentPositionId: 'pos-ceo', validFrom: '2026-09-30' });
  assert.deepEqual(calls[1].payload, {
    positionId: 'pos-new', subject: { kind: 'user', id: 'u-new' }, assignmentType: 'permanent', share: 1,
    validFrom: '2026-09-30', confirmBackdated: false,
  });
  // Ending the new position on its first day removes it together with the person assigned to it.
  toastUndo().click();
  await sleep(10);
  assert.deepEqual(calls[2], { kind: 'orgPositionEndRequest', payload: { positionId: 'pos-new', from: '2026-09-30', confirmBackdated: false } });
});

test('add a position without a person creates a vacancy only', async () => {
  await mount();
  stubTransport({ orgPositionCreateRequest: ok({ kind: 'position', value: { position_id: 'pos-new' } }) });
  toolButton('add').click();
  await sleep(0);
  const win = lastWindow();
  field(win, `${lt('f_position_name')} *`).value = 'Analyst';
  field(win, `${I18n.t('org_structure.col_unit')} *`).value = 'unit-it';
  submit(win);
  await closed();
  assert.deepEqual(calls.map((c) => c.kind), ['orgPositionCreateRequest']);
  assert.equal(calls[0].payload.parentPositionId, null);
  assert.ok(toastUndo(), 'a created vacancy can be taken back too');
});

test('add a position: a refused person does not leave the window open to create the position twice', async () => {
  await mount();
  stubTransport({
    orgPositionCreateRequest: ok({ kind: 'position', value: { position_id: 'pos-new' } }),
    orgAssignRequest: refused('assignment_overlap'),
  });
  toolButton('add').click();
  await sleep(0);
  const win = lastWindow();
  field(win, `${lt('f_position_name')} *`).value = 'Analyst';
  field(win, `${I18n.t('org_structure.col_unit')} *`).value = 'unit-it';
  field(win, lt('f_person')).value = 'u-new';
  submit(win);
  await closed();
  assert.equal(win.isConnected, false);
  const messages = [...document.querySelectorAll('.tf-toast-message')].map((m) => m.textContent);
  assert.ok(messages.some((m) => m.includes(lt('errors.assignment_overlap'))), messages.join('|'));
});

test('a required field left empty is marked and nothing is sent', async () => {
  await mount();
  toolButton('add').click();
  await sleep(0);
  const win = lastWindow();
  submit(win);
  await sleep(0);
  assert.equal(calls.length, 0);
  assert.equal(field(win, `${lt('f_position_name')} *`).getAttribute('error'), I18n.t('actions.required_field'));
});

// --- refusals and the backdated confirmation -------------------------------------------------------------

test('a refusal from the server stays in the window as the sentence of its rule', async () => {
  await mount();
  stubTransport({ orgAssignmentEndRequest: refused('position_is_head') });
  choose(openMenu('a-dev'), lt('menu_end'));
  const win = lastWindow();
  submit(win);
  await sleep(10);
  assert.equal(win.isConnected, true);
  assert.equal(win.querySelector('.tf-act__error').getAttribute('message'), lt('errors.position_is_head'));
  assert.equal(reloads, 0);
});

test('a rule this build has no sentence for still names its code', async () => {
  await mount();
  stubTransport({ orgAssignmentEndRequest: refused('rule_from_the_future') });
  choose(openMenu('a-dev'), lt('menu_end'));
  const win = lastWindow();
  submit(win);
  await sleep(10);
  assert.equal(win.querySelector('.tf-act__error').getAttribute('message'), lt('error_unknown', { code: 'rule_from_the_future' }));
});

test('a backdated change asks once and is then sent again confirmed', async () => {
  await mount();
  let attempt = 0;
  stubTransport({
    orgAssignmentEndRequest: () => {
      attempt += 1;
      return attempt === 1 ? refused('backdated_confirmation_required', { date: '2026-09-01' }) : ok({ kind: 'done' });
    },
  });
  choose(openMenu('a-dev'), lt('menu_end'));
  const win = lastWindow();
  field(win, `${lt('f_end_from')} *`).value = '2026-09-01';
  submit(win);
  await sleep(10);
  const confirm = lastWindow();
  assert.notEqual(confirm, win, 'a confirmation window opened on top');
  assert.ok(confirm.querySelector('.tf-act__subject').textContent.includes(formatDay('2026-09-01')));
  submit(confirm);
  await closed();
  await closed();
  assert.deepEqual(calls.map((c) => [c.kind, c.payload.confirmBackdated]), [
    ['orgAssignmentEndRequest', undefined], ['orgAssignmentEndRequest', true],
  ]);
  assert.equal(win.isConnected, false, 'the end went through');
});

test('declining the backdated confirmation sends nothing more and says so in the window', async () => {
  await mount();
  stubTransport({ orgAssignmentEndRequest: refused('backdated_confirmation_required', { date: '2026-09-01' }) });
  choose(openMenu('a-dev'), lt('menu_end'));
  const win = lastWindow();
  field(win, `${lt('f_end_from')} *`).value = '2026-09-01';
  submit(win);
  await sleep(10);
  lastWindow().querySelector('[data-act="cancel"]').click();
  await closed();
  assert.equal(calls.length, 1);
  assert.equal(win.querySelector('.tf-act__error').getAttribute('message'), lt('backdated_declined'));
});

test('the server warnings of a write are shown as warning toasts', async () => {
  await mount();
  stubTransport({
    orgAssignmentEndRequest: ok({ kind: 'done' }, [{ kind: 'unit_without_head', unit_id: 'unit-it', from: '2026-09-30' }]),
  });
  choose(openMenu('a-dev'), lt('menu_end'));
  submit(lastWindow());
  await closed();
  const warning = document.querySelector('tf-toast[tone="warning"] .tf-toast-message');
  assert.ok(warning, 'a warning toast');
  assert.ok(warning.textContent.includes('IT'), warning.textContent);
});

// --- export and import --------------------------------------------------------------------------------------

test('export asks the server for the chosen format on the day shown', async () => {
  await mount([]);
  stubTransport({ orgExportRequest: { file_name: 'org.csv', mime: 'text/csv', bytes: new Uint8Array([1]) } });
  toolButton('export').click();
  choose(document.querySelector('tf-menu'), lt('export_csv'));
  await sleep(0);
  toolButton('export').click();
  choose(document.querySelector('tf-menu'), lt('export_xlsx'));
  await sleep(0);
  assert.deepEqual(calls.map((c) => [c.kind, c.payload]), [
    ['orgExportRequest', { format: 'csv', at: '2026-09-30' }],
    ['orgExportRequest', { format: 'xlsx', at: '2026-09-30' }],
  ]);
});

test('a failed export is reported instead of vanishing', async () => {
  await mount([]);
  stubTransport({ orgExportRequest: () => { throw new Error('denied'); } });
  toolButton('export').click();
  choose(document.querySelector('tf-menu'), lt('export_csv'));
  await sleep(0);
  const messages = [...document.querySelectorAll('.tf-toast-message')].map((m) => m.textContent);
  assert.ok(messages.includes(lt('export_failed', { message: 'denied' })), messages.join('|'));
});

test('the header button opens the import window', async () => {
  await mount();
  headerButton('import').click();
  assert.ok(document.querySelector('tf-window.org-imp-window'));
});

// --- focus ---------------------------------------------------------------------------------------------------

test('after a write the focus returns to the button of the same row, although the rows were drawn again', async () => {
  await mount();
  stubTransport({ orgAssignmentUpdateRequest: ok({ kind: 'assignment', value: { id: 'a-dev-2' } }) });
  choose(openMenu('a-dev'), lt('menu_edit'));
  const win = lastWindow();
  field(win, `${lt('f_share')} *`).value = '0,5';
  // The rows are redrawn while the window is open: a new button now stands where the old one was.
  refreshListTab(input());
  table().rows = table().rows.map((r) => ({ ...r }));
  const focused = [];
  const proto = Object.getPrototypeOf(document.createElement('button'));
  const focus = proto.focus;
  proto.focus = function spy() { focused.push(this.closest?.('tf-button')?.dataset.rowId ?? this.tagName); return focus.call(this); };
  try {
    submit(win);
    await closed();
  } finally {
    proto.focus = focus;
  }
  assert.ok(focused.includes('a-dev'), `focus went to ${focused.join(',')}`);
});

// =============================================================================
// File: modules/org-structure/profile-cover.test.js
// Description: The org sections of the profile page against a stubbed transport.
//   What has to hold: absences, "who covers me" and "whom I cover" draw what the
//   server answered (the reason only when the answer carries it); a person adds,
//   changes and deletes their own absences with the wire's exclusive end date
//   while the window talks in the last day; an absence that came from another
//   source has no menu for its person; deputies are offered to administrators
//   only, and the undo of each write sends the inverse.
// =============================================================================

import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import { window, I18n, sleep, closed, cleanBody } from '../../lib/actions/_test-setup.js';

process.on('unhandledRejection', () => {});

const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
const { mountProfileCover } = await import('./profile-cover.js');

const ct = (key, params) => I18n.t(`org_structure.cover.${key}`, params);

const mine = { id: 'abs-1', user_id: 'u-me', valid_from: '2026-09-29', valid_to: '2026-10-03', kind: 'leave', reason: 'dentist', source: 'manual' };
const later = { id: 'abs-2', user_id: 'u-me', valid_from: '2026-12-14', valid_to: '2026-12-15', kind: 'training', reason: null, source: 'manual' };
const imported = { id: 'abs-3', user_id: 'u-me', valid_from: '2026-11-02', valid_to: '2026-11-09', kind: 'leave', reason: null, source: 'edokumenty' };
const deputy = {
  id: 'dep-1', user_id: 'u-me', user_name: 'Marek Nowak', deputy_user_id: 'u-pz', deputy_name: 'Piotr Zieliński',
  scope: 'approvals', valid_from: '2026-10-20', valid_to: '2026-10-25',
};
const covering = {
  id: 'dep-2', user_id: 'u-ew', user_name: 'Ewa Wiśniewska', deputy_user_id: 'u-me', deputy_name: 'Marek Nowak',
  scope: 'all', valid_from: '2026-09-01', valid_to: null,
};

const cover = (over = {}) => ({
  user_id: 'u-me',
  display_name: 'Marek Nowak',
  available: false,
  today: '2026-09-30',
  absences: [mine, later, imported],
  covered_by: [deputy],
  covering: [covering],
  can_see_absences: true,
  can_see_reason: true,
  can_edit_absences: true,
  can_edit_deputies: true,
  is_admin: false,
  ...over,
});

const calls = [];
let script = {};
let current = cover();

function stubTransport(handlers = {}) {
  script = handlers;
  ApiBinary.one = (kind, payload) => {
    calls.push({ kind, payload });
    if (kind === 'orgCoverRequest') return Promise.resolve(current);
    if (kind === 'orgMemberListRequest') {
      return Promise.resolve({ members: [{ user_id: 'u-me', display_name: 'Marek Nowak' }, { user_id: 'u-pz', display_name: 'Piotr Zieliński' }] });
    }
    const handler = script[kind];
    if (!handler) return Promise.reject(new Error(`unexpected request ${kind}`));
    return Promise.resolve(typeof handler === 'function' ? handler(payload) : handler);
  };
}

const ok = (result = null) => ({ ok: true, error: null, warnings: [], result });
let dispose = null;

async function mount(over = {}) {
  cleanBody();
  calls.length = 0;
  current = cover(over);
  document.body.innerHTML = '<div id="host"></div>';
  dispose = await mountProfileCover(document.getElementById('host'));
  await sleep(0);
}

beforeEach(() => {
  dispose?.();
  dispose = null;
  stubTransport({});
});

const section = (id) => document.getElementById(id);
const rows = (id) => [...section(id).querySelectorAll('.org-cover-row')];
const text = (el) => el.textContent.replace(/\s+/g, ' ').trim();
const menuFor = (row) => {
  row.querySelector('[data-act$="-menu"]').click();
  return document.querySelector('tf-menu');
};
const choose = (menu, label) => [...menu.querySelectorAll('tf-menu-item')]
  .find((i) => i.getAttribute('label') === label).querySelector('.tf-menu-item').click();
const lastWindow = () => [...document.querySelectorAll('tf-window.tf-act-window')].pop();
const field = (win, label) => win.querySelector(`[label="${label}"]`);
const submit = (win) => win.querySelector('[data-act="submit"]').click();
const toastUndo = () => [...document.querySelectorAll('tf-toast tf-button')].pop();

test('the three sections draw what the server answered, the running absence first', async () => {
  await mount();
  const titles = [...document.querySelectorAll('.org-cover-head h2')].map((h) => h.textContent);
  assert.deepEqual(titles.filter((t) => [ct('absences_title'), ct('covered_by_title'), ct('covering_title')].includes(t)), [ct('absences_title'), ct('covered_by_title'), ct('covering_title')]);

  const absences = rows('org-cover-absences');
  assert.equal(absences.length, 3);
  assert.match(text(absences[0]), /leave|Leave/i);
  assert.match(text(absences[0]), /dentist/, 'the reason is there because the answer carries it');
  assert.match(text(absences[0]), new RegExp(ct('phase_current')));
  assert.match(text(absences[0]), /4 days/, 'the 29th to the 2nd, inclusive of the exclusive end');
  assert.match(text(absences[1]), new RegExp(ct('phase_upcoming')));

  assert.match(text(rows('org-cover-covered-by')[0]), /Piotr Zieliński/);
  assert.match(text(rows('org-cover-covered-by')[0]), new RegExp(ct('scope_approvals')));
  assert.match(text(rows('org-cover-covering')[0]), /Ewa Wiśniewska/);
});

test('without the reason in the answer none is drawn', async () => {
  await mount({ can_see_reason: false, absences: [{ ...mine, reason: null }] });
  assert.doesNotMatch(text(section('org-cover-absences')), /dentist/);
});

test('an empty profile says so in each section', async () => {
  await mount({ absences: [], covered_by: [], covering: [] });
  assert.equal(section('org-cover-absences').querySelector('.org-cover-empty').textContent, ct('absence_empty'));
  assert.equal(section('org-cover-covered-by').querySelector('.org-cover-empty').textContent, ct('covered_by_empty'));
  assert.equal(section('org-cover-covering').querySelector('.org-cover-empty').textContent, ct('covering_empty'));
});

test('a person who is not a member of an organization gets no sections', async () => {
  cleanBody();
  ApiBinary.one = () => Promise.reject(new Error('not found'));
  document.body.innerHTML = '<div id="host">old</div>';
  dispose = await mountProfileCover(document.getElementById('host'));
  assert.equal(document.getElementById('host').innerHTML, '');
});

test('a person sets their own deputies and no note sends them to an administrator; other people\'s rows stay with administrators', async () => {
  await mount();
  assert.ok(document.querySelector('[data-act="deputy-add"]'));
  assert.equal(document.querySelectorAll('#org-cover-covered-by [data-act="deputy-menu"]').length, 1);
  assert.equal(section('org-cover-covered-by').querySelector('.org-cover-note'), null);
  assert.equal(document.querySelectorAll('#org-cover-covering [data-act="deputy-menu"]').length, 0, 'the row of somebody else');

  await mount({ can_edit_deputies: false });
  assert.equal(document.querySelector('[data-act="deputy-add"]'), null);
  assert.equal(document.querySelector('[data-act="deputy-menu"]'), null);

  await mount({ is_admin: true });
  assert.equal(document.querySelectorAll('[data-act="deputy-menu"]').length, 2);
});

test('an absence that came from another source has no menu for its person, but has one for an administrator', async () => {
  await mount();
  const menus = rows('org-cover-absences').map((r) => Boolean(r.querySelector('[data-act="absence-menu"]')));
  assert.deepEqual(menus, [true, false, true], 'the imported one is the second, by date');
  await mount({ is_admin: true });
  assert.deepEqual(rows('org-cover-absences').map((r) => Boolean(r.querySelector('[data-act="absence-menu"]'))), [true, true, true]);
});

test('a person who may not edit absences (somebody else\'s) gets no add button and no menus', async () => {
  await mount({ can_edit_absences: false });
  assert.equal(document.querySelector('[data-act="absence-add"]'), null);
  assert.equal(document.querySelector('[data-act="absence-menu"]'), null);
});

test('add: the window asks for the last day, the wire gets the exclusive end, and the undo deletes it', async () => {
  await mount();
  stubTransport({
    orgAbsenceAddRequest: ok({ kind: 'absence', value: { ...later, id: 'abs-new', valid_from: '2026-10-20', valid_to: '2026-10-25' } }),
    orgAbsenceDeleteRequest: ok({ kind: 'done' }),
  });
  document.querySelector('[data-act="absence-add"]').click();
  const win = lastWindow();
  assert.equal(field(win, `${ct('f_from')} *`).value, '2026-09-30', 'starts today');
  field(win, `${ct('f_from')} *`).value = '2026-10-20';
  field(win, ct('f_last')).value = '2026-10-24';
  field(win, `${ct('f_kind')} *`).value = 'leave';
  field(win, ct('f_reason')).value = 'family';
  submit(win);
  await closed();

  assert.deepEqual(calls.filter((c) => c.kind === 'orgAbsenceAddRequest').map((c) => c.payload), [{
    userId: null, validFrom: '2026-10-20', validTo: '2026-10-25', kind: 'leave', reason: 'family',
  }]);
  assert.equal(calls.filter((c) => c.kind === 'orgCoverRequest').length, 2, 'the section is read again');

  toastUndo().click();
  await sleep(10);
  assert.deepEqual(calls.find((c) => c.kind === 'orgAbsenceDeleteRequest').payload, { id: 'abs-new', confirmBackdated: false });
});

test('add: a last day before the first stays in the window as a sentence and nothing is sent', async () => {
  await mount();
  stubTransport({});
  document.querySelector('[data-act="absence-add"]').click();
  const win = lastWindow();
  field(win, `${ct('f_from')} *`).value = '2026-10-20';
  field(win, ct('f_last')).value = '2026-10-10';
  submit(win);
  await sleep(10);
  assert.equal(win.querySelector('.tf-act__error').getAttribute('message'), ct('errors.absence_order'));
  assert.equal(calls.filter((c) => c.kind === 'orgAbsenceAddRequest').length, 0);
});

test('edit: the window shows the last day, only what changed is sent, an emptied reason is cleared', async () => {
  await mount();
  stubTransport({ orgAbsenceUpdateRequest: ok({ kind: 'absence', value: { ...mine, valid_to: '2026-10-05', reason: null } }) });
  choose(menuFor(rows('org-cover-absences')[0]), ct('menu_edit'));
  const win = lastWindow();
  assert.equal(field(win, ct('f_last')).value, '2026-10-02', 'the inclusive last day of [29th, 3rd)');
  assert.equal(field(win, ct('f_reason')).value, 'dentist');
  field(win, ct('f_last')).value = '2026-10-04';
  field(win, ct('f_reason')).value = '';
  submit(win);
  await closed();
  assert.deepEqual(calls.filter((c) => c.kind === 'orgAbsenceUpdateRequest').map((c) => c.payload), [
    { id: 'abs-1', validTo: '2026-10-05', clear: ['reason'] },
  ]);
});

test('edit: saving without a change is refused in the window', async () => {
  await mount();
  stubTransport({});
  choose(menuFor(rows('org-cover-absences')[0]), ct('menu_edit'));
  const win = lastWindow();
  submit(win);
  await sleep(10);
  assert.equal(win.querySelector('.tf-act__error').getAttribute('message'), ct('nothing_changed'));
  assert.equal(calls.filter((c) => c.kind === 'orgAbsenceUpdateRequest').length, 0);
});

test('delete: confirming sends the delete and the undo adds the same absence again', async () => {
  await mount();
  stubTransport({ orgAbsenceDeleteRequest: ok({ kind: 'done' }), orgAbsenceAddRequest: ok({ kind: 'absence', value: mine }) });
  choose(menuFor(rows('org-cover-absences')[0]), ct('menu_delete'));
  submit(lastWindow());
  await closed();
  assert.deepEqual(calls.find((c) => c.kind === 'orgAbsenceDeleteRequest').payload, { id: 'abs-1' });

  toastUndo().click();
  await sleep(10);
  assert.deepEqual(calls.find((c) => c.kind === 'orgAbsenceAddRequest').payload, {
    userId: 'u-me', validFrom: '2026-09-29', validTo: '2026-10-03', kind: 'leave', reason: 'dentist', confirmBackdated: false,
  });
});

test('a duplicate deputy is refused with this screen\'s sentence', async () => {
  await mount();
  stubTransport({ orgDeputySetRequest: { ok: false, error: { code: 'duplicate', message: 'x' }, warnings: [], result: null } });
  document.querySelector('[data-act="deputy-add"]').click();
  await sleep(10);
  const win = lastWindow();
  win.querySelector('tf-person-picker [data-id="u-pz"]').click();
  submit(win);
  await sleep(10);
  assert.equal(win.querySelector('.tf-act__error').getAttribute('message'), ct('errors.duplicate_deputy'));
});

test('add deputy: the account list leaves the person out, the wire gets scope and exclusive end, the undo ends it', async () => {
  await mount();
  stubTransport({
    orgDeputySetRequest: ok({ kind: 'deputy', value: { ...deputy, id: 'dep-new' } }),
    orgDeputyEndRequest: ok({ kind: 'done' }),
  });
  document.querySelector('[data-act="deputy-add"]').click();
  await sleep(10);
  const win = lastWindow();
  assert.deepEqual([...win.querySelectorAll('tf-person-picker [data-id]')].map((n) => n.dataset.id), ['u-pz'], 'not the person themself');
  win.querySelector('tf-person-picker [data-id="u-pz"]').click();
  field(win, `${ct('f_scope')} *`).value = 'escalations';
  field(win, ct('f_last')).value = '2026-10-10';
  submit(win);
  await closed();
  assert.deepEqual(calls.find((c) => c.kind === 'orgDeputySetRequest').payload, {
    userId: 'u-me', deputyUserId: 'u-pz', scope: 'escalations', validFrom: '2026-09-30', validTo: '2026-10-11',
  });
  toastUndo().click();
  await sleep(10);
  assert.deepEqual(calls.find((c) => c.kind === 'orgDeputyEndRequest').payload, { id: 'dep-new', from: '2026-09-30', confirmBackdated: false });
});

test('end a deputy sends the end day, and the undo appoints the same one again', async () => {
  await mount();
  stubTransport({ orgDeputyEndRequest: ok({ kind: 'done' }), orgDeputySetRequest: ok({ kind: 'deputy', value: deputy }) });
  choose(menuFor(rows('org-cover-covered-by')[0]), ct('menu_end'));
  submit(lastWindow());
  await closed();
  assert.deepEqual(calls.find((c) => c.kind === 'orgDeputyEndRequest').payload, { id: 'dep-1', from: '2026-09-30' });
  toastUndo().click();
  await sleep(10);
  assert.deepEqual(calls.find((c) => c.kind === 'orgDeputySetRequest').payload, {
    userId: 'u-me', deputyUserId: 'u-pz', scope: 'approvals', validFrom: '2026-10-20', validTo: '2026-10-25', confirmBackdated: false,
  });
});

test('edit a deputy: only the changed fields are sent and the last day clears the end when emptied', async () => {
  await mount();
  stubTransport({ orgDeputyUpdateRequest: ok({ kind: 'deputy', value: deputy }) });
  choose(menuFor(rows('org-cover-covered-by')[0]), ct('menu_edit'));
  const win = lastWindow();
  assert.equal(field(win, ct('f_last')).value, '2026-10-24');
  field(win, ct('f_last')).value = '';
  field(win, `${ct('f_scope')} *`).value = 'all';
  submit(win);
  await closed();
  assert.deepEqual(calls.find((c) => c.kind === 'orgDeputyUpdateRequest').payload, { id: 'dep-1', scope: 'all', clear: ['valid_to'] });
});

test('a window\'s toast and the section survive the server refusing a change', async () => {
  await mount();
  stubTransport({ orgAbsenceAddRequest: { ok: false, error: { code: 'invalid_value', message: 'x' }, warnings: [], result: null } });
  document.querySelector('[data-act="absence-add"]').click();
  const win = lastWindow();
  field(win, ct('f_reason')).value = 'x';
  submit(win);
  await sleep(10);
  assert.equal(win.querySelector('.tf-act__error').getAttribute('message'), ct('errors.invalid_value'));
  assert.equal(rows('org-cover-absences').length, 3, 'nothing was redrawn');
});

test('the handover section lists what was handed over for absences and opens the handover screen for the person', async () => {
  const record = (over) => ({
    id: 'h-1', user_id: 'u-me', reason: 'absence', date: '2026-09-30', return_date: '2026-10-10', note: 'Galaz feature/opc',
    items: [{ status: 'done' }, { status: 'done' }, { status: 'kept' }], ...over,
  });
  stubTransport({
    orgHandoverRecordsRequest: { records: [record({}), record({ id: 'h-2', reason: 'departure' })] },
  });
  await mount();
  const hv = (key, params) => I18n.t(`org_structure.handover.${key}`, params);
  assert.equal(section('org-cover-handover').querySelector('h2').textContent, hv('profile_title'));
  const handed = rows('org-cover-handover');
  assert.equal(handed.length, 1, 'only the temporary handovers of an absence are listed');
  assert.match(text(handed[0]), new RegExp(hv('profile_done', { count: 3 })));
  assert.match(text(handed[0]), new RegExp(hv('profile_kept', { count: 1 })));
  assert.match(text(handed[0]), /Galaz feature\/opc/);
  assert.match(text(handed[0]), new RegExp(ct('phase_current')), 'away until the return day');

  const { Router } = await import('/js/router.js');
  const navigations = [];
  Router.navigate = (id, params) => { navigations.push([id, params]); return Promise.resolve(true); };
  section('org-cover-handover').querySelector('[data-act="handover-open"]').click();
  assert.deepEqual(navigations, [['org-structure', { tab: 'list', handover: 'u-me', reason: 'absence' }]]);
});

test('with no handover record the section says so and the rest of the profile is unaffected', async () => {
  await mount();
  assert.equal(section('org-cover-handover').querySelector('.org-cover-empty').textContent, I18n.t('org_structure.handover.profile_empty'));
  assert.equal(rows('org-cover-absences').length, 3);
});

void window;

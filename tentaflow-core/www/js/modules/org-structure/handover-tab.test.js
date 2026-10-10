// =============================================================================
// File: modules/org-structure/handover-tab.test.js
// Description: The handover screen against a stubbed transport. What has to
//   hold: the screen asks the server what the person holds for the reason and
//   day on screen and draws it grouped, every row with its proposed taker and
//   the reason for it; a request is not sent without a note, without a taker
//   where one is required or without a return day for an absence; "hand
//   everything to" changes only the selected rows the person may take; the
//   request carries exactly the ticked rows; a partial failure stays on screen
//   per item and "retry" sends only the failed keys; rows the server blocked
//   cannot be ticked; a failed read says so.
// =============================================================================

import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import { window, I18n, sleep, cleanBody } from '../../lib/actions/_test-setup.js';

process.on('unhandledRejection', () => {});

const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
const { mountHandoverTab, unmountHandoverTab, reasonText } = await import('./handover-tab.js');
const { formatDay } = await import('/js/lib/date-format.js');

const ht = (key, params) => I18n.t(`org_structure.handover.${key}`, params);

const anna = { user_id: 'u-anna', display_name: 'Anna Kowalska' };
const marek = { user_id: 'u-marek', display_name: 'Marek Nowak' };
const ewa = { user_id: 'u-ewa', display_name: 'Ewa Wiśniewska' };

const groups = [
  {
    category: 'task',
    items: [
      {
        key: 'task:p-1:t-1', category: 'task', title: '#231 Import OPC zawiesza się', role: 'assignee', state: 'in_progress',
        project_id: 'p-1', project_name: 'NextApp', action: 'transfer',
        suggestion: { user_id: 'u-anna', reason: 'deputy' }, eligible_user_ids: ['u-anna', 'u-marek'],
      },
      {
        key: 'task:p-2:t-2', category: 'task', title: '#7 Inny projekt', role: 'assignee', state: 'todo',
        project_id: 'p-2', project_name: 'Energetyka', action: 'transfer', suggestion: null, eligible_user_ids: ['u-marek'],
      },
    ],
  },
  {
    category: 'membership',
    items: [{
      key: 'member:p-1', category: 'membership', title: 'NextApp', role: 'editor', state: '', project_id: 'p-1', project_name: 'NextApp',
      action: 'end', suggestion: null, eligible_user_ids: null,
    }],
  },
  {
    category: 'position',
    items: [{
      key: 'position:a-1', category: 'position', title: 'Programista', role: 'member', state: '', unit_name: 'Dział IT',
      action: 'transfer_or_end', suggestion: { user_id: 'u-ewa', reason: 'manager' }, eligible_user_ids: ['u-anna', 'u-marek', 'u-ewa'],
    }],
  },
  {
    category: 'deputy',
    items: [{
      key: 'deputy:d-1', category: 'deputy', title: 'Ewa Wiśniewska', role: 'deputy', state: 'all', action: 'transfer_or_end',
      suggestion: null, eligible_user_ids: ['u-anna'], valid_to: '2026-12-01', blocked: 'project_archived',
    }],
  },
];

const listing = (over = {}) => ({
  variant: 'OrgStructureHandoverListResponse',
  user: { user_id: 'u-leaver', display_name: 'Piotr Zieliński' },
  reason: 'departure',
  date: '2026-10-31',
  return_date: null,
  assignment_ended_on: null,
  project_name: null,
  groups,
  takers: [marek, anna, ewa],
  ...over,
});

const calls = [];
let answers = {};
let failList = false;

beforeEach(() => {
  unmountHandoverTab();
  calls.length = 0;
  answers = {};
  failList = false;
  ApiBinary.one = (kind, payload) => {
    calls.push({ kind, payload });
    if (kind === 'orgHandoverListRequest') {
      if (failList) return Promise.reject(new Error('boom'));
      return Promise.resolve(answers.list ?? listing({ reason: payload.reason }));
    }
    const answer = answers[kind];
    if (!answer) return Promise.reject(new Error(`unexpected request ${kind}`));
    return Promise.resolve(typeof answer === 'function' ? answer(payload) : answer);
  };
});

let backs = 0;

let changes = 0;

async function mount({ isAdmin = true, target = { userId: 'u-leaver', reason: 'departure', projectId: null } } = {}) {
  cleanBody();
  backs = 0;
  changes = 0;
  document.body.innerHTML = '<div id="host"></div>';
  await mountHandoverTab(document.getElementById('host'), {
    target, isAdmin, onBack: () => { backs += 1; }, onChanged: () => { changes += 1; },
  });
  await sleep(0);
}

const $ = (selector) => document.querySelector(selector);
const $$ = (selector) => [...document.querySelectorAll(selector)];
const text = (el) => el.textContent.replace(/\s+/g, ' ').trim();
const rowOf = (key) => $(`[data-row="${key}"]`);

function click(el) {
  el.dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
}

function change(el, detail) {
  el.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail }));
}

const listCalls = () => calls.filter((c) => c.kind === 'orgHandoverListRequest');

test('the screen asks for what the person holds for the reason and draws it grouped, in screen order', async () => {
  await mount();
  assert.deepEqual(listCalls()[0].payload, { userId: 'u-leaver', reason: 'departure', projectId: null, date: null });
  assert.equal(text($('.org-ho-title h2')), ht('title', { name: 'Piotr Zieliński' }));
  assert.equal(text($('.org-ho-title tf-chip')), ht('items_count', { count: 5 }));
  assert.deepEqual($$('.org-ho-group').map((g) => g.dataset.group), ['task', 'membership', 'position', 'deputy']);
  assert.deepEqual($$('.org-ho-group[data-group="task"] .org-ho-item b').map(text), ['#231 Import OPC zawiesza się', '#7 Inny projekt']);
  assert.match(text(rowOf('task:p-1:t-1').querySelector('small')), new RegExp(`${ht('role_assignee')} · ${ht('state_in_progress')} · NextApp`));
  assert.match(text(rowOf('position:a-1').querySelector('small')), /Dział IT/);
});

test('every row shows its proposed taker with the reason; a row without a proposal asks for a choice', async () => {
  await mount();
  const select = (key) => rowOf(key).querySelector('tf-select');
  assert.equal(select('task:p-1:t-1').getAttribute('value'), 'u-anna');
  assert.equal(text(rowOf('task:p-1:t-1').querySelector('.org-ho-why')), ht('why_deputy'));
  // Only people who may take it are offered.
  assert.deepEqual([...select('task:p-1:t-1').querySelectorAll('option')].map((o) => o.value), ['u-anna', 'u-marek']);
  assert.equal(text(rowOf('task:p-2:t-2').querySelector('.org-ho-why')), ht('why_none'));
  assert.equal([...select('task:p-2:t-2').querySelectorAll('option')][0].value, '', 'a placeholder, not a silent first person');
  assert.equal(text(rowOf('position:a-1').querySelector('.org-ho-why')), ht('why_manager'));
  assert.ok([...select('position:a-1').querySelectorAll('option')].some((o) => o.value === '' && text(o) === ht('taker_vacant')), 'a seat can be left vacant');
  // A membership only ends.
  assert.equal(rowOf('member:p-1').querySelector('tf-select'), null);
  assert.equal(text(rowOf('member:p-1').querySelector('.org-ho-end')), ht('end_membership', { date: formatDay('2026-10-31') }));
});

test('a row the server blocked is shown but cannot be ticked', async () => {
  await mount();
  const box = rowOf('deputy:d-1').querySelector('tf-checkbox');
  assert.equal(box.hasAttribute('disabled'), true);
  assert.equal(box.hasAttribute('checked'), false);
  assert.equal(text(rowOf('deputy:d-1').querySelector('.org-ho-why')), ht('why_blocked_project_archived'));
  assert.equal(text($('[data-role="selected"]')), ht('selected', { count: 4 }));
});

test('the submit button counts the ticked rows and follows a change of the selection', async () => {
  await mount();
  const submit = $('[data-role="submit"]');
  assert.equal(submit.getAttribute('label'), ht('submit', { count: 4 }));
  change(rowOf('task:p-2:t-2').querySelector('tf-checkbox'), { checked: false });
  assert.equal(submit.getAttribute('label'), ht('submit', { count: 3 }));
  change($('[data-role="pick-all"]'), { checked: false });
  assert.equal(submit.getAttribute('label'), ht('submit', { count: 0 }));
  change($('[data-role="pick-all"]'), { checked: true });
  assert.equal(submit.getAttribute('label'), ht('submit', { count: 4 }), 'select all does not tick what is blocked');
});

test('nothing is sent without a note: the note field says so and takes focus back', async () => {
  await mount();
  // The second task has no proposal either; give it a person so only the note is missing.
  change(rowOf('task:p-2:t-2').querySelector('tf-select'), { value: 'u-marek' });
  click($('[data-act="submit"]'));
  await sleep(0);
  assert.equal(calls.filter((c) => c.kind === 'orgHandoverApplyRequest').length, 0);
  assert.equal($('[data-role="note"]').getAttribute('error'), ht('note_missing'));
});

test('a row that needs a person and has none is marked and nothing is sent', async () => {
  await mount();
  const note = $('[data-role="note"]');
  note.value = 'Galaz feature/opc';
  click($('[data-act="submit"]'));
  await sleep(0);
  assert.equal(calls.filter((c) => c.kind === 'orgHandoverApplyRequest').length, 0);
  assert.ok(rowOf('task:p-2:t-2').classList.contains('org-ho-row-error'));
  assert.equal(rowOf('task:p-1:t-1').classList.contains('org-ho-row-error'), false);
  // Choosing the person clears the mark.
  change(rowOf('task:p-2:t-2').querySelector('tf-select'), { value: 'u-marek' });
  assert.equal(rowOf('task:p-2:t-2').classList.contains('org-ho-row-error'), false);
  assert.equal(text(rowOf('task:p-2:t-2').querySelector('.org-ho-why')), ht('why_manual'));
});

test('"hand everything to" changes the selected rows that person may take and tells how many it could not', async () => {
  await mount();
  change(rowOf('task:p-1:t-1').querySelector('tf-checkbox'), { checked: false });
  change($('[data-role="all-to"]'), { value: 'u-anna' });
  click($('[data-act="apply-all"]'));
  await sleep(0);
  // Task 1 was not selected, task 2 is in a project Anna is not in, the position takes her, the membership takes nobody.
  assert.equal(rowOf('task:p-1:t-1').querySelector('tf-select').getAttribute('value'), 'u-anna', 'unselected rows keep their proposal');
  assert.equal(rowOf('task:p-2:t-2').querySelector('tf-select').getAttribute('value') || '', '');
  assert.equal(text(rowOf('position:a-1').querySelector('.org-ho-why')), ht('why_manual'));
  const toasts = $$('tf-toast, .tf-toast').map(text).join(' ');
  assert.match(toasts, new RegExp(ht('toast_all_skipped', { count: 1 }).slice(0, 12)));
});

test('a complete apply sends exactly the ticked rows with their takers and reads the list again', async () => {
  answers.orgHandoverApplyRequest = (payload) => ({
    ok: true, handover_id: 'h-1', error: null, applied: 4, scheduled: 0, failed: 0,
    items: payload.items.map((i) => ({ key: i.key, category: 'task', title: i.key, status: 'done', reason: null, taker_user_id: i.takerUserId })),
  });
  await mount();
  change(rowOf('task:p-2:t-2').querySelector('tf-select'), { value: 'u-marek' });
  change(rowOf('position:a-1').querySelector('tf-select'), { value: '' });
  $('[data-role="note"]').value = '  Galaz feature/opc  ';
  const before = listCalls().length;
  click($('[data-act="submit"]'));
  await sleep(10);
  const sent = calls.find((c) => c.kind === 'orgHandoverApplyRequest').payload;
  assert.deepEqual(sent, {
    userId: 'u-leaver',
    reason: 'departure',
    projectId: null,
    date: '2026-10-31',
    returnDate: null,
    note: 'Galaz feature/opc',
    items: [
      { key: 'task:p-1:t-1', takerUserId: 'u-anna' },
      { key: 'task:p-2:t-2', takerUserId: 'u-marek' },
      { key: 'member:p-1', takerUserId: null },
      { key: 'position:a-1', takerUserId: null },
    ],
  });
  assert.equal(listCalls().length, before + 1, 'the list is read again after the apply');
  assert.equal(text($('.org-ho-result h3')), ht('result_title'));
  assert.equal(text($('.org-ho-result-chips')), ht('result_done', { count: 4 }));
  assert.equal($('[data-act="retry"]'), null, 'nothing failed, nothing to retry');
  assert.equal($('[data-role="note"]').value, '', 'a handover that went through starts the next one with an empty note');
});

test('a partial failure stays on screen per item with its rule, and retry sends only what failed', async () => {
  answers.orgHandoverApplyRequest = {
    ok: false, handover_id: 'h-9', error: null, applied: 1, scheduled: 0, failed: 2,
    items: [
      { key: 'task:p-1:t-1', category: 'task', title: '#231 Import OPC zawiesza się', status: 'done', reason: null, taker_user_id: 'u-anna', project_name: 'NextApp' },
      { key: 'task:p-2:t-2', category: 'task', title: '#7 Inny projekt', status: 'failed', reason: 'internal', taker_user_id: 'u-marek', project_name: 'Energetyka' },
      { key: 'member:p-1', category: 'membership', title: 'NextApp', status: 'failed', reason: 'still_holds_work', taker_user_id: null, project_name: 'NextApp' },
    ],
  };
  answers.orgHandoverRetryRequest = { ok: true, handover_id: 'h-9', error: null, applied: 2, scheduled: 0, failed: 0, items: [] };
  await mount();
  change(rowOf('task:p-2:t-2').querySelector('tf-select'), { value: 'u-marek' });
  $('[data-role="note"]').value = 'n';
  click($('[data-act="submit"]'));
  await sleep(10);
  const rows = $$('.org-ho-result-row').map(text);
  assert.equal(rows.length, 3);
  assert.match(rows[0], new RegExp(ht('status_failed')), 'failures come first');
  assert.match(rows.join(' '), new RegExp(ht('reasons.still_holds_work')));
  assert.match(rows.join(' '), new RegExp(ht('reasons.internal')));
  assert.equal($('[data-role="note"]').value, 'n', 'the note stays while something failed');
  click($('[data-act="retry"]'));
  await sleep(10);
  assert.deepEqual(calls.find((c) => c.kind === 'orgHandoverRetryRequest').payload, { handoverId: 'h-9', keys: ['task:p-2:t-2', 'member:p-1'] });
});

test('a refused request shows the rule and moves nothing', async () => {
  answers.orgHandoverApplyRequest = {
    ok: false, handover_id: null, error: { code: 'empty_field', message: 'note' }, items: [], applied: 0, scheduled: 0, failed: 0,
  };
  await mount();
  change(rowOf('task:p-2:t-2').querySelector('tf-select'), { value: 'u-marek' });
  $('[data-role="note"]').value = 'x';
  click($('[data-act="submit"]'));
  await sleep(10);
  assert.equal($('.org-ho-result tf-alert').getAttribute('message'), ht('reasons.empty_field'));
  assert.equal($('[data-act="retry"]'), null);
  assert.equal(listCalls().length, 1, 'nothing moved, so the list is not read again and the choices stay');
  assert.equal(rowOf('task:p-2:t-2').querySelector('tf-select').getAttribute('value'), 'u-marek');
  assert.equal($('[data-role="note"]').value, 'x');
});

test('an absence needs a return day, sends it, and asks the list for no date', async () => {
  answers.orgHandoverApplyRequest = { ok: true, handover_id: 'h-2', error: null, applied: 1, scheduled: 0, failed: 0, items: [] };
  await mount({ target: { userId: 'u-me', reason: 'absence', projectId: null }, isAdmin: false });
  assert.deepEqual(listCalls()[0].payload, { userId: 'u-me', reason: 'absence', projectId: null, date: null });
  assert.equal($('[data-role="reason"]') !== null, true);
  const ret = $('[data-role="return"]');
  assert.equal(ret.getAttribute('value'), '2026-11-07', 'a week after the day the server reported');
  change(ret, { value: '' });
  $('[data-role="note"]').value = 'Wracam';
  change(rowOf('task:p-2:t-2').querySelector('tf-select'), { value: 'u-marek' });
  click($('[data-act="submit"]'));
  await sleep(0);
  assert.equal(calls.filter((c) => c.kind === 'orgHandoverApplyRequest').length, 0, 'no return day, no request');
  change($('[data-role="return"]'), { value: '2026-11-09' });
  click($('[data-act="submit"]'));
  await sleep(10);
  const sent = calls.find((c) => c.kind === 'orgHandoverApplyRequest').payload;
  assert.deepEqual([sent.reason, sent.returnDate, sent.date], ['absence', '2026-11-09', null]);
});

test('an impossible return day is refused in the field, keeps its text and sends nothing', async () => {
  await mount({ target: { userId: 'u-me', reason: 'absence', projectId: null }, isAdmin: false });
  const field = $('[data-role="return"]');
  const inner = field.querySelector('input');
  inner.value = '31.02.2026';
  inner.dispatchEvent(new window.Event('input', { bubbles: true }));
  $('[data-role="note"]').value = 'Wracam';
  click($('[data-act="submit"]'));
  await sleep(10);
  assert.equal(calls.filter((c) => c.kind === 'orgHandoverApplyRequest').length, 0);
  assert.equal($('[data-role="return"]').querySelector('input').value, '31.02.2026', 'the typed text is not wiped');
  assert.match($('[data-role="return"]').querySelector('tf-input').getAttribute('error'), /DD\/MM\/YYYY/);
});

test('only the reasons the caller may use are offered', async () => {
  await mount({ isAdmin: true });
  const values = (segmented) => [...segmented.querySelectorAll('button, [data-value]')].map((b) => b.dataset.value ?? b.getAttribute('value')).filter(Boolean);
  assert.ok(values($('[data-role="reason"]')).includes('departure'));
  calls.length = 0;
  await mount({ isAdmin: false, target: { userId: 'u-me', reason: 'departure', projectId: null } });
  // A member asking for a departure gets the absence, the only reason they may use.
  assert.equal(listCalls()[0].payload.reason, 'absence');
  calls.length = 0;
  await mount({ isAdmin: false, target: { userId: 'u-x', reason: 'project_removal', projectId: 'p-1' } });
  assert.deepEqual(listCalls()[0].payload, { userId: 'u-x', reason: 'project_removal', projectId: 'p-1', date: null });
});

test('a person with nothing to hand over gets an empty state, not an empty table', async () => {
  answers.list = listing({ groups: [] });
  await mount();
  assert.ok($('tf-empty-state'));
  assert.equal($('tf-empty-state').getAttribute('title'), ht('empty_title'));
  assert.equal($('.org-ho-bulk').hidden, true);
});

test('a project the server left out of the list is named on the screen', async () => {
  answers.list = listing({ skipped_projects: ['Energetyka', 'NextApp'] });
  await mount();
  const message = $('[data-role="ctxnote"]').getAttribute('message');
  assert.ok(message.includes(ht('note_skipped_projects', { projects: 'Energetyka, NextApp' })), message);
  answers.list = listing();
  await mount();
  assert.ok(!$('[data-role="ctxnote"]').getAttribute('message').includes('Energetyka'));
});

test('a failed read says so on the screen', async () => {
  failList = true;
  await mount();
  assert.equal(text($('.org-ho-error')), ht('load_failed', { message: 'boom' }));
});

test('back leaves the screen through the caller', async () => {
  await mount();
  click($('[data-act="back"]'));
  assert.equal(backs, 1);
});

test('a code the screen has no sentence for still names the org rule', () => {
  assert.equal(reasonText('taker_not_eligible'), ht('reasons.taker_not_eligible'));
  assert.notEqual(reasonText('assignment_overlap'), ht('reasons.assignment_overlap'));
  assert.match(reasonText('assignment_overlap'), /\S/);
});

test('the surrounding screen is told when something moved, and not when nothing did', async () => {
  answers.orgHandoverApplyRequest = {
    ok: false, handover_id: null, error: { code: 'empty_field', message: 'note' }, items: [], applied: 0, scheduled: 0, failed: 0,
  };
  await mount();
  change(rowOf('task:p-2:t-2').querySelector('tf-select'), { value: 'u-marek' });
  $('[data-role="note"]').value = 'x';
  click($('[data-act="submit"]'));
  await sleep(10);
  assert.equal(changes, 0, 'a refused request changed nothing');
  answers.orgHandoverApplyRequest = {
    ok: true, handover_id: 'h-3', error: null, applied: 1, scheduled: 0, failed: 0,
    items: [{ key: 'task:p-1:t-1', category: 'task', title: 't', status: 'done', reason: null, taker_user_id: 'u-anna' }],
  };
  change(rowOf('task:p-2:t-2').querySelector('tf-select'), { value: 'u-marek' });
  $('[data-role="note"]').value = 'y';
  click($('[data-act="submit"]'));
  await sleep(10);
  assert.equal(changes, 1);
});

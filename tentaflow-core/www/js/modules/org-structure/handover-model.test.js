// =============================================================================
// File: modules/org-structure/handover-model.test.js
// Description: The pure part of the handover screen: which reasons a caller may
//   use, rows from the server's groups (proposal chosen, blocked items never
//   ticked), "hand everything to" respecting who may take what, the checks
//   before sending, the request that goes out and the summary of the answer.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  absenceRecords, allowedReasons, applyPayload, applyToSelected, canTake, grouped, initialReason,
  initials, problems, rowsOf, selectedRows, sortedTakers, summarize, takerOptions,
} from './handover-model.js';

const item = (over) => ({
  key: 'task:p-1:t-1',
  title: '#1 Import OPC',
  role: 'assignee',
  state: 'todo',
  project_id: 'p-1',
  project_name: 'NextApp',
  action: 'transfer',
  suggestion: { user_id: 'u-anna', reason: 'deputy' },
  eligible_user_ids: ['u-anna', 'u-marek'],
  ...over,
});

const groups = [
  { category: 'task', items: [item({}), item({ key: 'task:p-2:t-2', title: '#2 Inne', project_id: 'p-2', suggestion: null, eligible_user_ids: ['u-marek'] })] },
  { category: 'membership', items: [item({ key: 'member:p-1', title: 'NextApp', action: 'end', suggestion: null, eligible_user_ids: null })] },
  { category: 'position', items: [item({ key: 'position:a-1', title: 'Programista', action: 'transfer_or_end', suggestion: null, eligible_user_ids: null })] },
  { category: 'deputy', items: [item({ key: 'deputy:d-1', title: 'Anna', blocked: 'project_archived', action: 'transfer_or_end', suggestion: null })] },
];

test('a departure is the administrator\'s, a project removal exists only for a project, an absence is everybody\'s', () => {
  assert.deepEqual(allowedReasons({ isAdmin: true, projectId: null }), ['departure', 'absence']);
  assert.deepEqual(allowedReasons({ isAdmin: false, projectId: null }), ['absence']);
  assert.deepEqual(allowedReasons({ isAdmin: true, projectId: 'p-1' }), ['project_removal']);
  assert.deepEqual(allowedReasons({ isAdmin: false, projectId: 'p-1' }), ['project_removal']);
  assert.equal(initialReason('departure', ['absence']), 'absence', 'a reason the caller may not use is not offered');
  assert.equal(initialReason('absence', ['departure', 'absence']), 'absence');
});

test('rows start ticked with the proposed person as the taker, except what the server says is blocked', () => {
  const rows = rowsOf(groups);
  assert.equal(rows.length, 5);
  const first = rows[0];
  assert.deepEqual([first.selected, first.taker, first.manual], [true, 'u-anna', false]);
  assert.equal(rows[1].taker, '', 'no proposal, no taker');
  const blocked = rows.find((r) => r.key === 'deputy:d-1');
  assert.deepEqual([blocked.selected, blocked.blocked], [false, 'project_archived']);
  assert.deepEqual(selectedRows(rows).map((r) => r.key), ['task:p-1:t-1', 'task:p-2:t-2', 'member:p-1', 'position:a-1']);
  assert.deepEqual(grouped(rows).map((g) => g.category), ['task', 'membership', 'position', 'deputy'], 'screen order, empty groups left out');
});

test('who may take a row comes from the row, and a row that only ends takes nobody', () => {
  const rows = rowsOf(groups);
  assert.equal(canTake(rows[0], 'u-marek'), true);
  assert.equal(canTake(rows[0], 'u-ewa'), false, 'not among the eligible');
  assert.equal(canTake(rows[2], 'u-ewa'), false, 'a membership ends, nobody takes it');
  assert.equal(canTake(rows[3], 'u-ewa'), true, 'no list = any member');
  assert.equal(canTake(rows[0], ''), false);
  const takers = [{ user_id: 'u-anna', display_name: 'Anna' }, { user_id: 'u-ewa', display_name: 'Ewa' }, { user_id: 'u-marek', display_name: 'Marek' }];
  assert.deepEqual(takerOptions(rows[0], takers).map((p) => p.user_id), ['u-anna', 'u-marek']);
  assert.deepEqual(takerOptions(rows[3], takers).map((p) => p.user_id), ['u-anna', 'u-ewa', 'u-marek']);
});

test('"hand everything to" sets the taker of the selected rows that may take them and counts the others', () => {
  const rows = rowsOf(groups);
  const result = applyToSelected(rows, 'u-marek');
  assert.deepEqual(result, { set: 3, skipped: 0 }, 'two tasks and the position; the membership takes nobody and the blocked row is not ticked');
  assert.deepEqual([rows[0].taker, rows[1].taker, rows[3].taker], ['u-marek', 'u-marek', 'u-marek']);
  assert.equal(rows[0].manual, true);
  const again = applyToSelected(rows, 'u-anna');
  assert.deepEqual(again, { set: 2, skipped: 1 }, 'the second task is in a project Anna is not in');
  assert.equal(rows[1].taker, 'u-marek', 'a row that cannot take her keeps its taker');
});

test('nothing is sent without a selection, a note, a return day for an absence and a taker where one is required', () => {
  const rows = rowsOf(groups);
  const codes = (over) => problems({ rows, note: 'ok', reason: 'departure', returnDate: null, today: '2026-10-01', ...over }).map((p) => p.code + (p.key ? `:${p.key}` : ''));
  assert.deepEqual(codes({}), ['taker_required:task:p-2:t-2']);
  assert.deepEqual(codes({ note: '   ' }), ['note_required', 'taker_required:task:p-2:t-2']);
  rows[1].taker = 'u-marek';
  assert.deepEqual(codes({}), []);
  assert.deepEqual(codes({ reason: 'absence' }), ['return_required']);
  assert.deepEqual(codes({ reason: 'absence', returnDate: '2026-10-01' }), ['return_not_after_today']);
  assert.deepEqual(codes({ reason: 'absence', returnDate: '2026-10-08' }), []);
  rows[1].taker = 'u-anna';
  assert.deepEqual(codes({}), ['taker_not_eligible:task:p-2:t-2']);
  for (const row of rows) row.selected = false;
  assert.deepEqual(codes({}), ['nothing_selected']);
});

test('the request carries the selected rows, a taker only where the row takes one, and the date of its reason', () => {
  const rows = rowsOf(groups);
  rows[1].taker = 'u-marek';
  const payload = applyPayload({ userId: 'u-leaver', reason: 'departure', projectId: 'p-1', date: '2026-10-31', returnDate: '2026-11-07', note: '  Galaz feature/opc  ', rows });
  assert.deepEqual(payload, {
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
  const absence = applyPayload({ userId: 'u', reason: 'absence', projectId: null, date: '2026-10-31', returnDate: '2026-11-07', note: 'n', rows });
  assert.deepEqual([absence.date, absence.returnDate, absence.projectId], [null, '2026-11-07', null]);
  const project = applyPayload({ userId: 'u', reason: 'project_removal', projectId: 'p-1', date: null, returnDate: null, note: 'n', rows });
  assert.equal(project.projectId, 'p-1');
});

test('the summary counts what became of every item and names what a retry would take', () => {
  const answer = {
    handover_id: 'h-1',
    items: [
      { key: 'a', status: 'done' }, { key: 'b', status: 'scheduled' }, { key: 'c', status: 'failed' },
      { key: 'd', status: 'not_started' }, { key: 'e', status: 'skipped' },
    ],
  };
  assert.deepEqual(summarize(answer), { done: 1, scheduled: 1, failed: 2, skipped: 1, failedKeys: ['c', 'd'], retryable: true, total: 5 });
  assert.equal(summarize({ items: answer.items }).retryable, false, 'without a record there is nothing to retry from');
  assert.equal(summarize({ handover_id: 'h', items: [{ key: 'a', status: 'done' }] }).retryable, false);
  assert.equal(summarize(null).total, 0);
});

test('the records of absences say what is away, what came back and what stayed', () => {
  const record = (over) => ({
    id: 'h', reason: 'absence', return_date: '2026-10-10', items: [{ status: 'done' }, { status: 'done' }, { status: 'returned' }, { status: 'kept' }, { status: 'failed' }], ...over,
  });
  const [away] = absenceRecords([record({}), record({ reason: 'departure' })], '2026-10-01');
  assert.deepEqual([away.away, away.done, away.returned, away.kept, away.failed], [true, 2, 1, 1, 1]);
  assert.equal(absenceRecords([record({})], '2026-10-10')[0].away, false, 'on the return day the work is no longer away');
  assert.equal(absenceRecords(null, '2026-10-01').length, 0);
});

test('small helpers: takers by name and initials', () => {
  assert.deepEqual(sortedTakers([{ user_id: '2', display_name: 'Żaneta' }, { user_id: '1', display_name: 'anna' }]).map((p) => p.user_id), ['1', '2']);
  assert.equal(initials('Piotr Zieliński'), 'PZ');
  assert.equal(initials(''), '?');
});

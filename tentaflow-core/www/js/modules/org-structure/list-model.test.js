// =============================================================================
// File: modules/org-structure/list-model.test.js
// Description: The pure model of the Lista tab: reading order of the rows, one
//   row per holder and per vacancy, the manager column, the filter chips and
//   their counts, the search (blind to case and diacritics), the row menu per
//   permission, the places a person can be moved to and what ending an
//   assignment takes with it.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  CHIPS, assignmentSnapshot, chipCounts, endConsequences, filterRows, listRows, menuItems, moveTargets,
} from './list-model.js';

const T = { vacancy: 'wakat', unknown_person: 'nieznana osoba' };
const t = (key) => T[key] ?? key;

const anna = { kind: 'user', id: 'u-anna' };
const jan = { kind: 'user', id: 'u-jan' };
const ola = { kind: 'external', id: 'x-ola' };

const view = {
  at: '2026-09-30',
  units: [
    { unit_id: 'unit-board', name: 'Zarząd', head_position_id: 'pos-ceo', deputy_head_position_ids: [] },
    { unit_id: 'unit-it', name: 'Dział IT', head_position_id: 'pos-cto', deputy_head_position_ids: [] },
  ],
  positions: [
    { position_id: 'pos-dev', unit_id: 'unit-it', name: 'Programista', primary_parent_position_id: 'pos-cto', valid_from: '2026-09-15' },
    { position_id: 'pos-cto', unit_id: 'unit-it', name: 'Dyrektor IT', primary_parent_position_id: 'pos-ceo', valid_from: '2025-01-01' },
    { position_id: 'pos-ceo', unit_id: 'unit-board', name: 'Prezes', primary_parent_position_id: null, valid_from: '2024-01-01' },
    { position_id: 'pos-qa', unit_id: 'unit-it', name: 'Tester', primary_parent_position_id: 'pos-cto', valid_from: '2025-06-01' },
    { position_id: 'pos-ops', unit_id: 'unit-board', name: 'Asystent', primary_parent_position_id: 'pos-ceo', valid_from: '2026-01-01' },
  ],
  assignments: [
    { id: 'a-ceo', position_id: 'pos-ceo', subject: anna, display_name: 'Anna Nowak', share: 1, assignment_type: 'permanent', is_primary: true, valid_from: '2024-01-01' },
    { id: 'a-cto', position_id: 'pos-cto', subject: jan, display_name: 'Jan Żółć', share: 1, assignment_type: 'permanent', is_primary: true, valid_from: '2025-01-01' },
    { id: 'a-qa', position_id: 'pos-qa', subject: ola, display_name: 'Ola Kowal', share: 0.5, assignment_type: 'contractor', is_primary: true, valid_from: '2025-06-01' },
    { id: 'a-dev', position_id: 'pos-dev', subject: jan, display_name: 'Jan Żółć', share: 0.25, assignment_type: 'acting', is_primary: false, valid_from: '2026-09-20' },
  ],
  vacancies: ['pos-ops'],
};

const rows = listRows(view, t);
const byId = (id) => rows.find((r) => r._id === id);

test('rows come in reading order: a manager, then everyone under them', () => {
  assert.deepEqual(rows.map((r) => r.positionName), ['Prezes', 'Dyrektor IT', 'Programista', 'Tester', 'Asystent']);
});

test('a holder and a vacancy each get one row; a person on two positions gets two', () => {
  assert.equal(rows.length, 5);
  assert.equal(rows.filter((r) => r.personName === 'Jan Żółć').length, 2);
  const vacancy = byId('vacant:pos-ops');
  assert.equal(vacancy.vacant, true);
  assert.equal(vacancy.personName, 'wakat');
  assert.equal(vacancy.assignmentId, null);
  assert.equal(vacancy.since, '2026-01-01', 'a vacancy is dated by its position');
});

test('the manager column names the holders above, or the empty position', () => {
  assert.equal(byId('a-ceo').reportsTo, '');
  assert.equal(byId('a-cto').reportsTo, 'Anna Nowak');
  assert.equal(byId('a-dev').reportsTo, 'Jan Żółć');
  const emptied = listRows({ ...view, assignments: view.assignments.filter((a) => a.id !== 'a-cto') }, t);
  assert.equal(emptied.find((r) => r._id === 'a-dev').reportsTo, 'Dyrektor IT — wakat');
});

test('an assignment row carries what the edit and undo writes need', () => {
  const row = byId('a-qa');
  assert.deepEqual(
    [row.share, row.type, row.primary, row.external, row.subject, row.positionId],
    [0.5, 'contractor', true, true, ola, 'pos-qa'],
  );
  assert.deepEqual(assignmentSnapshot(row), {
    positionId: 'pos-qa', subject: ola, assignmentType: 'contractor', share: 0.5, isPrimary: true, validTo: null,
  });
});

test('a cycle without a root is still listed rather than dropped', () => {
  const looped = {
    ...view,
    positions: [
      { position_id: 'p1', unit_id: 'unit-it', name: 'Jeden', primary_parent_position_id: 'p2', valid_from: '2026-01-01' },
      { position_id: 'p2', unit_id: 'unit-it', name: 'Dwa', primary_parent_position_id: 'p1', valid_from: '2026-01-01' },
    ],
    assignments: [],
  };
  assert.deepEqual(listRows(looped, t).map((r) => r.positionName).sort(), ['Dwa', 'Jeden']);
});

test('the chips come in the mockup order and count what they would show', () => {
  assert.deepEqual(CHIPS, ['all', 'vacant', 'changes', 'no_account']);
  assert.deepEqual(chipCounts(rows, '2026-09-10'), { all: 5, vacant: 1, changes: 1, no_account: 1 });
  assert.equal(chipCounts(rows, null).changes, 0, 'no day, no changes');
});

test('the vacancy and no-account chips each keep only their rows', () => {
  assert.deepEqual(filterRows(rows, { chip: 'vacant' }).map((r) => r._id), ['vacant:pos-ops']);
  assert.deepEqual(filterRows(rows, { chip: 'no_account' }).map((r) => r._id), ['a-qa']);
  assert.equal(filterRows(rows, { chip: 'all' }).length, 5);
});

test('changes since a day include rows starting on that day and after it', () => {
  assert.deepEqual(filterRows(rows, { chip: 'changes', since: '2026-09-15' }).map((r) => r._id), ['a-dev']);
  assert.deepEqual(filterRows(rows, { chip: 'changes', since: '2026-09-20' }).map((r) => r._id), ['a-dev']);
  assert.deepEqual(filterRows(rows, { chip: 'changes', since: '2026-09-21' }).map((r) => r._id), []);
});

test('the search finds a person, a position, a unit or a manager without case or diacritics', () => {
  // Jan Żółć himself (two rows) and the position that reports to him.
  assert.deepEqual(filterRows(rows, { query: 'zolc' }).map((r) => r._id), ['a-cto', 'a-dev', 'a-qa']);
  assert.deepEqual(filterRows(rows, { query: 'PROGRAM' }).map((r) => r._id), ['a-dev']);
  assert.deepEqual(filterRows(rows, { query: 'zarzad' }).map((r) => r._id), ['a-ceo', 'vacant:pos-ops']);
  assert.deepEqual(filterRows(rows, { query: '  ' }).length, 5, 'blank search shows everything');
  assert.deepEqual(filterRows(rows, { query: 'nobody' }), []);
});

test('the search and a chip work together', () => {
  assert.deepEqual(filterRows(rows, { chip: 'vacant', query: 'asyst' }).map((r) => r._id), ['vacant:pos-ops']);
  assert.deepEqual(filterRows(rows, { chip: 'vacant', query: 'prezes' }).map((r) => r._id), []);
});

test('the row menu exists for org.admin only, and a vacancy gets its own items', () => {
  assert.deepEqual(menuItems(byId('a-cto'), { isAdmin: false }), []);
  assert.deepEqual(menuItems(byId('vacant:pos-ops'), { isAdmin: false }), []);
  assert.deepEqual(menuItems(byId('a-cto'), { isAdmin: true }).map((i) => i.id ?? 'separator'), ['edit', 'move', 'deputy', 'separator', 'handover', 'end']);
  assert.deepEqual(menuItems(byId('vacant:pos-ops'), { isAdmin: true }).map((i) => i.id ?? 'separator'), ['assign', 'separator', 'end_position']);
  assert.equal(menuItems(byId('a-cto'), { isAdmin: true }).find((i) => i.id === 'end').danger, true);
});

test('a person can be moved to any vacant position, the own unit’s too', () => {
  assert.deepEqual(moveTargets(rows), [{ id: 'pos-ops', unitId: 'unit-board', label: 'Zarząd — Asystent' }]);
  assert.deepEqual(moveTargets(rows.filter((r) => !r.vacant)), [], 'nothing vacant, nowhere to move');
});

test('ending an assignment says what happens to the position, the unit, the reports and the person', () => {
  const keys = (row) => endConsequences(row, view, rows).map((c) => c.key);
  assert.deepEqual(keys(byId('a-cto')), ['becomes_vacant', 'unit_without_head', 'keeps_reports', 'person_leaves', 'primary_moves']);
  const cto = endConsequences(byId('a-cto'), view, rows);
  assert.deepEqual(cto.find((c) => c.key === 'keeps_reports').params, { count: 2 });
  assert.deepEqual(cto.find((c) => c.key === 'becomes_vacant').params, { position: 'Dyrektor IT', unit: 'Dział IT' });
  assert.deepEqual(keys(byId('a-dev')), ['becomes_vacant', 'person_leaves'], 'a leaf, secondary seat');
});

test('a position with a second holder does not become vacant when one of them leaves', () => {
  const two = [...view.assignments, { id: 'a-cto2', position_id: 'pos-cto', subject: anna, display_name: 'Anna Nowak', share: 0.5, assignment_type: 'permanent', is_primary: false, valid_from: '2026-01-01' }];
  const both = listRows({ ...view, assignments: two }, t);
  const keys = endConsequences(both.find((r) => r._id === 'a-cto'), { ...view, assignments: two }, both).map((c) => c.key);
  assert.equal(keys.includes('becomes_vacant'), false);
  assert.equal(keys.includes('unit_without_head'), false);
});

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
    { unitId: 'unit-board', name: 'Zarząd', headPositionId: 'pos-ceo', deputyHeadPositionIds: [] },
    { unitId: 'unit-it', name: 'Dział IT', headPositionId: 'pos-cto', deputyHeadPositionIds: [] },
  ],
  positions: [
    { positionId: 'pos-dev', unitId: 'unit-it', name: 'Programista', primaryParentPositionId: 'pos-cto', validFrom: '2026-09-15' },
    { positionId: 'pos-cto', unitId: 'unit-it', name: 'Dyrektor IT', primaryParentPositionId: 'pos-ceo', validFrom: '2025-01-01' },
    { positionId: 'pos-ceo', unitId: 'unit-board', name: 'Prezes', primaryParentPositionId: null, validFrom: '2024-01-01' },
    { positionId: 'pos-qa', unitId: 'unit-it', name: 'Tester', primaryParentPositionId: 'pos-cto', validFrom: '2025-06-01' },
    { positionId: 'pos-ops', unitId: 'unit-board', name: 'Asystent', primaryParentPositionId: 'pos-ceo', validFrom: '2026-01-01' },
  ],
  assignments: [
    { id: 'a-ceo', positionId: 'pos-ceo', subject: anna, displayName: 'Anna Nowak', share: 1, assignmentType: 'permanent', isPrimary: true, validFrom: '2024-01-01' },
    { id: 'a-cto', positionId: 'pos-cto', subject: jan, displayName: 'Jan Żółć', share: 1, assignmentType: 'permanent', isPrimary: true, validFrom: '2025-01-01' },
    { id: 'a-qa', positionId: 'pos-qa', subject: ola, displayName: 'Ola Kowal', share: 0.5, assignmentType: 'contractor', isPrimary: true, validFrom: '2025-06-01' },
    { id: 'a-dev', positionId: 'pos-dev', subject: jan, displayName: 'Jan Żółć', share: 0.25, assignmentType: 'acting', isPrimary: false, validFrom: '2026-09-20' },
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
      { positionId: 'p1', unitId: 'unit-it', name: 'Jeden', primaryParentPositionId: 'p2', validFrom: '2026-01-01' },
      { positionId: 'p2', unitId: 'unit-it', name: 'Dwa', primaryParentPositionId: 'p1', validFrom: '2026-01-01' },
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
  const two = [...view.assignments, { id: 'a-cto2', positionId: 'pos-cto', subject: anna, displayName: 'Anna Nowak', share: 0.5, assignmentType: 'permanent', isPrimary: false, validFrom: '2026-01-01' }];
  const both = listRows({ ...view, assignments: two }, t);
  const keys = endConsequences(both.find((r) => r._id === 'a-cto'), { ...view, assignments: two }, both).map((c) => c.key);
  assert.equal(keys.includes('becomes_vacant'), false);
  assert.equal(keys.includes('unit_without_head'), false);
});

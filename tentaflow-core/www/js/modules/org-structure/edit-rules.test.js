// =============================================================================
// File: modules/org-structure/edit-rules.test.js
// Description: The client-side rules of the edit mode: how many people a move
//   touches, which drops are refused and why (self, cycle, staff manager,
//   already there), what may land on what, the deputy heads' order and who
//   is offered in the strip of people without a position.
// =============================================================================

import '../../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const {
  branchIds, cardDropRule, moveTargets, peopleWithoutPosition, positionMoveCheck, reorder, reparentImpact,
  unitMoveCheck, unitMoveTargets, unitSeatCandidates, vacancyDropCheck,
} = await import('./edit-rules.js');
const { buildTreeModel } = await import('./tree.js');
const { sampleView, t } = await import('./tree-fixture.js');

const model = () => buildTreeModel(sampleView(), { t });

test('a move concerns every person on the position and below it, once, and counts the direct reports', () => {
  // Kierownik zespołu: Anna, the acting deputy, two developers (one of them Marek), a tester; the automation tester seat is vacant.
  assert.deepEqual(reparentImpact(model(), 'p-lead'), { people: 5, direct: 5 });
  // A leaf: just the one person.
  assert.deepEqual(reparentImpact(model(), 'p-tester'), { people: 1, direct: 0 });
  // A vacant leaf touches nobody.
  assert.deepEqual(reparentImpact(model(), 'p-tester-auto'), { people: 0, direct: 0 });
});

test('a person holding two seats inside the branch is counted once', () => {
  const view = sampleView();
  // Marek Nowak now also sits under the lead: still one person.
  view.assignments.push({
    id: 'a-extra', position_id: 'p-tester-auto', subject: { kind: 'external', id: 'm-nowak' }, assignment_type: 'permanent',
    share: 0.2, is_primary: false, valid_from: '2026-01-01', display_name: 'Marek Nowak',
  });
  assert.equal(reparentImpact(buildTreeModel(view, { t }), 'p-lead').people, 5);
});

test('the branch of a position is the position and all below it', () => {
  const byId = new Map(model().nodes.map((n) => [n.id, n]));
  const branch = branchIds(byId, 'p-lead');
  assert.deepEqual([...branch].sort(), ['p-dev1', 'p-dev2', 'p-lead', 'p-lead-deputy', 'p-tester', 'p-tester-auto']);
});

test('a card cannot go under itself or under anything of its own branch', () => {
  const m = model();
  assert.deepEqual(positionMoveCheck(m, 'p-lead', 'p-lead'), { ok: false, reason: 'self' });
  assert.deepEqual(positionMoveCheck(m, 'p-lead', 'p-dev1'), { ok: false, reason: 'cycle' });
  assert.deepEqual(positionMoveCheck(m, 'p-cto', 'p-tester'), { ok: false, reason: 'cycle' }, 'far down the branch is a cycle too');
});

test('a card can go under another one, but not a staff position and not where it already is', () => {
  const m = model();
  assert.deepEqual(positionMoveCheck(m, 'p-tester', 'p-sales'), { ok: true });
  assert.deepEqual(positionMoveCheck(m, 'p-tester', 'p-assistant'), { ok: false, reason: 'staff' });
  assert.deepEqual(positionMoveCheck(m, 'p-tester', 'p-lead'), { ok: false, reason: 'same' });
  assert.deepEqual(positionMoveCheck(m, 'p-tester', 'p-nowhere'), { ok: false, reason: 'unknown' });
  assert.deepEqual(positionMoveCheck(m, 'p-tester', null), { ok: true }, 'no manager is allowed');
  assert.deepEqual(positionMoveCheck(m, 'p-ceo', null), { ok: false, reason: 'same' }, 'the root already has none');
});

test('a unit follows the same rules among units', () => {
  const m = model();
  assert.deepEqual(unitMoveCheck(m, 'u-tech', 'u-delivery'), { ok: false, reason: 'cycle' });
  assert.deepEqual(unitMoveCheck(m, 'u-tech', 'u-tech'), { ok: false, reason: 'self' });
  assert.deepEqual(unitMoveCheck(m, 'u-delivery', 'u-sales'), { ok: true });
  assert.deepEqual(unitMoveCheck(m, 'u-delivery', 'u-tech'), { ok: false, reason: 'same' });
  assert.deepEqual(unitMoveCheck(m, 'u-board', null), { ok: false, reason: 'same' });
});

test('the chart is told the rule for the kind being dragged and refuses to mix kinds', () => {
  const rule = cardDropRule(model());
  assert.equal(rule({ id: 'p-lead', kind: 'position' }, { id: 'p-dev1', kind: 'position' }).reason, 'cycle');
  assert.equal(rule({ id: 'u-tech', kind: 'unit' }, { id: 'u-delivery', kind: 'unit' }).reason, 'cycle');
  assert.equal(rule({ id: 'p-lead', kind: 'position' }, { id: 'u-sales', kind: 'unit' }).ok, false);
  assert.equal(rule({ id: 'p-tester', kind: 'position' }, { id: 'p-sales', kind: 'position' }).ok, true);
});

test('a person can only be dropped on a vacancy', () => {
  const m = model();
  assert.deepEqual(vacancyDropCheck(m, 'p-tester-auto'), { ok: true });
  assert.deepEqual(vacancyDropCheck(m, 'p-tester'), { ok: false, reason: 'occupied' });
  assert.deepEqual(vacancyDropCheck(m, 'p-nowhere'), { ok: false, reason: 'unknown' });
});

test('the targets of a move say why the ones that cannot be chosen cannot', () => {
  const reasons = { unknown: 'u', self: 's', cycle: 'c', staff: 'f', same: 'same' };
  const targets = moveTargets(model(), 'p-lead', { describe: (n) => n.role, reasons });
  const byId = new Map(targets.map((x) => [x.id, x]));
  assert.equal(byId.has('p-lead'), false, 'not itself');
  assert.equal(byId.get('p-dev1').disabled, 'c');
  assert.equal(byId.get('p-assistant').disabled, 'f');
  assert.equal(byId.get('p-delivery').disabled, 'same', 'the current manager');
  assert.equal(byId.get('p-sales').disabled, undefined);
  const units = unitMoveTargets(model(), 'u-tech', { reasons });
  assert.equal(units.find((x) => x.id === 'u-delivery').disabled, 'c');
  assert.equal(units.find((x) => x.id === 'u-sales').disabled, undefined);
});

test('deputy heads are reordered in a copy, and a move that goes nowhere changes nothing', () => {
  const list = ['a', 'b', 'c'];
  assert.deepEqual(reorder(list, 0, 2), ['b', 'c', 'a']);
  assert.deepEqual(reorder(list, 2, 1), ['a', 'c', 'b']);
  assert.deepEqual(reorder(list, 1, 1), list);
  assert.deepEqual(reorder(list, 0, -1), list, 'off the top');
  assert.deepEqual(reorder(list, 0, 3), list, 'off the bottom');
  assert.deepEqual(list, ['a', 'b', 'c'], 'the input is never touched');
});

test('the seats a unit can still give to a head or a deputy exclude the two roles already filled', () => {
  const view = sampleView();
  assert.deepEqual(unitSeatCandidates(view, 'u-tech'), [], 'the head and the one deputy fill Technologia');
  const delivery = unitSeatCandidates(view, 'u-delivery').map((p) => p.position_id);
  assert.equal(delivery.includes('p-delivery'), false, 'the head is taken');
  assert.equal(delivery.includes('p-lead'), true);
});

test('people without a position are the active accounts nobody holds a seat for', () => {
  const view = sampleView();
  view.assignments.push({ id: 'a-u', position_id: 'p-tester-auto', subject: { kind: 'user', id: 'u-held' }, assignment_type: 'permanent', share: 1, is_primary: true, valid_from: '2026-01-01', display_name: 'Held' });
  const users = [
    { id: 'u-held', name: 'Held', isActive: true },
    { id: 'u-free', name: 'Free', isActive: true },
    { id: 'u-off', name: 'Off', isActive: false },
  ];
  assert.deepEqual(peopleWithoutPosition(users, view).map((u) => u.id), ['u-free']);
});

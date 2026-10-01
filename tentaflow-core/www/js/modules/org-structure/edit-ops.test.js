// =============================================================================
// File: modules/org-structure/edit-ops.test.js
// Description: The operations of the edit mode's draft: what the builders write
//   into an operation (the names `orgBatchRequest` takes, clearing by the wire's
//   own field names), and how a draft list folds them — a second edit of the same
//   thing merges, a move replaces the earlier move, an edit of something the draft
//   creates goes into its creating operation, ending such a thing removes it with
//   everything that depended on it.
// =============================================================================

import '../../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const ops = await import('./edit-ops.js');
const { sampleView } = await import('./tree-fixture.js');

const DAY = '2026-09-30';
const { addOp, withDependents, withoutOp } = ops;

test('a move, a head and the deputies are operations of the names the batch takes', () => {
  assert.deepEqual(ops.movePositionOp('p-tester', 'p-sales', DAY), { kind: 'positionMove', positionId: 'p-tester', newParentPositionId: 'p-sales', from: DAY });
  assert.deepEqual(ops.moveUnitOp('u-fin', null, DAY), { kind: 'unitMove', unitId: 'u-fin', newParentUnitId: null, from: DAY });
  assert.deepEqual(ops.setHeadOp('u-tech', 'p-cto', DAY), { kind: 'headSet', unitId: 'u-tech', headPositionId: 'p-cto', from: DAY });
  assert.deepEqual(ops.setDeputiesOp('u-tech', ['a', 'b'], DAY), { kind: 'deputyHeadsSet', unitId: 'u-tech', positionIds: ['a', 'b'], from: DAY });
});

test('a field edit names only what differs from the structure, and clears by the wire\'s field name', () => {
  const view = sampleView();
  view.positions.find((p) => p.position_id === 'p-tester').role_id = 'role-1';
  assert.equal(ops.updatePositionOp(view, 'p-tester', { name: 'Tester' }, DAY), null, 'nothing differs');
  assert.deepEqual(ops.updatePositionOp(view, 'p-tester', { name: 'QA', roleId: null }, DAY), {
    kind: 'positionUpdate', positionId: 'p-tester', from: DAY, name: 'QA', clear: ['role_id'],
  });
  view.units.find((u) => u.unit_id === 'u-sales').type_id = 'ty-1';
  assert.deepEqual(ops.updateUnitOp(view, 'u-sales', { typeId: null, color: '#ff0000' }, DAY), {
    kind: 'unitUpdate', unitId: 'u-sales', from: DAY, color: '#ff0000', clear: ['type_id'],
  });
});

test('an assignment edit is looked up by position and person and names only what differs', () => {
  const view = sampleView();
  const anna = view.assignments.find((a) => a.position_id === 'p-lead');
  assert.equal(ops.updateAssignmentOp(view, ops.assignmentRef(anna), { share: 1 }, DAY), null);
  assert.deepEqual(ops.updateAssignmentOp(view, ops.assignmentRef(anna), { share: 0.5, isPrimary: true }, DAY), {
    kind: 'assignmentUpdate', assignmentId: anna.id, from: DAY, share: 0.5,
  });
});

test('assigning a person without an account starts with the operation that creates them; replacing ends the old assignment', () => {
  const made = ops.assignOps('p-tester-auto', { kind: 'new_external', displayName: 'Ktoś', email: null }, { share: 0.5 }, DAY);
  assert.deepEqual(made.map((o) => o.kind), ['externalPersonCreate', 'assign']);
  assert.equal(made[0].createsSubject, true);
  assert.equal(made[1].subject.id, null, 'filled in by the draft from the temporary id it gives the person');
  const replaced = ops.assignOps('p-tester', { kind: 'user', id: 'u1' }, {}, DAY, { endPrevious: { id: 'a-old' } });
  assert.deepEqual(replaced.map((o) => o.kind), ['assign', 'assignmentEnd']);
  assert.equal(replaced[1].assignmentId, 'a-old');
});

test('a second edit of one field set merges, and the newest word on a field wins, set or cleared', () => {
  let list = addOp([], { kind: 'positionUpdate', positionId: 'p1', from: DAY, name: 'A', clear: [] });
  list = addOp(list, { kind: 'positionUpdate', positionId: 'p1', from: DAY, code: 'X', clear: [] });
  assert.equal(list.length, 1);
  assert.deepEqual(list[0], { kind: 'positionUpdate', positionId: 'p1', from: DAY, name: 'A', code: 'X', clear: [] });
  list = addOp(list, { kind: 'positionUpdate', positionId: 'p1', from: DAY, clear: ['code'] });
  assert.deepEqual(list[0], { kind: 'positionUpdate', positionId: 'p1', from: DAY, name: 'A', clear: ['code'] });
  list = addOp(list, { kind: 'positionUpdate', positionId: 'p1', from: DAY, code: 'Y', clear: [] });
  assert.deepEqual(list[0], { kind: 'positionUpdate', positionId: 'p1', from: DAY, name: 'A', code: 'Y', clear: [] });
  list = addOp(list, { kind: 'positionUpdate', positionId: 'p1', from: '2026-10-01', name: 'B', clear: [] });
  assert.equal(list.length, 2, 'another day is another change');
});

test('a move, a head and a list of deputies replace the earlier one for the same thing', () => {
  let list = addOp([], ops.movePositionOp('p1', 'p2', DAY));
  list = addOp(list, ops.movePositionOp('p1', 'p3', DAY));
  list = addOp(list, ops.movePositionOp('p9', 'p3', DAY));
  assert.deepEqual(list.map((o) => [o.positionId, o.newParentPositionId]), [['p1', 'p3'], ['p9', 'p3']]);
  list = addOp(list, ops.setHeadOp('u1', 'a', DAY));
  list = addOp(list, ops.setHeadOp('u1', 'b', DAY));
  list = addOp(list, ops.setDeputiesOp('u1', ['x'], DAY));
  list = addOp(list, ops.setDeputiesOp('u1', ['x', 'y'], DAY));
  assert.equal(list.filter((o) => o.kind === 'headSet').length, 1);
  assert.deepEqual(list.find((o) => o.kind === 'deputyHeadsSet').positionIds, ['x', 'y']);
});

test('an edit of something the draft creates goes into its creating operation', () => {
  let list = [{ ...ops.createUnitOp({ name: 'Nowy' }, DAY), tempId: 'tmp:u1' }];
  list = addOp(list, { kind: 'unitUpdate', unitId: 'tmp:u1', from: DAY, name: 'Nowszy', code: 'NW', clear: [] });
  list = addOp(list, ops.moveUnitOp('tmp:u1', 'u-sales', DAY));
  assert.equal(list.length, 1);
  assert.equal(list[0].name, 'Nowszy');
  assert.equal(list[0].code, 'NW');
  assert.equal(list[0].parentUnitId, 'u-sales');

  list = [{ ...ops.createPositionOp({ unitId: 'u1', name: 'P' }, DAY), tempId: 'tmp:p1' }];
  list = addOp(list, ops.movePositionOp('tmp:p1', 'p-x', DAY));
  list = addOp(list, { kind: 'positionUpdate', positionId: 'tmp:p1', from: DAY, isStaff: true, roleId: null, clear: ['role_id'] });
  assert.equal(list.length, 1);
  assert.equal(list[0].parentPositionId, 'p-x');
  assert.equal(list[0].isStaff, true);
  assert.equal(list[0].roleId, null);

  list = [{ kind: 'assign', tempId: 'tmp:a1', positionId: 'p1', subject: { kind: 'user', id: 'u' }, share: 1, assignmentType: 'permanent', validFrom: DAY }];
  list = addOp(list, { kind: 'assignmentUpdate', assignmentId: 'tmp:a1', from: DAY, share: 0.5 });
  assert.equal(list[0].share, 0.5);
});

test('ending something the draft creates removes it, and everything that used it', () => {
  let list = [
    { ...ops.createUnitOp({ name: 'U' }, DAY), tempId: 'tmp:u1' },
    { ...ops.createPositionOp({ unitId: 'tmp:u1', name: 'P' }, DAY), tempId: 'tmp:p1' },
    { kind: 'headSet', unitId: 'tmp:u1', headPositionId: 'tmp:p1', from: DAY },
    ops.movePositionOp('p-other', 'p-x', DAY),
  ];
  assert.deepEqual(withDependents(list, 0), [0, 1, 2]);
  list = addOp(list, ops.endUnitOp('tmp:u1', DAY));
  assert.deepEqual(list.map((o) => o.kind), ['positionMove']);
});

test('removing a row removes the operations that depended on it and no others', () => {
  const list = [
    { ...ops.createPositionOp({ unitId: 'u', name: 'P' }, DAY), tempId: 'tmp:p1' },
    { kind: 'assign', tempId: 'tmp:a1', positionId: 'tmp:p1', subject: { kind: 'user', id: 'x' }, validFrom: DAY },
    { kind: 'assignmentEnd', assignmentId: 'tmp:a1', from: DAY },
    ops.movePositionOp('p-other', 'p-x', DAY),
  ];
  assert.deepEqual(withDependents(list, 0), [0, 1, 2]);
  assert.deepEqual(withoutOp(list, 1).map((o) => o.kind), ['positionCreate', 'positionMove']);
  assert.deepEqual(withoutOp(list, 3).length, 3);
});

test('an operation is redated as a whole, and one without a day is left alone', () => {
  assert.equal(ops.withDay(ops.movePositionOp('a', 'b', DAY), '2026-11-01').from, '2026-11-01');
  assert.equal(ops.withDay(ops.createUnitOp({ name: 'U' }, DAY), '2026-11-01').validFrom, '2026-11-01');
  const person = { kind: 'externalPersonCreate', displayName: 'X' };
  assert.deepEqual(ops.withDay(person, '2026-11-01'), person);
});

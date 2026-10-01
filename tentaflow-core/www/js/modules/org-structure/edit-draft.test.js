// =============================================================================
// File: modules/org-structure/edit-draft.test.js
// Description: The edit mode's draft against a stubbed transport: an action is
//   added only after a dry run accepts it, creating operations get temporary ids
//   and the ids a dry run hands back are mapped to them, a refusal is a typed
//   error naming the rule, a backdated change is confirmed once, undo/redo and
//   "Cofnij" work on the draft (creating operations included, dependents going
//   with their row), and saving is ONE dry run and ONE apply of the same list.
// =============================================================================

import '../../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { BACKDATED, OrgWriteError, createDraft } = await import('./edit-draft.js');
const ops = await import('./edit-ops.js');
const { sampleView } = await import('./tree-fixture.js');

const DAY = '2026-09-30';

/** A server that answers a batch per operation: `refuse(op, index, payload)` returns an error or nothing. */
function harness({ refuse = () => null, confirm = async () => true, warn = () => [] } = {}) {
  const calls = [];
  const live = sampleView();
  const send = async (kind, payload) => {
    calls.push({ kind, payload: { ...payload, ops: payload.ops.map((o) => ({ ...o })) } });
    const results = payload.ops.map((op, index) => {
      const error = refuse(op, index, payload);
      return { index, ok: !error, error, temp_id: op.tempId ?? null, created_id: op.tempId ? `real-${calls.length}-${op.tempId}` : null };
    });
    const ok = results.every((r) => r.ok);
    return { ok, applied: ok && !payload.dryRun, error: null, results, warnings: warn(results), preview_at: DAY, preview: payload.dryRun ? sampleView() : null };
  };
  const draft = createDraft({ send, getLive: () => live, askBackdated: confirm, day: DAY });
  return { draft, calls, live };
}

test('an empty draft asks nothing of the server and shows no preview', async () => {
  const { draft, calls } = harness();
  await draft.flush();
  assert.equal(calls.length, 0);
  assert.equal(draft.preview, null);
  assert.equal(draft.dirty, false);
});

test('a warning of the dry run names what the draft created by its temporary id, never by the dry run\'s own', async () => {
  const { draft } = harness({ warn: (results) => results.filter((r) => r.created_id).map((r) => ({ kind: 'unit_without_head', unit_id: r.created_id })) });
  await draft.addChecked([ops.createUnitOp({ name: 'Nowy dział' }, DAY)]);
  assert.deepEqual(draft.preview.warnings, [{ kind: 'unit_without_head', unit_id: draft.ops[0].tempId }]);
});

test('an action is checked with a dry run before it is kept, and the answer is the preview', async () => {
  const { draft, calls } = harness();
  await draft.addChecked([ops.movePositionOp('p-tester', 'p-sales', DAY)]);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].kind, 'orgBatchRequest');
  assert.equal(calls[0].payload.dryRun, true);
  assert.deepEqual(calls[0].payload.ops, [{ kind: 'positionMove', positionId: 'p-tester', newParentPositionId: 'p-sales', from: DAY }]);
  assert.equal(draft.dirty, true);
  assert.equal(draft.preview.view.at, DAY, 'the structure the batch leaves is what the canvas draws');
});

test('creating operations get temporary ids, and the preview shows what they make under those ids, run after run', async () => {
  const { draft, calls } = harness();
  await draft.addChecked([ops.createUnitOp({ name: 'Nowy dział' }, DAY)]);
  const unit = draft.ops[0];
  assert.match(unit.tempId, /^tmp:u\d+$/);
  assert.notEqual(draft.results[0].createdId, unit.tempId, 'the dry run made the unit under an id that will not exist after saving');
  assert.ok(!JSON.stringify(draft.preview.view).includes(draft.results[0].createdId), 'and the preview no longer carries it');
  const firstRun = draft.preview.view;

  // The next dry run makes the unit under another id; the card keeps its own.
  await draft.addChecked([ops.createPositionOp({ unitId: unit.tempId, name: 'Szef' }, DAY)]);
  assert.equal(draft.ops[1].unitId, unit.tempId);
  assert.deepEqual(calls.at(-1).payload.ops.map((o) => o.kind), ['unitCreate', 'positionCreate']);
  assert.notEqual(firstRun, draft.preview.view);
});

test('assigning a person without an account chains the two operations through a temporary id', async () => {
  const { draft } = harness();
  await draft.addChecked(ops.assignOps('p-tester-auto', { kind: 'new_external', displayName: 'Ktoś' }, { share: 1 }, DAY));
  const [person, assign] = draft.ops;
  assert.equal(person.kind, 'externalPersonCreate');
  assert.match(person.tempId, /^tmp:e\d+$/);
  assert.equal(assign.subject.id, person.tempId);
  assert.equal('createsSubject' in person, false, 'the marker never reaches the wire');
});

test('an assignment of the preview is written as the live one, unless the draft made it', async () => {
  const { draft, live } = harness();
  const anna = live.assignments.find((a) => a.position_id === 'p-lead');
  await draft.addChecked([ops.endAssignmentOp({ id: anna.id }, DAY)]);
  assert.equal(draft.ops[0].assignmentId, anna.id);
});

test('a refused action is not kept, the rule that refused it is thrown, and the preview goes back', async () => {
  const { draft, calls } = harness({ refuse: (op) => (op.kind === 'positionMove' ? { code: 'reporting_cycle', message: 'cycle', date: DAY } : null) });
  await assert.rejects(draft.addChecked([ops.movePositionOp('p-lead', 'p-dev1', DAY)]), (err) => {
    assert.ok(err instanceof OrgWriteError);
    assert.equal(err.code, 'reporting_cycle');
    return true;
  });
  assert.equal(draft.dirty, false);
  assert.equal(draft.canUndo, false, 'a refused action leaves no step to undo');
  assert.equal(calls.length, 1, 'the empty draft after the refusal needs no further dry run');
});

test('an operation that was already refused is not blamed on the next action', async () => {
  const { draft } = harness({ refuse: (op) => (op.kind === 'headSet' ? { code: 'head_is_deputy', message: 'x' } : null) });
  // A draft whose row later starts failing (its dependency went away): loaded as it is.
  await draft.load([ops.setHeadOp('u-tech', 'p-cto-deputy', DAY)], DAY);
  assert.equal(draft.errors().length, 1);
  await draft.addChecked([ops.movePositionOp('p-tester', 'p-sales', DAY)]);
  assert.equal(draft.ops.length, 2, 'the new action is kept although the draft still has a refused row');
  assert.equal(draft.errors().length, 1);
  assert.equal(draft.errors()[0].error.code, 'head_is_deputy');
  assert.equal(draft.errors()[0].op.kind, 'headSet');
});

test('a backdated change is put to the administrator once, then sent with confirmBackdated', async () => {
  const asked = [];
  const { draft, calls } = harness({
    confirm: async (day) => { asked.push(day); return true; },
    refuse: (op, i, payload) => (payload.confirmBackdated ? null : { code: BACKDATED, message: 'before today', date: DAY }),
  });
  await draft.addChecked([ops.movePositionOp('p-tester', 'p-sales', DAY)]);
  assert.deepEqual(asked, [DAY]);
  assert.deepEqual(calls.map((c) => Boolean(c.payload.confirmBackdated)), [false, true]);
  assert.equal(draft.confirmed, true);
  assert.equal(draft.dirty, true);
  await draft.addChecked([ops.movePositionOp('p-dev1', 'p-sales', DAY)]);
  assert.equal(asked.length, 1, 'not asked again for the same day');
});

test('declining the backdating leaves the action refused', async () => {
  const { draft } = harness({ confirm: async () => false, refuse: () => ({ code: BACKDATED, message: 'x', date: DAY }) });
  await assert.rejects(draft.addChecked([ops.movePositionOp('p-tester', 'p-sales', DAY)]), { code: BACKDATED });
  assert.equal(draft.dirty, false);
});

test('undo and redo restore the draft, creating operations included', async () => {
  const { draft } = harness();
  await draft.addChecked([ops.createUnitOp({ name: 'Nowy' }, DAY)]);
  await draft.addChecked([ops.movePositionOp('p-tester', 'p-sales', DAY)]);
  draft.undo();
  assert.deepEqual(draft.ops.map((o) => o.kind), ['unitCreate']);
  draft.undo();
  assert.equal(draft.ops.length, 0, 'even what the draft created can be taken back: nothing is written yet');
  draft.redo();
  draft.redo();
  assert.deepEqual(draft.ops.map((o) => o.kind), ['unitCreate', 'positionMove']);
  assert.equal(draft.canRedo, false);
  draft.undo();
  await draft.addChecked([ops.movePositionOp('p-dev1', 'p-sales', DAY)]);
  assert.equal(draft.canRedo, false, 'a new change drops what could have been redone');
});

test('"Cofnij" on a row removes it with what depended on it, and the preview follows', async () => {
  const { draft, calls } = harness();
  await draft.addChecked([ops.createUnitOp({ name: 'U' }, DAY)]);
  await draft.addChecked([ops.createPositionOp({ unitId: draft.ops[0].tempId, name: 'P' }, DAY)]);
  await draft.addChecked([ops.movePositionOp('p-tester', 'p-sales', DAY)]);
  assert.equal(draft.remove(0), 2, 'the unit and the position in it');
  assert.deepEqual(draft.ops.map((o) => o.kind), ['positionMove']);
  await draft.flush();
  assert.deepEqual(calls.at(-1).payload.ops.map((o) => o.kind), ['positionMove']);
  draft.undo();
  assert.equal(draft.ops.length, 3, 'and the removal itself can be undone');
});

test('changing the effective day moves every operation to it and asks about a past day again', async () => {
  const { draft } = harness();
  await draft.addChecked([ops.movePositionOp('p-tester', 'p-sales', DAY), ops.createUnitOp({ name: 'U' }, DAY)]);
  draft.confirm();
  draft.setDay('2026-11-01');
  assert.deepEqual(draft.ops.map((o) => o.from ?? o.validFrom), ['2026-11-01', '2026-11-01']);
  assert.equal(draft.at, '2026-11-01');
  assert.equal(draft.confirmed, false);
  await draft.flush();
});

test('saving is one dry run and one apply of the same list, and the draft is then empty', async () => {
  const { draft, calls } = harness();
  await draft.addChecked([ops.movePositionOp('p-tester', 'p-sales', DAY)]);
  await draft.addChecked([ops.createUnitOp({ name: 'U' }, DAY)]);
  const before = calls.length;
  const out = await draft.save();
  assert.equal(out.applied, true);
  assert.deepEqual(calls.slice(before).map((c) => c.payload.dryRun), [true, false], 'a dry run first, then the apply');
  assert.deepEqual(calls.at(-1).payload.ops, calls.at(-2).payload.ops);
  draft.clear();
  assert.equal(draft.dirty, false);
  assert.equal(draft.canUndo, false);
});

test('a refusal at save time applies nothing and names the operations', async () => {
  let refuseNow = false;
  const { draft, calls } = harness({ refuse: (op) => (refuseNow && op.kind === 'positionMove' ? { code: 'not_valid_at', message: 'x' } : null) });
  await draft.addChecked([ops.movePositionOp('p-tester', 'p-sales', DAY)]);
  refuseNow = true;
  const before = calls.length;
  const out = await draft.save();
  assert.equal(out.applied, false);
  assert.equal(out.results[0].error.code, 'not_valid_at');
  assert.equal(calls.length - before, 1, 'no apply after a failed dry run');
  assert.equal(draft.dirty, true, 'the draft is kept for the administrator to fix');
});

test('a reorganization opened for editing starts as its operations, dated its day', async () => {
  const { draft, calls } = harness();
  await draft.load([ops.movePositionOp('p-tester', 'p-sales', '2026-11-01')], '2026-11-01');
  assert.equal(draft.at, '2026-11-01');
  assert.equal(draft.dirty, true);
  assert.equal(draft.canUndo, false);
  assert.equal(calls.length, 1);
});

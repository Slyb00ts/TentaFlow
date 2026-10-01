// =============================================================================
// File: modules/org-structure/history-model.test.js
// Description: The pure model of the Historia tab: the days of the date jumper,
//   how an audit entry and a difference read, what the chart marks for a diff,
//   who may approve a reorganization and how the operations a change set stores
//   come back in the shape the edit mode builds them in.
// =============================================================================

import '../../lib/actions/_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  changeSetFlags, chartMark, decorateModel, diffCounts, diffLine, diffMarks, entryModel, errorText,
  failedOps, groupChangeSets, markers, opSummary, opsFromWire, quickJumps, railPercent, railTicks, relativeDay, sliderDayOf,
  sliderRange, sliderValueOf, subtreeUnitIds, unitChoices, valueText, viewSubset, withoutOp,
} from './history-model.js';

// A translator that shows the key and the parameters: the tests state which sentence is chosen, not its wording.
const t = (key, params = {}) => `${key}${Object.keys(params).length ? JSON.stringify(params) : ''}`;

test('the distance of a day from today is read as today, future or past', () => {
  assert.deepEqual(relativeDay('2026-11-01', '2026-09-30'), { kind: 'future', days: 32 });
  assert.deepEqual(relativeDay('2026-09-01', '2026-09-30'), { kind: 'past', days: 29 });
  assert.deepEqual(relativeDay('2026-09-30', '2026-09-30'), { kind: 'today', days: 0 });
});

const entries = [
  { effective_date: '2026-11-01' },
  { effective_date: '2026-09-15' },
  { effective_date: '2026-09-15' },
  { effective_date: '2026-01-01' },
];

test('the marks are the distinct days with something changed, planned ones flagged, plans of open change sets included', () => {
  const marks = markers(entries, [
    { state: 'pending', effective_date: '2026-12-01' },
    { state: 'withdrawn', effective_date: '2027-06-01' },
    { state: 'applied', effective_date: '2027-07-01' },
  ], '2026-09-30');
  assert.deepEqual(marks, [
    { day: '2026-01-01', planned: false },
    { day: '2026-09-15', planned: false },
    { day: '2026-11-01', planned: true },
    { day: '2026-12-01', planned: true },
  ]);
});

test('the slider spans a month either side of today at least and every change at most', () => {
  assert.deepEqual(sliderRange([], '2026-09-30'), { from: '2026-08-31', to: '2026-10-30' });
  const range = sliderRange(markers(entries, [], '2026-09-30'), '2026-09-30');
  assert.deepEqual(range, { from: '2026-01-01', to: '2026-11-01' });
  assert.equal(sliderDayOf(sliderValueOf('2026-05-05', range), range), '2026-05-05');
  assert.equal(railPercent('2026-01-01', range), 0);
  assert.equal(railPercent('2026-11-01', range), 100);
  assert.equal(railPercent('2027-06-01', range), 100, 'a day beyond the rail sits at its end');
});

test('quick jumps offer the first change, the last past one, today and up to three planned days, once each', () => {
  const marks = markers([
    { effective_date: '2026-01-01' }, { effective_date: '2026-09-15' },
    { effective_date: '2026-10-01' }, { effective_date: '2026-11-01' }, { effective_date: '2026-12-01' }, { effective_date: '2027-01-01' },
  ], [], '2026-09-30');
  assert.deepEqual(quickJumps(marks, '2026-09-30').map((j) => [j.day, j.kind]), [
    ['2026-01-01', 'past'], ['2026-09-15', 'past'], ['2026-09-30', 'today'],
    ['2026-10-01', 'planned'], ['2026-11-01', 'planned'], ['2026-12-01', 'planned'],
  ]);
  assert.deepEqual(quickJumps([], '2026-09-30'), [{ day: '2026-09-30', kind: 'today' }]);
});

test('a value reads as its label, "none", or words for a flag, an assignment type or a share', () => {
  assert.equal(valueText('parent_unit_id', 'u-1', 'IT', t), 'IT');
  assert.equal(valueText('parent_unit_id', null, null, t), 'none');
  assert.equal(valueText('is_staff', 'true', null, t), 'yes');
  assert.equal(valueText('is_staff', 'false', null, t), 'no');
  assert.equal(valueText('assignment_type', 'acting', null, t), 'assignment_type.acting');
  assert.equal(valueText('share', '0.5', null, t), '50%');
  assert.equal(valueText('name', 'Zespół', null, t), 'Zespół');
});

const moveEntry = {
  id: 7,
  at: '2026-09-29 10:00:00',
  actor_name: 'Hanna',
  action: 'org.unit.move',
  target_name: 'DevOps',
  unit_name: 'DevOps',
  effective_date: '2026-11-01',
  changes: [{ field: 'parent_unit_id', before: 'u-0', after: 'u-9', before_label: 'IT', after_label: 'Realizacja' }],
};

test('an entry reads with its sentence, its before and after, who wrote it and whether it is planned', () => {
  const planned = entryModel(moveEntry, { t, today: '2026-09-30' });
  assert.equal(planned.title, 'action.unit_move{"name":"DevOps","unit":"DevOps","person":"","position":"","count":0}');
  assert.equal(planned.planned, true);
  assert.equal(planned.dot, 'plan');
  assert.deepEqual(planned.changes.map((c) => [c.label, c.before, c.after]), [['field.parent_unit_id', 'IT', 'Realizacja']]);
  assert.equal(planned.who, 'Hanna');
  assert.equal(planned.day, '2026-11-01');

  const past = entryModel({ ...moveEntry, effective_date: '2026-07-01' }, { t, today: '2026-09-30' });
  assert.equal(past.planned, false);
  assert.equal(past.dot, '');
});

test('a batch entry lists its operations, says how many were left out and names its source', () => {
  const batch = entryModel({
    id: 9, at: '2026-09-29 11:00:00', actor_name: 'Hanna', action: 'org.structure.batch', source: 'batch',
    effective_date: '2026-09-29', hidden_ops: 2,
    ops: Array.from({ length: 10 }, (_, i) => ({ action: 'org.unit.create', target_name: `U${i}` })),
  }, { t, today: '2026-09-30' });
  assert.equal(batch.ops.length, 9, 'eight lines and the "and more" one');
  assert.equal(batch.ops[8], 'ops_more{"count":2}');
  assert.equal(batch.hiddenOps, 2);
  assert.equal(batch.who, 'Hanna · source.batch');
  assert.equal(entryModel({ id: 1, at: '2026-09-29 11:00:00', action: 'org.unit.create', changes: [{ field: 'parent_unit_id', after: 'u-1' }] },
    { t, today: '2026-09-30' }).changes[0].created, true);
});

const items = [
  { change: 'added', entity: 'unit', id: 'qa', name: 'Quality', unit_id: 'qa' },
  { change: 'changed', entity: 'unit', id: 'devops', name: 'DevOps', unit_id: 'devops', field: 'parent_unit_id', before: 'it', after: 'board', before_label: 'IT', after_label: 'Board' },
  { change: 'added', entity: 'position', id: 'tester', name: 'Tester', unit_id: 'qa' },
  { change: 'changed', entity: 'position', id: 'lead', name: 'Lead', unit_id: 'devops', field: 'primary_parent_position_id', before: 'cto', after: null, before_label: 'Adam' },
  { change: 'removed', entity: 'assignment', id: 'lead', name: 'Lead', unit_id: 'devops', subject: { kind: 'user', id: 'ewa' }, subject_name: 'Ewa' },
  { change: 'added', entity: 'assignment', id: 'tester', name: 'Tester', unit_id: 'qa', subject: { kind: 'user', id: 'kasia' }, subject_name: 'Kasia' },
];

test('a difference reads as one sentence, with the values around a changed field', () => {
  assert.equal(diffLine(items[0], t).text, 'diff.unit_added{"name":"Quality"}');
  assert.deepEqual(diffLine(items[1], t), {
    text: 'diff.unit_changed{"name":"DevOps","field":"field.parent_unit_id"}', before: 'IT', after: 'Board',
  });
  assert.equal(diffLine(items[3], t).after, 'none', 'a cleared reference is "none"');
  assert.equal(diffLine(items[4], t).text, 'diff.assignment_removed{"person":"Ewa","name":"Lead"}');
  assert.equal(diffLine({ ...items[4], subject_name: '' }, t).text, 'diff.assignment_removed{"person":"diff.someone","name":"Lead"}');
});

test('the counts treat a thing with several changed fields as one thing', () => {
  const counts = diffCounts([...items, { ...items[1], field: 'name', before: 'a', after: 'b' }]);
  assert.deepEqual(counts, { added: 3, removed: 1, changed: 3, units: 2, positions: 2, assignments: 2 });
});

test('a diff marks positions and units on the chart, the strongest mark winning, and removed only on the "before" side', () => {
  const marks = diffMarks(items);
  assert.equal(marks.positions.get('tester'), 'added', 'an added position outranks its new holder');
  assert.equal(marks.positions.get('lead'), 'changed', 'a moved position and a leaving holder are one change');
  assert.equal(marks.units.get('qa'), 'added');
  assert.equal(marks.units.get('devops'), 'changed');
  assert.equal(marks.units.get('board'), undefined, 'only units the diff touches are marked');

  assert.equal(chartMark('added', 'after'), 'added');
  assert.equal(chartMark('added', 'before'), null);
  assert.equal(chartMark('removed', 'before'), 'error');
  assert.equal(chartMark('removed', 'after'), null);
  assert.equal(chartMark('changed', 'before'), 'changed');

  const model = decorateModel({
    nodes: [{ id: 'tester' }, { id: 'lead' }, { id: 'cto' }],
    units: [{ id: 'qa' }, { id: 'it' }],
  }, marks, 'after');
  assert.deepEqual(model.nodes.map((n) => n.mark), ['added', 'changed', null]);
  assert.deepEqual(model.units.map((u) => u.mark), ['added', null]);
});

const view = {
  at: '2026-09-30',
  units: [
    { unit_id: 'board', name: 'Board', parent_unit_id: null },
    { unit_id: 'it', name: 'IT', parent_unit_id: 'board' },
    { unit_id: 'devops', name: 'DevOps', parent_unit_id: 'it' },
    { unit_id: 'hr', name: 'HR', parent_unit_id: 'board' },
  ],
  positions: [
    { position_id: 'cto', unit_id: 'it' }, { position_id: 'lead', unit_id: 'devops' }, { position_id: 'hrm', unit_id: 'hr' },
  ],
  assignments: [{ position_id: 'lead', subject: { kind: 'user', id: 'ewa' } }, { position_id: 'hrm', subject: { kind: 'user', id: 'z' } }],
  vacancies: ['cto'],
};

test('a unit\'s part of a structure is the unit, its sub-units, their positions and the people on them', () => {
  assert.deepEqual([...subtreeUnitIds(view, 'it')].sort(), ['devops', 'it']);
  const part = viewSubset(view, 'it');
  assert.deepEqual(part.units.map((u) => u.unit_id), ['it', 'devops']);
  assert.deepEqual(part.positions.map((p) => p.position_id), ['cto', 'lead']);
  assert.deepEqual(part.assignments.map((a) => a.position_id), ['lead']);
  assert.deepEqual(part.vacancies, ['cto']);
  assert.equal(viewSubset(view, ''), view, 'no unit is the whole structure');
});

test('the unit choices list the busiest unit first', () => {
  const choices = unitChoices(view, view, items);
  assert.equal(choices[0].id, 'devops');
  assert.equal(choices[0].count, 3);
  assert.deepEqual(choices.map((c) => c.id).sort(), ['board', 'devops', 'hr', 'it', 'qa'].filter((id) => id !== 'qa').sort());
});

const set = (over) => ({ id: 'cs', state: 'pending', author_user_id: '11111111-2222-3333-4444-555555555555', effective_date: '2026-11-01', ...over });

test('only somebody other than the author may approve, and the author is told why the button is off', () => {
  const author = '11111111222233334444555555555555';
  const own = changeSetFlags(set(), { me: author, today: '2026-09-30' });
  assert.equal(own.canApprove, false);
  assert.equal(own.approveBlockedByAuthor, true);
  assert.equal(own.canEdit && own.canWithdraw, true);

  const sole = changeSetFlags(set(), { me: author, today: '2026-09-30', soleAdmin: true });
  assert.deepEqual([sole.canApprove, sole.approveBlockedByAuthor, sole.selfApproval], [true, false, true], 'the only administrator approves their own');

  const other = changeSetFlags(set(), { me: 'aaaaaaaabbbbccccddddeeeeeeeeeeee', today: '2026-09-30' });
  assert.equal(other.canApprove, true);
  assert.equal(other.approveBlockedByAuthor, false);
  assert.equal(other.days, 32);

  const draft = changeSetFlags(set({ state: 'draft' }), { me: 'x', today: '2026-09-30' });
  assert.deepEqual([draft.canSubmit, draft.canApprove, draft.isDraft], [true, false, true]);

  const applied = changeSetFlags(set({ state: 'applied' }), { me: 'x', today: '2026-09-30' });
  assert.deepEqual([applied.canApprove, applied.canEdit, applied.canWithdraw, applied.isApplied], [false, false, true, true], 'an approved one is withdrawable until its day');
  assert.equal(changeSetFlags(set({ state: 'applied', effective_date: '2026-09-30' }), { me: 'x', today: '2026-09-30' }).canWithdraw, false, 'not on the day itself');
  assert.equal(changeSetFlags(set({ effective_date: '2026-09-01' }), { me: 'x', today: '2026-09-30' }).datePassed, true);
});

test('open reorganizations come first, then approved ones waiting for their day, then the closed', () => {
  const groups = groupChangeSets([
    set({ id: 'a', state: 'applied', effective_date: '2026-05-01' }),
    set({ id: 'b', state: 'pending', effective_date: '2026-12-01' }),
    set({ id: 'c', state: 'draft', effective_date: '2026-10-01' }),
    set({ id: 'd', state: 'withdrawn', effective_date: '2026-06-01' }),
    set({ id: 'e', state: 'applied', effective_date: '2026-11-01' }),
    set({ id: 'f', state: 'applied', effective_date: '2026-09-30' }),
  ], '2026-09-30');
  assert.deepEqual(groups.open.map((s) => s.id), ['c', 'b']);
  assert.deepEqual(groups.upcoming.map((s) => s.id), ['f', 'e'], 'the day of taking effect is still "upcoming"');
  assert.deepEqual(groups.applied.map((s) => s.id), ['a']);
  assert.deepEqual(groups.withdrawn.map((s) => s.id), ['d']);
});

test('a typed refusal has its own sentence; an unknown rule falls back to the server\'s text', () => {
  assert.equal(errorText({ code: 'self_approval', message: 'x' }, t), 'error.self_approval{"date":"","id":"","field":""}');
  assert.equal(errorText({ code: 'from_a_newer_server', message: 'English text' }, t), 'English text');
  assert.equal(errorText(null, t), '');
  assert.deepEqual(failedOps([
    { index: 0, ok: true }, { index: 2, ok: false, error: { code: 'change_set_conflict', message: 'm' } },
  ], t).map((f) => f.index), [2]);
});

test('the stored operations come back in the shape the edit mode builds them in', () => {
  const stored = [
    { temp_id: 'tmp:q', request: { UnitCreateRequest: { name: 'Quality', parent_unit_id: 'board', valid_from: '2026-11-01', confirm_backdated: false } } },
    { temp_id: null, request: { PositionMoveRequest: { position_id: 'lead', new_parent_position_id: null, from: '2026-11-01' } } },
    { request: { AssignRequest: { position_id: 'p', subject: { kind: 'user', id: 'u' }, share: 1, valid_from: '2026-11-01' } } },
  ];
  const ops = opsFromWire(stored);
  assert.deepEqual(ops[0], { kind: 'unitCreate', tempId: 'tmp:q', name: 'Quality', parentUnitId: 'board', validFrom: '2026-11-01', confirmBackdated: false });
  assert.deepEqual(ops[1], { kind: 'positionMove', positionId: 'lead', newParentPositionId: null, from: '2026-11-01' });
  assert.equal(ops[2].kind, 'assign');
  assert.deepEqual(ops[2].subject, { kind: 'user', id: 'u' });
  assert.equal('tempId' in ops[1], false, 'no temp id, no key');
  assert.deepEqual(opsFromWire(undefined), []);
});

test('an operation is summarised by name where the live structure knows it, and can be dropped by position', () => {
  assert.equal(opSummary({ kind: 'unitCreate', name: 'Quality' }, view, t), 'op.unit_create{"name":"Quality"}');
  assert.equal(opSummary({ kind: 'unitMove', unitId: 'it' }, view, t), 'op.unit_move{"name":"IT"}');
  assert.equal(opSummary({ kind: 'somethingNew' }, view, t), 'op.unknown{"name":""}');
  assert.deepEqual(withoutOp([1, 2, 3], 1), [1, 3]);
});

test('the rail labels the first day, today and the marks that fit, today and the ends winning over a close mark', () => {
  const range = { from: '2026-01-01', to: '2026-11-01' };
  const marks = [{ day: '2026-05-12' }, { day: '2026-09-28' }, { day: '2026-09-15' }, { day: '2026-11-01' }];
  const ticks = railTicks(marks, '2026-09-30', range);
  assert.deepEqual(ticks.map((tick) => [tick.day, tick.kind]), [
    ['2026-01-01', 'start'], ['2026-05-12', 'mark'], ['2026-09-30', 'today'], ['2026-11-01', 'mark'],
  ]);
  assert.ok(ticks.every((tick, i) => i === 0 || tick.pos - ticks[i - 1].pos >= 9));
});

test('the slider span covers every marked day whatever order they come in', () => {
  const range = sliderRange([{ day: '2026-11-02' }, { day: '2026-09-30' }, { day: '2026-01-15' }], '2026-09-30');
  assert.deepEqual(range, { from: '2026-01-15', to: '2026-11-02' });
});

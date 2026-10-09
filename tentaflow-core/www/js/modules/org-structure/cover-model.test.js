// =============================================================================
// File: modules/org-structure/cover-model.test.js
// Description: The pure model of absences, deputies and visibility: the
//   inclusive/exclusive end-date conversion at the window's edge, the order of
//   rows, the badges the tree gets, what an absence patch names, and how the
//   visibility answer becomes rows. No DOM.
// =============================================================================

import '../../lib/actions/_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  absenceFields, absencePatch, absenceRows, dayCount, deputyRows, exclusiveEnd, lastDayOf, personOptions, phaseOf,
  rangeText, scopeKind, treeBadges, verdictTone, viewerRows, visibilityRows,
} from './cover-model.js';

test('the wire end is exclusive and the window talks in the last day, both ways', () => {
  assert.equal(lastDayOf('2026-10-25'), '2026-10-24');
  assert.equal(exclusiveEnd('2026-10-24'), '2026-10-25');
  assert.equal(lastDayOf(null), null);
  assert.equal(exclusiveEnd(''), null);
});

test('a range reads as one day, a span or an open end', () => {
  const opts = { from: 'from' };
  assert.equal(rangeText('2026-10-20', '2026-10-25', opts), '20/10/2026 – 24/10/2026');
  assert.equal(rangeText('2026-12-14', '2026-12-15', opts), '14/12/2026');
  assert.equal(rangeText('2026-10-20', null, opts), 'from 20/10/2026');
  assert.equal(dayCount({ valid_from: '2026-10-20', valid_to: '2026-10-25' }), 5);
  assert.equal(dayCount({ valid_from: '2026-10-20', valid_to: null }), null);
});

test('a phase is where the interval stands on the day, the end being exclusive', () => {
  const item = { valid_from: '2026-10-05', valid_to: '2026-10-08' };
  assert.equal(phaseOf(item, '2026-10-04'), 'upcoming');
  assert.equal(phaseOf(item, '2026-10-05'), 'current');
  assert.equal(phaseOf(item, '2026-10-07'), 'current');
  assert.equal(phaseOf(item, '2026-10-08'), 'past');
  assert.equal(phaseOf({ valid_from: '2026-01-01', valid_to: null }, '2026-10-08'), 'current');
});

test('absences are listed running first, then coming ones by date, then the past newest first', () => {
  const rows = absenceRows([
    { id: 'p1', valid_from: '2026-01-05', valid_to: '2026-01-06', source: 'manual' },
    { id: 'u2', valid_from: '2026-12-01', valid_to: '2026-12-03', source: 'manual' },
    { id: 'c1', valid_from: '2026-09-29', valid_to: '2026-10-02', source: 'edokumenty' },
    { id: 'u1', valid_from: '2026-11-01', valid_to: null, source: 'manual' },
    { id: 'p2', valid_from: '2026-03-01', valid_to: '2026-03-02', source: 'manual' },
  ], '2026-09-30');
  assert.deepEqual(rows.map((r) => r.absence.id), ['c1', 'u1', 'u2', 'p2', 'p1']);
  assert.deepEqual(rows.map((r) => r.manual), [false, true, true, true, true]);
});

test('deputies are listed in force first, then coming ones', () => {
  const rows = deputyRows([
    { id: 'b', valid_from: '2026-11-01', valid_to: null },
    { id: 'a', valid_from: '2026-09-01', valid_to: null },
    { id: 'z', valid_from: '2026-01-01', valid_to: '2026-02-01' },
  ], '2026-09-30');
  assert.deepEqual(rows.map((r) => [r.deputy.id, r.phase]), [['a', 'current'], ['b', 'upcoming'], ['z', 'past']]);
});

test('a wire scope is told from a project scope', () => {
  assert.equal(scopeKind('all'), 'all');
  assert.equal(scopeKind('escalations'), 'escalations');
  assert.equal(scopeKind('project:p-7'), 'project');
});

test('the tree badges name people by subject key and carry no reason', () => {
  const badges = treeBadges({
    absent_user_ids: ['u-1', 'u-2'],
    deputies: [{ deputy_user_id: 'u-9', user_id: 'u-1', scope: 'all' }],
  });
  assert.deepEqual([...badges.absentKeys].sort(), ['user:u-1', 'user:u-2']);
  assert.deepEqual([...badges.coveringKeys], ['user:u-9']);
  assert.deepEqual([...treeBadges(null).absentKeys], []);
});

test('the person picker lists accounts once, by name, and leaves out people without one', () => {
  const view = {
    assignments: [
      { subject: { kind: 'user', id: 'u-2' }, display_name: 'Zofia' },
      { subject: { kind: 'user', id: 'u-1' }, display_name: 'Anna' },
      { subject: { kind: 'user', id: 'u-1' }, display_name: 'Anna' },
      { subject: { kind: 'external', id: 'x-1' }, display_name: 'Ola' },
    ],
  };
  assert.deepEqual(personOptions(view), [{ id: 'u-1', name: 'Anna' }, { id: 'u-2', name: 'Zofia' }]);
});

test('the absence window turns its values into wire fields, or names the wrong one', () => {
  const ok = absenceFields({ from: '2026-10-20', last: '2026-10-24', kind: 'leave' });
  assert.deepEqual(ok.fields, { validFrom: '2026-10-20', validTo: '2026-10-25', kind: 'leave' });
  assert.deepEqual(absenceFields({ from: '2026-10-20', last: '', kind: 'other' }).fields,
    { validFrom: '2026-10-20', validTo: null, kind: 'other' });
  assert.equal(absenceFields({ from: '20.10.2026', last: '', kind: 'leave' }).error, 'from');
  assert.equal(absenceFields({ from: '2026-10-20', last: 'soon', kind: 'leave' }).error, 'last');
  assert.equal(absenceFields({ from: '2026-10-20', last: '2026-10-19', kind: 'leave' }).error, 'order');
  // A one-day absence: the last day is the first.
  assert.equal(absenceFields({ from: '2026-10-20', last: '2026-10-20', kind: 'leave' }).fields.validTo, '2026-10-21');
});

test('an absence patch names only what changed, and lists the emptied fields to clear', () => {
  const before = { valid_from: '2026-10-20', valid_to: '2026-10-25', kind: 'leave' };
  const same = absencePatch(before, { from: '2026-10-20', last: '2026-10-24', kind: 'leave' });
  assert.equal(same.changed, false);
  const moved = absencePatch(before, { from: '2026-10-21', last: '2026-10-24', kind: 'training' });
  assert.deepEqual(moved.patch, { validFrom: '2026-10-21', kind: 'training' });
  assert.deepEqual(moved.clear, []);
  const open = absencePatch(before, { from: '2026-10-20', last: '', kind: 'leave' });
  assert.deepEqual(open.clear, ['valid_to']);
  assert.equal(absencePatch(before, { from: 'x', last: '', kind: 'leave' }).error, 'from');
});

test('the visibility answer becomes rows in the mockup order with the people each verdict reaches', () => {
  const response = {
    subtree: [{ user_id: 'a', display_name: 'A' }, { user_id: 'b', display_name: 'B' }],
    direct: [{ user_id: 'a', display_name: 'A' }],
    rows: [
      { area: 'everyone_else', verdict: 'none', rule: 'none' },
      { area: 'absence_dates', verdict: 'subtree', rule: 'primary_manager' },
      { area: 'structure', verdict: 'all', rule: 'every_member' },
      { area: 'utilization', verdict: 'subtree', rule: 'primary_manager' },
      { area: 'position_history', verdict: 'own', rule: 'owner' },
    ],
  };
  const rows = visibilityRows(response);
  assert.deepEqual(rows.map((r) => r.area), ['structure', 'utilization', 'absence_dates', 'position_history', 'everyone_else']);
  assert.deepEqual(rows.map((r) => r.count), [0, 2, 2, 0, 0]);
  assert.deepEqual(rows.map((r) => r.tone), ['yes', 'yes', 'yes', 'part', 'no']);
  assert.equal(verdictTone('unheard-of'), 'no');
});

test('viewers come with the person first, then by rule strength and name', () => {
  const viewers = viewerRows({
    viewers: [
      { user_id: 'adm', display_name: 'Zed', rule: 'administrator' },
      { user_id: 'sup', display_name: 'Boss', rule: 'supervisor' },
      { user_id: 'me', display_name: 'Marek', rule: 'owner' },
      { user_id: 'dir', display_name: 'Anna', rule: 'primary_manager' },
      { user_id: 'adm2', display_name: 'Adam', rule: 'administrator' },
    ],
  }, 'me');
  assert.deepEqual(viewers.map((v) => v.user_id), ['me', 'dir', 'sup', 'adm2', 'adm']);
  assert.equal(viewers[0].self, true);
});

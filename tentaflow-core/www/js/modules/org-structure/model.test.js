// =============================================================================
// File: modules/org-structure/model.test.js
// Description: The read-model of the org-structure screen: who counts as a
//   person, how a warning is worded, and which strings are a calendar day.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  subjectKey, summarize, warningText,
} from './model.js';

const T = {
  vacancy: 'wakat',
  unknown_person: 'nieznana osoba',
};
const t = (key, params = {}) => (T[key] ?? `${key}:${JSON.stringify(params)}`);

const anna = { kind: 'user', id: 'u-anna' };
const jan = { kind: 'external', id: 'x-jan' };

const view = {
  at: '2026-09-30',
  timezone: 'Europe/Warsaw',
  units: [
    { unit_id: 'unit-it', name: 'IT', code: 'IT', type_id: 'ty-dept', head_position_id: 'pos-cto' },
    { unit_id: 'unit-qa', name: 'QA', code: null, type_id: null, head_position_id: null },
    { unit_id: 'unit-ops', name: 'Ops', code: null, type_id: null, head_position_id: 'pos-ops' },
  ],
  positions: [
    { position_id: 'pos-cto', unit_id: 'unit-it', name: 'CTO', primary_parent_position_id: null, is_staff: false, valid_from: '2026-01-01' },
    { position_id: 'pos-dev', unit_id: 'unit-it', name: 'Developer', primary_parent_position_id: 'pos-cto', is_staff: false, valid_from: '2026-02-01' },
    { position_id: 'pos-ops', unit_id: 'unit-ops', name: 'Ops lead', primary_parent_position_id: null, is_staff: true, valid_from: '2026-03-01' },
  ],
  assignments: [
    { position_id: 'pos-cto', subject: anna, display_name: 'Anna Nowak', share: 1 },
    { position_id: 'pos-dev', subject: anna, display_name: 'Anna Nowak', share: 0.5 },
    { position_id: 'pos-dev', subject: jan, display_name: '', share: 0.25 },
  ],
  vacancies: ['pos-ops'],
  warnings: [],
};

test('a person on two positions counts once and vacancies are the listed ones', () => {
  assert.deepEqual(summarize(view), { units: 3, positions: 3, people: 2, vacancies: 1 });
});

test('a platform account and an external person with one id are two people', () => {
  assert.notEqual(subjectKey({ kind: 'user', id: 'a' }), subjectKey({ kind: 'external', id: 'a' }));
});

test('every warning kind is worded with the name of what it is about', () => {
  assert.match(warningText({ kind: 'unit_without_head', unit_id: 'unit-qa' }, view, t), /"unit":"QA"/);
  assert.match(warningText({ kind: 'unit_without_head', unit_id: 'a907-not-in-view' }, view, t), /unknown_unit/, 'an id never reaches the sentence');
  assert.doesNotMatch(warningText({ kind: 'unit_without_head', unit_id: 'a907-not-in-view' }, view, t), /a907/);
  assert.match(warningText({ kind: 'share_overbooked', subject: anna, total: 1.5000001 }, view, t), /"person":"Anna Nowak".*"total":1.5/);
  assert.match(warningText({ kind: 'person_without_primary', subject: jan }, view, t), /nieznana osoba/);
  assert.match(warningText({ kind: 'from_a_newer_server' }, view, t), /warning_unknown/, 'an unknown kind is never shown raw');
});

// =============================================================================
// File: modules/org-structure/import-model.test.js
// Description: The pure model of the import panel: which files are accepted,
//   the request a run sends (login pinned to a decision, the ended-items
//   confirmation), the decisions offered per error, the error cards, what a
//   replace ends and who loses a seat, when "Zapisz wszystko" may be pressed
//   and which positions of the preview are new, changed or to be corrected.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  MAX_FILE_BYTES, actionsFor, canApply, endedItems, errorCards, errorCount, formatOfFile, needsEndedConfirmation, previewMarks,
  replaceImpact, rowsByStatus, rowsLabel, runPayload, seatsLost, unresolvedErrors, withResolution, withoutResolution,
} from './import-model.js';

const unknownPerson = {
  row: 47, rows: [47], column: 'person', kind: 'unknown_person', value: 'j.kowlski',
  suggestion: 'j.kowalski', suggestion_label: 'Jan Kowalski', message: 'x',
};
const cycle = { row: 89, rows: [88, 89], column: 'parent_code', kind: 'unit_cycle', message: 'x' };
const twoHeads = { row: 134, rows: [131, 134], column: 'head', kind: 'two_heads', message: 'x' };

const clean = { applied: false, counts: { rows: 10, added: 3, changed: 1, unchanged: 6, errors: 0 }, errors: [], rows: [] };
const withErrors = { ...clean, counts: { ...clean.counts, errors: 3 }, errors: [unknownPerson, cycle, twoHeads] };

test('only .csv and .xlsx are files of the import, whatever the case', () => {
  assert.equal(formatOfFile('struktura.CSV'), 'csv');
  assert.equal(formatOfFile('struktura-2026-09.xlsx'), 'xlsx');
  assert.equal(formatOfFile('struktura.xls'), null);
  assert.equal(formatOfFile('csv'), null);
  assert.equal(formatOfFile(''), null);
});

test('the file limit is what one frame of the dashboard socket can carry', () => {
  assert.equal(MAX_FILE_BYTES, 900 * 1024);
  assert.ok(MAX_FILE_BYTES < 1024 * 1024);
});

test('a run sends the same fields for a dry run, an apply and an error export', () => {
  const bytes = new Uint8Array([1, 2, 3]);
  assert.deepEqual(
    runPayload({ format: 'csv', bytes, mode: 'upsert', asOf: '', confirmBackdated: false, resolutions: [] }),
    { format: 'csv', bytes, mode: 'upsert', asOf: null, confirmBackdated: false, confirmEnded: false, resolutions: [] },
  );
  const payload = runPayload({
    format: 'xlsx', bytes, mode: 'replace', asOf: '2026-10-01', confirmBackdated: true, confirmEnded: true,
    resolutions: [{ row: 47, action: 'use_suggested_login', login: 'j.kowalski' }, { row: 3, action: 'skip_row' }],
  });
  assert.equal(payload.confirmEnded, true);
  assert.deepEqual(payload.resolutions, [
    { row: 47, action: 'use_suggested_login', login: 'j.kowalski' },
    { row: 3, action: 'skip_row' },
  ]);
});

test('one decision per row: deciding again replaces it and a decision can be taken back', () => {
  let decisions = withResolution([], 47, 'use_suggested_login', 'j.kowalski');
  decisions = withResolution(decisions, 3, 'skip_row');
  decisions = withResolution(decisions, 47, 'leave_vacant');
  assert.deepEqual(decisions, [{ row: 3, action: 'skip_row' }, { row: 47, action: 'leave_vacant' }]);
  assert.deepEqual(withoutResolution(decisions, 3), [{ row: 47, action: 'leave_vacant' }]);
});

test('the decisions offered follow what the server accepts for the error', () => {
  assert.deepEqual(actionsFor(unknownPerson), ['use_suggested_login', 'leave_vacant', 'skip_row']);
  assert.deepEqual(actionsFor({ ...unknownPerson, suggestion: null }), ['leave_vacant', 'skip_row'], 'no suggested login to use');
  assert.deepEqual(actionsFor({ row: 5, kind: 'ambiguous_person' }), ['leave_vacant', 'skip_row']);
  assert.deepEqual(actionsFor(cycle), ['skip_row']);
  assert.deepEqual(actionsFor({ row: 0, kind: 'unit_cycle' }), [], 'a problem of the file as a whole has no row to skip');
  assert.deepEqual(actionsFor({ row: 2, kind: 'backdated_confirmation_required' }), []);
  assert.deepEqual(actionsFor({ row: 2, kind: 'replace_matches_nothing' }), []);
});

test('rows are named as the spreadsheet numbers them: a run, a list or one', () => {
  assert.equal(rowsLabel(cycle), '88–89');
  assert.equal(rowsLabel(twoHeads), '131, 134');
  assert.equal(rowsLabel(unknownPerson), '47');
  assert.equal(rowsLabel({ row: 7, rows: [5, 6, 7] }), '5–7');
  assert.equal(rowsLabel({ row: 0, rows: [] }), '');
});

test('each error becomes a card with its decisions and the one already taken', () => {
  const cards = errorCards(withErrors, [{ row: 47, action: 'leave_vacant' }]);
  assert.equal(cards.length, 3);
  assert.deepEqual(cards.map((c) => c.issue.kind), ['unknown_person', 'unit_cycle', 'two_heads']);
  assert.equal(cards[0].decided, 'leave_vacant');
  assert.equal(cards[1].decided, null);
  assert.equal(new Set(cards.map((c) => c.key)).size, 3, 'keys are unique');
  assert.deepEqual(errorCards(null, []), []);
});

test('the number of errors is the number of listed problems, so the count and the list agree', () => {
  // A cycle is one problem on two rows: the server counts rows, the screen counts what it lists.
  assert.equal(errorCount({ counts: { errors: 5 }, errors: [unknownPerson, cycle, twoHeads] }), 3);
  assert.equal(errorCount({ counts: { errors: 2 } }), 2, 'no list at all, fall back to the server count');
  assert.equal(errorCount({ counts: { errors: 1 }, errors: [{ row: 0, kind: 'ended_confirmation_required' }] }), 0, 'the confirmation step is no error');
  assert.equal(errorCount(null), 0);
});

test('what a replace ends is read from the dry run and never computed on the screen', () => {
  const report = {
    counts: { units_ended: 2, positions_ended: 5, assignments_ended: 4 },
    ended: [
      { kind: 'unit', code: 'OLD', name: 'Stary dział', holders: [] },
      { kind: 'position', code: 'P-1', name: 'Księgowa', holders: ['Ewa Zet', 'Jan Nowy'] },
      { kind: 'assignment', code: '', name: 'Tester', holders: [{ display_name: 'Ewa Zet' }] },
    ],
  };
  assert.deepEqual(replaceImpact(report), { units: 2, positions: 5, assignments: 4, total: 11 });
  assert.equal(endedItems(report).length, 3);
  assert.deepEqual(endedItems(report)[2].holders, ['Ewa Zet'], 'a holder may arrive as an object');
  assert.equal(seatsLost(report), 2, 'each person once');
  assert.deepEqual(replaceImpact({ counts: {} }), { units: 0, positions: 0, assignments: 0, total: 0 });
  assert.deepEqual(endedItems({}), []);
});

test('the ended-items confirmation is asked in replace mode only, and only when something ends', () => {
  const ends = { counts: { units_ended: 1 }, ended: [] };
  assert.equal(needsEndedConfirmation(ends, 'replace'), true);
  assert.equal(needsEndedConfirmation(ends, 'upsert'), false);
  assert.equal(needsEndedConfirmation({ counts: {}, ended: [] }, 'replace'), false);
  assert.equal(needsEndedConfirmation({ counts: {}, ended: [{ kind: 'unit', holders: [] }] }, 'replace'), true);
  assert.equal(needsEndedConfirmation({ counts: {}, errors: [{ row: 0, kind: 'ended_confirmation_required' }] }, 'replace'), true);
  assert.deepEqual(errorCards({ errors: [{ row: 0, kind: 'ended_confirmation_required' }, unknownPerson] }, []).map((c) => c.issue.kind), ['unknown_person'], 'never a card');
});

test('"Zapisz wszystko" needs a report without errors that is not applied and not busy', () => {
  assert.equal(canApply({ report: clean, busy: null }), true);
  assert.equal(canApply({ report: null, busy: null }), false, 'no dry run yet');
  assert.equal(canApply({ report: withErrors, busy: null }), false);
  assert.equal(canApply({ report: { ...clean, counts: { ...clean.counts, errors: 1 }, errors: undefined }, busy: null }), false, 'the count alone blocks when there is no list');
  const confirm = { row: 0, rows: [], kind: 'ended_confirmation_required', message: 'x' };
  assert.equal(canApply({ report: { ...clean, counts: { ...clean.counts, errors: 1 }, errors: [confirm] }, busy: null }), true, 'a replace that only awaits its confirmation can be applied');
  assert.equal(canApply({ report: clean, busy: 'dry' }), false);
  assert.equal(canApply({ report: { ...clean, applied: true }, busy: null }), false);
  assert.equal(canApply({ report: { ...clean, file_error: { code: 'empty_file' } }, busy: null }), false);
  assert.equal(unresolvedErrors(withErrors), true);
  assert.equal(unresolvedErrors(clean), false);
});

test('the preview marks positions by what the file does, an error beating a change', () => {
  const marks = previewMarks({
    rows: [
      { row: 2, status: 'added', position_id: 'p-new' },
      { row: 3, status: 'changed', position_id: 'p-chg' },
      { row: 4, status: 'unchanged', position_id: 'p-same' },
      { row: 5, status: 'changed', position_id: 'p-both' },
      { row: 6, status: 'error', position_id: 'p-both' },
      { row: 7, status: 'error', position_id: null },
    ],
  });
  assert.deepEqual([...marks], [['p-new', 'added'], ['p-chg', 'changed'], ['p-both', 'error']]);
});

test('the list tabs take the rows of their status', () => {
  const report = { rows: [{ row: 2, status: 'added' }, { row: 3, status: 'changed' }, { row: 4, status: 'added' }] };
  assert.deepEqual(rowsByStatus(report, 'added').map((r) => r.row), [2, 4]);
  assert.deepEqual(rowsByStatus(report, 'changed').map((r) => r.row), [3]);
  assert.deepEqual(rowsByStatus(null, 'added'), []);
});

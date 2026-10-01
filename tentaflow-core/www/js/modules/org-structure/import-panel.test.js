// =============================================================================
// File: modules/org-structure/import-panel.test.js
// Description: The import window against a stubbed transport, through its
//   states: nothing chosen → a dry run → the report with its error cards →
//   decisions (each one runs the dry run again, the login pinned) → apply.
//   Also: the replace-mode warning and the confirmation of what it ends, the
//   backdated confirmation, "Pokaż w pliku", the error report download, the
//   preview with its marks, a file error, a refused file and a stale answer.
// =============================================================================

import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import { window, I18n, sleep, closed, cleanBody } from '../../lib/actions/_test-setup.js';

process.on('unhandledRejection', () => {});
URL.createObjectURL = () => 'blob:test';
URL.revokeObjectURL = () => {};

const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
const { formatDay } = await import('/js/lib/date-format.js');
const { openImportPanel } = await import('./import-panel.js');

const it = (key, params) => I18n.t(`org_structure.import.${key}`, params);
const DAY = '2026-09-30';

const REPORT_KINDS = /^orgImport(DryRun|Apply)Request$/;
const calls = [];
let script = {};
function stub(handlers) {
  script = handlers;
  ApiBinary.one = (kind, payload) => {
    calls.push({ kind, payload });
    const handler = script[kind];
    if (!handler) return Promise.reject(new Error(`unexpected request ${kind}`));
    try {
      const value = typeof handler === 'function' ? handler(payload) : handler;
      // The wire answers a dry run and an apply with `{ report }`; the fixtures are the reports.
      return Promise.resolve(REPORT_KINDS.test(kind) && !value?.then && !value?.report ? { report: value } : value);
    } catch (err) {
      return Promise.reject(err);
    }
  };
}

const counts = (over = {}) => ({
  rows: 212, added: 184, changed: 12, unchanged: 13, errors: 0,
  units_added: 30, positions_added: 90, assignments_added: 64, units_changed: 1, positions_changed: 4, assignments_changed: 7,
  units_ended: 0, positions_ended: 0, assignments_ended: 0, ...over,
});

const unknownPerson = {
  row: 47, rows: [47], column: 'person', kind: 'unknown_person', value: 'j.kowlski', message: 'no account',
  suggestion: 'j.kowalski', suggestion_label: 'Jan Kowalski',
};
const cycle = { row: 89, rows: [88, 89], column: 'parent_code', kind: 'unit_cycle', message: 'cycle' };
const twoHeads = { row: 134, rows: [131, 134], column: 'head', kind: 'two_heads', message: 'heads' };

const report = (over = {}) => ({
  mode: 'upsert', as_of: DAY, preview_at: DAY, applied: false, file_error: null, counts: counts(),
  rows: [
    { row: 2, status: 'added', unit_code: 'IT', position_code: 'IT-1', person: 'anna', position_id: 'p-1', changes: [{ entity: 'assignment', field: 'created', before: null, after: null }] },
    { row: 3, status: 'changed', unit_code: 'IT', position_code: 'IT-2', person: 'jan', position_id: 'p-2', changes: [{ entity: 'position', field: 'manager', before: 'IT-9', after: 'IT-1' }] },
    { row: 47, status: 'error', unit_code: 'IT', position_code: 'IT-3', person: 'j.kowlski', position_id: 'p-3', changes: [] },
  ],
  errors: [], warnings: [], preview: null, preview_partial: false, ...over,
});

const withErrors = () => report({ counts: counts({ errors: 3 }), errors: [unknownPerson, cycle, twoHeads] });

const preview = () => ({
  at: DAY,
  timezone: 'Europe/Warsaw',
  units: [{ unit_id: 'u-it', name: 'IT', head_position_id: 'p-1', deputy_head_position_ids: [] }],
  positions: [
    { position_id: 'p-1', unit_id: 'u-it', name: 'Head', primary_parent_position_id: null, valid_from: DAY },
    { position_id: 'p-2', unit_id: 'u-it', name: 'Dev', primary_parent_position_id: 'p-1', valid_from: DAY },
    { position_id: 'p-3', unit_id: 'u-it', name: 'QA', primary_parent_position_id: 'p-1', valid_from: DAY },
  ],
  assignments: [], vacancies: [], warnings: [],
});

const file = (name = 'struktura.csv', size = 40) => ({ name, size, arrayBuffer: async () => new Uint8Array([1, 2, 3]).buffer });

let applied = 0;
let win;

function open() {
  cleanBody();
  calls.length = 0;
  applied = 0;
  win = openImportPanel({ day: DAY, unitTypes: [], onApplied: async () => { applied += 1; } });
  return win;
}

const q = (role) => win.querySelector(`[data-role="${role}"]`);
const btn = (act) => win.querySelector(`[data-act="${act}"]`);
const dispatch = (el, type, detail) => el.dispatchEvent(new window.CustomEvent(type, { bubbles: true, detail }));
const chooseFile = async (f = file()) => { dispatch(q('file'), 'change', { files: [f] }); await sleep(0); await sleep(0); };
const cards = () => [...win.querySelectorAll('.org-imp-card')];
const cardButtons = (card) => [...card.querySelectorAll('[data-act]')].map((b) => b.dataset.act + (b.dataset.action ? `:${b.dataset.action}` : ''));
const statValues = () => [...win.querySelectorAll('tf-stat-card')].map((c) => [c.getAttribute('label'), c.getAttribute('value')]);

beforeEach(() => stub({}));

test('before a file is chosen: the mode, the day and the guidance, and nothing can be saved', () => {
  open();
  assert.equal(win.hasAttribute('modal'), true);
  assert.ok(q('file'), 'the file input');
  assert.equal(q('mode').value, 'upsert', 'upsert is the default');
  assert.equal(q('asof').getAttribute('value'), DAY);
  assert.equal(q('report').hidden, true);
  assert.equal(q('status').querySelector('tf-alert').getAttribute('message'), it('idle'));
  assert.equal(btn('apply').hasAttribute('disabled'), true);
  assert.equal(btn('apply').getAttribute('title'), it('apply_blocked_none'));
  assert.equal(btn('errors-report').hasAttribute('disabled'), true);
});

test('choosing a file runs the dry run with the file, the mode, the day and no decisions', async () => {
  stub({ orgImportDryRunRequest: report() });
  open();
  await chooseFile(file('struktura-solutio.xlsx'));
  assert.equal(calls.length, 1);
  assert.equal(calls[0].kind, 'orgImportDryRunRequest');
  assert.deepEqual(calls[0].payload, {
    format: 'xlsx', bytes: new Uint8Array([1, 2, 3]), mode: 'upsert', asOf: DAY,
    confirmBackdated: false, confirmEnded: false, resolutions: [],
  });
  assert.equal(q('meta').textContent, it('file_meta', { name: 'struktura-solutio.xlsx', count: 212 }));
});

test('the report shows the four counts, the tabs with their numbers and the error cards', async () => {
  stub({ orgImportDryRunRequest: withErrors() });
  open();
  await chooseFile();
  assert.equal(q('report').hidden, false);
  assert.deepEqual(statValues(), [[it('stat_added'), '184'], [it('stat_changed'), '12'], [it('stat_errors'), '3'], [it('stat_unchanged'), '13']]);
  assert.equal(win.querySelector('tf-stat-card[accent="danger"]').getAttribute('label'), it('stat_errors'));
  const tabs = [...q('tabs').querySelectorAll('.tf-seg-opt')].map((b) => b.textContent);
  assert.deepEqual(tabs, [it('tab_errors', { count: 3 }), it('tab_changed', { count: 12 }), it('tab_added', { count: 184 })]);
  assert.equal(cards().length, 3);
});

test('each card names the problem, where it is, and offers the decisions the server accepts', async () => {
  stub({ orgImportDryRunRequest: withErrors() });
  open();
  await chooseFile();
  const [person, loop, heads] = cards();
  assert.equal(person.querySelector('.org-imp-card-title').textContent, it('issue_unknown_person'));
  assert.equal(
    person.querySelector('.org-imp-card-where').textContent,
    `${it('location_rows', { count: 1, rows: '47' })} · ${it('location_column', { column: it('col_person') })} · ${it('location_value', { value: 'j.kowlski' })}`,
  );
  assert.match(person.querySelector('.org-imp-card-hint').textContent, /j\.kowalski.*Jan Kowalski/);
  assert.deepEqual(cardButtons(person), ['decide:use_suggested_login', 'decide:leave_vacant', 'decide:skip_row', 'show']);
  assert.match(loop.querySelector('.org-imp-card-where').textContent, /88–89/);
  assert.deepEqual(cardButtons(loop), ['decide:skip_row', 'show']);
  assert.match(heads.querySelector('.org-imp-card-where').textContent, /131, 134/);
  assert.equal(person.querySelector('[data-action="use_suggested_login"]').textContent.trim(), it('act_use_login'));
  assert.equal(person.querySelector('[data-action="leave_vacant"]').textContent.trim(), it('act_leave_vacant'));
});

test('with errors "Zapisz wszystko" stays locked and says how many there are; the error report is available', async () => {
  stub({ orgImportDryRunRequest: withErrors() });
  open();
  await chooseFile();
  assert.equal(btn('apply').hasAttribute('disabled'), true);
  assert.equal(btn('apply').getAttribute('title'), it('apply_blocked_errors', { count: 3 }));
  assert.equal(btn('errors-report').hasAttribute('disabled'), false);
  btn('apply').click();
  await sleep(0);
  assert.equal(calls.filter((c) => c.kind === 'orgImportApplyRequest').length, 0, 'a locked button sends nothing');
});

test('a decision runs the dry run again with the login pinned, and the resolved error goes away', async () => {
  let run = 0;
  stub({
    orgImportDryRunRequest: () => {
      run += 1;
      return run === 1 ? withErrors() : report({ counts: counts({ errors: 2 }), errors: [cycle, twoHeads] });
    },
  });
  open();
  await chooseFile();
  win.querySelector('[data-action="use_suggested_login"]').click();
  await sleep(0);
  assert.equal(calls.length, 2);
  assert.deepEqual(calls[1].payload.resolutions, [{ row: 47, action: 'use_suggested_login', login: 'j.kowalski' }]);
  assert.equal(cards().length, 2);
  const chip = q('decisions').querySelector('tf-chip');
  assert.equal(chip.getAttribute('label'), it('decision_row', { row: 47, action: it('decision_use_suggested_login') }));
});

test('taking a decision back runs the dry run again without it', async () => {
  stub({ orgImportDryRunRequest: withErrors() });
  open();
  await chooseFile();
  win.querySelector('[data-action="leave_vacant"]').click();
  await sleep(0);
  assert.deepEqual(calls[1].payload.resolutions, [{ row: 47, action: 'leave_vacant' }]);
  dispatch(q('decisions').querySelector('tf-chip'), 'remove');
  await sleep(0);
  assert.deepEqual(calls[2].payload.resolutions, []);
});

test('with no error left the save is open, and the flow ends in the apply, a toast and a reload', async () => {
  stub({
    orgImportDryRunRequest: report(),
    orgImportApplyRequest: report({ applied: true }),
  });
  open();
  await chooseFile();
  assert.equal(win.querySelector('.org-imp-cards'), null, 'no error cards');
  assert.equal(btn('apply').hasAttribute('disabled'), false);
  btn('apply').click();
  await sleep(0);
  await sleep(0);
  const apply = calls.find((c) => c.kind === 'orgImportApplyRequest');
  assert.deepEqual(apply.payload, {
    format: 'csv', bytes: new Uint8Array([1, 2, 3]), mode: 'upsert', asOf: DAY,
    confirmBackdated: false, confirmEnded: false, resolutions: [],
  });
  await closed();
  assert.equal(win.isConnected, false);
  assert.equal(applied, 1);
  const toasts = [...document.querySelectorAll('.tf-toast-message')].map((m) => m.textContent);
  assert.ok(toasts.includes(it('applied', { added: 184, changed: 12, ended: 0 })), toasts.join('|'));
});

test('an apply that finds errors keeps the window and shows what the server found', async () => {
  stub({ orgImportDryRunRequest: report(), orgImportApplyRequest: withErrors() });
  open();
  await chooseFile();
  btn('apply').click();
  await sleep(0);
  await sleep(0);
  assert.equal(win.isConnected, true);
  assert.equal(applied, 0);
  assert.equal(cards().length, 3);
});

test('the changed and added tabs list the rows with what changes', async () => {
  stub({ orgImportDryRunRequest: report() });
  open();
  await chooseFile();
  assert.equal(q('tabs').value, 'changed', 'no errors, so the first thing to read is what changes');
  const changed = q('list').querySelector('tf-table').rows;
  assert.deepEqual(changed.map((r) => [r.row, r.what, r.where]), [[3, 'jan', 'IT · IT-2']]);
  assert.equal(changed[0].changes, `${it('field_manager')}: IT-9 → IT-1`);
  dispatch(q('tabs'), 'change', { value: 'added' });
  const added = q('list').querySelector('tf-table').rows;
  assert.deepEqual(added.map((r) => [r.row, r.changes]), [[2, it('change_created_assignment')]]);
});

test('a decision that does not fit the row is shown as its own error, not swallowed', async () => {
  stub({ orgImportDryRunRequest: report({ counts: counts({ errors: 1 }), errors: [{ row: 47, rows: [47], kind: 'resolution_not_applicable', message: 'x' }] }) });
  open();
  await chooseFile();
  assert.equal(cards()[0].querySelector('.org-imp-card-title').textContent, it('issue_resolution_not_applicable'));
  assert.deepEqual(cardButtons(cards()[0]), ['show']);
});

test('a rule of the structure that refused an operation is worded by its code', async () => {
  const rejected = { row: 9, rows: [9], kind: 'rejected', code: 'reporting_cycle', message: 'x' };
  stub({ orgImportDryRunRequest: report({ counts: counts({ errors: 1 }), errors: [rejected] }) });
  open();
  await chooseFile();
  assert.equal(cards()[0].querySelector('.org-imp-card-title').textContent, I18n.t('org_structure.list.errors.reporting_cycle'));
});

test('a kind this build does not know still names itself', async () => {
  stub({ orgImportDryRunRequest: report({ counts: counts({ errors: 1 }), errors: [{ row: 9, rows: [9], kind: 'kind_from_the_future', message: 'x' }] }) });
  open();
  await chooseFile();
  assert.equal(cards()[0].querySelector('.org-imp-card-title').textContent, it('issue_unknown', { kind: 'kind_from_the_future' }));
});

test('an XLSX issue names its sheet in its location', async () => {
  stub({ orgImportDryRunRequest: report({ sheet: 'Osoby', counts: counts({ errors: 1 }), errors: [unknownPerson] }) });
  open();
  await chooseFile(file('a.xlsx'));
  assert.ok(cards()[0].querySelector('.org-imp-card-where').textContent.startsWith(it('location_sheet_rows', { sheet: 'Osoby', count: 1, rows: '47' })));
});

test('the warnings of the report are listed under the cards', async () => {
  stub({ orgImportDryRunRequest: report({ counts: counts({ errors: 1 }), errors: [cycle], warnings: [{ row: 12, rows: [12], kind: 'unit_without_head', message: 'x' }] }) });
  open();
  await chooseFile();
  assert.equal(win.querySelector('.org-imp-warnings li').textContent, it('warning_line', { rows: '12', text: it('issue_unit_without_head') }));
});

// --- backdated, show in file, error report ---------------------------------------------------------------------

test('a backdated confirmation card confirms and runs the dry run again with the flag', async () => {
  let run = 0;
  stub({
    orgImportDryRunRequest: () => {
      run += 1;
      return run === 1
        ? report({ counts: counts({ errors: 1 }), errors: [{ row: 5, rows: [5], kind: 'backdated_confirmation_required', message: 'x' }] })
        : report();
    },
  });
  open();
  await chooseFile();
  assert.deepEqual(cardButtons(cards()[0]), ['confirm-backdated', 'show']);
  btn('confirm-backdated').click();
  await sleep(0);
  assert.equal(calls[1].payload.confirmBackdated, true);
  assert.equal(cards().length, 0);
});

test('"Pokaż w pliku" shows every header with its value for EVERY row of the problem, under the sheet name', async () => {
  const cell = (header, value) => ({ header, value });
  stub({
    orgImportDryRunRequest: report({
      sheet: 'Osoby',
      counts: counts({ errors: 1 }),
      errors: [cycle],
      rows: [
        { row: 88, status: 'error', effect: 'added', cells: [cell('kod jednostki', 'DEV'), cell('nazwa jednostki', 'Zespół DevOps'), cell('kod nadrzędnej', 'REAL'), cell('typ', '')] },
        { row: 89, status: 'error', effect: 'added', cells: [cell('kod jednostki', 'REAL'), cell('kod nadrzędnej', 'DEV')] },
      ],
    }),
  });
  open();
  await chooseFile(file('a.xlsx'));
  win.querySelector('[data-act="show"]').click();
  const info = document.querySelector('tf-window.org-imp-show');
  assert.equal(info._titleEl.textContent, it('show_title_sheet', { sheet: 'Osoby', rows: '88–89' }), 'the sheet is in the title');
  const sections = [...info.querySelectorAll('.org-imp-show-row')];
  assert.deepEqual(sections.map((sec) => sec.querySelector('.org-imp-caption').textContent), [it('show_row_title', { row: 88 }), it('show_row_title', { row: 89 })]);
  assert.deepEqual([...sections[0].querySelectorAll('dt, dd')].map((n) => n.textContent), [
    'kod jednostki', 'DEV', 'nazwa jednostki', 'Zespół DevOps', 'kod nadrzędnej', 'REAL',
  ], 'empty cells are left out');
  assert.deepEqual([...sections[1].querySelectorAll('dt, dd')].map((n) => n.textContent), ['kod jednostki', 'REAL', 'kod nadrzędnej', 'DEV']);
  assert.equal(info.querySelector('tf-alert'), null, 'no note about what the report lacks');
});

test('a row the report carries no cells for says so instead of showing nothing', async () => {
  stub({ orgImportDryRunRequest: report({ counts: counts({ errors: 1 }), errors: [cycle] }) });
  open();
  await chooseFile();
  win.querySelector('[data-act="show"]').click();
  const info = document.querySelector('tf-window.org-imp-show');
  assert.equal(info.querySelectorAll('.org-imp-empty').length, 2);
  assert.equal(info.querySelector('.org-imp-empty').textContent, it('show_no_cells'));
});

test('the error report is requested with the very same run and downloaded', async () => {
  stub({
    orgImportDryRunRequest: withErrors(),
    orgExportErrorsRequest: { file_name: 'errors.csv', mime: 'text/csv', bytes: new Uint8Array([9]) },
  });
  open();
  await chooseFile();
  win.querySelector('[data-action="skip_row"]').click();
  await sleep(0);
  btn('errors-report').click();
  await sleep(0);
  const exported = calls.find((c) => c.kind === 'orgExportErrorsRequest');
  assert.deepEqual(exported.payload, calls[1].payload, 'same inputs as the last dry run');
  assert.deepEqual(exported.payload.resolutions, [{ row: 47, action: 'skip_row' }]);
});

// --- replace mode --------------------------------------------------------------------------------------------------------

const replaceReport = () => report({
  mode: 'replace',
  counts: counts({ errors: 1, units_ended: 2, positions_ended: 5, assignments_ended: 4 }),
  // As the server answers a replace that ends things and was not confirmed: one "error" that is the question.
  errors: [{ row: 0, rows: [], kind: 'ended_confirmation_required', message: 'confirm' }],
  ended: [
    { kind: 'unit', code: 'OLD', name: 'Old unit', holders: [] },
    { kind: 'position', code: 'P-7', name: 'Accountant', holders: ['Ewa Zet', 'Jan Nowy'] },
    { kind: 'assignment', code: '', name: 'Tester', holders: ['Ewa Zet'] },
  ],
});

test('replace mode warns before anything is run and shows what it ends once it has', async () => {
  stub({ orgImportDryRunRequest: (p) => (p.mode === 'replace' ? replaceReport() : report()) });
  open();
  dispatch(q('mode'), 'change', { value: 'replace' });
  assert.equal(q('mode-note').querySelector('tf-alert').getAttribute('tone'), 'warning');
  assert.equal(q('mode-note').querySelector('tf-alert').getAttribute('message'), it('mode_replace_note'));
  assert.equal(calls.length, 0, 'no file yet, no run');

  await chooseFile();
  assert.equal(calls[0].payload.mode, 'replace');
  assert.equal(
    q('mode-note').querySelector('tf-alert').getAttribute('message'),
    `${it('mode_replace_note')} ${it('replace_impact', { units: 2, positions: 5, assignments: 4, date: formatDay(DAY) })}`,
  );
  assert.deepEqual(statValues().pop(), [it('stat_ended'), '11'], 'a fifth count for what ends');
  const tabs = [...q('tabs').querySelectorAll('.tf-seg-opt')].map((b) => b.dataset.value);
  assert.deepEqual(tabs, ['errors', 'changed', 'added', 'ended']);
});

test('the ended tab lists each item with who loses a seat', async () => {
  stub({ orgImportDryRunRequest: replaceReport() });
  open();
  dispatch(q('mode'), 'change', { value: 'replace' });
  await chooseFile();
  dispatch(q('tabs'), 'change', { value: 'ended' });
  const rows = q('list').querySelector('tf-table').rows;
  assert.deepEqual(rows.map((r) => [r.kind, r.name, r.holders]), [
    [it('ended_kind_unit'), 'Old unit (OLD)', it('ended_nobody')],
    [it('ended_kind_position'), 'Accountant (P-7)', 'Ewa Zet, Jan Nowy'],
    [it('ended_kind_assignment'), 'Tester', 'Ewa Zet'],
  ]);
});

test('an apply that ends things asks first and sends confirmEnded only after the confirmation', async () => {
  ApiBinary.one = (kind, payload) => {
    calls.push({ kind, payload });
    if (kind === 'orgImportDryRunRequest') return Promise.resolve({ report: replaceReport() });
    return Promise.resolve({ report: { ...replaceReport(), applied: true } });
  };
  open();
  dispatch(q('mode'), 'change', { value: 'replace' });
  await chooseFile();
  btn('apply').click();
  await sleep(0);
  assert.equal(calls.filter((c) => c.kind === 'orgImportApplyRequest').length, 0, 'nothing is sent before the confirmation');
  const confirm = document.querySelector('tf-window.tf-act-window');
  assert.ok(confirm);
  assert.equal(confirm.querySelector('.tf-act__subject').textContent, it('end_confirm_subject', { date: formatDay(DAY) }));
  assert.ok(confirm.querySelector('.tf-act__note').getAttribute('message').startsWith(it('end_confirm_note', { units: 2, positions: 5, assignments: 4, seats: 2 })));
  confirm.querySelector('[data-act="submit"]').click();
  await sleep(0);
  await sleep(0);
  const apply = calls.find((c) => c.kind === 'orgImportApplyRequest');
  assert.equal(apply.payload.confirmEnded, true);
  assert.equal(apply.payload.mode, 'replace');
  await closed();
  assert.equal(applied, 1);
});

test('declining the ended-items confirmation sends nothing', async () => {
  ApiBinary.one = (kind, payload) => {
    calls.push({ kind, payload });
    return Promise.resolve({ report: replaceReport() });
  };
  open();
  dispatch(q('mode'), 'change', { value: 'replace' });
  await chooseFile();
  btn('apply').click();
  await sleep(0);
  document.querySelector('tf-window.tf-act-window [data-act="cancel"]').click();
  await closed();
  assert.equal(calls.filter((c) => c.kind === 'orgImportApplyRequest').length, 0);
  assert.equal(win.isConnected, true, 'the import window stays');
});

test('a replace that ends nothing is applied without a question', async () => {
  ApiBinary.one = (kind, payload) => {
    calls.push({ kind, payload });
    return Promise.resolve({ report: kind === 'orgImportApplyRequest' ? { ...report({ mode: 'replace' }), applied: true } : report({ mode: 'replace' }) });
  };
  open();
  dispatch(q('mode'), 'change', { value: 'replace' });
  await chooseFile();
  assert.match(q('mode-note').querySelector('tf-alert').getAttribute('message'), new RegExp(it('replace_none').slice(0, 20)));
  btn('apply').click();
  await sleep(0);
  await sleep(0);
  assert.equal(document.querySelector('tf-window.tf-act-window'), null);
  assert.equal(calls.filter((c) => c.kind === 'orgImportApplyRequest')[0].payload.confirmEnded, false);
});

// --- inputs that change the run ---------------------------------------------------------------------------------------

test('changing the day runs the dry run again for that day; an invalid day does not', async () => {
  ApiBinary.one = (kind, payload) => { calls.push({ kind, payload }); return Promise.resolve({ report: report() }); };
  open();
  await chooseFile();
  dispatch(q('asof'), 'change', { value: '2026-10-05' });
  await sleep(0);
  assert.equal(calls.length, 2);
  assert.equal(calls[1].payload.asOf, '2026-10-05');
  dispatch(q('asof'), 'change', { value: '2026-13-45' });
  await sleep(0);
  assert.equal(calls.length, 2);
});

test('a new file forgets the decisions and the backdated confirmation of the old one', async () => {
  ApiBinary.one = (kind, payload) => { calls.push({ kind, payload }); return Promise.resolve({ report: withErrors() }); };
  open();
  await chooseFile();
  win.querySelector('[data-action="skip_row"]').click();
  await sleep(0);
  await chooseFile(file('other.csv'));
  assert.deepEqual(calls[calls.length - 1].payload.resolutions, []);
  assert.equal(calls[calls.length - 1].payload.confirmBackdated, false);
});

// --- refusals ------------------------------------------------------------------------------------------------------------------

test('a file over the frame limit is refused before it is sent, with the server’s sentence', async () => {
  open();
  await chooseFile(file('big.csv', 900 * 1024 + 1));
  assert.equal(calls.length, 0);
  assert.equal(q('status').querySelector('tf-alert').getAttribute('message'), it('file_error_file_too_large'));
  assert.equal(btn('apply').hasAttribute('disabled'), true);
});

test('a file that is neither CSV nor XLSX is refused before it is sent', async () => {
  open();
  await chooseFile(file('list.pdf'));
  assert.equal(calls.length, 0);
  assert.equal(q('status').querySelector('tf-alert').getAttribute('message'), it('file_bad_type'));
});

test('an unusable file is a sentence about the file, with the missing column named', async () => {
  stub({ orgImportDryRunRequest: report({ file_error: { code: 'missing_column', message: 'x', field: 'unit_code' }, counts: counts({ rows: 0 }) }) });
  open();
  await chooseFile();
  assert.equal(q('report').hidden, true);
  assert.equal(q('status').querySelector('tf-alert').getAttribute('message'), it('file_error_missing_column', { column: it('col_unit_code') }));
  assert.equal(btn('apply').hasAttribute('disabled'), true);
});

test('a file error this build has no sentence for still names its code', async () => {
  stub({ orgImportDryRunRequest: report({ file_error: { code: 'error_from_the_future', message: 'x' } }) });
  open();
  await chooseFile();
  assert.equal(q('status').querySelector('tf-alert').getAttribute('message'), it('file_error_unknown', { code: 'error_from_the_future' }));
});

test('a request that fails is shown as a sentence and leaves nothing to save', async () => {
  stub({ orgImportDryRunRequest: () => { throw new Error('connection lost'); } });
  open();
  await chooseFile();
  assert.equal(q('status').querySelector('tf-alert').getAttribute('message'), it('request_failed', { message: 'connection lost' }));
  assert.equal(q('report').hidden, true);
  assert.equal(btn('apply').hasAttribute('disabled'), true);
});

test('an answer to an earlier run never overwrites the report of a later one', async () => {
  const late = {};
  let run = 0;
  stub({
    orgImportDryRunRequest: () => {
      run += 1;
      if (run === 1) return new Promise((resolve) => { late.resolve = () => resolve({ report: withErrors() }); });
      return { report: report() };
    },
  });
  open();
  dispatch(q('file'), 'change', { files: [file('first.csv')] });
  await sleep(0);
  dispatch(q('asof'), 'change', { value: '2026-10-05' });
  await sleep(0);
  late.resolve();
  await sleep(0);
  assert.equal(cards().length, 0, 'the late answer of the first run is dropped');
  assert.equal(btn('apply').hasAttribute('disabled'), false);
});

// --- the preview -----------------------------------------------------------------------------------------------------------------

test('the preview draws the tree the file leaves, marking added, changed and to-correct positions', async () => {
  stub({ orgImportDryRunRequest: report({ counts: counts({ errors: 1 }), errors: [cycle], preview: preview(), preview_partial: true }) });
  open();
  await chooseFile();
  const chart = win.querySelector('tf-org-tree');
  assert.ok(chart);
  const marks = Object.fromEntries(chart.model.nodes.map((n) => [n.id, n.mark]));
  assert.deepEqual(marks, { 'p-1': 'added', 'p-2': 'changed', 'p-3': 'error' });
  assert.equal(q('partial').hidden, false, 'a partial preview says so');
});

test('a complete preview does not claim to be partial, and no preview is a sentence', async () => {
  stub({ orgImportDryRunRequest: report({ preview: preview() }) });
  open();
  await chooseFile();
  assert.equal(q('partial').hidden, true);
  stub({ orgImportDryRunRequest: report({ preview: null }) });
  dispatch(q('asof'), 'change', { value: '2026-10-05' });
  await sleep(0);
  assert.equal(q('preview').textContent.trim(), it('preview_none'));
});

test('cancel closes the window', async () => {
  open();
  btn('cancel').click();
  await closed();
  assert.equal(win.isConnected, false);
});

test('a replace that awaits its confirmation is not blocked, has no error card, and opens on what it ends', async () => {
  stub({ orgImportDryRunRequest: replaceReport() });
  open();
  dispatch(q('mode'), 'change', { value: 'replace' });
  await chooseFile();
  assert.equal(btn('apply').hasAttribute('disabled'), false, 'Zapisz wszystko is open');
  assert.equal(cards().length, 0, 'the confirmation step is not a card');
  assert.equal(q('tabs').value, 'ended');
  assert.deepEqual(statValues()[2], [it('stat_errors'), '0']);
  assert.equal(win.textContent.includes('{'), false, 'no unfilled placeholder anywhere');
});

test('the confirmation window lists what ends and who loses a seat', async () => {
  ApiBinary.one = (kind, payload) => { calls.push({ kind, payload }); return Promise.resolve({ report: replaceReport() }); };
  open();
  dispatch(q('mode'), 'change', { value: 'replace' });
  await chooseFile();
  btn('apply').click();
  await sleep(0);
  const note = document.querySelector('tf-window.tf-act-window .tf-act__note').getAttribute('message');
  assert.ok(note.includes('Old unit (OLD)') && note.includes('Accountant (P-7) — Ewa Zet, Jan Nowy'), note);
});

test('a file the server never read shows its name, not a row count of zero', async () => {
  open();
  await chooseFile(file('big.csv', 900 * 1024 + 1));
  assert.equal(q('meta').textContent, 'big.csv');
});

test('an issue about the whole file names no rows', async () => {
  stub({ orgImportDryRunRequest: report({ counts: counts({ errors: 1 }), errors: [{ row: 0, rows: [], kind: 'replace_matches_nothing', message: 'x' }] }) });
  open();
  await chooseFile();
  assert.equal(cards()[0].querySelector('.org-imp-card-where').textContent, '');
});

test('the preview is drawn in units mode with the units marked by what the file does in them', async () => {
  stub({ orgImportDryRunRequest: report({ preview: preview(), counts: counts({ errors: 1 }), errors: [cycle] }) });
  open();
  await chooseFile();
  const chart = win.querySelector('tf-org-tree');
  assert.equal(chart.mode, 'units');
  assert.equal(chart.model.units.find((u) => u.id === 'u-it').mark, 'error', 'an error row in the unit wins');
});

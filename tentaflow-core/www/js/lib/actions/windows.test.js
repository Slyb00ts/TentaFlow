// =============================================================================
// File: lib/actions/windows.test.js
// Description: The assign, hand over, edit, move and confirm windows — required
//   fields and inline messages, submit / busy / server-error flow, keyboard
//   (Escape, Enter, backdrop), focus return and the toast an action ends with.
// =============================================================================

import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import { window, I18n, key, cleanBody, sleep, closed, people, agents } from './_test-setup.js';
const {
  openAssignWindow, openHandoverWindow, openEditWindow, openMoveWindow, openConfirmWindow,
} = await import('./index.js');

beforeEach(cleanBody);

const t = (k, v) => I18n.t(`actions.${k}`, v);
const submitBtn = (win) => win.querySelector('[data-act="submit"]');
const cancelBtn = (win) => win.querySelector('[data-act="cancel"]');
const errorEl = (win) => win.querySelector('.tf-act__error');
const pick = (win, id) => win.querySelector(`tf-person-picker [data-id="${id}"]`).click();
const deferred = () => {
  const d = {};
  d.promise = new Promise((resolve, reject) => { d.resolve = resolve; d.reject = reject; });
  return d;
};
const fieldErrors = (win) => [...win.querySelectorAll('.tf-act__field-error:not([hidden])')].map((e) => e.textContent);

function anchorButton() {
  const btn = document.createElement('button');
  document.body.appendChild(btn);
  return btn;
}

// --- shared behaviour ------------------------------------------------------------------------

test('a window is modal, shows the subject and names its action', () => {
  const win = openAssignWindow({ subject: 'NA-231 Import hangs', people, onSubmit: async () => {} });
  assert.equal(win.hasAttribute('modal'), true);
  assert.equal(win.querySelector('.tf-act__subject').textContent, 'NA-231 Import hangs');
  assert.equal(submitBtn(win).textContent.trim(), t('assign.submit'));
});

test('Escape closes the window and the backdrop goes with it', async () => {
  const win = openAssignWindow({ subject: 's', people, onSubmit: async () => {} });
  key(document, 'Escape');
  await closed();
  assert.equal(win.isConnected, false);
  assert.equal(document.querySelectorAll('.tf-window-backdrop').length, 0);
});

test('clicking the backdrop closes the window, Cancel too', async () => {
  const first = openAssignWindow({ subject: 's', people, onSubmit: async () => {} });
  first.previousElementSibling.click();
  await closed();
  assert.equal(first.isConnected, false);
  const second = openAssignWindow({ subject: 's', people, onSubmit: async () => {} });
  cancelBtn(second).click();
  await closed();
  assert.equal(second.isConnected, false);
});

test('focus returns to the anchor when the window closes', async () => {
  const anchor = anchorButton();
  anchor.focus();
  openAssignWindow({ subject: 's', people, anchor, onSubmit: async () => {} });
  document.querySelector('tf-window').close(true);
  await closed();
  assert.equal(document.activeElement, anchor);
});

test('while the request runs the window is busy: Escape, backdrop and buttons do nothing', async () => {
  const d = deferred();
  const win = openAssignWindow({ subject: 's', people, onSubmit: () => d.promise });
  pick(win, 'u1');
  submitBtn(win).click();
  await sleep(0);
  assert.equal(win.querySelector('.tf-act').getAttribute('aria-busy'), 'true');
  assert.equal(submitBtn(win).hasAttribute('disabled'), true);
  assert.equal(submitBtn(win).getAttribute('label'), t('saving'));
  assert.equal(cancelBtn(win).hasAttribute('disabled'), true);
  key(document, 'Escape');
  win.previousElementSibling.click();
  await closed();
  assert.equal(win.isConnected, true, 'still open');
  d.resolve();
  await closed();
  assert.equal(win.isConnected, false);
});

test('a server error stays in the window as text and the window can be submitted again', async () => {
  let attempt = 0;
  const win = openAssignWindow({
    subject: 's', people,
    onSubmit: async () => { attempt++; if (attempt === 1) throw new Error('Person is on leave'); },
  });
  pick(win, 'u1');
  submitBtn(win).click();
  await sleep(0);
  assert.equal(win.isConnected, true);
  assert.equal(errorEl(win).hidden, false);
  assert.equal(errorEl(win).getAttribute('message'), 'Person is on leave');
  assert.equal(win.querySelector('.tf-act').getAttribute('aria-busy'), 'false');
  assert.equal(submitBtn(win).hasAttribute('disabled'), false);
  submitBtn(win).click();
  await closed();
  assert.equal(win.isConnected, false);
  assert.equal(attempt, 2);
});

test('errorMessage turns a typed server error into text', async () => {
  const win = openAssignWindow({
    subject: 's', people,
    errorMessage: (err) => (err.code === 'ORG_CYCLE' ? 'This would create a cycle.' : 'other'),
    onSubmit: async () => { throw Object.assign(new Error('raw'), { code: 'ORG_CYCLE' }); },
  });
  pick(win, 'u1');
  submitBtn(win).click();
  await sleep(0);
  assert.equal(errorEl(win).getAttribute('message'), 'This would create a cycle.');
});

test('an error without a message falls back to a generic one', async () => {
  const win = openAssignWindow({ subject: 's', people, onSubmit: async () => { throw {}; } });
  pick(win, 'u1');
  submitBtn(win).click();
  await sleep(0);
  assert.equal(errorEl(win).getAttribute('message'), t('error_generic'));
});

test('a result with undo ends in an Undo toast that calls it', async () => {
  let undone = 0;
  const win = openAssignWindow({
    subject: 's', people,
    onSubmit: async () => ({ message: 'Assigned: Anna', undo: async () => { undone++; } }),
  });
  pick(win, 'u1');
  submitBtn(win).click();
  await closed();
  const toast = document.querySelector('tf-toast');
  assert.equal(toast.querySelector('.tf-toast-message').textContent, 'Assigned: Anna');
  toast.querySelector('tf-button').click();
  await sleep(0);
  assert.equal(undone, 1);
});

// --- assign ---------------------------------------------------------------------------------

test('assign: no person chosen shows a message under the list and sends nothing', async () => {
  let sent = 0;
  const win = openAssignWindow({ subject: 's', people, onSubmit: async () => { sent++; } });
  submitBtn(win).click();
  await sleep(0);
  assert.deepEqual(fieldErrors(win), [t('person_required')]);
  assert.equal(sent, 0);
  assert.equal(win.isConnected, true);
  pick(win, 'u1');
  assert.deepEqual(fieldErrors(win), [], 'choosing clears the message');
});

test('assign: submits the person, extra fields and the notify choice', async () => {
  const seen = [];
  const win = openAssignWindow({
    subject: 's', role: 'Lead developer', people, notify: true,
    extraFields: [{ key: 'share', label: 'Share', kind: 'select', options: ['25%', '50%'], value: '50%' }],
    onSubmit: async (v) => { seen.push(v); },
  });
  assert.equal(win.querySelector('.tf-act__label').textContent, 'Lead developer');
  pick(win, 'u2');
  submitBtn(win).click();
  await closed();
  assert.equal(seen[0].personId, 'u2');
  assert.equal(seen[0].person.name, 'Marek Nowak');
  assert.deepEqual(seen[0].fields, { share: '50%' });
  assert.equal(seen[0].notify, true);
});

test('assign: agents are offered only when allowed', () => {
  const without = openAssignWindow({ subject: 's', people, agents, onSubmit: async () => {} });
  assert.equal(without.querySelector('[data-id="a1"]'), null);
  cleanBody();
  const withAgents = openAssignWindow({ subject: 's', people, agents, allowAgents: true, onSubmit: async () => {} });
  assert.ok(withAgents.querySelector('[data-id="a1"]'));
});

test('assign: Enter on a chosen person submits', async () => {
  const seen = [];
  const win = openAssignWindow({ subject: 's', people, onSubmit: async (v) => { seen.push(v.personId); } });
  const list = win.querySelector('[role="listbox"]');
  list.focus();
  key(list, 'Enter');
  await closed();
  assert.deepEqual(seen, ['u4'], 'the suggested person is active first');
});

test('assign: with more fields to fill, Enter on a person moves to the first of them instead of submitting', async () => {
  const seen = [];
  const win = openAssignWindow({
    subject: 's', people,
    extraFields: [{ key: 'name', label: 'Name', kind: 'text' }, { key: 'share', label: 'Share', kind: 'select', options: ['25%'] }],
    onSubmit: async (v) => { seen.push(v); },
  });
  const input = win.querySelector('tf-input[label="Name"]');
  let focused = 0;
  input.focus = () => { focused += 1; };
  key(win.querySelector('[role="listbox"]'), 'Enter');
  await sleep(0);
  assert.equal(focused, 1, 'focus moved to the first extra field');
  assert.equal(seen.length, 0, 'nothing was submitted');
  assert.equal(win.isConnected, true);
});

// --- hand over ------------------------------------------------------------------------------

test('handover: an empty comment keeps the window open with an inline message', async () => {
  let sent = 0;
  const win = openHandoverWindow({ subject: 's', people, options: { notify: true }, onSubmit: async () => { sent++; } });
  pick(win, 'u1');
  submitBtn(win).click();
  await sleep(0);
  const comment = win.querySelector('tf-textarea');
  assert.equal(comment.getAttribute('error'), t('handover.comment_required'));
  assert.equal(win.querySelector('tf-textarea .tf-error-text').textContent, t('handover.comment_required'));
  assert.equal(sent, 0);
  assert.equal(win.isConnected, true);
  comment.value = 'MR !482 is open, tests are missing';
  comment.dispatchEvent(new window.Event('input', { bubbles: true }));
  assert.equal(comment.hasAttribute('error'), false, 'typing clears the message');
});

test('handover: a whitespace-only comment counts as empty', async () => {
  let sent = 0;
  const win = openHandoverWindow({ subject: 's', people, onSubmit: async () => { sent++; } });
  pick(win, 'u1');
  win.querySelector('tf-textarea').value = '   \n ';
  submitBtn(win).click();
  await sleep(0);
  assert.equal(sent, 0);
  assert.equal(win.querySelector('tf-textarea').hasAttribute('error'), true);
});

test('handover: both a missing person and a missing comment are marked at once', async () => {
  const win = openHandoverWindow({ subject: 's', people, onSubmit: async () => {} });
  submitBtn(win).click();
  await sleep(0);
  assert.deepEqual(fieldErrors(win), [t('person_required')]);
  assert.equal(win.querySelector('tf-textarea').hasAttribute('error'), true);
});

test('handover: submits person, trimmed comment and the chosen options', async () => {
  const seen = [];
  const win = openHandoverWindow({
    subject: 's', people, options: { stayWatcher: true, moveTimeTracking: false, notify: true },
    onSubmit: async (v) => { seen.push(v); },
  });
  const boxes = [...win.querySelectorAll('.tf-act__checks tf-checkbox')];
  assert.deepEqual(boxes.map((b) => b.getAttribute('label')),
    [t('handover.stay_watcher'), t('handover.move_time_tracking'), t('handover.notify')]);
  boxes[1].setAttribute('checked', '');
  pick(win, 'u4');
  win.querySelector('tf-textarea').value = '  Half done  ';
  submitBtn(win).click();
  await closed();
  assert.equal(seen[0].personId, 'u4');
  assert.equal(seen[0].comment, 'Half done');
  assert.deepEqual(seen[0].options, { stayWatcher: true, moveTimeTracking: true, notify: true });
});

test('handover: an option left out has no checkbox', () => {
  const win = openHandoverWindow({ subject: 's', people, options: { notify: true }, onSubmit: async () => {} });
  assert.equal(win.querySelectorAll('.tf-act__checks tf-checkbox').length, 1);
});

test('handover: Enter in the person list moves to the comment instead of submitting', async () => {
  let sent = 0;
  const win = openHandoverWindow({ subject: 's', people, onSubmit: async () => { sent++; } });
  const list = win.querySelector('[role="listbox"]');
  list.focus();
  key(list, 'Enter');
  await sleep(0);
  assert.equal(sent, 0);
  assert.equal(win.isConnected, true);
  assert.equal(document.activeElement, win.querySelector('tf-textarea textarea'));
});

// A day is typed into the field's inner input, as a user would.
function typeDay(field, text) {
  const inner = field.querySelector('input');
  inner.value = text;
  inner.dispatchEvent(new window.Event('input', { bubbles: true }));
}

// --- edit -----------------------------------------------------------------------------------

const editFields = () => [
  { key: 'title', label: 'Title', kind: 'text', value: 'Old title', required: true },
  { key: 'description', label: 'Description', kind: 'area', value: '' },
  { key: 'severity', label: 'Severity', kind: 'select', options: [{ value: 'a', label: 'Critical' }, { value: 'b', label: 'Minor' }], value: 'a' },
  { key: 'due', label: 'Due', kind: 'date', value: '2026-10-02' },
  { key: 'owner', label: 'Owner', kind: 'person', value: 'u1' },
  { key: 'share', label: 'Share', kind: 'number', value: 50, min: 0, max: 100 },
];

test('edit: renders one control per field kind with its value', () => {
  const win = openEditWindow({ subject: 's', fields: editFields(), people, onSubmit: async () => {} });
  assert.equal(win.querySelector('tf-input[label="Title *"]').value, 'Old title');
  assert.ok(win.querySelector('tf-textarea[label="Description"]'));
  assert.equal(win.querySelector('tf-select[label="Severity"]').value, 'a');
  const due = win.querySelector('tf-date-field[label="Due"]');
  assert.equal(due.value, '2026-10-02', 'the value is ISO');
  assert.equal(due.text, '02/10/2026', 'the text is in the language format');
  assert.equal(due.querySelector('.tf-date-field__pop').hidden, true, 'the calendar opens on demand');
  assert.equal(win.querySelector('tf-select[label="Owner"]').value, 'u1');
  assert.equal(win.querySelector('tf-input[label="Share"]').getAttribute('type'), 'number');
});

test('edit: a required field left empty is marked and nothing is sent', async () => {
  let sent = 0;
  const win = openEditWindow({ subject: 's', fields: editFields(), people, onSubmit: async () => { sent++; } });
  win.querySelector('tf-input[label="Title *"]').value = '  ';
  submitBtn(win).click();
  await sleep(0);
  assert.equal(win.querySelector('tf-input[label="Title *"]').getAttribute('error'), t('required_field'));
  assert.equal(sent, 0);
});

test('edit: a required select without a value starts on a placeholder and must be chosen', async () => {
  let sent = 0;
  const win = openEditWindow({
    subject: 's', people, onSubmit: async () => { sent++; },
    fields: [{ key: 'kind', label: 'Kind', kind: 'select', options: ['x', 'y'], required: true }],
  });
  assert.equal(win.querySelector('tf-select').value, '');
  submitBtn(win).click();
  await sleep(0);
  assert.deepEqual(fieldErrors(win), [t('required_field')]);
  assert.equal(sent, 0);
});

test('edit: numbers are checked against min and max and a bad one is refused', async () => {
  let sent = 0;
  const win = openEditWindow({ subject: 's', fields: editFields(), people, onSubmit: async () => { sent++; } });
  const share = win.querySelector('tf-input[label="Share"]');
  share.value = '120';
  submitBtn(win).click();
  await sleep(0);
  assert.equal(share.getAttribute('error'), t('number_max', { max: 100 }));
  share.value = '-1';
  submitBtn(win).click();
  await sleep(0);
  assert.equal(share.getAttribute('error'), t('number_min', { min: 0 }));
  assert.equal(sent, 0);
});

test('edit: submits every value and lists what changed', async () => {
  const seen = [];
  const win = openEditWindow({
    subject: 's', fields: editFields(), people,
    onSubmit: async (values, meta) => { seen.push({ values, meta }); },
  });
  win.querySelector('tf-input[label="Title *"]').value = 'New title';
  win.querySelector('tf-input[label="Share"]').value = '75,5';
  typeDay(win.querySelector('tf-date-field[label="Due"]'), '01/11/2026');
  submitBtn(win).click();
  await closed();
  assert.deepEqual(seen[0].values, {
    title: 'New title', description: '', severity: 'a', due: '2026-11-01', owner: 'u1', share: 75.5,
  });
  assert.deepEqual(seen[0].meta.changed.sort(), ['due', 'share', 'title']);
});

test('edit: a date can be picked from the calendar, cleared when optional, and a bad one is refused', async () => {
  const seen = [];
  const win = openEditWindow({
    subject: 's', people, fields: [{ key: 'due', label: 'Due', kind: 'date', value: '2026-10-02' }],
    onSubmit: async (v) => { seen.push(v.due); },
  });
  const field = win.querySelector('tf-date-field[label="Due"]');
  const pop = field.querySelector('.tf-date-field__pop');
  field.querySelector('tf-button').click();
  assert.equal(pop.hidden, false);
  pop.querySelector('tf-datepicker').dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value: '2026-10-15' } }));
  assert.equal(field.value, '2026-10-15');
  assert.equal(field.text, '15/10/2026');
  assert.equal(pop.hidden, true);

  typeDay(field, '45/13/2026');
  submitBtn(win).click();
  await sleep(0);
  assert.equal(field.querySelector('tf-input').getAttribute('error'), I18n.t('date_field.invalid', { format: 'DD/MM/YYYY' }));
  assert.equal(seen.length, 0);

  typeDay(field, '');
  submitBtn(win).click();
  await closed();
  assert.deepEqual(seen, [null]);
});

test('edit: a required date cannot be empty, out of range is refused, and typing a valid date moves the calendar', async () => {
  const win = openEditWindow({
    subject: 's', people, onSubmit: async () => {},
    fields: [{ key: 'from', label: 'From', kind: 'date', required: true, min: '2026-01-01' }],
  });
  const field = win.querySelector('tf-date-field[label="From *"]');
  const input = field.querySelector('tf-input');
  submitBtn(win).click();
  await sleep(0);
  assert.equal(field.getAttribute('error'), t('required_field'));
  typeDay(field, '31/12/2025');
  submitBtn(win).click();
  await sleep(0);
  assert.equal(input.getAttribute('error'), I18n.t('date_field.out_of_range'));
  typeDay(field, '04/03/2026');
  assert.equal(field.querySelector('tf-datepicker').value, '2026-03-04');
  assert.equal(input.hasAttribute('error'), false);
  assert.equal(field.hasAttribute('error'), false);
});

test('edit: Enter in a text field submits, Enter in the description does not', async () => {
  let sent = 0;
  const win = openEditWindow({ subject: 's', fields: editFields(), people, onSubmit: async () => { sent++; } });
  const area = win.querySelector('tf-textarea textarea');
  key(area, 'Enter');
  await sleep(0);
  assert.equal(sent, 0);
  key(area, 'Enter', { ctrlKey: true });
  await closed();
  assert.equal(sent, 1);
});

// --- move -----------------------------------------------------------------------------------

test('move: a target must be chosen', async () => {
  let sent = 0;
  const win = openMoveWindow({
    subject: 's', targets: [{ id: 'p1', label: 'Project A' }, { id: 'p2', label: 'Project B' }],
    onSubmit: async () => { sent++; },
  });
  submitBtn(win).click();
  await sleep(0);
  assert.deepEqual(fieldErrors(win), [t('move.target_required')]);
  assert.equal(sent, 0);
});

test('move: submits the chosen target', async () => {
  const seen = [];
  const win = openMoveWindow({
    subject: 's', selected: 'p1',
    targets: [{ id: 'p1', label: 'Project A' }, { id: 'p2', label: 'Project B' }, { id: 'p3', label: 'Project C', disabled: 'Ended' }],
    onSubmit: async (v) => { seen.push(v); },
  });
  const radios = [...win.querySelectorAll('tf-radio')];
  assert.equal(radios[2].hasAttribute('disabled'), true);
  assert.equal(radios[2].getAttribute('hint'), 'Ended');
  win.querySelector('tf-radio-group').value = 'p2';
  submitBtn(win).click();
  await closed();
  assert.equal(seen[0].targetId, 'p2');
  assert.equal(seen[0].target.label, 'Project B');
});

test('move: the consequence note names a target only once a different one is chosen', () => {
  const win = openMoveWindow({
    subject: 's', selected: 'p1',
    targets: [{ id: 'p1', label: 'Project A' }, { id: 'p2', label: 'Project B' }],
    noteFor: (target) => ({ tone: 'warning', text: `moves under ${target.label}` }),
    onSubmit: async () => {},
  });
  const note = win.querySelector('.tf-act__content tf-alert');
  const group = win.querySelector('tf-radio-group');
  assert.equal(note.hidden, true, 'nothing to warn about while the current target is kept');
  group.value = 'p2';
  group.dispatchEvent(new window.Event('change', { bubbles: true }));
  assert.equal(note.hidden, false);
  assert.equal(note.getAttribute('message'), 'moves under Project B');
  group.value = 'p1';
  group.dispatchEvent(new window.Event('change', { bubbles: true }));
  assert.equal(note.hidden, true);
});

test('move: with no targets there is nothing to submit', () => {
  const win = openMoveWindow({ subject: 's', targets: [], onSubmit: async () => {} });
  assert.equal(submitBtn(win).hasAttribute('disabled'), true);
  assert.equal(win.querySelector('tf-alert').getAttribute('message'), t('move.empty'));
});

test('move: a long list gets a filter that hides non-matching targets', () => {
  const targets = Array.from({ length: 12 }, (_, i) => ({ id: `t${i}`, label: i === 7 ? 'Finance' : `Unit ${i}` }));
  const win = openMoveWindow({ subject: 's', targets, onSubmit: async () => {} });
  const input = win.querySelector('tf-searchbox input');
  input.value = 'fin';
  input.dispatchEvent(new window.Event('input', { bubbles: true }));
  const shown = [...win.querySelectorAll('tf-radio')].filter((r) => !r.hasAttribute('hidden'));
  assert.deepEqual(shown.map((r) => r.getAttribute('label')), ['Finance']);
});

// --- confirm --------------------------------------------------------------------------------

test('confirm: shows the consequence and a danger button for delete', () => {
  const win = openConfirmWindow({ kind: 'delete', subject: 'Task', consequence: 'The task has history; it cannot be undone.', onSubmit: async () => {} });
  assert.equal(win.querySelector('.tf-act__note').getAttribute('message'), 'The task has history; it cannot be undone.');
  assert.equal(win.querySelector('.tf-act__note').getAttribute('tone'), 'danger');
  assert.equal(submitBtn(win).getAttribute('variant'), 'danger-solid');
  assert.equal(submitBtn(win).textContent.trim(), t('confirm.delete_submit'));
});

test('confirm: archive is a plain primary action with a warning note', () => {
  const win = openConfirmWindow({ kind: 'archive', subject: 'Task', consequence: 'Leaves the board.', onSubmit: async () => {} });
  assert.equal(win.querySelector('.tf-act__note').getAttribute('tone'), 'warning');
  assert.equal(submitBtn(win).getAttribute('variant'), 'primary');
});

test('confirm: a required reason must be given and is passed on', async () => {
  const seen = [];
  const win = openConfirmWindow({ kind: 'delete', subject: 's', consequence: 'c', requireReason: true, onSubmit: async (v) => { seen.push(v.reason); } });
  submitBtn(win).click();
  await sleep(0);
  assert.equal(seen.length, 0);
  assert.equal(win.querySelector('tf-textarea').getAttribute('error'), t('required_field'));
  win.querySelector('tf-textarea').value = 'duplicate of NA-198';
  submitBtn(win).click();
  await closed();
  assert.deepEqual(seen, ['duplicate of NA-198']);
});

test('confirm: the button stays locked until the phrase is typed exactly', async () => {
  const seen = [];
  const win = openConfirmWindow({ kind: 'delete', subject: 's', consequence: 'c', confirmPhrase: 'ProjectX', onSubmit: async () => { seen.push(1); } });
  const input = win.querySelector('tf-input');
  assert.equal(submitBtn(win).hasAttribute('disabled'), true);
  input.value = 'projectx';
  input.dispatchEvent(new window.Event('input', { bubbles: true }));
  assert.equal(submitBtn(win).hasAttribute('disabled'), true);
  submitBtn(win).click();
  await sleep(0);
  assert.equal(seen.length, 0);
  input.value = 'ProjectX';
  input.dispatchEvent(new window.Event('input', { bubbles: true }));
  assert.equal(submitBtn(win).hasAttribute('disabled'), false);
  submitBtn(win).click();
  await closed();
  assert.deepEqual(seen, [1]);
});

test('confirm: an unknown kind is rejected', () => {
  assert.throws(() => openConfirmWindow({ kind: 'explode', subject: 's', consequence: 'c', onSubmit: async () => {} }), /unknown confirm kind/);
});

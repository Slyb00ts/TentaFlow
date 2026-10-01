// =============================================================================
// File: components/tf-date-field.test.js
// Description: The date field — the day shown in the UI language's format, ISO
//   in `value`, events only for whole days, validation of typed text and range,
//   the calendar popup (open, pick, Escape) and the Intl-driven calendar.
// =============================================================================

import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import { I18n, window, key, cleanBody } from '../lib/actions/_test-setup.js';
await import('./tf-date-field.js');

beforeEach(cleanBody);

function mount(attrs = {}) {
  const field = document.createElement('tf-date-field');
  for (const [name, value] of Object.entries(attrs)) field.setAttribute(name, value);
  document.body.appendChild(field);
  return field;
}

const inner = (field) => field.querySelector('input');
function type(field, text) {
  inner(field).value = text;
  inner(field).dispatchEvent(new window.Event('input', { bubbles: true }));
}
const events = (field) => {
  const seen = [];
  field.addEventListener('change', (e) => seen.push(e.detail.value));
  return seen;
};

test('the value is ISO and the text is the day in the UI language order', () => {
  const field = mount({ value: '2026-09-30', label: 'Since' });
  assert.equal(field.value, '2026-09-30');
  assert.equal(inner(field).value, '30/09/2026');
  assert.equal(field.querySelector('tf-input').getAttribute('label'), 'Since');
  field.value = '2026-11-01';
  assert.equal(inner(field).value, '01/11/2026');
  field.value = '';
  assert.equal(inner(field).value, '');
  assert.equal(field.value, '');
});

test('a whole typed day is announced as ISO, half-typed text is not', () => {
  const field = mount();
  const seen = events(field);
  type(field, '3');
  type(field, '30/09');
  assert.deepEqual(seen, []);
  type(field, '30/09/2026');
  assert.deepEqual(seen, ['2026-09-30']);
  assert.equal(field.value, '2026-09-30');
  type(field, '');
  assert.deepEqual(seen, ['2026-09-30', '']);
});

test('text that is not a day is refused with a sentence, an out-of-range day too, and typing clears it', () => {
  const field = mount({ min: '2026-01-01', max: '2026-12-31' });
  type(field, '31/02/2026');
  assert.equal(field.value, '');
  assert.equal(field.validate(), false);
  assert.equal(field.querySelector('tf-input').getAttribute('error'), I18n.t('date_field.invalid', { format: 'DD/MM/YYYY' }));
  type(field, '31/12/2027');
  assert.equal(field.validate(), false);
  assert.equal(field.querySelector('tf-input').getAttribute('error'), I18n.t('date_field.out_of_range'));
  type(field, '15/06/2026');
  assert.equal(field.querySelector('tf-input').hasAttribute('error'), false);
  assert.equal(field.validate(), true);
  assert.equal(field.valid, true);
});

test('leaving the field writes a valid day in the canonical format', () => {
  const field = mount();
  type(field, '1/9/2026');
  inner(field).dispatchEvent(new window.Event('change', { bubbles: true }));
  assert.equal(inner(field).value, '01/09/2026');
});

test('the calendar opens from the button and from ArrowDown, picks a day and closes on Escape', () => {
  const field = mount({ value: '2026-09-30' });
  const pop = field.querySelector('.tf-date-field__pop');
  const seen = events(field);
  assert.equal(pop.hidden, true);
  field.querySelector('tf-button').click();
  assert.equal(pop.hidden, false);
  assert.equal(pop.querySelector('tf-datepicker').value, '2026-09-30');
  key(pop, 'Escape');
  assert.equal(pop.hidden, true);
  key(inner(field), 'ArrowDown');
  assert.equal(pop.hidden, false);
  pop.querySelector('.tf-dp-day[data-date="2026-09-12"]').click();
  assert.deepEqual(seen, ['2026-09-12']);
  assert.equal(inner(field).value, '12/09/2026');
  assert.equal(pop.hidden, true);
});

test('a disabled field does not open the calendar', () => {
  const field = mount({ disabled: '' });
  field.querySelector('tf-button').click();
  assert.equal(field.querySelector('.tf-date-field__pop').hidden, true);
});

test('the calendar starts the week on the language’s first day and names days and months with Intl', () => {
  const field = mount({ value: '2026-09-30' });
  field.querySelector('tf-button').click();
  const heads = [...document.querySelectorAll('.tf-dp-wday')].map((n) => n.textContent);
  assert.equal(heads.length, 7);
  assert.match(heads[0], /^Mon/i);
  assert.match(document.querySelector('.tf-dp-header span').textContent, /September 2026/);
  // 1 September 2026 is a Tuesday: one padding day (Monday 31 August) leads the grid.
  assert.equal(document.querySelector('.tf-dp-day').dataset.date, '2026-08-31');
});

test('arrow keys move between days and PageDown to the next month', () => {
  const field = mount({ value: '2026-09-30' });
  field.querySelector('tf-button').click();
  const cal = document.querySelector('tf-datepicker');
  key(cal.querySelector('.tf-dp-day[data-date="2026-09-30"]'), 'ArrowRight');
  assert.equal(document.activeElement.dataset.date, '2026-10-01');
  key(document.activeElement, 'PageDown');
  assert.equal(document.activeElement.dataset.date, '2026-11-01');
});

test('the open calendar lives outside the field, so a window around it cannot clip it, and goes back when closed', () => {
  const field = mount({ value: '2026-09-30' });
  const pop = field.querySelector('.tf-date-field__pop');
  field.querySelector('tf-button').click();
  assert.equal(pop.parentElement, document.body);
  key(pop, 'Escape');
  assert.equal(pop.parentElement, field);
});

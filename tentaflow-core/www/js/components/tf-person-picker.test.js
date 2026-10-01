// =============================================================================
// File: components/tf-person-picker.test.js
// Description: The person picker — order (suggested first, agents after people),
//   tags and load badges, search, single and multiple selection, keyboard,
//   ARIA listbox and the empty state.
// =============================================================================

import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import { I18n, window, key, cleanBody, people, agents } from '../lib/actions/_test-setup.js';
await import('./tf-person-picker.js');

beforeEach(cleanBody);

function mount({ items = people, multiple = false } = {}) {
  const picker = document.createElement('tf-person-picker');
  if (multiple) picker.setAttribute('multiple', '');
  picker.items = items;
  document.body.appendChild(picker);
  return picker;
}

const names = (picker) => [...picker.querySelectorAll('.tf-pp__name')].map((n) => n.textContent);
const search = (picker) => picker.querySelector('tf-searchbox input');
const list = (picker) => picker.querySelector('[role="listbox"]');
const type = (picker, text) => {
  const input = search(picker);
  input.value = text;
  input.dispatchEvent(new window.Event('input', { bubbles: true }));
};
const activeName = (picker) => picker.querySelector('.is-active .tf-pp__name')?.textContent;

test('suggested people come first, the rest keep the caller order', () => {
  assert.deepEqual(names(mount()), ['Paweł Szymański', 'Anna Kowalska', 'Marek Nowak', 'Piotr Zieliński', 'Ewa Wiśniewska']);
});

test('agents are listed after the people in their own group', () => {
  const picker = mount({ items: [...agents.map((a) => ({ ...a, kind: 'agent' })), ...people] });
  const order = names(picker);
  assert.equal(order[order.length - 1], 'Coding agent');
  assert.deepEqual([...picker.querySelectorAll('[role="group"]')].map((g) => g.getAttribute('aria-label')),
    [I18n.t('actions.picker.group_people'), I18n.t('actions.picker.group_agents')]);
  assert.equal(picker.querySelector('.tf-pp__agent-icon') !== null, true);
});

test('a row shows initials, function, suggestion, absence and overload tags', () => {
  const picker = mount();
  const row = (id) => picker.querySelector(`[data-id="${id}"]`);
  assert.equal(row('u1').querySelector('tf-avatar').getAttribute('initials'), 'AK');
  assert.equal(row('u2').querySelector('.tf-pp__role').textContent, 'Developer');
  assert.equal(row('u4').querySelector('tf-chip').getAttribute('label'), 'module deputy');
  assert.match(row('u5').querySelector('tf-chip').getAttribute('label'), /away until 24\.10, covered by Paweł S\./);
  assert.equal(row('u3').querySelector('tf-chip').getAttribute('label'), I18n.t('actions.picker.overloaded'));
  assert.equal(row('u1').querySelector('tf-chip'), null);
});

test('the load badge warns from 90 percent and turns critical above 100', () => {
  const picker = mount();
  const badge = (id) => picker.querySelector(`[data-id="${id}"] tf-badge`);
  assert.equal(badge('u1').getAttribute('tone'), 'neutral');
  assert.equal(badge('u2').getAttribute('tone'), 'warning');
  assert.equal(badge('u3').getAttribute('tone'), 'danger');
  assert.equal(badge('u3').getAttribute('value'), '130%');
});

test('search ignores case and diacritics and matches function and tags', () => {
  const picker = mount();
  type(picker, 'zielinski');
  assert.deepEqual(names(picker), ['Piotr Zieliński']);
  type(picker, 'tester');
  assert.deepEqual(names(picker), ['Ewa Wiśniewska']);
  type(picker, 'deputy');
  assert.deepEqual(names(picker), ['Paweł Szymański']);
});

test('nothing found shows the empty state and hides the list', () => {
  const picker = mount();
  type(picker, 'zzz');
  assert.equal(picker.querySelector('tf-empty-state').hidden, false);
  assert.equal(list(picker).hidden, true);
  type(picker, '');
  assert.equal(picker.querySelector('tf-empty-state').hidden, true);
});

test('single selection replaces the choice and reports it', () => {
  const picker = mount();
  const seen = [];
  picker.addEventListener('change', (e) => seen.push(e.detail.value));
  picker.querySelector('[data-id="u1"]').click();
  picker.querySelector('[data-id="u2"]').click();
  assert.deepEqual(seen, ['u1', 'u2']);
  assert.equal(picker.value, 'u2');
  assert.equal(picker.querySelectorAll('[aria-selected="true"]').length, 1);
});

test('multiple selection toggles and returns the ids', () => {
  const picker = mount({ multiple: true });
  picker.querySelector('[data-id="u1"]').click();
  picker.querySelector('[data-id="u2"]').click();
  picker.querySelector('[data-id="u1"]').click();
  assert.deepEqual(picker.value, ['u2']);
  assert.equal(list(picker).getAttribute('aria-multiselectable'), 'true');
});

test('the listbox is an ARIA listbox of options with an active descendant', () => {
  const picker = mount();
  assert.equal(list(picker).getAttribute('role'), 'listbox');
  const options = picker.querySelectorAll('[role="option"]');
  assert.equal(options.length, 5);
  assert.equal(list(picker).getAttribute('aria-activedescendant'), picker.querySelector('.is-active').id);
});

test('ArrowDown leaves the search box for the list and the arrows move the active row', () => {
  const picker = mount();
  const input = search(picker);
  input.focus();
  key(input, 'ArrowDown');
  assert.equal(document.activeElement, list(picker));
  assert.equal(activeName(picker), 'Paweł Szymański');
  key(list(picker), 'ArrowDown');
  assert.equal(activeName(picker), 'Anna Kowalska');
  key(list(picker), 'End');
  assert.equal(activeName(picker), 'Ewa Wiśniewska');
  key(list(picker), 'Home');
  assert.equal(activeName(picker), 'Paweł Szymański');
  key(list(picker), 'ArrowUp');
  assert.equal(document.activeElement, search(picker), 'Up from the first row returns to search');
});

test('Space selects the active row, Enter selects and activates', () => {
  const picker = mount();
  const activated = [];
  picker.addEventListener('activate', (e) => activated.push(e.detail.item.id));
  list(picker).focus();
  key(list(picker), 'ArrowDown');
  key(list(picker), ' ');
  assert.equal(picker.value, 'u1');
  assert.deepEqual(activated, []);
  key(list(picker), 'Enter');
  assert.deepEqual(activated, ['u1']);
});

test('in multiple mode Space toggles and Enter does not undo a choice', () => {
  const picker = mount({ multiple: true });
  list(picker).focus();
  key(list(picker), ' ');
  assert.deepEqual(picker.value, ['u4']);
  key(list(picker), 'Enter');
  assert.deepEqual(picker.value, ['u4']);
  key(list(picker), ' ');
  assert.deepEqual(picker.value, []);
});

test('a disabled person is skipped by the arrows and cannot be chosen', () => {
  const picker = mount({ items: [{ id: 'x', name: 'Away Person', disabled: 'already assigned' }, { id: 'y', name: 'Other Person' }] });
  assert.equal(activeName(picker), 'Other Person');
  picker.querySelector('[data-id="x"]').click();
  assert.equal(picker.value, null);
  assert.equal(picker.querySelector('[data-id="x"]').getAttribute('aria-disabled'), 'true');
  assert.equal(picker.querySelector('[data-id="x"]').getAttribute('title'), 'already assigned');
});

test('a name that looks like markup stays text', () => {
  const picker = mount({ items: [{ id: 'm', name: '<img src=x onerror=alert(1)>', role: '<b>x</b>' }] });
  assert.equal(picker.querySelector('img'), null);
  assert.equal(picker.querySelector('.tf-pp__name').textContent, '<img src=x onerror=alert(1)>');
});

test('setting value selects known ids only', () => {
  const picker = mount({ multiple: true });
  picker.value = ['u1', 'nope'];
  assert.deepEqual(picker.value, ['u1']);
});

// =============================================================================
// File: components/tf-radio-card.test.js
// Description: The card variant of <tf-radio> after it learned to host a
// control of its own (the account picker inside "Konto globalne", G01).
//
// Two properties have to hold at once: picking the option still works by
// pointer and by keyboard, and operating a control INSIDE the card neither
// changes the selection nor swallows the control's own default behaviour —
// which is what a <label> wrapper used to do, disabling the whole card along
// with the select it adopted.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { window } = await import('../sdk-runtime/_dom-test-harness.js');
if (typeof globalThis.MutationObserver !== 'function' && window.MutationObserver) {
  globalThis.MutationObserver = window.MutationObserver;
}
await import('./tf-radio.js');

/** A two-card group whose first card carries a select, like the agent screen. */
function mountGroup(value = 'user') {
  const group = document.createElement('tf-radio-group');
  group.setAttribute('cards', '');
  group.setAttribute('name', 'account-mode');
  group.setAttribute('value', value);
  group.innerHTML = `
    <tf-radio card value="global">
      <div class="title">Konto globalne</div>
      <select data-account><option value="a1">Claude Code — firma</option></select>
    </tf-radio>
    <tf-radio card value="user"><div class="title">Konto użytkownika</div></tf-radio>`;
  document.body.appendChild(group);
  return group;
}

const cards = (group) => [...group.querySelectorAll('.tf-radio-card-group__card')];

test('a card is a plain box, so a disabled control inside it cannot disable the option', () => {
  const group = mountGroup();
  const card = cards(group)[0];
  assert.equal(card.tagName, 'DIV');
  const select = card.querySelector('select');
  select.disabled = true;
  // A <label> would report the select as the control it labels; a box has none.
  assert.equal(card.control ?? null, null);
});

test('clicking the card selects the option and emits one change', () => {
  const group = mountGroup('user');
  const seen = [];
  group.addEventListener('change', (e) => seen.push(e.detail.value));
  cards(group)[0].click();
  assert.equal(group.value, 'global');
  assert.deepEqual(seen, ['global']);
  assert.ok(cards(group)[0].classList.contains('tf-radio-card-group__card--selected'));
});

test('Space and Enter on the card select it the same way a click does', () => {
  for (const key of [' ', 'Enter']) {
    document.body.innerHTML = '';
    const group = mountGroup('user');
    const seen = [];
    group.addEventListener('change', (e) => seen.push(e.detail.value));
    const input = cards(group)[0].querySelector('.tf-radio-card-group__input');
    const event = new window.KeyboardEvent('keydown', { key, bubbles: true, cancelable: true });
    input.dispatchEvent(event);
    assert.equal(group.value, 'global', `${key} selects the card`);
    assert.deepEqual(seen, ['global']);
    assert.ok(event.defaultPrevented, `${key} on the card is consumed by the option`);
  }
});

test('clicking the select inside a card neither changes the option nor is prevented', () => {
  const group = mountGroup('global');
  const seen = [];
  group.addEventListener('change', (e) => seen.push(e.detail.value));
  const select = cards(group)[0].querySelector('select');
  const event = new window.MouseEvent('click', { bubbles: true, cancelable: true });
  select.dispatchEvent(event);
  assert.equal(group.value, 'global');
  assert.deepEqual(seen, [], 'the group did not re-select itself');
  assert.equal(event.defaultPrevented, false, 'the dropdown may still open');
});

test('Space on the select inside an UNSELECTED card opens the dropdown instead of flipping the card', () => {
  const group = mountGroup('user');
  const seen = [];
  group.addEventListener('change', (e) => seen.push(e.detail.value));
  const select = cards(group)[0].querySelector('select');
  const event = new window.KeyboardEvent('keydown', { key: ' ', bubbles: true, cancelable: true });
  select.dispatchEvent(event);
  assert.equal(group.value, 'user', 'the option was not selected by typing into its control');
  assert.deepEqual(seen, []);
  assert.equal(event.defaultPrevented, false, 'the browser still gets the key');
});

test('a disabled card ignores both a click and a key', () => {
  const group = mountGroup('user');
  const radio = group.querySelector('tf-radio[value="global"]');
  radio.setAttribute('disabled', '');
  cards(group)[0].click();
  const input = cards(group)[0].querySelector('.tf-radio-card-group__input');
  input.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
  assert.equal(group.value, 'user');
  assert.ok(cards(group)[0].classList.contains('tf-radio-card-group__card--disabled'));
});

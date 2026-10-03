// =============================================================================
// File: components/tf-select.prefix.test.js
// Description: tf-select `prefix` and `dot` — the caption and state dot shown
// inside the field before the chosen value (the TentaBus instance picker:
// "● INSTANCJA Produkcja"). The caption is the select's accessible name, the
// dot follows its tone, and a select without either keeps its old markup.
// `hint` puts one explanatory line under the field, like tf-input's.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { window } = await import('../sdk-runtime/_dom-test-harness.js');
if (typeof globalThis.MutationObserver !== 'function' && window.MutationObserver) {
  globalThis.MutationObserver = window.MutationObserver;
}
const { TfSelect } = await import('./tf-select.js');

function mount(attrs = {}) {
  const el = new TfSelect();
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
  document.body.appendChild(el);
  el.setOptions([{ value: 'a', label: 'Produkcja' }, { value: 'b', label: 'Szkolenia' }], 'a');
  return el;
}

test('prefix renders inside the field and names the select', () => {
  const el = mount({ prefix: 'Instancja' });
  const wrap = el.querySelector('.tf-select-wrap');
  assert.ok(wrap.classList.contains('tf-select-wrap--prefix'));
  assert.equal(el.querySelector('.tf-select-prefix').textContent, 'Instancja');
  assert.equal(el.querySelector('select').getAttribute('aria-label'), 'Instancja');
});

test('dot follows its tone; an unknown tone draws none', () => {
  const el = mount({ prefix: 'Instancja', dot: 'ok' });
  assert.ok(el.querySelector('.tf-select-dot.tf-select-dot--ok'));
  el.setAttribute('dot', 'err');
  assert.ok(el.querySelector('.tf-select-dot--err'));
  el.setAttribute('dot', 'bogus');
  assert.equal(el.querySelector('.tf-select-dot'), null);
});

test('hint shows one line under the field and goes away with the attribute', () => {
  const el = mount({ hint: 'Starsze wiadomości są usuwane.' });
  const hint = el.querySelector('.tf-hint');
  assert.equal(hint.textContent, 'Starsze wiadomości są usuwane.');
  assert.equal(hint.style.display, '');
  el.removeAttribute('hint');
  assert.equal(hint.style.display, 'none');
});

test('without prefix or dot the field is unchanged', () => {
  const el = mount();
  assert.equal(el.querySelector('.tf-select-wrap').classList.contains('tf-select-wrap--prefix'), false);
  assert.equal(el.querySelector('.tf-select-prefix').textContent, '');
  assert.equal(el.querySelector('select').hasAttribute('aria-label'), false);
  el.setAttribute('prefix', 'Instancja');
  el.removeAttribute('prefix');
  assert.equal(el.querySelector('select').hasAttribute('aria-label'), false, 'the caption name goes with the caption');
});

test('wrapped caption follows the real native selection, options, and disabled state', async () => {
  const longName = 'Customer approval and archival notification '.repeat(7) + '· Message_CustomerApproval';
  const el = mount({ 'wrap-selected': '', label: 'Message declaration' });
  el.setOptions([{ value: 'long', label: longName }, { value: 'short', label: 'Validation error' }], 'long');
  const native = el.querySelector('select');
  const caption = el.querySelector('.tf-select-selected');
  assert.equal(el.querySelector('.tf-select-wrap').classList.contains('tf-select-wrap--wrap-selected'), true);
  assert.equal(caption.getAttribute('aria-hidden'), 'true');
  assert.equal(caption.textContent, longName);
  assert.equal(native.value, 'long');
  assert.equal(native.getAttribute('aria-label'), 'Message declaration');
  el.focus();
  assert.equal(document.activeElement, native);

  const changes = [];
  el.addEventListener('change', (event) => changes.push(event.detail.value));
  native.value = 'short';
  native.dispatchEvent(new Event('change', { bubbles: true }));
  assert.deepEqual(changes, ['short']);
  assert.equal(el.value, 'short');
  assert.equal(caption.textContent, 'Validation error');

  el.setOptions([{ value: 'new', label: 'Changed declaration · Message_New' }], 'new');
  assert.equal(caption.textContent, 'Changed declaration · Message_New');
  const appended = document.createElement('option');
  appended.value = 'async';
  appended.textContent = 'Asynchronously loaded declaration · Message_Async';
  el.append(appended);
  await new Promise((resolve) => setTimeout(resolve, 0));
  el.value = 'async';
  assert.equal(native.value, 'async');
  assert.equal(caption.textContent, appended.textContent);

  el.setAttribute('disabled', '');
  assert.equal(native.disabled, true);
  el.removeAttribute('wrap-selected');
  assert.equal(el.querySelector('.tf-select-wrap').classList.contains('tf-select-wrap--wrap-selected'), false);
  assert.equal(native.value, 'async');
});

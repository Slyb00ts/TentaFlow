// =============================================================================
// File: components/tf-select.prefix.test.js
// Description: tf-select `prefix` and `dot` — the caption and state dot shown
// inside the field before the chosen value (the TentaBus instance picker:
// "● INSTANCJA Produkcja"). The caption is the select's accessible name, the
// dot follows its tone, and a select without either keeps its old markup.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

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

test('without prefix or dot the field is unchanged', () => {
  const el = mount();
  assert.equal(el.querySelector('.tf-select-wrap').classList.contains('tf-select-wrap--prefix'), false);
  assert.equal(el.querySelector('.tf-select-prefix').textContent, '');
  assert.equal(el.querySelector('select').hasAttribute('aria-label'), false);
  el.setAttribute('prefix', 'Instancja');
  el.removeAttribute('prefix');
  assert.equal(el.querySelector('select').hasAttribute('aria-label'), false, 'the caption name goes with the caption');
});

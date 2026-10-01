// =============================================================================
// File: lib/actions/i18n-parity.test.js
// Description: The `actions` namespace has the same keys in all five locales,
//   the same placeholders, real translations — and every key the code asks for
//   exists.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const WWW = join(here, '..', '..', '..');
const LOCALES = ['pl', 'en', 'de', 'es', 'fr'];
const bundles = Object.fromEntries(LOCALES.map((l) => [l, JSON.parse(readFileSync(join(WWW, 'i18n', `${l}.json`), 'utf8')).actions]));

function flatten(node, prefix = '') {
  return Object.entries(node).flatMap(([k, v]) => (typeof v === 'object' ? flatten(v, `${prefix}${k}.`) : [[`${prefix}${k}`, v]]));
}
const flat = Object.fromEntries(LOCALES.map((l) => [l, Object.fromEntries(flatten(bundles[l]))]));
const placeholders = (s) => [...s.matchAll(/\{(\w+)(?:\|[^}]*)?\}/g)].map((m) => m[1]).sort();

test('every locale has the same keys as Polish', () => {
  assert.ok(Object.keys(flat.pl).length > 40);
  for (const l of LOCALES) assert.deepEqual(Object.keys(flat[l]).sort(), Object.keys(flat.pl).sort(), l);
});

test('every value is filled and keeps the placeholders of the Polish text', () => {
  for (const l of LOCALES) {
    for (const [k, v] of Object.entries(flat[l])) {
      assert.ok(v.trim().length > 0, `${l}.${k} is blank`);
      assert.deepEqual(placeholders(v), placeholders(flat.pl[k]), `${l}.${k}`);
    }
  }
});

test('counts use plural forms: Polish three, the others two', () => {
  const forms = (l, k) => flat[l][k].match(/\{count\|([^}]*)\}/)[1].split('|').length;
  for (const k of ['bulk.selected', 'bulk.subject']) {
    assert.equal(forms('pl', k), 3, k);
    for (const l of ['en', 'de', 'es', 'fr']) assert.equal(forms(l, k), 2, `${l}.${k}`);
  }
});

test('the long sentences are translated, not copied from English', () => {
  for (const k of ['handover.note', 'handover.comment_required', 'handover.comment_placeholder', 'move.empty', 'error_generic']) {
    for (const l of ['pl', 'de', 'es', 'fr']) assert.notEqual(flat[l][k], flat.en[k], `${l}.${k}`);
  }
});

test('every key the code asks for exists', () => {
  const missing = [];
  for (const file of readdirSync(here).filter((f) => f.endsWith('.js') && !f.endsWith('.test.js') && !f.startsWith('_'))) {
    for (const call of readFileSync(join(here, file), 'utf8').matchAll(/\bt\(([^)]*)\)/g)) {
      for (const [, key] of call[1].matchAll(/'([a-z_]+(?:\.[a-z_]+)*)'/g)) {
        if (!(key in flat.pl)) missing.push(`${file}: ${key}`);
      }
    }
  }
  const picker = readFileSync(join(WWW, 'js', 'components', 'tf-person-picker.js'), 'utf8');
  for (const [, key] of picker.matchAll(/\bt\('([a-z_]+)'/g)) {
    if (!(`picker.${key}` in flat.pl)) missing.push(`tf-person-picker.js: ${key}`);
  }
  assert.deepEqual(missing, []);
});

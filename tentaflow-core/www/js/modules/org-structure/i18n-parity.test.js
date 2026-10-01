// =============================================================================
// File: modules/org-structure/i18n-parity.test.js
// Description: The `org_structure` namespace and `nav.org_structure` have the
// same key set in all five locales, every value is non-blank with the Polish
// placeholders, and no locale is a copy of the English text.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const WWW_ROOT = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '..');
const LOCALES = ['pl', 'en', 'de', 'es', 'fr'];
const bundles = Object.fromEntries(LOCALES.map((l) => [l, JSON.parse(readFileSync(join(WWW_ROOT, 'i18n', `${l}.json`), 'utf8'))]));
// Nested groups (`list`, `import`) are compared key by key under their dotted path.
function flatten(object, prefix = '') {
  return Object.entries(object ?? {}).flatMap(([key, value]) => (value && typeof value === 'object'
    ? flatten(value, `${prefix}${key}.`)
    : [[`${prefix}${key}`, value]]));
}
const entries = (l) => flatten(bundles[l].org_structure);
const placeholders = (s) => [...String(s).matchAll(/\{([a-zA-Z0-9_]+)(?:\|[^}]*)?\}/g)].map((m) => m[1]).sort();

const reference = Object.fromEntries(entries('pl'));

test('every locale has the same org_structure keys as Polish', () => {
  assert.ok(Object.keys(reference).length > 0);
  for (const l of LOCALES) {
    assert.deepEqual(entries(l).map(([k]) => k).sort(), Object.keys(reference).sort(), l);
  }
});

test('every value is non-blank and keeps the Polish placeholders', () => {
  for (const l of LOCALES) {
    for (const [k, v] of entries(l)) {
      assert.equal(typeof v, 'string', `${k} in ${l}`);
      assert.ok(v.trim().length > 0, `${k} in ${l} is blank`);
      assert.deepEqual(placeholders(v), placeholders(reference[k]), `${k} in ${l}`);
    }
  }
});

test('the navigation entry exists in every locale', () => {
  for (const l of LOCALES) assert.ok(bundles[l].nav.org_structure?.trim(), l);
});

test('de, es and fr are translations, not copies of the English text', () => {
  const en = Object.fromEntries(entries('en'));
  for (const l of ['de', 'es', 'fr']) {
    const same = entries(l).filter(([k, v]) => v === en[k] && !/^(col_code|stat_units)$/.test(k) && v.length > 12);
    assert.deepEqual(same.map(([k]) => k), [], `${l} repeats English for these keys`);
  }
});

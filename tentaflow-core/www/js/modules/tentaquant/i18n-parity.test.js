// =============================================================================
// File: modules/tentaquant/i18n-parity.test.js
// Description: The WHOLE `tentaquant` namespace must have an identical key set
// across the five locales and interpolation placeholders matching the Polish
// source, so a missing translation never surfaces as a raw key. The apps-home
// tile keys the screen owns outside the namespace are checked one by one.
// =============================================================================

import { WWW_ROOT } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

const LOCALES = ['pl', 'en', 'de', 'es', 'fr'];
const NAMESPACE = 'tentaquant';
const SINGLE_KEYS = [
  'apps.tentaquant.name',
  'apps.tentaquant.desc',
];

const bundles = Object.fromEntries(LOCALES.map((l) => [l, JSON.parse(readFileSync(join(WWW_ROOT, 'i18n', `${l}.json`), 'utf8'))]));
const dig = (obj, path) => path.split('.').reduce((o, k) => (o && typeof o === 'object' ? o[k] : undefined), obj);
const flatten = (obj, prefix = '') => Object.entries(obj).flatMap(([k, v]) => (v && typeof v === 'object' ? flatten(v, `${prefix}${k}.`) : [`${prefix}${k}`]));
// `{n}` style placeholders and the `{n|one|few|many}` plural selector both name the parameter first.
const placeholders = (s) => [...String(s).matchAll(/\{([a-zA-Z0-9_]+)(?:\|[^}]*)?\}/g)].map((m) => m[1]).sort();

const reference = flatten(dig(bundles.pl, NAMESPACE)).sort();

test('the whole tentaquant namespace has the same key set in all five locales', () => {
  assert.ok(reference.length > 0, 'the pl namespace is not empty');
  for (const l of LOCALES) {
    const keys = flatten(dig(bundles[l], NAMESPACE) || {}).sort();
    assert.deepEqual(keys, reference, `${NAMESPACE} keys in ${l} match pl`);
  }
});

test('every tentaquant value is a non-empty string in all five locales', () => {
  for (const key of reference) {
    for (const l of LOCALES) {
      const v = dig(bundles[l], `${NAMESPACE}.${key}`);
      assert.equal(typeof v, 'string', `${NAMESPACE}.${key} in ${l}`);
      assert.ok(v.trim().length > 0, `${NAMESPACE}.${key} in ${l} is not blank`);
    }
  }
});

test('the apps-home tile keys exist everywhere and are non-empty strings', () => {
  for (const key of SINGLE_KEYS) {
    for (const l of LOCALES) {
      const v = dig(bundles[l], key);
      assert.equal(typeof v, 'string', `${key} in ${l}`);
      assert.ok(v.trim().length > 0, `${key} in ${l} is not blank`);
    }
  }
});

test('interpolation placeholders match the Polish source in every locale', () => {
  for (const key of reference) {
    const full = `${NAMESPACE}.${key}`;
    const expected = placeholders(dig(bundles.pl, full));
    for (const l of LOCALES) {
      assert.deepEqual(placeholders(dig(bundles[l], full)), expected, `${full} placeholders in ${l}`);
    }
  }
});

// Polish needs three plural forms; the other four locales need two. A form list
// short of that renders the wrong word for "2 projekty" or "5 projektów", which
// is exactly what rule 8 forbids fixing by concatenation.
test('every plural selector carries enough forms for its locale', () => {
  const selectors = (s) => [...String(s).matchAll(/\{[a-zA-Z0-9_]+\|([^}]*)\}/g)].map((m) => m[1].split('|'));
  for (const key of reference) {
    for (const l of LOCALES) {
      const value = dig(bundles[l], `${NAMESPACE}.${key}`);
      for (const forms of selectors(value)) {
        const need = l === 'pl' ? 3 : 2;
        assert.ok(forms.length >= need, `${NAMESPACE}.${key} in ${l} has ${forms.length} plural forms, needs ${need}`);
      }
    }
  }
});

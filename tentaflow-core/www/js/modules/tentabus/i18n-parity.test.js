// =============================================================================
// File: modules/tentabus/i18n-parity.test.js
// Description: The whole `tentabus` namespace has the same key set in all five
// locales with non-empty values and the Polish placeholders, and no value
// uses the words the owner banned from the screen: "DLQ" (it is
// "Nieprzetworzone wiadomości"), "węzeł" (it is "node"), the technical names
// of consumer groups / field policies / the schema registry, or markers of
// what is new or coming.
// =============================================================================

import { WWW_ROOT } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

const LOCALES = ['pl', 'en', 'de', 'es', 'fr'];
const NAMESPACE = 'tentabus';

const bundles = Object.fromEntries(LOCALES.map((l) => [l, JSON.parse(readFileSync(join(WWW_ROOT, 'i18n', `${l}.json`), 'utf8'))]));
const flatten = (obj, prefix = '') => Object.entries(obj).flatMap(([k, v]) => (v && typeof v === 'object' ? flatten(v, `${prefix}${k}.`) : [[`${prefix}${k}`, v]]));
const entries = (l) => flatten(bundles[l][NAMESPACE] || {});
const placeholders = (s) => [...String(s).matchAll(/\{([a-zA-Z0-9_]+)(?:\|[^}]*)?\}/g)].map((m) => m[1]).sort();

const reference = entries('pl');
const refKeys = reference.map(([k]) => k).sort();

test('the tentabus namespace has the same key set in all five locales', () => {
  assert.ok(refKeys.length > 0);
  for (const l of LOCALES) {
    assert.deepEqual(entries(l).map(([k]) => k).sort(), refKeys, `${NAMESPACE} keys in ${l} match pl`);
  }
});

test('every value is a non-empty string with the Polish placeholders', () => {
  const pl = Object.fromEntries(reference);
  for (const l of LOCALES) {
    for (const [k, v] of entries(l)) {
      assert.equal(typeof v, 'string', `${k} in ${l}`);
      assert.ok(v.trim().length > 0, `${k} in ${l} is not blank`);
      assert.deepEqual([...new Set(placeholders(v))], [...new Set(placeholders(pl[k]))], `${k} in ${l} keeps the placeholders`);
    }
  }
});

test('a Polish plural offers the three forms', () => {
  for (const [k, v] of reference) {
    for (const m of v.matchAll(/\{[a-zA-Z0-9_]+\|([^}]*)\}/g)) {
      assert.equal(m[1].split('|').length, 3, `${k}: ${m[0]}`);
    }
  }
});

// "DLQ" is "Nieprzetworzone wiadomości"; milestone and plan codes (M1–M5,
// F1–F7, "PLAN §…") and internal API talk are developer wording.
const FORBIDDEN_ALL = [/\bDLQ\b/, /\b[MF][1-7][a-z]?\b/, /PLAN §/, /\bendpoint/i, /Sync Ledger/, /\bbus\.leader\./];
const FORBIDDEN_PL = [
  /węz(eł|ł|le|ły|łów|łem|łach)/i,
  /grup\w* konsument/i,
  /polityk\w* pól/i,
  /rejestr\w* schemat/i,
  /\bNOWE\b/,
  /wkrótce/i,
  /heartbeat/i,
  /\bfollower/i,
  // Broker jargon the screen says in plain words: "czeka", "numer
  // wiadomości", "node prowadzący", "zgodne kopie", "zapis na dysk",
  // "kiedy potwierdza", and never a raw permission id.
  /\blag/i,
  /offset/i,
  /\bleader/i,
  /\bISR\b/,
  /fsync/i,
  /\bcommit/i,
  /\(in\)/,
  /endpoint/i,
  /\bbus\.[a-z_]+/,
  /\backs\b/i,
  /BlobRef/,
  /watermark/i,
];

test('no value says "DLQ", a milestone code or internal API talk in any locale', () => {
  for (const l of LOCALES) {
    for (const [k, v] of entries(l)) {
      for (const re of FORBIDDEN_ALL) assert.doesNotMatch(v, re, `${NAMESPACE}.${k} in ${l}`);
    }
  }
});

test('no Polish value uses a banned word', () => {
  for (const [k, v] of reference) {
    for (const re of FORBIDDEN_PL) assert.doesNotMatch(v, re, `${NAMESPACE}.${k}: "${v}"`);
  }
});

// =============================================================================
// File: modules/access-keys-scopes.test.js — schema-registry scope ids and the
//       "Wzory wiadomości" strings of the access-keys screen
// =============================================================================
// The scope id the dashboard sends must be byte-for-byte the id the REST gate
// rebuilds (`sync::resource_id::composite_resource_id` in Rust), or a granted
// key is refused forever with nothing on screen to explain why. And every
// `access_keys.bus_schema_*` key the screen asks for has to exist in all five
// locales — a missing one would print the raw key path.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  busSchemaScopeId,
  parseBusSchemaScopeId,
  scopeKey,
  busSchemaScopeNames,
} from './access-keys-scopes.js';

const HERE = dirname(fileURLToPath(import.meta.url));
const WWW_ROOT = resolve(HERE, '..', '..');
const LOCALES = ['pl', 'en', 'de', 'es', 'fr'];
const US = '\u001f';

test('scope id matches the Rust composite encoding', () => {
  // Same vector as `sync::resource_id::tests::composite_uses_length_prefixed_segments`
  // shape: `<len><US><part>` per part.
  assert.equal(
    busSchemaScopeId('tentabus-aaaaaaaa', 'org-1'),
    `17${US}tentabus-aaaaaaaa5${US}org-1`,
  );
});

test('lengths are UTF-8 bytes, not UTF-16 code units', () => {
  // "ż" is one code unit in JS but two bytes in UTF-8, as Rust counts it.
  assert.equal(busSchemaScopeId('tentabus-aaaaaaaa', 'żółw'), `17${US}tentabus-aaaaaaaa7${US}żółw`);
});

test('parse is the exact inverse and refuses anything else', () => {
  for (const [inst, org] of [['tentabus-aaaaaaaa', 'org-1'], ['tentabus-0000ffff', 'zażółć']]) {
    assert.deepEqual(parseBusSchemaScopeId(busSchemaScopeId(inst, org)), { instanceId: inst, orgId: org });
  }
  const good = busSchemaScopeId('tentabus-aaaaaaaa', 'org-1');
  assert.equal(parseBusSchemaScopeId(`${good}x`), null, 'trailing bytes');
  assert.equal(parseBusSchemaScopeId(good.slice(0, -1)), null, 'truncated');
  assert.equal(parseBusSchemaScopeId(`17${US}tentabus-aaaaaaaa`), null, 'one part only');
  assert.equal(parseBusSchemaScopeId('gpt-4o'), null, 'not a composite at all');
  assert.equal(parseBusSchemaScopeId(''), null);
  assert.equal(parseBusSchemaScopeId(undefined), null);
});

test('read and write grants on one scope are different keys', () => {
  const id = busSchemaScopeId('tentabus-aaaaaaaa', 'org-1');
  assert.notEqual(scopeKey('bus_schema_registry', id, 'read'), scopeKey('bus_schema_registry', id, 'write'));
  // Action-blind grants keep the key the matrix always used.
  assert.equal(scopeKey('model', 'gpt-4o', '*'), 'model:gpt-4o');
  assert.equal(scopeKey('model', 'gpt-4o', undefined), 'model:gpt-4o');
});

test('names never fall back to raw ids and stay distinct', () => {
  const id = busSchemaScopeId('tentabus-aaaaaaaa', 'org-1');
  const unknown = { instance: 'Usunięty TentaBus', org: 'Usunięta organizacja' };
  const known = busSchemaScopeNames([id], [{ addonId: 'tentabus-aaaaaaaa', title: 'Szpital' }], [{ orgId: 'org-1', name: 'Oddział' }], unknown);
  assert.deepEqual(known.get(id), { instance: 'Szpital', org: 'Oddział' });
  assert.deepEqual(busSchemaScopeNames([id], [], [], unknown).get(id), unknown);
  assert.deepEqual(busSchemaScopeNames(['garbage'], [], [], unknown).get('garbage'), unknown);

  // Two different removed instances must not produce the same header.
  const a = busSchemaScopeId('tentabus-aaaaaaaa', 'org-1');
  const b = busSchemaScopeId('tentabus-bbbbbbbb', 'org-1');
  const names = busSchemaScopeNames([a, b], [], [{ orgId: 'org-1', name: 'Oddział' }], unknown);
  assert.notEqual(names.get(a).instance, names.get(b).instance);
  for (const n of names.values()) assert.ok(!n.instance.includes('tentabus-'));
});

test('the whole access_keys namespace exists, key-identical, in all five locales', () => {
  const source = readFileSync(join(HERE, 'access-keys.js'), 'utf8');
  // Every literal key, including the ones passed through a helper parameter.
  const used = [...new Set([...source.matchAll(/'access_keys\.([a-z_0-9]+)'/g)].map((m) => m[1]))];
  assert.ok(used.length >= 90, `expected the screen's keys to be found, got ${used.length}`);
  const bundles = Object.fromEntries(LOCALES.map((loc) => [
    loc,
    JSON.parse(readFileSync(join(WWW_ROOT, 'i18n', `${loc}.json`), 'utf8')).access_keys ?? {},
  ]));
  const plKeys = Object.keys(bundles.pl).sort();
  const placeholders = (v) => [...new Set([...v.matchAll(/\{(\w+)[|}]/g)].map((m) => m[1]))].sort();
  for (const loc of LOCALES) {
    assert.deepEqual(Object.keys(bundles[loc]).sort(), plKeys, `${loc}: access_keys key set differs from pl`);
    for (const key of used) {
      const value = bundles[loc][key];
      assert.ok(typeof value === 'string' && value.trim(), `${loc}: access_keys.${key} missing`);
      assert.deepEqual(placeholders(value), placeholders(bundles.pl[key]), `${loc}: access_keys.${key} placeholders`);
    }
  }
  // Counted nouns go through a plural selector, never "1 zasobów".
  for (const key of ['resources_count', 'selected_count']) {
    assert.match(bundles.pl[key], /\{count\|[^|}]+\|[^|}]+\|[^|}]+\}/, `pl ${key} needs three forms`);
  }
  // Owner's naming: "Wzory wiadomości" in Polish, "schemas" in English, "node" never "węzeł".
  assert.equal(bundles.pl.bus_schema_title, 'Wzory wiadomości');
  assert.equal(bundles.en.bus_schema_title, 'Message schemas');
  assert.ok(!JSON.stringify(bundles).match(/templates|węz/i));
  // Write never implies read, and the text says so in every language.
  for (const loc of LOCALES) assert.ok(bundles[loc].bus_schema_write_desc.length > 20);
  assert.match(bundles.pl.bus_schema_write_desc, /bez Czytania/);
});

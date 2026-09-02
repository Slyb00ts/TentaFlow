// =============================================================================
// File: modules/tentanas/i18n-parity.test.js
// Description: The shares round added whole sections to the TentaNas
// translations; this asserts the key set of every one of them is identical
// across the five locales and that the interpolation placeholders match the
// Polish source, so a missing translation never surfaces as a raw key.
// =============================================================================

import { WWW_ROOT } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

const LOCALES = ['pl', 'en', 'de', 'es', 'fr'];
const SECTIONS = ['tentanas.shares', 'tentanas.wizard_share', 'tentanas.share_users', 'tentanas.fleet_mounts', 'tentanas.config'];
const SINGLE_KEYS = [
  'tentanas.tabs.shares',
  'tentanas.kpi.shares',
  'tentanas.jobs.kind_share_create',
  'tentanas.jobs.kind_share_update',
  'tentanas.jobs.kind_share_delete',
  'tentanas.jobs.kind_config_import',
  'tentanas.wizard.step_restore',
  'tentanas.wizard.restore_title',
  'tentanas.wizard.restore_sub',
  'tentanas.wizard.restore_skip',
  'tentanas.wizard.restore_apply',
  'addon_uninstall.entries.tentanas_smb_config',
  'addon_uninstall.entries.tentanas_nfs_exports',
  'addon_uninstall.entries.tentanas_fleet_mounts',
  'addon_uninstall.entries.tentanas_pools',
  'addon_uninstall.entries.tentanas_config_backup',
];

const bundles = Object.fromEntries(LOCALES.map((l) => [l, JSON.parse(readFileSync(join(WWW_ROOT, 'i18n', `${l}.json`), 'utf8'))]));
const dig = (obj, path) => path.split('.').reduce((o, k) => (o && typeof o === 'object' ? o[k] : undefined), obj);
const flatten = (obj, prefix = '') => Object.entries(obj).flatMap(([k, v]) => (v && typeof v === 'object' ? flatten(v, `${prefix}${k}.`) : [`${prefix}${k}`]));
// `{n}` style placeholders and the `{n|one|few|many}` plural selector both name the parameter first.
const placeholders = (s) => [...String(s).matchAll(/\{([a-zA-Z0-9_]+)(?:\|[^}]*)?\}/g)].map((m) => m[1]).sort();

test('every shares-round section has the same key set in all five locales', () => {
  for (const section of SECTIONS) {
    const reference = flatten(dig(bundles.pl, section)).sort();
    assert.ok(reference.length > 0, `${section} exists in pl`);
    for (const l of LOCALES) {
      const keys = flatten(dig(bundles[l], section) || {}).sort();
      assert.deepEqual(keys, reference, `${section} keys in ${l} match pl`);
    }
  }
});

test('the single keys added around the sections exist everywhere and are non-empty strings', () => {
  for (const key of SINGLE_KEYS) {
    for (const l of LOCALES) {
      const v = dig(bundles[l], key);
      assert.equal(typeof v, 'string', `${key} in ${l}`);
      assert.ok(v.trim().length > 0, `${key} in ${l} is not blank`);
    }
  }
});

test('interpolation placeholders match the Polish source in every locale', () => {
  for (const section of SECTIONS) {
    for (const key of flatten(dig(bundles.pl, section))) {
      const full = `${section}.${key}`;
      const expected = placeholders(dig(bundles.pl, full));
      for (const l of LOCALES) {
        assert.deepEqual(placeholders(dig(bundles[l], full)), expected, `${full} placeholders in ${l}`);
      }
    }
  }
  for (const key of SINGLE_KEYS) {
    const expected = placeholders(dig(bundles.pl, key));
    for (const l of LOCALES) assert.deepEqual(placeholders(dig(bundles[l], key)), expected, `${key} placeholders in ${l}`);
  }
});

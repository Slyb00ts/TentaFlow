// =============================================================================
// File: modules/mesh-node-registry.test.js
// Description: The Mesh screen's node-registry strip. Two invariants the screen
// cannot check for itself: the device kinds it offers are exactly the ones
// `sync_nodes.node_kind` accepts (a kind only SQLite knows about would be
// refused after the click), and every label it renders exists in all five
// locales. Plus the operator hint's mapping, which is the one place where the
// device kind is allowed to say anything about authority.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve as pathResolve } from 'node:path';

import { NODE_KINDS, operatorHintFor } from './mesh-helpers.js';

const here = dirname(fileURLToPath(import.meta.url));
const CORE_ROOT = pathResolve(here, '..', '..', '..');
const LOCALES = ['pl', 'en', 'de', 'es', 'fr'];

/// The kinds the database itself accepts, read out of the `sync_nodes` CHECK
/// constraint. Parsed rather than copied: a copy would drift silently, which is
/// the failure this test exists to catch.
function kindsFromSchema() {
  const sql = readFileSync(pathResolve(CORE_ROOT, 'src', 'db', 'migrations.rs'), 'utf8');
  const match = sql.match(/node_kind TEXT NOT NULL DEFAULT 'unknown'\s*\n\s*CHECK\(node_kind IN \(([^)]*)\)\)/);
  assert.ok(match, 'the sync_nodes.node_kind CHECK constraint must be findable in migrations.rs');
  return match[1].split(',').map((s) => s.trim().replace(/^'|'$/g, ''));
}

test('the screen offers exactly the device kinds the schema accepts', () => {
  assert.deepEqual([...NODE_KINDS].sort(), kindsFromSchema().sort());
});

test('operator hint follows the device kind, and only where the kind says something', () => {
  assert.equal(operatorHintFor('desktop'), true);
  assert.equal(operatorHintFor('server'), true);
  assert.equal(operatorHintFor('authority'), true);
  assert.equal(operatorHintFor('phone'), false);
  assert.equal(operatorHintFor('tablet'), false);
  for (const quiet of ['unknown', 'laptop', 'shared', '', undefined, 'nonsense']) {
    assert.equal(operatorHintFor(quiet), null, `no suggestion for '${quiet}'`);
  }
});

test('every registry label the strip renders exists in all five locales', () => {
  const keys = [
    'node_kind',
    'operator_node',
    'operator_hint_on',
    'operator_hint_off',
    'node_profile_saved',
    'node_profile_saved_local_only',
    'node_profile_failed',
    ...NODE_KINDS.map((k) => `node_kind_${k}`),
  ];
  for (const locale of LOCALES) {
    const bundle = JSON.parse(readFileSync(pathResolve(CORE_ROOT, 'www', 'i18n', `${locale}.json`), 'utf8'));
    for (const key of keys) {
      const value = bundle.mesh?.[key];
      assert.equal(typeof value, 'string', `${locale}: mesh.${key} is missing`);
      assert.notEqual(value.trim(), '', `${locale}: mesh.${key} is empty`);
    }
    // The hint is interpolated with the localized kind name; a bundle that drops
    // the placeholder renders a sentence about nothing.
    for (const key of ['operator_hint_on', 'operator_hint_off']) {
      assert.match(bundle.mesh[key], /\{kind\}/, `${locale}: mesh.${key} lost its {kind} placeholder`);
    }
  }
});

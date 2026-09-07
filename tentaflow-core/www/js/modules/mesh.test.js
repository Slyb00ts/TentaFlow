// =============================================================================
// Plik: modules/mesh.test.js
// Opis: Sprawdzenie parsowania modułu Mesh ładowanego podczas startu aplikacji.
// Przykład: node --test js/modules/mesh.test.js
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

test('moduł Mesh przechodzi parser ES bez powtórzonych deklaracji', () => {
  const result = spawnSync(process.execPath, ['--check', fileURLToPath(new URL('./mesh.js', import.meta.url))], {
    encoding: 'utf8', timeout: 10000,
  });
  assert.ifError(result.error);
  assert.equal(result.status, 0, result.stderr);
});

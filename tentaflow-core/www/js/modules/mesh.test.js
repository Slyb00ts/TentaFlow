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

test('statyczne importy drzewa app.js wskazują istniejące pliki dashboardu', (t) => {
  const result = spawnSync(process.execPath, ['--experimental-vm-modules', '--input-type=module', '-e', `
    import { SourceTextModule } from 'node:vm';
    import { readFileSync, statSync } from 'node:fs';
    import { resolve, dirname, relative, sep } from 'node:path';
    const root = resolve(process.argv[1]);
    const pending = [resolve(root, 'js/app.js')];
    const visited = new Set();
    const missing = [];
    while (pending.length) {
      const file = pending.pop();
      if (visited.has(file)) continue;
      visited.add(file);
      const module = new SourceTextModule(readFileSync(file, 'utf8'), { identifier: file });
      for (const specifier of module.dependencySpecifiers) {
        if (!specifier.startsWith('/') && !specifier.startsWith('.')) throw new Error('Nielokalny statyczny import: ' + specifier);
        const pathname = specifier.split(/[?#]/, 1)[0];
        const target = resolve(specifier.startsWith('/') ? root : dirname(file), specifier.startsWith('/') ? '.' + pathname : pathname);
        if (!target.startsWith(root + sep)) throw new Error('Import poza dashboardem');
        if (!statSync(target, { throwIfNoEntry: false })?.isFile()) missing.push({ source: relative(root, file), import: specifier });
        else pending.push(target);
      }
    }
    if (missing.length) { console.error(JSON.stringify(missing)); process.exitCode = 1; }
    else console.log(JSON.stringify({ modules: visited.size, missing: 0 }));
  `, fileURLToPath(new URL('../../', import.meta.url))], { encoding: 'utf8', timeout: 10000 });
  assert.ifError(result.error);
  assert.equal(result.status, 0, result.stderr);
  const observation = JSON.parse(result.stdout);
  assert.ok(observation.modules > 1);
  assert.equal(observation.missing, 0);
  t.diagnostic(`Sprawdzono ${observation.modules} modułów statycznego drzewa app.js`);
});

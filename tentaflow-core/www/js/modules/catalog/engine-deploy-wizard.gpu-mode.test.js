// =============================================================================
// File: modules/catalog/engine-deploy-wizard.gpu-mode.test.js
// Description: What the deploy wizard promises about GPU passthrough. A CPU-only
//       engine (`engine.gpu_supported === false`) never renders the GPU step, so
//       `selection.gpuSelectMode` kept its hard 'all' default and was sent
//       anyway — a searxng container came up with every host card in
//       DeviceRequests. Pinned here: such an engine always emits 'none', a GPU
//       engine keeps the operator's choice, and the deploy payload really uses
//       the resolved mode (also for `gpu_ids`).
//       The wizard imports the whole dashboard, so the functions under test are
//       cut out of the real file by brace matching — the code tested here is the
//       code that ships.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, 'engine-deploy-wizard.js'), 'utf8');

function cut(src, name) {
  const start = src.indexOf(`function ${name}(`);
  if (start < 0) throw new Error(`no definition: ${name}`);
  let depth = 0;
  let i = src.indexOf('{', start);
  for (; i < src.length; i += 1) {
    if (src[i] === '{') depth += 1;
    else if (src[i] === '}') {
      depth -= 1;
      if (depth === 0) break;
    }
  }
  return src.slice(start, i + 1);
}

// eslint-disable-next-line no-new-func
const effectiveGpuSelectMode = new Function(
  `${cut(source, 'effectiveGpuSelectMode')}\nreturn effectiveGpuSelectMode;`,
)();

const cpuOnly = { engine: { id: 'searxng', gpu_supported: false } };
const gpuEngine = { engine: { id: 'vllm', gpu_supported: true } };
const unspecified = { engine: { id: 'whisper' } };

test('a CPU-only engine can never emit a GPU mode', () => {
  for (const mode of ['all', 'specific', 'none', undefined, null]) {
    assert.equal(effectiveGpuSelectMode(cpuOnly, mode), 'none');
  }
});

test('a GPU engine keeps the operator choice', () => {
  assert.equal(effectiveGpuSelectMode(gpuEngine, 'all'), 'all');
  assert.equal(effectiveGpuSelectMode(gpuEngine, 'specific'), 'specific');
  assert.equal(effectiveGpuSelectMode(gpuEngine, 'none'), 'none');
  // Manifest without the field: the wizard still shows the step, default 'all'.
  assert.equal(effectiveGpuSelectMode(unspecified, 'specific'), 'specific');
  assert.equal(effectiveGpuSelectMode(unspecified, undefined), 'all');
  // No entry loaded yet must not throw.
  assert.equal(effectiveGpuSelectMode(null, 'all'), 'all');
});

test('the GPU step is skipped for a CPU-only engine', () => {
  // Skipping the step is what left gpuSelectMode at its default, so the two
  // predicates have to agree on which engines are CPU-only.
  const skip = cut(source, 'shouldSkipGpuStep');
  assert.match(skip, /gpu_supported/);
  assert.match(skip, /=== false\) return true/);
});

test('the deploy payload sends the resolved mode, not the raw selection', () => {
  assert.match(source, /gpu_select_mode: gpuSelectMode,/);
  assert.match(source, /gpu_ids: gpuSelectMode === 'specific' \? selection\.gpuIds : null,/);
  assert.match(
    source,
    /const gpuSelectMode = effectiveGpuSelectMode\(engineEntry, selection\.gpuSelectMode\);/,
  );
  // Nothing else may reach the wire with the unresolved selection.
  assert.equal(source.match(/gpu_select_mode:/g).length, 1);
});

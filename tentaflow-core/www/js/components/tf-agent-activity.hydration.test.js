// =============================================================================
// File: components/tf-agent-activity.hydration.test.js
// Description: Tests for the run row of <tf-agent-activity> — what it says a
//       run cost. The widget used to live on live events alone, so a session
//       console opened after the turn had ended dated every run from the moment
//       the client happened to look ("root 0s · 0 tokens" for a turn that took
//       74 seconds and spent 7434). These pin the hydration path: a persisted
//       row wins over observation time, a finished run is measured between its
//       own two timestamps, and a bare status update may not invent a row.
//       The module imports its tf-* siblings by absolute URL, which Node cannot
//       resolve; those imports only register elements, so the shipped source is
//       loaded with them stripped.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, 'tf-agent-activity.js'), 'utf8')
  .replace(/^import\s+'[^']*';$/gm, '');
const { TfAgentActivity } = await import(
  `data:text/javascript;base64,${Buffer.from(source).toString('base64')}`
);

const LABELS = { tokens: 'tok', runs_title: 'Runs', no_runs: 'No runs', cancel: 'Cancel' };

function mount() {
  const el = new TfAgentActivity();
  el.labels = LABELS;
  el.setAttribute('level', 'tree');
  document.body.appendChild(el);
  return el;
}

function rows(el) {
  return [...el.querySelectorAll('.tf-aa-run')].map((r) => ({
    agent: r.querySelector('.tf-aa-run-agent')?.textContent.trim(),
    meta: r.querySelector('.tf-aa-run-meta')?.textContent.trim(),
    title: r.querySelector('.tf-aa-run-main')?.getAttribute('title'),
  }));
}

// ---------------------------------------------------------------------------
// Hydration
// ---------------------------------------------------------------------------

test('a finished run is measured between its own two timestamps', () => {
  const el = mount();
  el.setRunInfo('r-1', {
    agent: 'root run #1',
    status: 'completed',
    startedAt: Date.UTC(2026, 7, 15, 14, 20, 57),
    finishedAt: Date.UTC(2026, 7, 15, 14, 22, 11),
    promptTokens: 6800,
    completionTokens: 634,
  });
  assert.deepEqual(rows(el), [{ agent: 'root run #1', meta: '1m 14s · 7434 tok', title: null }]);
});

test('the persisted start replaces the moment the client first looked', () => {
  const el = mount();
  // A replayed event creates the row with "now" as its start…
  el.applyEvent({ kind: 'child_spawned', run_id: 'r-2', agent: 'root' });
  assert.match(rows(el)[0].meta, /^0s · 0 tok$/);
  // …and the row it belongs to corrects it.
  el.setRunInfo('r-2', {
    status: 'running',
    startedAt: Date.now() - 95_000,
    promptTokens: 1000,
    completionTokens: 200,
  });
  const meta = rows(el)[0].meta;
  assert.match(meta, /^1m 3[45]s · 1200 tok$/, meta);
});

test('a run still going is measured against the clock, not left at zero', () => {
  const el = mount();
  el.setRunInfo('r-3', { status: 'running', startedAt: Date.now() - 42_000 });
  assert.match(rows(el)[0].meta, /^4[12]s · 0 tok$/);
});

test('a bare status update never invents a run the tree would count', () => {
  const el = mount();
  el.setRunStatus('ghost', 'completed');
  assert.equal(el.runCount, 0);
  assert.deepEqual(rows(el), []);
  // The same call on a row that exists still moves its status.
  el.setRunInfo('r-4', { status: 'running', startedAt: Date.now() });
  el.setRunStatus('r-4', 'failed');
  assert.equal(el.querySelector('tf-chip')?.textContent.trim(), 'failed');
});

test('an absent counter leaves the row at zero instead of guessing', () => {
  const el = mount();
  el.setRunInfo('r-5', { status: 'completed', startedAt: 1000, finishedAt: 4000 });
  assert.equal(rows(el)[0].meta, '3s · 0 tok');
});

test('the model the run addressed is offered without crowding the row', () => {
  const el = mount();
  el.setRunInfo('r-6', { status: 'completed', startedAt: 0, finishedAt: 1000, model: 'qwen3.5-nvfp4' });
  assert.equal(rows(el)[0].title, 'qwen3.5-nvfp4');
  assert.doesNotMatch(rows(el)[0].meta, /qwen/);
});

test('a timestamp handed over as a string is parsed, not dropped', () => {
  const el = mount();
  el.setRunInfo('r-7', {
    status: 'completed',
    startedAt: '2026-08-15T14:20:57Z',
    finishedAt: '2026-08-15T14:22:11Z',
  });
  assert.equal(rows(el)[0].meta, '1m 14s · 0 tok');
});

test('the parent link hydrated from a row nests the child under it', () => {
  const el = mount();
  el.setRunInfo('parent', { status: 'running', startedAt: Date.now(), agent: 'root #1' });
  el.setRunInfo('child', { status: 'running', startedAt: Date.now(), agent: 'sub #2', parentRunId: 'parent' });
  const depths = [...el.querySelectorAll('.tf-aa-run')].map((r) => r.getAttribute('style'));
  assert.deepEqual(depths, ['--depth:0', '--depth:1']);
});

// ---------------------------------------------------------------------------
// The list has to survive a session whose runs have all finished
// ---------------------------------------------------------------------------

test('a pinned list keeps showing runs after the last one ends', () => {
  const el = mount();
  el.setRunInfo('r-8', { status: 'completed', startedAt: 1000, finishedAt: 2000 });
  assert.equal(el.hasActivity(), false, 'nothing is in flight');
  assert.equal(el.querySelector('.tf-aa-run') != null, true, 'and the row is still on screen');
  assert.equal(el.runCount, 1, 'the badge counts exactly what the list holds');
});

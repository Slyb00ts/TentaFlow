// =============================================================================
// File: components/tf-agent-activity.current-line.test.js
// Description: Tests for the collapsed line of <tf-agent-activity> — what an
//       operator reads while a turn is in flight. Between two events a run is
//       waiting on its model, and the line used to call that "idle" for as long
//       as a generation took; these pin "thinking" with a moving clock, the
//       tool in flight while it executes, and that the clock stops with the run.
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

const LABELS = { idle: 'idle', thinking: 'thinking…', step_tool: 'tool' };

function mount() {
  const el = new TfAgentActivity();
  el.labels = LABELS;
  document.body.appendChild(el);
  return el;
}

const line = (el) => el.querySelector('.tf-aa-line')?.textContent.trim();

test('a running run with no tool in flight reads as thinking, not idle', (t) => {
  const el = mount();
  t.after(() => el.remove());
  el.applyEvent({ kind: 'child_spawned', run_id: 'r-1', agent: 'code-orchestrator' });
  assert.equal(line(el), 'code-orchestrator · thinking… · 0s');
});

test('a tool call reads as that tool until its result lands', (t) => {
  const el = mount();
  t.after(() => el.remove());
  el.applyEvent({ kind: 'child_spawned', run_id: 'r-1', agent: 'code-orchestrator' });
  el.applyEvent({ kind: 'tool_call_started', run_id: 'r-1', name: 'core.exec' });
  assert.equal(line(el), 'code-orchestrator · tool · core.exec · 0s');
  el.applyEvent({ kind: 'tool_call_finished', run_id: 'r-1', name: 'core.exec', status: 'ok' });
  assert.equal(line(el), 'code-orchestrator · thinking… · 0s');
});

test('the clock counts from the last sign of life and stops with the run', async (t) => {
  const el = mount();
  t.after(() => el.remove());
  el.applyEvent({ kind: 'child_spawned', run_id: 'r-1', agent: 'code-orchestrator' });
  el._runs.get('r-1').lastEventAt = Date.now() - 42_000;
  await new Promise((resolve) => setTimeout(resolve, 1100));
  assert.match(line(el), /^code-orchestrator · thinking… · 4[23]s$/);
  el.setRunStatus('r-1', 'completed');
  assert.equal(el._clock, null, 'a finished run left the clock ticking');
});

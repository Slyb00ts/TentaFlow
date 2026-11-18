// =============================================================================
// File: modules/run-events.derivation.test.js
// Description: Unit tests for `lib/run-events.js` — the ONE derivation that
//       turns stored run-event rows into <tf-run-timeline> records, shared by
//       the Zdarzenia browser and the Code Studio session tab. Three promises
//       are pinned because breaking any of them makes the widget state a
//       number nobody measured:
//         * tool spans pair by call_id, never by tool name,
//         * an opener with no closer keeps duration null (in flight),
//         * the model band is split at first_token into TTFT and decode.
//       The module is loaded as its REAL shipped source with only the browser
//       absolute import of I18n swapped for a stub, so the code under test is
//       the code that ships.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, '..', 'lib', 'run-events.js'), 'utf8');

// I18n resolves through the dashboard's absolute URLs, which node cannot load.
// The stub echoes the key so every derived label is deterministic.
const STUB = "const I18n = { t: (key) => key };\n";
const patched = source.replace(/^import \{ I18n \}.*$/m, STUB);
assert.notEqual(patched, source, 'the I18n import must have been replaced');

const moduleUrl = `data:text/javascript;base64,${Buffer.from(patched, 'utf8').toString('base64')}`;
const {
  normalizeRow, deriveTimeline, plotFrom, runOutcome, rowDetail, finishStatusLabel,
} = await import(moduleUrl);

function row(seq, kind, atMs, payload, extra = {}) {
  return normalizeRow({
    runId: 'run-1',
    seq,
    atMs: atMs,
    kind,
    origin: 'code_studio',
    actorKind: 'user',
    actorId: 'u-1',
    payloadJson: JSON.stringify({ kind, ...payload }),
    ...extra,
  });
}

test('two concurrent calls to one tool keep their own spans', () => {
  const rows = [
    row(1, 'tool_call', 1000, { name: 'core.fs_read' }, { callId: 'c-a' }),
    row(2, 'tool_call', 1010, { name: 'core.fs_read' }, { callId: 'c-b' }),
    // The SECOND call finishes first. Pairing by tool name would hand this end
    // to the first band and leave the second one open.
    row(3, 'tool_result', 1050, { name: 'core.fs_read', ok: true }, { callId: 'c-b' }),
    row(4, 'tool_result', 1400, { name: 'core.fs_read', ok: true }, { callId: 'c-a' }),
  ];
  const records = deriveTimeline(rows);
  const byId = new Map(records.map((r) => [r.id, r]));
  assert.equal(byId.get('run-1#1').duration, 400, 'the first call spans to its own result');
  assert.equal(byId.get('run-1#2').duration, 40, 'the second call spans to its own result');
});

test('an opener with no closer stays in flight instead of borrowing an end', () => {
  const rows = [
    row(1, 'request_started', 1000, { model: 'qwen3-27b' }),
    row(2, 'first_token', 1200, {}),
  ];
  const records = deriveTimeline(rows);
  assert.equal(records.length, 1);
  assert.equal(records[0].duration, null, 'no assistant_message means no end');
  assert.equal(records[0].ttft, 200, 'the TTFT that WAS measured is still stated');
});

test('the model band is split at first_token into TTFT and decode', () => {
  const rows = [
    row(1, 'request_started', 1000, { model: 'qwen3-27b' }),
    row(2, 'first_token', 1200, {}),
    row(3, 'assistant_message', 1900, { model: 'qwen3-27b', tokens: 40 }),
  ];
  const records = deriveTimeline(rows);
  assert.equal(records[0].ttft, 200);
  assert.equal(records[0].duration, 900);
  // The closer states the DECODE leg, not the whole span — otherwise the
  // ledger would print the request duration twice.
  assert.equal(rows[2].durationMs, 700);
});

test('plotFrom shifts records onto the earliest row as start=0', () => {
  const rows = [
    row(1, 'request_started', 5000, { model: 'qwen3-27b' }),
    row(2, 'assistant_message', 5300, { model: 'qwen3-27b' }),
  ];
  const plot = plotFrom(rows);
  assert.equal(plot.epoch, 5000);
  assert.equal(plot.records[0].start, 5000, 'the record keeps wall-clock start');
  assert.equal(plot.shifted[0].start, 0, 'the widget gets run-clock start');
});

// --- request_finished: the one closing rule --------------------------------

test('a flow with no LLM call is closed by request_finished with its status and duration', () => {
  const rows = [
    row(1, 'request_started', 1000, {}),
    row(2, 'step_start', 1000, { step: 'bus_consume' }),
    row(3, 'step_end', 1004, { step: 'bus_consume', status: 'ok' }),
    row(4, 'request_finished', 1005, { status: 'completed', duration_ms: 5 }),
  ];
  const records = deriveTimeline(rows);
  const band = records.find((r) => r.id === 'run-1#1');
  assert.equal(band.duration, 5, 'the request band ends at the closing row');
  assert.equal(band.status, 'completed');
  assert.equal(band.error, false);
  assert.equal(rows[3].durationMs, 5);
  assert.equal(rows[0].durationMs, 5, 'the opener states the whole span');
  assert.deepEqual(runOutcome(rows, 'run-1'), { status: 'completed', durationMs: 5 });
});

test('an error row does not close a run that has request_finished', () => {
  const rows = [
    row(1, 'request_started', 1000, {}),
    row(2, 'error', 1010, { stage: 'tool', message: 'step failed' }, { nodeId: 'n1' }),
    row(3, 'request_finished', 1100, { status: 'completed' }),
  ];
  const band = deriveTimeline(rows).find((r) => r.id === 'run-1#1');
  assert.equal(band.duration, 100, 'the run went on past the failed step');
  assert.equal(band.error, false);
  assert.equal(rows[1].bandId, null, 'the error is its own event, not the request closer');
});

test('assistant_message states the decode leg but does not close when request_finished exists', () => {
  const rows = [
    row(1, 'request_started', 1000, { model: 'm' }),
    row(2, 'first_token', 1200, {}),
    row(3, 'assistant_message', 1900, { model: 'm' }),
    row(4, 'request_finished', 2000, { status: 'completed' }),
  ];
  const band = deriveTimeline(rows).find((r) => r.id === 'run-1#1');
  assert.equal(band.ttft, 200);
  assert.equal(rows[2].durationMs, 700, 'decode leg');
  assert.equal(band.duration, 1000, 'the request ends at request_finished');
});

test('a failed run marks its band as an error and a cancelled one does not', () => {
  const failed = [row(1, 'request_started', 0, {}), row(2, 'request_finished', 30, { status: 'failed' })];
  assert.equal(deriveTimeline(failed)[0].error, true);
  assert.equal(deriveTimeline(failed)[0].status, 'failed');
  const cancelled = [row(1, 'request_started', 0, {}), row(2, 'request_finished', 30, { status: 'cancelled' })];
  assert.equal(deriveTimeline(cancelled)[0].error, false);
  assert.equal(deriveTimeline(cancelled)[0].status, 'cancelled');
});

test('a run without request_finished keeps the legacy closers', () => {
  const answered = [
    row(1, 'request_started', 1000, {}),
    row(2, 'assistant_message', 1500, {}),
  ];
  assert.equal(deriveTimeline(answered)[0].duration, 500);
  assert.equal(runOutcome(answered, 'run-1'), null, 'no outcome is invented');

  const errored = [
    row(1, 'request_started', 1000, {}),
    row(2, 'error', 1300, { stage: 'llm', message: 'x' }),
  ];
  const band = deriveTimeline(errored)[0];
  assert.equal(band.duration, 300);
  assert.equal(band.error, true);

  const open = [row(1, 'request_started', 1000, {})];
  assert.equal(deriveTimeline(open)[0].duration, null, 'still in flight: no end is guessed');
});

test('request_finished does not close a different run', () => {
  const rows = [
    row(1, 'request_started', 1000, {}, { runId: 'run-a' }),
    row(1, 'request_started', 1000, {}, { runId: 'run-b' }),
    row(2, 'request_finished', 1050, { status: 'completed' }, { runId: 'run-b' }),
  ];
  const byId = new Map(deriveTimeline(rows).map((r) => [r.id, r]));
  assert.equal(byId.get('run-a#1').duration, null);
  assert.equal(byId.get('run-b#1').duration, 50);
  assert.equal(runOutcome(rows, 'run-a'), null);
});

test('a closing row without its opener states only its own recorded duration', () => {
  const rows = [row(7, 'request_finished', 5000, { status: 'failed', duration_ms: 42 })];
  deriveTimeline(rows);
  assert.equal(rows[0].durationMs, 42);
  assert.deepEqual(runOutcome(rows, 'run-1'), { status: 'failed', durationMs: 42 });
  const bare = [row(7, 'request_finished', 5000, { status: 'failed' })];
  assert.deepEqual(runOutcome(bare, 'run-1'), { status: 'failed', durationMs: null });
});

test('request_finished is described by its status label; an unknown status stays text', () => {
  assert.equal(rowDetail(row(1, 'request_finished', 0, { status: 'completed' })), 'events.status_completed');
  assert.equal(finishStatusLabel('failed'), 'events.status_failed');
  assert.equal(finishStatusLabel('mystery'), 'mystery');
});

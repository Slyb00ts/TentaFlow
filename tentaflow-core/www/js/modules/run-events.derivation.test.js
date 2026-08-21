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
const { normalizeRow, deriveTimeline, plotFrom } = await import(moduleUrl);

function row(seq, kind, atMs, payload, extra = {}) {
  return normalizeRow({
    run_id: 'run-1',
    seq,
    at_ms: atMs,
    kind,
    origin: 'code_studio',
    actor_kind: 'user',
    actor_id: 'u-1',
    payload_json: JSON.stringify({ kind, ...payload }),
    ...extra,
  });
}

test('two concurrent calls to one tool keep their own spans', () => {
  const rows = [
    row(1, 'tool_call', 1000, { name: 'core.fs_read' }, { call_id: 'c-a' }),
    row(2, 'tool_call', 1010, { name: 'core.fs_read' }, { call_id: 'c-b' }),
    // The SECOND call finishes first. Pairing by tool name would hand this end
    // to the first band and leave the second one open.
    row(3, 'tool_result', 1050, { name: 'core.fs_read', ok: true }, { call_id: 'c-b' }),
    row(4, 'tool_result', 1400, { name: 'core.fs_read', ok: true }, { call_id: 'c-a' }),
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

// =============================================================================
// File: lib/run-events.js
// Description: Stored run-event rows → timeline records. This is the ONE
//   derivation shared by every host of <tf-run-timeline>: the Zdarzenia
//   browser (modules/events.js) and the Code Studio session tab
//   (modules/code-studio-panes.js). A second copy would drift, and two screens
//   showing different durations for the same run is worse than one screen.
//
//   The server returns STORED ROWS, not bars. Every span is a DIFFERENCE
//   BETWEEN TWO EVENTS derived here:
//     model band  request_started → assistant_message | error, split at
//                 first_token into TTFT and decoding,
//     tool band   tool_call → tool_result paired by call_id (never by tool
//                 name — two concurrent calls to one tool must not cross),
//     step band   step_start → step_end,
//     turn        turn_start → turn_end, drawn as a boundary by the widget.
//   An opener with no closer among the loaded rows keeps `duration: null`, so
//   the widget draws a start marker instead of a bar. No end is ever guessed,
//   substituted with "now", or dropped.
//
//   Labels come from the `events.*` i18n namespace because they name event
//   kinds, not screen chrome — the same event reads the same on every host.
// =============================================================================

import { I18n } from '/js/i18n.js';

function t(key, vars) { return I18n.t(`events.${key}`, vars ?? null); }

// -----------------------------------------------------------------------------
// Wire rows
// -----------------------------------------------------------------------------

/**
 * The decoders emit camelCase for some bodies and both spellings for others, so
 * a row is read through both names once and never again.
 */
function field(raw, camel, snake) {
  const v = raw[camel] ?? raw[snake];
  return v === undefined ? null : v;
}

export function normalizeRow(raw) {
  const runId = String(field(raw, 'runId', 'run_id') ?? '');
  const seq = Number(field(raw, 'seq', 'seq') ?? 0);
  const payloadJson = String(field(raw, 'payloadJson', 'payload_json') ?? '');
  let payload = null;
  try {
    payload = payloadJson ? JSON.parse(payloadJson) : null;
  } catch (_) {
    // A payload this build cannot parse stays visible as raw text in the
    // inspector; dropping the row would hide an event that WAS recorded.
    payload = null;
  }
  return {
    key: `${runId}#${seq}`,
    runId,
    seq,
    atMs: Number(field(raw, 'atMs', 'at_ms') ?? 0),
    kind: String(field(raw, 'kind', 'kind') ?? ''),
    origin: String(field(raw, 'origin', 'origin') ?? ''),
    actorKind: String(field(raw, 'actorKind', 'actor_kind') ?? ''),
    actorId: field(raw, 'actorId', 'actor_id'),
    actorUserId: field(raw, 'actorUserId', 'actor_user_id'),
    orgId: field(raw, 'orgId', 'org_id'),
    correlationId: field(raw, 'correlationId', 'correlation_id'),
    sessionId: field(raw, 'sessionId', 'session_id'),
    nodeId: field(raw, 'nodeId', 'node_id'),
    callId: field(raw, 'callId', 'call_id'),
    payload,
    payloadJson,
    // Filled by deriveTimeline().
    turn: null,
    bandId: null,
    durationMs: null,
  };
}

// -----------------------------------------------------------------------------
// Row text
// -----------------------------------------------------------------------------

export function actorLabel(row) {
  if (row.actorId) return row.actorId;
  return t(`actor_kind_${row.actorKind}`);
}

function truncate(text, max = 160) {
  const s = String(text ?? '');
  return s.length > max ? `${s.slice(0, max)}…` : s;
}

/** The model / tool / step this row is about, for the ledger's name column. */
export function rowName(row) {
  const p = row.payload ?? {};
  switch (row.kind) {
    case 'request_started':
    case 'first_token':
    case 'assistant_message':
      return p.model ?? '';
    case 'tool_call':
    case 'tool_result':
      return p.name ?? row.callId ?? '';
    case 'step_start':
    case 'step_end':
      return p.step ?? '';
    default:
      return '';
  }
}

function assistantBody(p) {
  const body = p.body;
  if (!body || typeof body !== 'object') return '';
  if (typeof body.text === 'string') return truncate(body.text);
  if (typeof body.omitted === 'string') return t('body_omitted', { reason: body.omitted });
  return '';
}

function toolArguments(p) {
  const args = p.arguments;
  if (!args || typeof args !== 'object') return '';
  return truncate(Object.entries(args).map(([k, v]) => `${k}=${v}`).join(' · '));
}

/** One line describing what the row records, built from its stored payload. */
export function rowDetail(row) {
  const p = row.payload ?? {};
  switch (row.kind) {
    case 'request_started':
      return [p.service_type, p.modality, p.flow_id].filter(Boolean).join(' · ');
    case 'first_token':
      return t('detail_first_token');
    case 'assistant_message': {
      const parts = [assistantBody(p)];
      if (typeof p.tokens === 'number') parts.push(t('detail_tokens', { count: p.tokens }));
      return parts.filter(Boolean).join(' · ');
    }
    case 'tool_call': {
      const parts = [toolArguments(p)];
      if (row.callId) parts.push(t('detail_call', { id: row.callId }));
      return parts.filter(Boolean).join(' · ');
    }
    case 'tool_result': {
      const status = p.ok === false ? t('result_failed') : t('result_ok');
      return [status, truncate(p.summary ?? '')].filter(Boolean).join(' · ');
    }
    case 'step_start':
      return p.step ?? '';
    case 'step_end':
      return [p.step, p.status].filter(Boolean).join(' · ');
    case 'turn_start':
      return t('detail_turn', { turn: p.turn ?? '?' });
    case 'turn_end':
      return [t('detail_turn', { turn: p.turn ?? '?' }), p.status].filter(Boolean).join(' · ');
    case 'error':
      return [p.stage, truncate(p.message ?? '')].filter(Boolean).join(' · ');
    default:
      return '';
  }
}

// -----------------------------------------------------------------------------
// Rows → timeline records
// -----------------------------------------------------------------------------

function newBand(row, lane, name) {
  return {
    id: row.key,
    seq: row.seq,
    start: row.atMs,
    duration: null,
    lane,
    kind: row.kind,
    origin: row.origin,
    actor: actorLabel(row),
    actorKind: row.actorKind,
    name,
    detail: rowDetail(row),
    turn: row.turn,
    ttft: null,
    error: false,
  };
}

/**
 * Walks every loaded row of every run and pairs the openers with their closers.
 * Returns the timeline records; each row is annotated in place with the band it
 * belongs to and with the duration THIS row states (a first_token states the
 * TTFT, an assistant_message the decode leg, a closer the whole span).
 */
export function deriveTimeline(rows) {
  const byRun = new Map();
  for (const row of rows) {
    row.turn = null;
    row.bandId = null;
    row.durationMs = null;
    if (!byRun.has(row.runId)) byRun.set(row.runId, []);
    byRun.get(row.runId).push(row);
  }

  const records = [];
  for (const runRows of byRun.values()) {
    runRows.sort((a, b) => a.seq - b.seq);
    let turn = null;
    let turnStart = null;
    let request = null;
    const openTools = new Map();
    const openSteps = new Map();

    for (const row of runRows) {
      const p = row.payload ?? {};
      if (row.kind === 'turn_start') {
        turn = p.turn ?? null;
        turnStart = row;
      }
      row.turn = turn;

      switch (row.kind) {
        case 'request_started': {
          // A previous request with no closer stays in flight on purpose: the
          // log has no end for it and the next request does not supply one.
          const band = newBand(row, 'model', p.model ?? t('unknown_model'));
          records.push(band);
          request = band;
          row.bandId = band.id;
          break;
        }
        case 'first_token': {
          if (request) {
            request.ttft = row.atMs - request.start;
            row.bandId = request.id;
            row.durationMs = request.ttft;
          }
          break;
        }
        case 'assistant_message': {
          if (request) {
            request.duration = row.atMs - request.start;
            row.bandId = request.id;
            // The decode leg when a first_token was recorded, otherwise the
            // whole request — never a difference from an event that is absent.
            row.durationMs = request.ttft === null
              ? request.duration
              : request.duration - request.ttft;
            request = null;
          }
          break;
        }
        case 'tool_call': {
          const band = newBand(row, 'tools', p.name ?? t('unknown_tool'));
          records.push(band);
          row.bandId = band.id;
          // No call_id means no way to pair it — the band stays open rather
          // than borrowing the end of a same-named call.
          if (row.callId) openTools.set(row.callId, band);
          break;
        }
        case 'tool_result': {
          const band = row.callId ? openTools.get(row.callId) : null;
          if (band) {
            band.duration = row.atMs - band.start;
            band.error = p.ok === false;
            row.bandId = band.id;
            row.durationMs = band.duration;
            openTools.delete(row.callId);
          }
          break;
        }
        case 'step_start': {
          const band = newBand(row, 'messages', p.step ?? t('unknown_step'));
          records.push(band);
          row.bandId = band.id;
          if (p.step) openSteps.set(p.step, band);
          break;
        }
        case 'step_end': {
          const band = p.step ? openSteps.get(p.step) : null;
          if (band) {
            band.duration = row.atMs - band.start;
            band.error = p.status === 'error' || p.status === 'failed';
            row.bandId = band.id;
            row.durationMs = band.duration;
            openSteps.delete(p.step);
          }
          break;
        }
        case 'turn_end': {
          if (turnStart && turnStart.turn === (p.turn ?? null)) {
            row.durationMs = row.atMs - turnStart.atMs;
          }
          break;
        }
        case 'error': {
          // An error closes the open request when there is one; otherwise it is
          // an instant of its own and gets no band.
          if (request) {
            request.duration = row.atMs - request.start;
            request.error = true;
            row.bandId = request.id;
            row.durationMs = request.duration;
            request = null;
          }
          break;
        }
        default:
          break;
      }
    }
  }

  // The row that OPENED a band states the whole span, the way the prototype's
  // ledger does. Done after the walk because the closer may arrive many rows
  // later — or never, in which case the cell stays "—".
  const bandById = new Map(records.map((band) => [band.id, band]));
  for (const row of rows) {
    const band = row.bandId ? bandById.get(row.bandId) : null;
    if (band && band.id === row.key) {
      row.durationMs = band.duration;
    }
  }
  return records;
}

/**
 * The epoch a set of rows is plotted against (start=0 on the run clock) and the
 * records shifted onto it. `records` come back with `start` relative to the
 * epoch, which is what <tf-run-timeline> expects.
 */
export function plotFrom(rows) {
  const records = deriveTimeline(rows);
  const epoch = rows.length ? Math.min(...rows.map((r) => r.atMs)) : 0;
  return { epoch, records, shifted: records.map((r) => ({ ...r, start: r.start - epoch })) };
}

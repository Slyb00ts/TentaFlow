// ===== File: modules/sla/model.js — SLA policy wording, coded-error mapping and view models (pure, no DOM) =====
//
// Every function here turns wire data (`SlaBody` responses, `FlowSummary.slaBrief`,
// `BusConsumerStatus`) into the words the screens print, so the same sentence
// is said by the policy list, the flow list, the execution history and the
// TentaBus consumer page, and so each can be checked against the real locale
// files. The server's `detail` text is English for logs and is never shown.

import { I18n } from '/js/i18n.js';
import { fmtExact } from '/js/utils.js';

export const T = (key, vars) => I18n.t(`sla.${key}`, vars);

export const CALL_WINDOWS = [1, 60, 3600, 86400];

const num = (n) => fmtExact(n, I18n.getLanguage());

/** The limits of a policy a field of the editor maps to, with the range the server enforces. */
export const FIELD_RANGES = {
  max_concurrency: [1, 1024],
  max_queue: [0, 10000],
  queue_timeout_ms: [1, 300000],
  call_limit: [1, 4294967295],
  timeout_ms: [100, 86400000],
  latency_p95_ms: [1, 4294967295],
  error_rate_pct: [1, 100],
  warn_at_pct: [1, 99],
};

/** The editor field a server `field` belongs to (the window select carries the call window's seconds). */
export const EDITOR_FIELD = { call_window_secs: 'call_window' };

export const editorField = (wireField) => EDITOR_FIELD[wireField] ?? wireField;

/** "30 s", "10 min", "2 h", "500 ms": the shortest exact form of a duration. */
export function durationText(ms) {
  const value = Number(ms);
  if (!Number.isFinite(value)) return '—';
  if (value >= 3_600_000 && value % 3_600_000 === 0) return T('dur_h', { value: num(value / 3_600_000) });
  if (value >= 60_000 && value % 60_000 === 0) return T('dur_min', { value: num(value / 60_000) });
  if (value >= 1000 && value % 1000 === 0) return T('dur_s', { value: num(value / 1000) });
  return T('dur_ms', { value: num(value) });
}

export const windowName = (secs) => (CALL_WINDOWS.includes(secs) ? T(`window_${secs}`) : `${num(secs)} s`);
/** "minutę": the window as it follows "na" ("60 na minutę"). */
export const windowPer = (secs) => (CALL_WINDOWS.includes(secs) ? T(`window_per_${secs}`) : `${num(secs)} s`);
export const windowShort = (secs) => (CALL_WINDOWS.includes(secs) ? T(`window_short_${secs}`) : `${num(secs)} s`);

/** "14:11" or, with seconds, "14:11:00" in the reader's locale. */
export function clockText(epochMs, withSeconds = false) {
  const at = new Date(Number(epochMs));
  if (!Number.isFinite(at.getTime())) return '—';
  return new Intl.DateTimeFormat(I18n.getLanguage(), {
    hour: '2-digit', minute: '2-digit', ...(withSeconds ? { second: '2-digit' } : {}), hour12: false,
  }).format(at);
}

// ---------------------------------------------------------------------------
// Policy list cells
// ---------------------------------------------------------------------------

/** `{ main, sub }` of the "Wątki" column. */
export function threadsText(spec) {
  const main = spec.maxConcurrency == null ? T('unlimited') : num(spec.maxConcurrency);
  if (spec.maxConcurrency == null) return { main, sub: '' };
  const parts = [spec.maxQueue > 0
    ? T('queue_detail', { queue: num(spec.maxQueue), wait: durationText(spec.queueTimeoutMs ?? 10_000) })
    : T('queue_none')];
  if (spec.ordering === 'none') parts.push(T('ordering_none_note'));
  return { main, sub: parts.join('; ') };
}

/** `{ main, sub }` of the "Limit / okno" column. */
export function callsText(spec) {
  if (spec.callLimit == null) return { main: T('unlimited'), sub: '' };
  return {
    main: T('calls_value', { limit: num(spec.callLimit), window: windowName(spec.callWindowSecs) }),
    sub: spec.warnAtPct == null ? '' : T('warn_detail', { pct: num(spec.warnAtPct) }),
  };
}

/** `{ main, sub }` of the "Progi" column. */
export function thresholdsText(spec) {
  const parts = [];
  if (spec.latencyP95Ms != null) parts.push(T('threshold_p95', { value: num(spec.latencyP95Ms) }));
  if (spec.errorRatePct != null) parts.push(T('threshold_errors', { value: num(spec.errorRatePct) }));
  return {
    main: parts.length ? parts.join(' · ') : '—',
    sub: spec.timeoutMs == null ? '' : T('timeout_detail', { value: durationText(spec.timeoutMs) }),
  };
}

/** "Nie można usunąć: polityka jest przypisana do 2 przepływów. …" */
export const deleteBlockedText = (count) => T('delete_blocked', { count: num(count), n: count });

/** "Rejestracja-API · 2 wątki · 60/min": one option of the assign window. */
export function policyOptionLabel(policy) {
  const spec = policy.spec;
  const parts = [spec.name];
  if (spec.maxConcurrency != null) parts.push(T('option_threads', { count: num(spec.maxConcurrency), n: spec.maxConcurrency }));
  if (spec.callLimit != null) parts.push(`${num(spec.callLimit)}/${windowShort(spec.callWindowSecs)}`);
  return parts.join(' · ');
}

// ---------------------------------------------------------------------------
// Coded errors
// ---------------------------------------------------------------------------

/** A refusal the server answered with a coded `SlaErrorWire`; carries what the screen needs to explain it. */
export class SlaRefusal extends Error {
  constructor(error) {
    super(error?.detail || error?.code || 'sla refusal');
    this.name = 'SlaRefusal';
    this.slaError = error ?? { code: 'unknown' };
  }
}

/** Throws a [`SlaRefusal`] when `response.error` carries a code; otherwise returns `response`. */
export function unlessRefused(response) {
  if (response?.error) throw new SlaRefusal(response.error);
  return response;
}

function invalidValueText(field) {
  if (field === 'name') return T('error.name');
  if (field === 'description') return T('error.description');
  if (field === 'call_window_secs') return T('error.window_required');
  if (field === 'call_limit') return T('error.limit_required');
  if (field === 'ordering') return T('error.ordering');
  const range = FIELD_RANGES[field];
  if (range) return T('error.range', { min: num(range[0]), max: num(range[1]) });
  return T('error.invalid_value');
}

/**
 * `{ message, field }` for a coded error. `field` is the editor field to mark
 * (`null` when the refusal belongs to the whole request). An unknown code gets
 * the generic text, never the English `detail`.
 */
export function describeSlaError(error) {
  const code = error?.code;
  if (code === 'invalid_value') return { message: invalidValueText(error.field), field: editorField(error.field) };
  if (code === 'name_taken') return { message: T('error.name_taken'), field: 'name' };
  if (code === 'policy_in_use') return { message: deleteBlockedText(Number(error.assignments) || 0), field: null };
  if (code === 'system_flow') return { message: T('error.system_flow'), field: null };
  if (code === 'governed_elsewhere') return { message: T('error.governed_elsewhere'), field: null };
  return { message: T('error.generic'), field: null };
}

/** The text of any failure of an SLA call: a coded refusal says its own, everything else keeps the transport's message. */
export function failureText(err) {
  if (err instanceof SlaRefusal) return describeSlaError(err.slaError).message;
  return T(`error.${protocolErrorKind(err)}`);
}

const PROTOCOL_CODES = {
  2: 'denied', policy_denied: 'denied', policydenied: 'denied',
  3: 'auth', auth_required: 'auth', authrequired: 'auth',
  4: 'node_unreachable', node_unreachable: 'node_unreachable', nodeunreachable: 'node_unreachable',
  6: 'rate_limited', rate_limited: 'rate_limited', ratelimited: 'rate_limited',
  9: 'not_found', not_found: 'not_found', notfound: 'not_found',
};

/** Which localized message a protocol failure gets; the server's English text is never shown. */
export function protocolErrorKind(err) {
  const code = typeof err?.code === 'string' ? err.code.toLowerCase() : err?.code;
  if (code != null && PROTOCOL_CODES[code]) return PROTOCOL_CODES[code];
  if (/timed out/i.test(String(err?.message ?? ''))) return 'timeout';
  return 'generic';
}

// ---------------------------------------------------------------------------
// State of a governed flow
// ---------------------------------------------------------------------------

const CHIP = { ok: 'ok', warn: 'warn', breach: 'err', throttled: 'warn' };
export const stateChip = (state) => CHIP[state] ?? 'info';
export const stateLabel = (state) => (state in CHIP ? T(`state.${state}`) : state);

/** What an open breach says in one line of the flow list. */
export function breachLine(breach, status) {
  const peak = Number(breach.peakValue);
  const threshold = Number(breach.threshold);
  switch (breach.kind) {
    case 'latency': return T('detail.latency', { value: num(Math.round(peak)), threshold: num(Math.round(threshold)) });
    case 'errors': return T('detail.errors', { value: formatPercent(peak), threshold: formatPercent(threshold) });
    case 'calls_limit': return T('detail.calls_limit', { used: num(Math.round(peak)), limit: num(Math.round(threshold)) });
    case 'concurrency_saturated': return T('detail.saturated');
    case 'timeouts': return T('detail.timeouts', { count: num(Math.round(peak)), n: Math.round(peak) });
    case 'calls_warn': return windowLine(status?.window ?? null, null);
    default: return '';
  }
}

function formatPercent(value) {
  return new Intl.NumberFormat(I18n.getLanguage(), { maximumFractionDigits: 1 }).format(value);
}

function windowLine(window, brief) {
  const used = window?.used ?? brief?.windowUsed;
  const limit = window?.limit ?? brief?.windowLimit;
  if (used == null || limit == null) return '';
  const base = { used: num(used), limit: num(limit) };
  return window?.resetsAtMs ? T('detail.window_reset', { ...base, time: clockText(window.resetsAtMs) }) : T('detail.window', base);
}

function concurrencyLine(inFlight, max) {
  return max == null ? '' : T('detail.concurrency', { used: num(inFlight ?? 0), max: num(max) });
}

/**
 * What the flow list shows for one flow's policy:
 * `{ kind: 'none' | 'system' | 'own' | 'elsewhere', policyId, name, state, chipStatus, label, detail }`.
 * `status` is the flow's `SlaFlowStatusWire` when the caller has read it (it adds the
 * window reset time and the open breach); the brief on the flow row is enough without it.
 */
export function slaView(flow, policies, status = null) {
  const brief = flow.slaBrief ?? null;
  if (flow.isSystem) return { kind: 'system' };
  if (!brief) return { kind: 'none' };
  const base = { state: brief.state, chipStatus: stateChip(brief.state), label: stateLabel(brief.state) };
  if (brief.governedElsewhere) {
    const detail = concurrencyLine(brief.inFlight, brief.maxConcurrency) || windowLine(null, brief);
    return { kind: 'elsewhere', policyId: '', name: T('cell.elsewhere'), ...base, detail };
  }
  const known = policies.get(flow.slaPolicyId);
  const name = known ? known.spec.name : T('cell.policy_unknown');
  const breach = status?.openBreaches?.find((b) => b.kind !== 'calls_warn') ?? null;
  let detail;
  if (breach) detail = breachLine(breach, status);
  else if (brief.state === 'warn') detail = windowLine(status?.window ?? null, brief);
  else detail = concurrencyLine(brief.inFlight, brief.maxConcurrency) || windowLine(status?.window ?? null, brief);
  return { kind: 'own', policyId: flow.slaPolicyId ?? '', name, ...base, detail };
}

// ---------------------------------------------------------------------------
// "Stan SLA" of the execution history: one row per node
// ---------------------------------------------------------------------------

const DASH = '—';

/** "3,1 %" for a rate the evaluator measured (one decimal at most). */
export const percentText = (value) => (value == null ? DASH : `${formatPercent(Number(value))} %`);

/** "25.08 07:02": when a breach was raised or resolved. */
export function dayAndClock(epochMs) {
  const at = new Date(Number(epochMs));
  if (!Number.isFinite(at.getTime())) return DASH;
  return new Intl.DateTimeFormat(I18n.getLanguage(), { day: '2-digit', month: '2-digit', hour: '2-digit', minute: '2-digit', hour12: false }).format(at);
}

/**
 * The cells of one node's row. `status` is that node's `SlaFlowStatusWire` (`null` when the
 * node did not answer), `policy.spec` the policy's limits, `consumer` the flow's
 * `BusConsumerStatus` on that node when the flow reads a topic.
 */
export function nodeCells({ status, spec, consumer = null, nowMs, bus = false }) {
  if (!status) {
    return {
      inFlight: DASH, inFlightSub: '', window: DASH, windowSub: '', p95: DASH, p95Sub: '', errors: DASH, errorsSub: '',
      refused: DASH, refusedSub: '', nodeSub: '', chipStatus: 'neutral', label: T('state.unreachable'), state: 'unreachable',
    };
  }
  const evaluation = status.evaluation ?? null;
  const window = status.window ?? null;
  const throttledUntil = consumer?.throttledUntil != null && Number(consumer.throttledUntil) > nowMs ? Number(consumer.throttledUntil) : null;
  const limit = status.maxConcurrency ?? spec.maxConcurrency ?? null;
  const today = (status.rejectedCallsToday ?? 0) + (status.rejectedConcurrencyToday ?? 0);
  const inWindow = evaluation ? (evaluation.rejectedCalls ?? 0) + (evaluation.rejectedConcurrency ?? 0) : null;
  const shownState = throttledUntil != null && status.state === 'ok' ? 'throttled' : status.state;
  const windowSub = throttledUntil != null
    ? T('cells.held_until', { time: clockText(throttledUntil, true) })
    : (window?.resetsAtMs ? T('cells.reset_at', { time: clockText(window.resetsAtMs) }) : '');
  return {
    inFlight: limit == null ? num(status.inFlight ?? 0) : `${num(status.inFlight ?? 0)}/${num(limit)}`,
    nodeSub: bus && consumer?.state === 'running' ? T('cells.leads') : '',
    inFlightSub: bus
      ? (limit == null ? '' : T(spec.ordering === 'none' ? 'cells.unordered' : 'cells.in_partition'))
      : (spec.maxQueue != null && limit != null && spec.maxQueue > 0 ? T('cells.queue', { waiting: num(status.waiting ?? 0), max: num(spec.maxQueue) }) : ''),
    window: window ? `${num(window.used)}/${num(window.limit)}` : DASH,
    windowSub,
    p95: evaluation?.p95Ms == null ? DASH : T('cells.ms', { value: num(Math.round(evaluation.p95Ms)) }),
    p95Sub: spec.latencyP95Ms == null ? '' : T('cells.threshold_ms', { value: num(spec.latencyP95Ms) }),
    errors: percentText(evaluation?.errorRatePct),
    errorsSub: spec.errorRatePct == null ? '' : T('cells.threshold_pct', { value: num(spec.errorRatePct) }),
    // A bus flow is slowed, not refused: its column counts today's holds. A refused flow counts the
    // window the evaluator looked at, with today's total beside it.
    refused: bus ? num(today) : (inWindow == null ? DASH : num(inWindow)),
    refusedSub: bus ? T('cells.today') : T(inWindow == null ? 'cells.today_count' : 'cells.today_in_window', { count: num(today) }),
    chipStatus: stateChip(shownState),
    label: stateLabel(shownState),
    state: shownState,
    throttledUntil,
  };
}

/** The worst state among the rows: breach, then warn, then ok. */
export function overallState(states) {
  for (const worst of ['breach', 'warn', 'throttled', 'ok']) if (states.includes(worst)) return worst;
  return null;
}

/** `{ kind, threshold, peak }` texts of one breach for the history table. */
export function breachCells(breach, spec) {
  const peak = Number(breach.peakValue);
  const threshold = Number(breach.threshold);
  const kind = T(`breach.${breach.kind}`);
  switch (breach.kind) {
    case 'latency': return { kind, threshold: T('cells.up_to_ms', { value: num(Math.round(threshold)) }), peak: T('cells.ms', { value: num(Math.round(peak)) }) };
    case 'errors': return { kind, threshold: T('cells.up_to_pct', { value: formatPercent(threshold) }), peak: percentText(peak) };
    case 'calls_warn': {
      const limit = spec.callLimit;
      const at = limit == null ? '' : ` (${num(Math.ceil((limit * threshold) / 100))}/${num(limit)})`;
      return { kind, threshold: `${T('warn_detail', { pct: formatPercent(threshold) })}${at}`, peak: limit == null ? percentText(peak) : `${num(Math.round((limit * peak) / 100))}/${num(limit)}` };
    }
    case 'calls_limit':
      return { kind, threshold: T('calls_per', { limit: num(Math.round(threshold)), window: windowPer(spec.callWindowSecs) }), peak: `${num(Math.round(peak))}/${num(Math.round(threshold))}` };
    case 'concurrency_saturated':
      return { kind, threshold: T('cells.slots', { count: num(Math.round(threshold)), n: Math.round(threshold) }), peak: num(Math.round(peak)) };
    default:
      return { kind, threshold: DASH, peak: num(Math.round(peak)) };
  }
}

/**
 * The reading that stands for a flow which reads a topic: the node whose consumer runs,
 * else the connected node, else the first. `readings` = `[{ node: { isLocal }, consumer }]`.
 */
export function leadingReading(readings) {
  return readings.find((r) => r.consumer?.state === 'running') ?? readings.find((r) => r.node.isLocal) ?? readings[0];
}

/**
 * What the assign action of a flow row does: `{ show, disabled, title }`. Assigning needs
 * `sla.manage` (`canManage`), the right to edit flows (`canEditFlows`), a flow that is not
 * a platform one and a policy of this organization.
 */
export function assignAction({ flow, kind, canManage, canEditFlows }) {
  if (!canManage) return { show: false, disabled: true, title: '' };
  if (flow.isSystem) return { show: true, disabled: true, title: T('cell.system_title') };
  if (kind === 'elsewhere') return { show: true, disabled: true, title: T('assign.elsewhere_title') };
  if (!canEditFlows) return { show: true, disabled: true, title: T('assign.no_edit_title') };
  return { show: true, disabled: false, title: T('assign.title') };
}

/** Resolves with `null` when `promise` fails or has not settled within `ms`: "no answer" for the caller. */
export function bounded(promise, ms) {
  let timer = null;
  const deadline = new Promise((resolve) => { timer = setTimeout(() => resolve(null), ms); });
  return Promise.race([Promise.resolve(promise).catch(() => null), deadline]).finally(() => clearTimeout(timer));
}

/**
 * Keeps the answer of the newest request only: `begin()` returns a check that stays true until
 * another `begin()` or a `cancel()`, so a late answer for an earlier request draws nothing.
 */
export function newestRequest() {
  let ticket = 0;
  return {
    begin() {
      const mine = ++ticket;
      return () => mine === ticket;
    },
    cancel() { ticket += 1; },
  };
}

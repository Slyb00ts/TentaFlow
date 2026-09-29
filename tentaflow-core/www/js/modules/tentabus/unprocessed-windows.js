// ===== File: modules/tentabus/unprocessed-windows.js — the windows over unprocessed messages: Pokaż, Ponów, Odrzuć, Ponów wszystkie =====
//
// What the server does, which each window states before anyone confirms
// (tentaflow-core/src/bus: `dlq_retry`, `dlq_discard`, `dlq_retry_all_v1`):
//   - "Ponów" appends the message to the END of its topic as a new message
//     (after everything written since). Every consumer of the topic gets it
//     again — also those that processed it the first time — and a consumer
//     that fails it again counts its attempts from zero. The message leaves
//     the list; it cannot be retried twice.
//   - "Odrzuć" takes it off the list for good: nobody can retry it any more.
//     Its bytes stay in the store until the store's cleanup removes them.
//   - "Ponów wszystkie" does "Ponów" for the oldest messages of the topic,
//     at most 500 at once, and leaves those rejected at write: the topic's
//     pattern would only reject them again.
//   - every one of them is written to the audit log.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { T, fmtCount, fmtBytes, fmtWhen } from '/js/modules/tentabus/format.js';
import { bytesToPreviewText, parseBlobRefJson } from '/js/modules/tentabus/payload.js';
import { reasonLabel, sourceText, attemptsText, receiversText, RETRY_ALL_MAX } from '/js/modules/tentabus/unprocessed.js';
import { windowEl, openConfirmWindow } from '/js/modules/tentabus/windows.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;
const PAYLOAD_PREVIEW_BYTES = 4096;

const kv = (pairs) => `<div class="tb-kv-grid">${pairs.map(([k, v]) => `<div class="k">${escapeHtml(k)}</div><div class="v">${v}</div>`).join('')}</div>`;

function reasonHtml(rec) {
  const chip = rec.atWrite ? ` <tf-chip size="sm" variant="outline" status="info" label="${escapeAttr(T('unprocessed.at_write'))}"></tf-chip>` : '';
  return `${escapeHtml(reasonLabel(rec.reason))}${chip}`;
}

function consumerHtml(rec) {
  return rec.group ? `<span class="mono">${escapeHtml(rec.group)}</span>` : escapeHtml(T('unprocessed.view.no_consumer'));
}

/** The title a window gives one message: where it sits, or when it was rejected. */
export function recordTitle(rec, nowMs = Date.now()) {
  if (rec.sourcePartition == null || rec.sourceOffset == null) return T('unprocessed.view.title_at_write', { when: fmtWhen(rec.arrivalMs, nowMs) });
  return T('unprocessed.view.title', { partition: fmtCount(rec.sourcePartition), number: fmtCount(rec.sourceOffset) });
}

/** The "label — value" lines of "Pokaż". */
export function recordFacts(rec, maxAttempts, nowMs = Date.now()) {
  const when = (ms) => (ms == null ? '—' : escapeHtml(fmtWhen(ms, nowMs)));
  const facts = [
    [T('unprocessed.view.k_source'), escapeHtml(sourceText(rec))],
    [T('unprocessed.view.k_consumer'), consumerHtml(rec)],
    [T('unprocessed.view.k_reason'), reasonHtml(rec)],
  ];
  // What the pattern check reports is the validator's own technical text.
  const errorKey = rec.atWrite ? 'unprocessed.view.k_error_at_write' : 'unprocessed.view.k_error';
  if (rec.errorHidden) facts.push([T(errorKey), escapeHtml(T('unprocessed.view.error_hidden'))]);
  else if (rec.errorMessage) facts.push([T(errorKey), escapeHtml(rec.errorMessage)]);
  facts.push([T('unprocessed.view.k_attempts'), escapeHtml(attemptsText(rec, maxAttempts))]);
  if (rec.atWrite) {
    facts.push([T('unprocessed.view.k_rejected'), when(rec.rejectedMs)]);
  } else {
    facts.push([T('unprocessed.view.k_written'), when(rec.writtenMs)]);
    facts.push([T('unprocessed.view.k_first'), when(rec.firstFailedMs)]);
    facts.push([T('unprocessed.view.k_last'), when(rec.lastFailedMs)]);
  }
  return facts;
}

function payloadHtml(rec) {
  const blob = rec.isBlobRef ? parseBlobRefJson(rec.payload) : null;
  if (blob) return `<div class="tb-explain-box">${escapeHtml(T('unprocessed.view.blob', { size: fmtBytes(blob.size_bytes), mime: blob.mime }))}</div>`;
  const text = bytesToPreviewText(rec.payload, PAYLOAD_PREVIEW_BYTES);
  return `<pre class="tb-payload" data-role="payload">${escapeHtml(text || T('unprocessed.view.body_empty'))}</pre>`;
}

/**
 * "Pokaż": one message with what is known about its failure and its body as
 * this reader may see it. `onRetry`/`onDiscard` (an administrator's) open
 * the matching window in its place; `null` leaves the button out.
 */
export function openUnprocessedView({ rec, maxAttempts, onRetry = null, onDiscard = null, nowMs = Date.now() }) {
  const win = windowEl({ title: recordTitle(rec, nowMs), icon: 'eye', width: 720, cls: 'tb-unp-view' });
  win.innerHTML = `
    <div slot="body" class="stack">
      ${kv(recordFacts(rec, maxAttempts, nowMs))}
      <div class="tb-unp-body-head"><b>${escapeHtml(T('unprocessed.view.k_body'))}</b>${rec.truncated ? ` <tf-chip size="sm" variant="outline" status="warn" label="${escapeAttr(T('unprocessed.view.truncated'))}"></tf-chip>` : ''}</div>
      ${payloadHtml(rec)}
      <div class="tb-audit-banner">${sprite('shield')}<span>${escapeHtml(T('unprocessed.view.rules'))} ${escapeHtml(T('unprocessed.view.audit'))}</span></div>
    </div>
    <div slot="footer">
      ${onDiscard ? `<tf-button variant="ghost" data-act="discard">${escapeHtml(T('unprocessed.action_discard'))}</tf-button>` : ''}
      ${onRetry ? `<tf-button variant="secondary" icon="refresh" data-act="retry">${escapeHtml(T('unprocessed.action_retry'))}</tf-button>` : ''}
      <tf-button variant="primary" data-act="close">${escapeHtml(T('unprocessed.view.close'))}</tf-button>
    </div>`;
  document.body.appendChild(win);
  win.addEventListener('click', (e) => {
    const btn = e.target.closest('[data-act]');
    if (!btn) return;
    win.close(true);
    if (btn.dataset.act === 'retry') onRetry?.();
    else if (btn.dataset.act === 'discard') onDiscard?.();
  });
  return win;
}

/** The sentences under "Co się stanie po ponowieniu" of one message. */
export function retryImpact({ topic, consumers, maxAttempts }) {
  const lines = [
    T('unprocessed.retry.impact_back', { topic }),
    receiversText(consumers),
    T('unprocessed.retry.impact_list'),
  ];
  if (maxAttempts) lines.push(T('unprocessed.retry.impact_again', { count: fmtCount(maxAttempts), n: maxAttempts }));
  return lines;
}

/** The sentences under "Co się stanie po ponowieniu" of "Ponów wszystkie". */
export function retryAllImpact({ topic, consumers, plan, maxAttempts }) {
  const lines = [T('unprocessed.retry_all.impact_back', { topic }), receiversText(consumers, plan.batch !== 1)];
  const left = plan.atWrite + plan.rest;
  lines.push(left > 0
    ? T('unprocessed.retry_all.impact_left', { count: fmtCount(left), n: left })
    : T('unprocessed.retry_all.impact_none_left'));
  if (maxAttempts) lines.push(T('unprocessed.retry.impact_again_many', { count: fmtCount(maxAttempts), n: maxAttempts }));
  return lines;
}

/** "Ponów" of one message. `retry()` answers the server's `accepted`. */
export function openRetryOne({ rec, topic, consumers, maxAttempts, retry, describeError, onDone, nowMs = Date.now() }) {
  return openConfirmWindow({
    title: T('unprocessed.retry.title'),
    icon: 'refresh',
    lead: kv([
      [T('unprocessed.view.k_from'), `<span class="mono">${escapeHtml(topic)}</span> · ${escapeHtml(sourceText(rec))}`],
      [T('unprocessed.view.k_consumer'), consumerHtml(rec)],
      [T('unprocessed.view.k_reason'), reasonHtml(rec)],
      [T('unprocessed.view.k_when'), escapeHtml(fmtWhen(rec.arrivalMs, nowMs))],
    ]),
    impactTitle: T('unprocessed.retry.will_happen'),
    impact: retryImpact({ topic, consumers, maxAttempts }),
    audit: T('unprocessed.retry.audit'),
    button: T('unprocessed.retry.button'),
    danger: false,
    run: retry,
    describeError,
    onDone,
  });
}

/** "Odrzuć" of one message. */
export function openDiscardOne({ rec, topic, discard, describeError, onDone }) {
  const lead = [
    `<div class="tb-explain-box">${escapeHtml(rec.group ? T('unprocessed.discard.lead_consumer', { group: rec.group }) : T('unprocessed.discard.lead'))}</div>`,
    kv([
      [T('unprocessed.view.k_from'), `<span class="mono">${escapeHtml(topic)}</span> · ${escapeHtml(sourceText(rec))}`],
      [T('unprocessed.view.k_reason'), reasonHtml(rec)],
    ]),
  ].join('');
  return openConfirmWindow({
    title: T('unprocessed.discard.title'),
    icon: 'close',
    lead,
    impactTitle: T('unprocessed.discard.will_happen'),
    impact: [T('unprocessed.discard.impact')],
    audit: T('unprocessed.discard.audit'),
    button: T('unprocessed.discard.button'),
    danger: true,
    run: discard,
    describeError,
    onDone,
  });
}

/**
 * "Ponów wszystkie" of one topic. `plan` = `retryAllPlan(...)`; `retryAll()`
 * answers `{ retried, failed }`.
 */
export function openRetryAll({ topic, plan, consumers, maxAttempts, retryAll, describeError, onDone }) {
  const leadKey = plan.exact ? 'unprocessed.retry_all.lead' : 'unprocessed.retry_all.lead_estimate';
  const lead = [
    `<div class="tb-explain-box">${T(leadKey, { topic: `<b class="mono">${escapeHtml(topic)}</b>`, count: `<b>${escapeHtml(fmtCount(plan.retryable))}</b>`, n: plan.retryable })}</div>`,
    plan.atWrite > 0 ? `<div class="tb-explain-box">${escapeHtml(T('unprocessed.retry_all.at_write', { count: fmtCount(plan.atWrite), n: plan.atWrite }))}</div>` : '',
  ].join('');
  return openConfirmWindow({
    title: T('unprocessed.retry_all.title'),
    icon: 'refresh',
    lead,
    impactTitle: T('unprocessed.retry.will_happen'),
    impact: retryAllImpact({ topic, consumers, plan, maxAttempts }),
    info: plan.rest > 0
      ? T('unprocessed.retry_all.limit_rest', { max: fmtCount(RETRY_ALL_MAX), count: fmtCount(plan.rest), n: plan.rest })
      : T('unprocessed.retry_all.limit', { max: fmtCount(RETRY_ALL_MAX) }),
    audit: T('unprocessed.retry.audit_many'),
    button: plan.exact || plan.rest > 0
      ? T('unprocessed.retry_all.button_n', { count: fmtCount(plan.batch), n: plan.batch })
      : T('unprocessed.retry_all.button'),
    danger: false,
    run: retryAll,
    describeError,
    onDone,
  });
}

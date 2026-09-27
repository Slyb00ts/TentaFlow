// ===== File: modules/tentabus/unprocessed.js — Nieprzetworzone wiadomości: the instance tab (T06) and a topic's section =====
//
// An unprocessed message is a record of `__dlq.<topic>`: a copy of a message
// its consumer gave up on after the topic's retries (`dlq.group_id`, the
// attempts and when they failed), or one the topic's pattern rejected when
// it was written (`dlq.rejected_at_ms`, no consumer). The server lists them
// newest first per topic (`DlqListRequest { newest_first }`), reading them
// with the source topic's data-hiding rules, and never lists one already
// retried or discarded.
//
// The instance tab: a tile per topic that has unprocessed messages (how
// many, the most common reason, when the last arrived, "Ponów wszystkie"),
// then one list of all of them, newest first, merged here from each topic's
// newest page. A row leads to its topic's section, where each message has
// "Pokaż", "Ponów" and "Odrzuć" (unprocessed-windows.js). Counts come from
// the stats snapshot, so the header card, the main tab, the section menu and
// the tiles say the same number.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { patchHtml, patchKeyedList, setAttr, setText, setRowsIfChanged } from '/js/lib/dom-patch.js';
import { T, fmtCount, fmtDayTime } from '/js/modules/tentabus/format.js';
import { headerText } from '/js/modules/tentabus/payload.js';
import { isInternalTopic } from '/js/modules/tentabus/model.js';
import { loadErrorHtml } from '/js/modules/tentabus/overview.js';
import '/js/components/tf-button.js';
import '/js/components/tf-table.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-spinner.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

/** Records one `DlqListRequest` asks for: the server's own ceiling. */
export const UNPROCESSED_PAGE = 100;
/** Rows a list shows at first and adds per "Wczytaj więcej". */
export const LIST_STEP = 10;
/** What one "Ponów wszystkie" republishes at most (`DLQ_RETRY_ALL_MAX` on the server). */
export const RETRY_ALL_MAX = 500;

export const REASONS = ['schema_violation', 'consumer_error', 'consumer_timeout', 'permission_denied', 'payload_too_large', 'blob_missing'];

const asNumber = (text) => (text != null && /^\d+$/.test(text) ? Number(text) : null);

/**
 * One wire record of `__dlq.<topic>` as the screen reads it. `source*` is
 * where the message sits in its topic (a message rejected at write never
 * got there), `arrivalMs` when it became unprocessed — the server's own
 * ordering key (`dlq::arrival_ms`).
 */
export function unprocessedRecord(topic, r) {
  const h = (k) => headerText(r.headers, k);
  const rejectedMs = asNumber(h('dlq.rejected_at_ms'));
  const lastFailedMs = asNumber(h('dlq.last_failed_at_ms'));
  const reason = h('dlq.reason');
  return {
    topic,
    key: `${topic}\u0000${r.partition}:${r.offset}`,
    dlqPartition: Number(r.partition),
    dlqOffset: Number(r.offset),
    sourcePartition: asNumber(h('dlq.source_partition')),
    sourceOffset: asNumber(h('dlq.source_offset')),
    group: h('dlq.group_id') || null,
    reason: REASONS.includes(reason) ? reason : 'unknown',
    atWrite: rejectedMs != null,
    attempts: asNumber(h('dlq.attempts')),
    errorMessage: h('dlq.error_message') || '',
    writtenMs: Number(r.timestampMs),
    firstFailedMs: asNumber(h('dlq.first_failed_at_ms')),
    lastFailedMs,
    rejectedMs,
    arrivalMs: lastFailedMs ?? rejectedMs ?? Number(r.timestampMs),
    payload: r.payloadPreview,
    isBlobRef: Boolean(r.isBlobRef),
    truncated: Boolean(r.truncated),
  };
}

/** "Program odbiorcy zgłosił błąd". */
export const reasonLabel = (reason) => T(`unprocessed.reason.${REASONS.includes(reason) ? reason : 'unknown'}`);

/** "partycja 2 · numer 72 373 099", or that it never reached the topic. */
export function sourceText(rec) {
  if (rec.sourcePartition == null || rec.sourceOffset == null) return T('unprocessed.source_at_write');
  return T('unprocessed.source', { partition: fmtCount(rec.sourcePartition), number: fmtCount(rec.sourceOffset) });
}

/**
 * "5 z 5": the attempts made out of what the topic allows; one check at
 * write. A message becomes unprocessed when its attempts reach the topic's
 * limit of that moment, so a limit lowered since is never shown below them.
 */
export function attemptsText(rec, maxAttempts) {
  if (rec.atWrite) return T('unprocessed.attempts_at_write');
  const made = rec.attempts ?? maxAttempts;
  if (made == null) return '—';
  return T('unprocessed.attempts', { count: fmtCount(made), max: fmtCount(Math.max(made, maxAttempts ?? 0)) });
}

/** "Wiadomość z partycji 2, numer 72 373 099" / "Wiadomość odrzucona przy zapisie dziś 11:40". */
export function messageName(rec, nowMs) {
  if (rec.sourcePartition == null || rec.sourceOffset == null) return T('unprocessed.what_at_write', { when: fmtDayTime(rec.arrivalMs, nowMs) });
  return T('unprocessed.what_source', { partition: fmtCount(rec.sourcePartition), number: fmtCount(rec.sourceOffset) });
}

/**
 * The newest unprocessed messages of several topics as one list. `entries`
 * = `[{ topic, records, hasMore }]`, each topic's newest records. A record
 * older than the oldest loaded one of a topic that has more pages could
 * still have unloaded messages of that topic above it, so it is held back;
 * `pending` names the topic whose next page would let the list go on.
 */
export function mergeNewest(entries) {
  const all = entries.flatMap((e) => e.records || []);
  all.sort((a, b) => b.arrivalMs - a.arrivalMs || a.topic.localeCompare(b.topic) || b.dlqOffset - a.dlqOffset);
  let frontier = -Infinity;
  let pending = null;
  for (const e of entries) {
    if (!e.hasMore || !(e.records || []).length) continue;
    const oldest = Math.min(...e.records.map((r) => r.arrivalMs));
    if (oldest > frontier) {
      frontier = oldest;
      pending = e.topic;
    }
  }
  return { rows: all.filter((r) => r.arrivalMs >= frontier), pending };
}

/** The most frequent reason among `records` (newest first; a tie goes to the newer). */
export function commonReason(records) {
  const counts = new Map();
  for (const r of records || []) counts.set(r.reason, (counts.get(r.reason) || 0) + 1);
  let best = null;
  for (const r of records || []) {
    if (best == null || counts.get(r.reason) > counts.get(best)) best = r.reason;
  }
  return best;
}

/**
 * What "Ponów wszystkie" of one topic would republish: `total` = what waits
 * (the stats snapshot), `records`/`hasMore` = the loaded page. Messages
 * rejected at write stay (the server leaves them). `exact` is false while
 * part of the list is not loaded: the count of those rejected at write among
 * them is not known.
 */
export function retryAllPlan({ total, records, hasMore }) {
  const atWrite = (records || []).filter((r) => r.atWrite).length;
  const retryable = Math.max(0, (hasMore ? Number(total) || 0 : (records || []).length) - atWrite);
  const batch = Math.min(retryable, RETRY_ALL_MAX);
  return { retryable, atWrite, batch, rest: retryable - batch, exact: !hasMore };
}

/** Consumers reading `topic` in the stats snapshot (the broker's own `tf-*` excluded). */
export function topicConsumers(stats, topic) {
  return [...new Set((stats?.groups || []).filter((g) => g.topic === topic && !String(g.group).startsWith('tf-')).map((g) => g.group))].sort();
}

const listFormat = (names) => new Intl.ListFormat(I18n.getLanguage(), { type: 'conjunction' }).format(names);

/** Who may retry and discard in this topic, for a reader who may not. */
export function whoCanRetry(adminLabels) {
  const names = (adminLabels || []).filter(Boolean);
  if (!names.length) return T('unprocessed.who_can_instance');
  return T('unprocessed.who_can_topic', { names: listFormat(names) });
}

/** "Dostaną ją wszyscy odbiorcy tego topiku (a, b) …": who a republished message reaches. */
export function receiversText(consumers, plural = false) {
  const suffix = plural ? '_many' : '';
  if (!consumers.length) return T(`unprocessed.retry.receivers_none${suffix}`);
  if (consumers.length === 1) return T(`unprocessed.retry.receivers_one${suffix}`, { name: consumers[0] });
  return T(`unprocessed.retry.receivers_all${suffix}`, { names: listFormat(consumers) });
}

// ---------------------------------------------------------------------------
// Table rows
// ---------------------------------------------------------------------------

function whenCell(rec, nowMs) {
  return `<span class="tf-table__cell-title">${escapeHtml(fmtDayTime(rec.arrivalMs, nowMs))}</span>`;
}

function reasonCell(rec) {
  const chip = rec.atWrite ? ` <span class="tf-chip tf-chip--outline info">${escapeHtml(T('unprocessed.at_write'))}</span>` : '';
  return `${escapeHtml(reasonLabel(rec.reason))}${chip}`;
}

function consumerCell(rec) {
  return rec.group ? `<span class="tf-table__cell--mono"><span class="tf-table__cell-title">${escapeHtml(rec.group)}</span></span>` : '—';
}

/** A row of the instance list: the topic above where the message sits. */
export function instanceRow(rec, maxAttempts, nowMs) {
  return {
    when: whenCell(rec, nowMs),
    source: `<span class="tf-table__cell--mono"><span class="tf-table__cell-title tf-table__cell-title--strong">${escapeHtml(rec.topic)}</span></span><div class="tf-table__cell-sub">${escapeHtml(sourceText(rec))}</div>`,
    consumer: consumerCell(rec),
    reason: reasonCell(rec),
    attempts: attemptsText(rec, maxAttempts),
    _key: rec.key,
    _topic: rec.topic,
  };
}

/** A row of a topic's section. */
export function sectionRow(rec, maxAttempts, nowMs) {
  return {
    when: whenCell(rec, nowMs),
    source: escapeHtml(sourceText(rec)),
    consumer: consumerCell(rec),
    reason: reasonCell(rec),
    attempts: attemptsText(rec, maxAttempts),
    _key: rec.key,
    _atWrite: rec.atWrite,
  };
}

function columnsHtml(withTopic) {
  return `
    <tf-column key="when" label="${escapeAttr(T('unprocessed.col_when'))}" renderer="html" nowrap></tf-column>
    <tf-column key="source" label="${escapeAttr(T('unprocessed.col_source'))}" renderer="html"${withTopic ? ' fill' : ''}></tf-column>
    <tf-column key="consumer" label="${escapeAttr(T('unprocessed.col_consumer'))}" renderer="html"></tf-column>
    <tf-column key="reason" label="${escapeAttr(T('unprocessed.col_reason'))}" renderer="html"${withTopic ? '' : ' fill'}></tf-column>
    <tf-column key="attempts" label="${escapeAttr(T('unprocessed.col_attempts'))}" nowrap></tf-column>`;
}

function noticeHtml(notice) {
  return notice
    ? `<tf-alert tone="${escapeAttr(notice.tone || 'success')}" title="${escapeAttr(notice.title)}" message="${escapeAttr(notice.text || '')}"></tf-alert>`
    : '';
}

function loadingHtml() {
  return `<div class="tb-state"><tf-spinner size="sm"></tf-spinner>${escapeHtml(T('shell.loading'))}</div>`;
}

function footerHtml(shown, total) {
  return `<span>${T('unprocessed.shown', { shown: `<b>${escapeHtml(fmtCount(shown))}</b>`, total: `<b>${escapeHtml(fmtCount(total))}</b>` })}</span>`;
}

// ---------------------------------------------------------------------------
// The instance tab (T06)
// ---------------------------------------------------------------------------

/**
 * The topics with unprocessed messages, most first: `{ topic, total,
 * lastAtMs }` from the stats snapshot (a topic the reader may not read
 * reports none).
 */
export function unprocessedTopics(stats) {
  return (stats?.topics || [])
    .filter((t) => !isInternalTopic(t.topic) && Number(t.dlqDepth) > 0)
    .map((t) => ({ topic: t.topic, total: Number(t.dlqDepth), lastAtMs: t.dlqLastAtMs ?? null }))
    .sort((a, b) => b.total - a.total || a.topic.localeCompare(b.topic));
}

function instanceSkeleton() {
  return `
    <div data-role="notice"></div>
    <div class="section-card">
      <div class="section-card-head"><div class="title">${sprite('share')} ${escapeHtml(T('unprocessed.where_title'))} <span data-role="topic-count"></span></div></div>
      <div class="section-sub">${escapeHtml(T('unprocessed.explain'))}</div>
      <div class="tb-unp-tiles" data-role="tiles"></div>
    </div>
    <div class="section-card">
      <div class="section-card-head">
        <div class="title">${sprite('inbox')} ${escapeHtml(T('unprocessed.list_title'))} <span data-role="list-count"></span></div>
        <div class="actions muted" data-role="list-hint"></div>
      </div>
      <div data-role="list-state"></div>
      <tf-table data-role="table">${columnsHtml(true)}</tf-table>
      <div class="tb-table-footer tb-unp-footer">
        <div data-role="footer"></div>
        <tf-button variant="secondary" size="sm" data-go="more" data-role="more" hidden>${escapeHtml(T('unprocessed.more'))}</tf-button>
      </div>
    </div>`;
}

function emptyHtml(message) {
  return `
    <div data-role="notice"></div>
    <div class="section-card">
      <div class="section-card-head"><div class="title">${sprite('inbox')} ${escapeHtml(T('unprocessed.section_title'))} <tf-chip size="sm" variant="outline" status="neutral" label="${escapeAttr(fmtCount(0))}"></tf-chip></div></div>
      <tf-empty-state badge icon="check" title="${escapeAttr(T('unprocessed.empty_title'))}" message="${escapeAttr(message)}"></tf-empty-state>
    </div>`;
}

function tileHtml(topic) {
  return `
    <div class="job-row clickable tb-unp-tile" role="link" tabindex="0" data-go="topic" data-topic="${escapeAttr(topic)}">
      <div class="job-ico">${sprite('inbox')}</div>
      <div class="job-main">
        <div class="job-name"><span class="mono">${escapeHtml(topic)}</span><tf-chip size="sm" variant="outline" status="warn" data-role="count"></tf-chip></div>
        <div class="job-sub" data-role="sub"></div>
        <div class="tb-unp-who" data-role="who" hidden>${sprite('lock')}<span></span></div>
      </div>
      <tf-button variant="secondary" size="sm" icon="refresh" data-go="retry-all" data-topic="${escapeAttr(topic)}" data-role="retry-all" hidden></tf-button>
    </div>`;
}

/**
 * Draws or repaints the tab from `ctx.view()` = `{ stats, byTopic, access,
 * shown, notice, error, errorKind, instanceLabel, nowMs }`: `byTopic` maps a
 * topic to its loaded `{ records, hasMore, error }` (`null` while loading),
 * `access` a topic to `{ canAdmin, adminLabels, maxAttempts }`.
 * `ctx.go(action)`: `{ kind: 'topic', topic }`, `{ kind: 'retry-all',
 * topic }`, `{ kind: 'more' }`, `{ kind: 'retry' }`.
 */
export function drawUnprocessed(body, ctx) {
  const view = ctx.view();
  const topics = unprocessedTopics(view.stats);
  let mode = 'list';
  if (!view.stats) mode = view.error ? `error:${view.errorKind}` : 'loading';
  else if (!topics.length) mode = 'empty';
  if (body.__tbMode !== mode) {
    body.__tbMode = mode;
    if (mode === 'loading') patchHtml(body, loadingHtml());
    else if (mode.startsWith('error:')) patchHtml(body, loadErrorHtml({ kind: view.errorKind, instanceLabel: view.instanceLabel, titleKey: 'unprocessed.error_title' }));
    else if (mode === 'empty') patchHtml(body, emptyHtml(T('unprocessed.empty_instance')));
    else {
      patchHtml(body, instanceSkeleton());
      body.querySelector('[data-role="table"]').addEventListener('row-click', (e) => ctx.go({ kind: 'topic', topic: e.detail.row._topic }));
    }
    if (!body.__tbWired) {
      body.__tbWired = true;
      body.addEventListener('click', (e) => {
        const el = e.target.closest('[data-go]');
        if (!el || !body.contains(el) || el.hasAttribute('disabled')) return;
        if (el.dataset.go === 'retry-all') e.stopPropagation();
        go(ctx, el);
      });
      body.addEventListener('keydown', (e) => {
        if (e.key !== 'Enter' && e.key !== ' ') return;
        const row = e.target.closest?.('[role="link"][data-go]');
        if (!row || !body.contains(row)) return;
        e.preventDefault();
        go(ctx, row);
      });
    }
  }
  const noticeHost = body.querySelector('[data-role="notice"]');
  if (noticeHost) patchHtml(noticeHost, noticeHtml(view.notice));
  if (mode === 'list') paintInstance(body, view, topics);
}

function go(ctx, el) {
  const d = el.dataset;
  if (d.go === 'topic' || d.go === 'retry-all') ctx.go({ kind: d.go, topic: d.topic });
  else ctx.go({ kind: d.go });
}

function paintInstance(body, view, topics) {
  const { byTopic, access, nowMs } = view;
  const topicCount = body.querySelector('[data-role="topic-count"]');
  patchHtml(topicCount, '<tf-chip size="sm" variant="outline" status="neutral"></tf-chip>');
  setAttr(topicCount.firstElementChild, 'label', fmtCount(topics.length));

  const tiles = body.querySelector('[data-role="tiles"]');
  patchKeyedList(tiles, topics.map((t) => ({ key: t.topic, html: tileHtml(t.topic) })));
  topics.forEach((t, i) => {
    const el = tiles.children[i];
    if (!el) return;
    const loaded = byTopic.get(t.topic) || null;
    const acc = access.get(t.topic) || null;
    setAttr(el.querySelector('[data-role="count"]'), 'label', fmtCount(t.total));
    const parts = [];
    const records = loaded?.records || [];
    const reason = commonReason(records);
    if (reason) {
      parts.push(records.length < t.total
        ? T('unprocessed.tile_common_recent', { count: fmtCount(records.length), reason: reasonLabel(reason).toLowerCase() })
        : T('unprocessed.tile_common', { reason: reasonLabel(reason).toLowerCase() }));
    }
    if (t.lastAtMs != null) parts.push(T('unprocessed.tile_last', { when: fmtDayTime(t.lastAtMs, nowMs) }));
    setText(el.querySelector('[data-role="sub"]'), parts.join(' · '));
    const btn = el.querySelector('[data-role="retry-all"]');
    const who = el.querySelector('[data-role="who"]');
    const plan = loaded ? retryAllPlan({ total: t.total, records, hasMore: loaded.hasMore }) : null;
    const canAdmin = acc?.canAdmin === true;
    btn.hidden = !(canAdmin && plan && plan.retryable > 0);
    setText(btn, plan?.exact ? T('unprocessed.retry_all_n', { count: fmtCount(plan.retryable) }) : T('unprocessed.retry_all'));
    who.hidden = !(acc && !canAdmin);
    if (acc && !canAdmin) setText(who.querySelector('span'), whoCanRetry(acc.adminLabels));
  });

  const entries = topics.map((t) => ({ topic: t.topic, records: byTopic.get(t.topic)?.records || [], hasMore: Boolean(byTopic.get(t.topic)?.hasMore) }));
  const { rows, pending } = mergeNewest(entries);
  const total = topics.reduce((s, t) => s + t.total, 0);
  const loading = topics.some((t) => !byTopic.has(t.topic) || byTopic.get(t.topic) == null);
  const failed = topics.map((t) => byTopic.get(t.topic)?.error).find(Boolean) || null;
  const shownRows = rows.slice(0, view.shown);
  const table = body.querySelector('[data-role="table"]');
  const stateHost = body.querySelector('[data-role="list-state"]');
  let stateHtml = '';
  if (!shownRows.length && loading) stateHtml = loadingHtml();
  else if (failed && !shownRows.length) stateHtml = `<div class="tb-state tb-state--error">${sprite('alert')}<span>${escapeHtml(failed)}</span><tf-button variant="secondary" size="sm" icon="refresh" data-go="retry">${escapeHtml(T('unprocessed.retry_load'))}</tf-button></div>`;
  patchHtml(stateHost, stateHtml);
  table.hidden = shownRows.length === 0;
  setRowsIfChanged(table, shownRows.map((r) => instanceRow(r, access.get(r.topic)?.maxAttempts ?? null, nowMs)));
  const count = body.querySelector('[data-role="list-count"]');
  patchHtml(count, '<tf-chip size="sm" variant="outline" status="neutral"></tf-chip>');
  setAttr(count.firstElementChild, 'label', fmtCount(total));
  // A reader who administers none of these topics can look, not retry.
  const anyAdmin = topics.some((t) => access.get(t.topic)?.canAdmin === true);
  setText(body.querySelector('[data-role="list-hint"]'), T(anyAdmin ? 'unprocessed.list_hint' : 'unprocessed.list_hint_read'));
  patchHtml(body.querySelector('[data-role="footer"]'), shownRows.length ? footerHtml(shownRows.length, total) : '');
  const more = body.querySelector('[data-role="more"]');
  more.hidden = !(rows.length > shownRows.length || (pending && shownRows.length < total));
  setAttr(more, 'disabled', loading && rows.length <= shownRows.length);
}

// ---------------------------------------------------------------------------
// A topic's section
// ---------------------------------------------------------------------------

function sectionSkeleton() {
  return `
    <div data-role="notice"></div>
    <div data-role="who"></div>
    <div class="section-card" data-role="card">
      <div class="section-card-head">
        <div class="title">${sprite('inbox')} ${escapeHtml(T('unprocessed.section_title'))} <span data-role="count"></span></div>
        <div class="actions" data-role="head-actions"></div>
      </div>
      <div class="section-sub" data-role="explain">${escapeHtml(T('unprocessed.explain'))}</div>
      <div data-role="list-state"></div>
      <tf-table data-role="table">${columnsHtml(false)}</tf-table>
      <div class="tb-table-footer tb-unp-footer" data-role="footer-row">
        <div data-role="footer"></div>
        <tf-button variant="secondary" size="sm" data-go="unp-more" data-role="more" hidden>${escapeHtml(T('unprocessed.more'))}</tf-button>
      </div>
    </div>`;
}

/**
 * Paints the Nieprzetworzone section of a topic's page. `view` carries the
 * page's `topic`, `access`, `adminLabels`, `stats`, `notice`, `nowMs` and
 * `unprocessed` = `{ records, hasMore, error } | null` plus `shown`.
 * The row buttons call `ctx.go({ kind: 'unp-view' | 'unp-retry' |
 * 'unp-discard', key })`; the header's `{ kind: 'unp-retry-all' }`.
 */
export function paintUnprocessedSection(host, view, ctx) {
  if (host.__tbUnp !== 'built') {
    host.__tbUnp = 'built';
    patchHtml(host, sectionSkeleton());
  }
  const { topic, access, stats, nowMs } = view;
  const canAdmin = Boolean(access.canAdmin);
  const loaded = view.unprocessed;
  const total = Number((stats?.topics || []).find((t) => t.topic === topic.name)?.dlqDepth) || 0;
  const records = loaded?.records || [];
  const maxAttempts = Number(topic.maxDeliveryAttempts) || null;
  patchHtml(host.querySelector('[data-role="notice"]'), noticeHtml(view.notice));
  patchHtml(host.querySelector('[data-role="who"]'), canAdmin || total === 0 ? '' : `<div class="tb-who-can">${sprite('lock')}<span>${escapeHtml(whoCanRetry(view.adminLabels))}</span></div>`);

  const count = host.querySelector('[data-role="count"]');
  patchHtml(count, '<tf-chip size="sm" variant="outline" status="neutral"></tf-chip>');
  setAttr(count.firstElementChild, 'label', fmtCount(total));
  const plan = loaded ? retryAllPlan({ total, records, hasMore: loaded.hasMore }) : null;
  const showRetryAll = canAdmin && plan && plan.retryable > 0 && total > 0;
  patchHtml(host.querySelector('[data-role="head-actions"]'), showRetryAll
    ? `<tf-button variant="primary" size="sm" icon="refresh" data-go="unp-retry-all">${escapeHtml(plan.exact ? T('unprocessed.retry_all_n', { count: fmtCount(plan.retryable) }) : T('unprocessed.retry_all'))}</tf-button>`
    : '');

  const empty = loaded && !loaded.error && total === 0 && records.length === 0;
  const stateHost = host.querySelector('[data-role="list-state"]');
  let stateHtml = '';
  if (empty) stateHtml = `<tf-empty-state badge icon="check" title="${escapeAttr(T('unprocessed.empty_title'))}" message="${escapeAttr(T('unprocessed.empty_topic'))}"></tf-empty-state>`;
  else if (!loaded) stateHtml = loadingHtml();
  else if (loaded.error && !records.length) stateHtml = `<div class="tb-state tb-state--error">${sprite('alert')}<span>${escapeHtml(loaded.error)}</span><tf-button variant="secondary" size="sm" icon="refresh" data-go="unp-reload">${escapeHtml(T('unprocessed.retry_load'))}</tf-button></div>`;
  patchHtml(stateHost, stateHtml);
  host.querySelector('[data-role="explain"]').hidden = empty;

  const shownRows = records.slice(0, view.shown);
  const table = host.querySelector('[data-role="table"]');
  table.hidden = shownRows.length === 0;
  table.rowActionsKey = (row) => `${row._key}|${row._atWrite}|${canAdmin}`;
  if (table.__tbAdmin !== canAdmin) {
    table.__tbAdmin = canAdmin;
    table.rowActions = (row, idx, currentRow) => {
      const live = () => currentRow?.() ?? row;
      const wrap = document.createElement('div');
      wrap.className = 'tf-table__row-actions';
      const add = (kind, label) => {
        const b = document.createElement('tf-button');
        b.setAttribute('variant', 'secondary');
        b.setAttribute('size', 'sm');
        b.dataset.act = kind;
        b.textContent = label;
        b.addEventListener('click', (e) => { e.stopPropagation(); ctx.go({ kind: `unp-${kind}`, key: live()._key }); });
        wrap.appendChild(b);
      };
      add('view', T('unprocessed.action_view'));
      if (canAdmin && !row._atWrite) add('retry', T('unprocessed.action_retry'));
      if (canAdmin) add('discard', T('unprocessed.action_discard'));
      return wrap;
    };
    table.__tfRowsSig = null;
  }
  setRowsIfChanged(table, shownRows.map((r) => sectionRow(r, maxAttempts, nowMs)));
  host.querySelector('[data-role="footer-row"]').hidden = shownRows.length === 0;
  patchHtml(host.querySelector('[data-role="footer"]'), shownRows.length ? footerHtml(shownRows.length, Math.max(total, records.length)) : '');
  const more = host.querySelector('[data-role="more"]');
  more.hidden = !(records.length > shownRows.length || (loaded?.hasMore && shownRows.length < total));
}

// ===== File: modules/tentabus/topics.js — the Topiki tab (T02): the instance's topics with search, filters, row actions and a summary footer =====
//
// One row per topic of the reader's (the broker's own `__*` topics never
// reach this list), joined with the live stats snapshot: incoming rate, what
// waits for the topic's consumers, its unprocessed messages and its size on
// disk. "Opóźnione" are the topics whose consumer falls behind or is paused
// with work waiting — the same thresholds the overview alerts use, so a topic
// counted here is the one Przegląd warns about. The eye and bin in a row open
// their window over this list without opening the row; the row itself opens
// the topic. The shell passes those moves in as `ctx.go`.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { setAttr, patchHtml } from '/js/lib/dom-patch.js';
import { T, fmtCount, fmtBytes, fmtRetention, contentTypeLabel } from '/js/modules/tentabus/format.js';
import { isLagging, isPausedWithBacklog } from '/js/modules/tentabus/alerts.js';
import { userTopics } from '/js/modules/tentabus/model.js';
import { loadErrorHtml } from '/js/modules/tentabus/overview.js';
import '/js/components/tf-table.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-spinner.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

export const TOPIC_FILTERS = ['all', 'delayed', 'dlq'];

/**
 * The list rows: each topic of the reader's with its stats. A topic the
 * snapshot does not list yet (created a moment ago) shows zeros, not dashes —
 * a new topic has no traffic, nothing waiting and nothing on disk.
 */
export function topicRows({ topics, stats, nowMs = Date.now() }) {
  const statsByTopic = new Map((stats?.topics || []).map((t) => [t.topic, t]));
  const groups = stats?.groups || [];
  return userTopics(topics).map((t) => {
    const s = statsByTopic.get(t.name);
    const readers = groups.filter((g) => g.topic === t.name);
    return {
      name: t.name,
      contentLabel: contentTypeLabel(t.contentType),
      schemaId: t.schemaId || '',
      rate: Number(s?.msgsInPerSec) || 0,
      waiting: Number(s?.totalLag) || 0,
      delayed: readers.some((g) => isLagging(g, nowMs) || isPausedWithBacklog(g)),
      dlq: Number(s?.dlqDepth) || 0,
      bytes: Number(s?.totalBytesOnDisk) || 0,
      partitions: Number(t.partitions) || 0,
      replicas: Number(t.replicationFactor) || 0,
      retentionMs: Number(t.retentionMs) || 0,
    };
  });
}

/** Rows of one filter + search (by name), and the per-filter counts of the segmented control. */
export function filterTopicRows(rows, { filter = 'all', query = '' } = {}) {
  const list = rows || [];
  const q = String(query || '').trim().toLowerCase();
  const counts = {
    all: list.length,
    delayed: list.filter((r) => r.delayed).length,
    dlq: list.filter((r) => r.dlq > 0).length,
  };
  const inFilter = (r) => filter === 'all' || (filter === 'delayed' ? r.delayed : r.dlq > 0);
  return {
    rows: list.filter((r) => inFilter(r) && (!q || r.name.toLowerCase().includes(q))).sort((a, b) => a.name.localeCompare(b.name)),
    counts,
  };
}

/** The footer under the table: totals of the rows it shows. */
export function topicsFooter(rows) {
  return (rows || []).reduce((acc, r) => ({
    topics: acc.topics + 1,
    partitions: acc.partitions + r.partitions,
    rate: acc.rate + r.rate,
    bytes: acc.bytes + r.bytes,
  }), { topics: 0, partitions: 0, rate: 0, bytes: 0 });
}

/** "HL7 v2 · wzór wynik-badania" / "JSON · bez wzoru": what the topic carries and what checks it. */
export function topicSubline(row) {
  const schema = row.schemaId ? T('topics.sub_schema', { name: row.schemaId }) : T('topics.sub_no_schema');
  return row.contentLabel ? `${row.contentLabel} · ${schema}` : schema;
}

// Cell markup lives in the tf-table shadow root: only controls.css classes apply there.
function tableRow(r) {
  const waiting = r.delayed && r.waiting > 0
    ? `<span class="tf-chip tf-chip--outline warn">${escapeHtml(T('topics.waiting_chip', { count: fmtCount(r.waiting) }))}</span>`
    : escapeHtml(fmtCount(r.waiting));
  const dlq = r.dlq > 0
    ? `<span class="tf-chip tf-chip--outline warn">${sprite('inbox')} ${escapeHtml(fmtCount(r.dlq))}</span>`
    : escapeHtml(fmtCount(0));
  return {
    name: `<span class="tf-table__cell--mono"><span class="tf-table__cell-title">${escapeHtml(r.name)}</span></span><div class="tf-table__cell-sub">${escapeHtml(topicSubline(r))}</div>`,
    rate: fmtCount(Math.round(r.rate)),
    waiting,
    dlq,
    bytes: fmtBytes(r.bytes),
    partitions: fmtCount(r.partitions),
    replicas: fmtCount(r.replicas),
    retention: fmtRetention(r.retentionMs),
    _key: r.name,
    _topic: r.name,
  };
}

function loadingHtml() {
  return `<div class="tb-state"><tf-spinner size="sm"></tf-spinner>${escapeHtml(T('shell.loading'))}</div>`;
}

function noticeHtml() {
  return '<div data-role="notice"></div>';
}

function emptyHtml(canAdmin) {
  return `
    ${noticeHtml()}
    <div class="section-card">
      <div class="section-card-head"><div class="title">${sprite('share')} ${escapeHtml(T('topics.title'))} <tf-chip size="sm" variant="outline" status="neutral" label="${escapeAttr(fmtCount(0))}"></tf-chip></div></div>
      <tf-empty-state badge icon="share" title="${escapeAttr(T('topics.empty_title'))}" message="${escapeAttr(T('topics.empty_sub'))}">
        ${canAdmin ? `<tf-button variant="primary" icon="plus" data-go="create">${escapeHtml(T('topics.create'))}</tf-button>` : ''}
      </tf-empty-state>
      ${canAdmin ? '' : `<div class="muted tb-admin-note">${sprite('lock')} ${escapeHtml(T('topics.admin_only'))}</div>`}
    </div>`;
}

function listHtml(canAdmin) {
  return `
    ${noticeHtml()}
    <div class="tf-toolbar tb-topics-toolbar">
      <tf-searchbox data-role="search" placeholder="${escapeAttr(T('topics.search'))}" debounce="150"></tf-searchbox>
      <tf-segmented data-role="filter" size="md" value="all"></tf-segmented>
      <span class="tf-toolbar-spacer"></span>
      ${canAdmin ? `<tf-button variant="primary" icon="plus" data-go="create">${escapeHtml(T('topics.create'))}</tf-button>` : ''}
    </div>
    <div class="section-card">
      <div class="section-card-head">
        <div class="title">${sprite('share')} ${escapeHtml(T('topics.title'))} <tf-chip size="sm" variant="outline" status="neutral" data-role="count"></tf-chip></div>
        <div class="actions muted" data-role="row-hint">${escapeHtml(T('topics.row_hint'))}</div>
      </div>
      ${canAdmin ? '' : `<div class="muted tb-admin-note">${sprite('lock')} ${escapeHtml(T('topics.admin_only'))}</div>`}
      <tf-table data-role="table">
        <tf-column key="name" label="${escapeAttr(T('topics.col_name'))}" renderer="html" fill></tf-column>
        <tf-column key="rate" label="${escapeAttr(T('topics.col_rate'))}" renderer="num"></tf-column>
        <tf-column key="waiting" label="${escapeAttr(T('topics.col_waiting'))}" renderer="html" align="num"></tf-column>
        <tf-column key="dlq" label="${escapeAttr(T('topics.col_dlq'))}" renderer="html" align="num"></tf-column>
        <tf-column key="bytes" label="${escapeAttr(T('topics.col_bytes'))}" align="num"></tf-column>
        <tf-column key="partitions" label="${escapeAttr(T('topics.col_partitions'))}" renderer="num"></tf-column>
        <tf-column key="replicas" label="${escapeAttr(T('topics.col_replicas'))}" renderer="num"></tf-column>
        <tf-column key="retention" label="${escapeAttr(T('topics.col_retention'))}"></tf-column>
      </tf-table>
      <div class="muted" data-role="no-match" hidden>${escapeHtml(T('topics.no_match'))}</div>
      <div class="tb-table-footer" data-role="footer"></div>
    </div>`;
}

// The row's own buttons: preview, delete (admins only) and the arrow that
// opens it. Each stops the click so it acts instead of opening the row.
function rowActions(ctx, canAdmin) {
  return (row, idx, currentRow) => {
    const live = () => currentRow?.() ?? row;
    const wrap = document.createElement('div');
    wrap.className = 'tf-table__row-actions';
    const add = (icon, label, kind) => {
      const b = document.createElement('tf-button');
      b.setAttribute('variant', 'ghost');
      b.setAttribute('size', 'sm');
      b.setAttribute('icon', icon);
      b.setAttribute('aria-label', label);
      b.title = label;
      b.dataset.act = kind;
      b.addEventListener('click', (e) => { e.stopPropagation(); ctx.go({ kind, topic: live()._topic }); });
      wrap.appendChild(b);
    };
    add('eye', T('topics.action_preview'), 'preview');
    if (canAdmin) add('trash', T('topics.action_delete'), 'delete');
    add('chevron-right', T('topics.action_open'), 'open');
    return wrap;
  };
}

/**
 * Draws or repaints the tab from `ctx.view()` = `{ topics, error, errorKind,
 * stats, instanceLabel, canAdmin, notice, nowMs }` (`topics` is `null` until
 * the list answered). `ctx.go(action)`: `{ kind: 'open'|'preview'|'delete',
 * topic }`, `{ kind: 'create' }`, `{ kind: 'retry' }`. The filter and the
 * search survive a repaint (they live on the body).
 */
export function drawTopics(body, ctx) {
  const view = ctx.view();
  const { topics, error, errorKind, instanceLabel, canAdmin } = view;
  let mode = 'list';
  if (topics == null) mode = error ? `error:${errorKind}` : 'loading';
  else if (userTopics(topics).length === 0) mode = 'empty';
  const modeKey = `${mode}:${canAdmin ? 'admin' : 'reader'}`;
  if (body.__tbMode !== modeKey) {
    body.__tbMode = modeKey;
    if (mode === 'loading') patchHtml(body, loadingHtml());
    else if (mode.startsWith('error:')) patchHtml(body, loadErrorHtml({ kind: errorKind, instanceLabel, titleKey: 'topics.error_title' }));
    else if (mode === 'empty') patchHtml(body, emptyHtml(canAdmin));
    else {
      patchHtml(body, listHtml(canAdmin));
      body.__tbFilter = body.__tbFilter || 'all';
      body.__tbQuery = body.__tbQuery || '';
      const table = body.querySelector('[data-role="table"]');
      table.rowActions = rowActions(ctx, canAdmin);
      table.rowActionsKey = (row) => row._topic;
      table.addEventListener('row-click', (e) => ctx.go({ kind: 'open', topic: e.detail.row._topic }));
      body.querySelector('[data-role="filter"]').addEventListener('change', (e) => { body.__tbFilter = e.detail?.value || 'all'; paintList(body, ctx.view()); });
      const search = body.querySelector('[data-role="search"]');
      search.value = body.__tbQuery;
      search.addEventListener('search', (e) => { body.__tbQuery = e.detail?.value || ''; paintList(body, ctx.view()); });
    }
    if (!body.__tbWired) {
      body.__tbWired = true;
      body.addEventListener('click', (e) => {
        if (e.target.closest('[data-go="retry"]')) ctx.go({ kind: 'retry' });
        else if (e.target.closest('[data-go="create"]')) ctx.go({ kind: 'create' });
      });
    }
  }
  paintNotice(body, view.notice);
  if (mode === 'list') paintList(body, view);
}

function paintNotice(body, notice) {
  const host = body.querySelector('[data-role="notice"]');
  if (!host) return;
  const sig = notice ? JSON.stringify(notice) : '';
  if (host.__tbSig === sig) return;
  host.__tbSig = sig;
  host.innerHTML = notice
    ? `<tf-alert tone="${escapeAttr(notice.tone || 'success')}" title="${escapeAttr(notice.title)}" message="${escapeAttr(notice.text || '')}"></tf-alert>`
    : '';
}

function paintList(body, view) {
  const all = topicRows({ topics: view.topics, stats: view.stats, nowMs: view.nowMs });
  const { rows, counts } = filterTopicRows(all, { filter: body.__tbFilter, query: body.__tbQuery });
  // Search and filters only earn their place once there is something to narrow.
  const toolbarUseful = all.length > 1;
  body.querySelector('[data-role="search"]').hidden = !toolbarUseful;
  const seg = body.querySelector('[data-role="filter"]');
  seg.hidden = !toolbarUseful;
  const countsSig = JSON.stringify(counts);
  if (seg.__tbCounts !== countsSig) {
    seg.__tbCounts = countsSig;
    seg.setOptions(TOPIC_FILTERS.map((f) => ({ value: f, label: `${T(`topics.filter_${f}`)} ${fmtCount(counts[f])}` })), body.__tbFilter);
  }
  setAttr(body.querySelector('[data-role="count"]'), 'label', fmtCount(rows.length));
  const table = body.querySelector('[data-role="table"]');
  const next = rows.map(tableRow);
  const sig = JSON.stringify(next);
  if (table.__tbSig !== sig) {
    table.__tbSig = sig;
    table.rows = next;
  }
  table.hidden = next.length === 0;
  body.querySelector('[data-role="no-match"]').hidden = next.length > 0;
  body.querySelector('[data-role="row-hint"]').hidden = next.length === 0;
  const f = topicsFooter(rows);
  const footer = body.querySelector('[data-role="footer"]');
  footer.hidden = next.length === 0;
  const footerHtml = [
    T('topics.footer_topics', { count: `<b>${escapeHtml(fmtCount(f.topics))}</b>`, n: f.topics }),
    T('topics.footer_partitions', { count: `<b>${escapeHtml(fmtCount(f.partitions))}</b>`, n: f.partitions }),
    T('topics.footer_rate', { count: `<b>${escapeHtml(fmtCount(Math.round(f.rate)))}</b>` }),
    T('topics.footer_bytes', { size: `<b>${escapeHtml(fmtBytes(f.bytes))}</b>` }),
  ].map((part) => `<span>${part}</span>`).join('');
  if (footer.__tbSig !== footerHtml) {
    footer.__tbSig = footerHtml;
    footer.innerHTML = footerHtml;
  }
}

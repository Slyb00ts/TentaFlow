// ===== File: modules/tentabus/consumers.js — the Odbiorcy tab (T05): who reads which topic, how much waits, pause and resume in the row =====
//
// A consumer ("odbiorca") is a group of programs reading one topic: every
// program of the group gets a different share of the messages, and the group
// remembers how far it has read. The list is the server's consumer list
// (`GroupListResponse`: how the program confirms, when the row last changed,
// whether the reader may change it) joined with the live stats snapshot,
// which moves the waiting count and the paused state every few seconds.
//
// "Opóźnieni" are the consumers with anything waiting — the same rule as the
// overview's "Odbiorcy z opóźnieniem" tile, so the two counts never differ.
// Pause and resume act from the row without opening it (and then show the
// consumer's page with the result); the row itself opens the consumer. The
// shell passes those moves in as `ctx.go`.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { setAttr, patchHtml } from '/js/lib/dom-patch.js';
import { T, fmtCount, fmtDayTime, consumerLabel, isKeyGroup } from '/js/modules/tentabus/format.js';
import { isLagging, isPausedWithBacklog } from '/js/modules/tentabus/alerts.js';
import { loadErrorHtml } from '/js/modules/tentabus/overview.js';
import '/js/components/tf-table.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-spinner.js';
import '/js/components/tf-progress-bar.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

/** How a consumer's program confirms a message (`bus::groups::CommitMode`), in the order the legend explains them. */
export const COMMIT_MODES = ['auto_after_success', 'explicit', 'at_most_once'];
export const CONSUMER_FILTERS = ['all', 'delayed', 'paused'];

/** The one key of a consumer: the same group name reading another topic is another consumer. */
export const consumerKey = (group, topic) => `${group}\u0000${topic}`;

/** "po udanym przetworzeniu" — how the program confirms, in plain words. */
export function commitModeLabel(mode) {
  return T(`consumers.commit.${COMMIT_MODES.includes(mode) ? mode : 'unknown'}`);
}

/** One sentence of what that way of confirming means for a message. */
export function commitModeHint(mode) {
  return T(`consumers.commit_hint.${COMMIT_MODES.includes(mode) ? mode : 'unknown'}`);
}

const measured = (v) => v != null && Number.isFinite(Number(v));

/**
 * The list rows: every consumer of the server's list, with the snapshot's
 * newer waiting count and paused state when the snapshot already lists it.
 * A waiting count the node cannot measure stays `null` — never a zero.
 */
export function consumerRows({ groups, stats, nowMs = Date.now() }) {
  const live = new Map((stats?.groups || []).map((g) => [consumerKey(g.group, g.topic), g]));
  return (groups || []).map((g) => {
    const s = live.get(consumerKey(g.group, g.topic));
    const paused = Boolean(s ? s.paused : g.paused);
    const lagTotal = s ? s.lagTotal : g.lagTotal;
    const merged = { ...g, ...(s || {}), paused, lagTotal };
    return {
      group: g.group,
      label: consumerLabel(g),
      keyGroup: isKeyGroup(g),
      keyGone: Boolean(g.keyGone),
      topic: g.topic,
      commitMode: g.commitMode || '',
      paused,
      waiting: measured(lagTotal) ? Number(lagTotal) : null,
      lagging: isLagging(merged, nowMs) || isPausedWithBacklog(merged),
      updatedAtMs: Number(g.updatedAtMs) || null,
      canAdmin: g.canAdmin === true,
    };
  });
}

/** Rows of one filter + search (consumer or topic name), and the per-filter counts. */
export function filterConsumerRows(rows, { filter = 'all', query = '' } = {}) {
  const list = rows || [];
  const q = String(query || '').trim().toLowerCase();
  const delayed = (r) => !r.keyGone && r.waiting != null && r.waiting > 0;
  const counts = {
    all: list.length,
    delayed: list.filter(delayed).length,
    paused: list.filter((r) => r.paused).length,
  };
  const inFilter = (r) => filter === 'all' || (filter === 'delayed' ? delayed(r) : r.paused);
  const matches = (r) => !q || r.group.toLowerCase().includes(q) || r.label.toLowerCase().includes(q) || r.topic.toLowerCase().includes(q);
  return {
    rows: list.filter((r) => inFilter(r) && matches(r))
      .sort((a, b) => a.group.localeCompare(b.group) || a.topic.localeCompare(b.topic)),
    counts,
  };
}

/** The footer under the table: totals of the rows it shows. */
export function consumersFooter(rows) {
  return (rows || []).reduce((acc, r) => ({
    consumers: acc.consumers + 1,
    waiting: acc.waiting + (r.keyGone ? 0 : r.waiting || 0),
    paused: acc.paused + (r.paused ? 1 : 0),
  }), { consumers: 0, waiting: 0, paused: 0 });
}

// Cell markup lives in the tf-table shadow root: only controls.css classes apply there.
function tableRow(r, maxWaiting, nowMs) {
  const share = maxWaiting > 0 && r.waiting != null ? Math.round((r.waiting / maxWaiting) * 100) : 0;
  const tone = r.lagging ? 'warning' : 'accent';
  const state = r.paused
    ? `<span class="tf-chip tf-chip--outline warn">${escapeHtml(T('consumers.state_paused'))}</span>`
    : `<span class="tf-chip tf-chip--outline ok">${escapeHtml(T('consumers.state_running'))}</span>`;
  return {
    group: r.keyGroup
      ? `<span class="tf-table__cell-title">${escapeHtml(r.label)}</span><div class="tf-table__cell-sub">${escapeHtml(T(r.keyGone ? 'consumers.key_gone_sub' : 'consumers.key_sub'))}</div>`
      : `<span class="tf-table__cell--mono"><span class="tf-table__cell-title">${escapeHtml(r.group)}</span></span>`,
    topic: `<span class="tf-table__cell--mono">${escapeHtml(r.topic)}</span>`,
    commit: commitModeLabel(r.commitMode),
    state,
    waiting: r.waiting == null
      ? '—'
      : `<div class="tf-table__cell-title">${escapeHtml(fmtCount(r.waiting))}</div><tf-progress-bar size="sm" value="${share}" tone="${tone}"></tf-progress-bar>`,
    changed: fmtDayTime(r.updatedAtMs, nowMs),
    _key: consumerKey(r.group, r.topic),
    _group: r.group,
    _topic: r.topic,
    _paused: r.paused,
    _canAdmin: r.canAdmin,
  };
}

function loadingHtml() {
  return `<div class="tb-state"><tf-spinner size="sm"></tf-spinner>${escapeHtml(T('shell.loading'))}</div>`;
}

function legendHtml() {
  return `
    <div class="legend-grid tb-commit-legend">
      ${COMMIT_MODES.map((m) => `
        <div class="legend-item">
          <div class="li-name">${escapeHtml(T(`consumers.legend.${m}`))}</div>
          <div class="li-sub">${escapeHtml(commitModeHint(m))}</div>
        </div>`).join('')}
    </div>`;
}

function emptyHtml() {
  return `
    <div data-role="notice"></div>
    <div class="section-card">
      <div class="section-card-head"><div class="title">${sprite('users')} ${escapeHtml(T('consumers.title'))} <tf-chip size="sm" variant="outline" status="neutral" label="${escapeAttr(fmtCount(0))}"></tf-chip></div></div>
      <tf-empty-state badge icon="users" title="${escapeAttr(T('consumers.empty_title'))}" message="${escapeAttr(T('consumers.empty_sub'))}"></tf-empty-state>
    </div>`;
}

function listHtml() {
  return `
    <div data-role="notice"></div>
    <div class="tf-toolbar tb-consumers-toolbar">
      <tf-searchbox data-role="search" placeholder="${escapeAttr(T('consumers.search'))}" debounce="150"></tf-searchbox>
      <tf-segmented data-role="filter" size="md" value="all"></tf-segmented>
    </div>
    <div class="section-card">
      <div class="section-card-head">
        <div class="title">${sprite('users')} ${escapeHtml(T('consumers.title'))} <tf-chip size="sm" variant="outline" status="neutral" data-role="count"></tf-chip></div>
        <div class="actions muted" data-role="row-hint">${escapeHtml(T('consumers.row_hint'))}</div>
      </div>
      <div class="section-sub">${escapeHtml(T('consumers.explain'))}</div>
      <div class="muted tb-admin-note" data-role="admin-note" hidden>${sprite('lock')} <span></span></div>
      <tf-table data-role="table">
        <tf-column key="group" label="${escapeAttr(T('consumers.col_group'))}" renderer="html" fill></tf-column>
        <tf-column key="topic" label="${escapeAttr(T('consumers.col_topic'))}" renderer="html"></tf-column>
        <tf-column key="commit" label="${escapeAttr(T('consumers.col_commit'))}"></tf-column>
        <tf-column key="state" label="${escapeAttr(T('consumers.col_state'))}" renderer="html"></tf-column>
        <tf-column key="waiting" label="${escapeAttr(T('consumers.col_waiting'))}" renderer="html" align="num"></tf-column>
        <tf-column key="changed" label="${escapeAttr(T('consumers.col_changed'))}" hide-below="1100"></tf-column>
      </tf-table>
      <div class="muted" data-role="no-match" hidden>${escapeHtml(T('consumers.no_match'))}</div>
      <div class="tb-table-footer" data-role="footer"></div>
    </div>
    ${legendHtml()}`;
}

// Pause / resume for a reader allowed to change the consumer, then the arrow
// that opens it. Each stops the click so it acts instead of opening the row.
function rowActions(ctx) {
  return (row, idx, currentRow) => {
    const live = () => currentRow?.() ?? row;
    const wrap = document.createElement('div');
    wrap.className = 'tf-table__row-actions';
    if (row._canAdmin) {
      const b = document.createElement('tf-button');
      b.setAttribute('variant', 'secondary');
      b.setAttribute('size', 'sm');
      b.setAttribute('icon', row._paused ? 'play' : 'pause');
      b.textContent = T(row._paused ? 'consumers.resume' : 'consumers.pause');
      b.dataset.act = row._paused ? 'resume' : 'pause';
      b.addEventListener('click', (e) => {
        e.stopPropagation();
        const r = live();
        ctx.go({ kind: r._paused ? 'resume' : 'pause', group: r._group, topic: r._topic });
      });
      wrap.appendChild(b);
    }
    const open = document.createElement('tf-button');
    open.setAttribute('variant', 'ghost');
    open.setAttribute('size', 'sm');
    open.setAttribute('icon', 'chevron-right');
    open.setAttribute('aria-label', T('consumers.open'));
    open.title = T('consumers.open');
    open.dataset.act = 'open';
    open.addEventListener('click', (e) => { e.stopPropagation(); const r = live(); ctx.go({ kind: 'open', group: r._group, topic: r._topic }); });
    wrap.appendChild(open);
    return wrap;
  };
}

/**
 * Draws or repaints the tab from `ctx.view()` = `{ groups, error, errorKind,
 * stats, instanceLabel, notice, nowMs }` (`groups` is `null` until the list
 * answered). `ctx.go(action)`: `{ kind: 'open'|'pause'|'resume', group,
 * topic }`, `{ kind: 'retry' }`. The filter and the search survive a repaint.
 */
export function drawConsumers(body, ctx) {
  const view = ctx.view();
  const { groups, error, errorKind, instanceLabel } = view;
  let mode = 'list';
  if (groups == null) mode = error ? `error:${errorKind}` : 'loading';
  else if (groups.length === 0) mode = 'empty';
  if (body.__tbMode !== mode) {
    body.__tbMode = mode;
    if (mode === 'loading') patchHtml(body, loadingHtml());
    else if (mode.startsWith('error:')) patchHtml(body, loadErrorHtml({ kind: errorKind, instanceLabel, titleKey: 'consumers.error_title' }));
    else if (mode === 'empty') patchHtml(body, emptyHtml());
    else {
      patchHtml(body, listHtml());
      body.__tbFilter = body.__tbFilter || 'all';
      body.__tbQuery = body.__tbQuery || '';
      const table = body.querySelector('[data-role="table"]');
      table.rowActions = rowActions(ctx);
      table.rowActionsKey = (row) => `${row._key}|${row._paused}|${row._canAdmin}`;
      table.addEventListener('row-click', (e) => ctx.go({ kind: 'open', group: e.detail.row._group, topic: e.detail.row._topic }));
      body.querySelector('[data-role="filter"]').addEventListener('change', (e) => { body.__tbFilter = e.detail?.value || 'all'; paintList(body, ctx.view()); });
      const search = body.querySelector('[data-role="search"]');
      search.value = body.__tbQuery;
      search.addEventListener('search', (e) => { body.__tbQuery = e.detail?.value || ''; paintList(body, ctx.view()); });
    }
    if (!body.__tbWired) {
      body.__tbWired = true;
      body.addEventListener('click', (e) => {
        if (e.target.closest('[data-go="retry"]')) ctx.go({ kind: 'retry' });
      });
    }
  }
  paintNotice(body, view.notice);
  if (mode === 'list') paintList(body, view);
}

function paintNotice(body, notice) {
  const host = body.querySelector('[data-role="notice"]');
  if (!host) return;
  patchHtml(host, notice
    ? `<tf-alert tone="${escapeAttr(notice.tone || 'success')}" title="${escapeAttr(notice.title)}" message="${escapeAttr(notice.text || '')}"></tf-alert>`
    : '');
}

function paintList(body, view) {
  const all = consumerRows({ groups: view.groups, stats: view.stats, nowMs: view.nowMs });
  const { rows, counts } = filterConsumerRows(all, { filter: body.__tbFilter, query: body.__tbQuery });
  const toolbarUseful = all.length > 1;
  body.querySelector('[data-role="search"]').hidden = !toolbarUseful;
  const seg = body.querySelector('[data-role="filter"]');
  seg.hidden = !toolbarUseful;
  const countsSig = JSON.stringify(counts);
  if (seg.__tbCounts !== countsSig) {
    seg.__tbCounts = countsSig;
    seg.setOptions(CONSUMER_FILTERS.map((f) => ({ value: f, label: `${T(`consumers.filter_${f}`)} ${fmtCount(counts[f])}` })), body.__tbFilter);
  }
  setAttr(body.querySelector('[data-role="count"]'), 'label', fmtCount(rows.length));
  // Who may pause and move is decided per topic; when the reader may change
  // none of the consumers, one line says who can.
  const note = body.querySelector('[data-role="admin-note"]');
  note.hidden = all.length === 0 || all.some((r) => r.canAdmin);
  note.querySelector('span').textContent = T('consumers.admin_only');
  const maxWaiting = rows.reduce((m, r) => Math.max(m, r.waiting || 0), 0);
  const table = body.querySelector('[data-role="table"]');
  const next = rows.map((r) => tableRow(r, maxWaiting, view.nowMs));
  const sig = JSON.stringify(next);
  if (table.__tbSig !== sig) {
    table.__tbSig = sig;
    table.rows = next;
  }
  table.hidden = next.length === 0;
  body.querySelector('[data-role="no-match"]').hidden = next.length > 0;
  body.querySelector('[data-role="row-hint"]').hidden = next.length === 0;
  const f = consumersFooter(rows);
  const footer = body.querySelector('[data-role="footer"]');
  footer.hidden = next.length === 0;
  const footerHtml = [
    T('consumers.footer_consumers', { count: `<b>${escapeHtml(fmtCount(f.consumers))}</b>`, n: f.consumers }),
    T('consumers.footer_waiting', { count: `<b>${escapeHtml(fmtCount(f.waiting))}</b>`, n: f.waiting }),
    T('consumers.footer_paused', { count: `<b>${escapeHtml(fmtCount(f.paused))}</b>`, n: f.paused }),
  ].map((part) => `<span>${part}</span>`).join('');
  if (footer.__tbSig !== footerHtml) {
    footer.__tbSig = footerHtml;
    footer.innerHTML = footerHtml;
  }
}

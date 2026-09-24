// ===== File: modules/tentabus/schemas.js — the Wzory wiadomości tab: the instance's message patterns as a read-only list =====
//
// T08's list (search, Wszystkie / W użyciu / Wycofane with counts, the table
// and the compatibility legend) over `SchemaSubjectListRequest`. The pattern
// windows (add, new version, compatibility, deprecate, delete) belong to the
// pattern page that comes with them; until then the list offers no row that
// opens anything, so nothing here promises an action the screen cannot take.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { setAttr, patchHtml } from '/js/lib/dom-patch.js';
import { T, fmtCount } from '/js/modules/tentabus/format.js';
import { loadErrorHtml } from '/js/modules/tentabus/overview.js';
import '/js/components/tf-table.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-spinner.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

export const SCHEMA_FILTERS = ['all', 'used', 'deprecated'];
const COMPATIBILITIES = ['none', 'backward', 'forward', 'full'];

const isDeprecated = (s) => s.deprecatedAtMs != null;
const isUsed = (s) => (s.usedByTopics || []).length > 0;

/** The state a pattern is in: withdrawn wins over "in use" (it still validates, but is on its way out). */
export function schemaState(s) {
  if (isDeprecated(s)) return 'deprecated';
  return isUsed(s) ? 'used' : 'unused';
}

/** Rows of one filter + search, and the per-filter counts of the segmented control. */
export function filterSchemas(subjects, { filter = 'all', query = '' } = {}) {
  const list = subjects || [];
  const q = String(query || '').trim().toLowerCase();
  const matches = (s) => !q || s.subject.toLowerCase().includes(q)
    || (s.usedByTopics || []).some((t) => t.toLowerCase().includes(q));
  const counts = {
    all: list.length,
    used: list.filter((s) => schemaState(s) === 'used').length,
    deprecated: list.filter(isDeprecated).length,
  };
  const rows = list
    .filter((s) => filter === 'all' || (filter === 'used' ? schemaState(s) === 'used' : isDeprecated(s)))
    .filter(matches)
    .sort((a, b) => a.subject.localeCompare(b.subject));
  return { rows, counts };
}

/** A format name in plain words; the raw value only for a type this screen has no name for. */
export function schemaFormatLabel(type) {
  const key = `schemas.format.${type}`;
  const text = T(key);
  return text === `tentabus.${key}` ? String(type || '') : text;
}

function tableRow(s) {
  const state = schemaState(s);
  const stateChip = {
    used: `<span class="tf-chip tf-chip--outline ok">${escapeHtml(T('schemas.state_used'))}</span>`,
    unused: `<span class="tf-chip tf-chip--outline">${escapeHtml(T('schemas.state_unused'))}</span>`,
    deprecated: `<span class="tf-chip tf-chip--outline warn">${escapeHtml(T('schemas.state_deprecated'))}</span>`,
  }[state];
  // Cell markup lives in the tf-table shadow root: only controls.css classes apply there.
  const used = isUsed(s)
    ? `<span class="tf-table__cell--mono"><span class="tf-table__cell-title">${s.usedByTopics.map(escapeHtml).join(', ')}</span></span>`
    : `<span class="tf-table__cell-sub">${escapeHtml(T('schemas.used_none'))}</span>`;
  const compat = COMPATIBILITIES.includes(s.compatibility) ? T(`schemas.compat.${s.compatibility}`) : String(s.compatibility || '');
  return {
    subject: `<span class="tf-table__cell--mono"><span class="tf-table__cell-title">${escapeHtml(s.subject)}</span></span>`,
    format: `<span class="tf-chip tf-chip--outline accent">${escapeHtml(schemaFormatLabel(s.schemaType))}</span>`,
    version: s.latestVersion == null ? '—' : fmtCount(s.latestVersion),
    compatibility: escapeHtml(compat),
    used,
    state: stateChip,
    _key: s.subject,
  };
}

function skeletonHtml() {
  const legend = COMPATIBILITIES.map((c) => `
    <div class="legend-item"><div class="li-name">${escapeHtml(T(`schemas.compat_title.${c}`))}</div><div class="li-sub">${escapeHtml(T(`schemas.compat_desc.${c}`))}</div></div>`).join('');
  return `
    <div class="tf-toolbar tb-schemas-toolbar">
      <tf-searchbox data-role="search" placeholder="${escapeAttr(T('schemas.search'))}" debounce="150"></tf-searchbox>
      <tf-segmented data-role="filter" size="md" value="all"></tf-segmented>
    </div>
    <div class="section-card">
      <div class="section-card-head"><div class="title">${sprite('file-code')} ${escapeHtml(T('schemas.title'))} <tf-chip size="sm" variant="outline" status="neutral" data-role="count"></tf-chip></div></div>
      <div class="section-sub">${escapeHtml(T('schemas.sub'))}</div>
      <tf-table data-role="table">
        <tf-column key="subject" label="${escapeAttr(T('schemas.col_subject'))}" renderer="html" fill></tf-column>
        <tf-column key="format" label="${escapeAttr(T('schemas.col_format'))}" renderer="html"></tf-column>
        <tf-column key="version" label="${escapeAttr(T('schemas.col_version'))}" renderer="num"></tf-column>
        <tf-column key="compatibility" label="${escapeAttr(T('schemas.col_compat'))}" renderer="html" hide-below="1100"></tf-column>
        <tf-column key="used" label="${escapeAttr(T('schemas.col_used'))}" renderer="html"></tf-column>
        <tf-column key="state" label="${escapeAttr(T('schemas.col_state'))}" renderer="html"></tf-column>
      </tf-table>
      <div class="muted" data-role="no-match" hidden>${escapeHtml(T('schemas.no_match'))}</div>
    </div>
    <div class="section-card">
      <div class="section-card-head"><div class="title">${sprite('info')} ${escapeHtml(T('schemas.legend_title'))}</div></div>
      <div class="legend-grid">${legend}</div>
    </div>`;
}

function emptyHtml() {
  return `
    <div class="section-card">
      <div class="section-card-head"><div class="title">${sprite('file-code')} ${escapeHtml(T('schemas.title'))} <tf-chip size="sm" variant="outline" status="neutral" label="0"></tf-chip></div></div>
      <tf-empty-state badge icon="file-text" title="${escapeAttr(T('schemas.empty_title'))}" message="${escapeAttr(T('schemas.empty_sub'))}"></tf-empty-state>
    </div>`;
}

/**
 * Draws or repaints the tab from `ctx.view()` =
 * `{ subjects, error, errorKind, instanceLabel }`; `ctx.go({ kind: 'retry' })`
 * reloads. The filter and the search survive a repaint (they live on the body).
 */
export function drawSchemas(body, ctx) {
  const { subjects, error, errorKind, instanceLabel } = ctx.view();
  let mode = 'list';
  if (subjects == null) mode = error ? `error:${errorKind}` : 'loading';
  else if (subjects.length === 0) mode = 'empty';
  if (body.__tbMode !== mode) {
    body.__tbMode = mode;
    if (mode === 'loading') patchHtml(body, `<div class="tb-state"><tf-spinner size="sm"></tf-spinner>${escapeHtml(T('shell.loading'))}</div>`);
    else if (mode.startsWith('error:')) patchHtml(body, loadErrorHtml({ kind: errorKind, instanceLabel, titleKey: 'schemas.error_title' }));
    else if (mode === 'empty') patchHtml(body, emptyHtml());
    else {
      patchHtml(body, skeletonHtml());
      body.__tbFilter = body.__tbFilter || 'all';
      body.__tbQuery = body.__tbQuery || '';
      const seg = body.querySelector('[data-role="filter"]');
      seg.addEventListener('change', (e) => { body.__tbFilter = e.detail?.value || 'all'; paintList(body, ctx.view().subjects); });
      body.querySelector('[data-role="search"]').addEventListener('search', (e) => { body.__tbQuery = e.detail?.value || ''; paintList(body, ctx.view().subjects); });
    }
    if (!body.__tbWired) {
      body.__tbWired = true;
      body.addEventListener('click', (e) => {
        if (e.target.closest('[data-go="retry"]')) ctx.go({ kind: 'retry' });
      });
    }
  }
  if (mode === 'list') paintList(body, subjects);
}

function paintList(body, subjects) {
  const { rows, counts } = filterSchemas(subjects, { filter: body.__tbFilter, query: body.__tbQuery });
  const seg = body.querySelector('[data-role="filter"]');
  const countsSig = JSON.stringify(counts);
  if (seg && seg.__tbCounts !== countsSig) {
    seg.__tbCounts = countsSig;
    seg.setOptions(SCHEMA_FILTERS.map((f) => ({ value: f, label: `${T(`schemas.filter_${f}`)} ${fmtCount(counts[f])}` })), body.__tbFilter);
  }
  setAttr(body.querySelector('[data-role="count"]'), 'label', fmtCount(counts.all));
  const table = body.querySelector('[data-role="table"]');
  const next = rows.map(tableRow);
  const sig = JSON.stringify(next);
  if (table.__tbSig !== sig) {
    table.__tbSig = sig;
    table.rows = next;
  }
  table.hidden = next.length === 0;
  body.querySelector('[data-role="no-match"]').hidden = next.length > 0;
}

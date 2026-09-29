// ===== File: modules/tentabus/schemas.js — the Wzory wiadomości tab: the instance's message patterns =====
//
// T08's list over `SchemaSubjectListRequest`: search, Wszystkie / W użyciu /
// Wycofane with counts, one row per pattern with the topics using it, and the
// compatibility legend. A row opens the pattern's page (schema-detail.js);
// "Dodaj wzór" and the row's bin open the windows in schema-windows.js. The
// bin is offered only for a pattern no topic uses — the server refuses to
// delete a bound pattern (`registry::delete`), so for a used one it turns
// into a lock that says, on hover, focus and tap alike, which topic holds it. Adding, withdrawing and deleting
// are the instance administrator's (`gate_admin`); a reader sees the list
// with one line saying so.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { setAttr, patchHtml } from '/js/lib/dom-patch.js';
import { T, fmtCount } from '/js/modules/tentabus/format.js';
import { loadErrorHtml } from '/js/modules/tentabus/overview.js';
import '/js/components/tf-table.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-button.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-spinner.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

export const SCHEMA_FILTERS = ['all', 'used', 'deprecated'];
export const COMPATIBILITIES = ['none', 'backward', 'forward', 'full'];

/**
 * Which pattern formats describe which payloads: a JSON Schema checks JSON,
 * an XSD checks XML, an HL7 v2 profile checks HL7 v2, and Avro / Protobuf /
 * Thrift describe binary payloads.
 */
export const SCHEMA_TYPES_BY_KIND = {
  json: ['json_schema'],
  xml: ['xsd'],
  hl7v2: ['hl7v2_profile'],
  binary: ['avro', 'protobuf', 'thrift'],
};

/** The payload kind (`json` / `xml` / `hl7v2` / `binary`) a pattern format checks, or '' when unknown. */
export function schemaKind(type) {
  return Object.keys(SCHEMA_TYPES_BY_KIND).find((k) => SCHEMA_TYPES_BY_KIND[k].includes(type)) || '';
}

const isDeprecated = (s) => s.deprecatedAtMs != null;
const isUsed = (s) => (s.usedByTopics || []).length > 0;

/** The state a pattern is in: withdrawn wins over "in use" (it still validates, but is on its way out). */
export function schemaState(s) {
  if (isDeprecated(s)) return 'deprecated';
  return isUsed(s) ? 'used' : 'unused';
}

/**
 * Rows of one filter + search, and the per-filter counts of the segmented
 * control. "W użyciu" is what a topic checks with — a withdrawn pattern a
 * topic still uses is in it (and in "Wycofane").
 */
export function filterSchemas(subjects, { filter = 'all', query = '' } = {}) {
  const list = subjects || [];
  const q = String(query || '').trim().toLowerCase();
  const matches = (s) => !q || s.subject.toLowerCase().includes(q)
    || (s.usedByTopics || []).some((t) => t.toLowerCase().includes(q));
  const counts = {
    all: list.length,
    used: list.filter(isUsed).length,
    deprecated: list.filter(isDeprecated).length,
  };
  const rows = list
    .filter((s) => filter === 'all' || (filter === 'used' ? isUsed(s) : isDeprecated(s)))
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

/** What is checked when a version is added, in words; the raw value for one this screen does not know. */
export function compatLabel(compatibility) {
  return COMPATIBILITIES.includes(compatibility) ? T(`schemas.compat.${compatibility}`) : String(compatibility || '');
}

/** "a, b i c" in the reader's language. */
export function listText(names) {
  return new Intl.ListFormat(I18n.getLanguage(), { type: 'conjunction' }).format(names);
}

/** Why a pattern cannot be deleted, or `null` when it can: the topics that still check with it. */
export function deleteBlocker(s) {
  const topics = [...(s.usedByTopics || [])].sort();
  if (!topics.length) return null;
  return T('schemas.delete_blocked', { topics: listText(topics), n: topics.length });
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
  return {
    subject: `<span class="tf-table__cell--mono"><span class="tf-table__cell-title">${escapeHtml(s.subject)}</span></span>`,
    format: `<span class="tf-chip tf-chip--outline accent">${escapeHtml(schemaFormatLabel(s.schemaType))}</span>`,
    version: s.latestVersion == null ? '—' : fmtCount(s.latestVersion),
    compatibility: escapeHtml(compatLabel(s.compatibility)),
    used,
    state: stateChip,
    _key: s.subject,
    _subject: s.subject,
    _blocker: deleteBlocker(s),
  };
}

// The row's own buttons: delete (administrators) and the arrow that opens it.
// While a topic uses the pattern the bin is a lock that still answers a
// click, a tap and Enter with the reason — a disabled button would keep it
// in a hover-only tooltip. Each stops the click so it acts instead of
// opening the row.
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
      b.addEventListener('click', (e) => {
        e.stopPropagation();
        const current = live();
        ctx.go(kind === 'delete-blocked' ? { kind, subject: current._subject, reason: current._blocker } : { kind, subject: current._subject });
      });
      wrap.appendChild(b);
    };
    if (canAdmin && row._blocker) add('lock', row._blocker, 'delete-blocked');
    else if (canAdmin) add('trash', T('schemas.action_delete'), 'delete');
    add('chevron-right', T('schemas.action_open'), 'open');
    return wrap;
  };
}

function legendHtml() {
  const items = COMPATIBILITIES.map((c) => `
    <div class="legend-item"><div class="li-name">${escapeHtml(T(`schemas.compat_title.${c}`))}</div><div class="li-sub">${escapeHtml(T(`schemas.compat_desc.${c}`))}</div></div>`).join('');
  return `
    <div class="section-card">
      <div class="section-card-head"><div class="title">${sprite('info')} ${escapeHtml(T('schemas.legend_title'))}</div></div>
      <div class="legend-grid">${items}</div>
    </div>`;
}

const adminNote = () => `<div class="muted tb-admin-note">${sprite('lock')} ${escapeHtml(T('schemas.admin_only'))}</div>`;

function listHtml(canAdmin) {
  return `
    <div data-role="notice"></div>
    <div class="tf-toolbar tb-schemas-toolbar">
      <tf-searchbox data-role="search" placeholder="${escapeAttr(T('schemas.search'))}" debounce="150"></tf-searchbox>
      <tf-segmented data-role="filter" size="md" value="all"></tf-segmented>
      <span class="tf-toolbar-spacer"></span>
      ${canAdmin ? `<tf-button variant="primary" icon="plus" data-go="add">${escapeHtml(T('schemas.add_action'))}</tf-button>` : ''}
    </div>
    <div class="section-card">
      <div class="section-card-head">
        <div class="title">${sprite('file-code')} ${escapeHtml(T('schemas.title'))} <tf-chip size="sm" variant="outline" status="neutral" data-role="count"></tf-chip></div>
        <div class="actions muted" data-role="row-hint">${escapeHtml(T('schemas.row_hint'))}</div>
      </div>
      <div class="section-sub">${escapeHtml(T('schemas.sub'))}</div>
      ${canAdmin ? '' : adminNote()}
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
    ${legendHtml()}`;
}

function emptyHtml(canAdmin) {
  return `
    <div data-role="notice"></div>
    <div class="section-card">
      <div class="section-card-head"><div class="title">${sprite('file-code')} ${escapeHtml(T('schemas.title'))} <tf-chip size="sm" variant="outline" status="neutral" label="0"></tf-chip></div></div>
      <tf-empty-state badge icon="file-text" title="${escapeAttr(T('schemas.empty_title'))}" message="${escapeAttr(T('schemas.empty_sub'))}">
        ${canAdmin ? `<tf-button variant="primary" icon="plus" data-go="add">${escapeHtml(T('schemas.add_action'))}</tf-button>` : ''}
      </tf-empty-state>
      ${canAdmin ? '' : adminNote()}
    </div>`;
}

/**
 * Draws or repaints the tab from `ctx.view()` = `{ subjects, error,
 * errorKind, instanceLabel, canAdmin, notice }`. `ctx.go(action)`:
 * `{ kind: 'open' | 'delete', subject }`, `{ kind: 'delete-blocked', subject,
 * reason }`, `{ kind: 'add' }`, `{ kind:
 * 'retry' }`. The filter and the search survive a repaint (they live on the body).
 */
export function drawSchemas(body, ctx) {
  const view = ctx.view();
  const { subjects, error, errorKind, instanceLabel, canAdmin } = view;
  let mode = 'list';
  if (subjects == null) mode = error ? `error:${errorKind}` : 'loading';
  else if (subjects.length === 0) mode = 'empty';
  const modeKey = `${mode}:${canAdmin ? 'admin' : 'reader'}`;
  if (body.__tbMode !== modeKey) {
    body.__tbMode = modeKey;
    if (mode === 'loading') patchHtml(body, `<div class="tb-state"><tf-spinner size="sm"></tf-spinner>${escapeHtml(T('shell.loading'))}</div>`);
    else if (mode.startsWith('error:')) patchHtml(body, loadErrorHtml({ kind: errorKind, instanceLabel, titleKey: 'schemas.error_title' }));
    else if (mode === 'empty') patchHtml(body, emptyHtml(canAdmin));
    else {
      patchHtml(body, listHtml(canAdmin));
      body.__tbFilter = body.__tbFilter || 'all';
      body.__tbQuery = body.__tbQuery || '';
      const table = body.querySelector('[data-role="table"]');
      table.rowActions = rowActions(ctx, canAdmin);
      table.rowActionsKey = (row) => `${row._subject}|${row._blocker || ''}`;
      table.addEventListener('row-click', (e) => ctx.go({ kind: 'open', subject: e.detail.row._subject }));
      body.querySelector('[data-role="filter"]').addEventListener('change', (e) => { body.__tbFilter = e.detail?.value || 'all'; paintList(body, ctx.view().subjects); });
      const search = body.querySelector('[data-role="search"]');
      search.value = body.__tbQuery;
      search.addEventListener('search', (e) => { body.__tbQuery = e.detail?.value || ''; paintList(body, ctx.view().subjects); });
    }
    if (!body.__tbWired) {
      body.__tbWired = true;
      body.addEventListener('click', (e) => {
        if (e.target.closest('[data-go="retry"]')) ctx.go({ kind: 'retry' });
        else if (e.target.closest('[data-go="add"]')) ctx.go({ kind: 'add' });
      });
    }
  }
  paintNotice(body, view.notice);
  if (mode === 'list') paintList(body, subjects);
}

function paintNotice(body, notice) {
  const host = body.querySelector('[data-role="notice"]');
  if (!host) return;
  patchHtml(host, notice
    ? `<tf-alert tone="${escapeAttr(notice.tone || 'success')}" title="${escapeAttr(notice.title)}" message="${escapeAttr(notice.text || '')}"></tf-alert>`
    : '');
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
  body.querySelector('[data-role="row-hint"]').hidden = next.length === 0;
}

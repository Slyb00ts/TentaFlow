// =============================================================================
// File: modules/org-structure/import-panel.js
// Description: The import window of the Lista tab (mockup F03): pick a CSV or
//   XLSX file, choose the mode and the day, run it as a dry run and read the
//   report — counts, an error card per problem with the decisions the server
//   accepts (use the suggested login, leave the seat vacant, skip the row), the
//   changed and added rows, and a preview of the tree the file leaves — then
//   "Zapisz wszystko". A dry run and an apply are the SAME run on the server,
//   so the report is what the apply will do; the button stays locked until the
//   report has no error. Every decision and every change of file, mode or day
//   runs the dry run again: the screen never holds a report for other inputs
//   than the ones the apply would send.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { escapeAttr, escapeHtml } from '/js/utils.js';
import { TfToast } from '/js/components/tf-toast.js';
import '/js/components/tf-window.js';
import '/js/components/tf-button.js';
import '/js/components/tf-file-input.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-input.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-stat-card.js';
import '/js/components/tf-table.js';
import '/js/components/tf-org-tree.js';
import { openConfirmWindow } from '/js/lib/actions/index.js';
import { downloadBytes } from '/js/lib/download.js';
import { formatDay, isIsoDay } from '/js/lib/date-format.js';
import { buildTreeModel } from '/js/modules/org-structure/tree.js';
import { treeLabels } from '/js/modules/org-structure/tree-tab.js';
import { writeErrorText } from '/js/modules/org-structure/list-actions.js';
import {
  MAX_FILE_BYTES, canApply, endedItems, errorCards, errorCount, formatOfFile, needsEndedConfirmation, previewMarks, previewUnitMarks, reportRow,
  replaceImpact, rowsByStatus, rowsLabel, runPayload, seatsLost, unresolvedErrors, withResolution, withoutResolution,
} from '/js/modules/org-structure/import-model.js';

const it = (key, params) => I18n.t(`org_structure.import.${key}`, params);
const ot = (key, params) => I18n.t(`org_structure.${key}`, params);

// A key this build has no sentence for reads as a generic sentence that names the code, never as a blank line.
function known(key, params, fallbackKey) {
  const full = `org_structure.import.${key}`;
  const text = I18n.t(full, params);
  return text && text !== full ? text : it(fallbackKey, params);
}

const columnLabel = (key) => known(`col_${key}`, {}, 'col_unknown');
const issueTitle = (issue) => (issue.kind === 'rejected' && issue.code
  ? writeErrorText({ code: issue.code })
  : known(`issue_${issue.kind}`, { value: issue.value ?? '', kind: issue.kind }, 'issue_unknown'));

const svg = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

const ENDED_IN_CONFIRM = 6;
const LIST_TABS = ['errors', 'changed', 'added', 'ended'];

function changeText(change) {
  const entity = change.entity;
  if (change.field === 'created') return known(`change_created_${entity}`, {}, 'change_unknown');
  if (change.field === 'ended') return known(`change_ended_${entity}`, {}, 'change_unknown');
  const name = known(`field_${change.field}`, {}, 'field_unknown');
  return `${name}: ${change.before ?? '—'} → ${change.after ?? '—'}`;
}

function rowListItems(rows) {
  return rows.map((row) => ({
    _id: `r${row.row}`,
    row: row.row,
    what: row.person || row.position_code || row.unit_code || '—',
    where: [row.unit_code, row.position_code].filter(Boolean).join(' · ') || '—',
    changes: (row.changes ?? []).map(changeText).join('; ') || '—',
  }));
}

function endedRows(report) {
  return endedItems(report).map((item, index) => ({
    _id: `e${index}`,
    kind: known(`ended_kind_${item.kind}`, {}, 'ended_kind_unknown'),
    name: item.code ? `${item.name} (${item.code})` : item.name,
    holders: item.holders.length ? item.holders.join(', ') : it('ended_nobody'),
  }));
}

/**
 * Opens the import window. `day` is the organization's today (the default of "od kiedy"), `unitTypes` names the
 * types in the preview, `onApplied()` runs after an apply that saved — the caller reloads the structure.
 * Returns the tf-window.
 */
export function openImportPanel({ day, unitTypes = [], onApplied }) {
  const s = {
    file: null, format: null, bytes: null, mode: 'upsert', asOf: day, confirmBackdated: false,
    resolutions: [], report: null, busy: null, failure: '', tab: 'errors', seq: 0,
  };

  const win = document.createElement('tf-window');
  win.setAttribute('title', it('title'));
  win.setAttribute('icon', 'arrow-up');
  win.setAttribute('buttons', 'close');
  win.setAttribute('modal', '');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '1080');
  win.setAttribute('min-width', '360');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.classList.add('org-imp-window');
  win.innerHTML = `
    <div slot="body" class="org-imp">
      <div class="org-imp-top">
        <div class="org-imp-file">
          <tf-file-input data-role="file" accept=".csv,.xlsx" label="${escapeAttr(it('file_label'))}"></tf-file-input>
          <div class="org-imp-meta" data-role="meta">${escapeHtml(it('file_hint'))}</div>
        </div>
        <div class="org-imp-mode">
          <span class="org-imp-caption">${escapeHtml(it('mode_label'))}</span>
          <tf-segmented data-role="mode" size="sm" value="upsert" aria-label="${escapeAttr(it('mode_label'))}">
            <option value="upsert">${escapeHtml(it('mode_upsert'))}</option>
            <option value="replace" variant="warn">${escapeHtml(it('mode_replace'))}</option>
          </tf-segmented>
        </div>
        <tf-date-field data-role="asof" class="org-imp-asof" label="${escapeAttr(it('asof_label'))}"
          hint="${escapeAttr(it('asof_hint'))}" value="${escapeAttr(day)}"></tf-date-field>
      </div>
      <div data-role="mode-note"></div>
      <div data-role="status"></div>
      <div data-role="report" hidden>
        <div class="org-imp-summary" data-role="summary"></div>
        <div class="org-imp-cols">
          <div class="org-imp-left">
            <tf-segmented data-role="tabs" size="sm" value="errors" aria-label="${escapeAttr(it('tabs_label'))}"></tf-segmented>
            <div class="org-imp-decisions" data-role="decisions"></div>
            <div class="org-imp-list" data-role="list"></div>
          </div>
          <div class="org-imp-right">
            <div class="org-imp-preview-head">
              <span class="org-imp-caption">${escapeHtml(it('preview_title'))}</span>
              <tf-chip data-role="partial" variant="outline" status="warn" hidden title="${escapeAttr(it('preview_partial_hint'))}">${escapeHtml(it('preview_partial'))}</tf-chip>
            </div>
            <div class="org-imp-preview" data-role="preview"></div>
            <div class="org-imp-legend">
              <span><i class="org-imp-swatch org-imp-swatch--added"></i>${escapeHtml(it('legend_added'))}</span>
              <span><i class="org-imp-swatch org-imp-swatch--changed"></i>${escapeHtml(it('legend_changed'))}</span>
              <span><i class="org-imp-swatch org-imp-swatch--error"></i>${escapeHtml(it('legend_error'))}</span>
            </div>
          </div>
        </div>
      </div>
    </div>
    <div slot="footer" class="org-imp-foot">
      <div class="org-imp-foot-note">${svg('lock')}<span>${escapeHtml(it('foot_note'))}</span></div>
      <div class="org-imp-foot-actions">
        <tf-button data-act="errors-report" variant="ghost" icon="download">${escapeHtml(it('btn_errors_report'))}</tf-button>
        <tf-button data-act="cancel" variant="secondary">${escapeHtml(I18n.t('actions.cancel'))}</tf-button>
        <tf-button data-act="apply" variant="primary" icon="check">${escapeHtml(it('btn_apply'))}</tf-button>
      </div>
    </div>`;

  const q = (role) => win.querySelector(`[data-role="${role}"]`);
  const btn = (act) => win.querySelector(`[data-act="${act}"]`);
  let chart = null;

  // ---------------------------------------------------------------------------
  // Rendering
  // ---------------------------------------------------------------------------

  function renderModeNote() {
    const host = q('mode-note');
    if (s.mode !== 'replace') {
      host.innerHTML = '';
      return;
    }
    const impact = s.report ? replaceImpact(s.report) : null;
    let text = it('mode_replace_note');
    if (impact && impact.total > 0) {
      text = `${text} ${it('replace_impact', { ...impact, date: formatDay(s.report.as_of) })}`;
    } else if (impact) {
      text = `${text} ${it('replace_none')}`;
    }
    host.innerHTML = `<tf-alert tone="warning" message="${escapeAttr(text)}"></tf-alert>`;
  }

  function renderStatus() {
    const host = q('status');
    let html = '';
    if (s.busy === 'dry') html = `<tf-alert tone="info" message="${escapeAttr(it('running'))}"></tf-alert>`;
    else if (s.failure) html = `<tf-alert tone="danger" message="${escapeAttr(s.failure)}"></tf-alert>`;
    else if (s.report?.file_error) {
      const { code, field } = s.report.file_error;
      const column = field ? columnLabel(field) : '';
      html = `<tf-alert tone="danger" message="${escapeAttr(known(`file_error_${code}`, { column, code }, 'file_error_unknown'))}"></tf-alert>`;
    } else if (!s.report) html = `<tf-alert tone="info" message="${escapeAttr(it('idle'))}"></tf-alert>`;
    host.innerHTML = html;
  }

  function summaryCard(label, value, detail, accent) {
    return `<tf-stat-card label="${escapeAttr(label)}" value="${value}" delta="${escapeAttr(detail)}" delta-type="neutral"
      delta-position="under-value"${accent ? ` accent="${accent}"` : ''}></tf-stat-card>`;
  }

  function renderSummary() {
    const c = s.report.counts;
    const cards = [
      summaryCard(it('stat_added'), c.added, it('stat_added_detail', { units: c.units_added, positions: c.positions_added, assignments: c.assignments_added }), 'success'),
      summaryCard(it('stat_changed'), c.changed, it('stat_changed_detail', { units: c.units_changed, positions: c.positions_changed, assignments: c.assignments_changed }), 'info'),
      summaryCard(it('stat_errors'), errorCount(s.report), errorCount(s.report) > 0 ? it('stat_errors_detail') : it('stat_errors_none'), errorCount(s.report) > 0 ? 'danger' : ''),
      summaryCard(it('stat_unchanged'), c.unchanged, it('stat_unchanged_detail'), ''),
    ];
    if (s.mode === 'replace') {
      cards.push(summaryCard(it('stat_ended'), replaceImpact(s.report).total,
        it('stat_ended_detail', { units: c.units_ended, positions: c.positions_ended, assignments: c.assignments_ended }), 'warning'));
    }
    q('summary').innerHTML = cards.join('');
  }

  function renderTabs() {
    const c = s.report.counts;
    const ended = endedItems(s.report).length;
    const counts = { errors: errorCount(s.report), changed: c.changed, added: c.added, ended };
    // What a replace ends gets its own tab, and only when there is something to list.
    const shown = LIST_TABS.filter((id) => id !== 'ended' || (s.mode === 'replace' && ended > 0));
    if (!shown.includes(s.tab)) s.tab = shown[0];
    // The options are replaced through the component: its <option> children are read only when it is built.
    q('tabs').setOptions(
      shown.map((id) => ({
        value: id,
        label: it(`tab_${id}`, { count: counts[id] }),
        variant: id === 'errors' && errorCount(s.report) > 0 ? 'err' : 'neutral',
      })),
      s.tab,
    );
  }

  function cardHtml(card) {
    const { issue } = card;
    const rowsCount = [...new Set([...(issue.rows ?? []), issue.row])].filter(Boolean).length;
    // An issue about the file as a whole (row 0) has no row to name.
    const where = rowsCount === 0 ? ''
      : s.report.sheet
        ? it('location_sheet_rows', { sheet: s.report.sheet, count: rowsCount, rows: rowsLabel(issue) })
        : it('location_rows', { count: rowsCount, rows: rowsLabel(issue) });
    const location = [
      where,
      issue.column ? it('location_column', { column: columnLabel(issue.column) }) : '',
      issue.value ? it(issue.kind === 'other_sheets_ignored' ? 'location_sheets' : 'location_value', { value: issue.value }) : '',
    ].filter(Boolean).join(' · ');
    let hint = '';
    if (issue.suggestion) {
      hint = issue.kind === 'unknown_person'
        ? `<span>${escapeHtml(it('suggest_login_prefix'))}</span> <b>${escapeHtml(issue.suggestion)}</b>${issue.suggestion_label ? ` <span>(${escapeHtml(issue.suggestion_label)})</span>` : ''}<span>?</span>`
        : `<span>${escapeHtml(it(issue.kind === 'unknown_unit_type' ? 'suggest_type' : 'suggest_generic', { value: issue.suggestion }))}</span>`;
    }
    const buttons = card.actions.map((action) => {
      const label = it({ use_suggested_login: 'act_use_login', leave_vacant: 'act_leave_vacant', skip_row: 'act_skip_row' }[action]);
      return `<tf-button size="sm" variant="${action === 'use_suggested_login' ? 'secondary' : 'ghost'}" data-act="decide" data-row="${issue.row}" data-action="${action}"${action === 'use_suggested_login' ? ` data-login="${escapeAttr(issue.suggestion)}"` : ''}>${escapeHtml(label)}</tf-button>`;
    });
    if (issue.kind === 'backdated_confirmation_required' && !s.confirmBackdated) {
      buttons.unshift(`<tf-button size="sm" variant="secondary" data-act="confirm-backdated">${escapeHtml(it('act_confirm_backdated'))}</tf-button>`);
    }
    if (issue.row) {
      buttons.push(`<tf-button size="sm" variant="ghost" data-act="show" data-index="${card.key}">${escapeHtml(it('act_show'))}</tf-button>`);
    }
    return `<div class="org-imp-card" data-card="${escapeAttr(card.key)}">
      <span class="org-imp-card-icon">${svg('x')}</span>
      <div class="org-imp-card-body">
        <div class="org-imp-card-title">${escapeHtml(issueTitle(issue))}</div>
        <div class="org-imp-card-where">${escapeHtml(location)}</div>
        ${hint ? `<div class="org-imp-card-hint">${hint}</div>` : ''}
        ${buttons.length ? `<div class="org-imp-card-fix">${buttons.join('')}</div>` : ''}
      </div>
    </div>`;
  }

  function warningsHtml() {
    const warnings = s.report.warnings ?? [];
    if (!warnings.length) return '';
    const items = warnings
      .map((w) => `<li>${escapeHtml(it('warning_line', { rows: rowsLabel(w), text: issueTitle(w) }))}</li>`)
      .join('');
    return `<div class="org-imp-warnings"><div class="org-imp-caption">${escapeHtml(it('warnings_title', { count: warnings.length }))}</div><ul>${items}</ul></div>`;
  }

  function renderDecisions() {
    const chips = s.resolutions.map((r) => `<tf-chip variant="outline" removable data-row="${r.row}"
      label="${escapeAttr(it('decision_row', { row: r.row, action: it(`decision_${r.action}`) }))}"></tf-chip>`).join('');
    const host = q('decisions');
    host.innerHTML = chips ? `<span class="org-imp-caption">${escapeHtml(it('decisions_title'))}</span>${chips}` : '';
    // The chip's `remove` does not bubble, so each chip carries its own listener.
    for (const chip of host.querySelectorAll('tf-chip')) {
      chip.addEventListener('remove', () => {
        s.resolutions = withoutResolution(s.resolutions, Number(chip.dataset.row));
        dryRun();
      });
    }
  }

  function renderList() {
    const host = q('list');
    if (s.tab === 'errors') {
      const cards = errorCards(s.report, s.resolutions);
      host.innerHTML = cards.length
        ? `<div class="org-imp-cards">${cards.map(cardHtml).join('')}</div>${warningsHtml()}`
        : `<div class="org-imp-empty">${escapeHtml(it('list_empty_errors'))}</div>${warningsHtml()}`;
      return;
    }
    if (s.tab === 'ended') {
      host.innerHTML = `<tf-table class="org-imp-rows" density="compact">
        <tf-column key="kind" label="${escapeAttr(it('col_ended_kind'))}"></tf-column>
        <tf-column key="name" label="${escapeAttr(it('col_ended_name'))}" fill></tf-column>
        <tf-column key="holders" label="${escapeAttr(it('col_ended_holders'))}"></tf-column>
      </tf-table>`;
      host.querySelector('tf-table').rows = endedRows(s.report);
      return;
    }
    const rows = rowListItems(rowsByStatus(s.report, s.tab));
    if (!rows.length) {
      host.innerHTML = `<div class="org-imp-empty">${escapeHtml(it(`list_empty_${s.tab}`))}</div>`;
      return;
    }
    host.innerHTML = `<tf-table class="org-imp-rows" density="compact">
      <tf-column key="row" label="${escapeAttr(it('col_row'))}" renderer="num"></tf-column>
      <tf-column key="what" label="${escapeAttr(it('col_what'))}"></tf-column>
      <tf-column key="where" label="${escapeAttr(it('col_where'))}"></tf-column>
      <tf-column key="changes" label="${escapeAttr(it('col_changes'))}" fill></tf-column>
    </tf-table>`;
    host.querySelector('tf-table').rows = rows;
  }

  function renderPreview() {
    const host = q('preview');
    const preview = s.report.preview;
    q('partial').hidden = !s.report.preview_partial;
    if (!preview) {
      chart = null;
      host.innerHTML = `<div class="org-imp-empty">${escapeHtml(it('preview_none'))}</div>`;
      return;
    }
    const model = buildTreeModel(preview, { unitTypes, t: ot });
    const marks = previewMarks(s.report);
    for (const node of model.nodes) node.mark = marks.get(node.id) ?? null;
    const unitMarks = previewUnitMarks(s.report);
    for (const unit of model.units) unit.mark = unitMarks.get(unit.id) ?? null;
    if (!chart || !host.contains(chart)) {
      chart = document.createElement('tf-org-tree');
      chart.classList.add('org-imp-tree');
      // Units, not people: a whole file's worth of cards does not fit a preview at a readable size, while a
      // frame per unit does — the same call the chart makes for a large structure.
      chart.mode = 'units';
      host.replaceChildren(chart);
      chart.labels = treeLabels();
    }
    chart.model = model;
  }

  function applyBlockReason() {
    if (!s.report) return it('apply_blocked_none');
    if (unresolvedErrors(s.report)) {
      return it('apply_blocked_errors', { count: errorCount(s.report) });
    }
    return '';
  }

  function renderFooter() {
    const applyBtn = btn('apply');
    const allowed = canApply({ report: s.report, busy: s.busy });
    applyBtn.toggleAttribute('disabled', !allowed);
    applyBtn.setAttribute('label', s.busy === 'apply' ? it('btn_applying') : it('btn_apply'));
    applyBtn.setAttribute('title', allowed ? '' : applyBlockReason());
    btn('errors-report').toggleAttribute('disabled', !s.bytes || Boolean(s.busy) || !(s.report?.errors ?? []).length);
    btn('cancel').toggleAttribute('disabled', s.busy === 'apply');
  }

  function render() {
    renderModeNote();
    renderStatus();
    const showReport = Boolean(s.report) && !s.report.file_error;
    q('report').hidden = !showReport;
    if (showReport) {
      renderSummary();
      renderTabs();
      renderDecisions();
      renderList();
      renderPreview();
    }
    // The row count is the server's: a file it never read (too big, wrong type) has none to state.
    q('meta').textContent = !s.file ? it('file_hint')
      : s.report && !s.report.file_error ? it('file_meta', { name: s.file.name, count: s.report.counts.rows })
        : s.file.name;
    renderFooter();
  }

  // ---------------------------------------------------------------------------
  // Runs
  // ---------------------------------------------------------------------------

  const payload = () => runPayload({
    format: s.format, bytes: s.bytes, mode: s.mode, asOf: s.asOf,
    confirmBackdated: s.confirmBackdated, resolutions: s.resolutions,
  });

  async function dryRun() {
    if (!s.bytes) return;
    const seq = ++s.seq;
    s.busy = 'dry';
    s.failure = '';
    render();
    try {
      const body = await ApiBinary.one('orgImportDryRunRequest', payload());
      if (seq !== s.seq) return;
      s.report = body.report;
      if (!s.report.file_error) {
        const hasErrors = unresolvedErrors(s.report);
        if (!hasErrors && s.tab === 'errors') {
          s.tab = s.mode === 'replace' && endedItems(s.report).length ? 'ended' : s.report.counts.changed > 0 ? 'changed' : 'added';
        }
        if (hasErrors) s.tab = 'errors';
      }
    } catch (err) {
      if (seq !== s.seq) return;
      s.report = null;
      s.failure = it('request_failed', { message: err.message || '' });
    }
    s.busy = null;
    render();
  }

  // The apply itself: the same run as the dry run, kept. `confirmEnded` is sent only after the administrator
  // saw what a replace ends.
  async function runApply(confirmEnded) {
    s.busy = 'apply';
    s.failure = '';
    render();
    try {
      const body = await ApiBinary.one('orgImportApplyRequest', { ...payload(), confirmEnded });
      if (body.report.applied) {
        const c = body.report.counts;
        s.busy = null;
        win.close(true);
        TfToast.show({
          tone: 'success',
          message: it('applied', { added: c.added, changed: c.changed, ended: replaceImpact(body.report).total }),
        });
        await onApplied?.(body.report);
        return;
      }
      // The server saw errors this run: what is shown is what it found, and nothing was saved.
      s.report = body.report;
      s.tab = 'errors';
    } catch (err) {
      s.failure = it('request_failed', { message: err.message || '' });
    }
    s.busy = null;
    render();
  }

  // The first items of what a replace ends, with who loses a seat, for the confirmation window.
  function endedSummary() {
    const items = endedItems(s.report);
    const shown = items.slice(0, ENDED_IN_CONFIRM).map((item) => {
      const kind = known(`ended_kind_${item.kind}`, {}, 'ended_kind_unknown');
      const who = item.holders.length ? ` — ${item.holders.join(', ')}` : '';
      return `${kind} ${item.code ? `${item.name} (${item.code})` : item.name}${who}`;
    });
    if (items.length > shown.length) shown.push(it('end_confirm_more', { count: items.length - shown.length }));
    return shown.join('; ');
  }

  function apply() {
    if (!canApply({ report: s.report, busy: s.busy })) return;
    if (!needsEndedConfirmation(s.report, s.mode)) {
      runApply(false);
      return;
    }
    const impact = replaceImpact(s.report);
    openConfirmWindow({
      kind: 'delete',
      title: it('end_confirm_title'),
      subject: it('end_confirm_subject', { date: formatDay(s.report.as_of) }),
      consequence: `${it('end_confirm_note', { ...impact, seats: seatsLost(s.report) })} ${endedSummary()}`,
      submitLabel: it('end_confirm_submit'),
      onSubmit: async () => { await runApply(true); },
    });
  }

  async function errorsReport() {
    if (!s.bytes || s.busy) return;
    s.busy = 'errors';
    renderFooter();
    try {
      const body = await ApiBinary.one('orgExportErrorsRequest', payload());
      downloadBytes(body.file_name ?? body.fileName ?? 'import-errors.csv', body.bytes, body.mime);
    } catch (err) {
      TfToast.show({ tone: 'danger', message: it('request_failed', { message: err.message || '' }) });
    }
    s.busy = null;
    renderFooter();
  }

  // What the server read of each row the problem is about: every header with its value, as in the file.
  function showInFile(key) {
    const card = errorCards(s.report, s.resolutions).find((c) => c.key === key);
    if (!card) return;
    const { issue } = card;
    const numbers = [...new Set([...(issue.rows ?? []), issue.row].filter(Boolean))].sort((a, b) => a - b);
    const sections = numbers.map((number) => {
      const parsed = reportRow(s.report, number);
      const cells = (parsed?.cells ?? []).filter((c) => c.value !== '');
      const body = cells.length
        ? `<dl class="org-imp-show-list">${cells.map((c) => `<dt>${escapeHtml(c.header)}</dt><dd>${escapeHtml(c.value)}</dd>`).join('')}</dl>`
        : `<div class="org-imp-empty">${escapeHtml(it('show_no_cells'))}</div>`;
      return `<section class="org-imp-show-row"><div class="org-imp-caption">${escapeHtml(it('show_row_title', { row: number }))}</div>${body}</section>`;
    }).join('');
    const info = document.createElement('tf-window');
    info.setAttribute('title', s.report.sheet
      ? it('show_title_sheet', { sheet: s.report.sheet, rows: rowsLabel(issue) })
      : it('show_title', { rows: rowsLabel(issue) }));
    info.setAttribute('icon', 'file');
    info.setAttribute('buttons', 'close');
    info.setAttribute('modal', '');
    info.setAttribute('draggable', '');
    info.setAttribute('width', '560');
    info.setAttribute('min-width', '360');
    info.setAttribute('initial-x', 'center');
    info.setAttribute('initial-y', 'center');
    info.classList.add('org-imp-show');
    info.innerHTML = `
      <div slot="body" class="org-imp-show-body">
        <div class="org-imp-card-title">${escapeHtml(issueTitle(issue))}</div>
        ${sections}
      </div>
      <div slot="footer"><tf-button variant="secondary" data-act="close-show">${escapeHtml(it('show_close'))}</tf-button></div>`;
    info.addEventListener('click', (e) => { if (e.target.closest('[data-act="close-show"]')) info.close(true); });
    document.body.appendChild(info);
  }

  async function pickFile(file) {
    if (!file) return;
    const format = formatOfFile(file.name);
    s.seq += 1;
    s.file = file;
    s.report = null;
    s.bytes = null;
    s.resolutions = [];
    s.confirmBackdated = false;
    if (!format) {
      s.failure = it('file_bad_type');
      render();
      return;
    }
    if (file.size > MAX_FILE_BYTES) {
      s.failure = it('file_error_file_too_large');
      render();
      return;
    }
    s.format = format;
    s.bytes = new Uint8Array(await file.arrayBuffer());
    await dryRun();
  }

  // ---------------------------------------------------------------------------
  // Events
  // ---------------------------------------------------------------------------

  win.addEventListener('change', (e) => {
    const role = e.target.closest?.('[data-role]')?.dataset.role;
    if (role === 'file') {
      pickFile(e.detail?.files?.[0]);
    } else if (role === 'mode') {
      const mode = String(e.detail?.value ?? 'upsert');
      if (mode === s.mode) return;
      s.mode = mode;
      if (s.bytes) dryRun(); else render();
    } else if (role === 'asof') {
      const value = String(e.detail?.value ?? '');
      if (!isIsoDay(value) || value === s.asOf) return;
      s.asOf = value;
      if (s.bytes) dryRun();
    } else if (role === 'tabs') {
      s.tab = String(e.detail?.value ?? 'errors');
      if (s.report) renderList();
    }
  });

  win.addEventListener('click', (e) => {
    const target = e.target.closest('[data-act]');
    if (!target || !win.contains(target) || target.hasAttribute('disabled')) return;
    switch (target.dataset.act) {
      case 'decide':
        s.resolutions = withResolution(s.resolutions, Number(target.dataset.row), target.dataset.action, target.dataset.login || null);
        dryRun();
        break;
      case 'confirm-backdated':
        s.confirmBackdated = true;
        dryRun();
        break;
      case 'show': showInFile(target.dataset.index); break;
      case 'errors-report': errorsReport(); break;
      case 'apply': apply(); break;
      case 'cancel': win.close(); break;
      default: break;
    }
  });

  win.addEventListener('close-request', (e) => { if (s.busy === 'apply') e.preventDefault(); });

  document.body.appendChild(win);
  render();
  return win;
}

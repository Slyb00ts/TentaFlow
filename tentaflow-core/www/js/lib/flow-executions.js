// ===== File: lib/flow-executions.js — the execution history table of a flow (Flows → history) =====
//
// The table lives here, apart from `modules/flows.js`, so it can be checked
// without the flow builder. Each run shows the correlation id of its chain
// (mono, shortened, the full id in its title) and, when it has one, a link
// to the Events timeline of that chain. A run without a correlation id (one
// that ran before ids were recorded) shows a dash and no link.

import { escapeHtml, escapeAttr, formatDate } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { shortCorrelation, eventsCorrelationHref } from '/js/lib/correlation.js';

function execStatus(status) {
  const v = (status || '').toLowerCase();
  if (v === 'completed' || v === 'success' || v === 'ok') return 'ok';
  if (v === 'running' || v === 'pending') return 'info';
  return 'warn';
}

function correlationCell(execution) {
  const id = execution.correlationId;
  if (!id) return '—';
  return `<code style="font-size:11px;" title="${escapeAttr(id)}">${escapeHtml(shortCorrelation(id))}</code>`;
}

function timelineCell(execution) {
  if (!execution.correlationId) return `<span style="color:var(--text-3);">${escapeHtml(I18n.t('flows.exec_no_timeline'))}</span>`;
  return `<a class="exec-timeline-link" href="${escapeAttr(eventsCorrelationHref(execution.correlationId))}">${escapeHtml(I18n.t('flows.exec_timeline'))} →</a>`;
}

/** The rows of the history table for `executions`. */
export function executionRowsHtml(executions) {
  return executions.map((e) => `
    <tr>
      <td><code style="font-size:11px;">${escapeHtml(e.id)}</code></td>
      <td><tf-chip status="${execStatus(e.status)}">${escapeHtml(e.status)}</tf-chip></td>
      <td class="exec-correlation">${correlationCell(e)}</td>
      <td style="font-size:12px;color:var(--text-3);">${formatDate(e.startedAtEpoch)}</td>
      <td style="font-size:12px;color:var(--text-3);">${e.completedAtEpoch ? formatDate(e.completedAtEpoch) : '—'}</td>
      <td>${timelineCell(e)}</td>
    </tr>`).join('');
}

/** The whole history table. */
export function executionsTableHtml(executions) {
  return `
    <table class="data-table">
      <thead><tr>
        <th>${escapeHtml(I18n.t('flows.col_exec_id'))}</th>
        <th>${escapeHtml(I18n.t('flows.col_status'))}</th>
        <th>${escapeHtml(I18n.t('flows.col_exec_correlation'))}</th>
        <th>${escapeHtml(I18n.t('flows.col_exec_start'))}</th>
        <th>${escapeHtml(I18n.t('flows.col_exec_end'))}</th>
        <th aria-label="${escapeAttr(I18n.t('flows.exec_timeline'))}"></th>
      </tr></thead>
      <tbody>${executionRowsHtml(executions)}</tbody>
    </table>`;
}

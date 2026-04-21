// =============================================================================
// File: modules/flows-user.js — Read-only flows list for role=user. Reuses
// FlowListRequest (same binary handler admins use); edit/delete UI is omitted.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { byId, escapeHtml, toast, formatDate } from '/js/utils.js';

let flows = [];

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

function statusChip(status) {
  const s = (status || '').toLowerCase();
  const cls = s === 'active' ? 'active' : (s === 'archived' ? 'archived' : 'draft');
  const label = I18n.t(`flows.status_${cls}`);
  return `<span class="flows-status-chip ${cls}">${escapeHtml(label)}</span>`;
}

const FlowsUserScreen = {
  render() {
    return `
      <div class="page-header">
        <div>
          <h1>${sprite('workflow-app')} ${escapeHtml(I18n.t('flows_user.title'))}</h1>
          <div class="sub">${escapeHtml(I18n.t('flows_user.subtitle'))}</div>
        </div>
      </div>
      <div class="card" style="padding: 0;"><div id="flows-user-host"></div></div>`;
  },
  async mount() {
    try {
      flows = await ApiBinary.list('flowListRequest');
      renderTable();
    } catch (err) {
      toast(`${I18n.t('flows_user.load_error')}: ${err.message}`, 'error');
    }
  },
  unmount() { flows = []; },
};

function renderTable() {
  const host = byId('flows-user-host');
  if (!host) return;
  if (flows.length === 0) {
    host.innerHTML = `<div class="empty-state"><div class="empty-state-text">${escapeHtml(I18n.t('flows_user.empty'))}</div></div>`;
    return;
  }
  host.innerHTML = `
    <table class="data-table">
      <thead><tr>
        <th>${escapeHtml(I18n.t('flows_user.col_name'))}</th>
        <th>${escapeHtml(I18n.t('flows_user.col_status'))}</th>
        <th>${escapeHtml(I18n.t('flows_user.col_updated'))}</th>
      </tr></thead>
      <tbody>
        ${flows.map((f) => `
          <tr>
            <td>${escapeHtml(f.name ?? '—')}</td>
            <td>${statusChip(f.status)}</td>
            <td>${f.updatedAtEpoch ? formatDate(f.updatedAtEpoch) : '—'}</td>
          </tr>`).join('')}
      </tbody>
    </table>`;
}

export default FlowsUserScreen;

// =============================================================================
// Plik: modules/flows.js
// Opis: Lista przeplywów + create (otwiera builder) + edit (builder) + delete +
//       historia wykonan. Status chip kolorowy, akcje icon-only.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import {
  byId, escapeHtml, escapeAttr, toast, formatDate,
} from '/js/utils.js';
import { TfWindow } from '/js/components/tf-window.js';
import { openFlowBuilder } from '/js/modules/flows-builder.js';
import { I18n } from '/js/i18n.js';

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

const FlowsScreen = {
  get title() { return I18n.t('flows.list_title'); },
  render() {
    return `
      <div class="page-header">
        <div>
          <h1>${sprite('flow')} ${escapeHtml(I18n.t('flows.list_title'))}</h1>
          <div class="sub">${escapeHtml(I18n.t('flows.subtitle'))}</div>
        </div>
        <div class="actions">
          <tf-button variant="primary" icon="plus" id="btn-new-flow">${escapeHtml(I18n.t('flows.new_flow_btn'))}</tf-button>
        </div>
      </div>
      <div id="flows-host"></div>
      <div id="execs-host"></div>`;
  },
  async mount() {
    byId('btn-new-flow').addEventListener('click', () => newFlow());
    await load();
  },
  unmount() { flows = []; },
};

async function load() {
  try {
    flows = await ApiBinary.list('flowListRequest');
    renderTable();
  } catch (err) {
    toast(`${I18n.t('flows.error_prefix')}: ${err.message}`, 'error');
  }
}

function renderTable() {
  const host = byId('flows-host');
  if (!host) return;
  if (flows.length === 0) {
    host.innerHTML = `
      <div class="empty-big">
        ${sprite('flow')}
        <h3>${escapeHtml(I18n.t('flows.empty_title'))}</h3>
        <p>${escapeHtml(I18n.t('flows.empty_desc'))}</p>
        <tf-button variant="primary" icon="plus" id="empty-new-flow">${escapeHtml(I18n.t('flows.new_flow_btn'))}</tf-button>
      </div>`;
    const btn = byId('empty-new-flow');
    if (btn) btn.addEventListener('click', () => newFlow());
    return;
  }
  host.innerHTML = `
    <table class="data-table">
      <thead>
        <tr>
          <th>${escapeHtml(I18n.t('flows.col_name'))}</th>
          <th>${escapeHtml(I18n.t('flows.col_desc'))}</th>
          <th>${escapeHtml(I18n.t('flows.col_status'))}</th>
          <th>${escapeHtml(I18n.t('flows.col_updated'))}</th>
          <th style="text-align:right;">${escapeHtml(I18n.t('flows.col_actions'))}</th>
        </tr>
      </thead>
      <tbody>
        ${flows.map(renderRow).join('')}
      </tbody>
    </table>`;
  bindRowActions();
}

function renderRow(f) {
  const status = f.status || (f.enabled ? 'active' : 'draft');
  const updated = f.updatedAtEpoch || f.updated_at_epoch || f.updated_at;
  return `
    <tr data-key="flow-${escapeAttr(f.id)}">
      <td data-label="${escapeAttr(I18n.t('flows.col_name'))}"><strong style="color: var(--accent-2);">${escapeHtml(f.name)}</strong></td>
      <td data-label="${escapeAttr(I18n.t('flows.col_desc'))}">${f.description ? escapeHtml(f.description) : '<span style="color:var(--text-3);">—</span>'}</td>
      <td data-label="${escapeAttr(I18n.t('flows.col_status'))}">${statusChip(status)}</td>
      <td data-label="${escapeAttr(I18n.t('flows.col_updated'))}" style="font-size:12px;color:var(--text-3);">${formatDate(updated)}</td>
      <td data-label="${escapeAttr(I18n.t('flows.col_actions'))}" style="text-align:right;">
        <tf-button variant="ghost" size="sm" icon="settings" data-flow-edit="${escapeAttr(f.id)}" title="${escapeAttr(I18n.t('flows.edit_title'))}"></tf-button>
        <tf-button variant="ghost" size="sm" icon="clock" data-flow-execs="${escapeAttr(f.id)}" title="${escapeAttr(I18n.t('flows.history_title_short'))}"></tf-button>
        <tf-button variant="danger" size="sm" icon="trash" data-flow-delete="${escapeAttr(f.id)}" data-flow-name="${escapeAttr(f.name)}" title="${escapeAttr(I18n.t('flows.delete_title'))}"></tf-button>
      </td>
    </tr>`;
}

function bindRowActions() {
  document.querySelectorAll('[data-flow-edit]').forEach((b) => {
    b.onclick = () => openFlowBuilder(b.dataset.flowEdit);
  });
  document.querySelectorAll('[data-flow-execs]').forEach((b) => {
    b.onclick = () => showExecs(b.dataset.flowExecs);
  });
  document.querySelectorAll('[data-flow-delete]').forEach((b) => {
    b.onclick = () => deleteFlow(b.dataset.flowDelete, b.dataset.flowName);
  });
}

async function newFlow() {
  try {
    const resp = await ApiBinary.action('flowCreateRequest', {
      name: I18n.t('flows.default_name'),
      description: null,
      graphJson: '{"nodes":[],"edges":[]}',
    });
    const id = resp?.flowId ?? resp?.flow_id;
    if (!id) throw new Error(I18n.t('flows.create_error_missing_id'));
    openFlowBuilder(id);
  } catch (err) {
    toast(I18n.t('flows.create_error', { error: err.message }), 'error');
  }
}

async function deleteFlow(flowId, flowName) {
  const ok = await TfWindow.confirm({
    title: I18n.t('flows.delete_confirm_title'),
    message: I18n.t('flows.delete_confirm_msg', { name: flowName }),
    description: I18n.t('flows.delete_confirm_desc'),
    confirmLabel: I18n.t('flows.delete_confirm_btn'),
    cancelLabel: I18n.t('flows.delete_cancel_btn'),
    danger: true,
  });
  if (!ok) return;
  try {
    const r = await ApiBinary.action('flowDeleteRequest', { flowId });
    if (r.deleted) {
      toast(I18n.t('flows.deleted_ok'), 'success');
      await load();
    } else {
      toast(I18n.t('flows.delete_not_found'), 'warning');
    }
  } catch (err) {
    toast(`${I18n.t('flows.error_prefix')}: ${err.message}`, 'error');
  }
}

async function showExecs(flowId) {
  try {
    const resp = await ApiBinary.one('flowExecutionsListRequest', { flowId });
    const execs = resp.executions ?? [];
    const host = byId('execs-host');
    if (!host) return;
    host.innerHTML = `
      <div class="card" style="margin-top: var(--space-4);">
        <div class="card-header">
          <h3 class="card-title">${escapeHtml(I18n.t('flows.exec_title', { id: flowId }))}</h3>
          <tf-button variant="ghost" size="sm" icon="x" id="execs-close" title="${escapeAttr(I18n.t('flows.close_title'))}"></tf-button>
        </div>
        ${execs.length === 0 ? `
          <div class="empty-state"><div class="empty-state-text">${escapeHtml(I18n.t('flows.exec_empty'))}</div></div>
        ` : `
          <table class="data-table">
            <thead><tr>
              <th>${escapeHtml(I18n.t('flows.col_exec_id'))}</th>
              <th>${escapeHtml(I18n.t('flows.col_status'))}</th>
              <th>${escapeHtml(I18n.t('flows.col_exec_start'))}</th>
              <th>${escapeHtml(I18n.t('flows.col_exec_end'))}</th>
            </tr></thead>
            <tbody>
              ${execs.map((e) => `
                <tr>
                  <td><code style="font-size:11px;">${escapeHtml(e.id)}</code></td>
                  <td><tf-chip status="${execStatus(e.status)}">${escapeHtml(e.status)}</tf-chip></td>
                  <td style="font-size:12px;color:var(--text-3);">${formatDate(e.startedAtEpoch)}</td>
                  <td style="font-size:12px;color:var(--text-3);">${e.completedAtEpoch ? formatDate(e.completedAtEpoch) : '—'}</td>
                </tr>`).join('')}
            </tbody>
          </table>`}
      </div>`;
    byId('execs-close')?.addEventListener('click', () => { host.innerHTML = ''; });
  } catch (err) {
    toast(`${I18n.t('flows.error_prefix')}: ${err.message}`, 'error');
  }
}

function execStatus(s) {
  const v = (s || '').toLowerCase();
  if (v === 'completed' || v === 'success' || v === 'ok') return 'ok';
  if (v === 'running' || v === 'pending') return 'info';
  return 'warn';
}

export default FlowsScreen;

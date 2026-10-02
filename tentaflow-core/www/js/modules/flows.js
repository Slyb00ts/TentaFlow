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
import { emptyProcessModel, processCommand } from './flows-builder/bpmn.js';
import { openProcessInstances } from './flows-builder/process-monitor.js';
import { openFormWindow } from '/js/lib/actions/form-window.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-table.js';
import '/js/components/tf-input.js';
import '/js/components/tf-textarea.js';

let flows = [];
let mode = 'flow';
let mounted = null;

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

function statusChip(status) {
  const s = (status || '').toLowerCase();
  const cls = s === 'active' ? 'active' : (s === 'archived' ? 'archived' : 'draft');
  const label = I18n.t(`flows.status_${cls}`);
  return `<tf-chip status="${cls === 'active' ? 'ok' : cls === 'archived' ? 'neutral' : 'warn'}">${escapeHtml(label)}</tf-chip>`;
}

const FlowsScreen = {
  get title() { return I18n.t('flows.list_title'); },
  render() {
    return `
      <div class="page-header">
        <div>
          <h1>${sprite('flow')} ${escapeHtml(I18n.t('flows.list_title'))}</h1>
          <div class="sub" id="flows-subtitle">${escapeHtml(I18n.t(mode === 'bpmn' ? 'bpmn.list_description' : 'flows.subtitle'))}</div>
        </div>
        <div class="actions">
          <tf-button variant="secondary" icon="clock" id="btn-process-instances">${escapeHtml(I18n.t('bpmn.my_work'))}</tf-button>
          <tf-button variant="primary" icon="plus" id="btn-new-flow">${escapeHtml(I18n.t('flows.new_flow_btn'))}</tf-button>
        </div>
      </div>
      <tf-tabs id="flows-mode" value="${mode}" variant="underline"><tf-tab id="flow" label="${escapeAttr(I18n.t('flows.list_title'))}"></tf-tab><tf-tab id="bpmn" label="${escapeAttr(I18n.t('bpmn.processes'))}"></tf-tab></tf-tabs>
      <div id="flows-host"></div>
      <div id="execs-host"></div>`;
  },
  async mount() {
    const state = { host: byId('flows-host'), generation: 0, offset: 0, canCreateFlow: false };
    mounted = state;
    byId('btn-new-flow').addEventListener('click', () => mode === 'bpmn' ? newProcess() : newFlow());
    byId('btn-process-instances').addEventListener('click', () => openProcessInstances());
    byId('flows-mode').addEventListener('change', (event) => {
      if (event.target.id !== 'flows-mode') return;
      mode = event.detail.value;
      state.offset = 0;
      byId('execs-host').replaceChildren();
      load();
    });
    try {
      const me = await ApiBinary.one('authMeRequest');
      if (mounted !== state || !state.host.isConnected) return;
      state.canCreateFlow = ['admin', 'power_user'].includes(me.role);
      if (!state.canCreateFlow) { mode = 'bpmn'; byId('flows-mode').value = mode; }
    } catch (error) {
      if (mounted !== state || !state.host.isConnected) return;
      toast(I18n.t('bpmn.request_error', { error: error.message }), 'error');
    }
    await load();
  },
  unmount() { mounted = null; flows = []; },
};

async function load() {
  const state = mounted;
  if (!state || !state.host.isConnected) return;
  const generation = ++state.generation;
  const selectedMode = mode;
  byId('flows-subtitle').textContent = I18n.t(selectedMode === 'bpmn' ? 'bpmn.list_description' : 'flows.subtitle');
  state.host.replaceChildren();
  const button = byId('btn-new-flow');
  button.textContent = I18n.t(mode === 'bpmn' ? 'bpmn.new_process' : 'flows.new_flow_btn');
  button.hidden = mode === 'flow' && !state.canCreateFlow;
  try {
    const response = selectedMode === 'bpmn'
      ? await ApiBinary.one('processDefinitionListRequest', { offset: state.offset, limit: 25 })
      : await ApiBinary.list('flowListRequest');
    if (mounted !== state || generation !== state.generation || !state.host.isConnected) return;
    flows = selectedMode === 'bpmn' ? response.definitions : response;
    renderTable(selectedMode === 'bpmn' ? response.total : flows.length);
  } catch (err) {
    if (mounted !== state || generation !== state.generation || !state.host.isConnected) return;
    toast(`${I18n.t('flows.error_prefix')}: ${err.message}`, 'error');
  }
}

function renderTable(total) {
  const host = byId('flows-host');
  if (!host) return;
  if (total === 0) {
    host.innerHTML = `
      <div class="empty-big">
        ${sprite('flow')}
        <h3>${escapeHtml(I18n.t(mode === 'bpmn' ? 'bpmn.processes_empty' : 'flows.empty_title'))}</h3>
        <p>${escapeHtml(I18n.t(mode === 'bpmn' ? 'bpmn.private_hint' : 'flows.empty_desc'))}</p>
        ${mode === 'bpmn' || mounted.canCreateFlow ? `<tf-button variant="primary" icon="plus" id="empty-new-flow">${escapeHtml(I18n.t(mode === 'bpmn' ? 'bpmn.new_process' : 'flows.new_flow_btn'))}</tf-button>` : ''}
      </div>`;
    const btn = byId('empty-new-flow');
    if (btn) btn.addEventListener('click', () => mode === 'bpmn' ? newProcess() : newFlow());
    return;
  }
  host.innerHTML = `<tf-table id="flows-table" ${mode === 'bpmn' ? `page-size="25" total="${total}" page="${mounted.offset / 25 + 1}"` : ''}>
    <tf-column key="nameHtml" renderer="html" label="${escapeAttr(I18n.t('flows.col_name'))}"></tf-column>
    <tf-column key="description" label="${escapeAttr(I18n.t('flows.col_desc'))}"></tf-column>
    <tf-column key="statusHtml" renderer="html" label="${escapeAttr(I18n.t('flows.col_status'))}"></tf-column>
    <tf-column key="versionLabel" label="${escapeAttr(I18n.t(mode === 'bpmn' ? 'bpmn.published_version' : 'flows.col_updated'))}"></tf-column></tf-table>`;
  const table = host.querySelector('tf-table');
  table.rowKey = mode === 'bpmn' ? 'definitionId' : 'id';
  table.rows = flows.map(renderRow);
  bindRowActions();
  const state = mounted;
  table.addEventListener('page-change', (event) => { if (mounted !== state || !state.host.contains(table)) return; state.offset = (event.detail.page - 1) * 25; load(); });
}

function renderRow(f) {
  if (mode === 'bpmn') return { ...f, nameHtml: `<strong>${escapeHtml(f.name)}</strong>`,
    statusHtml: `<tf-chip status="${f.archived ? 'neutral' : 'info'}">${escapeHtml(I18n.t(f.archived ? 'bpmn.archived' : 'bpmn.draft'))}</tf-chip>`,
    versionLabel: f.publishedVersion === null ? I18n.t('bpmn.not_published') : I18n.t('bpmn.version_number', { version: f.publishedVersion }) };
  const status = f.status || (f.enabled ? 'active' : 'draft');
  const updated = f.updatedAtEpoch || f.updated_at_epoch || f.updated_at;
  // The server rejects edit/delete/status changes on a system flow, so the row
  // offers only a read-only preview and no delete action.
  const isSystem = !!f.isSystem;
  const systemChip = isSystem
    ? ` <tf-chip status="info" data-flow-system="${escapeAttr(f.id)}">${escapeHtml(I18n.t('flows.system_chip'))}</tf-chip>`
    : '';
  // A factory flow is editable but never deletable; instead of delete it gets
  // "restore factory version" (the server refuses delete for it anyway).
  const isFactory = !!f.isFactory;
  const factoryChip = isFactory
    ? ` <tf-chip status="neutral" data-flow-factory="${escapeAttr(f.id)}">${escapeHtml(I18n.t('flows.factory_chip'))}</tf-chip>`
    : '';
  return { ...f, nameHtml: `<strong>${escapeHtml(f.name)}</strong>${systemChip}${factoryChip}`, statusHtml: statusChip(status), versionLabel: formatDate(updated) };
}

function bindRowActions() {
  const process = mode === 'bpmn';
  byId('flows-table').rowActions = (row, _index, current) => {
    const group = document.createElement('div');
    group.className = 'flows-row-actions';
    const add = (icon, label, action, variant = 'ghost') => {
      const button = document.createElement('tf-button');
      button.setAttribute('icon', icon); button.setAttribute('variant', variant); button.setAttribute('size', 'sm');
      button.setAttribute('title', label);
      button.addEventListener('click', () => action(current()));
      group.append(button);
    };
    add(row.isSystem ? 'eye' : 'settings', I18n.t(row.isSystem ? 'flows.preview' : 'flows.edit_title'), (selected) => openFlowBuilder(process ? selected.definitionId : selected.id, { mode: process ? 'bpmn' : 'flow' }));
    add('clock', I18n.t('flows.history_title_short'), (selected) => process ? openProcessInstances(selected.definitionId) : showExecs(selected.id));
    if (!process && row.isFactory) add('refresh', I18n.t('flows.factory_restore'), (selected) => restoreFactoryFlow(selected.id, selected.name));
    if (!process && !row.isSystem && !row.isFactory) add('trash', I18n.t('flows.delete_title'), (selected) => deleteFlow(selected.id, selected.name), 'danger');
    return group;
  };
}

function newProcess() {
  const state = mounted;
  const section = document.createElement('div');
  section.innerHTML = `<tf-input data-name required label="${escapeAttr(I18n.t('bpmn.process_name'))}"></tf-input><tf-textarea data-description label="${escapeAttr(I18n.t('flows.col_desc'))}"></tf-textarea>`;
  const model = emptyProcessModel();
  const command = processCommand();
  return openFormWindow({ title: I18n.t('bpmn.new_process'), icon: 'flow', note: { text: I18n.t('bpmn.private_hint') }, sections: [section], submitLabel: I18n.t('bpmn.create'),
    canSubmit: () => mounted === state && state.host.isConnected && !!section.querySelector('[data-name]').value.trim(),
    collect: () => ({ definitionId: null, expectedRevision: 0, name: section.querySelector('[data-name]').value.trim(), description: section.querySelector('[data-description]').value, model }),
    onSubmit: async (payload) => {
      const response = await ApiBinary.one('processDefinitionSaveRequest', command(payload));
      if (mounted === state && state.host.isConnected) openFlowBuilder(response.definition.definitionId, { mode: 'bpmn' });
    },
  });
}

async function restoreFactoryFlow(flowId, flowName) {
  const ok = await TfWindow.confirm({
    title: I18n.t('flows.factory_restore'),
    message: I18n.t('flows.factory_restore_confirm', { name: flowName }),
    description: I18n.t('flows.factory_restore_desc'),
    confirmLabel: I18n.t('flows.factory_restore_btn'),
    cancelLabel: I18n.t('flows.delete_cancel_btn'),
  });
  if (!ok) return;
  try {
    await ApiBinary.action('flowFactoryRestoreRequest', { flowId });
    toast(I18n.t('flows.factory_restored_ok', { name: flowName }), 'success');
    await load();
  } catch (err) {
    toast(`${I18n.t('flows.error_prefix')}: ${err.message}`, 'error');
  }
}

async function newFlow() {
  try {
    const resp = await ApiBinary.action('flowCreateRequest', {
      name: I18n.t('flows.default_name'),
      description: null,
      // R5 (flow_engine/validation.rs) rejects a graph with zero entry nodes,
      // so a brand-new flow needs a seeded `trigger` node from the start —
      // same node/graph shape the builder canvas writes on save (`position`
      // nested, not flat x/y).
      graphJson: '{"nodes":[{"id":"t1","type":"trigger","position":{"x":0,"y":0},"config":{}}],"edges":[]}',
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

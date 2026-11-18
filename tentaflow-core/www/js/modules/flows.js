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
import { executionsTableHtml } from '/js/lib/flow-executions.js';
import { emptyProcessModel, processCommand } from './flows-builder/bpmn.js';
import { openProcessInstances } from './flows-builder/process-monitor.js';
import { busConsumerText, busConsumerChipStatus } from './flows-builder/bus-consumer.js';
import { openFormWindow } from '/js/lib/actions/form-window.js';
import { setRowsIfChanged } from '/js/lib/dom-patch.js';
import { Router } from '/js/router.js';
import { slaCellHtml, paintSlaCells } from '/js/modules/sla/flow-cell.js';
import { loadBusPolicyIds, loadPolicies, policiesById, policyActions, renderPolicyTab } from '/js/modules/sla/policies-tab.js';
import { openAssignPolicyWindow } from '/js/modules/sla/assign-window.js';
import { startSlaState } from '/js/modules/sla/exec-state.js';
import { T as slaT, assignAction, failureText, newestRequest, slaView } from '/js/modules/sla/model.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-table.js';
import '/js/components/tf-input.js';
import '/js/components/tf-textarea.js';

let flows = [];
let mode = 'flow';
let mounted = null;
// How often the flow list refreshes the SLA state of its rows. The refresh writes into the cells
// that are already on screen; it never draws the table again.
const SLA_POLL_MS = 5000;
const VOLATILE_ROW_FIELDS = new Set(['slaBrief', 'busConsumer']);

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

/** `active`, `archived` or `draft`: the key of a flow's status label. */
const statusKey = (status) => {
  const s = (status || '').toLowerCase();
  return s === 'active' ? 'active' : (s === 'archived' ? 'archived' : 'draft');
};

function statusChip(status) {
  const cls = statusKey(status);
  const label = I18n.t(`flows.status_${cls}`);
  return `<tf-chip status="${cls === 'active' ? 'ok' : cls === 'archived' ? 'neutral' : 'warn'}">${escapeHtml(label)}</tf-chip>`;
}

function subtitleText(selectedMode, canManagePolicies) {
  if (selectedMode === 'bpmn') return I18n.t('bpmn.list_description');
  if (selectedMode === 'sla') return slaT(canManagePolicies ? 'subtitle' : 'subtitle_readonly');
  return I18n.t('flows.subtitle');
}

const FlowsScreen = {
  get title() { return I18n.t('flows.list_title'); },
  render() {
    return `
      <div class="page-header">
        <div>
          <h1>${sprite('flow')} ${escapeHtml(I18n.t('flows.list_title'))}</h1>
          <div class="sub" id="flows-subtitle">${escapeHtml(subtitleText(mode, true))}</div>
        </div>
        <div class="actions">
          <tf-button variant="secondary" icon="clock" id="btn-process-instances">${escapeHtml(I18n.t('bpmn.my_work'))}</tf-button>
          <tf-button variant="primary" icon="plus" id="btn-new-flow">${escapeHtml(I18n.t('flows.new_flow_btn'))}</tf-button>
        </div>
      </div>
      <tf-tabs id="flows-mode" value="${mode}" variant="underline"><tf-tab id="flow" label="${escapeAttr(I18n.t('flows.list_title'))}"></tf-tab><tf-tab id="bpmn" label="${escapeAttr(I18n.t('bpmn.processes'))}"></tf-tab><tf-tab id="sla" label="${escapeAttr(slaT('tab'))}"></tf-tab></tf-tabs>
      <div id="flows-host"></div>
      <div id="execs-host"></div>`;
  },
  async mount(params = {}) {
    const state = {
      host: byId('flows-host'), generation: 0, offset: 0, canCreateFlow: false,
      policies: new Map(), policyList: [], canManagePolicies: false, statuses: new Map(), polling: false, timer: null, execState: null, execs: newestRequest(),
    };
    mounted = state;
    if (params?.tab === 'sla') { mode = 'sla'; byId('flows-mode').value = mode; }
    byId('btn-new-flow').addEventListener('click', () => {
      if (mode === 'sla') policyHandlers().onNew();
      else if (mode === 'bpmn') newProcess();
      else newFlow();
    });
    byId('btn-process-instances').addEventListener('click', () => openProcessInstances());
    byId('flows-mode').addEventListener('change', (event) => {
      if (event.target.id !== 'flows-mode') return;
      mode = event.detail.value;
      state.offset = 0;
      state.execs.cancel();
      stopExecState();
      byId('execs-host').replaceChildren();
      load();
    });
    try {
      const me = await ApiBinary.one('authMeRequest');
      if (mounted !== state || !state.host.isConnected) return;
      state.canCreateFlow = ['admin', 'power_user'].includes(me.role);
      if (!state.canCreateFlow && mode === 'flow') { mode = 'bpmn'; byId('flows-mode').value = mode; }
    } catch (error) {
      if (mounted !== state || !state.host.isConnected) return;
      toast(I18n.t('bpmn.request_error', { error: error.message }), 'error');
    }
    state.timer = setInterval(pollSla, SLA_POLL_MS);
    await load();
  },
  unmount() {
    if (mounted?.timer) clearInterval(mounted.timer);
    stopExecState();
    mounted = null;
    flows = [];
  },
};

async function load() {
  const state = mounted;
  if (!state || !state.host.isConnected) return;
  const generation = ++state.generation;
  const selectedMode = mode;
  byId('flows-subtitle').textContent = subtitleText(selectedMode, state.canManagePolicies);
  state.host.replaceChildren();
  const button = byId('btn-new-flow');
  button.textContent = I18n.t(mode === 'bpmn' ? 'bpmn.new_process' : (mode === 'sla' ? 'sla.new_policy' : 'flows.new_flow_btn'));
  button.hidden = mode === 'flow' ? !state.canCreateFlow : (mode === 'sla' && !state.canManagePolicies);
  const stale = () => mounted !== state || generation !== state.generation || !state.host.isConnected;
  try {
    if (selectedMode === 'sla') {
      const [response, busPolicyIds] = await Promise.all([loadPolicies(), loadBusPolicyIds()]);
      if (stale()) return;
      state.policyList = response.policies;
      state.policies = policiesById(response.policies);
      state.canManagePolicies = response.canManage;
      byId('flows-subtitle').textContent = subtitleText('sla', state.canManagePolicies);
      button.hidden = !state.canManagePolicies;
      renderPolicyTab(state.host, response, policyHandlers(), { busPolicyIds });
      return;
    }
    if (selectedMode === 'bpmn') {
      const response = await ApiBinary.one('processDefinitionListRequest', { offset: state.offset, limit: 25 });
      if (stale()) return;
      flows = response.definitions;
      renderTable(response.total);
      return;
    }
    const [list, policies, statuses] = await Promise.all([
      ApiBinary.list('flowListRequest', { arrayKey: 'flows' }),
      loadPolicies().catch(() => null),
      readSlaStatuses(),
    ]);
    if (stale()) return;
    flows = list;
    state.statuses = statuses;
    if (policies) {
      state.policyList = policies.policies;
      state.policies = policiesById(policies.policies);
      state.canManagePolicies = policies.canManage;
    }
    renderTable(flows.length);
  } catch (err) {
    if (stale()) return;
    toast(selectedMode === 'sla' ? failureText(err) : `${I18n.t('flows.error_prefix')}: ${err.message}`, 'error');
  }
}

/** The state of every governed flow of the caller's organization, by flow id; empty when it cannot be read. */
async function readSlaStatuses() {
  try {
    const response = await ApiBinary.one('slaStatusRequest', {});
    return new Map((response.statuses ?? []).map((status) => [status.flowId, status]));
  } catch {
    return new Map();
  }
}

function policyHandlers() {
  const state = mounted;
  return policyActions({
    policies: () => state?.policyList ?? [],
    onChanged: () => { if (mounted === state && mode === 'sla') load(); },
  });
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
    ${mode === 'bpmn' ? '' : `<tf-column key="slaHtml" renderer="html" label="${escapeAttr(slaT('col_sla'))}"></tf-column>`}
    <tf-column key="statusHtml" renderer="html" label="${escapeAttr(I18n.t('flows.col_status'))}"></tf-column>
    <tf-column key="versionLabel" label="${escapeAttr(I18n.t(mode === 'bpmn' ? 'bpmn.published_version' : 'flows.col_updated'))}"></tf-column></tf-table>`;
  const table = host.querySelector('tf-table');
  table.rowKey = mode === 'bpmn' ? 'definitionId' : 'id';
  bindRowActions();
  if (mode === 'bpmn') table.rows = flows.map(renderRow);
  else paintFlowRows(table);
  const state = mounted;
  table.addEventListener('page-change', (event) => { if (mounted !== state || !state.host.contains(table)) return; state.offset = (event.detail.page - 1) * 25; load(); });
}

function renderRow(f) {
  if (mode === 'bpmn') return { ...f, nameHtml: `<strong>${escapeHtml(f.name)}</strong>`,
    statusHtml: `<tf-chip status="${f.archived ? 'neutral' : 'info'}">${escapeHtml(I18n.t(f.archived ? 'bpmn.archived' : 'bpmn.draft'))}</tf-chip>`,
    versionLabel: f.publishedVersion === null ? I18n.t('bpmn.not_published') : I18n.t('bpmn.version_number', { version: f.publishedVersion }) };
  const status = f.status || (f.enabled ? 'active' : 'draft');
  const updated = f.updatedAtEpoch;
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
  // A flow that reads the bus says so when it does not: the chip names the state, the title adds the reason and the node.
  const consumer = busConsumerText(f.busConsumer);
  const consumerChip = consumer && consumer.state !== 'running'
    ? ` <tf-chip status="${busConsumerChipStatus(consumer.state)}" title="${escapeAttr(consumer.detail)}" data-flow-consumer="${escapeAttr(f.id)}">${escapeHtml(consumer.text)}</tf-chip>`
      + (consumer.reason ? `<div class="flows-consumer-reason" data-flow-consumer-reason="${escapeAttr(f.id)}">${escapeHtml(consumer.reason)}</div>` : '')
    : '';
  const sla = slaView(f, mounted.policies, mounted.statuses.get(f.id));
  return {
    ...f,
    nameHtml: `<strong>${escapeHtml(f.name)}</strong>${systemChip}${factoryChip}${publishedChipHtml(f)}${consumerChip}`,
    slaHtml: slaCellHtml(f.id, sla),
    slaKind: sla.kind,
    statusHtml: statusChip(status),
    versionLabel: formatDate(updated),
  };
}

/**
 * The chip of a flow offered as a /v1 model: `/v1 · <model-name>`. The outline variant is sentence-case,
 * so the name keeps the case it was published with (the plain pill upper-cases its text).
 */
export function publishedChipHtml(f) {
  const name = (f.publishedModelName ?? '').trim();
  if (!name) return '';
  return ` <tf-chip status="info" variant="outline" data-flow-published="${escapeAttr(f.id)}" title="${escapeAttr(I18n.t('flow_publication.chip_title', { name }))}">/v1 · ${escapeHtml(name)}</tf-chip>`;
}

/** The SLA views of every flow row, by flow id. */
function slaViews() {
  return new Map(flows.map((flow) => [flow.id, slaView(flow, mounted.policies, mounted.statuses.get(flow.id))]));
}

/**
 * Gives the table its rows and paints the SLA values. The rows hold only what does not move
 * between polls (the volatile counters are left out of the comparison), so a refresh that changed
 * nothing visible assigns nothing; the SLA cells are written into the nodes already on screen.
 */
function paintFlowRows(table) {
  setRowsIfChanged(table, flows.map(renderRow), VOLATILE_ROW_FIELDS);
  paintSlaCells(table.shadowRoot, slaViews());
}

/** Refreshes the SLA column of the flow list in place. */
async function pollSla() {
  const state = mounted;
  if (!state || mode !== 'flow' || state.polling || document.hidden) return;
  state.polling = true;
  const generation = state.generation;
  try {
    const [list, statuses] = await Promise.all([ApiBinary.list('flowListRequest', { arrayKey: 'flows' }), readSlaStatuses()]);
    if (mounted !== state || mode !== 'flow' || generation !== state.generation || !state.host.isConnected) return;
    flows = list;
    state.statuses = statuses;
    const table = byId('flows-table');
    if (table) paintFlowRows(table);
    else renderTable(flows.length);
  } catch {
    // The next tick tries again; a failed refresh keeps what is on screen.
  } finally {
    state.polling = false;
  }
}

function bindRowActions() {
  const process = mode === 'bpmn';
  byId('flows-table').rowActions = (row, _index, current) => {
    const group = document.createElement('div');
    group.className = 'flows-row-actions';
    const add = (icon, label, action, variant = 'ghost', disabled = false) => {
      const button = document.createElement('tf-button');
      button.setAttribute('icon', icon); button.setAttribute('variant', variant); button.setAttribute('size', 'sm');
      button.setAttribute('title', label);
      if (disabled) button.setAttribute('disabled', '');
      else button.addEventListener('click', () => action(current()));
      group.append(button);
    };
    const assign = process ? { show: false } : assignAction({ flow: row, kind: row.slaKind, canManage: mounted.canManagePolicies, canEditFlows: mounted.canCreateFlow });
    if (assign.show) {
      add('shield', assign.title, (selected) => openAssignPolicyWindow({ flow: selected, policies: mounted.policyList, onChanged: () => load() }), 'ghost', assign.disabled);
    }
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
    const id = resp?.flowId;
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
    if (String(err.message).includes('[flow:has_history]')) {
      toast(I18n.t('flows.delete_has_history'), 'error');
      return;
    }
    toast(`${I18n.t('flows.error_prefix')}: ${err.message}`, 'error');
  }
}

function stopExecState() {
  if (!mounted?.execState) return;
  mounted.execState.dispose();
  mounted.execState = null;
}

// The list answers one page without a total: a full page means "the latest N", not "N in all".
const EXEC_PAGE_SIZE = 100;
export const execMetaKey = (count) => (count >= EXEC_PAGE_SIZE ? 'flows.exec_meta_latest' : 'flows.exec_meta');

async function showExecs(flowId) {
  const state = mounted;
  if (!state) return;
  const newest = state.execs.begin();
  // An answer that arrives after another flow was asked for, or after the tab changed, draws nothing.
  const current = () => mounted === state && newest() && mode === 'flow';
  try {
    const resp = await ApiBinary.one('flowExecutionsListRequest', { flowId });
    if (!current()) return;
    const execs = resp.executions ?? [];
    const host = byId('execs-host');
    if (!host) return;
    stopExecState();
    const flow = flows.find((item) => item.id === flowId);
    const governed = Boolean(flow?.slaPolicyId) && !flow.isSystem;
    host.innerHTML = `
      ${governed ? '<div id="sla-state-host"></div>' : ''}
      <div class="card" style="margin-top: var(--space-4);">
        <div class="card-header">
          <div>
            <h3 class="card-title">${escapeHtml(I18n.t('flows.exec_title', { name: flow?.name || flowId }))}</h3>
            <div class="hint" style="margin:0">${escapeHtml(I18n.t(execMetaKey(execs.length), { id: flowId, count: execs.length, n: execs.length, status: I18n.t(`flows.status_${statusKey(flow?.status || (flow?.enabled ? 'active' : 'draft'))}`) }))}</div>
          </div>
          <tf-button variant="ghost" size="sm" icon="x" id="execs-close" title="${escapeAttr(I18n.t('flows.close_title'))}"></tf-button>
        </div>
        ${execs.length === 0 ? `
          <div class="empty-state"><div class="empty-state-text">${escapeHtml(I18n.t('flows.exec_empty'))}</div></div>
        ` : executionsTableHtml(execs)}
      </div>`;
    byId('execs-close')?.addEventListener('click', () => { stopExecState(); host.innerHTML = ''; });
    if (governed) {
      mounted.execState = startSlaState({
        host: byId('sla-state-host'),
        flow,
        onOpenPolicies: () => Router.navigate('flows', { tab: 'sla' }),
      });
    }
  } catch (err) {
    if (!current()) return;
    toast(`${I18n.t('flows.error_prefix')}: ${err.message}`, 'error');
  }
}

export default FlowsScreen;

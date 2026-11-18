// ===== File: modules/sla/policies-tab.js — Przepływy → "Polityki SLA": the list of policies and its row actions =====
//
// Without `sla.manage` (`PolicyListResponse.canManage` false) the tab is a
// read-only view: no "Nowa polityka" and no buttons in the rows. A policy that
// governs flows has its delete button disabled, and the title says how many.

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeAttr, escapeHtml, toast } from '/js/utils.js';
import { TfWindow } from '/js/components/tf-window.js';
import '/js/components/tf-table.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-button.js';
import { T, callsText, deleteBlockedText, failureText, thresholdsText, threadsText, unlessRefused } from './model.js';
import { openPolicyEditor } from './policy-editor.js';

const MUTED = 'font-size:12px;color:var(--text-3)';
const sub = (text) => (text ? `<div style="${MUTED}">${escapeHtml(text)}</div>` : '');
const cell = ({ main, sub: second }) => `${escapeHtml(main)}${sub(second)}`;

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

/** One row of the policy table (plain data, so an unchanged list can be recognised). */
export function policyRow(policy) {
  const { spec } = policy;
  const count = policy.assignedFlows ?? 0;
  return {
    id: policy.id,
    name: spec.name,
    assignedFlows: count,
    nameHtml: `<strong>${escapeHtml(spec.name)}</strong>${sub(spec.description)}`,
    threads: cell(threadsText(spec)),
    calls: cell(callsText(spec)),
    thresholds: cell(thresholdsText(spec)),
    services: count > 0
      ? `<tf-chip status="accent" size="sm" variant="outline">${escapeHtml(T('services_count', { count, n: count }))}</tf-chip>`
      : `<span style="color:var(--text-3)">${escapeHtml(T('services_none'))}</span>`,
  };
}

function iconButton(icon, label, variant, onClick, blocked = false) {
  const button = document.createElement('tf-button');
  button.setAttribute('icon', icon);
  button.setAttribute('variant', variant);
  button.setAttribute('size', 'sm');
  button.setAttribute('title', label);
  button.setAttribute('aria-label', label);
  if (blocked) button.setAttribute('disabled', '');
  else button.addEventListener('click', onClick);
  return button;
}

/** The row buttons: edit, and delete (disabled, with the reason, while the policy governs flows). */
export function policyRowActions(row, current, handlers) {
  const group = document.createElement('div');
  group.className = 'flows-row-actions';
  group.append(iconButton('settings', T('edit_policy'), 'ghost', () => handlers.onEdit(current().id)));
  const blocked = row.assignedFlows > 0;
  group.append(iconButton('trash', blocked ? deleteBlockedText(row.assignedFlows) : T('delete_policy'), 'danger', () => handlers.onDelete(current().id), blocked));
  return group;
}

/**
 * Draws the tab into `host`. `response` is the `PolicyListResponse`;
 * `handlers` = `{ onNew, onEdit(id), onDelete(id) }`. Returns the table (or `null` when empty).
 */
export function renderPolicyTab(host, response, handlers) {
  const { policies, canManage } = response;
  if (policies.length === 0) {
    host.innerHTML = `
      <div class="empty-big">
        ${sprite('shield')}
        <h3>${escapeHtml(T('empty_title'))}</h3>
        <p>${escapeHtml(T(canManage ? 'empty_desc' : 'empty_readonly'))}</p>
        ${canManage ? `<tf-button variant="primary" icon="plus" id="empty-new-policy">${escapeHtml(T('new_policy'))}</tf-button>` : ''}
      </div>`;
    host.querySelector('#empty-new-policy')?.addEventListener('click', () => handlers.onNew());
    return null;
  }
  host.innerHTML = `<tf-table id="sla-table">
    <tf-column key="nameHtml" renderer="html" label="${escapeAttr(T('col_name'))}"></tf-column>
    <tf-column key="threads" renderer="html" label="${escapeAttr(T('col_threads'))}"></tf-column>
    <tf-column key="calls" renderer="html" label="${escapeAttr(T('col_calls'))}"></tf-column>
    <tf-column key="thresholds" renderer="html" label="${escapeAttr(T('col_thresholds'))}"></tf-column>
    <tf-column key="services" renderer="html" label="${escapeAttr(T('col_services'))}"></tf-column>
  </tf-table><tf-alert tone="info" message="${escapeAttr(T('info_note'))}" style="display: block; margin-top: 12px; "></tf-alert>`;
  const table = host.querySelector('tf-table');
  table.rowKey = 'id';
  if (canManage) {
    table.rowActions = (row, _index, current) => policyRowActions(row, current, handlers);
  }
  table.rows = policies.map(policyRow);
  return table;
}

/** Reads the policies. The list answers for the caller's own organization. */
export const loadPolicies = () => ApiBinary.one('slaPolicyListRequest', {});

/** Index of a `PolicyListResponse` by policy id. */
export const policiesById = (policies) => new Map(policies.map((policy) => [policy.id, policy]));

/**
 * The tab's actions: new, edit and delete, each followed by `onChanged()` so the
 * screen reloads the list. `policies()` answers the current list.
 */
export function policyActions({ policies, onChanged }) {
  const find = (id) => policies().find((policy) => policy.id === id);
  return {
    onNew: () => openPolicyEditor({ onSaved: onChanged }),
    onEdit: (id) => {
      const policy = find(id);
      if (policy) openPolicyEditor({ policy, onSaved: onChanged });
    },
    onDelete: async (id) => {
      const policy = find(id);
      if (!policy) return;
      const ok = await TfWindow.confirm({
        title: T('delete_confirm_title'),
        message: T('delete_confirm_msg', { name: policy.spec.name }),
        description: T('delete_confirm_desc'),
        confirmLabel: T('delete_confirm_btn'),
        cancelLabel: T('delete_cancel_btn'),
        danger: true,
      });
      if (!ok) return;
      try {
        unlessRefused(await ApiBinary.action('slaPolicyDeleteRequest', { policyId: id }));
        toast(T('deleted_ok', { name: policy.spec.name }), 'success');
        onChanged();
      } catch (err) {
        toast(failureText(err), 'error');
        onChanged();
      }
    },
  };
}

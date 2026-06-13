// =============================================================================
// Plik: modules/addons/access.js
// Opis: Tab Access dla detail addona (mockup M12b). Pokazuje zasoby ktore
//       addon UZYWA (aliasy + modele z manifestu, ze statusem grantu) oraz
//       zasoby ktore UDOSTEPNIA (wlasne aliasy, reverse view). Admin moze
//       approve/deny oczekujacych grantow; non-admin widzi tylko status.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import '/js/components/tf-table.js';
import '/js/components/tf-button.js';

let currentAddonId = null;
let currentAddonName = '';
let currentContainer = null;
let isAdminView = false;

let usesAlias = [];   // [{target, required, reason, grantStatus, ownerVisibility}]
let usesModel = [];   // [{...}]
let provides = [];    // [{alias, visibility}] derived from owned aliases

export const AccessTab = {
  async mount(container, addonId, { addonName = '', isAdmin = false } = {}) {
    currentAddonId = addonId;
    currentAddonName = addonName || addonId;
    currentContainer = container;
    isAdminView = !!isAdmin;
    await loadAll();
  },

  unmount() {
    currentAddonId = null;
    currentAddonName = '';
    currentContainer = null;
    isAdminView = false;
    usesAlias = [];
    usesModel = [];
    provides = [];
  },
};

async function loadAll() {
  if (!currentContainer) return;
  currentContainer.innerHTML = `<div class="addons-empty">${escapeHtml(I18n.t('common.loading'))}</div>`;
  try {
    const resp = await ApiBinary.one('addonAccessListRequest', { addonId: currentAddonId });
    usesAlias = (resp.usesAlias || resp.uses_alias || []).map(normalizeUse);
    usesModel = (resp.usesModel || resp.uses_model || []).map(normalizeUse);
    provides = await loadProvides();
    render();
  } catch (err) {
    currentContainer.innerHTML = `<div class="addons-empty" style="color:var(--danger);">${escapeHtml(err.message)}</div>`;
  }
}

function normalizeUse(u) {
  return {
    target: u.target ?? u.id ?? '',
    required: !!(u.required ?? false),
    reason: u.reason ?? '',
    grantStatus: u.grantStatus ?? u.grant_status ?? 'pending',
    ownerVisibility: u.ownerVisibility ?? u.owner_visibility ?? 'private',
  };
}

// Provides is a reverse view: this addon's OWN aliases. The addon-detail
// context carries no such list, so we derive it from modelAliasListRequest
// filtered by owner — the same ownership heuristic the Bindings tab uses
// until ModelAliasEntry exposes owner_addon_id. Addons can only own aliases
// (not models), so there is no models-provided section.
async function loadProvides() {
  try {
    const list = await ApiBinary.list('modelAliasListRequest', { arrayKey: 'aliases' });
    const prefix = String(currentAddonId || '').toLowerCase();
    if (!prefix) return [];
    return list
      .filter((a) => {
        const name = String(a.alias || '').toLowerCase();
        return name === prefix || name.startsWith(prefix + '-') || name.startsWith(prefix + '_');
      })
      .map((a) => ({
        alias: a.alias ?? '',
        visibility: normalizeVisibility(a.visibility ?? a.owner_visibility ?? a.ownerVisibility),
      }));
  } catch (_) {
    return [];
  }
}

function normalizeVisibility(v) {
  const s = String(v || '').toLowerCase();
  return (s === 'public' || s === 'restricted' || s === 'private') ? s : 'private';
}

function render() {
  if (!currentContainer) return;
  currentContainer.innerHTML = `
    <div class="alert info">
      <svg class="icon" width="18" height="18"><use href="#i-info"/></svg>
      <div>${escapeHtml(I18n.t('addon_access.help_text'))}</div>
    </div>

    <div class="section-card">
      <h3><svg class="icon icon-sm"><use href="#i-link"/></svg>${escapeHtml(
        I18n.t('addon_access.uses_aliases_title', { name: currentAddonName }))}
        <span class="muted">· ${usesAlias.length}</span></h3>
      <div class="section-sub">${escapeHtml(I18n.t('addon_access.uses_aliases_subtitle'))}</div>
      <div id="access-uses-alias"></div>
    </div>

    <div class="section-card">
      <h3><svg class="icon icon-sm"><use href="#i-cpu"/></svg>${escapeHtml(
        I18n.t('addon_access.uses_models_title', { name: currentAddonName }))}
        <span class="muted">· ${usesModel.length}</span></h3>
      <div class="section-sub">${escapeHtml(I18n.t('addon_access.uses_models_subtitle'))}</div>
      <div id="access-uses-model"></div>
    </div>

    <div class="section-card">
      <h3><svg class="icon icon-sm"><use href="#i-eye"/></svg>${escapeHtml(
        I18n.t('addon_access.provides_title', { name: currentAddonName }))}
        <span class="muted">· ${provides.length}</span></h3>
      <div class="section-sub">${escapeHtml(I18n.t('addon_access.provides_subtitle'))}</div>
      <div id="access-provides"></div>
    </div>
  `;

  renderUsesTable(currentContainer.querySelector('#access-uses-alias'), usesAlias, 'alias');
  renderUsesTable(currentContainer.querySelector('#access-uses-model'), usesModel, 'model');
  renderProvidesTable(currentContainer.querySelector('#access-provides'));
}

function renderUsesTable(host, rows, kind) {
  if (!host) return;
  if (rows.length === 0) {
    host.innerHTML = `<div class="addons-empty">${escapeHtml(I18n.t('addon_access.uses_empty'))}</div>`;
    return;
  }
  const targetLabel = kind === 'alias'
    ? I18n.t('addon_access.col_alias_id')
    : I18n.t('addon_access.col_model_id');
  host.innerHTML = `
    <tf-table>
      <tf-column key="target" label="${escapeAttr(targetLabel)}"></tf-column>
      <tf-column key="required" label="${escapeAttr(I18n.t('addon_access.col_required'))}" renderer="chip"></tf-column>
      <tf-column key="reason" label="${escapeAttr(I18n.t('addon_access.col_reason'))}"></tf-column>
      <tf-column key="visibility" label="${escapeAttr(I18n.t('addon_access.col_owner_visibility'))}" renderer="chip"></tf-column>
      <tf-column key="grant" label="${escapeAttr(I18n.t('addon_access.col_grant_status'))}" renderer="chip"></tf-column>
    </tf-table>
  `;
  const table = host.querySelector('tf-table');
  table.rows = rows.map((r) => useToRow(r, kind));
  // Admin-only approve/deny on pending rows. Non-admins and resolved rows
  // (granted/auto_granted/denied) render no action cell content.
  if (isAdminView) {
    table.rowActions = (row) => buildUseActions(row, kind);
  }
}

function useToRow(r, kind) {
  return {
    _kind: kind,
    _target: r.target,
    _grantStatus: r.grantStatus,
    target: r.target,
    required: {
      status: r.required ? 'err' : 'info',
      label: I18n.t(r.required ? 'addon_access.required' : 'addon_access.optional'),
    },
    reason: r.reason || '—',
    visibility: visibilityChip(r.ownerVisibility),
    grant: grantChip(r.grantStatus),
  };
}

function grantChip(status) {
  switch (status) {
    case 'granted':
    case 'auto_granted':
      return { status: 'ok', label: I18n.t('addon_access.grant.' + status) };
    case 'denied':
      return { status: 'err', label: I18n.t('addon_access.grant.denied') };
    case 'pending':
    default:
      return { status: 'warn', label: I18n.t('addon_access.grant.pending') };
  }
}

function visibilityChip(visibility) {
  const v = normalizeVisibility(visibility);
  const status = v === 'public' ? 'ok' : v === 'restricted' ? 'warn' : 'err';
  return { status, label: I18n.t('addon_access.visibility.' + v) };
}

function buildUseActions(row, kind) {
  if (row._grantStatus !== 'pending') return null;
  const wrap = document.createElement('div');
  wrap.style.display = 'flex';
  wrap.style.gap = '6px';
  wrap.style.justifyContent = 'flex-end';

  const approve = document.createElement('tf-button');
  approve.setAttribute('variant', 'primary');
  approve.setAttribute('size', 'sm');
  approve.setAttribute('icon', 'check');
  approve.textContent = I18n.t('addon_access.action_approve');
  approve.addEventListener('click', () => decide(kind, row._target, 'approve'));

  const deny = document.createElement('tf-button');
  deny.setAttribute('variant', 'danger');
  deny.setAttribute('size', 'sm');
  deny.setAttribute('icon', 'x');
  deny.textContent = I18n.t('addon_access.action_deny');
  deny.addEventListener('click', () => decide(kind, row._target, 'deny'));

  wrap.appendChild(approve);
  wrap.appendChild(deny);
  return wrap;
}

async function decide(kind, target, decision) {
  const collection = kind === 'alias' ? usesAlias : usesModel;
  const entry = collection.find((e) => e.target === target);
  if (!entry) return;
  const prev = entry.grantStatus;
  // Optimistic: flip to the expected terminal state, re-render, revert on error.
  entry.grantStatus = decision === 'approve' ? 'granted' : 'denied';
  render();
  try {
    const resp = await ApiBinary.action('addonAccessDecisionRequest', {
      addonId: currentAddonId,
      kind,
      target,
      decision,
    });
    applyTransitions(resp.transitions || []);
    render();
    toast(I18n.t('addon_access.decision_saved'), 'success');
  } catch (err) {
    entry.grantStatus = prev;
    render();
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

// Applies server-returned transitions ({addonId, before, after}) to the local
// model. The server is authoritative on the final state — reconcile against it
// rather than trusting the optimistic guess.
function applyTransitions(transitions) {
  for (const t of transitions) {
    const addonId = t.addonId ?? t.addon_id;
    if (addonId && addonId !== currentAddonId) continue;
    const after = t.after;
    if (!after) continue;
    const target = t.target ?? t.alias ?? t.model;
    if (!target) continue;
    const entry = usesAlias.find((e) => e.target === target)
      || usesModel.find((e) => e.target === target);
    if (entry) entry.grantStatus = after;
  }
}

function renderProvidesTable(host) {
  if (!host) return;
  if (provides.length === 0) {
    host.innerHTML = `<div class="addons-empty">${escapeHtml(I18n.t('addon_access.provides_empty'))}</div>`;
    return;
  }
  host.innerHTML = `
    <tf-table>
      <tf-column key="alias" label="${escapeAttr(I18n.t('addon_access.col_alias'))}"></tf-column>
      <tf-column key="visibility" label="${escapeAttr(I18n.t('addon_access.col_visibility'))}" renderer="chip"></tf-column>
    </tf-table>
  `;
  const table = host.querySelector('tf-table');
  table.rows = provides.map((p) => ({
    alias: p.alias,
    visibility: visibilityChip(p.visibility),
  }));
}

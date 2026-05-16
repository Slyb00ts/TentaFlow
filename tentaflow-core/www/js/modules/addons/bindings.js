// =============================================================================
// File: modules/addons/bindings.js
// Description: Bindings tab for addon detail. Shows the AI aliases owned by
//              this addon (readonly — full management lives in M16
//              Services -> Aliases) plus four storage usage cards (KV, SQL,
//              Vector, Recording). Vector is a F1a placeholder; backend
//              messages for owner-filtered alias list, addon storage stats
//              and recording stats are not yet present, so cards render as
//              empty-state placeholders rather than fabricating numbers.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { Router } from '/js/router.js';

let currentAddonId = null;
let currentContainer = null;
let aliases = [];

export const BindingsTab = {
  async mount(container, addonId) {
    currentAddonId = addonId;
    currentContainer = container;
    container.innerHTML = `<div class="addons-empty">${escapeHtml(I18n.t('common.loading'))}</div>`;
    await loadAliases();
    render();
  },

  unmount() {
    currentAddonId = null;
    currentContainer = null;
    aliases = [];
  },
};

// NOTE: prefix heuristic is a STAND-IN until ModelAliasEntry exposes
// owner_addon_id (see backend-todo in CHANGELOG). False positive risk:
// alias 'teams-spy' would appear under addon 'teams' even if owned by
// another addon. Admin should verify ownership via M16 Aliasy page.
// This view is READ-ONLY — no destructive action possible on misattributed alias.
async function loadAliases() {
  try {
    const list = await ApiBinary.list('modelAliasListRequest', { arrayKey: 'aliases' });
    const prefix = String(currentAddonId || '').toLowerCase();
    aliases = list.filter((a) => {
      const name = String(a.alias || '').toLowerCase();
      return prefix && (name === prefix || name.startsWith(prefix + '-') || name.startsWith(prefix + '_'));
    });
  } catch (err) {
    aliases = [];
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

function render() {
  if (!currentContainer) return;
  currentContainer.innerHTML = `
    <div class="addon-bindings">
      ${renderAliasesSection()}
      ${renderStorageSection()}
    </div>
  `;
  currentContainer.querySelector('#bindings-open-m16')?.addEventListener('click', () => {
    Router.navigate('services', { tab: 'aliases' });
  });
  currentContainer.querySelector('#bindings-refresh')?.addEventListener('click', async () => {
    await loadAliases();
    render();
  });
}

function renderAliasesSection() {
  const head = `
    <div class="section-card-head">
      <div class="title">
        <svg class="icon"><use href="#i-brain"/></svg>
        ${escapeHtml(I18n.t('addon_bindings.aliases_title'))}
        <span class="muted">· ${aliases.length}</span>
      </div>
      <div class="actions">
        <tf-button variant="ghost" icon="refresh" id="bindings-refresh">${escapeHtml(I18n.t('addon_bindings.refresh'))}</tf-button>
        <tf-button variant="secondary" icon="external-link" id="bindings-open-m16">${escapeHtml(I18n.t('addon_bindings.open_in_m16'))}</tf-button>
      </div>
    </div>
  `;

  const info = `
    <div class="bindings-info">
      <svg class="icon"><use href="#i-info"/></svg>
      <span>${escapeHtml(I18n.t('addon_bindings.readonly_note'))}</span>
    </div>
  `;

  if (aliases.length === 0) {
    return `
      <section class="section-card">
        ${head}
        ${info}
        <div class="addons-empty">${escapeHtml(I18n.t('addon_bindings.no_aliases'))}</div>
      </section>
    `;
  }

  const heuristicBanner = `
    <div class="bindings-info bindings-info-warn">
      <tf-chip status="warn">${escapeHtml(I18n.t('addon_bindings.heuristic_chip'))}</tf-chip>
      <span>${escapeHtml(I18n.t('addon_bindings.heuristic_note'))}</span>
    </div>
  `;

  const rows = aliases.map((a) => {
    const active = !!a.is_active;
    const statusLabel = active
      ? I18n.t('addon_bindings.status_active')
      : I18n.t('addon_bindings.status_inactive');
    const statusVariant = active ? 'ok' : 'warn';
    const target = String(a.target_model || '').trim();
    const fallback = String(a.fallback_targets || '').trim();
    const strategy = String(a.strategy || 'first_available');

    return `
      <tr>
        <td>
          <div class="alias-name">${escapeHtml(a.alias || '')}</div>
        </td>
        <td>
          <div class="alias-target">${target ? escapeHtml(target) : `<span class="muted">${escapeHtml(I18n.t('addon_bindings.no_target'))}</span>`}</div>
          ${fallback ? `<div class="alias-fallback">${escapeHtml(I18n.t('addon_bindings.fallback_chain'))}: ${escapeHtml(fallback)}</div>` : ''}
        </td>
        <td><tf-chip>${escapeHtml(strategy)}</tf-chip></td>
        <td><tf-chip status="${escapeAttr(statusVariant)}">${escapeHtml(statusLabel)}</tf-chip></td>
      </tr>
    `;
  }).join('');

  return `
    <section class="section-card">
      ${head}
      ${info}
      ${heuristicBanner}
      <!-- Raw <table class="tf-table"> is a project-wide class-only convention
           (see logs.js, profiling-sessions.js). The <tf-table> component
           expects array-driven .rows + <tf-column> children; this view stays
           on the convention to match its siblings. -->
      <table class="tf-table bindings-alias-table">
        <thead>
          <tr>
            <th>${escapeHtml(I18n.t('addon_bindings.col_alias'))}</th>
            <th>${escapeHtml(I18n.t('addon_bindings.col_target'))}</th>
            <th>${escapeHtml(I18n.t('addon_bindings.col_strategy'))}</th>
            <th>${escapeHtml(I18n.t('addon_bindings.col_status'))}</th>
          </tr>
        </thead>
        <tbody>${rows}</tbody>
      </table>
    </section>
  `;
}

function renderStorageSection() {
  // Backend does not yet expose AddonStorageStatsRequest / RecordingStatsRequest
  // on the admin dispatch path. Render the four-card layout with
  // empty-state placeholders so the page structure is final but no fake
  // numbers leak through. F1a accepted scope.
  const cards = [
    {
      icon: 'key',
      title: I18n.t('addon_bindings.storage_kv_title'),
      subtitle: I18n.t('addon_bindings.storage_kv_sub'),
      state: 'pending',
    },
    {
      icon: 'database',
      title: I18n.t('addon_bindings.storage_sql_title'),
      subtitle: I18n.t('addon_bindings.storage_sql_sub'),
      state: 'pending',
    },
    {
      icon: 'cpu',
      title: I18n.t('addon_bindings.storage_vector_title'),
      subtitle: I18n.t('addon_bindings.storage_vector_sub'),
      state: 'f2',
    },
    {
      icon: 'record',
      title: I18n.t('addon_bindings.storage_recording_title'),
      subtitle: I18n.t('addon_bindings.storage_recording_sub'),
      state: 'pending',
    },
  ];

  const cardsHtml = cards.map((c) => `
    <div class="usage-card ${c.state === 'f2' ? 'is-f2' : ''}">
      <div class="h">
        <svg class="icon"><use href="#i-${escapeAttr(c.icon)}"/></svg>
        <span>${escapeHtml(c.title)}</span>
      </div>
      <div class="v muted">—</div>
      <div class="sub">${escapeHtml(c.subtitle)}</div>
      <div class="bar-thin"><div class="fill" style="width:0%"></div></div>
      <div class="card-foot">
        ${c.state === 'f2'
          ? `<tf-chip status="info">${escapeHtml(I18n.t('addon_bindings.coming_f2'))}</tf-chip>`
          : `<tf-chip status="warn">${escapeHtml(I18n.t('addon_bindings.stats_unavailable'))}</tf-chip>`}
      </div>
    </div>
  `).join('');

  return `
    <section class="section-card">
      <div class="section-card-head">
        <div class="title">
          <svg class="icon"><use href="#i-database"/></svg>
          ${escapeHtml(I18n.t('addon_bindings.storage_title'))}
        </div>
      </div>
      <div class="usage-grid">${cardsHtml}</div>
    </section>
  `;
}

export default BindingsTab;

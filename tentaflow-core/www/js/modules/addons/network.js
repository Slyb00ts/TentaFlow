// =============================================================================
// File: modules/addons/network.js
// Description: Network tab for the addon detail view. Renders two sections:
//   (1) read-only manifest declarations with per-rule coverage status
//       (covered / missing / conflicting) so admins see what the addon needs,
//   (2) editable admin policy (allowed / blocked hosts + strict/permissive
//       mode). Backend: AddonNetworkRulesGet/Set.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';

let currentAddonId = null;
let state = { mode: 'strict', allowed: [], blocked: [], declared: [] };

export const NetworkTab = {
  async mount(container, addonId) {
    currentAddonId = addonId;
    state = { mode: 'strict', allowed: [], blocked: [], declared: [] };
    await loadAndRender(container);
  },

  unmount() {
    currentAddonId = null;
    state = { mode: 'strict', allowed: [], blocked: [], declared: [] };
  },
};

async function loadAndRender(container) {
  container.innerHTML = `<div class="addons-empty">${escapeHtml(I18n.t('common.loading'))}</div>`;
  try {
    const resp = await ApiBinary.one('addonNetworkRulesGetRequest', { addonId: currentAddonId });
    state.mode = String(resp.mode ?? 'strict').toLowerCase() === 'permissive' ? 'permissive' : 'strict';
    state.allowed = normalizeHosts(resp.allowedHosts ?? resp.allowed_hosts);
    state.blocked = normalizeHosts(resp.blockedHosts ?? resp.blocked_hosts);
    state.declared = normalizeDeclared(resp.declaredRules ?? resp.declared_rules);
    render(container);
  } catch (err) {
    container.innerHTML = `<div class="addons-empty" style="color:var(--danger);">${escapeHtml(err.message)}</div>`;
  }
}

function normalizeHosts(v) {
  if (!Array.isArray(v)) return [];
  return v.map((h) => String(h).trim()).filter((h) => h.length > 0);
}

function normalizeDeclared(v) {
  if (!Array.isArray(v)) return [];
  return v
    .map((r) => ({
      host: String(r?.host ?? '').trim(),
      port: Number.isFinite(Number(r?.port)) ? Number(r.port) : null,
      mode: String(r?.mode ?? 'allow').toLowerCase() === 'block' ? 'block' : 'allow',
      status: ['covered', 'missing', 'conflicting'].includes(String(r?.status))
        ? String(r.status)
        : 'missing',
    }))
    .filter((r) => r.host.length > 0);
}

function render(container) {
  container.innerHTML = `
    ${renderDeclaredSection()}
    ${renderAdminSection()}
  `;

  container.querySelectorAll('[data-quick-allow]').forEach((btn) => {
    btn.addEventListener('click', () => onQuickAdd(container, btn.getAttribute('data-quick-allow')));
  });

  container.querySelectorAll('tf-chip[clickable][data-mode]').forEach((chip) => {
    chip.addEventListener('click', () => {
      container.querySelectorAll('tf-chip[clickable][data-mode]').forEach((c) => c.removeAttribute('active'));
      chip.setAttribute('active', '');
      state.mode = chip.dataset.mode;
      const desc = container.querySelector('#mode-desc');
      if (desc) {
        desc.textContent = I18n.t(state.mode === 'strict'
          ? 'addon_network.mode_strict_desc'
          : 'addon_network.mode_permissive_desc');
      }
    });
  });

  attachListHandlers(container, 'allowed');
  attachListHandlers(container, 'blocked');

  container.querySelector('#net-save')?.addEventListener('click', () => onSave());
}

function renderDeclaredSection() {
  const title = escapeHtml(I18n.t('addon_network.declared_title'));
  const subtitle = escapeHtml(I18n.t('addon_network.declared_subtitle'));
  if (state.declared.length === 0) {
    return `
      <div class="section-card" style="margin-bottom:14px;">
        <h3>${title}</h3>
        <div class="section-sub">${subtitle}</div>
        <div class="addons-empty" style="padding:10px 0;">
          ${escapeHtml(I18n.t('addon_network.declared_empty'))}
        </div>
      </div>
    `;
  }
  const rows = state.declared.map((r) => renderDeclaredRow(r)).join('');
  return `
    <div class="section-card" style="margin-bottom:14px;">
      <h3>${title}</h3>
      <div class="section-sub">${subtitle}</div>
      <div class="network-declared-list">
        ${rows}
      </div>
    </div>
  `;
}

function renderDeclaredRow(rule) {
  const statusChip = renderStatusChip(rule.status);
  const modeChip = rule.mode === 'block'
    ? `<tf-chip status="warn">${escapeHtml(I18n.t('addon_network.col_mode'))}: block</tf-chip>`
    : `<tf-chip status="info">${escapeHtml(I18n.t('addon_network.col_mode'))}: allow</tf-chip>`;
  const port = rule.port != null
    ? `<span class="port">:${escapeHtml(String(rule.port))}</span>`
    : '';
  const quickAction = rule.status === 'missing'
    ? `<tf-button variant="ghost" size="small" icon="plus"
          class="quick-action" data-quick-allow="${escapeAttr(rule.host)}">
        ${escapeHtml(I18n.t('addon_network.quick_add_allowed'))}
      </tf-button>`
    : '';
  return `
    <div class="network-declared-row">
      <span class="host">${escapeHtml(rule.host)}</span>
      ${port}
      ${modeChip}
      ${statusChip}
      ${quickAction}
    </div>
  `;
}

function renderStatusChip(status) {
  if (status === 'covered') {
    return `<tf-chip status="ok">${escapeHtml(I18n.t('addon_network.status_covered'))}</tf-chip>`;
  }
  if (status === 'conflicting') {
    return `<tf-chip status="err">${escapeHtml(I18n.t('addon_network.status_conflicting'))}</tf-chip>`;
  }
  return `<tf-chip status="warn">${escapeHtml(I18n.t('addon_network.status_missing'))}</tf-chip>`;
}

function renderAdminSection() {
  return `
    <div class="section-card" style="margin-bottom:14px;">
      <h3>${escapeHtml(I18n.t('addon_network.admin_policy_title'))}</h3>
      <div class="section-sub">${escapeHtml(I18n.t('addon_network.admin_policy_subtitle'))}</div>
      <div style="display:flex;gap:10px;flex-wrap:wrap;margin-bottom:10px;">
        <tf-chip clickable data-mode="strict" ${state.mode === 'strict' ? 'active' : ''}>
          ${escapeHtml(I18n.t('addon_network.mode_strict'))}
        </tf-chip>
        <tf-chip clickable data-mode="permissive" ${state.mode === 'permissive' ? 'active' : ''}>
          ${escapeHtml(I18n.t('addon_network.mode_permissive'))}
        </tf-chip>
      </div>
      <div id="mode-desc" style="color:var(--text-3);font-size:12px;margin-bottom:14px;">
        ${escapeHtml(I18n.t(state.mode === 'strict' ? 'addon_network.mode_strict_desc' : 'addon_network.mode_permissive_desc'))}
      </div>
      <div style="display:grid;grid-template-columns:repeat(auto-fit,minmax(320px,1fr));gap:14px;">
        ${renderList('allowed', I18n.t('addon_network.allowed_hosts'), state.allowed, 'ok')}
        ${renderList('blocked', I18n.t('addon_network.blocked_hosts'), state.blocked, 'err')}
      </div>
      <div style="display:flex;justify-content:flex-end;margin-top:16px;">
        <tf-button variant="primary" id="net-save" icon="check">
          ${escapeHtml(I18n.t('addon_network.save'))}
        </tf-button>
      </div>
    </div>
  `;
}

function renderList(kind, title, hosts, chipStatus) {
  const chips = hosts.map((h, idx) => `
    <tf-chip status="${chipStatus}" data-host-chip data-host-idx="${idx}">
      ${escapeHtml(h)}
      <span data-host-remove data-host-idx="${idx}" style="margin-left:6px;cursor:pointer;font-weight:700;">×</span>
    </tf-chip>
  `).join('');

  return `
    <div class="card" style="padding:14px;">
      <div style="font-weight:600;color:var(--text);margin-bottom:10px;">${escapeHtml(title)}</div>
      <div data-host-list="${escapeAttr(kind)}" style="display:flex;flex-wrap:wrap;gap:6px;margin-bottom:10px;min-height:32px;">
        ${chips || `<div style="color:var(--text-3);font-size:12px;">—</div>`}
      </div>
      <div style="display:flex;gap:6px;">
        <tf-input
          data-host-input="${escapeAttr(kind)}"
          type="text"
          placeholder="${escapeAttr(I18n.t('addon_network.add_host_placeholder'))}"
          style="flex:1;"></tf-input>
        <tf-button variant="ghost" icon="plus" data-host-add="${escapeAttr(kind)}">
          ${escapeHtml(I18n.t('addon_network.add'))}
        </tf-button>
      </div>
    </div>
  `;
}

function attachListHandlers(container, kind) {
  const input = container.querySelector(`[data-host-input="${kind}"]`);
  const addBtn = container.querySelector(`[data-host-add="${kind}"]`);
  const list = container.querySelector(`[data-host-list="${kind}"]`);

  const addHost = () => {
    const val = String(input?.value ?? '').trim();
    if (!val) return;
    const target = kind === 'allowed' ? state.allowed : state.blocked;
    if (target.includes(val)) {
      toast(I18n.t('addon_network.already_exists'), 'warn');
      return;
    }
    target.push(val);
    if (input) input.value = '';
    render(container);
  };

  addBtn?.addEventListener('click', addHost);
  input?.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      addHost();
    }
  });

  list?.querySelectorAll('[data-host-remove]').forEach((x) => {
    x.addEventListener('click', (e) => {
      e.stopPropagation();
      const idx = Number(x.getAttribute('data-host-idx'));
      const target = kind === 'allowed' ? state.allowed : state.blocked;
      if (Number.isFinite(idx) && idx >= 0 && idx < target.length) {
        target.splice(idx, 1);
        render(container);
      }
    });
  });
}

async function onQuickAdd(container, host) {
  const value = String(host ?? '').trim();
  if (!value) return;
  if (state.allowed.includes(value)) {
    toast(I18n.t('addon_network.already_exists'), 'warn');
    return;
  }
  state.allowed.push(value);
  // Flip the declared-row status to "covered" locally so the hint updates
  // without rebuilding the whole section.
  const declaredRule = state.declared.find((r) => r.host === value);
  const prevStatus = declaredRule?.status;
  if (declaredRule) declaredRule.status = 'covered';
  try {
    await ApiBinary.action('addonNetworkRulesSetRequest', {
      addonId: currentAddonId,
      allowedHosts: state.allowed,
      blockedHosts: state.blocked,
      mode: state.mode,
    });
    render(container);
    toast(I18n.t('addon_network.quick_added_toast', { host: value }), 'success');
  } catch (err) {
    const idx = state.allowed.indexOf(value);
    if (idx >= 0) state.allowed.splice(idx, 1);
    if (declaredRule && prevStatus) declaredRule.status = prevStatus;
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

async function onSave() {
  try {
    await ApiBinary.action('addonNetworkRulesSetRequest', {
      addonId: currentAddonId,
      allowedHosts: state.allowed,
      blockedHosts: state.blocked,
      mode: state.mode,
    });
    toast(I18n.t('common.saved'), 'success');
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

// =============================================================================
// File: modules/addons/install-wizard.js
// Description: Generic install wizard launched from addon detail or registry.
//              Six steps total; this chunk implements steps 1-3 fully
//              (Permissions, Storage, Aliases). Steps 4-6 (Flow templates,
//              Legal profile, First camera) are placeholders pending F1a+
//              backend work. The wizard renders inside a tf-window modal.
//              Final "Install" issues addonInstallConfigureRequest (flagged
//              as missing — UI calls it but backend handler will need to be
//              added before this wizard is wired to the registry flow).
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';

// Wizard state (singleton — only one wizard at a time).
let state = null;

/**
 * Opens the install wizard.
 * @param {object} opts
 *   - addonId: string (required for reconfigure path; may be null for fresh install from manifest)
 *   - manifest: parsed manifest object with {permissions, storage, aliases, name, version, icon}
 *   - onDone: optional callback(addonId) on successful install
 */
export function openInstallWizard(opts = {}) {
  state = {
    addonId: opts.addonId || null,
    manifest: opts.manifest || {},
    onDone: opts.onDone || null,
    currentStep: 1,
    // Step 1 — permissions: pid -> {grant: 'allow'|'deny', reviewed: bool}
    permissions: new Map(),
    // Step 2 — storage:
    storage: {
      kvEnabled: true,
      kvQuotaBytes: 0,
      sqlEnabled: true,
      sqlBackend: 'sqlite',
      sqlEncryption: false,
    },
    // Step 3 — aliases: alias_name -> {enabled: bool, target: string}
    aliases: new Map(),
  };

  initFromManifest();

  const win = document.createElement('tf-window');
  win.setAttribute('title', I18n.t('install_wizard.title'));
  win.setAttribute('icon', 'puzzle');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '820');
  win.setAttribute('min-width', '640');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');

  const body = document.createElement('div');
  body.slot = 'body';
  body.id = 'install-wizard-body';
  win.appendChild(body);

  const footer = document.createElement('div');
  footer.slot = 'footer';
  footer.id = 'install-wizard-footer';
  win.appendChild(footer);

  win.addEventListener('action', (e) => {
    if (e.detail?.action !== 'close') return;
    if (hasUnsavedProgress()) {
      e.preventDefault();
      const ok = window.confirm(I18n.t('install_wizard.discard_confirm'));
      if (ok) win.close(true);
    }
  });

  document.body.appendChild(win);
  state.win = win;
  renderStep();
}

function initFromManifest() {
  const m = state.manifest || {};
  const perms = Array.isArray(m.permissions) ? m.permissions : [];
  for (const p of perms) {
    const pid = p.permission_id || p.permissionId;
    if (!pid) continue;
    const risk = (p.risk || 'low').toLowerCase();
    // Critical risk defaults to deny so admin must explicitly opt in.
    const defaultGrant = risk === 'critical' ? 'deny' : 'allow';
    state.permissions.set(pid, {
      displayName: p.display_name || p.displayName || pid,
      description: p.description || '',
      risk,
      grant: defaultGrant,
      reviewed: false,
    });
  }

  const storage = m.storage || {};
  state.storage.kvEnabled = storage.kv !== false;
  state.storage.sqlEnabled = !!storage.sql;
  state.storage.kvQuotaBytes = Number(storage.kv_quota_bytes || 0);
  if (Array.isArray(storage.sql_backends) && storage.sql_backends.length > 0) {
    state.storage.sqlBackend = String(storage.sql_backends[0]);
  }
  state.storage.sqlEncryption = !!storage.encryption;
  state.storage.migrationsDir = storage.migrations_dir || 'migrations/';

  const aliases = Array.isArray(m.aliases) ? m.aliases : [];
  for (const a of aliases) {
    const name = a.alias || a.name;
    if (!name) continue;
    state.aliases.set(name, {
      displayName: a.display_name || a.displayName || name,
      description: a.description || '',
      suggestedTarget: a.suggested_default || a.suggestedDefault || '',
      target: a.suggested_default || a.suggestedDefault || '',
      enabled: true,
      // status comes from backend conflict check; placeholder for F1a.
      conflictStatus: a.conflict_status || 'will-create',
      gated: !!a.gated,
    });
  }
}

function hasUnsavedProgress() {
  return state && state.currentStep > 1;
}

// --- Top-level rendering ---------------------------------------------------

function renderStep() {
  const bodyHost = state.win.querySelector('#install-wizard-body');
  const footHost = state.win.querySelector('#install-wizard-footer');
  if (!bodyHost || !footHost) return;
  bodyHost.innerHTML = `
    ${renderHeader()}
    ${renderProgress()}
    <div class="install-step-body">${renderStepBody()}</div>
  `;
  footHost.innerHTML = renderFooter();
  attachStepHandlers(bodyHost);
  attachFooterHandlers(footHost);
}

function renderHeader() {
  const m = state.manifest || {};
  const name = m.name || state.addonId || I18n.t('install_wizard.unnamed');
  const version = m.version || '';
  const description = m.description || '';
  return `
    <div class="install-header">
      <div class="big-ico"><svg><use href="#i-puzzle"/></svg></div>
      <div class="install-header-meta">
        <h1>${escapeHtml(name)}${version ? ` <span class="version">v${escapeHtml(version)}</span>` : ''}</h1>
        ${description ? `<div class="sub">${escapeHtml(description)}</div>` : ''}
      </div>
    </div>
  `;
}

function renderProgress() {
  const steps = [
    { n: 1, label: I18n.t('install_wizard.step1') },
    { n: 2, label: I18n.t('install_wizard.step2') },
    { n: 3, label: I18n.t('install_wizard.step3') },
    { n: 4, label: I18n.t('install_wizard.step4') },
    { n: 5, label: I18n.t('install_wizard.step5') },
    { n: 6, label: I18n.t('install_wizard.step6') },
  ];
  return `
    <div class="install-progress">
      ${steps.map((s) => {
        const cls = s.n < state.currentStep ? 'done' : s.n === state.currentStep ? 'active' : '';
        return `
          <div class="install-step ${cls}">
            <span class="num">${s.n}</span>
            <span class="label">${escapeHtml(s.label)}</span>
          </div>
        `;
      }).join('')}
    </div>
  `;
}

function renderStepBody() {
  switch (state.currentStep) {
    case 1: return renderPermissionsStep();
    case 2: return renderStorageStep();
    case 3: return renderAliasesStep();
    case 4:
    case 5:
    case 6:
      return renderPlaceholderStep(state.currentStep);
    default:
      return '';
  }
}

function renderFooter() {
  const canBack = state.currentStep > 1;
  const isLast = state.currentStep === 6;
  const ok = canAdvance();
  const nextLabel = isLast
    ? I18n.t('install_wizard.install')
    : I18n.t('install_wizard.next');
  return `
    <tf-button variant="ghost" data-wizard-back ${canBack ? '' : 'disabled'}>${escapeHtml(I18n.t('install_wizard.back'))}</tf-button>
    <div class="spacer" style="flex:1"></div>
    <tf-button variant="primary" icon="${isLast ? 'check' : 'chevron-right'}" data-wizard-next ${ok ? '' : 'disabled'}>${escapeHtml(nextLabel)}</tf-button>
  `;
}

// --- Step 1: Permissions ---------------------------------------------------

function renderPermissionsStep() {
  const items = Array.from(state.permissions.entries());
  if (items.length === 0) {
    return `<div class="addons-empty">${escapeHtml(I18n.t('install_wizard.permissions_none'))}</div>`;
  }
  const rows = items.map(([pid, p]) => {
    const riskStatus = ({ low: 'info', medium: 'warn', high: 'err', critical: 'err' })[p.risk] || 'info';
    const grantChecked = p.grant === 'allow' ? 'checked' : '';
    return `
      <div class="wizard-perm-row ${p.risk === 'critical' ? 'is-critical' : ''}" data-pid="${escapeAttr(pid)}">
        <div class="wizard-perm-main">
          <div class="wizard-perm-name">
            <span class="mono">${escapeHtml(pid)}</span>
            <tf-chip status="${escapeAttr(riskStatus)}">${escapeHtml(I18n.t('install_wizard.risk_' + p.risk))}</tf-chip>
          </div>
          <div class="wizard-perm-display">${escapeHtml(p.displayName)}</div>
          ${p.description ? `<div class="wizard-perm-desc">${escapeHtml(p.description)}</div>` : ''}
        </div>
        <div class="wizard-perm-controls">
          <label class="wizard-toggle">
            <tf-toggle data-role="grant" ${grantChecked}></tf-toggle>
            <span>${escapeHtml(I18n.t('install_wizard.grant'))}</span>
          </label>
          <label class="wizard-toggle">
            <tf-toggle data-role="reviewed" ${p.reviewed ? 'checked' : ''}></tf-toggle>
            <span>${escapeHtml(I18n.t('install_wizard.reviewed'))}</span>
          </label>
        </div>
      </div>
    `;
  }).join('');

  return `
    <h2 class="wizard-section-title">${escapeHtml(I18n.t('install_wizard.step1_title'))}</h2>
    <p class="wizard-section-sub">${escapeHtml(I18n.t('install_wizard.step1_sub'))}</p>
    <div class="wizard-perm-list">${rows}</div>
  `;
}

// --- Step 2: Storage -------------------------------------------------------

function renderStorageStep() {
  const s = state.storage;
  const manifestStorage = state.manifest?.storage || {};
  const sqlBackends = Array.isArray(manifestStorage.sql_backends) && manifestStorage.sql_backends.length > 0
    ? manifestStorage.sql_backends
    : ['sqlite'];

  return `
    <h2 class="wizard-section-title">${escapeHtml(I18n.t('install_wizard.step2_title'))}</h2>
    <p class="wizard-section-sub">${escapeHtml(I18n.t('install_wizard.step2_sub'))}</p>

    <div class="wizard-storage-row">
      <div class="wizard-storage-head">
        <svg class="icon"><use href="#i-hash"/></svg>
        <div class="wizard-storage-title">${escapeHtml(I18n.t('install_wizard.storage_kv'))}</div>
        <tf-toggle data-role="kv-enabled" ${s.kvEnabled ? 'checked' : ''}></tf-toggle>
      </div>
      <div class="wizard-storage-body">
        <div class="muted">${escapeHtml(I18n.t('install_wizard.storage_kv_sub'))}</div>
      </div>
    </div>

    <div class="wizard-storage-row">
      <div class="wizard-storage-head">
        <svg class="icon"><use href="#i-database"/></svg>
        <div class="wizard-storage-title">${escapeHtml(I18n.t('install_wizard.storage_sql'))}</div>
        <tf-toggle data-role="sql-enabled" ${s.sqlEnabled ? 'checked' : ''}></tf-toggle>
      </div>
      <div class="wizard-storage-body">
        <div class="muted">${escapeHtml(I18n.t('install_wizard.storage_sql_sub'))}</div>
        <div class="wizard-storage-field">
          <label>${escapeHtml(I18n.t('install_wizard.storage_sql_backend'))}</label>
          <tf-select data-role="sql-backend" value="${escapeAttr(s.sqlBackend)}">
            ${sqlBackends.map((b) => `<option value="${escapeAttr(b)}">${escapeHtml(b)}</option>`).join('')}
          </tf-select>
        </div>
        <div class="wizard-storage-field">
          <label>${escapeHtml(I18n.t('install_wizard.storage_migrations'))}</label>
          <span class="mono">${escapeHtml(state.storage.migrationsDir || 'migrations/')}</span>
        </div>
      </div>
    </div>

    ${manifestStorage.sql && !s.sqlEnabled ? `
      <div class="wizard-warning">
        <svg class="icon"><use href="#i-alert"/></svg>
        ${escapeHtml(I18n.t('install_wizard.storage_sql_required'))}
      </div>
    ` : ''}
  `;
}

// --- Step 3: Aliases -------------------------------------------------------

function renderAliasesStep() {
  const items = Array.from(state.aliases.entries());
  if (items.length === 0) {
    return `
      <h2 class="wizard-section-title">${escapeHtml(I18n.t('install_wizard.step3_title'))}</h2>
      <p class="wizard-section-sub">${escapeHtml(I18n.t('install_wizard.step3_sub'))}</p>
      <div class="addons-empty">${escapeHtml(I18n.t('install_wizard.aliases_none'))}</div>
    `;
  }
  const rows = items.map(([name, a]) => {
    const statusChip = ({
      'will-create': { status: 'ok', label: I18n.t('install_wizard.alias_will_create') },
      'exists-conflict': { status: 'err', label: I18n.t('install_wizard.alias_conflict') },
      'exists-compatible': { status: 'info', label: I18n.t('install_wizard.alias_compatible') },
    })[a.conflictStatus] || { status: 'info', label: a.conflictStatus };
    const blocked = a.conflictStatus === 'exists-conflict';
    return `
      <div class="wizard-alias-row ${blocked ? 'is-blocked' : ''}" data-alias="${escapeAttr(name)}">
        <div class="wizard-alias-main">
          <div class="wizard-alias-name mono">${escapeHtml(name)}</div>
          ${a.description ? `<div class="wizard-alias-desc">${escapeHtml(a.description)}</div>` : ''}
          ${a.gated ? `<div class="wizard-alias-gated"><tf-chip status="warn" icon="lock">${escapeHtml(I18n.t('install_wizard.alias_gated'))}</tf-chip></div>` : ''}
        </div>
        <div class="wizard-alias-target">
          <label>${escapeHtml(I18n.t('install_wizard.alias_target'))}</label>
          <tf-input data-role="target" value="${escapeAttr(a.target || '')}" placeholder="${escapeAttr(a.suggestedTarget || '')}"></tf-input>
          ${a.suggestedTarget ? `<div class="muted">${escapeHtml(I18n.t('install_wizard.alias_suggested'))}: <span class="mono">${escapeHtml(a.suggestedTarget)}</span></div>` : ''}
        </div>
        <div class="wizard-alias-side">
          <tf-chip status="${escapeAttr(statusChip.status)}">${escapeHtml(statusChip.label)}</tf-chip>
          <tf-toggle data-role="enabled" ${a.enabled && !blocked ? 'checked' : ''} ${blocked ? 'disabled' : ''}></tf-toggle>
        </div>
      </div>
    `;
  }).join('');
  return `
    <h2 class="wizard-section-title">${escapeHtml(I18n.t('install_wizard.step3_title'))}</h2>
    <p class="wizard-section-sub">${escapeHtml(I18n.t('install_wizard.step3_sub'))}</p>
    <div class="wizard-alias-list">${rows}</div>
  `;
}

// --- Steps 4-6 placeholders ------------------------------------------------

function renderPlaceholderStep(n) {
  const titleKey = `install_wizard.step${n}_title`;
  const subKey = `install_wizard.step${n}_sub`;
  return `
    <h2 class="wizard-section-title">${escapeHtml(I18n.t(titleKey))}</h2>
    <p class="wizard-section-sub">${escapeHtml(I18n.t(subKey))}</p>
    <div class="wizard-placeholder">
      <svg class="icon"><use href="#i-info"/></svg>
      <div>
        <strong>${escapeHtml(I18n.t('install_wizard.coming_f2'))}</strong>
        <div class="muted">${escapeHtml(I18n.t('install_wizard.placeholder_note'))}</div>
      </div>
    </div>
  `;
}

// --- Handlers --------------------------------------------------------------

function attachStepHandlers(root) {
  if (state.currentStep === 1) {
    root.querySelectorAll('.wizard-perm-row').forEach((row) => {
      const pid = row.dataset.pid;
      const p = state.permissions.get(pid);
      if (!p) return;
      const grantToggle = row.querySelector('tf-toggle[data-role="grant"]');
      const reviewedToggle = row.querySelector('tf-toggle[data-role="reviewed"]');
      grantToggle?.addEventListener('change', (e) => {
        const next = !!(e.detail?.checked ?? grantToggle.hasAttribute('checked'));
        if (next && p.risk === 'critical' && !p.criticalConfirmed) {
          const ok = window.confirm(I18n.t('install_wizard.critical_confirm').replace('{pid}', pid));
          if (!ok) {
            grantToggle.removeAttribute('checked');
            return;
          }
          p.criticalConfirmed = true;
        }
        p.grant = next ? 'allow' : 'deny';
        updateFooter();
      });
      reviewedToggle?.addEventListener('change', (e) => {
        p.reviewed = !!(e.detail?.checked ?? reviewedToggle.hasAttribute('checked'));
        updateFooter();
      });
    });
  } else if (state.currentStep === 2) {
    root.querySelector('tf-toggle[data-role="kv-enabled"]')?.addEventListener('change', (e) => {
      state.storage.kvEnabled = !!(e.detail?.checked ?? e.target.hasAttribute('checked'));
      renderStep();
    });
    root.querySelector('tf-toggle[data-role="sql-enabled"]')?.addEventListener('change', (e) => {
      state.storage.sqlEnabled = !!(e.detail?.checked ?? e.target.hasAttribute('checked'));
      renderStep();
    });
    root.querySelector('tf-select[data-role="sql-backend"]')?.addEventListener('change', (e) => {
      state.storage.sqlBackend = e.detail?.value || e.target.value || 'sqlite';
    });
  } else if (state.currentStep === 3) {
    root.querySelectorAll('.wizard-alias-row').forEach((row) => {
      const name = row.dataset.alias;
      const a = state.aliases.get(name);
      if (!a) return;
      row.querySelector('tf-input[data-role="target"]')?.addEventListener('input', (e) => {
        a.target = e.detail?.value ?? e.target.value ?? '';
      });
      row.querySelector('tf-toggle[data-role="enabled"]')?.addEventListener('change', (e) => {
        a.enabled = !!(e.detail?.checked ?? e.target.hasAttribute('checked'));
        updateFooter();
      });
    });
  }
}

function attachFooterHandlers(root) {
  root.querySelector('[data-wizard-back]')?.addEventListener('click', () => {
    if (state.currentStep > 1) {
      state.currentStep -= 1;
      renderStep();
    }
  });
  root.querySelector('[data-wizard-next]')?.addEventListener('click', async () => {
    if (!canAdvance()) return;
    if (state.currentStep === 6) {
      await finalizeInstall();
      return;
    }
    state.currentStep += 1;
    renderStep();
  });
}

function updateFooter() {
  const footHost = state.win?.querySelector('#install-wizard-footer');
  if (footHost) {
    footHost.innerHTML = renderFooter();
    attachFooterHandlers(footHost);
  }
}

function canAdvance() {
  if (state.currentStep === 1) {
    // All permissions must be reviewed; critical-risk allow requires explicit confirm (already enforced).
    for (const p of state.permissions.values()) {
      if (!p.reviewed) return false;
    }
    return true;
  }
  if (state.currentStep === 2) {
    const manifestStorage = state.manifest?.storage || {};
    if (manifestStorage.sql && !state.storage.sqlEnabled) return false;
    return true;
  }
  if (state.currentStep === 3) {
    for (const a of state.aliases.values()) {
      if (a.conflictStatus === 'exists-conflict' && a.enabled) return false;
    }
    return true;
  }
  return true;
}

async function finalizeInstall() {
  const ok = window.confirm(I18n.t('install_wizard.final_confirm'));
  if (!ok) return;
  const payload = {
    addonId: state.addonId || '',
    permissions: Array.from(state.permissions.entries()).map(([pid, p]) => ({
      permissionId: pid,
      grantMode: p.grant,
    })),
    storage: { ...state.storage },
    aliases: Array.from(state.aliases.entries())
      .filter(([, a]) => a.enabled)
      .map(([name, a]) => ({
        alias: name,
        targetModel: a.target || '',
      })),
  };

  try {
    // Backend handler `addonInstallConfigureRequest` is not implemented yet.
    // The call will surface a missing-variant error; this is intentional
    // until the backend lands so wizard does not silently succeed on a stub.
    const result = await ApiBinary.action('addonInstallConfigureRequest', payload);
    if (!result?.ok) {
      throw new Error(result?.error || 'install_configure_failed');
    }
    toast(I18n.t('install_wizard.install_success'), 'success');
    state.win.close(true);
    if (typeof state.onDone === 'function') {
      try { state.onDone(state.addonId); } catch (_) { /* ignore */ }
    }
  } catch (err) {
    toast(`${I18n.t('install_wizard.install_error')}: ${err.message}`, 'error');
  }
}

export default { openInstallWizard };

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
import { TfWindow } from '/js/components/tf-window.js';

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
      sqlQuotaBytes: 0,
      sqlBackend: 'sqlite',
      sqlEncryption: false,
    },
    // Step 3 — aliases: alias_name -> {enabled: bool, target: string}
    aliases: new Map(),
    // Step 4 — discovered ONVIF cameras
    cameras: {
      discovered: [],
      added: new Map(),
      // Set of cameraRowKey strings currently mid-flight in
      // cameraAddOnvifRequest. Guards against rapid double-click that would
      // open two dialogs and submit two AddOnvif calls for the same camera.
      pending: new Set(),
      loading: false,
      error: null,
    },
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

  // Intercept every close path (X button, ESC, future outside-click) via the
  // cancelable close-request event so the dirty check fires uniformly.
  win.addEventListener('close-request', (e) => {
    if (!hasUnsavedProgress()) return;
    e.preventDefault();
    TfWindow.confirm({
      title: I18n.t('install_wizard.discard_title'),
      message: I18n.t('install_wizard.discard_confirm'),
      confirmLabel: I18n.t('install_wizard.discard_yes'),
      cancelLabel: I18n.t('common.cancel'),
      danger: true,
      icon: 'alert',
    }).then((ok) => {
      if (ok) win.close(true);
    });
  });

  document.body.appendChild(win);
  state.win = win;
  state.existingAliases = [];
  renderStep();
  // Background-load the existing alias list so Step 3 can compute conflicts
  // against actual server state instead of trusting the manifest hint.
  loadExistingAliases();
}

async function loadExistingAliases() {
  try {
    const list = await ApiBinary.list('modelAliasListRequest', { arrayKey: 'aliases' });
    state.existingAliases = Array.isArray(list) ? list : [];
    recomputeAliasStatuses();
    if (state.currentStep === 3) renderStep();
  } catch (_) {
    // Leave manifest-provided statuses untouched on failure.
    state.existingAliases = [];
  }
}

function recomputeAliasStatuses() {
  if (!state.aliases) return;
  const existing = state.existingAliases || [];
  const addonId = String(state.addonId || '').toLowerCase();
  for (const [name, a] of state.aliases.entries()) {
    const hit = existing.find((e) => String(e.alias || '').toLowerCase() === String(name).toLowerCase());
    if (!hit) {
      a.conflictStatus = 'will-create';
      a.conflictOwner = null;
      continue;
    }
    // Defensive read — backend may not yet expose owner fields; treat unknown
    // ownership as a hard conflict so admin must resolve in M16.
    const ownerType = hit.owner_type || hit.ownerType || null;
    const ownerId = String(hit.owner_id || hit.ownerId || '').toLowerCase();
    if (ownerType === 'manual') {
      a.conflictStatus = 'exists-compatible';
      a.conflictOwner = 'manual';
    } else if (ownerType === 'addon' && ownerId === addonId) {
      a.conflictStatus = 'exists-compatible';
      a.conflictOwner = ownerId;
    } else {
      a.conflictStatus = 'exists-conflict';
      a.conflictOwner = ownerId || ownerType || 'unknown';
    }
  }
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
  state.storage.sqlQuotaBytes = Number(storage.sql_quota_bytes || 0);
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
    case 4: return renderCamerasStep();
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

// F1a supports only sqlite for the per-addon SQL store. Postgres is on the
// F8 roadmap; if a manifest declares it we surface a banner explaining the
// downgrade instead of silently dropping the option.
const SQL_BACKENDS_SUPPORTED_F1A = ['sqlite'];

// F1a default quotas (bytes). Used when manifest does not declare a quota.
const KV_QUOTA_DEFAULT = 100 * 1024 * 1024;   // 100 MiB
const SQL_QUOTA_DEFAULT = 500 * 1024 * 1024;  // 500 MiB
const KV_QUOTA_MIN = 1 * 1024 * 1024;
const KV_QUOTA_MAX = 1024 * 1024 * 1024;
const SQL_QUOTA_MIN = 16 * 1024 * 1024;
const SQL_QUOTA_MAX = 4 * 1024 * 1024 * 1024;

function formatBytes(n) {
  const v = Number(n) || 0;
  if (v >= 1024 * 1024 * 1024) return `${(v / (1024 * 1024 * 1024)).toFixed(1)} GiB`;
  if (v >= 1024 * 1024) return `${Math.round(v / (1024 * 1024))} MiB`;
  if (v >= 1024) return `${Math.round(v / 1024)} KiB`;
  return `${v} B`;
}

function renderStorageStep() {
  const s = state.storage;
  const manifestStorage = state.manifest?.storage || {};
  const manifestBackends = Array.isArray(manifestStorage.sql_backends) && manifestStorage.sql_backends.length > 0
    ? manifestStorage.sql_backends.map(String)
    : ['sqlite'];
  const sqlBackends = manifestBackends.filter((b) => SQL_BACKENDS_SUPPORTED_F1A.includes(b));
  const unsupportedBackends = manifestBackends.filter((b) => !SQL_BACKENDS_SUPPORTED_F1A.includes(b));
  if (sqlBackends.length === 0) sqlBackends.push('sqlite');
  if (!sqlBackends.includes(s.sqlBackend)) {
    s.sqlBackend = sqlBackends[0];
  }

  return `
    <h2 class="wizard-section-title">${escapeHtml(I18n.t('install_wizard.step2_title'))}</h2>
    <p class="wizard-section-sub">${escapeHtml(I18n.t('install_wizard.step2_sub'))}</p>

    ${unsupportedBackends.length > 0 ? `
      <div class="wizard-warning">
        <svg class="icon"><use href="#i-alert"/></svg>
        ${escapeHtml(I18n.t('install_wizard.storage_backend_downgrade').replace('{list}', unsupportedBackends.join(', ')))}
      </div>
    ` : ''}

    <div class="wizard-storage-row">
      <div class="wizard-storage-head">
        <svg class="icon"><use href="#i-hash"/></svg>
        <div class="wizard-storage-title">${escapeHtml(I18n.t('install_wizard.storage_kv'))}</div>
        <tf-toggle data-role="kv-enabled" ${s.kvEnabled ? 'checked' : ''}></tf-toggle>
      </div>
      <div class="wizard-storage-body">
        <div class="muted">${escapeHtml(I18n.t('install_wizard.storage_kv_sub'))}</div>
        <div class="wizard-storage-field">
          <label>${escapeHtml(I18n.t('install_wizard.storage_quota'))}</label>
          <!-- tf-slider does not exist yet; using native <input type="range"> is
               the documented escape hatch (CLAUDE.md rule 8 — "tf-slider extension
               planned"). Replace once the component lands. -->
          <input type="range"
                 data-role="kv-quota"
                 min="${KV_QUOTA_MIN}"
                 max="${KV_QUOTA_MAX}"
                 step="${1024 * 1024}"
                 value="${s.kvQuotaBytes || KV_QUOTA_DEFAULT}"
                 ${s.kvEnabled ? '' : 'disabled'}
                 class="wizard-quota-slider">
          <span class="mono" data-role="kv-quota-display">${escapeHtml(formatBytes(s.kvQuotaBytes || KV_QUOTA_DEFAULT))}</span>
        </div>
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
          <label>${escapeHtml(I18n.t('install_wizard.storage_quota'))}</label>
          <input type="range"
                 data-role="sql-quota"
                 min="${SQL_QUOTA_MIN}"
                 max="${SQL_QUOTA_MAX}"
                 step="${16 * 1024 * 1024}"
                 value="${s.sqlQuotaBytes || SQL_QUOTA_DEFAULT}"
                 ${s.sqlEnabled ? '' : 'disabled'}
                 class="wizard-quota-slider">
          <span class="mono" data-role="sql-quota-display">${escapeHtml(formatBytes(s.sqlQuotaBytes || SQL_QUOTA_DEFAULT))}</span>
        </div>
        <div class="wizard-storage-field">
          <label>${escapeHtml(I18n.t('install_wizard.storage_migrations'))}</label>
          <span class="iw-mono">${escapeHtml(state.storage.migrationsDir || 'migrations/')}</span>
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
    const conflictDetail = blocked && a.conflictOwner
      ? `<div class="wizard-alias-desc">${escapeHtml(I18n.t('install_wizard.alias_conflict_owner').replace('{owner}', a.conflictOwner))}</div>`
      : '';
    return `
      <div class="wizard-alias-row ${blocked ? 'is-blocked' : ''}" data-alias="${escapeAttr(name)}">
        <div class="wizard-alias-main">
          <div class="wizard-alias-name mono">${escapeHtml(name)}</div>
          ${a.description ? `<div class="wizard-alias-desc">${escapeHtml(a.description)}</div>` : ''}
          ${conflictDetail}
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

// --- Step 4: Discovered cameras (ONVIF) ------------------------------------

function renderCamerasStep() {
  const c = state.cameras;
  const tableId = 'wizard-cameras-table';
  const headerLine = `
    <div class="wizard-cameras-header">
      <div>
        <h2 class="wizard-section-title">${escapeHtml(I18n.t('install_wizard.step4_title'))}</h2>
        <p class="wizard-section-sub">${escapeHtml(I18n.t('install_wizard.step4_sub'))}</p>
      </div>
      <div class="wizard-cameras-actions">
        <tf-button variant="ghost" icon="refresh" data-role="cameras-refresh" ${c.loading ? 'disabled' : ''}>${escapeHtml(I18n.t('install_wizard.cameras_refresh'))}</tf-button>
        <tf-button variant="ghost" icon="chevron-right" data-role="cameras-skip">${escapeHtml(I18n.t('install_wizard.cameras_skip'))}</tf-button>
      </div>
    </div>
  `;

  if (c.loading) {
    return `
      ${headerLine}
      <div class="wizard-cameras-state">
        <svg class="icon spin"><use href="#i-refresh"/></svg>
        <span>${escapeHtml(I18n.t('install_wizard.cameras_loading'))}</span>
      </div>
    `;
  }

  if (c.error) {
    return `
      ${headerLine}
      <div class="wizard-warning">
        <svg class="icon"><use href="#i-alert"/></svg>
        ${escapeHtml(c.error)}
      </div>
    `;
  }

  if (!Array.isArray(c.discovered) || c.discovered.length === 0) {
    return `
      ${headerLine}
      <div class="addons-empty">${escapeHtml(I18n.t('install_wizard.cameras_empty'))}</div>
    `;
  }

  return `
    ${headerLine}
    <div class="section-card wizard-cameras-table-wrap">
      <tf-table id="${tableId}">
        <tf-column key="vendor" label="${escapeAttr(I18n.t('install_wizard.cameras_col_vendor'))}"></tf-column>
        <tf-column key="model" label="${escapeAttr(I18n.t('install_wizard.cameras_col_model'))}"></tf-column>
        <tf-column key="ip" label="${escapeAttr(I18n.t('install_wizard.cameras_col_ip'))}"></tf-column>
        <tf-column key="statusChip" label="${escapeAttr(I18n.t('install_wizard.cameras_col_status'))}" renderer="chip"></tf-column>
        <tf-column key="actions" label="${escapeAttr(I18n.t('install_wizard.cameras_col_actions'))}" renderer="html"></tf-column>
      </tf-table>
    </div>
  `;
}

function cameraRowKey(cam) {
  const xa = Array.isArray(cam?.xaddrs) ? cam.xaddrs[0] : null;
  return xa || `${cam?.vendor || ''}|${cam?.model || ''}|${cam?.ip || ''}`;
}

function buildCameraRows() {
  const c = state.cameras;
  return c.discovered.map((cam, idx) => {
    const key = cameraRowKey(cam);
    const isAdded = c.added.has(key);
    const isPending = c.pending.has(key);
    const statusChip = isAdded
      ? { status: 'ok', label: I18n.t('install_wizard.cameras_status_added') }
      : { status: 'info', label: I18n.t('install_wizard.cameras_status_not_added') };
    const actionLabel = isAdded
      ? I18n.t('install_wizard.cameras_action_added')
      : I18n.t('install_wizard.cameras_action_add');
    // Disable the action button while an AddOnvif call is in flight for this
    // row so a second click cannot open a second dialog / submit a duplicate.
    const actionsHtml = isAdded
      ? `<tf-chip status="ok" icon="check">${escapeHtml(actionLabel)}</tf-chip>`
      : `<tf-button size="sm" variant="primary" data-role="cam-add" data-idx="${idx}" ${isPending ? 'disabled' : ''} aria-label="${escapeAttr(I18n.t('install_wizard.cameras_action_add'))}">${escapeHtml(actionLabel)}</tf-button>`;
    return {
      vendor: cam.vendor || '—',
      model: cam.model || '—',
      ip: cam.ip || '—',
      statusChip,
      actions: actionsHtml,
    };
  });
}

async function loadDiscoveredCameras() {
  state.cameras.loading = true;
  state.cameras.error = null;
  renderStep();
  try {
    const resp = await ApiBinary.one('cameraDiscoverRequest');
    const list = Array.isArray(resp?.discovered) ? resp.discovered : [];
    state.cameras.discovered = list;
  } catch (err) {
    state.cameras.discovered = [];
    state.cameras.error = `${I18n.t('install_wizard.cameras_err_discover')}: ${err?.message || err}`;
  } finally {
    state.cameras.loading = false;
    renderStep();
  }
}

function mapCameraErrorCode(code) {
  const m = {
    auth_failed: 'install_wizard.cameras_err_auth_failed',
    no_profiles: 'install_wizard.cameras_err_no_profiles',
    profile_not_found: 'install_wizard.cameras_err_profile_not_found',
    timeout: 'install_wizard.cameras_err_timeout',
    transport: 'install_wizard.cameras_err_transport',
    url_userinfo_not_allowed: 'install_wizard.cameras_err_url_userinfo_not_allowed',
  };
  const key = m[code] || 'install_wizard.cameras_err_generic';
  return I18n.t(key);
}

function openAddCameraDialog(idx) {
  const cam = state.cameras.discovered[idx];
  if (!cam) return;
  const key = cameraRowKey(cam);
  // Guard against rapid double-click: if a dialog/submit is already in flight
  // for this row, refuse the second open. The button is also disabled while
  // pending, but click events can still queue from keyboard activation.
  if (state.cameras.pending.has(key) || state.cameras.added.has(key)) return;
  state.cameras.pending.add(key);
  renderStep();
  const defaultName = `${cam.vendor || ''} ${cam.model || ''}`.trim() || (cam.ip || 'camera');
  const xa = Array.isArray(cam.xaddrs) ? cam.xaddrs[0] : '';
  const dlg = document.createElement('tf-window');
  dlg.setAttribute('title', I18n.t('install_wizard.cameras_add_title'));
  dlg.setAttribute('icon', 'video');
  dlg.setAttribute('buttons', 'close');
  dlg.setAttribute('width', '520');
  dlg.setAttribute('min-width', '420');
  dlg.setAttribute('initial-x', 'center');
  dlg.setAttribute('initial-y', 'center');
  dlg.setAttribute('role', 'dialog');
  dlg.setAttribute('aria-modal', 'true');

  const body = document.createElement('div');
  body.slot = 'body';
  body.className = 'wizard-cameras-dialog';
  body.innerHTML = `
    <div class="wizard-cameras-dialog-info">
      <div><span class="muted">${escapeHtml(I18n.t('install_wizard.cameras_col_vendor'))}:</span> ${escapeHtml(cam.vendor || '—')}</div>
      <div><span class="muted">${escapeHtml(I18n.t('install_wizard.cameras_col_model'))}:</span> ${escapeHtml(cam.model || '—')}</div>
      <div><span class="muted">${escapeHtml(I18n.t('install_wizard.cameras_col_ip'))}:</span> ${escapeHtml(cam.ip || '—')}</div>
      <div class="muted small mono">${escapeHtml(xa || '')}</div>
    </div>
    <div class="wizard-cameras-dialog-fields">
      <label>
        <span>${escapeHtml(I18n.t('install_wizard.cameras_field_display_name'))} *</span>
        <tf-input data-field="display_name" value="${escapeAttr(defaultName)}" required></tf-input>
      </label>
      <label>
        <span>${escapeHtml(I18n.t('install_wizard.cameras_field_username'))} *</span>
        <tf-input data-field="username" value="" required autocomplete="off"></tf-input>
      </label>
      <label>
        <span>${escapeHtml(I18n.t('install_wizard.cameras_field_password'))} *</span>
        <tf-input data-field="password" type="password" value="" required autocomplete="new-password"></tf-input>
      </label>
      <label>
        <span>${escapeHtml(I18n.t('install_wizard.cameras_field_profile_token'))}</span>
        <tf-input data-field="profile_token" value="" placeholder="${escapeAttr(I18n.t('install_wizard.cameras_field_profile_token_ph'))}"></tf-input>
      </label>
      <label>
        <span>${escapeHtml(I18n.t('install_wizard.cameras_field_target_fps'))}</span>
        <tf-input data-field="target_fps" type="number" value="15" min="1" max="120"></tf-input>
      </label>
    </div>
    <div class="wizard-cameras-dialog-error" data-role="dialog-error" hidden></div>
  `;
  dlg.appendChild(body);

  const footer = document.createElement('div');
  footer.slot = 'footer';
  footer.style.cssText = 'display:flex;gap:8px;width:100%;';
  footer.innerHTML = `
    <div style="flex:1"></div>
    <tf-button variant="ghost" data-role="dialog-cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
    <tf-button variant="primary" icon="check" data-role="dialog-save">${escapeHtml(I18n.t('install_wizard.cameras_dialog_save'))}</tf-button>
  `;
  dlg.appendChild(footer);

  document.body.appendChild(dlg);

  const readField = (name) => {
    const el = body.querySelector(`tf-input[data-field="${name}"]`);
    if (!el) return '';
    return (el.value ?? el.getAttribute('value') ?? '').toString().trim();
  };

  const showError = (msg) => {
    const box = body.querySelector('[data-role="dialog-error"]');
    if (!box) return;
    box.hidden = !msg;
    box.textContent = msg || '';
  };

  // Clear the pending guard on any close path (Cancel, Esc, X, post-Save
   // re-render). tf-window emits `close` once when the dialog disappears.
  const clearPending = () => {
    state.cameras.pending.delete(key);
  };
  dlg.addEventListener('close', clearPending);

  footer.querySelector('[data-role="dialog-cancel"]')?.addEventListener('click', () => {
    dlg.close(true);
  });

  // Esc handling delegated to tf-window default behavior.

  footer.querySelector('[data-role="dialog-save"]')?.addEventListener('click', async () => {
    const displayName = readField('display_name');
    const username = readField('username');
    const password = readField('password');
    const profileToken = readField('profile_token');
    const targetFpsRaw = readField('target_fps');
    if (!displayName || !username || !password) {
      showError(I18n.t('install_wizard.cameras_err_required'));
      return;
    }
    let targetFps = null;
    if (targetFpsRaw !== '') {
      const n = Number(targetFpsRaw);
      if (!Number.isFinite(n) || n < 1 || n > 120) {
        showError(I18n.t('install_wizard.cameras_err_fps_range'));
        return;
      }
      targetFps = n;
    }
    showError('');
    const saveBtn = footer.querySelector('[data-role="dialog-save"]');
    saveBtn?.setAttribute('disabled', '');
    try {
      const payload = {
        displayName,
        deviceServiceUrl: xa,
        username,
        password,
        profileToken: profileToken || null,
        targetFps,
      };
      const resp = await ApiBinary.action('cameraAddOnvifRequest', payload);
      const key = cameraRowKey(cam);
      const cameraId = resp?.cameraId ?? resp?.camera_id ?? null;
      const rtspUrl = resp?.rtspUrl ?? resp?.rtsp_url ?? '';
      state.cameras.added.set(key, { cameraId, rtspUrl });
      toast(I18n.t('install_wizard.cameras_add_success'), 'success');
      dlg.close(true);
      renderStep();
    } catch (err) {
      const friendly = mapCameraErrorCode(err?.code);
      showError(`${friendly}${err?.message ? ` — ${err.message}` : ''}`);
      saveBtn?.removeAttribute('disabled');
    }
  });

  // Focus first field for keyboard users.
  requestAnimationFrame(() => {
    const first = body.querySelector('tf-input[data-field="display_name"]');
    first?.focus?.();
  });
}

function attachCamerasStepHandlers(root) {
  root.querySelector('[data-role="cameras-refresh"]')?.addEventListener('click', () => {
    loadDiscoveredCameras();
  });
  root.querySelector('[data-role="cameras-skip"]')?.addEventListener('click', () => {
    if (state.currentStep < 6) {
      state.currentStep += 1;
      renderStep();
    }
  });
  const table = root.querySelector('#wizard-cameras-table');
  if (table) {
    table.rows = buildCameraRows();
    table.addEventListener('click', (ev) => {
      const path = ev.composedPath();
      const btn = path.find((el) => el && el.tagName === 'TF-BUTTON' && el.dataset && el.dataset.role === 'cam-add');
      if (!btn) return;
      const idx = Number(btn.dataset.idx);
      if (Number.isInteger(idx)) openAddCameraDialog(idx);
    });
  }

  // Auto-load on first visit.
  if (
    !state.cameras.loading
    && !state.cameras.error
    && state.cameras.discovered.length === 0
    && !state.cameras._autoLoaded
  ) {
    state.cameras._autoLoaded = true;
    loadDiscoveredCameras();
  }
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
      grantToggle?.addEventListener('change', async (e) => {
        const next = !!(e.detail?.checked ?? grantToggle.hasAttribute('checked'));
        if (next && p.risk === 'critical' && !p.criticalConfirmed) {
          const ok = await TfWindow.confirm({
            title: I18n.t('install_wizard.critical_confirm_title'),
            message: I18n.t('install_wizard.critical_confirm').replace('{pid}', pid),
            description: p.description || '',
            confirmLabel: I18n.t('install_wizard.critical_confirm_grant'),
            cancelLabel: I18n.t('common.cancel'),
            danger: true,
            icon: 'alert',
          });
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
    const kvSlider = root.querySelector('input[data-role="kv-quota"]');
    const kvDisplay = root.querySelector('[data-role="kv-quota-display"]');
    if (kvSlider && kvDisplay) {
      kvSlider.addEventListener('input', () => {
        const v = Number(kvSlider.value) || 0;
        state.storage.kvQuotaBytes = v;
        kvDisplay.textContent = formatBytes(v);
      });
    }
    const sqlSlider = root.querySelector('input[data-role="sql-quota"]');
    const sqlDisplay = root.querySelector('[data-role="sql-quota-display"]');
    if (sqlSlider && sqlDisplay) {
      sqlSlider.addEventListener('input', () => {
        const v = Number(sqlSlider.value) || 0;
        state.storage.sqlQuotaBytes = v;
        sqlDisplay.textContent = formatBytes(v);
      });
    }
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
  } else if (state.currentStep === 4) {
    attachCamerasStepHandlers(root);
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
  const ok = await TfWindow.confirm({
    title: I18n.t('install_wizard.final_confirm_title'),
    message: I18n.t('install_wizard.final_confirm'),
    confirmLabel: I18n.t('install_wizard.install'),
    cancelLabel: I18n.t('common.cancel'),
    icon: 'check',
  });
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

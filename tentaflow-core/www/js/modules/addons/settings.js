// =============================================================================
// Plik: modules/addons/settings.js
// Opis: Tab Settings dla detail addona (admin). Renderuje dynamiczny formularz
//       z schema zwroconej przez backend (AddonConfigGetRequest) i zapisuje
//       wartosci (AddonConfigSetRequest). Pola secret nie pokazuja plaintextu —
//       pusta wartosc = backend pomija (nie nadpisuje sekretu). Pakiet z
//       [robot.cloud_account] dostaje przycisk wypelnienia pol z konta
//       producenta robota; nowy adres robota po zapisie prosi o zgode sieciowa.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { Router } from '/js/router.js';
import { confirmDialog } from '/js/lib/confirm-dialog.js';
import { openRobotCloudDialog, robotCloudFieldValues, robotCloudVendorName } from '/js/modules/addons/robot-cloud-dialog.js';

let currentAddonId = null;
let currentSchema = [];
let currentValues = new Map();
let cloudProvider = '';
let requirements = [];

export const SettingsTab = {
  async mount(container, addonId) {
    currentAddonId = addonId;
    await loadAndRender(container);
  },

  unmount() {
    currentAddonId = null;
    currentSchema = [];
    currentValues = new Map();
    cloudProvider = '';
    requirements = [];
  },
};

async function loadAndRender(container) {
  container.innerHTML = `<div class="addons-empty">${escapeHtml(I18n.t('common.loading'))}</div>`;
  try {
    const resp = await ApiBinary.one('addonConfigGetRequest', { addonId: currentAddonId });
    currentSchema = Array.isArray(resp.schema) ? resp.schema : [];
    cloudProvider = resp.cloudAccountProvider ?? resp.cloud_account_provider ?? '';
    requirements = Array.isArray(resp.requirements) ? resp.requirements : [];
    // values: tablica [[k,v], ...] lub obiekt
    currentValues = new Map();
    const raw = resp.values;
    if (Array.isArray(raw)) {
      for (const entry of raw) {
        if (Array.isArray(entry) && entry.length >= 2) {
          currentValues.set(String(entry[0]), String(entry[1] ?? ''));
        }
      }
    } else if (raw && typeof raw === 'object') {
      for (const [k, v] of Object.entries(raw)) {
        currentValues.set(String(k), String(v ?? ''));
      }
    }
    render(container);
  } catch (err) {
    container.innerHTML = `<div class="addons-empty" style="color:var(--danger);">${escapeHtml(err.message)}</div>`;
  }
}

function render(container) {
  if (currentSchema.length === 0) {
    container.innerHTML = `
      <div class="empty-state">
        <svg><use href="#i-settings"/></svg>
        <div class="empty-state-text">${escapeHtml(I18n.t('addons.settings_empty_title'))}</div>
        <div class="empty-state-sub">${escapeHtml(I18n.t('addons.settings_empty_sub'))}</div>
      </div>
    `;
    return;
  }

  const rows = currentSchema.map((field) => renderField(field)).join('');
  const requirementRows = requirements.map((req) => renderRequirement(req)).join('');

  container.innerHTML = `
    <div class="card" style="padding:16px;">
      <div class="tf-toolbar" style="margin-bottom:12px;">
        <div style="font-weight:700;color:var(--text);">${escapeHtml(I18n.t('addon_settings.title'))}</div>
        <div class="tf-toolbar-spacer"></div>
        ${cloudProvider ? `<tf-button variant="secondary" icon="cloud" id="addon-settings-robot-cloud">${escapeHtml(I18n.t('addons.robot_cloud.open', { vendor: robotCloudVendorName(cloudProvider) }))}</tf-button>` : ''}
      </div>
      <form id="addon-settings-form" style="display:flex;flex-direction:column;gap:14px;">
        ${rows}
      </form>
      ${requirementRows ? `<div class="addon-settings-reqs" style="margin-top:16px;">
        <div style="font-weight:700;color:var(--text);margin-bottom:8px;">${escapeHtml(I18n.t('addon_settings.requirements_title'))}</div>
        ${requirementRows}
      </div>` : ''}
      <div style="display:flex;gap:8px;justify-content:flex-end;margin-top:16px;">
        <tf-button variant="ghost" id="addon-settings-reload" icon="refresh">
          ${escapeHtml(I18n.t('addon_settings.reload_now'))}
        </tf-button>
        <tf-button variant="primary" id="addon-settings-save" icon="check">
          ${escapeHtml(I18n.t('common.save'))}
        </tf-button>
      </div>
    </div>
  `;

  container.querySelector('#addon-settings-save')?.addEventListener('click', () => onSave(container));
  container.querySelector('#addon-settings-reload')?.addEventListener('click', () => onReload());
  container.querySelectorAll('[data-install-engine]').forEach((btn) => {
    btn.addEventListener('click', () => {
      // The catalog is where an engine is deployed; the settings form only
      // reports what this node has.
      Router.navigate('catalog', { engine: btn.getAttribute('data-install-engine') });
    });
  });
  container.querySelector('#addon-settings-robot-cloud')?.addEventListener('click', () => {
    openRobotCloudDialog({
      provider: cloudProvider,
      onPick: (device) => {
        let filled = 0;
        for (const [key, value] of Object.entries(robotCloudFieldValues(device))) {
          const el = container.querySelector(`[data-cfg-id="${CSS.escape(key)}"]`);
          if (!el) continue;
          el.value = value;
          filled += 1;
        }
        toast(I18n.t(filled > 0 ? 'addons.robot_cloud.filled_save' : 'addons.robot_cloud.nothing_to_fill'), filled > 0 ? 'success' : 'warning');
      },
    });
  });
}

/// One declared requirement: which engine, and whether this node has it. A
/// missing engine links to the catalog (that is where an engine is deployed);
/// an unsupported host says so instead of offering an install that cannot work.
function renderRequirement(req) {
  const engine = String(req.engineId ?? req.engine_id ?? '');
  const status = String(req.status ?? '');
  const tone = status === 'installed' ? 'success' : status === 'missing' ? 'warning' : 'danger';
  const label = I18n.t(`addon_settings.requirement_${status}`);
  const action = status === 'missing'
    ? `<tf-button variant="ghost" size="sm" icon="download" data-install-engine="${escapeAttr(engine)}">${escapeHtml(I18n.t('addon_settings.requirement_install'))}</tf-button>`
    : '';
  return `<div class="tf-toolbar" style="gap:8px;">
      <code>${escapeHtml(engine)}</code>
      <tf-chip tone="${escapeAttr(tone)}">${escapeHtml(label)}</tf-chip>
      <div class="tf-toolbar-spacer"></div>
      ${action}
    </div>`;
}

function renderField(field) {
  const id = String(field.id ?? '');
  const label = String(field.label ?? id);
  const type = String(field.type ?? field.fieldType ?? field.field_type ?? 'text');
  const desc = String(field.description ?? '');
  const required = !!field.required;
  const secret = !!field.secret;
  const defaultVal = field.defaultValue ?? field.default_value ?? '';
  const currentVal = currentValues.has(id) ? currentValues.get(id) : String(defaultVal);
  const options = Array.isArray(field.options) ? field.options : [];
  const requiredMark = required
    ? `<span style="color:var(--danger);" title="${escapeAttr(I18n.t('addon_settings.required'))}">*</span>`
    : '';

  const header = `
    <div style="display:flex;flex-direction:column;gap:4px;">
      <label for="cfg-${escapeAttr(id)}" style="font-weight:600;color:var(--text);font-size:13px;">
        ${escapeHtml(label)} ${requiredMark}
      </label>
      ${desc ? `<div style="color:var(--text-3);font-size:12px;">${escapeHtml(desc)}</div>` : ''}
    </div>
  `;

  let input = '';
  if (type === 'bool' || type === 'boolean') {
    const checked = String(currentVal).toLowerCase() === 'true';
    input = `<tf-toggle data-cfg-id="${escapeAttr(id)}" data-cfg-type="bool" ${checked ? 'checked' : ''}></tf-toggle>`;
  } else if (type === 'select') {
    const opts = options.map((o) => {
      const sel = String(o) === String(currentVal) ? 'selected' : '';
      return `<tf-option value="${escapeAttr(o)}" ${sel}>${escapeHtml(o)}</tf-option>`;
    }).join('');
    input = `<tf-select data-cfg-id="${escapeAttr(id)}" data-cfg-type="select">${opts}</tf-select>`;
  } else if (type === 'number' || type === 'integer') {
    input = `<tf-input data-cfg-id="${escapeAttr(id)}" data-cfg-type="number" type="number" value="${escapeAttr(currentVal)}"></tf-input>`;
  } else if (type === 'password' || secret) {
    const hasValue = currentValues.has(id) && currentVal !== '';
    const placeholder = hasValue
      ? I18n.t('addon_settings.secret_placeholder')
      : '';
    input = `<tf-input data-cfg-id="${escapeAttr(id)}" data-cfg-type="password" data-cfg-secret="1" type="password" value="" placeholder="${escapeAttr(placeholder)}"></tf-input>`;
  } else {
    input = `<tf-input data-cfg-id="${escapeAttr(id)}" data-cfg-type="text" type="text" value="${escapeAttr(currentVal)}"></tf-input>`;
  }

  return `<div style="display:flex;flex-direction:column;gap:6px;">${header}${input}</div>`;
}

async function onSave(container) {
  const entries = [];
  const fields = container.querySelectorAll('[data-cfg-id]');
  for (const el of fields) {
    const id = el.getAttribute('data-cfg-id');
    const type = el.getAttribute('data-cfg-type');
    const isSecret = el.getAttribute('data-cfg-secret') === '1';
    let val;
    if (type === 'bool') {
      val = el.hasAttribute('checked') || el.checked ? 'true' : 'false';
    } else if (type === 'select') {
      val = String(el.value ?? '');
    } else {
      val = String(el.value ?? '');
    }
    // Secret: pusta wartosc = pomin (backend zachowa poprzedni sekret).
    if (isSecret && val === '') continue;
    entries.push([id, val]);
  }
  let pendingHosts = [];
  let privacyApplied = 0;
  try {
    const res = await ApiBinary.action('addonConfigSetRequest', {
      addonId: currentAddonId,
      values: entries,
    });
    pendingHosts = Array.isArray(res?.pendingNetworkHosts) ? res.pendingNetworkHosts : [];
    privacyApplied = Number(res?.privacyCamerasApplied ?? 0);
    for (const [id, val] of entries) {
      currentValues.set(id, val);
    }
    // Patch secret inputs in place: a non-empty submission means the
    // backend now has a value stored — show the "set" placeholder and
    // clear the field so the user can tell the save landed.
    const setPlaceholder = I18n.t('addon_settings.secret_placeholder');
    container.querySelectorAll('[data-cfg-secret="1"]').forEach((el) => {
      if (String(el.value ?? '') !== '') {
        el.value = '';
        el.setAttribute('placeholder', setPlaceholder);
      }
    });
    toast(
      privacyApplied > 0
        ? I18n.t('addon_settings.save_success_privacy', { count: privacyApplied })
        : I18n.t('addon_settings.save_success'),
      'success',
    );
  } catch (err) {
    toast(`${I18n.t('addon_settings.save_error')}: ${err.message}`, 'error');
    return;
  }
  if (pendingHosts.length > 0) await approvePendingHosts(currentAddonId, pendingHosts);
}

// The saved config moved a network rule (e.g. a new robot IP). The rule is not
// approved by the save itself — the admin confirms the new host here, which is
// the same allow-list write the Network tab performs.
async function approvePendingHosts(addonId, hosts) {
  const ok = await confirmDialog({
    title: I18n.t('addon_settings.network_approve_title'),
    lead: I18n.t('addon_settings.network_approve_lead'),
    consequences: hosts,
    confirmLabel: I18n.t('addon_settings.network_approve_action'),
    cancelLabel: I18n.t('common.cancel'),
    confirmIcon: 'check',
    variant: 'primary',
  });
  if (!ok) {
    toast(I18n.t('addon_settings.network_approve_skipped'), 'warning');
    return;
  }
  try {
    const current = await ApiBinary.one('addonNetworkRulesGetRequest', { addonId });
    const allowed = Array.isArray(current.allowedHosts ?? current.allowed_hosts)
      ? [...(current.allowedHosts ?? current.allowed_hosts)]
      : [];
    for (const host of hosts) if (!allowed.includes(host)) allowed.push(host);
    await ApiBinary.action('addonNetworkRulesSetRequest', {
      addonId,
      allowedHosts: allowed,
      blockedHosts: current.blockedHosts ?? current.blocked_hosts ?? [],
      mode: current.mode ?? 'strict',
    });
    toast(I18n.t('addon_settings.network_approved'), 'success');
  } catch (err) {
    toast(`${I18n.t('addon_settings.save_error')}: ${err.message}`, 'error');
  }
}

async function onReload() {
  try {
    await ApiBinary.action('addonReloadRequest', { addonId: currentAddonId });
    toast(I18n.t('addon_reload.success'), 'success');
  } catch (err) {
    toast(`${I18n.t('addon_reload.error')}: ${err.message}`, 'error');
  }
}

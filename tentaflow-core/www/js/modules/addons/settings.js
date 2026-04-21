// =============================================================================
// Plik: modules/addons/settings.js
// Opis: Tab Settings dla detail addona (admin). Renderuje dynamiczny formularz
//       z schema zwroconej przez backend (AddonConfigGetRequest) i zapisuje
//       wartosci (AddonConfigSetRequest). Pola secret nie pokazuja plaintextu —
//       pusta wartosc = backend pomija (nie nadpisuje sekretu).
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';

let currentAddonId = null;
let currentSchema = [];
let currentValues = new Map();

export const SettingsTab = {
  async mount(container, addonId) {
    currentAddonId = addonId;
    await loadAndRender(container);
  },

  unmount() {
    currentAddonId = null;
    currentSchema = [];
    currentValues = new Map();
  },
};

async function loadAndRender(container) {
  container.innerHTML = `<div class="addons-empty">${escapeHtml(I18n.t('common.loading'))}</div>`;
  try {
    const resp = await ApiBinary.one('addonConfigGetRequest', { addonId: currentAddonId });
    currentSchema = Array.isArray(resp.schema) ? resp.schema : [];
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

  container.innerHTML = `
    <div class="card" style="padding:16px;">
      <div style="font-weight:700;color:var(--text);margin-bottom:12px;">
        ${escapeHtml(I18n.t('addon_settings.title'))}
      </div>
      <form id="addon-settings-form" style="display:flex;flex-direction:column;gap:14px;">
        ${rows}
      </form>
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
}

function renderField(field) {
  const id = String(field.id ?? '');
  const label = String(field.label ?? id);
  const type = String(field.fieldType ?? field.field_type ?? 'text');
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
  try {
    await ApiBinary.action('addonConfigSetRequest', {
      addonId: currentAddonId,
      values: entries,
    });
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
    toast(I18n.t('addon_settings.save_success'), 'success');
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

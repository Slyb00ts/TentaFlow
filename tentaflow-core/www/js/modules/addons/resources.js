// =============================================================================
// Plik: modules/addons/resources.js
// Opis: Tab Resources dla detail addona (admin). Limity zasobow: max instancji,
//       CPU %, RAM MB, storage MB, HTTP requests/min, LLM tokens/min.
//       Backend: AddonResourcesGetRequest / AddonResourcesSetRequest.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';

let currentAddonId = null;

export const ResourcesTab = {
  async mount(container, addonId) {
    currentAddonId = addonId;
    await loadAndRender(container);
  },

  unmount() {
    currentAddonId = null;
  },
};

async function loadAndRender(container) {
  container.innerHTML = `<div class="addons-empty">${escapeHtml(I18n.t('common.loading'))}</div>`;
  try {
    const resp = await ApiBinary.one('addonResourcesGetRequest', { addonId: currentAddonId });
    const values = {
      maxInstances: Number(resp.maxInstances ?? resp.max_instances ?? 0),
      cpuLimitPct: Number(resp.cpuLimitPct ?? resp.cpu_limit_pct ?? 0),
      ramMb: Number(resp.ramMb ?? resp.ram_mb ?? 0),
      storageMb: Number(resp.storageMb ?? resp.storage_mb ?? 0),
      httpRequestsPerMin: Number(resp.httpRequestsPerMin ?? resp.http_requests_per_min ?? 0),
      llmTokensPerMin: Number(resp.llmTokensPerMin ?? resp.llm_tokens_per_min ?? 0),
    };
    render(container, values);
  } catch (err) {
    container.innerHTML = `<div class="addons-empty" style="color:var(--danger);">${escapeHtml(err.message)}</div>`;
  }
}

function render(container, values) {
  const fields = [
    { id: 'maxInstances', label: I18n.t('addon_resources.max_instances'), min: 0, step: 1 },
    { id: 'cpuLimitPct', label: I18n.t('addon_resources.cpu_limit_pct'), min: 0, max: 100, step: 1, suffix: '%' },
    { id: 'ramMb', label: I18n.t('addon_resources.ram_mb'), min: 0, step: 1, suffix: 'MB' },
    { id: 'storageMb', label: I18n.t('addon_resources.storage_mb'), min: 0, step: 1, suffix: 'MB' },
    { id: 'httpRequestsPerMin', label: I18n.t('addon_resources.http_rpm'), min: 0, step: 1 },
    { id: 'llmTokensPerMin', label: I18n.t('addon_resources.llm_tpm'), min: 0, step: 1 },
  ];

  const rows = fields.map((f) => {
    const max = f.max != null ? `max="${f.max}"` : '';
    return `
      <div style="display:flex;flex-direction:column;gap:6px;">
        <label style="font-weight:600;color:var(--text);font-size:13px;">
          ${escapeHtml(f.label)}${f.suffix ? ` <span style="color:var(--text-3);font-weight:400;">(${escapeHtml(f.suffix)})</span>` : ''}
        </label>
        <tf-input
          data-res-id="${escapeAttr(f.id)}"
          type="number"
          min="${f.min}"
          ${max}
          step="${f.step}"
          value="${escapeAttr(String(values[f.id]))}"></tf-input>
        <div data-err-for="${escapeAttr(f.id)}" style="color:var(--danger);font-size:12px;display:none;"></div>
      </div>
    `;
  }).join('');

  container.innerHTML = `
    <div class="card" style="padding:16px;">
      <div style="font-weight:700;color:var(--text);margin-bottom:14px;">
        ${escapeHtml(I18n.t('addon_resources.title'))}
      </div>
      <div style="display:grid;grid-template-columns:repeat(auto-fit,minmax(240px,1fr));gap:14px;">
        ${rows}
      </div>
      <div style="display:flex;justify-content:flex-end;margin-top:16px;">
        <tf-button variant="primary" id="resources-save" icon="check">
          ${escapeHtml(I18n.t('addon_resources.save'))}
        </tf-button>
      </div>
    </div>
  `;

  container.querySelector('#resources-save')?.addEventListener('click', () => onSave(container));
}

async function onSave(container) {
  const fields = container.querySelectorAll('[data-res-id]');
  const payload = { addonId: currentAddonId };
  let valid = true;
  for (const el of fields) {
    const id = el.getAttribute('data-res-id');
    const raw = String(el.value ?? '').trim();
    const num = Number(raw);
    const errBox = container.querySelector(`[data-err-for="${id}"]`);
    if (errBox) { errBox.style.display = 'none'; errBox.textContent = ''; }
    if (!Number.isFinite(num) || num < 0) {
      valid = false;
      if (errBox) {
        errBox.textContent = I18n.t('addon_resources.validation_positive');
        errBox.style.display = 'block';
      }
      continue;
    }
    if (id === 'cpuLimitPct' && num > 100) {
      valid = false;
      if (errBox) {
        errBox.textContent = I18n.t('addon_resources.validation_cpu');
        errBox.style.display = 'block';
      }
      continue;
    }
    payload[id] = num;
  }
  if (!valid) {
    toast(I18n.t('common.invalid_input'), 'error');
    return;
  }
  try {
    await ApiBinary.action('addonResourcesSetRequest', payload);
    toast(I18n.t('common.saved'), 'success');
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

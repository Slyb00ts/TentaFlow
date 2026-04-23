// =============================================================================
// File: modules/addons/tools.js
// Description: Tools tab for addon detail. Simple 3-column table (name,
//              description, parameters) matching the current dashboard layout.
//              Parameters render as compact chips (name:type) colored by
//              required/optional. Backend: AddonToolsRequest.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';

let currentAddonId = null;

export const ToolsTab = {
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
    const resp = await ApiBinary.one('addonToolsRequest', { addonId: currentAddonId });
    const tools = Array.isArray(resp.tools) ? resp.tools : [];
    render(container, tools);
  } catch (err) {
    container.innerHTML = `<div class="addons-empty" style="color:var(--danger);">${escapeHtml(err.message)}</div>`;
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

function render(container, tools) {
  if (tools.length === 0) {
    container.innerHTML = `
      <div class="empty-state">
        <svg><use href="#i-play"/></svg>
        <div class="empty-state-text">${escapeHtml(I18n.t('addons.tools_empty_title'))}</div>
        <div class="empty-state-sub">${escapeHtml(I18n.t('addons.tools_empty_sub'))}</div>
      </div>
    `;
    return;
  }

  container.innerHTML = `
    <div class="section-card" style="padding:0;overflow:auto;">
      <tf-table id="tools-table">
        <tf-column key="name" label="${escapeAttr(I18n.t('addons.tools_col_name'))}" renderer="html"></tf-column>
        <tf-column key="description" label="${escapeAttr(I18n.t('addons.tools_col_description'))}"></tf-column>
        <tf-column key="params" label="${escapeAttr(I18n.t('addons.tools_col_parameters'))}" renderer="html"></tf-column>
      </tf-table>
    </div>
  `;

  const tbl = container.querySelector('#tools-table');
  if (!tbl) return;
  tbl.rows = tools.map((t, idx) => {
    const name = String(t.name ?? `tool-${idx}`);
    const desc = String(t.description ?? '');
    const returnType = String(t.returnType ?? t.return_type ?? '');
    const params = Array.isArray(t.parameters) ? t.parameters : [];
    const nameCell = `<code style="font-family:'SF Mono',monospace;font-weight:600;color:var(--text);">${escapeHtml(name)}</code>${returnType ? ` <span style="color:var(--text-3);font-size:11px;">→ ${escapeHtml(returnType)}</span>` : ''}`;
    const paramsCell = renderParamsCell(params);
    return { name: nameCell, description: desc, params: paramsCell };
  });
}

function renderParamsCell(params) {
  if (!params || params.length === 0) {
    return '<span style="color:var(--text-3);">—</span>';
  }
  return params.map((p) => {
    const pname = String(p.name ?? '');
    const ptype = String(p.paramType ?? p.param_type ?? 'any');
    const preq = !!p.required;
    const status = preq ? 'warn' : 'info';
    const suffix = preq ? '*' : '';
    const title = preq
      ? I18n.t('addons.tool_param_required')
      : I18n.t('addons.tool_param_optional');
    return `<tf-chip status="${status}" title="${escapeAttr(title)}" style="margin-right:4px;margin-bottom:2px;"><code style="font-family:'SF Mono',monospace;">${escapeHtml(pname)}</code>:${escapeHtml(ptype)}${suffix}</tf-chip>`;
  }).join('');
}

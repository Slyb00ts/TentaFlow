// =============================================================================
// Plik: modules/flows/FlowsBinary.js
// Opis: Flows ekran zmigrowany na binary protocol (Task #37 demo).
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

const FlowsBinary = (() => {
  'use strict';

  let flowsList = [];

  async function loadFlows() {
    try {
      flowsList = await ApiBinary.list('flowListRequest');
      renderTable();
    } catch (err) {
      console.error('[flows-binary] load failed:', err);
      flowsList = [];
      renderTable();
    }
  }

  function renderTable() {
    const tbody = document.getElementById('flows-tbody');
    if (!tbody) return;
    tbody.innerHTML = flowsList.length === 0
      ? `<tr><td colspan="4"><div class="empty-state"><div class="empty-state-text">${I18n.t('flows.empty')}</div></div></td></tr>`
      : flowsList.map(f => `
          <tr>
            <td>${Utils.escapeHtml(f.name)}</td>
            <td>${Utils.escapeHtml(f.description ?? '')}</td>
            <td><span class="badge badge-${f.enabled ? 'success' : 'neutral'}">${f.enabled ? 'enabled' : 'disabled'}</span></td>
            <td>
              <button class="btn btn-ghost btn-sm" data-edit="${f.id}">${I18n.t('common.edit')}</button>
              <button class="btn btn-ghost btn-sm" data-delete="${f.id}">${I18n.t('common.delete')}</button>
            </td>
          </tr>
        `).join('');
    tbody.querySelectorAll('[data-delete]').forEach(b => b.addEventListener('click', () => deleteFlow(b.dataset.delete)));
  }

  async function deleteFlow(flowId) {
    if (!confirm(I18n.t('flows.delete_confirm'))) return;
    try {
      const r = await ApiBinary.action('flowDeleteRequest', { flowId });
      if (r.deleted) loadFlows();
    } catch (err) {
      App.showToast(err.message, 'error');
    }
  }

  return {
    mount: () => loadFlows(),
    unmount: () => { flowsList = []; },
  };
})();

export default FlowsBinary;

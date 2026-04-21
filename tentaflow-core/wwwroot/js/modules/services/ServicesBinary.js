// =============================================================================
// Plik: modules/services/ServicesBinary.js
// Opis: Services ekran (deployments) zmigrowany na binary protocol.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

const ServicesBinary = (() => {
  'use strict';

  let servicesList = [];

  async function loadServices() {
    try {
      servicesList = await ApiBinary.list('serviceListRequest');
      renderTable();
    } catch (err) {
      console.error('[services-binary] load failed:', err);
      servicesList = [];
      renderTable();
    }
  }

  function renderTable() {
    const tbody = document.getElementById('services-tbody');
    if (!tbody) return;
    tbody.innerHTML = servicesList.length === 0
      ? `<tr><td colspan="5"><div class="empty-state"><div class="empty-state-text">${I18n.t('services.empty')}</div></div></td></tr>`
      : servicesList.map(s => `
          <tr>
            <td>${Utils.escapeHtml(s.engineId)}</td>
            <td>${Utils.escapeHtml(s.modelId)}</td>
            <td><span class="badge badge-${s.status === 'running' ? 'success' : 'warning'}">${s.status}</span></td>
            <td>${Utils.escapeHtml(s.endpointUrl ?? '-')}</td>
            <td>
              <button class="btn btn-ghost btn-sm" data-stop="${s.id}">${I18n.t('common.stop')}</button>
            </td>
          </tr>
        `).join('');
    tbody.querySelectorAll('[data-stop]').forEach(b => b.addEventListener('click', () => stopService(b.dataset.stop)));
  }

  async function stopService(serviceId) {
    if (!confirm(I18n.t('services.stop_confirm'))) return;
    try {
      const r = await ApiBinary.action('serviceStopRequest', { serviceId });
      if (r.stopped) loadServices();
    } catch (err) {
      App.showToast(err.message, 'error');
    }
  }

  return {
    mount: () => loadServices(),
    unmount: () => { servicesList = []; },
  };
})();

export default ServicesBinary;

// =============================================================================
// Plik: modules/registries/RegistriesBinary.js
// Opis: Registries ekran zmigrowany na binary protocol.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

const RegistriesBinary = (() => {
  'use strict';
  let registries = [];

  async function loadRegistries() {
    try {
      registries = await ApiBinary.list('registryListRequest');
      renderTable();
    } catch (err) {
      console.error('[registries-binary] load failed:', err);
      registries = [];
      renderTable();
    }
  }

  function renderTable() {
    const tbody = document.getElementById('registries-tbody');
    if (!tbody) return;
    tbody.innerHTML = registries.length === 0
      ? `<tr><td colspan="4"><div class="empty-state"><div class="empty-state-text">${I18n.t('registries.empty')}</div></div></td></tr>`
      : registries.map(r => `
          <tr>
            <td>${Utils.escapeHtml(r.url)}</td>
            <td>${Utils.escapeHtml(r.kind)}</td>
            <td>${r.authRequired ? I18n.t('common.yes') : I18n.t('common.no')}</td>
            <td><span class="badge">${r.id}</span></td>
          </tr>
        `).join('');
  }

  return {
    mount: () => loadRegistries(),
    unmount: () => { registries = []; },
  };
})();

export default RegistriesBinary;

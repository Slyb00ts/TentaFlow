// =============================================================================
// Plik: modules/addons/AddonsBinary.js
// Opis: Addons (WASM plugins) ekran zmigrowany na binary protocol.
//       Bootstrap pokrywa container_list (addons jako kontenery); pelny
//       AddonList z permission tree po dopiac AddonListRequest variant w
//       phase 2.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

const AddonsBinary = (() => {
  'use strict';
  let containers = [];

  async function loadAddons() {
    try {
      // Bootstrap: addony przeplyaja przez container layer.
      containers = await ApiBinary.list('containerListRequest');
      renderTable();
    } catch (err) {
      console.error('[addons-binary] load failed:', err);
      containers = [];
      renderTable();
    }
  }

  function renderTable() {
    const tbody = document.getElementById('addons-tbody');
    if (!tbody) return;
    tbody.innerHTML = containers.length === 0
      ? `<tr><td colspan="3"><div class="empty-state"><div class="empty-state-text">${I18n.t('addons.empty')}</div></div></td></tr>`
      : containers.map(c => `
          <tr>
            <td>${Utils.escapeHtml(c.name)}</td>
            <td>${Utils.escapeHtml(c.image)}</td>
            <td><span class="badge">${c.state}</span></td>
          </tr>
        `).join('');
  }

  return {
    mount: () => loadAddons(),
    unmount: () => { containers = []; },
  };
})();

export default AddonsBinary;

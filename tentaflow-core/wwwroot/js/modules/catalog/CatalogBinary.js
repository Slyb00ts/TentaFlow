// =============================================================================
// Plik: modules/catalog/CatalogBinary.js
// Opis: Catalog (Hub engine list) ekran zmigrowany na binary protocol.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

const CatalogBinary = (() => {
  'use strict';
  let engines = [];

  async function loadEngines() {
    try {
      engines = await ApiBinary.list('hubEngineListRequest');
      renderGrid();
    } catch (err) {
      console.error('[catalog-binary] load failed:', err);
      engines = [];
      renderGrid();
    }
  }

  function renderGrid() {
    const grid = document.getElementById('catalog-grid');
    if (!grid) return;
    grid.innerHTML = engines.length === 0
      ? `<div class="empty-state"><div class="empty-state-text">${I18n.t('catalog.empty')}</div></div>`
      : engines.map(e => `
          <div class="catalog-card">
            <h3>${Utils.escapeHtml(e.displayName)}</h3>
            <p class="badge">${Utils.escapeHtml(e.category)}</p>
            <p>port: ${e.defaultPort}</p>
            <p>deploy: ${e.deployMethods.join(', ')}</p>
          </div>
        `).join('');
  }

  return {
    mount: () => loadEngines(),
    unmount: () => { engines = []; },
  };
})();

export default CatalogBinary;

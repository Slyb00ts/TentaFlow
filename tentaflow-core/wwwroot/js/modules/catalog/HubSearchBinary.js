// =============================================================================
// Plik: modules/catalog/HubSearchBinary.js
// Opis: HuggingFace hub search ekran zmigrowany na binary protocol.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

const HubSearchBinary = (() => {
  'use strict';
  let results = [];

  async function search(query) {
    try {
      const body = await ApiBinary.one('hubModelSearchRequest', { query });
      results = body.results ?? [];
      renderResults();
    } catch (err) {
      console.error('[hub-search] failed:', err);
      results = [];
      renderResults();
    }
  }

  function renderResults() {
    const tbody = document.getElementById('hub-search-tbody');
    if (!tbody) return;
    tbody.innerHTML = results.length === 0
      ? `<tr><td colspan="4"><div class="empty-state"><div class="empty-state-text">${I18n.t('hub.no_results')}</div></div></td></tr>`
      : results.map(r => `
          <tr>
            <td>${Utils.escapeHtml(r.repoId)}</td>
            <td>${Utils.escapeHtml(r.author)}</td>
            <td>${r.downloads}</td>
            <td>${r.likes}</td>
          </tr>
        `).join('');
  }

  return {
    mount: () => {
      document.getElementById('hub-search-btn')?.addEventListener('click', () => {
        const q = document.getElementById('hub-search-query')?.value;
        if (q) search(q);
      });
    },
    unmount: () => { results = []; },
    search,
  };
})();

export default HubSearchBinary;

// =============================================================================
// Plik: modules/prompts/PromptsBinary.js
// Opis: Prompts ekran (templates list) zmigrowany na binary protocol.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

const PromptsBinary = (() => {
  'use strict';
  let prompts = [];

  async function loadPrompts() {
    try {
      prompts = await ApiBinary.list('promptListRequest');
      renderTable();
    } catch (err) {
      console.error('[prompts-binary] load failed:', err);
      prompts = [];
      renderTable();
    }
  }

  function renderTable() {
    const tbody = document.getElementById('prompts-tbody');
    if (!tbody) return;
    tbody.innerHTML = prompts.length === 0
      ? `<tr><td colspan="3"><div class="empty-state"><div class="empty-state-text">${I18n.t('prompts.empty')}</div></div></td></tr>`
      : prompts.map(p => `
          <tr>
            <td>${Utils.escapeHtml(p.name)}</td>
            <td>${Utils.escapeHtml(p.category)}</td>
            <td>${Utils.formatDate(p.updatedAtEpoch * 1000)}</td>
          </tr>
        `).join('');
  }

  return {
    mount: () => loadPrompts(),
    unmount: () => { prompts = []; },
  };
})();

export default PromptsBinary;

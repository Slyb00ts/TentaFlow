// =============================================================================
// Plik: modules/prompts.js
// Opis: Lista promptów + szczegóły.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, toast, formatDate } from '/js/utils.js';

let prompts = [];

const PromptsScreen = {
  title: 'Prompty',
  render() {
    return `
      <div class="content-header"><h1>Prompty</h1></div>
      <div class="card" style="padding: 0;"><div id="prompts-host"></div></div>
      <div id="prompt-detail-host"></div>`;
  },
  async mount() {
    try {
      prompts = await ApiBinary.list('promptListRequest');
      renderTable();
    } catch (err) { toast(`Błąd: ${err.message}`, 'error'); }
  },
  unmount() { prompts = []; },
};

function renderTable() {
  const host = byId('prompts-host');
  if (prompts.length === 0) {
    host.innerHTML = `<div class="empty-state"><div class="empty-state-text">Brak promptów</div></div>`;
    return;
  }
  host.innerHTML = `
    <table class="data-table">
      <thead><tr><th>Nazwa</th><th>Kategoria</th><th>Aktualizacja</th><th></th></tr></thead>
      <tbody>
        ${prompts.map((p) => `
          <tr>
            <td>${escapeHtml(p.name)}</td>
            <td><tf-chip status="accent">${escapeHtml(p.category)}</tf-chip></td>
            <td>${formatDate(p.updatedAtEpoch)}</td>
            <td><tf-button variant="secondary" size="sm" data-detail="${escapeHtml(p.id)}">Pokaż</tf-button></td>
          </tr>`).join('')}
      </tbody>
    </table>`;
  host.querySelectorAll('[data-detail]').forEach((b) => {
    b.addEventListener('click', () => showDetail(b.dataset.detail));
  });
}

async function showDetail(promptId) {
  try {
    const d = await ApiBinary.one('promptDetailRequest', { promptId });
    const host = byId('prompt-detail-host');
    host.innerHTML = `
      <div class="card" style="margin-top: var(--space-4);">
        <div class="card-header">
          <h3 class="card-title">${escapeHtml(d.name)}</h3>
          <tf-button variant="ghost" size="sm" id="close-prompt">×</tf-button>
        </div>
        <div class="form-row"><span class="label">Kategoria</span><div>${escapeHtml(d.category)}</div></div>
        <div class="form-row"><span class="label">Zmienne</span>
          <div>${d.variables.map((v) => `<tf-chip status="accent">${escapeHtml(v)}</tf-chip>`).join(' ') || '—'}</div></div>
        <div class="form-row"><span class="label">Treść</span>
          <pre style="background: var(--color-bg); padding: var(--space-3); border-radius: var(--radius-md); border: 1px solid var(--color-border); white-space: pre-wrap;">${escapeHtml(d.template)}</pre>
        </div>
      </div>`;
    byId('close-prompt').addEventListener('click', () => { host.innerHTML = ''; });
  } catch (err) { toast(`Błąd: ${err.message}`, 'error'); }
}

export default PromptsScreen;

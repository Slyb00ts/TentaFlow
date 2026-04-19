// =============================================================================
// Plik: modules/hub.js
// Opis: Lista silnikow z manifestu (HubEngineList).
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, toast } from '/js/utils.js';

const HubScreen = {
  title: 'Hub silników',
  render() {
    return `
      <div class="content-header"><h1>Hub silników</h1></div>
      <div id="engines-host"></div>`;
  },
  async mount() {
    try {
      const engines = await ApiBinary.list('hubEngineListRequest');
      const host = byId('engines-host');
      if (engines.length === 0) {
        host.innerHTML = `<div class="empty-state"><div class="empty-state-text">Brak silników</div></div>`;
        return;
      }
      host.innerHTML = `
        <div class="metric-grid">
          ${engines.map((e) => `
            <div class="card">
              <div class="card-header">
                <h3 class="card-title">${escapeHtml(e.displayName)}</h3>
                <tf-chip status="accent">${escapeHtml(e.category)}</tf-chip>
              </div>
              <div class="form-row"><span class="label">ID</span><div><code>${escapeHtml(e.id)}</code></div></div>
              <div class="form-row"><span class="label">Port domyślny</span><div>${e.defaultPort}</div></div>
              <div class="form-row"><span class="label">Tryby deploymentu</span>
                <div>${e.deployMethods.map((m) => `<tf-chip status="accent">${escapeHtml(m)}</tf-chip>`).join(' ')}</div>
              </div>
            </div>`).join('')}
        </div>`;
    } catch (err) { toast(`Błąd: ${err.message}`, 'error'); }
  },
  unmount() {},
};

export default HubScreen;

// =============================================================================
// Plik: modules/registries.js
// Opis: Lista rejestrów (Docker/Conda/HF).
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, toast } from '/js/utils.js';

const RegistriesScreen = {
  title: 'Rejestry',
  render() {
    return `
      <div class="content-header"><h1>Rejestry</h1></div>
      <div class="card" style="padding: 0;"><div id="reg-host"></div></div>`;
  },
  async mount() {
    try {
      const regs = await ApiBinary.list('registryListRequest');
      const host = byId('reg-host');
      if (regs.length === 0) {
        host.innerHTML = `<div class="empty-state"><div class="empty-state-text">Brak rejestrów</div></div>`;
        return;
      }
      host.innerHTML = `
        <table class="data-table">
          <thead><tr><th>URL</th><th>Typ</th><th>Auth</th></tr></thead>
          <tbody>
            ${regs.map((r) => `<tr>
              <td><code>${escapeHtml(r.url)}</code></td>
              <td><tf-chip status="accent">${escapeHtml(r.kind)}</tf-chip></td>
              <td>${r.authRequired ? '<tf-chip status="warn">tak</tf-chip>' : '<tf-chip status="ok">nie</tf-chip>'}</td>
            </tr>`).join('')}
          </tbody>
        </table>`;
    } catch (err) { toast(`Błąd: ${err.message}`, 'error'); }
  },
  unmount() {},
};

export default RegistriesScreen;

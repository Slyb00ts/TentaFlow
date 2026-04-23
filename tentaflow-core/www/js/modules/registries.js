// =============================================================================
// Plik: modules/registries.js
// Opis: Lista rejestrów (Docker/Conda/HF).
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';

const RegistriesScreen = {
  get title() { return I18n.t('registries.title'); },
  render() {
    return `
      <div class="content-header"><h1>${escapeHtml(I18n.t('registries.title'))}</h1></div>
      <div class="card" style="padding: 0;"><div id="reg-host"></div></div>`;
  },
  async mount() {
    try {
      const regs = await ApiBinary.list('registryListRequest');
      const host = byId('reg-host');
      if (regs.length === 0) {
        host.innerHTML = `<div class="empty-state"><div class="empty-state-text">${escapeHtml(I18n.t('registries.empty'))}</div></div>`;
        return;
      }
      host.innerHTML = `
        <table class="data-table">
          <thead><tr>
            <th>${escapeHtml(I18n.t('registries.col_url'))}</th>
            <th>${escapeHtml(I18n.t('registries.col_type'))}</th>
            <th>${escapeHtml(I18n.t('registries.col_auth'))}</th>
          </tr></thead>
          <tbody>
            ${regs.map((r) => `<tr>
              <td><code>${escapeHtml(r.url)}</code></td>
              <td><tf-chip status="accent">${escapeHtml(r.kind)}</tf-chip></td>
              <td>${r.authRequired ? `<tf-chip status="warn">${escapeHtml(I18n.t('registries.auth_yes'))}</tf-chip>` : `<tf-chip status="ok">${escapeHtml(I18n.t('registries.auth_no'))}</tf-chip>`}</td>
            </tr>`).join('')}
          </tbody>
        </table>`;
    } catch (err) { toast(`${I18n.t('registries.error_prefix')}: ${err.message}`, 'error'); }
  },
  unmount() {},
};

export default RegistriesScreen;

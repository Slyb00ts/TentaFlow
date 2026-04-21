// =============================================================================
// Plik: modules/settings/SettingsBinary.js
// Opis: Settings ekran zmigrowany na binary protocol.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

const SettingsBinary = (() => {
  'use strict';
  let entries = [];

  async function loadSettings() {
    try {
      entries = await ApiBinary.list('settingsListRequest', { arrayKey: 'entries' });
      renderForm();
    } catch (err) {
      console.error('[settings-binary] load failed:', err);
      entries = [];
      renderForm();
    }
  }

  function renderForm() {
    const container = document.getElementById('settings-form');
    if (!container) return;
    container.innerHTML = entries.map(e => `
      <div class="form-row">
        <label for="set-${Utils.escapeHtml(e.key)}">${Utils.escapeHtml(e.key)}</label>
        <input id="set-${Utils.escapeHtml(e.key)}" type="${e.isSecret ? 'password' : 'text'}"
               value="${Utils.escapeHtml(e.value)}" data-key="${Utils.escapeHtml(e.key)}"
               data-secret="${e.isSecret}" />
      </div>
    `).join('');
  }

  async function saveSettings() {
    const inputs = document.querySelectorAll('#settings-form input[data-key]');
    for (const input of inputs) {
      try {
        await ApiBinary.action('settingsUpdateSingle', {
          key: input.dataset.key,
          value: input.value,
          isSecret: input.dataset.secret === 'true',
        });
      } catch (err) {
        App.showToast(`${input.dataset.key}: ${err.message}`, 'error');
        return;
      }
    }
    App.showToast(I18n.t('settings.saved'), 'success');
  }

  return {
    mount: () => {
      loadSettings();
      document.getElementById('btn-save-settings')?.addEventListener('click', saveSettings);
    },
    unmount: () => { entries = []; },
  };
})();

export default SettingsBinary;

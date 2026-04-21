// =============================================================================
// Plik: modules/apikeys/ApiKeysBinary.js
// Opis: ApiKeys ekran zmigrowany na binary WS protocol (Task #37 demo).
//       Logika identyczna jak ApiKeys.js, ale REST calls -> ApiBinary
//       (dispatch przez WebSocket Envelope+MessageBody).
//
//       Wzorzec do skopiowania dla pozostalych 24 ekranow:
//         1. import { ApiBinary } from '/js/protocol/api-binary-shim.js'
//         2. ApiClient.get('/api/...') -> ApiBinary.list('xVariantRequest')
//         3. ApiClient.post('/api/...') -> ApiBinary.action('xCreateRequest', payload)
//         4. ApiClient.delete('/api/...') -> ApiBinary.action('xDeleteRequest', { id })
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

const ApiKeysBinary = (() => {
  'use strict';

  let keysList = [];

  async function loadKeys() {
    try {
      keysList = await ApiBinary.list('apiKeyListRequest');
      renderTable();
    } catch (err) {
      console.error('[apikeys-binary] load failed:', err);
      keysList = [];
      renderTable();
    }
  }

  function renderTable() {
    const tbody = document.getElementById('apikeys-tbody');
    if (!tbody) return;

    if (keysList.length === 0) {
      tbody.innerHTML = `
        <tr>
          <td colspan="7">
            <div class="empty-state">
              <div class="empty-state-text" data-i18n="apikeys.empty">${I18n.t('apikeys.empty')}</div>
            </div>
          </td>
        </tr>
      `;
      return;
    }

    tbody.innerHTML = keysList.map(k => `
      <tr>
        <td><code>${Utils.escapeHtml(k.keyId)}</code></td>
        <td>${Utils.escapeHtml(k.name)}</td>
        <td>${Utils.formatDate(k.createdAtEpoch * 1000)}</td>
        <td>${k.lastUsedAtEpoch ? Utils.formatDate(k.lastUsedAtEpoch * 1000) : '-'}</td>
        <td>
          <button class="btn btn-ghost btn-sm" data-revoke="${k.keyId}">
            ${I18n.t('apikeys.deactivate')}
          </button>
        </td>
      </tr>
    `).join('');

    tbody.querySelectorAll('[data-revoke]').forEach(btn => {
      btn.addEventListener('click', () => handleRevoke(btn.dataset.revoke));
    });
  }

  async function handleRevoke(keyId) {
    if (!confirm(I18n.t('apikeys.deactivate_confirm'))) return;
    try {
      const result = await ApiBinary.action('apiKeyRevokeRequest', { keyId });
      if (result.deleted) {
        App.showToast(I18n.t('apikeys.deactivate_success'), 'success');
        await loadKeys();
      }
    } catch (err) {
      App.showToast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  }

  async function handleGenerate() {
    const name = document.getElementById('key-name')?.value?.trim();
    if (!name) return;

    try {
      const result = await ApiBinary.action('apiKeyCreateRequest', { name, scopes: [] });
      const resultEl = document.getElementById('gen-key-result');
      if (resultEl) {
        resultEl.hidden = false;
        resultEl.querySelector('.key-value').textContent = result.token;
      }
    } catch (err) {
      const errorEl = document.getElementById('gen-key-error');
      if (errorEl) {
        errorEl.textContent = err.message;
        errorEl.hidden = false;
      }
    }
  }

  return {
    mount: () => {
      document.getElementById('btn-generate-key')?.addEventListener('click', () => {
        document.getElementById('generate-key-modal')?.classList.add('active');
      });
      document.getElementById('gen-modal-submit')?.addEventListener('click', handleGenerate);
      loadKeys();
    },
    unmount: () => {
      keysList = [];
    },
  };
})();

export default ApiKeysBinary;

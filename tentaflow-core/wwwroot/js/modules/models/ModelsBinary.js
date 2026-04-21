// =============================================================================
// Plik: modules/models/ModelsBinary.js
// Opis: Models ekran zmigrowany na binary protocol (Task #37 demo).
//       Pokrywa ModelList + ModelDetail + ModelInstall + ModelDelete variants.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

const ModelsBinary = (() => {
  'use strict';

  let modelsList = [];

  async function loadModels() {
    try {
      modelsList = await ApiBinary.list('modelListRequest');
      renderTable();
    } catch (err) {
      console.error('[models-binary] load failed:', err);
      modelsList = [];
      renderTable();
    }
  }

  function renderTable() {
    const tbody = document.getElementById('models-tbody');
    if (!tbody) return;

    if (modelsList.length === 0) {
      tbody.innerHTML = `
        <tr><td colspan="5">
          <div class="empty-state">
            <div class="empty-state-text" data-i18n="models.empty">${I18n.t('models.empty')}</div>
          </div>
        </td></tr>
      `;
      return;
    }

    tbody.innerHTML = modelsList.map(m => `
      <tr>
        <td>${Utils.escapeHtml(m.id)}</td>
        <td>${Utils.escapeHtml(m.category)}</td>
        <td>${Utils.escapeHtml(m.engineId)}</td>
        <td>
          <span class="badge badge-${m.availability === 'ready' ? 'success' : 'warning'}">
            ${m.availability}
          </span>
        </td>
        <td>
          <button class="btn btn-ghost btn-sm" data-detail="${m.id}">
            ${I18n.t('common.details')}
          </button>
          <button class="btn btn-ghost btn-sm" data-delete="${m.id}">
            ${I18n.t('common.delete')}
          </button>
        </td>
      </tr>
    `).join('');

    tbody.querySelectorAll('[data-detail]').forEach(btn => {
      btn.addEventListener('click', () => showDetail(btn.dataset.detail));
    });
    tbody.querySelectorAll('[data-delete]').forEach(btn => {
      btn.addEventListener('click', () => deleteModel(btn.dataset.delete));
    });
  }

  async function showDetail(modelId) {
    try {
      const body = await ApiBinary.one('modelDetailRequest', { modelId });
      console.log('[models-binary] detail:', body);
      // TODO: integracja z ModelForm modal
    } catch (err) {
      App.showToast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  }

  async function deleteModel(modelId) {
    if (!confirm(I18n.t('models.delete_confirm'))) return;
    try {
      const result = await ApiBinary.action('modelDeleteRequest', { modelId });
      if (result.deleted) {
        App.showToast(I18n.t('models.delete_success'), 'success');
        await loadModels();
      }
    } catch (err) {
      App.showToast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  }

  async function installModel(modelId, sourceRepo) {
    try {
      const result = await ApiBinary.action('modelInstallRequest', { modelId, sourceRepo });
      if (result.accepted) {
        App.showToast(I18n.t('models.install_started'), 'success');
        await loadModels();
      }
    } catch (err) {
      App.showToast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  }

  return {
    mount: () => {
      loadModels();
    },
    unmount: () => {
      modelsList = [];
    },
    installModel,
  };
})();

export default ModelsBinary;

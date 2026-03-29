// =============================================================================
// Plik: modules/registries/Registries.js
// Opis: Widok zarzadzania rejestrami Docker - tabela CRUD, badge typow,
//       dynamiczny modal, delegacja zdarzen.
// Przyklad: ViewRouter.register('registries', Registries);
// =============================================================================

const Registries = (() => {
  'use strict';

  let registriesList = [];
  let editingId = null;
  let abortController = null;
  let activeModal = null;

  // Renderowanie HTML widoku
  function render() {
    return `
      <div class="card">
        <div class="card-header">
          <h3 data-i18n="registries.title">${I18n.t('registries.title')}</h3>
          <button class="btn btn-primary btn-sm" id="btn-add-registry" data-i18n="common.add">+ ${I18n.t('common.add')}</button>
        </div>
        <div class="card-body no-padding">
          <div class="table-wrapper">
            <table>
              <thead>
                <tr>
                  <th data-i18n="common.name">${I18n.t('common.name')}</th>
                  <th data-i18n="registries.type">${I18n.t('registries.type')}</th>
                  <th data-i18n="settings.portainer.url">${I18n.t('settings.portainer.url')}</th>
                  <th data-i18n="settings.portainer.form.username">${I18n.t('settings.portainer.form.username')}</th>
                  <th data-i18n="common.status">${I18n.t('common.status')}</th>
                  <th data-i18n="common.actions">${I18n.t('common.actions')}</th>
                </tr>
              </thead>
              <tbody id="registries-tbody">
                <tr>
                  <td colspan="6">
                    <div class="empty-state">
                      <div class="empty-state-text" data-i18n="common.loading">${I18n.t('common.loading')}</div>
                    </div>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
        </div>
      </div>
    `;
  }

  // Montowanie - zaladuj dane, podepnij zdarzenia
  function mount() {
    abortController = new AbortController();
    const signal = abortController.signal;

    loadRegistries();

    const addBtn = document.getElementById('btn-add-registry');
    if (addBtn) {
      addBtn.addEventListener('click', () => openRegistryModal(null), { signal });
    }

    // Delegacja zdarzen na tbody zamiast N listenerow na N elementow
    const tbody = document.getElementById('registries-tbody');
    if (tbody) {
      tbody.addEventListener('click', handleTableClick, { signal });
    }
  }

  // Odmontowanie
  function unmount() {
    if (abortController) {
      abortController.abort();
      abortController = null;
    }
    registriesList = [];
    editingId = null;
    // Usun modal rejestru jesli jest otwarty
    if (activeModal && activeModal.parentNode) {
      activeModal.remove();
      activeModal = null;
    }
  }

  // Zaladowanie rejestrow z API
  async function loadRegistries() {
    try {
      registriesList = await ApiClient.get('/api/registries');
      renderTable();
    } catch (e) {
      App.showToast(I18n.t('registries.load_error').replace('{error}', e.message), 'error');
    }
  }

  // Renderowanie tabeli
  function renderTable() {
    const tbody = document.getElementById('registries-tbody');
    if (!tbody) return;

    if (!registriesList.length) {
      tbody.innerHTML = `
        <tr>
          <td colspan="6">
            <div class="empty-state">
              <div class="empty-state-icon">&#128230;</div>
              <div class="empty-state-text" data-i18n="registries.empty">${I18n.t('registries.empty')}</div>
              <div class="empty-state-hint" data-i18n="registries.empty_hint">${I18n.t('registries.empty_hint')}</div>
            </div>
          </td>
        </tr>
      `;
      return;
    }

    tbody.innerHTML = registriesList.map(r => {
      const typeBadge = `<span class="badge badge-info">${Utils.escapeHtml(r.registry_type)}</span>`;
      const statusBadge = r.is_active
        ? `<span class="badge badge-success"><span class="status-dot status-dot-green"></span>${I18n.t('common.active')}</span>`
        : `<span class="badge badge-error"><span class="status-dot status-dot-red"></span>${I18n.t('common.inactive')}</span>`;

      return `
        <tr>
          <td><strong>${Utils.escapeHtml(r.name)}</strong></td>
          <td>${typeBadge}</td>
          <td>${Utils.escapeHtml(r.url)}</td>
          <td>${r.username ? Utils.escapeHtml(r.username) : '<span class="text-muted">-</span>'}</td>
          <td>${statusBadge}</td>
          <td>
            <div style="display: flex; gap: 4px;">
              <button class="btn btn-ghost btn-sm" data-edit-registry="${r.id}" title="${I18n.t('common.edit')}" data-i18n-title="common.edit">&#9998;</button>
              <button class="btn btn-ghost btn-sm" data-test-registry="${r.id}" title="${I18n.t('settings.portainer.test')}" data-i18n-title="settings.portainer.test">&#9881;</button>
              <button class="btn btn-ghost btn-sm" data-delete-registry="${r.id}" title="${I18n.t('common.delete')}" data-i18n-title="common.delete">&#10005;</button>
            </div>
          </td>
        </tr>
      `;
    }).join('');
  }

  // Delegowany handler klikniec w tabeli
  function handleTableClick(e) {
    const editBtn = e.target.closest('[data-edit-registry]');
    if (editBtn) {
      const id = parseInt(editBtn.dataset.editRegistry, 10);
      const reg = registriesList.find(r => r.id === id);
      if (reg) openRegistryModal(reg);
      return;
    }

    const testBtn = e.target.closest('[data-test-registry]');
    if (testBtn) {
      const id = parseInt(testBtn.dataset.testRegistry, 10);
      testRegistry(id, testBtn);
      return;
    }

    const deleteBtn = e.target.closest('[data-delete-registry]');
    if (deleteBtn) {
      const id = parseInt(deleteBtn.dataset.deleteRegistry, 10);
      deleteRegistry(id, deleteBtn);
    }
  }

  // Modal dodawania/edycji rejestru
  function openRegistryModal(registry) {
    editingId = registry ? registry.id : null;
    const isEdit = !!registry;

    const modalOverlay = document.createElement('div');
    modalOverlay.className = 'modal-overlay active';
    modalOverlay.innerHTML = `
      <div class="modal">
        <div class="modal-header">
          <h3>${isEdit ? I18n.t('common.edit') : I18n.t('common.add')}</h3>
          <button class="modal-close" id="reg-modal-close">&times;</button>
        </div>
        <div class="modal-body">
          <div class="form-group">
            <label for="reg-name">${I18n.t('common.name')} *</label>
            <input type="text" id="reg-name" placeholder="${I18n.t('settings.portainer.form.name_placeholder')}" value="${isEdit ? Utils.escapeAttr(registry.name) : ''}">
          </div>
          <div class="form-group">
            <label for="reg-type">${I18n.t('registries.type')}</label>
            <select id="reg-type">
              <option value="custom" ${(!isEdit || registry.registry_type === 'custom') ? 'selected' : ''}>Custom</option>
              <option value="dockerhub" ${(isEdit && registry.registry_type === 'dockerhub') ? 'selected' : ''}>Docker Hub</option>
              <option value="gitlab" ${(isEdit && registry.registry_type === 'gitlab') ? 'selected' : ''}>GitLab</option>
              <option value="github" ${(isEdit && registry.registry_type === 'github') ? 'selected' : ''}>GitHub</option>
              <option value="harbor" ${(isEdit && registry.registry_type === 'harbor') ? 'selected' : ''}>Harbor</option>
            </select>
          </div>
          <div class="form-group">
            <label for="reg-url">${I18n.t('settings.portainer.url')}</label>
            <input type="text" id="reg-url" placeholder="https://registry.example.com" value="${isEdit ? Utils.escapeAttr(registry.url) : ''}">
          </div>
          <div class="form-group">
            <label for="reg-username">${I18n.t('settings.portainer.form.username')} (${I18n.t('common.optional').toLowerCase()})</label>
            <input type="text" id="reg-username" placeholder="username" value="${isEdit ? Utils.escapeAttr(registry.username || '') : ''}">
          </div>
          <div class="form-group">
            <label for="reg-password">${I18n.t('settings.portainer.form.password')} (${I18n.t('common.optional').toLowerCase()})</label>
            <input type="password" id="reg-password" placeholder="${isEdit ? '*** (' + I18n.t('apikeys.close') + ')' : '********'}">
          </div>
          <div id="reg-form-error" class="form-error" hidden></div>
        </div>
        <div class="modal-footer">
          <button class="btn btn-secondary" id="reg-modal-cancel">${I18n.t('common.cancel')}</button>
          <button class="btn btn-primary" id="reg-modal-save">${isEdit ? I18n.t('common.save') : I18n.t('common.add')}</button>
        </div>
      </div>
    `;

    document.body.appendChild(modalOverlay);
    activeModal = modalOverlay;

    const closeModal = () => {
      if (modalOverlay.parentNode) modalOverlay.parentNode.removeChild(modalOverlay);
      if (activeModal === modalOverlay) activeModal = null;
      editingId = null;
    };

    modalOverlay.querySelector('#reg-modal-close').addEventListener('click', closeModal);
    modalOverlay.querySelector('#reg-modal-cancel').addEventListener('click', closeModal);
    modalOverlay.addEventListener('click', (e) => {
      if (e.target === modalOverlay) closeModal();
    });

    const saveBtn = modalOverlay.querySelector('#reg-modal-save');
    saveBtn.addEventListener('click', async () => {
      const data = {
        name: modalOverlay.querySelector('#reg-name').value.trim(),
        registry_type: modalOverlay.querySelector('#reg-type').value,
        url: modalOverlay.querySelector('#reg-url').value.trim(),
        username: modalOverlay.querySelector('#reg-username').value.trim(),
        password: modalOverlay.querySelector('#reg-password').value,
      };

      const errorEl = modalOverlay.querySelector('#reg-form-error');

      if (!data.name || !data.url) {
        if (errorEl) {
          errorEl.textContent = I18n.t('apikeys.key_name_required') + ' & URL';
          errorEl.hidden = false;
        }
        return;
      }

      saveBtn.disabled = true;
      try {
        if (editingId) {
          await ApiClient.put('/api/registries/' + editingId, data);
          App.showToast(I18n.t('registries.updated'), 'success');
        } else {
          await ApiClient.post('/api/registries', data);
          App.showToast(I18n.t('registries.created'), 'success');
        }
        closeModal();
        loadRegistries();
      } catch (e) {
        if (errorEl) {
          errorEl.textContent = e.message || I18n.t('common.error');
          errorEl.hidden = false;
        }
      } finally {
        saveBtn.disabled = false;
      }
    });
  }

  // Test polaczenia z rejestrem
  async function testRegistry(id, btn) {
    const originalText = btn.innerHTML;
    btn.innerHTML = '...';
    btn.disabled = true;

    try {
      const result = await ApiClient.post('/api/registries/' + id + '/test', {});
      if (result.connected) {
        App.showToast(I18n.t('registries.test_ok').replace('{status}', result.registry_status), 'success');
      } else {
        App.showToast(I18n.t('registries.test_error').replace('{error}', result.error || I18n.t('common.unknown')), 'error');
      }
    } catch (e) {
      App.showToast(I18n.t('registries.test_error').replace('{error}', e.message), 'error');
    } finally {
      btn.innerHTML = originalText;
      btn.disabled = false;
    }
  }

  // Usuwanie rejestru
  async function deleteRegistry(id, btn) {
    if (!confirm(I18n.t('settings.portainer.delete_confirm'))) return;

    if (btn) btn.disabled = true;
    try {
      await ApiClient.delete('/api/registries/' + id);
      App.showToast(I18n.t('registries.deleted'), 'success');
      loadRegistries();
    } catch (e) {
      App.showToast(`${I18n.t('common.error')}: ${e.message}`, 'error');
    } finally {
      if (btn) btn.disabled = false;
    }
  }

  return { render, mount, unmount };
})();

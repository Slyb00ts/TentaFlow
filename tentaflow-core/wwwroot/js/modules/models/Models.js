// =============================================================================
// Plik: modules/models/Models.js
// Opis: Widok zarzadzania modelami i aliasami - dwie sekcje CRUD,
//       badge typow uslug i polaczen, edycja inline aliasow.
// Przyklad: ViewRouter.register('models', Models);
// =============================================================================

const Models = (() => {
  'use strict';

  let modelsList = [];
  let poolInfo = [];
  let aliasesList = [];
  let unifiedModels = [];
  let editingAliasId = null;
  let abortController = null;
  let activeAliasModal = null;
  let cachedServices = [];
  let activeTab = 'registry';

  // Mapy badge'ow - zaalokowane raz na poziomie modulu
  const serviceTypeBadgeMap = {
    llm: { cls: 'service-type-llm', label: 'LLM' },
    embedding: { cls: 'service-type-embedding', label: 'Embedding' },
    stt: { cls: 'service-type-stt', label: 'STT' },
    tts: { cls: 'service-type-tts', label: 'TTS' },
    rag: { cls: 'service-type-rag', label: 'RAG' },
    memory: { cls: 'service-type-memory', label: 'Memory' },
    reranker: { cls: 'service-type-reranker', label: 'Reranker' },
  };

  // Renderowanie HTML widoku
  function render() {
    return `
      <div class="model-section">
        <div style="display:flex;gap:var(--spacing-sm);margin-bottom:var(--spacing-md);">
          <button class="btn btn-sm ${activeTab === 'registry' ? 'btn-primary' : 'btn-secondary'}" id="models-tab-registry">${I18n.t('models.registry')}</button>
          <button class="btn btn-sm ${activeTab === 'unified' ? 'btn-primary' : 'btn-secondary'}" id="models-tab-unified">${I18n.t('models.unified')}</button>
        </div>
      </div>

      <div id="models-unified-section" class="model-section" ${activeTab !== 'unified' ? 'hidden' : ''}>
        <div class="card">
          <div class="card-header model-section-header">
            <h3>${I18n.t('models.unified_title')}</h3>
          </div>
          <div class="card-body no-padding">
            <div class="table-wrapper">
              <table>
                <thead>
                  <tr>
                    <th>${I18n.t('common.name')}</th>
                    <th>${I18n.t('common.type')}</th>
                    <th>${I18n.t('common.strategy')}</th>
                    <th>${I18n.t('models.nodes')}</th>
                  </tr>
                </thead>
                <tbody id="unified-models-tbody">
                  <tr><td colspan="4"><div class="empty-state"><div class="empty-state-text">${I18n.t('common.loading')}</div></div></td></tr>
                </tbody>
              </table>
            </div>
          </div>
        </div>
      </div>

      <div id="models-registry-section" class="model-section" ${activeTab !== 'registry' ? 'hidden' : ''}>
        <div class="card">
          <div class="card-header model-section-header">
            <h3 data-i18n="models.registry">${I18n.t('models.registry')}</h3>
            <button class="btn btn-primary btn-sm" id="btn-add-model" data-i18n="common.add">+ ${I18n.t('common.add')}</button>
          </div>
          <div class="card-body no-padding">
            <div class="table-wrapper">
              <table>
                <thead>
                  <tr>
                    <th data-i18n="common.name">${I18n.t('common.name')}</th>
                    <th data-i18n="models.form.display_name">${I18n.t('models.form.display_name')}</th>
                    <th data-i18n="models.service_type">${I18n.t('models.service_type')}</th>
                    <th data-i18n="common.strategy">${I18n.t('common.strategy')}</th>
                    <th data-i18n="nav.services">${I18n.t('nav.services')}</th>
                    <th data-i18n="playground.flow">${I18n.t('playground.flow')}</th>
                    <th>${I18n.t('models.public')}</th>
                    <th data-i18n="common.active">${I18n.t('common.active')}</th>
                    <th data-i18n="common.actions">${I18n.t('common.actions')}</th>
                  </tr>
                </thead>
                <tbody id="models-list-tbody">
                  <tr>
                    <td colspan="9">
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
      </div>

      <div class="model-section">
        <div class="card">
          <div class="card-header model-section-header">
            <h3 data-i18n="models.aliases">${I18n.t('models.aliases')}</h3>
            <button class="btn btn-primary btn-sm" id="btn-add-alias" data-i18n="common.add">+ ${I18n.t('common.add')}</button>
          </div>
          <div class="card-body no-padding">
            <div class="table-wrapper">
              <table>
                <thead>
                  <tr>
                    <th>${I18n.t('models.alias')}</th>
                    <th data-i18n="models.target_model">${I18n.t('models.target_model')}</th>
                    <th>${I18n.t('models.fallback_targets')}</th>
                    <th>${I18n.t('common.strategy')}</th>
                    <th data-i18n="common.active">${I18n.t('common.active')}</th>
                    <th data-i18n="common.actions">${I18n.t('common.actions')}</th>
                  </tr>
                </thead>
                <tbody id="aliases-list-tbody">
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
      </div>
    `;
  }

  // Montowanie - zaladuj dane, podepnij zdarzenia
  function mount() {
    abortController = new AbortController();
    const signal = abortController.signal;

    loadModels();
    loadPoolInfo();
    loadAliases();
    loadUnifiedModels();

    // Zakladki Registry / Unified
    const tabRegistry = document.getElementById('models-tab-registry');
    const tabUnified = document.getElementById('models-tab-unified');
    if (tabRegistry) {
      tabRegistry.addEventListener('click', () => switchTab('registry'), { signal });
    }
    if (tabUnified) {
      tabUnified.addEventListener('click', () => switchTab('unified'), { signal });
    }

    const addModelBtn = document.getElementById('btn-add-model');
    if (addModelBtn) {
      addModelBtn.addEventListener('click', () => ModelForm.open(null, onModelSaved), { signal });
    }

    const addAliasBtn = document.getElementById('btn-add-alias');
    if (addAliasBtn) {
      addAliasBtn.addEventListener('click', openAliasModal, { signal });
    }

    // Delegacja zdarzen na tbody modeli
    const modelsTbody = document.getElementById('models-list-tbody');
    if (modelsTbody) {
      modelsTbody.addEventListener('click', handleModelsTableClick, { signal });
    }

    // Delegacja zdarzen na tbody aliasow
    const aliasesTbody = document.getElementById('aliases-list-tbody');
    if (aliasesTbody) {
      aliasesTbody.addEventListener('click', handleAliasesTableClick, { signal });
    }
  }

  // Odmontowanie
  function unmount() {
    if (abortController) {
      abortController.abort();
      abortController = null;
    }
    modelsList = [];
    aliasesList = [];
    unifiedModels = [];
    editingAliasId = null;
    activeTab = 'registry';
    // Usun modal aliasu jesli jest otwarty
    if (activeAliasModal && activeAliasModal.parentNode) {
      activeAliasModal.remove();
      activeAliasModal = null;
    }
    // Usun modal modelu jesli jest otwarty
    ModelForm.close();
  }

  // Przelaczanie zakladek Registry / Unified
  function switchTab(tab) {
    activeTab = tab;
    const registrySection = document.getElementById('models-registry-section');
    const unifiedSection = document.getElementById('models-unified-section');
    const tabRegistry = document.getElementById('models-tab-registry');
    const tabUnified = document.getElementById('models-tab-unified');

    if (registrySection) registrySection.hidden = tab !== 'registry';
    if (unifiedSection) unifiedSection.hidden = tab !== 'unified';
    if (tabRegistry) {
      tabRegistry.className = `btn btn-sm ${tab === 'registry' ? 'btn-primary' : 'btn-secondary'}`;
    }
    if (tabUnified) {
      tabUnified.className = `btn btn-sm ${tab === 'unified' ? 'btn-primary' : 'btn-secondary'}`;
    }
  }

  // Zaladowanie unified models z API
  async function loadUnifiedModels() {
    try {
      unifiedModels = await ApiClient.get('/api/models/unified');
      if (!Array.isArray(unifiedModels)) unifiedModels = [];
      renderUnifiedTable();
    } catch (err) {
      console.error('Blad ladowania unified models:', err);
      unifiedModels = [];
      renderUnifiedTable();
    }
  }

  // Renderowanie tabeli unified models
  function renderUnifiedTable() {
    const tbody = document.getElementById('unified-models-tbody');
    if (!tbody) return;

    if (unifiedModels.length === 0) {
      tbody.innerHTML = `<tr><td colspan="4"><div class="empty-state"><div class="empty-state-text">${I18n.t('common.no_data')}</div></div></td></tr>`;
      return;
    }

    tbody.innerHTML = unifiedModels.map(m => {
      const instances = m.instances || [];
      const nodeBadges = instances.map(inst => {
        const nodeName = inst.node_name || inst.node_id || '-';
        const statusClass = inst.status === 'running' ? 'badge-success' : 'badge-secondary';
        return `<span class="badge ${statusClass}" style="margin:2px;">${Utils.escapeHtml(nodeName)}</span>`;
      }).join('');

      const serviceTypeBadge = getServiceTypeBadge(m.service_type);
      const strategy = m.strategy || '-';

      return `
        <tr>
          <td><strong>${Utils.escapeHtml(m.model_name)}</strong></td>
          <td>${serviceTypeBadge}</td>
          <td><span class="badge connection-type-badge">${Utils.escapeHtml(strategy)}</span></td>
          <td>${nodeBadges || '-'}</td>
        </tr>
      `;
    }).join('');
  }

  // Zaladowanie modeli z API
  async function loadModels() {
    try {
      modelsList = await ApiClient.get('/api/models');
      renderModelsTable();
    } catch (err) {
      console.error('Blad ladowania modeli:', err);
      modelsList = [];
      renderModelsTable();
    }
  }

  // Zaladowanie pool info z API
  async function loadPoolInfo() {
    try {
      const data = await ApiClient.get('/api/models/pool');
      poolInfo = data?.models || [];
      renderModelsTable();
    } catch (err) {
      console.error('Blad ladowania pool info:', err);
      poolInfo = [];
    }
  }

  // Zaladowanie aliasow z API
  async function loadAliases() {
    try {
      aliasesList = await ApiClient.get('/api/model-aliases');
      cachedServices = await ApiClient.get('/api/services');
      renderAliasesTable();
    } catch (err) {
      console.error('Blad ladowania aliasow:', err);
      aliasesList = [];
      renderAliasesTable();
    }
  }

  // Renderowanie tabeli modeli
  function renderModelsTable() {
    const tbody = document.getElementById('models-list-tbody');
    if (!tbody) return;

    if (modelsList.length === 0) {
      tbody.innerHTML = `
        <tr>
          <td colspan="9">
            <div class="empty-state">
              <div class="empty-state-icon">&#9881;</div>
              <div class="empty-state-text" data-i18n="models.empty_models">${I18n.t('models.empty_models')}</div>
              <div class="empty-state-hint" data-i18n="models.empty_models_hint">${I18n.t('models.empty_models_hint')}</div>
            </div>
          </td>
        </tr>
      `;
      return;
    }

    tbody.innerHTML = modelsList.map(m => {
      const serviceTypeBadge = getServiceTypeBadge(m.service_type);
      const pool = poolInfo.find(p => p.model_name === m.model_name);
      const strategyLabel = pool ? pool.strategy : '-';
      const strategyBadge = strategyLabel !== '-'
        ? `<span class="badge connection-type-badge">${strategyLabel}</span>`
        : '-';
      const svcCount = pool ? pool.services.length : 0;
      const svcBadge = svcCount > 0
        ? `<span class="badge badge-info">${svcCount}</span>`
        : '<span class="badge badge-secondary">0</span>';
      const publicBadge = m.is_public
        ? '<span class="badge badge-success">Tak</span>'
        : '<span class="badge badge-error">Nie</span>';
      const activeBadge = m.is_active
        ? `<span class="badge badge-success"><span class="status-dot status-dot-green"></span>${I18n.t('common.active')}</span>`
        : `<span class="badge badge-error"><span class="status-dot status-dot-red"></span>${I18n.t('common.inactive')}</span>`;

      return `
        <tr>
          <td><strong>${Utils.escapeHtml(m.model_name)}</strong></td>
          <td>${Utils.escapeHtml(m.display_name || '-')}</td>
          <td>${serviceTypeBadge}</td>
          <td>${strategyBadge}</td>
          <td>${svcBadge}</td>
          <td>${Utils.escapeHtml(m.flow_id || '-')}</td>
          <td>${publicBadge}</td>
          <td>${activeBadge}</td>
          <td>
            <div style="display: flex; gap: 4px;">
              <button class="btn btn-ghost btn-sm" data-edit-model="${Utils.escapeAttr(String(m.id))}" title="${I18n.t('common.edit')}" data-i18n-title="common.edit">&#9998;</button>
              <button class="btn btn-ghost btn-sm" data-delete-model="${Utils.escapeAttr(String(m.id))}" title="${I18n.t('common.delete')}" data-i18n-title="common.delete">&#10005;</button>
            </div>
          </td>
        </tr>
      `;
    }).join('');

  }

  // Delegowany handler klikniec w tabeli modeli
  function handleModelsTableClick(e) {
    const editBtn = e.target.closest('[data-edit-model]');
    if (editBtn) {
      const id = parseInt(editBtn.dataset.editModel, 10);
      const model = modelsList.find(m => m.id === id);
      if (model) ModelForm.open(model, onModelSaved);
      return;
    }

    const deleteBtn = e.target.closest('[data-delete-model]');
    if (deleteBtn) {
      const id = parseInt(deleteBtn.dataset.deleteModel, 10);
      const model = modelsList.find(m => m.id === id);
      if (model) confirmDeleteModel(model, deleteBtn);
    }
  }

  // Renderowanie tabeli aliasow
  function renderAliasesTable() {
    const tbody = document.getElementById('aliases-list-tbody');
    if (!tbody) return;

    if (aliasesList.length === 0) {
      tbody.innerHTML = `
        <tr>
          <td colspan="6">
            <div class="empty-state">
              <div class="empty-state-icon">&#128279;</div>
              <div class="empty-state-text" data-i18n="models.empty_aliases">${I18n.t('models.empty_aliases')}</div>
              <div class="empty-state-hint" data-i18n="models.empty_aliases_hint">${I18n.t('models.empty_aliases_hint')}</div>
            </div>
          </td>
        </tr>
      `;
      return;
    }

    tbody.innerHTML = aliasesList.map(a => {
      const isEditing = editingAliasId === a.id;
      const activeBadge = a.is_active
        ? `<span class="badge badge-success"><span class="status-dot status-dot-green"></span>${I18n.t('common.active')}</span>`
        : `<span class="badge badge-error"><span class="status-dot status-dot-red"></span>${I18n.t('common.inactive')}</span>`;

      // Fallback targets
      const fallbacks = a.fallback_targets || [];
      const fallbackBadges = fallbacks.map(f =>
        `<span class="badge badge-info" style="margin:1px;">${Utils.escapeHtml(f)}</span>`
      ).join('') || '-';
      const aliasStrategy = a.strategy || '-';
      const stratBadge = aliasStrategy !== '-'
        ? `<span class="badge connection-type-badge">${Utils.escapeHtml(aliasStrategy)}</span>`
        : '-';

      if (isEditing) {
        return `
          <tr>
            <td>
              <input type="text" id="alias-edit-name-${a.id}" class="alias-inline-edit"
                value="${Utils.escapeAttr(a.alias || '')}">
            </td>
            <td>
              <select id="alias-edit-target-${a.id}" class="alias-inline-edit">
                ${cachedServices.map(s => {
                  const selected = s.name === (a.target_model || '') ? 'selected' : '';
                  return `<option value="${Utils.escapeAttr(s.name)}" ${selected}>${Utils.escapeHtml(s.name)} (${s.service_type})</option>`;
                }).join('')}
              </select>
            </td>
            <td>${fallbackBadges}</td>
            <td>${stratBadge}</td>
            <td>${activeBadge}</td>
            <td>
              <div style="display: flex; gap: 4px;">
                <button class="btn btn-primary btn-sm" data-save-alias="${a.id}" data-i18n="common.save">${I18n.t('common.save')}</button>
                <button class="btn btn-ghost btn-sm" data-cancel-alias="${a.id}" data-i18n="common.cancel">${I18n.t('common.cancel')}</button>
              </div>
            </td>
          </tr>
        `;
      }

      return `
        <tr>
          <td><strong>${Utils.escapeHtml(a.alias)}</strong></td>
          <td>${Utils.escapeHtml(a.target_model)}</td>
          <td>${fallbackBadges}</td>
          <td>${stratBadge}</td>
          <td>${activeBadge}</td>
          <td>
            <div style="display: flex; gap: 4px;">
              <button class="btn btn-ghost btn-sm" data-edit-alias="${a.id}" title="${I18n.t('common.edit')}" data-i18n-title="common.edit">&#9998;</button>
              <button class="btn btn-ghost btn-sm" data-delete-alias="${a.id}" title="${I18n.t('common.delete')}" data-i18n-title="common.delete">&#10005;</button>
            </div>
          </td>
        </tr>
      `;
    }).join('');

  }

  // Delegowany handler klikniec w tabeli aliasow
  function handleAliasesTableClick(e) {
    const editBtn = e.target.closest('[data-edit-alias]');
    if (editBtn) {
      editingAliasId = parseInt(editBtn.dataset.editAlias, 10);
      renderAliasesTable();
      return;
    }

    const cancelBtn = e.target.closest('[data-cancel-alias]');
    if (cancelBtn) {
      editingAliasId = null;
      renderAliasesTable();
      return;
    }

    const saveBtn = e.target.closest('[data-save-alias]');
    if (saveBtn) {
      const id = parseInt(saveBtn.dataset.saveAlias, 10);
      const nameInput = document.getElementById(`alias-edit-name-${id}`);
      const targetInput = document.getElementById(`alias-edit-target-${id}`);
      if (nameInput && targetInput) {
        saveAliasInline(id, nameInput.value.trim(), targetInput.value.trim(), saveBtn);
      }
      return;
    }

    const deleteBtn = e.target.closest('[data-delete-alias]');
    if (deleteBtn) {
      const id = parseInt(deleteBtn.dataset.deleteAlias, 10);
      const alias = aliasesList.find(a => a.id === id);
      if (alias) confirmDeleteAlias(alias, deleteBtn);
    }
  }

  // Modal dodawania aliasu
  async function openAliasModal() {
    const services = await ApiClient.get('/api/services');
    const serviceOptions = services.map(s => `<option value="${Utils.escapeAttr(s.name)}">${Utils.escapeHtml(s.name)} (${s.service_type})</option>`).join('');

    const modalOverlay = document.createElement('div');
    modalOverlay.className = 'modal-overlay active';
    modalOverlay.innerHTML = `
      <div class="modal">
        <div class="modal-header">
          <h3 data-i18n="models.new_alias">${I18n.t('models.new_alias')}</h3>
          <button class="modal-close" id="alias-modal-close">&times;</button>
        </div>
        <div class="modal-body">
          <div class="form-group">
            <label for="new-alias-name">${I18n.t('models.alias')}</label>
            <input type="text" id="new-alias-name" placeholder="np. gpt-4">
          </div>
          <div class="form-group">
            <label for="new-alias-target" data-i18n="models.target_model">${I18n.t('models.target_model')}</label>
            <select id="new-alias-target">
              <option value="">-- ${I18n.t('models.select_target')} --</option>
              ${serviceOptions}
            </select>
          </div>
          <div class="form-group">
            <label for="new-alias-fallbacks">${I18n.t('models.fallback_targets')}</label>
            <textarea id="new-alias-fallbacks" rows="2" placeholder="model1, model2, model3" style="font-size:var(--font-size-sm);"></textarea>
            <div class="form-hint">${I18n.t('models.fallback_hint')}</div>
          </div>
          <div class="form-group">
            <label for="new-alias-strategy">${I18n.t('common.strategy')}</label>
            <select id="new-alias-strategy">
              <option value="first_available">first_available</option>
              <option value="round_robin">round_robin</option>
              <option value="least_loaded">least_loaded</option>
            </select>
          </div>
          <div id="alias-form-error" class="form-error" hidden></div>
        </div>
        <div class="modal-footer">
          <button class="btn btn-secondary" id="alias-modal-cancel" data-i18n="common.cancel">${I18n.t('common.cancel')}</button>
          <button class="btn btn-primary" id="alias-modal-save" data-i18n="common.add">${I18n.t('common.add')}</button>
        </div>
      </div>
    `;

    document.body.appendChild(modalOverlay);
    activeAliasModal = modalOverlay;

    const closeModal = () => {
      if (modalOverlay.parentNode) modalOverlay.parentNode.removeChild(modalOverlay);
      if (activeAliasModal === modalOverlay) activeAliasModal = null;
    };

    modalOverlay.querySelector('#alias-modal-close').addEventListener('click', closeModal);
    modalOverlay.querySelector('#alias-modal-cancel').addEventListener('click', closeModal);
    modalOverlay.addEventListener('click', (e) => {
      if (e.target === modalOverlay) closeModal();
    });

    const saveBtn = modalOverlay.querySelector('#alias-modal-save');
    saveBtn.addEventListener('click', async () => {
      const aliasName = modalOverlay.querySelector('#new-alias-name').value.trim();
      const targetModel = modalOverlay.querySelector('#new-alias-target').value.trim();
      const fallbacksRaw = modalOverlay.querySelector('#new-alias-fallbacks').value.trim();
      const aliasStrategy = modalOverlay.querySelector('#new-alias-strategy').value;
      const errorEl = modalOverlay.querySelector('#alias-form-error');

      if (!aliasName || !targetModel) {
        if (errorEl) {
          errorEl.textContent = I18n.t('models.fields_required');
          errorEl.hidden = false;
        }
        return;
      }

      // Parsowanie fallback targets
      const fallbackTargets = fallbacksRaw
        ? fallbacksRaw.split(',').map(s => s.trim()).filter(Boolean)
        : [];

      saveBtn.disabled = true;
      try {
        await ApiClient.post('/api/model-aliases', {
          alias: aliasName,
          target_model: targetModel,
          fallback_targets: fallbackTargets,
          strategy: aliasStrategy,
        });
        App.showToast(I18n.t('models.alias_created').replace('{name}', aliasName), 'success');
        closeModal();
        loadAliases();
      } catch (err) {
        if (errorEl) {
          errorEl.textContent = err.message || I18n.t('common.error');
          errorEl.hidden = false;
        }
      } finally {
        saveBtn.disabled = false;
      }
    });
  }

  // Zapis aliasu inline
  async function saveAliasInline(id, aliasName, targetModel, btn) {
    if (!aliasName || !targetModel) {
      App.showToast(I18n.t('models.fields_required'), 'error');
      return;
    }

    if (btn) btn.disabled = true;
    try {
      await ApiClient.put(`/api/model-aliases/${id}`, {
        alias: aliasName,
        target_model: targetModel,
      });
      editingAliasId = null;
      App.showToast(I18n.t('models.alias_updated').replace('{name}', aliasName), 'success');
      loadAliases();
    } catch (err) {
      App.showToast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    } finally {
      if (btn) btn.disabled = false;
    }
  }

  // Potwierdzenie usuwania modelu
  async function confirmDeleteModel(model, btn) {
    if (!confirm(I18n.t('services.delete_confirm').replace('{name}', model.model_name))) return;

    if (btn) btn.disabled = true;
    try {
      await ApiClient.delete(`/api/models/${model.id}`);
      App.showToast(I18n.t('models.model_deleted').replace('{name}', model.model_name), 'success');
      loadModels();
    } catch (err) {
      App.showToast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    } finally {
      if (btn) btn.disabled = false;
    }
  }

  // Potwierdzenie usuwania aliasu
  async function confirmDeleteAlias(alias, btn) {
    if (!confirm(I18n.t('services.delete_confirm').replace('{name}', alias.alias))) return;

    if (btn) btn.disabled = true;
    try {
      await ApiClient.delete(`/api/model-aliases/${alias.id}`);
      App.showToast(I18n.t('models.alias_deleted').replace('{name}', alias.alias), 'success');
      loadAliases();
    } catch (err) {
      App.showToast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    } finally {
      if (btn) btn.disabled = false;
    }
  }

  // Callback po zapisie modelu
  function onModelSaved() {
    loadModels();
    loadPoolInfo();
  }

  // Badge typu uslugi
  function getServiceTypeBadge(type) {
    const t = serviceTypeBadgeMap[type] || { cls: '', label: type || '-' };
    return `<span class="badge service-type-badge ${t.cls}">${t.label}</span>`;
  }

  return { render, mount, unmount };
})();

// =============================================================================
// Plik: modules/models/Models.js
// Opis: Widok zarzadzania modelami i aliasami - dwie sekcje CRUD,
//       badge typow uslug i polaczen, edycja aliasow przez modal.
// Przyklad: ViewRouter.register('models', Models);
// =============================================================================

const Models = (() => {
  'use strict';

  let allModels = [];
  let aliasesList = [];
  let abortController = null;
  let activeAliasModal = null;
  let cachedServices = [];

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
        <div class="card">
          <div class="card-header model-section-header">
            <h3>${I18n.t('models.title', 'Models')}</h3>
          </div>
          <div class="card-body no-padding">
            <div class="table-wrapper">
              <table>
                <thead>
                  <tr>
                    <th>${I18n.t('common.name')}</th>
                    <th>${I18n.t('common.type')}</th>
                    <th>${I18n.t('common.status')}</th>
                    <th>${I18n.t('models.nodes', 'Nodes')}</th>
                  </tr>
                </thead>
                <tbody id="models-list-tbody">
                  <tr><td colspan="4"><div class="empty-state"><div class="empty-state-text">${I18n.t('common.loading')}</div></div></td></tr>
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

    loadAllModels();
    loadAliases();

    const addAliasBtn = document.getElementById('btn-add-alias');
    if (addAliasBtn) {
      addAliasBtn.addEventListener('click', () => openAliasModal(null), { signal });
    }

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
    allModels = [];
    aliasesList = [];
    if (activeAliasModal && activeAliasModal.parentNode) {
      activeAliasModal.remove();
      activeAliasModal = null;
    }
  }

  // Zaladowanie modeli z serwisow (pool info + unified/mesh)
  async function loadAllModels() {
    try {
      const [poolData, unified] = await Promise.all([
        ApiClient.get('/api/models/pool').catch(() => null),
        ApiClient.get('/api/models/unified').catch(() => []),
      ]);

      const poolModels = poolData?.models || [];
      const unifiedList = Array.isArray(unified) ? unified : [];

      // Polacz pool info (lokalne) z unified (mesh) — deduplikacja po nazwie
      const modelMap = new Map();

      for (const m of poolModels) {
        modelMap.set(m.model_name, {
          name: m.model_name,
          service_type: m.service_type || 'llm',
          status: 'running',
          nodes: [{ name: 'local', status: 'running' }],
        });
      }

      for (const m of unifiedList) {
        const existing = modelMap.get(m.model_name);
        if (existing) {
          const remoteNodes = (m.instances || []).map(i => ({
            name: i.node_name || i.node_id || '-',
            status: i.status || 'running',
          }));
          existing.nodes = existing.nodes.concat(remoteNodes);
        } else {
          modelMap.set(m.model_name, {
            name: m.model_name,
            service_type: m.service_type || 'llm',
            status: (m.instances || []).some(i => i.status === 'running') ? 'running' : 'offline',
            nodes: (m.instances || []).map(i => ({
              name: i.node_name || i.node_id || '-',
              status: i.status || 'running',
            })),
          });
        }
      }

      allModels = Array.from(modelMap.values());
      renderModelsTable();
    } catch (err) {
      console.error('Blad ladowania modeli:', err);
      allModels = [];
      renderModelsTable();
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

  // Renderowanie tabeli modeli (z serwisow + mesh)
  function renderModelsTable() {
    const tbody = document.getElementById('models-list-tbody');
    if (!tbody) return;

    if (allModels.length === 0) {
      tbody.innerHTML = `
        <tr>
          <td colspan="4">
            <div class="empty-state">
              <div class="empty-state-icon">&#9881;</div>
              <div class="empty-state-text">${I18n.t('models.empty', 'Brak uruchomionych modeli')}</div>
              <div class="empty-state-hint">${I18n.t('models.empty_hint', 'Modele pojawia sie automatycznie po uruchomieniu serwisow')}</div>
            </div>
          </td>
        </tr>
      `;
      return;
    }

    tbody.innerHTML = allModels.map(m => {
      const serviceTypeBadge = getServiceTypeBadge(m.service_type);
      const statusBadge = m.status === 'running'
        ? `<span class="badge badge-success"><span class="status-dot status-dot-green"></span>${I18n.t('common.active')}</span>`
        : `<span class="badge badge-secondary"><span class="status-dot status-dot-red"></span>${I18n.t('common.inactive')}</span>`;
      const nodeBadges = (m.nodes || []).map(n => {
        const cls = n.status === 'running' ? 'badge-success' : 'badge-secondary';
        return `<span class="badge ${cls}" style="margin:2px">${Utils.escapeHtml(n.name)}</span>`;
      }).join('') || '-';

      return `
        <tr>
          <td><strong>${Utils.escapeHtml(m.name)}</strong></td>
          <td>${serviceTypeBadge}</td>
          <td>${statusBadge}</td>
          <td>${nodeBadges}</td>
        </tr>
      `;
    }).join('');
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
      const activeBadge = a.is_active
        ? `<span class="badge badge-success"><span class="status-dot status-dot-green"></span>${I18n.t('common.active')}</span>`
        : `<span class="badge badge-error"><span class="status-dot status-dot-red"></span>${I18n.t('common.inactive')}</span>`;

      // Fallback targets — liczba
      const fallbacks = a.fallback_targets || [];
      const fallbackCount = fallbacks.length > 0
        ? `<span class="badge badge-info" title="${fallbacks.map(f => Utils.escapeAttr(f)).join(', ')}">${fallbacks.length}</span>`
        : '-';
      // Strategia z i18n
      const aliasStrategy = a.strategy || '-';
      const stratLabel = aliasStrategy !== '-'
        ? (I18n.t('models.strategy_' + aliasStrategy) || aliasStrategy)
        : '-';
      const stratBadge = aliasStrategy !== '-'
        ? `<span class="badge connection-type-badge">${Utils.escapeHtml(stratLabel)}</span>`
        : '-';

      return `
        <tr>
          <td><strong>${Utils.escapeHtml(a.alias)}</strong></td>
          <td>${Utils.escapeHtml(a.target_model)}</td>
          <td>${fallbackCount}</td>
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
      const id = parseInt(editBtn.dataset.editAlias, 10);
      const alias = aliasesList.find(a => a.id === id);
      if (alias) openAliasModal(alias);
      return;
    }

    const deleteBtn = e.target.closest('[data-delete-alias]');
    if (deleteBtn) {
      const id = parseInt(deleteBtn.dataset.deleteAlias, 10);
      const alias = aliasesList.find(a => a.id === id);
      if (alias) confirmDeleteAlias(alias, deleteBtn);
    }
  }

  // Modal dodawania/edycji aliasu
  async function openAliasModal(existingAlias = null) {
    const services = await ApiClient.get('/api/services');
    const serviceOptions = services.map(s => `<option value="${Utils.escapeAttr(s.name)}">${Utils.escapeHtml(s.name)} (${s.service_type})</option>`).join('');

    const isEdit = existingAlias !== null;
    const modalTitle = isEdit ? (I18n.t('models.edit_alias') || 'Edit Alias') : I18n.t('models.new_alias');
    const saveBtnLabel = isEdit ? I18n.t('common.save') : I18n.t('common.add');

    // Stan wybranych fallback targets
    let selectedFallbacks = isEdit ? [...(existingAlias.fallback_targets || [])] : [];

    const modalOverlay = document.createElement('div');
    modalOverlay.className = 'modal-overlay active';
    modalOverlay.innerHTML = `
      <div class="modal">
        <div class="modal-header">
          <h3>${Utils.escapeHtml(modalTitle)}</h3>
          <button class="modal-close" id="alias-modal-close">&times;</button>
        </div>
        <div class="modal-body">
          <div class="form-group">
            <label for="new-alias-name">${I18n.t('models.alias')}</label>
            <input type="text" id="new-alias-name" placeholder="np. gpt-4" value="${isEdit ? Utils.escapeAttr(existingAlias.alias || '') : ''}">
          </div>
          <div class="form-group">
            <label for="new-alias-target" data-i18n="models.target_model">${I18n.t('models.target_model')}</label>
            <select id="new-alias-target">
              <option value="">-- ${I18n.t('models.select_target')} --</option>
              ${serviceOptions}
            </select>
          </div>
          <div class="form-group">
            <label for="new-alias-strategy">${I18n.t('common.strategy')}</label>
            <select id="new-alias-strategy">
              <option value="first_available">${I18n.t('models.strategy_first_available')}</option>
              <option value="round_robin">${I18n.t('models.strategy_round_robin')}</option>
              <option value="least_loaded">${I18n.t('models.strategy_least_loaded')}</option>
            </select>
          </div>
          <div class="form-group">
            <label>${I18n.t('models.fallback_targets')}</label>
            <div id="alias-fallback-selected" style="display:flex;flex-wrap:wrap;gap:4px;margin-bottom:8px;min-height:28px;"></div>
            <select id="alias-fallback-add" class="chat-select">
              <option value="">-- ${I18n.t('models.add_fallback')} --</option>
              ${serviceOptions}
            </select>
            <div class="form-hint">${I18n.t('models.fallback_hint')}</div>
          </div>
          <div id="alias-form-error" class="form-error" hidden></div>
        </div>
        <div class="modal-footer">
          <button class="btn btn-secondary" id="alias-modal-cancel" data-i18n="common.cancel">${I18n.t('common.cancel')}</button>
          <button class="btn btn-primary" id="alias-modal-save">${Utils.escapeHtml(saveBtnLabel)}</button>
        </div>
      </div>
    `;

    document.body.appendChild(modalOverlay);
    activeAliasModal = modalOverlay;

    // Ustaw wartosc target select jesli edycja
    if (isEdit && existingAlias.target_model) {
      const targetSelect = modalOverlay.querySelector('#new-alias-target');
      targetSelect.value = existingAlias.target_model;
    }

    // Ustaw wartosc strategy select jesli edycja
    if (isEdit && existingAlias.strategy) {
      const strategySelect = modalOverlay.querySelector('#new-alias-strategy');
      strategySelect.value = existingAlias.strategy;
    }

    // Renderowanie badge'ow fallback targets
    function renderFallbackBadges() {
      const container = modalOverlay.querySelector('#alias-fallback-selected');
      if (!container) return;
      container.innerHTML = selectedFallbacks.map((name, idx) =>
        `<span class="badge badge-info" style="display:inline-flex;align-items:center;gap:4px;margin:1px;">
          ${Utils.escapeHtml(name)}
          <span data-remove-fallback="${idx}" style="cursor:pointer;font-weight:bold;line-height:1;">&times;</span>
        </span>`
      ).join('');
    }
    renderFallbackBadges();

    // Dodawanie fallback z select
    const fallbackSelect = modalOverlay.querySelector('#alias-fallback-add');
    fallbackSelect.addEventListener('change', () => {
      const val = fallbackSelect.value;
      if (val && !selectedFallbacks.includes(val)) {
        selectedFallbacks.push(val);
        renderFallbackBadges();
      }
      fallbackSelect.value = '';
    });

    // Usuwanie fallback badge przez delegacje
    const fallbackContainer = modalOverlay.querySelector('#alias-fallback-selected');
    fallbackContainer.addEventListener('click', (e) => {
      const removeBtn = e.target.closest('[data-remove-fallback]');
      if (removeBtn) {
        const idx = parseInt(removeBtn.dataset.removeFallback, 10);
        selectedFallbacks.splice(idx, 1);
        renderFallbackBadges();
      }
    });

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
      const aliasStrategy = modalOverlay.querySelector('#new-alias-strategy').value;
      const errorEl = modalOverlay.querySelector('#alias-form-error');

      if (!aliasName || !targetModel) {
        if (errorEl) {
          errorEl.textContent = I18n.t('models.fields_required');
          errorEl.hidden = false;
        }
        return;
      }

      saveBtn.disabled = true;
      try {
        const payload = {
          alias: aliasName,
          target_model: targetModel,
          fallback_targets: selectedFallbacks,
          strategy: aliasStrategy,
        };

        if (isEdit) {
          await ApiClient.put(`/api/model-aliases/${existingAlias.id}`, payload);
          App.showToast(I18n.t('models.alias_updated').replace('{name}', aliasName), 'success');
        } else {
          await ApiClient.post('/api/model-aliases', payload);
          App.showToast(I18n.t('models.alias_created').replace('{name}', aliasName), 'success');
        }
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

  // Badge typu uslugi
  function getServiceTypeBadge(type) {
    const t = serviceTypeBadgeMap[type] || { cls: '', label: type || '-' };
    return `<span class="badge service-type-badge ${t.cls}">${t.label}</span>`;
  }

  return { render, mount, unmount };
})();

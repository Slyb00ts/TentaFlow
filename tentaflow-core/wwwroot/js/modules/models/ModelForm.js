// =============================================================================
// Plik: modules/models/ModelForm.js
// Opis: Modal formularz dodawania/edycji modelu AI z walidacja JSON,
//       dynamicznym ladowaniem serwisow i flow.
// Przyklad: ModelForm.open(null, onSaved); // nowy model
//           ModelForm.open(modelData, onSaved); // edycja
// =============================================================================

const ModelForm = (() => {
  'use strict';

  let overlay = null;
  let currentModel = null;
  let onSaveCallback = null;
  let serviceOptions = [];
  let flowOptions = [];
  let quicStatusMap = {};
  let poolInfo = [];

  // Otwarcie modala
  function open(model, onSaved) {
    currentModel = model;
    onSaveCallback = onSaved;
    loadDependencies().then(() => createModal());
  }

  // Zaladowanie serwisow, flow, statusow QUIC i pool info
  async function loadDependencies() {
    try {
      const [services, flows, statusData, poolData] = await Promise.all([
        ApiClient.get('/api/services').catch(() => []),
        ApiClient.get('/api/flows').catch(() => []),
        ApiClient.get('/api/services/status').catch(() => ({})),
        ApiClient.get('/api/models/pool').catch(() => ({ models: [] })),
      ]);
      serviceOptions = Array.isArray(services) ? services : [];
      flowOptions = Array.isArray(flows) ? flows : [];
      quicStatusMap = statusData || {};
      poolInfo = poolData?.models || [];
    } catch {
      serviceOptions = [];
      flowOptions = [];
      quicStatusMap = {};
      poolInfo = [];
    }
  }

  // Tworzenie modala w DOM
  function createModal() {
    close();

    const isEdit = !!currentModel;
    const title = isEdit ? I18n.t('common.edit') : I18n.t('models.empty_models_hint').replace('Click "Add model" to register a new one', 'New model');

    overlay = document.createElement('div');
    overlay.className = 'modal-overlay active';
    overlay.innerHTML = `
      <div class="modal">
        <div class="modal-header">
          <h3>${title}</h3>
          <button class="modal-close" id="mf-close-btn">&times;</button>
        </div>
        <div class="modal-body">
          <form id="model-form">
            <div class="form-group">
              <label for="mf-name" data-i18n="models.form.name">${I18n.t('models.form.name')}</label>
              <input type="text" id="mf-name" placeholder="np. bielik-11b" required
                value="${Utils.escapeAttr(currentModel?.model_name || '')}">
            </div>

            <div class="form-group">
              <label for="mf-display-name" data-i18n="models.form.display_name">${I18n.t('models.form.display_name')}</label>
              <input type="text" id="mf-display-name" placeholder="np. Bielik 11B"
                value="${Utils.escapeAttr(currentModel?.display_name || '')}">
            </div>

            <div class="form-group">
              <label for="mf-service-type" data-i18n="models.service_type">${I18n.t('models.service_type')}</label>
              <select id="mf-service-type">
                <option value="llm" ${selField('service_type', 'llm')}>LLM</option>
                <option value="embedding" ${selField('service_type', 'embedding')}>Embedding</option>
                <option value="stt" ${selField('service_type', 'stt')}>STT</option>
                <option value="tts" ${selField('service_type', 'tts')}>TTS</option>
                <option value="rag" ${selField('service_type', 'rag')}>RAG</option>
                <option value="memory" ${selField('service_type', 'memory')}>Memory</option>
                <option value="reranker" ${selField('service_type', 'reranker')}>Reranker</option>
              </select>
            </div>

            <div class="form-group">
              <label for="mf-strategy" data-i18n="models.form.strategy">${I18n.t('models.form.strategy')}</label>
              <select id="mf-strategy">
                <option value="round_robin" ${getPoolStrategy() === 'round_robin' ? 'selected' : ''}>Round Robin</option>
                <option value="least_loaded" ${getPoolStrategy() === 'least_loaded' ? 'selected' : ''}>Least Loaded</option>
              </select>
            </div>

            <div class="form-group">
              <label data-i18n="nav.services">${I18n.t('nav.services')}</label>
              <div id="mf-services-list" style="max-height: 200px; overflow-y: auto; border: 1px solid var(--color-border); border-radius: var(--radius-sm); padding: var(--spacing-sm);">
                ${renderServiceCheckboxes()}
              </div>
              <div class="form-hint" data-i18n="models.form.services_hint">${I18n.t('models.form.services_hint')}</div>
            </div>

            <div class="form-group">
              <label for="mf-flow-id" data-i18n="playground.flow">${I18n.t('playground.flow')}</label>
              <select id="mf-flow-id">
                <option value="">-- --</option>
                ${flowOptions.map(f => `
                  <option value="${f.id}" ${currentModel?.flow_id === f.id ? 'selected' : ''}>
                    ${Utils.escapeHtml(f.name || f.id)}
                  </option>
                `).join('')}
              </select>
            </div>

            <div class="form-group">
              <label class="prompt-toggle-label">
                <input type="checkbox" id="mf-public" ${currentModel?.is_public ? 'checked' : ''}>
                <span>Publiczny</span>
              </label>
            </div>

            <div class="form-group">
              <label class="prompt-toggle-label">
                <input type="checkbox" id="mf-active" ${currentModel?.is_active !== 0 ? 'checked' : ''}>
                <span data-i18n="common.active">${I18n.t('common.active')}</span>
              </label>
            </div>

            <div id="mf-form-error" class="form-error" hidden></div>
          </form>
        </div>
        <div class="modal-footer">
          <button class="btn btn-secondary" id="mf-cancel-btn" data-i18n="common.cancel">${I18n.t('common.cancel')}</button>
          <button class="btn btn-primary" id="mf-save-btn">${isEdit ? I18n.t('common.save') : I18n.t('common.add')}</button>
        </div>
      </div>
    `;

    document.body.appendChild(overlay);

    // Zdarzenia
    overlay.querySelector('#mf-close-btn').addEventListener('click', close);
    overlay.querySelector('#mf-cancel-btn').addEventListener('click', close);
    overlay.querySelector('#mf-save-btn').addEventListener('click', handleSave);
    overlay.addEventListener('click', (e) => {
      if (e.target === overlay) close();
    });

    // Aktualizacja listy serwisow po zmianie typu uslugi
    overlay.querySelector('#mf-service-type').addEventListener('change', () => {
      const selectedType = overlay.querySelector('#mf-service-type').value;
      const filtered = serviceOptions.filter(s => s.service_type === selectedType);
      const assignedServices = getPoolServices();
      const container = overlay.querySelector('#mf-services-list');
      if (container) {
        if (filtered.length === 0) {
          container.innerHTML = `<div style="color: var(--color-text-muted); font-size: var(--font-size-sm); padding: var(--spacing-xs);" data-i18n="models.form.no_services">${I18n.t('models.form.no_services')}</div>`;
        } else {
          container.innerHTML = filtered.map(s => {
            const checked = assignedServices.includes(s.name) ? 'checked' : '';
            const raw = quicStatusMap[s.name] || '';
            let dotColor = 'gray';
            if (raw.includes('connected') && !raw.includes('disconnected')) dotColor = 'green';
            else if (raw.includes('connecting')) dotColor = 'yellow';
            else if (raw.includes('disconnected')) dotColor = 'red';
            else if (raw.includes('ready')) dotColor = 'green';
            return `
              <label style="display: flex; align-items: center; gap: 8px; padding: 4px 0; cursor: pointer;">
                <input type="checkbox" class="mf-service-cb" value="${Utils.escapeAttr(s.name)}" ${checked}>
                <span class="status-dot status-dot-${dotColor}"></span>
                <span>${Utils.escapeHtml(s.name)}</span>
              </label>
            `;
          }).join('');
        }
      }
    });
  }

  // Zamkniecie modala
  function close() {
    if (overlay && overlay.parentNode) {
      overlay.parentNode.removeChild(overlay);
    }
    overlay = null;
    currentModel = null;
  }

  // Obsluga zapisu
  async function handleSave() {
    const modelName = overlay.querySelector('#mf-name')?.value.trim();
    const displayName = overlay.querySelector('#mf-display-name')?.value.trim() || null;
    const serviceType = overlay.querySelector('#mf-service-type')?.value;
    const strategy = overlay.querySelector('#mf-strategy')?.value || 'round_robin';
    const flowId = overlay.querySelector('#mf-flow-id')?.value || null;
    const isPublic = overlay.querySelector('#mf-public')?.checked ? 1 : 0;
    const isActive = overlay.querySelector('#mf-active')?.checked ? 1 : 0;

    // Zbierz zaznaczone serwisy
    const serviceCheckboxes = overlay.querySelectorAll('.mf-service-cb:checked');
    const selectedServices = Array.from(serviceCheckboxes).map(cb => cb.value);

    if (!modelName) {
      showFormError(I18n.t('rules.pii.name_required'));
      return;
    }

    const saveBtn = overlay.querySelector('#mf-save-btn');
    if (saveBtn) {
      saveBtn.disabled = true;
      saveBtn.textContent = '...';
    }

    try {
      const payload = {
        model_name: modelName,
        display_name: displayName,
        service_type: serviceType,
        connection_type: 'quic',
        service_id: null,
        flow_id: flowId ? parseInt(flowId, 10) : null,
        is_public: isPublic,
        is_active: isActive,
        config_json: '{}',
      };

      if (currentModel) {
        payload.id = currentModel.id;
        await ApiClient.put(`/api/models/${currentModel.id}`, payload);
      } else {
        await ApiClient.post('/api/models', payload);
      }

      // Ustaw serwisy w model pool
      await ApiClient.put(`/api/models/${encodeURIComponent(modelName)}/services`, {
        services: selectedServices,
      });

      // Ustaw strategie
      await ApiClient.put(`/api/models/${encodeURIComponent(modelName)}/strategy`, {
        strategy: strategy,
      });

      App.showToast(I18n.t('models.form.saved').replace('{name}', modelName), 'success');
      close();
      if (onSaveCallback) onSaveCallback();
    } catch (err) {
      showFormError(err.message || I18n.t('common.error'));
    } finally {
      if (saveBtn) {
        saveBtn.disabled = false;
        saveBtn.textContent = currentModel ? I18n.t('common.save') : I18n.t('common.add');
      }
    }
  }

  // Wyswietlenie bledu w formularzu
  function showFormError(message) {
    const el = overlay?.querySelector('#mf-form-error');
    if (el) {
      el.textContent = message;
      el.hidden = false;
    }
  }

  // Pomocnik selected dla dowolnego pola
  function selField(field, value) {
    return currentModel?.[field] === value ? 'selected' : '';
  }

  // Strategia LB z pool info dla aktualnego modelu
  function getPoolStrategy() {
    if (!currentModel) return 'round_robin';
    const pool = poolInfo.find(p => p.model_name === currentModel.model_name);
    return pool ? pool.strategy : 'round_robin';
  }

  // Lista serwisow przypisanych do modelu z pool info
  function getPoolServices() {
    if (!currentModel) return [];
    const pool = poolInfo.find(p => p.model_name === currentModel.model_name);
    return pool ? pool.services : [];
  }

  // Renderowanie checkboxow serwisow
  function renderServiceCheckboxes() {
    const selectedType = currentModel?.service_type || 'llm';
    const filtered = serviceOptions.filter(s => s.service_type === selectedType);
    const assignedServices = getPoolServices();

    if (filtered.length === 0) {
      return `<div style="color: var(--color-text-muted); font-size: var(--font-size-sm); padding: var(--spacing-xs);" data-i18n="models.form.no_services">${I18n.t('models.form.no_services')}</div>`;
    }

    return filtered.map(s => {
      const checked = assignedServices.includes(s.name) ? 'checked' : '';
      const raw = quicStatusMap[s.name] || '';
      let dotColor = 'gray';
      if (raw.includes('connected') && !raw.includes('disconnected')) dotColor = 'green';
      else if (raw.includes('connecting')) dotColor = 'yellow';
      else if (raw.includes('disconnected')) dotColor = 'red';
      else if (raw.includes('ready')) dotColor = 'green';
      return `
        <label style="display: flex; align-items: center; gap: 8px; padding: 4px 0; cursor: pointer;">
          <input type="checkbox" class="mf-service-cb" value="${Utils.escapeAttr(s.name)}" ${checked}>
          <span class="status-dot status-dot-${dotColor}"></span>
          <span>${Utils.escapeHtml(s.name)}</span>
        </label>
      `;
    }).join('');
  }

  return { open, close };
})();

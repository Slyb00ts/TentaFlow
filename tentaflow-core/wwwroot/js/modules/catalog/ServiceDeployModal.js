// =============================================================================
// Plik: modules/catalog/ServiceDeployModal.js
// Opis: Modal 3-krokowy wdrazania standardowych uslug (TTS, STT, Embeddings, itp.)
//       Deploy przez /api/mesh/nodes/{nodeId}/deploy.
// Przyklad: ServiceDeployModal.open(nodeId, serviceObj);
// =============================================================================

const ServiceDeployModal = (() => {
  'use strict';

  let currentStep = 1;
  let serviceConfig = null;
  let nodeId = null;
  let params = {};
  let generatedYaml = '';
  let isProcessing = false;
  let hostGpus = [];

  // Generowanie losowego hasla alfanumerycznego
  function generateRandomPassword(length) {
    const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
    let result = '';
    const array = new Uint8Array(length);
    crypto.getRandomValues(array);
    for (let i = 0; i < length; i++) {
      result += chars[array[i] % chars.length];
    }
    return result;
  }

  // Otwarcie modala dla wybranej uslugi
  async function open(nodeIdParam, service) {
    nodeId = nodeIdParam;
    serviceConfig = service;
    currentStep = 1;
    isProcessing = false;
    params = {
      port: service.defaultPort || 5000,
      gpuId: 'all',
      configPath: `/opt/tentaflow/${service.id}/config.toml`,
    };

    if (service.id === 'tentaflow' || service.id === 'bms') {
      params.includeDb = true;
      params.dbPassword = generateRandomPassword(16);
      delete params.configPath;
    }
    if (service.id === 'bms') {
      params.emisePort = 11014;
    }

    generatedYaml = '';
    hostGpus = [];

    // Pokaz modal natychmiast
    createModal();

    // Pobierz dane rownolegle w tle
    const [settings, nodeData] = await Promise.all([
      ApiClient.get('/api/settings').catch(() => null),
      ApiClient.get(`/api/mesh/nodes/${encodeURIComponent(nodeId)}`).catch(() => null),
    ]);

    if (Array.isArray(settings)) {
      const hfSetting = settings.find(s => s.key === 'hf_token');
      if (hfSetting) params.hfToken = hfSetting.value;
    }
    if (nodeData && Array.isArray(nodeData.gpu_info)) {
      hostGpus = nodeData.gpu_info;
    }

    // Przerenderuj z zaladowanymi GPU
    updateUI();
  }

  // Zamkniecie modala
  function close() {
    removeModal();
    isProcessing = false;
  }

  // Utworzenie modala w DOM
  function createModal() {
    removeModal();

    const overlay = document.createElement('div');
    overlay.className = 'modal-overlay active';
    overlay.id = 'service-deploy-modal';

    overlay.innerHTML = `
      <div class="modal" style="max-width: 640px;">
        <div class="modal-header">
          <h3>Deploy: ${Utils.escapeHtml(serviceConfig.name)}</h3>
          <button class="modal-close" id="sdm-close">&times;</button>
        </div>
        <div class="modal-body">
          <div class="wizard-step-indicator" id="sdm-step-indicator">
            ${renderStepDots()}
          </div>
          <div id="sdm-content">
            ${renderStepContent()}
          </div>
        </div>
        <div class="modal-footer" id="sdm-footer">
          ${renderFooterButtons()}
        </div>
      </div>
    `;

    document.body.appendChild(overlay);
    mountEvents();
  }

  // Kropki wskaznika krokow
  function renderStepDots() {
    const totalSteps = 3;
    let html = '';
    for (let i = 1; i <= totalSteps; i++) {
      const cls = i === currentStep ? 'active'
        : i < currentStep ? 'done'
        : '';
      html += `<div class="wizard-step-dot ${cls}"></div>`;
    }
    return html;
  }

  // Przyciski nawigacji
  function renderFooterButtons() {
    const isLast = currentStep === 3;
    let html = `<button class="btn btn-ghost btn-sm" id="sdm-cancel">${I18n.t('common.cancel')}</button>`;
    if (currentStep > 1) {
      html += `<button class="btn btn-secondary btn-sm" id="sdm-prev">\u2190 ${I18n.t('common.back')}</button>`;
    }
    html += `<button class="btn btn-primary btn-sm" id="sdm-next">${isLast ? 'Deploy' : I18n.t('common.next') + ' \u2192'}</button>`;
    return html;
  }

  // Tresc biezacego kroku
  function renderStepContent() {
    switch (currentStep) {
      case 1: return renderStepParams();
      case 2: return renderStepYaml();
      case 3: return renderStepSummary();
      default: return '';
    }
  }

  // Krok 1: Parametry uslugi
  function renderStepParams() {
    const gpuSelect = serviceConfig.gpu ? `
      <div class="form-group">
        <label for="sdm-gpu">GPU</label>
        <select id="sdm-gpu">
          <option value="all" ${params.gpuId === 'all' ? 'selected' : ''}>All GPUs</option>
          ${hostGpus.length > 0
            ? hostGpus.map(g => `<option value="${g.index}" ${params.gpuId === String(g.index) ? 'selected' : ''}>GPU ${g.index}: ${Utils.escapeHtml(g.name)} (${Math.round(g.vram_total_mb / 1024)} GB)</option>`).join('')
            : `<option value="0" ${params.gpuId === '0' ? 'selected' : ''}>GPU 0</option>`
          }
        </select>
      </div>
    ` : '';

    const isDbService = serviceConfig.id === 'tentaflow' || serviceConfig.id === 'bms';
    const dbSection = isDbService ? `
      <div class="form-group">
        <label>
          <input type="checkbox" id="sdm-include-db" ${params.includeDb ? 'checked' : ''}>
          Dodaj kontener bazy danych ${serviceConfig.id === 'tentaflow' ? '(PostgreSQL)' : '(ClickHouse)'}
        </label>
      </div>
      <div class="form-group">
        <label for="sdm-db-password">Haslo bazy danych</label>
        <input type="text" id="sdm-db-password" value="${Utils.escapeAttr(params.dbPassword || '')}">
      </div>
    ` : '';

    const emiseSection = serviceConfig.id === 'bms' ? `
      <div class="form-group">
        <label for="sdm-emise-port">Port EMISE</label>
        <input type="number" id="sdm-emise-port" value="${Utils.escapeAttr(params.emisePort || 11014)}" min="1" max="65535">
      </div>
    ` : '';

    return `
      <div class="deploy-param-grid">
        <div class="form-group">
          <label for="sdm-port">Port</label>
          <input type="number" id="sdm-port" value="${Utils.escapeAttr(params.port)}" min="1" max="65535">
        </div>
        ${gpuSelect}
        ${emiseSection}
        ${isDbService ? '' : `<div class="form-group">
          <label for="sdm-config-path">Sciezka konfiguracji</label>
          <input type="text" id="sdm-config-path" value="${Utils.escapeAttr(params.configPath || '')}">
          <div class="form-hint">Sciezka do pliku konfiguracyjnego na hoscie</div>
        </div>`}
        ${dbSection}
      </div>
    `;
  }

  // Krok 2: Podglad YAML
  function renderStepYaml() {
    if (!generatedYaml) {
      generatedYaml = ComposeTemplates.generate(serviceConfig.id, params);
    }

    return `
      <div class="form-group">
        <label>Stack: tentaflow-${Utils.escapeHtml(serviceConfig.id)}</label>
      </div>
      <div class="form-group">
        <label for="sdm-yaml">Docker Compose YAML</label>
        <textarea id="sdm-yaml" class="yaml-preview" rows="14">${Utils.escapeHtml(generatedYaml)}</textarea>
        <div class="form-hint">Mozesz edytowac YAML przed wdrozeniem</div>
      </div>
    `;
  }

  // Krok 3: Podsumowanie i deploy
  function renderStepSummary() {
    let gpuLabel = 'Brak (CPU)';
    if (serviceConfig.gpu) {
      if (params.gpuId === 'all') {
        gpuLabel = 'All GPUs';
      } else {
        const gpuInfo = hostGpus.find(g => String(g.index) === String(params.gpuId));
        gpuLabel = gpuInfo
          ? `GPU ${gpuInfo.index}: ${Utils.escapeHtml(gpuInfo.name)} (${Math.round(gpuInfo.vram_total_mb / 1024)} GB)`
          : `GPU ${Utils.escapeHtml(params.gpuId)}`;
      }
    }

    return `
      <div class="wizard-summary">
        <div class="wizard-summary-row">
          <span class="wizard-summary-label">Usluga:</span>
          <span class="wizard-summary-value">${Utils.escapeHtml(serviceConfig.name)}</span>
        </div>
        <div class="wizard-summary-row">
          <span class="wizard-summary-label">Port:</span>
          <span class="wizard-summary-value">${Utils.escapeHtml(String(params.port))}</span>
        </div>
        <div class="wizard-summary-row">
          <span class="wizard-summary-label">GPU:</span>
          <span class="wizard-summary-value">${gpuLabel}</span>
        </div>
        ${params.configPath ? `<div class="wizard-summary-row">
          <span class="wizard-summary-label">Konfiguracja:</span>
          <span class="wizard-summary-value">${Utils.escapeHtml(params.configPath)}</span>
        </div>` : ''}
        ${params.includeDb !== undefined ? `<div class="wizard-summary-row">
          <span class="wizard-summary-label">Baza danych:</span>
          <span class="wizard-summary-value">${params.includeDb ? 'Lokalna' : 'Zewnetrzna'}</span>
        </div>` : ''}
        ${params.emisePort ? `<div class="wizard-summary-row">
          <span class="wizard-summary-label">Port EMISE:</span>
          <span class="wizard-summary-value">${Utils.escapeHtml(String(params.emisePort))}</span>
        </div>` : ''}
        <div class="wizard-summary-row">
          <span class="wizard-summary-label">Stack:</span>
          <span class="wizard-summary-value">tentaflow-${Utils.escapeHtml(serviceConfig.id)}</span>
        </div>
      </div>
      <div id="sdm-result"></div>
    `;
  }

  // Podpiecie zdarzen
  function mountEvents() {
    const closeBtn = document.getElementById('sdm-close');
    const cancelBtn = document.getElementById('sdm-cancel');
    const nextBtn = document.getElementById('sdm-next');
    const prevBtn = document.getElementById('sdm-prev');

    if (closeBtn) closeBtn.addEventListener('click', close);
    if (cancelBtn) cancelBtn.addEventListener('click', close);
    if (nextBtn) nextBtn.addEventListener('click', handleNext);
    if (prevBtn) prevBtn.addEventListener('click', handlePrev);

    bindStepInputs();
  }

  // Podpiecie inputow biezacego kroku
  function bindStepInputs() {
    const portInput = document.getElementById('sdm-port');
    if (portInput) {
      portInput.addEventListener('input', () => {
        params.port = portInput.value;
      });
    }

    const gpuSelect = document.getElementById('sdm-gpu');
    if (gpuSelect) {
      gpuSelect.addEventListener('change', () => {
        params.gpuId = gpuSelect.value;
      });
    }

    const configInput = document.getElementById('sdm-config-path');
    if (configInput) {
      configInput.addEventListener('input', () => {
        params.configPath = configInput.value;
      });
    }

    const includeDbCheckbox = document.getElementById('sdm-include-db');
    if (includeDbCheckbox) {
      includeDbCheckbox.addEventListener('change', () => {
        params.includeDb = includeDbCheckbox.checked;
      });
    }

    const dbPasswordInput = document.getElementById('sdm-db-password');
    if (dbPasswordInput) {
      dbPasswordInput.addEventListener('input', () => {
        params.dbPassword = dbPasswordInput.value;
      });
    }

    const emisePortInput = document.getElementById('sdm-emise-port');
    if (emisePortInput) {
      emisePortInput.addEventListener('input', () => {
        params.emisePort = emisePortInput.value;
      });
    }

    const yamlTextarea = document.getElementById('sdm-yaml');
    if (yamlTextarea) {
      yamlTextarea.addEventListener('input', () => {
        generatedYaml = yamlTextarea.value;
      });
    }
  }

  // Obsluga przycisku "Dalej" / "Wdroz"
  async function handleNext() {
    if (isProcessing) return;

    if (!validateStep()) return;

    if (currentStep === 3) {
      await executeDeploy();
      return;
    }

    // Przy przejsciu z kroku 1 na 2 - wygeneruj YAML
    if (currentStep === 1) {
      generatedYaml = '';
    }

    currentStep++;
    updateUI();
  }

  // Obsluga przycisku "Wstecz"
  function handlePrev() {
    if (currentStep > 1) {
      currentStep--;
      updateUI();
    }
  }

  // Odswiez caly modal
  function updateUI() {
    const indicator = document.getElementById('sdm-step-indicator');
    const content = document.getElementById('sdm-content');
    const footer = document.getElementById('sdm-footer');

    if (indicator) indicator.innerHTML = renderStepDots();
    if (content) content.innerHTML = renderStepContent();
    if (footer) footer.innerHTML = renderFooterButtons();

    mountEvents();
  }

  // Walidacja biezacego kroku
  function validateStep() {
    if (currentStep === 1) {
      const port = parseInt(params.port, 10);
      if (!port || port < 1 || port > 65535) {
        App.showToast('Podaj prawidlowy port (1-65535)', 'error');
        return false;
      }
      if (params.configPath !== undefined && !params.configPath.trim()) {
        App.showToast('Sciezka konfiguracji jest wymagana', 'error');
        return false;
      }
      return true;
    }

    if (currentStep === 2) {
      if (!generatedYaml.trim()) {
        App.showToast('YAML nie moze byc pusty', 'error');
        return false;
      }
      return true;
    }

    return true;
  }

  // Wykonanie wdrozenia przez mesh API
  async function executeDeploy() {
    isProcessing = true;
    const deployLogs = [];
    const deployStartTime = Date.now();
    let deployTimerInterval = null;
    const resultEl = document.getElementById('sdm-result');
    const nextBtn = document.getElementById('sdm-next');

    if (nextBtn) nextBtn.disabled = true;
    if (resultEl) {
      resultEl.innerHTML = `<div class="wizard-progress">Sprawdzanie portow...</div>`;
    }

    // Sprawdz zajete porty na hoscie
    const port = parseInt(params.port, 10) || serviceConfig.defaultPort;
    const freePort = await DeployUtils.findFreePort(nodeId, port);
    if (freePort !== port) {
      params.port = freePort;
      generatedYaml = generatedYaml.replace(
        new RegExp(`"${port}:`, 'g'),
        `"${freePort}:`
      );
      App.showToast(`Port ${port} zajety, uzyto ${freePort}`, 'warning');
    }

    if (resultEl) {
      resultEl.innerHTML = DeployUtils.renderDeployProgress('connecting', 'Laczenie...', null, deployLogs, deployStartTime);
    }

    deployTimerInterval = setInterval(() => {
      if (resultEl && isProcessing) {
        const timerEl = resultEl.querySelector('.deploy-timer');
        if (timerEl) {
          const elapsed = ((Date.now() - deployStartTime) / 1000).toFixed(0);
          timerEl.textContent = elapsed + 's';
        }
      }
    }, 1000);

    const stackName = `tentaflow-${serviceConfig.id}`;
    const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const token = ApiClient.getToken();
    const wsUrl = `${protocol}//${location.host}/ws/deploy`;
    const ws = new WebSocket(wsUrl, token ? [`bearer.${token}`] : []);

    ws.onopen = () => {
      ws.send(JSON.stringify({
        node_id: nodeId,
        stack_name: stackName,
        compose_yaml: generatedYaml,
        service_name: stackName,
        config_json: JSON.stringify(Object.assign({
          engine: serviceConfig.id,
          model_id: serviceConfig.id,
          port: parseInt(params.port, 10) || serviceConfig.defaultPort,
          container_name: stackName,
          service_type: serviceConfig.id
        }, serviceConfig.id === 'meeting-bot' ? { protocol: 'quic', service_type: 'meeting-bot' } : {}))
      }));
    };

    ws.onmessage = (e) => {
      try {
        const msg = JSON.parse(e.data);
        if (msg.message) deployLogs.push(msg.message);
        if (resultEl) {
          resultEl.innerHTML = DeployUtils.renderDeployProgress(msg.phase, msg.message || '', msg, deployLogs, deployStartTime);
          const logBox = resultEl.querySelector('.deploy-log-box');
          if (logBox) logBox.scrollTop = logBox.scrollHeight;
        }
        if (msg.phase === 'done') {
          clearInterval(deployTimerInterval);
          ws.close();
          if (msg.success) {
            App.showToast(`Usluga ${Utils.escapeHtml(serviceConfig.name)} wdrozona`, 'success');
            setTimeout(() => { close(); }, 2000);
          } else {
            App.showToast(`Blad: ${msg.error || 'Nieznany blad'}`, 'error');
          }
          isProcessing = false;
          if (nextBtn) nextBtn.disabled = false;
        }
      } catch {}
    };

    ws.onerror = () => {
      clearInterval(deployTimerInterval);
      if (resultEl) {
        resultEl.innerHTML = `<div class="wizard-error">Blad polaczenia WebSocket</div>`;
      }
      App.showToast('Blad polaczenia WebSocket', 'error');
      isProcessing = false;
      if (nextBtn) nextBtn.disabled = false;
    };

    ws.onclose = () => {
      clearInterval(deployTimerInterval);
      if (isProcessing) {
        isProcessing = false;
        if (nextBtn) nextBtn.disabled = false;
      }
    };
  }

  // Usuniecie modala z DOM
  function removeModal() {
    const existing = document.getElementById('service-deploy-modal');
    if (existing) existing.remove();
  }

  return { open, close };
})();

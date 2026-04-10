// =============================================================================
// Plik: modules/catalog/LLMDeployWizard.js
// Opis: Wizard 5-krokowy wdrazania serwera LLM — wybor silnika, modelu, parametrow.
//       Deploy przez /api/mesh/nodes/{nodeId}/deploy.
// Przyklad: LLMDeployWizard.open(nodeId);
// =============================================================================

const LLMDeployWizard = (() => {
  'use strict';

  let currentStep = 1;
  let nodeId = null;
  let selectedEngine = null;
  let selectedModel = null;
  let containerName = '';
  let params = {};
  let generatedYaml = '';
  let hfToken = '';
  let isProcessing = false;
  let hostGpus = [];

  // Losowy suffix 5 znakow (male litery + cyfry)
  function randomSuffix(len) {
    const chars = 'abcdefghijklmnopqrstuvwxyz0123456789';
    let r = '';
    for (let i = 0; i < (len || 5); i++) {
      r += chars[Math.floor(Math.random() * chars.length)];
    }
    return r;
  }

  // Silniki — dynamicznie pobierane z API, z fallbackiem
  const FALLBACK_ENGINES = [
    {
      id: 'sglang',
      name: 'SGLang',
      desc: 'Szybki serwer inference z ciagla przetwarzaniem wsadowym',
      icon: () => CatalogIcons.sglang(),
      deploy_mode: 'docker',
    },
    {
      id: 'vllm',
      name: 'vLLM',
      desc: 'Wydajny serwer inference z PagedAttention',
      icon: () => CatalogIcons.vllm(),
      deploy_mode: 'docker',
    },
    {
      id: 'ollama',
      name: 'Ollama',
      desc: 'Prosty serwer inference dla modeli GGUF',
      icon: () => CatalogIcons.ollama(),
      deploy_mode: 'native',
    },
    {
      id: 'llamacpp',
      name: 'LLama.cpp',
      desc: 'Lekki inference C++ z obsluga GGUF',
      icon: () => CatalogIcons.llamacpp(),
      deploy_mode: 'native',
    },
    {
      id: 'mlx',
      name: 'MLX',
      desc: 'Apple MLX framework for Apple Silicon inference',
      icon: () => CatalogIcons.llamacpp(),
      deploy_mode: 'native',
    },
    {
      id: 'tensorrt-llm',
      name: 'TensorRT-LLM',
      desc: 'NVIDIA TensorRT-LLM — zoptymalizowany inference GPU z kwantyzacja FP8/INT4',
      icon: () => CatalogIcons.nvidia(),
      deploy_mode: 'docker',
    },
  ];

  let ENGINES = [...FALLBACK_ENGINES];
  let MODELS = [];
  let deployMode = 'docker';
  let searchDebounceTimer = null;

  // Otwarcie wizarda
  async function open(nodeIdParam) {
    nodeId = nodeIdParam;
    currentStep = 1;
    isProcessing = false;
    selectedEngine = null;
    selectedModel = null;
    containerName = `tentaflow-ai-llm-${randomSuffix()}`;
    generatedYaml = '';
    hfToken = '';
    params = {
      port: 5010,
      gpuId: 'all',
      shmSize: '16g',
      gpuMemoryUtilization: 0.9,
    };

    // Pokaz modal natychmiast z domyslnymi danymi (krok 1)
    createModal();

    // Pobierz dane rownolegle w tle
    const [settings, nodeData, engines] = await Promise.all([
      ApiClient.get('/api/settings').catch(() => null),
      ApiClient.get(`/api/mesh/nodes/${encodeURIComponent(nodeId)}`).catch(() => null),
      ApiClient.get(`/api/hub/engines?node_id=${encodeURIComponent(nodeId)}&type=llm`).catch(() => null),
    ]);

    if (Array.isArray(settings)) {
      const hfSetting = settings.find(s => s.key === 'hf_token');
      if (hfSetting) hfToken = hfSetting.value;
    }

    hostGpus = [];
    if (nodeData && Array.isArray(nodeData.gpu_info)) {
      hostGpus = nodeData.gpu_info;
    }

    if (Array.isArray(engines) && engines.length > 0) {
      ENGINES = engines.map(e => ({
        id: e.id,
        name: e.name,
        desc: e.description,
        icon: () => (CatalogIcons[e.id] ? CatalogIcons[e.id]() : CatalogIcons.llamacpp()),
        deploy_mode: e.deploy_mode || 'docker',
        model_format: e.model_format || '',
      }));
    }

    // Przerenderuj krok 1 z nowymi danymi (silniki z API)
    updateUI();
  }

  // Zamkniecie wizarda
  function close() {
    removeModal();
    isProcessing = false;
  }

  // Utworzenie modala w DOM
  function createModal() {
    removeModal();

    const overlay = document.createElement('div');
    overlay.className = 'modal-overlay active';
    overlay.id = 'llm-deploy-wizard-modal';

    overlay.innerHTML = `
      <div class="modal" style="max-width: 640px;">
        <div class="modal-header">
          <h3>Deploy LLM Server</h3>
          <button class="modal-close" id="llm-close">&times;</button>
        </div>
        <div class="modal-body">
          <div class="wizard-step-indicator" id="llm-step-indicator">
            ${renderStepDots()}
          </div>
          <div id="llm-content">
            ${renderStepContent()}
          </div>
        </div>
        <div class="modal-footer" id="llm-footer">
          ${renderFooterButtons()}
        </div>
      </div>
    `;

    document.body.appendChild(overlay);
    mountEvents();
  }

  // Kropki wskaznika krokow
  function renderStepDots() {
    const totalSteps = 5;
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
    const isLast = currentStep === 5;
    let html = `<button class="btn btn-ghost btn-sm" id="llm-cancel">${I18n.t('common.cancel')}</button>`;
    if (currentStep > 1) {
      html += `<button class="btn btn-secondary btn-sm" id="llm-prev">\u2190 ${I18n.t('common.back')}</button>`;
    }
    html += `<button class="btn btn-primary btn-sm" id="llm-next">${isLast ? 'Deploy' : I18n.t('common.next') + ' \u2192'}</button>`;
    return html;
  }

  // Tresc biezacego kroku
  function renderStepContent() {
    switch (currentStep) {
      case 1: return renderStepEngine();
      case 2: return renderStepModel();
      case 3: return renderStepParams();
      case 4: return renderStepYaml();
      case 5: return renderStepSummary();
      default: return '';
    }
  }

  // Krok 1: Wybor silnika
  function renderStepEngine() {
    const cards = ENGINES.map(engine => {
      const selectedCls = selectedEngine === engine.id ? ' selected' : '';
      const modeBadge = engine.deploy_mode === 'native'
        ? '<span class="badge badge-success" style="font-size:10px;margin-left:6px;">native</span>'
        : '<span class="badge badge-info" style="font-size:10px;margin-left:6px;">docker</span>';
      return `
        <div class="engine-card${selectedCls}" data-engine="${Utils.escapeAttr(engine.id)}" data-deploy-mode="${Utils.escapeAttr(engine.deploy_mode || 'docker')}">
          <div class="engine-card-icon">${engine.icon()}</div>
          <div class="engine-card-name">${Utils.escapeHtml(engine.name)}${modeBadge}</div>
          <div class="engine-card-desc">${Utils.escapeHtml(engine.desc)}</div>
        </div>
      `;
    }).join('');

    return `
      <div class="engine-selector">
        ${cards}
      </div>
    `;
  }

  // Krok 2: Wybor modelu (z wyszukiwaniem HuggingFace)
  function renderStepModel() {
    const items = MODELS.map(model => {
      const mid = model.model_id || model.id;
      const selectedCls = selectedModel === mid ? ' selected' : '';
      const displayName = model.model_id || model.name || mid;
      const author = model.author || '';
      const downloads = model.downloads ? ` \u2193${formatCount(model.downloads)}` : '';
      return `
        <div class="model-item${selectedCls}" data-model-id="${Utils.escapeAttr(mid)}">
          <div class="model-item-name">${Utils.escapeHtml(displayName)}</div>
          <div class="model-item-info">${Utils.escapeHtml(author)}${downloads}</div>
        </div>
      `;
    }).join('');

    const customValue = selectedModel && !MODELS.find(m => (m.model_id || m.id) === selectedModel)
      ? selectedModel
      : '';

    return `
      <div class="form-group" style="margin-bottom: var(--spacing-sm);">
        <label for="llm-model-search">Szukaj modeli na HuggingFace:</label>
        <input type="text" id="llm-model-search" placeholder="np. llama, qwen, mistral..." value="">
      </div>
      <div class="model-list" id="llm-model-list">
        ${items}
      </div>
      <div class="form-group" style="margin-top: var(--spacing-md);">
        <label for="llm-custom-model">Wlasny model (HuggingFace repo)</label>
        <input type="text" id="llm-custom-model"
          placeholder="np. speakleash/Bielik-11B-v3.0-Instruct-FP8-Dynamic"
          value="${Utils.escapeAttr(customValue)}">
      </div>
    `;
  }

  function formatCount(n) {
    if (n >= 1000000) return (n / 1000000).toFixed(1) + 'M';
    if (n >= 1000) return (n / 1000).toFixed(1) + 'k';
    return String(n);
  }

  // Krok 3: Parametry
  function renderStepParams() {
    const showGpuMem = selectedEngine === 'sglang' || selectedEngine === 'vllm';

    const gpuMemField = showGpuMem ? `
      <div class="form-group">
        <label for="llm-gpu-mem">GPU Memory Utilization</label>
        <input type="number" id="llm-gpu-mem"
          value="${Utils.escapeAttr(params.gpuMemoryUtilization)}"
          step="0.05" min="0.1" max="1.0">
        <div class="form-hint">Ulamek pamieci GPU do uzycia (0.1 - 1.0)</div>
      </div>
    ` : '';

    return `
      <div class="form-group" style="margin-bottom: var(--spacing-md);">
        <label for="llm-cname">Nazwa kontenera</label>
        <input type="text" id="llm-cname" value="${Utils.escapeAttr(containerName)}" placeholder="np. tentaflow-ai-llm-abc12">
        <div class="form-hint">Unikalna nazwa kontenera/stacka</div>
      </div>
      <div class="deploy-param-grid">
        <div class="form-group">
          <label for="llm-port">Port</label>
          <input type="number" id="llm-port" value="${Utils.escapeAttr(params.port)}" min="1" max="65535">
        </div>
        <div class="form-group">
          <label for="llm-gpu">GPU</label>
          <select id="llm-gpu">
            <option value="all" ${params.gpuId === 'all' ? 'selected' : ''}>All GPUs</option>
            ${hostGpus.length > 0
              ? hostGpus.map(g => `<option value="${g.index}" ${params.gpuId === String(g.index) ? 'selected' : ''}>GPU ${g.index}: ${Utils.escapeHtml(g.name)} (${Math.round(g.vram_total_mb / 1024)} GB)</option>`).join('')
              : `<option value="0" ${params.gpuId === '0' ? 'selected' : ''}>GPU 0</option>`
            }
          </select>
        </div>
        <div class="form-group">
          <label for="llm-shm">SHM Size</label>
          <select id="llm-shm">
            <option value="8g" ${params.shmSize === '8g' ? 'selected' : ''}>8g</option>
            <option value="16g" ${params.shmSize === '16g' ? 'selected' : ''}>16g</option>
            <option value="32g" ${params.shmSize === '32g' ? 'selected' : ''}>32g</option>
            <option value="64g" ${params.shmSize === '64g' ? 'selected' : ''}>64g</option>
          </select>
        </div>
        ${gpuMemField}
      </div>
    `;
  }

  // Krok 4: Podglad YAML lub komend natywnych
  function renderStepYaml() {
    if (!generatedYaml) {
      if (deployMode === 'native') {
        generatedYaml = generateNativePreview(selectedEngine, params);
      } else {
        generatedYaml = ComposeTemplates.generateLLM(selectedEngine, params);
      }
    }

    const previewLabel = deployMode === 'native'
      ? 'Komendy do wykonania (natywnie):'
      : 'Docker Compose YAML';

    return `
      <div class="form-group">
        <label>Stack: ${Utils.escapeHtml(containerName)}</label>
        ${deployMode === 'native' ? '<span class="badge badge-success" style="margin-left:8px;">Native</span>' : ''}
      </div>
      <div class="form-group">
        <label for="llm-yaml">${previewLabel}</label>
        <textarea id="llm-yaml" class="yaml-preview" rows="14">${Utils.escapeHtml(generatedYaml)}</textarea>
        <div class="form-hint">${deployMode === 'native' ? 'Komendy zostana wykonane na hoscie bez Docker.' : 'Mozesz edytowac YAML przed wdrozeniem'}</div>
      </div>
    `;
  }

  // Generuj podglad komend natywnych per silnik
  function generateNativePreview(engineId, params) {
    const model = params.modelId || '';
    const port = params.port || 5010;
    switch (engineId) {
      case 'ollama':
        return `# Ollama (native)\nollama serve &\nollama pull ${model}\nexport OLLAMA_HOST=0.0.0.0:${port}`;
      case 'mlx':
        return `# MLX (native in-process — Apple Silicon Metal GPU)\n# Model ladowany bezposrednio przez InferenceManager (mlx-rs)\n# Brak osobnego procesu — zero overhead\n#\n# Model: ${model}\n# Port: ${port} (OpenAI API endpoint)`;
      case 'llamacpp':
        return `# LLama.cpp (native)\n# Install: brew install llama.cpp\nllama-server -m <model_path>.gguf --port ${port} -ngl 99`;
      default:
        return `# Engine: ${engineId}\n# Model: ${model}\n# Port: ${port}`;
    }
  }

  // Krok 5: Podsumowanie i deploy
  function renderStepSummary() {
    const engineDef = ENGINES.find(e => e.id === selectedEngine);
    const engineName = engineDef ? engineDef.name : selectedEngine;
    const modelDef = MODELS.find(m => m.id === selectedModel);
    const modelName = modelDef ? modelDef.name : selectedModel;
    let gpuLabel = 'All GPUs';
    if (params.gpuId !== 'all') {
      const gpuInfo = hostGpus.find(g => String(g.index) === String(params.gpuId));
      gpuLabel = gpuInfo
        ? `GPU ${gpuInfo.index}: ${Utils.escapeHtml(gpuInfo.name)} (${Math.round(gpuInfo.vram_total_mb / 1024)} GB)`
        : `GPU ${Utils.escapeHtml(params.gpuId)}`;
    }

    const showGpuMem = selectedEngine === 'sglang' || selectedEngine === 'vllm';

    const hfWarning = !hfToken ? `
      <div class="hf-token-warning" style="margin-top: var(--spacing-md);">
        Brak tokenu HuggingFace. Niektore modele moga wymagac autoryzacji.
        Dodaj token w Ustawienia &gt; HuggingFace Token.
      </div>
    ` : '';

    return `
      <div class="wizard-summary">
        <div class="wizard-summary-row">
          <span class="wizard-summary-label">Kontener:</span>
          <span class="wizard-summary-value">${Utils.escapeHtml(containerName)}</span>
        </div>
        <div class="wizard-summary-row">
          <span class="wizard-summary-label">Silnik:</span>
          <span class="wizard-summary-value">${Utils.escapeHtml(engineName)}</span>
        </div>
        <div class="wizard-summary-row">
          <span class="wizard-summary-label">Model:</span>
          <span class="wizard-summary-value">${Utils.escapeHtml(modelName)}</span>
        </div>
        <div class="wizard-summary-row">
          <span class="wizard-summary-label">Port:</span>
          <span class="wizard-summary-value">${Utils.escapeHtml(String(params.port))}</span>
        </div>
        <div class="wizard-summary-row">
          <span class="wizard-summary-label">GPU:</span>
          <span class="wizard-summary-value">${gpuLabel}</span>
        </div>
        <div class="wizard-summary-row">
          <span class="wizard-summary-label">SHM Size:</span>
          <span class="wizard-summary-value">${Utils.escapeHtml(params.shmSize)}</span>
        </div>
        ${showGpuMem ? `
          <div class="wizard-summary-row">
            <span class="wizard-summary-label">GPU Memory:</span>
            <span class="wizard-summary-value">${Utils.escapeHtml(String(params.gpuMemoryUtilization))}</span>
          </div>
        ` : ''}
        <div class="wizard-summary-row">
          <span class="wizard-summary-label">Stack:</span>
          <span class="wizard-summary-value">tentaflow-llm</span>
        </div>
      </div>
      ${hfWarning}
      <div id="llm-result"></div>
    `;
  }

  // Podpiecie zdarzen
  function mountEvents() {
    const closeBtn = document.getElementById('llm-close');
    const cancelBtn = document.getElementById('llm-cancel');
    const nextBtn = document.getElementById('llm-next');
    const prevBtn = document.getElementById('llm-prev');

    if (closeBtn) closeBtn.addEventListener('click', close);
    if (cancelBtn) cancelBtn.addEventListener('click', close);
    if (nextBtn) nextBtn.addEventListener('click', handleNext);
    if (prevBtn) prevBtn.addEventListener('click', handlePrev);

    bindStepInputs();
  }

  // Podpiecie inputow biezacego kroku
  function bindStepInputs() {
    // Krok 1: Wybor silnika
    const engineCards = document.querySelectorAll('.engine-card[data-engine]');
    engineCards.forEach(card => {
      card.addEventListener('click', async () => {
        selectedEngine = card.dataset.engine;
        deployMode = card.dataset.deployMode || 'docker';
        engineCards.forEach(c => c.classList.remove('selected'));
        card.classList.add('selected');

        // Zaladuj domyslne modele per silnik
        try {
          const defaults = await ApiClient.get(`/api/hub/models/defaults?engine=${encodeURIComponent(selectedEngine)}`);
          if (Array.isArray(defaults)) {
            MODELS = defaults;
          }
        } catch (err) {
          console.error('Blad pobierania domyslnych modeli:', err);
        }
      });
    });

    // Krok 2: Wybor modelu z listy
    const modelItems = document.querySelectorAll('.model-item[data-model-id]');
    modelItems.forEach(item => {
      item.addEventListener('click', () => {
        selectedModel = item.dataset.modelId;
        document.querySelectorAll('.model-item').forEach(m => m.classList.remove('selected'));
        item.classList.add('selected');
        const customInput = document.getElementById('llm-custom-model');
        if (customInput) customInput.value = '';
      });
    });

    // Krok 2: Wyszukiwanie modeli HuggingFace (z debounce)
    const searchInput = document.getElementById('llm-model-search');
    if (searchInput) {
      searchInput.addEventListener('input', () => {
        clearTimeout(searchDebounceTimer);
        const q = searchInput.value.trim();
        if (!q || q.length < 2) return;
        searchDebounceTimer = setTimeout(async () => {
          try {
            const results = await ApiClient.get(
              `/api/hub/models/search?q=${encodeURIComponent(q)}&engine=${encodeURIComponent(selectedEngine || 'sglang')}&limit=20`
            );
            if (Array.isArray(results)) {
              MODELS = results;
              const listEl = document.getElementById('llm-model-list');
              if (listEl) {
                listEl.innerHTML = MODELS.map(model => {
                  const mid = model.model_id || model.id;
                  const selectedCls = selectedModel === mid ? ' selected' : '';
                  const downloads = model.downloads ? ` \u2193${formatCount(model.downloads)}` : '';
                  return `
                    <div class="model-item${selectedCls}" data-model-id="${Utils.escapeAttr(mid)}">
                      <div class="model-item-name">${Utils.escapeHtml(mid)}</div>
                      <div class="model-item-info">${Utils.escapeHtml(model.author || '')}${downloads}</div>
                    </div>
                  `;
                }).join('');
                listEl.querySelectorAll('.model-item[data-model-id]').forEach(item => {
                  item.addEventListener('click', () => {
                    selectedModel = item.dataset.modelId;
                    listEl.querySelectorAll('.model-item').forEach(m => m.classList.remove('selected'));
                    item.classList.add('selected');
                    const ci = document.getElementById('llm-custom-model');
                    if (ci) ci.value = '';
                  });
                });
              }
            }
          } catch (err) {
            console.error('Blad wyszukiwania modeli:', err);
          }
        }, 300);
      });
    }

    // Krok 2: Wlasny model
    const customModelInput = document.getElementById('llm-custom-model');
    if (customModelInput) {
      customModelInput.addEventListener('input', () => {
        const val = customModelInput.value.trim();
        if (val) {
          selectedModel = val;
          document.querySelectorAll('.model-item').forEach(m => m.classList.remove('selected'));
        }
      });
    }

    // Krok 3: Nazwa kontenera
    const cnameInput = document.getElementById('llm-cname');
    if (cnameInput) {
      cnameInput.addEventListener('input', () => {
        containerName = cnameInput.value.trim();
      });
    }

    // Krok 3: Parametry
    const portInput = document.getElementById('llm-port');
    if (portInput) {
      portInput.addEventListener('input', () => {
        params.port = portInput.value;
      });
    }

    const gpuSelect = document.getElementById('llm-gpu');
    if (gpuSelect) {
      gpuSelect.addEventListener('change', () => {
        params.gpuId = gpuSelect.value;
      });
    }

    const shmSelect = document.getElementById('llm-shm');
    if (shmSelect) {
      shmSelect.addEventListener('change', () => {
        params.shmSize = shmSelect.value;
      });
    }

    const gpuMemInput = document.getElementById('llm-gpu-mem');
    if (gpuMemInput) {
      gpuMemInput.addEventListener('input', () => {
        params.gpuMemoryUtilization = parseFloat(gpuMemInput.value) || 0.9;
      });
    }

    // Krok 4: Edycja YAML
    const yamlTextarea = document.getElementById('llm-yaml');
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

    if (currentStep === 5) {
      await executeDeploy();
      return;
    }

    // Przy przejsciu na krok 4 - wygeneruj YAML lub komendy natywne
    if (currentStep === 3) {
      generatedYaml = '';
      params.modelId = selectedModel;
      params.hfToken = hfToken;
      params.containerName = containerName;
      params.deployMode = deployMode;
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
    const indicator = document.getElementById('llm-step-indicator');
    const content = document.getElementById('llm-content');
    const footer = document.getElementById('llm-footer');

    if (indicator) indicator.innerHTML = renderStepDots();
    if (content) content.innerHTML = renderStepContent();
    if (footer) footer.innerHTML = renderFooterButtons();

    mountEvents();
  }

  // Walidacja biezacego kroku
  function validateStep() {
    switch (currentStep) {
      case 1:
        if (!selectedEngine) {
          App.showToast('Wybierz silnik inference', 'error');
          return false;
        }
        return true;

      case 2:
        if (!selectedModel || !selectedModel.trim()) {
          App.showToast('Wybierz lub wpisz model', 'error');
          return false;
        }
        return true;

      case 3: {
        if (!containerName || !containerName.trim()) {
          App.showToast('Podaj nazwe kontenera', 'error');
          return false;
        }
        const port = parseInt(params.port, 10);
        if (!port || port < 1 || port > 65535) {
          App.showToast('Podaj prawidlowy port (1-65535)', 'error');
          return false;
        }
        return true;
      }

      case 4:
        if (!generatedYaml.trim()) {
          App.showToast('YAML nie moze byc pusty', 'error');
          return false;
        }
        return true;

      default:
        return true;
    }
  }

  // Wykonanie wdrozenia przez mesh API
  async function executeDeploy() {
    isProcessing = true;
    if (selectedEngine === 'tensorrt-llm') {
      App.showToast('TensorRT-LLM — wdrazanie jeszcze niedostepne', 'warning');
      isProcessing = false;
      return;
    }
    const deployLogs = [];
    const deployStartTime = Date.now();
    let deployTimerInterval = null;
    const resultEl = document.getElementById('llm-result');
    const nextBtn = document.getElementById('llm-next');

    if (nextBtn) nextBtn.disabled = true;
    if (resultEl) {
      resultEl.innerHTML = `<div class="wizard-progress">Sprawdzanie portow...</div>`;
    }

    // Sprawdz zajete porty na hoscie
    const port = parseInt(params.port, 10) || 5010;
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

    const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const token = ApiClient.getToken();
    const wsUrl = `${protocol}//${location.host}/ws/deploy`;
    const ws = new WebSocket(wsUrl, token ? [`bearer.${token}`] : []);

    ws.onopen = () => {
      ws.send(JSON.stringify({
        node_id: nodeId,
        stack_name: containerName,
        compose_yaml: generatedYaml,
        service_name: `llm-${containerName}`,
        config_json: JSON.stringify({
          engine: selectedEngine,
          model_id: selectedModel,
          port: parseInt(params.port, 10) || 5010,
          container_name: containerName,
          deploy_mode: deployMode
        })
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
            App.showToast('Serwer LLM wdrozony', 'success');
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
    const existing = document.getElementById('llm-deploy-wizard-modal');
    if (existing) existing.remove();
  }

  return { open, close };
})();

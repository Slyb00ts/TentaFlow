// =============================================================================
// Plik: modules/catalog/ServiceCatalog.js
// Opis: Katalog uslug z kafelkami — otwierany z MeshNodeDetail lub Services.
// Przyklad: ServiceCatalog.show(nodeId, 'mesh');
// =============================================================================

const ServiceCatalog = (() => {
  'use strict';

  let currentNodeId = null;
  let sourceContext = null;
  let activeTab = 'tentaflow';
  let boundBackHandler = null;
  let cachedNodes = [];

  // Katalog uslug
  const SERVICES = [
    { id: 'tts', name: 'Text to Speech', desc: 'Synteza mowy z tekstu (Sherpa-ONNX)', gpu: true, defaultPort: 5020 },
    { id: 'stt', name: 'Speech to Text', desc: 'Transkrypcja audio na tekst (Whisper)', gpu: true, defaultPort: 5030 },
    { id: 'embeddings', name: 'Embeddings', desc: 'Generowanie wektorow z tekstu', gpu: true, defaultPort: 5050 },
    { id: 'reranker', name: 'Reranker', desc: 'Rerankowanie wynikow wyszukiwania', gpu: true, defaultPort: 5055 },
    { id: 'rag', name: 'RAG Engine', desc: 'Retrieval Augmented Generation', gpu: false, defaultPort: 5040 },
    { id: 'tools', name: 'Tools', desc: 'Narzedzia i integracje zewnetrzne', gpu: false, defaultPort: 5060 },
    { id: 'memory', name: 'Memory', desc: 'Pamiec konwersacji i kontekstu', gpu: false, defaultPort: 5002 },
    { id: 'comfyui', name: 'Image Generation', desc: 'Generowanie obrazow (ComfyUI)', gpu: true, defaultPort: 5000 },
    { id: 'meeting-bot', name: 'Meeting Bot', desc: 'Bot AI do spotkan Teams — dolacza jako uczestnik, transkrybuje, odpowiada', gpu: false, defaultPort: 5000 },
  ];

  const LLM_SERVICE = {
    id: 'llm',
    name: 'LLM Server',
    desc: 'Serwer modeli jezykowych',
    gpu: true,
    defaultPort: 5010,
  };

  // Otwarcie katalogu uslug
  function show(nodeId, context) {
    sourceContext = context || 'mesh';
    activeTab = 'tentaflow';

    if (sourceContext === 'services' && !nodeId) {
      currentNodeId = null;
      renderTargetSelector();
      return;
    }

    currentNodeId = nodeId;
    renderCatalog();
  }

  // Krok wyboru noda/clustra (wywolanie z Services)
  async function renderTargetSelector() {
    const content = document.getElementById('content');
    if (!content) return;

    const pageTitle = document.getElementById('page-title');
    if (pageTitle) pageTitle.textContent = 'Katalog uslug';

    content.innerHTML = `
      <div class="catalog-container">
        <div class="mesh-detail-topbar">
          <button class="btn btn-ghost btn-sm" id="btn-catalog-back">\u2190 ${I18n.t('common.back')}</button>
        </div>
        <div class="catalog-target-selector">
          <h3>Wybierz gdzie chcesz deployowac</h3>
          <div class="form-group">
            <select id="deploy-target" class="deploy-target-select">
              <option value="">-- Wybierz --</option>
            </select>
          </div>
          <p class="empty-state-hint" id="target-loading">${I18n.t('common.loading')}</p>
        </div>
      </div>
    `;

    mountBackButton();

    // Zaladuj nody i klastry
    let nodes = [];
    let clusters = [];
    try {
      const [nodesResp, clustersResp] = await Promise.all([
        ApiClient.get('/api/mesh/nodes').catch(() => []),
        ApiClient.get('/api/clusters').catch(() => []),
      ]);
      nodes = (nodesResp || []).filter(n => {
        return n.is_trusted === true || n.is_local === true;
      });
      cachedNodes = nodes;
      clusters = clustersResp || [];
    } catch (err) {
      console.error('Blad ladowania nodow/klastrow:', err);
    }

    const loadingEl = document.getElementById('target-loading');
    if (loadingEl) loadingEl.remove();

    const select = document.getElementById('deploy-target');
    if (!select) return;

    let optionsHtml = '<option value="">-- Wybierz --</option>';

    if (nodes.length > 0) {
      optionsHtml += '<optgroup label="Nodes">';
      nodes.forEach(n => {
        const nid = n.node_id || n.id;
        const label = n.hostname || n.name || nid;
        optionsHtml += `<option value="node:${Utils.escapeAttr(nid)}">${Utils.escapeHtml(label)}</option>`;
      });
      optionsHtml += '</optgroup>';
    }

    if (clusters.length > 0) {
      optionsHtml += '<optgroup label="Clusters">';
      clusters.forEach(c => {
        const cid = c.id || c.cluster_id;
        optionsHtml += `<option value="cluster:${Utils.escapeAttr(cid)}">${Utils.escapeHtml(c.name || cid)}</option>`;
      });
      optionsHtml += '</optgroup>';
    }

    select.innerHTML = optionsHtml;

    select.addEventListener('change', () => {
      const val = select.value;
      if (!val) return;
      const [type, id] = val.split(':');
      if (type === 'node' && id) {
        currentNodeId = id;
        renderCatalog();
      } else if (type === 'cluster' && id) {
        currentNodeId = id;
        renderCatalog();
      }
    });
  }

  // Renderowanie gridu katalogu
  function renderCatalog() {
    const content = document.getElementById('content');
    if (!content) return;

    const pageTitle = document.getElementById('page-title');
    if (pageTitle) pageTitle.textContent = 'Katalog uslug';

    content.innerHTML = `
      <div class="catalog-container">
        <div class="mesh-detail-topbar">
          <button class="btn btn-ghost btn-sm" id="btn-catalog-back">\u2190 ${I18n.t('common.back')}</button>
        </div>

        <div class="catalog-tabs">
          <button class="catalog-tab${activeTab === 'tentaflow' ? ' active' : ''}" data-tab="tentaflow">TentaFlow</button>
          <button class="catalog-tab${activeTab === 'containers' ? ' active' : ''}" data-tab="containers">${I18n.t('containers.tab') || 'TentaFlow Containers'}</button>
          <button class="catalog-tab${activeTab === 'nim' ? ' active' : ''}" data-tab="nim">NVIDIA NIM</button>
        </div>

        <div id="catalog-content">
          ${renderTabContent()}
        </div>
      </div>
    `;

    mountEvents();
  }

  // Tresc zakladki na podstawie activeTab
  function renderTabContent() {
    switch (activeTab) {
      case 'tentaflow': return renderTentaFlowTab();
      case 'containers': return renderContainersTab();
      case 'nim': return renderNimTab();
      default: return '';
    }
  }

  // Zakladka TentaFlow - grid wszystkich uslug (w tym LLM)
  function renderTentaFlowTab() {
    const cards = SERVICES.map(s => `
      <div class="catalog-card" data-service-id="${Utils.escapeAttr(s.id)}">
        <div class="catalog-card-header">
          <div class="catalog-card-icon">${CatalogIcons.get(s.id)}</div>
          <div>
            <div class="catalog-card-title">${Utils.escapeHtml(s.name)}</div>
            <div class="catalog-card-port">Port: ${Utils.escapeHtml(String(s.defaultPort))}</div>
          </div>
        </div>
        <div class="catalog-card-desc">${Utils.escapeHtml(s.desc)}</div>
        <div class="catalog-card-footer">
          <div class="catalog-card-badges">
            ${s.gpu
              ? '<span class="badge catalog-badge catalog-badge-gpu">GPU</span>'
              : '<span class="badge catalog-badge catalog-badge-cpu">CPU</span>'}
          </div>
          <button class="btn btn-primary btn-sm catalog-deploy-btn">Deploy</button>
        </div>
      </div>
    `).join('');

    const llmCard = `
      <div class="catalog-card" data-service-id="llm">
        <div class="catalog-card-header">
          <div class="catalog-card-icon">${CatalogIcons.get('llm')}</div>
          <div>
            <div class="catalog-card-title">${Utils.escapeHtml(LLM_SERVICE.name)}</div>
            <div class="catalog-card-port">Port: ${LLM_SERVICE.defaultPort}</div>
          </div>
        </div>
        <div class="catalog-card-desc">${Utils.escapeHtml(LLM_SERVICE.desc)}</div>
        <div class="catalog-card-footer">
          <div class="catalog-card-badges">
            <span class="badge catalog-badge catalog-badge-gpu">GPU</span>
          </div>
          <button class="btn btn-primary btn-sm catalog-deploy-btn">Configure &amp; Deploy</button>
        </div>
      </div>
    `;

    return `<div class="catalog-grid">${cards}${llmCard}</div>`;
  }

  // Stan katalogu NIM
  let nimContainers = [];
  let nimFilteredContainers = [];
  let nimActiveCategory = 'all';
  let nimSearchQuery = '';
  let nimSearchTimer = null;
  let nimLoaded = false;
  let nimError = null;

  // Sprawdza czy wybrany node ma NVIDIA GPU (w tym DGX Spark)
  function selectedNodeHasNvidiaGpu() {
    if (!currentNodeId) return false;
    const node = cachedNodes.find(n => (n.node_id || n.id) === currentNodeId);
    if (!node) return false;
    const gpus = node.gpu_info || node.gpus || [];
    return gpus.some(g => {
      const name = (g.name || '').toLowerCase();
      return name.includes('nvidia') || name.includes('geforce') ||
        name.includes('rtx') || name.includes('gtx') || name.includes('tesla') ||
        name.includes('a100') || name.includes('h100') || name.includes('h200') ||
        name.includes('l40') || name.includes('dgx') || name.includes('grace') ||
        name.includes('blackwell') || name.includes('hopper') || name.includes('gb10') ||
        name.includes('gh200') || name.includes('b200') || name.includes('b100');
    });
  }

  function getSelectedNodeGpuNames() {
    if (!currentNodeId) return [];
    const node = cachedNodes.find(n => (n.node_id || n.id) === currentNodeId);
    if (!node) return [];
    return (node.gpu_info || node.gpus || []).map(g => g.name || 'Unknown GPU');
  }

  // Zakladka NVIDIA NIM - kontener ladowania
  function renderNimTab() {
    return `
      <div id="nim-catalog-container">
        <div class="empty-state">
          <div class="empty-state-icon">${CatalogIcons.nvidia(48)}</div>
          <div class="empty-state-text">${I18n.t('nim.loading_catalog')}</div>
        </div>
      </div>
    `;
  }

  // Pobranie katalogu NIM z backendu
  async function loadNimCatalog() {
    const container = document.getElementById('nim-catalog-container');
    if (!container) return;

    // Sprawdz czy wybrany node ma NVIDIA GPU
    if (!selectedNodeHasNvidiaGpu()) {
      const gpuNames = getSelectedNodeGpuNames();
      const gpuList = gpuNames.length > 0
        ? gpuNames.map(n => Utils.escapeHtml(n)).join(', ')
        : I18n.t('nim.no_gpu_detected') || 'No GPU detected';
      container.innerHTML = `
        <div class="empty-state">
          <div class="empty-state-icon">${CatalogIcons.nvidia(48)}</div>
          <div class="empty-state-text">${I18n.t('nim.not_supported')}</div>
          <div class="empty-state-hint">${I18n.t('nim.not_supported_hint')}</div>
          <div class="empty-state-hint" style="margin-top:8px;font-family:var(--font-mono, monospace);font-size:var(--font-size-xs);color:var(--color-text-muted);">${gpuList}</div>
        </div>
      `;
      return;
    }

    try {
      const data = await ApiClient.get('/api/nim/catalog');

      if (data.error === 'ngc_api_key_not_configured') {
        nimError = 'no_api_key';
        container.innerHTML = renderNimNoApiKey();
        mountNimSettingsLink();
        return;
      }

      if (data.error === 'ngc_auth_failed') {
        nimError = 'auth_failed';
        container.innerHTML = renderNimAuthFailed();
        mountNimSettingsLink();
        return;
      }

      if (data.error) {
        nimError = data.error;
        container.innerHTML = `
          <div class="empty-state">
            <div class="empty-state-icon">${CatalogIcons.nvidia(48)}</div>
            <div class="empty-state-text">${Utils.escapeHtml(data.error)}</div>
          </div>
        `;
        return;
      }

      nimContainers = data.containers || [];
      nimLoaded = true;
      nimError = null;
      nimActiveCategory = 'all';
      nimSearchQuery = '';
      applyNimFilters();
      container.innerHTML = renderNimCatalogContent();
      mountNimEvents();
    } catch (err) {
      console.error('Blad ladowania katalogu NIM:', err);
      container.innerHTML = `
        <div class="empty-state">
          <div class="empty-state-icon">${CatalogIcons.nvidia(48)}</div>
          <div class="empty-state-text">${I18n.t('common.error')}</div>
          <div class="empty-state-hint">${Utils.escapeHtml(err.message || '')}</div>
        </div>
      `;
    }
  }

  // Ekran braku klucza NGC API
  function renderNimNoApiKey() {
    return `
      <div class="empty-state">
        <div class="empty-state-icon">${CatalogIcons.nvidia(48)}</div>
        <div class="empty-state-text">${I18n.t('nim.no_api_key')}</div>
        <div class="empty-state-hint">${I18n.t('nim.no_api_key_hint')}</div>
        <button class="btn btn-primary btn-sm nim-go-settings" style="margin-top: var(--spacing-md);">${I18n.t('nim.go_to_settings')}</button>
      </div>
    `;
  }

  // Ekran bledu autentykacji NGC
  function renderNimAuthFailed() {
    return `
      <div class="empty-state">
        <div class="empty-state-icon nim-warning-icon">
          <svg xmlns="http://www.w3.org/2000/svg" width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="var(--color-warning)" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z"/>
            <line x1="12" y1="9" x2="12" y2="13"/>
            <line x1="12" y1="17" x2="12.01" y2="17"/>
          </svg>
        </div>
        <div class="empty-state-text">${I18n.t('nim.auth_failed')}</div>
        <div class="empty-state-hint">${I18n.t('nim.auth_failed_hint')}</div>
        <button class="btn btn-primary btn-sm nim-go-settings" style="margin-top: var(--spacing-md);">${I18n.t('nim.go_to_settings')}</button>
      </div>
    `;
  }

  // Nawigacja do ustawien
  function mountNimSettingsLink() {
    const btn = document.querySelector('.nim-go-settings');
    if (btn) {
      btn.addEventListener('click', () => {
        cleanup();
        ViewRouter.navigate('settings');
      });
    }
  }

  // Filtrowanie kontenerow NIM
  function applyNimFilters() {
    nimFilteredContainers = nimContainers.filter(c => {
      if (nimActiveCategory !== 'all' && c.category !== nimActiveCategory) return false;
      if (nimSearchQuery) {
        const q = nimSearchQuery.toLowerCase();
        const name = (c.display_name || c.name || '').toLowerCase();
        const desc = (c.description || '').toLowerCase();
        const pub = (c.publisher || '').toLowerCase();
        if (!name.includes(q) && !desc.includes(q) && !pub.includes(q)) return false;
      }
      return true;
    });
  }

  // Kategorie NIM
  const NIM_CATEGORIES = ['all', 'llm', 'vlm', 'embedding', 'reranker', 'stt', 'tts'];

  // Etykiety kategorii
  function nimCategoryLabel(cat) {
    if (cat === 'all') return I18n.t('nim.category_all');
    return cat.toUpperCase();
  }

  // Kolor publishera
  function publisherColor(publisher) {
    const p = (publisher || '').toLowerCase();
    if (p === 'nvidia') return 'var(--color-success)';
    if (p === 'meta') return '#1877F2';
    if (p === 'mistralai') return '#FF7000';
    return 'var(--color-text-secondary)';
  }

  // Tresc katalogu NIM (toolbar + grid)
  function renderNimCatalogContent() {
    const toolbar = `
      <div class="nim-toolbar">
        <input type="text" class="nim-search" placeholder="${I18n.t('nim.search')}" value="${Utils.escapeAttr(nimSearchQuery)}">
        <div class="nim-filters">
          ${NIM_CATEGORIES.map(cat => `
            <button class="nim-filter${nimActiveCategory === cat ? ' active' : ''}" data-category="${cat}">${nimCategoryLabel(cat)}</button>
          `).join('')}
        </div>
      </div>
    `;

    if (nimFilteredContainers.length === 0) {
      return toolbar + `
        <div class="empty-state" style="margin-top: var(--spacing-xl);">
          <div class="empty-state-text">${I18n.t('nim.no_results')}</div>
        </div>
      `;
    }

    const cards = nimFilteredContainers.map(c => {
      const color = publisherColor(c.publisher);
      const vram = c.min_gpu_memory_gb ? `${c.min_gpu_memory_gb} GB ${I18n.t('nim.vram')}` : '';
      return `
        <div class="catalog-card nim-card" data-nim-image="${Utils.escapeAttr(c.image)}" data-nim-name="${Utils.escapeAttr(c.name)}">
          <div class="catalog-card-header">
            <div class="catalog-card-icon nim-card-icon">${CatalogIcons.nvidia(24)}</div>
            <div>
              <div class="catalog-card-title">${Utils.escapeHtml(c.display_name || c.name)}</div>
              <span class="nim-publisher-badge" style="background: ${color}15; color: ${color}; border: 1px solid ${color}40;">${Utils.escapeHtml(c.publisher || '')}</span>
            </div>
          </div>
          <div class="catalog-card-desc nim-card-desc">${Utils.escapeHtml(c.description || '')}</div>
          <div class="catalog-card-footer">
            <div class="catalog-card-badges">
              <span class="badge catalog-badge nim-badge-category">${Utils.escapeHtml((c.category || '').toUpperCase())}</span>
              ${vram ? `<span class="badge catalog-badge nim-badge-vram">${vram}</span>` : ''}
              ${c.latest_tag ? `<span class="badge catalog-badge nim-badge-version">v${Utils.escapeHtml(c.latest_tag)}</span>` : ''}
            </div>
            <button class="btn btn-primary btn-sm nim-deploy-btn">${I18n.t('nim.deploy')}</button>
          </div>
        </div>
      `;
    }).join('');

    return toolbar + `<div class="catalog-grid">${cards}</div>`;
  }

  // Podpiecie zdarzen katalogu NIM
  function mountNimEvents() {
    // Wyszukiwanie z debounce
    const searchInput = document.querySelector('.nim-search');
    if (searchInput) {
      searchInput.addEventListener('input', () => {
        clearTimeout(nimSearchTimer);
        nimSearchTimer = setTimeout(() => {
          nimSearchQuery = searchInput.value.trim();
          applyNimFilters();
          refreshNimGrid();
        }, 300);
      });
    }

    // Filtry kategorii
    const filterBtns = document.querySelectorAll('.nim-filter[data-category]');
    filterBtns.forEach(btn => {
      btn.addEventListener('click', () => {
        nimActiveCategory = btn.dataset.category;
        applyNimFilters();
        refreshNimGrid();
      });
    });

    // Deploy na kartach
    mountNimDeployButtons();
  }

  // Podpiecie przyciskow deploy NIM
  function mountNimDeployButtons() {
    const cards = document.querySelectorAll('.nim-card[data-nim-image]');
    cards.forEach(card => {
      const btn = card.querySelector('.nim-deploy-btn');
      const handler = () => {
        const imageName = card.dataset.nimImage;
        const containerName = card.dataset.nimName;
        const container = nimContainers.find(c => c.image === imageName);
        if (container) {
          openNimDeployModal(container);
        }
      };
      if (btn) {
        btn.addEventListener('click', (e) => {
          e.stopPropagation();
          handler();
        });
      }
      card.addEventListener('click', handler);
    });
  }

  // Odswiezenie gridu NIM (bez przeladowania toolbara)
  function refreshNimGrid() {
    const container = document.getElementById('nim-catalog-container');
    if (!container) return;
    container.innerHTML = renderNimCatalogContent();
    mountNimEvents();
  }

  // Modal deploy NIM
  async function openNimDeployModal(container) {
    const existingModal = document.getElementById('nim-deploy-modal');
    if (existingModal) existingModal.remove();

    // Pobierz nody i GPU
    let nodes = [];
    try {
      const nodesResp = await ApiClient.get('/api/mesh/nodes');
      nodes = (nodesResp || []).filter(n => {
        return n.is_trusted === true || n.is_local === true;
      });
    } catch {}

    // Jesli mamy juz wybrany node z kontekstu, uzyj go
    const preselectedNode = currentNodeId || '';

    const overlay = document.createElement('div');
    overlay.className = 'modal-overlay active';
    overlay.id = 'nim-deploy-modal';

    overlay.innerHTML = `
      <div class="modal" style="max-width: 640px;">
        <div class="modal-header">
          <h3>Deploy: ${Utils.escapeHtml(container.display_name || container.name)}</h3>
          <button class="modal-close" id="nim-modal-close">&times;</button>
        </div>
        <div class="modal-body">
          <div class="wizard-summary" style="margin-bottom: var(--spacing-lg);">
            <div class="wizard-summary-row">
              <span class="wizard-summary-label">Image:</span>
              <span class="wizard-summary-value" style="font-size: var(--font-size-xs); font-family: monospace;">${Utils.escapeHtml(container.image)}:${Utils.escapeHtml(container.latest_tag || 'latest')}</span>
            </div>
            ${container.min_gpu_memory_gb ? `<div class="wizard-summary-row">
              <span class="wizard-summary-label">${I18n.t('nim.vram')}:</span>
              <span class="wizard-summary-value">${container.min_gpu_memory_gb} GB</span>
            </div>` : ''}
          </div>

          <div class="deploy-param-grid">
            <div class="form-group">
              <label for="nim-target-node">Node</label>
              <select id="nim-target-node">
                ${nodes.map(n => {
                  const nid = n.node_id || n.id;
                  const label = n.hostname || n.name || nid;
                  const selected = nid === preselectedNode ? 'selected' : '';
                  return `<option value="${Utils.escapeAttr(nid)}" ${selected}>${Utils.escapeHtml(label)}</option>`;
                }).join('')}
              </select>
            </div>
            <div class="form-group">
              <label for="nim-port">Port</label>
              <input type="number" id="nim-port" value="8000" min="1" max="65535">
            </div>
            <div class="form-group">
              <label for="nim-gpu">GPU</label>
              <select id="nim-gpu">
                <option value="all">All GPUs</option>
                <option value="0">GPU 0</option>
              </select>
            </div>
            <div class="form-group">
              <label for="nim-container-name">Container name</label>
              <input type="text" id="nim-container-name" value="nim-${Utils.escapeAttr(container.name)}">
            </div>
          </div>

          <div class="form-group" style="margin-top: var(--spacing-md);">
            <label>Environment variables</label>
            <div id="nim-env-vars">
              <div class="nim-env-row" style="display: flex; gap: var(--spacing-sm); margin-bottom: var(--spacing-xs);">
                <input type="text" class="nim-env-key" placeholder="Key" style="flex: 1;">
                <input type="text" class="nim-env-value" placeholder="Value" style="flex: 2;">
              </div>
            </div>
            <button class="btn btn-ghost btn-sm" id="nim-add-env" style="margin-top: var(--spacing-xs);">+ Add variable</button>
          </div>

          <div id="nim-deploy-result"></div>
        </div>
        <div class="modal-footer">
          <button class="btn btn-ghost btn-sm" id="nim-modal-cancel">${I18n.t('common.cancel')}</button>
          <button class="btn btn-primary btn-sm" id="nim-modal-deploy">${I18n.t('nim.deploy')}</button>
        </div>
      </div>
    `;

    document.body.appendChild(overlay);

    // Zaladuj GPU info po wybraniu noda
    const nodeSelect = document.getElementById('nim-target-node');
    const gpuSelect = document.getElementById('nim-gpu');

    async function loadNodeGpus(nodeIdVal) {
      if (!nodeIdVal) return;
      try {
        const nodeData = await ApiClient.get(`/api/mesh/nodes/${encodeURIComponent(nodeIdVal)}`);
        if (nodeData && Array.isArray(nodeData.gpu_info) && nodeData.gpu_info.length > 0) {
          let opts = '<option value="all">All GPUs</option>';
          nodeData.gpu_info.forEach(g => {
            opts += `<option value="${g.index}">GPU ${g.index}: ${Utils.escapeHtml(g.name)} (${Math.round(g.vram_total_mb / 1024)} GB)</option>`;
          });
          gpuSelect.innerHTML = opts;
        }
      } catch {}
    }

    if (preselectedNode) loadNodeGpus(preselectedNode);
    else if (nodes.length > 0) loadNodeGpus(nodes[0].node_id || nodes[0].id);

    nodeSelect.addEventListener('change', () => loadNodeGpus(nodeSelect.value));

    // Dodaj nowy wiersz env
    document.getElementById('nim-add-env').addEventListener('click', () => {
      const envContainer = document.getElementById('nim-env-vars');
      const row = document.createElement('div');
      row.className = 'nim-env-row';
      row.style.cssText = 'display: flex; gap: var(--spacing-sm); margin-bottom: var(--spacing-xs);';
      row.innerHTML = `
        <input type="text" class="nim-env-key" placeholder="Key" style="flex: 1;">
        <input type="text" class="nim-env-value" placeholder="Value" style="flex: 2;">
      `;
      envContainer.appendChild(row);
    });

    // Zamkniecie modala
    const closeModal = () => overlay.remove();
    document.getElementById('nim-modal-close').addEventListener('click', closeModal);
    document.getElementById('nim-modal-cancel').addEventListener('click', closeModal);

    // Deploy
    document.getElementById('nim-modal-deploy').addEventListener('click', () => {
      executeNimDeploy(container, overlay);
    });
  }

  // Wykonanie deploy NIM
  async function executeNimDeploy(container, overlay) {
    const deployBtn = document.getElementById('nim-modal-deploy');
    const resultEl = document.getElementById('nim-deploy-result');
    if (deployBtn) deployBtn.disabled = true;

    const targetNodeId = document.getElementById('nim-target-node').value;
    const port = parseInt(document.getElementById('nim-port').value, 10) || 8000;
    const gpuId = document.getElementById('nim-gpu').value;
    const rawName = document.getElementById('nim-container-name').value.trim() || `nim-${container.name}`;
    const containerName = rawName.toLowerCase().replace(/[^a-z0-9_-]/g, '-').replace(/-+/g, '-').replace(/^-|-$/g, '');

    if (!targetNodeId) {
      App.showToast('Wybierz node docelowy', 'error');
      if (deployBtn) deployBtn.disabled = false;
      return;
    }

    // Zbierz zmienne srodowiskowe
    const envVars = {};
    const envRows = document.querySelectorAll('.nim-env-row');
    envRows.forEach(row => {
      const key = row.querySelector('.nim-env-key').value.trim();
      const val = row.querySelector('.nim-env-value').value;
      if (key) envVars[key] = val;
    });

    // GPU config
    const gpuDevices = gpuId === 'all' ? '' : gpuId;
    const envBlock = Object.entries(envVars)
      .map(([k, v]) => `      ${k}: "${v}"`)
      .join('\n');

    const tag = container.latest_tag || 'latest';
    const image = `${container.image}:${tag}`;

    // Wygeneruj YAML
    const yaml = `services:
  ${containerName}:
    image: ${image}
    container_name: ${containerName}
    ports:
      - "${port}:8000"
    environment:
      NGC_API_KEY: "\${NGC_API_KEY}"
${gpuDevices ? `      NVIDIA_VISIBLE_DEVICES: "${gpuDevices}"` : ''}
${envBlock ? '\n' + envBlock : ''}
    deploy:
      resources:
        reservations:
          devices:
            - driver: nvidia
              count: ${gpuId === 'all' ? 'all' : '1'}
              capabilities: [gpu]
    restart: unless-stopped
`;

    const deployLogs = [];
    const deployStartTime = Date.now();
    let deployTimerInterval = null;

    if (resultEl) {
      resultEl.innerHTML = DeployUtils.renderDeployProgress('connecting', 'Laczenie...', null, deployLogs, deployStartTime);
    }

    deployTimerInterval = setInterval(() => {
      if (resultEl) {
        const timerEl = resultEl.querySelector('.deploy-timer');
        if (timerEl) {
          const elapsed = ((Date.now() - deployStartTime) / 1000).toFixed(0);
          timerEl.textContent = elapsed + 's';
        }
      }
    }, 1000);

    const stackName = containerName;
    const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const token = ApiClient.getToken();
    const wsUrl = `${protocol}//${location.host}/ws/deploy`;
    const ws = new WebSocket(wsUrl, token ? [`bearer.${token}`] : []);

    ws.onopen = () => {
      ws.send(JSON.stringify({
        node_id: targetNodeId,
        stack_name: stackName,
        compose_yaml: yaml,
        service_name: stackName,
        config_json: JSON.stringify({
          engine: 'nim',
          model_id: container.name,
          port: port,
          container_name: containerName,
          service_type: container.category || 'llm',
          image: image,
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
            App.showToast(`${Utils.escapeHtml(container.display_name || container.name)} deployed`, 'success');
            setTimeout(() => overlay.remove(), 2000);
          } else {
            const errMsg = msg.error || 'Unknown error';
            // EULA error — pokaz link do akceptacji i przycisk Retry
            if (errMsg.includes('EULA') || errMsg.includes('accept license') || errMsg.includes('Get Container')) {
              const modelPath = container.name.replace('/', '%2F');
              const eulaUrl = `https://build.nvidia.com/${container.name}`;
              if (resultEl) {
                resultEl.innerHTML = `
                  <div style="background:var(--color-warning-light);border:1px solid var(--color-warning);border-radius:var(--radius-md);padding:16px;margin-top:12px;">
                    <div style="font-weight:600;margin-bottom:8px;color:var(--color-warning);">License acceptance required</div>
                    <div style="margin-bottom:12px;color:var(--color-text-secondary);font-size:var(--font-size-sm);">
                      NVIDIA requires you to accept the model license before downloading the container.
                    </div>
                    <div style="display:flex;gap:8px;flex-wrap:wrap;">
                      <a href="${eulaUrl}" target="_blank" rel="noopener" class="btn btn-primary btn-sm"
                         style="text-decoration:none;">Accept License on NVIDIA ↗</a>
                      <button class="btn btn-secondary btn-sm" id="nim-retry-deploy">Retry Deploy</button>
                    </div>
                  </div>
                `;
                document.getElementById('nim-retry-deploy')?.addEventListener('click', () => {
                  executeNimDeploy(container, overlay);
                });
              }
            } else {
              App.showToast(`Error: ${errMsg}`, 'error');
            }
          }
          if (deployBtn) deployBtn.disabled = false;
        }
      } catch {}
    };

    ws.onerror = () => {
      clearInterval(deployTimerInterval);
      if (resultEl) {
        resultEl.innerHTML = `<div class="deploy-done-msg deploy-done--fail">WebSocket connection error</div>`;
      }
      App.showToast('WebSocket connection error', 'error');
      if (deployBtn) deployBtn.disabled = false;
    };

    ws.onclose = () => {
      clearInterval(deployTimerInterval);
      if (deployBtn) deployBtn.disabled = false;
    };
  }

  // Podpiecie zdarzen
  function mountEvents() {
    mountBackButton();
    mountTabEvents();

    // Zakladki
    const tabs = document.querySelectorAll('.catalog-tab[data-tab]');
    tabs.forEach(tab => {
      tab.addEventListener('click', () => {
        activeTab = tab.dataset.tab;
        updateTabs();
        updateContent();
      });
    });
  }

  // Przycisk powrotu
  function mountBackButton() {
    const backBtn = document.getElementById('btn-catalog-back');
    if (backBtn) {
      if (boundBackHandler) backBtn.removeEventListener('click', boundBackHandler);
      boundBackHandler = handleBack;
      backBtn.addEventListener('click', boundBackHandler);
    }
  }

  // Obsluga powrotu — zalezy od kontekstu
  function handleBack() {
    cleanup();
    if (sourceContext === 'mesh' && currentNodeId) {
      MeshNodeDetail.show(currentNodeId);
    } else if (sourceContext === 'services') {
      ViewRouter.navigate('services');
    } else {
      ViewRouter.navigate('mesh');
    }
  }

  // Podpiecie zdarzen wewnatrz aktywnej zakladki
  function mountTabEvents() {
    if (activeTab === 'nim') {
      loadNimCatalog();
      return;
    }
    if (activeTab === 'containers') {
      loadContainersCatalog();
      return;
    }
    if (activeTab === 'tentaflow') {
      const cards = document.querySelectorAll('.catalog-card[data-service-id]');
      cards.forEach(card => {
        const btn = card.querySelector('.catalog-deploy-btn');
        const handler = () => {
          const serviceId = card.dataset.serviceId;
          if (serviceId === 'llm') {
            LLMDeployWizard.open(currentNodeId);
          } else if (serviceId === 'stt') {
            SttDeployWizard.open(currentNodeId);
          } else {
            const service = SERVICES.find(s => s.id === serviceId);
            if (service) {
              ServiceDeployModal.open(currentNodeId, service);
            }
          }
        };
        if (btn) {
          btn.addEventListener('click', (e) => {
            e.stopPropagation();
            handler();
          });
        }
        card.addEventListener('click', handler);
      });
    }
  }

  // Aktualizacja klas aktywnych na zakladkach
  function updateTabs() {
    const tabs = document.querySelectorAll('.catalog-tab[data-tab]');
    tabs.forEach(tab => {
      if (tab.dataset.tab === activeTab) {
        tab.classList.add('active');
      } else {
        tab.classList.remove('active');
      }
    });
  }

  // Przerenderowanie zawartosci zakladki
  function updateContent() {
    const contentEl = document.getElementById('catalog-content');
    if (contentEl) {
      contentEl.innerHTML = renderTabContent();
      mountTabEvents();
    }
  }

  // Czyszczenie stanu
  function cleanup() {
    const backBtn = document.getElementById('btn-catalog-back');
    if (backBtn && boundBackHandler) {
      backBtn.removeEventListener('click', boundBackHandler);
    }
    boundBackHandler = null;
  }

  // ===========================================================================
  // ZAKLADKA: TentaFlow Containers — embedowane kontenery z bundle binarki
  // ===========================================================================

  let containersList = [];
  let containersLoaded = false;
  let containersError = null;
  let containersCategory = 'all';

  function renderContainersTab() {
    const loading = !containersLoaded;
    const empty = containersLoaded && containersList.length === 0;
    const cats = ['all', 'llm', 'stt', 'tts', 'embeddings', 'reranker', 'image', 'meeting', 'other'];

    const filterButtons = cats
      .map(c => `
        <button class="btn btn-ghost btn-sm catalog-cat-btn${containersCategory === c ? ' active' : ''}"
                data-cat="${c}">${(I18n.t('containers.cat_' + c)) || c}</button>
      `).join('');

    const filtered = containersCategory === 'all'
      ? containersList
      : containersList.filter(c => c.category === containersCategory);

    const cards = filtered.map(c => `
      <div class="catalog-card" data-container-name="${Utils.escapeAttr(c.name)}">
        <div class="catalog-card-header">
          <div class="catalog-card-icon">${CatalogIcons.get(c.category) || CatalogIcons.get(c.name) || ''}</div>
          <div>
            <div class="catalog-card-title">${Utils.escapeHtml(c.name)}</div>
            <div class="catalog-card-port">${Utils.escapeHtml(c.category)}</div>
          </div>
        </div>
        <div class="catalog-card-desc">${Utils.escapeHtml(c.description)}</div>
        <div class="catalog-card-footer">
          <div class="catalog-card-badges">
            <span class="badge catalog-badge catalog-badge-gpu">GPU</span>
          </div>
          <button class="btn btn-primary btn-sm catalog-deploy-container-btn">${I18n.t('catalog.deploy') || 'Deploy'}</button>
        </div>
      </div>
    `).join('');

    return `
      <div class="nim-toolbar">
        <div class="nim-filters">${filterButtons}</div>
      </div>
      ${loading ? `<div class="empty-state"><div class="empty-state-text">${I18n.t('common.loading')}</div></div>` : ''}
      ${empty ? `<div class="empty-state"><div class="empty-state-text">${I18n.t('containers.empty') || 'Brak embedowanych kontenerow'}</div></div>` : ''}
      ${containersError ? `<div class="empty-state"><div class="empty-state-text">${Utils.escapeHtml(containersError)}</div></div>` : ''}
      <div class="catalog-grid">${cards}</div>
    `;
  }

  async function loadContainersCatalog() {
    // Filter buttons
    document.querySelectorAll('.catalog-cat-btn').forEach(btn => {
      btn.addEventListener('click', () => {
        containersCategory = btn.dataset.cat;
        updateContent();
      });
    });
    // Deploy buttons
    document.querySelectorAll('.catalog-card[data-container-name]').forEach(card => {
      const btn = card.querySelector('.catalog-deploy-container-btn');
      const handler = () => openContainerDeploy(card.dataset.containerName);
      if (btn) btn.addEventListener('click', e => { e.stopPropagation(); handler(); });
      card.addEventListener('click', handler);
    });

    if (containersLoaded) return;
    try {
      const data = await ApiClient.get('/api/deploy/containers');
      containersList = Array.isArray(data) ? data : [];
      containersLoaded = true;
      updateContent();
    } catch (err) {
      containersError = err.message || 'blad';
      containersLoaded = true;
      updateContent();
    }
  }

  function openContainerDeploy(name) {
    const container = containersList.find(c => c.name === name);
    if (!container) return;

    const portsHint = container.category === 'meeting' ? '5000:5000/udp,5900:5900,6080:6080' : '5000:5000/udp';
    const html = `
      <div class="modal-backdrop" id="container-deploy-backdrop">
        <div class="modal" style="max-width: 560px;">
          <div class="modal-header">
            <h3>${I18n.t('containers.deploy_title') || 'Deploy kontenera'}: ${Utils.escapeHtml(name)}</h3>
            <button class="btn btn-ghost btn-sm" id="cd-close">&times;</button>
          </div>
          <div class="modal-body">
            <div class="form-group">
              <label>Instance name</label>
              <input id="cd-name" class="form-input" value="tentaflow-${Utils.escapeAttr(name)}">
            </div>
            <div class="form-group">
              <label>Ports (host:container/proto, comma)</label>
              <input id="cd-ports" class="form-input" value="${portsHint}">
            </div>
            <div class="form-group">
              <label>Volumes (host:container, comma)</label>
              <input id="cd-volumes" class="form-input" placeholder="/data/models:/data/models">
            </div>
            <div class="form-group">
              <label>Env (KEY=VAL, comma)</label>
              <textarea id="cd-env" class="form-input" rows="3" placeholder="MODEL=speakleash/Bielik-11B-v2.6-Instruct-AWQ"></textarea>
            </div>
            <div class="form-group">
              <label><input type="checkbox" id="cd-gpu" checked> ${I18n.t('containers.use_gpu') || 'Uzyj GPU'}</label>
            </div>
          </div>
          <div class="modal-footer">
            <button class="btn btn-secondary" id="cd-cancel">${I18n.t('common.cancel') || 'Anuluj'}</button>
            <button class="btn btn-primary" id="cd-deploy">${I18n.t('catalog.deploy') || 'Deploy'}</button>
          </div>
        </div>
      </div>
    `;
    const wrap = document.createElement('div');
    wrap.innerHTML = html;
    document.body.appendChild(wrap);

    const close = () => wrap.remove();
    document.getElementById('cd-close').onclick = close;
    document.getElementById('cd-cancel').onclick = close;

    document.getElementById('cd-deploy').onclick = async () => {
      const ports = parsePairs(document.getElementById('cd-ports').value, ':');
      const volumes = parsePairs(document.getElementById('cd-volumes').value, ':');
      const env = parseEnv(document.getElementById('cd-env').value);
      const body = {
        instance_name: document.getElementById('cd-name').value.trim() || null,
        ports,
        volumes,
        env,
        gpu: document.getElementById('cd-gpu').checked,
      };
      try {
        const resp = await ApiClient.post(`/api/deploy/${encodeURIComponent(name)}`, body);
        App.showToast(`${name}: ${resp.container || 'deployed'}`, 'success');
        close();
      } catch (err) {
        App.showToast(`Deploy nieudany: ${err.message}`, 'error');
      }
    };
  }

  function parsePairs(value, sep) {
    return value.split(',')
      .map(s => s.trim()).filter(Boolean)
      .map(p => {
        const idx = p.indexOf(sep);
        if (idx < 0) return null;
        return [p.slice(0, idx).trim(), p.slice(idx + 1).trim()];
      })
      .filter(Boolean);
  }

  function parseEnv(value) {
    const out = {};
    value.split(/[\n,]/).map(s => s.trim()).filter(Boolean).forEach(line => {
      const eq = line.indexOf('=');
      if (eq < 0) return;
      out[line.slice(0, eq).trim()] = line.slice(eq + 1).trim();
    });
    return out;
  }

  return { show, cleanup };
})();

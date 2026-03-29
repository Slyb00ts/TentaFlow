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
        const trust = (n.trust_status || n.status || '').toLowerCase();
        return trust === 'trusted' || trust === 'paired' || trust === 'local' || n.is_local;
      });
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
        const localTag = n.is_local ? ' (local)' : '';
        optionsHtml += `<option value="node:${Utils.escapeAttr(nid)}">${Utils.escapeHtml(label)}${localTag}</option>`;
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

  // Zakladka NVIDIA NIM - placeholder
  function renderNimTab() {
    return `
      <div class="empty-state catalog-nim-placeholder">
        <div class="empty-state-icon">${CatalogIcons.nvidia(48)}</div>
        <div class="empty-state-text">NVIDIA NIM</div>
        <div class="empty-state-hint">Wkrotce dostepne</div>
      </div>
    `;
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
    if (activeTab === 'tentaflow') {
      const cards = document.querySelectorAll('.catalog-card[data-service-id]');
      cards.forEach(card => {
        const btn = card.querySelector('.catalog-deploy-btn');
        const handler = () => {
          const serviceId = card.dataset.serviceId;
          if (serviceId === 'llm') {
            LLMDeployWizard.open(currentNodeId);
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

  return { show, cleanup };
})();

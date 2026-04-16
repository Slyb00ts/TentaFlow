// =============================================================================
// Plik: modules/catalog/EngineDeployWizard.js
// Opis: Wizard wdrażania silników AI dla nowego (uproszczonego) schematu
//       manifestu. Trzy kroki: 1) wybór trybu deploymentu (docker/native/external)
//       dostępnego dla platformy hosta, 2) wybór modelu (preset z manifestu lub
//       wyszukiwarka HuggingFace), 3) konfiguracja runtime (port, ekstra param).
// Przykład: EngineDeployWizard.open('vllm');
// =============================================================================

const EngineDeployWizard = (() => {
  'use strict';

  let currentStep = 1;
  let engineId = null;
  let engineEntry = null;
  let nodes = [];
  let hostOs = 'linux';
  let availableMethods = [];
  let hfToken = '';
  let modelSourceMode = 'preset';
  let hfSearchTimer = null;
  let hfResults = [];
  let hfSearching = false;
  let hfSearchQuery = '';

  let selection = {
    nodeId: null,
    deployMethod: null,
    modelPresetId: null,
    modelRepo: null,
    port: null,
    containerName: null
  };

  // Otwarcie wizarda dla danego silnika.
  // opts.nodeId — preselekcja węzła (np. z MeshNodeDetail).
  async function open(engId, opts) {
    engineId = engId;
    engineEntry = null;
    currentStep = 1;
    modelSourceMode = 'preset';
    hfResults = [];
    hfSearchQuery = '';
    selection = {
      nodeId: (opts && opts.nodeId) || null,
      deployMethod: null,
      modelPresetId: null,
      modelRepo: null,
      port: null,
      containerName: null
    };

    renderModalShell(`<div class="form-hint">${I18n.t('common.loading')}</div>`);

    try {
      await ManifestStore.init();
    } catch (err) {
      console.error('[EngineDeployWizard] init manifest blad:', err);
    }

    engineEntry = ManifestStore.byId(engineId);
    if (!engineEntry) {
      const msg = I18n.t('wizard.engineNotFound').replace('{id}', Utils.escapeHtml(engineId || ''));
      renderModalShell(`<div class="form-hint">${msg}</div>`);
      return;
    }

    try {
      nodes = await fetchNodes();
    } catch (err) {
      console.error('[EngineDeployWizard] fetchNodes blad:', err);
      nodes = [];
    }

    if (!selection.nodeId) {
      const local = nodes.find(n => n && n.is_local === true) || nodes[0];
      selection.nodeId = local ? (local.node_id || local.id) : null;
    }

    hostOs = pickHostOsFromNode(selection.nodeId);
    availableMethods = ManifestStore.availableDeployMethods(engineEntry, hostOs);

    if (availableMethods.length > 0) {
      selection.deployMethod = availableMethods[0];
    }

    const eng = engineEntry.engine || {};
    selection.port = eng.default_port || 8080;
    selection.containerName = `tentaflow-${(eng.id || 'svc').toLowerCase()}-${randomSuffix()}`;

    const presets = ManifestStore.modelPresets(engineEntry);
    if (presets.length > 0) {
      const rec = presets.find(p => p && p.recommended) || presets[0];
      if (rec) selection.modelPresetId = rec.id;
    } else {
      // Brak presetow — od razu otworz wyszukiwarke HF.
      modelSourceMode = 'hf';
    }

    try {
      if (typeof ApiClient !== 'undefined' && typeof ApiClient.get === 'function') {
        const settings = await ApiClient.get('/api/settings').catch(() => null);
        if (Array.isArray(settings)) {
          const t = settings.find(s => s && s.key === 'hf_token');
          if (t && t.value) hfToken = String(t.value);
        }
      }
    } catch {
      hfToken = '';
    }

    refreshModal();
  }

  function close() {
    const el = document.getElementById('engine-deploy-wizard');
    if (el) el.remove();
  }

  function randomSuffix(len) {
    const chars = 'abcdefghijklmnopqrstuvwxyz0123456789';
    let r = '';
    for (let i = 0; i < (len || 5); i++) {
      r += chars[Math.floor(Math.random() * chars.length)];
    }
    return r;
  }

  // Pobiera listę węzłów z mesh API z fallbackiem na "local".
  async function fetchNodes() {
    try {
      let resp;
      if (typeof ApiClient !== 'undefined' && typeof ApiClient.get === 'function') {
        resp = await ApiClient.get('/api/mesh/nodes');
      } else {
        const r = await fetch('/api/mesh/nodes');
        if (!r.ok) throw new Error('HTTP ' + r.status);
        resp = await r.json();
      }
      if (Array.isArray(resp) && resp.length > 0) {
        return resp.filter(n => n && (n.is_trusted === true || n.is_local === true));
      }
    } catch (err) {
      console.warn('[EngineDeployWizard] fetchNodes fallback:', err);
    }
    return [{
      node_id: 'local', id: 'local', name: 'Local', is_local: true,
      os: defaultUaOs()
    }];
  }

  function defaultUaOs() {
    const ua = (navigator.userAgent || '').toLowerCase();
    if (ua.indexOf('mac') !== -1) return 'macos';
    if (ua.indexOf('win') !== -1) return 'windows';
    return 'linux';
  }

  function pickHostOsFromNode(nodeId) {
    const node = nodes.find(n => n && (n.node_id || n.id) === nodeId);
    if (!node) return defaultUaOs();
    const os = node.os || (node.platform && node.platform.os);
    return os ? String(os).toLowerCase() : defaultUaOs();
  }

  // ---- Render modala -------------------------------------------------------

  // Tworzy szkielet modala (overlay + modal + header + body + footer).
  function renderModalShell(bodyHtml) {
    close();
    const overlay = document.createElement('div');
    overlay.className = 'modal-overlay active';
    overlay.id = 'engine-deploy-wizard';
    overlay.innerHTML =
      '<div class="modal" style="max-width: 720px;">' +
        '<div class="modal-header">' +
          '<h3 id="edw-title">' + I18n.t('wizard.title') + '</h3>' +
          '<button class="modal-close" id="edw-close" aria-label="' + I18n.t('common.close') + '">&times;</button>' +
        '</div>' +
        '<div class="modal-body" id="edw-body">' + bodyHtml + '</div>' +
        '<div class="modal-footer" id="edw-footer"></div>' +
      '</div>';
    document.body.appendChild(overlay);
    const closeBtn = document.getElementById('edw-close');
    if (closeBtn) closeBtn.addEventListener('click', close);
    overlay.addEventListener('click', (e) => { if (e.target === overlay) close(); });
  }

  function refreshModal() {
    const titleEl = document.getElementById('edw-title');
    if (titleEl && engineEntry && engineEntry.engine) {
      titleEl.textContent = I18n.t('wizard.title') + ': ' + (engineEntry.engine.name || engineEntry.engine.id);
    }
    const body = document.getElementById('edw-body');
    if (body) body.innerHTML = renderStepIndicator() + renderStepBody();
    const footer = document.getElementById('edw-footer');
    if (footer) footer.innerHTML = renderFooter();
    bindStepInputs();
    bindFooter();
  }

  function renderStepIndicator() {
    const total = 3;
    let html = '<div class="wizard-step-indicator">';
    for (let i = 1; i <= total; i++) {
      const cls = i === currentStep ? 'active' : i < currentStep ? 'done' : '';
      html += '<div class="wizard-step-dot ' + cls + '"></div>';
    }
    html += '</div>';
    return html;
  }

  function renderStepBody() {
    switch (currentStep) {
      case 1: return renderStepMethod();
      case 2: return renderStepModel();
      case 3: return renderStepRuntime();
      default: return '';
    }
  }

  function renderFooter() {
    let html = '<button class="btn btn-ghost btn-sm" id="edw-cancel">' + I18n.t('common.cancel') + '</button>';
    if (currentStep > 1) {
      html += '<button class="btn btn-secondary btn-sm" id="edw-back">\u2190 ' + I18n.t('common.back') + '</button>';
    }
    if (currentStep < 3) {
      html += '<button class="btn btn-primary btn-sm" id="edw-next">' + I18n.t('common.next') + ' \u2192</button>';
    } else {
      html += '<button class="btn btn-primary btn-sm" id="edw-deploy">' + I18n.t('wizard.startDeploy') + '</button>';
    }
    return html;
  }

  // ---- Krok 1: wybor trybu deploymentu ------------------------------------

  function renderStepMethod() {
    if (!availableMethods || availableMethods.length === 0) {
      const msg = I18n.t('wizard.noMethodsAvailable').replace('{os}', Utils.escapeHtml(hostOs));
      return '<h4>' + I18n.t('wizard.selectMethod') + '</h4>' +
        '<p class="form-hint">' + msg + '</p>';
    }

    const cards = availableMethods.map(m => {
      const sel = selection.deployMethod === m ? ' selected' : '';
      const name = I18n.t('wizard.method.' + m);
      const desc = I18n.t('wizard.method.' + m + 'Desc');
      return '<button type="button" class="deploy-method-card' + sel + '" data-method="' + Utils.escapeAttr(m) + '">' +
        '<div class="deploy-method-card-icon">' + methodIcon(m) + '</div>' +
        '<div class="deploy-method-card-name">' + Utils.escapeHtml(name) + '</div>' +
        '<div class="deploy-method-card-desc">' + Utils.escapeHtml(desc) + '</div>' +
      '</button>';
    }).join('');

    return '<h4>' + I18n.t('wizard.selectMethod') + '</h4>' +
      '<div class="deploy-method-grid">' + cards + '</div>';
  }

  function methodIcon(method) {
    if (method === 'docker') return '\uD83D\uDC33';
    if (method === 'native') return '\u26A1';
    if (method === 'external') return '\uD83D\uDD17';
    return '\uD83D\uDCE6';
  }

  // ---- Krok 2: wybor modelu (preset lub HuggingFace search) ---------------

  function renderStepModel() {
    const presets = ManifestStore.modelPresets(engineEntry);
    const hasPresets = presets.length > 0;

    let tabs = '<div class="wizard-tabs">';
    if (hasPresets) {
      tabs += '<button type="button" class="wizard-tab' + (modelSourceMode === 'preset' ? ' active' : '') + '" data-mode="preset">' +
        Utils.escapeHtml(I18n.t('wizard.fromPreset')) + '</button>';
    }
    tabs += '<button type="button" class="wizard-tab' + (modelSourceMode === 'hf' ? ' active' : '') + '" data-mode="hf">' +
      Utils.escapeHtml(I18n.t('wizard.searchHuggingface')) + '</button>';
    tabs += '</div>';

    const content = '<div class="wizard-tab-content">' +
      (modelSourceMode === 'preset' && hasPresets ? renderPresetSelector(presets) : renderHfSearch()) +
      '</div>';

    return '<h4>' + I18n.t('wizard.selectModel') + '</h4>' + tabs + content;
  }

  function renderPresetSelector(presets) {
    if (!presets.length) {
      return `<p class="form-hint">${I18n.t('wizard.noPresets')}</p>`;
    }
    const items = presets.map(p => {
      if (!p) return '';
      const id = p.id || '';
      const display = p.display_name || p.repo || id;
      const repo = p.repo || '';
      const quant = p.quantization || '';
      const star = p.recommended ? ' \u2B50' : '';
      const sel = selection.modelPresetId === id ? ' selected' : '';
      const info = repo + (quant ? ' \u2022 ' + quant : '');
      return '<div class="model-item' + sel + '" data-preset-id="' + Utils.escapeAttr(id) + '">' +
        '<div style="flex:1;min-width:0;">' +
          '<div class="model-item-name">' + Utils.escapeHtml(display) + star + '</div>' +
          (info ? '<div class="model-item-info">' + Utils.escapeHtml(info) + '</div>' : '') +
        '</div>' +
      '</div>';
    }).join('');

    return '<div class="model-list">' + items + '</div>' +
      '<p class="form-hint">' + I18n.t('wizard.presetHint') + '</p>';
  }

  function renderHfSearch() {
    const placeholder = I18n.t('wizard.hfSearchPlaceholder');
    const tokenLabel = I18n.t('wizard.huggingfaceToken');
    const repoFilterHint = hfSearchFilterHint();

    return `
      <div class="form-group">
        <input type="text" id="edw-hf-search" class="form-input"
          placeholder="${Utils.escapeAttr(placeholder)}"
          value="${Utils.escapeAttr(hfSearchQuery)}"
          autocomplete="off">
        <div class="form-hint">${I18n.t('wizard.hfSearchHint')}${repoFilterHint ? ' \u2022 ' + Utils.escapeHtml(repoFilterHint) : ''}</div>
      </div>
      <div class="form-group">
        <label for="edw-hf-token">${Utils.escapeHtml(tokenLabel)}</label>
        <input type="password" id="edw-hf-token" class="form-input"
          value="${Utils.escapeAttr(hfToken)}" autocomplete="off">
      </div>
      <div class="model-list" id="edw-hf-results">
        ${renderHfResultsHtml()}
      </div>
    `;
  }

  function renderHfResultsHtml() {
    if (hfSearching) {
      return `<p class="form-hint">${I18n.t('common.loading')}</p>`;
    }
    if (hfResults.length === 0) {
      return '';
    }
    return hfResults.map(r => {
      const id = r.id || r.modelId || '';
      const downloads = r.downloads ? formatCount(r.downloads) : '';
      const likes = r.likes ? r.likes : '';
      const lastModified = r.lastModified ? r.lastModified.substring(0, 10) : '';
      const sel = selection.modelRepo === id ? ' selected' : '';
      let info = '';
      if (downloads) info += '\u2193 ' + downloads;
      if (likes) info += (info ? ' \u2022 ' : '') + '\u2665 ' + likes;
      if (lastModified) info += (info ? ' \u2022 ' : '') + lastModified;
      return '<div class="model-item' + sel + '" data-repo="' + Utils.escapeAttr(id) + '">' +
        '<div style="flex:1;min-width:0;">' +
          '<div class="model-item-name" style="font-family:\'JetBrains Mono\',\'Fira Code\',monospace;font-size:var(--font-size-sm);">' + Utils.escapeHtml(id) + '</div>' +
          (info ? '<div class="model-item-info">' + Utils.escapeHtml(info) + '</div>' : '') +
        '</div>' +
      '</div>';
    }).join('');
  }

  // Wskazówka filtrująca: jeśli silnik wymaga GGUF/MLX — pokaż info.
  function hfSearchFilterHint() {
    if (!engineEntry || !engineEntry.engine) return '';
    const id = String(engineEntry.engine.id || '').toLowerCase();
    if (id.indexOf('llama') !== -1 || id.indexOf('llamacpp') !== -1) return 'GGUF';
    if (id === 'mlx') return 'mlx-community/*';
    return '';
  }

  function formatCount(n) {
    if (n >= 1000000) return (n / 1000000).toFixed(1) + 'M';
    if (n >= 1000) return (n / 1000).toFixed(1) + 'k';
    return String(n);
  }

  // ---- Krok 3: konfiguracja runtime ---------------------------------------

  function renderStepRuntime() {
    const eng = engineEntry.engine || {};
    const port = selection.port || eng.default_port || 8080;
    const cname = selection.containerName || '';

    const summary = renderModelSummary();

    let extra = '';
    if (selection.deployMethod === 'docker') {
      extra =
        '<div class="form-group">' +
          '<label for="edw-cname">' + I18n.t('wizard.containerName') + '</label>' +
          '<input type="text" id="edw-cname" class="form-input" value="' + Utils.escapeAttr(cname) + '">' +
        '</div>';
    }

    return '<h4>' + I18n.t('wizard.configureRuntime') + '</h4>' +
      summary +
      '<div class="form-group">' +
        '<label for="edw-port">' + I18n.t('wizard.port') + '</label>' +
        '<input type="number" id="edw-port" class="form-input" min="1" max="65535" value="' + Utils.escapeAttr(String(port)) + '">' +
      '</div>' + extra;
  }

  function renderModelSummary() {
    let modelDesc = '';
    if (selection.modelRepo) {
      modelDesc = '<code>' + Utils.escapeHtml(selection.modelRepo) + '</code> ' +
        '<span style="color:var(--color-text-muted);font-size:var(--font-size-xs);">(HuggingFace)</span>';
    } else if (selection.modelPresetId) {
      const presets = ManifestStore.modelPresets(engineEntry);
      const preset = presets.find(p => p && p.id === selection.modelPresetId);
      if (preset) {
        modelDesc = '<strong>' + Utils.escapeHtml(preset.display_name || preset.id) + '</strong>' +
          (preset.repo ? ' <span style="color:var(--color-text-muted);font-size:var(--font-size-xs);">' + Utils.escapeHtml(preset.repo) + '</span>' : '');
      }
    }
    if (!modelDesc) return '';
    return '<div class="form-group">' +
      '<label>' + I18n.t('wizard.modelLabel') + '</label>' +
      '<div>' + modelDesc + '</div>' +
    '</div>';
  }

  // ---- Bind eventow --------------------------------------------------------

  function bindStepInputs() {
    if (currentStep === 1) bindStepMethodInputs();
    if (currentStep === 2) bindStepModelInputs();
    if (currentStep === 3) bindStepRuntimeInputs();
  }

  function bindStepMethodInputs() {
    document.querySelectorAll('.deploy-method-card[data-method]').forEach(btn => {
      btn.addEventListener('click', () => {
        selection.deployMethod = btn.dataset.method;
        refreshModal();
      });
    });
  }

  function bindStepModelInputs() {
    // Przelaczanie miedzy zakladkami "preset" / "hf".
    document.querySelectorAll('.wizard-tab[data-mode]').forEach(t => {
      t.addEventListener('click', () => {
        modelSourceMode = t.dataset.mode;
        refreshModal();
      });
    });

    // Wybor presetu (klik calego wiersza).
    document.querySelectorAll('.model-item[data-preset-id]').forEach(it => {
      it.addEventListener('click', () => {
        selection.modelPresetId = it.dataset.presetId;
        selection.modelRepo = null;
        document.querySelectorAll('.model-item[data-preset-id]').forEach(x => x.classList.remove('selected'));
        it.classList.add('selected');
      });
    });

    const search = document.getElementById('edw-hf-search');
    if (search) {
      search.addEventListener('input', () => {
        clearTimeout(hfSearchTimer);
        hfSearchQuery = search.value;
        const q = search.value.trim();
        if (q.length < 2) {
          hfResults = [];
          updateHfResults();
          return;
        }
        hfSearchTimer = setTimeout(() => doHfSearch(q), 500);
      });
    }

    const tokenInput = document.getElementById('edw-hf-token');
    if (tokenInput) {
      tokenInput.addEventListener('input', () => {
        hfToken = tokenInput.value;
      });
    }

    bindHfResultClicks();
  }

  function bindHfResultClicks() {
    document.querySelectorAll('.model-item[data-repo]').forEach(it => {
      it.addEventListener('click', () => {
        selection.modelRepo = it.dataset.repo;
        selection.modelPresetId = null;
        document.querySelectorAll('.model-item[data-repo]').forEach(x => x.classList.remove('selected'));
        it.classList.add('selected');
      });
    });
  }

  function bindStepRuntimeInputs() {
    const portInput = document.getElementById('edw-port');
    if (portInput) {
      portInput.addEventListener('input', () => {
        const v = parseInt(portInput.value, 10);
        selection.port = isNaN(v) ? portInput.value : v;
      });
    }
    const cnameInput = document.getElementById('edw-cname');
    if (cnameInput) {
      cnameInput.addEventListener('input', () => {
        selection.containerName = cnameInput.value.trim();
      });
    }
  }

  function bindFooter() {
    const cancelBtn = document.getElementById('edw-cancel');
    if (cancelBtn) cancelBtn.addEventListener('click', close);

    const backBtn = document.getElementById('edw-back');
    if (backBtn) backBtn.addEventListener('click', () => {
      if (currentStep > 1) {
        currentStep--;
        refreshModal();
      }
    });

    const nextBtn = document.getElementById('edw-next');
    if (nextBtn) nextBtn.addEventListener('click', () => {
      if (!canAdvance()) return;
      currentStep++;
      refreshModal();
    });

    const deployBtn = document.getElementById('edw-deploy');
    if (deployBtn) deployBtn.addEventListener('click', startDeploy);
  }

  function canAdvance() {
    if (currentStep === 1) {
      if (!selection.deployMethod) {
        if (typeof App !== 'undefined' && App.showToast) App.showToast(I18n.t('wizard.selectMethod'), 'error');
        return false;
      }
      return true;
    }
    if (currentStep === 2) {
      if (!selection.modelPresetId && !selection.modelRepo) {
        if (typeof App !== 'undefined' && App.showToast) App.showToast(I18n.t('wizard.selectModel'), 'error');
        return false;
      }
      return true;
    }
    return true;
  }

  // ---- HuggingFace search --------------------------------------------------

  async function doHfSearch(query) {
    hfSearching = true;
    updateHfResults();
    try {
      const url = 'https://huggingface.co/api/models?search=' + encodeURIComponent(query) + '&limit=20';
      const headers = {};
      if (hfToken) headers['Authorization'] = 'Bearer ' + hfToken;
      const resp = await fetch(url, { headers });
      if (!resp.ok) throw new Error('HF API ' + resp.status);
      let data = await resp.json();
      if (!Array.isArray(data)) data = [];

      const engId = String((engineEntry.engine && engineEntry.engine.id) || '').toLowerCase();
      if (engId.indexOf('llama') !== -1 || engId.indexOf('llamacpp') !== -1) {
        data = data.filter(m => String(m.id || '').toLowerCase().indexOf('gguf') !== -1);
      } else if (engId === 'mlx') {
        data = data.filter(m => {
          const id = String(m.id || '').toLowerCase();
          return id.indexOf('mlx-') !== -1 || id.indexOf('mlx-community/') !== -1;
        });
      }

      hfResults = data;
    } catch (err) {
      console.error('[EngineDeployWizard] HF search blad:', err);
      hfResults = [];
    } finally {
      hfSearching = false;
      updateHfResults();
    }
  }

  function updateHfResults() {
    const box = document.getElementById('edw-hf-results');
    if (!box) return;
    box.innerHTML = renderHfResultsHtml();
    bindHfResultClicks();
  }

  // ---- Deploy --------------------------------------------------------------

  async function startDeploy() {
    const deployBtn = document.getElementById('edw-deploy');
    if (deployBtn) deployBtn.disabled = true;

    const eng = engineEntry.engine || {};
    const body = {
      engine_id: eng.id,
      deploy_method: selection.deployMethod,
      node_id: selection.nodeId,
      config: {
        model_preset_id: selection.modelPresetId || null,
        model_repo: selection.modelRepo || null,
        port: selection.port || eng.default_port,
        container_name: selection.containerName || null
      }
    };

    try {
      let data;
      if (typeof ApiClient !== 'undefined' && typeof ApiClient.post === 'function') {
        data = await ApiClient.post('/api/services/deploy', body);
      } else {
        const resp = await fetch('/api/services/deploy', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(body)
        });
        data = await resp.json();
        if (!resp.ok) throw new Error((data && (data.message || data.error_code)) || 'HTTP ' + resp.status);
      }

      const id = (data && data.deploy_id) ? data.deploy_id : '?';
      const msg = I18n.t('wizard.deployStarted').replace('{id}', id);
      if (typeof App !== 'undefined' && App.showToast) {
        App.showToast(msg, 'success');
      } else {
        alert(msg);
      }
      console.log('[EngineDeployWizard] deploy started:', data);
      setTimeout(close, 1500);
    } catch (err) {
      console.error('[EngineDeployWizard] deploy error:', err);
      const msg = I18n.t('wizard.deployFailed').replace('{error}', err.message || err);
      if (typeof App !== 'undefined' && App.showToast) {
        App.showToast(msg, 'error');
      } else {
        alert(msg);
      }
      if (deployBtn) deployBtn.disabled = false;
    }
  }

  return { open, close };
})();

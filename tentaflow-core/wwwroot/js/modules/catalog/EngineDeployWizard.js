// =============================================================================
// Plik: modules/catalog/EngineDeployWizard.js
// Opis: Wizard wdrazania silnikow AI dla nowego (uproszczonego) schematu
//       manifestu. Trzy kroki: 1) wybor trybu deploymentu (docker/native/external)
//       dostepnego dla platformy hosta, 2) wybor modelu (preset z manifestu lub
//       wyszukiwarka HuggingFace), 3) konfiguracja runtime (port, ekstra param).
// Przyklad: EngineDeployWizard.open('vllm');
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
  let modelSourceMode = 'preset'; // 'preset' albo 'hf'
  let hfSearchTimer = null;
  let hfResults = [];
  let hfSearching = false;

  let selection = {
    nodeId: null,
    deployMethod: null,
    modelPresetId: null,
    modelRepo: null,
    port: null,
    containerName: null
  };

  // Otwarcie wizarda dla danego silnika.
  // opts.nodeId — preselekcja wezla (np. z MeshNodeDetail).
  async function open(engId, opts) {
    engineId = engId;
    engineEntry = null;
    currentStep = 1;
    modelSourceMode = 'preset';
    hfResults = [];
    selection = {
      nodeId: (opts && opts.nodeId) || null,
      deployMethod: null,
      modelPresetId: null,
      modelRepo: null,
      port: null,
      containerName: null
    };

    renderModal('<div class="wizard-progress">' + I18n.t('common.loading') + '</div>');

    try {
      await ManifestStore.init();
    } catch (err) {
      console.error('[EngineDeployWizard] init manifest blad:', err);
    }

    engineEntry = ManifestStore.byId(engineId);
    if (!engineEntry) {
      renderModal('<div class="wizard-error">Engine \'' + Utils.escapeHtml(engineId || '') + '\' nie istnieje w manifescie</div>');
      return;
    }

    try {
      nodes = await fetchNodes();
    } catch (err) {
      console.error('[EngineDeployWizard] fetchNodes blad:', err);
      nodes = [];
    }

    // Domyslny wezel: preselekcja, lub local, lub pierwszy z listy.
    if (!selection.nodeId) {
      const local = nodes.find(n => n && n.is_local === true) || nodes[0];
      selection.nodeId = local ? (local.node_id || local.id) : null;
    }

    hostOs = pickHostOsFromNode(selection.nodeId);
    availableMethods = ManifestStore.availableDeployMethods(engineEntry, hostOs);

    // Domyslna metoda — pierwsza dostepna dla platformy.
    if (availableMethods.length > 0) {
      selection.deployMethod = availableMethods[0];
    }

    // Domyslne wartosci konfiguracji.
    const eng = engineEntry.engine || {};
    selection.port = eng.default_port || 8080;
    selection.containerName = `tentaflow-${(eng.id || 'svc').toLowerCase()}-${randomSuffix()}`;

    const presets = ManifestStore.modelPresets(engineEntry);
    if (presets.length > 0) {
      const rec = presets.find(p => p && p.recommended) || presets[0];
      if (rec) selection.modelPresetId = rec.id;
    }

    // Token HF z ustawien (opcjonalny).
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

  // Pobiera liste wezlow z mesh API z fallbackiem na "local".
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

  function renderModal(bodyHtml) {
    close();
    const overlay = document.createElement('div');
    overlay.className = 'modal-overlay active';
    overlay.id = 'engine-deploy-wizard';
    overlay.innerHTML =
      '<div class="modal" style="max-width: 720px;">' +
        '<div class="modal-header">' +
          '<h3 id="edw-title">' + I18n.t('wizard.title') + '</h3>' +
          '<button class="modal-close" id="edw-close">&times;</button>' +
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
    let html = '<button class="btn btn-ghost btn-sm" id="edw-cancel">' + I18n.t('wizard.cancel') + '</button>';
    if (currentStep > 1) {
      html += '<button class="btn btn-secondary btn-sm" id="edw-back">\u2190 ' + I18n.t('wizard.back') + '</button>';
    }
    if (currentStep < 3) {
      html += '<button class="btn btn-primary btn-sm" id="edw-next">' + I18n.t('wizard.next') + ' \u2192</button>';
    } else {
      html += '<button class="btn btn-primary btn-sm" id="edw-deploy">' + I18n.t('wizard.startDeploy') + '</button>';
    }
    return html;
  }

  // ---- Krok 1: wybor trybu deploymentu ------------------------------------

  function renderStepMethod() {
    if (!availableMethods || availableMethods.length === 0) {
      return '<h3>' + I18n.t('wizard.selectMethod') + '</h3>' +
        '<p class="empty-state-text">Brak dostepnych trybow deploymentu dla platformy ' + Utils.escapeHtml(hostOs) + '.</p>';
    }

    const labelMap = {
      docker: I18n.t('wizard.method.docker'),
      native: I18n.t('wizard.method.native'),
      external: I18n.t('wizard.method.external')
    };

    const cards = availableMethods.map(m => {
      const sel = selection.deployMethod === m ? ' selected' : '';
      return '<button type="button" class="edw-method-btn' + sel + '" data-method="' + Utils.escapeAttr(m) + '" ' +
        'style="display:flex;flex-direction:column;align-items:center;justify-content:center;padding:18px;border:1px solid var(--color-border);border-radius:8px;background:transparent;cursor:pointer;flex:1;min-width:140px;' +
        (selection.deployMethod === m ? 'border-color:var(--color-primary);background:var(--color-primary-light, rgba(52,152,219,0.1));' : '') + '">' +
        '<strong style="font-size:1.05em;">' + Utils.escapeHtml(labelMap[m] || m) + '</strong>' +
        '</button>';
    }).join('');

    return '<h3>' + I18n.t('wizard.selectMethod') + '</h3>' +
      '<div class="edw-method-grid" style="display:flex;gap:12px;flex-wrap:wrap;margin-top:12px;">' +
        cards +
      '</div>';
  }

  // ---- Krok 2: wybor modelu (preset lub HuggingFace search) ---------------

  function renderStepModel() {
    const presets = ManifestStore.modelPresets(engineEntry);
    const hasPresets = presets.length > 0;

    const tabs = '<div class="edw-model-tabs" style="display:flex;gap:8px;margin-bottom:12px;border-bottom:1px solid var(--color-border);">' +
      (hasPresets ? `<button type="button" class="edw-tab${modelSourceMode === 'preset' ? ' active' : ''}" data-source="preset" style="background:transparent;border:none;padding:8px 14px;cursor:pointer;border-bottom:2px solid ${modelSourceMode === 'preset' ? 'var(--color-primary)' : 'transparent'};">${Utils.escapeHtml(I18n.t('wizard.fromPreset'))}</button>` : '') +
      `<button type="button" class="edw-tab${modelSourceMode === 'hf' ? ' active' : ''}" data-source="hf" style="background:transparent;border:none;padding:8px 14px;cursor:pointer;border-bottom:2px solid ${modelSourceMode === 'hf' ? 'var(--color-primary)' : 'transparent'};">${Utils.escapeHtml(I18n.t('wizard.searchHuggingface'))}</button>` +
      '</div>';

    let body = '';
    if (modelSourceMode === 'preset' && hasPresets) {
      body = renderPresetPicker(presets);
    } else {
      body = renderHfSearch();
    }

    return '<h3>' + I18n.t('wizard.selectModel') + '</h3>' + tabs + body;
  }

  function renderPresetPicker(presets) {
    const rows = presets.map(p => {
      if (!p) return '';
      const id = p.id || '';
      const display = p.display_name || p.repo || id;
      const repo = p.repo || '';
      const quant = p.quantization || '';
      const star = p.recommended ? ' \u2605' : '';
      const sel = selection.modelPresetId === id ? ' checked' : '';
      return `<label class="edw-preset-row" style="display:flex;gap:10px;padding:8px;border:1px solid var(--color-border);border-radius:6px;margin-bottom:6px;cursor:pointer;align-items:center;">
        <input type="radio" name="edw-preset" value="${Utils.escapeAttr(id)}"${sel}>
        <div style="flex:1;">
          <div style="font-weight:600;">${Utils.escapeHtml(display)}${star}</div>
          <div style="font-size:0.85em;color:var(--color-text-muted);">${Utils.escapeHtml(repo)}${quant ? ' \u2022 ' + Utils.escapeHtml(quant) : ''}</div>
        </div>
      </label>`;
    }).join('');

    return `<div class="edw-preset-list">${rows}</div>`;
  }

  function renderHfSearch() {
    const repoFilter = hfSearchFilterHint();
    const items = hfResults.map(r => {
      const id = r.id || r.modelId || '';
      const downloads = r.downloads ? formatCount(r.downloads) : '';
      const likes = r.likes ? r.likes : '';
      const lastModified = r.lastModified ? r.lastModified.substring(0, 10) : '';
      const sel = selection.modelRepo === id ? ' selected' : '';
      return `<div class="edw-hf-item${sel}" data-repo="${Utils.escapeAttr(id)}"
        style="padding:8px;border:1px solid var(--color-border);border-radius:6px;margin-bottom:6px;cursor:pointer;${selection.modelRepo === id ? 'border-color:var(--color-primary);background:var(--color-primary-light, rgba(52,152,219,0.08));' : ''}">
        <div style="font-weight:600;font-family:monospace;font-size:0.9em;">${Utils.escapeHtml(id)}</div>
        <div style="font-size:0.8em;color:var(--color-text-muted);margin-top:2px;">
          ${downloads ? '\u2193 ' + downloads : ''}
          ${likes ? ' \u2022 \u2665 ' + likes : ''}
          ${lastModified ? ' \u2022 ' + lastModified : ''}
        </div>
      </div>`;
    }).join('');

    const placeholder = hfSearching
      ? `<div class="empty-state-hint">${I18n.t('common.loading')}</div>`
      : (hfResults.length === 0 ? '<div class="empty-state-hint">\u2014</div>' : '');

    return `
      <div class="form-group">
        <input type="text" id="edw-hf-search" class="form-input"
          placeholder="np. qwen, llama, mistral..." value="">
        ${repoFilter ? `<div class="form-hint" style="font-size:0.8em;color:var(--color-text-muted);margin-top:4px;">${repoFilter}</div>` : ''}
      </div>
      <div class="form-group">
        <label for="edw-hf-token" style="font-size:0.85em;">${Utils.escapeHtml(I18n.t('wizard.huggingfaceToken'))}</label>
        <input type="password" id="edw-hf-token" class="form-input" value="${Utils.escapeAttr(hfToken)}" autocomplete="off">
      </div>
      <div class="edw-hf-results" id="edw-hf-results" style="max-height:280px;overflow-y:auto;">
        ${items || placeholder}
      </div>
    `;
  }

  // Wskazowka filtrujaca: jesli silnik wymaga GGUF/MLX — pokaz info.
  function hfSearchFilterHint() {
    if (!engineEntry || !engineEntry.engine) return '';
    const id = String(engineEntry.engine.id || '').toLowerCase();
    if (id.indexOf('llama') !== -1 || id.indexOf('llamacpp') !== -1) return 'Filtrowane do modeli GGUF';
    if (id === 'mlx') return 'Filtrowane do mlx-community/*';
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
      extra = `
        <div class="form-group">
          <label for="edw-cname">Nazwa kontenera</label>
          <input type="text" id="edw-cname" class="form-input" value="${Utils.escapeAttr(cname)}">
        </div>
      `;
    }

    return '<h3>' + I18n.t('wizard.configureRuntime') + '</h3>' +
      summary +
      '<div class="form-group">' +
        '<label for="edw-port">Port</label>' +
        '<input type="number" id="edw-port" class="form-input" min="1" max="65535" value="' + Utils.escapeAttr(String(port)) + '">' +
      '</div>' + extra;
  }

  function renderModelSummary() {
    let modelDesc = '';
    if (selection.modelRepo) {
      modelDesc = `<code style="font-family:monospace;">${Utils.escapeHtml(selection.modelRepo)}</code> <span style="color:var(--color-text-muted);font-size:0.85em;">(HuggingFace)</span>`;
    } else if (selection.modelPresetId) {
      const presets = ManifestStore.modelPresets(engineEntry);
      const preset = presets.find(p => p && p.id === selection.modelPresetId);
      if (preset) {
        modelDesc = `<strong>${Utils.escapeHtml(preset.display_name || preset.id)}</strong>` +
          (preset.repo ? ` <span style="color:var(--color-text-muted);font-size:0.85em;">${Utils.escapeHtml(preset.repo)}</span>` : '');
      }
    }
    if (!modelDesc) return '';
    return `<div class="wizard-summary" style="margin-bottom:12px;padding:10px;background:var(--color-bg-secondary, rgba(0,0,0,0.04));border-radius:6px;">
      <div style="font-size:0.85em;color:var(--color-text-muted);margin-bottom:4px;">Model</div>
      <div>${modelDesc}</div>
    </div>`;
  }

  // ---- Bind eventow --------------------------------------------------------

  function bindStepInputs() {
    if (currentStep === 1) bindStepMethodInputs();
    if (currentStep === 2) bindStepModelInputs();
    if (currentStep === 3) bindStepRuntimeInputs();
  }

  function bindStepMethodInputs() {
    document.querySelectorAll('.edw-method-btn[data-method]').forEach(btn => {
      btn.addEventListener('click', () => {
        selection.deployMethod = btn.dataset.method;
        refreshModal();
      });
    });
  }

  function bindStepModelInputs() {
    document.querySelectorAll('.edw-tab[data-source]').forEach(t => {
      t.addEventListener('click', () => {
        modelSourceMode = t.dataset.source;
        refreshModal();
      });
    });

    document.querySelectorAll('input[name="edw-preset"]').forEach(r => {
      r.addEventListener('change', e => {
        if (e.target.checked) {
          selection.modelPresetId = e.target.value;
          selection.modelRepo = null;
        }
      });
    });

    const search = document.getElementById('edw-hf-search');
    if (search) {
      search.addEventListener('input', () => {
        clearTimeout(hfSearchTimer);
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

    document.querySelectorAll('.edw-hf-item[data-repo]').forEach(it => {
      it.addEventListener('click', () => {
        selection.modelRepo = it.dataset.repo;
        selection.modelPresetId = null;
        // Visualne podswietlenie bez pelnego refreshModal (zachowuje wyniki).
        document.querySelectorAll('.edw-hf-item').forEach(x => {
          x.style.borderColor = 'var(--color-border)';
          x.style.background = '';
        });
        it.style.borderColor = 'var(--color-primary)';
        it.style.background = 'var(--color-primary-light, rgba(52,152,219,0.08))';
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

      // Filtrowanie per silnik (GGUF / MLX).
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
    if (hfSearching) {
      box.innerHTML = `<div class="empty-state-hint">${I18n.t('common.loading')}</div>`;
      return;
    }
    if (hfResults.length === 0) {
      box.innerHTML = '<div class="empty-state-hint">\u2014</div>';
      return;
    }
    box.innerHTML = hfResults.map(r => {
      const id = r.id || r.modelId || '';
      const downloads = r.downloads ? formatCount(r.downloads) : '';
      const likes = r.likes ? r.likes : '';
      const lastModified = r.lastModified ? r.lastModified.substring(0, 10) : '';
      const isSel = selection.modelRepo === id;
      return `<div class="edw-hf-item" data-repo="${Utils.escapeAttr(id)}"
        style="padding:8px;border:1px solid ${isSel ? 'var(--color-primary)' : 'var(--color-border)'};border-radius:6px;margin-bottom:6px;cursor:pointer;${isSel ? 'background:var(--color-primary-light, rgba(52,152,219,0.08));' : ''}">
        <div style="font-weight:600;font-family:monospace;font-size:0.9em;">${Utils.escapeHtml(id)}</div>
        <div style="font-size:0.8em;color:var(--color-text-muted);margin-top:2px;">
          ${downloads ? '\u2193 ' + downloads : ''}
          ${likes ? ' \u2022 \u2665 ' + likes : ''}
          ${lastModified ? ' \u2022 ' + lastModified : ''}
        </div>
      </div>`;
    }).join('');

    box.querySelectorAll('.edw-hf-item[data-repo]').forEach(it => {
      it.addEventListener('click', () => {
        selection.modelRepo = it.dataset.repo;
        selection.modelPresetId = null;
        box.querySelectorAll('.edw-hf-item').forEach(x => {
          x.style.borderColor = 'var(--color-border)';
          x.style.background = '';
        });
        it.style.borderColor = 'var(--color-primary)';
        it.style.background = 'var(--color-primary-light, rgba(52,152,219,0.08))';
      });
    });
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
      if (typeof App !== 'undefined' && App.showToast) {
        App.showToast('Deploy wystartowal: ' + id, 'success');
      } else {
        alert('Deploy wystartowal: ' + id);
      }
      console.log('[EngineDeployWizard] deploy started:', data);
      setTimeout(close, 1500);
    } catch (err) {
      console.error('[EngineDeployWizard] deploy error:', err);
      if (typeof App !== 'undefined' && App.showToast) {
        App.showToast('Blad: ' + (err.message || err), 'error');
      } else {
        alert('Deploy nieudany: ' + (err.message || err));
      }
      if (deployBtn) deployBtn.disabled = false;
    }
  }

  return { open, close };
})();

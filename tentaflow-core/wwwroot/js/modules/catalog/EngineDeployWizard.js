// =============================================================================
// Plik: modules/catalog/EngineDeployWizard.js
// Opis: Uniwersalny wizard deploymentu silnikow AI. Zastepuje 3 stare wizardy
//       (LLMDeployWizard, SttDeployWizard, ServiceDeployModal). Dziala dla
//       wszystkich 12 kategorii.
//       Kroki: 1) wybor wezla, 2) wybor wariantu, 3) Build/Download, 4) konfig.
// Przyklad: EngineDeployWizard.open('llama-cpp', { nodeId: 'local' });
// =============================================================================

const EngineDeployWizard = (() => {
  'use strict';

  let currentStep = 1;
  let engineId = null;
  let engineEntry = null;
  let nodes = [];
  let licenseInfo = null;

  let selection = {
    nodeId: null,
    variantId: null,
    deployMethod: 'build',
    config: {}
  };

  // Otwarcie wizarda dla danego silnika.
  // opts.nodeId — preselekcja wezla (np. z MeshNodeDetail).
  async function open(engId, opts) {
    engineId = engId;
    engineEntry = null;
    currentStep = 1;
    selection = {
      nodeId: (opts && opts.nodeId) || null,
      variantId: null,
      deployMethod: 'build',
      config: {}
    };

    // Pokaz natychmiast szkielet (placeholder)
    renderModal('<div class="wizard-progress">' + I18n.t('common.loading') + '</div>');

    // Zaladuj manifest, licencje i wezly rownolegle
    try {
      await ManifestStore.init();
    } catch (err) {
      console.error('[EngineDeployWizard] init manifest blad:', err);
    }

    engineEntry = ManifestStore.byId(engineId);
    if (!engineEntry) {
      renderModal('<div class="wizard-error">Engine \'' + Utils.escapeHtml(engineId) + '\' nie istnieje w manifescie</div>');
      return;
    }

    try {
      [licenseInfo, nodes] = await Promise.all([
        LicenseBadge.fetchInfo(),
        fetchNodes()
      ]);
    } catch (err) {
      console.error('[EngineDeployWizard] init blad:', err);
      licenseInfo = { tier: 'free', allows_pro: false, allows_enterprise: false };
      nodes = [];
    }

    // Domyslny wezel: preselekcja, lub local, lub pierwszy z listy
    if (!selection.nodeId) {
      const local = nodes.find(n => n.is_local) || nodes[0];
      selection.nodeId = local ? (local.node_id || local.id) : null;
    }

    refreshModal();
  }

  function close() {
    const el = document.getElementById('engine-deploy-wizard');
    if (el) el.remove();
  }

  // Pobiera liste wezlow z mesh API (z fallbackiem do "local").
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
        return resp.filter(n => n.is_trusted === true || n.is_local === true);
      }
    } catch (err) {
      console.warn('[EngineDeployWizard] fetchNodes fallback:', err);
    }
    return [{
      node_id: 'local', id: 'local', name: 'Local', is_local: true,
      os: 'linux', arch: 'x86_64', gpu: 'cpu'
    }];
  }

  // Wyciaga capabilities (os/arch/gpu) z obiektu wezla z roznych zrodel.
  function nodeCapabilities(node) {
    if (!node) return null;
    const os = (node.os || (node.platform && node.platform.os) || 'linux').toLowerCase();
    const arch = (node.arch || (node.platform && node.platform.arch) || 'x86_64').toLowerCase();
    let gpu = node.gpu;
    if (!gpu && Array.isArray(node.gpu_info) && node.gpu_info.length > 0) {
      const g = node.gpu_info[0];
      const name = String(g.name || '').toLowerCase();
      if (name.indexOf('nvidia') !== -1 || name.indexOf('rtx') !== -1 || name.indexOf('gtx') !== -1) gpu = 'cuda';
      else if (name.indexOf('amd') !== -1 || name.indexOf('radeon') !== -1) gpu = 'rocm';
      else if (name.indexOf('apple') !== -1 || name.indexOf('metal') !== -1) gpu = 'metal';
      else gpu = 'cpu';
    }
    if (!gpu) gpu = 'cpu';
    return { os: os, arch: arch, gpu: gpu };
  }

  function selectedNode() {
    if (!selection.nodeId) return null;
    return nodes.find(n => (n.node_id || n.id) === selection.nodeId) || null;
  }

  function selectedVariant() {
    if (!engineEntry || !selection.variantId) return null;
    const variants = engineEntry.variant || [];
    return variants.find(v => v.id === selection.variantId) || null;
  }

  // Renderuje overlay modala (bez animacji, prostym innerHTML).
  function renderModal(bodyHtml) {
    close();
    const overlay = document.createElement('div');
    overlay.className = 'modal-overlay active';
    overlay.id = 'engine-deploy-wizard';
    overlay.innerHTML =
      '<div class="modal" style="max-width: 640px;">' +
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

  // Pelne odswiezenie zawartosci modala (po zaladowaniu danych lub zmianie kroku).
  function refreshModal() {
    const titleEl = document.getElementById('edw-title');
    const bodyEl = document.getElementById('edw-body');
    const footerEl = document.getElementById('edw-footer');
    if (!bodyEl || !footerEl) {
      // Modal zniknal — odtworz
      renderModal('');
    }

    const t = document.getElementById('edw-title');
    if (t && engineEntry && engineEntry.engine) {
      t.textContent = I18n.t('wizard.title') + ': ' + engineEntry.engine.name;
    }

    const b = document.getElementById('edw-body');
    if (b) b.innerHTML = renderStepIndicator() + renderStepBody();

    const f = document.getElementById('edw-footer');
    if (f) f.innerHTML = renderFooter();

    bindStepInputs();
    bindFooter();
  }

  function renderStepIndicator() {
    const total = 4;
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
      case 1: return renderStepNode();
      case 2: return renderStepVariant();
      case 3: return renderStepMethod();
      case 4: return renderStepConfig();
      default: return '';
    }
  }

  function renderFooter() {
    let html = '<button class="btn btn-ghost btn-sm" id="edw-cancel">' + I18n.t('wizard.cancel') + '</button>';
    if (currentStep > 1) {
      html += '<button class="btn btn-secondary btn-sm" id="edw-back">\u2190 ' + I18n.t('wizard.back') + '</button>';
    }
    if (currentStep < 4) {
      html += '<button class="btn btn-primary btn-sm" id="edw-next">' + I18n.t('wizard.next') + ' \u2192</button>';
    } else {
      html += '<button class="btn btn-primary btn-sm" id="edw-deploy">' + I18n.t('wizard.startDeploy') + '</button>';
    }
    return html;
  }

  // ---- Step 1: wybor wezla -------------------------------------------------

  function renderStepNode() {
    const opts = nodes.map(n => {
      const nid = n.node_id || n.id;
      const label = (n.hostname || n.name || nid) +
        ' (' + (n.os || 'linux') + '/' + (n.arch || 'x86_64') + '/' + (n.gpu || 'cpu') + ')';
      const sel = nid === selection.nodeId ? ' selected' : '';
      return '<option value="' + Utils.escapeAttr(nid) + '"' + sel + '>' + Utils.escapeHtml(label) + '</option>';
    }).join('');

    return '<h3>' + I18n.t('wizard.selectNode') + '</h3>' +
      '<div class="form-group">' +
        '<select id="edw-node-select" class="form-input">' + opts + '</select>' +
      '</div>';
  }

  // ---- Step 2: wybor wariantu ---------------------------------------------

  function renderStepVariant() {
    const node = selectedNode();
    const caps = nodeCapabilities(node);
    const compatible = ManifestStore.compatibleVariants(engineEntry, caps);

    if (compatible.length === 0) {
      return '<h3>' + I18n.t('wizard.selectVariant') + '</h3>' +
        '<p class="empty-state-text">' + I18n.t('manifest.noVariantsForPlatform') + '</p>';
    }

    const items = compatible.map(v => {
      const selectedCls = v.id === selection.variantId ? ' selected' : '';
      const disabled = v.status === 'planned' || v.status === 'deprecated';
      const statusKey = 'wizard.status' + v.status.charAt(0).toUpperCase() + v.status.slice(1);
      const statusLabel = I18n.t(statusKey);
      const vram = v.vram_gb_min ? '<span class="badge catalog-badge">VRAM \u2265 ' + v.vram_gb_min + ' GB</span>' : '';
      const ram = v.ram_gb_min ? '<span class="badge catalog-badge">RAM \u2265 ' + v.ram_gb_min + ' GB</span>' : '';
      const mode = '<span class="badge catalog-badge">' + Utils.escapeHtml(v.deploy_mode || '') + '</span>';
      const notes = v.notes_pl ? '<div class="variant-notes" style="font-size:0.85em;color:var(--color-text-muted);margin-top:4px;">' + Utils.escapeHtml(v.notes_pl) + '</div>' : '';

      return '<label class="variant-card' + selectedCls + (disabled ? ' disabled' : '') + '" style="display:block;border:1px solid var(--color-border);border-radius:6px;padding:10px;margin-bottom:8px;cursor:' + (disabled ? 'not-allowed' : 'pointer') + ';opacity:' + (disabled ? '0.5' : '1') + ';">' +
        '<div style="display:flex;align-items:center;gap:8px;">' +
          '<input type="radio" name="edw-variant" value="' + Utils.escapeAttr(v.id) + '"' +
            (v.id === selection.variantId ? ' checked' : '') +
            (disabled ? ' disabled' : '') + '>' +
          '<strong>' + Utils.escapeHtml(v.id) + '</strong>' +
          '<span class="badge badge-' + Utils.escapeAttr(v.status) + '" style="font-size:10px;">' + Utils.escapeHtml(statusLabel) + '</span>' +
          mode + vram + ram +
        '</div>' + notes +
      '</label>';
    }).join('');

    return '<h3>' + I18n.t('wizard.selectVariant') + '</h3>' +
      '<div class="variant-list">' + items + '</div>';
  }

  // ---- Step 3: Build / Download -------------------------------------------

  function renderStepMethod() {
    const variant = selectedVariant();
    if (!variant) {
      return '<p class="empty-state-text">' + I18n.t('wizard.selectVariant') + '</p>';
    }

    const hasBuild = !!variant.build;
    const hasDownload = !!variant.download;
    const proAllowed = LicenseBadge.isProAllowed(licenseInfo);
    const downloadEnabled = hasDownload && variant.download.enabled !== false && proAllowed;

    let html = '<h3>' + I18n.t('wizard.selectMethod') + '</h3>';

    if (hasBuild) {
      const sel = selection.deployMethod === 'build' ? ' checked' : '';
      html +=
        '<label class="method-card" style="display:block;border:1px solid var(--color-border);border-radius:6px;padding:12px;margin-bottom:8px;cursor:pointer;">' +
          '<input type="radio" name="edw-method" value="build"' + sel + '> ' +
          '<strong>' + I18n.t('wizard.build') + '</strong>' +
          '<p style="margin:6px 0 0 24px;color:var(--color-text-muted);font-size:0.9em;">' + I18n.t('wizard.buildDescription') + '</p>' +
        '</label>';
    }

    if (hasDownload) {
      const dl = variant.download;
      const sizeMb = dl && dl.size_mb ? ' (' + dl.size_mb + ' MB)' : '';
      const lockTitle = !proAllowed ? I18n.t('wizard.proRequired') : (dl && dl.enabled === false ? 'Download niedostepny w tej wersji' : '');
      const sel = selection.deployMethod === 'download' && downloadEnabled ? ' checked' : '';
      html +=
        '<label class="method-card' + (downloadEnabled ? '' : ' disabled') + '" style="display:block;border:1px solid var(--color-border);border-radius:6px;padding:12px;margin-bottom:8px;cursor:' + (downloadEnabled ? 'pointer' : 'not-allowed') + ';opacity:' + (downloadEnabled ? '1' : '0.55') + ';" title="' + Utils.escapeAttr(lockTitle) + '">' +
          '<input type="radio" name="edw-method" value="download"' + sel + (downloadEnabled ? '' : ' disabled') + '> ' +
          '<strong>' + I18n.t('wizard.download') + '</strong>' + sizeMb +
          '<p style="margin:6px 0 0 24px;color:var(--color-text-muted);font-size:0.9em;">' + I18n.t('wizard.downloadDescription') + '</p>' +
          (!proAllowed ? '<div style="margin:6px 0 0 24px;font-size:0.85em;color:var(--color-warning);">\uD83D\uDD12 ' + I18n.t('wizard.proRequired') + '</div>' : '') +
        '</label>';
    }

    if (!hasBuild && !hasDownload) {
      html += '<p class="empty-state-text">Brak metod deploymentu dla tego wariantu.</p>';
    }

    return html;
  }

  // ---- Step 4: konfig --------------------------------------------------------

  function renderStepConfig() {
    const eng = engineEntry.engine || {};
    const presets = engineEntry.model_preset || [];
    const port = (selection.config && selection.config.port) || eng.default_port || 8080;
    const presetSel = (selection.config && selection.config.model_preset_id)
      || (presets.find(p => p.recommended) || presets[0] || {}).id || '';

    let html = '<h3>' + I18n.t('wizard.configureRuntime') + '</h3>';

    if (presets.length > 0) {
      const opts = presets.map(p => {
        const sel = p.id === presetSel ? ' selected' : '';
        const star = p.recommended ? ' \u2605' : '';
        return '<option value="' + Utils.escapeAttr(p.id) + '"' + sel + '>' + Utils.escapeHtml(p.display_name) + star + '</option>';
      }).join('');
      html +=
        '<div class="form-group">' +
          '<label for="edw-preset">Model preset</label>' +
          '<select id="edw-preset" class="form-input">' + opts + '</select>' +
        '</div>';
    }

    html +=
      '<div class="form-group">' +
        '<label for="edw-port">Port</label>' +
        '<input type="number" id="edw-port" class="form-input" min="1024" max="65535" value="' + Utils.escapeAttr(String(port)) + '">' +
      '</div>';

    return html;
  }

  // ---- Eventy --------------------------------------------------------------

  function bindStepInputs() {
    const nodeSel = document.getElementById('edw-node-select');
    if (nodeSel) {
      nodeSel.addEventListener('change', () => {
        selection.nodeId = nodeSel.value;
        // Reset wybranego wariantu jesli zmieniono wezel
        selection.variantId = null;
      });
    }

    const variantRadios = document.querySelectorAll('input[name="edw-variant"]');
    variantRadios.forEach(r => {
      r.addEventListener('change', (e) => {
        if (e.target.checked) selection.variantId = e.target.value;
      });
    });

    const methodRadios = document.querySelectorAll('input[name="edw-method"]');
    methodRadios.forEach(r => {
      r.addEventListener('change', (e) => {
        if (e.target.checked) selection.deployMethod = e.target.value;
      });
    });

    const presetSel = document.getElementById('edw-preset');
    if (presetSel) {
      presetSel.addEventListener('change', () => {
        selection.config.model_preset_id = presetSel.value;
      });
    }

    const portInput = document.getElementById('edw-port');
    if (portInput) {
      portInput.addEventListener('input', () => {
        const v = parseInt(portInput.value, 10);
        selection.config.port = isNaN(v) ? portInput.value : v;
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
      // Auto-preselekcja przy wejsciu do kroku 3
      if (currentStep === 3) preselectMethod();
      // Auto-preselekcja przy wejsciu do kroku 4
      if (currentStep === 4) preselectConfig();
      refreshModal();
    });

    const deployBtn = document.getElementById('edw-deploy');
    if (deployBtn) deployBtn.addEventListener('click', startDeploy);
  }

  function canAdvance() {
    if (currentStep === 1) {
      if (!selection.nodeId) {
        if (typeof App !== 'undefined' && App.showToast) App.showToast(I18n.t('wizard.selectNode'), 'error');
        return false;
      }
      return true;
    }
    if (currentStep === 2) {
      if (!selection.variantId) {
        if (typeof App !== 'undefined' && App.showToast) App.showToast(I18n.t('wizard.selectVariant'), 'error');
        return false;
      }
      return true;
    }
    if (currentStep === 3) {
      if (!selection.deployMethod) {
        if (typeof App !== 'undefined' && App.showToast) App.showToast(I18n.t('wizard.selectMethod'), 'error');
        return false;
      }
      return true;
    }
    return true;
  }

  // Preselekcja metody Build/Download na podstawie wariantu i licencji
  function preselectMethod() {
    const variant = selectedVariant();
    if (!variant) return;
    const hasBuild = !!variant.build;
    const hasDownload = !!variant.download;
    const proAllowed = LicenseBadge.isProAllowed(licenseInfo);
    const downloadEnabled = hasDownload && variant.download.enabled !== false && proAllowed;

    if (selection.deployMethod === 'download' && !downloadEnabled) {
      selection.deployMethod = hasBuild ? 'build' : (hasDownload ? 'download' : 'build');
    }
    if (!selection.deployMethod) {
      selection.deployMethod = hasBuild ? 'build' : 'download';
    }
  }

  // Preselekcja portu i model_preset
  function preselectConfig() {
    const eng = engineEntry.engine || {};
    if (selection.config.port === undefined) {
      selection.config.port = eng.default_port || 8080;
    }
    const presets = engineEntry.model_preset || [];
    if (!selection.config.model_preset_id && presets.length > 0) {
      const rec = presets.find(p => p.recommended) || presets[0];
      if (rec) selection.config.model_preset_id = rec.id;
    }
  }

  // ---- Deploy --------------------------------------------------------------

  async function startDeploy() {
    const deployBtn = document.getElementById('edw-deploy');
    if (deployBtn) deployBtn.disabled = true;

    const body = {
      engine_id: engineEntry.engine.id,
      variant_id: selection.variantId,
      deploy_method: selection.deployMethod,
      node_id: selection.nodeId,
      config: selection.config || {}
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
        if (!resp.ok) throw new Error(data && (data.message || data.error_code) || 'HTTP ' + resp.status);
      }

      if (typeof App !== 'undefined' && App.showToast) {
        App.showToast('Deploy wystartowal: ' + (data && data.deploy_id ? data.deploy_id : '?'), 'success');
      }
      // TODO[future iter]: podpiac WebSocket data.websocket_url do live progress
      console.log('[EngineDeployWizard] deploy started:', data);
      setTimeout(close, 1500);
    } catch (err) {
      console.error('[EngineDeployWizard] deploy error:', err);
      if (typeof App !== 'undefined' && App.showToast) {
        App.showToast('Blad: ' + (err.message || err), 'error');
      }
      if (deployBtn) deployBtn.disabled = false;
    }
  }

  return {
    open: open,
    close: close
  };
})();

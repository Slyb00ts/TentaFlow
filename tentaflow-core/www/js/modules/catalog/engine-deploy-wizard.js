// =============================================================================
// Plik: modules/catalog/engine-deploy-wizard.js
// Opis: 3-krokowy wizard deploymentu silnika z manifestu:
//       (1) tryb: docker | native | external (kafelki wg availableDeployMethods)
//       (2) model: preset z manifestu albo wyszukiwarka HuggingFace Hub
//       (3) runtime: port, container name (gdy docker), ewentualne extra params
//       Submit → POST /api/services/deploy.
// =============================================================================

import { escapeHtml, escapeAttr, toast, apiGet, apiPost } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import * as Manifest from '/js/modules/catalog/manifest-store.js';
import { deployIcon, render as renderIcon } from '/js/modules/catalog/catalog-icons.js';

let currentStep = 1;
let engineEntry = null;
let availableMethods = [];
let hostOs = 'linux';
let nodes = [];
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
  containerName: null,
};

/// Publiczne API: otwiera wizard dla `engineId`. `opts` opcjonalnie zawiera
/// `nodeId` (preselekcja z MeshDetail) i `hostOs` (z katalogu).
export async function openDeployWizard(engineId, opts = {}) {
  currentStep = 1;
  modelSourceMode = 'preset';
  hfResults = [];
  hfSearchQuery = '';
  selection = {
    nodeId: opts.nodeId || null,
    deployMethod: null,
    modelPresetId: null,
    modelRepo: null,
    port: null,
    containerName: null,
  };

  renderShell(`<div class="form-hint">${escapeHtml(I18n.t('common.loading'))}</div>`);

  await Manifest.init();
  engineEntry = Manifest.byId(engineId);
  if (!engineEntry) {
    const msg = I18n.t('wizard.engineNotFound').replace('{id}', engineId);
    renderShell(`<div class="form-hint">${escapeHtml(msg)}</div>`);
    return;
  }

  nodes = await fetchNodes();
  if (!selection.nodeId) {
    const local = nodes.find((n) => n?.is_local === true) || nodes[0];
    selection.nodeId = local ? (local.node_id || local.id) : null;
  }

  hostOs = opts.hostOs || pickHostOs(selection.nodeId);
  availableMethods = Manifest.availableDeployMethods(engineEntry, hostOs);

  if (availableMethods.length > 0) {
    selection.deployMethod = availableMethods[0];
  }

  const eng = engineEntry.engine || {};
  selection.port = eng.default_port || 8080;
  selection.containerName = `tentaflow-${(eng.id || 'svc').toLowerCase()}-${randomSuffix()}`;

  const presets = Manifest.modelPresets(engineEntry);
  if (presets.length > 0) {
    const rec = presets.find((p) => p && p.recommended) || presets[0];
    if (rec) selection.modelPresetId = rec.id;
  } else {
    modelSourceMode = 'hf';
  }

  hfToken = await loadHfToken();

  refreshModal();
}

export function close() {
  const el = document.getElementById('engine-deploy-wizard');
  if (el) el.remove();
}

// ---- Data -----------------------------------------------------------------

async function fetchNodes() {
  try {
    const resp = await apiGet('/api/mesh/nodes');
    if (Array.isArray(resp) && resp.length > 0) {
      return resp.filter((n) => n && (n.is_trusted === true || n.is_local === true));
    }
  } catch (err) {
    console.warn('[wizard] fetchNodes fallback:', err);
  }
  return [{ node_id: 'local', id: 'local', is_local: true, platform: defaultUaOs() }];
}

async function loadHfToken() {
  try {
    const settings = await apiGet('/api/settings');
    if (Array.isArray(settings)) {
      const t = settings.find((s) => s && s.key === 'hf_token');
      if (t?.value) return String(t.value);
    }
  } catch {
    // ignore
  }
  return '';
}

function defaultUaOs() {
  const ua = navigator.userAgent.toLowerCase();
  if (ua.includes('mac')) return 'macos';
  if (ua.includes('win')) return 'windows';
  return 'linux';
}

function pickHostOs(nodeId) {
  const node = nodes.find((n) => n && (n.node_id || n.id) === nodeId);
  if (!node) return defaultUaOs();
  const os = node.platform || node.os;
  return os ? String(os).toLowerCase() : defaultUaOs();
}

function randomSuffix(len = 5) {
  const chars = 'abcdefghijklmnopqrstuvwxyz0123456789';
  let r = '';
  for (let i = 0; i < len; i++) r += chars[Math.floor(Math.random() * chars.length)];
  return r;
}

// ---- Shell ----------------------------------------------------------------

function renderShell(bodyHtml) {
  close();
  const overlay = document.createElement('div');
  overlay.className = 'modal-backdrop active';
  overlay.id = 'engine-deploy-wizard';
  overlay.innerHTML = `
    <div class="modal" style="max-width: 720px;">
      <div class="modal-header">
        <h3 id="edw-title">${escapeHtml(I18n.t('wizard.title'))}</h3>
        <button class="modal-close" id="edw-close" aria-label="${escapeAttr(I18n.t('common.close'))}">×</button>
      </div>
      <div class="modal-body" id="edw-body">${bodyHtml}</div>
      <div class="modal-footer" id="edw-footer"></div>
    </div>
  `;
  document.body.appendChild(overlay);
  overlay.querySelector('#edw-close')?.addEventListener('click', close);
  overlay.addEventListener('click', (e) => {
    if (e.target === overlay) close();
  });
}

function refreshModal() {
  const titleEl = document.getElementById('edw-title');
  if (titleEl && engineEntry?.engine) {
    titleEl.textContent = `${I18n.t('wizard.title')}: ${engineEntry.engine.name || engineEntry.engine.id}`;
  }
  const body = document.getElementById('edw-body');
  if (body) body.innerHTML = renderStepIndicator() + renderStepBody();
  const footer = document.getElementById('edw-footer');
  if (footer) footer.innerHTML = renderFooter();
  bindStepInputs();
  bindFooter();
}

function renderStepIndicator() {
  let html = '<div class="wizard-step-indicator">';
  for (let i = 1; i <= 3; i++) {
    const cls = i === currentStep ? 'active' : (i < currentStep ? 'done' : '');
    html += `<div class="wizard-step-dot ${cls}"><span>${i}</span></div>`;
    if (i < 3) html += '<div class="wizard-step-line"></div>';
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
  let html = `<button class="btn btn-ghost" id="edw-cancel">${escapeHtml(I18n.t('common.cancel'))}</button>`;
  if (currentStep > 1) {
    html += `<button class="btn btn-secondary" id="edw-back">← ${escapeHtml(I18n.t('common.back'))}</button>`;
  }
  if (currentStep < 3) {
    html += `<button class="btn btn-primary" id="edw-next">${escapeHtml(I18n.t('common.next'))} →</button>`;
  } else {
    html += `<button class="btn btn-primary" id="edw-deploy">${escapeHtml(I18n.t('wizard.startDeploy'))}</button>`;
  }
  return html;
}

// ---- Step 1: deploy method ------------------------------------------------

function renderStepMethod() {
  if (availableMethods.length === 0) {
    const msg = I18n.t('wizard.noMethodsAvailable').replace('{os}', escapeHtml(hostOs));
    return `
      <h4 class="wizard-step-title">${escapeHtml(I18n.t('wizard.selectMethod'))}</h4>
      <p class="form-hint">${msg}</p>
    `;
  }

  const cards = availableMethods.map((m) => {
    const sel = selection.deployMethod === m ? ' selected' : '';
    const name = I18n.t(`wizard.method.${m}`);
    const desc = I18n.t(`wizard.method.${m}Desc`);
    return `
      <button type="button" class="deploy-method-card${sel}" data-method="${escapeAttr(m)}">
        <div class="dm-ico">${deployIcon(m, 32)}</div>
        <div class="dm-name">${escapeHtml(name)}</div>
        <div class="dm-desc">${escapeHtml(desc)}</div>
      </button>
    `;
  }).join('');

  // Node selector (jeśli są inne node'y)
  let nodeSelector = '';
  if (nodes.length > 1) {
    const options = nodes.map((n) => {
      const id = n.node_id || n.id;
      const label = n.hostname || id;
      const selAttr = selection.nodeId === id ? ' selected' : '';
      const localLabel = n.is_local ? ` (${I18n.t('mesh.local')})` : '';
      return `<option value="${escapeAttr(id)}"${selAttr}>${escapeHtml(label)}${localLabel}</option>`;
    }).join('');
    nodeSelector = `
      <div class="form-group" style="margin-top:16px;">
        <label>${escapeHtml(I18n.t('wizard.targetNode'))}</label>
        <select class="input" id="edw-node-select">${options}</select>
      </div>
    `;
  }

  return `
    <h4 class="wizard-step-title">${escapeHtml(I18n.t('wizard.selectMethod'))}</h4>
    <div class="deploy-method-grid">${cards}</div>
    ${nodeSelector}
  `;
}

// ---- Step 2: model --------------------------------------------------------

function renderStepModel() {
  const presets = Manifest.modelPresets(engineEntry);
  const hasPresets = presets.length > 0;

  let tabs = '<div class="wizard-tabs">';
  if (hasPresets) {
    tabs += `<button type="button" class="wizard-tab${modelSourceMode === 'preset' ? ' active' : ''}" data-mode="preset">${escapeHtml(I18n.t('wizard.fromPreset'))}</button>`;
  }
  tabs += `<button type="button" class="wizard-tab${modelSourceMode === 'hf' ? ' active' : ''}" data-mode="hf">${escapeHtml(I18n.t('wizard.searchHuggingface'))}</button>`;
  tabs += '</div>';

  const content = modelSourceMode === 'preset' && hasPresets
    ? renderPresetSelector(presets)
    : renderHfSearch();

  return `
    <h4 class="wizard-step-title">${escapeHtml(I18n.t('wizard.selectModel'))}</h4>
    ${tabs}
    <div class="wizard-tab-content">${content}</div>
  `;
}

function renderPresetSelector(presets) {
  if (!presets.length) {
    return `<p class="form-hint">${escapeHtml(I18n.t('wizard.noPresets'))}</p>`;
  }
  const items = presets.map((p) => {
    if (!p) return '';
    const id = p.id || '';
    const display = p.display_name || p.repo || id;
    const repo = p.repo || '';
    const quant = p.quantization || '';
    const star = p.recommended ? `<span class="preset-star" title="${escapeAttr(I18n.t('wizard.recommended'))}">${renderIcon('star', 14)}</span>` : '';
    const sel = selection.modelPresetId === id ? ' selected' : '';
    const info = [repo, quant].filter(Boolean).join(' · ');
    return `
      <div class="model-item${sel}" data-preset-id="${escapeAttr(id)}">
        <div class="model-item-main">
          <div class="model-item-name">${escapeHtml(display)} ${star}</div>
          ${info ? `<div class="model-item-info">${escapeHtml(info)}</div>` : ''}
        </div>
      </div>
    `;
  }).join('');

  return `
    <div class="model-list">${items}</div>
    <p class="form-hint">${escapeHtml(I18n.t('wizard.presetHint'))}</p>
  `;
}

function renderHfSearch() {
  const filterHint = hfSearchFilterHint();
  return `
    <div class="form-group">
      <input type="text" id="edw-hf-search" class="input"
        placeholder="${escapeAttr(I18n.t('wizard.hfSearchPlaceholder'))}"
        value="${escapeAttr(hfSearchQuery)}" autocomplete="off">
      <div class="form-hint">${escapeHtml(I18n.t('wizard.hfSearchHint'))}${filterHint ? ' · ' + escapeHtml(filterHint) : ''}</div>
    </div>
    <div class="form-group">
      <label>${escapeHtml(I18n.t('wizard.huggingfaceToken'))}</label>
      <input type="password" id="edw-hf-token" class="input"
        value="${escapeAttr(hfToken)}" autocomplete="off">
    </div>
    <div class="model-list" id="edw-hf-results">${renderHfResultsHtml()}</div>
  `;
}

function renderHfResultsHtml() {
  if (hfSearching) return `<p class="form-hint">${escapeHtml(I18n.t('common.loading'))}</p>`;
  if (hfResults.length === 0) return '';
  return hfResults.map((r) => {
    const id = r.id || r.modelId || '';
    const downloads = r.downloads ? formatCount(r.downloads) : '';
    const likes = r.likes ? r.likes : '';
    const lastModified = r.lastModified ? r.lastModified.substring(0, 10) : '';
    const sel = selection.modelRepo === id ? ' selected' : '';
    const info = [
      downloads && `↓ ${downloads}`,
      likes && `♥ ${likes}`,
      lastModified,
    ].filter(Boolean).join(' · ');
    return `
      <div class="model-item${sel}" data-repo="${escapeAttr(id)}">
        <div class="model-item-main">
          <div class="model-item-name mono">${escapeHtml(id)}</div>
          ${info ? `<div class="model-item-info">${escapeHtml(info)}</div>` : ''}
        </div>
      </div>
    `;
  }).join('');
}

function hfSearchFilterHint() {
  const id = String(engineEntry?.engine?.id || '').toLowerCase();
  if (id.includes('llama') || id.includes('llamacpp')) return 'GGUF';
  if (id === 'mlx') return 'mlx-community/*';
  return '';
}

function formatCount(n) {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

// ---- Step 3: runtime ------------------------------------------------------

function renderStepRuntime() {
  const eng = engineEntry?.engine || {};
  const port = selection.port || eng.default_port || 8080;
  const cname = selection.containerName || '';

  let summary = '';
  if (selection.modelRepo) {
    summary = `
      <div class="form-group">
        <label>${escapeHtml(I18n.t('wizard.modelLabel'))}</label>
        <div><code>${escapeHtml(selection.modelRepo)}</code> <span class="form-hint inline">(HuggingFace)</span></div>
      </div>
    `;
  } else if (selection.modelPresetId) {
    const preset = Manifest.modelPresets(engineEntry).find((p) => p?.id === selection.modelPresetId);
    if (preset) {
      summary = `
        <div class="form-group">
          <label>${escapeHtml(I18n.t('wizard.modelLabel'))}</label>
          <div><strong>${escapeHtml(preset.display_name || preset.id)}</strong>${preset.repo ? ` <span class="form-hint inline">${escapeHtml(preset.repo)}</span>` : ''}</div>
        </div>
      `;
    }
  }

  let extra = '';
  if (selection.deployMethod === 'docker') {
    extra = `
      <div class="form-group">
        <label>${escapeHtml(I18n.t('wizard.containerName'))}</label>
        <input type="text" id="edw-cname" class="input" value="${escapeAttr(cname)}">
      </div>
    `;
  }

  return `
    <h4 class="wizard-step-title">${escapeHtml(I18n.t('wizard.configureRuntime'))}</h4>
    ${summary}
    <div class="form-group">
      <label>${escapeHtml(I18n.t('wizard.port'))}</label>
      <input type="number" id="edw-port" class="input" min="1" max="65535" value="${escapeAttr(String(port))}">
    </div>
    ${extra}
  `;
}

// ---- Bindings -------------------------------------------------------------

function bindStepInputs() {
  if (currentStep === 1) bindStepMethodInputs();
  if (currentStep === 2) bindStepModelInputs();
  if (currentStep === 3) bindStepRuntimeInputs();
}

function bindStepMethodInputs() {
  document.querySelectorAll('.deploy-method-card[data-method]').forEach((btn) => {
    btn.addEventListener('click', () => {
      selection.deployMethod = btn.dataset.method;
      refreshModal();
    });
  });
  const nodeSel = document.getElementById('edw-node-select');
  if (nodeSel) {
    nodeSel.addEventListener('change', () => {
      selection.nodeId = nodeSel.value;
      hostOs = pickHostOs(selection.nodeId);
      availableMethods = Manifest.availableDeployMethods(engineEntry, hostOs);
      if (!availableMethods.includes(selection.deployMethod)) {
        selection.deployMethod = availableMethods[0] || null;
      }
      refreshModal();
    });
  }
}

function bindStepModelInputs() {
  document.querySelectorAll('.wizard-tab[data-mode]').forEach((t) => {
    t.addEventListener('click', () => {
      modelSourceMode = t.dataset.mode;
      refreshModal();
    });
  });

  document.querySelectorAll('.model-item[data-preset-id]').forEach((it) => {
    it.addEventListener('click', () => {
      selection.modelPresetId = it.dataset.presetId;
      selection.modelRepo = null;
      document.querySelectorAll('.model-item[data-preset-id]').forEach((x) => x.classList.remove('selected'));
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
  document.querySelectorAll('.model-item[data-repo]').forEach((it) => {
    it.addEventListener('click', () => {
      selection.modelRepo = it.dataset.repo;
      selection.modelPresetId = null;
      document.querySelectorAll('.model-item[data-repo]').forEach((x) => x.classList.remove('selected'));
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
  document.getElementById('edw-cancel')?.addEventListener('click', close);
  document.getElementById('edw-back')?.addEventListener('click', () => {
    if (currentStep > 1) {
      currentStep--;
      refreshModal();
    }
  });
  document.getElementById('edw-next')?.addEventListener('click', () => {
    if (!canAdvance()) return;
    currentStep++;
    refreshModal();
  });
  document.getElementById('edw-deploy')?.addEventListener('click', startDeploy);
}

function canAdvance() {
  if (currentStep === 1) {
    if (!selection.deployMethod) {
      toast(I18n.t('wizard.selectMethod'), 'error');
      return false;
    }
    return true;
  }
  if (currentStep === 2) {
    if (!selection.modelPresetId && !selection.modelRepo) {
      toast(I18n.t('wizard.selectModel'), 'error');
      return false;
    }
    return true;
  }
  return true;
}

// ---- HF search ------------------------------------------------------------

async function doHfSearch(query) {
  hfSearching = true;
  updateHfResults();
  try {
    const url = `https://huggingface.co/api/models?search=${encodeURIComponent(query)}&limit=20`;
    const headers = {};
    if (hfToken) headers['Authorization'] = `Bearer ${hfToken}`;
    const resp = await fetch(url, { headers });
    if (!resp.ok) throw new Error(`HF API ${resp.status}`);
    let data = await resp.json();
    if (!Array.isArray(data)) data = [];

    const engId = String(engineEntry?.engine?.id || '').toLowerCase();
    if (engId.includes('llama') || engId.includes('llamacpp')) {
      data = data.filter((m) => String(m.id || '').toLowerCase().includes('gguf'));
    } else if (engId === 'mlx') {
      data = data.filter((m) => {
        const id = String(m.id || '').toLowerCase();
        return id.includes('mlx-') || id.includes('mlx-community/');
      });
    }
    hfResults = data;
  } catch (err) {
    console.error('[wizard] HF search error:', err);
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

// ---- Deploy ---------------------------------------------------------------

async function startDeploy() {
  const btn = document.getElementById('edw-deploy');
  if (btn) btn.disabled = true;

  const eng = engineEntry.engine || {};
  const body = {
    engine_id: eng.id,
    deploy_method: selection.deployMethod,
    node_id: selection.nodeId,
    config: {
      model_preset_id: selection.modelPresetId || null,
      model_repo: selection.modelRepo || null,
      port: selection.port || eng.default_port,
      container_name: selection.containerName || null,
    },
  };

  try {
    const data = await apiPost('/api/services/deploy', body);
    const id = data?.deploy_id || data?.deployId || '?';
    toast(I18n.t('wizard.deployStarted').replace('{id}', id), 'success');
    setTimeout(close, 1200);
  } catch (err) {
    toast(I18n.t('wizard.deployFailed').replace('{error}', err.message || err), 'error');
    if (btn) btn.disabled = false;
  }
}

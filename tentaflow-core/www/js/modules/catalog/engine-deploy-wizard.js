// =============================================================================
// File: modules/catalog/engine-deploy-wizard.js
// Purpose: 4-step engine deploy wizard driven by service manifest.
//   (1) method: docker | native | external (tiles from availableDeployMethods)
//   (2) model:  preset from manifest or HuggingFace Hub search
//   (3) gpu:    pick GPUs on the selected node (all | specific | none)
//   (4) runtime: port, container name (docker) and extras
//   Submit → POST /api/services/deploy.
// =============================================================================

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { ApiBinary } from '/js/protocol/api-binary-shim.js';
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
  gpuSelectMode: 'all',   // 'all' | 'specific' | 'none'
  gpuIds: [],             // e.g. ['0','2'] when gpuSelectMode === 'specific'
};

// Cache per-node GPU lists to avoid re-querying when switching back and forth.
const gpuListByNode = new Map();

// Ordered step ids with optional skip predicate. Runtime order derived at
// navigation time by filtering out steps whose skip() returns true.
const STEPS = [
  { id: 'method' },
  { id: 'model', skip: shouldSkipModelStep },
  { id: 'gpu', skip: shouldSkipGpuStep },
  { id: 'runtime' },
];

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
    gpuSelectMode: 'all',
    gpuIds: [],
  };
  gpuListByNode.clear();

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
  if (el) {
    if (typeof el.close === 'function') el.close(true);
    else el.remove();
  }
  const backdrop = document.getElementById('engine-deploy-wizard-backdrop');
  if (backdrop) backdrop.remove();
}

// ---- Data -----------------------------------------------------------------

async function fetchNodes() {
  try {
    const resp = await ApiBinary.list('meshNodeListRequest', { arrayKey: 'nodes' });
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
    const entries = await ApiBinary.list('settingsListRequest', { arrayKey: 'entries' });
    if (Array.isArray(entries)) {
      const t = entries.find((s) => s && s.key === 'hf_token');
      // Wartosci sekretow sa redaktowane przez protokol — traktujemy jako brak.
      if (t?.value && t.value !== '<redacted>') return String(t.value);
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
  const backdrop = document.createElement('div');
  backdrop.className = 'tf-window-backdrop';
  backdrop.id = 'engine-deploy-wizard-backdrop';
  document.body.appendChild(backdrop);

  const win = document.createElement('tf-window');
  win.id = 'engine-deploy-wizard';
  win.setAttribute('title', I18n.t('wizard.title'));
  win.setAttribute('buttons', 'close');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.setAttribute('width', '720');
  win.innerHTML = `
    <div slot="body" id="edw-body">${bodyHtml}</div>
    <div slot="footer" id="edw-footer"></div>
  `;
  document.body.appendChild(win);

  win.addEventListener('close-request', () => {
    backdrop.remove();
  });
  backdrop.addEventListener('click', () => close());
}

function refreshModal() {
  const win = document.getElementById('engine-deploy-wizard');
  if (win && engineEntry?.engine) {
    win.setAttribute('title', `${I18n.t('wizard.title')}: ${engineEntry.engine.name || engineEntry.engine.id}`);
  }
  const body = document.getElementById('edw-body');
  if (body) body.innerHTML = renderStepIndicator() + renderStepBody();
  const footer = document.getElementById('edw-footer');
  if (footer) footer.innerHTML = renderFooter();
  bindStepInputs();
  bindFooter();
}

function activeSteps() {
  return STEPS.filter((s) => !(typeof s.skip === 'function' && s.skip()));
}

function currentStepId() {
  const steps = activeSteps();
  const idx = Math.max(1, Math.min(currentStep, steps.length));
  return steps[idx - 1]?.id;
}

function renderStepIndicator() {
  const steps = activeSteps();
  let html = '<div class="wizard-step-indicator">';
  for (let i = 1; i <= steps.length; i++) {
    const cls = i === currentStep ? 'active' : (i < currentStep ? 'done' : '');
    html += `<div class="wizard-step-dot ${cls}"><span>${i}</span></div>`;
    if (i < steps.length) html += '<div class="wizard-step-line"></div>';
  }
  html += '</div>';
  return html;
}

function renderStepBody() {
  switch (currentStepId()) {
    case 'method':  return renderStepMethod();
    case 'model':   return renderStepModel();
    case 'gpu':     return renderStepGpu();
    case 'runtime': return renderStepRuntime();
    default: return '';
  }
}

function renderFooter() {
  const steps = activeSteps();
  let html = `<tf-button variant="ghost" id="edw-cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>`;
  if (currentStep > 1) {
    html += `<tf-button variant="secondary" id="edw-back"><svg width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" style="transform:rotate(180deg)"><use href="#i-chevron-right"/></svg>${escapeHtml(I18n.t('common.back'))}</tf-button>`;
  }
  if (currentStep < steps.length) {
    html += `<tf-button variant="primary" id="edw-next">${escapeHtml(I18n.t('common.next'))}<svg width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><use href="#i-chevron-right"/></svg></tf-button>`;
  } else {
    html += `<tf-button variant="primary" id="edw-deploy">${escapeHtml(I18n.t('wizard.startDeploy'))}</tf-button>`;
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
        <tf-select id="edw-node-select" value="${escapeAttr(selection.nodeId || '')}">${options}</tf-select>
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

  let tabs = `<tf-tabs variant="underline" id="edw-model-tabs" value="${escapeAttr(modelSourceMode)}">`;
  if (hasPresets) {
    tabs += `<tf-tab id="preset">${escapeHtml(I18n.t('wizard.fromPreset'))}</tf-tab>`;
  }
  tabs += `<tf-tab id="hf">${escapeHtml(I18n.t('wizard.searchHuggingface'))}</tf-tab>`;
  tabs += '</tf-tabs>';

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
  const hintText = `${I18n.t('wizard.hfSearchHint')}${filterHint ? ' · ' + filterHint : ''}`;
  return `
    <div class="form-group">
      <tf-input type="text" id="edw-hf-search"
        placeholder="${escapeAttr(I18n.t('wizard.hfSearchPlaceholder'))}"
        value="${escapeAttr(hfSearchQuery)}" autocomplete="off"
        hint="${escapeAttr(hintText)}"></tf-input>
    </div>
    <div class="form-group">
      <tf-input type="password" id="edw-hf-token"
        label="${escapeAttr(I18n.t('wizard.huggingfaceToken'))}"
        value="${escapeAttr(hfToken)}" autocomplete="off"></tf-input>
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
        <tf-input type="text" id="edw-cname"
          label="${escapeAttr(I18n.t('wizard.containerName'))}"
          value="${escapeAttr(cname)}"></tf-input>
      </div>
    `;
  }

  return `
    <h4 class="wizard-step-title">${escapeHtml(I18n.t('wizard.configureRuntime'))}</h4>
    ${summary}
    <div class="form-group">
      <tf-input type="number" id="edw-port"
        label="${escapeAttr(I18n.t('wizard.port'))}"
        value="${escapeAttr(String(port))}"></tf-input>
    </div>
    ${extra}
  `;
}

// ---- Step 3: GPUs ---------------------------------------------------------

// Model selection step ma sens tylko dla engines gdzie deploy wymaga modelu —
// LLM, STT, TTS, embeddings, vision, image-gen itd. Agenty (teams-bot) i tools
// są self-contained — nie pobierają modeli HuggingFace przy deploy. Manifest
// może jawnie wymusić przez `engine.requires_model = true/false`; bez tego
// heurystyka po category + obecności [[model_preset]].
function shouldSkipModelStep() {
  const eng = engineEntry?.engine;
  if (!eng) return false;
  if (eng.requires_model === false) return true;
  if (eng.requires_model === true) return false;
  const category = String(eng.category || '').toLowerCase();
  const modelOptional = new Set(['agents', 'tools']);
  if (!modelOptional.has(category)) return false;
  const presets = Manifest.modelPresets(engineEntry);
  return !presets || presets.length === 0;
}

// The GPU step is skipped when there are no GPUs on the selected node. The
// engine manifest may opt out via `engine.gpu_supported === false`; by default
// (field absent) we assume the engine can use GPUs if the node has any.
function shouldSkipGpuStep() {
  const gpus = nodeGpus(selection.nodeId);
  if (gpus.length === 0) return true;
  const gpuSupported = engineEntry?.engine?.gpu_supported;
  if (gpuSupported === false) return true;
  return false;
}

function nodeGpus(nodeId) {
  if (!nodeId) return [];
  if (gpuListByNode.has(nodeId)) return gpuListByNode.get(nodeId);
  const node = nodes.find((n) => n && (n.node_id || n.id) === nodeId);
  const gpus = Array.isArray(node?.gpus) ? node.gpus : [];
  gpuListByNode.set(nodeId, gpus);
  return gpus;
}

function nodeDisplayName(nodeId) {
  const node = nodes.find((n) => n && (n.node_id || n.id) === nodeId);
  return node?.hostname || node?.node_id || node?.id || nodeId || '';
}

function fmtMb(mb) {
  const n = Number(mb) || 0;
  if (n <= 0) return '—';
  if (n >= 1024) return `${Math.round(n / 1024)} GB`;
  return `${Math.round(n)} MB`;
}

function vendorStatus(vendor) {
  const v = String(vendor || '').toLowerCase();
  if (v.includes('nvidia')) return 'accent';
  if (v.includes('amd')) return 'warn';
  if (v.includes('intel')) return 'info';
  return 'info';
}

function gpuSummaryText(gpus) {
  if (selection.gpuSelectMode === 'none') return I18n.t('wizard.gpu_summary_none');
  if (selection.gpuSelectMode === 'all') return I18n.t('wizard.gpu_summary_all');
  const ids = new Set(selection.gpuIds);
  const chosen = gpus.filter((_, idx) => ids.has(String(idx)));
  const totalVram = chosen.reduce((s, g) => s + (g.vram_total_mb || 0), 0);
  return I18n.t('wizard.gpu_summary_specific', { n: chosen.length, total_vram: fmtMb(totalVram) });
}

function renderStepGpu() {
  const gpus = nodeGpus(selection.nodeId);
  const mode = selection.gpuSelectMode || 'all';
  const selectedSet = new Set(selection.gpuIds);

  const rows = gpus.map((g, idx) => {
    const meta = [
      `${fmtMb(g.vram_total_mb)} VRAM`,
      g.usage_percent != null ? `util ${Math.round(g.usage_percent)}%` : '',
      g.temperature_c != null ? `${Math.round(g.temperature_c)}°C` : '',
      g.driver_version ? `driver ${escapeHtml(String(g.driver_version))}` : '',
    ].filter(Boolean).join(' · ');
    const checked = selectedSet.has(String(idx)) ? 'checked' : '';
    const vendor = g.vendor || '—';
    return `
      <label class="gpu-row">
        <input type="checkbox" value="${idx}" ${checked}>
        <div class="gpu-info">
          <div class="gpu-name">GPU ${idx} · ${escapeHtml(String(g.name || ''))}</div>
          <div class="gpu-meta">${meta}</div>
        </div>
        <tf-chip status="${escapeAttr(vendorStatus(vendor))}">${escapeHtml(String(vendor))}</tf-chip>
      </label>
    `;
  }).join('');

  const listHidden = mode !== 'specific' ? 'hidden' : '';
  const nodeName = escapeHtml(nodeDisplayName(selection.nodeId));

  return `
    <h4 class="wizard-step-title">${escapeHtml(I18n.t('wizard.gpu_title', { node: nodeName }))}</h4>
    <p class="form-hint">${escapeHtml(I18n.t('wizard.gpu_subtitle'))}</p>

    <div class="gpu-mode-group">
      <label>
        <input type="radio" name="gpu-mode" value="all" ${mode === 'all' ? 'checked' : ''}>
        <span>${escapeHtml(I18n.t('wizard.gpu_mode_all', { n: gpus.length }))}</span>
      </label>
      <label>
        <input type="radio" name="gpu-mode" value="specific" ${mode === 'specific' ? 'checked' : ''}>
        <span>${escapeHtml(I18n.t('wizard.gpu_mode_specific'))}</span>
      </label>
      <label>
        <input type="radio" name="gpu-mode" value="none" ${mode === 'none' ? 'checked' : ''}>
        <span>${escapeHtml(I18n.t('wizard.gpu_mode_none'))}</span>
      </label>
    </div>

    <div class="gpu-list" ${listHidden}>${rows}</div>

    <div class="gpu-summary form-hint">${escapeHtml(gpuSummaryText(gpus))}</div>
  `;
}

function bindStepGpuInputs() {
  document.querySelectorAll('input[name="gpu-mode"]').forEach((radio) => {
    radio.addEventListener('change', () => {
      if (!radio.checked) return;
      const mode = radio.value;
      selection.gpuSelectMode = mode;
      if (mode === 'all' || mode === 'none') {
        selection.gpuIds = [];
      } else if (mode === 'specific' && selection.gpuIds.length === 0) {
        const gpus = nodeGpus(selection.nodeId);
        if (gpus.length > 0) selection.gpuIds = ['0'];
      }
      refreshModal();
    });
  });

  document.querySelectorAll('.gpu-list input[type="checkbox"]').forEach((cb) => {
    cb.addEventListener('change', () => {
      const id = String(cb.value);
      const set = new Set(selection.gpuIds);
      if (cb.checked) set.add(id); else set.delete(id);
      selection.gpuIds = Array.from(set).sort((a, b) => Number(a) - Number(b));
      const box = document.querySelector('.gpu-summary');
      if (box) box.textContent = gpuSummaryText(nodeGpus(selection.nodeId));
    });
  });
}

// ---- Bindings -------------------------------------------------------------

function bindStepInputs() {
  switch (currentStepId()) {
    case 'method':  bindStepMethodInputs(); break;
    case 'model':   bindStepModelInputs(); break;
    case 'gpu':     bindStepGpuInputs(); break;
    case 'runtime': bindStepRuntimeInputs(); break;
  }
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
    nodeSel.addEventListener('change', (e) => {
      selection.nodeId = e.detail?.value ?? nodeSel.value;
      hostOs = pickHostOs(selection.nodeId);
      availableMethods = Manifest.availableDeployMethods(engineEntry, hostOs);
      if (!availableMethods.includes(selection.deployMethod)) {
        selection.deployMethod = availableMethods[0] || null;
      }
      // GPU inventory is per-node; reset selection when target changes.
      selection.gpuSelectMode = 'all';
      selection.gpuIds = [];
      refreshModal();
    });
  }
}

function bindStepModelInputs() {
  const modelTabs = document.getElementById('edw-model-tabs');
  if (modelTabs) {
    modelTabs.addEventListener('change', (e) => {
      modelSourceMode = e.detail?.value || 'preset';
      refreshModal();
    });
  }

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
    search.addEventListener('input', (e) => {
      clearTimeout(hfSearchTimer);
      const v = e.detail?.value ?? search.value;
      hfSearchQuery = v;
      const q = String(v).trim();
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
    tokenInput.addEventListener('input', (e) => {
      hfToken = e.detail?.value ?? tokenInput.value;
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
    portInput.addEventListener('input', (e) => {
      const raw = e.detail?.value ?? portInput.value;
      const v = parseInt(raw, 10);
      selection.port = isNaN(v) ? raw : v;
    });
  }
  const cnameInput = document.getElementById('edw-cname');
  if (cnameInput) {
    cnameInput.addEventListener('input', (e) => {
      const raw = e.detail?.value ?? cnameInput.value;
      selection.containerName = String(raw).trim();
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
  switch (currentStepId()) {
    case 'method':
      if (!selection.deployMethod) {
        toast(I18n.t('wizard.selectMethod'), 'error');
        return false;
      }
      return true;
    case 'model':
      if (!selection.modelPresetId && !selection.modelRepo) {
        toast(I18n.t('wizard.selectModel'), 'error');
        return false;
      }
      return true;
    case 'gpu':
      if (selection.gpuSelectMode === 'specific' && selection.gpuIds.length === 0) {
        toast(I18n.t('wizard.gpu_select_at_least_one'), 'error');
        return false;
      }
      return true;
    default:
      return true;
  }
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
  if (btn) btn.setAttribute('disabled', '');

  const eng = engineEntry.engine || {};
  const configJson = JSON.stringify({
    model_preset_id: selection.modelPresetId || null,
    model_repo: selection.modelRepo || null,
    port: selection.port || eng.default_port,
    container_name: selection.containerName || null,
    gpu_select_mode: selection.gpuSelectMode,
    gpu_ids: selection.gpuSelectMode === 'specific' ? selection.gpuIds : null,
  });

  try {
    const data = await ApiBinary.action('serviceManifestDeployRequest', {
      engineId: eng.id,
      deployMethod: selection.deployMethod,
      nodeId: selection.nodeId,
      configJson,
    });
    const id = data?.deployId || '';
    if (!id) throw new Error('brak deployId w odpowiedzi serwera');
    toast(I18n.t('wizard.deployStarted').replace('{id}', id), 'success');
    // Zamknij wizard i pokaż live progress modal. Progress subscribes do
    // deploymentLogStreamRequest i pokazuje pasek + tail logów do zakończenia.
    close();
    const mod = await import('/js/modules/catalog/deploy-progress-modal.js');
    mod.openDeployProgressModal({
      deployId: id,
      engineId: eng.id,
      deployMethod: selection.deployMethod,
    });
  } catch (err) {
    toast(I18n.t('wizard.deployFailed').replace('{error}', err.message || err), 'error');
    if (btn) btn.removeAttribute('disabled');
  }
}

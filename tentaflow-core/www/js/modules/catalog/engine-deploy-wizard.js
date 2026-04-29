// =============================================================================
// File: modules/catalog/engine-deploy-wizard.js
// Purpose: 4-step engine deploy wizard driven by service manifest.
//   (1) method: docker | native | external (tiles from availableDeployMethods)
//   (2) model:  preset from manifest or HuggingFace Hub search
//   (3) gpu:    pick GPUs on the selected node (all | specific | none)
//   (4) runtime: port and deploy target name for docker, with compose-stack
//       manifests using a stack/project name instead of a single container name
//   Submit → POST /api/services/deploy.
// =============================================================================

import { escapeHtml, escapeAttr, toast, apiPost } from '/js/utils.js';
import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import * as Manifest from '/js/modules/catalog/manifest-store.js';
import { deployIcon, render as renderIcon } from '/js/modules/catalog/catalog-icons.js';

let currentStep = 1;
let engineEntry = null;
let availableMethods = [];
let hostOs = 'linux';
let nodes = [];
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
  { id: 'advanced', skip: shouldSkipAdvancedStep },
  { id: 'runtime' },
];

// Cache ostatniego wyniku /api/deploy/vllm/recommend (key: model+gpu_ids hash).
// Pozwala przeliczyc VRAM lokalnie przy zmianie suwaka bez ponownego HF fetch.
let advancedRecommendation = null;
let advancedRecommendDebounceTimer = null;

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
    // Advanced (vLLM Auto-tuned) - wartosci uzywane do build vllm_args
    advanced: {
      mode: 'auto',  // 'auto' = use recommended, 'manual' = override
      tensor_parallel: null,       // null = auto-pick
      pipeline_parallel: null,
      max_model_len: null,
      max_num_seqs: null,
      kv_cache_dtype: 'auto',
      gpu_memory_utilization: 0.9,
    },
  };
  advancedRecommendation = null;
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
  if (nodes.length === 0) {
    renderShell(`<div class="form-hint">${escapeHtml(I18n.t('wizard.noNodesAvailable'))}</div>`);
    return;
  }
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
      // MeshNodeInfo proto nie ma pola `is_trusted` — backend zwraca tylko
      // `source` ("local"|"trusted"|"discovered"). Dlatego filtrujemy po
      // is_local + source==="trusted", inaczej paired peery wypadaja z
      // listy i wizard pokazuje tylko lokalny node.
      return resp.filter((n) => n && (n.is_local === true || n.source === 'trusted'));
    }
  } catch (err) {
    console.warn('[wizard] fetchNodes:', err);
  }
  return [];
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

function dockerSection() {
  return engineEntry?.deploy?.docker || null;
}

function usesDockerCompose() {
  const docker = dockerSection();
  return !!(docker && docker.compose_path);
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
    case 'method':   return renderStepMethod();
    case 'model':    return renderStepModel();
    case 'gpu':      return renderStepGpu();
    case 'advanced': return renderStepAdvanced();
    case 'runtime':  return renderStepRuntime();
    default: return '';
  }
}

// Step Advanced wyswietlamy TYLKO dla LLM silnikow ktore akceptuja
// VLLM_ARGS-style override (vllm/sglang/llama-cpp). Inne silniki (TTS/STT/
// vision/image-gen) maja stalsze konfiguracje i nie maja kalkulatora VRAM.
function shouldSkipAdvancedStep() {
  const eng = engineEntry?.engine || {};
  const id = String(eng.id || '').toLowerCase();
  if (!['vllm', 'sglang', 'llama-cpp', 'tensorrt-llm'].includes(id)) return true;
  // Bez wybranego modelu nie ma jak liczyc VRAM
  if (!selection.modelRepo && !selection.modelPresetId) return true;
  // Bez wybranych GPU tez nie - kalkulator wymaga at least 1 GPU
  if (selection.gpuSelectMode === 'none') return true;
  return false;
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

// ---- Step Advanced: vLLM Auto-tuned -------------------------------------
// Inteligentny kalkulator VRAM. Czyta config.json modelu z HF, smart-pick
// TP/PP zgodne z liczba attention heads i hidden layers, suwaki ctx_len /
// max_seqs / kv_dtype / gpu_mem_util z hard limits ile VRAM zostaje (suwak
// nie pozwoli ustawic czegos co nie miesci sie w VRAM).

function getAdvancedModelName() {
  if (selection.modelRepo) return selection.modelRepo;
  if (selection.modelPresetId) {
    const presets = Manifest.modelPresets(engineEntry);
    const preset = presets.find((p) => p?.id === selection.modelPresetId);
    return preset?.repo || null;
  }
  return null;
}

function getAdvancedGpus() {
  const node = nodes.find((n) => (n.node_id || n.id) === selection.nodeId);
  if (!node) return [];
  const allGpus = (node.gpus || []).map((g, i) => ({
    index: g.index ?? i,
    name: g.name || 'GPU',
    memory_gb: Math.round(((g.vram_total_mb || g.memory_mb || 0) / 1024) * 10) / 10,
  }));
  if (selection.gpuSelectMode === 'specific') {
    const ids = new Set((selection.gpuIds || []).map(String));
    return allGpus.filter((g) => ids.has(String(g.index)));
  }
  return allGpus; // 'all'
}

async function fetchVllmRecommendation(overrides = {}) {
  const model = getAdvancedModelName();
  const gpus = getAdvancedGpus();
  if (!model || gpus.length === 0) return null;
  const body = {
    model,
    gpus,
    ...overrides,
  };
  try {
    return await apiPost('/api/deploy/vllm/recommend', body);
  } catch (err) {
    return { error: err.message || String(err) };
  }
}

function renderStepAdvanced() {
  const model = getAdvancedModelName() || '?';
  const gpus = getAdvancedGpus();
  const totalVramGb = gpus.reduce((acc, g) => acc + g.memory_gb, 0);

  const adv = selection.advanced;
  const summaryHtml = advancedRecommendation && !advancedRecommendation.error
    ? renderRecommendationSummary(advancedRecommendation)
    : `<div class="form-hint" id="edw-adv-loading">Pobieram config modelu z HuggingFace i kalkuluje VRAM...</div>`;

  return `
    <h4 class="wizard-step-title">Zaawansowane: vLLM Auto-tuned</h4>
    <div class="form-group">
      <label>Model</label>
      <div><code>${escapeHtml(model)}</code></div>
    </div>
    <div class="form-group">
      <label>Wybrane GPU (${gpus.length} × ${totalVramGb.toFixed(1)} GB total)</label>
      <div class="form-hint">${gpus.map((g) => `GPU ${g.index} (${g.memory_gb} GB)`).join(', ') || '—'}</div>
    </div>

    <div id="edw-adv-summary">${summaryHtml}</div>

    <div class="form-group" style="margin-top: 16px;">
      <label>Tryb konfiguracji</label>
      <div style="display:flex; gap:8px;">
        <tf-button size="sm" variant="${adv.mode === 'auto' ? 'primary' : 'secondary'}" data-adv-mode="auto">Auto-tuned</tf-button>
        <tf-button size="sm" variant="${adv.mode === 'manual' ? 'primary' : 'secondary'}" data-adv-mode="manual">Ręczna</tf-button>
      </div>
    </div>

    ${adv.mode === 'manual' ? renderAdvancedManualControls(adv) : `
      <div class="form-hint">
        Auto-tuned używa rekomendowanej konfiguracji (TP/PP zgodne z heads/layers, kv-cache fp8 dla wiekszych modeli, max_model_len ograniczone do tego co fits w VRAM).
        Przelacz na "Ręczna" aby skonfigurowac suwakami.
      </div>
    `}
  `;
}

function renderRecommendationSummary(rec) {
  if (rec.error) {
    return `<div class="form-hint" style="color:var(--color-error,#c00)">${escapeHtml(rec.error)}</div>`;
  }
  const m = rec.model_spec || {};
  const v = rec.vram_estimate || {};
  const r = rec.recommended || {};
  const totalGpus = (getAdvancedGpus() || []).length || 1;
  const totalVram = (getAdvancedGpus() || []).reduce((a, g) => a + g.memory_gb, 0);
  const usedTotal = (v.per_gpu_gb || 0) * (r.tensor_parallel || 1) * (r.pipeline_parallel || 1);
  const fillPct = Math.min(100, Math.round((v.per_gpu_gb || 0) / Math.max(1, totalVram / totalGpus) * 100));
  const fillColor = v.fits_per_gpu ? '#4caf50' : '#f44336';

  const warningsHtml = (rec.warnings || []).length > 0
    ? `<ul style="margin: 8px 0; padding-left: 20px; color: var(--color-warn, #b87100); font-size: 12px;">
         ${rec.warnings.map((w) => `<li>${escapeHtml(w)}</li>`).join('')}
       </ul>`
    : '';

  return `
    <div style="background: var(--color-surface-alt, #f5f5f5); padding: 12px; border-radius: 6px;">
      <div style="display:grid; grid-template-columns: 1fr 1fr; gap:8px; font-size: 13px;">
        <div><strong>Architektura:</strong> ${escapeHtml(m.model_type || '?')} (${escapeHtml((m.architectures || []).join(', ') || '?')})</div>
        <div><strong>Parametry:</strong> ${(m.estimated_params_billions || 0).toFixed(1)}B (${escapeHtml(m.dtype || '?')}${m.quantization ? `, ${escapeHtml(m.quantization)}` : ''})</div>
        <div><strong>Layers / Heads:</strong> ${m.num_hidden_layers || 0} / ${m.num_attention_heads || 0} (KV: ${m.num_key_value_heads || 0})</div>
        <div><strong>Max ctx (model):</strong> ${(m.max_position_embeddings || 0).toLocaleString()}</div>
      </div>
      <div style="margin-top: 12px;">
        <strong>Konfiguracja rekomendowana:</strong> TP=${r.tensor_parallel} × PP=${r.pipeline_parallel}, ctx=${(r.max_model_len || 0).toLocaleString()}, max_seqs=${r.max_num_seqs}, kv=${escapeHtml(r.kv_cache_dtype || 'auto')}
      </div>
      <div style="margin-top: 8px;">
        <div style="display:flex; justify-content: space-between; font-size:12px; margin-bottom: 4px;">
          <span>VRAM per GPU: ${(v.per_gpu_gb || 0).toFixed(1)} / ${(totalVram / totalGpus).toFixed(1)} GB (${fillPct}%)</span>
          <span>Total: ${(usedTotal).toFixed(1)} / ${totalVram.toFixed(1)} GB</span>
        </div>
        <div style="height:8px; background: #ddd; border-radius:4px; overflow: hidden;">
          <div style="width: ${fillPct}%; height:100%; background: ${fillColor}; transition: width 0.3s;"></div>
        </div>
        <div style="font-size:12px; margin-top:4px; color: var(--color-text-muted, #666);">
          Weights: ${(v.model_weights_gb || 0).toFixed(1)} GB · KV cache: ${(v.kv_cache_gb || 0).toFixed(1)} GB · Activations: ${(v.activations_gb || 0).toFixed(1)} GB
        </div>
      </div>
      ${warningsHtml}
      <details style="margin-top: 8px; font-size: 12px;">
        <summary style="cursor: pointer;">VLLM_ARGS (rozwijany)</summary>
        <pre style="background: #eee; padding: 6px; border-radius: 4px; overflow-x: auto; margin-top: 4px;">${escapeHtml(rec.recommended_vllm_args || '')}</pre>
      </details>
    </div>
  `;
}

function renderAdvancedManualControls(adv) {
  const rec = advancedRecommendation || {};
  const recCfg = rec.recommended || {};
  const maxCtx = rec.max_supported_model_len || 32768;
  const maxSeqs = rec.max_supported_num_seqs || 256;
  const tp = adv.tensor_parallel ?? recCfg.tensor_parallel ?? 1;
  const pp = adv.pipeline_parallel ?? recCfg.pipeline_parallel ?? 1;
  const ctx = adv.max_model_len ?? recCfg.max_model_len ?? 8192;
  const seqs = adv.max_num_seqs ?? recCfg.max_num_seqs ?? 16;
  const kv = adv.kv_cache_dtype || recCfg.kv_cache_dtype || 'auto';
  const memUtil = adv.gpu_memory_utilization ?? recCfg.gpu_memory_utilization ?? 0.9;
  const totalGpus = (getAdvancedGpus() || []).length || 1;

  return `
    <div class="form-group">
      <label>Tensor Parallel (1..${totalGpus})</label>
      <input type="number" id="edw-adv-tp" min="1" max="${totalGpus}" step="1" value="${tp}"
        style="width: 80px;">
      <span class="form-hint inline">musi dzielic num_attention_heads modelu</span>
    </div>
    <div class="form-group">
      <label>Pipeline Parallel (1..${totalGpus})</label>
      <input type="number" id="edw-adv-pp" min="1" max="${totalGpus}" step="1" value="${pp}"
        style="width: 80px;">
      <span class="form-hint inline">TP × PP musi byc <= liczba GPU; PP dzieli num_hidden_layers</span>
    </div>
    <div class="form-group">
      <label>Max model length (kontekst): <span id="edw-adv-ctx-val">${ctx.toLocaleString()}</span></label>
      <input type="range" id="edw-adv-ctx" min="512" max="${maxCtx}" step="512" value="${ctx}" style="width: 100%;">
      <span class="form-hint">Max wspierane: ${maxCtx.toLocaleString()} (limit z VRAM dla aktualnej konfiguracji)</span>
    </div>
    <div class="form-group">
      <label>Max równoległych zapytań: <span id="edw-adv-seqs-val">${seqs}</span></label>
      <input type="range" id="edw-adv-seqs" min="1" max="${maxSeqs}" step="1" value="${seqs}" style="width: 100%;">
      <span class="form-hint">Max wspierane: ${maxSeqs}</span>
    </div>
    <div class="form-group">
      <label>KV Cache Dtype</label>
      <select id="edw-adv-kv" style="padding: 4px;">
        <option value="auto" ${kv === 'auto' ? 'selected' : ''}>auto (fp16, default)</option>
        <option value="fp16" ${kv === 'fp16' ? 'selected' : ''}>fp16 (2 bytes/elem)</option>
        <option value="bfloat16" ${kv === 'bfloat16' ? 'selected' : ''}>bfloat16 (2 bytes/elem)</option>
        <option value="fp8" ${kv === 'fp8' ? 'selected' : ''}>fp8 (1 byte/elem - 2× wiecej kontekstu)</option>
      </select>
    </div>
    <div class="form-group">
      <label>GPU memory utilization: <span id="edw-adv-mem-val">${(memUtil * 100).toFixed(0)}%</span></label>
      <input type="range" id="edw-adv-mem" min="0.5" max="0.95" step="0.05" value="${memUtil}" style="width: 100%;">
      <span class="form-hint">Ile VRAM mozna uzyc (0.5 = 50%, 0.95 = 95%; rezerwuje miejsce dla CUDA workspace)</span>
    </div>
    <div class="form-hint" style="margin-top: 8px;">
      Wartosci sa zapisywane jako VLLM_ARGS w deploy. Suwaki maja hard limits z aktualnej konfiguracji - jezeli ustawisz wieksze, deploy moze sie OOM-owac.
    </div>
  `;
}

function bindAdvancedHandlers() {
  document.querySelectorAll('[data-adv-mode]').forEach((btn) => {
    btn.addEventListener('click', () => {
      selection.advanced.mode = btn.getAttribute('data-adv-mode');
      rerenderStepBody();
    });
  });

  const debounceRecompute = (overrides) => {
    if (advancedRecommendDebounceTimer) clearTimeout(advancedRecommendDebounceTimer);
    advancedRecommendDebounceTimer = setTimeout(async () => {
      advancedRecommendation = await fetchVllmRecommendation(overrides);
      const summaryDiv = document.getElementById('edw-adv-summary');
      if (summaryDiv && advancedRecommendation) {
        summaryDiv.innerHTML = renderRecommendationSummary(advancedRecommendation);
      }
    }, 300);
  };

  const buildOverrides = () => {
    const a = selection.advanced;
    return {
      tensor_parallel: a.tensor_parallel || undefined,
      pipeline_parallel: a.pipeline_parallel || undefined,
      max_model_len: a.max_model_len || undefined,
      max_num_seqs: a.max_num_seqs || undefined,
      kv_cache_dtype: a.kv_cache_dtype !== 'auto' ? a.kv_cache_dtype : undefined,
      gpu_memory_utilization: a.gpu_memory_utilization || undefined,
    };
  };

  const bindRange = (id, valSpanId, key, transform = (v) => v, displayFn = null) => {
    const el = document.getElementById(id);
    const valSpan = document.getElementById(valSpanId);
    if (!el) return;
    el.addEventListener('input', () => {
      const v = transform(el.value);
      selection.advanced[key] = v;
      if (valSpan) valSpan.textContent = displayFn ? displayFn(v) : v.toLocaleString();
      debounceRecompute(buildOverrides());
    });
  };

  bindRange('edw-adv-ctx', 'edw-adv-ctx-val', 'max_model_len', (v) => parseInt(v));
  bindRange('edw-adv-seqs', 'edw-adv-seqs-val', 'max_num_seqs', (v) => parseInt(v));
  bindRange('edw-adv-mem', 'edw-adv-mem-val', 'gpu_memory_utilization',
    (v) => parseFloat(v),
    (v) => `${(v * 100).toFixed(0)}%`);

  ['edw-adv-tp', 'edw-adv-pp'].forEach((id) => {
    const el = document.getElementById(id);
    if (!el) return;
    el.addEventListener('change', () => {
      const key = id === 'edw-adv-tp' ? 'tensor_parallel' : 'pipeline_parallel';
      selection.advanced[key] = parseInt(el.value);
      debounceRecompute(buildOverrides());
    });
  });

  const kvSelect = document.getElementById('edw-adv-kv');
  if (kvSelect) {
    kvSelect.addEventListener('change', () => {
      selection.advanced.kv_cache_dtype = kvSelect.value;
      debounceRecompute(buildOverrides());
    });
  }

  // Wyzwalaj initial fetch jezeli jeszcze nie ma rekomendacji (np. user
  // wszedl w step pierwszy raz bez auto-trigger).
  if (!advancedRecommendation) {
    debounceRecompute({});
  }
}

// ---- Step 3: runtime ------------------------------------------------------

function renderStepRuntime() {
  const eng = engineEntry?.engine || {};
  const port = selection.port || eng.default_port || 8080;
  const cname = selection.containerName || '';
  const composeMode = selection.deployMethod === 'docker' && usesDockerCompose();

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
          label="${escapeAttr(I18n.t(composeMode ? 'wizard.stackName' : 'wizard.containerName'))}"
          value="${escapeAttr(cname)}"></tf-input>
      </div>
    `;
  }

  const portField = composeMode ? '' : `
    <div class="form-group">
      <tf-input type="number" id="edw-port"
        label="${escapeAttr(I18n.t('wizard.port'))}"
        value="${escapeAttr(String(port))}"></tf-input>
    </div>
  `;

  return `
    <h4 class="wizard-step-title">${escapeHtml(I18n.t('wizard.configureRuntime'))}</h4>
    ${summary}
    ${portField}
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
  if (usesDockerCompose()) return true;
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
  const nodeName = escapeHtml(nodeDisplayName(selection.nodeId));

  // Option cards — zastepuja natywne radio buttony. Aktywna karta ma gradient
  // accent jako checkmark + tint tla + inner box-shadow.
  const icoAll = `<svg viewBox="0 0 24 24"><rect x="3" y="8" width="8" height="8" rx="1"/><rect x="13" y="8" width="8" height="8" rx="1"/><line x1="3" y1="3" x2="3" y2="6"/><line x1="21" y1="3" x2="21" y2="6"/><line x1="7" y1="4" x2="7" y2="7"/><line x1="17" y1="4" x2="17" y2="7"/></svg>`;
  const icoSpec = `<svg viewBox="0 0 24 24"><path d="M12 2l3.09 6.26L22 9.27l-5 4.87 1.18 6.88L12 17.77l-6.18 3.25L7 14.14 2 9.27l6.91-1.01z"/></svg>`;
  const icoCpu = `<svg viewBox="0 0 24 24"><rect x="4" y="4" width="16" height="16" rx="2"/><rect x="9" y="9" width="6" height="6"/><line x1="9" y1="1" x2="9" y2="4"/><line x1="15" y1="1" x2="15" y2="4"/><line x1="9" y1="20" x2="9" y2="23"/><line x1="15" y1="20" x2="15" y2="23"/><line x1="20" y1="9" x2="23" y2="9"/><line x1="20" y1="14" x2="23" y2="14"/><line x1="1" y1="9" x2="4" y2="9"/><line x1="1" y1="14" x2="4" y2="14"/></svg>`;

  const rows = gpus.map((g, idx) => {
    const meta = [
      `${fmtMb(g.vram_total_mb)} VRAM`,
      g.usage_percent != null ? `util ${Math.round(g.usage_percent)}%` : '',
      g.temperature_c != null ? `${Math.round(g.temperature_c)}°C` : '',
      g.driver_version ? `driver ${escapeHtml(String(g.driver_version))}` : '',
    ].filter(Boolean);
    const metaHtml = meta.map((m, i) => i < meta.length - 1 ? `<span>${escapeHtml(m)}</span><span class="sep">·</span>` : `<span>${escapeHtml(m)}</span>`).join(' ');
    const selected = selectedSet.has(String(idx));
    const vendor = String(g.vendor || '').toLowerCase();
    let brandClass = 'other';
    if (vendor.includes('nvidia')) brandClass = 'nvidia';
    else if (vendor.includes('amd') || vendor.includes('radeon')) brandClass = 'amd';
    else if (vendor.includes('intel')) brandClass = 'intel';
    const brandLabel = g.vendor || '—';
    return `
      <div class="gpu-row${selected ? ' selected' : ''}" data-gpu-idx="${idx}" role="checkbox" aria-checked="${selected}" tabindex="0">
        <div class="gpu-check"></div>
        <div class="gpu-info">
          <div class="gpu-name"><span class="gpu-idx">GPU ${idx} ·</span> ${escapeHtml(String(g.name || ''))}</div>
          <div class="gpu-meta">${metaHtml}</div>
        </div>
        <span class="gpu-brand ${brandClass}">${escapeHtml(String(brandLabel))}</span>
      </div>
    `;
  }).join('');

  const listHidden = mode !== 'specific' ? 'hidden' : '';
  const iconSummary = `<svg viewBox="0 0 24 24"><polyline points="9 11 12 14 22 4"/><path d="M21 12v7a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11"/></svg>`;

  return `
    <h4 class="wizard-step-title">${escapeHtml(I18n.t('wizard.gpu_title', { node: nodeName }))}</h4>
    <p class="form-hint">${escapeHtml(I18n.t('wizard.gpu_subtitle'))}</p>

    <div class="gpu-mode-cards">
      <button type="button" class="gpu-mode-card all${mode === 'all' ? ' active' : ''}" data-gpu-mode="all" aria-pressed="${mode === 'all'}">
        <span class="gpu-mode-ico">${icoAll}</span>
        <span class="gpu-mode-title">${escapeHtml(I18n.t('wizard.gpu_mode_all_title'))}<span class="gpu-mode-tag">${gpus.length}</span></span>
        <span class="gpu-mode-desc">${escapeHtml(I18n.t('wizard.gpu_mode_all_desc'))}</span>
      </button>
      <button type="button" class="gpu-mode-card specific${mode === 'specific' ? ' active' : ''}" data-gpu-mode="specific" aria-pressed="${mode === 'specific'}">
        <span class="gpu-mode-ico">${icoSpec}</span>
        <span class="gpu-mode-title">${escapeHtml(I18n.t('wizard.gpu_mode_specific_title'))}</span>
        <span class="gpu-mode-desc">${escapeHtml(I18n.t('wizard.gpu_mode_specific_desc'))}</span>
      </button>
      <button type="button" class="gpu-mode-card none${mode === 'none' ? ' active' : ''}" data-gpu-mode="none" aria-pressed="${mode === 'none'}">
        <span class="gpu-mode-ico">${icoCpu}</span>
        <span class="gpu-mode-title">${escapeHtml(I18n.t('wizard.gpu_mode_none_title'))}</span>
        <span class="gpu-mode-desc">${escapeHtml(I18n.t('wizard.gpu_mode_none_desc'))}</span>
      </button>
    </div>

    <div class="gpu-list" ${listHidden}>
      <div class="gpu-list-hint">${escapeHtml(I18n.t('wizard.gpu_list_hint', { n: gpus.length }))}</div>
      ${rows}
    </div>

    <div class="gpu-summary">${iconSummary}<span>${escapeHtml(gpuSummaryText(gpus))}</span></div>
  `;
}

function bindStepGpuInputs() {
  // Option cards — klik wybiera tryb.
  document.querySelectorAll('.gpu-mode-card[data-gpu-mode]').forEach((card) => {
    card.addEventListener('click', () => {
      const mode = card.dataset.gpuMode;
      if (!mode) return;
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

  // GPU cards — klik toggle selected.
  const toggleGpu = (row) => {
    const idx = String(row.dataset.gpuIdx);
    const set = new Set(selection.gpuIds);
    if (set.has(idx)) set.delete(idx); else set.add(idx);
    selection.gpuIds = Array.from(set).sort((a, b) => Number(a) - Number(b));
    row.classList.toggle('selected', set.has(idx));
    row.setAttribute('aria-checked', set.has(idx) ? 'true' : 'false');
    const box = document.querySelector('.gpu-summary span:last-child');
    if (box) box.textContent = gpuSummaryText(nodeGpus(selection.nodeId));
  };
  document.querySelectorAll('.gpu-list .gpu-row[data-gpu-idx]').forEach((row) => {
    row.addEventListener('click', () => toggleGpu(row));
    row.addEventListener('keydown', (e) => {
      if (e.key === ' ' || e.key === 'Enter') {
        e.preventDefault();
        toggleGpu(row);
      }
    });
  });
}

// ---- Bindings -------------------------------------------------------------

function bindStepInputs() {
  switch (currentStepId()) {
    case 'method':   bindStepMethodInputs(); break;
    case 'model':    bindStepModelInputs(); break;
    case 'gpu':      bindStepGpuInputs(); break;
    case 'advanced': bindAdvancedHandlers(); break;
    case 'runtime':  bindStepRuntimeInputs(); break;
  }
}

// Re-render zachowuje pozycje step ale przerysowuje body (np. po zmianie
// trybu auto/manual w Advanced step). renderShell powtarza wszystkie nagłówki
// + footer + bindings - tanie operacje (no DOM diff).
function rerenderStepBody() {
  renderShell(renderStepBody());
  bindStepInputs();
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
    const resp = await fetch(url);
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
  // Build vllm_args z Advanced step (jezeli aktywny dla tego silnika).
  // Auto-tuned -> uzywa recommended_vllm_args z kalkulatora.
  // Manual -> sklada CLI string z user-set wartosci suwakow.
  let vllmArgs = null;
  if (!shouldSkipAdvancedStep() && advancedRecommendation && !advancedRecommendation.error) {
    if (selection.advanced.mode === 'auto') {
      vllmArgs = advancedRecommendation.recommended_vllm_args || null;
    } else {
      const a = selection.advanced;
      const r = advancedRecommendation.recommended || {};
      const parts = [
        '--dtype', 'auto',
        '--gpu-memory-utilization', String(a.gpu_memory_utilization ?? r.gpu_memory_utilization ?? 0.9),
        '--max-model-len', String(a.max_model_len ?? r.max_model_len ?? 8192),
        '--max-num-seqs', String(a.max_num_seqs ?? r.max_num_seqs ?? 16),
        '--max-num-batched-tokens', String(Math.max(a.max_model_len ?? 8192, 8192)),
        '--enable-chunked-prefill',
      ];
      const tp = a.tensor_parallel ?? r.tensor_parallel ?? 1;
      const pp = a.pipeline_parallel ?? r.pipeline_parallel ?? 1;
      if (tp > 1) parts.push('--tensor-parallel-size', String(tp));
      if (pp > 1) parts.push('--pipeline-parallel-size', String(pp));
      const kv = a.kv_cache_dtype || r.kv_cache_dtype || 'auto';
      if (kv !== 'auto') parts.push('--kv-cache-dtype', kv);
      vllmArgs = parts.join(' ');
    }
  }

  const configJson = JSON.stringify({
    model_preset_id: selection.modelPresetId || null,
    model_repo: selection.modelRepo || null,
    port: usesDockerCompose() ? null : (selection.port || eng.default_port),
    container_name: selection.containerName || null,
    gpu_select_mode: selection.gpuSelectMode,
    gpu_ids: selection.gpuSelectMode === 'specific' ? selection.gpuIds : null,
    vllm_args: vllmArgs,
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

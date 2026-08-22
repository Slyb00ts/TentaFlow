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

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import * as Manifest from '/js/modules/catalog/manifest-store.js';
import { deployIcon, render as renderIcon } from '/js/modules/catalog/catalog-icons.js';
import { isCameraCvEngineId } from '/js/modules/catalog/camera-cv-bundles.js';
import { computeGpuGroups, gpuPairChipHtml, gpuTopologyLegendHtml, pcieLinkHtml, selectionLinkHintHtml, shortPciBusId } from '/js/modules/gpu-topology-view.js';
import '/js/components/tf-progress-bar.js';

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
let hfGgufFiles = [];
let hfGgufFilesRepo = '';
let hfGgufFilesLoading = false;
let hfGgufFilesError = '';

// Custom-bundle (unpaired instance) manifest preview state.
let customBundlePreview = null;
let customBundlePreviewLoading = false;
let customBundlePreviewError = '';

let selection = {
  nodeId: null,
  deployMethod: null,
  modelPresetId: null,
  modelRepo: null,
  modelFile: null,
  port: null,
  containerName: null,
  gpuSelectMode: 'all',   // 'all' | 'specific' | 'none'
  gpuIds: [],             // e.g. ['0','2'] when gpuSelectMode === 'specific'
};

// Callback fired once when the wizard closes (used by the cluster page to
// refresh its detail view after a cluster deploy). Set from opts.onClose.
let onCloseCallback = null;

// Cache per-node GPU lists to avoid re-querying when switching back and forth.
const gpuListByNode = new Map();

// Ordered step ids with optional skip predicate. Runtime order derived at
// navigation time by filtering out steps whose skip() returns true.
const STEPS = [
  { id: 'method', skip: shouldSkipMethodStep },
  { id: 'model', skip: shouldSkipModelStep },
  { id: 'gpu', skip: shouldSkipGpuStep },
  { id: 'advanced', skip: shouldSkipAdvancedStep },
  { id: 'cluster-config', skip: shouldSkipClusterConfigStep },
  { id: 'runtime', skip: shouldSkipRuntimeStep },
];

// Cluster deploy runs across the whole cluster, so there is no single deploy
// method, GPU picker or per-container runtime — those steps are node-only. The
// cluster-config step is the mirror image (cluster-only).
function shouldSkipMethodStep() { return selection.isCluster; }
function shouldSkipRuntimeStep() { return selection.isCluster; }
function shouldSkipClusterConfigStep() { return !selection.isCluster; }

// Cache ostatniego wyniku /api/deploy/vllm/recommend (key: model+gpu_ids hash).
// Pozwala przeliczyc VRAM lokalnie przy zmianie suwaka bez ponownego HF fetch.
let advancedRecommendation = null;
let advancedRecommendDebounceTimer = null;

// Cache `model_spec` (num_layers, num_kv_heads, head_dim, dtype) z pierwszego
// fetchu — pomocniczy kontekst dla readoutow (np. dtype wag). Pamiec KV NIE
// jest juz liczona client-side: model puli vLLM (KRYT-1) i osobne K/V llama.cpp
// liczy backend, a wizard pokazuje `vram_estimate.kv_pool_gb`/`pool_tokens`.
let cachedModelSpec = null;

// Poprzedni stan "at_limit" — gdy zmienia sie z false->true, dodajemy
// klase pulse na adv-pill zeby user zauwazyl ze jest na granicy.
let prevAtLimit = false;
let deployInFlight = false;


/// Publiczne API: otwiera wizard dla `engineId`. `opts` opcjonalnie zawiera
/// `nodeId` (preselekcja z MeshDetail) i `hostOs` (z katalogu).
export async function openDeployWizard(engineId, opts = {}) {
  currentStep = 1;
  deployInFlight = false;
  onCloseCallback = null;
  modelSourceMode = 'preset';
  hfResults = [];
  hfSearchQuery = '';
  hfGgufFiles = [];
  hfGgufFilesRepo = '';
  hfGgufFilesLoading = false;
  hfGgufFilesError = '';
  customBundlePreview = null;
  customBundlePreviewLoading = false;
  customBundlePreviewError = '';
  selection = {
    nodeId: opts.nodeId || null,
    deployMethod: null,
    modelPresetId: null,
    modelRepo: null,
    modelFile: null,
    port: null,
    containerName: null,
    gpuSelectMode: 'all',
    gpuIds: [],
    // Cluster (multi-node tensor-parallel) deploy. When `isCluster`, the wizard
    // skips method/gpu/runtime, keeps model+advanced and adds a cluster-config
    // step; startDeploy sends `clusterDeployRequest` instead of the node path.
    isCluster: opts.isCluster === true,
    clusterId: opts.clusterId || null,
    clusterMembers: [],
    gpusPerNode: 1,
    // null = wylicz z rozmiaru wag (`defaultReadyTimeoutSecs`).
    readyTimeoutSecs: null,
    servedModelName: '',
    pricing: { promptPer1k: null, completionPer1k: null, audioPerMin: null, imageEach: null },
    // External cloud provider credentials (deploy.external.requires_api_key).
    // Stored encrypted server-side; base_url/api_version only used by
    // openai-compatible/azure-openai engines that need an endpoint override.
    apiKey: '',
    baseUrl: '',
    apiVersion: '',
    // Custom bundle source (camera-CV engines): manifest URL of another
    // TentaFlow instance's /models/manifest/<bundle> endpoint + the API key
    // (model_bundle scope) authenticating the pull between UNPAIRED instances.
    visionBundleUrl: '',
    visionBundleApiKey: '',
    // 'api' = pay-per-token API key; 'subscription' = OAuth/ChatGPT-or-Google
    // subscription token (OpenAI Codex / Gemini Code Assist). Only OpenAI+Gemini.
    externalAuthMode: 'api',
    // Finalna komenda startowa silnika (krok runtime). 'auto' = backend buduje
    // ja per-dialekt z ustawien Advanced (readonly podglad); 'custom' = user
    // edytuje caly tekst, ktory leci jako `launch_command_override` w config_json.
    launchCommandMode: 'auto',
    launchCommandText: '',
    // Subscription OAuth: set once the browser login completes on the node.
    oauthFlowId: null,
    oauthAccount: null,
    // Generic engine parameters: wartosci dla silnikow ktore deklaruja
    // manifestowe [[parameter]] i NIE naleza do rodziny vLLM/MLX (np. ds4).
    // Klucz = `parameter.key`, trafiaja 1:1 do config_json.parameters{} i tam
    // `apply_parameters_deploy` mapuje je wg bindingow na env/flagi silnika.
    // Pusty na start; render czyta default z manifestu gdy brak wpisu.
    genericParams: {},
    // Advanced (vLLM Auto-tuned) - wartosci uzywane do build vllm_args.
    // `lockedParam` = ostatnio dotkniety przez usera slider/chip — backend
    // dostaje `lock_<param>: true` tylko dla niego, reszte parametrow
    // dopasowuje sam (auto-fit).
    advanced: {
      mode: 'auto',  // 'auto' = use recommended, 'manual' = override
      tensor_parallel: null,       // null = auto-pick
      pipeline_parallel: null,
      max_model_len: null,
      max_num_seqs: null,
      kv_cache_dtype: 'auto',
      // llama.cpp: osobny typ V cache (None = rowny K). vLLM/MLX nie maja
      // osobnego V — uzywaja samego `kv_cache_dtype`.
      kv_cache_dtype_v: null,
      // vLLM: opcjonalny `--max-num-batched-tokens` (driver szczytu aktywacji).
      max_num_batched_tokens: 8192,
      gpu_memory_utilization: 0.9,
      gpu_memory_touched: false,
      // Quantization override do kalkulatora VRAM (`quantization_override`).
      // Pre-fillsuje sie z presetu: `model_preset.vllm.quantize` (self-quant,
      // np. NVFP4) albo `model_preset.quantization`. Pusty = dtype ze zrodla
      // (config.json). User moze zmienic w trybie manual i przeliczyc.
      quantization: null,
      // vLLM `--trust-remote-code`: pozwala repo modelu wykonac wlasny kod przy
      // ladowaniu (wymagane przez Gemma 4, DeepSeek V4 i inne z custom kodem).
      // Default ON dla wygody; user moze zdjac dla nieufnego repo.
      trust_remote_code: true,
      // vLLM-family free-text flags appended LAST to vllm_args, so they win
      // over recipe/slider values in the backend's last-wins dedup.
      extra_args: '',
      // MLX-only: budzet pamieci (MB) dla Apple unified memory. Backend liczy
      // realny `pool_tokens` (engine='mlx') z tego budzetu + wybranych kv-bits.
      // Default 16 GB — 8 GB jest mniejsze niz wagi modelu 7B+, wiec wizard
      // otwieralby sie w permanentnym overflow. User moze zwiekszyc.
      mlx_max_memory_mb: 16384,
      // MLX kv-bits: 'none' | '8' | '4'. Mapowane na request `kv_cache_dtype`
      // = none|kv8|kv4 (osobne od vLLM/llama, ale to samo pole wire).
      mlx_kv_bits: 'none',
      // MLX max rownoleglych sekwencji (mlx-lm batched generation). Cap puli,
      // nie mnoznik pamieci pojedynczej sekwencji.
      mlx_max_num_seqs: 1,
      lockedParam: null,           // 'max_model_len' | 'max_num_seqs' | 'tensor_parallel' | 'gpu_memory_utilization' | null
      // Speculative decoding via vLLM `--speculative-config`. Pre-fillsuje
      // sie z presetu (model_preset.speculator_*) jezeli wybrany preset go
      // ma — patrz `applySpeculatorPreset()`.
      speculative: {
        enabled: false,
        model: '',         // HF repo, np. 'RedHatAI/gemma-4-31B-it-speculator.dflash'
        method: 'dflash',  // przekazywane do vllm 1:1
        num_tokens: 8,
      },
    },
  };
  advancedRecommendation = null;
  cachedModelSpec = null;
  prevAtLimit = false;
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

  if (selection.isCluster) {
    // Cluster deploy is tensor-parallel: the model is sharded across EVERY GPU
    // of EVERY member (TP = members × gpusPerNode). Pick a representative member
    // that the mesh reports with GPUs so the calculator can read a per-GPU VRAM
    // and derive gpusPerNode from real hardware — TP must be known already in the
    // Advanced step, which runs before cluster-config.
    selection.clusterMembers = await fetchClusterMembers(selection.clusterId);
    const memberIds = selection.clusterMembers.map((m) => m.node_id).filter(Boolean);
    const withGpu = memberIds.find((id) => nodeGpus(id).length > 0);
    selection.nodeId = withGpu || memberIds[0] || selection.nodeId;
    // gpusPerNode can never exceed a node's physical GPU count. DGX Spark exposes
    // 1 GPU/node → gpusPerNode=1, TP=members. This makes TP deterministic from
    // cluster hardware and available to the Advanced VRAM calculator.
    selection.gpusPerNode = Math.max(1, nodeGpus(selection.nodeId).length);
    // Unified GB10 memory OOM-kills at 0.9; 0.5 is the safe cluster default.
    selection.advanced.gpu_memory_utilization = 0.5;
  }

  if (!selection.nodeId) {
    const local = nodes.find((n) => n?.is_local === true) || nodes[0];
    selection.nodeId = local ? (local.node_id || local.id) : null;
  }

  hostOs = opts.hostOs || pickHostOs(selection.nodeId);
  availableMethods = Manifest.availableDeployMethods(engineEntry, hostOs, pickHostCaps(selection.nodeId));

  if (availableMethods.length > 0) {
    selection.deployMethod = availableMethods[0];
  }

  const eng = engineEntry.engine || {};
  selection.port = selection.isCluster ? 8100 : (eng.default_port || 8080);
  selection.containerName = `tentaflow-${(eng.id || 'svc').toLowerCase()}-${randomSuffix()}`;

  const presets = Manifest.modelPresets(engineEntry);
  if (presets.length > 0) {
    // A featured-tile click passes opts.presetId to open straight on that model.
    const rec = (opts.presetId && presets.find((p) => p && p.id === opts.presetId))
      || presets.find((p) => p && p.recommended)
      || presets[0];
    if (rec) {
      selection.modelPresetId = rec.id;
      applySpeculatorPreset(rec);
      // Domyslny wariant kwantyzacji (gdy preset go ma) — repo wariantu jako
      // model_repo (wygrywa nad preset.repo w backendzie).
      const dv = defaultQuantVariant(rec);
      selection.quantVariant = dv ? dv.quantization : null;
      selection.modelRepo = dv ? dv.repo : null;
      if (dv) selection.advanced.quantization = dv.quantization;
    }
  } else {
    modelSourceMode = isCameraCvEngine() ? 'custom' : 'hf';
  }

  // Armed only after the (loading/error) renderShell cycles are done, so the
  // internal close() they perform never fires this user-close refresh.
  onCloseCallback = typeof opts.onClose === 'function' ? opts.onClose : null;

  refreshModal();
}

/// Pre-fillsuje pola Speculative Decoding z `model_preset.speculator_*`.
/// Wywolywane przy starcie wizardu i przy zmianie presetu — gdy preset NIE
/// ma sparowanego speculatora, panel zostaje resetowany do disabled (zeby
/// stary speculator z poprzedniego presetu nie wyciekal).
function applySpeculatorPreset(preset) {
  const sp = selection.advanced.speculative;
  if (preset && preset.speculator_repo) {
    sp.model = preset.speculator_repo;
    sp.method = preset.speculator_method || 'dflash';
    sp.num_tokens = preset.speculator_num_tokens || 8;
    // Featured presety to gotowe, turnkey bloczki (np. Bielik NVFP4 + draft) —
    // speculative wlacza sie automatycznie, zeby deploy nie wymagal recznego
    // toggle. Zwykle (nie-featured) presety ze sparowanym draftem zostaja
    // user-opt-in jak dotad.
    sp.enabled = !!preset.featured;
  } else {
    sp.enabled = false;
    sp.model = '';
    sp.method = 'dflash';
    sp.num_tokens = 8;
  }
  // Quantization presetu — self-quant (vllm.quantize, np. NVFP4) ma priorytet
  // nad statyczna etykieta `quantization`. Bez tego kalkulator liczyl wagi w
  // dtype zrodla (BF16) i odrzucal NVFP4-owy model na GPU ktory by go zmiescil.
  const presetQuant = (preset && preset.vllm && preset.vllm.quantize)
    || (preset && preset.quantization)
    || null;
  selection.advanced.quantization = presetQuant;
}

export function close() {
  const el = document.getElementById('engine-deploy-wizard');
  if (el) {
    if (typeof el.close === 'function') el.close(true);
    else el.remove();
  }
  const backdrop = document.getElementById('engine-deploy-wizard-backdrop');
  if (backdrop) backdrop.remove();
  const cb = onCloseCallback;
  onCloseCallback = null;
  if (cb) { try { cb(); } catch (_) { /* refresh best-effort */ } }
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

/// Cluster members for a cluster deploy — normalized to `{ node_id, hostname }`.
/// Mirrors cluster-detail's resolveMembers (camelCase binary + snake_case legacy).
async function fetchClusterMembers(clusterId) {
  if (!clusterId) return [];
  try {
    const detail = await ApiBinary.one('clusterDetailRequest', { clusterId });
    const raw = (detail && (detail.members || detail.cluster?.members)) || [];
    return raw.map((m) => ({
      node_id: m.nodeId || m.node_id || m.id,
      hostname: m.hostname || m.node_name || m.nodeId || m.node_id || '',
    })).filter((m) => m.node_id);
  } catch (err) {
    console.warn('[wizard] fetchClusterMembers:', err);
    return [];
  }
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

/// Build the host caps payload that manifest-store uses to gate engines.
/// Mirrors `target` in `catalog.js`: same DGX Spark detection so wizard
/// agrees with the catalog tile that opened it.
function pickHostCaps(nodeId) {
  const node = nodes.find((n) => n && (n.node_id || n.id) === nodeId);
  if (!node) return { isDgxSpark: false };
  const os = String(node.platform || node.os || '').toLowerCase();
  const gpuNames = (Array.isArray(node.gpus) ? node.gpus : [])
    .map((g) => g?.name || '')
    .filter(Boolean);
  return {
    isDgxSpark: os === 'linux' && gpuNames.some((name) => /GB10/i.test(name)),
  };
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
    case 'cluster-config': return renderStepClusterConfig();
    case 'runtime':  return renderStepRuntime();
    default: return '';
  }
}

// Step Advanced wyswietlamy TYLKO dla LLM silnikow ktore akceptuja
// VLLM_ARGS-style override (vllm/sglang/llama-cpp). Inne silniki (TTS/STT/
// vision/image-gen) maja stalsze konfiguracje i nie maja kalkulatora VRAM.
/// Manifest [[parameter]] list for engines outside the vLLM/MLX families
/// (generated manifest serializes them under the singular key `parameter`).
/// Drives a generic Advanced step (enum/int/float/bool/string) — ds4 uses this
/// for backend / ctx / SSD streaming / MTP knobs.
function manifestParams() {
  const p = engineEntry?.parameter;
  return Array.isArray(p) ? p : [];
}

// Engines whose Advanced step is the bespoke VRAM/KV calculator (vLLM family +
// llama.cpp + MLX). They must NOT fall into the generic manifest-param renderer.
const ADV_CALC_ENGINES = ['vllm', 'vllm-spark', 'sglang', 'llama-cpp', 'tensorrt-llm', 'mlx'];

// vLLM-backed engines (embed / rerank / VL pooling models) carry backend="vllm"
// in the manifest — they get the SAME VRAM calculator + gpu_memory_utilization
// slider as the named vLLM engines, without a second hand-maintained id list.
function isAdvCalcEngine(id) {
  return ADV_CALC_ENGINES.includes(id) || engineEntry?.engine?.backend === 'vllm';
}

function hasGenericParams() {
  if (isAdvCalcEngine(engineId())) return false;
  return manifestParams().length > 0;
}

/// Current value for a generic param, falling back to its manifest default.
function genericParamValue(p) {
  const cur = selection.genericParams[p.key];
  return cur === undefined || cur === null ? p.default : cur;
}

function shouldSkipAdvancedStep() {
  const eng = engineEntry?.engine || {};
  const id = String(eng.id || '').toLowerCase();
  // Engines with manifest [[parameter]] (ds4 etc.) get a generic Advanced step
  // that needs neither a VRAM calc nor a GPU selection.
  if (hasGenericParams()) return false;
  // MLX (embedded, Apple unified memory) reuzywa ten sam krok, ale liczy
  // "ile tokenow kontekstu zmiesci sie w budzecie pamieci" zamiast vLLM args.
  if (!isAdvCalcEngine(id)) return true;
  // Bez wybranego modelu nie ma jak liczyc VRAM/KV
  if (!selection.modelRepo && !selection.modelPresetId) return true;
  // Bez wybranych GPU tez nie — ale MLX nie ma dyskretnego GPU (unified memory),
  // tam krok dziala z samego budzetu pamieci.
  if (id !== 'mlx' && selection.gpuSelectMode === 'none') return true;
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

// OpenAI (ChatGPT plan via Codex OAuth) and Google Gemini (Google AI via the
// Gemini CLI / Code Assist OAuth) can be driven by a personal subscription, not
// only a pay-per-token API key. Anthropic deliberately blocks this for third
// parties, so Claude is API-key only.
function subscriptionSupportedEngine() {
  // OpenAI subscription (ChatGPT plan via the Codex Responses backend) is wired
  // end-to-end. Gemini's Code Assist subscription API needs its own adapter and
  // is API-key only until that lands.
  return ['openai'].includes(String(engineEntry?.engine?.id || '').toLowerCase());
}

function renderMethodCard(m, auth) {
  const splitExternal = m === 'external' && auth;
  const active = splitExternal
    ? (selection.deployMethod === 'external' && selection.externalAuthMode === auth)
    : (selection.deployMethod === m);
  const sel = active ? ' selected' : '';
  const key = splitExternal ? `external_${auth}` : m;
  const authAttr = auth ? ` data-auth="${escapeAttr(auth)}"` : '';
  return `
    <button type="button" class="deploy-method-card${sel}" data-method="${escapeAttr(m)}"${authAttr}>
      <div class="dm-ico">${deployIcon(m, 32)}</div>
      <div class="dm-name">${escapeHtml(I18n.t(`wizard.method.${key}`))}</div>
      <div class="dm-desc">${escapeHtml(I18n.t(`wizard.method.${key}Desc`))}</div>
    </button>
  `;
}

function renderStepMethod() {
  if (availableMethods.length === 0) {
    const msg = I18n.t('wizard.noMethodsAvailable').replace('{os}', escapeHtml(hostOs));
    return `
      <h4 class="wizard-step-title">${escapeHtml(I18n.t('wizard.selectMethod'))}</h4>
      <p class="form-hint">${msg}</p>
    `;
  }

  // OpenAI / Gemini support BOTH a pay-per-token API key and an OAuth
  // subscription (ChatGPT plan / Google AI), so their single "external" method
  // splits into two tiles — like Docker/Native are separate tiles.
  const cards = availableMethods.flatMap((m) => {
    if (m === 'external' && subscriptionSupportedEngine()) {
      return [renderMethodCard('external', 'api'), renderMethodCard('external', 'subscription')];
    }
    return [renderMethodCard(m, null)];
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
  // Camera-CV bundle engines pull fixed weights, not HF repos — their second
  // source is "Custom": another TentaFlow instance's /models manifest + API key.
  const cameraCv = isCameraCvEngine();

  let tabs = `<tf-tabs variant="underline" id="edw-model-tabs" value="${escapeAttr(modelSourceMode)}">`;
  if (hasPresets) {
    tabs += `<tf-tab id="preset">${escapeHtml(I18n.t('wizard.fromPreset'))}</tf-tab>`;
  }
  if (cameraCv) {
    tabs += `<tf-tab id="custom">${escapeHtml(I18n.t('wizard.customBundle'))}</tf-tab>`;
  } else {
    tabs += `<tf-tab id="hf">${escapeHtml(I18n.t('wizard.searchHuggingface'))}</tf-tab>`;
  }
  tabs += '</tf-tabs>';

  let content;
  if (modelSourceMode === 'preset' && hasPresets) {
    content = renderPresetSelector(presets);
  } else if (cameraCv) {
    content = renderCustomBundleSource();
  } else {
    content = renderHfSearch();
  }

  return `
    <h4 class="wizard-step-title">${escapeHtml(I18n.t('wizard.selectModel'))}</h4>
    ${tabs}
    <div class="wizard-tab-content">${content}</div>
  `;
}

/// "Custom" bundle source for camera-CV engines: manifest URL of the serving
/// TentaFlow instance + API key. A signed manifest URL (with ?token=) needs no
/// key; a plain manifest URL is authenticated with the Bearer key created on
/// the serving instance ("Dostęp i klucze API" → model_bundle scope).
function renderCustomBundleSource() {
  return `
    <div class="form-group">
      <tf-input type="text" id="edw-bundle-url"
        label="${escapeAttr(I18n.t('wizard.customBundleUrl'))}"
        placeholder="https://other-instance:8090/models/manifest/vision-all"
        value="${escapeAttr(selection.visionBundleUrl)}" autocomplete="off"
        hint="${escapeAttr(I18n.t('wizard.customBundleUrlHint'))}"></tf-input>
    </div>
    <div class="form-group">
      <tf-input type="password" id="edw-bundle-api-key"
        label="${escapeAttr(I18n.t('wizard.customBundleApiKey'))}"
        placeholder="sk-..."
        value="${escapeAttr(selection.visionBundleApiKey)}" autocomplete="off"
        hint="${escapeAttr(I18n.t('wizard.customBundleApiKeyHint'))}"></tf-input>
    </div>
    <div class="form-group">
      <tf-button id="edw-bundle-preview" variant="ghost" icon="search">${escapeHtml(I18n.t('wizard.customBundlePreview') || 'Sprawdź manifest')}</tf-button>
    </div>
    <div id="edw-bundle-preview-result"></div>
  `;
}

/// Render the fetched-manifest preview: file list + (for a single-model
/// registry bundle) the importable model and an "Importuj do rejestru" action.
function renderBundlePreview(container) {
  if (!customBundlePreview) { container.innerHTML = ''; return; }
  if (customBundlePreviewLoading) {
    container.innerHTML = `<p class="form-hint">${escapeHtml(I18n.t('common.loading'))}</p>`;
    return;
  }
  if (customBundlePreviewError) {
    container.innerHTML = `<p class="form-hint error">${escapeHtml(customBundlePreviewError)}</p>`;
    return;
  }
  const m = customBundlePreview.model;
  const files = Array.isArray(customBundlePreview.files) ? customBundlePreview.files : [];
  const filesHtml = files.map((f) => `
    <div class="model-item">
      <div class="model-item-main">
        <div class="model-item-name mono">${escapeHtml(f.name)}</div>
        <div class="model-item-info">${escapeHtml(String(f.sha256 || '').slice(0, 12))} · ${escapeHtml(formatBytes(Number(f.size) || 0))}</div>
      </div>
    </div>`).join('');
  let importHtml = '';
  if (m && m.modelName) {
    const classes = Array.isArray(m.classes) ? m.classes.length : 0;
    importHtml = `
      <div class="model-item selected">
        <div class="model-item-main">
          <div class="model-item-name">${escapeHtml(m.modelName)}</div>
          <div class="model-item-info">${escapeHtml(m.op || '')} · ${classes} ${escapeHtml(I18n.t('wizard.customBundleClasses') || 'klas')}</div>
        </div>
      </div>
      <div class="form-group" style="margin-top:12px;">
        <tf-input type="text" id="edw-bundle-alias"
          label="${escapeAttr(I18n.t('wizard.customBundleAlias') || 'Alias (opcjonalnie)')}"
          value="${escapeAttr(selection.visionImportAlias || '')}" autocomplete="off"></tf-input>
      </div>
      <div class="form-group">
        <tf-button id="edw-bundle-import" variant="primary" icon="download">${escapeHtml(I18n.t('wizard.customBundleImport') || 'Importuj do rejestru')}</tf-button>
      </div>`;
  } else {
    importHtml = `<p class="form-hint">${escapeHtml(I18n.t('wizard.customBundleFixedHint') || 'To bundle silnika (nie pojedynczy model rejestru) — zostanie pobrany przy wdrożeniu tego silnika.')}</p>`;
  }
  container.innerHTML = `
    <div class="wizard-tab-content">
      <div class="model-list">${filesHtml}</div>
      ${importHtml}
    </div>`;
  const importBtn = container.querySelector('#edw-bundle-import');
  if (importBtn) importBtn.addEventListener('click', importCustomModel);
  const aliasInput = container.querySelector('#edw-bundle-alias');
  if (aliasInput) {
    aliasInput.addEventListener('input', (e) => {
      selection.visionImportAlias = String(e.detail?.value ?? aliasInput.value).trim();
    });
  }
}

/// Fetch the remote manifest through Core (server-side, Bearer key). Populates
/// the preview state and re-renders only the result container.
async function previewCustomManifest() {
  const url = String(selection.visionBundleUrl || '').trim();
  const key = String(selection.visionBundleApiKey || '').trim();
  if (!url) { toast(I18n.t('wizard.customBundleUrlInvalid') || 'Podaj URL manifestu', 'error'); return; }
  customBundlePreview = null;
  customBundlePreviewError = '';
  customBundlePreviewLoading = true;
  const box = document.getElementById('edw-bundle-preview-result');
  if (box) renderBundlePreview(box);
  try {
    const resp = await ApiBinary.action('visionImportFetchManifestRequest', {
      manifestUrl: url,
      apiKey: key,
    });
    if (resp && resp.error) throw new Error(resp.error);
    customBundlePreview = resp || { files: [], model: null };
  } catch (e) {
    customBundlePreviewError = e.message || String(e);
  } finally {
    customBundlePreviewLoading = false;
    const b = document.getElementById('edw-bundle-preview-result');
    if (b) renderBundlePreview(b);
  }
}

/// Import the previewed single-model registry bundle into the local registry.
async function importCustomModel() {
  const m = customBundlePreview && customBundlePreview.model;
  if (!m || !m.modelName) return;
  const btn = document.getElementById('edw-bundle-import');
  if (btn) btn.setAttribute('disabled', '');
  try {
    const resp = await ApiBinary.action('visionImportModelRequest', {
      manifestUrl: String(selection.visionBundleUrl || '').trim(),
      apiKey: String(selection.visionBundleApiKey || '').trim(),
      modelName: m.modelName,
      alias: selection.visionImportAlias || null,
    }, { timeoutMs: 10 * 60 * 1000 });
    if (!resp || !resp.ok) throw new Error((resp && resp.error) || 'import odrzucony');
    toast(`${I18n.t('wizard.customBundleImported') || 'Model zaimportowany'}: ${resp.importedModelName || m.modelName}`, 'success');
  } catch (e) {
    if (btn) btn.removeAttribute('disabled');
    toast(`${I18n.t('wizard.customBundleImportFailed') || 'Import nieudany'}: ${e.message || e}`, 'error');
  }
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
    ${renderQuantVariantSelector(presets)}
    <p class="form-hint">${escapeHtml(I18n.t('wizard.presetHint'))}</p>
  `;
}

/// Warianty kwantyzacji wybranego presetu (`[[model_preset.quant_variant]]`).
function presetQuantVariants(preset) {
  return (preset && Array.isArray(preset.quant_variant)) ? preset.quant_variant.filter(Boolean) : [];
}

/// Domyslny wariant: dopasowany do `quantization` presetu, inaczej pierwszy.
function defaultQuantVariant(preset) {
  const vs = presetQuantVariants(preset);
  if (!vs.length) return null;
  const q = (preset.quantization || '').toLowerCase();
  return vs.find((v) => (v.quantization || '').toLowerCase() === q) || vs[0];
}

/// Dropdown wyboru kwantyzacji (= podmiana repo HF) pod lista presetow. Renderuje
/// sie tylko gdy wybrany preset ma zdefiniowane `quant_variant`. Pod spodem widac
/// docelowe repo, zgodnie z pomyslem: wybor kwantyzacji + od razu wiadomo model.
function renderQuantVariantSelector(presets) {
  const preset = presets.find((p) => p?.id === selection.modelPresetId);
  const variants = presetQuantVariants(preset);
  if (variants.length < 2) return '';
  const current = (selection.quantVariant || (defaultQuantVariant(preset) || {}).quantization || '').toLowerCase();
  const opts = variants.map((v) => {
    const q = v.quantization || '';
    const label = v.display_name || q;
    const sel = q.toLowerCase() === current ? ' selected' : '';
    return `<option value="${escapeAttr(q)}"${sel}>${escapeHtml(label)}</option>`;
  }).join('');
  const active = variants.find((v) => (v.quantization || '').toLowerCase() === current) || variants[0];
  return `
    <div class="quant-variant-row">
      <label class="form-label" for="edw-quant-variant">${escapeHtml(I18n.t('wizard.quantVariant'))}</label>
      <tf-select id="edw-quant-variant" value="${escapeAttr(active.quantization || '')}">${opts}</tf-select>
      <div class="model-item-info mono">${escapeHtml(active.repo || '')}</div>
    </div>
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
    <div id="edw-hf-gguf-files">${renderHfGgufFilesHtml()}</div>
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

function renderHfGgufFilesHtml() {
  if (!isLlamaCppEngine() || !selection.modelRepo) return '';
  if (hfGgufFilesLoading) {
    return `<p class="form-hint">${escapeHtml(I18n.t('common.loading'))}</p>`;
  }
  if (hfGgufFilesError) {
    return `<p class="form-hint">${escapeHtml(hfGgufFilesError)}</p>`;
  }
  if (hfGgufFilesRepo !== selection.modelRepo) return '';
  if (hfGgufFiles.length === 0) {
    return `<p class="form-hint">${escapeHtml(I18n.t('wizard.ggufNoFiles') || 'No GGUF files found in this repository.')}</p>`;
  }
  const rows = hfGgufFiles.map((file) => {
    const sel = selection.modelFile === file.path ? ' selected' : '';
    const size = file.size ? formatBytes(file.size) : '';
    const quant = detectGgufQuantization(file.path);
    const info = [quant, size].filter(Boolean).join(' · ');
    return `
      <div class="model-item${sel}" data-gguf-file="${escapeAttr(file.path)}">
        <div class="model-item-main">
          <div class="model-item-name mono">${escapeHtml(file.path)}</div>
          ${info ? `<div class="model-item-info">${escapeHtml(info)}</div>` : ''}
        </div>
      </div>
    `;
  }).join('');
  return `
    <div class="form-group" style="margin-top:12px;">
      <label>${escapeHtml(I18n.t('wizard.ggufFileLabel') || 'GGUF file')}</label>
      <div class="model-list">${rows}</div>
      <p class="form-hint">${escapeHtml(I18n.t('wizard.ggufFileHint') || 'Choose one GGUF quantization file. TentaFlow will download only this file, not the whole repository.')}</p>
    </div>
  `;
}

function hfSearchFilterHint() {
  if (isLlamaCppEngine()) return 'GGUF';
  const id = String(engineEntry?.engine?.id || '').toLowerCase();
  if (id === 'mlx') return 'mlx-community/*';
  return '';
}

function engineId() {
  return String(engineEntry?.engine?.id || '').toLowerCase();
}

function isLlamaCppEngine() {
  const id = engineId();
  return id.includes('llama') || id.includes('llamacpp');
}

function isMlxEngine() {
  return engineId() === 'mlx';
}

function isCameraCvEngine() {
  return isCameraCvEngineId(engineId());
}

// vLLM-rodzina = silniki ktore akceptuja safetensors override kwantyzacji wag
// i jeden select KV (auto/fp8). Wagi llama.cpp/MLX wynikaja z pobranego pliku.
function isVllmFamilyEngine() {
  return ['vllm', 'vllm-spark', 'sglang', 'tensorrt-llm'].includes(engineId())
    || engineEntry?.engine?.backend === 'vllm';
}

// Pooling engines (embeddings / reranker) have no KV-cache pool, so their
// resting gpu_memory_utilization default is a tight budget, not the generative
// 0.9 — mirrors Core's `auto_gpu_memory_utilization(is_pooling)` cap.
function isPoolingEngine() {
  const cat = String(engineEntry?.engine?.category || '').toLowerCase();
  return cat === 'embeddings' || cat === 'reranker';
}

function formatCount(n) {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

function formatBytes(n) {
  if (n >= 1024 ** 3) return `${(n / (1024 ** 3)).toFixed(1)} GB`;
  if (n >= 1024 ** 2) return `${(n / (1024 ** 2)).toFixed(0)} MB`;
  if (n >= 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${n} B`;
}

function detectGgufQuantization(path) {
  const name = String(path || '').replace(/\.gguf$/i, '');
  const match = name.match(/(?:^|[-_])((?:IQ|Q)\d(?:_[A-Z0-9]+)+|BF16|F16|F32)(?:$|[-_])/i);
  return match ? match[1].toUpperCase() : '';
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
  // MLX nie ma dyskretnego GPU — wysylamy REALNY budzet Apple unified memory
  // jako pojedyncze "urzadzenie". Backend (engine='mlx', estimate_mlx_vram)
  // liczy z niego pule KV bez workspace'u vLLM, wiec maly budzet nie wywala
  // juz handlera twardym BadRequest. `gpu_memory_gb_each` = budzet / 1024.
  if (isMlxEngine()) {
    const mb = Number(selection.advanced?.mlx_max_memory_mb) || 0;
    return mb > 0
      ? [{ index: 0, name: 'Apple unified memory', memory_gb: mb / 1024 }]
      : [];
  }
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

// Full tensor-parallel device list for a cluster deploy. TP shards the model
// across every GPU of every member (members × gpusPerNode), so the VRAM
// calculator must see ALL devices — not one node's GPUs. Each member contributes
// `gpusPerNode` devices carrying that node's per-GPU VRAM; members whose GPU
// inventory has not yet propagated over the mesh reuse the representative node's
// per-GPU VRAM (identical Spark hardware). Single-node deploy falls back to the
// unchanged single-node path.
function getClusterAdvancedGpus() {
  if (!selection.isCluster) return getAdvancedGpus();
  const perNode = Math.max(1, Number(selection.gpusPerNode) || 1);
  const members = selection.clusterMembers || [];
  const gpuMemGb = (g) =>
    g ? Math.round(((g.vram_total_mb || g.memory_mb || 0) / 1024) * 10) / 10 : 0;
  const repGpus = nodeGpus(selection.nodeId);
  const fallbackMem = gpuMemGb(repGpus[0]);
  const fallbackName = (repGpus[0] && repGpus[0].name) || 'GPU';
  const out = [];
  let idx = 0;
  for (const m of members) {
    if (!m || !m.node_id) continue;
    const gpus = nodeGpus(m.node_id);
    for (let i = 0; i < perNode; i += 1) {
      const g = gpus[i] || gpus[0] || null;
      out.push({
        index: idx,
        name: (g && g.name) || fallbackName,
        memory_gb: gpuMemGb(g) || fallbackMem,
      });
      idx += 1;
    }
  }
  return out.filter((g) => g.memory_gb > 0);
}

async function fetchVllmRecommendation(overrides = {}) {
  const model = getAdvancedModelName();
  const gpus = selection.isCluster ? getClusterAdvancedGpus() : getAdvancedGpus();
  if (!model || gpus.length === 0) return null;
  const body = {
    model,
    gpus,
    ...overrides,
  };
  // Cluster deploy is tensor-parallel across the whole cluster. The backend
  // computes weights-per-GPU as model/TP, so lock TP = members × gpusPerNode
  // (the full device count sent above). This mirrors the deploy-time TP exactly,
  // so the VRAM budget the user sees is the real distributed budget.
  if (selection.isCluster) {
    body.tensor_parallel = clusterTpSize();
    body.lock_tensor_parallel = true;
  }
  // Jawne pole `engine` mowi backendowi ktorym modelem fizycznym liczyc VRAM.
  // Bez niego GGUF wykrywa sie po nazwie pliku, ale jawne pole jest pewne i
  // dziala tez dla nie-GGUF modeli llama.cpp. MLX czyta config.json (NIE GGUF),
  // ale ma wlasna fizyke unified-memory (estimate_mlx_vram) — bez engine='mlx'
  // backend cicho spadalby na vLLM math i raportowal fałszywy budzet.
  if (isLlamaCppEngine()) body.engine = 'llama-cpp';
  else if (isMlxEngine()) body.engine = 'mlx';
  else body.engine = engineId();
  // Metoda deployu rozstrzyga baze komendy w podgladzie (`launch_command`):
  // docker = `vllm serve`, native = `python -m vllm.entrypoints...`.
  if (selection.deployMethod) body.deploy_method = selection.deployMethod;
  // llama.cpp/GGUF: repo nie ma config.json, wiec backend liczy VRAM z metadanych
  // wybranego pliku .gguf. Bez sciezki pliku backend nie ma czego odczytac.
  // Wysylamy pole tylko gdy plik jest wybrany — pusty string oznaczalby dla
  // backendu "podano sciezke", a to nieprawda.
  if (isLlamaCppEngine() && selection.modelFile) {
    body.gguf_file = selection.modelFile;
  }
  try {
    const wireResp = await ApiBinary.action('deployVllmRecommendRequest', body);
    // Decoder pakuje cala odpowiedz w pole `json` (60+ pol w 4 zagniezdzonych
    // structach — patrz tentaflow-protocol-wasm decode_message_body).
    const resp = wireResp && wireResp.json ? JSON.parse(wireResp.json) : wireResp;
    if (resp && resp.model_spec && !cachedModelSpec) {
      // Cache pomocniczy: dtype wag do readoutow. Pamiec KV liczy backend.
      const ms = resp.model_spec;
      cachedModelSpec = {
        num_layers: ms.num_hidden_layers || ms.num_layers || 0,
        num_kv_heads: ms.num_key_value_heads || ms.num_kv_heads || ms.num_attention_heads || 0,
        head_dim: ms.head_dim || (ms.hidden_size && ms.num_attention_heads ? Math.round(ms.hidden_size / ms.num_attention_heads) : 0),
        dtype: ms.dtype || 'fp16',
      };
    }
    return resp;
  } catch (err) {
    return { error: err.message || String(err) };
  }
}

// Human label of the weight storage format for the summary readouts. A
// quantized checkpoint (AWQ / compressed-tensors / fp8 / nvfp4) still reports
// the config `dtype` (bf16) — the calculator's detected `quantization` label is
// what the weights are actually stored in, so it wins when present.
const QUANT_LABELS = {
  awq: 'int4 (awq)',
  gptq: 'int4 (gptq)',
  compressed_tensors_4bit: 'int4 (compressed-tensors)',
  w8a16: 'int8 (compressed-tensors)',
  bnb_4bit: 'int4 (bitsandbytes)',
  bnb_8bit: 'int8 (bitsandbytes)',
  modelopt_fp8: 'fp8 (modelopt)',
};
function weightDtypeLabel(ms) {
  if (!ms) return '?';
  const q = String(ms.quantization || '').toLowerCase();
  if (!q) return ms.dtype || '?';
  return QUANT_LABELS[q] || q.replace(/_/g, '-');
}

// Mapuje wybor kv-bits ('none'|'8'|'4') na etykiete wire `kv_cache_dtype`
// (none|kv8|kv4) ktora backend (estimate_mlx_vram) rozpoznaje.
function mlxKvDtypeLabel() {
  const bits = String(selection.advanced?.mlx_kv_bits || 'none');
  if (bits === '8') return 'kv8';
  if (bits === '4') return 'kv4';
  return 'none';
}

// Maksymalny kontekst per sekwencja jaki zmiesci sie w budzecie — z backendu.
// `pool_tokens` to cala pula KV; przy N sekwencjach kontekst per sekwencja to
// pool_tokens / N (mlx-lm dzieli pule miedzy sloty). Floor do 512.
function mlxMaxContextFromBackend() {
  const rec = advancedRecommendation;
  if (!rec || rec.error || !rec.vram_estimate) return null;
  const pool = Number(rec.vram_estimate.pool_tokens) || 0;
  const seqs = Math.max(1, Number(selection.advanced?.mlx_max_num_seqs) || 1);
  let perSeq = Math.floor(pool / seqs);
  const maxPos = Number(rec.model_spec?.max_position_embeddings) || 0;
  if (maxPos > 0) perSeq = Math.min(perSeq, maxPos);
  return Math.max(0, Math.floor(perSeq / 512) * 512);
}

// Readout "max kontekst" — wydzielony, zeby handler inputa odswiezal tylko go
// (bez re-renderu calego body, co gubiloby focus na polu).
function mlxReadoutHtml() {
  const rec = advancedRecommendation;
  if (!rec) return `<div class="adv-loading">${escapeHtml(tAdv('mlx_computing'))}</div>`;
  if (rec.error) return `<div class="adv-error">${escapeHtml(rec.error)}</div>`;
  const v = rec.vram_estimate || {};
  const weightsGb = Number(v.model_weights_gb) || 0;
  const kvPoolGb = Number(v.kv_pool_gb) || 0;
  const maxPos = Number(rec.model_spec?.max_position_embeddings) || 0;
  if (v.fits_per_gpu === false || v.fits_total === false) {
    return `<div class="adv-error">${escapeHtml(tAdv('mlx_weights_overflow', { gb: weightsGb.toFixed(1) }))}</div>`;
  }
  const tokens = mlxMaxContextFromBackend();
  if (tokens == null) return `<div class="adv-loading">${escapeHtml(tAdv('mlx_set_budget'))}</div>`;
  return `
    <div class="adv-cell-value" style="font-size:1.4em;"><strong>${tokens.toLocaleString()}</strong> ${escapeHtml(tAdv('mlx_tokens_unit'))}</div>
    <div class="adv-cell-sub">${escapeHtml(tAdv('mlx_readout_sub', {
      pool: kvPoolGb.toFixed(2),
      weights: weightsGb.toFixed(1),
      max: maxPos ? maxPos.toLocaleString() : '—',
    }))}</div>`;
}

// MLX advanced step — reuzywa ten sam krok co vLLM, ale liczy budzet pamieci +
// kv-bits + seqs -> max kontekst (z backendu). Single device: zadnych TP/PP,
// zadnego gpu_memory_utilization-jako-VRAM, zadnego override kwantyzacji wag
// (wagi wynikaja z pobranego repo mlx-community).
function renderMlxAdvanced() {
  const model = getAdvancedModelName() || '?';
  const adv = selection.advanced;
  const kvBits = String(adv.mlx_kv_bits || 'none');
  const seqs = Math.max(1, Number(adv.mlx_max_num_seqs) || 1);

  return `
    <h4 class="wizard-step-title">${escapeHtml(tAdv('mlx_title'))}</h4>
    <p class="form-hint" style="margin-bottom:14px;">${escapeHtml(tAdv('mlx_subtitle'))}</p>
    <div class="adv-section">
      <div class="adv-summary-cell">
        <div class="adv-cell-label">${escapeHtml(tAdv('summary_model'))}</div>
        <div class="adv-cell-value"><code>${escapeHtml(model)}</code></div>
      </div>
    </div>
    <div class="adv-section">
      <div class="adv-sec-title">${escapeHtml(tAdv('mlx_mem_title'))}</div>
      <tf-input id="edw-mlx-mem" type="number" min="512" step="256"
        value="${escapeAttr(String(adv.mlx_max_memory_mb || ''))}" style="max-width:220px;"></tf-input>
      <div class="adv-hint">${escapeHtml(tAdv('mlx_mem_hint'))}</div>
      <div class="adv-row-2" style="margin-top:14px;">
        <div class="adv-form-row">
          <label>${escapeHtml(tAdv('mlx_kv_label'))}</label>
          <tf-select id="edw-mlx-kv" value="${escapeAttr(kvBits)}">
            <option value="none">${escapeHtml(tAdv('mlx_kv_opt_none'))}</option>
            <option value="8">${escapeHtml(tAdv('mlx_kv_opt_8'))}</option>
            <option value="4">${escapeHtml(tAdv('mlx_kv_opt_4'))}</option>
          </tf-select>
          <div class="adv-hint">${escapeHtml(tAdv('mlx_kv_hint'))}</div>
        </div>
        <div class="adv-form-row">
          <label><span>${escapeHtml(tAdv('mlx_seqs_label'))}</span><span class="v" id="edw-mlx-seqs-val">${seqs}</span></label>
          <input type="range" class="adv-range" id="edw-mlx-seqs" min="1" max="32" step="1" value="${seqs}">
          <div class="adv-hint">${escapeHtml(tAdv('mlx_seqs_hint'))}</div>
        </div>
      </div>
      <div style="margin-top:14px;">
        <div class="adv-cell-label">${escapeHtml(tAdv('mlx_max_ctx_label'))}</div>
        <div id="edw-mlx-readout">${mlxReadoutHtml()}</div>
      </div>
    </div>
  `;
}

/// Generic Advanced step driven purely by manifest [[parameter]] (ds4 and any
/// future engine outside the vLLM/MLX families). Renders one tf-* control per
/// declared parameter; values flow into selection.genericParams and then into
/// config_json.parameters{} verbatim.
function renderGenericAdvanced() {
  const isPl = I18n.getLanguage() === 'pl';
  const label = (p) => (isPl ? (p.label_pl || p.label_en) : (p.label_en || p.label_pl)) || p.key;

  const controls = manifestParams().map((p) => {
    const v = genericParamValue(p);
    const common = `id="edw-gp-${escapeAttr(p.key)}" data-gp-key="${escapeAttr(p.key)}" data-gp-kind="${escapeAttr(p.kind)}"`;
    let field = '';
    if (p.kind === 'enum') {
      const opts = (p.options || [])
        .map((o) => `<option value="${escapeAttr(String(o))}"${String(o) === String(v) ? ' selected' : ''}>${escapeHtml(String(o))}</option>`)
        .join('');
      field = `<tf-select ${common} value="${escapeAttr(String(v))}">${opts}</tf-select>`;
    } else if (p.kind === 'bool') {
      field = `<tf-toggle ${common} ${v === true || v === 'true' ? 'checked' : ''}></tf-toggle>`;
    } else if (p.kind === 'int' || p.kind === 'float') {
      const r = p.range || {};
      const step = p.kind === 'int' ? (r.step || 1) : (r.step || 'any');
      const minA = r.min !== undefined ? ` min="${r.min}"` : '';
      const maxA = r.max !== undefined ? ` max="${r.max}"` : '';
      field = `<tf-input ${common} type="number"${minA}${maxA} step="${step}" value="${escapeAttr(String(v ?? ''))}" style="max-width:220px;"></tf-input>`;
    } else {
      field = `<tf-input ${common} type="text" value="${escapeAttr(String(v ?? ''))}" style="max-width:320px;"></tf-input>`;
    }
    return `
      <div class="adv-field" style="display:flex;flex-direction:column;gap:6px;margin-bottom:14px;">
        <label for="edw-gp-${escapeAttr(p.key)}" style="font-weight:600;">${escapeHtml(label(p))}</label>
        ${field}
      </div>`;
  }).join('');

  return `
    <div class="adv-section">
      <div class="adv-sec-title">
        <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/></svg>
        ${escapeHtml(engineEntry?.engine?.name || engineId())}
      </div>
      ${controls}
    </div>`;
}

function renderStepAdvanced() {
  if (hasGenericParams()) {
    return renderGenericAdvanced();
  }
  if (isMlxEngine()) {
    return renderMlxAdvanced();
  }
  const model = getAdvancedModelName() || '?';
  const gpus = selection.isCluster ? getClusterAdvancedGpus() : getAdvancedGpus();
  const totalVramGb = gpus.reduce((acc, g) => acc + g.memory_gb, 0);
  // Cluster: make it explicit this is a distributed tensor-parallel budget
  // (N GPU across M nodes), not a single "1 × Spark · 119 GB" node.
  const members = selection.clusterMembers.length || 0;
  const perNode = Math.max(1, Number(selection.gpusPerNode) || 1);
  const gpuLabel = selection.isCluster
    ? (gpus.length > 0
      ? `Distributed tensor-parallel: ${gpus.length} GPU (${members} × ${perNode} GPU/node) · ${totalVramGb.toFixed(1)} GB · TP=${gpus.length}`
      : '—')
    : (gpus.length > 0
      ? `${gpus.length} × ${gpus[0].name} · ${totalVramGb.toFixed(1)} GB VRAM`
      : '—');

  const adv = selection.advanced;
  const rec = advancedRecommendation;
  const isLoading = !rec;
  const hasError = rec && rec.error;

  const tk = (k, params) => I18n.t(`catalog.deploy_wizard.advanced.${k}`, params);

  // Summary of selections from previous steps.
  const summaryCard = `
    <div class="adv-section">
      <div class="adv-sec-title">
        <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 11l3 3 8-8"/><path d="M21 12v7a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11"/></svg>
        ${escapeHtml(tk('summary_title'))}
      </div>
      <div class="adv-summary-grid">
        <div class="adv-summary-cell">
          <div class="adv-cell-label">${escapeHtml(tk('summary_model'))}</div>
          <div class="adv-cell-value"><code>${escapeHtml(model)}</code></div>
          ${rec && rec.model_spec ? `<div class="adv-cell-sub">${(rec.model_spec.estimated_params_billions || 0).toFixed(1)}B params · ${escapeHtml(weightDtypeLabel(rec.model_spec))} · max ctx ${(rec.model_spec.max_position_embeddings || 0).toLocaleString()}</div>` : ''}
        </div>
        <div class="adv-summary-cell">
          <div class="adv-cell-label">${escapeHtml(tk('summary_gpu'))}</div>
          <div class="adv-cell-value">${escapeHtml(gpuLabel)}</div>
          <div class="adv-cell-sub">${escapeHtml(selection.isCluster
            ? (selection.clusterMembers.map((m) => nodeDisplayName(m.node_id)).filter(Boolean).join(' · ') || '—')
            : (gpus.map((g) => `GPU ${g.index}`).join(' · ') || '—'))}</div>
        </div>
      </div>
    </div>
  `;

  // VRAM calculator section.
  // `rec.error` to TWARDY blad pobrania/odczytu konfiguracji modelu (404
  // config.json, parse, brak pliku .gguf) — NIE przepelnienie VRAM. Realne
  // przepelnienie wraca jako poprawna odpowiedz z vram_estimate.fits_total ===
  // false i jest renderowane w renderVramCard (pill_oom + baner).
  const vramCard = isLoading
    ? `<div class="adv-section"><div class="adv-loading">${escapeHtml(tk('loading_config'))}</div></div>`
    : hasError
      ? `
        <div class="adv-section">
          <div class="adv-error-box">
            <strong>${escapeHtml(tk('error_config_failed'))}</strong>
            ${escapeHtml(rec.error)}
            <div class="adv-error-hint">${escapeHtml(tk('error_config_hint'))}</div>
          </div>
        </div>`
      : renderVramCard(rec, totalVramGb, gpus.length);

  // Mode card with auto/manual toggle and lock reset (only in manual when locked).
  const showReset = adv.mode === 'manual' && adv.lockedParam;
  const resetBtn = showReset
    ? `<button type="button" class="adv-reset-lock" id="edw-adv-reset-lock" title="${escapeAttr(tk('reset_lock_title'))}">🔄 ${escapeHtml(tk('reset_lock'))}</button>`
    : '';
  const modeCard = `
    <div class="adv-section">
      <div class="adv-sec-title">
        <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3"/></svg>
        ${escapeHtml(tk('mode_title'))}
        <div class="adv-sec-actions">${resetBtn}</div>
      </div>
      <tf-segmented id="edw-adv-mode" value="${escapeAttr(adv.mode)}" size="sm">
        <option value="auto" variant="neutral">${escapeHtml(tk('mode_auto'))}</option>
        <option value="manual" variant="neutral">${escapeHtml(tk('mode_manual'))}</option>
      </tf-segmented>
      ${adv.mode === 'auto'
        ? renderAutoAlert(rec)
        : `<div class="adv-manual">${renderAdvancedManualControls(adv, rec)}</div>`}
    </div>
  `;

  return `
    <h4 class="wizard-step-title">${escapeHtml(tk('title'))}</h4>
    <p class="form-hint" style="margin-bottom:14px;">${escapeHtml(tk('subtitle'))}</p>
    ${summaryCard}
    ${vramCard}
    ${modeCard}
    ${isVllmFamilyEngine() ? renderExtraArgsCard(adv) : ''}
    ${renderSpeculativeCard(adv)}
  `;
}

// Free-text vLLM/SGLang flags. Both modes (auto/manual) — the text is
// tokenized and appended after every generated flag, so it wins on the backend.
function renderExtraArgsCard(adv) {
  return `
    <div class="adv-section">
      <div class="adv-sec-title">
        <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/></svg>
        ${escapeHtml(tAdv('extra_args_label'))}
      </div>
      <div class="adv-form-row">
        <tf-textarea id="edw-adv-extra-args" rows="2" placeholder="--enable-prefix-caching --seed 42"
          value="${escapeAttr(adv.extra_args || '')}"></tf-textarea>
        <div class="adv-hint">${escapeHtml(tAdv('extra_args_hint'))}</div>
      </div>
    </div>
  `;
}

// Speculative Decoding panel — emit `--speculative-config '{...}'` w VLLM_ARGS.
// Pre-fillsuje sie z `model_preset.speculator_*`, ale toggle jest user-opt-in.
// Speculative methods that draft from the target itself — no drafter repo.
const SPEC_METHODS_WITHOUT_MODEL = ['mtp', 'ngram'];

function specMethodNeedsModel(method) {
  return !SPEC_METHODS_WITHOUT_MODEL.includes(String(method || '').toLowerCase());
}

function nativeMtpAvailable() {
  return advancedRecommendation?.native_mtp_available === true;
}

function renderSpeculativeCard(adv) {
  const sp = adv?.speculative || {};
  const tk = (k, params) => I18n.t(`catalog.deploy_wizard.advanced.${k}`, params);
  const enabled = sp.enabled === true;
  const method = sp.method || 'dflash';
  const needsModel = specMethodNeedsModel(method);
  const nativeMtpChip = nativeMtpAvailable()
    ? `<div class="adv-form-row"><tf-chip status="info" icon="zap">${escapeHtml(tk('spec_native_mtp'))}</tf-chip></div>`
    : '';
  const tooltipPl = 'Speculative decoding (vLLM --speculative-config). Mały model-drafter predyktuje N tokenów do przodu, target verifuje równolegle. Zysk do 2× szybszy decode bez utraty jakości. Wymaga sparowanego speculatora (np. RedHatAI/...-speculator.dflash).';
  return `
    <div class="adv-section">
      <div class="adv-sec-title">
        <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="13 17 18 12 13 7"/><polyline points="6 17 11 12 6 7"/></svg>
        Speculative Decoding
        <span class="adv-cell-sub" style="margin-left:8px;font-weight:normal;" title="${escapeAttr(tooltipPl)}">vLLM --speculative-config</span>
      </div>
      <div class="adv-form-row">
        <label><tf-toggle id="edw-adv-spec-enabled" ${enabled ? 'checked' : ''}></tf-toggle> <span>Włącz speculative decoding</span></label>
        <div class="adv-hint">Drafter musi być kompatybilny z modelem (np. ten sam tokenizer / wytrenowany pod target). Brak parowania — startup vLLM zakończy się błędem.</div>
      </div>
      ${nativeMtpChip}
      <div class="adv-row-2" style="${enabled ? '' : 'display:none;'}" id="edw-adv-spec-fields">
        <div class="adv-form-row" id="edw-adv-spec-model-row" style="${needsModel ? '' : 'display:none;'}">
          <label><span>Speculator (HF repo)</span></label>
          <tf-input id="edw-adv-spec-model" placeholder="RedHatAI/gemma-4-31B-it-speculator.dflash" value="${escapeAttr(sp.model || '')}"></tf-input>
          <div class="adv-hint">Repo z drafterem (sufix .dflash / .pflash / itd.).</div>
        </div>
        <div class="adv-form-row">
          <label><span>Method</span></label>
          <tf-select id="edw-adv-spec-method" value="${escapeAttr(method)}">
            <option value="dflash">dflash</option>
            <option value="pflash">pflash</option>
            <option value="eagle">eagle</option>
            <option value="eagle3">eagle3</option>
            <option value="medusa">medusa</option>
            <option value="mtp">mtp</option>
            <option value="draft_model">draft_model</option>
            <option value="ngram">ngram</option>
          </tf-select>
          <div class="adv-hint">Wartość przekazywana do vLLM 1:1.</div>
          <div class="adv-hint" id="edw-adv-spec-nomodel-hint" style="${needsModel ? 'display:none;' : ''}">${escapeHtml(tk('spec_no_model_hint'))}</div>
        </div>
      </div>
      <div class="adv-row-2" style="${enabled ? '' : 'display:none;'}" id="edw-adv-spec-tokens-row">
        <div class="adv-form-row">
          <label><span>num_speculative_tokens</span><span class="v" id="edw-adv-spec-num-val">${sp.num_tokens || 8}</span></label>
          <input type="range" class="adv-range" id="edw-adv-spec-num" min="1" max="16" step="1" value="${sp.num_tokens || 8}">
          <div class="adv-hint">Ile tokenów drafter predyktuje per krok. Typowo 4-8. Większa wartość = więcej zgadywania, ale więcej rejectów na trudnych promptach.</div>
        </div>
      </div>
    </div>
  `;
}

function tAdv(k, params) { return I18n.t(`catalog.deploy_wizard.advanced.${k}`, params); }

function renderVramCard(rec, totalVramGb, gpuCount) {
  const v = rec.vram_estimate || {};
  const r = rec.recommended || {};
  const isLcpp = isLlamaCppEngine();
  const perGpu = v.per_gpu_gb || 0;
  const tpPp = (r.tensor_parallel || 1) * (r.pipeline_parallel || 1);
  // Backend liczy `total_gb` poprawnie dla obu silnikow (dla llama.cpp compute
  // buffer liczony raz, nie ×N GPU). Mnozenie perGpu × tpPp client-side
  // zawyzaloby total dla llama.cpp, wiec preferujemy autorytatywna wartosc.
  const totalUsed = (typeof v.total_gb === 'number' && v.total_gb > 0) ? v.total_gb : perGpu * tpPp;
  const headroomGb = totalVramGb - totalUsed;
  const pctUsed = totalVramGb > 0 ? Math.min(200, Math.round((totalUsed / totalVramGb) * 100)) : 0;
  // Werdykt OOM nalezy do backendu (model puli: dla vLLM total_gb ≈ util×VRAM
  // gdy sie miesci, co jest POPRAWNE — vLLM realnie zajmuje util×VRAM). Czerwien
  // tylko gdy backend zwroci fits_per_gpu/fits_total == false. NIE traktujemy
  // wysokiego pctUsed jako OOM, inaczej "mieszczacy sie" deploy migalby na
  // czerwono mimo poprawnej konfiguracji.
  const backendFits = v.fits_per_gpu !== false && v.fits_total !== false;

  // Backend raportuje `at_limit: true` gdy konfiguracja jest na granicy — wtedy
  // pokazujemy zolty pill (auto_adjusted, mieści się, ale brak zapasu).
  const backendAtLimit = rec && rec.at_limit === true;
  let pillCls = 'adv-pill ok';
  let pillTxt = backendAtLimit ? tAdv('pill_at_limit', { p: pctUsed }) : tAdv('pill_fits');
  let barCls = 'ok';
  let kvCls = '';
  let leftCls = 'success';
  let totalCls = 'accent';
  if (!backendFits) {
    pillCls = 'adv-pill danger'; pillTxt = tAdv('pill_oom', { p: pctUsed });
    barCls = 'danger'; kvCls = 'danger'; leftCls = 'danger'; totalCls = 'danger';
  } else if (pctUsed > 90 || backendAtLimit) {
    pillCls = 'adv-pill warn';
    if (!backendAtLimit) pillTxt = tAdv('pill_warn', { p: pctUsed });
    barCls = 'warn'; kvCls = 'warn'; leftCls = 'warn';
  }
  // Pulse 1x na przejsciu na "at limit" zeby user zauwazyl. Klasa
  // jest dodawana tylko gdy stan zmienil sie z false -> true.
  const becameAtLimit = backendAtLimit && !prevAtLimit;
  prevAtLimit = backendAtLimit;
  if (becameAtLimit) pillCls += ' pulse';

  const weightsGb = v.model_weights_gb || 0;
  // KV to PULA resztkowa (vLLM: util*VRAM - wagi - aktywacje), nie skladnik
  // wymagany. `kv_pool_gb` jest autorytatywne; `kv_cache_gb` zostaje fallbackiem
  // dla starszych odpowiedzi. Pula nie zalezy od max_num_seqs (KRYT-1).
  const kvGb = (typeof v.kv_pool_gb === 'number' && v.kv_pool_gb > 0) ? v.kv_pool_gb : (v.kv_cache_gb || 0);
  const poolTokens = Number(v.pool_tokens) || 0;
  const concurrentSeqs = Number(v.concurrent_full_len_seqs) || 0;
  const actGb = v.activations_gb || 0;
  // Legend percentages must sum to <=100. When totalUsed > totalVramGb each
  // segment would clamp at 100%, producing nonsense like "59 + 100 + 56 = 215%".
  // Normalize against max(totalVramGb, totalUsed) so segments stay proportional
  // and free shows real headroom (or is omitted when negative).
  const legendBase = Math.max(totalVramGb, totalUsed);
  const w = (n) => legendBase > 0 ? (n / legendBase) * 100 : 0;
  const overflow = totalUsed > totalVramGb;
  const freeGb = Math.max(0, totalVramGb - totalUsed);

  // Realne przepelnienie VRAM (jedyne miejsce uzycia error_no_fit). Werdykt
  // nalezy do backendu (fits_per_gpu/fits_total). NIE eskalujemy na podstawie
  // samego pctUsed — vLLM celowo zajmuje util*VRAM i to nie jest OOM.
  const doesNotFit = !backendFits;
  const noFitBanner = doesNotFit
    ? `<div class="adv-error-box"><strong>${escapeHtml(tAdv('error_no_fit'))}</strong>${escapeHtml(tAdv('error_no_fit_hint'))}</div>`
    : '';

  return `
    <div class="adv-section">
      <div class="adv-sec-title">
        <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M2 3h20v18H2z"/><path d="M2 9h20"/></svg>
        ${escapeHtml(tAdv('vram_calc_title'))}
        <div class="adv-sec-actions"><span class="${pillCls}">${escapeHtml(pillTxt)}</span></div>
      </div>
      ${noFitBanner}
      <div class="adv-kpi-grid" id="edw-adv-kpi">
        <div class="adv-kpi"><div class="k-label">${escapeHtml(tAdv('kpi_weights'))}</div><div class="k-value">${weightsGb.toFixed(1)} GB</div><div class="k-sub">${escapeHtml(weightDtypeLabel(rec.model_spec))}</div></div>
        <div class="adv-kpi ${kvCls}"><div class="k-label">${escapeHtml(isLcpp ? tAdv('kpi_kv_total') : tAdv('kpi_kv_pool'))}</div><div class="k-value">${kvGb.toFixed(1)} GB</div><div class="k-sub">${escapeHtml(tAdv(isLcpp ? 'kpi_kv_total_sub' : 'kpi_kv_pool_sub', {
          tokens: poolTokens.toLocaleString(),
          // llama.cpp: whole slots only (-c = ctx × slots); vLLM: fractional pool capacity.
          seqs: isLcpp ? Math.max(1, Math.round(concurrentSeqs)).toLocaleString()
            : (concurrentSeqs >= 10 ? Math.round(concurrentSeqs).toLocaleString() : concurrentSeqs.toFixed(1)),
          ctx: (r.max_model_len || 0).toLocaleString(),
        }))}</div></div>
        <div class="adv-kpi"><div class="k-label">${escapeHtml(isLcpp ? tAdv('kpi_compute_buffer') : tAdv('kpi_activations'))}</div><div class="k-value">${actGb.toFixed(1)} GB</div><div class="k-sub">${escapeHtml(isLcpp ? tAdv('kpi_compute_buffer_sub') : tAdv('kpi_activations_sub'))}</div></div>
        <div class="adv-kpi ${leftCls}"><div class="k-label">${escapeHtml(tAdv('kpi_headroom'))}</div><div class="k-value">${headroomGb >= 0 ? headroomGb.toFixed(1) : '−' + Math.abs(headroomGb).toFixed(1)} GB</div><div class="k-sub">${escapeHtml(tAdv('kpi_headroom_sub', { p: Math.max(0, 100 - pctUsed) }))}</div></div>
        <div class="adv-kpi ${totalCls}"><div class="k-label">${escapeHtml(tAdv('kpi_total_avail'))}</div><div class="k-value">${totalUsed.toFixed(1)} GB / ${totalVramGb.toFixed(0)} GB</div><div class="k-sub">${escapeHtml(tAdv('kpi_total_sub', { p: pctUsed, n: gpuCount }))}</div></div>
      </div>
      <div class="adv-vram-bar-wrap">
        <div class="adv-vram-head"><span>${escapeHtml(tAdv('vram_usage'))}</span><span class="pct">${pctUsed}%</span></div>
        <div class="adv-vram-bar"><div class="fill ${barCls}" style="width:${Math.min(100, pctUsed)}%"></div></div>
        <div class="adv-vram-legend">
          <span class="lg-w">${escapeHtml(tAdv('legend_weights', { p: w(weightsGb).toFixed(0) }))}</span>
          <span class="lg-kv">${escapeHtml(tAdv('legend_kv', { p: w(kvGb).toFixed(0) }))}</span>
          <span class="lg-act">${escapeHtml(isLcpp ? tAdv('legend_compute', { p: w(actGb).toFixed(0) }) : tAdv('legend_activations', { p: w(actGb).toFixed(0) }))}</span>
          ${overflow
            ? `<span class="lg-free danger">${escapeHtml(tAdv('legend_short', { gb: Math.abs(headroomGb).toFixed(1) }))}</span>`
            : `<span class="lg-free">${escapeHtml(tAdv('legend_free', { p: w(freeGb).toFixed(0) }))}</span>`}
        </div>
      </div>
    </div>
  `;
}

function renderAutoAlert(rec) {
  if (!rec || rec.error) {
    return `<div class="form-hint" style="margin-top:10px;">${escapeHtml(tAdv('auto_default_hint'))}</div>`;
  }
  const r = rec.recommended || {};
  const args = rec.recommended_vllm_args || '';
  const warnings = rec.warnings || [];
  // Official vLLM recipe (recipes.vllm.ai) badge: signals that expert flags +
  // per-GPU env were pre-filled from the matched recipe. Args stay editable.
  const recipeEnv = rec.recommended_env || {};
  const envList = Object.keys(recipeEnv);
  const recipeBadge = rec.recipe_applied
    ? `<div style="margin-top:10px; padding:8px 10px; background:#e8f5e9; border:1px solid #66bb6a; border-radius:6px; font-size:12px; color:#1b5e20;">
        ✓ ${escapeHtml(tAdv('recipe_applied', { id: rec.recipe_applied }))}${
          envList.length
            ? `<br><span style="font-size:11px;">env: ${envList.map((k) => `<code>${escapeHtml(k)}=${escapeHtml(String(recipeEnv[k]))}</code>`).join(' ')}</span>`
            : ''
        }
      </div>`
    : '';
  // GPU compatibility: when the chosen GPU count doesn't fit the model
  // architecture (TP must divide num_attention_heads, PP must divide
  // num_hidden_layers), surface a warning chip with better-fitting counts.
  const compat = rec.gpu_compatibility;
  let compatChip = '';
  if (compat && !compat.clean_partition) {
    const better = (compat.better_gpu_counts || []).map((n) => `<code>${n}</code>`).join(' / ');
    compatChip = `
      <div style="margin-top:10px; padding:10px; background:#fff4e0; border:1px solid #ffb84d; border-radius:6px; font-size:12px; color:#663d00;">
        ⚠️ <strong>${escapeHtml(tAdv('compat_warn_title'))}</strong>
        ${escapeHtml(compat.warning || '')}<br>
        <em>${escapeHtml(tAdv('compat_better', { options: '' }))} ${better || '—'}</em>
      </div>
    `;
  } else if (compat && !compat.uses_all_gpus) {
    const better = (compat.better_gpu_counts || []).map((n) => `<code>${n}</code>`).join(' / ');
    compatChip = `
      <div style="margin-top:10px; padding:8px; background:#fffbe5; border:1px solid #f5d76e; border-radius:6px; font-size:12px; color:#5c4500;">
        ℹ️ ${escapeHtml(tAdv('compat_idle', { tp: compat.used_tp, pp: compat.used_pp, used: compat.used_tp * compat.used_pp, options: '' }))} ${better}
      </div>
    `;
  }
  return `
    <div class="adv-alert info">
      <div class="adv-alert-ico"><svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="16" x2="12" y2="12"/><line x1="12" y1="8" x2="12.01" y2="8"/></svg></div>
      <div class="adv-alert-body">
        <strong>${escapeHtml(tAdv('rec_intro'))}</strong>
        ${escapeHtml(tAdv('rec_summary', {
          tp: r.tensor_parallel || 1,
          pp: r.pipeline_parallel || 1,
          ctx: (r.max_model_len || 0).toLocaleString(),
          dtype: r.kv_cache_dtype || 'auto',
          seqs: r.max_num_seqs || 0,
          mu: (r.gpu_memory_utilization || 0.9).toFixed(2),
        }))}
        ${recipeBadge}
        ${args ? `<div class="adv-alert-args">${escapeHtml(args)}</div>` : ''}
        ${compatChip}
        ${warnings.length > 0 ? `<ul class="adv-alert-warn">${warnings.map((w) => `<li>${escapeHtml(w)}</li>`).join('')}</ul>` : ''}
      </div>
    </div>
  `;
}

// Presety kontekstu pokazywane jako chipy. Górny limit 1M — nawet jeśli
// model deklaruje mniej, chipy ponad max są wyszarzone (klasa "exceeds").
const CTX_PRESETS = [
  { label: '4k',   value: 4096 },
  { label: '8k',   value: 8192 },
  { label: '16k',  value: 16384 },
  { label: '32k',  value: 32768 },
  { label: '64k',  value: 65536 },
  { label: '128k', value: 131072 },
  { label: '262k', value: 262144 },
  { label: '512k', value: 524288 },
  { label: '1M',   value: 1048576 },
];

function renderAdvancedManualControls(adv, rec) {
  // `applied` jest aktualnym stanem auto-fit po overrides (backend zwraca
  // realne wartosci jakie zaproponowal po lockach usera). `recommended` zostaje
  // jako fallback gdy backend nie zwraca jeszcze `applied` (graceful degradation).
  const applied = rec?.applied || rec?.recommended || {};
  const recCfg = rec?.recommended || {};
  const isLcpp = isLlamaCppEngine();
  const autoAdjusted = new Set(Array.isArray(rec?.auto_adjusted) ? rec.auto_adjusted : []);
  const lockedParam = adv.lockedParam || null;

  // Limity per-param. Backend (po refaktorze rust) udostepnia
  // `max_supported_<param>` indywidualnie liczone pod aktualne overrides
  // — dzieki temu po zalockowaniu np. ctx, max num_seqs natychmiast spada.
  const modelMaxCtx = rec?.model_spec?.max_position_embeddings || 0;
  const vramMaxCtx = rec?.max_supported_model_len || 0;
  const ABSOLUTE_MAX = 1_048_576;
  const maxCtx = Math.min(ABSOLUTE_MAX, Math.max(modelMaxCtx, vramMaxCtx, 32768));
  // Seqs slider cap. vLLM family: fixed scheduler cap 256 (no memory cost).
  // llama.cpp: each slot reserves a full KV context, so the real cap comes
  // from the backend (`max_supported_num_seqs`).
  const seqsCap = isLcpp
    ? Math.max(1, Math.min(256, Number(rec?.max_supported_num_seqs) || 1))
    : 256;

  // Wartosci pokazywane na sliderach: dla locked param bierzemy z `adv`
  // (user-set), dla pozostalych z `applied` (auto-fit przez backend).
  const valueFor = (key, fallback) => {
    if (lockedParam === key) return adv[key] ?? applied[key] ?? recCfg[key] ?? fallback;
    return applied[key] ?? adv[key] ?? recCfg[key] ?? fallback;
  };

  const tp = valueFor('tensor_parallel', 1);
  const pp = adv.pipeline_parallel ?? applied.pipeline_parallel ?? recCfg.pipeline_parallel ?? 1;
  const ctx = valueFor('max_model_len', 8192);
  const seqs = valueFor('max_num_seqs', 16);
  // Never let the slider clamp a backend-applied value.
  const maxSeqs = Math.max(seqsCap, Number(seqs) || 1);
  // llama.cpp has no 'auto' option (its selects list f16..iq4_nl), so normalize
  // 'auto' to the engine default f16 — otherwise the tf-select renders empty.
  const normKv = (v) => (isLcpp && v === 'auto') ? 'f16' : v;
  const kv = normKv(adv.kv_cache_dtype || applied.kv_cache_dtype || recCfg.kv_cache_dtype || (isLcpp ? 'f16' : 'auto'));
  const kvV = normKv(adv.kv_cache_dtype_v || kv);
  const memUtil = valueFor('gpu_memory_utilization', isPoolingEngine() ? 0.2 : 0.9);
  const totalGpus = (getAdvancedGpus() || []).length || 1;

  // Helper: render the auto-adjust hint shown below a slider.
  const adjustHint = (key, prevVal, newVal) => {
    if (lockedParam === key) return ''; // locked = user value, never auto-tune
    if (!autoAdjusted.has(key)) return '';
    const fmt = (v) => typeof v === 'number' ? (v >= 1 ? v.toLocaleString() : v.toFixed(2)) : String(v ?? '?');
    const prevTxt = fmt(prevVal);
    const newTxt = fmt(newVal);
    // "Auto-adjusted from X to X" is noise — only show a real delta.
    if (prevTxt === newTxt) return '';
    return `<div class="adv-hint adjust-warn">⚙ ${escapeHtml(tAdv('auto_adjusted', { prev: prevTxt, new: newTxt }))}</div>`;
  };

  // Helper: lock marker rendered next to the slider label.
  const lockMark = (key) => lockedParam === key
    ? `<span class="adv-lock-tag" title="${escapeAttr(tAdv('lock_title'))}">🔒 ${escapeHtml(tAdv('lock_tag'))}</span>`
    : '';

  // Preset chips — disabled when they exceed the model's max context.
  const chips = CTX_PRESETS.map((p) => {
    const exceeds = p.value > maxCtx;
    const active = !exceeds && Math.abs(p.value - ctx) < 1024;
    const cls = ['adv-ctx-chip'];
    if (active) cls.push('active');
    if (exceeds) cls.push('exceeds');
    const title = exceeds
      ? tAdv('ctx_chip_exceeds', { max: maxCtx.toLocaleString() })
      : tAdv('ctx_chip_set', { label: p.label });
    return `<button type="button" class="${cls.join(' ')}" data-ctx="${p.value}" title="${escapeAttr(title)}" ${exceeds ? 'disabled' : ''}>${escapeHtml(p.label)}</button>`;
  }).join('');
  // "Max that fits in VRAM" — the backend's max_supported_model_len rounded
  // down to a 1024 boundary so the slider lands on a clean value.
  const vramFitCtx = Math.floor(vramMaxCtx / 1024) * 1024;
  const vramFitChip = vramFitCtx >= 1024
    ? `<tf-button size="sm" variant="ghost" icon="zap" id="edw-adv-ctx-vram-max" data-ctx="${vramFitCtx}" title="${escapeAttr(tAdv('ctx_chip_vram_max_title', { ctx: vramFitCtx.toLocaleString() }))}">${escapeHtml(tAdv('ctx_chip_vram_max'))}</tf-button>`
    : '';

  // Hinty auto-adjust — porownujemy applied z recommended zeby pokazac
  // delty. Backend powinien zwracac obie wartosci, ale dzialamy defensive
  // (graceful degradation): jezeli `auto_adjusted` puste, hint sie nie
  // pojawi, a porownanie applied vs recommended jest tylko podpowiedzia.
  const ctxAdjust = adjustHint('max_model_len', recCfg.max_model_len, applied.max_model_len);
  const seqsAdjust = adjustHint('max_num_seqs', recCfg.max_num_seqs, applied.max_num_seqs);
  const tpAdjust = adjustHint('tensor_parallel', recCfg.tensor_parallel, applied.tensor_parallel);
  const memAdjust = adjustHint('gpu_memory_utilization', recCfg.gpu_memory_utilization, applied.gpu_memory_utilization);

  const ctxHint = vramMaxCtx
    ? tAdv('ctx_hint_max_with_vram', { model: modelMaxCtx ? modelMaxCtx.toLocaleString() : '?', vram: vramMaxCtx.toLocaleString() })
    : tAdv('ctx_hint_max', { model: modelMaxCtx ? modelMaxCtx.toLocaleString() : '?' });

  // Override kwantyzacji WAG — tylko vLLM-rodzina (safetensors). Dla llama.cpp
  // i MLX wagi wynikaja z pobranego pliku (GGUF / repo mlx-community), wiec
  // dropdown jest ukryty (zadnego pola na zapas).
  let quantRowHtml = '';
  if (isVllmFamilyEngine()) {
    const quant = (adv.quantization || '').toLowerCase();
    const QUANT_OPTS = ['nvfp4', 'mxfp4', 'fp8', 'int8', 'awq', 'gptq'];
    const quantOpts = [`<option value="">${escapeHtml(tAdv('quant_opt_source'))}</option>`];
    if (quant && !QUANT_OPTS.includes(quant)) {
      quantOpts.push(`<option value="${escapeAttr(quant)}">${escapeHtml(quant.toUpperCase())}</option>`);
    }
    QUANT_OPTS.forEach((q) => quantOpts.push(`<option value="${q}">${q.toUpperCase()}</option>`));
    quantRowHtml = `
    <div class="adv-form-row">
      <label>${escapeHtml(tAdv('quant_label'))}</label>
      <tf-select id="edw-adv-quant" value="${escapeAttr(quant)}">
        ${quantOpts.join('')}
      </tf-select>
      <div class="adv-hint">${escapeHtml(tAdv('quant_hint'))}</div>
    </div>`;
  }

  // `--trust-remote-code` toggle (vLLM-rodzina). Default ON — wiele repo (Gemma
  // 4, DeepSeek V4) wymaga wlasnego kodu modelujacego. Zdjecie = bezpieczniejsze
  // dla nieufnego repo (kod nie wykona sie przy ladowaniu).
  let trustRowHtml = '';
  if (isVllmFamilyEngine()) {
    const on = adv.trust_remote_code !== false;
    trustRowHtml = `
    <div class="adv-form-row">
      <label><tf-toggle id="edw-adv-trust" ${on ? 'checked' : ''}></tf-toggle> <span>${escapeHtml(tAdv('trust_remote_code_label'))}</span></label>
      <div class="adv-hint">${escapeHtml(tAdv('trust_remote_code_hint'))}</div>
    </div>`;
  }

  // Sekcja KV cache — engine-aware. vLLM: jeden select (auto/fp8*) + opcjonalny
  // max-num-batched-tokens. llama.cpp: osobne K i V (f16..iq4_nl) + chip
  // flash-attention gdy kwantyzowane. Etykiety = realne tokeny CLI silnika
  // (vLLM nie ma fp16/bfloat16, llama nie ma fp8) — DRUG-11/DRUG-12.
  const VLLM_KV_OPTS = [
    ['auto', tAdv('kv_opt_auto')],
    ['fp8', tAdv('kv_opt_fp8')],
    ['fp8_e4m3', tAdv('kv_opt_fp8_e4m3')],
    ['fp8_e5m2', tAdv('kv_opt_fp8_e5m2')],
  ];
  const LCPP_KV_OPTS = [
    ['f16', tAdv('kv_opt_f16')],
    ['bf16', tAdv('kv_opt_bf16')],
    ['q8_0', tAdv('kv_opt_q8_0')],
    ['q5_1', tAdv('kv_opt_q5_1')],
    ['q5_0', tAdv('kv_opt_q5_0')],
    ['q4_1', tAdv('kv_opt_q4_1')],
    ['q4_0', tAdv('kv_opt_q4_0')],
    ['iq4_nl', tAdv('kv_opt_iq4_nl')],
  ];
  const kvOptionsHtml = (opts, selected) => opts.map(([val, label]) =>
    `<option value="${escapeAttr(val)}"${val === selected ? ' selected' : ''}>${escapeHtml(label)}</option>`).join('');

  // llama.cpp: kwantyzowane K lub V wlacza flash-attention (backend dodaje -fa).
  const lcppKvQuantized = (t) => !['f16', 'bf16', 'auto'].includes(String(t).toLowerCase());
  const flashChip = isLcpp && (lcppKvQuantized(kv) || lcppKvQuantized(kvV))
    ? `<tf-chip status="info" icon="zap">${escapeHtml(tAdv('flash_attn_auto'))}</tf-chip>`
    : '';

  let kvSectionHtml;
  if (isLcpp) {
    kvSectionHtml = `
    <div class="adv-row-2">
      <div class="adv-form-row">
        <label>${escapeHtml(tAdv('kv_k_label'))}</label>
        <tf-select id="edw-adv-kv" value="${escapeAttr(kv)}">${kvOptionsHtml(LCPP_KV_OPTS, kv)}</tf-select>
        <div class="adv-hint">${escapeHtml(tAdv('kv_k_hint'))}</div>
      </div>
      <div class="adv-form-row">
        <label>${escapeHtml(tAdv('kv_v_label'))}</label>
        <tf-select id="edw-adv-kv-v" value="${escapeAttr(kvV)}">${kvOptionsHtml(LCPP_KV_OPTS, kvV)}</tf-select>
        <div class="adv-hint">${escapeHtml(tAdv('kv_v_hint'))}</div>
      </div>
    </div>
    ${flashChip ? `<div class="adv-form-row">${flashChip}</div>` : ''}
    <div class="adv-form-row">
      <tf-chip status="info" icon="cpu">${escapeHtml(tAdv('llamacpp_ngl_info'))}</tf-chip>
    </div>`;
  } else {
    const batch = Number(adv.max_num_batched_tokens) || 8192;
    kvSectionHtml = `
    <div class="adv-row-2">
      <div class="adv-form-row">
        <label>${escapeHtml(tAdv('kv_label'))}</label>
        <tf-select id="edw-adv-kv" value="${escapeAttr(kv)}">${kvOptionsHtml(VLLM_KV_OPTS, kv)}</tf-select>
        <div class="adv-hint">${escapeHtml(tAdv('kv_hint'))}</div>
      </div>
      <div class="adv-form-row">
        <label>${escapeHtml(tAdv('batch_label'))}</label>
        <tf-input type="number" id="edw-adv-batch" min="512" step="512" value="${batch}"></tf-input>
        <div class="adv-hint">${escapeHtml(tAdv('batch_hint'))}</div>
      </div>
    </div>`;
  }

  return `
    <div class="adv-form-row">
      <label>
        <span>${escapeHtml(tAdv('ctx_label'))} ${lockMark('max_model_len')}</span>
        <span class="v" id="edw-adv-ctx-val">${ctx.toLocaleString()}</span>
      </label>
      <input type="range" class="adv-range" id="edw-adv-ctx" min="512" max="${maxCtx}" step="512" value="${ctx}">
      <div class="adv-ctx-presets">${chips}${vramFitChip}</div>
      <div class="adv-hint">${escapeHtml(ctxHint)}</div>
      ${ctxAdjust}
    </div>

    <div class="adv-row-2">
      <div class="adv-form-row">
        <label><span>${escapeHtml(isLcpp ? tAdv('tp_label_llamacpp') : tAdv('tp_label'))} ${lockMark('tensor_parallel')}</span><span class="v">${tp}</span></label>
        <tf-input type="number" id="edw-adv-tp" min="1" max="${totalGpus}" value="${tp}"></tf-input>
        <div class="adv-hint">${escapeHtml(tAdv('tp_hint', { n: totalGpus }))}</div>
        ${tpAdjust}
      </div>
      <div class="adv-form-row">
        <label><span>${escapeHtml(isLcpp ? tAdv('pp_label_llamacpp') : tAdv('pp_label'))}</span><span class="v">${pp}</span></label>
        <tf-input type="number" id="edw-adv-pp" min="1" max="${totalGpus}" value="${pp}"></tf-input>
        <div class="adv-hint">${escapeHtml(tAdv('pp_hint', { n: totalGpus }))}</div>
      </div>
    </div>

    <div class="adv-row-2">
      <div class="adv-form-row">
        <label><span>${escapeHtml(tAdv('seqs_label'))} ${lockMark('max_num_seqs')}</span><span class="v" id="edw-adv-seqs-val">${seqs}</span></label>
        <input type="range" class="adv-range" id="edw-adv-seqs" min="1" max="${maxSeqs}" step="1" value="${seqs}">
        <div class="adv-hint">${escapeHtml(isLcpp ? tAdv('seqs_hint_llamacpp') : tAdv('seqs_hint'))}</div>
        ${seqsAdjust}
      </div>
      ${isLcpp ? '' : `
      <div class="adv-form-row">
        <label><span>${escapeHtml(tAdv('mem_label'))} ${lockMark('gpu_memory_utilization')}</span><span class="v" id="edw-adv-mem-val">${(memUtil * 100).toFixed(0)}%</span></label>
        <input type="range" class="adv-range" id="edw-adv-mem" min="0.15" max="0.9" step="0.05" value="${memUtil}">
        <div class="adv-hint">${escapeHtml(tAdv('mem_hint'))}</div>
        ${memAdjust}
      </div>`}
    </div>

    ${quantRowHtml}

    ${trustRowHtml}

    ${kvSectionHtml}

    <div class="adv-hint" style="margin-top:10px;">
      ${escapeHtml(isLcpp ? tAdv('llamacpp_args_note') : tAdv('vllm_args_note'))}
    </div>
  `;
}

function bindAdvancedHandlers() {
  // Generic manifest params (ds4 etc.): one change handler per declared control,
  // value coerced by kind and stored in selection.genericParams.
  if (hasGenericParams()) {
    document.querySelectorAll('[data-gp-key]').forEach((el) => {
      const key = el.getAttribute('data-gp-key');
      const kind = el.getAttribute('data-gp-kind');
      const handler = (e) => {
        let val;
        if (kind === 'bool') {
          val = e.detail?.checked ?? el.checked ?? el.hasAttribute('checked');
        } else if (kind === 'int' || kind === 'float') {
          const raw = e.detail?.value ?? el.value;
          const num = Number(raw);
          val = Number.isFinite(num) ? (kind === 'int' ? Math.round(num) : num) : null;
        } else {
          val = e.detail?.value ?? el.value ?? '';
        }
        selection.genericParams[key] = val;
      };
      el.addEventListener('change', handler);
      if (kind === 'int' || kind === 'float' || kind === 'string') {
        el.addEventListener('input', handler);
      }
    });
    return;
  }
  // MLX: budzet pamieci / kv-bits / seqs -> backend liczy pule KV i pool_tokens,
  // readout odczytuje max kontekst z odpowiedzi. Debounce, zeby kazdy ruch
  // suwaka nie odpalal HF fetchu.
  const mlxMem = document.getElementById('edw-mlx-mem');
  if (mlxMem) {
    const onMlxMem = () => {
      const v = Math.max(0, Math.floor(Number(mlxMem.value) || 0));
      selection.advanced.mlx_max_memory_mb = v;
      const out = document.getElementById('edw-mlx-readout');
      if (out) out.innerHTML = `<div class="adv-loading">${escapeHtml(tAdv('mlx_computing'))}</div>`;
      mlxDebounceRecompute();
    };
    mlxMem.addEventListener('input', onMlxMem);
    mlxMem.addEventListener('change', onMlxMem);
  }

  const mlxKv = document.getElementById('edw-mlx-kv');
  if (mlxKv) {
    mlxKv.addEventListener('change', (e) => {
      selection.advanced.mlx_kv_bits = String(e.detail?.value ?? mlxKv.value ?? 'none');
      mlxDebounceRecompute();
    });
  }

  const mlxSeqs = document.getElementById('edw-mlx-seqs');
  const mlxSeqsVal = document.getElementById('edw-mlx-seqs-val');
  if (mlxSeqs) {
    mlxSeqs.addEventListener('input', () => {
      const v = Math.max(1, parseInt(mlxSeqs.value, 10) || 1);
      selection.advanced.mlx_max_num_seqs = v;
      if (mlxSeqsVal) mlxSeqsVal.textContent = String(v);
      // Kontekst per sekwencja = pool_tokens / seqs — liczymy z aktualnej
      // odpowiedzi bez ponownego fetchu (pula nie zalezy od seqs).
      const out = document.getElementById('edw-mlx-readout');
      if (out) out.innerHTML = mlxReadoutHtml();
    });
  }

  // Auto/manual mode — tf-segmented emits "change" with detail.value.
  const modeSeg = document.getElementById('edw-adv-mode');
  if (modeSeg) {
    modeSeg.addEventListener('change', (e) => {
      const v = e.detail?.value || 'auto';
      if (v !== selection.advanced.mode) {
        selection.advanced.mode = v;
        refreshModal();
      }
    });
  }

  const debounceRecompute = (overrides) => {
    if (advancedRecommendDebounceTimer) clearTimeout(advancedRecommendDebounceTimer);
    advancedRecommendDebounceTimer = setTimeout(async () => {
      advancedRecommendation = await fetchVllmRecommendation(overrides);
      // Re-render tylko body kroku, BEZ niszczenia stepper'a / footera.
      const body = document.getElementById('edw-body');
      if (body) {
        body.innerHTML = renderStepIndicator() + renderStepBody();
        bindStepInputs();
      }
    }, 300);
  };

  // MLX: ten sam round-trip, ale odswiezamy WYLACZNIE readout (bez re-renderu
  // calego MLX panelu) — zeby nie gubic focusu na polu budzetu pamieci.
  const mlxDebounceRecompute = () => {
    if (advancedRecommendDebounceTimer) clearTimeout(advancedRecommendDebounceTimer);
    advancedRecommendDebounceTimer = setTimeout(async () => {
      advancedRecommendation = await fetchVllmRecommendation(buildOverrides());
      const out = document.getElementById('edw-mlx-readout');
      if (out) out.innerHTML = mlxReadoutHtml();
    }, 300);
  };

  const buildOverrides = () => {
    const a = selection.advanced;
    // MLX: single device, brak TP/PP/locka. Wysylamy budzet (przez getAdvancedGpus),
    // kv-bits (none|kv8|kv4 jako kv_cache_dtype) i max_num_seqs (cap puli).
    if (isMlxEngine()) {
      const kvLabel = mlxKvDtypeLabel();
      return {
        max_num_seqs: Math.max(1, Number(a.mlx_max_num_seqs) || 1),
        kv_cache_dtype: kvLabel !== 'none' ? kvLabel : undefined,
      };
    }
    // `lock_<param>: true` informuje backend ze user zafiksowal ten parametr —
    // pozostale (auto-fit) zostana zmniejszone zeby zmiescic sie w VRAM. Bez
    // locka backend traktuje overrides jako sugestie i moze je obnizyc.
    const lock = a.lockedParam;
    const isLcpp = isLlamaCppEngine();
    const overrides = {
      tensor_parallel: a.tensor_parallel || undefined,
      pipeline_parallel: a.pipeline_parallel || undefined,
      max_model_len: a.max_model_len || undefined,
      max_num_seqs: a.max_num_seqs || undefined,
      // llama.cpp default KV type is f16 and the engine emits no flag for it,
      // so an explicit 'f16' is equivalent to 'auto' on the wire.
      kv_cache_dtype: (a.kv_cache_dtype !== 'auto' && !(isLcpp && a.kv_cache_dtype === 'f16')) ? a.kv_cache_dtype : undefined,
      // Send a value ONLY when the user actually moved the slider. Untouched →
      // undefined → Core's pooling-aware auto picks the budget (tight cap for
      // embed/rerank, generous for LLMs). Sending the slider's resting default
      // would mask that auto and starve pooling engines on a shared GPU.
      gpu_memory_utilization: a.gpu_memory_touched ? a.gpu_memory_utilization : undefined,
      lock_max_model_len: lock === 'max_model_len' || undefined,
      lock_max_num_seqs: lock === 'max_num_seqs' || undefined,
      lock_tensor_parallel: lock === 'tensor_parallel' || undefined,
    };
    // Override kwantyzacji wag tylko dla vLLM-rodziny (safetensors). Dla
    // llama.cpp/MLX wagi wynikaja z pobranego pliku, wiec nie nadpisujemy.
    if (isVllmFamilyEngine() && a.quantization) {
      overrides.quantization_override = a.quantization;
    }
    // llama.cpp: osobny typ V cache (gdy user wybral inny niz K). vLLM nie ma
    // osobnego V, MLX idzie wlasna sciezka wyzej.
    if (isLcpp && a.kv_cache_dtype_v && a.kv_cache_dtype_v !== a.kv_cache_dtype) {
      overrides.kv_cache_dtype_v = a.kv_cache_dtype_v;
    }
    // vLLM: opcjonalny `--max-num-batched-tokens` (driver szczytu aktywacji).
    if (isVllmFamilyEngine() && a.max_num_batched_tokens) {
      overrides.max_num_batched_tokens = a.max_num_batched_tokens;
    }
    return overrides;
  };

  // Po ruchu suwaka ctx/seqs/KV pokazujemy "przeliczam…" na kafelku KV — pula
  // (kv_pool_gb) i jej pojemnosc liczy backend, wiec nie estymujemy client-side
  // (to powielalo bug seqs-mnoznika). Backend nadpisze wartosc przy odpowiedzi.
  const markKvTileEstimating = () => {
    const tiles = document.querySelectorAll('#edw-adv-kpi .adv-kpi');
    const kvTile = tiles[1];
    if (!kvTile) return;
    const valEl = kvTile.querySelector('.k-value');
    if (valEl) valEl.textContent = tAdv('kv_recomputing');
    kvTile.classList.add('estimating');
  };

  const bindRange = (id, valSpanId, key, transform, displayFn, lockable) => {
    const el = document.getElementById(id);
    const valSpan = document.getElementById(valSpanId);
    if (!el) return;
    el.addEventListener('input', () => {
      const v = transform(el.value);
      selection.advanced[key] = v;
      if (key === 'gpu_memory_utilization') selection.advanced.gpu_memory_touched = true;
      if (lockable) selection.advanced.lockedParam = lockable;
      if (valSpan) valSpan.textContent = displayFn ? displayFn(v) : v.toLocaleString();
      // Dla vLLM max_num_seqs to wylacznie cap schedulera — pula KV od niego
      // NIE zalezy, wiec ruch suwaka seqs NIE rusza kafelka KV. llama.cpp i MLX
      // licza n_ctx = max_model_len * max_num_seqs, wiec tam seqs REALNIE zmienia
      // pule i kafelek musi pokazac "przeliczam…" do round-tripu z backendem.
      if (!(key === 'max_num_seqs' && isVllmFamilyEngine())) markKvTileEstimating();
      debounceRecompute(buildOverrides());
    });
  };

  bindRange('edw-adv-ctx', 'edw-adv-ctx-val', 'max_model_len', (v) => parseInt(v, 10), (v) => v.toLocaleString(), 'max_model_len');
  bindRange('edw-adv-seqs', 'edw-adv-seqs-val', 'max_num_seqs', (v) => parseInt(v, 10), (v) => String(v), 'max_num_seqs');
  // gpu_memory_utilization nie ma osobnego locka w backendzie — jest stale
  // wejscie do auto-fit, nie parametr do dopasowania. Lockable=null.
  bindRange('edw-adv-mem', 'edw-adv-mem-val', 'gpu_memory_utilization',
    (v) => parseFloat(v),
    (v) => `${(v * 100).toFixed(0)}%`,
    null);

  // Chipy presetów kontekstu — klik ustawia suwak i wyzwala recompute.
  const applyCtxPreset = (v, chip) => {
    selection.advanced.max_model_len = v;
    selection.advanced.lockedParam = 'max_model_len';
    const slider = document.getElementById('edw-adv-ctx');
    if (slider) slider.value = String(v);
    const valSpan = document.getElementById('edw-adv-ctx-val');
    if (valSpan) valSpan.textContent = v.toLocaleString();
    document.querySelectorAll('.adv-ctx-chip[data-ctx]').forEach((c) => c.classList.remove('active'));
    if (chip) chip.classList.add('active');
    markKvTileEstimating();
    debounceRecompute(buildOverrides());
  };
  document.querySelectorAll('.adv-ctx-chip[data-ctx]').forEach((chip) => {
    chip.addEventListener('click', () => {
      if (chip.classList.contains('exceeds')) return;
      const v = parseInt(chip.dataset.ctx, 10);
      if (!Number.isFinite(v)) return;
      applyCtxPreset(v, chip);
    });
  });
  const vramMaxBtn = document.getElementById('edw-adv-ctx-vram-max');
  if (vramMaxBtn) {
    vramMaxBtn.addEventListener('click', () => {
      const v = parseInt(vramMaxBtn.dataset.ctx, 10);
      if (Number.isFinite(v) && v > 0) applyCtxPreset(v, null);
    });
  }

  // tf-input dla TP/PP (emituje "change" z detail.value).
  ['edw-adv-tp', 'edw-adv-pp'].forEach((id) => {
    const el = document.getElementById(id);
    if (!el) return;
    el.addEventListener('change', (e) => {
      const raw = e.detail?.value ?? el.value;
      const key = id === 'edw-adv-tp' ? 'tensor_parallel' : 'pipeline_parallel';
      const v = parseInt(raw, 10);
      if (Number.isFinite(v)) {
        selection.advanced[key] = v;
        // TP locka — backend dostaje lock_tensor_parallel:true zeby auto-fit
        // dopasowal ctx/seqs do wybranego TP. PP nie ma osobnego locka.
        if (key === 'tensor_parallel') selection.advanced.lockedParam = 'tensor_parallel';
        debounceRecompute(buildOverrides());
      }
    });
  });

  // tf-select dla KV dtype (K). Nie ma osobnego locka — backend traktuje
  // kv_cache_dtype jako stale wejscie, nie jako parametr auto-fit.
  const kvSelect = document.getElementById('edw-adv-kv');
  if (kvSelect) {
    kvSelect.addEventListener('change', (e) => {
      const v = e.detail?.value ?? kvSelect.value;
      selection.advanced.kv_cache_dtype = v;
      markKvTileEstimating();
      debounceRecompute(buildOverrides());
    });
  }

  // llama.cpp: osobny select V cache. Zmiana V (kwantyzowane) zmienia pule KV
  // i wymaga flash-attention (backend dodaje -fa automatycznie).
  const kvVSelect = document.getElementById('edw-adv-kv-v');
  if (kvVSelect) {
    kvVSelect.addEventListener('change', (e) => {
      const v = e.detail?.value ?? kvVSelect.value;
      selection.advanced.kv_cache_dtype_v = v;
      markKvTileEstimating();
      debounceRecompute(buildOverrides());
    });
  }

  // vLLM: opcjonalny `--max-num-batched-tokens` (driver szczytu aktywacji).
  const batchInput = document.getElementById('edw-adv-batch');
  if (batchInput) {
    batchInput.addEventListener('change', (e) => {
      const raw = e.detail?.value ?? batchInput.value;
      const v = parseInt(raw, 10);
      selection.advanced.max_num_batched_tokens = Number.isFinite(v) && v > 0 ? v : 8192;
      markKvTileEstimating();
      debounceRecompute(buildOverrides());
    });
  }

  // Quantization wag — zmiana przelicza wagi w kalkulatorze VRAM (pusta
  // wartosc = dtype ze zrodla). To pozwala dopasowac model do GPU bez
  // czekania na pre-quant — kalkulator od razu pokazuje docelowy rozmiar.
  const quantSelect = document.getElementById('edw-adv-quant');
  if (quantSelect) {
    quantSelect.addEventListener('change', (e) => {
      const v = e.detail?.value ?? quantSelect.value;
      selection.advanced.quantization = v || null;
      debounceRecompute(buildOverrides());
    });
  }

  // `--trust-remote-code` toggle — czysto przelacznik flagi CLI, nie wplywa na
  // estymacje VRAM, wiec bez recompute.
  const trustToggle = document.getElementById('edw-adv-trust');
  if (trustToggle) {
    trustToggle.addEventListener('change', (e) => {
      const v = e.detail?.checked ?? trustToggle.checked;
      selection.advanced.trust_remote_code = !!v;
    });
  }

  const extraArgsTa = document.getElementById('edw-adv-extra-args');
  if (extraArgsTa) {
    const onExtra = (e) => {
      selection.advanced.extra_args = String(e.detail?.value ?? extraArgsTa.value ?? '');
    };
    extraArgsTa.addEventListener('input', onExtra);
    extraArgsTa.addEventListener('change', onExtra);
  }

  // Speculative Decoding — toggle, model repo, method, num_tokens.
  // Recompute VRAM nie jest wywolywane bo backend recommender (auto_fit_config)
  // nie modeluje pamieci speculatora. To swiadomy trade-off — drafter to
  // mniejszy model (~10-30% targetu), zwykle miesci sie w headroomie po
  // auto-fit. Jezeli vllm padnie OOM przy starcie, user zmniejszy
  // max-model-len recznie.
  const syncSpecModelRow = () => {
    const needsModel = specMethodNeedsModel(selection.advanced.speculative.method);
    const modelRow = document.getElementById('edw-adv-spec-model-row');
    if (modelRow) modelRow.style.display = needsModel ? '' : 'none';
    const noModelHint = document.getElementById('edw-adv-spec-nomodel-hint');
    if (noModelHint) noModelHint.style.display = needsModel ? 'none' : '';
  };
  const specToggle = document.getElementById('edw-adv-spec-enabled');
  if (specToggle) {
    specToggle.addEventListener('change', (e) => {
      const v = e.detail?.checked ?? specToggle.checked;
      selection.advanced.speculative.enabled = !!v;
      // A model with a built-in MTP head needs no drafter repo, so enabling
      // speculation defaults to `mtp` unless a preset already paired a drafter.
      if (v && nativeMtpAvailable() && !selection.advanced.speculative.model) {
        selection.advanced.speculative.method = 'mtp';
        const methodSel = document.getElementById('edw-adv-spec-method');
        if (methodSel) methodSel.value = 'mtp';
        syncSpecModelRow();
      }
      const fields = document.getElementById('edw-adv-spec-fields');
      const tokensRow = document.getElementById('edw-adv-spec-tokens-row');
      const display = v ? '' : 'none';
      if (fields) fields.style.display = display;
      if (tokensRow) tokensRow.style.display = display;
    });
  }
  const specModel = document.getElementById('edw-adv-spec-model');
  if (specModel) {
    specModel.addEventListener('change', (e) => {
      selection.advanced.speculative.model = String(e.detail?.value ?? specModel.value ?? '').trim();
    });
    specModel.addEventListener('input', (e) => {
      selection.advanced.speculative.model = String(e.detail?.value ?? specModel.value ?? '').trim();
    });
  }
  const specMethod = document.getElementById('edw-adv-spec-method');
  if (specMethod) {
    specMethod.addEventListener('change', (e) => {
      selection.advanced.speculative.method = e.detail?.value ?? specMethod.value ?? 'dflash';
      syncSpecModelRow();
    });
  }
  const specNum = document.getElementById('edw-adv-spec-num');
  const specNumVal = document.getElementById('edw-adv-spec-num-val');
  if (specNum) {
    specNum.addEventListener('input', () => {
      const v = parseInt(specNum.value, 10);
      if (Number.isFinite(v)) {
        selection.advanced.speculative.num_tokens = v;
        if (specNumVal) specNumVal.textContent = String(v);
      }
    });
  }

  // "Reset to auto" — czysci wszystkie locki + manualne wartosci, backend
  // dostaje czysty /recommend (bez overrides) i zwraca pelne auto-tuning.
  const resetBtn = document.getElementById('edw-adv-reset-lock');
  if (resetBtn) {
    resetBtn.addEventListener('click', () => {
      const a = selection.advanced;
      a.lockedParam = null;
      a.tensor_parallel = null;
      a.pipeline_parallel = null;
      a.max_model_len = null;
      a.max_num_seqs = null;
      a.gpu_memory_utilization = 0.9;
      a.gpu_memory_touched = false;
      a.kv_cache_dtype = 'auto';
      // Quantization NIE jest tuningiem — to wlasciwosc presetu modelu. Reset
      // czysci tylko auto-fit, kwantyzacja zostaje (buildOverrides ja niesie).
      debounceRecompute(buildOverrides());
    });
  }

  // Initial fetch gdy jeszcze nie ma rekomendacji — z quantization_override
  // presetu (inaczej kalkulator liczy wagi w dtype zrodla, nie NVFP4).
  if (!advancedRecommendation) {
    debounceRecompute(buildOverrides());
  }
}

// ---- Step: cluster-config (multi-node tensor-parallel) --------------------

function tCluster(k, params) { return I18n.t(`wizard.cluster.${k}`, params); }

// Members × gpusPerNode = tensor-parallel world size. Members come from the
// cluster detail fetched in openDeployWizard; gpusPerNode is user-set here.
function clusterTpSize() {
  const members = selection.clusterMembers.length || 0;
  const g = Math.max(1, Number(selection.gpusPerNode) || 1);
  return members * g;
}

/// Budzet fazy P6 (zaladowanie wag -> /v1/models 200). Przekroczenie NIE jest
/// bledem modelu — Core rozbiera wtedy caly klaster, wiec wartosc musi byc do
/// ustawienia. Default skaluje sie z rozmiarem wag: pierwszy start czyta je z
/// dysku na kazdym czlonku, po czym dochodzi CUDA-graph capture i autotune.
function defaultReadyTimeoutSecs() {
  const rec = advancedRecommendation && !advancedRecommendation.error ? advancedRecommendation : null;
  const weightsGb = rec?.vram_estimate?.model_weights_gb || 0;
  // ~15 GB/min laczne czytanie wag + 15 min stalego narzutu startu.
  const scaled = Math.ceil((weightsGb / 15) * 60) + 900;
  return Math.min(14400, Math.max(1800, scaled));
}

function clusterReadyTimeoutSecs() {
  const v = Number(selection.readyTimeoutSecs);
  return Number.isFinite(v) && v >= 300 ? Math.round(v) : defaultReadyTimeoutSecs();
}

function renderStepClusterConfig() {
  const members = selection.clusterMembers.length || 0;
  // gpusPerNode is bounded by the representative node's physical GPU count.
  // Spark = 1 GPU/node → the field locks to 1; multi-GPU nodes let the user pick.
  const maxGpusPerNode = Math.max(1, nodeGpus(selection.nodeId).length);
  const gpus = Math.min(maxGpusPerNode, Math.max(1, Number(selection.gpusPerNode) || 1));
  const gpusLocked = maxGpusPerNode <= 1;
  const p = selection.pricing;

  let modelSummary = '';
  if (selection.modelRepo) {
    modelSummary = `<div><code>${escapeHtml(selection.modelRepo)}</code> <span class="form-hint inline">(HuggingFace)</span></div>`;
  } else if (selection.modelPresetId) {
    const preset = Manifest.modelPresets(engineEntry).find((pr) => pr?.id === selection.modelPresetId);
    if (preset) modelSummary = `<div><strong>${escapeHtml(preset.display_name || preset.id)}</strong>${preset.repo ? ` <span class="form-hint inline">${escapeHtml(preset.repo)}</span>` : ''}</div>`;
  }

  return `
    <h4 class="wizard-step-title">${escapeHtml(tCluster('title'))}</h4>
    <p class="form-hint" style="margin-bottom:14px;">${escapeHtml(tCluster('subtitle', { n: members }))}</p>

    ${modelSummary ? `<div class="form-group"><label>${escapeHtml(I18n.t('wizard.modelLabel'))}</label>${modelSummary}</div>` : ''}

    <div class="form-group">
      <tf-input type="number" id="edw-cluster-gpus" min="1" max="${maxGpusPerNode}"
        ${gpusLocked ? 'disabled' : ''}
        label="${escapeAttr(tCluster('gpus_per_node'))}"
        value="${escapeAttr(String(gpus))}"></tf-input>
      <div class="cluster-deploy-tp-preview" style="margin-top:6px;">
        ${escapeHtml(tCluster('tp_label'))}: <strong id="edw-cluster-tp">${clusterTpSize()}</strong>
        <span class="form-hint inline">(${members} × <span id="edw-cluster-gpus-echo">${gpus}</span>)</span>
      </div>
    </div>

    <div class="form-group">
      <tf-input type="text" id="edw-cluster-served"
        label="${escapeAttr(tCluster('served_name'))}"
        placeholder="${escapeAttr(tCluster('served_name_hint'))}"
        value="${escapeAttr(selection.servedModelName || '')}"></tf-input>
    </div>

    <div class="form-group">
      <tf-input type="number" id="edw-cluster-port" min="1" max="65535"
        label="${escapeAttr(tCluster('port'))}"
        value="${escapeAttr(String(selection.port || 8100))}"></tf-input>
    </div>

    <div class="form-group">
      <tf-input type="number" id="edw-cluster-ready-timeout" min="300" step="60"
        label="${escapeAttr(tCluster('ready_timeout'))}"
        value="${escapeAttr(String(clusterReadyTimeoutSecs()))}"></tf-input>
      <div class="form-hint">${escapeHtml(tCluster('ready_timeout_hint'))}</div>
    </div>

    <div class="form-group">
      <tf-input type="text" id="edw-cluster-vllm-args"
        label="${escapeAttr(tCluster('vllm_args'))}"
        placeholder="--max-num-seqs 6 --max-cudagraph-capture-size 36"
        value="${escapeAttr(selection.clusterVllmArgs || '')}"></tf-input>
      <div class="form-hint">${escapeHtml(tCluster('vllm_args_hint'))}</div>
    </div>

    <div class="form-group">
      <label>${escapeHtml(tCluster('pricing_title'))}</label>
      <div class="form-hint" style="margin-bottom:6px;">${escapeHtml(tCluster('pricing_hint'))}</div>
      <div style="display:grid;grid-template-columns:1fr 1fr;gap:8px;">
        <tf-input id="edw-cluster-price-prompt" type="number" min="0" step="0.0001" label="${escapeAttr(I18n.t('analytics.col_price_prompt'))}" value="${escapeAttr(p.promptPer1k == null ? '' : String(p.promptPer1k))}"></tf-input>
        <tf-input id="edw-cluster-price-completion" type="number" min="0" step="0.0001" label="${escapeAttr(I18n.t('analytics.col_price_completion'))}" value="${escapeAttr(p.completionPer1k == null ? '' : String(p.completionPer1k))}"></tf-input>
        <tf-input id="edw-cluster-price-audio" type="number" min="0" step="0.0001" label="${escapeAttr(I18n.t('analytics.col_price_audio'))}" value="${escapeAttr(p.audioPerMin == null ? '' : String(p.audioPerMin))}"></tf-input>
        <tf-input id="edw-cluster-price-image" type="number" min="0" step="0.0001" label="${escapeAttr(I18n.t('analytics.col_price_image'))}" value="${escapeAttr(p.imageEach == null ? '' : String(p.imageEach))}"></tf-input>
      </div>
    </div>
  `;
}

function bindStepClusterConfigInputs() {
  const gpusInput = document.getElementById('edw-cluster-gpus');
  if (gpusInput) {
    const maxGpusPerNode = Math.max(1, nodeGpus(selection.nodeId).length);
    const onGpus = (e) => {
      const raw = e.detail?.value ?? gpusInput.value;
      const v = Math.max(1, Math.min(maxGpusPerNode, parseInt(raw, 10) || 1));
      selection.gpusPerNode = v;
      const tpEl = document.getElementById('edw-cluster-tp');
      const echoEl = document.getElementById('edw-cluster-gpus-echo');
      if (tpEl) tpEl.textContent = String(clusterTpSize());
      if (echoEl) echoEl.textContent = String(v);
      // TP world size changed → the Advanced VRAM budget (weights = model/TP) is
      // now stale. Invalidate it so the calculator re-fetches with the new TP
      // when the user steps back into Advanced.
      advancedRecommendation = null;
    };
    gpusInput.addEventListener('input', onGpus);
    gpusInput.addEventListener('change', onGpus);
  }

  const servedInput = document.getElementById('edw-cluster-served');
  if (servedInput) {
    servedInput.addEventListener('input', (e) => {
      selection.servedModelName = String(e.detail?.value ?? servedInput.value).trim();
    });
  }

  const portInput = document.getElementById('edw-cluster-port');
  if (portInput) {
    portInput.addEventListener('input', (e) => {
      const v = parseInt(e.detail?.value ?? portInput.value, 10);
      selection.port = Number.isFinite(v) ? v : 8100;
    });
  }

  const readyInput = document.getElementById('edw-cluster-ready-timeout');
  if (readyInput) {
    readyInput.addEventListener('input', (e) => {
      const v = parseInt(e.detail?.value ?? readyInput.value, 10);
      selection.readyTimeoutSecs = Number.isFinite(v) ? v : null;
    });
  }

  const argsInput = document.getElementById('edw-cluster-vllm-args');
  if (argsInput) {
    argsInput.addEventListener('input', (e) => {
      selection.clusterVllmArgs = String(e.detail?.value ?? argsInput.value ?? '');
    });
  }

  const priceBind = (id, key) => {
    const el = document.getElementById(id);
    if (!el) return;
    el.addEventListener('input', (e) => {
      const raw = String(e.detail?.value ?? el.value ?? '').trim();
      selection.pricing[key] = raw === '' ? null : Number(raw);
    });
  };
  priceBind('edw-cluster-price-prompt', 'promptPer1k');
  priceBind('edw-cluster-price-completion', 'completionPer1k');
  priceBind('edw-cluster-price-audio', 'audioPerMin');
  priceBind('edw-cluster-price-image', 'imageEach');
}

// ---- Step 3: runtime ------------------------------------------------------

function renderStepRuntime() {
  const eng = engineEntry?.engine || {};
  const port = selection.port || eng.default_port || 8080;
  const cname = selection.containerName || '';
  const composeMode = selection.deployMethod === 'docker' && usesDockerCompose();

  let summary = '';
  if (selection.modelRepo) {
    const fileHtml = selection.modelFile
      ? `<div><code>${escapeHtml(selection.modelFile)}</code> <span class="form-hint inline">GGUF</span></div>`
      : '';
    summary = `
      <div class="form-group">
        <label>${escapeHtml(I18n.t('wizard.modelLabel'))}</label>
        <div><code>${escapeHtml(selection.modelRepo)}</code> <span class="form-hint inline">(HuggingFace)</span></div>
        ${fileHtml}
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

  // Pole portu pokazujemy dla KAZDEGO docker deploy (single-container i compose
  // stack) — host port w mapowaniu host:container jest editable. Wartosc jest
  // wstepnie wypelniana pierwszym wolnym portem ktory przydzielilby serwer
  // (patrz bindStepRuntimeInputs -> suggestServicePortRequest), z mozliwoscia
  // zmiany. Dla native (python-bundle / binary / embedded) backend zawsze sam
  // alokuje port z puli i ignoruje wartosc z formularza, wiec tam pokazujemy
  // tylko informacyjny opis.
  // Cloud API providers have no local port — the endpoint is the provider URL.
  const isCloudExternal = externalCredsConfig().requiresApiKey;
  const isDocker = selection.deployMethod === 'docker';
  const portField = isCloudExternal
    ? ''
    : isDocker
    ? `
      <div class="form-group">
        <tf-input type="number" id="edw-port"
          label="${escapeAttr(I18n.t('wizard.port'))}"
          value="${escapeAttr(String(port))}"></tf-input>
        <span class="form-hint">${escapeHtml(I18n.t('wizard.portDockerHint'))}</span>
      </div>
    `
    : `
      <div class="form-group">
        <label>${escapeHtml(I18n.t('wizard.port'))}</label>
        <div class="form-readout">${escapeHtml(I18n.t('wizard.portAutoAllocated'))}</div>
      </div>
    `;

  return `
    <h4 class="wizard-step-title">${escapeHtml(I18n.t('wizard.configureRuntime'))}</h4>
    ${summary}
    ${renderExternalCredsFields()}
    ${portField}
    ${extra}
    ${renderLaunchCommandPanel()}
  `;
}

// Podglad i edycja finalnej komendy startowej silnika. Backend (launch_dialect)
// buduje ja per-dialekt z ustawien Advanced i zwraca jako `launch_command`.
// 'auto' = readonly podglad; 'custom' = caly tekst edytowalny → leci verbatim
// jako `launch_command_override` (ENGINE_LAUNCH_CMD). Tylko docker/native dla
// silnikow LLM (komenda dostepna); cloud/external nie maja lokalnej komendy.
function renderLaunchCommandPanel() {
  if (selection.deployMethod !== 'docker' && selection.deployMethod !== 'native') return '';
  const autoCmd = previewLaunchCommand();
  const mode = selection.launchCommandMode || 'auto';
  if (!autoCmd && mode !== 'custom') return '';
  const value = mode === 'custom' ? (selection.launchCommandText || autoCmd) : autoCmd;
  return `
    <div class="form-group">
      <label>Komenda startowa silnika</label>
      <tf-segmented id="edw-launch-mode" value="${escapeAttr(mode)}" size="sm">
        <option value="auto" variant="neutral">Auto</option>
        <option value="custom" variant="accent">Własna</option>
      </tf-segmented>
      <tf-textarea id="edw-launch-cmd" rows="4" ${mode === 'auto' ? 'disabled' : ''}
        value="${escapeAttr(value)}"></tf-textarea>
      <span class="form-hint">Placeholdery <code>$MODEL</code>/<code>$PORT</code> rozwija powłoka przy starcie. W trybie „Własna" cała komenda leci verbatim.</span>
    </div>
  `;
}

/// Credential metadata for the selected external engine. `requiresApiKey` gates
/// the API-key field; `showBaseUrl`/`showApiVersion` add an endpoint override
/// for engines that need one (generic openai-compatible, Azure).
function externalCredsConfig() {
  const eng = engineEntry?.engine || {};
  const ext = engineEntry?.deploy?.external || null;
  const requiresApiKey = selection.deployMethod === 'external' && !!ext?.requires_api_key;
  const api = String(eng.api || '').toLowerCase();
  const isGeneric = String(eng.id || '').toLowerCase() === 'openai-compatible';
  // Subscription mode (OpenAI/Gemini) takes an OAuth token, not an API key —
  // base URL / api-version overrides don't apply there.
  const subscription = requiresApiKey
    && subscriptionSupportedEngine()
    && selection.externalAuthMode === 'subscription';
  return {
    requiresApiKey,
    subscription,
    showBaseUrl: requiresApiKey && !subscription && (api === 'azure-openai' || isGeneric),
    showApiVersion: requiresApiKey && !subscription && api === 'azure-openai',
    defaultBaseUrl: ext?.detection_endpoint || '',
    defaultApiVersion: '2024-10-21',
  };
}

function renderExternalCredsFields() {
  const c = externalCredsConfig();
  if (!c.requiresApiKey) return '';
  const baseUrl = c.showBaseUrl
    ? `
      <div class="form-group">
        <tf-input type="text" id="edw-base-url"
          label="${escapeAttr(I18n.t('external.base_url'))}"
          placeholder="${escapeAttr(c.defaultBaseUrl)}"
          value="${escapeAttr(selection.baseUrl || '')}"></tf-input>
        <span class="form-hint">${escapeHtml(I18n.t('external.base_url_hint'))}</span>
      </div>`
    : '';
  const apiVersion = c.showApiVersion
    ? `
      <div class="form-group">
        <tf-input type="text" id="edw-api-version"
          label="${escapeAttr(I18n.t('external.api_version'))}"
          placeholder="${escapeAttr(c.defaultApiVersion)}"
          value="${escapeAttr(selection.apiVersion || '')}"></tf-input>
      </div>`
    : '';
  const engId = String(engineEntry?.engine?.id || '').toLowerCase();
  const keyLabel = c.subscription ? I18n.t('external.subscription_token') : I18n.t('external.api_key');
  const keyPlaceholder = c.subscription
    ? I18n.t('external.subscription_token_placeholder')
    : I18n.t('external.api_key_placeholder');
  const subHintKey = engId === 'gemini' ? 'external.subscription_hint_gemini' : 'external.subscription_hint_openai';
  const keyHint = c.subscription ? I18n.t(subHintKey) : I18n.t('external.api_key_hint');
  // Subscription = browser OAuth login (no key pasting). API mode = a key field.
  if (c.subscription) {
    return `
      <div class="form-group">
        <label>${escapeHtml(keyLabel)}</label>
        ${renderSubscriptionLogin()}
        <span class="form-hint">${escapeHtml(keyHint)}</span>
      </div>
    `;
  }
  return `
    <div class="form-group">
      <tf-input type="password" id="edw-api-key"
        label="${escapeAttr(keyLabel)}"
        placeholder="${escapeAttr(keyPlaceholder)}"
        value="${escapeAttr(selection.apiKey || '')}"></tf-input>
      <span class="form-hint">${escapeHtml(keyHint)}</span>
    </div>
    ${baseUrl}
    ${apiVersion}
  `;
}

function renderSubscriptionLogin() {
  if (selection.oauthFlowId) {
    const who = selection.oauthAccount
      ? I18n.t('external.oauth_signed_in_as', { account: selection.oauthAccount })
      : I18n.t('external.oauth_signed_in');
    return `
      <div style="display:flex;align-items:center;gap:10px;">
        <tf-chip status="ok">${escapeHtml(who)}</tf-chip>
        <tf-button variant="ghost" id="edw-oauth-login">${escapeHtml(I18n.t('external.oauth_relogin'))}</tf-button>
      </div>`;
  }
  return `
    <div style="display:flex;align-items:center;gap:10px;flex-wrap:wrap;">
      <tf-button variant="primary" icon="key" id="edw-oauth-login">${escapeHtml(I18n.t('external.oauth_login'))}</tf-button>
      <span data-oauth-status class="form-hint"></span>
    </div>`;
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
  // Cloud API providers don't pick a model at deploy time — the model picker in
  // the service edit view selects from the provider's live catalog instead.
  if (externalCredsConfig().requiresApiKey) return true;
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
  // Cluster deploy allocates GPUs on every member (all GPUs per node), so the
  // per-node GPU picker does not apply.
  if (selection.isCluster) return true;
  // Cloud API providers run remotely — there is no local GPU to allocate.
  if (externalCredsConfig().requiresApiKey) return true;
  const gpus = nodeGpus(selection.nodeId);
  if (gpus.length === 0) return true;
  if (usesDockerCompose()) return true;
  const gpuSupported = engineEntry?.engine?.gpu_supported;
  if (gpuSupported === false) return true;
  return false;
}

// GPU mode actually sent to the backend. An engine whose manifest declares
// `gpu_supported = false` never shows the GPU step, so `selection.gpuSelectMode`
// keeps its 'all' default — emitting that hands a CPU-only container every card
// on the host. Such an engine is pinned to an explicit 'none' rather than
// omitting the key: 'none' is already part of the deploy contract and reads back
// unambiguously from a stored config_json, whereas a missing key means "decide
// from the manifest/host" and would depend on the manifest carrying `gpus`.
function effectiveGpuSelectMode(entry, mode) {
  if (entry?.engine?.gpu_supported === false) return 'none';
  return mode || 'all';
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

function fmtGb(mb) {
  const gb = (Number(mb) || 0) / 1024;
  return gb >= 10 ? String(Math.round(gb)) : gb.toFixed(1);
}

function nodeGpuLinks(nodeId) {
  const node = nodes.find((n) => n && (n.node_id || n.id) === nodeId);
  return Array.isArray(node?.gpu_links) ? node.gpu_links : [];
}

function vramTone(pct) {
  if (pct <= 50) return 'success';
  if (pct <= 80) return 'warning';
  return 'danger';
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
  let text = I18n.t('wizard.gpu_summary_specific', { n: chosen.length, total_vram: fmtMb(totalVram) });
  const known = chosen.filter((g) => g.vram_used_mb != null && g.vram_total_mb);
  if (known.length > 0) {
    const free = known.reduce((s, g) => s + Math.max(0, g.vram_total_mb - g.vram_used_mb), 0);
    text += ` · ${I18n.t('wizard.gpu_summary_free', { free_vram: fmtMb(free) })}`;
  }
  return text;
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

  const gpuLinks = nodeGpuLinks(selection.nodeId);
  const topology = computeGpuGroups(gpus.length, gpuLinks);

  const rows = gpus.map((g, idx) => {
    const meta = [
      g.pci_bus_id ? escapeHtml(shortPciBusId(g.pci_bus_id)) : '',
      pcieLinkHtml(g),
      g.temperature_c != null ? escapeHtml(`${Math.round(g.temperature_c)}°C`) : '',
      g.usage_percent != null ? escapeHtml(`util ${Math.round(g.usage_percent)}%`) : '',
    ].filter(Boolean);
    const metaHtml = meta.map((m, i) => i < meta.length - 1 ? `<span>${m}</span><span class="sep">·</span>` : `<span>${m}</span>`).join(' ');
    const total = Number(g.vram_total_mb) || 0;
    let vramHtml;
    if (total > 0 && g.vram_used_mb != null) {
      const used = Math.min(total, Math.max(0, Number(g.vram_used_mb) || 0));
      const pct = Math.round((used / total) * 100);
      const text = I18n.t('wizard.gpu_vram_line', { used: fmtGb(used), total: fmtGb(total), free: fmtGb(total - used) });
      vramHtml = `<tf-progress-bar size="sm" value="${pct}" tone="${vramTone(pct)}"></tf-progress-bar><span class="gpu-vram-text">${escapeHtml(text)}</span>`;
    } else {
      vramHtml = `<span class="gpu-vram-text">${escapeHtml(`${fmtMb(total)} VRAM`)}</span>`;
    }
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
          <div class="gpu-line">
            <div class="gpu-name"><span class="gpu-idx">GPU ${idx} ·</span> ${escapeHtml(String(g.name || ''))}</div>
            <div class="gpu-meta">${metaHtml}</div>
          </div>
          <div class="gpu-vram">${vramHtml}</div>
        </div>
        ${gpuPairChipHtml(idx, topology)}
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
      ${topology.hasLinks ? gpuTopologyLegendHtml() : ''}
      <div class="gpu-topo-hint-slot">${selectionLinkHintHtml(gpuLinks, selection.gpuIds)}</div>
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
    const hintSlot = document.querySelector('.gpu-list .gpu-topo-hint-slot');
    if (hintSlot) hintSlot.innerHTML = selectionLinkHintHtml(nodeGpuLinks(selection.nodeId), selection.gpuIds);
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
    case 'cluster-config': bindStepClusterConfigInputs(); break;
    case 'runtime':  bindStepRuntimeInputs(); break;
  }
}

function bindStepMethodInputs() {
  document.querySelectorAll('.deploy-method-card[data-method]').forEach((btn) => {
    btn.addEventListener('click', () => {
      selection.deployMethod = btn.dataset.method;
      if (btn.dataset.auth) selection.externalAuthMode = btn.dataset.auth;
      refreshModal();
    });
  });
  const nodeSel = document.getElementById('edw-node-select');
  if (nodeSel) {
    nodeSel.addEventListener('change', (e) => {
      selection.nodeId = e.detail?.value ?? nodeSel.value;
      hostOs = pickHostOs(selection.nodeId);
      availableMethods = Manifest.availableDeployMethods(engineEntry, hostOs, pickHostCaps(selection.nodeId));
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
      selection.modelFile = null;
      const preset = Manifest.modelPresets(engineEntry).find((p) => p?.id === selection.modelPresetId);
      if (preset) applySpeculatorPreset(preset);
      // Wariant kwantyzacji: domyslny (dopasowany do quantization presetu). Repo
      // wariantu ida jako `model_repo`, ktory w backendzie wygrywa nad preset.repo
      // (resolve_model_repo) — bez wariantow dv=null → modelRepo=null (repo presetu).
      const dv = preset ? defaultQuantVariant(preset) : null;
      selection.quantVariant = dv ? dv.quantization : null;
      selection.modelRepo = dv ? dv.repo : null;
      if (dv) selection.advanced.quantization = dv.quantization;
      // Zmiana modelu uniewaznia estymacje VRAM — wymus ponowny /recommend.
      advancedRecommendation = null;
      cachedModelSpec = null;
      // Re-render, zeby pokazac dropdown wariantow dla wybranego presetu.
      refreshModal();
    });
  });

  const bundleUrl = document.getElementById('edw-bundle-url');
  if (bundleUrl) {
    bundleUrl.addEventListener('input', (e) => {
      selection.visionBundleUrl = String(e.detail?.value ?? bundleUrl.value).trim();
    });
  }
  const bundleKey = document.getElementById('edw-bundle-api-key');
  if (bundleKey) {
    bundleKey.addEventListener('input', (e) => {
      selection.visionBundleApiKey = String(e.detail?.value ?? bundleKey.value).trim();
    });
  }
  const bundlePreview = document.getElementById('edw-bundle-preview');
  if (bundlePreview) bundlePreview.addEventListener('click', previewCustomManifest);
  const previewBox = document.getElementById('edw-bundle-preview-result');
  if (previewBox) renderBundlePreview(previewBox);
  const quantVariantSel = document.getElementById('edw-quant-variant');
  if (quantVariantSel) {
    quantVariantSel.addEventListener('change', (e) => {
      const q = String(e.detail?.value ?? quantVariantSel.value ?? '').toLowerCase();
      const preset = Manifest.modelPresets(engineEntry).find((p) => p?.id === selection.modelPresetId);
      const v = presetQuantVariants(preset).find((x) => (x.quantization || '').toLowerCase() === q);
      if (!v) return;
      selection.quantVariant = v.quantization;
      selection.modelRepo = v.repo;
      selection.advanced.quantization = v.quantization;
      advancedRecommendation = null;
      cachedModelSpec = null;
      refreshModal();
    });
  }

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
      selection.modelFile = null;
      document.querySelectorAll('.model-item[data-repo]').forEach((x) => x.classList.remove('selected'));
      it.classList.add('selected');
      // Free-form HF model nie ma sparowanego speculatora w manifescie —
      // reset, niech user wpisze recznie jak chce.
      applySpeculatorPreset(null);
      advancedRecommendation = null;
      cachedModelSpec = null;
      if (isLlamaCppEngine()) {
        loadHfGgufFiles(selection.modelRepo);
      }
    });
  });
  bindGgufFileClicks();
}

function bindGgufFileClicks() {
  document.querySelectorAll('.model-item[data-gguf-file]').forEach((it) => {
    it.addEventListener('click', () => {
      selection.modelFile = it.dataset.ggufFile;
      const quant = detectGgufQuantization(selection.modelFile);
      selection.advanced.quantization = quant || null;
      document.querySelectorAll('.model-item[data-gguf-file]').forEach((x) => x.classList.remove('selected'));
      it.classList.add('selected');
      advancedRecommendation = null;
      cachedModelSpec = null;
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
      // Mark as user-chosen so the async suggestion below never overwrites it.
      selection.portUserEdited = true;
    });
  }
  // Pre-fill the (editable) port with the first free host port the server would
  // assign — instead of the static manifest default. Docker only (native
  // ignores the port). Fire-and-forget; updates the field when it returns.
  if (selection.deployMethod === 'docker' && !selection.portUserEdited) {
    ApiBinary.action('suggestServicePortRequest', {
      deploy_method: selection.deployMethod,
    })
      .then((r) => {
        if (r && r.available && r.port && !selection.portUserEdited) {
          selection.port = r.port;
          const pin = document.getElementById('edw-port');
          if (pin) pin.value = String(r.port);
        }
      })
      .catch(() => {
        /* suggestion is advisory; the deploy re-allocates authoritatively */
      });
  }
  const cnameInput = document.getElementById('edw-cname');
  if (cnameInput) {
    cnameInput.addEventListener('input', (e) => {
      const raw = e.detail?.value ?? cnameInput.value;
      selection.containerName = String(raw).trim();
    });
  }
  const apiKeyInput = document.getElementById('edw-api-key');
  if (apiKeyInput) {
    apiKeyInput.addEventListener('input', (e) => {
      selection.apiKey = String(e.detail?.value ?? apiKeyInput.value);
    });
  }
  const baseUrlInput = document.getElementById('edw-base-url');
  if (baseUrlInput) {
    baseUrlInput.addEventListener('input', (e) => {
      selection.baseUrl = String(e.detail?.value ?? baseUrlInput.value).trim();
    });
  }
  const apiVersionInput = document.getElementById('edw-api-version');
  if (apiVersionInput) {
    apiVersionInput.addEventListener('input', (e) => {
      selection.apiVersion = String(e.detail?.value ?? apiVersionInput.value).trim();
    });
  }
  document.getElementById('edw-oauth-login')?.addEventListener('click', startOauthLogin);

  // Komenda startowa: Auto/Własna + edytowalny tekst. Przelaczamy disabled
  // i prefill bez pelnego re-renderu kroku (lokalna manipulacja DOM).
  const launchSeg = document.getElementById('edw-launch-mode');
  launchSeg?.addEventListener('change', (e) => {
    const mode = e.detail?.value === 'custom' ? 'custom' : 'auto';
    selection.launchCommandMode = mode;
    const ta = document.getElementById('edw-launch-cmd');
    const autoCmd = previewLaunchCommand();
    if (mode === 'custom') {
      if (!selection.launchCommandText) selection.launchCommandText = autoCmd;
      if (ta) { ta.removeAttribute('disabled'); ta.value = selection.launchCommandText; }
    } else if (ta) {
      ta.setAttribute('disabled', '');
      ta.value = autoCmd;
    }
  });
  const launchTa = document.getElementById('edw-launch-cmd');
  launchTa?.addEventListener('input', (e) => {
    selection.launchCommandText = String(e.detail?.value ?? launchTa.value);
  });
}

// Subscription browser-OAuth: ask the node to start a login, open the provider's
// authorize page, then poll until the node captures the tokens.
async function startOauthLogin() {
  const btn = document.getElementById('edw-oauth-login');
  const statusEl = document.querySelector('[data-oauth-status]');
  if (btn) btn.setAttribute('disabled', '');
  if (statusEl) statusEl.textContent = I18n.t('external.oauth_opening');
  try {
    const res = await ApiBinary.action('serviceOauthStartRequest', {
      provider: engineEntry?.engine?.id || '',
      nodeId: selection.nodeId,
    });
    if (!res || res.error || !res.authorizeUrl) {
      throw new Error((res && res.error) || 'no authorize URL');
    }
    const url = res.authorizeUrl;
    const code = res.userCode || '';
    try { window.open(url, '_blank', 'noopener'); } catch (_e) { /* popup blocked — link shown below */ }
    if (statusEl) {
      statusEl.innerHTML = `${escapeHtml(I18n.t('external.oauth_enter_code'))}
        <a href="${escapeAttr(url)}" target="_blank" rel="noopener">${escapeHtml(url)}</a>
        → <strong style="font-size:1.15em;letter-spacing:2px">${escapeHtml(code)}</strong>
        <br><span class="form-hint">${escapeHtml(I18n.t('external.oauth_waiting'))}</span>`;
    }
    pollOauth(res.flowId);
  } catch (e) {
    if (statusEl) statusEl.textContent = I18n.t('external.oauth_failed', { error: e.message || String(e) });
    if (btn) btn.removeAttribute('disabled');
  }
}

function pollOauth(flowId) {
  const tick = async () => {
    // Abort if the wizard moved on / closed.
    if (!document.getElementById('engine-deploy-wizard')) return;
    try {
      const res = await ApiBinary.action('serviceOauthPollRequest', { flowId, nodeId: selection.nodeId });
      if (res && res.status === 'done') {
        selection.oauthFlowId = flowId;
        selection.oauthAccount = res.accountLabel || '';
        refreshModal();
        return;
      }
      if (res && res.status === 'error') {
        const statusEl = document.querySelector('[data-oauth-status]');
        if (statusEl) statusEl.textContent = I18n.t('external.oauth_failed', { error: res.error || '' });
        document.getElementById('edw-oauth-login')?.removeAttribute('disabled');
        return;
      }
      setTimeout(tick, 2000);
    } catch (e) {
      const statusEl = document.querySelector('[data-oauth-status]');
      if (statusEl) statusEl.textContent = I18n.t('external.oauth_failed', { error: e.message || String(e) });
      document.getElementById('edw-oauth-login')?.removeAttribute('disabled');
    }
  };
  setTimeout(tick, 2000);
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
      if (isCameraCvEngine() && modelSourceMode === 'custom') {
        if (!selection.visionBundleUrl.includes('/models/manifest/')) {
          toast(I18n.t('wizard.customBundleUrlInvalid'), 'error');
          return false;
        }
        return true;
      }
      if (!selection.modelPresetId && !selection.modelRepo) {
        toast(I18n.t('wizard.selectModel'), 'error');
        return false;
      }
      if (isLlamaCppEngine() && selection.modelRepo && !selection.modelFile) {
        toast(I18n.t('wizard.selectGgufFile') || 'Choose a GGUF file to download.', 'error');
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
    if (isLlamaCppEngine()) {
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
  updateHfGgufFiles();
}

async function loadHfGgufFiles(repo) {
  if (!repo) return;
  hfGgufFiles = [];
  hfGgufFilesRepo = repo;
  hfGgufFilesError = '';
  hfGgufFilesLoading = true;
  updateHfGgufFiles();
  try {
    const url = `https://huggingface.co/api/models/${encodeHfRepo(repo)}/tree/main?recursive=true`;
    const resp = await fetch(url);
    if (!resp.ok) throw new Error(`HF API ${resp.status}`);
    let data = await resp.json();
    if (!Array.isArray(data)) data = [];
    hfGgufFiles = data
      .map((entry) => ({
        path: entry.rfilename || entry.path || '',
        size: entry.size || 0,
        type: entry.type || 'file',
      }))
      .filter((entry) => entry.type === 'file' && /\.gguf$/i.test(entry.path))
      .sort((a, b) => a.path.localeCompare(b.path));
  } catch (err) {
    console.error('[wizard] HF GGUF files error:', err);
    hfGgufFiles = [];
    hfGgufFilesError = I18n.t('wizard.ggufFilesError') || 'Could not load GGUF files for this repository.';
  } finally {
    hfGgufFilesLoading = false;
    updateHfGgufFiles();
  }
}

function encodeHfRepo(repo) {
  return String(repo || '').split('/').map((part) => encodeURIComponent(part)).join('/');
}

function updateHfGgufFiles() {
  const box = document.getElementById('edw-hf-gguf-files');
  if (!box) return;
  box.innerHTML = renderHfGgufFilesHtml();
  bindGgufFileClicks();
}

// ---- CLI args helpers ------------------------------------------------------

// Shell-like split honouring single/double quotes, so a value such as
// `--speculative-config '{"method":"mtp"}'` stays one token (quotes stripped).
function tokenizeCliArgs(str) {
  const out = [];
  let cur = '';
  let inTok = false;
  let quote = null;
  for (let i = 0; i < str.length; i += 1) {
    const ch = str[i];
    if (quote) {
      if (ch === quote) quote = null;
      else if (ch === '\\' && quote === '"' && i + 1 < str.length) { cur += str[i + 1]; i += 1; }
      else cur += ch;
    } else if (ch === "'" || ch === '"') {
      quote = ch;
      inTok = true;
    } else if (/\s/.test(ch)) {
      if (inTok) { out.push(cur); cur = ''; inTok = false; }
    } else if (ch === '\\' && i + 1 < str.length) {
      cur += str[i + 1]; i += 1; inTok = true;
    } else {
      cur += ch;
      inTok = true;
    }
  }
  if (inTok) out.push(cur);
  return out;
}

// Single-quote a token when it needs it; JSON never contains `'`, and the
// backend splits with shlex (native) / xargs (docker), both of which honour it.
function quoteCliArg(tok) {
  if (tok !== '' && !/[\s'"\\$`|&;<>(){}*?]/.test(tok)) return tok;
  return `'${tok.replace(/'/g, "'\\''")}'`;
}

// Apply `overrides` (`{ flag, value }` for valued flags, `{ flag, present }`
// for boolean flags) to a tokenized argv: an existing occurrence — either
// `--flag value` or `--flag=value` — is rewritten in place, otherwise the flag
// is appended. `value: null` / `present: false` remove the flag; `value:
// undefined` leaves whatever the base argv carries untouched (an "unset" user
// choice must not strip a recipe-provided flag).
function mergeCliArgs(baseArgv, overrides) {
  const argv = baseArgv.slice();
  for (const ov of overrides) {
    const isBool = ov.present !== undefined;
    if (!isBool && ov.value === undefined) continue;
    const keep = isBool ? ov.present : ov.value !== null;
    const repl = keep ? (isBool ? [ov.flag] : [ov.flag, String(ov.value)]) : [];
    let replaced = false;
    for (let i = 0; i < argv.length;) {
      const t = argv[i];
      if (t !== ov.flag && !t.startsWith(`${ov.flag}=`)) { i += 1; continue; }
      const span = (isBool || t.includes('=')) ? 1 : 2;
      // First occurrence is rewritten in place; later duplicates are dropped.
      const ins = replaced ? [] : repl;
      argv.splice(i, span, ...ins);
      replaced = true;
      i += ins.length;
    }
    if (!replaced && keep) argv.push(...repl);
  }
  return argv;
}

// Final `vllm_args` for a single-node deploy, shared by the payload and the
// launch-command preview. Auto mode forwards the calculator's recipe flags;
// manual mode starts from the same string and overrides only the user knobs, so
// recipe-only flags (tool parsers, reasoning parser, mm encoder mode) survive.
function buildVllmArgs() {
  const rec = advancedRecommendation;
  if (shouldSkipAdvancedStep() || !rec || rec.error) return null;
  const a = selection.advanced;
  const recArgs = (rec.recommended_vllm_args || '').trim();
  // llama.cpp: the backend already returned llama-server flags after the
  // round-trip with the manual overrides — use them verbatim in both modes.
  if (isLlamaCppEngine()) return recArgs || null;

  let argv = tokenizeCliArgs(recArgs);
  if (a.mode === 'auto') {
    // The calculator's `--gpu-memory-utilization` would be taken as "user
    // explicit" by the backend and defeat its free-VRAM clamp; in auto mode the
    // user never chose it, so it is stripped and sent as a separate hint.
    argv = mergeCliArgs(argv, [
      { flag: '--gpu-memory-utilization', value: null },
      { flag: '--trust-remote-code', present: a.trust_remote_code !== false },
    ]);
  } else {
    const r = rec.recommended || {};
    const tp = a.tensor_parallel ?? r.tensor_parallel ?? 1;
    const pp = a.pipeline_parallel ?? r.pipeline_parallel ?? 1;
    const kv = a.kv_cache_dtype || r.kv_cache_dtype || 'auto';
    argv = mergeCliArgs(argv, [
      { flag: '--gpu-memory-utilization', value: String(a.gpu_memory_utilization ?? r.gpu_memory_utilization ?? 0.9) },
      { flag: '--max-model-len', value: String(a.max_model_len ?? r.max_model_len ?? 8192) },
      { flag: '--max-num-seqs', value: String(a.max_num_seqs ?? r.max_num_seqs ?? 16) },
      { flag: '--max-num-batched-tokens', value: String(a.max_num_batched_tokens || Math.max(a.max_model_len ?? 8192, 8192)) },
      { flag: '--tensor-parallel-size', value: tp > 1 ? String(tp) : null },
      { flag: '--pipeline-parallel-size', value: pp > 1 ? String(pp) : null },
      // vLLM accepts only the fp8 family here; 'auto' / empty quantization are
      // "unset" choices, so the recipe's own flag (if any) is left as is.
      { flag: '--kv-cache-dtype', value: kv !== 'auto' ? kv : undefined },
      { flag: '--quantization', value: a.quantization ? String(a.quantization) : undefined },
      { flag: '--trust-remote-code', present: a.trust_remote_code !== false },
    ]);
  }

  if (isVllmFamilyEngine()) {
    const sp = a.speculative;
    if (sp && sp.enabled && (!specMethodNeedsModel(sp.method) || sp.model)) {
      const cfg = { method: sp.method || 'dflash', num_speculative_tokens: sp.num_tokens || 8 };
      if (specMethodNeedsModel(sp.method)) cfg.model = sp.model;
      argv = mergeCliArgs(argv, [{ flag: '--speculative-config', value: JSON.stringify(cfg) }]);
    }
    // User free-text goes last: the backend dedups last-wins.
    argv = argv.concat(tokenizeCliArgs(a.extra_args || ''));
  }
  return argv.length ? argv.map(quoteCliArg).join(' ') : null;
}

// Launch-command preview for auto mode: the backend builds `launch_command` as
// `<base> <recommended_vllm_args>`, so swap that tail for the merged args the
// deploy will actually send.
function previewLaunchCommand() {
  const rec = advancedRecommendation;
  if (!rec || rec.error || !rec.launch_command) return '';
  const cmd = String(rec.launch_command);
  const recArgs = (rec.recommended_vllm_args || '').trim();
  const merged = buildVllmArgs();
  if (!recArgs || !cmd.endsWith(recArgs)) return cmd;
  const base = cmd.slice(0, cmd.length - recArgs.length).trimEnd();
  return merged ? `${base} ${merged}` : base;
}

// ---- Deploy ---------------------------------------------------------------

async function startDeploy() {
  if (deployInFlight) return;
  if (selection.isCluster) {
    await startClusterDeploy();
    return;
  }

  const btn = document.getElementById('edw-deploy');

  // External cloud providers: subscription needs a completed OAuth login; API
  // mode needs a key. Neither may deploy empty (the provider would reject calls).
  const creds = externalCredsConfig();
  if (creds.requiresApiKey) {
    if (creds.subscription && !selection.oauthFlowId) {
      toast(I18n.t('external.oauth_required'), 'error');
      return;
    }
    if (!creds.subscription && !selection.apiKey.trim()) {
      toast(I18n.t('external.api_key_required'), 'error');
      return;
    }
  }

  if (btn) btn.setAttribute('disabled', '');
  deployInFlight = true;

  const eng = engineEntry.engine || {};
  const vllmArgs = buildVllmArgs();

  // Suwak gpu_memory_utilization w panelu Advanced jest wspolny dla obu trybow
  // (auto/manual) — w auto mode `vllmArgs` bierzemy z `recommended_vllm_args`
  // ale nie zawiera on tego co user faktycznie ustawil na suwaku. Wysylamy
  // wartosc jako osobne pole, zeby backend zaklemowal ja przeciwko aktualnie
  // wolnemu VRAM (`min(user, auto_safe)`) niezaleznie od trybu i wpisu w
  // vllm_args. Jezeli Advanced step nie jest aktywny dla tego silnika,
  // `selection.advanced.gpu_memory_utilization` nadal istnieje (default state),
  // ale wtedy `shouldSkipAdvancedStep()` jest prawdziwe i wartosc nie ma
  // znaczenia — wysylamy mimo to bo to tylko hint, backend zdecyduje.
  const advActive = !shouldSkipAdvancedStep();
  // MLX: budzet pamieci + max kontekst ida do config_json.parameters{} po
  // kluczach manifestowych [[parameter]] (mlx_field) — apply_parameters_deploy
  // je persistuje, a runtime guard egzekwuje. NIE uzywamy vllm_args dla MLX.
  let mlxParameters = null;
  if (advActive && isMlxEngine()) {
    // Max kontekst per sekwencja liczy backend (pool_tokens / seqs) — patrz
    // mlxMaxContextFromBackend. 0 = sentinel "natywny kontekst / bez capa".
    mlxParameters = {
      memory_budget_mb: Number(selection.advanced.mlx_max_memory_mb) || 0,
      max_context_tokens: mlxMaxContextFromBackend() || 0,
    };
  }
  // Generic manifest params (ds4 etc.): emit the full key→value map (manifest
  // default for any control the user didn't touch) so apply_parameters_deploy
  // resolves each binding deterministically.
  let genericParameters = null;
  if (hasGenericParams()) {
    genericParameters = {};
    manifestParams().forEach((p) => { genericParameters[p.key] = genericParamValue(p); });
  }
  const gpuSelectMode = effectiveGpuSelectMode(engineEntry, selection.gpuSelectMode);
  const configJson = JSON.stringify({
    parameters: genericParameters || mlxParameters,
    model_preset_id: selection.modelPresetId || null,
    model_repo: selection.modelRepo || null,
    model_file: selection.modelFile || null,
    // Native (python-bundle / binary / embedded) zawsze dostaje port z
    // PortAllocatora — wartosc z formularza jest ignorowana po stronie
    // backendu, wiec wysylamy null. Docker honoruje user-provided host port:
    // single-container mapuje go bezposrednio, compose przekazuje jako
    // MILVUS_GRPC_PORT (a gdy zajety, backend bierze nastepny wolny).
    port: selection.deployMethod === 'docker'
      ? (selection.port || eng.default_port)
      : null,
    container_name: selection.containerName || null,
    gpu_select_mode: gpuSelectMode,
    gpu_ids: gpuSelectMode === 'specific' ? selection.gpuIds : null,
    // gpu_memory_utilization wysylamy gdy user wybral manual albo poruszyl
    // suwakiem w auto. Nietykany default 0.9 zostaje po stronie backendu jako
    // auto-clamp z aktualnego free VRAM.
    gpu_memory_utilization: (advActive && (selection.advanced.mode === 'manual' || selection.advanced.gpu_memory_touched))
      ? selection.advanced.gpu_memory_utilization
      : null,
    vllm_args: vllmArgs,
    // Edytowalna komenda startowa (tryb „Własna"): caly tekst leci verbatim do
    // backendu jako ENGINE_LAUNCH_CMD (docker entrypoint / native spawn przez
    // `sh -c`), z pominieciem budowanych argow. Auto → undefined (backend
    // buduje komende sam z vllm_args/dialektu).
    launch_command_override: (selection.launchCommandMode === 'custom'
      && selection.launchCommandText && selection.launchCommandText.trim())
      ? selection.launchCommandText.trim()
      : undefined,
    // Engine env from the matched vLLM recipe (e.g. VLLM_USE_FLASHINFER_MOE_FP4
    // on Blackwell). Backend (`apply_engine_env`) injects these into the engine
    // process env on both native and docker paths. Empty/missing = nothing extra.
    engine_env: (advActive && advancedRecommendation
      && advancedRecommendation.recommended_env
      && Object.keys(advancedRecommendation.recommended_env).length)
      ? advancedRecommendation.recommended_env
      : undefined,
    // External cloud provider credentials. `api_key` is encrypted server-side
    // (never persisted in clear). `base_url`/`api_version` override the
    // manifest endpoint for generic openai-compatible / Azure engines.
    api_key: (creds.requiresApiKey && !creds.subscription) ? selection.apiKey.trim() : undefined,
    // Custom camera-CV bundle source (model step "Custom" tab): manifest URL
    // of another TentaFlow instance + Bearer key. The key is encrypted
    // server-side like `api_key` before it lands in config_json.
    vision_bundle_url: (isCameraCvEngine() && modelSourceMode === 'custom' && selection.visionBundleUrl)
      ? selection.visionBundleUrl : undefined,
    vision_bundle_api_key: (isCameraCvEngine() && modelSourceMode === 'custom' && selection.visionBundleApiKey)
      ? selection.visionBundleApiKey : undefined,
    // Subscription: the node swaps this flow id for the captured OAuth tokens.
    oauth_flow_id: (creds.requiresApiKey && creds.subscription) ? selection.oauthFlowId : undefined,
    base_url: (creds.showBaseUrl && selection.baseUrl) ? selection.baseUrl : undefined,
    api_version: (creds.showApiVersion && selection.apiVersion) ? selection.apiVersion : undefined,
    auth_mode: creds.requiresApiKey ? (creds.subscription ? 'subscription' : 'api') : undefined,
  });

  console.log('[wizard][startDeploy] payload:', {
    engineId: eng.id,
    deployMethod: selection.deployMethod,
    nodeId: selection.nodeId,
    configJson,
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
      nodeId: selection.nodeId,
    });
  } catch (err) {
    deployInFlight = false;
    toast(I18n.t('wizard.deployFailed').replace('{error}', err.message || err), 'error');
    if (btn) btn.removeAttribute('disabled');
  }
}

// Cluster deploy: one blocking `clusterDeployRequest` (tensor-parallel across
// every member). max_model_len + gpu_memory_utilization come from the shared
// Advanced step (no duplicate controls); the backend computes the real TP size
// from members × gpusPerNode. On completion the wizard closes and its onClose
// callback refreshes the cluster page.
async function startClusterDeploy() {
  const btn = document.getElementById('edw-deploy');
  const eng = engineEntry.engine || {};

  const modelRepo = getAdvancedModelName();
  if (!modelRepo) {
    toast(I18n.t('wizard.selectModel'), 'error');
    return;
  }

  const p = selection.pricing;
  if ([p.promptPer1k, p.completionPer1k, p.audioPerMin, p.imageEach].some(
    (v) => v != null && (!Number.isFinite(v) || v < 0),
  )) {
    toast(I18n.t('wizard.cluster.pricing_invalid'), 'error');
    return;
  }

  const adv = selection.advanced;
  const rec = advancedRecommendation && !advancedRecommendation.error ? advancedRecommendation : null;
  const maxModelLen = adv.max_model_len || (rec && rec.recommended && rec.recommended.max_model_len) || 8192;
  const gpuMem = adv.gpu_memory_utilization ?? 0.5;

  if (btn) btn.setAttribute('disabled', '');
  const readyTimeoutSecs = clusterReadyTimeoutSecs();
  // `config_json` niesie `vllm_args` — backend dokleja je PO argumentach silnika,
  // wiec argparse pozwala nimi nadpisac kazdy zaszyty domysl bez przebudowy.
  // Protokol mial to pole od poczatku, ale sciezka klastrowa go nie wysylala,
  // przez co jedyna droga do zmiany np. `--max-num-seqs` byl rebuild.
  const clusterArgs = String(selection.clusterVllmArgs || '').trim();
  const configJson = clusterArgs ? JSON.stringify({ vllm_args: clusterArgs }) : null;
  try {
    const resp = await ApiBinary.action(
      'clusterDeployRequest',
      {
        clusterId: selection.clusterId,
        configJson,
        engineId: eng.id,
        modelRepo,
        modelPresetId: selection.modelPresetId || null,
        servedModelName: (selection.servedModelName && selection.servedModelName.trim()) || null,
        gpusPerNode: Math.max(1, Number(selection.gpusPerNode) || 1),
        gpuMemoryUtilization: gpuMem,
        maxModelLen,
        port: selection.port || 8100,
        readyTimeoutSecs,
        promptPer1k: p.promptPer1k ?? null,
        completionPer1k: p.completionPer1k ?? null,
        audioPerMin: p.audioPerMin ?? null,
        imageEach: p.imageEach ?? null,
      },
      { timeoutMs: readyTimeoutSecs * 1000 + 30000 },
    );
    if (resp && resp.ok) {
      toast(I18n.t('cluster_detail.deploy_ok'), 'success');
      // Deploy klastra leci teraz w tle — zamknij wizard i pokaż ten sam live
      // progress modal co node deploy. Subskrybuje deploymentLogStreamRequest
      // keyed by deployment_cluster_id (fazy P0-P6 + serve log).
      close();
      const depId = resp.deploymentClusterId || '';
      if (depId) {
        const mod = await import('/js/modules/catalog/deploy-progress-modal.js');
        mod.openDeployProgressModal({
          deployId: depId,
          engineId: eng.id,
          deployMethod: 'cluster',
        });
      }
    } else {
      const msg = String(resp?.message || '');
      toast(/rdma/i.test(msg) ? I18n.t('cluster_detail.deploy_rdma_required') : (msg || I18n.t('cluster_detail.deploy_failed')), 'error');
      if (btn) btn.removeAttribute('disabled');
    }
  } catch (err) {
    toast(I18n.t('wizard.deployFailed').replace('{error}', err.message || err), 'error');
    if (btn) btn.removeAttribute('disabled');
  }
}

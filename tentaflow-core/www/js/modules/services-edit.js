// =============================================================================
// File: modules/services-edit.js — Edit Service modal (CBOR binary)
// Otwierany przyciskiem ✏️ w wierszu Services. Zmiany wysyłane przez
// ApiBinary.action('serviceUpdateRequest', ...). VRAM hint pollowany co 2s
// (ApiBinary.action('serviceVramHintRequest', {gpu_index: 0})) — pasek
// pokazuje co już zajmuje GPU + podpowiada gpu_memory_utilization
// uwzględniając desktop reserve.
//
// Layout zgodny z designem: services-edit-modal-20260508/index.html
// (Stan B). Reużywa stylów z deploy-wizard (.modal-shell, .opt-card,
// .preset-card, .adv-section, .adv-vram-bar etc.) — frontend musi mieć
// te style w services.css albo deploy-wizard.css załadowane.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import * as ManifestStore from '/js/modules/catalog/manifest-store.js';

// True when the engine is served by CUDA vLLM (manifest backend="vllm", not the
// Metal variant) — single source of truth shared with the deploy wizard and the
// Rust `Engine::is_cuda_vllm`, replacing the old `engineId === 'vllm'` literal so
// every vLLM-backed engine (embed / rerank / VL) also gets the VRAM calculator.
function engineUsesCudaVllm(engineId) {
  return ManifestStore.byId(engineId)?.engine?.backend === 'vllm'
    && !String(engineId || '').endsWith('-metal');
}

let currentModalEl = null;
let vramPollHandle = null;
let recommendDebounceHandle = null;

export async function openEditModal(svc, opts = {}) {
  closeModal();
  // External cloud providers don't expose a model/VRAM config — editing them
  // means picking which of the provider's live models become available.
  if ((svc.deploy_method || '') === 'external') {
    return openExternalModelPicker(svc, opts);
  }
  const onSaved = opts.onSaved || (() => {});
  const engineId = opts.engineId || svc.engine_id;
  const cfg = parseConfig(svc.config_json);
  const initialPresetId = cfg.model_preset_id || null;
  const initialModelRepo = cfg.model_repo || '';
  // Load the manifest (idempotent) so the vLLM-backed check below sees the
  // `backend` field for embed/rerank/VL engines, not just literal "vllm".
  await ManifestStore.init();
  const isVllm = engineUsesCudaVllm(engineId);

  // Fetch presetow z manifestu (CBOR binary) ZAMIAST hardcoded list. Backend
  // zwraca dokladnie te [[model_preset]] ktore sa w pliku TOML silnika.
  let presets = [];
  try {
    const res = await ApiBinary.action('serviceEnginePresetsRequest', { engineId });
    presets = (res && res.presets) || [];
  } catch (_e) {
    presets = [];
  }

  const overlay = document.createElement('div');
  overlay.className = 'modal-overlay';
  overlay.innerHTML = renderShell(svc, engineId, cfg, isVllm, initialPresetId, initialModelRepo, presets);
  document.body.appendChild(overlay);
  currentModalEl = overlay;

  // Close handlers
  overlay.querySelector('[data-close]').addEventListener('click', closeModal);
  overlay.querySelector('[data-cancel]').addEventListener('click', closeModal);
  overlay.addEventListener('click', (e) => {
    if (e.target === overlay) closeModal();
  });

  // Mode toggle (Preset / Custom)
  overlay.querySelectorAll('.opt-card').forEach((card) => {
    card.addEventListener('click', () => {
      overlay.querySelectorAll('.opt-card').forEach((c) => c.classList.remove('active'));
      card.classList.add('active');
      const mode = card.dataset.modelMode;
      overlay.querySelector('[data-preset-list]').style.display = mode === 'preset' ? '' : 'none';
      overlay.querySelector('[data-custom-repo]').style.display = mode === 'custom' ? '' : 'none';
    });
  });

  // Preset radio
  overlay.querySelectorAll('.preset-card').forEach((card) => {
    card.addEventListener('click', () => {
      overlay.querySelectorAll('.preset-card').forEach((c) => c.classList.remove('selected'));
      card.classList.add('selected');
    });
  });

  // GPU memory slider — debounced VRAM recommendation refresh.
  const slider = overlay.querySelector('[data-mu-slider]');
  const sliderVal = overlay.querySelector('[data-mu-value]');
  if (slider) {
    slider.addEventListener('input', () => {
      sliderVal.textContent = parseFloat(slider.value).toFixed(2);
      scheduleRecommendRefresh(overlay, svc, engineId);
    });
  }
  // Inputy max_model_len / max_num_seqs / KV dtype / preset zmieniają model
  // lub config — recompute estimate.
  ['data-max-model-len', 'data-max-num-seqs', 'data-max-batched', 'data-kv-dtype'].forEach((sel) => {
    const el = overlay.querySelector(`[${sel}]`);
    if (el) el.addEventListener('change', () => scheduleRecommendRefresh(overlay, svc, engineId));
  });
  overlay.querySelectorAll('.preset-card').forEach((card) => {
    card.addEventListener('click', () => scheduleRecommendRefresh(overlay, svc, engineId));
  });

  // HuggingFace search w Custom mode (publiczne API HF, nie nasz CBOR —
  // to bezpośredni katalog modeli HF, nie wewnętrzny stan tentaflow).
  const customInput = overlay.querySelector('[data-custom-repo-input]');
  if (customInput) {
    let hfTimer = null;
    customInput.addEventListener('input', () => {
      const q = customInput.value.trim();
      if (hfTimer) clearTimeout(hfTimer);
      if (q.length < 2) {
        const box = overlay.querySelector('[data-hf-results]');
        if (box) box.innerHTML = '';
        return;
      }
      hfTimer = setTimeout(() => doHfSearch(overlay, q, svc, engineId), 300);
    });
  }

  // Save handler
  const saveLabel = I18n.t('services_edit.save') || 'Zapisz i restartuj';
  const savingLabel = I18n.t('services_edit.saving') || 'Zapisuję…';
  overlay.querySelector('[data-save]').addEventListener('click', async () => {
    const saveBtn = overlay.querySelector('[data-save]');
    saveBtn.setAttribute('disabled', '');
    saveBtn.textContent = savingLabel;
    try {
      const payload = collectPayload(overlay, svc.id, opts.nodeId);
      const res = await ApiBinary.action('serviceConfigUpdateRequest', payload);
      if (res && res.success === false && res.error) {
        showInlineError(overlay, res.error);
        saveBtn.removeAttribute('disabled');
        saveBtn.textContent = saveLabel;
        return;
      }
      closeModal();
      onSaved();
    } catch (e) {
      showInlineError(overlay, e.message || String(e));
      saveBtn.removeAttribute('disabled');
      saveBtn.textContent = saveLabel;
    }
  });

  // VRAM hint live poll (2s) — pasek "co używa GPU teraz"
  startVramPoll(overlay, svc.id);
  // Pierwszy estimate VRAM (HF config.json + estimate_vllm_vram).
  if (isVllm) scheduleRecommendRefresh(overlay, svc, engineId);
}

// External provider model picker: lists the provider's live models with a
// checkbox per model (pre-checked = already added to model_registry). "Add to
// models" persists the selection so chosen models appear in Modele dostępne.
async function openExternalModelPicker(svc, opts = {}) {
  const onSaved = opts.onSaved || (() => {});
  const nodeId = opts.nodeId || svc.node_id || undefined;

  const overlay = document.createElement('div');
  overlay.className = 'modal-overlay';
  overlay.innerHTML = `
    <div class="modal-shell">
      <div class="modal-head">
        <h2>${escapeHtml(svc.display_name || svc.engine_id)} — ${escapeHtml(I18n.t('external.select_models'))}</h2>
        <button class="modal-x" data-close aria-label="close">×</button>
      </div>
      <div class="modal-body">
        <p class="form-hint">${escapeHtml(I18n.t('external.select_models_hint'))}</p>
        <div data-status class="form-hint">${escapeHtml(I18n.t('external.loading_models'))}</div>
        <div data-model-list class="external-model-list"></div>
      </div>
      <div class="modal-foot" style="display:flex;gap:8px;align-items:center;">
        <tf-button variant="secondary" icon="refresh" data-refresh>${escapeHtml(I18n.t('external.refresh_models'))}</tf-button>
        <span style="flex:1"></span>
        <tf-button variant="ghost" data-cancel>${escapeHtml(I18n.t('common.cancel'))}</tf-button>
        <tf-button variant="primary" icon="check" data-save>${escapeHtml(I18n.t('external.add_to_models'))}</tf-button>
      </div>
    </div>`;
  document.body.appendChild(overlay);
  currentModalEl = overlay;

  overlay.querySelector('[data-close]').addEventListener('click', closeModal);
  overlay.querySelector('[data-cancel]').addEventListener('click', closeModal);
  overlay.addEventListener('click', (e) => {
    if (e.target === overlay) closeModal();
  });

  const listEl = overlay.querySelector('[data-model-list]');
  const statusEl = overlay.querySelector('[data-status]');

  async function load() {
    statusEl.style.display = '';
    statusEl.textContent = I18n.t('external.loading_models');
    listEl.innerHTML = '';
    try {
      const res = await ApiBinary.action('serviceModelCatalogRequest', { serviceId: svc.id, nodeId });
      if (res && res.error) {
        statusEl.textContent = I18n.t('external.load_failed', { error: res.error });
        return;
      }
      const models = (res && res.models) || [];
      if (models.length === 0) {
        statusEl.textContent = I18n.t('external.no_models');
        return;
      }
      statusEl.style.display = 'none';
      // Catalog entries carry no pricing fields, so pull any stored pricing for
      // this org and prefill the inputs for models that already have prices.
      const pricingByModel = new Map();
      try {
        const pr = await ApiBinary.one('modelMetricsPricingGet');
        (Array.isArray(pr?.rows) ? pr.rows : []).forEach((r) => {
          if (r && r.modelId) pricingByModel.set(r.modelId, r);
        });
      } catch (_) {
        // Pricing prefill is best-effort; empty means "no change" on save.
      }
      listEl.innerHTML = models.map((m) => {
        const id = m.id || m.model_name || '';
        const ctxLen = m.contextLength || m.context_length;
        const ctxTxt = ctxLen ? ` · ${ctxLen}` : '';
        const name = m.displayName || m.display_name || id;
        // Prefill from stored pricing (keyed by model id); empty stays empty.
        const stored = pricingByModel.get(id);
        const pv = (field) => {
          const v = stored ? stored[field] : undefined;
          return v === undefined || v === null ? '' : String(v);
        };
        const priceCell = (field, label, value) => `
          <tf-input class="external-price-in" type="number" min="0" step="0.0001"
            data-price-model="${escapeAttr(id)}" data-price-field="${escapeAttr(field)}"
            label="${escapeAttr(label)}" value="${escapeAttr(value)}"></tf-input>`;
        return `
          <div class="external-model-row" data-model-row style="display:flex;flex-direction:column;gap:6px;padding:8px 0;border-bottom:1px solid var(--border-subtle,#2a2a2a);">
            <label style="display:flex;align-items:center;gap:10px;">
              <tf-checkbox ${m.selected ? 'checked' : ''} data-model-id="${escapeAttr(id)}"></tf-checkbox>
              <span style="flex:1">${escapeHtml(name)}${escapeHtml(ctxTxt)}</span>
              <tf-chip>${escapeHtml(m.modality || 'chat')}</tf-chip>
            </label>
            <div class="external-model-pricing" style="display:grid;grid-template-columns:repeat(4,1fr);gap:6px;">
              ${priceCell('promptPer1k', I18n.t('model_metrics.col_price_prompt'), pv('promptPer1k'))}
              ${priceCell('completionPer1k', I18n.t('model_metrics.col_price_completion'), pv('completionPer1k'))}
              ${priceCell('audioPerMin', I18n.t('model_metrics.col_price_audio'), pv('audioPerMin'))}
              ${priceCell('imageEach', I18n.t('model_metrics.col_price_image'), pv('imageEach'))}
            </div>
          </div>`;
      }).join('');
      const pricingHint = document.createElement('p');
      pricingHint.className = 'form-hint';
      pricingHint.textContent = I18n.t('external.pricing_hint');
      listEl.prepend(pricingHint);
    } catch (e) {
      statusEl.style.display = '';
      statusEl.textContent = I18n.t('external.load_failed', { error: e.message || String(e) });
    }
  }

  overlay.querySelector('[data-refresh]').addEventListener('click', load);
  overlay.querySelector('[data-save]').addEventListener('click', async () => {
    const saveBtn = overlay.querySelector('[data-save]');
    saveBtn.setAttribute('disabled', '');
    const ids = Array.from(listEl.querySelectorAll('tf-checkbox'))
      .filter((c) => c.checked)
      .map((c) => c.getAttribute('data-model-id'))
      .filter(Boolean);

    // Per-model pricing: one entry per model where the user typed any of the
    // four fields. Empty fields stay null (None); negatives abort the save.
    const idSet = new Set(ids);
    const pricingByModel = new Map();
    let pricingInvalid = false;
    listEl.querySelectorAll('tf-input[data-price-model]').forEach((el) => {
      const modelId = el.getAttribute('data-price-model');
      if (!idSet.has(modelId)) return;
      const raw = String(el.value ?? '').trim();
      if (raw === '') return;
      const n = Number(raw);
      if (!Number.isFinite(n) || n < 0) { pricingInvalid = true; return; }
      const entry = pricingByModel.get(modelId) || { modelId };
      entry[el.getAttribute('data-price-field')] = n;
      pricingByModel.set(modelId, entry);
    });
    if (pricingInvalid) {
      statusEl.style.display = '';
      statusEl.textContent = I18n.t('external.pricing_invalid');
      saveBtn.removeAttribute('disabled');
      return;
    }
    const pricing = Array.from(pricingByModel.values());

    try {
      const res = await ApiBinary.action('serviceModelSelectionRequest', {
        serviceId: svc.id,
        nodeId,
        selectedModelIds: ids,
        pricing,
      });
      if (res && res.success === false) {
        statusEl.style.display = '';
        statusEl.textContent = I18n.t('external.save_failed', { error: res.error || '' });
        saveBtn.removeAttribute('disabled');
        return;
      }
      closeModal();
      onSaved();
    } catch (e) {
      statusEl.style.display = '';
      statusEl.textContent = I18n.t('external.save_failed', { error: e.message || String(e) });
      saveBtn.removeAttribute('disabled');
    }
  });

  await load();
}

function closeModal() {
  if (vramPollHandle) {
    clearInterval(vramPollHandle);
    vramPollHandle = null;
  }
  if (recommendDebounceHandle) {
    clearTimeout(recommendDebounceHandle);
    recommendDebounceHandle = null;
  }
  if (currentModalEl) {
    currentModalEl.remove();
    currentModalEl = null;
  }
}

// Debounced refresh kalkulatora VRAM. Po zmianie modelu / ctx / utilization
// czekamy 250ms zeby uniknąć spam HF API i auto-fit calls. Backend
// `DeployVllmRecommendRequest` fetchuje config.json modelu z HF (cache po
// stronie reqwest) i odpala `estimate_vllm_vram` — to ten SAM kalkulator
// którego używa wizard krok Advanced. KPI grid + segmentowy pasek w VRAM
// card są renderowane z `vram_estimate` (model_weights_gb/kv_cache_gb/
// activations_gb/per_gpu_gb/fits_per_gpu/warnings).
function scheduleRecommendRefresh(overlay, svc, engineId) {
  if (!engineUsesCudaVllm(engineId)) return;
  if (recommendDebounceHandle) clearTimeout(recommendDebounceHandle);
  recommendDebounceHandle = setTimeout(() => {
    fetchRecommendation(overlay, svc).catch((e) => {
      console.warn('VRAM recommend:', e);
    });
  }, 250);
}

async function fetchRecommendation(overlay, _svc) {
  const card = overlay.querySelector('[data-vram-card]');
  if (!card) return;

  // Model: preferuj selected preset repo, fallback na custom HF input.
  const sel = overlay.querySelector('.preset-card.selected');
  const customInput = overlay.querySelector('[data-custom-repo-input]');
  let modelRepo = null;
  if (sel) modelRepo = sel.dataset.presetRepo;
  if (!modelRepo && customInput && customInput.value.trim()) modelRepo = customInput.value.trim();
  if (!modelRepo) {
    card.innerHTML = `<div style="font-size:11px;color:var(--text-3);">${escapeHtml(I18n.t('services_edit.vram.no_model') || 'Wybierz model żeby policzyć VRAM.')}</div>`;
    return;
  }

  const muSlider = overlay.querySelector('[data-mu-slider]');
  const maxModelLen = overlay.querySelector('[data-max-model-len]');
  const maxNumSeqs = overlay.querySelector('[data-max-num-seqs]');
  const kvDtype = overlay.querySelector('[data-kv-dtype]');

  // Pobierz aktualne GPU specs (z VRAM hint snapshot — zostaje cached).
  let gpus = [];
  try {
    const res = await ApiBinary.action('serviceVramHintRequest', { gpuIndex: 0 });
    if (res && Array.isArray(res.gpus)) {
      gpus = res.gpus.map((g) => ({
        index: g.gpu_index,
        memory_gb: (g.total_mib || 0) / 1024,
        name: g.gpu_name || '',
      }));
    }
  } catch (_e) {}
  if (gpus.length === 0) {
    card.innerHTML = `<div style="font-size:11px;color:var(--text-3);">${escapeHtml(I18n.t('services_edit.vram.no_gpu') || 'GPU nie wykryte (nvidia-smi).')}</div>`;
    return;
  }

  card.innerHTML = `<div style="font-size:11px;color:var(--text-3);">${escapeHtml(I18n.t('services_edit.vram.loading') || 'Liczenie VRAM z config.json modelu HF…')}</div>`;

  const reqPayload = {
    model: modelRepo,
    gpus,
    gpuMemoryUtilization: muSlider ? parseFloat(muSlider.value) : 0.9,
    maxModelLen: maxModelLen?.value ? parseInt(maxModelLen.value, 10) : null,
    maxNumSeqs: maxNumSeqs?.value ? parseInt(maxNumSeqs.value, 10) : null,
    kvCacheDtype: kvDtype?.value || null,
    quantizationOverride: null,
    hfToken: null,
    tensorParallel: null,
    pipelineParallel: null,
    lockMaxModelLen: false,
    lockMaxNumSeqs: false,
    lockTensorParallel: false,
  };

  let resp;
  try {
    resp = await ApiBinary.action('deployVllmRecommendRequest', reqPayload);
  } catch (e) {
    card.innerHTML = `<div style="font-size:11px;color:var(--danger);">HF: ${escapeHtml(e.message || String(e))}</div>`;
    return;
  }
  renderRecommendation(card, resp);
}

function renderRecommendation(card, r) {
  const v = r.vram_estimate || {};
  const spec = r.model_spec || {};
  const totalGb = v.per_gpu_gb && r.applied?.tensor_parallel
    ? (v.per_gpu_gb * r.applied.tensor_parallel)
    : v.total_gb || 0;
  const fitsPct = totalGb > 0 ? Math.min(200, Math.round((v.total_gb / totalGb) * 100)) : 0;
  const fits = v.fits_per_gpu !== false;
  const tk = (k) => I18n.t(`services_edit.vram.${k}`) || k;

  const pillCls = fits ? 'ok' : 'danger';
  const pillTxt = fits
    ? (I18n.t('services_edit.vram.pill_fits') || 'Mieści się')
    : (I18n.t('services_edit.vram.pill_oom') || 'Nie mieści się');

  const weightsGb = (v.model_weights_gb || 0).toFixed(1);
  const kvGb = (v.kv_cache_gb || 0).toFixed(1);
  const actGb = (v.activations_gb || 0).toFixed(1);
  const perGpuGb = (v.per_gpu_gb || 0).toFixed(1);
  const totalUsed = (v.total_gb || 0).toFixed(1);

  const warningsHtml = (v.warnings || []).map((w) => `<li>${escapeHtml(w)}</li>`).join('');

  card.innerHTML = `
    <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:10px;">
      <span style="font-size:11px;color:var(--text-3);text-transform:uppercase;letter-spacing:0.08em;font-weight:700;">
        ${escapeHtml(tk('calc_title'))}
      </span>
      <span style="font-size:11px;padding:3px 9px;border-radius:999px;font-weight:700;${
        fits
          ? 'background:rgba(34,197,94,0.14);color:var(--success);'
          : 'background:rgba(239,68,68,0.14);color:var(--danger);'
      }">${escapeHtml(pillTxt)}</span>
    </div>
    <div style="display:grid;grid-template-columns:repeat(5,1fr);gap:8px;margin-bottom:10px;">
      ${kpiCell(tk('kpi_weights'), `${weightsGb} GB`, escapeHtml(spec.dtype || ''))}
      ${kpiCell(tk('kpi_kv'), `${kvGb} GB`, `ctx ${(r.applied?.max_model_len || 0).toLocaleString()}`)}
      ${kpiCell(tk('kpi_activations'), `${actGb} GB`, '')}
      ${kpiCell(tk('kpi_per_gpu'), `${perGpuGb} GB`, `${r.applied?.tensor_parallel || 1}× GPU`)}
      ${kpiCell(tk('kpi_total'), `${totalUsed} GB`, `${fitsPct}%`, fits ? 'success' : 'danger')}
    </div>
    ${warningsHtml ? `<ul style="font-size:11px;color:var(--warning);margin-left:18px;">${warningsHtml}</ul>` : ''}
  `;
}

async function doHfSearch(overlay, query, svc, engineId) {
  const box = overlay.querySelector('[data-hf-results]');
  if (!box) return;
  box.innerHTML = `<div style="font-size:11px;color:var(--text-3);">${escapeHtml(I18n.t('common.loading') || '…')}</div>`;
  try {
    const url = `https://huggingface.co/api/models?search=${encodeURIComponent(query)}&limit=15&sort=downloads&direction=-1`;
    const resp = await fetch(url);
    if (!resp.ok) throw new Error(`HF API ${resp.status}`);
    const data = await resp.json();
    if (!Array.isArray(data) || data.length === 0) {
      box.innerHTML = `<div style="font-size:11px;color:var(--text-3);">${escapeHtml(I18n.t('services_edit.hf_no_results') || 'No matches.')}</div>`;
      return;
    }
    box.innerHTML = data.map((m) => {
      const id = m.id || m.modelId || '';
      const downloads = m.downloads ? formatHfCount(m.downloads) : '';
      const likes = m.likes || '';
      const lastMod = m.lastModified ? m.lastModified.substring(0, 10) : '';
      const info = [downloads && `↓ ${downloads}`, likes && `♥ ${likes}`, lastMod].filter(Boolean).join(' · ');
      return `
        <div class="hf-item" data-hf-pick="${escapeAttr(id)}"
          style="padding:8px 10px;background:var(--bg-input);border:1px solid var(--border);border-radius:6px;cursor:pointer;">
          <div style="font-family:'JetBrains Mono',monospace;font-size:12.5px;color:var(--text);">${escapeHtml(id)}</div>
          ${info ? `<div style="font-size:10.5px;color:var(--text-3);margin-top:2px;">${escapeHtml(info)}</div>` : ''}
        </div>`;
    }).join('');
    // Click handler — set input value + trigger VRAM recompute.
    box.querySelectorAll('[data-hf-pick]').forEach((it) => {
      it.addEventListener('click', () => {
        const repo = it.dataset.hfPick;
        const input = overlay.querySelector('[data-custom-repo-input]');
        if (input) input.value = repo;
        box.innerHTML = '';
        scheduleRecommendRefresh(overlay, svc, engineId);
      });
    });
  } catch (e) {
    box.innerHTML = `<div style="font-size:11px;color:var(--danger);">HF: ${escapeHtml(e.message || String(e))}</div>`;
  }
}
function formatHfCount(n) {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

function kpiCell(label, value, sub, cls) {
  const valueColor = cls === 'success' ? 'color:var(--success);' : (cls === 'danger' ? 'color:var(--danger);' : '');
  return `
    <div style="background:var(--bg-3);border:1px solid var(--border);border-radius:8px;padding:8px 10px;">
      <div style="font-size:9.5px;color:var(--text-3);text-transform:uppercase;letter-spacing:0.06em;font-weight:700;">${escapeHtml(label)}</div>
      <div style="font-size:14px;font-weight:800;font-family:'JetBrains Mono',monospace;${valueColor}">${escapeHtml(value)}</div>
      <div style="font-size:10px;color:var(--text-3);">${escapeHtml(sub || '')}</div>
    </div>`;
}

function startVramPoll(overlay, serviceId) {
  const tick = async () => {
    try {
      const res = await ApiBinary.action('serviceVramHintRequest', {
        gpuIndex: 0,
        excludeServiceId: serviceId,
      });
      renderVramSnapshot(overlay, res);
    } catch (_e) {
      // silent — overlay zostawia ostatni snapshot
    }
  };
  tick();
  vramPollHandle = setInterval(tick, 2000);
}

function renderVramSnapshot(overlay, res) {
  if (!res || !Array.isArray(res.gpus) || res.gpus.length === 0) {
    const wrap = overlay.querySelector('[data-vram-bar]');
    if (wrap) wrap.innerHTML = '<div style="font-size:11px;color:var(--text-3);">GPU nie wykryte (brak nvidia-smi).</div>';
    return;
  }
  const gpu = res.gpus[0];
  const totalMib = gpu.total_mib || 1;
  const externalMib = (gpu.external_processes || []).reduce((acc, p) => acc + (p.used_mib || 0), 0);
  const externalPct = (externalMib / totalMib) * 100;

  const slider = overlay.querySelector('[data-mu-slider]');
  const utilVal = parseFloat(slider?.value || 0.9);
  const capMib = utilVal * totalMib;
  const capPct = (capMib / totalMib) * 100;
  const headroomMib = Math.max(0, totalMib - externalMib - capMib);
  const headroomPct = Math.max(0, 100 - externalPct - capPct);

  const procsHtml = (gpu.external_processes || [])
    .slice(0, 5)
    .map(
      (p) =>
        `<span class="name">${escapeHtml(p.process_name || `pid ${p.pid}`)} (PID ${p.pid})</span><span class="mb">${(p.used_mib || 0).toLocaleString()} MiB</span>`
    )
    .join('');

  const free = (gpu.free_mib || 0).toLocaleString();
  const recommended = res.recommended_utilization;
  const recommendHtml = recommended != null
    ? `<span style="color:var(--text-2);">Rekomendowane: <b style="color:var(--success);font-family:'JetBrains Mono',monospace;">${recommended.toFixed(2)}</b></span>`
    : '';

  const bar = overlay.querySelector('[data-vram-bar]');
  if (bar) {
    bar.innerHTML = `
      <div class="adv-extern-head">
        <span>Co już używa GPU 0 (${escapeHtml(gpu.gpu_name || '?')}, ${(totalMib/1024).toFixed(1)} GB)</span>
        <span class="extern-total">${externalMib.toLocaleString()} MiB · ${externalPct.toFixed(1)}%</span>
      </div>
      <div class="adv-extern-bar">
        ${(gpu.external_processes || []).map((p) => `<div class="ext-seg" title="${escapeHtml(p.process_name)}: ${p.used_mib} MiB" style="width: ${((p.used_mib || 0) / totalMib * 100).toFixed(2)}%;"></div>`).join('')}
        <div class="cap-seg" style="width: ${capPct.toFixed(1)}%; background: rgba(99,102,241,0.4);"></div>
        <div class="deploy-room" style="width: ${headroomPct.toFixed(1)}%; background: rgba(34,197,94,0.18);"></div>
      </div>
      <div class="adv-extern-procs">${procsHtml}</div>
      <div class="extern-foot">
        <span>Refresh co 2s · nvidia-smi</span>
        <span style="color:var(--text-2);">Wolne: <b style="color:var(--success);font-family:'JetBrains Mono',monospace;">${free} MiB</b> · ${recommendHtml}</span>
      </div>`;
  }
  updateVramFitBanner(overlay);
}

function updateVramFitBanner(overlay) {
  // Prosty banner: gdy slider value × total > free + external (wgl. fit
  // estymacji), ostrzegamy. Dokładny KV cache estimate wymaga manifest
  // schema — MVP pokazuje tylko czy raw cap mieści się w free.
  // TODO Krok 9: integracja z catalog VRAM calculator (recommend API).
}

function collectPayload(overlay, serviceId, nodeId) {
  const modeCard = overlay.querySelector('.opt-card.active');
  const mode = modeCard?.dataset.modelMode || 'preset';
  let modelRepo = null;
  let modelPresetId = null;
  if (mode === 'preset') {
    const sel = overlay.querySelector('.preset-card.selected');
    if (sel) modelPresetId = sel.dataset.presetId;
  } else {
    const repoInput = overlay.querySelector('[data-custom-repo-input]');
    if (repoInput && repoInput.value.trim()) modelRepo = repoInput.value.trim();
  }
  const muSlider = overlay.querySelector('[data-mu-slider]');
  const maxModelLen = overlay.querySelector('[data-max-model-len]');
  const maxNumSeqs = overlay.querySelector('[data-max-num-seqs]');
  const maxBatched = overlay.querySelector('[data-max-batched]');
  const kvDtype = overlay.querySelector('[data-kv-dtype]');
  const cp = overlay.querySelector('[data-chunked-prefill]');
  const restart = overlay.querySelector('[data-restart-after-save]');

  return {
    serviceId,
    nodeId: nodeId || undefined,
    modelRepo,
    modelPresetId,
    gpuMemoryUtilization: muSlider ? parseFloat(muSlider.value) : null,
    maxModelLen: maxModelLen?.value ? parseInt(maxModelLen.value, 10) : null,
    maxNumSeqs: maxNumSeqs?.value ? parseInt(maxNumSeqs.value, 10) : null,
    maxNumBatchedTokens: maxBatched?.value ? parseInt(maxBatched.value, 10) : null,
    kvCacheDtype: kvDtype?.value || null,
    chunkedPrefill: cp ? cp.checked : null,
    vllmArgsOverride: null,
    pinned: null,
    paused: null,
    restartAfterSave: restart ? restart.checked : true,
  };
}

function showInlineError(overlay, msg) {
  let box = overlay.querySelector('[data-error]');
  if (!box) return;
  box.style.display = '';
  box.textContent = msg;
}

function parseConfig(configJson) {
  if (!configJson) return {};
  try {
    return typeof configJson === 'string' ? JSON.parse(configJson) : configJson;
  } catch (_e) {
    return {};
  }
}

function escapeHtml(s) {
  return String(s ?? '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}
function escapeAttr(s) { return escapeHtml(s); }

function renderShell(svc, engineId, cfg, isVllm, initialPresetId, initialModelRepo, presets) {
  const status = (svc.status || '').toLowerCase();
  const isStarting = status === 'starting';
  const dotCls = status === 'running' ? 'ok' : (status === 'failed' ? '' : 'warn');
  const headMeta = `#${svc.id} · ${svc.endpoint_url ? svc.endpoint_url.replace(/^https?:\/\/[^/]*/, '') : ''} · ${svc.status}`;
  const restartWarn = isStarting || status === 'running' || status === 'degraded';
  const tk = (k) => I18n.t(`services_edit.${k}`) || k;

  return `
    <div class="modal-shell" style="margin-top:5vh;">
      <div class="modal-head">
        <div class="dot ${dotCls}"></div>
        <h3>${escapeHtml(tk('title'))}: ${escapeHtml(svc.display_name || engineId)}</h3>
        <span class="head-meta">${escapeHtml(headMeta)}</span>
        <button class="btn ghost" data-close style="padding:4px 10px;min-height:0;font-size:18px;" title="${escapeAttr(tk('close'))}">×</button>
      </div>
      <div class="modal-body">
        <div class="step-heading">
          <h2>${escapeHtml(tk('subtitle').replace('{engine}', engineId))}</h2>
          <p>${escapeHtml(tk('subtitle_desc'))}</p>
        </div>
        ${restartWarn ? `
          <div class="step-warn">
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 9v3M12 17h.01M5.07 19h13.86c1.54 0 2.5-1.67 1.73-3L13.73 4c-.77-1.33-2.69-1.33-3.46 0L3.34 16c-.77 1.33.19 3 1.73 3z"/></svg>
            <div class="txt">${escapeHtml(tk('restart_warn'))}</div>
          </div>
        ` : ''}

        <div data-error style="display:none;background:rgba(239,68,68,0.08);border:1px solid rgba(239,68,68,0.3);color:var(--danger);padding:10px 14px;border-radius:8px;font-size:12px;margin-bottom:14px;"></div>

        ${renderModelSection(initialPresetId, initialModelRepo, presets, tk)}

        ${isVllm ? renderVramSection(tk) : ''}

        ${isVllm ? renderEngineArgsSection(cfg, tk) : renderGenericArgsSection(tk)}

        <div class="body-section">
          <div class="body-section-title">${escapeHtml(tk('immutable'))}</div>
          <div class="fact-list">
            <div class="row"><span class="key">${escapeHtml(tk('immutable_engine'))}</span><span class="val">${escapeHtml(svc.engine_id || '')}</span></div>
            <div class="row"><span class="key">${escapeHtml(tk('immutable_method'))}</span><span class="val">${escapeHtml(svc.deploy_method || '')}</span></div>
            <div class="row"><span class="key">${escapeHtml(tk('immutable_transport'))}</span><span class="val">${escapeHtml(svc.transport || '')}</span></div>
            <div class="row"><span class="key">${escapeHtml(tk('immutable_port'))}</span><span class="val">${escapeHtml(String(svc.runtime_port || ''))}</span></div>
          </div>
        </div>
      </div>
      <div class="modal-foot">
        <button class="btn ghost" data-cancel>${escapeHtml(tk('cancel'))}</button>
        <div class="spacer"></div>
        <label style="font-size:12px;color:var(--text-2);display:inline-flex;align-items:center;gap:6px;cursor:pointer;">
          <input type="checkbox" data-restart-after-save checked style="accent-color:var(--accent-1);">
          ${escapeHtml(tk('restart_after_save'))}
        </label>
        <button class="btn primary" data-save>
          <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 12a9 9 0 1 1-3-6.7"/><polyline points="21 3 21 9 15 9"/></svg>
          ${escapeHtml(tk('save'))}
        </button>
      </div>
    </div>`;
}

function renderModelSection(initialPresetId, initialModelRepo, presets, tk) {
  // Wybor mode: jesli config ma `model_preset_id` ktore istnieje w manifescie
  // → preset. Jesli ma `model_repo` ktorym pasuje preset → preset (selected
  // ten ktory ma ten repo). Jesli `model_repo` ale ZADEN preset nie pasuje
  // → custom HF (bo user wybral cos spoza manifestu).
  let useCustom = false;
  let resolvedPresetId = initialPresetId;
  if (!resolvedPresetId && initialModelRepo) {
    const match = presets.find((p) => p.repo === initialModelRepo);
    if (match) {
      resolvedPresetId = match.id;
    } else {
      useCustom = true;
    }
  }
  const presetsHtml = renderPresetCards(presets, resolvedPresetId, tk);
  return `
    <div class="body-section">
      <div class="body-section-title">${escapeHtml(tk('model'))}</div>
      <div class="opt-cards">
        <div class="opt-card${useCustom ? '' : ' active'}" data-model-mode="preset">
          <div class="opt-icon">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="3" width="7" height="7"/><rect x="14" y="3" width="7" height="7"/><rect x="14" y="14" width="7" height="7"/><rect x="3" y="14" width="7" height="7"/></svg>
          </div>
          <div class="opt-title">${escapeHtml(tk('model_preset'))} <span class="tag">${presets.length}</span></div>
          <div class="opt-desc">${escapeHtml(tk('model_preset_desc'))}</div>
        </div>
        <div class="opt-card${useCustom ? ' active' : ''}" data-model-mode="custom">
          <div class="opt-icon">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="17 8 12 3 7 8"/><line x1="12" y1="3" x2="12" y2="15"/></svg>
          </div>
          <div class="opt-title">${escapeHtml(tk('model_custom'))}</div>
          <div class="opt-desc">${escapeHtml(tk('model_custom_desc'))}</div>
        </div>
      </div>
      <div data-preset-list style="${useCustom ? 'display:none;' : ''}">
        <div class="preset-list">
          ${presetsHtml || `<div style="font-size:11px;color:var(--text-3);padding:8px 0;">${escapeHtml(tk('no_presets'))}</div>`}
        </div>
      </div>
      <div data-custom-repo style="${useCustom ? '' : 'display:none;'}">
        <div class="form-row">
          <label>${escapeHtml(tk('hf_repo_label'))}</label>
          <input class="tf-input mono" data-custom-repo-input
            placeholder="${escapeAttr(tk('hf_search_placeholder'))}"
            value="${escapeAttr(initialModelRepo)}" autocomplete="off">
          <div class="hf-results" data-hf-results style="margin-top:8px;display:flex;flex-direction:column;gap:6px;max-height:240px;overflow-y:auto;"></div>
        </div>
      </div>
    </div>`;
}

function renderPresetCards(presets, initialPresetId, tk) {
  if (!presets || presets.length === 0) return '';
  // Wybor sel: jesli initialPresetId pasuje do listy → ten. Inaczej recommended
  // jako fallback. Inaczej pierwszy.
  const matched = initialPresetId && presets.find((p) => p.id === initialPresetId);
  const fallback = !matched && (presets.find((p) => p.recommended) || presets[0]);
  const selectedId = matched ? matched.id : fallback?.id;

  return presets
    .map((p) => {
      const sel = p.id === selectedId;
      const recPill = p.recommended
        ? `<span class="preset-rec">${escapeHtml(tk('recommended_pill'))}</span>`
        : '';
      const quantTag = p.quantization
        ? `<span class="sep">·</span><span>${escapeHtml(p.quantization)}</span>`
        : '';
      return `
        <div class="preset-card${sel ? ' selected' : ''}"
             data-preset-id="${escapeAttr(p.id)}"
             data-preset-repo="${escapeAttr(p.repo)}">
          <div class="preset-radio"></div>
          <div class="preset-info">
            <div class="preset-name">${escapeHtml(p.display_name)} ${recPill}</div>
            <div class="preset-meta">
              <span>${escapeHtml(p.repo)}</span>${quantTag}
            </div>
          </div>
        </div>`;
    })
    .join('');
}

function renderVramSection(tk) {
  return `
    <div class="body-section">
      <div class="body-section-title">${escapeHtml(tk('vram_title'))}</div>
      <div class="adv-section">
        <div data-vram-card>
          <div style="font-size:11px;color:var(--text-3);">${escapeHtml(tk('vram.loading') || 'Liczenie…')}</div>
        </div>
        <div class="adv-extern" data-vram-bar style="margin-top:14px;">
          <div style="font-size:11px;color:var(--text-3);">${escapeHtml(tk('vram_external_loading'))}</div>
        </div>
        <div style="margin-top:14px;">
          <label style="display:flex;justify-content:space-between;font-size:11.5px;font-weight:600;color:var(--text-2);">
            <span>gpu_memory_utilization</span>
            <span data-mu-value style="color:var(--accent-2);font-family:'JetBrains Mono',monospace;font-weight:700;">0.90</span>
          </label>
          <input type="range" data-mu-slider min="0.15" max="0.9" step="0.01" value="0.90" style="width:100%;margin-top:6px;">
          <div style="display:flex;justify-content:space-between;font-size:10.5px;color:var(--text-3);margin-top:4px;padding:0 8px;">
            <span>0.15</span><span>0.30</span><span>0.50</span><span>0.70</span><span>0.90</span>
          </div>
        </div>
      </div>
    </div>`;
}

function renderEngineArgsSection(cfg, tk) {
  return `
    <div class="body-section">
      <div class="body-section-title">${escapeHtml(tk('runtime_title'))}</div>
      <div class="adv-row-2">
        <div class="form-row">
          <label>${escapeHtml(tk('max_model_len'))}</label>
          <input class="tf-input mono" data-max-model-len value="${escapeAttr(cfg.max_model_len ?? '')}" placeholder="32768">
        </div>
        <div class="form-row">
          <label>${escapeHtml(tk('max_num_seqs'))}</label>
          <input class="tf-input mono" data-max-num-seqs value="${escapeAttr(cfg.max_num_seqs ?? '')}" placeholder="1">
        </div>
        <div class="form-row">
          <label>${escapeHtml(tk('max_batched'))}</label>
          <input class="tf-input mono" data-max-batched value="${escapeAttr(cfg.max_num_batched_tokens ?? '')}" placeholder="8192">
        </div>
        <div class="form-row">
          <label>${escapeHtml(tk('kv_dtype'))}</label>
          <select class="tf-select" data-kv-dtype>
            <option value="">${escapeHtml(tk('kv_dtype_default'))}</option>
            <option value="auto"${cfg.kv_cache_dtype === 'auto' ? ' selected' : ''}>auto</option>
            <option value="fp8"${cfg.kv_cache_dtype === 'fp8' ? ' selected' : ''}>fp8</option>
            <option value="fp16"${cfg.kv_cache_dtype === 'fp16' ? ' selected' : ''}>fp16</option>
            <option value="bf16"${cfg.kv_cache_dtype === 'bf16' ? ' selected' : ''}>bf16</option>
          </select>
        </div>
      </div>
      <div class="form-row" style="margin-top:8px;">
        <label style="display:flex;align-items:center;gap:8px;cursor:pointer;font-size:12.5px;">
          <input type="checkbox" data-chunked-prefill${cfg.chunked_prefill === false ? '' : ' checked'} style="accent-color:var(--accent-1);">
          <span>${escapeHtml(tk('chunked_prefill'))}</span>
        </label>
      </div>
    </div>`;
}

function renderGenericArgsSection(tk) {
  return `
    <div class="body-section">
      <div class="body-section-title">${escapeHtml(tk('runtime_title'))}</div>
      <div class="form-row">
        <label>${escapeHtml(tk('runtime_generic_note'))}</label>
      </div>
    </div>`;
}

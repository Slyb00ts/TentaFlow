// =============================================================================
// File: modules/services-edit.js — Edit Service modal (rkyv binary)
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

import { ApiBinary } from '../lib/api-binary.js';
import { I18n } from '../lib/i18n.js';

let currentModalEl = null;
let vramPollHandle = null;

export function openEditModal(svc, opts = {}) {
  closeModal();
  const onSaved = opts.onSaved || (() => {});
  const engineId = opts.engineId || svc.engine_id;
  const cfg = parseConfig(svc.config_json);
  const initialPresetId = cfg.model_preset_id || null;
  const initialModelRepo = cfg.model_repo || '';
  const isVllm = engineId === 'vllm';

  const overlay = document.createElement('div');
  overlay.className = 'modal-overlay';
  overlay.innerHTML = renderShell(svc, engineId, cfg, isVllm, initialPresetId, initialModelRepo);
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

  // GPU memory slider
  const slider = overlay.querySelector('[data-mu-slider]');
  const sliderVal = overlay.querySelector('[data-mu-value]');
  if (slider) {
    slider.addEventListener('input', () => {
      sliderVal.textContent = parseFloat(slider.value).toFixed(2);
      updateVramFitBanner(overlay);
    });
  }

  // Save handler
  overlay.querySelector('[data-save]').addEventListener('click', async () => {
    const saveBtn = overlay.querySelector('[data-save]');
    saveBtn.setAttribute('disabled', '');
    saveBtn.textContent = 'Zapisuję…';
    try {
      const payload = collectPayload(overlay, svc.id, opts.nodeId);
      const res = await ApiBinary.action('serviceUpdateRequest', payload);
      if (res && res.success === false && res.error) {
        showInlineError(overlay, res.error);
        saveBtn.removeAttribute('disabled');
        saveBtn.textContent = 'Zapisz i restartuj';
        return;
      }
      closeModal();
      onSaved();
    } catch (e) {
      showInlineError(overlay, e.message || String(e));
      saveBtn.removeAttribute('disabled');
      saveBtn.textContent = 'Zapisz i restartuj';
    }
  });

  // VRAM hint live poll (2s)
  startVramPoll(overlay, svc.id);
}

function closeModal() {
  if (vramPollHandle) {
    clearInterval(vramPollHandle);
    vramPollHandle = null;
  }
  if (currentModalEl) {
    currentModalEl.remove();
    currentModalEl = null;
  }
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

function renderShell(svc, engineId, cfg, isVllm, initialPresetId, initialModelRepo) {
  const status = (svc.status || '').toLowerCase();
  const isStarting = status === 'starting';
  const dotCls = status === 'running' ? 'ok' : (status === 'failed' ? '' : 'warn');
  const headMeta = `#${svc.id} · ${svc.endpoint_url ? svc.endpoint_url.replace(/^https?:\/\/[^/]*/, '') : ''} · ${svc.status}`;
  const restartWarn = isStarting || status === 'running' || status === 'degraded';

  return `
    <div class="modal-shell" style="margin-top:5vh;">
      <div class="modal-head">
        <div class="dot ${dotCls}"></div>
        <h3>Edycja serwisu: ${escapeHtml(svc.display_name || engineId)}</h3>
        <span class="head-meta">${escapeHtml(headMeta)}</span>
        <button class="btn ghost" data-close style="padding:4px 10px;min-height:0;font-size:18px;">×</button>
      </div>
      <div class="modal-body">
        <div class="step-heading">
          <h2>Konfiguracja silnika ${escapeHtml(engineId)}</h2>
          <p>Zmiana modelu, alokacji VRAM lub parametrów runtime. Engine_id / deploy_method / port są niezmienne.</p>
        </div>
        ${restartWarn ? `
          <div class="step-warn">
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 9v3M12 17h.01M5.07 19h13.86c1.54 0 2.5-1.67 1.73-3L13.73 4c-.77-1.33-2.69-1.33-3.46 0L3.34 16c-.77 1.33.19 3 1.73 3z"/></svg>
            <div class="txt">
              <strong>Serwis żyje teraz.</strong> Zapisanie zatrzyma serwis, zaktualizuje konfigurację
              i uruchomi go ponownie. Strumienie chat / TTS aktywne dostaną <em>503</em>. Reload modelu ~30–180 s.
            </div>
          </div>
        ` : ''}

        <div data-error style="display:none;background:rgba(239,68,68,0.08);border:1px solid rgba(239,68,68,0.3);color:var(--danger);padding:10px 14px;border-radius:8px;font-size:12px;margin-bottom:14px;"></div>

        ${renderModelSection(initialPresetId, initialModelRepo)}

        ${isVllm ? renderVramSection(cfg) : ''}

        ${isVllm ? renderEngineArgsSection(cfg) : renderGenericArgsSection(cfg)}

        <div class="body-section">
          <div class="body-section-title">Niezmienne</div>
          <div class="fact-list">
            <div class="row"><span class="key">Engine ID</span><span class="val">${escapeHtml(svc.engine_id || '')}</span></div>
            <div class="row"><span class="key">Deploy method</span><span class="val">${escapeHtml(svc.deploy_method || '')}</span></div>
            <div class="row"><span class="key">Transport</span><span class="val">${escapeHtml(svc.transport || '')}</span></div>
            <div class="row"><span class="key">Port</span><span class="val">${escapeHtml(String(svc.runtime_port || ''))}</span></div>
          </div>
        </div>
      </div>
      <div class="modal-foot">
        <button class="btn ghost" data-cancel>Anuluj</button>
        <div class="spacer"></div>
        <label style="font-size:12px;color:var(--text-2);display:inline-flex;align-items:center;gap:6px;cursor:pointer;">
          <input type="checkbox" data-restart-after-save checked style="accent-color:var(--accent-1);">
          Restart po zapisie
        </label>
        <button class="btn primary" data-save>
          <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 12a9 9 0 1 1-3-6.7"/><polyline points="21 3 21 9 15 9"/></svg>
          Zapisz i restartuj
        </button>
      </div>
    </div>`;
}

function renderModelSection(initialPresetId, initialModelRepo) {
  const useCustom = !initialPresetId && initialModelRepo;
  return `
    <div class="body-section">
      <div class="body-section-title">Model</div>
      <div class="opt-cards">
        <div class="opt-card${useCustom ? '' : ' active'}" data-model-mode="preset">
          <div class="opt-icon">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="3" width="7" height="7"/><rect x="14" y="3" width="7" height="7"/><rect x="14" y="14" width="7" height="7"/><rect x="3" y="14" width="7" height="7"/></svg>
          </div>
          <div class="opt-title">Preset</div>
          <div class="opt-desc">Z manifestu silnika.</div>
        </div>
        <div class="opt-card${useCustom ? ' active' : ''}" data-model-mode="custom">
          <div class="opt-icon">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="17 8 12 3 7 8"/><line x1="12" y1="3" x2="12" y2="15"/></svg>
          </div>
          <div class="opt-title">Custom HF repo</div>
          <div class="opt-desc">Dowolny <code>org/repo</code>.</div>
        </div>
      </div>
      <div data-preset-list style="${useCustom ? 'display:none;' : ''}">
        <div class="preset-list">
          ${renderPresetCards(initialPresetId)}
        </div>
      </div>
      <div data-custom-repo style="${useCustom ? '' : 'display:none;'}">
        <div class="form-row">
          <label>HuggingFace repo</label>
          <input class="tf-input mono" data-custom-repo-input placeholder="org/repo"
            value="${escapeAttr(initialModelRepo)}">
        </div>
      </div>
    </div>`;
}

function renderPresetCards(initialPresetId) {
  // Lista presetów obecnie hardkodowana — w pełnej wersji ładujemy z
  // /api/services/manifest/:engine_id (HTTP, JSON, OK bo to read-only
  // queryczny katalog manifestow). MVP: same te które już masz w DB.
  const presets = [
    { id: 'qwen3-5-9b-nvfp4', name: 'Qwen3.5-9B-NVFP4', repo: 'AxionML/Qwen3.5-9B-NVFP4', vram: '~8.4 GiB', tags: ['recommended', '4-bit NVFP4'] },
    { id: 'qwen3-5-0-8b-nvfp4', name: 'Qwen3.5-0.8B-NVFP4', repo: 'AxionML/Qwen3.5-0.8B-NVFP4', vram: '~1.2 GiB', tags: ['4-bit NVFP4'] },
    { id: 'deepseek-r1-distill-qwen-7b', name: 'DeepSeek-R1-Distill-Qwen-7B', repo: 'deepseek-ai/DeepSeek-R1-Distill-Qwen-7B', vram: '~14 GiB FP16', tags: ['reasoning'] },
  ];
  return presets
    .map((p) => {
      const sel = p.id === initialPresetId || (!initialPresetId && p.tags.includes('recommended'));
      const recPill = p.tags.includes('recommended')
        ? '<span class="preset-rec">RECOMMENDED</span>'
        : '';
      return `
        <div class="preset-card${sel ? ' selected' : ''}" data-preset-id="${escapeAttr(p.id)}">
          <div class="preset-radio"></div>
          <div class="preset-info">
            <div class="preset-name">${escapeHtml(p.name)} ${recPill}</div>
            <div class="preset-meta">
              <span>${escapeHtml(p.repo)}</span><span class="sep">·</span>
              <span>${escapeHtml(p.vram)}</span>
            </div>
          </div>
        </div>`;
    })
    .join('');
}

function renderVramSection(cfg) {
  const muVal = typeof cfg.gpu_memory_utilization === 'number' ? cfg.gpu_memory_utilization : 0.9;
  return `
    <div class="body-section">
      <div class="body-section-title">Alokacja VRAM</div>
      <div class="adv-section">
        <div class="adv-extern" data-vram-bar style="margin-top:0;">
          <div style="font-size:11px;color:var(--text-3);">Ładowanie GPU snapshot…</div>
        </div>
        <div style="margin-top:14px;">
          <label style="display:flex;justify-content:space-between;font-size:11.5px;font-weight:600;color:var(--text-2);">
            <span>gpu_memory_utilization</span>
            <span data-mu-value style="color:var(--accent-2);font-family:'JetBrains Mono',monospace;font-weight:700;">${muVal.toFixed(2)}</span>
          </label>
          <input type="range" data-mu-slider min="0.10" max="0.95" step="0.01" value="${muVal}" style="width:100%;margin-top:6px;">
          <div style="display:flex;justify-content:space-between;font-size:10.5px;color:var(--text-3);margin-top:4px;padding:0 8px;">
            <span>0.10</span><span>0.30</span><span>0.50</span><span>0.70</span><span>0.95</span>
          </div>
        </div>
      </div>
    </div>`;
}

function renderEngineArgsSection(cfg) {
  return `
    <div class="body-section">
      <div class="body-section-title">Parametry runtime (vLLM)</div>
      <div class="adv-row-2">
        <div class="form-row">
          <label>Max model len</label>
          <input class="tf-input mono" data-max-model-len value="${escapeAttr(cfg.max_model_len ?? '')}" placeholder="32768">
        </div>
        <div class="form-row">
          <label>Max num seqs</label>
          <input class="tf-input mono" data-max-num-seqs value="${escapeAttr(cfg.max_num_seqs ?? '')}" placeholder="1">
        </div>
        <div class="form-row">
          <label>Max num batched tokens</label>
          <input class="tf-input mono" data-max-batched value="${escapeAttr(cfg.max_num_batched_tokens ?? '')}" placeholder="8192">
        </div>
        <div class="form-row">
          <label>KV cache dtype</label>
          <select class="tf-select" data-kv-dtype>
            <option value="">(default)</option>
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
          <span>Enable chunked prefill</span>
        </label>
      </div>
    </div>`;
}

function renderGenericArgsSection(_cfg) {
  return `
    <div class="body-section">
      <div class="body-section-title">Parametry runtime</div>
      <div class="form-row">
        <label>Edycja typed parameters dostępna tylko dla vLLM. Dla pozostałych silników skorzystaj z deploy wizard'a (delete + create).</label>
      </div>
    </div>`;
}

// =============================================================================
// Plik: modules/catalog/nim-deploy.js
// Opis: Modal deploymentu kontenera NVIDIA NIM. Generuje docker-compose YAML,
//       wysyla przez WebSocket /ws/deploy, streamuje progress + logi.
//       Obsluguje blad EULA z linkiem do akceptacji licencji + retry.
// =============================================================================

import { escapeHtml, escapeAttr, toast, apiGet } from '/js/utils.js';
import { I18n } from '/js/i18n.js';

/// Otwiera modal deploy dla wybranego kontenera NIM (z `/api/nim/catalog`).
/// `preselectedNode` (opcjonalnie) — node z mesh detail kontekstu.
export async function openNimDeployModal(container, preselectedNode = null) {
  const existing = document.getElementById('nim-deploy-modal');
  if (existing) existing.remove();

  let nodes = [];
  try {
    const resp = await apiGet('/api/mesh/nodes');
    nodes = (resp || []).filter((n) => n.is_trusted === true || n.is_local === true);
  } catch {
    // ignore
  }

  const overlay = document.createElement('div');
  overlay.className = 'modal-backdrop active';
  overlay.id = 'nim-deploy-modal';
  overlay.innerHTML = `
    <div class="modal" style="max-width: 640px;">
      <div class="modal-header">
        <h3>${escapeHtml(I18n.t('nim.deploy_title'))}: ${escapeHtml(container.display_name || container.name)}</h3>
        <button class="modal-close" id="nim-modal-close">×</button>
      </div>
      <div class="modal-body">
        <div class="nim-summary">
          <div class="nim-summary-row">
            <span class="label">Image:</span>
            <span class="value mono">${escapeHtml(container.image)}:${escapeHtml(container.latest_tag || 'latest')}</span>
          </div>
          ${container.min_gpu_memory_gb ? `
            <div class="nim-summary-row">
              <span class="label">${escapeHtml(I18n.t('nim.vram'))}:</span>
              <span class="value">${container.min_gpu_memory_gb} GB</span>
            </div>` : ''}
        </div>

        <div class="nim-deploy-grid">
          <div class="form-group">
            <label>${escapeHtml(I18n.t('wizard.targetNode'))}</label>
            <select class="input" id="nim-target-node">
              ${nodes.map((n) => {
                const nid = n.node_id || n.id;
                const label = n.hostname || nid;
                const sel = nid === preselectedNode ? 'selected' : '';
                return `<option value="${escapeAttr(nid)}" ${sel}>${escapeHtml(label)}</option>`;
              }).join('')}
            </select>
          </div>
          <div class="form-group">
            <label>Port</label>
            <input type="number" class="input" id="nim-port" value="8000" min="1" max="65535">
          </div>
          <div class="form-group">
            <label>GPU</label>
            <select class="input" id="nim-gpu">
              <option value="all">All GPUs</option>
              <option value="0">GPU 0</option>
            </select>
          </div>
          <div class="form-group">
            <label>${escapeHtml(I18n.t('wizard.containerName'))}</label>
            <input type="text" class="input" id="nim-container-name" value="nim-${escapeAttr(container.name)}">
          </div>
        </div>

        <div class="form-group" style="margin-top:14px;">
          <label>${escapeHtml(I18n.t('nim.env_vars'))}</label>
          <div id="nim-env-vars">
            <div class="nim-env-row">
              <input type="text" class="input nim-env-key" placeholder="Key">
              <input type="text" class="input nim-env-value" placeholder="Value">
            </div>
          </div>
          <button class="btn btn-ghost btn-sm" id="nim-add-env" style="margin-top:6px;">+ ${escapeHtml(I18n.t('nim.add_env'))}</button>
        </div>

        <div id="nim-deploy-result"></div>
      </div>
      <div class="modal-footer">
        <button class="btn btn-ghost" id="nim-modal-cancel">${escapeHtml(I18n.t('common.cancel'))}</button>
        <button class="btn btn-primary" id="nim-modal-deploy">${escapeHtml(I18n.t('nim.deploy'))}</button>
      </div>
    </div>
  `;
  document.body.appendChild(overlay);

  const nodeSelect = document.getElementById('nim-target-node');
  const gpuSelect = document.getElementById('nim-gpu');

  async function loadNodeGpus(nodeId) {
    if (!nodeId) return;
    try {
      const data = await apiGet(`/api/mesh/nodes/${encodeURIComponent(nodeId)}`);
      const gpus = Array.isArray(data?.gpu_info) ? data.gpu_info : [];
      if (gpus.length > 0) {
        gpuSelect.innerHTML = '<option value="all">All GPUs</option>' +
          gpus.map((g, i) => {
            const idx = g.index ?? i;
            const vram = g.vram_total_mb ? Math.round(g.vram_total_mb / 1024) + ' GB' : '';
            return `<option value="${idx}">GPU ${idx}: ${escapeHtml(g.name || '')}${vram ? ` (${vram})` : ''}</option>`;
          }).join('');
      }
    } catch {
      // ignore
    }
  }

  if (preselectedNode) loadNodeGpus(preselectedNode);
  else if (nodes.length > 0) loadNodeGpus(nodes[0].node_id || nodes[0].id);

  nodeSelect?.addEventListener('change', () => loadNodeGpus(nodeSelect.value));

  document.getElementById('nim-add-env')?.addEventListener('click', () => {
    const envContainer = document.getElementById('nim-env-vars');
    const row = document.createElement('div');
    row.className = 'nim-env-row';
    row.innerHTML = `
      <input type="text" class="input nim-env-key" placeholder="Key">
      <input type="text" class="input nim-env-value" placeholder="Value">
    `;
    envContainer.appendChild(row);
  });

  const close = () => overlay.remove();
  document.getElementById('nim-modal-close')?.addEventListener('click', close);
  document.getElementById('nim-modal-cancel')?.addEventListener('click', close);

  document.getElementById('nim-modal-deploy')?.addEventListener('click', () => {
    executeDeploy(container, overlay);
  });
}

// ---- Deploy execution -----------------------------------------------------

async function executeDeploy(container, overlay) {
  const deployBtn = document.getElementById('nim-modal-deploy');
  const resultEl = document.getElementById('nim-deploy-result');
  if (deployBtn) deployBtn.disabled = true;

  const targetNodeId = document.getElementById('nim-target-node').value;
  const port = parseInt(document.getElementById('nim-port').value, 10) || 8000;
  const gpuId = document.getElementById('nim-gpu').value;
  const rawName = document.getElementById('nim-container-name').value.trim() || `nim-${container.name}`;
  const containerName = rawName.toLowerCase().replace(/[^a-z0-9_-]/g, '-').replace(/-+/g, '-').replace(/^-|-$/g, '');

  if (!targetNodeId) {
    toast(I18n.t('nim.select_node'), 'error');
    if (deployBtn) deployBtn.disabled = false;
    return;
  }

  const envVars = {};
  document.querySelectorAll('.nim-env-row').forEach((row) => {
    const key = row.querySelector('.nim-env-key')?.value.trim();
    const val = row.querySelector('.nim-env-value')?.value || '';
    if (key) envVars[key] = val;
  });

  const tag = container.latest_tag || 'latest';
  const image = `${container.image}:${tag}`;
  const yaml = buildComposeYaml({ containerName, image, port, gpuId, envVars });

  const startTime = Date.now();
  let timerInterval = null;
  const logs = [];

  if (resultEl) {
    resultEl.innerHTML = renderProgress('connecting', I18n.t('nim.connecting'), null, logs, startTime);
  }
  timerInterval = setInterval(() => {
    const t = resultEl?.querySelector('.deploy-timer');
    if (t) t.textContent = `${Math.floor((Date.now() - startTime) / 1000)}s`;
  }, 1000);

  const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
  const jwt = localStorage.getItem('tentaflow_jwt');
  const wsUrl = `${protocol}//${location.host}/ws/deploy`;
  const ws = jwt ? new WebSocket(wsUrl, [`bearer.${jwt}`]) : new WebSocket(wsUrl);

  ws.onopen = () => {
    ws.send(JSON.stringify({
      node_id: targetNodeId,
      stack_name: containerName,
      compose_yaml: yaml,
      service_name: containerName,
      config_json: JSON.stringify({
        engine: 'nim',
        model_id: container.name,
        port,
        container_name: containerName,
        service_type: container.category || 'llm',
        image,
      }),
    }));
  };

  ws.onmessage = (e) => {
    try {
      const msg = JSON.parse(e.data);
      if (msg.message) logs.push(msg.message);
      if (resultEl) {
        resultEl.innerHTML = renderProgress(msg.phase, msg.message || '', msg, logs, startTime);
        const logBox = resultEl.querySelector('.deploy-log-box');
        if (logBox) logBox.scrollTop = logBox.scrollHeight;
      }
      if (msg.phase === 'done') {
        clearInterval(timerInterval);
        ws.close();
        if (msg.success) {
          toast(`${container.display_name || container.name} deployed`, 'success');
          setTimeout(() => overlay.remove(), 1800);
        } else {
          handleDeployError(msg.error || 'Unknown error', container, overlay, resultEl);
        }
        if (deployBtn) deployBtn.disabled = false;
      }
    } catch {
      // ignore parse errors
    }
  };

  ws.onerror = () => {
    clearInterval(timerInterval);
    if (resultEl) {
      resultEl.innerHTML = `<div class="deploy-fail">${escapeHtml(I18n.t('nim.ws_error'))}</div>`;
    }
    toast(I18n.t('nim.ws_error'), 'error');
    if (deployBtn) deployBtn.disabled = false;
  };

  ws.onclose = () => {
    clearInterval(timerInterval);
    if (deployBtn) deployBtn.disabled = false;
  };
}

function handleDeployError(errMsg, container, overlay, resultEl) {
  // EULA error — pokaz link do akceptacji + Retry
  const eulaIndicators = ['EULA', 'accept license', 'Get Container'];
  const isEula = eulaIndicators.some((ind) => errMsg.includes(ind));
  if (isEula && resultEl) {
    const eulaUrl = `https://build.nvidia.com/${container.name}`;
    resultEl.innerHTML = `
      <div class="nim-eula-block">
        <div class="title">${escapeHtml(I18n.t('nim.eula_title'))}</div>
        <div class="desc">${escapeHtml(I18n.t('nim.eula_desc'))}</div>
        <div class="actions">
          <a href="${escapeAttr(eulaUrl)}" target="_blank" rel="noopener" class="btn btn-primary btn-sm">${escapeHtml(I18n.t('nim.accept_license'))} ↗</a>
          <button class="btn btn-secondary btn-sm" id="nim-retry-deploy">${escapeHtml(I18n.t('nim.retry'))}</button>
        </div>
      </div>
    `;
    document.getElementById('nim-retry-deploy')?.addEventListener('click', () => executeDeploy(container, overlay));
  } else {
    toast(`Error: ${errMsg}`, 'error');
  }
}

function buildComposeYaml({ containerName, image, port, gpuId, envVars }) {
  const gpuDevices = gpuId === 'all' ? '' : gpuId;
  const envBlock = Object.entries(envVars).map(([k, v]) => `      ${k}: "${v}"`).join('\n');
  return `services:
  ${containerName}:
    image: ${image}
    container_name: ${containerName}
    ports:
      - "${port}:8000"
    environment:
      NGC_API_KEY: "\${NGC_API_KEY}"
${gpuDevices ? `      NVIDIA_VISIBLE_DEVICES: "${gpuDevices}"` : ''}
${envBlock ? '\n' + envBlock : ''}
    deploy:
      resources:
        reservations:
          devices:
            - driver: nvidia
              count: ${gpuId === 'all' ? 'all' : '1'}
              capabilities: [gpu]
    restart: unless-stopped
`;
}

function renderProgress(phase, message, msg, logs, startTime) {
  const elapsed = Math.floor((Date.now() - startTime) / 1000);
  const phaseLabel = phaseLabelFor(phase);
  const success = phase === 'done' && msg?.success;
  const failed = phase === 'done' && msg && !msg.success;
  const cls = failed ? 'fail' : (success ? 'success' : 'progress');
  return `
    <div class="deploy-progress ${cls}">
      <div class="deploy-head">
        <span class="phase">${escapeHtml(phaseLabel)}</span>
        <span class="deploy-timer">${elapsed}s</span>
      </div>
      <div class="deploy-msg">${escapeHtml(message || '')}</div>
      ${logs.length > 0 ? `<pre class="deploy-log-box">${logs.map((l) => escapeHtml(l)).join('\n')}</pre>` : ''}
    </div>
  `;
}

function phaseLabelFor(phase) {
  const map = {
    connecting: I18n.t('nim.phase_connecting'),
    pulling: I18n.t('nim.phase_pulling'),
    starting: I18n.t('nim.phase_starting'),
    waiting: I18n.t('nim.phase_waiting'),
    done: I18n.t('nim.phase_done'),
  };
  return map[phase] || phase || '';
}

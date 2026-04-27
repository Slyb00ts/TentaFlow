// =============================================================================
// Plik: modules/catalog/nim-deploy.js
// Opis: Modal deploymentu kontenera NVIDIA NIM. Generuje docker-compose YAML,
//       wysyla przez WebSocket /ws/deploy, streamuje progress + logi.
//       Obsluguje blad EULA z linkiem do akceptacji licencji + retry.
// =============================================================================

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';

/// Otwiera modal deploy dla wybranego kontenera NIM (z `/api/nim/catalog`).
/// `preselectedNode` (opcjonalnie) — node z mesh detail kontekstu.
export async function openNimDeployModal(container, preselectedNode = null) {
  const existing = document.getElementById('nim-deploy-modal');
  if (existing) existing.remove();

  let nodes = [];
  try {
    const resp = await ApiBinary.list('meshNodeListRequest', { arrayKey: 'nodes' });
    // MeshNodeInfo proto nie ma `is_trusted` — uzywamy `source==="trusted"`.
    nodes = (resp || []).filter((n) => n.is_local === true || n.source === 'trusted');
  } catch {
    // ignore
  }

  const backdrop = document.createElement('div');
  backdrop.className = 'tf-window-backdrop';
  backdrop.id = 'nim-deploy-backdrop';

  const overlay = document.createElement('tf-window');
  overlay.id = 'nim-deploy-modal';
  overlay.setAttribute('title', `${I18n.t('nim.deploy_title')}: ${container.display_name || container.name}`);
  overlay.setAttribute('buttons', 'close');
  overlay.setAttribute('initial-x', 'center');
  overlay.setAttribute('initial-y', 'center');
  overlay.setAttribute('width', '640');
  overlay.innerHTML = `
    <div slot="body">
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
          <tf-select id="nim-target-node" value="${escapeAttr(preselectedNode || '')}">
            ${nodes.map((n) => {
              const nid = n.node_id || n.id;
              const label = n.hostname || nid;
              return `<option value="${escapeAttr(nid)}">${escapeHtml(label)}</option>`;
            }).join('')}
          </tf-select>
        </div>
        <div class="form-group">
          <tf-input type="number" id="nim-port" label="Port" value="8000"></tf-input>
        </div>
        <div class="form-group">
          <label>GPU</label>
          <tf-select id="nim-gpu" value="all">
            <option value="all">All GPUs</option>
            <option value="0">GPU 0</option>
          </tf-select>
        </div>
        <div class="form-group">
          <tf-input type="text" id="nim-container-name"
            label="${escapeAttr(I18n.t('wizard.containerName'))}"
            value="nim-${escapeAttr(container.name)}"></tf-input>
        </div>
      </div>

      <div class="form-group" style="margin-top:14px;">
        <label>${escapeHtml(I18n.t('nim.env_vars'))}</label>
        <div id="nim-env-vars">
          <div class="nim-env-row">
            <tf-input type="text" class="nim-env-key" placeholder="Key"></tf-input>
            <tf-input type="text" class="nim-env-value" placeholder="Value"></tf-input>
          </div>
        </div>
        <tf-button variant="ghost" size="sm" id="nim-add-env" style="margin-top:6px;">+ ${escapeHtml(I18n.t('nim.add_env'))}</tf-button>
      </div>

      <div id="nim-deploy-result"></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" id="nim-modal-deploy" data-action="deploy">${escapeHtml(I18n.t('nim.deploy'))}</tf-button>
    </div>
  `;
  document.body.appendChild(backdrop);
  document.body.appendChild(overlay);

  const nodeSelect = document.getElementById('nim-target-node');
  const gpuSelect = document.getElementById('nim-gpu');

  async function loadNodeGpus(nodeId) {
    if (!nodeId) return;
    try {
      const data = await ApiBinary.one('meshNodeDetailRequest', { nodeId });
      const node = data?.node;
      const gpus = Array.isArray(node?.gpus) ? node.gpus : [];
      if (gpus.length > 0) {
        const inner = gpuSelect.querySelector('select');
        if (!inner) return;
        const options = ['<option value="all">All GPUs</option>'];
        gpus.forEach((gpu, idx) => {
          const vram = gpu.vramTotalMb ? Math.round(gpu.vramTotalMb / 1024) + ' GB' : '';
          options.push(`<option value="${idx}">GPU ${idx}: ${escapeHtml(gpu.name || '')}${vram ? ` (${vram})` : ''}</option>`);
        });
        inner.innerHTML = options.join('');
        gpuSelect.setAttribute('value', 'all');
      }
    } catch {
      // ignore
    }
  }

  if (preselectedNode) loadNodeGpus(preselectedNode);
  else if (nodes.length > 0) loadNodeGpus(nodes[0].node_id || nodes[0].id);

  nodeSelect?.addEventListener('change', (e) => loadNodeGpus(e.detail?.value ?? nodeSelect.value));

  document.getElementById('nim-add-env')?.addEventListener('click', () => {
    const envContainer = document.getElementById('nim-env-vars');
    const row = document.createElement('div');
    row.className = 'nim-env-row';
    row.innerHTML = `
      <tf-input type="text" class="nim-env-key" placeholder="Key"></tf-input>
      <tf-input type="text" class="nim-env-value" placeholder="Value"></tf-input>
    `;
    envContainer.appendChild(row);
  });

  overlay.addEventListener('action', (e) => {
    if (e.detail?.action === 'deploy') {
      e.preventDefault();
      executeDeploy(container, overlay);
    }
    // cancel — standardowe zamkniecie
  });

  overlay.addEventListener('close-request', () => {
    backdrop.remove();
  });
  backdrop.addEventListener('click', () => {
    if (typeof overlay.close === 'function') overlay.close();
  });
}

// ---- Deploy execution -----------------------------------------------------

async function executeDeploy(container, overlay) {
  const deployBtn = document.getElementById('nim-modal-deploy');
  const resultEl = document.getElementById('nim-deploy-result');
  if (deployBtn) deployBtn.setAttribute('disabled', '');

  const targetNodeId = document.getElementById('nim-target-node').value;
  const port = parseInt(document.getElementById('nim-port').value, 10) || 8000;
  const gpuId = document.getElementById('nim-gpu').value;
  const rawName = String(document.getElementById('nim-container-name').value || '').trim() || `nim-${container.name}`;
  const containerName = rawName.toLowerCase().replace(/[^a-z0-9_-]/g, '-').replace(/-+/g, '-').replace(/^-|-$/g, '');

  if (!targetNodeId) {
    toast(I18n.t('nim.select_node'), 'error');
    if (deployBtn) deployBtn.removeAttribute('disabled');
    return;
  }

  const envVars = {};
  document.querySelectorAll('.nim-env-row').forEach((row) => {
    const keyEl = row.querySelector('.nim-env-key');
    const valEl = row.querySelector('.nim-env-value');
    const key = String(keyEl?.value || '').trim();
    const val = String(valEl?.value || '');
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
          setTimeout(() => {
            if (typeof overlay.close === 'function') overlay.close(true);
            else overlay.remove();
          }, 1800);
        } else {
          handleDeployError(msg.error || 'Unknown error', container, overlay, resultEl);
        }
        if (deployBtn) deployBtn.removeAttribute('disabled');
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
    if (deployBtn) deployBtn.removeAttribute('disabled');
  };

  ws.onclose = () => {
    clearInterval(timerInterval);
    if (deployBtn) deployBtn.removeAttribute('disabled');
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
          <a href="${escapeAttr(eulaUrl)}" target="_blank" rel="noopener" class="nim-eula-link">
            <tf-button variant="primary" size="sm">${escapeHtml(I18n.t('nim.accept_license'))} ↗</tf-button>
          </a>
          <tf-button variant="secondary" size="sm" id="nim-retry-deploy">${escapeHtml(I18n.t('nim.retry'))}</tf-button>
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

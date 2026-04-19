// =============================================================================
// Plik: modules/mesh-detail.js
// Opis: Drill-down view pojedynczego noda mesh. Topbar (back + hostname +
//       status + add service), system info row, VRAM summary (suma po GPU),
//       2-kolumnowy CPU + Memory, karty per-GPU (usage + VRAM + temp + power),
//       sekcja network interfaces, tabela kontenerow. Auto-refresh 2s gdy
//       tab widoczny, 5s gdy ukryty.
// =============================================================================

import {
  byId,
  escapeHtml,
  escapeAttr,
  apiGet,
  apiPost,
  toast,
  formatMb,
  formatBytes,
} from '/js/utils.js';
import { I18n } from '/js/i18n.js';

let currentNodeId = null;
let nodeData = null;
let refreshInterval = null;
let visibilityListener = null;
let wasDisconnected = false;
let lastFetchAt = null;

const MeshDetailScreen = {
  title: 'Node',
  async show(nodeId) {
    if (!nodeId) return;
    currentNodeId = nodeId;
    nodeData = null;
    wasDisconnected = false;
    lastFetchAt = null;

    const content = document.getElementById('content');
    if (!content) return;
    content.innerHTML = renderSkeleton();
    bindBack(content);

    await loadNode();
    renderDetail();

    setupRefresh();
    visibilityListener = () => setupRefresh();
    document.addEventListener('visibilitychange', visibilityListener);
  },
  cleanup() {
    if (refreshInterval) {
      clearInterval(refreshInterval);
      refreshInterval = null;
    }
    if (visibilityListener) {
      document.removeEventListener('visibilitychange', visibilityListener);
      visibilityListener = null;
    }
    currentNodeId = null;
    nodeData = null;
  },
};

// ---- Data ----------------------------------------------------------------

async function loadNode() {
  if (!currentNodeId) return;
  try {
    const data = await apiGet(`/api/mesh/nodes/${encodeURIComponent(currentNodeId)}`);
    const wasDc = wasDisconnected;
    nodeData = data;
    lastFetchAt = Date.now();
    wasDisconnected = false;
    if (wasDc) toast(I18n.t('mesh.reconnected'), 'success');
  } catch (err) {
    const age = lastFetchAt ? (Date.now() - lastFetchAt) / 1000 : Infinity;
    if (age > 30) wasDisconnected = true;
  }
}

function setupRefresh() {
  if (refreshInterval) clearInterval(refreshInterval);
  if (!currentNodeId) return;
  const interval = document.hidden ? 5000 : 2000;
  refreshInterval = setInterval(async () => {
    if (!currentNodeId) {
      MeshDetailScreen.cleanup();
      return;
    }
    await loadNode();
    if (currentNodeId) renderDetail();
  }, interval);
}

// ---- Helpers -------------------------------------------------------------

function bindBack(root) {
  root.addEventListener('click', (e) => {
    const back = e.target.closest('#btn-back-mesh');
    if (back) {
      MeshDetailScreen.cleanup();
      import('/js/router.js').then(({ Router }) => Router.navigate('mesh'));
    }
  });
}

function gaugeLevel(pct) {
  if (pct == null) return '';
  if (pct > 80) return 'hot';
  if (pct >= 50) return 'warm';
  return '';
}

// ---- Render --------------------------------------------------------------

function renderSkeleton() {
  return `
    <div class="mesh-detail">
      <div class="mesh-detail-topbar">
        <button class="btn btn-ghost btn-sm" id="btn-back-mesh">← ${escapeHtml(I18n.t('mesh.back_to_mesh'))}</button>
        <div class="mesh-detail-title"><span class="skeleton" style="display:inline-block;width:200px;height:24px;"></span></div>
      </div>
      <div class="mesh-detail-vram"><div class="skeleton" style="width:100%;height:32px;"></div></div>
      <div class="mesh-detail-grid">
        <div class="mesh-detail-card"><div class="skeleton" style="width:100%;height:120px;"></div></div>
        <div class="mesh-detail-card"><div class="skeleton" style="width:100%;height:120px;"></div></div>
      </div>
    </div>
  `;
}

function renderDetail() {
  const content = document.getElementById('content');
  if (!content) return;

  if (!nodeData) {
    content.innerHTML = `
      <div class="mesh-detail">
        <div class="mesh-detail-topbar">
          <button class="btn btn-ghost btn-sm" id="btn-back-mesh">← ${escapeHtml(I18n.t('mesh.back_to_mesh'))}</button>
        </div>
        <div class="empty-state"><div class="empty-state-text">${escapeHtml(I18n.t('mesh.load_error'))}</div></div>
      </div>
    `;
    bindBack(content);
    return;
  }

  const n = nodeData;
  const hostname = n.hostname || n.node_id?.slice(0, 12) || I18n.t('mesh.unknown_host');
  const online = isOnline(n);
  const statusChip = online
    ? `<span class="tag-status online">● ${escapeHtml(I18n.t('mesh.online'))}</span>`
    : `<span class="tag-status offline">● ${escapeHtml(I18n.t('mesh.offline'))}</span>`;

  const systemInfo = buildSystemInfo(n);
  const vramBar = buildVramSummary(n);
  const cpuMemory = buildCpuMemory(n);
  const gpuCards = buildGpuCards(n);
  const networkInterfaces = buildNetworkInterfaces(n);
  const containersTable = buildContainersTable(n);
  const modelsList = buildModelsList(n);

  const ageSec = lastFetchAt ? Math.round((Date.now() - lastFetchAt) / 1000) : 0;
  const freshness = ageSec < 5 ? '' : ageSec <= 30 ? ' stale' : ' disconnected';

  content.innerHTML = `
    <div class="mesh-detail${freshness}">
      <div class="mesh-detail-topbar">
        <button class="btn btn-ghost btn-sm" id="btn-back-mesh">← ${escapeHtml(I18n.t('mesh.back_to_mesh'))}</button>
        <div class="mesh-detail-title">
          <div class="name">${escapeHtml(hostname)}${n.is_local ? ` <span class="pill pill-local">${escapeHtml(I18n.t('mesh.local'))}</span>` : ''}</div>
          ${statusChip}
        </div>
        <div class="mesh-detail-actions"></div>
      </div>
      <div class="mesh-detail-sysinfo">${systemInfo}</div>
      ${vramBar}
      <div class="mesh-detail-grid">${cpuMemory}</div>
      ${gpuCards}
      ${networkInterfaces}
      ${modelsList}
      ${containersTable}
    </div>
  `;
  bindBack(content);
  bindContainerActions(content);
}

function isOnline(n) {
  const s = String(n.status || '').toLowerCase();
  if (n.is_local) return true;
  return s === 'connected' || s === 'online' || s === 'active' || s === 'ready';
}

function buildSystemInfo(n) {
  const parts = [];
  if (n.os_info) parts.push(escapeHtml(n.os_info));
  if (n.docker_version) parts.push(`Docker ${escapeHtml(n.docker_version)}`);
  const gpuSummary = Array.isArray(n.gpu_info) && n.gpu_info.length > 0
    ? n.gpu_info.map(g => g.name).filter(Boolean).join(', ')
    : null;
  if (gpuSummary) parts.push(escapeHtml(gpuSummary));
  if (parts.length === 0) return '<span class="muted">—</span>';
  return parts.map(p => `<span>${p}</span>`).join(' · ');
}

function buildVramSummary(n) {
  const gpus = Array.isArray(n.gpu_info) ? n.gpu_info : [];
  if (gpus.length === 0) {
    return `<div class="mesh-detail-vram muted">${escapeHtml(I18n.t('mesh.no_gpu'))}</div>`;
  }
  const used = gpus.reduce((s, g) => s + (g.vram_used_mb || 0), 0);
  const total = gpus.reduce((s, g) => s + (g.vram_total_mb || 0), 0);
  if (total === 0) {
    return `<div class="mesh-detail-vram muted">${escapeHtml(I18n.t('mesh.no_gpu'))}</div>`;
  }
  const pct = Math.round((used / total) * 100);
  return `
    <div class="mesh-detail-vram">
      <div class="mesh-detail-vram-head">
        <span class="label">${escapeHtml(I18n.t('mesh.vram_summary'))}</span>
        <span class="value">${formatMb(used)} / ${formatMb(total)} (${pct}%)</span>
      </div>
      <div class="bar"><div class="bar-fill ${gaugeLevel(pct)}" style="width:${pct}%"></div></div>
    </div>
  `;
}

function buildCpuMemory(n) {
  const cpuPct = n.cpu_usage ?? n.cpu_usage_percent;
  const cpuTemp = n.cpu_temperature_c;
  const ramUsed = n.ram_used_mb;
  const ramTotal = n.ram_total_mb;
  const swapUsed = n.swap_used_mb;
  const swapTotal = n.swap_total_mb;

  const cpuBar = cpuPct != null
    ? `<div class="bar"><div class="bar-fill ${gaugeLevel(cpuPct)}" style="width:${Math.min(100, Math.max(0, cpuPct))}%"></div></div>`
    : '';
  const cpuTempRow = cpuTemp != null
    ? `<div class="row"><span>${escapeHtml(I18n.t('mesh.temperature'))}</span><span>${Math.round(cpuTemp)}°C</span></div>`
    : '';
  const cpuCoresRow = n.cpu_count
    ? `<div class="row"><span>${escapeHtml(I18n.t('mesh.cores'))}</span><span>${n.cpu_count}</span></div>`
    : '';

  const ramPct = (ramUsed != null && ramTotal) ? Math.round((ramUsed / ramTotal) * 100) : null;
  const ramBar = ramPct != null
    ? `<div class="bar"><div class="bar-fill ${gaugeLevel(ramPct)}" style="width:${ramPct}%"></div></div>`
    : '';

  const swapRow = (swapTotal != null && swapTotal > 0)
    ? `
      <div class="row"><span>${escapeHtml(I18n.t('mesh.swap'))}</span><span>${formatMb(swapUsed || 0)} / ${formatMb(swapTotal)}</span></div>
      <div class="bar"><div class="bar-fill ${gaugeLevel(Math.round((swapUsed || 0) / swapTotal * 100))}" style="width:${Math.round((swapUsed || 0) / swapTotal * 100)}%"></div></div>
    `
    : '';

  return `
    <div class="mesh-detail-card">
      <div class="card-head">${escapeHtml(I18n.t('mesh.cpu'))} ${cpuPct != null ? `<span class="card-value">${Math.round(cpuPct)}%</span>` : ''}</div>
      ${cpuBar}
      ${cpuCoresRow}
      ${cpuTempRow}
    </div>
    <div class="mesh-detail-card">
      <div class="card-head">${escapeHtml(I18n.t('mesh.memory'))} ${ramUsed != null && ramTotal ? `<span class="card-value">${formatMb(ramUsed)} / ${formatMb(ramTotal)}</span>` : ''}</div>
      ${ramBar}
      ${swapRow}
    </div>
  `;
}

function buildGpuCards(n) {
  const gpus = Array.isArray(n.gpu_info) ? n.gpu_info : [];
  if (gpus.length === 0) return '';
  const cards = gpus.map((g, idx) => {
    const usage = g.usage_percent ?? 0;
    const vramPct = g.vram_total_mb ? Math.round((g.vram_used_mb / g.vram_total_mb) * 100) : 0;
    const power = (g.power_draw_w != null && g.power_limit_w)
      ? `${Math.round(g.power_draw_w)}W / ${Math.round(g.power_limit_w)}W`
      : (g.power_draw_w != null ? `${Math.round(g.power_draw_w)}W` : '—');
    return `
      <div class="mesh-detail-card gpu-card">
        <div class="card-head">GPU ${idx}: ${escapeHtml(g.name || '—')}</div>
        <div class="row"><span>${escapeHtml(I18n.t('mesh.usage'))}</span><span>${Math.round(usage)}%</span></div>
        <div class="bar"><div class="bar-fill ${gaugeLevel(usage)}" style="width:${Math.min(100, usage)}%"></div></div>
        <div class="row"><span>VRAM</span><span>${formatMb(g.vram_used_mb || 0)} / ${formatMb(g.vram_total_mb || 0)}</span></div>
        <div class="bar"><div class="bar-fill ${gaugeLevel(vramPct)}" style="width:${vramPct}%"></div></div>
        <div class="row"><span>${escapeHtml(I18n.t('mesh.temperature'))}</span><span>${g.temperature_c != null ? g.temperature_c + '°C' : '—'}</span></div>
        <div class="row"><span>${escapeHtml(I18n.t('mesh.power'))}</span><span>${power}</span></div>
      </div>
    `;
  }).join('');
  return `
    <h3 class="mesh-section-title">${escapeHtml(I18n.t('mesh.gpu_section'))}</h3>
    <div class="mesh-detail-gpu-grid">${cards}</div>
  `;
}

function buildNetworkInterfaces(n) {
  const ifaces = Array.isArray(n.network_interfaces) ? n.network_interfaces : [];
  if (ifaces.length === 0) return '';
  const rows = ifaces.map(i => {
    const up = i.link_up;
    const dot = up ? '<span class="net-dot up"></span>' : '<span class="net-dot down"></span>';
    const speed = i.speed_mbps ? (i.speed_mbps >= 1000 ? `${(i.speed_mbps / 1000).toFixed(0)}G` : `${i.speed_mbps}M`) : (up ? '—' : I18n.t('mesh.no_link'));
    const ip = i.ipv4_address || (up ? '—' : I18n.t('mesh.no_link'));
    const rx = i.rx_bytes_per_sec || 0;
    const tx = i.tx_bytes_per_sec || 0;
    const bw = `↓ ${formatBytes(rx)}/s · ↑ ${formatBytes(tx)}/s`;
    const badges = [];
    if (i.rdma_available) badges.push('<span class="net-badge rdma">RDMA</span>');
    if (i.numa_node != null) badges.push(`<span class="net-badge numa">NUMA${i.numa_node}</span>`);
    return `
      <div class="net-row">
        ${dot}
        <span class="net-name">${escapeHtml(i.name || '—')}</span>
        <span class="net-speed">${escapeHtml(speed)}</span>
        <span class="net-ip">${escapeHtml(ip)}</span>
        <span class="net-bw">${escapeHtml(bw)}</span>
        <span class="net-badges">${badges.join('')}</span>
      </div>
    `;
  }).join('');
  return `
    <h3 class="mesh-section-title">${escapeHtml(I18n.t('mesh.network_section'))}</h3>
    <div class="mesh-detail-card network-card">${rows}</div>
  `;
}

function buildModelsList(n) {
  const models = Array.isArray(n.models) ? n.models : [];
  if (models.length === 0) return '';
  const rows = models.map(m => {
    return `
      <div class="model-row">
        <span class="model-kind">${escapeHtml(m.kind || '—')}</span>
        <span class="model-alias"><code>${escapeHtml(m.alias || '—')}</code></span>
        <span class="model-backend">${escapeHtml(m.backend || '—')}</span>
        ${m.size_mb ? `<span class="model-size">${formatMb(m.size_mb)}</span>` : ''}
        ${m.loaded ? `<span class="tag-status online">● ${escapeHtml(I18n.t('mesh.loaded'))}</span>` : `<span class="tag-status offline">${escapeHtml(I18n.t('mesh.unloaded'))}</span>`}
      </div>
    `;
  }).join('');
  return `
    <h3 class="mesh-section-title">${escapeHtml(I18n.t('mesh.models_section'))}<span class="section-count">${models.length}</span></h3>
    <div class="mesh-detail-card models-card">${rows}</div>
  `;
}

function buildContainersTable(n) {
  const containers = Array.isArray(n.containers) ? n.containers : [];
  if (containers.length === 0) return '';
  const rows = containers.map(c => {
    const statusLower = String(c.status || '').toLowerCase();
    const running = statusLower.includes('up') || statusLower.includes('running');
    const statusClass = running ? 'running' : (statusLower.includes('exited') ? 'exited' : '');
    const cpuPct = c.cpu_percent != null ? `${c.cpu_percent.toFixed(1)}%` : '—';
    const mem = c.memory_limit_mb
      ? `${formatMb(c.memory_mb || 0)} / ${formatMb(c.memory_limit_mb)}`
      : formatMb(c.memory_mb || 0);
    const actions = running
      ? `<button class="btn btn-ghost btn-sm" data-container-action="stop" data-container-name="${escapeAttr(c.name)}">${escapeHtml(I18n.t('mesh.stop'))}</button>
         <button class="btn btn-ghost btn-sm" data-container-action="restart" data-container-name="${escapeAttr(c.name)}">${escapeHtml(I18n.t('mesh.restart'))}</button>`
      : `<button class="btn btn-ghost btn-sm" data-container-action="start" data-container-name="${escapeAttr(c.name)}">${escapeHtml(I18n.t('mesh.start'))}</button>`;
    return `
      <tr>
        <td><strong>${escapeHtml(c.name || '—')}</strong></td>
        <td><code>${escapeHtml(c.image || '—')}</code></td>
        <td><span class="container-status ${statusClass}">${escapeHtml(c.status || '—')}</span></td>
        <td>${escapeHtml(cpuPct)}</td>
        <td>${escapeHtml(mem)}</td>
        <td class="actions">${actions}</td>
      </tr>
    `;
  }).join('');
  return `
    <h3 class="mesh-section-title">${escapeHtml(I18n.t('mesh.containers'))}<span class="section-count">${containers.length}</span></h3>
    <div class="mesh-detail-card">
      <table class="data-table">
        <thead>
          <tr>
            <th>${escapeHtml(I18n.t('mesh.container_name'))}</th>
            <th>${escapeHtml(I18n.t('mesh.container_image'))}</th>
            <th>${escapeHtml(I18n.t('mesh.container_status'))}</th>
            <th>CPU</th>
            <th>RAM</th>
            <th>${escapeHtml(I18n.t('mesh.container_actions'))}</th>
          </tr>
        </thead>
        <tbody>${rows}</tbody>
      </table>
    </div>
  `;
}

function bindContainerActions(root) {
  root.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-container-action]');
    if (!btn) return;
    const action = btn.dataset.containerAction;
    const name = btn.dataset.containerName;
    if (!action || !name || !currentNodeId) return;
    try {
      await apiPost(`/api/mesh/nodes/${encodeURIComponent(currentNodeId)}/command`, {
        command: `container.${action}`,
        args: { name },
      });
      toast(`${action}: ${name}`, 'success');
      await loadNode();
      renderDetail();
    } catch (err) {
      toast(`${action} ${name}: ${err.message}`, 'error');
    }
  });
}

export default MeshDetailScreen;

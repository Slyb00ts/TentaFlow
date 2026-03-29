// =============================================================================
// Plik: MeshNodeDetail.js
// Opis: Widok szczegolow noda mesh — topbar z akcjami, info grid, metryki
//       z gauge bars, tabela kontenerow z akcjami, modal logow.
// Przyklad: MeshNodeDetail.show('node-id-123');
// =============================================================================

const MeshNodeDetail = (() => {
  'use strict';

  let currentNodeId = null;
  let nodeData = null;
  let refreshInterval = null;
  let boundHandleAction = null;

  const platformIcons = {
    linux: '\uD83D\uDC27',
    macos: '\uD83C\uDF4E',
    windows: '\uD83E\uDE9F',
    android: '\uD83D\uDCF1',
    ios: '\uD83D\uDCF1',
    unknown: '\uD83D\uDDA5\uFE0F'
  };

  // Klasa koloru gauge wg procentu
  function gaugeLevel(pct) {
    if (pct > 80) return 'high';
    if (pct >= 50) return 'medium';
    return 'low';
  }

  // Gauge bar — wersja duza (detail view)
  function renderGaugeLg(label, value, pct) {
    const level = gaugeLevel(pct);
    return `
      <div class="gauge gauge--lg">
        <div class="gauge-header">
          <span class="gauge-label">${Utils.escapeHtml(label)}</span>
          <span class="gauge-value">${Utils.escapeHtml(value)}</span>
        </div>
        <div class="gauge-bar gauge-bar--lg"><div class="gauge-fill ${level}" style="width:${Math.min(pct, 100)}%"></div></div>
      </div>
    `;
  }

  // Pobranie danych noda z API
  async function loadNode() {
    try {
      nodeData = await ApiClient.get(`/api/mesh/nodes/${encodeURIComponent(currentNodeId)}`);
    } catch (e) {
      nodeData = null;
    }
  }

  // Otwarcie widoku szczegolow — podmienia zawartosc #content
  async function show(nodeId) {
    // Wyczysc poprzedni interval jesli byl otwarty inny node
    if (refreshInterval) {
      clearInterval(refreshInterval);
      refreshInterval = null;
    }

    currentNodeId = nodeId;
    const content = document.getElementById('content');
    if (!content) return;

    // Odmontuj interval Mesh (bez wywolywania cleanup na MeshNodeDetail)
    if (typeof Mesh !== 'undefined' && Mesh.unmountRefreshOnly) {
      Mesh.unmountRefreshOnly();
    }

    content.innerHTML = `<p>${I18n.t('common.loading')}</p>`;

    await loadNode();
    renderDetail();
    bindEvents();

    refreshInterval = setInterval(async () => {
      // Sprawdz czy nadal jestesmy na widoku mesh (nie przelaczono na inny)
      if (!currentNodeId) { cleanup(); return; }
      await loadNode();
      if (!currentNodeId) { cleanup(); return; }
      renderDetail();
      bindEvents();
    }, 10000);
  }

  // Zamkniecie widoku — powrot do Mesh
  function close() {
    cleanup();
    ViewRouter.navigate('mesh');
  }

  // Czyszczenie stanu
  function cleanup() {
    if (refreshInterval) {
      clearInterval(refreshInterval);
      refreshInterval = null;
    }
    const content = document.getElementById('content');
    if (content && boundHandleAction) {
      content.removeEventListener('click', boundHandleAction);
    }
    boundHandleAction = null;
    currentNodeId = null;
    nodeData = null;
  }

  // Skrocony UUID
  function shortId(id) {
    if (!id) return '-';
    return id.length > 12 ? id.substring(0, 12) + '...' : id;
  }

  // Formatowanie info o GPU
  function formatGpuSummary(gpuInfo) {
    if (!Array.isArray(gpuInfo) || gpuInfo.length === 0) return '-';
    const counts = {};
    gpuInfo.forEach(g => {
      const name = g.name || g.device_name || 'GPU';
      const vram = g.vram_total_mb ? ` (${Utils.formatMb(g.vram_total_mb)})` : '';
      const key = `${name}${vram}`;
      counts[key] = (counts[key] || 0) + 1;
    });
    return Object.entries(counts)
      .map(([name, count]) => count > 1 ? `${count}x ${name}` : name)
      .join(', ');
  }

  // Renderowanie widoku szczegolow
  function renderDetail() {
    const content = document.getElementById('content');
    if (!content) return;

    if (!nodeData) {
      content.innerHTML = `
        <div class="mesh-detail-topbar">
          <button class="btn btn-ghost btn-sm" id="btn-back-to-mesh">\u2190 ${I18n.t('mesh.back_to_mesh')}</button>
        </div>
        <div class="empty-state">
          <div class="empty-state-text">${I18n.t('common.error')}</div>
        </div>
      `;
      return;
    }

    const node = nodeData;
    const hostname = node.hostname || node.name || node.id || I18n.t('mesh.unknown_host');
    const nodeId = node.node_id || node.id || '';
    const isLocal = node.is_local || node.source === 'local';
    const isTrusted = !isLocal && ((node.trust_status || node.status || '').toLowerCase() === 'trusted' || (node.trust_status || node.status || '').toLowerCase() === 'paired' || node.source === 'trusted');
    const canManage = isLocal || isTrusted;

    // Status
    const statusRaw = node.status || node.state || 'unknown';
    const statusLower = statusRaw.toLowerCase();
    const isOnline = statusLower === 'connected' || statusLower === 'online' || statusLower === 'active' || statusLower === 'ready';
    const statusClass = isOnline ? 'running' : 'stopped';

    // System
    const osDisplay = node.os_info || node.platform || node.os || '-';
    const dockerVersion = node.docker_version || node.docker || '-';
    const gpuSummary = formatGpuSummary(node.gpu_info);

    // Topbar z akcjami
    const actionButtons = canManage ? `
      <div class="mesh-detail-topbar-actions">
        <button class="btn btn-primary btn-sm" id="btn-add-service">${I18n.t('mesh.add_service')}</button>
      </div>
    ` : '';

    // Info grid (3 kolumny)
    const infoGrid = `
      <div class="mesh-detail-info">
        <div class="mesh-detail-info-item">
          <span class="mesh-detail-info-label">NODE ID</span>
          <span class="mesh-detail-info-value">${Utils.escapeHtml(shortId(nodeId))}</span>
        </div>
        <div class="mesh-detail-info-item">
          <span class="mesh-detail-info-label">HOSTNAME</span>
          <span class="mesh-detail-info-value">${Utils.escapeHtml(hostname)}</span>
        </div>
        <div class="mesh-detail-info-item">
          <span class="mesh-detail-info-label">STATUS</span>
          <span class="mesh-detail-info-value"><span class="container-status ${statusClass}">${Utils.escapeHtml(statusRaw)}</span></span>
        </div>
        <div class="mesh-detail-info-item">
          <span class="mesh-detail-info-label">SYSTEM</span>
          <span class="mesh-detail-info-value">${Utils.escapeHtml(osDisplay)}</span>
        </div>
        <div class="mesh-detail-info-item">
          <span class="mesh-detail-info-label">DOCKER</span>
          <span class="mesh-detail-info-value">${Utils.escapeHtml(dockerVersion)}</span>
        </div>
        <div class="mesh-detail-info-item">
          <span class="mesh-detail-info-label">GPU</span>
          <span class="mesh-detail-info-value">${Utils.escapeHtml(gpuSummary)}</span>
        </div>
      </div>
    `;

    // Metryki (4 kolumny)
    const metrics = [];

    // CPU gauge
    const cpuPct = node.cpu_usage != null ? Math.round(node.cpu_usage) : (node.cpu_percent != null ? Math.round(node.cpu_percent) : null);
    if (cpuPct != null) {
      metrics.push(renderGaugeLg('CPU', `${cpuPct}%`, cpuPct));
    }

    // RAM gauge
    if (node.ram_used_mb != null && node.ram_total_mb != null && node.ram_total_mb > 0) {
      const ramPct = Math.round((node.ram_used_mb / node.ram_total_mb) * 100);
      metrics.push(renderGaugeLg('RAM', `${Utils.formatMb(node.ram_used_mb)} / ${Utils.formatMb(node.ram_total_mb)}`, ramPct));
    }

    // Per-GPU gauges
    const gpuInfo = Array.isArray(node.gpu_info) ? node.gpu_info : [];
    gpuInfo.forEach((gpu, idx) => {
      const gpuName = gpu.name || gpu.device_name || `GPU ${idx}`;
      const gpuUsage = gpu.usage_percent != null ? Math.round(gpu.usage_percent) : 0;
      let gpuHtml = renderGaugeLg(`GPU ${idx} (${gpuName})`, `${gpuUsage}%`, gpuUsage);
      // VRAM per GPU
      if (gpu.vram_used_mb != null && gpu.vram_total_mb != null && gpu.vram_total_mb > 0) {
        const vramPct = Math.round((gpu.vram_used_mb / gpu.vram_total_mb) * 100);
        gpuHtml += renderGaugeLg(`VRAM ${idx}`, `${Utils.formatMb(gpu.vram_used_mb)} / ${Utils.formatMb(gpu.vram_total_mb)}`, vramPct);
      }
      metrics.push(`<div class="mesh-detail-metric-group">${gpuHtml}</div>`);
    });

    // Network per interface
    if (Array.isArray(node.network_interfaces)) {
      node.network_interfaces.forEach(iface => {
        const name = iface.name || iface.interface || '?';
        const rx = iface.rx_bytes != null ? Utils.formatBytes(iface.rx_bytes) : '0 B/s';
        const tx = iface.tx_bytes != null ? Utils.formatBytes(iface.tx_bytes) : '0 B/s';
        metrics.push(`
          <div class="gauge gauge--lg">
            <div class="gauge-header">
              <span class="gauge-label">${Utils.escapeHtml(name)}</span>
              <span class="gauge-value">\u2193 ${rx} \u2191 ${tx}</span>
            </div>
          </div>
        `);
      });
    } else if (node.network_rx_bytes != null || node.network_tx_bytes != null) {
      const rx = node.network_rx_bytes != null ? Utils.formatBytes(node.network_rx_bytes) : '0 B/s';
      const tx = node.network_tx_bytes != null ? Utils.formatBytes(node.network_tx_bytes) : '0 B/s';
      metrics.push(`
        <div class="gauge gauge--lg">
          <div class="gauge-header">
            <span class="gauge-label">Network</span>
            <span class="gauge-value">\u2193 ${rx} \u2191 ${tx}</span>
          </div>
        </div>
      `);
    }

    // Kontenery
    const containers = node.containers || [];
    let containersHtml = '';
    if (containers.length === 0) {
      containersHtml = `<p class="mesh-detail-empty">${I18n.t('common.no_data')}</p>`;
    } else {
      containersHtml = `
        <div class="table-wrapper"><table class="mesh-detail-table">
          <thead><tr>
            <th>${I18n.t('mesh.container_name')}</th>
            <th>${I18n.t('mesh.container_image')}</th>
            <th>${I18n.t('mesh.container_status')}</th>
            <th>CPU%</th>
            <th>RAM</th>
            <th>${I18n.t('mesh.container_actions')}</th>
          </tr></thead>
          <tbody>${containers.map(c => renderContainerRow(c)).join('')}</tbody>
        </table></div>
      `;
    }

    content.innerHTML = `
      <div class="mesh-detail">
        <div class="mesh-detail-topbar">
          <button class="btn btn-ghost btn-sm" id="btn-back-to-mesh">\u2190 ${I18n.t('mesh.back_to_mesh')}</button>
          <span class="mesh-detail-topbar-hostname">${Utils.escapeHtml(hostname)}</span>
          ${actionButtons}
        </div>

        <div class="mesh-detail-section">
          <div class="mesh-detail-section-title">${I18n.t('mesh.info')}</div>
          ${infoGrid}
        </div>

        ${metrics.length > 0 ? `
        <div class="mesh-detail-section">
          <div class="mesh-detail-section-title">Metrics</div>
          <div class="mesh-detail-metrics">${metrics.join('')}</div>
        </div>
        ` : ''}

        <div class="mesh-detail-section">
          <div class="mesh-detail-section-title">${I18n.t('mesh.containers')}</div>
          ${containersHtml}
        </div>
      </div>
    `;
  }

  // Wiersz kontenera z CPU%, RAM i akcjami
  function renderContainerRow(c) {
    const name = c.name || c.Names || c.id || '-';
    const image = c.image || c.Image || '-';
    const status = c.status || c.State || c.state || '-';
    const containerId = c.id || c.Id || c.container_id || '';
    const statusLower = status.toLowerCase();
    const isRunning = statusLower.includes('up') || statusLower.includes('running');

    // Status badge
    const statusClass = isRunning ? 'running' : (statusLower.includes('exited') ? 'exited' : 'stopped');

    // CPU i RAM kontenera
    const cpuPct = c.cpu_percent != null ? c.cpu_percent.toFixed(1) + '%' : '-';
    const ramUsage = c.ram_used_mb != null ? Utils.formatMb(c.ram_used_mb) : (c.memory_usage || '-');

    // Akcje zalezne od stanu
    let actionButtons = '';
    if (isRunning) {
      actionButtons = `
        <button class="btn btn-ghost btn-xs" data-container-action="stop" data-container-id="${Utils.escapeAttr(containerId)}">${I18n.t('mesh.stop')}</button>
        <button class="btn btn-ghost btn-xs" data-container-action="restart" data-container-id="${Utils.escapeAttr(containerId)}">${I18n.t('mesh.restart')}</button>
        <button class="btn btn-ghost btn-xs" data-container-action="logs" data-container-id="${Utils.escapeAttr(containerId)}">${I18n.t('mesh.logs')}</button>
      `;
    } else {
      actionButtons = `
        <button class="btn btn-ghost btn-xs" data-container-action="start" data-container-id="${Utils.escapeAttr(containerId)}">${I18n.t('mesh.start')}</button>
        <button class="btn btn-ghost btn-xs" data-container-action="logs" data-container-id="${Utils.escapeAttr(containerId)}">${I18n.t('mesh.logs')}</button>
        <button class="btn btn-ghost btn-xs btn-danger-text" data-container-action="remove" data-container-id="${Utils.escapeAttr(containerId)}">${I18n.t('mesh.remove')}</button>
      `;
    }

    return `
      <tr>
        <td>${Utils.escapeHtml(name)}</td>
        <td>${Utils.escapeHtml(image)}</td>
        <td><span class="container-status ${statusClass}">${Utils.escapeHtml(status)}</span></td>
        <td>${Utils.escapeHtml(cpuPct)}</td>
        <td>${Utils.escapeHtml(ramUsage)}</td>
        <td class="container-actions">${actionButtons}</td>
      </tr>
    `;
  }

  // Podpiecie zdarzen
  function bindEvents() {
    const backBtn = document.getElementById('btn-back-to-mesh');
    if (backBtn) {
      backBtn.addEventListener('click', close);
    }

    const addSvcBtn = document.getElementById('btn-add-service');
    if (addSvcBtn) {
      addSvcBtn.addEventListener('click', () => {
        const nid = currentNodeId;
        cleanup(); // Zatrzymaj refresh ZANIM ServiceCatalog podmieni content
        ServiceCatalog.show(nid, 'mesh');
      });
    }

    // Delegacja akcji kontenerow
    const content = document.getElementById('content');
    if (content) {
      if (boundHandleAction) {
        content.removeEventListener('click', boundHandleAction);
      }
      boundHandleAction = handleContainerAction;
      content.addEventListener('click', boundHandleAction);
    }
  }

  // Obsluga akcji na kontenerze
  async function handleContainerAction(e) {
    const btn = e.target.closest('[data-container-action]');
    if (!btn) return;

    const action = btn.dataset.containerAction;
    const containerId = btn.dataset.containerId;
    if (!containerId) return;

    if (action === 'logs') {
      showContainerLogs(containerId);
      return;
    }

    const commandMap = {
      start: 'ContainerStart',
      stop: 'ContainerStop',
      restart: 'ContainerRestart',
      remove: 'ContainerRemove'
    };
    const command = commandMap[action];
    if (!command) return;

    if (action === 'remove' && !confirm(I18n.t('common.confirm_delete'))) return;

    btn.disabled = true;
    try {
      await ApiClient.post(`/api/mesh/nodes/${encodeURIComponent(currentNodeId)}/command`, {
        command: command,
        container_id: containerId
      });
      App.showToast(I18n.t('common.success'), 'success');
      await loadNode();
      renderDetail();
      bindEvents();
    } catch (err) {
      App.showToast(err.message || I18n.t('common.error'), 'error');
    }
  }

  // Modal z logami kontenera
  async function showContainerLogs(containerId) {
    try {
      const result = await ApiClient.post(`/api/mesh/nodes/${encodeURIComponent(currentNodeId)}/command`, {
        command: 'ContainerLogs',
        container_id: containerId,
        tail_lines: 200
      });
      const logs = result?.logs || result?.output || '';

      const overlay = document.createElement('div');
      overlay.className = 'modal-overlay active';
      overlay.innerHTML = `
        <div class="modal" style="max-width:900px;">
          <div class="modal-header">
            <h3>${I18n.t('mesh.logs')}</h3>
            <button class="modal-close" id="logs-modal-close">&times;</button>
          </div>
          <div class="modal-body">
            <pre class="mesh-detail-logs">${Utils.escapeHtml(logs || I18n.t('common.no_data'))}</pre>
          </div>
          <div class="modal-footer">
            <button class="btn btn-secondary" id="logs-modal-ok">${I18n.t('common.close')}</button>
          </div>
        </div>
      `;
      document.body.appendChild(overlay);

      const closeModal = () => { if (overlay.parentNode) overlay.remove(); };
      overlay.querySelector('#logs-modal-close').addEventListener('click', closeModal);
      overlay.querySelector('#logs-modal-ok').addEventListener('click', closeModal);
      overlay.addEventListener('click', (e) => { if (e.target === overlay) closeModal(); });
    } catch (err) {
      App.showToast(err.message || I18n.t('common.error'), 'error');
    }
  }



  return { show, close, cleanup };
})();

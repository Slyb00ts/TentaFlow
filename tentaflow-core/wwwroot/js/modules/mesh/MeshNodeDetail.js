// =============================================================================
// Plik: MeshNodeDetail.js
// Opis: Widok szczegolow noda mesh — topbar z metadanymi, VRAM summary,
//       karty CPU/RAM/GPU/Network ze sparkline, tabela kontenerow,
//       skeleton loading, visibility-aware refresh, stale/disconnected states.
// Przyklad: MeshNodeDetail.show('node-id-123');
// =============================================================================

const MeshNodeDetail = (() => {
  'use strict';

  let currentNodeId = null;
  let nodeData = null;
  let refreshInterval = null;
  let boundHandleAction = null;
  let lastFetchTime = null;
  let wasDisconnected = false;

  // Instancje sparkline do czyszczenia
  let sparklines = [];

  // Ikony SVG inline — monochromatyczne, styl Feather, 24x24 viewBox
  const MeshIcons = {
    thermometer: (size = 14) => `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14 14.76V3.5a2.5 2.5 0 0 0-5 0v11.26a4.5 4.5 0 1 0 5 0z"/></svg>`,
    bolt: (size = 14) => `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"/></svg>`,
    gear: (size = 14) => `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09a1.65 1.65 0 0 0-1.08-1.51 1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1.08z"/></svg>`,
    linux: (size = 16) => `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 2C9.24 2 7 5.37 7 9.5c0 2.1.56 4 1.46 5.34C7.18 15.77 5 17.26 5 19c0 1.66 2.69 3 6 3h2c3.31 0 6-1.34 6-3 0-1.74-2.18-3.23-3.46-4.16C16.44 13.5 17 11.6 17 9.5 17 5.37 14.76 2 12 2z"/><circle cx="10" cy="9" r="1" fill="currentColor" stroke="none"/><circle cx="14" cy="9" r="1" fill="currentColor" stroke="none"/><path d="M10 13h4"/></svg>`,
    macos: (size = 16) => `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M18.71 19.5c-.83 1.24-1.71 2.45-3.05 2.47-1.34.03-1.77-.79-3.29-.79-1.53 0-2 .77-3.27.82-1.31.05-2.3-1.32-3.14-2.53C4.25 17 2.94 12.45 4.7 9.39c.87-1.52 2.43-2.48 4.12-2.51 1.28-.02 2.5.87 3.29.87.78 0 2.26-1.07 3.8-.91.65.03 2.47.26 3.64 1.98-.09.06-2.17 1.28-2.15 3.81.03 3.02 2.65 4.03 2.68 4.04-.03.07-.42 1.44-1.38 2.83"/><path d="M13 3.5c.73-.83 1.94-1.46 2.94-1.5.13 1.17-.34 2.35-1.04 3.19-.69.85-1.83 1.51-2.95 1.42-.15-1.15.41-2.35 1.05-3.11z"/></svg>`,
    windows: (size = 16) => `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 12h8V3.3L3 5v7z"/><path d="M13 12h8V2l-8 1.3V12z"/><path d="M3 12v7l8 1.7V12H3z"/><path d="M13 12v8.7L21 22V12h-8z"/></svg>`,
    android: (size = 16) => `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="5" y="10" width="14" height="10" rx="2"/><path d="M8 10V7a4 4 0 0 1 8 0v3"/><circle cx="9.5" cy="6" r="0.5" fill="currentColor" stroke="none"/><circle cx="14.5" cy="6" r="0.5" fill="currentColor" stroke="none"/><line x1="7" y1="2" x2="9" y2="5"/><line x1="17" y1="2" x2="15" y2="5"/></svg>`,
    ios: (size = 16) => `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="5" y="2" width="14" height="20" rx="3"/><line x1="12" y1="18" x2="12.01" y2="18"/></svg>`,
    desktop: (size = 16) => `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="3" width="20" height="14" rx="2"/><line x1="8" y1="21" x2="16" y2="21"/><line x1="12" y1="17" x2="12" y2="21"/></svg>`,
  };

  // Mapowanie platform na ikony SVG
  const platformIconMap = {
    linux: MeshIcons.linux,
    macos: MeshIcons.macos,
    windows: MeshIcons.windows,
    android: MeshIcons.android,
    ios: MeshIcons.ios,
    unknown: MeshIcons.desktop,
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

  // Etykieta progu temperatury/uzycia
  function thresholdLabel(value, thresholds) {
    if (value == null) return '';
    if (value > thresholds.high) return `<span class="mesh-threshold-label mesh-threshold-crit">${I18n.t('mesh.threshold_crit')}</span>`;
    if (value > thresholds.medium) return `<span class="mesh-threshold-label mesh-threshold-high">${I18n.t('mesh.threshold_high')}</span>`;
    return `<span class="mesh-threshold-label mesh-threshold-ok">${I18n.t('mesh.threshold_ok')}</span>`;
  }

  // Swiezosc danych
  function getDataFreshness() {
    if (!lastFetchTime) return 'disconnected';
    const age = (Date.now() - lastFetchTime) / 1000;
    if (age < 5) return 'live';
    if (age <= 30) return 'stale';
    return 'disconnected';
  }

  // Klasa CSS stanu na sekcji
  function freshnessClass() {
    const state = getDataFreshness();
    if (state === 'stale') return ' stale';
    if (state === 'disconnected') return ' disconnected';
    return '';
  }

  // Pobranie danych noda z API
  async function loadNode() {
    try {
      nodeData = await ApiClient.get(`/api/mesh/nodes/${encodeURIComponent(currentNodeId)}`);
      const prevDisconnected = wasDisconnected;
      lastFetchTime = Date.now();
      wasDisconnected = false;
      if (prevDisconnected) {
        App.showToast(I18n.t('mesh.reconnected'), 'success');
      }
    } catch (e) {
      wasDisconnected = getDataFreshness() === 'disconnected';
    }
  }

  // Konfiguracja visibility-aware refresh (2s widoczny, 5s ukryty)
  function setupRefreshInterval() {
    if (refreshInterval) clearInterval(refreshInterval);

    const interval = document.hidden ? 5000 : 2000;
    refreshInterval = setInterval(async () => {
      if (!currentNodeId) { cleanup(); return; }
      await loadNode();
      if (!currentNodeId) { cleanup(); return; }
      renderDetail();
      bindEvents();
      updateSparklines();
    }, interval);
  }

  function handleVisibilityChange() {
    if (!currentNodeId) return;
    setupRefreshInterval();
  }

  // Otwarcie widoku szczegolow
  async function show(nodeId) {
    if (refreshInterval) {
      clearInterval(refreshInterval);
      refreshInterval = null;
    }
    destroySparklines();

    currentNodeId = nodeId;
    lastFetchTime = null;
    wasDisconnected = false;
    const content = document.getElementById('content');
    if (!content) return;

    if (typeof Mesh !== 'undefined' && Mesh.unmountRefreshOnly) {
      Mesh.unmountRefreshOnly();
    }

    // Skeleton loading
    content.innerHTML = renderSkeleton();
    bindBackButton();

    await loadNode();
    renderDetail();
    bindEvents();
    createSparklines();
    updateSparklines();

    document.addEventListener('visibilitychange', handleVisibilityChange);
    setupRefreshInterval();
  }

  // Zamkniecie widoku
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
    document.removeEventListener('visibilitychange', handleVisibilityChange);
    destroySparklines();
    const content = document.getElementById('content');
    if (content && boundHandleAction) {
      content.removeEventListener('click', boundHandleAction);
    }
    boundHandleAction = null;
    currentNodeId = null;
    nodeData = null;
    lastFetchTime = null;
    wasDisconnected = false;
  }

  // Zniszczenie instancji sparkline
  function destroySparklines() {
    sparklines.forEach(s => s.destroy());
    sparklines = [];
  }

  // Tworzenie instancji sparkline z canvasow w DOM
  function createSparklines() {
    destroySparklines();
    const canvases = document.querySelectorAll('canvas[data-sparkline]');
    canvases.forEach(canvas => {
      const type = canvas.dataset.sparkline;
      let opts = {};
      switch (type) {
        case 'cpu':
          opts = { maxValue: 100, thresholds: { medium: 60, high: 85 } };
          break;
        case 'ram':
          opts = { maxValue: 100, thresholds: { medium: 70, high: 90 } };
          break;
        case 'gpu-usage':
          opts = { maxValue: 100, thresholds: { medium: 70, high: 90 } };
          break;
        case 'gpu-vram':
          opts = { maxValue: 100, thresholds: { medium: 70, high: 90 } };
          break;
        case 'cpu-temp':
          opts = { maxValue: 110, thresholds: { medium: 65, high: 80 } };
          break;
        case 'gpu-temp':
          opts = { maxValue: 110, thresholds: { medium: 70, high: 85 } };
          break;
        case 'network':
          opts = { dualLine: true, color: '#3b82f6' };
          break;
      }
      const chart = new SparklineChart(canvas, opts);
      chart._type = type;
      chart._gpuIdx = canvas.dataset.gpuIdx != null ? parseInt(canvas.dataset.gpuIdx) : null;
      sparklines.push(chart);
    });
  }

  // Aktualizacja sparkline wartosciami z nodeData
  function updateSparklines() {
    if (!nodeData) return;
    const node = nodeData;

    sparklines.forEach(s => {
      switch (s._type) {
        case 'cpu': {
          const v = node.cpu_usage != null ? node.cpu_usage : (node.cpu_percent != null ? node.cpu_percent : null);
          if (v != null) s.push(v);
          break;
        }
        case 'ram': {
          if (node.ram_used_mb != null && node.ram_total_mb > 0) {
            s.push((node.ram_used_mb / node.ram_total_mb) * 100);
          }
          break;
        }
        case 'cpu-temp': {
          if (node.cpu_temperature_c != null) s.push(node.cpu_temperature_c);
          break;
        }
        case 'gpu-usage': {
          const gpus = Array.isArray(node.gpu_info) ? node.gpu_info : [];
          if (s._gpuIdx != null && gpus[s._gpuIdx]) {
            s.push(gpus[s._gpuIdx].usage_percent || 0);
          }
          break;
        }
        case 'gpu-vram': {
          const gpus = Array.isArray(node.gpu_info) ? node.gpu_info : [];
          if (s._gpuIdx != null && gpus[s._gpuIdx]) {
            const g = gpus[s._gpuIdx];
            if (g.vram_total_mb > 0) s.push((g.vram_used_mb / g.vram_total_mb) * 100);
          }
          break;
        }
        case 'gpu-temp': {
          const gpus = Array.isArray(node.gpu_info) ? node.gpu_info : [];
          if (s._gpuIdx != null && gpus[s._gpuIdx]) {
            s.push(gpus[s._gpuIdx].temperature_c || 0);
          }
          break;
        }
        case 'network': {
          const ifaces = Array.isArray(node.network_interfaces) ? node.network_interfaces : [];
          const totalRx = ifaces.reduce((sum, i) => sum + (i.rx_bytes_per_sec || i.rx_bytes || 0), 0);
          const totalTx = ifaces.reduce((sum, i) => sum + (i.tx_bytes_per_sec || i.tx_bytes || 0), 0);
          s.pushDual(totalRx, totalTx);
          break;
        }
      }
    });
  }

  // Skrocony UUID
  function shortId(id) {
    if (!id) return '\u2013';
    return id.length > 12 ? id.substring(0, 12) + '...' : id;
  }

  // Formatowanie info o GPU do topbar
  function formatGpuSummary(gpuInfo) {
    if (!Array.isArray(gpuInfo) || gpuInfo.length === 0) return '';
    const counts = {};
    gpuInfo.forEach(g => {
      const name = g.name || g.device_name || 'GPU';
      counts[name] = (counts[name] || 0) + 1;
    });
    return Object.entries(counts)
      .map(([name, count]) => count > 1 ? `${count}x ${name}` : name)
      .join(', ');
  }

  // Ikona platformy (SVG)
  function platformIcon(platform) {
    const key = (platform || 'unknown').toLowerCase();
    const fn = platformIconMap[key] || platformIconMap.unknown;
    return fn(16);
  }

  // Skeleton loading
  function renderSkeleton() {
    return `
      <div class="mesh-detail">
        <div class="mesh-detail-topbar">
          <button class="btn btn-ghost btn-sm" id="btn-back-to-mesh">\u2190 ${I18n.t('mesh.back_to_mesh')}</button>
          <span class="mesh-detail-topbar-hostname"><span class="skeleton" style="display:inline-block;width:180px;height:24px;border-radius:var(--radius-sm);"></span></span>
          <span class="skeleton" style="display:inline-block;width:80px;height:20px;border-radius:var(--radius-sm);"></span>
        </div>
        <div class="mesh-detail-vram-summary"><div class="skeleton" style="width:100%;height:28px;border-radius:var(--radius-sm);"></div></div>
        <div class="mesh-detail-resource-grid">
          <div class="mesh-detail-section"><div class="skeleton" style="width:100%;height:120px;border-radius:var(--radius-sm);"></div></div>
          <div class="mesh-detail-section"><div class="skeleton" style="width:100%;height:120px;border-radius:var(--radius-sm);"></div></div>
        </div>
        <div class="mesh-detail-section"><div class="skeleton" style="width:100%;height:160px;border-radius:var(--radius-sm);"></div></div>
        <div class="mesh-detail-section"><div class="skeleton" style="width:100%;height:100px;border-radius:var(--radius-sm);"></div></div>
      </div>
    `;
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

    // Platforma
    const platform = (node.platform || node.os || 'unknown').toLowerCase();

    // Metadane topbar — linia 2
    const osDisplay = node.os_info || node.platform || node.os || '';
    const dockerVersion = node.docker_version || node.docker || '';
    const gpuSummary = formatGpuSummary(node.gpu_info);

    const metaParts = [];
    if (osDisplay) metaParts.push(Utils.escapeHtml(osDisplay));
    else metaParts.push('\u2013');
    if (dockerVersion && dockerVersion !== '-') metaParts.push('Docker ' + Utils.escapeHtml(dockerVersion));
    else metaParts.push('\u2013');
    if (gpuSummary) metaParts.push(Utils.escapeHtml(gpuSummary));
    else metaParts.push('\u2013');

    // Topbar z akcjami
    const actionButtons = canManage ? `
      <div class="mesh-detail-topbar-actions">
        <button class="btn btn-primary btn-sm" id="btn-add-service">${I18n.t('mesh.add_service')}</button>
      </div>
    ` : '';

    // VRAM summary bar
    const gpuInfo = Array.isArray(node.gpu_info) ? node.gpu_info : [];
    let vramHtml = '';
    if (gpuInfo.length > 0) {
      const vramUsed = gpuInfo.reduce((s, g) => s + (g.vram_used_mb || 0), 0);
      const vramTotal = gpuInfo.reduce((s, g) => s + (g.vram_total_mb || 0), 0);
      if (vramTotal > 0) {
        const vramPct = Math.round((vramUsed / vramTotal) * 100);
        const level = gaugeLevel(vramPct);
        vramHtml = `
          <div class="mesh-detail-vram-summary">
            <div class="gauge gauge--lg">
              <div class="gauge-header">
                <span class="gauge-label">${I18n.t('mesh.vram_summary')}</span>
                <span class="gauge-value">${Utils.formatMb(vramUsed)} / ${Utils.formatMb(vramTotal)} (${vramPct}%)</span>
              </div>
              <div class="gauge-bar gauge-bar--lg"><div class="gauge-fill ${level}" style="width:${Math.min(vramPct, 100)}%"></div></div>
            </div>
          </div>
        `;
      }
    } else {
      vramHtml = `<div class="mesh-detail-vram-summary"><span class="gauge-label">${I18n.t('mesh.vram_summary')}</span> <span class="mesh-detail-empty">${I18n.t('mesh.no_gpu')}</span></div>`;
    }

    // Karta CPU
    const cpuPct = node.cpu_usage != null ? Math.round(node.cpu_usage) : (node.cpu_percent != null ? Math.round(node.cpu_percent) : null);
    const cpuTemp = node.cpu_temperature_c;
    const cpuThresholds = { medium: 65, high: 80 };

    let cpuCardContent = '';
    if (cpuPct != null) {
      cpuCardContent += renderGaugeLg('CPU', `${cpuPct}%`, cpuPct);
    }
    if (cpuTemp != null) {
      cpuCardContent += `<div class="mesh-detail-gpu-info">${MeshIcons.thermometer()} <span>${Math.round(cpuTemp)}\u00B0C</span> ${thresholdLabel(cpuTemp, cpuThresholds)}</div>`;
    }
    cpuCardContent += `<div class="mesh-sparkline-container"><canvas data-sparkline="cpu" aria-label="CPU usage" role="img"></canvas></div>`;

    // Karta Memory
    let memCardContent = '';
    if (node.ram_used_mb != null && node.ram_total_mb != null && node.ram_total_mb > 0) {
      const ramPct = Math.round((node.ram_used_mb / node.ram_total_mb) * 100);
      memCardContent += renderGaugeLg('RAM', `${Utils.formatMb(node.ram_used_mb)} / ${Utils.formatMb(node.ram_total_mb)}`, ramPct);
    }
    // Swap — ukryty gdy swap_total_mb === 0
    if (node.swap_total_mb != null && node.swap_total_mb > 0) {
      const swapPct = Math.round((node.swap_used_mb / node.swap_total_mb) * 100);
      memCardContent += renderGaugeLg(I18n.t('mesh.swap'), `${Utils.formatMb(node.swap_used_mb || 0)} / ${Utils.formatMb(node.swap_total_mb)}`, swapPct);
    }
    memCardContent += `<div class="mesh-sparkline-container"><canvas data-sparkline="ram" aria-label="RAM usage" role="img"></canvas></div>`;

    // Karty GPU
    let gpuCardsHtml = '';
    gpuInfo.forEach((gpu, idx) => {
      const gpuName = gpu.name || gpu.device_name || `GPU ${idx}`;
      const gpuUsage = gpu.usage_percent != null ? Math.round(gpu.usage_percent) : 0;
      const gpuTempThresholds = { medium: 70, high: 85 };

      let row1 = '<div class="mesh-detail-gpu-metrics">';
      row1 += renderGaugeLg(I18n.t('mesh.gpu_usage'), `${gpuUsage}%`, gpuUsage);
      if (gpu.vram_used_mb != null && gpu.vram_total_mb > 0) {
        const vPct = Math.round((gpu.vram_used_mb / gpu.vram_total_mb) * 100);
        row1 += renderGaugeLg(I18n.t('mesh.gpu_vram'), `${Utils.formatMb(gpu.vram_used_mb)} / ${Utils.formatMb(gpu.vram_total_mb)}`, vPct);
      }
      row1 += '</div>';

      let row2 = '<div class="mesh-detail-gpu-info">';
      row2 += `${MeshIcons.thermometer()} <span>${gpu.temperature_c != null ? gpu.temperature_c + '\u00B0C' : 'N/A'}</span> ${thresholdLabel(gpu.temperature_c, gpuTempThresholds)}`;
      row2 += `<span class="mesh-detail-gpu-info-item">${MeshIcons.bolt()} ${gpu.power_draw_w != null ? Math.round(gpu.power_draw_w) + 'W' : 'N/A'}${gpu.power_limit_w != null ? ' / ' + Math.round(gpu.power_limit_w) + 'W' : ''}</span>`;
      row2 += '</div>';

      let row3 = `<div class="mesh-detail-gpu-sparklines">
        <div class="mesh-sparkline-container"><canvas data-sparkline="gpu-usage" data-gpu-idx="${idx}" aria-label="GPU ${idx} usage" role="img"></canvas></div>
        <div class="mesh-sparkline-container"><canvas data-sparkline="gpu-vram" data-gpu-idx="${idx}" aria-label="GPU ${idx} VRAM" role="img"></canvas></div>
      </div>`;

      gpuCardsHtml += `
        <div class="mesh-detail-section${freshnessClass()}">
          <div class="mesh-detail-section-title">GPU ${idx}: ${Utils.escapeHtml(gpuName)}</div>
          ${row1}${row2}${row3}
          ${getDataFreshness() === 'disconnected' ? renderDisconnectedOverlay() : ''}
          ${getDataFreshness() === 'stale' ? renderStaleBadge() : ''}
        </div>
      `;
    });

    // Karta Network
    let networkCardContent = '';
    const netIfaces = Array.isArray(node.network_interfaces) ? node.network_interfaces : [];
    if (netIfaces.length > 0) {
      netIfaces.forEach(iface => {
        const name = iface.name || iface.interface || '?';
        const linkUp = iface.link_up !== false;
        const dotClass = linkUp ? 'mesh-network-link-dot--up' : 'mesh-network-link-dot--down';
        const ipv4 = iface.ipv4_address || (linkUp ? '' : I18n.t('mesh.network_no_link'));
        const rx = iface.rx_bytes_per_sec != null ? Utils.formatBytes(iface.rx_bytes_per_sec) : (iface.rx_bytes != null ? Utils.formatBytes(iface.rx_bytes) : '0 B/s');
        const tx = iface.tx_bytes_per_sec != null ? Utils.formatBytes(iface.tx_bytes_per_sec) : (iface.tx_bytes != null ? Utils.formatBytes(iface.tx_bytes) : '0 B/s');
        const typeIcon = iface.interface_type === 'thunderbolt' ? MeshIcons.bolt(14) : '';
        const rdmaBadge = iface.rdma_available ? 'RDMA' : '';

        networkCardContent += `
          <div class="mesh-network-row">
            <span class="mesh-network-link-dot ${dotClass}">${linkUp ? '\u25CF' : '\u25CB'}</span>
            <span class="mesh-network-name">${typeIcon} ${Utils.escapeHtml(name)}</span>
            <span class="mesh-network-ip">${Utils.escapeHtml(ipv4)}</span>
            <span class="mesh-network-throughput">\u2193 ${rx} \u2191 ${tx}</span>
            <span class="mesh-network-rdma-badge">${rdmaBadge}</span>
            <button aria-label="${I18n.t('mesh.configure_network').replace('{name}', name)}" class="btn btn-ghost btn-xs mesh-network-config-btn" data-interface="${Utils.escapeAttr(name)}">${MeshIcons.gear(14)}</button>
          </div>
        `;
      });
    } else if (node.network_rx_bytes != null || node.network_tx_bytes != null) {
      const rx = node.network_rx_bytes != null ? Utils.formatBytes(node.network_rx_bytes) : '0 B/s';
      const tx = node.network_tx_bytes != null ? Utils.formatBytes(node.network_tx_bytes) : '0 B/s';
      networkCardContent += `<div class="mesh-network-row"><span class="mesh-network-throughput">\u2193 ${rx} \u2191 ${tx}</span></div>`;
    }
    networkCardContent += `<div class="mesh-sparkline-container" style="margin-top:var(--spacing-sm);"><canvas data-sparkline="network" aria-label="Network throughput" role="img"></canvas></div>`;

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

    // Stale/disconnected banner
    const freshness = getDataFreshness();
    let freshnessBanner = '';
    if (freshness === 'stale') {
      const age = Math.round((Date.now() - lastFetchTime) / 1000);
      freshnessBanner = `<div class="mesh-stale-badge" aria-live="polite">${I18n.t('mesh.stale')} (${age}s)</div>`;
    } else if (freshness === 'disconnected') {
      const age = lastFetchTime ? Math.round((Date.now() - lastFetchTime) / 1000) : '?';
      freshnessBanner = `<div class="mesh-stale-badge mesh-stale-badge--error" aria-live="assertive">${I18n.t('mesh.connection_lost').replace('{seconds}', age)}</div>`;
    }

    content.innerHTML = `
      <div class="mesh-detail">
        <div class="mesh-detail-topbar">
          <div class="mesh-detail-topbar-line1">
            <button class="btn btn-ghost btn-sm" id="btn-back-to-mesh">\u2190 ${I18n.t('mesh.back_to_mesh')}</button>
            <span class="mesh-detail-topbar-hostname" title="${Utils.escapeAttr(nodeId)}">${platformIcon(platform)} ${Utils.escapeHtml(hostname)}</span>
            <span class="container-status ${statusClass}">${Utils.escapeHtml(statusRaw)}</span>
            ${actionButtons}
          </div>
          <div class="mesh-detail-topbar-meta">${metaParts.filter(p => p !== '\u2013').join(' \u00B7 ')}</div>
          ${freshnessBanner}
        </div>

        ${vramHtml}

        <div class="mesh-detail-resource-grid">
          <div class="mesh-detail-section${freshnessClass()}">
            <div class="mesh-detail-section-title">CPU</div>
            ${cpuCardContent}
            ${freshness === 'disconnected' ? renderDisconnectedOverlay() : ''}
            ${freshness === 'stale' ? renderStaleBadge() : ''}
          </div>
          <div class="mesh-detail-section${freshnessClass()}">
            <div class="mesh-detail-section-title">Memory</div>
            ${memCardContent}
            ${freshness === 'disconnected' ? renderDisconnectedOverlay() : ''}
            ${freshness === 'stale' ? renderStaleBadge() : ''}
          </div>
        </div>

        ${gpuCardsHtml}

        ${(netIfaces.length > 0 || node.network_rx_bytes != null) ? `
        <div class="mesh-detail-section${freshnessClass()}">
          <div class="mesh-detail-section-title">Network</div>
          ${networkCardContent}
          ${freshness === 'disconnected' ? renderDisconnectedOverlay() : ''}
          ${freshness === 'stale' ? renderStaleBadge() : ''}
        </div>
        ` : ''}

        <div class="mesh-detail-section">
          <div class="mesh-detail-section-title">${I18n.t('mesh.containers')}</div>
          ${containersHtml}
        </div>
      </div>
    `;

    // Ponowne tworzenie sparklines po przerysowaniu DOM
    createSparklines();
  }

  // Overlay disconnected
  function renderDisconnectedOverlay() {
    return `<div class="mesh-disconnected-overlay" aria-live="assertive">${I18n.t('mesh.disconnected')}</div>`;
  }

  // Badge stale
  function renderStaleBadge() {
    return `<span class="mesh-stale-badge" aria-live="polite">${I18n.t('mesh.stale')}</span>`;
  }

  // Wiersz kontenera z CPU%, RAM i akcjami
  function renderContainerRow(c) {
    const name = c.name || c.Names || c.id || '-';
    const image = c.image || c.Image || '-';
    const status = c.status || c.State || c.state || '-';
    const containerId = c.id || c.Id || c.container_id || '';
    const statusLower = status.toLowerCase();
    const isRunning = statusLower.includes('up') || statusLower.includes('running');
    const statusClass = isRunning ? 'running' : (statusLower.includes('exited') ? 'exited' : 'stopped');
    const cpuPct = c.cpu_percent != null ? c.cpu_percent.toFixed(1) + '%' : '-';
    const ramUsage = c.ram_used_mb != null ? Utils.formatMb(c.ram_used_mb) : (c.memory_usage || '-');

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

  // Podpiecie przycisk wstecz (skeleton)
  function bindBackButton() {
    const backBtn = document.getElementById('btn-back-to-mesh');
    if (backBtn) backBtn.addEventListener('click', close);
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
        cleanup();
        ServiceCatalog.show(nid, 'mesh');
      });
    }

    const content = document.getElementById('content');
    if (content) {
      if (boundHandleAction) {
        content.removeEventListener('click', boundHandleAction);
      }
      boundHandleAction = handleContainerAction;
      content.addEventListener('click', boundHandleAction);
    }
  }

  // Obsluga akcji na kontenerze i konfiguracji sieci
  async function handleContainerAction(e) {
    // Przycisk konfiguracji sieci
    const netCfgBtn = e.target.closest('.mesh-network-config-btn');
    if (netCfgBtn) {
      const ifaceName = netCfgBtn.dataset.interface;
      if (!ifaceName || !currentNodeId || !nodeData) return;
      const ifaces = nodeData.network_interfaces || [];
      const ifaceData = ifaces.find(i => (i.name || i.interface) === ifaceName) || {};
      MeshNetworkConfig.show(currentNodeId, ifaceName, ifaceData);
      return;
    }

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

  return { show, close, cleanup, MeshIcons };
})();

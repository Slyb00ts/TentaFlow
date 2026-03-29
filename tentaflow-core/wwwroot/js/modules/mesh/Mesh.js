// =============================================================================
// Plik: Mesh.js
// Opis: Widok zarzadzania nodami mesh — kafelki z metrykami, parowanie,
//       reczne dodawanie, auto-refresh co 5 sekund.
// Przyklad: ViewRouter.register('mesh', Mesh);
// =============================================================================

const Mesh = (() => {
  'use strict';

  let nodes = [];
  let refreshInterval = null;

  // Ikony platform
  const platformIcons = {
    linux: '\uD83D\uDC27',
    macos: '\uD83C\uDF4E',
    windows: '\uD83E\uDE9F',
    android: '\uD83D\uDCF1',
    ios: '\uD83D\uDCF1',
    unknown: '\uD83D\uDDA5\uFE0F'
  };

  // Pobranie nodow z API
  async function loadNodes() {
    try {
      const response = await ApiClient.get('/api/mesh/nodes');
      nodes = response || [];
    } catch (e) {
      nodes = [];
    }
  }

  // Renderowanie widoku
  function render() {
    return `
      <div class="content-header" style="display:flex;align-items:center;justify-content:space-between;margin-bottom:var(--spacing-md);">
        <h2 data-i18n="mesh.title">${I18n.t('mesh.title')}</h2>
        <button class="btn btn-primary btn-sm" id="btn-add-node">
          + <span data-i18n="mesh.addNode">${I18n.t('mesh.addNode')}</span>
        </button>
      </div>
      <div id="mesh-sections-container">
        <p data-i18n="common.loading">${I18n.t('common.loading')}</p>
      </div>
    `;
  }

  // Montowanie — zaladuj dane, podepnij zdarzenia, auto-refresh
  async function mount() {
    const addBtn = document.getElementById('btn-add-node');
    if (addBtn) {
      addBtn.addEventListener('click', openConnectModal);
    }

    const container = document.getElementById('mesh-sections-container');
    if (container) {
      container.addEventListener('click', handleCardClick);
    }

    await loadNodes();
    renderSections();

    refreshInterval = setInterval(async () => {
      await loadNodes();
      renderSections();
    }, 5000);
  }

  // Odmontowanie — pelne (przy nawigacji do innego widoku)
  function unmount() {
    // Zatrzymaj MeshNodeDetail jesli jest otwarty
    if (typeof MeshNodeDetail !== 'undefined' && MeshNodeDetail.cleanup) {
      MeshNodeDetail.cleanup();
    }
    unmountRefreshOnly();
  }

  // Odmontowanie — tylko interval i listenery Mesh (bez cleanup MeshNodeDetail)
  function unmountRefreshOnly() {
    const container = document.getElementById('mesh-sections-container');
    if (container) {
      container.removeEventListener('click', handleCardClick);
    }
    nodes = [];
    if (refreshInterval) {
      clearInterval(refreshInterval);
      refreshInterval = null;
    }
  }

  // Podzial nodow na sekcje
  function categorizeNodes() {
    const local = nodes.filter(n => n.is_local || n.source === 'local');
    const trusted = nodes.filter(n => !n.is_local && n.source !== 'local' && (n.source === 'trusted' || (n.trust_status || n.status || '').toLowerCase() === 'trusted' || (n.trust_status || n.status || '').toLowerCase() === 'paired'));
    const discovered = nodes.filter(n => !n.is_local && n.source !== 'local' && !trusted.includes(n));
    return { local, trusted, discovered };
  }

  // Renderowanie sekcji nodow
  function renderSections() {
    const container = document.getElementById('mesh-sections-container');
    if (!container) return;

    const { local, trusted, discovered } = categorizeNodes();

    if (nodes.length === 0) {
      container.innerHTML = `
        <div class="empty-state">
          <div class="empty-state-icon">\uD83D\uDD17</div>
          <div class="empty-state-text" data-i18n="mesh.noNodes">${I18n.t('mesh.noNodes')}</div>
        </div>
      `;
      return;
    }

    let html = '';

    // Sekcja: Ten node
    if (local.length > 0) {
      html += renderSection(I18n.t('mesh.this_node'), local, 'local');
    }

    // Sekcja: Sparowane nody
    if (trusted.length > 0) {
      html += renderSection(I18n.t('mesh.paired_nodes'), trusted, 'trusted');
    }

    // Sekcja: Wykryte nody
    if (discovered.length > 0) {
      html += renderSection(I18n.t('mesh.discovered_nodes'), discovered, 'discovered');
    }

    container.innerHTML = html;
  }

  // Renderowanie sekcji z tytulem i kafelkami
  function renderSection(title, nodeList, type) {
    return `
      <div class="mesh-section">
        <div class="mesh-section-title">${Utils.escapeHtml(title)}</div>
        <div class="mesh-nodes-grid">
          ${nodeList.map(n => renderNodeCard(n, type)).join('')}
        </div>
      </div>
    `;
  }

  // Grupowanie nazw GPU — "RTX 4090, RTX 4090" -> "2x RTX 4090"
  function formatGpuNames(names) {
    const counts = {};
    names.forEach(n => { counts[n] = (counts[n] || 0) + 1; });
    return Object.entries(counts)
      .map(([name, count]) => count > 1 ? `${count}x ${name}` : name)
      .join(', ');
  }

  // Klasa koloru gauge wg procentu
  function gaugeLevel(pct) {
    if (pct > 80) return 'high';
    if (pct >= 50) return 'medium';
    return 'low';
  }

  // Pojedynczy gauge bar
  function renderGauge(label, value, pct) {
    const level = gaugeLevel(pct);
    return `
      <div class="gauge">
        <div class="gauge-header">
          <span class="gauge-label">${Utils.escapeHtml(label)}</span>
          <span class="gauge-value">${Utils.escapeHtml(value)}</span>
        </div>
        <div class="gauge-bar"><div class="gauge-fill ${level}" style="width:${Math.min(pct, 100)}%"></div></div>
      </div>
    `;
  }

  // Renderowanie jednego kafelka noda
  function renderNodeCard(node, type) {
    const platform = (node.platform || node.os || 'unknown').toLowerCase();
    const icon = platformIcons[platform] || platformIcons.unknown;
    const nodeId = node.node_id || node.id || '';
    const hostname = node.hostname || node.name || nodeId || I18n.t('mesh.unknown_host');

    // Trust badge
    let trustBadge = '';
    if (type === 'local') {
      trustBadge = `<span class="mesh-trust-badge mesh-trust-local">${I18n.t('mesh.local')}</span>`;
    } else if (type === 'trusted') {
      trustBadge = `<span class="mesh-trust-badge mesh-trust-trusted">${I18n.t('mesh.trusted')}</span>`;
    } else {
      trustBadge = `<span class="mesh-trust-badge mesh-trust-discovered">${I18n.t('mesh.discovered')}</span>`;
    }

    // Gauges
    const gauges = [];

    // CPU
    const cpuPct = node.cpu_usage != null ? Math.round(node.cpu_usage) : (node.cpu_percent != null ? Math.round(node.cpu_percent) : null);
    if (cpuPct != null) {
      gauges.push(renderGauge('CPU', `${cpuPct}%`, cpuPct));
    }

    // RAM
    if (node.ram_used_mb != null && node.ram_total_mb != null && node.ram_total_mb > 0) {
      const ramPct = Math.round((node.ram_used_mb / node.ram_total_mb) * 100);
      const ramUsed = Utils.formatMb(node.ram_used_mb);
      const ramTotal = Utils.formatMb(node.ram_total_mb);
      gauges.push(renderGauge('RAM', `${ramUsed} / ${ramTotal}`, ramPct));
    }

    // GPU — srednia usage + count
    const gpuInfo = Array.isArray(node.gpu_info) ? node.gpu_info : [];
    const gpuCount = gpuInfo.length || node.gpu_count || 0;
    if (gpuCount > 0 && gpuInfo.length > 0) {
      const avgUsage = Math.round(gpuInfo.reduce((s, g) => s + (g.usage_percent || 0), 0) / gpuInfo.length);
      const gpuLabel = `${avgUsage}% (${gpuCount}x GPU)`;
      gauges.push(renderGauge('GPU', gpuLabel, avgUsage));

      // VRAM — suma po wszystkich GPU
      const vramUsed = gpuInfo.reduce((s, g) => s + (g.vram_used_mb || 0), 0);
      const vramTotal = gpuInfo.reduce((s, g) => s + (g.vram_total_mb || 0), 0);
      if (vramTotal > 0) {
        const vramPct = Math.round((vramUsed / vramTotal) * 100);
        const vramUsedFmt = Utils.formatMb(vramUsed);
        const vramTotalFmt = Utils.formatMb(vramTotal);
        gauges.push(renderGauge('VRAM', `${vramUsedFmt} / ${vramTotalFmt}`, vramPct));
      }
    }

    // Stopka: kontenery + siec
    let footerParts = [];
    const cRunning = node.containers_running;
    const cTotal = node.containers_total;
    if (cRunning != null && cTotal != null) {
      footerParts.push(`<span>\uD83D\uDD32 Containers: <strong>${cRunning}</strong> / ${cTotal}</span>`);
    }
    const rxBytes = node.network_rx_bytes;
    const txBytes = node.network_tx_bytes;
    if (rxBytes != null || txBytes != null) {
      const rx = rxBytes != null ? Utils.formatBytes(rxBytes) : '0 B/s';
      const tx = txBytes != null ? Utils.formatBytes(txBytes) : '0 B/s';
      footerParts.push(`<span>\uD83D\uDD17 Net: \u2193 ${rx} \u2191 ${tx}</span>`);
    }

    const localClass = type === 'local' ? ' mesh-node-local' : '';
    const pairBtn = (type !== 'local' && type !== 'trusted')
      ? `<button class="btn btn-sm btn-primary" data-node-pair="${Utils.escapeAttr(nodeId)}">${I18n.t('mesh.pair')}</button>`
      : '';

    return `
      <div class="mesh-node-card${localClass}" data-node-detail="${Utils.escapeAttr(nodeId)}">
        <div class="mesh-node-header">
          <span class="mesh-node-icon">${icon}</span>
          <span class="mesh-node-name">${Utils.escapeHtml(hostname)}</span>
          ${trustBadge}
        </div>
        ${gauges.length > 0 ? `<div class="mesh-node-gauges">${gauges.join('')}</div>` : ''}
        ${footerParts.length > 0 ? `<div class="mesh-node-footer">${footerParts.join('')}</div>` : ''}
        ${pairBtn}
      </div>
    `;
  }

  // Obsluga klikniec na kartach nodow
  function handleCardClick(e) {
    // Parowanie
    const pairBtn = e.target.closest('[data-node-pair]');
    if (pairBtn) {
      const nodeId = pairBtn.dataset.nodePair;
      startPairing(nodeId);
      return;
    }

    // Cofniecie zaufania
    const revokeBtn = e.target.closest('[data-node-revoke]');
    if (revokeBtn) {
      const nodeId = revokeBtn.dataset.nodeRevoke;
      revokeTrust(nodeId);
      return;
    }

    // Serwisy na nodzie
    const svcBtn = e.target.closest('[data-node-services]');
    if (svcBtn) {
      const nodeId = svcBtn.dataset.nodeServices;
      showNodeServices(nodeId);
      return;
    }

    // Klikniecie na karte (nie na przycisk) — otworz szczegoly noda
    const card = e.target.closest('[data-node-detail]');
    if (card && !e.target.closest('button')) {
      const nodeId = card.dataset.nodeDetail;
      MeshNodeDetail.show(nodeId);
      return;
    }
  }

  // Parowanie noda — POST /api/mesh/pair/:id, nastepnie pokaz PIN modal
  async function startPairing(nodeId) {
    try {
      const result = await ApiClient.post(`/api/mesh/pair/${encodeURIComponent(nodeId)}`);
      App.showToast(I18n.t('mesh.pair_success'), 'success');
      openPinModal(nodeId, result);
    } catch (err) {
      App.showToast(err.message || I18n.t('common.error'), 'error');
    }
  }

  // Modal z PIN-em do potwierdzenia parowania
  function openPinModal(nodeId, pairResult) {
    const pin = pairResult?.pin || '';
    const overlay = document.createElement('div');
    overlay.className = 'modal-overlay active';
    overlay.innerHTML = `
      <div class="modal">
        <div class="modal-header">
          <h3>${I18n.t('mesh.pair_pin_title')}</h3>
          <button class="modal-close" id="pin-modal-close">&times;</button>
        </div>
        <div class="modal-body">
          ${pin ? `<p style="text-align:center;font-size:var(--font-size-2xl);font-weight:700;letter-spacing:0.2em;margin:var(--spacing-md) 0;">${Utils.escapeHtml(pin)}</p>` : ''}
          <div class="form-group">
            <label for="pin-input">${I18n.t('mesh.pin_label')}</label>
            <input type="text" id="pin-input" maxlength="6" placeholder="000000" style="text-align:center;font-size:var(--font-size-xl);letter-spacing:0.15em;">
            <div class="form-hint">${I18n.t('mesh.pair_pin_hint')}</div>
          </div>
          <div id="pin-form-error" class="form-error" hidden></div>
        </div>
        <div class="modal-footer">
          <button class="btn btn-secondary" id="pin-modal-cancel">${I18n.t('common.cancel')}</button>
          <button class="btn btn-primary" id="pin-modal-confirm">${I18n.t('mesh.confirm_pairing')}</button>
        </div>
      </div>
    `;

    document.body.appendChild(overlay);

    const closeModal = () => {
      if (overlay.parentNode) overlay.remove();
    };

    overlay.querySelector('#pin-modal-close').addEventListener('click', closeModal);
    overlay.querySelector('#pin-modal-cancel').addEventListener('click', closeModal);
    overlay.addEventListener('click', (e) => {
      if (e.target === overlay) closeModal();
    });

    overlay.querySelector('#pin-modal-confirm').addEventListener('click', async () => {
      const pinValue = overlay.querySelector('#pin-input').value.trim();
      const errorEl = overlay.querySelector('#pin-form-error');
      if (!pinValue) {
        if (errorEl) { errorEl.textContent = I18n.t('common.required'); errorEl.hidden = false; }
        return;
      }
      try {
        await ApiClient.post(`/api/mesh/pair/${encodeURIComponent(nodeId)}/confirm`, { pin: pinValue });
        App.showToast(I18n.t('mesh.pair_confirm_success'), 'success');
        closeModal();
        await loadNodes();
        renderSections();
      } catch (err) {
        if (errorEl) { errorEl.textContent = err.message || I18n.t('common.error'); errorEl.hidden = false; }
      }
    });
  }

  // Cofniecie zaufania — DELETE /api/mesh/trust/:id
  async function revokeTrust(nodeId) {
    if (!confirm(I18n.t('mesh.revoke_confirm'))) return;
    try {
      await ApiClient.delete(`/api/mesh/trust/${encodeURIComponent(nodeId)}`);
      App.showToast(I18n.t('mesh.revoke_success'), 'success');
      await loadNodes();
      renderSections();
    } catch (err) {
      App.showToast(err.message || I18n.t('common.error'), 'error');
    }
  }

  // Pokaz serwisy na nodzie
  async function showNodeServices(nodeId) {
    const node = nodes.find(n => (n.node_id || n.id) === nodeId);
    const hostname = node?.hostname || node?.name || nodeId;
    try {
      const details = await ApiClient.get(`/api/mesh/nodes/${encodeURIComponent(nodeId)}`);
      const services = details?.services || [];
      const overlay = document.createElement('div');
      overlay.className = 'modal-overlay active';
      overlay.innerHTML = `
        <div class="modal">
          <div class="modal-header">
            <h3>${I18n.t('mesh.services_on').replace('{name}', Utils.escapeHtml(hostname))}</h3>
            <button class="modal-close" id="svc-modal-close">&times;</button>
          </div>
          <div class="modal-body">
            ${services.length === 0
              ? `<p style="color:var(--color-text-muted);">${I18n.t('mesh.no_services')}</p>`
              : `<div class="table-wrapper"><table>
                  <thead><tr><th>${I18n.t('common.name')}</th><th>${I18n.t('common.type')}</th><th>${I18n.t('common.status')}</th></tr></thead>
                  <tbody>${services.map(s => `
                    <tr>
                      <td>${Utils.escapeHtml(s.name || s.service_id || '-')}</td>
                      <td><span class="badge service-type-badge">${Utils.escapeHtml(s.service_type || '-')}</span></td>
                      <td>${Utils.escapeHtml(s.status || '-')}</td>
                    </tr>
                  `).join('')}</tbody>
                </table></div>`
            }
          </div>
          <div class="modal-footer">
            <button class="btn btn-secondary" id="svc-modal-ok">${I18n.t('common.close')}</button>
          </div>
        </div>
      `;
      document.body.appendChild(overlay);

      const closeModal = () => { if (overlay.parentNode) overlay.remove(); };
      overlay.querySelector('#svc-modal-close').addEventListener('click', closeModal);
      overlay.querySelector('#svc-modal-ok').addEventListener('click', closeModal);
      overlay.addEventListener('click', (e) => { if (e.target === overlay) closeModal(); });
    } catch (err) {
      App.showToast(err.message || I18n.t('common.error'), 'error');
    }
  }

  // Modal dodawania noda recznie — POST /api/mesh/connect
  function openConnectModal() {
    const overlay = document.createElement('div');
    overlay.className = 'modal-overlay active';
    overlay.innerHTML = `
      <div class="modal">
        <div class="modal-header">
          <h3>${I18n.t('mesh.connect_title')}</h3>
          <button class="modal-close" id="connect-modal-close">&times;</button>
        </div>
        <div class="modal-body">
          <div class="form-group">
            <label for="connect-addr">${I18n.t('mesh.connect_addr')}</label>
            <input type="text" id="connect-addr" placeholder="192.168.1.100:8090">
            <div class="form-hint">${I18n.t('mesh.connect_addr_hint')}</div>
          </div>
          <div id="connect-form-error" class="form-error" hidden></div>
        </div>
        <div class="modal-footer">
          <button class="btn btn-secondary" id="connect-modal-cancel">${I18n.t('common.cancel')}</button>
          <button class="btn btn-primary" id="connect-modal-save">${I18n.t('mesh.addNode')}</button>
        </div>
      </div>
    `;

    document.body.appendChild(overlay);

    const closeModal = () => {
      if (overlay.parentNode) overlay.remove();
    };

    overlay.querySelector('#connect-modal-close').addEventListener('click', closeModal);
    overlay.querySelector('#connect-modal-cancel').addEventListener('click', closeModal);
    overlay.addEventListener('click', (e) => {
      if (e.target === overlay) closeModal();
    });

    overlay.querySelector('#connect-modal-save').addEventListener('click', async () => {
      const addr = overlay.querySelector('#connect-addr').value.trim();
      const errorEl = overlay.querySelector('#connect-form-error');
      if (!addr) {
        if (errorEl) { errorEl.textContent = I18n.t('common.required'); errorEl.hidden = false; }
        return;
      }
      const saveBtn = overlay.querySelector('#connect-modal-save');
      if (saveBtn) { saveBtn.disabled = true; saveBtn.textContent = '...'; }
      try {
        await ApiClient.post('/api/mesh/connect', { address: addr });
        App.showToast(I18n.t('mesh.connect_success'), 'success');
        closeModal();
        await loadNodes();
        renderSections();
      } catch (err) {
        if (errorEl) { errorEl.textContent = err.message || I18n.t('common.error'); errorEl.hidden = false; }
      } finally {
        if (saveBtn) { saveBtn.disabled = false; saveBtn.textContent = I18n.t('mesh.addNode'); }
      }
    });
  }

  return { render, mount, unmount, unmountRefreshOnly };
})();

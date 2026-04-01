// =============================================================================
// Plik: ClusterWizard.js
// Opis: 3-krokowy wizard tworzenia clustera — nazwa+nody, siec, podsumowanie.
//       Faza auto-detekcji z blokada selektorow, SSE streaming wynikow probing.
// =============================================================================

const ClusterWizard = (() => {
  'use strict';

  let currentStep = 1;
  let clusterName = '';
  let clusterDesc = '';
  let selectedNodes = [];
  let allNodes = [];
  let probeResults = [];
  let detectionResult = null;
  let interfaceOverrides = {};
  let probeEventSource = null;
  let isDetecting = false;

  // Zaladuj dostepne nody z API
  async function loadNodes() {
    try {
      const all = await ApiClient.get('/api/mesh/nodes');
      allNodes = (all || []).filter(n => {
        const trust = (n.trust_status || n.status || '').toLowerCase();
        return trust === 'trusted' || trust === 'paired' || trust === 'local' || n.is_local;
      });
      allNodes.sort((a, b) => {
        if (a.is_local && !b.is_local) return -1;
        if (!a.is_local && b.is_local) return 1;
        return (a.hostname || '').localeCompare(b.hostname || '');
      });
    } catch (e) {
      allNodes = [];
    }
  }

  // Otworz wizard
  function open(editCluster = null) {
    currentStep = 1;
    clusterName = editCluster?.name || '';
    clusterDesc = editCluster?.description || '';
    selectedNodes = [];
    probeResults = [];
    detectionResult = null;
    interfaceOverrides = {};

    loadNodes().then(() => render());
  }

  // Zamknij wizard
  function close() {
    if (probeEventSource) { probeEventSource.close(); probeEventSource = null; }
    Clusters.mount();
  }

  // Glowny render
  function render() {
    const container = document.getElementById('clusters-list-container');
    if (!container) return;

    container.innerHTML = `
      <div class="wizard-container">
        ${renderStepIndicator()}
        <div id="wizard-step-content">
          ${renderCurrentStepContent()}
        </div>
        ${renderNavigation()}
      </div>
    `;

    bindEvents();
  }

  // Pasek 3 krokow
  function renderStepIndicator() {
    const steps = [
      { num: 1, label: I18n.t('clusters.step_nodes') },
      { num: 2, label: I18n.t('clusters.step_network') },
      { num: 3, label: I18n.t('clusters.step_summary') },
    ];

    return `
      <div class="wizard-steps">
        ${steps.map((s, i) => {
          const cls = s.num < currentStep ? 'completed' : s.num === currentStep ? 'active' : '';
          const dot = s.num < currentStep ? '&#10003;' : s.num;
          const line = i < steps.length - 1
            ? `<div class="wizard-step-line ${s.num < currentStep ? 'completed' : ''}"></div>`
            : '';
          return `
            <div class="wizard-step ${cls}">
              <div class="wizard-step-dot">${dot}</div>
              <span>${s.label}</span>
            </div>
            ${line}
          `;
        }).join('')}
      </div>
    `;
  }

  // Nawigacja
  function renderNavigation() {
    const canNext = currentStep === 1 ? (clusterName.trim().length > 0 && selectedNodes.length >= 2)
      : currentStep === 2 ? !isDetecting
      : true;

    const nextLabel = currentStep === 3
      ? `&#10003; ${I18n.t('clusters.create_cluster')}`
      : `${I18n.t('common.next')} &rarr;`;
    const nextClass = currentStep === 3 ? 'btn-success' : 'btn-primary';

    return `
      <div class="wizard-nav">
        ${currentStep > 1
          ? `<button class="btn btn-secondary" id="wiz-back">&larr; ${I18n.t('common.back')}</button>`
          : '<div></div>'}
        <button class="btn btn-ghost" id="wiz-cancel">${I18n.t('common.cancel')}</button>
        <button class="btn ${nextClass}" id="wiz-next" ${!canNext ? 'disabled' : ''}>${nextLabel}</button>
      </div>
    `;
  }

  function renderCurrentStepContent() {
    switch (currentStep) {
      case 1: return renderStep1();
      case 2: return renderStep2();
      case 3: return renderStep3();
      default: return '';
    }
  }

  function renderCurrentStep() {
    const content = document.getElementById('wizard-step-content');
    if (content) content.innerHTML = renderCurrentStepContent();
    bindStepEvents();
  }

  // --- Krok 1: Nazwa + Nody (polaczone) ---
  function renderStep1() {
    const cards = allNodes.map(node => {
      const isSelected = selectedNodes.some(n => n.node_id === node.node_id);
      const gpuInfo = (node.gpu_info || []).map(g => g.name || '').filter(Boolean).join(', ');
      const vram = (node.gpu_info || []).reduce((sum, g) => sum + (g.vram_total_mb || 0), 0);
      const vramGB = (vram / 1024).toFixed(0);

      return `
        <div class="mesh-node-card wizard-node-card ${isSelected ? 'wizard-node-selected' : ''}"
             data-select-node="${Utils.escapeAttr(node.node_id)}">
          <div class="mesh-node-header">
            <span class="mesh-node-name">${Utils.escapeHtml(node.hostname || node.node_id)}</span>
            ${node.is_local ? '<span class="badge badge-info">Local</span>' : ''}
          </div>
          <div class="wizard-node-stats">
            <span>${node.cpu_count || '?'} CPU</span>
            <span>${node.ram_total_mb ? Math.round(node.ram_total_mb / 1024) + ' GB RAM' : ''}</span>
            ${vram > 0 ? `<span>${vramGB} GB VRAM</span>` : ''}
          </div>
          ${gpuInfo ? `<div class="wizard-node-gpu">${Utils.escapeHtml(gpuInfo)}</div>` : ''}
        </div>
      `;
    }).join('');

    const noNodes = allNodes.length === 0
      ? `<div class="empty-state">
           <div class="empty-state-icon">&#128268;</div>
           <div class="empty-state-text">${I18n.t('clusters.no_trusted_nodes')}</div>
           <a href="#" onclick="ViewRouter.navigate('mesh')" class="btn btn-primary btn-sm">
             ${I18n.t('clusters.go_to_mesh')}
           </a>
         </div>`
      : '';

    const selectedCount = selectedNodes.length;
    const countLabel = selectedCount > 0
      ? `${selectedCount} ${I18n.t('clusters.nodes_selected')}`
      : I18n.t('clusters.select_min_nodes');

    return `
      <div class="wizard-step-content">
        <div class="form-group">
          <label for="wiz-name">${I18n.t('clusters.name')}</label>
          <input type="text" id="wiz-name" placeholder="np. GPU-Farm"
                 value="${Utils.escapeAttr(clusterName)}">
        </div>
        <div class="form-group" style="margin-bottom:var(--spacing-lg);">
          <label for="wiz-desc">${I18n.t('clusters.description')}</label>
          <input type="text" id="wiz-desc" placeholder="${I18n.t('common.optional')}"
                 value="${Utils.escapeAttr(clusterDesc)}">
        </div>
        <div class="wizard-nodes-header">
          <div class="matrix-label">${I18n.t('clusters.select_nodes_label')}</div>
          <span class="wizard-nodes-count ${selectedCount >= 2 ? 'wizard-nodes-count-ok' : ''}">${countLabel}</span>
        </div>
        ${noNodes || `<div class="mesh-nodes-grid">${cards}</div>`}
      </div>
    `;
  }

  // --- Krok 2: Konfiguracja sieci ---
  function renderStep2() {
    const isLocked = isDetecting;

    const context = `<div class="wizard-context-bar">${Utils.escapeHtml(clusterName)} &mdash; ${selectedNodes.length} ${I18n.t('clusters.nodes_selected')}</div>`;

    const banner = isDetecting
      ? `<div class="probe-banner probe-banner-detecting">
           <span class="probe-dot-pulse"></span>
           <span>${I18n.t('clusters.detecting_in_progress')}</span>
           <span class="probe-progress">${probeResults.length} / ?</span>
         </div>`
      : detectionResult
        ? `<div class="probe-banner ${detectionResult.is_mixed ? 'probe-banner-warning' : 'probe-banner-success'}">
             <span>${detectionResult.is_mixed ? '&#9888;' : '&#10003;'}</span>
             <span>${Utils.escapeHtml(detectionResult.message)}</span>
           </div>`
        : '';

    const totalPairs = selectedNodes.length > 1 ? (selectedNodes.length * (selectedNodes.length - 1)) / 2 : 0;
    const completedPairs = probeResults.length;
    const subtitle = isDetecting
      ? I18n.t('clusters.probing_subtitle')
          .replace('{nodes}', selectedNodes.length)
          .replace('{completed}', completedPairs)
          .replace('{total}', totalPairs)
      : I18n.t('clusters.network_config_subtitle');

    const sectionTitle = `<h2 class="section-title">${I18n.t('clusters.network_configuration')}</h2>`;
    const sectionSubtitle = `<p class="section-subtitle">${subtitle}</p>`;

    const matrix = renderBandwidthMatrix();
    const selectors = renderInterfaceSelectors(isLocked);

    return `
      <div class="wizard-step-content">
        ${context}
        ${banner}
        ${sectionTitle}
        ${sectionSubtitle}
        ${selectors}
        ${matrix}
      </div>
    `;
  }

  // Selektory interfejsow
  function renderInterfaceSelectors(locked) {
    return `
      <div class="interface-section ${locked ? 'interface-locked' : ''}">
        <div class="matrix-label">${I18n.t('clusters.interface_assignment')}</div>
        ${selectedNodes.map(node => {
          const interfaces = node.network_interfaces || [];
          const currentIface = interfaceOverrides[node.node_id] ||
            (detectionResult?.per_node?.[node.node_id]?.interface) || '';

          return `
            <div class="interface-row">
              <div class="interface-hostname">${Utils.escapeHtml(node.hostname || node.node_id)}</div>
              <select class="interface-select" data-node-iface="${Utils.escapeAttr(node.node_id)}"
                      ${locked ? 'disabled' : ''}>
                ${interfaces.map(iface => {
                  const isSelected = iface.name === currentIface;
                  const speedLabel = iface.speed_mbps >= 1000
                    ? (iface.speed_mbps / 1000).toFixed(0) + ' Gbps'
                    : iface.speed_mbps + ' Mbps';
                  const reachInfo = getReachabilityInfo(node.node_id, iface.name);
                  return `<option value="${Utils.escapeAttr(iface.name)}" ${isSelected ? 'selected' : ''}>
                    ${Utils.escapeHtml(iface.name)} \u2014 ${speedLabel} ${reachInfo}
                  </option>`;
                }).join('')}
              </select>
              <div class="interface-badges">
                ${renderInterfaceBadges(interfaces.find(i => i.name === currentIface))}
              </div>
            </div>
          `;
        }).join('')}
      </div>
    `;
  }

  // Badge'e interfejsu
  function renderInterfaceBadges(iface) {
    if (!iface) return '';
    const badges = [];
    if (iface.rdma_available) badges.push('<span class="badge badge-roce">ROCE</span>');
    if (iface.numa_node !== undefined && iface.numa_node >= 0) badges.push('<span class="badge badge-c2c">C2C</span>');
    if (iface.is_wifi) badges.push('<span class="badge badge-warn">&#9888; WIFI</span>');
    if (!iface.rdma_available && !iface.is_wifi) badges.push('<span class="badge badge-eth">ETHERNET</span>');
    const speedLabel = iface.speed_mbps >= 1000
      ? (iface.speed_mbps / 1000).toFixed(0) + ' GBPS'
      : iface.speed_mbps + ' MBPS';
    badges.push(`<span class="badge badge-speed">${speedLabel}</span>`);
    return badges.join(' ');
  }

  // Osiagalnosc interfejsu
  function getReachabilityInfo(nodeId, ifaceName) {
    if (!detectionResult || !detectionResult.all_results) return '';
    const reaches = [];
    const noReach = [];
    for (const other of selectedNodes) {
      if (other.node_id === nodeId) continue;
      const found = detectionResult.all_results.some(r =>
        ((r.node_a === nodeId && r.interface_a === ifaceName && r.node_b === other.node_id) ||
         (r.node_b === nodeId && r.interface_b === ifaceName && r.node_a === other.node_id)) &&
        r.reachable
      );
      if (found) reaches.push(other.hostname || other.node_id);
      else noReach.push(other.hostname || other.node_id);
    }
    if (noReach.length > 0) return `(\u2717 ${noReach.join(', ')})`;
    if (reaches.length > 0) return `(\u2713 all)`;
    return '';
  }

  // Macierz przepustowosci
  function renderBandwidthMatrix() {
    if (selectedNodes.length < 2) return '';

    const bottleneck = findBottleneck();

    return `
      <div class="matrix-container">
        <div class="matrix-label">${I18n.t('clusters.measured_bandwidth')}</div>
        <table class="bw-matrix">
          <thead>
            <tr>
              <th></th>
              ${selectedNodes.map(n => `<th>${Utils.escapeHtml(n.hostname || n.node_id)}</th>`).join('')}
            </tr>
          </thead>
          <tbody>
            ${selectedNodes.map((rowNode, i) => `
              <tr>
                <th>${Utils.escapeHtml(rowNode.hostname || rowNode.node_id)}</th>
                ${selectedNodes.map((colNode, j) => {
                  if (i === j) return '<td class="cell-self">&mdash;</td>';
                  const result = findProbeResult(rowNode.node_id, colNode.node_id);
                  if (!result) return `<td class="cell-probing">${I18n.t('clusters.probing')}</td>`;
                  if (!result.reachable) return '<td class="cell-unreachable">unreachable</td>';
                  const bw = result.bandwidth_mbps;
                  const cls = bw > 100000 ? 'cell-fast' : bw > 5000 ? 'cell-medium' : 'cell-slow';
                  const label = bw >= 1000 ? (bw / 1000).toFixed(1) + ' Gbps' : bw.toFixed(0) + ' Mbps';
                  return `<td class="${cls}" data-cell-pair="${Utils.escapeAttr(rowNode.node_id + ':' + colNode.node_id)}">${label}</td>`;
                }).join('')}
              </tr>
            `).join('')}
          </tbody>
        </table>
        ${bottleneck ? renderBottleneckBar(bottleneck) : ''}
      </div>
    `;
  }

  function findBottleneck() {
    if (probeResults.length === 0) return null;
    const reachable = probeResults.filter(r => r.reachable);
    if (reachable.length === 0) return null;
    return reachable.reduce((min, r) => (!min || r.bandwidth_mbps < min.bandwidth_mbps) ? r : min, null);
  }

  function renderBottleneckBar(result) {
    const bwLabel = result.bandwidth_mbps >= 1000
      ? (result.bandwidth_mbps / 1000).toFixed(1) + ' Gbps'
      : result.bandwidth_mbps.toFixed(0) + ' Mbps';
    const hostA = getHostname(result.node_a);
    const hostB = getHostname(result.node_b);
    const ifaceA = result.interface_a || '';
    const detail = `${Utils.escapeHtml(hostA)} \u2194 ${Utils.escapeHtml(hostB)}${ifaceA ? ` via ${Utils.escapeHtml(ifaceA)}` : ''}`;

    return `
      <div class="bottleneck-bar">
        <span class="icon">&#9888;</span>
        <span class="label">${I18n.t('clusters.bottleneck')}: ${bwLabel}</span>
        <span class="detail">${detail}</span>
      </div>
    `;
  }

  function findProbeResult(nodeA, nodeB) {
    return probeResults.find(r =>
      (r.node_a === nodeA && r.node_b === nodeB) ||
      (r.node_a === nodeB && r.node_b === nodeA)
    );
  }

  // --- Krok 3: Podsumowanie ---
  function renderStep3() {
    const totalVram = selectedNodes.reduce((sum, n) =>
      sum + (n.gpu_info || []).reduce((s, g) => s + (g.vram_total_mb || 0), 0), 0);
    const totalRam = selectedNodes.reduce((sum, n) => sum + (n.ram_total_mb || 0), 0);
    const totalCpu = selectedNodes.reduce((sum, n) => sum + (n.cpu_count || 0), 0);
    const bottleneck = detectionResult?.bottleneck_mbps || 0;
    const bottleneckLabel = bottleneck >= 1000
      ? (bottleneck / 1000).toFixed(1)
      : bottleneck.toFixed(0);
    const bottleneckUnit = bottleneck >= 1000 ? 'Gbps' : 'Mbps';
    const hasBottleneck = bottleneck > 0 && bottleneck < 10000;

    return `
      <div class="wizard-step-content">
        <h2 class="section-title">${Utils.escapeHtml(clusterName)}</h2>
        ${clusterDesc ? `<p class="section-subtitle">${Utils.escapeHtml(clusterDesc)}</p>` : ''}

        <div class="stats-grid">
          <div class="stat-card">
            <div class="stat-label">VRAM</div>
            <div class="stat-value">${(totalVram / 1024).toFixed(0)} <span class="stat-unit">GB</span></div>
          </div>
          <div class="stat-card">
            <div class="stat-label">RAM</div>
            <div class="stat-value">${Math.round(totalRam / 1024)} <span class="stat-unit">GB</span></div>
          </div>
          <div class="stat-card">
            <div class="stat-label">CPU</div>
            <div class="stat-value">${totalCpu} <span class="stat-unit">cores</span></div>
          </div>
          <div class="stat-card">
            <div class="stat-label">${I18n.t('clusters.interconnect')}</div>
            <div class="stat-value ${hasBottleneck ? 'stat-warning' : ''}">${bottleneckLabel} <span class="stat-unit">${bottleneckUnit}</span></div>
            ${hasBottleneck || detectionResult?.is_mixed ? `<div style="margin-top:4px;">
              ${hasBottleneck ? '<span class="badge badge-warn">&#9888; Bottleneck</span>' : ''}
              ${detectionResult?.is_mixed ? '<span class="badge badge-eth">Mixed</span>' : ''}
            </div>` : ''}
          </div>
        </div>

        <div class="wizard-links-table">
          <div class="matrix-label">${I18n.t('clusters.link_summary')}</div>
          <table class="links-table">
            <thead>
              <tr>
                <th>${I18n.t('clusters.pair')}</th>
                <th>Interface A</th>
                <th>Interface B</th>
                <th>${I18n.t('clusters.bandwidth')}</th>
                <th>${I18n.t('clusters.type')}</th>
              </tr>
            </thead>
            <tbody>
              ${renderLinkRows()}
            </tbody>
          </table>
        </div>
      </div>
    `;
  }

  function renderLinkRows() {
    const assignments = detectionResult?.assignments || [];
    return assignments.map(a => {
      if (!a.bandwidth_mbps && a.bandwidth_mbps !== 0) {
        return `<tr>
          <td style="color:var(--color-text-muted);">${Utils.escapeHtml(getHostname(a.node_a))} \u2194 ${Utils.escapeHtml(getHostname(a.node_b))}</td>
          <td style="color:var(--color-text-muted);">&mdash;</td>
          <td style="color:var(--color-text-muted);">&mdash;</td>
          <td style="color:var(--color-text-muted);">pending</td>
          <td></td>
        </tr>`;
      }

      const bwLabel = a.bandwidth_mbps >= 1000
        ? (a.bandwidth_mbps / 1000).toFixed(1) + ' Gbps'
        : a.bandwidth_mbps.toFixed(0) + ' Mbps';
      const bwColor = a.bandwidth_mbps > 100000 ? 'var(--color-success)' :
                      a.bandwidth_mbps > 5000 ? 'var(--color-warning)' : 'var(--color-error)';

      let typeBadges = '';
      if (a.rdma) {
        typeBadges = '<span class="badge badge-roce">ROCE</span> <span class="badge badge-c2c">C2C</span>';
      } else if (a.is_wifi) {
        typeBadges = '<span class="badge badge-warn">&#9888; WIFI</span>';
      } else {
        typeBadges = '<span class="badge badge-eth">ETHERNET</span>';
      }

      return `<tr>
        <td>${Utils.escapeHtml(getHostname(a.node_a))} \u2194 ${Utils.escapeHtml(getHostname(a.node_b))}</td>
        <td>${Utils.escapeHtml(a.interface_a)}</td>
        <td>${Utils.escapeHtml(a.interface_b)}</td>
        <td style="color:${bwColor};font-weight:600;">${bwLabel}</td>
        <td>${typeBadges}</td>
      </tr>`;
    }).join('');
  }

  function getHostname(nodeId) {
    const node = selectedNodes.find(n => n.node_id === nodeId);
    return node?.hostname || nodeId;
  }

  // --- Probe SSE ---
  async function startProbe() {
    isDetecting = true;
    probeResults = [];
    detectionResult = null;
    interfaceOverrides = {};
    renderCurrentStep();

    const nodes = selectedNodes.map(n => ({
      node_id: n.node_id,
      interfaces: (n.network_interfaces || []).map(iface => ({
        name: iface.name,
        ip: iface.ipv4_address || '',
        netmask: iface.ipv4_netmask || '255.255.255.0',
        speed_mbps: iface.speed_mbps || 0,
        rdma: iface.rdma_available || false,
      })).filter(i => i.ip && i.ip !== '' && i.name !== 'lo')
    }));

    try {
      const response = await ApiClient.post('/api/clusters/probe', { nodes });
      const probeId = response.probe_id;

      const token = localStorage.getItem('auth_token') || '';
      probeEventSource = new EventSource(`/api/clusters/probe/${probeId}?token=${token}`);

      probeEventSource.addEventListener('probe_result', (e) => {
        const data = JSON.parse(e.data);
        probeResults.push(data);
        renderCurrentStep();
      });

      probeEventSource.addEventListener('detection_complete', (e) => {
        const data = JSON.parse(e.data);
        detectionResult = data;
        isDetecting = false;
        if (probeEventSource) { probeEventSource.close(); probeEventSource = null; }
        renderCurrentStep();
      });

      probeEventSource.onerror = () => {
        isDetecting = false;
        if (probeEventSource) { probeEventSource.close(); probeEventSource = null; }
        renderCurrentStep();
      };
    } catch (err) {
      isDetecting = false;
      App.showToast(err.message || 'Probe failed', 'error');
      renderCurrentStep();
    }
  }

  // --- Eventy ---
  function bindEvents() {
    document.getElementById('wiz-back')?.addEventListener('click', prevStep);
    document.getElementById('wiz-cancel')?.addEventListener('click', close);
    document.getElementById('wiz-next')?.addEventListener('click', handleNext);
    bindStepEvents();
  }

  function bindStepEvents() {
    // Krok 1: nazwa
    const nameInput = document.getElementById('wiz-name');
    if (nameInput) {
      nameInput.addEventListener('input', (e) => {
        clusterName = e.target.value;
        updateNextButton();
      });
    }
    const descInput = document.getElementById('wiz-desc');
    if (descInput) {
      descInput.addEventListener('input', (e) => { clusterDesc = e.target.value; });
    }

    // Krok 1: zaznaczanie nodow (klik na karte podswietla, bez checkboxa)
    document.querySelectorAll('[data-select-node]').forEach(card => {
      card.addEventListener('click', () => {
        const nodeId = card.dataset.selectNode;
        const node = allNodes.find(n => n.node_id === nodeId);
        if (!node) return;

        const idx = selectedNodes.findIndex(n => n.node_id === nodeId);
        if (idx >= 0) selectedNodes.splice(idx, 1);
        else selectedNodes.push(node);

        render();
      });
    });

    // Krok 2: zmiana interfejsu
    document.querySelectorAll('[data-node-iface]').forEach(select => {
      select.addEventListener('change', (e) => {
        const nodeId = select.dataset.nodeIface;
        interfaceOverrides[nodeId] = e.target.value;
        renderCurrentStep();
      });
    });

    // Rebind nawigacji
    document.getElementById('wiz-back')?.addEventListener('click', prevStep);
    document.getElementById('wiz-cancel')?.addEventListener('click', close);
    document.getElementById('wiz-next')?.addEventListener('click', handleNext);
  }

  function updateNextButton() {
    const nextBtn = document.getElementById('wiz-next');
    if (!nextBtn) return;
    if (currentStep === 1) {
      nextBtn.disabled = !(clusterName.trim().length > 0 && selectedNodes.length >= 2);
    }
  }

  async function handleNext() {
    if (currentStep === 3) {
      await createCluster();
      return;
    }

    currentStep++;

    if (currentStep === 2 && !detectionResult) {
      render();
      startProbe();
      return;
    }

    render();
  }

  function prevStep() {
    if (currentStep <= 1) return;
    currentStep--;
    render();
  }

  async function createCluster() {
    const nextBtn = document.getElementById('wiz-next');
    if (nextBtn) { nextBtn.disabled = true; nextBtn.textContent = '...'; }

    try {
      const members = selectedNodes.map(n => {
        const assignment = detectionResult?.per_node?.[n.node_id];
        const override = interfaceOverrides[n.node_id];
        const ifaceName = override || assignment?.interface || '';
        const iface = (n.network_interfaces || []).find(i => i.name === ifaceName);

        return {
          node_id: n.node_id,
          role: 'worker',
          interface_name: ifaceName,
          interface_ip: iface?.ipv4_address || '',
          interface_speed_mbps: iface?.speed_mbps || 0,
          interface_type: iface?.rdma_available ? 'rdma' : 'ethernet',
        };
      });

      const cluster = await ApiClient.post('/api/clusters', {
        name: clusterName,
        description: clusterDesc,
      });

      for (const m of members) {
        await ApiClient.post(`/api/clusters/${encodeURIComponent(cluster.cluster_id)}/members`, m);
      }

      App.showToast(I18n.t('clusters.create_success').replace('{name}', clusterName), 'success');
      close();
    } catch (err) {
      App.showToast(err.message || I18n.t('common.error'), 'error');
      if (nextBtn) { nextBtn.disabled = false; nextBtn.innerHTML = `&#10003; ${I18n.t('clusters.create_cluster')}`; }
    }
  }

  return { open, close };
})();

// =============================================================================
// Plik: modules/cluster-wizard.js
// Opis: 2-krokowy wizard tworzenia/edycji klastra. Krok 1: nazwa, opis,
//       strategia, failover. Krok 2: wybor nodow + live probe sieci (SSE).
//       Render w tf-window; wizard-step-indicator reusowany z engine-deploy.
// =============================================================================

import {
  byId,
  escapeHtml,
  escapeAttr,
  toast,
  apiGet,
  formatMb,
} from '/js/utils.js';
import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-input.js';
import '/js/components/tf-select.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-window.js';

// Stan wizarda (reset przy open).
let editCluster = null;
let onDoneCb = null;
let currentStep = 1;
let fName = '';
let fDesc = '';
let fStrategy = 'distributed';
let fFailover = false;
let fFailoverTarget = '';
let fHealthInterval = 5000;
let fTimeout = 10000;

let availableNodes = [];
let selectedIds = new Set();
let probeInProgress = false;
let probeResults = [];
let probeUnsub = null;
let winEl = null;
let backdropEl = null;

const ClusterWizard = {
  open({ cluster = null, onDone = null } = {}) {
    editCluster = cluster;
    onDoneCb = onDone;
    currentStep = 1;
    probeResults = [];
    probeInProgress = false;
    selectedIds = new Set();

    if (cluster) {
      fName = cluster.name || '';
      fDesc = cluster.description || '';
      fStrategy = cluster.strategy || 'distributed';
      fFailover = !!(cluster.failoverEnabled ?? cluster.failover_enabled);
      fFailoverTarget = cluster.failoverTarget || cluster.failover_target || '';
      fHealthInterval = cluster.healthCheckIntervalMs || cluster.health_check_interval_ms || 5000;
      fTimeout = cluster.timeoutMs || cluster.timeout_ms || 10000;
      const members = cluster.members || cluster.nodes || [];
      for (const m of members) selectedIds.add(m.nodeId || m.node_id || m.id);
    } else {
      fName = '';
      fDesc = '';
      fStrategy = 'distributed';
      fFailover = false;
      fFailoverTarget = '';
      fHealthInterval = 5000;
      fTimeout = 10000;
    }

    renderShell();
    loadNodes().then(() => refreshBody());
  },
  close,
};

function close() {
  if (probeUnsub) { try { probeUnsub(); } catch (_) {} probeUnsub = null; }
  probeInProgress = false;
  if (winEl && winEl.isConnected) winEl.remove();
  if (backdropEl && backdropEl.isConnected) backdropEl.remove();
  winEl = null;
  backdropEl = null;
}

// ---- Data ----------------------------------------------------------------

async function loadNodes() {
  try {
    const all = await ApiBinary.list('meshNodeListRequest', { arrayKey: 'nodes' });
    availableNodes = (Array.isArray(all) ? all : []).filter(n => {
      if (n.is_local) return true;
      const trust = String(n.trust || '').toLowerCase();
      const trusted = trust === 'trusted' || trust === 'paired' || n.is_trusted === true;
      return trusted;
    });
    availableNodes.sort((a, b) => {
      if (a.is_local && !b.is_local) return -1;
      if (!a.is_local && b.is_local) return 1;
      return String(a.hostname || '').localeCompare(String(b.hostname || ''));
    });
  } catch (_) {
    availableNodes = [];
  }
}

// ---- Shell ---------------------------------------------------------------

function renderShell() {
  close();
  backdropEl = document.createElement('div');
  backdropEl.className = 'tf-window-backdrop';
  document.body.appendChild(backdropEl);

  winEl = document.createElement('tf-window');
  winEl.setAttribute('title', editCluster ? I18n.t('clusters.edit_title') : I18n.t('clusters.create_title'));
  winEl.setAttribute('buttons', 'close');
  winEl.setAttribute('draggable', '');
  winEl.setAttribute('modal', '');
  winEl.setAttribute('width', '720');
  winEl.setAttribute('min-width', '520');
  winEl.setAttribute('initial-x', 'center');
  winEl.setAttribute('initial-y', 'center');
  winEl.innerHTML = `
    <div slot="body" id="cw-body"></div>
    <div slot="footer" id="cw-footer"></div>
  `;
  document.body.appendChild(winEl);

  winEl.addEventListener('action', (e) => {
    const a = e.detail?.action;
    if (a === 'close' || a === 'cancel') {
      close();
    }
  });
  winEl.addEventListener('close-request', () => {
    // Uzytkownik klika "x" w headerze: zamknij wizard + backdrop.
    if (backdropEl && backdropEl.isConnected) backdropEl.remove();
  });
  backdropEl.addEventListener('click', () => close());
}

function refreshBody() {
  const body = byId('cw-body');
  if (body) body.innerHTML = renderStepIndicator() + renderStepContent();
  const footer = byId('cw-footer');
  if (footer) footer.innerHTML = renderFooter();
  bindStepInputs();
  bindFooter();
}

function renderStepIndicator() {
  let html = '<div class="wizard-step-indicator">';
  for (let i = 1; i <= 2; i++) {
    const cls = i === currentStep ? 'active' : (i < currentStep ? 'done' : '');
    html += `<div class="wizard-step-dot ${cls}"><span>${i}</span></div>`;
    if (i < 2) html += '<div class="wizard-step-line"></div>';
  }
  html += '</div>';
  const labels = [I18n.t('cluster_wizard.step1_label'), I18n.t('cluster_wizard.step2_label')];
  html += `<div class="wizard-step-title">${escapeHtml(labels[currentStep - 1])}</div>`;
  return html;
}

function renderStepContent() {
  return currentStep === 1 ? renderStep1() : renderStep2();
}

// ---- Step 1 --------------------------------------------------------------

function renderStep1() {
  return `
    <div class="form-group">
      <tf-input id="cw-name" label="${escapeAttr(I18n.t('clusters.name'))}" value="${escapeAttr(fName)}" placeholder="np. GPU-Farm" maxlength="80"></tf-input>
    </div>
    <div class="form-group">
      <tf-input id="cw-desc" label="${escapeAttr(I18n.t('clusters.description'))}" value="${escapeAttr(fDesc)}" placeholder="${escapeAttr(I18n.t('common.optional'))}" maxlength="200"></tf-input>
    </div>
    <div class="form-group">
      <label class="cw-label">${escapeHtml(I18n.t('cluster_wizard.lb_strategy'))}</label>
      <tf-select id="cw-strategy" value="${escapeAttr(fStrategy)}">
        <option value="distributed"${fStrategy === 'distributed' ? ' selected' : ''}>${escapeHtml(I18n.t('clusters.strategy_distributed'))}</option>
        <option value="replicated"${fStrategy === 'replicated' ? ' selected' : ''}>${escapeHtml(I18n.t('clusters.strategy_replicated'))}</option>
        <option value="primary_replica"${fStrategy === 'primary_replica' ? ' selected' : ''}>${escapeHtml(I18n.t('clusters.strategy_primary_replica'))}</option>
      </tf-select>
    </div>
    <div class="form-group cw-inline">
      <label class="cw-label">${escapeHtml(I18n.t('cluster_wizard.failover_enabled'))}</label>
      <tf-toggle id="cw-failover" ${fFailover ? 'checked' : ''}></tf-toggle>
    </div>
    <div class="form-group" id="cw-failover-target-wrap" ${fFailover ? '' : 'style="display:none;"'}>
      <tf-input id="cw-failover-target" label="${escapeAttr(I18n.t('cluster_wizard.failover_target'))}" value="${escapeAttr(fFailoverTarget)}" placeholder="${escapeAttr(I18n.t('cluster_wizard.failover_target_hint'))}"></tf-input>
    </div>
    <div class="cw-row-2">
      <div class="form-group">
        <tf-input id="cw-health-interval" type="number" label="${escapeAttr(I18n.t('cluster_wizard.health_interval_ms'))}" value="${escapeAttr(fHealthInterval)}" min="1000" max="60000"></tf-input>
      </div>
      <div class="form-group">
        <tf-input id="cw-timeout" type="number" label="${escapeAttr(I18n.t('cluster_wizard.timeout_ms'))}" value="${escapeAttr(fTimeout)}" min="1000" max="60000"></tf-input>
      </div>
    </div>
  `;
}

// ---- Step 2 --------------------------------------------------------------

function renderStep2() {
  if (availableNodes.length === 0) {
    return `
      <div class="empty-state">
        <div class="empty-state-text">${escapeHtml(I18n.t('clusters.no_trusted_nodes'))}</div>
      </div>
    `;
  }

  const rows = availableNodes.map(n => {
    const id = n.node_id || n.id;
    const checked = selectedIds.has(id) ? 'checked' : '';
    const gpus = Array.isArray(n.gpu_info) ? n.gpu_info : [];
    const vramTotal = gpus.reduce((s, g) => s + (g.vram_total_mb || 0), 0);
    const ramTotal = n.ram_total_mb || 0;
    const online = isOnline(n);
    const statusChip = online
      ? `<tf-chip status="online" dot>${escapeHtml(I18n.t('mesh.online'))}</tf-chip>`
      : `<tf-chip status="offline" dot>${escapeHtml(I18n.t('mesh.offline'))}</tf-chip>`;
    return `
      <tr>
        <td><input type="checkbox" data-node-id="${escapeAttr(id)}" ${checked} class="cw-node-check"></td>
        <td><strong>${escapeHtml(n.hostname || id)}</strong>${n.is_local ? ` <tf-chip status="accent">${escapeHtml(I18n.t('mesh.local'))}</tf-chip>` : ''}</td>
        <td>${n.cpu_count || '—'}</td>
        <td>${ramTotal ? formatMb(ramTotal) : '—'}</td>
        <td>${gpus.length > 0 ? gpus.length : '—'}</td>
        <td>${vramTotal > 0 ? formatMb(vramTotal) : '—'}</td>
        <td>${statusChip}</td>
      </tr>
    `;
  }).join('');

  const countLabel = selectedIds.size >= 2
    ? `<span class="cw-count-ok">${selectedIds.size} ${escapeHtml(I18n.t('clusters.nodes_selected'))}</span>`
    : `<span class="cw-count-warn">${escapeHtml(I18n.t('clusters.select_min_nodes'))}</span>`;

  const probeBtn = selectedIds.size >= 2
    ? `<tf-button variant="secondary" size="sm" id="cw-run-probe" ${probeInProgress ? 'disabled' : ''}>${escapeHtml(probeInProgress ? I18n.t('cluster_wizard.probing') : I18n.t('cluster_wizard.run_probe'))}</tf-button>`
    : '';

  const matrix = selectedIds.size >= 2 ? renderProbeMatrix() : '';

  return `
    <div class="cw-step2">
      <div class="cw-step2-head">
        <div class="cw-count">${countLabel}</div>
        <div>${probeBtn}</div>
      </div>
      <table class="data-table cw-nodes-table">
        <thead>
          <tr>
            <th></th>
            <th>${escapeHtml(I18n.t('cluster_wizard.node'))}</th>
            <th>CPU</th>
            <th>RAM</th>
            <th>GPU</th>
            <th>VRAM</th>
            <th>${escapeHtml(I18n.t('cluster_wizard.status'))}</th>
          </tr>
        </thead>
        <tbody>${rows}</tbody>
      </table>
      ${matrix}
    </div>
  `;
}

function renderProbeMatrix() {
  const selected = availableNodes.filter(n => selectedIds.has(n.node_id || n.id));
  if (selected.length < 2) return '';

  const headers = selected.map(n => `<th>${escapeHtml(n.hostname || n.node_id)}</th>`).join('');
  const rows = selected.map((rowN, i) => {
    const cells = selected.map((colN, j) => {
      if (i === j) return '<td class="cell-self">—</td>';
      const res = findProbeBetween(rowN.node_id || rowN.id, colN.node_id || colN.id);
      if (!res) {
        return `<td class="cell-pending">${probeInProgress ? escapeHtml(I18n.t('clusters.probing')) : '—'}</td>`;
      }
      if (!res.reachable) {
        return '<td class="cell-fail">✗</td>';
      }
      const bw = res.bandwidth_mbps || 0;
      const bwLabel = bw >= 1000 ? `${(bw / 1000).toFixed(1)} Gbps` : `${bw.toFixed(0)} Mbps`;
      const cls = bw > 40000 ? 'ok' : (bw > 5000 ? 'warn' : 'slow');
      return `<td class="cell-result ${cls}">${escapeHtml(bwLabel)}</td>`;
    }).join('');
    return `<tr><th>${escapeHtml(rowN.hostname || rowN.node_id)}</th>${cells}</tr>`;
  }).join('');

  const summary = renderProbeSummary(selected);

  return `
    <div class="cw-matrix-wrap">
      <div class="cw-matrix-title">${escapeHtml(I18n.t('cluster_wizard.probe_matrix'))}</div>
      <table class="cluster-matrix"><thead><tr><th></th>${headers}</tr></thead><tbody>${rows}</tbody></table>
      ${summary}
    </div>
  `;
}

function renderProbeSummary(selected) {
  if (probeResults.length === 0) return '';
  const reachable = probeResults.filter(r => r.reachable);
  const total = (selected.length * (selected.length - 1)) / 2;
  if (probeInProgress) {
    return `<div class="cw-banner info">${escapeHtml(I18n.t('cluster_wizard.probe_progress').replace('{done}', reachable.length).replace('{total}', total))}</div>`;
  }
  if (reachable.length === 0) {
    return `<div class="cw-banner warn">${escapeHtml(I18n.t('cluster_wizard.probe_no_links'))}</div>`;
  }
  const min = reachable.reduce((m, r) => (!m || r.bandwidth_mbps < m.bandwidth_mbps) ? r : m, null);
  const bw = min ? (min.bandwidth_mbps >= 1000 ? `${(min.bandwidth_mbps / 1000).toFixed(1)} Gbps` : `${min.bandwidth_mbps.toFixed(0)} Mbps`) : '';
  return `<div class="cw-banner ok">${escapeHtml(I18n.t('cluster_wizard.probe_done').replace('{slowest}', bw))}</div>`;
}

function findProbeBetween(a, b) {
  if (!probeResults || probeResults.length === 0) return null;
  const matches = probeResults.filter(r =>
    (r.node_a === a && r.node_b === b) || (r.node_a === b && r.node_b === a)
  );
  if (matches.length === 0) return null;
  const reachable = matches.filter(r => r.reachable);
  if (reachable.length > 0) {
    return reachable.reduce((best, r) => r.bandwidth_mbps > best.bandwidth_mbps ? r : best);
  }
  return matches[0];
}

// ---- Footer --------------------------------------------------------------

function renderFooter() {
  const primaryLabel = currentStep === 2
    ? (editCluster ? I18n.t('common.save') : I18n.t('clusters.create_cluster'))
    : I18n.t('common.next');
  const canPrimary = currentStep === 1
    ? fName.trim().length > 0
    : selectedIds.size >= 2;

  return `
    <tf-button variant="ghost" id="cw-cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
    ${currentStep === 2 ? `<tf-button variant="secondary" id="cw-back">${escapeHtml(I18n.t('common.back'))}</tf-button>` : ''}
    <tf-button variant="primary" id="cw-primary" ${canPrimary ? '' : 'disabled'}>${escapeHtml(primaryLabel)}</tf-button>
  `;
}

// ---- Bindings ------------------------------------------------------------

function bindStepInputs() {
  if (currentStep === 1) {
    byId('cw-name')?.addEventListener('input', (e) => {
      fName = e.target.value || '';
      updatePrimaryBtn();
    });
    byId('cw-desc')?.addEventListener('input', (e) => { fDesc = e.target.value || ''; });
    byId('cw-strategy')?.addEventListener('change', (e) => { fStrategy = e.target.value || 'distributed'; });
    byId('cw-failover')?.addEventListener('change', (e) => {
      fFailover = !!e.target.checked;
      const wrap = byId('cw-failover-target-wrap');
      if (wrap) wrap.style.display = fFailover ? '' : 'none';
    });
    byId('cw-failover-target')?.addEventListener('input', (e) => { fFailoverTarget = e.target.value || ''; });
    byId('cw-health-interval')?.addEventListener('input', (e) => { fHealthInterval = parseInt(e.target.value, 10) || 5000; });
    byId('cw-timeout')?.addEventListener('input', (e) => { fTimeout = parseInt(e.target.value, 10) || 10000; });
  } else {
    // Step 2: checkboxy + probe btn
    document.querySelectorAll('.cw-node-check').forEach(cb => {
      cb.addEventListener('change', (e) => {
        const id = e.target.dataset.nodeId;
        if (e.target.checked) selectedIds.add(id);
        else selectedIds.delete(id);
        // Rerender krok 2 — zmienil sie licznik i matryca.
        refreshBody();
      });
    });
    byId('cw-run-probe')?.addEventListener('click', startProbe);
  }
}

function bindFooter() {
  byId('cw-cancel')?.addEventListener('click', close);
  byId('cw-back')?.addEventListener('click', () => {
    if (currentStep > 1) {
      currentStep--;
      refreshBody();
    }
  });
  byId('cw-primary')?.addEventListener('click', async () => {
    if (currentStep === 1) {
      if (fName.trim().length === 0) return;
      currentStep = 2;
      refreshBody();
      return;
    }
    await submitCluster();
  });
}

function updatePrimaryBtn() {
  const btn = byId('cw-primary');
  if (!btn) return;
  const can = currentStep === 1 ? fName.trim().length > 0 : selectedIds.size >= 2;
  if (can) btn.removeAttribute('disabled');
  else btn.setAttribute('disabled', '');
}

// ---- Probe ---------------------------------------------------------------

async function startProbe() {
  if (probeInProgress) return;
  const selected = availableNodes.filter(n => selectedIds.has(n.node_id || n.id));
  if (selected.length < 2) return;

  probeInProgress = true;
  probeResults = [];
  refreshBody();

  const nodeIds = selected.map(n => n.node_id || n.id);

  try {
    probeUnsub = await ApiBinary.subscribe(
      'clusterProbeStreamRequest',
      { nodeIds },
      {
        onChunk: (chunk) => {
          if (chunk.eventType === 'result' && chunk.sourceNode && chunk.targetNode) {
            probeResults.push({
              node_a: chunk.sourceNode,
              node_b: chunk.targetNode,
              reachable: !!chunk.success,
              bandwidth_mbps: chunk.bandwidthMbps || 0,
              latency_us: chunk.latencyMs ? chunk.latencyMs * 1000 : 0,
              rdma: String(chunk.interfaceType || '').toLowerCase() === 'rdma',
              interface_a: chunk.interfaceType || '',
              interface_b: chunk.interfaceType || '',
            });
            refreshBody();
          }
        },
        onEnd: () => {
          probeInProgress = false;
          probeUnsub = null;
          refreshBody();
        },
        onError: (err) => {
          probeInProgress = false;
          probeUnsub = null;
          toast(`${I18n.t('common.error')}: ${err.message ?? 'probe error'}`, 'error');
          refreshBody();
        },
      },
    );
  } catch (err) {
    probeInProgress = false;
    toast(err.message || I18n.t('common.error'), 'error');
    refreshBody();
  }
}

// ---- Submit --------------------------------------------------------------

async function submitCluster() {
  if (fName.trim().length === 0) {
    toast(I18n.t('common.required'), 'warning');
    return;
  }
  if (selectedIds.size < 2) {
    toast(I18n.t('clusters.select_min_nodes'), 'warning');
    return;
  }

  const btn = byId('cw-primary');
  if (btn) btn.setAttribute('disabled', '');

  try {
    let clusterId;
    if (editCluster) {
      clusterId = editCluster.id || editCluster.cluster_id;
      await ApiBinary.action('clusterUpdateRequest', {
        clusterId,
        name: fName.trim(),
        description: fDesc.trim() || null,
        strategy: fStrategy,
        failoverEnabled: fFailover,
        failoverTarget: fFailoverTarget || null,
        healthCheckIntervalMs: fHealthInterval,
        timeoutMs: fTimeout,
      });
      // Sync czlonkow: usun ktorych juz nie ma, dodaj nowych.
      const prevIds = new Set(
        (editCluster.members || editCluster.nodes || []).map(m => m.nodeId || m.node_id || m.id),
      );
      const newIds = new Set(selectedIds);
      for (const pid of prevIds) {
        if (!newIds.has(pid)) {
          await ApiBinary.action('clusterRemoveMemberRequest', {
            clusterId,
            nodeId: pid,
          }).catch(() => {});
        }
      }
      for (const nid of newIds) {
        if (!prevIds.has(nid)) {
          await addMember(clusterId, nid);
        }
      }
    } else {
      const created = await ApiBinary.action('clusterCreateRequest', {
        name: fName.trim(),
        description: fDesc.trim() || null,
        strategy: fStrategy,
        failoverEnabled: fFailover,
        failoverTarget: fFailoverTarget || null,
        healthCheckIntervalMs: fHealthInterval,
        timeoutMs: fTimeout,
      });
      clusterId = created.clusterId || created.cluster_id || created.id;
      for (const nid of selectedIds) {
        await addMember(clusterId, nid);
      }
    }

    toast(
      (editCluster ? I18n.t('clusters.update_success') : I18n.t('clusters.create_success'))
        .replace('{name}', fName.trim()),
      'success'
    );
    close();
    if (typeof onDoneCb === 'function') onDoneCb();
  } catch (err) {
    toast(err.message || I18n.t('common.error'), 'error');
    if (btn) btn.removeAttribute('disabled');
  }
}

async function addMember(clusterId, nodeId) {
  const node = availableNodes.find(n => (n.node_id || n.id) === nodeId);
  const ifaces = node && Array.isArray(node.network_interfaces) ? node.network_interfaces : [];
  // Wybierz najszybszy reachable interface (np. z probe) albo pierwszy non-lo.
  const probeIface = pickBestInterfaceFromProbe(nodeId) || ifaces.find(i => i.ipv4_address && i.name !== 'lo');
  await ApiBinary.action('clusterAddMemberRequest', {
    clusterId,
    nodeId,
    interfaceType: probeIface?.rdma_available ? 'rdma' : 'ethernet',
    interfaceSpeedMbps: probeIface?.speed_mbps || 0,
  });
}

function pickBestInterfaceFromProbe(nodeId) {
  if (probeResults.length === 0) return null;
  const reachable = probeResults.filter(r => r.reachable && (r.node_a === nodeId || r.node_b === nodeId));
  if (reachable.length === 0) return null;
  const best = reachable.reduce((m, r) => r.bandwidth_mbps > m.bandwidth_mbps ? r : m);
  const ifaceName = best.node_a === nodeId ? best.interface_a : best.interface_b;
  const node = availableNodes.find(n => (n.node_id || n.id) === nodeId);
  const ifaces = node && Array.isArray(node.network_interfaces) ? node.network_interfaces : [];
  return ifaces.find(i => i.name === ifaceName) || null;
}

// ---- Helpers -------------------------------------------------------------

function isOnline(node) {
  if (!node) return false;
  if (node.is_local) return true;
  const s = String(node.status || '').toLowerCase();
  return s === 'connected' || s === 'online' || s === 'active' || s === 'ready';
}

export default ClusterWizard;

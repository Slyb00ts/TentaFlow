// =============================================================================
// Plik: modules/clusters.js
// Opis: Widok Clusters — lista kafli klastrow z ring-gauges (CPU/RAM/VRAM/GPU),
//       node chips, connection-type chips, footer metryki i akcje icon-only.
//       Auto-refresh 5s przez patchInner. Klik kafla -> ClusterDetailScreen.
//       Nowy cluster / edycja przez ClusterWizard.
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
import { patchInner } from '/js/lib/patch.js';
import ClusterDetailScreen from '/js/modules/cluster-detail.js';
import ClusterWizard from '/js/modules/cluster-wizard.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-window.js';

let clusters = [];
let nodesById = new Map();
let refreshInterval = null;

const ClustersScreen = {
  title: 'Clusters',
  render() {
    return `
      <div class="clusters-shell">
        <div class="page-header" id="clusters-page-header">
          <div>
            <h1>${escapeHtml(I18n.t('clusters.title'))}</h1>
            <div class="sub" id="clusters-sub"></div>
          </div>
          <div class="actions">
            <tf-button variant="primary" icon="plus" id="btn-new-cluster">${escapeHtml(I18n.t('clusters.new'))}</tf-button>
          </div>
        </div>

        <div id="clusters-list-host">
          <div class="mesh-loading">${escapeHtml(I18n.t('common.loading'))}</div>
        </div>
      </div>
    `;
  },
  async mount() {
    byId('btn-new-cluster')?.addEventListener('click', () => openWizard(null));

    const host = byId('clusters-list-host');
    if (host) host.addEventListener('click', handleListClick);

    await loadAll();
    renderList();

    refreshInterval = setInterval(async () => {
      if (!document.querySelector('.clusters-shell')) {
        stopRefresh();
        return;
      }
      await loadAll();
      renderList();
    }, 5000);
  },
  unmount() {
    stopRefresh();
    const host = byId('clusters-list-host');
    if (host) host.removeEventListener('click', handleListClick);
    clusters = [];
    nodesById = new Map();
  },
};

function stopRefresh() {
  if (refreshInterval) {
    clearInterval(refreshInterval);
    refreshInterval = null;
  }
}

// ---- Data ----------------------------------------------------------------

async function loadAll() {
  try {
    // Klastry idą binarnie; mesh nodes pozostaje na REST (brak binarnego odpowiednika
    // dla pełnych info o nodach: gpus, network_interfaces, ram_total_mb itp.).
    const [clustersBody, nodesResp] = await Promise.all([
      ApiBinary.one('clusterListRequest').catch(() => null),
      ApiBinary.list('meshNodeListRequest', { arrayKey: 'nodes' }).catch(() => []),
    ]);
    clusters = Array.isArray(clustersBody?.clusters) ? clustersBody.clusters : [];
    const nodes = Array.isArray(nodesResp) ? nodesResp : [];
    nodesById = new Map(nodes.map(n => [n.node_id || n.id, n]));
    updateSubheader();
  } catch (err) {
    toast(`${I18n.t('clusters.title')}: ${err.message}`, 'error');
  }
}

function updateSubheader() {
  const sub = byId('clusters-sub');
  if (!sub) return;
  const total = clusters.length;
  const online = clusters.filter(c => clusterStatus(c) !== 'offline').length;
  const parts = [
    `${total} ${escapeHtml(I18n.t(total === 1 ? 'clusters.count_one' : 'clusters.count_many'))}`,
  ];
  if (total > 0) {
    parts.push(`${online} ${escapeHtml(I18n.t('clusters.healthy_short'))}`);
  }
  sub.textContent = parts.join(' · ');
}

// ---- Render ---------------------------------------------------------------

function renderList() {
  const host = byId('clusters-list-host');
  if (!host) return;

  if (clusters.length === 0) {
    patchInner(host, `
      <div class="empty-state">
        <div class="empty-state-text">${escapeHtml(I18n.t('clusters.noClusters'))}</div>
      </div>
    `);
    return;
  }

  patchInner(host, `<div class="clusters-grid">${clusters.map(renderClusterCard).join('')}</div>`);
}

function renderClusterCard(cluster) {
  const clusterId = cluster.id || cluster.cluster_id;
  const members = resolveMembers(cluster);
  const memberCount = members.length;
  const status = clusterStatus(cluster);
  const statusClass = status === 'offline' ? 'offline' : (status === 'degraded' ? 'degraded' : 'healthy');

  // Agregacja metryk po wszystkich nodach.
  const agg = aggregateMetrics(members);

  const statusChip = renderStatusChip(status);

  const ringsHtml = `
    <div class="cluster-gauges">
      ${renderRing('CPU', agg.cpuPct != null ? `${agg.cpuPct}` : '—', agg.cpuPct != null ? '%' : '', agg.cpuSub || '', agg.cpuPct)}
      ${renderRing('RAM', agg.ramUsedLabel || '—', '', agg.ramSub || '', agg.ramPct)}
      ${renderRing('VRAM', agg.vramUsedLabel || '—', '', agg.vramSub || '', agg.vramPct)}
      ${renderRing(escapeHtml(I18n.t('clusters.gauge_gpu')), agg.gpuPct != null ? `${agg.gpuPct}` : '—', agg.gpuPct != null ? '%' : '', agg.gpuSub || '', agg.gpuPct)}
    </div>
  `;

  const nodeChips = members.length > 0
    ? `<div class="cluster-node-chips">${members.map(m => renderNodeChip(m)).join('')}</div>`
    : `<div class="cluster-node-chips muted">${escapeHtml(I18n.t('clusters.no_members'))}</div>`;

  const linkChips = renderConnectionChips(members);

  const footerMeta = renderFooterMeta(cluster, agg);

  return `
    <div class="cluster-card ${statusClass}" data-cluster-detail="${escapeAttr(clusterId)}">
      <div class="cluster-card-head">
        <div class="cluster-card-ico">
          <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <circle cx="12" cy="5" r="2.3"/>
            <circle cx="5" cy="17" r="2.3"/>
            <circle cx="19" cy="17" r="2.3"/>
            <path d="M12 7.5v4M10.3 12.5l-4 3.5M13.7 12.5l4 3.5"/>
          </svg>
        </div>
        <div class="cluster-card-titlebox">
          <div class="cluster-card-title">${escapeHtml(cluster.name || clusterId || '—')} ${statusChip}</div>
          <div class="cluster-card-desc">${escapeHtml(cluster.description || '')}</div>
        </div>
        <div class="cluster-card-actions">
          <tf-button variant="ghost" size="sm" icon="settings" title="${escapeAttr(I18n.t('common.edit'))}" data-edit-cluster="${escapeAttr(clusterId)}"></tf-button>
          <tf-button variant="ghost" size="sm" icon="share" title="${escapeAttr(I18n.t('clusters.test_connections'))}" data-test-cluster="${escapeAttr(clusterId)}"></tf-button>
          <tf-button variant="ghost" size="sm" icon="trash" title="${escapeAttr(I18n.t('common.delete'))}" data-delete-cluster="${escapeAttr(clusterId)}"></tf-button>
        </div>
      </div>

      ${ringsHtml}
      ${nodeChips}
      ${linkChips}
      ${footerMeta}
    </div>
  `;
}

// ---- Cluster helpers ------------------------------------------------------

function resolveMembers(cluster) {
  const raw = cluster.members || cluster.nodes || [];
  return raw.map(m => {
    const nodeId = m.node_id || m.id;
    const live = nodesById.get(nodeId);
    return {
      node_id: nodeId,
      role: m.role || 'worker',
      hostname: (live && live.hostname) || m.hostname || m.node_name || nodeId,
      interface_type: m.interface_type || '',
      interface_speed_mbps: m.interface_speed_mbps || 0,
      live,
    };
  });
}

function clusterStatus(cluster) {
  const members = resolveMembers(cluster);
  if (members.length === 0) return 'offline';
  const onlineCnt = members.filter(m => isOnline(m.live)).length;
  if (onlineCnt === 0) return 'offline';
  if (onlineCnt < members.length) return 'degraded';
  return 'healthy';
}

function isOnline(node) {
  if (!node) return false;
  if (node.is_local) return true;
  const s = String(node.status || '').toLowerCase();
  return s === 'connected' || s === 'online' || s === 'active' || s === 'ready';
}

function renderStatusChip(status) {
  if (status === 'healthy') return `<tf-chip status="online" dot>${escapeHtml(I18n.t('clusters.status_healthy'))}</tf-chip>`;
  if (status === 'degraded') return `<tf-chip status="warning" dot>${escapeHtml(I18n.t('clusters.status_degraded'))}</tf-chip>`;
  return `<tf-chip status="offline" dot>${escapeHtml(I18n.t('clusters.status_offline'))}</tf-chip>`;
}

// ---- Aggregacja metryk ----------------------------------------------------

function aggregateMetrics(members) {
  const live = members.map(m => m.live).filter(Boolean);
  const n = live.length;

  // CPU: srednia z uzyciem, suma corsow
  let cpuSum = 0, cpuCnt = 0, coresTotal = 0;
  for (const x of live) {
    const v = x.cpu_usage ?? x.cpu_usage_percent;
    if (v != null && !isNaN(v)) { cpuSum += v; cpuCnt++; }
    if (x.cpu_count) coresTotal += x.cpu_count;
  }
  const cpuPct = cpuCnt > 0 ? Math.round(cpuSum / cpuCnt) : null;

  // RAM — suma used / suma total
  let ramUsed = 0, ramTotal = 0;
  for (const x of live) {
    if (x.ram_used_mb) ramUsed += x.ram_used_mb;
    if (x.ram_total_mb) ramTotal += x.ram_total_mb;
  }
  const ramPct = ramTotal > 0 ? Math.round((ramUsed / ramTotal) * 100) : null;

  // VRAM — suma po wszystkich GPU na wszystkich nodach
  let vramUsed = 0, vramTotal = 0, gpuCount = 0, gpuUsageSum = 0, gpuUsageCnt = 0;
  for (const x of live) {
    const gpus = Array.isArray(x.gpus) ? x.gpus : [];
    for (const g of gpus) {
      if (g.vram_used_mb) vramUsed += g.vram_used_mb;
      if (g.vram_total_mb) vramTotal += g.vram_total_mb;
      if (g.usage_percent != null) { gpuUsageSum += g.usage_percent; gpuUsageCnt++; }
      gpuCount++;
    }
  }
  const vramPct = vramTotal > 0 ? Math.round((vramUsed / vramTotal) * 100) : null;
  const gpuPct = gpuUsageCnt > 0 ? Math.round(gpuUsageSum / gpuUsageCnt) : null;

  return {
    nodeCount: n,
    cpuPct,
    cpuSub: coresTotal > 0 ? `${coresTotal} ${I18n.t('clusters.cores_short')}` : '',
    ramPct,
    ramUsedLabel: ramTotal > 0 ? formatMb(ramUsed) : '',
    ramSub: ramTotal > 0 ? `/ ${formatMb(ramTotal)}` : '',
    vramPct,
    vramUsedLabel: vramTotal > 0 ? formatMb(vramUsed) : '',
    vramSub: vramTotal > 0 ? `/ ${formatMb(vramTotal)}` : (gpuCount === 0 ? I18n.t('clusters.no_gpu') : ''),
    gpuPct,
    gpuSub: gpuCount > 0 ? `${gpuCount}× GPU` : I18n.t('clusters.no_gpu'),
  };
}

// ---- Rings ----------------------------------------------------------------

function renderRing(label, val, unit, sub, pct) {
  const safePct = pct == null ? 0 : Math.max(0, Math.min(100, pct));
  const hot = pct != null && pct > 85 ? ' hot' : (pct != null && pct > 60 ? ' warm' : '');
  const dim = pct == null ? ' dim' : '';
  return `
    <div class="gauge">
      <div class="gauge-ring${hot}${dim}" style="--pct: ${safePct};">
        <div class="gauge-val">${escapeHtml(val)}${unit ? `<span>${escapeHtml(unit)}</span>` : ''}</div>
      </div>
      <div class="gauge-label">${label}</div>
      <div class="gauge-sub">${escapeHtml(sub || '')}</div>
    </div>
  `;
}

// ---- Node / connection chips ---------------------------------------------

function renderNodeChip(member) {
  const online = isOnline(member.live);
  const status = online ? 'online' : 'offline';
  return `<tf-chip status="${status}" dot>${escapeHtml(member.hostname)}</tf-chip>`;
}

// Mapuj interface_type / speed -> klasa chipa polaczenia.
function connectionClass(type, speedMbps) {
  const t = String(type || '').toLowerCase();
  if (t === 'rdma' || t === 'infiniband') return 'rdma';
  if (t === 'roce') return 'roce';
  if (t === 'thunderbolt') return 'tb';
  if (t === 'wifi' || t === 'wlan') return 'wifi';
  if (speedMbps >= 10000) return 'eth10';
  if (speedMbps > 0 && speedMbps < 10000) return 'eth1';
  return 'eth1';
}

function connectionLabel(type, speedMbps) {
  const t = String(type || '').toLowerCase();
  if (t === 'rdma' || t === 'infiniband') return `RDMA ${formatSpeed(speedMbps)}`;
  if (t === 'roce') return `RoCE ${formatSpeed(speedMbps)}`;
  if (t === 'thunderbolt') return `Thunderbolt ${formatSpeed(speedMbps)}`;
  if (t === 'wifi' || t === 'wlan') return `Wi-Fi ${formatSpeed(speedMbps)}`;
  if (speedMbps > 0) return `Ethernet ${formatSpeed(speedMbps)}`;
  return 'Ethernet';
}

function formatSpeed(mbps) {
  if (!mbps) return '';
  if (mbps >= 1000) return `${(mbps / 1000).toFixed(0)}G`;
  return `${mbps}M`;
}

function renderConnectionChips(members) {
  // Uzywamy unikalnych typow polaczen po kazdym czlonku.
  const seen = new Set();
  const chips = [];
  for (const m of members) {
    if (!m.interface_type && !m.interface_speed_mbps) continue;
    const key = `${m.interface_type}|${Math.floor((m.interface_speed_mbps || 0) / 1000)}`;
    if (seen.has(key)) continue;
    seen.add(key);
    const cls = connectionClass(m.interface_type, m.interface_speed_mbps);
    const lbl = connectionLabel(m.interface_type, m.interface_speed_mbps);
    chips.push(`<span class="link-chip ${cls}">${escapeHtml(lbl)}</span>`);
  }
  if (chips.length === 0) return '';
  return `<div class="cluster-link-chips">${chips.join('')}</div>`;
}

// ---- Footer ---------------------------------------------------------------

function renderFooterMeta(cluster, agg) {
  const reqMin = cluster.requests_per_min != null ? `${cluster.requests_per_min} req/min` : '';
  const models = cluster.shared_models_count != null
    ? `${cluster.shared_models_count} ${I18n.t(cluster.shared_models_count === 1 ? 'clusters.model_one' : 'clusters.model_many')}`
    : '';
  const failover = cluster.failover_target_name ? `→ ${cluster.failover_target_name}` : '';
  const nodesLabel = `${agg.nodeCount} ${I18n.t(agg.nodeCount === 1 ? 'clusters.count_one_node' : 'clusters.count_many_nodes')}`;

  const parts = [nodesLabel];
  if (reqMin) parts.push(reqMin);
  if (models) parts.push(models);
  if (failover) parts.push(`Failover ${failover}`);

  return `<div class="cluster-card-footer">${parts.map(p => `<span>${escapeHtml(p)}</span>`).join('')}</div>`;
}

// ---- Click handlers -------------------------------------------------------

function handleListClick(e) {
  const editBtn = e.target.closest('[data-edit-cluster]');
  if (editBtn) {
    e.stopPropagation();
    const id = editBtn.dataset.editCluster;
    const cluster = clusters.find(c => String(c.id || c.cluster_id) === id);
    if (cluster) openWizard(cluster);
    return;
  }

  const testBtn = e.target.closest('[data-test-cluster]');
  if (testBtn) {
    e.stopPropagation();
    const id = testBtn.dataset.testCluster;
    ClusterDetailScreen.show(id);
    return;
  }

  const delBtn = e.target.closest('[data-delete-cluster]');
  if (delBtn) {
    e.stopPropagation();
    const id = delBtn.dataset.deleteCluster;
    const cluster = clusters.find(c => String(c.id || c.cluster_id) === id);
    if (cluster) confirmDelete(cluster);
    return;
  }

  const card = e.target.closest('[data-cluster-detail]');
  if (card) {
    const id = card.dataset.clusterDetail;
    if (id) ClusterDetailScreen.show(id);
  }
}

async function confirmDelete(cluster) {
  const { TfWindow } = await import('/js/components/tf-window.js');
  const name = cluster.name || cluster.id || cluster.cluster_id;
  const ok = await TfWindow.confirm({
    title: I18n.t('clusters.delete_title'),
    message: I18n.t('clusters.delete_confirm').replace('{name}', name),
    confirmLabel: I18n.t('common.delete'),
    cancelLabel: I18n.t('common.cancel'),
    danger: true,
  });
  if (!ok) return;

  try {
    const id = cluster.id || cluster.cluster_id;
    await ApiBinary.action('clusterDeleteRequest', { clusterId: id });
    toast(I18n.t('clusters.delete_success').replace('{name}', name), 'success');
    await loadAll();
    renderList();
  } catch (err) {
    toast(err.message || I18n.t('common.error'), 'error');
  }
}

function openWizard(cluster) {
  ClusterWizard.open({
    cluster,
    onDone: async () => {
      await loadAll();
      renderList();
    },
  });
}

export default ClustersScreen;

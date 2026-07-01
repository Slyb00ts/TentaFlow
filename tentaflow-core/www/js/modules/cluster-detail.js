// =============================================================================
// Plik: modules/cluster-detail.js
// Opis: Drill-down widok pojedynczego klastra — topbar, per-node gauges,
//       diagram SVG polaczen, matryca testow (live SSE probe), sekcja
//       load balancing / failover, shared models. Auto-refresh 5s z guard.
// =============================================================================

import {
  byId,
  escapeHtml,
  escapeAttr,
  toast,
  formatMb,
} from '/js/utils.js';
import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { patchInner } from '/js/lib/patch.js';
import { isOnline as isOnlineHelper } from '/js/modules/mesh-helpers.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-select.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-input.js';
import '/js/components/tf-spinner.js';

let currentClusterId = null;
let clusterData = null;
let nodesById = new Map();
let unifiedModels = [];
let refreshInterval = null;
let probeUnsub = null;
let probeResults = [];
let probeInProgress = false;
let probeAssignments = [];
let probeBottleneckMbps = null;
let probeAssignmentStatus = null;
let rdmaInProgress = false;
let rdmaResult = null;
let deployInProgress = false;
let deployResult = null;
let activeDeployment = null;

const ClusterDetailScreen = {
  title: 'Cluster',
  async show(clusterId) {
    if (!clusterId) return;
    currentClusterId = clusterId;
    clusterData = null;
    probeResults = [];
    probeInProgress = false;
    probeAssignments = [];
    probeBottleneckMbps = null;
    probeAssignmentStatus = null;
    deployInProgress = false;
    deployResult = null;
    activeDeployment = null;

    const content = document.getElementById('main');
    if (!content) return;
    content.innerHTML = renderSkeleton();
    bindBack(content);

    await loadAll();
    renderDetail();

    setupRefresh();
  },
  cleanup() {
    if (refreshInterval) {
      clearInterval(refreshInterval);
      refreshInterval = null;
    }
    if (probeUnsub) {
      try { probeUnsub(); } catch (_) {}
      probeUnsub = null;
    }
    currentClusterId = null;
    clusterData = null;
    probeResults = [];
    probeInProgress = false;
    probeAssignments = [];
    probeBottleneckMbps = null;
    probeAssignmentStatus = null;
    deployInProgress = false;
    deployResult = null;
    activeDeployment = null;
  },
};

// ---- Data ----------------------------------------------------------------

async function loadAll() {
  if (!currentClusterId) return;
  try {
    const [detailBody, nodes, unified] = await Promise.all([
      ApiBinary.one('clusterDetailRequest', { clusterId: currentClusterId }).catch(() => null),
      ApiBinary.list('meshNodeListRequest', { arrayKey: 'nodes' }).catch(() => []),
      ApiBinary.list('catalogListRequest', { arrayKey: 'entries' }).catch(() => []),
    ]);
    if (detailBody && detailBody.cluster) {
      // Skleic ClusterInfo + members[] z osobnych pol odpowiedzi w jeden obiekt
      // pasujacy do reszty kodu (resolveMembers oczekuje cluster.members).
      clusterData = { ...detailBody.cluster, members: detailBody.members || [] };
    } else {
      clusterData = null;
    }
    const nodesArr = Array.isArray(nodes) ? nodes : [];
    nodesById = new Map(nodesArr.map(n => [n.node_id || n.id, n]));
    unifiedModels = Array.isArray(unified) ? unified : [];
  } catch (err) {
    // zostaw stary stan, rerender pokaze ostatnie dane
  }
}

function setupRefresh() {
  if (refreshInterval) clearInterval(refreshInterval);
  refreshInterval = setInterval(async () => {
    if (!currentClusterId || !document.querySelector('.cluster-detail')) {
      ClusterDetailScreen.cleanup();
      return;
    }
    await loadAll();
    if (currentClusterId && document.querySelector('.cluster-detail')) {
      renderDetail();
    }
  }, 5000);
}

// ---- Render --------------------------------------------------------------

function renderSkeleton() {
  return `
    <div class="cluster-detail">
      <div class="cluster-detail-topbar">
        <tf-button variant="ghost" size="sm" id="btn-back-clusters">← ${escapeHtml(I18n.t('cluster_detail.back'))}</tf-button>
        <div class="cluster-detail-title"><span class="skeleton" style="display:inline-block;width:240px;height:24px;"></span></div>
      </div>
      <div class="cluster-detail-grid">
        <div class="cluster-detail-card"><div class="skeleton" style="width:100%;height:140px;"></div></div>
        <div class="cluster-detail-card"><div class="skeleton" style="width:100%;height:140px;"></div></div>
      </div>
    </div>
  `;
}

function renderDetail() {
  const content = document.getElementById('main');
  if (!content) return;

  if (!clusterData) {
    content.innerHTML = `
      <div class="cluster-detail">
        <div class="cluster-detail-topbar">
          <tf-button variant="ghost" size="sm" id="btn-back-clusters">← ${escapeHtml(I18n.t('cluster_detail.back'))}</tf-button>
        </div>
        <div class="empty-state"><div class="empty-state-text">${escapeHtml(I18n.t('cluster_detail.load_error'))}</div></div>
      </div>
    `;
    bindBack(content);
    return;
  }

  const c = clusterData;
  const members = resolveMembers(c);
  const status = clusterStatus(members);
  const statusChip = renderStatusChip(status);

  // Wykrywamy REALNA strukture (#cd-body), nie sam wrapper .cluster-detail —
  // skeleton tez ma .cluster-detail, wiec porownanie po klasie pomijaloby budowe
  // wlasciwego layoutu i strona zostawala pusta.
  const hasDetail = content.querySelector('#cd-body');
  if (!hasDetail) {
    content.innerHTML = `
      <div class="cluster-detail">
        <div class="cluster-detail-topbar">
          <tf-button variant="ghost" size="sm" id="btn-back-clusters">← ${escapeHtml(I18n.t('cluster_detail.back'))}</tf-button>
          <div class="cluster-detail-title">
            <div class="name" id="cd-name"></div>
            <div id="cd-status"></div>
          </div>
          <div class="cluster-detail-actions" id="cd-actions"></div>
        </div>
        <div id="cd-body"></div>
      </div>
    `;
    bindBack(content);
    bindBodyClicks(content);
  }

  const nameEl = byId('cd-name');
  if (nameEl) nameEl.textContent = c.name || c.id || c.cluster_id || '—';
  const statusEl = byId('cd-status');
  if (statusEl) statusEl.innerHTML = statusChip;

  const actionsEl = byId('cd-actions');
  if (actionsEl) {
    actionsEl.innerHTML = `
      <tf-button variant="secondary" size="sm" icon="edit" id="btn-edit-cluster">${escapeHtml(I18n.t('common.edit'))}</tf-button>
      <tf-button variant="secondary" size="sm" icon="share" id="btn-run-tests" ${probeInProgress ? 'disabled' : ''}>${escapeHtml(probeInProgress ? I18n.t('cluster_detail.testing') : I18n.t('cluster_detail.run_tests'))}</tf-button>
      <tf-button variant="secondary" size="sm" icon="bolt" id="btn-config-rdma" ${rdmaInProgress ? 'disabled' : ''}>${escapeHtml(rdmaInProgress ? I18n.t('cluster_detail.rdma_configuring') : I18n.t('cluster_detail.config_rdma'))}</tf-button>
      <tf-button variant="primary" size="sm" icon="rocket" id="btn-deploy-model" ${deployInProgress ? 'disabled' : ''}>${escapeHtml(deployInProgress ? I18n.t('cluster_detail.deploying') : I18n.t('cluster_detail.deploy_model'))}</tf-button>
      <tf-button variant="danger" size="sm" icon="trash" id="btn-delete-cluster">${escapeHtml(I18n.t('common.delete'))}</tf-button>
    `;
  }

  const body = byId('cd-body');
  if (body) {
    patchInner(body, `
      <div class="cluster-detail-grid">
        <div class="cluster-detail-col-nodes">${renderNodesColumn(members)}</div>
        <div class="cluster-detail-col-diagram">${renderDiagram(members)}</div>
        <div class="cluster-detail-col-summary">${renderSummaryColumn(c, members)}</div>
      </div>
      ${renderConnectionMatrix(members)}
      ${renderProbeAssignments(members)}
      ${renderRdmaConfig(members)}
      ${renderDeploySection(members)}
      ${renderRouting(c)}
      ${renderSharedModels(members)}
    `);
  }
}

function bindBack(root) {
  root.addEventListener('click', async (e) => {
    const back = e.target.closest('#btn-back-clusters');
    if (back) {
      ClusterDetailScreen.cleanup();
      const { Router } = await import('/js/router.js');
      Router.navigate('clusters');
    }
  });
}

function bindBodyClicks(root) {
  root.addEventListener('click', async (e) => {
    const editBtn = e.target.closest('#btn-edit-cluster');
    if (editBtn) {
      const { default: ClusterWizard } = await import('/js/modules/cluster-wizard.js');
      ClusterWizard.open({
        cluster: clusterData,
        onDone: async () => { await loadAll(); renderDetail(); },
      });
      return;
    }

    const delBtn = e.target.closest('#btn-delete-cluster');
    if (delBtn) {
      const { TfWindow } = await import('/js/components/tf-window.js');
      const name = clusterData?.name || currentClusterId;
      const ok = await TfWindow.confirm({
        title: I18n.t('clusters.delete_title'),
        message: I18n.t('clusters.delete_confirm').replace('{name}', name),
        confirmLabel: I18n.t('common.delete'),
        cancelLabel: I18n.t('common.cancel'),
        danger: true,
      });
      if (!ok) return;
      try {
        await ApiBinary.action('clusterDeleteRequest', { clusterId: currentClusterId });
        toast(I18n.t('clusters.delete_success').replace('{name}', name), 'success');
        ClusterDetailScreen.cleanup();
        const { Router } = await import('/js/router.js');
        Router.navigate('clusters');
      } catch (err) {
        toast(err.message || I18n.t('common.error'), 'error');
      }
      return;
    }

    const testBtn = e.target.closest('#btn-run-tests');
    if (testBtn && !probeInProgress) {
      await startClusterProbe();
      return;
    }

    const rdmaBtn = e.target.closest('#btn-config-rdma');
    if (rdmaBtn && !rdmaInProgress) {
      await configureClusterRdma();
      return;
    }

    const deployBtn = e.target.closest('#btn-deploy-model');
    if (deployBtn && !deployInProgress) {
      await openDeployModal();
      return;
    }

    const stopBtn = e.target.closest('#btn-deploy-stop');
    if (stopBtn) {
      await stopClusterDeploy();
      return;
    }

    const saveRouting = e.target.closest('#btn-save-routing');
    if (saveRouting) {
      await saveRoutingSettings();
      return;
    }
  });
}

// ---- Nodes column --------------------------------------------------------

function renderNodesColumn(members) {
  if (members.length === 0) {
    return `<div class="empty-state-small">${escapeHtml(I18n.t('clusters.no_members'))}</div>`;
  }
  return members.map(m => renderNodeMini(m)).join('');
}

function renderNodeMini(member) {
  const live = member.live;
  const online = memberOnline(member);
  const status = online ? 'online' : 'offline';

  const cpuPct = live ? pctOr(live.cpu_usage ?? live.cpu_usage_percent) : null;
  const ram = live && live.ram_total_mb
    ? Math.round(((live.ram_used_mb || 0) / live.ram_total_mb) * 100)
    : null;
  const gpus = live && Array.isArray(live.gpus) ? live.gpus : [];
  const vramUsed = gpus.reduce((s, g) => s + (g.vram_used_mb || 0), 0);
  const vramTotal = gpus.reduce((s, g) => s + (g.vram_total_mb || 0), 0);
  const vramPct = vramTotal > 0 ? Math.round((vramUsed / vramTotal) * 100) : null;

  const asg = findAssignment(member.node_id);
  // Po probe znamy wybrany interfejs — pokazujemy go zamiast surowych metadanych.
  const linkType = asg ? asg.interface_type : member.interface_type;
  const linkSpeed = asg ? asg.interface_speed_mbps : member.interface_speed_mbps;
  const linkClass = connectionClass(linkType, linkSpeed);
  const linkLabel = connectionLabel(linkType, linkSpeed);
  const asgDetail = asg
    ? `${asg.interface_name || ''}${asg.interface_ip ? ` ${asg.interface_ip}` : ''}`.trim()
    : '';

  return `
    <div class="cluster-detail-node ${online ? '' : 'offline'}">
      <div class="cdn-head">
        <div class="cdn-ico">${escapeHtml((member.hostname || '?').slice(0, 1).toUpperCase())}</div>
        <div class="cdn-titlebox">
          <div class="cdn-name">${escapeHtml(member.hostname)} <tf-chip status="${status}" dot>${escapeHtml(I18n.t(online ? 'mesh.online' : 'mesh.offline'))}</tf-chip></div>
          <div class="cdn-role">${escapeHtml(member.role)}</div>
        </div>
      </div>
      <div class="cdn-bars">
        ${renderMiniBar('CPU', cpuPct)}
        ${renderMiniBar('RAM', ram)}
        ${renderMiniBar('VRAM', vramPct)}
      </div>
      ${linkLabel ? `<div class="cdn-link"><span class="link-chip ${linkClass}">${asg ? '✓ ' : ''}${escapeHtml(linkLabel)}${asgDetail ? ` · ${escapeHtml(asgDetail)}` : ''}</span></div>` : ''}
    </div>
  `;
}

function renderMiniBar(label, pct) {
  const p = pct == null ? 0 : Math.max(0, Math.min(100, pct));
  const cls = pct == null ? 'dim' : (pct > 85 ? 'hot' : (pct > 60 ? 'warm' : ''));
  return `
    <div class="cdn-bar-row">
      <span class="cdn-bar-lbl">${label}</span>
      <div class="cdn-bar"><div class="cdn-bar-fill ${cls}" style="width:${p}%"></div></div>
      <span class="cdn-bar-val">${pct == null ? '—' : `${pct}%`}</span>
    </div>
  `;
}

// ---- Diagram -------------------------------------------------------------

function renderDiagram(members) {
  const n = members.length;
  if (n === 0) {
    return `<div class="cluster-diagram empty">${escapeHtml(I18n.t('clusters.no_members'))}</div>`;
  }

  const w = 320, h = 320;
  const cx = w / 2, cy = h / 2;
  const r = Math.min(w, h) / 2 - 48;
  const points = members.map((m, i) => {
    const a = (i / n) * Math.PI * 2 - Math.PI / 2;
    return { x: cx + r * Math.cos(a), y: cy + r * Math.sin(a), member: m };
  });

  // Linie miedzy kazda para
  const lines = [];
  for (let i = 0; i < n; i++) {
    for (let j = i + 1; j < n; j++) {
      const a = points[i], b = points[j];
      // Wyznacz typ polaczenia na podstawie probe / metadanych
      const res = findProbeBetween(members[i].node_id, members[j].node_id);
      const { cls, strokeWidth, label } = resolveLineStyle(res, members[i], members[j]);
      lines.push(`
        <line x1="${a.x}" y1="${a.y}" x2="${b.x}" y2="${b.y}" class="cd-link-line ${cls}" stroke-width="${strokeWidth}"/>
        ${label ? `<text x="${(a.x + b.x) / 2}" y="${(a.y + b.y) / 2 - 4}" class="cd-link-label">${escapeHtml(label)}</text>` : ''}
      `);
    }
  }

  const dots = points.map((p, i) => {
    const online = memberOnline(p.member);
    return `
      <g class="cd-node ${online ? '' : 'offline'}" transform="translate(${p.x}, ${p.y})">
        <circle r="18" class="cd-node-circle"/>
        <text y="5" text-anchor="middle" class="cd-node-label">${escapeHtml((p.member.hostname || '?').slice(0, 2).toUpperCase())}</text>
        <text y="38" text-anchor="middle" class="cd-node-host">${escapeHtml((p.member.hostname || '').slice(0, 12))}</text>
      </g>
    `;
  }).join('');

  return `
    <div class="cluster-diagram">
      <div class="cluster-diagram-title">${escapeHtml(I18n.t('cluster_detail.topology'))}</div>
      <svg viewBox="0 0 ${w} ${h}" xmlns="http://www.w3.org/2000/svg">
        <g class="cd-links">${lines.join('')}</g>
        <g class="cd-nodes">${dots}</g>
      </svg>
    </div>
  `;
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

function resolveLineStyle(probe, memberA, memberB) {
  // Probe ma priorytet. W braku - uzyj interface_type/speed z czlonkow.
  if (probe) {
    if (!probe.reachable) {
      return { cls: 'offline', strokeWidth: 1, label: '' };
    }
    const bw = probe.bandwidth_mbps || 0;
    const lat = probe.latency_us || 0;
    const cls = probe.rdma ? 'rdma' : (bw > 40000 ? 'rdma' : (bw > 10000 ? 'eth10' : (bw > 0 ? 'eth1' : 'offline')));
    const label = bw >= 1000 ? `${(bw / 1000).toFixed(1)}G${lat > 0 ? ` · ${Math.round(lat / 1000)}ms` : ''}` : (bw > 0 ? `${bw}M` : '');
    return { cls, strokeWidth: bw > 40000 ? 3 : (bw > 10000 ? 2 : 1.5), label };
  }
  const speed = Math.min(memberA.interface_speed_mbps || 0, memberB.interface_speed_mbps || 0);
  const type = memberA.interface_type || memberB.interface_type;
  const cls = connectionClass(type, speed);
  return { cls, strokeWidth: speed >= 40000 ? 3 : (speed >= 10000 ? 2 : 1.5), label: '' };
}

// ---- Summary column ------------------------------------------------------

function renderSummaryColumn(cluster, members) {
  const n = members.length;
  const onlineCnt = members.filter(m => memberOnline(m)).length;
  const totalCpu = members.reduce((s, m) => s + (m.live?.cpu_count || 0), 0);
  const totalRam = members.reduce((s, m) => s + (m.live?.ram_total_mb || 0), 0);
  const totalVram = members.reduce((s, m) => {
    const g = Array.isArray(m.live?.gpus) ? m.live.gpus : [];
    return s + g.reduce((x, gg) => x + (gg.vram_total_mb || 0), 0);
  }, 0);

  const rows = [
    [I18n.t('cluster_detail.total_nodes'), `${onlineCnt} / ${n}`],
    [I18n.t('cluster_detail.total_cpu'), `${totalCpu} ${I18n.t('clusters.cores_short')}`],
    [I18n.t('cluster_detail.total_ram'), totalRam > 0 ? formatMb(totalRam) : '—'],
    [I18n.t('cluster_detail.total_vram'), totalVram > 0 ? formatMb(totalVram) : '—'],
    [I18n.t('cluster_detail.strategy'), translateStrategy(cluster.strategy)],
  ];

  return `
    <div class="cluster-summary-card">
      <div class="cluster-summary-title">${escapeHtml(I18n.t('cluster_detail.summary'))}</div>
      ${rows.map(([k, v]) => `<div class="cluster-summary-row"><span class="k">${escapeHtml(k)}</span><span class="v">${escapeHtml(v)}</span></div>`).join('')}
      ${cluster.description ? `<div class="cluster-summary-desc">${escapeHtml(cluster.description)}</div>` : ''}
    </div>
  `;
}

function translateStrategy(s) {
  const k = String(s || 'distributed').toLowerCase();
  if (k === 'distributed') return I18n.t('clusters.strategy_distributed');
  if (k === 'replicated') return I18n.t('clusters.strategy_replicated');
  if (k === 'primary_replica') return I18n.t('clusters.strategy_primary_replica');
  return s;
}

// ---- Connection matrix (live via SSE) -----------------------------------

function renderConnectionMatrix(members) {
  if (members.length < 2) return '';

  const rows = members.map((rowM, i) => {
    const cells = members.map((colM, j) => {
      if (i === j) return '<td class="cell-self">—</td>';
      const res = findProbeBetween(rowM.node_id, colM.node_id);
      if (!res) {
        return `<td class="cell-pending">${probeInProgress ? escapeHtml(I18n.t('clusters.probing')) : '—'}</td>`;
      }
      if (!res.reachable) {
        return `<td class="cell-fail"><tf-chip status="error">✗</tf-chip></td>`;
      }
      const bw = res.bandwidth_mbps || 0;
      const lat = res.latency_us || 0;
      const bwLabel = bw >= 1000 ? `${(bw / 1000).toFixed(1)} Gbps` : `${bw.toFixed(0)} Mbps`;
      const latLabel = lat > 0 ? (lat >= 1000 ? `${(lat / 1000).toFixed(1)} ms` : `${lat} µs`) : '';
      const cls = bw > 40000 ? 'ok' : (bw > 5000 ? 'warn' : 'slow');
      return `<td class="cell-result ${cls}"><div>${escapeHtml(bwLabel)}</div>${latLabel ? `<div class="lat">${escapeHtml(latLabel)}</div>` : ''}</td>`;
    }).join('');
    return `<tr><th>${escapeHtml(rowM.hostname)}</th>${cells}</tr>`;
  }).join('');

  const headers = members.map(m => `<th>${escapeHtml(m.hostname)}</th>`).join('');

  return `
    <div class="cluster-matrix-section">
      <div class="cluster-matrix-title">${escapeHtml(I18n.t('cluster_detail.connection_matrix'))}</div>
      <table class="cluster-matrix">
        <thead><tr><th></th>${headers}</tr></thead>
        <tbody>${rows}</tbody>
      </table>
    </div>
  `;
}

// Sekcja "wybrane interfejsy" — pokazuje ktory NIC wygral per node + bottleneck
// klastra. Renderuje sie dopiero po otrzymaniu danych End z probe.
function renderProbeAssignments(members) {
  if (probeAssignments.length === 0 && probeBottleneckMbps == null) return '';

  const byNode = new Map(probeAssignments.map(a => [a.node_id, a]));
  const rows = members.map(m => {
    const a = byNode.get(m.node_id);
    if (!a) {
      return `<div class="cluster-summary-row"><span class="k">${escapeHtml(m.hostname)}</span><span class="v">—</span></div>`;
    }
    const cls = connectionClass(a.interface_type, a.interface_speed_mbps);
    const lbl = connectionLabel(a.interface_type, a.interface_speed_mbps) || a.interface_type || '—';
    const detail = `${a.interface_name || ''}${a.interface_ip ? ` (${a.interface_ip})` : ''}`.trim();
    return `
      <div class="cluster-summary-row">
        <span class="k">${escapeHtml(m.hostname)}</span>
        <span class="v"><span class="link-chip ${cls}">${escapeHtml(lbl)}</span>${detail ? ` <code>${escapeHtml(detail)}</code>` : ''}</span>
      </div>
    `;
  }).join('');

  const bottleneck = probeBottleneckMbps != null
    ? `<div class="cluster-summary-row"><span class="k">${escapeHtml(I18n.t('clusters.bottleneck'))}</span><span class="v">${escapeHtml(formatBandwidth(probeBottleneckMbps))}</span></div>`
    : '';

  return `
    <div class="cluster-matrix-section">
      <div class="cluster-matrix-title">${escapeHtml(I18n.t('cluster_detail.selected_interfaces'))}</div>
      <div class="cluster-summary-card">
        ${rows}
        ${bottleneck}
      </div>
    </div>
  `;
}

async function startClusterProbe() {
  if (!clusterData) return;
  const members = resolveMembers(clusterData);
  if (members.length < 2) {
    toast(I18n.t('clusters.select_min_nodes'), 'warning');
    return;
  }

  probeInProgress = true;
  probeResults = [];
  probeAssignments = [];
  probeBottleneckMbps = null;
  probeAssignmentStatus = null;
  renderDetail();

  const nodeIds = members.map(m => m.node_id);

  try {
    probeUnsub = await ApiBinary.subscribe(
      'clusterProbeStreamRequest',
      { nodeIds },
      {
        onChunk: (chunk) => {
          if (chunk.eventType === 'result' && chunk.sourceNode && chunk.targetNode) {
            // Mapuj na lokalny ksztalt probe wynikow uzywany przez findProbeBetween.
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
            renderDetail();
          }
        },
        onEnd: (end) => {
          probeInProgress = false;
          probeUnsub = null;
          // Pola End sa NOWE — starsze serwery (lub niezaktualizowany dekoder)
          // ich nie wysylaja, wiec czytamy defensywnie i pomijamy gdy brak.
          if (end && typeof end === 'object') {
            const bn = end.bottleneckMbps ?? end.bottleneck_mbps;
            probeBottleneckMbps = (bn != null) ? bn : null;
            probeAssignmentStatus = end.assignmentStatus ?? end.assignment_status ?? null;
            const asg = end.assignments;
            probeAssignments = Array.isArray(asg) ? asg.map(a => ({
              node_id: a.nodeId ?? a.node_id ?? '',
              interface_name: a.interfaceName ?? a.interface_name ?? '',
              interface_ip: a.interfaceIp ?? a.interface_ip ?? '',
              interface_speed_mbps: a.interfaceSpeedMbps ?? a.interface_speed_mbps ?? 0,
              interface_type: a.interfaceType ?? a.interface_type ?? '',
            })) : [];
          }
          renderDetail();
          toast(I18n.t('cluster_detail.tests_done'), 'success');
        },
        onError: (err) => {
          probeInProgress = false;
          probeUnsub = null;
          toast(`${I18n.t('common.error')}: ${err.message ?? 'probe error'}`, 'error');
          renderDetail();
        },
      },
    );
  } catch (err) {
    probeInProgress = false;
    toast(err.message || I18n.t('common.error'), 'error');
    renderDetail();
  }
}

// ---- RDMA auto-config ----------------------------------------------------

// Prompts for the sudo password (needed by the per-node NetworkConfig mesh
// command) and triggers cluster RDMA auto-config: detect each member's RoCE
// "twins", bring up the unconfigured one (IP on a dedicated subnet + MTU 9000),
// and persist the RoCE device list for distributed deploy. Everything runs in
// the program — no manual ip/netplan.
async function configureClusterRdma() {
  if (!clusterData) return;
  const members = resolveMembers(clusterData);
  if (members.length < 2) {
    toast(I18n.t('clusters.select_min_nodes'), 'warning');
    return;
  }

  const sudoPassword = await promptSudoPassword();
  if (sudoPassword == null) return; // cancelled

  rdmaInProgress = true;
  rdmaResult = null;
  renderDetail();

  try {
    const resp = await ApiBinary.action(
      'clusterRdmaConfigureRequest',
      { clusterId: currentClusterId, sudoPassword },
      { timeoutMs: 120000 },
    );
    rdmaResult = resp;
    rdmaInProgress = false;
    renderDetail();
    if (resp.ok) {
      toast(I18n.t('cluster_detail.rdma_done'), 'success');
    } else {
      toast(I18n.t('cluster_detail.rdma_partial'), 'warning');
    }
  } catch (err) {
    rdmaInProgress = false;
    toast(err.message || I18n.t('common.error'), 'error');
    renderDetail();
  }
}

// Modal sudo prompt built from tf-* primitives. Resolves to the entered string
// on confirm, or null on cancel.
async function promptSudoPassword() {
  const { TfWindow } = await import('/js/components/tf-window.js');
  await import('/js/components/tf-input.js');

  const body = document.createElement('div');
  body.innerHTML = `
    <div style="display:flex;flex-direction:column;gap:8px;min-width:320px;">
      <div>${escapeHtml(I18n.t('cluster_detail.rdma_sudo_hint'))}</div>
      <tf-input id="rdma-sudo-input" type="password" autofocus
        placeholder="${escapeAttr(I18n.t('cluster_detail.rdma_sudo_placeholder'))}"></tf-input>
    </div>
  `;

  const result = await TfWindow.open({
    title: I18n.t('cluster_detail.config_rdma'),
    icon: 'bolt',
    modal: true,
    width: 420,
    body,
    footer: `
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" data-action="confirm">${escapeHtml(I18n.t('cluster_detail.config_rdma'))}</tf-button>
    `,
  });

  if (result.action !== 'confirm') return null;
  const input = body.querySelector('#rdma-sudo-input');
  const value = input ? String(input.value || '') : '';
  return value.length > 0 ? value : null;
}

function renderRdmaConfig(members) {
  // Prefer the just-run action result; fall back to persisted member columns so
  // the section survives a page refresh.
  const fromMembers = members
    .map(m => ({
      hostname: m.hostname,
      rdmaDevices: m.rdmaDevices ?? m.rdma_devices ?? '',
      rdmaIp: m.rdmaIp ?? m.rdma_ip ?? '',
      rdmaSocketIfname: m.rdmaSocketIfname ?? m.rdma_socket_ifname ?? '',
    }))
    .filter(m => m.rdmaDevices || m.rdmaIp);

  const hasResult = rdmaResult && Array.isArray(rdmaResult.members) && rdmaResult.members.length > 0;
  if (!hasResult && fromMembers.length === 0) return '';

  let rows;
  if (hasResult) {
    rows = rdmaResult.members.map(m => {
      if (m.error) {
        return `<tr><th>${escapeHtml(m.hostname || m.nodeId)}</th>
          <td colspan="3"><tf-chip status="error">${escapeHtml(m.error)}</tf-chip></td></tr>`;
      }
      const ifaces = (m.interfaces || []).map(i => {
        const status = i.action === 'failed' ? 'error'
          : i.action === 'unchanged' ? 'neutral' : 'success';
        return `<div class="rdma-iface">
          <tf-chip status="${status}">${escapeHtml(i.role)}</tf-chip>
          <code>${escapeHtml(i.netdev)}</code>
          <span class="rdma-roce">${escapeHtml(i.roceDevice)}</span>
          <span class="rdma-ip">${escapeHtml(i.ipv4 || '—')}</span>
          <span class="rdma-mtu">MTU ${escapeHtml(String(i.mtu || ''))}</span>
          <span class="rdma-action">${escapeHtml(i.action)}</span>
        </div>`;
      }).join('');
      return `<tr><th>${escapeHtml(m.hostname || m.nodeId)}</th><td colspan="3">${ifaces}</td></tr>`;
    }).join('');
  } else {
    rows = fromMembers.map(m => `
      <tr>
        <th>${escapeHtml(m.hostname)}</th>
        <td><code>${escapeHtml(m.rdmaSocketIfname || '—')}</code></td>
        <td><span class="rdma-ip">${escapeHtml(m.rdmaIp || '—')}</span></td>
        <td><span class="rdma-roce">${escapeHtml(m.rdmaDevices || '—')}</span></td>
      </tr>
    `).join('');
  }

  return `
    <div class="cluster-rdma-section">
      <div class="cluster-rdma-title">${escapeHtml(I18n.t('cluster_detail.rdma_title'))}</div>
      <table class="cluster-rdma-table"><tbody>${rows}</tbody></table>
    </div>
  `;
}

// ---- Distributed deploy (vLLM tensor-parallel across the cluster) --------

// True when every member already has a RoCE IP from D1 (Configure RDMA). The
// distributed deploy needs the high-speed twin up, so we hint the user when it
// is not configured yet.
function clusterRdmaConfigured(members) {
  return members.length > 0 && members.every(m => String(m.rdma_ip || '').length > 0);
}

// Heuristic default engine: GB10 / Spark nodes ship the aarch64 image. We cannot
// read the arch from the member here, so we default to the Spark image and let
// the user switch to plain "vllm" for x86 clusters.
function defaultEngineId() {
  return 'vllm-spark';
}

function renderDeploySection(members) {
  const tpSize = members.length; // gpus_per_node default 1 → tp = members
  const rdmaOk = clusterRdmaConfigured(members);
  const hint = rdmaOk
    ? ''
    : `<div class="cluster-deploy-hint">${escapeHtml(I18n.t('cluster_detail.deploy_rdma_hint'))}</div>`;

  let statusBlock = '';
  if (deployInProgress) {
    statusBlock = `
      <div class="cluster-deploy-progress">
        <tf-spinner size="sm"></tf-spinner>
        <span>${escapeHtml(I18n.t('cluster_detail.deploy_in_progress'))}</span>
      </div>`;
  } else if (activeDeployment) {
    statusBlock = renderActiveDeployment(activeDeployment);
  } else if (deployResult) {
    statusBlock = renderDeployResult(deployResult);
  }

  return `
    <div class="cluster-deploy-section">
      <div class="cluster-matrix-title">${escapeHtml(I18n.t('cluster_detail.deploy_title'))}</div>
      <div class="cluster-deploy-meta">
        <span>${escapeHtml(I18n.t('cluster_detail.deploy_tp_size'))}: <strong>${tpSize}</strong></span>
        <span class="cluster-deploy-meta-dim">${escapeHtml(I18n.t('cluster_detail.deploy_tp_formula'))}</span>
      </div>
      ${hint}
      ${statusBlock}
    </div>
  `;
}

function renderActiveDeployment(dep) {
  const memberRows = (dep.members || []).map(m => renderDeployMemberRow(m)).join('');
  const endpoint = dep.endpointUrl
    ? `<div class="cluster-deploy-endpoint"><span class="k">${escapeHtml(I18n.t('cluster_detail.deploy_endpoint'))}</span><code>${escapeHtml(dep.endpointUrl)}</code></div>`
    : '';
  return `
    <div class="cluster-deploy-active">
      <div class="cluster-deploy-active-head">
        <tf-chip status="${dep.ok ? 'online' : 'warning'}" dot>${escapeHtml(dep.ok ? I18n.t('cluster_detail.deploy_active') : I18n.t('cluster_detail.deploy_degraded'))}</tf-chip>
        <span class="cluster-deploy-id"><code>${escapeHtml(dep.deploymentClusterId || '')}</code></span>
        <tf-button variant="danger" size="sm" icon="stop" id="btn-deploy-stop">${escapeHtml(I18n.t('cluster_detail.deploy_stop'))}</tf-button>
      </div>
      ${endpoint}
      <table class="cluster-rdma-table"><tbody>${memberRows}</tbody></table>
      ${dep.message ? `<div class="cluster-deploy-message">${escapeHtml(dep.message)}</div>` : ''}
    </div>
  `;
}

function renderDeployResult(res) {
  const memberRows = (res.members || []).map(m => renderDeployMemberRow(m)).join('');
  return `
    <div class="cluster-deploy-result">
      <div class="cluster-deploy-active-head">
        <tf-chip status="${res.ok ? 'online' : 'error'}" dot>${escapeHtml(res.ok ? I18n.t('cluster_detail.deploy_ok') : I18n.t('cluster_detail.deploy_failed'))}</tf-chip>
      </div>
      <table class="cluster-rdma-table"><tbody>${memberRows}</tbody></table>
      ${res.message ? `<div class="cluster-deploy-message">${escapeHtml(res.message)}</div>` : ''}
    </div>
  `;
}

function renderDeployMemberRow(m) {
  const detail = m.error
    ? `<tf-chip status="error">${escapeHtml(m.error)}</tf-chip>`
    : `<tf-chip status="success">${escapeHtml(I18n.t('cluster_detail.deploy_member_ok'))}</tf-chip>${m.deployId ? ` <code>${escapeHtml(m.deployId)}</code>` : ''}`;
  return `<tr>
    <th>${escapeHtml(m.hostname || m.nodeId)}</th>
    <td><tf-chip status="neutral">${escapeHtml(m.role)}</tf-chip></td>
    <td>${detail}</td>
  </tr>`;
}

// Modal collecting the deploy parameters; submits a ClusterDeployRequest. The
// tp_size preview updates live as the user changes gpus_per_node.
async function openDeployModal() {
  if (!clusterData) return;
  const members = resolveMembers(clusterData);
  if (members.length < 2) {
    toast(I18n.t('clusters.select_min_nodes'), 'warning');
    return;
  }

  const { TfWindow } = await import('/js/components/tf-window.js');
  await import('/js/components/tf-input.js');
  await import('/js/components/tf-select.js');

  const body = document.createElement('div');
  body.innerHTML = `
    <div class="cluster-deploy-form" style="display:flex;flex-direction:column;gap:12px;min-width:420px;">
      <div class="form-group">
        <label>${escapeHtml(I18n.t('cluster_detail.deploy_engine'))}</label>
        <tf-select id="dep-engine" value="${escapeAttr(defaultEngineId())}">
          <option value="vllm-spark"${defaultEngineId() === 'vllm-spark' ? ' selected' : ''}>vLLM (Spark / aarch64)</option>
          <option value="vllm">vLLM (x86 / CUDA)</option>
        </tf-select>
      </div>
      <div class="form-group">
        <label>${escapeHtml(I18n.t('cluster_detail.deploy_model_repo'))}</label>
        <tf-input id="dep-repo" placeholder="${escapeAttr(I18n.t('cluster_detail.deploy_model_repo_hint'))}"></tf-input>
      </div>
      <div class="form-group">
        <label>${escapeHtml(I18n.t('cluster_detail.deploy_served_name'))}</label>
        <tf-input id="dep-served" placeholder="${escapeAttr(I18n.t('cluster_detail.deploy_served_name_hint'))}"></tf-input>
      </div>
      <div class="form-group">
        <label>${escapeHtml(I18n.t('cluster_detail.deploy_gpus_per_node'))}</label>
        <tf-input id="dep-gpus" type="number" min="1" max="8" value="1"></tf-input>
        <div class="cluster-deploy-tp-preview">${escapeHtml(I18n.t('cluster_detail.deploy_tp_size'))}: <strong id="dep-tp">${members.length}</strong> <span class="cluster-deploy-meta-dim">(${members.length} × <span id="dep-gpus-echo">1</span>)</span></div>
      </div>
      <div class="form-group">
        <label>${escapeHtml(I18n.t('cluster_detail.deploy_gpu_mem'))}</label>
        <tf-input id="dep-gpumem" type="number" min="0.1" max="1.0" step="0.05" value="0.6"></tf-input>
      </div>
      <div class="form-group">
        <label>${escapeHtml(I18n.t('cluster_detail.deploy_max_len'))}</label>
        <tf-input id="dep-maxlen" type="number" min="512" step="512" value="8192"></tf-input>
      </div>
      <div class="form-group">
        <label>${escapeHtml(I18n.t('cluster_detail.deploy_port'))}</label>
        <tf-input id="dep-port" type="number" min="1" max="65535" value="8100"></tf-input>
      </div>
      <div class="form-group">
        <label>${escapeHtml(I18n.t('cluster_detail.deploy_pricing_title'))}</label>
        <div class="form-hint" style="margin-bottom:6px;">${escapeHtml(I18n.t('cluster_detail.deploy_pricing_hint'))}</div>
        <div style="display:grid;grid-template-columns:1fr 1fr;gap:8px;">
          <tf-input id="dep-price-prompt" type="number" min="0" step="0.0001" label="${escapeAttr(I18n.t('model_metrics.col_price_prompt'))}"></tf-input>
          <tf-input id="dep-price-completion" type="number" min="0" step="0.0001" label="${escapeAttr(I18n.t('model_metrics.col_price_completion'))}"></tf-input>
          <tf-input id="dep-price-audio" type="number" min="0" step="0.0001" label="${escapeAttr(I18n.t('model_metrics.col_price_audio'))}"></tf-input>
          <tf-input id="dep-price-image" type="number" min="0" step="0.0001" label="${escapeAttr(I18n.t('model_metrics.col_price_image'))}"></tf-input>
        </div>
      </div>
    </div>
  `;

  // Live tp_size preview.
  const memberCount = members.length;
  const gpusInput = body.querySelector('#dep-gpus');
  const tpEl = body.querySelector('#dep-tp');
  const echoEl = body.querySelector('#dep-gpus-echo');
  const updateTp = () => {
    const g = Math.max(1, parseInt(gpusInput?.value, 10) || 1);
    if (tpEl) tpEl.textContent = String(memberCount * g);
    if (echoEl) echoEl.textContent = String(g);
  };
  if (gpusInput) {
    gpusInput.addEventListener('input', updateTp);
    gpusInput.addEventListener('change', updateTp);
  }

  const result = await TfWindow.open({
    title: I18n.t('cluster_detail.deploy_title'),
    icon: 'rocket',
    modal: true,
    width: 520,
    body,
    footer: `
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" data-action="confirm">${escapeHtml(I18n.t('cluster_detail.deploy_submit'))}</tf-button>
    `,
  });

  if (result.action !== 'confirm') return;

  const repo = String(body.querySelector('#dep-repo')?.value || '').trim();
  if (!repo) {
    toast(I18n.t('cluster_detail.deploy_need_repo'), 'warning');
    return;
  }
  const served = String(body.querySelector('#dep-served')?.value || '').trim();
  const gpusPerNode = Math.max(1, parseInt(body.querySelector('#dep-gpus')?.value, 10) || 1);
  const gpuMem = parseFloat(body.querySelector('#dep-gpumem')?.value) || 0.6;
  const maxLen = parseInt(body.querySelector('#dep-maxlen')?.value, 10) || 8192;
  const port = parseInt(body.querySelector('#dep-port')?.value, 10) || 8100;
  const engineId = body.querySelector('#dep-engine')?.value || defaultEngineId();

  // Optional pricing captured at deploy time. Empty inputs stay null (backend
  // skips them); negative values are rejected client-side before submit.
  const priceOf = (sel) => {
    const raw = String(body.querySelector(sel)?.value ?? '').trim();
    if (raw === '') return null;
    const n = Number(raw);
    return Number.isFinite(n) && n >= 0 ? n : NaN;
  };
  const promptPer1k = priceOf('#dep-price-prompt');
  const completionPer1k = priceOf('#dep-price-completion');
  const audioPerMin = priceOf('#dep-price-audio');
  const imageEach = priceOf('#dep-price-image');
  if ([promptPer1k, completionPer1k, audioPerMin, imageEach].some((v) => Number.isNaN(v))) {
    toast(I18n.t('cluster_detail.deploy_pricing_invalid'), 'warning');
    return;
  }

  await runClusterDeploy({
    engineId,
    modelRepo: repo,
    servedModelName: served || null,
    gpusPerNode,
    gpuMemoryUtilization: gpuMem,
    maxModelLen: maxLen,
    port,
    promptPer1k,
    completionPer1k,
    audioPerMin,
    imageEach,
  });
}

async function runClusterDeploy(opts) {
  deployInProgress = true;
  deployResult = null;
  renderDetail();

  const readyTimeoutSecs = 600;
  try {
    const resp = await ApiBinary.action(
      'clusterDeployRequest',
      {
        clusterId: currentClusterId,
        engineId: opts.engineId,
        modelRepo: opts.modelRepo,
        servedModelName: opts.servedModelName,
        gpusPerNode: opts.gpusPerNode,
        gpuMemoryUtilization: opts.gpuMemoryUtilization,
        maxModelLen: opts.maxModelLen,
        port: opts.port,
        readyTimeoutSecs,
        promptPer1k: opts.promptPer1k ?? null,
        completionPer1k: opts.completionPer1k ?? null,
        audioPerMin: opts.audioPerMin ?? null,
        imageEach: opts.imageEach ?? null,
      },
      { timeoutMs: readyTimeoutSecs * 1000 + 30000 },
    );
    deployInProgress = false;
    deployResult = resp;
    if (resp.ok && resp.deploymentClusterId) {
      activeDeployment = resp;
      toast(I18n.t('cluster_detail.deploy_ok'), 'success');
    } else {
      activeDeployment = null;
      const msg = String(resp.message || '');
      // The backend rejects a deploy when RDMA isn't configured yet.
      if (/rdma/i.test(msg)) {
        toast(I18n.t('cluster_detail.deploy_rdma_required'), 'error');
      } else {
        toast(msg || I18n.t('cluster_detail.deploy_failed'), 'error');
      }
    }
    renderDetail();
  } catch (err) {
    deployInProgress = false;
    deployResult = null;
    toast(err.message || I18n.t('common.error'), 'error');
    renderDetail();
  }
}

async function stopClusterDeploy() {
  if (!activeDeployment || !activeDeployment.deploymentClusterId) return;
  const { TfWindow } = await import('/js/components/tf-window.js');
  const ok = await TfWindow.confirm({
    title: I18n.t('cluster_detail.deploy_stop_title'),
    message: I18n.t('cluster_detail.deploy_stop_confirm'),
    confirmLabel: I18n.t('cluster_detail.deploy_stop'),
    cancelLabel: I18n.t('common.cancel'),
    danger: true,
  });
  if (!ok) return;

  const deploymentClusterId = activeDeployment.deploymentClusterId;
  try {
    const resp = await ApiBinary.action(
      'clusterDeployStopRequest',
      { clusterId: currentClusterId, deploymentClusterId },
      { timeoutMs: 120000 },
    );
    activeDeployment = null;
    deployResult = resp;
    renderDetail();
    toast(resp.ok ? I18n.t('cluster_detail.deploy_stopped') : (resp.message || I18n.t('cluster_detail.deploy_stop_partial')), resp.ok ? 'success' : 'warning');
  } catch (err) {
    toast(err.message || I18n.t('common.error'), 'error');
  }
}

// ---- Routing (load balancing + failover) --------------------------------

function renderRouting(cluster) {
  const strategy = String(cluster.strategy || 'distributed');
  const failoverEnabled = !!(cluster.failoverEnabled ?? cluster.failover_enabled);
  const failoverTarget = cluster.failoverTarget || cluster.failover_target || '';

  return `
    <div class="cluster-routing-section">
      <div class="cluster-matrix-title">${escapeHtml(I18n.t('cluster_detail.routing'))}</div>
      <div class="cluster-routing-grid">
        <div class="form-group">
          <label>${escapeHtml(I18n.t('cluster_detail.lb_strategy'))}</label>
          <tf-select id="cd-strategy" value="${escapeAttr(strategy)}">
            <option value="distributed"${strategy === 'distributed' ? ' selected' : ''}>${escapeHtml(I18n.t('clusters.strategy_distributed'))}</option>
            <option value="replicated"${strategy === 'replicated' ? ' selected' : ''}>${escapeHtml(I18n.t('clusters.strategy_replicated'))}</option>
            <option value="primary_replica"${strategy === 'primary_replica' ? ' selected' : ''}>${escapeHtml(I18n.t('clusters.strategy_primary_replica'))}</option>
          </tf-select>
        </div>
        <div class="form-group">
          <label>${escapeHtml(I18n.t('cluster_detail.failover_enabled'))}</label>
          <tf-toggle id="cd-failover" ${failoverEnabled ? 'checked' : ''}></tf-toggle>
        </div>
        <div class="form-group">
          <label>${escapeHtml(I18n.t('cluster_detail.failover_target'))}</label>
          <tf-input id="cd-failover-target" value="${escapeAttr(failoverTarget)}" placeholder="${escapeAttr(I18n.t('cluster_detail.failover_target_hint'))}"></tf-input>
        </div>
        <div class="form-group form-group-actions">
          <tf-button variant="primary" id="btn-save-routing">${escapeHtml(I18n.t('common.save'))}</tf-button>
        </div>
      </div>
    </div>
  `;
}

async function saveRoutingSettings() {
  if (!clusterData) return;
  const strategy = byId('cd-strategy')?.value || 'distributed';
  try {
    await ApiBinary.action('clusterUpdateRequest', {
      clusterId: currentClusterId,
      strategy,
    });
    toast(I18n.t('clusters.update_success').replace('{name}', clusterData.name || ''), 'success');
    await loadAll();
    renderDetail();
  } catch (err) {
    toast(err.message || I18n.t('common.error'), 'error');
  }
}

// ---- Shared models -------------------------------------------------------

function renderSharedModels(members) {
  const memberIds = new Set(members.map(m => m.node_id));
  const uniq = new Map();
  for (const entry of unifiedModels) {
    const kindWrapper = entry && entry.kind;
    if (!kindWrapper || kindWrapper.kind !== 'service_model') continue;
    const alias = entry.id;
    if (!alias) continue;
    const instances = Array.isArray(kindWrapper.instances) ? kindWrapper.instances : [];
    const localInstances = instances.filter(i => memberIds.has(i.nodeId || i.node_id));
    if (localInstances.length === 0) continue;
    const surfaces = Array.isArray(entry.serviceSurfaces) ? entry.serviceSurfaces : [];
    const kindLabel = surfaces[0] || 'service';
    if (!uniq.has(alias)) uniq.set(alias, { alias, kind: kindLabel, count: 0 });
    uniq.get(alias).count += localInstances.length;
  }
  const list = Array.from(uniq.values());
  if (list.length === 0) {
    return `
      <div class="cluster-models-section">
        <div class="cluster-matrix-title">${escapeHtml(I18n.t('cluster_detail.shared_models'))}</div>
        <div class="empty-state-small">${escapeHtml(I18n.t('cluster_detail.no_shared_models'))}</div>
      </div>
    `;
  }
  const rows = list.map(m => `
    <div class="model-row">
      <span class="model-kind">${escapeHtml(m.kind || '—')}</span>
      <span class="model-alias"><code>${escapeHtml(m.alias)}</code></span>
      <tf-chip status="online">${escapeHtml(`${m.count}× ${I18n.t('cluster_detail.instance_short')}`)}</tf-chip>
    </div>
  `).join('');
  return `
    <div class="cluster-models-section">
      <div class="cluster-matrix-title">${escapeHtml(I18n.t('cluster_detail.shared_models'))}</div>
      <div class="cluster-detail-card models-card">${rows}</div>
    </div>
  `;
}

// ---- Helpers -------------------------------------------------------------

function resolveMembers(cluster) {
  const raw = cluster.members || cluster.nodes || [];
  return raw.map(m => {
    // Akceptujemy zarowno camelCase (binary) jak i snake_case (legacy).
    const nodeId = m.nodeId || m.node_id || m.id;
    const live = nodesById.get(nodeId);
    return {
      node_id: nodeId,
      role: m.role || 'worker',
      hostname: (live && live.hostname) || m.hostname || m.node_name || nodeId,
      status: m.status || m.member_status || '',
      interface_type: m.interfaceType || m.interface_type || '',
      interface_speed_mbps: m.interfaceSpeedMbps || m.interface_speed_mbps || 0,
      rdma_ip: m.rdmaIp || m.rdma_ip || '',
      rdma_devices: m.rdmaDevices || m.rdma_devices || '',
      rdma_socket_ifname: m.rdmaSocketIfname || m.rdma_socket_ifname || '',
      live,
    };
  });
}

function clusterStatus(members) {
  if (members.length === 0) return 'offline';
  const onlineCnt = members.filter(m => memberOnline(m)).length;
  if (onlineCnt === 0) return 'offline';
  if (onlineCnt < members.length) return 'degraded';
  return 'healthy';
}

// Online liczone z dwoch zrodel: status czlonka z backendu (peer_store) ORAZ
// swiezy stan polaczenia z mesh node list (nodesById -> member.live).
function memberOnline(m) {
  if (!m) return false;
  if (String(m.status || '').toLowerCase() === 'online') return true;
  return isOnline(m.live);
}

function findAssignment(nodeId) {
  if (!nodeId || probeAssignments.length === 0) return null;
  return probeAssignments.find(a => a.node_id === nodeId) || null;
}

function formatBandwidth(mbps) {
  if (mbps == null) return '—';
  return mbps >= 1000 ? `${(mbps / 1000).toFixed(1)} Gbps` : `${mbps} Mbps`;
}

function isOnline(node) {
  return isOnlineHelper(node);
}

function renderStatusChip(status) {
  if (status === 'healthy') return `<tf-chip status="online" dot>${escapeHtml(I18n.t('clusters.status_healthy'))}</tf-chip>`;
  if (status === 'degraded') return `<tf-chip status="warning" dot>${escapeHtml(I18n.t('clusters.status_degraded'))}</tf-chip>`;
  return `<tf-chip status="offline" dot>${escapeHtml(I18n.t('clusters.status_offline'))}</tf-chip>`;
}

function pctOr(v) {
  if (v == null || isNaN(v)) return null;
  return Math.round(v);
}

function connectionClass(type, speedMbps) {
  const t = String(type || '').toLowerCase();
  if (t === 'rdma' || t === 'infiniband') return 'rdma';
  if (t === 'roce') return 'roce';
  if (t === 'thunderbolt') return 'tb';
  if (t === 'wifi' || t === 'wlan') return 'wifi';
  if (speedMbps >= 10000) return 'eth10';
  return 'eth1';
}

function connectionLabel(type, speedMbps) {
  const t = String(type || '').toLowerCase();
  const sp = speedMbps ? (speedMbps >= 1000 ? `${(speedMbps / 1000).toFixed(0)}G` : `${speedMbps}M`) : '';
  if (t === 'rdma' || t === 'infiniband') return `RDMA${sp ? ` ${sp}` : ''}`;
  if (t === 'roce') return `RoCE${sp ? ` ${sp}` : ''}`;
  if (t === 'thunderbolt') return `TB${sp ? ` ${sp}` : ''}`;
  if (t === 'wifi' || t === 'wlan') return `Wi-Fi${sp ? ` ${sp}` : ''}`;
  return sp ? `Ethernet ${sp}` : '';
}

export default ClusterDetailScreen;

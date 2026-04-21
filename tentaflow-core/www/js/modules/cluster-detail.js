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
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-select.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-input.js';

let currentClusterId = null;
let clusterData = null;
let nodesById = new Map();
let unifiedModels = [];
let refreshInterval = null;
let probeUnsub = null;
let probeResults = [];
let probeInProgress = false;

const ClusterDetailScreen = {
  title: 'Cluster',
  async show(clusterId) {
    if (!clusterId) return;
    currentClusterId = clusterId;
    clusterData = null;
    probeResults = [];
    probeInProgress = false;

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
  },
};

// ---- Data ----------------------------------------------------------------

async function loadAll() {
  if (!currentClusterId) return;
  try {
    const [detailBody, nodes, unified] = await Promise.all([
      ApiBinary.one('clusterDetailRequest', { clusterId: currentClusterId }).catch(() => null),
      ApiBinary.list('meshNodeListRequest', { arrayKey: 'nodes' }).catch(() => []),
      ApiBinary.list('modelsUnifiedListRequest', { arrayKey: 'models' }).catch(() => []),
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

  const hasDetail = content.querySelector('.cluster-detail');
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
  const online = isOnline(live);
  const status = online ? 'online' : 'offline';

  const cpuPct = live ? pctOr(live.cpu_usage ?? live.cpu_usage_percent) : null;
  const ram = live && live.ram_total_mb
    ? Math.round(((live.ram_used_mb || 0) / live.ram_total_mb) * 100)
    : null;
  const gpus = live && Array.isArray(live.gpus) ? live.gpus : [];
  const vramUsed = gpus.reduce((s, g) => s + (g.vram_used_mb || 0), 0);
  const vramTotal = gpus.reduce((s, g) => s + (g.vram_total_mb || 0), 0);
  const vramPct = vramTotal > 0 ? Math.round((vramUsed / vramTotal) * 100) : null;

  const linkClass = connectionClass(member.interface_type, member.interface_speed_mbps);
  const linkLabel = connectionLabel(member.interface_type, member.interface_speed_mbps);

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
      ${linkLabel ? `<div class="cdn-link"><span class="link-chip ${linkClass}">${escapeHtml(linkLabel)}</span></div>` : ''}
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
    const online = isOnline(p.member.live);
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
  const onlineCnt = members.filter(m => isOnline(m.live)).length;
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

async function startClusterProbe() {
  if (!clusterData) return;
  const members = resolveMembers(clusterData);
  if (members.length < 2) {
    toast(I18n.t('clusters.select_min_nodes'), 'warning');
    return;
  }

  probeInProgress = true;
  probeResults = [];
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
        onEnd: () => {
          probeInProgress = false;
          probeUnsub = null;
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
  for (const m of unifiedModels) {
    const alias = m.model_name || m.alias;
    if (!alias) continue;
    const instances = Array.isArray(m.instances) ? m.instances : [];
    const inCluster = instances.some(i => memberIds.has(i.node_id));
    if (!inCluster) continue;
    if (!uniq.has(alias)) uniq.set(alias, { alias, kind: m.service_type || m.kind, count: 0 });
    uniq.get(alias).count += instances.filter(i => memberIds.has(i.node_id)).length;
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
      interface_type: m.interfaceType || m.interface_type || '',
      interface_speed_mbps: m.interfaceSpeedMbps || m.interface_speed_mbps || 0,
      live,
    };
  });
}

function clusterStatus(members) {
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

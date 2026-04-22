// =============================================================================
// Plik: modules/mesh.js
// Opis: Widok Mesh — sekcje (ten node / sparowane / oczekujace), kafelki z
//       ring-gauges (CPU/RAM/VRAM-sum/GPU-avg), meta rows (modele, aktywne
//       req/tok-s, RTT), auto-refresh 5s. Zakladki Lista/Diagram (tf-tabs).
//       Pair flow na tf-window. Chipy statusow nodow na tf-chip.
//       Dane z REST /api/mesh/nodes + /api/mesh/pending. JWT z localStorage.
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
import MeshDetailScreen from '/js/modules/mesh-detail.js';
import { renderDiagram, bindDiagramEvents, destroyDiagram } from '/js/modules/mesh-diagram.js';
import { patchInner } from '/js/lib/patch.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-input.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-window.js';

let nodes = [];
let pending = [];
let unifiedModels = [];
let refreshInterval = null;
let activeTab = 'list';

const MeshScreen = {
  title: 'Mesh',
  render() {
    return `
      <div class="mesh-shell">
        <div class="page-header" id="mesh-page-header">
          <div>
            <h1>${I18n.t('mesh.title')}</h1>
            <div class="sub" id="mesh-sub"></div>
          </div>
          <div class="actions">
            <tf-button variant="secondary" icon="plus" id="btn-pair-new">${escapeHtml(I18n.t('mesh.pair_new'))}</tf-button>
          </div>
        </div>

        <div class="mesh-tabs" id="mesh-tabs">
          <tf-tabs variant="soft" value="list" id="mesh-tabs-nav">
            <tf-tab id="list">${escapeHtml(I18n.t('mesh.tab_list'))}</tf-tab>
            <tf-tab id="diagram">${escapeHtml(I18n.t('mesh.tab_diagram'))}</tf-tab>
          </tf-tabs>
          <div class="mesh-tab-spacer"></div>
          <div class="mesh-legend-compact">
            <span><span class="dot" style="background:var(--success,#22c55e);"></span>${escapeHtml(I18n.t('mesh.legend_local'))}</span>
            <span><span class="dot" style="background:var(--accent-1,#6366f1);"></span>${escapeHtml(I18n.t('mesh.legend_paired'))}</span>
            <span><span class="dot" style="background:var(--warning,#f59e0b);"></span>${escapeHtml(I18n.t('mesh.legend_pending'))}</span>
            <span><span class="dot" style="background:var(--text-3,#6a7196);"></span>${escapeHtml(I18n.t('mesh.legend_offline'))}</span>
          </div>
        </div>

        <div id="mesh-tab-content">
          <div class="mesh-loading">${escapeHtml(I18n.t('common.loading'))}</div>
        </div>
      </div>
    `;
  },
  async mount() {
    byId('btn-pair-new')?.addEventListener('click', openPairModal);

    const tabsEl = byId('mesh-tabs-nav');
    if (tabsEl) tabsEl.addEventListener('change', handleTabChange);

    const contentEl = byId('mesh-tab-content');
    if (contentEl) contentEl.addEventListener('click', handleCardClick);

    await loadData();
    renderActiveTab();
    refreshInterval = setInterval(async () => {
      await loadData();
      renderActiveTab();
    }, 5000);
  },
  unmount() {
    if (refreshInterval) {
      clearInterval(refreshInterval);
      refreshInterval = null;
    }
    destroyDiagram();
    nodes = [];
    pending = [];
    unifiedModels = [];
    activeTab = 'list';
  },
};

// ---- Data ----------------------------------------------------------------

async function loadData() {
  try {
    const [nodesResp, pendingResp, unifiedResp] = await Promise.all([
      ApiBinary.list('meshNodeListRequest', { arrayKey: 'nodes' }),
      ApiBinary.list('meshPendingListRequest', { arrayKey: 'pending' }).catch(() => []),
      ApiBinary.list('modelsUnifiedListRequest', { arrayKey: 'models' }).catch(() => []),
    ]);
    nodes = Array.isArray(nodesResp) ? nodesResp : [];
    pending = Array.isArray(pendingResp) ? pendingResp : [];
    unifiedModels = Array.isArray(unifiedResp) ? unifiedResp : [];
    // Merge: backend populuje node.models tylko co ~30s (ModelsSync broadcast).
    // Lokalny service_registry jest swiezy od razu — sciagamy przez modelsUnifiedListRequest
    // i dla kazdego noda dokladamy brakujace modele (dedup po aliasie).
    mergeUnifiedModelsIntoNodes();
    updateSubheader();
  } catch (err) {
    toast(`${I18n.t('mesh.load_error')}: ${err.message}`, 'error');
  }
}

function updateSubheader() {
  const sub = byId('mesh-sub');
  if (!sub) return;
  const total = nodes.length;
  const online = nodes.filter(n => isOnline(n)).length;
  const pendingIncoming = pending.filter(p => p.state === 'incoming').length;
  const parts = [
    `${total} ${pluralize(total, 'mesh.count_node', 'mesh.count_nodes')}`,
    `${online} ${escapeHtml(I18n.t('mesh.online'))}`,
  ];
  if (pendingIncoming > 0) {
    parts.push(`${pendingIncoming} ${escapeHtml(I18n.t('mesh.pending_count'))}`);
  }
  sub.textContent = parts.join(' · ');
}

function mergeUnifiedModelsIntoNodes() {
  if (!Array.isArray(unifiedModels) || unifiedModels.length === 0) return;
  // Zbuduj mape node_id -> lista aliasow + service_type.
  const byNode = new Map();
  for (const m of unifiedModels) {
    const alias = m.model_name || m.alias;
    const kind = m.service_type || m.kind;
    if (!alias) continue;
    const instances = Array.isArray(m.instances) ? m.instances : [];
    for (const inst of instances) {
      const nid = inst.node_id;
      if (!nid) continue;
      if (!byNode.has(nid)) byNode.set(nid, []);
      byNode.get(nid).push({ alias, kind, loaded: inst.status === 'running' || inst.status === 'ready' });
    }
  }
  for (const node of nodes) {
    const extra = byNode.get(node.node_id);
    if (!extra || extra.length === 0) continue;
    const existing = Array.isArray(node.models) ? node.models.slice() : [];
    const seen = new Set(existing.map(m => m.alias).filter(Boolean));
    for (const m of extra) {
      if (!seen.has(m.alias)) {
        existing.push(m);
        seen.add(m.alias);
      }
    }
    node.models = existing;
  }
}

function pluralize(n, singleKey, pluralKey) {
  return escapeHtml(I18n.t(n === 1 ? singleKey : pluralKey));
}

// ---- Tabs -----------------------------------------------------------------

function handleTabChange(e) {
  const id = e.detail?.value;
  if (!id || id === activeTab) return;
  activeTab = id;
  renderActiveTab();
}

function renderActiveTab() {
  const host = byId('mesh-tab-content');
  if (!host) return;
  if (activeTab === 'diagram') {
    patchInner(host, renderDiagram(nodes));
    bindDiagramEvents(host, (nodeId) => MeshDetailScreen.show(nodeId));
  } else {
    patchInner(host, renderListSections());
  }
}

// ---- List / Sections ------------------------------------------------------

function renderListSections() {
  if (nodes.length === 0 && pending.length === 0) {
    return `<div class="empty-state"><div class="empty-state-text">${escapeHtml(I18n.t('mesh.no_nodes'))}</div></div>`;
  }

  const local = nodes.filter(n => n.is_local || n.source === 'local');
  const trusted = nodes.filter(n => !n.is_local && n.source === 'trusted');
  const discovered = nodes.filter(n => !n.is_local && n.source === 'discovered');
  const pendingIncoming = pending.filter(p => p.state === 'incoming');

  let html = '';
  if (local.length > 0) {
    html += renderSection(I18n.t('mesh.section_local'), local, 'local');
  }
  if (trusted.length > 0) {
    html += renderSection(I18n.t('mesh.section_paired'), trusted, 'trusted', trusted.length);
  }
  if (pendingIncoming.length > 0) {
    html += renderPendingSection(pendingIncoming);
  }
  if (discovered.length > 0) {
    html += renderSection(I18n.t('mesh.section_discovered'), discovered, 'discovered', discovered.length);
  }
  return html;
}

function renderSection(title, list, kind, count = null) {
  const countBadge = count != null ? `<span class="section-count">${count}</span>` : '';
  return `
    <h3 class="mesh-section-title">${escapeHtml(title)}${countBadge}</h3>
    <div class="mesh-grid">
      ${list.map(n => renderNodeCard(n, kind)).join('')}
    </div>
  `;
}

function renderPendingSection(list) {
  const cards = list.map(p => renderPendingCard(p)).join('');
  return `
    <h3 class="mesh-section-title">${escapeHtml(I18n.t('mesh.section_pending'))}<span class="section-count">${list.length}</span></h3>
    <div class="mesh-grid">${cards}</div>
  `;
}

/// Karta dla peera odkrytego przez mDNS/DHT, jeszcze nie sparowanego.
/// Dashed orange border, brak gauges, info o sposobie odkrycia + fingerprint.
function renderDiscoveredCard(node) {
  const nodeId = node.node_id || '';
  const fpRaw = nodeId ? nodeId.slice(0, 12) : '';
  const shortFp = fpRaw ? fpRaw.match(/.{1,2}/g).join(':') : '—';
  const hostname = node.hostname || (nodeId ? nodeId.slice(0, 12) : I18n.t('mesh.unknown_host'));
  const ip = node.ip || (node.ip_addresses && node.ip_addresses[0]) || '—';
  const details = [
    escapeHtml(String(ip)),
    escapeHtml(I18n.t('mesh.discovered_via_mdns')),
    `fingerprint ${escapeHtml(shortFp)}...`,
  ].join(' · ');
  return `
    <div class="mesh-card pending" data-node-detail="${escapeAttr(nodeId)}">
      <div class="mesh-card-head">
        <div class="mesh-card-ico pending"><svg class="icon icon-lg"><use href="#i-question"/></svg></div>
        <div class="mesh-card-title">
          <div class="name-t">${escapeHtml(hostname)}<tf-chip status="pending" dot>${escapeHtml(I18n.t('mesh.pending'))}</tf-chip></div>
          <div class="details">${details}</div>
        </div>
        <div class="mesh-card-actions">
          <tf-button variant="primary" size="sm" icon="plus" title="${escapeAttr(I18n.t('mesh.pair'))}" data-node-pair="${escapeAttr(nodeId)}"></tf-button>
        </div>
      </div>
      <div class="mesh-card-meta">
        <div class="meta-item"><svg class="icon"><use href="#i-info"/></svg><span>${escapeHtml(I18n.t('mesh.discovered_hint'))}</span></div>
      </div>
    </div>
  `;
}

function renderPendingCard(pairing) {
  const nodeId = pairing.remoteNodeId || pairing.remote_node_id || '';
  const shortId = nodeId.slice(0, 16);
  return `
    <div class="mesh-card pending">
      <div class="mesh-card-head">
        <div class="mesh-card-ico pending">?</div>
        <div class="mesh-card-title">
          <div class="name-t">${escapeHtml(shortId || I18n.t('mesh.unknown_host'))}<tf-chip status="pending" dot>${escapeHtml(I18n.t('mesh.pending'))}</tf-chip></div>
          <div class="details">${escapeHtml(I18n.t('mesh.pending_hint'))}</div>
        </div>
        <div class="mesh-card-actions">
          <tf-button variant="primary" size="sm" icon="plus" title="${escapeAttr(I18n.t('mesh.pair'))}" data-pairing-confirm="${escapeAttr(nodeId)}"></tf-button>
          <tf-button variant="ghost" size="sm" icon="x" title="${escapeAttr(I18n.t('mesh.reject_pairing'))}" data-pairing-reject="${escapeAttr(nodeId)}"></tf-button>
        </div>
      </div>
      <div class="mesh-card-meta">
        <div class="meta-item"><span><strong>${escapeHtml(I18n.t('mesh.fingerprint'))}:</strong> <code>${escapeHtml(shortId || '—')}</code></span></div>
      </div>
    </div>
  `;
}

function renderNodeCard(node, kind) {
  if (kind === 'discovered') {
    return renderDiscoveredCard(node);
  }
  const nodeId = node.node_id || '';
  const hostname = node.hostname || nodeId.slice(0, 12) || I18n.t('mesh.unknown_host');
  const online = isOnline(node);
  const offlineClass = !online && kind !== 'local' ? ' offline' : '';
  const localClass = kind === 'local' ? ' local' : '';

  // Ikona i kolor - zalezne od kind/status.
  const icoKind = kind === 'local' ? 'local' : kind === 'trusted' ? 'paired' : 'pending';
  const icoChar = kind === 'local' ? '⌂' : kind === 'trusted' ? '◎' : '?';

  // Status chip
  let statusChip = '';
  if (kind === 'local' || online) {
    statusChip = `<tf-chip status="online" dot>${escapeHtml(I18n.t('mesh.online'))}</tf-chip>`;
  } else {
    statusChip = `<tf-chip status="offline" dot>${escapeHtml(I18n.t('mesh.offline'))}</tf-chip>`;
  }

  // Relay chip — jesli routed przez inny node
  let relayChip = '';
  const route = node.route;
  if (route && route.direct === false && route.hops != null && route.next_hop) {
    const hopsLabel = route.hops === 1 ? I18n.t('mesh.hop_one') : I18n.t('mesh.hop_many', { count: route.hops });
    const nextHopNode = nodes.find(n => (n.node_id || '') === route.next_hop);
    const nextHopName = (nextHopNode && nextHopNode.hostname) || route.next_hop.slice(0, 8);
    relayChip = `<tf-chip status="info" title="${escapeAttr(hopsLabel + ' via ' + nextHopName)}">${escapeHtml(hopsLabel)}</tf-chip>`;
  }

  // Details row — IP + (uptime | RTT) + protocol
  const ip = node.ip || (node.ip_addresses && node.ip_addresses[0]) || '—';
  const detailBits = [escapeHtml(String(ip))];
  if (node.os_info) detailBits.push(escapeHtml(node.os_info));
  if (kind === 'local' && node.docker_version) {
    detailBits.push(`Docker ${escapeHtml(node.docker_version)}`);
  }

  // Gauges: CPU / RAM / VRAM-sum / GPU-avg
  const gauges = offlineClass ? '' : buildGauges(node);

  // Meta rows: Modele, Aktywne, (wybrane wg kind)
  const meta = offlineClass ? buildOfflineMeta(node) : buildMeta(node);

  // Akcje
  let actions = '';
  if (kind === 'trusted') {
    actions = `
      <tf-button variant="danger" size="sm" icon="trash" title="${escapeAttr(I18n.t('mesh.revoke_trust'))}" data-node-revoke="${escapeAttr(nodeId)}"></tf-button>
    `;
  } else if (kind === 'discovered') {
    actions = `
      <tf-button variant="primary" size="sm" icon="plus" title="${escapeAttr(I18n.t('mesh.pair'))}" data-node-pair="${escapeAttr(nodeId)}"></tf-button>
    `;
  }

  return `
    <div class="mesh-card${localClass}${offlineClass}" data-node-detail="${escapeAttr(nodeId)}">
      <div class="mesh-card-head">
        <div class="mesh-card-ico ${icoKind}">${icoChar}</div>
        <div class="mesh-card-title">
          <div class="name-t">${escapeHtml(hostname)}${statusChip}${relayChip}</div>
          <div class="details">${detailBits.join(' · ')}</div>
        </div>
        <div class="mesh-card-actions">${actions}</div>
      </div>
      ${gauges}
      ${meta}
    </div>
  `;
}

// ---- Gauges (ring) --------------------------------------------------------

function buildGauges(node) {
  const g = [];

  // CPU
  const cpuPct = pctOr(node.cpu_usage ?? node.cpu_usage_percent, null);
  const cpuSub = node.cpu_count ? `${node.cpu_count} cores` : '';
  g.push(renderRing('CPU', cpuPct != null ? `${cpuPct}` : '—', cpuPct != null ? `${cpuPct}%` : '—', cpuSub, cpuPct));

  // RAM
  if (node.ram_used_mb != null && node.ram_total_mb) {
    const pct = Math.round((node.ram_used_mb / node.ram_total_mb) * 100);
    g.push(renderRing('RAM', formatMb(node.ram_used_mb), '', `${formatMb(node.ram_used_mb)} / ${formatMb(node.ram_total_mb)}`, pct));
  } else {
    g.push(renderRing('RAM', '—', '', '', null));
  }

  // VRAM — suma z wszystkich GPU
  const gpus = Array.isArray(node.gpus) ? node.gpus : [];
  if (gpus.length > 0) {
    const vramUsed = gpus.reduce((s, x) => s + (x.vram_used_mb || 0), 0);
    const vramTotal = gpus.reduce((s, x) => s + (x.vram_total_mb || 0), 0);
    if (vramTotal > 0) {
      const pct = Math.round((vramUsed / vramTotal) * 100);
      const names = gpus.map(x => x.name).filter(Boolean);
      const sub = gpus.length === 1 ? (names[0] || '') : `${gpus.length}× GPU`;
      g.push(renderRing('VRAM', formatMb(vramUsed), '', sub, pct));
    } else {
      g.push(renderRing('VRAM', '—', '', I18n.t('mesh.no_gpu'), null));
    }
    // GPU util — srednia po wszystkich kartach
    const avgUsage = gpus.length > 0 ? Math.round(gpus.reduce((s, x) => s + (x.usage_percent || 0), 0) / gpus.length) : null;
    g.push(renderRing(I18n.t('mesh.gpu_util'), avgUsage != null ? `${avgUsage}` : '—', avgUsage != null ? `${avgUsage}%` : '', gpus.length > 1 ? I18n.t('mesh.gpu_avg_of', { count: gpus.length }) : '', avgUsage));
  } else {
    g.push(renderRing('VRAM', '—', '', I18n.t('mesh.no_gpu'), null));
    g.push(renderRing(I18n.t('mesh.gpu_util'), '—', '', I18n.t('mesh.no_gpu'), null));
  }

  return `<div class="gauges">${g.join('')}</div>`;
}

function renderRing(label, val, unit, sub, pct) {
  const safePct = pct == null ? 0 : Math.max(0, Math.min(100, pct));
  const hot = pct != null && pct > 85 ? ' hot' : (pct != null && pct > 60 ? ' warm' : '');
  const dim = pct == null ? ' dim' : '';
  return `
    <div class="gauge">
      <div class="gauge-ring${hot}${dim}" style="--pct: ${safePct};">
        <div class="gauge-val">${escapeHtml(val)}${unit ? `<span>${escapeHtml(unit.replace(val, ''))}</span>` : ''}</div>
      </div>
      <div class="gauge-label">${escapeHtml(label)}</div>
      <div class="gauge-sub">${escapeHtml(sub || '')}</div>
    </div>
  `;
}

// ---- Meta rows ------------------------------------------------------------

function buildMeta(node) {
  const parts = [];

  // Modele z ModelsSync (peer_store.models[])
  const models = Array.isArray(node.models) ? node.models : [];
  if (models.length > 0) {
    const aliases = models.slice(0, 4).map(m => m.alias).filter(Boolean);
    const more = models.length > 4 ? ` +${models.length - 4}` : '';
    parts.push(`<div class="meta-item"><span><strong>${escapeHtml(I18n.t('mesh.models'))}:</strong> ${escapeHtml(aliases.join(' · '))}${more}</span></div>`);
  } else {
    parts.push(`<div class="meta-item meta-muted"><span><strong>${escapeHtml(I18n.t('mesh.models'))}:</strong> ${escapeHtml(I18n.t('mesh.no_models'))}</span></div>`);
  }

  // Aktywne — req + tok/s
  const active = node.active_requests ?? 0;
  const tps = node.tokens_per_sec ?? 0;
  const tpsLabel = tps > 0 ? ` · ${tps.toFixed(0)} tok/s` : '';
  parts.push(`<div class="meta-item"><span><strong>${escapeHtml(I18n.t('mesh.active'))}:</strong> ${active} ${escapeHtml(I18n.t(active === 1 ? 'mesh.request_one' : 'mesh.request_many'))}${tpsLabel}</span></div>`);

  // Kontenery (jesli sa)
  const cRun = node.containers_running;
  const cTot = node.containers_total;
  if (cRun != null && cTot != null && cTot > 0) {
    parts.push(`<div class="meta-item"><span><strong>${escapeHtml(I18n.t('mesh.containers_short'))}:</strong> ${cRun} / ${cTot}</span></div>`);
  }

  return `<div class="mesh-card-meta">${parts.join('')}</div>`;
}

function buildOfflineMeta(node) {
  const lastSeen = node.discovered_at ? new Date(node.discovered_at).toLocaleString() : '';
  return `
    <div class="mesh-card-meta">
      <div class="meta-item warning-meta">
        <span>${escapeHtml(I18n.t('mesh.offline_last_seen'))} ${escapeHtml(lastSeen)}</span>
      </div>
    </div>
  `;
}

// ---- Helpers --------------------------------------------------------------

function isOnline(node) {
  const s = String(node.status || '').toLowerCase();
  if (node.is_local) return true;
  return s === 'connected' || s === 'online' || s === 'active' || s === 'ready';
}

function pctOr(value, fallback) {
  if (value == null || isNaN(value)) return fallback;
  return Math.round(value);
}

// ---- Click handlers -------------------------------------------------------

function handleCardClick(e) {
  // Pair (outgoing)
  const pairBtn = e.target.closest('[data-node-pair]');
  if (pairBtn) {
    e.stopPropagation();
    const nodeId = pairBtn.dataset.nodePair;
    openPinModal(nodeId);
    return;
  }
  // Revoke trust
  const revokeBtn = e.target.closest('[data-node-revoke]');
  if (revokeBtn) {
    e.stopPropagation();
    const nodeId = revokeBtn.dataset.nodeRevoke;
    revokeTrust(nodeId);
    return;
  }
  // Confirm incoming
  const confirmBtn = e.target.closest('[data-pairing-confirm]');
  if (confirmBtn) {
    e.stopPropagation();
    const nodeId = confirmBtn.dataset.pairingConfirm;
    openConfirmPinModal(nodeId);
    return;
  }
  // Reject incoming
  const rejectBtn = e.target.closest('[data-pairing-reject]');
  if (rejectBtn) {
    e.stopPropagation();
    const nodeId = rejectBtn.dataset.pairingReject;
    rejectPairing(nodeId);
    return;
  }
  // Detail (kliknieto tlo karty)
  const card = e.target.closest('[data-node-detail]');
  if (card) {
    const nodeId = card.dataset.nodeDetail;
    if (nodeId) MeshDetailScreen.show(nodeId);
  }
}

// ---- Pair flow ------------------------------------------------------------

// Generyczny helper do stworzenia okna pairingu (tf-window + backdrop).
function createPairWindow({ title, bodyHtml, submitLabel, submitAction, onSubmit }) {
  const win = document.createElement('tf-window');
  win.setAttribute('title', title);
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('min-width', '420');
  win.setAttribute('width', '460');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');

  const bodyWrap = document.createElement('div');
  bodyWrap.slot = 'body';
  bodyWrap.innerHTML = bodyHtml;
  win.appendChild(bodyWrap);

  const footWrap = document.createElement('div');
  footWrap.slot = 'footer';
  footWrap.innerHTML = `
    <tf-button variant="secondary" data-action="cancel" label="${escapeAttr(I18n.t('common.cancel'))}"></tf-button>
    <tf-button variant="primary" data-action="${escapeAttr(submitAction)}" label="${escapeAttr(submitLabel)}"></tf-button>
  `;
  win.appendChild(footWrap);

  const backdrop = document.createElement('div');
  backdrop.className = 'tf-window-backdrop';
  document.body.appendChild(backdrop);
  document.body.appendChild(win);

  const cleanup = () => {
    if (win.isConnected) win.remove();
    if (backdrop.isConnected) backdrop.remove();
  };

  win.addEventListener('action', async (e) => {
    const action = e.detail?.action;
    if (action === 'cancel' || action === 'close') {
      cleanup();
      return;
    }
    if (action === submitAction) {
      e.preventDefault();
      try {
        const ok = await onSubmit(win);
        if (ok) {
          cleanup();
          await loadData();
          renderActiveTab();
        }
      } catch (err) {
        const errBox = win.querySelector('.form-error');
        if (errBox) {
          errBox.textContent = err.message;
          errBox.hidden = false;
        }
      }
    }
  });

  return win;
}

function openPairModal() {
  // Modal: tylko NodeID. Backend generuje PIN, UI wyswietla go w kolejnym
  // modalu do odczytania uzytkownikowi na drugim urzadzeniu.
  const bodyHtml = `
    <tf-input id="pair-node-id" label="${escapeAttr(I18n.t('mesh.pair_node_id_label'))}" placeholder="${escapeAttr(I18n.t('mesh.pair_node_id_hint'))}" maxlength="64"></tf-input>
    <div class="form-error" hidden></div>
  `;
  createPairWindow({
    title: I18n.t('mesh.pair_title'),
    bodyHtml,
    submitLabel: I18n.t('mesh.pair'),
    submitAction: 'pair',
    onSubmit: async (win) => {
      const idHex = (win.querySelector('#pair-node-id')?.value || '').trim().toLowerCase();
      const errBox = win.querySelector('.form-error');
      if (!/^[0-9a-f]{64}$/.test(idHex)) {
        errBox.textContent = I18n.t('mesh.pair_invalid_node_id');
        errBox.hidden = false;
        return false;
      }
      try {
        const resp = await ApiBinary.action('meshPairingStartRequest', { remoteAddress: idHex });
        if (resp?.pin) {
          openPinDisplayModal(idHex, resp.pin);
        } else {
          toast(I18n.t('mesh.pair_success'), 'success');
        }
        return true;
      } catch (e) {
        errBox.textContent = e.message || I18n.t('mesh.pair_failed');
        errBox.hidden = false;
        return false;
      }
    },
  });
}

function openPinModal(nodeId) {
  // Skrot dla discovered card — od razu inicjuje parowanie i pokazuje PIN.
  // Nic do wpisania: backend generuje PIN, uzytkownik przekazuje drugiemu nodowi.
  (async () => {
    try {
      const resp = await ApiBinary.action('meshPairingStartRequest', { remoteAddress: nodeId });
      if (resp?.pin) {
        openPinDisplayModal(nodeId, resp.pin);
      } else {
        toast(I18n.t('mesh.pair_failed'), 'error');
      }
    } catch (e) {
      toast(`${I18n.t('mesh.pair_failed')}: ${e.message || ''}`, 'error');
    }
  })();
}

/// Modal pokazujacy wygenerowany PIN do przekazania na drugi node. Zawiera
/// odliczanie 60s, NodeID docelowego noda i instrukcje. User kopiuje PIN, idzie
/// do drugiego noda, potwierdza parowanie wpisujac PIN tam.
function openPinDisplayModal(targetNodeId, pin) {
  const pinGroups = pin.replace(/(\d{3})(\d{3})/, '$1 $2');
  const shortId = targetNodeId.slice(0, 16);
  const bodyHtml = `
    <div class="pair-pin-display">
      <div class="pair-pin-hint">${escapeHtml(I18n.t('mesh.pair_pin_display_intro', { node: shortId }))}</div>
      <div class="pair-pin-value" data-pin="${escapeAttr(pin)}">${escapeHtml(pinGroups)}</div>
      <div class="pair-pin-timer"><span id="pair-pin-countdown">60</span>s</div>
      <div class="pair-pin-steps">${escapeHtml(I18n.t('mesh.pair_pin_display_steps'))}</div>
    </div>
  `;
  createPairWindow({
    title: I18n.t('mesh.pair_pin_display_title'),
    bodyHtml,
    submitLabel: I18n.t('common.close') || 'Zamknij',
    submitAction: 'close',
    onSubmit: async () => true,
  });
  // Odliczanie 60s — po wygasnieciu PIN przestaje byc wazny (backend cleanup).
  let remaining = 60;
  const iv = setInterval(() => {
    remaining -= 1;
    const el = document.querySelector('#pair-pin-countdown');
    if (el) el.textContent = String(remaining);
    if (remaining <= 0 || !el) clearInterval(iv);
  }, 1000);
}

function openConfirmPinModal(nodeId) {
  // PIN dla incoming pairing confirm — OTP-style 6 cell input.
  const bodyHtml = `
    <div class="pair-pin-hint">${escapeHtml(I18n.t('mesh.confirm_pin_hint'))}</div>
    <tf-pin-input id="confirm-pin-input" length="6" group-size="3" autofocus></tf-pin-input>
    <div class="form-error" hidden style="text-align:center;"></div>
  `;
  createPairWindow({
    title: I18n.t('mesh.confirm_pin_title'),
    bodyHtml,
    submitLabel: I18n.t('mesh.confirm_pairing'),
    submitAction: 'confirm',
    onSubmit: async (win) => {
      const pinEl = win.querySelector('#confirm-pin-input');
      const pin = pinEl?.value || '';
      const errBox = win.querySelector('.form-error');
      if (pin.length !== 6) {
        errBox.textContent = I18n.t('mesh.pair_invalid_pin');
        errBox.hidden = false;
        pinEl?.setAttribute('error', '');
        setTimeout(() => pinEl?.removeAttribute('error'), 400);
        return false;
      }
      try {
        await ApiBinary.action('meshPairingConfirmRequest', { pairId: nodeId, pin });
        pinEl?.setAttribute('success', '');
        toast(I18n.t('mesh.pair_confirm_success'), 'success');
        return true;
      } catch (e) {
        errBox.textContent = e?.message || I18n.t('mesh.pair_invalid_pin');
        errBox.hidden = false;
        pinEl?.setAttribute('error', '');
        setTimeout(() => pinEl?.removeAttribute('error'), 400);
        return false;
      }
    },
  });
  // Enter na kompletnym PIN auto-submituje (submit event z tf-pin-input).
  queueMicrotask(() => {
    const pinEl = document.querySelector('#confirm-pin-input');
    if (!pinEl) return;
    pinEl.addEventListener('submit', () => {
      const win = pinEl.closest('tf-window');
      win?.querySelector('[data-action="confirm"]')?.click();
    });
  });
}

async function rejectPairing(nodeId) {
  try {
    await ApiBinary.action('meshPairingRejectRequest', { pairId: nodeId });
    toast(I18n.t('mesh.pairing_rejected'), 'success');
    await loadData();
    renderActiveTab();
  } catch (e) {
    toast(`${I18n.t('mesh.pairing_rejected')}: ${e.message}`, 'error');
  }
}

async function revokeTrust(nodeId) {
  if (!confirm(I18n.t('mesh.revoke_confirm'))) return;
  try {
    await ApiBinary.action('meshTrustRevokeRequest', { nodeId });
    toast(I18n.t('mesh.revoke_success'), 'success');
    await loadData();
    renderActiveTab();
  } catch (e) {
    toast(`${I18n.t('mesh.revoke_success')}: ${e.message}`, 'error');
  }
}

export default MeshScreen;

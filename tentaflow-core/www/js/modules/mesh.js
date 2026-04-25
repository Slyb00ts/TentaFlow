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
import { confirmDialog } from '/js/lib/confirm-dialog.js';
import { runPairProgress } from '/js/lib/pair-progress.js';
import { patchInner } from '/js/lib/patch.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-input.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-window.js';

let nodes = [];
let pending = [];
let unifiedModels = [];
let relayHealth = null;
let refreshInterval = null;
let refreshTickCount = 0;
let activeTab = 'list';

const MeshScreen = {
  title: 'Mesh',
  render() {
    // Naglowek + tabs w stylu Users (tf-screen + tf-detail-header). Liczba
    // nodow renderowana jest dynamicznie w updateSubheader/renderHeaderBadges,
    // w pierwszym przebiegu render() pokazujemy 0 — zostanie nadpisane po
    // loadData(). Dla local card uzywamy `i-network` (i-mesh-admin nie istnieje
    // w sprite). Dla tabow `i-management` (lista) i `i-cluster` (diagram).
    return `
      <tf-screen>
        <div slot="breadcrumb" class="tf-breadcrumb">
          <span class="crumb current">${escapeHtml(I18n.t('mesh.title'))}</span>
        </div>
        <div slot="header" class="tf-detail-header">
          <div class="big-ico">
            <svg viewBox="0 0 24 24"><use href="#i-network"/></svg>
          </div>
          <div class="d-meta">
            <div class="d-name">${escapeHtml(I18n.t('mesh.title'))} <span style="color:var(--text-3);font-weight:600;font-size:15px;" id="mesh-count-inline">· 0</span></div>
            <div class="d-sub" id="mesh-sub"></div>
            <div class="d-badges" id="mesh-badges"></div>
          </div>
          <div class="d-actions">
            <tf-button variant="primary" icon="plus" id="btn-pair-new">${escapeHtml(I18n.t('mesh.pair_new'))}</tf-button>
          </div>
        </div>
        <tf-tabs slot="tabs" variant="underline" value="${activeTab}" id="mesh-tabs-nav">
          <tf-tab id="list" icon="management" count="0">${escapeHtml(I18n.t('mesh.tab_list'))}</tf-tab>
          <tf-tab id="diagram" icon="cluster">${escapeHtml(I18n.t('mesh.tab_diagram'))}</tf-tab>
        </tf-tabs>
        <div id="mesh-tab-content">
          <div class="mesh-loading">${escapeHtml(I18n.t('common.loading'))}</div>
        </div>
      </tf-screen>
    `;
  },
  async mount() {
    byId('btn-pair-new')?.addEventListener('click', openPairModal);

    const tabsEl = byId('mesh-tabs-nav');
    if (tabsEl) tabsEl.addEventListener('change', handleTabChange);

    const contentEl = byId('mesh-tab-content');
    if (contentEl) contentEl.addEventListener('click', handleCardClick);

    await loadData();
    await loadRelayHealth();
    renderActiveTab();
    refreshTickCount = 0;
    refreshInterval = setInterval(async () => {
      refreshTickCount += 1;
      await loadData();
      // Relay status probujemy co ~30 s (co 6. tick z 5 s loop) — backend i tak
      // cache'uje wynik probe co 30 s, czesciej nic nie da.
      if (refreshTickCount % 6 === 0) {
        await loadRelayHealth();
      }
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
    relayHealth = null;
    refreshTickCount = 0;
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
  const total = nodes.length;
  const online = nodes.filter(n => isOnline(n)).length;
  const offline = nodes.filter(n => !n.is_local && !isOnline(n)).length;
  const pendingIncoming = pending.filter(p => p.state === 'incoming').length;
  if (sub) {
    const parts = [
      `${total} ${pluralize(total, 'mesh.count_node', 'mesh.count_nodes')}`,
      `${online} ${escapeHtml(I18n.t('mesh.online'))}`,
    ];
    if (offline > 0) {
      parts.push(`${offline} ${escapeHtml(I18n.t('mesh.offline').toLowerCase())}`);
    }
    if (pendingIncoming > 0) {
      parts.push(`${pendingIncoming} ${escapeHtml(I18n.t('mesh.pending_count'))}`);
    }
    sub.textContent = parts.join(' · ');
  }
  const countInline = byId('mesh-count-inline');
  if (countInline) countInline.textContent = `· ${total}`;
  const tabsNav = byId('mesh-tabs-nav');
  if (tabsNav) {
    const listTab = tabsNav.querySelector('tf-tab[id="list"]');
    if (listTab) listTab.setAttribute('count', String(total));
  }
}

async function loadRelayHealth() {
  try {
    relayHealth = await ApiBinary.one('networkRelayStatusRequest');
  } catch {
    relayHealth = { status: 'unknown' };
  }
  renderHeaderBadges();
}

function renderHeaderBadges() {
  const host = byId('mesh-badges');
  if (!host) return;
  if (!relayHealth) {
    host.innerHTML = '';
    return;
  }
  const parts = [];

  // Chip statusu relay-a — kolor zalezny od `status`. Pokazuje nazwe hosta z
  // URL-a (zwart) + RTT gdy connected/degraded.
  const status = String(relayHealth.status || 'unknown').toLowerCase();
  if (status !== 'unknown') {
    const url = String(relayHealth.url || '');
    const host_ = relayHostname(url) || '—';
    const rttMs = Number(relayHealth.rttMs ?? relayHealth.rtt_ms ?? 0);
    let chipStatus = 'info';
    let label = '';
    let withDot = true;
    if (status === 'connected') {
      chipStatus = 'ok';
      label = `${escapeHtml(I18n.t('mesh.relay_label'))} ${escapeHtml(host_)} · ${rttMs} ms`;
    } else if (status === 'degraded') {
      chipStatus = 'warn';
      label = `${escapeHtml(I18n.t('mesh.relay_label'))} ${escapeHtml(host_)} · ${rttMs} ms (${escapeHtml(I18n.t('mesh.relay_degraded'))})`;
    } else if (status === 'unreachable') {
      chipStatus = 'err';
      label = `${escapeHtml(I18n.t('mesh.relay_label'))} ${escapeHtml(host_)} · ${escapeHtml(I18n.t('mesh.relay_unreachable'))}`;
    } else if (status === 'disabled') {
      chipStatus = 'offline';
      withDot = false;
      label = `${escapeHtml(I18n.t('mesh.relay_label'))} · ${escapeHtml(I18n.t('mesh.relay_disabled'))}`;
    }
    parts.push(`<tf-chip status="${chipStatus}"${withDot ? ' dot' : ''}>${label}</tf-chip>`);
  }

  // Bind addr (jak naprawde slucha iroh) — info chip, bez kropki.
  const bind = String(relayHealth.bindAddrActual || relayHealth.bind_addr_actual || '');
  if (bind) {
    const addrOnly = bind.replace(/:\d+$/, '');
    parts.push(`<tf-chip status="info">${escapeHtml(I18n.t('mesh.bind_label'))} ${escapeHtml(addrOnly || bind)}</tf-chip>`);
  }

  host.innerHTML = parts.join('');
}

function relayHostname(url) {
  if (!url) return '';
  try {
    return new URL(url).hostname;
  } catch {
    return url.replace(/^https?:\/\//, '').replace(/\/.*$/, '');
  }
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
  const icoHtml = kind === 'local'
    ? '<svg class="icon icon-lg" aria-hidden="true"><use href="#i-home"/></svg>'
    : kind === 'trusted'
      ? '<svg class="icon icon-lg" aria-hidden="true"><use href="#i-core"/></svg>'
      : '?';

  // Status chip
  let statusChip = '';
  if (kind === 'local' || online) {
    statusChip = `<tf-chip status="online" dot>${escapeHtml(I18n.t('mesh.online'))}</tf-chip>`;
  } else {
    statusChip = `<tf-chip status="offline" dot>${escapeHtml(I18n.t('mesh.offline'))}</tf-chip>`;
  }

  // Relay chip — jesli routed przez inny node. Pokazuje 'via <hostname>' inline,
  // a tooltip daje pelny opis (hopsLabel + 'via' + nazwa). Uzywamy camelCase
  // lub snake_case (obydwa sa setowane przez wasm — patrz protocol/wasm bindings).
  let relayChip = '';
  const route = node.route;
  const nextHop = route && (route.nextHop || route.next_hop);
  if (route && route.direct === false && route.hops != null && nextHop) {
    const hopsLabel = route.hops === 1 ? I18n.t('mesh.hop_one') : I18n.t('mesh.hop_many', { count: route.hops });
    const nextHopNode = nodes.find(n => (n.node_id || '') === nextHop);
    const nextHopName = (nextHopNode && nextHopNode.hostname) || nextHop.slice(0, 8);
    const viaLabel = I18n.t('mesh.via_peer', { peer: nextHopName });
    relayChip = `<tf-chip status="info" title="${escapeAttr(hopsLabel + ' · ' + viaLabel)}">${escapeHtml(hopsLabel)} · ${escapeHtml(viaLabel)}</tf-chip>`;
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

  // Sekcja transportu/konektywnosci — local dostaje 2-pane (bind + relay),
  // peery dostaja `net-conn-row` (transport, adres, RTT). Offline peery dostaja
  // ten sam row ale w wariancie "no path"/"unreachable".
  let connectivityHtml = '';
  if (kind === 'local') {
    connectivityHtml = renderLocalConnectivity(node);
  } else if (kind === 'trusted') {
    connectivityHtml = renderTransportRow(node, online);
  }

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
        <div class="mesh-card-ico ${icoKind}">${icoHtml}</div>
        <div class="mesh-card-title">
          <div class="name-t">${escapeHtml(hostname)}${statusChip}${relayChip}</div>
          <div class="details">${detailBits.join(' · ')}</div>
        </div>
        <div class="mesh-card-actions">${actions}</div>
      </div>
      ${gauges}
      ${connectivityHtml}
      ${meta}
    </div>
  `;
}

// ---- Connectivity (local 2-pane + per-peer transport row) -----------------

/// 2-pane panel "Mesh bind | Relay" — pokazywany TYLKO dla karty `local`.
/// Pobiera dane z relayHealth (bindAddrActual, status relay-a, RTT, last
/// reachable). Hostname interfejsu nie jest dostepny w protokole — pomijamy
/// (mockup pokazywal `enp1s0np0` ale to opcjonalne).
function renderLocalConnectivity(node) {
  const bind = String(relayHealth?.bindAddrActual || relayHealth?.bind_addr_actual || '');
  const fallbackBind = (() => {
    if (bind) return bind;
    const ip = node.ip || (node.ip_addresses && node.ip_addresses[0]) || '';
    return ip ? `${ip}` : '—';
  })();

  const relayStatus = String(relayHealth?.status || 'unknown').toLowerCase();
  const relayUrl = String(relayHealth?.url || '');
  const relayHost = relayHostname(relayUrl) || relayUrl || '—';
  const rttMs = Number(relayHealth?.rttMs ?? relayHealth?.rtt_ms ?? 0);
  const lastSuccess = Number(relayHealth?.lastSuccessUnixSecs ?? relayHealth?.last_success_unix_secs ?? 0);

  let relayDotCls = 'ok';
  let relayLabel = I18n.t('mesh.relay_connected');
  let relayLabelCls = '';
  if (relayStatus === 'degraded') {
    relayDotCls = 'warn'; relayLabel = I18n.t('mesh.relay_degraded'); relayLabelCls = 'warn';
  } else if (relayStatus === 'unreachable') {
    relayDotCls = 'err'; relayLabel = I18n.t('mesh.relay_unreachable'); relayLabelCls = 'err';
  } else if (relayStatus === 'disabled') {
    relayDotCls = 'err'; relayLabel = I18n.t('mesh.relay_disabled'); relayLabelCls = 'err';
  } else if (relayStatus === 'unknown') {
    return ''; // bez danych nie ma sensu rysowac panelu
  }

  const relayMetaParts = [];
  if (relayStatus === 'connected' || relayStatus === 'degraded') {
    relayMetaParts.push(`${escapeHtml(I18n.t('mesh.rtt'))} ${rttMs} ms`);
  }
  if (lastSuccess > 0) {
    relayMetaParts.push(`${escapeHtml(I18n.t('mesh.last_reachable'))} ${escapeHtml(formatRelativeSecs(lastSuccess))}`);
  } else if (relayStatus === 'unreachable' || relayStatus === 'disabled') {
    relayMetaParts.push(escapeHtml(I18n.t('mesh.never_reachable')));
  }

  return `
    <div class="local-connectivity">
      <div class="local-conn-card">
        <div class="lc-head">
          <svg viewBox="0 0 24 24"><rect x="3" y="11" width="18" height="8" rx="1"/><path d="M7 11V7h10v4"/></svg>
          <span class="lc-title">${escapeHtml(I18n.t('mesh.local_bind_section'))}</span>
          <span class="lc-status"><tf-chip status="ok" dot>${escapeHtml(I18n.t('mesh.bind_listening'))}</tf-chip></span>
        </div>
        <div class="lc-body"><code>${escapeHtml(fallbackBind)}</code></div>
      </div>
      <div class="local-conn-card">
        <div class="lc-head">
          <svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="4"/><path d="M2 12h4M18 12h4M12 2v4M12 18v4"/></svg>
          <span class="lc-title">${escapeHtml(I18n.t('mesh.local_relay_section'))}</span>
          <span class="lc-status">
            <span class="lc-dot ${relayDotCls}"></span>
            <span class="lc-status-text${relayLabelCls ? ' ' + relayLabelCls : ''}">${escapeHtml(relayLabel)}</span>
          </span>
        </div>
        <div class="lc-body"><code>${escapeHtml(relayUrl || relayHost)}</code></div>
        ${relayMetaParts.length ? `<div class="lc-meta">${relayMetaParts.join(' · ')}</div>` : ''}
      </div>
    </div>
  `;
}

/// Pojedynczy row "Transport: <pill> <addr> <RTT>" pokazywany pod gauges peera.
/// Dla offline pokazuje `unreachable`/`no path` (zalezne od tego czy mamy
/// jakikolwiek slad poprzedniego connect-a).
function renderTransportRow(node, online) {
  const conn = node.connection;
  const heartbeat = node.heartbeat_ms ?? node.heartbeatMs ?? null;

  let pillCls = '';
  let pillLabel = '';
  let pillSvg = '';
  let addr = '';
  let rttHtml = '';

  if (online && conn && conn.transport === 'p2p') {
    pillCls = 'p2p';
    pillLabel = I18n.t('mesh.transport_p2p');
    pillSvg = '<svg viewBox="0 0 24 24"><path d="M5 12h14M13 6l6 6-6 6"/></svg>';
    addr = String(conn.address || '');
  } else if (online && conn && conn.transport === 'relay') {
    pillCls = 'relay';
    pillLabel = I18n.t('mesh.transport_relay');
    pillSvg = '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="4"/><path d="M2 12h4M18 12h4"/></svg>';
    addr = relayHostname(conn.relay_url || conn.relayUrl) || String(conn.address || '');
  } else if (online && conn) {
    pillCls = 'relay';
    pillLabel = connectionTransportLabel(conn.transport);
    pillSvg = '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="4"/><path d="M2 12h4M18 12h4"/></svg>';
    addr = String(conn.address || '');
  } else {
    // offline: brak `connection` znaczy ze nigdy nie udalo sie polaczyc lub
    // sesja zostala zerwana. Rozrozniamy "unreachable" (mielismy adres ale
    // nieosiagalny) od "no path" (brak jakiegokolwiek sladu).
    const lastSeen = node.discovered_at || node.last_seen || node.lastSeen;
    const everConnected = lastSeen != null;
    pillCls = 'relay-bad';
    pillLabel = everConnected ? I18n.t('mesh.transport_unreachable') : I18n.t('mesh.transport_no_path');
    pillSvg = '<svg viewBox="0 0 24 24"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>';
    addr = everConnected
      ? formatRelativeFromIso(lastSeen, I18n.t('mesh.last_reachable'))
      : '';
  }

  if (online && heartbeat != null) {
    rttHtml = `<span class="rtt">${escapeHtml(I18n.t('mesh.rtt'))} ${Math.round(Number(heartbeat))} ms</span>`;
  }

  return `
    <div class="net-conn-row">
      <span class="label">${escapeHtml(I18n.t('mesh.transport'))}:</span>
      <span class="net-conn-pill ${pillCls}">${pillSvg}${escapeHtml(pillLabel)}</span>
      ${addr ? `<span class="addr">${escapeHtml(addr)}</span>` : ''}
      ${rttHtml}
    </div>
  `;
}

function formatRelativeSecs(epochSecs) {
  if (!epochSecs) return '—';
  const diff = Math.max(0, Math.floor(Date.now() / 1000) - Number(epochSecs));
  if (diff < 60) return `${diff} s ago`;
  if (diff < 3600) return `${Math.floor(diff / 60)} min ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)} h ago`;
  return `${Math.floor(diff / 86400)} d ago`;
}

function formatRelativeFromIso(value, label) {
  if (!value) return '';
  const t = typeof value === 'number' ? value * 1000 : Date.parse(value);
  if (!t || isNaN(t)) return '';
  const secs = Math.floor((Date.now() - t) / 1000);
  if (secs < 0) return '';
  if (secs < 60) return `${label} ${secs} s ago`;
  if (secs < 3600) return `${label} ${Math.floor(secs / 60)} min ago`;
  if (secs < 86400) return `${label} ${Math.floor(secs / 3600)} h ago`;
  return `${label} ${Math.floor(secs / 86400)} d ago`;
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

  // Transport pokazany jest osobnym .net-conn-row pod gauges, wiec tu juz nie
  // duplikujemy connection summary.

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

function connectionTransportLabel(value) {
  if (value === 'p2p') return I18n.t('mesh.connection_p2p');
  if (value === 'relay') return I18n.t('mesh.connection_relay');
  if (value === 'custom') return I18n.t('mesh.connection_custom');
  return I18n.t('mesh.connection_unknown');
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
function createPairWindow({ title, bodyHtml, submitLabel, submitAction, onSubmit, width, minWidth }) {
  const win = document.createElement('tf-window');
  win.setAttribute('title', title);
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('min-width', String(minWidth ?? 420));
  win.setAttribute('width', String(width ?? 460));
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
  // Modal z dwoma zakladkami:
  //   QR — pokaz QR + hex + PIN (drugi nod skanuje albo wpisuje recznie)
  //   ID — wpisz hex drugiego noda recznie (fallback, stare flow)
  const bodyHtml = `
    <div class="pair-tabs">
      <tf-tabs variant="underline" value="qr" id="pair-tabs-nav">
        <tf-tab id="qr">${escapeHtml(I18n.t('mesh.pair_tab_qr'))}</tf-tab>
        <tf-tab id="id">${escapeHtml(I18n.t('mesh.pair_tab_id'))}</tf-tab>
      </tf-tabs>
    </div>
    <div class="pair-tab-panel" data-tab="qr">
      <div class="pair-qr-grid">
        <div class="pair-qr-box" id="pair-qr-box">
          <div class="pair-qr-loading">${escapeHtml(I18n.t('common.loading'))}</div>
        </div>
        <div class="pair-qr-info">
          <p class="pair-qr-hint">${escapeHtml(I18n.t('mesh.pair_qr_hint'))}</p>
          <div class="pair-cred-block">
            <div class="pair-cred-label">
              <span>${escapeHtml(I18n.t('mesh.pair_qr_hex_label'))}</span>
              <button type="button" class="pair-copy-btn" data-copy="hex">${escapeHtml(I18n.t('common.copy'))}</button>
            </div>
            <div class="pair-cred-value" id="pair-invite-hex">—</div>
          </div>
          <div class="pair-cred-block">
            <div class="pair-cred-label">
              <span>${escapeHtml(I18n.t('mesh.pair_qr_pin_label'))}</span>
              <button type="button" class="pair-copy-btn" data-copy="pin">${escapeHtml(I18n.t('common.copy'))}</button>
            </div>
            <div class="pair-cred-value pin" id="pair-invite-pin">—</div>
          </div>
          <div class="pair-pin-timer">
            <div class="ring"></div>
            <span>${escapeHtml(I18n.t('mesh.pair_qr_refresh_in'))} <b id="pair-invite-countdown">60s</b></span>
          </div>
        </div>
      </div>
    </div>
    <div class="pair-tab-panel" data-tab="id" hidden>
      <tf-input id="pair-node-id" label="${escapeAttr(I18n.t('mesh.pair_node_id_label'))}" placeholder="${escapeAttr(I18n.t('mesh.pair_node_id_hint'))}" maxlength="512"></tf-input>
      <tf-input id="pair-node-pin" label="${escapeAttr(I18n.t('mesh.pair_node_pin_label'))}" placeholder="123456" maxlength="6" inputmode="numeric"></tf-input>
      <tf-input id="pair-node-host" label="${escapeAttr(I18n.t('mesh.pair_node_host_label'))}" placeholder="${escapeAttr(I18n.t('mesh.pair_node_host_hint'))}"></tf-input>
      <tf-input id="pair-node-port" label="${escapeAttr(I18n.t('mesh.pair_node_port_label'))}" placeholder="${escapeAttr(I18n.t('mesh.pair_node_port_hint'))}" inputmode="numeric"></tf-input>
      <tf-input id="pair-node-relay" label="${escapeAttr(I18n.t('mesh.pair_node_relay_label'))}" placeholder="${escapeAttr(I18n.t('mesh.pair_node_relay_hint'))}"></tf-input>
      <button type="button" class="pair-scan-btn" id="pair-scan-btn" hidden>
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M23 19a2 2 0 0 1-2 2H3a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4l2-3h6l2 3h4a2 2 0 0 1 2 2z"/><circle cx="12" cy="13" r="4"/></svg>
        <span>${escapeHtml(I18n.t('mesh.pair_scan_camera'))}</span>
      </button>
      <div class="pair-id-hint">${escapeHtml(I18n.t('mesh.pair_id_hint'))}</div>
      <div class="form-error" hidden></div>
    </div>
  `;
  const win = createPairWindow({
    title: I18n.t('mesh.pair_title'),
    bodyHtml,
    submitLabel: I18n.t('mesh.pair'),
    submitAction: 'pair',
    width: 680,
    minWidth: 560,
    onSubmit: async (winEl) => {
      const activeTab = winEl.querySelector('#pair-tabs-nav')?.value || 'qr';
      if (activeTab === 'qr') {
        // Na zakladce QR "Paruj" tylko zamyka — pairing inicjuje drugi nod po
        // zeskanowaniu kodu. User moze skopiowac dane i robic recznie.
        return true;
      }
      const pairInputRaw = (winEl.querySelector('#pair-node-id')?.value || '').trim();
      const pinInput = winEl.querySelector('#pair-node-pin');
      const manualPin = String(pinInput?.value || '').replace(/\D/g, '');
      const manualHost = (winEl.querySelector('#pair-node-host')?.value || '').trim();
      const manualPort = (winEl.querySelector('#pair-node-port')?.value || '').trim();
      const manualRelayUrl = (winEl.querySelector('#pair-node-relay')?.value || '').trim();
      const errBox = winEl.querySelector('[data-tab="id"] .form-error');
      const parsed = await parseManualPairTarget(pairInputRaw);
      const idHex = parsed?.hex || '';
      const effectivePin = manualPin || parsed?.pin || '';
      if (!idHex) {
        if (errBox) {
          errBox.textContent = I18n.t('mesh.pair_invalid_node_id');
          errBox.hidden = false;
        }
        return false;
      }
      if (effectivePin && !/^\d{6}$/.test(effectivePin)) {
        if (errBox) {
          errBox.textContent = I18n.t('mesh.pair_invalid_pin');
          errBox.hidden = false;
        }
        return false;
      }
      const manualAddress = buildManualPairAddress(manualHost, manualPort);
      if (manualHost && !manualAddress) {
        if (errBox) {
          errBox.textContent = I18n.t('mesh.pair_invalid_socket');
          errBox.hidden = false;
        }
        return false;
      }
      if (errBox) errBox.hidden = true;
      const remoteAddresses = uniqueStrings((manualAddress ? [manualAddress] : []).concat(parsed?.addresses || []));
      const remoteRelayUrl = manualRelayUrl || parsed?.relayUrl || '';
      const remoteHostname = parsed?.host || '';
      const remotePublicKey = parsed?.publicKey || '';
      // Otwieramy progress-dialog od razu zamiast zostawiac user-a ze starym
      // modalem i awaita — submit wykonuje sie w tle a kroki przewijaja sie
      // wizualnie. Dla flow z PIN-em (invite aktywny u odbiorcy) konczy sie
      // auto-confirm; bez PIN lub z nieaktywnym — status 'pending' i banner
      // informujacy ze wyslane czeka na akceptacje.
      const result = await runPairProgress({
        target: { hostname: remoteHostname || I18n.t('mesh.unknown_host'), nodeId: idHex },
        submit: async () => {
          // Pairing robi w backendzie do 3 retry x 25s = ~67s + bufor pauz.
          // Default RPC timeout = 30s — za malo, RPC pada przed ostatnim
          // retry. Podbijamy do 90s zeby 3. proba zdazyla sie zakonczyc.
          const resp = await ApiBinary.action('meshPairingStartRequest', {
            remoteAddress: idHex,
            ...(effectivePin ? { pin: effectivePin } : {}),
            ...(remotePublicKey ? { remotePublicKey } : {}),
            ...(remoteAddresses.length ? { remoteAddresses } : {}),
            ...(remoteRelayUrl ? { remoteRelayUrl } : {}),
            ...(remoteHostname ? { remoteHostname } : {}),
          }, { timeoutMs: 90_000 });
          if (resp?.completed) return { outcome: 'confirmed', resp };
          if (resp?.pin) return { outcome: 'pending', resp };
          return { outcome: 'pending', resp };
        },
      });
      if (result.outcome === 'confirmed') {
        toast(I18n.t('mesh.pair_success'), 'success');
        return true;
      }
      if (result.outcome === 'pending') {
        // Dla backward-compat pokaz stary PIN-display modal, zeby user mogl
        // pokazac kod drugiej stronie gdy odbiorca zwrocil pending + pin.
        if (result.resp?.pin && !effectivePin) {
          openPinDisplayModal(idHex, result.resp.pin);
        }
        return true;
      }
      // cancelled / error — nie zamykamy outer dialogu wpisz-ID-PIN,
      // wiadomosc blędu jest juz na progress window.
      return false;
    },
  });
  // Tab switch + QR populate
  wireUpPairTabs(win);
}

/// Podepnij tab-switch + poll invite identity dla QR widoku.
async function wireUpPairTabs(winEl) {
  if (!winEl) return;
  const nav = winEl.querySelector('#pair-tabs-nav');
  const panels = winEl.querySelectorAll('.pair-tab-panel');
  if (nav) {
    nav.addEventListener('change', () => {
      const val = nav.value;
      panels.forEach((p) => {
        p.hidden = p.dataset.tab !== val;
      });
    });
  }

  // Auto-rozpakowanie `tentaflow-pair://...` URL-a wklejonego do pola
  // Node ID: wyciagamy hex do pola id, a PIN / relay / hostname do wlasciwych
  // pol. Dzieki temu user ktory zeskanowal QR systemowa kamera iOS i wkleil
  // calego linka widzi od razu ze PIN sie wypelnil sam — nie musi rozdzielac
  // recznie URL-a na kawalki.
  const idInput = winEl.querySelector('#pair-node-id');
  const pinInput = winEl.querySelector('#pair-node-pin');
  const hostInput = winEl.querySelector('#pair-node-host');
  const relayInput = winEl.querySelector('#pair-node-relay');
  if (idInput) {
    const unpack = async () => {
      const raw = String(idInput.value || '').trim();
      if (!raw.startsWith('tentaflow-pair://')) return;
      try {
        const { parsePairUri } = await import('/js/modules/qr-scanner.js');
        const parsed = parsePairUri(raw);
        if (!parsed) return;
        idInput.value = parsed.hex;
        if (pinInput && parsed.pin && !pinInput.value) pinInput.value = parsed.pin;
        if (relayInput && parsed.relayUrl && !relayInput.value) relayInput.value = parsed.relayUrl;
        if (hostInput && parsed.host && !hostInput.value) hostInput.value = parsed.host;
      } catch (_) { /* ignore */ }
    };
    idInput.addEventListener('paste', () => setTimeout(unpack, 0));
    idInput.addEventListener('input', unpack);
    idInput.addEventListener('change', unpack);
  }
  // Copy buttons
  winEl.querySelectorAll('.pair-copy-btn').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const which = btn.dataset.copy;
      const src = winEl.querySelector(which === 'hex' ? '#pair-invite-hex' : '#pair-invite-pin');
      const txt = (src?.textContent || '').replace(/\s/g, '');
      if (!txt || txt === '—') return;
      try { await navigator.clipboard.writeText(txt); } catch { /* ignore */ }
      const orig = btn.textContent;
      btn.textContent = I18n.t('common.copied') || 'OK';
      setTimeout(() => { btn.textContent = orig; }, 1200);
    });
  });

  // Przycisk "Zeskanuj kamerą" na zakladce "Wpisz ID" — pokazujemy TYLKO gdy
  // urzadzenie wspiera BarcodeDetector (telefon / tablet / nowoczesny laptop
  // z kamera). Kliknicie otwiera fullscreen overlay z kamera, po odczycie
  // QR auto-parse + submit.
  const scanBtn = winEl.querySelector('#pair-scan-btn');
  if (scanBtn) {
    try {
      const qrScanner = await import('/js/modules/qr-scanner.js');
      if (await qrScanner.isScannerSupported()) {
        scanBtn.hidden = false;
        scanBtn.addEventListener('click', async () => {
          try {
            const raw = await qrScanner.scanQr();
            if (!raw) return;
            const parsed = qrScanner.parsePairUri(raw);
            if (!parsed) {
              toast(I18n.t('mesh.qr_scan_invalid'), 'error');
              return;
            }
            // Wklej hex do inputu zeby user widzial co sie dzieje.
            const input = winEl.querySelector('#pair-node-id');
            if (input) input.value = parsed.hex;
            // Auto-submit: wyslij pairing start z odczytanym PIN jako hint.
            // Backend auto-confirm zadziala po stronie QR-owcy gdy PIN zgadza.
            try {
              const resp = await ApiBinary.action('meshPairingStartRequest', {
                remoteAddress: parsed.hex,
                ...(parsed.pin ? { pin: parsed.pin } : {}),
                ...(parsed.publicKey ? { remotePublicKey: parsed.publicKey } : {}),
                ...(parsed.addresses?.length ? { remoteAddresses: parsed.addresses } : {}),
                ...(parsed.relayUrl ? { remoteRelayUrl: parsed.relayUrl } : {}),
                ...(parsed.host ? { remoteHostname: parsed.host } : {}),
              });
              if (resp?.completed) {
                toast(I18n.t('mesh.pair_success'), 'success');
              } else if (!parsed.pin && resp?.pin) {
                openPinDisplayModal(parsed.hex, resp.pin);
              } else {
                toast(I18n.t('mesh.pair_success'), 'success');
              }
              if (winEl.isConnected) winEl.remove();
              document.querySelectorAll('.tf-window-backdrop').forEach((b) => b.remove());
              await loadData();
              renderActiveTab();
            } catch (e) {
              toast(e.message || I18n.t('mesh.pair_failed'), 'error');
            }
          } catch (e) {
            console.warn('[pair-scan]', e?.message);
            toast(I18n.t('mesh.qr_scan_failed'), 'error');
          }
        });
      }
    } catch (e) {
      // Jak import zawiedzie, zostawiamy button ukryty (hidden domyslnie w HTML).
      console.debug('[pair-scan] scanner module unavailable:', e?.message);
    }
  }

  // Pobierz identity + invite PIN, narysuj QR, odliczaj 60s, odswiezaj.
  const QR = await import('/js/lib/qrcode.js').catch(() => null);
  const refresh = async () => {
    if (!winEl.isConnected) return;
    try {
      const resp = await ApiBinary.one('meshIdentityRequest');
      const hex = resp?.nodeId || resp?.node_id || '';
      const pin = resp?.invitePin || resp?.invite_pin || '';
      const host = resp?.hostname || '';
      const relayUrl = resp?.relayUrl || resp?.relay_url || '';
      const publicKey = resp?.publicKey || resp?.public_key || '';
      const addresses = Array.isArray(resp?.addresses) ? resp.addresses.filter(Boolean) : [];
      const hexEl = winEl.querySelector('#pair-invite-hex');
      const pinEl = winEl.querySelector('#pair-invite-pin');
      if (hexEl) hexEl.textContent = hex;
      if (pinEl) pinEl.textContent = pin ? pin.replace(/(\d{3})(\d{3})/, '$1 $2') : '—';
      if (QR && hex) {
        // QR trzyma TYLKO identity + relay (relay-first). Adresy bezposrednie
        // sa pomijane: przy kilkunastu kartach sieciowych/dockerach payload
        // puchnie do kilkuset znakow i QR traci czytelnosc. Peer i tak
        // dostaje nas po relay, a gdy obaj jestesmy w tej samej sieci iroh
        // po otwarciu sesji sam hole-punchuje direct path przez mDNS/DHT.
        // Manual ip:port wpisujemy recznie tylko w trybie offline-LAN.
        const qs = new URLSearchParams();
        qs.set('pin', pin || '');
        qs.set('host', host || '');
        qs.set('ver', '2');
        if (publicKey) qs.set('pk', publicKey);
        if (relayUrl) qs.set('relay', relayUrl);
        const payload = `tentaflow-pair://${hex}?${qs.toString()}`;
        // Zmienna addresses zostaje nieuzywana — poza QR moze byc wyswietlona
        // w UI (lista "adresy tego noda") dla troubleshootingu LAN-only.
        void addresses;
        const svg = await QR.renderQrSvg(payload, { size: 220, errorCorrectionLevel: 'M' });
        const box = winEl.querySelector('#pair-qr-box');
        if (box) box.innerHTML = svg;
      }
    } catch (e) {
      console.warn('[pair-qr] identity fetch:', e?.message);
    }
  };
  await refresh();

  // Countdown — co sekunde. Odswiezamy identity co 50s (server TTL=60s).
  let remaining = 50;
  const countdownEl = winEl.querySelector('#pair-invite-countdown');
  const iv = setInterval(async () => {
    if (!winEl.isConnected) { clearInterval(iv); return; }
    remaining -= 1;
    if (countdownEl) countdownEl.textContent = `${remaining}s`;
    if (remaining <= 0) {
      remaining = 50;
      await refresh();
    }
  }, 1000);
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

async function parseManualPairTarget(raw) {
  if (!raw) return null;
  try {
    const qrScanner = await import('/js/modules/qr-scanner.js');
    const parsed = qrScanner.parsePairUri(raw);
    if (parsed) return parsed;
  } catch (_e) {
    // Ignorujemy — ponizej idzie fallback do czystego hex.
  }
  const idHex = raw.trim().toLowerCase();
  if (!/^[0-9a-f]{64}$/.test(idHex)) return null;
  return {
    hex: idHex,
    pin: '',
    host: '',
    relayUrl: '',
    publicKey: '',
    addresses: [],
  };
}

function buildManualPairAddress(host, port) {
  const hostValue = String(host || '').trim();
  const portValue = String(port || '').trim();
  if (!hostValue) return '';
  if (/^\[[^\]]+\]:\d+$/.test(hostValue) || /^[^:\s]+:\d+$/.test(hostValue)) {
    return hostValue;
  }
  if (!/^\d{1,5}$/.test(portValue)) {
    return '';
  }
  const numericPort = Number(portValue);
  if (numericPort < 1 || numericPort > 65535) {
    return '';
  }
  if (hostValue.includes(':') && !hostValue.startsWith('[')) {
    return `[${hostValue}]:${numericPort}`;
  }
  return `${hostValue}:${numericPort}`;
}

function uniqueStrings(values) {
  return Array.from(new Set((values || []).filter(Boolean)));
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
  const win = createPairWindow({
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
  // Poll — po sparowaniu inicjator usuwa outgoing pending entry. Kiedy nasz
  // entry znika (albo node pojawia sie jako trusted), zamykamy modal automatycznie.
  const pollIv = setInterval(async () => {
    if (!win.isConnected) {
      clearInterval(pollIv);
      return;
    }
    try {
      const pendingResp = await ApiBinary.list('meshPendingListRequest', { arrayKey: 'pending' });
      const stillPending = Array.isArray(pendingResp)
        && pendingResp.some(p => (p.remoteNodeId || p.remote_node_id) === targetNodeId);
      if (!stillPending) {
        clearInterval(pollIv);
        clearInterval(iv);
        if (win.isConnected) win.remove();
        document.querySelectorAll('.tf-window-backdrop').forEach(b => b.remove());
        toast(I18n.t('mesh.pair_confirm_success'), 'success');
        await loadData();
        renderActiveTab();
      }
    } catch (_e) {
      // sil — poll probuje ponownie
    }
  }, 2000);
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
  const peer = nodes.find((n) => (n.nodeId || n.node_id) === nodeId);
  const peerName = peer?.hostname || peer?.displayName || I18n.t('mesh.unknown_host');
  const ok = await confirmDialog({
    title: I18n.t('mesh.revoke_dialog_title'),
    lead: I18n.t('mesh.revoke_dialog_lead'),
    peer: { name: peerName, id: nodeId },
    consequences: [
      I18n.t('mesh.revoke_dialog_cons_disconnect'),
      I18n.t('mesh.revoke_dialog_cons_key'),
      I18n.t('mesh.revoke_dialog_cons_pair_again'),
    ],
    confirmLabel: I18n.t('mesh.revoke_dialog_confirm'),
    confirmIcon: 'trash',
    cancelLabel: I18n.t('common.cancel'),
    variant: 'danger',
  });
  if (!ok) return;
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

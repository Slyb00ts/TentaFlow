// =============================================================================
// Plik: modules/mesh.js
// Opis: Widok Mesh — sekcje (ten node / sparowane / oczekujace), kafelki z
//       ring-gauges (CPU/RAM/VRAM-sum/GPU-avg), meta rows (modele, aktywne
//       req/tok-s, RTT), auto-refresh 5s. Zakladki Lista/Diagram. Pair flow.
//       Dane z REST /api/mesh/nodes + /api/mesh/pending. JWT z localStorage.
// =============================================================================

import {
  byId,
  escapeHtml,
  escapeAttr,
  toast,
  apiGet,
  apiPost,
  apiDelete,
  formatMb,
} from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import MeshDetailScreen from '/js/modules/mesh-detail.js';
import { renderDiagram, bindDiagramEvents, destroyDiagram } from '/js/modules/mesh-diagram.js';
import { patchInner } from '/js/lib/patch.js';

let nodes = [];
let pending = [];
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
            <button class="btn btn-secondary" id="btn-pair-new">
              <span class="ico">+</span> ${escapeHtml(I18n.t('mesh.pair_new'))}
            </button>
          </div>
        </div>

        <div class="mesh-tabs" id="mesh-tabs">
          <div class="mesh-tab active" data-tab="list">${escapeHtml(I18n.t('mesh.tab_list'))}</div>
          <div class="mesh-tab" data-tab="diagram">${escapeHtml(I18n.t('mesh.tab_diagram'))}</div>
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

    const tabsEl = byId('mesh-tabs');
    if (tabsEl) tabsEl.addEventListener('click', handleTabClick);

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
    activeTab = 'list';
  },
};

// ---- Data ----------------------------------------------------------------

async function loadData() {
  try {
    const [nodesResp, pendingResp] = await Promise.all([
      apiGet('/api/mesh/nodes'),
      apiGet('/api/mesh/pending').catch(() => []),
    ]);
    nodes = Array.isArray(nodesResp) ? nodesResp : [];
    pending = Array.isArray(pendingResp) ? pendingResp : [];
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
  const pendingIncoming = pending.filter(p => p.direction === 'incoming').length;
  const parts = [
    `${total} ${pluralize(total, 'mesh.count_node', 'mesh.count_nodes')}`,
    `${online} ${escapeHtml(I18n.t('mesh.online'))}`,
  ];
  if (pendingIncoming > 0) {
    parts.push(`${pendingIncoming} ${escapeHtml(I18n.t('mesh.pending_count'))}`);
  }
  sub.textContent = parts.join(' · ');
}

function pluralize(n, singleKey, pluralKey) {
  return escapeHtml(I18n.t(n === 1 ? singleKey : pluralKey));
}

// ---- Tabs -----------------------------------------------------------------

function handleTabClick(e) {
  const tab = e.target.closest('.mesh-tab');
  if (!tab) return;
  const id = tab.dataset.tab;
  if (!id || id === activeTab) return;
  activeTab = id;
  document.querySelectorAll('.mesh-tab').forEach(t => t.classList.toggle('active', t.dataset.tab === id));
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
  const pendingIncoming = pending.filter(p => p.direction === 'incoming');

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

function renderPendingCard(pairing) {
  const nodeId = pairing.remote_node_id || '';
  const shortId = nodeId.slice(0, 16);
  return `
    <div class="mesh-card pending">
      <div class="mesh-card-head">
        <div class="mesh-card-ico pending">?</div>
        <div class="mesh-card-title">
          <div class="name-t">${escapeHtml(shortId || I18n.t('mesh.unknown_host'))}<span class="tag-status pending">${escapeHtml(I18n.t('mesh.pending'))}</span></div>
          <div class="details">${escapeHtml(I18n.t('mesh.pending_hint'))}</div>
        </div>
        <div class="mesh-card-actions">
          <button class="btn btn-primary btn-icon" title="${escapeAttr(I18n.t('mesh.pair'))}" data-pairing-confirm="${escapeAttr(nodeId)}">+</button>
          <button class="btn btn-ghost btn-icon" title="${escapeAttr(I18n.t('mesh.reject_pairing'))}" data-pairing-reject="${escapeAttr(nodeId)}">×</button>
        </div>
      </div>
      <div class="mesh-card-meta">
        <div class="meta-item"><span><strong>${escapeHtml(I18n.t('mesh.fingerprint'))}:</strong> <code>${escapeHtml(shortId || '—')}</code></span></div>
      </div>
    </div>
  `;
}

function renderNodeCard(node, kind) {
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
  if (kind === 'local') {
    statusChip = `<span class="tag-status online">● ${escapeHtml(I18n.t('mesh.online'))}</span>`;
  } else if (online) {
    statusChip = `<span class="tag-status online">● ${escapeHtml(I18n.t('mesh.online'))}</span>`;
  } else {
    statusChip = `<span class="tag-status offline">● ${escapeHtml(I18n.t('mesh.offline'))}</span>`;
  }

  // Relay chip — jesli routed przez inny node
  let relayChip = '';
  const route = node.route;
  if (route && route.direct === false && route.hops != null && route.next_hop) {
    const hopsLabel = route.hops === 1 ? I18n.t('mesh.hop_one') : I18n.t('mesh.hop_many', { count: route.hops });
    const nextHopNode = nodes.find(n => (n.node_id || '') === route.next_hop);
    const nextHopName = (nextHopNode && nextHopNode.hostname) || route.next_hop.slice(0, 8);
    relayChip = `<span class="tag-status relay" title="${escapeAttr(hopsLabel + ' via ' + nextHopName)}">${escapeHtml(hopsLabel)}</span>`;
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
      <button class="btn btn-ghost btn-icon" title="${escapeAttr(I18n.t('mesh.revoke_trust'))}" data-node-revoke="${escapeAttr(nodeId)}">×</button>
    `;
  } else if (kind === 'discovered') {
    actions = `
      <button class="btn btn-primary btn-icon" title="${escapeAttr(I18n.t('mesh.pair'))}" data-node-pair="${escapeAttr(nodeId)}">+</button>
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
  const gpus = Array.isArray(node.gpu_info) ? node.gpu_info : [];
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
  const hot = pct != null && pct > 80 ? ' hot' : (pct != null && pct > 60 ? ' warm' : '');
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

function openPairModal() {
  // Modal: node_id hex + PIN (outgoing — uzytkownik inicjuje).
  const html = `
    <div class="modal-backdrop active" id="pair-modal">
      <div class="modal">
        <div class="modal-header">
          <h3 class="modal-title">${escapeHtml(I18n.t('mesh.pair_title'))}</h3>
          <button class="modal-close" id="pair-close">×</button>
        </div>
        <div class="modal-body">
          <div class="form-row">
            <label class="label">${escapeHtml(I18n.t('mesh.pair_node_id_label'))}</label>
            <input class="input" id="pair-node-id" placeholder="${escapeAttr(I18n.t('mesh.pair_node_id_hint'))}" maxlength="64">
          </div>
          <div class="form-row">
            <label class="label">${escapeHtml(I18n.t('mesh.pair_pin_label'))}</label>
            <input class="input" id="pair-pin" placeholder="000000" maxlength="6" inputmode="numeric" style="text-align:center;letter-spacing:0.2em;">
            <div class="form-hint">${escapeHtml(I18n.t('mesh.pair_pin_hint'))}</div>
          </div>
          <div class="form-error" id="pair-error" hidden></div>
        </div>
        <div class="modal-footer">
          <button class="btn btn-secondary" id="pair-cancel">${escapeHtml(I18n.t('common.cancel'))}</button>
          <button class="btn btn-primary" id="pair-submit">${escapeHtml(I18n.t('mesh.pair'))}</button>
        </div>
      </div>
    </div>
  `;
  document.body.insertAdjacentHTML('beforeend', html);
  const close = () => byId('pair-modal')?.remove();
  byId('pair-close')?.addEventListener('click', close);
  byId('pair-cancel')?.addEventListener('click', close);
  byId('pair-submit')?.addEventListener('click', async () => {
    const idHex = byId('pair-node-id').value.trim().toLowerCase();
    const pin = byId('pair-pin').value.trim();
    const err = byId('pair-error');
    if (!/^[0-9a-f]{64}$/.test(idHex)) {
      err.textContent = I18n.t('mesh.pair_invalid_node_id');
      err.hidden = false;
      return;
    }
    if (!/^\d{6}$/.test(pin)) {
      err.textContent = I18n.t('mesh.pair_invalid_pin');
      err.hidden = false;
      return;
    }
    try {
      await apiPost(`/api/mesh/pair/${encodeURIComponent(idHex)}`, { pin });
      toast(I18n.t('mesh.pair_success'), 'success');
      close();
      await loadData();
      renderActiveTab();
    } catch (e) {
      err.textContent = e.message;
      err.hidden = false;
    }
  });
}

function openPinModal(nodeId) {
  // PIN dla outgoing pair — skrot gdy node juz wykryty.
  const html = `
    <div class="modal-backdrop active" id="pin-modal">
      <div class="modal">
        <div class="modal-header">
          <h3 class="modal-title">${escapeHtml(I18n.t('mesh.pair_pin_title'))}</h3>
          <button class="modal-close" id="pin-close">×</button>
        </div>
        <div class="modal-body">
          <div class="form-row">
            <label class="label">${escapeHtml(I18n.t('mesh.pair_pin_label'))}</label>
            <input class="input" id="pin-input" placeholder="000000" maxlength="6" inputmode="numeric" style="text-align:center;letter-spacing:0.2em;">
            <div class="form-hint">${escapeHtml(I18n.t('mesh.pair_pin_hint'))}</div>
          </div>
          <div class="form-error" id="pin-error" hidden></div>
        </div>
        <div class="modal-footer">
          <button class="btn btn-secondary" id="pin-cancel">${escapeHtml(I18n.t('common.cancel'))}</button>
          <button class="btn btn-primary" id="pin-submit">${escapeHtml(I18n.t('mesh.pair'))}</button>
        </div>
      </div>
    </div>
  `;
  document.body.insertAdjacentHTML('beforeend', html);
  const close = () => byId('pin-modal')?.remove();
  byId('pin-close')?.addEventListener('click', close);
  byId('pin-cancel')?.addEventListener('click', close);
  byId('pin-submit')?.addEventListener('click', async () => {
    const pin = byId('pin-input').value.trim();
    const err = byId('pin-error');
    if (!/^\d{6}$/.test(pin)) {
      err.textContent = I18n.t('mesh.pair_invalid_pin');
      err.hidden = false;
      return;
    }
    try {
      await apiPost(`/api/mesh/pair/${encodeURIComponent(nodeId)}`, { pin });
      toast(I18n.t('mesh.pair_success'), 'success');
      close();
      await loadData();
      renderActiveTab();
    } catch (e) {
      err.textContent = e.message;
      err.hidden = false;
    }
  });
}

function openConfirmPinModal(nodeId) {
  // PIN dla incoming pairing confirm.
  const html = `
    <div class="modal-backdrop active" id="confirm-pin-modal">
      <div class="modal">
        <div class="modal-header">
          <h3 class="modal-title">${escapeHtml(I18n.t('mesh.confirm_pin_title'))}</h3>
          <button class="modal-close" id="confirm-pin-close">×</button>
        </div>
        <div class="modal-body">
          <div class="form-row">
            <label class="label">${escapeHtml(I18n.t('mesh.pair_pin_label'))}</label>
            <input class="input" id="confirm-pin-input" placeholder="000000" maxlength="6" inputmode="numeric" style="text-align:center;letter-spacing:0.2em;">
            <div class="form-hint">${escapeHtml(I18n.t('mesh.confirm_pin_hint'))}</div>
          </div>
          <div class="form-error" id="confirm-pin-error" hidden></div>
        </div>
        <div class="modal-footer">
          <button class="btn btn-secondary" id="confirm-pin-cancel">${escapeHtml(I18n.t('common.cancel'))}</button>
          <button class="btn btn-primary" id="confirm-pin-submit">${escapeHtml(I18n.t('mesh.confirm_pairing'))}</button>
        </div>
      </div>
    </div>
  `;
  document.body.insertAdjacentHTML('beforeend', html);
  const close = () => byId('confirm-pin-modal')?.remove();
  byId('confirm-pin-close')?.addEventListener('click', close);
  byId('confirm-pin-cancel')?.addEventListener('click', close);
  byId('confirm-pin-submit')?.addEventListener('click', async () => {
    const pin = byId('confirm-pin-input').value.trim();
    const err = byId('confirm-pin-error');
    if (!/^\d{6}$/.test(pin)) {
      err.textContent = I18n.t('mesh.pair_invalid_pin');
      err.hidden = false;
      return;
    }
    try {
      await apiPost(`/api/mesh/pair/${encodeURIComponent(nodeId)}/confirm`, { pin });
      toast(I18n.t('mesh.pair_confirm_success'), 'success');
      close();
      await loadData();
      renderActiveTab();
    } catch (e) {
      err.textContent = e.message;
      err.hidden = false;
    }
  });
}

async function rejectPairing(nodeId) {
  try {
    await apiPost(`/api/mesh/pair/${encodeURIComponent(nodeId)}/reject`, {});
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
    await apiDelete(`/api/mesh/trust/${encodeURIComponent(nodeId)}`);
    toast(I18n.t('mesh.revoke_success'), 'success');
    await loadData();
    renderActiveTab();
  } catch (e) {
    toast(`${I18n.t('mesh.revoke_success')}: ${e.message}`, 'error');
  }
}

export default MeshScreen;

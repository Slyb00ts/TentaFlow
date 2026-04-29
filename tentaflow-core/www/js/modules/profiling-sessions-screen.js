// =============================================================================
// Plik: modules/profiling-sessions-screen.js
// Opis: Globalny ekran "Profiling" w sidebar — agreguje listy multi-source
//       sesji ze wszystkich znanych nodow mesh. Per nod montuje pojedyncza
//       instancje ProfilingSessionsView. Bez parametru `nodeId` (params)
//       traktowany jako "all nodes"; gdy params.nodeId obecny, pokazuje
//       tylko ten jeden nod (wejscie z mesh-detail "View sessions").
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { ProfilingSessionsView } from '/js/modules/profiling-sessions.js';
import { Router } from '/js/router.js';

function escapeHtml(s) {
  return String(s ?? '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

let mountedViews = [];

async function listNodes() {
  try {
    const nodes = await ApiBinary.list('meshNodeListRequest', { arrayKey: 'nodes' });
    return Array.isArray(nodes) ? nodes : [];
  } catch (err) {
    console.warn('[profiling-sessions-screen] meshNodeList failed:', err?.message);
    return [];
  }
}

function disposeViews() {
  for (const v of mountedViews) {
    try { v.unmount(); } catch (_e) { /* ignore */ }
  }
  mountedViews = [];
}

const ProfilingSessionsScreen = {
  title: 'Profiling sessions',

  async show(params = {}) {
    const main = document.getElementById('main');
    if (!main) return;
    disposeViews();

    main.innerHTML = `
      <div class="ps-screen-shell" style="padding: 16px 20px;">
        <div class="ps-screen-head" style="display:flex;align-items:center;gap:12px;margin-bottom:12px;">
          <h2 style="margin:0;">Profiling sessions</h2>
          <span style="color:var(--tf-text-2,#a0a8c8);font-size:13px;" id="ps-screen-sub"></span>
        </div>
        <div id="ps-screen-content"></div>
      </div>
    `;

    const sub = document.getElementById('ps-screen-sub');
    const content = document.getElementById('ps-screen-content');

    const filterNodeId = params && params.nodeId ? String(params.nodeId) : null;
    let nodes = await listNodes();
    if (filterNodeId) {
      nodes = nodes.filter((n) => (n.node_id || '') === filterNodeId);
      // Gdy nod nie zwrocony przez list (np. local lub przed pierwszym tickiem)
      // rezerwujemy slot z params.nodeName.
      if (nodes.length === 0) {
        nodes = [{ node_id: filterNodeId, hostname: params.nodeName || filterNodeId }];
      }
    }

    if (nodes.length === 0) {
      content.innerHTML = `
        <div class="empty-state" style="padding:48px;text-align:center;color:var(--tf-text-2,#a0a8c8);">
          <div style="font-size:15px;margin-bottom:6px;">No nodes available</div>
          <div style="font-size:13px;">Profiling sessions are tracked per mesh node. Add or pair a node first.</div>
        </div>
      `;
      return;
    }

    if (sub) {
      sub.textContent = filterNodeId
        ? `node: ${nodes[0].hostname || filterNodeId}`
        : `${nodes.length} node${nodes.length === 1 ? '' : 's'}`;
    }

    // Renderuje per-node section gdy >1 nod, w przeciwnym razie pojedyncza
    // instancja na pelnej szerokosci bez naglowka.
    for (const n of nodes) {
      const nodeId = n.node_id;
      const nodeName = n.hostname || nodeId;
      if (nodes.length > 1) {
        const header = document.createElement('div');
        header.style.cssText = 'margin: 18px 0 8px; padding: 8px 0; border-top: 1px solid var(--tf-border, #1f2548); font-size:13px; color: var(--tf-text-2, #a0a8c8);';
        header.textContent = `Node: ${nodeName}`;
        content.appendChild(header);
      }
      const host = document.createElement('div');
      content.appendChild(host);
      const view = new ProfilingSessionsView({
        nodeId,
        nodeName,
        availableSources: [],
        onOpenReport: (sessionId) => {
          Router.navigate('profile-report', { nodeId, sessionId });
        },
      });
      mountedViews.push(view);
      try {
        await view.mount(host);
      } catch (err) {
        console.error('[profiling-sessions-screen] mount failed for', nodeId, err);
        host.innerHTML = `<div style="color:var(--tf-danger,#ef4444);padding:8px 0;">Failed to load sessions for ${escapeHtml(nodeName)}: ${escapeHtml(err.message || String(err))}</div>`;
      }
    }
  },

  async unmount() {
    disposeViews();
  },
};

export default ProfilingSessionsScreen;

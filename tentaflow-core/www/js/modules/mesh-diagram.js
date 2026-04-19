// =============================================================================
// Plik: modules/mesh-diagram.js
// Opis: Widok Diagramu polaczen mesh. SVG z wezlami na polu okreglym wokol
//       lokalnego noda + linie krawedzi direct/relay z etykieta "X hops via Y".
//       Kliknieecie wezla otwiera MeshDetailScreen.
// =============================================================================

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';

let listenerBound = null;

/**
 * Renderuje SVG z wezlami mesh i krawedziami.
 * Layout: local node w centrum; paired/discovered na okregu; offline na
 * zewnetrznym okregu z mniejsza opacity.
 *
 * Krawedzie liczone z `node.route`:
 *   direct=true  → linia ciagla od local do peera
 *   direct=false → linia przerywana od local do next_hop + od next_hop do peera
 *                  z etykieta "N hops"
 */
export function renderDiagram(nodes) {
  if (!Array.isArray(nodes) || nodes.length === 0) {
    return `
      <div class="mesh-diagram empty">
        <div class="empty-state-text">${escapeHtml(I18n.t('mesh.diagram_empty'))}</div>
      </div>
    `;
  }

  const local = nodes.find(n => n.is_local || n.source === 'local');
  const peers = nodes.filter(n => !(n.is_local || n.source === 'local'));

  const W = 900;
  const H = 520;
  const cx = W / 2;
  const cy = H / 2;

  // Pozycje: local w srodku, peers na okregu. Radius 200; offline 250.
  const onlinePeers = peers.filter(isOnline);
  const offlinePeers = peers.filter(p => !isOnline(p));
  const positions = new Map();
  if (local) positions.set(local.node_id, { x: cx, y: cy });

  const placeCircular = (list, radius) => {
    const count = list.length;
    if (count === 0) return;
    const step = (Math.PI * 2) / count;
    list.forEach((p, i) => {
      const angle = -Math.PI / 2 + i * step;
      positions.set(p.node_id, {
        x: cx + radius * Math.cos(angle),
        y: cy + radius * Math.sin(angle),
      });
    });
  };
  placeCircular(onlinePeers, 200);
  placeCircular(offlinePeers, 250);

  // Krawedzie
  const edges = [];
  for (const p of peers) {
    const to = positions.get(p.node_id);
    if (!to) continue;
    const route = p.route;
    if (!route || route.hops == null) continue;
    if (route.direct || route.hops <= 1) {
      if (!local) continue;
      const from = positions.get(local.node_id);
      edges.push({
        from,
        to,
        dashed: false,
        label: null,
        color: isOnline(p) ? 'var(--accent-1, #6366f1)' : 'var(--text-3, #6a7196)',
      });
    } else if (route.next_hop) {
      const hop = nodes.find(n => n.node_id === route.next_hop);
      const hopPos = hop ? positions.get(hop.node_id) : (local ? positions.get(local.node_id) : null);
      if (hopPos && local) {
        edges.push({
          from: positions.get(local.node_id),
          to: hopPos,
          dashed: false,
          label: null,
          color: 'var(--accent-1, #6366f1)',
        });
      }
      if (hopPos) {
        edges.push({
          from: hopPos,
          to,
          dashed: true,
          label: I18n.t('mesh.hops_label', { count: route.hops }),
          color: 'var(--info, #60a5fa)',
        });
      }
    }
  }

  const linesSvg = edges.map(e => {
    const dasharray = e.dashed ? ' stroke-dasharray="6,4"' : '';
    const midX = (e.from.x + e.to.x) / 2;
    const midY = (e.from.y + e.to.y) / 2;
    const labelSvg = e.label
      ? `<text x="${midX}" y="${midY - 6}" fill="var(--text-2, #a1a7bf)" font-size="11" text-anchor="middle">${escapeHtml(e.label)}</text>`
      : '';
    return `
      <line x1="${e.from.x}" y1="${e.from.y}" x2="${e.to.x}" y2="${e.to.y}"
            stroke="${e.color}" stroke-width="2" opacity="0.6"${dasharray}/>
      ${labelSvg}
    `;
  }).join('');

  const renderNode = (n) => {
    const pos = positions.get(n.node_id);
    if (!pos) return '';
    const isLocal = n.is_local || n.source === 'local';
    const online = isOnline(n);
    let kind;
    if (isLocal) kind = 'local';
    else if (!online) kind = 'offline';
    else if (n.source === 'trusted') kind = 'paired';
    else kind = 'pending';
    const hostname = n.hostname || (n.node_id ? n.node_id.slice(0, 8) : I18n.t('mesh.unknown_host'));
    const ip = n.ip || '';
    const subLabel = isLocal ? I18n.t('mesh.you') : (online ? (ip || '') : I18n.t('mesh.offline'));
    return `
      <div class="diag-node ${kind}" style="left:${(pos.x / W * 100).toFixed(2)}%; top:${(pos.y / H * 100).toFixed(2)}%;"
           data-node-detail="${escapeAttr(n.node_id || '')}"
           role="button" tabindex="0">
        <div class="diag-dot"></div>
        <div class="diag-label">${escapeHtml(hostname)}<br><small>${escapeHtml(subLabel)}</small></div>
      </div>
    `;
  };

  return `
    <div class="mesh-diagram">
      <svg class="diagram-lines" viewBox="0 0 ${W} ${H}" preserveAspectRatio="xMidYMid meet">
        ${linesSvg}
      </svg>
      ${[...(local ? [local] : []), ...peers].map(renderNode).join('')}
      <div class="diag-hint">${escapeHtml(I18n.t('mesh.diagram_hint'))}</div>
    </div>
  `;
}

export function bindDiagramEvents(hostEl, onNodeClick) {
  destroyDiagram();
  listenerBound = (e) => {
    const target = e.target.closest('[data-node-detail]');
    if (!target) return;
    const id = target.dataset.nodeDetail;
    if (id && typeof onNodeClick === 'function') onNodeClick(id);
  };
  hostEl.addEventListener('click', listenerBound);
}

export function destroyDiagram() {
  if (listenerBound) {
    // listener podpiety do `host-tab-content` — renderActiveTab nadpisuje innerHTML, wiec wystarczy wyzerowac referencje.
    listenerBound = null;
  }
}

function isOnline(node) {
  const s = String(node.status || '').toLowerCase();
  if (node.is_local) return true;
  return s === 'connected' || s === 'online' || s === 'active' || s === 'ready';
}

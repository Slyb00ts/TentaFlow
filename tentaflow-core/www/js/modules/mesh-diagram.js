// =============================================================================
// Plik: modules/mesh-diagram.js
// Opis: Widok Diagramu polaczen mesh. SVG z wezlami na polu okreglym wokol
//       lokalnego noda + linie krawedzi direct/relay z etykieta "X hops via Y".
//       Kliknieecie wezla otwiera MeshDetailScreen. Obsluguje zoom (wheel /
//       pinch) oraz pan (drag myszki / pojedynczy palec touch).
// =============================================================================

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';

let listenerBound = null;
let resetListener = null;
// Trzymane referencje handlerow zoom/pan, zeby mozna je odpiac w destroyDiagram.
let zoomPanCleanup = null;
// Aktualna transformacja warstwy (inner) — wspolna dla SVG-lines i kafelkow nodow.
// Persistuje miedzy rebindami (auto-refresh co 5s) — resetowana tylko przyciskiem.
const view = { tx: 0, ty: 0, scale: 1 };
let viewInitialized = false;
const MIN_ZOOM = 0.3;
const MAX_ZOOM = 4;

// Wymiary "world" SVG — stale. Musza byc zgodne z viewBox uzywanym w renderDiagram.
const WORLD_W = 900;
const WORLD_H = 520;

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

  const W = WORLD_W;
  const H = WORLD_H;
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

  // Krawedzie — RYSUJEMY TYLKO miedzy zaufanymi (sparowanymi) nodami.
  // Discovered-but-not-paired pokazuja sie jako pojedyncze kropki bez krawedzi.
  // Wieloskok: hop (posredni) TEZ musi byc trusted, inaczej nie ma sparowanej sciezki.
  const isTrusted = (n) => n && (n.source === 'trusted' || n.is_local || n.source === 'local');
  const edges = [];
  for (const p of peers) {
    if (!isTrusted(p)) continue;
    const to = positions.get(p.node_id);
    if (!to) continue;
    const route = p.route;
    if (!route || route.hops == null) continue;
    const nextHop = route.nextHop || route.next_hop;
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
    } else if (nextHop) {
      const hop = nodes.find(n => n.node_id === nextHop);
      if (!isTrusted(hop)) continue;
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
    // Pozycja w world-coords zapisana w data-attr — applyTransform() przelicza na piksele.
    // Inline left/top to fallback na wypadek gdyby JS nie zdazyl (uzywa procentow world).
    return `
      <div class="diag-node ${kind}"
           style="left:${(pos.x / W * 100).toFixed(2)}%; top:${(pos.y / H * 100).toFixed(2)}%;"
           data-node-detail="${escapeAttr(n.node_id || '')}"
           data-world-x="${pos.x.toFixed(2)}"
           data-world-y="${pos.y.toFixed(2)}"
           role="button" tabindex="0">
        <div class="diag-dot"></div>
        <div class="diag-label">${escapeHtml(hostname)}<br><small>${escapeHtml(subLabel)}</small></div>
      </div>
    `;
  };

  return `
    <div class="mesh-diagram" data-allow-pinch>
      <button type="button" class="diag-reset-btn" data-diag-reset title="${escapeAttr(I18n.t('mesh.diagram_reset') || 'Reset')}">⟳</button>
      <div class="mesh-diagram-inner">
        <svg class="diagram-lines" viewBox="0 0 ${W} ${H}" preserveAspectRatio="xMidYMid meet">
          ${linesSvg}
        </svg>
        ${[...(local ? [local] : []), ...peers].map(renderNode).join('')}
      </div>
      <div class="diag-hint">${escapeHtml(I18n.t('mesh.diagram_hint'))}</div>
    </div>
  `;
}

export function bindDiagramEvents(hostEl, onNodeClick) {
  destroyDiagram();

  // Reset tylko przy pierwszym montazu. Auto-refresh (rebind co 5s) zachowuje view.
  if (!viewInitialized) {
    view.tx = 0;
    view.ty = 0;
    view.scale = 1;
    viewInitialized = true;
  }

  const diag = hostEl.querySelector('.mesh-diagram');
  const inner = hostEl.querySelector('.mesh-diagram-inner');
  if (!diag || !inner) return;

  // Przelicza viewBox SVG (wektor — ostro przy kazdym zoom) + pozycje pikselowe
  // nodow DOM. Zamiast CSS transform: scale() (ktory rasteryzuje layer i daje pikseloze).
  const applyTransform = () => {
    const svg = diag.querySelector('.diagram-lines');
    if (!svg) return;
    const rect = diag.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) return;

    // SVG zoom przez viewBox: ekran->world transformacja odwrotna.
    // tx/ty w pikselach ekranu, scale jako multiplier. World-box do wyswietlenia:
    const vw = WORLD_W / view.scale;
    const vh = WORLD_H / view.scale;
    const vx = -view.tx * WORLD_W / (rect.width * view.scale);
    const vy = -view.ty * WORLD_H / (rect.height * view.scale);
    svg.setAttribute('viewBox', `${vx} ${vy} ${vw} ${vh}`);
    // preserveAspectRatio=none pozwala niezaleznie skalowac X/Y (zgodnie z tym
    // jak liczymy pozycje nodow w pikselach — tez niezaleznie na osiach).
    svg.setAttribute('preserveAspectRatio', 'none');

    // Pozycje DOM nodow w pikselach ekranu.
    const pxPerWorldX = rect.width / WORLD_W;
    const pxPerWorldY = rect.height / WORLD_H;
    const nodeEls = diag.querySelectorAll('.diag-node');
    nodeEls.forEach((n) => {
      const wx = parseFloat(n.dataset.worldX);
      const wy = parseFloat(n.dataset.worldY);
      if (!Number.isFinite(wx) || !Number.isFinite(wy)) return;
      const screenX = wx * pxPerWorldX * view.scale + view.tx;
      const screenY = wy * pxPerWorldY * view.scale + view.ty;
      n.style.left = `${screenX}px`;
      n.style.top = `${screenY}px`;
    });

    // Skala fontu / kropki przez CSS var — tekst i dot rosna razem z zoomem.
    inner.style.setProperty('--tf-diag-scale', view.scale.toFixed(3));
  };
  applyTransform();

  // Klik na wezel (detal) — musi byc przed pan, zeby pan go nie blokowal.
  listenerBound = (e) => {
    // Reset
    if (e.target.closest('[data-diag-reset]')) {
      view.tx = 0;
      view.ty = 0;
      view.scale = 1;
      applyTransform();
      return;
    }
    // Kliknieecie wezla → detail (ale tylko jesli to NIE byl drag, sprawdza _wasDrag flag).
    if (hostEl._diagWasDrag) {
      hostEl._diagWasDrag = false;
      return;
    }
    const target = e.target.closest('[data-node-detail]');
    if (!target) return;
    const id = target.dataset.nodeDetail;
    if (id && typeof onNodeClick === 'function') onNodeClick(id);
  };
  hostEl.addEventListener('click', listenerBound);

  // Zoom wheel — centrum na kursorze.
  const onWheel = (e) => {
    e.preventDefault();
    const rect = diag.getBoundingClientRect();
    const mx = e.clientX - rect.left;
    const my = e.clientY - rect.top;
    const factor = e.deltaY < 0 ? 1.15 : 1 / 1.15;
    const newScale = Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, view.scale * factor));
    const realFactor = newScale / view.scale;
    // Utrzymuj punkt pod kursorem w tym samym miejscu na ekranie.
    view.tx = mx - realFactor * (mx - view.tx);
    view.ty = my - realFactor * (my - view.ty);
    view.scale = newScale;
    applyTransform();
  };
  diag.addEventListener('wheel', onWheel, { passive: false });

  // Pan myszka (pointer).
  let panning = false;
  let panStart = null;
  let dragMoved = 0;
  const onPointerDown = (e) => {
    // Pomin gdy klik na wezel — pozwolimy pojsc do listenerBound.
    if (e.target.closest('[data-node-detail]') || e.target.closest('[data-diag-reset]')) return;
    panning = true;
    dragMoved = 0;
    panStart = { x: e.clientX, y: e.clientY, tx: view.tx, ty: view.ty };
    try { diag.setPointerCapture(e.pointerId); } catch (_) {}
  };
  const onPointerMove = (e) => {
    if (!panning) return;
    const dx = e.clientX - panStart.x;
    const dy = e.clientY - panStart.y;
    dragMoved += Math.abs(dx) + Math.abs(dy);
    view.tx = panStart.tx + dx;
    view.ty = panStart.ty + dy;
    applyTransform();
  };
  const onPointerUp = (e) => {
    if (!panning) return;
    panning = false;
    try { diag.releasePointerCapture(e.pointerId); } catch (_) {}
    // Jesli uzytkownik przeciagnal > 5px, traktujemy to jako drag (nie klik).
    if (dragMoved > 5) hostEl._diagWasDrag = true;
  };
  diag.addEventListener('pointerdown', onPointerDown);
  diag.addEventListener('pointermove', onPointerMove);
  diag.addEventListener('pointerup', onPointerUp);
  diag.addEventListener('pointercancel', onPointerUp);

  // Touch pinch-to-zoom (2 palce).
  let pinch = null;
  const onTouchMove = (e) => {
    if (e.touches.length !== 2) return;
    e.preventDefault();
    const t1 = e.touches[0];
    const t2 = e.touches[1];
    const dist = Math.hypot(t2.clientX - t1.clientX, t2.clientY - t1.clientY);
    const rect = diag.getBoundingClientRect();
    const cx = (t1.clientX + t2.clientX) / 2 - rect.left;
    const cy = (t1.clientY + t2.clientY) / 2 - rect.top;
    if (!pinch) {
      pinch = { dist, scale: view.scale, tx: view.tx, ty: view.ty, cx, cy };
      return;
    }
    const factor = dist / pinch.dist;
    const newScale = Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, pinch.scale * factor));
    const realFactor = newScale / pinch.scale;
    view.tx = pinch.cx - realFactor * (pinch.cx - pinch.tx);
    view.ty = pinch.cy - realFactor * (pinch.cy - pinch.ty);
    view.scale = newScale;
    applyTransform();
    // Flaga drag zeby uniknac przypadkowego kliknecia wezla po pinch.
    hostEl._diagWasDrag = true;
  };
  const onTouchEnd = () => { pinch = null; };
  diag.addEventListener('touchmove', onTouchMove, { passive: false });
  diag.addEventListener('touchend', onTouchEnd);
  diag.addEventListener('touchcancel', onTouchEnd);

  // Resize okna — pxPerWorld sie zmienia, trzeba przeliczyc pozycje.
  const onResize = () => applyTransform();
  window.addEventListener('resize', onResize);

  zoomPanCleanup = () => {
    diag.removeEventListener('wheel', onWheel);
    diag.removeEventListener('pointerdown', onPointerDown);
    diag.removeEventListener('pointermove', onPointerMove);
    diag.removeEventListener('pointerup', onPointerUp);
    diag.removeEventListener('pointercancel', onPointerUp);
    diag.removeEventListener('touchmove', onTouchMove);
    diag.removeEventListener('touchend', onTouchEnd);
    diag.removeEventListener('touchcancel', onTouchEnd);
    window.removeEventListener('resize', onResize);
  };
}

export function destroyDiagram() {
  if (zoomPanCleanup) {
    try { zoomPanCleanup(); } catch (_) {}
    zoomPanCleanup = null;
  }
  listenerBound = null;
  resetListener = null;
}

function isOnline(node) {
  const s = String(node.status || '').toLowerCase();
  if (node.is_local) return true;
  return s === 'connected' || s === 'online' || s === 'active' || s === 'ready';
}

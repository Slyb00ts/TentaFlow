// =============================================================================
// File: modules/org-structure/render.js
// Description: SVG markup of the organization chart, as strings. Pure functions
//   over a computed layout: person cards, unit frames, level-of-detail tiles,
//   orthogonal rounded connectors and the standalone-export stylesheet. Strings
//   rather than DOM nodes because the chart re-renders the visible slice on
//   every pan step, and a template string is several times cheaper than
//   createElementNS at that rate; the same generator feeds the live chart and
//   the SVG/PNG/PDF exports, so what is printed is what is on screen.
// =============================================================================

import { DEFAULT_METRICS } from '/js/modules/org-structure/layout.js';
import { textWidth, wrapLines } from '/js/modules/org-structure/text-metrics.js';

export const LOD_ZOOM = 0.45;
const CORNER = 10;

export function esc(text) {
  return String(text ?? '')
    .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;').replace(/'/g, '&#39;');
}

function num(n) {
  return Math.round(n * 100) / 100;
}

/** Sizes of everything drawn, for a typography scale (1 = normal, larger in presentation mode). */
export function metricsFor(scale = 1) {
  const s = scale;
  return {
    s,
    cardW: 206 * s,
    cardH: 52 * s,
    stackW: 180 * s,
    tileW: 172 * s,
    layout: {
      hgap: DEFAULT_METRICS.hgap * s,
      vgap: DEFAULT_METRICS.vgap * s,
      tightGap: DEFAULT_METRICS.tightGap * s,
      staffGap: DEFAULT_METRICS.staffGap * s,
      stackIndent: DEFAULT_METRICS.stackIndent * s,
      stackTopGap: DEFAULT_METRICS.stackTopGap * s,
      pad: DEFAULT_METRICS.pad * s,
    },
  };
}

function avatarClass(id) {
  let h = 0;
  for (let i = 0; i < id.length; i += 1) h = (h * 31 + id.charCodeAt(i)) >>> 0;
  return `ot-av-${h % 8}`;
}

function initialsOf(name) {
  const parts = String(name ?? '').trim().split(/\s+/).filter(Boolean);
  return parts.length ? parts.slice(0, 2).map((p) => Array.from(p)[0]).join('').toUpperCase() : '?';
}

// The user glyph of the sprite, inlined: an exported SVG has no sprite to point `<use>` at.
function userGlyph(cx, cy, size) {
  const k = size / 24;
  return `<g class="ot-glyph" transform="translate(${num(cx - size / 2)} ${num(cy - size / 2)}) scale(${num(k)})"><circle cx="12" cy="7" r="4"/><path d="M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2"/></g>`;
}

function avatar(node, cx, cy, r, s, fontSize) {
  if (node.vacant) {
    return `<circle class="ot-av-vac" cx="${num(cx)}" cy="${num(cy)}" r="${num(r)}"/>${userGlyph(cx, cy, r * 0.95)}`;
  }
  return `<circle class="ot-av ${avatarClass(node.id)}" cx="${num(cx)}" cy="${num(cy)}" r="${num(r)}"/>`
    + `<text class="ot-av-text" x="${num(cx)}" y="${num(cy + fontSize * 0.36)}" font-size="${num(fontSize)}" text-anchor="middle">${esc(initialsOf(node.name))}</text>`;
}

function pill(label, x, y, s, cls, attrs = '') {
  const w = label.length * 5.6 * s + 14 * s;
  return {
    width: w,
    svg: `<g class="ot-tag ${cls}"${attrs} transform="translate(${num(x - w)} ${num(y)})"><rect width="${num(w)}" height="${num(15 * s)}" rx="${num(7.5 * s)}"/>`
      + `<text x="${num(w / 2)}" y="${num(11 * s)}" font-size="${num(9.5 * s)}" text-anchor="middle">${esc(label)}</text></g>`,
  };
}

const BADGE_ORDER = ['me', 'absent', 'covering', 'acting', 'deputy', 'staff'];

function badgePills(node, w, s, labels) {
  let right = w - 10 * s;
  let out = '';
  const shown = BADGE_ORDER.filter((kind) => node.badges.includes(kind)).slice(0, 3);
  for (const kind of shown) {
    const { width, svg } = pill(labels.badge(kind), right, -8 * s, s, `ot-tag-${kind}`);
    out += svg;
    right -= width + 4 * s;
  }
  return out;
}

const CARD_MAX = 300;
const STACK_MAX = 274;

/** The name line of a card: the holder (with "+N" for further holders) or the vacancy. */
export function cardName(node, labels) {
  if (node.vacant) return `— ${labels.vacancy} —`;
  return node.extraHolders ? `${node.name} +${node.extraHolders}` : node.name;
}

/**
 * Size of a card from what it says: as wide as its longest line needs (between the normal width
 * and a maximum), and above the maximum the text wraps and the card grows taller. Nothing is cut.
 */
export function cardFit(node, m, stacked, labels, handleRoom = 0) {
  const { s } = m;
  const base = (stacked ? 180 : 206) * s;
  const max = (stacked ? STACK_MAX : CARD_MAX) * s;
  const name = cardName(node, labels);
  const need = 64 * s + handleRoom + Math.max(textWidth(name, 12.5 * s, 700), textWidth(node.role, 10.5 * s, 400));
  const w = Math.min(max, Math.max(base, Math.ceil(need)));
  const area = w - 64 * s - handleRoom;
  const nameLines = wrapLines(name, area, 12.5 * s, 700);
  const roleLines = wrapLines(node.role, area, 10.5 * s, 400);
  const content = nameLines.length * 15.5 * s + roleLines.length * 13.5 * s;
  return { w, h: Math.max(m.cardH, Math.ceil(content + 22 * s)), nameLines, roleLines };
}

// The "⋯" handle of the edit mode: a round hit target with three dots. `data-menu` is what the
// chart's click and keyboard handling look for.
function menuHandle(id, kind, cx, cy, s, label) {
  const r = 10 * s;
  const dot = (dx) => `<circle class="ot-menu-dot" cx="${num(cx + dx * s)}" cy="${num(cy)}" r="${num(1.4 * s)}"/>`;
  return `<g class="ot-menu" data-menu="${esc(id)}" data-kind="${kind}" role="button" tabindex="-1" aria-label="${esc(label)}">`
    + `<title>${esc(label)}</title><circle class="ot-menu-bg" cx="${num(cx)}" cy="${num(cy)}" r="${num(r)}"/>${dot(-4.5)}${dot(0)}${dot(4.5)}</g>`;
}

/**
 * One person (or vacancy) card of the persons view.
 * ctx: { s, lod, labels, uid, level, selectedId, activeId, pathSet, matchSet, expandable, expanded, editing }
 */
export function cardSvg(node, item, ctx) {
  const { s, labels } = ctx;
  // A stacked, folded card reserves room on its right for the pill; the card itself is narrower.
  const w = item.w - (item.node.padRight ?? 0);
  const { h } = item;
  const x = num(item.x);
  const y = num(item.y);
  const selected = ctx.selectedId === node.id;
  const cls = ['ot-card',
    node.vacant && 'ot-vacant', node.staff && 'ot-staff', node.badges.includes('me') && 'ot-me',
    ctx.pathSet?.has(node.id) && 'ot-path', selected && 'ot-selected', ctx.activeId === node.id && 'ot-active',
    ctx.matchSet?.has(node.id) && 'ot-match', node.mark && `ot-mark-${node.mark}`].filter(Boolean).join(' ');
  const label = node.vacant ? `${labels.vacancy}, ${node.role}` : `${node.name}, ${node.role}, ${node.unitName}`;
  const { nameLines, roleLines } = item.node.fit;
  const top = (h - (nameLines.length * 15.5 + roleLines.length * 13.5) * s) / 2;
  const lines = nameLines.map((text, i) => `<text class="ot-name" x="${num(52 * s)}" y="${num(top + (12 + i * 15.5) * s)}" font-size="${num(12.5 * s)}">${esc(text)}</text>`).join('')
    + roleLines.map((text, i) => `<text class="ot-role" x="${num(52 * s)}" y="${num(top + (nameLines.length * 15.5 + 11 + i * 13.5) * s)}" font-size="${num(10.5 * s)}">${esc(text)}</text>`).join('');
  const expandAttr = ctx.expandable ? ` aria-expanded="${ctx.expanded}"` : '';
  // The count doubles as the collapse handle of an expanded manager.
  const countLabel = node.below > 0 ? String(node.below) : (ctx.expandable && ctx.expanded ? '−' : '');
  const count = countLabel && !node.staff
    ? pill(countLabel, w - 10 * s, h - 9 * s, s, 'ot-count', ctx.expandable ? ` data-toggle="${esc(node.id)}"` : '').svg
    : '';
  const handle = ctx.editing ? menuHandle(node.id, 'position', w - 17 * s, h / 2 - 5 * s, s, labels.menu(node.vacant ? `${labels.vacancy}, ${node.role}` : node.name)) : '';
  return `<g class="${cls}" id="${ctx.uid}-${esc(node.id)}" data-node="${esc(node.id)}" role="treeitem" aria-level="${ctx.level}" aria-selected="${selected}"${expandAttr} aria-label="${esc(label)}" transform="translate(${x} ${y})">`
    + `<title>${esc(label)}</title>`
    + `<rect class="ot-card-bg" width="${num(w)}" height="${num(h)}" rx="${num(CORNER * s)}"/>`
    + `<rect class="ot-bar" x="0" y="${num(8 * s)}" width="${num(3.5 * s)}" height="${num(h - 16 * s)}" rx="${num(1.75 * s)}" fill="${esc(node.color)}"/>`
    + avatar(node, 28 * s, h / 2, 15 * s, s, 10.5 * s)
    + lines
    + badgePills(node, w, s, labels)
    + count
    + handle
    + '</g>';
}

/** Card at low zoom: a colour block, and a unit tile (name and head count) for unit heads. */
export function lodSvg(node, item, ctx) {
  const w = item.w - (item.node.padRight ?? 0);
  const { h } = item;
  const x = num(item.x);
  const y = num(item.y);
  const selected = ctx.selectedId === node.id;
  const label = node.vacant ? `${ctx.labels.vacancy}, ${node.role}` : `${node.name}, ${node.role}, ${node.unitName}`;
  const cls = ['ot-lod', selected && 'ot-selected', ctx.pathSet?.has(node.id) && 'ot-path', ctx.activeId === node.id && 'ot-active',
    node.mark && `ot-mark-${node.mark}`].filter(Boolean).join(' ');
  const expandAttr = ctx.expandable ? ` aria-expanded="${ctx.expanded}"` : '';
  const base = `<g class="${cls}" id="${ctx.uid}-${esc(node.id)}" data-node="${esc(node.id)}" role="treeitem" aria-level="${ctx.level}" aria-selected="${selected}"${expandAttr} aria-label="${esc(label)}" transform="translate(${x} ${y})">`
    + `<title>${esc(label)}</title>`
    + `<rect width="${num(w)}" height="${num(h)}" rx="${num(CORNER * ctx.s)}" fill="${esc(node.color)}" fill-opacity="${node.isUnitHead ? 0.85 : 0.5}"/>`;
  if (!node.isUnitHead) return `${base}</g>`;
  const size = Math.max(12 * ctx.s, Math.min(h * 0.42, 13 / ctx.zoom));
  // The type shrinks to the tile instead of being cut.
  const fit = (text, px, weight) => Math.min(px, (px * (w - 8 * ctx.s)) / Math.max(1, textWidth(text, px, weight)));
  const count = ctx.labels.people(node.unitPeople);
  const nameSize = fit(node.unitName, size, 800);
  const countSize = fit(count, size * 0.8, 600);
  return `${base}<text class="ot-lod-name" x="${num(w / 2)}" y="${num(h / 2)}" font-size="${num(nameSize)}" text-anchor="middle">${esc(node.unitName)}</text>`
    + `<text class="ot-lod-count" x="${num(w / 2)}" y="${num(h / 2 + size * 1.05)}" font-size="${num(countSize)}" text-anchor="middle">${esc(count)}</text></g>`;
}

/** The "+N" pill under a folded node, joined to the card by a short stem. */
export function morePillSvg(node, item, count, ctx) {
  const { s } = ctx;
  const label = `+${count}`;
  const w = label.length * 6.4 * s + 16 * s;
  // Below the card in the vertical chart, beside it in the horizontal one and in a stack.
  const beside = ctx.horizontal || item.role === 'stacked';
  const cardRight = item.x + item.w - (item.node.padRight ?? 0);
  const left = beside ? cardRight + 8 * s : item.x + item.w / 2 - w / 2;
  const top = beside ? item.y + item.h / 2 - 9 * s : item.y + item.h + 20 * s;
  return `<g class="ot-more" data-more="${esc(node.id)}" role="button" tabindex="-1" aria-label="${esc(ctx.labels.expand(count))}" transform="translate(${num(left)} ${num(top)})">`
    + `<title>${esc(ctx.labels.expand(count))}</title>`
    + `<rect width="${num(w)}" height="${num(18 * s)}" rx="${num(9 * s)}"/><text x="${num(w / 2)}" y="${num(12.5 * s)}" font-size="${num(10 * s)}" text-anchor="middle">${esc(label)}</text></g>`;
}

// ---------------------------------------------------------------------------
// Units view
// ---------------------------------------------------------------------------

const GRID_MAX = 24;

/** Width of a member tile: the longest name or role in the tree, between the normal width and a maximum. */
export function tileWidthFor(nodes, m, labels) {
  const { s } = m;
  let need = 0;
  for (const n of nodes) {
    need = Math.max(need, textWidth(cardName(n, labels), 11 * s, 700), textWidth(n.role, 9.5 * s, 400));
  }
  return Math.min(260 * s, Math.max(172 * s, Math.ceil(need + 41 * s)));
}

function tileLines(node, w, s, labels) {
  const area = w - 41 * s;
  const nameLines = wrapLines(cardName(node, labels), area, 11 * s, 700);
  const roleLines = wrapLines(node.role, area, 9.5 * s, 400);
  return { nameLines, roleLines, h: Math.max(36 * s, Math.ceil(14 * s + nameLines.length * 13.5 * s + roleLines.length * 12 * s)) };
}

export function unitStats(unit, labels) {
  const stats = [labels.people(unit.people)];
  if (unit.vacancies > 0) stats.push(labels.vacancies(unit.vacancies));
  if (unit.span != null) stats.push(labels.span(unit.span));
  return stats.join(' · ');
}

/**
 * Size of a unit frame: wide enough for its title and numbers (wrapped above a maximum), head row
 * on top, member tiles in a grid below — tiles as tall as their longest wrapped text needs.
 */
export function frameSize(unit, memberCount, m, labels) {
  const { s } = m;
  const room = m.editRoom ?? 0;
  const max = m.gridMax ?? GRID_MAX;
  const shownMembers = unit.members.slice(0, max);
  const shown = max === 0 ? 0 : shownMembers.length + (memberCount > max ? 1 : 0);
  const columns = shown === 0 ? 1 : Math.min(m.gridCols ?? 4, Math.max(1, Math.ceil(Math.sqrt(shown * 0.9))));
  const rows = Math.ceil(shown / columns);
  const gridW = columns * m.tileW + (columns - 1) * 8 * s;
  const stats = unitStats(unit, labels);
  const w = Math.max(
    240 * s, gridW + 24 * s,
    Math.min(440 * s, textWidth(unit.name, 14 * s, 800) + 28 * s + room),
    Math.min(440 * s, textWidth(stats, 10.5 * s, 600) + 28 * s),
  );
  const titleLines = wrapLines(unit.name, w - 28 * s - room, 14 * s, 800);
  const statsLines = wrapLines(stats, w - 28 * s, 10.5 * s, 600);
  const top = 58 * s + (titleLines.length - 1) * 17 * s + (statsLines.length - 1) * 13 * s;
  const tileH = Math.max(36 * s, ...shownMembers.map((n) => tileLines(n, m.tileW, s, labels).h));
  const headH = unit.head ? tileLines(unit.head, w - 24 * s, s, labels).h + 2 * s : 0;
  const h = top + (unit.head ? headH + 8 * s : 4 * s) + (rows ? rows * (tileH + 6 * s) + 8 * s : 6 * s);
  return { w, h, columns, shown, max, tileH, headH, top, titleLines, statsLines };
}

function tileRow(node, x, y, w, h, s, ctx, head) {
  const selected = ctx.selectedId === node.id;
  const label = node.vacant ? `${ctx.labels.vacancy}, ${node.role}` : `${node.name}, ${node.role}`;
  const { nameLines, roleLines } = tileLines(node, w, s, ctx.labels);
  const top = (h - (nameLines.length * 13.5 + roleLines.length * 12) * s) / 2;
  const text = nameLines.map((t, i) => `<text class="ot-name" x="${num(31 * s)}" y="${num(top + (11 + i * 13.5) * s)}" font-size="${num(11 * s)}">${esc(t)}</text>`).join('')
    + roleLines.map((t, i) => `<text class="ot-role" x="${num(31 * s)}" y="${num(top + (nameLines.length * 13.5 + 10 + i * 12) * s)}" font-size="${num(9.5 * s)}">${esc(t)}</text>`).join('');
  return `<g class="ot-tile${head ? ' ot-head' : ''}${node.vacant ? ' ot-vacant' : ''}${selected ? ' ot-selected' : ''}${ctx.pathSet?.has(node.id) ? ' ot-path' : ''}${ctx.matchSet?.has(node.id) ? ' ot-match' : ''}" id="${ctx.uid}-${esc(node.id)}" data-node="${esc(node.id)}" role="treeitem" aria-level="${ctx.level + 1}" aria-selected="${selected}" aria-label="${esc(label)}" transform="translate(${num(x)} ${num(y)})">`
    + `<title>${esc(label)}</title>`
    + `<rect class="ot-tile-bg" width="${num(w)}" height="${num(h)}" rx="${num(8 * s)}"/>`
    + avatar(node, 15 * s, h / 2, 10.5 * s, s, 8.5 * s)
    + text
    + '</g>';
}

/**
 * A unit as a frame: coloured band, name, numbers, head on top, team below.
 * `unit` carries `members` (nodes, head excluded) and `head` (node or null).
 * ctx adds `unitSelected`.
 */
export function frameSvg(unit, item, ctx, size) {
  const { s, labels } = ctx;
  const { w, h } = item;
  const stats = unitStats(unit, labels);
  const cls = ['ot-frame', ctx.unitSelected && 'ot-selected', ctx.pathSet?.has(`unit:${unit.id}`) && 'ot-path',
    unit.mark && `ot-mark-${unit.mark}`].filter(Boolean).join(' ');
  const expandAttr = ctx.expandable ? ` aria-expanded="${ctx.expanded}"` : '';
  const label = `${unit.name}, ${stats}`;
  const titleAt = (i) => 28 * s + i * 17 * s;
  const statsBase = 46 * s + (size.titleLines.length - 1) * 17 * s;
  let body = `<g class="${cls}" id="${ctx.uid}-unit-${esc(unit.id)}" data-unit="${esc(unit.id)}" role="treeitem" aria-level="${ctx.level}" aria-selected="${ctx.unitSelected}"${expandAttr} aria-label="${esc(label)}" transform="translate(${num(item.x)} ${num(item.y)})">`
    + `<title>${esc(label)}</title>`
    + `<rect class="ot-frame-bg" width="${num(w)}" height="${num(h)}" rx="${num(14 * s)}"/>`
    + `<rect class="ot-frame-band" width="${num(w)}" height="${num(6 * s)}" rx="${num(3 * s)}" fill="${esc(unit.color)}"/>`
    + (ctx.editing && !ctx.lod ? menuHandle(unit.id, 'unit', w - 19 * s, 24 * s, s, labels.menu(unit.name)) : '')
    + size.titleLines.map((t, i) => `<text class="ot-frame-title" x="${num(14 * s)}" y="${num(titleAt(i))}" font-size="${num(14 * s)}">${esc(t)}</text>`).join('')
    + size.statsLines.map((t, i) => `<text class="ot-frame-stats" x="${num(14 * s)}" y="${num(statsBase + i * 13 * s)}" font-size="${num(10.5 * s)}">${esc(t)}</text>`).join('');
  if (ctx.lod) return `${body}</g>`;
  if (ctx.expandable && ctx.expanded) {
    body += pill('−', w - 10 * s, h - 9 * s, s, 'ot-count', ` data-toggle="unit:${esc(unit.id)}"`).svg;
  }
  let y = size.top;
  if (unit.head) {
    body += tileRow(unit.head, 12 * s, y, w - 24 * s, size.headH, s, ctx, true);
    y += size.headH + 8 * s;
  }
  unit.members.slice(0, size.max).forEach((member, i) => {
    const col = i % size.columns;
    const row = Math.floor(i / size.columns);
    body += tileRow(member, 12 * s + col * (ctx.tileW + 8 * s), y + row * (size.tileH + 6 * s), ctx.tileW, size.tileH, s, ctx, false);
  });
  if (size.max > 0 && unit.members.length > size.max) {
    const i = size.max;
    const col = i % size.columns;
    const row = Math.floor(i / size.columns);
    body += `<text class="ot-frame-more" x="${num(12 * s + col * (ctx.tileW + 8 * s) + 8 * s)}" y="${num(y + row * (size.tileH + 6 * s) + size.tileH / 2 + 4 * s)}" font-size="${num(10.5 * s)}">+${unit.members.length - size.max}</text>`;
  }
  return `${body}</g>`;
}

// ---------------------------------------------------------------------------
// Connectors
// ---------------------------------------------------------------------------

function elbow(fromX, fromY, toX, toY, busY, r) {
  if (Math.abs(toX - fromX) < 1) return `M${num(fromX)} ${num(fromY)}V${num(toY)}`;
  const dir = toX > fromX ? 1 : -1;
  const radius = Math.min(r, Math.abs(toX - fromX) / 2, Math.max(1, (toY - fromY) / 2));
  return `M${num(fromX)} ${num(fromY)}V${num(busY - radius)}Q${num(fromX)} ${num(busY)} ${num(fromX + dir * radius)} ${num(busY)}`
    + `H${num(toX - dir * radius)}Q${num(toX)} ${num(busY)} ${num(toX)} ${num(busY + radius)}V${num(toY)}`;
}

function elbowH(fromX, fromY, toX, toY, busX, r) {
  if (Math.abs(toY - fromY) < 1) return `M${num(fromX)} ${num(fromY)}H${num(toX)}`;
  const dir = toY > fromY ? 1 : -1;
  const radius = Math.min(r, Math.abs(toY - fromY) / 2, Math.max(1, (toX - fromX) / 2));
  return `M${num(fromX)} ${num(fromY)}H${num(busX - radius)}Q${num(busX)} ${num(fromY)} ${num(busX)} ${num(fromY + dir * radius)}`
    + `V${num(toY - dir * radius)}Q${num(busX)} ${num(toY)} ${num(busX + radius)} ${num(toY)}H${num(toX)}`;
}

/**
 * Every connector of a layout: `{ d, childId, parentId, kind: 'line'|'staff', box }`.
 * `toId` maps an item id to the id the path highlight and the culling use.
 */
export function buildEdges(layout, s, horizontal = false) {
  const edges = [];
  const push = (d, childId, parentId, kind, a, b) => {
    edges.push({
      d, childId, parentId, kind,
      x0: Math.min(a.x, b.x), y0: Math.min(a.y, b.y), x1: Math.max(a.x, b.x), y1: Math.max(a.y, b.y),
    });
  };
  const byId = layout.byId;
  const stackTop = new Map();
  for (const item of layout.items) {
    if (!item.parentId) continue;
    const parent = byId.get(item.parentId);
    if (horizontal) {
      // Turned on its side: staff hang below their manager, reports branch off its right edge.
      if (item.role === 'staff') {
        const x = parent.x + parent.w / 2;
        push(`M${num(x)} ${num(parent.y + parent.h)}V${num(item.y)}`, item.id, parent.id, 'staff',
          { x, y: parent.y + parent.h }, { x, y: item.y });
      } else {
        const fromY = parent.y + parent.h / 2;
        const toY = item.y + item.h / 2;
        push(elbowH(parent.x + parent.w, fromY, item.x, toY, item.x - 14 * s, CORNER * s), item.id, parent.id, 'line',
          { x: parent.x + parent.w, y: fromY }, { x: item.x, y: toY });
      }
      continue;
    }
    if (item.role === 'staff') {
      const y = parent.y + parent.h / 2;
      push(`M${num(parent.x + parent.w)} ${num(y)}H${num(item.x)}`, item.id, parent.id, 'staff',
        { x: parent.x + parent.w, y }, { x: item.x, y });
    } else if (item.role === 'stacked') {
      const spineX = item.x - 14 * s;
      const midY = item.y + item.h / 2;
      const key = `${parent.id}:${spineX}`;
      if (!stackTop.has(key)) stackTop.set(key, item.y);
      const busY = stackTop.get(key) - 14 * s;
      const fromX = parent.x + 16 * s;
      const trunk = `M${num(fromX)} ${num(parent.y + parent.h)}`;
      // The first column hangs straight off the manager's card; further columns branch off a bus above them.
      const start = Math.abs(spineX - fromX) < 0.5
        ? `${trunk}V${num(midY - CORNER * s)}`
        : `${trunk}V${num(busY)}H${num(spineX)}V${num(midY - CORNER * s)}`;
      push(`${start}Q${num(spineX)} ${num(midY)} ${num(spineX + CORNER * s)} ${num(midY)}H${num(item.x)}`,
        item.id, parent.id, 'line', { x: Math.min(fromX, spineX), y: parent.y + parent.h }, { x: item.x, y: midY });
    } else {
      const px = parent.x + parent.w / 2;
      const cx = item.x + item.w / 2;
      const fromY = parent.y + parent.h;
      const busY = item.y - 14 * s;
      push(elbow(px, fromY, cx, item.y, busY, CORNER * s), item.id, parent.id, 'line',
        { x: px, y: fromY }, { x: cx, y: item.y });
    }
  }
  return edges;
}

/** Dotted functional (matrix) lines between two laid-out boxes; both ends must be on screen. */
export function functionalEdge(child, parent, s, index, horizontal = false) {
  if (horizontal) {
    const sx = parent.x + parent.w;
    const sy = parent.y + parent.h / 2;
    const tx = child.x;
    const ty = child.y + child.h / 2;
    const reach = Math.max(30 * s, (tx - sx) / 2) + (index % 4) * 4 * s;
    return `M${num(sx)} ${num(sy)}C${num(sx + reach)} ${num(sy)} ${num(tx - reach)} ${num(ty)} ${num(tx)} ${num(ty)}`;
  }
  const cx = child.x + child.w / 2;
  const px = parent.x + parent.w / 2;
  const fromY = parent.y + parent.h;
  const spread = (index % 4) * 4 * s;
  if (child.y - fromY >= 24 * s) {
    return elbow(px, fromY, cx, child.y, child.y - 6 * s - spread, CORNER * s);
  }
  const sx = child.x + child.w;
  const sy = child.y + child.h / 2;
  const tx = parent.x + parent.w;
  const ty = parent.y + parent.h / 2;
  const bend = 40 * s + spread;
  return `M${num(sx)} ${num(sy)}C${num(sx + bend)} ${num(sy)} ${num(tx + bend)} ${num(ty)} ${num(tx)} ${num(ty)}`;
}

// ---------------------------------------------------------------------------
// Standalone export stylesheet
// ---------------------------------------------------------------------------

export const EXPORT_PALETTES = {
  light: {
    bg: '#ffffff', card: '#ffffff', cardStroke: '#cbd5e1', text: '#0f172a', text2: '#475569', text3: '#64748b',
    line: '#94a3b8', hl: '#6366f1', staff: '#8b5cf6', functional: '#f59e0b', pill: '#eef2ff', pillText: '#3730a3',
    avatar: '#e0e7ff', avatarText: '#3730a3', frame: '#f8fafc',
  },
  dark: {
    bg: '#0a0d24', card: '#131735', cardStroke: '#2a2f5a', text: '#f1f5f9', text2: '#94a3b8', text3: '#64748b',
    line: '#3b4270', hl: '#818cf8', staff: '#a78bfa', functional: '#f59e0b', pill: '#1c2150', pillText: '#c7d2fe',
    avatar: '#312e81', avatarText: '#c7d2fe', frame: '#0f1330',
  },
};

/** CSS embedded in an exported SVG; literal colours, because a file on disk has no theme tokens. */
export function exportStyle(theme) {
  const c = EXPORT_PALETTES[theme] ?? EXPORT_PALETTES.light;
  return `text{font-family:Manrope,Inter,system-ui,-apple-system,Segoe UI,sans-serif}`
    + `.ot-card-bg,.ot-tile-bg{fill:${c.card};stroke:${c.cardStroke};stroke-width:1}`
    + `.ot-vacant .ot-card-bg,.ot-vacant .ot-tile-bg{fill:none;stroke-dasharray:5 4}`
    + `.ot-staff .ot-card-bg{stroke:${c.staff}}`
    + `.ot-frame-bg{fill:${c.frame};stroke:${c.cardStroke};stroke-width:1}`
    + `.ot-frame-title{fill:${c.text};font-weight:800}.ot-frame-stats,.ot-frame-more{fill:${c.text2}}`
    + `.ot-name{fill:${c.text};font-weight:700}.ot-role{fill:${c.text2}}.ot-vacant .ot-name{fill:${c.text3};font-style:italic}`
    + `.ot-av{fill:${c.avatar}}.ot-av-text{fill:${c.avatarText};font-weight:800}`
    + `.ot-av-vac{fill:none;stroke:${c.text3};stroke-dasharray:3 3}.ot-glyph{fill:none;stroke:${c.text3};stroke-width:2}`
    + `.ot-tag rect{fill:${c.pill};stroke:${c.cardStroke}}.ot-tag text{fill:${c.pillText};font-weight:800}`
    + `.ot-lines path{fill:none;stroke:${c.line};stroke-width:1.5}`
    + `.ot-lines path.hl{stroke:${c.hl};stroke-width:2.5}.ot-lines path.staff{stroke:${c.staff}}`
    + `.ot-functional path{fill:none;stroke:${c.functional};stroke-width:1.5;stroke-dasharray:2 5;stroke-linecap:round}`
    + `.ot-more rect{fill:${c.pill};stroke:${c.cardStroke}}.ot-more text{fill:${c.pillText};font-weight:800}`;
}

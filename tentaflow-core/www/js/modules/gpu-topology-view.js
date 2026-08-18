// =============================================================================
// File: gpu-topology-view.js — GPU pair/topology helpers shared by the deploy
// wizard GPU step and the mesh node detail (fast-link groups, pair chips,
// selection link stats, NxN link matrix). Reads `node.gpu_links`
// [{a, b, link, p2p_ok}] where a<b index `node.gpus`.
// =============================================================================

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-tooltip.js';

// Higher rank = faster interconnect. NODE/SYS cross the CPU and never form a group.
const FAST_RANK = { NVL: 4, PIX: 3, PXB: 2, PHB: 1 };
const GROUP_LETTERS = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ';
const PALETTE_SIZE = 6;

// "0000:82:00.0" -> "82:00" (domain and function carry no signal for humans).
export function shortPciBusId(id) {
  return String(id || '').replace(/^[0-9a-f]+:/i, '').replace(/\.[0-9a-f]+$/i, '').toLowerCase();
}

function normLink(link) {
  const l = String(link || 'UNKNOWN').toUpperCase();
  return l in FAST_RANK || l === 'NODE' || l === 'SYS' ? l : 'UNKNOWN';
}

export function linkLabel(link) {
  const l = normLink(link);
  return l === 'NVL' ? 'NVLink' : l;
}

function pairKey(a, b) {
  return a < b ? `${a}-${b}` : `${b}-${a}`;
}

// Map "a-b" -> {link, p2pOk} for every declared link.
export function linkMap(gpuLinks) {
  const map = new Map();
  for (const l of Array.isArray(gpuLinks) ? gpuLinks : []) {
    const a = Number(l?.a);
    const b = Number(l?.b);
    if (!Number.isInteger(a) || !Number.isInteger(b) || a === b) continue;
    map.set(pairKey(a, b), { link: normLink(l.link), p2pOk: l.p2p_ok !== false });
  }
  return map;
}

// Connected components over fast links. Returns { hasLinks, groups, byGpu }
// where groups = [{ letter, color, members, link }] (link = weakest fast link
// inside the group, so the chip never overstates the interconnect).
export function computeGpuGroups(gpuCount, gpuLinks) {
  const links = linkMap(gpuLinks);
  const byGpu = new Map();
  const groups = [];
  if (links.size === 0 || gpuCount < 2) return { hasLinks: false, groups, byGpu };

  const parent = Array.from({ length: gpuCount }, (_, i) => i);
  const find = (i) => (parent[i] === i ? i : (parent[i] = find(parent[i])));
  for (const [key, info] of links) {
    if (!(info.link in FAST_RANK)) continue;
    const [a, b] = key.split('-').map(Number);
    if (a >= gpuCount || b >= gpuCount) continue;
    parent[find(a)] = find(b);
  }

  const members = new Map();
  for (let i = 0; i < gpuCount; i++) {
    const root = find(i);
    if (!members.has(root)) members.set(root, []);
    members.get(root).push(i);
  }
  for (const list of members.values()) {
    if (list.length < 2) continue;
    let weakest = null;
    for (let i = 0; i < list.length; i++) {
      for (let j = i + 1; j < list.length; j++) {
        const info = links.get(pairKey(list[i], list[j]));
        if (!info || !(info.link in FAST_RANK)) continue;
        if (weakest === null || FAST_RANK[info.link] < FAST_RANK[weakest]) weakest = info.link;
      }
    }
    const n = groups.length;
    const group = {
      letter: GROUP_LETTERS[n % GROUP_LETTERS.length],
      color: n % PALETTE_SIZE,
      members: list,
      link: weakest || 'PHB',
    };
    groups.push(group);
    for (const idx of list) byGpu.set(idx, group);
  }
  return { hasLinks: true, groups, byGpu };
}

// Pair chip for one GPU row/card. Empty string when the node has no links.
export function gpuPairChipHtml(idx, topology) {
  if (!topology?.hasLinks) return '';
  const group = topology.byGpu.get(idx);
  if (!group) {
    return `<tf-tooltip text="${escapeAttr(I18n.t('gpu_topology.no_pair_tip'))}"><tf-chip status="neutral" class="gpu-pair-chip">${escapeHtml(I18n.t('gpu_topology.no_pair'))}</tf-chip></tf-tooltip>`;
  }
  const label = I18n.t('gpu_topology.pair', { letter: group.letter, link: linkLabel(group.link) });
  return `<tf-chip status="neutral" class="gpu-pair-chip" data-gpu-group="${group.color}">${escapeHtml(label)}</tf-chip>`;
}

export function gpuTopologyLegendHtml() {
  return `<div class="gpu-topo-legend">${escapeHtml(I18n.t('gpu_topology.legend'))}</div>`;
}

// Fast vs CPU-crossing links among the selected GPU indices, plus P2P failures.
export function selectionLinkStats(gpuLinks, selectedIdxs) {
  const links = linkMap(gpuLinks);
  const sel = Array.from(new Set((selectedIdxs || []).map(Number))).filter(Number.isInteger).sort((a, b) => a - b);
  let fast = 0;
  let slow = 0;
  let noP2p = 0;
  for (let i = 0; i < sel.length; i++) {
    for (let j = i + 1; j < sel.length; j++) {
      const info = links.get(pairKey(sel[i], sel[j]));
      if (!info) continue;
      if (info.link in FAST_RANK) fast++; else slow++;
      if (!info.p2pOk) noP2p++;
    }
  }
  return { fast, slow, noP2p, pairs: fast + slow };
}

export function selectionLinkHintHtml(gpuLinks, selectedIdxs) {
  const stats = selectionLinkStats(gpuLinks, selectedIdxs);
  if (stats.pairs === 0) return '';
  const text = I18n.t('gpu_topology.selection_hint', { fast: stats.fast, slow: stats.slow });
  const warn = stats.noP2p > 0
    ? ` <tf-chip status="warn">${escapeHtml(I18n.t('gpu_topology.no_p2p'))}</tf-chip>`
    : '';
  return `<div class="gpu-topo-hint">${escapeHtml(text)}${warn}</div>`;
}

// NxN matrix of link classes; empty string below two GPUs or without links.
export function gpuTopologyMatrixHtml(gpuCount, gpuLinks) {
  const links = linkMap(gpuLinks);
  if (gpuCount < 2 || links.size === 0) return '';
  const head = Array.from({ length: gpuCount }, (_, i) => `<div class="gpu-topo-head">GPU${i}</div>`).join('');
  const rows = [];
  for (let r = 0; r < gpuCount; r++) {
    const cells = [`<div class="gpu-topo-head">GPU${r}</div>`];
    for (let c = 0; c < gpuCount; c++) {
      if (r === c) { cells.push('<div class="gpu-topo-cell self">—</div>'); continue; }
      const info = links.get(pairKey(r, c));
      const link = info ? info.link : 'UNKNOWN';
      const tip = I18n.t('gpu_topology.cell_tip', {
        a: Math.min(r, c), b: Math.max(r, c), link: linkLabel(link),
        p2p: I18n.t(info && info.p2pOk ? 'gpu_topology.p2p_yes' : 'gpu_topology.p2p_no'),
      });
      cells.push(`<tf-tooltip text="${escapeAttr(tip)}"><div class="gpu-topo-cell link-${link.toLowerCase()}${info && !info.p2pOk ? ' no-p2p' : ''}">${escapeHtml(link)}</div></tf-tooltip>`);
    }
    rows.push(cells.join(''));
  }
  return `
    <div class="gpu-topo-matrix" style="--gpu-topo-n:${gpuCount};">
      <div class="gpu-topo-head"></div>${head}${rows.join('')}
    </div>
    ${gpuTopologyLegendHtml()}
  `;
}

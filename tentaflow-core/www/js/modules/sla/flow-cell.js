// ===== File: modules/sla/flow-cell.js — the "Polityka SLA" cell of the flow list: drawn once, values painted in place =====
//
// The flow list polls. A poll must not rebuild the cell: its chip, its detail
// line and anything selected in it would blink. So the cell markup carries only
// what does not change between polls (the policy name, the kind of cell) and
// `paintSlaCells` writes the state, the label and the detail line into the nodes
// that are already there. The markup is the one of the accepted mockup (2a).

import { escapeAttr, escapeHtml } from '/js/utils.js';
import { setAttr, setText } from '/js/lib/dom-patch.js';
import '/js/components/tf-chip.js';
import { T } from './model.js';

const COLUMN_STYLE = 'display:flex;flex-direction:column;gap:4px;align-items:flex-start';

/** The markup of one cell for `view` (see `slaView`); `flowId` ties the nodes to the poll that paints them. */
export function slaCellHtml(flowId, view) {
  if (view.kind === 'system') {
    return `<span style="font-size:12px;color:var(--text-3)" title="${escapeAttr(T('cell.system_title'))}">${escapeHtml(T('cell.system'))}</span>`;
  }
  if (view.kind === 'none') return '<span style="color:var(--text-3)">—</span>';
  const nameStyle = view.kind === 'elsewhere' ? 'font-size:13px;color:var(--text-2)' : 'font-size:13px';
  return `<div data-sla-flow="${escapeAttr(flowId)}" data-sla-kind="${view.kind}" style="${COLUMN_STYLE}">`
    + `<span style="${nameStyle}">${escapeHtml(view.name)}</span>`
    + '<tf-chip data-sla-chip></tf-chip>'
    + '<span data-sla-detail style="font-size:11px;color:var(--text-3)"></span></div>';
}

/**
 * Writes the live values into the cells found under `root` (a table's shadow
 * root, or any element). `views` is a Map of flow id → view; a cell whose kind
 * no longer matches its view is left for the next full draw.
 */
export function paintSlaCells(root, views) {
  for (const box of root.querySelectorAll('[data-sla-flow]')) {
    const view = views.get(box.dataset.slaFlow);
    if (!view || view.kind !== box.dataset.slaKind) continue;
    const chip = box.querySelector('[data-sla-chip]');
    setAttr(chip, 'status', view.chipStatus);
    setAttr(chip, 'label', view.label);
    setAttr(chip, 'title', view.detail);
    setText(box.querySelector('[data-sla-detail]'), view.detail);
  }
}

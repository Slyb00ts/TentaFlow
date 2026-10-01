// =============================================================================
// File: modules/org-structure/history-compare.js
// Description: "Przed i po" of the Historia tab (mockup F04): one unit of the
//   structure drawn on two days side by side — today and a planned day, or the
//   live structure and the same day with a reorganization applied — with the
//   differences marked on the cards (new, changed, removed). The differences and
//   the two views come from the server; this draws them. The charts are the
//   ordinary tf-org-tree, created once and only given a new model.
// =============================================================================

import { I18n } from '/js/i18n.js';
import { escapeAttr, escapeHtml } from '/js/utils.js';
import '/js/components/tf-org-tree.js';
import '/js/components/tf-select.js';
import '/js/components/tf-chip.js';
import { buildTreeModel } from '/js/modules/org-structure/tree.js';
import { treeLabels } from '/js/modules/org-structure/tree-tab.js';
import { summarize } from '/js/modules/org-structure/model.js';
import {
  decorateModel, diffMarks, subtreeUnitIds, unitChoices, viewSubset,
} from '/js/modules/org-structure/history-model.js';

const t = (key, params) => I18n.t(`org_structure.history.${key}`, params);
const ot = (key, params) => I18n.t(`org_structure.${key}`, params);

// A subtree of this many positions is drawn card by card; a bigger one as units, the way the chart does for a large structure.
const PERSON_CARDS_LIMIT = 40;

/**
 * @param {HTMLElement} host the section the panel fills
 * @returns {{ show(comparison: object | null): void, dispose(): void }}
 *   a comparison is `{ before: { view, label }, after: { view, label, planned }, items, unitTypes }`
 */
export function createComparePanel(host) {
  let comparison = null;
  let unitId = '';
  let charts = null;

  host.innerHTML = `
    <div class="org-hist-card-head">
      <div class="org-hist-card-title" data-role="title"></div>
      <span class="org-hist-hint" data-role="hint"></span>
      <span class="org-hist-grow"></span>
      <tf-select class="org-hist-unit-select" data-role="unit" label="${escapeAttr(t('compare_unit'))}"></tf-select>
    </div>
    <div class="org-hist-empty" data-role="empty"></div>
    <div class="org-hist-compare" data-role="grid" hidden>
      <div class="org-hist-compare-side">
        <div class="org-hist-compare-head"><span data-role="before-label"></span><tf-chip variant="outline" data-role="before-people"></tf-chip></div>
        <div class="org-hist-tree-box" data-role="before-tree"></div>
      </div>
      <div class="org-hist-compare-arrow" aria-hidden="true">→</div>
      <div class="org-hist-compare-side">
        <div class="org-hist-compare-head"><span data-role="after-label"></span><tf-chip variant="outline" data-role="after-people"></tf-chip></div>
        <div class="org-hist-tree-box" data-role="after-tree"></div>
      </div>
    </div>
    <div class="org-hist-legend" data-role="legend" hidden>
      <span><i class="org-hist-swatch is-added"></i>${escapeHtml(t('legend_new'))}</span>
      <span><i class="org-hist-swatch is-changed"></i>${escapeHtml(t('legend_changed'))}</span>
      <span><i class="org-hist-swatch is-removed"></i>${escapeHtml(t('legend_removed'))}</span>
    </div>`;
  const q = (role) => host.querySelector(`[data-role="${role}"]`);

  const onUnit = (e) => {
    unitId = String(e.detail?.value ?? '');
    draw();
  };
  q('unit').addEventListener('change', onUnit);

  function chart(role) {
    const el = document.createElement('tf-org-tree');
    el.classList.add('org-hist-tree');
    q(role).replaceChildren(el);
    el.labels = treeLabels();
    return el;
  }

  function draw() {
    const empty = q('empty');
    const grid = q('grid');
    if (!comparison) {
      grid.hidden = true;
      q('legend').hidden = true;
      q('title').textContent = t('compare_title_plain');
      q('hint').textContent = '';
      q('unit').hidden = true;
      empty.hidden = false;
      empty.textContent = t('compare_pick');
      return;
    }
    const { before, after, items, unitTypes } = comparison;
    const choices = unitChoices(before.view, after.view, items);
    // A unit both days know shows a real "before"; a unit the reorganization creates has nothing to show on that side.
    const existedBefore = (c) => (before.view.units ?? []).some((u) => u.unit_id === c.id);
    if (!choices.some((c) => c.id === unitId)) unitId = (choices.find(existedBefore) ?? choices[0])?.id ?? '';
    const unitSelect = q('unit');
    unitSelect.hidden = choices.length === 0;
    unitSelect.setOptions(
      choices.map((c) => ({ value: c.id, label: c.count ? `${c.name} (${c.count})` : c.name })),
      unitId,
    );
    const unitName = choices.find((c) => c.id === unitId)?.name ?? '';
    q('title').textContent = t('compare_title', { unit: unitName });
    q('hint').textContent = t('compare_hint', { before: before.label, after: after.label });

    const within = new Set([...subtreeUnitIds(before.view, unitId), ...subtreeUnitIds(after.view, unitId)]);
    const inUnit = items.filter((item) => !unitId || within.has(item.unit_id));
    const scoped = { before: viewSubset(before.view, unitId), after: viewSubset(after.view, unitId) };
    const hasStructure = scoped.before.positions.length + scoped.after.positions.length > 0;
    const note = !hasStructure || inUnit.length === 0;
    empty.hidden = !note;
    empty.textContent = note ? t('compare_none') : '';
    grid.hidden = !hasStructure;
    q('legend').hidden = !hasStructure;
    if (!hasStructure) return;

    // The marks come from the whole comparison, not from the unit's part: a position moved in from another unit is still new here.
    const marks = diffMarks(items);
    const big = Math.max(scoped.before.positions.length, scoped.after.positions.length) > PERSON_CARDS_LIMIT;
    if (!charts || !q('before-tree').contains(charts.before)) {
      charts = { before: chart('before-tree'), after: chart('after-tree') };
    }
    for (const side of ['before', 'after']) {
      const model = buildTreeModel(scoped[side], { unitTypes, t: ot });
      decorateModel(model, marks, side);
      charts[side].mode = big ? 'units' : 'persons';
      charts[side].model = model;
      q(`${side}-label`).textContent = side === 'before' ? before.label : after.label;
      q(`${side}-people`).textContent = t('compare_people', { count: summarize(scoped[side]).people });
    }
  }

  draw();
  return {
    /** Shows a comparison (see the constructor); `null` shows the prompt to pick one. */
    show(next) {
      comparison = next;
      draw();
    },
    dispose() {
      q('unit').removeEventListener('change', onUnit);
      host.replaceChildren();
      charts = null;
    },
  };
}

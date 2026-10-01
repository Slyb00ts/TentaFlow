// =============================================================================
// File: modules/org-structure/edit-people.js
// Description: The "Osoby bez stanowiska" strip of the edit mode: the accounts
//   nobody holds a position for, each a chip that can be dragged onto a vacancy
//   of the chart. The chart cannot know about drags that start outside it, so
//   this module drives the same outline through `chart.setDropHover` and asks
//   `chart.nodeAt` what lies under the pointer. A chip also works from the
//   keyboard (Enter picks the person, and the caller offers a vacancy to fill).
// =============================================================================

import { escapeAttr, escapeHtml } from '/js/utils.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-avatar.js';
import { vacancyDropCheck } from '/js/modules/org-structure/edit-rules.js';
import { initialsOf } from '/js/modules/org-structure/tree.js';

const DRAG_SLOP = 4;

/** Accounts of the organization as the strip and the assign window use them. */
export function normalizeUsers(list) {
  return (Array.isArray(list) ? list : []).map((u) => ({
    id: String(u.id),
    name: u.displayName || u.display_name || u.username,
    email: u.email || '',
    isActive: (u.isActive ?? u.is_active) !== false,
  }));
}

/** Role catalog entries as `{ id, name }` in the interface language, falling back to the first translation. */
export function normalizeRoles(list, language) {
  return (Array.isArray(list) ? list : []).map((role) => {
    const pairs = role.nameTranslations ?? role.name_translations ?? [];
    const exact = pairs.find((pair) => Array.isArray(pair) && pair[0] === language);
    const name = (exact ?? pairs.find(Array.isArray))?.[1] || role.slug;
    return { id: String(role.id), name };
  });
}

/** Draws the chips into `host`; `labels` = `{ title(count), hint, empty }`. */
export function renderStrip(host, people, labels) {
  const chips = people.map((p) => (
    `<tf-chip variant="outline" class="org-person-chip" role="button" tabindex="0" data-user="${escapeAttr(p.id)}" title="${escapeAttr(labels.chipHint)}">`
    + `<tf-avatar slot="lead" size="sm" initials="${escapeAttr(initialsOf(p.name))}"></tf-avatar>${escapeHtml(p.name)}</tf-chip>`
  )).join('');
  host.innerHTML = `<span class="org-people-title">${escapeHtml(labels.title(people.length))}</span>`
    + `<div class="org-people-chips">${chips || `<span class="org-people-empty">${escapeHtml(labels.empty)}</span>`}</div>`
    + `<span class="org-people-hint">${escapeHtml(labels.hint)}</span>`;
}

/**
 * Makes the chips in `host` draggable onto the chart. `ctx`: `{ chart, model(), name(userId), onDrop(userId, positionId),
 * onReject(reason), onPick(userId, chip) }`. Returns a function that removes every listener.
 */
export function attachPersonDrag(host, ctx) {
  let drag = null;

  const cleanup = () => {
    if (!drag) return;
    drag.ghost?.remove();
    ctx.chart.setDropHover(null);
    drag = null;
  };

  const onDown = (e) => {
    const chip = e.target.closest?.('.org-person-chip');
    if (!chip || (e.pointerType === 'mouse' && e.button !== 0)) return;
    drag = { id: chip.dataset.user, chip, sx: e.clientX, sy: e.clientY, moved: false, ghost: null, pointerId: e.pointerId };
    chip.setPointerCapture?.(e.pointerId);
  };

  const hitOf = (e) => {
    const hit = ctx.chart.nodeAt(e.clientX, e.clientY, 'position');
    return { hit, verdict: hit ? vacancyDropCheck(ctx.model(), hit.id) : null };
  };

  const onMove = (e) => {
    if (!drag || e.pointerId !== drag.pointerId) return;
    if (!drag.moved && Math.hypot(e.clientX - drag.sx, e.clientY - drag.sy) > DRAG_SLOP) {
      drag.moved = true;
      drag.ghost = document.createElement('div');
      drag.ghost.className = 'org-person-ghost';
      drag.ghost.textContent = ctx.name(drag.id);
      document.body.appendChild(drag.ghost);
    }
    if (!drag.moved) return;
    drag.ghost.style.transform = `translate(${Math.round(e.clientX + 14)}px, ${Math.round(e.clientY + 14)}px)`;
    const { hit, verdict } = hitOf(e);
    drag.ghost.dataset.state = verdict ? (verdict.ok ? 'ok' : 'bad') : 'none';
    ctx.chart.setDropHover(hit, verdict ? verdict.ok : null);
  };

  const onUp = (e) => {
    if (!drag || e.pointerId !== drag.pointerId) return;
    const finished = drag;
    const { hit, verdict } = finished.moved ? hitOf(e) : { hit: null, verdict: null };
    cleanup();
    if (!finished.moved) {
      ctx.onPick(finished.id, finished.chip);
    } else if (hit && verdict.ok) {
      ctx.onDrop(finished.id, hit.id);
    } else if (hit) {
      ctx.onReject(verdict.reason);
    }
  };

  const onKey = (e) => {
    if (e.key === 'Escape' && drag) cleanup();
    else if (e.key === 'Enter' && host.contains(e.target) && e.target.closest?.('.org-person-chip')) {
      const chip = e.target.closest('.org-person-chip');
      ctx.onPick(chip.dataset.user, chip);
    }
  };

  host.addEventListener('pointerdown', onDown);
  host.addEventListener('pointermove', onMove);
  host.addEventListener('pointerup', onUp);
  host.addEventListener('pointercancel', cleanup);
  document.addEventListener('keydown', onKey);
  return () => {
    cleanup();
    host.removeEventListener('pointerdown', onDown);
    host.removeEventListener('pointermove', onMove);
    host.removeEventListener('pointerup', onUp);
    host.removeEventListener('pointercancel', cleanup);
    document.removeEventListener('keydown', onKey);
  };
}

// ===== File: modules/tentanas/dialogs.js — TentaNas dialog helpers: the retype-window preset, the job follow-up, danger rows and path crumbs =====
//
// The retype-to-confirm window itself lives in lib/retype-dialog.js (shared
// with TentaBus); this file holds what is TentaNas-specific around it.

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { T, sprite, errMessage } from '/js/modules/tentanas/format.js';
import { reportParked } from '/js/modules/tentanas/approvals.js';
import '/js/components/tf-button.js';
import '/js/components/tf-breadcrumb.js';

/** TentaNas windows are scoped by `.nas-modal` and state errors in the screen's own wording. */
export const NAS_DIALOG = { className: 'nas-modal', describeError: errMessage };

/** The generic "type {name} to confirm" line, for a retype window without a line of its own. */
export const nasRetypeLabel = (name) => T('danger.retype', { name: `<code>${escapeHtml(name)}</code>` });

/**
 * Standard follow-up of an admin action: a `{ job }` answer opens the job
 * log and calls `onDone` when it finishes; an `{ approval }` answer means the
 * node PARKED the operation for a second admin and nothing ran, so it reports
 * that instead of a success; a direct answer calls `onDone` right away;
 * `null` (sudo prompt cancelled) does nothing.
 */
export function followResponse(screen, res, onDone, successMessage = '') {
  if (!res) return;
  if (res.approval && res.approval.requestId) {
    reportParked(res.approval);
    if (onDone) onDone(res);
    return;
  }
  if (res.job && res.job.jobId) {
    screen.openJobLog(res.job.jobId, onDone);
    return;
  }
  if (successMessage) toast(successMessage, 'success');
  if (onDone) onDone(res);
}

/** Danger-zone row: a description on the left, the action button on the right. */
export function dangerRowHtml({ title, desc, action, icon = 'trash', act, disabled = false }) {
  return `
    <div class="dz-row">
      <div><div class="dz-title">${escapeHtml(title)}</div><div class="dz-desc">${escapeHtml(desc)}</div></div>
      <tf-button variant="danger" size="sm" icon="${escapeAttr(icon)}" data-act="${escapeAttr(act)}" ${disabled ? 'disabled' : ''}>${escapeHtml(action)}</tf-button>
    </div>`;
}

export const warningHtml = (tone, text) => `<div class="wizard-warning ${tone}">${sprite(tone === 'danger' ? 'alert' : 'info')}<div>${escapeHtml(text)}</div></div>`;

/**
 * Breadcrumb of a browsed path: the root label, then one item per segment,
 * every item but the last a link. `wirePathCrumbs` maps a clicked link back
 * to the path it stands for (with the leading slash of `path` preserved).
 */
export function pathCrumbsHtml(rootLabel, path) {
  const parts = String(path || '').split('/').filter(Boolean);
  const items = [rootLabel, ...parts];
  return `<tf-breadcrumb class="nas-crumbs">${items.map((label, i) => (i === items.length - 1
    ? `<tf-breadcrumb-item current>${escapeHtml(label)}</tf-breadcrumb-item>`
    : `<tf-breadcrumb-item href="#">${escapeHtml(label)}</tf-breadcrumb-item>`)).join('')}</tf-breadcrumb>`;
}

export function wirePathCrumbs(el, path, go) {
  const prefix = String(path || '').startsWith('/') ? '/' : '';
  const parts = String(path || '').split('/').filter(Boolean);
  el.addEventListener('click', (e) => {
    const a = e.target.closest('a');
    if (!a) return;
    e.preventDefault();
    const i = [...el.querySelectorAll('a')].indexOf(a);
    go(i <= 0 ? '' : prefix + parts.slice(0, i).join('/'));
  });
}

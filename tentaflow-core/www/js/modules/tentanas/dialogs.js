// ===== File: modules/tentanas/dialogs.js — the retype-to-confirm danger dialog and the job follow-up shared by pools, datasets and snapshots =====
//
// Every destructive TentaNas action (destroy pool, destroy dataset, roll
// back a snapshot) confirms the same way the addon uninstall dialog does:
// the admin retypes the exact name and the danger button stays disabled
// until it matches. One implementation keeps the three dialogs identical.

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { T, sprite, errMessage } from '/js/modules/tentanas/format.js';
import '/js/components/tf-window.js';
import '/js/components/tf-input.js';
import '/js/components/tf-button.js';
import '/js/components/tf-breadcrumb.js';

/**
 * Opens a danger dialog whose confirm button unlocks only when `name` is
 * retyped exactly. `bodyHtml` sits above the retype row; `wire(win)` may
 * attach handlers to it. `onConfirm(win)` runs the action; it may throw
 * (the dialog stays open with the error under the retype field) or return
 * `false` (nothing ran, the dialog stays open unchanged). `retypeLabel`
 * replaces the generic "type {name} to confirm" line (HTML, already
 * escaped by the caller); `secondary` adds a middle footer button
 * (`{ label, icon, onClick }`) for the safer alternative the mockups offer
 * next to a destructive action ("Zrób Clone zamiast").
 */
export function openRetypeDialog({ title, subtitle = '', icon = 'trash', name, bodyHtml = '', confirmLabel, confirmIcon = 'trash', width = 560, wire = null, retypeLabel = '', secondary = null, onConfirm }) {
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', title);
  if (subtitle) win.setAttribute('subtitle', subtitle);
  win.setAttribute('icon', icon);
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', String(width));
  win.setAttribute('min-width', '460');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.innerHTML = `
    <div slot="body" class="stack">
      ${bodyHtml}
      <div class="confirm-type">
        <span>${retypeLabel || T('danger.retype', { name: `<code>${escapeHtml(name)}</code>` })}</span>
        <tf-input id="nas-retype" autocomplete="off" spellcheck="false" placeholder="${escapeAttr(name)}"></tf-input>
      </div>
      <div class="num-err" id="nas-retype-error" hidden></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      ${secondary ? `<span class="spacer" style="flex:1"></span><tf-button variant="secondary" icon="${escapeAttr(secondary.icon || 'copy')}" data-act="secondary">${escapeHtml(secondary.label)}</tf-button>` : ''}
      <tf-button variant="danger" icon="${escapeAttr(confirmIcon)}" data-action="confirm" disabled>${escapeHtml(confirmLabel)}</tf-button>
    </div>`;
  document.body.appendChild(win);
  const input = win.querySelector('#nas-retype');
  const btn = win.querySelector('[data-action="confirm"]');
  if (secondary) win.querySelector('[data-act="secondary"]').addEventListener('click', () => { win.close(true); secondary.onClick(); });
  let busy = false;
  const armed = () => input.value.trim() === name;
  const syncButton = () => {
    if (armed() && !busy) btn.removeAttribute('disabled');
    else btn.setAttribute('disabled', '');
  };
  input.addEventListener('input', syncButton);
  input.addEventListener('change', syncButton);
  input.addEventListener('keydown', (e) => { if (e.key === 'Enter' && armed() && !busy) btn.click(); });
  if (wire) wire(win, syncButton);
  win.addEventListener('action', async (e) => {
    if (e.detail?.action === 'cancel') { win.close(true); return; }
    if (e.detail?.action !== 'confirm') return;
    e.preventDefault();
    if (busy || !armed()) return;
    busy = true;
    syncButton();
    try {
      // `false` means "nothing happened" (the sudo prompt was cancelled):
      // the dialog stays open with the retype still armed.
      const outcome = await onConfirm(win);
      if (outcome === false) { busy = false; syncButton(); return; }
      win.close(true);
    } catch (err) {
      busy = false;
      syncButton();
      const errEl = win.querySelector('#nas-retype-error');
      errEl.textContent = errMessage(err);
      errEl.hidden = false;
    }
  });
  return win;
}

/**
 * Standard follow-up of an admin action: a `{ job }` answer opens the job
 * log and calls `onDone` when it finishes; a direct answer calls `onDone`
 * right away; `null` (sudo prompt cancelled) does nothing.
 */
export function followResponse(screen, res, onDone, successMessage = '') {
  if (!res) return;
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

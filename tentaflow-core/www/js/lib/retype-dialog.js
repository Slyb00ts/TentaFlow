// ===== File: lib/retype-dialog.js — the "retype the name to confirm" danger window shared by every screen with destructive actions =====
//
// A destructive action (destroy a pool, delete a topic, remove a message
// pattern) confirms the same way on every screen: the admin retypes the exact
// name and the danger button stays disabled until it matches. One
// implementation keeps those windows identical across TentaNas and TentaBus.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import '/js/components/tf-window.js';
import '/js/components/tf-input.js';
import '/js/components/tf-button.js';

const defaultDescribeError = (err) => String(err?.message || err || '');

/**
 * Opens a danger window whose confirm button unlocks only when `name` is
 * retyped exactly.
 *
 * `retypeLabel` is the HTML line above the field (already escaped by the
 * caller — it usually highlights the name). `bodyHtml` sits above it; `wire(win,
 * syncButton)` may attach handlers to it. `className` scopes the window for
 * the calling screen's stylesheet (a tf-window is appended to <body>, outside
 * the screen root). `onConfirm(win)` runs the action; it may throw (the window
 * stays open with `describeError(err)` under the field) or return `false`
 * (nothing ran, the window stays open unchanged). `secondary` adds a middle
 * footer button (`{ label, icon, onClick }`) for a safer alternative.
 *
 * `modal` dims the screen behind the window while it is open.
 *
 * `alsoArmed()` is a SECOND condition the confirm button waits for, on top of
 * the retyped name — for an action with a second victim the name does not
 * mention. Callers that pass it must re-run `syncButton` (handed to `wire`)
 * whenever their own condition changes, or the button never unlocks.
 */
export function openRetypeDialog({
  title, subtitle = '', icon = 'trash', name, bodyHtml = '', retypeLabel,
  confirmLabel, confirmIcon = 'trash', width = 560, className = '',
  describeError = defaultDescribeError, wire = null, secondary = null, alsoArmed = null, modal = false, onConfirm,
}) {
  const win = document.createElement('tf-window');
  if (className) win.className = className;
  if (modal) win.setAttribute('modal', '');
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
        <div class="field">
          <label>${retypeLabel}</label>
          <tf-input id="retype-input" autocomplete="off" spellcheck="false" placeholder="${escapeAttr(name)}"></tf-input>
        </div>
      </div>
      <div class="num-err" id="retype-error" hidden></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      ${secondary ? `<span class="spacer" style="flex:1"></span><tf-button variant="secondary" icon="${escapeAttr(secondary.icon || 'copy')}" data-act="secondary">${escapeHtml(secondary.label)}</tf-button>` : ''}
      <tf-button variant="danger" icon="${escapeAttr(confirmIcon)}" data-action="confirm" disabled>${escapeHtml(confirmLabel)}</tf-button>
    </div>`;
  document.body.appendChild(win);
  const input = win.querySelector('#retype-input');
  const btn = win.querySelector('[data-action="confirm"]');
  const cancelBtn = win.querySelector('[data-action="cancel"]');
  if (secondary) win.querySelector('[data-act="secondary"]').addEventListener('click', () => { win.close(true); secondary.onClick(); });
  let busy = false;
  const armed = () => input.value.trim() === name && (!alsoArmed || alsoArmed() === true);
  const syncButton = () => {
    if (armed() && !busy) btn.removeAttribute('disabled');
    else btn.setAttribute('disabled', '');
    cancelBtn.toggleAttribute('disabled', busy);
  };
  input.addEventListener('input', syncButton);
  input.addEventListener('change', syncButton);
  input.addEventListener('keydown', (e) => { if (e.key === 'Enter' && armed() && !busy) btn.click(); });
  if (wire) wire(win, syncButton);
  // While the action is out the window stays (Escape, the close button):
  // closed, it could not show the answer — a refusal would go unseen.
  win.addEventListener('close-request', (e) => { if (busy) e.preventDefault(); });
  win.addEventListener('action', async (e) => {
    if (e.detail?.action === 'cancel') { if (!busy) win.close(true); return; }
    if (e.detail?.action !== 'confirm') return;
    e.preventDefault();
    if (busy || !armed()) return;
    busy = true;
    syncButton();
    try {
      // `false` means "nothing happened" (e.g. a sudo prompt was cancelled):
      // the window stays open with the retype still armed.
      const outcome = await onConfirm(win);
      if (outcome === false) { busy = false; syncButton(); return; }
      win.close(true);
    } catch (err) {
      busy = false;
      syncButton();
      const errEl = win.querySelector('#retype-error');
      errEl.textContent = describeError(err);
      errEl.hidden = false;
    }
  });
  return win;
}

// ===== File: lib/sudo-dialog.js — the "type the node's sudo password to fix this" window shared by every screen that repairs a node =====
//
// When a node is missing something only root can put right, the dashboard
// fixes it instead of sending the operator to a terminal: it asks for the
// node's sudo password once and runs the repair. The window stays open while
// the repair runs, so a wrong password or a refusal is shown under the field
// and can be corrected in place rather than lost with a closed window.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import '/js/components/tf-window.js';
import '/js/components/tf-input.js';
import '/js/components/tf-button.js';
import '/js/components/tf-alert.js';

const defaultDescribeError = (err) => String(err?.message || err || '');

/**
 * Asks for the sudo password of `nodeName` and runs `onConfirm(password)`.
 *
 * `explainHtml` (already escaped by the caller) says what will be done as root
 * and why. `onConfirm` may throw: the window stays open with
 * `describeError(err)` under the field and the password cleared. Resolves to
 * what `onConfirm` returned, or `null` when the operator cancelled.
 *
 * The password lives only in the field and in the one call it is handed to.
 */
export function openSudoDialog({
  title, nodeName = '', explainHtml = '', confirmLabel, confirmIcon = 'key',
  width = 560, describeError = defaultDescribeError, onConfirm,
}) {
  return new Promise((resolve) => {
    const win = document.createElement('tf-window');
    win.setAttribute('modal', '');
    win.setAttribute('title', title);
    win.setAttribute('icon', 'key');
    win.setAttribute('buttons', 'close');
    win.setAttribute('draggable', '');
    win.setAttribute('width', String(width));
    win.setAttribute('min-width', '420');
    win.setAttribute('initial-x', 'center');
    win.setAttribute('initial-y', 'center');
    const label = nodeName
      ? I18n.t('sudo_dialog.password_label_node', { node: nodeName })
      : I18n.t('sudo_dialog.password_label');
    win.innerHTML = `
      <div slot="body" class="stack">
        ${explainHtml ? `<p>${explainHtml}</p>` : ''}
        <tf-input id="sudo-dialog-password" type="password" autocomplete="current-password"
          autofocus label="${escapeAttr(label)}"></tf-input>
        <p>${escapeHtml(I18n.t('sudo_dialog.not_stored'))}</p>
        <tf-alert tone="danger" id="sudo-dialog-error" hidden></tf-alert>
      </div>
      <div slot="footer">
        <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
        <tf-button variant="primary" icon="${escapeAttr(confirmIcon)}" data-action="confirm">${escapeHtml(confirmLabel)}</tf-button>
      </div>`;
    document.body.appendChild(win);

    const input = win.querySelector('#sudo-dialog-password');
    const confirmBtn = win.querySelector('[data-action="confirm"]');
    const cancelBtn = win.querySelector('[data-action="cancel"]');
    const errorEl = win.querySelector('#sudo-dialog-error');
    let busy = false;
    let settled = false;
    const finish = (value) => {
      if (settled) return;
      settled = true;
      resolve(value);
    };
    const setBusy = (value) => {
      busy = value;
      confirmBtn.toggleAttribute('disabled', value);
      cancelBtn.toggleAttribute('disabled', value);
      input.toggleAttribute('disabled', value);
    };

    input.addEventListener('keydown', (e) => { if (e.key === 'Enter' && !busy) confirmBtn.click(); });
    // A repair in flight keeps the window: closed, it could not show the outcome.
    win.addEventListener('close-request', (e) => {
      if (busy) { e.preventDefault(); return; }
      finish(null);
    });
    win.addEventListener('action', async (e) => {
      const action = e.detail?.action;
      if (action === 'cancel') {
        if (!busy) { finish(null); win.close(true); }
        return;
      }
      if (action !== 'confirm') return;
      e.preventDefault();
      if (busy) return;
      const password = String(input.value || '');
      if (!password) {
        input.setAttribute('error', I18n.t('sudo_dialog.password_required'));
        return;
      }
      input.removeAttribute('error');
      errorEl.hidden = true;
      setBusy(true);
      try {
        const outcome = await onConfirm(password);
        finish(outcome);
        win.close(true);
      } catch (err) {
        setBusy(false);
        input.value = '';
        errorEl.setAttribute('message', describeError(err));
        errorEl.hidden = false;
        input.focus?.();
      }
    });
  });
}

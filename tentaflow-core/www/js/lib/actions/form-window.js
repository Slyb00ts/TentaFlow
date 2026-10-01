// ===== File: lib/actions/form-window.js — the one modal every shared action window is built on =====
//
// A modal tf-window with the subject of the action, an optional note, the
// caller's fields, an error line and Cancel / submit. What it guarantees for
// every window built on it: validation runs before anything is sent; while the
// request runs the window is busy (fields inert, buttons locked, Escape and the
// backdrop do nothing); a refusal from the server stays in the window as text
// under the fields and the window stays open; a success closes it and shows the
// toast the result asks for.

import { escapeAttr, escapeHtml } from '/js/utils.js';
import '/js/components/tf-window.js';
import '/js/components/tf-button.js';
import '/js/components/tf-alert.js';
import { focusTarget, t, validateFields } from './fields.js';
import { showResultToast } from './toast.js';

const deepActiveElement = () => {
  let el = document.activeElement;
  while (el?.shadowRoot?.activeElement) el = el.shadowRoot.activeElement;
  return el;
};

const describeError = (err) => String(err?.message || '').trim() || t('error_generic');

/**
 * Opens the window and returns the tf-window element.
 *
 * `sections` are nodes placed under the subject line; `fields` are the field
 * controllers (lib/actions/fields.js) checked before submit; `validate()` is an
 * extra check for controls that are not fields (it must mark its own errors and
 * answer whether all is fine). `collect()` builds what `onSubmit` receives.
 * `canSubmit()` keeps the submit button locked while it answers false — it is
 * re-evaluated on every input or change inside the window.
 *
 * `onSubmit(values)` returns a promise. Its result may be `{ message, undo }`:
 * with `undo` the window ends in an "Undo" toast, with only `message` in a plain
 * one. A rejection is shown inside the window (`errorMessage(err)` may turn a
 * typed server error into text; otherwise `err.message` is used).
 * `anchor` is the element focus returns to when the window closes.
 */
export function openFormWindow({
  title, icon, subject, note = null, width = 560, sections = [], fields = [],
  validate = null, collect, canSubmit = null, submitLabel, submitVariant = 'primary',
  submitIcon = 'check', onSubmit, errorMessage = describeError, anchor = null,
}) {
  const win = document.createElement('tf-window');
  win.setAttribute('title', title);
  win.setAttribute('icon', icon);
  win.setAttribute('buttons', 'close');
  win.setAttribute('modal', '');
  win.setAttribute('draggable', '');
  win.setAttribute('width', String(width));
  win.setAttribute('min-width', '360');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.classList.add('tf-act-window');
  win.innerHTML = `
    <div slot="body" class="tf-act">
      ${subject ? `<div class="tf-act__subject">${escapeHtml(subject)}</div>` : ''}
      ${note ? `<tf-alert class="tf-act__note" tone="${escapeAttr(note.tone ?? 'info')}" message="${escapeAttr(note.text)}"></tf-alert>` : ''}
      <div class="tf-act__content"></div>
      <tf-alert class="tf-act__error" tone="danger" role="alert" hidden></tf-alert>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-act="cancel">${escapeHtml(t('cancel'))}</tf-button>
      <tf-button variant="${escapeAttr(submitVariant)}" icon="${escapeAttr(submitIcon)}" data-act="submit">${escapeHtml(submitLabel)}</tf-button>
    </div>`;
  const body = win.querySelector('.tf-act');
  win.querySelector('.tf-act__content').append(...sections);
  const submitBtn = win.querySelector('[data-act="submit"]');
  const cancelBtn = win.querySelector('[data-act="cancel"]');
  const errorEl = win.querySelector('.tf-act__error');
  let busy = false;

  const syncSubmit = () => {
    submitBtn.toggleAttribute('disabled', busy || (canSubmit ? !canSubmit() : false));
  };
  const showError = (text) => {
    errorEl.setAttribute('message', text);
    errorEl.hidden = !text;
    if (text) errorEl.scrollIntoView?.({ block: 'nearest' });
  };
  const setBusy = (on) => {
    busy = on;
    body.toggleAttribute('inert', on);
    body.setAttribute('aria-busy', String(on));
    cancelBtn.toggleAttribute('disabled', on);
    submitBtn.setAttribute('label', on ? t('saving') : submitLabel);
    syncSubmit();
  };

  async function submit() {
    if (busy || submitBtn.hasAttribute('disabled')) return;
    showError('');
    const fieldsOk = validateFields(fields);
    const extraOk = validate ? validate() !== false : true;
    if (!fieldsOk || !extraOk) return;
    const values = collect();
    const restoreFocus = deepActiveElement();
    setBusy(true);
    let result;
    try {
      result = await onSubmit(values);
    } catch (err) {
      setBusy(false);
      showError(errorMessage(err));
      if (restoreFocus?.isConnected) restoreFocus.focus?.();
      return;
    }
    win.close(true);
    showResultToast(result);
  }

  win.addEventListener('close-request', (e) => { if (busy) e.preventDefault(); });
  win.addEventListener('closed', () => { if (anchor?.isConnected) focusTarget(anchor).focus?.(); });
  win.addEventListener('input', syncSubmit);
  win.addEventListener('change', syncSubmit);
  win.addEventListener('click', (e) => {
    const btn = e.target.closest('[data-act]');
    if (!btn || !win.contains(btn)) return;
    if (btn.dataset.act === 'cancel' && !busy) win.close(true);
    else if (btn.dataset.act === 'submit') submit();
  });
  win.addEventListener('keydown', (e) => {
    if (e.key !== 'Enter' || e.defaultPrevented) return;
    const tag = e.target.tagName;
    const multiline = tag === 'TEXTAREA';
    const plainField = tag === 'INPUT' && !e.target.closest('tf-searchbox');
    if ((multiline && (e.ctrlKey || e.metaKey)) || plainField) {
      e.preventDefault();
      submit();
    }
  });

  document.body.appendChild(win);
  const backdrop = win.previousElementSibling;
  if (backdrop?.classList.contains('tf-window-backdrop')) {
    backdrop.addEventListener('click', () => { if (!busy) win.close(); });
  }
  syncSubmit();
  return win;
}

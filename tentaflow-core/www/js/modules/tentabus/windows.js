// ===== File: modules/tentabus/windows.js — the two window shapes every TentaBus change goes through =====
//
// PROJEKT-SZCZEGOLOW.md: a change is made in a modal tf-window that says what
// will happen before anyone confirms, and a refusal stays in the window where
// it can be read. Two shapes cover every change on the screen:
//   - `openChangeWindow`: fields, a live "Co się stanie …" line computed from
//     what the fields hold, and a save button that unlocks once something
//     differs from what is there now (topic settings, message patterns);
//   - `openConfirmWindow`: a fixed lead and impact, and one action
//     (unprocessed messages, withdrawing a message pattern).

import { escapeHtml } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { patchHtml } from '/js/lib/dom-patch.js';
import { T } from '/js/modules/tentabus/format.js';
import '/js/components/tf-window.js';
import '/js/components/tf-button.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

/** A modal, draggable tf-window scoped by `tb-window` plus the caller's class. */
export function windowEl({ title, icon, width, cls }) {
  const win = document.createElement('tf-window');
  win.className = `tb-window ${cls}`;
  win.setAttribute('title', title);
  win.setAttribute('icon', icon);
  win.setAttribute('buttons', 'close');
  win.setAttribute('modal', '');
  win.setAttribute('draggable', '');
  win.setAttribute('width', String(width));
  win.setAttribute('min-width', '360');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  return win;
}

/**
 * One window with fields. `fields()` = the body's controls (markup), `wire(win,
 * sync)` attaches their handlers and calls `sync()` on every change,
 * `draft(win)` = the values the controls hold now (or `null` when one is not
 * valid), `impact(draft)` = the "Co się stanie" sentences, `save(draft)` sends
 * it and may throw: the window stays open with the refusal —
 * `errorHtml(err, draft)` when given (markup), else `describeError(err)` as
 * text. A refusal is hidden again as soon as the fields change. `onSaved(draft,
 * result)` runs after the window closed. `saveLabel`/`saveIcon` name the
 * action and `willHappen` the impact line; `current` is what is there now —
 * the save unlocks only once the draft differs from it.
 */
export function openChangeWindow({
  title, icon, width = 620, cls = 'tb-change-window', fields, wire, draft, current, impact,
  save, describeError, errorHtml = null, onSaved,
  saveLabel = T('settings.save'), saveIcon = 'check', willHappen = T('settings.will_happen'),
}) {
  const win = windowEl({ title, icon, width, cls });
  win.innerHTML = `
    <div slot="body" class="stack">
      ${fields()}
      <div class="tb-will-happen" data-role="impact" aria-live="polite"></div>
      <div class="tb-window-error" role="alert" data-role="error" hidden>${sprite('alert')}<div data-role="error-text"></div></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-act="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" icon="${saveIcon}" data-act="save" disabled>${escapeHtml(saveLabel)}</tf-button>
    </div>`;
  document.body.appendChild(win);
  let busy = false;
  let refusedSig = null;
  const saveBtn = win.querySelector('[data-act="save"]');
  const cancelBtn = win.querySelector('[data-act="cancel"]');
  const errEl = win.querySelector('[data-role="error"]');
  const changed = (d) => d != null && Object.keys(d).some((k) => d[k] !== current[k]);
  const sync = () => {
    const d = draft(win);
    const lines = d == null ? [] : impact(d);
    patchHtml(win.querySelector('[data-role="impact"]'), d == null
      ? `${sprite('info')}<div>${escapeHtml(T('settings.fix_fields'))}</div>`
      : changed(d)
        ? `${sprite('info')}<div><b>${escapeHtml(willHappen)}</b> ${lines.map(escapeHtml).join(' ')}</div>`
        : `${sprite('info')}<div>${escapeHtml(T('settings.nothing_changed'))}</div>`);
    saveBtn.toggleAttribute('disabled', busy || !changed(d));
    cancelBtn.toggleAttribute('disabled', busy);
    if (refusedSig != null && JSON.stringify(d) !== refusedSig) {
      refusedSig = null;
      errEl.hidden = true;
    }
  };
  wire(win, sync);
  sync();
  win.addEventListener('close-request', (e) => { if (busy) e.preventDefault(); });
  win.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-act]');
    if (!btn || btn.hasAttribute('disabled')) return;
    if (btn.dataset.act === 'cancel') { win.close(true); return; }
    if (btn.dataset.act !== 'save') return;
    const d = draft(win);
    if (!changed(d)) return;
    busy = true;
    sync();
    errEl.hidden = true;
    let result;
    try {
      result = await save(d);
    } catch (err) {
      busy = false;
      sync();
      const text = errEl.querySelector('[data-role="error-text"]');
      if (errorHtml) text.innerHTML = errorHtml(err, d);
      else text.textContent = describeError(err);
      errEl.hidden = false;
      refusedSig = JSON.stringify(d);
      return;
    }
    win.close(true);
    onSaved(d, result);
  });
  return win;
}

/**
 * One confirm window: a lead (markup), "Co się stanie" (`impactTitle` and the
 * `impact` sentences), an optional `info` line, an error line and the action.
 * `run()` sends it (may throw: the window stays with `describeError(err)`);
 * `onDone(result)` runs after the window closed. `audit` is the footer note.
 */
export function openConfirmWindow({
  title, icon, lead, impactTitle, impact, info = null, audit = null, button, buttonIcon = null,
  danger = false, width = 600, cls = 'tb-unp-confirm', run, describeError, onDone,
}) {
  const win = windowEl({ title, icon, width, cls });
  win.innerHTML = `
    <div slot="body" class="stack">
      ${lead}
      <div class="tb-will-happen" data-role="impact">${sprite('info')}<div><b>${escapeHtml(impactTitle)}</b> ${impact.map(escapeHtml).join(' ')}</div></div>
      ${info ? `<div class="tb-will-happen">${sprite('info')}<div>${escapeHtml(info)}</div></div>` : ''}
      <div class="tb-window-error" role="alert" data-role="error" hidden>${sprite('alert')}<span></span></div>
    </div>
    <div slot="footer">
      ${audit ? `<span class="tb-foot-note">${sprite('file-text')}${escapeHtml(audit)}</span>` : ''}
      <tf-button variant="ghost" data-act="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="${danger ? 'danger' : 'primary'}" icon="${buttonIcon || (danger ? 'close' : 'refresh')}" data-act="go">${escapeHtml(button)}</tf-button>
    </div>`;
  document.body.appendChild(win);
  const goBtn = win.querySelector('[data-act="go"]');
  const cancelBtn = win.querySelector('[data-act="cancel"]');
  let busy = false;
  const sync = () => {
    goBtn.toggleAttribute('disabled', busy);
    cancelBtn.toggleAttribute('disabled', busy);
  };
  win.addEventListener('close-request', (e) => { if (busy) e.preventDefault(); });
  win.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-act]');
    if (!btn || btn.hasAttribute('disabled')) return;
    if (btn.dataset.act === 'cancel') { win.close(true); return; }
    if (btn.dataset.act !== 'go') return;
    busy = true;
    sync();
    const errEl = win.querySelector('[data-role="error"]');
    errEl.hidden = true;
    let result;
    try {
      result = await run();
    } catch (err) {
      busy = false;
      sync();
      errEl.querySelector('span').textContent = describeError(err);
      errEl.hidden = false;
      return;
    }
    win.close(true);
    onDone?.(result);
  });
  return win;
}

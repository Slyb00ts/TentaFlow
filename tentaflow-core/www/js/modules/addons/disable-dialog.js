// =============================================================================
// File: modules/addons/disable-dialog.js
// Description: The confirmation before an addon instance is DISABLED (n18d).
//              Generic platform code: it asks the answering node what
//              disabling does there (AddonDisablePreviewRequest) — an app
//              supplies those consequences from its own real state through
//              its `disable_consequences` provider (TentaNas: shares and
//              targets that keep serving, schedules that stop or go on,
//              arrays and pools that stay) — and words each one. An app with
//              no provider and no background work is disabled without a
//              dialog, exactly as before.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import '/js/components/tf-window.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';

const t = (key, vars) => I18n.t(`addon_disable.${key}`, vars);

// What each effect looks like: the chip's status and the row's icon. Every
// status is one tf-chip knows (STATUS_CLASSES), so none renders as the
// neutral default by accident.
const EFFECT = {
  continues: { status: 'ok', icon: 'play' },
  stops: { status: 'warn', icon: 'pause' },
  kept: { status: 'info', icon: 'check' },
};

/** One consequence as a sentence in the reader's language; the node's `kind` never shows. */
export function consequenceText(c) {
  const key = `addon_disable.consequences.${c.kind}`;
  const vars = { ...(c.countVars || {}), names: (Array.isArray(c.names) ? c.names : []).join(', ') };
  const words = I18n.t(key, vars);
  return words === key ? '' : words;
}

function consequencesHtml(preview) {
  const rows = (preview.consequences || [])
    .map((c) => ({ c, text: consequenceText(c) }))
    .filter((r) => r.text);
  if (!rows.length) return '';
  const where = preview.nodeName ? t('on_node', { node: preview.nodeName }) : t('on_this_node');
  return `
    <div class="disable-consequences">
      <div class="disable-consequences-title">${escapeHtml(where)}</div>
      <ul class="disable-consequence-list">
        ${rows.map(({ c, text }) => {
          const effect = EFFECT[c.effect] || EFFECT.kept;
          return `<li class="disable-consequence ${escapeHtml(c.effect || 'kept')}">
            <svg class="icon"><use href="#i-${effect.icon}"/></svg>
            <span class="disable-consequence-text">${escapeHtml(text)}</span>
            <tf-chip size="sm" status="${effect.status}" label="${escapeHtml(t('effect_' + (EFFECT[c.effect] ? c.effect : 'kept')))}"></tf-chip>
          </li>`;
        }).join('')}
      </ul>
    </div>`;
}

function bodyHtml(preview, error) {
  return `
    <div class="disable-intro">
      <b>${escapeHtml(t('not_uninstall'))}</b> ${escapeHtml(t('intro'))}
      ${preview?.backgroundOnDisable ? `<div class="disable-background"><tf-chip size="sm" status="info" label="${escapeHtml(t('background_label'))}"></tf-chip> <span>${escapeHtml(t('background_hint'))}</span></div>` : ''}
    </div>
    ${error ? `<div class="alert warn"><svg class="icon"><use href="#i-alert"/></svg><div>${escapeHtml(t('preview_error', { error }))}</div></div>` : ''}
    ${preview ? consequencesHtml(preview) : ''}`;
}

/**
 * Resolves true when the admin confirmed the disable (or when there is
 * nothing to confirm: the app has no consequence provider and no background
 * work), false when the dialog was dismissed.
 */
export async function confirmDisable({ addonId, displayName }) {
  let preview = null;
  let error = '';
  try {
    preview = await ApiBinary.one('addonDisablePreviewRequest', { addonId });
  } catch (err) {
    error = err?.message || String(err);
  }
  if (preview && !(preview.consequences || []).length && !preview.backgroundOnDisable) return true;

  const name = preview?.displayName || displayName || '';
  return new Promise((resolve) => {
    const win = document.createElement('tf-window');
    win.setAttribute('title', t('title', { name }));
    win.setAttribute('icon', 'pause');
    win.setAttribute('buttons', 'close');
    win.setAttribute('draggable', '');
    win.setAttribute('min-width', '460');
    win.setAttribute('width', '560');
    win.setAttribute('initial-x', 'center');
    win.setAttribute('initial-y', 'center');
    win.classList.add('addon-disable-window');
    const body = document.createElement('div');
    body.slot = 'body';
    body.innerHTML = bodyHtml(preview, error);
    const foot = document.createElement('div');
    foot.slot = 'footer';
    foot.innerHTML = `
      <tf-button variant="secondary" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" icon="pause" data-action="confirm">${escapeHtml(t('confirm'))}</tf-button>`;
    win.appendChild(body);
    win.appendChild(foot);
    let settled = false;
    const settle = (value) => {
      if (settled) return;
      settled = true;
      resolve(value);
    };
    win.addEventListener('action', (e) => {
      const action = e.detail?.action;
      if (action === 'confirm') { settle(true); win.close(true); }
      else if (action === 'cancel') { settle(false); win.close(true); }
    });
    // The header's close button (and anything else that closes the window).
    win.addEventListener('close-request', () => settle(false));
    document.body.appendChild(win);
  });
}

// =============================================================================
// File: modules/settings-alert-collectors.js
// Description: The platform admin's list of INTERNAL alert collectors
//              (owner decision 2026-09-26, TentaNas wave 9b round 3). An
//              organisation's forwarding target may reach public addresses
//              plus the hosts, addresses and networks on this list — never
//              loopback, link-local or a cloud metadata address, listed or
//              not (the node refuses such an entry). Plain http:// only to an
//              entry marked "allow http". Stored as the replicated platform
//              setting `tentanas.forward_allowlist` through
//              SettingsUpdateRequest (Admin policy); organisations cannot
//              change it. The node validates the list and refuses a bad one
//              with a coded refusal this card words.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import '/js/components/tf-input.js';
import '/js/components/tf-button.js';

export const ALLOWLIST_SETTING = 'tentanas.forward_allowlist';

const t = (key, vars) => I18n.t(`settings.alert_collectors.${key}`, vars);

/** The stored list, or [] for an empty or unreadable setting. */
export function parseAllowlist(value) {
  try {
    const list = JSON.parse(String(value || '') || '[]');
    return Array.isArray(list)
      ? list.map((e) => ({ entry: String(e?.entry || ''), allowHttp: Boolean(e?.allow_http), port: Number(e?.port) || '' })).filter((e) => e.entry)
      : [];
  } catch {
    return [];
  }
}

function rowHtml(entry, i) {
  return `
    <div class="alert-collector-row" data-row="${i}">
      <tf-input data-role="entry" value="${escapeAttr(entry.entry)}" placeholder="10.0.5.0/24" autocomplete="off" spellcheck="false"></tf-input>
      <tf-input data-role="port" type="number" value="${escapeAttr(String(entry.port || ''))}" placeholder="${escapeAttr(t('port_any'))}" class="alert-collector-port"></tf-input>
      <label class="alert-collector-http"><input type="checkbox" data-role="http" ${entry.allowHttp ? 'checked' : ''}> ${escapeHtml(t('allow_http'))}</label>
      <tf-button size="sm" variant="ghost" icon="trash" data-act="remove" title="${escapeAttr(t('remove'))}"></tf-button>
    </div>`;
}

/** The card for the settings tab; `value` is the stored setting. */
export function renderAlertCollectorsCard(value) {
  const list = parseAllowlist(value);
  return `
    <div class="card" id="alert-collectors-card">
      <div class="card-header"><h3>${escapeHtml(t('title'))}</h3></div>
      <div class="card-body">
        <p class="form-hint" style="margin:0 0 12px;">${escapeHtml(t('hint'))}</p>
        <p class="form-hint" style="margin:0 0 12px;">${escapeHtml(t('never'))}</p>
        <div id="alert-collectors-rows">${list.map(rowHtml).join('')}</div>
        <div class="num-err" id="alert-collectors-error" hidden></div>
        <div style="display:flex;gap:8px;margin-top:12px;">
          <tf-button variant="ghost" icon="plus" id="alert-collectors-add">${escapeHtml(t('add'))}</tf-button>
          <tf-button variant="primary" icon="check" id="alert-collectors-save">${escapeHtml(I18n.t('common.save'))}</tf-button>
        </div>
      </div>
    </div>`;
}

/** The list as the rows show it now (empty rows left out). */
export function readAllowlist(host) {
  return [...host.querySelectorAll('.alert-collector-row')]
    .map((row) => {
      const port = Number(String(row.querySelector('[data-role="port"]')?.value || '').trim()) || 0;
      const out = {
        entry: String(row.querySelector('[data-role="entry"]')?.value || '').trim(),
        allow_http: Boolean(row.querySelector('[data-role="http"]')?.checked),
      };
      if (port) out.port = port;
      return out;
    })
    .filter((e) => e.entry);
}

/** The node's refusal of a list, in words; anything else as the common error. */
function refusalText(err) {
  const code = /refusal:(forward_allowlist_[a-z]+)/.exec(String(err?.message || ''))?.[1];
  if (code) {
    const key = `settings.alert_collectors.${code}`;
    const words = I18n.t(key);
    if (words !== key) return words;
  }
  return t('save_failed');
}

/** Wires the card; `onSaved(value)` gets the stored JSON. */
export function bindAlertCollectorsCard(host, onSaved) {
  const card = host.querySelector('#alert-collectors-card');
  if (!card) return;
  const rows = card.querySelector('#alert-collectors-rows');
  const errEl = card.querySelector('#alert-collectors-error');
  card.querySelector('#alert-collectors-add')?.addEventListener('click', () => {
    rows.insertAdjacentHTML('beforeend', rowHtml({ entry: '', allowHttp: false }, rows.children.length));
  });
  rows.addEventListener('click', (e) => {
    if (e.target.closest('[data-act="remove"]')) e.target.closest('.alert-collector-row')?.remove();
  });
  card.querySelector('#alert-collectors-save')?.addEventListener('click', async () => {
    const value = JSON.stringify(readAllowlist(card));
    try {
      await ApiBinary.action('settingsUpdateRequest', { entries: [{ key: ALLOWLIST_SETTING, value, isSecret: false }] });
      errEl.hidden = true;
      toast(t('saved'), 'success');
      onSaved?.(value);
    } catch (err) {
      errEl.textContent = refusalText(err);
      errEl.hidden = false;
    }
  });
}

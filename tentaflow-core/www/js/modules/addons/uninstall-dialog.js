// =============================================================================
// File: modules/addons/uninstall-dialog.js
// Description: The uninstall confirmation for an addon instance (admin). Opens
//              a tf-window, fetches the side-effect-free teardown plan
//              (AddonTeardownPlanRequest) and lists what the wipe removes and
//              what it consciously keeps, with sizes and the instances that
//              depend on the package. The danger button unlocks only after the
//              admin retypes the instance name; confirm sends
//              AddonUninstallRequest. Shared by the card action and the detail
//              header so both flows show the same facts.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, escapeAttr, toast, formatBytes } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import '/js/components/tf-window.js';

const t = (key, vars) => I18n.t(`addon_uninstall.${key}`, vars);

// Localized label for a plan entry; a kind without a translation shows the
// backend's English description so a new hook never renders an empty row.
function entryLabel(entry) {
  const key = `addon_uninstall.entries.${entry.kind}`;
  const label = I18n.t(key);
  return label === key ? entry.description : label;
}

function renderEntries(entries, removed) {
  const rows = entries.filter((e) => !!e.removed === removed);
  if (rows.length === 0) return '';
  const title = removed ? t('will_remove') : t('will_keep');
  const items = rows.map((e) => `
    <li class="uninstall-entry ${removed ? 'removed' : 'kept'}">
      <div class="uninstall-entry-label">${escapeHtml(entryLabel(e))}</div>
      <div class="uninstall-entry-path"><code>${escapeHtml(e.path)}</code>${e.sizeBytes > 0 ? ` · ${escapeHtml(formatBytes(e.sizeBytes))}` : ''}</div>
    </li>`).join('');
  return `
    <div class="uninstall-section">
      <div class="uninstall-section-title">${escapeHtml(title)}</div>
      <ul class="uninstall-entries">${items}</ul>
    </div>`;
}

function renderDependents(dependents) {
  if (!dependents.length) return '';
  const names = dependents.map((d) => `<b>${escapeHtml(d.displayName)}</b>${d.optional ? ` (${escapeHtml(t('dependent_optional'))})` : ''}`).join(', ');
  return `
    <div class="alert warn">
      <svg class="icon"><use href="#i-alert"/></svg>
      <div>${t('dependents', { names })}</div>
    </div>`;
}

function renderPlan(plan) {
  const entries = Array.isArray(plan.entries) ? plan.entries : [];
  const dependents = Array.isArray(plan.dependents) ? plan.dependents : [];
  const total = entries.filter((e) => e.removed).reduce((sum, e) => sum + Number(e.sizeBytes || 0), 0);
  return `
    <div class="uninstall-intro">${t('intro', { name: `<b>${escapeHtml(plan.displayName)}</b>` })}</div>
    ${renderDependents(dependents)}
    ${renderEntries(entries, true)}
    ${renderEntries(entries, false)}
    ${total > 0 ? `<div class="uninstall-total">${escapeHtml(t('total', { size: formatBytes(total) }))}</div>` : ''}
    <label class="uninstall-retype">
      <span>${t('retype', { name: `<code>${escapeHtml(plan.displayName)}</code>` })}</span>
      <tf-input id="uninstall-retype" autocomplete="off" spellcheck="false" placeholder="${escapeAttr(plan.displayName)}"></tf-input>
    </label>`;
}

/**
 * Opens the dialog for `addonId`. `displayName` seeds the title while the plan
 * loads; the plan's own name is what the admin has to retype. `onDone()` runs
 * after a successful uninstall (the caller refreshes its view).
 */
export function openUninstallDialog({ addonId, displayName, onDone }) {
  const win = document.createElement('tf-window');
  win.setAttribute('title', t('confirm_title'));
  win.setAttribute('subtitle', displayName || addonId);
  win.setAttribute('icon', 'trash');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('min-width', '460');
  win.setAttribute('width', '520');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.classList.add('addon-uninstall-window');

  const body = document.createElement('div');
  body.slot = 'body';
  body.innerHTML = `<div class="uninstall-loading">${escapeHtml(I18n.t('common.loading'))}</div>`;
  const foot = document.createElement('div');
  foot.slot = 'footer';
  foot.innerHTML = `
    <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
    <tf-button variant="danger" icon="trash" data-action="confirm" disabled>${escapeHtml(t('button'))}</tf-button>
  `;
  win.appendChild(body);
  win.appendChild(foot);
  const confirmBtn = foot.querySelector('[data-action="confirm"]');

  let plan = null;
  let busy = false;

  const armed = () => {
    const typed = (body.querySelector('#uninstall-retype')?.value || '').trim();
    return !!plan && typed === plan.displayName;
  };
  const syncButton = () => {
    if (armed() && !busy) confirmBtn.removeAttribute('disabled');
    else confirmBtn.setAttribute('disabled', '');
  };

  win.addEventListener('action', async (e) => {
    if (e.detail?.action !== 'confirm') return;
    e.preventDefault();
    if (!armed() || busy) return;
    busy = true;
    syncButton();
    try {
      const res = await ApiBinary.action('addonUninstallRequest', { addonId });
      if (res && res.ok === false) throw new Error(t('error'));
      toast(t('success', { name: plan.displayName }), 'success');
      win.close(true);
      await onDone?.();
    } catch (err) {
      busy = false;
      syncButton();
      toast(`${t('error')}: ${err.message}`, 'error');
    }
  });

  document.body.appendChild(win);

  ApiBinary.one('addonTeardownPlanRequest', { addonId }).then((res) => {
    plan = res;
    body.innerHTML = renderPlan(plan);
    body.querySelector('#uninstall-retype')?.addEventListener('input', syncButton);
    syncButton();
  }).catch((err) => {
    body.innerHTML = `<div class="alert warn"><svg class="icon"><use href="#i-alert"/></svg><div>${escapeHtml(t('plan_error'))}: ${escapeHtml(err.message)}</div></div>`;
  });

  return win;
}

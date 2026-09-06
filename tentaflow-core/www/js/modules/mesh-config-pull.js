// =============================================================================
// Plik: modules/mesh-config-pull.js
// Opis: Kreator ręcznego pull konfiguracji między środowiskami (ROADMAP Z12),
//       wzorowany 1:1 na `mesh-baseline-adopt.js` (donor-select -> start ->
//       diff -> apply). W odróżnieniu od baseline adopt, pull configu jest
//       synchroniczny (mała paczka: flows/aliasy/settings, nie cały baseline)
//       — nie ma tu wieloetapowego pollingu fazy, tylko trzy kroki: wybór
//       węzła źródłowego, przegląd różnic, zastosowanie. Promocja "w górę"
//       (w tym na PROD) pokazuje WIDOCZNY modal ostrzegawczy z liczbowym
//       opisem skutków PRZED polem potwierdzenia nazwy środowiska (D-Z12.8).
// =============================================================================

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-modal.js';
import '/js/components/tf-spinner.js';
import '/js/components/tf-input.js';
import '/js/components/tf-radio.js';
import '/js/components/tf-checkbox.js';

const ENV_ORDER = ['dev', 'test', 'prod'];

let modalEl = null;
let bodyEl = null;
let footerEl = null;
let onClosed = null;
let donors = [];
let selectedDonorId = null;
let pullId = null;
let diff = null;

function t(key, vars = null) {
  return I18n.t(`mesh.pull_wizard.${key}`, vars);
}

function setFooter(actions) {
  if (!footerEl) return;
  footerEl.innerHTML = actions.map((a) => `
    <tf-button variant="${escapeAttr(a.variant || 'secondary')}" data-action="${escapeAttr(a.action)}" ${a.disabled ? 'disabled' : ''}>${escapeHtml(a.label || '')}</tf-button>
  `).join('');
  footerEl.querySelectorAll('tf-button[data-action]').forEach((btn) => {
    btn.addEventListener('click', () => handleFooterAction(btn.dataset.action));
  });
}

function handleFooterAction(action) {
  if (action === 'close') { closeModal(); return; }
  if (action === 'back-to-donors') { renderDonorSelection(); return; }
  if (action === 'start-pull') { startPull(); return; }
  if (action === 'apply') { applyDiff(); return; }
}

function closeModal() {
  if (modalEl?.isConnected) modalEl.removeAttribute('open');
  modalEl?.remove();
  modalEl = null; bodyEl = null; footerEl = null;
  const cb = onClosed; onClosed = null;
  cb?.();
}

/// Otwiera kreator pull konfiguracji. `onDone` wołane po zamknięciu.
export async function openConfigPullModal({ onDone } = {}) {
  closeModal();
  onClosed = typeof onDone === 'function' ? onDone : null;
  donors = []; selectedDonorId = null; pullId = null; diff = null;

  modalEl = document.createElement('tf-modal');
  modalEl.setAttribute('variant', 'modal');
  modalEl.setAttribute('title', t('title'));

  bodyEl = document.createElement('div');
  bodyEl.setAttribute('slot', 'body');
  bodyEl.className = 'config-pull-body';
  bodyEl.innerHTML = `<div class="baseline-loading"><tf-spinner size="md"></tf-spinner></div>`;
  modalEl.appendChild(bodyEl);

  footerEl = document.createElement('div');
  footerEl.setAttribute('slot', 'footer');
  modalEl.appendChild(footerEl);

  modalEl.addEventListener('close', () => closeModal(), { once: true });
  document.body.appendChild(modalEl);
  modalEl.setAttribute('open', '');

  await loadAndRenderDonors();
}

// ---- Krok 1: wybór węzła źródłowego ---------------------------------------

async function loadAndRenderDonors() {
  if (!bodyEl) return;
  bodyEl.innerHTML = `<div class="baseline-loading"><tf-spinner size="md"></tf-spinner></div>`;
  let resp = null;
  try {
    resp = await ApiBinary.one('environmentPullDonorListRequest');
  } catch (err) {
    renderError(err);
    return;
  }
  donors = Array.isArray(resp?.donors) ? resp.donors : [];
  renderDonorSelection();
}

function renderDonorSelection() {
  pullId = null; diff = null;
  if (!bodyEl) return;

  if (donors.length === 0) {
    bodyEl.innerHTML = `
      <p class="baseline-intro">${escapeHtml(t('intro'))}</p>
      <div class="baseline-empty">${escapeHtml(t('no_donors'))}</div>
    `;
    setFooter([{ action: 'close', variant: 'secondary', label: I18n.t('common.close') }]);
    return;
  }

  // Grupowanie po środowisku — sekcja per env, sortowana Dev -> Test -> Prod.
  const byEnv = new Map();
  for (const d of donors) {
    const key = d.environment || 'prod';
    if (!byEnv.has(key)) byEnv.set(key, []);
    byEnv.get(key).push(d);
  }
  const sections = [...byEnv.entries()].sort((a, b) => ENV_ORDER.indexOf(a[0]) - ENV_ORDER.indexOf(b[0]));

  bodyEl.innerHTML = `
    <p class="baseline-intro">${escapeHtml(t('intro'))}</p>
    <tf-radio-group name="pull-donor" nested>
      ${sections.map(([env, nodes]) => `
        <div class="pull-env-group">
          <div class="pull-env-group-title">
            <span class="env-sidebar-badge env-${escapeAttr(env)}">${escapeHtml(I18n.t(`settings_environment.badge_${env}`))}</span>
          </div>
          ${nodes.map((d) => `
            <div class="baseline-donor-row">
              <tf-radio value="${escapeAttr(d.nodeId)}" label="${escapeAttr(d.hostname || d.nodeId.slice(0, 12))}"></tf-radio>
            </div>
          `).join('')}
        </div>
      `).join('')}
    </tf-radio-group>
  `;

  bodyEl.querySelector('tf-radio-group[name="pull-donor"]')?.addEventListener('change', (e) => {
    selectedDonorId = e.detail?.value ?? null;
    syncStartButtonState();
  });

  setFooter([
    { action: 'close', variant: 'secondary', label: I18n.t('common.cancel') },
    { action: 'start-pull', variant: 'primary', label: t('fetch'), disabled: true },
  ]);
}

function syncStartButtonState() {
  const btn = footerEl?.querySelector('[data-action="start-pull"]');
  if (!btn) return;
  if (selectedDonorId) btn.removeAttribute('disabled');
  else btn.setAttribute('disabled', '');
}

// ---- Krok 2: pobranie paczki + podgląd różnic ------------------------------

async function startPull() {
  if (!selectedDonorId || !bodyEl) return;
  bodyEl.innerHTML = `<div class="baseline-loading"><tf-spinner size="md"></tf-spinner><p>${escapeHtml(t('fetching'))}</p></div>`;
  setFooter([{ action: 'close', variant: 'secondary', label: I18n.t('common.cancel'), disabled: true }]);

  try {
    const startResp = await ApiBinary.action('environmentPullStartRequest', { donorNodeId: selectedDonorId });
    if (startResp.phase === 'failed') {
      renderError(new Error(startResp.error || t('fetch_failed')));
      return;
    }
    pullId = startResp.pullId;
    diff = await ApiBinary.one('environmentImportPreviewDiffRequest', { pullId });
    renderDiff();
  } catch (err) {
    renderError(err);
  }
}

function renderDiff() {
  if (!bodyEl || !diff) return;
  const promoting = ENV_ORDER.indexOf(diff.toEnvironment) > ENV_ORDER.indexOf(diff.fromEnvironment);
  const rows = [
    ...diff.added.map((d) => ({ ...d, status: 'added' })),
    ...diff.changed.map((d) => ({ ...d, status: 'changed' })),
  ];

  bodyEl.innerHTML = `
    <div class="env-diff-summary">
      <span class="env-diff-chip">${escapeHtml(diff.fromEnvironment)} → ${escapeHtml(diff.toEnvironment)}</span>
      <span>${escapeHtml(t('diff_summary', {
        flows: diff.flowsCount, aliases: diff.aliasesCount, settings: diff.settingsCount,
      }))}</span>
    </div>
    <ul class="env-diff-list">
      ${rows.map((r) => `
        <li>
          <tf-checkbox class="pull-diff-row" data-value="${escapeAttr(`${r.table}:${r.resourceId}`)}"></tf-checkbox>
          <span class="env-diff-status env-diff-${escapeAttr(r.status)}">${escapeHtml(r.status)}</span>
          <span>${escapeHtml(r.table)}</span> — <span>${escapeHtml(r.label)}</span>
        </li>
      `).join('') || `<li class="hint">${escapeHtml(t('diff_empty'))}</li>`}
    </ul>
    ${promoting ? `
      <div class="callout danger pull-promote-warning">
        <h4>${escapeHtml(t('promote_title'))}</h4>
        <p>${escapeHtml(t('promote_body', { from: diff.fromEnvironment, to: diff.toEnvironment }))}</p>
        <p>${escapeHtml(t('promote_counts', {
          flows: diff.flowsCount, aliases: diff.aliasesCount, settings: diff.settingsCount,
        }))}</p>
        <tf-input id="pull-confirm-name" placeholder="${escapeAttr(diff.toEnvironment.toUpperCase())}"></tf-input>
      </div>
    ` : ''}
  `;

  setFooter([
    { action: 'back-to-donors', variant: 'secondary', label: I18n.t('common.back') },
    { action: 'apply', variant: promoting ? 'danger' : 'primary', label: t('apply'), disabled: promoting },
  ]);

  if (promoting) {
    const input = bodyEl.querySelector('#pull-confirm-name');
    const applyBtn = footerEl?.querySelector('[data-action="apply"]');
    const sync = () => {
      const ok = (input?.value ?? '') === diff.toEnvironment.toUpperCase();
      if (ok) applyBtn?.removeAttribute('disabled');
      else applyBtn?.setAttribute('disabled', '');
    };
    input?.addEventListener('input', sync);
  }
}

// ---- Krok 3: zastosowanie --------------------------------------------------

async function applyDiff() {
  if (!bodyEl || !pullId || !diff) return;
  const selected = [...bodyEl.querySelectorAll('tf-checkbox.pull-diff-row[checked]')]
    .map((el) => el.dataset.value);
  const promoting = ENV_ORDER.indexOf(diff.toEnvironment) > ENV_ORDER.indexOf(diff.fromEnvironment);
  const confirmEnvironmentName = promoting ? (bodyEl.querySelector('#pull-confirm-name')?.value ?? '') : null;

  setFooter([{ action: 'close', variant: 'secondary', label: I18n.t('common.cancel'), disabled: true }]);
  bodyEl.innerHTML = `<div class="baseline-loading"><tf-spinner size="md"></tf-spinner></div>`;

  try {
    const result = await ApiBinary.action('environmentImportApplyRequest', {
      pullId, confirmEnvironmentName, selectedResourceKeys: selected,
    });
    bodyEl.innerHTML = `<div class="baseline-empty">${escapeHtml(t('applied', { n: result.importedCount }))}</div>`;
    setFooter([{ action: 'close', variant: 'primary', label: I18n.t('common.close') }]);
    toast(t('toast_applied', { n: result.importedCount }), 'success');
  } catch (err) {
    renderError(err);
  }
}

function renderError(err) {
  if (!bodyEl) return;
  bodyEl.innerHTML = `<div class="baseline-empty" style="color:var(--danger);">${escapeHtml(err.message || String(err))}</div>`;
  setFooter([{ action: 'close', variant: 'secondary', label: I18n.t('common.close') }]);
}

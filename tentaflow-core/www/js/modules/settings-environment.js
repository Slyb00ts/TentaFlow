// =============================================================================
// Plik: modules/settings-environment.js
// Opis: Zakladka "Środowisko" w ekranie Ustawienia (ROADMAP Z12). Środowisko
//       węzła (Dev/Test/Prod) jest atrybutem tożsamości węzła w JEDNEJ sieci
//       mesh — bezpośrednia synchronizacja działa TYLKO między węzłami tego
//       samego środowiska (fencing w warstwie sync/handshake, egzekwowany
//       serwerowo niezależnie od tego ekranu). Zmiana środowiska ZAWSZE
//       przechodzi przez modal potwierdzenia (D-Z12.9); dla celu PROD modal
//       wymaga wpisania słowa „PROD”, walidowanego też SERWEROWO. Karta
//       „Paczki konfiguracji” obsługuje transport plikowy (eksport/import)
//       tej samej paczki, którą kreator Mesh (`mesh-config-pull.js`) pobiera
//       przez QUIC.
// =============================================================================

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-button.js';
import '/js/components/tf-input.js';
import '/js/components/tf-file-input.js';
import '/js/components/tf-window.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-checkbox.js';

const ENV_ORDER = ['dev', 'test', 'prod'];

let state = {
  kind: 'prod',
  isolationStrict: false,
};

function t(key, vars = null) {
  return I18n.t(`settings_environment.${key}`, vars);
}

export async function loadEnvironmentTab() {
  const resp = await ApiBinary.one('environmentGetKindRequest').catch(() => null);
  if (resp) {
    state = { kind: resp.kind ?? 'prod', isolationStrict: !!resp.isolationStrict };
  }
}

function envLabel(kind) {
  return t(`badge_${kind}`);
}

function envConsequences(kind) {
  return t(`consequences_${kind}`);
}

export function renderEnvironmentTab() {
  return `
    <div class="settings-env">
      <div class="card">
        <h3>${escapeHtml(t('selector_title'))}</h3>
        <p class="hint">${escapeHtml(t('selector_hint'))}</p>
        <tf-segmented id="env-kind-select" value="${escapeAttr(state.kind)}" size="lg">
          ${ENV_ORDER.map((k) => `<option value="${k}">${escapeHtml(envLabel(k))}</option>`).join('')}
        </tf-segmented>
        <p class="hint env-consequences env-${escapeAttr(state.kind)}">${escapeHtml(envConsequences(state.kind))}</p>
        <div class="env-badge-preview">
          <span class="env-sidebar-badge env-test">${escapeHtml(t('badge_test'))}</span>
          <span class="env-sidebar-badge env-prod">${escapeHtml(t('badge_prod'))}</span>
        </div>
      </div>

      <div class="card">
        <h3>${escapeHtml(t('isolation_title'))}</h3>
        <p class="hint">${escapeHtml(t('isolation_hint'))}</p>
        <label class="toggle-row">
          <tf-toggle id="env-isolation-strict" ${state.isolationStrict ? 'checked' : ''}></tf-toggle>
          <span>${escapeHtml(t('isolation_toggle_label'))}</span>
        </label>
      </div>

      <div class="card">
        <h3>${escapeHtml(t('bundle_title'))} <tf-chip variant="accent" size="sm">${escapeHtml(I18n.t('common.new_badge') || 'NOWE')}</tf-chip></h3>
        <p class="hint">${escapeHtml(t('bundle_hint'))}</p>
        <div class="toolbar-row">
          <tf-button id="env-bundle-export" icon="download" variant="secondary" label="${escapeAttr(t('bundle_export'))}"></tf-button>
          <tf-file-input id="env-bundle-import-file" accept=".json" label="${escapeAttr(t('bundle_import'))}"></tf-file-input>
        </div>
        <div id="env-bundle-diff-host"></div>
      </div>
    </div>
  `;
}

// -----------------------------------------------------------------------------
// Confirm modal helper — mirrors `mesh.js::createPairWindow` (tf-window +
// backdrop, footer buttons via `action` events).
// -----------------------------------------------------------------------------

function createConfirmWindow({ title, bodyHtml, submitLabel, danger, onSubmit }) {
  const win = document.createElement('tf-window');
  win.setAttribute('title', title);
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('min-width', '460');
  win.setAttribute('width', '520');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');

  const bodyWrap = document.createElement('div');
  bodyWrap.slot = 'body';
  bodyWrap.innerHTML = bodyHtml;
  win.appendChild(bodyWrap);

  const footWrap = document.createElement('div');
  footWrap.slot = 'footer';
  footWrap.innerHTML = `
    <tf-button variant="secondary" data-action="cancel" label="${escapeAttr(I18n.t('common.cancel'))}"></tf-button>
    <tf-button variant="${danger ? 'danger' : 'primary'}" data-action="confirm" label="${escapeAttr(submitLabel)}"></tf-button>
  `;
  win.appendChild(footWrap);

  const backdrop = document.createElement('div');
  backdrop.className = 'tf-window-backdrop';
  document.body.appendChild(backdrop);
  document.body.appendChild(win);

  const cleanup = () => {
    if (win.isConnected) win.remove();
    if (backdrop.isConnected) backdrop.remove();
  };

  win.addEventListener('action', async (e) => {
    const action = e.detail?.action;
    if (action === 'cancel' || action === 'close') {
      cleanup();
      return;
    }
    if (action === 'confirm') {
      e.preventDefault();
      try {
        const ok = await onSubmit(win);
        if (ok) cleanup();
      } catch (err) {
        const errBox = win.querySelector('.form-error');
        if (errBox) {
          errBox.textContent = err.message;
          errBox.hidden = false;
        }
      }
    }
  });

  return win;
}

// Modal "Czy na pewno?" zmiany środowiska (D-Z12.9). Dla celu PROD wymagane
// jest dokładne wpisanie słowa "PROD" — przycisk potwierdzenia jest disabled
// do czasu zgodności tekstu (walidacja SERWEROWA to jedyny realny gating,
// to pole to tylko UX).
function openSetKindConfirmModal(newKind, onDone) {
  const requiresProd = newKind === 'prod';
  const bodyHtml = `
    <p>${escapeHtml(t('confirm_intro', { from: envLabel(state.kind), to: envLabel(newKind) }))}</p>
    <ul class="env-consequence-list">
      <li>${escapeHtml(t('confirm_consequence_sync'))}</li>
      <li>${escapeHtml(t('confirm_consequence_repair'))}</li>
      <li>${escapeHtml(t('confirm_consequence_routing'))}</li>
      <li>${escapeHtml(t('confirm_consequence_rejected'))}</li>
    </ul>
    ${requiresProd ? `
      <div class="callout danger">
        <p>${escapeHtml(t('confirm_prod_warning'))}</p>
        <tf-input id="env-confirm-prod-input" placeholder="PROD"></tf-input>
      </div>
    ` : ''}
    <div class="form-error" hidden></div>
  `;

  const win = createConfirmWindow({
    title: t('confirm_title'),
    bodyHtml,
    submitLabel: t('confirm_submit'),
    danger: requiresProd,
    onSubmit: async (winEl) => {
      const confirmValue = requiresProd ? (winEl.querySelector('#env-confirm-prod-input')?.value ?? '') : undefined;
      const resp = await ApiBinary.action('environmentSetKindRequest', {
        newKind,
        confirmEnvironmentName: requiresProd ? confirmValue : null,
      });
      state.kind = resp.kind;
      toast(t('toast_kind_changed'), 'success');
      window.dispatchEvent(new CustomEvent('tf:environment-changed'));
      onDone?.();
      return true;
    },
  });

  if (requiresProd) {
    const input = win.querySelector('#env-confirm-prod-input');
    const submitBtn = win.querySelector('[data-action="confirm"]');
    const updateDisabled = () => {
      const ok = (input?.value ?? '') === 'PROD';
      if (ok) submitBtn?.removeAttribute('disabled');
      else submitBtn?.setAttribute('disabled', '');
    };
    updateDisabled();
    input?.addEventListener('input', updateDisabled);
  }
}

export function bindEnvironmentTab(host, onChanged) {
  host.querySelector('#env-kind-select')?.addEventListener('change', (e) => {
    const newKind = e.detail?.value;
    if (!newKind || newKind === state.kind) return;
    // Rewinduj segmented do biezacej wartosci — realna zmiana nastapi
    // dopiero po potwierdzeniu w modalu (nigdy przed nim, D-Z12.9).
    e.target.setAttribute('value', state.kind);
    openSetKindConfirmModal(newKind, () => onChanged?.());
  });

  host.querySelector('#env-isolation-strict')?.addEventListener('change', async (e) => {
    const strict = !!e.target.checked;
    try {
      await ApiBinary.action('environmentSetStrictIsolationRequest', { strict });
      state.isolationStrict = strict;
      toast(t('toast_isolation_changed'), 'success');
    } catch (err) {
      e.target.checked = !strict;
      toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  });

  host.querySelector('#env-bundle-export')?.addEventListener('click', async () => {
    try {
      const resp = await ApiBinary.one('environmentExportBundleRequest');
      const bytes = resp.archiveBytes instanceof Uint8Array ? resp.archiveBytes : new Uint8Array(resp.archiveBytes ?? []);
      const blob = new Blob([bytes], { type: 'application/json' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = resp.filename || 'tentaflow-config-bundle.json';
      document.body.appendChild(a);
      a.click();
      a.remove();
      URL.revokeObjectURL(url);
      toast(t('toast_exported'), 'success');
    } catch (err) {
      toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  });

  host.querySelector('#env-bundle-import-file')?.addEventListener('change', async (e) => {
    const file = e.detail?.files?.[0] ?? null;
    if (!file) return;
    try {
      const bytes = new Uint8Array(await file.arrayBuffer());
      const startResp = await ApiBinary.action('environmentImportFromFileRequest', { archiveBytes: bytes });
      await renderBundleDiff(host, startResp.pullId);
    } catch (err) {
      toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  });
}

async function renderBundleDiff(host, pullId) {
  const diffHost = host.querySelector('#env-bundle-diff-host');
  if (!diffHost) return;
  diffHost.innerHTML = `<p class="hint">${escapeHtml(I18n.t('common.loading'))}</p>`;
  let diff;
  try {
    diff = await ApiBinary.one('environmentImportPreviewDiffRequest', { pullId });
  } catch (err) {
    diffHost.innerHTML = `<p class="hint" style="color:var(--danger);">${escapeHtml(err.message)}</p>`;
    return;
  }

  const promoting = ENV_ORDER.indexOf(diff.toEnvironment) > ENV_ORDER.indexOf(diff.fromEnvironment);
  // File-transport import always requires confirmation when landing on Prod,
  // even at same rank (N3, delta-review) — the server enforces this
  // unconditionally for `from_file` bundles (`dispatch/environment.rs`
  // `requires_confirmation`, P2-4), because the archive's declared
  // `fromEnvironment` is a self-reported claim in an uploaded file, not a
  // donor-attested value. `renderBundleDiff` is only ever reached from the
  // file-import flow, so this mirrors the server gate exactly.
  const requiresConfirmation = promoting || diff.toEnvironment === 'prod';
  const rows = [...diff.added.map((d) => ({ ...d, status: 'added' })), ...diff.changed.map((d) => ({ ...d, status: 'changed' }))];

  diffHost.innerHTML = `
    <div class="env-diff-summary">
      <span class="env-diff-chip">${escapeHtml(diff.fromEnvironment)} → ${escapeHtml(diff.toEnvironment)}</span>
      <span>${escapeHtml(t('diff_summary', {
        flows: diff.flowsCount,
        aliases: diff.aliasesCount,
        settings: diff.settingsCount,
      }))}</span>
    </div>
    ${promoting
      ? `<div class="callout danger"><p>${escapeHtml(t('promote_warning', { from: diff.fromEnvironment, to: diff.toEnvironment }))}</p></div>`
      : requiresConfirmation
        ? `<div class="callout danger"><p>${escapeHtml(t('import_prod_warning', { to: diff.toEnvironment }))}</p></div>`
        : ''}
    <ul class="env-diff-list">
      ${rows.map((r) => `
        <li>
          <tf-checkbox class="env-diff-row" data-value="${escapeAttr(`${r.table}:${r.resourceId}`)}"></tf-checkbox>
          <span class="env-diff-status env-diff-${escapeAttr(r.status)}">${escapeHtml(r.status)}</span>
          <span>${escapeHtml(r.table)}</span> — <span>${escapeHtml(r.label)}</span>
        </li>
      `).join('') || `<li class="hint">${escapeHtml(t('diff_empty'))}</li>`}
    </ul>
    ${diff.skipped.length ? `<p class="hint">${escapeHtml(t('diff_skipped_note', { n: diff.skipped.length }))}</p>` : ''}
    ${requiresConfirmation ? `<tf-input id="env-diff-confirm-name" placeholder="${escapeAttr(diff.toEnvironment.toUpperCase())}"></tf-input>` : ''}
    <tf-button id="env-diff-apply" variant="${requiresConfirmation ? 'danger' : 'primary'}" label="${escapeAttr(t('diff_apply'))}"></tf-button>
  `;

  diffHost.querySelector('#env-diff-apply')?.addEventListener('click', async () => {
    const selected = [...diffHost.querySelectorAll('tf-checkbox.env-diff-row[checked]')]
      .map((el) => el.dataset.value);
    const confirmEnvironmentName = requiresConfirmation ? (diffHost.querySelector('#env-diff-confirm-name')?.value ?? '') : null;
    try {
      const result = await ApiBinary.action('environmentImportApplyRequest', {
        pullId,
        confirmEnvironmentName,
        selectedResourceKeys: selected,
      });
      toast(t('toast_imported', { n: result.importedCount }), 'success');
      diffHost.innerHTML = '';
    } catch (err) {
      toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  });
}

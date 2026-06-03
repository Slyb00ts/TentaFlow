// =============================================================================
// Plik: modules/mesh-baseline-adopt.js
// Opis: Admin UI dla adopcji baseline'u (FAZA C krok 3b). Modal: wybor dawcy
//       z listy zaufanych peerow (BaselineDonorListRequest), start adopcji
//       (BaselineAdoptStartRequest), polling fazy (BaselineAdoptStatusRequest)
//       jako stepper, raport koncowy oraz odblokowanie zawieszonego stanu
//       (BaselineAdoptClearRequest). Wszystko przez binarny protokol CBOR.
// =============================================================================

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-modal.js';
import '/js/components/tf-progress-bar.js';
import '/js/components/tf-spinner.js';

// Kolejnosc faz dla steppera. `None` to brak stanu, `Completed` = koniec.
// Faza widoczna adminowi (Elected -> Receiving -> Importing -> Completed);
// `Imported` jest krotkim stanem przejsciowym tuz przed Completed.
const PHASE_ORDER = ['Elected', 'Receiving', 'Importing', 'Imported', 'Completed'];
const POLL_INTERVAL_MS = 1500;

let modalEl = null;
// tf-modal._build() (on connect) MOVES [slot=...] children into the shadow card
// and STRIPS their slot attribute, so querying `[slot="body"]` after connect
// returns null. We keep stable references to the body/footer nodes (identity
// survives the move) and re-render their innerHTML in place.
let bodyEl = null;
let footerEl = null;
let pollTimer = null;
let selectedDonorId = null;
let donorCandidates = [];
let onClosed = null;

// Faza, ktora uwazamy za "zawieszona" i pozwalamy odblokowac: kazda inna niz
// None/Completed. Backend odmowi (Conflict) gdy trwa aktywny transfer/import —
// wtedy pokazujemy toast i nie czyscimy lokalnie nic.
function isSuspendedPhase(phase) {
  return phase && phase !== 'None' && phase !== 'Completed';
}

/// Otwiera modal adopcji baseline. `onDone` wolane po zamknieciu (mesh.js
/// odswieza liste nodow). Bezpieczne do wielokrotnego wywolania — istniejacy
/// modal jest najpierw zamykany.
export async function openBaselineAdoptModal({ onDone } = {}) {
  closeModal();
  onClosed = typeof onDone === 'function' ? onDone : null;
  selectedDonorId = null;
  donorCandidates = [];

  modalEl = document.createElement('tf-modal');
  modalEl.setAttribute('variant', 'modal');
  modalEl.setAttribute('title', I18n.t('mesh.baseline_adopt_title'));

  bodyEl = document.createElement('div');
  bodyEl.setAttribute('slot', 'body');
  bodyEl.className = 'baseline-adopt-body';
  bodyEl.innerHTML = `<div class="baseline-loading"><tf-spinner size="md"></tf-spinner></div>`;
  modalEl.appendChild(bodyEl);

  // Footer slot must exist BEFORE connect so tf-modal._build() places it inside
  // the card. We re-render its content (buttons) per phase via setFooter().
  footerEl = document.createElement('div');
  footerEl.setAttribute('slot', 'footer');
  modalEl.appendChild(footerEl);

  modalEl.addEventListener('close', handleModalClose, { once: true });
  document.body.appendChild(modalEl);
  modalEl.setAttribute('open', '');

  // Najpierw sprawdz biezacy status — jezeli adopcja juz trwa lub zostala
  // zakonczona, wchodzimy od razu w widok postepu/raportu zamiast listy.
  let status = null;
  try {
    status = await ApiBinary.one('baselineAdoptStatusRequest');
  } catch (err) {
    // Brak uprawnien / blad protokolu — pokaz komunikat i pozwol zamknac.
    renderError(err);
    return;
  }

  if (status && isPhasePresent(status.phase)) {
    renderProgress(status);
    startPolling();
    return;
  }

  await loadAndRenderDonors();
}

function isPhasePresent(phase) {
  return phase && phase !== 'None';
}

// ---- Widok 1: wybor dawcy ------------------------------------------------

async function loadAndRenderDonors() {
  if (!bodyEl) return;
  bodyEl.innerHTML = `<div class="baseline-loading"><tf-spinner size="md"></tf-spinner></div>`;
  let resp = null;
  try {
    resp = await ApiBinary.one('baselineDonorListRequest');
  } catch (err) {
    renderError(err);
    return;
  }
  donorCandidates = Array.isArray(resp?.candidates) ? resp.candidates : [];
  renderDonorSelection();
}

function renderDonorSelection() {
  const body = bodyEl;
  if (!body) return;

  if (donorCandidates.length === 0) {
    body.innerHTML = `
      <p class="baseline-intro">${escapeHtml(I18n.t('mesh.baseline_adopt_intro'))}</p>
      <div class="baseline-empty">${escapeHtml(I18n.t('mesh.baseline_no_donors'))}</div>
    `;
    setFooter([
      { action: 'close', variant: 'secondary', label: I18n.t('common.close') },
    ]);
    return;
  }

  const rows = donorCandidates.map((c) => {
    const nodeId = c.nodeId || c.node_id || '';
    const name = c.displayName || c.display_name || nodeId.slice(0, 12);
    const trustedChip = c.trusted
      ? `<tf-chip status="ok" dot>${escapeHtml(I18n.t('mesh.baseline_trusted'))}</tf-chip>`
      : `<tf-chip status="warn" dot>${escapeHtml(I18n.t('mesh.baseline_untrusted'))}</tf-chip>`;
    // summary moze byc null — pokazujemy "—"/"nieznane" zamiast pustki.
    const s = c.summary;
    const summaryText = s
      ? I18n.t('mesh.baseline_summary_counts', {
          org: s.orgName || s.org_name || '—',
          users: Number(s.users ?? 0),
          flows: Number(s.flows ?? 0),
          roles: Number(s.roles ?? 0),
        })
      : I18n.t('mesh.baseline_summary_unknown');
    return `
      <tr class="baseline-donor-row${selectedDonorId === nodeId ? ' selected' : ''}" data-donor="${escapeAttr(nodeId)}" role="radio" aria-checked="${selectedDonorId === nodeId}" tabindex="0">
        <td class="baseline-donor-pick"><span class="baseline-pick-dot"></span></td>
        <td>
          <div class="baseline-donor-name">${escapeHtml(name)}</div>
          <div class="baseline-donor-id">${escapeHtml(nodeId.slice(0, 16))}…</div>
        </td>
        <td>${trustedChip}</td>
        <td class="baseline-donor-summary">${escapeHtml(summaryText)}</td>
      </tr>
    `;
  }).join('');

  body.innerHTML = `
    <p class="baseline-intro">${escapeHtml(I18n.t('mesh.baseline_adopt_intro'))}</p>
    <p class="baseline-warning">${escapeHtml(I18n.t('mesh.baseline_adopt_warning'))}</p>
    <div class="baseline-donor-table-wrap">
      <table class="baseline-donor-table" role="radiogroup">
        <thead>
          <tr>
            <th></th>
            <th>${escapeHtml(I18n.t('mesh.baseline_col_node'))}</th>
            <th>${escapeHtml(I18n.t('mesh.baseline_col_trust'))}</th>
            <th>${escapeHtml(I18n.t('mesh.baseline_col_summary'))}</th>
          </tr>
        </thead>
        <tbody>${rows}</tbody>
      </table>
    </div>
  `;

  // Selekcja dawcy: klik (lub Enter/Spacja) w wiersz aktualizuje wybor +
  // enabled stan przycisku startu. Wiersz dziala jak radio (role=radio).
  const selectRow = (row) => {
    selectedDonorId = row.dataset.donor || null;
    body.querySelectorAll('.baseline-donor-row').forEach((r) => {
      const on = r.dataset.donor === selectedDonorId;
      r.classList.toggle('selected', on);
      r.setAttribute('aria-checked', String(on));
    });
    syncStartButtonState();
  };
  body.querySelectorAll('.baseline-donor-row').forEach((row) => {
    row.addEventListener('click', () => selectRow(row));
    row.addEventListener('keydown', (e) => {
      if (e.key === ' ' || e.key === 'Enter') {
        e.preventDefault();
        selectRow(row);
      }
    });
  });

  setFooter([
    { action: 'close', variant: 'secondary', label: I18n.t('common.cancel') },
    { action: 'start', variant: 'primary', label: I18n.t('mesh.baseline_start') },
  ]);
  syncStartButtonState();
}

function syncStartButtonState() {
  const btn = footerEl?.querySelector('tf-button[data-action="start"]');
  if (!btn) return;
  if (selectedDonorId) btn.removeAttribute('disabled');
  else btn.setAttribute('disabled', '');
}

async function startAdoption() {
  if (!selectedDonorId) return;
  const btn = footerEl?.querySelector('tf-button[data-action="start"]');
  // Disabled w trakcie requestu chroni przed podwojnym startem (single-flight
  // i tak odrzuca po stronie hosta, ale UX lepszy bez podwojnego klikniecia).
  if (btn) btn.setAttribute('disabled', '');
  try {
    const resp = await ApiBinary.action('baselineAdoptStartRequest', {
      donorNodeId: selectedDonorId,
    });
    if (!resp?.started) {
      // single-flight zajety / odmowa — pokaz powod, zostan na liscie.
      toast(resp?.message || I18n.t('mesh.baseline_start_refused'), 'error');
      if (btn) btn.removeAttribute('disabled');
      return;
    }
    toast(I18n.t('mesh.baseline_started'), 'success');
    // Wchodzimy w widok postepu i startujemy polling.
    const status = await ApiBinary.one('baselineAdoptStatusRequest').catch(() => ({
      phase: 'Elected',
      peer: selectedDonorId,
    }));
    renderProgress(status);
    startPolling();
  } catch (err) {
    toast(`${I18n.t('mesh.baseline_start_error')}: ${err.message || err}`, 'error');
    if (btn) btn.removeAttribute('disabled');
  }
}

// ---- Widok 2: postep (stepper) -------------------------------------------

function renderProgress(status) {
  const body = bodyEl;
  if (!body) return;

  const phase = status?.phase || 'Elected';
  if (phase === 'Completed') {
    renderReport(status);
    return;
  }

  const activeIdx = PHASE_ORDER.indexOf(phase);
  const totalSteps = PHASE_ORDER.length - 1; // bez Completed (to widok raportu)
  const progressPct = activeIdx >= 0
    ? Math.min(100, Math.round(((activeIdx + 1) / totalSteps) * 100))
    : 0;

  const steps = PHASE_ORDER.filter((p) => p !== 'Completed').map((p) => {
    const idx = PHASE_ORDER.indexOf(p);
    let state = 'pending';
    if (idx < activeIdx) state = 'done';
    else if (idx === activeIdx) state = 'active';
    const dot = state === 'active'
      ? '<tf-spinner size="sm"></tf-spinner>'
      : `<span class="baseline-step-dot ${state}"></span>`;
    return `
      <div class="baseline-step ${state}">
        ${dot}
        <span class="baseline-step-label">${escapeHtml(I18n.t(`mesh.baseline_phase_${p.toLowerCase()}`))}</span>
      </div>
    `;
  }).join('');

  const peer = status?.peer || selectedDonorId || '';
  const peerName = donorNameFor(peer);

  body.innerHTML = `
    <p class="baseline-intro">${escapeHtml(I18n.t('mesh.baseline_progress_intro', { donor: peerName }))}</p>
    <div class="baseline-stepper">${steps}</div>
    <tf-progress-bar value="${progressPct}" tone="accent" size="md" label="${escapeAttr(I18n.t(`mesh.baseline_phase_${phase.toLowerCase()}`))}"></tf-progress-bar>
  `;

  setFooter([
    { action: 'close', variant: 'secondary', label: I18n.t('common.close') },
    { action: 'clear', variant: 'danger', label: I18n.t('mesh.baseline_clear') },
  ]);
}

// ---- Widok 3: raport koncowy ---------------------------------------------

function renderReport(status) {
  const body = bodyEl;
  if (!body) return;
  stopPolling();

  const r = status?.report;
  const rows = r
    ? [
        [I18n.t('mesh.baseline_report_merged'), Number(r.usersMergedByEmail ?? r.users_merged_by_email ?? 0)],
        [I18n.t('mesh.baseline_report_joined'), Number(r.usersJoinedDonorOrg ?? r.users_joined_donor_org ?? 0)],
        [I18n.t('mesh.baseline_report_collisions'), Number(r.collisionsSuffixed ?? r.collisions_suffixed ?? 0)],
      ]
    : [];

  const reportHtml = r
    ? `
      <table class="baseline-report-table">
        <tbody>
          ${rows.map(([label, val]) => `
            <tr>
              <td class="baseline-report-label">${escapeHtml(label)}</td>
              <td class="baseline-report-value">${escapeHtml(String(val))}</td>
            </tr>
          `).join('')}
        </tbody>
      </table>
    `
    : `<div class="baseline-empty">${escapeHtml(I18n.t('mesh.baseline_report_missing'))}</div>`;

  body.innerHTML = `
    <div class="baseline-done-head">
      <tf-chip status="ok" dot>${escapeHtml(I18n.t('mesh.baseline_phase_completed'))}</tf-chip>
    </div>
    <p class="baseline-intro">${escapeHtml(I18n.t('mesh.baseline_report_intro'))}</p>
    ${reportHtml}
  `;

  setFooter([
    { action: 'clear', variant: 'secondary', label: I18n.t('mesh.baseline_dismiss') },
    { action: 'close', variant: 'primary', label: I18n.t('common.close') },
  ]);
}

// ---- Odblokowanie zawieszonego stanu -------------------------------------

async function clearState() {
  try {
    const resp = await ApiBinary.one('baselineAdoptClearRequest');
    if (resp?.cleared) {
      toast(I18n.t('mesh.baseline_cleared'), 'success');
    } else {
      toast(resp?.message || I18n.t('mesh.baseline_clear_noop'), 'info');
    }
    stopPolling();
    // Po wyczyszczeniu wracamy do listy dawcow (mozna sprobowac ponownie).
    selectedDonorId = null;
    await loadAndRenderDonors();
  } catch (err) {
    // Conflict = aktywny transfer/import, nie mozna przerwac.
    if (err.code === 'Conflict') {
      toast(I18n.t('mesh.baseline_clear_conflict'), 'error');
    } else {
      toast(`${I18n.t('mesh.baseline_clear_error')}: ${err.message || err}`, 'error');
    }
  }
}

// ---- Polling -------------------------------------------------------------

function startPolling() {
  stopPolling();
  pollTimer = setInterval(async () => {
    if (!modalEl || !modalEl.isConnected) {
      stopPolling();
      return;
    }
    let status = null;
    try {
      status = await ApiBinary.one('baselineAdoptStatusRequest');
    } catch (err) {
      // Blad jednego ticka nie przerywa pollingu — odczekamy nastepny.
      console.warn('[baseline-adopt] status poll failed:', err?.message);
      return;
    }
    const phase = status?.phase || 'None';
    if (phase === 'None') {
      // Stan zniknal (np. zewnetrzne clear) — wroc do listy.
      stopPolling();
      await loadAndRenderDonors();
      return;
    }
    if (phase === 'Completed') {
      renderReport(status);
      return;
    }
    renderProgress(status);
  }, POLL_INTERVAL_MS);
}

function stopPolling() {
  if (pollTimer) {
    clearInterval(pollTimer);
    pollTimer = null;
  }
}

// ---- Footer / modal lifecycle --------------------------------------------

// Re-renderuje stopke modala z tf-button. `disabled` ustawiane potem przez
// syncStartButtonState / startAdoption.
function setFooter(actions) {
  if (!footerEl) return;
  footerEl.innerHTML = actions.map((a) => `
    <tf-button variant="${escapeAttr(a.variant || 'secondary')}" data-action="${escapeAttr(a.action)}">${escapeHtml(a.label || '')}</tf-button>
  `).join('');
  footerEl.querySelectorAll('tf-button[data-action]').forEach((btn) => {
    btn.addEventListener('click', () => handleFooterAction(btn.dataset.action));
  });
}

function handleFooterAction(action) {
  if (action === 'close') {
    closeModal();
    return;
  }
  if (action === 'start') {
    startAdoption();
    return;
  }
  if (action === 'clear') {
    clearState();
    return;
  }
}

function donorNameFor(nodeId) {
  const c = donorCandidates.find((x) => (x.nodeId || x.node_id) === nodeId);
  if (c) return c.displayName || c.display_name || nodeId.slice(0, 12);
  return nodeId ? `${nodeId.slice(0, 12)}…` : '—';
}

function renderError(err) {
  if (!bodyEl) return;
  bodyEl.innerHTML = `<div class="baseline-empty baseline-error">${escapeHtml(err?.message || I18n.t('mesh.baseline_load_error'))}</div>`;
  setFooter([{ action: 'close', variant: 'secondary', label: I18n.t('common.close') }]);
}

function handleModalClose() {
  stopPolling();
  modalEl = null;
  bodyEl = null;
  footerEl = null;
  const cb = onClosed;
  onClosed = null;
  if (cb) cb();
}

function closeModal() {
  stopPolling();
  if (modalEl) {
    // Zdejmujemy listener `close` zeby zamkniecie programowe nie wywolalo
    // handleModalClose drugi raz — callback onClosed odpalamy tutaj raz.
    modalEl.removeEventListener('close', handleModalClose);
    modalEl.removeAttribute('open');
    const el = modalEl;
    setTimeout(() => { if (el.isConnected) el.remove(); }, 300);
    modalEl = null;
  }
  bodyEl = null;
  footerEl = null;
  const cb = onClosed;
  onClosed = null;
  if (cb) cb();
}

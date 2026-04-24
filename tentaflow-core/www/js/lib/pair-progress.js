// =============================================================================
// Plik: lib/pair-progress.js
// Opis: Okno postepu parowania — zamiast modala "wpisz ID + PIN" od razu po
//       submit pokazujemy animowana timeline z 5 krokami: przygotowanie,
//       nawiazywanie laczonosci, handshake, synchronizacja kluczy, polaczono.
//       Kroki przewijaja sie wizualnie w trakcie RPC; finalny sukces/error
//       wpada z odpowiedzi backendu. Na sukcesie auto-close po 1.5s.
// Eksport: runPairProgress({ target, submit }) -> Promise<{ outcome, error? }>
// =============================================================================

import { I18n } from '/js/i18n.js';
import { escapeHtml } from '/js/utils.js';

const STEP_IDS = ['prepare', 'reach', 'handshake', 'keys', 'connected'];

const STEP_LABEL_KEY = {
  prepare:   'mesh.pair_progress_step_prepare',
  reach:     'mesh.pair_progress_step_reach',
  handshake: 'mesh.pair_progress_step_handshake',
  keys:      'mesh.pair_progress_step_keys',
  connected: 'mesh.pair_progress_step_connected',
};

const STEP_DETAIL_KEY = {
  prepare:   'mesh.pair_progress_detail_prepare',
  reach:     'mesh.pair_progress_detail_reach',
  handshake: 'mesh.pair_progress_detail_handshake',
  keys:      'mesh.pair_progress_detail_keys',
  connected: 'mesh.pair_progress_detail_connected',
};

// SVG fragments (minimal)
const SVG_DOT_SPIN = '<svg viewBox="0 0 24 24" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12a9 9 0 1 1-6.22-8.56"/></svg>';
const SVG_DOT_CHECK = '<svg viewBox="0 0 24 24" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>';
const SVG_DOT_X = '<svg viewBox="0 0 24 24" stroke-linecap="round" stroke-linejoin="round"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>';
const SVG_PEER_ICO = '<svg viewBox="0 0 24 24" stroke-linecap="round" stroke-linejoin="round"><rect x="4" y="4" width="16" height="16" rx="2"/><rect x="9" y="9" width="6" height="6"/><line x1="9" y1="2" x2="9" y2="4"/><line x1="15" y1="2" x2="15" y2="4"/><line x1="9" y1="20" x2="9" y2="22"/><line x1="15" y1="20" x2="15" y2="22"/><line x1="20" y1="9" x2="22" y2="9"/><line x1="20" y1="14" x2="22" y2="14"/><line x1="2" y1="9" x2="4" y2="9"/><line x1="2" y1="14" x2="4" y2="14"/></svg>';
const SVG_HEAD_ICO = '<svg viewBox="0 0 24 24" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><path d="M8 12h8"/><path d="M12 8v8"/></svg>';
const SVG_HEAD_OK = '<svg viewBox="0 0 24 24" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>';
const SVG_HEAD_ERR = '<svg viewBox="0 0 24 24" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>';
const SVG_BANNER_OK = '<svg viewBox="0 0 24 24" stroke-linecap="round" stroke-linejoin="round"><path d="M22 11.08V12a10 10 0 1 1-5.93-9.14"/><polyline points="22 4 12 14.01 9 11.01"/></svg>';
const SVG_BANNER_ERR = '<svg viewBox="0 0 24 24" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>';

/**
 * Otwiera okno postepu parowania i wykonuje `submit()` w tle, animujac kroki.
 *
 * @param {Object}   opts
 * @param {Object}   opts.target           — { hostname, nodeId } peera
 * @param {Function} opts.submit           — async () => Promise<any> ktora robi
 *                                            faktyczny pairing RPC. Powinna
 *                                            zwrocic descryptor wyniku albo
 *                                            throw z errorem.
 * @returns {Promise<{ outcome: 'confirmed'|'pending'|'cancelled'|'error', error? }>}
 */
export function runPairProgress({ target, submit }) {
  return new Promise((resolve) => {
    const win = document.createElement('tf-window');
    win.setAttribute('title', I18n.t('mesh.pair_progress_title'));
    win.setAttribute('buttons', 'close');
    win.setAttribute('width', '520');
    win.setAttribute('initial-x', 'center');
    win.setAttribute('initial-y', 'center');

    // Head icon — pod runtime zmieniamy na success/error/cancel.
    const headIcoEl = document.createElement('div');
    headIcoEl.slot = 'head-icon';
    headIcoEl.className = 'pair-progress__head-ico';
    headIcoEl.innerHTML = SVG_HEAD_ICO;

    const body = document.createElement('div');
    body.slot = 'body';
    body.className = 'pair-progress__body';

    const hostname = (target?.hostname || I18n.t('mesh.unknown_host')).toString();
    const nodeId = (target?.nodeId || target?.node_id || '').toString();

    body.innerHTML = `
      <div class="pair-progress__target">
        <div class="pair-progress__target-ico">${SVG_PEER_ICO}</div>
        <div class="pair-progress__target-meta">
          <div class="pair-progress__target-name">${escapeHtml(hostname)}</div>
          <div class="pair-progress__target-id">${escapeHtml(nodeId)}</div>
        </div>
      </div>
      <div class="pair-progress__steps">
        ${STEP_IDS.map(renderStep).join('')}
      </div>
      <div class="pair-progress__banner" hidden></div>
    `;
    win.appendChild(body);

    const foot = document.createElement('div');
    foot.slot = 'footer';
    foot.className = 'pair-progress__foot';
    foot.innerHTML = `
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('mesh.pair_progress_cancel'))}</tf-button>
    `;
    win.appendChild(foot);

    const backdrop = document.createElement('div');
    backdrop.className = 'tf-window-backdrop';
    document.body.append(backdrop, win);

    let settled = false;
    let tickerTimer = null;
    let autoCloseTimer = null;

    const cleanup = (result) => {
      if (settled) return;
      settled = true;
      if (tickerTimer) clearTimeout(tickerTimer);
      if (autoCloseTimer) clearTimeout(autoCloseTimer);
      win.remove();
      backdrop.remove();
      resolve(result);
    };

    win.addEventListener('action', (e) => {
      const a = e.detail?.action;
      if (a === 'cancel' || a === 'close') cleanup({ outcome: 'cancelled' });
    });

    // Animowane przewijanie krokow w trakcie RPC. Kazdy przejsciowy krok =
    // aktywny, poprzednie = done. Robimy to w sposob ciagly az do finalnego
    // sygnalu z submit().
    let currentIdx = 0;
    const setStep = (id, state, detail) => {
      const el = body.querySelector(`[data-step="${id}"]`);
      if (!el) return;
      el.classList.remove('pending', 'active', 'done', 'error');
      el.classList.add(state);
      const dot = el.querySelector('.step-dot');
      if (dot) {
        if (state === 'active') dot.innerHTML = SVG_DOT_SPIN;
        else if (state === 'done') dot.innerHTML = SVG_DOT_CHECK;
        else if (state === 'error') dot.innerHTML = SVG_DOT_X;
        else dot.innerHTML = '';
      }
      if (detail != null) {
        const det = el.querySelector('.step-detail');
        if (det) det.textContent = detail;
      }
    };

    const advance = () => {
      if (settled) return;
      if (currentIdx >= STEP_IDS.length - 1) return; // ostatni krok rezerwujemy dla final state
      if (currentIdx > 0) setStep(STEP_IDS[currentIdx - 1], 'done');
      setStep(STEP_IDS[currentIdx], 'active');
      currentIdx += 1;
      tickerTimer = setTimeout(advance, 550);
    };

    // Start: prepare jako active.
    setStep(STEP_IDS[0], 'active');
    currentIdx = 1;
    tickerTimer = setTimeout(advance, 450);

    // Uruchamiamy submit rownolegle. Czekamy na result albo error.
    Promise.resolve()
      .then(() => submit())
      .then((res) => {
        if (settled) return;
        // Wszystkie kroki = done.
        for (const id of STEP_IDS) setStep(id, 'done');
        clearTimeout(tickerTimer);
        // Head icon + banner: success / pending (gdy response Pending od strony
        // receivera zwrocony przez backend, co czasem sie dzieje gdy invite
        // PIN wygasl — traktujemy jak "wyslano, czeka").
        const pending = res && res.outcome === 'pending';
        headIcoEl.className = `pair-progress__head-ico ${pending ? 'pair-progress__head-ico--info' : 'pair-progress__head-ico--success'}`;
        headIcoEl.innerHTML = pending ? SVG_HEAD_ICO : SVG_HEAD_OK;
        win.setAttribute('title', pending
          ? I18n.t('mesh.pair_progress_title_pending')
          : I18n.t('mesh.pair_progress_title_success'));
        showBanner(body, pending ? 'info' : 'success', {
          icon: SVG_BANNER_OK,
          title: pending
            ? I18n.t('mesh.pair_progress_banner_pending_title')
            : I18n.t('mesh.pair_progress_banner_success_title'),
          desc: pending
            ? I18n.t('mesh.pair_progress_banner_pending_desc')
            : I18n.t('mesh.pair_progress_banner_success_desc'),
          countdown: pending ? null : 1.5,
        });
        // Zamien przycisk Cancel na Gotowe/OK.
        foot.innerHTML = `
          <tf-button variant="primary" data-action="confirm">${escapeHtml(I18n.t('common.ok'))}</tf-button>
        `;
        if (!pending) {
          autoCloseTimer = setTimeout(() => cleanup({ outcome: 'confirmed', result: res }), 1500);
        }
      })
      .catch((err) => {
        if (settled) return;
        clearTimeout(tickerTimer);
        // Oznacz aktywny krok jako error, reszta pending.
        const errIdx = Math.max(0, Math.min(currentIdx - 1, STEP_IDS.length - 1));
        for (let i = 0; i < STEP_IDS.length; i++) {
          if (i < errIdx) setStep(STEP_IDS[i], 'done');
          else if (i === errIdx) setStep(STEP_IDS[i], 'error', err?.message || String(err || ''));
          else setStep(STEP_IDS[i], 'pending');
        }
        headIcoEl.className = 'pair-progress__head-ico pair-progress__head-ico--error';
        headIcoEl.innerHTML = SVG_HEAD_ERR;
        win.setAttribute('title', I18n.t('mesh.pair_progress_title_error'));
        showBanner(body, 'error', {
          icon: SVG_BANNER_ERR,
          title: I18n.t('mesh.pair_progress_banner_error_title'),
          desc: err?.message || I18n.t('mesh.pair_failed'),
        });
        foot.innerHTML = `
          <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
          <tf-button variant="primary" data-action="retry">${escapeHtml(I18n.t('mesh.pair_progress_retry'))}</tf-button>
        `;
      });
  });
}

function renderStep(id) {
  return `
    <div class="pair-step pending" data-step="${id}">
      <div class="step-dot"></div>
      <div class="step-body">
        <div class="step-label">${escapeHtml(I18n.t(STEP_LABEL_KEY[id]))}</div>
        <div class="step-detail">${escapeHtml(I18n.t(STEP_DETAIL_KEY[id]))}</div>
      </div>
    </div>
  `;
}

function showBanner(body, kind, { icon, title, desc, countdown }) {
  const el = body.querySelector('.pair-progress__banner');
  if (!el) return;
  el.className = `pair-progress__banner pair-progress__banner--${kind}`;
  el.innerHTML = `
    ${icon || ''}
    <div class="bar-inner">
      <b>${escapeHtml(title || '')}</b>
      <span>${escapeHtml(desc || '')}</span>
    </div>
    ${countdown != null ? `<span class="countdown">${countdown.toFixed(1)}s</span>` : ''}
  `;
  el.hidden = false;
  if (countdown != null) {
    let remaining = countdown;
    const cd = el.querySelector('.countdown');
    const iv = setInterval(() => {
      remaining -= 0.1;
      if (remaining <= 0) { clearInterval(iv); return; }
      if (cd) cd.textContent = `${remaining.toFixed(1)}s`;
    }, 100);
  }
}

export default runPairProgress;

// =============================================================================
// Plik: modules/connection-overlay.js
// Opis: Overlay "Utracono połączenie" — wyswietla blur + modal gdy BinaryWsClient
//       traci kontakt z daemonem. Countdown do nastepnej proby, manual "Spróbuj
//       teraz" button, status log. Automatycznie znika po reconnect.
// Przyklad: init() wywolywane raz z app.js; potem nic nie trzeba robic, overlay
//       sam reaguje na lifecycle eventy z ApiBinary.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';

const MAX_LOG_ENTRIES = 30;
const AUTO_HIDE_DELAY_MS = 500;

let mounted = false;
let el = null;
let shell = null;
let ring = null;
let countdownEl = null;
let attemptEl = null;
let logEl = null;
let hideTimer = null;

let state = 'ok'; // ok | disconnected | reconnecting
let nextAttemptAt = 0;
let ringMax = 1;
let raf = null;

function timeStr() {
  return new Date().toTimeString().slice(0, 8);
}

function addLogEntry(kind, msg) {
  if (!logEl) return;
  const entry = document.createElement('div');
  entry.className = `conn-log-entry ${kind}`;
  const ts = document.createElement('span');
  ts.className = 'ts';
  ts.textContent = timeStr();
  const m = document.createElement('span');
  m.className = 'msg';
  m.textContent = msg;
  entry.appendChild(ts);
  entry.appendChild(m);
  logEl.appendChild(entry);
  while (logEl.children.length > MAX_LOG_ENTRIES) {
    logEl.removeChild(logEl.firstChild);
  }
  logEl.scrollTop = logEl.scrollHeight;
}

function build() {
  el = document.createElement('div');
  el.className = 'conn-overlay';
  el.setAttribute('aria-live', 'assertive');
  el.innerHTML = `
    <div class="conn-overlay-card" role="dialog" aria-labelledby="conn-overlay-title">
      <div class="conn-overlay-head">
        <div class="dot"></div>
        <h3 id="conn-overlay-title">${escapeHtml(I18n.t('connection.title'))}</h3>
      </div>
      <div class="conn-overlay-body">
        <div class="conn-overlay-icon">
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M1 1l22 22"/>
            <path d="M16.72 11.06A10.94 10.94 0 0 1 19 12.55"/>
            <path d="M5 12.55a10.94 10.94 0 0 1 5.17-2.39"/>
            <path d="M10.71 5.05A16 16 0 0 1 22.58 9"/>
            <path d="M1.42 9a15.91 15.91 0 0 1 4.7-2.88"/>
            <path d="M8.53 16.11a6 6 0 0 1 6.95 0"/>
            <line x1="12" y1="20" x2="12.01" y2="20"/>
          </svg>
        </div>
        <div class="conn-overlay-heading">${escapeHtml(I18n.t('connection.heading'))}</div>
        <div class="conn-overlay-desc">${escapeHtml(I18n.t('connection.description'))}</div>

        <div class="conn-retry">
          <div class="conn-retry-ring">
            <svg viewBox="0 0 50 50" aria-hidden="true">
              <circle class="track" cx="25" cy="25" r="20"/>
              <circle class="fill" cx="25" cy="25" r="20"/>
            </svg>
            <div class="countdown">–</div>
          </div>
          <div class="conn-retry-info">
            <div class="line-1">${escapeHtml(I18n.t('connection.next_attempt'))}</div>
            <div class="line-2">${escapeHtml(I18n.t('connection.attempt_hint', { attempt: 0 }))}</div>
          </div>
        </div>

        <div class="conn-log tf-scroll" role="log"></div>
      </div>
      <div class="conn-overlay-foot">
        <button class="tf-btn tf-btn-ghost" data-action="logout">
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4"/><polyline points="16 17 21 12 16 7"/><line x1="21" y1="12" x2="9" y2="12"/></svg>
          <span>${escapeHtml(I18n.t('connection.btn_logout'))}</span>
        </button>
        <button class="tf-btn tf-btn-primary" data-action="retry-now">
          <svg viewBox="0 0 24 24" aria-hidden="true"><polyline points="23 4 23 10 17 10"/><path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"/></svg>
          <span>${escapeHtml(I18n.t('connection.btn_retry_now'))}</span>
        </button>
      </div>
    </div>
  `;

  document.body.appendChild(el);

  const card = el.querySelector('.conn-overlay-card');
  ring = card.querySelector('.conn-retry-ring .fill');
  countdownEl = card.querySelector('.conn-retry-ring .countdown');
  attemptEl = card.querySelector('.conn-retry-info .line-2');
  logEl = card.querySelector('.conn-log');

  // Inicjalny stroke-dasharray dla pierscienia (r=20 → circ = 2π·20 ≈ 125.66).
  const circ = 2 * Math.PI * 20;
  if (ring) {
    ring.setAttribute('stroke-dasharray', String(circ));
    ring.style.strokeDashoffset = '0';
  }

  card.querySelector('[data-action="retry-now"]').addEventListener('click', () => {
    addLogEntry('info', I18n.t('connection.log_manual_retry'));
    ApiBinary.reconnectNow();
  });

  card.querySelector('[data-action="logout"]').addEventListener('click', () => {
    ApiBinary.clearSession();
    window.location.href = '/';
  });

  // W widocznosci strony odswiez ring gladko — requestAnimationFrame raz na sekund.
  const tick = () => {
    if (state === 'disconnected' && nextAttemptAt > 0) {
      const remainingMs = Math.max(0, nextAttemptAt - Date.now());
      const seconds = Math.ceil(remainingMs / 1000);
      if (countdownEl) countdownEl.textContent = seconds > 0 ? `${seconds}s` : '…';
      if (ring && ringMax > 0) {
        const frac = remainingMs / ringMax;
        ring.style.strokeDashoffset = String(circ * (1 - frac));
      }
    }
    raf = requestAnimationFrame(tick);
  };
  raf = requestAnimationFrame(tick);
}

function show() {
  if (!el) return;
  el.classList.add('visible');
  // Blur glownej aplikacji — najlepiej przez klase na root (#app) albo body.
  const app = document.getElementById('app') || document.body;
  app.classList.add('app-blurred');
  if (hideTimer) {
    clearTimeout(hideTimer);
    hideTimer = null;
  }
}

function hide() {
  if (!el) return;
  // Fade out po delayu (zeby "Reconnected" animacja mogla sie zrobic).
  if (hideTimer) clearTimeout(hideTimer);
  hideTimer = setTimeout(() => {
    el.classList.remove('visible');
    const app = document.getElementById('app') || document.body;
    app.classList.remove('app-blurred');
  }, AUTO_HIDE_DELAY_MS);
}

function escapeHtml(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) => (
    { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]
  ));
}

/** Przeformatuj line-2 tekst z liczba probe. */
function updateAttemptLine(attempt) {
  if (!attemptEl) return;
  attemptEl.textContent = I18n.t('connection.attempt_hint', { attempt });
}

/**
 * Publiczne API modułu — init() podpina sie do ApiBinary lifecycle.
 */
export function init() {
  if (mounted) return;
  mounted = true;
  build();

  ApiBinary.onLifecycle((ev) => {
    switch (ev.type) {
      case 'disconnected':
        state = 'disconnected';
        addLogEntry('err', I18n.t('connection.log_lost', { reason: ev.info?.reason ?? '' }));
        show();
        break;
      case 'reconnect-scheduled':
        state = 'disconnected';
        nextAttemptAt = Date.now() + (ev.info?.delayMs ?? 0);
        ringMax = ev.info?.delayMs ?? 1;
        updateAttemptLine(ev.info?.attempt ?? 0);
        addLogEntry('warn', I18n.t('connection.log_retry_scheduled', {
          attempt: ev.info?.attempt ?? 0,
          delay: Math.round((ev.info?.delayMs ?? 0) / 1000),
        }));
        show();
        break;
      case 'reconnect-attempt':
        addLogEntry('info', I18n.t('connection.log_retry_attempt', { attempt: ev.info?.attempt ?? 0 }));
        break;
      case 'open':
        if (state !== 'ok') {
          addLogEntry('ok', I18n.t('connection.log_restored'));
          state = 'ok';
          hide();
        }
        break;
      case 'close':
        // Close przez user intent (setJwt/clearSession) — nie pokazuj overlay.
        if (ev.info?.local) break;
        // Inaczej traktuj jako disconnected (backup — powinno leciec tez 'disconnected').
        if (state === 'ok') {
          state = 'disconnected';
          show();
        }
        break;
      default:
        break;
    }
  });
}

/** Destroy — do testow / HMR. */
export function destroy() {
  if (!mounted) return;
  if (raf) cancelAnimationFrame(raf);
  if (hideTimer) clearTimeout(hideTimer);
  if (el && el.parentNode) el.parentNode.removeChild(el);
  el = null;
  mounted = false;
}

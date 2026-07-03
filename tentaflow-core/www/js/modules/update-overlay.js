// =============================================================================
// Plik: modules/update-overlay.js
// Opis: Okno "Nowa wersja aplikacji" — modal w stylu connection-overlay. Pojawia
//       sie gdy handshake WS wykryje, ze front nie zgadza sie z backendem (zmiana
//       zasobow lub twardy mismatch protokolu po aktualizacji backendu/addonu).
//       "Odśwież" robi pelny hard-reload (kasuje service worker + cache), zeby
//       uzytkownik nie musial tego robic recznie. "Później" pomija (poza sesja).
//       Twardy mismatch ma mocniejszy wariant "Wymagana aktualizacja".
// Przyklad: init() wywolywane raz z app.js; potem sam reaguje na lifecycle event.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';

let mounted = false;
let el = null;
// Dismiss trzymany per hash serwera — pominiecie jednej wersji nie ukrywa okna
// dla kolejnej, nowszej wersji.
let dismissedHash = null;
let currentServerHash = null;

function escapeHtml(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) => (
    { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]
  ));
}

function t(key, fallback, vars) {
  const v = I18n.t(key, vars);
  return v === key ? fallback : v;
}

function shortHash(h) {
  return String(h ?? '').slice(0, 8) || '—';
}

function build() {
  el = document.createElement('div');
  el.className = 'update-overlay';
  el.setAttribute('aria-live', 'polite');
  el.innerHTML = `
    <div class="update-overlay-card" role="dialog" aria-labelledby="update-overlay-title">
      <div class="update-overlay-head">
        <span class="dot"></span>
        <h3 id="update-overlay-title"></h3>
        <span class="ver">TentaFlow</span>
      </div>
      <div class="update-overlay-body">
        <div class="update-overlay-icon">
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <g class="arrow">
              <line x1="12" y1="3" x2="12" y2="15"></line>
              <polyline points="7 10 12 15 17 10"></polyline>
            </g>
            <path d="M4 17v2a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-2"></path>
          </svg>
        </div>
        <div class="update-overlay-heading"></div>
        <div class="update-overlay-desc"></div>
        <div class="update-overlay-vercmp">
          <div class="spin">
            <svg viewBox="0 0 50 50" aria-hidden="true">
              <circle class="track" cx="25" cy="25" r="20"/>
              <circle class="fill" cx="25" cy="25" r="20"/>
            </svg>
          </div>
          <div class="info">
            <div class="l1"></div>
            <div class="l2">
              <span class="old"></span>
              <span class="arrow2">→</span>
              <span class="new"></span>
            </div>
          </div>
        </div>
      </div>
      <div class="update-overlay-foot">
        <tf-button size="sm" variant="ghost" data-action="dismiss">${escapeHtml(t('update.later', 'Później'))}</tf-button>
        <tf-button size="sm" variant="primary" icon="refresh" data-action="reload">${escapeHtml(t('update.reload', 'Odśwież'))}</tf-button>
      </div>
    </div>
  `;
  document.body.appendChild(el);

  el.querySelector('[data-action="reload"]').addEventListener('click', () => hardReload());
  el.querySelector('[data-action="dismiss"]').addEventListener('click', () => {
    dismissedHash = currentServerHash;
    hide();
  });
}

function show(info) {
  if (!el) return;
  const required = !!info?.required;
  el.classList.toggle('required', required);
  el.querySelector('#update-overlay-title').textContent = required
    ? t('update.title_required', 'Wymagana aktualizacja')
    : t('update.title', 'Nowa wersja aplikacji');
  el.querySelector('.update-overlay-heading').textContent = required
    ? t('update.title_required', 'Wymagana aktualizacja')
    : t('update.title', 'Nowa wersja aplikacji');
  el.querySelector('.update-overlay-desc').textContent = required
    ? t('update.desc_required', 'Front jest niezgodny z serwerem. Odśwież, aby kontynuować.')
    : t('update.desc', 'Serwer został zaktualizowany. Odśwież, aby załadować najnowszą wersję.');
  el.querySelector('.update-overlay-vercmp .l1').textContent = required
    ? t('update.cmp_required', 'Zmiana protokołu komunikacji')
    : t('update.cmp', 'Aktualizacja gotowa do instalacji');
  el.querySelector('.update-overlay-vercmp .old').textContent = shortHash(info?.current);
  el.querySelector('.update-overlay-vercmp .new').textContent = shortHash(info?.server);
  el.classList.add('visible');
}

function hide() {
  if (el) el.classList.remove('visible');
}

/** Pelny hard-reload: usun NASZ service worker + cache, potem przeladuj z sieci.
 *  Filtrujemy po scriptURL (/sw.js) i prefiksie 'tentaflow-', zeby nie ruszac
 *  ewentualnych innych SW/cache na tym samym originie. */
async function hardReload() {
  try {
    if ('serviceWorker' in navigator) {
      const regs = await navigator.serviceWorker.getRegistrations();
      await Promise.all(
        regs
          .filter((r) => {
            const url = r.active?.scriptURL || r.installing?.scriptURL || r.waiting?.scriptURL || '';
            return url.endsWith('/sw.js');
          })
          .map((r) => r.unregister()),
      );
    }
    if (window.caches) {
      const keys = await caches.keys();
      await Promise.all(
        keys.filter((k) => k.startsWith('tentaflow-')).map((k) => caches.delete(k)),
      );
    }
  } catch { /* ignore — reload i tak pobierze swiezy front */ }
  window.location.reload();
}

/** Wspolny handler — zrodlo sygnalu (handshake WS albo DOM event) nie ma znaczenia. */
function handleUpdate(info) {
  currentServerHash = info?.server ?? null;
  // Twardy mismatch pokazujemy zawsze; miekki pomijamy jesli user go odrzucil
  // dla tego samego hasha serwera.
  if (!info?.required && currentServerHash && currentServerHash === dismissedHash) {
    return;
  }
  show(info);
}

/** init() — podpina sie do ApiBinary lifecycle ('update-available') oraz do DOM
 *  eventu 'tf:update-available' (druga, niezalezna od WS sciezka sygnalu). */
export function init() {
  if (mounted) return;
  mounted = true;
  build();

  ApiBinary.onLifecycle((ev) => {
    if (ev.type === 'update-available') handleUpdate(ev.info);
  });
  window.addEventListener('tf:update-available', (ev) => handleUpdate(ev.detail));
}

/** Destroy — do testow / HMR. */
export function destroy() {
  if (!mounted) return;
  if (el && el.parentNode) el.parentNode.removeChild(el);
  el = null;
  mounted = false;
}

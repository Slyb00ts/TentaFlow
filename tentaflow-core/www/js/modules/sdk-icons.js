// =============================================================================
// Plik: modules/sdk-icons.js
// Opis: Renderer ikon SDK. IconName (snake_case z Rust enum IconName w
//       theme.rs) → <svg><use href="/img/icons.svg#icon-<id>"/>.
//       Renderer korzysta z external SVG sprite — przeglądarka pobiera plik
//       raz, kolejne use'y reużywają cache. currentColor pozwala na styling
//       przez `color: var(--sdk-color-*)` na elemencie rodzica.
// Przykład:
//   import { renderIcon } from '/js/modules/sdk-icons.js';
//   container.appendChild(renderIcon('search', { size: 20, ariaLabel: 'Szukaj' }));
// =============================================================================

const ICON_SPRITE_URL = '/img/icons.svg';
const SVG_NS = 'http://www.w3.org/2000/svg';
const XLINK_NS = 'http://www.w3.org/1999/xlink';

// Mapowanie IconName (snake_case z Rust) -> id symbolu w sprite.
// Każdy wariant `IconName` w `tentaflow-core/src/addon/ui/theme.rs` musi mieć
// odpowiadający wpis tutaj oraz <symbol id="icon-X"> w icons.svg.
const ICON_NAME_MAP = {
  // Navigation
  home: 'icon-home',
  dashboard: 'icon-dashboard',
  cameras: 'icon-cameras',
  alarms: 'icon-alarms',
  profiles: 'icon-profiles',
  models: 'icon-models',
  zones: 'icon-zones',
  audit: 'icon-audit',
  evidence: 'icon-evidence',
  settings: 'icon-settings',
  help: 'icon-help',

  // Actions
  add: 'icon-add',
  edit: 'icon-edit',
  delete: 'icon-delete',
  save: 'icon-save',
  cancel: 'icon-cancel',
  search: 'icon-search',
  filter: 'icon-filter',
  refresh: 'icon-refresh',
  more: 'icon-more',
  close: 'icon-close',
  check: 'icon-check',

  // Data / Content
  video: 'icon-video',
  image: 'icon-image',
  person: 'icon-person',
  vehicle: 'icon-vehicle',
  face: 'icon-face',
  document: 'icon-document',
  file: 'icon-file',
  folder: 'icon-folder',
  code: 'icon-code',

  // Status
  success: 'icon-success',
  warning: 'icon-warning',
  danger: 'icon-danger',
  info: 'icon-info',
  locked: 'icon-locked',
  unlocked: 'icon-unlocked',
  eye: 'icon-eye',
  eye_off: 'icon-eye-off',

  // Direction
  arrow_up: 'icon-arrow-up',
  arrow_down: 'icon-arrow-down',
  arrow_left: 'icon-arrow-left',
  arrow_right: 'icon-arrow-right',
  chevron_up: 'icon-chevron-up',
  chevron_down: 'icon-chevron-down',
  chevron_left: 'icon-chevron-left',
  chevron_right: 'icon-chevron-right',

  // System
  power: 'icon-power',
  settings2: 'icon-settings2',
  user: 'icon-user',
  users: 'icon-users',
  logout: 'icon-logout',
  bell: 'icon-bell',
  star: 'icon-star',
};

// Mapowanie nazwanych rozmiarów na klasy CSS — pozwala stylom z `sdk-theme.css`
// definiować rozmiar bez konieczności inline `width`/`height` na każdym <svg>.
const SIZE_CLASS = {
  sm: 'sdk-icon-sm',
  md: 'sdk-icon-md',
  lg: 'sdk-icon-lg',
  xl: 'sdk-icon-xl',
};

/**
 * Renderuje IconName jako element <svg> z referencją do sprite'u.
 * @param {string} name  Nazwa ikony w snake_case (IconName z Rust)
 * @param {Object} [options]
 * @param {number|string} [options.size]      Rozmiar w px albo klucz: sm/md/lg/xl. Domyślnie 'md'.
 * @param {string}        [options.className] Dodatkowa klasa CSS.
 * @param {string}        [options.color]     Nazwa zmiennej `--sdk-color-<color>` (np. 'primary', 'danger').
 * @param {string}        [options.ariaLabel] Tekst dla czytników. Bez tego ikona jest aria-hidden.
 * @returns {SVGSVGElement|null}
 */
export function renderIcon(name, options = {}) {
  if (!name) return null;

  const { size = 'md', className = '', color = null, ariaLabel = null } = options;

  let symbolId = ICON_NAME_MAP[name];
  if (!symbolId) {
    // Nieznana nazwa — logujemy raz i fallbackujemy na ikonę "help" (znak zapytania).
    // Nie rzucamy wyjątku, bo addon nie powinien wywrócić całego UI błędną nazwą.
    if (!renderIcon._warned) renderIcon._warned = new Set();
    if (!renderIcon._warned.has(name)) {
      renderIcon._warned.add(name);
      console.warn('[sdk-icons] Nieznana nazwa ikony:', name);
    }
    symbolId = ICON_NAME_MAP.help;
  }

  const svg = document.createElementNS(SVG_NS, 'svg');

  // Rozmiar: liczba/'NNpx' -> inline; klucz sm/md/lg/xl -> klasa CSS.
  let sizeClass = '';
  if (typeof size === 'number') {
    svg.setAttribute('width', String(size));
    svg.setAttribute('height', String(size));
  } else if (SIZE_CLASS[size]) {
    sizeClass = SIZE_CLASS[size];
  } else if (typeof size === 'string') {
    // Dowolny string (np. '18') traktujemy jako px.
    svg.setAttribute('width', size);
    svg.setAttribute('height', size);
  }

  svg.setAttribute('class', ['sdk-icon', sizeClass, className].filter(Boolean).join(' '));
  svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('focusable', 'false');

  if (color) {
    svg.style.color = `var(--sdk-color-${color}, currentColor)`;
  }

  if (ariaLabel) {
    svg.setAttribute('role', 'img');
    svg.setAttribute('aria-label', ariaLabel);
  } else {
    svg.setAttribute('aria-hidden', 'true');
  }

  const use = document.createElementNS(SVG_NS, 'use');
  use.setAttribute('href', `${ICON_SPRITE_URL}#${symbolId}`);
  // xlink:href dla starych silników WebKit (Safari < 12) — koszt zerowy.
  use.setAttributeNS(XLINK_NS, 'xlink:href', `${ICON_SPRITE_URL}#${symbolId}`);
  svg.appendChild(use);

  return svg;
}

/**
 * Czy podana nazwa jest znanym IconName? Przydatne przy walidacji payloadów.
 */
export function hasIcon(name) {
  return typeof name === 'string' && Object.prototype.hasOwnProperty.call(ICON_NAME_MAP, name);
}

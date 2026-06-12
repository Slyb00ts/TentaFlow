// =============================================================================
// Plik: sdk-runtime/icon-renderer.js
// Opis: Renderer `IconRef` (catalog §1.5 inline.rs). Wspólny moduł dla
// wszystkich grup komponentów, które przyjmują ikonki (Button, IconButton,
// LinkButton, Link, Fab, NavTab, BreadcrumbItem, MenuItem, ListItem,
// SidebarItem itd.). Faza 6 Krok 3.3b-2.
//
// IconRef ma 2 warianty:
//   - Named { name: IconName, size?, tone? }     — ikona z named sprite
//   - Asset { ref_, size_px?, alt? }              — addon-supplied URL
//
// Named: renderowane jako <svg> z `<use href="#tf-icon-<name>">` —
// głównej dashboard SVG sprite (zarejestrowany w `index.html`). Sprite'a
// nie ładuje ten moduł; addon korzysta z 142 zarezerwowanych nazw.
// Asset: <img src="..."> z URL-scheme allowlist + `alt` jako wymaganym
// dla a11y (gdy alt absent, używamy aria-hidden=true).
// =============================================================================

// Whitelist 142 nazw z spec'u `protocol/ui/icon_name.rs`. Pełne odbicie
// — addon nie może wymyślić nazwy (decoder spec'a też odrzuca).
export const ICON_NAMES = new Set([
  'add', 'alarms', 'alert', 'apps', 'arrow', 'arrow_down', 'arrow_left',
  'arrow_out', 'arrow_right', 'arrow_up', 'audit', 'ban', 'bar_chart',
  'bell', 'bolt', 'brain', 'branch', 'cameras', 'cancel', 'catalog',
  'chart_line', 'chat', 'check', 'chevron_down', 'chevron_left',
  'chevron_right', 'chevron_up', 'chip', 'clock', 'clock_glance',
  'close', 'cloud', 'cluster', 'code', 'collapse', 'copy', 'core', 'cpu',
  'cylinder', 'danger', 'dashboard', 'database', 'delete', 'desktop',
  'docker', 'document', 'download', 'edit', 'evidence', 'external_link',
  'eye', 'eye_off', 'face', 'file', 'file_text', 'filter', 'flow',
  'folder', 'globe', 'globe_grid', 'gpu', 'grid_rows', 'grip', 'help',
  'home', 'home_simple', 'host', 'iface_lan', 'iface_loop', 'iface_tb',
  'iface_virt', 'iface_vpn', 'iface_wifi', 'image', 'info', 'key',
  'line_chart', 'list', 'lock', 'locked', 'logout', 'management', 'max',
  'meeting', 'message', 'mic', 'min', 'model', 'models', 'more',
  'network', 'network_svg', 'os', 'paperclip', 'pause', 'person', 'pi',
  'pin', 'play', 'plus', 'power', 'profiles', 'prompt', 'puzzle',
  'question', 'rag_db', 'ram', 'record', 'record_dot', 'refresh',
  'registry', 'rotate', 'rules', 'save', 'search', 'send', 'services',
  'settings', 'settings2', 'share', 'shield', 'sparkle', 'speaker', 'speaker_alt',
  'star', 'stop', 'success', 'transform', 'trash', 'trend', 'unlock',
  'unlocked', 'user', 'users', 'vehicle', 'video', 'volume', 'warning',
  'workflow_app', 'x', 'zap', 'zones',
]);

const ICON_SIZES = new Set(['xs', 'sm', 'md', 'lg', 'xl']);
const TONES = new Set([
  'neutral', 'primary', 'success', 'warning', 'critical', 'info', 'muted',
]);

const NAMED_KEYS = new Set(['kind', 'name', 'size', 'tone']);
const ASSET_KEYS = new Set(['kind', 'ref', 'size_px', 'alt']);

// URL scheme allowlist dla asset.ref:
//   - relatywne ścieżki (np. `/addon/icons/foo.svg`) — bez scheme'u na wire
//   - `https:` (produkcja)
//   - `http:` (dev environments; produkcyjny dispatcher może to zaostrzyć)
//   - `data:image/{png,jpeg,webp,gif};base64,...` — raster only, patrz
//     `SAFE_ASSET_DATA_PREFIX`
// Wszystko inne (`javascript:`, `file:`, `data:image/svg+xml`, ...) → reject.
const SAFE_ASSET_SCHEMES = new Set(['https:', 'http:']);
// SVG nie jest dozwolony jako `data:` URI bo to active document i może
// zawierać <script>/<foreignObject> w niezaufanym addon-content. Tylko
// raster + `;base64,` żeby ograniczyć inne escape vectors.
const SAFE_ASSET_DATA_PREFIX = /^data:image\/(png|jpeg|webp|gif);base64,/i;

function assertOnlyKnownObjectKeys(obj, allowedKeys, ctx) {
  for (const k of Object.keys(obj)) {
    if (!allowedKeys.has(k)) {
      throw new TypeError(`${ctx}: unexpected key '${k}'`);
    }
  }
}

/// Tworzy <svg> albo <img> Element dla `IconRef`. Element ma już
/// ustawione semantic classy / aria-hidden / aria-label zgodnie ze
/// spec'em. Nie attachuje listener'ów ani binding'ów — kaller (renderer
/// Button-a itd.) decyduje gdzie wstawić zwrócony Element.
export function renderIcon(iconRef, ctx) {
  if (!iconRef || typeof iconRef !== 'object') {
    throw new TypeError(`${ctx}: IconRef must be object`);
  }
  if (iconRef.kind === 'named') {
    assertOnlyKnownObjectKeys(iconRef, NAMED_KEYS, `${ctx}.named`);
    const name = iconRef.name;
    if (typeof name !== 'string' || !ICON_NAMES.has(name)) {
      throw new TypeError(
        `${ctx}.named.name: unknown icon '${name}' (not in spec whitelist)`
      );
    }
    const size = iconRef.size;
    if (size != null && !ICON_SIZES.has(size)) {
      throw new TypeError(`${ctx}.named.size: invalid '${size}'`);
    }
    const tone = iconRef.tone;
    if (tone != null && !TONES.has(tone)) {
      throw new TypeError(`${ctx}.named.tone: invalid '${tone}'`);
    }
    const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    svg.classList.add('tf-icon');
    svg.classList.add(`tf-icon--name-${name}`);
    if (size) svg.classList.add(`tf-icon--size-${size}`);
    if (tone) svg.classList.add(`tf-icon--tone-${tone}`);
    svg.setAttribute('aria-hidden', 'true'); // domyślnie decorative; aria-label przychodzi z parent'a
    svg.setAttribute('focusable', 'false');
    const use = document.createElementNS('http://www.w3.org/2000/svg', 'use');
    // Sprite contract per `www/img/icons.svg` header: symbol id `icon-K`
    // gdzie K = wire name z `_` zamienionym na `-`. href format:
    // `/img/icons.svg#icon-arrow-down` itd.
    const spriteKey = name.replace(/_/g, '-');
    use.setAttribute('href', `/img/icons.svg#icon-${spriteKey}`);
    svg.appendChild(use);
    return svg;
  }
  if (iconRef.kind === 'asset') {
    assertOnlyKnownObjectKeys(iconRef, ASSET_KEYS, `${ctx}.asset`);
    const ref = iconRef.ref;
    if (typeof ref !== 'string' || ref.length === 0) {
      throw new TypeError(`${ctx}.asset.ref must be non-empty string`);
    }
    const safeSrc = assertSafeAssetSrc(ref, `${ctx}.asset.ref`);
    let sizePx = iconRef.size_px;
    if (sizePx != null) {
      if (typeof sizePx === 'bigint') {
        if (sizePx <= 0n || sizePx > 0xFFFFn) {
          throw new TypeError(`${ctx}.asset.size_px: must be u16 > 0`);
        }
        sizePx = Number(sizePx);
      } else if (!Number.isInteger(sizePx) || sizePx <= 0 || sizePx > 0xFFFF) {
        throw new TypeError(`${ctx}.asset.size_px: must be u16 > 0`);
      }
    }
    const alt = iconRef.alt;
    if (alt != null && typeof alt !== 'string') {
      throw new TypeError(`${ctx}.asset.alt: must be string`);
    }
    const img = document.createElement('img');
    img.classList.add('tf-icon', 'tf-icon--asset');
    img.setAttribute('src', safeSrc);
    if (sizePx != null) {
      img.setAttribute('width', String(sizePx));
      img.setAttribute('height', String(sizePx));
    }
    if (alt != null && alt.length > 0) {
      img.setAttribute('alt', alt);
    } else {
      img.setAttribute('alt', '');
      img.setAttribute('aria-hidden', 'true');
    }
    img.setAttribute('loading', 'lazy');
    img.setAttribute('decoding', 'async');
    return img;
  }
  throw new TypeError(`${ctx}.kind must be 'named' or 'asset', got ${iconRef.kind}`);
}

// Walidacja URL'a dla asset.ref. Pozwala: relatywne ścieżki (np.
// `/addon/icons/foo.svg`), `https:` URLs, oraz data: URIs ograniczone
// do image MIME. Wszystko inne odrzucamy z TypeMismatch-style errorem.
function assertSafeAssetSrc(raw, ctx) {
  const stripped = String(raw).replace(/^[\x00-\x20]+/, '').replace(/[\t\r\n]/g, '');
  if (stripped.length === 0) {
    throw new TypeError(`${ctx}: empty after control-char strip`);
  }
  if (SAFE_ASSET_DATA_PREFIX.test(stripped)) return stripped;
  let u;
  try {
    u = new URL(stripped, 'http://_/');
  } catch {
    throw new TypeError(`${ctx}: invalid URL '${raw}'`);
  }
  const rawHasScheme = /^[a-z][a-z0-9+.-]*:/i.test(stripped);
  if (rawHasScheme && !SAFE_ASSET_SCHEMES.has(u.protocol)) {
    throw new TypeError(`${ctx}: unsafe URL scheme '${u.protocol}'`);
  }
  return stripped;
}

// =============================================================================
// Plik: shared-styles.js
// Opis: Wspolne stylesheets dla Shadow DOM. Fetchuje /css/controls.css raz
//       i udostepnia gotowy CSSStyleSheet do adopcji przez komponenty.
//       Fallback: zwraca tresc jako <style> gdy Constructable Stylesheets
//       nie jest dostepny (Safari < 16.4).
// =============================================================================

let _sheetPromise = null;
let _rawCssPromise = null;

// Both caches below evict on rejection, for the reason spelled out at
// `cacheOnce`: a cached REJECTED promise disables control styles for every
// component for the life of the page, with no retry, after a single 404 or
// dropped connection.
async function fetchCss() {
  if (!_rawCssPromise) {
    const pending = fetch('/css/controls.css').then((r) => {
      if (!r.ok) throw new Error(`controls.css: ${r.status}`);
      return r.text();
    });
    pending.catch(() => { if (_rawCssPromise === pending) _rawCssPromise = null; });
    _rawCssPromise = pending;
  }
  return _rawCssPromise;
}

export async function getControlsSheet() {
  if (!('adoptedStyleSheets' in Document.prototype) || typeof CSSStyleSheet !== 'function') {
    return null;
  }
  if (!_sheetPromise) {
    // Evict on rejection for the same reason as `fetchCss` above: this promise
    // awaits that one, so a single failed fetch would otherwise stick here too
    // and no component would ever retry the control styles.
    const pending = (async () => {
      const css = await fetchCss();
      const sheet = new CSSStyleSheet();
      sheet.replaceSync(css);
      return sheet;
    })();
    pending.catch(() => { if (_sheetPromise === pending) _sheetPromise = null; });
    _sheetPromise = pending;
  }
  return _sheetPromise;
}

// =============================================================================
// Screen-scoped stylesheets for Shadow DOM.
//
// A selector cannot cross the shadow boundary, so a screen sheet whose rules
// are all prefixed (`.nas-root .trend polyline { ... }`) matches NOTHING inside
// a shadow root — there is no `.nas-root` ancestor in there. Cells that tf-table
// writes with `td.innerHTML` therefore rendered unstyled (a sparkline polyline
// fell back to the UA default fill: black).
//
// A sheet marked in index.html with `data-shadow-scope="<selector>"` carries the
// same declarations WITHOUT the prefix. A component passes its shadow root and
// its host here; every marked sheet whose selector matches an ancestor of the
// host is adopted into that root. Custom properties still inherit through the
// boundary, so `var(--accent-1)` resolves from the host's cascade unchanged.
//
// Such a sheet must stay out of the document cascade (its class names are
// generic: .mono, .text-3, .muted) — index.html links it with `media="not all"`.
// =============================================================================

const _scopedSheets = new Map();
const _scopedCss = new Map();

// A FAILED fetch must never stay in the cache. Caching the rejected promise
// would make every later table re-reject the same stale failure for the life of
// the page, with no retry — one 404 (or one dropped connection) would disable
// the sheet permanently. Evict on rejection so the next caller tries again.
function cacheOnce(map, href, start) {
  if (!map.has(href)) {
    const pending = start();
    pending.catch(() => { if (map.get(href) === pending) map.delete(href); });
    map.set(href, pending);
  }
  return map.get(href);
}

function fetchScopedCss(href) {
  return cacheOnce(_scopedCss, href, () => fetch(href).then((r) => {
    if (!r.ok) throw new Error(`${href}: ${r.status}`);
    return r.text();
  }));
}

function getScopedSheet(href) {
  return cacheOnce(_scopedSheets, href, async () => {
    if (typeof Document !== 'function'
      || !('adoptedStyleSheets' in Document.prototype)
      || typeof CSSStyleSheet !== 'function') {
      return null;
    }
    const sheet = new CSSStyleSheet();
    sheet.replaceSync(await fetchScopedCss(href));
    return sheet;
  });
}

/// Starts fetching every marked sheet as soon as this module loads, instead of
/// when the first table is built. Without it the first TentaNas table of a
/// session paints its cells while a real network fetch is still in flight — the
/// sheet is linked `media="not all"`, so the browser has no reason to load it
/// on its own and the cells flash unstyled. With the cache warm, adoption
/// resolves in a microtask and lands before the first paint.
export function warmScopedSheets() {
  if (typeof document === 'undefined' || typeof document.querySelectorAll !== 'function') return;
  for (const link of document.querySelectorAll('link[data-shadow-scope]')) {
    const href = link.getAttribute('href');
    if (href) getScopedSheet(href).catch(() => {});
  }
}

/// Adopts every `link[data-shadow-scope]` sheet whose scope selector matches an
/// ancestor of `host`. Sheets are constructed once per href and shared by every
/// shadow root that adopts them.
export async function adoptScopedSheetsInto(shadowRoot, host) {
  if (!shadowRoot || !host || typeof host.closest !== 'function') return;
  for (const link of document.querySelectorAll('link[data-shadow-scope]')) {
    const scope = link.getAttribute('data-shadow-scope');
    const href = link.getAttribute('href');
    if (!scope || !href) continue;
    let inScope = false;
    // An unparseable selector must not take the whole table down with it.
    try { inScope = !!host.closest(scope); } catch { inScope = false; }
    if (!inScope) continue;
    // One unreachable sheet must not stop the others from being adopted.
    try {
      const sheet = await getScopedSheet(href);
      if (sheet) {
        const current = shadowRoot.adoptedStyleSheets || [];
        if (!current.includes(sheet)) shadowRoot.adoptedStyleSheets = [...current, sheet];
        continue;
      }
      // fallback — dump the CSS into a <style> (Safari < 16.4).
      // APPEND, not prepend: adoptControlsInto's own fallback prepends its
      // <style>, so prepending here too would put the screen sheet BEFORE the
      // control base and let controls.css win every equal-specificity rule —
      // the inverse of the adopted-sheet order this refines.
      const style = document.createElement('style');
      style.textContent = await fetchScopedCss(href);
      shadowRoot.append(style);
    } catch { /* sheet unavailable — the cell keeps the control base styles */ }
  }
}

warmScopedSheets();

// =============================================================================
// Sprite z symbolami ikon (<symbol id="i-*">) zywuje w light DOM (body).
// W Shadow DOM referencje <use href="#i-..."> nie osiagaja symboli z document
// (spec ambiguity + ograniczenia Chrome/Safari), dlatego klonujemy sprite
// do shadow root raz — aby <use> mialo lokalny target.
// =============================================================================

let _cachedSprite = null;

function getSourceSprite() {
  return document.querySelector('svg[data-role="sprite"]')
    || document.querySelector('body > svg[aria-hidden="true"]');
}

export function injectSpriteIntoShadow(shadowRoot) {
  if (!shadowRoot) return;
  if (!_cachedSprite) {
    const src = getSourceSprite();
    if (!src) return;
    _cachedSprite = src.cloneNode(true);
    // wyzerowanie atrybutow rozmiaru — sprite ma byc niewidoczny
    _cachedSprite.setAttribute('width', '0');
    _cachedSprite.setAttribute('height', '0');
    _cachedSprite.setAttribute('aria-hidden', 'true');
    _cachedSprite.style.position = 'absolute';
    _cachedSprite.style.width = '0';
    _cachedSprite.style.height = '0';
    _cachedSprite.style.overflow = 'hidden';
    _cachedSprite.removeAttribute('data-role');
  }
  shadowRoot.appendChild(_cachedSprite.cloneNode(true));
}

export async function adoptControlsInto(shadowRoot) {
  const sheet = await getControlsSheet();
  if (sheet) {
    shadowRoot.adoptedStyleSheets = [...(shadowRoot.adoptedStyleSheets || []), sheet];
    return;
  }
  // fallback — zrzut CSS do <style>
  const css = await fetchCss();
  const style = document.createElement('style');
  style.textContent = css;
  shadowRoot.prepend(style);
}

// =============================================================================
// Design tokens for canvas painters. A <canvas> cannot inherit CSS, so every
// component that paints has to read the --tf-* value itself; resolving it
// against the document element keeps a light DOM and a shadow root on the same
// palette. One reader for all of them, so a token that stops resolving fails
// the same way everywhere.
// =============================================================================

/// `scope` resolves the token against one element instead of the document, for
/// the components whose palette can be overridden per instance (tf-run-timeline
/// sets its --tf-rt-* tokens on the host).
export function cssToken(name, fallback, scope) {
  if (typeof getComputedStyle !== 'function' || typeof document === 'undefined') return fallback;
  const target = scope || document.documentElement;
  if (!target) return fallback;
  const value = getComputedStyle(target).getPropertyValue(name);
  return value && value.trim() ? value.trim() : fallback;
}

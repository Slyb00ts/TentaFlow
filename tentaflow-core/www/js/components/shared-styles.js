// =============================================================================
// Plik: shared-styles.js
// Opis: Wspolne stylesheets dla Shadow DOM. Fetchuje /css/controls.css raz
//       i udostepnia gotowy CSSStyleSheet do adopcji przez komponenty.
//       Fallback: zwraca tresc jako <style> gdy Constructable Stylesheets
//       nie jest dostepny (Safari < 16.4).
// =============================================================================

let _sheetPromise = null;
let _rawCssPromise = null;

async function fetchCss() {
  if (!_rawCssPromise) {
    _rawCssPromise = fetch('/css/controls.css').then((r) => {
      if (!r.ok) throw new Error(`controls.css: ${r.status}`);
      return r.text();
    });
  }
  return _rawCssPromise;
}

export async function getControlsSheet() {
  if (!('adoptedStyleSheets' in Document.prototype) || typeof CSSStyleSheet !== 'function') {
    return null;
  }
  if (!_sheetPromise) {
    _sheetPromise = (async () => {
      const css = await fetchCss();
      const sheet = new CSSStyleSheet();
      sheet.replaceSync(css);
      return sheet;
    })();
  }
  return _sheetPromise;
}

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

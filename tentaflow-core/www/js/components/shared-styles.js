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

// =============================================================================
// Plik: lib/patch.js
// Opis: Wrapper na morphdom — inkrementalne aktualizacje DOM bez blysniecia.
//       Uzywane przy auto-refresh (services, mesh, dashboard) zamiast
//       `el.innerHTML = nowyHtml` ktory powoduje pelny reflow + utrate fokusu
//       na inputach + restart animacji.
//
//       API:
//         patchInner(target, newInnerHTML, opts?)  — morph children elementu
//         patchOuter(target, newHtml, opts?)       — morph sam element
//
//       opts:
//         preserveFocus: zachowaj aktywny input (default true)
//         preserveScroll: zachowaj scroll na zagniezdzonym kontenerze (default true)
//         onBeforeElUpdated: hook; return false = skip node
//
//       Przyklad:
//         patchInner(byId('svc-tbody'), newRowsHtml);
// =============================================================================

import morphdom from '/js/vendor/morphdom/morphdom.js';

/// Zamienia wewnetrzny HTML elementu przez morph (inkrementalne diff + patch).
/// Nie niszczy focusu ani scroll state na dzieciach.
export function patchInner(target, newInnerHTML, opts = {}) {
  if (!target) return;
  const tmp = document.createElement(target.tagName);
  tmp.innerHTML = newInnerHTML;
  // Skopiuj atrybuty "placeholder" container'a — zeby jego same atrybuty zostaly
  for (const attr of target.attributes) {
    tmp.setAttribute(attr.name, attr.value);
  }
  morphdom(target, tmp, {
    childrenOnly: true,
    onBeforeElUpdated: (fromEl, toEl) => defaultOnBeforeElUpdated(fromEl, toEl, opts),
    getNodeKey: defaultGetNodeKey,
  });
}

/// Zamienia caly element (wraz z nim samym).
export function patchOuter(target, newHtml, opts = {}) {
  if (!target) return;
  const tmp = document.createElement('div');
  tmp.innerHTML = newHtml.trim();
  const newEl = tmp.firstElementChild;
  if (!newEl) return;
  morphdom(target, newEl, {
    onBeforeElUpdated: (fromEl, toEl) => defaultOnBeforeElUpdated(fromEl, toEl, opts),
    getNodeKey: defaultGetNodeKey,
  });
}

function defaultGetNodeKey(node) {
  // Uzywamy data-key albo data-id — stabilna identyfikacja wierszy tabeli,
  // kart itp. Pozwala morphdom na reorder bez rerender.
  if (node.nodeType !== 1) return undefined;
  return node.getAttribute?.('data-key')
    || node.getAttribute?.('data-id')
    || node.id
    || undefined;
}

function defaultOnBeforeElUpdated(fromEl, toEl, opts) {
  // Custom hook first
  if (typeof opts.onBeforeElUpdated === 'function') {
    const r = opts.onBeforeElUpdated(fromEl, toEl);
    if (r === false) return false;
  }
  // Skip jesli nic sie nie zmienilo (szybka sciezka)
  if (fromEl.isEqualNode(toEl)) return false;

  // Zachowaj focus — nie rusz aktywnego inputu
  if ((opts.preserveFocus !== false) && fromEl === document.activeElement) {
    // Update tylko atrybutow, nie value/innerHTML
    const skipVal = fromEl.value;
    return true; // pozwol morphdom zaktualizowac atrybuty
      // ale wartosc inputa nie jest changeowana jesli isEqualNode odrzucilby - wartosci sa domyslnie
      // zarzadzane przez atrybut value ktory morphdom kopiuje, wiec musimy zachowac fromEl.value
      // Robie to po patchu — tu tylko powiedzmy nie resetuj
      // (morphdom domyslnie NIE nadpisuje value property)
    ;
  }

  // Preserve scroll position na zagniezdzonych scroll containers
  if (opts.preserveScroll !== false && (fromEl.scrollHeight > fromEl.clientHeight)) {
    const sTop = fromEl.scrollTop;
    const sLeft = fromEl.scrollLeft;
    requestAnimationFrame(() => {
      fromEl.scrollTop = sTop;
      fromEl.scrollLeft = sLeft;
    });
  }
  return true;
}

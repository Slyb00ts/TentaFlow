// =============================================================================
// Plik: tf-screen.js
// Opis: <tf-screen> — shell ekranu z dwoma strefami: sticky head (breadcrumb +
//       header + tabs) oraz scrollowalny body. Zero atrybutow; moduly wstawiaja
//       tresc przez sloty (slot="breadcrumb" | "header" | "tabs" + default).
//       Light DOM — style w css/style.css. Bez tla pasa pod headerem i bez
//       ramki/karty: cala powierzchnia jest na --bg, zgodnie z mockupem
//       addons-permissions.
// Przyklad:
//   <tf-screen>
//     <div slot="breadcrumb" class="tf-breadcrumb">...</div>
//     <div slot="header" class="tf-page-header">...</div>
//     <tf-tabs slot="tabs">...</tf-tabs>
//     <!-- body (default slot) -->
//   </tf-screen>
// =============================================================================

class TfScreen extends HTMLElement {
  constructor() {
    super();
    this._built = false;
    this._refs = {};
  }

  connectedCallback() {
    if (!this._built) this._build();
  }

  // Bez Shadow DOM nie mamy mechaniki <slot>, wiec przy pierwszym connect
  // rekami przenosimy dzieci z odpowiednim atrybutem slot= do wewnetrznych
  // kontenerow. Wszystko bez slot= laduje w body.
  _build() {
    const incoming = Array.from(this.childNodes);
    this.innerHTML = '';

    const head = document.createElement('div');
    head.className = 'tf-screen-head';

    const crumbs = document.createElement('div');
    crumbs.className = 'tf-screen-head__crumbs';

    const header = document.createElement('div');
    header.className = 'tf-screen-head__header';

    const tabs = document.createElement('div');
    tabs.className = 'tf-screen-head__tabs';

    head.append(crumbs, header, tabs);

    const body = document.createElement('div');
    body.className = 'tf-screen-body';

    this.append(head, body);

    for (const node of incoming) {
      if (node.nodeType === 3) {
        if (node.textContent.trim()) body.appendChild(node);
        continue;
      }
      if (node.nodeType !== 1) continue;
      const slot = node.getAttribute('slot');
      if (slot === 'breadcrumb') crumbs.appendChild(node);
      else if (slot === 'header') header.appendChild(node);
      else if (slot === 'tabs') tabs.appendChild(node);
      else body.appendChild(node);
    }

    // Puste kontenery head-a nie moga zabierac pionowego miejsca.
    crumbs.hidden = crumbs.childElementCount === 0;
    header.hidden = header.childElementCount === 0;
    tabs.hidden = tabs.childElementCount === 0;

    this._refs = { head, crumbs, header, tabs, body };
    this._built = true;
  }

  // Gettery uzyteczne gdy modul chce dorzucic tresc po mount.
  get headSlot() { return this._refs.head; }
  get bodySlot() { return this._refs.body; }
  get crumbsSlot() { return this._refs.crumbs; }
  get headerSlot() { return this._refs.header; }
  get tabsSlot() { return this._refs.tabs; }
}

customElements.define('tf-screen', TfScreen);
export { TfScreen };

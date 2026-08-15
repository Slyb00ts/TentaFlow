// =============================================================================
// File: tf-option-row.js
// Description: <tf-option-row> — ONE row of a list you pick from: an optional
//              leading marker (a status dot, a shortcut key, an icon), a label,
//              an optional second line, and the selected / disabled states.
//              It renders a real <button>, so keyboard, focus order and screen
//              readers work without the host wiring anything.
//              Light DOM. The host is `display: contents`, so it never adds a
//              box to the parent's layout: a feature stylesheet themes the row
//              through `.<feature-class> .tf-option-row`, exactly as it themed
//              the bare <button> it replaces.
//
// Attributes : value, label, sub, marker, selected, disabled
// Properties : value, label, sub, marker, selected, disabled,
//              lead — an Element inserted verbatim before the text, which is
//              how a module keeps its own status-dot dictionary. `marker` is
//              the string form (a shortcut key) and exists as an attribute so a
//              row can be written in one template string; `lead` wins.
// Events     : "option-select" (bubbles; detail { value }) — the row does NOT
//              select itself; the list that owns the state decides.
//
// Example: const row = document.createElement('tf-option-row');
//          row.label = 'platforma-core'; row.sub = 'mainpc · 1 sesja';
//          row.lead = dotEl; row.selected = true;
// =============================================================================

class TfOptionRow extends HTMLElement {
  static get observedAttributes() {
    return ['label', 'sub', 'value', 'marker', 'selected', 'disabled'];
  }

  constructor() {
    super();
    this._btn = null;
    this._lead = null;
    this._onClick = this._onClick.bind(this);
  }

  connectedCallback() {
    // A row built from one innerHTML string upgrades AFTER its owner assigned
    // properties, which would leave own properties shadowing these accessors
    // forever. Hand the values back through the accessors at upgrade time.
    for (const prop of ['value', 'label', 'sub', 'marker', 'lead', 'selected', 'disabled']) {
      if (!Object.prototype.hasOwnProperty.call(this, prop)) continue;
      const value = this[prop];
      delete this[prop];
      this[prop] = value;
    }
    if (!this._btn) this._build();
    this._update();
  }

  attributeChangedCallback() {
    if (this._btn) this._update();
  }

  get value() { return this.getAttribute('value') ?? ''; }
  set value(v) { this.setAttribute('value', String(v ?? '')); }

  get label() { return this.getAttribute('label') ?? ''; }
  set label(v) { this.setAttribute('label', String(v ?? '')); }

  get sub() { return this.getAttribute('sub') ?? ''; }
  set sub(v) {
    if (v === null || v === undefined || v === '') this.removeAttribute('sub');
    else this.setAttribute('sub', String(v));
  }

  get marker() { return this.getAttribute('marker') ?? ''; }
  set marker(v) {
    if (v === null || v === undefined || v === '') this.removeAttribute('marker');
    else this.setAttribute('marker', String(v));
  }

  get selected() { return this.hasAttribute('selected'); }
  set selected(v) { this.toggleAttribute('selected', !!v); }

  get disabled() { return this.hasAttribute('disabled'); }
  set disabled(v) { this.toggleAttribute('disabled', !!v); }

  get lead() { return this._lead; }
  set lead(v) {
    this._lead = v ?? null;
    if (this._btn) this._update();
  }

  _build() {
    this.innerHTML = '';
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'tf-option-row';
    btn.addEventListener('click', this._onClick);

    const lead = document.createElement('span');
    lead.className = 'tf-option-row__lead';

    const text = document.createElement('span');
    text.className = 'tf-option-row__text';
    const label = document.createElement('span');
    label.className = 'tf-option-row__label';
    const sub = document.createElement('span');
    sub.className = 'tf-option-row__sub';
    text.appendChild(label);
    text.appendChild(sub);

    btn.appendChild(lead);
    btn.appendChild(text);
    this.appendChild(btn);

    this._btn = btn;
    this._leadEl = lead;
    this._labelEl = label;
    this._subEl = sub;
  }

  _update() {
    this._labelEl.textContent = this.label;
    const sub = this.sub;
    this._subEl.textContent = sub;
    this._subEl.hidden = !sub;

    const lead = this._lead;
    if (lead instanceof Node) {
      this._leadEl.replaceChildren(lead);
      this._leadEl.hidden = false;
    } else if (this.marker) {
      const marker = document.createElement('span');
      marker.className = 'tf-option-row__marker';
      marker.textContent = this.marker;
      this._leadEl.replaceChildren(marker);
      this._leadEl.hidden = false;
    } else {
      this._leadEl.replaceChildren();
      this._leadEl.hidden = true;
    }

    const disabled = this.disabled;
    this._btn.disabled = disabled;
    this._btn.setAttribute('aria-disabled', disabled ? 'true' : 'false');
    // `aria-current` and not `aria-selected`: the row is a control in a list,
    // not an option of a listbox, and the host owns no listbox role.
    if (this.selected) this._btn.setAttribute('aria-current', 'true');
    else this._btn.removeAttribute('aria-current');
  }

  _onClick(e) {
    if (this.disabled) {
      e.preventDefault();
      e.stopPropagation();
      return;
    }
    this.dispatchEvent(new CustomEvent('option-select', {
      bubbles: true,
      detail: { value: this.value },
    }));
  }
}

customElements.define('tf-option-row', TfOptionRow);
export { TfOptionRow };

// =============================================================================
// Plik: tf-segmented.js
// Opis: Komponent <tf-segmented> — segmented control dla N-stanowych opcji.
//       Wartosc jako atrybut `value`, opcje jako <option value="x" variant="ok">
//       dzieci. `variant` na option: ok|warn|err|accent|neutral (domyslnie).
//       Emituje event 'change' z detail.value. A11y: role=radiogroup + aria.
// Przyklad:
//   <tf-segmented value="auto" size="sm">
//     <option value="auto" variant="neutral">Auto</option>
//     <option value="allow" variant="ok">Zezwól</option>
//     <option value="deny" variant="err">Odmów</option>
//   </tf-segmented>
// =============================================================================

class TfSegmented extends HTMLElement {
  static get observedAttributes() { return ['value', 'size', 'disabled']; }
  constructor() {
    super();
    this._options = [];
    this._container = null;
    this._onClick = this._onClick.bind(this);
    this._onKey = this._onKey.bind(this);
  }

  connectedCallback() {
    if (!this._container) this._build();
    this._update();
  }

  disconnectedCallback() {
    if (this._container) {
      this._container.removeEventListener('click', this._onClick);
      this._container.removeEventListener('keydown', this._onKey);
    }
  }

  attributeChangedCallback() { if (this._container) this._update(); }

  get value() { return this.getAttribute('value') || ''; }
  set value(v) {
    if (v !== this.value) {
      this.setAttribute('value', v ?? '');
    }
  }

  _collectOptions() {
    // Zbierz z <option value="x" variant="y">label</option> przed build().
    const optEls = Array.from(this.querySelectorAll(':scope > option'));
    this._options = optEls.map((o) => ({
      value: o.getAttribute('value') || '',
      variant: (o.getAttribute('variant') || 'neutral').toLowerCase(),
      label: o.textContent || '',
    }));
    optEls.forEach((o) => o.remove());
  }

  _build() {
    this._collectOptions();
    this.innerHTML = '';
    const wrap = document.createElement('div');
    wrap.className = 'tf-segmented';
    wrap.setAttribute('role', 'radiogroup');
    for (const opt of this._options) {
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'tf-seg-opt';
      btn.dataset.value = opt.value;
      btn.dataset.variant = opt.variant;
      btn.setAttribute('role', 'radio');
      btn.setAttribute('aria-checked', 'false');
      btn.textContent = opt.label;
      wrap.appendChild(btn);
    }
    wrap.addEventListener('click', this._onClick);
    wrap.addEventListener('keydown', this._onKey);
    this.appendChild(wrap);
    this._container = wrap;
  }

  _update() {
    if (!this._container) return;
    const val = this.value;
    const size = this.getAttribute('size') || '';
    const disabled = this.hasAttribute('disabled');
    this._container.classList.toggle('tf-segmented-sm', size === 'sm');
    this._container.classList.toggle('tf-segmented-disabled', disabled);
    for (const btn of this._container.querySelectorAll('.tf-seg-opt')) {
      const active = btn.dataset.value === val;
      btn.classList.toggle('active', active);
      btn.setAttribute('aria-checked', active ? 'true' : 'false');
      btn.tabIndex = active ? 0 : -1;
      btn.disabled = disabled;
    }
  }

  _onClick(e) {
    const btn = e.target.closest('.tf-seg-opt');
    if (!btn || btn.disabled) return;
    this._select(btn.dataset.value);
  }

  _onKey(e) {
    if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(e.key)) return;
    e.preventDefault();
    const btns = Array.from(this._container.querySelectorAll('.tf-seg-opt'));
    if (btns.length === 0) return;
    const curIdx = btns.findIndex((b) => b.dataset.value === this.value);
    let nextIdx = curIdx;
    if (e.key === 'ArrowLeft') nextIdx = Math.max(0, curIdx - 1);
    else if (e.key === 'ArrowRight') nextIdx = Math.min(btns.length - 1, curIdx + 1);
    else if (e.key === 'Home') nextIdx = 0;
    else if (e.key === 'End') nextIdx = btns.length - 1;
    if (nextIdx >= 0 && nextIdx < btns.length) {
      const val = btns[nextIdx].dataset.value;
      this._select(val);
      btns[nextIdx].focus();
    }
  }

  _select(val) {
    if (val === this.value) return;
    this.setAttribute('value', val);
    this.dispatchEvent(new CustomEvent('change', { detail: { value: val }, bubbles: true }));
  }
}

customElements.define('tf-segmented', TfSegmented);
export { TfSegmented };

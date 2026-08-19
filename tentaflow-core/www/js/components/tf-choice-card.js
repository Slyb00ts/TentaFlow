// =============================================================================
// File: tf-choice-card.js
// Description: <tf-choice-card> + <tf-choice-group> — a card that presents ONE
//              option of an irreversible architectural choice together with its
//              consequences, and the group that makes the options exclusive.
//              The shape a wizard needs when the decision cannot be undone: an
//              icon + name, an optional "default" pill, one sentence of prose,
//              a consequence list where each line carries its own tone, and an
//              UNAVAILABLE state that says what to install rather than just
//              greying out. Light DOM, styles in css/controls.css, no deps.
//
// <tf-choice-card>
//   Attributes: value, icon (sprite id), heading, description, pill,
//     pill-tone (ok|warn|err|info|accent|neutral), note (shown under the card —
//     the "what to install" line for a disabled option), selected, disabled.
//   Properties: value, heading, description, note, selected, disabled,
//     features — [{ icon, tone, lead, text }]; `lead` renders bold in the
//     line's tone, `text` follows in prose.
//   Events  : "choice-select" (bubbles, cancelable; detail { value }) — the
//     card does NOT select itself; the group (or the host) owns the state.
//
// <tf-choice-group>
//   Attributes: value, columns (default 2), aria-label.
//   Properties: value.
//   Events  : "change" (bubbles; detail { value }).
//   Keyboard: Arrow keys / Home / End move between enabled cards (roving
//     tabindex, wrapping), Space/Enter selects. role="radiogroup" + role="radio".
//
// Example: <tf-choice-group value="native" aria-label="Execution mode">
//            <tf-choice-card value="native" icon="zap" heading="Native"
//              pill="default" pill-tone="warn"></tf-choice-card>
//            <tf-choice-card value="container" icon="shield" heading="Container"
//              disabled note="Install Docker on this node"></tf-choice-card>
//          </tf-choice-group>
// =============================================================================

const PILL_TONES = new Set(['ok', 'warn', 'err', 'info', 'accent', 'neutral']);
const FEATURE_TONES = new Set(['ok', 'warn', 'err', 'info', 'accent', 'muted']);

function safeIconName(value) {
  const text = String(value || '').trim();
  return /^[a-z0-9_-]{1,64}$/i.test(text) ? text : '';
}

function iconSvg(name, className) {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('class', className);
  svg.setAttribute('aria-hidden', 'true');
  const use = document.createElementNS('http://www.w3.org/2000/svg', 'use');
  use.setAttribute('href', `#i-${name}`);
  svg.appendChild(use);
  return svg;
}

class TfChoiceCard extends HTMLElement {
  static get observedAttributes() {
    return ['value', 'icon', 'heading', 'description', 'pill', 'pill-tone',
      'note', 'selected', 'disabled'];
  }

  constructor() {
    super();
    this._card = null;
    this._features = [];
    this._onClick = this._onClick.bind(this);
    this._onKeyDown = this._onKeyDown.bind(this);
  }

  connectedCallback() {
    // A group parsed from one innerHTML string upgrades BEFORE its cards, so its
    // first _sync() writes `card.selected` onto a plain element. That own
    // property shadows the accessor forever: the attribute is never written and
    // the card can never repaint its selection. Hand the values to the
    // accessors at upgrade time instead.
    for (const prop of ['value', 'heading', 'description', 'note', 'selected', 'disabled', 'features']) {
      if (!Object.prototype.hasOwnProperty.call(this, prop)) continue;
      const value = this[prop];
      delete this[prop];
      this[prop] = value;
    }
    if (!this._card) this._build();
    this._render();
    // Tell an enclosing group synchronously, so a card appended after the group
    // was built still gets its radio role and its place in the roving tabindex.
    const group = this.parentElement;
    if (group && group.tagName === 'TF-CHOICE-GROUP' && typeof group._sync === 'function') {
      group._sync();
    }
  }

  attributeChangedCallback(name, oldVal, newVal) {
    if (oldVal === newVal || !this._card) return;
    this._render();
  }

  get value() { return this.getAttribute('value') || ''; }
  set value(v) { this.setAttribute('value', String(v ?? '')); }

  get heading() { return this.getAttribute('heading') || ''; }
  set heading(v) { this.setAttribute('heading', String(v ?? '')); }

  get description() { return this.getAttribute('description') || ''; }
  set description(v) { this.setAttribute('description', String(v ?? '')); }

  get note() { return this.getAttribute('note') || ''; }
  set note(v) { this.setAttribute('note', String(v ?? '')); }

  get selected() { return this.hasAttribute('selected'); }
  set selected(v) { if (v) this.setAttribute('selected', ''); else this.removeAttribute('selected'); }

  get disabled() { return this.hasAttribute('disabled'); }
  set disabled(v) { if (v) this.setAttribute('disabled', ''); else this.removeAttribute('disabled'); }

  get features() { return this._features; }
  set features(list) {
    this._features = Array.isArray(list) ? list : [];
    if (this._card) this._render();
  }

  // Focus target for the group's roving tabindex.
  focus(opts) { this._card?.focus(opts); }

  _build() {
    this.innerHTML = '';
    const card = document.createElement('div');
    card.className = 'tf-choice-card';
    card.addEventListener('click', this._onClick);
    card.addEventListener('keydown', this._onKeyDown);
    this.appendChild(card);
    this._card = card;
  }

  _render() {
    const card = this._card;
    const disabled = this.disabled;
    const selected = this.selected;

    card.textContent = '';
    card.className = `tf-choice-card${selected ? ' is-selected' : ''}${disabled ? ' is-disabled' : ''}`;
    // A card inside a group is a radio; standalone it is a toggle button. The
    // group stamps the role, so the standalone default lives here.
    if (!card.hasAttribute('role')) card.setAttribute('role', 'button');
    if (card.getAttribute('role') === 'radio') card.setAttribute('aria-checked', selected ? 'true' : 'false');
    else card.setAttribute('aria-pressed', selected ? 'true' : 'false');
    card.setAttribute('aria-disabled', disabled ? 'true' : 'false');
    if (!card.hasAttribute('tabindex')) card.setAttribute('tabindex', disabled ? '-1' : '0');
    if (disabled) card.setAttribute('tabindex', '-1');

    const head = document.createElement('div');
    head.className = 'tf-choice-card__head';
    const icon = safeIconName(this.getAttribute('icon'));
    if (icon) head.appendChild(iconSvg(icon, 'tf-choice-card__icon'));

    const heading = document.createElement('span');
    heading.className = 'tf-choice-card__heading';
    heading.textContent = this.heading;
    head.appendChild(heading);

    const pill = this.getAttribute('pill');
    if (pill) {
      const tone = (this.getAttribute('pill-tone') || 'warn').toLowerCase();
      const el = document.createElement('span');
      el.className = `tf-choice-card__pill tf-choice-card__pill--${PILL_TONES.has(tone) ? tone : 'warn'}`;
      el.textContent = pill;
      head.appendChild(el);
    }
    card.appendChild(head);

    if (this.description) {
      const desc = document.createElement('p');
      desc.className = 'tf-choice-card__desc';
      desc.textContent = this.description;
      card.appendChild(desc);
    }

    if (this._features.length) {
      const list = document.createElement('ul');
      list.className = 'tf-choice-card__features';
      for (const feature of this._features) {
        const li = this._buildFeature(feature);
        if (li) list.appendChild(li);
      }
      if (list.children.length) card.appendChild(list);
    }

    // The unavailable case must say what to do about it, not just dim the card.
    if (this.note) {
      const note = document.createElement('p');
      note.className = `tf-choice-card__note${disabled ? ' tf-choice-card__note--blocking' : ''}`;
      note.textContent = this.note;
      card.appendChild(note);
      const noteId = `${this.id || `tf-cc-${this.value || 'x'}`}-note`;
      note.id = noteId;
      card.setAttribute('aria-describedby', noteId);
    } else {
      card.removeAttribute('aria-describedby');
    }
  }

  _buildFeature(feature) {
    if (!feature || typeof feature !== 'object') return null;
    const lead = feature.lead == null ? '' : String(feature.lead);
    const text = feature.text == null ? '' : String(feature.text);
    if (!lead && !text) return null;

    const tone = String(feature.tone || '').toLowerCase();
    const li = document.createElement('li');
    li.className = `tf-choice-card__feature${FEATURE_TONES.has(tone) ? ` tf-choice-card__feature--${tone}` : ''}`;

    const icon = safeIconName(feature.icon);
    if (icon) li.appendChild(iconSvg(icon, 'tf-choice-card__feature-icon'));

    const body = document.createElement('span');
    body.className = 'tf-choice-card__feature-text';
    if (lead) {
      const strong = document.createElement('b');
      strong.textContent = lead;
      body.appendChild(strong);
    }
    if (text) body.appendChild(document.createTextNode(text));
    li.appendChild(body);
    return li;
  }

  _onClick() { this._emitSelect(); }

  _onKeyDown(e) {
    if (e.key !== ' ' && e.key !== 'Enter') return;
    e.preventDefault();
    this._emitSelect();
  }

  // Intent only. A disabled card is inert — an unavailable architecture must not
  // be selectable by keyboard just because it is still on screen.
  _emitSelect() {
    if (this.disabled) return;
    this.dispatchEvent(new CustomEvent('choice-select', {
      bubbles: true,
      cancelable: true,
      detail: { value: this.value },
    }));
  }
}

customElements.define('tf-choice-card', TfChoiceCard);

class TfChoiceGroup extends HTMLElement {
  static get observedAttributes() { return ['value', 'columns']; }

  constructor() {
    super();
    this._built = false;
    this._syncing = false;
    this._onSelect = this._onSelect.bind(this);
    this._onKeyDown = this._onKeyDown.bind(this);
  }

  connectedCallback() {
    if (!this._built) {
      this._built = true;
      this.setAttribute('role', 'radiogroup');
      this.addEventListener('choice-select', this._onSelect);
      this.addEventListener('keydown', this._onKeyDown);
    }
    this._sync();
  }

  attributeChangedCallback(name, oldVal, newVal) {
    if (oldVal === newVal || !this._built) return;
    this._sync();
  }

  get value() { return this.getAttribute('value') || ''; }
  set value(v) { this.setAttribute('value', String(v ?? '')); }

  get cards() {
    return [...this.querySelectorAll(':scope > tf-choice-card')];
  }

  _enabledCards() { return this.cards.filter((c) => !c.disabled); }

  // Mirrors the group's value onto the cards and maintains a roving tabindex:
  // exactly one enabled card is in the tab order, the rest are reachable with
  // the arrow keys — the APG radiogroup contract.
  _sync() {
    if (this._syncing) return;
    this._syncing = true;
    try {
      const cards = this.cards;
      const value = this.value;
      let focusable = null;
      for (const card of cards) {
        const on = !card.disabled && card.value === value;
        card.selected = on;
        const inner = card.querySelector('.tf-choice-card');
        if (inner) {
          // Written here, not left to the card's re-render: assigning an already
          // correct `selected` fires no attribute change, so nothing would run.
          inner.setAttribute('role', 'radio');
          inner.setAttribute('aria-checked', on ? 'true' : 'false');
          inner.removeAttribute('aria-pressed');
        }
        if (!card.disabled && !focusable && on) focusable = card;
      }
      if (!focusable) focusable = this._enabledCards()[0] || null;
      for (const card of cards) {
        const inner = card.querySelector('.tf-choice-card');
        if (!inner) continue;
        inner.setAttribute('tabindex', !card.disabled && card === focusable ? '0' : '-1');
      }
    } finally {
      this._syncing = false;
    }
  }

  _onSelect(e) {
    const card = e.target.closest('tf-choice-card');
    if (!card || card.disabled) return;
    if (this.value === card.value) return;
    this.value = card.value;
    this.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { value: card.value } }));
  }

  _onKeyDown(e) {
    const keys = ['ArrowRight', 'ArrowDown', 'ArrowLeft', 'ArrowUp', 'Home', 'End'];
    if (!keys.includes(e.key)) return;
    const enabled = this._enabledCards();
    if (enabled.length < 2 && e.key !== 'Home' && e.key !== 'End') return;
    if (!enabled.length) return;
    const current = e.target.closest('tf-choice-card');
    const idx = enabled.indexOf(current);
    e.preventDefault();

    let next;
    if (e.key === 'Home') next = enabled[0];
    else if (e.key === 'End') next = enabled[enabled.length - 1];
    else {
      const step = (e.key === 'ArrowRight' || e.key === 'ArrowDown') ? 1 : -1;
      const from = idx >= 0 ? idx : 0;
      next = enabled[(from + step + enabled.length) % enabled.length];
    }
    if (!next) return;
    // Arrow keys move AND select — the radiogroup contract.
    if (this.value !== next.value) {
      this.value = next.value;
      this.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { value: next.value } }));
    } else {
      this._sync();
    }
    next.focus();
  }
}

customElements.define('tf-choice-group', TfChoiceGroup);
export { TfChoiceCard, TfChoiceGroup };

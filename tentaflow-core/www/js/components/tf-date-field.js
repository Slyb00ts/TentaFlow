// =============================================================================
// File: tf-date-field.js
// Description: <tf-date-field> — a day field that shows and accepts the day in
//   the UI language's own format (30.09.2026), opens a calendar popup
//   (<tf-datepicker>) and exposes the day as ISO `YYYY-MM-DD` through `value`.
//   Typing, pasting and the keyboard work without the popup: ArrowDown or
//   Alt+ArrowDown opens it, Escape closes it. `change`/`input` events carry the
//   ISO day in `detail.value` and fire whenever the day itself changes (a
//   complete valid date, a pick, a clear) — never for half-typed text.
// Example:
//   <tf-date-field label="Od kiedy" value="2026-09-30" min="2026-01-01" required></tf-date-field>
// Attributes: value, label, hint, error, min, max, required, disabled, aria-label
//   (names the input when there is no visible `label`).
// =============================================================================

import { I18n } from '/js/i18n.js';
import { dateFormatHint, formatDay, isIsoDay, parseDay } from '/js/lib/date-format.js';
import './tf-input.js';
import './tf-button.js';
import './tf-datepicker.js';

const t = (key, vars) => I18n.t(`date_field.${key}`, vars);

class TfDateField extends HTMLElement {
  static get observedAttributes() { return ['value', 'label', 'hint', 'error', 'min', 'max', 'required', 'disabled', 'aria-label']; }

  constructor() {
    super();
    this._iso = '';
    this._built = false;
    this._ownError = '';
    this._onOutside = this._onOutside.bind(this);
    this._onViewport = this._onViewport.bind(this);
    this._unsubscribe = null;
  }

  connectedCallback() {
    if (!this._built) this._build();
    this._writeText();
    this._syncAttrs();
    this._unsubscribe = I18n.subscribe(() => {
      if (this._iso) this._writeText();
      this._syncAttrs();
    });
  }

  disconnectedCallback() {
    this._close();
    this._unsubscribe?.();
    this._unsubscribe = null;
  }

  attributeChangedCallback(name, oldVal, newVal) {
    if (oldVal === newVal) return;
    // The value is readable before the element is connected (a form reads its fields as it builds them).
    if (name === 'value') this._iso = isIsoDay(newVal) ? newVal : '';
    if (!this._built) return;
    if (name === 'value') {
      this._ownError = '';
      this._writeText();
    }
    this._syncAttrs();
  }

  /** The day as `YYYY-MM-DD`, or an empty string while the field is empty or holds text that is not a day. */
  get value() { return this._iso; }
  set value(v) {
    const next = v == null ? '' : String(v);
    if (next !== (this.getAttribute('value') ?? '')) this.setAttribute('value', next);
    else if (this._built) { this._iso = isIsoDay(next) ? next : ''; this._ownError = ''; this._writeText(); this._syncAttrs(); }
  }

  /** True when the field is empty or holds a day inside [min, max]. */
  get valid() { return this._problem() === ''; }

  /** The text of the field as the user sees it. */
  get text() { return this._input?.value ?? ''; }

  focus() { this._input.focus(); }

  /** Marks the field when it holds text that is not a day or a day outside [min, max]; answers whether it is fine. */
  validate() {
    this._ownError = this._problem();
    this._syncAttrs();
    return this._ownError === '';
  }

  _build() {
    this._built = true;
    this.innerHTML = '';
    this._row = document.createElement('div');
    this._row.className = 'tf-date-field__row';
    this._input = document.createElement('tf-input');
    this._input.setAttribute('autocomplete', 'off');
    this._input.setAttribute('inputmode', 'numeric');
    this._toggle = document.createElement('tf-button');
    this._toggle.setAttribute('variant', 'ghost');
    this._toggle.setAttribute('icon', 'calendar');
    this._toggle.className = 'tf-date-field__toggle';
    this._row.append(this._input);
    this._pop = document.createElement('div');
    this._pop.className = 'tf-date-field__pop';
    this._pop.hidden = true;
    this._calendar = document.createElement('tf-datepicker');
    this._pop.append(this._calendar);
    this.append(this._row, this._pop);
    // The calendar button sits inside the field's frame, so it stays level with the input whatever hint or error the field shows.
    this._input.querySelector('.tf-input-wrap')?.append(this._toggle);

    this._input.addEventListener('input', (e) => this._onTyped(e));
    this._input.addEventListener('change', (e) => { e.stopPropagation(); this._commitText(); });
    this._input.addEventListener('keydown', (e) => this._onInputKey(e));
    this._toggle.addEventListener('click', () => (this._pop.hidden ? this._open() : this._close()));
    this._pop.addEventListener('keydown', (e) => {
      if (e.key !== 'Escape') return;
      e.preventDefault();
      e.stopPropagation();
      this._close();
      this._input.focus();
    });
    this._calendar.addEventListener('change', (e) => {
      e.stopPropagation();
      this._setDay(e.detail.value);
      this._close();
      this._input.focus();
    });
  }

  _writeText() {
    this._input.value = this._iso ? formatDay(this._iso) : '';
    if (this._iso) this._calendar.setAttribute('value', this._iso);
  }

  _syncAttrs() {
    const required = this.hasAttribute('required');
    const label = this.getAttribute('label') || '';
    this._input.setAttribute('label', required && label ? `${label} *` : label);
    this._input.setAttribute('placeholder', t('placeholder'));
    const hint = this.getAttribute('hint');
    if (hint) this._input.setAttribute('hint', hint); else this._input.removeAttribute('hint');
    const error = this.getAttribute('error') || this._ownError;
    if (error) this._input.setAttribute('error', error); else this._input.removeAttribute('error');
    for (const attr of ['min', 'max']) {
      const v = this.getAttribute(attr);
      if (v) this._calendar.setAttribute(attr, v); else this._calendar.removeAttribute(attr);
    }
    const disabled = this.hasAttribute('disabled');
    this._input.toggleAttribute('disabled', disabled);
    this._toggle.toggleAttribute('disabled', disabled);
    this._toggle.setAttribute('aria-label', t('pick'));
    const name = this.getAttribute('aria-label');
    const field = this._input.querySelector('input');
    if (field) { if (name) field.setAttribute('aria-label', name); else field.removeAttribute('aria-label'); }
  }

  _problem() {
    const typed = this._input.value.trim();
    if (!typed) return '';
    const iso = parseDay(typed);
    if (!iso) return t('invalid', { format: dateFormatHint() });
    const min = this.getAttribute('min');
    const max = this.getAttribute('max');
    if ((min && iso < min) || (max && iso > max)) return t('out_of_range');
    return '';
  }

  // Half-typed text never changes the day; the field announces a day only once the text is one.
  _onTyped(e) {
    e.stopPropagation();
    this._ownError = '';
    this.removeAttribute('error');
    this._syncAttrs();
    const typed = this._input.value.trim();
    const iso = typed === '' ? '' : parseDay(typed);
    if (iso === null) { this._announce(''); return; }
    if (iso) this._calendar.setAttribute('value', iso);
    this._announce(iso);
  }

  _announce(iso) {
    if (iso === this._iso) return;
    this._iso = iso;
    this.dispatchEvent(new CustomEvent('input', { bubbles: true, detail: { value: iso } }));
    this.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { value: iso } }));
  }

  // On leaving the field a valid day is rewritten in the canonical format (1.9.2026 → 01.09.2026).
  _commitText() {
    const typed = this._input.value.trim();
    const iso = parseDay(typed);
    if (iso) this._input.value = formatDay(iso);
    this._ownError = this._problem();
    this._syncAttrs();
  }

  _setDay(iso) {
    this._ownError = '';
    this.removeAttribute('error');
    this._input.value = formatDay(iso);
    this._syncAttrs();
    this._announce(iso);
  }

  _onInputKey(e) {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      this._open();
    } else if (e.key === 'Escape' && !this._pop.hidden) {
      e.preventDefault();
      e.stopPropagation();
      this._close();
    }
  }

  _open() {
    if (this.hasAttribute('disabled')) return;
    this._calendar.setAttribute('value', this._iso || parseDay(this._input.value) || '');
    // Out of the host while open: a window around the field clips and offsets fixed children (transform, overflow).
    document.body.appendChild(this._pop);
    this._pop.hidden = false;
    this._place();
    document.addEventListener('pointerdown', this._onOutside, true);
    window.addEventListener('resize', this._onViewport);
    window.addEventListener('scroll', this._onViewport, true);
    (this._calendar.querySelector('.tf-dp-day[tabindex="0"]'))?.focus();
  }

  _close() {
    if (!this._pop || this._pop.hidden) return;
    this._pop.hidden = true;
    this.append(this._pop);
    document.removeEventListener('pointerdown', this._onOutside, true);
    window.removeEventListener('resize', this._onViewport);
    window.removeEventListener('scroll', this._onViewport, true);
  }

  _onOutside(e) {
    if (!this.contains(e.target) && !this._pop.contains(e.target)) this._close();
  }

  _onViewport(e) {
    if (e.type === 'scroll' && (this._pop.contains(e.target) || e.target.contains?.(this._pop))) return;
    this._close();
  }

  // The popup leaves the flow so it never pushes the fields of a window off the screen.
  _place() {
    const pop = this._pop;
    const anchor = this._row.getBoundingClientRect();
    pop.style.top = '0px';
    pop.style.left = '0px';
    const w = pop.offsetWidth;
    const h = pop.offsetHeight;
    const gap = 4;
    const margin = 8;
    const below = window.innerHeight - anchor.bottom - gap - margin;
    const above = anchor.top - gap - margin;
    const top = h > below && above > below ? Math.max(margin, anchor.top - gap - h) : anchor.bottom + gap;
    const left = Math.min(Math.max(margin, anchor.left), window.innerWidth - w - margin);
    pop.style.top = `${Math.round(top)}px`;
    pop.style.left = `${Math.round(Math.max(margin, left))}px`;
  }
}

customElements.define('tf-date-field', TfDateField);
export { TfDateField };

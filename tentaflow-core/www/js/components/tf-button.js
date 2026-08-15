// =============================================================================
// Plik: tf-button.js
// Opis: Komponent <tf-button> — renderuje standardowy button z klasami .tf-btn.
//       Light DOM (bez Shadow DOM) zeby controls.css obslugiwal style.
//       Atrybuty: variant, tone, size, icon, disabled, full-width.
// Przyklad: <tf-button variant="primary" icon="plus">Dodaj</tf-button>
// =============================================================================

import { Sfx } from '/js/lib/sfx.js';

function escapeHtml(value) {
  return String(value)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

function safeIconName(value) {
  const text = String(value || '').trim();
  return /^[a-z0-9_-]{1,64}$/i.test(text) ? text : '';
}

const VARIANT_CLASS = {
  primary:        'tf-btn-primary',
  secondary:      'tf-btn-secondary',
  ghost:          'tf-btn-ghost',
  outline:        'tf-btn-outline',
  danger:         'tf-btn-danger',
  'danger-solid': 'tf-btn-danger-solid',
  'danger-outline': 'tf-btn-danger-outline',
  success:        'tf-btn-success',
};

// SDK tone (neutral/primary/success/warning/critical/info/muted) maps onto the
// real button (.tf-btn--tone-*) so only the inner button is tinted. "neutral"
// keeps the variant's own colour, so it gets no tone class.
const TONE_CLASS = {
  primary:  'tf-btn--tone-primary',
  success:  'tf-btn--tone-success',
  warning:  'tf-btn--tone-warning',
  critical: 'tf-btn--tone-critical',
  info:     'tf-btn--tone-info',
  muted:    'tf-btn--tone-muted',
};

class TfButton extends HTMLElement {
  static get observedAttributes() {
    return ['variant', 'tone', 'size', 'icon', 'trailing-icon', 'disabled', 'type', 'label', 'full-width'];
  }

  constructor() {
    super();
    this._btn = null;
    this._observer = null;
    this._onLightMutation = this._onLightMutation.bind(this);
  }

  connectedCallback() {
    if (!this._btn) this._build();
    this._update();
    // A caller that sets textContent/innerHTML on the host AFTER the upgrade
    // throws away the <button> this component built, leaving unstyled bare
    // text. Rebuild from whatever the caller left in the light DOM.
    if (!this._observer && typeof MutationObserver !== 'undefined') {
      this._observer = new MutationObserver(this._onLightMutation);
      this._observer.observe(this, { childList: true });
    }
  }

  disconnectedCallback() {
    if (this._observer) {
      this._observer.disconnect();
      this._observer = null;
    }
  }

  attributeChangedCallback() {
    if (this._btn) this._update();
  }

  // The rebuild re-appends the button, which mutates children again; that pass
  // sees an intact button and returns, so there is no loop.
  _onLightMutation() {
    if (!this._btn || this.contains(this._btn)) return;
    this._build();
    this._update();
  }

  _build() {
    // przenosimy slot content do wnetrza <button>, zachowujac HTML
    // jesli podano atrybut "label" — ma pierwszenstwo nad slotem
    const labelAttr = this.getAttribute('label');
    const innerHtml = labelAttr !== null ? escapeHtml(labelAttr) : this.innerHTML;
    this.innerHTML = '';
    const btn = document.createElement('button');
    btn.className = 'tf-btn';
    btn.innerHTML = this._renderContent(innerHtml);
    btn.addEventListener('click', (e) => {
      if (this.hasAttribute('disabled')) {
        e.preventDefault();
        e.stopImmediatePropagation();
        return;
      }
      const variant = this.getAttribute('variant') || 'primary';
      if (variant === 'primary' || variant === 'secondary' || variant === 'danger' || variant === 'danger-solid' || variant === 'danger-outline' || variant === 'success') {
        Sfx.play('ui-click');
      }
    });
    this.appendChild(btn);
    this._btn = btn;
  }

  _renderContent(text) {
    const icon = safeIconName(this.getAttribute('icon'));
    // Stroke + fill ustawione inline, bo symbole w spricie nie maja wlasnych
    // atrybutow (sprite() w modulach dodaje to przez klase .icon; tu emitujemy
    // SVG bez tej klasy, wiec atrybuty musza byc explicit).
    let iconSvg = icon
      ? `<svg width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><use href="#i-${icon}"/></svg>`
      : '';
    let body = typeof text === 'string' ? text : '';
    // Jesli brak atrybutu icon, ale tresc zaczyna sie od <svg>, traktujemy go
    // jak ikone — dzieki temu oba elementy (svg + span) sa flex-children .tf-btn
    // i dostaja automatyczny gap: 8px.
    if (!iconSvg && body) {
      const m = body.match(/^\s*(<svg[\s\S]*?<\/svg>)([\s\S]*)$/i);
      if (m) {
        iconSvg = m[1];
        body = m[2];
      }
    }
    // Detekcja koncowego <svg> — ikona po tekscie (np. strzalka "Dalej →").
    let trailSvg = '';
    if (body) {
      const mEnd = body.match(/^([\s\S]*?)(<svg[\s\S]*?<\/svg>)\s*$/i);
      if (mEnd) {
        body = mEnd[1];
        trailSvg = mEnd[2];
      }
    }
    // Atrybut "trailing-icon" wygrywa nad wykrytym koncowym <svg> w tresci —
    // emitujemy ikone ze spritu po tekscie (np. strzalka, check).
    const trailingIcon = safeIconName(this.getAttribute('trailing-icon'));
    if (trailingIcon) {
      trailSvg = `<svg width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><use href="#i-${trailingIcon}"/></svg>`;
    }
    const hasText = body && body.trim().length > 0;
    return iconSvg + (hasText ? `<span>${body}</span>` : '') + trailSvg;
  }

  _update() {
    if (!this._btn) return;
    const variant = this.getAttribute('variant') || 'primary';
    const size = this.getAttribute('size') || 'md';
    const icon = this.getAttribute('icon');

    // aktualizacja tekstu przez atrybut "label" — przerenderowanie contentu
    if (this.hasAttribute('label')) {
      this._btn.innerHTML = this._renderContent(escapeHtml(this.getAttribute('label')));
    }

    const hasText = (this.textContent || '').trim().length > 0 || this._btn.textContent.trim().length > 0;

    const classes = ['tf-btn'];
    const variantClass = VARIANT_CLASS[variant] || VARIANT_CLASS.primary;
    classes.push(variantClass);
    const tone = this.getAttribute('tone');
    if (tone && TONE_CLASS[tone]) classes.push(TONE_CLASS[tone]);
    if (size === 'sm') classes.push('tf-btn-sm');
    if (icon && !hasText) classes.push('tf-btn-icon');
    if (this.hasAttribute('full-width')) classes.push('tf-btn-full-width');
    this._btn.className = classes.join(' ');

    if (this.hasAttribute('disabled')) this._btn.setAttribute('disabled', '');
    else this._btn.removeAttribute('disabled');

    const type = this.getAttribute('type');
    if (type) this._btn.setAttribute('type', type);
  }
}

customElements.define('tf-button', TfButton);
export { TfButton };

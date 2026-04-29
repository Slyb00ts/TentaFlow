// =============================================================================
// Plik: tf-button.js
// Opis: Komponent <tf-button> — renderuje standardowy button z klasami .tf-btn.
//       Light DOM (bez Shadow DOM) zeby controls.css obslugiwal style.
//       Atrybuty: variant, size, icon, disabled. Magnetic hover effect na
//       primary (przesuniecie o max 3px w strone kursora).
// Przyklad: <tf-button variant="primary" icon="plus">Dodaj</tf-button>
// =============================================================================

import { Sfx } from '/js/lib/sfx.js';

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

class TfButton extends HTMLElement {
  static get observedAttributes() {
    return ['variant', 'size', 'icon', 'disabled', 'type', 'label'];
  }

  constructor() {
    super();
    this._btn = null;
    this._magnetRAF = 0;
    this._onPointerMove = this._onPointerMove.bind(this);
    this._onPointerLeave = this._onPointerLeave.bind(this);
  }

  connectedCallback() {
    if (!this._btn) this._build();
    this._update();
    this._attachMagnet();
  }

  disconnectedCallback() {
    this._detachMagnet();
  }

  attributeChangedCallback() {
    if (this._btn) this._update();
    // magnetic aktywny tylko na primary
    this._detachMagnet();
    this._attachMagnet();
  }

  _build() {
    // przenosimy slot content do wnetrza <button>, zachowujac HTML
    // jesli podano atrybut "label" — ma pierwszenstwo nad slotem
    const labelAttr = this.getAttribute('label');
    const innerHtml = labelAttr !== null ? labelAttr : this.innerHTML;
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
    const icon = this.getAttribute('icon');
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
      this._btn.innerHTML = this._renderContent(this.getAttribute('label'));
    }

    const hasText = (this.textContent || '').trim().length > 0 || this._btn.textContent.trim().length > 0;

    const classes = ['tf-btn'];
    const variantClass = VARIANT_CLASS[variant] || VARIANT_CLASS.primary;
    classes.push(variantClass);
    if (size === 'sm') classes.push('tf-btn-sm');
    if (icon && !hasText) classes.push('tf-btn-icon');
    this._btn.className = classes.join(' ');

    if (this.hasAttribute('disabled')) this._btn.setAttribute('disabled', '');
    else this._btn.removeAttribute('disabled');

    const type = this.getAttribute('type');
    if (type) this._btn.setAttribute('type', type);

    if (variant === 'primary') {
      this._btn.setAttribute('data-magnet', '');
    } else {
      this._btn.removeAttribute('data-magnet');
    }
  }

  _attachMagnet() {
    if (!this._btn) return;
    const variant = this.getAttribute('variant') || 'primary';
    if (variant !== 'primary') return;
    if (window.matchMedia && window.matchMedia('(pointer: coarse)').matches) return;
    this._btn.addEventListener('pointermove', this._onPointerMove);
    this._btn.addEventListener('pointerleave', this._onPointerLeave);
  }

  _detachMagnet() {
    if (!this._btn) return;
    this._btn.removeEventListener('pointermove', this._onPointerMove);
    this._btn.removeEventListener('pointerleave', this._onPointerLeave);
    this._btn.style.removeProperty('--tf-magnet-x');
    this._btn.style.removeProperty('--tf-magnet-y');
  }

  _onPointerMove(e) {
    if (this._magnetRAF) return;
    this._magnetRAF = requestAnimationFrame(() => {
      this._magnetRAF = 0;
      const r = this._btn.getBoundingClientRect();
      const cx = r.left + r.width / 2;
      const cy = r.top + r.height / 2;
      const maxOffset = 3;
      const dx = ((e.clientX - cx) / (r.width / 2)) * maxOffset;
      const dy = ((e.clientY - cy) / (r.height / 2)) * maxOffset;
      this._btn.style.setProperty('--tf-magnet-x', `${dx.toFixed(2)}px`);
      this._btn.style.setProperty('--tf-magnet-y', `${dy.toFixed(2)}px`);
    });
  }

  _onPointerLeave() {
    this._btn.style.setProperty('--tf-magnet-x', '0px');
    this._btn.style.setProperty('--tf-magnet-y', '0px');
  }
}

customElements.define('tf-button', TfButton);
export { TfButton };

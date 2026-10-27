// =============================================================================
// File: tf-tooltip.js
// Description: <tf-tooltip> — hover/focus tooltip that wraps child content and
//              shows a positioned bubble after a configurable delay. Light DOM.
// Example:
//   <tf-tooltip text="Save changes" side="bottom">
//     <tf-button>Save</tf-button>
//   </tf-tooltip>
//
//   Rich bubble: a child marked slot="content" is moved into the bubble as it
//   is (markup, live-updated values), so the text attribute is not used.
//   `align="start|end"` pins the bubble to that edge of the trigger instead of
//   centring it (a wide bubble under a narrow trigger would leave the screen),
//   `block` lets the trigger fill its parent instead of shrinking to its content.
//   <tf-tooltip side="bottom" align="start" block>
//     <div tabindex="0">Decode</div>
//     <div slot="content" id="tip">…</div>
//   </tf-tooltip>
// =============================================================================

class TfTooltip extends HTMLElement {
  static get observedAttributes() { return ['text', 'side', 'delay', 'align', 'block']; }

  constructor() {
    super();
    this._wrap = null;
    this._bubble = null;
    this._rich = false;
    this._timer = null;
    this._onEnter = this._onEnter.bind(this);
    this._onLeave = this._onLeave.bind(this);
    this._onFocusIn = this._onFocusIn.bind(this);
    this._onFocusOut = this._onFocusOut.bind(this);
  }

  connectedCallback() {
    if (!this._wrap) this._build();
    this._updateText();
  }

  disconnectedCallback() {
    this._clearTimer();
    if (this._wrap) {
      this._wrap.removeEventListener('mouseenter', this._onEnter);
      this._wrap.removeEventListener('mouseleave', this._onLeave);
      this._wrap.removeEventListener('focusin', this._onFocusIn);
      this._wrap.removeEventListener('focusout', this._onFocusOut);
    }
  }

  attributeChangedCallback(name) {
    if (name === 'text' && this._bubble) this._updateText();
    if ((name === 'side' || name === 'align') && this._bubble) this._updateSide();
    if (name === 'block' && this._wrap) this._wrap.classList.toggle('block', this.hasAttribute('block'));
  }

  get delay() { return parseInt(this.getAttribute('delay') || '400', 10); }
  get side() { return this.getAttribute('side') || 'top'; }

  _build() {
    const content = this.querySelector(':scope > [slot="content"]');
    // Wrap existing children
    const wrap = document.createElement('span');
    wrap.className = 'tf-tooltip-wrap';
    wrap.classList.toggle('block', this.hasAttribute('block'));
    while (this.firstChild) {
      if (this.firstChild === content) { this.removeChild(content); continue; }
      wrap.appendChild(this.firstChild);
    }

    const bubble = document.createElement('span');
    bubble.className = 'tf-tooltip-bubble';
    bubble.setAttribute('role', 'tooltip');
    if (content) {
      this._rich = true;
      bubble.classList.add('rich');
      content.removeAttribute('slot');
      bubble.appendChild(content);
    }
    this._updateSideClass(bubble);
    wrap.appendChild(bubble);

    wrap.addEventListener('mouseenter', this._onEnter);
    wrap.addEventListener('mouseleave', this._onLeave);
    wrap.addEventListener('focusin', this._onFocusIn);
    wrap.addEventListener('focusout', this._onFocusOut);

    this.appendChild(wrap);
    this._wrap = wrap;
    this._bubble = bubble;
  }

  _updateText() {
    if (this._bubble && !this._rich) this._bubble.textContent = this.getAttribute('text') || '';
  }

  _updateSide() {
    if (this._bubble) this._updateSideClass(this._bubble);
  }

  _updateSideClass(el) {
    el.classList.remove('side-top', 'side-bottom', 'side-left', 'side-right', 'align-start', 'align-end');
    el.classList.add(`side-${this.side}`);
    const align = this.getAttribute('align');
    if (align === 'start' || align === 'end') el.classList.add(`align-${align}`);
  }

  _clearTimer() {
    if (this._timer) { clearTimeout(this._timer); this._timer = null; }
  }

  _show() {
    this._clearTimer();
    this._timer = setTimeout(() => {
      if (this._wrap) this._wrap.classList.add('show');
    }, this.delay);
  }

  _hide() {
    this._clearTimer();
    if (this._wrap) this._wrap.classList.remove('show');
  }

  _onEnter() { this._show(); }
  _onLeave() { this._hide(); }
  _onFocusIn() { this._show(); }
  _onFocusOut() { this._hide(); }
}

customElements.define('tf-tooltip', TfTooltip);
export { TfTooltip };

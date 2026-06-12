// =============================================================================
// File: tf-breadcrumb.js
// Opis: Navigation breadcrumb container. Children are <tf-breadcrumb-item>
//       elements separated by a ">" delimiter. Supports href and current state.
// =============================================================================

function escapeHtml(value) {
  return String(value)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

class TfBreadcrumb extends HTMLElement {
  constructor() {
    super();
    this._nav = null;
    this._observer = null;
  }

  connectedCallback() {
    if (!this._nav) {
      this._build();
      this.appendChild(this._nav);
    }
    this._render();
    this._observer = new MutationObserver(() => {
      if (!this._rendering) this._render();
    });
    this._observer.observe(this, { childList: true, attributes: true });
  }

  disconnectedCallback() {
    if (this._observer) {
      this._observer.disconnect();
      this._observer = null;
    }
  }

  _build() {
    const nav = document.createElement('nav');
    nav.className = 'tf-breadcrumb';
    nav.setAttribute('aria-label', 'Breadcrumb');
    this._nav = nav;
  }

  _render() {
    this._rendering = true;
    const items = this.querySelectorAll('tf-breadcrumb-item');
    const parts = [];

    items.forEach((item, i) => {
      const href = item.getAttribute('href');
      const current = item.hasAttribute('current');
      // Labels can carry arbitrary user/state text — escape before innerHTML.
      const text = escapeHtml(item.textContent.trim());

      if (i > 0) {
        parts.push('<span class="tf-breadcrumb-sep" aria-hidden="true">&#8250;</span>');
      }

      if (current || !href) {
        parts.push(`<span class="tf-breadcrumb-item current" aria-current="page">${text}</span>`);
      } else {
        parts.push(`<a class="tf-breadcrumb-item" href="${escapeHtml(href)}">${text}</a>`);
      }
    });

    this._nav.innerHTML = parts.join('');
    this._rendering = false;
  }
}

class TfBreadcrumbItem extends HTMLElement {
  static get observedAttributes() {
    return ['href', 'current'];
  }

  attributeChangedCallback() {
    const parent = this.closest('tf-breadcrumb');
    if (parent && parent._nav) parent._render();
  }
}

customElements.define('tf-breadcrumb', TfBreadcrumb);
customElements.define('tf-breadcrumb-item', TfBreadcrumbItem);
export { TfBreadcrumb, TfBreadcrumbItem };

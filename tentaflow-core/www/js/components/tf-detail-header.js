// =============================================================================
// File: tf-detail-header.js
// Description: <tf-detail-header> — addon/entity detail header with large icon,
//              title, subtitle, version chip, badges slot and actions slot.
// Example:
//   <tf-detail-header title="Eureka" subtitle="MF Public Data" icon="database" version="1.0.0">
//     <span slot="badges"><tf-chip class="ok">Active</tf-chip></span>
//     <span slot="actions"><tf-button variant="primary">Install</tf-button></span>
//   </tf-detail-header>
// =============================================================================

class TfDetailHeader extends HTMLElement {
  static get observedAttributes() { return ['title', 'subtitle', 'icon', 'version']; }

  constructor() {
    super();
    this._root = null;
    this._titleEl = null;
    this._subtitleEl = null;
    this._iconEl = null;
    this._iconWrap = null;
    this._slottedIcon = null;
    this._versionEl = null;
    this._badgesArea = null;
    this._actionsArea = null;
  }

  connectedCallback() {
    if (!this._root) this._build();
    this._update();
  }

  attributeChangedCallback() {
    if (this._root) this._update();
  }

  _build() {
    // Collect slot children before clearing
    const badgesSlot = this.querySelector(':scope > [slot="badges"]');
    const actionsSlot = this.querySelector(':scope > [slot="actions"]');
    const iconSlot = this.querySelector(':scope > [slot="icon"]');
    const statusSlot = this.querySelector(':scope > [slot="status"]');
    this.innerHTML = '';

    const root = document.createElement('div');
    root.className = 'tf-detail-header';

    // Icon circle — a slotted icon (slot="icon") replaces the sprite <svg>
    const iconWrap = document.createElement('div');
    iconWrap.className = 'tf-detail-icon';
    const iconSvg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    iconSvg.classList.add('tf-detail-icon-svg');
    const useEl = document.createElementNS('http://www.w3.org/2000/svg', 'use');
    iconSvg.appendChild(useEl);
    if (iconSlot) {
      iconSlot.removeAttribute('slot');
      iconWrap.appendChild(iconSlot);
    } else {
      iconWrap.appendChild(iconSvg);
    }

    // Meta section
    const meta = document.createElement('div');
    meta.className = 'tf-detail-meta';

    const topRow = document.createElement('div');
    topRow.className = 'tf-detail-top-row';

    const titleEl = document.createElement('span');
    titleEl.className = 'tf-detail-title';

    const versionEl = document.createElement('span');
    versionEl.className = 'tf-chip accent';

    topRow.appendChild(titleEl);
    topRow.appendChild(versionEl);
    // slot="status" sits next to the title (entity state), while slot="badges"
    // stays the metadata row underneath.
    if (statusSlot) {
      statusSlot.removeAttribute('slot');
      topRow.appendChild(statusSlot);
    }

    const subtitleEl = document.createElement('div');
    subtitleEl.className = 'tf-detail-subtitle';

    const badgesArea = document.createElement('div');
    badgesArea.className = 'tf-detail-badges';
    if (badgesSlot) {
      badgesSlot.removeAttribute('slot');
      badgesArea.appendChild(badgesSlot);
    }

    meta.appendChild(topRow);
    meta.appendChild(subtitleEl);
    meta.appendChild(badgesArea);

    // Actions
    const actionsArea = document.createElement('div');
    actionsArea.className = 'tf-detail-actions';
    if (actionsSlot) {
      actionsSlot.removeAttribute('slot');
      actionsArea.appendChild(actionsSlot);
    }

    root.appendChild(iconWrap);
    root.appendChild(meta);
    root.appendChild(actionsArea);
    this.appendChild(root);

    this._root = root;
    this._iconEl = iconSvg;
    this._iconWrap = iconWrap;
    this._slottedIcon = iconSlot;
    this._titleEl = titleEl;
    this._subtitleEl = subtitleEl;
    this._versionEl = versionEl;
    this._badgesArea = badgesArea;
    this._actionsArea = actionsArea;
  }

  _update() {
    const title = this.getAttribute('title') || '';
    const subtitle = this.getAttribute('subtitle') || '';
    const icon = this.getAttribute('icon') || '';
    const version = this.getAttribute('version') || '';

    this._titleEl.textContent = title;
    this._subtitleEl.textContent = subtitle;
    this._subtitleEl.style.display = subtitle ? '' : 'none';

    if (this._slottedIcon) {
      this._iconWrap.style.display = '';
    } else if (icon) {
      this._iconEl.querySelector('use').setAttribute('href', `#i-${icon}`);
      this._iconWrap.style.display = '';
    } else {
      this._iconWrap.style.display = 'none';
    }

    this._versionEl.textContent = version;
    this._versionEl.style.display = version ? '' : 'none';
  }
}

customElements.define('tf-detail-header', TfDetailHeader);
export { TfDetailHeader };

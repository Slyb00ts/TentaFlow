// =============================================================================
// File: tf-addon-ui-frame.js
// Description: <tf-addon-ui-frame> — renders a sandboxed iframe for an addon
//              UI bundle. Sandbox is locked to allow-scripts only (no popups,
//              no same-origin). On load, posts a `ui.init` event into the
//              iframe carrying permissions + theme so the addon bundle can
//              wire its UI before any user interaction. Cleanup releases the
//              owned blob URL (if any) on disconnect.
// Example:
//   <tf-addon-ui-frame addon-id="tentavision" component-id="dashboard"
//                      src-url="blob:..." permissions="alias.read,camera.read">
//   </tf-addon-ui-frame>
// =============================================================================

class TfAddonUiFrame extends HTMLElement {
  static get observedAttributes() {
    return ['src-url', 'permissions'];
  }

  constructor() {
    super();
    this._shadow = this.attachShadow({ mode: 'open' });
    this._iframe = null;
    this._statusEl = null;
    this._onLoad = this._onLoad.bind(this);
    this._onError = this._onError.bind(this);
  }

  connectedCallback() {
    if (!this._iframe) this._build();
    this._applySrc();
  }

  disconnectedCallback() {
    if (this._iframe) {
      this._iframe.removeEventListener('load', this._onLoad);
      this._iframe.removeEventListener('error', this._onError);
    }
  }

  attributeChangedCallback(name) {
    if (!this._iframe) return;
    if (name === 'src-url') this._applySrc();
  }

  // Exposed for the host harness to register the inner iframe in its
  // event.source → addon registry.
  get iframe() { return this._iframe; }

  _build() {
    const style = document.createElement('style');
    style.textContent = `
      :host { display: block; position: relative; width: 100%; height: 100%; min-height: 240px; }
      .tf-addon-frame-shell {
        position: absolute; inset: 0;
        border: 1px solid var(--tf-border, #2a2f3a);
        border-radius: 8px;
        overflow: hidden;
        background: var(--tf-surface, #161a22);
      }
      iframe {
        width: 100%; height: 100%; border: 0; display: block; background: #fff;
      }
      .tf-addon-frame-status {
        position: absolute; left: 50%; top: 50%;
        transform: translate(-50%, -50%);
        color: var(--tf-text-2, #b6bcc7);
        font-size: 12px;
        font-family: var(--tf-font-ui, system-ui), sans-serif;
        pointer-events: none;
      }
      .tf-addon-frame-status[hidden] { display: none; }
      .tf-addon-frame-status[data-kind="error"] { color: #ff6b6b; }
    `;
    const shell = document.createElement('div');
    shell.className = 'tf-addon-frame-shell';

    const iframe = document.createElement('iframe');
    // Hard-coded sandbox per F1c-P0 Q1 decision (Minimal). Any expansion of
    // this list (e.g. adding allow-popups) MUST go through a design review:
    // allow-same-origin in particular would break the isolation guarantee
    // the bridge relies on.
    iframe.setAttribute('sandbox', 'allow-scripts');
    iframe.setAttribute('referrerpolicy', 'no-referrer');
    iframe.setAttribute('loading', 'eager');
    iframe.setAttribute('title', `Addon UI: ${this.getAttribute('addon-id') || 'unknown'}`);
    iframe.addEventListener('load', this._onLoad);
    iframe.addEventListener('error', this._onError);

    const status = document.createElement('div');
    status.className = 'tf-addon-frame-status';
    status.textContent = 'Ładowanie addona…';

    shell.appendChild(iframe);
    shell.appendChild(status);
    this._shadow.appendChild(style);
    this._shadow.appendChild(shell);

    this._iframe = iframe;
    this._statusEl = status;
  }

  _applySrc() {
    const src = this.getAttribute('src-url') || '';
    if (!src) {
      this._showStatus('Brak źródła bundla', 'error');
      return;
    }
    if (this._iframe.src !== src) {
      this._showStatus('Ładowanie addona…', 'info');
      this._iframe.src = src;
    }
  }

  _onLoad() {
    this._hideStatus();
    const permissions = (this.getAttribute('permissions') || '')
      .split(',')
      .map((s) => s.trim())
      .filter(Boolean);
    const theme = document.documentElement.dataset.theme || 'dark';
    // Send init event so the addon can render against the correct theme +
    // know which capabilities are available before issuing requests.
    try {
      this._iframe.contentWindow?.postMessage({
        kind: 'event',
        id: null,
        topic: 'ui.init',
        payload: { permissions, theme },
      }, '*');
    } catch (err) {
      console.warn('[tf-addon-ui-frame] ui.init postMessage failed:', err);
    }
    this.dispatchEvent(new CustomEvent('tf-addon-ui-ready', {
      bubbles: true,
      detail: {
        addonId: this.getAttribute('addon-id'),
        componentId: this.getAttribute('component-id'),
      },
    }));
  }

  _onError() {
    this._showStatus('Błąd ładowania addona', 'error');
  }

  _showStatus(text, kind) {
    if (!this._statusEl) return;
    this._statusEl.textContent = text;
    this._statusEl.dataset.kind = kind || 'info';
    this._statusEl.hidden = false;
  }

  _hideStatus() {
    if (this._statusEl) this._statusEl.hidden = true;
  }
}

customElements.define('tf-addon-ui-frame', TfAddonUiFrame);
export { TfAddonUiFrame };

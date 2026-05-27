// =============================================================================
// Plik: components/tf-fps-counter.js
// Opis: Licznik FPS / metryki w czasie rzeczywistym (Specialized::FpsCounter).
// Subskrybuje stream binary protocol, ostatnia wartosc wyswietlana duzo, ostatnie
// 60 probek opcjonalnie jako sparkline pod spodem.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import '/js/components/tf-sparkline.js';

class TfFpsCounter extends HTMLElement {
  constructor() {
    super();
    this._streamId = null;
    this._label = null;
    this._format = null;
    this._showSparkline = false;
    this._history = [];
    this._maxHistory = 60;
    this._handle = null;
    this._valueEl = null;
    this._labelEl = null;
    this._sparkEl = null;
  }

  static get observedAttributes() { return ['stream-id', 'label', 'format', 'show-sparkline']; }

  connectedCallback() {
    this.classList.add('sdk-fps-counter');
    if (!this._valueEl) {
      this._valueEl = document.createElement('span');
      this._valueEl.className = 'sdk-fps-value';
      this._valueEl.textContent = '—';
      this.appendChild(this._valueEl);
      this._labelEl = document.createElement('span');
      this._labelEl.className = 'sdk-fps-label';
      this.appendChild(this._labelEl);
    }
    if (this._label) this._labelEl.textContent = this._label;
    if (this._showSparkline && !this._sparkEl) {
      this._sparkEl = document.createElement('tf-sparkline');
      this._sparkEl.height = 18;
      this.appendChild(this._sparkEl);
    }
    this._subscribe();
  }

  disconnectedCallback() { this._unsubscribe(); }

  attributeChangedCallback(name, _old, value) {
    if (name === 'stream-id') { this._streamId = value || null; if (this.isConnected) { this._unsubscribe(); this._subscribe(); } }
    else if (name === 'label') { this._label = value || null; if (this._labelEl) this._labelEl.textContent = value || ''; }
    else if (name === 'format') { this._format = value || null; }
    else if (name === 'show-sparkline') { this._showSparkline = value !== null && value !== 'false'; }
  }

  _subscribe() {
    if (!this._streamId) return;
    try {
      this._handle = ApiBinary.subscribe(
        'streamSubscribeRequest',
        { streamId: this._streamId },
        {
          onChunk: (pkt) => this._handlePacket(pkt),
          onError: (err) => console.warn('[tf-fps-counter] subscribe failed:', err?.message ?? err),
          onEnd: () => { this._handle = null; },
        },
      );
    } catch (err) {
      console.warn('[tf-fps-counter] subscribe error:', err?.message ?? err);
    }
  }

  _unsubscribe() {
    const streamId = this._streamId;
    this._handle = null;
    if (streamId) {
      ApiBinary.action('streamCloseRequest', { streamId }).catch(() => {});
    }
  }

  _handlePacket(pkt) {
    let payload = pkt;
    if (pkt && pkt.payload) payload = pkt.payload;
    let value = null;
    if (payload instanceof Uint8Array) {
      try { const j = JSON.parse(new TextDecoder().decode(payload)); value = Number(j.value ?? j.fps ?? j); } catch {}
    } else if (typeof payload === 'string') {
      try { const j = JSON.parse(payload); value = Number(j.value ?? j.fps ?? j); }
      catch { value = Number(payload); }
    } else if (typeof payload === 'number') value = payload;
    if (!Number.isFinite(value)) return;
    this._history.push(value);
    if (this._history.length > this._maxHistory) this._history.shift();
    if (this._valueEl) {
      this._valueEl.textContent = this._formatValue(value);
    }
    if (this._sparkEl) {
      this._sparkEl.points = this._history;
    }
  }

  _formatValue(v) {
    if (!this._format) return v.toFixed(1);
    // Bardzo prosty interpreter — {0:.1f}, {0} ; addon moze przekazac np. "%.0f FPS"
    if (this._format.includes('%')) {
      try { return this._format.replace(/%\.(\d+)f/, (_, d) => v.toFixed(Number(d))).replace(/%d|%f/, String(v)); }
      catch { return String(v); }
    }
    return this._format.replace(/\{0(?::([^}]+))?\}/, (_, spec) => {
      if (!spec) return String(v);
      const m = /\.(\d+)f/.exec(spec);
      if (m) return v.toFixed(Number(m[1]));
      return String(v);
    });
  }
}

if (!customElements.get('tf-fps-counter')) {
  customElements.define('tf-fps-counter', TfFpsCounter);
}

export { TfFpsCounter };

// =============================================================================
// Plik: components/tf-alarm-feed.js
// Opis: Live feed alarmow (Specialized::AlarmFeed). Subskrybuje stream_id z
// binary protocol (jak tf-video-stream), kazdy pakiet text/JSON jest jednym
// itemem; trzymamy max_items ostatnich, posortowanych chronologicznie odwrotnie.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

class TfAlarmFeed extends HTMLElement {
  constructor() {
    super();
    this._streamId = null;
    this._maxItems = 50;
    this._heightPx = null;
    this._onItemClick = null;
    this._items = [];
    this._handle = null;
    this._listEl = null;
  }

  static get observedAttributes() { return ['stream-id', 'max-items', 'height-px']; }

  connectedCallback() {
    this.classList.add('sdk-alarm-feed');
    if (this._heightPx) this.style.maxHeight = `${this._heightPx}px`;
    if (!this._listEl) {
      this._listEl = document.createElement('div');
      this._listEl.className = 'sdk-alarm-feed-list';
      this.appendChild(this._listEl);
    }
    this._subscribe();
  }

  disconnectedCallback() { this._unsubscribe(); }

  attributeChangedCallback(name, _old, value) {
    if (name === 'stream-id') { this._streamId = value || null; if (this.isConnected) { this._unsubscribe(); this._subscribe(); } }
    else if (name === 'max-items') { const n = Number(value); if (Number.isFinite(n) && n > 0) this._maxItems = n; }
    else if (name === 'height-px') { const n = Number(value); if (Number.isFinite(n) && n > 0) { this._heightPx = n; this.style.maxHeight = `${n}px`; } }
  }

  set onItemClick(cb) { this._onItemClick = typeof cb === 'function' ? cb : null; }

  _subscribe() {
    if (!this._streamId) return;
    try {
      this._handle = ApiBinary.subscribe(
        'streamSubscribeRequest',
        { streamId: this._streamId },
        {
          onChunk: (pkt) => this._handlePacket(pkt),
          onError: (err) => console.warn('[tf-alarm-feed] subscribe failed:', err?.message ?? err),
          onEnd: () => { this._handle = null; },
        },
      );
    } catch (err) {
      console.warn('[tf-alarm-feed] subscribe error:', err?.message ?? err);
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
    let parsed = payload;
    if (payload instanceof Uint8Array) {
      try { parsed = JSON.parse(new TextDecoder().decode(payload)); } catch { return; }
    } else if (typeof payload === 'string') {
      try { parsed = JSON.parse(payload); } catch { parsed = { message: payload }; }
    }
    if (!parsed || typeof parsed !== 'object') return;
    this._items.unshift({
      id: parsed.id ?? String(Date.now()),
      time: parsed.time ?? parsed.timestamp ?? new Date().toISOString(),
      message: parsed.message ?? parsed.label ?? '',
      severity: parsed.severity ?? 'info',
      raw: parsed,
    });
    if (this._items.length > this._maxItems) this._items.length = this._maxItems;
    this._renderList();
  }

  _renderList() {
    const el = this._listEl;
    if (!el) return;
    el.innerHTML = '';
    for (const item of this._items) {
      const row = document.createElement('div');
      row.className = `sdk-alarm-item sev-${item.severity}`;
      if (this._onItemClick) {
        row.style.cursor = 'pointer';
        row.addEventListener('click', () => this._onItemClick(item.raw));
      }
      const t = document.createElement('div');
      t.className = 'sdk-alarm-item-time';
      t.textContent = item.time;
      const m = document.createElement('div');
      m.className = 'sdk-alarm-item-msg';
      m.textContent = item.message;
      row.appendChild(t);
      row.appendChild(m);
      el.appendChild(row);
    }
  }
}

if (!customElements.get('tf-alarm-feed')) {
  customElements.define('tf-alarm-feed', TfAlarmFeed);
}

export { TfAlarmFeed };

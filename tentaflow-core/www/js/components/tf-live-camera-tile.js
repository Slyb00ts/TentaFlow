// =============================================================================
// Plik: tf-live-camera-tile.js
// Opis: Komponent <tf-live-camera-tile camera-id ttl-secs [label] [height-px]
//       addon-id panel-id> — kafelek live podgladu z kamery. Co (ttl_secs / 2)
//       sekund odswieza atrybut `src` w wewnetrznym <img> przez nowy signed URL
//       (frame_url) pobierany akcja `__tentaflow.frame_url__` w panelu addonu.
//       Cleanup timera w disconnectedCallback gwarantuje brak wyciekow gdy
//       addon-app re-renderuje panel.
// Przyklad: <tf-live-camera-tile camera-id="550e..." ttl-secs="30"
//             addon-id="tentavision" panel-id="home"></tf-live-camera-tile>
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

// Zarezerwowany action ID w panelach addonu zwracajacy {frameUrl: "..."}.
// Addon-side: deklaruje action o tym ID + on_action zwraca signed URL z
// frame_url(camera_id, ttl_secs). Jezeli action nie istnieje — pokazujemy
// "Brak podglądu" zamiast pustego src.
const FRAME_URL_ACTION = '__tentaflow.frame_url__';

// Bezpieczne granice (zgodne z LIVE_CAMERA_TILE_TTL_MIN/MAX w hoscie).
const TTL_MIN = 5;
const TTL_MAX = 300;
const TTL_DEFAULT = 30;

// Po bledzie cofamy sie do wolniejszego backoffu: ttl * 2 sekund — zeby nie
// hammerowac brokenowanego addonu petlas zadania.
const ERROR_BACKOFF_MULT = 2;

class TfLiveCameraTile extends HTMLElement {
  static get observedAttributes() {
    return ['camera-id', 'ttl-secs', 'label', 'height-px', 'addon-id', 'panel-id'];
  }

  constructor() {
    super();
    this._img = null;
    this._labelEl = null;
    this._errorEl = null;
    this._timer = null;
    this._fetching = false;
    this._disposed = false;
  }

  connectedCallback() {
    if (!this._img) this._build();
    this._update();
    this._scheduleRefresh(0);
  }

  disconnectedCallback() {
    this._disposed = true;
    this._stopTimer();
  }

  attributeChangedCallback(name) {
    if (!this._img) return;
    if (name === 'camera-id' || name === 'ttl-secs') {
      this._stopTimer();
      this._scheduleRefresh(0);
    }
    this._update();
  }

  _build() {
    this.classList.add('tf-live-camera-tile');
    this._labelEl = document.createElement('div');
    this._labelEl.className = 'tf-live-camera-label';

    this._img = document.createElement('img');
    this._img.className = 'tf-live-camera-img';
    this._img.alt = 'Live preview';

    this._errorEl = document.createElement('div');
    this._errorEl.className = 'tf-live-camera-error';
    this._errorEl.hidden = true;
    this._errorEl.textContent = 'Brak podglądu kamery';

    this.appendChild(this._labelEl);
    this.appendChild(this._img);
    this.appendChild(this._errorEl);
  }

  _update() {
    const label = this.getAttribute('label') ?? '';
    if (label) {
      this._labelEl.textContent = label;
      this._labelEl.hidden = false;
    } else {
      this._labelEl.textContent = '';
      this._labelEl.hidden = true;
    }
    const height = this.getAttribute('height-px');
    if (height && Number.isFinite(Number(height)) && Number(height) > 0) {
      this.style.height = `${Number(height)}px`;
    } else {
      this.style.removeProperty('height');
    }
  }

  _ttlSecs() {
    const raw = Number(this.getAttribute('ttl-secs'));
    if (!Number.isFinite(raw) || raw <= 0) return TTL_DEFAULT;
    if (raw < TTL_MIN) return TTL_MIN;
    if (raw > TTL_MAX) return TTL_MAX;
    return Math.floor(raw);
  }

  _stopTimer() {
    if (this._timer != null) {
      clearTimeout(this._timer);
      this._timer = null;
    }
  }

  _scheduleRefresh(delayMs) {
    this._stopTimer();
    if (this._disposed) return;
    this._timer = setTimeout(() => this._refresh(), Math.max(0, delayMs));
  }

  async _refresh() {
    if (this._disposed || this._fetching) return;
    const cameraId = this.getAttribute('camera-id') ?? '';
    const addonId = this.getAttribute('addon-id') ?? '';
    const panelId = this.getAttribute('panel-id') ?? '';
    const ttl = this._ttlSecs();
    if (!cameraId || !addonId || !panelId) {
      this._showError();
      return;
    }
    this._fetching = true;
    try {
      const res = await ApiBinary.one('addonUiActionRequest', {
        addonId,
        panelId,
        actionId: FRAME_URL_ACTION,
        params: { cameraId, ttlSecs: ttl },
      });
      const url = String(res?.frameUrl ?? res?.frame_url ?? '').trim();
      if (!url) throw new Error('empty frameUrl');
      this._applyUrl(url);
      this._scheduleRefresh((ttl * 1000) / 2);
    } catch (e) {
      console.warn('[tf-live-camera-tile] frame_url fetch failed:', e?.message ?? e);
      this._showError();
      // Slower backoff po bledzie — zeby addon w stanie awarii nie generowal
      // bezsensownego ruchu na binary WS.
      this._scheduleRefresh((ttl * 1000) * ERROR_BACKOFF_MULT);
    } finally {
      this._fetching = false;
    }
  }

  _applyUrl(url) {
    if (!url || !this._img) return;
    // URL jest signed (HMAC); same-origin path /frames/<ref>?token=... — nie
    // walidujemy schematu dalej, host go wystawil.
    this._img.src = url;
    this._img.hidden = false;
    if (this._errorEl) this._errorEl.hidden = true;
  }

  _showError() {
    if (this._img) {
      this._img.removeAttribute('src');
      this._img.hidden = true;
    }
    if (this._errorEl) this._errorEl.hidden = false;
  }
}

if (!customElements.get('tf-live-camera-tile')) {
  customElements.define('tf-live-camera-tile', TfLiveCameraTile);
}

export { TfLiveCameraTile };

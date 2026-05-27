// =============================================================================
// File: tf-alarm-card.js
// Description: <tf-alarm-card> — alarm feed item with severity indicator, title,
//              camera name, timestamp and action button. Emits 'action' with
//              detail.type on button click. Default slot for extra chips/metadata.
// Example: <tf-alarm-card severity="critical" title="Motion detected"
//            camera="Front door" time="12:34:56"></tf-alarm-card>
// =============================================================================

class TfAlarmCard extends HTMLElement {
  static get observedAttributes() { return ['severity', 'title', 'camera', 'time']; }

  constructor() {
    super();
    this._root = null;
    this._titleEl = null;
    this._cameraEl = null;
    this._timeEl = null;
    this._slotArea = null;
  }

  connectedCallback() {
    if (!this._root) this._build();
    this._update();
  }

  attributeChangedCallback() {
    if (this._root) this._update();
  }

  _build() {
    // Preserve slot children before clearing
    const slotChildren = Array.from(this.childNodes);
    this.innerHTML = '';

    const card = document.createElement('div');
    card.className = 'tf-alarm-card';

    const thumb = document.createElement('div');
    thumb.className = 'tf-alarm-thumb';

    const body = document.createElement('div');
    body.className = 'tf-alarm-body';

    const titleEl = document.createElement('div');
    titleEl.className = 'tf-alarm-title';

    const meta = document.createElement('div');
    meta.className = 'tf-alarm-meta';

    const cameraEl = document.createElement('span');
    cameraEl.className = 'tf-alarm-camera';

    const timeEl = document.createElement('span');
    timeEl.className = 'tf-alarm-time';

    meta.appendChild(cameraEl);
    meta.appendChild(timeEl);

    const slotArea = document.createElement('div');
    slotArea.className = 'tf-alarm-slot';
    for (const child of slotChildren) slotArea.appendChild(child);

    body.appendChild(titleEl);
    body.appendChild(meta);
    body.appendChild(slotArea);

    const actionBtn = document.createElement('button');
    actionBtn.type = 'button';
    actionBtn.className = 'tf-alarm-action';
    actionBtn.textContent = 'Open';
    actionBtn.addEventListener('click', () => {
      this.dispatchEvent(new CustomEvent('action', {
        bubbles: true,
        detail: { type: 'open' },
      }));
    });

    card.appendChild(thumb);
    card.appendChild(body);
    card.appendChild(actionBtn);
    this.appendChild(card);

    this._root = card;
    this._titleEl = titleEl;
    this._cameraEl = cameraEl;
    this._timeEl = timeEl;
    this._slotArea = slotArea;
  }

  _update() {
    const severity = this.getAttribute('severity') || 'info';
    const title = this.getAttribute('title') || '';
    const camera = this.getAttribute('camera') || '';
    const time = this.getAttribute('time') || '';

    this._root.className = `tf-alarm-card ${severity}`;
    this._titleEl.textContent = title;
    this._cameraEl.textContent = camera;
    this._timeEl.textContent = time;
    this._slotArea.style.display = this._slotArea.childNodes.length ? '' : 'none';
  }
}

customElements.define('tf-alarm-card', TfAlarmCard);
export { TfAlarmCard };

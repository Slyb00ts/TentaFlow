// =============================================================================
// File: tf-file-input.js
// Description: <tf-file-input> — styled file upload component.
//   Attributes: accept, multiple, label, disabled, capture (user/environment),
//   no-drop (disables drag-and-drop, click-to-pick only).
//   Events: change (detail: {files: FileList}).
// =============================================================================

class TfFileInput extends HTMLElement {
  static get observedAttributes() {
    return ['accept', 'multiple', 'label', 'disabled', 'capture', 'no-drop'];
  }

  constructor() {
    super();
    this._wrap = null;
    this._input = null;
    this._display = null;
    this._onClick = this._onClick.bind(this);
    this._onDragOver = this._onDragOver.bind(this);
    this._onDragLeave = this._onDragLeave.bind(this);
    this._onDrop = this._onDrop.bind(this);
  }

  connectedCallback() {
    if (!this._wrap) this._build();
    this._update();
  }

  attributeChangedCallback(name, oldVal, newVal) {
    if (oldVal === newVal || !this._wrap) return;
    this._update();
  }

  _build() {
    this.innerHTML = '';

    const wrap = document.createElement('div');
    wrap.className = 'tf-file-input';

    const dropzone = document.createElement('div');
    dropzone.className = 'tf-file-input-dropzone';
    dropzone.setAttribute('role', 'button');
    dropzone.setAttribute('tabindex', '0');
    dropzone.addEventListener('click', this._onClick);
    dropzone.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        this._onClick();
      }
    });
    dropzone.addEventListener('dragover', this._onDragOver);
    dropzone.addEventListener('dragleave', this._onDragLeave);
    dropzone.addEventListener('drop', this._onDrop);

    const labelEl = document.createElement('span');
    labelEl.className = 'tf-file-input-label';
    dropzone.appendChild(labelEl);

    const display = document.createElement('span');
    display.className = 'tf-file-input-display';
    dropzone.appendChild(display);

    const input = document.createElement('input');
    input.type = 'file';
    input.className = 'tf-file-input-native';
    input.style.display = 'none';
    input.addEventListener('change', (e) => {
      // Native change bubbles up to this host too; without stopping it, listeners
      // on <tf-file-input> receive a second detail-less `change` right after our
      // CustomEvent and would treat it as an empty selection.
      e.stopPropagation();
      this._showFiles(input.files);
      this.dispatchEvent(new CustomEvent('change', {
        bubbles: true,
        detail: { files: input.files },
      }));
    });

    wrap.appendChild(input);
    wrap.appendChild(dropzone);

    this.appendChild(wrap);
    this._wrap = wrap;
    this._input = input;
    this._dropzone = dropzone;
    this._labelEl = labelEl;
    this._display = display;
  }

  _update() {
    const accept = this.getAttribute('accept') || '';
    const multiple = this.hasAttribute('multiple');
    const label = this.getAttribute('label') || (multiple ? 'Choose files' : 'Choose file');
    const disabled = this.hasAttribute('disabled');

    if (accept) this._input.setAttribute('accept', accept);
    else this._input.removeAttribute('accept');

    if (multiple) this._input.setAttribute('multiple', '');
    else this._input.removeAttribute('multiple');

    const capture = this.getAttribute('capture');
    if (capture) this._input.setAttribute('capture', capture);
    else this._input.removeAttribute('capture');

    this._labelEl.textContent = label;
    this._dropzone.classList.toggle('tf-file-input-disabled', disabled);

    if (disabled) {
      this._dropzone.removeAttribute('tabindex');
      this._dropzone.setAttribute('aria-disabled', 'true');
    } else {
      this._dropzone.setAttribute('tabindex', '0');
      this._dropzone.removeAttribute('aria-disabled');
    }
  }

  _onClick() {
    if (this.hasAttribute('disabled')) return;
    this._input.click();
  }

  _onDragOver(e) {
    if (this.hasAttribute('disabled') || this.hasAttribute('no-drop')) return;
    e.preventDefault();
    this._dropzone.classList.add('tf-file-input-over');
  }

  _onDragLeave() {
    this._dropzone.classList.remove('tf-file-input-over');
  }

  _onDrop(e) {
    if (this.hasAttribute('disabled') || this.hasAttribute('no-drop')) return;
    e.preventDefault();
    this._dropzone.classList.remove('tf-file-input-over');
    if (e.dataTransfer?.files) {
      this._showFiles(e.dataTransfer.files);
      this.dispatchEvent(new CustomEvent('change', {
        bubbles: true,
        detail: { files: e.dataTransfer.files },
      }));
    }
  }

  _showFiles(files) {
    if (!files || files.length === 0) {
      this._display.textContent = '';
      return;
    }
    if (files.length === 1) {
      this._display.textContent = files[0].name;
    } else {
      this._display.textContent = `${files.length} files selected`;
    }
  }
}

customElements.define('tf-file-input', TfFileInput);
export { TfFileInput };
